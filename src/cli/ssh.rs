//! Running OpenSSH's own config parser, `ssh -G -F <file> -- <host>`.
//!
//! `ssh -G` prints the effective configuration for a host without
//! connecting. sshcm uses it twice: to show ssh's own answer
//! (`resolve --via-ssh`), and to check an edited config before it is
//! written (see `mutate::transaction`).

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

/// An `ssh -G` invocation for `host` against the config at `file`. stdin is
/// closed so ssh can never stop to prompt.
fn ssh_g(file: &Path, host: &str) -> Command {
    let mut cmd = Command::new("ssh");
    cmd.arg("-G")
        .arg("-F")
        .arg(file)
        .arg("--") // a host starting with `-` must not be read as an option
        .arg(host)
        .stdin(Stdio::null());
    cmd
}

/// The effective configuration ssh would use for `host`, as printed by
/// `ssh -G`, including ssh's built-in defaults.
pub fn effective_config(file: &Path, host: &str) -> Result<String> {
    let output = ssh_g(file, host)
        .output()
        .context("running `ssh -G` (is OpenSSH installed?)")?;
    if !output.status.success() {
        bail!(
            "`ssh -G` failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout).context("`ssh -G` printed non-UTF-8 output")
}

/// Whether ssh accepts the config at `file`:
/// `Ok(Ok(()))` if it does, `Ok(Err(message))` if it rejects it, and `Err`
/// if ssh could not be run at all.
pub fn check(file: &Path, host: &str) -> std::io::Result<Result<(), String>> {
    let out = ssh_g(file, host).stdout(Stdio::null()).output()?;
    if out.status.success() {
        return Ok(Ok(()));
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    // ssh names the file it read, which here is our temp file; show a
    // neutral name instead. The pseudo-terminal notice is noise.
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
