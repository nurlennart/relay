//! Relay's workspace: the `App` state, its frame loop, and the screens drawn from it.
mod editor;
mod files;
mod icons;
mod library;
mod quick_open;
mod sessions;
mod settings;
mod shortcuts;
mod sidebar;
mod storage;
mod tabs;
#[cfg(test)]
mod tests;
#[cfg(feature = "ui-check")]
mod ui_check;
mod widgets;

use crate::{
    csv_io, database, file_dialog, import, json_io,
    model::{self, Connection, Protocol},
    platform, rdp, secret,
    session::{self, Session},
    state, theme,
};
use eframe::egui::{self, Color32, RichText, Stroke, Vec2};
use icons::{Icon, icon_button};
use sessions::*;
use sidebar::*;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use widgets::*;

/// Opens Relay's main window and runs until it closes.
pub fn run() -> eframe::Result {
    // Rendering checks start from defaults and never touch the user's saved state.
    let check = cfg!(feature = "ui-check") && std::env::var_os("RELAY_UI_CHECK").is_some();
    let saved = if check {
        state::State::default()
    } else {
        state::load(&state::path())
    };
    eframe::run_native(
        "Relay",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                // Matches the Linux desktop entry, so docks show Relay's icon and name.
                .with_app_id("relay")
                .with_inner_size(saved.window.unwrap_or([1280., 820.]))
                .with_maximized(saved.maximized)
                .with_min_inner_size([720., 560.])
                .with_icon(app_icon()),
            wgpu_options: gpu_options(),
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
    /// Several connections chosen in the library, to delete together.
    DeleteMany(Vec<u64>),
    /// Several connections to move into the group typed.
    MoveToGroup(Vec<u64>, String),
    Close(usize),
    Settings,
    Shortcuts,
    Library,
    Import(csv_io::Import),
}

#[derive(Clone, Copy, PartialEq)]
enum LibraryKind {
    Personal,
    Sqlite,
    Mssql,
    Mysql,
}

/// What the library dialog holds for one kind of database server.
struct ServerForm {
    connection: String,
    /// Keep the connection string in the credential store after it opens successfully.
    remember: bool,
    tls: bool,
}

impl ServerForm {
    /// A form filled from a remembered connection string, or starting from `example`.
    fn new(saved: Option<String>, example: &str, without_tls: bool) -> Self {
        Self {
            remember: saved.is_some(),
            connection: saved.unwrap_or_else(|| example.into()),
            tls: !without_tls,
        }
    }

    fn server(&self) -> database::Server {
        database::Server {
            connection: self.connection.clone(),
            tls: self.tls,
        }
    }
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
    /// The edited connection as saved when the editor opened, to keep others' edits since.
    editor_base: Option<Connection>,
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
    /// A silent background reload of a shared library, separate from `pending` so it never
    /// blocks or announces anything.
    sync: Option<std::sync::mpsc::Receiver<Result<database::Snapshot, String>>>,
    /// When the shared library was last reloaded in the background.
    synced: Instant,
    /// Whether the window had focus last frame, to reload when it returns.
    was_focused: bool,
    db_kind: LibraryKind,
    db_path: String,
    db_mssql: ServerForm,
    db_mysql: ServerForm,
    db_reveal: bool,
    /// Whether the open server library's connection string is in the credential store, so it
    /// reopens at startup.
    server_remembered: bool,
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
    /// Quick open, from the tab bar's +: a library search that also connects to typed hosts.
    quick_open: Option<Palette>,
    /// A snippet being added (no index) or edited in Settings.
    snippet_draft: Option<(Option<usize>, state::Snippet)>,
    pending_save: bool,
    /// A connection to open at the start of the next frame, outside any dialog or list.
    connect_request: Option<Connection>,
    /// Opens `connect_request` as an administrative remote desktop session.
    connect_admin: bool,
    /// A connection whose files to open at the start of the next frame.
    browse_request: Option<Connection>,
    /// The search for installed fonts, started when Settings first shows them.
    font_scan: Option<crate::fonts::Scan>,
    /// The Settings page shown when Settings opens.
    settings_tab: settings::SettingsTab,
    /// Connections chosen together in the library with Ctrl+click, Shift+click or Ctrl+A.
    /// `selected` stays the one the keyboard is on, and where a Shift+click range starts.
    marked: std::collections::BTreeSet<u64>,
    /// Connections to open in new tabs at the start of the next frame.
    connect_many: Vec<Connection>,
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
        let state_path = state::path();
        let saved = state::load(&state_path);
        let font_error = set_fonts(&cc.egui_ctx, saved.terminal.font.as_deref()).err();
        theme::load(&theme::path()).apply(&cc.egui_ctx);
        apply_style(&cc.egui_ctx);
        let path = model::data_path();
        let loaded = model::load(&path);
        let parent = platform::Window::of(cc);
        if let Some(parent) = parent {
            parent.set_theme(theme::current(&cc.egui_ctx));
            platform::install_shortcuts(parent, cc.egui_ctx.clone());
        }
        let mut app = Self::with(
            cc.egui_ctx.clone(),
            path,
            loaded,
            secret::load(secret::Account::Mssql),
            secret::load(secret::Account::Mysql),
            parent,
            saved,
        );
        app.state_path = Some(state_path);
        if let Some(error) = font_error {
            app.error = Some(error);
        }
        app.reopen_library();
        app.restore_tabs();
        app
    }

    fn with(
        ctx: egui::Context,
        path: PathBuf,
        loaded: Result<Vec<Connection>, String>,
        saved_mssql: Option<String>,
        saved_mysql: Option<String>,
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
            editor_base: None,
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
            sync: None,
            synced: Instant::now(),
            was_focused: true,
            db_kind: LibraryKind::Personal,
            db_path: String::new(),
            db_mssql: ServerForm::new(saved_mssql, MSSQL_EXAMPLE, state.mssql_without_tls),
            db_mysql: ServerForm::new(saved_mysql, "", state.mysql_without_tls),
            db_reveal: false,
            server_remembered: false,
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
            split_fraction: SPLIT_MIDDLE,
            palette: None,
            quick_open: None,
            snippet_draft: None,
            pending_save: false,
            connect_request: None,
            connect_admin: false,
            browse_request: None,
            font_scan: None,
            settings_tab: Default::default(),
            marked: Default::default(),
            connect_many: vec![],
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
        if self.state.forget_tabs {
            return;
        }
        for (index, connection) in self.state.open.clone().into_iter().enumerate() {
            let browsing = self.state.file_tabs.contains(&index);
            let mut session = if self.state.local_tabs.contains(&index) {
                Session::restored_local(self.ctx.clone())
            } else {
                Session::restored(connection, browsing, self.ctx.clone())
            };
            session.admin = self.state.admin_tabs.contains(&index);
            session.library = (self.state.tab_libraries.get(index))
                .unwrap_or(&self.state.library)
                .clone();
            self.sessions.push(session);
        }
        self.active = self.state.active_tab.filter(|i| *i < self.sessions.len());
    }

    /// Records the open tabs so the next start can restore them.
    fn remember_tabs(&mut self) {
        if self.state.forget_tabs {
            self.state.open.clear();
            self.state.file_tabs.clear();
            self.state.admin_tabs.clear();
            self.state.local_tabs.clear();
            self.state.tab_libraries.clear();
            self.state.active_tab = None;
            self.save_state();
            return;
        }
        self.state.open = self.sessions.iter().map(|s| s.connection.clone()).collect();
        self.state.file_tabs = (0..self.sessions.len())
            .filter(|i| self.sessions[*i].browsing)
            .collect();
        self.state.admin_tabs = (0..self.sessions.len())
            .filter(|i| self.sessions[*i].admin)
            .collect();
        self.state.local_tabs = (0..self.sessions.len())
            .filter(|i| self.sessions[*i].local)
            .collect();
        self.state.tab_libraries = self.sessions.iter().map(|s| s.library.clone()).collect();
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
            // Only a remembered connection string can reopen a server library.
            state::Library::Mssql(_) if self.db_mssql.remember => {
                self.db_kind = LibraryKind::Mssql;
                self.server_remembered = true;
                database::Location::Mssql(self.db_mssql.server())
            }
            state::Library::Mysql(_) if self.db_mysql.remember => {
                self.db_kind = LibraryKind::Mysql;
                self.server_remembered = true;
                database::Location::Mysql(self.db_mysql.server())
            }
            state::Library::Mssql(_) | state::Library::Mysql(_) => return,
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
    /// Workspace and tab shortcuts, including those a remote desktop forwards through its hook.
    /// Returns whether a visible terminal or remote desktop has the keyboard.
    fn workspace_shortcuts(&mut self, ctx: &egui::Context) -> bool {
        // Either side of a split view can hold the focused terminal or remote desktop.
        let terminal_focused = [self.active, self.split]
            .into_iter()
            .flatten()
            .filter_map(|i| self.sessions.get(i))
            .any(|s| s.keyboard_focused());
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
            if shortcut(ctrl | egui::Modifiers::SHIFT, egui::Key::T) {
                self.open_local_shell();
            }
            let ssh = self
                .active
                .and_then(|i| self.sessions.get(i))
                .is_some_and(|s| s.connection.protocol == Protocol::Ssh && s.terminal.is_some());
            if ssh && shortcut(ctrl | egui::Modifiers::SHIFT, egui::Key::P) {
                self.palette = Some(Palette::default());
            }
            // Tab switching, including Ctrl+1–9, also works from a focused terminal, which has no use
            // for these keys.
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
            if native.is_some()
                && let Some(parent) = self.parent
            {
                // Take the keyboard back from the remote desktop.
                platform::focus(parent);
            }
        }
        if !terminal_focused && !self.covered() {
            // Ctrl+Shift+N creates a folder in the file browser.
            if !ctx.input(|i| i.modifiers.shift) && shortcut(ctrl, egui::Key::N) {
                self.new_connection();
            }
            if shortcut(ctrl, egui::Key::K) {
                self.search_focus = true;
                self.active = None;
            }
            if shortcut(ctrl, egui::Key::T) {
                self.open_quick();
            }
            if shortcut(ctrl, egui::Key::B) {
                self.set_sidebar_collapsed(!self.sidebar_collapsed(ctx));
            }
            if shortcut(ctrl, egui::Key::W)
                && let Some(active) = self.active
            {
                self.request_close(active);
            }
            let shift = ctrl | egui::Modifiers::SHIFT;
            if let Some(active) = self.active {
                if shortcut(shift, egui::Key::PageUp) && active > 0 {
                    self.move_session(active, active - 1);
                } else if shortcut(shift, egui::Key::PageDown) {
                    self.move_session(active, active + 1);
                }
            }
        }
        terminal_focused
    }

    fn select_tab(&mut self, index: usize) {
        self.active = index.checked_sub(1).filter(|i| *i < self.sessions.len());
    }

    /// Whether a dialog covers the workspace. Native RDP windows sit above GPU content, so
    /// they are hidden while this is true.
    fn covered(&self) -> bool {
        self.quitting || !matches!(self.modal, Modal::None) || self.quick_open.is_some()
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
        // A library with repeated IDs cannot be loaded again, by anyone who shares it.
        let mut ids = std::collections::HashSet::new();
        if next.iter().any(|c| c.id == 0 || !ids.insert(c.id)) {
            self.error = Some("These changes would give two connections the same ID.".into());
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
        self.connect_admin = false;
        self.ctx.request_repaint();
    }

    /// Opens `connection`'s administrative session in a new tab on the next frame, as
    /// mstsc /admin does, without changing the saved connection.
    fn request_connect_admin(&mut self, connection: Connection) {
        self.request_connect(connection);
        self.connect_admin = true;
    }

    /// Opens a file browser for `connection` in a new tab on the next frame.
    fn request_browse(&mut self, connection: Connection) {
        self.browse_request = Some(connection);
        self.ctx.request_repaint();
    }

    fn connect(&mut self, connection: Connection, admin: bool) {
        self.open_session(connection, false, admin, state::Library::of(&self.location));
    }

    /// Opens a new tab for `connection` from `library`: its files if `browsing`, otherwise its
    /// terminal or desktop, the administrative session if `admin`.
    fn open_session(
        &mut self,
        connection: Connection,
        browsing: bool,
        admin: bool,
        library: state::Library,
    ) {
        let id = connection.id;
        let current = library == state::Library::of(&self.location);
        let started = if browsing {
            Session::browse(connection, self.ctx.clone())
        } else {
            Session::start(connection, self.ctx.clone())
        };
        match started {
            Ok(mut session) => {
                session.library = library;
                session.admin = admin;
                self.add_session(session);
                if id != 0 && current {
                    self.state.note_recent(&self.location, id);
                    self.save_state();
                }
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// Opens a shell on this computer in a new tab.
    fn open_local_shell(&mut self) {
        match Session::local(self.ctx.clone()) {
            Ok(session) => self.add_session(session),
            Err(e) => self.error = Some(e),
        }
    }

    /// Adds `session` as the last tab and selects it.
    fn add_session(&mut self, session: Session) {
        for s in &mut self.sessions {
            s.place(None);
        }
        self.sessions.push(session);
        self.active = Some(self.sessions.len() - 1);
        self.remember_tabs();
    }

    /// How terminals look now: the saved font size, in the chosen scheme.
    fn terminal_look(&self, ctx: &egui::Context) -> crate::terminal::Look {
        let settings = &self.state.terminal;
        crate::terminal::Look {
            font_size: settings.font_size,
            colors: settings.scheme.colors(theme::current(ctx).is_light()),
        }
    }

    /// Puts text a program in a terminal copied (OSC 52) on the clipboard, and says so, as a
    /// server could otherwise change what is pasted next without anyone noticing.
    fn take_copies(&mut self, ctx: &egui::Context) {
        let mut copied = None;
        for s in &self.sessions {
            if let Some(text) = s.terminal.as_ref().and_then(|t| t.take_copied()) {
                copied = Some((s.title().into_owned(), text));
            }
        }
        let Some((name, text)) = copied else {
            return;
        };
        if !self.state.terminal.programs_copy {
            return;
        }
        let count = text.chars().count();
        ctx.copy_text(text);
        let unit = if count == 1 {
            "character"
        } else {
            "characters"
        };
        self.notify(format!("{name} copied {count} {unit} to the clipboard"));
    }

    /// Keeps a font size a terminal zoomed to.
    fn zoomed(&mut self, font_size: f32) {
        if font_size != self.state.terminal.font_size {
            self.state.terminal.font_size = font_size;
            self.save_state();
        }
    }

    /// Shows session `index`'s terminal beside its file browser, or the other way round,
    /// reusing an open tab for the same connection when there is one.
    fn open_companion(&mut self, index: usize) {
        let Some(session) = self.sessions.get(index) else {
            return;
        };
        let (connection, browsing) = (session.connection.clone(), !session.browsing);
        let library = session.library.clone();
        let existing = self.sessions.iter().position(|s| {
            s.browsing == browsing
                && s.connection.same_target(&connection)
                && !self.detached.contains(&s.key)
        });
        let other = match existing {
            Some(other) => other,
            None => {
                let count = self.sessions.len();
                self.open_session(connection, browsing, false, library);
                if self.sessions.len() == count {
                    return;
                }
                count
            }
        };
        if self.is_detached(index) {
            self.active = Some(other);
            return;
        }
        self.active = Some(index);
        self.split = Some(other);
        self.remember_tabs();
    }

    /// Closes tab `index`, first asking while its session is still connected or connecting.
    /// A tab whose session ended, or that never connected, closes at once.
    fn request_close(&mut self, index: usize) {
        let Some(session) = self.sessions.get(index) else {
            return;
        };
        if session.tab_state() == session::TabState::Open {
            self.modal = Modal::Close(index);
        } else {
            self.close_tab(index);
        }
    }

    fn close_tab(&mut self, index: usize) {
        if index >= self.sessions.len() {
            return;
        }
        self.sessions.remove(index);
        let shift = |tab: Option<usize>| match tab {
            Some(a) if a == index => None,
            Some(a) if a > index => Some(a - 1),
            other => other,
        };
        self.active = shift(self.active);
        self.split = shift(self.split);
        self.remember_tabs();
    }

    /// Deletes the saved connections `ids`. When they were all the search found, the search
    /// clears, rather than offering to quick-connect to what was typed.
    fn delete_connections(&mut self, ids: &[u64]) -> bool {
        let last_match = !self.query.is_empty()
            && (self.filtered().iter()).all(|&i| ids.contains(&self.connections[i].id));
        let next = (self.connections.iter())
            .filter(|c| !ids.contains(&c.id))
            .cloned()
            .collect();
        if !self.persist(next) {
            return false;
        }
        if last_match {
            self.query.clear();
        }
        if self.selected.is_some_and(|id| ids.contains(&id)) {
            self.selected = None;
        }
        self.marked.retain(|id| !ids.contains(id));
        true
    }

    /// Applies `change` to each saved connection in `ids`, saving them all at once.
    fn change_connections(&mut self, ids: &[u64], change: impl Fn(&mut Connection)) -> bool {
        let mut next = self.connections.clone();
        for c in next.iter_mut().filter(|c| ids.contains(&c.id)) {
            change(c);
        }
        self.persist(next)
    }

    /// Detaches open sessions from connections that left the open library, so a connection
    /// added later with the same ID is not mistaken for theirs.
    fn forget_removed(&mut self) {
        let library = state::Library::of(&self.location);
        let mut changed = false;
        for s in &mut self.sessions {
            if s.connection.id != 0
                && s.library == library
                && !self.connections.iter().any(|c| c.id == s.connection.id)
            {
                s.connection.id = 0;
                changed = true;
            }
        }
        if changed {
            self.remember_tabs();
        }
    }

    /// Session `index`'s connection with its current saved settings, when it came from the
    /// open library.
    fn current_settings(&self, index: usize) -> Connection {
        let session = &self.sessions[index];
        (self.connections.iter())
            .filter(|_| session.library == state::Library::of(&self.location))
            .find(|c| c.id == session.connection.id)
            .unwrap_or(&session.connection)
            .clone()
    }

    /// Reconnects a session in its tab with the connection's current saved settings.
    fn reconnect(&mut self, index: usize) {
        let connection = self.current_settings(index);
        if let Err(e) = self.sessions[index].reconnect(connection) {
            self.error = Some(e);
        }
    }

    fn new_connection(&mut self) {
        if self.pending.is_some() {
            self.notify("Wait for the current library operation to finish.");
            return;
        }
        let mut connection = Connection::new();
        // A connection added while a group is open belongs to that group.
        if let Some(group) = &self.group {
            connection.group = group.clone();
        }
        self.edit(connection);
    }

    fn edit(&mut self, connection: Connection) {
        self.editor_base = Some(connection.clone()).filter(|c| c.id != 0);
        self.modal = Modal::Editor(connection);
        self.editor_error.clear();
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
                    .corner_radius(RADIUS_LARGE)
                    .shadow(ui.visuals().popup_shadow)
                    .inner_margin(egui::Margin {
                        left: 12,
                        right: 6,
                        top: 6,
                        bottom: 6,
                    })
                    .show(ui, |ui| {
                        ui.set_max_width(420.);
                        ui.horizontal(|ui| {
                            let (dot, _) =
                                ui.allocate_exact_size(Vec2::splat(8.), egui::Sense::hover());
                            ui.painter().circle_filled(dot.center(), 3.5, colors.accent);
                            ui.label(text);
                            dismiss = icon_button(ui, Icon::Close, 22., "Dismiss").clicked();
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
                let transfers: usize = self.sessions.iter().map(Session::active_transfers).sum();
                if transfers > 0 {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        format!("Unfinished file transfers ({transfers}) will be cancelled."),
                    );
                }
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
                        // The warning above was the last chance to export unsaved changes.
                        self.recovery = None;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if ui.button("Keep working").clicked() {
                        self.quitting = false;
                    }
                });
            });
            return;
        }
        if escape
            && matches!(
                self.modal,
                Modal::Delete(_) | Modal::Close(_) | Modal::DeleteMany(_) | Modal::MoveToGroup(..)
            )
        {
            self.modal = Modal::None;
        }
        match &mut self.modal {
            Modal::DeleteMany(ids) => {
                let ids = ids.clone();
                dialog("delete-many", ctx).show(ctx, |ui| {
                    ui.set_width(380.);
                    ui.heading(format!("Delete {} connections?", ids.len()));
                    let names: Vec<_> = (self.connections.iter())
                        .filter(|c| ids.contains(&c.id))
                        .map(|c| c.name.as_str())
                        .collect();
                    ui.label(
                        RichText::new(summary_of_names(&names)).color(theme::palette(ctx).muted),
                    );
                    ui.label("This removes the saved entries. Open sessions stay open.");
                    ui.horizontal(|ui| {
                        let delete = format!("Delete {} connections", ids.len());
                        if ui.button(delete).clicked() && self.delete_connections(&ids) {
                            self.modal = Modal::None;
                        }
                        if ui.button("Cancel").clicked() {
                            self.modal = Modal::None;
                        }
                    });
                });
                return;
            }
            Modal::MoveToGroup(ids, group) => {
                let groups: std::collections::BTreeSet<_> =
                    self.connections.iter().map(|c| c.group.clone()).collect();
                let (mut apply, mut cancel) = (false, false);
                dialog("move-to-group", ctx).show(ctx, |ui| {
                    ui.set_width(380.);
                    ui.heading(format!("Move {} connections to a group", ids.len()));
                    labeled(ui, "Group", |ui| group_input(ui, group, &groups));
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.horizontal(|ui| {
                        apply = ui
                            .add_enabled(!group.trim().is_empty(), egui::Button::new("Move"))
                            .clicked()
                            || (enter && !group.trim().is_empty());
                        cancel = ui.button("Cancel").clicked();
                    });
                });
                if apply {
                    let (ids, group) = (ids.clone(), group.trim().to_string());
                    let count = ids.len();
                    if self.change_connections(&ids, |c| c.group = group.clone()) {
                        self.modal = Modal::None;
                        self.notify(format!("Moved {count} connections to {group}"));
                    }
                } else if cancel {
                    self.modal = Modal::None;
                }
                return;
            }
            _ => {}
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
                        if ui.button("Delete connection").clicked()
                            && self.delete_connections(&[id])
                        {
                            self.modal = Modal::None;
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
                    .map_or("session".into(), |s| format!("“{}”", s.title()));
                let transfers = self
                    .sessions
                    .get(index)
                    .map_or(0, Session::active_transfers);
                dialog("close", ctx).show(ctx, |ui| {
                    ui.set_width(380.);
                    ui.heading(format!("Close {name}?"));
                    ui.label(
                        "The connection will end. Save your work on the remote machine first.",
                    );
                    if transfers > 0 {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            match transfers {
                                1 => "1 transfer is not finished and will be cancelled.".into(),
                                n => {
                                    format!("{n} transfers are not finished and will be cancelled.")
                                }
                            },
                        );
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Close session").clicked() {
                            self.modal = Modal::None;
                            self.close_tab(index);
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
        if !self.sessions.is_empty() && !self.state.forget_tabs {
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
        self.sync_library(ctx);
        if let Some(connection) = self.connect_request.take() {
            let admin = std::mem::take(&mut self.connect_admin);
            self.connect(connection, admin);
        }
        for connection in std::mem::take(&mut self.connect_many) {
            self.connect(connection, false);
        }
        if let Some(connection) = self.browse_request.take() {
            self.open_session(connection, true, false, state::Library::of(&self.location));
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
        let terminal_focused = self.workspace_shortcuts(ctx);
        // Before the views behind it, so its keys never reach them.
        self.quick_open(ctx);
        // A click on Relay's own interface takes the keyboard back from a remote desktop, whose
        // native window gets its clicks directly.
        if ctx.input(|i| i.pointer.any_pressed())
            && let Some(parent) = self.parent
        {
            platform::focus(parent);
        }
        for s in &mut self.sessions {
            s.poll(self.parent);
        }
        self.take_copies(ctx);
        if self.split == self.active {
            self.split = None;
        }
        // Only sessions on screen wake the window with their output; the others catch up when
        // their tab is shown.
        for (index, session) in self.sessions.iter().enumerate() {
            let shown = [self.active, self.split].contains(&Some(index)) || self.is_detached(index);
            session.set_shown(shown);
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
            if let Some(session) = self.active.and_then(|i| self.sessions.get_mut(i)) {
                if let Some(terminal) = &mut session.terminal {
                    terminal.focus();
                }
                if let Some(files) = &mut session.files {
                    files.view.focus_requested = true;
                }
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
            .frame(egui::Frame::new().fill(colors.bg).inner_margin(16.))
            .show(ctx, |ui| {
                if let Some(error) = self.error.clone() {
                    egui::Frame::new()
                        .fill(colors.error_bg)
                        .inner_margin(egui::Margin::symmetric(12, 8))
                        .corner_radius(RADIUS)
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
                            self.export(file_dialog::Kind::ExportJson, true);
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
                        let divider = split_divider(ui, area, &mut self.split_fraction);
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
        self.keyboard_shortcuts(ctx, terminal_focused);
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
    }
}

/// A few of `names`, and how many more there are, for a confirmation.
fn summary_of_names(names: &[&str]) -> String {
    const SHOWN: usize = 4;
    let mut text = names
        .iter()
        .take(SHOWN)
        .copied()
        .collect::<Vec<_>>()
        .join(", ");
    if names.len() > SHOWN {
        text.push_str(&format!(" and {} more", names.len() - SHOWN));
    }
    text
}

/// Uses the system's own fonts, and `terminal_font` for terminals if given. A terminal font
/// that cannot be read leaves the system's monospace font in place and says why.
fn set_fonts(ctx: &egui::Context, terminal_font: Option<&std::path::Path>) -> Result<(), String> {
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
    let mut result = Ok(());
    if let Some(path) = terminal_font {
        match crate::fonts::read(path) {
            Ok(bytes) => {
                let name = format!("terminal:{}", path.display());
                fonts
                    .font_data
                    .insert(name.clone(), egui::FontData::from_owned(bytes).into());
                (fonts.families.entry(egui::FontFamily::Monospace))
                    .or_default()
                    .insert(0, name);
            }
            Err(e) => result = Err(e),
        }
    }
    ctx.set_fonts(fonts);
    result
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

/// Where split view's divider starts, and returns to on a double-click.
const SPLIT_MIDDLE: f32 = 0.5;

/// The divider between split view's two sides in `area`, dragged to resize them and
/// double-clicked to return to the middle. Returns the room it takes between them.
fn split_divider(ui: &mut egui::Ui, area: egui::Rect, fraction: &mut f32) -> egui::Rect {
    let colors = theme::palette(ui.ctx());
    let gap = 16.;
    let x = area.left() + area.width() * *fraction;
    let divider = egui::Rect::from_min_max(
        egui::pos2(x - gap / 2., area.top()),
        egui::pos2(x + gap / 2., area.bottom()),
    );
    let handle = ui
        .interact(
            divider,
            egui::Id::new("split-divider"),
            egui::Sense::click_and_drag(),
        )
        .on_hover_cursor(egui::CursorIcon::ResizeHorizontal)
        .on_hover_text("Drag to resize · Double-click to center");
    if handle.double_clicked() {
        *fraction = SPLIT_MIDDLE;
    } else if handle.dragged()
        && let Some(pointer) = handle.interact_pointer_pos()
    {
        *fraction = ((pointer.x - area.left()) / area.width()).clamp(0.25, 0.75);
    }
    let line = if handle.hovered() || handle.dragged() {
        colors.accent
    } else {
        colors.line
    };
    ui.painter()
        .vline(x, area.y_range(), Stroke::new(1_f32, line));
    divider
}

/// egui's GPU setup, asking for memory in small blocks. By default wgpu reserves GPU memory
/// 256 MB at a time, which Relay's few textures never fill; on an integrated GPU, such as
/// most laptops', that memory comes out of the computer's RAM.
fn gpu_options() -> eframe::egui_wgpu::WgpuConfiguration {
    use eframe::{egui_wgpu, wgpu};
    let mut setup = egui_wgpu::WgpuSetupCreateNew::default();
    let device = setup.device_descriptor.clone();
    setup.device_descriptor = std::sync::Arc::new(move |adapter| wgpu::DeviceDescriptor {
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        ..device(adapter)
    });
    egui_wgpu::WgpuConfiguration {
        wgpu_setup: egui_wgpu::WgpuSetup::CreateNew(setup),
        ..Default::default()
    }
}

/// Relay's icon for its windows, rendered by assets/make_icon.py.
fn app_icon() -> egui::IconData {
    egui::IconData {
        rgba: include_bytes!("../../assets/icon.rgba").to_vec(),
        width: 256,
        height: 256,
    }
}
