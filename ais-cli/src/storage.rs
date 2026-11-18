//! Read-only access to client stores and the ais session index cache.

use crate::i18n::{language, write_private};
use crate::model::Session;
use indexmap::IndexMap;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Map, Value};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

fn uri_path(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~:".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    if !out.starts_with('/') {
        out.insert(0, '/');
    }
    out
}

/// Never create sidecars in a client's storage; respect a live WAL.
pub fn read_database(path: &Path) -> rusqlite::Result<Connection> {
    if !path.exists() {
        return Err(rusqlite::Error::InvalidPath(path.to_path_buf()));
    }
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let wal = PathBuf::from(format!("{}-wal", path.display()));
    // An immutable connection is safe only when no WAL is present.
    let suffix = if wal.exists() { "?mode=ro" } else { "?mode=ro&immutable=1" };
    let uri = format!("file://{}{suffix}", uri_path(&resolved));
    let con =
        Connection::open_with_flags(uri, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    con.busy_timeout(std::time::Duration::from_millis(300))?;
    con.execute_batch("PRAGMA query_only=ON")?;
    Ok(con)
}

/// Every row as an ordered JSON-like map.
pub fn query_rows(con: &Connection, sql: &str, params: &[&dyn rusqlite::ToSql]) -> rusqlite::Result<Vec<IndexMap<String, Value>>> {
    let mut statement = con.prepare(sql)?;
    let names: Vec<String> = statement.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = statement.query(params)?;
    let mut result = Vec::new();
    while let Some(row) = rows.next()? {
        let mut map = IndexMap::new();
        for (i, name) in names.iter().enumerate() {
            let value = match row.get_ref(i)? {
                ValueRef::Null => Value::Null,
                ValueRef::Integer(v) => Value::from(v),
                ValueRef::Real(v) => Value::from(v),
                ValueRef::Text(t) => Value::from(String::from_utf8_lossy(t).into_owned()),
                ValueRef::Blob(b) => Value::from(String::from_utf8_lossy(b).into_owned()),
            };
            map.insert(name.clone(), value);
        }
        result.push(map);
    }
    Ok(result)
}

pub const SAMPLE_LIMIT: u64 = 384 * 1024;

/// Read complete JSONL records, tolerating partial concurrent writes.
///
/// The overview samples head and tail so multi-gigabyte histories do not delay
/// the picker. The focused detail scan streams the complete file separately.
pub fn json_records(path: &Path, full: bool, mut visit: impl FnMut(&Map<String, Value>)) -> std::io::Result<()> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    let mut handle = |raw: &[u8]| {
        if let Ok(Value::Object(item)) = serde_json::from_slice::<Value>(raw) {
            visit(&item);
        }
    };
    if full || size <= SAMPLE_LIMIT {
        let mut reader = BufReader::with_capacity(256 * 1024, file);
        let mut buffer = Vec::new();
        loop {
            buffer.clear();
            if reader.read_until(b'\n', &mut buffer)? == 0 {
                break;
            }
            handle(&buffer);
        }
        return Ok(());
    }
    let mut head = vec![0u8; 96 * 1024];
    let read = read_full(&mut file, &mut head)?;
    head.truncate(read);
    file.seek(SeekFrom::Start((head.len() as u64).max(size.saturating_sub(288 * 1024))))?;
    let mut reader = BufReader::new(file);
    let mut partial = Vec::new();
    reader.read_until(b'\n', &mut partial)?; // discard the initial partial record
    let mut tail = Vec::new();
    reader.read_to_end(&mut tail)?;
    let head_lines: Vec<&[u8]> = split_lines(&head);
    let keep = head_lines.len().saturating_sub(1);
    for raw in &head_lines[..keep] {
        handle(raw);
    }
    for raw in split_lines(&tail) {
        handle(raw);
    }
    Ok(())
}

fn read_full(file: &mut File, buffer: &mut [u8]) -> std::io::Result<usize> {
    let mut total = 0;
    while total < buffer.len() {
        let n = file.read(&mut buffer[total..])?;
        if n == 0 {
            break;
        }
        total += n;
    }
    Ok(total)
}

/// `bytes.splitlines()`: \n, \r\n and \r separate lines.
fn split_lines(data: &[u8]) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < data.len() {
        if data[i] == b'\n' || data[i] == b'\r' {
            lines.push(&data[start..i]);
            if data[i] == b'\r' && i + 1 < data.len() && data[i + 1] == b'\n' {
                i += 1;
            }
            start = i + 1;
        }
        i += 1;
    }
    if start < data.len() {
        lines.push(&data[start..]);
    }
    lines
}

pub fn signature(path: &Path) -> std::io::Result<[u64; 3]> {
    let meta = std::fs::metadata(path)?;
    let mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_nanos() as u64).unwrap_or(0);
    #[cfg(unix)]
    let inode = {
        use std::os::unix::fs::MetadataExt;
        meta.ino()
    };
    #[cfg(not(unix))]
    let inode = 0u64;
    Ok([mtime, meta.len(), inode])
}

pub struct Cache {
    pub root: Option<PathBuf>,
    pub enabled: bool,
    data: IndexMap<String, Value>,
}

impl Cache {
    pub const VERSION: u64 = 6;

    pub fn new(root: Option<PathBuf>, enabled: bool) -> Self {
        let enabled = enabled && root.is_some();
        let mut data = IndexMap::new();
        if enabled
            && let Some(root) = &root
            && let Ok(text) = std::fs::read_to_string(root.join("index.json"))
            && let Ok(Value::Object(mut parsed)) = serde_json::from_str::<Value>(&text)
            && parsed.get("version").and_then(Value::as_u64) == Some(Self::VERSION)
            && let Some(Value::Object(entries)) = parsed.remove("entries")
        {
            data = entries.into_iter().collect();
        }
        Self { root, enabled, data }
    }

    #[cfg(test)]
    pub fn disabled() -> Self {
        Self { root: None, enabled: false, data: IndexMap::new() }
    }

    fn key(client: &str, profile: &str, path: &Path) -> String {
        format!("{}:{client}:{profile}:{}", language(), path.display())
    }

    pub fn get(&self, path: &Path, client: &str, profile: &str) -> Option<Session> {
        let entry = self.data.get(&Self::key(client, profile, path))?;
        let sig: [u64; 3] = serde_json::from_value(entry.get("sig")?.clone()).ok()?;
        if signature(path).ok()? != sig {
            return None;
        }
        let mut session: Session = serde_json::from_value(entry.get("session")?.clone()).ok()?;
        session.source_sig = Some(sig);
        Some(session)
    }

    pub fn put(&mut self, path: &Path, session: &Session) {
        let Ok(sig) = signature(path) else { return };
        if session.source_sig.is_some_and(|s| s != sig) {
            return; // A client appended a new turn while we were reading.
        }
        let Ok(value) = serde_json::to_value(session) else { return };
        self.data.insert(Self::key(&session.client, &session.profile, path), serde_json::json!({"sig": sig, "session": value}));
    }

    pub fn save(&mut self) {
        if !self.enabled {
            return;
        }
        let Some(root) = &self.root else { return };
        // Only prune files that have disappeared; individual source failures
        // must not invalidate the remaining adapters' cache entries.
        let entries: Map<String, Value> = self
            .data
            .iter()
            .filter(|(_, v)| v.pointer("/session/path").and_then(Value::as_str).is_some_and(|p| Path::new(p).exists()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let payload = serde_json::json!({"version": Self::VERSION, "entries": entries});
        if std::fs::create_dir_all(root).is_ok() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700));
            }
            let _ = write_private(&root.join("index.json"), payload.to_string().as_bytes());
        }
    }
}

pub fn default_cache(home_dir: &Path) -> PathBuf {
    let root = std::env::var_os("XDG_CACHE_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home_dir.join(".cache"));
    root.join("ai-sessions")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn large_files_sample_head_and_tail_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let mut file = File::create(&path).unwrap();
        writeln!(file, "{{\"n\": 0}}").unwrap();
        let filler = "x".repeat(1000);
        for n in 1..1000 {
            writeln!(file, "{{\"n\": {n}, \"f\": \"{filler}\"}}").unwrap();
        }
        write!(file, "{{\"n\": 99999, \"partial").unwrap();
        drop(file);
        let mut seen = Vec::new();
        json_records(&path, false, |r| seen.push(r["n"].as_i64().unwrap())).unwrap();
        assert_eq!(seen[0], 0);
        assert_eq!(*seen.last().unwrap(), 999);
        assert!(seen.len() < 500);
        let mut all = 0;
        json_records(&path, true, |_| all += 1).unwrap();
        assert_eq!(all, 1000);
    }

    #[test]
    fn cache_round_trip_and_invalidation() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.jsonl");
        std::fs::write(&source, "{}\n").unwrap();
        let mut cache = Cache::new(Some(dir.path().join("cache")), true);
        let session = Session { path: source.display().to_string(), title: "T".into(), ..Session::new("omp", "1") };
        cache.put(&source, &session);
        cache.save();
        let cache = Cache::new(Some(dir.path().join("cache")), true);
        assert_eq!(cache.get(&source, "omp", "").unwrap().title, "T");
        std::fs::write(&source, "{}\n{}\n").unwrap();
        assert!(cache.get(&source, "omp", "").is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("cache/index.json")).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
