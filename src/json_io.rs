//! Relay's own export format: JSON that keeps every setting of each connection.
use crate::{
    csv_io::{Collector, Import},
    model::{self, Connection},
};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// The format version written to exports; newer files are refused rather than half read.
const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct Export<C> {
    relay: u32,
    connections: C,
}

pub fn export(path: &Path, connections: &[Connection]) -> Result<(), String> {
    let export = Export {
        relay: VERSION,
        connections,
    };
    let bytes = serde_json::to_vec_pretty(&export).map_err(|e| e.to_string())?;
    model::atomic_write(path, &bytes)
}

/// Reads an export, or a personal library file, for review against `existing`.
pub fn read(path: &Path, existing: &[Connection]) -> Result<Import, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    let filename = path.file_name().unwrap_or_default().to_string_lossy();
    parse(&bytes, existing, filename.into())
}

fn parse(bytes: &[u8], existing: &[Connection], filename: String) -> Result<Import, String> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum File {
        Export(Export<Vec<serde_json::Value>>),
        Library(Vec<serde_json::Value>),
    }
    let unreadable = |e| format!("{filename} is not a Relay export or library: {e}");
    let entries = match serde_json::from_slice(bytes).map_err(unreadable)? {
        File::Export(export) if export.relay > VERSION => {
            return Err(format!(
                "{filename} was exported by a newer version of Relay. Update Relay to import it."
            ));
        }
        File::Export(export) => export.connections,
        File::Library(connections) => connections,
    };
    let mut result = Collector::new(existing, filename);
    // One unreadable entry is skipped like an invalid CSV row, not the whole file.
    for (i, entry) in entries.into_iter().enumerate() {
        let name = entry.get("name").and_then(|n| n.as_str()).unwrap_or("");
        let source = format!("Entry {} ({name})", i + 1);
        match serde_json::from_value::<Connection>(entry) {
            Ok(mut c) => {
                c.adopt_description();
                result.add(&source, c)?;
            }
            Err(e) => result.skip(format!("{source}: {e}")),
        }
    }
    Ok(result.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Protocol;

    #[test]
    fn exports_round_trip_every_setting() {
        let mut ssh = Connection::new();
        ssh.id = 7;
        ssh.name = "Build".into();
        ssh.host = "build.example".into();
        ssh.protocol = Protocol::Ssh;
        ssh.port = 22;
        ssh.notes = "Rack 4".into();
        ssh.ssh.jump = "bastion.example".into();
        ssh.ssh.identity = "~/.ssh/build".into();
        ssh.ssh.forwards = "L8080:localhost:80".into();
        let mut rdp = Connection::new();
        rdp.id = 9;
        rdp.name = "Desk".into();
        rdp.host = "desk.example".into();
        rdp.rdp.gateway = "gw.example".into();
        rdp.rdp.drives = true;
        rdp.rdp.display = model::Display::Fixed {
            width: 1920,
            height: 1080,
        };
        rdp.metadata
            .insert("ConnectionUrl".into(), "https://desk".into());
        let path = std::env::temp_dir().join(format!("relay-export-{}.json", std::process::id()));
        export(&path, &[ssh.clone(), rdp.clone()]).unwrap();
        let import = read(&path, &[]).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(import.skipped.is_empty());
        // Only the IDs change, as they are numbered against the library imported into.
        ssh.id = 1;
        rdp.id = 2;
        assert_eq!(import.connections, [ssh.clone(), rdp]);
        // A personal library file imports too; what the library has is skipped.
        let library = serde_json::to_vec(&[&ssh]).unwrap();
        let again = parse(&library, &import.connections, "connections.json".into()).unwrap();
        assert_eq!((again.connections.len(), again.duplicates), (0, 1));
    }

    #[test]
    fn newer_exports_and_bad_entries_are_reported() {
        let newer = br#"{"relay": 2, "connections": []}"#;
        let error = parse(newer, &[], "next.json".into()).err().unwrap();
        assert!(error.contains("newer version"));
        let mixed = br#"{"relay": 1, "connections": [
            {"name": "Odd", "host": 5},
            {"id": 3, "name": "Desk", "host": "desk.example", "username": "", "group": "",
             "protocol": "Rdp", "port": 3389, "favorite": false}
        ]}"#;
        let import = parse(mixed, &[], "mixed.json".into()).unwrap();
        assert_eq!(import.connections.len(), 1);
        assert_eq!(import.skipped.len(), 1);
        assert!(import.skipped[0].starts_with("Entry 1 (Odd)"));
    }
}
