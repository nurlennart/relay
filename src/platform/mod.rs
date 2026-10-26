//! What differs between Windows, macOS, and Linux: data locations, fonts, the SSH client,
//! and the native window features that only embedded Windows remote desktops need.
use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawDisplayHandle,
    RawWindowHandle, WindowHandle,
};
use std::path::PathBuf;

#[cfg(not(windows))]
mod other;
#[cfg(windows)]
pub mod windows;

#[cfg(not(windows))]
use other as sys;
#[cfg(windows)]
use windows as sys;

pub use sys::{find_window, focus, install_shortcuts, take_command};

/// A native window of Relay's own. File pickers attach to it, and on Windows remote desktops
/// live inside it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Window {
    window: RawWindowHandle,
    display: RawDisplayHandle,
}

impl Window {
    pub fn of(source: &(impl HasWindowHandle + HasDisplayHandle)) -> Option<Self> {
        Some(Self {
            window: source.window_handle().ok()?.as_raw(),
            display: source.display_handle().ok()?.as_raw(),
        })
    }
}

impl HasWindowHandle for Window {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        // Relay's windows outlive every dialog and remote desktop attached to them.
        Ok(unsafe { WindowHandle::borrow_raw(self.window) })
    }
}

impl HasDisplayHandle for Window {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        Ok(unsafe { DisplayHandle::borrow_raw(self.display) })
    }
}

/// Relay shortcuts that work while a remote desktop has the keyboard.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    PreviousTab,
    NextTab,
    /// Returns keyboard focus to Relay.
    #[cfg_attr(
        not(windows),
        allow(dead_code, reason = "only the Windows hook sends it")
    )]
    Release,
}

/// Where Relay keeps its library, state, and theme: `%LOCALAPPDATA%\Relay` on Windows,
/// `~/Library/Application Support/Relay` on macOS, and `$XDG_DATA_HOME/relay` on Linux.
pub fn data_dir() -> PathBuf {
    let name = if cfg!(any(windows, target_os = "macos")) {
        "Relay"
    } else {
        "relay"
    };
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(name)
}

pub fn home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}

/// The OpenSSH client Relay runs for SSH sessions.
pub fn ssh_program() -> PathBuf {
    sys::ssh_program()
}

/// Installed fonts to prefer over egui's own, best first.
pub fn fonts(monospace: bool) -> &'static [&'static str] {
    if monospace {
        sys::MONOSPACE_FONTS
    } else {
        sys::PROPORTIONAL_FONTS
    }
}

/// Finds `name` on `PATH` or in well-known install folders that apps started from a desktop
/// launcher may not have on their `PATH`, such as Homebrew's.
pub fn find_program(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(sys::EXTRA_PROGRAM_DIRS.iter().map(PathBuf::from))
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}
