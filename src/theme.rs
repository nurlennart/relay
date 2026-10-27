use eframe::egui::{self, Color32, Stroke};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Default, Debug, PartialEq, Serialize, Deserialize)]
pub enum Theme {
    #[default]
    Graphite,
    Midnight,
    Amethyst,
    Ember,
    Daylight,
    Linen,
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Color32,
    pub panel: Color32,
    pub surface: Color32,
    pub hover: Color32,
    pub line: Color32,
    pub muted: Color32,
    pub text: Color32,
    pub accent: Color32,
    pub selected: Color32,
    /// A quieter line than `line`, for dividers between rows.
    pub divider: Color32,
    /// Hover outlines and the edges of raised controls.
    pub edge: Color32,
    pub rdp: Color32,
    pub error_bg: Color32,
}

impl Theme {
    pub const ALL: [Self; 6] = [
        Self::Graphite,
        Self::Midnight,
        Self::Amethyst,
        Self::Ember,
        Self::Daylight,
        Self::Linen,
    ];

    fn is_light(self) -> bool {
        matches!(self, Self::Daylight | Self::Linen)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Graphite => "Graphite",
            Self::Midnight => "Midnight",
            Self::Amethyst => "Amethyst",
            Self::Ember => "Ember",
            Self::Daylight => "Daylight",
            Self::Linen => "Linen",
        }
    }

    pub fn colors(self) -> Palette {
        let (bg, panel, surface, accent) = match self {
            Self::Graphite => ([19, 22, 27], [25, 29, 35], [35, 41, 49], [137, 222, 173]),
            Self::Midnight => ([13, 21, 36], [19, 30, 48], [28, 43, 64], [126, 190, 255]),
            Self::Amethyst => ([25, 18, 34], [34, 25, 45], [47, 36, 61], [204, 167, 255]),
            Self::Ember => ([28, 22, 19], [37, 29, 25], [50, 40, 33], [246, 185, 116]),
            Self::Daylight => (
                [244, 247, 251],
                [255, 255, 255],
                [234, 239, 246],
                [24, 77, 145],
            ),
            Self::Linen => (
                [248, 245, 239],
                [255, 253, 248],
                [239, 235, 227],
                [123, 68, 28],
            ),
        };
        let rgb = |v: [u8; 3]| Color32::from_rgb(v[0], v[1], v[2]);
        let surface = rgb(surface);
        let accent = rgb(accent);
        let contrast = if self.is_light() {
            Color32::BLACK
        } else {
            Color32::WHITE
        };
        let panel = rgb(panel);
        Palette {
            bg: rgb(bg),
            panel,
            surface,
            hover: surface.lerp_to_gamma(contrast, if self.is_light() { 0.035 } else { 0.07 }),
            line: surface.lerp_to_gamma(contrast, if self.is_light() { 0.20 } else { 0.09 }),
            muted: if self.is_light() {
                rgb([77, 88, 104])
            } else {
                rgb([179, 187, 200])
            },
            text: if self.is_light() {
                rgb([31, 41, 55])
            } else {
                rgb([233, 237, 244])
            },
            accent,
            selected: surface.lerp_to_gamma(accent, if self.is_light() { 0.10 } else { 0.16 }),
            divider: panel.lerp_to_gamma(contrast, if self.is_light() { 0.08 } else { 0.06 }),
            edge: surface.lerp_to_gamma(contrast, if self.is_light() { 0.30 } else { 0.18 }),
            // SSH takes the accent, so themes with a blue accent give RDP violet instead.
            rdp: match self {
                Self::Midnight => rgb([204, 160, 255]),
                Self::Daylight => rgb([111, 52, 168]),
                _ if self.is_light() => rgb([24, 77, 145]),
                _ => rgb([145, 188, 242]),
            },
            error_bg: if self.is_light() {
                rgb([255, 232, 232])
            } else {
                rgb([66, 36, 39])
            },
        }
    }

    pub fn apply(self, ctx: &egui::Context) {
        let c = self.colors();
        let mut v = if self.is_light() {
            egui::Visuals::light()
        } else {
            egui::Visuals::dark()
        };
        v.error_fg_color = if self.is_light() {
            Color32::from_rgb(157, 29, 41)
        } else {
            Color32::from_rgb(255, 166, 176)
        };
        v.warn_fg_color = if self.is_light() {
            Color32::from_rgb(128, 72, 0)
        } else {
            Color32::from_rgb(233, 192, 119)
        };
        v.panel_fill = c.bg;
        v.window_fill = c.panel;
        v.extreme_bg_color = c.bg;
        v.faint_bg_color = c.panel;
        v.override_text_color = Some(c.text);
        // Placeholders sit clearly below labels and values, so they never read as entries.
        v.weak_text_color = Some(c.muted.lerp_to_gamma(c.bg, 0.35));
        v.hyperlink_color = c.accent;
        v.selection.bg_fill = c.selected;
        v.selection.stroke = Stroke::new(1_f32, c.accent);
        v.widgets.inactive.bg_fill = c.surface;
        v.widgets.inactive.weak_bg_fill = c.surface;
        v.widgets.inactive.bg_stroke = Stroke::new(1_f32, c.line);
        v.widgets.hovered.bg_fill = c.hover;
        v.widgets.hovered.weak_bg_fill = c.hover;
        v.widgets.hovered.bg_stroke = Stroke::new(1_f32, c.edge);
        v.widgets.active.bg_fill = c.selected;
        v.widgets.active.weak_bg_fill = c.selected;
        v.widgets.active.bg_stroke = Stroke::new(1_f32, c.accent);
        v.widgets.noninteractive.bg_stroke = Stroke::new(1_f32, c.line);
        for widget in [
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
        ] {
            widget.corner_radius = egui::CornerRadius::same(6);
        }
        v.window_corner_radius = egui::CornerRadius::same(10);
        v.menu_corner_radius = egui::CornerRadius::same(8);
        v.window_stroke = Stroke::new(1_f32, c.line);
        // Soft shadows lift dialogs and menus; light themes need less to read as depth.
        let shadow = |offset: i8, blur: u8, alpha: u8| egui::Shadow {
            offset: [0, offset],
            blur,
            spread: 0,
            color: Color32::from_black_alpha(if self.is_light() { alpha / 3 } else { alpha }),
        };
        v.window_shadow = shadow(12, 32, 120);
        v.popup_shadow = shadow(4, 14, 90);
        ctx.set_visuals(v);
        ctx.data_mut(|data| data.insert_temp(egui::Id::new("relay-theme"), self));
        ctx.request_repaint();
    }

    pub fn save(self, path: &Path) -> Result<(), String> {
        crate::model::atomic_write(path, &serde_json::to_vec(&self).map_err(|e| e.to_string())?)
    }
}

pub fn path() -> PathBuf {
    crate::model::data_path().with_file_name("theme.json")
}
pub fn load(path: &Path) -> Theme {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}
pub fn current(ctx: &egui::Context) -> Theme {
    ctx.data(|data| data.get_temp(egui::Id::new("relay-theme")))
        .unwrap_or_default()
}
pub fn palette(ctx: &egui::Context) -> Palette {
    current(ctx).colors()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn themes_persist_and_keep_readable_contrast() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("theme-test-{}.json", std::process::id()));
        let ctx = egui::Context::default();
        let luminance = |color: Color32| {
            let channel = |v: u8| {
                let s = v as f32 / 255.;
                if s <= 0.04045 {
                    s / 12.92
                } else {
                    ((s + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
        };
        for theme in Theme::ALL {
            theme.apply(&ctx);
            assert_eq!(current(&ctx), theme);
            theme.save(&path).unwrap();
            assert_eq!(load(&path), theme);
            let c = palette(&ctx);
            assert_eq!(ctx.style().visuals.panel_fill, c.bg);
            assert_eq!(ctx.style().visuals.dark_mode, !theme.is_light());
            // SSH and RDP badges are told apart by color.
            let distance = |a: Color32, b: Color32| {
                [(a.r(), b.r()), (a.g(), b.g()), (a.b(), b.b())]
                    .iter()
                    .map(|(x, y)| x.abs_diff(*y) as u32)
                    .sum::<u32>()
            };
            assert!(
                distance(c.accent, c.rdp) >= 90,
                "{}: SSH and RDP badges look alike",
                theme.name()
            );
            for background in [c.bg, c.panel, c.surface, c.selected, c.hover] {
                for foreground in [
                    c.text,
                    c.muted,
                    c.accent,
                    c.rdp,
                    ctx.style().visuals.error_fg_color,
                    ctx.style().visuals.warn_fg_color,
                ] {
                    let a = luminance(foreground);
                    let b = luminance(background);
                    assert!(
                        (a.max(b) + 0.05) / (a.min(b) + 0.05) >= 4.5,
                        "{} contrast: {:?} on {:?}",
                        theme.name(),
                        foreground,
                        background
                    );
                }
            }
        }
        std::fs::write(&path, b"invalid").unwrap();
        assert_eq!(load(&path), Theme::Graphite);
        assert_eq!(std::fs::read(&path).unwrap(), b"invalid");
        std::fs::remove_file(&path).unwrap();
        assert_eq!(load(&path), Theme::Graphite);
    }
}
