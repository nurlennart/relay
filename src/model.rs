use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Protocol {
    #[default]
    Rdp,
    Ssh,
}

impl Protocol {
    pub fn label(self) -> &'static str {
        match self {
            Self::Rdp => "RDP",
            Self::Ssh => "SSH",
        }
    }
    pub fn port(self) -> u16 {
        match self {
            Self::Rdp => 3389,
            Self::Ssh => 22,
        }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Connection {
    pub id: u64,
    pub name: String,
    pub host: String,
    pub username: String,
    pub group: String,
    pub protocol: Protocol,
    pub port: u16,
    pub favorite: bool,
    #[serde(default)]
    pub metadata: std::collections::BTreeMap<String, String>,
}

impl Connection {
    pub fn new() -> Self {
        Self {
            port: 3389,
            group: "Personal".into(),
            ..Self::default()
        }
    }
    pub fn validate(&mut self) -> Result<(), String> {
        self.name = self.name.trim().into();
        self.host = self.host.trim().into();
        self.username = self.username.trim().into();
        self.group = self.group.trim().into();
        if self.name.is_empty() {
            return Err("Give this connection a name.".into());
        }
        if self.host.is_empty()
            || self.host.starts_with('-')
            || self
                .host
                .chars()
                .any(|c| !(c.is_ascii_alphanumeric() || ".-:_".contains(c)))
        {
            return Err(
                "Enter a hostname, IPv4 address, or bare IPv6 address (without a port).".into(),
            );
        }
        if self.host.contains(':') && self.host.parse::<std::net::Ipv6Addr>().is_err() {
            return Err("Use the separate Port field. \
                 IPv6 addresses must be bare addresses, such as 2001:db8::1."
                .into());
        }
        if self.username.starts_with('-') || self.username.chars().any(|c| c.is_control()) {
            return Err(
                "The username cannot start with a dash or contain control characters.".into(),
            );
        }
        if self
            .name
            .chars()
            .chain(self.group.chars())
            .any(|c| c.is_control())
        {
            return Err("Names cannot contain control characters.".into());
        }
        if self.port == 0 {
            return Err("Port must be between 1 and 65535.".into());
        }
        if self.group.is_empty() {
            self.group = "Personal".into();
        }
        Ok(())
    }
    pub fn address(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
    pub fn matches(&self, query: &str) -> bool {
        [
            &self.name,
            &self.host,
            &self.group,
            &self.username,
            self.protocol.label(),
        ]
        .iter()
        .any(|s| s.to_lowercase().contains(query))
    }
}

pub fn data_path() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Relay")
        .join("connections.json")
}

pub fn load(path: &Path) -> Result<Vec<Connection>, String> {
    match fs::read(path) {
        Ok(bytes) => decode(&bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) => Err(format!("Cannot open connections: {e}")),
    }
}

pub fn decode(bytes: &[u8]) -> Result<Vec<Connection>, String> {
    let mut connections: Vec<Connection> = serde_json::from_slice(bytes).map_err(|e| {
        format!("Cannot read saved connections: {e}. The file has been left untouched.")
    })?;
    let mut ids = std::collections::HashSet::new();
    for c in &mut connections {
        c.validate()
            .map_err(|e| format!("Invalid saved connection: {e}"))?;
        if c.id == 0 || !ids.insert(c.id) {
            return Err(
                "Saved connections contain duplicate IDs. The file has been left untouched.".into(),
            );
        }
    }
    Ok(connections)
}

pub fn save(path: &Path, connections: &[Connection]) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(connections).map_err(|e| e.to_string())?;
    atomic_write(path, &bytes)
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    fs::create_dir_all(path.parent().ok_or("Invalid storage path")?).map_err(|e| e.to_string())?;
    let temp = path.with_file_name(format!(
        "{}.tmp",
        path.file_name()
            .ok_or("Invalid storage filename")?
            .to_string_lossy()
    ));
    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temp)
        .map_err(|e| e.to_string())?;
    use std::io::Write;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    // MoveFileEx replaces atomically on Windows, including when the destination already exists.
    use std::os::windows::ffi::OsStrExt;
    let from: Vec<u16> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    drop(file);
    unsafe {
        if windows_sys::Win32::Storage::FileSystem::MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            windows_sys::Win32::Storage::FileSystem::MOVEFILE_REPLACE_EXISTING
                | windows_sys::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH,
        ) == 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_hosts_and_ports() {
        let mut c = Connection::new();
        c.name = " Test ".into();
        for host in ["server.example", "10.0.0.1", "2001:db8::1"] {
            c.host = host.into();
            assert!(c.validate().is_ok());
        }
        for host in [
            "",
            "-option",
            "host\nusername:s:evil",
            "host /admin",
            "user@host",
            "[::1]",
        ] {
            c.host = host.into();
            assert!(c.validate().is_err());
        }
        c.host = "localhost".into();
        c.port = 0;
        assert!(c.validate().is_err());
    }
    #[test]
    fn serialization_and_search() {
        let mut c = Connection::new();
        c.name = "Gateway".into();
        c.host = "::1".into();
        c.protocol = Protocol::Ssh;
        assert_eq!(c.address(), "[::1]:3389");
        assert!(c.matches("gateway"));
        assert!(c.matches("ssh"));
        let json = serde_json::to_string(&c).unwrap();
        assert!(!json.contains("password"));
        assert_eq!(
            serde_json::from_str::<Connection>(&json).unwrap().host,
            "::1"
        );
    }
    #[test]
    fn saves_replaces_and_preserves_corrupt_data() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("relay-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("connections.json");
        assert!(load(&path).unwrap().is_empty());
        let mut c = Connection::new();
        c.name = "Test".into();
        c.id = 1;
        c.host = "localhost".into();
        save(&path, &[c.clone()]).unwrap();
        c.name = "Updated".into();
        save(&path, &[c]).unwrap();
        assert_eq!(load(&path).unwrap()[0].name, "Updated");
        fs::write(&path, "broken").unwrap();
        assert!(load(&path).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "broken");
        fs::remove_file(path).unwrap();
        fs::remove_dir(dir).unwrap();
    }
}
