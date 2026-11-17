//! UI themes, terminal color tables, typography scale and persisted settings.

use crate::cli::canonical_path;
use crate::view::TermColors;
use eframe::egui::Color32;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

pub const DEFAULT_FONT_SIZE: f32 = 14.0;
pub const SCHEMA_VERSION: u32 = 1;

/// Unified type & spacing scale used across UI modules.
pub mod scale {
    pub const FONT_XS: f32 = 10.5;
    pub const FONT_SM: f32 = 11.5;
    pub const FONT_MD: f32 = 13.0;
    pub const FONT_LG: f32 = 14.0;
    pub const FONT_XL: f32 = 17.0;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    Dark,
    Light,
    /// Follow the operating system's light / dark appearance.
    System,
}

impl Theme {
    pub fn label(self) -> &'static str {
        match self {
            Theme::Dark => "Dark",
            Theme::Light => "Light",
            Theme::System => "System",
        }
    }

    pub const ALL: [Theme; 3] = [Theme::System, Theme::Light, Theme::Dark];

    pub fn next(self) -> Self {
        match self {
            Theme::System => Theme::Light,
            Theme::Light => Theme::Dark,
            Theme::Dark => Theme::System,
        }
    }

    /// The concrete theme to paint with; `System` follows `system_light`.
    pub fn resolve(self, system_light: bool) -> Self {
        match self {
            Theme::System if system_light => Theme::Light,
            Theme::System => Theme::Dark,
            other => other,
        }
    }
}

#[derive(Clone, Copy)]
pub struct TermTable {
    pub bg: Color32,
    pub fg: Color32,
    pub selection: Color32,
    pub ansi: [Color32; 16],
}

pub fn rgb(value: [u8; 3]) -> Color32 {
    Color32::from_rgb(value[0], value[1], value[2])
}

const fn c(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

/// UI colors. Dark mirrors the curses `ais` brand palette.
#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Color32,
    pub chrome: Color32,
    pub panel: Color32,
    pub raised: Color32,
    pub border: Color32,
    pub border_strong: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub faint: Color32,
    pub accent: Color32,
    pub accent_text: Color32,
    pub green: Color32,
    pub amber: Color32,
    pub red: Color32,
    pub cyan: Color32,
    pub hover: Color32,
    pub light: bool,
    /// 1.0 = full color, lower mutes accent tints (CLI colors), 0.0 = grayscale.
    pub saturation: f32,
    pub term: TermColors,
}

impl Palette {
    pub fn dark() -> Self {
        let bg = c(13, 15, 20);
        Self {
            bg,
            chrome: c(17, 19, 26),
            panel: c(20, 23, 31),
            raised: c(28, 32, 42),
            border: c(37, 42, 54),
            border_strong: c(56, 63, 80),
            text: c(232, 236, 244),
            muted: c(162, 172, 192),
            faint: c(116, 126, 148),
            accent: c(185, 163, 255),
            accent_text: c(18, 16, 30),
            green: c(149, 225, 180),
            amber: c(243, 202, 134),
            red: c(242, 145, 166),
            cyan: c(115, 216, 244),
            hover: c(33, 37, 49),
            light: false,
            saturation: 1.0,
            term: TermColors {
                bg: c(11, 13, 18),
                fg: c(222, 227, 236),
                cursor: c(185, 163, 255),
                selection: c(62, 56, 104),
                ansi: [
                    c(30, 33, 42),
                    c(242, 120, 140),
                    c(149, 225, 160),
                    c(243, 202, 134),
                    c(130, 170, 255),
                    c(198, 160, 246),
                    c(115, 216, 244),
                    c(214, 220, 230),
                    c(92, 102, 122),
                    c(255, 145, 165),
                    c(175, 240, 185),
                    c(250, 220, 160),
                    c(160, 192, 255),
                    c(215, 185, 255),
                    c(150, 228, 250),
                    c(246, 248, 252),
                ],
            },
        }
    }

    pub fn light() -> Self {
        Self {
            bg: c(243, 244, 248),
            chrome: c(236, 238, 243),
            panel: c(250, 250, 252),
            raised: c(255, 255, 255),
            border: c(219, 222, 230),
            border_strong: c(196, 200, 212),
            text: c(26, 30, 40),
            muted: c(82, 90, 108),
            faint: c(110, 118, 136),
            accent: c(108, 79, 216),
            accent_text: c(255, 255, 255),
            green: c(28, 130, 86),
            amber: c(166, 106, 8),
            red: c(196, 52, 76),
            cyan: c(16, 124, 150),
            hover: c(232, 234, 241),
            light: true,
            saturation: 1.0,
            term: TermColors {
                bg: c(253, 253, 254),
                fg: c(36, 40, 50),
                cursor: c(108, 79, 216),
                selection: c(214, 205, 250),
                ansi: [
                    c(40, 44, 52),
                    c(200, 50, 70),
                    c(30, 130, 70),
                    c(150, 100, 0),
                    c(40, 90, 190),
                    c(140, 60, 170),
                    c(10, 120, 140),
                    c(90, 95, 105),
                    c(120, 125, 135),
                    c(220, 70, 90),
                    c(50, 150, 90),
                    c(175, 125, 20),
                    c(60, 110, 210),
                    c(160, 80, 190),
                    c(30, 140, 160),
                    c(40, 44, 52),
                ],
            },
        }
    }

    pub fn for_theme(theme: Theme, scheme: UiTheme, terminal: TerminalPalette) -> Self {
        let light = matches!(theme, Theme::Light);
        let mut palette = scheme.palette(light);
        if let Some(table) = terminal.colors() {
            palette.term.bg = table.bg;
            palette.term.fg = table.fg;
            palette.term.selection = table.selection;
            palette.term.ansi = table.ansi;
        }
        palette
    }

    /// Builds a full UI palette from a theme's key colors; surfaces, borders
    /// and secondary text are guaranteed to meet WCAG contrast targets.
    fn from_seed(seed: &Seed, light: bool, base: Palette) -> Self {
        let bg = rgb(seed.bg);
        let panel = rgb(seed.panel);
        let text = ensure_contrast(rgb(seed.text), panel, 7.0, light);
        let accent = rgb(seed.accent);
        let white = c(255, 255, 255);
        let dark_ink = c(16, 18, 24);
        let (chrome, raised) = if light {
            (mix(bg, text, 0.035), mix(panel, white, 0.6))
        } else {
            (mix(bg, panel, 0.5), mix(panel, text, 0.06))
        };
        let term_bg = if light {
            mix(panel, white, 0.5)
        } else {
            mix(bg, panel, 0.3)
        };
        let muted = ensure_contrast(mix(text, panel, 0.25), bg, 4.6, light);
        let muted = ensure_contrast(muted, panel, 4.6, light);
        let faint = ensure_contrast(mix(text, panel, 0.43), bg, 3.2, light);
        let faint = ensure_contrast(faint, panel, 3.2, light);
        let accent_text = if contrast_ratio(white, accent) >= contrast_ratio(dark_ink, accent) {
            white
        } else {
            dark_ink
        };
        Self {
            bg,
            chrome,
            panel,
            raised,
            border: mix(panel, text, if light { 0.14 } else { 0.13 }),
            border_strong: mix(panel, text, 0.25),
            text,
            muted,
            faint,
            accent,
            accent_text,
            green: rgb(seed.green),
            amber: rgb(seed.amber),
            red: rgb(seed.red),
            cyan: rgb(seed.cyan),
            hover: mix(panel, text, if light { 0.06 } else { 0.07 }),
            light,
            saturation: 1.0,
            term: TermColors {
                bg: term_bg,
                fg: text,
                cursor: accent,
                selection: mix(term_bg, accent, 0.3),
                ansi: base.term.ansi,
            },
        }
    }
}

fn ensure_contrast(color: Color32, against: Color32, min_ratio: f32, light_bg: bool) -> Color32 {
    if contrast_ratio(color, against) >= min_ratio {
        return color;
    }
    let target = if light_bg { c(10, 12, 18) } else { c(248, 250, 255) };
    for step in 1..=20 {
        let candidate = mix(color, target, step as f32 / 20.0);
        if contrast_ratio(candidate, against) >= min_ratio {
            return candidate;
        }
    }
    target
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let lerp = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(lerp(a.r(), b.r()), lerp(a.g(), b.g()), lerp(a.b(), b.b()))
}

/// Moves a color toward its own gray; `amount` 1.0 keeps it, 0.0 is gray.
pub fn desaturate(color: Color32, amount: f32) -> Color32 {
    let gray = (luminance(color) * 255.0).round() as u8;
    mix(Color32::from_rgb(gray, gray, gray), color, amount)
}

fn luminance(color: Color32) -> f32 {
    (0.2126 * color.r() as f32 + 0.7152 * color.g() as f32 + 0.0722 * color.b() as f32) / 255.0
}

pub fn wcag_luminance(color: Color32) -> f32 {
    let channel = |v: u8| {
        let s = f32::from(v) / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
}

pub fn contrast_ratio(a: Color32, b: Color32) -> f32 {
    let la = wcag_luminance(a);
    let lb = wcag_luminance(b);
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Key colors of one theme variant.
struct Seed {
    bg: [u8; 3],
    panel: [u8; 3],
    text: [u8; 3],
    accent: [u8; 3],
    green: [u8; 3],
    amber: [u8; 3],
    red: [u8; 3],
    cyan: [u8; 3],
}

#[allow(clippy::too_many_arguments)]
const fn seed(
    bg: [u8; 3],
    panel: [u8; 3],
    text: [u8; 3],
    accent: [u8; 3],
    green: [u8; 3],
    amber: [u8; 3],
    red: [u8; 3],
    cyan: [u8; 3],
) -> Seed {
    Seed {
        bg,
        panel,
        text,
        accent,
        green,
        amber,
        red,
        cyan,
    }
}

/// Interface color theme; each has a dark and a light variant, picked by
/// the Light / Dark / System mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiTheme {
    Ais,
    AisSoft,
    Mono,
    Nord,
    Nocturne,
    TokyoNight,
    Gruvbox,
    Catppuccin,
    Solarized,
    GitHub,
    One,
    RosePine,
    Everforest,
    Kanagawa,
    Ayu,
}

impl UiTheme {
    pub const ALL: [UiTheme; 15] = [
        UiTheme::Ais,
        UiTheme::AisSoft,
        UiTheme::Mono,
        UiTheme::Nord,
        UiTheme::Nocturne,
        UiTheme::TokyoNight,
        UiTheme::Gruvbox,
        UiTheme::Catppuccin,
        UiTheme::Solarized,
        UiTheme::GitHub,
        UiTheme::One,
        UiTheme::RosePine,
        UiTheme::Everforest,
        UiTheme::Kanagawa,
        UiTheme::Ayu,
    ];

    pub fn label(self) -> &'static str {
        match self {
            UiTheme::Ais => "ais",
            UiTheme::AisSoft => "ais Soft",
            UiTheme::Mono => "Mono (black & white)",
            UiTheme::Nord => "Nord",
            UiTheme::Nocturne => "Nocturne",
            UiTheme::TokyoNight => "Tokyo Night",
            UiTheme::Gruvbox => "Gruvbox",
            UiTheme::Catppuccin => "Catppuccin",
            UiTheme::Solarized => "Solarized",
            UiTheme::GitHub => "GitHub",
            UiTheme::One => "One",
            UiTheme::RosePine => "Rosé Pine",
            UiTheme::Everforest => "Everforest",
            UiTheme::Kanagawa => "Kanagawa",
            UiTheme::Ayu => "Ayu",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            UiTheme::Ais => "ais",
            UiTheme::AisSoft => "ais-soft",
            UiTheme::Mono => "mono",
            UiTheme::Nord => "nord",
            UiTheme::Nocturne => "nocturne",
            UiTheme::TokyoNight => "tokyo-night",
            UiTheme::Gruvbox => "gruvbox",
            UiTheme::Catppuccin => "catppuccin",
            UiTheme::Solarized => "solarized",
            UiTheme::GitHub => "github",
            UiTheme::One => "one",
            UiTheme::RosePine => "rose-pine",
            UiTheme::Everforest => "everforest",
            UiTheme::Kanagawa => "kanagawa",
            UiTheme::Ayu => "ayu",
        }
    }

    pub fn from_id(id: Option<&str>) -> Self {
        let id = if id == Some("dracula") { Some("nocturne") } else { id };
        Self::ALL
            .into_iter()
            .find(|theme| Some(theme.id()) == id)
            .unwrap_or(UiTheme::Ais)
    }

    /// Terminal colors that belong to this theme (used when the terminal
    /// palette is "match UI").
    fn terminal(self, light: bool) -> Option<TerminalPalette> {
        match (self, light) {
            (UiTheme::Nord, false) => Some(TerminalPalette::Nord),
            (UiTheme::Nocturne, false) => Some(TerminalPalette::Dracula),
            (UiTheme::TokyoNight, false) => Some(TerminalPalette::TokyoNight),
            (UiTheme::Gruvbox, false) => Some(TerminalPalette::Gruvbox),
            (UiTheme::Catppuccin, false) => Some(TerminalPalette::Catppuccin),
            (UiTheme::Solarized, false) => Some(TerminalPalette::SolarizedDark),
            (UiTheme::Solarized, true) => Some(TerminalPalette::SolarizedLight),
            _ => None,
        }
    }

    fn seed(self, light: bool) -> Option<Seed> {
        Some(match (self, light) {
            (UiTheme::Ais, _) => return None,
            (UiTheme::AisSoft, false) => seed(
                [22, 24, 30],
                [28, 31, 39],
                [205, 210, 220],
                [160, 148, 210],
                [140, 190, 160],
                [210, 180, 130],
                [205, 140, 155],
                [125, 185, 205],
            ),
            (UiTheme::AisSoft, true) => seed(
                [232, 233, 237],
                [240, 240, 243],
                [50, 54, 64],
                [112, 96, 180],
                [50, 120, 86],
                [150, 104, 30],
                [170, 70, 88],
                [40, 112, 135],
            ),
            (UiTheme::Mono, false) => seed(
                [8, 8, 8],
                [17, 17, 17],
                [236, 236, 236],
                [236, 236, 236],
                [175, 175, 175],
                [205, 205, 205],
                [255, 255, 255],
                [190, 190, 190],
            ),
            (UiTheme::Mono, true) => seed(
                [236, 236, 236],
                [250, 250, 250],
                [17, 17, 17],
                [17, 17, 17],
                [85, 85, 85],
                [55, 55, 55],
                [0, 0, 0],
                [70, 70, 70],
            ),
            (UiTheme::Nord, false) => seed(
                [36, 41, 51],
                [46, 52, 64],
                [236, 239, 244],
                [136, 192, 208],
                [163, 190, 140],
                [235, 203, 139],
                [191, 97, 106],
                [143, 188, 187],
            ),
            (UiTheme::Nord, true) => seed(
                [229, 233, 240],
                [236, 239, 244],
                [46, 52, 64],
                [94, 129, 172],
                [86, 128, 66],
                [160, 112, 24],
                [176, 68, 79],
                [52, 120, 134],
            ),
            (UiTheme::Nocturne, false) => seed(
                [33, 34, 44],
                [40, 42, 54],
                [248, 248, 242],
                [189, 147, 249],
                [80, 250, 123],
                [255, 184, 108],
                [255, 85, 85],
                [139, 233, 253],
            ),
            (UiTheme::Nocturne, true) => seed(
                [245, 241, 222],
                [255, 251, 235],
                [31, 31, 31],
                [100, 74, 201],
                [20, 113, 10],
                [163, 77, 20],
                [203, 58, 42],
                [3, 106, 150],
            ),
            (UiTheme::TokyoNight, false) => seed(
                [22, 22, 30],
                [26, 27, 38],
                [192, 202, 245],
                [122, 162, 247],
                [158, 206, 106],
                [224, 175, 104],
                [247, 118, 142],
                [125, 207, 255],
            ),
            (UiTheme::TokyoNight, true) => seed(
                [208, 213, 227],
                [225, 226, 231],
                [52, 59, 88],
                [46, 125, 233],
                [88, 117, 57],
                [140, 108, 62],
                [245, 42, 101],
                [0, 113, 151],
            ),
            (UiTheme::Gruvbox, false) => seed(
                [29, 32, 33],
                [40, 40, 40],
                [235, 219, 178],
                [254, 128, 25],
                [184, 187, 38],
                [250, 189, 47],
                [251, 73, 52],
                [142, 192, 124],
            ),
            (UiTheme::Gruvbox, true) => seed(
                [242, 229, 188],
                [251, 241, 199],
                [60, 56, 54],
                [175, 58, 3],
                [121, 116, 14],
                [181, 118, 20],
                [157, 0, 6],
                [66, 123, 88],
            ),
            (UiTheme::Catppuccin, false) => seed(
                [17, 17, 27],
                [30, 30, 46],
                [205, 214, 244],
                [203, 166, 247],
                [166, 227, 161],
                [249, 226, 175],
                [243, 139, 168],
                [137, 220, 235],
            ),
            (UiTheme::Catppuccin, true) => seed(
                [220, 224, 232],
                [239, 241, 245],
                [76, 79, 105],
                [136, 57, 239],
                [64, 160, 43],
                [223, 142, 29],
                [210, 15, 57],
                [23, 146, 153],
            ),
            (UiTheme::Solarized, false) => seed(
                [0, 33, 43],
                [0, 43, 54],
                [197, 205, 205],
                [38, 139, 210],
                [133, 153, 0],
                [181, 137, 0],
                [220, 50, 47],
                [42, 161, 152],
            ),
            (UiTheme::Solarized, true) => seed(
                [238, 232, 213],
                [253, 246, 227],
                [7, 54, 66],
                [38, 139, 210],
                [133, 153, 0],
                [181, 137, 0],
                [220, 50, 47],
                [42, 161, 152],
            ),
            (UiTheme::GitHub, false) => seed(
                [1, 4, 9],
                [13, 17, 23],
                [230, 237, 243],
                [47, 129, 247],
                [63, 185, 80],
                [210, 153, 34],
                [248, 81, 73],
                [57, 197, 207],
            ),
            (UiTheme::GitHub, true) => seed(
                [246, 248, 250],
                [255, 255, 255],
                [31, 35, 40],
                [9, 105, 218],
                [26, 127, 55],
                [154, 103, 0],
                [207, 34, 46],
                [27, 124, 131],
            ),
            (UiTheme::One, false) => seed(
                [33, 37, 43],
                [40, 44, 52],
                [215, 218, 224],
                [97, 175, 239],
                [152, 195, 121],
                [229, 192, 123],
                [224, 108, 117],
                [86, 182, 194],
            ),
            (UiTheme::One, true) => seed(
                [234, 234, 235],
                [250, 250, 250],
                [56, 58, 66],
                [64, 120, 242],
                [80, 161, 79],
                [193, 132, 1],
                [228, 86, 73],
                [1, 132, 188],
            ),
            (UiTheme::RosePine, false) => seed(
                [25, 23, 36],
                [31, 29, 46],
                [224, 222, 244],
                [196, 167, 231],
                [143, 193, 160],
                [246, 193, 119],
                [235, 111, 146],
                [156, 207, 216],
            ),
            (UiTheme::RosePine, true) => seed(
                [242, 233, 225],
                [250, 244, 237],
                [87, 82, 121],
                [144, 122, 169],
                [70, 130, 100],
                [234, 157, 52],
                [180, 99, 122],
                [86, 148, 159],
            ),
            (UiTheme::Everforest, false) => seed(
                [35, 42, 46],
                [45, 53, 59],
                [211, 198, 170],
                [127, 187, 179],
                [167, 192, 128],
                [219, 188, 127],
                [230, 126, 128],
                [131, 192, 146],
            ),
            (UiTheme::Everforest, true) => seed(
                [239, 235, 212],
                [253, 246, 227],
                [92, 106, 114],
                [58, 148, 197],
                [141, 161, 1],
                [223, 160, 0],
                [248, 85, 82],
                [53, 167, 124],
            ),
            (UiTheme::Kanagawa, false) => seed(
                [22, 22, 29],
                [31, 31, 40],
                [220, 215, 186],
                [126, 156, 216],
                [152, 187, 108],
                [230, 195, 132],
                [228, 104, 118],
                [127, 180, 202],
            ),
            (UiTheme::Kanagawa, true) => seed(
                [229, 221, 176],
                [242, 236, 188],
                [84, 84, 100],
                [77, 105, 155],
                [111, 137, 78],
                [119, 113, 63],
                [200, 64, 83],
                [89, 123, 117],
            ),
            (UiTheme::Ayu, false) => seed(
                [11, 14, 20],
                [13, 16, 23],
                [217, 215, 206],
                [230, 180, 80],
                [170, 217, 76],
                [255, 180, 84],
                [240, 113, 120],
                [149, 230, 203],
            ),
            (UiTheme::Ayu, true) => seed(
                [243, 244, 245],
                [252, 252, 252],
                [92, 97, 102],
                [255, 153, 64],
                [134, 179, 0],
                [242, 174, 73],
                [240, 113, 113],
                [76, 191, 153],
            ),
        })
    }

    pub fn palette(self, light: bool) -> Palette {
        let base = if light { Palette::light() } else { Palette::dark() };
        let Some(seed) = self.seed(light) else {
            return base;
        };
        let mut palette = Palette::from_seed(&seed, light, base);
        if let Some(table) = self.terminal(light).and_then(TerminalPalette::colors) {
            palette.term.bg = table.bg;
            palette.term.fg = table.fg;
            palette.term.selection = table.selection;
            palette.term.ansi = table.ansi;
        }
        match self {
            UiTheme::AisSoft => {
                palette.saturation = 0.6;
                let bg = palette.term.bg;
                palette.term.ansi = palette.term.ansi.map(|color| mix(desaturate(color, 0.6), bg, 0.12));
            }
            UiTheme::Mono => {
                palette.saturation = 0.0;
                palette.term.ansi = palette.term.ansi.map(|color| desaturate(color, 0.0));
                palette.term.selection = desaturate(palette.term.selection, 0.0);
            }
            _ => {}
        }
        palette
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalPalette {
    Auto,
    Dracula,
    Nord,
    TokyoNight,
    Gruvbox,
    Catppuccin,
    SolarizedDark,
    SolarizedLight,
}

impl TerminalPalette {
    pub const ALL: [TerminalPalette; 8] = [
        TerminalPalette::Auto,
        TerminalPalette::Dracula,
        TerminalPalette::Nord,
        TerminalPalette::TokyoNight,
        TerminalPalette::Gruvbox,
        TerminalPalette::Catppuccin,
        TerminalPalette::SolarizedDark,
        TerminalPalette::SolarizedLight,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TerminalPalette::Auto => "ais (match UI)",
            TerminalPalette::Dracula => "Nocturne",
            TerminalPalette::Nord => "Nord",
            TerminalPalette::TokyoNight => "Tokyo Night",
            TerminalPalette::Gruvbox => "Gruvbox",
            TerminalPalette::Catppuccin => "Catppuccin Mocha",
            TerminalPalette::SolarizedDark => "Solarized Dark",
            TerminalPalette::SolarizedLight => "Solarized Light",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            TerminalPalette::Auto => "auto",
            TerminalPalette::Dracula => "dracula",
            TerminalPalette::Nord => "nord",
            TerminalPalette::TokyoNight => "tokyo-night",
            TerminalPalette::Gruvbox => "gruvbox",
            TerminalPalette::Catppuccin => "catppuccin",
            TerminalPalette::SolarizedDark => "solarized-dark",
            TerminalPalette::SolarizedLight => "solarized-light",
        }
    }

    pub fn colors(self) -> Option<TermTable> {
        let table = |bg: [u8; 3], fg: [u8; 3], selection: [u8; 3], ansi: [[u8; 3]; 16]| {
            Some(TermTable {
                bg: rgb(bg),
                fg: rgb(fg),
                selection: rgb(selection),
                ansi: ansi.map(rgb),
            })
        };
        match self {
            TerminalPalette::Auto => None,
            TerminalPalette::Dracula => table(
                [40, 42, 54],
                [248, 248, 242],
                [68, 71, 90],
                [
                    [33, 34, 44],
                    [255, 85, 85],
                    [80, 250, 123],
                    [241, 250, 140],
                    [189, 147, 249],
                    [255, 121, 198],
                    [139, 233, 253],
                    [248, 248, 242],
                    [98, 114, 164],
                    [255, 110, 110],
                    [105, 255, 148],
                    [255, 255, 165],
                    [214, 172, 255],
                    [255, 146, 223],
                    [164, 243, 255],
                    [255, 255, 255],
                ],
            ),
            TerminalPalette::Nord => table(
                [46, 52, 64],
                [216, 222, 233],
                [67, 76, 94],
                [
                    [59, 66, 82],
                    [191, 97, 106],
                    [163, 190, 140],
                    [235, 203, 139],
                    [129, 161, 193],
                    [180, 142, 173],
                    [136, 192, 208],
                    [229, 233, 240],
                    [76, 86, 106],
                    [191, 97, 106],
                    [163, 190, 140],
                    [235, 203, 139],
                    [129, 161, 193],
                    [180, 142, 173],
                    [143, 188, 187],
                    [236, 239, 244],
                ],
            ),
            TerminalPalette::TokyoNight => table(
                [26, 27, 38],
                [192, 202, 245],
                [51, 70, 124],
                [
                    [21, 22, 30],
                    [247, 118, 142],
                    [158, 206, 106],
                    [224, 175, 104],
                    [122, 162, 247],
                    [187, 154, 247],
                    [125, 207, 255],
                    [192, 202, 245],
                    [65, 72, 104],
                    [255, 137, 157],
                    [185, 223, 143],
                    [255, 199, 119],
                    [122, 162, 247],
                    [199, 177, 255],
                    [166, 227, 255],
                    [192, 202, 245],
                ],
            ),
            TerminalPalette::Gruvbox => table(
                [40, 40, 40],
                [235, 219, 178],
                [80, 73, 69],
                [
                    [40, 40, 40],
                    [204, 36, 29],
                    [152, 151, 26],
                    [215, 153, 33],
                    [69, 133, 136],
                    [177, 98, 134],
                    [104, 157, 106],
                    [168, 153, 132],
                    [146, 131, 116],
                    [251, 73, 52],
                    [184, 187, 38],
                    [250, 189, 47],
                    [131, 165, 152],
                    [211, 134, 155],
                    [142, 192, 124],
                    [235, 219, 178],
                ],
            ),
            TerminalPalette::Catppuccin => table(
                [30, 30, 46],
                [205, 214, 244],
                [69, 71, 110],
                [
                    [69, 71, 90],
                    [243, 139, 168],
                    [166, 227, 161],
                    [249, 226, 175],
                    [137, 180, 250],
                    [203, 166, 247],
                    [148, 226, 213],
                    [186, 194, 222],
                    [88, 91, 112],
                    [243, 139, 168],
                    [166, 227, 161],
                    [249, 226, 175],
                    [137, 180, 250],
                    [203, 166, 247],
                    [148, 226, 213],
                    [205, 214, 244],
                ],
            ),
            TerminalPalette::SolarizedDark => table(
                [0, 43, 54],
                [147, 161, 161],
                [7, 54, 66],
                [
                    [7, 54, 66],
                    [220, 50, 47],
                    [133, 153, 0],
                    [181, 137, 0],
                    [38, 139, 210],
                    [211, 54, 130],
                    [42, 161, 152],
                    [238, 232, 213],
                    [0, 43, 54],
                    [203, 75, 22],
                    [88, 110, 117],
                    [101, 123, 131],
                    [131, 148, 150],
                    [108, 113, 196],
                    [147, 161, 161],
                    [253, 246, 227],
                ],
            ),
            TerminalPalette::SolarizedLight => table(
                [253, 246, 227],
                [101, 123, 131],
                [238, 232, 213],
                [
                    [7, 54, 66],
                    [220, 50, 47],
                    [133, 153, 0],
                    [181, 137, 0],
                    [38, 139, 210],
                    [211, 54, 130],
                    [42, 161, 152],
                    [238, 232, 213],
                    [0, 43, 54],
                    [203, 75, 22],
                    [88, 110, 117],
                    [101, 123, 131],
                    [131, 148, 150],
                    [108, 113, 196],
                    [147, 161, 161],
                    [253, 246, 227],
                ],
            ),
        }
    }
}

/// Font families offered in Settings. Nerd Fonts carry extra icon glyphs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontChoice {
    JetBrains,
    FiraCode,
    Hack,
    System,
}

impl FontChoice {
    pub const ALL: [FontChoice; 4] = [
        FontChoice::JetBrains,
        FontChoice::FiraCode,
        FontChoice::Hack,
        FontChoice::System,
    ];

    pub fn label(self) -> &'static str {
        match self {
            FontChoice::JetBrains => "JetBrains Mono Nerd Font",
            FontChoice::FiraCode => "Fira Code Nerd Font",
            FontChoice::Hack => "Hack Nerd Font",
            FontChoice::System if cfg!(target_os = "macos") => "System (SF / Menlo)",
            FontChoice::System => "System (DejaVu)",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            FontChoice::JetBrains => "jetbrains",
            FontChoice::FiraCode => "firacode",
            FontChoice::Hack => "hack",
            FontChoice::System => "system",
        }
    }

    fn from_id(id: Option<&str>, default: Self) -> Self {
        Self::ALL
            .into_iter()
            .find(|choice| Some(choice.id()) == id)
            .unwrap_or(default)
    }

    fn stem(self) -> Option<&'static str> {
        match self {
            FontChoice::JetBrains => Some("JetBrainsMonoNerdFont"),
            FontChoice::FiraCode => Some("FiraCodeNerdFont"),
            FontChoice::Hack => Some("HackNerdFont"),
            FontChoice::System => None,
        }
    }

    /// Candidate files: `variant` is "Propo" (UI) or "Mono" (terminal).
    pub fn files(self, home: &Path, variant: &str, weights: &[&str]) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Some(stem) = self.stem() {
            for dir in [
                home.join("Library/Fonts"),
                PathBuf::from("/Library/Fonts"),
                home.join(".local/share/fonts"),
                PathBuf::from("/usr/share/fonts/truetype/nerd-fonts"),
            ] {
                for weight in weights {
                    out.push(dir.join(format!("{stem}{variant}-{weight}.ttf")));
                }
            }
        }
        out
    }
}

/// Conversation list on the empty-pane start screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartSessions {
    /// Hidden while the sidebar's Sessions tab already lists them.
    Auto,
    Always,
    Never,
}

impl StartSessions {
    pub const ALL: [StartSessions; 3] = [StartSessions::Auto, StartSessions::Always, StartSessions::Never];

    pub fn label(self) -> &'static str {
        match self {
            StartSessions::Auto => "Auto",
            StartSessions::Always => "Always",
            StartSessions::Never => "Never",
        }
    }

    fn from_id(id: Option<&str>) -> Self {
        match id {
            Some("always") => StartSessions::Always,
            Some("never") => StartSessions::Never,
            _ => StartSessions::Auto,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpaceLayout {
    pub stacked: bool,
    pub split_x: f32,
    pub split_y: f32,
}

/// Persisted preferences (`$XDG_CONFIG_HOME/ais-shell/settings.json`).
#[derive(Clone, Debug)]
pub struct Settings {
    pub schema_version: u32,
    pub theme: Theme,
    pub ui_theme: UiTheme,
    pub ui_scale: f32,
    pub show_sidebar: bool,
    pub show_files: bool,
    pub terminal_palette: TerminalPalette,
    pub font_size: f32,
    pub show_icons: bool,
    pub start_sessions: StartSessions,
    /// Sidebar on the right, Files/Usage/Alerts dock on the left.
    pub swap_sides: bool,
    pub show_agents: bool,
    pub show_status_bar: bool,
    pub option_as_meta: bool,
    pub copy_on_select: bool,
    pub cursor_blink: bool,
    pub system_notifications: bool,
    pub auto_live_usage: bool,
    pub allow_osc52: bool,
    pub reduce_motion: bool,
    pub cli_args: BTreeMap<String, String>,
    pub cli_commands: BTreeMap<String, String>,
    pub open_spaces: Vec<PathBuf>,
    pub active_space: usize,
    pub space_layouts: BTreeMap<String, SpaceLayout>,
    pub window_size: Option<[f32; 2]>,
    pub window_pos: Option<[f32; 2]>,
    pub sidebar_sessions: bool,
    pub right_usage: bool,
    pub ui_font: FontChoice,
    pub term_font: FontChoice,
    pub keymap: crate::keys::Keymap,
    /// Wide search field centered in the title bar; space tabs get their own row.
    pub search_center: bool,
    /// macOS: keep the system title bar instead of ais' own (needs restart).
    pub native_titlebar: bool,
    /// ⌘⌥ arrows move between panes (↑↓) and spaces (←→).
    pub nav_arrows: bool,
    /// Vim keys: ⌃J/⌃K (⌃N/⌃P) in lists, ⌘⌥H/J/K/L between spaces and panes.
    pub nav_vim: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            theme: Theme::System,
            ui_theme: UiTheme::Ais,
            ui_scale: 1.0,
            show_sidebar: true,
            show_files: false,
            terminal_palette: TerminalPalette::Auto,
            font_size: DEFAULT_FONT_SIZE,
            show_icons: true,
            start_sessions: StartSessions::Auto,
            swap_sides: false,
            show_agents: true,
            show_status_bar: true,
            option_as_meta: false,
            copy_on_select: false,
            cursor_blink: false,
            system_notifications: true,
            auto_live_usage: false,
            allow_osc52: true,
            reduce_motion: false,
            cli_args: BTreeMap::new(),
            cli_commands: BTreeMap::new(),
            open_spaces: Vec::new(),
            active_space: 0,
            space_layouts: BTreeMap::new(),
            window_size: None,
            window_pos: None,
            sidebar_sessions: false,
            right_usage: false,
            ui_font: FontChoice::JetBrains,
            term_font: FontChoice::JetBrains,
            keymap: crate::keys::Keymap::default(),
            search_center: true,
            native_titlebar: false,
            nav_arrows: true,
            nav_vim: false,
        }
    }
}

pub fn settings_path(home: &Path) -> PathBuf {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".config"))
        .join("ais-shell/settings.json")
}

impl Settings {
    pub fn load_checked(path: &Path) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let Ok(bytes) = fs::read(path) else {
            return (Self::default(), warnings);
        };
        let value = match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) if v.is_object() => v,
            Ok(_) | Err(_) => {
                let backup = path.with_extension("json.bak");
                let _ = fs::copy(path, &backup);
                warnings.push(format!(
                    "Settings file was corrupted; backed up to {}",
                    backup.display()
                ));
                return (Self::default(), warnings);
            }
        };
        let (settings, key_warnings) = Self::from_value_checked(&value);
        warnings.extend(key_warnings);
        (settings, warnings)
    }

    #[cfg(test)]
    pub fn from_value(value: &Value) -> Self {
        Self::from_value_checked(value).0
    }

    pub fn from_value_checked(value: &Value) -> (Self, Vec<String>) {
        let d = Self::default();
        let boolean = |key: &str, old: &str, default: bool| {
            value
                .get(key)
                .or_else(|| value.get(old))
                .and_then(Value::as_bool)
                .unwrap_or(default)
        };
        let number = |key: &str, old: &str, default: f32| {
            value
                .get(key)
                .or_else(|| value.get(old))
                .and_then(Value::as_f64)
                .map(|n| n as f32)
                .unwrap_or(default)
        };
        let pair_f32 = |key: &str| -> Option<[f32; 2]> {
            let arr = value.get(key)?.as_array()?;
            if arr.len() != 2 {
                return None;
            }
            let a = arr[0].as_f64()? as f32;
            let b = arr[1].as_f64()? as f32;
            (a.is_finite() && b.is_finite()).then_some([a, b])
        };
        let palette_id = value.get("terminal_palette").and_then(Value::as_str).unwrap_or("auto");
        let raw_keys: BTreeMap<String, String> = value
            .get("keys")
            .and_then(Value::as_object)
            .map(|keys| {
                keys.iter()
                    .filter_map(|(id, binding)| Some((id.clone(), binding.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        let (keymap, key_warnings) = crate::keys::Keymap::from_overrides(raw_keys);

        let mut open_spaces: Vec<PathBuf> = Vec::new();
        if let Some(items) = value.get("open_spaces").and_then(Value::as_array) {
            for item in items.iter().filter_map(Value::as_str) {
                let raw = PathBuf::from(item);
                let path = if raw.exists() { canonical_path(&raw) } else { raw };
                if !open_spaces.contains(&path) {
                    open_spaces.push(path);
                }
            }
        }

        let space_layouts = value
            .get("space_layouts")
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .map(|(k, v)| {
                        let stacked = v.get("stacked").and_then(Value::as_bool).unwrap_or(false);
                        let split_x = v.get("split_x").and_then(Value::as_f64).unwrap_or(0.5) as f32;
                        let split_y = v.get("split_y").and_then(Value::as_f64).unwrap_or(0.5) as f32;
                        (
                            k.clone(),
                            SpaceLayout {
                                stacked,
                                split_x: split_x.clamp(0.15, 0.85),
                                split_y: split_y.clamp(0.15, 0.85),
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();

        let settings = Self {
            schema_version: value
                .get("schema_version")
                .and_then(Value::as_u64)
                .map(|v| v as u32)
                .unwrap_or(SCHEMA_VERSION),
            theme: match value.get("theme").and_then(Value::as_str) {
                Some("light") => Theme::Light,
                Some("dark") => Theme::Dark,
                _ => Theme::System,
            },
            ui_theme: UiTheme::from_id(value.get("ui_theme").and_then(Value::as_str)),
            ui_scale: number("ui_scale", "", d.ui_scale).clamp(0.7, 1.6),
            show_sidebar: boolean("show_sidebar", "show_projects", d.show_sidebar),
            show_files: boolean("show_files", "", d.show_files),
            terminal_palette: TerminalPalette::ALL
                .into_iter()
                .find(|palette| palette.id() == palette_id)
                .unwrap_or(TerminalPalette::Auto),
            font_size: number("font_size", "terminal_font_size", d.font_size).clamp(10.0, 28.0),
            show_icons: boolean("show_icons", "", d.show_icons),
            start_sessions: StartSessions::from_id(value.get("start_sessions").and_then(Value::as_str)),
            swap_sides: boolean("swap_sides", "", d.swap_sides),
            show_agents: boolean("show_agents", "", d.show_agents),
            show_status_bar: boolean("show_status_bar", "", d.show_status_bar),
            option_as_meta: boolean("option_as_meta", "", d.option_as_meta),
            copy_on_select: boolean("copy_on_select", "", d.copy_on_select),
            cursor_blink: boolean("cursor_blink", "", d.cursor_blink),
            system_notifications: boolean("system_notifications", "", d.system_notifications),
            auto_live_usage: boolean("auto_live_usage", "", d.auto_live_usage),
            allow_osc52: boolean("allow_osc52", "", d.allow_osc52),
            reduce_motion: boolean("reduce_motion", "", d.reduce_motion),
            cli_args: value
                .get("cli_args")
                .and_then(Value::as_object)
                .map(|map| {
                    map.iter()
                        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                        .collect()
                })
                .unwrap_or_default(),
            cli_commands: value
                .get("cli_commands")
                .and_then(Value::as_object)
                .map(|map| {
                    map.iter()
                        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                        .collect()
                })
                .unwrap_or_default(),
            open_spaces,
            active_space: value.get("active_space").and_then(Value::as_u64).unwrap_or(0) as usize,
            space_layouts,
            window_size: pair_f32("window_size"),
            window_pos: pair_f32("window_pos"),
            sidebar_sessions: boolean("sidebar_sessions", "", d.sidebar_sessions),
            right_usage: boolean("right_usage", "", d.right_usage),
            ui_font: FontChoice::from_id(value.get("ui_font").and_then(Value::as_str), d.ui_font),
            term_font: FontChoice::from_id(value.get("term_font").and_then(Value::as_str), d.term_font),
            keymap,
            search_center: boolean("search_center", "", d.search_center),
            native_titlebar: boolean("native_titlebar", "", d.native_titlebar),
            nav_arrows: boolean("nav_arrows", "", d.nav_arrows),
            nav_vim: boolean("nav_vim", "", d.nav_vim),
        };
        (settings, key_warnings)
    }

    pub fn to_value(&self) -> Value {
        let layouts: BTreeMap<&String, Value> = self
            .space_layouts
            .iter()
            .map(|(k, v)| {
                (
                    k,
                    json!({
                        "stacked": v.stacked,
                        "split_x": v.split_x,
                        "split_y": v.split_y,
                    }),
                )
            })
            .collect();
        json!({
            "schema_version": self.schema_version,
            "theme": self.theme.label().to_ascii_lowercase(),
            "ui_theme": self.ui_theme.id(),
            "ui_scale": self.ui_scale,
            "show_sidebar": self.show_sidebar,
            "show_files": self.show_files,
            "terminal_palette": self.terminal_palette.id(),
            "font_size": self.font_size,
            "show_icons": self.show_icons,
            "start_sessions": self.start_sessions.label().to_ascii_lowercase(),
            "swap_sides": self.swap_sides,
            "show_agents": self.show_agents,
            "show_status_bar": self.show_status_bar,
            "option_as_meta": self.option_as_meta,
            "copy_on_select": self.copy_on_select,
            "cursor_blink": self.cursor_blink,
            "system_notifications": self.system_notifications,
            "auto_live_usage": self.auto_live_usage,
            "allow_osc52": self.allow_osc52,
            "reduce_motion": self.reduce_motion,
            "cli_args": self.cli_args,
            "cli_commands": self.cli_commands,
            "open_spaces": self.open_spaces,
            "active_space": self.active_space,
            "space_layouts": layouts,
            "window_size": self.window_size,
            "window_pos": self.window_pos,
            "sidebar_sessions": self.sidebar_sessions,
            "right_usage": self.right_usage,
            "ui_font": self.ui_font.id(),
            "term_font": self.term_font.id(),
            "keys": self.keymap.overrides,
            "search_center": self.search_center,
            "native_titlebar": self.native_titlebar,
            "nav_arrows": self.nav_arrows,
            "nav_vim": self.nav_vim,
        })
    }

    /// Writes atomically using a PID-scoped temp file so concurrent exits never collide.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let Some(parent) = path.parent() else {
            return Err("Settings path has no parent directory".into());
        };
        fs::create_dir_all(parent).map_err(|e| format!("Cannot create settings folder: {e}"))?;
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        let bytes = serde_json::to_vec_pretty(&self.to_value()).map_err(|e| format!("Cannot encode settings: {e}"))?;
        fs::write(&tmp, bytes).map_err(|e| format!("Cannot write settings: {e}"))?;
        fs::rename(&tmp, path).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            format!("Cannot save settings: {e}")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_themes_round_trip_and_meet_wcag_contrast() {
        for theme in UiTheme::ALL {
            assert_eq!(UiTheme::from_id(Some(theme.id())), theme);
            for light in [false, true] {
                let palette = theme.palette(light);
                assert_eq!(palette.light, light);
                let text_ratio = contrast_ratio(palette.text, palette.panel);
                let muted_ratio = contrast_ratio(palette.muted, palette.panel);
                let faint_ratio = contrast_ratio(palette.faint, palette.panel);
                let accent_ratio = contrast_ratio(palette.accent_text, palette.accent);
                assert!(
                    text_ratio >= 4.5,
                    "{} ({light}) text ratio {text_ratio:.2}",
                    theme.label()
                );
                assert!(
                    muted_ratio >= 4.5,
                    "{} ({light}) muted ratio {muted_ratio:.2}",
                    theme.label()
                );
                assert!(
                    faint_ratio >= 3.0,
                    "{} ({light}) faint ratio {faint_ratio:.2}",
                    theme.label()
                );
                assert!(
                    accent_ratio >= 4.5,
                    "{} ({light}) accent_text ratio {accent_ratio:.2}",
                    theme.label()
                );
            }
        }
        assert_eq!(UiTheme::from_id(Some("dracula")), UiTheme::Nocturne);
        assert_eq!(UiTheme::from_id(None), UiTheme::Ais);
        let mono = UiTheme::Mono.palette(false);
        for color in mono.term.ansi.iter().chain([&mono.accent, &mono.green, &mono.red]) {
            assert!(color.r() == color.g() && color.g() == color.b());
        }
    }

    #[test]
    fn settings_round_trip_and_legacy_keys() {
        let legacy =
            json!({"theme": "light", "show_projects": false, "terminal_font_size": 17.0, "terminal_palette": "nord"});
        let settings = Settings::from_value(&legacy);
        assert_eq!(settings.theme, Theme::Light);
        assert!(!settings.show_sidebar);
        assert_eq!(settings.font_size, 17.0);
        assert_eq!(settings.terminal_palette, TerminalPalette::Nord);
        assert!(!settings.auto_live_usage);
        let again = Settings::from_value(&settings.to_value());
        assert_eq!(again.font_size, 17.0);
        assert!(!again.show_sidebar);
    }

    #[test]
    fn corrupt_settings_creates_backup_and_warns() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("ais-shell-cfg-{}-{stamp}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        fs::write(&path, "{broken json").unwrap();
        let (settings, warnings) = Settings::load_checked(&path);
        assert_eq!(settings.theme, Theme::System);
        assert!(!warnings.is_empty());
        assert!(dir.join("settings.json.bak").is_file());
        let _ = fs::remove_dir_all(dir);
    }
}
