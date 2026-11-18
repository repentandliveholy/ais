use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CliKind {
    Omp,
    Claude,
    Codex,
    Agy,
    Gemini,
    Pi,
    Shell,
}

impl CliKind {
    pub const ALL: [CliKind; 6] = [
        CliKind::Omp,
        CliKind::Claude,
        CliKind::Codex,
        CliKind::Agy,
        CliKind::Gemini,
        CliKind::Pi,
    ];

    pub const fn index(self) -> usize {
        match self {
            CliKind::Omp => 0,
            CliKind::Claude => 1,
            CliKind::Codex => 2,
            CliKind::Agy => 3,
            CliKind::Gemini => 4,
            CliKind::Pi => 5,
            CliKind::Shell => 6,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            CliKind::Omp => "OMP",
            CliKind::Claude => "Claude",
            CliKind::Codex => "Codex",
            CliKind::Agy => "agy",
            CliKind::Gemini => "Gemini",
            CliKind::Pi => "Pi",
            CliKind::Shell => "Terminal",
        }
    }

    pub const fn command(self) -> &'static str {
        match self {
            CliKind::Omp => "omp",
            CliKind::Claude => "claude",
            CliKind::Codex => "codex",
            CliKind::Agy => "agy",
            CliKind::Gemini => "gemini",
            CliKind::Pi => "pi",
            CliKind::Shell => "shell",
        }
    }

    pub const fn install_url(self) -> &'static str {
        match self {
            CliKind::Omp => "https://github.com/can1357/oh-my-pi",
            CliKind::Claude => "https://docs.anthropic.com/en/docs/claude-code",
            CliKind::Codex => "https://github.com/openai/codex",
            CliKind::Agy => "https://antigravity.google",
            CliKind::Gemini => "https://github.com/google-gemini/gemini-cli",
            CliKind::Pi => "https://github.com/badlogic/pi-mono",
            CliKind::Shell => "",
        }
    }

    /// Brand colors shared with the curses `ais` (OMP mint, Claude amber,
    /// Codex blue, agy lavender, Gemini cyan, Pi pink).
    pub const fn tint(self) -> [u8; 3] {
        match self {
            CliKind::Omp => [149, 225, 180],
            CliKind::Claude => [243, 202, 134],
            CliKind::Codex => [149, 187, 255],
            CliKind::Agy => [185, 163, 255],
            CliKind::Gemini => [115, 216, 244],
            CliKind::Pi => [242, 145, 166],
            CliKind::Shell => [176, 184, 198],
        }
    }

    /// Explicit vivid, high-contrast tints for light themes instead of scaling dark tints down.
    pub const fn light_tint(self) -> [u8; 3] {
        match self {
            CliKind::Omp => [16, 126, 78],
            CliKind::Claude => [178, 92, 12],
            CliKind::Codex => [28, 92, 204],
            CliKind::Agy => [102, 62, 198],
            CliKind::Gemini => [10, 118, 152],
            CliKind::Pi => [186, 42, 82],
            CliKind::Shell => [84, 92, 108],
        }
    }

    /// Identifier used by `ais --client` and in its JSON output.
    pub const fn id(self) -> &'static str {
        match self {
            CliKind::Omp => "omp",
            CliKind::Claude => "claude",
            CliKind::Codex => "codex",
            CliKind::Agy => "agy",
            CliKind::Gemini => "gemini",
            CliKind::Pi => "pi",
            CliKind::Shell => "shell",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        CliKind::ALL.into_iter().find(|kind| kind.id() == id)
    }

    /// Arguments that continue the most recent conversation in the cwd.
    pub const fn continue_args(self) -> Option<&'static [&'static str]> {
        match self {
            CliKind::Omp => Some(&["--continue"]),
            CliKind::Claude => Some(&["--continue"]),
            CliKind::Codex => Some(&["resume", "--last"]),
            CliKind::Gemini => Some(&["--resume", "latest"]),
            CliKind::Pi => Some(&["--continue"]),
            CliKind::Agy | CliKind::Shell => None,
        }
    }

    pub const fn is_ai(self) -> bool {
        !matches!(self, CliKind::Shell)
    }

    pub fn installed_with(self, override_cmd: Option<&str>) -> bool {
        let cmd = override_cmd
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.command());
        which(cmd).is_some()
    }
}

/// The user's home directory (`$HOME` on Unix, `%USERPROFILE%` on Windows).
pub fn home_dir() -> PathBuf {
    #[cfg(windows)]
    let var = env::var_os("USERPROFILE").or_else(|| env::var_os("HOME"));
    #[cfg(not(windows))]
    let var = env::var_os("HOME");
    var.map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

/// Canonicalizes a path consistently when possible (stripping Windows `\\?\`).
pub fn canonical_path(path: &Path) -> PathBuf {
    let resolved = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    #[cfg(windows)]
    {
        let text = resolved.to_string_lossy();
        if let Some(rest) = text.strip_prefix(r"\\?\")
            && !rest.starts_with("UNC")
        {
            return PathBuf::from(rest);
        }
    }
    resolved
}

/// Hides the console window when spawning background helper commands on Windows.
pub fn no_window(command: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command
}

/// Seconds since the Unix epoch → `SystemTime`. `None` for zero, negative,
/// non-finite or absurdly large values instead of a panic on bad input.
pub fn unix_time(seconds: f64) -> Option<std::time::SystemTime> {
    if seconds.is_nan() || seconds <= 0.0 {
        return None;
    }
    let offset = std::time::Duration::try_from_secs_f64(seconds).ok()?;
    std::time::UNIX_EPOCH.checked_add(offset)
}

pub fn which(command: &str) -> Option<PathBuf> {
    let expanded = if let Some(rest) = command.strip_prefix("~/").or_else(|| command.strip_prefix("~\\")) {
        home_dir().join(rest)
    } else {
        PathBuf::from(command)
    };
    if expanded.components().count() > 1 {
        return executable(&expanded).then_some(expanded);
    }
    let path = env::var_os("PATH")?;
    #[cfg(windows)]
    let extensions: Vec<String> = env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
        .split(';')
        .map(|s| s.to_ascii_lowercase())
        .collect();
    for dir in env::split_paths(&path) {
        #[cfg(windows)]
        if let Some(found) = windows_candidate(&dir, command, &extensions) {
            return Some(found);
        }
        #[cfg(not(windows))]
        {
            let direct = dir.join(command);
            if executable(&direct) {
                return Some(direct);
            }
        }
    }
    None
}

/// npm installs `codex` (a POSIX shell script), `codex.cmd` and `codex.ps1`
/// side by side. Prefer `PATHEXT` matches and accept a bare name only when it
/// already carries an executable extension.
#[cfg(any(windows, test))]
fn windows_candidate(dir: &Path, binary: &str, extensions: &[String]) -> Option<PathBuf> {
    for ext in extensions {
        if ext.is_empty() {
            continue;
        }
        let with = dir.join(format!("{binary}{ext}"));
        if with.is_file() {
            return Some(with);
        }
    }
    let full = dir.join(binary);
    let lower = binary.to_ascii_lowercase();
    (full.is_file()
        && extensions
            .iter()
            .any(|ext| !ext.is_empty() && lower.ends_with(ext.as_str())))
    .then_some(full)
}

fn executable(path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_time_rejects_bad_input() {
        assert!(unix_time(1_700_000_000.5).is_some());
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY, 1e300] {
            assert!(unix_time(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn cli_ids_round_trip() {
        for kind in CliKind::ALL {
            assert_eq!(CliKind::from_id(kind.id()), Some(kind));
        }
        assert_eq!(CliKind::from_id("shell"), None);
    }

    #[test]
    fn windows_which_skips_npm_shell_shim() {
        let dir = std::env::temp_dir().join(format!("ais-shell-which-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        for name in ["codex", "codex.cmd", "codex.ps1", "tool.exe", "script"] {
            fs::write(dir.join(name), "").unwrap();
        }
        let exts: Vec<String> = [".com", ".exe", ".bat", ".cmd"]
            .into_iter()
            .map(str::to_string)
            .collect();
        assert_eq!(windows_candidate(&dir, "codex", &exts), Some(dir.join("codex.cmd")));
        assert_eq!(windows_candidate(&dir, "tool.exe", &exts), Some(dir.join("tool.exe")));
        assert_eq!(windows_candidate(&dir, "tool", &exts), Some(dir.join("tool.exe")));
        assert_eq!(windows_candidate(&dir, "script", &exts), None);
        let _ = fs::remove_dir_all(dir);
    }
}
