//! The connection library: search, the connection table, and its rows.
use super::*;
use egui::{Align, FontId, Layout, Margin, Rect, Sense, UiBuilder};
use std::rc::Rc;

/// What the library shows, kept between frames: a busy terminal or desktop repaints often,
/// and finding, counting and sorting an unchanged library each time would be wasted.
#[derive(Default)]
pub(super) struct Views {
    order: Option<(OrderKey, Shown)>,
    /// Marked connections were checked against the order last made, so none is hidden.
    reconciled: bool,
    /// Each group with how many connections it has, by name, for the generation given.
    groups: Option<(Generation, Groups)>,
    /// The sidebar's counts, for the generation and recent connections given.
    counts: Option<((Generation, Vec<u64>), Counts)>,
}

/// How many connections each of the sidebar's filters shows.
#[derive(Clone, Copy, Default)]
pub(super) struct Counts {
    pub recent: usize,
    pub favorites: usize,
    pub rdp: usize,
    pub ssh: usize,
    pub vnc: usize,
}

/// Everything the shown order depends on.
/// Which connections the views were made from: how often they were replaced, and how many
/// there are, which also notices ones changed directly, as tests and rendering checks do.
type Generation = (u64, usize);

/// The positions in `connections` of those shown, and their IDs, in the order shown.
type Shown = (Rc<Vec<usize>>, Rc<Vec<u64>>);

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
        self.shown().0
    }

    /// The IDs of the connections shown, in the order shown.
    fn shown_ids(&self) -> Rc<Vec<u64>> {
        self.shown().1
    }

    fn shown(&self) -> Shown {
        let recent = self.state.recent(&self.location);
        let key = OrderKey {
            generation: self.generation(),
            query: self.query.clone(),
            filter: self.filter,
            group: self.group.clone(),
            recent: recent.to_vec(),
        };
        let mut views = self.views.borrow_mut();
        if let Some((cached, shown)) = &views.order
            && *cached == key
        {
            return shown.clone();
        }
        let order = Rc::new(self.find_shown(recent));
        let ids: Rc<Vec<u64>> = Rc::new(order.iter().map(|&i| self.connections[i].id).collect());
        views.order = Some((key, (order.clone(), ids.clone())));
        views.reconciled = false;
        (order, ids)
    }

    /// How many connections each of the sidebar's filters shows.
    pub(super) fn counts(&self) -> Counts {
        let recent = self.state.recent(&self.location);
        let key = (self.generation(), recent.to_vec());
        let mut views = self.views.borrow_mut();
        if let Some((cached, counts)) = &views.counts
            && *cached == key
        {
            return *counts;
        }
        let mut counts = Counts::default();
        for c in &self.connections {
            counts.recent += usize::from(recent.contains(&c.id));
            counts.favorites += usize::from(c.favorite);
            match c.protocol {
                Protocol::Rdp => counts.rdp += 1,
                Protocol::Ssh => counts.ssh += 1,
                Protocol::Vnc => counts.vnc += 1,
            }
        }
        views.counts = Some((key, counts));
        counts
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
        let total = self.connections.len();
        let search_id = egui::Id::new("library-search");
        let focused = ui.memory(|m| m.focused());
        // One toolbar: the view's title and count, search, then actions.
        let mut search = None;
        let mut order = None;
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
                    let shown = self.filtered();
                    let open = self.sessions.iter().filter(|s| s.is_running()).count();
                    let mut summary = if shown.len() == total {
                        total.to_string()
                    } else {
                        format!("{} of {total}", shown.len())
                    };
                    order = Some(shown);
                    if open > 0 {
                        summary.push_str(&format!(" · {open} open"));
                    }
                    title_line(ui, title, &summary);
                });
            });
        });
        let order = order.unwrap();
        // Connections chosen together stay chosen while they are shown, so actions on them
        // never reach rows a search or filter hid. Only shown rows can be chosen, so this is
        // needed only when what is shown changes.
        let shown = self.shown_ids();
        let reconciled = std::mem::replace(&mut self.views.get_mut().reconciled, true);
        if !reconciled && !self.marked.is_empty() {
            // A set to look the chosen ones up in, so choosing thousands stays quick.
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
            // The selection may be a row a search or filter now hides, which is never chosen.
            if self.marked.is_empty()
                && let Some(current) = self.selected
                && shown.contains(&current)
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
                let columns = table_columns(width, self.state.library_columns);
                ui.spacing_mut().item_spacing.y = 0.;
                let (header, _) = ui.allocate_exact_size(Vec2::new(width, 32.), Sense::hover());
                let mut x = header.min.x;
                for (index, (column, w)) in columns.iter().enumerate() {
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
                    ui.interact(
                        Rect::from_min_size(
                            egui::pos2(x, header.top()),
                            Vec2::new(*w, header.height()),
                        ),
                        ui.id().with(("column-title", index)),
                        Sense::hover(),
                    )
                    .on_hover_text(column.title());
                    x += w + GAP;
                    if let Some(slot) = column.slot().filter(|&slot| {
                        slot < 3 && (slot != 2 || columns.iter().any(|c| c.0 == Column::Group))
                    }) {
                        let handle = Rect::from_center_size(
                            egui::pos2(x - GAP / 2., header.center().y),
                            Vec2::new(8., header.height()),
                        );
                        let response = ui
                            .interact(
                                handle,
                                ui.id().with(("column", slot)),
                                Sense::click_and_drag(),
                            )
                            .on_hover_cursor(egui::CursorIcon::ResizeHorizontal)
                            .on_hover_text("Drag to resize · Double-click to reset");
                        ui.painter().vline(
                            handle.center().x,
                            header.y_range().shrink(8.),
                            Stroke::new(
                                1_f32,
                                if response.hovered() || response.dragged() {
                                    colors.muted
                                } else {
                                    colors.line
                                },
                            ),
                        );
                        if response.dragged() {
                            let delta = ui.input(|i| i.pointer.delta().x);
                            self.state.library_columns = Some(resize_column(
                                &columns,
                                slot,
                                delta,
                                self.state.library_columns,
                            ));
                        }
                        if response.double_clicked() {
                            self.state.library_columns =
                                Some(reset_column(&columns, slot, self.state.library_columns));
                            self.ctx.request_repaint();
                        }
                        if response.drag_stopped() || response.double_clicked() {
                            self.save_state();
                        }
                    }
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
                let shown = self.shown_ids();
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
        response.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::SelectableLabel,
                ui.is_enabled(),
                selected,
                format!("{}, {}, {}", c.name, c.protocol.label(), target_label(c)),
            )
        });
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
        if response.has_focus() {
            ui.painter().rect_stroke(
                rect.shrink2(Vec2::new(0., 2.)),
                RADIUS,
                Stroke::new(1_f32, colors.accent),
                egui::StrokeKind::Inside,
            );
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
                            format!("Favorite {}", c.name),
                        )
                    });
                    if response.has_focus() {
                        ui.painter().rect_stroke(
                            star,
                            RADIUS_SMALL,
                            Stroke::new(1_f32, colors.accent),
                            egui::StrokeKind::Inside,
                        );
                    }
                    let (icon, color) =
                        match (c.favorite, response.hovered() || response.has_focus()) {
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
                    let name_font = FontId::proportional(14.);
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
                    if c.username.is_empty() {
                        center_block(ui, &[&host_font]);
                    } else {
                        center_block(ui, &[&host_font, &sub_font]);
                    }
                    // The address sits between the name and the muted user in weight.
                    ui.add(
                        egui::Label::new(
                            RichText::new(c.address())
                                .font(host_font)
                                .color(colors.text),
                        )
                        .truncate(),
                    )
                    .on_hover_text(c.address());
                    if !c.username.is_empty() {
                        ui.add(
                            egui::Label::new(
                                RichText::new(&c.username)
                                    .font(sub_font)
                                    .color(colors.muted),
                            )
                            .truncate(),
                        )
                        .on_hover_text(&c.username);
                    }
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
                    if *w >= 128. && ((selected && self.marked.is_empty()) || hovered) {
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
        let can_forget = match c.protocol {
            Protocol::Rdp => rdp::SAVES_PASSWORDS,
            Protocol::Vnc | Protocol::Ssh => true,
        };
        let forget_tip = if cfg!(windows) && c.protocol == Protocol::Rdp {
            "Removes the credentials Windows saved for this host"
        } else {
            "Removes the password Relay saved in your system credential store"
        };
        if can_forget
            && ui
                .button("Forget saved password")
                .on_hover_text(forget_tip)
                .clicked()
        {
            match self.forget_saved_password(c) {
                Ok(true) => self.notify(format!("Removed saved password for {}", c.name)),
                Ok(false) => self.notify(format!("No saved password for {}", c.name)),
                Err(e) => self.error = Some(format!("Cannot remove saved password: {e}")),
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
    fn slot(self) -> Option<usize> {
        match self {
            Self::Name => Some(0),
            Self::Protocol => Some(1),
            Self::Host => Some(2),
            Self::Group => Some(3),
            _ => None,
        }
    }

    fn flexible(self) -> bool {
        matches!(self, Self::Name | Self::Host | Self::Group)
    }

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

/// Narrow tables drop group and protocol; saved preferences survive these temporary changes.
pub(super) fn table_columns(width: f32, saved: Option<[f32; 4]>) -> Vec<(Column, f32)> {
    let preferred = column_preferences(saved);
    let mut columns = vec![(Column::Star, 28.), (Column::Name, preferred[0])];
    if width >= 560. {
        columns.push((Column::Protocol, preferred[1]));
    }
    columns.push((Column::Host, preferred[2]));
    if width >= 720. {
        columns.push((Column::Group, preferred[3]));
    }
    columns.push((
        Column::Actions,
        if width >= 560. {
            128. + ACTION_INSET
        } else {
            36.
        },
    ));
    let fixed: f32 = columns
        .iter()
        .filter(|(c, _)| !c.flexible())
        .map(|c| c.1)
        .sum();
    let flexible = (width - fixed - GAP * (columns.len() - 1) as f32).max(0.);
    let count = columns.iter().filter(|(c, _)| c.flexible()).count() as f32;
    let minimum = (flexible / count).min(48.);
    let weights: f32 = columns
        .iter()
        .filter(|(c, _)| c.flexible())
        .map(|c| c.1)
        .sum();
    for (column, w) in &mut columns {
        if column.flexible() {
            *w = minimum + (flexible - minimum * count) * *w / weights;
        }
    }
    columns
}

fn column_preferences(saved: Option<[f32; 4]>) -> [f32; 4] {
    saved
        .filter(|values| {
            values
                .iter()
                .all(|v| v.is_finite() && *v > 0. && *v <= 10000.)
        })
        .map(|mut values| {
            values[1] = values[1].clamp(48., 120.);
            values
        })
        .unwrap_or([0.45, 64., 0.40, 0.15])
}

/// Restore the divider's pair without changing the other columns' preferences.
fn reset_column(columns: &[(Column, f32)], slot: usize, saved: Option<[f32; 4]>) -> [f32; 4] {
    let defaults = column_preferences(None);
    if slot == 1 {
        let protocol = columns.iter().find(|c| c.0 == Column::Protocol).unwrap().1;
        return resize_column(columns, slot, defaults[1] - protocol, saved);
    }
    let mut preferred = column_preferences(saved);
    let donor = if slot == 2 { 3 } else { 2 };
    let total = preferred[slot] + preferred[donor];
    preferred[slot] = total * defaults[slot] / (defaults[slot] + defaults[donor]);
    preferred[donor] = total - preferred[slot];
    preferred
}

/// Transfer space from another text column, keeping the table within the window.
fn resize_column(
    columns: &[(Column, f32)],
    slot: usize,
    delta: f32,
    saved: Option<[f32; 4]>,
) -> [f32; 4] {
    let mut preferred = column_preferences(saved);
    let donor = if slot == 2 { 3 } else { 2 };
    let width = |slot| {
        columns
            .iter()
            .find(|(c, _)| c.slot() == Some(slot))
            .unwrap()
            .1
    };
    let change = delta.clamp((48. - width(slot)).min(0.), (width(donor) - 48.).max(0.));
    let change = if slot == 1 {
        change.min(120. - width(slot))
    } else {
        change
    };
    let weight: f32 = columns
        .iter()
        .filter(|(c, _)| c.flexible())
        .map(|(c, _)| preferred[c.slot().unwrap()])
        .sum();
    let space: f32 = columns
        .iter()
        .filter(|(c, _)| c.flexible())
        .map(|(_, w)| w - 48.)
        .sum();
    for (column, w) in columns {
        if let Some(i) = column.slot() {
            let adjusted = *w
                + if i == slot {
                    change
                } else if i == donor {
                    -change
                } else {
                    0.
                };
            preferred[i] = if i == 1 {
                adjusted
            } else {
                (adjusted - 48.).max(0.0001) / space.max(0.0001) * weight
            };
        }
    }
    preferred
}

/// A connection's user and address, as typed for quick connect.
pub(super) fn target_label(c: &Connection) -> String {
    if c.username.is_empty() {
        c.address()
    } else {
        format!("{}@{}", c.username, c.address())
    }
}

#[cfg(test)]
mod column_tests {
    use super::*;

    #[test]
    fn resizing_each_column_preserves_the_table_width_and_other_columns() {
        let columns = table_columns(1000., None);
        for slot in 0..4 {
            for delta in [-10000., -20., 20., 10000.] {
                let saved = resize_column(&columns, slot, delta, None);
                let resized = table_columns(1000., Some(saved));
                let donor = if slot == 2 { 3 } else { 2 };
                for ((column, before), (_, after)) in columns.iter().zip(&resized) {
                    assert!(*after >= 28. && after.is_finite());
                    if column.slot() != Some(slot) && column.slot() != Some(donor) {
                        assert!((before - after).abs() < 0.01, "{slot}: {column:?}");
                    }
                }
                assert!((resized.iter().map(|c| c.1).sum::<f32>() + GAP * 5. - 1000.).abs() < 0.01);
            }
        }
    }

    #[test]
    fn resetting_a_divider_restores_default_proportions_and_preserves_other_columns() {
        for width in [500., 700., 1000.] {
            let saved = [0.7, 90., 0.2, 0.1];
            let columns = table_columns(width, Some(saved));
            for slot in 0..3 {
                if !columns.iter().any(|(c, _)| c.slot() == Some(slot))
                    || (slot == 2 && width < 720.)
                {
                    continue;
                }
                let reset = reset_column(&columns, slot, Some(saved));
                let restored = table_columns(width, Some(reset));
                let donor = if slot == 2 { 3 } else { 2 };
                for ((column, before), (_, after)) in columns.iter().zip(&restored) {
                    if column.slot() != Some(slot) && column.slot() != Some(donor) {
                        assert!((before - after).abs() < 0.01);
                    }
                }
                if slot == 1 {
                    assert_eq!(reset[1], 64.);
                } else {
                    let defaults = column_preferences(None);
                    assert!(
                        (reset[slot] / reset[donor] - defaults[slot] / defaults[donor]).abs()
                            < 0.001
                    );
                }
            }
        }
    }

    #[test]
    fn invalid_preferences_fall_back_and_narrow_windows_keep_saved_sizes() {
        for values in [[f32::NAN; 4], [-1.; 4], [0.; 4], [f32::MAX; 4]] {
            assert_eq!(table_columns(900., Some(values)), table_columns(900., None));
        }
        let saved = resize_column(&table_columns(1000., None), 0, 50., None);
        let narrow_saved = resize_column(&table_columns(500., Some(saved)), 0, 20., Some(saved));
        assert_eq!(narrow_saved[3], saved[3]);
        assert_eq!(narrow_saved[1], saved[1]);
        for width in [240., 400., 559., 560., 719., 720., 1400.] {
            let columns = table_columns(width, Some(saved));
            assert!(columns.iter().all(|c| c.1 > 0.));
            assert!(
                (columns.iter().map(|c| c.1).sum::<f32>() + GAP * (columns.len() - 1) as f32
                    - width)
                    .abs()
                    < 0.01
            );
        }
    }
}
