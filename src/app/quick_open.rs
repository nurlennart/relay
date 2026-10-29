//! Quick open: a search over the library from the tab bar's +, which also connects to any host
//! typed as quick connect understands it, without leaving the tab in front.
use super::*;
use egui::{Align, Align2, Layout, Sense, UiBuilder};

/// How many connections quick open lists at once.
const ROWS: usize = 8;

/// What a row of quick open opens.
#[derive(Clone, Copy, PartialEq)]
enum Choice {
    /// The library connection at this position.
    Saved(usize),
    /// The typed host, through quick connect.
    Typed,
}

impl App {
    /// Opens quick open, keeping what was typed if it is open already.
    pub(super) fn open_quick(&mut self) {
        self.quick_open.get_or_insert_with(Palette::default);
    }

    /// Gives the keyboard back to the tab in front after quick open closes.
    fn refocus_session(&mut self) {
        if let Some(session) = self.active().and_then(|i| self.sessions.get_mut(i)) {
            if let Some(terminal) = &mut session.terminal {
                terminal.focus();
            }
            if let Some(files) = &mut session.files {
                files.view.focus_requested = true;
            }
        }
    }

    /// The library connections quick open lists for `query`: recently opened ones first, then
    /// by name.
    pub(super) fn quick_matches(&self, query: &str) -> Vec<usize> {
        let query = query.trim().to_lowercase();
        let recent = self.state.recent(&self.location);
        let mut found: Vec<usize> = (0..self.connections.len())
            .filter(|&i| self.connections[i].matches(&query))
            .collect();
        found.sort_by_cached_key(|&i| {
            let c = &self.connections[i];
            let rank = recent.iter().position(|id| *id == c.id);
            (rank.is_none(), rank, c.name.to_lowercase())
        });
        found.truncate(ROWS);
        found
    }

    pub(super) fn quick_open(&mut self, ctx: &egui::Context) {
        let Some(mut open) = self.quick_open.take() else {
            return;
        };
        let colors = theme::palette(ctx);
        // Keys first, so neither the search field nor the views behind see them.
        let (down, up, enter, escape) = ctx.input_mut(|i| {
            // Enter with any modifier is quick open's, never the list or tab behind it. Its
            // modifiers choose what it does.
            let enter = i.events.iter().find_map(|e| match e {
                egui::Event::Key {
                    key: egui::Key::Enter,
                    pressed: true,
                    modifiers,
                    ..
                } => Some(*modifiers),
                _ => None,
            });
            i.events.retain(|e| {
                !matches!(
                    e,
                    egui::Event::Key {
                        key: egui::Key::Enter,
                        ..
                    }
                )
            });
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                enter,
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            )
        });
        let modifiers = enter.unwrap_or_default();
        let mut typed = None;
        let mut browses = false;
        let mut chosen = None;
        let width = dialog_width(ctx, 540.);
        let search_id = egui::Id::new("quick-open-search");
        let area = egui::Area::new(egui::Id::new("quick-open"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_TOP, Vec2::new(0., BAR_HEIGHT + 24.))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(colors.panel)
                    .stroke(Stroke::new(1_f32, colors.line))
                    .corner_radius(RADIUS_DIALOG)
                    .inner_margin(12.)
                    .shadow(ui.visuals().window_shadow)
                    .show(ui, |ui| {
                        ui.set_width(width);
                        let search = search_field(
                            ui,
                            &mut open.query,
                            "Search the library, or type a host or ssh://…",
                            "",
                            search_id,
                            width,
                        );
                        if !open.focused {
                            search.request_focus();
                            open.focused = true;
                        }
                        // Results come from the query as this frame's typing or paste left it,
                        // so Enter in the same frame acts on what is shown.
                        typed = Connection::quick(&open.query);
                        browses = model::browses(&open.query);
                        let mut rows: Vec<Choice> = (self.quick_matches(&open.query))
                            .into_iter()
                            .map(Choice::Saved)
                            .collect();
                        if typed.is_some() {
                            rows.push(Choice::Typed);
                        }
                        let last = rows.len().saturating_sub(1);
                        open.selected = if down {
                            (open.selected + 1).min(last)
                        } else if up {
                            open.selected.saturating_sub(1)
                        } else {
                            open.selected.min(last)
                        };
                        // Alt+Enter connects to what was typed even when something in the
                        // library matches, as in the library's search.
                        chosen = match enter {
                            Some(m) if m.alt => typed.as_ref().map(|_| Choice::Typed),
                            Some(_) => rows.get(open.selected).copied(),
                            None => None,
                        };
                        // The pointer selects a row only when it moves, so one resting over
                        // the list does not undo the arrow keys.
                        let moved = ui.input(|i| i.pointer.delta() != Vec2::ZERO);
                        if rows.is_empty() {
                            let text = if self.connections.is_empty() {
                                "Type a host to connect, or add connections in the library."
                            } else {
                                "Nothing in the library matches."
                            };
                            ui.label(RichText::new(text).color(colors.muted));
                        }
                        ui.spacing_mut().item_spacing.y = 2.;
                        for (row, &choice) in rows.iter().enumerate() {
                            let (rect, response) =
                                ui.allocate_exact_size(Vec2::new(width, 32.), Sense::click());
                            if response.hovered() && moved {
                                open.selected = row;
                            }
                            if response.clicked() {
                                chosen = Some(choice);
                            }
                            if row == open.selected {
                                ui.painter().rect_filled(rect, RADIUS, colors.selected);
                            }
                            let (c, title) = match choice {
                                Choice::Saved(i) => {
                                    let c = &self.connections[i];
                                    (c, c.name.clone())
                                }
                                Choice::Typed => {
                                    let action = if browses {
                                        "Browse files on"
                                    } else {
                                        "Connect to"
                                    };
                                    (typed.as_ref().unwrap(), action.into())
                                }
                            };
                            let address = if c.username.is_empty() {
                                c.address()
                            } else {
                                format!("{}@{}", c.username, c.address())
                            };
                            let mut line = ui.new_child(
                                UiBuilder::new()
                                    .max_rect(rect.shrink2(Vec2::new(10., 0.)))
                                    .layout(Layout::left_to_right(Align::Center)),
                            );
                            protocol_badge(&mut line, c.protocol);
                            // One layout keeps the title and address on a shared baseline,
                            // which two labels of different fonts, centered apart, miss.
                            let style = line.style().clone();
                            let mut text = egui::text::LayoutJob::default();
                            for part in [
                                RichText::new(title + "  ").strong(),
                                RichText::new(address)
                                    .monospace()
                                    .size(12.)
                                    .color(colors.muted),
                            ] {
                                part.append_to(
                                    &mut text,
                                    &style,
                                    egui::FontSelection::Default,
                                    Align::BOTTOM,
                                );
                            }
                            line.add(egui::Label::new(text).truncate());
                        }
                        ui.add_space(8.);
                        ui.label(
                            RichText::new(
                                "Arrow keys select · Enter connects · Shift+Enter browses files \
                                 · Esc closes",
                            )
                            .size(11.)
                            .color(colors.muted),
                        );
                    });
            });
        match chosen {
            Some(Choice::Saved(i)) => {
                let connection = self.connections[i].clone();
                if modifiers.shift && connection.protocol == Protocol::Ssh {
                    self.request_browse(connection);
                } else {
                    self.request_connect(connection);
                }
            }
            Some(Choice::Typed) => {
                let connection = typed.unwrap();
                if browses || (modifiers.shift && connection.protocol == Protocol::Ssh) {
                    self.request_browse(connection);
                } else {
                    self.request_connect(connection);
                }
            }
            None if escape || area.response.clicked_elsewhere() => self.refocus_session(),
            None => self.quick_open = Some(open),
        }
    }
}
