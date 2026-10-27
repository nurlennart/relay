use crate::model::Connection;
use eframe::egui::{self, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize};
use std::{
    io::{Read, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// How much of the latest output is kept as it arrived, for ssh's parting words.
const TAIL: usize = 4096;
/// How long after the process exits its output is awaited, where the pseudo-terminal does
/// not report its end (as on Windows).
const DRAIN_WAIT: Duration = Duration::from_secs(2);

pub struct Terminal {
    pub parser: Arc<Mutex<vt100::Parser<Replies>>>,
    master: Box<dyn MasterPty + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Box<dyn Child + Send + Sync>,
    size: (u16, u16),
    pub ended: bool,
    pub focused: bool,
    pub error: Option<String>,
    /// The ssh exit code once the session has ended.
    pub exit_code: Option<u32>,
    started: Instant,
    /// When the process was seen to exit.
    exited: Option<Instant>,
    /// The latest output as received, before the screen interprets it.
    tail: Arc<Mutex<Vec<u8>>>,
    /// Whether all output has been read.
    drained: Arc<AtomicBool>,
    /// A mouse selection between two visible cells, in drag order.
    selection: Option<((u16, u16), (u16, u16))>,
    focus_requested: bool,
}

#[derive(Default)]
pub struct Replies(Vec<u8>);
impl vt100::Callbacks for Replies {
    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        _i1: Option<u8>,
        _i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        if c == 'n' && params == [&[6u16][..]] {
            let (row, col) = screen.cursor_position();
            self.0
                .extend(format!("\x1b[{};{}R", row + 1, col + 1).as_bytes());
        } else if c == 'n' && params == [&[5u16][..]] {
            self.0.extend(b"\x1b[0n");
        } else if c == 'c' {
            self.0.extend(b"\x1b[?1;2c");
        }
    }
}

impl Terminal {
    pub fn ssh(c: &Connection, ctx: egui::Context) -> Result<Self, String> {
        let mut command = CommandBuilder::new(crate::platform::ssh_program());
        command.args(c.ssh_args());
        for (flag, spec) in c.ssh.forward_args()? {
            command.args([flag, &spec]);
        }
        command.args(["--", &c.host]);
        Self::spawn(command, Some(ctx)).map_err(|e| {
            format!(
                "Cannot start ssh. Install an OpenSSH client (on Windows, the OpenSSH Client \
                 optional feature): {e}"
            )
        })
    }

    /// A quiet process whose screen shows `transcript`, for rendering checks.
    #[cfg(feature = "ui-check")]
    pub fn demo(transcript: &'static [u8]) -> Self {
        let quiet = if cfg!(windows) {
            "ping -n 60 127.0.0.1 >nul"
        } else {
            "sleep 60"
        };
        let terminal = Self::spawn(shell(quiet), None).unwrap();
        let parser = terminal.parser.clone();
        std::thread::spawn(move || {
            // Let the pseudo-terminal's startup output settle first.
            std::thread::sleep(std::time::Duration::from_millis(500));
            parser.lock().unwrap().process(transcript);
        });
        terminal
    }

    fn spawn(mut command: CommandBuilder, ctx: Option<egui::Context>) -> Result<Self, String> {
        // portable-pty rebuilds the environment from the registry, and a REG_SZ value with a
        // trailing NUL ends the block early, dropping variables such as USERPROFILE. Without
        // them ssh cannot find its home and exits with 255. Pass Relay's own environment.
        command.env_clear();
        for (key, value) in std::env::vars_os() {
            command.env(key, value);
        }
        command.env("TERM", "xterm-256color");
        let pair = portable_pty::native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| e.to_string())?;
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let writer = Arc::new(Mutex::new(
            pair.master.take_writer().map_err(|e| e.to_string())?,
        ));
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|e| e.to_string())?;
        drop(pair.slave);
        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            24,
            80,
            5000,
            Replies::default(),
        )));
        let output = parser.clone();
        let replies = writer.clone();
        let tail = Arc::new(Mutex::new(Vec::new()));
        let drained = Arc::new(AtomicBool::new(false));
        let (received, done) = (tail.clone(), drained.clone());
        std::thread::spawn(move || {
            let mut bytes = [0u8; 8192];
            loop {
                match reader.read(&mut bytes) {
                    Ok(0) => break,
                    Err(e) => {
                        if let Ok(mut parser) = output.lock() {
                            parser
                                .process(format!("\r\nTerminal output ended: {e}\r\n").as_bytes());
                        }
                        if let Some(ctx) = &ctx {
                            ctx.request_repaint();
                        }
                        break;
                    }
                    Ok(n) => {
                        if let Ok(mut tail) = received.lock() {
                            tail.extend_from_slice(&bytes[..n]);
                            let excess = tail.len().saturating_sub(TAIL);
                            tail.drain(..excess);
                        }
                        if let Ok(mut parser) = output.lock() {
                            parser.process(&bytes[..n]);
                            let response = std::mem::take(&mut parser.callbacks_mut().0);
                            drop(parser);
                            if !response.is_empty() {
                                let mut writer = replies.lock().unwrap();
                                if writer
                                    .write_all(&response)
                                    .and_then(|_| writer.flush())
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            if let Some(ctx) = &ctx {
                                ctx.request_repaint();
                            }
                        } else {
                            break;
                        }
                    }
                }
            }
            done.store(true, Ordering::Release);
            if let Some(ctx) = &ctx {
                ctx.request_repaint();
            }
        });
        Ok(Self {
            parser,
            master: pair.master,
            writer,
            child,
            size: (24, 80),
            ended: false,
            focused: false,
            error: None,
            exit_code: None,
            started: Instant::now(),
            exited: None,
            tail,
            drained,
            selection: None,
            focus_requested: true,
        })
    }

    pub fn poll(&mut self) {
        if !self.ended {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    self.ended = true;
                    self.exited = Some(Instant::now());
                    self.exit_code = Some(status.exit_code());
                    self.error = Some(format!("Session ended ({status})."));
                }
                Err(e) => {
                    self.error = Some(format!("Cannot check SSH process: {e}"));
                }
                _ => {}
            }
        }
    }

    #[cfg(feature = "ui-check")]
    pub fn select(&mut self, start: (u16, u16), end: (u16, u16)) {
        self.selection = Some((start, end));
    }

    /// Types `text` into the session, and presses Enter after it when `run` is set.
    pub fn send_text(&mut self, text: &str, run: bool) {
        if self.ended {
            return;
        }
        let bracketed = self.parser.lock().unwrap().screen().bracketed_paste();
        let bytes = snippet_bytes(text, run, bracketed);
        self.parser.lock().unwrap().screen_mut().set_scrollback(0);
        let mut writer = self.writer.lock().unwrap();
        if let Err(e) = writer.write_all(&bytes).and_then(|_| writer.flush()) {
            self.error = Some(format!("Cannot send terminal input: {e}"));
        }
    }

    /// How long the session has been running, or ran before it ended.
    pub fn uptime(&self) -> Duration {
        self.started.elapsed()
    }

    /// Whether the ended process's output has all arrived, or has had time to.
    pub fn output_complete(&self) -> bool {
        self.drained.load(Ordering::Acquire)
            || self.exited.is_some_and(|at| at.elapsed() >= DRAIN_WAIT)
    }

    /// The last `n` lines of output with text, oldest first, such as ssh's parting words. They
    /// come from the output as received, so a cleared or redrawn screen does not hide them.
    pub fn last_lines(&self, n: usize) -> Vec<String> {
        plain_lines(&self.tail.lock().unwrap(), n)
    }

    /// Focuses the terminal the next time it is shown.
    pub fn focus(&mut self) {
        self.focus_requested = true;
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        let font = FontId::monospace(14.);
        let cell = ui.fonts_mut(|f| Vec2::new(f.glyph_width(&font, 'M'), f.row_height(&font)));
        let (frame, response) =
            ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let painter = ui.painter_at(frame);
        painter.rect_filled(frame, 8., TERMINAL_BG);
        let bar = Rect::from_min_size(frame.min, Vec2::new(frame.width(), TITLE_HEIGHT));
        painter.rect_filled(
            bar,
            egui::CornerRadius {
                nw: 8,
                ne: 8,
                sw: 0,
                se: 0,
            },
            TITLE_BG,
        );
        painter.line_segment(
            [bar.left_bottom(), bar.right_bottom()],
            Stroke::new(1_f32, BORDER),
        );
        painter.rect_stroke(
            frame,
            8.,
            Stroke::new(1_f32, BORDER),
            egui::StrokeKind::Inside,
        );
        let content = Rect::from_min_max(
            Pos2::new(frame.min.x + PADDING, bar.max.y + PADDING),
            frame.max - Vec2::splat(PADDING),
        );

        if response.clicked() || std::mem::take(&mut self.focus_requested) {
            response.request_focus();
        }
        lock_focus(ui, &response);
        self.focused = response.has_focus();
        let rows = ((content.height() / cell.y) as u16).clamp(2, 300);
        let cols = ((content.width() / cell.x) as u16).clamp(2, 500);
        if self.size != (rows, cols) {
            match self.master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            }) {
                Ok(()) => {
                    self.size = (rows, cols);
                    self.parser
                        .lock()
                        .unwrap()
                        .screen_mut()
                        .set_size(rows, cols);
                }
                Err(e) => self.error = Some(format!("Terminal resize failed: {e}")),
            }
        }
        let cell_at = |pos: Pos2| {
            let offset = pos - content.min;
            let row = (offset.y / cell.y).floor().clamp(0., (rows - 1) as f32) as u16;
            let col = (offset.x / cell.x).floor().clamp(0., (cols - 1) as f32) as u16;
            (row, col)
        };
        // A drag is reported once the pointer has moved, so anchor where the button went down.
        if response.drag_started_by(egui::PointerButton::Primary)
            && let Some(pos) = ui.input(|i| i.pointer.press_origin())
        {
            response.request_focus();
            let start = cell_at(pos);
            self.selection = Some((start, start));
        }
        if response.dragged_by(egui::PointerButton::Primary)
            && let (Some(pos), Some(selection)) =
                (response.interact_pointer_pos(), &mut self.selection)
        {
            selection.1 = cell_at(pos);
        }
        if response.clicked() {
            self.selection = None;
        }

        let mut parser = self.parser.lock().unwrap();
        let offset = parser.screen().scrollback();
        // vt100 clamps the offset to the history it holds, which reveals the history length.
        parser.screen_mut().set_scrollback(usize::MAX);
        let history = parser.screen().scrollback();
        parser.screen_mut().set_scrollback(offset);
        if response.hovered() {
            let delta = ui.input(|i| i.raw_scroll_delta.y);
            if delta.abs() > 0. {
                let lines = (delta / cell.y).round() as isize;
                let next = (offset as isize + lines).clamp(0, history as isize) as usize;
                if next != offset {
                    parser.screen_mut().set_scrollback(next);
                    self.selection = None;
                }
            }
        }
        let screen = parser.screen();
        let scrolled = screen.scrollback();
        let selection = self
            .selection
            .map(|(a, b)| if a <= b { (a, b) } else { (b, a) });
        let selected = |row: u16, col: u16| {
            selection.is_some_and(|(start, end)| (row, col) >= start && (row, col) <= end)
        };

        // Title bar: state and size on the left, scroll position or hints on the right.
        let text_y = bar.center().y;
        let (state, dot) = if self.ended {
            (
                self.error.clone().unwrap_or_else(|| "Session ended".into()),
                ui.visuals().error_fg_color,
            )
        } else {
            (format!("SSH · {cols}×{rows}"), RUNNING)
        };
        painter.circle_filled(Pos2::new(bar.min.x + 16., text_y), 4., dot);
        painter.text(
            Pos2::new(bar.min.x + 28., text_y),
            egui::Align2::LEFT_CENTER,
            state,
            FontId::proportional(12.),
            TITLE_TEXT,
        );
        // The hint needs room beside the state; narrow panes, such as split view, drop it.
        let room = bar.width() > 560.;
        let (hint, hint_color) = if scrolled > 0 {
            (
                format!("↑ {scrolled} lines back · scroll down or type to return"),
                SCROLLED,
            )
        } else if room {
            (
                format!("Drag to select · Right-click copies or pastes · {COPY_SHORTCUT}"),
                TITLE_MUTED,
            )
        } else {
            (String::new(), TITLE_MUTED)
        };
        painter.text(
            Pos2::new(bar.max.x - 14., text_y),
            egui::Align2::RIGHT_CENTER,
            hint,
            FontId::proportional(11.),
            hint_color,
        );

        // Paint runs of same-styled ASCII cells as one string; other glyphs may not share the
        // monospace advance, so they are painted per cell to stay on the grid.
        let paint = |col: u16, row: u16, text: &str, cells: usize, fg: Color32, underline: bool| {
            let pos = content.min + Vec2::new(col as f32 * cell.x, row as f32 * cell.y);
            if !text.trim_end().is_empty() {
                painter.text(pos, egui::Align2::LEFT_TOP, text, font.clone(), fg);
            }
            if underline {
                let y = pos.y + cell.y - 1.;
                painter.line_segment(
                    [
                        Pos2::new(pos.x, y),
                        Pos2::new(pos.x + cells as f32 * cell.x, y),
                    ],
                    Stroke::new(1_f32, fg),
                );
            }
        };
        let (cursor_row, cursor_col) = screen.cursor_position();
        let cursor_visible = !screen.hide_cursor() && scrolled == 0 && !self.ended;
        let focused = response.has_focus();
        for row in 0..rows {
            let mut run = String::new();
            let mut run_start = 0;
            let mut run_style = None;
            for col in 0..cols {
                let Some(c) = screen.cell(row, col) else {
                    continue;
                };
                if c.is_wide_continuation() {
                    continue;
                }
                let (mut fg, mut bg) = cell_colors(c);
                let block_cursor =
                    focused && cursor_visible && (row, col) == (cursor_row, cursor_col);
                if block_cursor {
                    (fg, bg) = (TERMINAL_BG, CURSOR);
                } else if selected(row, col) {
                    bg = SELECTION;
                }
                let width = if c.is_wide() { cell.x * 2. } else { cell.x };
                if bg != TERMINAL_BG {
                    let pos = content.min + Vec2::new(col as f32 * cell.x, row as f32 * cell.y);
                    painter.rect_filled(Rect::from_min_size(pos, Vec2::new(width, cell.y)), 0., bg);
                }
                let contents = c.contents();
                let text = if contents.is_empty() { " " } else { contents };
                let style = (fg, c.underline());
                let batchable = !c.is_wide() && text.len() == 1 && text.is_ascii();
                if run_style != Some(style) || !batchable {
                    if let Some((fg, underline)) = run_style.take() {
                        paint(run_start, row, &run, run.len(), fg, underline);
                    }
                    run.clear();
                }
                if batchable {
                    if run.is_empty() {
                        run_start = col;
                        run_style = Some(style);
                    }
                    run.push_str(text);
                } else {
                    paint(col, row, text, if c.is_wide() { 2 } else { 1 }, fg, style.1);
                }
            }
            if let Some((fg, underline)) = run_style {
                paint(run_start, row, &run, run.len(), fg, underline);
            }
        }
        if cursor_visible && !focused {
            let pos =
                content.min + Vec2::new(cursor_col as f32 * cell.x, cursor_row as f32 * cell.y);
            painter.rect_stroke(
                Rect::from_min_size(pos, cell),
                0.,
                Stroke::new(1_f32, CURSOR.gamma_multiply(0.7)),
                egui::StrokeKind::Inside,
            );
        }
        if history > 0 {
            // A thin scrollbar on the right edge shows where the view sits in the history.
            let track = Rect::from_min_max(
                Pos2::new(frame.max.x - 6., content.min.y),
                Pos2::new(frame.max.x - 3., content.max.y),
            );
            let total = (history + rows as usize) as f32;
            let height = (track.height() * rows as f32 / total).max(16.);
            let bottom = track.max.y - (track.height() - height) * scrolled as f32 / history as f32;
            painter.rect_filled(
                Rect::from_min_max(
                    Pos2::new(track.min.x, bottom - height),
                    Pos2::new(track.max.x, bottom),
                ),
                2.,
                if scrolled > 0 { SCROLLED } else { BORDER },
            );
        }

        let selection_text = selection
            .map(|(start, end)| screen.contents_between(start.0, start.1, end.0, end.1 + 1));
        if response.secondary_clicked() {
            response.request_focus();
            match &selection_text {
                Some(text) => {
                    ui.ctx().copy_text(text.clone());
                    self.selection = None;
                }
                None => ui
                    .ctx()
                    .send_viewport_cmd(egui::ViewportCommand::RequestPaste),
            }
        }
        let mut input = vec![];
        if response.has_focus() && !self.ended {
            let (events, modifiers) = ui.input(|i| (i.events.clone(), i.modifiers));
            let mode = Mode {
                application_cursor: screen.application_cursor(),
                bracketed_paste: screen.bracketed_paste(),
            };
            let copy;
            (input, copy) = translate(&events, modifiers, mode);
            if copy {
                ui.ctx()
                    .copy_text(selection_text.unwrap_or_else(|| screen.contents()));
            }
        }
        if !input.is_empty() {
            parser.screen_mut().set_scrollback(0);
            drop(parser);
            self.selection = None;
            let mut writer = self.writer.lock().unwrap();
            if let Err(e) = writer.write_all(&input).and_then(|_| writer.flush()) {
                self.error = Some(format!("Cannot send terminal input: {e}"));
            }
        }
    }
}

/// The last `n` lines of `output` with text, without escape sequences or text a carriage
/// return wrote over.
fn plain_lines(output: &[u8], n: usize) -> Vec<String> {
    let mut text = String::new();
    let output = String::from_utf8_lossy(output);
    let mut chars = output.chars();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                // Control sequences end with a byte from @ to ~. Those that move the cursor to
                // a line start a new one: Windows draws line breaks that way.
                Some('[') => {
                    if let Some(end) = chars.by_ref().find(|c| ('@'..='~').contains(c))
                        && matches!(end, 'H' | 'f' | 'd' | 'E' | 'F')
                    {
                        text.push('\n');
                    }
                }
                // Operating system commands, such as window titles, end with BEL or ESC and a
                // backslash.
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\x1b' {
                            chars.next();
                        }
                        if c == '\x07' || c == '\x1b' {
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\n' | '\r' => text.push(c),
            c if c.is_control() => {}
            c => text.push(c),
        }
    }
    let mut lines: Vec<_> = (text.split('\n').rev())
        .map(|line| {
            line.trim_end_matches('\r')
                .rsplit('\r')
                .next()
                .unwrap_or("")
        })
        .filter(|line| !line.trim().is_empty())
        .take(n)
        .map(String::from)
        .collect();
    lines.reverse();
    lines
}

#[cfg(test)]
impl Terminal {
    /// A terminal running `script` with the system shell.
    pub fn script(script: &str) -> Self {
        Self::spawn(shell(script), None).unwrap()
    }
}

/// Runs `script` with the system shell, for tests and rendering checks.
#[cfg(any(test, feature = "ui-check"))]
fn shell(script: &str) -> CommandBuilder {
    let mut command;
    if cfg!(windows) {
        command = CommandBuilder::new(r"C:\Windows\System32\cmd.exe");
        command.args(["/d", "/c", script]);
    } else {
        command = CommandBuilder::new("/bin/sh");
        command.args(["-c", script]);
    }
    command
}

/// Copies the selection; Ctrl+C alone interrupts, except on macOS where Cmd is free.
const COPY_SHORTCUT: &str = if cfg!(target_os = "macos") {
    "Cmd+C"
} else {
    "Ctrl+Shift+C"
};

// The terminal keeps its own dark palette in every app theme.
const TERMINAL_BG: Color32 = Color32::from_rgb(14, 17, 22);
const TITLE_BG: Color32 = Color32::from_rgb(22, 26, 33);
const TITLE_TEXT: Color32 = Color32::from_rgb(200, 207, 218);
const TITLE_MUTED: Color32 = Color32::from_rgb(122, 132, 148);
const BORDER: Color32 = Color32::from_rgb(42, 48, 58);
const FOREGROUND: Color32 = Color32::from_rgb(215, 222, 232);
const CURSOR: Color32 = Color32::from_rgb(230, 235, 242);
const SELECTION: Color32 = Color32::from_rgb(52, 70, 98);
const RUNNING: Color32 = Color32::from_rgb(137, 222, 173);
const SCROLLED: Color32 = Color32::from_rgb(229, 192, 123);
const TITLE_HEIGHT: f32 = 30.;
const PADDING: f32 = 10.;

/// A cell's colors after bold, dim, and inverse attributes.
fn cell_colors(c: &vt100::Cell) -> (Color32, Color32) {
    let fg = match c.fgcolor() {
        // Bold brightens the eight basic colors, as most terminals do.
        vt100::Color::Idx(i) if c.bold() && i < 8 => color(vt100::Color::Idx(i + 8), FOREGROUND),
        vt100::Color::Default if c.bold() => color(vt100::Color::Idx(15), FOREGROUND),
        other => color(other, FOREGROUND),
    };
    let mut bg = color(c.bgcolor(), TERMINAL_BG);
    let mut fg = if c.dim() { mix(fg, bg, 0.45) } else { fg };
    if c.inverse() {
        std::mem::swap(&mut fg, &mut bg);
    }
    (fg, bg)
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let channel = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(
        channel(a.r(), b.r()),
        channel(a.g(), b.g()),
        channel(a.b(), b.b()),
    )
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn lock_focus(ui: &egui::Ui, response: &egui::Response) {
    // Without a lock filter, egui moves focus on Tab and arrows and drops it on Escape.
    if response.has_focus() {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                response.id,
                egui::EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                },
            )
        });
    }
}

#[derive(Clone, Copy, Default)]
struct Mode {
    application_cursor: bool,
    bracketed_paste: bool,
}

/// Converts one frame of egui input into terminal bytes, and reports whether to copy the screen.
fn translate(events: &[egui::Event], modifiers: egui::Modifiers, mode: Mode) -> (Vec<u8>, bool) {
    let mut input = vec![];
    let mut copy = false;
    // egui-winit reports Ctrl+C/X (Cmd+C/X on macOS) and Shift+Delete only as Copy/Cut, and
    // sends the text of an Alt combination after its key event. On macOS Ctrl+C stays a key.
    let mut meta = false;
    for event in events {
        match event {
            egui::Event::Text(text) => {
                if std::mem::take(&mut meta) {
                    input.push(0x1b);
                }
                input.extend(text.as_bytes());
            }
            egui::Event::Paste(text) => {
                if mode.bracketed_paste {
                    input.extend(b"\x1b[200~");
                }
                input.extend(text.replace("\r\n", "\n").replace('\n', "\r").as_bytes());
                if mode.bracketed_paste {
                    input.extend(b"\x1b[201~");
                }
            }
            egui::Event::Copy if modifiers.shift || modifiers.mac_cmd => copy = true,
            egui::Event::Copy => input.push(0x03),
            egui::Event::Cut if modifiers.mac_cmd => copy = true,
            egui::Event::Cut if modifiers.ctrl => input.push(0x18),
            egui::Event::Cut => input.extend(b"\x1b[3;2~"),
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => {
                let bytes = key_bytes(*key, *modifiers, mode.application_cursor);
                if modifiers.alt && !modifiers.ctrl {
                    if bytes.is_empty() {
                        meta = true;
                    } else {
                        input.push(0x1b);
                        input.extend(bytes);
                    }
                } else {
                    input.extend(bytes);
                }
            }
            _ => {}
        }
    }
    (input, copy)
}

/// The bytes a snippet sends: line breaks become Enter, multi-line text is pasted as one block
/// when the shell supports bracketed paste, and `run` presses Enter at the end.
fn snippet_bytes(text: &str, run: bool, bracketed: bool) -> Vec<u8> {
    let text = text.trim_end_matches(['\r', '\n']).replace("\r\n", "\n");
    let body = text.replace('\n', "\r");
    let mut bytes = vec![];
    if bracketed && text.contains('\n') {
        bytes.extend(b"\x1b[200~");
        bytes.extend(body.as_bytes());
        bytes.extend(b"\x1b[201~");
    } else {
        bytes.extend(body.as_bytes());
    }
    if run {
        bytes.push(b'\r');
    }
    bytes
}

fn key_bytes(key: egui::Key, modifiers: egui::Modifiers, application_cursor: bool) -> Vec<u8> {
    use egui::Key::*;
    if modifiers.ctrl && !modifiers.alt {
        let name = key.name();
        if name.len() == 1 && name.as_bytes()[0].is_ascii_alphabetic() {
            return vec![name.as_bytes()[0].to_ascii_uppercase() - b'@'];
        }
        if key == Space {
            return vec![0];
        }
    }
    let bytes: &[u8] = match key {
        Enter => b"\r",
        Backspace => b"\x7f",
        Tab if modifiers.shift => b"\x1b[Z",
        Tab => b"\t",
        Escape => b"\x1b",
        ArrowUp if application_cursor => b"\x1bOA",
        ArrowDown if application_cursor => b"\x1bOB",
        ArrowRight if application_cursor => b"\x1bOC",
        ArrowLeft if application_cursor => b"\x1bOD",
        ArrowUp => b"\x1b[A",
        ArrowDown => b"\x1b[B",
        ArrowRight => b"\x1b[C",
        ArrowLeft => b"\x1b[D",
        Home => b"\x1b[H",
        End => b"\x1b[F",
        Delete => b"\x1b[3~",
        Insert => b"\x1b[2~",
        PageUp => b"\x1b[5~",
        PageDown => b"\x1b[6~",
        F1 => b"\x1bOP",
        F2 => b"\x1bOQ",
        F3 => b"\x1bOR",
        F4 => b"\x1bOS",
        F5 => b"\x1b[15~",
        F6 => b"\x1b[17~",
        F7 => b"\x1b[18~",
        F8 => b"\x1b[19~",
        F9 => b"\x1b[20~",
        F10 => b"\x1b[21~",
        F11 => b"\x1b[23~",
        F12 => b"\x1b[24~",
        _ => b"",
    };
    bytes.to_vec()
}

fn color(color: vt100::Color, default: Color32) -> Color32 {
    match color {
        vt100::Color::Default => default,
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
        vt100::Color::Idx(i) if i < 16 => {
            let palette = [
                [24, 29, 35],
                [225, 108, 117],
                [137, 222, 173],
                [229, 192, 123],
                [123, 170, 237],
                [198, 151, 226],
                [112, 202, 215],
                [215, 222, 232],
                [107, 116, 132],
                [246, 140, 150],
                [170, 238, 195],
                [245, 214, 156],
                [162, 198, 248],
                [220, 180, 244],
                [153, 225, 233],
                [246, 248, 252],
            ];
            let [r, g, b] = palette[i as usize];
            Color32::from_rgb(r, g, b)
        }
        vt100::Color::Idx(i) if i < 232 => {
            let n = i - 16;
            let c = |v| if v == 0 { 0 } else { 55 + 40 * v };
            Color32::from_rgb(c(n / 36), c((n / 6) % 6), c(n % 6))
        }
        vt100::Color::Idx(i) => Color32::from_gray(8 + (i - 232) * 10),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keys_and_terminal_output() {
        assert_eq!(
            key_bytes(egui::Key::C, egui::Modifiers::CTRL, false),
            vec![3]
        );
        assert_eq!(
            key_bytes(egui::Key::ArrowUp, egui::Modifiers::NONE, true),
            b"\x1bOA"
        );
        let key = |key, modifiers| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        let mode = Mode::default();
        assert_eq!(
            translate(&[egui::Event::Copy], egui::Modifiers::CTRL, mode),
            (vec![3], false)
        );
        assert_eq!(
            translate(
                &[egui::Event::Copy],
                egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
                mode
            ),
            (vec![], true)
        );
        assert_eq!(
            translate(&[egui::Event::Cut], egui::Modifiers::CTRL, mode).0,
            vec![0x18]
        );
        assert_eq!(
            translate(
                &[
                    key(egui::Key::B, egui::Modifiers::ALT),
                    egui::Event::Text("b".into())
                ],
                egui::Modifiers::ALT,
                mode
            )
            .0,
            b"\x1bb"
        );
        assert_eq!(
            translate(
                &[egui::Event::Paste("a\r\nb".into())],
                egui::Modifiers::NONE,
                Mode {
                    bracketed_paste: true,
                    ..mode
                }
            )
            .0,
            b"\x1b[200~a\rb\x1b[201~"
        );
        let mut parser = vt100::Parser::new(24, 80, 100);
        parser.process(b"\x1b[31mred\x1b[0m\r\nnext");
        assert!(parser.screen().contents().contains("red\nnext"));
        assert_eq!(
            parser.screen().cell(0, 0).unwrap().fgcolor(),
            vt100::Color::Idx(1)
        );
    }
    #[test]
    fn snippets_type_lines_and_optionally_run() {
        assert_eq!(snippet_bytes("df -h", true, false), b"df -h\r");
        assert_eq!(snippet_bytes("df -h\n", false, true), b"df -h");
        assert_eq!(
            snippet_bytes("cd /srv\r\nls", true, true),
            b"\x1b[200~cd /srv\rls\x1b[201~\r"
        );
        assert_eq!(snippet_bytes("a\nb", false, false), b"a\rb");
    }
    #[test]
    fn focused_terminal_keeps_navigation_keys() {
        let ctx = egui::Context::default();
        let frame = |key: Option<egui::Key>, focus: bool| {
            let events = key
                .map(|key| egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                })
                .into_iter()
                .collect();
            let mut focused = false;
            let _ = ctx.run(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let (_, response) =
                            ui.allocate_exact_size(Vec2::splat(100.), Sense::click());
                        if focus {
                            response.request_focus();
                        }
                        lock_focus(ui, &response);
                        let _ = ui.button("Other");
                        focused = response.has_focus();
                    });
                },
            );
            focused
        };
        frame(None, true);
        assert!(frame(None, false));
        for key in [
            egui::Key::Tab,
            egui::Key::ArrowDown,
            egui::Key::ArrowRight,
            egui::Key::Escape,
        ] {
            frame(Some(key), false);
            assert!(
                frame(None, false),
                "{key:?} moved focus out of the terminal"
            );
        }
    }
    fn run(command: CommandBuilder) -> Terminal {
        let mut terminal = Terminal::spawn(command, None).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        while !terminal.ended && std::time::Instant::now() < deadline {
            terminal.poll();
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        // Give the reader thread a moment to drain the final output.
        std::thread::sleep(std::time::Duration::from_millis(300));
        terminal
    }
    #[test]
    fn child_receives_the_full_environment() {
        let (script, home, lookup) = if cfg!(windows) {
            (
                "echo H=%USERPROFILE%& where cmd",
                "USERPROFILE",
                r"system32\cmd.exe",
            )
        } else {
            ("echo H=$HOME; command -v sh", "HOME", "/sh")
        };
        let terminal = run(shell(script));
        let screen = terminal.parser.lock().unwrap().screen().contents();
        let home = std::env::var(home).unwrap();
        assert!(screen.contains(&format!("H={home}")), "{screen}");
        assert!(
            screen.to_lowercase().contains(lookup),
            "PATH lookup failed: {screen}"
        );
    }
    #[test]
    fn ssh_errors_reach_the_terminal() {
        let mut c = crate::model::Connection::new();
        c.protocol = crate::model::Protocol::Ssh;
        c.port = 22;
        c.host = "relay-test.invalid".into();
        let mut terminal = Terminal::ssh(&c, egui::Context::default()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while !terminal.ended && std::time::Instant::now() < deadline {
            terminal.poll();
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
        let screen = terminal.parser.lock().unwrap().screen().contents();
        assert!(
            screen.contains("relay-test.invalid"),
            "ssh output was lost: {screen:?}"
        );
    }
    #[test]
    fn dragging_selects_and_ctrl_shift_c_copies_it() {
        let quiet = if cfg!(windows) {
            "ping -n 10 127.0.0.1 >nul"
        } else {
            "sleep 10"
        };
        let mut terminal = Terminal::spawn(shell(quiet), None).unwrap();
        let parser = terminal.parser.clone();
        let ctx = egui::Context::default();
        let mut frame = |events: Vec<egui::Event>, modifiers| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800., 400.))),
                    events,
                    modifiers,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::NONE)
                        .show(ctx, |ui| terminal.show(ui));
                },
            )
        };
        frame(vec![], egui::Modifiers::NONE);
        // Let the pseudo-terminal's startup and resize output settle, then show known text.
        std::thread::sleep(std::time::Duration::from_millis(700));
        parser
            .lock()
            .unwrap()
            .process(b"\x1b[2J\x1b[Hhello world\r\nsecond line");
        frame(vec![], egui::Modifiers::NONE);
        let font = FontId::monospace(14.);
        let cell = ctx.fonts_mut(|f| Vec2::new(f.glyph_width(&font, 'M'), f.row_height(&font)));
        let at = |row: f32, col: f32| {
            Pos2::new(
                PADDING + (col + 0.5) * cell.x,
                TITLE_HEIGHT + PADDING + (row + 0.5) * cell.y,
            )
        };
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let (start, end) = (at(0., 6.), at(1., 5.));
        frame(
            vec![egui::Event::PointerMoved(start), button(start, true)],
            egui::Modifiers::NONE,
        );
        frame(vec![egui::Event::PointerMoved(end)], egui::Modifiers::NONE);
        frame(vec![button(end, false)], egui::Modifiers::NONE);
        let shortcut = egui::Modifiers::CTRL | egui::Modifiers::SHIFT;
        let output = frame(vec![egui::Event::Copy], shortcut);
        let copied: Vec<_> = output
            .platform_output
            .commands
            .iter()
            .filter_map(|c| match c {
                egui::OutputCommand::CopyText(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        let screen = parser.lock().unwrap().screen().contents();
        assert!(screen.starts_with("hello world"), "{screen:?}");
        assert_eq!(copied, ["world\nsecond"]);
    }
    #[test]
    fn plain_lines_drop_escapes_and_overwritten_text() {
        let output = b"\x1b]0;deploy@build\x07\x1b[1;31mfailed\x1b[0m\r\n\
            Password: \r\n\r\n10%\r50%\r100%\x1b[2J\x1b[5;1Hdeploy@build: Permission denied.\x1b[K";
        assert_eq!(
            plain_lines(output, 3),
            ["Password: ", "100%", "deploy@build: Permission denied."]
        );
        assert_eq!(plain_lines(output, 9)[0], "failed");
        assert!(plain_lines(b"", 2).is_empty());
    }

    #[test]
    fn last_lines_read_what_the_process_printed_last() {
        let script = if cfg!(windows) {
            "echo Welcome&& echo.&& echo deploy@build: Permission denied."
        } else {
            "echo Welcome && echo && echo 'deploy@build: Permission denied.'"
        };
        let mut terminal = Terminal::spawn(shell(script), None).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        while terminal.last_lines(1) != ["deploy@build: Permission denied."] {
            assert!(
                std::time::Instant::now() < deadline,
                "the output did not arrive: {:?}",
                terminal.last_lines(3)
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
            terminal.poll();
        }
        // Blank lines between are skipped.
        assert_eq!(terminal.last_lines(2)[0], "Welcome");
    }

    #[test]
    fn pty_runs_and_receives_input() {
        let command = if cfg!(windows) {
            let mut command = CommandBuilder::new(r"C:\Windows\System32\cmd.exe");
            command.args(["/d", "/q"]);
            command
        } else {
            CommandBuilder::new("/bin/sh")
        };
        let mut terminal = Terminal::spawn(command, None).unwrap();
        let ready = std::time::Instant::now() + std::time::Duration::from_secs(8);
        while terminal
            .parser
            .lock()
            .unwrap()
            .screen()
            .contents()
            .is_empty()
        {
            assert!(
                std::time::Instant::now() < ready,
                "the pseudo-terminal did not start"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        {
            let mut writer = terminal.writer.lock().unwrap();
            writer.write_all(b"echo RELAY_PTY_OK\r\n").unwrap();
            writer.flush().unwrap();
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        loop {
            if terminal
                .parser
                .lock()
                .unwrap()
                .screen()
                .contents()
                .lines()
                .any(|line| line.trim() == "RELAY_PTY_OK")
            {
                break;
            }
            terminal.poll();
            assert!(
                std::time::Instant::now() < deadline,
                "the pseudo-terminal did not return output; screen={:?}, status={:?}",
                terminal.parser.lock().unwrap().screen().contents(),
                terminal.error
            );
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        terminal
            .master
            .resize(PtySize {
                rows: 30,
                cols: 100,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
    }
}
