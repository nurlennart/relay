//! Imports OpenSSH client configuration and Remote Desktop (.rdp) files.
use crate::{
    csv_io::{Collector, Import},
    model::{Audio, Connection, Display, Keys, Protocol},
};
use std::path::{Path, PathBuf};

/// The user's OpenSSH configuration, `~/.ssh/config`.
pub fn ssh_config_path() -> Option<PathBuf> {
    crate::platform::home_dir().map(|home| home.join(".ssh").join("config"))
}

pub fn read_ssh_config(path: &Path, existing: &[Connection]) -> Result<Import, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    let home = crate::platform::home_dir().unwrap_or_default();
    ssh_config(&text, &home.to_string_lossy(), existing)
}

struct Block {
    patterns: Vec<String>,
    options: Vec<(String, String)>,
}

/// Turns every concrete `Host` alias into an SSH profile. As in OpenSSH, each alias collects
/// options from all matching blocks, including wildcard ones, and the first value wins.
fn ssh_config(text: &str, home: &str, existing: &[Connection]) -> Result<Import, String> {
    let mut collector = Collector::new(existing, "SSH config".into());
    // Options before the first Host block apply to every host.
    let mut blocks = vec![Block {
        patterns: vec!["*".into()],
        options: vec![],
    }];
    let mut in_match = false;
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = match line.split_once(|c: char| c.is_whitespace() || c == '=') {
            Some((key, value)) => (key, value.trim_start_matches([' ', '\t', '=']).trim()),
            None => (line, ""),
        };
        let value = value.trim_matches('"');
        match key.to_ascii_lowercase().as_str() {
            "host" => {
                in_match = false;
                blocks.push(Block {
                    patterns: value.split_whitespace().map(str::to_string).collect(),
                    options: vec![],
                });
            }
            "match" => {
                in_match = true;
                collector.skip(format!(
                    "Line {}: Match blocks are not imported",
                    number + 1
                ));
            }
            "include" => collector.skip(format!(
                "Line {}: Include is not followed ({value})",
                number + 1
            )),
            key if !in_match => blocks
                .last_mut()
                .unwrap()
                .options
                .push((key.to_string(), value.to_string())),
            _ => {}
        }
    }
    let aliases: Vec<String> = blocks
        .iter()
        .flat_map(|b| &b.patterns)
        .filter(|p| !p.contains(['*', '?', '!']))
        .fold(vec![], |mut aliases, alias| {
            if !aliases.contains(alias) {
                aliases.push(alias.clone());
            }
            aliases
        });
    for alias in aliases {
        let matching: Vec<&(String, String)> = blocks
            .iter()
            .filter(|b| host_matches(&b.patterns, &alias))
            .flat_map(|b| &b.options)
            .collect();
        let first = |key: &str| {
            matching
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
        };
        let expand = |value: &str| {
            let value = value.replace("%h", &alias).replace("%d", home);
            match value.strip_prefix('~') {
                Some(rest) => format!("{home}{rest}"),
                None => value,
            }
        };
        let mut c = Connection::new();
        c.protocol = Protocol::Ssh;
        c.name = alias.clone();
        c.group = "SSH config".into();
        c.host = expand(first("hostname").unwrap_or(&alias));
        c.username = first("user").unwrap_or_default().to_string();
        c.port = match first("port") {
            Some(port) => match port.parse() {
                Ok(port) => port,
                Err(_) => {
                    collector.skip(format!("{alias}: invalid port {port:?}"));
                    continue;
                }
            },
            None => 22,
        };
        if let Some(jump) = first("proxyjump").filter(|j| !j.eq_ignore_ascii_case("none")) {
            c.ssh.jump = jump.to_string();
        }
        if let Some(identity) = first("identityfile") {
            c.ssh.identity = expand(identity);
        }
        // Forwards accumulate instead of taking the first value.
        let forwards: Vec<String> = matching
            .iter()
            .filter_map(|(key, value)| {
                let flag = match key.as_str() {
                    "localforward" => "L",
                    "remoteforward" => "R",
                    "dynamicforward" => "D",
                    _ => return None,
                };
                // "8080 localhost:80" becomes "L8080:localhost:80".
                Some(format!(
                    "{flag}{}",
                    value.split_whitespace().collect::<Vec<_>>().join(":")
                ))
            })
            .collect();
        c.ssh.forwards = forwards.join(", ");
        collector.add(&alias, c)?;
    }
    Ok(collector.finish())
}

/// OpenSSH host matching: `*` and `?` wildcards, and `!pattern` excludes.
fn host_matches(patterns: &[String], host: &str) -> bool {
    let mut matched = false;
    for pattern in patterns {
        match pattern.strip_prefix('!') {
            Some(negated) if glob(negated, host) => return false,
            Some(_) => {}
            None => matched |= glob(pattern, host),
        }
    }
    matched
}

fn glob(pattern: &str, text: &str) -> bool {
    let (pattern, text): (Vec<char>, Vec<char>) = (
        pattern.to_ascii_lowercase().chars().collect(),
        text.to_ascii_lowercase().chars().collect(),
    );
    // Iterative wildcard matching with backtracking to the last `*`.
    let (mut p, mut t, mut star, mut mark) = (0, 0, None, 0);
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            mark = t;
            p += 1;
        } else if let Some(s) = star {
            p = s + 1;
            mark += 1;
            t = mark;
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}

/// Imports Remote Desktop Connection files, each given as its file name and bytes.
pub fn rdp_files(files: &[(String, Vec<u8>)], existing: &[Connection]) -> Result<Import, String> {
    let label = match files {
        [(name, _)] => name.clone(),
        files => format!("{} .rdp files", files.len()),
    };
    let mut collector = Collector::new(existing, label);
    for (name, bytes) in files {
        let text = decode(bytes);
        let mut c = Connection::new();
        c.name = Path::new(name)
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| name.clone());
        c.group = "RDP files".into();
        let mut size = (None, None);
        let mut dynamic = true;
        let mut gateway_used = true;
        for line in text.lines() {
            let mut parts = line.trim().splitn(3, ':');
            let (Some(key), Some(_kind), Some(value)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let flag = value.trim() == "1";
            let number = value.trim().parse::<i64>().ok();
            match key.to_ascii_lowercase().as_str() {
                "full address" => {
                    // Reuse quick-connect parsing for host, port, and bracketed IPv6.
                    if let Some(target) = Connection::quick(&format!("rdp {}", value.trim())) {
                        c.host = target.host;
                        c.port = target.port;
                    } else {
                        c.host = value.trim().into();
                    }
                }
                "server port" => {
                    if let Some(port) = number.and_then(|n| u16::try_from(n).ok()) {
                        c.port = port;
                    }
                }
                "username" => c.username = value.trim().into(),
                "gatewayhostname" => c.rdp.gateway = value.trim().into(),
                // 0 and 4 mean the gateway is not used.
                "gatewayusagemethod" => gateway_used = !matches!(number, Some(0 | 4)),
                "redirectclipboard" => c.rdp.clipboard = flag,
                "redirectprinters" => c.rdp.printers = flag,
                "redirectsmartcards" => c.rdp.smart_cards = flag,
                "redirectdrives" => c.rdp.drives |= flag,
                "drivestoredirect" => c.rdp.drives |= !value.trim().is_empty(),
                "audiocapturemode" => c.rdp.microphone = flag,
                "audiomode" => {
                    c.rdp.audio = match number {
                        Some(1) => Audio::Remote,
                        Some(2) => Audio::Off,
                        _ => Audio::Local,
                    }
                }
                "keyboardhook" => {
                    c.rdp.keys = match number {
                        Some(0) => Keys::Local,
                        Some(1) => Keys::Remote,
                        _ => Keys::FullScreen,
                    }
                }
                "dynamic resolution" => dynamic = flag,
                "desktopwidth" => size.0 = number.and_then(|n| u16::try_from(n).ok()),
                "desktopheight" => size.1 = number.and_then(|n| u16::try_from(n).ok()),
                _ => {}
            }
        }
        if !gateway_used {
            c.rdp.gateway.clear();
        }
        if let (false, (Some(width), Some(height))) = (dynamic, size) {
            c.rdp.display = Display::Fixed { width, height };
        }
        collector.add(name, c)?;
    }
    Ok(collector.finish())
}

/// .rdp files are usually UTF-16LE with a byte order mark; some tools write UTF-8.
fn decode(bytes: &[u8]) -> String {
    let utf16 =
        bytes.starts_with(&[0xff, 0xfe]) || (bytes.len() > 2 && bytes[1] == 0 && bytes[0] != 0);
    if utf16 {
        let body = bytes.strip_prefix(&[0xff, 0xfe]).unwrap_or(bytes);
        encoding_rs::UTF_16LE.decode(body).0.into_owned()
    } else {
        match std::str::from_utf8(bytes) {
            Ok(text) => text.trim_start_matches('\u{feff}').to_owned(),
            Err(_) => encoding_rs::WINDOWS_1252.decode(bytes).0.into_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"
# Personal machines
ServerAliveInterval 30

Host build
    HostName build.example.com
    User deploy
    IdentityFile ~/.ssh/id_ed25519
    LocalForward 8080 localhost:80

Host db-* !db-legacy
    ProxyJump ops@bastion.example.com:2222
    DynamicForward 1080

Host db-primary db-legacy
    HostName %h.internal
    Port 2200

Match host *.corp
    User someone

Host *
    User fallback
    IdentityFile ~/.ssh/id_rsa

Include conf.d/*
Host bad
    Port nope
"#;

    #[test]
    fn ssh_config_follows_openssh_rules() {
        let import = ssh_config(CONFIG, r"C:\Users\alex", &[]).unwrap();
        let find = |name: &str| import.connections.iter().find(|c| c.name == name).unwrap();
        let build = find("build");
        assert_eq!((build.host.as_str(), build.port), ("build.example.com", 22));
        assert_eq!(build.username, "deploy", "the first value wins over Host *");
        assert_eq!(build.ssh.identity, r"C:\Users\alex/.ssh/id_ed25519");
        assert_eq!(build.ssh.forwards, "L8080:localhost:80");
        let primary = find("db-primary");
        assert_eq!(
            (primary.host.as_str(), primary.port),
            ("db-primary.internal", 2200)
        );
        assert_eq!(primary.ssh.jump, "ops@bastion.example.com:2222");
        assert_eq!(primary.ssh.forwards, "D1080");
        assert_eq!(primary.username, "fallback");
        let legacy = find("db-legacy");
        assert!(legacy.ssh.jump.is_empty(), "!db-legacy excludes it");
        assert_eq!(import.connections.len(), 3);
        assert!(
            import
                .connections
                .iter()
                .all(|c| c.protocol == Protocol::Ssh)
        );
        let reasons = import.skipped.join("\n");
        assert!(reasons.contains("Match"), "{reasons}");
        assert!(reasons.contains("Include"), "{reasons}");
        assert!(reasons.contains("bad: invalid port"), "{reasons}");
        let again = ssh_config(CONFIG, r"C:\Users\alex", &import.connections).unwrap();
        assert_eq!((again.connections.len(), again.duplicates), (0, 3));
    }

    #[test]
    fn globs_match_like_openssh() {
        assert!(glob("db-*", "DB-primary"));
        assert!(glob("*.example.com", "a.b.example.com"));
        assert!(glob("h?st", "host"));
        assert!(!glob("db-*", "web-1"));
        assert!(!host_matches(&["*".into(), "!secret".into()], "secret"));
    }

    #[test]
    fn rdp_files_map_their_settings() {
        let file = "screen mode id:i:1\r\nfull address:s:desk.example.com:3390\r\n\
            username:s:CORP\\alex\r\ngatewayhostname:s:gw.example.com\r\n\
            gatewayusagemethod:i:1\r\nredirectclipboard:i:0\r\ndrivestoredirect:s:*\r\n\
            audiomode:i:2\r\naudiocapturemode:i:1\r\nkeyboardhook:i:1\r\n\
            dynamic resolution:i:0\r\ndesktopwidth:i:1600\r\ndesktopheight:i:900\r\n";
        let mut utf16 = vec![0xff, 0xfe];
        for unit in file.encode_utf16() {
            utf16.extend(unit.to_le_bytes());
        }
        let plain = "full address:s:[2001:db8::7]\nusername:s:bob\ngatewayhostname:s:unused\n\
            gatewayusagemethod:i:0\n";
        let files = [
            ("Office desk.rdp".to_string(), utf16),
            ("lab.rdp".to_string(), plain.as_bytes().to_vec()),
            ("broken.rdp".to_string(), b"username:s:nobody\n".to_vec()),
        ];
        let import = rdp_files(&files, &[]).unwrap();
        let desk = &import.connections[0];
        assert_eq!(desk.name, "Office desk");
        assert_eq!((desk.host.as_str(), desk.port), ("desk.example.com", 3390));
        assert_eq!(desk.username, "CORP\\alex");
        assert_eq!(desk.rdp.gateway, "gw.example.com");
        assert!(!desk.rdp.clipboard && desk.rdp.drives && desk.rdp.microphone);
        assert_eq!((desk.rdp.audio, desk.rdp.keys), (Audio::Off, Keys::Remote));
        assert_eq!(
            desk.rdp.display,
            Display::Fixed {
                width: 1600,
                height: 900
            }
        );
        let lab = &import.connections[1];
        assert_eq!((lab.host.as_str(), lab.port), ("2001:db8::7", 3389));
        assert!(
            lab.rdp.gateway.is_empty(),
            "usage method 0 means no gateway"
        );
        assert_eq!(lab.rdp.display, Display::Fit);
        assert_eq!(
            import.skipped.len(),
            1,
            "a file without an address is skipped"
        );
    }
}
