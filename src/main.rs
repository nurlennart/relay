#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod csv_io;
mod database;
mod file_dialog;
mod model;
mod native_host;
mod rdp;
mod secret;
mod session;
mod state;
mod terminal;
mod theme;
mod ui;
#[cfg(feature = "ui-check")]
mod ui_check;

use eframe::egui::{self, Color32, RichText, Stroke, Vec2};
use model::{Connection, Protocol};
use raw_window_handle::HasWindowHandle;
use session::Session;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use windows_sys::Win32::Foundation::HWND;

fn main() -> eframe::Result {
    // Rendering checks start from defaults and never touch the user's saved state.
    let check = cfg!(feature = "ui-check") && std::env::var_os("RELAY_UI_CHECK").is_some();
    let saved = if check {
        state::State::default()
    } else {
        state::load(&state::path())
    };
    eframe::run_native(
        "Relay · Remote workspace",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size(saved.window.unwrap_or([1280., 820.]))
                .with_maximized(saved.maximized)
                .with_min_inner_size([720., 560.])
                .with_icon(egui::IconData {
                    // Rendered by assets/make_icon.py.
                    rgba: include_bytes!("../assets/icon.rgba").to_vec(),
                    width: 256,
                    height: 256,
                }),
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
    Recent,
    Favorites,
    Rdp,
    Ssh,
}

/// The dialog covering the workspace. Only one is open at a time.
#[derive(Default)]
enum Modal {
    #[default]
    None,
    Editor(Connection),
    Delete(u64),
    Close(usize),
    Settings,
    Library,
    Import(csv_io::Import),
}

#[derive(Clone, Copy, PartialEq)]
enum LibraryKind {
    Personal,
    Sqlite,
    Mssql,
}

const MSSQL_EXAMPLE: &str = "Server=tcp:sql.example.com,1433;Database=Relay;\
    Integrated Security=true;Encrypt=true;TrustServerCertificate=false";

struct App {
    connections: Vec<Connection>,
    sessions: Vec<Session>,
    active: Option<usize>,
    filter: Filter,
    query: String,
    modal: Modal,
    /// The quit confirmation overlays `modal`, which returns when the user keeps working.
    quitting: bool,
    editor_error: String,
    error: Option<String>,
    storage_ok: bool,
    path: PathBuf,
    parent: HWND,
    search_focus: bool,
    ctx: egui::Context,
    location: database::Location,
    revision: i64,
    pending: Option<std::sync::mpsc::Receiver<Result<database::Snapshot, String>>>,
    pending_location: Option<database::Location>,
    recovery: Option<Vec<Connection>>,
    pending_editor: Option<Connection>,
    db_kind: LibraryKind,
    db_path: String,
    db_mssql: String,
    /// Keep the MSSQL connection string in DPAPI storage after it opens successfully.
    db_remember: bool,
    db_reveal: bool,
    /// A short confirmation shown as a toast until it expires.
    notice: Option<(String, Instant)>,
    group: Option<String>,
    selected: Option<u64>,
    /// Scroll the selected row into view on the next frame.
    scroll_to: bool,
    /// The connection list's last scroll offset and visible height.
    list_view: (f32, f32),
    /// Window size, sidebar, last library, and recents, kept between launches.
    state: state::State,
    /// Where `state` is saved; tests leave it unset.
    state_path: Option<PathBuf>,
    /// The tab shown last frame, to focus a terminal when its tab opens.
    shown: Option<usize>,
    pending_save: bool,
}

const NOTICE_SECONDS: u64 = 4;

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
        // A real terminal font for SSH sessions and addresses; egui's own is the fallback.
        for (name, file) in [
            ("cascadia", "CascadiaMono.ttf"),
            ("consolas", "consola.ttf"),
        ] {
            if let Ok(bytes) = std::fs::read(format!(r"C:\Windows\Fonts\{file}")) {
                fonts
                    .font_data
                    .insert(name.into(), egui::FontData::from_owned(bytes).into());
                fonts
                    .families
                    .get_mut(&egui::FontFamily::Monospace)
                    .unwrap()
                    .insert(0, name.into());
                break;
            }
        }
        cc.egui_ctx.set_fonts(fonts);
        theme::load(&theme::path()).apply(&cc.egui_ctx);
        apply_style(&cc.egui_ctx);
        let path = model::data_path();
        let loaded = model::load(&path);
        let parent = match cc.window_handle().map(|h| h.as_raw()) {
            Ok(raw_window_handle::RawWindowHandle::Win32(h)) => h.hwnd.get() as HWND,
            _ => std::ptr::null_mut(),
        };
        native_host::install_shortcuts(parent, cc.egui_ctx.clone());
        let state_path = state::path();
        let mut app = Self::with(
            cc.egui_ctx.clone(),
            path,
            loaded,
            secret::load(&secret::path()),
            parent,
            state::load(&state_path),
        );
        app.state_path = Some(state_path);
        app.reopen_library();
        app
    }

    fn with(
        ctx: egui::Context,
        path: PathBuf,
        loaded: Result<Vec<Connection>, String>,
        saved_mssql: Option<String>,
        parent: HWND,
        state: state::State,
    ) -> Self {
        let location = database::Location::Local(path.clone());
        let (connections, error) = match loaded {
            Ok(c) => (c, None),
            Err(e) => (vec![], Some(e)),
        };
        Self {
            connections,
            sessions: vec![],
            active: None,
            filter: Filter::All,
            query: String::new(),
            modal: Modal::None,
            quitting: false,
            editor_error: String::new(),
            storage_ok: error.is_none(),
            error,
            path,
            parent,
            search_focus: false,
            ctx,
            location,
            revision: 0,
            pending: None,
            pending_location: None,
            recovery: None,
            pending_editor: None,
            db_kind: LibraryKind::Personal,
            db_path: String::new(),
            db_remember: saved_mssql.is_some(),
            db_mssql: saved_mssql.unwrap_or_else(|| MSSQL_EXAMPLE.into()),
            db_reveal: false,
            notice: None,
            group: None,
            selected: None,
            scroll_to: false,
            list_view: (0., 0.),
            state,
            state_path: None,
            shown: None,
            pending_save: false,
        }
    }

    fn notify(&mut self, text: impl Into<String>) {
        self.notice = Some((text.into(), Instant::now()));
    }

    fn sidebar_collapsed(&self, ctx: &egui::Context) -> bool {
        self.state
            .sidebar_collapsed
            .unwrap_or(ctx.content_rect().width() < 1000.)
    }

    fn set_sidebar_collapsed(&mut self, collapsed: bool) {
        self.state.sidebar_collapsed = Some(collapsed);
        self.save_state();
    }

    /// Saves workspace state. It is a convenience, so failures are not reported.
    fn save_state(&self) {
        if let Some(path) = &self.state_path {
            let _ = self.state.save(path);
        }
    }

    /// Reopens the library used last time, if it was a shared one.
    fn reopen_library(&mut self) {
        let location = match &self.state.library {
            state::Library::Personal => return,
            state::Library::Sqlite(path) => {
                self.db_kind = LibraryKind::Sqlite;
                self.db_path = path.display().to_string();
                database::Location::Sqlite(path.clone())
            }
            // Only a remembered connection string can reopen an MSSQL library.
            state::Library::Mssql if self.db_remember => {
                self.db_kind = LibraryKind::Mssql;
                database::Location::Mssql(self.db_mssql.clone())
            }
            state::Library::Mssql => return,
        };
        self.pending_location = Some(location.clone());
        self.pending = Some(database::start(location, 0, None, self.ctx.clone()));
    }

    /// Moves session `from` to position `to`, keeping the same tab selected.
    fn move_session(&mut self, from: usize, to: usize) {
        if from >= self.sessions.len() || to >= self.sessions.len() || from == to {
            return;
        }
        let session = self.sessions.remove(from);
        self.sessions.insert(to, session);
        self.active = self.active.map(|i| moved(i, from, to));
        self.shown = self.active;
    }

    /// Selects tab `index`, where 0 is the connection library and 1.. are sessions.
    fn select_tab(&mut self, index: usize) {
        self.active = index.checked_sub(1).filter(|i| *i < self.sessions.len());
    }

    /// Whether a dialog covers the workspace. Native RDP windows sit above GPU content, so
    /// they are hidden while this is true.
    fn covered(&self) -> bool {
        self.quitting || !matches!(self.modal, Modal::None)
    }

    fn persist(&mut self, next: Vec<Connection>) -> bool {
        if !self.storage_ok {
            self.error = Some(
                "Repair the saved connections file and restart Relay before making changes. \
                 Your existing file has been preserved."
                    .into(),
            );
            return false;
        }
        if self.pending.is_some() {
            return false;
        }
        self.recovery = Some(next.clone());
        self.pending_editor = match &self.modal {
            Modal::Editor(c) => Some(c.clone()),
            _ => None,
        };
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
        let id = connection.id;
        match Session::start(connection, self.ctx.clone()) {
            Ok(session) => {
                for s in &mut self.sessions {
                    s.place(None);
                }
                self.sessions.push(session);
                self.active = Some(self.sessions.len() - 1);
                if id != 0 {
                    self.state.note_recent(&self.location, id);
                    self.save_state();
                }
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// Reconnects a session in its tab with the connection's current saved settings.
    fn reconnect(&mut self, index: usize) {
        let session = &self.sessions[index];
        let connection = self
            .connections
            .iter()
            .find(|c| c.id == session.connection.id)
            .unwrap_or(&session.connection)
            .clone();
        if let Err(e) = self.sessions[index].reconnect(connection) {
            self.error = Some(e);
        }
    }

    fn new_connection(&mut self) {
        if self.pending.is_some() {
            self.notify("Wait for the current library operation to finish.");
            return;
        }
        self.edit(Connection::new());
    }

    fn edit(&mut self, connection: Connection) {
        self.modal = Modal::Editor(connection);
        self.editor_error.clear();
    }

    fn editor(&mut self, ctx: &egui::Context) {
        let colors = theme::palette(ctx);
        let Modal::Editor(c) = &self.modal else {
            return;
        };
        let mut c = c.clone();
        let mut save = false;
        let mut cancel = false;
        let rdp = c.protocol == Protocol::Rdp;
        let groups: std::collections::BTreeSet<_> =
            self.connections.iter().map(|c| c.group.clone()).collect();
        let fields_height = (ctx.content_rect().height() - 300.).max(200.);
        dialog("edit-connection", ctx).show(ctx, |ui| {
            ui.set_width(440.);
            ui.add_space(8.);
            let title = if c.id == 0 {
                "New connection"
            } else {
                "Edit connection"
            };
            ui.label(RichText::new(title).size(25.).strong());
            ui.label(RichText::new("A familiar name. A machine to reach.").color(colors.muted));
            ui.add_space(15.);
            egui::ScrollArea::vertical()
                .id_salt("editor-fields")
                .max_height(fields_height)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let half = (ui.available_width() - ui.spacing().item_spacing.x) / 2.;
                        for protocol in [Protocol::Rdp, Protocol::Ssh] {
                            let button = egui::Button::new(protocol.label())
                                .selected(c.protocol == protocol);
                            if ui.add_sized([half, 40.], button).clicked() {
                                if c.port == c.protocol.port() {
                                    c.port = protocol.port();
                                }
                                c.protocol = protocol;
                            }
                        }
                    });
                    ui.add_space(10.);
                    field(
                        ui,
                        "Connection name",
                        &mut c.name,
                        "e.g. Office workstation",
                    );
                    ui.horizontal_top(|ui| {
                        let port_width = 96.;
                        let host_width =
                            ui.available_width() - port_width - ui.spacing().item_spacing.x;
                        labeled(ui, "Host / IP address", |ui| {
                            ui.add_sized(
                                [host_width, CONTROL_HEIGHT],
                                input(&mut c.host, "e.g. 192.168.1.20"),
                            );
                        });
                        labeled(ui, "Port", |ui| {
                            // Digits only; an empty field reads as 0, which validation rejects.
                            let mut port = if c.port == 0 {
                                String::new()
                            } else {
                                c.port.to_string()
                            };
                            let response =
                                ui.add_sized([port_width, CONTROL_HEIGHT], input(&mut port, ""));
                            if response.changed() {
                                let digits: String =
                                    port.chars().filter(char::is_ascii_digit).collect();
                                c.port = digits.parse().unwrap_or(if digits.is_empty() {
                                    0
                                } else {
                                    c.port
                                });
                            }
                        });
                    });
                    let user_hint = if rdp { "DOMAIN\\username" } else { "username" };
                    field(ui, "Username (optional)", &mut c.username, user_hint);
                    labeled(ui, "Group", |ui| {
                        ui.horizontal(|ui| {
                            let button = CONTROL_HEIGHT;
                            ui.add_sized(
                                [
                                    ui.available_width() - button - ui.spacing().item_spacing.x,
                                    CONTROL_HEIGHT,
                                ],
                                input(&mut c.group, "Personal"),
                            );
                            let choose = ui
                                .add_enabled(
                                    !groups.is_empty(),
                                    egui::Button::new(RichText::new("▼").size(10.))
                                        .min_size(Vec2::splat(button)),
                                )
                                .on_hover_text("Choose an existing group");
                            egui::Popup::menu(&choose).show(|ui| {
                                for group in &groups {
                                    if ui.button(group).clicked() {
                                        c.group = group.clone();
                                    }
                                }
                            });
                        });
                    });
                    ui.checkbox(&mut c.favorite, "Add to favorites");
                    if c.protocol == Protocol::Rdp {
                        ui.add_space(4.);
                        rdp_options(ui, &mut c.rdp);
                    }
                });
            ui.add_space(8.);
            let help = if rdp {
                "Windows asks for credentials; tick “Remember me” to skip the prompt next time.\n\
                 Windows keeps them in Credential Manager. Relay never sees passwords."
            } else {
                "Uses Windows OpenSSH, including your SSH config and agent.\n\
                 Host verification and password prompts appear in the terminal."
            };
            ui.label(RichText::new(help).size(12.).color(colors.muted));
            if !self.editor_error.is_empty() {
                ui.colored_label(ui.visuals().error_fg_color, &self.editor_error);
            }
            ui.add_space(14.);
            ui.horizontal(|ui| {
                save = accent_button(ui, "Save connection", 175.).clicked();
                cancel = ui.button("Cancel").clicked();
            });
            ui.label(
                RichText::new("Enter saves · Esc cancels")
                    .size(11.)
                    .color(colors.muted),
            );
        });
        let (enter, escape) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if cancel || escape {
            self.modal = Modal::None;
            return;
        }
        save |= enter;
        if save && let Err(e) = c.validate() {
            self.editor_error = e;
            save = false;
        }
        self.modal = Modal::Editor(c.clone());
        if save {
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
                self.modal = Modal::None;
            }
        }
    }

    /// Shows the current notice in a corner without moving the page, until it expires.
    fn toast(&mut self, ctx: &egui::Context) {
        let Some((text, shown)) = self.notice.clone() else {
            return;
        };
        let remaining = Duration::from_secs(NOTICE_SECONDS).saturating_sub(shown.elapsed());
        if remaining.is_zero() {
            self.notice = None;
            return;
        }
        ctx.request_repaint_after(remaining);
        let colors = theme::palette(ctx);
        let mut dismiss = false;
        egui::Area::new(egui::Id::new("notice"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::RIGHT_BOTTOM, Vec2::new(-24., -24.))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(colors.surface)
                    .stroke(Stroke::new(1_f32, colors.line))
                    .corner_radius(8)
                    .inner_margin(egui::Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.set_max_width(420.);
                        ui.horizontal(|ui| {
                            ui.colored_label(colors.accent, "●");
                            ui.label(text);
                            dismiss = ui.small_button("×").clicked();
                        });
                    });
            });
        if dismiss {
            self.notice = None;
        }
    }

    fn confirmations(&mut self, ctx: &egui::Context) {
        let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if self.quitting && escape {
            self.quitting = false;
        }
        if self.quitting {
            dialog("quit", ctx).show(ctx, |ui| {
                ui.set_width(380.);
                ui.heading("Close Relay?");
                ui.label("Save your work on each remote machine before closing.");
                if self.recovery.is_some() {
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        "There are unsaved library changes in memory. \
                         Export them before closing or they will be lost.",
                    );
                }
                ui.horizontal(|ui| {
                    let quit = egui::Button::new("End sessions and quit");
                    if ui.add_enabled(self.pending.is_none(), quit).clicked() {
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
        if escape && matches!(self.modal, Modal::Delete(_) | Modal::Close(_)) {
            self.modal = Modal::None;
        }
        match self.modal {
            Modal::Delete(id) => {
                let name = self
                    .connections
                    .iter()
                    .find(|c| c.id == id)
                    .map_or("connection".into(), |c| format!("“{}”", c.name));
                dialog("delete", ctx).show(ctx, |ui| {
                    ui.set_width(360.);
                    ui.heading(format!("Delete {name}?"));
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
                                self.modal = Modal::None;
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            self.modal = Modal::None;
                        }
                    });
                });
            }
            Modal::Close(index) => {
                let name = self
                    .sessions
                    .get(index)
                    .map_or("session".into(), |s| format!("“{}”", s.connection.name));
                dialog("close", ctx).show(ctx, |ui| {
                    ui.set_width(380.);
                    ui.heading(format!("Close {name}?"));
                    ui.label(
                        "The connection will end. Save your work on the remote machine first.",
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Close session").clicked() {
                            self.sessions.remove(index);
                            self.active = match self.active {
                                Some(a) if a == index => None,
                                Some(a) if a > index => Some(a - 1),
                                other => other,
                            };
                            self.modal = Modal::None;
                        }
                        if ui.button("Keep open").clicked() {
                            self.modal = Modal::None;
                        }
                    });
                });
            }
            _ => {}
        }
    }
}

impl eframe::App for App {
    fn on_exit(&mut self) {
        self.save_state();
    }

    #[cfg(feature = "ui-check")]
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        ui_check::hover(raw_input);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let colors = theme::palette(ctx);
        self.poll_storage();
        // Remember the restored window size; a maximized window keeps the size it returns to.
        let (inner, maximized) = ctx.input(|i| (i.viewport().inner_rect, i.viewport().maximized));
        if let Some(maximized) = maximized {
            self.state.maximized = maximized;
            if !maximized && let Some(inner) = inner {
                self.state.window = Some([inner.width().round(), inner.height().round()]);
            }
        }
        #[cfg(feature = "ui-check")]
        ui_check::capture(ctx);
        if ctx.input(|i| i.viewport().close_requested()) && self.pending.is_some() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.notify("Wait for the library operation to finish before closing Relay.");
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
        let shortcut = |modifiers, key| ctx.input_mut(|i| i.consume_key(modifiers, key));
        let ctrl = egui::Modifiers::CTRL;
        // Remote desktops take the keyboard, so their shortcuts arrive through a native hook.
        let native = native_host::take_command();
        if !self.covered() {
            use native_host::Command;
            // Tab switching also works from a focused terminal, which has no use for these keys.
            let ctrl_alt = ctrl | egui::Modifiers::ALT;
            let command = native.or_else(|| {
                if shortcut(ctrl_alt, egui::Key::PageUp)
                    || shortcut(ctrl | egui::Modifiers::SHIFT, egui::Key::Tab)
                {
                    Some(Command::PreviousTab)
                } else if shortcut(ctrl_alt, egui::Key::PageDown) || shortcut(ctrl, egui::Key::Tab)
                {
                    Some(Command::NextTab)
                } else {
                    None
                }
            });
            let tabs = self.sessions.len() + 1;
            let current = self.active.map_or(0, |i| i + 1);
            match command {
                Some(Command::PreviousTab) => self.select_tab((current + tabs - 1) % tabs),
                Some(Command::NextTab) => self.select_tab((current + 1) % tabs),
                Some(Command::Release) | None => {}
            }
            if native.is_some() {
                // Take the keyboard back from the remote desktop.
                unsafe {
                    windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus(self.parent);
                }
            }
        }
        if !terminal_focused && !self.covered() {
            if shortcut(ctrl, egui::Key::N) {
                self.new_connection();
            }
            if shortcut(ctrl, egui::Key::K) {
                self.search_focus = true;
                self.active = None;
            }
            if shortcut(ctrl, egui::Key::B) {
                self.set_sidebar_collapsed(!self.sidebar_collapsed(ctx));
            }
            if shortcut(ctrl, egui::Key::W)
                && let Some(active) = self.active
            {
                self.modal = Modal::Close(active);
            }
            let shift = ctrl | egui::Modifiers::SHIFT;
            if let Some(active) = self.active {
                if shortcut(shift, egui::Key::PageUp) && active > 0 {
                    self.move_session(active, active - 1);
                } else if shortcut(shift, egui::Key::PageDown) {
                    self.move_session(active, active + 1);
                }
            }
            let numbers = [
                egui::Key::Num1,
                egui::Key::Num2,
                egui::Key::Num3,
                egui::Key::Num4,
                egui::Key::Num5,
                egui::Key::Num6,
                egui::Key::Num7,
                egui::Key::Num8,
            ];
            for (index, key) in numbers.into_iter().enumerate() {
                if shortcut(ctrl, key) {
                    self.select_tab(index);
                }
            }
            if shortcut(ctrl, egui::Key::Num9) {
                self.select_tab(self.sessions.len());
            }
        }
        for s in &mut self.sessions {
            s.poll(self.parent);
        }
        if self.shown != self.active {
            self.shown = self.active;
            if let Some(terminal) = self
                .active
                .and_then(|i| self.sessions.get_mut(i))
                .and_then(|s| s.terminal.as_mut())
            {
                terminal.focus();
            }
        }
        // Hide native children before painting overlays; native child HWNDs sit above GPU content.
        let covered = self.covered();
        for (i, s) in self.sessions.iter_mut().enumerate() {
            if self.active != Some(i) || covered {
                s.place(None);
            }
        }
        self.sidebar(ctx);
        self.tabs(ctx);
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
                    let mut full_screen = false;
                    let mut save = false;
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
                        let desktop = s.terminal.is_none() && !s.window.is_null();
                        // Quick connections are not in the library until saved.
                        let unsaved = s.connection.id == 0
                            && !self.connections.iter().any(|c| c.same_target(&s.connection));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if unsaved {
                                save = ui
                                    .button("Save to library")
                                    .on_hover_text("Keep this connection in the library")
                                    .clicked();
                            }
                            if desktop && s.is_running() && s.error.is_none() && !s.full_screen() {
                                full_screen = ui
                                    .button("Full screen")
                                    .on_hover_text("Ctrl+Alt+Break leaves and re-enters full screen")
                                    .clicked();
                                ui.label(
                                    RichText::new(
                                        "Ctrl+Alt+PgUp/PgDn switch tabs · Ctrl+Alt+Home returns to Relay",
                                    )
                                    .size(11.)
                                    .color(colors.muted),
                                );
                            }
                        });
                    });
                    if full_screen
                        && let Err(e) = self.sessions[index].enter_full_screen()
                    {
                        self.error = Some(e);
                    }
                    if save {
                        self.edit(self.sessions[index].connection.clone());
                    }
                    let s = &self.sessions[index];
                    ui.add_space(10.);
                    if s.terminal.is_some() {
                        if !s.is_running() && ui.button("Reconnect").clicked() {
                            self.reconnect(index);
                        }
                        self.sessions[index].terminal.as_mut().unwrap().show(ui);
                    } else if let Some(error) = &s.error {
                        let error = error.clone();
                        ui.add_space(40.);
                        ui.label(RichText::new(error).size(18.));
                        ui.add_space(12.);
                        if ui.button("Reconnect").clicked() {
                            self.reconnect(index);
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
                        if let Some(status) = s.status() {
                            ui.colored_label(ui.visuals().warn_fg_color, status);
                        }
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
        self.toast(ctx);
        let covered = self.covered();
        let ppp = ctx.pixels_per_point();
        let pixels = |r: egui::Rect| {
            [
                (r.min.x * ppp).round() as i32,
                (r.min.y * ppp).round() as i32,
                (r.width() * ppp).round() as i32,
                (r.height() * ppp).round() as i32,
            ]
        };
        // Toasts, tooltips and menus float over the remote desktop; cut them out of it.
        let holes: Vec<[i32; 4]> = session_rect
            .map(|session| {
                overlays(ctx)
                    .into_iter()
                    .map(|r| r.intersect(session))
                    .filter(|r| r.is_positive())
                    .map(|r| {
                        let [x, y, w, h] = pixels(r);
                        let [sx, sy, ..] = pixels(session);
                        [x - sx, y - sy, w, h]
                    })
                    .collect()
            })
            .unwrap_or_default();
        for (i, s) in self.sessions.iter_mut().enumerate() {
            let rect = if !covered && self.active == Some(i) {
                session_rect.map(pixels)
            } else {
                None
            };
            s.place_around(rect, &holes);
        }
        if !self.sessions.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
    }
}

fn accent_button(ui: &mut egui::Ui, text: &str, width: f32) -> egui::Response {
    let colors = theme::palette(ui.ctx());
    ui.add_sized(
        [width, CONTROL_HEIGHT],
        egui::Button::new(RichText::new(text).color(colors.bg).strong())
            .fill(colors.accent)
            .corner_radius(6),
    )
}
/// Screen areas of visible floating layers: toasts, tooltips, menus and popups.
fn overlays(ctx: &egui::Context) -> Vec<egui::Rect> {
    ctx.memory(|memory| {
        let visible = memory.areas().visible_layer_ids();
        memory
            .layer_ids()
            .filter(|layer| {
                visible.contains(layer)
                    && matches!(layer.order, egui::Order::Foreground | egui::Order::Tooltip)
            })
            .filter_map(|layer| memory.area_rect(layer.id))
            .collect()
    })
}

/// Where item `index` ends up after the item at `from` moves to `to`.
fn moved(index: usize, from: usize, to: usize) -> usize {
    if index == from {
        to
    } else if from < index && index <= to {
        index - 1
    } else if to <= index && index < from {
        index + 1
    } else {
        index
    }
}

/// Spacing and text styles shared by every screen.
fn apply_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.spacing.window_margin = egui::Margin::same(24);
    style.spacing.item_spacing = Vec2::new(10., 10.);
    style.spacing.button_padding = Vec2::new(12., 8.);
    // Labels are not selectable text: hovering them should not show a text cursor.
    style.interaction.selectable_labels = false;
    style
        .text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(14.));
    style
        .text_styles
        .insert(egui::TextStyle::Button, egui::FontId::proportional(14.));
    ctx.set_style(style);
}

/// A dialog width that leaves a margin in narrow windows.
fn dialog_width(ctx: &egui::Context, preferred: f32) -> f32 {
    preferred.min(ctx.content_rect().width() - 96.)
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
fn rdp_options(ui: &mut egui::Ui, options: &mut model::RdpOptions) {
    use model::{Audio, Display, Keys};
    egui::CollapsingHeader::new("Remote desktop options")
        .id_salt("rdp-options")
        .default_open(*options != model::RdpOptions::default())
        .show(ui, |ui| {
            field(
                ui,
                "RD Gateway (optional)",
                &mut options.gateway,
                "gateway.example.com",
            );
            ui.label("Use on the remote computer");
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut options.clipboard, "Clipboard");
                ui.checkbox(&mut options.drives, "Local drives");
                ui.checkbox(&mut options.printers, "Printers");
                ui.checkbox(&mut options.smart_cards, "Smart cards");
                ui.checkbox(&mut options.microphone, "Microphone");
            });
            egui::Grid::new("rdp-choices")
                .num_columns(2)
                .spacing([12., 8.])
                .show(ui, |ui| {
                    ui.label("Remote audio");
                    egui::ComboBox::from_id_salt("rdp-audio")
                        .width(220.)
                        .selected_text(options.audio.label())
                        .show_ui(ui, |ui| {
                            for audio in Audio::ALL {
                                ui.selectable_value(&mut options.audio, audio, audio.label());
                            }
                        });
                    ui.end_row();
                    ui.label("Windows key combinations");
                    egui::ComboBox::from_id_salt("rdp-keys")
                        .width(220.)
                        .selected_text(options.keys.label())
                        .show_ui(ui, |ui| {
                            for keys in Keys::ALL {
                                ui.selectable_value(&mut options.keys, keys, keys.label());
                            }
                        });
                    ui.end_row();
                    ui.label("Resolution");
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut options.display, Display::Fit, "Fit to tab");
                        let fixed = matches!(options.display, Display::Fixed { .. });
                        if ui.radio(fixed, "Fixed").clicked() && !fixed {
                            options.display = Display::Fixed {
                                width: 1920,
                                height: 1080,
                            };
                        }
                        if let Display::Fixed { width, height } = &mut options.display {
                            ui.add(egui::DragValue::new(width).range(200..=8192));
                            ui.label("×");
                            ui.add(egui::DragValue::new(height).range(200..=8192));
                        }
                    });
                    ui.end_row();
                });
        });
}
/// The height of text fields and buttons, so controls in a row line up.
const CONTROL_HEIGHT: f32 = 36.;

/// A single-line text field with room around its text, which is vertically centered.
fn input<'a>(value: &'a mut String, hint: &str) -> egui::TextEdit<'a> {
    egui::TextEdit::singleline(value)
        .hint_text(hint)
        .margin(egui::Margin::symmetric(11, 8))
        .vertical_align(egui::Align::Center)
}

/// A label with a control close below it; groups are spaced further apart.
fn labeled<R>(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 5.;
        ui.label(label);
        add(ui)
    })
    .inner
}

fn field(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str) {
    labeled(ui, label, |ui| {
        ui.add_sized([ui.available_width(), CONTROL_HEIGHT], input(value, hint));
    });
}
