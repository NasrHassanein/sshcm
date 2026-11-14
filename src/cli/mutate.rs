//! The write path shared by every mutating command:
//!
//! lock → read → edit → diff → confirm → (`ssh -G` on temp file) → backup →
//! atomic rename → prune old backups.

use std::fmt::Write as _;
use std::io::{BufRead, IsTerminal, Write as _};
use std::path::Path;
use std::process::{Command as Process, Stdio};
use std::time::Duration;

use anyhow::{Context, Result};
use similar::TextDiff;
use sshcm::edit::EditError;
use sshcm::io::{self, backup, lock};
use sshcm::syntax::Document;

use super::args::WriteOpts;

/// How long to wait for another sshcm process to finish.
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// OpenSSH rejected the edited config; nothing was written.
#[derive(Debug, thiserror::Error)]
#[error(
    "ssh rejected the edited config, so nothing was written:\n  {0}\n(use --no-validate to write anyway)"
)]
pub struct ValidationFailed(pub String);

/// Apply `edit` to the config at `path` and write the result safely.
///
/// `probe_host` is passed to `ssh -G` for validation; it should be the alias
/// the edit concerns, so `Match` blocks are exercised realistically.
pub fn transaction(
    path: &Path,
    opts: &WriteOpts,
    probe_host: &str,
    edit: impl FnOnce(&mut Document) -> Result<(), EditError>,
) -> Result<()> {
    let real = io::real_path(path).with_context(|| format!("resolving {}", path.display()))?;
    io::create_private_dir(&io::state_dir(&real))?;
    let _lock = lock::acquire(&io::lock_path(&real), LOCK_TIMEOUT)?;

    let (original, existed) = match std::fs::read_to_string(&real) {
        Ok(s) => (s, true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (String::new(), false),
        Err(e) => return Err(e).with_context(|| format!("reading {}", real.display())),
    };

    let mut doc = Document::parse(&original);
    edit(&mut doc)?;
    let updated = doc.render();

    if updated == original {
        eprintln!("No changes.");
        return Ok(());
    }

    print!("{}", diff(&original, &updated, &path.display().to_string()));
    if opts.dry_run {
        eprintln!("Dry run: nothing written.");
        return Ok(());
    }
    if !opts.yes && std::io::stdin().is_terminal() && !confirm(path)? {
        eprintln!("Cancelled: nothing written.");
        return Ok(());
    }

    let mut backup_path = None;
    io::write_atomic(&real, updated.as_bytes(), |tmp| -> Result<()> {
        if !opts.no_validate {
            validate(tmp, &real, existed, probe_host)?;
        }
        if existed && !opts.no_backup {
            backup_path =
                Some(backup::create(&real, original.as_bytes()).context("saving backup")?);
        }
        Ok(())
    })?;

    if backup_path.is_some() {
        backup::prune(&real, backup::DEFAULT_KEEP).context("pruning old backups")?;
    }
    match backup_path {
        Some(b) => eprintln!("Updated {} (backup: {})", path.display(), b.display()),
        None => eprintln!("Updated {}", path.display()),
    }
    Ok(())
}

/// Run `ssh -G` on the candidate. A rejection is only fatal if the original
/// file was accepted: we must not block every edit of an already-broken file.
/// Run `ssh -G` on the candidate. If ssh rejects it, check the original
/// too, purely to give a better message: editing a file ssh already rejects
/// (or that is not an ssh config at all) is refused either way, and
/// `--no-validate` is the explicit override.
fn validate(candidate: &Path, original: &Path, existed: bool, host: &str) -> Result<()> {
    match ssh_check(candidate, host) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(msg)) => {
            if existed && matches!(ssh_check(original, host), Ok(Err(_))) {
                Err(ValidationFailed(format!(
                    "ssh already rejects {} before this edit; run `sshcm lint` to see why\n  {msg}",
                    original.display()
                ))
                .into())
            } else {
                Err(ValidationFailed(msg).into())
            }
        }
        Err(e) => {
            eprintln!(
                "warning: could not run `ssh -G` to validate ({e}); writing without validation"
            );
            Ok(())
        }
    }
}

/// `Ok(Ok(()))` if ssh accepts the file, `Ok(Err(stderr))` if it rejects it,
/// `Err` if ssh could not be run at all.
pub fn ssh_check(file: &Path, host: &str) -> std::io::Result<Result<(), String>> {
    let out = Process::new("ssh")
        .arg("-G")
        .arg("-F")
        .arg(file)
        .arg("--")
        .arg(host)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .output()?;
    if out.status.success() {
        Ok(Ok(()))
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // ssh names the file it read; show the user's path, not our temp file.
        let msg = stderr
            .lines()
            .filter(|l| !l.starts_with("Pseudo-terminal"))
            .collect::<Vec<_>>()
            .join("\n  ")
            .replace(&file.display().to_string(), "<edited config>");
        Ok(Err(if msg.is_empty() {
            format!("ssh exited with {}", out.status)
        } else {
            msg
        }))
    }
}

fn confirm(path: &Path) -> Result<bool> {
    eprint!("Apply these changes to {}? [y/N] ", path.display());
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

/// A unified diff, colored when stdout is a terminal and `NO_COLOR` is unset.
pub fn diff(old: &str, new: &str, name: &str) -> String {
    let color = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let text = TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(name, name)
        .to_string();
    if !color {
        return text;
    }
    let mut out = String::with_capacity(text.len() + 256);
    for line in text.split_inclusive('\n') {
        let code = match line.as_bytes().first() {
            _ if line.starts_with("---") || line.starts_with("+++") => "1",
            Some(b'+') => "32",
            Some(b'-') => "31",
            Some(b'@') => "36",
            _ => "",
        };
        if code.is_empty() {
            out.push_str(line);
        } else {
            let body = line.trim_end_matches(['\n', '\r']);
            let _ = write!(out, "\x1b[{code}m{body}\x1b[0m{}", &line[body.len()..]);
        }
    }
    out
}
