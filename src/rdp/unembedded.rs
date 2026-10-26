//! Stands in for the embedded client where Windows' RDP control does not exist. Sessions
//! there run FreeRDP in a terminal instead, so these are never constructed.
use crate::{model::Connection, platform::Window};
use eframe::egui;

/// Never constructed: without the Windows control, remote desktops run in a FreeRDP window.
pub enum RdpClient {}

#[allow(clippy::unused_self)]
impl RdpClient {
    pub fn new(
        _parent: Option<Window>,
        _connection: &Connection,
        _ctx: egui::Context,
    ) -> Result<Self, String> {
        Err("Embedded remote desktops need Windows.".into())
    }
    pub fn place(&mut self, _rect: Option<[i32; 4]>, _holes: &[[i32; 4]]) -> Result<(), String> {
        match *self {}
    }
    pub fn set_parent(&mut self, _parent: Window) {
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
