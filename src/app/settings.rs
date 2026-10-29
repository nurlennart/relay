//! Settings, themes, and SSH snippets.
use super::*;
use egui::{Align, Align2, FontId, Layout, Margin, Rect, Sense, UiBuilder};

impl App {
    pub(super) fn settings(&mut self, ctx: &egui::Context) {
        if !matches!(self.modal, Modal::Settings) {
            return;
        }
        let colors = theme::palette(ctx);
        let mut choice = theme::current(ctx);
        let before = choice;
        let mut close = false;
        let mut action = None;
        let mut copied_version = None;
        let mut shortcuts = false;
        dialog("settings", ctx).show(ctx, |ui| {
            ui.set_width(dialog_width(ctx, 540.));
            ui.horizontal(|ui| {
                ui.label(RichText::new("Settings").size(TITLE_SIZE).strong());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    close = icon_button(ui, Icon::Close, 26., "Close (Esc)").clicked();
                    shortcuts = ui
                        .button("Keyboard shortcuts")
                        .on_hover_text("F1")
                        .clicked();
                    let version = version();
                    if ui
                        .add(
                            egui::Label::new(RichText::new(&version).size(12.).color(colors.muted))
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
            ui.add_space(4.);
            tab_bar(ui, &mut self.settings_tab);
            let height = (ctx.content_rect().height() - 220.).clamp(200., 540.);
            egui::ScrollArea::vertical()
                .id_salt(("settings-body", self.settings_tab))
                .auto_shrink([false, false])
                .min_scrolled_height(height)
                .max_height(height)
                .show(ui, |ui| {
                    ui.add_space(12.);
                    match self.settings_tab {
                        SettingsTab::Appearance => {
                            theme_picker(ui, &mut choice);
                            zoom_picker(ui);
                        }
                        SettingsTab::Terminal => self.terminal_settings(ui),
                        SettingsTab::Library => action = self.library_settings(ui),
                        SettingsTab::Snippets => {
                            ui.label(
                                RichText::new(
                                    "Saved commands for SSH sessions. Press Ctrl+Shift+P in a \
                                     terminal.",
                                )
                                .color(colors.muted),
                            );
                            ui.add_space(4.);
                            self.snippet_settings(ui);
                        }
                    }
                });
            // Below the tabs' content, so a message shows whichever tab is open.
            if let Some(error) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            if let Some((notice, _)) = &self.notice {
                ui.colored_label(colors.accent, notice);
            }
        });
        if choice != before {
            choice.apply(ctx);
            for window in self
                .parent
                .iter()
                .chain(self.windows.iter().map(|(_, w)| w))
            {
                window.set_theme(choice);
            }
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
        if shortcuts {
            self.modal = Modal::Shortcuts;
        }
        match action {
            Some(LibraryAction::Switch) => self.modal = Modal::Library,
            // Each import replaces settings with its preview once read.
            Some(LibraryAction::Import(Source::Csv)) => self.import_csv(),
            Some(LibraryAction::Import(Source::SshConfig)) => self.import_ssh_config(),
            Some(LibraryAction::Import(Source::RdpFiles)) => self.import_rdp_files(),
            Some(LibraryAction::Import(Source::Json)) => self.import_json(),
            Some(LibraryAction::Export(kind)) => self.export(kind, false),
            Some(LibraryAction::Recover) => self.export(file_dialog::Kind::ExportJson, true),
            None => {}
        }
    }

    /// Where connections are stored, and importing and exporting them.
    fn library_settings(&mut self, ui: &mut egui::Ui) -> Option<LibraryAction> {
        let colors = theme::palette(ui.ctx());
        let mut action = None;
        section_label(ui, "CONNECTION LIBRARY");
        ui.label(RichText::new("Your connections are stored in").color(colors.muted));
        ui.add_space(2.);
        let (detail, facts) = self.library_summary();
        // The button comes first so the row takes its height and centers the name.
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_enabled(self.pending.is_none(), egui::Button::new("Switch library…"))
                    .on_hover_text("Personal, a shared SQLite file, MSSQL, or MySQL")
                    .clicked()
                {
                    action = Some(LibraryAction::Switch);
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    let (dot, _) = ui.allocate_exact_size(Vec2::splat(8.), Sense::hover());
                    ui.painter().circle_filled(dot.center(), 3.5, colors.accent);
                    ui.add(
                        egui::Label::new(RichText::new(self.location.label()).strong()).truncate(),
                    );
                });
            });
        });
        if !detail.is_empty() {
            ui.add(
                egui::Label::new(
                    RichText::new(&detail)
                        .monospace()
                        .size(12.)
                        .color(colors.text),
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
            RichText::new(
                "Bring in connections from CSV, your SSH config, .rdp files, or a Relay JSON \
                 file. JSON keeps every setting; CSV suits other tools.",
            )
            .color(colors.muted),
        );
        ui.add_space(4.);
        let width = (ui.available_width() - ui.spacing().item_spacing.x) / 2.;
        let rows: [[(&str, &str, LibraryAction); 2]; 3] = [
            [
                ("Import CSV", "", LibraryAction::Import(Source::Csv)),
                (
                    "Import SSH config",
                    "Reads ~/.ssh/config",
                    LibraryAction::Import(Source::SshConfig),
                ),
            ],
            [
                (
                    "Import .rdp files",
                    "",
                    LibraryAction::Import(Source::RdpFiles),
                ),
                (
                    "Import JSON",
                    "A Relay JSON export, or a connections.json library",
                    LibraryAction::Import(Source::Json),
                ),
            ],
            [
                (
                    "Export JSON",
                    "Every setting, including SSH and remote desktop options",
                    LibraryAction::Export(file_dialog::Kind::ExportJson),
                ),
                (
                    "Export CSV",
                    "Without SSH and remote desktop options",
                    LibraryAction::Export(file_dialog::Kind::ExportCsv),
                ),
            ],
        ];
        for row in rows {
            ui.horizontal(|ui| {
                for (label, hint, choice) in row {
                    // Imports wait for a library switch in progress; exports do not.
                    let enabled =
                        self.pending.is_none() || matches!(choice, LibraryAction::Export(_));
                    let mut button = ui
                        .add_enabled_ui(enabled, |ui| outline_button(ui, label, width))
                        .inner;
                    if !hint.is_empty() {
                        button = button.on_hover_text(hint);
                    }
                    if button.clicked() {
                        action = Some(choice);
                    }
                }
            });
        }
        if self.recovery.is_some() && ui.button("Export unsaved changes").clicked() {
            action = Some(LibraryAction::Recover);
        }
        action
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

    /// The terminal font, its size, and the color scheme, with a sample of how they look.
    fn terminal_settings(&mut self, ui: &mut egui::Ui) {
        let colors = theme::palette(ui.ctx());
        let scan = (self.font_scan)
            .get_or_insert_with(|| crate::fonts::Scan::start(ui.ctx().clone()))
            .clone();
        let settings = self.state.terminal.clone();
        let mut next = settings.clone();
        // Row names at the left edge, in the same muted color as "Colors" below them.
        let row_label = |ui: &mut egui::Ui, text: &str| {
            let size = Vec2::new(64., ui.spacing().interact_size.y);
            ui.allocate_ui_with_layout(size, Layout::left_to_right(Align::Center), |ui| {
                ui.set_min_size(size);
                ui.label(RichText::new(text).color(colors.muted));
            });
        };
        ui.horizontal(|ui| {
            row_label(ui, "Font");
            let current = match &settings.font {
                None => "System monospace".to_string(),
                Some(path) => (scan.fonts().unwrap_or_default().iter())
                    .find(|f| &f.path == path)
                    .map(|f| f.family.clone())
                    .unwrap_or_else(|| {
                        path.file_name().map_or_else(
                            || path.display().to_string(),
                            |n| n.to_string_lossy().into_owned(),
                        )
                    }),
            };
            egui::ComboBox::from_id_salt("terminal-font")
                .selected_text(current)
                .width(220.)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut next.font, None, "System monospace");
                    match scan.fonts() {
                        None => {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(RichText::new("Looking for fonts…").color(colors.muted));
                            });
                        }
                        Some(fonts) => {
                            for font in fonts {
                                ui.selectable_value(
                                    &mut next.font,
                                    Some(font.path.clone()),
                                    &font.family,
                                )
                                .on_hover_text(font.path.display().to_string());
                            }
                        }
                    }
                });
            if ui
                .button("Choose file…")
                .on_hover_text("A TrueType or OpenType font file")
                .clicked()
                && let Some(path) = file_dialog::choose(self.parent, file_dialog::Kind::Font)
                    .into_iter()
                    .next()
            {
                next.font = Some(path);
            }
        });
        ui.horizontal(|ui| {
            row_label(ui, "Size");
            ui.add(
                egui::Slider::new(&mut next.font_size, crate::terminal::FONT_SIZES)
                    .step_by(1.)
                    .fixed_decimals(0)
                    .suffix(" pt"),
            );
            if next.font_size != crate::terminal::FONT_SIZE && ui.button("Reset").clicked() {
                next.font_size = crate::terminal::FONT_SIZE;
            }
        });
        ui.label(
            RichText::new(format!(
                "In a terminal, {ZOOM_KEYS} change the size, and {ZOOM_RESET_KEY} resets it."
            ))
            .size(12.)
            .color(colors.muted),
        );
        ui.checkbox(
            &mut next.programs_copy,
            "Programs on the server can copy to this computer's clipboard",
        )
        .on_hover_text(
            "As tmux, vim and others do with OSC 52. Relay says when it happens. \
             Programs can never read the clipboard.",
        );
        ui.add_space(6.);
        ui.label(RichText::new("Colors").color(colors.muted));
        ui.add_space(2.);
        let workspace = theme::current(ui.ctx());
        let schemes = crate::terminal::Scheme::ALL;
        for pair in schemes.chunks(2) {
            ui.columns(2, |columns| {
                for (column, &scheme) in pair.iter().enumerate() {
                    if scheme_option(
                        &mut columns[column],
                        scheme,
                        workspace,
                        next.scheme == scheme,
                    )
                    .clicked()
                    {
                        next.scheme = scheme;
                    }
                }
            });
        }
        ui.add_space(6.);
        terminal_sample(ui, next.font_size, &next.scheme.colors(workspace));
        if next.font != settings.font {
            match set_fonts(ui.ctx(), next.font.as_deref()) {
                Ok(()) => self.error = None,
                Err(e) => {
                    self.error = Some(e);
                    next.font = settings.font.clone();
                    let _ = set_fonts(ui.ctx(), next.font.as_deref());
                }
            }
        }
        if next != settings {
            self.state.terminal = next;
            self.save_state();
        }
        ui.add_space(14.);
        ui.separator();
        section_label(ui, "SESSIONS");
        let mut reopen = !self.state.forget_tabs;
        if ui.checkbox(&mut reopen, "Reopen tabs at startup").changed() {
            self.state.forget_tabs = !reopen;
            self.remember_tabs();
        }
        ui.label(
            RichText::new(
                "Tabs open when Relay closes come back, and each connects when you first select \
                 it. Turned off, Relay starts with only the library and keeps no list of your \
                 tabs.",
            )
            .size(12.)
            .color(colors.muted),
        );
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
            .active()
            .filter(|i| self.sessions.get(*i).is_some_and(|s| s.terminal.is_some()));
        let Some(index) = index.filter(|_| !self.covered()) else {
            return;
        };
        let colors = theme::palette(ctx);
        // Keys first, so the search field does not see them.
        let (down, up, enter, escape) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            )
        });
        let mut chosen = None;
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
                        // Matches come from the query as this frame's typing or paste left it,
                        // so Enter in the same frame sends what is shown. Only shown ones count.
                        let mut matches = state::matching(&self.state.snippets, &palette.query);
                        matches.truncate(8);
                        let last = matches.len().saturating_sub(1);
                        palette.selected = if down {
                            (palette.selected + 1).min(last)
                        } else if up {
                            palette.selected.saturating_sub(1)
                        } else {
                            palette.selected.min(last)
                        };
                        if enter {
                            chosen = matches.get(palette.selected).copied();
                        }
                        // The pointer selects a row only when it moves, so one resting over
                        // the list does not undo the arrow keys.
                        let moved = ui.input(|i| i.pointer.delta() != Vec2::ZERO);
                        if self.state.snippets.is_empty() {
                            ui.label(
                                RichText::new("No snippets yet. Add your first one in Settings.")
                                    .color(colors.muted),
                            );
                        } else if matches.is_empty() {
                            ui.label(RichText::new("No snippets match.").color(colors.muted));
                        }
                        ui.spacing_mut().item_spacing.y = 2.;
                        for (row, &i) in matches.iter().enumerate() {
                            let snippet = &self.state.snippets[i];
                            let (rect, response) =
                                ui.allocate_exact_size(Vec2::new(width, 42.), Sense::click());
                            if response.hovered() && moved {
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
            self.settings_tab = SettingsTab::Snippets;
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
    Json,
}

/// The pages of Settings. The one last shown opens again until Relay closes.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq)]
pub(super) enum SettingsTab {
    #[default]
    Appearance,
    Terminal,
    Library,
    Snippets,
}

impl SettingsTab {
    const ALL: [Self; 4] = [
        Self::Appearance,
        Self::Terminal,
        Self::Library,
        Self::Snippets,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Terminal => "Terminal",
            Self::Library => "Library",
            Self::Snippets => "Snippets",
        }
    }
}

/// What the Library tab asks for, done once Settings is drawn.
enum LibraryAction {
    Switch,
    Import(Source),
    Export(file_dialog::Kind),
    /// Export edits that could not be saved to the library.
    Recover,
}

/// The row of page names across the top of Settings, underlining the open one.
pub(super) fn tab_bar(ui: &mut egui::Ui, current: &mut SettingsTab) {
    let colors = theme::palette(ui.ctx());
    let font = FontId::proportional(13.);
    let (bar, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.), Sense::hover());
    let mut left = bar.left();
    for tab in SettingsTab::ALL {
        let galley =
            ui.painter()
                .layout_no_wrap(tab.label().into(), font.clone(), Color32::PLACEHOLDER);
        let text = Rect::from_min_size(
            egui::pos2(left, bar.center().y - galley.size().y / 2.),
            galley.size(),
        );
        // The gap between names belongs to the name before it, so it still takes the click.
        let rect = Rect::from_x_y_ranges(left..=text.right() + 20., bar.y_range());
        let response = ui.interact(rect, egui::Id::new(("settings-tab", tab)), Sense::click());
        let open = *current == tab;
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, open, tab.label())
        });
        if response.clicked() {
            *current = tab;
        }
        let color = if open || response.hovered() || response.has_focus() {
            colors.text
        } else {
            colors.muted
        };
        ui.painter().galley(text.min, galley, color);
        if open {
            ui.painter().rect_filled(
                Rect::from_x_y_ranges(text.x_range(), bar.bottom() - 2.0..=bar.bottom()),
                1.,
                colors.accent,
            );
        }
        left = rect.right();
    }
    ui.painter().hline(
        bar.x_range(),
        bar.bottom() + 0.5,
        Stroke::new(1_f32, colors.line),
    );
}

/// The workspace themes, two to a row.
fn theme_picker(ui: &mut egui::Ui, choice: &mut theme::Theme) {
    let colors = theme::palette(ui.ctx());
    ui.label(
        RichText::new("Choose a workspace theme. Saved on this computer.").color(colors.muted),
    );
    ui.add_space(4.);
    for pair in theme::Theme::ALL.chunks(2) {
        ui.columns(2, |columns| {
            for (column, &candidate) in pair.iter().enumerate() {
                if theme_option(&mut columns[column], candidate, *choice == candidate).clicked() {
                    *choice = candidate;
                }
            }
        });
    }
}

/// The workspace zoom, which the zoom keys also change outside a terminal.
fn zoom_picker(ui: &mut egui::Ui) {
    let colors = theme::palette(ui.ctx());
    let ctx = ui.ctx().clone();
    let zoom = ctx.zoom_factor();
    let percent = |zoom: f32| format!("{:.0}%", zoom * 100.);
    ui.add_space(16.);
    ui.horizontal(|ui| {
        ui.label("Zoom");
        egui::ComboBox::from_id_salt("zoom")
            .selected_text(percent(zoom))
            .show_ui(ui, |ui| {
                for step in [0.5, 0.75, 0.9, 1., 1.1, 1.25, 1.5, 1.75, 2.] {
                    if ui.selectable_label(zoom == step, percent(step)).clicked() {
                        ctx.set_zoom_factor(step);
                    }
                }
            });
        if zoom != 1. && ui.button("Reset").clicked() {
            ctx.set_zoom_factor(1.);
        }
    });
    ui.label(
        RichText::new(format!(
            "{WINDOW_ZOOM_KEYS} also zoom outside a terminal, and {ZOOM_RESET_KEY} resets it."
        ))
        .size(12.)
        .color(colors.muted),
    );
}

/// Relay's version, with the commit it was built from when that is known, such as
/// `Relay 0.4.0 · a0bb71f`.
pub(super) fn version() -> String {
    match option_env!("RELAY_COMMIT") {
        Some(commit) => format!("Relay {} · {commit}", env!("CARGO_PKG_VERSION")),
        None => format!("Relay {}", env!("CARGO_PKG_VERSION")),
    }
}

/// The keys that zoom a terminal, as Settings names them.
const ZOOM_KEYS: &str = if cfg!(target_os = "macos") {
    "Cmd+Plus and Cmd+Minus (or Cmd with the scroll wheel)"
} else {
    "Ctrl+Plus and Ctrl+Minus (or Ctrl with the scroll wheel)"
};
/// The keys that zoom the workspace, outside a terminal.
const WINDOW_ZOOM_KEYS: &str = if cfg!(target_os = "macos") {
    "Cmd+Plus and Cmd+Minus"
} else {
    "Ctrl+Plus and Ctrl+Minus"
};
const ZOOM_RESET_KEY: &str = if cfg!(target_os = "macos") {
    "Cmd+0"
} else {
    "Ctrl+0"
};

/// A terminal color scheme to choose, with its background and a few of its colors.
fn scheme_option(
    ui: &mut egui::Ui,
    scheme: crate::terminal::Scheme,
    workspace: theme::Theme,
    selected: bool,
) -> egui::Response {
    let colors = theme::palette(ui.ctx());
    let pal = scheme.colors(workspace);
    let response = ui.add_sized(
        [ui.available_width(), 34.],
        egui::Button::new("").selected(selected),
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), scheme.name())
    });
    let center = response.rect.center();
    let swatch = egui::Rect::from_center_size(
        egui::pos2(response.rect.left() + 26., center.y),
        Vec2::new(36., 20.),
    );
    ui.painter().rect(
        swatch,
        4.,
        pal.background,
        Stroke::new(1_f32, colors.line),
        egui::StrokeKind::Inside,
    );
    for (i, color) in [pal.foreground, pal.ansi[1], pal.ansi[2], pal.ansi[4]]
        .into_iter()
        .enumerate()
    {
        ui.painter().circle_filled(
            egui::pos2(swatch.left() + 7. + i as f32 * 7.3, center.y),
            2.6,
            color,
        );
    }
    ui.painter().text(
        egui::pos2(swatch.right() + 10., center.y),
        Align2::LEFT_CENTER,
        scheme.name(),
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

/// A few lines of shell output in the terminal font, size and colors `pal`.
fn terminal_sample(ui: &mut egui::Ui, size: f32, pal: &crate::terminal::Colors) {
    let font = FontId::monospace(size);
    let lines: [&[(&str, Color32)]; 3] = [
        &[
            ("deploy@build", pal.ansi[2]),
            (":", pal.foreground),
            ("~", pal.ansi[4]),
            ("$ ls -l", pal.foreground),
        ],
        &[
            ("drwxr-xr-x  ", pal.foreground),
            ("releases", pal.ansi[4]),
            ("  ", pal.foreground),
            ("run.sh", pal.ansi[2]),
            ("  ", pal.foreground),
            ("error.log", pal.ansi[1]),
        ],
        &[("$ ", pal.foreground), (" ", pal.cursor)],
    ];
    egui::Frame::new()
        .fill(pal.background)
        .stroke(Stroke::new(1_f32, pal.border))
        .corner_radius(RADIUS)
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.;
            for (row, line) in lines.iter().enumerate() {
                let mut job = egui::text::LayoutJob::default();
                for (i, (text, color)) in line.iter().enumerate() {
                    // The last line ends with the cursor, a block in its color.
                    let cursor = row == 2 && i == 1;
                    job.append(
                        text,
                        0.,
                        egui::TextFormat {
                            font_id: font.clone(),
                            color: if cursor { pal.background } else { *color },
                            background: if cursor {
                                pal.cursor
                            } else {
                                Color32::TRANSPARENT
                            },
                            ..Default::default()
                        },
                    );
                }
                ui.add(egui::Label::new(job).wrap_mode(egui::TextWrapMode::Truncate));
            }
        });
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
