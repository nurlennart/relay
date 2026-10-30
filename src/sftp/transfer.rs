//! Copying files and folders in either direction: each transfer is planned first, so its
//! progress has a total, then streamed through a fixed buffer into a staged file that takes
//! the destination's place only once complete.
use super::*;
use crate::platform::DownloadRoot;
use std::hash::{DefaultHasher, Hasher};

/// The largest file whose contents are compared before an edit replaces it; past this, its
/// size and time must do.
const COMPARE_LIMIT: u64 = 64 * 1024 * 1024;

/// Runs queued transfers one at a time.
pub(super) async fn transfers(
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
pub(super) struct Plan<F, P> {
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

/// Copies what `job` names. A single file's transfer returns the version the server has
/// afterwards, which an edit checks for before its next upload.
async fn transfer(
    session: &SftpSession,
    id: u64,
    job: &Job,
    cancel: &CancellationToken,
    events: &Events,
) -> Result<Option<Version>, Failure> {
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
            unchanged,
            in_place,
        } => {
            let existing = exists(session, remote)
                .await
                .map_err(|e| error("Cannot check the destination", &describe(e)))?;
            // A quick look first saves uploading in vain; the contents are compared just
            // before the upload takes the file's place.
            if let Some(expected) = unchanged {
                still(session, remote, expected, false, cancel).await?;
            }
            if existing && !replace {
                return Err(Failure::Exists(format!(
                    "“{}” already exists on the server.",
                    file_name(remote)
                )));
            }
            let (from, to, stop) = (local.clone(), remote.clone(), cancel.clone());
            let planning = tokio::task::spawn_blocking(move || plan_upload(&from, &to, &stop));
            // Cancelling frees the queue at once; the listing stops at its next folder.
            let plan = cancellable(cancel, planning)
                .await?
                .map_err(|e| error("Cannot read the files", &e))??;
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
            let mut contents = 0;
            for (from, to, _) in &plan.files {
                let checks = Checks {
                    replace: *replace,
                    unchanged: unchanged.as_ref(),
                    in_place: *in_place,
                };
                contents = upload_file(session, from, to, checks, cancel, &mut progress).await?;
                progress.file_done();
            }
            if plan.folders.is_empty() {
                let stamp = session.metadata(remote).await.ok().map(|m| Stamp::of(&m));
                return Ok(stamp.map(|stamp| Version { stamp, contents }));
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
            let plan = plan_download(session, remote, local, cancel).await?;
            events.send(Event::Started {
                id,
                total: plan.total(),
                files: plan.files.len() as u64,
                skipped: plan.skipped,
            });
            let chosen = local.parent().unwrap_or(local);
            let destination = DownloadRoot::open(chosen)
                .map_err(|e| error("Cannot open the download folder", &e))?;
            for folder in &plan.folders {
                destination
                    .create_folder(folder)
                    .map_err(|e| error(&format!("Cannot create {}", folder.display()), &e))?;
            }
            let mut version = None;
            for (from, to, _) in &plan.files {
                version = download_file(
                    session,
                    from,
                    to,
                    (&destination, *replace),
                    cancel,
                    &mut progress,
                )
                .await?;
                progress.file_done();
            }
            if plan.folders.is_empty() {
                return Ok(version);
            }
        }
    }
    Ok(None)
}

/// Waits for `work` unless the transfer is cancelled first.
async fn cancellable<T>(
    cancel: &CancellationToken,
    work: impl Future<Output = T>,
) -> Result<T, Failure> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(Failure::Cancelled),
        done = work => Ok(done),
    }
}

/// Lists a local file or folder for upload. Links to folders are skipped, since they can loop.
/// A name the server cannot be sent unchanged stops it before anything is copied, and so
/// does cancelling, checked at each folder.
pub(super) fn plan_upload(
    local: &Path,
    remote: &str,
    cancel: &CancellationToken,
) -> Result<Plan<String, PathBuf>, Failure> {
    let mut plan = Plan {
        folders: vec![],
        files: vec![],
        skipped: 0,
    };
    let fail = |path: &Path, e: std::io::Error| {
        Failure::Error(format!("Cannot read {}: {e}", path.display()))
    };
    let meta = std::fs::metadata(local).map_err(|e| fail(local, e))?;
    if !meta.is_dir() {
        plan.files.push((local.into(), remote.into(), meta.len()));
        return Ok(plan);
    }
    let mut pending = vec![(local.to_path_buf(), remote.to_string())];
    while let Some((folder, target)) = pending.pop() {
        if cancel.is_cancelled() {
            return Err(Failure::Cancelled);
        }
        plan.folders.push(target.clone());
        let mut children: Vec<_> = std::fs::read_dir(&folder)
            .and_then(|d| d.collect::<Result<_, _>>())
            .map_err(|e| fail(&folder, e))?;
        children.sort_by_key(|c| c.file_name());
        for child in children {
            let path = child.path();
            let Some(name) = child.file_name().to_str().map(String::from) else {
                // Replaced characters could merge two files into one on the server.
                return Err(Failure::Error(format!(
                    "“{}” in {} has a name that is not valid text, so the server cannot \
                     be sent it unchanged.",
                    child.file_name().to_string_lossy(),
                    folder.display()
                )));
            };
            let link = child.file_type().map_err(|e| fail(&path, e))?.is_symlink();
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
/// Cancelling stops it at once, even while the server is slow to answer.
async fn plan_download(
    session: &SftpSession,
    remote: &str,
    local: &Path,
    cancel: &CancellationToken,
) -> Result<Plan<PathBuf, String>, Failure> {
    let fail = |e: &dyn std::fmt::Display| Failure::Error(format!("Cannot read {remote}: {e}"));
    let mut plan = Plan {
        folders: vec![],
        files: vec![],
        skipped: 0,
    };
    // The item that was chosen is followed if it is a link.
    let meta = cancellable(cancel, session.metadata(remote))
        .await?
        .map_err(|e| fail(&describe(e)))?;
    if file_type(&meta) != TYPE_DIR {
        plan.files.push((remote.into(), local.into(), meta.len()));
        return Ok(plan);
    }
    let mut pending = vec![(remote.to_string(), local.to_path_buf())];
    while let Some((folder, target)) = pending.pop() {
        plan.folders.push(target.clone());
        let mut items: Vec<_> = cancellable(cancel, session.read_dir(folder.clone()))
            .await?
            .map_err(|e| fail(&describe(e)))?
            .collect();
        items.sort_by_key(|i| i.file_name());
        // Names this computer cannot tell apart would be saved over one another.
        let mut keys = std::collections::HashMap::new();
        for item in items {
            let name = item.file_name();
            // A server must not be able to write outside the chosen folder.
            let Some(local_name) = local_name(&name) else {
                return Err(fail(&format!("“{name}” cannot be saved on this computer.")));
            };
            if let Some(other) = keys.insert(crate::platform::name_key(&name), name.clone()) {
                return Err(fail(&format!(
                    "“{other}” and “{name}” in {folder} would be the same file on this computer."
                )));
            }
            let path = join(&folder, &name);
            let meta = item.metadata();
            match file_type(&meta) {
                TYPE_DIR => pending.push((path, target.join(local_name))),
                TYPE_FILE => plan.files.push((path, target.join(local_name), meta.len())),
                TYPE_LINK => match cancellable(cancel, session.metadata(path.clone())).await? {
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

/// Fails unless remote `path` is still the `expected` version: its size and time, and, with
/// `contents`, what it holds, up to [`COMPARE_LIMIT`].
async fn still(
    session: &SftpSession,
    path: &str,
    expected: &Version,
    contents: bool,
    cancel: &CancellationToken,
) -> Result<(), Failure> {
    let name = file_name(path);
    let changed = || {
        Failure::Changed(format!(
            "“{name}” changed on the server since you opened it."
        ))
    };
    let meta = match session.metadata(path).await {
        Ok(meta) => meta,
        Err(SftpError::Status(s)) if s.status_code == StatusCode::NoSuchFile => {
            return Err(Failure::Changed(format!(
                "“{name}” is no longer on the server."
            )));
        }
        Err(e) => {
            return Err(Failure::Error(format!(
                "Cannot check “{name}”: {}",
                describe(e)
            )));
        }
    };
    if Stamp::of(&meta) != expected.stamp {
        return Err(changed());
    }
    if !contents || meta.len() > COMPARE_LIMIT {
        return Ok(());
    }
    let fail = |e: &dyn std::fmt::Display| Failure::Error(format!("Cannot check “{name}”: {e}"));
    let mut file = session.open(path).await.map_err(|e| fail(&describe(e)))?;
    let mut hash = DefaultHasher::new();
    let mut buffer = vec![0u8; CHUNK];
    loop {
        let read = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(Failure::Cancelled),
            read = file.read(&mut buffer) => read,
        };
        match read.map_err(|e| fail(&e))? {
            0 => break,
            n => hash.write(&buffer[..n]),
        }
    }
    if hash.finish() != expected.contents {
        return Err(changed());
    }
    Ok(())
}

/// How carefully an upload replaces an existing file.
#[derive(Clone, Copy)]
struct Checks<'a> {
    /// Replace a file already there; otherwise stop at one, even one that appears while the
    /// transfer runs.
    replace: bool,
    /// The version the file must still be when the upload takes its place.
    unchanged: Option<&'a Version>,
    /// Write over the file directly where its folder takes no new files.
    in_place: bool,
}

/// Uploads one file, returning a fingerprint of what it sent.
async fn upload_file(
    session: &SftpSession,
    local: &Path,
    remote: &str,
    checks: Checks<'_>,
    cancel: &CancellationToken,
    progress: &mut Progress<'_>,
) -> Result<u64, Failure> {
    let unchanged = checks.unchanged;
    let fail = |e: &dyn std::fmt::Display| Failure::Error(format!("{}: {e}", file_name(remote)));
    let taken = || {
        Failure::Exists(format!(
            "“{}” already exists on the server.",
            file_name(remote)
        ))
    };
    let mut source = tokio::fs::File::open(local).await.map_err(|e| fail(&e))?;
    let meta = match session.metadata(remote).await {
        Ok(meta) => meta,
        // A new file is created exclusively, so one that appeared meanwhile is never
        // replaced, and only what this upload created is removed if it fails.
        Err(SftpError::Status(s)) if s.status_code == StatusCode::NoSuchFile => {
            let flags = OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE;
            let mut target = match session.open_with_flags(remote, flags).await {
                Ok(target) => target,
                Err(e) => {
                    if exists(session, remote).await.unwrap_or(false) {
                        return Err(taken());
                    }
                    return Err(fail(&describe(e)));
                }
            };
            let mut hash = DefaultHasher::new();
            let copied = match copy(&mut source, &mut target, cancel, progress, &mut hash).await {
                Ok(()) => target.shutdown().await.map_err(|e| fail(&e)),
                Err(e) => Err(e),
            };
            if copied.is_err() {
                // Leave no partial file behind.
                drop(target);
                let _ = session.remove_file(remote).await;
            }
            return copied.map(|()| hash.finish());
        }
        Err(e) => return Err(fail(&describe(e))),
    };
    if file_type(&meta) != TYPE_FILE {
        return Err(fail(&"A folder or other item already has this name."));
    }
    if !checks.replace {
        return Err(taken());
    }
    // A file being replaced keeps its contents until its replacement is complete.
    // Through a link, the file it points to is replaced and the link stays.
    let real = session
        .canonicalize(remote)
        .await
        .unwrap_or_else(|_| remote.into());
    if let Some((temp, mut target)) = stage(session, &real).await {
        let mut hash = DefaultHasher::new();
        let mut copied = match copy(&mut source, &mut target, cancel, progress, &mut hash).await {
            Ok(()) => target.shutdown().await.map_err(|e| fail(&e)),
            Err(e) => Err(e),
        };
        drop(target);
        // Someone may have saved the file while it uploaded.
        if copied.is_ok()
            && let Some(expected) = unchanged
        {
            copied = still(session, &real, expected, true, cancel).await;
        }
        // The staged file is private until it takes the original's place, and then has
        // its permissions; without them it does not take it.
        if copied.is_ok()
            && let Some(mode) = meta.permissions
        {
            let permissions = Metadata {
                permissions: Some(mode & 0o7777),
                ..Metadata::empty()
            };
            copied = session
                .set_metadata(temp.clone(), permissions)
                .await
                .map_err(|e| fail(&format!("Cannot set its permissions: {}", describe(e))));
        }
        if copied.is_ok() {
            copied = replace(session, &temp, &real).await.map_err(|e| fail(&e));
        } else {
            let _ = session.remove_file(temp).await;
        }
        return copied.map(|()| hash.finish());
    }
    // A folder that takes no new files may still let its files be overwritten in place,
    // though an interrupted upload then leaves the file half written, so only when asked.
    if !checks.in_place {
        return Err(Failure::InPlace(
            "The folder takes no new files, which replacing it safely needs.".into(),
        ));
    }
    if let Some(expected) = unchanged {
        still(session, remote, expected, true, cancel).await?;
    }
    let mut hash = DefaultHasher::new();
    let mut target = session
        .open_with_flags(remote, OpenFlags::TRUNCATE | OpenFlags::WRITE)
        .await
        .map_err(|e| fail(&describe(e)))?;
    // The file was there before, so a failure leaves it as far as it got, never removed.
    match copy(&mut source, &mut target, cancel, progress, &mut hash).await {
        Ok(()) => target.shutdown().await.map_err(|e| fail(&e))?,
        Err(e) => return Err(e),
    }
    Ok(hash.finish())
}

/// Creates a new file beside `path`, readable only by its owner, to write its replacement
/// into, or `None` where the folder takes no new files.
async fn stage(session: &SftpSession, path: &str) -> Option<(String, File)> {
    let temp = join(&parent(path)?, &crate::model::temp_name(file_name(path)));
    let private = Metadata {
        permissions: Some(0o600),
        ..Metadata::empty()
    };
    let file = session
        .open_with_flags_and_attributes(
            temp.clone(),
            OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
            private,
        )
        .await
        .ok()?;
    Some((temp, file))
}

/// Moves the complete file `temp` over `path`. SFTP servers commonly refuse to rename onto an
/// existing file; then `path` is first moved aside to a backup, which takes its place again
/// if the move still fails, and stays, with the original contents, if even that fails.
async fn replace(session: &SftpSession, temp: &str, path: &str) -> Result<(), String> {
    match session.rename(temp, path).await {
        Ok(()) => return Ok(()),
        Err(SftpError::Status(s)) if s.status_code == StatusCode::Failure => {}
        Err(e) => {
            let _ = session.remove_file(temp).await;
            return Err(format!("Cannot replace it: {}", describe(e)));
        }
    }
    let backup = join(
        &parent(path).unwrap_or_default(),
        &crate::model::temp_name(file_name(path)),
    );
    if let Err(e) = session.rename(path, backup.as_str()).await {
        let _ = session.remove_file(temp).await;
        return Err(format!("Cannot replace it: {}", describe(e)));
    }
    match session.rename(temp, path).await {
        Ok(()) => {
            let _ = session.remove_file(backup).await;
            Ok(())
        }
        Err(e) => {
            let reason = describe(e);
            if session.rename(backup.as_str(), path).await.is_ok() {
                let _ = session.remove_file(temp).await;
                Err(format!("Cannot replace it: {reason}"))
            } else {
                Err(format!(
                    "Cannot replace it: {reason} The original is kept as {}, the upload as {}.",
                    file_name(&backup),
                    file_name(temp)
                ))
            }
        }
    }
}

/// Downloads one file, returning the version it was: how it looked when it was opened, and a
/// fingerprint of what was read. Unless `replace`, a file that appeared at `local` meanwhile
/// stops it.
async fn download_file(
    session: &SftpSession,
    remote: &str,
    local: &Path,
    destination: (&DownloadRoot, bool),
    cancel: &CancellationToken,
    progress: &mut Progress<'_>,
) -> Result<Option<Version>, Failure> {
    let (destination, replace) = destination;
    let fail = |e: &dyn std::fmt::Display| Failure::Error(format!("{}: {e}", file_name(remote)));
    let mut source = session.open(remote).await.map_err(|e| fail(&describe(e)))?;
    let meta = source.metadata().await.ok();
    let modified = meta.as_ref().and_then(|m| m.modified().ok());
    // The file appears under its own name only once it is complete.
    let (publication, file) = destination.create(local).map_err(|e| fail(&e))?;
    let mut target = tokio::fs::File::from_std(file);
    let mut hash = DefaultHasher::new();
    let mut copied = copy(&mut source, &mut target, cancel, progress, &mut hash).await;
    if copied.is_ok() {
        copied = target.flush().await.map_err(|e| fail(&e));
    }
    let target = target.into_std().await;
    if copied.is_ok() {
        if let Some(modified) = modified {
            let _ = target.set_modified(modified);
        }
        drop(target);
        copied = publication.publish(replace).map_err(|e| {
            if !replace && e.kind() == std::io::ErrorKind::AlreadyExists {
                Failure::Exists(format!(
                    "“{}” already exists in {}.",
                    local.file_name().unwrap_or_default().to_string_lossy(),
                    local.parent().unwrap_or(local).display()
                ))
            } else {
                fail(&e)
            }
        });
    } else {
        drop(target);
    }
    copied.map(|()| {
        meta.as_ref().map(|m| Version {
            stamp: Stamp::of(m),
            contents: hash.finish(),
        })
    })
}

/// Copies `reader` to `writer` through one buffer, stopping promptly when cancelled, and
/// fingerprints what passes into `hash`.
async fn copy(
    reader: &mut (impl AsyncRead + Unpin),
    writer: &mut (impl AsyncWrite + Unpin),
    cancel: &CancellationToken,
    progress: &mut Progress<'_>,
    hash: &mut DefaultHasher,
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
        hash.write(&buffer[..n]);
        progress.add(n as u64);
    }
}
