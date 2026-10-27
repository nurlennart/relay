use crate::model::{Connection, Protocol};
use std::{collections::HashSet, path::Path};

const COLUMNS: [&str; 17] = [
    "ConnectionId",
    "ConnectionName",
    "ConnectionDestination",
    "ConnectionDescription",
    "ConnectionExternalId",
    "ConnectionImageId",
    "ConnectionPort",
    "ConnectionUrl",
    "ConnectionIpAddress",
    "ConnectionMACAddress",
    "ConnectionProtocol",
    "ConnectionCustom1",
    "ConnectionCustom2",
    "ConnectionCustom3",
    "FolderName",
    "Username",
    "Favorite",
];

/// A synthetic CSV in the full export schema, used by tests and UI previews.
#[cfg(any(test, feature = "ui-check"))]
pub const FIXTURE: &str = "tests/fixtures/export-sample.csv";

pub struct Import {
    pub connections: Vec<Connection>,
    pub skipped: Vec<String>,
    pub duplicates: usize,
    pub filename: String,
}

impl Import {
    /// `existing` with the imported connections added, numbered and checked for duplicates
    /// against `existing` as it is now; a shared library may have changed since the import
    /// was read.
    pub fn merged(&self, existing: &[Connection]) -> Result<Vec<Connection>, String> {
        let mut collector = Collector::new(existing, self.filename.clone());
        for c in &self.connections {
            collector.add(&c.name, c.clone())?;
        }
        let mut next = existing.to_vec();
        next.extend(collector.finish().connections);
        Ok(next)
    }
}

pub fn read(path: &Path, existing: &[Connection]) -> Result<Import, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes.len() > 10 * 1024 * 1024 {
        return Err("CSV files must be smaller than 10 MB.".into());
    }
    let text = if bytes.starts_with(&[0xff, 0xfe]) {
        encoding_rs::UTF_16LE.decode(&bytes[2..]).0.into_owned()
    } else if bytes.starts_with(&[0xfe, 0xff]) {
        encoding_rs::UTF_16BE.decode(&bytes[2..]).0.into_owned()
    } else {
        match std::str::from_utf8(&bytes) {
            Ok(s) => s.trim_start_matches('\u{feff}').to_owned(),
            Err(_) => encoding_rs::WINDOWS_1252.decode(&bytes).0.into_owned(),
        }
    };
    parse(
        &text,
        existing,
        path.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into(),
    )
}

// Cells starting with these characters can run as spreadsheet formulas.
const FORMULA: [char; 6] = ['=', '+', '-', '@', '\t', '\r'];

/// Prefixes an apostrophe to formula-like cells, and to cells already starting with one so that
/// `unguard` can restore every value exactly.
fn guard(s: String) -> String {
    if s.starts_with(FORMULA) || s.starts_with('\'') {
        format!("'{s}")
    } else {
        s
    }
}

fn unguard(s: &str) -> &str {
    match s.strip_prefix('\'') {
        Some(rest) if rest.starts_with(FORMULA) || rest.starts_with('\'') => rest,
        _ => s,
    }
}

/// Validates imported connections, skips duplicates of existing or earlier ones, and assigns
/// IDs after the existing library.
pub struct Collector {
    import: Import,
    seen: HashSet<String>,
    next_id: u64,
}

impl Collector {
    pub fn new(existing: &[Connection], filename: String) -> Self {
        Self {
            import: Import {
                connections: vec![],
                skipped: vec![],
                duplicates: 0,
                filename,
            },
            seen: existing.iter().map(identity).collect(),
            next_id: existing.iter().map(|c| c.id).max().unwrap_or(0),
        }
    }
    pub fn skip(&mut self, reason: String) {
        self.import.skipped.push(reason);
    }
    /// Adds `c`, or records why it was skipped; `source` names it in that reason.
    pub fn add(&mut self, source: &str, mut c: Connection) -> Result<(), String> {
        if let Err(e) = c.validate() {
            self.skip(format!("{source}: {e}"));
            return Ok(());
        }
        if !self.seen.insert(identity(&c)) {
            self.import.duplicates += 1;
            return Ok(());
        }
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or("Connection ID limit reached")?;
        c.id = self.next_id;
        self.import.connections.push(c);
        Ok(())
    }
    pub fn finish(self) -> Import {
        self.import
    }
}

fn identity(c: &Connection) -> String {
    if let Some(id) = c.metadata.get("ConnectionId").filter(|v| !v.is_empty()) {
        format!("id:{}", id.to_lowercase())
    } else {
        format!(
            "{}|{}|{}|{}|{}|{}",
            c.protocol.label(),
            c.host.to_lowercase(),
            c.port,
            c.username,
            c.name,
            c.group
        )
    }
}

fn parse(text: &str, existing: &[Connection], filename: String) -> Result<Import, String> {
    let delimiter = if text.lines().next().unwrap_or("").contains(';') {
        b';'
    } else {
        b','
    };
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .trim(csv::Trim::All)
        .from_reader(text.as_bytes());
    let headers = reader.headers().map_err(|e| e.to_string())?.clone();
    let normalized: Vec<_> = headers
        .iter()
        .map(|h| h.to_ascii_lowercase().replace(['_', ' '], ""))
        .collect();
    let index = |names: &[&str]| normalized.iter().position(|h| names.contains(&h.as_str()));
    let name =
        index(&["connectionname", "name"]).ok_or("CSV needs a ConnectionName or Name column.")?;
    let host = index(&["connectiondestination", "host", "hostname"])
        .ok_or("CSV needs a ConnectionDestination or Host column.")?;
    let protocol = index(&["connectionprotocol", "protocol"])
        .ok_or("CSV needs a ConnectionProtocol or Protocol column.")?;
    let port = index(&["connectionport", "port"]);
    let username = index(&["username", "user"]);
    let group = index(&["foldername", "group"]);
    let favorite = index(&["favorite"]);
    let ip = index(&["connectionipaddress"]);
    let mut result = Collector::new(existing, filename);
    for (i, row) in reader.records().enumerate() {
        let record = match row {
            Ok(row) => row,
            Err(e) => {
                result.skip(format!("Row {}: {e}", i + 2));
                continue;
            }
        };
        let value =
            |idx: Option<usize>| unguard(idx.and_then(|i| record.get(i)).unwrap_or("")).to_string();
        let raw_protocol = value(Some(protocol));
        let protocol = match raw_protocol.to_uppercase().as_str() {
            "RDP" => Protocol::Rdp,
            "SSH" => Protocol::Ssh,
            _ => {
                result.skip(format!(
                    "Row {}: unsupported protocol {raw_protocol:?}",
                    i + 2
                ));
                continue;
            }
        };
        let raw_port = value(port);
        let port = if raw_port.is_empty() {
            protocol.port()
        } else {
            match raw_port.parse::<u16>() {
                Ok(p) => p,
                Err(_) => {
                    result.skip(format!("Row {}: invalid port", i + 2));
                    continue;
                }
            }
        };
        let mut c = Connection {
            name: value(Some(name)),
            host: value(Some(host)),
            username: value(username),
            group: value(group),
            protocol,
            port,
            favorite: matches!(
                value(favorite).to_lowercase().as_str(),
                "true" | "1" | "yes"
            ),
            ..Connection::default()
        };
        if c.host.is_empty() {
            c.host = value(ip);
        }
        for column in COLUMNS {
            if let Some(idx) = headers.iter().position(|h| h.eq_ignore_ascii_case(column)) {
                c.metadata.insert(column.into(), value(Some(idx)));
            }
        }
        c.notes = c
            .metadata
            .remove("ConnectionDescription")
            .unwrap_or_default();
        let source = format!("Row {} ({})", i + 2, c.name);
        result.add(&source, c)?;
    }
    Ok(result.finish())
}

pub fn export(path: &Path, connections: &[Connection]) -> Result<(), String> {
    let mut writer = csv::WriterBuilder::new()
        .delimiter(b';')
        .from_writer(vec![]);
    writer.write_record(COLUMNS).map_err(|e| e.to_string())?;
    for c in connections {
        let mut values = c.metadata.clone();
        for (key, value) in [
            ("ConnectionName", c.name.clone()),
            ("ConnectionDestination", c.host.clone()),
            ("ConnectionPort", c.port.to_string()),
            ("ConnectionProtocol", c.protocol.label().into()),
            ("FolderName", c.group.clone()),
            ("ConnectionDescription", c.notes.clone()),
            ("Username", c.username.clone()),
            ("Favorite", c.favorite.to_string()),
        ] {
            values.insert(key.into(), value);
        }
        let cells: Vec<_> = COLUMNS
            .iter()
            .map(|key| guard(values.get(*key).cloned().unwrap_or_default()))
            .collect();
        writer.write_record(cells).map_err(|e| e.to_string())?;
    }
    let bytes = writer.into_inner().map_err(|e| e.to_string())?;
    crate::model::atomic_write(path, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn imports_export_fixture_and_deduplicates() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
        let first = read(&fixture, &[]).unwrap();
        let count = |protocol| {
            first
                .connections
                .iter()
                .filter(|c| c.protocol == protocol)
                .count()
        };
        assert_eq!((count(Protocol::Rdp), count(Protocol::Ssh)), (4, 2));
        assert_eq!(first.skipped.len(), 1, "{:?}", first.skipped);
        assert!(first.skipped[0].contains("VNC"));
        assert_eq!(first.duplicates, 1);
        let find = |name: &str| first.connections.iter().find(|c| c.name == name).unwrap();
        let lab = find("Lab node");
        assert_eq!(
            (lab.host.as_str(), lab.port, lab.favorite),
            ("192.0.2.77", 3389, true)
        );
        assert_eq!(find("Reception; front desk").port, 3389);
        assert_eq!(find("Design workstation").username, "STUDIO\\alex");
        assert_eq!(find("Archive").notes, "Old \"cold\" storage");
        let second = read(&fixture, &first.connections).unwrap();
        assert_eq!(second.duplicates, 7);
        assert!(second.connections.is_empty());

        let mut utf16 = vec![0xff, 0xfe];
        for unit in std::fs::read_to_string(&fixture).unwrap().encode_utf16() {
            utf16.extend(unit.to_le_bytes());
        }
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("utf16-test-{}.csv", std::process::id()));
        std::fs::write(&path, utf16).unwrap();
        let decoded = read(&path, &[]);
        std::fs::remove_file(path).unwrap();
        assert_eq!(decoded.unwrap().connections.len(), first.connections.len());
    }
    #[test]
    fn formula_guard_roundtrips_exactly() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("guard-test-{}.csv", std::process::id()));
        let names = [
            "-backup", "+test", "=SUM(1)", "@home", "'quoted", "''twice", "plain",
        ];
        let connections: Vec<_> = names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let mut c = Connection::new();
                c.id = i as u64 + 1;
                c.name = (*name).into();
                c.host = format!("host{i}.example");
                c
            })
            .collect();
        export(&path, &connections).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(";'-backup;") && text.contains(";'=SUM(1);"));
        let import = read(&path, &[]).unwrap();
        std::fs::remove_file(path).unwrap();
        let imported: Vec<_> = import.connections.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(imported, names);
    }
    #[test]
    fn merging_numbers_against_the_library_as_it_is_now() {
        let text = "Name,Host,Protocol\nA,a.example,SSH\nB,b.example,SSH\n";
        let import = parse(text, &[], "test.csv".into()).unwrap();
        assert_eq!(import.connections[0].id, 1);
        // While the import was reviewed, a teammate added B as ID 1.
        let mut theirs = import.connections[1].clone();
        theirs.id = 1;
        let merged = import.merged(&[theirs]).unwrap();
        let ids: Vec<_> = merged.iter().map(|c| (c.id, c.name.as_str())).collect();
        assert_eq!(ids, [(1, "B"), (2, "A")]);
    }
    #[test]
    fn quoted_fields_defaults_and_rejected_rows() {
        let text = "Name,Host,Protocol,Group\n\
            \"A, B\",localhost,SSH,Office\n\
            Bad,-host,RDP,Office\n\
            Other,localhost,VNC,Office\n";
        let result = parse(text, &[], "test.csv".into()).unwrap();
        assert_eq!(result.connections[0].name, "A, B");
        assert_eq!(result.connections[0].port, 22);
        assert_eq!(result.skipped.len(), 2);
    }
    #[test]
    fn export_roundtrips_fields_and_metadata() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("export-test-{}.csv", std::process::id()));
        let mut c = Connection::new();
        c.id = 1;
        c.name = "Office; Büro".into();
        c.host = "localhost".into();
        c.username = "DOMAIN\\alex".into();
        c.favorite = true;
        c.notes = "Quoted \"description\"\nSecond line".into();
        c.metadata
            .insert("ConnectionExternalId".into(), "ext-7".into());
        export(&path, std::slice::from_ref(&c)).unwrap();
        let import = read(&path, &[]).unwrap();
        assert!(import.skipped.is_empty());
        assert_eq!(import.connections[0].name, c.name);
        assert_eq!(import.connections[0].username, c.username);
        assert!(import.connections[0].favorite);
        assert_eq!(import.connections[0].notes, c.notes);
        assert_eq!(
            import.connections[0].metadata["ConnectionExternalId"],
            "ext-7"
        );
        std::fs::remove_file(path).unwrap();
    }
}
