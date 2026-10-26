#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! Relay: a remote connection workspace for RDP and SSH.

mod app;
mod csv_io;
mod database;
mod file_dialog;
mod import;
mod model;
mod platform;
mod rdp;
mod secret;
mod session;
mod state;
mod terminal;
mod theme;

fn main() -> eframe::Result {
    app::run()
}
