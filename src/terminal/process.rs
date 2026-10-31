//! The program a terminal runs: a pseudo-terminal, with threads of its own that read its
//! output into the screen and wait for it to end.
use super::Replies;
mod queue;
use eframe::egui;
use portable_pty::{ChildKiller, CommandBuilder, ExitStatus, MasterPty, PtySize};
use queue::InputQueue;
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

pub struct Process {
    /// The screen the output is read into.
    pub parser: Arc<Mutex<vt100::Parser<Replies>>>,
    master: Box<dyn MasterPty + Send>,
    /// Input for the program, which a thread of its own writes, so a program that stops
    /// reading never holds up the window.
    input: Arc<InputQueue>,
    /// Why writing input failed, once it has.
    input_error: Arc<Mutex<Option<String>>>,
    /// Stops the process, which a thread of its own waits for.
    killer: Box<dyn ChildKiller + Send + Sync>,
    /// How the process ended, once it has, as its waiting thread saw.
    exit: Arc<Mutex<Option<std::io::Result<ExitStatus>>>>,
    started: Instant,
    /// When the process was seen to exit.
    exited: Option<Instant>,
    /// The latest output as received, before the screen interprets it, and when it arrived.
    tail: Arc<Mutex<(Vec<u8>, Instant)>>,
    /// Whether all output has been read.
    drained: Arc<AtomicBool>,
    /// How many bytes of output have arrived, so searches know when to look again.
    pub output_bytes: Arc<AtomicU64>,
    /// Whether the terminal is on screen. Output in a hidden one is still read, but does not
    /// wake the window: it shows when its tab does.
    shown: Arc<AtomicBool>,
}

impl Process {
    /// Starts `command` in a pseudo-terminal of 24 rows and 80 columns. Output wakes `ctx`
    /// while the terminal is shown, and the program ending always does.
    pub fn spawn(mut command: CommandBuilder, ctx: Option<egui::Context>) -> Result<Self, String> {
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
        let mut writer = pair.master.take_writer().map_err(|e| e.to_string())?;
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
        let input = Arc::new(InputQueue::default());
        let ended = input.clone();
        std::thread::spawn(move || {
            let status = child.wait();
            // Input has nowhere to go now, so the thread writing it ends too.
            ended.close();
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
        let queued = input.clone();
        let input_error = Arc::new(Mutex::new(None));
        let (failed, wake) = (input_error.clone(), ctx.clone());
        // It ends once the terminal and its output thread are gone, or the program stops
        // taking input.
        std::thread::spawn(move || {
            while let Some(bytes) = queued.recv() {
                if let Err(e) = writer.write_all(&bytes).and_then(|_| writer.flush()) {
                    *failed.lock().unwrap() = Some(e.to_string());
                    queued.close();
                    if let Some(ctx) = wake {
                        ctx.request_repaint();
                    }
                    break;
                }
            }
        });
        let output = parser.clone();
        let replies = input.clone();
        let reply_error = input_error.clone();
        let tail = Arc::new(Mutex::new((Vec::new(), Instant::now())));
        let drained = Arc::new(AtomicBool::new(false));
        let output_bytes = Arc::new(AtomicU64::new(0));
        let shown = Arc::new(AtomicBool::new(true));
        let (received, done, counted) = (tail.clone(), drained.clone(), output_bytes.clone());
        let visible = shown.clone();
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
                            let (latest, at) = &mut *tail;
                            latest.extend_from_slice(&bytes[..n]);
                            latest.drain(..latest.len().saturating_sub(TAIL));
                            *at = Instant::now();
                        }
                        if let Ok(mut parser) = output.lock() {
                            parser.process(&bytes[..n]);
                            // Counted only once on the screen, under the same lock a search
                            // reads both with, so it never takes output it has not seen as
                            // searched.
                            counted.fetch_add(n as u64, Ordering::Relaxed);
                            let response = std::mem::take(&mut parser.callbacks_mut().0);
                            // A program copying to the clipboard is announced from any tab.
                            let copied = parser.callbacks().1.copied.is_some();
                            drop(parser);
                            // Answers queue behind input typed before them.
                            if !response.is_empty()
                                && let Err(e) = replies.send(&response, true)
                                // Once the program has ended, a reply has no one to read it.
                                && !replies.is_closed()
                            {
                                *reply_error.lock().unwrap() = Some(e);
                                if let Some(ctx) = &ctx {
                                    ctx.request_repaint();
                                }
                            }
                            if let Some(ctx) = &ctx
                                && (copied || visible.load(Ordering::Relaxed))
                            {
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
            input,
            input_error,
            killer,
            exit,
            started: Instant::now(),
            exited: None,
            tail,
            drained,
            output_bytes,
            shown,
        })
    }

    /// Queues `bytes` for the program without waiting for it to read them. A failure to
    /// write them later is reported by [`Self::take_input_error`].
    pub fn write(&self, bytes: &[u8]) -> Result<(), String> {
        self.input.send(bytes, false)
    }

    /// Why writing input to the program failed, once; `None` while it has not.
    pub fn take_input_error(&self) -> Option<String> {
        self.input_error.lock().unwrap().take()
    }

    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), String> {
        (self.master)
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| e.to_string())
    }

    /// How the process ended, once its waiting thread has seen it; later calls return `None`.
    pub fn take_exit(&mut self) -> Option<std::io::Result<ExitStatus>> {
        let status = self.exit.lock().unwrap().take();
        if status.is_some() {
            self.exited = Some(Instant::now());
        }
        status
    }

    /// How long the process has been running, or ran before it ended.
    pub fn uptime(&self) -> Duration {
        self.exited.unwrap_or_else(Instant::now) - self.started
    }

    /// Whether the ended process's output has all arrived, or has had time to.
    pub fn output_complete(&self) -> bool {
        let quiet = self.tail.lock().unwrap().1.elapsed() >= QUIET;
        self.drained.load(Ordering::Acquire)
            || self.exited.is_some_and(|at| at.elapsed() >= QUIET) && quiet
    }

    /// The last `n` lines of output with text, oldest first. They come from the output as
    /// received, so a cleared or redrawn screen does not hide them.
    pub fn last_lines(&self, n: usize) -> Vec<String> {
        plain_lines(&self.tail.lock().unwrap().0, n)
    }

    /// Whether the terminal is on screen, so output in a hidden one does not wake the window.
    pub fn set_shown(&self, shown: bool) {
        self.shown.store(shown, Ordering::Relaxed);
    }

    /// Whether the waiting thread has seen the process end, before anything polled it.
    #[cfg(test)]
    pub fn exit_seen(&self) -> bool {
        self.exit.lock().unwrap().is_some()
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.input.close();
        // Its waiting thread collects it once it has stopped.
        let _ = self.killer.kill();
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

/// The shell a local terminal runs, with its arguments: the login shell from `SHELL` on macOS
/// and Linux (as a login shell on macOS, as Terminal does there), and on Windows PowerShell 7
/// if installed, then Windows PowerShell, then the command prompt.
pub fn local_shell() -> (std::path::PathBuf, Vec<&'static str>) {
    if cfg!(windows) {
        for program in ["pwsh.exe", "powershell.exe"] {
            if let Some(path) = crate::platform::find_program(program) {
                return (path, vec!["-NoLogo"]);
            }
        }
        let cmd =
            std::env::var_os("ComSpec").unwrap_or_else(|| r"C:\Windows\System32\cmd.exe".into());
        return (cmd.into(), vec![]);
    }
    let shell = std::env::var_os("SHELL")
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_file())
        .unwrap_or_else(|| "/bin/sh".into());
    let args = if cfg!(target_os = "macos") {
        vec!["-l"]
    } else {
        vec![]
    };
    (shell, args)
}

/// Runs `script` with the system shell, for tests and rendering checks.
#[cfg(any(test, feature = "ui-check"))]
pub fn shell(script: &str) -> CommandBuilder {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn full_input_does_not_block_output_that_requests_a_reply() {
        let process = Process::spawn(
            shell(
                r#"stty raw -echo; printf 'READY\r\n'; sleep 0.3; printf '\033[6n';
            i=0; while [ "$i" -lt 4096 ]; do
                printf 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'; i=$((i+1));
            done; printf '\r\nOUTPUT_DONE\r\n'; sleep 30"#,
            ),
            None,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !process
            .parser
            .lock()
            .unwrap()
            .screen()
            .contents()
            .contains("READY")
        {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        process.write(&vec![b'x'; 1024 * 1024]).unwrap();
        for _ in 0..300 {
            if process.write(b"x").is_err() {
                break;
            }
        }
        while !process
            .last_lines(2)
            .iter()
            .any(|line| line.contains("OUTPUT_DONE"))
        {
            assert!(
                Instant::now() < deadline,
                "output stopped behind the full input queue"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
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
    fn pty_runs_and_receives_input() {
        let command = if cfg!(windows) {
            let mut command = CommandBuilder::new(r"C:\Windows\System32\cmd.exe");
            command.args(["/d", "/q"]);
            command
        } else {
            CommandBuilder::new("/bin/sh")
        };
        let process = Process::spawn(command, None).unwrap();
        let ready = std::time::Instant::now() + std::time::Duration::from_secs(8);
        while process
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
        process.write(b"echo RELAY_PTY_OK\r\n").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        loop {
            if process
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
            assert!(
                std::time::Instant::now() < deadline,
                "the pseudo-terminal did not return output; screen={:?}",
                process.parser.lock().unwrap().screen().contents(),
            );
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        process.resize(30, 100).unwrap();
        // Once a program ends, its input closes, which lets the thread writing it go.
        let ended = Process::spawn(shell("exit 0"), None).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        while !ended.exit_seen() {
            assert!(
                std::time::Instant::now() < deadline,
                "the program did not end"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(ended.input.is_closed());
        assert!(ended.write(b"too late").is_err());
        // A program that does not read never holds up whoever sends it input.
        if cfg!(unix) {
            process.write(b"sleep 30\r").unwrap();
            let sent = std::time::Instant::now();
            for _ in 0..4 {
                let _ = process.write(&vec![b'x'; 1024 * 1024]);
            }
            assert!(sent.elapsed() < std::time::Duration::from_secs(1));
        }
    }
}
