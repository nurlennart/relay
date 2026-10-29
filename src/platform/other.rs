//! macOS and Linux. Remote desktops are drawn by egui, so there are no native child windows
//! to place, find, or take focus from.
use super::{Command, Window};
use std::path::PathBuf;

impl Window {
    pub fn set_theme(self, _theme: crate::theme::Theme) {}
}

#[cfg(target_os = "macos")]
pub const PROPORTIONAL_FONTS: &[&str] = &[
    "/System/Library/Fonts/SFNS.ttf",
    "/System/Library/Fonts/Helvetica.ttc",
];
#[cfg(target_os = "macos")]
pub const MONOSPACE_FONTS: &[&str] = &[
    "/System/Library/Fonts/SFNSMono.ttf",
    "/System/Library/Fonts/Menlo.ttc",
];
#[cfg(target_os = "macos")]
pub const SYMBOL_FONTS: &[&str] = &["/System/Library/Fonts/Apple Symbols.ttf"];
#[cfg(target_os = "macos")]
pub const EXTRA_PROGRAM_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin"];

#[cfg(not(target_os = "macos"))]
pub const PROPORTIONAL_FONTS: &[&str] = &[
    "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/google-noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
];
#[cfg(not(target_os = "macos"))]
pub const MONOSPACE_FONTS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
    "/usr/share/fonts/dejavu-sans-mono-fonts/DejaVuSansMono.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansMono-Regular.ttf",
];
/// DejaVu Sans and Noto Sans Symbols 2 have the dingbats, Noto Sans Symbols the technical
/// signs such as ⎿, wherever each distribution installs them.
#[cfg(not(target_os = "macos"))]
pub const SYMBOL_FONTS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansSymbols-Regular.ttf",
    "/usr/share/fonts/noto/NotoSansSymbols-Regular.ttf",
    "/usr/share/fonts/google-noto/NotoSansSymbols-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf",
    "/usr/share/fonts/noto/NotoSansSymbols2-Regular.ttf",
    "/usr/share/fonts/google-noto/NotoSansSymbols2-Regular.ttf",
];
#[cfg(not(target_os = "macos"))]
pub const EXTRA_PROGRAM_DIRS: &[&str] = &["/usr/local/bin", "/snap/bin"];

pub fn ssh_program() -> PathBuf {
    super::find_program("ssh").unwrap_or_else(|| "ssh".into())
}

/// Prepares a helper process that runs without a window. Nothing is needed here.
pub fn background(_command: &mut std::process::Command) {}

pub fn install_shortcuts(_main: Window, _ctx: eframe::egui::Context) {}

pub fn take_command() -> Option<Command> {
    None
}

pub fn find_window(_title: &str, _known: &[Window]) -> Option<Window> {
    None
}

pub fn focus(_window: Window) {}
