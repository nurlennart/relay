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
    /// An SSH private key.
    Key,
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
            Self::Key => None,
        }
    }
}

/// Shows a picker and returns the chosen paths; an empty list means it was cancelled.
pub fn choose(parent: Option<Window>, kind: Kind) -> Vec<PathBuf> {
    let mut dialog = rfd::FileDialog::new();
    if let Some((name, extensions)) = kind.filter() {
        dialog = dialog.add_filter(name, extensions);
    }
    if let Some(parent) = &parent {
        dialog = dialog.set_parent(parent);
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
        Kind::ImportCsv | Kind::ImportJson | Kind::Database | Kind::Key => {
            dialog.pick_file().into_iter().collect()
        }
    }
}

/// Asks for files to upload, or folders with `folders`.
pub fn pick_uploads(parent: Option<Window>, folders: bool) -> Vec<PathBuf> {
    let mut dialog = rfd::FileDialog::new();
    if let Some(parent) = &parent {
        dialog = dialog.set_parent(parent);
    }
    if folders {
        dialog.pick_folders().unwrap_or_default()
    } else {
        dialog.pick_files().unwrap_or_default()
    }
}

/// Asks for a folder to download into, starting at `start`.
pub fn pick_folder(parent: Option<Window>, start: Option<&Path>) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().set_title("Download to folder");
    if let Some(parent) = &parent {
        dialog = dialog.set_parent(parent);
    }
    if let Some(start) = start {
        dialog = dialog.set_directory(start);
    }
    dialog.pick_folder()
}

/// Asks where to save a download named `name`, starting at `start`. The dialog asks before
/// replacing an existing file.
pub fn save_as(parent: Option<Window>, name: &str, start: Option<&Path>) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().set_file_name(name);
    if let Some(parent) = &parent {
        dialog = dialog.set_parent(parent);
    }
    if let Some(start) = start {
        dialog = dialog.set_directory(start);
    }
    dialog.save_file()
}
