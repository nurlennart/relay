use crate::{
    model::{Connection, Protocol},
    platform::Window,
    rdp::RdpClient,
    sftp::Browser,
    state::Library,
    terminal::Terminal,
};
use std::time::{Duration, Instant};

/// How a session's tab presents it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TabState {
    Open,
    Ended,
    /// Restored from the last run and not connected yet.
    Waiting,
}

/// An open tab: an SSH terminal, an SFTP file browser, or a remote desktop.
pub struct Session {
    pub connection: Connection,
    /// The library `connection`'s ID belongs to.
    pub library: Library,
    pub terminal: Option<Terminal>,
    /// Shows the connection's files over SFTP instead of a terminal.
    pub browsing: bool,
    pub files: Option<Browser>,
    rdp: Option<RdpClient>,
    /// Connects the remote desktop to the server's administrative session, like mstsc /admin.
    /// It belongs to the tab, not the saved connection.
    pub admin: bool,
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
    /// A shell on this computer rather than a connection.
    pub local: bool,
    /// Identifies the session while tabs move, for its detached window.
    pub key: u64,
    /// The window a detached remote desktop lives in, instead of Relay's main window.
    host: Option<Window>,
}

/// Automatic SSH reconnection gives up after this many attempts in a row.
const ATTEMPTS: u32 = 10;

/// What ssh prints when it gives up on signing in, rather than losing a connection. These are
/// not translated, so they can be recognized in its last lines.
const SIGN_IN_FAILURES: [&str; 5] = [
    "Permission denied",
    "Host key verification failed",
    "Too many authentication failures",
    "Authentication failed",
    "REMOTE HOST IDENTIFICATION HAS CHANGED",
];

/// Whether ssh's `last_lines` say it could not sign in.
pub fn sign_in_failed(last_lines: &[String]) -> bool {
    (last_lines.iter()).any(|line| SIGN_IN_FAILURES.iter().any(|f| line.contains(f)))
}

/// How long to wait before reconnecting an SSH session that exited, if at all. Only a dropped
/// connection (ssh exits with 255) qualifies, never a failed sign-in or `exit`. A first drop
/// must also come after the session had been up long enough to sign in, in case ssh failed
/// with a message `sign_in_failed` does not know.
pub fn retry_delay(
    code: Option<u32>,
    uptime: Duration,
    attempt: u32,
    sign_in_failed: bool,
) -> Option<Duration> {
    if code != Some(255) || attempt >= ATTEMPTS || sign_in_failed {
        return None;
    }
    if attempt == 0 && uptime < Duration::from_secs(10) {
        return None;
    }
    Some(Duration::from_secs((1u64 << attempt.min(5)).min(30)))
}

impl Session {
    /// A session for `connection` whose terminal runs `script` on this computer.
    #[cfg(test)]
    pub fn scripted(connection: Connection, script: &str, ctx: eframe::egui::Context) -> Self {
        let terminal = Terminal::script(script, Some(ctx.clone()));
        Self::new(connection, Some(terminal), ctx)
    }

    fn new(connection: Connection, terminal: Option<Terminal>, ctx: eframe::egui::Context) -> Self {
        Self {
            connection,
            library: Library::Personal,
            terminal,
            browsing: false,
            files: None,
            rdp: None,
            admin: false,
            error: None,
            started: Instant::now(),
            ctx,
            ended: false,
            waiting: false,
            retry: None,
            attempts: 0,
            local: false,
            key: {
                static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            },
            host: None,
        }
    }
    pub fn start(mut connection: Connection, ctx: eframe::egui::Context) -> Result<Self, String> {
        connection.validate()?;
        let terminal = terminal_for(&connection, &ctx)?;
        Ok(Self::new(connection, terminal, ctx))
    }
    /// A shell on this computer, in a tab of its own.
    pub fn local(ctx: eframe::egui::Context) -> Result<Self, String> {
        let terminal = Terminal::local(ctx.clone())?;
        Ok(Self {
            local: true,
            ..Self::new(local_connection(), Some(terminal), ctx)
        })
    }
    /// A local shell tab from the last run, which starts when it is first shown.
    pub fn restored_local(ctx: eframe::egui::Context) -> Self {
        Self {
            local: true,
            ..Self::restored(local_connection(), false, ctx)
        }
    }
    /// Browses an SSH connection's files over SFTP.
    pub fn browse(mut connection: Connection, ctx: eframe::egui::Context) -> Result<Self, String> {
        if connection.protocol != Protocol::Ssh {
            return Err("Files can be browsed on SSH connections.".into());
        }
        connection.validate()?;
        let files = Browser::open(&connection, ctx.clone(), None);
        Ok(Self {
            browsing: true,
            files: Some(files),
            ..Self::new(connection, None, ctx)
        })
    }
    /// A tab from the last run that connects when it is first shown.
    pub fn restored(connection: Connection, browsing: bool, ctx: eframe::egui::Context) -> Self {
        Self {
            ended: true,
            waiting: true,
            browsing,
            ..Self::new(connection, None, ctx)
        }
    }
    #[cfg(any(test, feature = "ui-check"))]
    pub fn demo_files(connection: Connection, files: Browser, ctx: eframe::egui::Context) -> Self {
        Self {
            browsing: true,
            files: Some(files),
            ..Self::new(connection, None, ctx)
        }
    }
    #[cfg(feature = "ui-check")]
    pub fn demo(connection: Connection, terminal: Terminal, ctx: eframe::egui::Context) -> Self {
        Self::new(connection, Some(terminal), ctx)
    }
    /// The tab's name: the connection's, marked when it is an administrative session.
    pub fn title(&self) -> std::borrow::Cow<'_, str> {
        if self.admin {
            format!("{} (admin)", self.connection.name).into()
        } else {
            self.connection.name.as_str().into()
        }
    }
    /// Connects a restored tab; other sessions are unaffected.
    pub fn resume(&mut self) -> Result<(), String> {
        if std::mem::take(&mut self.waiting) {
            self.restart(self.connection.clone())?;
        }
        Ok(())
    }
    pub fn poll(&mut self, parent: Option<Window>) {
        if self.waiting {
            return;
        }
        if self.browsing {
            if let Some(files) = &mut self.files {
                files.poll();
                self.ended = files.ended();
            }
            return;
        }
        if let Some(terminal) = &mut self.terminal {
            terminal.poll();
            if terminal.ended && !self.ended && self.retry.is_none() {
                // ssh's last words can still be on their way when it exits.
                if !terminal.output_complete() {
                    self.ctx.request_repaint_after(Duration::from_millis(50));
                    return;
                }
                // A session that stayed up a while starts its reconnection attempts afresh.
                if terminal.uptime() > Duration::from_secs(30) {
                    self.attempts = 0;
                }
                // A local shell ends when it is told to; there is nothing to reconnect.
                let failed = self.local || sign_in_failed(&terminal.last_lines(2));
                if let Some(delay) =
                    retry_delay(terminal.exit_code, terminal.uptime(), self.attempts, failed)
                {
                    self.retry = Some(Instant::now() + delay);
                    self.attempts += 1;
                }
            }
            self.ended = terminal.ended;
            if self.retry.is_some_and(|at| Instant::now() >= at) {
                self.retry = None;
                if let Err(e) = self.restart(self.connection.clone()) {
                    self.error = Some(e);
                }
            }
            // Nothing else wakes the window for a reconnection: wake it when the countdown
            // shows another second, and when the retry is due.
            if let Some(at) = self.retry {
                let left = at.saturating_duration_since(Instant::now());
                let tick = Duration::from_nanos((left.as_nanos() % 1_000_000_000) as u64);
                let wait = if tick.is_zero() {
                    left.min(Duration::from_secs(1))
                } else {
                    tick
                };
                self.ctx.request_repaint_after(wait);
            }
            return;
        }
        if self.ended {
            return;
        }
        if self.rdp.is_none() {
            match RdpClient::new(
                self.host.or(parent),
                &self.connection,
                self.admin,
                self.ctx.clone(),
            ) {
                Ok(rdp) => self.rdp = Some(rdp),
                Err(error) => {
                    self.error = Some(error);
                    self.ended = true;
                }
            }
        }
        if let Some(rdp) = &mut self.rdp {
            rdp.poll();
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
    /// Hosts a remote desktop in `window`, or back in the main window with `None`.
    pub fn set_host(&mut self, window: Option<Window>, main: Option<Window>) {
        self.host = window;
        if let (Some(rdp), Some(parent)) = (&mut self.rdp, window.or(main)) {
            rdp.set_parent(parent);
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
        if self.local {
            self.terminal = Some(Terminal::local(self.ctx.clone())?);
        } else {
            connection.validate()?;
            if self.browsing {
                // A reconnected browser returns to the folder it showed.
                let folder = self
                    .files
                    .as_ref()
                    .map(|f| f.path.clone())
                    .filter(|p| !p.is_empty());
                self.files = Some(Browser::open(&connection, self.ctx.clone(), folder));
            } else {
                self.terminal = terminal_for(&connection, &self.ctx)?;
            }
            self.rdp = None;
            self.connection = connection;
        }
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
        self.rdp.as_ref().and_then(|r| r.display_warning())
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
    pub fn can_full_screen(&self) -> bool {
        self.rdp.as_ref().is_some_and(|r| r.can_full_screen())
    }
    pub fn full_screen(&self) -> bool {
        self.rdp.as_ref().is_some_and(|r| r.full_screen())
    }
    pub fn enter_full_screen(&mut self) -> Result<(), String> {
        match &mut self.rdp {
            Some(rdp) => rdp.enter_full_screen(),
            None => Err("The remote desktop is not ready yet.".into()),
        }
    }
    pub fn leave_full_screen(&mut self) {
        if let Some(rdp) = &mut self.rdp {
            rdp.leave_full_screen();
        }
    }
    /// Whether a remote desktop has been created for this session.
    pub fn has_desktop(&self) -> bool {
        self.rdp.is_some()
    }
    /// Shows the remote desktop, or its prompts, in the rest of `ui`. Returns the area a
    /// native desktop window should cover.
    pub fn show_desktop(&mut self, ui: &mut eframe::egui::Ui) -> Option<eframe::egui::Rect> {
        self.rdp.as_mut().map(|rdp| rdp.show(ui))
    }
    /// Transfers that are queued or running in the session's file browser.
    pub fn active_transfers(&self) -> usize {
        self.files.as_ref().map_or(0, Browser::active_transfers)
    }
    /// Whether the session's terminal or desktop has the keyboard.
    pub fn keyboard_focused(&self) -> bool {
        self.terminal.as_ref().is_some_and(|t| t.focused)
            || self.rdp.as_ref().is_some_and(|r| r.focused())
    }
    /// Gives the session the keyboard the next time it is shown: its terminal, file browser, or
    /// remote desktop.
    pub fn focus(&mut self) {
        if let Some(terminal) = &mut self.terminal {
            terminal.focus();
        }
        if let Some(files) = &mut self.files {
            files.view.focus_requested = true;
        }
        if let Some(rdp) = &mut self.rdp {
            rdp.focus();
        }
    }
    /// Withdraws any request for the keyboard, such as a new session's, so a side of a pair
    /// that is not in front takes it only when clicked.
    pub fn cancel_focus(&mut self) {
        if let Some(terminal) = &mut self.terminal {
            terminal.cancel_focus();
        }
        if let Some(files) = &mut self.files {
            files.view.focus_requested = false;
        }
        if let Some(rdp) = &mut self.rdp {
            rdp.cancel_focus();
        }
    }
    /// Tells the session whether it is on screen, so a hidden one's output does not wake the
    /// window.
    pub fn set_shown(&self, shown: bool) {
        if let Some(terminal) = &self.terminal {
            terminal.set_shown(shown);
        }
        if let Some(rdp) = &self.rdp {
            rdp.set_shown(shown);
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

/// What a local shell tab shows as its connection: the shell's name and this computer.
fn local_connection() -> Connection {
    let mut connection = Connection::new();
    connection.name = "Local shell".into();
    connection.host = "localhost".into();
    connection.protocol = Protocol::Ssh;
    connection.port = 22;
    connection
}

/// The terminal a connection runs in: ssh for SSH, none for remote desktops.
fn terminal_for(
    connection: &Connection,
    ctx: &eframe::egui::Context,
) -> Result<Option<Terminal>, String> {
    match connection.protocol {
        Protocol::Ssh => Terminal::ssh(connection, ctx.clone()).map(Some),
        Protocol::Rdp => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_dropped_connections_retry_with_growing_delays() {
        let up = Duration::from_secs(120);
        let retry = |code, uptime, attempt| retry_delay(code, uptime, attempt, false);
        assert_eq!(retry(Some(255), up, 0), Some(Duration::from_secs(1)));
        assert_eq!(
            retry(Some(255), Duration::ZERO, 3),
            Some(Duration::from_secs(8))
        );
        assert_eq!(retry(Some(255), up, 9), Some(Duration::from_secs(30)));
        assert_eq!(retry(Some(255), up, ATTEMPTS), None, "gives up");
        assert_eq!(retry(Some(0), up, 0), None, "exit ends the session");
        assert_eq!(retry(Some(1), up, 0), None);
        assert_eq!(
            retry(Some(255), Duration::from_secs(2), 0),
            None,
            "a connection that never came up is not retried"
        );
        // A slow failed sign-in is not retried, on the first try or a later one.
        assert_eq!(retry_delay(Some(255), up, 0, true), None);
        assert_eq!(retry_delay(Some(255), up, 2, true), None);
    }

    #[test]
    fn exits_and_reconnections_wake_the_window_with_no_timer() {
        // Repaints requested from outside a frame, as the windowing layer would receive them.
        let ctx = eframe::egui::Context::default();
        let wakes = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
        let seen = wakes.clone();
        ctx.set_request_repaint_callback(move |info| seen.lock().unwrap().push(info.delay));
        let script = if cfg!(windows) {
            "ping -n 2 127.0.0.1 >nul& echo Connection reset by peer& exit 255"
        } else {
            "sleep 1; echo 'Connection reset by peer'; exit 255"
        };
        let terminal = Terminal::script(script, Some(ctx.clone()));
        let mut session = Session::new(Connection::new(), Some(terminal), ctx.clone());
        // A later attempt, which the time a session was up does not protect.
        session.attempts = 1;
        // The exit wakes the window by itself, with nothing checking on the process.
        let deadline = Instant::now() + Duration::from_secs(10);
        while !session.terminal.as_ref().unwrap().exit_seen() {
            assert!(Instant::now() < deadline, "the process did not end");
            std::thread::sleep(Duration::from_millis(20));
        }
        // The thread stores the exit before it wakes the window, so the wake can trail slightly.
        while !wakes.lock().unwrap().contains(&Duration::ZERO) {
            assert!(Instant::now() < deadline, "the exit woke nothing");
            std::thread::sleep(Duration::from_millis(5));
        }
        while session.retry.is_none() {
            assert!(Instant::now() < deadline, "no reconnection was scheduled");
            std::thread::sleep(Duration::from_millis(20));
            session.poll(None);
        }
        // Once egui's own first frames are past, the countdown asks to be woken within a second.
        for _ in 0..3 {
            let _ = ctx.run(Default::default(), |_| {});
        }
        let output = ctx.run(Default::default(), |_| session.poll(None));
        let delay = output.viewport_output[&eframe::egui::ViewportId::ROOT].repaint_delay;
        assert!(
            delay > Duration::ZERO && delay <= Duration::from_secs(1),
            "the countdown asked for {delay:?}"
        );
    }

    #[test]
    fn a_failed_sign_in_is_recognized_however_fast_ssh_exits() {
        // ssh can exit before its last words are read; they must still count.
        let script = if cfg!(windows) {
            "echo deploy@build: Permission denied.&& exit 255"
        } else {
            "echo 'deploy@build: Permission denied.'; exit 255"
        };
        for _ in 0..10 {
            let terminal = Terminal::script(script, None);
            let mut session = Session::new(Connection::new(), Some(terminal), Default::default());
            // A later attempt, which the time a session was up does not protect.
            session.attempts = 1;
            let deadline = Instant::now() + Duration::from_secs(10);
            while !session.ended {
                assert!(Instant::now() < deadline, "the script did not end");
                session.poll(None);
            }
            assert_eq!(session.retry, None, "a failed sign-in was retried");
            assert_eq!(session.tab_state(), TabState::Ended);
        }
    }

    #[test]
    fn sign_in_failures_are_told_from_dropped_connections() {
        let lines = |text: &str| text.lines().map(String::from).collect::<Vec<_>>();
        for failed in [
            "deploy@build.example: Permission denied (publickey,password).",
            "Host key verification failed.",
            "Received disconnect from 10.0.0.9 port 22:2: Too many authentication failures\n\
             Disconnected from 10.0.0.9 port 22",
        ] {
            assert!(sign_in_failed(&lines(failed)), "{failed}");
        }
        for dropped in [
            "client_loop: send disconnect: Broken pipe",
            "Timeout, server build.example not responding.",
            "Connection to build.example closed by remote host.\nConnection to build.example closed.",
            "ssh: connect to host build.example port 22: Network is unreachable",
        ] {
            assert!(!sign_in_failed(&lines(dropped)), "{dropped}");
        }
    }

    #[test]
    fn a_local_shell_runs_and_ends_without_reconnecting() {
        let ctx = eframe::egui::Context::default();
        let mut session = Session::local(ctx.clone()).unwrap();
        assert!(session.local && session.is_running());
        assert_eq!(session.connection.protocol, Protocol::Ssh);
        // Give the shell a moment to start, then leave it as anyone would.
        std::thread::sleep(Duration::from_millis(500));
        session
            .terminal
            .as_mut()
            .unwrap()
            .send_text("exit 255", true);
        let deadline = Instant::now() + Duration::from_secs(20);
        while session.is_running() {
            assert!(Instant::now() < deadline, "the shell did not exit");
            std::thread::sleep(Duration::from_millis(20));
            session.poll(None);
        }
        // Even the exit code a dropped ssh connection has: a shell is not reconnected.
        assert_eq!(session.retry, None);
        assert_eq!(session.tab_state(), TabState::Ended);
        // Starting it again needs no connection details.
        session.reconnect(session.connection.clone()).unwrap();
        assert!(session.is_running());
    }

    #[test]
    fn restored_tabs_wait_without_connecting() {
        let mut c = Connection::new();
        c.name = "Desk".into();
        c.host = "desk.example".into();
        let session = Session::restored(c, false, eframe::egui::Context::default());
        assert!(!session.is_running());
        assert_eq!(session.tab_state(), TabState::Waiting);
        assert!(session.terminal.is_none() && !session.has_desktop());
    }

    #[test]
    fn restored_browsers_wait_and_never_open_a_desktop() {
        let mut c = Connection::new();
        c.name = "Files".into();
        c.host = "files.example".into();
        let mut session = Session::restored(c, true, eframe::egui::Context::default());
        assert_eq!(session.tab_state(), TabState::Waiting);
        session.waiting = false;
        session.poll(None);
        assert!(session.files.is_none() && !session.has_desktop());
    }
}
