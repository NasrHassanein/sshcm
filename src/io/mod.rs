//! Safe file I/O for config edits: path resolution, locking, backups, and
//! atomic replacement.
//!
//! State (lock files, backups) lives in a private `.sshcm/` directory next to
//! the config's *real* location.

pub mod atomic;
pub mod backup;
pub mod lock;

use std::fs::{self, DirBuilder};
use std::io;
use std::path::{Path, PathBuf};

pub use atomic::write_atomic;

/// True if `path` can be written by its group or by others (mode `0o022`).
/// ssh refuses such config files with "Bad owner or permissions". A file
/// whose metadata can't be read counts as not writable, because there is
/// nothing to report about it.
pub fn writable_by_others(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o022 != 0)
}

/// Where sshcm keeps state for `config` (a resolved, real path).
pub fn state_dir(config: &Path) -> PathBuf {
    config
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".sshcm")
}

/// The lock file guarding `config` (a resolved, real path).
pub fn lock_path(config: &Path) -> PathBuf {
    let name = config
        .file_name()
        .map_or_else(|| "config".into(), |n| n.to_string_lossy().into_owned());
    state_dir(config).join(format!("{name}.lock"))
}

/// Create `dir` (and parents) with mode `0700` where supported.
///
/// # Errors
/// Any I/O error.
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut b = DirBuilder::new();
    b.recursive(true);
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(dir)
}

/// The file an edit of `path` must actually write.
///
/// Symlinks are followed, so editing a symlinked config (e.g. into a
/// dotfiles repo) rewrites the target and keeps the link. A path that does
/// not exist yet resolves to itself, with its directory created (`0700`).
///
/// # Errors
/// A dangling symlink, or any I/O error.
pub fn real_path(path: &Path) -> io::Result<PathBuf> {
    match fs::canonicalize(path) {
        Ok(p) => Ok(p),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if fs::symlink_metadata(path).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("{} is a dangling symlink", path.display()),
                ));
            }
            let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
            let parent = parent.unwrap_or_else(|| Path::new("."));
            create_private_dir(parent)?;
            let name = path.file_name().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "path has no file name")
            })?;
            Ok(fs::canonicalize(parent)?.join(name))
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_path_follows_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("dotfiles-config");
        let link = dir.path().join("config");
        fs::write(&target, "x").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert_eq!(
            real_path(&link).unwrap(),
            fs::canonicalize(&target).unwrap()
        );

        let dangling = dir.path().join("dangling");
        std::os::unix::fs::symlink(dir.path().join("missing"), &dangling).unwrap();
        assert!(real_path(&dangling).is_err());
    }

    #[test]
    fn real_path_of_new_file_creates_parent() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("new-ssh-dir").join("config");
        let real = real_path(&p).unwrap();
        assert!(real.parent().unwrap().is_dir());
        assert_eq!(real.file_name().unwrap(), "config");
    }
}
