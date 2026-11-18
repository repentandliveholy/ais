//! Self-update from the signed release manifest.
//!
//! `latest.json` lists one raw binary per target with its SHA-256. The
//! manifest bytes are signed with the release Ed25519 key; the public half is
//! compiled in. Nothing is installed unless the signature and hash both match.

use crate::i18n::{read_settings, tr, write_private};
use crate::model::now;
use crate::trf;
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

pub const SITE: &str = "https://ais.repentandliveholy.org";
pub const TARGET: &str = env!("AIS_TARGET");
/// Release signing key (public half). The private key never leaves the release machine/CI secret.
const PUBLIC_KEY: &str = include_str!("../release-key.pub");
const CHECK_INTERVAL: f64 = 24.0 * 3600.0;
const MAX_BINARY: u64 = 64 * 1024 * 1024;

static NOTICE: Mutex<Option<String>> = Mutex::new(None);

#[derive(Deserialize, Debug)]
pub struct Asset {
    pub url: String,
    pub sha256: String,
}

#[derive(Deserialize, Debug)]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub assets: HashMap<String, Asset>,
}

pub const GITHUB_LATEST: &str = "https://github.com/repentandliveholy/ais/releases/latest/download/latest.json";

/// The website first, then the GitHub release itself (same signed bytes).
fn manifest_urls() -> Vec<String> {
    match std::env::var("AIS_UPDATE_URL").ok().filter(|v| !v.is_empty()) {
        Some(url) => vec![url],
        None => vec![format!("{SITE}/releases/latest.json"), GITHUB_LATEST.to_string()],
    }
}

pub fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let core = text.trim().trim_start_matches('v').split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    Some((parts.next()??, parts.next().unwrap_or(Some(0))?, parts.next().unwrap_or(Some(0))?))
}

pub fn newer(candidate: &str, current: &str) -> bool {
    matches!((parse_version(candidate), parse_version(current)), (Some(a), Some(b)) if a > b)
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(Duration::from_secs(60)).user_agent(&format!("ais/{}", crate::cli::VERSION)).build()
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
    response.into_reader().take(limit + 1).read_to_end(&mut body).map_err(|e| e.to_string())?;
    if body.len() as u64 > limit {
        return Err("response too large".into());
    }
    Ok(body)
}

pub fn verify_manifest(body: &[u8], signature: &[u8], public_key: &str) -> Result<Manifest, String> {
    let engine = base64::engine::general_purpose::STANDARD;
    let key: [u8; 32] = engine.decode(public_key.trim()).map_err(|_| "bad public key")?.try_into().map_err(|_| "bad public key length")?;
    let key = VerifyingKey::from_bytes(&key).map_err(|_| "bad public key")?;
    let signature: [u8; 64] = engine
        .decode(String::from_utf8_lossy(signature).trim())
        .map_err(|_| "bad signature encoding")?
        .try_into()
        .map_err(|_| "bad signature length")?;
    key.verify(body, &Signature::from_bytes(&signature)).map_err(|_| "manifest signature does not match")?;
    serde_json::from_slice(body).map_err(|e| format!("bad manifest: {e}"))
}

pub fn fetch_manifest() -> Result<Manifest, String> {
    let mut last = String::from("no update source");
    for url in manifest_urls() {
        let attempt = get(&url, 1024 * 1024)
            .and_then(|body| get(&format!("{url}.sig"), 4096).map(|signature| (body, signature)))
            .and_then(|(body, signature)| verify_manifest(&body, &signature, PUBLIC_KEY));
        match attempt {
            Ok(manifest) => return Ok(manifest),
            Err(error) => last = format!("{url}: {error}"),
        }
    }
    Err(last)
}

fn state_path() -> PathBuf {
    crate::storage::default_cache(&crate::i18n::home()).join("update.json")
}

fn last_check() -> f64 {
    std::fs::read_to_string(state_path())
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("checked").and_then(Value::as_f64))
        .unwrap_or(0.0)
}

fn record_check() {
    let _ = write_private(&state_path(), serde_json::json!({"checked": now()}).to_string().as_bytes());
}

/// Builds run from a checkout (cargo target dirs) are never overwritten.
fn is_dev_build(exe: &Path) -> bool {
    exe.components().any(|c| c.as_os_str() == "target") && exe.components().any(|c| c.as_os_str() == "debug" || c.as_os_str() == "release")
}

pub enum Installed {
    UpToDate(String),
    Updated(String),
    Available(String, String),
}

/// Download, verify and atomically replace the running executable.
pub fn install(manifest: &Manifest) -> Result<Installed, String> {
    let current = crate::cli::VERSION;
    if !newer(&manifest.version, current) {
        return Ok(Installed::UpToDate(current.into()));
    }
    let asset = manifest.assets.get(TARGET).ok_or_else(|| format!("no build for {TARGET} in release {}", manifest.version))?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    if is_dev_build(&exe) {
        return Ok(Installed::Available(manifest.version.clone(), "development build".into()));
    }
    let dir = exe.parent().ok_or("executable has no parent directory")?;
    let body = get(&asset.url, MAX_BINARY)?;
    let digest = format!("{:x}", Sha256::digest(&body));
    if !digest.eq_ignore_ascii_case(asset.sha256.trim()) {
        return Err("downloaded binary does not match the signed checksum".into());
    }
    let mut temp = match tempfile::Builder::new().prefix(".ais-update-").tempfile_in(dir) {
        Ok(temp) => temp,
        Err(_) => return Ok(Installed::Available(manifest.version.clone(), dir.display().to_string())),
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
    Ok(Installed::Updated(manifest.version.clone()))
}

fn enabled() -> bool {
    if std::env::var_os("AIS_NO_UPDATE").is_some_and(|v| !v.is_empty()) {
        return false;
    }
    read_settings().get("auto_update") != Some(&Value::Bool(false))
}

pub fn take_notice() -> Option<String> {
    NOTICE.lock().ok()?.take()
}

/// Check at most once a day while the TUI runs; install quietly when possible.
pub fn background_check(_view: &mut crate::ui::View) {
    if !enabled() || now() - last_check() < CHECK_INTERVAL {
        return;
    }
    std::thread::spawn(|| {
        let Ok(manifest) = fetch_manifest() else { return };
        record_check();
        let notice = match install(&manifest) {
            Ok(Installed::Updated(version)) => trf!("ais updated to {version} · rr restarts into the new version", version = version),
            Ok(Installed::Available(version, _)) => trf!("ais {version} is available · run ais --update", version = version),
            _ => return,
        };
        if let Ok(mut slot) = NOTICE.lock() {
            *slot = Some(notice);
        }
    });
}

pub fn run_manual() -> Result<i32, String> {
    println!("{}", trf!("ais {version} · {target} · checking for updates…", version = crate::cli::VERSION, target = TARGET));
    let manifest = fetch_manifest()?;
    record_check();
    match install(&manifest)? {
        Installed::UpToDate(version) => println!("{}", trf!("Already up to date ({version})", version = version)),
        Installed::Updated(version) => println!("{}", trf!("Updated to {version}", version = version)),
        Installed::Available(version, reason) => {
            println!(
                "{}",
                trf!("ais {version} is available, but this copy cannot be replaced ({reason})", version = version, reason = reason)
            );
            println!("{}", tr("Reinstall with the install script or download it from the website."));
            return Ok(1);
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn versions_compare_numerically() {
        assert!(newer("0.10.0", "0.9.9"));
        assert!(newer("v1.0.0", "0.9.0"));
        assert!(!newer("0.4.0", "0.4.0"));
        assert!(!newer("garbage", "0.4.0"));
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
    fn embedded_key_is_valid() {
        let key = base64::engine::general_purpose::STANDARD.decode(PUBLIC_KEY.trim()).unwrap();
        assert!(VerifyingKey::from_bytes(&key.try_into().unwrap()).is_ok());
    }
}
