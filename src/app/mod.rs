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
    state, theme, vnc,
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

#[derive(Clone, Copy, PartialEq)]
enum Filter {
    All,
    Recent,
    Favorites,
    Rdp,
    Ssh,
    Vnc,
}

/// The dialog covering the workspace. Only one is open at a time.
#[derive(Default)]
enum Modal {
    #[default]
    None,
    Editor(Connection),
    /// Connections to delete: one, or several chosen in the library together.
    Delete(Vec<u64>),
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
    /// Open sessions, in no particular order; each is known by its `key`.
    sessions: Vec<Session>,
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
    /// Counts replacements of `connections`, so views of them know when to look again.
    generation: u64,
    /// The library as last shown, kept until what it was made from changes.
    views: std::cell::RefCell<library::Views>,
    pending: Option<std::sync::mpsc::Receiver<Result<database::Snapshot, String>>>,
    pending_location: Option<database::Location>,
    recovery: Option<Vec<Connection>>,
    pending_editor: Option<Connection>,
    /// A silent background reload of a shared library, separate from `pending` so it never
    /// blocks or announces anything.
    sync: Option<std::sync::mpsc::Receiver<Result<Option<database::Snapshot>, String>>>,
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
    /// The tab in front, by the key of a session it shows; `None` shows the library. Which side
    /// of a pair is in front is the pair's own `focus`, so `active` derives the session.
    front: Option<u64>,
    /// Whether a tab or side was chosen since the last frame, which the keyboard a side still
    /// holds from before must not undo.
    chosen: bool,
    /// The session tab shown last, by key, and the one shown before it, so closing a tab
    /// returns to the one it was opened from.
    tab_trail: (Option<u64>, Option<u64>),
    /// Sessions shown in their own windows, by session key.
    detached: Vec<u64>,
    /// The main window while a remote desktop fills it in full screen.
    full_window: Option<FullWindow>,
    /// When the main window was last full screen, or on its way in or out, so its size then
    /// is not remembered as its ordinary size.
    full_screen_seen: Option<Instant>,
    /// Whether the bar for leaving full screen shows, from the pointer reaching the top edge.
    full_screen_bar: bool,
    /// Native windows found for detached sessions, which remote desktops are moved into.
    windows: Vec<(u64, platform::Window)>,
    /// The tab strip, in order: each tab one session or a pair side by side, by session key,
    /// with the side of each pair that has the keyboard. Every session has exactly one tab;
    /// `sync_tabs` keeps it so.
    tabs: Vec<tabs::Tab>,
    /// The sessions on screen that had the keyboard last frame, to notice one taking it.
    keyboard: Vec<u64>,
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
/// How long after full screen the window's size is left alone, while it returns to its own.
const FULL_SCREEN_SETTLE: Duration = Duration::from_secs(1);

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
        // Within the range the zoom keys reach, in case the file was edited by hand.
        if let Some(zoom) = saved.zoom {
            cc.egui_ctx.set_zoom_factor(zoom.clamp(0.2, 5.));
        }
        let path = model::data_path();
        let loaded = database::load_personal(&path);
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
        loaded: Result<database::Snapshot, String>,
        saved_mssql: Option<String>,
        saved_mysql: Option<String>,
        parent: Option<platform::Window>,
        state: state::State,
    ) -> Self {
        let location = database::Location::Local(path.clone());
        let (connections, revision, error) = match loaded {
            Ok(snapshot) => (snapshot.connections, snapshot.revision, None),
            Err(e) => (vec![], 0, Some(e)),
        };
        Self {
            connections,
            sessions: vec![],
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
            revision,
            generation: 0,
            views: Default::default(),
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
            front: None,
            chosen: false,
            tab_trail: (None, None),
            detached: vec![],
            full_window: None,
            full_screen_seen: None,
            full_screen_bar: false,
            windows: vec![],
            tabs: vec![],
            keyboard: vec![],
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
        let first = self.sessions.len();
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
        self.sync_tabs();
        // Pairs that no longer fit the saved tabs, such as one naming a tab twice, are dropped.
        let keys: Vec<u64> = self.sessions[first..].iter().map(|s| s.key).collect();
        for saved in self.state.pairs.clone() {
            let (Some(&left), Some(&right)) = (keys.get(saved.left), keys.get(saved.right)) else {
                continue;
            };
            let focus = if saved.right_focused { right } else { left };
            if let Some(place) = self.join(left, right, focus)
                && let tabs::Tab::Pair(pair) = &mut self.tabs[place]
            {
                pair.fraction = saved.fraction.clamp(SPLIT_MIN, 1. - SPLIT_MIN);
            }
        }
        self.activate(self.state.active_tab.and_then(|i| keys.get(i).copied()));
    }

    /// Records the open tabs so the next start can restore them.
    fn remember_tabs(&mut self) {
        if self.state.forget_tabs {
            self.state.open.clear();
            self.state.file_tabs.clear();
            self.state.admin_tabs.clear();
            self.state.local_tabs.clear();
            self.state.tab_libraries.clear();
            self.state.pairs.clear();
            self.state.active_tab = None;
            self.save_state();
            return;
        }
        self.record_tabs();
        self.save_state();
    }

    /// Writes the open tabs into `state`, in strip order, without saving it.
    fn record_tabs(&mut self) {
        self.sync_tabs();
        let order = self.strip_order();
        let sessions: Vec<&Session> = order.iter().map(|&i| &self.sessions[i]).collect();
        let which = |test: fn(&Session) -> bool| {
            (0..sessions.len()).filter(|&i| test(sessions[i])).collect()
        };
        self.state.file_tabs = which(|s| s.browsing);
        self.state.admin_tabs = which(|s| s.admin);
        self.state.local_tabs = which(|s| s.local);
        self.state.open = sessions.iter().map(|s| s.connection.clone()).collect();
        self.state.tab_libraries = sessions.iter().map(|s| s.library.clone()).collect();
        let saved = |key| order.iter().position(|&i| self.sessions[i].key == key);
        self.state.pairs = (self.tabs.iter())
            .filter_map(|tab| match *tab {
                tabs::Tab::Pair(pair) => Some(state::SavedPair {
                    left: saved(pair.left)?,
                    right: saved(pair.right)?,
                    fraction: pair.fraction,
                    right_focused: pair.focus == pair.right,
                }),
                tabs::Tab::Single(_) => None,
            })
            .collect();
        let active = self.active();
        self.state.active_tab = active.and_then(|a| order.iter().position(|&i| i == a));
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

    /// Workspace and tab shortcuts, including those a remote desktop forwards through its hook.
    /// Returns whether a visible terminal or remote desktop has the keyboard.
    fn workspace_shortcuts(&mut self, ctx: &egui::Context) -> bool {
        // Either side of a pair can hold the focused terminal or remote desktop.
        let terminal_focused = (self.on_screen().into_iter())
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
                .active()
                .and_then(|i| self.sessions.get(i))
                .is_some_and(|s| s.connection.protocol == Protocol::Ssh && s.terminal.is_some());
            if ssh && shortcut(ctrl | egui::Modifiers::SHIFT, egui::Key::P) {
                self.palette = Some(Palette::default());
            }
            // Ctrl+Alt+Enter, as in FreeRDP: macOS and laptop keyboards often lack Break.
            let desktop =
                (self.active()).filter(|&i| !self.is_detached(i) && self.sessions[i].has_desktop());
            if let Some(index) = desktop
                && self.sessions[index].full_screen_in_window()
                && shortcut(
                    egui::Modifiers::CTRL | egui::Modifiers::ALT,
                    egui::Key::Enter,
                )
            {
                if self.sessions[index].full_screen() {
                    self.sessions[index].leave_full_screen();
                } else {
                    self.enter_full_screen(index);
                }
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
            // A pair counts as one tab.
            let tabs = self.tabs.len() + 1;
            let current = self.active_tab().map_or(0, |i| i + 1);
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
                self.select_tab(self.tabs.len());
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
                self.activate(None);
            }
            if shortcut(ctrl, egui::Key::T) {
                self.open_quick();
            }
            if shortcut(ctrl, egui::Key::B) {
                self.set_sidebar_collapsed(!self.sidebar_collapsed(ctx));
            }
            if shortcut(ctrl, egui::Key::W)
                && let Some(active) = self.active()
            {
                self.request_close(active);
            }
            let shift = ctrl | egui::Modifiers::SHIFT;
            if let Some(place) = self.active_tab() {
                if shortcut(shift, egui::Key::PageUp) && place > 0 {
                    self.move_tab(place, place - 1);
                } else if shortcut(shift, egui::Key::PageDown) {
                    self.move_tab(place, place + 1);
                }
            }
        }
        terminal_focused
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

    /// Remembers the restored window size; a maximized window keeps the size it returns to.
    /// Sizes from full screen, and from the moments entering and leaving it, are not the
    /// window's own. The window opens at its size before zooming, which the zoomed points are
    /// scaled back to.
    fn remember_window(
        &mut self,
        inner: Option<egui::Rect>,
        maximized: Option<bool>,
        full_screen: bool,
        zoom: f32,
    ) {
        if full_screen {
            self.full_screen_seen = Some(Instant::now());
        }
        if self
            .full_screen_seen
            .is_some_and(|at| at.elapsed() < FULL_SCREEN_SETTLE)
        {
            return;
        }
        if let Some(maximized) = maximized {
            self.state.maximized = maximized;
            if !maximized && let Some(inner) = inner {
                let size = inner.size() * zoom;
                self.state.window = Some([size.x.round(), size.y.round()]);
            }
        }
    }

    /// Shows session `index`'s remote desktop full screen.
    fn enter_full_screen(&mut self, index: usize) {
        let in_window = self.sessions[index].full_screen_in_window();
        match self.sessions[index].enter_full_screen() {
            Ok(()) if in_window => {
                self.notify("Ctrl+Alt+Enter or the top edge of the screen leaves full screen");
            }
            Ok(()) => {}
            Err(e) => self.error = Some(e),
        }
    }

    /// Fills the window for the remote desktop in front that asked for full screen, and
    /// returns it; others in the background leave full screen.
    fn sync_full_screen(&mut self, ctx: &egui::Context) -> Option<usize> {
        // Windows' RDP control keeps a full screen of its own, which Relay leaves alone.
        let active = self.active().filter(|&i| !self.is_detached(i));
        for (i, s) in self.sessions.iter_mut().enumerate() {
            if Some(i) != active && s.full_screen() && s.full_screen_in_window() {
                s.leave_full_screen();
            }
        }
        let shown = active.filter(|&i| {
            let s = &self.sessions[i];
            s.full_screen() && s.full_screen_in_window()
        });
        let window = ctx.input(|i| i.viewport().fullscreen) == Some(true);
        let (state, command) = full_window_step(self.full_window, shown.is_some(), window);
        self.full_window = state;
        if let Some(on) = command {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
        }
        if state.is_none() {
            self.full_screen_bar = false;
            // Left through the window itself, such as macOS's green button.
            if let Some(index) = shown {
                self.sessions[index].leave_full_screen();
            }
            return None;
        }
        shown
    }

    /// The library or the sessions in front, below the tab bar, noting in `session_rects`
    /// the area each remote desktop covers.
    fn workspace_panel(
        &mut self,
        ctx: &egui::Context,
        session_rects: &mut Vec<(usize, egui::Rect)>,
    ) {
        let colors = theme::palette(ctx);
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
                let pair = (self.active_pair()).and_then(|(place, pair)| {
                    let left = self.session_index(pair.left)?;
                    let right = self.session_index(pair.right)?;
                    Some((place, pair.fraction, left, right))
                });
                match (self.active(), pair) {
                    (Some(index), _) if self.is_detached(index) => {
                        self.detached_placeholder(ui, index)
                    }
                    (Some(_), Some((place, fraction, left, right))) => {
                        self.pair_panes(ui, place, fraction, [left, right], session_rects);
                    }
                    (Some(index), None) => {
                        if let Some(rect) = self.session_pane(ui, index, Pane::Main) {
                            session_rects.push((index, rect));
                        }
                    }
                    (None, _) => self.workspace(ui),
                }
            });
    }

    /// Session `index`'s remote desktop alone in the window, with a bar to leave full screen
    /// while the pointer is at the top edge, as in Remote Desktop Connection.
    fn full_screen_desktop(&mut self, ctx: &egui::Context, index: usize) {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(egui::Color32::BLACK))
            .show(ctx, |ui| {
                self.sessions[index].show_desktop(ui);
            });
        let pointer = ctx.input(|i| i.pointer.hover_pos());
        let at_edge = |p: egui::Pos2| p.y <= 2.;
        if pointer.is_some_and(at_edge) {
            self.full_screen_bar = true;
        }
        if !self.full_screen_bar {
            return;
        }
        let mut leave = false;
        let bar = egui::Area::new(egui::Id::new("full screen bar"))
            .anchor(egui::Align2::CENTER_TOP, Vec2::ZERO)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.label(self.sessions[index].title());
                        leave = ui
                            .button("Leave full screen")
                            .on_hover_text("Ctrl+Alt+Enter")
                            .clicked();
                    });
                });
            })
            .response;
        if leave {
            self.sessions[index].leave_full_screen();
        }
        if leave || !pointer.is_some_and(|p| at_edge(p) || bar.rect.expand(12.).contains(p)) {
            self.full_screen_bar = false;
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
        self.sync_tabs();
        self.activate_session(self.sessions.len() - 1);
        self.remember_tabs();
    }

    /// How terminals look now: the saved font size, in the chosen scheme.
    fn terminal_look(&self, ctx: &egui::Context) -> crate::terminal::Look {
        let settings = &self.state.terminal;
        crate::terminal::Look {
            font_size: settings.font_size,
            colors: settings.scheme.colors(theme::current(ctx)),
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
        // An open tab of its own, or the other side of this session's pair.
        let key = session.key;
        let partner = self.partner(key);
        let existing = self.sessions.iter().position(|s| {
            s.browsing == browsing
                && s.connection.same_target(&connection)
                && !self.detached.contains(&s.key)
                && (self.tabs.contains(&tabs::Tab::Single(s.key)) || partner == Some(s.key))
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
        let other_key = self.sessions[other].key;
        // A session in its own window, or already beside another, shows the other in a tab.
        if partner == Some(other_key) || self.join(key, other_key, key).is_none() {
            self.activate_session(other);
            return;
        }
        self.activate_session(index);
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
        self.sync_tabs();
        let in_front = self.active() == Some(index);
        let key = self.sessions[index].key;
        let (partner, place) = (self.partner(key), self.tab_of(key).unwrap_or(0));
        self.sessions.remove(index);
        // A pair's other side stays, as a tab of its own.
        self.sync_tabs();
        // Closing the session in front shows another rather than the library: the other side
        // of its pair, the one it was opened from, or the tab to its left.
        if in_front {
            let open = |key: Option<u64>| key.filter(|&k| self.session_index(k).is_some());
            let left = (self.tabs.get(place.saturating_sub(1))).map(tabs::Tab::focus);
            self.activate(open(partner).or(open(self.tab_trail.1)).or(left));
        }
        self.remember_tabs();
    }

    /// Settles which side of the tab in front has the keyboard. A side that takes it by
    /// itself, such as a remote desktop clicked into, comes in front, unless a side was chosen
    /// since the last frame: one still holding the keyboard from before must not undo that.
    fn follow_focus(&mut self) {
        let focused: Vec<u64> = (self.on_screen().into_iter())
            .map(|i| &self.sessions[i])
            .filter(|s| s.keyboard_focused())
            .map(|s| s.key)
            .collect();
        let taken = (focused.iter().copied()).find(|key| !self.keyboard.contains(key));
        if !std::mem::take(&mut self.chosen)
            && let Some(key) = taken
        {
            self.bring_forward(Some(key));
        }
        self.keyboard = focused;
        // Only the side in front takes the keyboard when asked, such as a new terminal's.
        let active = self.active();
        for index in self.on_screen() {
            if Some(index) != active {
                self.sessions[index].cancel_focus();
            }
        }
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
        // A connection added while a group or protocol is open belongs to it.
        if let Some(group) = &self.group {
            connection.group = group.clone();
        }
        let protocol = match self.filter {
            Filter::Rdp => Some(Protocol::Rdp),
            Filter::Ssh => Some(Protocol::Ssh),
            Filter::Vnc => Some(Protocol::Vnc),
            Filter::All | Filter::Recent | Filter::Favorites => None,
        };
        if let Some(protocol) = protocol {
            connection.protocol = protocol;
            connection.port = protocol.port();
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
                let edits: usize = self.sessions.iter().map(Session::unsaved_edits).sum();
                if edits > 0 {
                    ui.colored_label(ui.visuals().warn_fg_color, unsaved_edits(edits));
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
                        self.remember_tabs();
                        self.sessions.clear();
                        self.activate(None);
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
                Modal::Delete(_) | Modal::Close(_) | Modal::MoveToGroup(..)
            )
        {
            self.modal = Modal::None;
        }
        match &mut self.modal {
            Modal::Delete(ids) => {
                let ids = ids.clone();
                let names: Vec<_> = (self.connections.iter())
                    .filter(|c| ids.contains(&c.id))
                    .map(|c| c.name.clone())
                    .collect();
                dialog("delete", ctx).show(ctx, |ui| {
                    ui.set_width(380.);
                    let delete = if let [name] = &names[..] {
                        ui.heading(format!("Delete “{name}”?"));
                        ui.label("This removes the saved entry. Any open session stays open.");
                        "Delete connection".to_string()
                    } else {
                        ui.heading(format!("Delete {} connections?", ids.len()));
                        ui.label(
                            RichText::new(summary_of_names(&names))
                                .color(theme::palette(ctx).muted),
                        );
                        ui.label("This removes the saved entries. Open sessions stay open.");
                        format!("Delete {} connections", ids.len())
                    };
                    ui.horizontal(|ui| {
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
        if let Modal::Close(index) = self.modal {
            let name = self
                .sessions
                .get(index)
                .map_or("session".into(), |s| format!("“{}”", s.title()));
            let transfers = self
                .sessions
                .get(index)
                .map_or(0, Session::active_transfers);
            let edits = self.sessions.get(index).map_or(0, Session::unsaved_edits);
            dialog("close", ctx).show(ctx, |ui| {
                ui.set_width(380.);
                ui.heading(format!("Close {name}?"));
                ui.label("The connection will end. Save your work on the remote machine first.");
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
                if edits > 0 {
                    ui.colored_label(ui.visuals().warn_fg_color, unsaved_edits(edits));
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
    }
}

impl eframe::App for App {
    fn on_exit(&mut self) {
        // Quitting ends sessions first; keep the tabs recorded before that.
        // Selecting tabs and resizing pairs leave the record to here.
        if !self.sessions.is_empty() && !self.state.forget_tabs {
            self.record_tabs();
        }
        self.save_state();
    }

    #[cfg(feature = "ui-check")]
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        ui_check::hover(raw_input);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
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
        // Remember the zoom, whether set in Settings or with the keys.
        let zoom = ctx.zoom_factor();
        if self.state.zoom.unwrap_or(1.) != zoom {
            self.state.zoom = (zoom != 1.).then_some(zoom);
            self.save_state();
        }
        let viewport = ctx.input(|i| i.viewport().clone());
        let full_screen = self.full_window.is_some() || viewport.fullscreen == Some(true);
        self.remember_window(viewport.inner_rect, viewport.maximized, full_screen, zoom);
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
        self.sync_tabs();
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
        let on_screen = self.on_screen();
        // Only sessions on screen wake the window with their output; the others catch up when
        // their tab is shown.
        for index in 0..self.sessions.len() {
            let shown = on_screen.contains(&index) || self.is_detached(index);
            self.sessions[index].set_shown(shown);
        }
        // A tab restored from the last run connects when it is first shown.
        for &index in &on_screen {
            if let Some(session) = self.sessions.get_mut(index)
                && let Err(e) = session.resume()
            {
                self.error = Some(e);
            }
        }
        self.follow_focus();
        // Hide native children before painting overlays; embedded remote desktops sit above GPU content.
        let covered = self.covered();
        for (i, s) in self.sessions.iter_mut().enumerate() {
            let detached = self.detached.contains(&s.key);
            if !detached && (!on_screen.contains(&i) || covered) {
                s.place(None);
            }
        }
        let full_screen = self.sync_full_screen(ctx);
        // Sessions on screen, with the area each remote desktop should cover.
        let mut session_rects: Vec<(usize, egui::Rect)> = vec![];
        if let Some(index) = full_screen {
            self.full_screen_desktop(ctx, index);
        } else {
            self.sidebar(ctx);
            self.tabs(ctx);
            self.workspace_panel(ctx, &mut session_rects);
        }
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
        // Toasts, tooltips and menus float over remote desktops; cut them out of each.
        let floating = overlays(ctx);
        for (i, s) in self.sessions.iter_mut().enumerate() {
            // Detached windows are separate from the main window's dialogs.
            if let Some((_, rect, holes)) = windows.iter().find(|(index, ..)| *index == i) {
                s.place_around(Some(*rect), holes);
                continue;
            }
            match (session_rects.iter()).find(|(index, _)| *index == i && !covered) {
                Some((_, rect)) => {
                    let (area, holes) = native_area(*rect, ppp, &floating);
                    s.place_around(Some(area), &holes);
                }
                None => s.place(None),
            }
        }
    }
}

/// The main window while a remote desktop fills it in full screen.
#[derive(Clone, Copy, Debug, PartialEq)]
struct FullWindow {
    /// The window has reported being full screen since the desktop asked for it.
    confirmed: bool,
    /// The window was full screen before, so it stays that way afterwards.
    was_full_screen: bool,
}

/// The next full-screen state of the main window, given whether a desktop in front `wants`
/// it and whether the `window` reports being full screen, and the command to send it.
/// `None` while a desktop wants it means the window left full screen on its own.
fn full_window_step(
    state: Option<FullWindow>,
    wants: bool,
    window: bool,
) -> (Option<FullWindow>, Option<bool>) {
    match (state, wants) {
        (None, false) => (None, None),
        (None, true) => {
            let state = FullWindow {
                confirmed: window,
                was_full_screen: window,
            };
            (Some(state), (!window).then_some(true))
        }
        (Some(state), true) if window => (
            Some(FullWindow {
                confirmed: true,
                ..state
            }),
            None,
        ),
        (Some(state), true) if state.confirmed => (None, None),
        // Some window managers take a moment, and some never go full screen.
        (Some(state), true) => (Some(state), None),
        (Some(state), false) => (None, (!state.was_full_screen).then_some(false)),
    }
}

/// A few of `names`, and how many more there are, for a confirmation.
fn summary_of_names(names: &[String]) -> String {
    const SHOWN: usize = 4;
    let mut text = names[..names.len().min(SHOWN)].join(", ");
    if names.len() > SHOWN {
        text.push_str(&format!(" and {} more", names.len() - SHOWN));
    }
    text
}

/// Warns that `count` edited files have changes not on the server, before closing.
fn unsaved_edits(count: usize) -> String {
    let what = match count {
        1 => "1 edited file has changes".to_string(),
        n => format!("{n} edited files have changes"),
    };
    format!("{what} not on the server. The copies stay in the temporary folder.")
}

/// Uses the system's own fonts, and `terminal_font` for terminals if given. A terminal font
/// that cannot be read leaves the system's monospace font in place and says why.
fn set_fonts(ctx: &egui::Context, terminal_font: Option<&std::path::Path>) -> Result<(), String> {
    let mut fonts = egui::FontDefinitions::default();
    // Every font is checked before egui gets it, as egui panics on one it cannot parse; a
    // missing or damaged system font is passed over.
    // Prefer the system's own fonts; a real terminal font matters for SSH sessions.
    for (family, monospace) in [
        (egui::FontFamily::Proportional, false),
        (egui::FontFamily::Monospace, true),
    ] {
        if let Some((path, bytes)) = platform::fonts(monospace)
            .iter()
            .find_map(|path| Some((path, crate::fonts::read(path.as_ref()).ok()?)))
        {
            let name = path.to_string();
            fonts
                .font_data
                .insert(name.clone(), egui::FontData::from_owned(bytes).into());
            fonts.families.entry(family).or_default().insert(0, name);
        }
    }
    // Symbols that no font before has, such as the ✻ and ⎿ in command-line tools' status
    // lines, come from the system's symbol fonts rather than showing as boxes.
    for path in platform::symbol_fonts() {
        if let Ok(bytes) = crate::fonts::read(path.as_ref()) {
            let name = path.to_string();
            fonts
                .font_data
                .insert(name.clone(), egui::FontData::from_owned(bytes).into());
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.entry(family).or_default().push(name.clone());
            }
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

/// Where split view's divider starts, and returns to on a double-click.
const SPLIT_MIDDLE: f32 = 0.5;
/// The narrowest a side of split view gets, as a fraction of the width.
const SPLIT_MIN: f32 = 0.25;

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
        *fraction = ((pointer.x - area.left()) / area.width()).clamp(SPLIT_MIN, 1. - SPLIT_MIN);
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
