//! What differs between Windows, macOS, and Linux: data locations, fonts, the SSH client,
//! and the native window features that only embedded Windows remote desktops need.
use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawDisplayHandle,
    RawWindowHandle, WindowHandle,
};
use std::path::{Path, PathBuf};

#[cfg(not(windows))]
mod other;
#[cfg(windows)]
pub mod windows;

#[cfg(not(windows))]
use other as sys;
#[cfg(windows)]
use windows as sys;

pub use sys::{background, find_window, focus, install_shortcuts, take_command};

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

/// Where downloads go unless the person picks another folder.
pub fn downloads_dir() -> Option<PathBuf> {
    dirs::download_dir().or_else(dirs::home_dir)
}

/// What a file name is known as in local folders, where two names with the same key are one
/// file: Windows and macOS volumes usually ignore case.
pub fn name_key(name: &str) -> String {
    if cfg!(any(windows, target_os = "macos")) {
        name.to_lowercase()
    } else {
        name.into()
    }
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

/// Installed fonts with symbols the others lack, each used if present, as a last resort.
pub fn symbol_fonts() -> &'static [&'static str] {
    sys::SYMBOL_FONTS
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

/// Opens `path` in `editor`, a command line where `{file}` stands for the file (which
/// otherwise goes last), or in the system's editor for that kind of file when it is empty.
/// Relay does not wait for the editor to close.
pub fn edit_file(editor: &str, path: &Path) -> Result<(), String> {
    let words = split_command(editor);
    let Some((program, args)) = words.split_first() else {
        return sys::edit_with_default(path);
    };
    let file = path.to_string_lossy();
    let mut command = std::process::Command::new(program);
    let mut placed = false;
    for arg in args {
        placed |= arg.contains("{file}");
        command.arg(arg.replace("{file}", &file));
    }
    if !placed {
        command.arg(path);
    }
    spawn_detached(command).map_err(|e| format!("Cannot start {program}: {e}"))
}

/// Starts `command` and reaps it when it exits, without waiting for that.
fn spawn_detached(mut command: std::process::Command) -> std::io::Result<()> {
    let mut child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    std::thread::Builder::new()
        .name("relay-editor".into())
        .spawn(move || child.wait())?;
    Ok(())
}

/// Splits a command line at spaces, keeping text in double or single quotes together.
/// Backslashes are kept as they are, since Windows paths are full of them.
pub fn split_command(line: &str) -> Vec<String> {
    let mut words = vec![];
    let mut word: Option<String> = None;
    let mut quote = None;
    for c in line.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => word.get_or_insert_default().push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                word.get_or_insert_default();
            }
            (None, c) if c.is_whitespace() => words.extend(word.take()),
            (None, c) => word.get_or_insert_default().push(c),
        }
    }
    words.extend(word);
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_commands_split_at_spaces_outside_quotes() {
        assert_eq!(split_command("  "), Vec::<String>::new());
        assert_eq!(split_command("code --wait"), ["code", "--wait"]);
        assert_eq!(
            split_command(r#""C:\Program Files\Notepad++\notepad++.exe" -multiInst"#),
            [r"C:\Program Files\Notepad++\notepad++.exe", "-multiInst"]
        );
        assert_eq!(
            split_command("emacsclient -n '+{file}' \"\""),
            ["emacsclient", "-n", "+{file}", ""]
        );
    }
}
