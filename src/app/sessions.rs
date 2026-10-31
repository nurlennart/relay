//! Session panes: the header, terminal, or remote desktop of a tab, and detached windows.
use super::*;

impl App {
    pub(super) fn is_detached(&self, index: usize) -> bool {
        self.sessions
            .get(index)
            .is_some_and(|s| self.detached.contains(&s.key))
    }

    /// Moves session `index` into its own window. A pair it was in is unsplit, and the other
    /// side stays in front.
    pub(super) fn detach(&mut self, index: usize) {
        let Some(session) = self.sessions.get_mut(index) else {
            return;
        };
        if let Some(terminal) = &mut session.terminal {
            terminal.focus();
        }
        let key = session.key;
        let partner = self.partner(key);
        let in_front = self.active() == Some(index);
        self.detached.push(key);
        if let Some(place) = self.tab_of(key)
            && partner.is_some()
        {
            self.unsplit(place);
        }
        if in_front {
            self.activate(partner);
        }
    }

    /// Returns a detached session to the tabs and selects it.
    pub(super) fn attach(&mut self, key: u64) {
        self.detached.retain(|k| *k != key);
        self.windows.retain(|(k, _)| *k != key);
        if let Some(index) = self.sessions.iter().position(|s| s.key == key) {
            self.sessions[index].set_host(None, self.parent);
            self.activate_session(index);
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
                let (area, holes) = native_area(rect, ppp, &floating);
                placed.push((index, area, holes));
            }
        }
        placed
    }

    /// Shows the pair at `place` side by side in `ui`, the `left` and `right` sessions either
    /// side of a divider at `fraction` of the width. Adds where each remote desktop should
    /// cover to `rects`.
    pub(super) fn pair_panes(
        &mut self,
        ui: &mut egui::Ui,
        place: usize,
        mut fraction: f32,
        [left, right]: [usize; 2],
        rects: &mut Vec<(usize, egui::Rect)>,
    ) {
        let area = ui.available_rect_before_wrap();
        let divider = split_divider(ui, area, &mut fraction);
        if let Some(tabs::Tab::Pair(pair)) = self.tabs.get_mut(place) {
            pair.fraction = fraction;
        }
        let sides = [
            (
                left,
                egui::Rect::from_min_max(area.min, egui::pos2(divider.left(), area.bottom())),
                Pane::Left,
            ),
            (
                right,
                egui::Rect::from_min_max(egui::pos2(divider.right(), area.top()), area.max),
                Pane::Beside,
            ),
        ];
        // A click into a side brings it in front.
        let pressed = ui.input(|i| {
            (i.pointer.any_pressed())
                .then(|| i.pointer.interact_pos())
                .flatten()
        });
        for (index, rect, pane) in sides {
            if pressed.is_some_and(|p| rect.contains(p)) && self.active() != Some(index) {
                self.activate_session(index);
            }
            let mut pane_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(rect)
                    .layout(egui::Layout::top_down(egui::Align::LEFT)),
            );
            pane_ui.set_clip_rect(rect);
            if let Some(rect) = self.session_pane(&mut pane_ui, index, pane) {
                rects.push((index, rect));
            }
        }
        ui.allocate_rect(area, egui::Sense::hover());
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
        // A tab of its own shows its actions in the tab bar. The sides of a pair and detached
        // windows keep a header, naming each side of a pair to tell them apart.
        if pane != Pane::Main {
            let mut actions = Actions::default();
            ui.horizontal(|ui| {
                ui.set_min_height(22.);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    actions = self.session_actions(ui, index, pane);
                    // The name takes the room the buttons leave, shortened to fit.
                    if matches!(pane, Pane::Left | Pane::Beside) {
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            let s = &self.sessions[index];
                            let (icon, _) =
                                ui.allocate_exact_size(Vec2::splat(14.), egui::Sense::hover());
                            icons::paint_sized(
                                ui.painter(),
                                if s.browsing {
                                    Icon::Folder
                                } else {
                                    Icon::protocol(s.connection.protocol)
                                },
                                icon.center(),
                                14.,
                                match s.connection.protocol {
                                    Protocol::Rdp | Protocol::Vnc => colors.rdp,
                                    Protocol::Ssh => colors.accent,
                                },
                            );
                            let title = RichText::new(s.title()).size(12.).color(colors.muted);
                            ui.add(egui::Label::new(title).truncate())
                                .on_hover_ui(|ui| {
                                    ui.label(tabs::details(
                                        &s.connection,
                                        tabs::Kind::of(s),
                                        s.tab_state(),
                                    ));
                                });
                        });
                    }
                });
            });
            ui.add_space(4.);
            // The panes may have changed; they are laid out again next frame.
            if self.apply_actions(index, actions) {
                return None;
            }
        }
        let s = &self.sessions[index];
        if s.browsing {
            self.files_pane(ui, index);
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

/// What a session's action buttons asked for this frame.
#[derive(Default)]
pub(super) struct Actions {
    full_screen: bool,
    unsplit: bool,
    attach: bool,
    save: bool,
    snippets: bool,
    companion: bool,
}

impl App {
    /// Draws session `index`'s action buttons from right to left in `ui`: a pane's header, or
    /// the tab bar for a tab of its own.
    pub(super) fn session_actions(&self, ui: &mut egui::Ui, index: usize, pane: Pane) -> Actions {
        let mut actions = Actions::default();
        let s = &self.sessions[index];
        ui.spacing_mut().button_padding = Vec2::new(8., 1.);
        ui.spacing_mut().item_spacing.x = 6.;
        match pane {
            Pane::Beside => {
                actions.unsplit = ui
                    .button("Unsplit")
                    .on_hover_text("Show the two sessions as separate tabs again (Ctrl+\\)")
                    .clicked();
            }
            Pane::Window => {
                actions.attach = ui
                    .button("Back to tabs")
                    .on_hover_text("Closing this window does the same")
                    .clicked();
            }
            Pane::Main | Pane::Left => {}
        }
        // Quick connections are not in the library until saved.
        let unsaved = s.connection.id == 0
            && !s.local
            && !self
                .connections
                .iter()
                .any(|c| c.same_target(&s.connection));
        if unsaved {
            actions.save = ui
                .button("Save to library")
                .on_hover_text("Keep this connection in the library")
                .clicked();
        }
        let terminal = s.connection.protocol == Protocol::Ssh && s.terminal.is_some();
        // A terminal and its files open side by side; a local shell has no server to browse.
        let shell = terminal && !s.local;
        if s.browsing {
            actions.companion = ui
                .button("Terminal")
                .on_hover_text("Open a terminal on this server beside the files")
                .clicked();
        } else if shell {
            actions.companion = ui
                .button("Files")
                .on_hover_text("Browse this server's files over SFTP beside the terminal")
                .clicked();
        }
        // The snippet palette opens over the main window's active tab.
        if terminal && s.is_running() && pane != Pane::Window {
            actions.snippets = ui
                .button("Snippets")
                .on_hover_text("Saved commands · Ctrl+Shift+P")
                .clicked();
        }
        // A desktop drawn by Relay fills the main window, not a detached one.
        if s.has_desktop()
            && s.can_full_screen()
            && s.is_running()
            && s.error.is_none()
            && !s.full_screen()
            && !(s.full_screen_in_window() && pane == Pane::Window)
        {
            actions.full_screen = ui
                .button("Full screen")
                .on_hover_text(s.full_screen_help())
                .clicked();
        }
        actions
    }

    /// Carries out what session `index`'s buttons asked. Returns whether the panes changed, so
    /// they are laid out again next frame.
    pub(super) fn apply_actions(&mut self, index: usize, actions: Actions) -> bool {
        if actions.full_screen {
            self.enter_full_screen(index);
        }
        if actions.save {
            self.edit(self.sessions[index].connection.clone());
        }
        if actions.snippets {
            self.palette = Some(Palette::default());
        }
        if actions.unsplit
            && let Some(place) = self.tab_of(self.sessions[index].key)
        {
            self.unsplit(place);
        }
        if actions.companion {
            self.open_companion(index);
            return true;
        }
        if actions.attach {
            self.attach(self.sessions[index].key);
        }
        false
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

/// `rect` in physical pixels, for a native remote desktop window, and the parts of `floating`
/// over it, relative to its corner, to cut out of it.
pub(super) fn native_area(
    rect: egui::Rect,
    ppp: f32,
    floating: &[egui::Rect],
) -> ([i32; 4], Vec<[i32; 4]>) {
    let pixels = |r: egui::Rect| {
        [
            (r.min.x * ppp).round() as i32,
            (r.min.y * ppp).round() as i32,
            (r.width() * ppp).round() as i32,
            (r.height() * ppp).round() as i32,
        ]
    };
    let [sx, sy, ..] = pixels(rect);
    let holes = (floating.iter())
        .map(|r| r.intersect(rect))
        .filter(|r| r.is_positive())
        .map(|r| {
            let [x, y, w, h] = pixels(r);
            [x - sx, y - sy, w, h]
        })
        .collect();
    (pixels(rect), holes)
}

/// Where a session pane is shown, which decides its header actions.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Pane {
    /// A tab of its own.
    Main,
    /// The left side of a pair.
    Left,
    /// The right side of a pair.
    Beside,
    /// A detached session window.
    Window,
}

pub(super) fn session_viewport(key: u64) -> egui::ViewportId {
    egui::ViewportId::from_hash_of(("relay-session", key))
}
