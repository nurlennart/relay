use super::*;
use egui::{Align, Align2, FontId, Layout, Margin, Rect, Sense, UiBuilder};

const ROW: f32 = 48.;
const GAP: f32 = 16.;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Column {
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

impl App {
    pub fn sidebar(&mut self, ctx: &egui::Context) {
        let colors = theme::palette(ctx);
        let collapsed = self.sidebar_collapsed(ctx);
        egui::SidePanel::left("sidebar")
            .exact_width(if collapsed { 64. } else { 236. })
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(colors.panel)
                    .inner_margin(if collapsed {
                        Margin::symmetric(10, 20)
                    } else {
                        Margin::symmetric(16, 20)
                    })
                    .stroke(Stroke::new(1_f32, colors.line)),
            )
            .show(ctx, |ui| {
                let header = ui.horizontal(|ui| {
                    egui::Frame::new()
                        .fill(colors.accent)
                        .corner_radius(10)
                        .inner_margin(Margin::symmetric(10, 4))
                        .show(ui, |ui| {
                            ui.label(RichText::new("R").size(24.).strong().color(colors.bg));
                        });
                    if !collapsed {
                        ui.vertical(|ui| {
                            ui.label(RichText::new("Relay").size(23.).strong());
                            ui.label(
                                RichText::new("Remote workspace")
                                    .size(11.)
                                    .color(colors.muted),
                            );
                        });
                    }
                });
                let toggle = ui
                    .interact(header.response.rect, ui.id().with("toggle"), Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text(if collapsed {
                        "Expand sidebar (Ctrl+B)"
                    } else {
                        "Collapse sidebar (Ctrl+B)"
                    });
                if toggle.clicked() {
                    self.sidebar_collapsed = Some(!collapsed);
                }
                ui.add_space(if collapsed { 20. } else { 28. });
                let favorites = self.connections.iter().filter(|c| c.favorite).count();
                let count =
                    |p: Protocol| self.connections.iter().filter(|c| c.protocol == p).count();
                let filters = [
                    (Filter::All, "▦", "All connections", self.connections.len()),
                    (Filter::Favorites, "☆", "Favorites", favorites),
                    (Filter::Rdp, "▣", "Remote desktop", count(Protocol::Rdp)),
                    (Filter::Ssh, ">_", "SSH terminal", count(Protocol::Ssh)),
                ];
                let mut chosen = None;
                if collapsed {
                    for (filter, symbol, label, count) in filters {
                        let selected = self.filter == filter && self.group.is_none();
                        if rail_row(ui, symbol, &format!("{label} ({count})"), selected) {
                            chosen = Some(filter);
                        }
                    }
                } else {
                    for (index, (filter, symbol, label, count)) in filters.into_iter().enumerate() {
                        if index == 0 {
                            section_label(ui, "LIBRARY");
                        } else if index == 2 {
                            ui.add_space(20.);
                            section_label(ui, "PROTOCOLS");
                        }
                        let selected = self.filter == filter && self.group.is_none();
                        if nav_row(ui, symbol, label, count, selected) {
                            chosen = Some(filter);
                        }
                    }
                }
                if let Some(filter) = chosen {
                    self.filter = filter;
                    self.group = None;
                    self.active = None;
                }
                if !collapsed {
                    ui.add_space(20.);
                    section_label(ui, "GROUPS");
                    let mut groups = std::collections::BTreeMap::<String, usize>::new();
                    for c in &self.connections {
                        *groups.entry(c.group.clone()).or_default() += 1;
                    }
                    egui::ScrollArea::vertical()
                        .id_salt("groups")
                        .max_height((ui.available_height() - 200.).max(90.))
                        .show(ui, |ui| {
                            if groups.is_empty() {
                                ui.label(
                                    RichText::new("Your groups will appear here.")
                                        .size(12.)
                                        .color(colors.muted),
                                );
                            }
                            for (group, count) in groups {
                                let selected = self.group.as_ref() == Some(&group);
                                if nav_row(ui, "▱", &group, count, selected) {
                                    self.group = Some(group);
                                    self.filter = Filter::All;
                                    self.active = None;
                                }
                            }
                        });
                }
                ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
                    if collapsed {
                        if rail_row(ui, "⚙", "Settings", false) {
                            self.modal = Modal::Settings;
                        }
                        return;
                    }
                    ui.label(
                        RichText::new("Ctrl+K search · Ctrl+B sidebar")
                            .size(11.)
                            .color(colors.muted),
                    );
                    ui.add_space(8.);
                    if outline_button(ui, "Settings", ui.available_width()).clicked() {
                        self.modal = Modal::Settings;
                    }
                    ui.add_space(12.);
                    let card = egui::Frame::new()
                        .fill(colors.surface)
                        .corner_radius(8)
                        .inner_margin(12.)
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.colored_label(colors.accent, "●");
                                ui.label(RichText::new(self.location.label()).size(12.).strong());
                            });
                            ui.label(
                                RichText::new(if self.pending.is_some() {
                                    "Updating library…"
                                } else {
                                    "Connection profiles only"
                                })
                                .size(11.)
                                .color(colors.muted),
                            );
                            let manage = egui::Button::new(
                                RichText::new("Manage library  →")
                                    .size(12.)
                                    .color(colors.accent),
                            )
                            .frame(false);
                            if ui.add_enabled(self.pending.is_none(), manage).clicked() {
                                self.modal = Modal::Library;
                            }
                        });
                    if let database::Location::Local(path) | database::Location::Sqlite(path) =
                        &self.location
                    {
                        card.response.on_hover_text(path.display().to_string());
                    }
                    ui.add_space(10.);
                });
            });
    }

    pub fn filtered(&self) -> Vec<Connection> {
        let query = self.query.to_lowercase();
        let mut items: Vec<_> = self
            .connections
            .iter()
            .filter(|c| {
                c.matches(&query)
                    && self.group.as_ref().is_none_or(|g| g == &c.group)
                    && match self.filter {
                        Filter::All => true,
                        Filter::Favorites => c.favorite,
                        Filter::Rdp => c.protocol == Protocol::Rdp,
                        Filter::Ssh => c.protocol == Protocol::Ssh,
                    }
            })
            .cloned()
            .collect();
        items.sort_by_key(|c| c.name.to_lowercase());
        items
    }

    pub fn tabs(&mut self, ctx: &egui::Context) {
        let colors = theme::palette(ctx);
        egui::TopBottomPanel::top("tabs")
            .exact_height(58.)
            .frame(
                egui::Frame::new()
                    .fill(colors.panel)
                    .inner_margin(Margin::symmetric(20, 10))
                    .stroke(Stroke::new(1_f32, colors.line)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::horizontal()
                    .id_salt("tabs-scroll")
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let library =
                                tab(ui, "      Connection library", self.active.is_none())
                                    .on_hover_text("Ctrl+1");
                            library_icon(
                                ui,
                                egui::pos2(library.rect.left() + 18., library.rect.center().y),
                                if self.active.is_none() {
                                    colors.accent
                                } else {
                                    colors.muted
                                },
                            );
                            if library.clicked() {
                                self.active = None;
                            }
                            for (index, s) in self.sessions.iter().enumerate() {
                                ui.push_id(index, |ui| {
                                    let session = session_tab(
                                        ui,
                                        &s.connection,
                                        s.is_running(),
                                        self.active == Some(index),
                                    );
                                    if session.inner.0.clicked() {
                                        self.active = Some(index);
                                    }
                                    if session.inner.1.clicked() {
                                        self.modal = Modal::Close(index);
                                    }
                                });
                            }
                            #[cfg(feature = "ui-check")]
                            if std::env::var_os("RELAY_UI_TABS").is_some() {
                                for (i, c) in self.connections.iter().take(2).enumerate() {
                                    session_tab(ui, c, i == 0, false);
                                }
                            }
                            if ui
                                .add_sized([32., 34.], egui::Button::new("+").frame(false))
                                .on_hover_text("New connection (Ctrl+N)")
                                .clicked()
                            {
                                self.new_connection();
                            }
                        });
                    });
            });
    }

    pub fn workspace(&mut self, ui: &mut egui::Ui) {
        let colors = theme::palette(ui.ctx());
        let connections = self.filtered();
        let total = self.connections.len();
        if total == 0 {
            self.empty_library(ui);
            return;
        }
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
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    let title = self.group.as_deref().unwrap_or(match self.filter {
                        Filter::All => "All connections",
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
                        [ui.available_width().min(420.), 36.],
                        egui::TextEdit::singleline(&mut self.query)
                            .id(search_id)
                            .hint_text("Search name, address, or group   Ctrl+K")
                            .margin(Margin::symmetric(12, 9)),
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
        if (search.lost_focus() && enter || enter_idle)
            && let Some(c) = connections.get(index.unwrap_or(0))
        {
            self.selected = Some(c.id);
            self.connect(c.clone());
            return;
        }
        ui.add_space(14.);
        if connections.is_empty() {
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

    fn empty_library(&mut self, ui: &mut egui::Ui) {
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

    fn connection_table(&mut self, ui: &mut egui::Ui, connections: &[Connection]) {
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

    fn connection_row(
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
        if selected || response.hovered() {
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
            self.connect(c.clone());
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
            let ui = &mut ui.new_child(
                UiBuilder::new()
                    .max_rect(cell)
                    .layout(Layout::top_down(Align::LEFT)),
            );
            ui.set_clip_rect(cell.intersect(ui.clip_rect()));
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
                        self.connect(c.clone());
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
                            self.connect(c.clone());
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
                            if c.protocol == Protocol::Rdp
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

    pub fn refresh(&mut self) {
        if self.pending.is_none() {
            self.pending_save = false;
            self.pending = Some(database::start(
                self.location.clone(),
                self.revision,
                None,
                self.ctx.clone(),
            ));
        }
    }
    pub fn poll_storage(&mut self) {
        let result = self.pending.as_ref().map(|rx| rx.try_recv());
        if let Some(result) = result {
            match result {
                Ok(Ok(snapshot)) => {
                    self.connections = snapshot.connections;
                    self.revision = snapshot.revision;
                    self.storage_ok = true;
                    let mut remember_error = None;
                    let saved = self.pending_save;
                    let opened = self
                        .pending_location
                        .as_ref()
                        .map(|l| l.label().to_lowercase());
                    if let Some(location) = self.pending_location.take() {
                        if let database::Location::Mssql(connection) = &location {
                            remember_error = self.remember_mssql(connection).err();
                        }
                        self.location = location;
                        if matches!(self.modal, Modal::Library) {
                            self.modal = Modal::None;
                        }
                        self.group = None;
                        self.filter = Filter::All;
                        if !self.db_remember {
                            self.db_mssql.clear();
                        }
                    }
                    if self.pending_save {
                        self.recovery = None;
                    }
                    self.pending_save = false;
                    self.pending_editor = None;
                    self.pending = None;
                    self.error = remember_error;
                    // Routine saves stay silent; loads say what happened.
                    if let Some(opened) = opened {
                        self.notify(format!("Opened {opened}."));
                    } else if !saved {
                        self.notify("Library refreshed.");
                    }
                }
                Ok(Err(e)) => {
                    self.pending_save = false;
                    self.error = Some(e);
                    self.pending = None;
                    self.pending_location = None;
                    // Reopen the editor so a rejected edit is not lost.
                    if let Some(editor) = self.pending_editor.take() {
                        self.modal = Modal::Editor(editor);
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.error = Some(
                        "The library worker stopped unexpectedly. Your changes were not confirmed."
                            .into(),
                    );
                    self.pending = None;
                    self.pending_location = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
    }
    fn import_csv(&mut self) {
        for s in &mut self.sessions {
            s.place(None);
        }
        match file_dialog::choose(self.parent, false, false) {
            Ok(Some(path)) => match csv_io::read(&path, &self.connections) {
                Ok(import) => self.modal = Modal::Import(import),
                Err(e) => self.error = Some(e),
            },
            Ok(None) => {}
            Err(e) => self.error = Some(e),
        }
    }
    pub fn export_csv(&mut self, recovery: bool) {
        for s in &mut self.sessions {
            s.place(None);
        }
        match file_dialog::choose(self.parent, true, false) {
            Ok(Some(path)) => {
                let connections = if recovery {
                    self.recovery.as_deref().unwrap_or(&self.connections)
                } else {
                    &self.connections
                };
                match csv_io::export(&path, connections) {
                    Ok(()) => {
                        let text = format!(
                            "Exported {} connections to {}",
                            connections.len(),
                            path.display()
                        );
                        self.notify(text);
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            Ok(None) => {}
            Err(e) => self.error = Some(e),
        }
    }
    pub fn settings(&mut self, ctx: &egui::Context) {
        if !matches!(self.modal, Modal::Settings) {
            return;
        }
        let colors = theme::palette(ctx);
        let mut choice = theme::current(ctx);
        let before = choice;
        let mut close = false;
        let mut manage = false;
        let mut import = false;
        let mut export = false;
        let mut recovery = false;
        dialog("settings", ctx).show(ctx, |ui| {
            ui.set_width(dialog_width(ctx, 540.));
            ui.horizontal(|ui| {
                ui.label(RichText::new("Settings").size(25.).strong());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    close = ui.button("Close").clicked();
                });
            });
            egui::ScrollArea::vertical()
                .id_salt("settings-body")
                .min_scrolled_height((ctx.content_rect().height() - 180.).clamp(200., 580.))
                .max_height((ctx.content_rect().height() - 180.).clamp(200., 580.))
                .show(ui, |ui| {
                    ui.add_space(14.);
                    section_label(ui, "APPEARANCE");
                    ui.label(
                        RichText::new("Choose a workspace theme. Saved on this computer.")
                            .color(colors.muted),
                    );
                    ui.add_space(4.);
                    for pair in theme::Theme::ALL.chunks(2) {
                        ui.columns(2, |columns| {
                            for (column, &candidate) in pair.iter().enumerate() {
                                if theme_option(
                                    &mut columns[column],
                                    candidate,
                                    choice == candidate,
                                )
                                .clicked()
                                {
                                    choice = candidate;
                                }
                            }
                        });
                    }
                    ui.add_space(14.);
                    ui.separator();
                    section_label(ui, "CONNECTION LIBRARY");
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(self.location.label()).strong());
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            manage = ui
                                .add_enabled(
                                    self.pending.is_none(),
                                    egui::Button::new("Manage library"),
                                )
                                .clicked();
                        });
                    });
                    ui.label(
                        RichText::new(
                            "Personal storage, shared SQLite files, or Microsoft SQL Server.",
                        )
                        .size(12.)
                        .color(colors.muted),
                    );
                    ui.add_space(14.);
                    ui.separator();
                    section_label(ui, "IMPORT & EXPORT");
                    ui.label(
                        RichText::new("Transfer connection profiles using CSV files.")
                            .color(colors.muted),
                    );
                    ui.add_space(4.);
                    let button_width = (ui.available_width() - ui.spacing().item_spacing.x) / 2.;
                    ui.horizontal(|ui| {
                        ui.add_enabled_ui(self.pending.is_none(), |ui| {
                            import = outline_button(ui, "Import CSV", button_width).clicked();
                        });
                        export = outline_button(ui, "Export CSV", button_width).clicked();
                    });
                    if self.recovery.is_some() {
                        recovery = ui.button("Export unsaved changes").clicked();
                    }
                    if let Some(error) = &self.error {
                        ui.colored_label(ui.visuals().error_fg_color, error);
                    }
                    if let Some((notice, _)) = &self.notice {
                        ui.colored_label(colors.accent, notice);
                    }
                });
        });
        if choice != before {
            choice.apply(ctx);
            #[cfg(feature = "ui-check")]
            let preview = std::env::var_os("RELAY_UI_CHECK").is_some();
            #[cfg(not(feature = "ui-check"))]
            let preview = false;
            if !preview && let Err(error) = choice.save(&theme::path()) {
                self.error = Some(format!(
                    "Theme applied, but could not save preference: {error}"
                ));
            }
        }
        if close || ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.modal = Modal::None;
        }
        if manage {
            self.modal = Modal::Library;
        }
        if import {
            // Replaces settings with the import preview once a file is read.
            self.import_csv();
        }
        if export {
            self.export_csv(false);
        }
        if recovery {
            self.export_csv(true);
        }
    }

    pub fn library_dialogs(&mut self, ctx: &egui::Context) {
        let colors = theme::palette(ctx);
        let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if matches!(self.modal, Modal::Library) {
            let mut open = false;
            let mut cancel = false;
            dialog("database", ctx).show(ctx, |ui| {
                ui.set_width(dialog_width(ctx, 600.));
                ui.heading("Choose your connection library");
                ui.label(
                    RichText::new("Switch libraries without interrupting your open sessions.")
                        .color(colors.muted),
                );
                ui.add_space(16.);
                ui.horizontal(|ui| {
                    for (kind, label) in [
                        (LibraryKind::Personal, "Personal"),
                        (LibraryKind::Sqlite, "SQLite file"),
                        (LibraryKind::Mssql, "MSSQL server"),
                    ] {
                        ui.selectable_value(&mut self.db_kind, kind, label);
                    }
                });
                ui.add_space(16.);
                match self.db_kind {
                    LibraryKind::Personal => {
                        ui.label("Your personal library stays on this computer.");
                        ui.label(
                            RichText::new(self.path.display().to_string())
                                .monospace()
                                .size(12.)
                                .color(colors.muted),
                        );
                    }
                    LibraryKind::Sqlite => {
                        field(
                            ui,
                            "Database path (local, mapped drive, or UNC)",
                            &mut self.db_path,
                            "\\\\server\\share\\relay.sqlite",
                        );
                        if ui.button("Browse…").clicked() {
                            match file_dialog::choose(self.parent, false, true) {
                                Ok(Some(path)) => self.db_path = path.display().to_string(),
                                Ok(None) => {}
                                Err(e) => self.error = Some(e),
                            }
                        }
                        ui.label(
                            RichText::new(
                                "Creates a new database if the file does not exist. Existing \
                                 personal connections are not moved; use CSV import to add them.",
                            )
                            .size(12.)
                            .color(colors.muted),
                        );
                        ui.add_space(10.);
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            "For network drives, use a reliable share with file locking and \
                             backups. MSSQL is the better choice for concurrent team editing.",
                        );
                    }
                    LibraryKind::Mssql => {
                        ui.label("ADO.NET connection string");
                        let hint = "Server=tcp:sql,1433;Database=Relay;Integrated Security=true";
                        ui.horizontal(|ui| {
                            ui.add_sized(
                                [ui.available_width() - 70., 36.],
                                egui::TextEdit::singleline(&mut self.db_mssql)
                                    .password(!self.db_reveal)
                                    .hint_text(hint),
                            );
                            ui.toggle_value(&mut self.db_reveal, "Show");
                        });
                        ui.checkbox(
                            &mut self.db_remember,
                            "Remember on this computer (encrypted for your Windows account)",
                        );
                        let storage = if self.db_remember {
                            "Saved with Windows DPAPI after the library opens."
                        } else {
                            "Held in memory for this session only. \
                             Opening without this option removes a saved string."
                        };
                        ui.label(
                            RichText::new(format!(
                                "Windows integrated authentication and SQL logins are supported. \
                                 TLS is required. {storage}"
                            ))
                            .size(12.)
                            .color(colors.muted),
                        );
                        ui.add_space(10.);
                        ui.label(
                            "Run sql/mssql.sql once in an existing database. \
                             Your team needs SELECT and UPDATE on dbo.RelayLibrary.",
                        );
                    }
                }
                ui.add_space(18.);
                ui.horizontal(|ui| {
                    let idle = self.pending.is_none();
                    let button =
                        egui::Button::new(RichText::new("Open library").color(colors.bg).strong())
                            .fill(colors.accent)
                            .min_size(Vec2::new(150., 36.));
                    open = ui.add_enabled(idle, button).clicked();
                    cancel = ui.add_enabled(idle, egui::Button::new("Cancel")).clicked();
                    if !idle {
                        ui.spinner();
                        ui.label("Opening library…");
                    }
                });
                if let Some(error) = &self.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
            });
            if open {
                self.open_library();
            } else if cancel || escape && self.pending.is_none() {
                self.modal = Modal::None;
            }
        }
        if let Modal::Import(import) = &self.modal {
            let mut confirm = false;
            let mut cancel = false;
            dialog("import", ctx).show(ctx, |ui| {
                ui.set_width(dialog_width(ctx, 680.));
                ui.heading("Review CSV import");
                ui.label(RichText::new(&import.filename).color(colors.muted));
                ui.add_space(16.);
                ui.horizontal(|ui| {
                    ui.colored_label(
                        colors.accent,
                        format!("{} ready to import", import.connections.len()),
                    );
                    ui.label(format!("{} duplicates", import.duplicates));
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        format!("{} skipped", import.skipped.len()),
                    );
                });
                ui.add_space(12.);
                egui::ScrollArea::vertical()
                    .id_salt("import-preview")
                    .max_height((ctx.content_rect().height() - 380.).clamp(100., 260.))
                    .show(ui, |ui| {
                        for c in &import.connections {
                            ui.horizontal(|ui| {
                                protocol_badge(ui, c.protocol);
                                ui.add(egui::Label::new(&c.name).truncate());
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(&c.group).size(11.).color(colors.muted),
                                    )
                                    .truncate(),
                                );
                            });
                        }
                    });
                if !import.skipped.is_empty() {
                    ui.collapsing("Skipped rows — inspect reasons", |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("skip-reasons")
                            .max_height(150.)
                            .show(ui, |ui| {
                                for reason in &import.skipped {
                                    ui.label(reason);
                                }
                            });
                    });
                }
                ui.add_space(16.);
                ui.label(
                    RichText::new(
                        "Existing connections stay unchanged. \
                         Unsupported protocols such as VNC are not imported.",
                    )
                    .size(12.)
                    .color(colors.muted),
                );
                ui.horizontal(|ui| {
                    let label = format!("Import {} connections", import.connections.len());
                    let button = egui::Button::new(RichText::new(label).color(colors.bg).strong())
                        .fill(colors.accent);
                    let ready = !import.connections.is_empty() && self.pending.is_none();
                    confirm = ui.add_enabled(ready, button).clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
            if confirm {
                let mut next = self.connections.clone();
                next.extend(import.connections.iter().cloned());
                if self.persist(next) {
                    self.modal = Modal::None;
                }
            } else if cancel || escape {
                self.modal = Modal::None;
            }
        }
    }

    /// Stores or removes the encrypted connection string after it opened successfully.
    fn remember_mssql(&self, connection: &str) -> Result<(), String> {
        let path = secret::path();
        if self.db_remember {
            secret::save(&path, connection).map_err(|e| {
                format!("The library opened, but the connection string could not be saved: {e}")
            })
        } else {
            secret::forget(&path).map_err(|e| {
                format!(
                    "The library opened, but the saved connection string could not be removed: {e}"
                )
            })
        }
    }

    fn open_library(&mut self) {
        let location = match self.db_kind {
            LibraryKind::Personal => database::Location::Local(self.path.clone()),
            LibraryKind::Sqlite if !self.db_path.trim().is_empty() => {
                database::Location::Sqlite(PathBuf::from(self.db_path.trim()))
            }
            LibraryKind::Mssql if !self.db_mssql.trim().is_empty() => {
                database::Location::Mssql(self.db_mssql.clone())
            }
            _ => {
                self.error = Some("Enter a database path or connection string.".into());
                return;
            }
        };
        self.pending_save = false;
        self.pending_location = Some(location.clone());
        self.pending = Some(database::start(location, 0, None, self.ctx.clone()));
    }
}

fn theme_option(ui: &mut egui::Ui, candidate: theme::Theme, selected: bool) -> egui::Response {
    let colors = theme::palette(ui.ctx());
    let response = ui.add_sized(
        [ui.available_width(), 40.],
        egui::Button::new("").selected(selected),
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), candidate.name())
    });
    let center = response.rect.center();
    ui.painter().circle_filled(
        egui::pos2(response.rect.left() + 20., center.y),
        5.,
        candidate.colors().accent,
    );
    ui.painter().text(
        egui::pos2(response.rect.left() + 40., center.y),
        Align2::LEFT_CENTER,
        candidate.name(),
        FontId::proportional(14.),
        colors.text,
    );
    if selected {
        ui.painter().text(
            egui::pos2(response.rect.right() - 16., center.y),
            Align2::RIGHT_CENTER,
            "Selected",
            FontId::proportional(12.),
            colors.accent,
        );
    }
    response
}

/// Table columns with their widths; narrow tables drop the group, then the protocol column.
fn table_columns(width: f32) -> Vec<(Column, f32)> {
    // Widths above 1 are fixed pixels; the rest are shares of the flexible space.
    let mut columns = vec![(Column::Star, 30.), (Column::Name, 0.41)];
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

fn section_label(ui: &mut egui::Ui, text: &str) {
    let colors = theme::palette(ui.ctx());
    ui.add_space(2.);
    ui.label(RichText::new(text).size(10.).color(colors.muted));
    ui.add_space(6.);
}
fn nav_row(ui: &mut egui::Ui, symbol: &str, label: &str, count: usize, selected: bool) -> bool {
    let colors = theme::palette(ui.ctx());
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 36.), Sense::click());
    if selected || response.hovered() {
        ui.painter().rect_filled(
            rect,
            6.,
            if selected {
                colors.selected
            } else {
                colors.hover
            },
        );
    }
    let color = if selected { colors.accent } else { colors.text };
    nav_icon(ui, symbol, rect.min + Vec2::new(16., 18.), color);
    let text_rect =
        Rect::from_min_max(rect.min + Vec2::new(36., 8.), rect.max - Vec2::new(30., 4.));
    ui.scope_builder(UiBuilder::new().max_rect(text_rect), |ui| {
        ui.add(
            egui::Label::new(RichText::new(label).size(13.).color(color))
                .truncate()
                .selectable(false),
        );
    });
    ui.painter().text(
        rect.max - Vec2::new(10., 18.),
        Align2::RIGHT_CENTER,
        count.to_string(),
        FontId::monospace(11.),
        colors.muted,
    );
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(label)
        .clicked()
}
fn rail_row(ui: &mut egui::Ui, symbol: &str, tooltip: &str, selected: bool) -> bool {
    let colors = theme::palette(ui.ctx());
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 40.), Sense::click());
    if selected || response.hovered() {
        let fill = if selected {
            colors.selected
        } else {
            colors.hover
        };
        ui.painter().rect_filled(rect, 6., fill);
    }
    let color = if selected { colors.accent } else { colors.text };
    nav_icon(ui, symbol, rect.center(), color);
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(tooltip)
        .clicked()
}
fn nav_icon(ui: &egui::Ui, symbol: &str, center: egui::Pos2, color: Color32) {
    match symbol {
        "▦" => library_icon(ui, center, color),
        "▱" => {
            let points = [
                (-7., -5.),
                (-2., -5.),
                (0., -3.),
                (7., -3.),
                (7., 6.),
                (-7., 6.),
            ]
            .map(|(x, y)| center + Vec2::new(x, y));
            ui.painter().add(egui::Shape::closed_line(
                points.to_vec(),
                Stroke::new(1.25_f32, color),
            ));
            ui.painter().line_segment(
                [center + Vec2::new(-7., -1.), center + Vec2::new(7., -1.)],
                Stroke::new(1.25_f32, color),
            );
        }
        _ => {
            ui.painter().text(
                center,
                Align2::CENTER_CENTER,
                symbol,
                FontId::monospace(14.),
                color,
            );
        }
    }
}
fn library_icon(ui: &egui::Ui, center: egui::Pos2, color: Color32) {
    for y in 0..2 {
        for x in 0..2 {
            let min = center + Vec2::new(-6.5 + x as f32 * 8., -6.5 + y as f32 * 8.);
            ui.painter()
                .rect_filled(Rect::from_min_size(min, Vec2::splat(5.)), 1., color);
        }
    }
}
fn tab(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
    let colors = theme::palette(ui.ctx());
    ui.add(
        egui::Button::new(RichText::new(label).size(13.).color(if selected {
            colors.text
        } else {
            colors.muted
        }))
        .fill(if selected {
            colors.surface
        } else {
            Color32::TRANSPARENT
        })
        .stroke(Stroke::NONE)
        .corner_radius(6)
        .min_size(Vec2::new(0., 36.)),
    )
}
fn session_tab(
    ui: &mut egui::Ui,
    connection: &Connection,
    running: bool,
    selected: bool,
) -> egui::InnerResponse<(egui::Response, egui::Response)> {
    let colors = theme::palette(ui.ctx());
    let text = if selected { colors.text } else { colors.muted };
    let label = connection.name.as_str();
    let label_width = ui.fonts_mut(|fonts| {
        fonts
            .layout_no_wrap(label.to_owned(), FontId::proportional(13.), text)
            .size()
            .x
    });
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(label_width + 82., 36.), Sense::hover());
    ui.painter().rect_filled(
        rect,
        6.,
        if selected {
            colors.surface
        } else {
            colors.panel
        },
    );
    ui.painter().rect_stroke(
        rect,
        6.,
        Stroke::new(1_f32, colors.line),
        egui::StrokeKind::Inside,
    );
    let status = if running {
        colors.accent
    } else {
        ui.visuals().error_fg_color
    };
    ui.painter()
        .circle_filled(egui::pos2(rect.left() + 14., rect.center().y), 3.5, status);
    let select_rect =
        Rect::from_min_max(rect.min + Vec2::new(22., 0.), rect.max - Vec2::new(36., 0.));
    let close_rect = Rect::from_center_size(
        egui::pos2(rect.right() - 18., rect.center().y),
        Vec2::splat(28.),
    );
    let select = ui
        .put(
            select_rect,
            egui::Button::new(RichText::new(label).size(13.).color(text)).frame(false),
        )
        .on_hover_text(format!(
            "{} · {} · {}",
            connection.protocol.label(),
            connection.address(),
            if running { "open" } else { "ended" }
        ));
    let close = ui
        .put(
            close_rect,
            egui::Button::new(RichText::new("×").size(17.))
                .small()
                .frame(false),
        )
        .on_hover_text(format!("Close {label} (Ctrl+W)"));
    egui::InnerResponse {
        inner: (select, close),
        response,
    }
}
fn outline_button(ui: &mut egui::Ui, label: &str, width: f32) -> egui::Response {
    let colors = theme::palette(ui.ctx());
    ui.add_sized(
        [width, 36.],
        egui::Button::new(RichText::new(label).size(13.))
            .fill(colors.surface)
            .stroke(Stroke::new(1_f32, colors.line))
            .corner_radius(6),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn test_app(ctx: &egui::Context) -> App {
        let connections = ["Alpha", "Bravo", "Charlie"]
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let mut c = Connection::new();
                c.id = i as u64 + 1;
                c.name = (*name).into();
                c.host = format!("{}.example", name.to_lowercase());
                c
            })
            .collect();
        // A null window makes connect() report an error instead of opening a session.
        App::with(
            ctx.clone(),
            PathBuf::from("unused.json"),
            Ok(connections),
            None,
            std::ptr::null_mut(),
        )
    }

    fn library_frame(app: &mut App, ctx: &egui::Context, events: Vec<egui::Event>) {
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(1200., 800.),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| app.workspace(ui));
            },
        );
    }

    fn press(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn search_enter_connects_the_top_match() {
        let ctx = egui::Context::default();
        let mut app = test_app(&ctx);
        app.search_focus = true;
        library_frame(&mut app, &ctx, vec![]);
        library_frame(&mut app, &ctx, vec![egui::Event::Text("ch".into())]);
        library_frame(&mut app, &ctx, vec![]);
        assert_eq!(app.query, "ch");
        assert_eq!(app.selected, Some(3));
        library_frame(&mut app, &ctx, vec![press(egui::Key::Enter)]);
        assert_eq!(app.selected, Some(3));
        assert_eq!(
            app.error.as_deref(),
            Some("Cannot obtain the native application window."),
            "Enter did not try to connect"
        );
    }

    #[test]
    fn arrows_move_selection_and_escape_clears_search() {
        let ctx = egui::Context::default();
        let mut app = test_app(&ctx);
        app.search_focus = true;
        library_frame(&mut app, &ctx, vec![]);
        library_frame(&mut app, &ctx, vec![egui::Event::Text("a".into())]);
        library_frame(&mut app, &ctx, vec![]);
        // "a" matches all three names (and hosts); the first is selected.
        assert_eq!(app.selected, Some(1));
        for expected in [2, 3, 3] {
            library_frame(&mut app, &ctx, vec![press(egui::Key::ArrowDown)]);
            assert_eq!(app.selected, Some(expected));
            assert!(ctx.memory(|m| m.has_focus(egui::Id::new("library-search"))));
        }
        library_frame(&mut app, &ctx, vec![press(egui::Key::ArrowUp)]);
        assert_eq!(app.selected, Some(2));
        library_frame(&mut app, &ctx, vec![press(egui::Key::Escape)]);
        assert!(app.query.is_empty());
        // With nothing focused, arrows and Enter still drive the list.
        library_frame(&mut app, &ctx, vec![press(egui::Key::ArrowUp)]);
        assert_eq!(app.selected, Some(1));
        assert!(app.error.is_none());
        library_frame(&mut app, &ctx, vec![press(egui::Key::Enter)]);
        assert!(app.error.is_some(), "Enter did not try to connect");
    }

    #[test]
    fn narrow_tables_drop_columns_and_fit() {
        let kinds = |width| -> Vec<_> { table_columns(width).into_iter().map(|c| c.0).collect() };
        assert!(kinds(900.).contains(&Column::Group));
        assert!(!kinds(700.).contains(&Column::Group) && kinds(700.).contains(&Column::Protocol));
        assert!(!kinds(500.).contains(&Column::Protocol));
        for width in [500., 700., 900., 1400.] {
            let columns = table_columns(width);
            let used: f32 =
                columns.iter().map(|c| c.1).sum::<f32>() + GAP * (columns.len() - 1) as f32;
            assert!((used - width).abs() < 0.5, "{width}: {used}");
        }
    }

    #[test]
    fn tab_close_is_inside_tab_and_does_not_select_it() {
        for close_target in [false, true] {
            let ctx = egui::Context::default();
            let frame = |events| {
                let mut result = None;
                let _ = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(
                            egui::Pos2::ZERO,
                            Vec2::new(500., 100.),
                        )),
                        events,
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            ui.horizontal(|ui| {
                                let mut first = Connection::new();
                                first.name = "Design workstation".into();
                                let mut second = Connection::new();
                                second.name = "Build server".into();
                                let session = session_tab(ui, &first, true, true);
                                assert!(session.response.rect.contains_rect(session.inner.1.rect));
                                assert!(!session.inner.0.rect.intersects(session.inner.1.rect));
                                let next = session_tab(ui, &second, false, false);
                                assert_eq!(
                                    session.response.rect.center().y,
                                    next.response.rect.center().y
                                );
                                assert!(session.response.rect.right() < next.response.rect.left());
                                result = Some((
                                    session.inner.0.clicked(),
                                    session.inner.1.clicked(),
                                    if close_target {
                                        session.inner.1.rect.center()
                                    } else {
                                        session.inner.0.rect.center()
                                    },
                                ));
                            });
                        });
                    },
                );
                result.unwrap()
            };
            let (_, _, pos) = frame(vec![]);
            frame(vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
            let (selected, closed, _) = frame(vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }]);
            assert_eq!(selected, !close_target);
            assert_eq!(closed, close_target);
        }
    }

    #[test]
    fn navigation_and_theme_clicks_include_label_and_both_edges() {
        for (theme_row, x) in [
            (false, 18.),
            (false, 85.),
            (false, 242.),
            (true, 18.),
            (true, 85.),
            (true, 242.),
        ] {
            let ctx = egui::Context::default();
            let frame = |events| {
                let mut clicked = false;
                let output = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(
                            egui::Pos2::ZERO,
                            Vec2::new(260., 80.),
                        )),
                        events,
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            clicked = if theme_row {
                                theme_option(ui, theme::Theme::Midnight, false).clicked()
                            } else {
                                nav_row(ui, "▱", "Infrastructure", 10, false)
                            };
                        });
                    },
                );
                (clicked, output.platform_output.cursor_icon)
            };
            frame(vec![]);
            let pos = egui::pos2(x, 26.);
            let (_, cursor) = frame(vec![egui::Event::PointerMoved(pos)]);
            assert_ne!(cursor, egui::CursorIcon::Text, "text cursor at x={x}");
            frame(vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }]);
            let (clicked, _) = frame(vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }]);
            assert!(clicked, "navigation row ignored click at x={x}");
        }
    }
}
