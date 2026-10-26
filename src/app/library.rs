//! The connection library: search, the connection table, and its rows.
use super::*;
use egui::{Align, Align2, FontId, Layout, Margin, Rect, Sense, UiBuilder};

impl App {
    pub(super) fn filtered(&self) -> Vec<Connection> {
        let query = self.query.to_lowercase();
        let recent = self.state.recent(&self.location);
        let mut items: Vec<_> = self
            .connections
            .iter()
            .filter(|c| {
                c.matches(&query)
                    && self.group.as_ref().is_none_or(|g| g == &c.group)
                    && match self.filter {
                        Filter::All => true,
                        Filter::Recent => recent.contains(&c.id),
                        Filter::Favorites => c.favorite,
                        Filter::Rdp => c.protocol == Protocol::Rdp,
                        Filter::Ssh => c.protocol == Protocol::Ssh,
                    }
            })
            .cloned()
            .collect();
        if self.filter == Filter::Recent && self.group.is_none() {
            items.sort_by_key(|c| recent.iter().position(|id| *id == c.id));
        } else {
            items.sort_by_key(|c| c.name.to_lowercase());
        }
        items
    }

    pub(super) fn workspace(&mut self, ui: &mut egui::Ui) {
        let colors = theme::palette(ui.ctx());
        let connections = self.filtered();
        let total = self.connections.len();
        let search_id = egui::Id::new("library-search");
        let focused = ui.memory(|m| m.focused());
        let mut index = self
            .selected
            .and_then(|id| connections.iter().position(|c| c.id == id));
        if !self.query.is_empty() && index.is_none() && !connections.is_empty() {
            index = Some(0);
            self.selected = Some(connections[0].id);
        }
        // Arrow keys move the selection while typing in search or when nothing has focus.
        if (focused.is_none() || focused == Some(search_id)) && !connections.is_empty() {
            let (down, up) = ui.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                )
            });
            let last = connections.len() - 1;
            let next = match index {
                _ if !down && !up => None,
                None => Some(if down { 0 } else { last }),
                Some(i) if down => Some((i + 1).min(last)),
                Some(i) => Some(i.saturating_sub(1)),
            };
            if let Some(next) = next {
                index = Some(next);
                self.selected = Some(connections[next].id);
                self.scroll_to = true;
            }
        }
        let enter_idle = focused.is_none()
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));

        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if accent_button(ui, "+  New connection", 166.)
                    .on_hover_text("Ctrl+N")
                    .clicked()
                {
                    self.new_connection();
                }
                ui.with_layout(Layout::left_to_right(Align::Max), |ui| {
                    let title = self.group.as_deref().unwrap_or(match self.filter {
                        Filter::All => "All connections",
                        Filter::Recent => "Recent",
                        Filter::Favorites => "Favorites",
                        Filter::Rdp => "Remote desktops",
                        Filter::Ssh => "SSH terminals",
                    });
                    ui.add(egui::Label::new(RichText::new(title).size(24.).strong()).truncate());
                    let open = self.sessions.iter().filter(|s| s.is_running()).count();
                    let mut summary = if connections.len() == total {
                        format!("{total} connections")
                    } else {
                        format!("{} of {total}", connections.len())
                    };
                    if open > 0 {
                        summary.push_str(&format!(" · {open} open"));
                    }
                    ui.add(
                        egui::Label::new(RichText::new(summary).size(13.).color(colors.muted))
                            .truncate(),
                    );
                });
            });
        });
        ui.add_space(14.);
        let mut search = None;
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if !matches!(self.location, database::Location::Local(_))
                    && outline_button(ui, "Refresh", 86.)
                        .on_hover_text("Load the latest team changes")
                        .clicked()
                {
                    self.refresh();
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    let response = ui.add_sized(
                        [ui.available_width().min(420.), CONTROL_HEIGHT],
                        input(&mut self.query, "Search name, address, or group   Ctrl+K")
                            .id(search_id),
                    );
                    if ui.available_width() > 230. {
                        ui.label(
                            RichText::new("↑↓ select · Enter connects · Esc clears")
                                .size(11.)
                                .color(colors.muted),
                        );
                    }
                    search = Some(response);
                });
            });
        });
        let search = search.unwrap();
        if self.search_focus {
            search.request_focus();
            self.search_focus = false;
        }
        if search.has_focus() {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    search_id,
                    egui::EventFilter {
                        vertical_arrows: true,
                        horizontal_arrows: true,
                        ..Default::default()
                    },
                )
            });
        }
        let (enter, escape) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if search.lost_focus() && escape {
            self.query.clear();
        }
        // Anything that parses as a host can be reached without a saved profile: Enter does
        // that when nothing matches, and Alt+Enter always does.
        let quick = Connection::quick(&self.query);
        let submitted = search.lost_focus() && enter;
        let alt = ui.input(|i| i.modifiers.alt);
        if submitted
            && (alt || connections.is_empty())
            && let Some(target) = &quick
        {
            self.request_connect(target.clone());
            return;
        }
        if (submitted || enter_idle)
            && let Some(c) = connections.get(index.unwrap_or(0))
        {
            self.selected = Some(c.id);
            self.request_connect(c.clone());
            return;
        }
        ui.add_space(14.);
        if let Some(target) = &quick {
            let label = format!("{} {}", target.protocol.label(), target_label(target));
            if connections.is_empty() {
                ui.horizontal(|ui| {
                    if accent_button(ui, &format!("Connect to {label}"), 0.)
                        .on_hover_text("Enter")
                        .clicked()
                    {
                        self.request_connect(target.clone());
                    }
                    ui.label(
                        RichText::new(
                            "Enter connects. Not in your library; save it from the session to keep it.",
                        )
                        .size(12.)
                        .color(colors.muted),
                    );
                });
                return;
            }
            ui.label(
                RichText::new(format!("Alt+Enter connects to {label} directly"))
                    .size(11.)
                    .color(colors.muted),
            );
            ui.add_space(4.);
        }
        if connections.is_empty() {
            if total == 0 {
                self.empty_library(ui);
                return;
            }
            ui.horizontal(|ui| {
                ui.label(RichText::new("No connections match.").color(colors.muted));
                if !self.query.is_empty() && ui.button("Clear search").clicked() {
                    self.query.clear();
                }
                if (self.group.is_some() || self.filter != Filter::All)
                    && ui.button("Show all").clicked()
                {
                    self.group = None;
                    self.filter = Filter::All;
                }
            });
            return;
        }
        self.connection_table(ui, &connections);
    }

    pub(super) fn empty_library(&mut self, ui: &mut egui::Ui) {
        let colors = theme::palette(ui.ctx());
        ui.add_space(8.);
        egui::Frame::new()
            .fill(colors.panel)
            .corner_radius(10)
            .stroke(Stroke::new(1_f32, colors.line))
            .inner_margin(30.)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add_space(45.);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new("Your workspace, ready to connect.")
                            .size(23.)
                            .strong(),
                    );
                    ui.add_space(8.);
                    ui.label(
                        RichText::new("Add a machine or bring your existing connection library.")
                            .color(colors.muted),
                    );
                    ui.add_space(22.);
                    ui.horizontal(|ui| {
                        ui.add_space((ui.available_width() - 330.).max(0.) / 2.);
                        if accent_button(ui, "+  New connection", 166.).clicked() {
                            self.new_connection();
                        }
                        if outline_button(ui, "Import CSV", 145.).clicked() {
                            self.import_csv();
                        }
                    });
                });
                ui.add_space(55.);
            });
    }

    pub(super) fn connection_table(&mut self, ui: &mut egui::Ui, connections: &[Connection]) {
        let colors = theme::palette(ui.ctx());
        egui::Frame::new()
            .fill(colors.panel)
            .corner_radius(10)
            .stroke(Stroke::new(1_f32, colors.line))
            .inner_margin(Margin::symmetric(20, 4))
            .show(ui, |ui| {
                let width = ui.available_width();
                let columns = table_columns(width);
                let (header, _) = ui.allocate_exact_size(Vec2::new(width, 32.), Sense::hover());
                let mut x = header.min.x;
                for (column, w) in &columns {
                    ui.painter().text(
                        egui::pos2(x, header.center().y),
                        Align2::LEFT_CENTER,
                        column.title(),
                        FontId::proportional(11.),
                        colors.muted,
                    );
                    x += w + GAP;
                }
                ui.painter().line_segment(
                    [header.left_bottom(), header.right_bottom()],
                    Stroke::new(1_f32, colors.line),
                );
                let height = (ui.available_height() - 8.).max(ROW * 3.);
                let mut area = egui::ScrollArea::vertical()
                    .id_salt("connection-list")
                    .max_height(height)
                    .auto_shrink([false, false]);
                let selected = self
                    .selected
                    .and_then(|id| connections.iter().position(|c| c.id == id));
                if std::mem::take(&mut self.scroll_to)
                    && let Some(index) = selected
                {
                    let (offset, visible) = self.list_view;
                    let top = index as f32 * ROW;
                    if top < offset {
                        area = area.vertical_scroll_offset(top);
                    } else if top + ROW > offset + visible {
                        area = area.vertical_scroll_offset(top + ROW - visible);
                    }
                }
                ui.spacing_mut().item_spacing.y = 0.;
                let output = area.show_rows(ui, ROW, connections.len(), |ui, range| {
                    for index in range {
                        let divider = index + 1 < connections.len();
                        self.connection_row(ui, &connections[index], &columns, divider);
                    }
                });
                self.list_view = (output.state.offset.y, output.inner_rect.height());
            });
    }

    pub(super) fn connection_row(
        &mut self,
        ui: &mut egui::Ui,
        c: &Connection,
        columns: &[(Column, f32)],
        divider: bool,
    ) {
        let (height, gap) = (ROW, GAP);
        let colors = theme::palette(ui.ctx());
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click());
        if divider {
            ui.painter().line_segment(
                [rect.left_bottom(), rect.right_bottom()],
                Stroke::new(1_f32, colors.line),
            );
        }
        let selected = self.selected == Some(c.id);
        // The star, name and buttons take the hover from the row itself, so use the pointer.
        if selected || ui.rect_contains_pointer(rect) {
            let fill = if selected {
                colors.selected
            } else {
                colors.hover
            };
            ui.painter()
                .rect_filled(rect.shrink2(Vec2::new(0., 2.)), 6., fill);
        }
        if response.clicked() {
            self.selected = Some(c.id);
        }
        if response.double_clicked() {
            self.request_connect(c.clone());
        }
        let description = c
            .metadata
            .get("ConnectionDescription")
            .filter(|d| !d.trim().is_empty());
        let mut x = rect.min.x;
        for (column, w) in columns {
            let cell =
                Rect::from_min_size(egui::pos2(x, rect.min.y + 6.), Vec2::new(*w, height - 12.));
            x += w + gap;
            // A child ui does not move the row cursor, so rows keep their exact height.
            let align = match column {
                Column::Star => Align::Center,
                _ => Align::LEFT,
            };
            let ui = &mut ui.new_child(
                UiBuilder::new()
                    .max_rect(cell)
                    .layout(Layout::top_down(align)),
            );
            // Leave room for hover outlines, which extend slightly past a widget.
            ui.set_clip_rect(cell.expand(4.).intersect(ui.clip_rect()));
            ui.spacing_mut().item_spacing.y = 2.;
            match column {
                Column::Star => {
                    ui.spacing_mut().button_padding.x = 0.;
                    ui.add_space(3.);
                    let star = egui::Button::new(
                        RichText::new(if c.favorite { "★" } else { "☆" })
                            .size(17.)
                            .color(if c.favorite {
                                colors.accent
                            } else {
                                colors.muted
                            }),
                    )
                    .frame(false)
                    .min_size(Vec2::new(28., 28.));
                    if ui.add(star).on_hover_text("Toggle favorite").clicked() {
                        let mut next = self.connections.clone();
                        if let Some(item) = next.iter_mut().find(|v| v.id == c.id) {
                            item.favorite = !item.favorite;
                        }
                        self.persist(next);
                    }
                }
                Column::Name => {
                    if description.is_none() {
                        ui.add_space(8.);
                    }
                    let name = ui.add(
                        egui::Label::new(RichText::new(&c.name).size(14.).strong())
                            .truncate()
                            .sense(Sense::click()),
                    );
                    if name.clicked() {
                        self.selected = Some(c.id);
                    }
                    if name.double_clicked() {
                        self.request_connect(c.clone());
                    }
                    name.on_hover_text(&c.name);
                    if let Some(description) = description {
                        ui.add(
                            egui::Label::new(
                                RichText::new(description).size(11.).color(colors.muted),
                            )
                            .truncate(),
                        );
                    }
                }
                Column::Protocol => {
                    ui.add_space(5.);
                    protocol_badge(ui, c.protocol);
                }
                Column::Host => {
                    ui.add(
                        egui::Label::new(
                            RichText::new(c.address())
                                .monospace()
                                .size(12.)
                                .color(colors.text),
                        )
                        .truncate(),
                    )
                    .on_hover_text(c.address());
                    let user = if c.username.is_empty() {
                        "Ask on connect"
                    } else {
                        &c.username
                    };
                    ui.add(
                        egui::Label::new(RichText::new(user).size(11.).color(colors.muted))
                            .truncate(),
                    );
                }
                Column::Group => {
                    ui.add_space(9.);
                    ui.add(
                        egui::Label::new(RichText::new(&c.group).size(12.).color(colors.muted))
                            .truncate(),
                    )
                    .on_hover_text(&c.group);
                }
                Column::Actions => {
                    ui.add_space(4.);
                    ui.spacing_mut().button_padding = Vec2::new(10., 4.);
                    ui.horizontal(|ui| {
                        let connect =
                            egui::Button::new(RichText::new("Connect").size(12.)).corner_radius(5);
                        if ui.add_sized([72., 28.], connect).clicked() {
                            self.request_connect(c.clone());
                        }
                        ui.menu_button("···", |ui| {
                            if ui.button("Edit connection").clicked() {
                                self.edit(c.clone());
                                ui.close();
                            }
                            if ui.button("Duplicate").clicked() {
                                let mut copy = c.clone();
                                copy.id = 0;
                                copy.name = format!("{} (copy)", c.name);
                                copy.metadata.remove("ConnectionId");
                                self.edit(copy);
                                ui.close();
                            }
                            if ui.button("Copy address").clicked() {
                                ui.ctx().copy_text(c.address());
                                self.notify(format!("Copied {}", c.address()));
                                ui.close();
                            }
                            if rdp::EMBEDDED
                                && c.protocol == Protocol::Rdp
                                && ui
                                    .button("Forget saved password")
                                    .on_hover_text(
                                        "Removes the credentials Windows saved for this host",
                                    )
                                    .clicked()
                            {
                                match rdp::forget_credentials(&c.host) {
                                    Ok(true) => self.notify(format!(
                                        "Removed saved credentials for {}",
                                        c.host
                                    )),
                                    Ok(false) => self.notify(format!(
                                        "Windows has no saved credentials for {}",
                                        c.host
                                    )),
                                    Err(e) => {
                                        self.error =
                                            Some(format!("Cannot remove saved credentials: {e}"))
                                    }
                                }
                                ui.close();
                            }
                            ui.separator();
                            if ui.button("Delete connection").clicked() {
                                self.modal = Modal::Delete(c.id);
                                ui.close();
                            }
                        });
                    });
                }
            }
        }
    }
}

pub(super) const ROW: f32 = 48.;

pub(super) const GAP: f32 = 16.;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Column {
    Star,
    Name,
    Protocol,
    Host,
    Group,
    Actions,
}

impl Column {
    fn title(self) -> &'static str {
        match self {
            Self::Name => "NAME",
            Self::Protocol => "PROTOCOL",
            Self::Host => "HOST / USER",
            Self::Group => "GROUP",
            Self::Star | Self::Actions => "",
        }
    }
}

/// Table columns with their widths; narrow tables drop the group, then the protocol column.
pub(super) fn table_columns(width: f32) -> Vec<(Column, f32)> {
    // Widths above 1 are fixed pixels; the rest are shares of the flexible space.
    let mut columns = vec![(Column::Star, 40.), (Column::Name, 0.41)];
    if width >= 560. {
        columns.push((Column::Protocol, 56.));
    }
    columns.push((Column::Host, 0.35));
    if width >= 720. {
        columns.push((Column::Group, 0.24));
    }
    columns.push((Column::Actions, 120.));
    let fixed: f32 = columns.iter().filter(|c| c.1 > 1.).map(|c| c.1).sum();
    let weights: f32 = columns.iter().filter(|c| c.1 <= 1.).map(|c| c.1).sum();
    let flexible = (width - fixed - GAP * (columns.len() - 1) as f32).max(120.);
    columns
        .into_iter()
        .map(|(column, w)| (column, if w > 1. { w } else { flexible * w / weights }))
        .collect()
}

/// A connection's user and address, as typed for quick connect.
pub(super) fn target_label(c: &Connection) -> String {
    if c.username.is_empty() {
        c.address()
    } else {
        format!("{}@{}", c.username, c.address())
    }
}
