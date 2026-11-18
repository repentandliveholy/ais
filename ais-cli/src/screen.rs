//! An in-memory terminal frame: cells, styles and clickable regions.

use crate::i18n::language;
use crate::text::{cell_width, clean, clip, width};
use std::path::Path;

pub const WORDMARK: [&str; 3] = [" ▄▀▀▄  ▀  ▄▀▀▀ ", " █▄▄█  █  ▀▀▀▄ ", " █  █  █  ▄▄▄▀ "];

/// Hex colors drive the SVG exporter; ANSI 256 indexes drive the terminal.
pub fn palette_hex(name: &str) -> &'static str {
    match name {
        "bg" => "#121212",
        "row" => "#262626",
        "stripe" => "#303030",
        "panel" => "#1c1c1c",
        "selection" => "#444444",
        "muted" => "#a0aec5",
        "faint" => "#56657d",
        "accent" => "#b9a3ff",
        "cyan" => "#73d8f4",
        "green" => "#95e1b4",
        "amber" => "#f3ca86",
        "red" => "#f291a6",
        "blue" => "#95bbff",
        _ => "#edf1f8",
    }
}

pub fn ansi_color(style: &str) -> u8 {
    match style {
        "muted" => 146,
        "faint" => 60,
        "accent" => 147,
        "cyan" => 117,
        "green" => 115,
        "amber" => 222,
        "red" => 211,
        "blue" => 111,
        _ => 255,
    }
}

pub fn client_style(client: &str) -> &'static str {
    match client {
        "omp" => "green",
        "claude" => "amber",
        "codex" => "blue",
        "agy" => "accent",
        "gemini" => "cyan",
        "pi" => "red",
        _ => "text",
    }
}

pub fn status_style(status: &str) -> &'static str {
    match status {
        "ready" => "green",
        "error" => "red",
        "interrupted" | "awaiting" => "amber",
        "tool" => "cyan",
        "thinking" => "accent",
        "user" => "blue",
        _ => "muted",
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Tab(&'static str),
    Key(char),
    Scope(&'static str),
    Client(String),
    Search,
    Row(usize),
    Close,
    Noop,
    New(usize),
    Lang(usize),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    pub ch: String,
    pub style: &'static str,
    pub selected: bool,
    pub bold: bool,
    pub background: &'static str,
}

impl Default for Cell {
    fn default() -> Self {
        Self { ch: " ".into(), style: "text", selected: false, bold: false, background: "" }
    }
}

pub type Style = (&'static str, bool, bool, &'static str);

pub struct Screen {
    pub cols: usize,
    pub rows: usize,
    pub cells: Vec<Vec<Cell>>,
    /// Clickable regions: (y0, y1, x0, x1, action); later regions win.
    pub hits: Vec<(i64, i64, i64, i64, Action)>,
}

/// A styled text fragment for `spans`.
pub type Part = (String, &'static str, bool);

pub fn part(text: impl Into<String>, style: &'static str) -> Part {
    (text.into(), style, false)
}

impl Screen {
    pub fn new(cols: usize, rows: usize) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        Self { cols, rows, cells: vec![vec![Cell::default(); cols]; rows], hits: Vec::new() }
    }

    pub fn hit(&mut self, y: i64, x: i64, w: i64, action: Action, h: i64) {
        if w > 0 && h > 0 {
            self.hits.push((y, y + h, x, x + w, action));
        }
    }

    pub fn hit_at(&self, y: i64, x: i64) -> Option<Action> {
        self.hits.iter().rev().find(|(y0, y1, x0, x1, _)| *y0 <= y && y < *y1 && *x0 <= x && x < *x1).map(|h| h.4.clone())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn put_ex(&mut self, y: i64, x: i64, text: &str, style: &'static str, selected: bool, bold: bool, limit: Option<i64>) {
        let cols = self.cols as i64;
        if y < 0 || y >= self.rows as i64 || x >= cols {
            return;
        }
        let value = clean(text, 0).replace(['\n', '\t'], " ");
        let room = (cols - x.max(0)).min(limit.unwrap_or(cols));
        let value = clip(&value, room, true);
        let row = &mut self.cells[y as usize];
        let mut x = x;
        for c in value.chars() {
            let w = cell_width(c) as i64;
            if w == 0 {
                if 0 < x && x <= cols {
                    row[(x - 1) as usize].ch.push(c);
                }
                continue;
            }
            if x >= 0 && x + w <= cols {
                row[x as usize] = Cell { ch: c.to_string(), style, selected, bold, background: "" };
                if w == 2 {
                    row[(x + 1) as usize] = Cell { ch: String::new(), style, selected, bold, background: "" };
                }
            }
            x += w;
        }
    }

    pub fn put(&mut self, y: i64, x: i64, text: &str, style: &'static str) {
        self.put_ex(y, x, text, style, false, false, None);
    }

    pub fn put_limit(&mut self, y: i64, x: i64, text: &str, style: &'static str, limit: i64) {
        self.put_ex(y, x, text, style, false, false, Some(limit));
    }

    pub fn put_bold(&mut self, y: i64, x: i64, text: &str, style: &'static str) {
        self.put_ex(y, x, text, style, false, true, None);
    }

    /// Put parts side by side; return the end column.
    pub fn spans(&mut self, y: i64, x: i64, parts: &[Part], selected: bool, limit: Option<i64>) -> i64 {
        let end = match limit {
            Some(l) => x + l,
            None => self.cols as i64,
        };
        let mut x = x;
        for (text, style, bold) in parts {
            if x >= end {
                break;
            }
            self.put_ex(y, x, text, style, selected, *bold, Some(end - x));
            x += width(&clip(&clean(text, 0), end - x, true)) as i64;
        }
        x
    }

    pub fn fill(&mut self, y: i64, x: i64, count: i64, selected: bool) {
        self.put_ex(y, x, &" ".repeat(count.max(0) as usize), "text", selected, false, None);
    }

    pub fn shade(&mut self, y: i64, x: i64, count: i64, background: &'static str) {
        if y < 0 || y >= self.rows as i64 {
            return;
        }
        let start = x.max(0) as usize;
        let end = ((x + count).max(0) as usize).min(self.cols);
        for cell in self.cells[y as usize].iter_mut().take(end).skip(start) {
            cell.background = background;
        }
    }

    pub fn rule(&mut self, y: i64) {
        let count = self.cols as i64 - 4;
        self.put(y, 2, &"─".repeat(count.max(0) as usize), "faint");
    }

    /// Runs of identically styled cells in one row.
    pub fn runs(&self, y: usize) -> Vec<(usize, String, Style)> {
        let mut result = Vec::new();
        let mut start = 0;
        let mut value = String::new();
        let mut previous: Option<Style> = None;
        for (x, c) in self.cells[y].iter().enumerate() {
            let style = (c.style, c.selected, c.bold, c.background);
            if let Some(p) = previous
                && p != style
            {
                result.push((start, std::mem::take(&mut value), p));
                start = x;
            }
            value.push_str(&c.ch);
            previous = Some(style);
        }
        if let Some(p) = previous {
            result.push((start, value, p));
        }
        result
    }

    pub fn row_text(&self, y: usize) -> String {
        self.cells[y].iter().map(|c| c.ch.as_str()).collect::<String>()
    }

    pub fn plain(&self) -> String {
        (0..self.rows).map(|y| self.row_text(y).trim_end().to_string()).collect::<Vec<_>>().join("\n")
    }

    pub fn svg(&self) -> String {
        // One text node per terminal cell preserves the actual layout even when
        // the fallback font has different glyph metrics.
        let (cw, ch, pad) = (9usize, 20usize, 20usize);
        let (w, h) = (self.cols * cw + pad * 2, self.rows * ch + pad * 2);
        let mut parts = vec![
            format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}">"#),
            format!(r#"<rect width="100%" height="100%" rx="16" fill="{}"/>"#, palette_hex("bg")),
            r#"<g font-family="Menlo, DejaVu Sans Mono, monospace" font-size="15" xml:space="preserve">"#.to_string(),
        ];
        for (y, row) in self.cells.iter().enumerate() {
            for (x, c) in row.iter().enumerate() {
                let background = if c.selected { "selection" } else { c.background };
                if !background.is_empty() {
                    parts.push(format!(
                        r#"<rect x="{}" y="{}" width="{cw}" height="{ch}" fill="{}"/>"#,
                        pad + x * cw,
                        pad + y * ch,
                        palette_hex(background)
                    ));
                }
                if !c.ch.trim().is_empty() {
                    let escaped =
                        c.ch.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#x27;");
                    parts.push(format!(
                        r#"<text x="{}" y="{}" fill="{}" font-weight="{}">{escaped}</text>"#,
                        pad + x * cw,
                        pad + y * ch + 15,
                        palette_hex(c.style),
                        if c.bold { "700" } else { "400" }
                    ));
                }
            }
        }
        parts.push("</g>".into());
        parts.push("</svg>".into());
        parts.join("\n")
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let extension = path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
        let content = match extension.as_str() {
            "svg" => self.svg(),
            "html" => format!(
                r#"<!doctype html><html lang="{}"><meta charset="utf-8"><title>ais · AI Sessions</title><body style="margin:0;background:#0b0e16;display:grid;place-items:center;min-height:100vh">{}</body></html>"#,
                language(),
                self.svg()
            ),
            _ => self.plain() + "\n",
        };
        std::fs::write(path, content)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_characters_and_limits() {
        let mut s = Screen::new(10, 2);
        s.put(0, 0, "日本語テキスト", "text");
        assert_eq!(s.plain().lines().next().unwrap(), "日本語テ…");
        s.put_limit(1, 2, "abcdefgh", "text", 4);
        assert_eq!(s.row_text(1).trim_end(), "  abc…");
        s.hit(1, 0, 5, Action::Search, 1);
        s.hit(1, 2, 2, Action::Close, 1);
        assert_eq!(s.hit_at(1, 3), Some(Action::Close));
        assert_eq!(s.hit_at(1, 0), Some(Action::Search));
        assert_eq!(s.hit_at(0, 0), None);
    }
}
