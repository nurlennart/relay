//! Download paths anchored to directory handles instead of names that can be swapped
//! while a remote read is in flight. cap-std confines relative operations on every OS.
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions},
};
use std::{
    ffi::OsString,
    fs::File,
    io,
    path::{Component, Path, PathBuf},
};

pub struct DownloadRoot {
    path: PathBuf,
    dir: Dir,
}

impl DownloadRoot {
    pub fn open(path: &Path) -> io::Result<Self> {
        Ok(Self {
            path: path.to_path_buf(),
            dir: Dir::open_ambient_dir(path, ambient_authority())?,
        })
    }

    /// Opens each component relative to the preceding directory. A racing symlink
    /// cannot escape that directory, even after the metadata check.
    fn directory(&self, path: &Path, create: bool) -> io::Result<Dir> {
        let below = path
            .strip_prefix(&self.path)
            .map_err(|_| io::Error::other("it is outside the chosen folder"))?;
        let mut dir = self.dir.try_clone()?;
        for part in below.components() {
            let Component::Normal(name) = part else {
                return Err(io::Error::other("invalid download folder"));
            };
            match dir.symlink_metadata(name) {
                Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
                Ok(_) => return Err(io::Error::other("a download folder is a link or file")),
                Err(e) if create && e.kind() == io::ErrorKind::NotFound => dir.create_dir(name)?,
                Err(e) => return Err(e),
            }
            dir = dir.open_dir(name)?;
        }
        Ok(dir)
    }

    pub fn create_folder(&self, path: &Path) -> io::Result<()> {
        self.directory(path, true).map(|_| ())
    }

    pub fn create(&self, path: &Path) -> io::Result<(DownloadTarget, File)> {
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::other("invalid download name"))?;
        let dir = self.directory(path.parent().unwrap_or(&self.path), false)?;
        let partial = OsString::from(crate::model::temp_name(&name.to_string_lossy()));
        let file = dir.open_with(&partial, OpenOptions::new().create_new(true).write(true))?;
        Ok((
            DownloadTarget {
                dir,
                name: name.into(),
                partial,
            },
            file.into_std(),
        ))
    }
}

/// Keeps the actual parent open through copying, publication and failure cleanup.
pub struct DownloadTarget {
    dir: Dir,
    name: OsString,
    partial: OsString,
}

impl DownloadTarget {
    pub fn publish(&self, replace: bool) -> io::Result<()> {
        if replace {
            return self.dir.rename(&self.partial, &self.dir, &self.name);
        }
        // There must be no check-then-rename fallback: on filesystems without hard
        // links, it could overwrite a file created after that check. Save as lets the
        // user explicitly authorize replacement on those filesystems instead.
        self.dir.hard_link(&self.partial, &self.dir, &self.name)
            .map_err(|e| io::Error::new(e.kind(), format!(
                "Cannot safely create this download: {e}. Use Save as to confirm its destination."
            )))?;
        let _ = self.dir.remove_file(&self.partial);
        Ok(())
    }
}

impl Drop for DownloadTarget {
    fn drop(&mut self) {
        let _ = self.dir.remove_file(&self.partial);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn a_new_download_does_not_replace_a_racing_destination() {
        let base = std::env::temp_dir().join(crate::model::temp_name("download-race"));
        std::fs::create_dir(&base).unwrap();
        let root = DownloadRoot::open(&base).unwrap();
        let path = base.join("file");
        let (target, mut file) = root.create(&path).unwrap();
        file.write_all(b"download").unwrap();
        drop(file);
        std::fs::write(&path, b"another writer").unwrap();
        assert_eq!(
            target.publish(false).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"another writer");
        drop(target);
        assert_eq!(std::fs::read_dir(&base).unwrap().count(), 1);
        drop(root);
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn publication_stays_with_the_open_parent() {
        let base = std::env::temp_dir().join(crate::model::temp_name("download-root"));
        let inside = base.join("inside");
        std::fs::create_dir_all(&inside).unwrap();
        let root = DownloadRoot::open(&base).unwrap();
        let (target, mut file) = root.create(&inside.join("file")).unwrap();
        file.write_all(b"complete").unwrap();
        drop(file);
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let outside = base.join("outside");
            std::fs::create_dir(&outside).unwrap();
            std::fs::write(outside.join("file"), "untouched").unwrap();
            std::fs::rename(&inside, base.join("original")).unwrap();
            symlink(&outside, &inside).unwrap();
            target.publish(true).unwrap();
            assert_eq!(std::fs::read(outside.join("file")).unwrap(), b"untouched");
            assert_eq!(
                std::fs::read(base.join("original/file")).unwrap(),
                b"complete"
            );
            // A swap before opening the file is refused as well.
            assert!(root.create(&inside.join("another")).is_err());
        }
        #[cfg(not(unix))]
        {
            target.publish(true).unwrap();
            assert_eq!(std::fs::read(inside.join("file")).unwrap(), b"complete");
        }
        drop(target);
        drop(root);
        std::fs::remove_dir_all(base).unwrap();
    }
}
