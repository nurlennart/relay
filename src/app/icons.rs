//! Line icons drawn with the painter, so they look the same with every system font.
use super::*;
use egui::{Pos2, Rect, Sense};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Icon {
    Library,
    Recent,
    Star,
    StarFilled,
    Desktop,
    /// A desktop with a pointer on it: one whose screen is shared, as VNC does.
    SharedDesktop,
    Terminal,
    Folder,
    Settings,
    Plus,
    Close,
    More,
    Search,
    Sidebar,
    ChevronDown,
    ChevronUp,
    File,
    Up,
    Back,
    Refresh,
    Upload,
    Download,
}

impl Icon {
    pub(super) fn protocol(protocol: Protocol) -> Self {
        match protocol {
            Protocol::Rdp => Self::Desktop,
            Protocol::Vnc => Self::SharedDesktop,
            Protocol::Ssh => Self::Terminal,
        }
    }
}

/// Paints `icon` in a 16-point box around `center`.
pub(super) fn paint(painter: &egui::Painter, icon: Icon, center: Pos2, color: Color32) {
    paint_sized(painter, icon, center, 16., color);
}

/// Paints `icon` scaled to a `size`-point box around `center`.
pub(super) fn paint_sized(
    painter: &egui::Painter,
    icon: Icon,
    center: Pos2,
    size: f32,
    color: Color32,
) {
    // Icons are drawn on a 16-point grid centered on the origin.
    let s = size / 16.;
    let at = |x: f32, y: f32| center + Vec2::new(x * s, y * s);
    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| Rect::from_min_max(at(x0, y0), at(x1, y1));
    let stroke = Stroke::new(1.3 * s.max(0.8), color);
    let line = |points: &[(f32, f32)]| {
        painter.add(egui::Shape::line(
            points.iter().map(|&(x, y)| at(x, y)).collect(),
            stroke,
        ));
    };
    let outline = |r: Rect, radius: f32| {
        painter.rect_stroke(r, radius * s, stroke, egui::StrokeKind::Middle);
    };
    match icon {
        Icon::Library => {
            for (x, y) in [(-6., -6.), (1., -6.), (-6., 1.), (1., 1.)] {
                outline(rect(x, y, x + 5., y + 5.), 1.2);
            }
        }
        Icon::Recent => {
            painter.circle_stroke(center, 6.5 * s, stroke);
            line(&[(0., -3.5), (0., 0.), (2.8, 1.8)]);
        }
        Icon::Star | Icon::StarFilled => {
            let points: Vec<Pos2> = (0..10)
                .map(|i| {
                    let radius = if i % 2 == 0 { 7. } else { 3. };
                    let angle = std::f32::consts::PI * (i as f32 / 5. - 0.5);
                    at(radius * angle.cos(), radius * angle.sin() + 0.6)
                })
                .collect();
            if icon == Icon::StarFilled {
                // The star is concave: fill its pentagon core, then each point.
                let core: Vec<Pos2> = points.iter().skip(1).step_by(2).copied().collect();
                painter.add(egui::Shape::convex_polygon(core, color, Stroke::NONE));
                for i in (0..10).step_by(2) {
                    painter.add(egui::Shape::convex_polygon(
                        vec![points[(i + 9) % 10], points[i], points[i + 1]],
                        color,
                        Stroke::NONE,
                    ));
                }
            }
            painter.add(egui::Shape::closed_line(points, stroke));
        }
        Icon::Desktop | Icon::SharedDesktop => {
            // The screen, not screen and stand together, sits on the centre line, so it lines
            // up with text beside it as the other icons' boxes do.
            outline(rect(-7., -5., 7., 4.5), 1.5);
            line(&[(0., 4.5), (0., 7.5)]);
            line(&[(-3.5, 7.5), (3.5, 7.5)]);
            if icon == Icon::SharedDesktop {
                // An arrow pointer on the screen, its tip up and to the left, with a short tail.
                let head = [(-3., -3.), (2., -0.6), (-0.6, 2.)];
                painter.add(egui::Shape::convex_polygon(
                    head.iter().map(|&(x, y)| at(x, y)).collect(),
                    color,
                    Stroke::NONE,
                ));
                line(&[(0., 0.), (2.2, 2.2)]);
            }
        }
        Icon::Terminal => {
            outline(rect(-7., -6., 7., 6.), 2.);
            line(&[(-4., -2.), (-1.5, 0.3), (-4., 2.6)]);
            line(&[(0.5, 2.6), (4., 2.6)]);
        }
        Icon::Folder => {
            painter.add(egui::Shape::closed_line(
                [
                    (-7., -5.),
                    (-2., -5.),
                    (0., -3.),
                    (7., -3.),
                    (7., 6.),
                    (-7., 6.),
                ]
                .map(|(x, y)| at(x, y))
                .to_vec(),
                stroke,
            ));
            line(&[(-7., -1.), (7., -1.)]);
        }
        Icon::Settings => {
            painter.circle_stroke(center, 2.3 * s, stroke);
            painter.circle_stroke(center, 5. * s, stroke);
            for i in 0..8 {
                let angle = std::f32::consts::FRAC_PI_4 * i as f32;
                let (x, y) = (angle.cos(), angle.sin());
                line(&[(x * 5., y * 5.), (x * 7., y * 7.)]);
            }
        }
        Icon::Plus => {
            line(&[(-5.5, 0.), (5.5, 0.)]);
            line(&[(0., -5.5), (0., 5.5)]);
        }
        Icon::Close => {
            line(&[(-4., -4.), (4., 4.)]);
            line(&[(4., -4.), (-4., 4.)]);
        }
        Icon::More => {
            for x in [-5., 0., 5.] {
                painter.circle_filled(at(x, 0.), 1.4 * s, color);
            }
        }
        Icon::Search => {
            painter.circle_stroke(at(-1.2, -1.2), 4.8 * s, stroke);
            line(&[(2.4, 2.4), (6., 6.)]);
        }
        Icon::Sidebar => {
            outline(rect(-7., -6., 7., 6.), 2.);
            line(&[(-2.5, -6.), (-2.5, 6.)]);
        }
        Icon::ChevronDown => line(&[(-4., -2.), (0., 2.), (4., -2.)]),
        Icon::ChevronUp => line(&[(-4., 2.), (0., -2.), (4., 2.)]),
        Icon::File => {
            painter.add(egui::Shape::closed_line(
                [(-5., -7.), (1.5, -7.), (5., -3.5), (5., 7.), (-5., 7.)]
                    .map(|(x, y)| at(x, y))
                    .to_vec(),
                stroke,
            ));
            line(&[(1.5, -7.), (1.5, -3.5), (5., -3.5)]);
        }
        Icon::Up => {
            line(&[(0., 6.), (0., -6.)]);
            line(&[(-4.5, -1.5), (0., -6.), (4.5, -1.5)]);
        }
        Icon::Back => {
            line(&[(6., 0.), (-6., 0.)]);
            line(&[(-1.5, -4.5), (-6., 0.), (-1.5, 4.5)]);
        }
        Icon::Refresh => {
            // A clockwise arc open at the top right, with a filled arrowhead at its end
            // pointing on along the circle.
            let (radius, start, sweep) = (5.5_f32, -0.75_f32, 4.9_f32);
            let points = (0..=24)
                .map(|i| {
                    let angle = start + sweep * i as f32 / 24.;
                    at(radius * angle.cos(), radius * angle.sin())
                })
                .collect();
            painter.add(egui::Shape::line(points, stroke));
            let end = start + sweep;
            let point = Vec2::new(radius * end.cos(), radius * end.sin());
            let along = Vec2::new(-end.sin(), end.cos());
            let across = Vec2::new(-along.y, along.x);
            let corner = |v: Vec2| at(v.x, v.y);
            painter.add(egui::Shape::convex_polygon(
                vec![
                    corner(point + along * 2.6),
                    corner(point - along * 0.6 + across * 2.5),
                    corner(point - along * 0.6 - across * 2.5),
                ],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Upload | Icon::Download => {
            line(&[(-6.5, 3.), (-6.5, 6.5), (6.5, 6.5), (6.5, 3.)]);
            line(&[(0., -6.5), (0., 3.)]);
            if icon == Icon::Upload {
                line(&[(-3.5, -3.), (0., -6.5), (3.5, -3.)]);
            } else {
                line(&[(-3.5, -0.5), (0., 3.), (3.5, -0.5)]);
            }
        }
    }
}

/// A frameless square button showing `icon`, filled while hovered.
pub(super) fn icon_button(
    ui: &mut egui::Ui,
    icon: Icon,
    size: f32,
    tooltip: &str,
) -> egui::Response {
    let colors = theme::palette(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), tooltip)
    });
    let hovered = response.hovered() || response.has_focus();
    // An outline keeps the hover visible on rows that are already highlighted.
    if hovered || response.is_pointer_button_down_on() {
        ui.painter().rect(
            rect,
            RADIUS,
            colors.hover,
            Stroke::new(1_f32, colors.line),
            egui::StrokeKind::Inside,
        );
    }
    let color = if !ui.is_enabled() {
        colors.muted.gamma_multiply(0.5)
    } else if hovered {
        colors.text
    } else {
        colors.muted
    };
    paint(ui.painter(), icon, rect.center(), color);
    response.on_hover_text(tooltip)
}
