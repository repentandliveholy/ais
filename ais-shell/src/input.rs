//! Keyboard and mouse encoding for the PTY (xterm + kitty keyboard protocol).

use alacritty_terminal::term::TermMode;
use eframe::egui::{Key, Modifiers};

/// xterm modifier parameter: 1 + shift + 2·alt + 4·ctrl.
fn modifier_param(mods: Modifiers) -> u8 {
    1 + u8::from(mods.shift) + 2 * u8::from(mods.alt) + 4 * u8::from(mods.ctrl)
}

fn csi(body: &str) -> Vec<u8> {
    format!("\x1b[{body}").into_bytes()
}

/// Cursor-style keys: arrows, Home, End and F1–F4 share one encoding.
fn cursor_key(letter: char, mods: Modifiers, app_cursor: bool) -> Vec<u8> {
    let param = modifier_param(mods);
    if param > 1 {
        csi(&format!("1;{param}{letter}"))
    } else if app_cursor {
        format!("\x1bO{letter}").into_bytes()
    } else {
        csi(&letter.to_string())
    }
}

fn tilde_key(code: u8, mods: Modifiers) -> Vec<u8> {
    let param = modifier_param(mods);
    if param > 1 {
        csi(&format!("{code};{param}~"))
    } else {
        csi(&format!("{code}~"))
    }
}

fn kitty_key(code: u32, mods: Modifiers) -> Vec<u8> {
    let param = modifier_param(mods);
    if param > 1 {
        csi(&format!("{code};{param}u"))
    } else {
        csi(&format!("{code}u"))
    }
}

pub fn letter_of(key: Key) -> Option<char> {
    let name = key.name();
    let mut chars = name.chars();
    let first = chars.next()?;
    (chars.next().is_none() && first.is_ascii_alphanumeric()).then(|| first.to_ascii_lowercase())
}

/// Encodes a key press. `option_as_meta` makes Option+letter send ESC+letter
/// (the caller then drops the matching text event).
pub fn encode_key(key: Key, mods: Modifiers, mode: TermMode, option_as_meta: bool) -> Option<Vec<u8>> {
    let kitty = mode.contains(TermMode::DISAMBIGUATE_ESC_CODES);
    let kitty_all = mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC);
    let app_cursor = mode.contains(TermMode::APP_CURSOR);

    // macOS Command combinations that reach the terminal behave like in
    // Terminal.app / iTerm2 "natural text editing".
    if mods.mac_cmd {
        return match key {
            Key::ArrowLeft | Key::Home => Some(vec![0x01]),
            Key::ArrowRight | Key::End => Some(vec![0x05]),
            Key::Backspace => Some(vec![0x15]),
            _ => None,
        };
    }

    let plain_alt = mods.alt && !mods.ctrl && !mods.shift;
    Some(match key {
        Key::Enter => {
            if kitty_all || (kitty && (mods.shift || mods.alt || mods.ctrl)) {
                kitty_key(13, mods)
            } else if mods.shift || mods.alt {
                // Shift/Option+Enter: newline in Claude Code, Codex, Gemini.
                b"\x1b\r".to_vec()
            } else if mods.ctrl {
                b"\n".to_vec()
            } else {
                b"\r".to_vec()
            }
        }
        Key::Tab => {
            if mods.shift && !mods.ctrl && !mods.alt {
                b"\x1b[Z".to_vec()
            } else if (kitty || kitty_all) && (mods.ctrl || mods.alt || mods.shift) {
                kitty_key(9, mods)
            } else {
                b"\t".to_vec()
            }
        }
        Key::Backspace => {
            if (kitty || kitty_all) && (mods.ctrl || mods.shift || mods.alt) {
                kitty_key(127, mods)
            } else if mods.alt {
                b"\x1b\x7f".to_vec()
            } else if mods.ctrl {
                vec![0x08]
            } else {
                vec![0x7f]
            }
        }
        Key::Escape => {
            if kitty || kitty_all {
                kitty_key(27, mods)
            } else {
                vec![0x1b]
            }
        }
        Key::ArrowLeft if plain_alt && !kitty => b"\x1bb".to_vec(),
        Key::ArrowRight if plain_alt && !kitty => b"\x1bf".to_vec(),
        Key::ArrowUp => cursor_key('A', mods, app_cursor),
        Key::ArrowDown => cursor_key('B', mods, app_cursor),
        Key::ArrowRight => cursor_key('C', mods, app_cursor),
        Key::ArrowLeft => cursor_key('D', mods, app_cursor),
        Key::Home => cursor_key('H', mods, app_cursor),
        Key::End => cursor_key('F', mods, app_cursor),
        Key::Insert => tilde_key(2, mods),
        Key::Delete => tilde_key(3, mods),
        Key::PageUp => tilde_key(5, mods),
        Key::PageDown => tilde_key(6, mods),
        Key::F1 => cursor_key('P', mods, true),
        Key::F2 => cursor_key('Q', mods, true),
        Key::F3 => cursor_key('R', mods, true),
        Key::F4 => cursor_key('S', mods, true),
        Key::F5 => tilde_key(15, mods),
        Key::F6 => tilde_key(17, mods),
        Key::F7 => tilde_key(18, mods),
        Key::F8 => tilde_key(19, mods),
        Key::F9 => tilde_key(20, mods),
        Key::F10 => tilde_key(21, mods),
        Key::F11 => tilde_key(23, mods),
        Key::F12 => tilde_key(24, mods),
        Key::Space if mods.ctrl && (kitty || kitty_all) => kitty_key(32, mods),
        Key::Space if mods.ctrl => vec![0x00],
        Key::OpenBracket if mods.ctrl => {
            if kitty && (mods.shift || mods.alt) {
                kitty_key(b'[' as u32, mods)
            } else {
                vec![0x1b]
            }
        }
        Key::Backslash if mods.ctrl => vec![0x1c],
        Key::CloseBracket if mods.ctrl => vec![0x1d],
        Key::Slash | Key::Minus if mods.ctrl => vec![0x1f],
        _ => {
            let letter = letter_of(key)?;
            if (kitty || kitty_all) && (mods.ctrl || mods.alt) && (mods.shift || (mods.ctrl && mods.alt)) {
                return Some(kitty_key(letter as u32, mods));
            }
            if mods.ctrl && letter.is_ascii_alphabetic() {
                let byte = (letter as u8) & 0x1f;
                if mods.alt { vec![0x1b, byte] } else { vec![byte] }
            } else if mods.alt && option_as_meta {
                let ch = if mods.shift {
                    letter.to_ascii_uppercase()
                } else {
                    letter
                };
                vec![0x1b, ch as u8]
            } else {
                return None;
            }
        }
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    None,
    WheelUp,
    WheelDown,
}

/// Encodes a mouse report for programs that enabled mouse tracking.
/// `col`/`row` are 0-based viewport cells.
pub fn encode_mouse(
    button: MouseButton,
    pressed: bool,
    motion: bool,
    col: usize,
    row: usize,
    mods: Modifiers,
    mode: TermMode,
) -> Option<Vec<u8>> {
    let mut code: u32 = match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::None => 3,
        MouseButton::WheelUp => 64,
        MouseButton::WheelDown => 65,
    };
    if motion {
        code += 32;
    }
    if mods.shift {
        code += 4;
    }
    if mods.alt {
        code += 8;
    }
    if mods.ctrl {
        code += 16;
    }
    if mode.contains(TermMode::SGR_MOUSE) {
        let suffix = if pressed { 'M' } else { 'm' };
        return Some(format!("\x1b[<{code};{};{}{suffix}", col + 1, row + 1).into_bytes());
    }
    // Legacy X10 encoding: release is button 3, coordinates capped at 223.
    if !pressed {
        code = (code & !3) | 3;
    }
    if col >= 223 || row >= 223 {
        return None;
    }
    Some(vec![
        0x1b,
        b'[',
        b'M',
        (32 + code) as u8,
        (33 + col) as u8,
        (33 + row) as u8,
    ])
}

/// Quotes a path for pasting into a shell or CLI prompt.
pub fn shell_quote(path: &str) -> String {
    if !path.is_empty() && path.chars().all(|c| c.is_alphanumeric() || "/._-+:@%,~".contains(c)) {
        path.to_string()
    } else {
        format!("'{}'", path.replace('\'', "'\\''"))
    }
}

/// Formats a relative path for `@mention` insertion in AI CLI prompts, escaping spaces.
pub fn mention_quote(path: &str) -> String {
    if !path.is_empty()
        && path
            .chars()
            .all(|c| !c.is_whitespace() && !matches!(c, '"' | '\'' | '\\'))
    {
        format!("@{path}")
    } else {
        format!("@\"{}\"", path.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

/// Splits user-provided extra CLI arguments, honouring simple quotes.
pub fn split_args(text: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut has_token = false;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), '\\') => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            (Some(_), c) => current.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                has_token = true;
            }
            (None, '\\') => {
                if let Some(next) = chars.next() {
                    current.push(next);
                    has_token = true;
                }
            }
            (None, c) if c.is_whitespace() => {
                if has_token || !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                    has_token = false;
                }
            }
            (None, c) => {
                current.push(c);
                has_token = true;
            }
        }
    }
    if has_token || !current.is_empty() {
        args.push(current);
    }
    args
}

/// Finds an http(s) URL around `col` in a line of terminal text.
pub fn url_at(line: &str, col: usize) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    if col >= chars.len() {
        return None;
    }
    let is_url_char = |c: char| !c.is_whitespace() && !"<>\"'`│┃|".contains(c);
    if !is_url_char(chars[col]) {
        return None;
    }
    let mut start = col;
    while start > 0 && is_url_char(chars[start - 1]) {
        start -= 1;
    }
    let mut end = col;
    while end + 1 < chars.len() && is_url_char(chars[end + 1]) {
        end += 1;
    }
    let token: String = chars[start..=end].iter().collect();
    let begin = token.find("https://").or_else(|| token.find("http://"))?;
    let url = token[begin..].trim_end_matches(['.', ',', ';', ':', ')', ']', '}', '!', '?']);
    (url.len() > 8).then(|| url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(shift: bool, alt: bool, ctrl: bool) -> Modifiers {
        Modifiers {
            alt,
            ctrl,
            shift,
            mac_cmd: false,
            command: false,
        }
    }

    #[test]
    fn enter_variants() {
        let none = TermMode::empty();
        assert_eq!(
            encode_key(Key::Enter, mods(false, false, false), none, false).unwrap(),
            b"\r"
        );
        assert_eq!(
            encode_key(Key::Enter, mods(true, false, false), none, false).unwrap(),
            b"\x1b\r"
        );
        let kitty = TermMode::DISAMBIGUATE_ESC_CODES;
        assert_eq!(
            encode_key(Key::Enter, mods(true, false, false), kitty, false).unwrap(),
            b"\x1b[13;2u"
        );
        assert_eq!(
            encode_key(Key::A, mods(true, false, true), kitty, false).unwrap(),
            b"\x1b[97;6u"
        );
    }

    #[test]
    fn arrows_follow_cursor_mode_and_modifiers() {
        let none = TermMode::empty();
        assert_eq!(
            encode_key(Key::ArrowUp, mods(false, false, false), none, false).unwrap(),
            b"\x1b[A"
        );
        assert_eq!(
            encode_key(Key::ArrowUp, mods(false, false, false), TermMode::APP_CURSOR, false).unwrap(),
            b"\x1bOA"
        );
        assert_eq!(
            encode_key(Key::ArrowLeft, mods(false, true, false), none, false).unwrap(),
            b"\x1bb"
        );
        assert_eq!(
            encode_key(Key::ArrowLeft, mods(false, false, true), none, false).unwrap(),
            b"\x1b[1;5D"
        );
    }

    #[test]
    fn control_letters_and_meta() {
        let none = TermMode::empty();
        assert_eq!(
            encode_key(Key::C, mods(false, false, true), none, false).unwrap(),
            vec![0x03]
        );
        assert_eq!(
            encode_key(Key::B, mods(false, true, false), none, true).unwrap(),
            b"\x1bb"
        );
        assert!(encode_key(Key::B, mods(false, true, false), none, false).is_none());
        assert!(encode_key(Key::B, mods(false, false, false), none, false).is_none());
    }

    #[test]
    fn sgr_mouse_buttons_and_motion() {
        let mode = TermMode::SGR_MOUSE | TermMode::MOUSE_REPORT_CLICK | TermMode::MOUSE_MOTION;
        assert_eq!(
            encode_mouse(MouseButton::WheelUp, true, false, 4, 2, mods(false, false, false), mode).unwrap(),
            b"\x1b[<64;5;3M"
        );
        assert_eq!(
            encode_mouse(MouseButton::Right, true, false, 1, 1, mods(false, false, false), mode).unwrap(),
            b"\x1b[<2;2;2M"
        );
        assert_eq!(
            encode_mouse(MouseButton::Middle, false, false, 1, 1, mods(false, false, false), mode).unwrap(),
            b"\x1b[<1;2;2m"
        );
        assert_eq!(
            encode_mouse(MouseButton::None, true, true, 1, 1, mods(false, false, false), mode).unwrap(),
            b"\x1b[<35;2;2M"
        );
    }

    #[test]
    fn quoting_splitting_and_urls() {
        assert_eq!(shell_quote("/tmp/a.png"), "/tmp/a.png");
        assert_eq!(shell_quote("/tmp/my file.png"), "'/tmp/my file.png'");
        assert_eq!(mention_quote("src/app.rs"), "@src/app.rs");
        assert_eq!(mention_quote("my docs/plan final.md"), "@\"my docs/plan final.md\"");
        assert_eq!(
            split_args("--model opus --append-system-prompt 'be brief'"),
            vec!["--model", "opus", "--append-system-prompt", "be brief"]
        );
        assert_eq!(
            url_at("see https://example.com/a). now", 8).as_deref(),
            Some("https://example.com/a")
        );
        assert_eq!(url_at("plain words", 3), None);
    }
}
