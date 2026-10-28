//! Session panes: the header, terminal, or remote desktop of a tab, and detached windows.
use super::*;

impl App {
    pub(super) fn is_detached(&self, index: usize) -> bool {
        self.sessions
            .get(index)
            .is_some_and(|s| self.detached.contains(&s.key))
    }

    /// Moves session `index` into its own window.
    pub(super) fn detach(&mut self, index: usize) {
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
    pub(super) fn attach(&mut self, key: u64) {
        self.detached.retain(|k| *k != key);
        self.windows.retain(|(k, _)| *k != key);
        if let Some(index) = self.sessions.iter().position(|s| s.key == key) {
            self.sessions[index].set_host(None, self.parent);
            self.active = Some(index);
        }
    }

    /// Stands in for a session that lives in its own window.
    pub(super) fn detached_placeholder(&mut self, ui: &mut egui::Ui, index: usize) {
        let colors = theme::palette(ui.ctx());
        let key = self.sessions[index].key;
        ui.add_space((ui.available_height() * 0.22).clamp(16., 160.));
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new(format!(
                    "{} is open in its own window.",
                    self.sessions[index].title()
                ))
                .size(15.)
                .strong(),
            );
            ui.add_space(8.);
            ui.horizontal(|ui| {
                ui.add_space((ui.available_width() - 248.).max(0.) / 2.);
                if accent_button(ui, "Show window", 120.).clicked() {
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
    pub(super) fn detached_windows(
        &mut self,
        ctx: &egui::Context,
    ) -> Vec<(usize, [i32; 4], Vec<[i32; 4]>)> {
        let mut placed = vec![];
        for key in self.detached.clone() {
            let Some(index) = self.sessions.iter().position(|s| s.key == key) else {
                // The session was closed; its window closes with it.
                self.detached.retain(|k| *k != key);
                self.windows.retain(|(k, _)| *k != key);
                continue;
            };
            let title = format!("{} · Relay", self.sessions[index].title());
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
            }
            // Back in the tabs, by closing the window or with its button while it was drawn,
            // the session is done with the window. Looking for it again would find it before it
            // closes and move a remote desktop back in, to be destroyed along with it.
            if !self.detached.contains(&key) {
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
                        window.set_theme(theme::current(ctx));
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

    /// Shows session `index` in `ui`: its header, then its terminal, status, or remote desktop.
    /// Returns the area a remote desktop should cover.
    pub(super) fn session_pane(
        &mut self,
        ui: &mut egui::Ui,
        index: usize,
        pane: Pane,
    ) -> Option<egui::Rect> {
        let colors = theme::palette(ui.ctx());
        let mut full_screen = false;
        let mut unsplit = false;
        let mut attach = false;
        let mut save = false;
        let mut snippets = false;
        let mut companion = false;
        let s = &self.sessions[index];
        ui.horizontal(|ui| {
            ui.set_min_height(26.);
            ui.spacing_mut().button_padding = Vec2::new(10., 3.);
            let desktop = s.has_desktop();
            // Quick connections are not in the library until saved.
            let unsaved = s.connection.id == 0
                && !s.local
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
                // A terminal and its files open side by side.
                if s.browsing {
                    companion = ui
                        .button("Terminal")
                        .on_hover_text("Open a terminal on this server beside the files")
                        .clicked();
                } else if s.connection.protocol == Protocol::Ssh && s.terminal.is_some() && !s.local
                {
                    companion = ui
                        .button("Files")
                        .on_hover_text("Browse this server's files over SFTP beside the terminal")
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
                if desktop
                    && s.can_full_screen()
                    && s.is_running()
                    && s.error.is_none()
                    && !s.full_screen()
                {
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
                // The name and address take the room the buttons leave, shortened to fit.
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    let (icon, _) = ui.allocate_exact_size(Vec2::splat(16.), egui::Sense::hover());
                    icons::paint(
                        ui.painter(),
                        if s.browsing {
                            Icon::Folder
                        } else {
                            Icon::protocol(s.connection.protocol)
                        },
                        icon.center(),
                        match s.connection.protocol {
                            Protocol::Rdp => colors.rdp,
                            Protocol::Ssh => colors.accent,
                        },
                    );
                    ui.add(
                        egui::Label::new(RichText::new(s.title()).size(14.).strong()).truncate(),
                    );
                    // A local shell names the program it runs.
                    let address = if s.local {
                        crate::terminal::local_shell().0.display().to_string()
                    } else {
                        s.connection.address()
                    };
                    ui.add(
                        egui::Label::new(
                            RichText::new(&address)
                                .monospace()
                                .size(12.)
                                .color(colors.muted),
                        )
                        .truncate(),
                    )
                    .on_hover_text(&address);
                });
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
        if companion {
            self.open_companion(index);
            if pane == Pane::Beside {
                return None;
            }
        }
        if attach {
            self.attach(self.sessions[index].key);
        }
        let s = &self.sessions[index];
        ui.add_space(6.);
        if s.browsing {
            self.files_pane(ui, index, pane);
        } else if s.terminal.is_some() {
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
            } else if !s.is_running()
                && ui
                    .button(if s.local { "Start again" } else { "Reconnect" })
                    .clicked()
            {
                self.reconnect(index);
            }
            let mut look = self.terminal_look(ui.ctx());
            self.sessions[index]
                .terminal
                .as_mut()
                .unwrap()
                .show(ui, &mut look);
            self.zoomed(look.font_size);
        } else if let Some(error) = &s.error {
            let error = error.clone();
            ui.add_space(40.);
            ui.label(RichText::new(error).size(15.));
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
            let patience = Duration::from_secs(20);
            match patience.checked_sub(s.started.elapsed()) {
                // Nothing else wakes the window when the note becomes due.
                Some(left) => ui.ctx().request_repaint_after(left),
                None => {
                    ui.label("Waiting for the Windows RDP control to initialize.");
                }
            }
        } else {
            if let Some(status) = s.status() {
                ui.colored_label(ui.visuals().warn_fg_color, status);
            }
            if let Some(warning) = s.display_warning() {
                ui.colored_label(ui.visuals().warn_fg_color, warning);
            }
            return self.sessions[index].show_desktop(ui);
        }
        None
    }
}

/// Screen areas of visible floating layers: toasts, tooltips, menus and popups.
pub(super) fn overlays(ctx: &egui::Context) -> Vec<egui::Rect> {
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

/// Where a session pane is shown, which decides its header actions.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Pane {
    Main,
    /// The right side of split view.
    Beside,
    /// A detached session window.
    Window,
}

pub(super) fn session_viewport(key: u64) -> egui::ViewportId {
    egui::ViewportId::from_hash_of(("relay-session", key))
}
