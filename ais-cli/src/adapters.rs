//! Readers for each client's local history store.

use crate::i18n::{home, tr};
use crate::model::{CLIENTS, Session, Todo, canonical, stamp, system_time};
use crate::storage::{Cache, SAMPLE_LIMIT, json_records, query_rows, read_database, signature};
use crate::text::{clean, clean_value, line, line_value, py_str, pystr, truthy};
use crate::trf;
use indexmap::IndexMap;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const TEXT_LIMIT: usize = 6000;

fn get<'a>(map: &'a Map<String, Value>, key: &str) -> &'a Value {
    map.get(key).unwrap_or(&Value::Null)
}

/// First truthy value of `map[keys]`, Python `a or b or c`.
fn first<'a>(map: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().filter_map(|k| map.get(*k)).find(|v| truthy(v))
}

fn first_str(map: &Map<String, Value>, keys: &[&str], default: &str) -> String {
    first(map, keys).map(pystr).unwrap_or_else(|| default.to_string())
}

/// `map.get(key, default)` rendered with `line`.
fn get_line(map: &Map<String, Value>, key: &str, default: &str, limit: usize) -> String {
    match map.get(key) {
        Some(v) => line_value(v, limit),
        None => line(default, limit),
    }
}

fn empty() -> &'static Map<String, Value> {
    static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
    EMPTY.get_or_init(Map::new)
}

fn object<'a>(map: &'a Map<String, Value>, key: &str) -> &'a Map<String, Value> {
    map.get(key).and_then(Value::as_object).unwrap_or(empty())
}

pub fn text_content(content: &Value) -> String {
    match content {
        Value::String(text) => clean(text, TEXT_LIMIT),
        Value::Array(blocks) => {
            let parts: Vec<String> = blocks
                .iter()
                .filter_map(Value::as_object)
                .filter(|b| matches!(b.get("type").and_then(Value::as_str), Some("text" | "input_text" | "output_text")))
                .map(|b| b.get("text").map(py_str).unwrap_or_default())
                .collect();
            clean(&parts.join("\n"), TEXT_LIMIT)
        }
        _ => String::new(),
    }
}

fn tool_blocks(content: &Value) -> Vec<&Map<String, Value>> {
    match content {
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(Value::as_object)
            .filter(|b| matches!(b.get("type").and_then(Value::as_str), Some("tool_use" | "toolCall")))
            .collect(),
        _ => Vec::new(),
    }
}

fn model_name(model: &Value) -> String {
    match model {
        Value::Object(m) => match m.get("id") {
            Some(id) => line_value(id, 0),
            None => m.get("name").map(|v| line_value(v, 0)).unwrap_or_default(),
        },
        other => line_value(other, 0),
    }
}

fn is_ask(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower == "ask" || lower == "askuserquestion"
}

pub fn set_message(s: &mut Session, role: &str, content: &Value, message: &Map<String, Value>) {
    if role != "user" && role != "assistant" {
        return;
    }
    let text = text_content(content);
    if role == "user" {
        if text.is_empty() {
            return;
        }
        let trimmed = text.trim_start();
        if ["<local-command-", "<command-name>", "<system-reminder>"].iter().any(|p| trimmed.starts_with(p)) {
            return;
        }
        s.messages += 1;
        s.status = "user".into();
        s.status_detail = tr("Last record: user prompt");
        if s.title.is_empty() {
            s.title = line(&text, 160);
        }
        s.last_user = text;
        return;
    }
    if !text.is_empty() {
        s.last_assistant = text.clone();
    }
    s.messages += 1;
    if let Some(model) = message.get("model")
        && truthy(model)
        && model.as_str() != Some("<synthetic>")
    {
        s.model = model_name(model);
    }
    let tools = tool_blocks(content);
    s.tools += tools.len() as i64;
    if let Some(last) = tools.last() {
        let name = last.get("name").map(py_str).unwrap_or_else(|| "tool".into());
        s.status = if is_ask(&name) { "awaiting" } else { "tool" }.into();
        s.last_action = line(&first_str(last, &["intent"], &name), 1000);
        s.status_detail = trf!("Last record: tool call {v0}", v0 = &name);
    } else if !text.is_empty() {
        s.status = "ready".into();
        s.status_detail = tr("Last record: assistant response");
    } else if content
        .as_array()
        .is_some_and(|blocks| blocks.iter().filter_map(Value::as_object).any(|b| b.get("type").and_then(Value::as_str) == Some("thinking")))
    {
        s.status = "thinking".into();
        s.status_detail = tr("Last record: thinking");
    }
    let reason = match message.get("stopReason") {
        Some(v) => py_str(v),
        None => message.get("stop_reason").map(py_str).unwrap_or_default(),
    }
    .to_lowercase();
    if matches!(reason.as_str(), "error" | "aborted" | "interrupted") {
        s.status = if reason == "error" { "error" } else { "interrupted" }.into();
        s.status_detail = line(&first_str(message, &["errorMessage"], &reason), 2000);
    }
}

fn file_stem(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

pub fn parse_jsonl(path: &Path, client: &str, full: bool, profile: &str, state_root: &str) -> std::io::Result<Session> {
    let meta = std::fs::metadata(path)?;
    let stem = file_stem(path);
    let mut s = Session {
        path: path.to_string_lossy().into_owned(),
        updated: meta.modified().map(system_time).unwrap_or(0.0),
        profile: profile.into(),
        state_root: state_root.into(),
        source: "JSONL".into(),
        exact: full || meta.len() <= SAMPLE_LIMIT,
        source_sig: signature(path).ok(),
        ..Session::new(client, &stem)
    };
    if (client == "omp" || client == "pi") && stem.contains('_') {
        s.id = stem.rsplit('_').next().unwrap_or(&stem).to_string();
    }
    if client == "codex" && stem.starts_with("rollout-") {
        let chars: Vec<char> = stem.chars().collect();
        s.id = chars[chars.len().saturating_sub(36)..].iter().collect();
    }
    let mut explicit_title = String::new();
    json_records(path, full, |r| {
        let kind = r.get("type").and_then(Value::as_str).unwrap_or("");
        if kind == "session" || kind == "session_meta" {
            let payload = r.get("payload").and_then(Value::as_object).unwrap_or(r);
            s.id = first_str(payload, &["id", "session_id"], &s.id);
            s.cwd = match payload.get("cwd") {
                Some(v) => canonical(&pystr(v)),
                None => canonical(&s.cwd),
            };
            s.created = stamp(get(payload, "timestamp"), s.created);
            if let Some(title) = payload.get("title").filter(|v| truthy(v)) {
                explicit_title = line_value(title, 500);
            }
            s.parent = first_str(payload, &["parentSessionId", "parentSession"], &s.parent);
            s.provider = get_line(payload, "model_provider", &s.provider.clone(), 0);
            s.source = line(&first_str(payload, &["originator", "source"], &s.source), 0);
            if let Some(Value::Object(source)) = payload.get("source")
                && let Some(agent) = source.get("subagent")
            {
                s.parent = match agent {
                    Value::Object(agent) => match object(agent, "thread_spawn").get("parent_thread_id") {
                        Some(v) => py_str(v),
                        None => "subagent".into(),
                    },
                    _ => "subagent".into(),
                };
            }
        }
        if let Some(cwd) = r.get("cwd").filter(|v| truthy(v)) {
            s.cwd = canonical(&py_str(cwd));
        }
        if let Some(branch) = r.get("gitBranch").filter(|v| truthy(v)) {
            s.branch = line_value(branch, 0);
        }
        if r.get("isSidechain").is_some_and(truthy) {
            s.parent = first_str(r, &["parentSessionId"], "subagent");
        }
        if matches!(kind, "title" | "title_change" | "session_info" | "ai-title" | "summary")
            && let Some(title) = first(r, &["title", "name", "aiTitle", "summary"])
        {
            explicit_title = line_value(title, 500);
        }
        if kind == "model_change" {
            let role = r.get("role").and_then(|v| if v.is_null() { None } else { Some(py_str(v)) });
            if role.as_deref().is_some_and(|role| role != "default" && role != "main") {
                return;
            }
            let model = first(r, &["model", "modelId"]).cloned().unwrap_or(Value::String(String::new()));
            s.model = model_name(&model);
            let from_model = match &model {
                Value::Object(m) => m.get("provider").filter(|v| truthy(v)).map(pystr),
                _ => None,
            };
            s.provider = line(&from_model.unwrap_or_else(|| first_str(r, &["provider"], &s.provider)), 0);
        }
        if kind == "turn_context" {
            let p = object(r, "payload");
            s.model = get_line(p, "model", &s.model.clone(), 0);
            s.branch = get_line(p, "git_branch", &s.branch.clone(), 0);
        }
        if matches!(kind, "message" | "assistant" | "user")
            && let Some(Value::Object(m)) = r.get("message")
        {
            let role = m.get("role").map(py_str).unwrap_or_else(|| kind.to_string());
            set_message(&mut s, &role, get(m, "content"), m);
        }
        if kind == "custom" {
            let ct = r.get("customType").and_then(Value::as_str).unwrap_or("");
            let data = match r.get("data") {
                Some(Value::Object(d)) => d,
                None => empty(),
                _ => return,
            };
            if ct == "tool_execution_start" {
                let name = data.get("toolName").map(py_str).unwrap_or_else(|| "tool".into());
                s.status = if is_ask(&name) { "awaiting" } else { "tool" }.into();
                s.last_action = line(&first_str(data, &["intent"], &name), 1000);
                s.status_detail = trf!("Last record: {v0} · {v1}", v0 = &name, v1 = &s.last_action);
            } else if ct == "session_exit" {
                let reason = first_str(data, &["kind", "reason"], "normal");
                s.status = if reason.contains("error") {
                    "error"
                } else if ["crash", "interrupt", "signal"].iter().any(|x| reason.contains(x)) {
                    "interrupted"
                } else {
                    "closed"
                }
                .into();
                s.status_detail = tr("CLI exit: ") + &line(&first_str(data, &["reason"], &reason), 0);
            } else if ct == "user_todo_edit" || ct == "todo_update" {
                let mut todo = Vec::new();
                for phase in data.get("phases").and_then(Value::as_array).into_iter().flatten() {
                    let Some(phase) = phase.as_object() else { continue };
                    for task in phase.get("tasks").and_then(Value::as_array).into_iter().flatten() {
                        if let Some(task) = task.as_object() {
                            todo.push(Todo {
                                text: line(&first_str(task, &["content", "text"], ""), 500),
                                status: line_value(get(task, "status"), 0),
                            });
                        }
                    }
                }
                todo.truncate(40);
                s.todo = todo;
            }
        }
        if kind == "cost-state" {
            s.cost = r.get("totalCostUSD").and_then(Value::as_f64);
        }
        if kind == "model_usage"
            && let Some(total) = object(r, "usage").get("totalTokens").and_then(Value::as_f64)
        {
            s.tokens = Some(s.tokens.unwrap_or(0) + total as i64);
        }
        if kind == "system" && matches!(r.get("subtype").and_then(Value::as_str), Some("api_error" | "error")) {
            s.status = "error".into();
            s.status_detail = line(&first_str(r, &["content", "error"], &tr("API error")), 1500);
        }
        if kind == "response_item" {
            let p = object(r, "payload");
            let pk = p.get("type").and_then(Value::as_str).unwrap_or("");
            let role = p.get("role").and_then(Value::as_str).unwrap_or("");
            if pk == "message" && (role == "user" || role == "assistant") {
                let text = text_content(get(p, "content"));
                if role == "user" && ["<environment_context>", "# AGENTS.md", "<INSTRUCTIONS>"].iter().any(|m| text.starts_with(m)) {
                    return;
                }
                set_message(&mut s, role, get(p, "content"), p);
            } else if pk == "function_call" || pk == "custom_tool_call" {
                s.tools += 1;
                let name = p.get("name").map(py_str).unwrap_or_else(|| "tool".into());
                s.status = if name.contains("request_user_input") { "awaiting" } else { "tool" }.into();
                s.last_action = line(&name, 0);
                s.status_detail = tr("Last record: tool call ") + &line(&name, 0);
            }
        }
        if kind == "event_msg" {
            let p = object(r, "payload");
            let pk = p.get("type").and_then(Value::as_str).unwrap_or("");
            match pk {
                "task_complete" | "turn_complete" | "task_completed" => {
                    s.status = "ready".into();
                    s.status_detail = tr("Last record: turn completed");
                    if let Some(message) = p.get("last_agent_message").filter(|v| truthy(v)) {
                        s.last_assistant = clean_value(message, TEXT_LIMIT);
                    }
                }
                "turn_aborted" | "task_failed" => {
                    s.status = if pk == "turn_aborted" { "interrupted" } else { "error" }.into();
                    s.status_detail = line(&first_str(p, &["reason"], pk), 1500);
                }
                "token_count" => {
                    let info = p.get("info").and_then(Value::as_object).unwrap_or(empty());
                    if let Some(total) = info
                        .get("total_token_usage")
                        .and_then(Value::as_object)
                        .and_then(|t| t.get("total_tokens"))
                        .filter(|v| !v.is_null())
                    {
                        s.tokens = total.as_i64().or_else(|| total.as_f64().map(|f| f as i64));
                    }
                }
                "agent_message" | "user_message" => {
                    // Legacy rollouts may contain only events. Avoid double-counting
                    // mirrored response_item messages in modern rollouts.
                    let text = clean_value(get(p, "message"), TEXT_LIMIT);
                    if !text.is_empty() {
                        if pk == "agent_message" {
                            s.last_assistant = text;
                        } else {
                            if s.title.is_empty() {
                                s.title = line(&text, 160);
                            }
                            s.last_user = text;
                        }
                    }
                }
                _ => {}
            }
        }
    })?;
    s.title = if !explicit_title.is_empty() {
        explicit_title
    } else if !s.title.is_empty() {
        s.title.clone()
    } else {
        let from_user = line(&s.last_user, 160);
        if from_user.is_empty() { tr("Untitled") } else { from_user }
    };
    Ok(s)
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct SourceInfo {
    pub count: usize,
    pub roots: Vec<String>,
    pub error: String,
}

fn sibling_markers(client: &str) -> Option<[&'static str; 2]> {
    match client {
        "claude" => Some(["projects", ".claude.json"]),
        "codex" => Some(["sessions", "auth.json"]),
        _ => None,
    }
}

fn sorted_children(dir: &Path, keep: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| keep(p)).collect();
    found.sort();
    found
}

fn rglob_jsonl(folder: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            rglob_jsonl(&path, out);
        } else if path.extension().is_some_and(|e| e == "jsonl") {
            out.push(path);
        }
    }
}

fn mtime(path: &Path) -> f64 {
    std::fs::metadata(path).and_then(|m| m.modified()).map(system_time).unwrap_or(0.0)
}

/// Receives growing partial results and the read errors so far.
pub type Publish<'a> = &'a mut dyn FnMut(&[Session], &[String]);

#[derive(Clone)]
pub struct Collector {
    pub home: PathBuf,
    pub cache: Arc<Mutex<Cache>>,
    pub overrides: IndexMap<String, Vec<PathBuf>>,
    pub sources: IndexMap<String, SourceInfo>,
    pub errors: Vec<String>,
}

impl Collector {
    pub fn new(home_dir: PathBuf, cache: Cache, overrides: IndexMap<String, Vec<PathBuf>>) -> Self {
        Self {
            home: home_dir,
            cache: Arc::new(Mutex::new(cache)),
            overrides,
            sources: CLIENTS.iter().map(|c| (c.to_string(), SourceInfo::default())).collect(),
            errors: Vec::new(),
        }
    }

    pub fn is_real_home(&self) -> bool {
        self.home == home() || std::fs::canonicalize(home()).is_ok_and(|h| h == self.home)
    }

    pub fn reset(&mut self) {
        self.errors.clear();
        for source in self.sources.values_mut() {
            *source = SourceInfo::default();
        }
    }

    pub fn root(&self, client: &str) -> Vec<PathBuf> {
        if let Some(found) = self.overrides.get(client) {
            return found.clone();
        }
        let h = &self.home;
        let mut roots = vec![match client {
            "omp" => h.join(".omp/agent"),
            "pi" => h.join(".pi/agent"),
            "claude" => h.join(".claude"),
            "codex" => h.join(".codex"),
            "agy" => h.join(".gemini/antigravity-cli"),
            _ => h.join(".gemini"),
        }];
        let env = match client {
            "omp" => Some("PI_CODING_AGENT_DIR"),
            "claude" => Some("CLAUDE_CONFIG_DIR"),
            "codex" => Some("CODEX_HOME"),
            _ => None,
        };
        let env_value = env.and_then(std::env::var_os).filter(|v| !v.is_empty());
        if let Some(value) = env_value.filter(|_| self.is_real_home()) {
            let text = value.to_string_lossy();
            roots = vec![match text.strip_prefix("~/") {
                Some(rest) => home().join(rest),
                None => PathBuf::from(&*text),
            }];
        } else if let Some(markers) = sibling_markers(client) {
            // Extra accounts are usually separate config dirs: ~/.claude-work,
            // ~/.codex-personal. Only real stores with history or a login count.
            let prefix = format!(".{client}-");
            roots.extend(sorted_children(h, |p| {
                p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(&prefix))
                    && p.is_dir()
                    && markers.iter().any(|m| p.join(m).exists())
            }));
        }
        if client == "omp" {
            for profile in sorted_children(&h.join(".omp/profiles"), |p| p.join("agent").exists()) {
                roots.push(profile.join("agent"));
            }
        }
        roots
    }

    pub fn warn(&mut self, client: &str, message: &str) {
        let message = line(message, 600);
        if let Some(source) = self.sources.get_mut(client) {
            source.error = message.clone();
        }
        self.errors.push(format!("{client}: {message}"));
    }

    /// Scan every store; `publish` receives growing partial results.
    pub fn scan(&mut self, mut publish: Option<Publish<'_>>) -> Vec<Session> {
        let mut all: IndexMap<String, Session> = IndexMap::new();
        // Small, indexed stores appear first while OMP's file index is built.
        for client in ["agy", "codex", "claude", "gemini", "pi", "omp"] {
            for root in self.root(client) {
                if !root.exists() {
                    continue;
                }
                self.sources[client].roots.push(root.to_string_lossy().into_owned());
                let result = match client {
                    "agy" => self.scan_agy(&root),
                    "gemini" => Ok(self.scan_gemini(&root)),
                    _ => {
                        let errors_before = self.errors.clone();
                        let mut progress = |rows: &[Session]| {
                            if let Some(publish) = publish.as_mut() {
                                for item in rows {
                                    all.insert(item.key(), item.clone());
                                }
                                let snapshot: Vec<Session> = all.values().cloned().collect();
                                publish(&snapshot, &errors_before);
                            }
                        };
                        Ok(self.scan_files(&root, client, &mut progress))
                    }
                };
                match result {
                    Ok(rows) => {
                        self.sources[client].count += rows.len();
                        for s in rows {
                            all.insert(s.key(), s);
                        }
                    }
                    Err(error) => self.warn(client, &format!("{}: {error}", root.display())),
                }
                if let Some(publish) = publish.as_mut() {
                    let snapshot: Vec<Session> = all.values().cloned().collect();
                    publish(&snapshot, &self.errors);
                }
            }
        }
        self.cache.lock().unwrap().save();
        let mut result: Vec<Session> = all.into_values().collect();
        crate::model::sort_recent(&mut result);
        result
    }

    fn scan_files(&mut self, root: &Path, client: &str, progress: &mut dyn FnMut(&[Session])) -> Vec<Session> {
        let mut dirs = if client == "claude" { vec![root.join("projects")] } else { vec![root.join("sessions")] };
        if client == "codex" {
            dirs.push(root.join("archived_sessions"));
        }
        // Pi supports an explicit session directory independently of its state.
        if client == "pi"
            && let Some(dir) = std::env::var_os("PI_CODING_AGENT_SESSION_DIR").filter(|v| !v.is_empty())
            && self.is_real_home()
        {
            dirs = vec![PathBuf::from(dir)];
        }
        let name_of = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut profile = String::new();
        if client == "omp" && root.parent().and_then(Path::parent).is_some_and(|p| name_of(p) == "profiles") {
            profile = root.parent().map(name_of).unwrap_or_default();
        }
        if sibling_markers(client).is_some() && name_of(root).starts_with(&format!(".{client}-")) {
            profile = name_of(root).chars().skip(client.len() + 2).collect();
        }
        let state_root = root.to_string_lossy().into_owned();
        let mut sessions: IndexMap<String, Session> = IndexMap::new();
        for folder in &dirs {
            let mut paths = Vec::new();
            rglob_jsonl(folder, &mut paths);
            let mut keyed: Vec<(f64, PathBuf)> = paths.into_iter().map(|p| (mtime(&p), p)).collect();
            keyed.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            let archived = name_of(folder) == "archived_sessions";
            for (index, (_, path)) in keyed.iter().enumerate() {
                // Claude's subagent files should not pollute the top-level list.
                if client == "claude" && (path.components().any(|c| c.as_os_str() == "subagents") || name_of(path).starts_with("agent-")) {
                    continue;
                }
                let cached = self.cache.lock().unwrap().get(path, client, &profile);
                let parsed = match cached {
                    Some(s) => Ok(s),
                    None => parse_jsonl(path, client, false, &profile, &state_root).inspect(|s| {
                        self.cache.lock().unwrap().put(path, s);
                    }),
                };
                match parsed {
                    Ok(mut s) => {
                        if archived {
                            s.archived = true;
                        }
                        let newer = sessions.get(&s.id).is_none_or(|previous| s.updated > previous.updated);
                        if newer {
                            sessions.insert(s.id.clone(), s);
                        }
                    }
                    Err(error) => self.warn(client, &format!("{}: {error}", name_of(path))),
                }
                if index % 64 == 0 {
                    let rows: Vec<Session> = sessions.values().cloned().collect();
                    progress(&rows);
                }
            }
        }
        if client == "codex" {
            self.enrich_codex(root, &mut sessions);
        }
        if client == "omp" {
            self.enrich_omp(root, &mut sessions);
        }
        sessions.into_values().collect()
    }

    fn enrich_omp(&mut self, root: &Path, sessions: &mut IndexMap<String, Session>) {
        let path = root.join("history.db");
        if !path.exists() {
            return;
        }
        let result = read_database(&path).and_then(|con| query_rows(&con, "SELECT session_id,title FROM session_titles", &[]));
        match result {
            Ok(rows) => {
                for row in rows {
                    let id = row.get("session_id").map(py_str).unwrap_or_default();
                    if let Some(s) = sessions.get_mut(&id) {
                        s.title = line_value(row.get("title").unwrap_or(&Value::Null), 500);
                    }
                }
            }
            Err(error) => self.warn("omp", &trf!("Titles from history.db: {v0}", v0 = error.to_string())),
        }
    }

    fn enrich_codex(&mut self, root: &Path, sessions: &mut IndexMap<String, Session>) {
        let mut paths = sorted_children(root, |p| {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            name.starts_with("state_") && name.ends_with(".sqlite")
        });
        if paths.is_empty() {
            return;
        }
        paths.sort_by(|a, b| mtime(b).partial_cmp(&mtime(a)).unwrap_or(std::cmp::Ordering::Equal));
        let rows = match read_database(&paths[0]).and_then(|con| query_rows(&con, "SELECT * FROM threads", &[])) {
            Ok(rows) => rows,
            Err(error) => {
                self.warn("codex", &trf!("Codex index: {v0}", v0 = error.to_string()));
                return;
            }
        };
        let state_root = root.to_string_lossy().into_owned();
        for row in rows {
            let r: Map<String, Value> = row.into_iter().collect();
            let id = r.get("id").map(py_str).unwrap_or_default();
            if !sessions.contains_key(&id) {
                let path = PathBuf::from(r.get("rollout_path").map(pystr).unwrap_or_default());
                if !path.is_file() {
                    continue;
                }
                let s = Session { path: path.to_string_lossy().into_owned(), state_root: state_root.clone(), ..Session::new("codex", &id) };
                sessions.insert(id.clone(), s);
            }
            let s = sessions.get_mut(&id).unwrap();
            s.title = line(&first_str(&r, &["title"], &s.title), 500);
            s.cwd = canonical(&first_str(&r, &["cwd"], &s.cwd));
            s.updated = s.updated.max(stamp(first(&r, &["updated_at_ms", "updated_at"]).unwrap_or(&Value::Null), 0.0));
            s.created = stamp(first(&r, &["created_at_ms", "created_at"]).unwrap_or(&Value::Null), s.created);
            s.model = line(&first_str(&r, &["model"], &s.model), 0);
            s.provider = line(&first_str(&r, &["model_provider"], &s.provider), 0);
            s.branch = line(&first_str(&r, &["git_branch"], &s.branch), 0);
            if let Some(tokens) = first(&r, &["tokens_used"]) {
                s.tokens = tokens.as_i64().or_else(|| tokens.as_f64().map(|f| f as i64));
            }
            s.archived = r.get("archived").is_some_and(truthy);
            s.parent = first_str(&r, &["attached_thread_parent_id"], &s.parent);
            let source = r.get("source").map(pystr).unwrap_or_default();
            if (source.contains("\"subagent\"") || source == "subagent") && s.parent.is_empty() {
                s.parent = "subagent".into();
            }
            let fallback = if source.is_empty() { s.source.clone() } else { source };
            s.source = line(&first_str(&r, &["originator"], &fallback), 0);
        }
    }

    fn scan_agy(&mut self, root: &Path) -> Result<Vec<Session>, String> {
        let path = root.join("conversation_summaries.db");
        if !path.exists() {
            return Ok(Vec::new());
        }
        let con = read_database(&path).map_err(|e| e.to_string())?;
        let rows = query_rows(&con, "SELECT * FROM conversation_summaries", &[]).map_err(|e| e.to_string())?;
        let custom = canonical(&root.to_string_lossy()) != canonical(&home().join(".gemini/antigravity-cli").to_string_lossy());
        let mut result = Vec::new();
        for row in rows {
            let r: Map<String, Value> = row.into_iter().collect();
            let uris = r.get("workspace_uris").map(pystr).unwrap_or_default();
            let uris: Value = serde_json::from_str(if uris.is_empty() { "[]" } else { &uris }).map_err(|e| e.to_string())?;
            let workspaces: Vec<String> = uris.as_array().into_iter().flatten().filter_map(Value::as_str).map(canonical).collect();
            let status = r.get("status").map(pystr).unwrap_or_default();
            let mut mapped = match status.to_lowercase().as_str() {
                "completed" | "done" => "ready",
                "error" | "failed" => "error",
                "cancelled" => "interrupted",
                "waiting" => "awaiting",
                "running" => "tool",
                _ => "saved",
            };
            if r.get("killed").is_some_and(truthy) {
                mapped = "interrupted";
            }
            let mut s = Session {
                cwd: workspaces.first().cloned().unwrap_or_default(),
                title: line(&first_str(&r, &["title", "preview"], &tr("Untitled")), 500),
                path: path.to_string_lossy().into_owned(),
                updated: stamp(get(&r, "last_modified_time"), 0.0),
                status: mapped.into(),
                status_detail: tr("Status in agy index: ") + &if status.is_empty() { tr("saved") } else { status.clone() },
                last_user: clean_value(get(&r, "preview"), TEXT_LIMIT),
                steps: r.get("step_count").and_then(Value::as_i64),
                source: line(&first_str(&r, &["source"], "agy CLI"), 0),
                project_id: first_str(&r, &["project_id"], ""),
                parent: first_str(&r, &["parent_conversation_id"], ""),
                state_root: root.to_string_lossy().into_owned(),
                workspaces,
                exact: true,
                ..Session::new("agy", &r.get("conversation_id").map(py_str).unwrap_or_default())
            };
            s.note = tr("Status was saved by the client; it does not confirm a running process.");
            if custom {
                s.resume_supported = false;
                s.note += &tr(" Resuming from a custom agy directory is not supported.");
            }
            result.push(s);
        }
        Ok(result)
    }

    fn scan_gemini(&mut self, root: &Path) -> Vec<Session> {
        let mut project_map: std::collections::HashMap<String, String> = Default::default();
        let sha = |text: &str| format!("{:x}", Sha256::digest(text.as_bytes()));
        if let Ok(text) = std::fs::read_to_string(root.join("projects.json"))
            && let Ok(Value::Object(data)) = serde_json::from_str::<Value>(&text)
            && let Some(Value::Object(projects)) = data.get("projects")
        {
            for (cwd, slug) in projects {
                project_map.insert(py_str(slug), canonical(cwd));
                project_map.insert(sha(cwd), canonical(cwd));
            }
        }
        // Older Gemini uses sha256(cwd); its history file contains the original
        // project path. No directory name is decoded heuristically.
        if let Ok(text) = std::fs::read_to_string(root.join("history.json"))
            && let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&text)
        {
            for item in items {
                if let Some(cwd) = item.get("project").filter(|v| truthy(v)) {
                    let cwd = py_str(cwd);
                    project_map.insert(sha(&cwd), canonical(&cwd));
                }
            }
        }
        let custom = canonical(&root.to_string_lossy()) != canonical(&home().join(".gemini").to_string_lossy());
        let mut result = Vec::new();
        for folder in sorted_children(&root.join("tmp"), |p| p.is_dir()) {
            for path in sorted_children(&folder.join("chats"), |p| {
                let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                name.starts_with("session-") && name.ends_with(".json")
            }) {
                match self.read_gemini(&path, &folder, &project_map, root, custom) {
                    Ok(s) => result.push(s),
                    Err(error) => {
                        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        self.warn("gemini", &format!("{name}: {error}"));
                    }
                }
            }
        }
        result
    }

    fn read_gemini(
        &self,
        path: &Path,
        folder: &Path,
        project_map: &std::collections::HashMap<String, String>,
        root: &Path,
        custom: bool,
    ) -> Result<Session, String> {
        let name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut cwd = project_map.get(&name).cloned().unwrap_or_default();
        let marker = folder.join(".project_root");
        if marker.exists() {
            cwd = canonical(std::fs::read_to_string(&marker).map_err(|e| e.to_string())?.trim());
        }
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let r: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        let r = r.as_object().ok_or("not an object")?;
        let id = r.get("sessionId").ok_or("'sessionId'")?;
        let mut s = Session {
            cwd: cwd.clone(),
            path: path.to_string_lossy().into_owned(),
            updated: stamp(get(r, "lastUpdated"), mtime(path)),
            created: stamp(get(r, "startTime"), 0.0),
            title: line_value(get(r, "summary"), 500),
            state_root: root.to_string_lossy().into_owned(),
            source: "Gemini CLI".into(),
            exact: true,
            ..Session::new("gemini", &py_str(id))
        };
        for m in r.get("messages").and_then(Value::as_array).into_iter().flatten() {
            let Some(m) = m.as_object() else { return Err("message is not an object".into()) };
            let kind = m.get("type").map(py_str).unwrap_or_default();
            let role = if kind == "gemini" { "assistant".to_string() } else { kind };
            set_message(&mut s, &role, get(m, "content"), m);
            s.tools += m.get("toolCalls").and_then(Value::as_array).map(Vec::len).unwrap_or(0) as i64;
            s.model = line(&first_str(m, &["model"], &s.model), 0);
        }
        if s.title.is_empty() {
            let from_user = line(&s.last_user, 160);
            s.title = if from_user.is_empty() { tr("Untitled") } else { from_user };
        }
        if cwd.is_empty() {
            s.resume_supported = false;
            s.note = tr("History found, but the original Gemini directory is unknown.");
        } else if custom {
            s.resume_supported = false;
            s.note = tr("Resuming from a custom Gemini directory is not supported.");
        }
        Ok(s)
    }
}

/// Complete details for one session (full file / FTS read).
pub fn detail(session: &Session) -> Result<Session, String> {
    if session.client == "agy" {
        let mut s = session.clone();
        if let Ok(con) = read_database(Path::new(&s.path))
            && let Ok(rows) = query_rows(
                &con,
                "SELECT user_body,agent_body FROM conversations_fts WHERE conversation_id=? ORDER BY CAST(chunk_index AS INTEGER) DESC LIMIT 1",
                &[&s.id],
            )
            && let Some(row) = rows.first()
        {
            let user = clean_value(row.get("user_body").unwrap_or(&Value::Null), TEXT_LIMIT).trim().to_string();
            if !user.is_empty() {
                s.last_user = user;
            }
            let agent = pystr(row.get("agent_body").unwrap_or(&Value::Null));
            let agent: Vec<char> = agent.trim().chars().collect();
            s.last_assistant = clean(&agent[agent.len().saturating_sub(TEXT_LIMIT)..].iter().collect::<String>(), 0);
        }
        return Ok(s);
    }
    if session.client == "gemini" || session.exact {
        return Ok(session.clone());
    }
    let mut s =
        parse_jsonl(Path::new(&session.path), &session.client, true, &session.profile, &session.state_root).map_err(|e| e.to_string())?;
    // Index-provided metadata is authoritative for titles and archive flags.
    s.title = session.title.clone();
    s.archived = session.archived;
    s.parent = session.parent.clone();
    s.updated = s.updated.max(session.updated);
    if session.tokens.is_some() {
        s.tokens = session.tokens;
    }
    if s.branch.is_empty() {
        s.branch = session.branch.clone();
    }
    if s.model.is_empty() {
        s.model = session.model.clone();
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_lines(path: &Path, lines: &[Value]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut file = std::fs::File::create(path).unwrap();
        for item in lines {
            writeln!(file, "{item}").unwrap();
        }
    }

    #[test]
    fn omp_title_status_last_action_and_partial_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("2025_abc.jsonl");
        write_lines(
            &path,
            &[
                serde_json::json!({"type": "session", "id": "abc", "cwd": dir.path(), "timestamp": "2025-01-01T00:00:00Z"}),
                serde_json::json!({"type": "message", "message": {"role": "user", "content": "Fix the build"}}),
                serde_json::json!({"type": "model_change", "model": {"id": "opus", "provider": "anthropic"}}),
                serde_json::json!({"type": "custom", "customType": "tool_execution_start", "data": {"toolName": "bash", "intent": "Running tests"}}),
            ],
        );
        let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        write!(file, "{{\"type\": \"message\", \"mess").unwrap();
        let s = parse_jsonl(&path, "omp", false, "", "").unwrap();
        assert_eq!(s.id, "abc");
        assert_eq!(s.title, "Fix the build");
        assert_eq!(s.status, "tool");
        assert_eq!(s.last_action, "Running tests");
        assert_eq!(s.model, "opus");
        assert_eq!(s.provider, "anthropic");
        assert_eq!(s.created, 1_735_689_600.0);
    }

    #[test]
    fn claude_metadata_and_aborted_reply() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("id.jsonl");
        write_lines(
            &path,
            &[
                serde_json::json!({"type": "user", "cwd": dir.path(), "gitBranch": "main", "message": {"role": "user", "content": [{"type": "text", "text": "Hi"}]}}),
                serde_json::json!({"type": "assistant", "message": {"role": "assistant", "model": "claude-opus", "content": [{"type": "text", "text": "Hello"}], "stop_reason": "end_turn"}}),
                serde_json::json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "content": "x"}]}}),
                serde_json::json!({"type": "summary", "summary": "Greeting"}),
            ],
        );
        let s = parse_jsonl(&path, "claude", false, "", "").unwrap();
        assert_eq!((s.title.as_str(), s.status.as_str(), s.last_assistant.as_str()), ("Greeting", "ready", "Hello"));
        assert_eq!(s.branch, "main");
        assert_eq!(s.messages, 2);
    }

    #[test]
    fn codex_rollout_id_children_and_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions/rollout-2025-01-01T00-00-00-019e7000-aaaa-7000-bbbb-000000000001.jsonl");
        write_lines(
            &path,
            &[
                serde_json::json!({"type": "session_meta", "payload": {"id": "019e7000-aaaa-7000-bbbb-000000000001", "cwd": dir.path(), "source": {"subagent": {"thread_spawn": {"parent_thread_id": "p1"}}}}}),
                serde_json::json!({"type": "event_msg", "payload": {"type": "token_count", "info": {"total_token_usage": {"total_tokens": 1234}}}}),
                serde_json::json!({"type": "event_msg", "payload": {"type": "task_complete", "last_agent_message": "Done"}}),
            ],
        );
        let s = parse_jsonl(&path, "codex", false, "", "").unwrap();
        assert_eq!(s.id, "019e7000-aaaa-7000-bbbb-000000000001");
        assert_eq!(s.parent, "p1");
        assert_eq!(s.tokens, Some(1234));
        assert_eq!(s.status, "ready");
        assert_eq!(s.last_assistant, "Done");
    }

    #[test]
    fn duplicate_moved_sessions_keep_newest_file_and_siblings_are_profiles() {
        let dir = tempfile::tempdir().unwrap();
        let home_dir = dir.path().to_path_buf();
        let older = home_dir.join(".codex/sessions/a/rollout-x-019e7000-aaaa-7000-bbbb-000000000009.jsonl");
        let newer = home_dir.join(".codex/archived_sessions/rollout-x-019e7000-aaaa-7000-bbbb-000000000009.jsonl");
        write_lines(&older, &[serde_json::json!({"type": "session_meta", "payload": {"title": "old"}})]);
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_lines(&newer, &[serde_json::json!({"type": "session_meta", "payload": {"title": "new"}})]);
        write_lines(&home_dir.join(".claude-work/projects/p/s1.jsonl"), &[serde_json::json!({"type": "summary", "summary": "work"})]);
        let mut collector = Collector::new(home_dir, Cache::disabled(), IndexMap::new());
        let sessions = collector.scan(None);
        let codex: Vec<&Session> = sessions.iter().filter(|s| s.client == "codex").collect();
        assert_eq!(codex.len(), 1);
        assert_eq!(codex[0].title, "new");
        assert!(codex[0].archived);
        let claude = sessions.iter().find(|s| s.client == "claude").unwrap();
        assert_eq!(claude.profile, "work");
    }
}
