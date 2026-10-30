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
        match self.dir.hard_link(&self.partial, &self.dir, &self.name) {
            Ok(()) => {
                let _ = self.dir.remove_file(&self.partial);
                Ok(())
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(e),
            Err(_) if self.dir.symlink_metadata(&self.name).is_ok() => {
                Err(io::Error::from(io::ErrorKind::AlreadyExists))
            }
            Err(_) => self.dir.rename(&self.partial, &self.dir, &self.name),
        }
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
