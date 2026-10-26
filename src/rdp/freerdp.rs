//! FreeRDP, which shows remote desktops in its own window on macOS and Linux. Relay runs it in
//! a terminal tab, so its credential and certificate prompts work like an SSH login.
use crate::model::{Audio, Connection, Display, Keys};
use portable_pty::CommandBuilder;
use std::path::Path;

/// FreeRDP clients, best first. Packages of version 3 often add a `3` suffix.
#[cfg(target_os = "macos")]
const CLIENTS: &[&str] = &["sdl-freerdp3", "sdl-freerdp", "xfreerdp3", "xfreerdp"];
#[cfg(not(target_os = "macos"))]
const CLIENTS: &[&str] = &[
    "xfreerdp3",
    "sdl-freerdp3",
    "wlfreerdp3",
    "xfreerdp",
    "sdl-freerdp",
    "wlfreerdp",
];

#[cfg(target_os = "macos")]
const MISSING: &str = "FreeRDP is not installed. Install it with `brew install freerdp`, then \
    reconnect.";
#[cfg(not(target_os = "macos"))]
const MISSING: &str = "FreeRDP is not installed. Install it with your package manager, such as \
    `sudo apt install freerdp3-x11` or `sudo dnf install freerdp`, then reconnect.";

/// The FreeRDP command that opens `connection`.
pub fn command(connection: &Connection) -> Result<CommandBuilder, String> {
    let program = CLIENTS
        .iter()
        .find_map(|name| crate::platform::find_program(name))
        .ok_or(MISSING)?;
    let mut command = CommandBuilder::new(&program);
    command.args(arguments(connection, major_version(&program)));
    Ok(command)
}

/// FreeRDP's major version, assuming 3 when it cannot be read.
fn major_version(program: &Path) -> u32 {
    std::process::Command::new(program)
        .arg("--version")
        .output()
        .ok()
        .and_then(|output| parse_version(&String::from_utf8_lossy(&output.stdout)))
        .unwrap_or(3)
}

/// Reads the major version from output such as `This is FreeRDP version 3.10.3 (...)`.
fn parse_version(text: &str) -> Option<u32> {
    text.split("version ")
        .nth(1)?
        .split('.')
        .next()?
        .parse()
        .ok()
}

/// FreeRDP arguments for `connection`. Hosts, gateways, and usernames were validated, so none
/// can be mistaken for an option.
fn arguments(connection: &Connection, version: u32) -> Vec<String> {
    let options = &connection.rdp;
    let mut args = vec![
        format!("/v:{}", connection.address()),
        format!("/title:{}", connection.name),
    ];
    if !connection.username.is_empty() {
        args.push(format!("/u:{}", connection.username));
    }
    if !options.gateway.is_empty() {
        args.push(if version >= 3 {
            format!("/gateway:g:{}", options.gateway)
        } else {
            format!("/g:{}", options.gateway)
        });
    }
    match options.display {
        Display::Fit => args.push("/dynamic-resolution".into()),
        Display::Fixed { width, height } => {
            args.push(format!("/size:{width}x{height}"));
            args.push("/smart-sizing".into());
        }
    }
    let flag = |on: bool, name: &str| format!("{}{name}", if on { '+' } else { '-' });
    args.push(flag(options.clipboard, "clipboard"));
    for (on, arg) in [
        (options.drives, "+home-drive"),
        (options.printers, "/printer"),
        (options.smart_cards, "/smartcard"),
        (options.microphone, "/microphone"),
    ] {
        if on {
            args.push(arg.into());
        }
    }
    args.push(
        match options.audio {
            Audio::Local => "/sound",
            Audio::Remote => "/audio-mode:1",
            Audio::Off => "/audio-mode:2",
        }
        .into(),
    );
    if options.keys == Keys::Local {
        args.push("-grab-keyboard".into());
    }
    args.extend([
        "+auto-reconnect".into(),
        "/auto-reconnect-max-retries:20".into(),
    ]);
    args
}

/// Never constructed: without the Windows control, remote desktops run in a FreeRDP window.
pub enum RdpClient {}

#[allow(clippy::unused_self)]
impl RdpClient {
    pub fn new(
        _parent: Option<crate::platform::Window>,
        _connection: &Connection,
        _ctx: eframe::egui::Context,
    ) -> Result<Self, String> {
        Err("Embedded remote desktops need Windows.".into())
    }
    pub fn place(&mut self, _rect: Option<[i32; 4]>, _holes: &[[i32; 4]]) -> Result<(), String> {
        match *self {}
    }
    pub fn set_parent(&mut self, _parent: crate::platform::Window) {
        match *self {}
    }
    pub fn ended(&self) -> bool {
        match *self {}
    }
    pub fn error(&self) -> Option<String> {
        match *self {}
    }
    pub fn reconnecting(&self) -> bool {
        match *self {}
    }
    pub fn full_screen(&self) -> bool {
        match *self {}
    }
    pub fn enter_full_screen(&self) -> Result<(), String> {
        match *self {}
    }
    pub fn display_warning(&self) -> Option<&str> {
        match *self {}
    }
}

/// FreeRDP does not save passwords, so there is nothing to forget.
pub fn forget_credentials(_host: &str) -> Result<bool, String> {
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Protocol, RdpOptions};

    fn connection() -> Connection {
        let mut c = Connection::new();
        c.name = "Office desk".into();
        c.host = "desk.example".into();
        c.protocol = Protocol::Rdp;
        c
    }

    #[test]
    fn default_options_become_arguments() {
        let mut c = connection();
        c.username = r"CORP\alex".into();
        assert_eq!(
            arguments(&c, 3),
            [
                "/v:desk.example:3389",
                "/title:Office desk",
                r"/u:CORP\alex",
                "/dynamic-resolution",
                "+clipboard",
                "/sound",
                "+auto-reconnect",
                "/auto-reconnect-max-retries:20",
            ]
        );
    }

    #[test]
    fn every_option_reaches_freerdp() {
        let mut c = connection();
        c.host = "2001:db8::1".into();
        c.port = 3390;
        c.rdp = RdpOptions {
            gateway: "gw.example.com".into(),
            clipboard: false,
            drives: true,
            printers: true,
            smart_cards: true,
            microphone: true,
            audio: Audio::Off,
            keys: Keys::Local,
            display: Display::Fixed {
                width: 1600,
                height: 900,
            },
        };
        let args = arguments(&c, 3);
        for expected in [
            "/v:[2001:db8::1]:3390",
            "/gateway:g:gw.example.com",
            "/size:1600x900",
            "/smart-sizing",
            "-clipboard",
            "+home-drive",
            "/printer",
            "/smartcard",
            "/microphone",
            "/audio-mode:2",
            "-grab-keyboard",
        ] {
            assert!(args.iter().any(|a| a == expected), "{expected} in {args:?}");
        }
        assert!(arguments(&c, 2).iter().any(|a| a == "/g:gw.example.com"));
    }

    #[test]
    fn versions_parse_from_banner() {
        assert_eq!(
            parse_version("This is FreeRDP version 3.10.3 (3.10.3)\n"),
            Some(3)
        );
        assert_eq!(
            parse_version("This is FreeRDP version 2.11.5 (2.11.5)"),
            Some(2)
        );
        assert_eq!(parse_version("garbage"), None);
    }
}
