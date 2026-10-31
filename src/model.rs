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
    Vnc,
}

impl Protocol {
    pub const ALL: [Self; 3] = [Self::Rdp, Self::Ssh, Self::Vnc];
    pub fn label(self) -> &'static str {
        match self {
            Self::Rdp => "RDP",
            Self::Ssh => "SSH",
            Self::Vnc => "VNC",
        }
    }
    pub fn port(self) -> u16 {
        match self {
            Self::Rdp => 3389,
            Self::Ssh => 22,
            Self::Vnc => 5900,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Connection {
    pub id: u64,
    pub name: String,
    pub host: String,
    pub username: String,
    pub group: String,
    pub protocol: Protocol,
    pub port: u16,
    pub favorite: bool,
    /// Free-form notes, searched along with the other fields.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
    #[serde(default)]
    pub metadata: std::collections::BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "RdpOptions::is_default")]
    pub rdp: RdpOptions,
    #[serde(default, skip_serializing_if = "SshOptions::is_default")]
    pub ssh: SshOptions,
    #[serde(default, skip_serializing_if = "VncOptions::is_default")]
    pub vnc: VncOptions,
}

/// How a VNC connection shares this computer with the remote one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VncOptions {
    /// Copied text crosses in both directions.
    pub clipboard: bool,
    /// Shows the remote screen without sending keys or the pointer.
    pub view_only: bool,
}

impl Default for VncOptions {
    fn default() -> Self {
        Self {
            clipboard: true,
            view_only: false,
        }
    }
}

impl VncOptions {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// OpenSSH options Relay passes on the command line.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SshOptions {
    /// Jump hosts as for `ssh -J`: `[user@]host[:port]`, comma separated.
    pub jump: String,
    /// A private key file for `ssh -i`.
    pub identity: String,
    /// Port forwards, comma separated: `L[bind:]port:host:hostport`, `R...`, or `D[bind:]port`.
    pub forwards: String,
}

impl SshOptions {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// The forwards as ssh flags and values, such as `("-L", "8080:localhost:80")`.
    pub fn forward_args(&self) -> Result<Vec<(&'static str, String)>, String> {
        let invalid = |item: &str| {
            format!(
                "Port forward {item:?} is not valid. Use L8080:localhost:80, \
                 R9000:localhost:9000 or D1080, separated by commas."
            )
        };
        self.forwards
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(|item| {
                let (flag, spec) = match item.get(..1).map(str::to_ascii_uppercase).as_deref() {
                    Some("L") => ("-L", &item[1..]),
                    Some("R") => ("-R", &item[1..]),
                    Some("D") => ("-D", &item[1..]),
                    _ => return Err(invalid(item)),
                };
                let spec = spec.trim();
                let Some(parts) = fields(spec) else {
                    return Err(invalid(item));
                };
                let shape = if flag == "-D" {
                    parts <= 2
                } else {
                    (3..=4).contains(&parts)
                };
                let safe = !spec.starts_with('-')
                    && spec
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || ".-_:[]*".contains(c));
                if spec.is_empty() || !shape || !safe {
                    return Err(invalid(item));
                }
                Ok((flag, spec.to_string()))
            })
            .collect()
    }
}

/// How many fields a forward has: colons separate them, except inside the brackets around
/// an IPv6 address such as `[::1]`. `None` if the brackets do not pair up.
fn fields(spec: &str) -> Option<usize> {
    let (mut count, mut bracketed) = (1, false);
    for c in spec.chars() {
        match c {
            '[' if !bracketed => bracketed = true,
            ']' if bracketed => bracketed = false,
            '[' | ']' => return None,
            ':' if !bracketed => count += 1,
            _ => {}
        }
    }
    (!bracketed).then_some(count)
}

/// Where remote audio plays.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Audio {
    #[default]
    Local,
    Remote,
    Off,
}

/// Where system key combinations such as Alt+Tab apply.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Keys {
    Local,
    Remote,
    #[default]
    FullScreen,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Display {
    /// The remote resolution follows the tab size.
    #[default]
    Fit,
    /// A fixed remote resolution, scaled to the tab.
    Fixed { width: u16, height: u16 },
}

impl Audio {
    pub const ALL: [Self; 3] = [Self::Local, Self::Remote, Self::Off];
    pub fn label(self) -> &'static str {
        match self {
            Self::Local => "Play on this computer",
            Self::Remote => "Play on the remote computer",
            Self::Off => "Do not play",
        }
    }
}

impl Keys {
    pub const ALL: [Self; 3] = [Self::FullScreen, Self::Remote, Self::Local];
    pub fn label(self) -> &'static str {
        match self {
            Self::Local => "On this computer",
            Self::Remote => "On the remote computer",
            Self::FullScreen => "Remote only in full screen",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RdpOptions {
    /// RD Gateway host; empty connects directly.
    pub gateway: String,
    pub clipboard: bool,
    pub drives: bool,
    pub printers: bool,
    pub smart_cards: bool,
    pub microphone: bool,
    pub audio: Audio,
    pub keys: Keys,
    pub display: Display,
}

impl Default for RdpOptions {
    fn default() -> Self {
        Self {
            gateway: String::new(),
            clipboard: true,
            drives: false,
            printers: false,
            smart_cards: false,
            microphone: false,
            audio: Audio::Local,
            keys: Keys::FullScreen,
            display: Display::Fit,
        }
    }
}

impl RdpOptions {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Quick-connect prefixes and the protocol each means. Longer prefixes come first.
const QUICK_PREFIXES: [(&str, Protocol); 8] = [
    ("ssh://", Protocol::Ssh),
    ("rdp://", Protocol::Rdp),
    ("vnc://", Protocol::Vnc),
    ("sftp://", Protocol::Ssh),
    ("ssh ", Protocol::Ssh),
    ("rdp ", Protocol::Rdp),
    ("vnc ", Protocol::Vnc),
    ("sftp ", Protocol::Ssh),
];

/// Whether a quick-connect target asks for a file browser (`sftp://` or `sftp `).
pub fn browses(text: &str) -> bool {
    let text = text.trim_start();
    ["sftp://", "sftp "].iter().any(|prefix| {
        text.get(..prefix.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
    })
}

/// Accepts hostnames and addresses: no leading dash, no spaces or URL syntax.
fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && !host.starts_with('-')
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".-:_".contains(c))
}

/// `current` with the fields `draft` changed from `base`, so edits made in the meantime by
/// someone else are kept. Fields both sides changed to different values are named as
/// conflicts, with `draft`'s value in the result.
pub fn merge(
    base: &Connection,
    draft: &Connection,
    current: &Connection,
) -> (Connection, Vec<&'static str>) {
    // Naming every field here makes a new one fail to compile until it is merged below.
    let Connection {
        id: _,
        name: _,
        host: _,
        port: _,
        username: _,
        group: _,
        protocol: _,
        favorite: _,
        notes: _,
        metadata: _,
        ssh:
            SshOptions {
                jump: _,
                identity: _,
                forwards: _,
            },
        rdp:
            RdpOptions {
                gateway: _,
                clipboard: _,
                drives: _,
                printers: _,
                smart_cards: _,
                microphone: _,
                audio: _,
                keys: _,
                display: _,
            },
        vnc: VncOptions {
            clipboard: _,
            view_only: _,
        },
    } = base;
    let mut merged = current.clone();
    let mut conflicts = vec![];
    macro_rules! take {
        ($($($field:ident).+ => $label:literal),* $(,)?) => {$(
            if draft.$($field).+ != base.$($field).+ {
                if current.$($field).+ != base.$($field).+
                    && current.$($field).+ != draft.$($field).+
                {
                    conflicts.push($label);
                }
                merged.$($field).+ = draft.$($field).+.clone();
            }
        )*};
    }
    take! {
        name => "name",
        host => "host",
        port => "port",
        username => "username",
        group => "group",
        protocol => "protocol",
        favorite => "favorite",
        notes => "notes",
        metadata => "imported details",
        ssh.jump => "jump host",
        ssh.identity => "identity file",
        ssh.forwards => "port forwards",
        rdp.gateway => "RD Gateway",
        rdp.clipboard => "local resources",
        rdp.drives => "local resources",
        rdp.printers => "local resources",
        rdp.smart_cards => "local resources",
        rdp.microphone => "local resources",
        rdp.audio => "remote audio",
        rdp.keys => "key combinations",
        rdp.display => "resolution",
        vnc.clipboard => "clipboard",
        vnc.view_only => "view only",
    }
    (merged, conflicts)
}

impl Connection {
    pub fn new() -> Self {
        Self {
            port: 3389,
            group: "Personal".into(),
            ..Self::default()
        }
    }
    /// Moves a CSV description kept as metadata, before connections had notes, into the notes.
    pub fn adopt_description(&mut self) {
        if let Some(description) = self.metadata.remove("ConnectionDescription")
            && self.notes.is_empty()
        {
            self.notes = description;
        }
    }

    pub fn validate(&mut self) -> Result<(), String> {
        self.name = self.name.trim().into();
        self.host = self.host.trim().into();
        self.username = self.username.trim().into();
        self.group = self.group.trim().into();
        self.notes = self.notes.trim().replace("\r\n", "\n");
        if self.name.is_empty() {
            return Err("Give this connection a name.".into());
        }
        if !valid_host(&self.host) {
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
        self.rdp.gateway = self.rdp.gateway.trim().into();
        if !self.rdp.gateway.is_empty() && !valid_host(&self.rdp.gateway) {
            return Err("Enter the RD Gateway as a hostname, such as gateway.example.com.".into());
        }
        self.ssh.jump = self.ssh.jump.trim().into();
        self.ssh.identity = self.ssh.identity.trim().trim_matches('"').into();
        self.ssh.forwards = self.ssh.forwards.trim().into();
        let jump_valid = self.ssh.jump.split(',').map(str::trim).all(|hop| {
            !hop.is_empty()
                && !hop.starts_with('-')
                && hop
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || ".-_@:[]".contains(c))
        });
        if !self.ssh.jump.is_empty() && !jump_valid {
            return Err("Enter jump hosts as user@host:port, separated by commas.".into());
        }
        if self.ssh.identity.starts_with('-') || self.ssh.identity.chars().any(|c| c.is_control()) {
            return Err("Enter the identity file as a path to a private key.".into());
        }
        self.ssh.forward_args()?;
        let pixels = |v: u16| (200..=8192).contains(&v);
        if let Display::Fixed { width, height } = self.rdp.display
            && !(pixels(width) && pixels(height))
        {
            return Err("A fixed resolution must be between 200 and 8192 pixels.".into());
        }
        if self.group.is_empty() {
            self.group = "Personal".into();
        }
        Ok(())
    }
    /// Parses a quick-connect target: `host`, `host:port`, `user@host`, `[IPv6]:port`, with
    /// an optional `ssh `, `rdp `, `vnc `, `sftp `, `ssh://`, `rdp://`, `vnc://` or `sftp://`
    /// prefix. Without a prefix, ports from 5900 to 5999 mean VNC, `user@host` and port 22
    /// SSH, and anything else RDP, matching how people write each. As in VNC viewers, a VNC
    /// port below 100 is a display number: `vnc://host:1` is port 5901. SFTP targets are SSH
    /// connections; [`browses`] tells them apart.
    pub fn quick(text: &str) -> Option<Self> {
        // Trailing space matters: "ssh " is a prefix still being typed, not a host.
        let mut rest = text.trim_start();
        let mut protocol = None;
        for (prefix, kind) in QUICK_PREFIXES {
            if rest
                .get(..prefix.len())
                .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
            {
                protocol = Some(kind);
                rest = rest[prefix.len()..].trim();
                break;
            }
        }
        let rest = rest.trim_end();
        if rest.is_empty() || rest.contains(char::is_whitespace) {
            return None;
        }
        let rest = rest.trim_end_matches('/');
        let (user, address) = rest.rsplit_once('@').unwrap_or(("", rest));
        let (host, port) = if let Some(inner) = address.strip_prefix('[') {
            let (host, after) = inner.split_once(']')?;
            let port = match after {
                "" => None,
                after => Some(after.strip_prefix(':')?.parse::<u16>().ok()?),
            };
            (host, port)
        } else if address.matches(':').count() == 1 {
            let (host, port) = address.split_once(':')?;
            (host, Some(port.parse::<u16>().ok()?))
        } else {
            // No colon, or a bare IPv6 address.
            (address, None)
        };
        let protocol = protocol.unwrap_or(if port.is_some_and(|p| (5900..6000).contains(&p)) {
            Protocol::Vnc
        } else if !user.is_empty() || port == Some(22) {
            Protocol::Ssh
        } else {
            Protocol::Rdp
        });
        let port = match port {
            Some(display) if protocol == Protocol::Vnc && display < 100 => Some(5900 + display),
            port => port,
        };
        let mut connection = Self::new();
        connection.protocol = protocol;
        connection.port = port.unwrap_or(protocol.port());
        connection.host = host.into();
        connection.username = user.into();
        connection.name = if user.is_empty() {
            host.into()
        } else {
            format!("{user}@{host}")
        };
        connection.validate().ok()?;
        Some(connection)
    }
    /// Whether `other` reaches the same endpoint as the same user.
    pub fn same_target(&self, other: &Self) -> bool {
        self.protocol == other.protocol
            && self.host.eq_ignore_ascii_case(&other.host)
            && self.port == other.port
            && self.username == other.username
    }
    /// OpenSSH options for this connection, without port forwards or the destination.
    pub fn ssh_args(&self) -> Vec<String> {
        let mut args = vec!["-p".to_string(), self.port.to_string()];
        if !self.username.is_empty() {
            args.extend(["-l".into(), self.username.clone()]);
        }
        if !self.ssh.jump.is_empty() {
            args.extend(["-J".into(), self.ssh.jump.clone()]);
        }
        if !self.ssh.identity.is_empty() {
            args.extend(["-i".into(), self.ssh.identity.clone()]);
        }
        // Keepalives notice a dead connection within about 45 seconds.
        for option in ["ServerAliveInterval=15", "ServerAliveCountMax=3"] {
            args.extend(["-o".into(), option.into()]);
        }
        args
    }
    pub fn address(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
    /// Whether any field contains `query`, which is lowercase. The library asks of every
    /// connection each frame, so an empty search answers without lowercasing anything.
    pub fn matches(&self, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }
        [
            &self.name,
            &self.host,
            &self.group,
            &self.username,
            &self.notes,
            self.protocol.label(),
        ]
        .iter()
        .any(|s| s.to_lowercase().contains(query))
    }
}

pub fn data_path() -> PathBuf {
    crate::platform::data_dir().join("connections.json")
}

#[cfg(test)]
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
        c.adopt_description();
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

/// Saves `connections` to `path`, returning the bytes written.
pub fn save(path: &Path, connections: &[Connection]) -> Result<Vec<u8>, String> {
    let bytes = serde_json::to_vec_pretty(connections).map_err(|e| e.to_string())?;
    atomic_write(path, &bytes)?;
    Ok(bytes)
}

/// A hidden name beside `name` for a file written before it takes `name`'s place. It is
/// random, and callers create it exclusively, so it never replaces anything already there.
/// It fits wherever `name` does: a long name is shortened so the whole stays within the 255
/// bytes most file systems allow.
pub fn temp_name(name: &str) -> String {
    const ADDED: usize = ".".len() + ".".len() + 12 + ".relay-part".len();
    let mut bytes = [0u8; 6];
    // Without randomness the name repeats, and creating it exclusively fails instead.
    let _ = getrandom::fill(&mut bytes);
    let suffix: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let mut end = name.len().min(255 - ADDED);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    format!(".{}.{suffix}.relay-part", &name[..end])
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    fs::create_dir_all(path.parent().ok_or("Invalid storage path")?).map_err(|e| e.to_string())?;
    let temp = path.with_file_name(temp_name(
        &path
            .file_name()
            .ok_or("Invalid storage filename")?
            .to_string_lossy(),
    ));
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)
        .map_err(|e| e.to_string())?;
    use std::io::Write;
    let written = file.write_all(bytes).and_then(|_| file.sync_all());
    drop(file);
    // Renaming replaces the destination atomically, on Windows through MoveFileEx.
    if let Err(e) = written.and_then(|_| fs::rename(&temp, path)) {
        let _ = fs::remove_file(&temp);
        return Err(e.to_string());
    }
    // On Unix the new directory entry is durable only once the directory is synced.
    #[cfg(unix)]
    if let Some(parent) = path.parent()
        && let Ok(dir) = fs::File::open(parent)
    {
        let _ = dir.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn merging_keeps_both_sides_and_names_conflicts() {
        let base = Connection::new();
        let mut draft = base.clone();
        draft.notes = "mine".into();
        draft.rdp.drives = true;
        let mut current = base.clone();
        current.host = "theirs.example".into();
        current.rdp.printers = true;
        let (merged, conflicts) = merge(&base, &draft, &current);
        assert!(conflicts.is_empty());
        assert_eq!(
            (merged.notes.as_str(), merged.host.as_str()),
            ("mine", "theirs.example")
        );
        assert!(merged.rdp.drives && merged.rdp.printers);
        // The same change on both sides is no conflict; different ones are.
        current.notes = "mine".into();
        current.rdp.drives = true;
        current.rdp.clipboard = false;
        draft.rdp.microphone = true;
        draft.port = 3390;
        current.port = 3391;
        let (merged, conflicts) = merge(&base, &draft, &current);
        assert_eq!(conflicts, ["port"]);
        assert_eq!(merged.port, 3390);
        assert!(!merged.rdp.clipboard && merged.rdp.microphone);
        draft.rdp.gateway = "gw1.example".into();
        current.rdp.gateway = "gw2.example".into();
        let (_, conflicts) = merge(&base, &draft, &current);
        assert_eq!(conflicts, ["port", "RD Gateway"]);
        draft.vnc.view_only = true;
        let (merged, _) = merge(&base, &draft, &current);
        assert!(merged.vnc.view_only && merged.vnc.clipboard);
    }
    #[test]
    fn vnc_options_are_saved_only_when_changed() {
        let mut c = Connection::new();
        c.protocol = Protocol::Vnc;
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains("\"Vnc\"") && !json.contains("view_only"));
        c.vnc.view_only = true;
        let json = serde_json::to_string(&c).unwrap();
        let read: Connection = serde_json::from_str(&json).unwrap();
        assert_eq!(read.vnc, c.vnc);
    }
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
        assert!(!c.matches("rack"));
        c.notes = "Rack 4, ask Sam before rebooting".into();
        assert!(c.matches("rack"));
        let json = serde_json::to_string(&c).unwrap();
        assert!(!json.contains("password"));
        assert_eq!(
            serde_json::from_str::<Connection>(&json).unwrap().host,
            "::1"
        );
    }
    #[test]
    fn quick_targets_parse_like_people_write_them() {
        let target = |text| {
            Connection::quick(text).map(|c| (c.protocol, c.username, c.host, c.port, c.name))
        };
        let expect = |protocol, user: &str, host: &str, port, name: &str| {
            Some((protocol, user.into(), host.into(), port, name.into()))
        };
        assert_eq!(
            target("build"),
            expect(Protocol::Rdp, "", "build", 3389, "build")
        );
        assert_eq!(
            target("10.0.0.5:3390"),
            expect(Protocol::Rdp, "", "10.0.0.5", 3390, "10.0.0.5")
        );
        assert_eq!(
            target("deploy@build.example.com"),
            expect(
                Protocol::Ssh,
                "deploy",
                "build.example.com",
                22,
                "deploy@build.example.com"
            )
        );
        assert_eq!(target("gw:22"), expect(Protocol::Ssh, "", "gw", 22, "gw"));
        assert_eq!(
            target("  SSH root@[2001:db8::1]:2222 "),
            expect(
                Protocol::Ssh,
                "root",
                "2001:db8::1",
                2222,
                "root@2001:db8::1"
            )
        );
        assert_eq!(
            target("rdp://desk/"),
            expect(Protocol::Rdp, "", "desk", 3389, "desk")
        );
        assert_eq!(
            target("sftp://deploy@build:2222"),
            expect(Protocol::Ssh, "deploy", "build", 2222, "deploy@build")
        );
        assert_eq!(
            target("SFTP build"),
            expect(Protocol::Ssh, "", "build", 22, "build")
        );
        assert!(browses(" sftp://build") && browses("Sftp build"));
        assert!(!browses("ssh build") && !browses("sftpserver") && !browses("sftp"));
        assert_eq!(
            target("desk:5901"),
            expect(Protocol::Vnc, "", "desk", 5901, "desk")
        );
        assert_eq!(
            target("alex@mac.local:5900"),
            expect(Protocol::Vnc, "alex", "mac.local", 5900, "alex@mac.local")
        );
        assert_eq!(
            target("vnc://pi:2"),
            expect(Protocol::Vnc, "", "pi", 5902, "pi")
        );
        assert_eq!(
            target("VNC pi:6001"),
            expect(Protocol::Vnc, "", "pi", 6001, "pi")
        );
        assert_eq!(
            target("vnc pi"),
            expect(Protocol::Vnc, "", "pi", 5900, "pi")
        );
        assert_eq!(
            target("rdp alex@desk"),
            expect(Protocol::Rdp, "alex", "desk", 3389, "alex@desk")
        );
        assert_eq!(
            target("2001:db8::1"),
            expect(Protocol::Rdp, "", "2001:db8::1", 3389, "2001:db8::1")
        );
        for invalid in [
            "",
            "ssh ",
            "two words",
            "host:notaport",
            "-oProxyCommand=x",
            "host:0",
        ] {
            assert_eq!(target(invalid), None, "{invalid:?}");
        }
    }
    #[test]
    fn ssh_options_validate_and_become_flags() {
        let mut c = Connection::new();
        c.name = "Build".into();
        c.host = "build.example".into();
        c.protocol = Protocol::Ssh;
        c.ssh = SshOptions {
            jump: " deploy@bastion.example:2222, hop2 ".into(),
            identity: "\"C:\\Keys\\id_ed25519\"".into(),
            forwards: "L8080:localhost:80, r9000:127.0.0.1:9000 ,D1080, L127.0.0.1:5432:db:5432, \
                       L8081:[::1]:80, L[::1]:8082:[fe80::1]:80, D[::]:1081"
                .into(),
        };
        assert!(c.validate().is_ok());
        assert_eq!(c.ssh.identity, "C:\\Keys\\id_ed25519");
        assert_eq!(
            c.ssh.forward_args().unwrap(),
            [
                ("-L", "8080:localhost:80".into()),
                ("-R", "9000:127.0.0.1:9000".into()),
                ("-D", "1080".into()),
                ("-L", "127.0.0.1:5432:db:5432".into()),
                ("-L", "8081:[::1]:80".into()),
                ("-L", "[::1]:8082:[fe80::1]:80".into()),
                ("-D", "[::]:1081".into()),
            ]
        );
        for forwards in [
            "X8080:a:80",
            "L8080",
            "L-oProxyCommand=x:a:1",
            "D1:2:3",
            "L1:a b:2:3",
            "L8080:::1:80",
            "L8080:[::1:80",
            "L8080:[[::1]]:80",
        ] {
            c.ssh.forwards = forwards.into();
            assert!(c.validate().is_err(), "{forwards}");
        }
        c.ssh.forwards.clear();
        for jump in ["-oProxyCommand=calc", "a b", "host,"] {
            c.ssh.jump = jump.into();
            assert!(c.validate().is_err(), "{jump}");
        }
        c.ssh.jump.clear();
        c.ssh.identity = "-oProxyCommand=calc".into();
        assert!(c.validate().is_err());
    }
    #[test]
    fn rdp_options_default_quietly_and_validate() {
        let mut c = Connection::new();
        c.name = "Desk".into();
        c.host = "desk.example".into();
        let json = serde_json::to_string(&c).unwrap();
        assert!(!json.contains("rdp"), "default options are omitted: {json}");
        let old: Connection = serde_json::from_str(
            r#"{"id":1,"name":"Old","host":"old.example","username":"","group":"G","protocol":"Rdp","port":3389,"favorite":false}"#,
        )
        .unwrap();
        assert_eq!(old.rdp, RdpOptions::default());
        assert!(old.rdp.clipboard);
        c.rdp.gateway = " gw.example.com ".into();
        c.rdp.drives = true;
        c.rdp.display = Display::Fixed {
            width: 1920,
            height: 1080,
        };
        assert!(c.validate().is_ok());
        assert_eq!(c.rdp.gateway, "gw.example.com");
        let roundtrip: Connection =
            serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(roundtrip.rdp, c.rdp);
        c.rdp.gateway = "https://gw".into();
        assert!(c.validate().is_err());
        c.rdp.gateway.clear();
        c.rdp.display = Display::Fixed {
            width: 100,
            height: 1080,
        };
        assert!(c.validate().is_err());
    }
    #[test]
    fn saves_replaces_and_preserves_corrupt_data() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("relay-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("connections.json");
        assert!(load(&path).unwrap().is_empty());
        // Files beside it are someone else's.
        let other = dir.join("connections.json.tmp");
        fs::write(&other, "unrelated").unwrap();
        let mut c = Connection::new();
        c.name = "Test".into();
        c.id = 1;
        c.host = "localhost".into();
        save(&path, &[c.clone()]).unwrap();
        c.name = "Updated".into();
        save(&path, &[c]).unwrap();
        assert_eq!(load(&path).unwrap()[0].name, "Updated");
        assert_eq!(fs::read_to_string(&other).unwrap(), "unrelated");
        fs::remove_file(other).unwrap();
        fs::write(&path, "broken").unwrap();
        assert!(load(&path).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "broken");
        fs::remove_file(path).unwrap();
        fs::remove_dir(dir).unwrap();
    }
}
