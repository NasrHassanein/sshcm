//! Timestamped backups of the config, taken before every write.
//!
//! Backups live in `<config dir>/.sshcm/backups/<file>.<UTC timestamp>.bak`
//! (directory mode `0700`, files `0600`). Names sort chronologically, so the
//! newest backup is always the last in lexical order.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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
        let stamp = utc_stamp(SystemTime::now());
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

/// `time` as a compact UTC timestamp, `YYYYMMDDTHHMMSS.ffffffZ` (RFC 3339
/// with microseconds, minus the `-` and `:` that are awkward in file
/// names). Fixed width, so lexical order is chronological order. Times
/// before 1970 (a badly wrong clock) are written as the epoch.
fn utc_stamp(time: SystemTime) -> String {
    let since_epoch = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = since_epoch.as_secs();
    let (year, month, day) = civil_from_days(secs / 86_400);
    let rem = secs % 86_400;
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}.{:06}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        since_epoch.subsec_micros()
    )
}

/// The proleptic Gregorian `(year, month, day)` of `days` since 1970-01-01.
/// Howard Hinnant's `civil_from_days`, specialised to non-negative input:
/// it counts in 400-year eras of 146 097 days, with years starting on
/// 1 March so the leap day falls at the end of the year.
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468; // days from 0000-03-01 to 1970-01-01
    let era = z / 146_097;
    let doe = z % 146_097; // day of era, 0..=146_096
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // 0..=399
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year from 1 March
    let mp = (5 * doy + 2) / 153; // month from March, 0..=11
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = era * 400 + yoe + u64::from(month <= 2);
    (year, month, day)
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
    fn utc_stamp_format() {
        use std::time::Duration;
        let at =
            |secs: u64, micros: u32| utc_stamp(UNIX_EPOCH + Duration::new(secs, micros * 1000));
        assert_eq!(at(0, 0), "19700101T000000.000000Z");
        assert_eq!(at(951_782_400, 0), "20000229T000000.000000Z"); // leap day
        assert_eq!(at(1_700_000_000, 42), "20231114T221320.000042Z");
        assert_eq!(at(4_107_542_399, 999_999), "21000228T235959.999999Z"); // 2100 is not leap
        assert_eq!(at(4_107_542_400, 0), "21000301T000000.000000Z");
    }

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
