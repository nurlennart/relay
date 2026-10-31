//! Opt-in rendering fixture. Never writes the user's connection library.
use super::*;
use std::sync::atomic::{AtomicU32, Ordering};

pub fn seed(app: &mut App) {
    if std::env::var_os("RELAY_UI_CHECK").is_none() {
        return;
    }
    app.state = state::State::default();
    app.state_path = None;
    app.ctx.set_zoom_factor(1.);
    if let Ok(name) = std::env::var("RELAY_UI_THEME")
        && let Some(theme) = theme::Theme::ALL
            .into_iter()
            .find(|t| t.name().eq_ignore_ascii_case(&name))
    {
        theme.apply(&app.ctx);
        if let Some(window) = app.parent {
            window.set_theme(theme);
        }
    }
    if let Ok(size) = std::env::var("RELAY_UI_SIZE")
        && let Some((w, h)) = size.split_once('x')
        && let (Ok(w), Ok(h)) = (w.parse(), h.parse())
    {
        app.ctx
            .send_viewport_cmd(egui::ViewportCommand::InnerSize(Vec2::new(w, h)));
    }
    // Enlarges everything, to inspect icons and small details.
    if let Some(zoom) = std::env::var("RELAY_UI_ZOOM")
        .ok()
        .and_then(|z| z.parse::<f32>().ok())
    {
        app.ctx.set_zoom_factor(zoom);
    }
    // The open library, for Settings: sqlite, mssql, mssql-forgotten, or mysql.
    let server = |connection: &str| database::Server {
        connection: connection.into(),
        tls: true,
    };
    match std::env::var("RELAY_UI_LOCATION").as_deref() {
        Ok("sqlite") => {
            app.location =
                database::Location::Sqlite(PathBuf::from("\\\\fileserver\\team\\relay.sqlite"))
        }
        Ok(kind @ ("mssql" | "mssql-forgotten")) => {
            app.location = database::Location::Mssql(server(
                "Server=tcp:sql01\\SQLEXPRESS;Database=Relay;Integrated Security=true",
            ));
            app.server_remembered = kind == "mssql";
        }
        Ok("mysql") => {
            app.location =
                database::Location::Mysql(server("mysql://relay:secret@db.example.com:3306/relay"));
            app.server_remembered = true;
        }
        _ => {}
    }
    if let Ok(tab) = std::env::var("RELAY_UI_SETTINGS") {
        app.modal = Modal::Settings;
        app.settings_tab = match tab.as_str() {
            "terminal" => settings::SettingsTab::Terminal,
            "library" => settings::SettingsTab::Library,
            "snippets" => settings::SettingsTab::Snippets,
            _ => settings::SettingsTab::Appearance,
        };
    }
    if std::env::var_os("RELAY_UI_SHORTCUTS").is_some() {
        app.modal = Modal::Shortcuts;
    }
    if let Ok(query) = std::env::var("RELAY_UI_QUICK") {
        app.quick_open = Some(Palette {
            query,
            ..Default::default()
        });
    }
    match std::env::var("RELAY_UI_FILTER").as_deref() {
        Ok("favorites") => app.filter = Filter::Favorites,
        Ok("recent") => app.filter = Filter::Recent,
        _ => {}
    }
    if let Ok(query) = std::env::var("RELAY_UI_QUERY") {
        app.query = query;
    }
    app.selected = std::env::var("RELAY_UI_SELECT")
        .ok()
        .and_then(|id| id.parse().ok());
    app.storage_ok = false;
    app.connections = [
        (
            "Design workstation",
            "192.0.2.24",
            "studio\\alex",
            "Studio",
            Protocol::Rdp,
        ),
        (
            "Build server",
            "build.example.com",
            "deploy",
            "Infrastructure",
            Protocol::Ssh,
        ),
        (
            "Office desktop",
            "192.0.2.42",
            "alex",
            "Studio",
            Protocol::Rdp,
        ),
        (
            "Staging gateway",
            "staging.example.com",
            "ops",
            "Infrastructure",
            Protocol::Ssh,
        ),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, (name, host, username, group, protocol))| Connection {
        id: (i + 1) as u64,
        name: name.into(),
        host: host.into(),
        username: username.into(),
        group: group.into(),
        protocol,
        port: protocol.port(),
        favorite: i < 2,
        notes: Default::default(),
        metadata: Default::default(),
        rdp: Default::default(),
        ssh: Default::default(),
        vnc: Default::default(),
    })
    .collect();
    // A fuller library for the README screenshots.
    if std::env::var_os("RELAY_UI_SHOWCASE").is_some() {
        let notes = [
            "Color grading, 2× RTX",
            "Nightly builds and release archives",
            "",
            "Smoke tests before each release",
        ];
        for (c, note) in app.connections.iter_mut().zip(notes) {
            c.notes = note.into();
        }
        let more = [
            (
                "Web frontend 1",
                "web1.example.com",
                "deploy",
                "Production",
                Protocol::Ssh,
                "nginx, behind the load balancer",
                true,
            ),
            (
                "Web frontend 2",
                "web2.example.com",
                "deploy",
                "Production",
                Protocol::Ssh,
                "",
                false,
            ),
            (
                "Database primary",
                "db1.example.com",
                "ops",
                "Production",
                Protocol::Ssh,
                "PostgreSQL 17, maintenance Sundays 02:00",
                true,
            ),
            (
                "Monitoring",
                "grafana.example.com",
                "ops",
                "Infrastructure",
                Protocol::Ssh,
                "",
                false,
            ),
            (
                "Domain controller",
                "dc01.corp.example.com",
                "CORP\\admin",
                "Office",
                Protocol::Rdp,
                "",
                false,
            ),
            (
                "File server",
                "files.corp.example.com",
                "CORP\\admin",
                "Office",
                Protocol::Rdp,
                "Shares and backups",
                false,
            ),
            (
                "Reception PC",
                "192.0.2.51",
                "frontdesk",
                "Office",
                Protocol::Rdp,
                "",
                false,
            ),
            (
                "Render node",
                "192.0.2.60",
                "studio\\alex",
                "Studio",
                Protocol::Rdp,
                "",
                false,
            ),
            (
                "Home NAS",
                "nas.home.arpa",
                "admin",
                "Home lab",
                Protocol::Ssh,
                "",
                false,
            ),
            (
                "Raspberry Pi",
                "pi.home.arpa",
                "pi",
                "Home lab",
                Protocol::Ssh,
                "Pi-hole and Home Assistant",
                false,
            ),
        ];
        for (name, host, username, group, protocol, notes, favorite) in more {
            app.connections.push(Connection {
                id: app.connections.len() as u64 + 1,
                name: name.into(),
                host: host.into(),
                username: username.into(),
                group: group.into(),
                protocol,
                port: protocol.port(),
                favorite,
                notes: notes.into(),
                metadata: Default::default(),
                rdp: Default::default(),
                ssh: Default::default(),
                vnc: Default::default(),
            });
        }
    }
    if std::env::var_os("RELAY_UI_NO_FAVORITES").is_some() {
        for c in &mut app.connections {
            c.favorite = false;
        }
    }
    if std::env::var_os("RELAY_UI_EMPTY").is_some() {
        app.connections.clear();
    }
    if std::env::var_os("RELAY_UI_EDITOR").is_some() {
        app.new_connection();
    }
    if std::env::var_os("RELAY_UI_SNIPPETS").is_some() {
        let snippet = |name: &str, command: &str, run| state::Snippet {
            name: name.into(),
            command: command.into(),
            run,
        };
        app.state.snippets = vec![
            snippet("Tail syslog", "tail -f /var/log/syslog", true),
            snippet("Disk usage", "df -h", true),
            snippet("Restart nginx", "sudo systemctl restart nginx", false),
            snippet(
                "Deploy",
                "cd /srv/relay
git pull && ./deploy.sh",
                false,
            ),
        ];
    }
    // Terminal look: a scheme by its position in Settings, and a font size.
    if let Some(scheme) = std::env::var("RELAY_UI_SCHEME")
        .ok()
        .and_then(|i| i.parse::<usize>().ok())
        .and_then(|i| crate::terminal::Scheme::ALL.get(i).copied())
    {
        app.state.terminal.scheme = scheme;
    }
    if let Some(size) = std::env::var("RELAY_UI_FONT_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
    {
        app.state.terminal.font_size = size;
    }
    // Connections chosen together, by ID.
    if let Ok(ids) = std::env::var("RELAY_UI_MARKED") {
        app.marked = ids.split(',').filter_map(|id| id.parse().ok()).collect();
    }
    if std::env::var_os("RELAY_UI_LOCAL").is_some() {
        app.open_local_shell();
    }
    if std::env::var_os("RELAY_UI_TERMINAL").is_some() {
        let mut connection = app.connections[1].clone();
        connection.host = "build.example.com".into();
        let mut terminal = if std::env::var_os("RELAY_UI_STREAM").is_some() {
            crate::terminal::Terminal::demo_stream(app.ctx.clone())
        } else {
            crate::terminal::Terminal::demo(transcript())
        };
        if std::env::var_os("RELAY_UI_SELECTION").is_some() {
            terminal.select((13, 0), (15, 40));
        }
        if let Ok(query) = std::env::var("RELAY_UI_SEARCH") {
            terminal.search_for(&query);
        }
        if std::env::var_os("RELAY_UI_ENDED").is_some() {
            terminal.ended = true;
            terminal.exit_code = Some(0);
        }
        app.sessions.push(session::Session::demo(
            connection,
            terminal,
            app.ctx.clone(),
        ));
        app.sync_tabs();
        app.activate_session(0);
        if std::env::var_os("RELAY_UI_SPLIT").is_some() {
            let mut connection = app.connections[3].clone();
            connection.host = "staging.example.com".into();
            let terminal = crate::terminal::Terminal::demo(transcript());
            app.sessions.push(session::Session::demo(
                connection,
                terminal,
                app.ctx.clone(),
            ));
            app.sync_tabs();
            let keys = (app.sessions[0].key, app.sessions[1].key);
            app.join(keys.0, keys.1, keys.0);
        }
        if std::env::var_os("RELAY_UI_PALETTE").is_some() {
            app.palette = Some(Palette::default());
        }
        if std::env::var_os("RELAY_UI_DETACH").is_some() {
            app.detach(0);
        }
        // Show the library with the session's tab open beside it.
        if std::env::var_os("RELAY_UI_LIBRARY").is_some() {
            app.activate(None);
        }
    }
    // Many sessions open with the second in front, to judge the selected tab among the rest.
    if let Some(count) = std::env::var("RELAY_UI_SESSIONS")
        .ok()
        .and_then(|n| n.parse::<usize>().ok())
    {
        for i in 1..=count {
            let mut connection = app.connections[1].clone();
            connection.name = format!("Test {i}");
            let terminal = crate::terminal::Terminal::demo(transcript());
            app.sessions.push(session::Session::demo(
                connection,
                terminal,
                app.ctx.clone(),
            ));
        }
        app.sync_tabs();
        app.select_tab(2);
    }
    if std::env::var_os("RELAY_UI_FILES").is_some() {
        files(app);
    }
    if std::env::var_os("RELAY_UI_SSH").is_some() {
        let mut c = app.connections[1].clone();
        c.ssh.jump = "ops@bastion.example.com".into();
        c.ssh.forwards = "L8080:localhost:80, D1080".into();
        app.edit(c);
    }
    if std::env::var_os("RELAY_UI_RDP").is_some() {
        let mut c = app.connections[0].clone();
        c.rdp.gateway = "gateway.example.com".into();
        c.rdp.drives = true;
        c.rdp.display = model::Display::Fixed {
            width: 1920,
            height: 1080,
        };
        app.edit(c);
    }
    if std::env::var_os("RELAY_UI_VNC").is_some() {
        let mut c = app.connections[0].clone();
        c.protocol = Protocol::Vnc;
        c.port = 5900;
        c.username = String::new();
        c.vnc.view_only = true;
        app.edit(c);
    }
    // A confirmation dialog: close (the first tab's session), delete, move or quit.
    match std::env::var("RELAY_UI_CONFIRM").as_deref() {
        Ok("delete") => app.modal = Modal::Delete(vec![1]),
        Ok("move") => app.modal = Modal::MoveToGroup(vec![1, 2], "Studio".into()),
        Ok("quit") => app.quitting = true,
        _ => {}
    }
    // A live VNC session, to `host:port`.
    if let Ok(target) = std::env::var("RELAY_UI_VNC_SESSION")
        && let Some(c) = Connection::quick(&format!("vnc://{target}"))
    {
        app.connect(c, false);
        if std::env::var("RELAY_UI_CONFIRM").as_deref() == Ok("close") {
            app.modal = Modal::Close(0);
        }
    }
    if let Some(kind) = std::env::var_os("RELAY_UI_DATABASE") {
        app.modal = Modal::Library;
        app.db_kind = if kind == "mysql" {
            LibraryKind::Mysql
        } else {
            LibraryKind::Mssql
        };
        app.db_mysql.tls = std::env::var_os("RELAY_UI_NO_TLS").is_none();
    }
    if std::env::var_os("RELAY_UI_IMPORT").is_some() {
        app.modal = Modal::Import(
            csv_io::read(
                &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(csv_io::FIXTURE),
                &[],
            )
            .unwrap(),
        );
    }
    if std::env::var_os("RELAY_UI_FIXTURE").is_some() {
        app.connections = csv_io::read(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(csv_io::FIXTURE),
            &[],
        )
        .unwrap()
        .connections;
    }
}

/// Moves the pointer to `RELAY_UI_HOVER=x,y` to capture hover states, after right-clicking
/// at `RELAY_UI_CONTEXT=x,y` once to capture a context menu.
pub fn hover(raw_input: &mut egui::RawInput) {
    let point = |name: &str| {
        let position = std::env::var(name).ok()?;
        let (x, y) = position.split_once(',')?;
        Some(egui::pos2(x.parse().ok()?, y.parse().ok()?))
    };
    static FRAMES: AtomicU32 = AtomicU32::new(0);
    let frame = FRAMES.fetch_add(1, Ordering::Relaxed);
    let context = point("RELAY_UI_CONTEXT");
    if let Some(pos) = context {
        // Pressed and released on separate frames, as a real click is.
        if let Some(pressed) = match frame {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        } {
            raw_input.events.push(egui::Event::PointerMoved(pos));
            raw_input.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Secondary,
                pressed,
                modifiers: Default::default(),
            });
        }
    }
    // The hover waits for the right-click, which moves the pointer too.
    if let Some(pos) = point("RELAY_UI_HOVER")
        && (context.is_none() || frame > 2)
    {
        raw_input.events.push(egui::Event::PointerMoved(pos));
    }
}

pub fn capture(ctx: &egui::Context) {
    let Some(path) = std::env::var_os("RELAY_UI_CHECK") else {
        return;
    };
    // Shows the fixture without capturing it, so Relay repaints only when it would anyway,
    // for measuring what it costs while idle.
    if path == "idle" {
        return;
    }
    static FRAMES: AtomicU32 = AtomicU32::new(0);
    // The terminal fixture needs its transcript in place before the capture.
    let frame = match std::env::var("RELAY_UI_FRAMES").map(|f| f.parse()) {
        Ok(Ok(frames)) => frames,
        _ if std::env::var_os("RELAY_UI_TERMINAL").is_some() => 90,
        _ => 5,
    };
    if FRAMES.fetch_add(1, Ordering::Relaxed) == frame {
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
    }
    let events = ctx.input(|input| input.events.clone());
    for event in &events {
        if let egui::Event::Screenshot { image, .. } = event {
            let pixels: Vec<_> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
            image::save_buffer(
                &path,
                &pixels,
                image.width() as u32,
                image.height() as u32,
                image::ColorType::Rgba8,
            )
            .expect("save UI check image");
            // Exit directly: a normal close would stop at the open-sessions confirmation.
            std::process::exit(0);
        }
    }
    ctx.request_repaint();
}

/// The README screenshots' session, with lines short enough for split view.
fn transcript() -> &'static [u8] {
    if std::env::var_os("RELAY_UI_SHOWCASE").is_some() {
        SHOWCASE_TRANSCRIPT
    } else {
        TRANSCRIPT
    }
}

const SHOWCASE_TRANSCRIPT: &[u8] = b"\x1b[2J\x1b[H\
Last login: Wed Oct 28 09:14:02 2026 from 192.0.2.10\r\n\
\x1b[1;32mdeploy@build\x1b[0m:\x1b[1;34m~\x1b[0m$ df -h /srv\r\n\
Filesystem      Size  Used Avail Use% Mounted on\r\n\
/dev/nvme0n1p2  468G  187G  258G  42% /srv\r\n\
\x1b[1;32mdeploy@build\x1b[0m:\x1b[1;34m~\x1b[0m$ systemctl is-active nginx relay-ci\r\n\
active\r\n\
active\r\n\
\x1b[1;32mdeploy@build\x1b[0m:\x1b[1;34m~\x1b[0m$ ls apps\r\n\
\x1b[1;36mcurrent\x1b[0m  \x1b[1;34mrelay-0.4.0\x1b[0m  \x1b[1;34mrelay-0.5.0\x1b[0m  \x1b[1;34mrelay-0.5.1\x1b[0m\r\n\
\x1b[1;32mdeploy@build\x1b[0m:\x1b[1;34m~\x1b[0m$ git -C apps/current log --oneline -4\r\n\
\x1b[33m5fac73a\x1b[0m (\x1b[1;36mHEAD -> \x1b[1;32mmain\x1b[0m) Add quick open\r\n\
\x1b[33m4938270\x1b[0m Reconnect SSH after network drops\r\n\
\x1b[33mf52e8fb\x1b[0m Browse files over SFTP\r\n\
\x1b[33m0c1d9a4\x1b[0m Share a library through MySQL\r\n\
\x1b[1;32mdeploy@build\x1b[0m:\x1b[1;34m~\x1b[0m$ ./deploy.sh staging\r\n\
\x1b[1;34m==>\x1b[0m \x1b[1mBuilding release\x1b[0m\r\n    \
\x1b[1;32mFinished\x1b[0m `release` profile in 41.2s\r\n\
\x1b[1;34m==>\x1b[0m \x1b[1mUploading relay-0.5.1.tar.gz (18 MB)\x1b[0m\r\n\
\x1b[1;34m==>\x1b[0m \x1b[1mRestarting relay-ci on staging\x1b[0m\r\n\
\x1b[32m\xe2\x9c\x93\x1b[0m Deployed 5fac73a to staging in 58s\r\n\
\x1b[1;32mdeploy@build\x1b[0m:\x1b[1;34m~\x1b[0m$ ls -lhG releases\r\n\
total 54M\r\n\
-rw-r--r-- 1 deploy 18M Oct 12 16:44 \x1b[1;31mrelay-0.4.0.tar.gz\x1b[0m\r\n\
-rw-r--r-- 1 deploy 18M Oct 20 11:02 \x1b[1;31mrelay-0.5.0.tar.gz\x1b[0m\r\n\
-rw-r--r-- 1 deploy 18M Oct 28 09:21 \x1b[1;31mrelay-0.5.1.tar.gz\x1b[0m\r\n\
\x1b[1;32mdeploy@build\x1b[0m:\x1b[1;34m~\x1b[0m$ \x1b[7m \x1b[0m";

/// A typical SSH session for the terminal rendering check.
const TRANSCRIPT: &[u8] = b"\x1b[2J\x1b[H\
Welcome to Ubuntu 24.04.1 LTS (GNU/Linux 6.8.0-45-generic x86_64)\r\n\r\n \
* Documentation:  https://help.ubuntu.com\r\n \
* Management:     https://landscape.canonical.com\r\n\r\n\
Last login: Mon Oct 26 09:14:02 2026 from 192.0.2.10\r\n\
\x1b[1;32mdeploy@build\x1b[0m:\x1b[1;34m~\x1b[0m$ ls -la\r\n\
total 32\r\n\
drwxr-x--- 6 deploy deploy 4096 Oct 26 09:12 \x1b[1;34m.\x1b[0m\r\n\
drwxr-xr-x 4 root   root   4096 Sep  2 14:01 \x1b[1;34m..\x1b[0m\r\n\
-rw------- 1 deploy deploy 2211 Oct 26 09:10 .bash_history\r\n\
drwxrwxr-x 3 deploy deploy 4096 Oct 12 16:44 \x1b[1;34mapps\x1b[0m\r\n\
-rwxr-xr-x 1 deploy deploy  918 Oct 20 11:02 \x1b[1;32mdeploy.sh\x1b[0m\r\n\
lrwxrwxrwx 1 deploy deploy   21 Oct  1 08:30 \x1b[1;36mcurrent\x1b[0m -> apps/relay-0.3.0\r\n\
\x1b[1;32mdeploy@build\x1b[0m:\x1b[1;34m~\x1b[0m$ systemctl status nginx --no-pager\r\n\
\x1b[1;32m\xe2\x97\x8f\x1b[0m nginx.service - A high performance web server and a reverse proxy server\r\n     \
Loaded: loaded (/usr/lib/systemd/system/nginx.service; \x1b[1;32menabled\x1b[0m)\r\n     \
Active: \x1b[1;32mactive (running)\x1b[0m since Mon 2026-10-26 08:02:11 UTC; 1h 12min ago\r\n   \
Main PID: 1042 (nginx)\r\n\
\x1b[1;32mdeploy@build\x1b[0m:\x1b[1;34m~\x1b[0m$ git -C apps/relay log --oneline -3\r\n\
\x1b[33m5fac73a\x1b[0m Make RDP practical for daily use\r\n\
\x1b[33m4938270\x1b[0m Fix SSH sessions exiting with 255 before showing anything\r\n\
\x1b[33mf52e8fb\x1b[0m Make the library keyboard-driven, denser, and responsive\r\n\
\x1b[1;32mdeploy@build\x1b[0m:\x1b[1;34m~\x1b[0m$ \x1b[7m \x1b[0m\x1b[2m  # selection, dim and inverse text\x1b[0m";

/// A file browser on the build server with a few items and transfers, beside its terminal
/// with `RELAY_UI_SPLIT`.
fn files(app: &mut App) {
    use crate::sftp::{Browser, Direction, Entry};
    let day = 1_793_000_000;
    let item = |name: &str, dir: bool, size: u64, age: u32, mode: u32| Entry {
        name: name.into(),
        dir,
        link: false,
        special: false,
        size,
        modified: Some(day - age * 3600),
        mode: Some(mode),
    };
    let mut browser = Browser::demo(
        "/home/deploy/relay",
        vec![
            item("assets", true, 0, 30, 0o040_755),
            item("releases", true, 0, 4, 0o040_755),
            item("src", true, 0, 2, 0o040_755),
            item(".env", false, 312, 300, 0o100_600),
            item("Cargo.toml", false, 3_338, 20, 0o100_644),
            item("deploy.sh", false, 1_204, 50, 0o100_755),
            item("relay-0.4.0.tar.gz", false, 18_874_368, 1, 0o100_644),
            item("server.log", false, 4_404_019, 0, 0o100_640),
        ],
    );
    browser.home = "/home/deploy".into();
    browser.view.select_only("relay-0.4.0.tar.gz");
    browser.view.focused = true;
    browser.demo_transfer(
        "relay-0.4.0.tar.gz",
        Direction::Download,
        7_340_032,
        18_874_368,
    );
    browser.demo_transfer("Cargo.toml", Direction::Upload, 3_338, 3_338);
    match std::env::var("RELAY_UI_FILES").as_deref() {
        Ok("hostkey") => browser.demo_prompt(
            "The authenticity of host 'build.example.com (192.0.2.10)' can't be established.\n\
             ED25519 key fingerprint is SHA256:3vYkqfGx9tQ0mZbXcW8hL2pN7rJ5sD1aE4uK6oI9yTw.\n\
             This key is not known by any other names.\n\
             Are you sure you want to continue connecting (yes/no/[fingerprint])?",
            crate::askpass::Kind::YesNo,
        ),
        Ok("newfolder") => {
            browser.view.new_folder = Some("reports".into());
        }
        Ok("delete") => {
            browser.view.confirm = Some(crate::sftp::Confirm::Delete(vec![
                "relay-0.4.0.tar.gz".into(),
            ]));
        }
        // A queue longer than its share of the tab, which scrolls.
        Ok("queue") => {
            for n in 1..=8 {
                browser.demo_transfer(
                    &format!("build-{n:02}.log"),
                    Direction::Download,
                    n * 40_000,
                    400_000,
                );
            }
        }
        Ok("edit") => {
            use crate::sftp::EditState;
            browser.demo_edit("Cargo.toml", EditState::Open);
            browser.demo_edit("deploy.sh", EditState::Uploading);
            browser.demo_edit(
                ".env",
                EditState::Changed("“.env” changed on the server since you opened it.".into()),
            );
            browser.demo_edit("server.log", EditState::Failed("Permission denied.".into()));
        }
        Ok("password") => browser.demo_prompt(
            "deploy@build.example.com's password:",
            crate::askpass::Kind::Secret,
        ),
        _ => {}
    }
    let connection = app.connections[1].clone();
    if std::env::var_os("RELAY_UI_SPLIT").is_some() {
        let terminal = crate::terminal::Terminal::demo(transcript());
        app.sessions.push(session::Session::demo(
            connection.clone(),
            terminal,
            app.ctx.clone(),
        ));
    }
    app.sessions.push(session::Session::demo_files(
        connection,
        browser,
        app.ctx.clone(),
    ));
    app.sync_tabs();
    let count = app.sessions.len();
    if std::env::var_os("RELAY_UI_SPLIT").is_some() {
        let keys = (app.sessions[count - 2].key, app.sessions[count - 1].key);
        app.join(keys.0, keys.1, keys.0);
        app.activate_session(count - 2);
    } else {
        app.activate_session(count - 1);
    }
    // Show the library with the tabs open beside it.
    if std::env::var_os("RELAY_UI_LIBRARY").is_some() {
        app.activate(None);
    }
}
