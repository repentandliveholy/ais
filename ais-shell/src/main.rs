#[cfg(target_os = "windows")]
compile_error!("ais-shell supports macOS and Linux (POSIX PTYs) only; Windows is not supported yet.");

mod app;
mod cli;
mod input;
mod keys;
mod log;
mod projects;
mod sessions;
mod terminal;
mod theme;
mod ui_chrome;
mod ui_dock;
mod ui_settings;
mod ui_sidebar;
mod ui_workspace;
mod usage;
mod view;
mod widgets;

use eframe::egui;

fn main() -> eframe::Result<()> {
    log::install_panic_hook();

    if std::env::args().any(|arg| arg == "--version" || arg == "-V") {
        println!("{} {}", app::APP_TITLE, env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if std::env::args().any(|arg| arg == "--help" || arg == "-h") {
        println!("{} {}\n\n{USAGE}", app::APP_TITLE, env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if let Err(message) = check_args(std::env::args().skip(1)) {
        eprintln!("ais-shell: {message}\n\n{USAGE}");
        std::process::exit(2);
    }

    import_login_env();

    let (settings, _) = theme::Settings::load_checked(&theme::settings_path(&cli::home_dir()));
    let initial_size = settings
        .window_size
        .map(|[w, h]| [w.clamp(760.0, 3840.0), h.clamp(480.0, 2400.0)])
        .unwrap_or([1440.0, 900.0]);

    let mut viewport = egui::ViewportBuilder::default()
        .with_title(app::APP_TITLE)
        .with_app_id("ais")
        .with_inner_size(initial_size)
        .with_min_inner_size([760.0, 480.0])
        .with_drag_and_drop(true);
    if let Some([x, y]) = settings.window_pos
        && x.is_finite()
        && y.is_finite()
        && (-2000.0..=6000.0).contains(&x)
        && (-2000.0..=4000.0).contains(&y)
    {
        viewport = viewport.with_position([x, y]);
    }
    if app::custom_titlebar() {
        // ais draws its own title bar (and window buttons) in the theme's colors.
        viewport = viewport
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false)
            .with_titlebar_buttons_shown(false);
    }
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-256.png")) {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        app::APP_NAME,
        options,
        Box::new(|cc| Ok(Box::new(app::AisApp::new(cc)))),
    )
}

const USAGE: &str = "\
Usage: ais-shell [PATH] [--run CLI]

  PATH           open this folder as a project space
  --run CLI      start a CLI in it right away:
                 omp, claude, codex, agy, gemini, pi or shell
  -h, --help     print this help
  -V, --version  print the version

Environment:
  AIS_BIN        path to the `ais` indexer (conversation lists, usage limits)
  AIS_FORCE_LOGIN_PATH  import login shell environment even when started from a terminal";

/// Rejects typos on the command line before a window opens.
fn check_args(args: impl Iterator<Item = String>) -> Result<(), String> {
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--run" => match args.next() {
                Some(id) if matches!(id.as_str(), "shell" | "terminal") || cli::CliKind::from_id(&id).is_some() => {}
                Some(id) => return Err(format!("unknown CLI for --run: {id}")),
                None => return Err("--run needs a CLI name".into()),
            },
            // macOS passes a process serial number to apps opened from Finder.
            flag if flag.starts_with("-psn_") => {}
            flag if flag.starts_with('-') => return Err(format!("unknown option: {flag}")),
            _ => {}
        }
    }
    Ok(())
}

/// Apps started from Finder/Dock get a minimal PATH (/usr/bin:/bin…) and miss
/// shell exports (API keys, nvm, pyenv). Ask the user's login shell for its
/// environment once, reading stdout concurrently so a chatty rc never deadlocks.
fn import_login_env() {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    if std::env::var_os("TERM").is_some() && std::env::var_os("AIS_FORCE_LOGIN_PATH").is_none() {
        return;
    }
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let Ok(mut child) = Command::new(&shell)
        .args(["-l", "-i", "-c", "printf '__AIS_ENV__'; env -0; printf '__AIS_END__'"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    let reader = child.stdout.take().and_then(|mut stdout| {
        std::thread::Builder::new()
            .name("ais-login-env".into())
            .spawn(move || {
                let mut bytes = Vec::new();
                let _ = stdout.read_to_end(&mut bytes);
                bytes
            })
            .ok()
    });

    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                log::warn("Timed out importing login shell environment");
                return;
            }
        }
    }
    let output = reader.and_then(|handle| handle.join().ok()).unwrap_or_default();
    let Some(start) = find_subslice(&output, b"__AIS_ENV__") else {
        return;
    };
    let rest = &output[start + b"__AIS_ENV__".len()..];
    let Some(end) = find_subslice(rest, b"__AIS_END__") else {
        return;
    };
    for entry in rest[..end].split(|&b| b == 0) {
        let Ok(line) = std::str::from_utf8(entry) else {
            continue;
        };
        let Some((key, val)) = line.split_once('=') else {
            continue;
        };
        if key.is_empty()
            || matches!(
                key,
                "TERM" | "COLORTERM" | "SHLVL" | "PWD" | "OLDPWD" | "_" | "AIS_BIN" | "AIS_FORCE_LOGIN_PATH"
            )
        {
            continue;
        }
        // SAFETY: called on the main thread before any worker thread is spawned.
        unsafe { std::env::set_var(key, val) };
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::check_args;

    fn check(args: &[&str]) -> Result<(), String> {
        check_args(args.iter().map(|arg| arg.to_string()))
    }

    #[test]
    fn command_line_is_validated() {
        assert!(check(&[]).is_ok());
        assert!(check(&["~/code/app", "--run", "claude"]).is_ok());
        assert!(check(&["--run", "shell"]).is_ok());
        assert!(check(&["-psn_0_12345"]).is_ok());
        assert!(check(&["--run"]).is_err());
        assert!(check(&["--run", "vim"]).is_err());
        assert!(check(&["--frobnicate"]).is_err());
    }
}
