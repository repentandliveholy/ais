//! Central workspace: welcome screen, toolbar, agent cards, pane grid and terminal view.

use crate::app::{Action, AisApp, LaunchMode, PendingPaste, RightTab, Space, Target};
use crate::cli::CliKind;
use crate::input::{self, MouseButton};
use crate::keys::Command as Cmd;
use crate::sessions::truncate;
use crate::terminal::TerminalPane;
use crate::theme::{Palette, scale};
use crate::view::{self, DrawOptions};
use crate::widgets::{
    UiIcon, ago_system, close_button, kind_color, mini_bar, paint_logo, pill, pretty_path, primary_button,
    session_row_detailed, small_button, tiny_icon_button, tool_button,
};
use alacritty_terminal::term::TermMode;
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, Id, ImeEvent, Key, Layout, Margin, Modifiers, Pos2, Rect, RichText,
    Sense, Stroke, UiBuilder, Vec2,
};
use std::time::Duration;

struct Divider {
    rect: Rect,
    horizontal_drag: bool,
    extent: f32,
}

fn layout_panes(space: &Space, area: Rect, gap: f32) -> (Vec<Rect>, Vec<Divider>) {
    let count = space.panes.len();
    if count <= 1 || space.zoomed {
        let mut rects = vec![Rect::NOTHING; count.max(1)];
        rects[space.active.min(count.saturating_sub(1))] = area;
        return (rects, Vec::new());
    }
    let width = (area.width() - gap).max(1.0);
    let height = (area.height() - gap).max(1.0);
    let x = area.left() + (width * space.split_x).round();
    let y = area.top() + (height * space.split_y).round();
    let left = Rect::from_min_max(area.min, Pos2::new(x, area.bottom()));
    let right = Rect::from_min_max(Pos2::new(x + gap, area.top()), area.max);
    let top = Rect::from_min_max(area.min, Pos2::new(area.right(), y));
    let bottom = Rect::from_min_max(Pos2::new(area.left(), y + gap), area.max);
    let vdiv = |top_y: f32, bottom_y: f32| Divider {
        rect: Rect::from_min_max(Pos2::new(x, top_y), Pos2::new(x + gap, bottom_y)),
        horizontal_drag: true,
        extent: width,
    };
    let hdiv = |left_x: f32, right_x: f32| Divider {
        rect: Rect::from_min_max(Pos2::new(left_x, y), Pos2::new(right_x, y + gap)),
        horizontal_drag: false,
        extent: height,
    };
    match (count, space.stacked) {
        (2, false) => (vec![left, right], vec![vdiv(area.top(), area.bottom())]),
        (2, true) => (vec![top, bottom], vec![hdiv(area.left(), area.right())]),
        (3, false) => {
            let upper = Rect::from_min_max(right.min, Pos2::new(right.right(), y));
            let lower = Rect::from_min_max(Pos2::new(right.left(), y + gap), right.max);
            (
                vec![left, upper, lower],
                vec![vdiv(area.top(), area.bottom()), hdiv(right.left(), right.right())],
            )
        }
        (3, true) => {
            let first = Rect::from_min_max(bottom.min, Pos2::new(x, bottom.bottom()));
            let second = Rect::from_min_max(Pos2::new(x + gap, bottom.top()), bottom.max);
            (
                vec![top, first, second],
                vec![hdiv(area.left(), area.right()), vdiv(bottom.top(), bottom.bottom())],
            )
        }
        _ => {
            let rects = vec![
                Rect::from_min_max(area.min, Pos2::new(x, y)),
                Rect::from_min_max(Pos2::new(x + gap, area.top()), Pos2::new(area.right(), y)),
                Rect::from_min_max(Pos2::new(area.left(), y + gap), Pos2::new(x, area.bottom())),
                Rect::from_min_max(Pos2::new(x + gap, y + gap), area.max),
            ];
            (
                rects,
                vec![vdiv(area.top(), area.bottom()), hdiv(area.left(), area.right())],
            )
        }
    }
}

impl AisApp {
    pub fn workspace(&mut self, ui: &mut egui::Ui) {
        let palette = self.palette();
        let Some(space_index) = self.active_space else {
            self.welcome(ui, &palette);
            return;
        };
        if !self.spaces[space_index].available {
            self.unavailable_space(ui, space_index, &palette);
            return;
        }
        self.toolbar(ui, space_index, &palette);
        ui.add_space(6.0);
        let area = ui.available_rect_before_wrap();
        ui.allocate_rect(area, Sense::hover());
        let (rects, dividers) = layout_panes(&self.spaces[space_index], area, 6.0);
        for (index, divider) in dividers.iter().enumerate() {
            let id = Id::new(("divider", space_index, index));
            let response = ui
                .interact(divider.rect, id, Sense::drag())
                .on_hover_cursor(if divider.horizontal_drag {
                    egui::CursorIcon::ResizeHorizontal
                } else {
                    egui::CursorIcon::ResizeVertical
                });
            let color = if response.hovered() || response.dragged() {
                palette.accent.gamma_multiply(0.6)
            } else {
                Color32::TRANSPARENT
            };
            let line = if divider.horizontal_drag {
                Rect::from_center_size(divider.rect.center(), Vec2::new(2.0, divider.rect.height() - 16.0))
            } else {
                Rect::from_center_size(divider.rect.center(), Vec2::new(divider.rect.width() - 16.0, 2.0))
            };
            ui.painter().rect_filled(line, CornerRadius::same(1), color);
            if response.dragged() {
                let delta = response.drag_delta();
                let space = &mut self.spaces[space_index];
                if divider.horizontal_drag {
                    space.split_x = (space.split_x + delta.x / divider.extent).clamp(0.15, 0.85);
                } else {
                    space.split_y = (space.split_y + delta.y / divider.extent).clamp(0.15, 0.85);
                }
                self.mark_settings_dirty();
            }
            if response.double_clicked() {
                let space = &mut self.spaces[space_index];
                space.split_x = 0.5;
                space.split_y = 0.5;
                self.mark_settings_dirty();
            }
        }
        let uids: Vec<u64> = self.spaces[space_index].panes.iter().map(|pane| pane.uid).collect();
        for (uid, rect) in uids.into_iter().zip(rects) {
            if rect.is_positive() {
                self.pane_ui(ui, space_index, uid, rect, &palette);
            }
        }
    }

    fn unavailable_space(&mut self, ui: &mut egui::Ui, space_index: usize, palette: &Palette) {
        let name = self.spaces[space_index].name.clone();
        let path = self.spaces[space_index].path.clone();
        ui.add_space((ui.available_height() * 0.28).max(20.0));
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new(format!("Folder unavailable: {name}"))
                    .size(scale::FONT_XL)
                    .strong()
                    .color(palette.red),
            );
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!(
                    "{} does not exist right now (external drive unmounted or folder moved).",
                    path.display()
                ))
                .size(scale::FONT_MD)
                .color(palette.muted),
            );
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if ui.add(small_button("Retry", palette)).clicked() {
                    self.actions.push(Action::RetrySpace(space_index));
                }
                if ui.add(small_button("Close space", palette)).clicked() {
                    self.actions.push(Action::CloseSpace(space_index));
                }
            });
        });
    }

    fn welcome(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        let any_cli = CliKind::ALL.iter().any(|k| self.cli[k.index()].installed);
        ui.add_space((ui.available_height() * 0.14).max(16.0));
        ui.vertical_centered(|ui| {
            let (mark, _) = ui.allocate_exact_size(Vec2::splat(56.0), Sense::hover());
            paint_logo(ui.painter(), mark, palette);
            ui.add_space(10.0);
            ui.label(RichText::new(crate::app::APP_TITLE).size(24.0).strong());
            ui.label(
                RichText::new("All your AI coding CLIs, projects and conversations in one window.")
                    .size(scale::FONT_LG)
                    .color(palette.muted),
            );
            ui.add_space(16.0);
            if ui
                .add(primary_button(
                    &format!("Open folder…   {}", self.key_hint(Cmd::OpenFolder)),
                    palette,
                ))
                .clicked()
            {
                self.actions.push(Action::PickFolder);
            }
            ui.add_space(18.0);
            let card_w = ui.available_width().min(540.0);
            egui::Frame::NONE
                .fill(palette.panel)
                .stroke(Stroke::new(1.0, palette.border))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(Margin::same(14))
                .show(ui, |ui| {
                    ui.set_width(card_w);
                    ui.label(
                        RichText::new("Getting started")
                            .strong()
                            .size(scale::FONT_MD)
                            .color(palette.text),
                    );
                    ui.add_space(6.0);
                    for (step, body) in [
                        (
                            "1. Project spaces",
                            "Each folder opens as a tab with up to 4 side-by-side CLI or shell panes.",
                        ),
                        (
                            "2. Local history",
                            if self.sessions.ais().is_some() {
                                "ais indexer detected — resume past Claude, Codex, OMP, Gemini, agy or Pi sessions."
                            } else {
                                "Optional `ais` indexer not in PATH (set AIS_BIN to list saved conversations & limits)."
                            },
                        ),
                        (
                            "3. Keyboard first",
                            "Use the search bar or shortcuts to launch CLIs, split panes and switch spaces.",
                        ),
                    ] {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(step).strong().size(scale::FONT_SM).color(palette.accent));
                            ui.label(RichText::new(body).size(scale::FONT_SM).color(palette.muted));
                        });
                    }
                    if !any_cli {
                        ui.add_space(8.0);
                        ui.separator();
                        ui.add_space(6.0);
                        self.no_cli_banner(ui, palette);
                    }
                });
        });
    }

    fn no_cli_banner(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new("No AI CLIs found in PATH.")
                    .strong()
                    .size(scale::FONT_SM)
                    .color(palette.amber),
            );
            if ui.add(small_button("Re-check CLIs", palette)).clicked() {
                self.start_cli_check();
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            for kind in CliKind::ALL {
                if ui
                    .link(
                        RichText::new(kind.label())
                            .size(scale::FONT_SM)
                            .color(kind_color(kind, palette)),
                    )
                    .clicked()
                {
                    self.actions.push(Action::OpenUrl(kind.install_url().to_string()));
                }
            }
        });
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, space_index: usize, palette: &Palette) {
        let (name, path, panes, zoomed) = {
            let space = &self.spaces[space_index];
            (space.name.clone(), space.path.clone(), space.panes.len(), space.zoomed)
        };
        let show_icons = self.settings.show_icons;
        let narrow = ui.available_width() < 640.0;
        let label = |text: &'static str| if narrow { "" } else { text };
        ui.horizontal(|ui| {
            ui.set_height(28.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if panes > 1
                    && tool_button(
                        ui,
                        if zoomed { UiIcon::Compress } else { UiIcon::Expand },
                        "",
                        zoomed,
                        show_icons,
                        palette,
                    )
                    .on_hover_text(format!("Zoom active pane  {}", self.key_hint(Cmd::ZoomPane)))
                    .clicked()
                {
                    self.actions.push(Action::ToggleZoom);
                }
                if tool_button(ui, UiIcon::SplitDown, "", false, show_icons, palette)
                    .on_hover_text(format!("Split down  {}", self.key_hint(Cmd::SplitDown)))
                    .clicked()
                {
                    self.actions.push(Action::Split(false));
                }
                if tool_button(ui, UiIcon::SplitRight, "", false, show_icons, palette)
                    .on_hover_text(format!("Split right  {}", self.key_hint(Cmd::SplitRight)))
                    .clicked()
                {
                    self.actions.push(Action::Split(true));
                }
                if tool_button(ui, UiIcon::Terminal, label("Terminal"), false, show_icons, palette)
                    .on_hover_text(format!(
                        "Open a shell in this project  {}",
                        self.key_hint(Cmd::NewShell)
                    ))
                    .clicked()
                {
                    self.actions.push(Action::Shell(Target::Auto));
                }
                if tool_button(
                    ui,
                    UiIcon::History,
                    "",
                    self.settings.sidebar_sessions && self.settings.show_sidebar,
                    show_icons,
                    palette,
                )
                .on_hover_text("Conversations of this project")
                .clicked()
                {
                    self.actions.push(Action::ShowSessions(true));
                }
                if tool_button(
                    ui,
                    UiIcon::Files,
                    "",
                    self.right_tab() == Some(RightTab::Files),
                    show_icons,
                    palette,
                )
                .on_hover_text(format!("Files  {}", self.key_hint(Cmd::ToggleFiles)))
                .clicked()
                {
                    self.actions.push(Action::ToggleFiles);
                }
                if tool_button(
                    ui,
                    UiIcon::Gauge,
                    label("Usage"),
                    self.right_tab() == Some(RightTab::Usage),
                    show_icons,
                    palette,
                )
                .on_hover_text(format!("Plan limits: 5h / week  {}", self.key_hint(Cmd::ToggleUsage)))
                .clicked()
                {
                    self.actions.push(Action::ToggleUsage);
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    let pretty = pretty_path(&path, &self.home);
                    let title = ui.add(
                        egui::Label::new(RichText::new(&name).strong().size(16.0))
                            .truncate()
                            .sense(Sense::click()),
                    );
                    if title.on_hover_text(format!("{pretty}\nClick to copy path")).clicked() {
                        self.actions
                            .push(Action::CopyText(path.to_string_lossy().into_owned(), "project path"));
                    }
                    if ui.available_width() > 80.0 {
                        ui.add(
                            egui::Label::new(RichText::new(pretty).size(scale::FONT_SM).color(palette.faint))
                                .truncate(),
                        );
                    }
                });
            });
        });
        if self.settings.show_agents {
            ui.add_space(6.0);
            self.agent_strip(ui, palette);
        }
    }

    fn pane_ui(&mut self, ui: &mut egui::Ui, space_index: usize, uid: u64, rect: Rect, palette: &Palette) {
        let Some(pane_index) = self.spaces[space_index].index_of(uid) else {
            return;
        };
        let is_active = self.spaces[space_index].active == pane_index;
        let multi = self.spaces[space_index].panes.len() > 1;
        let zoomed = self.spaces[space_index].zoomed;
        let show_icons = self.settings.show_icons;
        let header_h = 30.0;
        let painter = ui.painter();
        let pane_ref = &self.spaces[space_index].panes[pane_index];
        let accent = pane_ref
            .client()
            .map(|kind| kind_color(kind, palette))
            .unwrap_or(palette.muted);
        let border = if pane_ref.attention {
            palette.amber
        } else if is_active && multi {
            palette.accent.gamma_multiply(0.8)
        } else {
            palette.border
        };
        painter.rect(
            rect,
            CornerRadius::same(10),
            if pane_ref.is_empty() {
                palette.panel
            } else {
                palette.term.bg
            },
            Stroke::new(if is_active && multi { 1.5 } else { 1.0 }, border),
            egui::StrokeKind::Inside,
        );
        let header = Rect::from_min_size(rect.min, Vec2::new(rect.width(), header_h));
        painter.line_segment(
            [
                header.left_bottom() + Vec2::new(1.0, 0.0),
                header.right_bottom() - Vec2::new(1.0, 0.0),
            ],
            Stroke::new(1.0, palette.border),
        );
        let body = Rect::from_min_max(
            header.left_bottom() + Vec2::new(1.0, 1.0),
            rect.max - Vec2::new(1.0, 1.0),
        );

        let mut header_ui = ui.new_child(
            UiBuilder::new()
                .id_salt(("pane-header", uid))
                .max_rect(header)
                .layout(Layout::left_to_right(Align::Center))
                .sense(Sense::click()),
        );
        header_ui.expand_to_include_rect(header);
        {
            let pane = &self.spaces[space_index].panes[pane_index];
            let mut actions = Vec::new();
            let mut right_ui = header_ui.new_child(
                UiBuilder::new()
                    .max_rect(header)
                    .layout(Layout::right_to_left(Align::Center)),
            );
            {
                let ui = &mut right_ui;
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.add_space(6.0);
                if close_button(ui, palette)
                    .on_hover_text(format!("Close pane  {}", self.key_hint(Cmd::ClosePane)))
                    .clicked()
                {
                    actions.push(Action::ClosePane(uid));
                }
                if multi
                    && tiny_icon_button(
                        ui,
                        if zoomed { UiIcon::Compress } else { UiIcon::Expand },
                        show_icons,
                        palette,
                    )
                    .on_hover_text(format!("Zoom  {}", self.key_hint(Cmd::ZoomPane)))
                    .clicked()
                {
                    actions.push(Action::FocusPane(uid));
                    actions.push(Action::ToggleZoom);
                }
                if pane.spec.is_some()
                    && tiny_icon_button(ui, UiIcon::Refresh, show_icons, palette)
                        .on_hover_text(format!(
                            "{}  {}",
                            if pane.is_running() {
                                "Restart (kills the running program)"
                            } else {
                                "Run again"
                            },
                            self.key_hint(Cmd::RestartPane)
                        ))
                        .clicked()
                {
                    actions.push(Action::RestartPane(uid));
                }
            }
            let title_area = Rect::from_min_max(
                header.min + Vec2::new(10.0, 0.0),
                Pos2::new(
                    (right_ui.min_rect().left() - 8.0).max(header.left() + 10.0),
                    header.bottom(),
                ),
            );
            let mut left_ui = header_ui.new_child(
                UiBuilder::new()
                    .max_rect(title_area)
                    .layout(Layout::left_to_right(Align::Center)),
            );
            left_ui.set_clip_rect(title_area.intersect(ui.clip_rect()));
            let ui = &mut left_ui;
            ui.spacing_mut().item_spacing.x = 6.0;
            if pane.is_busy() && !self.settings.reduce_motion {
                ui.add(egui::Spinner::new().size(11.0).color(accent));
            } else {
                let (dot, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                ui.painter()
                    .circle_filled(dot.center(), 4.0, if pane.is_empty() { palette.faint } else { accent });
            }
            ui.add(egui::Label::new(RichText::new(pane.title()).strong().size(12.5).color(palette.text)).truncate());
            if let Some(exit) = &pane.exit {
                let ok = exit.code == 0 && exit.signal.is_none();
                ui.label(pill(&exit.describe(), if ok { palette.muted } else { palette.red }));
            } else if pane.attention {
                ui.label(pill("▲ needs you", palette.amber));
            } else if pane.is_busy() {
                ui.label(RichText::new("● working").size(11.0).color(accent));
            }
            let cwd = pane.cwd();
            let detail = pane
                .osc_title
                .as_ref()
                .map(|title| title.trim().to_string())
                .filter(|title| !title.is_empty())
                .or_else(|| {
                    cwd.as_ref()
                        .and_then(|cwd| cwd.file_name())
                        .map(|name| name.to_string_lossy().into_owned())
                });
            if let Some(detail) = detail
                && ui.available_width() > 30.0
            {
                let label = ui
                    .add(egui::Label::new(RichText::new(detail).size(scale::FONT_SM).color(palette.muted)).truncate());
                if let Some(cwd) = cwd {
                    label.on_hover_text(cwd.display().to_string());
                }
            }
            self.actions.extend(actions);
        }
        let header_click = header_ui.response();
        if header_click.clicked() {
            self.actions.push(Action::FocusPane(uid));
        }
        if header_click.double_clicked() && multi {
            self.actions.push(Action::ToggleZoom);
        }

        if self.spaces[space_index].panes[pane_index].is_empty() {
            self.start_screen(ui, space_index, uid, body, palette);
        } else {
            self.terminal_ui(ui, space_index, pane_index, body, palette);
        }
    }

    fn start_screen(&mut self, ui: &mut egui::Ui, space_index: usize, uid: u64, body: Rect, palette: &Palette) {
        let path = self.spaces[space_index].path.clone();
        let name = self.spaces[space_index].name.clone();
        self.sessions.ensure(&path, Duration::from_secs(60));
        let start_bg = ui.interact(body, Id::new(("start-bg", uid)), Sense::click());
        let mut child = ui.new_child(
            UiBuilder::new()
                .max_rect(body.shrink(16.0))
                .layout(Layout::top_down(Align::Min)),
        );
        let target = Target::Pane(uid);
        let mut actions = Vec::new();
        let installed_clis: Vec<CliKind> = CliKind::ALL
            .iter()
            .filter(|kind| self.cli[kind.index()].installed)
            .copied()
            .collect();
        let show_icons = self.settings.show_icons;
        egui::ScrollArea::vertical()
            .id_salt(("start", uid))
            .auto_shrink([false, false])
            .show(&mut child, |ui| {
                ui.label(RichText::new(format!("Start in {name}")).size(scale::FONT_XL).strong());
                ui.label(RichText::new(pretty_path(&path, &self.home)).size(scale::FONT_SM).color(palette.faint));
                ui.add_space(10.0);
                let width = (ui.available_width() - 12.0).max(120.0);
                ui.set_max_width(width);
                if installed_clis.is_empty() {
                    egui::Frame::NONE
                        .fill(palette.raised)
                        .stroke(Stroke::new(1.0, palette.amber.gamma_multiply(0.5)))
                        .corner_radius(CornerRadius::same(9))
                        .inner_margin(Margin::same(10))
                        .show(ui, |ui| {
                            ui.set_width(width - 20.0);
                            self.no_cli_banner(ui, palette);
                        });
                    ui.add_space(8.0);
                }
                let gap = 8.0;
                let card_h = 46.0;
                let columns = ((width / 260.0).floor() as usize).clamp(1, 3);
                let card_w = (width - (columns as f32 - 1.0) * gap) / columns as f32;
                let mut kinds: Vec<Option<CliKind>> = installed_clis.iter().copied().map(Some).collect();
                kinds.push(None);
                for row in kinds.chunks(columns) {
                    let (row_rect, _) = ui.allocate_exact_size(Vec2::new(width, card_h), Sense::hover());
                    for (column, kind) in row.iter().enumerate() {
                        let rect = Rect::from_min_size(
                            row_rect.min + Vec2::new(column as f32 * (card_w + gap), 0.0),
                            Vec2::new(card_w, card_h),
                        );
                        ui.painter().rect(
                            rect,
                            CornerRadius::same(9),
                            palette.raised,
                            Stroke::new(1.0, palette.border),
                            egui::StrokeKind::Inside,
                        );
                        let inner = rect.shrink2(Vec2::new(12.0, 0.0));
                        let mut card = ui.new_child(
                            UiBuilder::new()
                                .id_salt(("start-card", uid, column, kind.map(|k| k.index())))
                                .max_rect(inner)
                                .layout(Layout::left_to_right(Align::Center)),
                        );
                        card.set_clip_rect(inner.intersect(ui.clip_rect()));
                        card.spacing_mut().item_spacing.x = 6.0;
                        match kind {
                            Some(kind) => {
                                let color = kind_color(*kind, palette);
                                card.label(RichText::new("●").color(color).size(10.0));
                                card.label(RichText::new(kind.label()).strong().color(color));
                                if card_w >= 340.0 {
                                    card.label(RichText::new(self.cli_program(*kind)).size(scale::FONT_XS).color(palette.faint));
                                }
                                card.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if kind.continue_args().is_some()
                                        && ui
                                            .add(small_button("Continue", palette))
                                            .on_hover_text("Continue the most recent conversation in this folder")
                                            .clicked()
                                    {
                                        actions.push(Action::Launch(*kind, LaunchMode::Continue, target));
                                    }
                                    if ui.add(small_button("New", palette)).clicked() {
                                        actions.push(Action::Launch(*kind, LaunchMode::New, target));
                                    }
                                });
                            }
                            None => {
                                if show_icons {
                                    let (r, _) = card.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                                    UiIcon::Terminal.paint(card.painter(), r, palette.text);
                                }
                                card.label(RichText::new("Terminal").strong());
                                if card_w >= 300.0 {
                                    card.label(RichText::new("$SHELL").size(scale::FONT_XS).color(palette.faint));
                                }
                                card.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if ui.add(small_button("Open shell", palette)).clicked() {
                                        actions.push(Action::Shell(target));
                                    }
                                });
                            }
                        }
                    }
                    ui.add_space(gap - ui.spacing().item_spacing.y);
                }
                ui.add_space(8.0);
                let sidebar_lists_sessions = self.settings.show_sidebar && self.settings.sidebar_sessions;
                let show_recent = match self.settings.start_sessions {
                    crate::theme::StartSessions::Always => true,
                    crate::theme::StartSessions::Never => false,
                    crate::theme::StartSessions::Auto => !sidebar_lists_sessions,
                };
                if show_recent {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Recent conversations").strong());
                        if self.sessions.get(&path).is_some_and(|list| list.loading) {
                            ui.add(egui::Spinner::new().size(11.0));
                        }
                    });
                    ui.add_space(4.0);
                    match self.sessions.get(&path) {
                        Some(list) if !list.sessions.is_empty() => {
                            for session in list.sessions.iter().take(8) {
                                let response = session_row_detailed(ui, session, palette)
                                    .on_hover_text("Resume in this pane");
                                if response.clicked() {
                                    actions.push(Action::Resume(Box::new(session.clone()), target));
                                }
                            }
                            if list.sessions.len() > 8
                                && ui
                                    .add(
                                        egui::Button::new(
                                            RichText::new(format!("All {} conversations →", list.sessions.len()))
                                                .size(12.0)
                                                .color(palette.accent),
                                        )
                                        .frame(false),
                                    )
                                    .clicked()
                            {
                                actions.push(Action::ShowSessions(true));
                            }
                        }
                        Some(list) if list.error.is_some() => {
                            ui.label(
                                RichText::new(list.error.clone().unwrap_or_default())
                                    .size(scale::FONT_SM)
                                    .color(palette.muted),
                            );
                        }
                        Some(list) if !list.loading => {
                            ui.label(
                                RichText::new("No saved conversations here yet.")
                                    .size(12.0)
                                    .color(palette.muted),
                            );
                        }
                        _ => {}
                    }
                }
                ui.add_space(14.0);
                ui.label(
                    RichText::new(format!(
                        "Tips: {} finds anything · {} searches scrollback · drop files on a pane to insert paths · ⇧↵ / ⌥↵ newline",
                        self.key_hint(Cmd::Search),
                        self.key_hint(Cmd::Find)
                    ))
                    .size(11.0)
                    .color(palette.faint),
                );
            });
        if start_bg.clicked() {
            actions.push(Action::FocusPane(uid));
        }
        self.actions.extend(actions);
    }

    fn terminal_ui(
        &mut self,
        ui: &mut egui::Ui,
        space_index: usize,
        pane_index: usize,
        mut body: Rect,
        palette: &Palette,
    ) {
        let uid = self.spaces[space_index].panes[pane_index].uid;
        let search_open = self.pane_searches.get(&uid).is_some_and(|s| s.open);
        let mut search_closed = false;
        let mut search_jump: Option<i32> = None;
        if search_open {
            let search_rect = Rect::from_min_size(body.min, Vec2::new(body.width(), 32.0));
            body = Rect::from_min_max(Pos2::new(body.left(), body.top() + 32.0), body.max);
            let mut search_ui = ui.new_child(
                UiBuilder::new()
                    .id_salt(("pane-search-bar", uid))
                    .max_rect(search_rect)
                    .layout(Layout::left_to_right(Align::Center)),
            );
            let entry = self.pane_searches.entry(uid).or_default();
            egui::Frame::NONE
                .fill(palette.panel)
                .stroke(Stroke::new(1.0, palette.border))
                .inner_margin(Margin::symmetric(8, 2))
                .show(&mut search_ui, |ui| {
                    ui.set_width(ui.available_width());
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut entry.query)
                            .hint_text(
                                RichText::new("Find in scrollback…")
                                    .size(scale::FONT_SM)
                                    .color(palette.faint),
                            )
                            .desired_width(220.0)
                            .return_key(None),
                    );
                    if entry.focus_input {
                        edit.request_focus();
                        entry.focus_input = false;
                    }
                    if edit.changed() {
                        entry.matches = self.spaces[space_index].panes[pane_index].scrollback_search(&entry.query);
                        entry.current = entry.matches.len().saturating_sub(1);
                        if let Some(m) = entry.matches.get(entry.current) {
                            search_jump = Some(m.line);
                        }
                    }
                    let count_text = if entry.query.is_empty() {
                        String::new()
                    } else if entry.matches.is_empty() {
                        "0 matches".to_string()
                    } else {
                        format!("{}/{}", entry.current + 1, entry.matches.len())
                    };
                    if !count_text.is_empty() {
                        ui.label(RichText::new(count_text).size(scale::FONT_SM).color(palette.muted));
                    }
                    let enter = edit.has_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    let shift_held = ui.input(|i| i.modifiers.shift);
                    let prev = ui.add(small_button("↑", palette)).clicked() || (enter && shift_held);
                    let next = ui.add(small_button("↓", palette)).clicked() || (enter && !shift_held);
                    if !entry.matches.is_empty() {
                        if prev {
                            entry.current = (entry.current + entry.matches.len() - 1) % entry.matches.len();
                            search_jump = Some(entry.matches[entry.current].line);
                        } else if next {
                            entry.current = (entry.current + 1) % entry.matches.len();
                            search_jump = Some(entry.matches[entry.current].line);
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if close_button(ui, palette).clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                            search_closed = true;
                        }
                    });
                });
        }
        if let Some(line) = search_jump {
            self.spaces[space_index].panes[pane_index].scroll_to_line(line);
        }
        if search_closed {
            if let Some(s) = self.pane_searches.get_mut(&uid) {
                s.open = false;
                s.matches.clear();
            }
            self.pending_focus = Some(uid);
        }

        let metrics = view::metrics(ui.ctx(), self.settings.font_size);
        let scrollbar_w = 8.0;
        let inner = Rect::from_min_max(
            body.min + Vec2::new(8.0, 4.0),
            body.max - Vec2::new(4.0 + scrollbar_w, 4.0),
        );
        let option_as_meta = self.settings.option_as_meta;
        let copy_on_select = self.settings.copy_on_select;
        let window_focused = self.window_focused;
        let cursor_on = !self.settings.cursor_blink || (ui.input(|i| i.time) * 1.6).fract() < 0.6;
        if self.settings.cursor_blink {
            ui.ctx().request_repaint_after(Duration::from_millis(300));
        }
        let pending_focus = self.pending_focus;
        let mut actions = Vec::new();
        let mut pending_paste: Option<PendingPaste> = None;
        let pane = &mut self.spaces[space_index].panes[pane_index];
        let (rows, cols) = metrics.grid_size(inner);
        pane.resize(rows, cols, metrics.cell.x, metrics.cell.y);

        let id = Id::new(("terminal", uid));
        let response = ui.interact(inner, id, Sense::click_and_drag());
        if pending_focus == Some(uid) {
            response.request_focus();
            self.pending_focus = None;
        }
        let focused = response.has_focus();
        if focused {
            ui.memory_mut(|memory| {
                memory.set_focus_lock_filter(
                    id,
                    egui::EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                )
            });
            if let Some((crow, ccol)) = pane.cursor_viewport_cell() {
                let cx = inner.left() + ccol as f32 * metrics.cell.x;
                let cy = inner.top() + crow as f32 * metrics.cell.y;
                let cursor_rect = Rect::from_min_size(Pos2::new(cx, cy), Vec2::new(2.0, metrics.cell.y));
                ui.ctx().output_mut(|o| {
                    o.ime = Some(egui::output::IMEOutput {
                        rect: inner,
                        cursor_rect,
                    });
                });
            }
        }
        pane.report_focus(focused && window_focused);
        let mode = pane.mode();
        let shift = ui.input(|i| i.modifiers.shift);
        let command = ui.input(|i| i.modifiers.command);
        let mouse_mode = mode.intersects(TermMode::MOUSE_MODE) && !shift;
        let pointer = response.interact_pointer_pos().or(response.hover_pos());
        let cell_at = |pos: Pos2| metrics.cell_at(inner, pos, cols, rows);

        let hover_link = pointer.filter(|_| response.hovered()).and_then(|pos| {
            let (row, col, _) = cell_at(pos);
            pane.hyperlink_at(row, col)
        });
        if command && hover_link.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }

        if response.clicked() || response.drag_started() || response.secondary_clicked() {
            response.request_focus();
            actions.push(Action::FocusPane(uid));
        }

        if command && response.clicked() && hover_link.is_some() {
            if let Some(url) = hover_link.clone() {
                actions.push(Action::OpenUrl(url));
            }
        } else if mouse_mode {
            let state = self.mouse.entry(uid).or_default();
            let mods = ui.input(|i| i.modifiers);
            let button_pairs = [
                (0usize, egui::PointerButton::Primary, MouseButton::Left),
                (1usize, egui::PointerButton::Middle, MouseButton::Middle),
                (2usize, egui::PointerButton::Secondary, MouseButton::Right),
            ];
            for (idx, egui_btn, mouse_btn) in button_pairs {
                let (pressed, released) =
                    ui.input(|i| (i.pointer.button_pressed(egui_btn), i.pointer.button_released(egui_btn)));
                if pressed
                    && response.hovered()
                    && let Some(pos) = pointer
                {
                    let (row, col, _) = cell_at(pos);
                    if let Some(bytes) = input::encode_mouse(mouse_btn, true, false, col, row, mods, mode) {
                        pane.write_raw(&bytes);
                    }
                    state.pressed_buttons[idx] = Some((row, col));
                } else if ui.input(|i| i.pointer.button_down(egui_btn))
                    && mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION)
                    && let Some(pos) = pointer
                {
                    let (row, col, _) = cell_at(pos);
                    if state.pressed_buttons[idx] != Some((row, col)) {
                        if let Some(bytes) = input::encode_mouse(mouse_btn, true, true, col, row, mods, mode) {
                            pane.write_raw(&bytes);
                        }
                        state.pressed_buttons[idx] = Some((row, col));
                    }
                }
                if released && state.pressed_buttons[idx].is_some() {
                    if let Some(pos) = pointer {
                        let (row, col, _) = cell_at(pos);
                        if let Some(bytes) = input::encode_mouse(mouse_btn, false, false, col, row, mods, mode) {
                            pane.write_raw(&bytes);
                        }
                    }
                    state.pressed_buttons[idx] = None;
                }
            }
            if mode.contains(TermMode::MOUSE_MOTION)
                && state.pressed_buttons.iter().all(Option::is_none)
                && response.hovered()
                && let Some(pos) = pointer
            {
                let (row, col, _) = cell_at(pos);
                if state.last_hover_cell != Some((row, col)) {
                    if let Some(bytes) = input::encode_mouse(MouseButton::None, true, true, col, row, mods, mode) {
                        pane.write_raw(&bytes);
                    }
                    state.last_hover_cell = Some((row, col));
                }
            }
        } else {
            if response.clicked() {
                pane.clear_selection();
            }
            if response.drag_started_by(egui::PointerButton::Primary) {
                let origin = ui.input(|i| i.pointer.press_origin()).or(pointer);
                if let Some(pos) = origin {
                    let (row, col, right) = cell_at(pos);
                    pane.begin_selection(row, col, right, 1);
                }
            }
            if response.dragged_by(egui::PointerButton::Primary)
                && let Some(pos) = pointer
            {
                let (row, col, right) = cell_at(pos);
                pane.update_selection(row, col, right);
                if pos.y < inner.top() {
                    pane.scroll_lines(1);
                } else if pos.y > inner.bottom() {
                    pane.scroll_lines(-1);
                }
            }
            if response.drag_stopped()
                && copy_on_select
                && let Some(text) = pane.selected_text()
            {
                ui.ctx().copy_text(text);
            }
            if (response.double_clicked() || response.triple_clicked())
                && let Some(pos) = pointer
            {
                let (row, col, right) = cell_at(pos);
                pane.begin_selection(row, col, right, if response.triple_clicked() { 3 } else { 2 });
                pane.update_selection(row, col, right);
                if copy_on_select && let Some(text) = pane.selected_text() {
                    ui.ctx().copy_text(text);
                }
            }
        }

        if response.hovered() {
            let delta = ui.input(|i| i.raw_scroll_delta.y);
            if delta.abs() > 0.0 {
                let lines = pane.accumulate_scroll(delta / metrics.cell.y);
                if lines != 0 {
                    let up = lines > 0;
                    let count = lines.unsigned_abs().min(12) as usize;
                    if mouse_mode {
                        if let Some(pos) = pointer {
                            let (row, col, _) = cell_at(pos);
                            let button = if up {
                                MouseButton::WheelUp
                            } else {
                                MouseButton::WheelDown
                            };
                            for _ in 0..count {
                                if let Some(bytes) =
                                    input::encode_mouse(button, true, false, col, row, Modifiers::NONE, mode)
                                {
                                    pane.write_raw(&bytes);
                                }
                            }
                        }
                    } else if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
                        let key: &[u8] = match (up, mode.contains(TermMode::APP_CURSOR)) {
                            (true, true) => b"\x1bOA",
                            (true, false) => b"\x1b[A",
                            (false, true) => b"\x1bOB",
                            (false, false) => b"\x1b[B",
                        };
                        for _ in 0..count {
                            pane.write_raw(key);
                        }
                    } else {
                        pane.scroll_lines(lines);
                    }
                }
            }
        }

        if focused {
            let events = ui.input(|i| i.events.clone());
            let alt_held = ui.input(|i| i.modifiers.alt);
            for event in events {
                match event {
                    egui::Event::Text(text) => {
                        if !(option_as_meta && alt_held) {
                            pane.write(text.as_bytes());
                        }
                    }
                    egui::Event::Ime(ImeEvent::Commit(text)) => {
                        if !text.is_empty() {
                            pane.write(text.as_bytes());
                        }
                    }
                    egui::Event::Paste(text) => {
                        let trimmed = text.trim_end_matches(['\r', '\n']);
                        if !mode.contains(TermMode::BRACKETED_PASTE)
                            && (trimmed.contains('\n') || trimmed.contains('\r'))
                        {
                            let lines = trimmed.lines().count().max(2);
                            pending_paste = Some(PendingPaste {
                                pane_uid: uid,
                                text,
                                lines,
                            });
                        } else {
                            pane.paste(&text);
                        }
                    }
                    egui::Event::Copy | egui::Event::Cut => {
                        if let Some(text) = pane.selected_text() {
                            ui.ctx().copy_text(text);
                        }
                    }
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => {
                        if modifiers.ctrl && modifiers.shift && key == Key::C {
                            if let Some(text) = pane.selected_text() {
                                ui.ctx().copy_text(text);
                            }
                            continue;
                        }
                        if modifiers.shift && !modifiers.ctrl && !modifiers.alt && !mode.contains(TermMode::ALT_SCREEN)
                        {
                            match key {
                                Key::PageUp => {
                                    pane.scroll_page(true);
                                    continue;
                                }
                                Key::PageDown => {
                                    pane.scroll_page(false);
                                    continue;
                                }
                                _ => {}
                            }
                        }
                        if let Some(bytes) = input::encode_key(key, modifiers, mode, option_as_meta) {
                            pane.write(&bytes);
                        }
                    }
                    _ => {}
                }
            }
        }

        let dropped = ui.input(|i| i.raw.dropped_files.clone());
        if !dropped.is_empty()
            && ui
                .input(|i| i.pointer.hover_pos())
                .is_some_and(|pos| body.contains(pos))
        {
            let paths: Vec<String> = dropped
                .iter()
                .filter_map(|file| file.path.as_ref())
                .filter(|path| !path.is_dir())
                .map(|path| input::shell_quote(&path.to_string_lossy()))
                .collect();
            if !paths.is_empty() {
                pane.paste(&format!("{} ", paths.join(" ")));
                response.request_focus();
            }
        }
        let hovering_files = !ui.input(|i| i.raw.hovered_files.is_empty())
            && ui
                .input(|i| i.pointer.hover_pos())
                .is_some_and(|pos| body.contains(pos));

        let search_state = self.pane_searches.get(&uid);
        let search_matches: &[crate::terminal::SearchMatch] = search_state
            .filter(|s| s.open)
            .map(|s| s.matches.as_slice())
            .unwrap_or(&[]);
        let current_match = search_state.filter(|s| s.open).map(|s| s.current);

        view::draw(
            ui,
            inner,
            pane,
            &palette.term,
            &metrics,
            DrawOptions {
                focused: focused && window_focused,
                cursor_on,
                accent: palette.accent,
                hover_link: hover_link.as_deref().filter(|_| command),
                search_matches,
                current_match,
            },
        );
        if hovering_files {
            ui.painter().rect(
                body.shrink(4.0),
                CornerRadius::same(8),
                palette.accent.gamma_multiply(0.12),
                Stroke::new(1.5, palette.accent),
                egui::StrokeKind::Inside,
            );
        }
        let (offset, history, screen) = pane.scroll_state();
        if history > 0 {
            let track = Rect::from_min_max(
                Pos2::new(body.right() - scrollbar_w - 2.0, body.top() + 4.0),
                Pos2::new(body.right() - 3.0, body.bottom() - 4.0),
            );
            let bar = ui.interact(track, Id::new(("scrollbar", uid)), Sense::click_and_drag());
            let total = (history + screen) as f32;
            let thumb_h = (screen as f32 / total * track.height()).max(28.0);
            let travel = (track.height() - thumb_h).max(1.0);
            if (bar.dragged() || bar.clicked())
                && let Some(pos) = bar.interact_pointer_pos()
            {
                let fraction = ((track.bottom() - thumb_h / 2.0 - pos.y) / travel).clamp(0.0, 1.0);
                pane.scroll_to_offset((fraction * history as f32).round() as usize);
            }
            let (offset, _, _) = pane.scroll_state();
            let progress = offset as f32 / history as f32;
            let thumb = Rect::from_min_size(
                Pos2::new(track.left(), track.bottom() - thumb_h - progress * travel),
                Vec2::new(track.width(), thumb_h),
            );
            let alpha = if bar.hovered() || bar.dragged() {
                0.9
            } else if offset > 0 {
                0.6
            } else {
                0.25
            };
            ui.painter()
                .rect_filled(thumb, CornerRadius::same(4), palette.muted.gamma_multiply(alpha));
        }
        if offset > 0 {
            let label = format!("↓  {offset} lines below");
            let text = RichText::new(&label).size(scale::FONT_SM).color(palette.accent_text);
            let text_w = ui.fonts(|f| {
                f.layout_no_wrap(
                    label.clone(),
                    egui::FontId::proportional(scale::FONT_SM),
                    palette.accent_text,
                )
                .size()
                .x
            });
            let btn_w = (text_w + 24.0).clamp(110.0, (inner.width() - 16.0).max(80.0));
            let size = Vec2::new(btn_w, 24.0);
            let rect = Rect::from_min_size(
                Pos2::new(inner.right() - size.x - 10.0, inner.bottom() - size.y - 8.0),
                size,
            );
            let jump = ui.put(
                rect,
                egui::Button::new(text)
                    .fill(palette.accent)
                    .corner_radius(CornerRadius::same(12)),
            );
            if jump.clicked() {
                pane.scroll_to_bottom();
                actions.push(Action::FocusPane(uid));
            }
        }

        let has_selection = pane.has_selection();
        let running = pane.is_running();
        let has_spec = pane.spec.is_some();
        response.context_menu(|ui| {
            if ui.add_enabled(has_selection, egui::Button::new("Copy")).clicked() {
                let text = self.spaces[space_index]
                    .panes
                    .get(pane_index)
                    .and_then(TerminalPane::selected_text);
                if let Some(text) = text {
                    actions.push(Action::CopyText(text, "selection"));
                }
                ui.close_menu();
            }
            if ui.button("Select all").clicked() {
                if let Some(pane) = self.spaces[space_index].panes.get_mut(pane_index) {
                    pane.select_all();
                }
                ui.close_menu();
            }
            if ui
                .button(format!("Find in scrollback  {}", self.key_hint(Cmd::Find)))
                .clicked()
            {
                actions.push(Action::FocusPane(uid));
                actions.push(Action::OpenFind);
                ui.close_menu();
            }
            if ui
                .button(format!("Clear scrollback  {}", self.key_hint(Cmd::ClearScrollback)))
                .clicked()
            {
                actions.push(Action::FocusPane(uid));
                actions.push(Action::ClearScrollback);
                ui.close_menu();
            }
            ui.separator();
            if ui
                .button(format!("Split right  {}", self.key_hint(Cmd::SplitRight)))
                .clicked()
            {
                actions.push(Action::Split(true));
                ui.close_menu();
            }
            if ui
                .button(format!("Split down  {}", self.key_hint(Cmd::SplitDown)))
                .clicked()
            {
                actions.push(Action::Split(false));
                ui.close_menu();
            }
            if has_spec && ui.button(if running { "Restart" } else { "Run again" }).clicked() {
                actions.push(Action::RestartPane(uid));
                ui.close_menu();
            }
            if ui
                .button(format!("Close pane  {}", self.key_hint(Cmd::ClosePane)))
                .clicked()
            {
                actions.push(Action::ClosePane(uid));
                ui.close_menu();
            }
        });
        if pending_paste.is_some() {
            self.pending_paste = pending_paste;
        }
        self.actions.extend(actions);
    }

    fn agent_strip(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        let installed: Vec<CliKind> = CliKind::ALL
            .into_iter()
            .filter(|kind| self.cli[kind.index()].installed)
            .collect();
        if installed.is_empty() {
            egui::Frame::NONE
                .fill(palette.raised)
                .stroke(Stroke::new(1.0, palette.border))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    self.no_cli_banner(ui, palette);
                });
            return;
        }
        let gap = 6.0;
        let available = ui.available_width();
        let count = installed.len().max(1);
        let fit = (((available + gap) / (150.0 + gap)).floor() as usize).clamp(1, count);
        let rows = count.div_ceil(fit);
        let columns = count.div_ceil(rows);
        let card_w = ((available - gap * (columns as f32 - 1.0)) / columns as f32 - 0.5)
            .floor()
            .min(300.0);
        let layout = Layout::left_to_right(Align::Min).with_main_wrap(true);
        ui.with_layout(layout, |ui| {
            ui.spacing_mut().item_spacing = Vec2::new(gap, gap);
            for kind in installed {
                let instances = self.agent_instances(kind);
                let top = instances.first();
                let color = kind_color(kind, palette);
                let (rect, response) = ui.allocate_exact_size(Vec2::new(card_w, 58.0), Sense::click());
                let state_color = top.map(|instance| instance.state.color(palette));
                let border = match top.map(|instance| instance.state) {
                    Some(crate::app::AgentState::NeedsYou) => palette.amber,
                    Some(crate::app::AgentState::Error) => palette.red.gamma_multiply(0.8),
                    Some(crate::app::AgentState::Working) => color.gamma_multiply(0.8),
                    _ if response.hovered() => palette.border_strong,
                    _ => palette.border,
                };
                let painter = ui.painter_at(rect);
                painter.rect(
                    rect,
                    CornerRadius::same(9),
                    if response.hovered() {
                        palette.hover
                    } else {
                        palette.raised
                    },
                    Stroke::new(1.0, border),
                    egui::StrokeKind::Inside,
                );
                painter.rect_filled(
                    Rect::from_min_size(rect.min + Vec2::new(0.0, 8.0), Vec2::new(3.0, rect.height() - 16.0)),
                    CornerRadius::same(2),
                    color,
                );
                let x = rect.left() + 12.0;
                let font = |size: f32| egui::FontId::proportional(size);
                painter.text(
                    Pos2::new(x, rect.top() + 7.0),
                    Align2::LEFT_TOP,
                    kind.label(),
                    egui::FontId::new(13.5, egui::FontFamily::Proportional),
                    color,
                );
                if instances.len() > 1 {
                    painter.text(
                        Pos2::new(rect.right() - 10.0, rect.top() + 8.0),
                        Align2::RIGHT_TOP,
                        format!("×{}", instances.len()),
                        font(11.0),
                        palette.muted,
                    );
                }
                let status_y = rect.top() + 26.0;
                let (status, status_color) = match top {
                    Some(instance) => {
                        let mut text = format!(
                            "{} {} · {}",
                            instance.state.symbol(),
                            instance.state.label(),
                            instance.project
                        );
                        if !instance.detail.is_empty() {
                            text.push_str(&format!(" · {}", instance.detail));
                        }
                        (text, state_color.unwrap_or(palette.muted))
                    }
                    None => match &self.cli[kind.index()].snapshot {
                        Some(snapshot) => (
                            match snapshot.updated {
                                Some(updated) => {
                                    format!("idle · {} {}", snapshot.status, crate::widgets::age_system(updated))
                                }
                                None => format!("idle · {}", snapshot.status),
                            },
                            palette.faint,
                        ),
                        None => ("idle · double-click to start".into(), palette.faint),
                    },
                };
                let dot = Pos2::new(x + 4.0, status_y + 7.0);
                if top.is_some_and(|instance| instance.state == crate::app::AgentState::Working)
                    && !self.settings.reduce_motion
                {
                    let t = ui.input(|i| i.time) as f32;
                    painter.circle_filled(dot, 2.5 + (t * 5.0).sin().abs() * 1.5, status_color);
                } else {
                    painter.circle_filled(dot, 3.5, status_color);
                }
                let mut job = egui::text::LayoutJob::simple_singleline(status, font(scale::FONT_SM), status_color);
                job.wrap = egui::text::TextWrapping::truncate_at_width(card_w - 34.0);
                let galley = ui.fonts(|fonts| fonts.layout_job(job));
                let clipped = Rect::from_min_size(Pos2::new(x + 12.0, status_y), Vec2::new(card_w - 26.0, 15.0));
                ui.painter_at(clipped).galley(clipped.min, galley, status_color);

                if let Some(headline) = self.usage.headline(kind) {
                    let bar_y = rect.bottom() - 13.0;
                    let half = (card_w - 24.0) / 2.0;
                    let bars: Vec<_> = [headline.short, headline.week].into_iter().flatten().collect();
                    for (index, limit) in bars.into_iter().enumerate() {
                        let left = x + index as f32 * (half + 4.0);
                        mini_bar(
                            &painter,
                            Rect::from_min_size(Pos2::new(left, bar_y), Vec2::new(half - 2.0, 9.0)),
                            limit,
                            palette,
                        );
                    }
                }
                let tip_instances: Vec<String> = instances
                    .iter()
                    .map(|instance| {
                        format!(
                            "{} {} — {}{}",
                            instance.state.symbol(),
                            instance.project,
                            instance.state.label(),
                            if instance.detail.is_empty() {
                                String::new()
                            } else {
                                format!(" · {}", instance.detail)
                            }
                        )
                    })
                    .collect();
                let usage_lines: Vec<String> = self
                    .usage
                    .limits_for(kind)
                    .iter()
                    .filter(|limit| limit.used.is_some())
                    .map(|limit| {
                        let used = limit.clamped_used().unwrap_or(0.0);
                        format!(
                            "{}{} {}: {:.0}% used · {}",
                            if limit.account_title.is_empty() {
                                String::new()
                            } else {
                                format!("[{}] ", limit.account_title)
                            },
                            limit.label,
                            limit.window,
                            used * 100.0,
                            limit.reset_text()
                        )
                    })
                    .collect();
                let last_saved = self.cli[kind.index()].snapshot.clone();
                let response = response.on_hover_ui(|ui| {
                    ui.set_max_width(380.0);
                    ui.label(RichText::new(kind.label()).strong().color(color));
                    if tip_instances.is_empty() {
                        ui.label(RichText::new("No running sessions").color(palette.muted));
                        if let Some(snapshot) = &last_saved {
                            let project = snapshot
                                .project
                                .file_name()
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            ui.label(
                                RichText::new(format!(
                                    "Last saved: {} · {project} · {}",
                                    snapshot.status,
                                    snapshot.updated.map(ago_system).unwrap_or_default()
                                ))
                                .size(scale::FONT_SM)
                                .color(palette.muted),
                            );
                            if let Some(title) = &snapshot.title {
                                ui.label(
                                    RichText::new(truncate(title, 90))
                                        .size(scale::FONT_SM)
                                        .color(palette.faint),
                                );
                            }
                        }
                    }
                    for line in &tip_instances {
                        ui.label(RichText::new(line).size(12.0));
                    }
                    if !usage_lines.is_empty() {
                        ui.separator();
                        for line in &usage_lines {
                            ui.label(RichText::new(line).size(scale::FONT_SM).color(palette.muted));
                        }
                    }
                    ui.separator();
                    ui.label(
                        RichText::new(if tip_instances.is_empty() {
                            "Double-click: new session · right-click: continue, split, usage"
                        } else {
                            "Click: jump to it (again to cycle) · right-click: more"
                        })
                        .size(11.0)
                        .color(palette.faint),
                    );
                });
                if !instances.is_empty() && response.clicked() {
                    self.actions.push(Action::FocusAgent(kind));
                } else if instances.is_empty() && response.double_clicked() {
                    self.actions.push(Action::Launch(kind, LaunchMode::New, Target::Auto));
                }
                let jump: Vec<(u64, String)> = instances
                    .iter()
                    .map(|instance| {
                        (
                            instance.uid,
                            format!("{} — {}", instance.project, instance.state.label()),
                        )
                    })
                    .collect();
                response.context_menu(|ui| {
                    ui.label(RichText::new(kind.label()).strong().color(color));
                    for (uid, text) in &jump {
                        if ui.button(format!("Go to {text}")).clicked() {
                            self.actions.push(Action::FocusPane(*uid));
                            ui.close_menu();
                        }
                    }
                    if !jump.is_empty() {
                        ui.separator();
                    }
                    if ui.button("New session").clicked() {
                        self.actions.push(Action::Launch(kind, LaunchMode::New, Target::Auto));
                        ui.close_menu();
                    }
                    if kind.continue_args().is_some() && ui.button("Continue last session").clicked() {
                        self.actions
                            .push(Action::Launch(kind, LaunchMode::Continue, Target::Auto));
                        ui.close_menu();
                    }
                    if ui.button("New session in split").clicked() {
                        self.actions.push(Action::Launch(kind, LaunchMode::New, Target::Split));
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Browse conversations").clicked() {
                        self.actions.push(Action::ShowSessions(true));
                        ui.close_menu();
                    }
                    if ui.button("Usage limits").clicked() {
                        self.actions.push(Action::SetRightTab(Some(RightTab::Usage)));
                        ui.close_menu();
                    }
                });
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn space_with(count: usize, stacked: bool) -> Space {
        let mut space = Space::new(PathBuf::from("/tmp"), None);
        for _ in 0..count {
            space.panes.push(TerminalPane::empty());
        }
        space.stacked = stacked;
        space
    }

    #[test]
    fn layouts_cover_area_without_overlap() {
        let area = Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 600.0));
        for count in 1..=4 {
            for stacked in [false, true] {
                let space = space_with(count, stacked);
                let (rects, _) = layout_panes(&space, area, 6.0);
                assert_eq!(rects.len(), count);
                for (i, a) in rects.iter().enumerate() {
                    assert!(area.contains_rect(*a), "{count} {stacked} {a:?}");
                    for b in rects.iter().skip(i + 1) {
                        assert!(!a.intersects(b.shrink(0.5)), "overlap {a:?} {b:?}");
                    }
                }
            }
        }
    }
}
