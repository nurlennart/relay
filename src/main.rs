#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod csv_io;
mod database;
mod file_dialog;
mod model;
mod native_host;
mod rdp;
mod session;
mod terminal;
mod theme;
mod ui;
#[cfg(feature = "ui-check")]
mod ui_check;

use eframe::egui::{self, Color32, RichText, Stroke, Vec2};
use model::{Connection, Protocol};
use raw_window_handle::HasWindowHandle;
use session::Session;
use std::{path::PathBuf, time::Duration};
use windows_sys::Win32::Foundation::HWND;

fn main() -> eframe::Result {
    eframe::run_native(
        "Relay · Remote workspace",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1280., 820.])
                .with_min_inner_size([1080., 650.]),
            ..Default::default()
        },
        Box::new(|cc| {
            #[allow(unused_mut)]
            let mut app = App::new(cc);
            #[cfg(feature = "ui-check")]
            ui_check::seed(&mut app);
            Ok(Box::new(app))
        }),
    )
}

#[derive(PartialEq)]
enum Filter {
    All,
    Favorites,
    Rdp,
    Ssh,
}

struct App {
    connections: Vec<Connection>,
    sessions: Vec<Session>,
    active: Option<usize>,
    filter: Filter,
    query: String,
    editor: Option<Connection>,
    editor_error: String,
    delete: Option<u64>,
    close: Option<usize>,
    error: Option<String>,
    storage_ok: bool,
    path: PathBuf,
    parent: HWND,
    search_focus: bool,
    quitting: bool,
    ctx: egui::Context,
    location: database::Location,
    revision: i64,
    pending: Option<std::sync::mpsc::Receiver<Result<database::Snapshot, String>>>,
    pending_location: Option<database::Location>,
    recovery: Option<Vec<Connection>>,
    pending_editor: Option<Connection>,
    db_open: bool,
    settings_open: bool,
    db_kind: usize,
    db_path: String,
    db_mssql: String,
    import: Option<csv_io::Import>,
    notice: Option<String>,
    group: Option<String>,
    selected: Option<u64>,
    pending_save: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_theme(egui::Theme::Dark);
        let mut fonts = egui::FontDefinitions::default();
        if let Ok(bytes) = std::fs::read("C:\\Windows\\Fonts\\segoeui.ttf") {
            fonts
                .font_data
                .insert("segoe".into(), egui::FontData::from_owned(bytes).into());
            fonts
                .families
                .get_mut(&egui::FontFamily::Proportional)
                .unwrap()
                .insert(0, "segoe".into());
        }
        cc.egui_ctx.set_fonts(fonts);
        theme::load(&theme::path()).apply(&cc.egui_ctx);
        let mut style = (*cc.egui_ctx.style()).clone();
        style.spacing.window_margin = egui::Margin::same(24);
        style.spacing.item_spacing = Vec2::new(10., 10.);
        style.spacing.button_padding = Vec2::new(12., 8.);
        style
            .text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(14.));
        style
            .text_styles
            .insert(egui::TextStyle::Button, egui::FontId::proportional(14.));
        cc.egui_ctx.set_style(style);
        let path = model::data_path();
        let location = database::Location::Local(path.clone());
        let (connections, error) = match model::load(&path) {
            Ok(c) => (c, None),
            Err(e) => (vec![], Some(e)),
        };
        let parent = match cc.window_handle().map(|h| h.as_raw()) {
            Ok(raw_window_handle::RawWindowHandle::Win32(h)) => h.hwnd.get() as HWND,
            _ => std::ptr::null_mut(),
        };
        Self {
            connections,
            sessions: vec![],
            active: None,
            filter: Filter::All,
            query: String::new(),
            editor: None,
            editor_error: String::new(),
            delete: None,
            close: None,
            storage_ok: error.is_none(),
            error,
            path,
            parent,
            search_focus: false,
            quitting: false,
            ctx: cc.egui_ctx.clone(),
            location, revision: 0, pending: None, pending_location: None, recovery: None, pending_editor: None,
            settings_open: false, db_open: false, db_kind: 0, db_path: String::new(),
            db_mssql: "Server=tcp:sql.example.com,1433;Database=Relay;Integrated Security=true;Encrypt=true;TrustServerCertificate=false".into(),
            import: None, notice: None, group: None, selected: None, pending_save: false,
        }
    }

    fn persist(&mut self, next: Vec<Connection>) -> bool {
        if !self.storage_ok {
            self.error = Some("Repair the saved connections file and restart Relay before making changes. Your existing file has been preserved.".into());
            return false;
        }
        if self.pending.is_some() {
            return false;
        }
        self.recovery = Some(next.clone());
        self.pending_editor = self.editor.clone();
        self.pending_save = true;
        self.pending = Some(database::start(
            self.location.clone(),
            self.revision,
            Some(next),
            self.ctx.clone(),
        ));
        true
    }

    fn connect(&mut self, connection: Connection) {
        if self.parent.is_null() {
            self.error = Some("Cannot obtain the native application window.".into());
            return;
        }
        match Session::start(connection, self.ctx.clone()) {
            Ok(session) => {
                for s in &mut self.sessions {
                    s.place(None);
                }
                self.sessions.push(session);
                self.active = Some(self.sessions.len() - 1);
            }
            Err(e) => self.error = Some(e),
        }
    }

    fn new_connection(&mut self) {
        if self.pending.is_some() {
            self.notice = Some("Wait for the current library operation to finish.".into());
            return;
        }
        self.editor = Some(Connection::new());
        self.editor_error.clear();
    }

    fn editor(&mut self, ctx: &egui::Context) {
        let colors = theme::palette(ctx);
        let Some(mut c) = self.editor.clone() else {
            return;
        };
        let mut save = false;
        let mut cancel = false;
        dialog("edit-connection", ctx).show(ctx, |ui| {
            ui.set_width(440.); ui.add_space(8.);
            ui.label(RichText::new(if c.id == 0 { "New connection" } else { "Edit connection" }).size(25.).strong());
            ui.label(RichText::new("A familiar name. A machine to reach.").color(colors.muted)); ui.add_space(15.);
            ui.horizontal(|ui| {
                for protocol in [Protocol::Rdp, Protocol::Ssh] {
                    if ui.add_sized([210., 40.], egui::Button::new(protocol.label()).selected(c.protocol == protocol)).clicked() {
                        if c.port == c.protocol.port() { c.port = protocol.port(); } c.protocol = protocol;
                    }
                }
            }); ui.add_space(10.);
            field(ui, "Connection name", &mut c.name, "e.g. Office workstation");
            ui.horizontal(|ui| {
                ui.vertical(|ui| { ui.label("Host / IP address"); ui.add_sized([330., 32.], egui::TextEdit::singleline(&mut c.host).hint_text("e.g. 192.168.1.20")); });
                ui.vertical(|ui| { ui.label("Port"); ui.add_sized([90., 32.], egui::DragValue::new(&mut c.port).range(1..=65535).speed(1)); });
            });
            field(ui, "Username (optional)", &mut c.username, if c.protocol == Protocol::Rdp { "DOMAIN\\username" } else { "username" });
            field(ui, "Group", &mut c.group, "Personal");
            ui.checkbox(&mut c.favorite, "Add to favorites"); ui.add_space(8.);
            ui.label(RichText::new(if c.protocol == Protocol::Rdp { "Credentials are handled by Windows Remote Desktop.\nPasswords are never stored by Relay." } else { "Uses Windows OpenSSH, including your SSH config and agent.\nHost verification and password prompts appear in the terminal." }).size(12.).color(colors.muted));
            if !self.editor_error.is_empty() { ui.colored_label(ui.visuals().error_fg_color, &self.editor_error); }
            ui.add_space(14.);
            ui.horizontal(|ui| {
                if accent_button(ui, "Save connection", 175.).clicked() { save = true; }
                if ui.button("Cancel").clicked() { cancel = true; }
            }); ui.add_space(6.);
        });
        if cancel {
            self.editor = None;
        } else if save {
            match c.validate() {
                Err(e) => {
                    self.editor_error = e;
                    self.editor = Some(c);
                }
                Ok(()) => {
                    let mut next = self.connections.clone();
                    if c.id == 0 {
                        c.id = next
                            .iter()
                            .map(|c| c.id)
                            .max()
                            .unwrap_or(0)
                            .saturating_add(1);
                        next.push(c);
                    } else if let Some(existing) = next.iter_mut().find(|v| v.id == c.id) {
                        *existing = c;
                    }
                    if self.persist(next) {
                        self.editor = None;
                    }
                }
            }
        } else {
            self.editor = Some(c);
        }
    }

    fn confirmations(&mut self, ctx: &egui::Context) {
        if self.quitting {
            dialog("quit", ctx).show(ctx, |ui| {
                ui.set_width(380.);
                ui.heading("Close Relay?");
                ui.label("Save your work on each remote machine before closing.");
                if self.recovery.is_some(){ui.colored_label(ui.visuals().error_fg_color,"There are unsaved library changes in memory. Export them before closing or they will be lost.");}
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            self.pending.is_none(),
                            egui::Button::new("End sessions and quit"),
                        )
                        .clicked()
                    {
                        self.sessions.clear();
                        self.active = None;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if ui.button("Keep working").clicked() {
                        self.quitting = false;
                    }
                });
            });
            return;
        }
        if let Some(id) = self.delete {
            dialog("delete", ctx).show(ctx, |ui| {
                ui.set_width(360.);
                ui.heading("Delete saved connection?");
                ui.label("This removes the saved entry. Any open session stays open.");
                ui.horizontal(|ui| {
                    if ui.button("Delete connection").clicked() {
                        let next = self
                            .connections
                            .iter()
                            .filter(|c| c.id != id)
                            .cloned()
                            .collect();
                        if self.persist(next) {
                            self.delete = None;
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        self.delete = None;
                    }
                });
            });
        }
        if let Some(index) = self.close {
            dialog("close", ctx).show(ctx, |ui| {
                ui.set_width(380.);
                ui.heading("Close this session?");
                ui.label("The connection will end. Save your work on the remote machine first.");
                ui.horizontal(|ui| {
                    if ui.button("Close session").clicked() {
                        self.sessions.remove(index);
                        self.active = match self.active {
                            Some(a) if a == index => None,
                            Some(a) if a > index => Some(a - 1),
                            other => other,
                        };
                        self.close = None;
                    }
                    if ui.button("Keep open").clicked() {
                        self.close = None;
                    }
                });
            });
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let colors = theme::palette(ctx);
        self.poll_storage();
        #[cfg(feature = "ui-check")]
        ui_check::capture(ctx);
        if ctx.input(|i| i.viewport().close_requested()) && self.pending.is_some() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.notice =
                Some("Wait for the library operation to finish before closing Relay.".into());
        }
        if ctx.input(|i| i.viewport().close_requested())
            && (self.sessions.iter().any(|s| s.is_running()) || self.recovery.is_some())
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.quitting = true;
        }
        let terminal_focused = self
            .active
            .and_then(|i| self.sessions.get(i))
            .and_then(|s| s.terminal.as_ref())
            .is_some_and(|t| t.focused);
        if !terminal_focused
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::N))
        {
            self.new_connection();
        }
        if !terminal_focused
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::K))
        {
            self.search_focus = true;
            self.active = None;
        }
        if !terminal_focused
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::W))
        {
            self.close = self.active;
        }
        for s in &mut self.sessions {
            s.poll(self.parent);
        }
        // Hide native children before painting overlays; native child HWNDs sit above GPU content.
        let modal = self.editor.is_some()
            || self.delete.is_some()
            || self.close.is_some()
            || self.quitting
            || self.settings_open
            || self.db_open
            || self.import.is_some();
        for (i, s) in self.sessions.iter_mut().enumerate() {
            if self.active != Some(i) || modal {
                s.place(None);
            }
        }
        self.sidebar(ctx);
        self.tabs(ctx);
        egui::TopBottomPanel::bottom("status")
            .frame(
                egui::Frame::new()
                    .fill(colors.panel)
                    .inner_margin(egui::Margin::symmetric(18, 8)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "{} open sessions",
                            self.sessions.iter().filter(|s| s.is_running()).count()
                        ))
                        .size(11.)
                        .color(colors.accent),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!(
                                "{}  ·  Remote passwords never stored",
                                self.location.label()
                            ))
                            .size(11.)
                            .color(colors.muted),
                        )
                        .on_hover_text(self.path.display().to_string());
                    });
                });
            });
        let mut session_rect = None;
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(colors.bg).inner_margin(24.))
            .show(ctx, |ui| {
                if let Some(error) = self.error.clone() {
                    egui::Frame::new()
                        .fill(colors.error_bg)
                        .inner_margin(12.)
                        .corner_radius(6)
                        .show(ui, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.colored_label(ui.visuals().error_fg_color, error);
                                if ui.small_button("Dismiss").clicked() {
                                    self.error = None;
                                }
                            });
                        });
                    ui.add_space(12.);
                }
                if let Some(notice) = self.notice.clone() {
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(colors.accent, notice);
                        if ui.small_button("×").clicked() {
                            self.notice = None;
                        }
                    });
                    ui.add_space(10.);
                }
                if self.recovery.is_some() && self.pending.is_none() {
                    ui.horizontal(|ui| {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            "Unsaved changes are held in memory.",
                        );
                        if ui.button("Export unsaved changes").clicked() {
                            self.export_csv(true);
                        }
                    });
                    ui.add_space(10.);
                }
                if let Some(index) = self.active {
                    let s = &self.sessions[index];
                    ui.horizontal(|ui| {
                        protocol_badge(ui, s.connection.protocol);
                        ui.label(RichText::new(&s.connection.name).size(19.).strong());
                        ui.label(
                            RichText::new(s.connection.address())
                                .monospace()
                                .size(12.)
                                .color(colors.muted),
                        );
                    });
                    ui.add_space(10.);
                    if s.terminal.is_some() {
                        let reconnect = !s.is_running();
                        let connection = s.connection.clone();
                        if reconnect && ui.button("Reconnect in new tab").clicked() {
                            self.connect(connection);
                        }
                        self.sessions[index].terminal.as_mut().unwrap().show(ui);
                    } else if let Some(error) = &s.error {
                        let error = error.clone();
                        let connection = s.connection.clone();
                        ui.add_space(40.);
                        ui.label(RichText::new(error).size(18.));
                        ui.add_space(12.);
                        if ui.button("Reconnect in new tab").clicked() {
                            self.connect(connection);
                        }
                    } else if s.window.is_null() {
                        ui.add_space(50.);
                        ui.spinner();
                        ui.heading("Opening your session…");
                        ui.label(
                            RichText::new(
                                "Complete any Windows authentication prompt to continue.",
                            )
                            .color(colors.muted),
                        );
                        if s.started.elapsed().as_secs() > 20 {
                            ui.label("Waiting for the Windows RDP control to initialize.");
                        }
                    } else {
                        if let Some(warning) = s.display_warning() {
                            ui.colored_label(ui.visuals().warn_fg_color, warning);
                        }
                        session_rect = Some(ui.available_rect_before_wrap());
                    }
                } else {
                    self.workspace(ui);
                }
            });
        self.editor(ctx);
        self.confirmations(ctx);
        self.settings(ctx);
        self.library_dialogs(ctx);
        let modal = self.editor.is_some()
            || self.delete.is_some()
            || self.close.is_some()
            || self.quitting
            || self.settings_open
            || self.db_open
            || self.import.is_some();
        let ppp = ctx.pixels_per_point();
        for (i, s) in self.sessions.iter_mut().enumerate() {
            let rect = if !modal && self.active == Some(i) {
                session_rect.map(|r| {
                    [
                        (r.min.x * ppp).round() as i32,
                        (r.min.y * ppp).round() as i32,
                        (r.width() * ppp).round() as i32,
                        (r.height() * ppp).round() as i32,
                    ]
                })
            } else {
                None
            };
            s.place(rect);
        }
        if !self.sessions.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
    }
}

fn accent_button(ui: &mut egui::Ui, text: &str, width: f32) -> egui::Response {
    let colors = theme::palette(ui.ctx());
    ui.add_sized(
        [width, 37.],
        egui::Button::new(RichText::new(text).color(colors.bg).strong())
            .fill(colors.accent)
            .corner_radius(6),
    )
}
fn dialog(id: &'static str, ctx: &egui::Context) -> egui::Modal {
    let colors = theme::palette(ctx);
    egui::Modal::new(egui::Id::new(id)).frame(
        egui::Frame::new()
            .fill(colors.panel)
            .inner_margin(24.)
            .corner_radius(12)
            .stroke(Stroke::new(1_f32, colors.line)),
    )
}
fn protocol_badge(ui: &mut egui::Ui, protocol: Protocol) {
    let colors = theme::palette(ui.ctx());
    let color = match protocol {
        Protocol::Rdp => colors.rdp,
        Protocol::Ssh => colors.accent,
    };
    egui::Frame::new()
        .fill(color.gamma_multiply(0.13))
        .corner_radius(4)
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.label(
                RichText::new(protocol.label())
                    .monospace()
                    .size(11.)
                    .color(color),
            );
        });
}
fn field(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str) {
    ui.label(label);
    ui.add_sized(
        [ui.available_width(), 32.],
        egui::TextEdit::singleline(value).hint_text(hint),
    );
}
