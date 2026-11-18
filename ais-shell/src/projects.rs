use crate::cli::{CliKind, canonical_path};
use percent_encoding::percent_decode_str;
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Newest history file per CLI: (file, project, title, modified).
type LatestFiles = HashMap<CliKind, (PathBuf, PathBuf, Option<String>, Option<SystemTime>)>;

#[derive(Clone, Debug)]
pub struct CachedSessionHeader {
    pub modified: Option<SystemTime>,
    pub size: u64,
    pub cwd: PathBuf,
    pub title: Option<String>,
}

#[derive(Clone, Debug)]
pub struct CachedGeminiFile {
    pub modified: Option<SystemTime>,
    pub size: u64,
    pub cwd: PathBuf,
    pub updated: Option<SystemTime>,
    pub status: String,
    pub title: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ScanCache {
    pub jsonl: HashMap<PathBuf, CachedSessionHeader>,
    pub gemini: HashMap<PathBuf, CachedGeminiFile>,
}

#[derive(Clone, Debug)]
pub struct CliSnapshot {
    pub status: String,
    pub updated: Option<SystemTime>,
    pub project: PathBuf,
    pub title: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ScanResult {
    pub projects: Vec<Project>,
    pub latest: BTreeMap<CliKind, CliSnapshot>,
}

#[derive(Clone, Debug)]
pub struct Project {
    pub path: PathBuf,
    pub sessions: usize,
    pub clients: BTreeMap<CliKind, usize>,
    pub updated: Option<SystemTime>,
    pub last_title: Option<String>,
}

impl Project {
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| self.path.display().to_string())
    }

    pub fn pretty_path(&self, home: &Path) -> String {
        let text = self.path.to_string_lossy();
        let home = home.to_string_lossy();
        let base = home.trim_end_matches(['/', '\\']);
        if text == home || text == base {
            "~".to_string()
        } else if let Some(rest) = text.strip_prefix(base) {
            if rest.starts_with('/') || rest.starts_with('\\') {
                format!("~{rest}")
            } else {
                text.into_owned()
            }
        } else {
            text.into_owned()
        }
    }

    pub fn age(&self) -> String {
        let Some(updated) = self.updated else {
            return "—".to_string();
        };
        let Ok(elapsed) = SystemTime::now().duration_since(updated) else {
            return "now".to_string();
        };
        let seconds = elapsed.as_secs();
        if seconds < 60 {
            "now".to_string()
        } else if seconds < 3_600 {
            format!("{}m", seconds / 60)
        } else if seconds < 86_400 {
            format!("{}h", seconds / 3_600)
        } else {
            format!("{}d", seconds / 86_400)
        }
    }
}

#[cfg(test)]
pub fn scan_projects(home: &Path) -> ScanResult {
    let mut cache = ScanCache::default();
    scan_projects_cached(home, &mut cache)
}

pub fn scan_projects_cached(home: &Path, cache: &mut ScanCache) -> ScanResult {
    let mut projects: HashMap<PathBuf, Project> = HashMap::new();
    let mut latest_files: LatestFiles = HashMap::new();
    let mut roots = vec![
        (home.join(".omp/agent/sessions"), CliKind::Omp),
        (home.join(".claude/projects"), CliKind::Claude),
        (home.join(".codex/sessions"), CliKind::Codex),
        (home.join(".pi/agent/sessions"), CliKind::Pi),
    ];
    if let Ok(profiles) = fs::read_dir(home.join(".omp/profiles")) {
        for profile in profiles.flatten() {
            roots.push((profile.path().join("agent/sessions"), CliKind::Omp));
        }
    }
    for (root, client) in roots {
        scan_root(&root, client, &mut projects, &mut latest_files, cache, 0);
    }
    let agy_snapshot = scan_agy(home, &mut projects);
    let gemini_snapshot = scan_gemini(home, &mut projects, cache);
    let mut result: Vec<Project> = projects.into_values().collect();
    result.sort_by(|left, right| {
        right
            .updated
            .cmp(&left.updated)
            .then_with(|| left.path.cmp(&right.path))
    });
    let mut latest: BTreeMap<CliKind, CliSnapshot> = latest_files
        .into_iter()
        .filter_map(|(client, (path, project, title, updated))| {
            read_tail_status(&path).map(|status| {
                (
                    client,
                    CliSnapshot {
                        status,
                        updated,
                        project,
                        title,
                    },
                )
            })
        })
        .collect();
    if let Some(snapshot) = agy_snapshot {
        latest.insert(CliKind::Agy, snapshot);
    }
    if let Some(snapshot) = gemini_snapshot {
        latest.insert(CliKind::Gemini, snapshot);
    }
    ScanResult {
        projects: result,
        latest,
    }
}

fn scan_agy(home: &Path, projects: &mut HashMap<PathBuf, Project>) -> Option<CliSnapshot> {
    let path = home.join(".gemini/antigravity-cli/conversation_summaries.db");
    if !path.is_file() {
        return None;
    }
    let connection = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let mut statement = connection
        .prepare(
            "SELECT title, status, workspace_uris, last_modified_time \
             FROM conversation_summaries ORDER BY last_modified_time DESC LIMIT 500",
        )
        .ok()?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0).unwrap_or_default(),
                row.get::<_, String>(1).unwrap_or_default(),
                row.get::<_, String>(2).unwrap_or_default(),
                row.get::<_, String>(3).unwrap_or_default(),
            ))
        })
        .ok()?;
    let mut latest = None;
    for (title, status, workspaces, modified) in rows.flatten() {
        let updated = parse_sqlite_time(&modified);
        let paths = parse_workspace_uris(&workspaces);
        if latest.is_none() {
            latest = Some(CliSnapshot {
                status: agy_status(&status),
                updated,
                project: paths.first().cloned().unwrap_or_default(),
                title: (!title.is_empty()).then_some(title.clone()),
            });
        }
        for cwd in paths {
            let canonical = canonical_path(&cwd);
            let project = projects.entry(canonical.clone()).or_insert_with(|| Project {
                path: canonical,
                sessions: 0,
                clients: BTreeMap::new(),
                updated: None,
                last_title: None,
            });
            project.sessions += 1;
            *project.clients.entry(CliKind::Agy).or_insert(0) += 1;
            project.updated = match (project.updated, updated) {
                (Some(old), Some(new)) => Some(old.max(new)),
                (old, new) => old.or(new),
            };
            if project.last_title.is_none() && !title.is_empty() {
                project.last_title = Some(title.clone());
            }
        }
    }
    latest
}

fn scan_gemini(home: &Path, projects: &mut HashMap<PathBuf, Project>, cache: &mut ScanCache) -> Option<CliSnapshot> {
    let mapping_path = home.join(".gemini/projects.json");
    let mapping: Value = serde_json::from_slice(&fs::read(&mapping_path).ok()?).ok()?;
    let by_hash: HashMap<String, PathBuf> = mapping
        .get("projects")?
        .as_object()?
        .keys()
        .map(|cwd| {
            let hash = format!("{:x}", Sha256::digest(cwd.as_bytes()));
            (hash, PathBuf::from(cwd))
        })
        .collect();
    let tmp = home.join(".gemini/tmp");
    let Ok(entries) = fs::read_dir(tmp) else {
        return None;
    };
    let mut latest: Option<CliSnapshot> = None;
    for entry in entries.flatten() {
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        let fallback_hash = entry.file_name().to_string_lossy().into_owned();
        let chats = entry.path().join("chats");
        let Ok(files) = fs::read_dir(chats) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("");
            if !name.starts_with("session-") || path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Ok(meta) = fs::metadata(&path) else {
                continue;
            };
            let modified = meta.modified().ok();
            let size = meta.len();
            let cached = cache
                .gemini
                .get(&path)
                .filter(|entry| entry.modified == modified && entry.size == size)
                .cloned();
            let parsed = match cached {
                Some(entry) => entry,
                None => {
                    let Ok(bytes) = fs::read(&path) else {
                        continue;
                    };
                    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
                        continue;
                    };
                    let hash = value
                        .get("projectHash")
                        .and_then(Value::as_str)
                        .unwrap_or(&fallback_hash);
                    let Some(cwd) = by_hash.get(hash) else {
                        continue;
                    };
                    let updated = value
                        .get("lastUpdated")
                        .and_then(Value::as_str)
                        .and_then(parse_iso_timestamp)
                        .or(modified);
                    let status = gemini_status(&value);
                    let title = value
                        .get("title")
                        .and_then(Value::as_str)
                        .filter(|title| !title.is_empty())
                        .map(str::to_string);
                    let item = CachedGeminiFile {
                        modified,
                        size,
                        cwd: cwd.clone(),
                        updated,
                        status,
                        title,
                    };
                    cache.gemini.insert(path.clone(), item.clone());
                    item
                }
            };
            let canonical = canonical_path(&parsed.cwd);
            let project = projects.entry(canonical.clone()).or_insert_with(|| Project {
                path: canonical,
                sessions: 0,
                clients: BTreeMap::new(),
                updated: None,
                last_title: None,
            });
            project.sessions += 1;
            *project.clients.entry(CliKind::Gemini).or_insert(0) += 1;
            project.updated = match (project.updated, parsed.updated) {
                (Some(old), Some(new)) => Some(old.max(new)),
                (old, new) => old.or(new),
            };
            let replace = latest
                .as_ref()
                .and_then(|snapshot| snapshot.updated)
                .zip(parsed.updated)
                .is_none_or(|(old, new)| new >= old);
            if replace {
                latest = Some(CliSnapshot {
                    status: parsed.status,
                    updated: parsed.updated,
                    project: parsed.cwd,
                    title: parsed.title,
                });
            }
        }
    }
    latest
}

fn gemini_status(value: &Value) -> String {
    let Some(messages) = value.get("messages").and_then(Value::as_array) else {
        return "saved".to_string();
    };
    for message in messages.iter().rev() {
        let kind = message.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "user" => return "prompt".to_string(),
            "gemini" | "model" | "assistant" => {
                if message
                    .get("toolCalls")
                    .and_then(Value::as_array)
                    .is_some_and(|calls| !calls.is_empty())
                {
                    return "tool".to_string();
                }
                if message
                    .get("content")
                    .and_then(Value::as_str)
                    .is_some_and(|content| !content.is_empty())
                {
                    return "ready".to_string();
                }
                return "thinking".to_string();
            }
            "error" => return "error".to_string(),
            _ => {}
        }
    }
    "saved".to_string()
}

fn parse_iso_timestamp(value: &str) -> Option<SystemTime> {
    let normalized = value.trim().trim_end_matches('Z').replace('T', " ");
    parse_sqlite_time(&normalized)
}

fn parse_workspace_uris(raw: &str) -> Vec<PathBuf> {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_str)
        .filter_map(|value| value.strip_prefix("file://"))
        .map(|value| {
            let decoded = percent_decode_str(value).decode_utf8_lossy().into_owned();
            #[cfg(windows)]
            {
                let bytes = decoded.as_bytes();
                if bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':' {
                    return PathBuf::from(&decoded[1..]);
                }
            }
            PathBuf::from(decoded)
        })
        .collect()
}

fn agy_status(status: &str) -> String {
    let upper = status.to_ascii_uppercase();
    if upper.contains("RUNNING") {
        "running".to_string()
    } else if upper.contains("IDLE") || upper.contains("READY") || upper.contains("COMPLETE") {
        "ready".to_string()
    } else if upper.contains("ERROR") || upper.contains("FAIL") {
        "error".to_string()
    } else if upper.contains("CANCEL") || upper.contains("KILL") || upper.contains("ABORT") {
        "interrupted".to_string()
    } else {
        "saved".to_string()
    }
}

fn parse_sqlite_time(value: &str) -> Option<SystemTime> {
    let value = value.trim().trim_end_matches(" UTC");
    let (date, rest) = value.split_once(' ')?;
    let mut date = date.split('-');
    let year = date.next()?.parse::<i64>().ok()?;
    let month = date.next()?.parse::<u32>().ok()?;
    let day = date.next()?.parse::<u32>().ok()?;
    let clock = rest.split(['.', '+', '-']).next()?;
    let mut clock = clock.split(':');
    let hour = clock.next()?.parse::<u64>().ok()?;
    let minute = clock.next()?.parse::<u64>().ok()?;
    let second = clock.next()?.parse::<u64>().ok()?;
    if !(1970..=9999).contains(&year) || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let seconds = days.max(0) as u64 * 86_400 + hour * 3_600 + minute * 60 + second;
    UNIX_EPOCH.checked_add(Duration::from_secs(seconds))
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_prime = (i64::from(month) + 9) % 12;
    let day_of_year = (153 * month_prime + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn scan_root(
    root: &Path,
    client: CliKind,
    projects: &mut HashMap<PathBuf, Project>,
    latest_files: &mut LatestFiles,
    cache: &mut ScanCache,
    depth: usize,
) {
    if depth > 8 {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            if client == CliKind::Claude && path.file_name().and_then(|n| n.to_str()) == Some("subagents") {
                continue;
            }
            scan_root(&path, client, projects, latest_files, cache, depth + 1);
            continue;
        }
        if !kind.is_file() {
            continue;
        }
        if client == CliKind::Claude
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("agent-"))
        {
            continue;
        }
        let extension = path.extension().and_then(|value| value.to_str());
        if extension != Some("jsonl") && extension != Some("json") {
            continue;
        }
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        let modified = meta.modified().ok();
        let size = meta.len();
        let (cwd, title, updated) = if let Some(cached) = cache
            .jsonl
            .get(&path)
            .filter(|entry| entry.modified == modified && entry.size == size)
        {
            (cached.cwd.clone(), cached.title.clone(), cached.modified)
        } else {
            let Some((cwd, title, updated)) = read_session(&path, client) else {
                continue;
            };
            cache.jsonl.insert(
                path.clone(),
                CachedSessionHeader {
                    modified: updated,
                    size,
                    cwd: cwd.clone(),
                    title: title.clone(),
                },
            );
            (cwd, title, updated)
        };
        let canonical = canonical_path(&cwd);
        let replace = latest_files
            .get(&client)
            .and_then(|(_, _, _, old)| *old)
            .zip(updated)
            .is_none_or(|(old, new)| new >= old);
        if replace {
            latest_files.insert(client, (path.clone(), canonical.clone(), title.clone(), updated));
        }
        let project = projects.entry(canonical.clone()).or_insert_with(|| Project {
            path: canonical,
            sessions: 0,
            clients: BTreeMap::new(),
            updated: None,
            last_title: None,
        });
        project.sessions += 1;
        *project.clients.entry(client).or_insert(0) += 1;
        project.updated = match (project.updated, updated) {
            (Some(old), Some(new)) => Some(old.max(new)),
            (old, new) => old.or(new),
        };
        if project.last_title.is_none() {
            project.last_title = title;
        }
    }
}

fn read_tail_status(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    for window in [256 * 1024_u64, 1024 * 1024, 4 * 1024 * 1024] {
        let start = length.saturating_sub(window);
        file.seek(SeekFrom::Start(start)).ok()?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).ok()?;
        let text = String::from_utf8_lossy(&bytes);
        let mut lines: Vec<&str> = text.lines().collect();
        if start > 0 && lines.len() > 1 {
            lines.remove(0);
        }
        for line in lines.iter().rev() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(value) = serde_json::from_str::<Value>(line)
                && let Some(status) = status_from_value(&value)
            {
                return Some(status);
            }
        }
        if let Ok(value) = serde_json::from_str::<Value>(text.trim())
            && let Some(status) = status_from_value(&value)
        {
            return Some(status);
        }
        if start == 0 {
            break;
        }
    }
    None
}

fn status_from_value(value: &Value) -> Option<String> {
    if let Value::Array(items) = value {
        return items.iter().rev().find_map(status_from_value);
    }
    if let Some(status) = value.get("status").and_then(Value::as_str)
        && let Some(normalized) = normalize_status(status)
    {
        return Some(normalized);
    }
    let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "error" | "failure" => return Some("error".to_string()),
        "interrupt" | "interrupted" | "aborted" | "cancelled" | "canceled" => {
            return Some("interrupted".to_string());
        }
        "closed" | "exit" | "exited" => return Some("closed".to_string()),
        "tool" | "tool_call" | "tool_use" | "function_call" => return Some("tool".to_string()),
        "session" | "session_meta" | "turn_context" => return Some("saved".to_string()),
        "assistant" => {
            if let Some(stop) = value
                .pointer("/message/stop_reason")
                .and_then(Value::as_str)
                .or_else(|| value.get("stop_reason").and_then(Value::as_str))
            {
                match stop {
                    "tool_use" | "tool_calls" => return Some("tool".to_string()),
                    "end_turn" | "stop" | "stop_sequence" | "max_tokens" => {
                        return Some("ready".to_string());
                    }
                    _ => {}
                }
            }
            return Some("ready".to_string());
        }
        "user" => return Some("prompt".to_string()),
        "event_msg" => {
            if let Some(payload) = value.get("payload") {
                let payload_type = payload.get("type").and_then(Value::as_str).unwrap_or("");
                match payload_type {
                    "agent_message" | "task_complete" => return Some("ready".to_string()),
                    "turn_aborted" => return Some("interrupted".to_string()),
                    "error" => return Some("error".to_string()),
                    "agent_reasoning" => return Some("thinking".to_string()),
                    "exec_command_begin" | "exec_command_end" | "mcp_tool_call_begin" | "mcp_tool_call_end" => {
                        return Some("tool".to_string());
                    }
                    "user_message" => return Some("prompt".to_string()),
                    _ => {}
                }
            }
        }
        _ => {}
    }
    if let Some(role) = value
        .pointer("/message/role")
        .and_then(Value::as_str)
        .or_else(|| value.get("role").and_then(Value::as_str))
    {
        match role {
            "assistant" => return Some("ready".to_string()),
            "user" => return Some("prompt".to_string()),
            "tool" => return Some("tool".to_string()),
            _ => {}
        }
    }
    if value
        .pointer("/message/content")
        .or_else(|| value.get("content"))
        .is_some_and(content_has_tool)
    {
        return Some("tool".to_string());
    }
    None
}

fn normalize_status(status: &str) -> Option<String> {
    let value = status.trim().to_ascii_lowercase();
    let known = match value.as_str() {
        "ready" | "completed" | "complete" | "done" | "success" => "ready",
        "error" | "failed" | "failure" => "error",
        "interrupted" | "aborted" | "cancelled" | "canceled" => "interrupted",
        "awaiting" | "waiting" | "question" | "input" => "awaiting",
        "thinking" | "reasoning" => "thinking",
        "tool" | "tool_call" | "tool_use" => "tool",
        "closed" | "exit" | "exited" => "closed",
        "saved" | "session" => "saved",
        _ => return (!value.is_empty()).then_some(value),
    };
    Some(known.to_string())
}

fn content_has_tool(content: &Value) -> bool {
    match content {
        Value::Array(items) => items.iter().any(|item| {
            matches!(
                item.get("type").and_then(Value::as_str),
                Some("tool_use" | "tool_call" | "tool_result" | "function_call")
            )
        }),
        Value::Object(map) => matches!(
            map.get("type").and_then(Value::as_str),
            Some("tool_use" | "tool_call" | "tool_result" | "function_call")
        ),
        _ => false,
    }
}

fn non_empty_str(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

/// Reads project `cwd` and optional title using known top-level paths per CLI
/// so tool call inputs/names never create phantom projects or titles.
fn read_session(path: &Path, client: CliKind) -> Option<(PathBuf, Option<String>, Option<SystemTime>)> {
    let file = File::open(path).ok()?;
    let updated = file.metadata().ok().and_then(|meta| meta.modified().ok());
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    let mut cwd: Option<String> = None;
    let mut title: Option<String> = None;

    for _ in 0..160 {
        line.clear();
        if reader.read_line(&mut line).ok()? == 0 {
            break;
        }
        let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
        match client {
            CliKind::Omp | CliKind::Pi => {
                if kind == "session" {
                    cwd = cwd.or_else(|| non_empty_str(value.get("cwd")).map(str::to_string));
                    title = title.or_else(|| non_empty_str(value.get("title")).map(str::to_string));
                } else if matches!(kind, "title" | "title_change" | "session_info") {
                    title = non_empty_str(value.get("title"))
                        .or_else(|| non_empty_str(value.get("name")))
                        .map(str::to_string)
                        .or(title);
                } else if cwd.is_none() {
                    cwd = non_empty_str(value.get("cwd")).map(str::to_string);
                }
            }
            CliKind::Claude => {
                if cwd.is_none() {
                    cwd = non_empty_str(value.get("cwd")).map(str::to_string);
                }
                if matches!(kind, "summary" | "ai-title" | "custom-title" | "title") || value.get("summary").is_some() {
                    title = non_empty_str(value.get("summary"))
                        .or_else(|| non_empty_str(value.get("aiTitle")))
                        .or_else(|| non_empty_str(value.get("customTitle")))
                        .or_else(|| non_empty_str(value.get("title")))
                        .map(str::to_string)
                        .or(title);
                }
            }
            CliKind::Codex => {
                if kind == "session_meta" {
                    let payload = value.get("payload").unwrap_or(&value);
                    cwd = cwd
                        .or_else(|| non_empty_str(payload.get("cwd")).map(str::to_string))
                        .or_else(|| non_empty_str(value.get("cwd")).map(str::to_string));
                    title = title
                        .or_else(|| non_empty_str(payload.get("title")).map(str::to_string))
                        .or_else(|| non_empty_str(value.get("title")).map(str::to_string));
                } else if cwd.is_none() {
                    cwd = non_empty_str(value.get("cwd")).map(str::to_string);
                }
            }
            _ => {}
        }
        if cwd.is_some() && title.is_some() {
            break;
        }
    }
    cwd.map(|cwd| (PathBuf::from(cwd), title, updated))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_parse_without_panicking() {
        let epoch = |text: &str| parse_sqlite_time(text).map(|time| time.duration_since(UNIX_EPOCH).unwrap().as_secs());
        assert_eq!(epoch("2023-11-14 22:13:20"), Some(1_700_000_000));
        assert_eq!(epoch("2023-11-14 22:13:20.123456 UTC"), Some(1_700_000_000));
        assert_eq!(
            parse_iso_timestamp("2023-11-14T22:13:20.000Z"),
            parse_sqlite_time("2023-11-14 22:13:20")
        );
        for bad in [
            "",
            "garbage",
            "2023-13-01 00:00:00",
            "999999999999999999-01-01 00:00:00",
            "2023-01-01 99:00:00",
        ] {
            assert_eq!(parse_sqlite_time(bad), None, "{bad}");
        }
    }

    #[test]
    fn ignores_tool_call_nested_cwd_and_name() {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("ais-shell-phantom-{}-{stamp}", std::process::id()));
        let home = root.join("home");
        let real_project = root.join("real");
        let fake_tool_cwd = root.join("fake_tool_cwd");
        fs::create_dir_all(&real_project).unwrap();
        fs::create_dir_all(&fake_tool_cwd).unwrap();
        let claude = home.join(".claude/projects/p");
        fs::create_dir_all(&claude).unwrap();
        fs::write(
            claude.join("s.jsonl"),
            format!(
                "{}\n{}\n",
                serde_json::json!({
                    "type": "assistant",
                    "message": {
                        "role": "assistant",
                        "content": [{"type": "tool_use", "name": "bash", "input": {"cwd": fake_tool_cwd}}]
                    }
                }),
                serde_json::json!({"cwd": real_project, "summary": "Real title"})
            ),
        )
        .unwrap();
        let scan = scan_projects(&home);
        assert_eq!(scan.projects.len(), 1);
        assert_eq!(scan.projects[0].path, canonical_path(&real_project));
        assert_eq!(scan.projects[0].last_title.as_deref(), Some("Real title"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn scans_omp_and_claude_projects() {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("ais-shell-scan-{}-{stamp}", std::process::id()));
        let home = root.join("home");
        let first = root.join("project one");
        let second = root.join("project two");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        let omp = home.join(".omp/agent/sessions/project");
        let claude = home.join(".claude/projects/project");
        fs::create_dir_all(&omp).unwrap();
        fs::create_dir_all(&claude).unwrap();
        fs::write(
            omp.join("one.jsonl"),
            format!(
                "{}\n{}\n",
                serde_json::json!({"type": "session", "cwd": first, "title": "First"}),
                serde_json::json!({"type": "message", "message": {"role": "assistant"}})
            ),
        )
        .unwrap();
        fs::write(
            claude.join("two.jsonl"),
            format!(
                "{}\n{}\n",
                serde_json::json!({"cwd": second, "summary": "Second"}),
                serde_json::json!({"type": "assistant", "message": {"role": "assistant", "stop_reason": "end_turn"}})
            ),
        )
        .unwrap();
        let scan = scan_projects(&home);
        assert_eq!(scan.projects.len(), 2);
        let one = scan
            .projects
            .iter()
            .find(|project| project.path == canonical_path(&first))
            .expect("OMP project");
        assert_eq!(one.sessions, 1);
        assert_eq!(one.clients.get(&CliKind::Omp), Some(&1));
        let two = scan
            .projects
            .iter()
            .find(|project| project.path == canonical_path(&second))
            .expect("Claude project");
        assert_eq!(two.clients.get(&CliKind::Claude), Some(&1));
        assert_eq!(
            scan.latest.get(&CliKind::Omp).map(|snap| snap.status.as_str()),
            Some("ready")
        );
        assert_eq!(
            scan.latest.get(&CliKind::Claude).map(|snap| snap.status.as_str()),
            Some("ready")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn scans_agy_sqlite_status_and_workspace() {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("ais-shell-agy-{}-{stamp}", std::process::id()));
        let home = root.join("home");
        let workspace = root.join("agy project");
        fs::create_dir_all(&workspace).unwrap();
        let folder = home.join(".gemini/antigravity-cli");
        fs::create_dir_all(&folder).unwrap();
        let database = folder.join("conversation_summaries.db");
        {
            let connection = Connection::open(&database).unwrap();
            connection
                .execute(
                    "CREATE TABLE conversation_summaries (
                        title TEXT,
                        status TEXT,
                        workspace_uris TEXT,
                        last_modified_time TEXT
                    )",
                    [],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO conversation_summaries VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![
                        "Agy task",
                        "CASCADE_RUN_STATUS_IDLE",
                        format!("[\"file://{}\"]", workspace.display()),
                        "2026-11-16 00:00:00+00:00"
                    ],
                )
                .unwrap();
        }
        let scan = scan_projects(&home);
        let project = scan
            .projects
            .iter()
            .find(|project| project.path == canonical_path(&workspace))
            .expect("agy project");
        assert_eq!(project.clients.get(&CliKind::Agy), Some(&1));
        let snapshot = scan.latest.get(&CliKind::Agy).expect("agy snapshot");
        assert_eq!(snapshot.status, "ready");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn scans_gemini_mapping_and_last_message_status() {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("ais-shell-gemini-{}-{stamp}", std::process::id()));
        let home = root.join("home");
        let workspace = root.join("gemini project");
        fs::create_dir_all(&workspace).unwrap();
        let cwd = workspace.display().to_string();
        let hash = format!("{:x}", Sha256::digest(cwd.as_bytes()));
        fs::create_dir_all(home.join(".gemini")).unwrap();
        fs::write(
            home.join(".gemini/projects.json"),
            serde_json::json!({"projects": {cwd: "gemini-project"}}).to_string(),
        )
        .unwrap();
        let chats = home.join(format!(".gemini/tmp/{hash}/chats"));
        fs::create_dir_all(&chats).unwrap();
        fs::write(
            chats.join("session-test.json"),
            serde_json::json!({
                "projectHash": hash,
                "lastUpdated": "2026-11-16T00:10:00Z",
                "messages": [
                    {"type": "user", "content": "hello"},
                    {"type": "gemini", "content": "done"}
                ]
            })
            .to_string(),
        )
        .unwrap();
        let scan = scan_projects(&home);
        let project = scan
            .projects
            .iter()
            .find(|project| project.path == canonical_path(&workspace))
            .expect("Gemini project");
        assert_eq!(project.clients.get(&CliKind::Gemini), Some(&1));
        let snapshot = scan.latest.get(&CliKind::Gemini).expect("Gemini snapshot");
        assert_eq!(snapshot.status, "ready");
        let _ = fs::remove_dir_all(root);
    }
}
