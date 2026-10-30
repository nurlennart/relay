//! File browsing over SFTP. The system's OpenSSH client runs the sftp subsystem
//! (`ssh -T -s host sftp`), so SSH config, keys, agents, jump hosts and known_hosts apply
//! exactly as in the terminal, and russh-sftp speaks the protocol over ssh's standard input
//! and output. Prompts reach the window through [`askpass`].
//!
//! A worker thread owns the connection. The browser sends it commands and reads its events
//! each frame; transfers stream through a fixed buffer and run one at a time in a queue.
//! Files edited in an external editor go through the same queue (see [`edit`]).
mod edit;
mod transfer;

pub use edit::{Edit, EditState};

use crate::{askpass, model::Connection};
use eframe::egui;
use futures_util::stream::{self, StreamExt};
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
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc as std_mpsc,
    },
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
/// How many links a listing resolves at once.
const LOOKUPS: usize = 16;
/// How much of a file is in memory at once while it is copied.
const CHUNK: usize = 256 * 1024;
/// How often a running transfer reports progress.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// Lines of ssh's error output kept to explain a failed connection.
const LOG_LINES: usize = 20;
/// How many transfers that went through, or were cancelled, the queue keeps showing. Older
/// ones go; running and failed ones always stay.
const KEEP_FINISHED: usize = 100;
/// How often copies open in an editor are checked for saves.
const WATCH_INTERVAL: Duration = Duration::from_millis(500);

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

/// A remote file's size and modification time, which tell whether someone changed it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stamp {
    size: u64,
    /// Seconds since 1970.
    modified: Option<u32>,
}

/// A remote file as Relay last read or wrote it. Size and time alone miss a save of the same
/// size within the same second, or one that keeps the time, so the contents count too.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Version {
    stamp: Stamp,
    /// A fingerprint of the contents, taken as they were copied.
    contents: u64,
}

impl Stamp {
    fn of(meta: &Metadata) -> Self {
        Self {
            size: meta.len(),
            modified: meta.mtime,
        }
    }
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
        /// Upload only if the remote file is still this version, as an edited file is when
        /// nobody else saved it in the meantime.
        unchanged: Option<Version>,
        /// Where the folder takes no new files, write over an existing file directly, which
        /// an interrupted upload leaves half written. Only when asked after such a refusal;
        /// edited files are never written so.
        in_place: bool,
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
            Self::Upload {
                replace, unchanged, ..
            } => {
                *replace = true;
                *unchanged = None;
            }
            Self::Download { replace, .. } => *replace = true,
        }
        job
    }

    /// The job again, writing over the file in place where its folder takes no new files.
    fn in_place(&self) -> Self {
        let mut job = self.replacing();
        if let Self::Upload { in_place, .. } = &mut job {
            *in_place = true;
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
    /// The remote file changed since it was last read, so an edit would overwrite someone
    /// else's.
    Changed(String),
    /// The folder takes no new files, so the file could only be written over in place.
    InPlace(String),
    Error(String),
}

impl Failure {
    pub fn message(&self) -> &str {
        match self {
            Self::Cancelled => "Cancelled.",
            Self::Exists(m) | Self::Changed(m) | Self::InPlace(m) | Self::Error(m) => m,
        }
    }
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
    /// The edited file it opens or saves, by [`Edit::id`].
    pub edit: Option<u64>,
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
        match (&self.state, self.total) {
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

/// The current folder's entries as shown: their positions in `entries`, and their names.
pub struct Listing {
    pub order: Vec<usize>,
    pub names: Vec<String>,
}

/// What the shown order is worked out from.
#[derive(PartialEq)]
struct ListingKey {
    listed: u64,
    sort: Sort,
    descending: bool,
    filter: String,
    show_hidden: bool,
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
    /// Stop editing a file whose latest changes are not on the server, by [`Edit::id`].
    StopEditing(u64),
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

/// A local file's size and modification time, which change when it is saved.
type Seen = (u64, Option<SystemTime>);

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
    /// A transfer ended. A single file's transfer says which version the server has now.
    Finished {
        id: u64,
        result: Result<Option<Version>, Failure>,
    },
    /// The copy of edit `id` was saved.
    Saved {
        id: u64,
        seen: Seen,
        contents: u64,
    },
    /// Saves are checked by polling, as file notifications are unavailable.
    WatchFallback(String),
    EditStopChecked {
        id: u64,
        clean: bool,
        cancel: CancellationToken,
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
    /// Where the watcher of edited files reports, beside the worker.
    events_out: Events,
    /// Watches the copies of edited files, once there are any.
    watcher: Option<std_mpsc::Sender<edit::Watch>>,
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
    /// Files open in an external editor.
    pub edits: Vec<Edit>,
    pub view: View,
    /// Counts the listings received, so the shown order knows when to be worked out again.
    listed: u64,
    /// The shown order and names, and what they were worked out from.
    listing: Option<(ListingKey, Arc<Listing>)>,
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
        let (sender, events) = std_mpsc::channel();
        let events_out = Events {
            sender,
            ctx: None,
            shown: Default::default(),
            progress: Default::default(),
        };
        let mut browser = Self::with(commands, events, events_out, None);
        browser.demo_inbox = Some(inbox);
        browser.status = Status::Ready;
        browser.path = path.into();
        browser.home = path.into();
        browser.set_entries(entries);
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
                    unchanged: None,
                    in_place: false,
                },
                self.path.clone(),
            ),
        };
        let id = self.queue(job, direction, name.into(), destination, None);
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
        events_out: Events,
        start: Option<String>,
    ) -> Self {
        Self {
            commands,
            events,
            events_out,
            watcher: None,
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
            edits: vec![],
            view: View {
                focus_requested: true,
                ..View::default()
            },
            listed: 0,
            listing: None,
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
            shown: Arc::new(AtomicBool::new(true)),
            progress: Default::default(),
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
        let mut browser = Self::with(commands, events, events_out, start);
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
        let progress = std::mem::take(&mut *self.events_out.progress.lock().unwrap());
        for (id, (done, files_done)) in progress {
            self.apply(Event::Progress {
                id,
                done,
                files_done,
            });
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
                        self.set_entries(entries);
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
                let mut finished = None;
                if let Some(t) = self.transfers.iter_mut().find(|t| t.id == id) {
                    t.state = match &result {
                        Ok(_) => {
                            t.done = t.total.unwrap_or(t.done);
                            t.files_done = t.files;
                            TransferState::Done
                        }
                        Err(failure) => TransferState::Stopped(failure.clone()),
                    };
                    finished = Some((t.direction, t.edit));
                }
                if let Some((_, Some(edit))) = finished {
                    self.edit_finished(edit, result);
                }
                // Uploads, finished or partly done, change remote folders.
                if let Some((Direction::Upload, _)) = finished {
                    self.refresh();
                }
                self.trim_transfers();
            }
            Event::Saved { id, seen, contents } => self.edit_saved(id, seen, contents),
            Event::WatchFallback(notice) => self.notice = Some((notice, Instant::now())),
            Event::EditStopChecked { id, clean, cancel } => {
                if !cancel.is_cancelled() {
                    self.edit_stop_checked(id, clean);
                }
            }
            Event::Ended(reason) => {
                self.events_out.progress.lock().unwrap().clear();
                self.status = Status::Ended(reason);
                self.loading = false;
                self.busy = 0;
                self.prompts.clear();
                let ended = Failure::Error("The connection ended.".into());
                for t in &mut self.transfers {
                    if !t.finished() {
                        t.state = TransferState::Stopped(ended.clone());
                        if let Some(edit) = t.edit {
                            edit::interrupted(&mut self.edits, edit, &ended);
                        }
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
            let Some(name) = path.file_name() else {
                continue;
            };
            let Some(name) = name.to_str().map(String::from) else {
                self.error = Some(format!(
                    "“{}” has a name that is not valid text, so the server cannot be sent it \
                     unchanged.",
                    name.to_string_lossy()
                ));
                continue;
            };
            let job = Job::Upload {
                remote: join(folder, &name),
                local: path,
                replace,
                unchanged: None,
                in_place: false,
            };
            self.queue(job, Direction::Upload, name, folder.into(), None);
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
                        None,
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
        self.queue(job, Direction::Download, name.into(), folder, None);
    }

    fn queue(
        &mut self,
        job: Job,
        direction: Direction,
        name: String,
        destination: String,
        edit: Option<u64>,
    ) -> u64 {
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
            edit,
            started: None,
            job: job.clone(),
            cancel: cancel.clone(),
        });
        self.send(Command::Transfer(Queued { id, job, cancel }));
        id
    }

    /// Tells the browser whether it is on screen, so progress in a hidden one does not wake
    /// the window.
    pub fn set_shown(&self, shown: bool) {
        self.events_out.shown.store(shown, Ordering::Relaxed);
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
        // An edited file's transfer is retried as the edit stands now.
        if let Some(edit) = self.transfers[index].edit {
            return self.retry_edit(edit, replace);
        }
        let old = self.transfers.remove(index);
        let job = if replace {
            old.job.replacing()
        } else {
            old.job
        };
        self.queue(job, old.direction, old.name, old.destination, None);
    }

    /// Queues a stopped upload again, writing over the file in place where its folder takes
    /// no new files. An edited file is never written so.
    pub fn retry_in_place(&mut self, id: u64) {
        let Some(index) = self
            .transfers
            .iter()
            .position(|t| t.id == id && t.edit.is_none())
        else {
            return;
        };
        let old = self.transfers.remove(index);
        let job = old.job.in_place();
        self.queue(job, old.direction, old.name, old.destination, None);
    }

    /// Lets the oldest transfers that went through or were cancelled go, past
    /// [`KEEP_FINISHED`] of them.
    fn trim_transfers(&mut self) {
        let settled = |t: &Transfer| {
            matches!(
                t.state,
                TransferState::Done | TransferState::Stopped(Failure::Cancelled)
            )
        };
        let mut excess =
            (self.transfers.iter().filter(|t| settled(t)).count()).saturating_sub(KEEP_FINISHED);
        self.transfers.retain(|t| {
            let drop = excess > 0 && settled(t);
            excess -= usize::from(drop);
            !drop
        });
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

    /// The current folder's entries as shown, from [`order`](Self::order).
    #[cfg(test)]
    pub fn visible(&self) -> Vec<&Entry> {
        self.order().into_iter().map(|i| &self.entries[i]).collect()
    }

    /// Replaces the folder's entries, which the shown order then follows.
    fn set_entries(&mut self, entries: Vec<Entry>) {
        self.entries = entries;
        self.listed += 1;
    }

    /// The current folder's entries as shown, worked out again only when the listing or how
    /// it is shown changes rather than every frame, which matters in folders of thousands.
    pub fn listing(&mut self) -> Arc<Listing> {
        let key = ListingKey {
            listed: self.listed,
            sort: self.view.sort,
            descending: self.view.descending,
            filter: self.view.filter.clone(),
            show_hidden: self.view.show_hidden,
        };
        if let Some((cached, listing)) = &self.listing
            && *cached == key
        {
            return listing.clone();
        }
        let order = self.order();
        let names = order
            .iter()
            .map(|&i| self.entries[i].name.clone())
            .collect();
        let listing = Arc::new(Listing { order, names });
        self.listing = Some((key, listing.clone()));
        listing
    }

    /// The positions in `entries` of the current folder's entries as shown: filtered, without
    /// hidden ones unless asked, and sorted with folders first.
    pub fn order(&self) -> Vec<usize> {
        let filter = self.view.filter.trim().to_lowercase();
        let mut items: Vec<_> = (0..self.entries.len())
            .filter(|&i| {
                let e = &self.entries[i];
                (self.view.show_hidden || !e.hidden())
                    && (filter.is_empty() || e.name.to_lowercase().contains(&filter))
            })
            .collect();
        items.sort_by(|&a, &b| {
            let (a, b) = (&self.entries[a], &self.entries[b]);
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
        self.finish_edits();
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
    /// Whether the browser is on screen. Progress in a hidden one does not wake the window;
    /// everything else, such as a transfer ending or a prompt, still does.
    shown: Arc<AtomicBool>,
    /// Only the latest counters per transfer are kept while the UI sleeps.
    progress: Arc<Mutex<std::collections::BTreeMap<u64, (u64, u64)>>>,
}

impl Events {
    fn send(&self, event: Event) {
        let progress = matches!(event, Event::Started { .. } | Event::Progress { .. });
        if let Event::Progress {
            id,
            done,
            files_done,
        } = event
        {
            self.progress.lock().unwrap().insert(id, (done, files_done));
        } else {
            if let Event::Finished { id, .. } = &event
                && let Some((done, files_done)) = self.progress.lock().unwrap().remove(id)
            {
                // Preserve the last counters before success, cancellation or failure,
                // and prevent an older cached counter from following completion.
                let _ = self.sender.send(Event::Progress {
                    id: *id,
                    done,
                    files_done,
                });
            }
            if matches!(event, Event::Ended(_)) {
                self.progress.lock().unwrap().clear();
            }
            let _ = self.sender.send(event);
        }
        if let Some(ctx) = &self.ctx
            && (!progress || self.shown.load(Ordering::Relaxed))
        {
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
    tokio::spawn(transfer::transfers(session.clone(), queued, events.clone()));
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
    // Links show what they point to, so links to folders open like folders. Their targets are
    // looked up several at a time, as one by one a folder of links takes a round trip each.
    let folder = &path;
    let entries = stream::iter(items)
        .map(|item| async move {
            let meta = item.metadata();
            let name = item.file_name();
            let target = if file_type(&meta) == TYPE_LINK {
                session.metadata(join(folder, &name)).await.ok()
            } else {
                None
            };
            Entry::new(name, &meta, target.as_ref())
        })
        .buffered(LOOKUPS)
        .collect()
        .await;
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

/// Compares names as people expect: ignoring case, with numbers in numeric order. It reads
/// the names in place, so sorting thousands allocates nothing.
pub fn natural(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut a, mut b) = (a, b);
    loop {
        match (a.chars().next(), b.chars().next()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, _) => return std::cmp::Ordering::Less,
            (_, None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let digits = |s: &str| s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
                let (end_a, end_b) = (digits(a), digits(b));
                let (x, y) = (
                    a[..end_a].trim_start_matches('0'),
                    b[..end_b].trim_start_matches('0'),
                );
                let order = x.len().cmp(&y.len()).then_with(|| x.cmp(y));
                if order.is_ne() {
                    return order;
                }
                (a, b) = (&a[end_a..], &b[end_b..]);
            }
            (Some(x), Some(y)) => {
                let order = x.to_lowercase().cmp(y.to_lowercase());
                if order.is_ne() {
                    return order;
                }
                (a, b) = (&a[x.len_utf8()..], &b[y.len_utf8()..]);
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
        // Through the kept listing, so each change to how it is shown must be seen.
        let names = |b: &mut Browser| b.listing().names.clone();
        assert_eq!(
            names(&mut browser),
            ["alpha", "Beta", "file2.txt", "file10.txt"]
        );
        browser.view.sort = Sort::Size;
        browser.view.descending = true;
        browser.view.show_hidden = true;
        assert_eq!(
            names(&mut browser),
            ["Beta", "alpha", "file2.txt", "file10.txt", ".hidden"]
        );
        browser.view.sort = Sort::Modified;
        browser.view.descending = false;
        browser.view.filter = "FILE".into();
        assert_eq!(names(&mut browser), ["file2.txt", "file10.txt"]);
        assert_eq!(
            browser.listing().names,
            browser
                .visible()
                .iter()
                .map(|e| e.name.clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_shown_listing_is_kept_until_the_folder_or_its_view_changes() {
        let mut browser = Browser::demo("/srv", vec![entry("b", false, 1, 1)]);
        let first = browser.listing();
        assert!(
            Arc::ptr_eq(&first, &browser.listing()),
            "sorted again unchanged"
        );
        // A new listing of the same folder, as a refresh brings.
        browser.set_entries(vec![entry("a", false, 1, 1), entry("b", false, 1, 1)]);
        let refreshed = browser.listing();
        assert!(!Arc::ptr_eq(&first, &refreshed));
        assert_eq!(refreshed.names, ["a", "b"]);
        assert_eq!(refreshed.order, [0, 1]);
    }

    #[test]
    fn names_compare_naturally_without_regard_to_case() {
        use std::cmp::Ordering::*;
        for (a, b, expected) in [
            ("file2", "file10", Less),
            ("file02", "file2", Equal),
            ("file007", "file10", Less),
            ("Alpha", "alpha", Equal),
            ("alpha", "Beta", Less),
            ("v1.10.2", "v1.9.9", Greater),
            ("x", "x1", Less),
            ("Äpfel2", "äpfel10", Less),
            ("10", "9", Greater),
            ("", "a", Less),
        ] {
            assert_eq!(natural(a, b), expected, "{a} vs {b}");
            assert_eq!(natural(b, a), expected.reverse(), "{b} vs {a}");
        }
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

    #[cfg(unix)]
    #[test]
    fn listings_resolve_every_link() {
        use std::os::unix::fs::symlink;
        let root = scratch("links");
        std::fs::create_dir(root.join("folder")).unwrap();
        std::fs::write(root.join("file.txt"), "12345").unwrap();
        // More links than are resolved at once.
        for i in 0..LOOKUPS * 2 {
            let target = if i % 2 == 0 { "folder" } else { "file.txt" };
            symlink(target, root.join(format!("link-{i:02}"))).unwrap();
        }
        symlink("missing", root.join("broken")).unwrap();
        let Some(browser) = local_browser(&root) else {
            return;
        };
        let entry = |name: &str| browser.entries.iter().find(|e| e.name == name).unwrap();
        assert_eq!(browser.entries.len(), LOOKUPS * 2 + 3);
        for i in 0..LOOKUPS * 2 {
            let link = entry(&format!("link-{i:02}"));
            assert!(link.link);
            assert_eq!(link.dir, i % 2 == 0, "{}", link.name);
            if i % 2 == 1 {
                assert_eq!(link.size, 5, "a link to a file shows its size");
            }
        }
        assert!(entry("broken").link && !entry("broken").dir);
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

        #[cfg(unix)]
        {
            use std::os::unix::{ffi::OsStrExt, fs::symlink};
            // A link inside an existing download leads nowhere else, even when replacing.
            let elsewhere = root.join("elsewhere");
            std::fs::create_dir_all(&elsewhere).unwrap();
            std::fs::write(elsewhere.join("leaf.txt"), "victim").unwrap();
            std::fs::remove_dir_all(local.join("tree/inner")).unwrap();
            symlink(&elsewhere, local.join("tree/inner")).unwrap();
            browser.clear_finished();
            browser.download(&["tree".into()], &local);
            wait(&mut browser, "the refusal", |b| b.active_transfers() == 0);
            let id = browser.transfers[0].id;
            browser.retry(id, true);
            wait(&mut browser, "the link", |b| b.active_transfers() == 0);
            let state = &browser.transfers[0].state;
            assert!(
                matches!(state, TransferState::Stopped(Failure::Error(m)) if m.contains("link")),
                "{state:?}"
            );
            assert_eq!(
                std::fs::read_to_string(elsewhere.join("leaf.txt")).unwrap(),
                "victim"
            );

            // Names that are not text are not changed into others, which could merge them.
            let odd = local.join("odd");
            std::fs::create_dir_all(&odd).unwrap();
            // File systems such as macOS's refuse such names, so there is nothing to merge.
            let made = [0xff, 0xfe].into_iter().all(|byte| {
                let name = [b'x', byte];
                std::fs::write(odd.join(std::ffi::OsStr::from_bytes(&name)), [byte]).is_ok()
            });
            if !made {
                eprintln!("skipping names that are not text: this file system refuses them");
                let _ = std::fs::remove_dir_all(&root);
                return;
            }
            browser.clear_finished();
            browser.upload(vec![odd], &folder, false);
            wait(&mut browser, "the odd names", |b| b.active_transfers() == 0);
            let state = &browser.transfers[0].state;
            assert!(
                matches!(state, TransferState::Stopped(Failure::Error(m)) if m.contains("not valid text")),
                "{state:?}"
            );
            assert!(!remote.join("odd").exists(), "nothing is copied");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cancelled_transfers_leave_nothing_behind() {
        let root = scratch("cancel");
        let local = root.join("local");
        std::fs::create_dir_all(&local).unwrap();
        // Listing what to upload stops when cancelled, however large the tree.
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert_eq!(
            transfer::plan_upload(&root, "/up", &cancelled).err(),
            Some(Failure::Cancelled)
        );
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

        // A long session keeps only the latest transfers that went through or were
        // cancelled, and every one that failed.
        browser.download(&["missing.bin".into()], &local);
        let failed = browser.transfers.last().unwrap().id;
        wait(&mut browser, "the failure", |b| b.active_transfers() == 0);
        browser.download(&vec!["huge.bin".to_string(); KEEP_FINISHED + 5], &local);
        browser.cancel_all();
        wait(&mut browser, "the cancellations", |b| {
            b.active_transfers() == 0
        });
        assert_eq!(browser.transfers.len(), KEEP_FINISHED + 1);
        assert!(browser.transfers.iter().any(|t| t.id == failed));
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
        // What is staged is readable by nobody else while it is written.
        #[cfg(unix)]
        for name in listing(&remote) {
            use std::os::unix::fs::PermissionsExt;
            if name.starts_with(".huge.bin.") {
                let meta = std::fs::metadata(remote.join(&name)).unwrap();
                assert_eq!(meta.permissions().mode() & 0o077, 0, "{name}");
            }
        }
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

        // A name as long as allowed still leaves room to stage its replacement.
        let long = format!("{}.txt", "n".repeat(236));
        std::fs::write(remote.join(&long), "old").unwrap();
        std::fs::write(local.join(&long), "new").unwrap();
        browser.clear_finished();
        browser.upload(vec![local.join(&long)], &folder, true);
        wait(&mut browser, "the long name", |b| b.active_transfers() == 0);
        assert_eq!(browser.transfers[0].state, TransferState::Done);
        assert_eq!(std::fs::read_to_string(remote.join(&long)).unwrap(), "new");
        std::fs::remove_file(remote.join(&long)).unwrap();
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

    /// An editor that opens nothing, for tests that save the copy themselves.
    const NO_EDITOR: &str = if cfg!(windows) { "cmd /c rem" } else { "true" };

    /// Waits for `browser`'s only edit to settle, and returns it.
    fn settled<'a>(browser: &'a mut Browser, what: &str) -> &'a Edit {
        wait(browser, what, |b| {
            !matches!(b.edits[0].state, EditState::Opening | EditState::Uploading)
                && b.active_transfers() == 0
        });
        &browser.edits[0]
    }

    #[test]
    fn edits_upload_each_save_and_stop_at_changes_on_the_server() {
        let root = scratch("edit");
        std::fs::write(root.join("notes.txt"), "one").unwrap();
        let Some(mut browser) = local_browser(&root) else {
            return;
        };
        browser.edit("notes.txt", NO_EDITOR);
        let edit = settled(&mut browser, "the copy");
        assert_eq!(edit.state, EditState::Open);
        let local = edit.local.clone();
        assert_eq!(local.file_name().unwrap(), "notes.txt");
        assert_eq!(std::fs::read_to_string(&local).unwrap(), "one");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let folder = std::fs::metadata(local.parent().unwrap()).unwrap();
            assert_eq!(folder.permissions().mode() & 0o777, 0o700);
        }
        // Editing the same file again opens the same copy.
        browser.edit("notes.txt", NO_EDITOR);
        assert_eq!(browser.edits.len(), 1);

        // A save is uploaded.
        std::fs::write(&local, "two").unwrap();
        wait(&mut browser, "the upload", |b| {
            b.edits[0].uploaded.is_some()
        });
        let edit = settled(&mut browser, "the upload");
        assert_eq!(edit.state, EditState::Open);
        assert_eq!(
            std::fs::read_to_string(root.join("notes.txt")).unwrap(),
            "two"
        );
        assert_eq!(browser.unsaved_edits(), 0);

        // Restoring a same-size save's mtime must not hide its changed contents.
        let modified = std::fs::metadata(&local).unwrap().modified().unwrap();
        std::fs::write(&local, "TWO").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&local)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        wait(&mut browser, "the save that preserves metadata", |b| {
            b.active_transfers() == 0
                && std::fs::read_to_string(root.join("notes.txt"))
                    .ok()
                    .as_deref()
                    == Some("TWO")
        });
        assert_eq!(browser.unsaved_edits(), 0);

        // Someone else saves on the server; the next save waits instead of overwriting.
        std::fs::write(root.join("notes.txt"), "theirs, longer").unwrap();
        std::fs::write(&local, "three").unwrap();
        wait(&mut browser, "the conflict", |b| {
            matches!(b.edits[0].state, EditState::Changed(_))
        });
        assert_eq!(
            std::fs::read_to_string(root.join("notes.txt")).unwrap(),
            "theirs, longer"
        );
        assert_eq!(std::fs::read_to_string(&local).unwrap(), "three");
        assert_eq!(browser.unsaved_edits(), 1);
        assert!(matches!(
            browser.transfers.last().unwrap().state,
            TransferState::Stopped(Failure::Changed(_))
        ));

        // Overwriting from the transfer queue goes through the edit.
        let id = browser.transfers.last().unwrap().id;
        browser.retry(id, true);
        let edit = settled(&mut browser, "the overwrite");
        assert_eq!(edit.state, EditState::Open);
        assert_eq!(
            std::fs::read_to_string(root.join("notes.txt")).unwrap(),
            "three"
        );
        assert_eq!(
            browser.transfers.len(),
            1,
            "earlier uploads leave the queue"
        );

        // Stopping removes the copy.
        let id = browser.edits[0].id;
        browser.offer_stop_edit(id);
        assert!(
            browser.edits[0].checking(),
            "closing is checked off the GUI thread"
        );
        wait(&mut browser, "the saved copy to close", |b| {
            b.edits.is_empty()
        });
        assert!(browser.view.confirm.is_none(), "nothing to lose");
        assert!(browser.edits.is_empty());
        assert!(!local.parent().unwrap().exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_upload_keeps_the_changes_for_a_retry_after_reconnecting() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch("edit-fail");
        let remote = root.join("remote");
        std::fs::create_dir_all(&remote).unwrap();
        std::fs::write(remote.join("app.conf"), "port = 80").unwrap();
        let Some(mut browser) = local_browser(&remote) else {
            return;
        };
        browser.edit("app.conf", NO_EDITOR);
        let local = settled(&mut browser, "the copy").local.clone();

        // A file and folder nobody may write make the upload fail.
        let mode = |m| std::fs::Permissions::from_mode(m);
        std::fs::set_permissions(remote.join("app.conf"), mode(0o444)).unwrap();
        std::fs::set_permissions(&remote, mode(0o555)).unwrap();
        if std::fs::write(remote.join("probe"), "").is_ok() {
            eprintln!("skipping: permissions do not apply to this user");
            std::fs::set_permissions(&remote, mode(0o755)).unwrap();
            return;
        }
        std::fs::write(&local, "port = 8080").unwrap();
        wait(&mut browser, "the failure", |b| {
            matches!(b.edits[0].state, EditState::Failed(_))
        });
        assert_eq!(browser.unsaved_edits(), 1);
        assert_eq!(std::fs::read_to_string(&local).unwrap(), "port = 8080");
        // Stopping now asks first.
        let id = browser.edits[0].id;
        browser.offer_stop_edit(id);
        assert_eq!(browser.view.confirm, Some(Confirm::StopEditing(id)));
        browser.view.confirm = None;

        // A new connection takes the edit over, and Retry uploads the kept changes.
        std::fs::set_permissions(&remote, mode(0o755)).unwrap();
        std::fs::set_permissions(remote.join("app.conf"), mode(0o644)).unwrap();
        let edits = browser.take_edits();
        drop(browser);
        assert!(local.exists(), "an unsaved copy outlives its browser");
        let mut browser = local_browser(&remote).unwrap();
        browser.adopt_edits(edits);
        let id = browser.edits[0].id;
        browser.retry_edit(id, false);
        let edit = settled(&mut browser, "the retry");
        assert_eq!(edit.state, EditState::Open, "{:?}", edit.state);
        assert_eq!(
            std::fs::read_to_string(remote.join("app.conf")).unwrap(),
            "port = 8080"
        );

        // Saves made after taking over are watched too.
        std::fs::write(&local, "port = 9090").unwrap();
        wait(&mut browser, "the next upload", |b| {
            std::fs::read_to_string(remote.join("app.conf")).unwrap() == "port = 9090"
                && b.edits[0].state == EditState::Open
        });
        drop(browser);
        let deadline = Instant::now() + Duration::from_secs(5);
        while local.exists() {
            assert!(
                Instant::now() < deadline,
                "a saved copy goes with its browser"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn edits_notice_changes_that_keep_the_size_and_time() {
        let root = scratch("edit-same");
        let file = root.join("token.txt");
        std::fs::write(&file, "aaaa").unwrap();
        let Some(mut browser) = local_browser(&root) else {
            return;
        };
        browser.edit("token.txt", NO_EDITOR);
        let local = settled(&mut browser, "the copy").local.clone();
        // Someone saves the same number of bytes and puts the old time back.
        let modified = std::fs::metadata(&file).unwrap().modified().unwrap();
        std::fs::write(&file, "bbbb").unwrap();
        let theirs = std::fs::File::options().write(true).open(&file).unwrap();
        theirs.set_modified(modified).unwrap();
        drop(theirs);
        std::fs::write(&local, "cccc").unwrap();
        wait(&mut browser, "the conflict", |b| {
            matches!(b.edits[0].state, EditState::Changed(_))
        });
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "bbbb");
        assert_eq!(listing(&root), ["token.txt"], "no staged file is left");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The upload reads from a pipe the test writes, so it is surely under way while the
    /// file changes on the server.
    #[cfg(unix)]
    #[test]
    fn a_save_on_the_server_during_an_upload_is_not_overwritten() {
        use std::io::Write;
        let root = scratch("edit-race");
        let remote = root.join("remote");
        std::fs::create_dir_all(&remote).unwrap();
        let file = remote.join("data.txt");
        std::fs::write(&file, "original").unwrap();
        let Some(mut browser) = local_browser(&remote) else {
            return;
        };
        browser.edit("data.txt", NO_EDITOR);
        let copy = settled(&mut browser, "the copy").local.clone();
        let pipe = root.join("pipe");
        let made = std::process::Command::new("mkfifo").arg(&pipe).status();
        if !made.is_ok_and(|s| s.success()) {
            eprintln!("skipping: mkfifo is not available");
            return;
        }
        browser.edits[0].local = pipe.clone();
        let id = browser.edits[0].id;
        browser.retry_edit(id, false);
        let mut writer = std::fs::File::options().write(true).open(&pipe).unwrap();
        writer.write_all(b"mine, half").unwrap();
        // The replacement being written beside the file shows the upload is under way.
        wait(&mut browser, "the upload to start", |_| {
            listing(&remote).len() > 1
        });
        std::fs::write(&file, "theirs").unwrap();
        writer.write_all(b" and the rest").unwrap();
        drop(writer);
        wait(&mut browser, "the upload to end", |b| {
            b.edits[0].state != EditState::Uploading
        });
        assert!(
            matches!(browser.edits[0].state, EditState::Changed(_)),
            "{:?}",
            browser.edits[0].state
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "theirs");
        assert_eq!(listing(&remote), ["data.txt"], "no staged file is left");
        let _ = std::fs::remove_dir_all(copy.parent().unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn saves_while_disconnected_or_not_yet_seen_keep_the_copy() {
        let root = scratch("edit-offline");
        std::fs::write(root.join("a.txt"), "a").unwrap();
        std::fs::write(root.join("b.txt"), "b").unwrap();
        std::fs::write(root.join("c.txt"), "c").unwrap();
        let Some(mut browser) = local_browser(&root) else {
            return;
        };
        browser.edit("a.txt", NO_EDITOR);
        browser.edit("b.txt", NO_EDITOR);
        browser.edit("c.txt", NO_EDITOR);
        wait(&mut browser, "the copies", |b| {
            b.edits.iter().all(|e| e.state == EditState::Open)
        });
        let (a, b) = (
            browser.edits[0].local.clone(),
            browser.edits[1].local.clone(),
        );
        assert_eq!(browser.unsaved_edits(), 0);

        browser.disconnect("Gone.");
        std::fs::write(&a, "a, offline").unwrap();
        // Counted at once, before the save is reported.
        assert_eq!(browser.unsaved_edits(), 1);
        wait(&mut browser, "the save to be noticed", |b| {
            matches!(b.edits[0].state, EditState::Failed(_))
        });
        assert_eq!(browser.edits[1].state, EditState::Open);
        // A save just before closing, too quick to be reported.
        std::fs::write(&b, "b, at the last moment").unwrap();
        // A save that keeps the size and time, which the copy's look cannot tell.
        let c = browser.edits[2].local.clone();
        let modified = std::fs::metadata(&c).unwrap().modified().unwrap();
        std::fs::write(&c, "C").unwrap();
        let file = std::fs::File::options().write(true).open(&c).unwrap();
        file.set_modified(modified).unwrap();
        drop(file);
        let id = browser.edits[2].id;
        browser.offer_stop_edit(id);
        wait(&mut browser, "the content check", |b| {
            b.view.confirm.is_some()
        });
        assert_eq!(browser.view.confirm, Some(Confirm::StopEditing(id)));
        drop(browser);
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "a, offline");
        assert_eq!(
            std::fs::read_to_string(&b).unwrap(),
            "b, at the last moment"
        );
        assert_eq!(std::fs::read_to_string(&c).unwrap(), "C");
        for copy in [a, b, c] {
            let _ = std::fs::remove_dir_all(copy.parent().unwrap());
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A folder that takes no new files leaves nowhere to stage a save, and an edited file is
    /// never written over in place, where an interrupted upload would leave it half written.
    #[cfg(unix)]
    #[test]
    fn edits_are_not_written_over_in_place() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch("edit-in-place");
        let remote = root.join("remote");
        let local = root.join("local");
        std::fs::create_dir_all(&remote).unwrap();
        std::fs::create_dir_all(&local).unwrap();
        let file = remote.join("app.conf");
        std::fs::write(&file, "port = 80").unwrap();
        let Some(mut browser) = local_browser(&remote) else {
            return;
        };
        browser.edit("app.conf", NO_EDITOR);
        let copy = settled(&mut browser, "the copy").local.clone();
        let mode = |m| std::fs::Permissions::from_mode(m);
        std::fs::set_permissions(&remote, mode(0o555)).unwrap();
        if std::fs::write(remote.join("probe"), "").is_ok() {
            eprintln!("skipping: permissions do not apply to this user");
            std::fs::set_permissions(&remote, mode(0o755)).unwrap();
            return;
        }
        std::fs::write(&copy, "port = 8080").unwrap();
        wait(&mut browser, "the refusal", |b| {
            matches!(b.edits[0].state, EditState::Failed(_))
        });
        let EditState::Failed(message) = &browser.edits[0].state else {
            unreachable!()
        };
        assert!(message.contains("takes no new files"), "{message}");
        // Overwriting does not write in place either.
        let id = browser.edits[0].id;
        browser.retry_edit(id, true);
        settled(&mut browser, "the second refusal");
        assert!(matches!(browser.edits[0].state, EditState::Failed(_)));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "port = 80");
        assert_eq!(std::fs::read_to_string(&copy).unwrap(), "port = 8080");
        assert_eq!(listing(&remote), ["app.conf"]);

        // An ordinary upload writes in place only once asked to.
        std::fs::write(local.join("app.conf"), "uploaded").unwrap();
        let folder = browser.path.clone();
        browser.upload(vec![local.join("app.conf")], &folder, true);
        wait(&mut browser, "the upload", |b| b.active_transfers() == 0);
        let upload = browser.transfers.last().unwrap();
        assert!(
            matches!(upload.state, TransferState::Stopped(Failure::InPlace(_))),
            "{:?}",
            upload.state
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "port = 80");
        browser.retry_in_place(upload.id);
        wait(&mut browser, "the upload in place", |b| {
            b.active_transfers() == 0
        });
        assert_eq!(browser.transfers.last().unwrap().state, TransferState::Done);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "uploaded");

        // Once the folder takes new files, Retry saves safely.
        std::fs::set_permissions(&remote, mode(0o755)).unwrap();
        browser.retry_edit(id, true);
        let edit = settled(&mut browser, "the retry");
        assert_eq!(edit.state, EditState::Open, "{:?}", edit.state);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "port = 8080");
        let _ = std::fs::remove_dir_all(copy.parent().unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn hidden_progress_does_not_wake_the_window_but_its_end_does() {
        let ctx = egui::Context::default();
        let wakes = Arc::new(Mutex::new(0));
        let seen = wakes.clone();
        ctx.set_request_repaint_callback(move |_| *seen.lock().unwrap() += 1);
        let (sender, incoming) = std_mpsc::channel();
        let events = Events {
            sender,
            ctx: Some(ctx.clone()),
            shown: Arc::new(AtomicBool::new(false)),
            progress: Default::default(),
        };
        // egui reports a wake only when sooner than one already due, so frames pass first.
        let woke = |event| {
            for _ in 0..3 {
                let _ = ctx.run(egui::RawInput::default(), |_| {});
            }
            *wakes.lock().unwrap() = 0;
            events.send(event);
            *wakes.lock().unwrap() > 0
        };
        let progress = || Event::Progress {
            id: 1,
            done: 1,
            files_done: 0,
        };
        assert!(!woke(progress()));
        let result = Err(Failure::Cancelled);
        assert!(woke(Event::Finished { id: 1, result }));
        events.shown.store(true, Ordering::Relaxed);
        assert!(woke(progress()));
        // A sleeping window keeps one snapshot, not every intermediate update.
        while incoming.try_recv().is_ok() {}
        events.shown.store(false, Ordering::Relaxed);
        for done in 0..100_000 {
            events.send(Event::Progress {
                id: 1,
                done,
                files_done: 2,
            });
        }
        assert_eq!(events.progress.lock().unwrap().len(), 1);
        assert!(incoming.try_recv().is_err());
        events.send(Event::Finished {
            id: 1,
            result: Err(Failure::Cancelled),
        });
        assert!(events.progress.lock().unwrap().is_empty());
        assert!(matches!(
            incoming.try_recv().unwrap(),
            Event::Progress {
                done: 99_999,
                files_done: 2,
                ..
            }
        ));
        assert!(matches!(
            incoming.try_recv().unwrap(),
            Event::Finished { .. }
        ));
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
