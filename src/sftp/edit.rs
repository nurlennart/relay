//! Editing remote files in an external editor: a copy is downloaded into a private folder
//! and opened, and each save is uploaded through the transfer queue, unless the file changed
//! on the server since it was opened or last uploaded. A copy whose changes are not on the
//! server yet is never removed without asking.
use super::*;

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
    stamp: Option<Stamp>,
    /// The copy as last saved, so a watch started again reports only newer saves.
    seen: Option<Seen>,
    /// Saved again while uploading: upload once more when that finishes.
    again: bool,
}

impl Edit {
    /// Whether the copy holds changes the server does not have.
    pub fn unsaved(&self) -> bool {
        self.opened
            && (self.again
                || matches!(
                    self.state,
                    EditState::Uploading | EditState::Failed(_) | EditState::Changed(_)
                ))
    }
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
            stamp: None,
            seen: None,
            again: false,
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
        let Some(edit) = self.edits.iter_mut().find(|e| e.id == id) else {
            return;
        };
        edit.state = EditState::Uploading;
        edit.again = false;
        let job = Job::Upload {
            local: edit.local.clone(),
            remote: edit.remote.clone(),
            replace: true,
            unchanged: if overwrite { None } else { edit.stamp },
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
        match self.edits.iter().find(|e| e.id == id) {
            Some(edit) if edit.unsaved() => self.view.confirm = Some(Confirm::StopEditing(id)),
            Some(_) => self.stop_edit(id),
            None => {}
        }
    }

    /// Stops watching edit `id` and removes its copy, with any changes not uploaded.
    pub fn stop_edit(&mut self, id: u64) {
        let Some(index) = self.edits.iter().position(|e| e.id == id) else {
            return;
        };
        let edit = self.edits.remove(index);
        self.send(Command::Unwatch(id));
        for t in self.transfers.iter().filter(|t| t.edit == Some(id)) {
            t.cancel.cancel();
        }
        remove_copy(&edit);
    }

    /// Edits whose changes are not on the server.
    pub fn unsaved_edits(&self) -> usize {
        self.edits.iter().filter(|e| e.unsaved()).count()
    }

    /// Hands over the edits, as a browser that reconnects does to the next one.
    pub fn take_edits(&mut self) -> Vec<Edit> {
        let edits = std::mem::take(&mut self.edits);
        for edit in &edits {
            self.send(Command::Unwatch(edit.id));
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
                self.send(Command::Watch {
                    id,
                    path: edit.local.clone(),
                    seen: edit.seen,
                });
            }
            self.edits.push(edit);
            if !opened {
                self.download_copy(id);
            }
        }
    }

    /// Follows up on a transfer of edit `id`'s copy.
    pub(super) fn edit_finished(&mut self, id: u64, result: Result<Option<Stamp>, Failure>) {
        let Some(index) = self.edits.iter().position(|e| e.id == id) else {
            return;
        };
        let edit = &mut self.edits[index];
        match result {
            Ok(stamp) if !edit.opened => {
                edit.opened = true;
                edit.stamp = stamp;
                edit.seen = seen(&edit.local);
                edit.state = EditState::Open;
                let watch = Command::Watch {
                    id,
                    path: edit.local.clone(),
                    seen: edit.seen,
                };
                self.send(watch);
                self.open_editor(index);
            }
            Ok(stamp) => {
                edit.stamp = stamp;
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
    pub(super) fn edit_saved(&mut self, id: u64, seen: Seen) {
        let Some(edit) = self.edits.iter_mut().find(|e| e.id == id) else {
            return;
        };
        edit.seen = Some(seen);
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
            stamp: None,
            seen: None,
            again: false,
        });
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
pub(super) fn discard_saved(edits: &[Edit]) {
    for edit in edits.iter().filter(|e| !e.unsaved()) {
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

/// Creates a new folder, readable only by this user, for one copy to edit. The copy keeps
/// its name, so editors recognize its type.
fn copy_folder() -> std::io::Result<PathBuf> {
    let base = std::env::temp_dir();
    loop {
        let mut bytes = [0u8; 6];
        getrandom::fill(&mut bytes).map_err(std::io::Error::other)?;
        let suffix: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let folder = base.join(format!("relay-edit-{suffix}"));
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

/// Reports saves to the copies it is told to watch. A save is reported once the file has
/// looked the same for a moment, so a file still being written is not uploaded half done.
pub(super) async fn watch(mut commands: mpsc::UnboundedReceiver<Command>, events: Events) {
    struct Watched {
        id: u64,
        path: PathBuf,
        /// The last save reported.
        seen: Option<Seen>,
        /// How the file looked at the last check.
        last: Option<Seen>,
    }
    let mut watched: Vec<Watched> = vec![];
    let mut tick = tokio::time::interval(WATCH_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(Command::Watch { id, path, seen }) => {
                    watched.retain(|w| w.id != id);
                    watched.push(Watched { id, path, seen, last: seen });
                }
                Some(Command::Unwatch(id)) => watched.retain(|w| w.id != id),
                Some(_) => {}
                None => return,
            },
            _ = tick.tick(), if !watched.is_empty() => {
                for w in &mut watched {
                    // Editors that save by replacing the file leave a moment without one.
                    let now = tokio::fs::metadata(&w.path)
                        .await
                        .ok()
                        .map(|m| (m.len(), m.modified().ok()));
                    if let Some(seen) = now
                        && now != w.seen
                        && now == w.last
                    {
                        w.seen = now;
                        events.send(Event::Saved { id: w.id, seen });
                    }
                    w.last = now;
                }
            }
        }
    }
}
