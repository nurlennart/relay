//! Download paths anchored to directory handles instead of names that can be swapped
//! while a remote read is in flight. cap-std confines relative operations on every OS.
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions},
};
use std::{
    ffi::{OsStr, OsString},
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
        let folder = path.parent().unwrap_or(&self.path);
        let dir = self.directory(folder, false)?;
        let partial = OsString::from(crate::model::temp_name(&name.to_string_lossy()));
        let file = dir.open_with(&partial, OpenOptions::new().create_new(true).write(true))?;
        Ok((
            DownloadTarget {
                dir,
                folder: folder.into(),
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
    /// The parent's path, for systems that rename only by path.
    folder: PathBuf,
    name: OsString,
    partial: OsString,
}

impl DownloadTarget {
    pub fn publish(&self, replace: bool) -> io::Result<()> {
        if replace {
            return self.dir.rename(&self.partial, &self.dir, &self.name);
        }
        // A hard link takes the name only while it is free, in one step.
        match self.dir.hard_link(&self.partial, &self.dir, &self.name) {
            Ok(()) => {
                let _ = self.dir.remove_file(&self.partial);
                return Ok(());
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Err(e),
            Err(_) => {}
        }
        // FAT and exFAT drives, and some network shares, have no hard links. A rename the
        // system refuses to make over an existing file is just as safe. Where neither is
        // possible, looking first and then renaming could replace a file created between
        // the two, so the download stops instead.
        rename_new(&self.dir, &self.folder, &self.partial, &self.name).map_err(|e| match e.kind() {
            io::ErrorKind::Unsupported | io::ErrorKind::InvalidInput => io::Error::new(
                io::ErrorKind::Unsupported,
                "this drive cannot add a file without risking replacing another; \
                     use Save as to choose the file to replace",
            ),
            _ => e,
        })
    }
}

/// Renames `from` to `to` in `dir` unless `to` exists, in one step: `AlreadyExists` if it
/// does, and `Unsupported` or `InvalidInput` where the system or file system cannot.
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
fn rename_new(dir: &Dir, _folder: &Path, from: &OsStr, to: &OsStr) -> io::Result<()> {
    use rustix::fs::{RenameFlags, renameat_with};
    Ok(renameat_with(dir, from, dir, to, RenameFlags::NOREPLACE)?)
}

/// Without `MOVEFILE_REPLACE_EXISTING`, Windows refuses to move onto an existing file. File
/// systems without hard links have no links or junctions either, so the path stays put.
#[cfg(windows)]
fn rename_new(_dir: &Dir, folder: &Path, from: &OsStr, to: &OsStr) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let wide = |name: &OsStr| -> Vec<u16> {
        folder
            .join(name)
            .as_os_str()
            .encode_wide()
            .chain([0])
            .collect()
    };
    let (from, to) = (wide(from), wide(to));
    // SAFETY: both are NUL-terminated wide strings that outlive the call.
    let moved = unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileExW(from.as_ptr(), to.as_ptr(), 0)
    };
    if moved == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    windows
)))]
fn rename_new(_dir: &Dir, _folder: &Path, _from: &OsStr, _to: &OsStr) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
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
        // A free name is taken, also on drives without hard links, such as FAT.
        let (target, mut file) = root.create(&base.join("free")).unwrap();
        file.write_all(b"new").unwrap();
        drop(file);
        target.publish(false).unwrap();
        drop(target);
        assert_eq!(std::fs::read(base.join("free")).unwrap(), b"new");
        assert_eq!(std::fs::read_dir(&base).unwrap().count(), 2);
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
