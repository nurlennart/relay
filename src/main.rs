#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! Relay: a remote connection workspace for RDP and SSH.

mod app;
mod askpass;
mod csv_io;
mod database;
mod file_dialog;
mod fonts;
mod frame;
mod import;
mod json_io;
mod model;
mod platform;
mod rdp;
mod secret;
mod session;
mod sftp;
mod state;
mod terminal;
mod theme;
mod vnc;

#[cfg(test)]
fn test_frame(
    ctx: &eframe::egui::Context,
    input: eframe::egui::RawInput,
    draw: impl FnMut(&mut eframe::egui::Ui),
) -> eframe::egui::FullOutput {
    let mut output = ctx.run_ui(input, draw);
    // These tests inspect the shapes and platform output without a GPU renderer.
    output.textures_delta.clear();
    output
}

fn main() -> eframe::Result {
    // ssh starts Relay again to ask for passwords for the file browser; see `askpass`.
    if let Some(code) = askpass::helper() {
        std::process::exit(code);
    }
    app::run()
}
