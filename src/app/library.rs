//! The connection library: search, the connection table, and its rows.
use super::*;
use egui::{Align, FontId, Layout, Margin, Rect, Sense, UiBuilder};
use std::rc::Rc;

/// What the library shows, kept between frames: a busy terminal or desktop repaints often,
/// and finding, counting and sorting an unchanged library each time would be wasted.
#[derive(Default)]
pub(super) struct Views {
    order: Option<(OrderKey, Rc<Vec<usize>>)>,
    /// Each group with how many connections it has, by name, for the generation given.
    groups: Option<(Generation, Groups)>,
}

/// Everything the shown order depends on.
/// Which connections the views were made from: how often they were replaced, and how many
/// there are, which also notices ones changed directly, as tests and rendering checks do.
type Generation = (u64, usize);

/// Each group with how many connections it has, by name.
type Groups = Rc<Vec<(String, usize)>>;

#[derive(PartialEq)]
struct OrderKey {
    generation: Generation,
    query: String,
    filter: Filter,
    group: Option<String>,
    recent: Vec<u64>,
}

impl App {
    /// Replaces the connections, so views of them are made again.
    pub(super) fn set_connections(&mut self, connections: Vec<Connection>) {
        self.connections = connections;
        self.generation += 1;
    }

    /// Whether the library offers Refresh: a shared one always, and the personal one once a
    /// save was refused, as when another Relay window saved it first.
    pub(super) fn offers_refresh(&self) -> bool {
        !matches!(self.location, database::Location::Local(_)) || self.recovery.is_some()
    }

    fn generation(&self) -> Generation {
        (self.generation, self.connections.len())
    }

    /// The positions in `connections` of those shown, in the order shown.
    pub(super) fn filtered(&self) -> Rc<Vec<usize>> {
        let recent = self.state.recent(&self.location);
        let key = OrderKey {
            generation: self.generation(),
            query: self.query.clone(),
            filter: self.filter,
            group: self.group.clone(),
            recent: recent.to_vec(),
        };
        let mut views = self.views.borrow_mut();
        if let Some((cached, order)) = &views.order
            && *cached == key
        {
            return order.clone();
        }
        let order = Rc::new(self.find_shown(recent));
        views.order = Some((key, order.clone()));
        order
    }

    /// Each group with how many connections it has, by name.
    pub(super) fn groups(&self) -> Groups {
        let generation = self.generation();
        let mut views = self.views.borrow_mut();
        if let Some((cached, groups)) = &views.groups
            && *cached == generation
        {
            return groups.clone();
        }
        let mut counts = std::collections::BTreeMap::<&str, usize>::new();
        for c in &self.connections {
            *counts.entry(&c.group).or_default() += 1;
        }
        let groups: Groups = Rc::new(
            counts
                .into_iter()
                .map(|(group, count)| (group.to_string(), count))
                .collect(),
        );
        views.groups = Some((generation, groups.clone()));
        groups
    }

    fn find_shown(&self, recent: &[u64]) -> Vec<usize> {
        let query = self.query.to_lowercase();
        let mut items: Vec<_> = (0..self.connections.len())
            .filter(|&i| {
                let c = &self.connections[i];
                c.matches(&query)
                    && self.group.as_ref().is_none_or(|g| g == &c.group)
                    && match self.filter {
                        Filter::All => true,
                        Filter::Recent => recent.contains(&c.id),
                        Filter::Favorites => c.favorite,
                        Filter::Rdp => c.protocol == Protocol::Rdp,
                        Filter::Ssh => c.protocol == Protocol::Ssh,
                        Filter::Vnc => c.protocol == Protocol::Vnc,
                    }
            })
            .collect();
        let connections = &self.connections;
        if self.filter == Filter::Recent && self.group.is_none() {
            items.sort_by_cached_key(|&i| recent.iter().position(|id| *id == connections[i].id));
        } else {
            items.sort_by_cached_key(|&i| connections[i].name.to_lowercase());
        }
        items
    }

    pub(super) fn workspace(&mut self, ui: &mut egui::Ui) {
        let colors = theme::palette(ui.ctx());
        let order = self.filtered();
        let total = self.connections.len();
        let search_id = egui::Id::new("library-search");
        let focused = ui.memory(|m| m.focused());
        // Connections chosen together stay chosen while they are shown, so actions on them
        // never reach rows a search or filter hid.
        let shown: Vec<u64> = order.iter().map(|&i| self.connections[i].id).collect();
        // A set to look the chosen ones up in, so choosing thousands stays quick.
        if !self.marked.is_empty() {
            let visible: std::collections::HashSet<u64> = shown.iter().copied().collect();
            self.marked.retain(|id| visible.contains(id));
        }
        if focused.is_none() {
            let (all, clear) = ui.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::COMMAND, egui::Key::A),
                    !self.marked.is_empty()
                        && i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
                )
            });
            if all && shown.len() > 1 {
                self.marked = shown.iter().copied().collect();
            }
            if clear {
                self.marked.clear();
            }
        }
        let mut index = self
            .selected
            .and_then(|id| order.iter().position(|&i| self.connections[i].id == id));
        if !self.query.is_empty() && index.is_none() && !order.is_empty() {
            index = Some(0);
            self.selected = Some(self.connections[order[0]].id);
        }
        // Arrow keys move the selection while typing in search or when nothing has focus.
        if (focused.is_none() || focused == Some(search_id)) && !order.is_empty() {
            let (down, up) = ui.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                )
            });
            let last = order.len() - 1;
            let next = match index {
                _ if !down && !up => None,
                None => Some(if down { 0 } else { last }),
                Some(i) if down => Some((i + 1).min(last)),
                Some(i) => Some(i.saturating_sub(1)),
            };
            if let Some(next) = next {
                index = Some(next);
                self.selected = Some(self.connections[order[next]].id);
                self.scroll_to = true;
                self.marked.clear();
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
                let personal = matches!(self.location, database::Location::Local(_));
                let hint = if personal {
                    "Load the library as last saved, as by another Relay window. \
                     Your unsaved changes can still be exported."
                } else {
                    "Load the latest team changes"
                };
                if self.offers_refresh()
                    && outline_button(ui, "Refresh", 0.)
                        .on_hover_text(hint)
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
                        Filter::Rdp => "RDP desktops",
                        Filter::Ssh => "SSH terminals",
                        Filter::Vnc => "VNC desktops",
                    });
                    let open = self.sessions.iter().filter(|s| s.is_running()).count();
                    let mut summary = if order.len() == total {
                        total.to_string()
                    } else {
                        format!("{} of {total}", order.len())
                    };
                    if open > 0 {
                        summary.push_str(&format!(" · {open} open"));
                    }
                    title_line(ui, title, &summary);
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
            && (alt || order.is_empty())
            && let Some(target) = &quick
        {
            if quick_browses {
                self.request_browse(target.clone());
            } else {
                self.request_connect(target.clone());
            }
            return;
        }
        if enter_idle && self.marked.len() > 1 {
            self.connect_marked(&shown);
            return;
        }
        if (submitted || enter_idle)
            && let Some(&i) = order.get(index.unwrap_or(0))
        {
            let c = self.connections[i].clone();
            self.selected = Some(c.id);
            if browse && c.protocol == Protocol::Ssh {
                self.request_browse(c);
            } else {
                self.request_connect(c);
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
            if order.is_empty() {
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
        if order.is_empty() {
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
        if self.marked.len() > 1 {
            self.selection_bar(ui, &shown);
            ui.add_space(8.);
        }
        self.connection_table(ui, &order);
    }

    /// The marked connections, in the order shown.
    fn marked_in_order(&self, shown: &[u64]) -> Vec<u64> {
        (shown.iter())
            .filter(|id| self.marked.contains(id))
            .copied()
            .collect()
    }

    /// Opens each marked connection in a tab of its own.
    pub(super) fn connect_marked(&mut self, shown: &[u64]) {
        let ids = self.marked_in_order(shown);
        self.connect_many = (ids.iter())
            .filter_map(|id| self.connections.iter().find(|c| c.id == *id).cloned())
            .collect();
        self.ctx.request_repaint();
    }

    /// Favorites every marked connection, or, when they all are already, none of them.
    pub(super) fn favorite_marked(&mut self, shown: &[u64]) {
        let ids = self.marked_in_order(shown);
        let favorite = !self.all_marked_favorite();
        self.change_connections(&ids, |c| c.favorite = favorite);
    }

    fn all_marked_favorite(&self) -> bool {
        (self.connections.iter())
            .filter(|c| self.marked.contains(&c.id))
            .all(|c| c.favorite)
    }

    /// Actions on the connections chosen together, above the table.
    fn selection_bar(&mut self, ui: &mut egui::Ui, shown: &[u64]) {
        let colors = theme::palette(ui.ctx());
        let count = self.marked.len();
        egui::Frame::new()
            .fill(colors.selected)
            .corner_radius(RADIUS_LARGE)
            .inner_margin(Margin::symmetric(12, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.set_min_height(CONTROL_HEIGHT - 6.);
                    ui.label(RichText::new(format!("{count} selected")).strong());
                    ui.add_space(8.);
                    if ui
                        .button(format!("Connect {count}"))
                        .on_hover_text("Opens each in a tab of its own (Enter)")
                        .clicked()
                    {
                        self.connect_marked(shown);
                    }
                    let favorite = if self.all_marked_favorite() {
                        "Remove from favorites"
                    } else {
                        "Add to favorites"
                    };
                    if ui.button(favorite).clicked() {
                        self.favorite_marked(shown);
                    }
                    if ui.button("Move to group…").clicked() {
                        self.modal = Modal::MoveToGroup(self.marked_in_order(shown), String::new());
                    }
                    if ui.button("Delete…").clicked() {
                        self.modal = Modal::Delete(self.marked_in_order(shown));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Clear").on_hover_text("Esc").clicked() {
                            self.marked.clear();
                        }
                        ui.label(
                            RichText::new(format!(
                                "{CLICK_KEY}+click adds or removes · Shift+click selects a range"
                            ))
                            .size(11.)
                            .color(colors.muted),
                        );
                    });
                });
            });
    }

    /// The right-click menu of a row among several chosen together, acting on all of them.
    fn selection_menu(&mut self, ui: &mut egui::Ui, shown: &[u64]) {
        ui.set_min_width(MENU_WIDTH);
        let count = self.marked.len();
        if ui.button(format!("Connect {count}")).clicked() {
            self.connect_marked(shown);
            ui.close();
        }
        menu_separator(ui);
        let favorite = if self.all_marked_favorite() {
            "Remove from favorites"
        } else {
            "Add to favorites"
        };
        if ui.button(favorite).clicked() {
            self.favorite_marked(shown);
            ui.close();
        }
        if ui.button("Move to group…").clicked() {
            self.modal = Modal::MoveToGroup(self.marked_in_order(shown), String::new());
            ui.close();
        }
        menu_separator(ui);
        if ui.button(format!("Delete {count} connections…")).clicked() {
            self.modal = Modal::Delete(self.marked_in_order(shown));
            ui.close();
        }
    }

    /// A click on a row: alone it selects the row, with Ctrl (Cmd on macOS) it adds the row to
    /// those chosen or takes it out, and with Shift it chooses the rows from the selected one.
    pub(super) fn click_row(&mut self, id: u64, modifiers: egui::Modifiers, shown: &[u64]) {
        let at = |id: u64| shown.iter().position(|x| *x == id);
        if modifiers.shift
            && let (Some(from), Some(to)) = (self.selected.and_then(at), at(id))
        {
            self.marked = shown[from.min(to)..=from.max(to)].iter().copied().collect();
            return;
        }
        if modifiers.command {
            if self.marked.is_empty()
                && let Some(current) = self.selected
            {
                self.marked.insert(current);
            }
            if !self.marked.remove(&id) {
                self.marked.insert(id);
            }
        } else {
            self.marked.clear();
        }
        self.selected = Some(id);
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

    /// Lists the connections at positions `order`.
    pub(super) fn connection_table(&mut self, ui: &mut egui::Ui, order: &[usize]) {
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
                // Rows scrolled under the header stop at its line; egui otherwise draws them a few
                // points past the list's edge.
                ui.visuals_mut().clip_rect_margin = 0.;
                let height = (ui.available_height() - 2.).max(ROW * 3.);
                let mut area = egui::ScrollArea::vertical()
                    .id_salt("connection-list")
                    .max_height(height)
                    // The table ends with its last row instead of filling the window.
                    .auto_shrink([false, true]);
                let selected = self
                    .selected
                    .and_then(|id| order.iter().position(|&i| self.connections[i].id == id));
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
                let shown: Vec<u64> = order.iter().map(|&i| self.connections[i].id).collect();
                let output = area.show_rows(ui, ROW, order.len(), |ui, range| {
                    // Only the rows on screen are drawn, and copied to draw them.
                    for index in range {
                        let divider = index + 1 < order.len();
                        let c = self.connections[order[index]].clone();
                        self.connection_row(ui, &c, &columns, divider, &shown);
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
        shown: &[u64],
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
        // Rows chosen together are highlighted; otherwise the selected one is.
        let selected = if self.marked.is_empty() {
            self.selected == Some(c.id)
        } else {
            self.marked.contains(&c.id)
        };
        let modifiers = ui.input(|i| i.modifiers);
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
            self.click_row(c.id, modifiers, shown);
        }
        if response.double_clicked() && !modifiers.command && !modifiers.shift {
            self.request_connect(c.clone());
        }
        // A right-click on a row among those chosen acts on all of them.
        let together = self.marked.len() > 1 && self.marked.contains(&c.id);
        menu_popup(egui::Popup::context_menu(&response)).show(|ui| {
            if together {
                self.selection_menu(ui, shown)
            } else {
                self.connection_menu(ui, c)
            }
        });
        // The first line of the notes sits under the name; hovering it shows them all.
        let description = c.notes.lines().map(str::trim).find(|l| !l.is_empty());
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
                        self.click_row(c.id, modifiers, shown);
                    }
                    if name.double_clicked() && !modifiers.command && !modifiers.shift {
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
                        )
                        .on_hover_text(&c.notes);
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
                    // Keep the buttons inside the row's highlight, clear of its rounded end.
                    ui.add_space(ACTION_INSET);
                    let more = icon_button(ui, Icon::More, ACTION_SIZE, "More actions");
                    menu_popup(egui::Popup::menu(&more)).show(|ui| self.connection_menu(ui, c));
                    // Connect appears where the pointer or keyboard is; double-click and
                    // Enter connect any row.
                    // Rows chosen together connect from the bar above the table.
                    if (selected && self.marked.is_empty()) || hovered {
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
    /// Grouped as opening, changing, then removing; styled by `menu_popup`.
    pub(super) fn connection_menu(&mut self, ui: &mut egui::Ui, c: &Connection) {
        let colors = theme::palette(ui.ctx());
        ui.set_min_width(MENU_WIDTH);
        if ui.add(menu_item("Connect", "Enter", &colors)).clicked() {
            self.request_connect(c.clone());
            ui.close();
        }
        if c.protocol == Protocol::Rdp
            && ui
                .add_enabled(
                    rdp::ADMIN_SESSIONS,
                    egui::Button::new("Connect to admin session"),
                )
                .on_hover_text(
                    "Like mstsc /admin: signs in to the server's administrative session, \
                     which needs no Remote Desktop license and bypasses the connection broker",
                )
                .on_disabled_hover_text("Not supported on this platform")
                .clicked()
        {
            self.request_connect_admin(c.clone());
            ui.close();
        }
        if c.protocol == Protocol::Ssh
            && ui
                .add(menu_item("Browse files", "Shift+Enter", &colors))
                .on_hover_text("Open the server's files over SFTP")
                .clicked()
        {
            self.request_browse(c.clone());
            ui.close();
        }
        menu_separator(ui);
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
        menu_separator(ui);
        if ui.button("Delete connection").clicked() {
            self.modal = Modal::Delete(vec![c.id]);
            ui.close();
        }
    }
}

/// A view's title with its count beside it in smaller text, the two sharing a baseline.
fn title_line(ui: &mut egui::Ui, title: &str, count: &str) {
    let colors = theme::palette(ui.ctx());
    let width = ui.available_width();
    let galley = |ui: &egui::Ui, text: RichText, width: f32| {
        egui::WidgetText::from(text).into_galley(
            ui,
            Some(egui::TextWrapMode::Truncate),
            width.max(0.),
            egui::TextStyle::Body,
        )
    };
    let title = galley(ui, RichText::new(title).size(TITLE_SIZE).strong(), width);
    let gap = 8.;
    let count = galley(
        ui,
        RichText::new(count).size(12.).color(colors.muted),
        width - title.size().x - gap,
    );
    // Where the first line's letters sit, from the top of a galley.
    let baseline = |g: &egui::Galley| {
        g.rows
            .first()
            .and_then(|row| row.row.glyphs.first().map(|glyph| row.pos.y + glyph.pos.y))
            .unwrap_or(g.size().y)
    };
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(
            (title.size().x + gap + count.size().x).min(width),
            title.size().y,
        ),
        Sense::hover(),
    );
    let top = rect.top();
    let shift = baseline(&title) - baseline(&count);
    let count_pos = egui::pos2(rect.left() + title.size().x + gap, top + shift);
    ui.painter().galley(rect.left_top(), title, colors.text);
    if count_pos.x < rect.right() {
        ui.painter().galley(count_pos, count, colors.muted);
    }
}

/// The key held to add a row to those chosen.
const CLICK_KEY: &str = if cfg!(target_os = "macos") {
    "Cmd"
} else {
    "Ctrl"
};

pub(super) const ROW: f32 = 40.;

pub(super) const GAP: f32 = 16.;

/// The space between a cell's main line and the muted line under it.
const SUBLINE_GAP: f32 = 2.;

/// The height of the buttons at the end of a row.
const ACTION_SIZE: f32 = 26.;

/// The space between the last button in a row and the end of its highlight.
const ACTION_INSET: f32 = 10.;

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
    // More, Connect, and for SSH the file browser button.
    columns.push((Column::Actions, 128. + ACTION_INSET));
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
