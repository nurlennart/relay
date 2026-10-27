//! File browsing over SFTP. The system's OpenSSH client runs the sftp subsystem
//! (`ssh -T -s host sftp`), so SSH config, keys, agents, jump hosts and known_hosts apply
//! exactly as in the terminal, and russh-sftp speaks the protocol over ssh's standard input
//! and output. Prompts reach the window through [`askpass`].
//!
//! A worker thread owns the connection. The browser sends it commands and reads its events
//! each frame; transfers stream through a fixed buffer and run one at a time in a queue.
use crate::{askpass, model::Connection};
use eframe::egui;
use russh_sftp::{
    client::{
        SftpSession,
        error::Error as SftpError,
        fs::{File, Metadata},
    },
    protocol::{OpenFlags, StatusCode},
};
use std::{
    collections::{BTreeSet, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc as std_mpsc},
    time::{Duration, Instant, SystemTime},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{mpsc, oneshot},
};
use tokio_util::sync::CancellationToken;

/// How long the SFTP handshake may take, which includes typing passwords and accepting a host key.
const CONNECT_TIMEOUT: u64 = 60 * 60;
/// How long the server may take to answer each request once connected.
const REQUEST_TIMEOUT: u64 = 60;
/// How much of a file is in memory at once while it is copied.
const CHUNK: usize = 256 * 1024;
/// How often a running transfer reports progress.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// Lines of ssh's error output kept to explain a failed connection.
const LOG_LINES: usize = 20;

/// One item in a remote folder.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    /// A folder, or a link to one.
    pub dir: bool,
    pub link: bool,
    /// Not a regular file, folder, or link: a socket, device, or pipe.
    pub special: bool,
    pub size: u64,
    /// Seconds since 1970.
    pub modified: Option<u32>,
    /// Unix mode bits, including the file type.
    pub mode: Option<u32>,
}

/// The file type bits of a Unix mode.
const TYPE_MASK: u32 = 0o170_000;
const TYPE_DIR: u32 = 0o040_000;
const TYPE_FILE: u32 = 0o100_000;
const TYPE_LINK: u32 = 0o120_000;

fn file_type(meta: &Metadata) -> u32 {
    meta.permissions.unwrap_or(TYPE_FILE) & TYPE_MASK
}

impl Entry {
    fn new(name: String, meta: &Metadata, target: Option<&Metadata>) -> Self {
        let kind = file_type(meta);
        let resolved = target.map_or(kind, file_type);
        Self {
            name,
            dir: resolved == TYPE_DIR,
            link: kind == TYPE_LINK,
            special: ![TYPE_DIR, TYPE_FILE, TYPE_LINK].contains(&resolved),
            size: target.unwrap_or(meta).len(),
            modified: meta.mtime,
            mode: meta.permissions,
        }
    }

    pub fn hidden(&self) -> bool {
        self.name.starts_with('.')
    }

    /// The mode as `ls -l` shows it, such as `drwxr-xr-x`.
    pub fn permissions(&self) -> String {
        let Some(mode) = self.mode else {
            return String::new();
        };
        let kind = match mode & TYPE_MASK {
            TYPE_DIR => 'd',
            TYPE_LINK => 'l',
            TYPE_FILE => '-',
            0o020_000 => 'c',
            0o060_000 => 'b',
            0o010_000 => 'p',
            0o140_000 => 's',
            _ => '?',
        };
        let mut text = String::from(kind);
        for shift in [6, 3, 0] {
            let bits = (mode >> shift) & 0o7;
            text.push(if bits & 4 != 0 { 'r' } else { '-' });
            text.push(if bits & 2 != 0 { 'w' } else { '-' });
            text.push(if bits & 1 != 0 { 'x' } else { '-' });
        }
        text
    }
}

/// Where the connection stands.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Connecting,
    Ready,
    /// The connection failed or ended, with the reason.
    Ended(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Direction {
    Upload,
    Download,
}

/// What a transfer copies. Destinations are full paths, including the item's name.
#[derive(Clone, Debug, PartialEq)]
pub enum Job {
    Upload {
        local: PathBuf,
        remote: String,
        replace: bool,
    },
    Download {
        remote: String,
        local: PathBuf,
        replace: bool,
    },
}

impl Job {
    fn replacing(&self) -> Self {
        let mut job = self.clone();
        match &mut job {
            Self::Upload { replace, .. } | Self::Download { replace, .. } => *replace = true,
        }
        job
    }
}

/// Why a transfer stopped early.
#[derive(Clone, Debug, PartialEq)]
pub enum Failure {
    Cancelled,
    /// The destination exists and the transfer was not asked to replace it.
    Exists(String),
    Error(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum TransferState {
    Queued,
    Running,
    Done,
    Stopped(Failure),
}

pub struct Transfer {
    pub id: u64,
    pub direction: Direction,
    pub name: String,
    /// The folder it goes to, for display.
    pub destination: String,
    pub done: u64,
    /// Bytes to copy, once the worker has counted them.
    pub total: Option<u64>,
    pub files_done: u64,
    pub files: u64,
    pub state: TransferState,
    /// Items skipped because they are links to folders, which could loop.
    pub skipped: u64,
    started: Option<Instant>,
    job: Job,
    cancel: CancellationToken,
}

impl Transfer {
    pub fn finished(&self) -> bool {
        matches!(self.state, TransferState::Done | TransferState::Stopped(_))
    }

    /// The fraction copied, once the size is known.
    pub fn fraction(&self) -> Option<f32> {
        match (self.state.clone(), self.total) {
            (TransferState::Done, _) => Some(1.),
            (_, Some(0)) => Some(0.),
            (_, Some(total)) => Some((self.done as f64 / total as f64).min(1.) as f32),
            _ => None,
        }
    }

    /// The full paths it copies from and to, for display.
    pub fn paths(&self) -> (String, String) {
        match &self.job {
            Job::Upload { local, remote, .. } => (local.display().to_string(), remote.clone()),
            Job::Download { remote, local, .. } => (remote.clone(), local.display().to_string()),
        }
    }

    /// Bytes per second since the transfer started.
    pub fn rate(&self) -> Option<f64> {
        let seconds = self.started?.elapsed().as_secs_f64();
        (self.state == TransferState::Running && seconds > 0.5).then(|| self.done as f64 / seconds)
    }
}

/// A question from ssh shown in the browser until it is answered.
pub struct Prompt {
    pub id: u64,
    pub text: String,
    pub kind: askpass::Kind,
    /// What has been typed so far.
    pub answer: String,
    /// Show the answer as it is typed.
    pub reveal: bool,
    reply: Option<oneshot::Sender<Option<String>>>,
}

/// How the folder's items are ordered.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Sort {
    #[default]
    Name,
    Size,
    Modified,
}

/// A question shown above the list before something irreversible happens.
#[derive(Clone, Debug, PartialEq)]
pub enum Confirm {
    /// Delete these names in the current folder.
    Delete(Vec<String>),
    /// Upload `local` into `folder`, where the items named in `existing` already exist.
    Replace {
        local: Vec<PathBuf>,
        folder: String,
        existing: Vec<String>,
    },
}

/// The browser's own state: selection, sorting, and what is being edited.
#[derive(Default)]
pub struct View {
    pub selected: BTreeSet<String>,
    /// The row the keyboard is on.
    pub cursor: Option<String>,
    /// Where a Shift-selection starts.
    pub anchor: Option<String>,
    pub sort: Sort,
    pub descending: bool,
    pub filter: String,
    pub show_hidden: bool,
    /// The path field's text while it is being edited.
    pub path_edit: Option<String>,
    /// The item being renamed, and its new name so far.
    pub rename: Option<(String, String)>,
    /// The name of the folder being created.
    pub new_folder: Option<String>,
    pub confirm: Option<Confirm>,
    /// The browser has the keyboard.
    pub focused: bool,
    /// Give the text field that just opened (path, filter, rename, new folder) the keyboard.
    pub focus_field: bool,
    pub focus_requested: bool,
    pub scroll_to_cursor: bool,
    /// Folders to go back to, most recent last.
    pub back: Vec<String>,
}

impl View {
    /// Selects only `name`, which becomes the cursor and the start of Shift-selection.
    pub fn select_only(&mut self, name: &str) {
        self.selected = BTreeSet::from([name.to_string()]);
        self.cursor = Some(name.into());
        self.anchor = Some(name.into());
    }

    /// Clears the selection and anything being edited, as when another folder opens.
    fn reset(&mut self) {
        self.selected.clear();
        self.cursor = None;
        self.anchor = None;
        self.rename = None;
        self.new_folder = None;
        self.confirm = None;
        self.filter.clear();
    }
}

enum Command {
    List { seq: u64, path: String },
    Mkdir(String),
    Rename { from: String, to: String },
    Delete(Vec<String>),
    Transfer(Queued),
    Close,
}

struct Queued {
    id: u64,
    job: Job,
    cancel: CancellationToken,
}

enum Event {
    Ask(askpass::Request),
    Gone(u64),
    Ready {
        home: String,
    },
    Listing {
        seq: u64,
        result: Result<(String, Vec<Entry>), String>,
    },
    /// An operation finished: a notice, or an error. The folder may have changed.
    Changed(Result<String, String>),
    Started {
        id: u64,
        total: u64,
        files: u64,
        skipped: u64,
    },
    Progress {
        id: u64,
        done: u64,
        files_done: u64,
    },
    Finished {
        id: u64,
        result: Result<(), Failure>,
    },
    Ended(String),
}

/// How the worker starts the SFTP server.
struct Launch {
    command: tokio::process::Command,
    /// The askpass program to name in ssh's environment, if ssh may need to ask anything.
    askpass: Option<PathBuf>,
}

/// An SFTP connection and the folder it shows.
pub struct Browser {
    commands: mpsc::UnboundedSender<Command>,
    events: std_mpsc::Receiver<Event>,
    pub status: Status,
    /// The folder shown; empty until the first listing arrives.
    pub path: String,
    pub home: String,
    pub entries: Vec<Entry>,
    /// A listing is on its way.
    pub loading: bool,
    /// Folder operations sent and not finished yet.
    pub busy: usize,
    /// The last operation's error, shown until dismissed.
    pub error: Option<String>,
    /// The last operation's confirmation, shown briefly.
    pub notice: Option<(String, Instant)>,
    pub prompts: Vec<Prompt>,
    pub transfers: Vec<Transfer>,
    pub view: View,
    /// The folder to open first: where a reconnected browser was.
    start: Option<String>,
    /// The listing request whose answer is awaited; older answers are dropped.
    seq: u64,
    /// The item to select when the awaited listing arrives.
    land_on: Option<String>,
    next_id: u64,
    pub started: Instant,
    /// Keeps a demo browser's commands going somewhere, so it stays connected.
    #[cfg(any(test, feature = "ui-check"))]
    demo_inbox: Option<mpsc::UnboundedReceiver<Command>>,
}

impl Browser {
    /// Connects to `c` with the system's ssh, opening `start` or the home folder.
    pub fn open(c: &Connection, ctx: egui::Context, start: Option<String>) -> Self {
        let mut command = std::process::Command::new(crate::platform::ssh_program());
        command.args(ssh_args(c));
        crate::platform::background(&mut command);
        Self::launch(
            Launch {
                command: command.into(),
                askpass: std::env::current_exe().ok(),
            },
            Some(ctx),
            start,
        )
    }

    /// Talks to an SFTP server started by `command` directly, without ssh, for tests.
    #[cfg(test)]
    fn local(command: std::process::Command) -> Self {
        Self::launch(
            Launch {
                command: command.into(),
                askpass: None,
            },
            None,
            None,
        )
    }

    /// Runs `command` as ssh would be run, with `askpass` as the helper, for live tests.
    #[cfg(test)]
    fn launch_for_test(command: std::process::Command, askpass: PathBuf) -> Self {
        Self::launch(
            Launch {
                command: command.into(),
                askpass: Some(askpass),
            },
            None,
            None,
        )
    }

    /// A browser showing `entries` without a connection, for rendering checks.
    #[cfg(any(test, feature = "ui-check"))]
    pub fn demo(path: &str, entries: Vec<Entry>) -> Self {
        let (commands, inbox) = mpsc::unbounded_channel();
        let (_, events) = std_mpsc::channel();
        let mut browser = Self::with(commands, events, None);
        browser.demo_inbox = Some(inbox);
        browser.status = Status::Ready;
        browser.path = path.into();
        browser.home = path.into();
        browser.entries = entries;
        browser
    }

    /// Adds a transfer as if it were under way, for rendering checks.
    #[cfg(feature = "ui-check")]
    pub fn demo_transfer(&mut self, name: &str, direction: Direction, done: u64, total: u64) {
        let (job, destination) = match direction {
            Direction::Download => (
                Job::Download {
                    remote: join(&self.path, name),
                    local: Path::new("~/Downloads").join(name),
                    replace: false,
                },
                "~/Downloads".into(),
            ),
            Direction::Upload => (
                Job::Upload {
                    local: Path::new("~/Projects/relay").join(name),
                    remote: join(&self.path, name),
                    replace: false,
                },
                self.path.clone(),
            ),
        };
        let id = self.queue(job, direction, name.into(), destination);
        let transfer = self.transfers.iter_mut().find(|t| t.id == id).unwrap();
        transfer.total = Some(total);
        transfer.done = done;
        transfer.files = 1;
        transfer.state = if done == total {
            TransferState::Done
        } else {
            transfer.started = Some(Instant::now() - Duration::from_secs(4));
            TransferState::Running
        };
    }

    /// Shows the browser still connecting, with ssh asking `text`, for rendering checks.
    #[cfg(feature = "ui-check")]
    pub fn demo_prompt(&mut self, text: &str, kind: askpass::Kind) {
        let (reply, _) = oneshot::channel();
        self.status = Status::Connecting;
        self.prompts.push(Prompt {
            id: 1,
            text: text.into(),
            kind,
            answer: String::new(),
            reveal: false,
            reply: Some(reply),
        });
    }

    fn with(
        commands: mpsc::UnboundedSender<Command>,
        events: std_mpsc::Receiver<Event>,
        start: Option<String>,
    ) -> Self {
        Self {
            commands,
            events,
            status: Status::Connecting,
            path: String::new(),
            home: String::new(),
            entries: vec![],
            loading: false,
            busy: 0,
            error: None,
            notice: None,
            prompts: vec![],
            transfers: vec![],
            view: View {
                focus_requested: true,
                ..View::default()
            },
            start,
            seq: 0,
            land_on: None,
            next_id: 1,
            started: Instant::now(),
            #[cfg(any(test, feature = "ui-check"))]
            demo_inbox: None,
        }
    }

    fn launch(launch: Launch, ctx: Option<egui::Context>, start: Option<String>) -> Self {
        let (commands, inbox) = mpsc::unbounded_channel();
        let (sender, events) = std_mpsc::channel();
        let events_out = Events {
            sender,
            ctx: ctx.clone(),
        };
        let spawned = std::thread::Builder::new()
            .name("relay-sftp".into())
            .spawn({
                let events = events_out.clone();
                move || match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(worker(launch, inbox, events)),
                    Err(e) => events.send(Event::Ended(format!("Cannot start SFTP: {e}"))),
                }
            });
        let mut browser = Self::with(commands, events, start);
        if let Err(e) = spawned {
            browser.status = Status::Ended(format!("Cannot start SFTP: {e}"));
        }
        browser
    }

    /// Applies what the worker reported since the last frame.
    pub fn poll(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            self.apply(event);
        }
        if self
            .notice
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(4))
        {
            self.notice = None;
        }
    }

    fn apply(&mut self, event: Event) {
        match event {
            Event::Ask(request) => self.prompts.push(Prompt {
                id: request.id,
                text: request.text,
                kind: request.kind,
                answer: String::new(),
                reveal: false,
                reply: Some(request.reply),
            }),
            Event::Gone(id) => self.prompts.retain(|p| p.id != id),
            Event::Ready { home } => {
                self.status = Status::Ready;
                self.prompts.clear();
                self.home = home.clone();
                let start = self.start.take().unwrap_or(home);
                self.list(start);
            }
            Event::Listing { seq, result } if seq == self.seq => {
                self.loading = false;
                match result {
                    Ok((path, entries)) => {
                        if path != self.path {
                            self.view.reset();
                            self.view.focus_requested = true;
                        }
                        self.path = path;
                        self.entries = entries;
                        let names: BTreeSet<_> =
                            self.entries.iter().map(|e| e.name.clone()).collect();
                        self.view.selected.retain(|n| names.contains(n));
                        if self
                            .view
                            .cursor
                            .as_ref()
                            .is_some_and(|c| !names.contains(c))
                        {
                            self.view.cursor = None;
                        }
                        if let Some(name) = self.land_on.take().filter(|n| names.contains(n)) {
                            self.view.select_only(&name);
                            self.view.scroll_to_cursor = true;
                        }
                    }
                    Err(e) => {
                        // The first folder may be gone after a reconnect; fall back to home.
                        if self.path.is_empty() && !self.home.is_empty() {
                            self.list(self.home.clone());
                        }
                        self.view.back.pop();
                        self.error = Some(e);
                    }
                }
            }
            Event::Listing { .. } => {}
            Event::Changed(result) => {
                self.busy = self.busy.saturating_sub(1);
                match result {
                    Ok(notice) => self.notice = Some((notice, Instant::now())),
                    Err(e) => self.error = Some(e),
                }
                self.refresh();
            }
            Event::Started {
                id,
                total,
                files,
                skipped,
            } => {
                if let Some(t) = self.transfers.iter_mut().find(|t| t.id == id) {
                    t.state = TransferState::Running;
                    t.total = Some(total);
                    t.files = files;
                    t.skipped = skipped;
                    t.started = Some(Instant::now());
                }
            }
            Event::Progress {
                id,
                done,
                files_done,
            } => {
                if let Some(t) = self.transfers.iter_mut().find(|t| t.id == id) {
                    t.done = done;
                    t.files_done = files_done;
                }
            }
            Event::Finished { id, result } => {
                let mut upload = false;
                if let Some(t) = self.transfers.iter_mut().find(|t| t.id == id) {
                    upload = t.direction == Direction::Upload;
                    t.state = match result {
                        Ok(()) => {
                            t.done = t.total.unwrap_or(t.done);
                            t.files_done = t.files;
                            TransferState::Done
                        }
                        Err(failure) => TransferState::Stopped(failure),
                    };
                }
                // Uploads, finished or partly done, change remote folders.
                if upload {
                    self.refresh();
                }
            }
            Event::Ended(reason) => {
                self.status = Status::Ended(reason);
                self.loading = false;
                self.busy = 0;
                self.prompts.clear();
                for t in &mut self.transfers {
                    if !t.finished() {
                        t.state =
                            TransferState::Stopped(Failure::Error("The connection ended.".into()));
                    }
                }
            }
        }
    }

    pub fn ended(&self) -> bool {
        matches!(self.status, Status::Ended(_))
    }

    #[cfg(test)]
    fn ready(&self) -> bool {
        self.status == Status::Ready
    }

    fn send(&mut self, command: Command) {
        if self.commands.send(command).is_err() && !self.ended() {
            self.status = Status::Ended("The connection ended.".into());
        }
    }

    /// Opens folder `path`, which may be relative to the current one.
    pub fn navigate(&mut self, path: &str) {
        let path = path.trim();
        if path.is_empty() {
            return;
        }
        let target = if path.starts_with('/') {
            path.to_string()
        } else if path == "~" {
            self.home.clone()
        } else if let Some(rest) = path.strip_prefix("~/") {
            join(&self.home, rest)
        } else {
            join(&self.path, path)
        };
        if !self.path.is_empty() && target != self.path {
            self.view.back.push(self.path.clone());
        }
        self.list(target);
    }

    fn list(&mut self, path: String) {
        self.seq += 1;
        self.land_on = None;
        self.loading = true;
        let seq = self.seq;
        self.send(Command::List { seq, path });
    }

    pub fn refresh(&mut self) {
        if !self.path.is_empty() {
            self.list(self.path.clone());
        }
    }

    pub fn up(&mut self) {
        if let Some(parent) = parent(&self.path) {
            let from = file_name(&self.path).to_string();
            self.navigate(&parent);
            // Land on the folder just left, as file managers do.
            self.land_on = Some(from);
        }
    }

    pub fn back(&mut self) {
        if let Some(path) = self.view.back.pop() {
            self.list(path);
        }
    }

    pub fn create_folder(&mut self, name: &str) -> Result<(), String> {
        let name = check_name(name)?;
        self.busy += 1;
        let path = join(&self.path, name);
        self.send(Command::Mkdir(path));
        Ok(())
    }

    /// Renames `from` in the current folder to `to`.
    pub fn rename(&mut self, from: &str, to: &str) -> Result<(), String> {
        let to = check_name(to)?;
        if to == from {
            return Ok(());
        }
        if self.entries.iter().any(|e| e.name == to) {
            return Err(format!("“{to}” already exists here."));
        }
        self.busy += 1;
        self.view.select_only(to);
        self.send(Command::Rename {
            from: join(&self.path, from),
            to: join(&self.path, to),
        });
        Ok(())
    }

    /// Moves `names` in the current folder into folder `target`, a full path.
    pub fn move_into(&mut self, names: &[String], target: &str) {
        for name in names {
            let from = join(&self.path, name);
            if from == target || target.starts_with(&format!("{from}/")) {
                self.error = Some(format!("“{name}” cannot be moved into itself."));
                continue;
            }
            self.busy += 1;
            self.send(Command::Rename {
                from,
                to: join(target, name),
            });
        }
    }

    pub fn delete(&mut self, names: &[String]) {
        if names.is_empty() {
            return;
        }
        self.busy += 1;
        let paths = names.iter().map(|n| join(&self.path, n)).collect();
        self.send(Command::Delete(paths));
    }

    /// Names among `local` that already exist in the current folder.
    pub fn conflicts(&self, local: &[PathBuf]) -> Vec<String> {
        local
            .iter()
            .filter_map(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| self.entries.iter().any(|e| &e.name == n))
            .collect()
    }

    /// Uploads local files and folders into remote `folder`, first asking about items that
    /// would replace ones shown in the current folder.
    pub fn offer_upload(&mut self, local: Vec<PathBuf>, folder: &str) {
        if local.is_empty() {
            return;
        }
        if folder == self.path {
            let existing = self.conflicts(&local);
            if !existing.is_empty() {
                self.view.confirm = Some(Confirm::Replace {
                    local,
                    folder: folder.into(),
                    existing,
                });
                return;
            }
        }
        self.upload(local, folder, false);
    }

    /// Queues uploads of local files and folders into remote `folder`.
    pub fn upload(&mut self, local: Vec<PathBuf>, folder: &str, replace: bool) {
        for path in local {
            let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
                continue;
            };
            let job = Job::Upload {
                remote: join(folder, &name),
                local: path,
                replace,
            };
            self.queue(job, Direction::Upload, name, folder.into());
        }
    }

    /// Queues downloads of `names` in the current folder into local `folder`.
    pub fn download(&mut self, names: &[String], folder: &Path) {
        for name in names {
            match local_name(name) {
                Some(local) => {
                    let job = Job::Download {
                        remote: join(&self.path, name),
                        local: folder.join(local),
                        replace: false,
                    };
                    self.queue(
                        job,
                        Direction::Download,
                        name.clone(),
                        folder.display().to_string(),
                    );
                }
                None => {
                    self.error = Some(format!("“{name}” cannot be saved on this computer."));
                }
            }
        }
    }

    /// Queues a download of `name` to exactly `local`, replacing it: the save dialog has
    /// already asked.
    pub fn download_as(&mut self, name: &str, local: PathBuf) {
        let folder = local
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let job = Job::Download {
            remote: join(&self.path, name),
            local,
            replace: true,
        };
        self.queue(job, Direction::Download, name.into(), folder);
    }

    fn queue(&mut self, job: Job, direction: Direction, name: String, destination: String) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let cancel = CancellationToken::new();
        self.transfers.push(Transfer {
            id,
            direction,
            name,
            destination,
            done: 0,
            total: None,
            files_done: 0,
            files: 0,
            state: TransferState::Queued,
            skipped: 0,
            started: None,
            job: job.clone(),
            cancel: cancel.clone(),
        });
        self.send(Command::Transfer(Queued { id, job, cancel }));
        id
    }

    pub fn cancel(&mut self, id: u64) {
        if let Some(t) = self.transfers.iter().find(|t| t.id == id) {
            t.cancel.cancel();
        }
    }

    pub fn cancel_all(&mut self) {
        for t in &self.transfers {
            t.cancel.cancel();
        }
    }

    /// Queues a stopped transfer again, replacing what is at its destination if `replace`.
    pub fn retry(&mut self, id: u64, replace: bool) {
        let Some(index) = self.transfers.iter().position(|t| t.id == id) else {
            return;
        };
        let old = self.transfers.remove(index);
        let job = if replace {
            old.job.replacing()
        } else {
            old.job
        };
        self.queue(job, old.direction, old.name, old.destination);
    }

    pub fn clear_finished(&mut self) {
        self.transfers.retain(|t| !t.finished());
    }

    /// Transfers that are queued or running.
    pub fn active_transfers(&self) -> usize {
        self.transfers.iter().filter(|t| !t.finished()).count()
    }

    /// Answers prompt `id`; `None` declines it.
    pub fn answer(&mut self, id: u64, answer: Option<String>) {
        if let Some(index) = self.prompts.iter().position(|p| p.id == id) {
            let mut prompt = self.prompts.remove(index);
            if let Some(reply) = prompt.reply.take() {
                let _ = reply.send(answer);
            }
        }
    }

    /// Stops connecting, or ends the connection, showing `reason`.
    pub fn disconnect(&mut self, reason: &str) {
        for prompt in std::mem::take(&mut self.prompts) {
            if let Some(reply) = prompt.reply {
                let _ = reply.send(None);
            }
        }
        self.cancel_all();
        let _ = self.commands.send(Command::Close);
        self.apply(Event::Ended(reason.into()));
    }

    /// The current folder's entries as shown: filtered, without hidden ones unless asked,
    /// and sorted with folders first.
    pub fn visible(&self) -> Vec<&Entry> {
        let filter = self.view.filter.trim().to_lowercase();
        let mut items: Vec<_> = self
            .entries
            .iter()
            .filter(|e| self.view.show_hidden || !e.hidden())
            .filter(|e| filter.is_empty() || e.name.to_lowercase().contains(&filter))
            .collect();
        items.sort_by(|a, b| {
            let order = match self.view.sort {
                Sort::Name => natural(&a.name, &b.name),
                Sort::Size => a.size.cmp(&b.size).then_with(|| natural(&a.name, &b.name)),
                Sort::Modified => a
                    .modified
                    .cmp(&b.modified)
                    .then_with(|| natural(&a.name, &b.name)),
            };
            b.dir.cmp(&a.dir).then(if self.view.descending {
                order.reverse()
            } else {
                order
            })
        });
        items
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        // The worker ends, and with it ssh, when it reads this or the channel closes.
        self.cancel_all();
        let _ = self.commands.send(Command::Close);
    }
}

/// The ssh command line for browsing `c`: its usual options, no forwards, and the sftp
/// subsystem instead of a shell.
pub fn ssh_args(c: &Connection) -> Vec<String> {
    let mut args = vec!["-T".to_string()];
    args.extend(c.ssh_args());
    // Forwards belong to the terminal, which may already hold their ports; X11 is not needed,
    // and a configured RemoteCommand would replace the sftp subsystem.
    for option in [
        "ClearAllForwardings=yes",
        "ForwardX11=no",
        "RemoteCommand=none",
    ] {
        args.extend(["-o".into(), option.into()]);
    }
    args.extend(["-s".into(), "--".into(), c.host.clone(), "sftp".into()]);
    args
}

/// Sends events to the browser and wakes the window to show them.
#[derive(Clone)]
struct Events {
    sender: std_mpsc::Sender<Event>,
    ctx: Option<egui::Context>,
}

impl Events {
    fn send(&self, event: Event) {
        let _ = self.sender.send(event);
        if let Some(ctx) = &self.ctx {
            ctx.request_repaint();
        }
    }
}

async fn worker(
    mut launch: Launch,
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: Events,
) {
    if let Some(program) = launch.askpass.take() {
        let listener = match askpass::Listener::bind().await {
            Ok(listener) => listener,
            Err(e) => return events.send(Event::Ended(e)),
        };
        let endpoint = match listener.endpoint() {
            Ok(endpoint) => endpoint,
            Err(e) => return events.send(Event::Ended(e)),
        };
        // OpenSSH asks through the helper even when Relay was started from a terminal.
        launch
            .command
            .env("SSH_ASKPASS", program)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env(askpass::ENV, endpoint);
        let (messages, mut incoming) = mpsc::unbounded_channel();
        tokio::spawn(listener.serve(messages));
        let forward = events.clone();
        tokio::spawn(async move {
            while let Some(message) = incoming.recv().await {
                forward.send(match message {
                    askpass::Message::Ask(request) => Event::Ask(request),
                    askpass::Message::Gone(id) => Event::Gone(id),
                });
            }
        });
    }
    let mut child = match launch
        .command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            return events.send(Event::Ended(format!(
                "Cannot start ssh. Install an OpenSSH client (on Windows, the OpenSSH Client \
                 optional feature): {e}"
            )));
        }
    };
    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return events.send(Event::Ended("Cannot talk to ssh.".into()));
    };
    let log = Arc::new(Mutex::new(VecDeque::new()));
    let reading = tokio::spawn({
        let log = log.clone();
        async move {
            let mut lines = tokio::io::BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let mut log = log.lock().unwrap();
                if log.len() == LOG_LINES {
                    log.pop_front();
                }
                log.push_back(line);
            }
        }
    });
    // Waiting owns the process, so ending the worker ends ssh.
    let (exit_sender, mut exited) = oneshot::channel();
    tokio::spawn(async move {
        let _ = exit_sender.send(child.wait().await);
        drop(child);
    });

    let config = russh_sftp::client::Config {
        request_timeout_secs: CONNECT_TIMEOUT,
        ..Default::default()
    };
    let connecting = SftpSession::new_with_config(tokio::io::join(stdout, stdin), config);
    tokio::pin!(connecting);
    let mut early = vec![];
    let session = loop {
        tokio::select! {
            session = &mut connecting => break session,
            command = commands.recv() => match command {
                None | Some(Command::Close) => return,
                Some(command) => early.push(command),
            },
        }
    };
    let session = match session {
        Ok(session) => Arc::new(session),
        Err(e) => {
            // ssh says what went wrong as it exits.
            let status = tokio::time::timeout(Duration::from_secs(5), &mut exited).await;
            let _ = tokio::time::timeout(Duration::from_secs(1), reading).await;
            let code = status
                .ok()
                .and_then(|s| s.ok()?.ok())
                .and_then(|s| s.code());
            return events.send(Event::Ended(connect_failure(&log.lock().unwrap(), code, e)));
        }
    };
    session.set_timeout(REQUEST_TIMEOUT);
    let home = session
        .canonicalize(".")
        .await
        .unwrap_or_else(|_| "/".into());
    events.send(Event::Ready { home });

    let (queue, queued) = mpsc::unbounded_channel();
    tokio::spawn(transfers(session.clone(), queued, events.clone()));
    let run = |command: Command| match command {
        Command::Transfer(job) => {
            let _ = queue.send(job);
        }
        Command::Close => {}
        command => {
            tokio::spawn(operation(session.clone(), command, events.clone()));
        }
    };
    for command in early {
        run(command);
    }
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                None | Some(Command::Close) => return,
                Some(command) => run(command),
            },
            status = &mut exited => {
                let _ = tokio::time::timeout(Duration::from_secs(1), reading).await;
                let code = status.ok().and_then(|s| s.ok()).and_then(|s| s.code());
                let detail = last_error(&log.lock().unwrap())
                    .or_else(|| code.map(|c| format!("ssh exited with code {c}.")))
                    .unwrap_or_default();
                return events.send(Event::Ended(
                    format!("The connection ended. {detail}").trim_end().to_string(),
                ));
            }
        }
    }
}

/// The last thing ssh said that looks like an error, skipping notices such as host keys
/// being added to known_hosts.
fn last_error(log: &VecDeque<String>) -> Option<String> {
    log.iter()
        .rev()
        .map(|line| line.trim())
        .find(|line| !line.is_empty() && !line.starts_with("Warning: Permanently added"))
        .map(String::from)
}

/// Explains why the SFTP connection did not come up.
fn connect_failure(log: &VecDeque<String>, code: Option<i32>, error: SftpError) -> String {
    let said = log
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    if said.contains("subsystem request failed") {
        return "The server does not offer SFTP. Ask its administrator to enable the sftp \
                subsystem."
            .into();
    }
    if said.contains("Host key verification failed") && said.contains("REMOTE HOST IDENTIFICATION")
    {
        return "The server's host key has changed, so Relay did not connect. If the change \
                is expected, remove the old key from known_hosts (ssh-keygen -R host)."
            .into();
    }
    match (last_error(log), code) {
        (Some(line), _) => format!("Cannot open SFTP: {line}"),
        (None, Some(code)) => format!("Cannot open SFTP: ssh exited with code {code}."),
        (None, None) => format!("Cannot open SFTP: {}", describe(error)),
    }
}

/// One folder operation, reported as a notice or an error.
async fn operation(session: Arc<SftpSession>, command: Command, events: Events) {
    let result = match command {
        Command::List { seq, path } => {
            let result = list(&session, &path).await;
            return events.send(Event::Listing { seq, result });
        }
        Command::Mkdir(path) => {
            let name = file_name(&path);
            match session.create_dir(path.clone()).await {
                Ok(()) => Ok(format!("Created “{name}”")),
                Err(e) => Err(format!("Cannot create “{name}”: {}", describe(e))),
            }
        }
        Command::Rename { from, to } => {
            let (name, moved) = (file_name(&from), parent(&from) != parent(&to));
            match exists(&session, &to).await {
                Ok(true) => Err(format!(
                    "Cannot move “{name}”: “{}” already exists.",
                    file_name(&to)
                )),
                Err(e) => Err(format!("Cannot rename “{name}”: {}", describe(e))),
                Ok(false) => match session.rename(from.clone(), to.clone()).await {
                    Ok(()) if moved => Ok(format!(
                        "Moved “{name}” to {}",
                        parent(&to).unwrap_or_default()
                    )),
                    Ok(()) => Ok(format!("Renamed “{name}” to “{}”", file_name(&to))),
                    Err(e) => Err(format!("Cannot rename “{name}”: {}", describe(e))),
                },
            }
        }
        Command::Delete(paths) => {
            let mut failed = vec![];
            for path in &paths {
                if let Err(e) = remove(&session, path).await {
                    failed.push(format!("“{}”: {}", file_name(path), describe(e)));
                }
            }
            match (failed.len(), paths.len()) {
                (0, 1) => Ok(format!("Deleted “{}”", file_name(&paths[0]))),
                (0, n) => Ok(format!("Deleted {n} items")),
                _ => Err(format!("Cannot delete {}", failed.join("; "))),
            }
        }
        Command::Transfer(_) | Command::Close => return,
    };
    events.send(Event::Changed(result));
}

async fn list(session: &SftpSession, path: &str) -> Result<(String, Vec<Entry>), String> {
    let fail = |e| format!("Cannot open {path}: {}", describe(e));
    let path = session.canonicalize(path).await.map_err(fail)?;
    let items = session.read_dir(path.clone()).await.map_err(fail)?;
    let mut entries = vec![];
    for item in items {
        let meta = item.metadata();
        let name = item.file_name();
        // Links show what they point to, so links to folders open like folders.
        let target = if file_type(&meta) == TYPE_LINK {
            session.metadata(join(&path, &name)).await.ok()
        } else {
            None
        };
        entries.push(Entry::new(name, &meta, target.as_ref()));
    }
    Ok((path, entries))
}

async fn exists(session: &SftpSession, path: &str) -> Result<bool, SftpError> {
    match session.symlink_metadata(path).await {
        Ok(_) => Ok(true),
        Err(SftpError::Status(s)) if s.status_code == StatusCode::NoSuchFile => Ok(false),
        Err(e) => Err(e),
    }
}

/// Deletes a file, a link, or a folder with everything in it.
async fn remove(session: &SftpSession, path: &str) -> Result<(), SftpError> {
    let meta = session.symlink_metadata(path).await?;
    if file_type(&meta) != TYPE_DIR {
        return session.remove_file(path).await;
    }
    // Files go as they are found; folders go last, deepest first.
    let mut folders = vec![path.to_string()];
    let mut next = 0;
    while next < folders.len() {
        let folder = folders[next].clone();
        next += 1;
        for item in session.read_dir(folder.clone()).await? {
            let child = join(&folder, &item.file_name());
            if file_type(&item.metadata()) == TYPE_DIR {
                folders.push(child);
            } else {
                session.remove_file(child).await?;
            }
        }
    }
    for folder in folders.iter().rev() {
        session.remove_dir(folder.clone()).await?;
    }
    Ok(())
}

/// Runs queued transfers one at a time.
async fn transfers(
    session: Arc<SftpSession>,
    mut queued: mpsc::UnboundedReceiver<Queued>,
    events: Events,
) {
    while let Some(Queued { id, job, cancel }) = queued.recv().await {
        let result = if cancel.is_cancelled() {
            Err(Failure::Cancelled)
        } else {
            transfer(&session, id, &job, &cancel, &events).await
        };
        events.send(Event::Finished { id, result });
    }
}

/// The files and folders one transfer creates, found before copying so progress has a total.
struct Plan<F, P> {
    folders: Vec<F>,
    files: Vec<(P, F, u64)>,
    skipped: u64,
}

impl<F, P> Plan<F, P> {
    fn total(&self) -> u64 {
        self.files.iter().map(|(_, _, size)| size).sum()
    }
}

/// Reports a transfer's progress, often enough to look live.
struct Progress<'a> {
    id: u64,
    done: u64,
    files_done: u64,
    sent: Instant,
    events: &'a Events,
}

impl Progress<'_> {
    fn add(&mut self, bytes: u64) {
        self.done += bytes;
        if self.sent.elapsed() >= PROGRESS_INTERVAL {
            self.report();
        }
    }

    fn file_done(&mut self) {
        self.files_done += 1;
        self.report();
    }

    fn report(&mut self) {
        self.sent = Instant::now();
        self.events.send(Event::Progress {
            id: self.id,
            done: self.done,
            files_done: self.files_done,
        });
    }
}

async fn transfer(
    session: &SftpSession,
    id: u64,
    job: &Job,
    cancel: &CancellationToken,
    events: &Events,
) -> Result<(), Failure> {
    let error = |what: &str, e: &dyn std::fmt::Display| Failure::Error(format!("{what}: {e}"));
    let mut progress = Progress {
        id,
        done: 0,
        files_done: 0,
        sent: Instant::now(),
        events,
    };
    match job {
        Job::Upload {
            local,
            remote,
            replace,
        } => {
            let existing = exists(session, remote)
                .await
                .map_err(|e| error("Cannot check the destination", &describe(e)))?;
            if existing && !replace {
                return Err(Failure::Exists(format!(
                    "“{}” already exists on the server.",
                    file_name(remote)
                )));
            }
            let (from, to) = (local.clone(), remote.clone());
            let plan = tokio::task::spawn_blocking(move || plan_upload(&from, &to))
                .await
                .map_err(|e| error("Cannot read the files", &e))?
                .map_err(|e| error(&format!("Cannot read {}", local.display()), &e))?;
            events.send(Event::Started {
                id,
                total: plan.total(),
                files: plan.files.len() as u64,
                skipped: plan.skipped,
            });
            for folder in &plan.folders {
                if !exists(session, folder).await.unwrap_or(false) {
                    session
                        .create_dir(folder.clone())
                        .await
                        .map_err(|e| error(&format!("Cannot create {folder}"), &describe(e)))?;
                }
            }
            for (from, to, _) in &plan.files {
                upload_file(session, from, to, cancel, &mut progress).await?;
                progress.file_done();
            }
        }
        Job::Download {
            remote,
            local,
            replace,
        } => {
            if !replace && std::fs::symlink_metadata(local).is_ok() {
                return Err(Failure::Exists(format!(
                    "“{}” already exists in {}.",
                    local.file_name().unwrap_or_default().to_string_lossy(),
                    local.parent().unwrap_or(local).display()
                )));
            }
            let plan = plan_download(session, remote, local)
                .await
                .map_err(|e| error(&format!("Cannot read {remote}"), &e))?;
            events.send(Event::Started {
                id,
                total: plan.total(),
                files: plan.files.len() as u64,
                skipped: plan.skipped,
            });
            for folder in &plan.folders {
                tokio::fs::create_dir_all(folder)
                    .await
                    .map_err(|e| error(&format!("Cannot create {}", folder.display()), &e))?;
            }
            for (from, to, _) in &plan.files {
                download_file(session, from, to, cancel, &mut progress).await?;
                progress.file_done();
            }
        }
    }
    Ok(())
}

/// Lists a local file or folder for upload. Links to folders are skipped, since they can loop.
fn plan_upload(local: &Path, remote: &str) -> std::io::Result<Plan<String, PathBuf>> {
    let mut plan = Plan {
        folders: vec![],
        files: vec![],
        skipped: 0,
    };
    let meta = std::fs::metadata(local)?;
    if !meta.is_dir() {
        plan.files.push((local.into(), remote.into(), meta.len()));
        return Ok(plan);
    }
    let mut pending = vec![(local.to_path_buf(), remote.to_string())];
    while let Some((folder, target)) = pending.pop() {
        plan.folders.push(target.clone());
        let mut children: Vec<_> = std::fs::read_dir(&folder)?.collect::<Result<_, _>>()?;
        children.sort_by_key(|c| c.file_name());
        for child in children {
            let path = child.path();
            let name = child.file_name().to_string_lossy().into_owned();
            let link = child.file_type()?.is_symlink();
            match std::fs::metadata(&path) {
                Ok(meta) if meta.is_dir() && link => plan.skipped += 1,
                Ok(meta) if meta.is_dir() => pending.push((path, join(&target, &name))),
                Ok(meta) if meta.is_file() => {
                    plan.files.push((path, join(&target, &name), meta.len()))
                }
                // Broken links, sockets, and devices have nothing to copy.
                _ => plan.skipped += 1,
            }
        }
    }
    Ok(plan)
}

/// Lists a remote file or folder for download. Links to folders inside it are skipped.
async fn plan_download(
    session: &SftpSession,
    remote: &str,
    local: &Path,
) -> Result<Plan<PathBuf, String>, String> {
    let mut plan = Plan {
        folders: vec![],
        files: vec![],
        skipped: 0,
    };
    // The item that was chosen is followed if it is a link.
    let meta = session.metadata(remote).await.map_err(describe)?;
    if file_type(&meta) != TYPE_DIR {
        plan.files.push((remote.into(), local.into(), meta.len()));
        return Ok(plan);
    }
    let mut pending = vec![(remote.to_string(), local.to_path_buf())];
    while let Some((folder, target)) = pending.pop() {
        plan.folders.push(target.clone());
        let mut items: Vec<_> = session
            .read_dir(folder.clone())
            .await
            .map_err(describe)?
            .collect();
        items.sort_by_key(|i| i.file_name());
        for item in items {
            let name = item.file_name();
            // A server must not be able to write outside the chosen folder.
            let Some(local_name) = local_name(&name) else {
                return Err(format!("“{name}” cannot be saved on this computer."));
            };
            let path = join(&folder, &name);
            let meta = item.metadata();
            match file_type(&meta) {
                TYPE_DIR => pending.push((path, target.join(local_name))),
                TYPE_FILE => plan.files.push((path, target.join(local_name), meta.len())),
                TYPE_LINK => match session.metadata(path.clone()).await {
                    Ok(target_meta) if file_type(&target_meta) == TYPE_FILE => {
                        plan.files
                            .push((path, target.join(local_name), target_meta.len()))
                    }
                    _ => plan.skipped += 1,
                },
                _ => plan.skipped += 1,
            }
        }
    }
    Ok(plan)
}

async fn upload_file(
    session: &SftpSession,
    local: &Path,
    remote: &str,
    cancel: &CancellationToken,
    progress: &mut Progress<'_>,
) -> Result<(), Failure> {
    let fail = |e: &dyn std::fmt::Display| Failure::Error(format!("{}: {e}", file_name(remote)));
    let mut source = tokio::fs::File::open(local).await.map_err(|e| fail(&e))?;
    // A file being replaced keeps its contents until its replacement is complete.
    if let Ok(meta) = session.metadata(remote).await
        && file_type(&meta) == TYPE_FILE
    {
        // Through a link, the file it points to is replaced and the link stays.
        let real = session
            .canonicalize(remote)
            .await
            .unwrap_or_else(|_| remote.into());
        if let Some((temp, mut target)) = stage(session, &real).await {
            let mut copied = match copy(&mut source, &mut target, cancel, progress).await {
                Ok(()) => target.shutdown().await.map_err(|e| fail(&e)),
                Err(e) => Err(e),
            };
            drop(target);
            if copied.is_ok() {
                let permissions = Metadata {
                    permissions: meta.permissions.map(|p| p & 0o7777),
                    ..Metadata::empty()
                };
                let _ = session.set_metadata(temp.clone(), permissions).await;
                copied = replace(session, &temp, &real).await.map_err(|e| fail(&e));
            } else {
                let _ = session.remove_file(temp).await;
            }
            return copied;
        }
        // A folder that takes no new files still lets its files be overwritten in place.
    }
    let mut target = session
        .open_with_flags(
            remote,
            OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE,
        )
        .await
        .map_err(|e| fail(&describe(e)))?;
    let copied = match copy(&mut source, &mut target, cancel, progress).await {
        Ok(()) => target.shutdown().await.map_err(|e| fail(&e)),
        Err(e) => Err(e),
    };
    if copied.is_err() {
        // Leave no partial file behind.
        drop(target);
        let _ = session.remove_file(remote).await;
    }
    copied
}

/// Creates a new file beside `path` to write its replacement into, or `None` where the
/// folder takes no new files.
async fn stage(session: &SftpSession, path: &str) -> Option<(String, File)> {
    let temp = join(&parent(path)?, &crate::model::temp_name(file_name(path)));
    let file = session
        .open_with_flags(
            temp.clone(),
            OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
        )
        .await
        .ok()?;
    Some((temp, file))
}

/// Moves the complete file `temp` over `path`. SFTP servers commonly refuse to rename onto an
/// existing file, and then `path` is removed first. Once it is, `temp` holds the only copy,
/// so it stays if the move still fails.
async fn replace(session: &SftpSession, temp: &str, path: &str) -> Result<(), String> {
    let removed = match session.rename(temp, path).await {
        Ok(()) => return Ok(()),
        Err(SftpError::Status(s)) if s.status_code == StatusCode::Failure => {
            session.remove_file(path).await
        }
        Err(e) => Err(e),
    };
    if let Err(e) = removed {
        let _ = session.remove_file(temp).await;
        return Err(format!("Cannot replace it: {}", describe(e)));
    }
    session.rename(temp, path).await.map_err(|e| {
        format!(
            "Uploaded as {}, which cannot take its place: {}",
            file_name(temp),
            describe(e)
        )
    })
}

async fn download_file(
    session: &SftpSession,
    remote: &str,
    local: &Path,
    cancel: &CancellationToken,
    progress: &mut Progress<'_>,
) -> Result<(), Failure> {
    let fail = |e: &dyn std::fmt::Display| Failure::Error(format!("{}: {e}", file_name(remote)));
    let mut source = session.open(remote).await.map_err(|e| fail(&describe(e)))?;
    let modified = source.metadata().await.ok().and_then(|m| m.modified().ok());
    // The file appears under its own name only once it is complete.
    let partial = partial_path(local);
    let mut target = tokio::fs::File::create_new(&partial)
        .await
        .map_err(|e| fail(&e))?;
    let mut copied = copy(&mut source, &mut target, cancel, progress).await;
    if copied.is_ok() {
        copied = target.flush().await.map_err(|e| fail(&e));
    }
    let target = target.into_std().await;
    if copied.is_ok() {
        if let Some(modified) = modified {
            let _ = target.set_modified(modified);
        }
        drop(target);
        copied = tokio::fs::rename(&partial, local)
            .await
            .map_err(|e| fail(&e));
    } else {
        drop(target);
    }
    if copied.is_err() {
        let _ = tokio::fs::remove_file(&partial).await;
    }
    copied
}

/// Where a download is written until it is complete: beside its destination.
fn partial_path(local: &Path) -> PathBuf {
    let name = local.file_name().unwrap_or_default().to_string_lossy();
    local.with_file_name(crate::model::temp_name(&name))
}

/// Copies `reader` to `writer` through one buffer, stopping promptly when cancelled.
async fn copy(
    reader: &mut (impl AsyncRead + Unpin),
    writer: &mut (impl AsyncWrite + Unpin),
    cancel: &CancellationToken,
    progress: &mut Progress<'_>,
) -> Result<(), Failure> {
    let mut buffer = vec![0u8; CHUNK];
    loop {
        let read = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(Failure::Cancelled),
            read = reader.read(&mut buffer) => read,
        };
        let n = read.map_err(|e| Failure::Error(format!("Cannot read: {e}")))?;
        if n == 0 {
            return Ok(());
        }
        let written = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(Failure::Cancelled),
            written = writer.write_all(&buffer[..n]) => written,
        };
        written.map_err(|e| Failure::Error(format!("Cannot write: {e}")))?;
        progress.add(n as u64);
    }
}

/// Puts an SFTP error in words.
fn describe(error: SftpError) -> String {
    match error {
        SftpError::Status(status) => {
            let message = status.error_message.trim().trim_end_matches('.');
            match status.status_code {
                StatusCode::NoSuchFile => "No such file or folder.".into(),
                StatusCode::PermissionDenied => "Permission denied.".into(),
                StatusCode::OpUnsupported => "The server does not support this.".into(),
                StatusCode::NoConnection | StatusCode::ConnectionLost => {
                    "The connection was lost.".into()
                }
                _ if !message.is_empty() && !message.eq_ignore_ascii_case("failure") => {
                    format!("{message}.")
                }
                _ => "The server could not do this. The item may already exist, or a folder \
                      may not be empty."
                    .into(),
            }
        }
        SftpError::Timeout => "The server did not answer in time.".into(),
        SftpError::IO(e) => format!("Connection error: {e}"),
        other => other.to_string(),
    }
}

/// Checks a name typed for a new or renamed item.
pub fn check_name(name: &str) -> Result<&str, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Enter a name.".into());
    }
    if name == "." || name == ".." || name.contains('/') || name.contains('\0') {
        return Err("A name cannot be . or .. or contain a slash.".into());
    }
    Ok(name)
}

/// A remote name as one local path component, or `None` if it would be anything else here,
/// such as `..` or, on Windows, a name with a backslash or drive.
pub fn local_name(name: &str) -> Option<&Path> {
    let path = Path::new(name);
    let mut components = path.components();
    match (components.next(), components.next()) {
        (Some(std::path::Component::Normal(part)), None) if part == path.as_os_str() => Some(path),
        _ => None,
    }
}

/// Joins a remote folder and a name with `/`.
pub fn join(folder: &str, name: &str) -> String {
    if folder.is_empty() {
        name.into()
    } else if folder.ends_with('/') {
        format!("{folder}{name}")
    } else {
        format!("{folder}/{name}")
    }
}

/// The folder containing remote `path`, or `None` at the root.
pub fn parent(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.rfind('/') {
        Some(0) => Some("/".into()),
        Some(i) => Some(trimmed[..i].into()),
        None => None,
    }
}

/// The last part of remote `path`.
pub fn file_name(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or(trimmed)
}

/// The folders from the root, or from home as `~`, down to `path`, each with its name, for
/// breadcrumbs.
pub fn crumbs(path: &str, home: &str) -> Vec<(String, String)> {
    let home = home.trim_end_matches('/');
    let (mut crumbs, mut current, rest) = match path.strip_prefix(home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => (
            vec![("~".to_string(), home.to_string())],
            home.to_string(),
            rest,
        ),
        _ => (
            vec![("/".to_string(), "/".to_string())],
            String::new(),
            path,
        ),
    };
    for part in rest.split('/').filter(|p| !p.is_empty()) {
        current = format!("{current}/{part}");
        crumbs.push((part.to_string(), current.clone()));
    }
    crumbs
}

/// Compares names as people expect: ignoring case, with numbers in numeric order.
pub fn natural(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, _) => return std::cmp::Ordering::Less,
            (_, None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let number = |chars: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut digits = String::new();
                    while let Some(c) = chars.peek().filter(|c| c.is_ascii_digit()) {
                        digits.push(*c);
                        chars.next();
                    }
                    digits
                };
                let (x, y) = (number(&mut a), number(&mut b));
                let (tx, ty) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
                let order = tx.len().cmp(&ty.len()).then_with(|| tx.cmp(ty));
                if order.is_ne() {
                    return order;
                }
            }
            (Some(x), Some(y)) => {
                let order = x.to_lowercase().cmp(y.to_lowercase());
                if order.is_ne() {
                    return order;
                }
                a.next();
                b.next();
            }
        }
    }
}

/// A byte count as people read it, such as `4.2 MB`.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.;
    let mut unit = 0;
    while value >= 1024. && unit + 1 < UNITS.len() {
        value /= 1024.;
        unit += 1;
    }
    if value < 10. {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// A modification time in local time, such as `2026-10-27 14:03`.
pub fn modified(seconds: u32) -> String {
    let time = SystemTime::UNIX_EPOCH + Duration::from_secs(seconds.into());
    chrono::DateTime::<chrono::Local>::from(time)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, dir: bool, size: u64, modified: u32) -> Entry {
        Entry {
            name: name.into(),
            dir,
            link: false,
            special: false,
            size,
            modified: Some(modified),
            mode: Some(if dir { 0o040_755 } else { 0o100_644 }),
        }
    }

    #[test]
    fn remote_paths_join_and_split() {
        assert_eq!(join("/", "etc"), "/etc");
        assert_eq!(join("/home/a", "b"), "/home/a/b");
        assert_eq!(parent("/home/a"), Some("/home".into()));
        assert_eq!(parent("/home"), Some("/".into()));
        assert_eq!(parent("/"), None);
        assert_eq!(file_name("/home/a/"), "a");
        assert_eq!(
            crumbs("/srv/www", "/home/deploy"),
            vec![
                ("/".into(), "/".into()),
                ("srv".into(), "/srv".into()),
                ("www".into(), "/srv/www".into())
            ]
        );
        assert_eq!(
            crumbs("/home/deploy/app", "/home/deploy"),
            vec![
                ("~".into(), "/home/deploy".into()),
                ("app".into(), "/home/deploy/app".into())
            ]
        );
        assert_eq!(crumbs("/home/deployer", "/home/deploy")[0].0, "/");
        assert_eq!(crumbs("/", "/"), vec![("/".into(), "/".into())]);
    }

    #[test]
    fn names_are_checked() {
        assert_eq!(check_name("  report.txt "), Ok("report.txt"));
        for bad in ["", " ", ".", "..", "a/b"] {
            assert!(check_name(bad).is_err(), "{bad:?}");
        }
        assert!(local_name("report.txt").is_some());
        for unsafe_name in ["..", ".", "../escape", "a/b", ""] {
            assert!(local_name(unsafe_name).is_none(), "{unsafe_name:?}");
        }
        #[cfg(windows)]
        for unsafe_name in ["..\\escape", "C:evil", "a\\b"] {
            assert!(local_name(unsafe_name).is_none(), "{unsafe_name:?}");
        }
    }

    #[test]
    fn listings_sort_folders_first_in_natural_order() {
        let mut browser = Browser::demo(
            "/srv",
            vec![
                entry("file10.txt", false, 5, 3),
                entry("file2.txt", false, 50, 1),
                entry("Beta", true, 0, 2),
                entry(".hidden", false, 1, 9),
                entry("alpha", true, 0, 5),
            ],
        );
        let names = |b: &Browser| {
            b.visible()
                .iter()
                .map(|e| e.name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(&browser),
            ["alpha", "Beta", "file2.txt", "file10.txt"]
        );
        browser.view.sort = Sort::Size;
        browser.view.descending = true;
        browser.view.show_hidden = true;
        assert_eq!(
            names(&browser),
            ["Beta", "alpha", "file2.txt", "file10.txt", ".hidden"]
        );
        browser.view.sort = Sort::Modified;
        browser.view.descending = false;
        browser.view.filter = "FILE".into();
        assert_eq!(names(&browser), ["file2.txt", "file10.txt"]);
    }

    #[test]
    fn sizes_modes_and_failures_read_well() {
        assert_eq!(size(512), "512 B");
        assert_eq!(size(4_404_019), "4.2 MB");
        assert_eq!(size(300 * 1024 * 1024 * 1024), "300 GB");
        assert_eq!(entry("bin", true, 0, 0).permissions(), "drwxr-xr-x");
        assert_eq!(entry("a", false, 0, 0).permissions(), "-rw-r--r--");
        let socket = Metadata {
            permissions: Some(0o140_755),
            ..Metadata::empty()
        };
        let e = Entry::new("s".into(), &socket, None);
        assert!(!e.dir && e.special, "sockets are not folders");
        let log = VecDeque::from([
            "Warning: Permanently added 'h' (ED25519) to the list of known hosts.".to_string(),
            "deploy@h: Permission denied (publickey,password).".to_string(),
        ]);
        assert_eq!(
            connect_failure(&log, Some(255), SftpError::Timeout),
            "Cannot open SFTP: deploy@h: Permission denied (publickey,password)."
        );
        let log = VecDeque::from(["subsystem request failed on channel 0".to_string()]);
        assert!(connect_failure(&log, Some(255), SftpError::Timeout).contains("does not offer"));
    }

    #[test]
    fn ssh_runs_the_sftp_subsystem_without_forwards() {
        let mut c = Connection::new();
        c.protocol = crate::model::Protocol::Ssh;
        c.host = "build.example".into();
        c.port = 2222;
        c.username = "deploy".into();
        c.ssh.jump = "ops@bastion".into();
        c.ssh.forwards = "L8080:localhost:80".into();
        let args = ssh_args(&c);
        let text = args.join(" ");
        assert!(text.starts_with("-T -p 2222 -l deploy -J ops@bastion"));
        assert!(text.contains("ClearAllForwardings=yes"));
        assert!(text.contains("RemoteCommand=none"));
        assert!(!text.contains("8080"));
        assert!(text.ends_with("-s -- build.example sftp"));
    }

    /// The OpenSSH SFTP server, which tests talk to over a pipe without ssh.
    fn sftp_server() -> Option<PathBuf> {
        [
            "/usr/lib/openssh/sftp-server",
            "/usr/libexec/openssh/sftp-server",
            "/usr/libexec/sftp-server",
            "/usr/lib/ssh/sftp-server",
        ]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
    }

    /// Polls `browser` until `done` holds or a few seconds pass.
    fn wait(browser: &mut Browser, what: &str, done: impl Fn(&Browser) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !done(browser) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
            browser.poll();
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "relay-sftp-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A browser on a local sftp-server, opened on `folder`.
    fn local_browser(folder: &Path) -> Option<Browser> {
        let Some(server) = sftp_server() else {
            eprintln!("skipping: no sftp-server installed");
            return None;
        };
        let mut command = std::process::Command::new(server);
        command.current_dir(folder);
        let mut browser = Browser::local(command);
        wait(&mut browser, "the listing", |b| {
            b.ended() || (b.ready() && !b.loading)
        });
        assert!(browser.ready(), "{:?}", browser.status);
        Some(browser)
    }

    /// The names in local folder `dir`, sorted.
    fn listing(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn names(browser: &Browser) -> Vec<String> {
        browser.visible().iter().map(|e| e.name.clone()).collect()
    }

    #[test]
    fn browses_and_changes_a_real_server() {
        let root = scratch("ops");
        std::fs::create_dir(root.join("docs")).unwrap();
        std::fs::write(root.join("notes.txt"), "hello").unwrap();
        let Some(mut browser) = local_browser(&root) else {
            return;
        };
        let canonical = root.canonicalize().unwrap().display().to_string();
        assert_eq!(browser.path, canonical);
        assert_eq!(names(&browser), ["docs", "notes.txt"]);
        let notes = browser
            .entries
            .iter()
            .find(|e| e.name == "notes.txt")
            .unwrap();
        assert_eq!(notes.size, 5);

        browser.create_folder("new folder").unwrap();
        wait(&mut browser, "the new folder", |b| {
            b.busy == 0 && !b.loading && b.entries.iter().any(|e| e.name == "new folder")
        });
        assert!(root.join("new folder").is_dir());

        browser.rename("notes.txt", "renamed.txt").unwrap();
        wait(&mut browser, "the rename", |b| {
            b.busy == 0 && !b.loading && b.entries.iter().any(|e| e.name == "renamed.txt")
        });
        assert!(
            browser.rename("renamed.txt", "docs").is_err(),
            "names clash"
        );

        browser.move_into(&["renamed.txt".into()], &join(&canonical, "docs"));
        wait(&mut browser, "the move", |b| b.busy == 0 && !b.loading);
        assert!(
            root.join("docs/renamed.txt").is_file(),
            "{:?}",
            browser.error
        );

        browser.navigate("docs");
        wait(&mut browser, "the subfolder", |b| !b.loading);
        assert_eq!(names(&browser), ["renamed.txt"]);
        browser.up();
        wait(&mut browser, "the parent", |b| !b.loading);
        assert_eq!(browser.path, canonical);
        assert_eq!(browser.view.cursor.as_deref(), Some("docs"));

        browser.delete(&["docs".into(), "new folder".into()]);
        wait(&mut browser, "the deletion", |b| b.busy == 0 && !b.loading);
        assert!(browser.error.is_none(), "{:?}", browser.error);
        assert!(names(&browser).is_empty());
        assert!(!root.join("docs").exists());

        browser.navigate("/definitely/not/here");
        wait(&mut browser, "the failed listing", |b| !b.loading);
        assert!(browser.error.as_deref().unwrap().contains("No such file"));
        assert_eq!(browser.path, canonical, "a failed listing keeps the folder");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn transfers_stream_both_ways_and_refuse_to_overwrite() {
        let root = scratch("transfers");
        let remote = root.join("remote");
        let local = root.join("local");
        std::fs::create_dir_all(remote.join("tree/inner")).unwrap();
        std::fs::create_dir_all(&local).unwrap();
        // Larger than the buffer, and not a multiple of it.
        let big: Vec<u8> = (0..(3 * CHUNK + 1234))
            .map(|i| (i * 7 % 251) as u8)
            .collect();
        std::fs::write(remote.join("big.bin"), &big).unwrap();
        std::fs::write(remote.join("tree/inner/leaf.txt"), "leaf").unwrap();
        let Some(mut browser) = local_browser(&remote) else {
            return;
        };

        browser.download(&["big.bin".into(), "tree".into()], &local);
        wait(&mut browser, "the downloads", |b| b.active_transfers() == 0);
        for t in &browser.transfers {
            assert_eq!(t.state, TransferState::Done, "{}", t.name);
        }
        assert_eq!(std::fs::read(local.join("big.bin")).unwrap(), big);
        assert_eq!(
            std::fs::read_to_string(local.join("tree/inner/leaf.txt")).unwrap(),
            "leaf"
        );
        assert_eq!(listing(&local), ["big.bin", "tree"]);

        // Downloading again stops at the existing file until asked to replace it.
        browser.clear_finished();
        browser.download(&["big.bin".into()], &local);
        wait(&mut browser, "the refusal", |b| b.active_transfers() == 0);
        assert!(matches!(
            browser.transfers[0].state,
            TransferState::Stopped(Failure::Exists(_))
        ));
        let id = browser.transfers[0].id;
        browser.retry(id, true);
        wait(&mut browser, "the replacement", |b| {
            b.active_transfers() == 0
        });
        assert_eq!(browser.transfers[0].state, TransferState::Done);

        // Uploads land in the folder and refresh it.
        std::fs::write(local.join("up.txt"), "up").unwrap();
        let folder = browser.path.clone();
        assert_eq!(browser.conflicts(&[local.join("big.bin")]), ["big.bin"]);
        browser.upload(
            vec![local.join("up.txt"), local.join("tree")],
            &folder,
            false,
        );
        wait(&mut browser, "the uploads", |b| {
            b.active_transfers() == 0 && !b.loading
        });
        assert!(names(&browser).contains(&"up.txt".to_string()));
        assert!(matches!(
            browser.transfers.last().unwrap().state,
            TransferState::Stopped(Failure::Exists(_))
        ));
        assert_eq!(
            std::fs::read_to_string(remote.join("up.txt")).unwrap(),
            "up"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cancelled_transfers_leave_nothing_behind() {
        let root = scratch("cancel");
        let local = root.join("local");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::write(root.join("huge.bin"), vec![1u8; 64 * 1024 * 1024]).unwrap();
        let Some(mut browser) = local_browser(&root) else {
            return;
        };
        browser.download(&["huge.bin".into()], &local);
        let id = browser.transfers[0].id;
        wait(&mut browser, "the transfer to start", |b| {
            b.transfers[0].done > 0 || b.transfers[0].finished()
        });
        browser.cancel(id);
        wait(&mut browser, "the cancellation", |b| {
            b.active_transfers() == 0
        });
        // A fast disk may finish first; otherwise nothing partial may remain.
        if browser.transfers[0].state != TransferState::Done {
            assert_eq!(
                browser.transfers[0].state,
                TransferState::Stopped(Failure::Cancelled)
            );
            assert!(listing(&local).is_empty());
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn replacing_keeps_the_original_until_the_upload_is_complete() {
        let root = scratch("replace");
        let remote = root.join("remote");
        let local = root.join("local");
        std::fs::create_dir_all(&remote).unwrap();
        std::fs::create_dir_all(&local).unwrap();
        std::fs::write(remote.join("huge.bin"), "original").unwrap();
        // Not Relay's, whatever its name.
        std::fs::write(remote.join("huge.bin.relay-part"), "unrelated").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::Permissions::from_mode(0o640);
            std::fs::set_permissions(remote.join("huge.bin"), mode).unwrap();
        }
        std::fs::write(local.join("huge.bin"), vec![1u8; 64 * 1024 * 1024]).unwrap();
        let Some(mut browser) = local_browser(&remote) else {
            return;
        };
        let folder = browser.path.clone();

        browser.upload(vec![local.join("huge.bin")], &folder, true);
        let id = browser.transfers[0].id;
        wait(&mut browser, "the transfer to start", |b| {
            b.transfers[0].done > 0 || b.transfers[0].finished()
        });
        browser.cancel(id);
        wait(&mut browser, "the cancellation", |b| {
            b.active_transfers() == 0
        });
        // A fast disk may finish first; otherwise the original is untouched.
        if browser.transfers[0].state != TransferState::Done {
            assert_eq!(
                browser.transfers[0].state,
                TransferState::Stopped(Failure::Cancelled)
            );
            assert_eq!(
                std::fs::read_to_string(remote.join("huge.bin")).unwrap(),
                "original"
            );
        }
        assert_eq!(listing(&remote), ["huge.bin", "huge.bin.relay-part"]);

        // A complete upload takes the original's place and permissions.
        std::fs::write(local.join("huge.bin"), "new").unwrap();
        browser.clear_finished();
        browser.upload(vec![local.join("huge.bin")], &folder, true);
        wait(&mut browser, "the replacement", |b| {
            b.active_transfers() == 0
        });
        assert_eq!(browser.transfers[0].state, TransferState::Done);
        assert_eq!(
            std::fs::read_to_string(remote.join("huge.bin")).unwrap(),
            "new"
        );
        assert_eq!(listing(&remote), ["huge.bin", "huge.bin.relay-part"]);
        assert_eq!(
            std::fs::read_to_string(remote.join("huge.bin.relay-part")).unwrap(),
            "unrelated"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt, symlink};
            let meta = std::fs::metadata(remote.join("huge.bin")).unwrap();
            assert_eq!(meta.permissions().mode() & 0o7777, 0o640);

            // Through a link, the file it points to is replaced and the link stays.
            symlink("huge.bin", remote.join("link.bin")).unwrap();
            std::fs::write(local.join("link.bin"), "via link").unwrap();
            browser.clear_finished();
            browser.upload(vec![local.join("link.bin")], &folder, true);
            wait(&mut browser, "the replacement through a link", |b| {
                b.active_transfers() == 0
            });
            assert_eq!(browser.transfers[0].state, TransferState::Done);
            let link = std::fs::symlink_metadata(remote.join("link.bin")).unwrap();
            assert!(link.file_type().is_symlink());
            assert_eq!(
                std::fs::read_to_string(remote.join("huge.bin")).unwrap(),
                "via link"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_server_that_exits_ends_the_browser_with_its_reason() {
        let mut command = std::process::Command::new(if cfg!(windows) { "cmd" } else { "sh" });
        if cfg!(windows) {
            command.args([
                "/C",
                "echo ssh: Could not resolve hostname nowhere 1>&2 & exit 255",
            ]);
        } else {
            command.args([
                "-c",
                "echo 'ssh: Could not resolve hostname nowhere' >&2; exit 255",
            ]);
        }
        let mut browser = Browser::local(command);
        wait(&mut browser, "the failure", Browser::ended);
        let Status::Ended(reason) = &browser.status else {
            unreachable!()
        };
        assert_eq!(
            reason,
            "Cannot open SFTP: ssh: Could not resolve hostname nowhere"
        );
    }

    /// Signs in to a real server with a password through ssh and Relay's askpass helper,
    /// accepting its host key, then transfers a file both ways. Needs `cargo build` first,
    /// and `RELAY_TEST_SFTP=user@host:port` and `RELAY_TEST_SFTP_PASSWORD`.
    #[test]
    #[ignore = "needs an SSH server"]
    fn sftp_live() {
        let target = std::env::var("RELAY_TEST_SFTP").expect("RELAY_TEST_SFTP");
        let password = std::env::var("RELAY_TEST_SFTP_PASSWORD").expect("RELAY_TEST_SFTP_PASSWORD");
        let c = Connection::quick(&format!("ssh://{target}")).expect("user@host:port");
        let root = scratch("live");
        // A fresh known_hosts makes the host key prompt appear; the user's config is ignored.
        let known = root.join("known_hosts");
        let mut command = std::process::Command::new(crate::platform::ssh_program());
        command.args(["-F", "none", "-o"]);
        command.arg(format!("UserKnownHostsFile={}", known.display()));
        command.args([
            "-o",
            "StrictHostKeyChecking=ask",
            "-o",
            "PubkeyAuthentication=no",
        ]);
        command.args(ssh_args(&c));
        // The test binary lives in target/<profile>/deps; Relay itself one folder up.
        let exe = std::env::current_exe().unwrap();
        let relay = exe
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .join(if cfg!(windows) { "relay.exe" } else { "relay" });
        assert!(relay.is_file(), "run cargo build first");
        let mut browser = Browser::launch_for_test(command, relay);
        wait(&mut browser, "the host key prompt", |b| {
            !b.prompts.is_empty() || b.ended()
        });
        assert_eq!(
            browser.prompts[0].kind,
            askpass::Kind::YesNo,
            "{}",
            browser.prompts[0].text
        );
        let id = browser.prompts[0].id;
        browser.answer(id, Some("yes".into()));
        wait(&mut browser, "the password prompt", |b| {
            !b.prompts.is_empty() || b.ended()
        });
        assert_eq!(
            browser.prompts[0].kind,
            askpass::Kind::Secret,
            "{}",
            browser.prompts[0].text
        );
        let id = browser.prompts[0].id;
        browser.answer(id, Some(password));
        wait(&mut browser, "the listing", |b| {
            b.ended() || (b.ready() && !b.loading)
        });
        assert!(browser.ready(), "{:?}", browser.status);
        assert!(std::fs::read_to_string(&known).unwrap().contains(&c.host));

        let local = root.join("hello.txt");
        std::fs::write(&local, "hello over ssh").unwrap();
        let folder = browser.path.clone();
        browser.upload(vec![local.clone()], &folder, true);
        wait(&mut browser, "the upload", |b| {
            b.active_transfers() == 0 && !b.loading
        });
        assert!(names(&browser).contains(&"hello.txt".to_string()));
        std::fs::remove_file(&local).unwrap();
        browser.download(&["hello.txt".into()], &root);
        wait(&mut browser, "the download", |b| b.active_transfers() == 0);
        assert_eq!(std::fs::read_to_string(&local).unwrap(), "hello over ssh");
        browser.delete(&["hello.txt".into()]);
        wait(&mut browser, "the deletion", |b| b.busy == 0 && !b.loading);
        assert!(!names(&browser).contains(&"hello.txt".to_string()));
        let _ = std::fs::remove_dir_all(&root);
    }
}
