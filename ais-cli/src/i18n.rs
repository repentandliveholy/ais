//! Interface language and the small, persistent user preference.

use indexmap::IndexMap;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};

pub const LANGUAGES: [&str; 3] = ["en", "ru", "fr"];

static LANGUAGE: RwLock<&'static str> = RwLock::new("en");

fn table(lang: &str) -> Option<&'static HashMap<String, String>> {
    static RU: OnceLock<HashMap<String, String>> = OnceLock::new();
    static FR: OnceLock<HashMap<String, String>> = OnceLock::new();
    match lang {
        "ru" => Some(RU.get_or_init(|| serde_json::from_str(include_str!("../locales/ru.json")).unwrap())),
        "fr" => Some(FR.get_or_init(|| serde_json::from_str(include_str!("../locales/fr.json")).unwrap())),
        _ => None,
    }
}

pub fn language() -> &'static str {
    *LANGUAGE.read().unwrap()
}

pub fn set_language(value: &str) -> Result<(), String> {
    let found = LANGUAGES.iter().find(|l| **l == value).ok_or_else(|| format!("Unsupported language: {value}"))?;
    *LANGUAGE.write().unwrap() = found;
    Ok(())
}

pub fn tr(message: &str) -> String {
    table(language()).and_then(|t| t.get(message)).cloned().unwrap_or_else(|| message.to_string())
}

#[derive(Clone, Debug)]
pub enum Arg {
    S(String),
    I(i64),
    F(f64),
}

impl From<&str> for Arg {
    fn from(v: &str) -> Self {
        Arg::S(v.to_string())
    }
}
impl From<String> for Arg {
    fn from(v: String) -> Self {
        Arg::S(v)
    }
}
impl From<&String> for Arg {
    fn from(v: &String) -> Self {
        Arg::S(v.clone())
    }
}
impl From<i64> for Arg {
    fn from(v: i64) -> Self {
        Arg::I(v)
    }
}
impl From<usize> for Arg {
    fn from(v: usize) -> Self {
        Arg::I(v as i64)
    }
}
impl From<i32> for Arg {
    fn from(v: i32) -> Self {
        Arg::I(v as i64)
    }
}
impl From<f64> for Arg {
    fn from(v: f64) -> Self {
        Arg::F(v)
    }
}

/// Apply a Python format spec subset: `[[fill]align][width][.precision][type]`.
pub fn format_spec(value: &Arg, spec: &str) -> String {
    let mut chars: Vec<char> = spec.chars().collect();
    let mut align = None;
    if chars.len() >= 2 && "<>^".contains(chars[1]) {
        align = Some(chars[1]);
        chars.drain(..2);
    } else if !chars.is_empty() && "<>^".contains(chars[0]) {
        align = Some(chars[0]);
        chars.remove(0);
    }
    let spec: String = chars.into_iter().collect();
    let (width_part, rest) = match spec.find(|c: char| !c.is_ascii_digit()) {
        Some(index) => (&spec[..index], &spec[index..]),
        None => (spec.as_str(), ""),
    };
    let width: usize = width_part.parse().unwrap_or(0);
    let mut precision = None;
    let mut kind = rest;
    if let Some(stripped) = rest.strip_prefix('.') {
        let end = stripped.find(|c: char| !c.is_ascii_digit()).unwrap_or(stripped.len());
        precision = stripped[..end].parse::<usize>().ok();
        kind = &stripped[end..];
    }
    let text = match (value, kind) {
        (Arg::S(s), _) => s.clone(),
        (Arg::I(i), "f") => format!("{:.*}", precision.unwrap_or(6), *i as f64),
        (Arg::I(i), "d") => {
            if width_part.starts_with('0') && width > 0 {
                return format!("{i:0width$}");
            }
            i.to_string()
        }
        (Arg::I(i), _) => i.to_string(),
        (Arg::F(f), "f") => format!("{:.*}", precision.unwrap_or(6), f),
        (Arg::F(f), "g") => {
            if f.fract() == 0.0 {
                format!("{}", *f as i64)
            } else {
                format!("{f}")
            }
        }
        (Arg::F(f), _) => match precision {
            Some(p) => format!("{f:.p$}"),
            None => {
                if f.fract() == 0.0 {
                    format!("{f:.1}")
                } else {
                    format!("{f}")
                }
            }
        },
    };
    let default_right = !matches!(value, Arg::S(_));
    let count = text.chars().count();
    if count >= width {
        return text;
    }
    let pad = width - count;
    match align.unwrap_or(if default_right { '>' } else { '<' }) {
        '>' => format!("{}{text}", " ".repeat(pad)),
        '^' => format!("{}{text}{}", " ".repeat(pad / 2), " ".repeat(pad - pad / 2)),
        _ => format!("{text}{}", " ".repeat(pad)),
    }
}

/// Python `str.format(**values)` over named fields.
pub fn format_named(template: &str, args: &[(&str, Arg)]) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(index) = rest.find(['{', '}']) {
        out.push_str(&rest[..index]);
        let tail = &rest[index..];
        if tail.starts_with("{{") || tail.starts_with("}}") {
            out.push(tail.chars().next().unwrap());
            rest = &tail[2..];
            continue;
        }
        if let Some(after) = tail.strip_prefix('}') {
            out.push('}');
            rest = after;
            continue;
        }
        let Some(end) = tail.find('}') else {
            out.push_str(tail);
            return out;
        };
        let field = &tail[1..end];
        let (name, spec) = field.split_once(':').unwrap_or((field, ""));
        match args.iter().find(|(key, _)| *key == name) {
            Some((_, value)) => out.push_str(&format_spec(value, spec)),
            None => out.push_str(&tail[..=end]),
        }
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    out
}

pub fn trf(message: &str, args: &[(&str, Arg)]) -> String {
    format_named(&tr(message), args)
}

#[macro_export]
macro_rules! trf {
    ($msg:expr $(, $key:ident = $value:expr)* $(,)?) => {
        $crate::i18n::trf($msg, &[$((stringify!($key), $crate::i18n::Arg::from($value))),*])
    };
}

pub fn home() -> PathBuf {
    #[cfg(windows)]
    let var = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    #[cfg(not(windows))]
    let var = std::env::var_os("HOME");
    var.map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

pub fn settings_path() -> PathBuf {
    let root = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home().join(".config"));
    root.join("ai-sessions").join("settings.json")
}

pub fn read_settings() -> Map<String, Value> {
    fs::read_to_string(settings_path())
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

pub fn preferred_language() -> &'static str {
    let settings = read_settings();
    let value = settings.get("language").and_then(Value::as_str).unwrap_or("en");
    LANGUAGES.iter().find(|l| **l == value).copied().unwrap_or("en")
}

/// Write a file atomically with mode 600 (private on Unix).
pub fn write_private(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or(std::path::Path::new("."));
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::Builder::new().prefix(".ais-").tempfile_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o600))?;
    }
    temp.write_all(data)?;
    temp.flush()?;
    temp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// Merge values into settings.json atomically, keeping unknown keys.
pub fn save_settings(values: IndexMap<&str, Value>) -> std::io::Result<()> {
    let mut settings = read_settings();
    for (key, value) in values {
        settings.insert(key.to_string(), value);
    }
    let mut text = serde_json::to_string(&Value::Object(settings)).unwrap_or_default();
    text.push('\n');
    write_private(&settings_path(), text.as_bytes())
}

pub fn save_language(value: &str) -> std::io::Result<()> {
    if !LANGUAGES.contains(&value) {
        return Err(std::io::Error::other(format!("Unsupported language: {value}")));
    }
    save_settings(IndexMap::from([("language", Value::from(value))]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_format_specs() {
        assert_eq!(format_named("{v0:8}|{v1:5}", &[("v0", "OMP".into()), ("v1", 12i64.into())]), "OMP     |   12");
        assert_eq!(format_named("{v0:3.0f}%", &[("v0", 7.6.into())]), "  8%");
        assert_eq!(format_named("${v0:.4f}", &[("v0", 0.5.into())]), "$0.5000");
        assert_eq!(format_named("{a} {missing}", &[("a", "x".into())]), "x {missing}");
    }

    #[test]
    fn every_translation_keeps_placeholders() {
        let names = |text: &str| {
            let mut found: Vec<String> =
                regex::Regex::new(r"\{([a-z0-9_]+)[^}]*\}").unwrap().captures_iter(text).map(|c| c[1].to_string()).collect();
            found.sort();
            found
        };
        for lang in ["ru", "fr"] {
            for (key, value) in table(lang).unwrap() {
                assert_eq!(names(key), names(value), "{lang}: {key}");
            }
        }
        assert_eq!(table("ru").unwrap().len(), table("fr").unwrap().len());
    }
}
