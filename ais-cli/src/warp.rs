//! Launch a CLI through Warp's new-tab file URI handler on macOS.

use crate::text::{line, shell_join, shell_quote};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub fn warp_scheme() -> Option<String> {
    if !cfg!(target_os = "macos") || std::env::var("TERM_PROGRAM").as_deref() != Ok("WarpTerminal") {
        return None;
    }
    let focus = std::env::var("WARP_FOCUS_URL").unwrap_or_default();
    let preferred = focus.split_once("://").map(|(s, _)| s.to_lowercase()).unwrap_or_default();
    // Preserve the running channel even when its custom scheme is not registered.
    Some(if ["warp", "warposs", "warppreview", "warpterminal"].contains(&preferred.as_str()) { preferred } else { "warp".into() })
}

#[cfg(target_os = "macos")]
pub use imp::{open_tab, serve_tab};

#[cfg(not(target_os = "macos"))]
pub fn open_tab(_argv: &[String], _cwd: &str, _env: &[(String, String)], _remove: &[String]) -> Result<&'static str, String> {
    Err("Opening a Warp tab requires macOS and Warp".into())
}

#[cfg(not(target_os = "macos"))]
pub fn serve_tab(_path: &Path) -> Result<(), String> {
    Err("Warp tabs require macOS".into())
}

#[allow(dead_code)]
fn quote(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

#[allow(dead_code)]
fn tab_config(cwd: &str, script: &Path) -> String {
    let title = line(Path::new(cwd).file_name().map(|n| n.to_string_lossy().into_owned()).as_deref().unwrap_or(cwd), 0);
    let title = if title.is_empty() { cwd.to_string() } else { title };
    let command = shell_join(&["/bin/sh".to_string(), script.to_string_lossy().into_owned()]);
    format!(
        "name = {t}\ntitle = {t}\n\n[[panes]]\nid = \"root\"\nsplit = \"horizontal\"\nchildren = [\"main\", \"right\"]\n\n[[panes]]\nid = \"main\"\ntype = \"terminal\"\nis_focused = true\ndirectory = {d}\ncommands = [{c}]\n\n[[panes]]\nid = \"right\"\nsplit = \"vertical\"\nchildren = [\"top\", \"bottom\"]\n\n[[panes]]\nid = \"top\"\ntype = \"terminal\"\ndirectory = {d}\n\n[[panes]]\nid = \"bottom\"\ntype = \"terminal\"\ndirectory = {d}\n",
        t = quote(&title),
        d = quote(cwd),
        c = quote(&command)
    )
}

#[allow(dead_code)]
fn launch_script(cwd: &str, folder: &Path, exe: &Path, state: &Path) -> String {
    [
        "#!/bin/sh".to_string(),
        format!("cd -- {} || exit 1", shell_quote(cwd)),
        "rm -f -- \"$0\"".to_string(),
        format!("rmdir -- {}", shell_quote(&folder.to_string_lossy())),
        format!("exec {}", shell_join(&[exe.to_string_lossy().into_owned(), "--warp-serve".into(), state.to_string_lossy().into_owned()])),
    ]
    .join("\n")
        + "\n"
}

#[allow(dead_code)]
fn request(argv: &[String], cwd: &str, env: &[(String, String)]) -> Value {
    let keys = ["PATH", "CODEX_HOME", "CLAUDE_CONFIG_DIR", "PI_CODING_AGENT_DIR", "OMP_PROFILE"];
    let mut config = serde_json::Map::new();
    for key in keys {
        if let Some((_, v)) = env.iter().rev().find(|(k, _)| k == key) {
            config.insert(key.into(), Value::from(v.clone()));
        }
    }
    json!({"argv": argv, "cwd": cwd, "env": config})
}

#[allow(dead_code)]
fn unused(_: PathBuf) {}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use crate::model::now;
    use sha2::{Digest, Sha256};
    use std::fs::{File, OpenOptions};
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::process::Command;
    use std::time::Duration;

    fn find_warp_app(scheme: &str) -> Option<PathBuf> {
        let names: &[&str] = match scheme {
            "warposs" => &["WarpOss.app"],
            "warp" | "warpterminal" => &["Warp.app"],
            "warppreview" => &["WarpPreview.app"],
            _ => &[],
        };
        for root in [PathBuf::from("/Applications"), crate::i18n::home().join("Applications")] {
            for name in names {
                let app = root.join(name);
                let Ok(info) = plist::Value::from_file(app.join("Contents/Info.plist")) else { continue };
                let Some(info) = info.as_dictionary() else { continue };
                let schemes: Vec<String> = info
                    .get("CFBundleURLTypes")
                    .and_then(|v| v.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|e| e.as_dictionary()?.get("CFBundleURLSchemes")?.as_array().cloned())
                    .flatten()
                    .filter_map(|s| s.as_string().map(str::to_string))
                    .collect();
                // Locally packaged OSS builds may omit CFBundleURLTypes.
                // Explicit `open -a` still delivers the URL to the app.
                let matching_oss =
                    scheme == "warposs" && info.get("CFBundleIdentifier").and_then(|v| v.as_string()) == Some("dev.warp.WarpOss");
                let executable = info.get("CFBundleExecutable").and_then(|v| v.as_string()).unwrap_or("");
                if (schemes.iter().any(|s| s == scheme) || matching_oss)
                    && !executable.is_empty()
                    && app.join("Contents/MacOS").join(executable).is_file()
                {
                    return Some(app);
                }
            }
        }
        None
    }

    fn registry_path(cwd: &str, scheme: &str) -> Result<PathBuf, String> {
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        let root = std::env::temp_dir().join(format!("ais-warp-{uid}"));
        if !root.exists() {
            std::fs::create_dir(&root).map_err(|e| e.to_string())?;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
        }
        let meta = std::fs::metadata(&root).map_err(|e| e.to_string())?;
        if meta.uid() != uid || meta.mode() & 0o077 != 0 {
            return Err("Warp tab registry must be private".into());
        }
        let real = crate::model::realpath(Path::new(cwd));
        let key = format!("{:x}", Sha256::digest(format!("{scheme}:{}", real.display()).as_bytes()));
        Ok(root.join(format!("{}.json", &key[..32])))
    }

    struct Lock(File);

    impl Drop for Lock {
        fn drop(&mut self) {
            // SAFETY: valid descriptor owned by self.
            unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
        }
    }

    fn lock(path: &Path) -> Result<Lock, String> {
        let file = OpenOptions::new().create(true).append(true).open(path.with_extension("lock")).map_err(|e| e.to_string())?;
        // SAFETY: valid descriptor; blocking exclusive lock.
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        Ok(Lock(file))
    }

    fn read_state(path: &Path) -> serde_json::Map<String, Value> {
        let Ok(text) = std::fs::read_to_string(path) else { return Default::default() };
        let Ok(Value::Object(state)) = serde_json::from_str::<Value>(&text) else { return Default::default() };
        if let Some(pid) = state.get("pid").and_then(Value::as_i64).filter(|p| *p != 0) {
            // SAFETY: signal 0 only checks that the process exists.
            if unsafe { libc::kill(pid as i32, 0) } != 0 {
                return Default::default();
            }
        } else if now() - state.get("created").and_then(Value::as_f64).unwrap_or(0.0) > 30.0 {
            return Default::default();
        }
        state
    }

    fn write_state(path: &Path, state: &serde_json::Map<String, Value>) -> Result<(), String> {
        let temporary = path.with_extension("tmp");
        std::fs::write(&temporary, Value::Object(state.clone()).to_string()).map_err(|e| e.to_string())?;
        std::fs::rename(&temporary, path).map_err(|e| e.to_string())
    }

    fn open_uri(uri: &str) -> Result<(), String> {
        let scheme = uri.split_once("://").map(|(s, _)| s).unwrap_or("");
        let app = find_warp_app(scheme).ok_or_else(|| format!("Cannot locate the Warp application for {scheme}"))?;
        let mut child = Command::new("/usr/bin/open")
            .arg("-a")
            .arg(&app)
            .arg(uri)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                if status.success() {
                    return Ok(());
                }
                let mut stderr = String::new();
                if let Some(mut pipe) = child.stderr.take() {
                    use std::io::Read;
                    let _ = pipe.read_to_string(&mut stderr);
                }
                let message = line(&stderr, 0);
                return Err(if message.is_empty() { "Warp could not open a tab".into() } else { message });
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                return Err("Warp did not respond within 10 seconds".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn tab_config_dir(scheme: &str) -> PathBuf {
        let directory = match scheme {
            "warposs" => ".warp-oss",
            "warppreview" => ".warp-preview",
            _ => ".warp",
        };
        crate::i18n::home().join(directory).join("tab_configs")
    }

    fn create_tab_config(cwd: &str, scheme: &str, script: &Path, key: &str) -> Result<String, String> {
        let directory = tab_config_dir(scheme);
        std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        let name = format!("ais-{key}");
        let path = directory.join(format!("{name}.toml"));
        std::fs::write(&path, tab_config(cwd, script)).map_err(|e| e.to_string())?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
        Ok(format!("{scheme}://tab_config/{name}"))
    }

    /// Open (or reuse) the project's Warp tab; "opened", "reused" or "focused".
    pub fn open_tab(argv: &[String], cwd: &str, env: &[(String, String)], _remove: &[String]) -> Result<&'static str, String> {
        let scheme = warp_scheme().ok_or("Opening a Warp tab requires macOS and Warp")?;
        let state_path = registry_path(cwd, &scheme)?;
        let wanted = request(argv, cwd, env);
        let _guard = lock(&state_path)?;
        let mut state = read_state(&state_path);
        if !state.is_empty() {
            let focus = state.get("focus_url").and_then(Value::as_str).unwrap_or("").to_string();
            if focus.is_empty() {
                return Err("Warp tab is still opening; try again in a moment".into());
            }
            // Never send a second CLI into an active agent's terminal input.
            if state.get("busy").is_some_and(|v| v.as_bool() == Some(true)) || state.contains_key("request") {
                open_uri(&focus)?;
                return Ok("focused");
            }
            open_uri(&focus)?;
            state.insert("request".into(), wanted);
            write_state(&state_path, &state)?;
            return Ok("reused");
        }
        let folder = tempfile::Builder::new().prefix("ais-warp-launch-").tempdir().map_err(|e| e.to_string())?.keep();
        let script = folder.join("launch.sh");
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let result = (|| -> Result<(), String> {
            std::fs::write(&script, launch_script(cwd, &folder, &exe, &state_path)).map_err(|e| e.to_string())?;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
            let mut fresh = serde_json::Map::new();
            fresh.insert("created".into(), Value::from(now()));
            fresh.insert("request".into(), wanted.clone());
            write_state(&state_path, &fresh)?;
            let stem = state_path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            open_uri(&create_tab_config(cwd, &scheme, &script, &stem)?)
        })();
        if let Err(error) = result {
            let _ = std::fs::remove_file(&state_path);
            let _ = std::fs::remove_file(&script);
            let _ = std::fs::remove_dir(&folder);
            return Err(error);
        }
        Ok("opened")
    }

    extern "C" fn ignore(_: libc::c_int) {}

    /// Keep one terminal per project available after a client exits.
    pub fn serve_tab(path: &Path) -> Result<(), String> {
        let focus_url = std::env::var("WARP_FOCUS_URL").unwrap_or_default();
        if focus_url.is_empty() {
            return Err("Warp did not supply the new tab session URL".into());
        }
        // A CLI runs in our foreground process group and receives Ctrl+C too.
        // SAFETY: installing a no-op handler; it resets to default in the child.
        unsafe {
            libc::signal(libc::SIGINT, ignore as libc::sighandler_t);
            libc::signal(libc::SIGQUIT, ignore as libc::sighandler_t);
        }
        {
            let _guard = lock(path)?;
            let mut state = read_state(path);
            state.insert("pid".into(), Value::from(std::process::id()));
            state.insert("focus_url".into(), Value::from(focus_url));
            state.insert("busy".into(), Value::from(false));
            write_state(path, &state)?;
        }
        let result = (|| -> Result<(), String> {
            loop {
                let wanted = {
                    let _guard = lock(path)?;
                    let mut state = read_state(path);
                    let wanted = state.remove("request");
                    if wanted.is_some() {
                        state.insert("busy".into(), Value::from(true));
                        write_state(path, &state)?;
                    }
                    wanted
                };
                let Some(wanted) = wanted else {
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                };
                let cwd = wanted.get("cwd").and_then(Value::as_str).unwrap_or(".").to_string();
                let argv: Vec<String> = wanted
                    .get("argv")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect();
                let title = line(Path::new(&cwd).file_name().map(|n| n.to_string_lossy().into_owned()).as_deref().unwrap_or(&cwd), 0);
                let _ = std::env::set_current_dir(&cwd);
                let real = crate::model::realpath(Path::new(&cwd));
                let mut out = std::io::stdout();
                let _ = write!(out, "\x1b]7;file://{}\x07\x1b]0;{title}\x07", real.display());
                let _ = out.flush();
                let saved = crate::cli::save_terminal();
                let mut command = Command::new(argv.first().cloned().unwrap_or_default());
                command.args(argv.iter().skip(1)).current_dir(&cwd).env_remove("OMP_PROFILE");
                for (key, value) in wanted.get("env").and_then(Value::as_object).into_iter().flatten() {
                    if let Some(value) = value.as_str() {
                        command.env(key, value);
                    }
                }
                match command.status() {
                    Ok(status) => {
                        let code = status.code().unwrap_or(-1);
                        println!("\n{title} · exit {code} · select another session in ais");
                    }
                    Err(error) => eprintln!("{error}"),
                }
                crate::cli::restore_terminal(saved);
                let _ = write!(out, "\x1b[0m\x1b[?25h\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l\x1b[?1049l\x1b]0;{title}\x07");
                let _ = out.flush();
                let _guard = lock(path)?;
                let mut state = read_state(path);
                state.insert("busy".into(), Value::from(false));
                write_state(path, &state)?;
            }
        })();
        if let Ok(_guard) = lock(path) {
            let _ = std::fs::remove_file(path);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_config_has_three_panes_and_quoted_values() {
        let text = tab_config("/tmp/my \"proj\"", Path::new("/tmp/x/launch.sh"));
        assert!(text.contains(r#"name = "my \"proj\"""#));
        assert!(text.contains(r#"commands = ["/bin/sh /tmp/x/launch.sh"]"#));
        assert_eq!(text.matches("[[panes]]").count(), 5);
    }

    #[test]
    fn launch_script_execs_the_worker() {
        let script = launch_script("/a b", Path::new("/t/f"), Path::new("/bin/ais"), Path::new("/t/s.json"));
        assert!(script.contains("cd -- '/a b' || exit 1"));
        assert!(script.ends_with("exec /bin/ais --warp-serve /t/s.json\n"));
    }
}
