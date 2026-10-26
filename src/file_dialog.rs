//! Native file pickers, attached to Relay's window.
use crate::platform::Window;
use std::path::PathBuf;

/// What a picker is for, which sets its file types and options.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    ImportCsv,
    ExportCsv,
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
        Kind::NewDatabase => dialog
            .set_file_name("relay.sqlite")
            .save_file()
            .into_iter()
            .collect(),
        Kind::ImportCsv | Kind::Database | Kind::Key => dialog.pick_file().into_iter().collect(),
    }
}
