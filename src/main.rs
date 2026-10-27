#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! Relay: a remote connection workspace for RDP and SSH.

mod app;
mod askpass;
mod csv_io;
mod database;
mod file_dialog;
mod import;
mod model;
mod platform;
mod rdp;
mod secret;
mod session;
mod sftp;
mod state;
mod terminal;
mod theme;

fn main() -> eframe::Result {
    // ssh starts Relay again to ask for passwords for the file browser; see `askpass`.
    if let Some(code) = askpass::helper() {
        std::process::exit(code);
    }
    app::run()
}
