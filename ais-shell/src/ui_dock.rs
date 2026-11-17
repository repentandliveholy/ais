//! Right dock panels (Files, Usage, Alerts) and stacked toasts.

use crate::app::{Action, AisApp, NoticeKind, REVEAL_LABEL, RightTab};
use crate::cli::CliKind;
use crate::input;
use crate::keys::Command as Cmd;
use crate::theme::scale;
use crate::usage::{Limit, STALE_LIVE_AGE};
use crate::widgets::{
    UiIcon, ago, file_tint, format_size, kind_color, list_row_compact, notice_color, pretty_path, right_dock,
    right_tabs, small_button, tool_button, usage_row,
};
use eframe::egui::{self, Align, Align2, Color32, CornerRadius, Id, Layout, Margin, Rect, RichText, Stroke, Vec2};

impl AisApp {
    pub fn files_panel(&mut self, ctx: &egui::Context) {
        let palette = self.palette();
        let Some(index) = self.active_space else {
            self.dock_placeholder(ctx, RightTab::Files, "Open a project to browse its files.");
            return;
        };
        if !self.spaces[index].available {
            self.dock_placeholder(ctx, RightTab::Files, "Project folder is currently unavailable.");
            return;
        }
        let unread = self.unread;
        let show_icons = self.settings.show_icons;
        let home = self.home.clone();
        let mut navigate = None;
        let mut refresh = false;
        let mut actions = Vec::new();
        let installed: Vec<CliKind> = CliKind::ALL
            .into_iter()
            .filter(|kind| self.cli[kind.index()].installed)
            .collect();
        let space = &mut self.spaces[index];
        right_dock(ctx, &palette).show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(tab) = right_tabs(ui, RightTab::Files, unread, &palette) {
                    actions.push(Action::SetRightTab(Some(tab)));
                }
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                ui.add_enabled_ui(space.files_dir != space.path, |ui| {
                    if tool_button(ui, UiIcon::Up, "", false, show_icons, &palette)
                        .on_hover_text("Up one folder")
                        .clicked()
                        && let Some(parent) = space.files_dir.parent()
                    {
                        navigate = Some(parent.to_path_buf());
                    }
                });
                if tool_button(ui, UiIcon::Refresh, "", false, show_icons, &palette)
                    .on_hover_text("Refresh")
                    .clicked()
                {
                    refresh = true;
                }
                let relative = space
                    .files_dir
                    .strip_prefix(&space.path)
                    .ok()
                    .filter(|rel| !rel.as_os_str().is_empty())
                    .map(|rel| format!("{}/{}", space.name, rel.display()))
                    .unwrap_or_else(|| pretty_path(&space.files_dir, &home));
                ui.add_space(4.0);
                ui.add(egui::Label::new(RichText::new(relative).size(11.0).color(palette.muted)).truncate());
            });
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    refresh |= ui.checkbox(&mut space.files_show_hidden, "hidden").changed();
                    let filter = ui.add(
                        egui::TextEdit::singleline(&mut space.files_filter)
                            .hint_text(RichText::new("Filter files").size(12.5).color(palette.faint))
                            .margin(Margin::symmetric(8, 5))
                            .desired_width((ui.available_width() - 16.0).max(40.0)),
                    );
                    refresh |= filter.changed();
                });
            });
            ui.add(
                egui::Label::new(
                    RichText::new("Double-click or drag a file onto a pane to insert its path")
                        .size(scale::FONT_XS)
                        .color(palette.faint),
                )
                .truncate(),
            );
            ui.add_space(2.0);
            if let Some(error) = &space.files_error {
                ui.label(RichText::new(error).color(palette.red).size(11.0));
            }
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.set_max_width(ui.available_width() - 10.0);
                for entry in &space.files {
                    let response = list_row_compact(ui, Id::new(("file-row", &entry.path)), palette.hover, |ui| {
                        ui.horizontal(|ui| {
                            if show_icons {
                                let (r, _) = ui.allocate_exact_size(Vec2::splat(13.0), egui::Sense::hover());
                                if entry.is_dir {
                                    UiIcon::Folder.paint(ui.painter(), r, palette.accent);
                                } else {
                                    UiIcon::Files.paint(ui.painter(), r, file_tint(&entry.name, &palette));
                                }
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if !entry.is_dir {
                                    ui.label(RichText::new(format_size(entry.size)).size(10.0).color(palette.faint));
                                }
                                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                    ui.add(
                                        egui::Label::new(RichText::new(&entry.name).size(12.5).color(
                                            if entry.is_dir {
                                                palette.text
                                            } else {
                                                palette.text.gamma_multiply(0.85)
                                            },
                                        ))
                                        .truncate(),
                                    );
                                });
                            });
                        });
                    });
                    let rel = entry
                        .path
                        .strip_prefix(&space.path)
                        .map(|rel| rel.to_string_lossy().into_owned())
                        .unwrap_or_else(|_| entry.path.to_string_lossy().into_owned());
                    if response.clicked() && entry.is_dir {
                        navigate = Some(entry.path.clone());
                    }
                    if response.double_clicked() && !entry.is_dir {
                        actions.push(Action::InsertText(format!("{} ", input::shell_quote(&rel))));
                    }
                    let path = entry.path.clone();
                    let response = response.on_hover_text(path.display().to_string());
                    response.context_menu(|ui| {
                        if entry.is_dir {
                            if ui.button("Open as project space").clicked() {
                                actions.push(Action::OpenProject(path.clone()));
                                ui.close_menu();
                            }
                            if ui.button("Terminal here").clicked() {
                                actions.push(Action::ShellIn(path.clone()));
                                ui.close_menu();
                            }
                            ui.menu_button("Start CLI here", |ui| {
                                for kind in &installed {
                                    if ui.button(kind.label()).clicked() {
                                        actions.push(Action::LaunchIn(*kind, path.clone()));
                                        ui.close_menu();
                                    }
                                }
                            });
                            ui.separator();
                        }
                        if ui
                            .button("Insert @mention")
                            .on_hover_text("For Claude / Codex / Gemini prompts")
                            .clicked()
                        {
                            actions.push(Action::InsertText(format!("{} ", input::mention_quote(&rel))));
                            ui.close_menu();
                        }
                        if ui.button("Insert path").clicked() {
                            actions.push(Action::InsertText(format!("{} ", input::shell_quote(&rel))));
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Copy full path").clicked() {
                            actions.push(Action::CopyText(path.to_string_lossy().into_owned(), "path"));
                            ui.close_menu();
                        }
                        if ui.button(REVEAL_LABEL).clicked() {
                            actions.push(Action::Reveal(path.clone()));
                            ui.close_menu();
                        }
                    });
                }
            });
        });
        if let Some(path) = navigate
            && path.starts_with(&space.path)
        {
            space.files_dir = path;
            space.files_filter.clear();
            refresh = true;
        }
        if refresh {
            space.files_dirty = true;
        }
        self.actions.extend(actions);
    }

    fn dock_placeholder(&mut self, ctx: &egui::Context, current: RightTab, hint: &str) {
        let palette = self.palette();
        let mut picked = None;
        right_dock(ctx, &palette).show(ctx, |ui| {
            ui.horizontal(|ui| picked = right_tabs(ui, current, self.unread, &palette));
            ui.add_space(16.0);
            ui.label(RichText::new(hint).color(palette.muted));
        });
        if let Some(tab) = picked {
            self.actions.push(Action::SetRightTab(Some(tab)));
        }
    }

    pub fn usage_panel(&mut self, ctx: &egui::Context) {
        let palette = self.palette();
        let show_icons = self.settings.show_icons;
        let auto_live = self.settings.auto_live_usage;
        let mut actions = Vec::new();
        right_dock(ctx, &palette).show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(tab) = right_tabs(ui, RightTab::Usage, self.unread, &palette) {
                    actions.push(Action::SetRightTab(Some(tab)));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if self.usage.loading_live {
                        ui.add(egui::Spinner::new().size(13.0));
                    } else if tool_button(ui, UiIcon::Refresh, "", false, show_icons, &palette)
                        .on_hover_text("Ask the official CLIs for current limits")
                        .clicked()
                    {
                        actions.push(Action::RefreshUsage);
                    }
                });
            });
            let state = if self.usage.loading_live {
                "checking live limits…".to_string()
            } else if let Some(at) = self.usage.live_at
                && self.usage.report.as_ref().is_some_and(|r| r.live)
            {
                if at.elapsed() > STALE_LIVE_AGE {
                    format!("live · checked {} (stale)", ago(at.elapsed()))
                } else {
                    format!("live · checked {}", ago(at.elapsed()))
                }
            } else {
                "cached snapshot".to_string()
            };
            ui.add_space(4.0);
            ui.label(RichText::new(state).size(11.0).color(palette.faint));
            ui.add_space(6.0);
            if !self.usage.available() {
                ui.label(
                    RichText::new("Usage needs the ais launcher (set AIS_BIN or put ais on PATH).")
                        .color(palette.muted),
                );
                return;
            }
            if let Some(error) = self.usage.display_error() {
                ui.label(RichText::new(error).size(scale::FONT_SM).color(palette.red));
            }
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.set_max_width(ui.available_width() - 12.0);
                let Some(report) = self.usage.report.clone() else {
                    if self.usage.loading_cache || self.usage.loading_live {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(RichText::new("Loading…").color(palette.muted));
                        });
                    } else {
                        ui.label(RichText::new("No cached usage snapshot yet — click ↻ to refresh.").color(palette.muted));
                    }
                    return;
                };
                // One group per CLI account: two Claude logins never share a block.
                let mut groups: Vec<(Option<CliKind>, Vec<&Limit>)> = Vec::new();
                for limit in &report.limits {
                    match groups
                        .iter_mut()
                        .find(|(kind, list)| *kind == limit.client && list[0].account == limit.account)
                    {
                        Some((_, list)) => list.push(limit),
                        None => groups.push((limit.client, vec![limit])),
                    }
                }
                if groups.is_empty() {
                    ui.label(RichText::new("No provider reported limits yet.").color(palette.muted));
                }
                for (kind, limits) in groups {
                    let (title, color) = match kind {
                        Some(kind) => (kind.label().to_string(), kind_color(kind, &palette)),
                        None => ("Other".to_string(), palette.text),
                    };
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(title).strong().size(scale::FONT_LG).color(color));
                        if let Some(source) = limits.first().map(|limit| limit.source.clone()) {
                            ui.label(RichText::new(source).size(scale::FONT_XS).color(palette.faint));
                        }
                    });
                    if let Some(account) = limits.first().map(|limit| limit.account_title.clone())
                        && !account.is_empty()
                    {
                        ui.label(RichText::new(account).size(scale::FONT_XS).color(palette.muted));
                    }
                    for limit in limits {
                        usage_row(ui, limit, &palette);
                    }
                    ui.add_space(6.0);
                    ui.separator();
                }
                for error in &report.errors {
                    ui.label(RichText::new(error).size(11.0).color(palette.red));
                }
                ui.add_space(6.0);
                let note = if auto_live {
                    "Read through official CLIs (codex, agy, claude). Background live refresh is enabled in Settings."
                } else {
                    "Shows cached snapshots automatically. Click ↻ for a live check, or enable background live refresh in Settings."
                };
                ui.label(RichText::new(note).size(scale::FONT_XS).color(palette.faint));
            });
        });
        self.actions.extend(actions);
    }

    pub fn notices_panel(&mut self, ctx: &egui::Context) {
        let palette = self.palette();
        let mut actions = Vec::new();
        self.unread = 0;
        right_dock(ctx, &palette).show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(tab) = right_tabs(ui, RightTab::Notices, 0, &palette) {
                    actions.push(Action::SetRightTab(Some(tab)));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if !self.notices.is_empty()
                        && ui
                            .add(small_button("Clear", &palette))
                            .on_hover_text("Remove all alerts")
                            .clicked()
                    {
                        actions.push(Action::ClearNotices);
                    }
                });
            });
            ui.label(
                RichText::new(format!("{} events this session", self.notices.len()))
                    .size(11.0)
                    .color(palette.faint),
            );
            ui.add_space(6.0);
            if self.notices.is_empty() {
                ui.add_space(24.0);
                ui.vertical_centered(|ui| {
                    if self.settings.show_icons {
                        let (r, _) = ui.allocate_exact_size(Vec2::splat(22.0), egui::Sense::hover());
                        UiIcon::Bell.paint(ui.painter(), r, palette.faint);
                    }
                    ui.add_space(4.0);
                    ui.label(RichText::new("No alerts yet").color(palette.muted));
                    ui.label(
                        RichText::new("Finished turns, bells, errors and launches show up here.")
                            .size(11.0)
                            .color(palette.faint),
                    );
                });
                return;
            }
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.set_max_width(ui.available_width() - 12.0);
                for notice in self.notices.iter().rev() {
                    let color = notice_color(notice.kind, &palette);
                    let frame = egui::Frame::NONE
                        .fill(palette.raised)
                        .stroke(Stroke::new(1.0, palette.border))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin {
                            left: 12,
                            right: 9,
                            top: 6,
                            bottom: 7,
                        })
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(notice_label(notice.kind))
                                        .size(scale::FONT_XS)
                                        .strong()
                                        .color(color),
                                );
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.label(
                                        RichText::new(ago(notice.at.elapsed()))
                                            .size(scale::FONT_XS)
                                            .color(palette.faint),
                                    );
                                });
                            });
                            ui.add(egui::Label::new(RichText::new(&notice.text).size(12.5).color(palette.text)).wrap());
                        });
                    let rect = frame.response.rect;
                    ui.painter().rect_filled(
                        Rect::from_min_size(rect.min + Vec2::new(0.0, 7.0), Vec2::new(3.0, rect.height() - 14.0)),
                        CornerRadius::same(2),
                        color,
                    );
                    ui.add_space(4.0);
                }
            });
        });
        self.actions.extend(actions);
    }

    pub fn toast(&mut self, ctx: &egui::Context) {
        let palette = self.palette();
        let active: Vec<(&crate::app::Notice, f32)> = self
            .notices
            .iter()
            .rev()
            .filter_map(|notice| {
                let ttl = if matches!(notice.kind, NoticeKind::Error | NoticeKind::Attention) {
                    6.0
                } else {
                    3.0
                };
                let age = notice.at.elapsed().as_secs_f32();
                (age <= ttl).then(|| (notice, ((ttl - age) / 0.5).clamp(0.0, 1.0)))
            })
            .take(3)
            .collect();
        if active.is_empty() {
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
        let shadow_color = Color32::from_black_alpha(if palette.light { 36 } else { 90 });
        egui::Area::new(Id::new("toast-stack"))
            .anchor(Align2::RIGHT_BOTTOM, Vec2::new(-16.0, -38.0))
            .interactable(false)
            .show(ctx, |ui| {
                ui.with_layout(Layout::bottom_up(Align::Max), |ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    for (notice, fade) in active {
                        ui.scope(|ui| {
                            ui.set_opacity(fade);
                            let color = notice_color(notice.kind, &palette);
                            egui::Frame::NONE
                                .fill(palette.raised)
                                .stroke(Stroke::new(1.0, color.gamma_multiply(0.7)))
                                .corner_radius(CornerRadius::same(10))
                                .shadow(egui::Shadow {
                                    offset: [0, 6],
                                    blur: 18,
                                    spread: 0,
                                    color: shadow_color,
                                })
                                .inner_margin(Margin::symmetric(14, 9))
                                .show(ui, |ui| {
                                    ui.set_max_width(420.0);
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new(notice_glyph(notice.kind)).size(10.0).color(color));
                                        ui.label(RichText::new(&notice.text).size(scale::FONT_MD));
                                    });
                                });
                        });
                    }
                });
            });
    }

    pub fn pending_paste_modal(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending_paste.as_ref() else {
            return;
        };
        let palette = self.palette();
        let uid = pending.pane_uid;
        let text = pending.text.clone();
        let lines = pending.lines;
        let preview = crate::sessions::truncate(&text.replace('\n', " ↵ "), 140);
        let mut decision: Option<bool> = None;
        egui::Modal::new(Id::new("multiline-paste-modal"))
            .backdrop_color(Color32::from_black_alpha(if palette.light { 60 } else { 140 }))
            .frame(
                egui::Frame::NONE
                    .fill(palette.panel)
                    .stroke(Stroke::new(1.0, palette.border_strong))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(16)),
            )
            .show(ctx, |ui| {
                ui.set_max_width(420.0);
                ui.label(
                    RichText::new(format!("Paste {lines} lines into terminal?"))
                        .strong()
                        .size(scale::FONT_LG),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new("Bracketed paste is off in this program, so newlines will execute immediately.")
                        .size(scale::FONT_SM)
                        .color(palette.muted),
                );
                ui.add_space(8.0);
                egui::Frame::NONE
                    .fill(palette.raised)
                    .stroke(Stroke::new(1.0, palette.border))
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::same(8))
                    .show(ui, |ui| {
                        ui.label(RichText::new(preview).monospace().size(11.5).color(palette.muted));
                    });
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.add(small_button("Paste anyway", &palette)).clicked() {
                        decision = Some(true);
                    }
                    if ui.add(small_button("Cancel", &palette)).clicked() {
                        decision = Some(false);
                    }
                });
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            decision = Some(false);
        }
        if let Some(confirm) = decision {
            self.pending_paste = None;
            if confirm && let Some((space_index, pane_index)) = self.find_pane(uid) {
                self.spaces[space_index].panes[pane_index].paste(&text);
                self.pending_focus = Some(uid);
            }
        }
    }
}

fn notice_label(kind: NoticeKind) -> &'static str {
    match kind {
        NoticeKind::Info => "INFO",
        NoticeKind::Success => "DONE",
        NoticeKind::Error => "ERROR",
        NoticeKind::Attention => "NEEDS YOU",
    }
}

fn notice_glyph(kind: NoticeKind) -> &'static str {
    match kind {
        NoticeKind::Info => "●",
        NoticeKind::Success => "✓",
        NoticeKind::Error => "!",
        NoticeKind::Attention => "▲",
    }
}

#[allow(dead_code)]
fn _unused_cmd_ref() -> Cmd {
    Cmd::Search
}
