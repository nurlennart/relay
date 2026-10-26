//! The navigation sidebar: filters, groups, and settings.
use super::*;
use egui::{Align, Align2, FontId, Layout, Margin, Rect, Sense, UiBuilder};

impl App {
    pub(super) fn sidebar(&mut self, ctx: &egui::Context) {
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
                    logo(ui, 40., colors.accent, colors.bg);
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
                    self.set_sidebar_collapsed(!collapsed);
                }
                ui.add_space(if collapsed { 20. } else { 28. });
                let favorites = self.connections.iter().filter(|c| c.favorite).count();
                let recent = self.state.recent(&self.location);
                let recent = self
                    .connections
                    .iter()
                    .filter(|c| recent.contains(&c.id))
                    .count();
                let count =
                    |p: Protocol| self.connections.iter().filter(|c| c.protocol == p).count();
                let filters = [
                    (Filter::All, "▦", "All connections", self.connections.len()),
                    (Filter::Recent, "◷", "Recent", recent),
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
                        } else if index == 3 {
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
                        .max_height((ui.available_height() - 250.).max(60.))
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
                            // The footer stacks bottom-up, so the card's rows are added last
                            // to first.
                            ui.set_width(ui.available_width());
                            let manage = egui::Button::new(
                                RichText::new("Manage library  →")
                                    .size(12.)
                                    .color(colors.accent),
                            )
                            .frame(false);
                            if ui.add_enabled(self.pending.is_none(), manage).clicked() {
                                self.modal = Modal::Library;
                            }
                            ui.label(
                                RichText::new(if self.pending.is_some() {
                                    "Updating library…"
                                } else {
                                    "Connection profiles only"
                                })
                                .size(11.)
                                .color(colors.muted),
                            );
                            ui.horizontal(|ui| {
                                ui.colored_label(colors.accent, "●");
                                ui.label(RichText::new(self.location.label()).size(12.).strong());
                            });
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
}

pub(super) fn nav_row(
    ui: &mut egui::Ui,
    symbol: &str,
    label: &str,
    count: usize,
    selected: bool,
) -> bool {
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

pub(super) fn rail_row(ui: &mut egui::Ui, symbol: &str, tooltip: &str, selected: bool) -> bool {
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

pub(super) fn nav_icon(ui: &egui::Ui, symbol: &str, center: egui::Pos2, color: Color32) {
    match symbol {
        "▦" => library_icon(ui, center, color),
        "◷" => {
            let stroke = Stroke::new(1.25_f32, color);
            ui.painter().circle_stroke(center, 6.5, stroke);
            ui.painter()
                .line_segment([center, center + Vec2::new(0., -4.)], stroke);
            ui.painter()
                .line_segment([center, center + Vec2::new(3., 1.5)], stroke);
        }
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

/// Relay's mark, matching assets/make_icon.py: a remote window with a prompt, and a second
/// window behind it.
pub(super) fn logo(ui: &mut egui::Ui, size: f32, tile: Color32, mark: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let painter = ui.painter();
    let at = |x: f32, y: f32| rect.min + Vec2::new(x * size, y * size);
    let round = size * 0.07;
    let stroke = Stroke::new(size * 0.06, mark);
    painter.rect_filled(rect, size * 0.24, tile);
    let offset = 0.085;
    painter.rect_stroke(
        Rect::from_min_max(
            at(0.16 + offset, 0.30 - offset),
            at(0.76 + offset, 0.79 - offset),
        ),
        round,
        Stroke::new(stroke.width, mark.gamma_multiply(0.45)),
        egui::StrokeKind::Middle,
    );
    let front = Rect::from_min_max(at(0.16, 0.30), at(0.76, 0.79));
    painter.rect_filled(front, round, tile);
    painter.rect_stroke(front, round, stroke, egui::StrokeKind::Middle);
    let bar = 0.42;
    painter.line_segment([at(0.16, bar), at(0.76, bar)], stroke);
    let (x, y, arm) = (0.30, (bar + 0.79) / 2. + 0.01, 0.085);
    painter.add(egui::Shape::line(
        vec![at(x, y - arm), at(x + arm, y), at(x, y + arm)],
        stroke,
    ));
    painter.line_segment([at(0.48, y + arm), at(0.62, y + arm)], stroke);
}

pub(super) fn library_icon(ui: &egui::Ui, center: egui::Pos2, color: Color32) {
    for y in 0..2 {
        for x in 0..2 {
            let min = center + Vec2::new(-6.5 + x as f32 * 8., -6.5 + y as f32 * 8.);
            ui.painter()
                .rect_filled(Rect::from_min_size(min, Vec2::splat(5.)), 1., color);
        }
    }
}
