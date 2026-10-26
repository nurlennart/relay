use super::*;
use egui::{Align, Align2, FontId, Layout, Margin, Rect, Sense, UiBuilder};

impl App {
    pub fn sidebar(&mut self, ctx: &egui::Context) {
        let colors = theme::palette(ctx);
        egui::SidePanel::left("sidebar")
            .exact_width(236.)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(colors.panel)
                    .inner_margin(Margin::symmetric(16, 20))
                    .stroke(Stroke::new(1_f32, colors.line)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    egui::Frame::new()
                        .fill(colors.accent)
                        .corner_radius(10)
                        .inner_margin(Margin::symmetric(10, 4))
                        .show(ui, |ui| {
                            ui.label(RichText::new("R").size(24.).strong().color(colors.bg));
                        });
                    ui.vertical(|ui| {
                        ui.label(RichText::new("Relay").size(23.).strong());
                        ui.label(
                            RichText::new("Remote workspace")
                                .size(11.)
                                .color(colors.muted),
                        );
                    });
                });
                ui.add_space(28.);
                section_label(ui, "LIBRARY");
                for (filter, symbol, label, count) in [
                    (Filter::All, "▦", "All connections", self.connections.len()),
                    (
                        Filter::Favorites,
                        "☆",
                        "Favorites",
                        self.connections.iter().filter(|c| c.favorite).count(),
                    ),
                ] {
                    if nav_row(
                        ui,
                        symbol,
                        label,
                        count,
                        self.filter == filter && self.group.is_none(),
                    ) {
                        self.filter = filter;
                        self.group = None;
                        self.active = None;
                    }
                }
                ui.add_space(20.);
                section_label(ui, "PROTOCOLS");
                for (filter, symbol, label, protocol) in [
                    (Filter::Rdp, "▣", "Remote desktop", Protocol::Rdp),
                    (Filter::Ssh, ">_", "SSH terminal", Protocol::Ssh),
                ] {
                    if nav_row(
                        ui,
                        symbol,
                        label,
                        self.connections
                            .iter()
                            .filter(|c| c.protocol == protocol)
                            .count(),
                        self.filter == filter && self.group.is_none(),
                    ) {
                        self.filter = filter;
                        self.group = None;
                        self.active = None;
                    }
                }
                ui.add_space(20.);
                section_label(ui, "GROUPS");
                let mut groups = std::collections::BTreeMap::<String, usize>::new();
                for c in &self.connections {
                    *groups.entry(c.group.clone()).or_default() += 1;
                }
                egui::ScrollArea::vertical()
                    .id_salt("groups")
                    .max_height((ui.available_height() - 220.).max(90.))
                    .show(ui, |ui| {
                        if groups.is_empty() {
                            ui.label(
                                RichText::new("Your groups will appear here.")
                                    .size(12.)
                                    .color(colors.muted),
                            );
                        }
                        for (group, count) in groups {
                            if nav_row(ui, "▱", &group, count, self.group.as_ref() == Some(&group))
                            {
                                self.group = Some(group);
                                self.filter = Filter::All;
                                self.active = None;
                            }
                        }
                    });
                ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
                    ui.label(
                        RichText::new("Ctrl+K  Search     Ctrl+N  New")
                            .size(11.)
                            .color(colors.muted),
                    );
                    ui.add_space(8.);
                    if outline_button(ui, "Settings", ui.available_width()).clicked() {
                        self.modal = Modal::Settings;
                    }
                    ui.add_space(12.);
                    egui::Frame::new()
                        .fill(colors.surface)
                        .corner_radius(8)
                        .inner_margin(12.)
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.colored_label(colors.accent, "●");
                                ui.label(RichText::new(self.location.label()).size(12.).strong());
                            });
                            ui.label(
                                RichText::new(if self.pending.is_some() {
                                    "Updating library…"
                                } else {
                                    "Connection profiles only"
                                })
                                .size(11.)
                                .color(colors.muted),
                            );
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("Library settings  →")
                                            .size(12.)
                                            .color(colors.accent),
                                    )
                                    .frame(false),
                                )
                                .clicked()
                            {
                                self.modal = Modal::Settings;
                            }
                        });
                    ui.add_space(10.);
                });
            });
    }

    pub fn filtered(&self) -> Vec<Connection> {
        let query = self.query.to_lowercase();
        let mut items: Vec<_> = self
            .connections
            .iter()
            .filter(|c| {
                c.matches(&query)
                    && self.group.as_ref().is_none_or(|g| g == &c.group)
                    && match self.filter {
                        Filter::All => true,
                        Filter::Favorites => c.favorite,
                        Filter::Rdp => c.protocol == Protocol::Rdp,
                        Filter::Ssh => c.protocol == Protocol::Ssh,
                    }
            })
            .cloned()
            .collect();
        items.sort_by_key(|c| c.name.to_lowercase());
        items
    }

    pub fn tabs(&mut self, ctx: &egui::Context) {
        let colors = theme::palette(ctx);
        egui::TopBottomPanel::top("tabs")
            .exact_height(58.)
            .frame(
                egui::Frame::new()
                    .fill(colors.panel)
                    .inner_margin(Margin::symmetric(20, 10))
                    .stroke(Stroke::new(1_f32, colors.line)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::horizontal()
                    .id_salt("tabs-scroll")
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let library =
                                tab(ui, "      Connection library", self.active.is_none());
                            library_icon(
                                ui,
                                egui::pos2(library.rect.left() + 18., library.rect.center().y),
                                if self.active.is_none() {
                                    colors.accent
                                } else {
                                    colors.muted
                                },
                            );
                            if library.clicked() {
                                self.active = None;
                            }
                            for (index, s) in self.sessions.iter().enumerate() {
                                ui.push_id(index, |ui| {
                                    let session = session_tab(
                                        ui,
                                        &format!(
                                            "{}  {}",
                                            s.connection.protocol.label(),
                                            s.connection.name
                                        ),
                                        self.active == Some(index),
                                    );
                                    if session.inner.0.clicked() {
                                        self.active = Some(index);
                                    }
                                    if session.inner.1.clicked() {
                                        self.modal = Modal::Close(index);
                                    }
                                });
                            }
                            #[cfg(feature = "ui-check")]
                            if std::env::var_os("RELAY_UI_TABS").is_some() {
                                session_tab(ui, "RDP  Design workstation", false);
                                session_tab(ui, "SSH  Build server", false);
                            }
                            if ui
                                .add_sized([32., 34.], egui::Button::new("+").frame(false))
                                .on_hover_text("New connection")
                                .clicked()
                            {
                                self.new_connection();
                            }
                        });
                    });
            });
    }

    pub fn workspace(&mut self, ui: &mut egui::Ui) {
        let colors = theme::palette(ui.ctx());
        ui.add_space(8.);
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(
                    RichText::new("CONNECTION LIBRARY")
                        .size(10.)
                        .color(colors.accent),
                );
                ui.add_space(2.);
                let title = self.group.as_deref().unwrap_or(match self.filter {
                    Filter::All => "All connections",
                    Filter::Favorites => "Favorites",
                    Filter::Rdp => "Remote desktops",
                    Filter::Ssh => "SSH terminals",
                });
                ui.add(egui::Label::new(RichText::new(title).size(29.).strong()).truncate());
                ui.label(
                    RichText::new(format!(
                        "{} saved connections · {} open sessions",
                        self.connections.len(),
                        self.sessions.iter().filter(|s| s.is_running()).count()
                    ))
                    .size(13.)
                    .color(colors.muted),
                );
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if accent_button(ui, "+  New connection", 166.).clicked() {
                    self.new_connection();
                }
            });
        });
        ui.add_space(26.);
        ui.horizontal(|ui| {
            let response = ui.add_sized(
                [ui.available_width().min(345.), 36.],
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text("Search by name, address, or group…")
                    .margin(Margin::symmetric(12, 9)),
            );
            if self.search_focus {
                response.request_focus();
                self.search_focus = false;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if outline_button(ui, "Refresh", 86.)
                    .on_hover_text("Load the latest team changes")
                    .clicked()
                {
                    self.refresh();
                }
            });
        });
        ui.add_space(18.);
        let connections = self.filtered();
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("{} connections", connections.len()))
                    .size(12.)
                    .strong(),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new("Double-click a name to connect")
                        .size(11.)
                        .color(colors.muted),
                );
            });
        });
        ui.add_space(8.);
        if connections.is_empty() {
            egui::Frame::new()
                .fill(colors.panel)
                .corner_radius(10)
                .stroke(Stroke::new(1_f32, colors.line))
                .inner_margin(30.)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.add_space(45.);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new("Your workspace, ready to connect.")
                                .size(23.)
                                .strong(),
                        );
                        ui.add_space(8.);
                        ui.label(
                            RichText::new(
                                "Add a machine or bring your existing connection library.",
                            )
                            .color(colors.muted),
                        );
                        ui.add_space(22.);
                        ui.horizontal(|ui| {
                            ui.add_space((ui.available_width() - 330.).max(0.) / 2.);
                            if accent_button(ui, "+  New connection", 166.).clicked() {
                                self.new_connection();
                            }
                            if outline_button(ui, "Import CSV", 145.).clicked() {
                                self.import_csv();
                            }
                        });
                    });
                    ui.add_space(55.);
                });
            return;
        }
        egui::Frame::new()
            .fill(colors.panel)
            .corner_radius(10)
            .stroke(Stroke::new(1_f32, colors.line))
            .inner_margin(Margin::symmetric(24, 6))
            .show(ui, |ui| {
                let width = ui.available_width();
                let flexible = (width - 36. - 66. - 128. - 80.).max(260.);
                let widths = [
                    36.,
                    flexible * 0.41,
                    66.,
                    flexible * 0.35,
                    flexible * 0.24,
                    128.,
                ];
                let (header, _) = ui.allocate_exact_size(Vec2::new(width, 36.), Sense::hover());
                let mut x = header.min.x;
                for (label, w) in ["", "NAME", "PROTOCOL", "HOST / USER", "GROUP", ""]
                    .iter()
                    .zip(widths)
                {
                    ui.painter().text(
                        egui::pos2(x, header.center().y),
                        Align2::LEFT_CENTER,
                        label,
                        FontId::proportional(10.),
                        colors.muted,
                    );
                    x += w + 16.;
                }
                ui.painter().line_segment(
                    [header.left_bottom(), header.right_bottom()],
                    Stroke::new(1_f32, colors.line),
                );
                let height = (ui.available_height() - 12.).max(150.);
                egui::ScrollArea::vertical()
                    .id_salt("connection-list")
                    .max_height(height)
                    .auto_shrink([false, false])
                    .show_rows(ui, 64., connections.len(), |ui, range| {
                        for index in range {
                            let c = &connections[index];
                            let (rect, response) =
                                ui.allocate_exact_size(Vec2::new(width, 64.), Sense::click());
                            let selected = self.selected == Some(c.id);
                            if selected || response.hovered() {
                                ui.painter().rect_filled(
                                    rect,
                                    6.,
                                    if selected {
                                        colors.selected
                                    } else {
                                        colors.hover
                                    },
                                );
                            }
                            if response.clicked() {
                                self.selected = Some(c.id);
                            }
                            if response.double_clicked() {
                                self.connect(c.clone());
                            }
                            let mut x = rect.min.x;
                            for (column, w) in widths.into_iter().enumerate() {
                                let cell = Rect::from_min_size(
                                    egui::pos2(x, rect.min.y + 10.),
                                    Vec2::new(w, 44.),
                                );
                                ui.scope_builder(
                                    UiBuilder::new()
                                        .max_rect(cell)
                                        .layout(Layout::top_down(Align::LEFT)),
                                    |ui| {
                                        ui.set_clip_rect(cell.intersect(ui.clip_rect()));
                                        match column {
                                            0 => {
                                                ui.spacing_mut().button_padding.x = 0.;
                                                ui.add_space(3.);
                                                if ui
                                                    .add(
                                                        egui::Button::new(
                                                            RichText::new(if c.favorite {
                                                                "★"
                                                            } else {
                                                                "☆"
                                                            })
                                                            .size(18.)
                                                            .color(if c.favorite {
                                                                colors.accent
                                                            } else {
                                                                colors.muted
                                                            }),
                                                        )
                                                        .frame(false)
                                                        .min_size(Vec2::new(30., 30.)),
                                                    )
                                                    .on_hover_text("Toggle favorite")
                                                    .clicked()
                                                {
                                                    let mut next = self.connections.clone();
                                                    if let Some(item) =
                                                        next.iter_mut().find(|v| v.id == c.id)
                                                    {
                                                        item.favorite = !item.favorite;
                                                    }
                                                    self.persist(next);
                                                }
                                            }
                                            1 => {
                                                let response = ui.add(
                                                    egui::Label::new(
                                                        RichText::new(&c.name).size(14.).strong(),
                                                    )
                                                    .truncate()
                                                    .sense(Sense::click()),
                                                );
                                                if response.clicked() {
                                                    self.selected = Some(c.id);
                                                }
                                                if response.double_clicked() {
                                                    self.connect(c.clone());
                                                }
                                                response.on_hover_text(&c.name);
                                                ui.add(
                                                    egui::Label::new(
                                                        RichText::new(
                                                            if c.protocol == Protocol::Rdp {
                                                                "Remote desktop"
                                                            } else {
                                                                "Secure shell"
                                                            },
                                                        )
                                                        .size(11.)
                                                        .color(colors.muted),
                                                    )
                                                    .truncate(),
                                                );
                                            }
                                            2 => {
                                                ui.add_space(8.);
                                                protocol_badge(ui, c.protocol);
                                            }
                                            3 => {
                                                ui.add(
                                                    egui::Label::new(
                                                        RichText::new(c.address())
                                                            .monospace()
                                                            .size(11.)
                                                            .color(colors.text),
                                                    )
                                                    .truncate(),
                                                )
                                                .on_hover_text(c.address());
                                                ui.add(
                                                    egui::Label::new(
                                                        RichText::new(if c.username.is_empty() {
                                                            "Ask on connect"
                                                        } else {
                                                            &c.username
                                                        })
                                                        .size(11.)
                                                        .color(colors.muted),
                                                    )
                                                    .truncate(),
                                                );
                                            }
                                            4 => {
                                                ui.add_space(10.);
                                                ui.add(
                                                    egui::Label::new(
                                                        RichText::new(&c.group)
                                                            .size(12.)
                                                            .color(colors.muted),
                                                    )
                                                    .truncate(),
                                                )
                                                .on_hover_text(&c.group);
                                            }
                                            _ => {
                                                ui.add_space(4.);
                                                ui.horizontal(|ui| {
                                                    if ui
                                                        .add_sized(
                                                            [74., 30.],
                                                            egui::Button::new(
                                                                RichText::new("Connect").size(12.),
                                                            )
                                                            .corner_radius(5),
                                                        )
                                                        .clicked()
                                                    {
                                                        self.connect(c.clone());
                                                    }
                                                    ui.menu_button("···", |ui| {
                                                        if ui.button("Edit connection").clicked() {
                                                            self.edit(c.clone());
                                                            ui.close();
                                                        }
                                                        if ui.button("Delete connection").clicked()
                                                        {
                                                            self.modal = Modal::Delete(c.id);
                                                            ui.close();
                                                        }
                                                    });
                                                });
                                            }
                                        }
                                    },
                                );
                                x += w + 16.;
                            }
                            if index + 1 < connections.len() {
                                ui.painter().line_segment(
                                    [rect.left_bottom(), rect.right_bottom()],
                                    Stroke::new(1_f32, colors.line),
                                );
                            }
                        }
                    });
            });
    }

    pub fn refresh(&mut self) {
        if self.pending.is_none() {
            self.pending_save = false;
            self.pending = Some(database::start(
                self.location.clone(),
                self.revision,
                None,
                self.ctx.clone(),
            ));
        }
    }
    pub fn poll_storage(&mut self) {
        let result = self.pending.as_ref().map(|rx| rx.try_recv());
        if let Some(result) = result {
            match result {
                Ok(Ok(snapshot)) => {
                    self.connections = snapshot.connections;
                    self.revision = snapshot.revision;
                    self.storage_ok = true;
                    if let Some(location) = self.pending_location.take() {
                        self.location = location;
                        if matches!(self.modal, Modal::Library) {
                            self.modal = Modal::None;
                        }
                        self.group = None;
                        self.filter = Filter::All;
                        self.db_mssql.clear();
                    }
                    if self.pending_save {
                        self.recovery = None;
                    }
                    self.pending_save = false;
                    self.pending_editor = None;
                    self.pending = None;
                    self.error = None;
                    self.notice = Some("Library updated successfully.".into());
                }
                Ok(Err(e)) => {
                    self.pending_save = false;
                    self.error = Some(e);
                    self.pending = None;
                    self.pending_location = None;
                    // Reopen the editor so a rejected edit is not lost.
                    if let Some(editor) = self.pending_editor.take() {
                        self.modal = Modal::Editor(editor);
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.error = Some(
                        "The library worker stopped unexpectedly. Your changes were not confirmed."
                            .into(),
                    );
                    self.pending = None;
                    self.pending_location = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
    }
    fn import_csv(&mut self) {
        for s in &mut self.sessions {
            s.place(None);
        }
        match file_dialog::choose(self.parent, false, false) {
            Ok(Some(path)) => match csv_io::read(&path, &self.connections) {
                Ok(import) => self.modal = Modal::Import(import),
                Err(e) => self.error = Some(e),
            },
            Ok(None) => {}
            Err(e) => self.error = Some(e),
        }
    }
    pub fn export_csv(&mut self, recovery: bool) {
        for s in &mut self.sessions {
            s.place(None);
        }
        match file_dialog::choose(self.parent, true, false) {
            Ok(Some(path)) => {
                let connections = if recovery {
                    self.recovery.as_deref().unwrap_or(&self.connections)
                } else {
                    &self.connections
                };
                match csv_io::export(&path, connections) {
                    Ok(()) => {
                        self.notice = Some(format!(
                            "Exported {} connections to {}",
                            connections.len(),
                            path.display()
                        ))
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            Ok(None) => {}
            Err(e) => self.error = Some(e),
        }
    }
    pub fn settings(&mut self, ctx: &egui::Context) {
        if !matches!(self.modal, Modal::Settings) {
            return;
        }
        let colors = theme::palette(ctx);
        let mut choice = theme::current(ctx);
        let before = choice;
        let mut close = false;
        let mut manage = false;
        let mut import = false;
        let mut export = false;
        let mut recovery = false;
        dialog("settings", ctx).show(ctx, |ui| {
            ui.set_width(540.);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Settings").size(25.).strong());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    close = ui.button("Close").clicked();
                });
            });
            egui::ScrollArea::vertical()
                .id_salt("settings-body")
                .min_scrolled_height((ctx.content_rect().height() - 180.).clamp(200., 580.))
                .max_height((ctx.content_rect().height() - 180.).clamp(200., 580.))
                .show(ui, |ui| {
                    ui.add_space(14.);
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
                    section_label(ui, "CONNECTION LIBRARY");
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(self.location.label()).strong());
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            manage = ui
                                .add_enabled(
                                    self.pending.is_none(),
                                    egui::Button::new("Manage library"),
                                )
                                .clicked();
                        });
                    });
                    ui.label(
                        RichText::new(
                            "Personal storage, shared SQLite files, or Microsoft SQL Server.",
                        )
                        .size(12.)
                        .color(colors.muted),
                    );
                    ui.add_space(14.);
                    ui.separator();
                    section_label(ui, "IMPORT & EXPORT");
                    ui.label(
                        RichText::new("Transfer connection profiles using CSV files.")
                            .color(colors.muted),
                    );
                    ui.add_space(4.);
                    let button_width = (ui.available_width() - ui.spacing().item_spacing.x) / 2.;
                    ui.horizontal(|ui| {
                        ui.add_enabled_ui(self.pending.is_none(), |ui| {
                            import = outline_button(ui, "Import CSV", button_width).clicked();
                        });
                        export = outline_button(ui, "Export CSV", button_width).clicked();
                    });
                    if self.recovery.is_some() {
                        recovery = ui.button("Export unsaved changes").clicked();
                    }
                    if let Some(error) = &self.error {
                        ui.colored_label(ui.visuals().error_fg_color, error);
                    }
                    if let Some(notice) = &self.notice {
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
        if close || ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.modal = Modal::None;
        }
        if manage {
            self.modal = Modal::Library;
        }
        if import {
            // Replaces settings with the import preview once a file is read.
            self.import_csv();
        }
        if export {
            self.export_csv(false);
        }
        if recovery {
            self.export_csv(true);
        }
    }

    pub fn library_dialogs(&mut self, ctx: &egui::Context) {
        let colors = theme::palette(ctx);
        if matches!(self.modal, Modal::Library) {
            let mut open = false;
            let mut cancel = false;
            dialog("database", ctx).show(ctx, |ui| {
                ui.set_width(600.);
                ui.heading("Choose your connection library");
                ui.label(
                    RichText::new("Switch libraries without interrupting your open sessions.")
                        .color(colors.muted),
                );
                ui.add_space(16.);
                ui.horizontal(|ui| {
                    for (kind, label) in [
                        (LibraryKind::Personal, "Personal"),
                        (LibraryKind::Sqlite, "SQLite file"),
                        (LibraryKind::Mssql, "MSSQL server"),
                    ] {
                        ui.selectable_value(&mut self.db_kind, kind, label);
                    }
                });
                ui.add_space(16.);
                match self.db_kind {
                    LibraryKind::Personal => {
                        ui.label("Your personal library stays on this computer.");
                        ui.label(
                            RichText::new(self.path.display().to_string())
                                .monospace()
                                .size(12.)
                                .color(colors.muted),
                        );
                    }
                    LibraryKind::Sqlite => {
                        field(
                            ui,
                            "Database path (local, mapped drive, or UNC)",
                            &mut self.db_path,
                            "\\\\server\\share\\relay.sqlite",
                        );
                        if ui.button("Browse…").clicked() {
                            match file_dialog::choose(self.parent, false, true) {
                                Ok(Some(path)) => self.db_path = path.display().to_string(),
                                Ok(None) => {}
                                Err(e) => self.error = Some(e),
                            }
                        }
                        ui.label(
                            RichText::new(
                                "Creates a new database if the file does not exist. Existing \
                                 personal connections are not moved; use CSV import to add them.",
                            )
                            .size(12.)
                            .color(colors.muted),
                        );
                        ui.add_space(10.);
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            "For network drives, use a reliable share with file locking and \
                             backups. MSSQL is the better choice for concurrent team editing.",
                        );
                    }
                    LibraryKind::Mssql => {
                        ui.label("ADO.NET connection string");
                        let hint = "Server=tcp:sql,1433;Database=Relay;Integrated Security=true";
                        ui.add_sized(
                            [ui.available_width(), 36.],
                            egui::TextEdit::singleline(&mut self.db_mssql)
                                .password(true)
                                .hint_text(hint),
                        );
                        ui.label(
                            RichText::new(
                                "Windows integrated authentication and SQL logins are supported. \
                                 TLS is required. The string is held in memory for this session \
                                 only.",
                            )
                            .size(12.)
                            .color(colors.muted),
                        );
                        ui.add_space(10.);
                        ui.label(
                            "Run sql/mssql.sql once in an existing database. \
                             Your team needs SELECT and UPDATE on dbo.RelayLibrary.",
                        );
                    }
                }
                ui.add_space(18.);
                ui.horizontal(|ui| {
                    let idle = self.pending.is_none();
                    let button =
                        egui::Button::new(RichText::new("Open library").color(colors.bg).strong())
                            .fill(colors.accent)
                            .min_size(Vec2::new(150., 36.));
                    open = ui.add_enabled(idle, button).clicked();
                    cancel = ui.add_enabled(idle, egui::Button::new("Cancel")).clicked();
                    if !idle {
                        ui.spinner();
                        ui.label("Opening library…");
                    }
                });
                if let Some(error) = &self.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
            });
            if open {
                self.open_library();
            } else if cancel {
                self.modal = Modal::None;
            }
        }
        if let Modal::Import(import) = &self.modal {
            let mut confirm = false;
            let mut cancel = false;
            dialog("import", ctx).show(ctx, |ui| {
                ui.set_width(680.);
                ui.heading("Review CSV import");
                ui.label(RichText::new(&import.filename).color(colors.muted));
                ui.add_space(16.);
                ui.horizontal(|ui| {
                    ui.colored_label(
                        colors.accent,
                        format!("{} ready to import", import.connections.len()),
                    );
                    ui.label(format!("{} duplicates", import.duplicates));
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        format!("{} skipped", import.skipped.len()),
                    );
                });
                ui.add_space(12.);
                egui::ScrollArea::vertical()
                    .id_salt("import-preview")
                    .max_height(260.)
                    .show(ui, |ui| {
                        for c in &import.connections {
                            ui.horizontal(|ui| {
                                protocol_badge(ui, c.protocol);
                                ui.add(egui::Label::new(&c.name).truncate());
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(&c.group).size(11.).color(colors.muted),
                                    )
                                    .truncate(),
                                );
                            });
                        }
                    });
                if !import.skipped.is_empty() {
                    ui.collapsing("Skipped rows — inspect reasons", |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("skip-reasons")
                            .max_height(150.)
                            .show(ui, |ui| {
                                for reason in &import.skipped {
                                    ui.label(reason);
                                }
                            });
                    });
                }
                ui.add_space(16.);
                ui.label(
                    RichText::new(
                        "Existing connections stay unchanged. \
                         Unsupported protocols such as VNC are not imported.",
                    )
                    .size(12.)
                    .color(colors.muted),
                );
                ui.horizontal(|ui| {
                    let label = format!("Import {} connections", import.connections.len());
                    let button = egui::Button::new(RichText::new(label).color(colors.bg).strong())
                        .fill(colors.accent);
                    let ready = !import.connections.is_empty() && self.pending.is_none();
                    confirm = ui.add_enabled(ready, button).clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
            if confirm {
                let mut next = self.connections.clone();
                next.extend(import.connections.iter().cloned());
                if self.persist(next) {
                    self.modal = Modal::None;
                }
            } else if cancel {
                self.modal = Modal::None;
            }
        }
    }

    fn open_library(&mut self) {
        let location = match self.db_kind {
            LibraryKind::Personal => database::Location::Local(self.path.clone()),
            LibraryKind::Sqlite if !self.db_path.trim().is_empty() => {
                database::Location::Sqlite(PathBuf::from(self.db_path.trim()))
            }
            LibraryKind::Mssql if !self.db_mssql.trim().is_empty() => {
                database::Location::Mssql(self.db_mssql.clone())
            }
            _ => {
                self.error = Some("Enter a database path or connection string.".into());
                return;
            }
        };
        self.pending_save = false;
        self.pending_location = Some(location.clone());
        self.pending = Some(database::start(location, 0, None, self.ctx.clone()));
    }
}

fn theme_option(ui: &mut egui::Ui, candidate: theme::Theme, selected: bool) -> egui::Response {
    let colors = theme::palette(ui.ctx());
    let response = ui.add_sized(
        [ui.available_width(), 40.],
        egui::Button::new("").selected(selected),
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), candidate.name())
    });
    let center = response.rect.center();
    ui.painter().circle_filled(
        egui::pos2(response.rect.left() + 20., center.y),
        5.,
        candidate.colors().accent,
    );
    ui.painter().text(
        egui::pos2(response.rect.left() + 40., center.y),
        Align2::LEFT_CENTER,
        candidate.name(),
        FontId::proportional(14.),
        colors.text,
    );
    if selected {
        ui.painter().text(
            egui::pos2(response.rect.right() - 16., center.y),
            Align2::RIGHT_CENTER,
            "Selected",
            FontId::proportional(12.),
            colors.accent,
        );
    }
    response
}

fn section_label(ui: &mut egui::Ui, text: &str) {
    let colors = theme::palette(ui.ctx());
    ui.add_space(2.);
    ui.label(RichText::new(text).size(10.).color(colors.muted));
    ui.add_space(6.);
}
fn nav_row(ui: &mut egui::Ui, symbol: &str, label: &str, count: usize, selected: bool) -> bool {
    let colors = theme::palette(ui.ctx());
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 36.), Sense::click());
    if selected || response.hovered() {
        ui.painter().rect_filled(
            rect,
            6.,
            if selected {
                colors.selected
            } else {
                colors.hover
            },
        );
    }
    let color = if selected { colors.accent } else { colors.text };
    if symbol == "▦" {
        library_icon(ui, rect.min + Vec2::new(16., 18.), color);
    } else if symbol == "▱" {
        let center = rect.min + Vec2::new(16., 18.);
        let points = [
            (-7., -5.),
            (-2., -5.),
            (0., -3.),
            (7., -3.),
            (7., 6.),
            (-7., 6.),
        ]
        .map(|(x, y)| center + Vec2::new(x, y));
        ui.painter().add(egui::Shape::closed_line(
            points.to_vec(),
            Stroke::new(1.25_f32, color),
        ));
        ui.painter().line_segment(
            [center + Vec2::new(-7., -1.), center + Vec2::new(7., -1.)],
            Stroke::new(1.25_f32, color),
        );
    } else {
        ui.painter().text(
            rect.min + Vec2::new(10., 18.),
            Align2::LEFT_CENTER,
            symbol,
            FontId::monospace(14.),
            color,
        );
    }
    let text_rect =
        Rect::from_min_max(rect.min + Vec2::new(36., 8.), rect.max - Vec2::new(30., 4.));
    ui.scope_builder(UiBuilder::new().max_rect(text_rect), |ui| {
        ui.add(
            egui::Label::new(RichText::new(label).size(13.).color(color))
                .truncate()
                .selectable(false),
        );
    });
    ui.painter().text(
        rect.max - Vec2::new(10., 18.),
        Align2::RIGHT_CENTER,
        count.to_string(),
        FontId::monospace(11.),
        colors.muted,
    );
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(label)
        .clicked()
}
fn library_icon(ui: &egui::Ui, center: egui::Pos2, color: Color32) {
    for y in 0..2 {
        for x in 0..2 {
            let min = center + Vec2::new(-6.5 + x as f32 * 8., -6.5 + y as f32 * 8.);
            ui.painter()
                .rect_filled(Rect::from_min_size(min, Vec2::splat(5.)), 1., color);
        }
    }
}
fn tab(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
    let colors = theme::palette(ui.ctx());
    ui.add(
        egui::Button::new(RichText::new(label).size(13.).color(if selected {
            colors.text
        } else {
            colors.muted
        }))
        .fill(if selected {
            colors.surface
        } else {
            Color32::TRANSPARENT
        })
        .stroke(Stroke::NONE)
        .corner_radius(6)
        .min_size(Vec2::new(0., 36.)),
    )
}
fn session_tab(
    ui: &mut egui::Ui,
    label: &str,
    selected: bool,
) -> egui::InnerResponse<(egui::Response, egui::Response)> {
    let colors = theme::palette(ui.ctx());
    let text = if selected { colors.text } else { colors.muted };
    let label_width = ui.fonts_mut(|fonts| {
        fonts
            .layout_no_wrap(label.to_owned(), FontId::proportional(13.), text)
            .size()
            .x
    });
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(label_width + 68., 36.), Sense::hover());
    ui.painter().rect_filled(
        rect,
        6.,
        if selected {
            colors.surface
        } else {
            colors.panel
        },
    );
    ui.painter().rect_stroke(
        rect,
        6.,
        Stroke::new(1_f32, colors.line),
        egui::StrokeKind::Inside,
    );
    let select_rect =
        Rect::from_min_max(rect.min + Vec2::new(4., 0.), rect.max - Vec2::new(36., 0.));
    let close_rect = Rect::from_center_size(
        egui::pos2(rect.right() - 18., rect.center().y),
        Vec2::splat(28.),
    );
    let select = ui.put(
        select_rect,
        egui::Button::new(RichText::new(label).size(13.).color(text)).frame(false),
    );
    let close = ui
        .put(
            close_rect,
            egui::Button::new(RichText::new("×").size(17.))
                .small()
                .frame(false),
        )
        .on_hover_text(format!("Close {label}"));
    egui::InnerResponse {
        inner: (select, close),
        response,
    }
}
fn outline_button(ui: &mut egui::Ui, label: &str, width: f32) -> egui::Response {
    let colors = theme::palette(ui.ctx());
    ui.add_sized(
        [width, 36.],
        egui::Button::new(RichText::new(label).size(13.))
            .fill(colors.surface)
            .stroke(Stroke::new(1_f32, colors.line))
            .corner_radius(6),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tab_close_is_inside_tab_and_does_not_select_it() {
        for close_target in [false, true] {
            let ctx = egui::Context::default();
            let frame = |events| {
                let mut result = None;
                let _ = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(
                            egui::Pos2::ZERO,
                            Vec2::new(500., 100.),
                        )),
                        events,
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            ui.horizontal(|ui| {
                                let session = session_tab(ui, "RDP  Design workstation", true);
                                assert!(session.response.rect.contains_rect(session.inner.1.rect));
                                assert!(!session.inner.0.rect.intersects(session.inner.1.rect));
                                let next = session_tab(ui, "SSH  Build server", false);
                                assert_eq!(
                                    session.response.rect.center().y,
                                    next.response.rect.center().y
                                );
                                assert!(session.response.rect.right() < next.response.rect.left());
                                result = Some((
                                    session.inner.0.clicked(),
                                    session.inner.1.clicked(),
                                    if close_target {
                                        session.inner.1.rect.center()
                                    } else {
                                        session.inner.0.rect.center()
                                    },
                                ));
                            });
                        });
                    },
                );
                result.unwrap()
            };
            let (_, _, pos) = frame(vec![]);
            frame(vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
            let (selected, closed, _) = frame(vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }]);
            assert_eq!(selected, !close_target);
            assert_eq!(closed, close_target);
        }
    }

    #[test]
    fn navigation_and_theme_clicks_include_label_and_both_edges() {
        for (theme_row, x) in [
            (false, 18.),
            (false, 85.),
            (false, 242.),
            (true, 18.),
            (true, 85.),
            (true, 242.),
        ] {
            let ctx = egui::Context::default();
            let frame = |events| {
                let mut clicked = false;
                let output = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(
                            egui::Pos2::ZERO,
                            Vec2::new(260., 80.),
                        )),
                        events,
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            clicked = if theme_row {
                                theme_option(ui, theme::Theme::Midnight, false).clicked()
                            } else {
                                nav_row(ui, "▱", "Infrastructure", 10, false)
                            };
                        });
                    },
                );
                (clicked, output.platform_output.cursor_icon)
            };
            frame(vec![]);
            let pos = egui::pos2(x, 26.);
            let (_, cursor) = frame(vec![egui::Event::PointerMoved(pos)]);
            assert_ne!(cursor, egui::CursorIcon::Text, "text cursor at x={x}");
            frame(vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }]);
            let (clicked, _) = frame(vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }]);
            assert!(clicked, "navigation row ignored click at x={x}");
        }
    }
}
