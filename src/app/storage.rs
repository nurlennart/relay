//! Loading and saving libraries, switching between them, and import and export.
use super::*;

/// How often a shared library is reloaded in the background.
const SYNC_INTERVAL: Duration = Duration::from_secs(60);
/// Regaining focus reloads only if the last reload is at least this old, so switching windows
/// back and forth does not query the server each time.
const SYNC_FOCUS_MIN: Duration = Duration::from_secs(5);

impl App {
    pub(super) fn refresh(&mut self) {
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

    pub(super) fn poll_storage(&mut self) {
        let result = self.pending.as_ref().map(|rx| rx.try_recv());
        if let Some(result) = result {
            match result {
                Ok(Ok(snapshot)) => {
                    self.connections = snapshot.connections;
                    self.revision = snapshot.revision;
                    self.storage_ok = true;
                    let mut remember_error = None;
                    let saved = self.pending_save;
                    let opened = self.pending_location.as_ref().map(|l| l.label());
                    if let Some(location) = self.pending_location.take() {
                        remember_error = self.remember_server(&location).err();
                        self.state.use_library(&location);
                        self.save_state();
                        self.location = location;
                        if matches!(self.modal, Modal::Library) {
                            self.modal = Modal::None;
                        }
                        self.group = None;
                        self.filter = Filter::All;
                        for form in [&mut self.db_mssql, &mut self.db_mysql] {
                            if !form.remember {
                                form.connection.clear();
                            }
                        }
                    }
                    if self.pending_save {
                        self.recovery = None;
                    }
                    self.pending_save = false;
                    self.pending_editor = None;
                    self.pending = None;
                    self.error = remember_error;
                    self.forget_removed();
                    // Routine saves stay silent; loads say what happened.
                    if let Some(opened) = opened {
                        self.notify(format!("Opened the {opened}."));
                    } else if !saved {
                        self.notify("Library refreshed.");
                    }
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

    /// Reloads a shared library every minute and when the window regains focus, so teammates'
    /// changes appear on their own. It is silent: nothing is announced, no control waits on it,
    /// failures are dropped, and the list is replaced only when the library's revision changed.
    pub(super) fn sync_library(&mut self, ctx: &egui::Context) {
        let focused = ctx.input(|i| i.focused);
        let regained = focused && !self.was_focused;
        self.was_focused = focused;
        // Loads, saves and switches take precedence; their result is the newer one.
        if self.pending.is_some() {
            self.sync = None;
        }
        if let Some(rx) = &self.sync {
            match rx.try_recv() {
                Ok(result) => {
                    self.sync = None;
                    if let Ok(snapshot) = result
                        && snapshot.revision != self.revision
                        && self.recovery.is_none()
                    {
                        self.connections = snapshot.connections;
                        self.revision = snapshot.revision;
                        self.forget_removed();
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.sync = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        // The personal library has no other writers. Unsaved changes after a rejected save
        // stay on screen until the user deals with them.
        if matches!(self.location, database::Location::Local(_))
            || !self.storage_ok
            || self.recovery.is_some()
        {
            return;
        }
        let elapsed = self.synced.elapsed();
        let due = elapsed >= SYNC_INTERVAL || (regained && elapsed >= SYNC_FOCUS_MIN);
        if due && self.sync.is_none() && self.pending.is_none() {
            self.synced = Instant::now();
            self.sync = Some(database::start(
                self.location.clone(),
                self.revision,
                None,
                self.ctx.clone(),
            ));
        }
        ctx.request_repaint_after(SYNC_INTERVAL.saturating_sub(self.synced.elapsed()));
    }

    pub(super) fn import_csv(&mut self) {
        for s in &mut self.sessions {
            s.place(None);
        }
        if let Some(path) = file_dialog::choose(self.parent, file_dialog::Kind::ImportCsv).first() {
            self.review(csv_io::read(path, &self.connections));
        }
    }

    /// Imports a Relay JSON export or a personal library file for review.
    pub(super) fn import_json(&mut self) {
        for s in &mut self.sessions {
            s.place(None);
        }
        if let Some(path) = file_dialog::choose(self.parent, file_dialog::Kind::ImportJson).first()
        {
            self.review(json_io::read(path, &self.connections));
        }
    }

    /// Imports the user's OpenSSH configuration for review.
    pub(super) fn import_ssh_config(&mut self) {
        match import::ssh_config_path() {
            Some(path) if path.exists() => {
                self.review(import::read_ssh_config(&path, &self.connections));
            }
            Some(path) => {
                self.error = Some(format!("There is no SSH config at {}.", path.display()));
            }
            None => self.error = Some("Cannot find your user profile folder.".into()),
        }
    }

    /// Imports one or more .rdp files for review.
    pub(super) fn import_rdp_files(&mut self) {
        for s in &mut self.sessions {
            s.place(None);
        }
        let paths = file_dialog::choose(self.parent, file_dialog::Kind::RdpFiles);
        if paths.is_empty() {
            return;
        }
        let files: Result<Vec<_>, String> = paths
            .iter()
            .map(|path| {
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into();
                std::fs::read(path)
                    .map(|bytes| (name, bytes))
                    .map_err(|e| format!("Cannot read {}: {e}", path.display()))
            })
            .collect();
        self.review(files.and_then(|files| import::rdp_files(&files, &self.connections)));
    }

    /// Shows an import for review before anything is saved.
    pub(super) fn review(&mut self, import: Result<csv_io::Import, String>) {
        match import {
            Ok(import) => self.modal = Modal::Import(import),
            Err(e) => self.error = Some(e),
        }
    }

    /// Exports the library as CSV, or as JSON with every setting (`Kind::ExportJson`).
    /// Unsaved changes are always exported as JSON, so nothing in them is lost.
    pub(super) fn export(&mut self, kind: file_dialog::Kind, recovery: bool) {
        for s in &mut self.sessions {
            s.place(None);
        }
        let kind = if recovery {
            file_dialog::Kind::ExportJson
        } else {
            kind
        };
        if let Some(path) = file_dialog::choose(self.parent, kind).into_iter().next() {
            let connections = if recovery {
                self.recovery.as_deref().unwrap_or(&self.connections)
            } else {
                &self.connections
            };
            let written = if kind == file_dialog::Kind::ExportJson {
                json_io::export(&path, connections)
            } else {
                csv_io::export(&path, connections)
            };
            match written {
                Ok(()) => {
                    let text = format!(
                        "Exported {} connections to {}",
                        connections.len(),
                        path.display()
                    );
                    self.notify(text);
                }
                Err(e) => self.error = Some(e),
            }
        }
    }

    pub(super) fn library_dialogs(&mut self, ctx: &egui::Context) {
        let colors = theme::palette(ctx);
        let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if matches!(self.modal, Modal::Library) {
            let mut open = false;
            let mut cancel = false;
            dialog("database", ctx).show(ctx, |ui| {
                ui.set_width(dialog_width(ctx, 600.));
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
                        (LibraryKind::Mysql, "MySQL server"),
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
                        ui.horizontal(|ui| {
                            for (label, kind) in [
                                ("Open existing…", file_dialog::Kind::Database),
                                ("Create new…", file_dialog::Kind::NewDatabase),
                            ] {
                                if ui.button(label).clicked()
                                    && let Some(path) =
                                        file_dialog::choose(self.parent, kind).first()
                                {
                                    self.db_path = path.display().to_string();
                                }
                            }
                        });
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
                             backups. A database server is the better choice for concurrent \
                             team editing.",
                        );
                    }
                    LibraryKind::Mssql | LibraryKind::Mysql => {
                        let mysql = self.db_kind == LibraryKind::Mysql;
                        let form = if mysql {
                            &mut self.db_mysql
                        } else {
                            &mut self.db_mssql
                        };
                        let (label, hint) = if mysql {
                            (
                                "Connection URL",
                                "mysql://user:password@db.example.com:3306/relay",
                            )
                        } else {
                            (
                                "ADO.NET connection string",
                                "Server=tcp:sql,1433;Database=Relay;Integrated Security=true",
                            )
                        };
                        ui.label(label);
                        ui.horizontal(|ui| {
                            let show = 64.;
                            ui.add_sized(
                                [
                                    ui.available_width() - show - ui.spacing().item_spacing.x,
                                    CONTROL_HEIGHT,
                                ],
                                input(&mut form.connection, hint).password(!self.db_reveal),
                            );
                            let reveal = egui::Button::new("Show")
                                .selected(self.db_reveal)
                                .min_size(Vec2::new(show, CONTROL_HEIGHT));
                            if ui.add(reveal).clicked() {
                                self.db_reveal = !self.db_reveal;
                            }
                        });
                        ui.checkbox(
                            &mut form.remember,
                            "Remember on this computer (in your system credential store)",
                        );
                        ui.checkbox(
                            &mut form.tls,
                            "Encrypt the connection with TLS (recommended)",
                        );
                        if !mysql
                            && let Some(encrypt) = database::ado_encrypt(&form.connection)
                            && encrypt != form.tls
                        {
                            ui.colored_label(
                                ui.visuals().warn_fg_color,
                                if form.tls {
                                    "The connection string says Encrypt=false, but this \
                                     setting decides: the connection will use TLS."
                                } else {
                                    "The connection string asks for encryption, but this \
                                     setting decides: the connection will not use TLS."
                                },
                            );
                        }
                        let storage = if form.remember {
                            "Saved to your credential store after the library opens."
                        } else {
                            "Held in memory for this session only. \
                             Opening without this option removes a saved string."
                        };
                        let auth = match (mysql, cfg!(windows)) {
                            (true, _) => "MySQL and MariaDB logins are supported.",
                            (false, true) => {
                                "Windows integrated authentication and SQL logins are supported."
                            }
                            (false, false) => "SQL logins are supported.",
                        };
                        ui.label(
                            RichText::new(format!("{auth} {storage}"))
                                .size(12.)
                                .color(colors.muted),
                        );
                        if !form.tls {
                            ui.colored_label(
                                ui.visuals().warn_fg_color,
                                "Without TLS, the password and your connection library cross the \
                                 network unencrypted. Use this only on a network you trust.",
                            );
                        } else if mysql {
                            ui.label(
                                RichText::new(
                                    "The server's certificate must be trusted. For a private \
                                     certificate authority, add ?root_cert_path=/path/to/ca.pem \
                                     to the URL.",
                                )
                                .size(12.)
                                .color(colors.muted),
                            );
                        }
                        ui.add_space(4.);
                        egui::CollapsingHeader::new(
                            RichText::new("First-time setup")
                                .size(12.)
                                .color(colors.muted),
                        )
                        .id_salt(("database-setup", mysql))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(if mysql {
                                    "Existing libraries are ready to use. For a new library, ask \
                                     your database administrator to run sql/mysql.sql once and \
                                     give your team read and update access to RelayLibrary."
                                } else {
                                    "Existing libraries are ready to use. For a new library, ask \
                                     your database administrator to run sql/mssql.sql once and \
                                     give your team read and update access to dbo.RelayLibrary."
                                })
                                .size(12.)
                                .color(colors.muted),
                            );
                        });
                    }
                }
                ui.add_space(12.);
                ui.horizontal(|ui| {
                    let idle = self.pending.is_none();
                    let button =
                        egui::Button::new(RichText::new("Open library").color(colors.bg).strong())
                            .fill(colors.accent)
                            .min_size(Vec2::new(130., CONTROL_HEIGHT));
                    open = ui.add_enabled(idle, button).clicked();
                    cancel = ui
                        .add_enabled_ui(idle, |ui| outline_button(ui, "Cancel", 80.))
                        .inner
                        .clicked();
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
            } else if cancel || escape && self.pending.is_none() {
                self.modal = Modal::None;
            }
        }
        if let Modal::Import(import) = &self.modal {
            let mut confirm = false;
            let mut cancel = false;
            dialog("import", ctx).show(ctx, |ui| {
                ui.set_width(dialog_width(ctx, 680.));
                ui.heading("Review import");
                ui.label(
                    RichText::new(format!(
                        "{}, into the {}",
                        import.filename,
                        self.location.label()
                    ))
                    .color(colors.muted),
                );
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
                    .max_height((ctx.content_rect().height() - 380.).clamp(100., 260.))
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
                        .fill(colors.accent)
                        .min_size(Vec2::new(0., CONTROL_HEIGHT));
                    let ready = !import.connections.is_empty() && self.pending.is_none();
                    confirm = ui.add_enabled(ready, button).clicked();
                    cancel = outline_button(ui, "Cancel", 80.).clicked();
                });
            });
            if confirm {
                match import.merged(&self.connections) {
                    Ok(next) => {
                        if self.persist(next) {
                            self.modal = Modal::None;
                        }
                    }
                    Err(e) => self.error = Some(e),
                }
            } else if cancel || escape {
                self.modal = Modal::None;
            }
        }
    }

    /// Stores or removes a server library's connection string after it opened successfully.
    /// Saves or forgets the connection string of a server library that just opened, as its
    /// form asks, and notes whether it is now saved.
    pub(super) fn remember_server(&mut self, location: &database::Location) -> Result<(), String> {
        let (form, account, server) = match location {
            database::Location::Mssql(server) => (&self.db_mssql, secret::Account::Mssql, server),
            database::Location::Mysql(server) => (&self.db_mysql, secret::Account::Mysql, server),
            database::Location::Local(_) | database::Location::Sqlite(_) => return Ok(()),
        };
        if form.remember {
            let saved = secret::save(account, &server.connection);
            self.server_remembered = saved.is_ok();
            saved.map_err(|e| {
                format!("The library opened, but the connection string could not be saved: {e}")
            })
        } else {
            // A string that could not be removed is still there, and reopens the library.
            let forgotten = secret::forget(account);
            self.server_remembered = forgotten.is_err();
            forgotten.map_err(|e| {
                format!(
                    "The library opened, but the saved connection string could not be removed: {e}"
                )
            })
        }
    }

    pub(super) fn open_library(&mut self) {
        let location = match self.db_kind {
            LibraryKind::Personal => database::Location::Local(self.path.clone()),
            LibraryKind::Sqlite if !self.db_path.trim().is_empty() => {
                database::Location::Sqlite(PathBuf::from(self.db_path.trim()))
            }
            LibraryKind::Mssql if !self.db_mssql.connection.trim().is_empty() => {
                database::Location::Mssql(self.db_mssql.server())
            }
            LibraryKind::Mysql if !self.db_mysql.connection.trim().is_empty() => {
                database::Location::Mysql(self.db_mysql.server())
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
