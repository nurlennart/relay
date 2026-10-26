#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod csv_io;
mod database;
mod file_dialog;
mod import;
mod model;
mod platform;
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
use session::Session;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

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
                // Matches the Linux desktop entry, so docks show Relay's icon and name.
                .with_app_id("relay")
                .with_inner_size(saved.window.unwrap_or([1280., 820.]))
                .with_maximized(saved.maximized)
                .with_min_inner_size([720., 560.])
                .with_icon(app_icon()),
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

/// Integrated authentication needs Windows; elsewhere the example uses an SQL login.
const MSSQL_EXAMPLE: &str = if cfg!(windows) {
    "Server=tcp:sql.example.com,1433;Database=Relay;\
     Integrated Security=true;Encrypt=true;TrustServerCertificate=false"
} else {
    "Server=tcp:sql.example.com,1433;Database=Relay;\
     User ID=relay;Password=;Encrypt=true;TrustServerCertificate=false"
};

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
    /// Relay's main window, which pickers and embedded remote desktops attach to.
    parent: Option<platform::Window>,
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
    /// Keep the MSSQL connection string in the credential store after it opens successfully.
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
    /// Sessions shown in their own windows, by session key.
    detached: Vec<u64>,
    /// Native windows found for detached sessions, which remote desktops are moved into.
    windows: Vec<(u64, platform::Window)>,
    /// The session shown beside the active one in split view.
    split: Option<usize>,
    /// Where the split divider sits, as a fraction of the width.
    split_fraction: f32,
    /// The open snippet palette, over the active SSH tab.
    palette: Option<Palette>,
    /// A snippet being added (no index) or edited in Settings.
    snippet_draft: Option<(Option<usize>, state::Snippet)>,
    pending_save: bool,
    /// A connection to open at the start of the next frame, outside any dialog or list.
    connect_request: Option<Connection>,
}

const NOTICE_SECONDS: u64 = 4;

#[derive(Default)]
struct Palette {
    query: String,
    /// The highlighted row among the matching snippets.
    selected: usize,
    focused: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_theme(egui::Theme::Dark);
        let mut fonts = egui::FontDefinitions::default();
        // Prefer the system's own fonts; a real terminal font matters for SSH sessions.
        for (family, monospace) in [
            (egui::FontFamily::Proportional, false),
            (egui::FontFamily::Monospace, true),
        ] {
            if let Some((path, bytes)) = platform::fonts(monospace)
                .iter()
                .find_map(|path| Some((path, std::fs::read(path).ok()?)))
            {
                let name = path.to_string();
                fonts
                    .font_data
                    .insert(name.clone(), egui::FontData::from_owned(bytes).into());
                fonts.families.entry(family).or_default().insert(0, name);
            }
        }
        cc.egui_ctx.set_fonts(fonts);
        theme::load(&theme::path()).apply(&cc.egui_ctx);
        apply_style(&cc.egui_ctx);
        let path = model::data_path();
        let loaded = model::load(&path);
        let parent = platform::Window::of(cc);
        if let Some(parent) = parent {
            platform::install_shortcuts(parent, cc.egui_ctx.clone());
        }
        let state_path = state::path();
        let mut app = Self::with(
            cc.egui_ctx.clone(),
            path,
            loaded,
            secret::load(),
            parent,
            state::load(&state_path),
        );
        app.state_path = Some(state_path);
        app.reopen_library();
        app.restore_tabs();
        app
    }

    fn with(
        ctx: egui::Context,
        path: PathBuf,
        loaded: Result<Vec<Connection>, String>,
        saved_mssql: Option<String>,
        parent: Option<platform::Window>,
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
            detached: vec![],
            windows: vec![],
            split: None,
            split_fraction: 0.5,
            palette: None,
            snippet_draft: None,
            pending_save: false,
            connect_request: None,
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

    /// Reopens last run's tabs. Each connects when first shown, starting with the selected one.
    fn restore_tabs(&mut self) {
        for connection in self.state.open.clone() {
            self.sessions
                .push(Session::restored(connection, self.ctx.clone()));
        }
        self.active = self.state.active_tab.filter(|i| *i < self.sessions.len());
    }

    /// Records the open tabs so the next start can restore them.
    fn remember_tabs(&mut self) {
        self.state.open = self.sessions.iter().map(|s| s.connection.clone()).collect();
        self.state.active_tab = self.active;
        self.save_state();
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
        self.split = self.split.map(|i| moved(i, from, to));
        self.shown = self.active;
        self.remember_tabs();
    }

    fn is_detached(&self, index: usize) -> bool {
        self.sessions
            .get(index)
            .is_some_and(|s| self.detached.contains(&s.key))
    }

    /// Moves session `index` into its own window.
    fn detach(&mut self, index: usize) {
        let Some(session) = self.sessions.get_mut(index) else {
            return;
        };
        if let Some(terminal) = &mut session.terminal {
            terminal.focus();
        }
        self.detached.push(session.key);
        if self.split == Some(index) {
            self.split = None;
        }
        if self.active == Some(index) {
            self.active = self.split.take();
        }
    }

    /// Returns a detached session to the tabs and selects it.
    fn attach(&mut self, key: u64) {
        self.detached.retain(|k| *k != key);
        self.windows.retain(|(k, _)| *k != key);
        if let Some(index) = self.sessions.iter().position(|s| s.key == key) {
            self.sessions[index].set_host(None, self.parent);
            self.active = Some(index);
        }
    }

    /// Stands in for a session that lives in its own window.
    fn detached_placeholder(&mut self, ui: &mut egui::Ui, index: usize) {
        let colors = theme::palette(ui.ctx());
        let key = self.sessions[index].key;
        ui.add_space(60.);
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new(format!(
                    "{} is open in its own window.",
                    self.sessions[index].connection.name
                ))
                .size(18.),
            );
            ui.add_space(12.);
            ui.horizontal(|ui| {
                ui.add_space((ui.available_width() - 260.).max(0.) / 2.);
                if accent_button(ui, "Show window", 130.).clicked() {
                    ui.ctx()
                        .send_viewport_cmd_to(session_viewport(key), egui::ViewportCommand::Focus);
                }
                if ui
                    .add_sized([120., CONTROL_HEIGHT], egui::Button::new("Back to tabs"))
                    .clicked()
                {
                    self.attach(key);
                }
            });
            ui.label(RichText::new("Closing that window also brings it back.").color(colors.muted));
        });
    }

    /// Shows each detached session in its own window. Returns, for remote desktops, their
    /// physical rectangle in that window and the overlay holes to cut out of it.
    #[allow(clippy::type_complexity)]
    fn detached_windows(&mut self, ctx: &egui::Context) -> Vec<(usize, [i32; 4], Vec<[i32; 4]>)> {
        let mut placed = vec![];
        for key in self.detached.clone() {
            let Some(index) = self.sessions.iter().position(|s| s.key == key) else {
                // The session was closed; its window closes with it.
                self.detached.retain(|k| *k != key);
                self.windows.retain(|(k, _)| *k != key);
                continue;
            };
            let title = format!("{} · Relay", self.sessions[index].connection.name);
            let builder = egui::ViewportBuilder::default()
                .with_title(&title)
                .with_inner_size([1100., 760.])
                .with_min_inner_size([520., 380.])
                .with_icon(app_icon());
            let colors = theme::palette(ctx);
            let mut close = false;
            let shown = ctx.show_viewport_immediate(session_viewport(key), builder, |ctx, _| {
                close = ctx.input(|i| i.viewport().close_requested());
                let mut rect = None;
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().fill(colors.bg).inner_margin(16.))
                    .show(ctx, |ui| {
                        if self.is_detached(index) {
                            rect = self.session_pane(ui, index, Pane::Window);
                        }
                    });
                rect.map(|rect| (rect, ctx.pixels_per_point(), overlays(ctx)))
            });
            if close {
                self.attach(key);
                continue;
            }
            // An embedded remote desktop moves into the window once it exists.
            let window = match self.windows.iter().find(|(k, _)| *k == key) {
                Some((_, window)) => Some(*window),
                None => {
                    let mut known: Vec<_> = self.windows.iter().map(|(_, w)| *w).collect();
                    known.extend(self.parent);
                    let found = platform::find_window(&title, &known);
                    if let Some(window) = found {
                        self.windows.push((key, window));
                        self.sessions[index].set_host(Some(window), self.parent);
                    }
                    found
                }
            };
            if let (Some(_), Some((rect, ppp, floating))) = (window, shown) {
                let pixels = |r: egui::Rect| {
                    [
                        (r.min.x * ppp).round() as i32,
                        (r.min.y * ppp).round() as i32,
                        (r.width() * ppp).round() as i32,
                        (r.height() * ppp).round() as i32,
                    ]
                };
                let [sx, sy, ..] = pixels(rect);
                let holes = floating
                    .iter()
                    .map(|r| r.intersect(rect))
                    .filter(|r| r.is_positive())
                    .map(|r| {
                        let [x, y, w, h] = pixels(r);
                        [x - sx, y - sy, w, h]
                    })
                    .collect();
                placed.push((index, pixels(rect), holes));
            }
        }
        placed
    }

    /// Shows `index` beside the active session, or closes split view if it is already there.
    fn split_with(&mut self, index: usize) {
        if self.split == Some(index) {
            self.split = None;
        } else if self.active.is_some_and(|a| a != index) && index < self.sessions.len() {
            self.split = Some(index);
        }
    }

    /// Opens split view with the neighbouring tab, or closes it.
    fn toggle_split(&mut self) {
        if self.split.take().is_some() {
            return;
        }
        if let Some(active) = self.active {
            let next = if active + 1 < self.sessions.len() {
                Some(active + 1)
            } else {
                active.checked_sub(1)
            };
            if let Some(next) = next {
                self.split_with(next);
            } else {
                self.notify("Open a second session to use split view.");
            }
        }
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

    /// Opens `connection` in a new tab on the next frame.
    fn request_connect(&mut self, connection: Connection) {
        self.connect_request = Some(connection);
        self.ctx.request_repaint();
    }

    fn connect(&mut self, connection: Connection) {
        let id = connection.id;
        match Session::start(connection, self.ctx.clone()) {
            Ok(session) => {
                for s in &mut self.sessions {
                    s.place(None);
                }
                self.sessions.push(session);
                self.active = Some(self.sessions.len() - 1);
                self.remember_tabs();
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
        let parent = self.parent;
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
                    ui.add_space(4.);
                    if c.protocol == Protocol::Rdp {
                        rdp_options(ui, &mut c.rdp);
                    } else {
                        ssh_options(ui, &mut c.ssh, parent);
                    }
                });
            ui.add_space(8.);
            let help = match (rdp, rdp::EMBEDDED) {
                (true, true) => {
                    "Windows asks for credentials; tick “Remember me” to skip the prompt next \
                     time.\nWindows keeps them in Credential Manager. Relay never sees passwords."
                }
                (true, false) => {
                    "Opens in a FreeRDP window. Certificate and password prompts appear in the \
                     tab.\nRelay never stores passwords."
                }
                (false, _) => {
                    "Uses your OpenSSH client, including your SSH config and agent.\n\
                     Host verification and password prompts appear in the terminal."
                }
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

    /// Shows session `index` in `ui`: its header, then its terminal, status, or remote desktop.
    /// Returns the area a remote desktop should cover.
    fn session_pane(&mut self, ui: &mut egui::Ui, index: usize, pane: Pane) -> Option<egui::Rect> {
        let colors = theme::palette(ui.ctx());
        let mut full_screen = false;
        let mut unsplit = false;
        let mut attach = false;
        let mut save = false;
        let mut snippets = false;
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
            let desktop = s.has_desktop();
            // Quick connections are not in the library until saved.
            let unsaved = s.connection.id == 0
                && !self
                    .connections
                    .iter()
                    .any(|c| c.same_target(&s.connection));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                match pane {
                    Pane::Beside => {
                        unsplit = ui
                            .button("Close split")
                            .on_hover_text("Ctrl+\\ toggles split view")
                            .clicked();
                    }
                    Pane::Window => {
                        attach = ui
                            .button("Back to tabs")
                            .on_hover_text("Closing this window does the same")
                            .clicked();
                    }
                    Pane::Main => {}
                }
                if unsaved {
                    save = ui
                        .button("Save to library")
                        .on_hover_text("Keep this connection in the library")
                        .clicked();
                }
                // The snippet palette opens over the main window's active tab.
                let ssh = s.connection.protocol == Protocol::Ssh;
                if ssh && s.terminal.is_some() && s.is_running() && pane != Pane::Window {
                    snippets = ui
                        .button("Snippets")
                        .on_hover_text("Saved commands · Ctrl+Shift+P")
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
        if full_screen && let Err(e) = self.sessions[index].enter_full_screen() {
            self.error = Some(e);
        }
        if save {
            self.edit(self.sessions[index].connection.clone());
        }
        if snippets {
            self.palette = Some(Palette::default());
        }
        if unsplit {
            self.split = None;
        }
        if attach {
            self.attach(self.sessions[index].key);
        }
        let s = &self.sessions[index];
        ui.add_space(10.);
        if s.terminal.is_some() {
            if let Some(status) = s.status() {
                let (mut now, mut stop) = (false, false);
                ui.horizontal(|ui| {
                    ui.colored_label(ui.visuals().warn_fg_color, status);
                    now = ui.button("Reconnect now").clicked();
                    stop = ui.button("Stop").clicked();
                });
                if now {
                    self.reconnect(index);
                } else if stop {
                    self.sessions[index].stop_retrying();
                }
            } else if !s.is_running() && ui.button("Reconnect").clicked() {
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
        } else if !s.has_desktop() {
            ui.add_space(50.);
            ui.spinner();
            ui.heading("Opening your session…");
            ui.label(
                RichText::new("Complete any Windows authentication prompt to continue.")
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
            return Some(ui.available_rect_before_wrap());
        }
        None
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
                            let shift = |tab: Option<usize>| match tab {
                                Some(a) if a == index => None,
                                Some(a) if a > index => Some(a - 1),
                                other => other,
                            };
                            self.active = shift(self.active);
                            self.split = shift(self.split);
                            self.modal = Modal::None;
                            self.remember_tabs();
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
        // Quitting ends sessions first; keep the tabs recorded before that.
        if !self.sessions.is_empty() {
            self.state.active_tab = self.active;
        }
        self.save_state();
    }

    #[cfg(feature = "ui-check")]
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        ui_check::hover(raw_input);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let colors = theme::palette(ctx);
        self.poll_storage();
        if let Some(connection) = self.connect_request.take() {
            self.connect(connection);
        }
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
        // Either side of a split view can hold the focused terminal.
        let terminal_focused = [self.active, self.split]
            .into_iter()
            .flatten()
            .filter_map(|i| self.sessions.get(i))
            .filter_map(|s| s.terminal.as_ref())
            .any(|t| t.focused);
        let shortcut = |modifiers, key| ctx.input_mut(|i| i.consume_key(modifiers, key));
        // Cmd on macOS, Ctrl elsewhere.
        let ctrl = egui::Modifiers::COMMAND;
        // Remote desktops take the keyboard, so their shortcuts arrive through a native hook.
        let native = platform::take_command();
        if !self.covered() {
            use platform::Command;
            if shortcut(ctrl, egui::Key::Backslash) {
                self.toggle_split();
            }
            let ssh = self
                .active
                .and_then(|i| self.sessions.get(i))
                .is_some_and(|s| s.connection.protocol == Protocol::Ssh && s.terminal.is_some());
            if ssh && shortcut(ctrl | egui::Modifiers::SHIFT, egui::Key::P) {
                self.palette = Some(Palette::default());
            }
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
            if native.is_some()
                && let Some(parent) = self.parent
            {
                // Take the keyboard back from the remote desktop.
                platform::focus(parent);
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
        if self.split == self.active {
            self.split = None;
        }
        // A tab restored from the last run connects when it is first shown.
        for index in [self.active, self.split].into_iter().flatten() {
            if let Some(session) = self.sessions.get_mut(index)
                && let Err(e) = session.resume()
            {
                self.error = Some(e);
            }
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
        // Hide native children before painting overlays; embedded remote desktops sit above GPU content.
        let covered = self.covered();
        for (i, s) in self.sessions.iter_mut().enumerate() {
            let detached = self.detached.contains(&s.key);
            if !detached && ((self.active != Some(i) && self.split != Some(i)) || covered) {
                s.place(None);
            }
        }
        self.sidebar(ctx);
        self.tabs(ctx);
        // Sessions on screen, with the area each remote desktop should cover.
        let mut session_rects: Vec<(usize, egui::Rect)> = vec![];
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
                let split = self.split.filter(|i| {
                    Some(*i) != self.active && *i < self.sessions.len() && !self.is_detached(*i)
                });
                match (self.active, split) {
                    (Some(index), _) if self.is_detached(index) => {
                        self.detached_placeholder(ui, index)
                    }
                    (Some(left), Some(right)) => {
                        let area = ui.available_rect_before_wrap();
                        let gap = 16.;
                        let x = area.left() + area.width() * self.split_fraction;
                        let divider = egui::Rect::from_min_max(
                            egui::pos2(x - gap / 2., area.top()),
                            egui::pos2(x + gap / 2., area.bottom()),
                        );
                        let handle = ui
                            .interact(divider, egui::Id::new("split-divider"), egui::Sense::drag())
                            .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
                        if handle.dragged()
                            && let Some(pointer) = handle.interact_pointer_pos()
                        {
                            self.split_fraction =
                                ((pointer.x - area.left()) / area.width()).clamp(0.25, 0.75);
                        }
                        let line = if handle.hovered() || handle.dragged() {
                            colors.accent
                        } else {
                            colors.line
                        };
                        ui.painter()
                            .vline(x, area.y_range(), Stroke::new(1_f32, line));
                        let sides = [
                            (
                                left,
                                egui::Rect::from_min_max(
                                    area.min,
                                    egui::pos2(divider.left(), area.bottom()),
                                ),
                                Pane::Main,
                            ),
                            (
                                right,
                                egui::Rect::from_min_max(
                                    egui::pos2(divider.right(), area.top()),
                                    area.max,
                                ),
                                Pane::Beside,
                            ),
                        ];
                        for (index, rect, pane) in sides {
                            let mut pane_ui = ui.new_child(
                                egui::UiBuilder::new()
                                    .max_rect(rect)
                                    .layout(egui::Layout::top_down(egui::Align::LEFT)),
                            );
                            pane_ui.set_clip_rect(rect);
                            if let Some(rect) = self.session_pane(&mut pane_ui, index, pane) {
                                session_rects.push((index, rect));
                            }
                        }
                        ui.allocate_rect(area, egui::Sense::hover());
                    }
                    (Some(index), None) => {
                        if let Some(rect) = self.session_pane(ui, index, Pane::Main) {
                            session_rects.push((index, rect));
                        }
                    }
                    (None, _) => self.workspace(ui),
                }
            });
        self.editor(ctx);
        self.confirmations(ctx);
        self.settings(ctx);
        self.library_dialogs(ctx);
        self.snippet_palette(ctx);
        self.toast(ctx);
        let windows = self.detached_windows(ctx);
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
        // Toasts, tooltips and menus float over remote desktops; cut them out of each.
        let floating = overlays(ctx);
        let holes = |session: egui::Rect| -> Vec<[i32; 4]> {
            floating
                .iter()
                .map(|r| r.intersect(session))
                .filter(|r| r.is_positive())
                .map(|r| {
                    let [x, y, w, h] = pixels(r);
                    let [sx, sy, ..] = pixels(session);
                    [x - sx, y - sy, w, h]
                })
                .collect()
        };
        for (i, s) in self.sessions.iter_mut().enumerate() {
            // Detached windows are separate from the main window's dialogs.
            if let Some((_, rect, holes)) = windows.iter().find(|(index, ..)| *index == i) {
                s.place_around(Some(*rect), holes);
                continue;
            }
            let rect = session_rects
                .iter()
                .find(|(index, _)| *index == i && !covered)
                .map(|(_, rect)| *rect);
            s.place_around(rect.map(pixels), &rect.map(holes).unwrap_or_default());
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

/// Relay's icon for its windows, rendered by assets/make_icon.py.
fn app_icon() -> egui::IconData {
    egui::IconData {
        rgba: include_bytes!("../assets/icon.rgba").to_vec(),
        width: 256,
        height: 256,
    }
}

/// Where a session pane is shown, which decides its header actions.
#[derive(Clone, Copy, PartialEq)]
enum Pane {
    Main,
    /// The right side of split view.
    Beside,
    /// A detached session window.
    Window,
}

fn session_viewport(key: u64) -> egui::ViewportId {
    egui::ViewportId::from_hash_of(("relay-session", key))
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
fn ssh_options(
    ui: &mut egui::Ui,
    options: &mut model::SshOptions,
    parent: Option<platform::Window>,
) {
    egui::CollapsingHeader::new("SSH options")
        .id_salt("ssh-options")
        .default_open(*options != model::SshOptions::default())
        .show(ui, |ui| {
            field(
                ui,
                "Jump host (optional)",
                &mut options.jump,
                "user@bastion.example.com:22",
            );
            labeled(ui, "Identity file (optional)", |ui| {
                ui.horizontal(|ui| {
                    let browse = 80.;
                    ui.add_sized(
                        [
                            ui.available_width() - browse - ui.spacing().item_spacing.x,
                            CONTROL_HEIGHT,
                        ],
                        input(&mut options.identity, "Uses your SSH config and agent"),
                    );
                    let button =
                        egui::Button::new("Browse…").min_size(Vec2::new(browse, CONTROL_HEIGHT));
                    if ui.add(button).clicked()
                        && let Some(path) =
                            file_dialog::choose(parent, file_dialog::Kind::Key).first()
                    {
                        options.identity = path.display().to_string();
                    }
                });
            });
            field(
                ui,
                "Port forwards (optional)",
                &mut options.forwards,
                "L8080:localhost:80, R9000:localhost:9000, D1080",
            );
            ui.label(
                RichText::new(
                    "L forwards a local port to the remote side, R the reverse, and D opens a \
                     SOCKS proxy.",
                )
                .size(11.)
                .color(theme::palette(ui.ctx()).muted),
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
                    ui.label("System key combinations");
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
