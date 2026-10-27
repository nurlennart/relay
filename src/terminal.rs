mod links;
mod mouse;
mod search;

use crate::model::Connection;
use eframe::egui::{self, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use portable_pty::{ChildKiller, CommandBuilder, ExitStatus, MasterPty, PtySize};
use std::{
    io::{Read, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

/// How much of the latest output is kept as it arrived, for ssh's parting words.
const TAIL: usize = 4096;
/// Where the pseudo-terminal does not report the end of the output (as on Windows), the
/// output of an exited process is complete once none has arrived for this long.
const QUIET: Duration = Duration::from_millis(250);

pub struct Terminal {
    pub parser: Arc<Mutex<vt100::Parser<Replies>>>,
    master: Box<dyn MasterPty + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    /// Stops the process, which a thread of its own waits for.
    killer: Box<dyn ChildKiller + Send + Sync>,
    /// How the process ended, once it has, as its waiting thread saw.
    exit: Arc<Mutex<Option<std::io::Result<ExitStatus>>>>,
    size: (u16, u16),
    pub ended: bool,
    pub focused: bool,
    pub error: Option<String>,
    /// The ssh exit code once the session has ended.
    pub exit_code: Option<u32>,
    started: Instant,
    /// When the process was seen to exit.
    exited: Option<Instant>,
    /// The latest output as received, before the screen interprets it, and when it arrived.
    tail: Arc<Mutex<(Vec<u8>, Instant)>>,
    /// Whether all output has been read.
    drained: Arc<AtomicBool>,
    /// How many bytes of output have arrived, so searches know when to look again.
    output_bytes: Arc<AtomicU64>,
    /// A mouse selection between two visible cells, in drag order.
    selection: Option<((u16, u16), (u16, u16))>,
    focus_requested: bool,
    /// The search bar, while it is open.
    search: Option<search::Search>,
    /// The button held down in a program that receives the mouse, and the cell last reported.
    mouse_held: Option<mouse::Button>,
    mouse_cell: Option<(u16, u16)>,
    /// Wheel movement not yet sent as whole lines.
    wheel: f32,
    /// Everything sent to the process, for tests to check.
    #[cfg(test)]
    sent: Vec<u8>,
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
        let (parser, counted) = (terminal.parser.clone(), terminal.output_bytes.clone());
        std::thread::spawn(move || {
            // Let the pseudo-terminal's startup output settle first.
            std::thread::sleep(std::time::Duration::from_millis(500));
            parser.lock().unwrap().process(transcript);
            // Counted as output, so an open search looks again.
            counted.fetch_add(transcript.len() as u64, Ordering::Relaxed);
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
        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|e| e.to_string())?;
        drop(pair.slave);
        // Waiting on a thread wakes the window once when the process ends, so nothing has to
        // check on it while it runs.
        let killer = child.clone_killer();
        let exit = Arc::new(Mutex::new(None));
        let waited = exit.clone();
        let wake = ctx.clone();
        std::thread::spawn(move || {
            let status = child.wait();
            *waited.lock().unwrap() = Some(status);
            if let Some(ctx) = wake {
                ctx.request_repaint();
            }
        });
        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            24,
            80,
            5000,
            Replies::default(),
        )));
        let output = parser.clone();
        let replies = writer.clone();
        let tail = Arc::new(Mutex::new((Vec::new(), Instant::now())));
        let drained = Arc::new(AtomicBool::new(false));
        let output_bytes = Arc::new(AtomicU64::new(0));
        let (received, done, counted) = (tail.clone(), drained.clone(), output_bytes.clone());
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
                        counted.fetch_add(n as u64, Ordering::Relaxed);
                        if let Ok(mut tail) = received.lock() {
                            let (latest, at) = &mut *tail;
                            latest.extend_from_slice(&bytes[..n]);
                            latest.drain(..latest.len().saturating_sub(TAIL));
                            *at = Instant::now();
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
            killer,
            exit,
            size: (24, 80),
            ended: false,
            focused: false,
            error: None,
            exit_code: None,
            started: Instant::now(),
            exited: None,
            tail,
            drained,
            output_bytes,
            selection: None,
            focus_requested: true,
            search: None,
            mouse_held: None,
            mouse_cell: None,
            wheel: 0.,
            #[cfg(test)]
            sent: vec![],
        })
    }

    pub fn poll(&mut self) {
        if !self.ended {
            match self.exit.lock().unwrap().take() {
                Some(Ok(status)) => {
                    self.ended = true;
                    self.exited = Some(Instant::now());
                    self.exit_code = Some(status.exit_code());
                    self.error = Some(format!("Session ended ({status})."));
                }
                Some(Err(e)) => {
                    self.ended = true;
                    self.exited = Some(Instant::now());
                    self.error = Some(format!("Cannot check SSH process: {e}"));
                }
                None => {}
            }
        }
    }

    /// A terminal printing log lines steadily, for measuring rendering under load.
    #[cfg(feature = "ui-check")]
    pub fn demo_stream(ctx: egui::Context) -> Self {
        let script = "i=0; while :; do i=$((i+1)); \
            echo \"$i 2026-10-27T21:00:00Z GET /api/v1/items?page=$i status=200 time=12ms \
            user=deploy host=build.example.com\"; sleep 0.01; done";
        Self::spawn(shell(script), Some(ctx)).unwrap()
    }

    /// Opens the search bar with `query`, for rendering checks.
    #[cfg(feature = "ui-check")]
    pub fn search_for(&mut self, query: &str) {
        let mut search = search::Search::default();
        search.query = query.into();
        self.search = Some(search);
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
        self.exited.unwrap_or_else(Instant::now) - self.started
    }

    /// Whether the ended process's output has all arrived, or has had time to.
    pub fn output_complete(&self) -> bool {
        let quiet = self.tail.lock().unwrap().1.elapsed() >= QUIET;
        self.drained.load(Ordering::Acquire)
            || self.exited.is_some_and(|at| at.elapsed() >= QUIET) && quiet
    }

    /// The last `n` lines of output with text, oldest first, such as ssh's parting words. They
    /// come from the output as received, so a cleared or redrawn screen does not hide them.
    pub fn last_lines(&self, n: usize) -> Vec<String> {
        plain_lines(&self.tail.lock().unwrap().0, n)
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

        // The search bar sits in the title bar; its shortcut works from the terminal or the bar.
        let search_focused = self
            .search
            .as_ref()
            .is_some_and(|_| ui.memory(|m| m.has_focus(response.id.with("search"))));
        if (self.focused || search_focused)
            && ui.input_mut(|i| {
                FIND_KEYS
                    .iter()
                    .any(|modifiers| i.consume_key(*modifiers, egui::Key::F))
            })
        {
            self.search.get_or_insert_with(Default::default).focus = true;
        }
        let mut step = None;
        if let Some(search) = &mut self.search {
            match search_bar(ui, bar, response.id.with("search"), search) {
                Some(SearchAction::Step(older)) => step = Some(older),
                Some(SearchAction::Close) => {
                    self.search = None;
                    self.focus_requested = true;
                }
                None => {}
            }
        }

        let mut parser = self.parser.lock().unwrap();
        let offset = parser.screen().scrollback();
        // vt100 clamps the offset to the history it holds, which reveals the history length.
        parser.screen_mut().set_scrollback(usize::MAX);
        let history = parser.screen().scrollback();
        parser.screen_mut().set_scrollback(offset);

        // Finding matches reads the whole history, so it happens again only when the query
        // changes or, at most a few times a second, when new output arrives.
        let output = self.output_bytes.load(Ordering::Relaxed);
        let mut reveal = false;
        if let Some(search) = &mut self.search {
            if search.stale(output) {
                let mut finder = search::Finder::new(&search.query);
                each_line(parser.screen_mut(), rows, cols, history, |line| {
                    finder.line(line)
                });
                let bottom = history - offset + rows as usize - 1;
                reveal = search.update(finder.found, output, bottom);
            }
            if let Some(wait) = search.refresh_in(output) {
                ui.ctx().request_repaint_after(wait);
            }
            if let Some(older) = step {
                search.step(older);
                reveal = true;
            }
            // Scroll the selected match into view, near the middle when it was out of view.
            if reveal && let Some(m) = search.current.and_then(|i| search.matches.get(i)) {
                let top = history - parser.screen().scrollback();
                let row = m[0].0;
                if row < top || row >= top + rows as usize {
                    let top = row.saturating_sub(rows as usize / 2).min(history);
                    parser.screen_mut().set_scrollback(history - top);
                    self.selection = None;
                }
            }
        }

        // Programs such as htop and vim can ask for the mouse; Shift keeps it for selecting.
        let modifiers = ui.input(|i| i.modifiers);
        let mouse_mode = parser.screen().mouse_protocol_mode();
        let encoding = parser.screen().mouse_protocol_encoding();
        let reporting =
            mouse_mode != vt100::MouseProtocolMode::None && !modifiers.shift && !self.ended;
        let pointer = ui
            .input(|i| i.pointer.hover_pos())
            .filter(|p| content.contains(*p));
        let over = response.contains_pointer() && pointer.is_some();

        // Links in view underline under the pointer and open with Ctrl+click (Cmd+click).
        let view_links: Vec<(String, Vec<(usize, u16)>)> = {
            let lines = visible_lines(parser.screen(), rows, cols);
            (lines.iter())
                .flat_map(|line| {
                    links::find(&line.text).into_iter().map(|range| {
                        (
                            line.text[range.clone()].iter().collect(),
                            line.cells[range].to_vec(),
                        )
                    })
                })
                .collect()
        };
        let hovered_link = pointer.filter(|_| over).and_then(|pos| {
            let (row, col) = cell_at(pos);
            (view_links.iter()).find(|(_, cells)| cells.contains(&(row as usize, col)))
        });
        let mut opened_link = false;
        if let Some((url, _)) = hovered_link {
            if modifiers.command {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            let pressed = ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary));
            if pressed && modifiers.command {
                ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                opened_link = true;
            } else {
                response
                    .clone()
                    .on_hover_text_at_pointer(format!("{url}\n{OPEN_LINK}"));
            }
        }

        let mut input = vec![];
        if reporting {
            let events = ui.input(|i| i.events.clone());
            for event in &events {
                match event {
                    egui::Event::PointerButton {
                        pos,
                        button,
                        pressed,
                        modifiers,
                    } => {
                        let Some(button) = mouse::Button::from_egui(*button) else {
                            continue;
                        };
                        let action = if *pressed && over && !opened_link {
                            self.mouse_held = Some(button);
                            mouse::Action::Press(button)
                        } else if !*pressed && self.mouse_held == Some(button) {
                            self.mouse_held = None;
                            mouse::Action::Release(button)
                        } else {
                            continue;
                        };
                        let at = cell_at(*pos);
                        self.mouse_cell = Some(at);
                        input.extend(mouse::report(mouse_mode, encoding, action, at, *modifiers));
                    }
                    egui::Event::PointerMoved(pos) if over || self.mouse_held.is_some() => {
                        let at = cell_at(*pos);
                        if self.mouse_cell != Some(at) {
                            self.mouse_cell = Some(at);
                            let action = mouse::Action::Motion(self.mouse_held);
                            input
                                .extend(mouse::report(mouse_mode, encoding, action, at, modifiers));
                        }
                    }
                    egui::Event::MouseWheel {
                        unit,
                        delta,
                        modifiers,
                    } if over => {
                        self.wheel += wheel_lines(*unit, delta.y, cell.y, rows);
                        let at = pointer.map_or((0, 0), cell_at);
                        while self.wheel.abs() >= 1. {
                            let button = if self.wheel > 0. {
                                mouse::Button::WheelUp
                            } else {
                                mouse::Button::WheelDown
                            };
                            self.wheel -= self.wheel.signum();
                            let action = mouse::Action::Press(button);
                            input.extend(mouse::report(
                                mouse_mode, encoding, action, at, *modifiers,
                            ));
                        }
                    }
                    _ => {}
                }
            }
            if !input.is_empty() {
                // Reports go to the program, which is only on the live screen.
                parser.screen_mut().set_scrollback(0);
            }
        } else {
            self.mouse_held = None;
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
            if response.hovered() {
                let delta = ui.input(|i| i.raw_scroll_delta.y);
                if parser.screen().alternate_screen() {
                    // Full-screen programs such as less have no history here; the wheel moves
                    // through their text with the arrow keys instead, as in other terminals.
                    self.wheel += ui.input(|i| {
                        (i.events.iter())
                            .map(|event| match event {
                                egui::Event::MouseWheel { unit, delta, .. } => {
                                    wheel_lines(*unit, delta.y, cell.y, rows)
                                }
                                _ => 0.,
                            })
                            .sum::<f32>()
                    });
                    let application_cursor = parser.screen().application_cursor();
                    while self.wheel.abs() >= 1. {
                        let key = if self.wheel > 0. {
                            egui::Key::ArrowUp
                        } else {
                            egui::Key::ArrowDown
                        };
                        self.wheel -= self.wheel.signum();
                        input.extend(key_bytes(key, egui::Modifiers::NONE, application_cursor));
                    }
                } else if delta.abs() > 0. {
                    let offset = parser.screen().scrollback();
                    let lines = (delta / cell.y).round() as isize;
                    let next = (offset as isize + lines).clamp(0, history as isize) as usize;
                    if next != offset {
                        parser.screen_mut().set_scrollback(next);
                        self.selection = None;
                    }
                }
            }
        }
        if response.clicked() {
            self.selection = None;
        }

        let screen = parser.screen();
        let scrolled = screen.scrollback();
        let top = history - scrolled;
        let selection = self
            .selection
            .map(|(a, b)| if a <= b { (a, b) } else { (b, a) });
        let selected = |row: u16, col: u16| {
            selection.is_some_and(|(start, end)| (row, col) >= start && (row, col) <= end)
        };
        // The matches in view, by visible cell, and whether each is the selected one.
        let mut highlights = std::collections::HashMap::new();
        if let Some(search) = &self.search {
            let first = search.matches.partition_point(|m| m[m.len() - 1].0 < top);
            for (i, m) in search.matches.iter().enumerate().skip(first) {
                if m[0].0 >= top + rows as usize {
                    break;
                }
                for (row, col) in m {
                    if let Some(row) = row.checked_sub(top) {
                        highlights.insert((row as u16, *col), search.current == Some(i));
                    }
                }
            }
        }

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
        // The hint needs room beside the state; narrow panes, such as split view, drop it. The
        // search bar takes its place.
        let room = bar.width() > 640.;
        let (hint, hint_color) = if self.search.is_some() {
            (String::new(), TITLE_MUTED)
        } else if scrolled > 0 {
            (
                format!("↑ {scrolled} lines back · scroll down or type to return"),
                SCROLLED,
            )
        } else if room && mouse_mode != vt100::MouseProtocolMode::None {
            (
                format!(
                    "The program uses the mouse · Shift+drag to select · {FIND_SHORTCUT} finds"
                ),
                TITLE_MUTED,
            )
        } else if room {
            (
                format!(
                    "Drag to select · Right-click copies or pastes · {COPY_SHORTCUT} · \
                     {FIND_SHORTCUT} finds"
                ),
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
                } else if let Some(current) = highlights.get(&(row, col)) {
                    (fg, bg) = if *current {
                        (TERMINAL_BG, CURRENT_MATCH)
                    } else {
                        (FOREGROUND, MATCH)
                    };
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
        if let Some((_, cells)) = hovered_link {
            for (row, col) in cells {
                let pos = content.min + Vec2::new(*col as f32 * cell.x, *row as f32 * cell.y);
                let y = pos.y + cell.y - 1.;
                painter.line_segment(
                    [Pos2::new(pos.x, y), Pos2::new(pos.x + cell.x, y)],
                    Stroke::new(1_f32, LINK),
                );
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
            // Ticks beside the scrollbar mark where matches are in the history.
            if let Some(search) = &self.search {
                let mut last = f32::NEG_INFINITY;
                for (i, m) in search.matches.iter().enumerate() {
                    let y = track.min.y + track.height() * m[0].0 as f32 / total;
                    let current = search.current == Some(i);
                    if y - last >= 2. || current {
                        last = y;
                        let tick = Rect::from_min_size(
                            Pos2::new(track.min.x - 3., y - 1.),
                            Vec2::new(6., 2.),
                        );
                        let color = if current { CURRENT_MATCH } else { MATCH_TICK };
                        painter.rect_filled(tick, 0., color);
                    }
                }
            }
        }

        let selection_text = selection
            .map(|(start, end)| screen.contents_between(start.0, start.1, end.0, end.1 + 1));
        if response.secondary_clicked() && !reporting {
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
        if response.has_focus() && !self.ended {
            let (events, modifiers) = ui.input(|i| (i.events.clone(), i.modifiers));
            let mode = Mode {
                application_cursor: screen.application_cursor(),
                bracketed_paste: screen.bracketed_paste(),
            };
            let (keys, copy) = translate(&events, modifiers, mode);
            input.extend(keys);
            if copy {
                ui.ctx()
                    .copy_text(selection_text.unwrap_or_else(|| screen.contents()));
            }
        }
        if !input.is_empty() {
            parser.screen_mut().set_scrollback(0);
            drop(parser);
            self.selection = None;
            #[cfg(test)]
            self.sent.extend(&input);
            let mut writer = self.writer.lock().unwrap();
            if let Err(e) = writer.write_all(&input).and_then(|_| writer.flush()) {
                self.error = Some(format!("Cannot send terminal input: {e}"));
            }
        }
    }
}

enum SearchAction {
    /// Select the older match, or the newer one.
    Step(bool),
    Close,
}

/// The search field and its controls at the right of the title `bar`.
fn search_bar(
    ui: &mut egui::Ui,
    bar: Rect,
    id: egui::Id,
    search: &mut search::Search,
) -> Option<SearchAction> {
    let area = Rect::from_min_max(
        Pos2::new((bar.max.x - 380.).max(bar.min.x + 150.), bar.min.y + 3.),
        Pos2::new(bar.max.x - 8., bar.max.y - 3.),
    );
    let mut ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(area)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    // The terminal keeps its dark colors in every app theme, and so does its search bar.
    *ui.visuals_mut() = egui::Visuals::dark();
    ui.visuals_mut().extreme_bg_color = TERMINAL_BG;
    ui.visuals_mut().widgets.inactive.weak_bg_fill = TITLE_BG;
    ui.spacing_mut().item_spacing.x = 2.;
    let mut action = None;
    // Drawn with lines, as the interface font has no arrows.
    let button = |ui: &mut egui::Ui, points: &[[f32; 2]], hint: &str| {
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(22.), Sense::click());
        if response.hovered() {
            ui.painter().rect_filled(rect, 4., BORDER);
        }
        let stroke = Stroke::new(1.5_f32, TITLE_TEXT);
        let point = |[x, y]: [f32; 2]| rect.center() + Vec2::new(x, y);
        for pair in points.chunks(2) {
            ui.painter()
                .line_segment([point(pair[0]), point(pair[1])], stroke);
        }
        response.on_hover_text(hint).clicked()
    };
    let cross = [[-4., -4.], [4., 4.], [-4., 4.], [4., -4.]];
    let down = [[-4., -2.], [0., 2.], [0., 2.], [4., -2.]];
    let up = [[-4., 2.], [0., -2.], [0., -2.], [4., 2.]];
    if button(&mut ui, &cross, "Close (Esc)") {
        action = Some(SearchAction::Close);
    }
    if button(&mut ui, &down, "Newer match (Shift+Enter)") {
        action = Some(SearchAction::Step(false));
    }
    if button(&mut ui, &up, "Older match (Enter)") {
        action = Some(SearchAction::Step(true));
    }
    ui.add_space(4.);
    ui.label(
        egui::RichText::new(search.summary())
            .size(11.)
            .color(TITLE_MUTED),
    );
    ui.add_space(4.);
    let field = ui.add(
        egui::TextEdit::singleline(&mut search.query)
            .id(id)
            .hint_text(egui::RichText::new("Find in output").color(TITLE_MUTED))
            .text_color(TITLE_TEXT)
            .desired_width(ui.available_width().min(200.))
            .margin(egui::Margin::symmetric(6, 2)),
    );
    if std::mem::take(&mut search.focus) {
        field.request_focus();
        // Selecting the query lets typing replace it.
        if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), id) {
            let all = egui::text::CCursorRange::two(
                egui::text::CCursor::new(0),
                egui::text::CCursor::new(search.query.chars().count()),
            );
            state.cursor.set_char_range(Some(all));
            state.store(ui.ctx(), id);
        }
    }
    // A single-line field gives up focus on Enter and Escape.
    if field.lost_focus() {
        let (enter, shift, escape) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::Enter),
                i.modifiers.shift,
                i.key_pressed(egui::Key::Escape),
            )
        });
        if escape {
            action = Some(SearchAction::Close);
        } else if enter {
            action = Some(SearchAction::Step(!shift));
            field.request_focus();
        }
    }
    action
}

/// Wheel movement in lines of text `height` high, on a screen of `rows`.
fn wheel_lines(unit: egui::MouseWheelUnit, delta: f32, height: f32, rows: u16) -> f32 {
    match unit {
        egui::MouseWheelUnit::Line => delta,
        egui::MouseWheelUnit::Point => delta / height,
        egui::MouseWheelUnit::Page => delta * rows as f32,
    }
}

/// Reads the visible `rows` of `screen` into `line`, numbering the first visible row `top`,
/// and hands each line to `done` once it is complete, reusing `line` for the next. A row that
/// wrapped continues on the next as one line, so a line can stay incomplete for a later call.
fn read_rows(
    screen: &vt100::Screen,
    rows: std::ops::Range<u16>,
    cols: u16,
    top: usize,
    line: &mut search::Line,
    done: &mut impl FnMut(&search::Line),
) {
    for row in rows {
        for col in 0..cols {
            let Some(c) = screen.cell(row, col) else {
                continue;
            };
            if c.is_wide_continuation() {
                continue;
            }
            line.text.push(c.contents().chars().next().unwrap_or(' '));
            line.cells.push((top + row as usize, col));
        }
        if !screen.row_wrapped(row) {
            done(line);
            line.text.clear();
            line.cells.clear();
        }
    }
}

/// The lines in view, numbered from the top of the view.
fn visible_lines(screen: &vt100::Screen, rows: u16, cols: u16) -> Vec<search::Line> {
    let (mut lines, mut line) = (vec![], search::Line::default());
    read_rows(screen, 0..rows, cols, 0, &mut line, &mut |l| {
        lines.push(l.clone())
    });
    if !line.text.is_empty() {
        lines.push(line);
    }
    lines
}

/// Hands `done` every line, from the oldest in the `history` to the bottom of the screen, one
/// at a time, so the whole history is never copied. The view is moved through the history to
/// read it, and put back.
fn each_line(
    screen: &mut vt100::Screen,
    rows: u16,
    cols: u16,
    history: usize,
    mut done: impl FnMut(&search::Line),
) {
    let offset = screen.scrollback();
    let total = history + rows as usize;
    let (mut read, mut line) = (0, search::Line::default());
    while read < total {
        let scrollback = history.saturating_sub(read);
        screen.set_scrollback(scrollback);
        let top = history - scrollback;
        let first = (read - top) as u16;
        read_rows(screen, first..rows, cols, top, &mut line, &mut done);
        read = top + rows as usize;
    }
    if !line.text.is_empty() {
        done(&line);
    }
    screen.set_scrollback(offset);
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
    /// A terminal running `script` with the system shell, waking `ctx` if given.
    pub fn script(script: &str, ctx: Option<egui::Context>) -> Self {
        Self::spawn(shell(script), ctx).unwrap()
    }

    /// Whether the waiting thread has seen the process end, before anything polled it.
    pub fn exit_seen(&self) -> bool {
        self.exit.lock().unwrap().is_some()
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

/// Opens the search bar: Cmd+F on macOS, where Cmd never reaches the remote side; elsewhere
/// Ctrl+Shift+F, as plain Ctrl+F belongs to the shell and programs such as less.
const FIND_KEYS: &[egui::Modifiers] = if cfg!(target_os = "macos") {
    &[
        egui::Modifiers::MAC_CMD,
        egui::Modifiers::MAC_CMD.plus(egui::Modifiers::SHIFT),
    ]
} else {
    &[egui::Modifiers::CTRL.plus(egui::Modifiers::SHIFT)]
};
const FIND_SHORTCUT: &str = if cfg!(target_os = "macos") {
    "Cmd+F"
} else {
    "Ctrl+Shift+F"
};
const OPEN_LINK: &str = if cfg!(target_os = "macos") {
    "Cmd+click to open"
} else {
    "Ctrl+click to open"
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
const MATCH: Color32 = Color32::from_rgb(92, 76, 38);
const MATCH_TICK: Color32 = Color32::from_rgb(160, 132, 70);
const CURRENT_MATCH: Color32 = Color32::from_rgb(229, 192, 123);
const LINK: Color32 = Color32::from_rgb(120, 170, 240);
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
        // Its waiting thread collects it once it has stopped.
        let _ = self.killer.kill();
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
    /// A terminal running a quiet process, its parser for feeding it output, and a context.
    fn harness() -> (Terminal, Arc<Mutex<vt100::Parser<Replies>>>, egui::Context) {
        let quiet = if cfg!(windows) {
            "ping -n 20 127.0.0.1 >nul"
        } else {
            "sleep 20"
        };
        let terminal = Terminal::spawn(shell(quiet), None).unwrap();
        let parser = terminal.parser.clone();
        (terminal, parser, egui::Context::default())
    }

    /// Shows `terminal` for one frame with `events`, while `modifiers` are held.
    fn show_frame(
        ctx: &egui::Context,
        terminal: &mut Terminal,
        events: Vec<egui::Event>,
        modifiers: egui::Modifiers,
    ) -> egui::FullOutput {
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
    }

    fn cell_center(ctx: &egui::Context, row: f32, col: f32) -> Pos2 {
        let font = FontId::monospace(14.);
        let cell = ctx.fonts_mut(|f| Vec2::new(f.glyph_width(&font, 'M'), f.row_height(&font)));
        Pos2::new(
            PADDING + (col + 0.5) * cell.x,
            TITLE_HEIGHT + PADDING + (row + 0.5) * cell.y,
        )
    }

    /// Shows `text` on a cleared screen once the process's own startup output has settled.
    fn settle(ctx: &egui::Context, terminal: &mut Terminal, text: &[u8]) {
        show_frame(ctx, terminal, vec![], egui::Modifiers::NONE);
        std::thread::sleep(std::time::Duration::from_millis(700));
        let mut parser = terminal.parser.lock().unwrap();
        parser.process(b"\x1b[2J\x1b[H");
        parser.process(text);
    }

    fn key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn search_finds_text_in_the_history_and_scrolls_to_it() {
        let (mut terminal, parser, ctx) = harness();
        let lines: String = (1..=300).map(|i| format!("entry {i}\r\n")).collect();
        settle(&ctx, &mut terminal, lines.as_bytes());
        let find = FIND_KEYS[0];
        let none = egui::Modifiers::NONE;
        show_frame(&ctx, &mut terminal, vec![key(egui::Key::F, find)], find);
        assert!(
            terminal.search.is_some(),
            "the shortcut did not open the search"
        );
        show_frame(&ctx, &mut terminal, vec![], none);
        show_frame(
            &ctx,
            &mut terminal,
            vec![egui::Event::Text("ENTRY 42".into())],
            none,
        );
        show_frame(&ctx, &mut terminal, vec![], none);
        let search = terminal.search.as_ref().unwrap();
        assert_eq!(
            (search.query.as_str(), search.summary().as_str()),
            ("ENTRY 42", "1 of 1")
        );
        let screen = parser.lock().unwrap().screen().contents();
        assert!(
            screen.lines().any(|l| l == "entry 42"),
            "not scrolled to the match: {screen}"
        );
        assert!(
            terminal.sent.is_empty(),
            "typing in the search reached the process"
        );
        // Enter moves to the older match: entry 4 is older than entries 40–49.
        let query = terminal.search.as_mut().unwrap();
        query.query = "entry 4".into();
        show_frame(&ctx, &mut terminal, vec![], none);
        let search = terminal.search.as_ref().unwrap();
        let count = search.matches.len();
        assert_eq!(count, 1 + 10, "entry 4 and entries 40–49");
        let before = search.current.unwrap();
        show_frame(&ctx, &mut terminal, vec![key(egui::Key::Enter, none)], none);
        assert_eq!(
            terminal.search.as_ref().unwrap().current,
            Some((before + count - 1) % count)
        );
        // Escape closes it and gives the keyboard back to the terminal.
        show_frame(
            &ctx,
            &mut terminal,
            vec![key(egui::Key::Escape, none)],
            none,
        );
        assert!(terminal.search.is_none());
        show_frame(&ctx, &mut terminal, vec![], none);
        assert!(terminal.focused);
    }

    #[test]
    fn programs_that_ask_get_the_mouse_and_shift_still_selects() {
        let (mut terminal, _parser, ctx) = harness();
        // As htop does: report presses and releases, in SGR encoding.
        settle(&ctx, &mut terminal, b"\x1b[?1000h\x1b[?1006hhello world");
        let none = egui::Modifiers::NONE;
        show_frame(&ctx, &mut terminal, vec![], none);
        let pos = cell_center(&ctx, 2., 3.);
        let button = |pressed, modifiers| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers,
        };
        show_frame(
            &ctx,
            &mut terminal,
            vec![egui::Event::PointerMoved(pos), button(true, none)],
            none,
        );
        show_frame(&ctx, &mut terminal, vec![button(false, none)], none);
        assert_eq!(terminal.sent, b"\x1b[<0;4;3M\x1b[<0;4;3m");
        // With Shift, a drag selects instead.
        terminal.sent.clear();
        let shift = egui::Modifiers::SHIFT;
        let start = cell_center(&ctx, 0., 0.);
        let end = cell_center(&ctx, 0., 4.);
        let at = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: shift,
        };
        show_frame(
            &ctx,
            &mut terminal,
            vec![egui::Event::PointerMoved(start), at(start, true)],
            shift,
        );
        show_frame(
            &ctx,
            &mut terminal,
            vec![egui::Event::PointerMoved(end)],
            shift,
        );
        show_frame(&ctx, &mut terminal, vec![at(end, false)], shift);
        assert!(terminal.sent.is_empty(), "a Shift+drag was reported");
        assert_eq!(terminal.selection, Some(((0, 0), (0, 4))));
        // Without mouse reporting, a full-screen program's text moves with the wheel's arrows.
        terminal
            .parser
            .lock()
            .unwrap()
            .process(b"\x1b[?1000l\x1b[?1049h");
        let wheel = egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: Vec2::new(0., -2.),
            modifiers: none,
        };
        show_frame(
            &ctx,
            &mut terminal,
            vec![egui::Event::PointerMoved(pos), wheel],
            none,
        );
        assert_eq!(terminal.sent, b"\x1b[B\x1b[B");
    }

    #[test]
    fn ctrl_click_opens_a_link() {
        let (mut terminal, _parser, ctx) = harness();
        settle(&ctx, &mut terminal, b"docs: https://example.com/guide.");
        let command = egui::Modifiers::COMMAND;
        let pos = cell_center(&ctx, 0., 12.);
        show_frame(
            &ctx,
            &mut terminal,
            vec![egui::Event::PointerMoved(pos)],
            command,
        );
        let press = egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: command,
        };
        let output = show_frame(&ctx, &mut terminal, vec![press], command);
        let opened: Vec<_> = (output.platform_output.commands.iter())
            .filter_map(|c| match c {
                egui::OutputCommand::OpenUrl(url) => Some(url.url.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(opened, ["https://example.com/guide"]);
        // A plain click does not open it.
        let plain = egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        };
        let output = show_frame(&ctx, &mut terminal, vec![plain], egui::Modifiers::NONE);
        let commands = output.platform_output.commands;
        assert!(
            !commands
                .iter()
                .any(|c| matches!(c, egui::OutputCommand::OpenUrl(_)))
        );
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
