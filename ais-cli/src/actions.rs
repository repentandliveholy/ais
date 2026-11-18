//! Small desktop side effects: clipboard and revealing a folder.
//!
//! Every helper is best-effort and never panics into the TUI loop.

use crate::model::which;
use base64::Engine;
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::process::{Command, Stdio};

/// Copy text; return the method used ("" when nothing worked).
pub fn copy_text(text: &str) -> String {
    let commands: &[&[&str]] =
        &[&["pbcopy"], &["wl-copy"], &["xclip", "-selection", "clipboard"], &["xsel", "--clipboard", "--input"], &["clip.exe"], &["clip"]];
    for command in commands {
        let Some(binary) = which(command[0]) else { continue };
        if command[0] == "wl-copy" && std::env::var_os("WAYLAND_DISPLAY").is_none() {
            continue;
        }
        if (command[0] == "xclip" || command[0] == "xsel") && std::env::var_os("DISPLAY").is_none() {
            continue;
        }
        let child = Command::new(binary).args(&command[1..]).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
        let Ok(mut child) = child else { continue };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        if child.wait().is_ok_and(|s| s.success()) {
            return command[0].to_string();
        }
    }
    if std::io::stdout().is_terminal() {
        // OSC 52 works over SSH and inside most modern terminals.
        let payload = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
        let mut out = std::io::stdout();
        let _ = write!(out, "\x1b]52;c;{payload}\x07");
        let _ = out.flush();
        return "osc52".into();
    }
    String::new()
}

/// Open a folder in the system file manager; return the opener's name.
pub fn reveal(path: &str) -> Result<String, String> {
    if path.is_empty() || !Path::new(path).is_dir() {
        return Err(path.to_string());
    }
    let openers: &[&str] = if cfg!(target_os = "macos") {
        &["open"]
    } else if cfg!(windows) {
        &["explorer"]
    } else {
        &["xdg-open", "gio"]
    };
    for opener in openers {
        let Some(binary) = which(opener) else { continue };
        let mut command = Command::new(binary);
        if *opener == "gio" {
            command.arg("open");
        }
        command.arg(path).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        if command.spawn().is_ok() {
            return Ok(opener.to_string());
        }
    }
    Err("open / xdg-open".into())
}
