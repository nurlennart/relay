//! Terminal color schemes, chosen in Settings and shared by every terminal.
use eframe::egui::Color32;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum Scheme {
    /// Dark or light, following the workspace theme.
    Workspace,
    #[default]
    Dark,
    Light,
    SolarizedDark,
    SolarizedLight,
    Monokai,
}

impl Scheme {
    pub const ALL: [Self; 6] = [
        Self::Workspace,
        Self::Dark,
        Self::Light,
        Self::SolarizedDark,
        Self::SolarizedLight,
        Self::Monokai,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Workspace => "Match workspace theme",
            Self::Dark => "Dark",
            Self::Light => "Light",
            Self::SolarizedDark => "Solarized Dark",
            Self::SolarizedLight => "Solarized Light",
            Self::Monokai => "Monokai",
        }
    }

    /// The scheme's colors, in a workspace with a light theme or not.
    pub fn colors(self, light_workspace: bool) -> Colors {
        match self {
            Self::Workspace if light_workspace => Self::Light.colors(true),
            Self::Workspace | Self::Dark => DARK,
            Self::Light => LIGHT,
            Self::SolarizedDark => SOLARIZED_DARK,
            Self::SolarizedLight => SOLARIZED_LIGHT,
            Self::Monokai => MONOKAI,
        }
    }
}

/// The colors a terminal is painted with: its frame, text, highlights, and the 16 basic
/// colors programs choose from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Colors {
    pub light: bool,
    pub background: Color32,
    pub title: Color32,
    pub title_text: Color32,
    pub title_muted: Color32,
    pub border: Color32,
    pub foreground: Color32,
    pub cursor: Color32,
    pub selection: Color32,
    pub running: Color32,
    pub ended: Color32,
    pub scrolled: Color32,
    pub matched: Color32,
    pub match_tick: Color32,
    pub current_match: Color32,
    /// Text on the current match.
    pub current_match_text: Color32,
    pub link: Color32,
    /// Whether bold text in one of the eight basic colors uses its bright variant, as most
    /// terminals do. Solarized keeps its bright colors for its grays, so it does not.
    pub bold_brightens: bool,
    pub ansi: [Color32; 16],
}

const fn rgb(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

const DARK: Colors = Colors {
    light: false,
    background: rgb(0x0e1116),
    title: rgb(0x161a21),
    title_text: rgb(0xc8cfda),
    title_muted: rgb(0x7a8494),
    border: rgb(0x2a303a),
    foreground: rgb(0xd7dee8),
    cursor: rgb(0xe6ebf2),
    selection: rgb(0x344662),
    running: rgb(0x89dead),
    ended: rgb(0xe16c75),
    scrolled: rgb(0xe5c07b),
    matched: rgb(0x5c4c26),
    match_tick: rgb(0xa08446),
    current_match: rgb(0xe5c07b),
    current_match_text: rgb(0x0e1116),
    link: rgb(0x78aaf0),
    bold_brightens: true,
    ansi: [
        rgb(0x181d23),
        rgb(0xe16c75),
        rgb(0x89dead),
        rgb(0xe5c07b),
        rgb(0x7baaed),
        rgb(0xc697e2),
        rgb(0x70cad7),
        rgb(0xd7dee8),
        rgb(0x6b7484),
        rgb(0xf68c96),
        rgb(0xaaeec3),
        rgb(0xf5d69c),
        rgb(0xa2c6f8),
        rgb(0xdcb4f4),
        rgb(0x99e1e9),
        rgb(0xf6f8fc),
    ],
};

// Light backgrounds need darker versions of the basic colors, and "white" text must stay
// readable, so the whites are grays here.
const LIGHT: Colors = Colors {
    light: true,
    background: rgb(0xfbfbfa),
    title: rgb(0xeef0f3),
    title_text: rgb(0x29303a),
    title_muted: rgb(0x5f6875),
    border: rgb(0xd3d8df),
    foreground: rgb(0x242933),
    cursor: rgb(0x242933),
    selection: rgb(0xbcd3f2),
    running: rgb(0x1e8a4c),
    ended: rgb(0xb4232f),
    scrolled: rgb(0x8a5a00),
    matched: rgb(0xf7e2a1),
    match_tick: rgb(0xc89a2a),
    current_match: rgb(0xe8a33a),
    current_match_text: rgb(0x242933),
    link: rgb(0x1f5fbf),
    bold_brightens: true,
    ansi: [
        rgb(0x242933),
        rgb(0xb8323c),
        rgb(0x217a3c),
        rgb(0x8a6500),
        rgb(0x2156b0),
        rgb(0x8a3ea8),
        rgb(0x00727f),
        rgb(0x7e8591),
        rgb(0x5f6875),
        rgb(0xd0434f),
        rgb(0x2a924a),
        rgb(0xa57900),
        rgb(0x2f6ccc),
        rgb(0xa450c4),
        rgb(0x008a99),
        rgb(0x3b424d),
    ],
};

// Ethan Schoonover's Solarized palette, with its usual assignment to the basic colors.
const BASE03: Color32 = rgb(0x002b36);
const BASE02: Color32 = rgb(0x073642);
const BASE01: Color32 = rgb(0x586e75);
const BASE00: Color32 = rgb(0x657b83);
const BASE0: Color32 = rgb(0x839496);
const BASE1: Color32 = rgb(0x93a1a1);
const BASE2: Color32 = rgb(0xeee8d5);
const BASE3: Color32 = rgb(0xfdf6e3);
const YELLOW: Color32 = rgb(0xb58900);
const ORANGE: Color32 = rgb(0xcb4b16);
const SOLARIZED: [Color32; 16] = [
    BASE02,
    rgb(0xdc322f),
    rgb(0x859900),
    YELLOW,
    rgb(0x268bd2),
    rgb(0xd33682),
    rgb(0x2aa198),
    BASE2,
    BASE03,
    ORANGE,
    BASE01,
    BASE00,
    BASE0,
    rgb(0x6c71c4),
    BASE1,
    BASE3,
];

const SOLARIZED_DARK: Colors = Colors {
    light: false,
    background: BASE03,
    title: BASE02,
    title_text: BASE1,
    title_muted: BASE01,
    border: rgb(0x0e4452),
    foreground: BASE0,
    cursor: BASE1,
    selection: BASE02,
    running: rgb(0x859900),
    ended: rgb(0xdc322f),
    scrolled: YELLOW,
    matched: rgb(0x3c4a1c),
    match_tick: rgb(0x8a7020),
    current_match: YELLOW,
    current_match_text: BASE03,
    link: rgb(0x268bd2),
    bold_brightens: false,
    ansi: SOLARIZED,
};

const SOLARIZED_LIGHT: Colors = Colors {
    light: true,
    background: BASE3,
    title: BASE2,
    title_text: BASE02,
    title_muted: BASE00,
    border: rgb(0xd9d2bf),
    foreground: BASE00,
    cursor: BASE01,
    selection: rgb(0xe0dbc8),
    running: rgb(0x859900),
    ended: rgb(0xdc322f),
    scrolled: ORANGE,
    matched: rgb(0xf1dfa0),
    match_tick: YELLOW,
    current_match: YELLOW,
    current_match_text: BASE03,
    link: rgb(0x268bd2),
    bold_brightens: false,
    ansi: SOLARIZED,
};

// Wimer Hazenberg's Monokai. It has no blue of its own, so its cyan stands in, and its
// bright colors are the same as the normal ones, as in most terminal ports.
const MONOKAI_RED: Color32 = rgb(0xf92672);
const MONOKAI_GREEN: Color32 = rgb(0xa6e22e);
const MONOKAI_YELLOW: Color32 = rgb(0xe6db74);
const MONOKAI_BLUE: Color32 = rgb(0x66d9ef);
const MONOKAI_PURPLE: Color32 = rgb(0xae81ff);
const MONOKAI_CYAN: Color32 = rgb(0xa1efe4);
const MONOKAI_WHITE: Color32 = rgb(0xf8f8f2);

const MONOKAI: Colors = Colors {
    light: false,
    background: rgb(0x272822),
    title: rgb(0x1e1f1c),
    title_text: MONOKAI_WHITE,
    title_muted: rgb(0x8f8a73),
    border: rgb(0x3e3d32),
    foreground: MONOKAI_WHITE,
    cursor: MONOKAI_WHITE,
    selection: rgb(0x49483e),
    running: MONOKAI_GREEN,
    ended: MONOKAI_RED,
    scrolled: MONOKAI_YELLOW,
    matched: rgb(0x5b5631),
    match_tick: rgb(0xa59f52),
    current_match: MONOKAI_YELLOW,
    current_match_text: rgb(0x272822),
    link: MONOKAI_BLUE,
    bold_brightens: false,
    ansi: [
        rgb(0x3e3d32),
        MONOKAI_RED,
        MONOKAI_GREEN,
        MONOKAI_YELLOW,
        MONOKAI_BLUE,
        MONOKAI_PURPLE,
        MONOKAI_CYAN,
        MONOKAI_WHITE,
        rgb(0x8f8a73),
        MONOKAI_RED,
        MONOKAI_GREEN,
        MONOKAI_YELLOW,
        MONOKAI_BLUE,
        MONOKAI_PURPLE,
        MONOKAI_CYAN,
        rgb(0xf9f8f5),
    ],
};

#[cfg(test)]
mod tests {
    use super::*;

    fn contrast(a: Color32, b: Color32) -> f32 {
        let luminance = |c: Color32| {
            let channel = |v: u8| {
                let s = v as f32 / 255.;
                if s <= 0.04045 {
                    s / 12.92
                } else {
                    ((s + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
        };
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn every_scheme_keeps_its_text_readable() {
        for scheme in Scheme::ALL {
            for light in [false, true] {
                let c = scheme.colors(light);
                let name = scheme.name();
                // Solarized is low in contrast by design, just under 4.5 for its light text.
                let solarized = matches!(scheme, Scheme::SolarizedDark | Scheme::SolarizedLight);
                let text = if solarized { 4. } else { 4.5 };
                assert!(contrast(c.foreground, c.background) >= text, "{name} text");
                assert!(
                    contrast(c.foreground, c.selection) >= 3.,
                    "{name} selection"
                );
                assert!(contrast(c.foreground, c.matched) >= 3., "{name} match");
                assert!(
                    contrast(c.current_match_text, c.current_match) >= 3.,
                    "{name} current match"
                );
                assert!(contrast(c.title_text, c.title) >= text, "{name} title");
                // The basic colors programs print in, apart from black, which is meant for
                // backgrounds; Solarized's grays are its own business.
                if !solarized {
                    for (i, ansi) in c.ansi.iter().enumerate().skip(1) {
                        assert!(
                            contrast(*ansi, c.background) >= 3.,
                            "{name} color {i}: {ansi:?}"
                        );
                    }
                }
            }
        }
        assert_eq!(Scheme::Workspace.colors(true), LIGHT);
        assert_eq!(Scheme::Workspace.colors(false), DARK);
    }
}
