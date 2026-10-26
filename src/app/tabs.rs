//! The tab strip above the workspace.
use super::*;
use egui::{FontId, Margin, Rect, Sense};

impl App {
    pub(super) fn tabs(&mut self, ctx: &egui::Context) {
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
                            // Tabs can be dragged; the drop position follows the pointer.
                            let mut rects = vec![];
                            let mut split_with = None;
                            let mut window_action = None;
                            let mut dragging = None;
                            let mut dropped = None;
                            for (index, s) in self.sessions.iter().enumerate() {
                                ui.push_id(index, |ui| {
                                    let session = session_tab(
                                        ui,
                                        &s.connection,
                                        s.tab_state(),
                                        self.active == Some(index) || self.split == Some(index),
                                    );
                                    rects.push(session.response.rect);
                                    if session.response.dragged() {
                                        dragging = Some(index);
                                    }
                                    if session.response.drag_stopped() {
                                        dropped = Some(index);
                                    }
                                    let detached = self.detached.contains(&s.key);
                                    if session.inner.0.clicked() && detached {
                                        // A detached session's tab brings its window forward.
                                        ui.ctx().send_viewport_cmd_to(
                                            session_viewport(s.key),
                                            egui::ViewportCommand::Focus,
                                        );
                                    } else if session.inner.0.clicked() {
                                        if self.split == Some(index) {
                                            // Selecting the right side swaps the two sides.
                                            self.split = self.active;
                                        }
                                        self.active = Some(index);
                                    }
                                    if session.inner.1.clicked() {
                                        self.modal = Modal::Close(index);
                                    }
                                    session.inner.0.context_menu(|ui| {
                                        let beside = self.split == Some(index);
                                        let other =
                                            self.active.is_some_and(|a| a != index) && !detached;
                                        if (beside || other)
                                            && ui
                                                .button(if beside {
                                                    "Close split view"
                                                } else {
                                                    "Open beside current tab"
                                                })
                                                .clicked()
                                        {
                                            split_with = Some(index);
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
                                        if ui.button("Close tab").clicked() {
                                            self.modal = Modal::Close(index);
                                            ui.close();
                                        }
                                    });
                                });
                            }
                            if let Some(index) = split_with {
                                self.split_with(index);
                            }
                            match window_action {
                                Some((index, false)) => self.detach(index),
                                Some((index, true)) => self.attach(self.sessions[index].key),
                                None => {}
                            }
                            let pointer = ui.ctx().pointer_interact_pos().map(|p| p.x);
                            if let (Some(from), Some(x)) = (dragging.or(dropped), pointer) {
                                let to = drop_index(&rects, from, x);
                                if dropped.is_some() {
                                    self.move_session(from, to);
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
                                    session_tab(ui, c, state, false);
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
}

pub(super) fn tab(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
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

pub(super) fn session_tab(
    ui: &mut egui::Ui,
    connection: &Connection,
    state: session::TabState,
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
    let (rect, response) = ui.allocate_exact_size(Vec2::new(label_width + 82., 36.), Sense::drag());
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
    let (status, state) = match state {
        session::TabState::Open => (colors.accent, "open"),
        session::TabState::Ended => (ui.visuals().error_fg_color, "ended"),
        session::TabState::Waiting => (colors.muted, "connects when selected"),
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
            state
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

/// The position a dragged tab takes when dropped at `x`: the number of other tabs left of it.
pub(super) fn drop_index(rects: &[Rect], from: usize, x: f32) -> usize {
    rects
        .iter()
        .enumerate()
        .filter(|(i, r)| *i != from && r.center().x < x)
        .count()
}
