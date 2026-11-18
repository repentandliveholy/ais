//! The interactive browser: layout, rendering and the terminal event loop.

use crate::actions::{copy_text, reveal};
use crate::adapters::{Collector, SourceInfo, detail};
use crate::i18n::{LANGUAGES, language, read_settings, save_language, save_settings, set_language, tr};
use crate::model::{
    CLIENTS, Entry, Filter, Grouping, Project, Session, age, clear_path_caches, filter_sessions, folder_entries, group_projects, label,
    now, number, parse_query, pretty_path, status_text, strftime, validate_launch, which,
};
use crate::screen::{Action, Part, Screen, WORDMARK, ansi_color, client_style, part, status_style};
use crate::text::{casefold, clean, clip, line, shell_quote, width, wrap};
use crate::trf;
use crate::usage::{Cancel, Usage, fetch_live_usage, load_usage, max_usage_scroll, render_usage, usage_strip_lines};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use crossterm::style::{Attribute, Color, Print, SetAttribute, SetBackgroundColor, SetForegroundColor};
use crossterm::{cursor, execute, queue, terminal};
use indexmap::IndexMap;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct View {
    pub cwd: String,
    pub home: PathBuf,
    pub scope: &'static str,
    pub client: String,
    pub query: String,
    pub selected: i64,
    pub selected_key: String,
    pub top: i64,
    pub detail_scroll: i64,
    pub children: bool,
    pub archived: bool,
    pub searching: bool,
    pub loaded: bool,
    pub busy_detail: bool,
    pub message: String,
    pub modal: &'static str,
    pub new_client: usize,
    pub language_index: usize,
    pub new_cwd: String,
    pub narrow_detail: bool,
    pub tab: &'static str,
    pub usage_scroll: usize,
    pub live_usage: bool,
    pub auto_refresh: bool,
    pub project_path: Option<String>,
    pub project_index: i64,
    pub project_top: i64,
    pub tree: bool,
    pub folder_path: Option<String>,
    pub folder_stack: Vec<(Option<String>, i64, i64)>,
    pub usage_tab: &'static str,
    pub usage_project: Option<String>,
    pub pins: HashSet<String>,
    pub help_scroll: i64,
    /// Usage survives returning from a CLI, so the strip never starts empty.
    pub usage: Option<Usage>,
    pub usage_live_at: f64,
    pub usage_refreshing: bool,
    /// Bumped whenever the session list changes, so cached rows are rebuilt.
    pub data_rev: u64,
    memo: std::cell::RefCell<Memo>,
}

/// Everything that decides which rows are visible.
#[derive(Clone, PartialEq)]
struct EntriesKey {
    data: (usize, usize, u64),
    tab: &'static str,
    scope: &'static str,
    cwd: String,
    client: String,
    query: String,
    children: bool,
    archived: bool,
    tree: bool,
    project_path: Option<String>,
    folder_path: Option<String>,
    pins: HashSet<String>,
}

/// Rows and per-client counts from the last frame. Filtering, sorting and
/// grouping all sessions is far too slow to redo on every scroll step.
#[derive(Clone, Default)]
struct Memo {
    entries: Option<(EntriesKey, Rc<Vec<Entry>>)>,
    counts: Option<(EntriesKey, Rc<HashMap<&'static str, usize>>)>,
}

impl std::fmt::Debug for Memo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Memo")
    }
}

impl View {
    pub fn new(cwd: &str, home: &Path) -> Self {
        Self {
            cwd: cwd.into(),
            home: home.into(),
            scope: "cwd",
            client: "all".into(),
            query: String::new(),
            selected: 0,
            selected_key: String::new(),
            top: 0,
            detail_scroll: 0,
            children: false,
            archived: false,
            searching: false,
            loaded: false,
            busy_detail: false,
            message: String::new(),
            modal: "",
            new_client: 0,
            language_index: 0,
            new_cwd: String::new(),
            narrow_detail: false,
            tab: "sessions",
            usage_scroll: 0,
            live_usage: true,
            auto_refresh: true,
            project_path: None,
            project_index: 0,
            project_top: 0,
            tree: false,
            folder_path: None,
            folder_stack: Vec::new(),
            usage_tab: "sessions",
            usage_project: None,
            pins: HashSet::new(),
            help_scroll: 0,
            usage: None,
            usage_live_at: 0.0,
            usage_refreshing: false,
            data_rev: 0,
            memo: Default::default(),
        }
    }

    fn entries_key(&self, sessions: &[Session]) -> EntriesKey {
        EntriesKey {
            data: (sessions.as_ptr() as usize, sessions.len(), self.data_rev),
            tab: self.tab,
            scope: self.scope,
            cwd: self.cwd.clone(),
            client: self.client.clone(),
            query: self.query.clone(),
            children: self.children,
            archived: self.archived,
            tree: self.tree,
            project_path: self.project_path.clone(),
            folder_path: self.folder_path.clone(),
            pins: self.pins.clone(),
        }
    }

    fn grouping(&self) -> Grouping<'_> {
        Grouping { client: &self.client, query: &self.query, children: self.children, archived: self.archived, pins: &self.pins }
    }
}

const HISTORY_REFRESH: Duration = Duration::from_secs(10);
const USAGE_REFRESH: Duration = Duration::from_secs(60);
/// The strip refreshes rarely: each refresh runs the official CLIs.
const STRIP_REFRESH_SECONDS: f64 = 300.0;
const RESTART_KEY: Duration = Duration::from_millis(500);
/// Upper bound on rows moved per frame by one burst of queued wheel/arrow events.
const MAX_BURST_STEPS: i64 = 5;
const DOUBLE_CLICK: Duration = Duration::from_millis(450);
pub const STRIP_WIDTH: i64 = 27;
pub const STRIP_MIN_COLS: i64 = 150;

pub fn date(value: f64) -> String {
    if value == 0.0 {
        return "—".into();
    }
    strftime(value, "%d.%m.%Y %H:%M:%S").unwrap_or_else(|| "—".into())
}

fn header_height(cols: i64, rows: i64) -> i64 {
    if rows >= 26 && cols >= 70 { 5 } else { 4 }
}

pub fn list_page_size(cols: i64, rows: i64) -> i64 {
    let header = header_height(cols, rows);
    let row_height = if rows >= 26 { 4 } else { 3 };
    // One blank line separates items; the last item needs no trailing gap.
    ((rows - 4 - (header + 7) + 1).div_euclid(row_height + 1)).max(1)
}

pub fn keep_selection_visible(view: &mut View, count: i64, page_size: i64) {
    view.selected = view.selected.min(count - 1).max(0);
    view.top = view.top.min(count - page_size).max(0);
    if view.selected < view.top {
        view.top = view.selected;
    } else if view.selected >= view.top + page_size {
        view.top = view.selected - page_size + 1;
    }
}

fn pinned_first(items: Vec<Entry>, pins: &HashSet<String>) -> Vec<Entry> {
    if pins.is_empty() {
        return items;
    }
    // Stable: pinned rows keep their recency order above the others.
    let (pinned, rest): (Vec<Entry>, Vec<Entry>) = items.into_iter().partition(|e| pins.contains(&e.key()));
    pinned.into_iter().chain(rest).collect()
}

pub fn scoped_sessions<'a>(sessions: &'a [Session], v: &View) -> std::borrow::Cow<'a, [Session]> {
    use std::borrow::Cow;
    match &v.project_path {
        None => Cow::Borrowed(sessions),
        Some(path) if !path.is_empty() => Cow::Owned(sessions.iter().filter(|s| s.in_cwd(path)).cloned().collect()),
        Some(_) => Cow::Owned(sessions.iter().filter(|s| s.cwd.is_empty() && s.workspaces.is_empty()).cloned().collect()),
    }
}

/// Visible rows, recomputed only when the data or a filter changed.
pub fn visible_entries(sessions: &[Session], v: &View) -> Rc<Vec<Entry>> {
    let key = v.entries_key(sessions);
    if let Some((cached, rows)) = &v.memo.borrow().entries
        && *cached == key
    {
        return rows.clone();
    }
    let rows = Rc::new(compute_entries(sessions, v));
    v.memo.borrow_mut().entries = Some((key, rows.clone()));
    rows
}

/// Sessions per client for the client chips (ignores client, query and pins).
fn client_counts(sessions: &[Session], v: &View) -> Rc<HashMap<&'static str, usize>> {
    let mut key = v.entries_key(sessions);
    key.client.clear();
    key.query.clear();
    key.pins.clear();
    key.tree = false;
    key.folder_path = None;
    key.scope = if v.tab == "projects" || v.project_path.is_some() { "all" } else { v.scope };
    key.tab = "";
    if let Some((cached, counts)) = &v.memo.borrow().counts
        && *cached == key
    {
        return counts.clone();
    }
    let counted = filter_sessions(
        &scoped_sessions(sessions, v),
        &Filter {
            cwd: &v.cwd,
            scope: key.scope,
            client: "all",
            query: "",
            children: v.children,
            archived: v.archived,
            pins: crate::model::empty_pins(),
        },
    );
    let counts: Rc<HashMap<&'static str, usize>> =
        Rc::new(CLIENTS.iter().map(|c| (*c, counted.iter().filter(|s| s.client == *c).count())).collect());
    v.memo.borrow_mut().counts = Some((key, counts.clone()));
    counts
}

fn compute_entries(sessions: &[Session], v: &View) -> Vec<Entry> {
    if v.tab == "projects" {
        if let Some(folder) = &v.folder_path {
            let entries = folder_entries(sessions, folder, &v.grouping());
            let (folders, own): (Vec<Entry>, Vec<Entry>) = entries.into_iter().partition(|e| matches!(e, Entry::Project(_)));
            return pinned_first(folders, &v.pins).into_iter().chain(pinned_first(own, &v.pins)).collect();
        }
        let projects: Vec<Entry> = group_projects(sessions, &v.grouping(), v.tree).into_iter().map(Entry::Project).collect();
        return if v.tree { projects } else { pinned_first(projects, &v.pins) };
    }
    let scoped = scoped_sessions(sessions, v);
    let scope = if v.project_path.is_some() { "all" } else { v.scope };
    let filter =
        Filter { cwd: &v.cwd, scope, client: &v.client, query: &v.query, children: v.children, archived: v.archived, pins: &v.pins };
    pinned_first(filter_sessions(&scoped, &filter).into_iter().map(Entry::Session).collect(), &v.pins)
}

fn draw_usage_strip(screen: &mut Screen, usage: Option<&Usage>, x: i64, top: i64, bottom: i64, strip: i64, refreshing: bool) {
    for y in top..bottom {
        screen.put(y, x, "│", "faint");
    }
    let (cx, cw) = (x + 2, strip - 4);
    let title = tr("LIMITS");
    screen.put_ex(top, cx, &title, "accent", false, true, Some(cw));
    if refreshing {
        screen.put(top, cx + width(&title) as i64 + 1, "↻", "cyan");
    }
    screen.hit(top, x, strip, Action::Tab("usage"), bottom - top);
    let Some(usage) = usage.filter(|u| u.loaded) else {
        screen.put_limit(top + 2, cx, &tr("loading…"), "muted", cw);
        return;
    };
    let mut y = top + 2;
    for parts in usage_strip_lines(usage, cw, now()) {
        if y >= bottom - 1 {
            break;
        }
        screen.spans(y, cx, &parts, false, Some(cw));
        y += 1;
    }
    screen.put_limit(bottom - 1, cx, &tr("U — full usage"), "faint", cw);
}

/// Put text, then repaint plain-word search matches in amber.
#[allow(clippy::too_many_arguments)]
fn put_highlighted(
    screen: &mut Screen,
    y: i64,
    x: i64,
    text: &str,
    style: &'static str,
    words: &[String],
    selected: bool,
    bold: bool,
    limit: i64,
) {
    screen.put_ex(y, x, text, style, selected, bold, Some(limit));
    if words.is_empty() || limit <= 0 {
        return;
    }
    let room = (screen.cols as i64 - x.max(0)).min(limit);
    let shown: Vec<char> = clip(&clean(text, 0).replace(['\n', '\t'], " "), room, true).chars().collect();
    let folded: Vec<char> = casefold(&shown.iter().collect::<String>()).chars().collect();
    if folded.len() != shown.len() {
        return; // Case folding changed lengths (for example ß); skip safely.
    }
    for word in words {
        let needle: Vec<char> = word.chars().collect();
        if needle.is_empty() || needle.len() > folded.len() {
            continue;
        }
        let mut start = 0;
        while start + needle.len() <= folded.len() {
            let Some(offset) = (start..=folded.len() - needle.len()).find(|i| folded[*i..*i + needle.len()] == needle[..]) else {
                break;
            };
            let prefix: String = shown[..offset].iter().collect();
            let piece: String = shown[offset..offset + needle.len()].iter().collect();
            screen.put_ex(y, x + width(&prefix) as i64, &piece, "amber", selected, true, None);
            start = offset + needle.len();
        }
    }
}

pub fn copy_payload(entry: &Entry, identity: bool) -> String {
    match entry {
        Entry::Project(p) => p.cwd.clone(),
        Entry::Session(s) if identity => s.id.clone(),
        Entry::Session(s) => {
            let command = s.command();
            if s.cwd.is_empty() { command } else { format!("cd {} && {command}", shell_quote(&s.cwd)) }
        }
    }
}

pub fn load_pins() -> HashSet<String> {
    match read_settings().get("pins") {
        Some(Value::Array(items)) => items.iter().map(crate::text::py_str).collect(),
        _ => HashSet::new(),
    }
}

pub fn open_projects(v: &mut View, root: bool) {
    if root {
        v.folder_path = None;
        v.folder_stack.clear();
        v.project_index = 0;
        v.project_top = 0;
    }
    v.tab = "projects";
    v.project_path = None;
    v.selected = v.project_index;
    v.top = v.project_top;
    v.query.clear();
    v.searching = false;
    v.narrow_detail = false;
    v.selected_key.clear();
}

pub fn enter_folder(v: &mut View, project: &Project) {
    v.folder_stack.push((v.folder_path.clone(), v.selected, v.top));
    v.folder_path = Some(project.cwd.clone());
    v.query.clear();
    v.searching = false;
    v.selected = 0;
    v.top = 0;
    v.detail_scroll = 0;
}

pub fn leave_folder(v: &mut View) {
    match v.folder_stack.pop() {
        Some((path, selected, top)) => {
            v.folder_path = path;
            v.selected = selected;
            v.top = top;
        }
        None => {
            v.folder_path = None;
            v.selected = 0;
            v.top = 0;
        }
    }
    v.query.clear();
    v.searching = false;
    v.detail_scroll = 0;
}

pub fn enter_project(v: &mut View, project: &Project) {
    v.project_index = v.selected;
    v.project_top = v.top;
    v.tab = "sessions";
    v.project_path = Some(project.cwd.clone());
    v.query.clear();
    v.searching = false;
    v.selected = 0;
    v.top = 0;
    v.detail_scroll = 0;
}

pub fn open_usage(v: &mut View) {
    if v.tab != "usage" {
        v.usage_tab = v.tab;
        v.usage_project = v.project_path.clone();
    }
    v.tab = "usage";
    v.message.clear();
}

pub fn close_usage(v: &mut View) {
    v.tab = if v.usage_tab != "usage" { v.usage_tab } else { "sessions" };
    v.project_path = v.usage_project.clone();
    v.message.clear();
}

type Lines = Vec<(String, &'static str)>;

fn adder(result: &mut Lines, maximum: i64) -> impl FnMut(&str, &'static str) + '_ {
    move |text: &str, style: &'static str| {
        for row in wrap(text, maximum) {
            result.push((row, style));
        }
    }
}

fn unique<T: Clone + Eq + std::hash::Hash>(items: impl IntoIterator<Item = T>) -> Vec<T> {
    let mut seen = HashSet::new();
    items.into_iter().filter(|i| seen.insert(i.clone())).collect()
}

pub fn project_detail_lines(project: &Project, maximum: i64, home: &Path, expanded: bool) -> Lines {
    let mut result = Lines::new();
    let mut add = adder(&mut result, maximum);
    let name = project.name();
    add(if name.is_empty() { tr("No directory") } else { name }.as_str(), "green");
    add(&tr("DIRECTORY"), "muted");
    add(&pretty_path(&project.cwd, Some(home)), "cyan");
    add(&trf!("{count} sessions · {elapsed}", count = project.sessions.len(), elapsed = age(project.updated(), None)), "muted");
    add(&(tr("Updated  ") + &date(project.updated())), "muted");
    add("", "text");
    for client in CLIENTS {
        let count = project.sessions.iter().filter(|s| s.client == client).count();
        if count > 0 {
            add(&format!("{}  {count}", label(client)), client_style(client));
        }
    }
    if expanded {
        let created: Vec<f64> = project.sessions.iter().map(|s| s.created).filter(|c| *c != 0.0).collect();
        if !created.is_empty() {
            add(&(tr("Created  ") + &date(created.iter().copied().fold(f64::INFINITY, f64::min))), "muted");
        }
        add("", "text");
        add(&tr("SAVED STATUSES"), "accent");
        for status in unique(project.sessions.iter().map(|s| s.status.clone())) {
            let count = project.sessions.iter().filter(|s| s.status == status).count();
            add(&format!("{}  {count}", status_text(&status)), status_style(&status));
        }
        let models: Vec<String> = project
            .sessions
            .iter()
            .map(|s| [s.model.as_str(), s.provider.as_str()].iter().filter(|v| !v.is_empty()).copied().collect::<Vec<_>>().join(" / "))
            .collect();
        let branches: Vec<String> = project.sessions.iter().map(|s| s.branch.clone()).collect();
        for (heading, values) in [(tr("MODEL / PROVIDER"), models), (tr("BRANCHES"), branches)] {
            let recorded: Vec<String> = values.into_iter().filter(|v| !v.is_empty()).collect();
            if !recorded.is_empty() {
                add("", "text");
                add(&heading, "accent");
                for value in unique(recorded.iter().cloned()) {
                    let count = recorded.iter().filter(|v| **v == value).count();
                    add(&format!("{value}  · {count}"), "cyan");
                }
            }
        }
    }
    add("", "text");
    add(&tr("RECENT SESSIONS"), "accent");
    let mut recent = project.sessions.clone();
    crate::model::sort_recent(&mut recent);
    let shown = if expanded { recent.len() } else { recent.len().min(3) };
    for session in &recent[..shown] {
        if expanded {
            add(if session.title.is_empty() { &session.id } else { &session.title }, "text");
        } else {
            add(&clip(&session.title, maximum, true), "text");
        }
        add(&format!("{} · {} · {}", label(&session.client), status_text(&session.status), age(session.updated, None)), "muted");
        if expanded {
            if !session.model.is_empty() || !session.branch.is_empty() {
                let parts: Vec<&str> = [session.model.as_str(), session.branch.as_str()].into_iter().filter(|v| !v.is_empty()).collect();
                add(&parts.join(" · "), "cyan");
            }
            let preview = [&session.last_action, &session.last_assistant, &session.last_user, &session.status_detail]
                .into_iter()
                .find(|v| !v.is_empty());
            if let Some(preview) = preview {
                add(&format!("↳ {}", line(preview, 0)), "muted");
            }
            add(&format!("ID  {}", session.id), "muted");
            add("", "text");
        }
    }
    drop(add);
    result
}

pub fn detail_lines(entry: &Entry, maximum: i64, home: &Path) -> Lines {
    let s = match entry {
        Entry::Project(p) => return project_detail_lines(p, maximum, home, true),
        Entry::Session(s) => s,
    };
    let mut result = Lines::new();
    {
        let mut add = adder(&mut result, maximum);
        let profile = if s.profile.is_empty() { String::new() } else { format!("  /  {}", s.profile) };
        add(&format!("{}{profile}", label(&s.client)), client_style(&s.client));
    }
    let title_rows = wrap(&s.title, maximum);
    for row in title_rows.iter().take(2) {
        result.push((row.clone(), "text"));
    }
    if title_rows.len() > 2 {
        let last = result.last_mut().unwrap();
        last.0 = clip(&last.0, (maximum - 1).max(1), true) + "…";
    }
    let mut add = adder(&mut result, maximum);
    let not_recorded = tr("Not recorded");
    let field = |add: &mut dyn FnMut(&str, &'static str), name: &str, value: &str, style: &'static str| {
        add(name, "muted");
        add(if value.is_empty() { &not_recorded } else { value }, style);
    };
    add("", "text");
    field(&mut add, &tr("DIRECTORY"), &pretty_path(&s.cwd, Some(home)), "cyan");
    if s.workspaces.len() > 1 {
        for cwd in &s.workspaces[1..] {
            add(&pretty_path(cwd, Some(home)), "cyan");
        }
    }
    let model: Vec<&str> = [s.model.as_str(), s.provider.as_str()].into_iter().filter(|v| !v.is_empty()).collect();
    field(&mut add, &tr("MODEL / PROVIDER"), &model.join(" · "), "text");
    field(&mut add, &tr("LAST SAVED STATUS"), &status_text(&s.status), status_style(&s.status));
    if !s.status_detail.is_empty() {
        add(&s.status_detail, "muted");
    }
    if !s.last_action.is_empty() {
        field(&mut add, &tr("LAST ACTION"), &s.last_action, "text");
    }
    add("", "text");
    if let Some(steps) = s.steps {
        add(&trf!("{v0} steps · {v1} tokens", v0 = steps, v1 = number(s.tokens)), "muted");
    } else {
        let prefix = if s.exact { "" } else { "≥" };
        add(
            &trf!(
                "{v0}{v1} messages · {v2}{v3} tool calls · {v4} tokens",
                v0 = prefix,
                v1 = s.messages,
                v2 = prefix,
                v3 = s.tools,
                v4 = number(s.tokens)
            ),
            "muted",
        );
    }
    if let Some(cost) = s.cost {
        add(&trf!("Cost reported by client: ${v0:.4f}", v0 = cost), "muted");
    }
    if !s.exact {
        add(&tr("Preview from the start and end of the file; details load when selected."), "muted");
    }
    if !s.branch.is_empty() {
        add(&format!("Git: {}", s.branch), "green");
    }
    add(&(tr("Updated  ") + &date(s.updated)), "muted");
    add(&format!("ID  {}", s.id), "muted");
    if !s.parent.is_empty() {
        field(&mut add, &tr("PARENT SESSION"), &s.parent, "muted");
    }
    if !s.todo.is_empty() {
        add("", "text");
        add(&tr("TASKS"), "accent");
        for task in &s.todo {
            let done = task.status == "completed" || task.status == "done";
            add(&format!("{}{}", if done { "✓ " } else { "◦ " }, task.text), if done { "green" } else { "text" });
        }
    }
    let missing = tr("Text is not saved in the available index");
    add("", "text");
    add(&tr("LAST PROMPT"), "blue");
    add(if s.last_user.is_empty() { &missing } else { &s.last_user }, "muted");
    add("", "text");
    add(&tr("LAST RESPONSE"), "green");
    add(if s.last_assistant.is_empty() { &missing } else { &s.last_assistant }, "text");
    add("", "text");
    add(&(tr("RESUME IN ") + &label(&s.client).to_uppercase()), client_style(&s.client));
    add(&s.command(), "cyan");
    if which(&s.client).is_none() {
        add(&tr("CLI not found in PATH"), "amber");
    }
    if s.cwd.is_empty() || !Path::new(&s.cwd).is_dir() {
        add(&tr("Original directory unavailable"), "amber");
    }
    if !s.note.is_empty() {
        add(&s.note, "muted");
    }
    add("", "text");
    field(&mut add, &tr("SOURCE"), if s.source.is_empty() { &s.client } else { &s.source }, "muted");
    add(&(tr("Created  ") + &date(s.created)), "muted");
    add(&pretty_path(&s.path, Some(home)), "muted");
    drop(add);
    result
}

/// (wide, list width, usage strip width): identical for Projects and Sessions.
pub fn layout(cols: i64, view: &View) -> (bool, i64, i64) {
    let wide = cols >= 112 && !view.narrow_detail;
    let strip = if wide && cols >= STRIP_MIN_COLS { STRIP_WIDTH } else { 0 };
    let left = if wide { 54.max(((cols - strip) as f64 * 0.52) as i64) } else { cols - 3 };
    (wide, left, strip)
}

pub fn detail_width(cols: i64, view: &View) -> i64 {
    let (wide, left, strip) = layout(cols, view);
    if !wide {
        return cols - 6;
    }
    if strip > 0 { (cols - strip - 1) - (left + 1) - 4 } else { cols - left - 7 }
}

pub fn detail_scroll_limit(entry: Option<&Entry>, view: &View, cols: i64, rows: i64) -> i64 {
    let Some(entry) = entry else { return 0 };
    if matches!(entry, Entry::Project(_)) && !view.narrow_detail {
        return 0;
    }
    let header = header_height(cols, rows);
    let wide = layout(cols, view).0;
    let visible = (rows - 4 - (header + 7) - i64::from(wide)).max(1);
    (detail_lines(entry, detail_width(cols, view).max(4), &view.home).len() as i64 - visible).max(0)
}

pub fn render(sessions: &[Session], v: &mut View, cols: usize, rows: usize, errors: &[String], usage: Option<&Usage>) -> Screen {
    let mut screen = Screen::new(cols, rows);
    let (cols, rows) = (cols as i64, rows as i64);
    if cols < 44 || rows < 16 {
        screen.put_bold(1, 1, "ais · AI Sessions", "accent");
        screen.put(3, 1, &tr("Enlarge terminal: minimum 44 × 16"), "amber");
        screen.put(5, 1, &tr("Q / Ctrl+C — quit"), "muted");
        return screen;
    }
    let header = if rows >= 26 && cols >= 70 {
        for (i, row) in WORDMARK.iter().enumerate() {
            // A, i, and s keep their own colors in the terminal wordmark.
            for (x, c) in row.chars().enumerate() {
                let style = if x < 6 {
                    "cyan"
                } else if x < 10 {
                    "accent"
                } else {
                    "green"
                };
                screen.put_ex(i as i64 + 1, x as i64 + 2, &c.to_string(), style, false, true, None);
            }
        }
        screen.put_bold(1, 22, "AI SESSIONS", "text");
        screen.put(2, 22, &tr("All conversations. One entry point."), "muted");
        screen.put_limit(3, 22, &pretty_path(&v.cwd, Some(&v.home)), "cyan", cols - 25);
        5
    } else {
        screen.put_bold(1, 2, "◈ ais", "accent");
        screen.put_bold(1, 10, "AI SESSIONS", "text");
        screen.put_limit(2, 2, &pretty_path(&v.cwd, Some(&v.home)), "cyan", cols - 5);
        4
    };
    screen.rule(header - 1);
    let tabs: [(&'static str, String, &'static str); 3] =
        [("projects", tr(" Projects [P] "), "green"), ("sessions", tr(" Sessions [S] "), "text"), ("usage", " Usage [U] ".into(), "cyan")];
    let mut x = 2;
    for (name, text, style) in &tabs {
        let text = if cols >= 65 { text.clone() } else { format!(" [{}] ", name[..1].to_uppercase()) };
        let chosen = v.tab == *name;
        screen.put_ex(header + 1, x, &text, style, chosen, chosen, None);
        screen.hit(header + 1, x, width(&text) as i64, Action::Tab(name), 1);
        x += width(&text) as i64 + 1;
    }
    screen.put_limit(header + 1, cols - 10, &format!("{} [L]", language().to_uppercase()), "muted", 8);
    screen.hit(header + 1, cols - 10, 6, Action::Key('L'), 1);
    let mut x = 2;
    for (name, text) in [("cwd", tr("Current directory")), ("all", tr("All projects"))] {
        let chosen = v.scope == name && v.tab != "projects" && v.project_path.is_none();
        let text = format!("{}{text} ", if chosen { " ● " } else { " ○ " });
        screen.put_ex(header, x, &text, if chosen { "accent" } else { "muted" }, chosen, chosen, None);
        screen.hit(header, x, width(&text) as i64, Action::Scope(name), 1);
        x += width(&text) as i64 + 2;
    }
    let hint = tr("Tab — switch");
    if cols >= 80 {
        screen.put(header, cols - width(&hint) as i64 - 3, &hint, "muted");
    }
    let counts = client_counts(sessions, v);
    let mut x = 2;
    for client in std::iter::once("all").chain(CLIENTS) {
        let name = if client == "all" { tr("All") } else { label(client).to_string() };
        let count = if client == "all" { counts.values().sum() } else { counts[client] };
        let text = format!(" {name} {count} ");
        if x + width(&text) as i64 >= cols - 1 {
            break;
        }
        let chosen = v.client == client;
        screen.put_ex(header + 2, x, &text, if client == "all" { "text" } else { client_style(client) }, chosen, chosen, None);
        screen.hit(header + 2, x, width(&text) as i64, Action::Client(client.into()), 1);
        x += width(&text) as i64 + 1;
    }
    if x + 14 < cols - 1 && cols >= 100 {
        screen.put(header + 2, x + 1, &tr("0–6 · ←→"), "faint");
    }
    let mut flags: Vec<(String, &'static str)> = Vec::new();
    if v.children {
        flags.push((tr("◆ child sessions"), "cyan"));
    }
    if v.archived {
        flags.push((tr("◆ archive"), "cyan"));
    }
    if v.tree && v.tab == "projects" {
        flags.push((tr("◆ tree"), "green"));
    }
    let flags_text = flags.iter().map(|f| f.0.as_str()).collect::<Vec<_>>().join("  ");
    let flags_room = if !flags.is_empty() && cols >= 80 { width(&flags_text) as i64 + 3 } else { 0 };
    let search = if !v.query.is_empty() || v.searching {
        format!("⌕ {}{}", v.query, if v.searching { "▏" } else { "" })
    } else {
        tr("⌕ Search: title, directory, model, ID, last message…")
            + &if cols - flags_room >= 120 { tr("   client: model: status: since:3d -word") } else { String::new() }
    };
    screen.put_limit(header + 4, 2, &search, if !v.query.is_empty() || v.searching { "text" } else { "muted" }, cols - 4 - flags_room);
    screen.hit(header + 4, 2, cols - 4 - flags_room, Action::Search, 1);
    if flags_room > 0 {
        let mut parts: Vec<Part> = Vec::new();
        for (i, (text, style)) in flags.iter().enumerate() {
            if i > 0 {
                parts.push(part("  ", "faint"));
            }
            parts.push(part(text.clone(), style));
        }
        screen.spans(header + 4, cols - width(&flags_text) as i64 - 2, &parts, false, None);
    }
    let words = parse_query(&v.query).highlights();
    screen.rule(header + 5);
    let filtered = visible_entries(sessions, v);
    keep_selection_visible(v, filtered.len() as i64, list_page_size(cols, rows));
    let selected = v.selected;
    let current = filtered.get(selected as usize);
    v.detail_scroll = v.detail_scroll.min(detail_scroll_limit(current, v, cols, rows)).max(0);
    let (start, bottom) = (header + 7, rows - 4);
    let (wide, left_width, strip) = layout(cols, v);
    let mut title = trf!("SESSIONS  {v0}", v0 = filtered.len())
        + &if filtered.len() != sessions.len() { format!(" / {}", sessions.len()) } else { String::new() };
    if v.tab == "projects" {
        title = trf!("PROJECTS  {v0}", v0 = filtered.len());
        if let Some(folder) = &v.folder_path {
            title += &format!(" · {}", pretty_path(folder, Some(&v.home)));
        }
    } else if let Some(project) = &v.project_path {
        title = format!("{} · {title}", pretty_path(project, Some(&v.home)));
    }
    if !v.loaded {
        title += &tr("  ·  loading history…");
    }
    screen.put_ex(start - 1, 2, &title, "muted", false, true, Some(left_width - 3));
    if let (true, Some(current)) = (v.narrow_detail, current) {
        for (i, (text, style)) in detail_lines(current, cols - 6, &v.home).iter().skip(v.detail_scroll as usize).enumerate() {
            if start + i as i64 >= bottom {
                break;
            }
            screen.put_limit(start + i as i64, 3, text, style, cols - 6);
        }
    } else if !filtered.is_empty() {
        let row_height = if rows >= 26 { 4 } else { 3 };
        let capacity = list_page_size(cols, rows);
        let top = v.top;
        for (index, entry) in filtered.iter().enumerate().skip(top as usize).take(capacity as usize) {
            let index = index as i64;
            let y = start + (index - top) * (row_height + 1);
            let chosen = index == selected;
            for sub in 0..row_height {
                screen.fill(y + sub, 2, left_width - 3, chosen);
            }
            screen.put_ex(y, 2, if chosen { "▎" } else { " " }, "accent", chosen, false, None);
            let stripe = if index % 2 == 1 { "stripe" } else { "row" };
            match entry {
                Entry::Project(project) => {
                    let prefix = clip(&project.prefix, (left_width - 24).max(0), false);
                    let mut indent = width(&prefix) as i64;
                    let mut name_x = 4 + indent;
                    if !prefix.is_empty() {
                        screen.put_ex(y, 4, &prefix, "faint", chosen, false, None);
                    }
                    if v.pins.contains(&project.key()) {
                        screen.put_ex(y, name_x, "★", "amber", chosen, true, None);
                        name_x += 2;
                        indent += 2;
                    }
                    let name = project.name();
                    let shown = if name.is_empty() { tr("No directory") } else { name.clone() };
                    put_highlighted(&mut screen, y, name_x, &shown, "text", &words, chosen, true, (left_width - 8 - indent).max(1));
                    if !project.children.is_empty() && !v.tree {
                        let nested = trf!("  ▸ subfolders: {count}", count = project.children.len());
                        let w = width(&name) as i64;
                        screen.put_ex(
                            y,
                            (left_width - 4).min(name_x + w + 1),
                            &nested,
                            "faint",
                            chosen,
                            false,
                            Some((left_width - name_x - w - 5).max(0)),
                        );
                    }
                    put_highlighted(
                        &mut screen,
                        y + 1,
                        name_x,
                        &pretty_path(&project.cwd, Some(&v.home)),
                        "cyan",
                        &words,
                        chosen,
                        false,
                        (left_width - 7 - indent).max(1),
                    );
                    let mut counts: Vec<Part> = Vec::new();
                    for c in CLIENTS {
                        let total = project.sessions.iter().filter(|s| s.client == c).count();
                        if total > 0 {
                            counts.push((format!("{} {total}", label(c)), client_style(c), chosen));
                            counts.push(part(" · ", "faint"));
                        }
                    }
                    counts.pop();
                    let summary =
                        trf!("{count} sessions · {elapsed}", count = project.sessions.len(), elapsed = age(project.updated(), None));
                    let room = (left_width - 8 - indent).max(1);
                    if row_height == 4 {
                        screen.put_ex(y + 2, name_x, &summary, "muted", chosen, false, Some(room));
                        screen.spans(y + 3, name_x, &counts, chosen, Some(room));
                    } else {
                        let mut parts = vec![part(format!("{summary} · "), "muted")];
                        parts.extend(counts);
                        screen.spans(y + 2, name_x, &parts, chosen, Some(room));
                    }
                }
                Entry::Session(session) => {
                    let elapsed = age(session.updated, None);
                    let mut title_x = 4;
                    if v.pins.contains(&session.key()) {
                        screen.put_ex(y, 4, "★", "amber", chosen, true, None);
                        title_x = 6;
                    }
                    let shown = if session.title.is_empty() { &session.id } else { &session.title };
                    put_highlighted(
                        &mut screen,
                        y,
                        title_x,
                        shown,
                        "text",
                        &words,
                        chosen,
                        chosen,
                        left_width - width(&elapsed) as i64 - 5 - title_x,
                    );
                    screen.put_ex(y, left_width - width(&elapsed) as i64 - 2, &elapsed, "muted", chosen, false, None);
                    screen.put_ex(y + 1, 4, label(&session.client), client_style(&session.client), chosen, true, None);
                    let state = status_text(&session.status);
                    screen.put_ex(y + 1, 13, &format!("● {state}"), status_style(&session.status), chosen, false, Some(21));
                    if left_width >= 64 {
                        let model = if session.model.is_empty() { tr("model not recorded") } else { session.model.clone() };
                        screen.put_ex(y + 1, 36, &model, "muted", chosen, false, Some(left_width - 39));
                    }
                    let location = pretty_path(&session.cwd, Some(&v.home))
                        + &if session.branch.is_empty() { String::new() } else { format!(" · {}", session.branch) };
                    put_highlighted(&mut screen, y + 2, 4, &location, "cyan", &words, chosen, false, left_width - 7);
                    if row_height == 4 {
                        let preview = [&session.last_action, &session.last_assistant, &session.last_user]
                            .into_iter()
                            .find(|t| !t.is_empty())
                            .unwrap_or(&session.status_detail);
                        put_highlighted(
                            &mut screen,
                            y + 3,
                            4,
                            &format!("↳ {}", line(preview, 0)),
                            "muted",
                            &words,
                            chosen,
                            false,
                            left_width - 7,
                        );
                    }
                }
            }
            for sub in 0..row_height {
                screen.shade(y + sub, 2, left_width - 3, stripe);
            }
            screen.hit(y, 2, left_width - 3, Action::Row(index as usize), row_height);
        }
        if filtered.len() as i64 > capacity {
            let position = format!("{} / {}", selected + 1, filtered.len());
            screen.put(start - 1, left_width - width(&position) as i64 - 3, &position, "muted");
        }
    } else {
        screen.put_bold(start + 1, 4, &tr("No matching sessions yet"), "text");
        screen.put_limit(start + 3, 4, &tr("Tab — all projects   ·   / — search   ·   N — new"), "muted", left_width - 6);
        if !v.query.is_empty() {
            screen.put(start + 5, 4, &tr("Esc — clear search"), "muted");
        }
        let y = if v.query.is_empty() { start + 5 } else { start + 7 };
        screen.put_limit(y, 4, &tr("? — search syntax and all keys"), "faint", left_width - 6);
    }
    if wide {
        let divider = left_width + 1;
        for y in start - 1..bottom {
            screen.put(y, divider, "│", "faint");
        }
        let (dx, dw) = (divider + 3, detail_width(cols, v));
        let busy = if v.busy_detail { tr(" · loading…") } else { String::new() };
        screen.put_ex(start - 1, dx, &(tr("DETAILS") + &busy), "accent", false, true, Some(dw));
        match current {
            Some(Entry::Project(project)) => {
                for (i, (text, style)) in project_detail_lines(project, dw, &v.home, false).iter().enumerate() {
                    if start + i as i64 >= bottom - 1 {
                        break;
                    }
                    screen.put_limit(start + i as i64, dx, text, style, dw);
                }
                screen.put_limit(bottom - 1, dx, &tr("D expand · O sessions · E folder · F pin"), "muted", dw);
            }
            Some(entry) => {
                for (i, (text, style)) in detail_lines(entry, dw, &v.home).iter().skip(v.detail_scroll as usize).enumerate() {
                    if start + i as i64 >= bottom - 1 {
                        break;
                    }
                    screen.put_limit(start + i as i64, dx, text, style, dw);
                }
                screen.put_limit(bottom - 1, dx, &tr("[ ] scroll · D expand · Y copy · F pin"), "muted", dw);
            }
            None => screen.put_limit(start + 1, dx, &tr("Select a session on the left"), "muted", dw),
        }
        if strip > 0 {
            draw_usage_strip(&mut screen, usage, cols - strip, start - 1, bottom, strip, v.usage_refreshing);
        }
    }
    screen.rule(rows - 4);
    let mut primary = if cols >= 75 {
        tr("↑↓ select   Enter resume   Tab directory/all   ←→ client   / search")
    } else {
        tr("↑↓ select  Enter run  Tab scope  / search")
    };
    if v.tab == "projects" {
        primary = tr("↑↓ select   Enter open   O folder sessions   ←→ client   / search");
    }
    let mut secondary = if cols >= 100 {
        tr("N new   F pin   Y copy   E folder   D details   R refresh   ? help   Esc back   Q quit")
    } else {
        tr("N new  F pin  Y copy  D details  ? help  Esc back  Q quit")
    };
    if v.tab == "projects" {
        secondary = if cols >= 100 {
            tr("T tree   F pin   E folder   D details   S sessions   U usage   ? help   Esc back   Q quit")
        } else {
            tr("T tree  F pin  D details  S sessions  ? help  Esc back  Q quit")
        };
    }
    screen.put_limit(rows - 3, 2, &primary, "text", cols - 4);
    screen.put_limit(rows - 2, 2, &secondary, "muted", cols - 4);
    let note = if !v.message.is_empty() {
        v.message.clone()
    } else if !errors.is_empty() {
        trf!("{v0} read errors · ? details", v0 = errors.len())
    } else {
        tr("Statuses saved in history · source files are read-only")
    };
    screen.put_limit(rows - 1, 2, &note, if !v.message.is_empty() || !errors.is_empty() { "amber" } else { "faint" }, cols - 4);
    if !v.modal.is_empty() {
        draw_modal(&mut screen, v, errors);
    }
    screen
}

const HELP_SECTIONS: [(&str, &[(&str, &str)]); 4] = [
    (
        "NAVIGATION",
        &[
            ("↑↓  j k", "select"),
            ("PgUp PgDn", "page up / down"),
            ("Home End  g G", "first / last"),
            ("Enter", "open folder · resume session"),
            ("Esc  Backspace", "back · reset filters"),
            ("Tab", "current directory / all projects"),
            ("← →  0–6", "filter by client"),
            ("Mouse", "click select · double-click open"),
        ],
    ),
    (
        "ACTIONS",
        &[
            ("N", "new session in current directory"),
            ("Shift+N", "new session in selected directory"),
            ("F", "pin / unpin (pinned stay on top)"),
            ("y", "copy resume command"),
            ("Y", "copy session ID / folder path"),
            ("E", "open folder in file manager"),
            ("O", "open the selected folder sessions"),
        ],
    ),
    (
        "VIEW",
        &[
            ("P  S  U", "projects · sessions · usage"),
            ("T", "project tree on / off"),
            ("D  [ ]", "expand details · scroll"),
            ("C  A", "child sessions · Codex archive"),
            ("L", "language: English / Russian / French"),
            ("R  rr", "refresh · restart ais"),
            ("Q  Ctrl+C", "quit"),
        ],
    ),
    (
        "SEARCH  / or .",
        &[
            ("word  -word", "must contain / must not contain"),
            ("client:claude", "also c: · cli:"),
            ("model:opus", "model or provider · m:"),
            ("status:error", "saved status · st:"),
            ("branch:main  dir:api", "git branch · directory (b: in:)"),
            ("since:3d", "90m · 6h · 2w · today · yesterday"),
            ("is:pinned", "is:child · is:archived"),
            ("Ctrl+U  Ctrl+W", "clear query · delete word"),
        ],
    ),
];

type HelpRow = Vec<(String, &'static str)>;

/// Help as rows of (text, style) parts; two sections per row when wide.
pub fn help_lines(columns: usize) -> Vec<HelpRow> {
    let section = |index: usize, key_width: usize| -> Vec<HelpRow> {
        let (title, rows) = HELP_SECTIONS[index];
        let mut result = vec![vec![(tr(title), "title")]];
        for (key, text) in rows {
            result.push(vec![(crate::text::ljust(key, key_width), "cyan"), (tr(text), "text")]);
        }
        result
    };
    if columns == 2 {
        let left: Vec<HelpRow> = [section(0, 16), vec![vec![]], section(1, 16)].concat();
        let right: Vec<HelpRow> = [section(2, 22), vec![vec![]], section(3, 22)].concat();
        return (0..left.len().max(right.len()))
            .map(|i| {
                let mut row = left.get(i).cloned().unwrap_or_default();
                row.push(("\t".into(), ""));
                row.extend(right.get(i).cloned().unwrap_or_default());
                row
            })
            .collect();
    }
    let mut rows = Vec::new();
    for index in 0..HELP_SECTIONS.len() {
        rows.extend(section(index, if index >= 2 { 22 } else { 16 }));
        rows.push(vec![]);
    }
    rows.pop();
    rows
}

fn draw_help(screen: &mut Screen, v: &mut View, errors: &[String]) {
    let (cols, rows) = (screen.cols as i64, screen.rows as i64);
    let two = cols >= 104;
    let w = (if two { 120 } else { 84 }).min(cols - 6);
    let mut lines = help_lines(if two { 2 } else { 1 });
    lines.push(vec![]);
    let mut colors: HelpRow = vec![(tr("Client colors: "), "muted")];
    for c in CLIENTS {
        colors.push((label(c).into(), client_style(c)));
        colors.push(("  ".into(), "muted"));
    }
    lines.push(colors);
    lines.push(vec![(tr("Statuses describe the last saved record."), "muted")]);
    if !errors.is_empty() {
        lines.push(vec![]);
        lines.push(vec![(tr("READ ERRORS"), "amber")]);
        for e in errors.iter().take(4) {
            lines.push(vec![(line(e, 0), "amber")]);
        }
    }
    let viewport = (rows - 8).max(3);
    v.help_scroll = v.help_scroll.min(lines.len() as i64 - viewport).max(0);
    let shown: Vec<&HelpRow> = lines.iter().skip(v.help_scroll as usize).take(viewport as usize).collect();
    let height = shown.len() as i64 + 4;
    let (x, y) = ((cols - w) / 2, ((rows - height) / 2).max(1));
    screen.hit(0, 0, cols, Action::Close, rows);
    screen.hit(y, x, w, Action::Noop, height);
    for i in 0..height {
        screen.fill(y + i, x, w, true);
    }
    screen.put_ex(y + 1, x + 2, &tr("AI SESSIONS · CONTROLS"), "accent", true, true, Some(w - 4));
    let more = lines.len() as i64 > viewport;
    let hint = if more { tr("↑↓ scroll · Esc / ? close") } else { tr("Esc / ? — close") };
    screen.put_ex(y + 1, x + w - width(&hint) as i64 - 2, &hint, "muted", true, false, None);
    let column = (w - 4) / 2 + 2;
    for (i, parts) in shown.iter().enumerate() {
        let mut px = x + 2;
        for (text, style) in parts.iter() {
            if text == "\t" {
                px = x + column;
                continue;
            }
            let bold = *style == "title";
            let style = if bold { "accent" } else { style };
            let end = if px >= x + column || !two { x + w - 2 } else { x + column - 2 };
            screen.put_ex(y + 3 + i as i64, px, text, style, true, bold, Some((end - px).max(0)));
            px += width(text) as i64;
        }
    }
}

pub fn draw_modal(screen: &mut Screen, v: &mut View, errors: &[String]) {
    if v.modal == "help" {
        draw_help(screen, v, errors);
        return;
    }
    let (cols, rows) = (screen.cols as i64, screen.rows as i64);
    let w = 84.min(cols - 8);
    let (x, y) = ((cols - w) / 2, ((rows - 18) / 2).max(2));
    let mut options: HashMap<usize, Action> = HashMap::new();
    let mut lines: Lines;
    if v.modal == "language" {
        lines = vec![
            (tr("LANGUAGE"), "accent"),
            (String::new(), "text"),
            (tr("↑↓ choose · Enter save · Esc cancel"), "muted"),
            (String::new(), "text"),
        ];
        for (i, name) in ["English", "Русский", "Français"].iter().enumerate() {
            options.insert(lines.len(), Action::Lang(i));
            lines.push((format!("{}{name}", if i == v.language_index { "› " } else { "  " }), "text"));
        }
    } else {
        lines = vec![
            (tr("NEW SESSION"), "accent"),
            (String::new(), "text"),
            (tr("Choose CLI: ↑↓ or 1–6 · Enter — launch"), "muted"),
            (tr("Directory: ") + &pretty_path(if v.new_cwd.is_empty() { &v.cwd } else { &v.new_cwd }, Some(&v.home)), "cyan"),
            (String::new(), "text"),
        ];
        for (i, client) in CLIENTS.iter().enumerate() {
            let available = which(client).is_some();
            options.insert(lines.len(), Action::New(i));
            lines.push((
                format!(
                    "{}{}  {}{}",
                    if i == v.new_client { "› " } else { "  " },
                    i + 1,
                    label(client),
                    if available { String::new() } else { tr(" · not installed") }
                ),
                if available { client_style(client) } else { "faint" },
            ));
        }
        lines.push((String::new(), "text"));
        lines.push((tr("Esc — cancel"), "muted"));
    }
    lines.truncate((rows - 6).max(0) as usize);
    screen.hit(0, 0, cols, Action::Close, rows);
    screen.hit(y, x, w, Action::Noop, lines.len() as i64 + 2);
    for i in 0..lines.len() as i64 + 2 {
        screen.fill(y + i, x, w, true);
    }
    for (i, (text, style)) in lines.iter().enumerate() {
        screen.put_ex(y + i as i64 + 1, x + 2, text, style, true, i == 0, Some(w - 4));
        if let Some(action) = options.get(&i) {
            screen.hit(y + i as i64 + 1, x + 1, w - 2, action.clone(), 1);
        }
    }
}

// ---------------------------------------------------------------- terminal

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Key {
    Char(char),
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Enter,
    Tab,
    Backspace,
    Esc,
    CtrlC,
    CtrlU,
    CtrlW,
    Resize,
    Click(i64, i64),
}

/// What the browser asks the launcher to do next.
pub enum Outcome {
    Quit,
    Restart,
    Resume(Box<Session>),
    New(String, String),
}

struct Input;

impl Input {
    /// (key, came from a wheel) or None on timeout / ignored events.
    fn read(&mut self, wait: Duration) -> Option<(Key, bool)> {
        if !event::poll(wait).ok()? {
            return None;
        }
        match event::read().ok()? {
            Event::Key(key) => {
                if key.kind == KeyEventKind::Release {
                    return None;
                }
                let control = key.modifiers.contains(KeyModifiers::CONTROL);
                Some((
                    match key.code {
                        KeyCode::Char('c') if control => Key::CtrlC,
                        KeyCode::Char('u') if control => Key::CtrlU,
                        KeyCode::Char('w') if control => Key::CtrlW,
                        KeyCode::Char('h') if control => Key::Backspace,
                        KeyCode::Char('j') if control => Key::Enter,
                        KeyCode::Char('m') if control => Key::Enter,
                        KeyCode::Char(c) if !control => Key::Char(c),
                        KeyCode::Up => Key::Up,
                        KeyCode::Down => Key::Down,
                        KeyCode::Left => Key::Left,
                        KeyCode::Right => Key::Right,
                        KeyCode::PageUp => Key::PageUp,
                        KeyCode::PageDown => Key::PageDown,
                        KeyCode::Home => Key::Home,
                        KeyCode::End => Key::End,
                        KeyCode::Enter => Key::Enter,
                        KeyCode::Tab => Key::Tab,
                        KeyCode::Backspace => Key::Backspace,
                        KeyCode::Esc => Key::Esc,
                        _ => return None,
                    },
                    false,
                ))
            }
            Event::Mouse(mouse) => {
                match mouse.kind {
                    MouseEventKind::ScrollUp => Some((Key::Up, true)),
                    MouseEventKind::ScrollDown => Some((Key::Down, true)),
                    // Clicks are discrete commands, never part of a wheel burst.
                    MouseEventKind::Down(MouseButton::Left) => Some((Key::Click(mouse.column as i64, mouse.row as i64), false)),
                    _ => None,
                }
            }
            Event::Resize(_, _) => Some((Key::Resize, false)),
            Event::Paste(text) => text.chars().next().map(|c| (Key::Char(c), false)),
            _ => None,
        }
    }

    /// Drain queued scroll events of the same kind (wheel reports, or arrow
    /// keys that terminals emit for wheel emulation / key repeat). Returns the
    /// final direction, how many queued events point that way, and the first
    /// unrelated event to process next.
    fn scroll_burst(&mut self, mut direction: Key, from_mouse: bool) -> (Key, i64, Option<(Key, bool)>) {
        let mut count = 1;
        loop {
            if !event::poll(Duration::ZERO).unwrap_or(false) {
                return (direction, count, None);
            }
            match self.read(Duration::ZERO) {
                None => continue,
                Some((key, mouse)) if mouse == from_mouse && matches!(key, Key::Up | Key::Down) => {
                    if key == direction {
                        count += 1;
                    } else {
                        direction = key;
                        count = 1;
                    }
                }
                Some(other) => return (direction, count, Some(other)),
            }
        }
    }
}

struct TerminalGuard;

static ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn leave_terminal_if_active() {
    if ACTIVE.swap(false, std::sync::atomic::Ordering::SeqCst) {
        leave_terminal();
    }
}

impl TerminalGuard {
    fn enter() -> std::io::Result<Self> {
        ACTIVE.store(true, std::sync::atomic::Ordering::SeqCst);
        terminal::enable_raw_mode()?;
        let mut out = std::io::stdout();
        // Save/disable alternate-scroll arrow emulation while receiving mouse
        // reports (wheel and press only: no motion or drag tracking).
        write!(out, "\x1b[?1007s\x1b[?1006s")?;
        execute!(out, terminal::EnterAlternateScreen, cursor::Hide, terminal::Clear(terminal::ClearType::All))?;
        #[cfg(windows)]
        execute!(out, event::EnableMouseCapture)?;
        #[cfg(not(windows))]
        write!(out, "\x1b[?1007l\x1b[?1000h\x1b[?1006h")?;
        out.flush()?;
        Ok(Self)
    }
}

pub fn leave_terminal() {
    let mut out = std::io::stdout();
    #[cfg(windows)]
    let _ = execute!(out, event::DisableMouseCapture);
    let _ = write!(out, "\x1b[?1000l\x1b[?1006l");
    let _ = execute!(out, SetAttribute(Attribute::Reset), cursor::Show, terminal::LeaveAlternateScreen);
    let _ = write!(out, "\x1b[?1006r\x1b[?1007r");
    let _ = out.flush();
    let _ = terminal::disable_raw_mode();
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        leave_terminal_if_active();
    }
}

fn background(state: &str) -> Color {
    Color::AnsiValue(match state {
        "selected" => 238,
        "row" => 235,
        "stripe" => 236,
        _ => 233,
    })
}

fn paint(screen: &Screen, previous: &mut Vec<String>) -> std::io::Result<()> {
    let mut out = std::io::stdout().lock();
    let full = previous.len() != screen.rows;
    if full {
        *previous = vec![String::new(); screen.rows];
        queue!(out, SetBackgroundColor(background("default")), terminal::Clear(terminal::ClearType::All))?;
    }
    for (y, slot) in previous.iter_mut().enumerate() {
        let runs = screen.runs(y);
        let signature = format!("{runs:?}");
        if !full && *slot == signature {
            continue;
        }
        queue!(out, cursor::MoveTo(0, y as u16))?;
        for (_, text, (style, selected, bold, bg)) in runs {
            let state = if selected {
                "selected"
            } else if bg.is_empty() {
                "default"
            } else {
                bg
            };
            queue!(
                out,
                SetAttribute(Attribute::Reset),
                SetForegroundColor(Color::AnsiValue(ansi_color(style))),
                SetBackgroundColor(background(state))
            )?;
            if bold {
                queue!(out, SetAttribute(Attribute::Bold))?;
            }
            queue!(out, Print(text))?;
        }
        *slot = signature;
    }
    queue!(out, SetAttribute(Attribute::Reset))?;
    out.flush()
}

enum Msg {
    Batch(Vec<Session>, Vec<String>),
    Done(Vec<Session>, Vec<String>, IndexMap<String, SourceInfo>),
    Error(String),
    Detail(u64, Box<Session>),
    DetailError(u64, String),
    Usage(Box<Usage>),
    UsageLive(Box<Usage>),
    UsageFailure(String),
    UsageDone,
}

fn spawn_scan(collector: &Collector, tx: &mpsc::Sender<Msg>) {
    let mut worker = collector.clone();
    worker.reset();
    let tx = tx.clone();
    std::thread::spawn(move || {
        let progress = tx.clone();
        let mut publish = move |rows: &[Session], errors: &[String]| {
            let _ = progress.send(Msg::Batch(rows.to_vec(), errors.to_vec()));
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| worker.scan(Some(&mut publish))));
        match result {
            Ok(rows) => {
                let _ = tx.send(Msg::Done(rows, worker.errors.clone(), worker.sources.clone()));
            }
            Err(_) => {
                let _ = tx.send(Msg::Error("history scan failed".into()));
            }
        }
    });
}

fn spawn_usage(collector: &Collector, tx: &mpsc::Sender<Msg>, cancel: Cancel, live: bool) -> std::thread::JoinHandle<()> {
    let collector = collector.clone();
    let tx = tx.clone();
    std::thread::spawn(move || {
        let _ = tx.send(Msg::Usage(Box::new(load_usage(&collector))));
        if live {
            match fetch_live_usage(&collector, &cancel, 30.0) {
                Ok(usage) => {
                    let _ = tx.send(Msg::UsageLive(Box::new(usage)));
                }
                Err(crate::usage::UsageError::Cancelled) => {}
                Err(error) => {
                    if !cancel.is_set() {
                        let _ = tx.send(Msg::UsageFailure(line(&error.safe(), 0)));
                    }
                }
            }
        }
        let _ = tx.send(Msg::UsageDone);
    })
}

fn entry_key(entries: &[Entry], index: i64) -> String {
    if entries.is_empty() { String::new() } else { entries[(index.max(0) as usize).min(entries.len() - 1)].key() }
}

pub fn browse(collector: &mut Collector, view: &mut View) -> std::io::Result<Outcome> {
    let (tx, rx) = mpsc::channel::<Msg>();
    let mut sessions: Vec<Session> = Vec::new();
    let mut detail_cache: HashMap<String, Session> = HashMap::new();
    let mut requested: HashSet<String> = HashSet::new();
    let mut usage = view.usage.clone().unwrap_or_default();
    let mut usage_requested = false;
    let mut usage_refreshing = false;
    let mut usage_cancel = Cancel::default();
    let mut usage_thread: Option<std::thread::JoinHandle<()>> = None;
    let mut usage_running = false;
    let mut restore_key = view.selected_key.clone();
    let mut select_first_while_loading = restore_key.is_empty();
    let mut refreshing_history = false;
    let mut refresh_pending = false;
    let mut refresh_to_first = false;
    let mut detail_generation: u64 = 0;
    let mut history_finished_at: Option<Instant> = None;
    let mut usage_finished_at: Option<Instant> = None;
    view.loaded = false;
    view.busy_detail = false;
    if view.pins.is_empty() {
        view.pins = load_pins();
    }
    let mut scanning = true;
    collector.reset();
    spawn_scan(collector, &tx);

    let guard = TerminalGuard::enter()?;
    let mut input = Input;
    let mut last_key = String::new();
    let mut chosen_since = Instant::now();
    let mut dirty = true;
    let mut last_shape = (0usize, 0usize);
    let mut last_render = Instant::now();
    let mut pending_input: Option<(Key, bool)> = None;
    let mut refresh_key_at: Option<Instant> = None;
    let mut last_screen: Option<Screen> = None;
    let mut last_click: (Option<Instant>, Option<Action>) = (None, None);
    let mut painted: Vec<String> = Vec::new();

    let save_pins = |view: &mut View| {
        let mut pins: Vec<&String> = view.pins.iter().collect();
        pins.sort();
        let value = Value::from(pins.into_iter().cloned().collect::<Vec<_>>());
        if let Err(error) = save_settings(IndexMap::from([("pins", value)])) {
            view.message = trf!("Could not save pins: {error}", error = line(&error.to_string(), 0));
        }
    };

    let outcome = 'outer: loop {
        while let Ok(message) = rx.try_recv() {
            let old = visible_entries(&sessions, view);
            let old_key = entry_key(&old, view.selected);
            match message {
                Msg::Batch(rows, errors) => {
                    if refreshing_history {
                        // Keep the visible list stable until all adapters finish.
                        continue;
                    }
                    collector.errors = errors;
                    sessions = rows.into_iter().map(|s| detail_cache.get(&s.key()).cloned().unwrap_or(s)).collect();
                    view.data_rev += 1;
                    apply_selection(view, &sessions, &mut restore_key, &old_key, select_first_while_loading, false);
                    view.loaded = false;
                }
                Msg::Done(rows, errors, sources) => {
                    collector.errors = errors;
                    collector.sources = sources;
                    sessions = rows.into_iter().map(|s| detail_cache.get(&s.key()).cloned().unwrap_or(s)).collect();
                    view.data_rev += 1;
                    apply_selection(view, &sessions, &mut restore_key, &old_key, select_first_while_loading, true);
                    view.loaded = true;
                    refreshing_history = false;
                    select_first_while_loading = false;
                    history_finished_at = Some(Instant::now());
                    scanning = false;
                }
                Msg::Detail(generation, session) => {
                    if generation != detail_generation {
                        continue;
                    }
                    let key = session.key();
                    if session.client != "agy" {
                        collector.cache.lock().unwrap().put(Path::new(&session.path), &session);
                    }
                    for s in sessions.iter_mut() {
                        if s.key() == key {
                            *s = (*session).clone();
                        }
                    }
                    view.data_rev += 1;
                    detail_cache.insert(key, *session);
                    view.busy_detail = false;
                }
                Msg::Error(error) => {
                    view.loaded = true;
                    view.message = error;
                    refreshing_history = false;
                    restore_key.clear();
                    select_first_while_loading = false;
                    history_finished_at = Some(Instant::now());
                    scanning = false;
                }
                Msg::DetailError(generation, error) => {
                    if generation != detail_generation {
                        continue;
                    }
                    view.busy_detail = false;
                    view.message = tr("Details: ") + &error;
                }
                Msg::Usage(value) => {
                    // Keep the last live report visible while fetching again.
                    if !usage.live || !view.live_usage {
                        usage = *value;
                    }
                }
                Msg::UsageLive(value) => {
                    usage = *value;
                    usage_refreshing = false;
                    if view.tab == "usage" {
                        view.message.clear();
                    }
                }
                Msg::UsageFailure(value) => {
                    usage_refreshing = false;
                    if view.tab == "usage" {
                        view.message = value.clone();
                    }
                    if !usage.errors.contains(&value) {
                        usage.errors.push(value);
                    }
                }
                Msg::UsageDone => {
                    usage_refreshing = false;
                    usage_running = false;
                    usage_finished_at = Some(Instant::now());
                }
            }
            view.usage = Some(usage.clone());
            view.usage_refreshing = usage_refreshing;
            dirty = true;
        }
        if let Some(notice) = crate::update::take_notice() {
            view.message = notice;
            dirty = true;
        }
        let filtered = visible_entries(&sessions, view);
        view.selected = view.selected.min(filtered.len() as i64 - 1).max(0);
        let entry = filtered.get(view.selected as usize).cloned();
        let current = match &entry {
            Some(Entry::Session(s)) => Some(s.clone()),
            _ => None,
        };
        let key_now = entry.as_ref().map(Entry::key).unwrap_or_default();
        if key_now != last_key {
            last_key = key_now;
            chosen_since = Instant::now();
            view.detail_scroll = 0;
            view.busy_detail = false;
        }
        let instant = Instant::now();
        let history_due = view.auto_refresh
            && (view.tab == "sessions" || view.tab == "projects")
            && history_finished_at.is_some_and(|t| instant - t >= HISTORY_REFRESH);
        if (refresh_pending || history_due) && !scanning && !view.busy_detail {
            select_first_while_loading = refresh_to_first;
            restore_key = if refresh_to_first { String::new() } else { entry.as_ref().map(Entry::key).unwrap_or_default() };
            view.selected_key = restore_key.clone();
            refresh_to_first = false;
            view.loaded = false;
            view.message.clear();
            refreshing_history = true;
            refresh_pending = false;
            history_finished_at = None;
            detail_generation += 1;
            clear_path_caches();
            detail_cache.clear();
            requested.clear();
            scanning = true;
            spawn_scan(collector, &tx);
            dirty = true;
        }
        if let Some(current) = &current
            && view.loaded
            && !requested.contains(&current.key())
            && chosen_since.elapsed() > Duration::from_millis(250)
        {
            requested.insert(current.key());
            view.busy_detail = true;
            let tx = tx.clone();
            let generation = detail_generation;
            let target = current.clone();
            std::thread::spawn(move || {
                let _ = tx.send(match detail(&target) {
                    Ok(s) => Msg::Detail(generation, Box::new(s)),
                    Err(e) => Msg::DetailError(generation, e),
                });
            });
            dirty = true;
        }
        let (w, h) = terminal::size().map(|(w, h)| (w as usize, h as usize)).unwrap_or((80, 24));
        if (w, h) != last_shape {
            last_shape = (w, h);
            dirty = true;
            painted.clear();
        }
        let usage_due = view.auto_refresh && usage_finished_at.is_some_and(|t| instant - t >= USAGE_REFRESH);
        // The strip loads the cached snapshot once, then refreshes live rarely.
        let strip_due = view.tab != "usage"
            && layout(w as i64, view).2 > 0
            && (!usage.loaded
                || (view.live_usage
                    && (view.auto_refresh || view.usage_live_at == 0.0)
                    && now() - view.usage_live_at >= STRIP_REFRESH_SECONDS));
        if strip_due && !usage_running {
            view.usage_live_at = now();
        }
        if ((view.tab == "usage" && (!usage_requested || usage_due)) || strip_due) && !usage_running {
            usage_requested = true;
            usage_refreshing = view.live_usage;
            usage_finished_at = None;
            usage_cancel = Cancel::default();
            usage_running = true;
            if let Some(handle) = usage_thread.take() {
                let _ = handle.join();
            }
            usage_thread = Some(spawn_usage(collector, &tx, usage_cancel.clone(), view.live_usage));
            dirty = true;
        }
        if dirty || last_render.elapsed() > Duration::from_secs(15) {
            view.usage_refreshing = usage_refreshing;
            let mut screen = render(&sessions, view, w, h, &collector.errors, Some(&usage));
            if view.tab == "usage" {
                view.usage_scroll = view.usage_scroll.min(max_usage_scroll(&usage, w, h));
                render_usage(&mut screen, &usage, view.usage_scroll, usage_refreshing, &view.message);
                if !view.modal.is_empty() {
                    let errors = collector.errors.clone();
                    draw_modal(&mut screen, view, &errors);
                }
            }
            paint(&screen, &mut painted)?;
            last_screen = Some(screen);
            dirty = false;
            last_render = Instant::now();
        }
        let (mut key, from_mouse) = match pending_input.take() {
            Some(found) => found,
            None => match input.read(Duration::from_millis(100)) {
                Some(found) => found,
                None => continue,
            },
        };
        if matches!(key, Key::CtrlC | Key::Char('q') | Key::Char('Q')) {
            break 'outer Outcome::Quit;
        }
        let mut steps: i64 = 1;
        if matches!(key, Key::Up | Key::Down) {
            // Coalesce queued wheel reports and arrow repeats into one frame,
            // keeping their count so fast scrolling never stalls or lags.
            let (direction, count, pending) = input.scroll_burst(key, from_mouse);
            key = direction;
            pending_input = pending;
            steps = count.min(MAX_BURST_STEPS);
        }
        dirty = true;
        if key == Key::Resize {
            continue;
        }
        let (w, h) = (w as i64, h as i64);
        if let Key::Click(cx, cy) = key {
            let action = last_screen.as_ref().and_then(|s| s.hit_at(cy, cx));
            let clicked_at = Instant::now();
            let double = action.is_some() && last_click.1 == action && last_click.0.is_some_and(|t| clicked_at - t <= DOUBLE_CLICK);
            last_click = (Some(clicked_at), action.clone());
            let mut next: Option<Key> = None;
            match action {
                None | Some(Action::Noop) => {}
                Some(Action::Close) => view.modal = "",
                Some(Action::New(i)) => {
                    view.new_client = i;
                    next = Some(Key::Enter);
                }
                Some(Action::Lang(i)) => {
                    view.language_index = i;
                    next = Some(Key::Enter);
                }
                Some(_) if !view.modal.is_empty() => {}
                Some(Action::Tab(name)) => {
                    if !(view.tab == "usage" && name == "usage") {
                        next = Some(Key::Char(match name {
                            "projects" => 'P',
                            "sessions" => 'S',
                            _ => 'U',
                        }));
                    }
                    if name == "sessions" && view.tab == "sessions" && view.project_path.is_none() {
                        next = None;
                    }
                }
                Some(Action::Key(c)) => next = Some(Key::Char(c)),
                Some(_) if view.tab == "usage" => {}
                Some(Action::Scope(name)) => {
                    view.tab = "sessions";
                    view.project_path = None;
                    view.scope = name;
                    view.selected = 0;
                    view.top = 0;
                    restore_key.clear();
                }
                Some(Action::Client(client)) => {
                    view.client = client;
                    view.selected = 0;
                    view.top = 0;
                    restore_key.clear();
                }
                Some(Action::Search) => view.searching = true,
                Some(Action::Row(index)) => {
                    restore_key.clear();
                    if double && view.selected == index as i64 {
                        next = Some(Key::Enter);
                        last_click = (None, None);
                    } else {
                        view.selected = index as i64;
                        keep_selection_visible(view, filtered.len() as i64, list_page_size(w, h));
                    }
                }
            }
            match next {
                Some(k) => key = k,
                None => continue,
            }
        }
        if !view.searching
            && let Key::Char(c @ ('j' | 'k' | 'g' | 'G')) = key
        {
            // Vim-style aliases share the arrow / Home / End code paths.
            key = match c {
                'j' => Key::Down,
                'k' => Key::Up,
                'g' => Key::Home,
                _ => Key::End,
            };
        }
        if key == Key::Esc {
            refresh_key_at = None;
            if !view.modal.is_empty() {
                view.modal = "";
            } else if view.tab == "usage" {
                close_usage(view);
            } else if view.project_path.is_some() {
                open_projects(view, false);
                restore_key.clear();
            } else if view.tab == "projects" {
                if view.folder_path.is_some() {
                    leave_folder(view);
                } else if !view.query.is_empty() || view.searching {
                    view.query.clear();
                    view.searching = false;
                    view.selected = 0;
                    view.top = 0;
                }
            } else {
                restore_key.clear();
                view.selected_key.clear();
                view.query.clear();
                view.searching = false;
                view.narrow_detail = false;
                view.scope = "cwd";
                view.client = "all".into();
                view.children = false;
                view.archived = false;
                view.selected = 0;
                view.top = 0;
                view.detail_scroll = 0;
                view.message.clear();
            }
            continue;
        }
        if key == Key::Backspace && !view.searching {
            // Backspace walks back one screen; text fields keep their
            // normal delete behavior in the search branch below.
            refresh_key_at = None;
            if !view.modal.is_empty() {
                view.modal = "";
            } else if view.tab == "usage" {
                close_usage(view);
            } else if view.project_path.is_some() {
                open_projects(view, false);
                restore_key.clear();
            } else if view.tab == "projects" && view.folder_path.is_some() {
                leave_folder(view);
            } else if view.tab == "sessions" {
                open_projects(view, false);
                restore_key.clear();
            }
            continue;
        }
        let plain = !view.searching && view.modal.is_empty();
        let is = |k: Key, chars: &str| matches!(k, Key::Char(c) if chars.contains(c));
        if is(key, "rR") && plain {
            let instant = Instant::now();
            if refresh_key_at.is_some_and(|t| instant - t <= RESTART_KEY) {
                break 'outer Outcome::Restart;
            }
            refresh_key_at = Some(instant);
        } else {
            refresh_key_at = None;
        }
        if view.modal == "language" {
            match key {
                Key::Up | Key::Down | Key::Left | Key::Right => {
                    view.language_index = (view.language_index + steps as usize) % LANGUAGES.len();
                }
                Key::Enter => {
                    let _ = set_language(LANGUAGES[view.language_index]);
                    view.modal = "";
                    view.message.clear();
                    if let Err(error) = save_language(language()) {
                        view.message = trf!("Could not save language: {error}", error = line(&error.to_string(), 0));
                    }
                    refresh_pending = true;
                    if !usage_running {
                        usage_requested = false;
                    }
                }
                _ => {}
            }
            continue;
        }
        if is(key, "lL") && plain {
            view.modal = "language";
            view.language_index = LANGUAGES.iter().position(|l| *l == language()).unwrap_or(0);
            continue;
        }
        if is(key, "tT") && plain {
            view.tree = !view.tree;
            view.folder_path = None;
            view.folder_stack.clear();
            view.selected = 0;
            view.top = 0;
            view.message = if view.tree { tr("Project tree: on") } else { tr("Project tree: off") };
            restore_key.clear();
            continue;
        }
        if is(key, "pP") && plain {
            if view.tab != "projects" || view.folder_path.is_some() {
                open_projects(view, true);
            }
            restore_key.clear();
            continue;
        }
        if view.tab == "usage" {
            let maximum = max_usage_scroll(&usage, w as usize, h as usize);
            match key {
                k if is(k, "uU") => close_usage(view),
                k if is(k, "sS") => {
                    view.tab = "sessions";
                    view.message.clear();
                }
                Key::Down | Key::PageDown | Key::Char(']') => {
                    view.usage_scroll = maximum.min(view.usage_scroll + if key == Key::PageDown { 8 } else { steps as usize });
                }
                Key::Up | Key::PageUp | Key::Char('[') => {
                    view.usage_scroll = view.usage_scroll.saturating_sub(if key == Key::PageUp { 8 } else { steps as usize });
                }
                Key::Home => view.usage_scroll = 0,
                Key::End => view.usage_scroll = maximum,
                k if is(k, "rR") => {
                    view.usage_scroll = 0;
                    if !usage_running {
                        usage_requested = false;
                        view.message.clear();
                    }
                }
                _ => {}
            }
            continue;
        }
        if !view.modal.is_empty() {
            if key == Key::Char('?') {
                view.modal = "";
            } else if view.modal == "help" {
                match key {
                    Key::Down => view.help_scroll += steps,
                    Key::PageDown => view.help_scroll += 8,
                    Key::Up => view.help_scroll = (view.help_scroll - steps).max(0),
                    Key::PageUp => view.help_scroll = (view.help_scroll - 8).max(0),
                    Key::Home => view.help_scroll = 0,
                    _ => {}
                }
            } else if view.modal == "new" {
                let mut key = key;
                if let Key::Char(c) = key
                    && let Some(d) = c.to_digit(10)
                    && (1..=CLIENTS.len() as u32).contains(&d)
                {
                    view.new_client = d as usize - 1;
                    key = Key::Enter;
                }
                match key {
                    Key::Down | Key::Right => view.new_client = (view.new_client + steps as usize) % CLIENTS.len(),
                    Key::Up | Key::Left => {
                        view.new_client = (view.new_client as i64 - steps).rem_euclid(CLIENTS.len() as i64) as usize;
                    }
                    Key::Enter => {
                        let client = CLIENTS[view.new_client];
                        if which(client).is_some() {
                            // Remembering the choice is a convenience only.
                            let _ = save_settings(IndexMap::from([("new_client", Value::from(client))]));
                            let cwd = if view.new_cwd.is_empty() { view.cwd.clone() } else { view.new_cwd.clone() };
                            break 'outer Outcome::New(client.into(), cwd);
                        }
                        view.message = trf!("{v0} is not installed", v0 = client);
                    }
                    _ => {}
                }
            }
            continue;
        }
        if from_mouse && view.narrow_detail {
            let limit = detail_scroll_limit(entry.as_ref(), view, w, h);
            let delta = if key == Key::Down { steps } else { -steps };
            view.detail_scroll = (view.detail_scroll + delta).min(limit).max(0);
            continue;
        }
        let page = list_page_size(w, h);
        match key {
            Key::Enter => {
                if view.searching {
                    view.searching = false;
                } else if let Some(Entry::Project(project)) = &entry {
                    if project.children.is_empty() {
                        enter_project(view, project);
                    } else {
                        enter_folder(view, project);
                    }
                    restore_key.clear();
                } else if let Some(current) = &current {
                    match validate_launch(current) {
                        Ok(_) => {
                            view.selected_key = current.key();
                            break 'outer Outcome::Resume(Box::new(current.clone()));
                        }
                        Err(error) => view.message = error,
                    }
                }
            }
            k if is(k, "oO") && !view.searching && matches!(entry, Some(Entry::Project(_))) => {
                if let Some(Entry::Project(project)) = &entry {
                    enter_project(view, project);
                }
                restore_key.clear();
            }
            Key::Tab => {
                restore_key.clear();
                view.project_path = None;
                view.tab = "sessions";
                view.scope = if view.scope == "cwd" { "all" } else { "cwd" };
                view.selected = 0;
                view.top = 0;
            }
            Key::Down | Key::Up | Key::PageDown | Key::PageUp | Key::Home | Key::End => {
                restore_key.clear();
                if !filtered.is_empty() {
                    select_first_while_loading = key == Key::Home && !view.loaded;
                }
                match key {
                    Key::Down => view.selected += steps,
                    Key::Up => view.selected -= steps,
                    Key::PageDown => view.selected += page,
                    Key::PageUp => view.selected -= page,
                    Key::Home => view.selected = 0,
                    _ => view.selected = filtered.len() as i64 - 1,
                }
                keep_selection_visible(view, filtered.len() as i64, page);
            }
            Key::Left | Key::Right if !view.searching => {
                restore_key.clear();
                let choices: Vec<&str> = std::iter::once("all").chain(CLIENTS).collect();
                let index = choices.iter().position(|c| *c == view.client).unwrap_or(0) as i64;
                let next = (index + if key == Key::Right { 1 } else { -1 }).rem_euclid(choices.len() as i64);
                view.client = choices[next as usize].into();
                view.selected = 0;
                view.top = 0;
            }
            Key::Backspace if view.searching => {
                view.query.pop();
                view.selected = 0;
                view.top = 0;
            }
            Key::CtrlU if view.searching => {
                view.query.clear();
                view.selected = 0;
                view.top = 0;
            }
            Key::CtrlW if view.searching => {
                let trimmed = view.query.trim_end().to_string();
                view.query = delete_word(&trimmed);
                view.selected = 0;
                view.top = 0;
            }
            Key::Char(c) if view.searching && !c.is_control() => {
                view.query.push(c);
                view.selected = 0;
                view.top = 0;
            }
            Key::Char('/' | '.') => view.searching = true,
            k if is(k, "uU") => {
                open_usage(view);
                if !usage_running {
                    usage_requested = false;
                }
            }
            k if is(k, "sS") => {
                view.tab = "sessions";
                view.project_path = None;
                view.selected = 0;
                view.top = 0;
            }
            Key::Char(c @ ('n' | 'N')) => {
                view.new_cwd = match &entry {
                    Some(Entry::Project(p)) if !p.cwd.is_empty() => p.cwd.clone(),
                    _ => match &view.project_path {
                        Some(path) if !path.is_empty() => path.clone(),
                        _ => match &current {
                            Some(s) if c == 'N' && !s.cwd.is_empty() => s.cwd.clone(),
                            _ => view.cwd.clone(),
                        },
                    },
                };
                let remembered = read_settings().get("new_client").and_then(Value::as_str).map(str::to_string);
                view.new_client = match &current {
                    Some(s) => CLIENTS.iter().position(|c| *c == s.client).unwrap_or(0),
                    None => remembered.and_then(|r| CLIENTS.iter().position(|c| *c == r)).unwrap_or(0),
                };
                view.modal = "new";
            }
            k if is(k, "dD") => view.narrow_detail = !view.narrow_detail,
            Key::Char(']') => {
                let limit = detail_scroll_limit(entry.as_ref(), view, w, h);
                view.detail_scroll = (view.detail_scroll.min(limit) + 5).min(limit);
            }
            Key::Char('[') => {
                let limit = detail_scroll_limit(entry.as_ref(), view, w, h);
                view.detail_scroll = (view.detail_scroll.min(limit) - 5).max(0);
            }
            k if is(k, "cC") => {
                view.children = !view.children;
                view.message = if view.children { tr("Child sessions: shown") } else { tr("Child sessions: hidden") };
                view.selected = 0;
                view.top = 0;
            }
            k if is(k, "aA") => {
                view.archived = !view.archived;
                view.message = if view.archived { tr("Archive: shown") } else { tr("Archive: hidden") };
                view.selected = 0;
                view.top = 0;
            }
            k if is(k, "rR") => {
                // Consume R even during a scan/detail read; it must never become
                // a search character because a background job is still busy.
                restore_key.clear();
                view.selected_key.clear();
                view.selected = 0;
                view.top = 0;
                view.detail_scroll = 0;
                select_first_while_loading = true;
                if !refreshing_history {
                    refresh_pending = true;
                    refresh_to_first = true;
                }
            }
            Key::Char('?') => {
                view.modal = "help";
                view.help_scroll = 0;
            }
            Key::Char(c @ '0'..='6') => {
                restore_key.clear();
                let choices: Vec<&str> = std::iter::once("all").chain(CLIENTS).collect();
                view.client = choices[c.to_digit(10).unwrap() as usize].into();
                view.selected = 0;
                view.top = 0;
            }
            k if is(k, "fF*") && entry.is_some() => {
                let entry = entry.as_ref().unwrap();
                let name = match entry {
                    Entry::Project(p) => {
                        let n = p.name();
                        if n.is_empty() { tr("No directory") } else { n }
                    }
                    Entry::Session(s) => {
                        if s.title.is_empty() {
                            s.id.clone()
                        } else {
                            s.title.clone()
                        }
                    }
                };
                let key = entry.key();
                if view.pins.remove(&key) {
                    view.message = trf!("Unpinned: {name}", name = line(&name, 60));
                } else {
                    view.pins.insert(key.clone());
                    view.message = trf!("Pinned: {name}", name = line(&name, 60));
                }
                save_pins(view);
                let fresh = visible_entries(&sessions, view);
                if let Some(found) = fresh.iter().position(|e| e.key() == key) {
                    view.selected = found as i64;
                }
                keep_selection_visible(view, fresh.len() as i64, page);
            }
            k if is(k, "yY") && entry.is_some() => {
                let text = copy_payload(entry.as_ref().unwrap(), key == Key::Char('Y'));
                // The clipboard helper may write OSC 52 to the terminal.
                let method = copy_text(&text);
                painted.clear();
                view.message = if method.is_empty() {
                    tr("Clipboard unavailable: install pbcopy, wl-copy or xclip")
                } else {
                    trf!("Copied: {text}", text = line(&text, 90))
                };
            }
            k if is(k, "eE") && entry.is_some() => {
                let folder = entry.as_ref().unwrap().cwd().to_string();
                view.message = match reveal(&folder) {
                    Ok(_) => trf!("Opened folder: {path}", path = pretty_path(&folder, Some(&view.home))),
                    Err(_) => trf!("Cannot open folder: {path}", path = pretty_path(&folder, Some(&view.home))),
                };
            }
            _ => {}
        }
    };
    drop(guard);
    usage_cancel.set();
    if let Some(handle) = usage_thread.take() {
        let _ = handle.join();
    }
    collector.cache.lock().unwrap().save();
    view.usage = Some(usage);
    Ok(outcome)
}

/// Ctrl+W: drop the last word and its trailing spaces.
fn delete_word(text: &str) -> String {
    let trimmed = text.trim_end();
    let cut = trimmed.rfind(char::is_whitespace).map(|i| i + trimmed[i..].chars().next().map_or(1, char::len_utf8)).unwrap_or(0);
    trimmed[..cut].to_string()
}

fn apply_selection(view: &mut View, sessions: &[Session], restore_key: &mut String, old_key: &str, first: bool, done: bool) {
    let wanted = if restore_key.is_empty() { old_key.to_string() } else { restore_key.clone() };
    if first {
        // Startup and manual refresh follow the first sorted row.
        view.selected = 0;
        view.top = 0;
    } else if !wanted.is_empty() {
        let fresh = visible_entries(sessions, view);
        if let Some(found) = fresh.iter().position(|s| s.key() == wanted) {
            view.selected = found as i64;
            restore_key.clear();
        } else if done {
            restore_key.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Session;

    fn demo(cwd: &str) -> Vec<Session> {
        crate::cli::demo_sessions(cwd)
    }

    #[test]
    fn render_all_terminal_sizes_and_both_tabs() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = crate::model::realpath(dir.path()).to_string_lossy().into_owned();
        let sessions = demo(&cwd);
        for (cols, rows) in [(44, 16), (80, 24), (120, 40), (160, 50), (30, 10)] {
            for tab in ["sessions", "projects"] {
                let mut view = View::new(&cwd, dir.path());
                view.tab = tab;
                view.loaded = true;
                let screen = render(&sessions, &mut view, cols, rows, &[], Some(&crate::usage::demo_usage()));
                let text = screen.plain();
                if cols >= 44 && rows >= 16 {
                    assert!(text.contains("AI SESSIONS"), "{cols}x{rows}");
                }
            }
        }
    }

    #[test]
    fn rows_tabs_and_clients_are_clickable() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = crate::model::realpath(dir.path()).to_string_lossy().into_owned();
        let sessions = demo(&cwd);
        let mut view = View::new(&cwd, dir.path());
        view.loaded = true;
        let screen = render(&sessions, &mut view, 140, 44, &[], None);
        assert!(screen.hits.iter().any(|h| h.4 == Action::Row(0)));
        assert!(screen.hits.iter().any(|h| h.4 == Action::Tab("projects")));
        assert!(screen.hits.iter().any(|h| h.4 == Action::Client("claude".into())));
        view.modal = "new";
        let screen = render(&sessions, &mut view, 140, 44, &[], None);
        assert_eq!(screen.hit_at(0, 0), Some(Action::Close));
        assert!(screen.hits.iter().any(|h| h.4 == Action::New(5)));
    }

    #[test]
    fn help_fits_in_both_layouts() {
        for columns in [1, 2] {
            let rows = help_lines(columns);
            assert!(rows.iter().flatten().any(|(t, _)| t.contains("Shift+N")));
        }
    }

    #[test]
    fn selection_moves_window_only_at_visible_edge() {
        let dir = tempfile::tempdir().unwrap();
        let mut view = View::new("/", dir.path());
        view.selected = 5;
        keep_selection_visible(&mut view, 100, 4);
        assert_eq!((view.selected, view.top), (5, 2));
        view.selected = 4;
        keep_selection_visible(&mut view, 100, 4);
        assert_eq!(view.top, 2);
        view.selected = 1;
        keep_selection_visible(&mut view, 100, 4);
        assert_eq!(view.top, 1);
        view.selected = 1000;
        keep_selection_visible(&mut view, 0, 4);
        assert_eq!((view.selected, view.top), (0, 0));
    }

    #[test]
    fn copy_payload_quotes_directory() {
        let s = Session { cwd: "/a b".into(), ..Session::new("claude", "id1") };
        assert_eq!(copy_payload(&Entry::Session(s.clone()), false), "cd '/a b' && claude --resume id1");
        assert_eq!(copy_payload(&Entry::Session(s), true), "id1");
    }
}
