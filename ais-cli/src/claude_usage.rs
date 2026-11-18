//! Read Claude's built-in interactive /usage command without a model turn.
//!
//! The official print command returns session costs instead of plan limits. A
//! private PTY keeps the supported interactive command available while preventing
//! its terminal controls from reaching the ais UI. User customizations are disabled.

use crate::i18n::tr;
use crate::text::clean;
use crate::trf;
use crate::usage::{Cancel, Env, UsageError};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use regex::Regex;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::OnceLock;
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub label: String,
    pub used_percent: f64,
    pub reset_text: String,
}

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).unwrap())
}

/// Read only native quota sections, choosing the last redraw of each one.
///
/// Verified with Claude Code 2.1.325's screen-reader Usage pane. The separate
/// local-session attribution percentages below it are not account limits.
pub fn native_usage_windows(screen: &str) -> Vec<Window> {
    static HEADER: OnceLock<Regex> = OnceLock::new();
    static SECTION: OnceLock<Regex> = OnceLock::new();
    static PERCENT: OnceLock<Regex> = OnceLock::new();
    let header = re(&HEADER, r"Settings\s+Status\s+Config\s+Usage\s+Stats");
    let frames: Vec<&str> = header.split(screen).collect();
    if frames.len() < 2 || !frames.last().unwrap().contains("Esc to cancel") {
        return Vec::new();
    }
    let frame = frames.last().unwrap();
    let section =
        re(&SECTION, r"(?i)(?P<label>Current session|Current week(?:[ \t]*\([^\n)]+\))?)\n(?P<progress>[^\n]*)\n(?P<reset>Resets[^\n]+)");
    let percent = re(&PERCENT, r"(?i)(\d+(?:\.\d+)?)%\s+used\b");
    let mut windows: indexmap::IndexMap<String, Window> = indexmap::IndexMap::new();
    for caps in section.captures_iter(frame) {
        let Some(found) = percent.captures(&caps["progress"]) else { continue };
        let value: f64 = found[1].parse().unwrap_or(-1.0);
        if !(0.0..=100.0).contains(&value) {
            continue;
        }
        let label = caps["label"].to_string();
        windows.insert(label.to_lowercase(), Window { label, used_percent: value, reset_text: caps["reset"].to_string() });
    }
    windows.into_values().collect()
}

fn refreshing(screen: &str) -> bool {
    // screen-reader redraws append changed rows without repeating the header.
    // A later completed footer supersedes the initial Refreshing marker.
    let pieces: Vec<&str> = screen.rsplitn(3, "Esc to cancel").collect();
    let latest = if pieces.len() > 1 { pieces[1] } else { screen };
    latest.contains("Refreshing")
}

fn kill_group(pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        // SAFETY: plain signal delivery to the private CLI's own session.
        unsafe {
            libc::killpg(pid as i32, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = pid;
}

pub fn read_claude_usage(binary: &Path, env: &Env, cwd: &Path, cancel: &Cancel, timeout: f64) -> Result<Vec<Window>, UsageError> {
    if cancel.is_set() {
        return Err(UsageError::Cancelled);
    }
    static LOGIN: OnceLock<Regex> = OnceLock::new();
    static TRUST: OnceLock<Regex> = OnceLock::new();
    static PROMPT: OnceLock<Regex> = OnceLock::new();
    static FAILED: OnceLock<Regex> = OnceLock::new();
    let login = re(&LOGIN, r"(?i)Not logged in|Please (?:run /login|log in)|Login required");
    let trust = re(&TRUST, r"(?i)Enter y/n:|Do you trust|Is this a project you created or one you trust");
    let prompt = re(&PROMPT, r"(?m)^(?:\$|❯|>)\s*$");
    let failed = re(&FAILED, r"(?i)failed to (?:fetch|load).*usage|error fetching usage|usage.*(?:unauthorized|forbidden)");

    let launch = || -> Result<_, UsageError> {
        let pair = native_pty_system()
            .openpty(PtySize { rows: 70, cols: 180, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| UsageError::Os(e.to_string()))?;
        let mut command = CommandBuilder::new(binary);
        command.args(["--safe-mode", "--ax-screen-reader", "--tools", ""]);
        command.env_clear();
        for (key, value) in env {
            command.env(key, value);
        }
        command.env("TERM", "xterm-256color");
        command.cwd(cwd);
        let child = pair.slave.spawn_command(command).map_err(|e| UsageError::Os(e.to_string()))?;
        drop(pair.slave);
        Ok((pair.master, child))
    };
    let (master, mut child) = launch()?;
    let pid = child.process_id();
    let mut writer = master.take_writer().map_err(|e| UsageError::Os(e.to_string()))?;
    let mut reader = master.try_clone_reader().map_err(|e| UsageError::Os(e.to_string()))?;
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buffer = vec![0u8; 65536];
        loop {
            match reader.read(&mut buffer) {
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
    let result = (|| {
        let deadline = Instant::now() + Duration::from_secs_f64(timeout);
        let mut raw: Vec<u8> = Vec::new();
        let mut sent = false;
        let mut last_change = Instant::now();
        let mut report: Option<Vec<Window>> = None;
        loop {
            if cancel.is_set() {
                return Err(UsageError::Cancelled);
            }
            let now = Instant::now();
            // Wait for the final redraw/network update to settle before taking
            // a snapshot. Never expose a partial loading frame as live usage.
            if let Some(found) = &report
                && now - last_change >= Duration::from_millis(350)
            {
                return Ok(found.clone());
            }
            if now >= deadline {
                return Err(if sent {
                    UsageError::Value(tr("Claude /usage did not provide plan limits"))
                } else {
                    UsageError::Timeout(tr("CLI request timed out"))
                });
            }
            let wait = Duration::from_millis(100).min(deadline - now);
            let chunk = match rx.recv_timeout(wait) {
                Ok(chunk) => chunk,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => Vec::new(),
            };
            if chunk.is_empty() {
                if let Some(found) = report {
                    return Ok(found);
                }
                if let Ok(Some(status)) = child.try_wait()
                    && !status.success()
                {
                    return Err(UsageError::Value(trf!("CLI exited with code {code}", code = status.exit_code() as i64)));
                }
                return Err(UsageError::Value(tr("Claude /usage did not provide plan limits")));
            }
            last_change = Instant::now();
            raw.extend_from_slice(&chunk);
            if raw.len() > 2 * 1024 * 1024 {
                return Err(UsageError::Value(tr("CLI response too large")));
            }
            let screen = clean(&String::from_utf8_lossy(&raw), 0);
            if login.is_match(&screen) {
                return Err(UsageError::Value(tr(
                    "Claude CLI cannot see a login in this environment; check claude auth status in the same terminal",
                )));
            }
            if trust.is_match(&screen) {
                return Err(UsageError::Value(tr("Claude requires workspace trust; open claude in this directory first")));
            }
            if !sent && prompt.is_match(&screen) {
                // The only input ever submitted is a documented built-in UI
                // command. No initial prompt, inference, login or auto-approval.
                writer.write_all(b"/usage\r").map_err(|e| UsageError::Os(e.to_string()))?;
                let _ = writer.flush();
                raw.clear();
                sent = true;
                continue;
            }
            if sent {
                if failed.is_match(&screen) {
                    return Err(UsageError::Value(tr("Claude could not load plan limits; check /usage in the official CLI")));
                }
                report = if refreshing(&screen) { None } else { Some(native_usage_windows(&screen)).filter(|w| !w.is_empty()) };
            }
        }
    })();
    kill_group(pid);
    let _ = child.kill();
    let _ = child.wait();
    drop(writer);
    drop(master);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_capture_preserves_plan_windows() {
        let screen = clean(include_str!("../../tests/fixtures/claude_usage_2_1_325.txt"), 0);
        let windows = native_usage_windows(&screen);
        assert!(!windows.is_empty());
        assert!(windows.iter().any(|w| w.label.eq_ignore_ascii_case("current session")));
        assert!(windows.iter().all(|w| w.reset_text.starts_with("Resets")));
    }

    #[test]
    fn partial_or_nonquota_frames_are_rejected() {
        assert!(native_usage_windows("Current session\n5% used\nResets 5pm").is_empty());
        assert!(native_usage_windows("Settings Status Config Usage Stats\nCurrent session\n5% used\nResets 5pm\n").is_empty());
        let ok = "Settings Status Config Usage Stats\nCurrent session\n▌ 5% used\nResets 5pm (Europe/Berlin)\nEsc to cancel";
        assert_eq!(native_usage_windows(ok)[0].used_percent, 5.0);
    }
}
