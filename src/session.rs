use crate::{
    model::{Connection, Protocol},
    rdp::RdpClient,
    terminal::Terminal,
};
use std::time::Instant;
use windows_sys::Win32::Foundation::HWND;

pub struct Session {
    pub connection: Connection,
    pub terminal: Option<Terminal>,
    rdp: Option<RdpClient>,
    pub window: HWND,
    pub error: Option<String>,
    pub started: Instant,
    ctx: eframe::egui::Context,
    ended: bool,
}
impl Session {
    pub fn start(mut connection: Connection, ctx: eframe::egui::Context) -> Result<Self, String> {
        connection.validate()?;
        let terminal = if connection.protocol == Protocol::Ssh {
            Some(Terminal::ssh(&connection, ctx.clone())?)
        } else {
            None
        };
        Ok(Self {
            connection,
            terminal,
            rdp: None,
            window: std::ptr::null_mut(),
            error: None,
            started: Instant::now(),
            ctx,
            ended: false,
        })
    }
    pub fn poll(&mut self, parent: HWND) {
        if let Some(terminal) = &mut self.terminal {
            terminal.poll();
            self.ended = terminal.ended;
            return;
        }
        if self.ended {
            return;
        }
        if self.rdp.is_none() {
            match RdpClient::new(parent, &self.connection, self.ctx.clone()) {
                Ok(rdp) => {
                    self.window = rdp.window();
                    self.rdp = Some(rdp);
                }
                Err(error) => {
                    self.error = Some(error);
                    self.ended = true;
                }
            }
        }
        if let Some(rdp) = &self.rdp
            && rdp.ended()
        {
            self.ended = true;
            self.error = rdp.error();
        }
    }
    pub fn place(&mut self, rect: Option<[i32; 4]>) {
        if let Some(rdp) = &mut self.rdp
            && let Err(error) = rdp.place(rect)
        {
            let _ = rdp.place(None);
            self.error = Some(error);
        }
    }
    pub fn display_warning(&self) -> Option<&str> {
        self.rdp.as_ref().and_then(|r| r.display_warning.as_deref())
    }
    pub fn is_running(&self) -> bool {
        !self.ended
    }
}
