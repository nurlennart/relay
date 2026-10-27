//! Settings, themes, and SSH snippets.
use super::*;
use egui::{Align, Align2, FontId, Layout, Margin, Sense, UiBuilder};

impl App {
    pub(super) fn settings(&mut self, ctx: &egui::Context) {
        if !matches!(self.modal, Modal::Settings) {
            return;
        }
        let colors = theme::palette(ctx);
        let mut choice = theme::current(ctx);
        let before = choice;
        let mut close = false;
        let mut manage = false;
        let mut import = None;
        let mut export = false;
        let mut recovery = false;
        let mut copied_version = None;
        dialog("settings", ctx).show(ctx, |ui| {
            ui.set_width(dialog_width(ctx, 540.));
            ui.horizontal(|ui| {
                ui.label(RichText::new("Settings").size(TITLE_SIZE).strong());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    close = icon_button(ui, Icon::Close, 26., "Close (Esc)").clicked();
                    let version = version();
                    if ui
                        .add(
                            egui::Label::new(
                                RichText::new(&version).size(12.).color(colors.muted),
                            )
                            .sense(Sense::click()),
                        )
                        .on_hover_text("Click to copy")
                        .clicked()
                    {
                        ui.ctx().copy_text(version.clone());
                        copied_version = Some(version);
                    }
                });
            });
            egui::ScrollArea::vertical()
                .id_salt("settings-body")
                .min_scrolled_height((ctx.content_rect().height() - 180.).clamp(200., 900.))
                .max_height((ctx.content_rect().height() - 180.).clamp(200., 900.))
                .show(ui, |ui| {
                    ui.add_space(8.);
                    section_label(ui, "APPEARANCE");
                    ui.label(
                        RichText::new("Choose a workspace theme. Saved on this computer.")
                            .color(colors.muted),
                    );
                    ui.add_space(4.);
                    for pair in theme::Theme::ALL.chunks(2) {
                        ui.columns(2, |columns| {
                            for (column, &candidate) in pair.iter().enumerate() {
                                if theme_option(
                                    &mut columns[column],
                                    candidate,
                                    choice == candidate,
                                )
                                .clicked()
                                {
                                    choice = candidate;
                                }
                            }
                        });
                    }
                    ui.add_space(14.);
                    ui.separator();
                    section_label(ui, "SESSIONS");
                    let mut reopen = !self.state.forget_tabs;
                    if ui
                        .checkbox(&mut reopen, "Reopen tabs at startup")
                        .changed()
                    {
                        self.state.forget_tabs = !reopen;
                        self.remember_tabs();
                    }
                    ui.label(
                        RichText::new(
                            "Tabs open when Relay closes come back, and each connects when you \
                             first select it. Turned off, Relay starts with only the library \
                             and keeps no list of your tabs.",
                        )
                        .size(12.)
                        .color(colors.muted),
                    );
                    ui.add_space(14.);
                    ui.separator();
                    section_label(ui, "CONNECTION LIBRARY");
                    ui.label(RichText::new("Your connections are stored in").color(colors.muted));
                    ui.add_space(2.);
                    let (detail, facts) = self.library_summary();
                    // The button comes first so the row takes its height and centers the name.
                    ui.horizontal(|ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            manage = ui
                                .add_enabled(
                                    self.pending.is_none(),
                                    egui::Button::new("Switch library…"),
                                )
                                .on_hover_text("Personal, a shared SQLite file, MSSQL, or MySQL")
                                .clicked();
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                let (dot, _) =
                                    ui.allocate_exact_size(Vec2::splat(8.), Sense::hover());
                                ui.painter().circle_filled(dot.center(), 3.5, colors.accent);
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(self.location.label()).strong(),
                                    )
                                    .truncate(),
                                );
                            });
                        });
                    });
                    if !detail.is_empty() {
                        ui.add(
                            egui::Label::new(
                                RichText::new(&detail).monospace().size(12.).color(colors.text),
                            )
                            .truncate(),
                        )
                        .on_hover_text(&detail);
                    }
                    ui.label(RichText::new(facts).size(12.).color(colors.muted));
                    ui.add_space(14.);
                    ui.separator();
                    section_label(ui, "IMPORT & EXPORT");
                    ui.label(
                        RichText::new("Bring in connections from CSV, your SSH config, or .rdp files. Export as CSV.")
                            .color(colors.muted),
                    );
                    ui.add_space(4.);
                    let button_width = (ui.available_width() - ui.spacing().item_spacing.x) / 2.;
                    let idle = self.pending.is_none();
                    ui.add_enabled_ui(idle, |ui| {
                        ui.horizontal(|ui| {
                            if outline_button(ui, "Import CSV", button_width).clicked() {
                                import = Some(Source::Csv);
                            }
                            if outline_button(ui, "Import SSH config", button_width)
                                .on_hover_text("Reads ~/.ssh/config")
                                .clicked()
                            {
                                import = Some(Source::SshConfig);
                            }
                        });
                    });
                    ui.horizontal(|ui| {
                        ui.add_enabled_ui(idle, |ui| {
                            if outline_button(ui, "Import .rdp files", button_width).clicked() {
                                import = Some(Source::RdpFiles);
                            }
                        });
                        export = outline_button(ui, "Export CSV", button_width).clicked();
                    });
                    if self.recovery.is_some() {
                        recovery = ui.button("Export unsaved changes").clicked();
                    }
                    ui.add_space(14.);
                    ui.separator();
                    section_label(ui, "SNIPPETS");
                    ui.label(
                        RichText::new(
                            "Saved commands for SSH sessions. Press Ctrl+Shift+P in a terminal.",
                        )
                        .color(colors.muted),
                    );
                    ui.add_space(4.);
                    self.snippet_settings(ui);
                    if let Some(error) = &self.error {
                        ui.colored_label(ui.visuals().error_fg_color, error);
                    }
                    if let Some((notice, _)) = &self.notice {
                        ui.colored_label(colors.accent, notice);
                    }
                });
        });
        if choice != before {
            choice.apply(ctx);
            #[cfg(feature = "ui-check")]
            let preview = std::env::var_os("RELAY_UI_CHECK").is_some();
            #[cfg(not(feature = "ui-check"))]
            let preview = false;
            if !preview && let Err(error) = choice.save(&theme::path()) {
                self.error = Some(format!(
                    "Theme applied, but could not save preference: {error}"
                ));
            }
        }
        if let Some(version) = copied_version {
            self.notify(format!("Copied {version}"));
        }
        if close || ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.modal = Modal::None;
        }
        if manage {
            self.modal = Modal::Library;
        }
        // Each import replaces settings with its preview once read.
        match import {
            Some(Source::Csv) => self.import_csv(),
            Some(Source::SshConfig) => self.import_ssh_config(),
            Some(Source::RdpFiles) => self.import_rdp_files(),
            None => {}
        }
        if export {
            self.export_csv(false);
        }
        if recovery {
            self.export_csv(true);
        }
    }

    /// Where the open library is, and what to know about it: who shares it, whether it is
    /// encrypted, and whether it opens again when Relay starts.
    fn library_summary(&self) -> (String, String) {
        let detail = self.location.detail();
        let facts = match &self.location {
            database::Location::Local(_) => "On this computer only. Opens at startup.".into(),
            database::Location::Sqlite(_) => "A shared file. Reopens at startup.".into(),
            database::Location::Mssql(server) | database::Location::Mysql(server) => {
                let tls = if server.tls { "TLS on" } else { "TLS off" };
                let startup = if self.server_remembered {
                    "Reopens at startup."
                } else {
                    "Not remembered, so Relay starts with Personal next time."
                };
                format!("Shared with your team · {tls} · {startup}")
            }
        };
        (detail, facts)
    }

    /// Lists, adds, edits, and deletes snippets inside Settings.
    pub(super) fn snippet_settings(&mut self, ui: &mut egui::Ui) {
        let colors = theme::palette(ui.ctx());
        if let Some((index, draft)) = &mut self.snippet_draft {
            field(ui, "Name", &mut draft.name, "e.g. Tail syslog");
            labeled(ui, "Command", |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut draft.command)
                        .font(egui::TextStyle::Monospace)
                        .desired_rows(3)
                        .desired_width(f32::INFINITY)
                        .margin(Margin::symmetric(11, 8))
                        .hint_text(RichText::new("tail -f /var/log/syslog").weak()),
                );
            });
            ui.checkbox(
                &mut draft.run,
                "Run it: press Enter after typing the command",
            );
            let ready = !draft.name.trim().is_empty() && !draft.command.trim().is_empty();
            let (index, draft) = (*index, draft.clone());
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(ready, egui::Button::new("Save snippet"))
                    .clicked()
                {
                    let snippet = state::Snippet {
                        name: draft.name.trim().into(),
                        ..draft
                    };
                    match index {
                        Some(i) => self.state.snippets[i] = snippet,
                        None => self.state.snippets.push(snippet),
                    }
                    self.snippet_draft = None;
                    self.save_state();
                }
                if ui.button("Cancel").clicked() {
                    self.snippet_draft = None;
                }
            });
            return;
        }
        let mut edit = None;
        let mut delete = None;
        for (i, snippet) in self.state.snippets.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.small_button("Delete").clicked() {
                        delete = Some(i);
                    }
                    if ui.small_button("Edit").clicked() {
                        edit = Some(i);
                    }
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.label(RichText::new(&snippet.name).strong());
                        ui.add(
                            egui::Label::new(
                                RichText::new(snippet.command.lines().next().unwrap_or(""))
                                    .monospace()
                                    .size(12.)
                                    .color(colors.muted),
                            )
                            .truncate(),
                        );
                    });
                });
            });
        }
        if let Some(i) = edit {
            self.snippet_draft = Some((Some(i), self.state.snippets[i].clone()));
        }
        if let Some(i) = delete {
            self.state.snippets.remove(i);
            self.save_state();
        }
        if outline_button(ui, "Add snippet", 140.).clicked() {
            self.snippet_draft = Some((
                None,
                state::Snippet {
                    run: true,
                    ..Default::default()
                },
            ));
        }
    }

    /// A searchable list of snippets over the active SSH tab; Enter types the chosen one.
    pub(super) fn snippet_palette(&mut self, ctx: &egui::Context) {
        let Some(mut palette) = self.palette.take() else {
            return;
        };
        let index = self
            .active
            .filter(|i| self.sessions.get(*i).is_some_and(|s| s.terminal.is_some()));
        let Some(index) = index.filter(|_| !self.covered()) else {
            return;
        };
        let colors = theme::palette(ctx);
        let matches = state::matching(&self.state.snippets, &palette.query);
        let (down, up, enter, escape) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            )
        });
        let last = matches.len().saturating_sub(1);
        palette.selected = if down {
            (palette.selected + 1).min(last)
        } else if up {
            palette.selected.saturating_sub(1)
        } else {
            palette.selected.min(last)
        };
        let mut chosen = if enter {
            matches.get(palette.selected).copied()
        } else {
            None
        };
        let mut manage = false;
        let width = dialog_width(ctx, 540.);
        let search_id = egui::Id::new("snippet-search");
        let area = egui::Area::new(egui::Id::new("snippet-palette"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_TOP, Vec2::new(0., 110.))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(colors.panel)
                    .stroke(Stroke::new(1_f32, colors.line))
                    .corner_radius(RADIUS_DIALOG)
                    .inner_margin(12.)
                    .shadow(ui.visuals().window_shadow)
                    .show(ui, |ui| {
                        ui.set_width(width);
                        let search = search_field(
                            ui,
                            &mut palette.query,
                            "Search snippets",
                            "",
                            search_id,
                            width,
                        );
                        if !palette.focused {
                            search.request_focus();
                            palette.focused = true;
                        }
                        if self.state.snippets.is_empty() {
                            ui.label(
                                RichText::new("No snippets yet. Add your first one in Settings.")
                                    .color(colors.muted),
                            );
                        } else if matches.is_empty() {
                            ui.label(RichText::new("No snippets match.").color(colors.muted));
                        }
                        ui.spacing_mut().item_spacing.y = 2.;
                        for (row, &i) in matches.iter().enumerate().take(8) {
                            let snippet = &self.state.snippets[i];
                            let (rect, response) =
                                ui.allocate_exact_size(Vec2::new(width, 42.), Sense::click());
                            if response.hovered() {
                                palette.selected = row;
                            }
                            if response.clicked() {
                                chosen = Some(i);
                            }
                            if row == palette.selected {
                                ui.painter().rect_filled(rect, RADIUS, colors.selected);
                            }
                            let mut text = ui.new_child(
                                UiBuilder::new()
                                    .max_rect(rect.shrink2(Vec2::new(10., 5.)))
                                    .layout(Layout::top_down(Align::LEFT)),
                            );
                            text.spacing_mut().item_spacing.y = 2.;
                            text.horizontal(|ui| {
                                ui.label(RichText::new(&snippet.name).strong());
                                ui.label(
                                    RichText::new(if snippet.run { "runs" } else { "types" })
                                        .size(11.)
                                        .color(colors.muted),
                                );
                            });
                            text.add(
                                egui::Label::new(
                                    RichText::new(snippet.command.lines().next().unwrap_or(""))
                                        .monospace()
                                        .size(12.)
                                        .color(colors.muted),
                                )
                                .truncate(),
                            );
                        }
                        ui.add_space(8.);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("Arrow keys select · Enter sends · Esc closes")
                                    .size(11.)
                                    .color(colors.muted),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                manage = ui.small_button("Manage…").clicked();
                            });
                        });
                    });
            });
        let terminal = self.sessions[index].terminal.as_mut().unwrap();
        if let Some(i) = chosen {
            let snippet = &self.state.snippets[i];
            terminal.send_text(&snippet.command, snippet.run);
            terminal.focus();
            return;
        }
        if escape || area.response.clicked_elsewhere() {
            terminal.focus();
            return;
        }
        if manage {
            self.modal = Modal::Settings;
            return;
        }
        self.palette = Some(palette);
    }
}

/// Where Settings imports connections from.
pub(super) enum Source {
    Csv,
    SshConfig,
    RdpFiles,
}

/// Relay's version, with the commit it was built from when that is known, such as
/// `Relay 0.4.0 · a0bb71f`.
pub(super) fn version() -> String {
    match option_env!("RELAY_COMMIT") {
        Some(commit) => format!("Relay {} · {commit}", env!("CARGO_PKG_VERSION")),
        None => format!("Relay {}", env!("CARGO_PKG_VERSION")),
    }
}

pub(super) fn theme_option(
    ui: &mut egui::Ui,
    candidate: theme::Theme,
    selected: bool,
) -> egui::Response {
    let colors = theme::palette(ui.ctx());
    let response = ui.add_sized(
        [ui.available_width(), 34.],
        egui::Button::new("").selected(selected),
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), candidate.name())
    });
    let center = response.rect.center();
    ui.painter().circle_filled(
        egui::pos2(response.rect.left() + 16., center.y),
        5.,
        candidate.colors().accent,
    );
    ui.painter().text(
        egui::pos2(response.rect.left() + 30., center.y),
        Align2::LEFT_CENTER,
        candidate.name(),
        FontId::proportional(13.),
        colors.text,
    );
    if selected {
        ui.painter().text(
            egui::pos2(response.rect.right() - 12., center.y),
            Align2::RIGHT_CENTER,
            "Selected",
            FontId::proportional(11.),
            colors.accent,
        );
    }
    response
}
