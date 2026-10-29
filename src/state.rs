//! Workspace state kept between launches: window size, sidebar, last library, and recents.
use crate::database::Location;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// How many recent connections each library keeps.
const RECENT: usize = 12;

/// A library: the one to reopen at startup, and the one each tab came from. Server libraries
/// reopen only with a remembered connection string.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(from = "SavedLibrary")]
pub enum Library {
    #[default]
    Personal,
    Sqlite(PathBuf),
    /// A server library by its server and database, never its credentials, as connection IDs
    /// belong to one database. `None` was saved before Relay recorded which, and matches no
    /// open library.
    Mssql(Option<String>),
    Mysql(Option<String>),
}

/// A library as saved by this or an earlier Relay, which named server libraries by kind alone.
#[derive(Deserialize)]
#[serde(untagged)]
enum SavedLibrary {
    Kind(String),
    Place(SavedPlace),
}

#[derive(Deserialize)]
enum SavedPlace {
    Sqlite(PathBuf),
    Mssql(Option<String>),
    Mysql(Option<String>),
}

impl From<SavedLibrary> for Library {
    fn from(saved: SavedLibrary) -> Self {
        match saved {
            SavedLibrary::Kind(kind) if kind == "Mssql" => Self::Mssql(None),
            SavedLibrary::Kind(kind) if kind == "Mysql" => Self::Mysql(None),
            SavedLibrary::Kind(_) => Self::Personal,
            SavedLibrary::Place(SavedPlace::Sqlite(path)) => Self::Sqlite(path),
            SavedLibrary::Place(SavedPlace::Mssql(server)) => Self::Mssql(server),
            SavedLibrary::Place(SavedPlace::Mysql(server)) => Self::Mysql(server),
        }
    }
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
    /// Start with no tabs instead of reopening last run's, and keep no record of them.
    pub forget_tabs: bool,
    /// Which of `open` are SFTP file browsers rather than terminals. A separate list keeps
    /// `open` readable by earlier versions, which restore them as terminals.
    pub file_tabs: Vec<usize>,
    /// Which of `open` are administrative remote desktop sessions, kept apart like `file_tabs`.
    pub admin_tabs: Vec<usize>,
    /// The library each of `open` came from, as their IDs belong to it. Tabs saved without
    /// one came from `library`.
    pub tab_libraries: Vec<Library>,
    /// The folder downloads from the file browser went to last.
    pub download_folder: Option<PathBuf>,
    /// Which of `open` are local shells, kept apart like `file_tabs`.
    pub local_tabs: Vec<usize>,
    /// Tabs among `open` shown side by side, kept apart like `file_tabs`.
    pub pairs: Vec<SavedPair>,
    pub terminal: TerminalSettings,
}

/// Two of `open` shown side by side in one tab.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedPair {
    pub left: usize,
    pub right: usize,
    /// Where the divider sits, as a fraction of the width.
    pub fraction: f32,
    /// Whether the right side had the keyboard last.
    pub right_focused: bool,
}

impl Default for SavedPair {
    fn default() -> Self {
        Self {
            left: 0,
            right: 0,
            fraction: 0.5,
            right_focused: false,
        }
    }
}

/// How terminals look and whether they keep logs, the same for every tab.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalSettings {
    pub font_size: f32,
    /// A font file to use instead of the system's monospace font.
    pub font: Option<PathBuf>,
    pub scheme: crate::terminal::Scheme,
    /// Programs in a terminal may put text on the clipboard (OSC 52), as tmux and vim do.
    pub programs_copy: bool,
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            font_size: crate::terminal::FONT_SIZE,
            font: None,
            scheme: Default::default(),
            programs_copy: true,
        }
    }
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
            Location::Mssql(_) => Self::Mssql(Some(location.detail())),
            Location::Mysql(_) => Self::Mysql(Some(location.detail())),
        }
    }
}

/// Identifies a library for recents. Server connection strings can hold passwords, so server
/// libraries go by their server and database.
fn key(location: &Location) -> String {
    match location {
        Location::Local(_) => "personal".into(),
        Location::Sqlite(path) => format!("sqlite:{}", path.display()),
        Location::Mssql(_) => format!("mssql:{}", location.detail()),
        Location::Mysql(_) => format!("mysql:{}", location.detail()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(connection: &str) -> crate::database::Server {
        crate::database::Server {
            connection: connection.into(),
            tls: true,
        }
    }

    #[test]
    fn server_libraries_are_told_apart_by_server_and_database() {
        let a = Location::Mssql(server("Server=sql01;Database=A;Password=secret"));
        let b = Location::Mssql(server("Server=sql01;Database=B;Password=secret"));
        assert_ne!(Library::of(&a), Library::of(&b));
        let mut state = State::default();
        state.note_recent(&a, 1);
        assert!(
            state.recent(&b).is_empty(),
            "recents stay with their database"
        );
        let json = serde_json::to_string(&Library::of(&a)).unwrap();
        assert!(!json.contains("secret"), "{json}");
        assert_eq!(
            serde_json::from_str::<Library>(&json).unwrap(),
            Library::of(&a)
        );
    }

    #[test]
    fn libraries_saved_by_earlier_versions_still_load() {
        let read = |json: &str| serde_json::from_str::<Library>(json).unwrap();
        assert_eq!(read(r#""Personal""#), Library::Personal);
        assert_eq!(
            read(r#"{"Sqlite":"team.sqlite"}"#),
            Library::Sqlite(PathBuf::from("team.sqlite"))
        );
        // Which server they came from is unknown, so they match no open library.
        assert_eq!(read(r#""Mssql""#), Library::Mssql(None));
        assert_eq!(read(r#""Mysql""#), Library::Mysql(None));
        let a = Location::Mssql(server("Server=sql01;Database=A"));
        assert_ne!(Library::of(&a), Library::Mssql(None));
    }
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
