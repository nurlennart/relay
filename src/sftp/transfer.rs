//! Copying files and folders in either direction: each transfer is planned first, so its
//! progress has a total, then streamed through a fixed buffer into a staged file that takes
//! the destination's place only once complete.
use super::*;
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
            let mut contents = 0;
            for (from, to, _) in &plan.files {
                contents =
                    upload_file(session, from, to, unchanged.as_ref(), cancel, &mut progress)
                        .await?;
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
            let mut version = None;
            for (from, to, _) in &plan.files {
                version = download_file(session, from, to, cancel, &mut progress).await?;
                progress.file_done();
            }
            if plan.folders.is_empty() {
                return Ok(version);
            }
        }
    }
    Ok(None)
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

/// Uploads one file, returning a fingerprint of what it sent. With `unchanged`, the file it
/// replaces must still be that version when the upload takes its place.
async fn upload_file(
    session: &SftpSession,
    local: &Path,
    remote: &str,
    unchanged: Option<&Version>,
    cancel: &CancellationToken,
    progress: &mut Progress<'_>,
) -> Result<u64, Failure> {
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
            let mut hash = DefaultHasher::new();
            let mut copied = match copy(&mut source, &mut target, cancel, progress, &mut hash).await
            {
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
            return copied.map(|()| hash.finish());
        }
        // A folder that takes no new files still lets its files be overwritten in place, once
        // it is clear nobody changed it.
        if let Some(expected) = unchanged {
            still(session, remote, expected, true, cancel).await?;
        }
    }
    let mut hash = DefaultHasher::new();
    let mut target = session
        .open_with_flags(
            remote,
            OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE,
        )
        .await
        .map_err(|e| fail(&describe(e)))?;
    let copied = match copy(&mut source, &mut target, cancel, progress, &mut hash).await {
        Ok(()) => target.shutdown().await.map_err(|e| fail(&e)),
        Err(e) => Err(e),
    };
    if copied.is_err() {
        // Leave no partial file behind.
        drop(target);
        let _ = session.remove_file(remote).await;
    }
    copied.map(|()| hash.finish())
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

/// Downloads one file, returning the version it was: how it looked when it was opened, and a
/// fingerprint of what was read.
async fn download_file(
    session: &SftpSession,
    remote: &str,
    local: &Path,
    cancel: &CancellationToken,
    progress: &mut Progress<'_>,
) -> Result<Option<Version>, Failure> {
    let fail = |e: &dyn std::fmt::Display| Failure::Error(format!("{}: {e}", file_name(remote)));
    let mut source = session.open(remote).await.map_err(|e| fail(&describe(e)))?;
    let meta = source.metadata().await.ok();
    let modified = meta.as_ref().and_then(|m| m.modified().ok());
    // The file appears under its own name only once it is complete.
    let partial = partial_path(local);
    let mut target = tokio::fs::File::create_new(&partial)
        .await
        .map_err(|e| fail(&e))?;
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
        copied = tokio::fs::rename(&partial, local)
            .await
            .map_err(|e| fail(&e));
    } else {
        drop(target);
    }
    if copied.is_err() {
        let _ = tokio::fs::remove_file(&partial).await;
    }
    copied.map(|()| {
        meta.as_ref().map(|m| Version {
            stamp: Stamp::of(m),
            contents: hash.finish(),
        })
    })
}

/// Where a download is written until it is complete: beside its destination.
fn partial_path(local: &Path) -> PathBuf {
    let name = local.file_name().unwrap_or_default().to_string_lossy();
    local.with_file_name(crate::model::temp_name(&name))
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
