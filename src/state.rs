//! Workspace state kept between launches: window size, sidebar, last library, and recents.
use crate::database::Location;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// How many recent connections each library keeps.
const RECENT: usize = 12;

/// The library to reopen at startup. Server libraries reopen only with a remembered
/// connection string.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum Library {
    #[default]
    Personal,
    Sqlite(PathBuf),
    Mssql,
    Mysql,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// The restored (not maximized) window size in points.
    pub window: Option<[f32; 2]>,
    pub maximized: bool,
    pub sidebar_collapsed: Option<bool>,
    pub library: Library,
    /// Server libraries opened without TLS; the default is to require it.
    pub mssql_without_tls: bool,
    pub mysql_without_tls: bool,
    /// Recently opened connection IDs per library, most recent first.
    pub recent: BTreeMap<String, Vec<u64>>,
    /// Saved commands for SSH sessions.
    pub snippets: Vec<Snippet>,
    /// Tabs open when Relay last changed them, restored at startup.
    pub open: Vec<crate::model::Connection>,
    /// The selected tab among `open`.
    pub active_tab: Option<usize>,
    /// Which of `open` are SFTP file browsers rather than terminals. A separate list keeps
    /// `open` readable by earlier versions, which restore them as terminals.
    pub file_tabs: Vec<usize>,
    /// The folder downloads from the file browser went to last.
    pub download_folder: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snippet {
    pub name: String,
    pub command: String,
    /// Press Enter after typing the command.
    pub run: bool,
}

/// Indices of snippets whose name or command contains `query`, ignoring case.
pub fn matching(snippets: &[Snippet], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    snippets
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            s.name.to_lowercase().contains(&query) || s.command.to_lowercase().contains(&query)
        })
        .map(|(i, _)| i)
        .collect()
}

pub fn path() -> PathBuf {
    crate::model::data_path().with_file_name("state.json")
}

/// Loads saved state; anything unreadable falls back to defaults.
pub fn load(path: &Path) -> State {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

impl State {
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        crate::model::atomic_write(path, &bytes)
    }

    pub fn recent(&self, location: &Location) -> &[u64] {
        self.recent
            .get(&key(location))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// Records `location` as the library to reopen, with its TLS choice.
    pub fn use_library(&mut self, location: &Location) {
        self.library = Library::of(location);
        match location {
            Location::Mssql(server) => self.mssql_without_tls = !server.tls,
            Location::Mysql(server) => self.mysql_without_tls = !server.tls,
            Location::Local(_) | Location::Sqlite(_) => {}
        }
    }

    pub fn note_recent(&mut self, location: &Location, id: u64) {
        let list = self.recent.entry(key(location)).or_default();
        list.retain(|v| *v != id);
        list.insert(0, id);
        list.truncate(RECENT);
    }
}

impl Library {
    pub fn of(location: &Location) -> Self {
        match location {
            Location::Local(_) => Self::Personal,
            Location::Sqlite(path) => Self::Sqlite(path.clone()),
            Location::Mssql(_) => Self::Mssql,
            Location::Mysql(_) => Self::Mysql,
        }
    }
}

/// Identifies a library for recents. Server connection strings can hold passwords, so all
/// libraries of one server kind share a key.
fn key(location: &Location) -> String {
    match location {
        Location::Local(_) => "personal".into(),
        Location::Sqlite(path) => format!("sqlite:{}", path.display()),
        Location::Mssql(_) => "mssql".into(),
        Location::Mysql(_) => "mysql".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recents_are_per_library_unique_and_bounded() {
        let mut state = State::default();
        let personal = Location::Local(PathBuf::from("connections.json"));
        let team = Location::Sqlite(PathBuf::from(r"\\server\team\relay.sqlite"));
        for id in [1, 2, 3, 2] {
            state.note_recent(&personal, id);
        }
        assert_eq!(state.recent(&personal), [2, 3, 1]);
        assert!(state.recent(&team).is_empty());
        for id in 0..20 {
            state.note_recent(&team, id);
        }
        assert_eq!(state.recent(&team).len(), RECENT);
        assert_eq!(state.recent(&team)[0], 19);
        for location in [
            Location::Mssql(crate::database::Server {
                connection: "Password=secret".into(),
                tls: true,
            }),
            Location::Mysql(crate::database::Server {
                connection: "mysql://relay:secret@db/relay".into(),
                tls: false,
            }),
        ] {
            state.note_recent(&location, 7);
            state.use_library(&location);
        }
        assert!(state.mysql_without_tls && !state.mssql_without_tls);
        let json = serde_json::to_string(&state).unwrap();
        assert!(!json.contains("secret"), "{json}");
    }

    #[test]
    fn snippets_match_name_or_command() {
        let snippet = |name: &str, command: &str| Snippet {
            name: name.into(),
            command: command.into(),
            run: true,
        };
        let snippets = [
            snippet("Tail syslog", "tail -f /var/log/syslog"),
            snippet("Disk usage", "df -h"),
            snippet("Restart nginx", "sudo systemctl restart nginx"),
        ];
        assert_eq!(matching(&snippets, ""), [0, 1, 2]);
        assert_eq!(matching(&snippets, "NGINX"), [2]);
        assert_eq!(matching(&snippets, "log"), [0]);
        assert_eq!(matching(&snippets, " df "), [1]);
        assert!(matching(&snippets, "reboot").is_empty());
    }

    #[test]
    fn state_roundtrips_and_tolerates_bad_files() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("state-test-{}.json", std::process::id()));
        let state = State {
            window: Some([1400., 900.]),
            maximized: true,
            sidebar_collapsed: Some(true),
            library: Library::Sqlite(PathBuf::from(r"\\server\team\relay.sqlite")),
            ..State::default()
        };
        state.save(&path).unwrap();
        assert_eq!(load(&path), state);
        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(load(&path), State::default());
        std::fs::remove_file(&path).unwrap();
        assert_eq!(load(&path), State::default());
    }
}
