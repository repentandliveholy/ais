//! ais · AI Sessions — browse local AI CLI history and resume any session.

mod actions;
mod adapters;
mod claude_usage;
mod cli;
mod i18n;
mod model;
mod screen;
mod storage;
mod text;
mod ui;
mod update;
mod usage;
mod warp;

/// Re-run the same executable with the same arguments (the `rr` key).
pub fn restart() -> ! {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let exe = std::env::current_exe().unwrap_or_else(|_| "ais".into());
    let mut command = std::process::Command::new(exe);
    command.args(args);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = command.exec();
        eprintln!("ais: {error}");
        std::process::exit(1);
    }
    #[cfg(not(unix))]
    {
        let code = command.status().ok().and_then(|s| s.code()).unwrap_or(1);
        std::process::exit(code);
    }
}

fn main() {
    // A panic must never leave the terminal in raw/alternate-screen mode.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // `ais --list | head` closes stdout early: end quietly like other CLIs.
        let message = info
            .payload()
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| info.payload().downcast_ref::<&str>().copied())
            .unwrap_or("");
        if message.contains("Broken pipe") || message.contains("os error 32") || message.contains("os error 232") {
            std::process::exit(0);
        }
        ui::leave_terminal_if_active();
        default_hook(info);
    }));
    let args: Vec<String> = std::env::args().collect();
    match cli::main(args) {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("ais: {}", text::line(&error, 0));
            std::process::exit(1);
        }
    }
}
