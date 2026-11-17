//! Left/right sidebar: Projects and Sessions tabs.

use crate::app::{Action, AisApp, REVEAL_LABEL, Target, side_width};
use crate::keys::Command as Cmd;
use crate::sessions::truncate;
use crate::theme::{Palette, scale};
use crate::widgets::{UiIcon, kind_color, list_row, session_row, tabs, tool_button};
use eframe::egui::{self, Align, Layout, Margin, RichText, Stroke};
use std::path::Path;
use std::time::Duration;

impl AisApp {
    pub fn sidebar(&mut self, ctx: &egui::Context) {
        let palette = self.palette();
        let (default_width, range) = side_width(ctx, 290.0);
        let side = if self.settings.swap_sides {
            egui::panel::Side::Right
        } else {
            egui::panel::Side::Left
        };
        let show_icons = self.settings.show_icons;
        egui::SidePanel::new(side, "sidebar")
            .resizable(true)
            .default_width(default_width)
            .width_range(range)
            .frame(
                egui::Frame::NONE
                    .fill(palette.panel)
                    .inner_margin(Margin::symmetric(10, 10))
                    .stroke(Stroke::new(1.0, palette.border)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let sessions = self.settings.sidebar_sessions;
                    if let Some(index) = tabs(ui, &["Projects", "Sessions"], sessions as usize, &palette) {
                        self.actions.push(Action::ShowSessions(index == 1));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if tool_button(ui, UiIcon::Refresh, "", false, show_icons, &palette)
                            .on_hover_text("Rescan history")
                            .clicked()
                        {
                            self.actions.push(Action::RefreshProjects);
                            self.actions.push(Action::RefreshSessions);
                        }
                    });
                });
                ui.add_space(6.0);
                if self.settings.sidebar_sessions {
                    self.sessions_list(ui, &palette);
                } else {
                    self.projects_list(ui, &palette);
                }
            });
    }

    fn projects_list(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        ui.add(
            egui::TextEdit::singleline(&mut self.project_search)
                .hint_text(
                    RichText::new(format!("Filter {} projects", self.projects.len()))
                        .size(12.5)
                        .color(palette.faint),
                )
                .margin(Margin::symmetric(8, 5))
                .desired_width(f32::INFINITY),
        );
        ui.add_space(6.0);
        let query = self.project_search.trim().to_lowercase();
        let active_path = self.active_space.map(|i| self.spaces[i].path.clone());
        let mut hovered_project = None;
        let show_icons = self.settings.show_icons;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if self.scanning && self.projects.is_empty() {
                ui.add_space(20.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Scanning local AI history…").color(palette.muted));
                });
            }
            if !self.scanning && self.projects.is_empty() {
                ui.add_space(20.0);
                ui.label(
                    RichText::new(format!(
                        "No AI CLI history found yet.\nOpen any folder with {} and start a CLI.",
                        self.key_hint(Cmd::OpenFolder)
                    ))
                    .color(palette.muted),
                );
            }
            for project in &self.projects {
                let pretty = project.pretty_path(&self.home);
                if !query.is_empty()
                    && !project.name().to_lowercase().contains(&query)
                    && !pretty.to_lowercase().contains(&query)
                {
                    continue;
                }
                let is_active = active_path.as_ref() == Some(&project.path);
                let is_open = self.spaces.iter().any(|space| space.path == project.path);
                let response = list_row(
                    ui,
                    egui::Id::new(("project-row", &project.path)),
                    is_active,
                    palette,
                    |ui| {
                        ui.horizontal(|ui| {
                            if show_icons {
                                let (icon_rect, _) =
                                    ui.allocate_exact_size(egui::Vec2::splat(13.0), egui::Sense::hover());
                                let icon = if is_open { UiIcon::FolderOpen } else { UiIcon::Folder };
                                icon.paint(
                                    ui.painter(),
                                    icon_rect,
                                    if is_open { palette.accent } else { palette.muted },
                                );
                            }
                            let name = RichText::new(project.name()).strong().size(13.5).color(if is_open {
                                palette.text
                            } else {
                                palette.text.gamma_multiply(0.9)
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(RichText::new(project.age()).size(11.0).color(palette.faint));
                                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                    ui.add(egui::Label::new(name).truncate());
                                });
                            });
                        });
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = egui::Vec2::new(7.0, 1.0);
                            for (kind, count) in &project.clients {
                                ui.label(
                                    RichText::new(format!("{} {count}", kind.label()))
                                        .size(scale::FONT_XS)
                                        .color(kind_color(*kind, palette)),
                                );
                            }
                        });
                    },
                );
                let response = response.on_hover_text(&pretty);
                if response.hovered() {
                    hovered_project = Some(project.path.clone());
                }
                if response.clicked() {
                    self.actions.push(Action::OpenProject(project.path.clone()));
                }
                let path = project.path.clone();
                response.context_menu(|ui| {
                    if ui.button("Open space").clicked() {
                        self.actions.push(Action::OpenProject(path.clone()));
                        ui.close_menu();
                    }
                    if ui.button("Copy path").clicked() {
                        self.actions
                            .push(Action::CopyText(path.to_string_lossy().into_owned(), "path"));
                        ui.close_menu();
                    }
                    if ui.button(REVEAL_LABEL).clicked() {
                        self.actions.push(Action::Reveal(path.clone()));
                        ui.close_menu();
                    }
                });
            }
        });
        if let Some(path) = hovered_project {
            self.sessions.preload([path.as_path()]);
        }
    }

    fn sessions_list(&mut self, ui: &mut egui::Ui, palette: &Palette) {
        let Some(space_index) = self.active_space else {
            ui.label(RichText::new("Open a project to see its conversations.").color(palette.muted));
            return;
        };
        if !self.spaces[space_index].available {
            ui.label(RichText::new("Folder is currently unavailable.").color(palette.red));
            return;
        }
        let path = self.spaces[space_index].path.clone();
        self.sessions.ensure(&path, Duration::from_secs(60));
        ui.horizontal(|ui| {
            ui.label(RichText::new(&self.spaces[space_index].name).strong());
            if self.sessions.get(&path).is_some_and(|list| list.loading) {
                ui.add(egui::Spinner::new().size(12.0));
            }
        });
        ui.add(
            egui::TextEdit::singleline(&mut self.session_search)
                .hint_text(RichText::new("Filter conversations").size(12.5).color(palette.faint))
                .margin(Margin::symmetric(8, 5))
                .desired_width(f32::INFINITY),
        );
        ui.add_space(6.0);
        let query = self.session_search.trim().to_lowercase();
        let Some(list) = self.sessions.get(&path) else {
            return;
        };
        if let Some(error) = &list.error {
            ui.label(RichText::new(error).size(scale::FONT_SM).color(palette.red));
        }
        if list.sessions.is_empty() && !list.loading && list.error.is_none() {
            ui.label(RichText::new("No saved conversations in this folder.").color(palette.muted));
        }
        let ais = self.sessions.ais().map(Path::to_path_buf);
        let mut actions = Vec::new();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for session in &list.sessions {
                if !query.is_empty()
                    && !session.title.to_lowercase().contains(&query)
                    && !session.client.label().to_lowercase().contains(&query)
                    && !session.id.starts_with(&query)
                {
                    continue;
                }
                let response = session_row(ui, session, palette);
                let response = response.on_hover_ui(|ui| {
                    ui.set_max_width(360.0);
                    ui.label(RichText::new(&session.title).strong());
                    if !session.last_user.is_empty() {
                        ui.label(RichText::new(truncate(&session.last_user, 240)).color(palette.muted));
                    }
                    let mut meta = vec![session.client.label().to_string(), session.short_id().to_string()];
                    if !session.model.is_empty() {
                        meta.push(session.model.clone());
                    }
                    if let Some(count) = session.messages {
                        meta.push(format!("{count} messages"));
                    }
                    ui.label(RichText::new(meta.join(" · ")).size(11.0).color(palette.faint));
                    ui.label(
                        RichText::new("Click to resume · right-click for more")
                            .size(11.0)
                            .color(palette.faint),
                    );
                });
                if response.clicked() {
                    actions.push(Action::Resume(Box::new(session.clone()), Target::Auto));
                }
                response.context_menu(|ui| {
                    if ui.button("Resume here").clicked() {
                        actions.push(Action::Resume(Box::new(session.clone()), Target::Auto));
                        ui.close_menu();
                    }
                    if ui.button("Resume in new split").clicked() {
                        actions.push(Action::Resume(Box::new(session.clone()), Target::Split));
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Copy session ID").clicked() {
                        actions.push(Action::CopyText(session.id.clone(), "session ID"));
                        ui.close_menu();
                    }
                    if let Some(command) = session.command_line(&path, ais.as_deref())
                        && ui.button("Copy resume command").clicked()
                    {
                        actions.push(Action::CopyText(command, "resume command"));
                        ui.close_menu();
                    }
                });
            }
        });
        self.actions.extend(actions);
    }
}
