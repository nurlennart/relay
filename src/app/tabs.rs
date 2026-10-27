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
                egui::ScrollArea::horizontal()
                    .id_salt("tabs-scroll")
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                    .show(ui, |ui| {
                        ui.horizontal_centered(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.;
                            let library = library_tab(ui, self.active.is_none())
                                .on_hover_text("Connection library (Ctrl+1)");
                            if library.clicked() {
                                self.active = None;
                            }
                            // Tabs can be dragged; the drop position follows the pointer.
                            let mut rects = vec![];
                            let mut split_with = None;
                            let mut window_action = None;
                            let mut dragging = None;
                            let mut dropped = None;
                            let mut close = None;
                            for (index, s) in self.sessions.iter().enumerate() {
                                ui.push_id(index, |ui| {
                                    let session = session_tab(
                                        ui,
                                        &s.connection,
                                        s.browsing,
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
                                        close = Some(index);
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
                                            close = Some(index);
                                            ui.close();
                                        }
                                    });
                                });
                            }
                            if let Some(index) = split_with {
                                self.split_with(index);
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
                                    session_tab(ui, c, false, state, false);
                                }
                            }
                            if icon_button(ui, Icon::Plus, 26., "New connection (Ctrl+N)").clicked()
                            {
                                self.new_connection();
                            }
                        });
                    });
            });
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

pub(super) fn session_tab(
    ui: &mut egui::Ui,
    connection: &Connection,
    browsing: bool,
    state: session::TabState,
    selected: bool,
) -> egui::InnerResponse<(egui::Response, egui::Response)> {
    let colors = theme::palette(ui.ctx());
    let text = if selected { colors.text } else { colors.muted };
    let label = connection.name.as_str();
    let galley = egui::WidgetText::from(RichText::new(label).size(13.)).into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        TAB_LABEL_MAX,
        egui::TextStyle::Body,
    );
    // Protocol icon, name, then the close button.
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(galley.size().x + 62., 28.), Sense::drag());
    let hovered = ui.rect_contains_pointer(rect);
    tab_background(ui, rect, selected, hovered);
    // The protocol's icon carries the session state in its color.
    let (status, state) = match state {
        session::TabState::Open => (colors.accent, "open"),
        session::TabState::Ended => (ui.visuals().error_fg_color, "ended"),
        session::TabState::Waiting => (colors.muted, "connects when selected"),
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
        .on_hover_text(format!(
            "{} · {} · {}",
            if browsing {
                "SFTP"
            } else {
                connection.protocol.label()
            },
            connection.address(),
            state
        ));
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

/// The position a dragged tab takes when dropped at `x`: the number of other tabs left of it.
pub(super) fn drop_index(rects: &[Rect], from: usize, x: f32) -> usize {
    rects
        .iter()
        .enumerate()
        .filter(|(i, r)| *i != from && r.center().x < x)
        .count()
}
