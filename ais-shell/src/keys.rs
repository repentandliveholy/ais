//! Rebindable keyboard shortcuts. Every binding includes ⌘ (Ctrl off macOS),
//! so plain keys and Ctrl/Option combos always reach the terminal.

use eframe::egui::{Key, Modifiers};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    Search,
    Find,
    OpenFolder,
    NewShell,
    SplitRight,
    SplitDown,
    ClosePane,
    CloseSpace,
    RestartPane,
    ClearScrollback,
    ZoomPane,
    FullscreenTerminal,
    ToggleWindowFullscreen,
    NextPane,
    PrevPane,
    NextSpace,
    PrevSpace,
    ToggleTitleBar,
    ToggleSidebar,
    ToggleFiles,
    ToggleUsage,
    ToggleAlerts,
    ToggleToolbar,
    ToggleAgents,
    TogglePaneHeaders,
    ToggleStatusBar,
    ShowConversations,
    RefreshSessions,
    Rescan,
    ToggleTheme,
    Settings,
    FontBigger,
    FontSmaller,
    FontReset,
}

impl Command {
    pub const ALL: [Command; 34] = [
        Command::Search,
        Command::Find,
        Command::OpenFolder,
        Command::NewShell,
        Command::SplitRight,
        Command::SplitDown,
        Command::ClosePane,
        Command::CloseSpace,
        Command::RestartPane,
        Command::ClearScrollback,
        Command::ZoomPane,
        Command::FullscreenTerminal,
        Command::ToggleWindowFullscreen,
        Command::NextPane,
        Command::PrevPane,
        Command::NextSpace,
        Command::PrevSpace,
        Command::ToggleTitleBar,
        Command::ToggleSidebar,
        Command::ToggleFiles,
        Command::ToggleUsage,
        Command::ToggleAlerts,
        Command::ToggleToolbar,
        Command::ToggleAgents,
        Command::TogglePaneHeaders,
        Command::ToggleStatusBar,
        Command::ShowConversations,
        Command::RefreshSessions,
        Command::Rescan,
        Command::ToggleTheme,
        Command::Settings,
        Command::FontBigger,
        Command::FontSmaller,
        Command::FontReset,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Command::Search => "search",
            Command::Find => "find",
            Command::OpenFolder => "open-folder",
            Command::NewShell => "new-shell",
            Command::SplitRight => "split-right",
            Command::SplitDown => "split-down",
            Command::ClosePane => "close-pane",
            Command::CloseSpace => "close-space",
            Command::RestartPane => "restart-pane",
            Command::ClearScrollback => "clear-scrollback",
            Command::ZoomPane => "zoom-pane",
            Command::FullscreenTerminal => "fullscreen-terminal",
            Command::ToggleWindowFullscreen => "toggle-window-fullscreen",
            Command::NextPane => "next-pane",
            Command::PrevPane => "prev-pane",
            Command::NextSpace => "next-space",
            Command::PrevSpace => "prev-space",
            Command::ToggleTitleBar => "toggle-title-bar",
            Command::ToggleSidebar => "toggle-sidebar",
            Command::ToggleFiles => "toggle-files",
            Command::ToggleUsage => "toggle-usage",
            Command::ToggleAlerts => "toggle-alerts",
            Command::ToggleToolbar => "toggle-toolbar",
            Command::ToggleAgents => "toggle-agents",
            Command::TogglePaneHeaders => "toggle-pane-headers",
            Command::ToggleStatusBar => "toggle-status-bar",
            Command::ShowConversations => "show-conversations",
            Command::RefreshSessions => "refresh-sessions",
            Command::Rescan => "rescan",
            Command::ToggleTheme => "toggle-theme",
            Command::Settings => "settings",
            Command::FontBigger => "font-bigger",
            Command::FontSmaller => "font-smaller",
            Command::FontReset => "font-reset",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Command::Search => "Search & run",
            Command::Find => "Find in scrollback",
            Command::OpenFolder => "Open folder",
            Command::NewShell => "New terminal",
            Command::SplitRight => "Split right",
            Command::SplitDown => "Split down",
            Command::ClosePane => "Close pane",
            Command::CloseSpace => "Close space",
            Command::RestartPane => "Restart pane",
            Command::ClearScrollback => "Clear scrollback",
            Command::ZoomPane => "Zoom active pane",
            Command::FullscreenTerminal => "Fullscreen terminal",
            Command::ToggleWindowFullscreen => "Window fullscreen",
            Command::NextPane => "Next pane",
            Command::PrevPane => "Previous pane",
            Command::NextSpace => "Next space",
            Command::PrevSpace => "Previous space",
            Command::ToggleTitleBar => "Toggle title bar",
            Command::ToggleSidebar => "Toggle sidebar",
            Command::ToggleFiles => "Toggle files",
            Command::ToggleUsage => "Toggle usage limits",
            Command::ToggleAlerts => "Toggle alerts",
            Command::ToggleToolbar => "Toggle project toolbar",
            Command::ToggleAgents => "Toggle agent pills",
            Command::TogglePaneHeaders => "Toggle pane headers",
            Command::ToggleStatusBar => "Toggle status bar",
            Command::ShowConversations => "Toggle conversations",
            Command::RefreshSessions => "Refresh conversations",
            Command::Rescan => "Rescan AI history",
            Command::ToggleTheme => "Cycle light / dark",
            Command::Settings => "Open settings",
            Command::FontBigger => "Bigger font",
            Command::FontSmaller => "Smaller font",
            Command::FontReset => "Reset font size",
        }
    }

    pub fn default_binding(self) -> Option<Binding> {
        let b = |key, shift, alt| {
            Some(Binding {
                key,
                shift,
                alt,
                ctrl: false,
            })
        };
        match self {
            Command::Search => b(Key::P, false, false),
            Command::Find => b(Key::F, false, false),
            Command::OpenFolder => b(Key::O, false, false),
            Command::NewShell => b(Key::T, false, false),
            Command::SplitRight => b(Key::D, false, false),
            Command::SplitDown => b(Key::D, true, false),
            Command::ClosePane => b(Key::W, false, false),
            Command::CloseSpace => b(Key::W, true, false),
            Command::RestartPane => b(Key::R, true, false),
            Command::ClearScrollback => b(Key::K, false, false),
            Command::ZoomPane => b(Key::Enter, false, false),
            Command::FullscreenTerminal => b(Key::Enter, true, false),
            Command::ToggleWindowFullscreen => None,
            Command::NextPane => b(Key::CloseBracket, false, false),
            Command::PrevPane => b(Key::OpenBracket, false, false),
            Command::NextSpace => b(Key::CloseBracket, true, false),
            Command::PrevSpace => b(Key::OpenBracket, true, false),
            Command::ToggleTitleBar => None,
            Command::ToggleSidebar => b(Key::B, false, false),
            Command::ToggleFiles => b(Key::E, true, false),
            Command::ToggleUsage => b(Key::U, false, false),
            Command::ToggleAlerts => b(Key::A, true, false),
            Command::ToggleToolbar => None,
            Command::ToggleAgents => b(Key::G, true, false),
            Command::TogglePaneHeaders => None,
            Command::ToggleStatusBar => None,
            Command::ShowConversations => b(Key::L, true, false),
            Command::RefreshSessions => b(Key::R, true, true),
            Command::Rescan => None,
            Command::ToggleTheme => b(Key::T, true, true),
            Command::Settings => b(Key::Comma, false, false),
            Command::FontBigger => b(Key::Equals, false, false),
            Command::FontSmaller => b(Key::Minus, false, false),
            Command::FontReset => b(Key::Num0, false, false),
        }
    }
}

/// How the implied primary modifier is written: ⌘ on macOS, Ctrl+ elsewhere.
pub const MOD: &str = if cfg!(target_os = "macos") { "⌘" } else { "Ctrl+" };

/// Formats a shortcut chord consistently for the current OS.
pub fn format_chord(ctrl: bool, alt: bool, shift: bool, key: &str) -> String {
    if cfg!(target_os = "macos") {
        let mut out = String::new();
        if ctrl {
            out.push('⌃');
        }
        if alt {
            out.push('⌥');
        }
        if shift {
            out.push('⇧');
        }
        out.push('⌘');
        out.push_str(key);
        out
    } else {
        let mut parts = vec!["Ctrl"];
        if alt {
            parts.push("Alt");
        }
        if shift {
            parts.push("Shift");
        }
        let mut out = parts.join("+");
        out.push('+');
        out.push_str(key);
        out
    }
}

/// A ⌘-chord (Ctrl+ on Linux). ⌘/Ctrl itself is implied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    pub key: Key,
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

impl Binding {
    /// Builds a binding from a key press; `None` unless ⌘ (or Ctrl off macOS) is held.
    pub fn from_press(key: Key, mods: Modifiers) -> Option<Self> {
        mods.command.then_some(Self {
            key,
            shift: mods.shift,
            alt: mods.alt,
            ctrl: mods.ctrl && cfg!(target_os = "macos"),
        })
    }

    #[cfg(test)]
    pub fn matches(self, key: Key, mods: Modifiers) -> bool {
        self.matches_with_plus_fallback(key, mods, false)
    }

    fn matches_with_plus_fallback(self, key: Key, mods: Modifiers, allow_shift_plus: bool) -> bool {
        let ctrl = mods.ctrl && cfg!(target_os = "macos");
        if !mods.command || self.alt != mods.alt || self.ctrl != ctrl {
            return false;
        }
        if allow_shift_plus
            && self.key == Key::Equals
            && !self.shift
            && (key == Key::Plus || (key == Key::Equals && mods.shift))
        {
            return true;
        }
        let key_ok = self.key == key || (self.key == Key::Equals && key == Key::Plus);
        key_ok && self.shift == mods.shift
    }

    pub fn label(self) -> String {
        format_chord(self.ctrl, self.alt, self.shift, &key_symbol(self.key))
    }

    pub fn to_id(self) -> String {
        let mut parts = vec!["cmd"];
        if self.ctrl {
            parts.push("ctrl");
        }
        if self.alt {
            parts.push("alt");
        }
        if self.shift {
            parts.push("shift");
        }
        let mut out = parts.join("+");
        out.push('+');
        out.push_str(self.key.name());
        out
    }

    pub fn from_id(id: &str) -> Option<Self> {
        let (mods, key) = id.rsplit_once('+')?;
        let key = Key::from_name(key)?;
        let has = |name: &str| mods.split('+').any(|part| part == name);
        Some(Self {
            key,
            shift: has("shift"),
            alt: has("alt"),
            ctrl: has("ctrl"),
        })
    }
}

fn key_symbol(key: Key) -> String {
    match key {
        Key::Enter => "↵".into(),
        Key::ArrowUp => "↑".into(),
        Key::ArrowDown => "↓".into(),
        Key::ArrowLeft => "←".into(),
        Key::ArrowRight => "→".into(),
        Key::Backspace => "⌫".into(),
        Key::Tab => "⇥".into(),
        Key::Space => "Space".into(),
        other => other.symbol_or_name().to_string(),
    }
}

/// Checks if a binding is shadowed by fixed navigation / workspace shortcuts.
pub fn fixed_conflict_reason(binding: Binding, nav_arrows: bool, nav_vim: bool) -> Option<&'static str> {
    if !binding.ctrl
        && !binding.alt
        && let Ok(digit) = binding.key.name().parse::<usize>()
    {
        if !binding.shift && (1..=9).contains(&digit) {
            return Some("reserved for switching project spaces");
        }
        if binding.shift && (1..=6).contains(&digit) {
            return Some("reserved for launching an AI CLI");
        }
    }
    if binding.alt && !binding.shift && !binding.ctrl {
        if nav_arrows
            && matches!(
                binding.key,
                Key::ArrowUp | Key::ArrowDown | Key::ArrowLeft | Key::ArrowRight
            )
        {
            return Some("used by arrow navigation");
        }
        if nav_vim && matches!(binding.key, Key::H | Key::J | Key::K | Key::L) {
            return Some("used by vim navigation");
        }
    }
    None
}

/// User overrides on top of the defaults: id → binding id, `""` = unbound.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Keymap {
    pub overrides: BTreeMap<String, String>,
}

impl Keymap {
    /// Parses overrides from `settings.json`, dropping invalid bindings or
    /// duplicate chords and returning human-readable warnings.
    pub fn from_overrides(raw: BTreeMap<String, String>) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let mut overrides = BTreeMap::new();
        for (cmd_id, bind_id) in raw {
            let Some(command) = Command::ALL.into_iter().find(|cmd| cmd.id() == cmd_id) else {
                warnings.push(format!("Ignored unknown shortcut command '{cmd_id}'"));
                continue;
            };
            if bind_id.is_empty() {
                overrides.insert(cmd_id, bind_id);
                continue;
            }
            let Some(binding) = Binding::from_id(&bind_id) else {
                warnings.push(format!(
                    "Ignored invalid shortcut '{bind_id}' for “{}” (reverted to default)",
                    command.label()
                ));
                continue;
            };
            if Some(binding) != command.default_binding() {
                overrides.insert(cmd_id, binding.to_id());
            }
        }
        let mut map = Self { overrides };
        let mut seen: Vec<(Binding, Command)> = Vec::new();
        for command in Command::ALL {
            if let Some(binding) = map.binding(command) {
                if let Some((_, existing)) = seen.iter().find(|(b, _)| *b == binding) {
                    warnings.push(format!(
                        "Shortcut {} collided between “{}” and “{}”; unbound “{}”",
                        binding.label(),
                        existing.label(),
                        command.label(),
                        command.label()
                    ));
                    map.set(command, None);
                } else {
                    seen.push((binding, command));
                }
            }
        }
        (map, warnings)
    }

    pub fn binding(&self, command: Command) -> Option<Binding> {
        match self.overrides.get(command.id()) {
            Some(id) if id.is_empty() => None,
            Some(id) => Binding::from_id(id).or_else(|| command.default_binding()),
            None => command.default_binding(),
        }
    }

    pub fn set(&mut self, command: Command, binding: Option<Binding>) {
        if binding == command.default_binding() {
            self.overrides.remove(command.id());
        } else {
            let id = binding.map(|binding| binding.to_id()).unwrap_or_default();
            self.overrides.insert(command.id().to_string(), id);
        }
    }

    pub fn hint(&self, command: Command) -> String {
        self.binding(command).map(|binding| binding.label()).unwrap_or_default()
    }

    pub fn lookup(&self, key: Key, mods: Modifiers) -> Option<Command> {
        Command::ALL
            .into_iter()
            .find(|command| {
                self.binding(*command)
                    .is_some_and(|binding| binding.matches_with_plus_fallback(key, mods, false))
            })
            .or_else(|| {
                Command::ALL.into_iter().find(|command| {
                    self.binding(*command)
                        .is_some_and(|binding| binding.matches_with_plus_fallback(key, mods, true))
                })
            })
    }

    /// Another command already using `binding`.
    pub fn conflict(&self, command: Command, binding: Binding) -> Option<Command> {
        Command::ALL
            .into_iter()
            .find(|other| *other != command && self.binding(*other) == Some(binding))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bindings_round_trip_and_match() {
        let binding = Binding {
            key: Key::D,
            shift: true,
            alt: false,
            ctrl: false,
        };
        assert_eq!(Binding::from_id(&binding.to_id()), Some(binding));
        assert!(binding.matches(Key::D, Modifiers::COMMAND | Modifiers::SHIFT));
        assert!(!binding.matches(Key::D, Modifiers::COMMAND));
        let mut keymap = Keymap::default();
        assert_eq!(
            keymap.lookup(Key::D, Modifiers::COMMAND | Modifiers::SHIFT),
            Some(Command::SplitDown)
        );
        keymap.set(Command::SplitDown, None);
        assert_eq!(keymap.lookup(Key::D, Modifiers::COMMAND | Modifiers::SHIFT), None);
        keymap.set(Command::SplitDown, Command::SplitDown.default_binding());
        assert!(keymap.overrides.is_empty());
        assert_eq!(keymap.conflict(Command::Search, binding), Some(Command::SplitDown));
    }

    #[test]
    fn default_bindings_are_unique_and_non_conflicting() {
        let keymap = Keymap::default();
        for command in Command::ALL {
            if let Some(binding) = command.default_binding() {
                assert_eq!(keymap.conflict(command, binding), None, "{}", command.label());
                assert_eq!(fixed_conflict_reason(binding, true, true), None, "{}", command.label());
            }
        }
    }

    #[test]
    fn shift_plus_matches_font_bigger() {
        let keymap = Keymap::default();
        assert_eq!(
            keymap.lookup(Key::Equals, Modifiers::COMMAND | Modifiers::SHIFT),
            Some(Command::FontBigger)
        );
        assert_eq!(
            keymap.lookup(Key::Plus, Modifiers::COMMAND | Modifiers::SHIFT),
            Some(Command::FontBigger)
        );
    }

    #[test]
    fn detects_fixed_conflicts_and_invalid_overrides() {
        let b_space = Binding {
            key: Key::Num2,
            shift: false,
            alt: false,
            ctrl: false,
        };
        assert!(fixed_conflict_reason(b_space, true, false).is_some());
        let b_vim = Binding {
            key: Key::J,
            shift: false,
            alt: true,
            ctrl: false,
        };
        assert!(fixed_conflict_reason(b_vim, true, true).is_some());
        assert!(fixed_conflict_reason(b_vim, true, false).is_none());

        let mut raw = BTreeMap::new();
        raw.insert("search".into(), "garbage-binding".into());
        raw.insert("toggle-sidebar".into(), "cmd+p".into());
        let (keymap, warnings) = Keymap::from_overrides(raw);
        assert_eq!(warnings.len(), 2);
        assert_eq!(keymap.binding(Command::Search), Command::Search.default_binding());
        assert_eq!(keymap.binding(Command::ToggleSidebar), None);
    }
}
