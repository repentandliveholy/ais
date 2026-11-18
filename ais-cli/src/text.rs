//! Text sanitizing and terminal cell geometry shared by every renderer.

use regex::Regex;
use serde_json::Value;
use std::sync::OnceLock;
use unicode_general_category::{GeneralCategory, get_general_category};
use unicode_width::UnicodeWidthChar;

fn ansi() -> &'static Regex {
    static ANSI: OnceLock<Regex> = OnceLock::new();
    // Strip all terminal controls, including OSC hyperlinks and bidi overrides.
    ANSI.get_or_init(|| Regex::new(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b\[[0-?]*[ -/]*[@-~]|\x1b[@-_]").unwrap())
}

/// Python truthiness of a JSON value.
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn py_number(number: &serde_json::Number) -> String {
    if let Some(int) = number.as_i64() {
        int.to_string()
    } else if let Some(int) = number.as_u64() {
        int.to_string()
    } else {
        let float = number.as_f64().unwrap_or(0.0);
        if float.fract() == 0.0 && float.abs() < 1e16 { format!("{float:.1}") } else { float.to_string() }
    }
}

fn py_repr(value: &Value) -> String {
    match value {
        Value::String(text) => format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'")),
        Value::Array(items) => format!("[{}]", items.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(items) => {
            format!("{{{}}}", items.iter().map(|(k, v)| format!("'{k}': {}", py_repr(v))).collect::<Vec<_>>().join(", "))
        }
        other => py_str(other),
    }
}

/// Python `str(value)` for JSON values.
pub fn py_str(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::String(text) => text.clone(),
        Value::Number(number) => py_number(number),
        other => py_repr(other),
    }
}

/// Python-like `str(value or "")` for JSON values.
pub fn pystr(value: &Value) -> String {
    if truthy(value) { py_str(value) } else { String::new() }
}

fn hidden(c: char) -> bool {
    if c == '\n' || c == '\t' {
        return false;
    }
    matches!(
        get_general_category(c),
        GeneralCategory::Control
            | GeneralCategory::Format
            | GeneralCategory::Surrogate
            | GeneralCategory::PrivateUse
            | GeneralCategory::Unassigned
    )
}

fn take_chars(text: &str, count: usize) -> &str {
    match text.char_indices().nth(count) {
        Some((index, _)) => &text[..index],
        None => text,
    }
}

/// Remove terminal controls and non-portable characters; `limit` counts chars.
pub fn clean(text: &str, limit: usize) -> String {
    let text = if limit > 0 { take_chars(text, limit * 2 + 256) } else { text };
    let stripped = ansi().replace_all(text, "");
    let result: String = stripped.chars().filter(|c| !hidden(*c)).collect();
    if limit > 0 { take_chars(&result, limit).to_string() } else { result }
}

pub fn clean_value(value: &Value, limit: usize) -> String {
    clean(&pystr(value), limit)
}

/// One line with collapsed whitespace.
pub fn line(text: &str, limit: usize) -> String {
    let cleaned = clean(text, if limit > 0 { limit * 2 } else { 0 });
    let joined = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if limit > 0 { take_chars(&joined, limit).to_string() } else { joined }
}

pub fn line_value(value: &Value, limit: usize) -> String {
    line(&pystr(value), limit)
}

pub fn casefold(text: &str) -> String {
    text.to_lowercase()
}

pub fn cell_width(c: char) -> usize {
    if c == '\n' || c == '\t' {
        return 1;
    }
    c.width().unwrap_or(0)
}

pub fn width(text: &str) -> usize {
    text.chars().map(cell_width).sum()
}

pub fn clip(text: &str, maximum: i64, ellipsis: bool) -> String {
    if maximum <= 0 {
        return String::new();
    }
    let maximum = maximum as usize;
    if width(text) <= maximum {
        return text.to_string();
    }
    let budget = maximum - usize::from(ellipsis);
    let mut result = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = cell_width(c);
        if used + w > budget {
            break;
        }
        result.push(c);
        used += w;
    }
    if ellipsis {
        result.push('…');
    }
    result
}

fn expand_tabs(text: &str, size: usize) -> String {
    let mut out = String::new();
    let mut column = 0;
    for c in text.chars() {
        match c {
            '\t' => {
                let spaces = size - column % size;
                out.extend(std::iter::repeat_n(' ', spaces));
                column += spaces;
            }
            '\n' | '\r' => {
                out.push(c);
                column = 0;
            }
            _ => {
                out.push(c);
                column += 1;
            }
        }
    }
    out
}

/// Python `str.splitlines()` for the separators that survive `clean`.
pub fn splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        match rest.find(['\n', '\r']) {
            Some(index) => {
                lines.push(&rest[..index]);
                let skip = if rest[index..].starts_with("\r\n") { 2 } else { 1 };
                rest = &rest[index + skip..];
            }
            None => {
                lines.push(rest);
                break;
            }
        }
    }
    lines
}

pub fn wrap(text: &str, maximum: i64) -> Vec<String> {
    let mut result = Vec::new();
    if maximum <= 0 {
        return result;
    }
    let expanded = expand_tabs(&clean(text, 0), 4);
    for paragraph in splitlines(&expanded) {
        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            let mut word = word.to_string();
            if !current.is_empty() && width(&format!("{current} {word}")) as i64 > maximum {
                result.push(std::mem::take(&mut current));
            }
            while width(&word) as i64 > maximum {
                let piece = clip(&word, maximum, false);
                if piece.is_empty() {
                    break;
                }
                word = word[piece.len()..].to_string();
                result.push(piece);
            }
            if !word.is_empty() {
                current = if current.is_empty() { word } else { format!("{current} {word}") };
            }
        }
        result.push(current);
    }
    if result.is_empty() {
        result.push(String::new());
    }
    result
}

/// POSIX shell quoting identical to Python's `shlex.quote`.
pub fn shell_quote(text: &str) -> String {
    if text.is_empty() {
        return "''".into();
    }
    let safe = text.chars().all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    if safe { text.to_string() } else { format!("'{}'", text.replace('\'', "'\"'\"'")) }
}

pub fn shell_join(argv: &[String]) -> String {
    argv.iter().map(|a| shell_quote(a)).collect::<Vec<_>>().join(" ")
}

/// Left-pad / right-pad by character count, like Python's format widths.
pub fn ljust(text: &str, size: usize) -> String {
    let count = text.chars().count();
    if count >= size { text.to_string() } else { format!("{text}{}", " ".repeat(size - count)) }
}

pub fn rjust(text: &str, size: usize) -> String {
    let count = text.chars().count();
    if count >= size { text.to_string() } else { format!("{}{text}", " ".repeat(size - count)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_are_removed() {
        assert_eq!(clean("a\x1b[31mb\x1b]8;;http://x\x07c\u{202e}d\u{e000}", 0), "abcd");
        assert_eq!(line("  a \n\t b  ", 0), "a b");
        assert_eq!(line("abcdef", 3), "abc");
    }

    #[test]
    fn clip_and_wrap_use_cells() {
        assert_eq!(clip("日本語テキスト", 5, true), "日本…");
        assert_eq!(wrap("one two three", 7), vec!["one two", "three"]);
        assert_eq!(wrap("", 7), vec![""]);
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn quoting_matches_shlex() {
        assert_eq!(shell_quote("/a b/c"), "'/a b/c'");
        assert_eq!(shell_quote("it's"), "'it'\"'\"'s'");
        assert_eq!(shell_quote("plain-path/x.y"), "plain-path/x.y");
    }
}
