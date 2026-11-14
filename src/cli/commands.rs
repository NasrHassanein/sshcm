//! Subcommand implementations. Each takes parsed input and returns the text
//! to print, so they can be unit-tested without touching stdout.

use std::fmt::Write as _;
use std::path::Path;
use std::process::{Command as Process, Stdio};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use sshcm::diagnostic::Diagnostic;
use sshcm::hosts::{find_alias_in, host_entries_in};
use sshcm::include::ConfigSet;
use sshcm::model::pattern::wildcard_match;
use sshcm::model::value::quote;
use sshcm::resolve::{ResolveContext, resolve as resolve_host};
use sshcm::syntax::{BlockKind, Document, Line, LineKind};

use super::{NotFound, table};

const NONE: &str = "-";

/// `path` for display: the home directory abbreviated to `~`.
pub fn display(path: &Path) -> String {
    if let Some(home) = home::home_dir()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return Path::new("~").join(rest).display().to_string();
    }
    path.display().to_string()
}

/// Diagnostics as `file:line: message` lines.
pub fn format_diagnostics<'a>(diags: impl IntoIterator<Item = &'a Diagnostic>) -> Vec<String> {
    diags
        .into_iter()
        .map(|d| format!("{}:{}: {}", display(&d.file), d.line, d.message))
        .collect()
}

pub fn list(set: &ConfigSet, pattern: Option<&str>, json: bool) -> Result<String> {
    let entries: Vec<_> = host_entries_in(set)
        .into_iter()
        .filter(|e| pattern.is_none_or(|g| e.entry.patterns.iter().any(|p| wildcard_match(g, p))))
        .collect();

    if json {
        return Ok(serde_json::to_string_pretty(&entries)? + "\n");
    }
    let multi_file = set.files().len() > 1;
    let rows: Vec<Vec<String>> = entries
        .into_iter()
        .map(|located| {
            let e = located.entry;
            let or_none = |v: Option<String>| v.unwrap_or_else(|| NONE.to_owned());
            let identity = match e.identity_files.as_slice() {
                [] => NONE.to_owned(),
                [one] => one.clone(),
                [first, rest @ ..] => format!("{first} (+{})", rest.len()),
            };
            let source = if multi_file {
                format!("{}:{}", display(&located.file), e.line)
            } else {
                e.line.to_string()
            };
            vec![
                e.patterns.join(" "),
                or_none(e.hostname),
                or_none(e.user),
                or_none(e.port),
                identity,
                source,
            ]
        })
        .collect();
    let last = if multi_file { "SOURCE" } else { "LINE" };
    Ok(table::render(
        &["HOST", "HOSTNAME", "USER", "PORT", "IDENTITY", last],
        &rows,
    ))
}

#[derive(Serialize)]
struct ShownBlock {
    file: String,
    line: usize,
    text: String,
}

pub fn show(set: &ConfigSet, alias: &str, json: bool) -> Result<String> {
    let found = find_alias_in(set, alias);
    if found.is_empty() {
        return Err(NotFound(alias.to_owned()).into());
    }

    let shown: Vec<ShownBlock> = found
        .iter()
        .map(|&(fid, bi)| {
            let file = &set.files()[fid];
            let block = &file.doc.blocks()[bi];
            let lines = file.doc.block_lines(block);
            // Trailing comments/blank lines usually introduce the *next*
            // block, so leave them out of the display.
            let keep = lines
                .iter()
                .rposition(|l| !matches!(l.kind(), LineKind::Blank | LineKind::Comment { .. }))
                .map_or(0, |p| p + 1);
            ShownBlock {
                file: display(&file.path),
                line: block.lines().start + 1,
                text: lines[..keep].iter().map(Line::to_string).collect(),
            }
        })
        .collect();

    if json {
        return Ok(serde_json::to_string_pretty(&shown)? + "\n");
    }
    let mut out = String::new();
    if shown.len() > 1 {
        let _ = writeln!(
            out,
            "# note: `{alias}` appears in {} Host blocks; for any option set in more than one, the first wins",
            shown.len()
        );
    }
    for (n, b) in shown.iter().enumerate() {
        if n > 0 {
            out.push('\n');
        }
        let _ = writeln!(out, "# {}:{}", b.file, b.line);
        out.push_str(&b.text);
        if !b.text.ends_with('\n') {
            out.push('\n');
        }
    }
    Ok(out)
}

pub fn resolve(
    set: &ConfigSet,
    host: &str,
    local_user: &str,
    json: bool,
) -> Result<(String, Vec<String>)> {
    let ctx = ResolveContext {
        local_user: local_user.to_owned(),
    };
    let res = resolve_host(set, host, &ctx);
    let warnings = format_diagnostics(res.warnings.iter());

    if json {
        return Ok((serde_json::to_string_pretty(&res)? + "\n", warnings));
    }
    let root = &set.root().path;
    let rows: Vec<Vec<String>> = res
        .settings
        .iter()
        .map(|s| {
            let args: Vec<_> = s.args.iter().map(|a| quote(a)).collect();
            let source = if &s.file == root {
                format!("# line {}", s.line)
            } else {
                format!("# {}:{}", display(&s.file), s.line)
            };
            vec![s.keyword.clone(), args.join(" "), source]
        })
        .collect();
    let out = if rows.is_empty() {
        format!("# no settings in this configuration apply to `{host}`\n")
    } else {
        // Headerless table: drop the (empty) header row.
        let t = table::render(&["", "", ""], &rows);
        t.split_once('\n')
            .map_or(t.clone(), |(_, rest)| rest.to_owned())
    };
    Ok((out, warnings))
}

/// Run `ssh -G` and return its stdout.
pub fn resolve_via_ssh(path: &Path, host: &str) -> Result<String> {
    let output = Process::new("ssh")
        .arg("-G")
        .arg("-F")
        .arg(path)
        .arg("--")
        .arg(host)
        .stdin(Stdio::null())
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

pub fn dump_syntax(doc: &Document) -> String {
    let mut out = String::new();
    for block in doc.blocks() {
        let kind = match block.kind() {
            BlockKind::Global => "Global",
            BlockKind::Host => "Host",
            BlockKind::Match => "Match",
        };
        let r = block.lines();
        let _ = if r.is_empty() {
            writeln!(out, "{kind} (empty)")
        } else {
            writeln!(out, "{kind} (lines {}-{})", r.start + 1, r.end)
        };
        for (i, line) in doc.block_lines(block).iter().enumerate() {
            let tag = match line.kind() {
                LineKind::Blank => "blank".to_owned(),
                LineKind::Comment { .. } => "comment".to_owned(),
                LineKind::Invalid => "INVALID".to_owned(),
                LineKind::Directive(_) => format!(
                    "{}  sep={:?}  value={:?}",
                    line.keyword().unwrap_or_default(),
                    line.separator().unwrap_or_default(),
                    line.value().unwrap_or_default()
                ),
            };
            let _ = writeln!(out, "  {:>4} {tag}", r.start + i + 1);
        }
    }
    out
}
