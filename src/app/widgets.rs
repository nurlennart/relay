//! Controls and styling shared by every screen.
use super::*;

pub(super) fn accent_button(ui: &mut egui::Ui, text: &str, width: f32) -> egui::Response {
    let colors = theme::palette(ui.ctx());
    ui.add_sized(
        [width, CONTROL_HEIGHT],
        egui::Button::new(RichText::new(text).color(colors.bg).strong())
            .fill(colors.accent)
            .corner_radius(RADIUS),
    )
}

/// Spacing and text styles shared by every screen: compact, at desktop text sizes.
pub(super) fn apply_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.spacing.window_margin = egui::Margin::same(20);
    style.spacing.menu_margin = egui::Margin::same(6);
    style.spacing.item_spacing = Vec2::new(8., 8.);
    style.spacing.button_padding = Vec2::new(10., 5.);
    style.spacing.interact_size.y = 24.;
    style.spacing.scroll = egui::style::ScrollStyle::thin();
    // Labels are not selectable text: hovering them should not show a text cursor.
    style.interaction.selectable_labels = false;
    for (text, size) in [
        (egui::TextStyle::Body, 13.),
        (egui::TextStyle::Button, 13.),
        (egui::TextStyle::Small, 11.),
    ] {
        style
            .text_styles
            .insert(text, egui::FontId::proportional(size));
    }
    style.text_styles.insert(
        egui::TextStyle::Heading,
        egui::FontId::proportional(TITLE_SIZE),
    );
    ctx.set_style(style);
}

/// Corner radii: badges and small inline buttons, controls and rows, cards and menus, dialogs.
pub(super) const RADIUS_SMALL: u8 = 4;
pub(super) const RADIUS: u8 = 6;
pub(super) const RADIUS_LARGE: u8 = 8;
pub(super) const RADIUS_DIALOG: u8 = 10;

/// The size of icons beside text in rows, tabs and fields.
pub(super) const ICON_SIZE: f32 = 14.;

/// The size of dialog and page titles.
pub(super) const TITLE_SIZE: f32 = 17.;

/// The height of the tab bar, which the sidebar's header matches so their edges line up.
pub(super) const BAR_HEIGHT: f32 = 40.;

/// A dialog width that leaves a margin in narrow windows.
pub(super) fn dialog_width(ctx: &egui::Context, preferred: f32) -> f32 {
    preferred.min(ctx.content_rect().width() - 96.)
}

pub(super) fn dialog(id: &'static str, ctx: &egui::Context) -> egui::Modal {
    let colors = theme::palette(ctx);
    egui::Modal::new(egui::Id::new(id)).frame(
        egui::Frame::new()
            .fill(colors.panel)
            .inner_margin(20.)
            .corner_radius(RADIUS_DIALOG)
            .stroke(Stroke::new(1_f32, colors.line))
            .shadow(ctx.style().visuals.window_shadow),
    )
}

pub(super) fn protocol_badge(ui: &mut egui::Ui, protocol: Protocol) {
    let colors = theme::palette(ui.ctx());
    let color = match protocol {
        Protocol::Rdp => colors.rdp,
        Protocol::Ssh => colors.accent,
    };
    // Both labels are three letters, so badges share one width and line up in a column.
    egui::Frame::new()
        .fill(color.gamma_multiply(0.13))
        .stroke(Stroke::new(1_f32, color.gamma_multiply(0.22)))
        .corner_radius(RADIUS_SMALL)
        .inner_margin(egui::Margin::symmetric(6, 1))
        .show(ui, |ui| {
            ui.label(
                RichText::new(protocol.label())
                    .size(10.)
                    .strong()
                    .extra_letter_spacing(0.4)
                    .color(color),
            );
        });
}

/// The height of text fields and buttons, so controls in a row line up.
pub(super) const CONTROL_HEIGHT: f32 = 32.;

/// A single-line text field with room around its text, which is vertically centered.
pub(super) fn input<'a>(value: &'a mut String, hint: &str) -> egui::TextEdit<'a> {
    egui::TextEdit::singleline(value)
        .hint_text(hint)
        .margin(egui::Margin::symmetric(10, 6))
        .vertical_align(egui::Align::Center)
}

/// A label with a control close below it; groups are spaced further apart.
pub(super) fn labeled<R>(
    ui: &mut egui::Ui,
    label: &str,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 4.;
        ui.label(
            RichText::new(label)
                .size(12.)
                .color(theme::palette(ui.ctx()).muted),
        );
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
        [width, CONTROL_HEIGHT],
        egui::Button::new(RichText::new(label).size(13.))
            .fill(colors.surface)
            .stroke(Stroke::new(1_f32, colors.line))
            .corner_radius(RADIUS),
    )
}

/// Small letter-spaced capitals for section and column headings.
pub(super) fn caption(text: &str) -> RichText {
    RichText::new(text).size(10.).extra_letter_spacing(0.6)
}

pub(super) fn section_label(ui: &mut egui::Ui, text: &str) {
    let colors = theme::palette(ui.ctx());
    ui.add_space(2.);
    ui.label(caption(text).color(colors.muted));
    ui.add_space(4.);
}

/// A search field with a magnifier inside it and `shortcut` shown at its end while empty.
pub(super) fn search_field(
    ui: &mut egui::Ui,
    value: &mut String,
    hint: &str,
    shortcut: &str,
    id: egui::Id,
    width: f32,
) -> egui::Response {
    let colors = theme::palette(ui.ctx());
    let empty = value.is_empty();
    let response = ui.add_sized(
        [width, CONTROL_HEIGHT],
        egui::TextEdit::singleline(value)
            .hint_text(hint)
            .margin(egui::Margin {
                left: 30,
                right: 10,
                top: 6,
                bottom: 6,
            })
            .vertical_align(egui::Align::Center)
            .id(id),
    );
    let rect = response.rect;
    icons::paint_sized(
        ui.painter(),
        icons::Icon::Search,
        egui::pos2(rect.left() + 16., rect.center().y),
        ICON_SIZE,
        colors.muted,
    );
    if empty && !response.has_focus() && !shortcut.is_empty() {
        ui.painter().text(
            egui::pos2(rect.right() - 10., rect.center().y),
            egui::Align2::RIGHT_CENTER,
            shortcut,
            egui::FontId::proportional(11.),
            colors.muted,
        );
    }
    response
}
