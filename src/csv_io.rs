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

pub struct Import {
    pub connections: Vec<Connection>,
    pub skipped: Vec<String>,
    pub duplicates: usize,
    pub filename: String,
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
    let mut result = Import {
        connections: vec![],
        skipped: vec![],
        duplicates: 0,
        filename,
    };
    let mut seen: HashSet<_> = existing.iter().map(identity).collect();
    let mut next_id = existing.iter().map(|c| c.id).max().unwrap_or(0);
    for (i, row) in reader.records().enumerate() {
        let record = match row {
            Ok(row) => row,
            Err(e) => {
                result.skipped.push(format!("Row {}: {e}", i + 2));
                continue;
            }
        };
        let value = |idx: Option<usize>| idx.and_then(|i| record.get(i)).unwrap_or("").to_string();
        let raw_protocol = value(Some(protocol));
        let protocol = match raw_protocol.to_uppercase().as_str() {
            "RDP" => Protocol::Rdp,
            "SSH" => Protocol::Ssh,
            _ => {
                result.skipped.push(format!(
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
                    result.skipped.push(format!("Row {}: invalid port", i + 2));
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
        if let Err(e) = c.validate() {
            result
                .skipped
                .push(format!("Row {} ({}): {e}", i + 2, c.name));
            continue;
        }
        if !seen.insert(identity(&c)) {
            result.duplicates += 1;
            continue;
        }
        next_id = next_id
            .checked_add(1)
            .ok_or("Connection ID limit reached")?;
        c.id = next_id;
        result.connections.push(c);
    }
    Ok(result)
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
            ("Username", c.username.clone()),
            ("Favorite", c.favorite.to_string()),
        ] {
            values.insert(key.into(), value);
        }
        // Keep exports safe when opened in spreadsheet software.
        let cells: Vec<_> = COLUMNS
            .iter()
            .map(|key| {
                let s = values.get(*key).cloned().unwrap_or_default();
                if s.starts_with(['=', '+', '-', '@']) {
                    format!("'{s}")
                } else {
                    s
                }
            })
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
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("export.csv");
        if !fixture.exists() {
            return;
        }
        let first = read(&fixture, &[]).unwrap();
        assert_eq!(
            first
                .connections
                .iter()
                .filter(|c| c.protocol == Protocol::Rdp)
                .count(),
            69
        );
        assert_eq!(
            first
                .connections
                .iter()
                .filter(|c| c.protocol == Protocol::Ssh)
                .count(),
            2
        );
        assert_eq!(first.skipped.len(), 8);
        let second = read(&fixture, &first.connections).unwrap();
        assert_eq!(second.duplicates, 71);
        assert!(second.connections.is_empty());
    }
    #[test]
    fn quoted_fields_defaults_and_rejected_rows() {
        let text = "Name,Host,Protocol,Group\n\"A, B\",localhost,SSH,Office\nBad,-host,RDP,Office\nOther,localhost,VNC,Office\n";
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
        c.metadata.insert(
            "ConnectionDescription".into(),
            "Quoted \"description\"".into(),
        );
        export(&path, std::slice::from_ref(&c)).unwrap();
        let import = read(&path, &[]).unwrap();
        assert!(import.skipped.is_empty());
        assert_eq!(import.connections[0].name, c.name);
        assert_eq!(import.connections[0].username, c.username);
        assert!(import.connections[0].favorite);
        assert_eq!(
            import.connections[0].metadata["ConnectionDescription"],
            c.metadata["ConnectionDescription"]
        );
        std::fs::remove_file(path).unwrap();
    }
}
