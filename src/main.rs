#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod csv_io;
mod database;
mod file_dialog;
mod model;
mod native_host;
mod rdp;
mod secret;
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
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use windows_sys::Win32::Foundation::HWND;

fn main() -> eframe::Result {
    eframe::run_native(
        "Relay · Remote workspace",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1280., 820.])
                .with_min_inner_size([720., 560.]),
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
    /// The user's sidebar choice; without one, the sidebar collapses in narrow windows.
    sidebar_collapsed: Option<bool>,
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
        let loaded = model::load(&path);
        let parent = match cc.window_handle().map(|h| h.as_raw()) {
            Ok(raw_window_handle::RawWindowHandle::Win32(h)) => h.hwnd.get() as HWND,
            _ => std::ptr::null_mut(),
        };
        Self::with(
            cc.egui_ctx.clone(),
            path,
            loaded,
            secret::load(&secret::path()),
            parent,
        )
    }

    fn with(
        ctx: egui::Context,
        path: PathBuf,
        loaded: Result<Vec<Connection>, String>,
        saved_mssql: Option<String>,
        parent: HWND,
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
            sidebar_collapsed: None,
            pending_save: false,
        }
    }

    fn notify(&mut self, text: impl Into<String>) {
        self.notice = Some((text.into(), Instant::now()));
    }

    fn sidebar_collapsed(&self, ctx: &egui::Context) -> bool {
        self.sidebar_collapsed
            .unwrap_or(ctx.content_rect().width() < 1000.)
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
                        for protocol in [Protocol::Rdp, Protocol::Ssh] {
                            let button = egui::Button::new(protocol.label())
                                .selected(c.protocol == protocol);
                            if ui.add_sized([210., 40.], button).clicked() {
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
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label("Host / IP address");
                            ui.add_sized(
                                [330., 32.],
                                egui::TextEdit::singleline(&mut c.host)
                                    .hint_text("e.g. 192.168.1.20"),
                            );
                        });
                        ui.vertical(|ui| {
                            ui.label("Port");
                            ui.add_sized(
                                [90., 32.],
                                egui::DragValue::new(&mut c.port).range(1..=65535).speed(1),
                            );
                        });
                    });
                    let user_hint = if rdp { "DOMAIN\\username" } else { "username" };
                    field(ui, "Username (optional)", &mut c.username, user_hint);
                    ui.label("Group");
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [ui.available_width() - 46., 32.],
                            egui::TextEdit::singleline(&mut c.group).hint_text("Personal"),
                        );
                        ui.add_enabled_ui(!groups.is_empty(), |ui| {
                            ui.menu_button("▾", |ui| {
                                for group in &groups {
                                    if ui.button(group).clicked() {
                                        c.group = group.clone();
                                        ui.close();
                                    }
                                }
                            })
                            .response
                            .on_hover_text("Choose an existing group");
                        });
                    });
                    ui.checkbox(&mut c.favorite, "Add to favorites");
                });
            ui.add_space(8.);
            let help = if rdp {
                "Credentials are handled by Windows Remote Desktop.\n\
                 Passwords are never stored by Relay."
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
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let colors = theme::palette(ctx);
        self.poll_storage();
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
        if !self.covered() {
            // Tab switching also works from a focused terminal, which has no use for Ctrl+Tab.
            let tabs = self.sessions.len() + 1;
            let current = self.active.map_or(0, |i| i + 1);
            if shortcut(ctrl | egui::Modifiers::SHIFT, egui::Key::Tab) {
                self.select_tab((current + tabs - 1) % tabs);
            } else if shortcut(ctrl, egui::Key::Tab) {
                self.select_tab((current + 1) % tabs);
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
                self.sidebar_collapsed = Some(!self.sidebar_collapsed(ctx));
            }
            if shortcut(ctrl, egui::Key::W)
                && let Some(active) = self.active
            {
                self.modal = Modal::Close(active);
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
        self.toast(ctx);
        let covered = self.covered();
        let ppp = ctx.pixels_per_point();
        for (i, s) in self.sessions.iter_mut().enumerate() {
            let rect = if !covered && self.active == Some(i) {
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
fn field(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str) {
    ui.label(label);
    ui.add_sized(
        [ui.available_width(), 32.],
        egui::TextEdit::singleline(value).hint_text(hint),
    );
}
