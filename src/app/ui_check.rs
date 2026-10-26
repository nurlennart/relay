//! Opt-in rendering fixture. Never writes the user's connection library.
use super::*;
use std::sync::atomic::{AtomicU32, Ordering};

pub fn seed(app: &mut App) {
    if std::env::var_os("RELAY_UI_CHECK").is_none() {
        return;
    }
    app.state = state::State::default();
    app.state_path = None;
    if let Ok(name) = std::env::var("RELAY_UI_THEME")
        && let Some(theme) = theme::Theme::ALL
            .into_iter()
            .find(|t| t.name().eq_ignore_ascii_case(&name))
    {
        theme.apply(&app.ctx);
    }
    if let Ok(size) = std::env::var("RELAY_UI_SIZE")
        && let Some((w, h)) = size.split_once('x')
        && let (Ok(w), Ok(h)) = (w.parse(), h.parse())
    {
        app.ctx
            .send_viewport_cmd(egui::ViewportCommand::InnerSize(Vec2::new(w, h)));
    }
    if std::env::var_os("RELAY_UI_SETTINGS").is_some() {
        app.modal = Modal::Settings;
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
        metadata: Default::default(),
        rdp: Default::default(),
        ssh: Default::default(),
    })
    .collect();
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
    if std::env::var_os("RELAY_UI_TERMINAL").is_some() {
        let mut connection = app.connections[1].clone();
        connection.host = "build.example.com".into();
        let mut terminal = crate::terminal::Terminal::demo(TRANSCRIPT);
        if std::env::var_os("RELAY_UI_SELECTION").is_some() {
            terminal.select((13, 0), (15, 40));
        }
        app.sessions.push(session::Session::demo(
            connection,
            terminal,
            app.ctx.clone(),
        ));
        app.active = Some(0);
        if std::env::var_os("RELAY_UI_SPLIT").is_some() {
            let mut connection = app.connections[3].clone();
            connection.host = "staging.example.com".into();
            let terminal = crate::terminal::Terminal::demo(TRANSCRIPT);
            app.sessions.push(session::Session::demo(
                connection,
                terminal,
                app.ctx.clone(),
            ));
            app.split = Some(1);
        }
        if std::env::var_os("RELAY_UI_PALETTE").is_some() {
            app.palette = Some(Palette::default());
        }
        if std::env::var_os("RELAY_UI_DETACH").is_some() {
            app.detach(0);
        }
        // Show the library with the session's tab open beside it.
        if std::env::var_os("RELAY_UI_LIBRARY").is_some() {
            app.active = None;
        }
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
    if std::env::var_os("RELAY_UI_DATABASE").is_some() {
        app.modal = Modal::Library;
        app.db_kind = LibraryKind::Mssql;
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

/// Moves the pointer to `RELAY_UI_HOVER=x,y` to capture hover states.
pub fn hover(raw_input: &mut egui::RawInput) {
    if let Ok(position) = std::env::var("RELAY_UI_HOVER")
        && let Some((x, y)) = position.split_once(',')
        && let (Ok(x), Ok(y)) = (x.parse(), y.parse())
    {
        raw_input
            .events
            .push(egui::Event::PointerMoved(egui::pos2(x, y)));
    }
}

pub fn capture(ctx: &egui::Context) {
    let Some(path) = std::env::var_os("RELAY_UI_CHECK") else {
        return;
    };
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
