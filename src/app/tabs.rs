//! The tab strip above the workspace.
use super::*;
use egui::{Margin, Rect, Sense};

impl App {
    pub(super) fn tabs(&mut self, ctx: &egui::Context) {
        let colors = theme::palette(ctx);
        egui::TopBottomPanel::top("tabs")
            .exact_height(BAR_HEIGHT)
            .frame(
                egui::Frame::new()
                    .fill(colors.panel)
                    .inner_margin(Margin::symmetric(8, 6)),
            )
            .show(ctx, |ui| {
                // The tab in front shows its session's actions at the right, where they stay
                // put; the tabs shrink or scroll in the room left of them.
                let bar = ui.max_rect();
                let mut strip = bar;
                if let Some(index) = self.bar_session() {
                    let mut right = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(bar)
                            .layout(egui::Layout::right_to_left(egui::Align::Center)),
                    );
                    let actions = self.session_actions(&mut right, index, Pane::Main);
                    if right.min_rect().width() > 0. {
                        strip.max.x = right.min_rect().left() - 12.;
                    }
                    self.apply_actions(index, actions);
                }
                let mut ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(strip)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );
                let ui = &mut ui;
                egui::ScrollArea::horizontal()
                    .id_salt("tabs-scroll")
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                    .show(ui, |ui| {
                        ui.horizontal_centered(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.;
                            let library = library_tab(ui, self.front.is_none())
                                .on_hover_text("Connection library (Ctrl+1)");
                            if library.clicked() {
                                self.activate(None);
                            }
                            // Tabs can be dragged; the drop position follows the pointer.
                            // A pair moves as one.
                            let active_tab = self.active_tab();
                            let active = self.active();
                            let mut activate = None;
                            let mut rects = vec![];
                            let mut beside = None;
                            let mut unsplit = None;
                            let mut swap = None;
                            let mut window_action = None;
                            let mut dragging = None;
                            let mut dropped = None;
                            let mut close = None;
                            let mut reconnect = None;
                            for (place, tab) in self.tabs.clone().into_iter().enumerate() {
                                let joined = matches!(tab, Tab::Pair(_));
                                let selected = active_tab == Some(place);
                                ui.push_id(place, |ui| {
                                    // The pair's background goes under its sides once they are laid out.
                                    let background = ui.painter().add(egui::Shape::Noop);
                                    if joined {
                                        ui.spacing_mut().item_spacing.x = 0.;
                                    }
                                    let mut sides: Vec<Rect> = vec![];
                                    for key in tab.keys() {
                                        let Some(index) = self.session_index(key) else {
                                            continue;
                                        };
                                        let s = &self.sessions[index];
                                        let look = if joined {
                                            Look::Side {
                                                selected,
                                                focused: active == Some(index),
                                            }
                                        } else {
                                            Look::Tab { selected }
                                        };
                                        let session = ui.push_id(key, |ui| {
                                            session_tab(
                                                ui,
                                                &s.connection,
                                                Kind::of(s),
                                                s.tab_state(),
                                                look,
                                            )
                                        });
                                        let session = session.inner;
                                        sides.push(session.response.rect);
                                        if session.response.dragged() {
                                            dragging = Some(place);
                                        }
                                        if session.response.drag_stopped() {
                                            dropped = Some(place);
                                        }
                                        let detached = self.detached.contains(&key);
                                        if session.inner.0.clicked() && detached {
                                            // A detached session's tab brings its window forward.
                                            ui.ctx().send_viewport_cmd_to(
                                                session_viewport(key),
                                                egui::ViewportCommand::Focus,
                                            );
                                        } else if session.inner.0.clicked() {
                                            // A side of a pair takes the keyboard; it never swaps.
                                            activate = Some(key);
                                        }
                                        // The close button, or a middle click anywhere on the tab.
                                        if session.inner.1.clicked()
                                            || session.inner.0.middle_clicked()
                                            || session.inner.1.middle_clicked()
                                        {
                                            close = Some(index);
                                        }
                                        let menu = egui::Popup::context_menu(&session.inner.0);
                                        menu_popup(menu).show(|ui| {
                                            ui.set_min_width(MENU_WIDTH);
                                            // A session can look open after its network went
                                            // away, so reconnecting is offered whenever it has
                                            // connected.
                                            if s.tab_state() != session::TabState::Waiting
                                                && ui.button("Reconnect").clicked()
                                            {
                                                reconnect = Some(index);
                                                ui.close();
                                            }
                                            if joined {
                                                if ui.button("Swap sides").clicked() {
                                                    swap = Some(place);
                                                    ui.close();
                                                }
                                                if ui.button("Unsplit").clicked() {
                                                    unsplit = Some(place);
                                                    ui.close();
                                                }
                                            } else if !detached
                                                && active_tab.is_some_and(|a| {
                                                    a != place
                                                        && matches!(
                                                            self.tabs[a],
                                                            Tab::Single(k)
                                                                if !self.detached.contains(&k)
                                                        )
                                                })
                                                && ui.button("Open beside current tab").clicked()
                                            {
                                                beside = Some(index);
                                                ui.close();
                                            }
                                            let window = if detached {
                                                "Back to tabs"
                                            } else {
                                                "Move to new window"
                                            };
                                            if ui.button(window).clicked() {
                                                window_action = Some((index, detached));
                                                ui.close();
                                            }
                                            let close_label =
                                                if joined { "Close session" } else { "Close tab" };
                                            if ui.button(close_label).clicked() {
                                                close = Some(index);
                                                ui.close();
                                            }
                                        });
                                    }
                                    let Some(rect) = sides.iter().copied().reduce(Rect::union)
                                    else {
                                        return;
                                    };
                                    if joined {
                                        let colors = theme::palette(ui.ctx());
                                        let fill = if selected {
                                            colors.surface
                                        } else {
                                            egui::Color32::TRANSPARENT
                                        };
                                        ui.painter().set(
                                            background,
                                            egui::epaint::RectShape::new(
                                                rect,
                                                RADIUS,
                                                fill,
                                                Stroke::new(1_f32, colors.line),
                                                egui::StrokeKind::Inside,
                                            ),
                                        );
                                        if let [left, _] = sides[..] {
                                            ui.painter().vline(
                                                left.right(),
                                                left.y_range().shrink(7.),
                                                Stroke::new(1_f32, colors.line),
                                            );
                                        }
                                    }
                                    rects.push(rect);
                                });
                            }
                            if let Some(key) = activate {
                                self.activate(Some(key));
                            }
                            if let Some(index) = beside {
                                self.open_beside(index);
                            }
                            if let Some(place) = swap {
                                self.swap_sides(place);
                            }
                            if let Some(place) = unsplit {
                                self.unsplit(place);
                            }
                            if let Some(index) = reconnect {
                                self.reconnect(index);
                            }
                            if let Some(index) = close {
                                self.request_close(index);
                            }
                            match window_action {
                                Some((index, false)) => self.detach(index),
                                Some((index, true)) => self.attach(self.sessions[index].key),
                                None => {}
                            }
                            let pointer = ui.ctx().pointer_interact_pos().map(|p| p.x);
                            if let (Some(from), Some(x)) = (dragging.or(dropped), pointer)
                                && from < rects.len()
                            {
                                let to = drop_index(&rects, from, x);
                                if dropped.is_some() {
                                    self.move_tab(from, to);
                                } else if to != from {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                                    let others: Vec<_> = rects
                                        .iter()
                                        .enumerate()
                                        .filter(|(i, _)| *i != from)
                                        .map(|(_, r)| *r)
                                        .collect();
                                    let x = match to {
                                        0 => others[0].left() - 4.,
                                        n => others[n - 1].right() + 4.,
                                    };
                                    let y = rects[from].y_range();
                                    ui.painter().vline(x, y, Stroke::new(2_f32, colors.accent));
                                }
                            }
                            #[cfg(feature = "ui-check")]
                            if std::env::var_os("RELAY_UI_TABS").is_some() {
                                for (i, c) in self.connections.iter().take(2).enumerate() {
                                    let state = if i == 0 {
                                        session::TabState::Open
                                    } else {
                                        session::TabState::Ended
                                    };
                                    session_tab(
                                        ui,
                                        c,
                                        Kind::Session,
                                        state,
                                        Look::Tab { selected: false },
                                    );
                                }
                            }
                            if icon_button(ui, Icon::Plus, 26., "Open a connection (Ctrl+T)")
                                .clicked()
                            {
                                self.open_quick();
                            }
                            if icon_button(
                                ui,
                                Icon::Terminal,
                                26.,
                                &format!("Local shell on this computer ({LOCAL_SHELL_KEYS})"),
                            )
                            .clicked()
                            {
                                self.open_local_shell();
                            }
                        });
                    });
            });
    }

    /// The session whose actions the tab bar shows: the tab in front, when it is a single
    /// session in this window. A pair's sides and detached windows have their own headers.
    fn bar_session(&self) -> Option<usize> {
        match self.tabs.get(self.active_tab()?)? {
            Tab::Single(key) if !self.detached.contains(key) => self.session_index(*key),
            _ => None,
        }
    }
}

/// The fill behind a tab: raised when selected, faint while hovered.
fn tab_background(ui: &egui::Ui, rect: Rect, selected: bool, hovered: bool) {
    let colors = theme::palette(ui.ctx());
    if selected {
        ui.painter().rect(
            rect,
            RADIUS,
            colors.surface,
            Stroke::new(1_f32, colors.line),
            egui::StrokeKind::Inside,
        );
    } else if hovered {
        ui.painter()
            .rect_filled(rect, RADIUS, colors.hover.gamma_multiply(0.6));
    }
}

/// The widest a session tab's name gets before it is shortened.
const TAB_LABEL_MAX: f32 = 180.;
/// The same for each side of a pair, which shares one tab.
const SIDE_LABEL_MAX: f32 = 115.;

/// How a session's tab is drawn.
#[derive(Clone, Copy)]
pub(super) enum Look {
    /// A tab of its own, raised when selected.
    Tab { selected: bool },
    /// One side of a pair, on the pair's background: bright while it has the keyboard in the
    /// selected tab.
    Side { selected: bool, focused: bool },
}

/// The first tab, which shows the connection library.
pub(super) fn library_tab(ui: &mut egui::Ui, selected: bool) -> egui::Response {
    let colors = theme::palette(ui.ctx());
    let text = if selected { colors.text } else { colors.muted };
    let galley = egui::WidgetText::from(RichText::new("Library").size(13.)).into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        egui::TextStyle::Body,
    );
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(galley.size().x + 40., 28.), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, true, selected, "Library")
    });
    tab_background(ui, rect, selected, response.hovered());
    icons::paint_sized(
        ui.painter(),
        Icon::Library,
        egui::pos2(rect.left() + 16., rect.center().y),
        ICON_SIZE,
        if selected {
            colors.accent
        } else {
            colors.muted
        },
    );
    ui.painter().galley(
        egui::pos2(rect.left() + 30., rect.center().y - galley.size().y / 2.),
        galley,
        text,
    );
    response
}

/// What a tab holds, beyond its connection's protocol.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Kind {
    Session,
    Files,
    Admin,
    Local,
}

impl Kind {
    pub(super) fn of(s: &Session) -> Self {
        if s.local {
            Self::Local
        } else if s.browsing {
            Self::Files
        } else if s.admin {
            Self::Admin
        } else {
            Self::Session
        }
    }
}

pub(super) const LOCAL_SHELL_KEYS: &str = if cfg!(target_os = "macos") {
    "Cmd+Shift+T"
} else {
    "Ctrl+Shift+T"
};

pub(super) fn session_tab(
    ui: &mut egui::Ui,
    connection: &Connection,
    kind: Kind,
    state: session::TabState,
    look: Look,
) -> egui::InnerResponse<(egui::Response, egui::Response)> {
    let colors = theme::palette(ui.ctx());
    let (selected, label_max) = match look {
        Look::Tab { selected } => (selected, TAB_LABEL_MAX),
        Look::Side { selected, focused } => (selected && focused, SIDE_LABEL_MAX),
    };
    let text = if selected { colors.text } else { colors.muted };
    let (browsing, admin) = (kind == Kind::Files, kind == Kind::Admin);
    let label = if admin {
        format!("{} (admin)", connection.name)
    } else {
        connection.name.clone()
    };
    let label = label.as_str();
    let galley = egui::WidgetText::from(RichText::new(label).size(13.)).into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        label_max,
        egui::TextStyle::Body,
    );
    // Protocol icon, name, then the close button.
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(galley.size().x + 62., 28.), Sense::drag());
    let hovered = ui.rect_contains_pointer(rect);
    match look {
        Look::Tab { selected } => tab_background(ui, rect, selected, hovered),
        // The pair draws the background; a side only lights up under the pointer.
        Look::Side { selected, .. } => tab_background(ui, rect, false, hovered && !selected),
    }
    // The protocol's icon carries the session state in its color.
    let status = match state {
        session::TabState::Open => colors.accent,
        session::TabState::Ended => ui.visuals().error_fg_color,
        session::TabState::Waiting => colors.muted,
    };
    icons::paint_sized(
        ui.painter(),
        if browsing {
            Icon::Folder
        } else {
            Icon::protocol(connection.protocol)
        },
        egui::pos2(rect.left() + 16., rect.center().y),
        ICON_SIZE,
        status,
    );
    let select_rect = Rect::from_min_max(rect.min, rect.max - Vec2::new(30., 0.));
    let close_rect = Rect::from_center_size(
        egui::pos2(rect.right() - 16., rect.center().y),
        Vec2::splat(20.),
    );
    let select = ui
        .interact(select_rect, response.id.with("select"), Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_ui(|ui| {
            ui.label(details(connection, kind, state));
        });
    select.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, true, selected, label)
    });
    ui.painter().galley(
        egui::pos2(rect.left() + 30., rect.center().y - galley.size().y / 2.),
        galley,
        text,
    );
    let close = ui.interact(close_rect, response.id.with("close"), Sense::click());
    close.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Close {label}"))
    });
    // The close button shows on the selected tab and under the pointer, as in browsers.
    if selected || hovered {
        if close.hovered() {
            ui.painter()
                .rect_filled(close_rect, RADIUS_SMALL, colors.hover);
        }
        icons::paint_sized(
            ui.painter(),
            Icon::Close,
            close_rect.center(),
            13.,
            if close.hovered() {
                colors.text
            } else {
                colors.muted
            },
        );
    }
    let close = close.on_hover_text(format!("Close {label} (Ctrl+W)"));
    egui::InnerResponse {
        inner: (select, close),
        response,
    }
}

/// What a session tab's tooltip says: what it is, where it connects, and its state. Session
/// panes have no header of their own, so this is where the address shows.
pub(super) fn details(connection: &Connection, kind: Kind, state: session::TabState) -> String {
    let state = match state {
        session::TabState::Open => "open",
        session::TabState::Ended => "ended",
        session::TabState::Waiting => "connects when selected",
    };
    if kind == Kind::Local {
        // A local shell names the program it runs.
        let program = crate::terminal::local_shell().0;
        return format!("Local shell · {} · {state}", program.display());
    }
    let what = match kind {
        Kind::Files => "SFTP",
        Kind::Admin => "RDP admin session",
        _ => connection.protocol.label(),
    };
    let user = match connection.username.as_str() {
        "" => String::new(),
        user => format!("{user}@"),
    };
    format!("{what} · {user}{} · {state}", connection.address())
}

/// The position a dragged tab takes when dropped at `x`: the number of other tabs left of it.
pub(super) fn drop_index(rects: &[Rect], from: usize, x: f32) -> usize {
    rects
        .iter()
        .enumerate()
        .filter(|(i, r)| *i != from && r.center().x < x)
        .count()
}

/// A tab in the strip: one session, or two shown side by side, by session key.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Tab {
    Single(u64),
    Pair(Pair),
}

/// Two sessions shown side by side in one tab.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Pair {
    pub left: u64,
    pub right: u64,
    /// Where the divider sits, as a fraction of the width.
    pub fraction: f32,
    /// The side that had the keyboard last, which gets it again when the tab is selected.
    pub focus: u64,
}

impl Tab {
    /// The sessions the tab shows, left to right.
    pub(super) fn keys(&self) -> impl Iterator<Item = u64> {
        match *self {
            Self::Single(key) => [Some(key), None],
            Self::Pair(pair) => [Some(pair.left), Some(pair.right)],
        }
        .into_iter()
        .flatten()
    }

    pub(super) fn holds(&self, key: u64) -> bool {
        self.keys().any(|k| k == key)
    }

    /// The session that takes the keyboard when the tab is selected.
    pub(super) fn focus(&self) -> u64 {
        match *self {
            Self::Single(key) => key,
            Self::Pair(pair) => pair.focus,
        }
    }
}

impl App {
    /// Keeps the strip in step with the sessions, after they are opened, closed or moved to
    /// windows. A closed session leaves its tab, the other side of its pair staying as a tab of
    /// its own; a pair never holds a session in its own window; and a session without a tab
    /// gets one at the end. Checking costs no allocation, so it is cheap to do every frame.
    pub(super) fn sync_tabs(&mut self) {
        let open = |key| self.sessions.iter().any(|s| s.key == key);
        let kept = |key| open(key) && !self.detached.contains(&key);
        let held: usize = self.tabs.iter().map(|t| t.keys().count()).sum();
        let in_step = held == self.sessions.len()
            && self.sessions.iter().all(|s| self.tab_of(s.key).is_some())
            && self.tabs.iter().all(|t| match *t {
                Tab::Single(key) => open(key),
                Tab::Pair(p) => kept(p.left) && kept(p.right),
            });
        if in_step {
            return;
        }
        let open: Vec<u64> = self.sessions.iter().map(|s| s.key).collect();
        let kept = |key| open.contains(&key) && !self.detached.contains(&key);
        self.tabs = std::mem::take(&mut self.tabs)
            .into_iter()
            .flat_map(|tab| match tab {
                Tab::Pair(p) if !kept(p.left) || !kept(p.right) => {
                    vec![Tab::Single(p.left), Tab::Single(p.right)]
                }
                tab => vec![tab],
            })
            .collect();
        let mut seen = vec![];
        self.tabs.retain(|tab| {
            let keep = tab.keys().all(|k| open.contains(&k) && !seen.contains(&k));
            seen.extend(tab.keys());
            keep
        });
        for key in open {
            if !self.tabs.iter().any(|t| t.holds(key)) {
                self.tabs.push(Tab::Single(key));
            }
        }
    }

    /// Shows `key`'s tab, or the library for `None`, with that session in front, and gives
    /// it the keyboard. Every way of choosing a tab or a side comes through here: a click, a
    /// shortcut, restoring the workspace, or the tab in front closing.
    pub(super) fn activate(&mut self, key: Option<u64>) {
        if !self.bring_forward(key) {
            return;
        }
        self.chosen = true;
        if let Some(index) = key.and_then(|k| self.session_index(k)) {
            self.sessions[index].focus();
        }
    }

    /// Shows session `index`'s tab with it in front, as `activate` does.
    pub(super) fn activate_session(&mut self, index: usize) {
        if let Some(key) = self.sessions.get(index).map(|s| s.key) {
            self.activate(Some(key));
        }
    }

    /// Brings `key`'s tab in front with that side of a pair as its focus, or the library for
    /// `None`, and notes the tab left behind to return to. The library is passed through, not
    /// returned to. Returns false, changing nothing, for a session without a tab.
    pub(super) fn bring_forward(&mut self, key: Option<u64>) -> bool {
        self.sync_tabs();
        let Some(key) = key else {
            self.front = None;
            return true;
        };
        let Some(place) = self.tab_of(key) else {
            return false;
        };
        if let Tab::Pair(pair) = &mut self.tabs[place] {
            pair.focus = key;
        }
        let (last, before) = self.tab_trail;
        self.tab_trail = if last.and_then(|k| self.tab_of(k)) == Some(place) {
            (Some(key), before)
        } else {
            (Some(key), last)
        };
        self.front = Some(key);
        true
    }

    /// The session in front: the tab in front's, or the side of its pair with the keyboard.
    pub(super) fn active(&self) -> Option<usize> {
        let place = self.active_tab()?;
        self.session_index(self.tabs[place].focus())
    }

    pub(super) fn session_index(&self, key: u64) -> Option<usize> {
        self.sessions.iter().position(|s| s.key == key)
    }

    /// The position in the strip of the tab showing session `key`.
    pub(super) fn tab_of(&self, key: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.holds(key))
    }

    /// The position in the strip of the tab in front; `None` while the library shows.
    pub(super) fn active_tab(&self) -> Option<usize> {
        self.tab_of(self.front?)
    }

    /// The pair in front, if the tab in front is one.
    pub(super) fn active_pair(&self) -> Option<(usize, Pair)> {
        let place = self.active_tab()?;
        match self.tabs[place] {
            Tab::Pair(pair) => Some((place, pair)),
            Tab::Single(_) => None,
        }
    }

    /// The session shown beside session `key`, if it is in a pair.
    pub(super) fn partner(&self, key: u64) -> Option<u64> {
        self.tabs.iter().find_map(|t| match *t {
            Tab::Pair(p) if p.left == key => Some(p.right),
            Tab::Pair(p) if p.right == key => Some(p.left),
            _ => None,
        })
    }

    /// The sessions on screen in the main window, left to right.
    pub(super) fn on_screen(&self) -> Vec<usize> {
        let Some(place) = self.active_tab() else {
            return vec![];
        };
        (self.tabs[place].keys())
            .filter(|k| !self.detached.contains(k))
            .filter_map(|k| self.session_index(k))
            .collect()
    }

    /// The sessions in strip order, left to right, as they are saved.
    pub(super) fn strip_order(&self) -> Vec<usize> {
        (self.tabs.iter())
            .flat_map(Tab::keys)
            .filter_map(|k| self.session_index(k))
            .collect()
    }

    /// Shows sessions `left` and `right` side by side, in the place of `focus`'s tab, which
    /// is one of them. Both must be tabs of their own, in the main window. Returns the pair's
    /// place.
    pub(super) fn join(&mut self, left: u64, right: u64, focus: u64) -> Option<usize> {
        self.sync_tabs();
        let free = |key| self.tabs.contains(&Tab::Single(key)) && !self.detached.contains(&key);
        if left == right || !free(left) || !free(right) || ![left, right].contains(&focus) {
            return None;
        }
        let other = if focus == left { right } else { left };
        self.tabs.retain(|t| *t != Tab::Single(other));
        let place = self.tab_of(focus)?;
        self.tabs[place] = Tab::Pair(Pair {
            left,
            right,
            fraction: SPLIT_MIDDLE,
            focus,
        });
        Some(place)
    }

    /// Shows session `index` beside the session in front, which keeps the keyboard.
    pub(super) fn open_beside(&mut self, index: usize) {
        let (Some(active), Some(other)) = (
            self.active().map(|i| self.sessions[i].key),
            self.sessions.get(index).map(|s| s.key),
        ) else {
            return;
        };
        if self.join(active, other, active).is_some() {
            self.remember_tabs();
        }
    }

    /// Splits the tab in front with its neighbour, the next one or else the one before, or
    /// unsplits it when it is a pair.
    pub(super) fn toggle_split(&mut self) {
        let Some(place) = self.active_tab() else {
            return;
        };
        let active = match self.tabs[place] {
            Tab::Pair(_) => return self.unsplit(place),
            Tab::Single(key) if self.detached.contains(&key) => return,
            Tab::Single(key) => key,
        };
        let free = |tab: Option<&Tab>| match tab {
            Some(&Tab::Single(key)) if !self.detached.contains(&key) => Some(key),
            _ => None,
        };
        let next = free(self.tabs.get(place + 1));
        let previous = free(place.checked_sub(1).and_then(|i| self.tabs.get(i)));
        let joined = match (next, previous) {
            (Some(next), _) => self.join(active, next, active),
            (None, Some(previous)) => self.join(previous, active, active),
            (None, None) => None,
        };
        if joined.is_some() {
            self.remember_tabs();
        } else {
            self.notify("Open a second session to use split view.");
        }
    }

    /// Turns the pair at `place` back into two tabs, left then right.
    pub(super) fn unsplit(&mut self, place: usize) {
        if let Some(&Tab::Pair(pair)) = self.tabs.get(place) {
            self.tabs[place] = Tab::Single(pair.left);
            self.tabs.insert(place + 1, Tab::Single(pair.right));
            self.remember_tabs();
        }
    }

    /// Swaps the sides of the pair at `place`, each keeping its width.
    pub(super) fn swap_sides(&mut self, place: usize) {
        if let Some(Tab::Pair(pair)) = self.tabs.get_mut(place) {
            std::mem::swap(&mut pair.left, &mut pair.right);
            pair.fraction = 1. - pair.fraction;
            self.remember_tabs();
        }
    }

    /// Moves the tab at `from` to position `to`.
    pub(super) fn move_tab(&mut self, from: usize, to: usize) {
        if from >= self.tabs.len() || to >= self.tabs.len() || from == to {
            return;
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        self.remember_tabs();
    }

    /// Selects tab `index`, where 0 is the connection library and 1.. are the tabs after it.
    /// A pair returns to the side that had the keyboard last.
    pub(super) fn select_tab(&mut self, index: usize) {
        let tab = index.checked_sub(1).and_then(|i| self.tabs.get(i));
        self.activate(tab.map(Tab::focus));
    }
}
