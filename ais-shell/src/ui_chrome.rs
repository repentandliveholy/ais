//! Title bar, space tabs, title search dropdown and status bar.

use crate::app::{Action, AisApp, RightTab, SEARCH_ID, Space, TITLE_HEIGHT, custom_titlebar};
use crate::cli::CliKind;
use crate::keys::{Command as Cmd, MOD};
use crate::sessions::truncate;
use crate::terminal::TerminalPane;
use crate::theme::{Palette, scale};
use crate::update::UpdateStatus;
use crate::usage::Limit;
use crate::widgets::{
    UiIcon, close_button, kind_color, paint_logo, plural, pretty_path, read_git_branch, small_button, theme_button,
    tool_button, tool_button_tinted, usage_badge, window_buttons,
};
use eframe::egui::{
    self, Align, Color32, CornerRadius, FontId, Id, Layout, Margin, Pos2, Rect, RichText, Sense, Stroke, Vec2,
};
use std::path::Path;
use std::time::{Duration, Instant};

impl AisApp {
    pub fn title_bar(&mut self, ctx: &egui::Context) {
        let palette = self.palette();
        let custom = custom_titlebar();
        let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        let center = self.settings.search_center;
        let show_icons = self.settings.show_icons;
        egui::TopBottomPanel::top("title")
            .exact_height(TITLE_HEIGHT)
            .frame(
                egui::Frame::NONE
                    .fill(palette.chrome)
                    .inner_margin(Margin::symmetric(12, 0))
                    .stroke(Stroke::new(1.0, if center { palette.chrome } else { palette.border })),
            )
            .show(ctx, |ui| {
                if custom {
                    let bar = ui.max_rect().expand2(Vec2::new(12.0, 0.0));
                    let drag = ui.interact(bar, Id::new("titlebar-drag"), Sense::click_and_drag());
                    if drag.drag_started() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    if drag.double_clicked() {
                        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                    }
                }
                ui.horizontal_centered(|ui| {
                    if custom && !fullscreen {
                        window_buttons(ui, &palette, self.window_focused);
                        ui.add_space(10.0);
                    }
                    let (mark, _) = ui.allocate_exact_size(Vec2::new(24.0, 24.0), Sense::hover());
                    paint_logo(ui.painter(), mark, &palette);
                    ui.label(RichText::new("ais").strong().size(16.0).color(palette.text));
                    ui.add_space(6.0);
                    if tool_button(
                        ui,
                        UiIcon::Sidebar,
                        "",
                        self.settings.show_sidebar,
                        show_icons,
                        &palette,
                    )
                    .on_hover_text(format!("Toggle sidebar  {}", self.key_hint(Cmd::ToggleSidebar)))
                    .clicked()
                    {
                        self.actions.push(Action::ToggleSidebar);
                    }
                    ui.add_space(4.0);
                    let compact = ui.available_width() < 760.0;
                    if center {
                        let left_end = ui.cursor().min.x;
                        let right_start = ui
                            .with_layout(Layout::right_to_left(Align::Center), |ui| {
                                self.title_controls(ui, &palette);
                                ui.min_rect().left()
                            })
                            .inner;
                        let screen_center = ctx.screen_rect().center().x;
                        let desired = (ctx.screen_rect().width() * 0.42).clamp(260.0, 640.0);
                        let half = (screen_center - left_end).min(right_start - screen_center) - 12.0;
                        let (x0, width) = if half * 2.0 >= 260.0 {
                            let width = desired.min(half * 2.0);
                            (screen_center - width / 2.0, width)
                        } else {
                            (left_end + 8.0, (right_start - left_end - 16.0).max(80.0))
                        };
                        let rect =
                            Rect::from_min_size(Pos2::new(x0, ui.max_rect().center().y - 13.0), Vec2::new(width, 26.0));
                        ui.scope_builder(
                            egui::UiBuilder::new()
                                .max_rect(rect)
                                .layout(Layout::left_to_right(Align::Center)),
                            |ui| {
                                self.search_field(ui, &palette, compact);
                            },
                        );
                    } else {
                        let right_width = if compact {
                            5.0 * 36.0 + 8.0
                        } else {
                            160.0 + 4.0 * 36.0 + 16.0
                        };
                        let tabs_width = (ui.available_width() - right_width).max(80.0);
                        ui.allocate_ui_with_layout(
                            Vec2::new(tabs_width, 32.0),
                            Layout::left_to_right(Align::Center),
                            |ui| {
                                egui::ScrollArea::horizontal()
                                    .auto_shrink([false, true])
                                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                                    .show(ui, |ui| {
                                        ui.horizontal_centered(|ui| self.space_tabs(ui, &palette));
                                    });
                            },
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            self.title_controls(ui, &palette);
                            let hint = self.key_hint(Cmd::Search);
                            let label = if compact {
                                hint
                            } else if hint.is_empty() {
                                "Search & run".to_string()
                            } else {
                                format!("Search & run  {hint}")
                            };
                            if tool_button(ui, UiIcon::Search, &label, false, show_icons, &palette)
                                .on_hover_text("Command palette: launch, resume, open projects")
                                .clicked()
                            {
                                self.actions.push(Action::OpenPalette);
                            }
                        });
                    }
                });
            });
        if center {
            egui::TopBottomPanel::top("space-tabs")
                .exact_height(34.0)
                .frame(
                    egui::Frame::NONE
                        .fill(palette.chrome)
                        .inner_margin(Margin::symmetric(10, 0))
                        .stroke(Stroke::new(1.0, palette.border)),
                )
                .show(ctx, |ui| {
                    egui::ScrollArea::horizontal()
                        .auto_shrink([false, false])
                        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                        .show(ui, |ui| {
                            ui.horizontal_centered(|ui| self.space_tabs(ui, &palette));
                        });
                });
        }
    }

    fn title_controls(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        let show_icons = self.settings.show_icons;
        if let UpdateStatus::Updated(version) = &self.update_status
            && tool_button_tinted(
                ui,
                UiIcon::Refresh,
                &format!("v{version}"),
                false,
                show_icons,
                palette,
                Some(palette.accent),
            )
            .on_hover_text(format!("ais updated to v{version} — click to restart"))
            .clicked()
        {
            self.actions.push(Action::RestartToUpdate);
        }
        if theme_button(ui, palette, self.settings.theme)
            .on_hover_text(format!(
                "Appearance: {} — click for {}  {}",
                self.settings.theme.label(),
                self.settings.theme.next().label(),
                self.key_hint(Cmd::ToggleTheme)
            ))
            .clicked()
        {
            self.actions.push(Action::ToggleTheme);
        }
        if tool_button(ui, UiIcon::Settings, "", self.show_settings, show_icons, palette)
            .on_hover_text(format!("Settings  {}", self.key_hint(Cmd::Settings)))
            .clicked()
        {
            self.actions.push(Action::OpenSettings);
        }
        let blocks_btn = tool_button(
            ui,
            UiIcon::Layout,
            "",
            !self.settings.show_agents
                || !self.settings.show_toolbar
                || !self.settings.show_pane_headers
                || !self.settings.show_status_bar,
            show_icons,
            palette,
        )
        .on_hover_text("Toggle UI blocks & fullscreen terminal");
        let popup_id = Id::new("title-blocks-popup");
        if blocks_btn.clicked() {
            ui.memory_mut(|m| m.toggle_popup(popup_id));
        }
        egui::popup_below_widget(
            ui,
            popup_id,
            &blocks_btn,
            egui::PopupCloseBehavior::CloseOnClickOutside,
            |ui| {
                ui.set_min_width(210.0);
                self.layout_blocks_menu(ui);
            },
        );
        if tool_button(
            ui,
            UiIcon::Fullscreen,
            "",
            self.terminal_fullscreen,
            show_icons,
            palette,
        )
        .on_hover_text(format!(
            "Fullscreen terminal  {}",
            self.key_hint(Cmd::FullscreenTerminal)
        ))
        .clicked()
        {
            self.actions.push(Action::ToggleFullscreenTerminal);
        }
        let unread_str = if self.unread > 0 {
            self.unread.min(99).to_string()
        } else {
            String::new()
        };
        let alerts_open = self.right_tab() == Some(RightTab::Notices);
        let bell_tint = (self.unread > 0 && !alerts_open).then_some(palette.amber);
        if tool_button_tinted(
            ui,
            UiIcon::Bell,
            &unread_str,
            alerts_open,
            show_icons,
            palette,
            bell_tint,
        )
        .on_hover_text(format!("Alerts  {}", self.key_hint(Cmd::ToggleAlerts)))
        .clicked()
        {
            self.actions.push(Action::OpenNotices);
        }
    }

    pub fn layout_blocks_menu(&mut self, ui: &mut egui::Ui) {
        let items = [
            (
                self.settings.show_title_bar,
                "Title bar",
                Cmd::ToggleTitleBar,
                Action::ToggleTitleBar,
            ),
            (
                self.settings.show_sidebar,
                "Sidebar",
                Cmd::ToggleSidebar,
                Action::ToggleSidebar,
            ),
            (
                self.settings.show_toolbar,
                "Project toolbar",
                Cmd::ToggleToolbar,
                Action::ToggleToolbar,
            ),
            (
                self.settings.show_agents,
                "Agent pills",
                Cmd::ToggleAgents,
                Action::ToggleAgents,
            ),
            (
                self.settings.show_pane_headers,
                "Pane headers",
                Cmd::TogglePaneHeaders,
                Action::TogglePaneHeaders,
            ),
            (
                self.settings.show_status_bar,
                "Status bar",
                Cmd::ToggleStatusBar,
                Action::ToggleStatusBar,
            ),
        ];
        for (checked, label, cmd, action) in items {
            let hint = self.key_hint(cmd);
            let text = if hint.is_empty() {
                label.to_string()
            } else {
                format!("{label}  ({hint})")
            };
            let mut val = checked;
            if ui.checkbox(&mut val, text).clicked() {
                self.actions.push(action);
            }
        }
        ui.separator();
        let dock_items = [
            (
                self.right_tab() == Some(RightTab::Files),
                "Files dock",
                Cmd::ToggleFiles,
                Action::ToggleFiles,
            ),
            (
                self.right_tab() == Some(RightTab::Usage),
                "Usage dock",
                Cmd::ToggleUsage,
                Action::ToggleUsage,
            ),
        ];
        for (checked, label, cmd, action) in dock_items {
            let hint = self.key_hint(cmd);
            let text = if hint.is_empty() {
                label.to_string()
            } else {
                format!("{label}  ({hint})")
            };
            let mut val = checked;
            if ui.checkbox(&mut val, text).clicked() {
                self.actions.push(action);
            }
        }
        ui.separator();
        let fs_hint = self.key_hint(Cmd::FullscreenTerminal);
        let fs_label = if self.terminal_fullscreen {
            format!("Exit fullscreen terminal  {fs_hint}")
        } else {
            format!("Fullscreen terminal  {fs_hint}")
        };
        if ui.button(fs_label).clicked() {
            self.actions.push(Action::ToggleFullscreenTerminal);
            ui.close_menu();
        }
        let win_fs = ui.ctx().input(|i| i.viewport().fullscreen.unwrap_or(false));
        let win_label = if win_fs {
            "Exit window fullscreen"
        } else {
            "Window fullscreen"
        };
        if ui.button(win_label).clicked() {
            self.actions.push(Action::ToggleWindowFullscreen);
            ui.close_menu();
        }
        if ui.button("Show all blocks").clicked() {
            self.actions.push(Action::ShowAllBlocks);
            ui.close_menu();
        }
    }

    /// Floating hover bar shown near the top-right edge when the title bar is
    /// hidden or fullscreen terminal mode is active, so mouse users always have
    /// an effortless way back.
    pub fn floating_chrome_bar(&mut self, ctx: &egui::Context) {
        let screen = ctx.screen_rect();
        let popup_id = Id::new("floating-blocks-popup");
        let popup_open = ctx.memory(|m| m.is_popup_open(popup_id));
        let near_top = ctx.input(|i| {
            i.pointer
                .hover_pos()
                .is_some_and(|pos| pos.y <= screen.top() + 44.0 && pos.x >= screen.right() - 420.0)
        });
        if !near_top && !popup_open {
            return;
        }
        let palette = self.palette();
        let show_icons = self.settings.show_icons;
        egui::Area::new(Id::new("floating-chrome-bar"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::RIGHT_TOP, Vec2::new(-10.0, 8.0))
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(palette.panel.gamma_multiply(0.94))
                    .stroke(Stroke::new(1.0, palette.border_strong))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::symmetric(8, 4))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            if self.terminal_fullscreen {
                                if ui
                                    .add(small_button(
                                        &format!("Exit fullscreen  {}", self.key_hint(Cmd::FullscreenTerminal)),
                                        &palette,
                                    ))
                                    .clicked()
                                {
                                    self.actions.push(Action::ToggleFullscreenTerminal);
                                }
                            } else if !self.settings.show_title_bar {
                                if ui.add(small_button("Show title bar", &palette)).clicked() {
                                    self.actions.push(Action::ToggleTitleBar);
                                }
                                if tool_button(ui, UiIcon::Fullscreen, "", false, show_icons, &palette)
                                    .on_hover_text(format!(
                                        "Fullscreen terminal  {}",
                                        self.key_hint(Cmd::FullscreenTerminal)
                                    ))
                                    .clicked()
                                {
                                    self.actions.push(Action::ToggleFullscreenTerminal);
                                }
                            }
                            let win_fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                            if ui
                                .add(small_button(
                                    if win_fs { "Windowed" } else { "OS Fullscreen" },
                                    &palette,
                                ))
                                .clicked()
                            {
                                self.actions.push(Action::ToggleWindowFullscreen);
                            }
                            let blocks_btn = tool_button(ui, UiIcon::Layout, "Blocks", false, show_icons, &palette);
                            if blocks_btn.clicked() {
                                ui.memory_mut(|m| m.toggle_popup(popup_id));
                            }
                            egui::popup_below_widget(
                                ui,
                                popup_id,
                                &blocks_btn,
                                egui::PopupCloseBehavior::CloseOnClickOutside,
                                |ui| {
                                    ui.set_min_width(210.0);
                                    self.layout_blocks_menu(ui);
                                },
                            );
                        });
                    });
            });
    }

    fn search_field(&mut self, ui: &mut egui::Ui, palette: &Palette, compact: bool) {
        let id = Id::new(SEARCH_ID);
        let focused = ui.memory(|memory| memory.has_focus(id));
        let active = focused || self.search_open;
        let frame = egui::Frame::NONE
            .fill(if active { palette.panel } else { palette.raised })
            .stroke(Stroke::new(
                1.0,
                if active {
                    palette.accent.gamma_multiply(0.7)
                } else {
                    palette.border
                },
            ))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::symmetric(10, 0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_height(24.0);
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if self.settings.show_icons {
                        let (r, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                        UiIcon::Search.paint(ui.painter(), r, palette.faint);
                    }
                    let hint = self.key_hint(Cmd::Search);
                    let hint_width = if hint.is_empty() || compact { 0.0 } else { 40.0 };
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut self.palette_state.query)
                            .id(id)
                            .frame(false)
                            .return_key(None)
                            .hint_text(
                                RichText::new(if compact {
                                    "Search"
                                } else {
                                    "Search projects, conversations, commands…"
                                })
                                .color(palette.faint),
                            )
                            .font(FontId::proportional(scale::FONT_MD))
                            .desired_width((ui.available_width() - hint_width).max(40.0)),
                    );
                    if edit.gained_focus() || edit.changed() {
                        if !self.search_open {
                            self.search_open = true;
                            self.palette_state.cache_dirty = true;
                            if let Some(path) = self.active_space.map(|i| self.spaces[i].path.clone()) {
                                self.sessions.ensure(&path, Duration::from_secs(60));
                            }
                        }
                        if edit.changed() {
                            self.palette_state.selected = 0;
                        }
                    }
                    if hint_width > 0.0 {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(RichText::new(hint).size(11.0).color(palette.faint));
                        });
                    }
                });
            });
        let rect = frame.response.rect;
        self.search_rect = rect;
        if ui.interact(rect, id.with("frame"), Sense::click()).clicked() {
            ui.memory_mut(|memory| memory.request_focus(id));
        }
    }

    fn close_search(&mut self, ctx: &egui::Context) {
        self.search_open = false;
        self.palette_state.query.clear();
        ctx.memory_mut(|memory| memory.surrender_focus(Id::new(SEARCH_ID)));
        self.pending_focus = self
            .active_space
            .and_then(|i| self.spaces.get(i))
            .and_then(Space::active_pane)
            .map(|pane| pane.uid);
    }

    pub fn search_dropdown(&mut self, ctx: &egui::Context) {
        if !self.search_open {
            return;
        }
        let nav = self.search_nav;
        let clicked_outside = ctx.input(|i| {
            i.pointer.any_pressed()
                && i.pointer
                    .interact_pos()
                    .is_some_and(|pos| !self.search_rect.contains(pos) && !self.search_drop_rect.contains(pos))
        });
        if nav.escape || clicked_outside {
            self.close_search(ctx);
            return;
        }
        let palette = self.palette();
        self.ensure_palette_cache(&palette);
        let typed_item = self.typed_folder_item();
        let filtered = crate::ui_settings::filter_palette(
            &self.palette_state.cached_items,
            typed_item.as_ref(),
            &self.palette_state.query,
        );
        let count = filtered.len();
        if count > 0 {
            if nav.down {
                self.palette_state.selected = (self.palette_state.selected + 1) % count;
            }
            if nav.up {
                self.palette_state.selected = (self.palette_state.selected + count - 1) % count;
            }
        }
        self.palette_state.selected = self.palette_state.selected.min(count.saturating_sub(1));
        let mut chosen = if nav.enter {
            filtered
                .get(self.palette_state.selected)
                .map(|item| item.action.clone())
        } else {
            None
        };
        let screen = ctx.screen_rect();
        let width = self.search_rect.width().max(460.0).min(screen.width() - 24.0);
        let x = (self.search_rect.center().x - width / 2.0).clamp(screen.left() + 12.0, screen.right() - width - 12.0);
        let pos = Pos2::new(x, self.search_rect.bottom() + 6.0);
        let selected = self.palette_state.selected;
        let vim = self.settings.nav_vim;
        let area = egui::Area::new(Id::new("search-dropdown"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(palette.panel)
                    .stroke(Stroke::new(1.0, palette.border_strong))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(8))
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 8],
                        blur: 24,
                        spread: 0,
                        color: Color32::from_black_alpha(if palette.light { 40 } else { 120 }),
                    })
                    .show(ui, |ui| {
                        ui.set_width(width - 16.0);
                        let (pick, hover) = crate::ui_settings::palette_rows(
                            ui,
                            &filtered,
                            selected,
                            nav.up || nav.down,
                            &palette,
                            (screen.height() * 0.6).max(200.0),
                        );
                        if pick.is_some() {
                            chosen = pick;
                        }
                        if let Some(index) = hover {
                            self.palette_state.selected = index;
                        }
                        ui.add_space(4.0);
                        let keys = if vim {
                            "↑↓ / ⌃J ⌃K select · ↵ run · esc close"
                        } else {
                            "↑↓ select · ↵ run · esc close"
                        };
                        ui.label(RichText::new(keys).size(scale::FONT_XS).color(palette.faint));
                    });
            });
        self.search_drop_rect = area.response.rect;
        if let Some(action) = chosen {
            self.close_search(ctx);
            self.actions.push(action);
        }
    }

    fn space_tabs(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        let mut close = None;
        let mut move_action = None;
        ui.spacing_mut().item_spacing.x = 4.0;
        let close_hint = format!("Close space (kills its terminals)  {}", self.key_hint(Cmd::CloseSpace));
        let labels: Vec<String> = self
            .spaces
            .iter()
            .map(|space| {
                if self.spaces.iter().filter(|other| other.name == space.name).count() < 2 {
                    return space.name.clone();
                }
                match space.path.parent() {
                    Some(parent) if parent == self.home => format!("~/{}", space.name),
                    Some(parent) => match parent.file_name() {
                        Some(dir) => format!("{}/{}", dir.to_string_lossy(), space.name),
                        None => space.path.display().to_string(),
                    },
                    None => space.name.clone(),
                }
            })
            .collect();
        let total_spaces = self.spaces.len();
        let reveal = std::mem::take(&mut self.reveal_tab);
        for (index, space) in self.spaces.iter().enumerate() {
            let active = self.active_space == Some(index);
            let id = Id::new(("space-tab", index));
            let hovered = ui.ctx().read_response(id).is_some_and(|response| response.hovered());
            let (fill, stroke) = if active {
                (palette.raised, Stroke::new(1.0, palette.border_strong))
            } else if hovered {
                (palette.hover, Stroke::new(1.0, palette.border_strong))
            } else {
                (palette.panel, Stroke::new(1.0, palette.border))
            };
            let tab = ui.scope_builder(egui::UiBuilder::new().id_salt(id).sense(Sense::click()), |ui| {
                egui::Frame::NONE
                    .fill(fill)
                    .stroke(stroke)
                    .corner_radius(CornerRadius::same(7))
                    .inner_margin(Margin {
                        left: 10,
                        right: 4,
                        top: 2,
                        bottom: 2,
                    })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            let (dot, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
                            let dot_color = if !space.available {
                                Some(palette.red)
                            } else if space.attention() {
                                Some(palette.amber)
                            } else if space.busy() {
                                Some(palette.accent)
                            } else if space.running_count() > 0 {
                                Some(palette.green)
                            } else {
                                None
                            };
                            match dot_color {
                                Some(color) => {
                                    let radius = if space.busy() && !self.settings.reduce_motion {
                                        2.5 + 1.0 * (ui.input(|i| i.time) as f32 * 5.0).sin().abs()
                                    } else {
                                        3.5
                                    };
                                    ui.painter().circle_filled(dot.center(), radius, color);
                                }
                                None => {
                                    ui.painter()
                                        .circle_stroke(dot.center(), 3.0, Stroke::new(1.0, palette.faint));
                                }
                            }
                            let mut text = RichText::new(&labels[index]).size(12.5);
                            text = if !space.available {
                                text.color(palette.red)
                            } else if active {
                                text.color(palette.text).strong()
                            } else {
                                text.color(palette.muted)
                            };
                            ui.label(text);
                            if index < 9 {
                                ui.label(
                                    RichText::new(format!("{MOD}{}", index + 1))
                                        .size(scale::FONT_XS)
                                        .color(if active { palette.muted } else { palette.faint }),
                                );
                            }
                            if close_button(ui, palette).on_hover_text(&close_hint).clicked() {
                                close = Some(index);
                            }
                        });
                    });
            });
            let tip = if space.available {
                pretty_path(&space.path, &self.home)
            } else {
                format!("{} (folder unavailable)", pretty_path(&space.path, &self.home))
            };
            let response = tab.response.on_hover_text(tip);
            if active && reveal {
                response.scroll_to_me(None);
            }
            if active {
                let rect = response.rect;
                ui.painter().line_segment(
                    [
                        Pos2::new(rect.left() + 9.0, rect.bottom() - 1.0),
                        Pos2::new(rect.right() - 9.0, rect.bottom() - 1.0),
                    ],
                    Stroke::new(2.0, palette.accent),
                );
            }
            if response.clicked() && close != Some(index) {
                self.actions.push(Action::SelectSpace(index));
            }
            if response.middle_clicked() {
                close = Some(index);
            }
            response.context_menu(|ui| {
                if !space.available && ui.button("Retry folder").clicked() {
                    self.actions.push(Action::RetrySpace(index));
                    ui.close_menu();
                }
                if index > 0 && ui.button("Move left").clicked() {
                    move_action = Some((index, -1));
                    ui.close_menu();
                }
                if index + 1 < total_spaces && ui.button("Move right").clicked() {
                    move_action = Some((index, 1));
                    ui.close_menu();
                }
                if total_spaces > 1 {
                    ui.separator();
                }
                if ui.button("Close space").clicked() {
                    close = Some(index);
                    ui.close_menu();
                }
            });
        }
        let add_label = if self.spaces.is_empty() { "Open project" } else { "" };
        if tool_button(ui, UiIcon::Plus, add_label, false, self.settings.show_icons, palette)
            .on_hover_text(format!(
                "Open a folder  {} · recent projects  {}",
                self.key_hint(Cmd::OpenFolder),
                self.key_hint(Cmd::Search)
            ))
            .clicked()
        {
            self.actions.push(Action::PickFolder);
        }
        if let Some((index, delta)) = move_action {
            self.actions.push(Action::MoveSpace(index, delta));
        }
        if let Some(index) = close {
            self.actions.push(Action::CloseSpace(index));
        }
    }

    fn git_branch(&mut self, path: &Path) -> Option<String> {
        if let Some((at, branch)) = self.git_branches.get(path)
            && at.elapsed() < Duration::from_secs(5)
        {
            return branch.clone();
        }
        let branch = read_git_branch(path);
        self.git_branches
            .insert(path.to_path_buf(), (Instant::now(), branch.clone()));
        branch
    }

    pub fn status_bar(&mut self, ctx: &egui::Context) {
        let palette = self.palette();
        let space_info = self
            .active_space
            .and_then(|i| self.spaces.get(i))
            .map(|space| (space.path.clone(), space.available));
        let branch = space_info
            .as_ref()
            .filter(|(_, avail)| *avail)
            .and_then(|(path, _)| self.git_branch(path));
        egui::TopBottomPanel::bottom("status")
            .exact_height(24.0)
            .frame(
                egui::Frame::NONE
                    .fill(palette.chrome)
                    .inner_margin(Margin::symmetric(12, 0))
                    .stroke(Stroke::new(1.0, palette.border)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let small = |text: String, color: Color32| RichText::new(text).size(scale::FONT_SM).color(color);
                    let sep = |ui: &mut egui::Ui| {
                        ui.label(RichText::new("│").size(11.0).color(palette.border_strong));
                    };
                    if let Some((path, available)) = &space_info {
                        let label = if *available {
                            pretty_path(path, &self.home)
                        } else {
                            format!("{} (unavailable)", pretty_path(path, &self.home))
                        };
                        ui.label(small(label, if *available { palette.muted } else { palette.red }))
                            .on_hover_text("Project folder");
                        if let Some(branch) = &branch {
                            ui.label(small(format!("⎇ {branch}"), palette.cyan))
                                .on_hover_text("Git branch");
                        }
                    } else {
                        ui.label(small(format!("{} projects", self.projects.len()), palette.muted));
                    }
                    if let Some(pane) = self
                        .active_space
                        .and_then(|i| self.spaces.get(i))
                        .and_then(Space::active_pane)
                        .filter(|pane| !pane.is_empty())
                    {
                        sep(ui);
                        let color = pane
                            .client()
                            .map(|kind| kind_color(kind, &palette))
                            .unwrap_or(palette.muted);
                        let state = if pane.is_busy() {
                            "working"
                        } else if pane.is_running() {
                            "idle"
                        } else {
                            "exited"
                        };
                        ui.label(small(format!("● {}", truncate(&pane.title(), 36)), color));
                        ui.label(small(
                            state.into(),
                            if pane.is_busy() { palette.amber } else { palette.faint },
                        ));
                        ui.label(small(format!("{}×{}", pane.cols, pane.rows), palette.faint))
                            .on_hover_text("Terminal size (columns × rows)");
                        let mode = pane.mode();
                        let mut flags = Vec::new();
                        if mode.contains(alacritty_terminal::term::TermMode::ALT_SCREEN) {
                            flags.push("fullscreen");
                        }
                        if mode.intersects(alacritty_terminal::term::TermMode::MOUSE_MODE) {
                            flags.push("mouse · ⇧drag selects");
                        }
                        if !flags.is_empty() {
                            ui.label(small(flags.join(" · "), palette.faint));
                        }
                        let (offset, _, _) = pane.scroll_state();
                        if offset > 0 {
                            ui.label(small(format!("↑ {offset} lines"), palette.amber));
                        }
                    }
                    if self.scanning {
                        sep(ui);
                        ui.add(egui::Spinner::new().size(11.0));
                        ui.label(small("scanning history".into(), palette.faint));
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        let wide = ui.available_width() > 520.0;
                        let hint = self.key_hint(Cmd::Search);
                        if !hint.is_empty() {
                            ui.label(small(format!("{hint} search"), palette.faint));
                            sep(ui);
                        }
                        let kind = self
                            .active_space
                            .and_then(|i| self.spaces.get(i))
                            .and_then(Space::active_pane)
                            .and_then(TerminalPane::client)
                            .filter(|kind| kind.is_ai())
                            .unwrap_or(CliKind::Claude);
                        if let Some(headline) = self.usage.headline(kind) {
                            let pct = |limit: &Limit| {
                                if limit.is_reset_passed() {
                                    0
                                } else {
                                    limit
                                        .clamped_used()
                                        .map(|used| (used * 100.0).round() as u32)
                                        .unwrap_or(0)
                                }
                            };
                            let tone = |limit: &Limit| {
                                if limit.is_reset_passed() {
                                    palette.faint
                                } else {
                                    match limit.clamped_used().unwrap_or(0.0) {
                                        used if used >= 0.9 => palette.red,
                                        used if used >= 0.7 => palette.amber,
                                        _ => palette.muted,
                                    }
                                }
                            };
                            let format_limit = |prefix: &str, limit: &Limit| {
                                let p = pct(limit);
                                let used = if limit.is_reset_passed() {
                                    0.0
                                } else {
                                    limit.clamped_used().unwrap_or(0.0)
                                };
                                let badge = usage_badge(used, limit.is_reset_passed());
                                if badge.is_empty() {
                                    format!("{prefix} {p}%")
                                } else {
                                    format!("{badge}{prefix} {p}%")
                                }
                            };
                            let response = ui
                                .horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 7.0;
                                    if wide {
                                        if let Some(week) = headline.week {
                                            ui.label(small(format_limit("wk", week), tone(week)));
                                        }
                                        if let Some(short) = headline.short {
                                            ui.label(small(format_limit(&short.window, short), tone(short)));
                                        }
                                        ui.label(small(kind.label().into(), kind_color(kind, &palette)));
                                    } else if let Some(primary) = headline.short.or(headline.week) {
                                        let prefix = if primary.window == "week" {
                                            "wk"
                                        } else {
                                            &primary.window
                                        };
                                        ui.label(small(format_limit(prefix, primary), tone(primary)));
                                    }
                                })
                                .response;
                            if response
                                .interact(Sense::click())
                                .on_hover_text(format!(
                                    "{} limits — click for details  {}",
                                    kind.label(),
                                    self.key_hint(Cmd::ToggleUsage)
                                ))
                                .clicked()
                            {
                                self.actions.push(Action::ToggleUsage);
                            }
                            sep(ui);
                        }
                        let panes = self.spaces.iter().flat_map(|space| space.panes.iter());
                        let (mut running, mut busy, mut total) = (0, 0, 0);
                        for pane in panes {
                            total += 1;
                            if pane.is_running() && pane.client().is_some_and(CliKind::is_ai) {
                                running += 1;
                                if pane.is_busy() {
                                    busy += 1;
                                }
                            }
                        }
                        if busy > 0 {
                            ui.add(egui::Spinner::new().size(10.0).color(palette.amber));
                            ui.label(small(format!("{busy} working"), palette.amber));
                        }
                        ui.label(small(
                            plural(running, "agent"),
                            if running > 0 { palette.green } else { palette.faint },
                        ))
                        .on_hover_text("AI CLIs running in all spaces");
                        if wide {
                            sep(ui);
                            ui.label(small(
                                format!("{} · {}", plural(self.spaces.len(), "space"), plural(total, "pane")),
                                palette.faint,
                            ));
                        }
                    });
                });
            });
    }
}
