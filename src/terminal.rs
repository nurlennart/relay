use crate::model::Connection;
use eframe::egui::{self, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize};
use std::{
    io::{Read, Write},
    sync::{Arc, Mutex},
};

pub struct Terminal {
    pub parser: Arc<Mutex<vt100::Parser<Replies>>>,
    master: Box<dyn MasterPty + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Box<dyn Child + Send + Sync>,
    size: (u16, u16),
    pub ended: bool,
    pub focused: bool,
    pub error: Option<String>,
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
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        let mut command =
            CommandBuilder::new(std::path::PathBuf::from(root).join("System32\\OpenSSH\\ssh.exe"));
        command.args(["-p", &c.port.to_string()]);
        if !c.username.is_empty() {
            command.args(["-l", &c.username]);
        }
        command.args(["--", &c.host]);
        Self::spawn(command, Some(ctx))
    }

    fn spawn(mut command: CommandBuilder, ctx: Option<egui::Context>) -> Result<Self, String> {
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
        let child = pair.slave.spawn_command(command).map_err(|e| {
            format!("Cannot start terminal. Check that Windows OpenSSH Client is installed: {e}")
        })?;
        drop(pair.slave);
        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            24,
            80,
            5000,
            Replies::default(),
        )));
        let output = parser.clone();
        let replies = writer.clone();
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
        })
    }

    pub fn poll(&mut self) {
        if !self.ended {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    self.ended = true;
                    self.error = Some(format!("Session ended ({status})."));
                }
                Err(e) => {
                    self.error = Some(format!("Cannot check SSH process: {e}"));
                }
                _ => {}
            }
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        let colors = super::theme::palette(ui.ctx());
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(
                    "Click terminal to type · Ctrl+Shift+C copies screen · Ctrl+Shift+V pastes",
                )
                .size(11.)
                .color(colors.muted),
            );
            if self.ended {
                ui.colored_label(
                    colors.muted,
                    self.error.as_deref().unwrap_or("Session ended"),
                );
            }
        });
        let font = FontId::monospace(14.);
        let cell = ui.fonts_mut(|f| Vec2::new(f.glyph_width(&font, 'M'), f.row_height(&font)));
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click());
        if response.clicked() {
            response.request_focus();
        }
        self.focused = response.has_focus();
        let rows = ((rect.height() / cell.y) as u16).clamp(2, 300);
        let cols = ((rect.width() / cell.x) as u16).clamp(2, 500);
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
        let mut parser = self.parser.lock().unwrap();
        if response.hovered() {
            let delta = ui.input(|i| i.raw_scroll_delta.y);
            if delta.abs() > 0. {
                let scroll = parser.screen().scrollback() as isize;
                parser
                    .screen_mut()
                    .set_scrollback((scroll + (delta / cell.y) as isize).max(0) as usize);
            }
        }
        let screen = parser.screen();
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4., colors.bg);
        for row in 0..rows {
            for col in 0..cols {
                let Some(c) = screen.cell(row, col) else {
                    continue;
                };
                if c.is_wide_continuation() {
                    continue;
                }
                let pos = rect.min + Vec2::new(col as f32 * cell.x, row as f32 * cell.y);
                let mut fg = color(c.fgcolor(), colors.text);
                let mut bg = color(c.bgcolor(), colors.bg);
                if c.inverse() {
                    std::mem::swap(&mut fg, &mut bg);
                }
                let width = if c.is_wide() { cell.x * 2. } else { cell.x };
                painter.rect_filled(Rect::from_min_size(pos, Vec2::new(width, cell.y)), 0., bg);
                painter.text(pos, egui::Align2::LEFT_TOP, c.contents(), font.clone(), fg);
                if c.underline() {
                    painter.line_segment(
                        [
                            pos + Vec2::new(0., cell.y - 1.),
                            pos + Vec2::new(width, cell.y - 1.),
                        ],
                        Stroke::new(1_f32, fg),
                    );
                }
            }
        }
        if response.has_focus() && !screen.hide_cursor() && screen.scrollback() == 0 {
            let (row, col) = screen.cursor_position();
            let pos = Pos2::new(
                rect.min.x + col as f32 * cell.x,
                rect.min.y + row as f32 * cell.y,
            );
            painter.rect_stroke(
                Rect::from_min_size(pos, cell),
                0.,
                Stroke::new(1_f32, colors.accent),
                egui::StrokeKind::Inside,
            );
        }
        let mut input = vec![];
        if response.has_focus() && !self.ended {
            let events = ui.input(|i| i.events.clone());
            for event in events {
                match event {
                    egui::Event::Text(text) => input.extend(text.as_bytes()),
                    egui::Event::Paste(text) => {
                        if screen.bracketed_paste() {
                            input.extend(b"\x1b[200~");
                        }
                        input.extend(text.replace("\r\n", "\n").replace('\n', "\r").as_bytes());
                        if screen.bracketed_paste() {
                            input.extend(b"\x1b[201~");
                        }
                    }
                    egui::Event::Copy => ui.ctx().copy_text(screen.contents()),
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => {
                        if modifiers.ctrl && modifiers.shift && key == egui::Key::C {
                            ui.ctx().copy_text(screen.contents());
                        } else if modifiers.ctrl && modifiers.shift && key == egui::Key::V {
                        } else {
                            input.extend(key_bytes(key, modifiers, screen.application_cursor()));
                        }
                    }
                    _ => {}
                }
            }
        }
        if !input.is_empty() {
            parser.screen_mut().set_scrollback(0);
            drop(parser);
            let mut writer = self.writer.lock().unwrap();
            if let Err(e) = writer.write_all(&input).and_then(|_| writer.flush()) {
                self.error = Some(format!("Cannot send terminal input: {e}"));
            }
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
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
        let mut parser = vt100::Parser::new(24, 80, 100);
        parser.process(b"\x1b[31mred\x1b[0m\r\nnext");
        assert!(parser.screen().contents().contains("red\nnext"));
        assert_eq!(
            parser.screen().cell(0, 0).unwrap().fgcolor(),
            vt100::Color::Idx(1)
        );
    }
    #[test]
    fn conpty_runs_and_receives_input() {
        let mut command = CommandBuilder::new("C:\\Windows\\System32\\cmd.exe");
        command.args(["/d", "/q"]);
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
                "ConPTY startup handshake did not complete"
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
                "ConPTY did not return terminal output; screen={:?}, status={:?}",
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
