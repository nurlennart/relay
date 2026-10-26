//! Opt-in rendering fixture. Never writes the user's connection library.
use super::*;
use std::sync::atomic::{AtomicU32, Ordering};

pub fn seed(app: &mut App) {
    if std::env::var_os("RELAY_UI_CHECK").is_none() {
        return;
    }
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
    })
    .collect();
    if std::env::var_os("RELAY_UI_EDITOR").is_some() {
        app.new_connection();
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

pub fn capture(ctx: &egui::Context) {
    let Some(path) = std::env::var_os("RELAY_UI_CHECK") else {
        return;
    };
    static FRAMES: AtomicU32 = AtomicU32::new(0);
    if FRAMES.fetch_add(1, Ordering::Relaxed) == 5 {
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
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
    ctx.request_repaint();
}
