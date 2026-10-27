//! The connection library: search, the connection table, and its rows.
use super::*;
use egui::{Align, FontId, Layout, Margin, Rect, Sense, UiBuilder};

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
        // Shift+Enter browses an SSH connection's files instead of opening its terminal.
        let browse = ui.input(|i| i.modifiers.shift);
        let enter_idle = focused.is_none()
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));

        // One toolbar: the view's title and count, search, then actions.
        let mut search = None;
        ui.horizontal(|ui| {
            ui.set_min_height(CONTROL_HEIGHT);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                // An empty library offers the same action in its welcome.
                if total > 0
                    && accent_button(ui, "New connection", 0.)
                        .on_hover_text("Ctrl+N")
                        .clicked()
                {
                    self.new_connection();
                }
                if !matches!(self.location, database::Location::Local(_))
                    && outline_button(ui, "Refresh", 0.)
                        .on_hover_text("Load the latest team changes")
                        .clicked()
                {
                    self.refresh();
                }
                let width = (ui.available_width() - 180.).clamp(160., 320.);
                search = Some(
                    search_field(
                        ui,
                        &mut self.query,
                        "Search or type a host",
                        "Ctrl+K",
                        search_id,
                        width,
                    )
                    .on_hover_text(
                        "Arrow keys select · Enter connects · Esc clears\n\
                         Alt+Enter connects to a typed host without saving it",
                    ),
                );
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    let title = self.group.as_deref().unwrap_or(match self.filter {
                        Filter::All => "All connections",
                        Filter::Recent => "Recent",
                        Filter::Favorites => "Favorites",
                        Filter::Rdp => "Remote desktops",
                        Filter::Ssh => "SSH terminals",
                    });
                    let open = self.sessions.iter().filter(|s| s.is_running()).count();
                    let mut summary = if connections.len() == total {
                        total.to_string()
                    } else {
                        format!("{} of {total}", connections.len())
                    };
                    if open > 0 {
                        summary.push_str(&format!(" · {open} open"));
                    }
                    // One line of text, so the title and the smaller count share a baseline.
                    let mut line = egui::text::LayoutJob::default();
                    for part in [
                        RichText::new(title).size(TITLE_SIZE).strong(),
                        RichText::new(format!("  {summary}"))
                            .size(12.)
                            .color(colors.muted),
                    ] {
                        part.append_to(
                            &mut line,
                            ui.style(),
                            egui::FontSelection::Default,
                            Align::BOTTOM,
                        );
                    }
                    ui.add(egui::Label::new(line).truncate());
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
        // `sftp://host` and `sftp host` open a file browser.
        let quick_browses = model::browses(&self.query);
        let submitted = search.lost_focus() && enter;
        let alt = ui.input(|i| i.modifiers.alt);
        if submitted
            && (alt || connections.is_empty())
            && let Some(target) = &quick
        {
            if quick_browses {
                self.request_browse(target.clone());
            } else {
                self.request_connect(target.clone());
            }
            return;
        }
        if (submitted || enter_idle)
            && let Some(c) = connections.get(index.unwrap_or(0))
        {
            self.selected = Some(c.id);
            if browse && c.protocol == Protocol::Ssh {
                self.request_browse(c.clone());
            } else {
                self.request_connect(c.clone());
            }
            return;
        }
        ui.add_space(10.);
        if let Some(target) = &quick {
            let kind = if quick_browses {
                "SFTP"
            } else {
                target.protocol.label()
            };
            let label = format!("{kind} {}", target_label(target));
            if connections.is_empty() {
                ui.horizontal(|ui| {
                    if accent_button(ui, &format!("Connect to {label}"), 0.)
                        .on_hover_text("Enter")
                        .clicked()
                    {
                        if quick_browses {
                            self.request_browse(target.clone());
                        } else {
                            self.request_connect(target.clone());
                        }
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
        ui.add_space((ui.available_height() * 0.22).clamp(16., 160.));
        ui.vertical_centered(|ui| {
            ui.spacing_mut().item_spacing.y = 4.;
            logo(ui, 44., colors.surface, colors.accent);
            ui.add_space(10.);
            ui.label(RichText::new("No connections yet").size(15.).strong());
            ui.label(
                RichText::new("Add a machine, or bring an existing library from CSV.")
                    .color(colors.muted),
            );
            ui.add_space(12.);
            let (new, import) = (140., 110.);
            ui.horizontal(|ui| {
                let gap = ui.spacing().item_spacing.x;
                ui.add_space(((ui.available_width() - new - import - gap) / 2.).max(0.));
                if accent_button(ui, "New connection", new)
                    .on_hover_text("Ctrl+N")
                    .clicked()
                {
                    self.new_connection();
                }
                if outline_button(ui, "Import CSV", import)
                    .on_hover_text("SSH config and .rdp files import from Settings")
                    .clicked()
                {
                    self.import_csv();
                }
            });
            ui.add_space(10.);
            ui.label(
                RichText::new("Or type a host in search and press Enter to connect right away.")
                    .size(11.)
                    .color(colors.muted),
            );
        });
    }

    pub(super) fn connection_table(&mut self, ui: &mut egui::Ui, connections: &[Connection]) {
        let colors = theme::palette(ui.ctx());
        egui::Frame::new()
            .fill(colors.panel)
            .corner_radius(RADIUS_LARGE)
            .stroke(Stroke::new(1_f32, colors.line))
            .inner_margin(Margin::symmetric(8, 2))
            .show(ui, |ui| {
                let width = ui.available_width();
                let columns = table_columns(width);
                ui.spacing_mut().item_spacing.y = 0.;
                let (header, _) = ui.allocate_exact_size(Vec2::new(width, 32.), Sense::hover());
                let mut x = header.min.x;
                for (column, w) in &columns {
                    let galley = egui::WidgetText::from(caption(column.title())).into_galley(
                        ui,
                        Some(egui::TextWrapMode::Truncate),
                        *w,
                        egui::TextStyle::Small,
                    );
                    ui.painter().galley(
                        egui::pos2(x, header.center().y - galley.size().y / 2.),
                        galley,
                        colors.muted,
                    );
                    x += w + GAP;
                }
                ui.painter().line_segment(
                    [header.left_bottom(), header.right_bottom()],
                    Stroke::new(1_f32, colors.line),
                );
                let height = (ui.available_height() - 2.).max(ROW * 3.);
                let mut area = egui::ScrollArea::vertical()
                    .id_salt("connection-list")
                    .max_height(height)
                    // The table ends with its last row instead of filling the window.
                    .auto_shrink([false, true]);
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
                Stroke::new(1_f32, colors.divider),
            );
        }
        let selected = self.selected == Some(c.id);
        // The star, name and buttons take the hover from the row itself, so use the pointer.
        let hovered = ui.rect_contains_pointer(rect);
        if selected || hovered {
            let fill = if selected {
                colors.selected
            } else {
                colors.hover
            };
            let band = rect.shrink2(Vec2::new(0., 2.));
            ui.painter().rect_filled(band, RADIUS, fill);
        }
        if response.clicked() {
            self.selected = Some(c.id);
        }
        if response.double_clicked() {
            self.request_connect(c.clone());
        }
        response.context_menu(|ui| self.connection_menu(ui, c));
        let description = c
            .metadata
            .get("ConnectionDescription")
            .filter(|d| !d.trim().is_empty());
        let line = |ui: &egui::Ui, font: FontId| ui.fonts_mut(|f| f.row_height(&font));
        let mut x = rect.min.x;
        for (column, w) in columns {
            let cell = Rect::from_min_size(egui::pos2(x, rect.min.y), Vec2::new(*w, height));
            x += w + gap;
            // Single-line cells center on the row; two-line cells center as a block.
            let layout = match column {
                Column::Star => Layout::left_to_right(Align::Center).with_main_align(Align::Center),
                Column::Actions => Layout::right_to_left(Align::Center),
                Column::Group => Layout::left_to_right(Align::Center),
                Column::Name | Column::Host | Column::Protocol => Layout::top_down(Align::LEFT),
            };
            // A child ui does not move the row cursor, so rows keep their exact height.
            let ui = &mut ui.new_child(UiBuilder::new().max_rect(cell).layout(layout));
            // Leave room for hover outlines, which extend slightly past a widget.
            ui.set_clip_rect(cell.expand(4.).intersect(ui.clip_rect()));
            ui.spacing_mut().item_spacing = Vec2::new(4., SUBLINE_GAP);
            let center_block = |ui: &mut egui::Ui, lines: &[&FontId]| {
                let block = lines.iter().map(|&f| line(ui, f.clone())).sum::<f32>()
                    + SUBLINE_GAP * (lines.len() - 1) as f32;
                ui.add_space(((height - block) / 2.).floor());
            };
            match column {
                Column::Star => {
                    let (star, response) = ui.allocate_exact_size(Vec2::splat(24.), Sense::click());
                    response.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::WidgetType::Checkbox,
                            true,
                            c.favorite,
                            "Favorite",
                        )
                    });
                    let (icon, color) = match (c.favorite, response.hovered()) {
                        (true, _) => (Icon::StarFilled, colors.accent),
                        (false, true) => (Icon::Star, colors.text),
                        (false, false) => (Icon::Star, colors.muted.gamma_multiply(0.7)),
                    };
                    icons::paint_sized(ui.painter(), icon, star.center(), ICON_SIZE, color);
                    if response
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .on_hover_text("Toggle favorite")
                        .clicked()
                    {
                        let mut next = self.connections.clone();
                        if let Some(item) = next.iter_mut().find(|v| v.id == c.id) {
                            item.favorite = !item.favorite;
                        }
                        self.persist(next);
                    }
                }
                Column::Name => {
                    let name_font = FontId::proportional(13.);
                    let sub_font = FontId::proportional(11.);
                    if description.is_some() {
                        center_block(ui, &[&name_font, &sub_font]);
                    } else {
                        center_block(ui, &[&name_font]);
                    }
                    let name = ui.add(
                        egui::Label::new(RichText::new(&c.name).font(name_font).strong())
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
                                RichText::new(description)
                                    .font(sub_font)
                                    .color(colors.muted),
                            )
                            .truncate(),
                        );
                    }
                }
                Column::Protocol => {
                    // The badge adds a point of padding and a point of outline on each side.
                    ui.add_space(
                        ((height - line(ui, FontId::proportional(10.)) - 4.) / 2.).floor(),
                    );
                    protocol_badge(ui, c.protocol);
                }
                Column::Host => {
                    let host_font = FontId::monospace(12.);
                    let sub_font = FontId::proportional(11.);
                    center_block(ui, &[&host_font, &sub_font]);
                    // The address sits between the name and the muted user in weight.
                    ui.add(
                        egui::Label::new(
                            RichText::new(c.address())
                                .font(host_font)
                                .color(colors.text.lerp_to_gamma(colors.muted, 0.35)),
                        )
                        .truncate(),
                    )
                    .on_hover_text(c.address());
                    let user = if c.username.is_empty() {
                        RichText::new("Ask on connect").italics()
                    } else {
                        RichText::new(&c.username)
                    };
                    ui.add(egui::Label::new(user.font(sub_font).color(colors.muted)).truncate());
                }
                Column::Group => {
                    ui.add(
                        egui::Label::new(RichText::new(&c.group).size(12.).color(colors.muted))
                            .truncate(),
                    )
                    .on_hover_text(&c.group);
                }
                Column::Actions => {
                    let more = icon_button(ui, Icon::More, ACTION_SIZE, "More actions");
                    egui::Popup::menu(&more).show(|ui| self.connection_menu(ui, c));
                    // Connect appears where the pointer or keyboard is; double-click and
                    // Enter connect any row.
                    if selected || hovered {
                        let connect = egui::Button::new(RichText::new("Connect").size(12.))
                            .fill(colors.surface)
                            .stroke(Stroke::new(1_f32, colors.line))
                            .corner_radius(RADIUS);
                        if ui.add_sized([68., ACTION_SIZE], connect).clicked() {
                            self.request_connect(c.clone());
                        }
                        if c.protocol == Protocol::Ssh
                            && icon_button(
                                ui,
                                Icon::Folder,
                                ACTION_SIZE,
                                "Browse files over SFTP (Shift+Enter)",
                            )
                            .clicked()
                        {
                            self.request_browse(c.clone());
                        }
                    }
                }
            }
        }
    }

    /// Actions on a saved connection, from its row's menu or a right-click.
    pub(super) fn connection_menu(&mut self, ui: &mut egui::Ui, c: &Connection) {
        if ui.button("Connect").clicked() {
            self.request_connect(c.clone());
            ui.close();
        }
        if c.protocol == Protocol::Ssh
            && ui
                .button("Browse files")
                .on_hover_text("Open the server's files over SFTP · Shift+Enter")
                .clicked()
        {
            self.request_browse(c.clone());
            ui.close();
        }
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
        if rdp::SAVES_PASSWORDS
            && c.protocol == Protocol::Rdp
            && ui
                .button("Forget saved password")
                .on_hover_text("Removes the credentials Windows saved for this host")
                .clicked()
        {
            match rdp::forget_credentials(&c.host) {
                Ok(true) => self.notify(format!("Removed saved credentials for {}", c.host)),
                Ok(false) => {
                    self.notify(format!("Windows has no saved credentials for {}", c.host))
                }
                Err(e) => self.error = Some(format!("Cannot remove saved credentials: {e}")),
            }
            ui.close();
        }
        ui.separator();
        if ui.button("Delete connection").clicked() {
            self.modal = Modal::Delete(c.id);
            ui.close();
        }
    }
}

pub(super) const ROW: f32 = 40.;

pub(super) const GAP: f32 = 16.;

/// The space between a cell's main line and the muted line under it.
const SUBLINE_GAP: f32 = 2.;

/// The height of the buttons at the end of a row.
const ACTION_SIZE: f32 = 26.;

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
    let mut columns = vec![(Column::Star, 28.), (Column::Name, 0.41)];
    if width >= 560. {
        columns.push((Column::Protocol, 64.));
    }
    columns.push((Column::Host, 0.35));
    if width >= 720. {
        columns.push((Column::Group, 0.24));
    }
    columns.push((Column::Actions, 100.));
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
