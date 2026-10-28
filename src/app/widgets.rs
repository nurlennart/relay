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

/// The narrowest a menu gets, so short menus still read as menus.
pub(super) const MENU_WIDTH: f32 = 200.;

/// A menu popup in the theme's colors: roomy items without outlines, a quiet hover, and
/// corners that follow the menu's own.
pub(super) fn menu_popup(popup: egui::Popup<'_>) -> egui::Popup<'_> {
    let colors = theme::palette(popup.ctx());
    let light = theme::current(popup.ctx()).is_light();
    // A hover a shade off the menu's fill, rather than the raised controls' hover.
    let hover = colors
        .panel
        .lerp_to_gamma(colors.text, if light { 0.06 } else { 0.08 });
    let pressed = colors
        .panel
        .lerp_to_gamma(colors.text, if light { 0.10 } else { 0.13 });
    popup.style(move |style: &mut egui::Style| {
        egui::containers::menu::menu_style(style);
        style.spacing.menu_margin = egui::Margin::same(5);
        style.spacing.button_padding = Vec2::new(10., 4.);
        style.spacing.item_spacing.y = 1.;
        // A long item widens the menu instead of breaking onto a second line.
        style.wrap_mode = Some(egui::TextWrapMode::Extend);
        style.visuals.menu_corner_radius = egui::CornerRadius::same(RADIUS_LARGE);
        style.visuals.window_fill = colors.panel;
        style.visuals.window_stroke = Stroke::new(1_f32, colors.line);
        // Separators are drawn with the non-interactive stroke.
        style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1_f32, colors.divider);
        let widgets = &mut style.visuals.widgets;
        widgets.hovered.weak_bg_fill = hover;
        widgets.hovered.bg_fill = hover;
        widgets.active.weak_bg_fill = pressed;
        widgets.active.bg_fill = pressed;
        for widget in [
            &mut widgets.inactive,
            &mut widgets.hovered,
            &mut widgets.active,
        ] {
            // Concentric with the menu: its radius less the margin.
            widget.corner_radius = egui::CornerRadius::same(RADIUS_SMALL);
        }
    })
}

/// A discreet line between groups of menu items.
pub(super) fn menu_separator(ui: &mut egui::Ui) {
    ui.add(egui::Separator::default().spacing(7.));
}

/// A menu item with its keyboard shortcut at the right, in quieter text.
pub(super) fn menu_item<'a>(
    text: &'a str,
    shortcut: &'a str,
    colors: &theme::Palette,
) -> egui::Button<'a> {
    egui::Button::new(text).shortcut_text(RichText::new(shortcut).size(12.).color(colors.muted))
}

/// The size of icons beside text in rows, tabs and fields.
pub(super) const ICON_SIZE: f32 = 14.;

/// The size of dialog and page titles.
pub(super) const TITLE_SIZE: f32 = 17.;

/// The height of the tab bar, which the sidebar's header matches so their edges line up.
pub(super) const BAR_HEIGHT: f32 = 40.;

/// A dialog width that leaves a margin in narrow windows.
/// A group name to type, with a button listing the existing `groups` to choose from.
pub(super) fn group_input(
    ui: &mut egui::Ui,
    group: &mut String,
    groups: &std::collections::BTreeSet<String>,
) {
    let colors = theme::palette(ui.ctx());
    ui.horizontal(|ui| {
        let button = CONTROL_HEIGHT;
        ui.add_sized(
            [
                ui.available_width() - button - ui.spacing().item_spacing.x,
                CONTROL_HEIGHT,
            ],
            input(group, "Personal"),
        );
        let choose = ui
            .add_enabled(
                !groups.is_empty(),
                egui::Button::new("").min_size(Vec2::splat(button)),
            )
            .on_hover_text("Choose an existing group");
        icons::paint_sized(
            ui.painter(),
            Icon::ChevronDown,
            choose.rect.center(),
            14.,
            if choose.enabled() {
                colors.text
            } else {
                colors.muted
            },
        );
        egui::Popup::menu(&choose).show(|ui| {
            for name in groups {
                if ui.button(name).clicked() {
                    *group = name.clone();
                }
            }
        });
    });
}

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
    let label = |protocol: Protocol| {
        egui::WidgetText::from(
            RichText::new(protocol.label())
                .size(10.)
                .strong()
                .extra_letter_spacing(0.4),
        )
        .into_galley(
            ui,
            Some(egui::TextWrapMode::Extend),
            f32::INFINITY,
            egui::TextStyle::Small,
        )
    };
    let galley = label(protocol);
    // Every badge is as wide as the widest label, so names beside them line up. It keeps its
    // own size, so a row centring it does not stretch it to the row's height.
    let width = [Protocol::Rdp, Protocol::Ssh]
        .map(|p| label(p).size().x)
        .into_iter()
        .fold(0_f32, f32::max);
    // A point of outline and of padding above and below; six of padding at the sides.
    let size = Vec2::new(width + 14., galley.size().y + 4.);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect(
        rect,
        RADIUS_SMALL,
        color.gamma_multiply(0.13),
        Stroke::new(1_f32, color.gamma_multiply(0.22)),
        egui::StrokeKind::Inside,
    );
    ui.painter()
        .galley(rect.center() - galley.size() / 2., galley, color);
}

/// The height of text fields and buttons, so controls in a row line up.
pub(super) const CONTROL_HEIGHT: f32 = 32.;

/// A single-line text field with room around its text, which is vertically centered.
pub(super) fn input<'a>(value: &'a mut String, hint: &str) -> egui::TextEdit<'a> {
    egui::TextEdit::singleline(value)
        // Weak text takes the placeholder color, which the theme's text color would override.
        .hint_text(RichText::new(hint).weak())
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
    let mut response = ui.add_sized(
        [width, CONTROL_HEIGHT],
        egui::TextEdit::singleline(value)
            // Weak text takes the placeholder color, which the theme's text color would override.
            .hint_text(RichText::new(hint).weak())
            .margin(egui::Margin {
                left: 30,
                // Typed text stops short of the clear button.
                right: if empty { 10 } else { 30 },
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
    // A clear button mirrors the search icon at the other end once something is typed.
    if !value.is_empty() {
        let center = egui::pos2(rect.right() - 16., rect.center().y);
        let clear = ui
            .interact(
                egui::Rect::from_center_size(center, Vec2::splat(20.)),
                id.with("clear"),
                egui::Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text("Clear");
        clear.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Clear"));
        if clear.hovered() {
            ui.painter().circle_filled(center, 10., colors.hover);
        }
        let color = if clear.hovered() {
            colors.text
        } else {
            colors.muted
        };
        icons::paint_sized(ui.painter(), icons::Icon::Close, center, 14., color);
        if clear.clicked() {
            value.clear();
            response.mark_changed();
            // Keep typing in the field rather than wherever the click left focus.
            response.request_focus();
            ui.ctx().request_repaint();
        }
    }
    response
}
