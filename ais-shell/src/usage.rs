//! Provider plan limits (5-hour / weekly windows) read through
//! `ais --usage --json`. The cached snapshot is shown immediately; a live
//! request through the official CLIs runs on demand or when opted in.

use crate::cli::{CliKind, unix_time};
use crate::log;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

const CACHE_TIMEOUT: Duration = Duration::from_secs(15);
const LIVE_TIMEOUT: Duration = Duration::from_secs(45);
/// After this age, a live report may be superseded by a newer cached snapshot.
pub const STALE_LIVE_AGE: Duration = Duration::from_secs(30 * 60);

#[derive(Clone, Debug)]
pub struct Limit {
    pub client: Option<CliKind>,
    pub label: String,
    pub window: String,
    /// Fraction used (0..1); `None` when the provider did not report it.
    pub used: Option<f32>,
    pub reset: Option<SystemTime>,
    pub reset_label: String,
    pub updated: Option<SystemTime>,
    pub source: String,
    pub status: String,
    /// Stable account key from ais ("#ab12cd" or "CLI").
    pub account: String,
    /// "email · organization · plan" when the official CLI reported it.
    pub account_title: String,
}

impl Limit {
    fn from_json(value: &Value) -> Option<Self> {
        let text = |key: &str| value.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
        let time = |key: &str| value.get(key).and_then(Value::as_f64).and_then(unix_time);
        let source = text("source");
        let provider = text("provider");
        let lower = format!("{provider} {source}").to_lowercase();
        let client = if lower.contains("claude") || lower.contains("anthropic") {
            Some(CliKind::Claude)
        } else if lower.contains("codex") || lower.contains("openai") {
            Some(CliKind::Codex)
        } else if lower.contains("agy") || lower.contains("antigravity") {
            Some(CliKind::Agy)
        } else if lower.contains("gemini") {
            Some(CliKind::Gemini)
        } else {
            None
        };
        let used = value
            .get("used")
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite())
            .map(|used| (used as f32).clamp(0.0, 1.0));
        Some(Self {
            client,
            label: text("label"),
            window: text("window"),
            used,
            reset: time("reset"),
            reset_label: text("reset_label"),
            updated: time("updated"),
            source,
            status: text("status"),
            account: text("account"),
            account_title: String::new(),
        })
    }

    pub fn clamped_used(&self) -> Option<f32> {
        self.used.map(|u| if u.is_finite() { u.clamp(0.0, 1.0) } else { 0.0 })
    }

    pub fn is_reset_passed(&self) -> bool {
        self.reset
            .is_some_and(|reset| reset.duration_since(SystemTime::now()).is_err())
    }

    /// "resets in 3h 12m", the provider's own text, or nothing.
    pub fn reset_text(&self) -> String {
        if let Some(reset) = self.reset {
            return match reset.duration_since(SystemTime::now()) {
                Ok(left) => format!("resets in {}", human_duration(left)),
                Err(_) => "window reset — refresh".into(),
            };
        }
        // "Resets 11pm (Europe/Berlin)" → "resets 11pm": the zone is the
        // user's own and only makes the line wrap.
        let label = match self.reset_label.rfind(" (") {
            Some(at) if self.reset_label.ends_with(')') => &self.reset_label[..at],
            _ => self.reset_label.as_str(),
        };
        label
            .strip_prefix("Resets ")
            .map(|rest| format!("resets {rest}"))
            .unwrap_or_else(|| label.to_string())
    }
}

pub struct Headline<'a> {
    pub short: Option<&'a Limit>,
    pub week: Option<&'a Limit>,
}

pub fn human_duration(duration: Duration) -> String {
    let minutes = duration.as_secs() / 60;
    let (days, hours, mins) = (minutes / 1440, (minutes % 1440) / 60, minutes % 60);
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {mins:02}m")
    } else {
        format!("{mins}m")
    }
}

#[derive(Clone, Debug, Default)]
pub struct UsageReport {
    pub limits: Vec<Limit>,
    pub errors: Vec<String>,
    pub live: bool,
}

pub fn parse(bytes: &[u8]) -> Result<UsageReport, String> {
    let value: Value = serde_json::from_slice(bytes).map_err(|error| format!("Unexpected usage output: {error}"))?;
    let limits_array = value
        .get("limits")
        .and_then(Value::as_array)
        .ok_or_else(|| "Unexpected usage output: missing 'limits' array".to_string())?;
    let mut limits: Vec<Limit> = limits_array.iter().filter_map(Limit::from_json).collect();
    let titles: Vec<(String, String, String)> = value
        .get("accounts")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let text = |key: &str| item.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
                    let title = [text("email"), text("name"), text("plan")]
                        .into_iter()
                        .filter(|part| !part.is_empty())
                        .collect::<Vec<_>>()
                        .join(" · ");
                    (!title.is_empty()).then(|| (text("provider"), text("key"), title))
                })
                .collect()
        })
        .unwrap_or_default();
    for limit in &mut limits {
        if let Some((_, _, title)) = titles.iter().find(|(provider, key, _)| {
            *key == limit.account && limit.source.to_lowercase().contains(&provider_cli(provider))
        }) {
            limit.account_title = title.clone();
        }
    }
    let errors = value
        .get("errors")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    Ok(UsageReport {
        limits,
        errors,
        live: value.get("live").and_then(Value::as_bool).unwrap_or(false),
    })
}

pub struct UsageStore {
    ais: Option<PathBuf>,
    pub report: Option<UsageReport>,
    pub live_error: Option<String>,
    pub cache_error: Option<String>,
    pub loading_live: bool,
    pub loading_cache: bool,
    pub loaded_at: Option<Instant>,
    pub live_at: Option<Instant>,
    tx: Sender<(bool, Result<UsageReport, String>)>,
    rx: Receiver<(bool, Result<UsageReport, String>)>,
    waker: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl UsageStore {
    pub fn new(ais: Option<PathBuf>) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            ais,
            report: None,
            live_error: None,
            cache_error: None,
            loading_live: false,
            loading_cache: false,
            loaded_at: None,
            live_at: None,
            tx,
            rx,
            waker: None,
        }
    }

    pub fn set_waker(&mut self, waker: Arc<dyn Fn() + Send + Sync>) {
        self.waker = Some(waker);
    }

    pub fn available(&self) -> bool {
        self.ais.is_some()
    }

    pub fn display_error(&self) -> Option<&str> {
        self.live_error.as_deref().or_else(|| {
            if self.report.is_none() {
                self.cache_error.as_deref()
            } else {
                None
            }
        })
    }

    /// Cached snapshot (fast, no network).
    pub fn load_cached(&mut self) {
        if self.loading_cache || self.ais.is_none() {
            return;
        }
        self.loading_cache = true;
        if !self.spawn(false) {
            self.loading_cache = false;
        }
    }

    /// Live request through the official CLIs (can take up to ~30 s each).
    pub fn refresh_live(&mut self) {
        if self.loading_live {
            return;
        }
        if self.ais.is_none() {
            self.live_error = Some("The ais launcher was not found (set AIS_BIN or add ais to PATH)".into());
            return;
        }
        self.loading_live = true;
        if !self.spawn(true) {
            self.loading_live = false;
            self.live_error = Some("Failed to spawn background thread for usage refresh".into());
        }
    }

    fn spawn(&self, live: bool) -> bool {
        let Some(ais) = self.ais.clone() else {
            return false;
        };
        let tx = self.tx.clone();
        let waker = self.waker.clone();
        let name = if live { "ais-usage-live" } else { "ais-usage-cache" };
        match thread::Builder::new().name(name.into()).spawn(move || {
            let result = run(&ais, live);
            let _ = tx.send((live, result));
            if let Some(waker) = waker {
                waker();
            }
        }) {
            Ok(_) => true,
            Err(error) => {
                log::error(&format!("Usage thread spawn failed: {error}"));
                false
            }
        }
    }

    pub fn poll(&mut self) {
        while let Ok((live, result)) = self.rx.try_recv() {
            if live {
                self.loading_live = false;
                self.live_at = Some(Instant::now());
            } else {
                self.loading_cache = false;
            }
            self.loaded_at = Some(Instant::now());
            match result {
                Ok(report) => {
                    let replace_live = self.report.as_ref().is_none_or(|current| !current.live)
                        || self.live_at.is_some_and(|at| at.elapsed() > STALE_LIVE_AGE);
                    if live || replace_live {
                        self.report = Some(report);
                    }
                    if live {
                        self.live_error = None;
                        self.cache_error = None;
                    } else {
                        self.cache_error = None;
                    }
                }
                Err(error) => {
                    if live {
                        self.live_error = Some(error);
                    } else {
                        self.cache_error = Some(error);
                    }
                }
            }
        }
    }

    pub fn limits_for(&self, kind: CliKind) -> Vec<&Limit> {
        self.report
            .as_ref()
            .map(|report| {
                report
                    .limits
                    .iter()
                    .filter(|limit| limit.client == Some(kind))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The most constrained window per CLI, for compact displays.
    pub fn headline(&self, kind: CliKind) -> Option<Headline<'_>> {
        let limits = self.limits_for(kind);
        let pick = |window: &str| {
            limits
                .iter()
                .filter(|limit| limit.window == window && limit.used.is_some())
                .max_by(|a, b| a.used.partial_cmp(&b.used).unwrap_or(std::cmp::Ordering::Equal))
                .copied()
        };
        let short = pick("5h");
        let week = pick("week");
        (short.is_some() || week.is_some()).then_some(Headline { short, week })
    }
}

fn provider_cli(provider: &str) -> String {
    match provider {
        "anthropic" => "claude",
        "openai-codex" => "codex",
        "google-antigravity" => "agy",
        other => other,
    }
    .to_string()
}

pub fn wait_with_timeout(mut child: Child, timeout: Duration, label: &str) -> Result<std::process::Output, String> {
    use std::io::Read;
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdout_handle = stdout_pipe.and_then(|mut out| {
        thread::Builder::new()
            .name("ais-proc-stdout".into())
            .spawn(move || {
                let mut buf = Vec::new();
                let _ = out.read_to_end(&mut buf);
                buf
            })
            .ok()
    });
    let stderr_handle = stderr_pipe.and_then(|mut err| {
        thread::Builder::new()
            .name("ais-proc-stderr".into())
            .spawn(move || {
                let mut buf = Vec::new();
                let _ = err.read_to_end(&mut buf);
                buf
            })
            .ok()
    });

    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() < timeout => thread::sleep(Duration::from_millis(30)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{label} timed out after {}s", timeout.as_secs()));
            }
            Err(error) => return Err(format!("{label} failed: {error}")),
        }
    };
    let stdout = stdout_handle.and_then(|h| h.join().ok()).unwrap_or_default();
    let stderr = stderr_handle.and_then(|h| h.join().ok()).unwrap_or_default();
    Ok(std::process::Output { status, stdout, stderr })
}

fn run(ais: &Path, live: bool) -> Result<UsageReport, String> {
    let mut command = Command::new(ais);
    command.args(["--usage", "--json"]);
    if !live {
        command.arg("--cached-usage");
    }
    let child = command
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("Cannot run ais: {error}"))?;
    let timeout = if live { LIVE_TIMEOUT } else { CACHE_TIMEOUT };
    let output = wait_with_timeout(child, timeout, "ais --usage")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let msg = stderr.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("").trim();
        if !msg.is_empty() {
            return Err(format!("ais --usage failed: {msg}"));
        }
        return Err(format!("ais --usage failed (exit {})", output.status));
    }
    parse(&output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_limits_and_maps_clients() {
        let json = r#"{"limits":[
            {"provider":"anthropic","label":"Claude","window":"5h","used":0.17,"reset":0,"updated":1794845823.1,"source":"Claude CLI · cached","status":"","reset_label":"Resets 11pm (Europe/Berlin)"},
            {"provider":"openai-codex","label":"codex","window":"week","used":0.79,"reset":4102444800,"updated":1794845819.7,"source":"Codex CLI · live","status":""},
            {"provider":"google","label":"Gemini Models","window":"5h","used":null,"source":"agy CLI · cached","status":"unavailable"}
        ],"errors":["x failed"],"live":true}"#.as_bytes();
        let report = parse(json).expect("report");
        assert_eq!(report.limits.len(), 3);
        assert_eq!(report.limits[0].client, Some(CliKind::Claude));
        assert_eq!(report.limits[1].client, Some(CliKind::Codex));
        assert_eq!(report.limits[2].client, Some(CliKind::Agy));
        assert_eq!(report.limits[2].used, None);
        assert_eq!(report.limits[0].reset_text(), "resets 11pm");
        assert!(report.limits[1].reset_text().starts_with("resets in"));
        assert_eq!(report.errors, vec!["x failed"]);
    }

    #[test]
    fn rejects_missing_limits_and_clamps_used() {
        assert!(parse(br#"{"ok":true}"#).is_err());
        let json = br#"{"limits":[{"provider":"anthropic","label":"Claude","window":"5h","used":17.5}]}"#;
        let report = parse(json).expect("clamped report");
        assert_eq!(report.limits[0].clamped_used(), Some(1.0));
    }

    #[test]
    fn accounts_label_each_limit_group() {
        let json = r##"{"accounts":[{"provider":"anthropic","key":"#a","email":"a@example.com","name":"","plan":"Team"},
            {"provider":"anthropic","key":"#b","email":"b@example.com","name":"","plan":"Max"}],
            "limits":[{"provider":"anthropic","account":"#a","label":"Claude","window":"5h","used":0.1,"source":"Claude CLI · live"},
            {"provider":"anthropic","account":"#b","label":"Claude","window":"5h","used":0.7,"source":"Claude CLI · live"}]}"##;
        let report = parse(json.as_bytes()).expect("report");
        assert_eq!(report.limits[0].account_title, "a@example.com · Team");
        assert_eq!(report.limits[1].account_title, "b@example.com · Max");
        assert_ne!(report.limits[0].account, report.limits[1].account);
    }

    #[test]
    fn headline_keeps_5h_and_week_distinct() {
        let mut store = UsageStore::new(None);
        store.report = Some(UsageReport {
            limits: vec![Limit {
                client: Some(CliKind::Claude),
                label: "Claude".into(),
                window: "5h".into(),
                used: Some(0.25),
                reset: None,
                reset_label: String::new(),
                updated: None,
                source: String::new(),
                status: String::new(),
                account: String::new(),
                account_title: String::new(),
            }],
            errors: Vec::new(),
            live: false,
        });
        let headline = store.headline(CliKind::Claude).expect("headline");
        assert!(headline.short.is_some());
        assert!(headline.week.is_none());
    }

    #[test]
    fn durations_are_compact() {
        assert_eq!(human_duration(Duration::from_secs(59 * 60)), "59m");
        assert_eq!(human_duration(Duration::from_secs(3 * 3600 + 5 * 60)), "3h 05m");
        assert_eq!(human_duration(Duration::from_secs(2 * 86400 + 3600)), "2d 1h");
    }
}
