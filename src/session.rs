use crate::{
    model::{Connection, Protocol},
    rdp::RdpClient,
    terminal::Terminal,
};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::HWND;

/// How a session's tab presents it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TabState {
    Open,
    Ended,
    /// Restored from the last run and not connected yet.
    Waiting,
}

pub struct Session {
    pub connection: Connection,
    pub terminal: Option<Terminal>,
    rdp: Option<RdpClient>,
    pub window: HWND,
    pub error: Option<String>,
    pub started: Instant,
    ctx: eframe::egui::Context,
    ended: bool,
    /// Restored from the last run; connects when first shown.
    waiting: bool,
    /// When a dropped SSH connection reconnects on its own.
    retry: Option<Instant>,
    /// Automatic reconnection attempts since the session was last up.
    attempts: u32,
}

/// Automatic SSH reconnection gives up after this many attempts in a row.
const ATTEMPTS: u32 = 10;

/// How long to wait before reconnecting an SSH session that exited, if at all. Only a dropped
/// connection (ssh exits with 255) of a session that had been up long enough to log in
/// qualifies, so wrong passwords, unknown hosts, and `exit` never loop.
pub fn retry_delay(code: Option<u32>, uptime: Duration, attempt: u32) -> Option<Duration> {
    if code != Some(255) || attempt >= ATTEMPTS {
        return None;
    }
    if attempt == 0 && uptime < Duration::from_secs(10) {
        return None;
    }
    Some(Duration::from_secs((1u64 << attempt.min(5)).min(30)))
}

impl Session {
    fn new(connection: Connection, terminal: Option<Terminal>, ctx: eframe::egui::Context) -> Self {
        Self {
            connection,
            terminal,
            rdp: None,
            window: std::ptr::null_mut(),
            error: None,
            started: Instant::now(),
            ctx,
            ended: false,
            waiting: false,
            retry: None,
            attempts: 0,
        }
    }
    pub fn start(mut connection: Connection, ctx: eframe::egui::Context) -> Result<Self, String> {
        connection.validate()?;
        let terminal = if connection.protocol == Protocol::Ssh {
            Some(Terminal::ssh(&connection, ctx.clone())?)
        } else {
            None
        };
        Ok(Self::new(connection, terminal, ctx))
    }
    /// A tab from the last run that connects when it is first shown.
    pub fn restored(connection: Connection, ctx: eframe::egui::Context) -> Self {
        Self {
            ended: true,
            waiting: true,
            ..Self::new(connection, None, ctx)
        }
    }
    #[cfg(feature = "ui-check")]
    pub fn demo(connection: Connection, terminal: Terminal, ctx: eframe::egui::Context) -> Self {
        Self::new(connection, Some(terminal), ctx)
    }
    /// Connects a restored tab; other sessions are unaffected.
    pub fn resume(&mut self) -> Result<(), String> {
        if std::mem::take(&mut self.waiting) {
            self.restart(self.connection.clone())?;
        }
        Ok(())
    }
    pub fn poll(&mut self, parent: HWND) {
        if self.waiting {
            return;
        }
        if let Some(terminal) = &mut self.terminal {
            terminal.poll();
            if terminal.ended && !self.ended && self.retry.is_none() {
                if let Some(delay) =
                    retry_delay(terminal.exit_code, terminal.uptime(), self.attempts)
                {
                    self.retry = Some(Instant::now() + delay);
                    self.attempts += 1;
                }
            } else if !terminal.ended && terminal.uptime() > Duration::from_secs(30) {
                self.attempts = 0;
            }
            self.ended = terminal.ended;
            if self.retry.is_some_and(|at| Instant::now() >= at) {
                self.retry = None;
                if let Err(e) = self.restart(self.connection.clone()) {
                    self.error = Some(e);
                }
            }
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
    /// Shows the session at `rect`, or hides it, with `holes` cut out for overlays.
    pub fn place(&mut self, rect: Option<[i32; 4]>) {
        self.place_around(rect, &[]);
    }
    pub fn place_around(&mut self, rect: Option<[i32; 4]>, holes: &[[i32; 4]]) {
        if let Some(rdp) = &mut self.rdp
            && let Err(error) = rdp.place(rect, holes)
        {
            let _ = rdp.place(None, &[]);
            self.error = Some(error);
        }
    }
    /// Restarts the session in place, using `connection` as it is saved now.
    pub fn reconnect(&mut self, connection: Connection) -> Result<(), String> {
        self.attempts = 0;
        self.retry = None;
        self.waiting = false;
        self.restart(connection)
    }
    fn restart(&mut self, mut connection: Connection) -> Result<(), String> {
        connection.validate()?;
        self.terminal = if connection.protocol == Protocol::Ssh {
            Some(Terminal::ssh(&connection, self.ctx.clone())?)
        } else {
            None
        };
        self.rdp = None;
        self.window = std::ptr::null_mut();
        self.connection = connection;
        self.error = None;
        self.ended = false;
        self.started = Instant::now();
        Ok(())
    }
    /// Stops a pending automatic reconnection.
    pub fn stop_retrying(&mut self) {
        self.retry = None;
    }
    pub fn display_warning(&self) -> Option<&str> {
        self.rdp.as_ref().and_then(|r| r.display_warning.as_deref())
    }
    /// A transient connection state worth showing above the session.
    pub fn status(&self) -> Option<String> {
        if let Some(at) = self.retry {
            let seconds = at.saturating_duration_since(Instant::now()).as_secs() + 1;
            return Some(format!(
                "Connection lost. Reconnecting in {seconds} s (attempt {} of {ATTEMPTS})…",
                self.attempts
            ));
        }
        self.rdp
            .as_ref()
            .filter(|r| r.reconnecting())
            .map(|_| "Connection interrupted. Reconnecting…".into())
    }
    pub fn full_screen(&self) -> bool {
        self.rdp.as_ref().is_some_and(|r| r.full_screen())
    }
    pub fn enter_full_screen(&mut self) -> Result<(), String> {
        match &self.rdp {
            Some(rdp) => rdp.enter_full_screen(),
            None => Err("The remote desktop is not ready yet.".into()),
        }
    }
    pub fn is_running(&self) -> bool {
        !self.ended
    }
    pub fn tab_state(&self) -> TabState {
        if self.waiting {
            TabState::Waiting
        } else if self.ended && self.retry.is_none() {
            TabState::Ended
        } else {
            TabState::Open
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_dropped_connections_retry_with_growing_delays() {
        let up = Duration::from_secs(120);
        assert_eq!(retry_delay(Some(255), up, 0), Some(Duration::from_secs(1)));
        assert_eq!(
            retry_delay(Some(255), Duration::ZERO, 3),
            Some(Duration::from_secs(8))
        );
        assert_eq!(retry_delay(Some(255), up, 9), Some(Duration::from_secs(30)));
        assert_eq!(retry_delay(Some(255), up, ATTEMPTS), None, "gives up");
        assert_eq!(retry_delay(Some(0), up, 0), None, "exit ends the session");
        assert_eq!(retry_delay(Some(1), up, 0), None);
        assert_eq!(
            retry_delay(Some(255), Duration::from_secs(2), 0),
            None,
            "a connection that never came up is not retried"
        );
    }

    #[test]
    fn restored_tabs_wait_without_connecting() {
        let mut c = Connection::new();
        c.name = "Desk".into();
        c.host = "desk.example".into();
        let session = Session::restored(c, eframe::egui::Context::default());
        assert!(!session.is_running());
        assert_eq!(session.tab_state(), TabState::Waiting);
        assert!(session.terminal.is_none() && session.window.is_null());
    }
}
