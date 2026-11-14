//! Advisory locking so two `sshcm` processes never interleave a
//! read-modify-write of the same config.
//!
//! The lock is taken on a separate lock file, not on the config itself:
//! [`super::write_atomic`] replaces the config by renaming a new inode over
//! it, so a lock held on the old inode would protect nothing.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A held lock; released when dropped.
#[derive(Debug)]
pub struct ConfigLock {
    _file: File,
}

/// The lock could not be taken.
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    /// Another process held the lock for the whole timeout.
    #[error("another sshcm process is editing this config (lock {0} busy for {1:?})")]
    Timeout(PathBuf, Duration),
    /// The lock file could not be created or locked.
    #[error("cannot lock {0}: {1}")]
    Io(PathBuf, #[source] io::Error),
}

/// Take an exclusive lock on `lock_path`, waiting up to `timeout`.
///
/// # Errors
/// [`LockError::Timeout`] if the lock stays busy; [`LockError::Io`] otherwise.
pub fn acquire(lock_path: &Path, timeout: Duration) -> Result<ConfigLock, LockError> {
    let io_err = |e| LockError::Io(lock_path.to_owned(), e);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path)
        .map_err(io_err)?;

    let start = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(ConfigLock { _file: file }),
            Err(TryLockError::WouldBlock) if start.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(TryLockError::WouldBlock) => {
                return Err(LockError::Timeout(lock_path.to_owned(), timeout));
            }
            Err(TryLockError::Error(e)) => return Err(io_err(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_lock_times_out_until_first_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");
        let short = Duration::from_millis(120);

        let first = acquire(&path, short).unwrap();
        assert!(matches!(acquire(&path, short), Err(LockError::Timeout(..))));
        drop(first);
        assert!(acquire(&path, short).is_ok());
    }
}
