//! Timestamped backups of the config, taken before every write.
//!
//! Backups live in `<config dir>/.sshcm/backups/<file>.<UTC timestamp>.bak`
//! (directory mode `0700`, files `0600`). Names sort chronologically, so the
//! newest backup is always the last in lexical order.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// How many backups `prune` keeps per config file by default.
pub const DEFAULT_KEEP: usize = 20;

/// The backup directory for `config` (a resolved, real path).
pub fn dir(config: &Path) -> PathBuf {
    super::state_dir(config).join("backups")
}

fn file_name(config: &Path) -> String {
    config
        .file_name()
        .map_or_else(|| "config".to_owned(), |n| n.to_string_lossy().into_owned())
}

/// Write `contents` as a new backup of `config`; returns the backup's path.
///
/// # Errors
/// Any I/O error creating the directory or file.
pub fn create(config: &Path, contents: &[u8]) -> io::Result<PathBuf> {
    let dir = dir(config);
    super::create_private_dir(&dir)?;
    let name = file_name(config);

    // Microsecond timestamps make collisions practically impossible; if one
    // happens anyway, `create_new` refuses and we retry with a fresh stamp.
    loop {
        let stamp: String = humantime::format_rfc3339_micros(SystemTime::now())
            .to_string()
            .chars()
            .filter(|c| !matches!(c, '-' | ':'))
            .collect();
        let path = dir.join(format!("{name}.{stamp}.bak"));
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        match opts.open(&path) {
            Ok(mut f) => {
                f.write_all(contents)?;
                f.sync_all()?;
                return Ok(path);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
}

/// Backups of `config`, oldest first.
///
/// # Errors
/// Any I/O error other than the directory not existing yet.
pub fn list(config: &Path) -> io::Result<Vec<PathBuf>> {
    let prefix = format!("{}.", file_name(config));
    let mut out: Vec<PathBuf> = match fs::read_dir(dir(config)) {
        Ok(rd) => rd
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                    n.starts_with(&prefix)
                        && Path::new(n)
                            .extension()
                            .is_some_and(|e| e.eq_ignore_ascii_case("bak"))
                        && n[prefix.len()..].starts_with(|c: char| c.is_ascii_digit())
                })
            })
            .collect(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    out.sort();
    Ok(out)
}

/// Delete all but the newest `keep` backups of `config`. Count-based rather
/// than age-based: a config edited rarely still keeps its history, and a
/// burst of edits cannot fill the disk. Returns the deleted paths.
///
/// # Errors
/// Any I/O error listing or deleting.
pub fn prune(config: &Path, keep: usize) -> io::Result<Vec<PathBuf>> {
    let all = list(config)?;
    let excess = all.len().saturating_sub(keep);
    let doomed = all[..excess].to_vec();
    for p in &doomed {
        fs::remove_file(p)?;
    }
    Ok(doomed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_list_prune() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let other = dir.path().join("config.d");

        let b: Vec<_> = (0..5)
            .map(|i| create(&config, format!("v{i}").as_bytes()).unwrap())
            .collect();
        create(&other, b"not ours").unwrap();

        assert_eq!(
            list(&config).unwrap(),
            b,
            "sorted oldest first, other files excluded"
        );
        assert_eq!(fs::read_to_string(&b[4]).unwrap(), "v4");

        assert_eq!(prune(&config, 2).unwrap(), b[..3]);
        assert_eq!(list(&config).unwrap(), b[3..]);
        assert_eq!(list(&other).unwrap().len(), 1);
    }

    #[test]
    fn list_without_directory_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            list(&dir.path().join("config")).unwrap(),
            Vec::<PathBuf>::new()
        );
    }

    #[test]
    fn backups_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let b = create(&dir.path().join("config"), b"secret").unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&b), 0o600);
        assert_eq!(mode(b.parent().unwrap()), 0o700);
    }
}
