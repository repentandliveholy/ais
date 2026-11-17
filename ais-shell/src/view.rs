//! Paints an `alacritty_terminal` grid with egui.
//!
//! Text is grouped into same-style runs of glyphs whose advance matches the
//! cell width; anything else (wide CJK/emoji, fallback-font symbols, combining
//! marks) is placed individually and centered in its cells, so the grid never
//! drifts out of alignment.

use crate::terminal::{SearchMatch, TerminalPane};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor};
use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{self, Color32, CornerRadius, FontFamily, FontId, Pos2, Rect, Shape, Stroke, Vec2};

pub const BOLD_FAMILY: &str = "ais-mono-bold";

#[derive(Clone, Copy)]
pub struct TermColors {
    pub bg: Color32,
    pub fg: Color32,
    pub cursor: Color32,
    pub selection: Color32,
    pub ansi: [Color32; 16],
}

#[derive(Clone)]
pub struct Metrics {
    pub cell: Vec2,
    pub font: FontId,
    pub bold: FontId,
}

pub fn metrics(ctx: &egui::Context, size: f32) -> Metrics {
    let font = FontId::new(size, FontFamily::Monospace);
    let bold = FontId::new(size, FontFamily::Name(BOLD_FAMILY.into()));
    let (width, height) = ctx.fonts(|fonts| (fonts.glyph_width(&font, 'M'), fonts.row_height(&font)));
    Metrics {
        cell: Vec2::new(width.max(1.0), height.max(1.0)),
        font,
        bold,
    }
}

impl Metrics {
    /// Viewport cell under a screen position (clamped to the grid).
    pub fn cell_at(&self, rect: Rect, pos: Pos2, cols: u16, rows: u16) -> (usize, usize, bool) {
        let x = (pos.x - rect.left()) / self.cell.x;
        let y = (pos.y - rect.top()) / self.cell.y;
        let col = (x.floor().max(0.0) as usize).min(usize::from(cols.saturating_sub(1)));
        let row = (y.floor().max(0.0) as usize).min(usize::from(rows.saturating_sub(1)));
        let right_half = x - x.floor() >= 0.5;
        (row, col, right_half)
    }

    pub fn grid_size(&self, rect: Rect) -> (u16, u16) {
        let cols = (rect.width() / self.cell.x).floor().max(1.0) as u16;
        let rows = (rect.height() / self.cell.y).floor().max(1.0) as u16;
        (rows, cols)
    }
}

fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

fn ansi_256(index: u8, colors: &TermColors) -> Color32 {
    if index < 16 {
        return colors.ansi[usize::from(index)];
    }
    if index < 232 {
        const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
        let value = index - 16;
        return rgb(
            LEVELS[usize::from(value / 36)],
            LEVELS[usize::from((value % 36) / 6)],
            LEVELS[usize::from(value % 6)],
        );
    }
    let gray = 8 + (index - 232) * 10;
    rgb(gray, gray, gray)
}

pub fn blend(from: Color32, to: Color32, amount: f32) -> Color32 {
    let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * amount).round() as u8;
    rgb(mix(from.r(), to.r()), mix(from.g(), to.g()), mix(from.b(), to.b()))
}

fn resolve(color: Color, colors: &TermColors, overrides: &Colors) -> Color32 {
    match color {
        Color::Spec(value) => rgb(value.r, value.g, value.b),
        Color::Indexed(index) => match overrides[usize::from(index)] {
            Some(value) => rgb(value.r, value.g, value.b),
            None => ansi_256(index, colors),
        },
        Color::Named(name) => {
            if let Some(value) = overrides[name] {
                return rgb(value.r, value.g, value.b);
            }
            let index = name as usize;
            match name {
                NamedColor::Foreground | NamedColor::BrightForeground => colors.fg,
                NamedColor::Background => colors.bg,
                NamedColor::Cursor => colors.cursor,
                NamedColor::DimForeground => blend(colors.fg, colors.bg, 0.35),
                _ if index < 16 => colors.ansi[index],
                _ => {
                    // DimBlack..DimWhite follow Cursor (258) in declaration order.
                    let dim_index = index.saturating_sub(NamedColor::DimBlack as usize).min(7);
                    blend(colors.ansi[dim_index], colors.bg, 0.35)
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
struct Style {
    fg: Color32,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
}

struct Run {
    row: usize,
    col: usize,
    cells: usize,
    text: String,
    style: Style,
}

pub struct DrawOptions<'a> {
    pub focused: bool,
    pub cursor_on: bool,
    pub accent: Color32,
    pub hover_link: Option<&'a str>,
    pub search_matches: &'a [SearchMatch],
    pub current_match: Option<usize>,
}

pub fn draw(
    ui: &egui::Ui,
    rect: Rect,
    pane: &TerminalPane,
    colors: &TermColors,
    metrics: &Metrics,
    options: DrawOptions<'_>,
) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::ZERO, colors.bg);
    let cell = metrics.cell;
    let emulator = pane.emulator();
    let content = emulator.term.renderable_content();
    let offset = content.display_offset as i32;
    let selection = content.selection;
    let cursor = content.cursor;
    let overrides = content.colors;
    let cursor_row = cursor.point.line.0 + offset;
    let cursor_col = cursor.point.column.0;
    let block_cursor = options.focused && options.cursor_on && cursor.shape == CursorShape::Block;

    let mut backgrounds: Vec<(usize, usize, usize, Color32)> = Vec::new();
    let mut runs: Vec<Run> = Vec::new();
    let mut singles: Vec<(usize, usize, usize, String, Style)> = Vec::new();
    let origin = rect.left_top();

    ui.ctx().fonts(|fonts| {
        let mut run: Option<Run> = None;
        for indexed in content.display_iter {
            let row = indexed.point.line.0 + offset;
            if row < 0 {
                continue;
            }
            let row = row as usize;
            let col = indexed.point.column.0;
            let flags = indexed.cell.flags;
            if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            let span = if flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
            let mut fg = resolve(indexed.cell.fg, colors, overrides);
            let mut bg = resolve(indexed.cell.bg, colors, overrides);
            if flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if flags.contains(Flags::DIM) {
                fg = blend(fg, bg, 0.4);
            }
            if !options.search_matches.is_empty() {
                let grid_line = indexed.point.line.0;
                for (match_idx, m) in options.search_matches.iter().enumerate() {
                    if m.line == grid_line && col >= m.col && col < m.col + m.len {
                        let is_current = options.current_match == Some(match_idx);
                        bg = blend(bg, options.accent, if is_current { 0.6 } else { 0.25 });
                        break;
                    }
                }
            }
            if selection.is_some_and(|range| range.contains(indexed.point)) {
                bg = colors.selection;
            }
            if block_cursor && row as i32 == cursor_row && col == cursor_col {
                fg = colors.bg;
                bg = colors.cursor;
            }
            if bg != colors.bg {
                match backgrounds.last_mut() {
                    Some((r, start, len, color)) if *r == row && *start + *len == col && *color == bg => *len += span,
                    _ => backgrounds.push((row, col, span, bg)),
                }
            }
            let hovered_osc8 = options
                .hover_link
                .is_some_and(|uri| indexed.cell.hyperlink().is_some_and(|link| link.uri() == uri));
            let style = Style {
                fg,
                bold: flags.contains(Flags::BOLD),
                italic: flags.contains(Flags::ITALIC),
                underline: flags.intersects(Flags::ALL_UNDERLINES) || hovered_osc8,
                strike: flags.contains(Flags::STRIKEOUT),
            };
            let mut ch = indexed.cell.c;
            if flags.contains(Flags::HIDDEN) || ch == '\t' || ch == '\0' {
                ch = ' ';
            }
            let zerowidth = indexed.cell.zerowidth().filter(|marks| !marks.is_empty());
            let font = if style.bold { &metrics.bold } else { &metrics.font };
            let fits = span == 1
                && zerowidth.is_none()
                && (ch.is_ascii() || (fonts.glyph_width(font, ch) - cell.x).abs() < 0.6);
            if fits {
                if let Some(current) = run.as_mut()
                    && current.row == row
                    && current.col + current.cells == col
                    && current.style == style
                {
                    current.text.push(ch);
                    current.cells += 1;
                    continue;
                }
                if let Some(done) = run.take() {
                    runs.push(done);
                }
                run = Some(Run {
                    row,
                    col,
                    cells: 1,
                    text: ch.to_string(),
                    style,
                });
            } else {
                if let Some(done) = run.take() {
                    runs.push(done);
                }
                let mut text = ch.to_string();
                if let Some(marks) = zerowidth {
                    text.extend(marks.iter());
                }
                singles.push((row, col, span, text, style));
            }
        }
        if let Some(done) = run.take() {
            runs.push(done);
        }
    });

    let mut shapes: Vec<Shape> = Vec::with_capacity(backgrounds.len() + runs.len() + singles.len());
    for (row, col, len, color) in backgrounds {
        let min = origin + Vec2::new(col as f32 * cell.x, row as f32 * cell.y);
        // Slight overlap avoids hairline seams between rows at fractional scales.
        let size = Vec2::new(len as f32 * cell.x + 0.5, cell.y + 0.5);
        shapes.push(Shape::rect_filled(
            Rect::from_min_size(min, size),
            CornerRadius::ZERO,
            color,
        ));
    }
    let mut decorations: Vec<Shape> = Vec::new();
    let job_for = |text: String, style: &Style| {
        let font = if style.bold {
            metrics.bold.clone()
        } else {
            metrics.font.clone()
        };
        LayoutJob::single_section(
            text,
            TextFormat {
                font_id: font,
                color: style.fg,
                italics: style.italic,
                ..Default::default()
            },
        )
    };
    let mut decorate = |row: usize, col: usize, cells: usize, style: &Style| {
        let left = origin.x + col as f32 * cell.x;
        let right = left + cells as f32 * cell.x;
        let top = origin.y + row as f32 * cell.y;
        if style.underline {
            let y = top + cell.y - 1.5;
            decorations.push(Shape::line_segment(
                [Pos2::new(left, y), Pos2::new(right, y)],
                Stroke::new(1.0, style.fg),
            ));
        }
        if style.strike {
            let y = top + cell.y * 0.55;
            decorations.push(Shape::line_segment(
                [Pos2::new(left, y), Pos2::new(right, y)],
                Stroke::new(1.0, style.fg),
            ));
        }
    };
    ui.ctx().fonts(|fonts| {
        for run in runs {
            decorate(run.row, run.col, run.cells, &run.style);
            if run.text.trim().is_empty() {
                continue;
            }
            let pos = origin + Vec2::new(run.col as f32 * cell.x, run.row as f32 * cell.y);
            let galley = fonts.layout_job(job_for(run.text, &run.style));
            shapes.push(Shape::galley(pos, galley, run.style.fg));
        }
        for (row, col, span, text, style) in singles {
            decorate(row, col, span, &style);
            if text.trim().is_empty() {
                continue;
            }
            let galley = fonts.layout_job(job_for(text, &style));
            let width = galley.size().x;
            let slot = span as f32 * cell.x;
            let x = origin.x + col as f32 * cell.x + ((slot - width) / 2.0).max(0.0).min(slot);
            let y = origin.y + row as f32 * cell.y + ((cell.y - galley.size().y) / 2.0).min(0.0);
            shapes.push(Shape::galley(Pos2::new(x, y), galley, style.fg));
        }
    });
    shapes.extend(decorations);

    // Non-block cursor shapes, or the hollow block of an unfocused pane.
    if options.cursor_on && cursor.shape != CursorShape::Hidden && cursor_row >= 0 && !block_cursor {
        let min = origin + Vec2::new(cursor_col as f32 * cell.x, cursor_row as f32 * cell.y);
        let full = Rect::from_min_size(min, cell);
        let color = colors.cursor;
        match cursor.shape {
            CursorShape::Beam => shapes.push(Shape::rect_filled(
                Rect::from_min_size(min, Vec2::new(2.0, cell.y)),
                CornerRadius::ZERO,
                color,
            )),
            CursorShape::Underline => shapes.push(Shape::rect_filled(
                Rect::from_min_size(min + Vec2::new(0.0, cell.y - 2.0), Vec2::new(cell.x, 2.0)),
                CornerRadius::ZERO,
                color,
            )),
            _ => shapes.push(Shape::rect_stroke(
                full.shrink(0.5),
                CornerRadius::ZERO,
                Stroke::new(1.0, color),
                egui::StrokeKind::Inside,
            )),
        }
    }
    painter.extend(shapes);
}
