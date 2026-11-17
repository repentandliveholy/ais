//! Per-project conversation lists, read through the `ais` indexer
//! (`ais --json -C <project>`), and the argv that resumes each of them.

use crate::cli::{CliKind, unix_time, which};
use crate::log;
use crate::terminal::LaunchSpec;
use crate::usage::wait_with_timeout;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

const SESSIONS_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Debug)]
pub struct Session {
    pub client: CliKind,
    pub id: String,
    pub cwd: PathBuf,
    pub path: String,
    pub title: String,
    pub updated: Option<SystemTime>,
    pub status: String,
    pub model: String,
    pub profile: String,
    pub project_id: String,
    pub messages: Option<u64>,
    pub last_user: String,
    pub resume_supported: bool,
}

impl Session {
    fn from_json(value: &Value) -> Option<Self> {
        let text = |key: &str| value.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
        let client = CliKind::from_id(value.get("client")?.as_str()?)?;
        let updated = value.get("updated").and_then(Value::as_f64).and_then(unix_time);
        let mut title = text("title").replace(['\n', '\r'], " ");
        if title.trim().is_empty() {
            title = "(untitled)".to_string();
        }
        Some(Self {
            client,
            id: text("id"),
            cwd: PathBuf::from(text("cwd")),
            path: text("path"),
            title,
            updated,
            status: text("status"),
            model: text("model"),
            profile: text("profile"),
            project_id: text("project_id"),
            messages: value.get("messages").and_then(Value::as_u64),
            last_user: text("last_user"),
            resume_supported: value.get("resume_supported").and_then(Value::as_bool).unwrap_or(false),
        })
    }

    pub fn short_id(&self) -> &str {
        let end = self
            .id
            .char_indices()
            .nth(8)
            .map(|(index, _)| index)
            .unwrap_or(self.id.len());
        &self.id[..end]
    }

    /// The argv that resumes this conversation in its own client.
    pub fn resume_spec(
        &self,
        fallback_cwd: &Path,
        ais: Option<&Path>,
        custom_command: Option<&str>,
        extra_args: &[String],
    ) -> Result<LaunchSpec, String> {
        if !self.resume_supported {
            return Err(format!("{} cannot resume this session", self.client.label()));
        }
        let cwd = if self.cwd.is_dir() {
            self.cwd.clone()
        } else {
            fallback_cwd.to_path_buf()
        };
        let label = format!("{} · {}", self.client.label(), truncate(&self.title, 40));
        let cli_bin = || {
            custom_command
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| self.client.command())
                .to_string()
        };
        let (program, mut args): (String, Vec<String>) = match self.client {
            // OMP profiles also need OMP_PROFILE in the environment; the ais
            // launcher handles that when no custom command or extra args are set.
            CliKind::Omp => {
                match ais.filter(|_| custom_command.is_none_or(|c| c.trim().is_empty()) && extra_args.is_empty()) {
                    Some(ais) => (
                        ais.to_string_lossy().into_owned(),
                        vec![
                            "--resume".into(),
                            self.id.clone(),
                            "--client".into(),
                            "omp".into(),
                            "--no-return".into(),
                            "--same-tab".into(),
                        ],
                    ),
                    None => {
                        let mut args = Vec::new();
                        if !self.profile.is_empty() {
                            args.extend(["--profile".to_string(), self.profile.clone()]);
                        }
                        args.extend([
                            "--resume".to_string(),
                            if self.path.is_empty() {
                                self.id.clone()
                            } else {
                                self.path.clone()
                            },
                        ]);
                        (cli_bin(), args)
                    }
                }
            }
            CliKind::Pi => (
                cli_bin(),
                vec![
                    "--session".into(),
                    if self.path.is_empty() {
                        self.id.clone()
                    } else {
                        self.path.clone()
                    },
                ],
            ),
            CliKind::Claude => (cli_bin(), vec!["--resume".into(), self.id.clone()]),
            CliKind::Codex => {
                let mut args = vec!["resume".to_string(), self.id.clone()];
                if self.cwd.is_dir() {
                    args.extend(["--cd".to_string(), self.cwd.to_string_lossy().into_owned()]);
                }
                (cli_bin(), args)
            }
            CliKind::Agy => {
                let mut args = Vec::new();
                if !self.project_id.is_empty() {
                    args.extend(["--project".to_string(), self.project_id.clone()]);
                }
                args.extend(["--conversation".to_string(), self.id.clone()]);
                (cli_bin(), args)
            }
            CliKind::Gemini => (cli_bin(), vec!["--resume".into(), self.id.clone()]),
            CliKind::Shell => return Err("Not a conversation".into()),
        };
        if !(self.client == CliKind::Omp && ais.is_some_and(|a| program == a.to_string_lossy())) {
            args.extend(extra_args.iter().cloned());
        }
        Ok(LaunchSpec {
            client: self.client,
            program,
            args,
            cwd,
            label,
        })
    }

    pub fn command_line(&self, fallback_cwd: &Path, ais: Option<&Path>) -> Option<String> {
        let spec = self.resume_spec(fallback_cwd, ais, None, &[]).ok()?;
        let mut parts = vec![crate::input::shell_quote(&spec.program)];
        parts.extend(spec.args.iter().map(|arg| crate::input::shell_quote(arg)));
        Some(format!(
            "cd {} && {}",
            crate::input::shell_quote(&spec.cwd.to_string_lossy()),
            parts.join(" ")
        ))
    }
}

pub fn truncate(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Locates the Python `ais` launcher: `$AIS_BIN`, then `PATH`, then beside the
/// executable (or bundle Resources), and in debug builds the sibling repo copy.
pub fn find_ais() -> Option<PathBuf> {
    if let Some(path) = env::var_os("AIS_BIN").map(PathBuf::from)
        && path.is_file()
    {
        return Some(path);
    }
    if let Some(path) = which("ais") {
        return Some(path);
    }
    if let Ok(exe) = env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let beside = dir.join("ais");
        if beside.is_file() {
            return Some(beside);
        }
        let resources = dir.join("../Resources/ais");
        if resources.is_file() {
            return Some(resources.canonicalize().unwrap_or(resources));
        }
    }
    #[cfg(debug_assertions)]
    {
        let sibling = Path::new(env!("CARGO_MANIFEST_DIR")).join("../ais");
        if sibling.is_file() {
            return Some(sibling.canonicalize().unwrap_or(sibling));
        }
    }
    None
}

#[derive(Default)]
pub struct SessionList {
    pub sessions: Vec<Session>,
    pub loading: bool,
    pub error: Option<String>,
    pub loaded_at: Option<Instant>,
}

pub struct SessionStore {
    ais: Option<PathBuf>,
    lists: HashMap<PathBuf, SessionList>,
    tx: Sender<(PathBuf, Result<Vec<Session>, String>)>,
    rx: Receiver<(PathBuf, Result<Vec<Session>, String>)>,
    waker: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Background preload queue (projects nobody is looking at yet).
    queue: VecDeque<PathBuf>,
    running: usize,
}

/// How many `ais --json` indexers may run at once while preloading.
const PRELOAD_PARALLEL: usize = 2;
/// A preloaded list counts as fresh for this long.
pub const PRELOAD_MAX_AGE: Duration = Duration::from_secs(120);

impl SessionStore {
    pub fn new(ais: Option<PathBuf>) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            ais,
            lists: HashMap::new(),
            tx,
            rx,
            waker: None,
            queue: VecDeque::new(),
            running: 0,
        }
    }

    pub fn ais(&self) -> Option<&Path> {
        self.ais.as_deref()
    }

    pub fn set_waker(&mut self, waker: Arc<dyn Fn() + Send + Sync>) {
        self.waker = Some(waker);
    }

    pub fn get(&self, project: &Path) -> Option<&SessionList> {
        self.lists.get(project)
    }

    /// Loads the list if it was never loaded or is older than `max_age`.
    pub fn ensure(&mut self, project: &Path, max_age: Duration) {
        let stale = self
            .lists
            .get(project)
            .is_none_or(|list| !list.loading && list.loaded_at.is_none_or(|at| at.elapsed() > max_age));
        if stale {
            self.refresh(project);
        }
    }

    /// Queues projects for background loading; already fresh, loading or
    /// queued ones are skipped. The queue is drained by `poll`, a few at a time.
    pub fn preload<'a>(&mut self, projects: impl IntoIterator<Item = &'a Path>) {
        if self.ais.is_none() {
            return;
        }
        for project in projects {
            if self.is_fresh_or_loading(project, PRELOAD_MAX_AGE) || self.queue.iter().any(|queued| queued == project) {
                continue;
            }
            self.queue.push_back(project.to_path_buf());
        }
        self.pump();
    }

    fn is_fresh_or_loading(&self, project: &Path, max_age: Duration) -> bool {
        self.lists
            .get(project)
            .is_some_and(|list| list.loading || list.loaded_at.is_some_and(|at| at.elapsed() <= max_age))
    }

    fn pump(&mut self) {
        while self.running < PRELOAD_PARALLEL {
            let Some(project) = self.queue.pop_front() else {
                break;
            };
            if !self.is_fresh_or_loading(&project, PRELOAD_MAX_AGE) {
                self.refresh(&project);
            }
        }
    }

    pub fn refresh(&mut self, project: &Path) {
        let entry = self.lists.entry(project.to_path_buf()).or_default();
        if entry.loading {
            return;
        }
        let Some(ais) = self.ais.clone() else {
            entry.error = Some("The ais launcher was not found (set AIS_BIN or add ais to PATH)".into());
            entry.loaded_at = Some(Instant::now());
            return;
        };
        entry.loading = true;
        self.running += 1;
        let tx = self.tx.clone();
        let waker = self.waker.clone();
        let project_buf = project.to_path_buf();
        if let Err(error) = thread::Builder::new().name("ais-sessions".into()).spawn(move || {
            let result = load(&ais, &project_buf);
            let _ = tx.send((project_buf, result));
            if let Some(waker) = waker {
                waker();
            }
        }) {
            log::error(&format!("Session thread spawn failed: {error}"));
            self.running = self.running.saturating_sub(1);
            entry.loading = false;
            entry.error = Some(format!("Cannot spawn session loader: {error}"));
            entry.loaded_at = Some(Instant::now());
        }
    }

    pub fn poll(&mut self) {
        while let Ok((project, result)) = self.rx.try_recv() {
            self.running = self.running.saturating_sub(1);
            let entry = self.lists.entry(project).or_default();
            entry.loading = false;
            entry.loaded_at = Some(Instant::now());
            match result {
                Ok(sessions) => {
                    entry.sessions = sessions;
                    entry.error = None;
                }
                Err(error) => entry.error = Some(error),
            }
        }
        self.pump();
    }
}

fn load(ais: &Path, project: &Path) -> Result<Vec<Session>, String> {
    let child = Command::new(ais)
        .arg("--json")
        .arg("-C")
        .arg(project)
        .current_dir(project)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("Cannot run ais: {error}"))?;
    let output = wait_with_timeout(child, SESSIONS_TIMEOUT, "ais --json")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let line = stderr.lines().last().unwrap_or("unknown error");
        return Err(format!("ais failed: {}", truncate(line, 160)));
    }
    let value: Value =
        serde_json::from_slice(&output.stdout).map_err(|error| format!("Unexpected ais output: {error}"))?;
    let mut sessions: Vec<Session> = value
        .as_array()
        .map(|items| items.iter().filter_map(Session::from_json).collect())
        .unwrap_or_default();
    sessions.sort_by(|left, right| right.updated.cmp(&left.updated));
    Ok(sessions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ais_json_and_builds_resume_argv() {
        let value: Value = serde_json::json!({
            "client": "codex",
            "id": "01a1e030-3300",
            "cwd": "/tmp",
            "title": "fix\nthe build",
            "updated": 1_700_000_000.5,
            "status": "ready",
            "resume_supported": true
        });
        let session = Session::from_json(&value).expect("session");
        assert_eq!(session.title, "fix the build");
        assert_eq!(session.short_id(), "01a1e030");
        let spec = session
            .resume_spec(
                Path::new("/"),
                None,
                Some("/opt/codex"),
                &["--model".into(), "o3".into()],
            )
            .expect("spec");
        assert_eq!(spec.program, "/opt/codex");
        assert_eq!(
            spec.args,
            vec!["resume", "01a1e030-3300", "--cd", "/tmp", "--model", "o3"]
        );
        assert_eq!(spec.cwd, PathBuf::from("/tmp"));
    }

    #[test]
    fn unsupported_sessions_do_not_resume() {
        let value = serde_json::json!({"client": "gemini", "id": "x", "resume_supported": false});
        let session = Session::from_json(&value).expect("session");
        assert!(session.resume_spec(Path::new("/tmp"), None, None, &[]).is_err());
    }
}
