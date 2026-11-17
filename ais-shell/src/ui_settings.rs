//! Command palette (with cached items + fuzzy scoring) and tabbed Settings window.

use crate::app::{
    APP_TITLE, Action, AisApp, LaunchMode, NoticeKind, PaletteItem, SettingsTab, Space, Target, command_action,
    custom_titlebar,
};
use crate::cli::{CliKind, canonical_path};
use crate::keys::{Command as Cmd, MOD, format_chord};
use crate::sessions::truncate;
use crate::theme::{FontChoice, Palette, Settings, StartSessions, TerminalPalette, Theme, UiTheme, scale};
use crate::widgets::{
    SETTINGS_CONTROL, SETTINGS_WIDTH, age_system, close_button, kind_color, open_external, pretty_path, section,
    setting_note, setting_row, small_button, tabs,
};
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Id, Key, Layout, Margin, Pos2, RichText, Sense, Stroke, Vec2,
};
use std::path::PathBuf;
use std::time::{Duration, Instant};

impl AisApp {
    pub fn ensure_palette_cache(&mut self, palette: &Palette) {
        if !self.palette_state.cache_dirty {
            return;
        }
        self.palette_state.cached_items = self.build_palette_items(palette);
        self.palette_state.cache_dirty = false;
    }

    pub fn typed_folder_item(&self) -> Option<PaletteItem> {
        let typed = self.palette_state.query.trim();
        if !typed.starts_with('/') && !typed.starts_with('~') {
            return None;
        }
        let expanded = match typed.strip_prefix('~') {
            Some(rest) => self.home.join(rest.trim_start_matches('/')),
            None => PathBuf::from(typed),
        };
        expanded.is_dir().then(|| PaletteItem {
            group: "Open folder",
            title: typed.to_string(),
            detail: "open as project space".into(),
            color: None,
            action: Action::OpenProject(canonical_path(&expanded)),
        })
    }

    fn build_palette_items(&self, palette: &Palette) -> Vec<PaletteItem> {
        let mut items = Vec::new();
        let project = self.active_space.map(|i| self.spaces[i].name.clone());
        let where_ = project.unwrap_or_else(|| "first project".into());
        if self.active_space.is_some() {
            for kind in CliKind::ALL {
                if !self.cli[kind.index()].installed {
                    continue;
                }
                items.push(PaletteItem {
                    group: "Launch",
                    title: format!("New {} session", kind.label()),
                    detail: where_.clone(),
                    color: Some(kind_color(kind, palette)),
                    action: Action::Launch(kind, LaunchMode::New, Target::Auto),
                });
                if kind.continue_args().is_some() {
                    items.push(PaletteItem {
                        group: "Launch",
                        title: format!("Continue last {} session", kind.label()),
                        detail: where_.clone(),
                        color: Some(kind_color(kind, palette)),
                        action: Action::Launch(kind, LaunchMode::Continue, Target::Auto),
                    });
                }
            }
            items.push(PaletteItem {
                group: "Launch",
                title: "New terminal".into(),
                detail: self.key_hint(Cmd::NewShell),
                color: None,
                action: Action::Shell(Target::Auto),
            });
            if let Some(space) = self.active_space.and_then(|i| self.spaces.get(i))
                && let Some(list) = self.sessions.get(&space.path)
            {
                for session in list.sessions.iter().take(40) {
                    items.push(PaletteItem {
                        group: "Resume conversation",
                        title: truncate(&session.title, 70),
                        detail: format!(
                            "{} · {}",
                            session.client.label(),
                            session.updated.map(age_system).unwrap_or_default()
                        ),
                        color: Some(kind_color(session.client, palette)),
                        action: Action::Resume(Box::new(session.clone()), Target::Auto),
                    });
                }
            }
        }
        items.push(PaletteItem {
            group: "Open project",
            title: "Open folder…".into(),
            detail: self.key_hint(Cmd::OpenFolder),
            color: None,
            action: Action::PickFolder,
        });
        for project in &self.projects {
            items.push(PaletteItem {
                group: "Open project",
                title: project.name(),
                detail: project.pretty_path(&self.home),
                color: None,
                action: Action::OpenProject(project.path.clone()),
            });
        }
        for command in Cmd::ALL {
            items.push(PaletteItem {
                group: "Commands",
                title: command.label().into(),
                detail: self.key_hint(command),
                color: None,
                action: command_action(command),
            });
        }
        items.push(PaletteItem {
            group: "Commands",
            title: "Refresh usage limits".into(),
            detail: String::new(),
            color: None,
            action: Action::RefreshUsage,
        });
        for theme in UiTheme::ALL {
            items.push(PaletteItem {
                group: "Theme",
                title: format!("Theme: {}", theme.label()),
                detail: if theme == self.settings.ui_theme {
                    "current".into()
                } else {
                    String::new()
                },
                color: None,
                action: Action::SetUiTheme(theme),
            });
        }
        items
    }

    pub fn command_palette(&mut self, ctx: &egui::Context) {
        if !self.palette_state.open {
            return;
        }
        let palette = self.palette();
        self.ensure_palette_cache(&palette);
        let typed_item = self.typed_folder_item();
        let filtered = filter_palette(
            &self.palette_state.cached_items,
            typed_item.as_ref(),
            &self.palette_state.query,
        );
        let nav = self.read_list_nav(ctx);
        if nav.escape {
            self.palette_state.open = false;
            self.pending_focus = self
                .active_space
                .and_then(|i| self.spaces.get(i))
                .and_then(Space::active_pane)
                .map(|pane| pane.uid);
            return;
        }
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
        let width = (screen.width() - 40.0).clamp(320.0, 640.0);
        let selected = self.palette_state.selected;
        let vim = self.settings.nav_vim;
        let modal = egui::Modal::new(Id::new("command-palette"))
            .backdrop_color(Color32::from_black_alpha(if palette.light { 60 } else { 140 }))
            .frame(
                egui::Frame::NONE
                    .fill(palette.panel)
                    .stroke(Stroke::new(1.0, palette.border_strong))
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(Margin::same(10)),
            )
            .area(
                egui::Modal::default_area(Id::new("command-palette-area"))
                    .anchor(Align2::CENTER_TOP, Vec2::new(0.0, (screen.height() * 0.12).max(40.0))),
            );
        let response = modal.show(ctx, |ui| {
            ui.set_width(width);
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.palette_state.query)
                    .hint_text(
                        RichText::new("Search: claude, continue, resume, project… or type a path like ~/Dev/app")
                            .color(palette.faint),
                    )
                    .font(FontId::proportional(16.0))
                    .desired_width(f32::INFINITY)
                    .return_key(None)
                    .frame(false),
            );
            if self.palette_state.just_opened || !edit.has_focus() {
                edit.request_focus();
                self.palette_state.just_opened = false;
            }
            if edit.changed() {
                self.palette_state.selected = 0;
            }
            ui.separator();
            let (pick, hover) = palette_rows(
                ui,
                &filtered,
                selected,
                nav.up || nav.down,
                &palette,
                (screen.height() * 0.55).max(200.0),
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
        if response.should_close() {
            self.palette_state.open = false;
        }
        if !self.palette_state.open {
            self.pending_focus = self.pending_focus.or_else(|| {
                self.active_space
                    .and_then(|i| self.spaces.get(i))
                    .and_then(Space::active_pane)
                    .map(|pane| pane.uid)
            });
        }
        if let Some(action) = chosen {
            self.palette_state.open = false;
            self.actions.push(action);
        }
    }

    pub fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            self.capturing = None;
            return;
        }
        let palette = self.palette();
        let mut open = true;
        let mut changed = false;
        let mut style_changed = false;
        let mut fonts_changed = false;
        let light = self.effective_theme() == Theme::Light;
        let backdrop = egui::Area::new(Id::new("settings-backdrop"))
            .order(egui::Order::Middle)
            .fixed_pos(Pos2::ZERO)
            .show(ctx, |ui| {
                let screen = ctx.screen_rect();
                ui.painter()
                    .rect_filled(screen, 0.0, Color32::from_black_alpha(if light { 70 } else { 120 }));
                ui.allocate_rect(screen, Sense::click())
            })
            .inner;
        if backdrop.clicked() && self.capturing.is_none() {
            open = false;
        }
        egui::Window::new("Settings")
            .order(egui::Order::Foreground)
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .frame(
                egui::Frame::window(&ctx.style())
                    .fill(palette.panel)
                    .stroke(Stroke::new(1.0, palette.border_strong))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin {
                        left: 14,
                        right: 10,
                        top: 8,
                        bottom: 12,
                    }),
            )
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.set_width(SETTINGS_WIDTH + 4.0);
                    ui.label(RichText::new("Settings").size(scale::FONT_MD).strong().color(palette.text));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if close_button(ui, &palette).on_hover_text("Close  esc").clicked() {
                            open = false;
                        }
                    });
                });
                if ctx.input(|i| i.key_pressed(Key::Escape)) && self.capturing.is_none() {
                    open = false;
                }
                ui.add_space(4.0);
                // Tab bar + filter input
                ui.horizontal(|ui| {
                    ui.set_width(SETTINGS_WIDTH);
                    let selected = SettingsTab::ALL
                        .iter()
                        .position(|t| *t == self.settings_tab)
                        .unwrap_or(0);
                    let labels: Vec<&str> = SettingsTab::ALL.iter().map(|t| t.label()).collect();
                    if let Some(idx) = tabs(ui, &labels, selected, &palette) {
                        self.settings_tab = SettingsTab::ALL[idx];
                    }
                });
                ui.add_space(4.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.settings_search)
                        .hint_text(RichText::new("Search settings…").size(12.0).color(palette.faint))
                        .margin(Margin::symmetric(8, 4))
                        .desired_width(SETTINGS_WIDTH),
                );
                ui.add_space(4.0);
                ui.separator();

                let q = self.settings_search.trim().to_lowercase();
                let searching = !q.is_empty();
                let matches_query = |hay: &str| !searching || hay.to_lowercase().contains(&q);
                let active_tab = self.settings_tab;
                let show_tab = |tab: SettingsTab| searching || active_tab == tab;

                let max_height = (ctx.screen_rect().height() * 0.78 - 110.0).max(160.0);
                egui::ScrollArea::vertical()
                    .max_height(max_height)
                    .min_scrolled_height(max_height)
                    .show(ui, |ui| {
                        ui.set_width(SETTINGS_WIDTH);
                        let visuals = ui.visuals_mut();
                        visuals.slider_trailing_fill = true;
                        visuals.selection.bg_fill = palette.accent;
                        visuals.widgets.inactive.bg_fill = palette.border_strong;
                        visuals.widgets.hovered.bg_fill = palette.muted;
                        visuals.widgets.active.bg_fill = palette.accent;
                        ui.spacing_mut().slider_width = SETTINGS_CONTROL - 64.0;

                        if show_tab(SettingsTab::Appearance) {
                            if matches_query("appearance theme mode palette scale font size icons motion") {
                                section(ui, "Appearance", &palette);
                            }
                            if matches_query("theme color scheme") {
                                setting_row(ui, "Theme", |ui| {
                                    egui::ComboBox::from_id_salt("ui-theme")
                                        .selected_text(self.settings.ui_theme.label())
                                        .width(SETTINGS_CONTROL)
                                        .show_ui(ui, |ui| {
                                            for choice in UiTheme::ALL {
                                                let swatch = choice.palette(light);
                                                let response = ui.horizontal(|ui| {
                                                    for color in [swatch.bg, swatch.panel, swatch.accent, swatch.green, swatch.red] {
                                                        let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                                                        ui.painter().rect_filled(rect, 2.0, color);
                                                        ui.painter().rect_stroke(
                                                            rect,
                                                            2.0,
                                                            Stroke::new(1.0, palette.border),
                                                            egui::StrokeKind::Inside,
                                                        );
                                                    }
                                                    ui.selectable_label(self.settings.ui_theme == choice, choice.label())
                                                });
                                                if response.inner.clicked() {
                                                    self.settings.ui_theme = choice;
                                                    style_changed = true;
                                                }
                                            }
                                        });
                                });
                            }
                            if matches_query("mode system light dark") {
                                setting_row(ui, "Mode", |ui| {
                                    let selected = Theme::ALL.iter().position(|theme| *theme == self.settings.theme).unwrap_or(0);
                                    let labels: Vec<&str> = Theme::ALL.iter().map(|theme| theme.label()).collect();
                                    if let Some(index) = tabs(ui, &labels, selected, &palette) {
                                        self.settings.theme = Theme::ALL[index];
                                        style_changed = true;
                                    }
                                });
                            }
                            if matches_query("terminal colors palette") {
                                setting_row(ui, "Terminal colors", |ui| {
                                    egui::ComboBox::from_id_salt("terminal-palette")
                                        .selected_text(self.settings.terminal_palette.label())
                                        .width(SETTINGS_CONTROL)
                                        .show_ui(ui, |ui| {
                                            for choice in TerminalPalette::ALL {
                                                if ui
                                                    .selectable_label(self.settings.terminal_palette == choice, choice.label())
                                                    .clicked()
                                                {
                                                    self.settings.terminal_palette = choice;
                                                    style_changed = true;
                                                }
                                            }
                                        });
                                });
                            }
                            if matches_query("ui scale zoom") {
                                setting_row(ui, "UI scale", |ui| {
                                    style_changed |= ui
                                        .add(egui::Slider::new(&mut self.settings.ui_scale, 0.7..=1.6).step_by(0.05))
                                        .changed();
                                });
                            }
                            if matches_query("interface font terminal font") {
                                for (label, salt, is_ui) in [("Interface font", "ui-font", true), ("Terminal font", "term-font", false)] {
                                    setting_row(ui, label, |ui| {
                                        let current = if is_ui { self.settings.ui_font } else { self.settings.term_font };
                                        egui::ComboBox::from_id_salt(salt)
                                            .selected_text(current.label())
                                            .width(SETTINGS_CONTROL)
                                            .show_ui(ui, |ui| {
                                                for choice in FontChoice::ALL {
                                                    if ui.selectable_label(current == choice, choice.label()).clicked() {
                                                        if is_ui {
                                                            self.settings.ui_font = choice;
                                                        } else {
                                                            self.settings.term_font = choice;
                                                        }
                                                        fonts_changed = true;
                                                    }
                                                }
                                            });
                                    });
                                }
                            }
                            if matches_query("font size") {
                                setting_row(ui, "Font size", |ui| {
                                    style_changed |= ui
                                        .add(egui::Slider::new(&mut self.settings.font_size, 10.0..=28.0).step_by(0.5))
                                        .changed();
                                });
                            }
                            if matches_query("icons reduce motion animation") {
                                setting_row(ui, "Icons & motion", |ui| {
                                    ui.vertical(|ui| {
                                        changed |= ui.checkbox(&mut self.settings.show_icons, "Vector UI icons").changed();
                                        changed |= ui.checkbox(&mut self.settings.reduce_motion, "Reduce motion (stop pulsing dots)").changed();
                                    });
                                });
                            }
                            if matches_query("layout side panels conversations agents status bar title bar") {
                                section(ui, "Layout", &palette);
                                setting_row(ui, "Side panels", |ui| {
                                    let labels = ["Sidebar left", "Sidebar right"];
                                    if let Some(index) = tabs(ui, &labels, self.settings.swap_sides as usize, &palette) {
                                        self.settings.swap_sides = index == 1;
                                        changed = true;
                                    }
                                });
                                setting_note(ui, "Files / Usage / Alerts dock sits on the opposite side.", &palette);
                                setting_row(ui, "Conversations", |ui| {
                                    let selected = StartSessions::ALL
                                        .iter()
                                        .position(|mode| *mode == self.settings.start_sessions)
                                        .unwrap_or(0);
                                    let labels: Vec<&str> = StartSessions::ALL.iter().map(|mode| mode.label()).collect();
                                    if let Some(index) = tabs(ui, &labels, selected, &palette) {
                                        self.settings.start_sessions = StartSessions::ALL[index];
                                        changed = true;
                                    }
                                });
                                setting_note(ui, "List on an empty pane. Auto hides it while the sidebar shows Sessions.", &palette);
                                setting_row(ui, "Show", |ui| {
                                    ui.vertical(|ui| {
                                        changed |= ui.checkbox(&mut self.settings.show_agents, "Agent cards").changed();
                                        changed |= ui.checkbox(&mut self.settings.show_status_bar, "Status bar").changed();
                                    });
                                });
                                setting_row(ui, "Search field", |ui| {
                                    let labels = ["Center, tabs below", "Compact"];
                                    if let Some(index) = tabs(ui, &labels, (!self.settings.search_center) as usize, &palette) {
                                        self.settings.search_center = index == 0;
                                        changed = true;
                                    }
                                });
                                if cfg!(target_os = "macos") {
                                    setting_row(ui, "Window frame", |ui| {
                                        changed |= ui
                                            .checkbox(&mut self.settings.native_titlebar, "System title bar")
                                            .changed();
                                    });
                                    let note = if self.settings.native_titlebar == custom_titlebar() {
                                        "Restart ais to apply."
                                    } else {
                                        "Off: ais draws its own bar in the theme colors."
                                    };
                                    setting_note(ui, note, &palette);
                                }
                            }
                        }

                        if show_tab(SettingsTab::Terminal)
                            && matches_query("terminal option meta copy select cursor blink notifications osc 52 clipboard usage background")
                        {
                            section(ui, "Terminal & privacy", &palette);
                            setting_row(ui, "Behavior", |ui| {
                                ui.vertical(|ui| {
                                    changed |= ui
                                        .checkbox(&mut self.settings.option_as_meta, "Option sends Meta (⌥B / ⌥F)")
                                        .changed();
                                    changed |= ui.checkbox(&mut self.settings.copy_on_select, "Copy on select").changed();
                                    changed |= ui.checkbox(&mut self.settings.cursor_blink, "Blinking cursor").changed();
                                    changed |= ui
                                        .checkbox(&mut self.settings.system_notifications, "System notifications")
                                        .changed();
                                    changed |= ui
                                        .checkbox(&mut self.settings.allow_osc52, "Allow OSC 52 clipboard writes")
                                        .changed();
                                    changed |= ui
                                        .checkbox(&mut self.settings.auto_live_usage, "Auto-refresh live usage limits")
                                        .changed();
                                });
                            });
                            setting_note(
                                ui,
                                "Live usage polling runs official CLIs (claude, codex, agy) every 5–15 min. Off = cached snapshots only unless you click ↻.",
                                &palette,
                            );
                        }

                        if show_tab(SettingsTab::Keyboard) {
                            if matches_query("navigation arrows vim keyboard shortcuts") {
                                section(ui, "Navigation", &palette);
                                setting_row(ui, "Arrow keys", |ui| {
                                    let panes_chord = format_chord(false, true, false, "↑↓");
                                    let spaces_chord = format_chord(false, true, false, "←→");
                                    changed |= ui
                                        .checkbox(
                                            &mut self.settings.nav_arrows,
                                            format!("{panes_chord} panes · {spaces_chord} spaces"),
                                        )
                                        .changed();
                                });
                                setting_row(ui, "Vim keys", |ui| {
                                    let panes_chord = format_chord(false, true, false, "J/K");
                                    let spaces_chord = format_chord(false, true, false, "H/L");
                                    changed |= ui
                                        .checkbox(
                                            &mut self.settings.nav_vim,
                                            format!("{panes_chord} panes · {spaces_chord} spaces"),
                                        )
                                        .changed();
                                });
                                setting_note(ui, "Lists (search, palette): ↑↓ always; with vim keys also ⌃J/⌃K and ⌃N/⌃P.", &palette);
                                section(ui, "Keyboard shortcuts", &palette);
                                setting_note(
                                    ui,
                                    &format!(
                                        "Click a shortcut, then press a chord with {}. ⌫ clears, esc cancels.",
                                        MOD.trim_end_matches('+')
                                    ),
                                    &palette,
                                );
                            }
                            for command in Cmd::ALL {
                                if !matches_query(command.label()) && !matches_query(command.id()) {
                                    continue;
                                }
                                setting_row(ui, command.label(), |ui| {
                                    let capturing = self.capturing == Some(command);
                                    let text = if capturing {
                                        "Press keys…".to_string()
                                    } else {
                                        let hint = self.key_hint(command);
                                        if hint.is_empty() { "—".to_string() } else { hint }
                                    };
                                    let button = egui::Button::new(
                                        RichText::new(text)
                                            .size(12.5)
                                            .color(if capturing { palette.accent } else { palette.text }),
                                    )
                                    .fill(if capturing {
                                        palette.accent.gamma_multiply(0.14)
                                    } else {
                                        palette.raised
                                    })
                                    .stroke(Stroke::new(1.0, if capturing { palette.accent } else { palette.border }))
                                    .min_size(Vec2::new(120.0, 24.0));
                                    if ui.add(button).clicked() {
                                        self.capturing = if capturing { None } else { Some(command) };
                                        self.capture_error = None;
                                    }
                                    if self.settings.keymap.binding(command) != command.default_binding()
                                        && ui.small_button("reset").on_hover_text("Back to the default shortcut").clicked()
                                    {
                                        self.settings.keymap.set(command, command.default_binding());
                                        changed = true;
                                    }
                                });
                                if self.capturing == Some(command)
                                    && let Some(error) = &self.capture_error
                                {
                                    setting_note(ui, error, &palette);
                                }
                            }
                            if matches_query("fixed spaces launch") {
                                setting_row(ui, "Fixed", |ui| {
                                    let spaces = format_chord(false, false, false, "1–9");
                                    let launch = format_chord(false, false, true, "1–6");
                                    ui.label(
                                        RichText::new(format!("{spaces} spaces · {launch} launch a CLI"))
                                            .size(12.0)
                                            .color(palette.muted),
                                    );
                                });
                            }
                        }

                        if show_tab(SettingsTab::Clis) {
                            if matches_query("cli command args path extra") {
                                section(ui, "CLI commands & arguments", &palette);
                            }
                            let mut cli_cmd_edited = false;
                            for kind in CliKind::ALL {
                                if !matches_query(kind.label()) && !matches_query(kind.command()) {
                                    continue;
                                }
                                let installed = self.cli[kind.index()].installed;
                                let label = RichText::new(kind.label()).color(kind_color(kind, &palette));
                                setting_row(ui, label, |ui| {
                                    ui.vertical(|ui| {
                                        ui.horizontal(|ui| {
                                            let cmd_entry = self.settings.cli_commands.entry(kind.id().to_string()).or_default();
                                            let cmd_edit = ui.add(
                                                egui::TextEdit::singleline(cmd_entry)
                                                    .desired_width(110.0)
                                                    .hint_text(RichText::new(kind.command()).color(palette.faint)),
                                            );
                                            if cmd_edit.changed() {
                                                changed = true;
                                                cli_cmd_edited = true;
                                            }
                                            let arg_entry = self.settings.cli_args.entry(kind.id().to_string()).or_default();
                                            changed |= ui
                                                .add(
                                                    egui::TextEdit::singleline(arg_entry)
                                                        .desired_width(140.0)
                                                        .hint_text(RichText::new("extra args").color(palette.faint)),
                                                )
                                                .changed();
                                        });
                                        ui.label(
                                            RichText::new(if installed { "installed" } else { "not found" })
                                                .size(11.0)
                                                .color(if installed { palette.green } else { palette.faint }),
                                        );
                                    });
                                });
                            }
                            self.settings.cli_args.retain(|_, value| !value.trim().is_empty());
                            self.settings.cli_commands.retain(|_, value| !value.trim().is_empty());
                            if cli_cmd_edited {
                                self.start_cli_check();
                            }
                            if matches_query("cli command args re-check") {
                                setting_note(ui, "Left: optional binary/wrapper path. Right: extra flags (e.g. --model opus).", &palette);
                                setting_row(ui, "", |ui| {
                                    if ui.add(small_button("Re-check CLIs", &palette)).clicked() {
                                        self.start_cli_check();
                                    }
                                });
                            }
                        }

                        if show_tab(SettingsTab::About) && matches_query("about version license repository settings log reset") {
                            section(ui, "About", &palette);
                            let ais = self
                                .sessions
                                .ais()
                                .map(|path| pretty_path(path, &self.home))
                                .unwrap_or_else(|| "not found (set AIS_BIN)".into());
                            for (label, value) in [
                                ("Version", format!("{APP_TITLE} {}", env!("CARGO_PKG_VERSION"))),
                                ("License", "MIT (not affiliated with Anthropic, OpenAI, or Google)".into()),
                                ("ais indexer", ais),
                                ("Settings", pretty_path(&self.settings_file, &self.home)),
                                ("Log file", pretty_path(crate::log::log_path(), &self.home)),
                            ] {
                                setting_row(ui, label, |ui| {
                                    ui.add(egui::Label::new(RichText::new(value).size(12.0).color(palette.muted)).truncate());
                                });
                            }
                            setting_row(ui, "Repository", |ui| {
                                if ui.link(env!("CARGO_PKG_REPOSITORY")).clicked() {
                                    open_external(env!("CARGO_PKG_REPOSITORY"), false);
                                }
                            });
                            setting_row(ui, "Manage", |ui| {
                                ui.horizontal(|ui| {
                                    if ui.add(small_button("Open settings file", &palette)).clicked() {
                                        self.flush_settings();
                                        open_external(&self.settings_file.to_string_lossy(), false);
                                    }
                                    let armed = self
                                        .reset_confirm_at
                                        .is_some_and(|at| at.elapsed() < Duration::from_secs(5));
                                    let label = if armed { "Confirm reset?" } else { "Reset to defaults" };
                                    if ui.add(small_button(label, &palette)).clicked() {
                                        if armed {
                                            let spaces = self.settings.open_spaces.clone();
                                            let active = self.settings.active_space;
                                            self.settings = Settings::default();
                                            self.settings.open_spaces = spaces;
                                            self.settings.active_space = active;
                                            self.reset_confirm_at = None;
                                            fonts_changed = true;
                                            style_changed = true;
                                            self.notice(NoticeKind::Info, "Settings reset to defaults");
                                        } else {
                                            self.reset_confirm_at = Some(Instant::now());
                                        }
                                    }
                                });
                            });
                        }
                    });
            });
        self.show_settings = open;
        if fonts_changed {
            self.configure_fonts(ctx);
        }
        if style_changed {
            self.configure_style(ctx);
        }
        if changed || style_changed || fonts_changed {
            self.mark_settings_dirty();
        }
    }
}

fn fuzzy_score(haystack: &str, query: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let words: Vec<&str> = query.split_whitespace().collect();
    if words.iter().all(|w| haystack.contains(w)) {
        let mut score = 1000;
        if haystack.starts_with(words[0]) {
            score += 200;
        }
        return Some(score - (haystack.len() as i32).min(200));
    }
    // Single-token subsequence fuzzy match
    let q_chars: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).collect();
    let mut qi = 0;
    let mut consecutive = 0i32;
    let mut score = 100i32;
    for ch in haystack.chars() {
        if qi < q_chars.len() && ch == q_chars[qi] {
            qi += 1;
            consecutive += 1;
            score += 10 + consecutive * 4;
        } else {
            consecutive = 0;
        }
    }
    (qi == q_chars.len()).then_some(score - (haystack.len() as i32).min(80))
}

pub fn filter_palette<'a>(
    items: &'a [PaletteItem],
    typed_item: Option<&'a PaletteItem>,
    query: &str,
) -> Vec<&'a PaletteItem> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        let mut out = Vec::new();
        if let Some(item) = typed_item {
            out.push(item);
        }
        out.extend(items.iter().take(60));
        return out;
    }
    let mut scored: Vec<(i32, usize, &'a PaletteItem)> = Vec::new();
    if let Some(item) = typed_item {
        scored.push((2000, 0, item));
    }
    for (idx, item) in items.iter().enumerate() {
        let hay = format!("{} {} {}", item.group, item.title, item.detail).to_lowercase();
        if let Some(score) = fuzzy_score(&hay, &q) {
            scored.push((score, idx + 1, item));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored.into_iter().take(60).map(|(_, _, item)| item).collect()
}

pub fn palette_rows(
    ui: &mut egui::Ui,
    filtered: &[&PaletteItem],
    selected: usize,
    scroll: bool,
    palette: &Palette,
    max_height: f32,
) -> (Option<Action>, Option<usize>) {
    let mut chosen = None;
    let mut hovered = None;
    egui::ScrollArea::vertical()
        .max_height(max_height)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            if filtered.is_empty() {
                ui.label(RichText::new("Nothing matches").color(palette.muted));
            }
            let mut last_group = "";
            for (index, item) in filtered.iter().enumerate() {
                if item.group != last_group {
                    ui.add_space(4.0);
                    ui.label(RichText::new(item.group).size(scale::FONT_XS).color(palette.faint));
                    last_group = item.group;
                }
                let is_selected = index == selected;
                let frame = egui::Frame::NONE
                    .fill(if is_selected {
                        palette.accent.gamma_multiply(0.18)
                    } else {
                        Color32::TRANSPARENT
                    })
                    .corner_radius(CornerRadius::same(7))
                    .inner_margin(Margin::symmetric(8, 5))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            if let Some(color) = item.color {
                                ui.label(RichText::new("●").size(10.0).color(color));
                            }
                            ui.label(RichText::new(&item.title).size(13.5).color(palette.text));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.add(
                                    egui::Label::new(RichText::new(&item.detail).size(11.0).color(palette.faint))
                                        .truncate(),
                                );
                            });
                        });
                    });
                let row = ui.interact(frame.response.rect, Id::new(("palette-item", index)), Sense::click());
                if is_selected && scroll {
                    row.scroll_to_me(None);
                }
                if row.hovered() && ui.input(|i| i.pointer.delta().length() > 0.0) {
                    hovered = Some(index);
                }
                if row.clicked() {
                    chosen = Some(item.action.clone());
                }
            }
        });
    (chosen, hovered)
}
