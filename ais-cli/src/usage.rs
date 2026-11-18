//! Provider limits from the official CLIs (and the DeepSeek balance API).

use crate::adapters::Collector;
use crate::claude_usage::{Window, read_claude_usage};
use crate::i18n::{settings_path, tr, write_private};
use crate::model::{now, parse_iso, strftime, which};
use crate::screen::{Part, Screen, part};
use crate::text::{clean, clip, line, line_value, ljust, py_str, rjust, truthy, width, wrap as text_wrap};
use crate::trf;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, mpsc};
use std::time::{Duration, Instant};

pub type Env = BTreeMap<OsString, OsString>;

#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn is_set(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    pub fn set(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[derive(Debug, Clone)]
pub enum UsageError {
    Cancelled,
    Timeout(String),
    /// OS errors may contain paths: only a generic reason is ever shown.
    Os(#[allow(dead_code)] String),
    Value(String),
}

impl UsageError {
    pub fn safe(&self) -> String {
        match self {
            UsageError::Cancelled => String::new(),
            UsageError::Os(_) => tr("Could not launch official CLI"),
            UsageError::Timeout(text) | UsageError::Value(text) => text.clone(),
        }
    }
}

fn value_err(message: &str) -> UsageError {
    UsageError::Value(tr(message))
}

pub fn provider_name(provider: &str) -> &str {
    match provider {
        "anthropic" => "Anthropic / Claude",
        "openai-codex" => "OpenAI / Codex",
        "google-antigravity" => "Google / agy",
        "deepseek" => "DeepSeek API",
        other => other,
    }
}

pub fn cli_source(provider: &str) -> Option<&'static str> {
    match provider {
        "anthropic" => Some("Claude CLI"),
        "openai-codex" => Some("Codex CLI"),
        "google-antigravity" => Some("agy CLI"),
        "deepseek" => Some("DeepSeek API"),
        _ => None,
    }
}

const DEEPSEEK_BALANCE_URL: &str = "https://api.deepseek.com/user/balance";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Limit {
    pub provider: String,
    pub account: String,
    pub label: String,
    pub window: String,
    pub used: Option<f64>,
    pub reset: f64,
    pub updated: f64,
    pub source: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub id: String,
    /// Preserve native human reset text when the CLI does not export an epoch.
    #[serde(default)]
    pub reset_label: String,
}

impl Limit {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: &str,
        account: &str,
        label: &str,
        window: &str,
        used: Option<f64>,
        reset: f64,
        updated: f64,
        source: &str,
    ) -> Self {
        Self {
            provider: provider.into(),
            account: account.into(),
            label: label.into(),
            window: window.into(),
            used,
            reset,
            updated,
            source: source.into(),
            status: String::new(),
            id: String::new(),
            reset_label: String::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Report {
    pub provider: String,
    pub text: String,
    pub updated: f64,
    #[serde(default)]
    pub cached: bool,
}

/// Who a set of limits belongs to, as reported by the official CLI.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Account {
    pub provider: String,
    pub key: String,
    pub email: String,
    pub name: String,
    pub plan: String,
    pub dirs: Vec<String>,
    pub expires: f64,
    pub sessions_5h: i64,
    pub sessions_7d: i64,
    pub credits: String,
    pub warning: String,
}

impl Account {
    pub fn new(provider: &str, key: &str) -> Self {
        Self { provider: provider.into(), key: key.into(), ..Default::default() }
    }

    pub fn title(&self) -> &str {
        if !self.email.is_empty() {
            &self.email
        } else if !self.name.is_empty() {
            &self.name
        } else {
            &self.key
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Usage {
    pub limits: Vec<Limit>,
    pub errors: Vec<String>,
    pub loaded: bool,
    pub live: bool,
    pub reports: Vec<Report>,
    /// Last attempt, separately from the successful snapshot timestamps.
    pub checked: f64,
    pub accounts: Vec<Account>,
}

impl Usage {
    pub fn account(&self, provider: &str, key: &str) -> Option<&Account> {
        self.accounts.iter().find(|a| a.provider == provider && a.key == key)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "accounts": self.accounts,
            "limits": self.limits,
            "errors": self.errors,
            "live": self.live,
            "reports": self.reports,
            "checked": self.checked,
        })
    }
}

// ---------------------------------------------------------------- DeepSeek

pub fn deepseek_key_path() -> PathBuf {
    settings_path().parent().map(|p| p.join("deepseek.key")).unwrap_or_else(|| PathBuf::from("deepseek.key"))
}

/// DEEPSEEK_API_KEY, else the private key file written by --set-deepseek-key.
pub fn deepseek_key() -> String {
    if let Ok(key) = std::env::var("DEEPSEEK_API_KEY")
        && !key.trim().is_empty()
    {
        return key.trim().to_string();
    }
    let path = deepseek_key_path();
    match std::fs::metadata(&path) {
        Ok(meta) if meta.len() <= 4096 => std::fs::read_to_string(&path).map(|t| t.trim().to_string()).unwrap_or_default(),
        _ => String::new(),
    }
}

pub fn save_deepseek_key(key: &str) -> std::io::Result<PathBuf> {
    let path = deepseek_key_path();
    write_private(&path, format!("{}\n", key.trim()).as_bytes())?;
    Ok(path)
}

fn group_thousands(integer: &str) -> String {
    let (sign, digits) = integer.strip_prefix('-').map(|d| ("-", d)).unwrap_or(("", integer));
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    format!("{sign}{out}")
}

pub fn money(amount: &Value, currency: &str) -> String {
    let parsed = match amount {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    };
    let value = match parsed {
        Some(v) if v.is_finite() => {
            let text = format!("{v:.2}");
            let (integer, fraction) = text.split_once('.').unwrap_or((&text, "00"));
            format!("{}.{fraction}", group_thousands(integer))
        }
        _ => line(&py_str(amount), 20),
    };
    match currency {
        "CNY" => format!("¥{value}"),
        "USD" => format!("${value}"),
        _ => format!("{value} {}", line(currency, 8)),
    }
}

/// Documented /user/balance body: is_available + balance_infos[].
pub fn parse_deepseek_balance(payload: &Value, updated: f64) -> Result<Vec<Limit>, UsageError> {
    let unsupported = || value_err("Unsupported API response");
    let infos = payload.get("balance_infos").and_then(Value::as_array).ok_or_else(unsupported)?;
    let mut limits = Vec::new();
    for info in infos {
        let info = info.as_object().ok_or_else(unsupported)?;
        let currency = line(&crate::text::pystr(info.get("currency").unwrap_or(&Value::Null)), 8);
        let null = Value::Null;
        let total = money(info.get("total_balance").unwrap_or(&null), &currency);
        let parts = [
            trf!("topped up {amount}", amount = money(info.get("topped_up_balance").unwrap_or(&null), &currency)),
            trf!("granted {amount}", amount = money(info.get("granted_balance").unwrap_or(&null), &currency)),
        ];
        let label = trf!("balance {total}", total = total) + "  ·  " + &parts.join("  ·  ");
        let empty = match info.get("total_balance") {
            Some(Value::Number(n)) => n.as_f64().is_some_and(|v| v <= 0.0),
            Some(Value::String(s)) => s.trim().parse::<f64>().is_ok_and(|v| v <= 0.0),
            _ => false,
        };
        let window = if currency.is_empty() { "—".to_string() } else { currency.clone() };
        let mut limit = Limit::new("deepseek", "API", &label, &window, None, 0.0, updated, "DeepSeek API · live");
        limit.status = if empty { "balance-empty" } else { "balance" }.into();
        limit.id = currency;
        limits.push(limit);
    }
    if limits.is_empty() {
        return Err(value_err("API did not report a balance"));
    }
    let funded: Vec<Limit> = limits.iter().filter(|l| l.status == "balance").cloned().collect();
    if !funded.is_empty() {
        limits = funded; // An empty second currency is not an exhausted account.
    }
    if payload.get("is_available") == Some(&Value::Bool(false)) {
        limits[0].status = "balance-empty".into();
    }
    Ok(limits)
}

fn fetch_deepseek_balance(key: &str, cancel: &Cancel, timeout: f64) -> Result<Vec<Limit>, UsageError> {
    if cancel.is_set() {
        return Err(UsageError::Cancelled);
    }
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs_f64(timeout.min(15.0))).build();
    let response = agent
        .get(DEEPSEEK_BALANCE_URL)
        .set("Authorization", &format!("Bearer {key}"))
        .set("Accept", "application/json")
        .set("User-Agent", "ais")
        .call();
    let body = match response {
        Ok(response) => {
            let mut body = Vec::new();
            response.into_reader().take(256 * 1024).read_to_end(&mut body).map_err(|_| value_err("DeepSeek API is unreachable"))?;
            body
        }
        // Never echo the response body or the key; status codes are enough.
        Err(ureq::Error::Status(code, _)) if code == 401 || code == 403 => {
            return Err(UsageError::Value(trf!("DeepSeek rejected the API key (HTTP {code})", code = code as i64)));
        }
        Err(ureq::Error::Status(code, _)) => {
            return Err(UsageError::Value(trf!("DeepSeek API error (HTTP {code})", code = code as i64)));
        }
        Err(_) => return Err(value_err("DeepSeek API is unreachable")),
    };
    if cancel.is_set() {
        return Err(UsageError::Cancelled);
    }
    let payload: Value = serde_json::from_slice(&body).map_err(|_| value_err("Unsupported API response"))?;
    parse_deepseek_balance(&payload, now())
}

// ---------------------------------------------------------------- helpers

fn window_seconds(window: &str) -> Option<f64> {
    match window {
        "5h" => Some(5.0 * 3600.0),
        "week" => Some(7.0 * 86400.0),
        _ => None,
    }
}

pub fn plan_name(value: &str) -> String {
    let value = line(value, 40);
    match value.to_lowercase().as_str() {
        "team" => "Team".into(),
        "max" => "Max".into(),
        "pro" => "Pro".into(),
        "plus" => "Plus".into(),
        "free" => "Free".into(),
        "enterprise" => "Enterprise".into(),
        "business" => "Business".into(),
        "edu" => "Edu".into(),
        _ => {
            let mut chars = value.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        }
    }
}

/// Session files touched in the last 5 hours / 7 days (mtime only).
pub fn local_activity(folder: &Path, current: Option<f64>, suffix: &str) -> (i64, i64) {
    let current = current.unwrap_or_else(now);
    let (mut recent, mut week) = (0, 0);
    if !folder.is_dir() {
        return (0, 0);
    }
    let mut stack = vec![folder.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                if name != "subagents" {
                    stack.push(entry.path());
                }
                continue;
            }
            if !name.ends_with(suffix) || name.starts_with("agent-") {
                continue;
            }
            let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else { continue };
            let age = current - crate::model::system_time(modified);
            if age <= 7.0 * 86400.0 {
                week += 1;
                if age <= 5.0 * 3600.0 {
                    recent += 1;
                }
            }
        }
    }
    (recent, week)
}

/// 'Resets 5:20pm (Europe/Berlin)' or 'Resets Nov 15 at 6am (Zone)' -> epoch.
///
/// Only these exact native forms are converted; anything else stays a label.
pub fn parse_reset_text(text: &str, current: Option<f64>) -> f64 {
    static RESET: OnceLock<Regex> = OnceLock::new();
    let re = RESET.get_or_init(|| {
        Regex::new(
            r"(?i)Resets\s+(?:(?P<month>[A-Z][a-z]{2})\s+(?P<day>\d{1,2})(?:\s+at)?\s+)?(?P<hour>\d{1,2})(?::(?P<minute>\d{2}))?\s*(?P<ampm>am|pm)(?:\s*\((?P<zone>[A-Za-z_]+(?:/[A-Za-z0-9_+-]+)*)\))?\s*$",
        )
        .unwrap()
    });
    let Some(caps) = re.captures(text) else { return 0.0 };
    let current = current.unwrap_or_else(now);
    let zone = match caps.name("zone") {
        Some(name) => match jiff::tz::TimeZone::get(name.as_str()) {
            Ok(zone) => zone,
            Err(_) => return 0.0,
        },
        None => jiff::tz::TimeZone::system(),
    };
    let Ok(ts) = jiff::Timestamp::from_nanosecond((current * 1e9) as i128) else { return 0.0 };
    let now_zoned = ts.to_zoned(zone);
    let raw_hour: i8 = caps["hour"].parse().unwrap_or(99);
    let minute: i8 = caps.name("minute").map(|m| m.as_str().parse().unwrap_or(99)).unwrap_or(0);
    if minute > 59 || raw_hour > 12 {
        return 0.0;
    }
    let hour = raw_hour % 12 + if caps["ampm"].eq_ignore_ascii_case("pm") { 12 } else { 0 };
    const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    let seconds = |z: &jiff::Zoned| z.timestamp().as_nanosecond() as f64 / 1e9;
    let result = if let Some(month) = caps.name("month") {
        let Some(index) = MONTHS.iter().position(|m| *m == month.as_str().to_lowercase()) else { return 0.0 };
        let day: i8 = caps["day"].parse().unwrap_or(0);
        let build = |year: i16| {
            now_zoned.with().year(year).month(index as i8 + 1).day(day).hour(hour).minute(minute).second(0).subsec_nanosecond(0).build()
        };
        let Ok(mut target) = build(now_zoned.year()) else { return 0.0 };
        if seconds(&target) < current - 86400.0 {
            let Ok(next) = build(now_zoned.year() + 1) else { return 0.0 };
            target = next;
        }
        target
    } else {
        let Ok(mut target) = now_zoned.with().hour(hour).minute(minute).second(0).subsec_nanosecond(0).build() else {
            return 0.0;
        };
        if seconds(&target) <= current - 60.0 {
            let Ok(next) = target.checked_add(jiff::Span::new().days(1)) else { return 0.0 };
            target = next;
        }
        target
    };
    seconds(&result)
}

pub fn duration_text(seconds: f64) -> String {
    let seconds = (seconds as i64).max(60);
    if seconds >= 86400 {
        return trf!("{d}d {h}h", d = seconds / 86400, h = seconds % 86400 / 3600);
    }
    if seconds >= 3600 {
        return trf!("{h}h {m}m", h = seconds / 3600, m = seconds % 3600 / 60);
    }
    trf!("{m}m", m = seconds / 60)
}

/// Project the current burn rate to the reset; None when unknowable.
pub fn pace(limit: &Limit, current: f64) -> Option<(String, &'static str)> {
    let span = window_seconds(&limit.window)?;
    let used = limit.used?;
    if limit.reset == 0.0 || limit.reset <= current {
        return None;
    }
    let left = limit.reset - current;
    let elapsed = span - left;
    if elapsed < span * 0.03 || used <= 0.0 {
        return None;
    }
    let early = elapsed < span * 0.15;
    let rate = used / elapsed;
    let expected = elapsed / span;
    if used >= 1.0 {
        return Some((tr("limit reached · wait for reset"), "red"));
    }
    let runs_out = (1.0 - used) / rate;
    if runs_out < left * 0.92 && !early {
        return Some((
            trf!("at this pace runs out in {time} — before reset", time = duration_text(runs_out)),
            if runs_out < left * 0.5 { "red" } else { "amber" },
        ));
    }
    if used > expected + 0.1 {
        return Some((
            trf!(
                "ahead of an even pace ({used:.0f}% used, {expected:.0f}% of window elapsed)",
                used = used * 100.0,
                expected = expected * 100.0
            ),
            "amber",
        ));
    }
    Some((trf!("on pace · {expected:.0f}% of window elapsed", expected = expected * 100.0), "green"))
}

fn numeric(value: &Value) -> Option<f64> {
    value.as_f64().filter(|v| v.is_finite())
}

fn timestamp(value: &Value) -> f64 {
    numeric(value).filter(|v| *v > 0.0 && *v < 253_402_300_800.0).unwrap_or(0.0)
}

fn valid_epoch(value: f64) -> f64 {
    if value.is_finite() && value > 0.0 && value < 253_402_300_800.0 { value } else { 0.0 }
}

pub fn anonymous_account(value: &str) -> String {
    format!("#{}", &format!("{:x}", Sha256::digest(value.as_bytes()))[..6])
}

/// Documented account/rateLimits/read result, never credentials or rollouts.
///
/// https://developers.openai.com/codex/app-server/#rate-limits-chatgpt
/// The public multi-bucket view supersedes the duplicate legacy bucket.
pub fn parse_codex_usage(payload: &Value, updated: f64) -> Result<Vec<Limit>, UsageError> {
    let payload = payload.as_object().ok_or_else(|| value_err("Unsupported CLI response"))?;
    let mut buckets: Map<String, Value> = match payload.get("rateLimitsByLimitId") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(map)) => map.clone(),
        Some(_) => return Err(value_err("Unsupported CLI response")),
    };
    if buckets.is_empty() {
        let single = payload.get("rateLimits").filter(|v| v.is_object()).ok_or_else(|| value_err("CLI did not report limits"))?;
        buckets.insert("codex".into(), single.clone());
    }
    let account = match payload.get("accountId") {
        Some(Value::String(id)) if !id.is_empty() => anonymous_account(id),
        _ => "CLI".into(),
    };
    let mut limits = Vec::new();
    for (key, bucket) in &buckets {
        let bucket = bucket.as_object().ok_or_else(|| value_err("Unsupported CLI response"))?;
        for kind in ["primary", "secondary"] {
            let window = match bucket.get(kind) {
                None | Some(Value::Null) => continue,
                Some(Value::Object(w)) => w,
                Some(_) => return Err(value_err("Unsupported CLI response")),
            };
            let minutes = window.get("windowDurationMins").and_then(numeric);
            let name = match minutes {
                Some(300.0) => "5h".to_string(),
                Some(10080.0) => "week".to_string(),
                Some(m) if m > 0.0 => {
                    if m.fract() == 0.0 {
                        format!("{}m", m as i64)
                    } else {
                        format!("{m}m")
                    }
                }
                _ => kind.to_string(),
            };
            // Reject invalid percentages rather than displaying invented values.
            let fraction = window
                .get("usedPercent")
                .filter(|v| !v.is_boolean())
                .and_then(numeric)
                .filter(|v| (0.0..=100.0).contains(v))
                .map(|v| v / 100.0);
            let label =
                line(&crate::text::pystr(first_truthy(bucket, &["limitName", "limitId"]).unwrap_or(&Value::String(key.clone()))), 80);
            let mut limit = Limit::new(
                "openai-codex",
                &account,
                &label,
                &name,
                fraction,
                timestamp(window.get("resetsAt").unwrap_or(&Value::Null)),
                updated,
                "Codex CLI · live",
            );
            limit.id = format!("{}:{kind}", line(key, 80));
            limits.push(limit);
        }
    }
    if limits.is_empty() {
        return Err(value_err("CLI did not report limits"));
    }
    Ok(limits)
}

fn first_truthy<'a>(map: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().filter_map(|k| map.get(*k)).find(|v| truthy(v))
}

fn kill_group(child: &mut std::process::Child) {
    #[cfg(unix)]
    // SAFETY: the child was started in its own process group.
    unsafe {
        libc::killpg(child.id() as i32, libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

pub enum CliOutput {
    Text(String),
    Rpc(Map<String, Value>),
}

/// Bounded, cancellable official CLI transport; stderr never enters the UI.
///
/// Only initialize and rate-limit read are allowed. No thread or model turn is
/// created. Server requests (including external auth refresh) are declined.
pub fn cli_output(command: &[OsString], env: &Env, cwd: &Path, cancel: &Cancel, timeout: f64, rpc: bool) -> Result<CliOutput, UsageError> {
    if cancel.is_set() {
        return Err(UsageError::Cancelled);
    }
    let mut builder = Command::new(&command[0]);
    builder
        .args(&command[1..])
        .env_clear()
        .envs(env)
        .current_dir(cwd)
        .stdin(if rpc { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        builder.process_group(0);
    }
    let mut child = builder.spawn().map_err(|e| UsageError::Os(e.to_string()))?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stdin = child.stdin.take();
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buffer = vec![0u8; 65536];
        loop {
            match stdout.read(&mut buffer) {
                Ok(0) | Err(_) => {
                    let _ = tx.send(Vec::new());
                    break;
                }
                Ok(n) => {
                    if tx.send(buffer[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let mut send = |message: Value| -> Result<(), UsageError> {
        let stdin = stdin.as_mut().ok_or_else(|| value_err("CLI closed before replying"))?;
        stdin.write_all(format!("{message}\n").as_bytes()).and_then(|_| stdin.flush()).map_err(|_| value_err("CLI closed before replying"))
    };
    let result = (|| {
        if rpc {
            send(json!({"id": 1, "method": "initialize", "params": {
                "clientInfo": {"name": "ai_sessions", "title": "ais Usage", "version": "1.0"}}}))?;
        }
        let deadline = Instant::now() + Duration::from_secs_f64(timeout);
        let mut buffer: Vec<u8> = Vec::new();
        let mut output: Vec<u8> = Vec::new();
        let mut total = 0usize;
        let mut expected = 1;
        // account/read is optional identity metadata: a short grace period,
        // and any failure keeps the already received rate limits.
        let mut results = Map::new();
        let mut account_deadline: Option<Instant> = None;
        loop {
            if cancel.is_set() {
                return Err(UsageError::Cancelled);
            }
            if expected == 3 && account_deadline.is_some_and(|d| Instant::now() >= d) {
                return Ok(CliOutput::Rpc(results));
            }
            let current = Instant::now();
            if current >= deadline {
                return Err(UsageError::Timeout(tr("CLI request timed out")));
            }
            let chunk = match rx.recv_timeout(Duration::from_millis(100).min(deadline - current)) {
                Ok(chunk) => chunk,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => Vec::new(),
            };
            if chunk.is_empty() && expected == 3 {
                return Ok(CliOutput::Rpc(results));
            }
            if chunk.is_empty() {
                let status = loop {
                    if cancel.is_set() {
                        return Err(UsageError::Cancelled);
                    }
                    if Instant::now() >= deadline {
                        return Err(UsageError::Timeout(tr("CLI request timed out")));
                    }
                    match child.try_wait() {
                        Ok(Some(status)) => break status,
                        Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                        Err(e) => return Err(UsageError::Os(e.to_string())),
                    }
                };
                if !status.success() {
                    let code = status.code().map(|c| c as i64).unwrap_or(-1);
                    return Err(UsageError::Value(trf!("CLI exited with code {code}", code = code)));
                }
                if rpc {
                    return Err(value_err("CLI closed before replying"));
                }
                return Ok(CliOutput::Text(String::from_utf8_lossy(&output).into_owned()));
            }
            total += chunk.len();
            if total > 2 * 1024 * 1024 {
                return Err(value_err("CLI response too large"));
            }
            if !rpc {
                output.extend_from_slice(&chunk);
                continue;
            }
            buffer.extend_from_slice(&chunk);
            while let Some(index) = buffer.iter().position(|b| *b == b'\n') {
                let raw: Vec<u8> = buffer.drain(..=index).collect();
                let message: Value = serde_json::from_slice(&raw[..raw.len() - 1]).map_err(|_| value_err("Unsupported CLI response"))?;
                let message = message.as_object().ok_or_else(|| value_err("Unsupported CLI response"))?;
                if message.contains_key("method") {
                    if let Some(id) = message.get("id") {
                        send(json!({"id": id, "error": {"code": -32601, "message": "Unsupported client request"}}))?;
                    }
                    continue;
                }
                let id = message.get("id").and_then(Value::as_i64);
                if expected == 2 && id == Some(3) {
                    if let Some(result) = message.get("result").filter(|v| v.is_object()) {
                        results.insert("account".into(), result.clone());
                    }
                    continue;
                }
                if id != Some(expected) {
                    continue;
                }
                if message.contains_key("error") && expected == 3 {
                    return Ok(CliOutput::Rpc(results));
                }
                if let Some(error) = message.get("error") {
                    let code = match error.get("code") {
                        Some(Value::Number(n)) if n.is_i64() => n.to_string(),
                        _ => "?".into(),
                    };
                    return Err(UsageError::Value(trf!(
                        "CLI request rejected (code {code}); check login in the official CLI",
                        code = code
                    )));
                }
                let Some(result) = message.get("result") else { return Err(value_err("Unsupported CLI response")) };
                match expected {
                    1 => {
                        send(json!({"method": "initialized", "params": {}}))?;
                        send(json!({"id": 2, "method": "account/rateLimits/read"}))?;
                        send(json!({"id": 3, "method": "account/read", "params": {}}))?;
                        expected = 2;
                    }
                    2 => {
                        results.insert("limits".into(), result.clone());
                        if results.contains_key("account") {
                            return Ok(CliOutput::Rpc(results));
                        }
                        expected = 3;
                        account_deadline = Some(Instant::now() + Duration::from_millis(400));
                    }
                    _ => {
                        results.insert("account".into(), result.clone());
                        return Ok(CliOutput::Rpc(results));
                    }
                }
            }
        }
    })();
    // Include any children the official CLI started; do not leave a
    // provider request or language server running after Q / timeout.
    drop(stdin);
    kill_group(&mut child);
    result
}

pub fn unavailable(provider: &str, reason: &str) -> Limit {
    let mut limit = Limit::new(provider, "CLI", reason, "—", None, 0.0, 0.0, "Official CLI");
    limit.status = "unavailable".into();
    limit
}

fn claude_status() -> Limit {
    unavailable("anthropic", if which("claude").is_some() { "No saved Claude CLI limits" } else { "Claude CLI not installed" })
}

pub fn parse_claude_usage(windows: &[Window], updated: f64) -> Result<Vec<Limit>, UsageError> {
    static MODEL: OnceLock<Regex> = OnceLock::new();
    let model = MODEL.get_or_init(|| Regex::new(r"\(([^)]+)\)").unwrap());
    let mut limits = Vec::new();
    for window in windows {
        let heading = &window.label;
        let (name, label) = if heading.to_lowercase() == "current session" {
            ("5h", "Claude".to_string())
        } else {
            let label = match model.captures(heading) {
                Some(caps) if caps[1].to_lowercase() != "all models" => caps[1].to_string(),
                _ => "All Claude models".to_string(),
            };
            ("week", label)
        };
        let mut limit = Limit::new("anthropic", "CLI", &label, name, Some(window.used_percent / 100.0), 0.0, updated, "Claude CLI · live");
        limit.id = heading.clone();
        limit.reset_label = window.reset_text.clone();
        limits.push(limit);
    }
    if limits.is_empty() {
        return Err(value_err("Claude /usage did not provide plan limits"));
    }
    Ok(limits)
}

fn cache_path(collector: &Collector) -> Option<PathBuf> {
    let cache = collector.cache.lock().unwrap();
    if !cache.enabled {
        return None;
    }
    let root = cache.root.clone()?;
    drop(cache);
    // Isolate alternate homes and --source stores without saving their paths.
    let resolve = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()).to_string_lossy().into_owned();
    let roots: Vec<String> = ["codex", "agy", "claude"]
        .iter()
        .map(|client| {
            let paths: Vec<String> = collector.root(client).iter().map(|p| format!("'{}'", resolve(p))).collect();
            format!("'{client}': [{}]", paths.join(", "))
        })
        .collect();
    let context = format!("{}{{{}}}", resolve(&collector.home), roots.join(", "));
    let key = format!("{:x}", Sha256::digest(context.as_bytes()));
    Some(root.join(format!("official-usage-{}.json", &key[..16])))
}

fn read_cache(path: &Path, usage: &mut Usage) -> Result<(), ()> {
    if std::fs::metadata(path).map_err(|_| ())?.len() > 2 * 1024 * 1024 {
        return Err(());
    }
    let payload: Value = serde_json::from_str(&std::fs::read_to_string(path).map_err(|_| ())?).map_err(|_| ())?;
    if payload.get("version").and_then(Value::as_i64) != Some(1) {
        return Err(());
    }
    let mut saved = Vec::new();
    for item in payload.get("limits").and_then(Value::as_array).ok_or(())? {
        let mut r: Limit = serde_json::from_value(item.clone()).map_err(|_| ())?;
        let source = cli_source(&r.provider).ok_or(())?;
        let valid = r.source == format!("{source} · live")
            && valid_epoch(r.updated) != 0.0
            && r.used.is_none_or(|u| u.is_finite() && (0.0..=1.0).contains(&u))
            && r.reset.is_finite()
            && r.reset >= 0.0
            && r.reset < 253_402_300_800.0;
        if !valid {
            return Err(());
        }
        r.source = format!("{source} · cached");
        saved.push(r);
    }
    // Migrate earlier official text snapshots into the same normalized
    // bars; unsupported old text never substitutes for current quotas.
    for item in payload.get("reports").and_then(Value::as_array).ok_or(())? {
        let r: Report = serde_json::from_value(item.clone()).map_err(|_| ())?;
        if r.provider != "google-antigravity" || valid_epoch(r.updated) == 0.0 {
            return Err(());
        }
        match parse_agy_usage(&r.text, r.updated) {
            Ok(migrated) => saved.extend(migrated.into_iter().map(|mut l| {
                l.source = "agy CLI · cached".into();
                l
            })),
            Err(_) => usage.errors.push(tr("Cached agy report format unavailable; refresh with the official CLI")),
        }
    }
    usage.limits.extend(saved);
    for item in payload.get("accounts").and_then(Value::as_array).into_iter().flatten() {
        if let Ok(account) = serde_json::from_value::<Account>(item.clone()) {
            usage.accounts.push(account);
        }
    }
    Ok(())
}

/// Only ais snapshots produced by the official adapters; no OMP database.
pub fn load_usage(collector: &Collector) -> Usage {
    let mut usage = Usage { loaded: true, ..Default::default() };
    if let Some(path) = cache_path(collector)
        && path.exists()
    {
        let mut attempt = Usage::default();
        if read_cache(&path, &mut attempt).is_ok() {
            usage.limits = attempt.limits;
            usage.accounts = attempt.accounts;
            usage.errors.extend(attempt.errors);
        } else {
            usage.errors.push(tr("Official CLI cache could not be read"));
        }
    }
    if !usage.limits.iter().any(|r| r.provider == "anthropic") {
        usage.limits.insert(0, claude_status());
    }
    if !usage.limits.iter().any(|r| r.provider == "openai-codex") {
        usage.limits.push(unavailable("openai-codex", "No saved Codex CLI limits"));
    }
    if !usage.limits.iter().any(|r| r.provider == "google-antigravity") {
        usage.limits.push(unavailable("google-antigravity", "No saved agy CLI limits"));
    }
    usage
}

fn save_usage(collector: &Collector, usage: &mut Usage) {
    let Some(path) = cache_path(collector) else { return };
    // Failed providers retain their last successful snapshot and timestamp.
    let limits: Vec<Limit> = usage
        .limits
        .iter()
        .filter(|r| r.status != "unavailable")
        .filter_map(|r| cli_source(&r.provider).map(|s| Limit { source: format!("{s} · live"), ..r.clone() }))
        .collect();
    let reports: Vec<Report> = usage.reports.iter().map(|r| Report { cached: false, ..r.clone() }).collect();
    let payload = json!({"version": 1, "limits": limits, "reports": reports, "accounts": usage.accounts});
    if write_private(&path, payload.to_string().as_bytes()).is_err() {
        usage.errors.push(tr("Official CLI cache could not be saved"));
    }
}

fn agy_report(text: &str) -> Result<String, UsageError> {
    // Sanitize the public report and preserve the existing model-group filter.
    // The TSV parser below validates its columns before normalizing quotas.
    static EMAIL: OnceLock<Regex> = OnceLock::new();
    static ERROR: OnceLock<Regex> = OnceLock::new();
    static HIDE: OnceLock<Regex> = OnceLock::new();
    static SHOW: OnceLock<Regex> = OnceLock::new();
    static QUOTA: OnceLock<Regex> = OnceLock::new();
    let text = clean(text, 0).trim().to_string();
    let text = EMAIL.get_or_init(|| Regex::new(r"[\w.+-]+@[\w.-]+\.[A-Za-z]{2,}").unwrap()).replace_all(&text, "[account]").into_owned();
    if ERROR.get_or_init(|| Regex::new(r"(?im)^\s*(error|failed|unauthorized|forbidden)\b").unwrap()).is_match(&text) {
        return Err(value_err("CLI could not report limits; check the official CLI"));
    }
    // Preserve the existing Gemini-only display preference. Once a Claude/GPT
    // group starts, omit its continuation rows until the next Gemini heading.
    let hide = HIDE.get_or_init(|| Regex::new(r"(?i)claude|gpt").unwrap());
    let show = SHOW.get_or_init(|| Regex::new(r"(?i)gemini").unwrap());
    let mut rows = Vec::new();
    let mut hidden = false;
    for row in crate::text::splitlines(&text) {
        if hide.is_match(row) {
            hidden = true;
        } else if show.is_match(row) {
            hidden = false;
        }
        if !hidden {
            rows.push(row);
        }
    }
    let text = rows.join("\n").trim().to_string();
    if text.is_empty() || !QUOTA.get_or_init(|| Regex::new(r"(?i)%|remaining|reset|unlimited").unwrap()).is_match(&text) {
        return Err(value_err("CLI did not report limits"));
    }
    Ok(text)
}

/// Official `agy -p /usage` TSV: group, quota, remaining %, reset ISO.
pub fn parse_agy_usage(text: &str, updated: f64) -> Result<Vec<Limit>, UsageError> {
    static PERCENT: OnceLock<Regex> = OnceLock::new();
    static FIVE: OnceLock<Regex> = OnceLock::new();
    static ZONE: OnceLock<Regex> = OnceLock::new();
    let percent_re = PERCENT.get_or_init(|| Regex::new(r"^(\d+(?:\.\d+)?)%$").unwrap());
    let five_re = FIVE.get_or_init(|| Regex::new(r"(?i)five[ -]hour|5[ -]hour").unwrap());
    let zone_re = ZONE.get_or_init(|| Regex::new(r"(?:Z|[+-]\d\d:\d\d)$").unwrap());
    let text = agy_report(text)?;
    let mut limits = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let unsupported = || value_err("Unsupported agy usage report; expected official quota rows");
    for row in crate::text::splitlines(&text) {
        if row.trim().is_empty() {
            continue;
        }
        let columns: Vec<&str> = row.split('\t').map(str::trim).collect();
        if columns.len() != 4 {
            return Err(unsupported());
        }
        let (group, label, amount, reset) = (columns[0], columns[1], columns[2], columns[3]);
        let percent = percent_re.captures(amount);
        let value: Option<f64> = percent.and_then(|c| c[1].parse().ok()).filter(|v| (0.0..=100.0).contains(v));
        let (Some(value), false, false) = (value, group.is_empty(), label.is_empty()) else { return Err(unsupported()) };
        let window = if label.to_lowercase().contains("weekly") {
            "week"
        } else if five_re.is_match(label) {
            "5h"
        } else {
            return Err(value_err("Unsupported agy quota window"));
        };
        let reset_at = if zone_re.is_match(reset) { valid_epoch(parse_iso(reset).unwrap_or(0.0)) } else { 0.0 };
        if reset_at == 0.0 {
            return Err(value_err("Unsupported agy quota reset time"));
        }
        if !seen.insert((group.to_string(), label.to_string())) {
            continue;
        }
        let mut limit = Limit::new(
            "google-antigravity",
            "CLI",
            &line(group, 80),
            window,
            Some(1.0 - value / 100.0),
            reset_at,
            updated,
            "agy CLI · live",
        );
        limit.id = format!("{group}:{label}");
        limits.push(limit);
    }
    if limits.is_empty() {
        return Err(value_err("CLI did not report limits"));
    }
    limits.sort_by_key(|r| if r.window == "5h" { 0 } else { 1 });
    Ok(limits)
}

/// (config dir, is the CLI default) for every Claude store ais reads.
pub fn claude_stores(collector: &Collector) -> Vec<(PathBuf, bool)> {
    let default = collector.home.join(".claude");
    let plain = collector.is_real_home()
        && std::env::var_os("CLAUDE_CONFIG_DIR").is_none_or(|v| v.is_empty())
        && !collector.overrides.contains_key("claude");
    collector
        .root("claude")
        .into_iter()
        .map(|root| {
            let is_default = plain && root == default;
            (root, is_default)
        })
        .collect()
}

fn claude_identity(binary: &Path, env: &Env, cwd: &Path, cancel: &Cancel, timeout: f64) -> Map<String, Value> {
    let command: Vec<OsString> = vec![binary.into(), "auth".into(), "status".into(), "--json".into()];
    match cli_output(&command, env, cwd, cancel, timeout.min(6.0), false) {
        Ok(CliOutput::Text(text)) => serde_json::from_str::<Value>(&text).ok().and_then(|v| v.as_object().cloned()).unwrap_or_default(),
        _ => Map::new(),
    }
}

/// Parse JSON, tolerating lone UTF-16 surrogate escapes (`\ud83d` without its
/// pair). Claude Code can save a prompt cut in the middle of an emoji, and
/// strict serde_json would then reject the whole `.claude.json`.
pub fn lenient_json(text: &str) -> Option<Value> {
    if let Ok(value) = serde_json::from_str(text) {
        return Some(value);
    }
    let bytes = text.as_bytes();
    let hex = |at: usize| -> Option<u16> {
        let digits = text.get(at..at + 4)?;
        u16::from_str_radix(digits, 16).ok()
    };
    let escape_at =
        |at: usize| -> Option<u16> { (bytes.get(at) == Some(&b'\\') && bytes.get(at + 1) == Some(&b'u')).then(|| hex(at + 2)).flatten() };
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut last = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            i += 1;
            continue;
        }
        let Some(unit) = escape_at(i) else {
            i += 2; // any other escape, including an escaped backslash
            continue;
        };
        let high = (0xD800..0xDC00).contains(&unit);
        let low = (0xDC00..0xE000).contains(&unit);
        let paired = high && escape_at(i + 6).is_some_and(|next| (0xDC00..0xE000).contains(&next));
        if paired {
            i += 12;
        } else if high || low {
            out.push_str(&text[last..i]);
            out.push_str("\\ufffd");
            i += 6;
            last = i;
        } else {
            i += 6;
        }
    }
    out.push_str(&text[last..]);
    serde_json::from_str(&out).ok()
}

/// A folder this Claude config already trusts, so /usage never needs a prompt.
pub fn trusted_dir(config: &Path, fallback: &Path, home: &Path) -> PathBuf {
    let Ok(meta) = std::fs::metadata(config) else { return fallback.to_path_buf() };
    if meta.len() > 64 * 1024 * 1024 {
        return fallback.to_path_buf();
    }
    let Ok(text) = std::fs::read_to_string(config) else { return fallback.to_path_buf() };
    let Some(Value::Object(data)) = lenient_json(&text) else { return fallback.to_path_buf() };
    let Some(Value::Object(projects)) = data.get("projects") else { return fallback.to_path_buf() };
    let trusted = |info: &Value| info.get("hasTrustDialogAccepted").is_some_and(truthy);
    // Claude never keeps trust for the home folder (or its ancestors): "Yes
    // lasts for this session only", so a saved flag there still prompts.
    let usable = |path: &Path| path.is_dir() && !home.starts_with(path);
    if usable(fallback) && projects.get(&*fallback.to_string_lossy()).is_some_and(trusted) {
        return fallback.to_path_buf();
    }
    for (path, info) in projects {
        if trusted(info) && usable(Path::new(path)) {
            return PathBuf::from(path);
        }
    }
    fallback.to_path_buf()
}

pub fn agy_method(method: &str) -> String {
    match method {
        "consumer" => "Google account".into(),
        "enterprise" => "Workspace".into(),
        "vertex" => "Vertex AI".into(),
        other => other.into(),
    }
}

/// (email, auth method) from the agy log written by this /usage run.
///
/// agy has no account command; its own log records the signed-in OAuth user.
/// Only logs created by the run are read, so an old login is never reused.
pub fn agy_identity(store: &Path, started: f64) -> (String, String) {
    static AUTH: OnceLock<Regex> = OnceLock::new();
    let auth = AUTH.get_or_init(|| Regex::new(r"applyAuthResult: email=([^,\s]+), authMethod=(\w+)").unwrap());
    let mut logs: Vec<(f64, PathBuf)> = std::fs::read_dir(store.join("log"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if !(name.starts_with("cli-") && name.ends_with(".log")) {
                return None;
            }
            let modified = crate::model::system_time(e.metadata().ok()?.modified().ok()?);
            (modified >= started - 2.0).then(|| (modified, e.path()))
        })
        .collect();
    logs.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    for (_, path) in logs {
        let Ok(file) = std::fs::File::open(&path) else { continue };
        let mut data = Vec::new();
        if file.take(2 * 1024 * 1024).read_to_end(&mut data).is_err() {
            continue;
        }
        let text = String::from_utf8_lossy(&data);
        if let Some(caps) = auth.captures_iter(&text).last() {
            return (line(&caps[1], 120), caps[2].to_string());
        }
    }
    (String::new(), String::new())
}

pub fn account_key(parts: &[&str]) -> String {
    if parts.iter().any(|p| !p.is_empty()) {
        anonymous_account(&parts.iter().filter(|p| !p.is_empty()).copied().collect::<Vec<_>>().join("|"))
    } else {
        "CLI".into()
    }
}

pub fn pretty_dir(path: &Path, home_dir: &Path) -> String {
    let text = path.to_string_lossy();
    let home_text = home_dir.to_string_lossy();
    let prefix = format!("{home_text}{}", std::path::MAIN_SEPARATOR);
    match text.strip_prefix(&prefix) {
        Some(rest) => format!("~{}{rest}", std::path::MAIN_SEPARATOR),
        None => text.into_owned(),
    }
}

/// `claude auth status` output and the config dirs signed in to that account.
type Identity = (Map<String, Value>, Vec<(PathBuf, bool)>);

type Fetched = Result<(Vec<Limit>, Vec<Account>), UsageError>;

fn base_env(collector: &Collector) -> Env {
    let mut env: Env = std::env::vars_os().collect();
    env.insert("HOME".into(), collector.home.clone().into_os_string());
    env
}

fn fetch_codex(collector: &Collector, binary: &Path, cancel: &Cancel, timeout: f64) -> Fetched {
    let roots = collector.root("codex");
    let root = roots.first().ok_or_else(|| value_err("CLI source not configured"))?;
    let mut env = base_env(collector);
    env.insert("CODEX_HOME".into(), root.clone().into_os_string());
    let command: Vec<OsString> = vec![binary.into(), "app-server".into()];
    let CliOutput::Rpc(result) = cli_output(&command, &env, &collector.home, cancel, timeout, true)? else {
        return Err(value_err("Unsupported CLI response"));
    };
    let raw = result.get("limits").cloned().unwrap_or(Value::Null);
    let limits = parse_codex_usage(&raw, now())?;
    let empty = Map::new();
    let info = result.get("account").and_then(Value::as_object).unwrap_or(&empty);
    let account = info.get("account").and_then(Value::as_object).unwrap_or(&empty);
    let bucket = raw.get("rateLimits").and_then(Value::as_object).unwrap_or(&empty);
    let key = limits.first().map(|l| l.account.clone()).unwrap_or_else(|| "CLI".into());
    let mut details = Account::new("openai-codex", &key);
    details.email = line(&crate::text::pystr(account.get("email").unwrap_or(&Value::Null)), 120);
    details.name = line(&crate::text::pystr(account.get("workspaceName").unwrap_or(&Value::Null)), 80);
    details.plan = plan_name(&crate::text::pystr(
        first_truthy(account, &["planType"]).or_else(|| first_truthy(bucket, &["planType"])).unwrap_or(&Value::Null),
    ));
    details.dirs = vec![pretty_dir(root, &collector.home)];
    let credits = bucket.get("credits").and_then(Value::as_object).unwrap_or(&empty);
    if credits.get("unlimited").is_some_and(truthy) {
        details.credits = tr("unlimited credits");
    } else if credits.get("hasCredits").is_some_and(truthy) {
        details.credits = trf!("credits: {balance}", balance = line_value(credits.get("balance").unwrap_or(&Value::Null), 20));
    }
    if bucket.get("rateLimitReachedType").is_some_and(truthy) || bucket.get("spendControlReached").is_some_and(truthy) {
        let kind = first_truthy(bucket, &["rateLimitReachedType"]).map(crate::text::pystr).unwrap_or_else(|| "spend control".into());
        details.warning = trf!("limit reached: {kind}", kind = line(&kind, 40));
    }
    (details.sessions_5h, details.sessions_7d) = local_activity(&root.join("sessions"), None, ".jsonl");
    Ok((limits, vec![details]))
}

fn fetch_claude(collector: &Collector, binary: &Path, cancel: &Cancel, timeout: f64) -> Fetched {
    let stores = claude_stores(collector);
    if stores.is_empty() {
        return Err(value_err("CLI source not configured"));
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| collector.home.clone());
    let found: Vec<(Map<String, Value>, Env)> = std::thread::scope(|scope| {
        let handles: Vec<_> = stores
            .iter()
            .map(|(root, is_default)| {
                scope.spawn(move || {
                    let mut env = base_env(collector);
                    if *is_default {
                        env.remove(&OsString::from("CLAUDE_CONFIG_DIR"));
                    } else {
                        env.insert("CLAUDE_CONFIG_DIR".into(), root.clone().into_os_string());
                    }
                    (claude_identity(binary, &env, &collector.home, cancel, timeout), env)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap_or_default()).collect()
    });
    let text = |map: &Map<String, Value>, key: &str| map.get(key).map(crate::text::pystr).unwrap_or_default();
    let mut identities: indexmap::IndexMap<String, Identity> = indexmap::IndexMap::new();
    let mut envs: std::collections::HashMap<String, (Env, PathBuf)> = Default::default();
    for (store, (status, env)) in stores.iter().zip(found) {
        let store_text = store.0.to_string_lossy().into_owned();
        let key = if status.get("loggedIn") == Some(&Value::Bool(false)) {
            account_key(&["logged-out", &store_text])
        } else if !status.is_empty() {
            account_key(&[&text(&status, "email"), &text(&status, "orgId")])
        } else {
            account_key(&[&store_text])
        };
        identities.entry(key.clone()).or_insert_with(|| (status.clone(), Vec::new())).1.push(store.clone());
        let config = if store.1 { collector.home.join(".claude.json") } else { store.0.join(".claude.json") };
        envs.entry(key).or_insert_with(|| (env, trusted_dir(&config, &cwd, &collector.home)));
    }
    let keys: Vec<String> = identities.keys().cloned().collect();
    let results: Vec<(Account, Vec<Limit>, String)> = std::thread::scope(|scope| {
        let handles: Vec<_> = keys
            .iter()
            .map(|key| {
                let (status, members) = &identities[key];
                let (env, trusted) = &envs[key];
                scope.spawn(move || {
                    let email = line(&text(status, "email"), 120);
                    let mut org = line(&text(status, "orgName"), 80);
                    if !email.is_empty()
                        && (org.to_lowercase() == format!("{email}'s organization").to_lowercase()
                            || org.to_lowercase() == email.to_lowercase())
                    {
                        org.clear(); // A personal default organization only repeats the email.
                    }
                    let mut details = Account::new("anthropic", key);
                    details.email = email;
                    details.name = org;
                    details.plan = plan_name(&text(status, "subscriptionType"));
                    details.dirs = members
                        .iter()
                        .map(|(root, default)| pretty_dir(root, &collector.home) + &if *default { tr(" (default)") } else { String::new() })
                        .collect();
                    details.expires = crate::model::stamp(status.get("loginExpiresAt").unwrap_or(&Value::from(0)), 0.0);
                    for (root, _) in members {
                        let (recent, week) = local_activity(&root.join("projects"), None, ".jsonl");
                        details.sessions_5h += recent;
                        details.sessions_7d += week;
                    }
                    if status.get("loggedIn") == Some(&Value::Bool(false)) {
                        return (details, Vec::new(), tr("not logged in · run claude /login with this config dir"));
                    }
                    let rows = read_claude_usage(binary, env, trusted, cancel, timeout).and_then(|w| parse_claude_usage(&w, now()));
                    match rows {
                        Ok(mut rows) => {
                            for row in &mut rows {
                                row.account = key.clone();
                                row.reset = parse_reset_text(&row.reset_label, None);
                            }
                            (details, rows, String::new())
                        }
                        Err(UsageError::Cancelled) => (details, Vec::new(), String::new()),
                        Err(error) => (details, Vec::new(), error.safe()),
                    }
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    if cancel.is_set() {
        return Err(UsageError::Cancelled);
    }
    let mut accounts = Vec::new();
    let mut limits = Vec::new();
    let mut errors = Vec::new();
    for (details, rows, error) in results {
        limits.extend(rows);
        if !error.is_empty() {
            let mut limit = Limit::new("anthropic", &details.key, &error, "—", None, 0.0, 0.0, "Official CLI");
            limit.status = "unavailable".into();
            limits.push(limit);
            errors.push(error);
        }
        accounts.push(details);
    }
    if !errors.is_empty() && !limits.iter().any(|r| r.status != "unavailable") {
        return Err(UsageError::Value(errors.remove(0)));
    }
    Ok((limits, accounts))
}

fn fetch_agy(collector: &Collector, binary: &Path, cancel: &Cancel, timeout: f64) -> Fetched {
    let store = collector.home.join(".gemini/antigravity-cli");
    // agy has no documented flag/environment variable for alternate stores.
    if collector.root("agy") != vec![store.clone()] {
        return Err(value_err("agy CLI does not support an alternate usage store"));
    }
    let started = now();
    let command: Vec<OsString> = vec![binary.into(), "-p".into(), "/usage".into()];
    let CliOutput::Text(result) = cli_output(&command, &base_env(collector), &collector.home, cancel, timeout, false)? else {
        return Err(value_err("Unsupported CLI response"));
    };
    let mut limits = parse_agy_usage(&result, now())?;
    let (email, method) = agy_identity(&store, started);
    let key = if email.is_empty() { "CLI".to_string() } else { account_key(&[&email]) };
    for limit in &mut limits {
        limit.account = key.clone();
    }
    let mut details = Account::new("google-antigravity", &key);
    details.email = email;
    details.plan = agy_method(&method);
    details.dirs = vec![pretty_dir(&store, &collector.home)];
    (details.sessions_5h, details.sessions_7d) = local_activity(&store.join("conversations"), None, ".db");
    Ok((limits, vec![details]))
}

fn fetch(client: &str, collector: &Collector, cancel: &Cancel, timeout: f64) -> Fetched {
    if client == "deepseek" {
        let key = deepseek_key();
        let mut limits = fetch_deepseek_balance(&key, cancel, timeout)?;
        let tail: String = key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
        for limit in &mut limits {
            limit.account = format!("key …{tail}");
        }
        return Ok((limits, Vec::new()));
    }
    let binary = which(client).ok_or_else(|| value_err("Official CLI not installed"))?;
    match client {
        "codex" => fetch_codex(collector, &binary, cancel, timeout),
        "claude" => fetch_claude(collector, &binary, cancel, timeout),
        _ => fetch_agy(collector, &binary, cancel, timeout),
    }
}

fn not_installed(client: &str) -> String {
    match client {
        "claude" => tr("Claude Code not installed"),
        "codex" => tr("Codex CLI not installed"),
        _ => tr("Antigravity CLI not installed"),
    }
}

/// One plain next step for a failed provider, shown after its error.
fn error_hint(client: &str, error: &UsageError) -> Option<String> {
    let text = error.safe();
    let timed_out = matches!(error, UsageError::Timeout(_));
    Some(match (client, error) {
        (_, UsageError::Os(_)) => trf!("reinstall {client} or check that it starts from a terminal", client = client),
        ("deepseek", _) => return None,
        _ if timed_out => tr("the CLI answered too slowly; press R to retry"),
        ("codex", _) => tr("run codex login in a terminal (sign in with ChatGPT), then press R"),
        ("claude", _) if text.starts_with(&tr("Claude /usage did not provide plan limits")) => {
            tr("run claude, type /usage and check that limits show there")
        }
        ("claude", _) => return None, // login and trust messages already say what to do
        _ => tr("open Antigravity and sign in, then press R"),
    })
}

pub fn fetch_live_usage(collector: &Collector, cancel: &Cancel, timeout: f64) -> Result<Usage, UsageError> {
    if cancel.is_set() {
        return Err(UsageError::Cancelled);
    }
    let mut usage = load_usage(collector);
    usage.checked = now();
    // The DeepSeek key belongs to the real user, never to an alternate --home.
    let with_deepseek = collector.is_real_home() && !deepseek_key().is_empty();
    let mut clients = vec!["claude", "codex", "agy"];
    if with_deepseek {
        clients.push("deepseek");
    }
    let results: Vec<(&str, Fetched)> = std::thread::scope(|scope| {
        let handles: Vec<_> =
            clients.iter().map(|client| (*client, scope.spawn(move || fetch(client, collector, cancel, timeout)))).collect();
        handles.into_iter().map(|(client, h)| (client, h.join().unwrap_or_else(|_| Err(value_err("CLI closed before replying"))))).collect()
    });
    for (client, result) in results {
        let provider = match client {
            "claude" => "anthropic",
            "codex" => "openai-codex",
            "agy" => "google-antigravity",
            _ => "deepseek",
        };
        match result {
            Ok((value, accounts)) => {
                usage.limits.retain(|r| r.provider != provider);
                usage.limits.extend(value);
                usage.accounts.retain(|a| a.provider != provider);
                usage.accounts.extend(accounts);
                usage.live = true;
            }
            Err(UsageError::Cancelled) => return Err(UsageError::Cancelled),
            Err(error) if error.safe() == tr("Official CLI not installed") => {
                // Not using a provider is not an error: show a neutral row only.
                if !usage.limits.iter().any(|r| r.provider == provider && r.status != "unavailable") {
                    usage.limits.retain(|r| r.provider != provider);
                    usage.limits.push(unavailable(provider, &not_installed(client)));
                }
            }
            Err(error) => {
                // OS exception text can contain paths; provider stderr and RPC
                // error messages can contain identities. Show only safe reasons.
                let mut message = format!("{client}: {}", error.safe());
                if let Some(hint) = error_hint(client, &error) {
                    message.push_str(" → ");
                    message.push_str(&hint);
                }
                usage.errors.push(message);
                if !usage.limits.iter().any(|r| r.provider == provider && r.status != "unavailable") {
                    usage.limits.retain(|r| r.provider != provider);
                    usage.limits.push(unavailable(provider, "Limits unavailable; see source errors"));
                }
            }
        }
    }
    if cancel.is_set() {
        return Err(UsageError::Cancelled);
    }
    save_usage(collector, &mut usage);
    Ok(usage)
}

pub fn demo_usage() -> Usage {
    let n = now();
    let mut limits = vec![
        Limit::new("anthropic", "#demo01", "Claude", "5h", Some(0.38), n + 8200.0, n - 180.0, "Demo"),
        Limit::new("anthropic", "#demo01", &tr("All Claude models"), "week", Some(0.64), n + 3.0 * 86400.0, n - 180.0, "Demo"),
        Limit::new("openai-codex", "#demo02", "Codex", "5h", Some(0.91), n + 2400.0, n - 60.0, "Demo"),
        Limit::new("openai-codex", "#demo02", "Codex", "week", Some(0.73), n + 4.0 * 86400.0, n - 60.0, "Demo"),
        Limit::new("google-antigravity", "#demo03", "Gemini", "5h", Some(0.12), n + 13000.0, n - 240.0, "Demo"),
        Limit::new("google-antigravity", "#demo03", "Gemini", "week", Some(0.85), n + 2.0 * 86400.0, n - 240.0, "Demo"),
    ];
    limits.splice(
        2..2,
        [
            Limit::new("anthropic", "#demo04", "Claude", "5h", Some(0.72), n + 3.0 * 3600.0, n - 200.0, "Demo"),
            Limit::new("anthropic", "#demo04", &tr("All Claude models"), "week", Some(0.31), n + 5.0 * 86400.0, n - 200.0, "Demo"),
        ],
    );
    let account = |provider: &str, key: &str, email: &str, name: &str, plan: &str, dirs: &[&str], expires: f64, s5: i64, s7: i64| Account {
        provider: provider.into(),
        key: key.into(),
        email: email.into(),
        name: name.into(),
        plan: plan.into(),
        dirs: dirs.iter().map(|d| d.to_string()).collect(),
        expires,
        sessions_5h: s5,
        sessions_7d: s7,
        ..Default::default()
    };
    Usage {
        limits,
        loaded: true,
        accounts: vec![
            account("anthropic", "#demo01", "dev@example.com", "Example Team", "Team", &["~/.claude (default)"], n + 20.0 * 86400.0, 3, 41),
            account("anthropic", "#demo04", "side@example.com", "", "Max", &["~/.claude-side"], n + 9.0 * 86400.0, 1, 12),
            account("openai-codex", "#demo02", "dev@example.com", "", "Plus", &["~/.codex"], 0.0, 2, 17),
            account("google-antigravity", "#demo03", "", "", "", &[], 0.0, 0, 0),
        ],
        ..Default::default()
    }
}

// ---------------------------------------------------------------- rendering

pub fn reset_text(value: f64, current: f64) -> String {
    if value == 0.0 {
        return tr("reset not recorded");
    }
    let seconds = (value - current) as i64;
    if seconds <= 0 {
        return tr("window expired · refresh");
    }
    if seconds >= 86400 {
        return trf!("resets in {v0}d {v1}h", v0 = seconds / 86400, v1 = seconds % 86400 / 3600);
    }
    if seconds >= 3600 {
        return trf!("resets in {v0}h {v1}m", v0 = seconds / 3600, v1 = seconds % 3600 / 60);
    }
    trf!("resets in {v0}m", v0 = (seconds / 60).max(1))
}

/// Local wall-clock time with its current UTC offset, e.g. 22:39:36 +03.
pub fn local_stamp(value: f64) -> String {
    let Some(zoned) = crate::model::local(value) else { return "—".into() };
    let offset = zoned.strftime("%z").to_string();
    let offset = if offset.ends_with("00") { offset[..3].to_string() } else { format!("{}:{}", &offset[..3], &offset[3..]) };
    format!("{} {offset}", zoned.strftime("%Y-%m-%d %H:%M:%S"))
}

pub fn snapshot_text(updated: f64) -> String {
    if updated == 0.0 {
        return tr("Snapshot time unknown");
    }
    trf!("Snapshot: {timestamp}", timestamp = local_stamp(updated))
}

fn wrap_indented(text: &str, maximum: i64) -> Vec<String> {
    let indent = text.len() - text.trim_start_matches(' ').len();
    text_wrap(text.trim(), maximum - indent as i64).into_iter().map(|row| format!("{}{row}", " ".repeat(indent))).collect()
}

pub fn usage_content(usage: &Usage, cols: i64, current: f64) -> Vec<(String, &'static str)> {
    let mut content: Vec<(String, &'static str)> = Vec::new();
    let extend = |content: &mut Vec<(String, &'static str)>, text: &str, style: &'static str| {
        content.extend(wrap_indented(text, cols - 4).into_iter().map(|t| (t, style)));
    };
    let mut previous: Option<(String, String)> = None;
    let multiple: std::collections::HashSet<&str> = usage
        .limits
        .iter()
        .map(|r| r.provider.as_str())
        .filter(|p| {
            usage
                .limits
                .iter()
                .filter(|r| r.provider == *p && r.account != "CLI")
                .map(|r| r.account.as_str())
                .collect::<std::collections::HashSet<_>>()
                .len()
                > 1
        })
        .collect();
    for r in &usage.limits {
        let group = (r.provider.clone(), r.account.clone());
        if previous.as_ref() != Some(&group) {
            if previous.is_some() {
                content.push((String::new(), "text"));
            }
            let account = usage.account(&r.provider, &r.account);
            let who = account.map(|a| a.title().to_string()).unwrap_or_else(|| r.account.clone());
            let mut heading = provider_name(&r.provider).to_string();
            if who != "CLI" {
                heading += &format!("  ·  {who}");
            }
            if let Some(account) = account {
                for v in [&account.name, &account.plan] {
                    if !v.is_empty() {
                        heading += &format!("  ·  {v}");
                    }
                }
            }
            let mut source = if r.status != "unavailable" { r.source.clone() } else { String::new() };
            let prefix = format!("{} · ", provider_name(&r.provider));
            if (cli_source(&r.provider).is_some() || provider_name(&r.provider) != r.provider)
                && let Some(rest) = source.strip_prefix(&prefix)
            {
                source = rest.to_string();
            }
            let suffix = if source.is_empty() { String::new() } else { format!("  ·  {}", tr(&source)) };
            extend(&mut content, &(heading + &suffix), "cyan");
            if multiple.contains(r.provider.as_str()) {
                let short = provider_name(&r.provider).rsplit(" / ").next().unwrap_or("").to_string();
                content.push((
                    "  ".to_string() + &trf!("separate account · limits are not shared with other {provider} accounts", provider = short),
                    "accent",
                ));
            }
            if let Some(account) = account {
                let mut facts = Vec::new();
                if !account.dirs.is_empty() {
                    facts.push(trf!("config: {dirs}", dirs = account.dirs.join(", ")));
                }
                if account.expires != 0.0 {
                    facts.push(trf!("login valid until {date}", date = strftime(account.expires, "%Y-%m-%d").unwrap_or_default()));
                }
                if !account.credits.is_empty() {
                    facts.push(account.credits.clone());
                }
                if !facts.is_empty() {
                    extend(&mut content, &format!("  {}", facts.join("  ·  ")), "muted");
                }
                if !account.dirs.is_empty() {
                    extend(
                        &mut content,
                        &format!(
                            "  {}",
                            trf!(
                                "local sessions — last 5h: {recent} · last 7 days: {week}",
                                recent = account.sessions_5h,
                                week = account.sessions_7d
                            )
                        ),
                        "muted",
                    );
                }
                if account.dirs.len() > 1 {
                    extend(
                        &mut content,
                        &format!("  {}", tr("these config dirs are signed in to the same account and share its limits")),
                        "muted",
                    );
                }
                if !account.warning.is_empty() {
                    content.push((format!("  ⚠ {}", account.warning), "red"));
                }
            }
            if r.updated != 0.0 {
                extend(&mut content, &format!("  {}", snapshot_text(r.updated)), "muted");
            }
            previous = Some(group);
        }
        if r.status == "unavailable" {
            extend(&mut content, &tr(&r.label), "amber");
            continue;
        }
        if r.status == "balance" || r.status == "balance-empty" {
            let empty = r.status == "balance-empty";
            extend(&mut content, &format!("  {} {}", ljust(&r.window, 6), r.label), if empty { "red" } else { "green" });
            if empty {
                content.push((format!("  ⚠ {}", tr("balance exhausted · top up to keep using the API")), "red"));
            }
            continue;
        }
        let expired = r.reset != 0.0 && r.reset <= current;
        let stale = current - r.updated > 3600.0 || expired;
        let style = match r.used {
            _ if stale => "muted",
            Some(u) if u >= 0.9 => "red",
            Some(u) if u >= 0.7 => "amber",
            _ => "green",
        };
        let bar_width: usize = if cols < 70 { 8 } else { 16 };
        let meter = match r.used {
            None => "░".repeat(bar_width) + &tr("  remaining usage unknown"),
            Some(used) => {
                let filled = ((used * bar_width as f64).round() as usize).min(bar_width);
                let percentages = if cols >= 70 {
                    trf!("  {v0:3.0f}% used · {v1:3.0f}% remaining", v0 = used * 100.0, v1 = (1.0 - used) * 100.0)
                } else {
                    trf!("  {used:.0f}% used · {remaining:.0f}% left", used = used * 100.0, remaining = (1.0 - used) * 100.0)
                };
                "█".repeat(filled) + &"░".repeat(bar_width - filled) + &percentages
            }
        };
        let mut reset = if r.reset != 0.0 {
            reset_text(r.reset, current)
        } else if !r.reset_label.is_empty() {
            r.reset_label.clone()
        } else {
            reset_text(r.reset, current)
        };
        if r.reset != 0.0 && r.reset > current {
            let format = if r.reset - current > 86400.0 { " (%a %H:%M)" } else { " (%H:%M)" };
            reset += &strftime(r.reset, format).unwrap_or_default();
        }
        if stale {
            reset += &tr(" · stale");
        }
        let prefix = format!("  {} ", ljust(&r.window, 6));
        if cols >= 120 {
            let label = clip(&tr(&r.label), 24, true);
            content.push((format!("{prefix}{}  {meter}  ·  {reset}", ljust(&label, 24)), style));
        } else {
            content.push((prefix + &tr(&r.label), "text"));
            content.push((format!("    {meter}"), style));
            content.push((format!("    {reset}"), "muted"));
        }
        if !stale && let Some((text, style)) = pace(r, current) {
            content.push((format!("{}↳ {text}", if cols >= 120 { " ".repeat(41) } else { "    ".into() }), style));
        }
    }
    for report in &usage.reports {
        content.push((String::new(), "text"));
        let source = if report.cached { tr("agy CLI · cached") } else { tr("agy CLI · live") };
        extend(&mut content, &format!("{} · {source}", provider_name(&report.provider)), "cyan");
        extend(&mut content, &snapshot_text(report.updated), "muted");
        if current - report.updated > 3600.0 {
            content.push((tr(" · stale").trim().to_string(), "amber"));
        }
        extend(&mut content, &report.text, "text");
    }
    if !usage.loaded {
        content = vec![(tr("Loading local usage snapshots…"), "muted")];
    } else if usage.limits.is_empty() && usage.reports.is_empty() {
        content = vec![
            (tr("No official CLI limits available."), "text"),
            (tr("Remaining usage cannot be calculated from session tokens."), "muted"),
        ];
    }
    if usage.limits.iter().any(|r| r.source.ends_with(" · cached")) || usage.reports.iter().any(|r| r.cached) {
        extend(&mut content, &tr("Cached snapshots may belong to a previous CLI account."), "muted");
    }
    if !usage.errors.is_empty() {
        content.push((String::new(), "text"));
        content.push((tr("SOURCE ERRORS"), "amber"));
        for error in &usage.errors {
            extend(&mut content, error, "amber");
        }
    }
    content
}

pub fn usage_viewport(cols: usize, rows: usize) -> usize {
    let header = if rows >= 26 && cols >= 70 { 5 } else { 4 };
    (rows as i64 - 5 - (header + 5)).max(1) as usize
}

pub fn max_usage_scroll(usage: &Usage, cols: usize, rows: usize) -> usize {
    usage_content(usage, cols as i64, now()).len().saturating_sub(usage_viewport(cols, rows))
}

pub fn render_usage(base: &mut Screen, usage: &Usage, scroll: usize, refreshing: bool, message: &str) {
    let (cols, rows) = (base.cols as i64, base.rows as i64);
    if cols < 44 || rows < 16 {
        return;
    }
    let header = if rows >= 26 && cols >= 70 { 5 } else { 4 };
    base.fill(header, 0, cols, false);
    base.put_limit(header, 2, &tr("◉ Official CLIs · current signed-in accounts"), "cyan", cols - 4);
    for y in header + 2..rows {
        base.fill(y, 0, cols, false);
    }
    base.put_bold(header + 2, 2, &if cols >= 70 { tr("USAGE · PROVIDER LIMITS") } else { tr("USAGE · LIMITS") }, "accent");
    base.put_limit(header + 3, 2, &tr("Used and remaining · shared account quotas, independent of cwd"), "muted", cols - 4);
    base.rule(header + 4);
    let (start, bottom) = (header + 5, rows - 5);
    let content = usage_content(usage, cols, now());
    let scroll = scroll.min(content.len().saturating_sub(usage_viewport(cols as usize, rows as usize)));
    if content.len() as i64 > bottom - start {
        let indicator = if cols >= 80 { format!("↑↓ / PgUp PgDn · {}", scroll + 1) } else { format!("↑↓ {}", scroll + 1) };
        base.put(header + 2, cols - width(&indicator) as i64 - 2, &indicator, "muted");
    }
    for (i, (text, style)) in content[scroll..].iter().enumerate() {
        if start + i as i64 >= bottom {
            break;
        }
        base.put_limit(start + i as i64, 2, text, style, cols - 4);
    }
    base.rule(rows - 5);
    base.put_limit(rows - 4, 2, &tr("↑↓ scroll   R refresh   rr restart   Esc / Backspace back   Q quit"), "text", cols - 4);
    base.put_limit(rows - 3, 2, &tr("One account may be used by multiple CLIs; limits are not added together."), "muted", cols - 4);
    base.put_limit(rows - 2, 2, &tr("Without a snapshot, remaining usage is unknown."), "muted", cols - 4);
    let mut footer =
        if refreshing { tr("Fetching current limits via official CLIs…") } else { tr("Official CLI snapshots · R refresh") };
    if usage.checked != 0.0 && !refreshing {
        footer = trf!("Last check: {timestamp}", timestamp = local_stamp(usage.checked));
    }
    let shown = if message.is_empty() { footer } else { message.to_string() };
    base.put_limit(rows - 1, 2, &shown, if !message.is_empty() || refreshing { "amber" } else { "faint" }, cols - 4);
}

fn strip_name(provider: &str) -> &str {
    match provider {
        "anthropic" => "Claude",
        "openai-codex" => "Codex",
        "google-antigravity" => "agy",
        "deepseek" => "DeepSeek",
        other => other,
    }
}

fn strip_style(provider: &str) -> &'static str {
    match provider {
        "anthropic" => "amber",
        "openai-codex" => "blue",
        "google-antigravity" => "accent",
        "deepseek" => "cyan",
        _ => "text",
    }
}

fn compact_reset(limit: &Limit, current: f64) -> String {
    let left = if limit.reset != 0.0 { limit.reset - current } else { 0.0 };
    if left <= 0.0 {
        return String::new();
    }
    if left >= 86400.0 {
        return format!("{}d", (left / 86400.0) as i64);
    }
    if left >= 3600.0 {
        return format!("{}h", (left / 3600.0) as i64);
    }
    format!("{}m", ((left / 60.0) as i64).max(1))
}

/// Narrow, graphical summary: one header per account, one bar per window.
pub fn usage_strip_lines(usage: &Usage, strip_width: i64, current: f64) -> Vec<Vec<Part>> {
    let mut groups: Vec<(String, String)> = Vec::new();
    for r in &usage.limits {
        let group = (r.provider.clone(), r.account.clone());
        if !groups.contains(&group) {
            groups.push(group);
        }
    }
    let per_provider = |provider: &str| groups.iter().filter(|(p, _)| p == provider).count();
    let mut rows: Vec<Vec<Part>> = Vec::new();
    for (provider, key) in &groups {
        let items: Vec<&Limit> = usage.limits.iter().filter(|r| &r.provider == provider && &r.account == key).collect();
        let mut tag = String::new();
        if let Some(account) = usage.account(provider, key) {
            if per_provider(provider) > 1 && !account.email.is_empty() {
                tag = account.email.split('@').next().unwrap_or("").to_string();
            }
            tag = [account.plan.clone(), tag].into_iter().filter(|v| !v.is_empty()).collect::<Vec<_>>().join(" · ");
        }
        if !rows.is_empty() {
            rows.push(Vec::new());
        }
        let mut heading = vec![(strip_name(provider).to_string(), strip_style(provider), true)];
        if !tag.is_empty() {
            heading.push(part(format!(" {tag}"), "muted"));
        }
        rows.push(heading);
        if items.iter().all(|r| r.status == "unavailable") {
            rows.push(vec![part(format!("  {}", tr("unavailable")), "faint")]);
            continue;
        }
        for r in &items {
            if r.status == "balance" || r.status == "balance-empty" {
                let total = r.label.split("  ·  ").next().unwrap_or("").to_string();
                let empty = r.status == "balance-empty";
                let mut row = vec![part(format!("  {total}"), if empty { "red" } else { "green" })];
                if empty {
                    row.push(part(" ⚠", "red"));
                }
                rows.push(row);
            }
        }
        for (window, short) in [("5h", "5h"), ("week", "wk")] {
            let candidates: Vec<&&Limit> =
                items.iter().filter(|r| r.window == window && r.used.is_some() && r.status != "unavailable").collect();
            let Some(r) = candidates.iter().copied().reduce(|best, item| if item.used.unwrap() > best.used.unwrap() { item } else { best })
            else {
                continue;
            };
            let stale = current - r.updated > 3600.0 || (r.reset != 0.0 && r.reset <= current);
            let used = r.used.unwrap().clamp(0.0, 1.0);
            let style = if stale {
                "muted"
            } else if used >= 0.9 {
                "red"
            } else if used >= 0.7 {
                "amber"
            } else {
                "green"
            };
            let reset = compact_reset(r, current);
            let bar = (strip_width - 3 - 5 - 4).max(4) as usize;
            let filled = ((used * bar as f64).round() as usize).min(bar);
            rows.push(vec![
                part(format!("{short} "), "muted"),
                part("█".repeat(filled), style),
                part("░".repeat(bar - filled), "faint"),
                part(format!("{:>4.0}%", used * 100.0), style),
                part(if reset.is_empty() { String::new() } else { format!(" {}", rjust(&reset, 3)) }, "faint"),
            ]);
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lenient_json_repairs_lone_surrogates() {
        let broken = r#"{"a": "cut \ud83d", "b": "ok \ud83d\ude00", "c": "lone \udc00", "d": "\\ud83d"}"#;
        assert!(serde_json::from_str::<Value>(broken).is_err());
        let value = lenient_json(broken).unwrap();
        assert_eq!(value["a"], "cut \u{fffd}");
        assert_eq!(value["b"], "ok 😀");
        assert_eq!(value["c"], "lone \u{fffd}");
        assert_eq!(value["d"], "\\ud83d");
    }

    #[test]
    fn trusted_dir_never_picks_home() {
        let root = std::env::temp_dir().join(format!("ais-trust-{}", std::process::id()));
        let home = root.join("home");
        let project = home.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let config = root.join("claude.json");
        let body = serde_json::json!({"projects": {
            root.to_string_lossy(): {"hasTrustDialogAccepted": true},
            home.to_string_lossy(): {"hasTrustDialogAccepted": true},
            project.to_string_lossy(): {"hasTrustDialogAccepted": true},
        }});
        std::fs::write(&config, body.to_string()).unwrap();
        assert_eq!(trusted_dir(&config, &home, &home), project);
        assert_eq!(trusted_dir(&config, &project, &home), project);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn native_reset_text_becomes_epoch_only_for_exact_forms() {
        let base = parse_iso("2025-11-14T12:00:00Z").unwrap();
        let soon = parse_reset_text("Resets 5:20pm (Europe/Berlin)", Some(base));
        assert_eq!(soon, parse_iso("2025-11-14T16:20:00Z").unwrap());
        let later = parse_reset_text("Resets Nov 15 at 6am (Europe/Berlin)", Some(base));
        assert_eq!(later, parse_iso("2025-11-15T05:00:00Z").unwrap());
        let wrapped = parse_reset_text("Resets 9am (UTC)", Some(base));
        assert_eq!(wrapped, parse_iso("2025-11-15T09:00:00Z").unwrap());
        assert_eq!(parse_reset_text("Resets soon", Some(base)), 0.0);
        assert_eq!(parse_reset_text("Resets 5pm (Mars/Base)", Some(base)), 0.0);
    }

    #[test]
    fn pace_projects_running_out_before_reset() {
        let n = 1_000_000.0;
        let mut limit = Limit::new("anthropic", "CLI", "Claude", "5h", Some(0.8), n + 3.0 * 3600.0, n, "x");
        assert_eq!(pace(&limit, n).unwrap().1, "red");
        limit.used = Some(0.3);
        assert_eq!(pace(&limit, n).unwrap().1, "green");
        limit.used = None;
        assert!(pace(&limit, n).is_none());
    }

    #[test]
    fn codex_public_schema_multiple_buckets() {
        let payload = json!({"accountId": "acc", "rateLimits": {"primary": {"usedPercent": 99}},
            "rateLimitsByLimitId": {"codex": {"limitName": "Codex", "primary": {"usedPercent": 25, "windowDurationMins": 300, "resetsAt": 1900000000},
                                              "secondary": {"usedPercent": 101, "windowDurationMins": 10080}}}});
        let limits = parse_codex_usage(&payload, 1.0).unwrap();
        assert_eq!(limits.len(), 2);
        assert_eq!((limits[0].window.as_str(), limits[0].used), ("5h", Some(0.25)));
        assert_eq!((limits[1].window.as_str(), limits[1].used), ("week", None));
        assert!(limits[0].account.starts_with('#'));
        assert!(parse_codex_usage(&json!({}), 1.0).is_err());
    }

    #[test]
    fn agy_native_tsv_remaining_percent() {
        let text = include_str!("../../tests/fixtures/agy_usage_1_5_9.tsv");
        let limits = parse_agy_usage(text, 1.0).unwrap();
        assert!(!limits.is_empty());
        assert!(limits.iter().all(|l| l.used.is_some() && l.reset > 0.0));
        assert!(limits.iter().all(|l| !l.label.to_lowercase().contains("claude")));
    }

    #[test]
    fn balance_parsing() {
        let payload = json!({"is_available": true, "balance_infos": [
            {"currency": "CNY", "total_balance": "0.00", "granted_balance": "0", "topped_up_balance": "0"},
            {"currency": "USD", "total_balance": "1234.5", "granted_balance": "0", "topped_up_balance": "1234.5"}]});
        let limits = parse_deepseek_balance(&payload, 1.0).unwrap();
        assert_eq!(limits.len(), 1);
        assert!(limits[0].label.starts_with("balance $1,234.50"));
        assert!(parse_deepseek_balance(&json!({"balance_infos": []}), 1.0).is_err());
    }

    #[test]
    fn demo_usage_renders_both_widths() {
        let usage = demo_usage();
        let wide = usage_content(&usage, 140, now());
        assert!(wide.iter().any(|(t, _)| t.contains("dev@example.com")));
        let narrow = usage_content(&usage, 60, now());
        assert!(narrow.len() > wide.len());
        assert!(!usage_strip_lines(&usage, 23, now()).is_empty());
    }
}
