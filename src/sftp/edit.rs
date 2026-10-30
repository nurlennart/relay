//! Editing remote files in an external editor: a copy is downloaded into a private folder
//! and opened, and each save is uploaded through the transfer queue, unless the file changed
//! on the server since it was opened or last uploaded. A copy whose changes are not on the
//! server yet is never removed without asking.
//!
//! Saves are watched by a thread of the browser's own rather than the connection's, so
//! saves made while disconnected are noticed too.
use super::*;
use notify::Watcher;

#[derive(Clone, Debug, PartialEq)]
pub enum EditState {
    /// The copy is downloading.
    Opening,
    /// Open in the editor, with every save on the server.
    Open,
    /// A save is on its way to the server.
    Uploading,
    /// Opening or uploading failed. The copy keeps its changes until retried.
    Failed(String),
    /// The server's file changed since it was opened or last uploaded, so the save waits to
    /// be uploaded over it.
    Changed(String),
}

/// A remote file open in an external editor.
pub struct Edit {
    pub id: u64,
    pub name: String,
    /// The file on the server.
    pub remote: String,
    /// The copy the editor works on, alone in a folder of Relay's.
    pub local: PathBuf,
    pub state: EditState,
    /// When a save last reached the server.
    pub uploaded: Option<SystemTime>,
    /// The copy has been downloaded.
    pub opened: bool,
    /// The editor command, empty for the system's.
    editor: String,
    /// The server's file as last downloaded or uploaded; an upload expects to find it so.
    version: Option<Version>,
    /// The copy as last saved, so a watch started again reports only newer saves.
    seen: Option<Seen>,
    /// Contents last reported by the save watcher, including offline saves.
    seen_contents: Option<u64>,
    /// The copy as it was when it last matched the server's file.
    synced: Option<Seen>,
    /// A fingerprint of the contents the server's file and the copy last shared. Size and
    /// time can stay the same through a save, so only this proves a copy has nothing new.
    synced_contents: Option<u64>,
    /// The copy as it was when the upload under way started.
    sending: Option<Seen>,
    /// Saved again while uploading: upload once more when that finishes.
    again: bool,
    /// A close check runs on the watcher, never the GUI thread.
    stop_check: Option<CancellationToken>,
}

impl Edit {
    pub fn checking(&self) -> bool {
        self.stop_check.is_some()
    }
    /// Whether the copy holds changes the server does not have. The copy itself is checked,
    /// so a save not reported yet counts too.
    pub fn unsaved(&self) -> bool {
        self.opened
            && (self.again
                || matches!(
                    self.state,
                    EditState::Uploading | EditState::Failed(_) | EditState::Changed(_)
                )
                || seen(&self.local) != self.synced
                || self.seen_contents != self.synced_contents)
    }

    /// Whether the copy surely holds nothing the server lacks: besides [`Self::unsaved`],
    /// its contents are compared, so a save that kept its size and time is not lost.
    fn synced(&self) -> bool {
        !self.opened
            || (!self.unsaved()
                && self.synced_contents.is_some()
                && fingerprint(&self.local).ok() == self.synced_contents)
    }
}

/// What the watcher is told.
pub(super) enum Watch {
    /// Report saves to the copy at `path` of edit `id` other than `seen`.
    Add {
        id: u64,
        path: PathBuf,
        seen: Option<Seen>,
        contents: Option<u64>,
    },
    Remove(u64),
    CheckStop {
        id: u64,
        path: PathBuf,
        expected: Option<u64>,
        cancel: CancellationToken,
    },
    Finish(Vec<Edit>),
}

impl Browser {
    /// Opens `name` in the current folder in `editor` (empty for the system's), downloading a
    /// copy first. A file already open is shown in the editor again.
    pub fn edit(&mut self, name: &str, editor: &str) {
        let remote = join(&self.path, name);
        if let Some(index) = self.edits.iter().position(|e| e.remote == remote) {
            if self.edits[index].opened {
                self.open_editor(index);
            }
            return;
        }
        let Some(local_name) = local_name(name) else {
            self.error = Some(format!("“{name}” cannot be saved on this computer."));
            return;
        };
        let folder = match copy_folder() {
            Ok(folder) => folder,
            Err(e) => {
                self.error = Some(format!("Cannot make a copy to edit: {e}"));
                return;
            }
        };
        let id = self.next_id;
        self.next_id += 1;
        self.edits.push(Edit {
            id,
            name: name.into(),
            remote,
            local: folder.join(local_name),
            state: EditState::Opening,
            uploaded: None,
            opened: false,
            editor: editor.into(),
            version: None,
            seen: None,
            seen_contents: None,
            synced: None,
            synced_contents: None,
            sending: None,
            again: false,
            stop_check: None,
        });
        self.download_copy(id);
    }

    /// Shows edit `id`'s copy in its editor again, as after the editor was closed.
    pub fn reopen_edit(&mut self, id: u64) {
        if let Some(index) = self.edits.iter().position(|e| e.id == id && e.opened) {
            self.open_editor(index);
        }
    }

    fn open_editor(&mut self, index: usize) {
        let edit = &self.edits[index];
        if let Err(e) = crate::platform::edit_file(&edit.editor, &edit.local) {
            self.error = Some(format!(
                "{e}. Choose another editor in Settings → Terminal, or open {} yourself; \
                 saves are still uploaded.",
                edit.local.display()
            ));
        }
    }

    /// Downloads edit `id`'s copy, again if an earlier attempt failed.
    fn download_copy(&mut self, id: u64) {
        let Some(edit) = self.edits.iter_mut().find(|e| e.id == id) else {
            return;
        };
        edit.state = EditState::Opening;
        let job = Job::Download {
            remote: edit.remote.clone(),
            local: edit.local.clone(),
            // The folder is Relay's own.
            replace: true,
        };
        let (name, folder) = (edit.name.clone(), parent_folder(&edit.local));
        self.forget_transfers(id);
        self.queue(job, Direction::Download, name, folder, Some(id));
    }

    /// Uploads edit `id`'s copy, over whatever is on the server if `overwrite`, and otherwise
    /// only if nobody changed it there.
    fn upload_copy(&mut self, id: u64, overwrite: bool) {
        let ended = self.ended();
        let Some(edit) = self.edits.iter_mut().find(|e| e.id == id) else {
            return;
        };
        edit.again = false;
        if ended {
            edit.state = EditState::Failed("Not connected. Reconnect, then retry.".into());
            return;
        }
        edit.state = EditState::Uploading;
        edit.sending = seen(&edit.local);
        let job = Job::Upload {
            local: edit.local.clone(),
            remote: edit.remote.clone(),
            replace: true,
            unchanged: if overwrite { None } else { edit.version },
            in_place: false,
        };
        let (name, folder) = (edit.name.clone(), parent(&edit.remote).unwrap_or_default());
        self.forget_transfers(id);
        self.queue(job, Direction::Upload, name, folder, Some(id));
    }

    /// Clears edit `id`'s finished transfers from the queue, so saving often does not fill it.
    fn forget_transfers(&mut self, id: u64) {
        self.transfers
            .retain(|t| t.edit != Some(id) || !t.finished());
    }

    /// Tries edit `id` again: downloads a copy that never arrived, or uploads the copy,
    /// replacing the server's changed file if `overwrite`.
    pub fn retry_edit(&mut self, id: u64, overwrite: bool) {
        match self.edits.iter().find(|e| e.id == id) {
            Some(edit) if !edit.opened => self.download_copy(id),
            Some(_) => self.upload_copy(id, overwrite),
            None => {}
        }
    }

    /// Stops editing `id`, first asking if its changes are not on the server.
    pub fn offer_stop_edit(&mut self, id: u64) {
        let Some(edit) = self.edits.iter_mut().find(|e| e.id == id) else {
            return;
        };
        if edit.checking() {
            return;
        }
        if edit.unsaved() {
            self.view.confirm = Some(Confirm::StopEditing(id));
        } else if !edit.opened {
            self.stop_edit(id);
        } else {
            let cancel = CancellationToken::new();
            edit.stop_check = Some(cancel.clone());
            let check = Watch::CheckStop {
                id,
                path: edit.local.clone(),
                expected: edit.synced_contents,
                cancel,
            };
            if !self.watch(check) {
                self.edits
                    .iter_mut()
                    .find(|e| e.id == id)
                    .unwrap()
                    .stop_check = None;
                self.view.confirm = Some(Confirm::StopEditing(id));
            }
        }
    }

    pub(super) fn edit_stop_checked(&mut self, id: u64, clean: bool) {
        let Some(edit) = self.edits.iter_mut().find(|e| e.id == id) else {
            return;
        };
        edit.stop_check = None;
        if clean {
            self.forget_edit(id, false);
        } else {
            self.view.confirm = Some(Confirm::StopEditing(id));
        }
    }

    /// Stops watching edit `id` and removes its copy, with any changes not uploaded.
    pub fn stop_edit(&mut self, id: u64) {
        self.forget_edit(id, true);
    }

    fn forget_edit(&mut self, id: u64, discard: bool) {
        let Some(index) = self.edits.iter().position(|e| e.id == id) else {
            return;
        };
        let edit = self.edits.remove(index);
        if let Some(cancel) = &edit.stop_check {
            cancel.cancel();
        }
        self.watch(Watch::Remove(id));
        for t in self.transfers.iter().filter(|t| t.edit == Some(id)) {
            t.cancel.cancel();
        }
        if discard {
            remove_copy(&edit);
        }
    }

    /// Edits whose changes are not on the server.
    pub fn unsaved_edits(&self) -> usize {
        self.edits.iter().filter(|e| e.unsaved()).count()
    }

    /// Hands over the edits, as a browser that reconnects does to the next one.
    pub fn take_edits(&mut self) -> Vec<Edit> {
        let mut edits = std::mem::take(&mut self.edits);
        for edit in &mut edits {
            if let Some(cancel) = edit.stop_check.take() {
                cancel.cancel();
            }
            self.watch(Watch::Remove(edit.id));
        }
        edits
    }

    /// Carries on with edits from an earlier connection: copies are watched again, so saves
    /// made meanwhile are uploaded, and copies that never arrived are downloaded.
    pub fn adopt_edits(&mut self, edits: Vec<Edit>) {
        for mut edit in edits {
            edit.id = self.next_id;
            self.next_id += 1;
            let id = edit.id;
            if edit.state == EditState::Uploading {
                edit.state = EditState::Failed("The upload did not finish.".into());
            }
            let opened = edit.opened;
            if opened {
                self.watch(Watch::Add {
                    id,
                    path: edit.local.clone(),
                    seen: edit.seen,
                    contents: edit.seen_contents,
                });
            }
            self.edits.push(edit);
            if !opened {
                self.download_copy(id);
            }
        }
    }

    /// Follows up on a transfer of edit `id`'s copy.
    pub(super) fn edit_finished(&mut self, id: u64, result: Result<Option<Version>, Failure>) {
        let Some(index) = self.edits.iter().position(|e| e.id == id) else {
            return;
        };
        let edit = &mut self.edits[index];
        match result {
            Ok(version) if !edit.opened => {
                edit.opened = true;
                edit.version = version;
                edit.seen = seen(&edit.local);
                edit.synced = edit.seen;
                edit.synced_contents = version.map(|v| v.contents);
                edit.seen_contents = edit.synced_contents;
                edit.state = EditState::Open;
                let watch = Watch::Add {
                    id,
                    path: edit.local.clone(),
                    seen: edit.seen,
                    contents: edit.seen_contents,
                };
                self.watch(watch);
                self.open_editor(index);
            }
            Ok(version) => {
                edit.version = version;
                edit.synced = edit.sending.take();
                edit.synced_contents = version.map(|v| v.contents);
                edit.uploaded = Some(SystemTime::now());
                edit.state = EditState::Open;
                if edit.again {
                    self.upload_copy(id, false);
                }
            }
            Err(Failure::Changed(message)) => {
                edit.again = false;
                edit.state = EditState::Changed(message);
            }
            Err(failure) => {
                edit.again = false;
                edit.state = EditState::Failed(failure.message().into());
            }
        }
    }

    /// Uploads a save of edit `id`'s copy, or once the upload under way finishes.
    pub(super) fn edit_saved(&mut self, id: u64, seen: Seen, contents: u64) {
        let Some(edit) = self.edits.iter_mut().find(|e| e.id == id) else {
            return;
        };
        edit.seen = Some(seen);
        edit.seen_contents = Some(contents);
        match edit.state {
            EditState::Opening => {}
            EditState::Uploading => edit.again = true,
            _ => self.upload_copy(id, false),
        }
    }

    /// Adds an edit as if it were open in an editor, for rendering checks.
    #[cfg(feature = "ui-check")]
    pub fn demo_edit(&mut self, name: &str, state: EditState) {
        let id = self.next_id;
        self.next_id += 1;
        self.edits.push(Edit {
            id,
            name: name.into(),
            remote: join(&self.path, name),
            local: std::env::temp_dir().join("relay-edit-3f9a1c").join(name),
            uploaded: (state == EditState::Open).then(SystemTime::now),
            opened: state != EditState::Opening,
            state,
            editor: String::new(),
            version: None,
            seen: None,
            seen_contents: None,
            synced: None,
            synced_contents: None,
            sending: None,
            again: false,
            stop_check: None,
        });
    }

    /// Tells the watcher, starting it with the first copy to watch.
    fn watch(&mut self, watch: Watch) -> bool {
        if self.watcher.is_none() {
            let (sender, commands) = std_mpsc::channel();
            let events = self.events_out.clone();
            let spawned = std::thread::Builder::new()
                .name("relay-edit-watch".into())
                .spawn(move || watcher(commands, events));
            if let Err(e) = spawned {
                self.error = Some(format!("Cannot watch for saves: {e}"));
                return false;
            }
            self.watcher = Some(sender);
        }
        self.watcher
            .as_ref()
            .is_some_and(|watcher| watcher.send(watch).is_ok())
    }

    /// Reuse the watcher for cleanup, which can read large copies after the UI closes.
    pub(super) fn finish_edits(&mut self) {
        let mut edits = std::mem::take(&mut self.edits);
        for edit in &mut edits {
            if let Some(cancel) = edit.stop_check.take() {
                cancel.cancel();
            }
        }
        if let Some(watcher) = self.watcher.take() {
            // If the worker has failed, keep the copies rather than deleting unverified data.
            let _ = watcher.send(Watch::Finish(edits));
        } else {
            for edit in edits.iter().filter(|e| !e.opened) {
                remove_copy(edit);
            }
        }
    }
}

/// Marks edit `id` as stopped by `failure` when its transfer cannot finish.
pub(super) fn interrupted(edits: &mut [Edit], id: u64, failure: &Failure) {
    if let Some(edit) = edits.iter_mut().find(|e| e.id == id) {
        edit.again = false;
        edit.state = EditState::Failed(failure.message().into());
    }
}

/// Removes the copies whose changes are all on the server, as a browser closes. The others
/// stay where they are, so nothing typed is lost.
fn discard_saved(edits: &[Edit]) {
    for edit in edits.iter().filter(|e| e.synced()) {
        remove_copy(edit);
    }
}

fn remove_copy(edit: &Edit) {
    if let Some(folder) = edit.local.parent() {
        let _ = std::fs::remove_dir_all(folder);
    }
}

fn parent_folder(path: &Path) -> String {
    path.parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

/// How the local file at `path` looks now.
fn seen(path: &Path) -> Option<Seen> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()))
}

/// A fingerprint of the local file at `path`, taken as transfers take theirs.
fn fingerprint(path: &Path) -> std::io::Result<u64> {
    use std::{hash::Hasher, io::Read};
    let mut file = std::fs::File::open(path)?;
    let mut hash = std::hash::DefaultHasher::new();
    let mut buffer = vec![0u8; CHUNK];
    loop {
        match file.read(&mut buffer)? {
            0 => return Ok(hash.finish()),
            n => hash.write(&buffer[..n]),
        }
    }
}

/// Creates a new folder, readable only by this user, for one copy to edit. The copy keeps
/// its name, so editors recognize its type.
fn copy_folder() -> std::io::Result<PathBuf> {
    let base = std::env::temp_dir();
    loop {
        let mut bytes = [0u8; 6];
        getrandom::fill(&mut bytes).map_err(std::io::Error::other)?;
        let suffix: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let folder = base.join(format!("relay-edit-{suffix}"));
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        match builder.create(&folder) {
            Ok(()) => return Ok(folder),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}

/// Reports saves to the copies it is told to watch, until the browser goes. A save is
/// reported once the file has looked the same for a moment, so a file still being written is
/// not uploaded half done.
fn watcher(commands: std_mpsc::Receiver<Watch>, events: Events) {
    use std_mpsc::RecvTimeoutError;
    // Coalesce native events; a busy editor must not create an unbounded event queue.
    let changed = Arc::new(Mutex::new(Some(BTreeSet::<PathBuf>::new())));
    let reported = changed.clone();
    let mut native = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let mut paths = reported.lock().unwrap();
        match event {
            Ok(event) if matches!(event.kind, notify::EventKind::Access(_)) => {}
            Ok(event) if !event.paths.is_empty() => {
                if let Some(pending) = paths.as_mut() {
                    pending.extend(event.paths);
                    if pending.len() > 1024 {
                        *paths = None;
                    }
                }
            }
            _ => *paths = None,
        }
    })
    .ok();
    struct Watched {
        id: u64,
        path: PathBuf,
        seen: Option<Seen>,
        contents: Option<u64>,
        last: Option<Seen>,
        dirty: bool,
        stable: bool,
        native: bool,
        checked: Instant,
    }
    let mut watched: Vec<Watched> = vec![];
    let mut next = Instant::now();
    loop {
        let command = if watched.is_empty() {
            commands.recv().map_err(|_| RecvTimeoutError::Disconnected)
        } else {
            commands.recv_timeout(next.saturating_duration_since(Instant::now()))
        };
        match command {
            Ok(Watch::Add {
                id,
                path,
                seen,
                contents,
            }) => {
                let path = path.canonicalize().unwrap_or(path);
                watched.retain(|w| w.id != id);
                let watching = native.as_mut().map(|n| {
                    n.watch(
                        path.parent().unwrap_or(&path),
                        notify::RecursiveMode::NonRecursive,
                    )
                });
                let direct = matches!(watching, Some(Ok(())));
                if !direct {
                    events.send(Event::WatchError(format!(
                        "Saves to {} will be checked every 5 seconds because file notifications are unavailable.", path.display()
                    )));
                }
                watched.push(Watched {
                    id,
                    path,
                    seen,
                    contents,
                    last: seen,
                    dirty: true,
                    stable: false,
                    native: direct,
                    checked: Instant::now(),
                });
            }
            Ok(Watch::Remove(id)) => {
                if let Some(w) = watched.iter().find(|w| w.id == id)
                    && let Some(native) = &mut native
                {
                    let _ = native.unwatch(w.path.parent().unwrap_or(&w.path));
                }
                watched.retain(|w| w.id != id);
            }
            Ok(Watch::CheckStop {
                id,
                path,
                expected,
                cancel,
            }) => {
                if cancel.is_cancelled() {
                    continue;
                }
                let clean = expected.is_some() && fingerprint(&path).ok() == expected;
                if !cancel.is_cancelled() {
                    if clean {
                        if let Some(folder) = path.parent() {
                            let _ = std::fs::remove_dir_all(folder);
                        }
                        watched.retain(|w| w.id != id);
                        if let Some(native) = &mut native {
                            let _ = native.unwatch(path.parent().unwrap_or(&path));
                        }
                    }
                    events.send(Event::EditStopChecked { id, clean, cancel });
                }
            }
            Ok(Watch::Finish(edits)) => {
                drop(native);
                discard_saved(&edits);
                return;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        if Instant::now() < next {
            continue;
        }
        next = Instant::now() + WATCH_INTERVAL;
        let dirty = changed.lock().unwrap().replace(BTreeSet::new());
        for w in &mut watched {
            let signalled = dirty.as_ref().is_none_or(|paths| {
                paths.iter().any(|path| {
                    path == &w.path
                        || Some(path.as_path()) == w.path.parent()
                        || path.parent() == w.path.parent()
                })
            });
            let now = seen(&w.path);
            if signalled || now != w.last {
                w.dirty = true;
                w.stable = false;
            } else {
                w.stable = true;
            }
            w.last = now;
            if !w.stable || now.is_none() {
                continue;
            }
            if !w.dirty && (w.native || w.checked.elapsed() < Duration::from_secs(5)) {
                continue;
            }
            let Ok(contents) = fingerprint(&w.path) else {
                continue;
            };
            // A file replaced or still being written must settle again.
            if seen(&w.path) != now {
                w.stable = false;
                continue;
            }
            if w.seen != now || w.contents != Some(contents) {
                w.seen = now;
                w.contents = Some(contents);
                events.send(Event::Saved {
                    id: w.id,
                    seen: now.unwrap(),
                    contents,
                });
            }
            w.dirty = false;
            w.checked = Instant::now();
        }
    }
}
