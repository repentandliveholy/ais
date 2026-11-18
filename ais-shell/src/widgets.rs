//! Shared UI widgets, painted vector icons, theme helpers and platform actions.

use crate::cli::CliKind;
#[cfg(target_os = "windows")]
use crate::cli::no_window;
use crate::log;
use crate::sessions::Session;
use crate::theme::{Palette, Theme, desaturate, scale};
use crate::usage::Limit;
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Id, Layout, Margin, Pos2, Rect, RichText, Sense, Stroke, Vec2,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiIcon {
    Sidebar,
    Settings,
    Bell,
    Refresh,
    Expand,
    Compress,
    SplitRight,
    SplitDown,
    History,
    Files,
    Terminal,
    Plus,
    Up,
    Search,
    Gauge,
    Folder,
    FolderOpen,
    Layout,
    Fullscreen,
    Agents,
}

impl UiIcon {
    pub fn fallback(self) -> &'static str {
        match self {
            UiIcon::Sidebar => "Side",
            UiIcon::Settings => "Set",
            UiIcon::Bell => "Alerts",
            UiIcon::Refresh => "↻",
            UiIcon::Expand => "⤢",
            UiIcon::Compress => "⤡",
            UiIcon::SplitRight => "Split →",
            UiIcon::SplitDown => "Split ↓",
            UiIcon::History => "Hist",
            UiIcon::Files => "Files",
            UiIcon::Terminal => ">_",
            UiIcon::Plus => "+",
            UiIcon::Up => "..",
            UiIcon::Search => "Search",
            UiIcon::Gauge => "Usage",
            UiIcon::Folder => "[dir]",
            UiIcon::FolderOpen => "[open]",
            UiIcon::Layout => "Blocks",
            UiIcon::Fullscreen => "Full",
            UiIcon::Agents => "Pills",
        }
    }

    pub fn paint(self, painter: &egui::Painter, rect: Rect, color: Color32) {
        let c = rect.center();
        let r = (rect.width().min(rect.height()) * 0.36).max(4.5);
        let stroke = Stroke::new(1.5, color);
        match self {
            UiIcon::Sidebar => {
                let box_rect = Rect::from_center_size(c, Vec2::new(r * 2.1, r * 1.7));
                painter.rect_stroke(box_rect, CornerRadius::same(2), stroke, egui::StrokeKind::Inside);
                let split_x = box_rect.left() + box_rect.width() * 0.36;
                painter.line_segment(
                    [
                        Pos2::new(split_x, box_rect.top()),
                        Pos2::new(split_x, box_rect.bottom()),
                    ],
                    stroke,
                );
            }
            UiIcon::Settings => {
                painter.circle_stroke(c, r * 0.52, stroke);
                for step in 0..8 {
                    let angle = step as f32 * std::f32::consts::TAU / 8.0;
                    let dir = Vec2::angled(angle);
                    painter.line_segment([c + dir * (r * 0.68), c + dir * (r * 1.05)], stroke);
                }
            }
            UiIcon::Bell => {
                let top = c + Vec2::new(0.0, -r * 0.85);
                let base_y = c.y + r * 0.55;
                let w = r * 0.85;
                painter.line_segment(
                    [
                        top + Vec2::new(-w * 0.55, r * 0.35),
                        top + Vec2::new(w * 0.55, r * 0.35),
                    ],
                    stroke,
                );
                painter.line_segment(
                    [top + Vec2::new(-w * 0.55, r * 0.35), Pos2::new(c.x - w, base_y)],
                    stroke,
                );
                painter.line_segment(
                    [top + Vec2::new(w * 0.55, r * 0.35), Pos2::new(c.x + w, base_y)],
                    stroke,
                );
                painter.line_segment(
                    [Pos2::new(c.x - w * 1.15, base_y), Pos2::new(c.x + w * 1.15, base_y)],
                    stroke,
                );
                painter.circle_filled(Pos2::new(c.x, base_y + r * 0.35), 1.4, color);
            }
            UiIcon::Refresh => {
                let pts: Vec<Pos2> = (0..=14)
                    .map(|i| {
                        let angle = -0.3 + (i as f32 / 14.0) * std::f32::consts::PI * 1.55;
                        c + Vec2::angled(angle) * (r * 0.85)
                    })
                    .collect();
                painter.add(egui::Shape::line(pts, stroke));
                let tip = c + Vec2::angled(-0.3) * (r * 0.85);
                painter.line_segment([tip, tip + Vec2::new(-3.2, -1.8)], stroke);
                painter.line_segment([tip, tip + Vec2::new(0.8, -3.5)], stroke);
            }
            UiIcon::Expand => {
                let d = r * 0.85;
                painter.line_segment([c + Vec2::new(-d, d), c + Vec2::new(d, -d)], stroke);
                painter.line_segment([c + Vec2::new(d, -d), c + Vec2::new(d - 4.0, -d)], stroke);
                painter.line_segment([c + Vec2::new(d, -d), c + Vec2::new(d, -d + 4.0)], stroke);
                painter.line_segment([c + Vec2::new(-d, d), c + Vec2::new(-d + 4.0, d)], stroke);
                painter.line_segment([c + Vec2::new(-d, d), c + Vec2::new(-d, d - 4.0)], stroke);
            }
            UiIcon::Compress => {
                let d = r * 0.85;
                painter.line_segment([c + Vec2::new(-d, d), c + Vec2::new(-1.5, 1.5)], stroke);
                painter.line_segment([c + Vec2::new(d, -d), c + Vec2::new(1.5, -1.5)], stroke);
                painter.line_segment([c + Vec2::new(-1.5, 1.5), c + Vec2::new(-5.0, 1.5)], stroke);
                painter.line_segment([c + Vec2::new(-1.5, 1.5), c + Vec2::new(-1.5, 5.0)], stroke);
                painter.line_segment([c + Vec2::new(1.5, -1.5), c + Vec2::new(5.0, -1.5)], stroke);
                painter.line_segment([c + Vec2::new(1.5, -1.5), c + Vec2::new(1.5, -5.0)], stroke);
            }
            UiIcon::SplitRight => {
                let box_rect = Rect::from_center_size(c, Vec2::new(r * 2.1, r * 1.7));
                painter.rect_stroke(box_rect, CornerRadius::same(2), stroke, egui::StrokeKind::Inside);
                painter.line_segment(
                    [Pos2::new(c.x, box_rect.top()), Pos2::new(c.x, box_rect.bottom())],
                    stroke,
                );
            }
            UiIcon::SplitDown => {
                let box_rect = Rect::from_center_size(c, Vec2::new(r * 2.1, r * 1.7));
                painter.rect_stroke(box_rect, CornerRadius::same(2), stroke, egui::StrokeKind::Inside);
                painter.line_segment(
                    [Pos2::new(box_rect.left(), c.y), Pos2::new(box_rect.right(), c.y)],
                    stroke,
                );
            }
            UiIcon::History => {
                painter.circle_stroke(c, r * 0.85, stroke);
                painter.line_segment([c, c + Vec2::new(0.0, -r * 0.45)], stroke);
                painter.line_segment([c, c + Vec2::new(r * 0.4, r * 0.2)], stroke);
            }
            UiIcon::Files => {
                let box_rect = Rect::from_center_size(c, Vec2::new(r * 1.55, r * 1.9));
                painter.rect_stroke(box_rect, CornerRadius::same(2), stroke, egui::StrokeKind::Inside);
                let y1 = c.y - r * 0.3;
                let y2 = c.y + r * 0.25;
                painter.line_segment([Pos2::new(c.x - r * 0.4, y1), Pos2::new(c.x + r * 0.4, y1)], stroke);
                painter.line_segment([Pos2::new(c.x - r * 0.4, y2), Pos2::new(c.x + r * 0.4, y2)], stroke);
            }
            UiIcon::Terminal => {
                let box_rect = Rect::from_center_size(c, Vec2::new(r * 2.1, r * 1.7));
                painter.rect_stroke(box_rect, CornerRadius::same(2), stroke, egui::StrokeKind::Inside);
                painter.line_segment(
                    [c + Vec2::new(-r * 0.55, -r * 0.35), c + Vec2::new(-r * 0.1, 0.0)],
                    stroke,
                );
                painter.line_segment(
                    [c + Vec2::new(-r * 0.55, r * 0.35), c + Vec2::new(-r * 0.1, 0.0)],
                    stroke,
                );
                painter.line_segment(
                    [c + Vec2::new(r * 0.05, r * 0.35), c + Vec2::new(r * 0.55, r * 0.35)],
                    stroke,
                );
            }
            UiIcon::Plus => {
                let d = r * 0.78;
                painter.line_segment([c + Vec2::new(-d, 0.0), c + Vec2::new(d, 0.0)], stroke);
                painter.line_segment([c + Vec2::new(0.0, -d), c + Vec2::new(0.0, d)], stroke);
            }
            UiIcon::Up => {
                let d = r * 0.75;
                painter.line_segment([c + Vec2::new(0.0, d), c + Vec2::new(0.0, -d)], stroke);
                painter.line_segment([c + Vec2::new(0.0, -d), c + Vec2::new(-d * 0.65, -d * 0.15)], stroke);
                painter.line_segment([c + Vec2::new(0.0, -d), c + Vec2::new(d * 0.65, -d * 0.15)], stroke);
            }
            UiIcon::Search => {
                let lens = c + Vec2::new(-r * 0.18, -r * 0.18);
                painter.circle_stroke(lens, r * 0.62, stroke);
                painter.line_segment(
                    [lens + Vec2::new(r * 0.44, r * 0.44), c + Vec2::new(r * 0.85, r * 0.85)],
                    stroke,
                );
            }
            UiIcon::Gauge => {
                let pts: Vec<Pos2> = (0..=12)
                    .map(|i| {
                        let angle = std::f32::consts::PI + (i as f32 / 12.0) * std::f32::consts::PI;
                        c + Vec2::new(0.0, r * 0.25) + Vec2::angled(angle) * (r * 0.9)
                    })
                    .collect();
                painter.add(egui::Shape::line(pts, stroke));
                painter.line_segment(
                    [c + Vec2::new(0.0, r * 0.25), c + Vec2::new(r * 0.55, -r * 0.45)],
                    stroke,
                );
            }
            UiIcon::Folder | UiIcon::FolderOpen => {
                let box_rect = Rect::from_center_size(c + Vec2::new(0.0, r * 0.1), Vec2::new(r * 2.0, r * 1.4));
                painter.rect_stroke(box_rect, CornerRadius::same(2), stroke, egui::StrokeKind::Inside);
                let tab = Rect::from_min_size(box_rect.min - Vec2::new(0.0, r * 0.32), Vec2::new(r * 0.85, r * 0.35));
                painter.rect_stroke(tab, CornerRadius::same(1), stroke, egui::StrokeKind::Inside);
            }
            UiIcon::Layout => {
                let box_rect = Rect::from_center_size(c, Vec2::new(r * 2.1, r * 1.7));
                painter.rect_stroke(box_rect, CornerRadius::same(2), stroke, egui::StrokeKind::Inside);
                let split_x = box_rect.left() + box_rect.width() * 0.38;
                let split_y = box_rect.top() + box_rect.height() * 0.36;
                painter.line_segment(
                    [
                        Pos2::new(box_rect.left(), split_y),
                        Pos2::new(box_rect.right(), split_y),
                    ],
                    stroke,
                );
                painter.line_segment(
                    [Pos2::new(split_x, split_y), Pos2::new(split_x, box_rect.bottom())],
                    stroke,
                );
            }
            UiIcon::Fullscreen => {
                let d = r * 0.88;
                let arm = r * 0.45;
                for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                    let corner = c + Vec2::new(d * sx, d * sy);
                    painter.line_segment([corner, corner + Vec2::new(-arm * sx, 0.0)], stroke);
                    painter.line_segment([corner, corner + Vec2::new(0.0, -arm * sy)], stroke);
                }
            }
            UiIcon::Agents => {
                let pill_w = r * 1.9;
                let pill_h = r * 0.68;
                let top = Rect::from_center_size(c + Vec2::new(0.0, -r * 0.48), Vec2::new(pill_w, pill_h));
                let bot = Rect::from_center_size(c + Vec2::new(0.0, r * 0.48), Vec2::new(pill_w, pill_h));
                painter.rect_stroke(top, CornerRadius::same(3), stroke, egui::StrokeKind::Inside);
                painter.rect_stroke(bot, CornerRadius::same(3), stroke, egui::StrokeKind::Inside);
            }
        }
    }
}

/// Icon button using painted vector geometry when icons are enabled, or a clean
/// readable text fallback when `show_icons` is disabled — never an empty button.
pub fn tool_button(
    ui: &mut egui::Ui,
    icon: UiIcon,
    label: &str,
    selected: bool,
    show_icons: bool,
    palette: &Palette,
) -> egui::Response {
    tool_button_tinted(ui, icon, label, selected, show_icons, palette, None)
}

pub fn tool_button_tinted(
    ui: &mut egui::Ui,
    icon: UiIcon,
    label: &str,
    selected: bool,
    show_icons: bool,
    palette: &Palette,
    custom_tint: Option<Color32>,
) -> egui::Response {
    let fg = custom_tint.unwrap_or(if selected { palette.accent } else { palette.muted });
    let fill = if custom_tint.is_some() && !selected {
        fg.gamma_multiply(0.14)
    } else if selected {
        palette.accent.gamma_multiply(0.14)
    } else {
        Color32::TRANSPARENT
    };
    let text = if show_icons {
        label.to_string()
    } else if label.is_empty() {
        icon.fallback().to_string()
    } else {
        label.to_string()
    };
    let font = FontId::proportional(scale::FONT_MD);
    let galley = if text.is_empty() {
        None
    } else {
        Some(ui.fonts(|fonts| fonts.layout_no_wrap(text, font, fg)))
    };
    let icon_w = if show_icons { 16.0 } else { 0.0 };
    let gap = if show_icons && galley.is_some() { 5.0 } else { 0.0 };
    let text_w = galley.as_ref().map(|g| g.size().x).unwrap_or(0.0);
    let width = (icon_w + gap + text_w + 12.0).max(28.0);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 26.0), Sense::click());
    let hovered = response.hovered();
    let plate = if hovered { palette.hover } else { fill };
    if plate != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, CornerRadius::same(7), plate);
    }
    let ink = if hovered && custom_tint.is_none() && !selected {
        palette.text
    } else {
        fg
    };
    let content_w = icon_w + gap + text_w;
    let start_x = rect.center().x - content_w * 0.5;
    if show_icons {
        let icon_rect = Rect::from_center_size(Pos2::new(start_x + icon_w * 0.5, rect.center().y), Vec2::splat(16.0));
        icon.paint(ui.painter(), icon_rect, ink);
    }
    if let Some(galley) = galley {
        let pos = Pos2::new(start_x + icon_w + gap, rect.center().y - galley.size().y * 0.5);
        ui.painter().galley(pos, galley, ink);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub fn tiny_icon_button(ui: &mut egui::Ui, icon: UiIcon, show_icons: bool, palette: &Palette) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(5), palette.hover);
    }
    let color = if response.hovered() {
        palette.text
    } else {
        palette.muted
    };
    if show_icons {
        let icon_rect = Rect::from_center_size(rect.center(), Vec2::splat(14.0));
        icon.paint(ui.painter(), icon_rect, color);
    } else {
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            icon.fallback(),
            FontId::proportional(scale::FONT_SM),
            color,
        );
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Close / minimize / full-screen buttons for the custom macOS title bar.
pub fn window_buttons(ui: &mut egui::Ui, palette: &Palette, focused: bool) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(56.0, 22.0), Sense::hover());
    let group_hovered = ui.rect_contains_pointer(rect);
    let colors = [c32(255, 95, 87), c32(254, 188, 46), c32(40, 200, 64)];
    for (index, base) in colors.into_iter().enumerate() {
        let center = Pos2::new(rect.left() + 8.0 + index as f32 * 20.0, rect.center().y);
        let hit = Rect::from_center_size(center, Vec2::splat(18.0));
        let response = ui.interact(hit, Id::new(("window-button", index)), Sense::click());
        let mut color = if focused || group_hovered {
            base
        } else {
            palette.border_strong
        };
        if palette.saturation < 1.0 && (focused || group_hovered) {
            color = desaturate(color, palette.saturation);
        }
        if response.is_pointer_button_down_on() {
            color = color.gamma_multiply(0.8);
        }
        let painter = ui.painter();
        painter.circle_filled(center, 6.0, color);
        painter.circle_stroke(center, 6.0, Stroke::new(0.5, Color32::from_black_alpha(40)));
        if group_hovered {
            let ink = Stroke::new(1.2, Color32::from_black_alpha(150));
            let d = 2.6;
            match index {
                0 => {
                    painter.line_segment([center + Vec2::new(-d, -d), center + Vec2::new(d, d)], ink);
                    painter.line_segment([center + Vec2::new(-d, d), center + Vec2::new(d, -d)], ink);
                }
                1 => {
                    painter.line_segment([center + Vec2::new(-3.2, 0.0), center + Vec2::new(3.2, 0.0)], ink);
                }
                _ => {
                    let fill = Color32::from_black_alpha(150);
                    painter.add(egui::Shape::convex_polygon(
                        vec![
                            center + Vec2::new(-3.0, -3.0),
                            center + Vec2::new(1.5, -3.0),
                            center + Vec2::new(-3.0, 1.5),
                        ],
                        fill,
                        Stroke::NONE,
                    ));
                    painter.add(egui::Shape::convex_polygon(
                        vec![
                            center + Vec2::new(3.0, 3.0),
                            center + Vec2::new(-1.5, 3.0),
                            center + Vec2::new(3.0, -1.5),
                        ],
                        fill,
                        Stroke::NONE,
                    ));
                }
            };
        }
        if response.clicked() {
            let ctx = ui.ctx();
            match index {
                0 => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                1 => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
                _ => {
                    let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
                }
            }
        }
    }
}

/// Mode button with a painted glyph (font glyphs sit off-center):
/// sun = light, crescent = dark, half disc = follow the system.
pub fn theme_button(ui: &mut egui::Ui, palette: &Palette, mode: Theme) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(28.0, 26.0), Sense::click());
    let painter = ui.painter();
    let hovered = response.hovered();
    let plate = if hovered { palette.hover } else { palette.chrome };
    if hovered {
        painter.rect_filled(rect, CornerRadius::same(7), plate);
    }
    let ink = if hovered { palette.text } else { palette.muted };
    let c = rect.center();
    match mode {
        Theme::Light => {
            painter.circle_filled(c, 3.4, ink);
            for step in 0..8 {
                let angle = step as f32 * std::f32::consts::TAU / 8.0;
                let dir = Vec2::angled(angle);
                painter.line_segment([c + dir * 5.4, c + dir * 7.2], Stroke::new(1.4, ink));
            }
        }
        Theme::Dark => {
            painter.circle_filled(c, 6.0, ink);
            painter.circle_filled(c + Vec2::new(3.0, -2.4), 5.0, plate);
        }
        Theme::System => {
            painter.circle_stroke(c, 6.0, Stroke::new(1.4, ink));
            let half: Vec<Pos2> = (0..=16)
                .map(|step| {
                    let angle = -std::f32::consts::FRAC_PI_2 + step as f32 * std::f32::consts::PI / 16.0;
                    c + Vec2::angled(angle) * 6.0
                })
                .collect();
            painter.add(egui::Shape::convex_polygon(half, ink, Stroke::NONE));
        }
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

const fn c32(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

/// Small × drawn with strokes with a 24×22 hit target.
pub fn close_button(ui: &mut egui::Ui, palette: &Palette) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
    let hovered = response.hovered();
    if hovered {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(5), palette.red.gamma_multiply(0.18));
    }
    let color = if hovered { palette.red } else { palette.faint };
    let c = rect.center();
    let d = 3.5;
    let stroke = Stroke::new(1.4, color);
    ui.painter()
        .line_segment([c + Vec2::new(-d, -d), c + Vec2::new(d, d)], stroke);
    ui.painter()
        .line_segment([c + Vec2::new(-d, d), c + Vec2::new(d, -d)], stroke);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Tabs drawn as separate plates: every tab has a visible frame, the
/// selected one is tinted with the accent. Returns the newly clicked index.
pub fn tabs(ui: &mut egui::Ui, labels: &[&str], selected: usize, palette: &Palette) -> Option<usize> {
    let mut clicked = None;
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.spacing_mut().button_padding = Vec2::new(10.0, 3.0);
        for (index, label) in labels.iter().enumerate() {
            let active = index == selected;
            let button = egui::Button::new(RichText::new(*label).size(scale::FONT_SM + 0.5).color(if active {
                palette.text
            } else {
                palette.muted
            }))
            .fill(if active {
                palette.accent.gamma_multiply(0.16)
            } else {
                palette.raised
            })
            .stroke(Stroke::new(
                1.0,
                if active {
                    palette.accent.gamma_multiply(0.55)
                } else {
                    palette.border
                },
            ))
            .corner_radius(CornerRadius::same(6))
            .min_size(Vec2::new(0.0, 24.0));
            let response = ui.add(button).on_hover_cursor(egui::CursorIcon::PointingHand);
            if response.clicked() && !active {
                clicked = Some(index);
            }
        }
    });
    clicked
}

pub fn small_button<'a>(text: &str, palette: &Palette) -> egui::Button<'a> {
    egui::Button::new(RichText::new(text).size(12.0).color(palette.text))
        .fill(palette.hover)
        .stroke(Stroke::new(1.0, palette.border))
        .corner_radius(CornerRadius::same(6))
        .min_size(Vec2::new(0.0, 24.0))
}

pub fn primary_button<'a>(text: &str, palette: &Palette) -> egui::Button<'a> {
    egui::Button::new(
        RichText::new(text)
            .size(scale::FONT_LG)
            .strong()
            .color(palette.accent_text),
    )
    .fill(palette.accent)
    .corner_radius(CornerRadius::same(9))
    .min_size(Vec2::new(200.0, 34.0))
}

pub fn pill(text: &str, color: Color32) -> RichText {
    RichText::new(format!(" {text} "))
        .size(scale::FONT_XS)
        .color(color)
        .background_color(color.gamma_multiply(0.15))
}

pub const SETTINGS_WIDTH: f32 = 490.0;
pub const SETTINGS_LABEL: f32 = 148.0;
pub const SETTINGS_CONTROL: f32 = 236.0;

pub fn setting_row<R>(
    ui: &mut egui::Ui,
    label: impl Into<egui::WidgetText>,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.horizontal(|ui| {
        let height = ui.spacing().interact_size.y;
        ui.allocate_ui_with_layout(
            Vec2::new(SETTINGS_LABEL, height),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.set_min_size(Vec2::new(SETTINGS_LABEL, height));
                ui.add(egui::Label::new(label).truncate());
            },
        );
        ui.with_layout(Layout::left_to_right(Align::Min), add).inner
    })
    .inner
}

pub fn setting_note(ui: &mut egui::Ui, text: &str, palette: &Palette) {
    ui.horizontal(|ui| {
        ui.add_space(SETTINGS_LABEL + ui.spacing().item_spacing.x);
        ui.add(egui::Label::new(RichText::new(text).size(11.0).color(palette.faint)).wrap());
    });
}

pub fn section(ui: &mut egui::Ui, title: &str, palette: &Palette) {
    ui.add_space(10.0);
    ui.label(
        RichText::new(title.to_uppercase())
            .size(11.0)
            .strong()
            .color(palette.faint),
    );
    ui.add_space(2.0);
}

pub fn list_row(
    ui: &mut egui::Ui,
    id: Id,
    selected: bool,
    palette: &Palette,
    add: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    let hovered = ui.ctx().read_response(id).is_some_and(|response| response.hovered());
    let fill = if selected {
        palette.accent.gamma_multiply(0.13)
    } else if hovered {
        palette.hover
    } else if palette.light {
        palette.bg
    } else {
        palette.raised.gamma_multiply(0.7)
    };
    let frame = egui::Frame::NONE
        .fill(fill)
        .stroke(if selected {
            Stroke::new(1.0, palette.accent.gamma_multiply(0.45))
        } else {
            Stroke::NONE
        })
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(9, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().interact_size.y = 16.0;
            ui.spacing_mut().item_spacing.y = 2.0;
            add(ui);
        });
    let response = ui.interact(frame.response.rect, id, Sense::click());
    ui.add_space(4.0);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub fn list_row_compact(ui: &mut egui::Ui, id: Id, hover: Color32, add: impl FnOnce(&mut egui::Ui)) -> egui::Response {
    let hovered = ui.ctx().read_response(id).is_some_and(|response| response.hovered());
    let frame = egui::Frame::NONE
        .fill(if hovered { hover } else { Color32::TRANSPARENT })
        .corner_radius(CornerRadius::same(5))
        .inner_margin(Margin::symmetric(5, 3))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().interact_size.y = 20.0;
            add(ui);
        });
    ui.interact(frame.response.rect, id, Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub fn session_row_detailed(ui: &mut egui::Ui, session: &Session, palette: &Palette) -> egui::Response {
    let id = ui.id().with(("session-row-detailed", session.client.id(), &session.id));
    list_row(ui, id, false, palette, |ui| {
        ui.horizontal(|ui| {
            let color = kind_color(session.client, palette);
            ui.label(
                RichText::new(session.client.label())
                    .size(scale::FONT_XS)
                    .strong()
                    .color(color),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(session.updated.map(age_system).unwrap_or_default())
                        .size(scale::FONT_XS)
                        .color(palette.faint),
                );
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(egui::Label::new(RichText::new(&session.title).size(scale::FONT_MD).strong()).truncate());
                });
            });
        });
        if !session.last_user.is_empty() && session.last_user != session.title {
            let prompt = session.last_user.split_whitespace().collect::<Vec<_>>().join(" ");
            ui.add(
                egui::Label::new(
                    RichText::new(format!("› {prompt}"))
                        .size(scale::FONT_SM)
                        .color(palette.muted),
                )
                .truncate(),
            );
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            if !session.status.is_empty() {
                ui.label(
                    RichText::new(&session.status)
                        .size(scale::FONT_XS)
                        .color(status_color(&session.status, palette)),
                );
            }
            if let Some(count) = session.messages {
                ui.label(
                    RichText::new(format!("{count} messages"))
                        .size(scale::FONT_XS)
                        .color(palette.faint),
                );
            }
            if !session.model.is_empty() {
                ui.label(RichText::new(&session.model).size(scale::FONT_XS).color(palette.faint));
            }
            ui.add(
                egui::Label::new(
                    RichText::new(session.short_id())
                        .size(scale::FONT_XS)
                        .color(palette.faint),
                )
                .truncate(),
            );
        });
    })
}

pub fn session_row(ui: &mut egui::Ui, session: &Session, palette: &Palette) -> egui::Response {
    let id = ui.id().with(("session-row", session.client.id(), &session.id));
    list_row(ui, id, false, palette, |ui| {
        ui.horizontal(|ui| {
            let color = kind_color(session.client, palette);
            ui.label(
                RichText::new(session.client.label())
                    .size(scale::FONT_XS)
                    .strong()
                    .color(color),
            );
            ui.add(egui::Label::new(RichText::new(&session.title).size(scale::FONT_MD)).truncate());
        });
        ui.horizontal(|ui| {
            let age = session.updated.map(age_system).unwrap_or_default();
            ui.label(RichText::new(age).size(scale::FONT_XS).color(palette.faint));
            if !session.status.is_empty() {
                ui.label(
                    RichText::new(&session.status)
                        .size(scale::FONT_XS)
                        .color(status_color(&session.status, palette)),
                );
            }
            if !session.model.is_empty() {
                ui.add(
                    egui::Label::new(RichText::new(&session.model).size(scale::FONT_XS).color(palette.faint))
                        .truncate(),
                );
            }
        });
    })
}

/// Brand mark that adapts to light/dark themes and saturation (including Mono).
pub fn paint_logo(painter: &egui::Painter, rect: Rect, palette: &Palette) {
    let radius = rect.width() * 0.24;
    let plate = if palette.light {
        palette.raised
    } else {
        Color32::from_rgb(17, 21, 31)
    };
    painter.rect_filled(rect, CornerRadius::same(radius as u8), plate);
    if palette.light {
        painter.rect_stroke(
            rect,
            CornerRadius::same(radius as u8),
            Stroke::new(1.0, palette.border_strong),
            egui::StrokeKind::Inside,
        );
    }
    let s = rect.width() / 512.0;
    let p = |x: f32, y: f32| rect.min + Vec2::new(x * s, y * s);
    let tone = |color: Color32| {
        if palette.saturation < 1.0 {
            desaturate(color, palette.saturation)
        } else {
            color
        }
    };
    let c_cyan = tone(palette.cyan);
    let c_accent = tone(palette.accent);
    let c_green = tone(palette.green);
    let stroke = |color: Color32| Stroke::new((25.0 * s).max(1.4), color);
    let arch: Vec<Pos2> = (0..=16)
        .map(|i| {
            let t = std::f32::consts::PI * (1.0 - i as f32 / 16.0);
            p(232.0 + 101.0 * t.cos(), 212.0 - 108.0 * t.sin())
        })
        .collect();
    painter.line_segment([p(131.0, 350.0), p(131.0, 212.0)], stroke(c_cyan));
    painter.add(egui::Shape::line(arch, stroke(c_accent)));
    painter.line_segment([p(333.0, 212.0), p(333.0, 350.0)], stroke(c_accent));
    painter.line_segment([p(131.0, 273.0), p(333.0, 273.0)], stroke(c_accent));
    painter.line_segment([p(381.0, 161.0), p(381.0, 273.0)], stroke(c_green));
    painter.line_segment([p(381.0, 273.0), p(274.0, 380.0)], stroke(c_green));
    painter.line_segment([p(274.0, 380.0), p(171.0, 380.0)], stroke(c_green));
    painter.circle_filled(p(256.0, 273.0), 27.0 * s, plate);
}

pub fn usage_color(used: f32, reset_passed: bool, palette: &Palette) -> Color32 {
    if reset_passed {
        palette.faint
    } else if used >= 0.9 {
        palette.red
    } else if used >= 0.7 {
        palette.amber
    } else {
        palette.green
    }
}

pub fn usage_badge(used: f32, reset_passed: bool) -> &'static str {
    if reset_passed {
        ""
    } else if used >= 0.9 {
        "!"
    } else if used >= 0.7 {
        "▲"
    } else {
        ""
    }
}

pub fn mini_bar(painter: &egui::Painter, rect: Rect, limit: &Limit, palette: &Palette) {
    let reset_passed = limit.is_reset_passed();
    let used = if reset_passed {
        0.0
    } else {
        limit.clamped_used().unwrap_or(0.0)
    };
    let window = if limit.window == "week" {
        "wk"
    } else {
        limit.window.as_str()
    };
    let badge = usage_badge(used, reset_passed);
    let label = if badge.is_empty() {
        format!("{window} {:.0}%", used * 100.0)
    } else {
        format!("{badge}{window} {:.0}%", used * 100.0)
    };
    let label_w = 54.0_f32.min(rect.width() * 0.65);
    painter.text(
        Pos2::new(rect.left(), rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(10.0),
        if reset_passed { palette.faint } else { palette.muted },
    );
    let track = Rect::from_min_max(
        Pos2::new(rect.left() + label_w, rect.center().y - 2.0),
        Pos2::new(rect.right(), rect.center().y + 2.0),
    );
    if track.width() > 4.0 {
        painter.rect_filled(track, CornerRadius::same(2), palette.border);
        let fill = Rect::from_min_size(track.min, Vec2::new(track.width() * used, track.height()));
        painter.rect_filled(fill, CornerRadius::same(2), usage_color(used, reset_passed, palette));
    }
}

pub fn usage_row(ui: &mut egui::Ui, limit: &Limit, palette: &Palette) {
    let reset_passed = limit.is_reset_passed();
    ui.add_space(3.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(&limit.label).size(12.5));
        ui.label(RichText::new(&limit.window).size(11.0).color(palette.faint));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| match limit.clamped_used() {
            Some(raw_used) => {
                let used = if reset_passed { 0.0 } else { raw_used };
                let badge = usage_badge(used, reset_passed);
                let text = if reset_passed {
                    "reset".to_string()
                } else if badge.is_empty() {
                    format!("{:.0}% used", used * 100.0)
                } else {
                    format!("{badge} {:.0}% used", used * 100.0)
                };
                ui.label(
                    RichText::new(text)
                        .size(12.0)
                        .strong()
                        .color(usage_color(used, reset_passed, palette)),
                );
            }
            None => {
                ui.label(
                    RichText::new(if limit.status.is_empty() {
                        "unknown"
                    } else {
                        &limit.status
                    })
                    .size(11.0)
                    .color(palette.faint),
                );
            }
        });
    });
    if let Some(raw_used) = limit.clamped_used() {
        let used = if reset_passed { 0.0 } else { raw_used };
        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 7.0), Sense::hover());
        ui.painter().rect_filled(rect, CornerRadius::same(3), palette.border);
        let fill = Rect::from_min_size(rect.min, Vec2::new(rect.width() * used, rect.height()));
        ui.painter()
            .rect_filled(fill, CornerRadius::same(3), usage_color(used, reset_passed, palette));
    }
    let mut meta = Vec::new();
    if let Some(raw_used) = limit.clamped_used()
        && !reset_passed
    {
        meta.push(format!("{:.0}% left", (1.0 - raw_used).max(0.0) * 100.0));
    }
    let reset = limit.reset_text();
    if !reset.is_empty() {
        meta.push(reset);
    }
    if let Some(updated) = limit.updated
        && updated.elapsed().is_ok_and(|age| age > Duration::from_secs(600))
    {
        meta.push(format!("updated {}", ago_system(updated)));
    }
    if !meta.is_empty() {
        ui.label(
            RichText::new(meta.join(" · "))
                .size(scale::FONT_XS)
                .color(palette.faint),
        );
    }
}

pub fn kind_color(kind: CliKind, palette: &Palette) -> Color32 {
    let [r, g, b] = if palette.light { kind.light_tint() } else { kind.tint() };
    let color = Color32::from_rgb(r, g, b);
    if palette.saturation < 1.0 {
        desaturate(color, palette.saturation)
    } else {
        color
    }
}

pub fn notice_color(kind: crate::app::NoticeKind, palette: &Palette) -> Color32 {
    match kind {
        crate::app::NoticeKind::Info => palette.muted,
        crate::app::NoticeKind::Success => palette.green,
        crate::app::NoticeKind::Error => palette.red,
        crate::app::NoticeKind::Attention => palette.amber,
    }
}

pub fn status_color(status: &str, palette: &Palette) -> Color32 {
    match status {
        "ready" => palette.green,
        "error" => palette.red,
        "interrupted" | "awaiting" | "prompt" => palette.amber,
        "thinking" | "tool" => palette.cyan,
        _ => palette.faint,
    }
}

pub fn file_tint(name: &str, palette: &Palette) -> Color32 {
    let extension = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match extension.as_str() {
        "rs" | "go" | "c" | "h" | "cpp" | "swift" => palette.amber,
        "py" | "js" | "ts" | "tsx" | "jsx" => palette.term.ansi[4],
        "md" | "txt" | "rst" => palette.accent,
        "json" | "toml" | "yaml" | "yml" | "lock" => palette.term.ansi[3],
        "sh" | "zsh" | "bash" | "fish" | "ps1" | "bat" | "cmd" => palette.green,
        "png" | "jpg" | "jpeg" | "svg" | "webp" | "gif" => palette.term.ansi[5],
        _ => palette.muted,
    }
}

pub fn load_first(paths: &[PathBuf]) -> Option<Vec<u8>> {
    paths.iter().find_map(|path| fs::read(path).ok())
}

pub fn pretty_path(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn format_age(duration: Duration) -> String {
    let seconds = duration.as_secs();
    match seconds {
        0..=59 => "now".into(),
        60..=3_599 => format!("{}m", seconds / 60),
        3_600..=86_399 => format!("{}h", seconds / 3_600),
        _ => format!("{}d", seconds / 86_400),
    }
}

/// "just now" / "5m ago" — for sentences, where a bare "now" reads wrong.
pub fn ago(duration: Duration) -> String {
    if duration.as_secs() < 60 {
        "just now".into()
    } else {
        format!("{} ago", format_age(duration))
    }
}

pub fn ago_system(time: std::time::SystemTime) -> String {
    std::time::SystemTime::now()
        .duration_since(time)
        .map(ago)
        .unwrap_or_else(|_| "just now".into())
}

pub fn age_system(time: std::time::SystemTime) -> String {
    std::time::SystemTime::now()
        .duration_since(time)
        .map(format_age)
        .unwrap_or_else(|_| "now".into())
}

pub fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// Current branch from `.git/HEAD` (worktrees' `.git` files included).
pub fn read_git_branch(start: &Path) -> Option<String> {
    let mut dir = Some(start);
    while let Some(path) = dir {
        let git = path.join(".git");
        let git_dir = if git.is_dir() {
            Some(git)
        } else if git.is_file() {
            fs::read_to_string(&git)
                .ok()
                .and_then(|text| text.strip_prefix("gitdir:").map(|rest| PathBuf::from(rest.trim())))
                .map(|dir| if dir.is_absolute() { dir } else { path.join(dir) })
        } else {
            None
        };
        if let Some(git_dir) = git_dir {
            let head = fs::read_to_string(git_dir.join("HEAD")).ok()?;
            let head = head.trim();
            return Some(match head.strip_prefix("ref: refs/heads/") {
                Some(branch) => branch.to_string(),
                None => head.chars().take(7).collect(),
            });
        }
        dir = path.parent();
    }
    None
}

/// Shows the platform folder chooser; `None` when cancelled.
pub fn choose_folder(start: &Path) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    let output = {
        let escape = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!(
            "POSIX path of (choose folder with prompt \"Open a project folder in ais\" default location POSIX file \"{}\")",
            escape(&start.to_string_lossy())
        );
        Command::new("osascript").arg("-e").arg(script).output().ok()?
    };
    #[cfg(target_os = "windows")]
    let output = {
        let escaped = start.to_string_lossy().replace('\'', "''");
        let script = format!(
            "Add-Type -AssemblyName System.Windows.Forms; \
             [System.Windows.Forms.Application]::EnableVisualStyles(); \
             $d = New-Object System.Windows.Forms.FolderBrowserDialog; \
             $d.Description = 'Open a project folder in ais'; \
             $d.UseDescriptionForTitle = $true; \
             $d.SelectedPath = '{escaped}'; \
             if ($d.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {{ \
                 [Console]::OutputEncoding = [System.Text.Encoding]::UTF8; \
                 Write-Output $d.SelectedPath \
             }}"
        );
        let mut cmd = Command::new("powershell.exe");
        cmd.args([
            "-NoProfile",
            "-NonInteractive",
            "-STA",
            "-WindowStyle",
            "Hidden",
            "-Command",
            &script,
        ]);
        no_window(&mut cmd);
        cmd.output().ok()?
    };
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let output = Command::new("zenity")
        .args(["--file-selection", "--directory", "--filename"])
        .arg(format!("{}/", start.display()))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let trimmed = text.trim_end_matches(['/', '\\']);
    let path = if trimmed.is_empty() {
        if cfg!(windows) {
            return None;
        }
        PathBuf::from("/")
    } else if cfg!(windows) && trimmed.len() == 2 && trimmed.as_bytes()[1] == b':' {
        PathBuf::from(format!("{trimmed}\\"))
    } else {
        PathBuf::from(trimmed)
    };
    path.is_dir().then_some(path)
}

pub fn open_external(target: &str, reveal: bool) {
    let target = target.to_string();
    if let Err(error) = thread::Builder::new().name("ais-open".into()).spawn(move || {
        #[cfg(target_os = "macos")]
        let status = if reveal {
            Command::new("open").arg("-R").arg(&target).status()
        } else {
            Command::new("open").arg(&target).status()
        };
        #[cfg(target_os = "windows")]
        let status: std::io::Result<std::process::ExitStatus> = {
            if reveal && !Path::new(&target).is_dir() {
                use std::os::windows::process::CommandExt;
                let safe = target.replace('"', "");
                Command::new("explorer.exe")
                    .raw_arg(format!("/select,\"{safe}\""))
                    .status()
            } else {
                use std::os::windows::ffi::OsStrExt;
                let wide_op: Vec<u16> = "open\0".encode_utf16().collect();
                let wide_target: Vec<u16> = std::ffi::OsStr::new(&target)
                    .encode_wide()
                    .chain(std::iter::once(0))
                    .collect();
                let rc = unsafe {
                    windows_sys::Win32::UI::Shell::ShellExecuteW(
                        std::ptr::null_mut(),
                        wide_op.as_ptr(),
                        wide_target.as_ptr(),
                        std::ptr::null(),
                        std::ptr::null(),
                        windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
                    )
                } as isize;
                if rc <= 32 {
                    Err(std::io::Error::other(format!("ShellExecuteW returned {rc}")))
                } else {
                    return;
                }
            }
        };
        #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
        let status = {
            let path = Path::new(&target);
            let shown = match path.parent() {
                Some(parent) if reveal && !path.is_dir() => parent.as_os_str(),
                _ => path.as_os_str(),
            };
            Command::new("xdg-open").arg(shown).status()
        };
        if let Err(error) = status {
            log::warn(&format!("open_external failed for '{target}': {error}"));
        }
    }) {
        log::warn(&format!("open_external spawn failed: {error}"));
    }
}

pub fn system_notification(text: &str) {
    let text = text.to_string();
    if let Err(error) = thread::Builder::new().name("ais-notify".into()).spawn(move || {
        #[cfg(target_os = "macos")]
        {
            let escape = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
            let script = format!(
                "display notification \"{}\" with title \"{}\"",
                escape(&text),
                crate::app::APP_NAME
            );
            if let Err(error) = Command::new("osascript").arg("-e").arg(script).status() {
                log::warn(&format!("system_notification failed: {error}"));
            }
        }
        #[cfg(target_os = "windows")]
        {
            let xml_escape = |s: &str| {
                s.replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;")
                    .replace('"', "&quot;")
                    .replace('\'', "&apos;")
            };
            let title = xml_escape(crate::app::APP_NAME);
            let body = xml_escape(&text).replace('\'', "''");
            let script = format!(
                "[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null; \
                 [Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType = WindowsRuntime] | Out-Null; \
                 $xml = New-Object Windows.Data.Xml.Dom.XmlDocument; \
                 $xml.LoadXml('<toast><visual><binding template=\"ToastText02\"><text id=\"1\">{title}</text><text id=\"2\">{body}</text></binding></visual></toast>'); \
                 $toast = [Windows.UI.Notifications.ToastNotification]::new($xml); \
                 [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('ais').Show($toast)"
            );
            let mut cmd = Command::new("powershell.exe");
            cmd.args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", &script]);
            no_window(&mut cmd);
            if let Err(error) = cmd.status() {
                log::warn(&format!("system_notification failed: {error}"));
            }
        }
        #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
        {
            if let Err(error) = Command::new("notify-send")
                .arg(crate::app::APP_NAME)
                .arg(&text)
                .status()
            {
                log::warn(&format!("system_notification failed: {error}"));
            }
        }
    }) {
        log::warn(&format!("system_notification spawn failed: {error}"));
    }
}

pub fn right_dock(ctx: &egui::Context, palette: &Palette) -> egui::SidePanel {
    let (default_width, range) = crate::app::side_width(ctx, 290.0);
    let swapped = ctx
        .data(|data| data.get_temp::<bool>(Id::new("swap-sides")))
        .unwrap_or(false);
    let side = if swapped {
        egui::panel::Side::Left
    } else {
        egui::panel::Side::Right
    };
    egui::SidePanel::new(side, "right-dock")
        .resizable(true)
        .default_width(default_width)
        .width_range(range)
        .frame(
            egui::Frame::NONE
                .fill(palette.panel)
                .inner_margin(Margin::symmetric(10, 10))
                .stroke(Stroke::new(1.0, palette.border)),
        )
}

pub fn right_tabs(
    ui: &mut egui::Ui,
    current: crate::app::RightTab,
    unread: usize,
    palette: &Palette,
) -> Option<crate::app::RightTab> {
    let alerts = if unread > 0 && current != crate::app::RightTab::Notices {
        format!("Alerts · {}", unread.min(99))
    } else {
        "Alerts".to_string()
    };
    let items = [
        ("Files", crate::app::RightTab::Files),
        ("Usage", crate::app::RightTab::Usage),
        (alerts.as_str(), crate::app::RightTab::Notices),
    ];
    let labels: Vec<&str> = items.iter().map(|(label, _)| *label).collect();
    let selected = items.iter().position(|(_, tab)| *tab == current).unwrap_or(0);
    tabs(ui, &labels, selected, palette).map(|index| items[index].1)
}
