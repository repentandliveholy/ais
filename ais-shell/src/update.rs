//! Self-update from the signed release manifest (`ais-gui-latest.json`).
//!
//! `ais-gui-latest.json` lists one binary per target with its SHA-256. The
//! manifest bytes are signed with the release Ed25519 key; the public half is
//! compiled in. Nothing is installed unless both signature and hash match.

use crate::cli::{home_dir, no_window};
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const SITE: &str = "https://ais.repentandliveholy.org";
pub const TARGET: &str = env!("AIS_TARGET");
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const PUBLIC_KEY: &str = include_str!("../release-key.pub");
const CHECK_INTERVAL_SECS: f64 = 24.0 * 3600.0;
const MAX_BINARY: u64 = 128 * 1024 * 1024;
pub const RELEASES_URL: &str = "https://github.com/repentandliveholy/ais/releases";
pub const GITHUB_LATEST: &str = "https://github.com/repentandliveholy/ais/releases/latest/download/ais-gui-latest.json";

#[derive(Debug)]
pub struct Asset {
    pub url: String,
    pub sha256: String,
}

#[derive(Debug)]
pub struct Manifest {
    pub version: String,
    pub assets: HashMap<String, Asset>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Installed {
    UpToDate(String),
    Updated(String),
    Available(String, String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateStatus {
    Idle,
    Checking,
    UpToDate(String),
    Updated(String),
    Available { version: String, reason: String },
    Error(String),
}

fn manifest_urls() -> Vec<String> {
    match std::env::var("AIS_GUI_UPDATE_URL")
        .or_else(|_| std::env::var("AIS_SHELL_UPDATE_URL"))
        .ok()
        .filter(|v| !v.is_empty())
    {
        Some(url) => vec![url],
        None => vec![
            format!("{SITE}/releases/ais-gui-latest.json"),
            GITHUB_LATEST.to_string(),
        ],
    }
}

pub fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let core = text.trim().trim_start_matches('v').split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    Some((
        parts.next()??,
        parts.next().unwrap_or(Some(0))?,
        parts.next().unwrap_or(Some(0))?,
    ))
}

pub fn newer(candidate: &str, current: &str) -> bool {
    matches!(
        (parse_version(candidate), parse_version(current)),
        (Some(a), Some(b)) if a > b
    )
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(60))
        .user_agent(&format!("ais/{VERSION}"))
        .build()
}

fn get(url: &str, limit: u64) -> Result<Vec<u8>, String> {
    if !url.starts_with("https://") && std::env::var_os("AIS_UPDATE_INSECURE").is_none() {
        return Err(format!("refusing non-HTTPS URL: {url}"));
    }
    let response = agent().get(url).call().map_err(|e| match e {
        ureq::Error::Status(code, _) => format!("HTTP {code}"),
        other => other.kind().to_string(),
    })?;
    let mut body = Vec::new();
    response
        .into_reader()
        .take(limit + 1)
        .read_to_end(&mut body)
        .map_err(|e| e.to_string())?;
    if body.len() as u64 > limit {
        return Err("response too large".into());
    }
    Ok(body)
}

pub fn verify_manifest(body: &[u8], signature: &[u8], public_key: &str) -> Result<Manifest, String> {
    let engine = base64::engine::general_purpose::STANDARD;
    let key: [u8; 32] = engine
        .decode(public_key.trim())
        .map_err(|_| "bad public key")?
        .try_into()
        .map_err(|_| "bad public key length")?;
    let key = VerifyingKey::from_bytes(&key).map_err(|_| "bad public key")?;
    let signature: [u8; 64] = engine
        .decode(String::from_utf8_lossy(signature).trim())
        .map_err(|_| "bad signature encoding")?
        .try_into()
        .map_err(|_| "bad signature length")?;
    key.verify(body, &Signature::from_bytes(&signature))
        .map_err(|_| "manifest signature does not match")?;
    let value: Value = serde_json::from_slice(body).map_err(|e| format!("bad manifest: {e}"))?;
    let version = value
        .get("version")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "bad manifest: missing version".to_string())?
        .to_string();
    let mut assets = HashMap::new();
    if let Some(map) = value.get("assets").and_then(Value::as_object) {
        for (target, entry) in map {
            let Some(url) = entry.get("url").and_then(Value::as_str) else {
                continue;
            };
            let Some(sha256) = entry.get("sha256").and_then(Value::as_str) else {
                continue;
            };
            assets.insert(
                target.clone(),
                Asset {
                    url: url.to_string(),
                    sha256: sha256.to_string(),
                },
            );
        }
    }
    Ok(Manifest { version, assets })
}

pub fn fetch_manifest() -> Result<Manifest, String> {
    let mut last = String::from("no update source");
    for url in manifest_urls() {
        let attempt = get(&url, 1024 * 1024)
            .and_then(|body| get(&format!("{url}.sig"), 4096).map(|sig| (body, sig)))
            .and_then(|(body, sig)| verify_manifest(&body, &sig, PUBLIC_KEY));
        match attempt {
            Ok(manifest) => return Ok(manifest),
            Err(error) => last = format!("{url}: {error}"),
        }
    }
    Err(last)
}

pub fn state_path() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home_dir().join(".cache"));
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home_dir().join(".cache"));
    base.join("ais/update.json")
}

fn last_check() -> f64 {
    std::fs::read_to_string(state_path())
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("checked").and_then(Value::as_f64))
        .unwrap_or(0.0)
}

fn record_check() {
    let path = state_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, serde_json::json!({"checked": now_secs()}).to_string());
}

pub fn should_auto_check(auto_update: bool) -> bool {
    if !auto_update {
        return false;
    }
    if std::env::var_os("AIS_NO_UPDATE").is_some_and(|v| !v.is_empty()) {
        return false;
    }
    now_secs() - last_check() >= CHECK_INTERVAL_SECS
}

/// Builds run from a cargo `target/` checkout are never overwritten.
pub fn is_dev_build(exe: &Path) -> bool {
    let text = exe.to_string_lossy();
    let mut parts = text.split(['/', '\\']);
    let mut has_target = false;
    let mut has_profile = false;
    for part in parts.by_ref() {
        if part == "target" {
            has_target = true;
        } else if part == "debug" || part == "release" {
            has_profile = true;
        }
    }
    has_target && has_profile
}

/// Download, verify and atomically replace the running `ais` executable.
pub fn install(manifest: &Manifest) -> Result<Installed, String> {
    if !newer(&manifest.version, VERSION) {
        return Ok(Installed::UpToDate(VERSION.into()));
    }
    let asset = manifest
        .assets
        .get(TARGET)
        .ok_or_else(|| format!("no build for {TARGET} in release {}", manifest.version))?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    if is_dev_build(&exe) {
        return Ok(Installed::Available(
            manifest.version.clone(),
            "development build".into(),
        ));
    }
    let dir = exe.parent().ok_or("executable has no parent directory")?;
    let body = get(&asset.url, MAX_BINARY)?;
    let digest = format!("{:x}", Sha256::digest(&body));
    if !digest.eq_ignore_ascii_case(asset.sha256.trim()) {
        return Err("downloaded binary does not match the signed checksum".into());
    }
    let mut temp = match tempfile::Builder::new().prefix(".ais-update-").tempfile_in(dir) {
        Ok(temp) => temp,
        Err(_) => {
            return Ok(Installed::Available(
                manifest.version.clone(),
                dir.display().to_string(),
            ));
        }
    };
    std::io::Write::write_all(&mut temp, &body).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    #[cfg(windows)]
    {
        self_replace::self_replace(temp.path()).map_err(|e| e.to_string())?;
    }
    #[cfg(not(windows))]
    {
        temp.persist(&exe).map_err(|e| e.error.to_string())?;
    }
    #[cfg(target_os = "macos")]
    resign_app_bundle(&exe);
    Ok(Installed::Updated(manifest.version.clone()))
}

#[cfg(target_os = "macos")]
fn resign_app_bundle(exe: &Path) {
    // When running from ais.app/Contents/MacOS/ais, keep the ad-hoc signature valid.
    let Some(macos_dir) = exe.parent() else { return };
    let Some(contents_dir) = macos_dir.parent() else { return };
    let Some(app_dir) = contents_dir.parent() else { return };
    if macos_dir.file_name().and_then(|n| n.to_str()) == Some("MacOS")
        && contents_dir.file_name().and_then(|n| n.to_str()) == Some("Contents")
        && app_dir.extension().and_then(|e| e.to_str()) == Some("app")
    {
        let _ = Command::new("codesign")
            .args(["--force", "--sign", "-"])
            .arg(app_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Also triggers `ais --update` when a companion `ais` CLI binary is available.
pub fn update_companion_cli(ais: &Path) -> Result<String, String> {
    let mut cmd = Command::new(ais);
    cmd.arg("--update")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    no_window(&mut cmd);
    let output = cmd.output().map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if output.status.success() {
        Ok(if stdout.is_empty() {
            "ais CLI checked".into()
        } else {
            stdout
        })
    } else {
        Err(if stderr.is_empty() { stdout } else { stderr })
    }
}

pub fn check_and_install(ais: Option<&Path>) -> Result<Installed, String> {
    let manifest = fetch_manifest()?;
    record_check();
    let result = install(&manifest)?;
    if let Some(ais) = ais {
        let _ = update_companion_cli(ais);
    }
    Ok(result)
}

pub fn run_manual() -> Result<i32, String> {
    println!("ais {VERSION} · {TARGET} · checking for updates…");
    let manifest = fetch_manifest()?;
    record_check();
    match install(&manifest)? {
        Installed::UpToDate(version) => {
            println!("Already up to date ({version})");
        }
        Installed::Updated(version) => {
            println!("Updated to {version}");
        }
        Installed::Available(version, reason) => {
            println!("ais {version} is available, but this copy cannot be replaced ({reason})");
            println!("Download the latest release from {RELEASES_URL}");
            return Ok(1);
        }
    }
    if let Some(ais) = crate::sessions::find_ais()
        && let Ok(msg) = update_companion_cli(&ais)
    {
        for line in msg.lines() {
            println!("ais-cli: {line}");
        }
    }
    Ok(0)
}

pub fn spawn_restart() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut cmd = Command::new(&exe);
    cmd.args(std::env::args_os().skip(1));
    cmd.spawn()
        .map_err(|e| format!("Cannot restart {}: {e}", exe.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn versions_compare_numerically() {
        assert!(newer("0.10.0", "0.9.9"));
        assert!(newer("v1.0.0", "0.9.0"));
        assert!(!newer("0.2.0", "0.2.0"));
        assert!(!newer("garbage", "0.2.0"));
    }

    #[test]
    fn manifest_requires_a_valid_signature() {
        let signing = SigningKey::from_bytes(&[7u8; 32]);
        let engine = base64::engine::general_purpose::STANDARD;
        let public = engine.encode(signing.verifying_key().to_bytes());
        let body = br#"{"version":"9.9.9","assets":{"x":{"url":"https://e/x","sha256":"00"}}}"#;
        let signature = engine.encode(signing.sign(body).to_bytes());
        let manifest = verify_manifest(body, signature.as_bytes(), &public).unwrap();
        assert_eq!(manifest.version, "9.9.9");
        let mut tampered = body.to_vec();
        tampered[13] = b'8';
        assert!(verify_manifest(&tampered, signature.as_bytes(), &public).is_err());
    }

    #[test]
    fn detects_dev_builds() {
        assert!(is_dev_build(Path::new("/repo/ais-shell/target/debug/ais-shell")));
        assert!(is_dev_build(Path::new(
            r"C:\repo\ais-shell\target\release\ais-shell.exe"
        )));
        assert!(!is_dev_build(Path::new("/Applications/ais.app/Contents/MacOS/ais")));
        assert!(!is_dev_build(Path::new(
            r"C:\Users\user\AppData\Local\ais\bin\ais-shell.exe"
        )));
    }
}
