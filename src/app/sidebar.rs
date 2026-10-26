//! The navigation sidebar: filters, groups, and settings.
use super::*;
use egui::{Align, Align2, FontId, Layout, Margin, Rect, Sense, UiBuilder};

impl App {
    pub(super) fn sidebar(&mut self, ctx: &egui::Context) {
        let colors = theme::palette(ctx);
        let collapsed = self.sidebar_collapsed(ctx);
        let margin = if collapsed { 8 } else { 10 };
        egui::SidePanel::left("sidebar")
            .exact_width(if collapsed { 52. } else { 224. })
            .resizable(false)
            .frame(egui::Frame::new().fill(colors.panel).inner_margin(Margin {
                left: margin,
                right: margin,
                top: 0,
                bottom: 8,
            }))
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 2.;
                // The header is as tall as the tab bar, so the two bottom edges form one line.
                let (header, toggle) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), BAR_HEIGHT),
                    Sense::click(),
                );
                let edge = ui.max_rect().x_range().expand(margin as f32);
                ui.painter()
                    .hline(edge, header.bottom(), Stroke::new(1_f32, colors.line));
                let mark = if collapsed {
                    header.center()
                } else {
                    egui::pos2(header.left() + 15., header.center().y)
                };
                ui.put(
                    Rect::from_center_size(mark, Vec2::splat(22.)),
                    |ui: &mut egui::Ui| {
                        logo(ui, 22., colors.accent, colors.bg);
                        ui.response()
                    },
                );
                if !collapsed {
                    ui.painter().text(
                        egui::pos2(header.left() + 34., header.center().y),
                        Align2::LEFT_CENTER,
                        "Relay",
                        FontId::proportional(14.),
                        colors.text,
                    );
                    icons::paint_sized(
                        ui.painter(),
                        Icon::Sidebar,
                        egui::pos2(header.right() - 12., header.center().y),
                        14.,
                        if toggle.hovered() {
                            colors.text
                        } else {
                            colors.muted
                        },
                    );
                }
                let toggle = toggle
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text(if collapsed {
                        "Expand sidebar (Ctrl+B)"
                    } else {
                        "Collapse sidebar (Ctrl+B)"
                    });
                if toggle.clicked() {
                    self.set_sidebar_collapsed(!collapsed);
                }
                ui.add_space(12.);
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
                    (
                        Filter::All,
                        Icon::Library,
                        "All connections",
                        self.connections.len(),
                    ),
                    (Filter::Recent, Icon::Recent, "Recent", recent),
                    (Filter::Favorites, Icon::Star, "Favorites", favorites),
                    (
                        Filter::Rdp,
                        Icon::Desktop,
                        "Remote desktop",
                        count(Protocol::Rdp),
                    ),
                    (
                        Filter::Ssh,
                        Icon::Terminal,
                        "SSH terminal",
                        count(Protocol::Ssh),
                    ),
                ];
                let mut chosen = None;
                if collapsed {
                    for (filter, icon, label, count) in filters {
                        let selected = self.filter == filter && self.group.is_none();
                        if rail_row(ui, icon, &format!("{label} ({count})"), selected) {
                            chosen = Some(filter);
                        }
                    }
                } else {
                    for (index, (filter, icon, label, count)) in filters.into_iter().enumerate() {
                        if index == 3 {
                            ui.add_space(12.);
                            section_label(ui, "PROTOCOLS");
                        }
                        let selected = self.filter == filter && self.group.is_none();
                        if nav_row(ui, icon, label, count, selected) {
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
                    ui.add_space(12.);
                    section_label(ui, "GROUPS");
                    let mut groups = std::collections::BTreeMap::<String, usize>::new();
                    for c in &self.connections {
                        *groups.entry(c.group.clone()).or_default() += 1;
                    }
                    egui::ScrollArea::vertical()
                        .id_salt("groups")
                        .max_height((ui.available_height() - 52.).max(60.))
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 2.;
                            if groups.is_empty() {
                                ui.label(
                                    RichText::new("Groups appear as you add connections.")
                                        .size(12.)
                                        .color(colors.muted),
                                );
                            }
                            for (group, count) in groups {
                                let selected = self.group.as_ref() == Some(&group);
                                if nav_row(ui, Icon::Folder, &group, count, selected) {
                                    self.group = Some(group);
                                    self.filter = Filter::All;
                                    self.active = None;
                                }
                            }
                        });
                }
                ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
                    if collapsed {
                        if rail_row(ui, Icon::Settings, "Settings", false) {
                            self.modal = Modal::Settings;
                        }
                        return;
                    }
                    // The library in use, which opens library management, beside Settings.
                    let (row, _) = ui
                        .allocate_exact_size(Vec2::new(ui.available_width(), 30.), Sense::hover());
                    ui.painter()
                        .hline(edge, row.top() - 6., Stroke::new(1_f32, colors.line));
                    let settings = ui
                        .put(
                            Rect::from_center_size(
                                egui::pos2(row.right() - 14., row.center().y),
                                Vec2::splat(28.),
                            ),
                            |ui: &mut egui::Ui| icon_button(ui, Icon::Settings, 28., "Settings"),
                        )
                        .clicked();
                    if settings {
                        self.modal = Modal::Settings;
                    }
                    let library_rect =
                        Rect::from_min_max(row.min, egui::pos2(row.right() - 32., row.max.y));
                    let idle = self.pending.is_none();
                    let library = ui.interact(
                        library_rect,
                        ui.id().with("library"),
                        if idle { Sense::click() } else { Sense::hover() },
                    );
                    if library.hovered() && idle {
                        ui.painter().rect_filled(library_rect, 6., colors.hover);
                    }
                    ui.painter().circle_filled(
                        egui::pos2(library_rect.left() + 12., row.center().y),
                        3.5,
                        if idle { colors.accent } else { colors.muted },
                    );
                    let label = if idle {
                        self.location.label()
                    } else {
                        "Updating library…"
                    };
                    let galley = egui::WidgetText::from(RichText::new(label).size(12.))
                        .into_galley(
                            ui,
                            Some(egui::TextWrapMode::Truncate),
                            library_rect.width() - 30.,
                            egui::TextStyle::Body,
                        );
                    ui.painter().galley(
                        egui::pos2(
                            library_rect.left() + 24.,
                            row.center().y - galley.size().y / 2.,
                        ),
                        galley,
                        colors.text,
                    );
                    let mut tip =
                        String::from("Connection profiles only · Click to manage library");
                    if let database::Location::Local(path) | database::Location::Sqlite(path) =
                        &self.location
                    {
                        tip = format!("{}\n{tip}", path.display());
                    }
                    if library
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .on_hover_text(tip)
                        .clicked()
                    {
                        self.modal = Modal::Library;
                    }
                });
            });
    }
}

pub(super) fn nav_row(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    count: usize,
    selected: bool,
) -> bool {
    let colors = theme::palette(ui.ctx());
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 28.), Sense::click());
    if selected || response.hovered() {
        ui.painter().rect_filled(
            rect,
            5.,
            if selected {
                colors.selected
            } else {
                colors.hover
            },
        );
    }
    let (icon_color, color) = if selected {
        (colors.accent, colors.text)
    } else {
        (colors.muted, colors.text)
    };
    icons::paint_sized(
        ui.painter(),
        icon,
        egui::pos2(rect.left() + 15., rect.center().y),
        15.,
        icon_color,
    );
    let text_rect =
        Rect::from_min_max(rect.min + Vec2::new(32., 0.), rect.max - Vec2::new(30., 0.));
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(text_rect)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            ui.add(
                egui::Label::new(RichText::new(label).size(13.).color(color))
                    .truncate()
                    .selectable(false),
            );
        },
    );
    // Empty filters show no count, which keeps the column quiet.
    if count > 0 {
        ui.painter().text(
            egui::pos2(rect.right() - 8., rect.center().y),
            Align2::RIGHT_CENTER,
            count.to_string(),
            FontId::proportional(11.),
            colors.muted,
        );
    }
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(label)
        .clicked()
}

pub(super) fn rail_row(ui: &mut egui::Ui, icon: Icon, tooltip: &str, selected: bool) -> bool {
    let colors = theme::palette(ui.ctx());
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 34.), Sense::click());
    if selected || response.hovered() {
        let fill = if selected {
            colors.selected
        } else {
            colors.hover
        };
        ui.painter().rect_filled(rect, 6., fill);
    }
    let color = if selected {
        colors.accent
    } else {
        colors.muted
    };
    icons::paint(ui.painter(), icon, rect.center(), color);
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(tooltip)
        .clicked()
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
