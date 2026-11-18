//! Sessions, projects, search queries and launch commands.

use crate::i18n::{home, tr};
use crate::text::{casefold, shell_join};
use crate::trf;
use regex::Regex;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

pub const CLIENTS: [&str; 6] = ["omp", "claude", "codex", "agy", "gemini", "pi"];

pub fn label(client: &str) -> &'static str {
    match client {
        "omp" => "OMP",
        "claude" => "Claude",
        "codex" => "Codex",
        "agy" => "agy",
        "gemini" => "Gemini",
        "pi" => "Pi",
        _ => "?",
    }
}

/// English status label (translated with `tr`).
pub fn status_label(status: &str) -> Option<&'static str> {
    Some(match status {
        "saved" => "Saved",
        "ready" => "Response ready",
        "closed" => "Closed",
        "interrupted" => "Interrupted",
        "error" => "Error",
        "awaiting" => "Question",
        "tool" => "Tool",
        "thinking" => "Thinking",
        "user" => "Prompt",
        _ => return None,
    })
}

pub fn status_text(status: &str) -> String {
    tr(status_label(status).unwrap_or(status))
}

pub fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

pub fn system_time(time: SystemTime) -> f64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs_f64(),
        Err(e) => -e.duration().as_secs_f64(),
    }
}

/// Seconds since the epoch from epoch numbers (seconds or ms) or ISO strings.
pub fn stamp(value: &Value, default: f64) -> f64 {
    match value {
        Value::Number(n) => {
            let v = n.as_f64().unwrap_or(default);
            if v > 1e11 { v / 1000.0 } else { v }
        }
        Value::Bool(b) => f64::from(u8::from(*b)),
        Value::String(text) => parse_iso(text).unwrap_or(default),
        _ => default,
    }
}

pub fn parse_iso(text: &str) -> Option<f64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(ts) = text.parse::<jiff::Timestamp>() {
        return Some(ts.as_nanosecond() as f64 / 1e9);
    }
    let local = jiff::tz::TimeZone::system();
    if let Ok(dt) = text.parse::<jiff::civil::DateTime>() {
        return dt.to_zoned(local).ok().map(|z| z.timestamp().as_nanosecond() as f64 / 1e9);
    }
    if let Ok(date) = text.parse::<jiff::civil::Date>() {
        return date.to_zoned(local).ok().map(|z| z.timestamp().as_nanosecond() as f64 / 1e9);
    }
    None
}

pub fn local(value: f64) -> Option<jiff::Zoned> {
    if !value.is_finite() || value.abs() > 253_402_300_800.0 {
        return None;
    }
    let ts = jiff::Timestamp::from_nanosecond((value * 1e9) as i128).ok()?;
    Some(ts.to_zoned(jiff::tz::TimeZone::system()))
}

pub fn strftime(value: f64, format: &str) -> Option<String> {
    local(value).map(|z| z.strftime(format).to_string())
}

pub fn path_from_uri(value: &str) -> String {
    if let Some(rest) = value.strip_prefix("file://") {
        // urlparse(...).path: drop the authority, keep the path, percent-decode.
        let path = match rest.find('/') {
            Some(index) => &rest[index..],
            None => "",
        };
        let path = path.split(['?', '#']).next().unwrap_or("");
        return percent_decode(path);
    }
    value.to_string()
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 3 <= bytes.len() {
            let hex = |b: u8| (b as char).to_digit(16);
            if let (Some(high), Some(low)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((high * 16 + low) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn expand_user(value: &str) -> PathBuf {
    if value == "~" {
        return home();
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return home().join(rest);
    }
    PathBuf::from(value)
}

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `os.path.realpath`: resolve what exists, keep the rest lexically.
pub fn realpath(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(path) };
    let absolute = normalize(&absolute);
    if let Ok(found) = std::fs::canonicalize(&absolute) {
        return strip_verbatim(found);
    }
    let mut existing = absolute.clone();
    let mut tail = Vec::new();
    while let Some(name) = existing.file_name().map(|n| n.to_os_string()) {
        tail.push(name);
        existing.pop();
        if let Ok(found) = std::fs::canonicalize(&existing) {
            let mut result = strip_verbatim(found);
            for part in tail.iter().rev() {
                result.push(part);
            }
            return result;
        }
    }
    absolute
}

fn strip_verbatim(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let text = path.to_string_lossy();
        if let Some(rest) = text.strip_prefix(r"\\?\")
            && !rest.starts_with("UNC")
        {
            return PathBuf::from(rest);
        }
    }
    path
}

static CANONICAL: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
static SAME: OnceLock<Mutex<HashMap<(String, String), bool>>> = OnceLock::new();

pub fn canonical(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    let cache = CANONICAL.get_or_init(Default::default);
    if let Some(found) = cache.lock().unwrap().get(value) {
        return found.clone();
    }
    let result = realpath(&expand_user(&path_from_uri(value))).to_string_lossy().into_owned();
    let mut guard = cache.lock().unwrap();
    if guard.len() > 8192 {
        guard.clear();
    }
    guard.insert(value.to_string(), result.clone());
    result
}

#[cfg(unix)]
fn file_identity(path: &str) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

#[cfg(not(unix))]
fn file_identity(_path: &str) -> Option<(u64, u64)> {
    None
}

pub fn same_cwd(left: &str, right: &str) -> bool {
    if left.is_empty() || right.is_empty() {
        return false;
    }
    let key = (left.to_string(), right.to_string());
    let cache = SAME.get_or_init(Default::default);
    if let Some(found) = cache.lock().unwrap().get(&key) {
        return *found;
    }
    let result = match (file_identity(left), file_identity(right)) {
        (Some(a), Some(b)) => a == b,
        _ => canonical(left) == canonical(right),
    };
    let mut guard = cache.lock().unwrap();
    if guard.len() > 8192 {
        guard.clear();
    }
    guard.insert(key, result);
    result
}

pub fn clear_path_caches() {
    if let Some(c) = CANONICAL.get() {
        c.lock().unwrap().clear();
    }
    if let Some(c) = SAME.get() {
        c.lock().unwrap().clear();
    }
}

pub fn file_name(path: &str) -> String {
    Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

pub fn pretty_path(value: &str, home_dir: Option<&Path>) -> String {
    if value.is_empty() {
        return tr("Directory not recorded");
    }
    let home_str = home_dir.map(Path::to_path_buf).unwrap_or_else(home).to_string_lossy().into_owned();
    let value = canonical(value);
    let sep = std::path::MAIN_SEPARATOR;
    if value == home_str || value.starts_with(&format!("{home_str}{sep}")) { format!("~{}", &value[home_str.len()..]) } else { value }
}

pub fn age(value: f64, current: Option<f64>) -> String {
    if value == 0.0 || !value.is_finite() {
        return "—".into();
    }
    let Some(recorded) = local(value) else { return "—".into() };
    let seconds = (current.unwrap_or_else(now) - value).max(0.0) as i64;
    if seconds < 60 {
        return tr("now");
    }
    if seconds < 3600 {
        return trf!("{v0} min", v0 = seconds / 60);
    }
    if seconds < 86400 {
        return trf!("{v0} h", v0 = seconds / 3600);
    }
    if seconds < 86400 * 30 {
        return trf!("{v0} d", v0 = seconds / 86400);
    }
    recorded.strftime("%d.%m.%y").to_string()
}

pub fn number(value: Option<i64>) -> String {
    match value {
        None => "—".into(),
        Some(v) if v >= 1_000_000 => format!("{:.1}M", v as f64 / 1_000_000.0),
        Some(v) if v >= 1000 => format!("{:.1}k", v as f64 / 1000.0),
        Some(v) => v.to_string(),
    }
}

fn lenient_int<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<i64>, D::Error> {
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(value.and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64))))
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Todo {
    pub text: String,
    pub status: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub client: String,
    pub id: String,
    pub cwd: String,
    pub title: String,
    pub path: String,
    pub updated: f64,
    pub created: f64,
    pub model: String,
    pub provider: String,
    pub status: String,
    pub status_detail: String,
    pub last_action: String,
    pub last_user: String,
    pub last_assistant: String,
    pub messages: i64,
    pub tools: i64,
    #[serde(deserialize_with = "lenient_int")]
    pub tokens: Option<i64>,
    pub cost: Option<f64>,
    pub branch: String,
    pub profile: String,
    pub source: String,
    pub project_id: String,
    #[serde(deserialize_with = "lenient_int")]
    pub steps: Option<i64>,
    pub parent: String,
    pub todo: Vec<Todo>,
    pub exact: bool,
    pub archived: bool,
    pub state_root: String,
    pub workspaces: Vec<String>,
    pub resume_supported: bool,
    pub note: String,
    /// Source file signature used by the cache (never serialized).
    #[serde(skip)]
    pub source_sig: Option<[u64; 3]>,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            client: String::new(),
            id: String::new(),
            cwd: String::new(),
            title: String::new(),
            path: String::new(),
            updated: 0.0,
            created: 0.0,
            model: String::new(),
            provider: String::new(),
            status: "saved".into(),
            status_detail: String::new(),
            last_action: String::new(),
            last_user: String::new(),
            last_assistant: String::new(),
            messages: 0,
            tools: 0,
            tokens: None,
            cost: None,
            branch: String::new(),
            profile: String::new(),
            source: String::new(),
            project_id: String::new(),
            steps: None,
            parent: String::new(),
            todo: Vec::new(),
            exact: false,
            archived: false,
            state_root: String::new(),
            workspaces: Vec::new(),
            resume_supported: true,
            note: String::new(),
            source_sig: None,
        }
    }
}

impl Session {
    pub fn new(client: &str, id: &str) -> Self {
        Self { client: client.into(), id: id.into(), ..Default::default() }
    }

    pub fn key(&self) -> String {
        format!("{}:{}:{}:{}", self.client, self.state_root, self.profile, self.id)
    }

    pub fn search_text(&self) -> String {
        casefold(
            &[
                self.title.as_str(),
                &self.cwd,
                &self.client,
                &self.model,
                &self.id,
                &self.last_user,
                &self.last_assistant,
                &self.last_action,
                &self.branch,
                &self.profile,
                &self.status,
                &status_label(&self.status).map(tr).unwrap_or_default(),
            ]
            .join(" "),
        )
    }

    pub fn in_cwd(&self, cwd: &str) -> bool {
        std::iter::once(&self.cwd).chain(self.workspaces.iter()).any(|p| same_cwd(p, cwd))
    }

    pub fn argv(&self) -> Result<Vec<String>, String> {
        if !self.resume_supported {
            return Err(if self.note.is_empty() { tr("This client does not support resuming by ID") } else { self.note.clone() });
        }
        let target = if self.path.is_empty() { self.id.clone() } else { self.path.clone() };
        let mut argv: Vec<String> = Vec::new();
        match self.client.as_str() {
            "omp" => {
                argv.push("omp".into());
                if !self.profile.is_empty() {
                    argv.extend(["--profile".into(), self.profile.clone()]);
                }
                argv.extend(["--resume".into(), target]);
            }
            "pi" => argv.extend(["pi".into(), "--session".into(), target]),
            "claude" => argv.extend(["claude".into(), "--resume".into(), self.id.clone()]),
            "codex" => {
                argv.extend(["codex".into(), "resume".into(), self.id.clone()]);
                if !self.cwd.is_empty() {
                    argv.extend(["--cd".into(), self.cwd.clone()]);
                }
            }
            "agy" => {
                argv.push("agy".into());
                if !self.project_id.is_empty() {
                    argv.extend(["--project".into(), self.project_id.clone()]);
                }
                argv.extend(["--conversation".into(), self.id.clone()]);
            }
            "gemini" => argv.extend(["gemini".into(), "--resume".into(), self.id.clone()]),
            other => return Err(trf!("No launch adapter for {v0}", v0 = other)),
        }
        Ok(argv)
    }

    pub fn command(&self) -> String {
        match self.argv() {
            Ok(argv) => shell_join(&argv),
            Err(_) => tr("Resume unavailable"),
        }
    }

    /// Environment changes for the client: (set, remove).
    pub fn environment(&self) -> (Vec<(String, String)>, Vec<String>) {
        let mut set = Vec::new();
        let mut remove = Vec::new();
        if self.client == "omp" {
            if self.profile.is_empty() {
                remove.push("OMP_PROFILE".to_string());
            } else {
                set.push(("OMP_PROFILE".to_string(), self.profile.clone()));
            }
        }
        if !self.state_root.is_empty() {
            let key = match self.client.as_str() {
                "codex" => Some("CODEX_HOME"),
                "claude" => Some("CLAUDE_CONFIG_DIR"),
                "omp" | "pi" => Some("PI_CODING_AGENT_DIR"),
                _ => None,
            };
            if let Some(key) = key {
                if self.client == "claude"
                    && Path::new(&self.state_root) == home().join(".claude")
                    && std::env::var_os(key).is_none_or(|v| v.is_empty())
                {
                    // The default store must stay default: Claude keys its keychain
                    // login by config dir, so an explicit path is another login.
                    remove.push(key.to_string());
                } else {
                    set.push((key.to_string(), self.state_root.clone()));
                }
            }
        }
        (set, remove)
    }
}

// ---------------------------------------------------------------- queries

fn field_key(name: &str) -> Option<&'static str> {
    Some(match name {
        "client" | "c" | "cli" => "client",
        "model" | "m" => "model",
        "status" | "st" => "status",
        "branch" | "b" => "branch",
        "dir" | "in" | "cwd" => "dir",
        "profile" => "profile",
        "since" | "age" => "since",
        "is" => "is",
        _ => return None,
    })
}

/// '90m', '6h', '3d', '2w', 'today', 'yesterday' -> seconds back from now.
pub fn duration(value: &str) -> Option<f64> {
    let value = casefold(value.trim());
    let midnight = || {
        let zoned = jiff::Zoned::now();
        let start = zoned.start_of_day().ok()?;
        Some(start.timestamp().as_second() as f64)
    };
    if value == "today" || value == "сегодня" {
        return Some((now() - midnight()?).max(1.0));
    }
    if value == "yesterday" || value == "вчера" {
        return Some(now() - midnight()? + 86400.0);
    }
    static DURATION: OnceLock<Regex> = OnceLock::new();
    let re = DURATION.get_or_init(|| Regex::new(r"^(\d+(?:\.\d+)?)\s*(m|min|h|d|w|mo|y)?$").unwrap());
    let caps = re.captures(&value)?;
    let amount: f64 = caps[1].parse().ok()?;
    let unit = match caps.get(2).map(|m| m.as_str()).unwrap_or("d") {
        "m" | "min" => 60.0,
        "h" => 3600.0,
        "d" => 86400.0,
        "w" => 604800.0,
        "mo" => 2_592_000.0,
        _ => 31_536_000.0,
    };
    Some(amount * unit)
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Query {
    pub words: Vec<String>,
    pub excluded: Vec<String>,
    pub fields: Vec<(String, String, bool)>,
}

impl Query {
    pub fn highlights(&self) -> Vec<String> {
        self.words.iter().filter(|w| !w.is_empty()).cloned().collect()
    }
}

/// Plain words, -excluded words and field:value filters (see ? help).
pub fn parse_query(query: &str) -> Query {
    static TOKEN: OnceLock<Regex> = OnceLock::new();
    let re = TOKEN.get_or_init(|| Regex::new(r#"(-?)(?:([A-Za-z]+):)?("[^"]*"?|\S+)"#).unwrap());
    let mut parsed = Query::default();
    for caps in re.captures_iter(query) {
        let negative = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let mut name = caps.get(2).map(|m| m.as_str()).unwrap_or("").to_string();
        let mut value = casefold(caps[3].trim_matches('"'));
        let mut key = if name.is_empty() { None } else { field_key(&casefold(&name)) };
        if !name.is_empty() && key.is_none() {
            // An unknown prefix (for example http:) is ordinary text.
            value = format!("{}:{value}", casefold(&name));
            name.clear();
            key = None;
        }
        if value.is_empty() {
            continue;
        }
        if let Some(key) = key {
            parsed.fields.push((key.to_string(), value, !negative.is_empty()));
        } else if !negative.is_empty() && value != "-" {
            parsed.excluded.push(value);
        } else if value == "-" {
            parsed.words.push(format!("{negative}{value}"));
        } else {
            parsed.words.push(value);
        }
    }
    parsed
}

pub fn field_match(s: &Session, key: &str, value: &str, pins: &HashSet<String>) -> bool {
    match key {
        "client" => s.client.starts_with(value) || casefold(label(&s.client)).starts_with(value),
        "model" => casefold(&format!("{} {}", s.model, s.provider)).contains(value),
        "status" => {
            let english = status_label(&s.status).unwrap_or(&s.status);
            s.status.starts_with(value)
                || casefold(&tr(english)).contains(value)
                || casefold(status_label(&s.status).unwrap_or("")).contains(value)
        }
        "branch" => casefold(&s.branch).contains(value),
        "dir" => std::iter::once(&s.cwd).chain(s.workspaces.iter()).filter(|p| !p.is_empty()).any(|p| casefold(p).contains(value)),
        "profile" => casefold(&s.profile).contains(value),
        "since" => match duration(value) {
            None => true,
            Some(seconds) => s.updated >= now() - seconds,
        },
        "is" => match value {
            "pinned" | "pin" | "fav" => pins.contains(&s.key()),
            "child" => !s.parent.is_empty(),
            "archived" | "archive" => s.archived,
            "exact" => s.exact,
            _ => true,
        },
        _ => true,
    }
}

pub fn query_match(s: &Session, query: &Query, pins: &HashSet<String>) -> bool {
    if !query.words.is_empty() || !query.excluded.is_empty() {
        let text = s.search_text();
        if !query.words.iter().all(|w| text.contains(w.as_str())) || query.excluded.iter().any(|w| text.contains(w.as_str())) {
            return false;
        }
    }
    query.fields.iter().all(|(key, value, negative)| field_match(s, key, value, pins) != *negative)
}

pub fn sort_recent(items: &mut [Session]) {
    items.sort_by(|a, b| b.updated.partial_cmp(&a.updated).unwrap_or(std::cmp::Ordering::Equal));
}

#[derive(Clone, Debug)]
pub struct Filter<'a> {
    pub cwd: &'a str,
    pub scope: &'a str,
    pub client: &'a str,
    pub query: &'a str,
    pub children: bool,
    pub archived: bool,
    pub pins: &'a HashSet<String>,
}

pub fn empty_pins() -> &'static HashSet<String> {
    static EMPTY: OnceLock<HashSet<String>> = OnceLock::new();
    EMPTY.get_or_init(HashSet::new)
}

pub fn filter_sessions(sessions: &[Session], f: &Filter) -> Vec<Session> {
    let parsed = parse_query(f.query);
    let explicit: Vec<&str> = parsed.fields.iter().filter(|(k, _, neg)| k == "is" && !neg).map(|(_, v, _)| v.as_str()).collect();
    // An explicit is:child / is:archived filter implies showing those rows.
    let children = f.children || explicit.contains(&"child");
    let archived = f.archived || explicit.iter().any(|v| *v == "archived" || *v == "archive");
    let mut result: Vec<Session> = sessions
        .iter()
        .filter(|s| {
            (f.scope == "all" || s.in_cwd(f.cwd))
                && (f.client == "all" || s.client == f.client)
                && (children || s.parent.is_empty())
                && (archived || !s.archived)
                && query_match(s, &parsed, f.pins)
        })
        .cloned()
        .collect();
    sort_recent(&mut result);
    result
}

// ---------------------------------------------------------------- projects

#[derive(Clone, Debug, Default)]
pub struct Project {
    pub cwd: String,
    pub sessions: Vec<Session>,
    pub depth: usize,
    pub prefix: String,
    pub children: Vec<Project>,
}

impl Project {
    pub fn key(&self) -> String {
        format!("project:{}", self.cwd)
    }

    pub fn updated(&self) -> f64 {
        self.sessions.iter().map(|s| s.updated).fold(0.0, f64::max)
    }

    /// Latest activity in this folder or any nested project folder.
    pub fn subtree_updated(&self) -> f64 {
        self.children.iter().map(Project::subtree_updated).fold(self.updated(), f64::max)
    }

    pub fn name(&self) -> String {
        file_name(&self.cwd)
    }
}

fn order_key(p: &Project, subtree: bool) -> (f64, String, String) {
    (-(if subtree { p.subtree_updated() } else { p.updated() }), casefold(&p.name()), p.cwd.clone())
}

fn sort_projects(items: &mut [Project], subtree: bool) {
    let mut keyed: Vec<((f64, String, String), usize)> = items.iter().enumerate().map(|(i, p)| (order_key(p, subtree), i)).collect();
    keyed.sort_by(|a, b| {
        a.0.0.partial_cmp(&b.0.0).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.0.1.cmp(&b.0.1)).then_with(|| a.0.2.cmp(&b.0.2))
    });
    let order: Vec<usize> = keyed.into_iter().map(|(_, i)| i).collect();
    let snapshot: Vec<Project> = items.to_vec();
    for (slot, index) in order.into_iter().enumerate() {
        items[slot] = snapshot[index].clone();
    }
}

/// Group sessions by folder; returns a nested forest (roots) and flat groups.
pub struct Tree {
    pub roots: Vec<Project>,
    pub flat: Vec<Project>,
}

fn parent_path(path: &str) -> Option<String> {
    Path::new(path).parent().map(|p| p.to_string_lossy().into_owned())
}

pub fn project_tree(sessions: &[Session], client: &str, query: &str, children: bool, archived: bool, pins: &HashSet<String>) -> Tree {
    let filter = Filter { cwd: "", scope: "all", client, query, children, archived, pins };
    let mut groups: indexmap::IndexMap<String, Vec<Session>> = indexmap::IndexMap::new();
    for session in filter_sessions(sessions, &filter) {
        // Unknown directories remain visible, without pretending they are cwd.
        let mut paths: indexmap::IndexSet<String> = indexmap::IndexSet::new();
        for p in std::iter::once(&session.cwd).chain(session.workspaces.iter()) {
            if !p.is_empty() {
                paths.insert(canonical(p));
            }
        }
        if paths.is_empty() {
            paths.insert(String::new());
        }
        for path in paths {
            groups.entry(path).or_default().push(session.clone());
        }
    }
    let flat: Vec<Project> = groups.into_iter().map(|(cwd, sessions)| Project { cwd, sessions, ..Default::default() }).collect();
    let known: HashSet<&str> = flat.iter().map(|p| p.cwd.as_str()).collect();
    // Nearest ancestor folder that is itself a project.
    let parent_of = |path: &str| -> Option<String> {
        if path.is_empty() {
            return None;
        }
        let mut current = path.to_string();
        loop {
            let parent = parent_path(&current)?;
            if parent == current || parent.is_empty() {
                return None;
            }
            if known.contains(parent.as_str()) {
                return Some(parent);
            }
            current = parent;
        }
    };
    let parents: Vec<Option<String>> = flat.iter().map(|p| parent_of(&p.cwd)).collect();
    // Build children bottom-up: deepest paths first.
    let mut nodes: HashMap<String, Project> = flat.iter().map(|p| (p.cwd.clone(), p.clone())).collect();
    let mut order: Vec<usize> = (0..flat.len()).collect();
    order.sort_by_key(|i| std::cmp::Reverse(Path::new(&flat[*i].cwd).components().count()));
    let mut attached: HashMap<String, Vec<String>> = HashMap::new();
    for i in 0..flat.len() {
        if let Some(parent) = &parents[i] {
            attached.entry(parent.clone()).or_default().push(flat[i].cwd.clone());
        }
    }
    for i in order {
        let cwd = &flat[i].cwd;
        if let Some(kids) = attached.get(cwd) {
            let mut children: Vec<Project> = kids.iter().filter_map(|k| nodes.remove(k)).collect();
            sort_projects(&mut children, true);
            if let Some(node) = nodes.get_mut(cwd) {
                node.children = children;
            }
        }
    }
    let mut roots: Vec<Project> =
        flat.iter().zip(parents.iter()).filter(|(_, parent)| parent.is_none()).filter_map(|(p, _)| nodes.remove(&p.cwd)).collect();
    sort_projects(&mut roots, true);
    // Flat entries carry their children too (used for counts and folder views).
    let mut flat_full = Vec::new();
    fn collect(items: &[Project], out: &mut Vec<Project>) {
        for p in items {
            out.push(p.clone());
            collect(&p.children, out);
        }
    }
    collect(&roots, &mut flat_full);
    Tree { roots, flat: flat_full }
}

pub struct Grouping<'a> {
    pub client: &'a str,
    pub query: &'a str,
    pub children: bool,
    pub archived: bool,
    pub pins: &'a HashSet<String>,
}

impl Grouping<'_> {
    pub fn all() -> Grouping<'static> {
        Grouping { client: "all", query: "", children: false, archived: false, pins: empty_pins() }
    }
}

pub fn group_projects(sessions: &[Session], g: &Grouping, tree: bool) -> Vec<Project> {
    let built = project_tree(sessions, g.client, g.query, g.children, g.archived, g.pins);
    if tree {
        let mut flat = Vec::new();
        fn flatten(items: &[Project], depth: usize, prefix: &str, out: &mut Vec<Project>) {
            for (index, project) in items.iter().enumerate() {
                let last = index == items.len() - 1;
                let mut item = project.clone();
                item.depth = depth;
                item.prefix = format!(
                    "{prefix}{}",
                    if depth == 0 {
                        ""
                    } else if last {
                        "└─ "
                    } else {
                        "├─ "
                    }
                );
                out.push(item);
                let next = format!(
                    "{prefix}{}",
                    if depth == 0 {
                        ""
                    } else if last {
                        "   "
                    } else {
                        "│  "
                    }
                );
                flatten(&project.children, depth + 1, &next, out);
            }
        }
        flatten(&built.roots, 0, "", &mut flat);
        return flat;
    }
    let mut projects = built.flat;
    sort_projects(&mut projects, false);
    for p in &mut projects {
        p.depth = 0;
        p.prefix.clear();
    }
    projects
}

#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum Entry {
    Session(Session),
    Project(Project),
}

impl Entry {
    pub fn key(&self) -> String {
        match self {
            Entry::Session(s) => s.key(),
            Entry::Project(p) => p.key(),
        }
    }

    pub fn cwd(&self) -> &str {
        match self {
            Entry::Session(s) => &s.cwd,
            Entry::Project(p) => &p.cwd,
        }
    }
}

/// Direct subfolders with sessions, then the folder's own sessions.
pub fn folder_entries(sessions: &[Session], path: &str, g: &Grouping) -> Vec<Entry> {
    let built = project_tree(sessions, g.client, g.query, g.children, g.archived, g.pins);
    let Some(project) = built.flat.into_iter().find(|p| p.cwd == path) else { return Vec::new() };
    let mut folders = project.children.clone();
    sort_projects(&mut folders, true);
    let mut own = project.sessions.clone();
    sort_recent(&mut own);
    folders
        .into_iter()
        .map(|mut f| {
            f.depth = 0;
            f.prefix.clear();
            Entry::Project(f)
        })
        .chain(own.into_iter().map(Entry::Session))
        .collect()
}

pub fn which(binary: &str) -> Option<PathBuf> {
    let candidate = Path::new(binary);
    if candidate.components().count() > 1 {
        return candidate.is_file().then(|| candidate.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    #[cfg(windows)]
    let extensions: Vec<String> =
        std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into()).split(';').map(|s| s.to_lowercase()).collect();
    for dir in std::env::split_paths(&path) {
        let full = dir.join(binary);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&full)
                && meta.is_file()
                && meta.permissions().mode() & 0o111 != 0
            {
                return Some(full);
            }
        }
        #[cfg(windows)]
        {
            if full.is_file() {
                return Some(full);
            }
            for ext in &extensions {
                let with = dir.join(format!("{binary}{ext}"));
                if with.is_file() {
                    return Some(with);
                }
            }
        }
    }
    None
}

pub fn validate_launch(session: &Session) -> Result<Vec<String>, String> {
    let mut argv = session.argv()?;
    if session.cwd.is_empty() {
        return Err(tr("Session directory unknown; resume disabled"));
    }
    if !Path::new(&session.cwd).is_dir() {
        return Err(trf!("Directory no longer exists: {v0}", v0 = &session.cwd));
    }
    let binary = which(&argv[0]).ok_or_else(|| trf!("Client {v0} not found in PATH", v0 = &argv[0]))?;
    argv[0] = binary.to_string_lossy().into_owned();
    Ok(argv)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(client: &str, id: &str, cwd: &str, updated: f64) -> Session {
        Session { cwd: cwd.into(), updated, ..Session::new(client, id) }
    }

    #[test]
    fn query_fields_negation_and_unknown_prefix() {
        let q = parse_query("api -draft client:cl -st:error http://x");
        assert_eq!(q.words, vec!["api", "http://x"]);
        assert_eq!(q.excluded, vec!["draft"]);
        assert_eq!(q.fields, vec![("client".into(), "cl".into(), false), ("status".into(), "error".into(), true)]);
    }

    #[test]
    fn resume_arguments_use_native_flags() {
        let mut s = session("codex", "abc", "/tmp/x y", 1.0);
        assert_eq!(s.command(), "codex resume abc --cd '/tmp/x y'");
        s.client = "omp".into();
        s.profile = "work".into();
        s.path = "/p/s.jsonl".into();
        assert_eq!(s.command(), "omp --profile work --resume /p/s.jsonl");
        s.resume_supported = false;
        assert!(s.argv().is_err());
    }

    #[test]
    fn projects_nest_and_order_by_latest_activity() {
        let dir = tempfile::tempdir().unwrap();
        let root = realpath(dir.path()).to_string_lossy().into_owned();
        let api = Path::new(&root).join("api").to_string_lossy().into_owned();
        let other = format!("{root}-other");
        for p in [&api, &other] {
            std::fs::create_dir_all(p).unwrap();
        }
        let sessions = vec![session("omp", "1", &root, 10.0), session("claude", "2", &api, 30.0), session("codex", "3", &other, 20.0)];
        let tree = group_projects(&sessions, &Grouping::all(), true);
        let names: Vec<(String, String)> = tree.iter().map(|p| (p.prefix.clone(), p.cwd.clone())).collect();
        assert_eq!(names, vec![(String::new(), root.clone()), ("└─ ".into(), api.clone()), (String::new(), other.clone())]);
        let flat = group_projects(&sessions, &Grouping::all(), false);
        assert_eq!(flat[0].cwd, api);
        let entries = folder_entries(&sessions, &root, &Grouping::all());
        assert!(matches!(&entries[0], Entry::Project(p) if p.cwd == api));
        assert!(matches!(&entries[1], Entry::Session(s) if s.id == "1"));
    }

    #[test]
    fn durations() {
        assert_eq!(duration("90m"), Some(5400.0));
        assert_eq!(duration("2w"), Some(1_209_600.0));
        assert_eq!(duration("3"), Some(259_200.0));
        assert!(duration("today").unwrap() >= 1.0);
        assert_eq!(duration("soon"), None);
    }
}
