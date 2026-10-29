//! Remote desktops drawn by IronRDP inside the tab. The connection runs on its own thread; the
//! tab asks for the password, shows certificate decisions, draws the desktop as a texture, and
//! forwards keyboard, mouse, wheel, resizes, and clipboard text. Nothing here is platform
//! specific beyond the choice made in `rdp/mod.rs`.
mod clipboard;
mod connection;
mod frame;
mod input;

use crate::{
    model::{Audio, Connection, Display, Keys, RdpOptions},
    platform::Window,
};
use connection::{Command, Event, Outcome, Settings, Untrusted, Worker};
use eframe::egui::{self, RichText};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// Relay never saves RDP passwords here, so there is nothing to forget.
pub const SAVES_PASSWORDS: bool = false;

/// IronRDP cannot ask for a server's administrative session yet.
pub const ADMIN_SESSIONS: bool = false;

/// How signing in works, for the connection editor.
pub const SIGN_IN_HELP: &str = "The tab asks for the password each time it connects and keeps it \
    only while the session is open.\nCertificates your system does not trust are shown for you \
    to accept.";

/// The options of `options` this backend cannot honor, as the editor lists them.
pub fn unsupported(options: &RdpOptions) -> Vec<&'static str> {
    let mut missing = vec![];
    for (on, name) in [
        (options.drives, "Local drives"),
        (options.printers, "Printers"),
        (options.smart_cards, "Smart cards"),
        (options.microphone, "Microphone"),
    ] {
        if on {
            missing.push(name);
        }
    }
    match options.audio {
        Audio::Local => missing.push("Playing remote audio on this computer"),
        Audio::Remote => missing.push("Leaving audio on the remote computer"),
        Audio::Off => {}
    }
    if options.keys != Keys::Local {
        missing.push("Sending system key combinations such as Alt+Tab to the remote computer");
    }
    missing
}

pub fn forget_credentials(_host: &str) -> Result<bool, String> {
    Ok(false)
}

/// What the user typed into the sign-in prompt. The password lives only in memory.
#[derive(Clone)]
struct Credentials {
    username: String,
    password: String,
}

enum Stage {
    SignIn {
        credentials: Credentials,
        error: Option<String>,
        /// Whether the prompt still has to put the cursor where typing is needed.
        place_cursor: bool,
    },
    Trust {
        credentials: Credentials,
        certificate: Untrusted,
    },
    Running {
        worker: Worker,
        credentials: Credentials,
        connected: bool,
        reconnecting: bool,
    },
    Ended(String),
}

pub struct RdpClient {
    connection: Connection,
    ctx: egui::Context,
    stage: Stage,
    /// The tab area in physical pixels and the display scale in percent, once shown.
    area: Option<([u16; 2], u32)>,
    /// The resolution last asked of the remote computer, and when the tab took a new size.
    requested: Option<[u16; 2]>,
    resized_at: Option<Instant>,
    texture: Option<egui::TextureHandle>,
    translator: input::Translator,
    keys: ironrdp_input::Database,
    focused: bool,
    /// Take the keyboard the next time the desktop is drawn.
    focus_requested: bool,
    warning: Option<String>,
    clipboard: Option<arboard::Clipboard>,
    /// Local clipboard text already offered, text the remote side copied, and the last check.
    offered: Option<String>,
    copied: Option<String>,
    checked: Option<Instant>,
    /// Whether the tab is on screen, shared with each connection.
    shown: Arc<AtomicBool>,
}

impl RdpClient {
    pub fn new(
        _parent: Option<Window>,
        connection: &Connection,
        _admin: bool,
        ctx: egui::Context,
    ) -> Result<Self, String> {
        Ok(Self {
            connection: connection.clone(),
            ctx,
            stage: Stage::SignIn {
                credentials: Credentials {
                    username: connection.username.clone(),
                    password: String::new(),
                },
                error: None,
                place_cursor: true,
            },
            area: None,
            requested: None,
            resized_at: None,
            texture: None,
            translator: input::Translator::default(),
            keys: ironrdp_input::Database::new(),
            focused: false,
            focus_requested: false,
            warning: None,
            clipboard: None,
            offered: None,
            copied: None,
            checked: None,
            shown: Arc::new(AtomicBool::new(true)),
        })
    }

    /// Desktops are drawn by egui, so there is no native window to move.
    pub fn place(&mut self, _rect: Option<[i32; 4]>, _holes: &[[i32; 4]]) -> Result<(), String> {
        Ok(())
    }

    pub fn set_parent(&mut self, _parent: Window) {}

    /// Tells the connection whether its tab is on screen, so a hidden desktop's updates do not
    /// wake the window.
    pub fn set_shown(&self, shown: bool) {
        self.shown.store(shown, Ordering::Relaxed);
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
        let mut next = None;
        for event in worker.events() {
            match event {
                Event::Connected => (*connected, *reconnecting) = (true, false),
                Event::Reconnecting => *reconnecting = true,
                Event::Copied(text) => {
                    self.ctx.copy_text(text.clone());
                    self.copied = Some(text);
                }
                Event::FixedResolution => {
                    self.warning = Some(
                        "The remote computer cannot change its resolution, so the desktop is \
                         scaled to the tab."
                            .into(),
                    );
                }
                Event::Ended(outcome) => {
                    next = Some(match outcome {
                        Outcome::SignIn(error) => Stage::SignIn {
                            credentials: Credentials {
                                password: String::new(),
                                ..credentials.clone()
                            },
                            error: Some(error),
                            place_cursor: true,
                        },
                        Outcome::Certificate(certificate) => Stage::Trust {
                            credentials: credentials.clone(),
                            certificate,
                        },
                        Outcome::Closed(reason) => {
                            Stage::Ended(format!("The remote session ended: {reason}."))
                        }
                        Outcome::Failed(error) => Stage::Ended(error),
                    });
                }
            }
        }
        if let Some(next) = next {
            self.stage = next;
            self.texture = None;
            self.focused = false;
            self.translator.reset();
            self.keys = ironrdp_input::Database::new();
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
    pub fn can_full_screen(&self) -> bool {
        false
    }
    pub fn full_screen(&self) -> bool {
        false
    }
    pub fn enter_full_screen(&self) -> Result<(), String> {
        Err("Full screen is not available for this remote desktop.".into())
    }
    pub fn display_warning(&self) -> Option<&str> {
        self.warning.as_deref()
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

    /// Draws the sign-in prompt, certificate decision, or desktop into the rest of `ui`.
    /// Returns the area a native desktop window would cover.
    pub fn show(&mut self, ui: &mut egui::Ui) -> egui::Rect {
        let rect = ui.available_rect_before_wrap();
        let ppp = ui.ctx().pixels_per_point();
        self.area = Some((
            physical_size(rect.size(), ppp),
            ((ppp * 100.).round() as u32).max(100),
        ));
        self.poll();
        // A request made while connecting lapses, rather than taking the keyboard later.
        let focus = std::mem::take(&mut self.focus_requested);
        let mut start = None;
        match &mut self.stage {
            Stage::SignIn {
                credentials,
                error,
                place_cursor,
            } => {
                let place = std::mem::take(place_cursor);
                if sign_in(ui, &self.connection, credentials, error.as_deref(), place) {
                    start = Some(credentials.clone());
                }
            }
            Stage::Trust {
                credentials,
                certificate,
            } => match trust(ui, certificate) {
                Some(true) => {
                    connection::trust(certificate);
                    start = Some(credentials.clone());
                }
                Some(false) => {
                    self.stage = Stage::Ended(format!(
                        "The certificate of {} was not trusted.",
                        certificate.host
                    ));
                }
                None => {}
            },
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
            self.start(credentials);
        }
        rect
    }

    /// Starts connecting with `credentials` at the tab's current size.
    fn start(&mut self, credentials: Credentials) {
        let options = &self.connection.rdp;
        let (area, scale) = self.area.unwrap_or(([1280, 800], 100));
        let size = match options.display {
            Display::Fit => remote_size(area),
            Display::Fixed { width, height } => [width, height],
        };
        let (username, domain) = split_user(&credentials.username);
        let settings = Settings {
            host: self.connection.host.clone(),
            port: self.connection.port,
            gateway: (!options.gateway.is_empty()).then(|| options.gateway.clone()),
            username,
            domain,
            password: credentials.password.clone(),
            size,
            scale,
            clipboard: options.clipboard,
        };
        self.requested = Some(size);
        self.warning = None;
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
        let frame = worker.frame.clone();
        let (rect, response) = ui.allocate_exact_size(rect.size(), egui::Sense::click_and_drag());
        if focus || response.clicked() || response.drag_started() {
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
        let update = frame.lock().ok().and_then(|mut f| f.take());
        if let Some(update) = update {
            frame::upload(ui.ctx(), &mut self.texture, update);
        }
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0., egui::Color32::BLACK);
        let Some(texture) = &self.texture else {
            return;
        };
        let view = input::View::fit(rect, texture.size());
        painter.image(
            texture.id(),
            view.rect,
            egui::Rect::from_min_max(egui::pos2(0., 0.), egui::pos2(1., 1.)),
            egui::Color32::WHITE,
        );
        // The remote pointer is part of the picture.
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::None);
        }

        let mut commands = vec![];
        let (events, modifiers) = ui.input(|i| (i.events.clone(), i.modifiers));
        let pointer = response.hovered() || response.is_pointer_button_down_on();
        if focused && self.connection.rdp.clipboard {
            // A paste shortcut carries the local clipboard; the remote side needs it first.
            for event in &events {
                if let egui::Event::Paste(text) = event {
                    commands.extend(self.offer(text.clone()));
                }
            }
        }
        let ops = self
            .translator
            .translate(&events, modifiers, &view, focused, pointer);
        let mut input: Vec<_> = self.keys.apply(ops).into_vec();
        if self.focused && !focused {
            // Keys held while focus moved away would otherwise stay down remotely.
            input.extend(self.keys.release_all());
            self.translator.reset();
        }
        self.focused = focused;
        if !input.is_empty() {
            commands.push(Command::Input(input));
        }
        commands.extend(self.follow_size());
        if self.connection.rdp.clipboard && (focused || pointer) {
            commands.extend(self.check_clipboard());
        }
        if let Stage::Running { worker, .. } = &self.stage {
            for command in commands {
                worker.send(command);
            }
        }
    }

    /// Asks for a resolution that matches the tab once its size has settled.
    fn follow_size(&mut self) -> Option<Command> {
        let (Display::Fit, Some((area, scale))) = (self.connection.rdp.display, self.area) else {
            return None;
        };
        let wanted = remote_size(area);
        if self.requested == Some(wanted) {
            self.resized_at = None;
            return None;
        }
        let since = *self.resized_at.get_or_insert_with(Instant::now);
        if since.elapsed() < Duration::from_millis(400) {
            self.ctx.request_repaint_after(Duration::from_millis(100));
            return None;
        }
        self.requested = Some(wanted);
        self.resized_at = None;
        Some(Command::Resize(wanted, scale))
    }

    /// Offers local clipboard text when it has changed, checking at most once a second.
    fn check_clipboard(&mut self) -> Option<Command> {
        if self
            .checked
            .is_some_and(|at| at.elapsed() < Duration::from_secs(1))
        {
            return None;
        }
        self.checked = Some(Instant::now());
        if self.clipboard.is_none() {
            self.clipboard = arboard::Clipboard::new().ok();
        }
        let text = self.clipboard.as_mut()?.get_text().ok()?;
        self.offer(text)
    }

    /// Offers `text` unless the remote side already has it.
    fn offer(&mut self, text: String) -> Option<Command> {
        if text.is_empty()
            || self.offered.as_ref() == Some(&text)
            || self.copied.as_ref() == Some(&text)
        {
            return None;
        }
        self.offered = Some(text.clone());
        Some(Command::Offer(text))
    }
}

/// The size of `size` points in physical pixels.
fn physical_size(size: egui::Vec2, ppp: f32) -> [u16; 2] {
    [size.x, size.y].map(|v| (v * ppp).round().clamp(0., f32::from(u16::MAX)) as u16)
}

/// The remote resolution for a tab of `area` pixels: an even width, both sides within the
/// 200 to 8192 pixels display control allows.
fn remote_size(area: [u16; 2]) -> [u16; 2] {
    [area[0].clamp(200, 8192) & !1, area[1].clamp(200, 8192)]
}

/// Splits `DOMAIN\user` into its parts. `user@domain` stays whole, as Windows accepts it.
fn split_user(username: &str) -> (String, Option<String>) {
    match username.split_once('\\') {
        Some((domain, user)) if !domain.is_empty() => (user.into(), Some(domain.into())),
        _ => (username.into(), None),
    }
}

/// The sign-in prompt. Returns whether the user asked to connect.
fn sign_in(
    ui: &mut egui::Ui,
    connection: &Connection,
    credentials: &mut Credentials,
    error: Option<&str>,
    place_cursor: bool,
) -> bool {
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
        ui.label("User name");
        let user = ui.add(
            egui::TextEdit::singleline(&mut credentials.username)
                // Weak text takes the placeholder color, which the theme's text color would override.
                .hint_text(RichText::new(r"DOMAIN\user").weak())
                .desired_width(f32::INFINITY),
        );
        ui.add_space(6.);
        ui.label("Password");
        let password = ui.add(
            egui::TextEdit::singleline(&mut credentials.password)
                .password(true)
                .desired_width(f32::INFINITY),
        );
        // Enter leaves a single-line field, so read it before moving the cursor anywhere.
        let enter = (user.lost_focus() || password.lost_focus())
            && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if place_cursor {
            if credentials.username.is_empty() {
                user.request_focus();
            } else {
                password.request_focus();
            }
        }
        ui.add_space(12.);
        let ready = !credentials.username.trim().is_empty();
        connect = ui
            .add_enabled(ready, egui::Button::new("Connect"))
            .clicked()
            || (enter && ready);
        ui.add_space(8.);
        ui.label(
            RichText::new("Relay keeps the password only while this session is open.")
                .size(12.)
                .weak(),
        );
    });
    connect
}

/// The certificate decision. Returns whether the user trusted the certificate, once they
/// decide.
fn trust(ui: &mut egui::Ui, certificate: &Untrusted) -> Option<bool> {
    let mut decision = None;
    ui.add_space(40.);
    ui.vertical_centered(|ui| {
        ui.set_max_width(520.);
        ui.heading(format!(
            "{} presented an untrusted certificate",
            certificate.host
        ));
        ui.add_space(8.);
        ui.label(format!(
            "Your system could not verify it ({}). Connect only if you expect this, for \
             example because the computer uses a self-signed certificate.",
            certificate.reason
        ));
        ui.add_space(8.);
        ui.label(RichText::new("SHA-256 fingerprint").weak());
        ui.label(
            RichText::new(certificate.fingerprint_text())
                .monospace()
                .size(12.),
        );
        ui.add_space(12.);
        ui.horizontal(|ui| {
            if ui.button("Trust and connect").clicked() {
                decision = Some(true);
            }
            if ui.button("Cancel").clicked() {
                decision = Some(false);
            }
        });
        ui.label(
            RichText::new("Relay trusts it for this host until it quits.")
                .size(12.)
                .weak(),
        );
    });
    decision
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_unsupported_options_are_listed() {
        let mut options = RdpOptions {
            audio: Audio::Off,
            keys: Keys::Local,
            ..RdpOptions::default()
        };
        options.gateway = "gw.example.com".into();
        options.display = Display::Fixed {
            width: 1600,
            height: 900,
        };
        assert!(
            unsupported(&options).is_empty(),
            "clipboard, gateway and display work"
        );
        options.drives = true;
        options.smart_cards = true;
        options.audio = Audio::Local;
        options.keys = Keys::FullScreen;
        assert_eq!(
            unsupported(&options),
            [
                "Local drives",
                "Smart cards",
                "Playing remote audio on this computer",
                "Sending system key combinations such as Alt+Tab to the remote computer",
            ]
        );
    }

    #[test]
    fn tab_sizes_become_valid_resolutions() {
        assert_eq!(physical_size(egui::vec2(800.5, 600.), 1.5), [1201, 900]);
        assert_eq!(remote_size([1201, 900]), [1200, 900]);
        assert_eq!(remote_size([20, 9000]), [200, 8192]);
    }

    #[test]
    fn domains_split_from_user_names() {
        assert_eq!(
            split_user(r"CORP\alex"),
            ("alex".into(), Some("CORP".into()))
        );
        assert_eq!(
            split_user("alex@corp.example"),
            ("alex@corp.example".into(), None)
        );
        assert_eq!(split_user(r"\alex"), (r"\alex".into(), None));
    }

    #[test]
    fn clients_start_at_the_sign_in_prompt() {
        let mut c = Connection::new();
        c.host = "desk.example".into();
        c.username = "alex".into();
        let client = RdpClient::new(None, &c, false, egui::Context::default()).expect("client");
        assert!(!client.ended() && !client.reconnecting() && !client.focused());
        assert!(matches!(
            &client.stage,
            Stage::SignIn { credentials, error: None, place_cursor: true }
                if credentials.username == "alex" && credentials.password.is_empty()
        ));
    }
}
