//! Native file pickers, attached to Relay's window.
use crate::platform::Window;
use std::path::{Path, PathBuf};

/// What a picker is for, which sets its file types and options.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    ImportCsv,
    ExportCsv,
    /// A Relay JSON export, or a personal library file.
    ImportJson,
    ExportJson,
    /// An existing SQLite library.
    Database,
    /// A new SQLite library, created when it opens.
    NewDatabase,
    /// One or more Remote Desktop files.
    RdpFiles,
    /// A font for terminals.
    Font,
}

impl Kind {
    fn filter(self) -> Option<(&'static str, &'static [&'static str])> {
        match self {
            Self::ImportCsv | Self::ExportCsv => Some(("CSV file", &["csv"])),
            Self::ImportJson | Self::ExportJson => Some(("Relay JSON file", &["json"])),
            Self::Database | Self::NewDatabase => {
                Some(("SQLite database", &["sqlite", "db", "sqlite3"]))
            }
            Self::RdpFiles => Some(("Remote Desktop file", &["rdp"])),
            Self::Font => Some(("Font", &["ttf", "otf", "ttc"])),
        }
    }
}

/// A picker attached to `parent`, when there is one.
fn dialog(parent: Option<Window>) -> rfd::FileDialog {
    let dialog = rfd::FileDialog::new();
    match &parent {
        Some(parent) => dialog.set_parent(parent),
        None => dialog,
    }
}

/// Shows a picker and returns the chosen paths; an empty list means it was cancelled.
pub fn choose(parent: Option<Window>, kind: Kind) -> Vec<PathBuf> {
    let mut dialog = dialog(parent);
    if let Some((name, extensions)) = kind.filter() {
        dialog = dialog.add_filter(name, extensions);
    }
    match kind {
        Kind::RdpFiles => dialog.pick_files().unwrap_or_default(),
        Kind::ExportCsv => dialog
            .set_file_name("connections.csv")
            .save_file()
            .into_iter()
            .collect(),
        Kind::ExportJson => dialog
            .set_file_name("relay-connections.json")
            .save_file()
            .into_iter()
            .collect(),
        Kind::NewDatabase => dialog
            .set_file_name("relay.sqlite")
            .save_file()
            .into_iter()
            .collect(),
        Kind::ImportCsv | Kind::ImportJson | Kind::Database | Kind::Font => {
            dialog.pick_file().into_iter().collect()
        }
    }
}

/// Asks for an SSH private key, starting in the folder of `current` when it names one, or else
/// in `~/.ssh`, which the macOS and Linux pickers hide as a dot folder.
pub fn pick_key(parent: Option<Window>, current: &str) -> Option<PathBuf> {
    let home = dirs::home_dir();
    let current = match (current.trim().strip_prefix("~/"), &home) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(current.trim()),
    };
    let start = current
        .parent()
        .filter(|folder| !folder.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .into_iter()
        .chain(home.map(|home| home.join(".ssh")))
        .find(|folder| folder.is_dir());
    let mut dialog = dialog(parent);
    if let Some(start) = start {
        dialog = dialog.set_directory(start);
    }
    dialog.pick_file()
}

/// Asks for files to upload, or folders with `folders`.
pub fn pick_uploads(parent: Option<Window>, folders: bool) -> Vec<PathBuf> {
    let dialog = dialog(parent);
    if folders {
        dialog.pick_folders().unwrap_or_default()
    } else {
        dialog.pick_files().unwrap_or_default()
    }
}

/// Asks for a folder to download into, starting at `start`.
pub fn pick_folder(parent: Option<Window>, start: Option<&Path>) -> Option<PathBuf> {
    let mut dialog = dialog(parent).set_title("Download to folder");
    if let Some(start) = start {
        dialog = dialog.set_directory(start);
    }
    dialog.pick_folder()
}

/// Asks where to save a download named `name`, starting at `start`. The dialog asks before
/// replacing an existing file.
pub fn save_as(parent: Option<Window>, name: &str, start: Option<&Path>) -> Option<PathBuf> {
    let mut dialog = dialog(parent).set_file_name(name);
    if let Some(start) = start {
        dialog = dialog.set_directory(start);
    }
    dialog.save_file()
}
