//! Crash-safe file replacement.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

/// Replace `target` with `contents` atomically.
///
/// 1. write a temp file in the same directory (same filesystem, so the final
///    `rename` is atomic), with the target's permissions (or `0600` for a new
///    file), and `fsync` it;
/// 2. run `check` on the temp file — e.g. validation and backup. If it
///    fails, the temp file is deleted and `target` is untouched;
/// 3. rename over `target`, then `fsync` the directory so the rename itself
///    survives a crash.
///
/// At every instant `target` holds either the complete old or the complete
/// new contents; a reader never sees a partial file.
///
/// # Errors
/// Any I/O error, or the error returned by `check`.
pub fn write_atomic<E: From<io::Error>>(
    target: &Path,
    contents: &[u8],
    check: impl FnOnce(&Path) -> Result<(), E>,
) -> Result<(), E> {
    let dir = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    let mut tmp = tempfile::Builder::new()
        .prefix(".sshcm-")
        .suffix(".tmp")
        .tempfile_in(dir)?;
    tmp.write_all(contents)?;
    copy_permissions(target, tmp.as_file())?;
    tmp.as_file().sync_all()?;

    check(tmp.path())?;

    tmp.persist(target).map_err(|e| e.error)?;
    sync_dir(dir)?;
    Ok(())
}

fn copy_permissions(target: &Path, file: &File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = match fs::metadata(target) {
        Ok(m) => m.permissions().mode() & 0o7777,
        Err(e) if e.kind() == io::ErrorKind::NotFound => 0o600,
        Err(e) => return Err(e),
    };
    file.set_permissions(fs::Permissions::from_mode(mode))
}

fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(dir: &Path) -> Vec<String> {
        let mut v: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn replaces_contents() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config");
        fs::write(&p, "old").unwrap();
        write_atomic::<io::Error>(&p, b"new", |_| Ok(())).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "new");
        assert_eq!(entries(dir.path()), ["config"]);
    }

    #[test]
    fn failed_check_leaves_target_untouched_and_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config");
        fs::write(&p, "original").unwrap();

        let mut seen = String::new();
        let err = write_atomic(&p, b"replacement", |tmp| {
            seen = fs::read_to_string(tmp).unwrap();
            Err(io::Error::other("injected failure"))
        })
        .unwrap_err();

        assert_eq!(err.to_string(), "injected failure");
        assert_eq!(seen, "replacement", "check sees the complete new contents");
        assert_eq!(fs::read_to_string(&p).unwrap(), "original");
        assert_eq!(entries(dir.path()), ["config"]);
    }

    #[test]
    fn preserves_mode_and_defaults_new_files_to_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;

        let existing = dir.path().join("existing");
        fs::write(&existing, "x").unwrap();
        fs::set_permissions(&existing, fs::Permissions::from_mode(0o640)).unwrap();
        write_atomic::<io::Error>(&existing, b"y", |_| Ok(())).unwrap();
        assert_eq!(mode(&existing), 0o640);

        let fresh = dir.path().join("fresh");
        write_atomic::<io::Error>(&fresh, b"y", |_| Ok(())).unwrap();
        assert_eq!(mode(&fresh), 0o600);
    }
}
