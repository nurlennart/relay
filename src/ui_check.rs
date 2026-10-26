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
    app.settings_open = std::env::var_os("RELAY_UI_SETTINGS").is_some();
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
    })
    .collect();
    if std::env::var_os("RELAY_UI_EDITOR").is_some() {
        app.new_connection();
    }
    if std::env::var_os("RELAY_UI_DATABASE").is_some() {
        app.db_open = true;
        app.db_kind = 2;
    }
    if std::env::var_os("RELAY_UI_IMPORT").is_some() {
        app.import = Some(
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
