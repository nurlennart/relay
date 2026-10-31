//! VNC desktops drawn by Relay inside the tab, on every platform. The connection runs on its
//! own thread; the tab connects at once, asks for a password or a Mac account only when the
//! server wants one, draws the screen and the server's pointer, and forwards keyboard, mouse,
//! wheel and copied text.
mod auth;
mod connection;
mod decode;
mod input;
mod rfb;

use crate::{frame, model::Connection};
use connection::{Command, Event, Outcome, Settings, Worker};
use eframe::egui::{self, RichText};
use rfb::{Credentials, Needs};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// How to leave full screen, for the Full screen button.
pub const FULL_SCREEN_HELP: &str = "Ctrl+Alt+Enter or the bar at the top edge leaves full \
    screen\nCtrl+Alt+PgUp/PgDn switch tabs";

/// How signing in works, for the connection editor.
pub const SIGN_IN_HELP: &str = "The tab asks for a password, or a Mac's user name and password, \
    when the server wants one, and keeps it only while the session is open.\nVNC does not \
    encrypt the session, so use it on networks you trust or through an SSH tunnel.";

enum Stage {
    SignIn {
        needs: Needs,
        credentials: Credentials,
        error: Option<String>,
        /// Whether the prompt still has to put the cursor where typing is needed.
        place_cursor: bool,
    },
    Running {
        worker: Worker,
        /// What was entered at the prompt, kept for reconnecting.
        credentials: Option<Credentials>,
        connected: bool,
        reconnecting: bool,
    },
    Ended(String),
}

/// The server's pointer as the tab draws it.
enum Pointer {
    /// None yet: the system's arrow shows.
    Unknown,
    /// The server hid its pointer.
    Hidden,
    Shape {
        texture: egui::TextureHandle,
        hotspot: [u16; 2],
    },
}

pub struct VncClient {
    connection: Connection,
    ctx: egui::Context,
    stage: Stage,
    texture: Option<egui::TextureHandle>,
    pointer: Pointer,
    translator: input::Translator,
    focused: bool,
    /// Whether the desktop fills the window in full screen.
    full_screen: bool,
    /// Take the keyboard the next time the desktop is drawn.
    focus_requested: bool,
    /// Text the remote side copied, so it is not offered back.
    copied: Option<String>,
    /// Whether the tab is on screen, shared with each connection.
    shown: Arc<AtomicBool>,
}

impl VncClient {
    /// A client that starts connecting at once; it asks for a password only if the server
    /// wants one.
    pub fn new(connection: &Connection, ctx: egui::Context) -> Self {
        let mut client = Self {
            connection: connection.clone(),
            ctx,
            stage: Stage::Ended(String::new()),
            texture: None,
            pointer: Pointer::Unknown,
            translator: input::Translator::default(),
            focused: false,
            full_screen: false,
            focus_requested: false,
            copied: None,
            shown: Arc::new(AtomicBool::new(true)),
        };
        client.start(None);
        client
    }

    /// Tells the connection whether its tab is on screen, so a hidden desktop's updates do not
    /// wake the window. Hiding it lets go of held keys and buttons, whose releases now go
    /// elsewhere.
    pub fn set_shown(&mut self, shown: bool) {
        self.shown.store(shown, Ordering::Relaxed);
        if !shown {
            self.focused = false;
            let input = self.translator.release_all();
            if let Stage::Running { worker, .. } = &self.stage
                && !input.is_empty()
            {
                worker.send(Command::Input(input));
            }
        }
    }

    /// Applies what the connection reported since the last frame.
    pub fn poll(&mut self) {
        let Stage::Running {
            worker,
            credentials,
            connected,
            reconnecting,
        } = &mut self.stage
        else {
            return;
        };
        let copied = worker.copied.lock().ok().and_then(|mut c| c.take());
        if let Some(text) = copied {
            self.ctx.copy_text(text.clone());
            self.copied = Some(text);
        }
        let mut next = None;
        for event in worker.events() {
            match event {
                Event::Connected { mac } => {
                    (*connected, *reconnecting) = (true, false);
                    self.translator.mac = mac;
                }
                Event::Reconnecting => *reconnecting = true,
                Event::Ended(Outcome::SignIn(needs, error)) => {
                    let username = match credentials {
                        Some(c) if !c.username.is_empty() => c.username.clone(),
                        _ => self.connection.username.clone(),
                    };
                    next = Some(Stage::SignIn {
                        needs,
                        credentials: Credentials {
                            username,
                            password: String::new(),
                        },
                        error,
                        place_cursor: true,
                    });
                }
                Event::Ended(Outcome::Failed(error)) => next = Some(Stage::Ended(error)),
            }
        }
        if let Some(next) = next {
            self.stage = next;
            self.texture = None;
            self.pointer = Pointer::Unknown;
            self.focused = false;
            self.full_screen = false;
            self.translator = input::Translator::default();
        }
    }

    pub fn ended(&self) -> bool {
        matches!(self.stage, Stage::Ended(_))
    }
    pub fn error(&self) -> Option<String> {
        match &self.stage {
            Stage::Ended(error) => Some(error.clone()),
            _ => None,
        }
    }
    pub fn reconnecting(&self) -> bool {
        matches!(
            self.stage,
            Stage::Running {
                reconnecting: true,
                ..
            }
        )
    }
    /// Full screen needs a desktop to show.
    pub fn can_full_screen(&self) -> bool {
        matches!(
            self.stage,
            Stage::Running {
                connected: true,
                ..
            }
        )
    }
    pub fn full_screen(&self) -> bool {
        self.full_screen
    }
    /// Asks to fill the window, giving the desktop the keyboard; Relay's window follows.
    pub fn enter_full_screen(&mut self) -> Result<(), String> {
        if !self.can_full_screen() {
            return Err("The remote desktop is not connected yet.".into());
        }
        self.full_screen = true;
        self.focus_requested = true;
        Ok(())
    }
    pub fn leave_full_screen(&mut self) {
        self.full_screen = false;
    }
    /// Whether the desktop has the keyboard, so Relay's own shortcuts stay out of its way.
    pub fn focused(&self) -> bool {
        self.focused
    }
    /// Gives the desktop, or the sign-in form before it, the keyboard when next shown.
    pub fn focus(&mut self) {
        match &mut self.stage {
            Stage::SignIn { place_cursor, .. } => *place_cursor = true,
            _ => self.focus_requested = true,
        }
    }
    /// Withdraws a request for the keyboard, such as the sign-in form's when it first shows.
    pub fn cancel_focus(&mut self) {
        self.focus_requested = false;
        if let Stage::SignIn { place_cursor, .. } = &mut self.stage {
            *place_cursor = false;
        }
    }

    /// Draws the sign-in prompt or desktop into the rest of `ui`, returning the area it covers.
    pub fn show(&mut self, ui: &mut egui::Ui) -> egui::Rect {
        let rect = ui.available_rect_before_wrap();
        self.poll();
        // A request made while connecting lapses, rather than taking the keyboard later.
        let focus = std::mem::take(&mut self.focus_requested);
        let mut start = None;
        match &mut self.stage {
            Stage::SignIn {
                needs,
                credentials,
                error,
                place_cursor,
            } => {
                let place = std::mem::take(place_cursor);
                if sign_in(
                    ui,
                    &self.connection,
                    needs,
                    credentials,
                    error.as_deref(),
                    place,
                ) {
                    start = Some(credentials.clone());
                }
            }
            Stage::Running {
                connected: false, ..
            } => {
                ui.add_space(50.);
                ui.spinner();
                ui.heading(format!("Connecting to {}…", self.connection.host));
            }
            Stage::Running { .. } => self.desktop(ui, rect, focus),
            Stage::Ended(_) => {}
        }
        if let Some(credentials) = start {
            self.start(Some(credentials));
        }
        rect
    }

    /// Starts connecting, with `credentials` once the user has entered them.
    fn start(&mut self, credentials: Option<Credentials>) {
        let options = &self.connection.vnc;
        let settings = Settings {
            host: self.connection.host.clone(),
            port: self.connection.port,
            credentials: credentials.clone(),
            clipboard: options.clipboard,
            view_only: options.view_only,
        };
        self.stage = match Worker::start(settings, self.ctx.clone(), self.shown.clone()) {
            Ok(worker) => Stage::Running {
                worker,
                credentials,
                connected: false,
                reconnecting: false,
            },
            Err(error) => Stage::Ended(error),
        };
    }

    /// Draws the remote desktop into `rect` and forwards input to it, taking the keyboard if
    /// `focus`.
    fn desktop(&mut self, ui: &mut egui::Ui, rect: egui::Rect, focus: bool) {
        let Stage::Running { worker, .. } = &self.stage else {
            return;
        };
        let view_only = self.connection.vnc.view_only;
        let (rect, response) = ui.allocate_exact_size(rect.size(), egui::Sense::click_and_drag());
        if !view_only && (focus || response.clicked() || response.drag_started()) {
            response.request_focus();
        }
        if response.has_focus() {
            // Tab, arrows, and Escape belong to the remote computer.
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    response.id,
                    egui::EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                )
            });
        }
        let focused = response.has_focus();
        let update = worker.frame.lock().ok().and_then(|mut f| f.take());
        if let Some(update) = update {
            frame::upload(ui.ctx(), &mut self.texture, update);
        }
        let shape = (worker.pointer.lock().ok())
            .filter(|p| p.changed)
            .map(|mut p| {
                p.changed = false;
                p.shape.clone()
            });
        if let Some(shape) = shape {
            self.pointer = pointer(ui.ctx(), shape);
        }
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0., egui::Color32::BLACK);
        let Some(texture) = &self.texture else {
            return;
        };
        let view = frame::View::fit(rect, texture.size());
        let whole = egui::Rect::from_min_max(egui::pos2(0., 0.), egui::pos2(1., 1.));
        painter.image(texture.id(), view.rect, whole, egui::Color32::WHITE);
        if let (Some(at), false) = (response.hover_pos(), view_only) {
            match &self.pointer {
                Pointer::Unknown => {}
                Pointer::Hidden => ui.ctx().set_cursor_icon(egui::CursorIcon::None),
                Pointer::Shape { texture, hotspot } => {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::None);
                    // The pointer scales with the desktop, as if it were part of the picture.
                    let origin = view.local([0., 0.]);
                    let spot = view.local(hotspot.map(f32::from)) - origin;
                    let size = view.local(texture.size().map(|v| v as f32)) - origin;
                    let shape = egui::Rect::from_min_size(at - spot, size);
                    ui.painter()
                        .image(texture.id(), shape, whole, egui::Color32::WHITE);
                }
            }
        }
        if view_only {
            return;
        }

        if focused {
            // Lets an input method compose text for the desktop, its window by the pointer.
            let at = response.hover_pos().unwrap_or(view.rect.center());
            let cursor_rect = egui::Rect::from_min_size(at, egui::vec2(1., 16.));
            ui.ctx().output_mut(|o| {
                o.ime = Some(egui::output::IMEOutput {
                    rect: view.rect,
                    cursor_rect,
                })
            });
        }
        let mut commands = vec![];
        let (events, modifiers) = ui.input(|i| (i.events.clone(), i.modifiers));
        let over = response.hovered() || response.is_pointer_button_down_on();
        if focused && self.connection.vnc.clipboard {
            // A paste shortcut carries the local clipboard; the remote side needs it first.
            for event in &events {
                if let egui::Event::Paste(text) = event
                    && self.copied.as_ref() != Some(text)
                {
                    commands.push(Command::Offer(text.clone()));
                }
            }
        }
        let mut input = self
            .translator
            .translate(&events, modifiers, &view, focused, over);
        if self.focused && !focused {
            // Keys held while focus moved away would otherwise stay down remotely.
            input.extend(self.translator.release_all());
        }
        self.focused = focused;
        if !input.is_empty() {
            commands.push(Command::Input(input));
        }
        if let Stage::Running { worker, .. } = &self.stage {
            for command in commands {
                worker.send(command);
            }
        }
    }
}

/// How the tab draws the server's pointer `shape`.
fn pointer(ctx: &egui::Context, shape: Option<decode::Cursor>) -> Pointer {
    let Some(shape) = shape else {
        return Pointer::Unknown;
    };
    if shape.size.contains(&0) || shape.rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 0) {
        return Pointer::Hidden;
    }
    let image = egui::ColorImage::from_rgba_unmultiplied(shape.size, &shape.rgba);
    Pointer::Shape {
        texture: ctx.load_texture("vnc-pointer", image, egui::TextureOptions::LINEAR),
        hotspot: shape.hotspot,
    }
}

/// The sign-in prompt. Returns whether the user asked to connect.
fn sign_in(
    ui: &mut egui::Ui,
    connection: &Connection,
    needs: &Needs,
    credentials: &mut Credentials,
    error: Option<&str>,
    place_cursor: bool,
) -> bool {
    let account = *needs == Needs::Account;
    let mut connect = false;
    ui.add_space(40.);
    ui.vertical_centered(|ui| {
        ui.set_max_width(340.);
        ui.heading(format!("Sign in to {}", connection.host));
        ui.add_space(12.);
        if let Some(error) = error {
            ui.colored_label(ui.visuals().error_fg_color, error);
            ui.add_space(8.);
        }
        let user = account.then(|| {
            ui.label("User name");
            let user = ui.add(
                egui::TextEdit::singleline(&mut credentials.username)
                    .hint_text(RichText::new("An account on the Mac").weak())
                    .desired_width(f32::INFINITY),
            );
            ui.add_space(6.);
            user
        });
        ui.label("Password");
        let password = ui.add(
            egui::TextEdit::singleline(&mut credentials.password)
                .password(true)
                .desired_width(f32::INFINITY),
        );
        // Enter leaves a single-line field, so read it before moving the cursor anywhere.
        let left = password.lost_focus() || user.as_ref().is_some_and(|u| u.lost_focus());
        let enter = left && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if place_cursor {
            match &user {
                Some(user) if credentials.username.is_empty() => user.request_focus(),
                _ => password.request_focus(),
            }
        }
        ui.add_space(12.);
        let ready = !account || !credentials.username.trim().is_empty();
        connect = ui
            .add_enabled(ready, egui::Button::new("Connect"))
            .clicked()
            || (enter && ready);
        ui.add_space(8.);
        let note = if account {
            "Relay keeps the password only while this session is open."
        } else {
            "Relay keeps the password only while this session is open. VNC uses only its \
             first eight characters."
        };
        ui.label(RichText::new(note).size(12.).weak());
    });
    if connect {
        credentials.username = credentials.username.trim().into();
    }
    connect
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_screen_waits_for_a_desktop() {
        let mut c = Connection::new();
        // Nothing listens on port 9 here, so the client never gets a desktop.
        c.host = "127.0.0.1".into();
        c.port = 9;
        let mut client = VncClient::new(&c, egui::Context::default());
        assert!(!client.can_full_screen());
        assert!(client.enter_full_screen().is_err());
        assert!(!client.full_screen());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !client.ended() {
            assert!(
                std::time::Instant::now() < deadline,
                "the attempt did not end"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
            client.poll();
        }
        let error = client.error().unwrap();
        assert!(
            error.starts_with("Cannot connect to 127.0.0.1: "),
            "{error}"
        );
    }

    #[test]
    fn empty_and_transparent_pointers_hide_it() {
        let ctx = egui::Context::default();
        let shape = |size: [usize; 2], alpha: u8| decode::Cursor {
            size,
            hotspot: [0, 0],
            rgba: [0, 0, 0, alpha].repeat(size[0] * size[1]),
        };
        assert!(matches!(pointer(&ctx, None), Pointer::Unknown));
        assert!(matches!(
            pointer(&ctx, Some(shape([0, 0], 255))),
            Pointer::Hidden
        ));
        assert!(matches!(
            pointer(&ctx, Some(shape([2, 2], 0))),
            Pointer::Hidden
        ));
        assert!(matches!(
            pointer(&ctx, Some(shape([2, 2], 255))),
            Pointer::Shape { .. }
        ));
    }
}
