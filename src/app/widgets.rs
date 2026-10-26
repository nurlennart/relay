//! Controls and styling shared by every screen.
use super::*;

pub(super) fn accent_button(ui: &mut egui::Ui, text: &str, width: f32) -> egui::Response {
    let colors = theme::palette(ui.ctx());
    ui.add_sized(
        [width, CONTROL_HEIGHT],
        egui::Button::new(RichText::new(text).color(colors.bg).strong())
            .fill(colors.accent)
            .corner_radius(6),
    )
}

/// Spacing and text styles shared by every screen.
pub(super) fn apply_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.spacing.window_margin = egui::Margin::same(24);
    style.spacing.item_spacing = Vec2::new(10., 10.);
    style.spacing.button_padding = Vec2::new(12., 8.);
    // Labels are not selectable text: hovering them should not show a text cursor.
    style.interaction.selectable_labels = false;
    style
        .text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(14.));
    style
        .text_styles
        .insert(egui::TextStyle::Button, egui::FontId::proportional(14.));
    ctx.set_style(style);
}

/// A dialog width that leaves a margin in narrow windows.
pub(super) fn dialog_width(ctx: &egui::Context, preferred: f32) -> f32 {
    preferred.min(ctx.content_rect().width() - 96.)
}

pub(super) fn dialog(id: &'static str, ctx: &egui::Context) -> egui::Modal {
    let colors = theme::palette(ctx);
    egui::Modal::new(egui::Id::new(id)).frame(
        egui::Frame::new()
            .fill(colors.panel)
            .inner_margin(24.)
            .corner_radius(12)
            .stroke(Stroke::new(1_f32, colors.line)),
    )
}

pub(super) fn protocol_badge(ui: &mut egui::Ui, protocol: Protocol) {
    let colors = theme::palette(ui.ctx());
    let color = match protocol {
        Protocol::Rdp => colors.rdp,
        Protocol::Ssh => colors.accent,
    };
    egui::Frame::new()
        .fill(color.gamma_multiply(0.13))
        .corner_radius(4)
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.label(
                RichText::new(protocol.label())
                    .monospace()
                    .size(11.)
                    .color(color),
            );
        });
}

/// The height of text fields and buttons, so controls in a row line up.
pub(super) const CONTROL_HEIGHT: f32 = 36.;

/// A single-line text field with room around its text, which is vertically centered.
pub(super) fn input<'a>(value: &'a mut String, hint: &str) -> egui::TextEdit<'a> {
    egui::TextEdit::singleline(value)
        .hint_text(hint)
        .margin(egui::Margin::symmetric(11, 8))
        .vertical_align(egui::Align::Center)
}

/// A label with a control close below it; groups are spaced further apart.
pub(super) fn labeled<R>(
    ui: &mut egui::Ui,
    label: &str,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 5.;
        ui.label(label);
        add(ui)
    })
    .inner
}

pub(super) fn field(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str) {
    labeled(ui, label, |ui| {
        ui.add_sized([ui.available_width(), CONTROL_HEIGHT], input(value, hint));
    });
}

pub(super) fn outline_button(ui: &mut egui::Ui, label: &str, width: f32) -> egui::Response {
    let colors = theme::palette(ui.ctx());
    ui.add_sized(
        [width, 36.],
        egui::Button::new(RichText::new(label).size(13.))
            .fill(colors.surface)
            .stroke(Stroke::new(1_f32, colors.line))
            .corner_radius(6),
    )
}

pub(super) fn section_label(ui: &mut egui::Ui, text: &str) {
    let colors = theme::palette(ui.ctx());
    ui.add_space(2.);
    ui.label(RichText::new(text).size(10.).color(colors.muted));
    ui.add_space(6.);
}
