//! Whole-config operations: lint, fmt, export/import, backups. Each prints
//! its own output and returns an error carrying the right exit code.

use std::collections::HashSet;
use std::io::Read as _;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use sshcm::edit::{self, Position};
use sshcm::format::{FormatOptions, format};
use sshcm::hosts::find_alias_in;
use sshcm::include::ConfigSet;
use sshcm::interchange::{Export, VERSION, export};
use sshcm::io::{self, backup};
use sshcm::lint::{LintContext, Severity, lint as run_lint};
use sshcm::syntax::Document;

use super::args::WriteOpts;
use super::commands::display;
use super::mutate::{diff, transaction};

/// `lint` found problems.
#[derive(Debug, thiserror::Error)]
#[error("{errors} error(s), {warnings} warning(s)")]
pub struct LintFailed {
    pub errors: usize,
    pub warnings: usize,
}

/// `fmt --check` found files that would change.
#[derive(Debug, thiserror::Error)]
#[error("{0} file(s) would be reformatted; run `sshcm fmt` to apply")]
pub struct FmtCheckFailed(pub usize);

pub fn lint(
    set: &ConfigSet,
    local_user: &str,
    strict: bool,
    json: bool,
    file_checks: bool,
) -> Result<()> {
    let ctx = LintContext {
        home: home::home_dir().unwrap_or_default(),
        local_user: local_user.to_owned(),
        check_files: file_checks,
    };
    let findings = run_lint(set, &ctx);
    let errors = findings
        .iter()
        .filter(|f| f.severity == Severity::Error)
        .count();
    let warnings = findings.len() - errors;

    if json {
        println!("{}", serde_json::to_string_pretty(&findings)?);
    } else {
        for f in &findings {
            let place = if f.line == 0 {
                display(&f.file)
            } else {
                format!("{}:{}", display(&f.file), f.line)
            };
            let sev = match f.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
            };
            println!("{place}: {sev}[{}]: {}", f.code, f.message);
        }
        if findings.is_empty() {
            eprintln!("No problems found.");
        }
    }
    if errors > 0 || (strict && warnings > 0) {
        return Err(LintFailed { errors, warnings }.into());
    }
    if warnings > 0 && !json {
        eprintln!("{warnings} warning(s)");
    }
    Ok(())
}

pub fn fmt(
    set: &ConfigSet,
    opts: &FormatOptions,
    check: bool,
    all: bool,
    w: &WriteOpts,
) -> Result<()> {
    let targets = if all { set.files().len() } else { 1 };
    let files = &set.files()[..targets];

    if check {
        let mut changed = 0;
        for f in files {
            let old = f.doc.render();
            let new = format(&f.doc, opts);
            if new != old {
                print!("{}", diff(&old, &new, &display(&f.path)));
                changed += 1;
            }
        }
        if changed > 0 {
            return Err(FmtCheckFailed(changed).into());
        }
        eprintln!("Already formatted.");
        return Ok(());
    }
    for f in files {
        transaction(&f.path, w, "sshcm-fmt", |d| {
            *d = Document::parse(&format(d, opts));
            Ok(())
        })?;
    }
    Ok(())
}

pub fn export_json(set: &ConfigSet) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&export(set))?);
    Ok(())
}

pub fn import(
    config: &Path,
    set: &ConfigSet,
    source: &Path,
    skip_existing: bool,
    w: &WriteOpts,
) -> Result<()> {
    let text = if source == Path::new("-") {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .context("reading standard input")?;
        s
    } else {
        std::fs::read_to_string(source).with_context(|| format!("reading {}", source.display()))?
    };
    let data: Export = serde_json::from_str(&text).context("parsing import JSON")?;
    if data.version != VERSION {
        bail!(
            "unsupported export version {} (expected {VERSION})",
            data.version
        );
    }

    let existing: HashSet<Vec<String>> = export(set).hosts.into_iter().map(|h| h.host).collect();
    let mut seen = HashSet::new();
    let (mut todo, mut conflicts) = (Vec::new(), Vec::new());
    for rec in data.hosts {
        let clash = rec
            .aliases()
            .find(|a| seen.contains(*a) || !find_alias_in(set, a).is_empty())
            .map(str::to_owned)
            .or_else(|| existing.contains(&rec.host).then(|| rec.host.join(" ")));
        if let Some(c) = clash {
            conflicts.push(c);
            continue;
        }
        seen.extend(rec.aliases().map(str::to_owned));
        todo.push(rec);
    }

    if !conflicts.is_empty() {
        let list = conflicts.join(", ");
        if !skip_existing {
            bail!("already defined: {list} (use --skip-existing to import the rest)");
        }
        eprintln!("Skipping existing: {list}");
    }
    if todo.is_empty() {
        eprintln!("Nothing to import.");
        return Ok(());
    }
    let probe = todo
        .iter()
        .find_map(|r| r.aliases().next())
        .unwrap_or("sshcm-import")
        .to_owned();
    transaction(config, w, &probe, |d| {
        todo.iter()
            .try_for_each(|r| edit::add_block(d, &r.host, &r.options, &Position::Auto))
    })
}

/// Backups of `config`, newest first.
fn backups(config: &Path) -> Result<Vec<std::path::PathBuf>> {
    let real = io::real_path(config)?;
    let mut all = backup::list(&real)?;
    all.reverse();
    Ok(all)
}

pub fn backup_list(config: &Path) -> Result<()> {
    let all = backups(config)?;
    if all.is_empty() {
        eprintln!("No backups of {} yet.", display(config));
    }
    for (i, p) in all.iter().enumerate() {
        let size = std::fs::metadata(p).map_or(0, |m| m.len());
        let name = p.file_name().unwrap_or_default().to_string_lossy();
        println!("{:>3}  {name}  ({size} bytes)", i + 1);
    }
    Ok(())
}

pub fn backup_restore(config: &Path, id: &str, w: &WriteOpts) -> Result<()> {
    let all = backups(config)?;
    let chosen = match id {
        "latest" => all.first(),
        n if n.parse::<usize>().is_ok() => n
            .parse::<usize>()
            .ok()
            .and_then(|n| all.get(n.checked_sub(1)?)),
        name => all
            .iter()
            .find(|p| p.file_name().is_some_and(|f| f == name)),
    }
    .ok_or_else(|| anyhow!("no backup `{id}`; see `sshcm backup list`"))?
    .clone();

    let text = std::fs::read_to_string(&chosen)
        .with_context(|| format!("reading {}", chosen.display()))?;
    eprintln!("Restoring {}", display(&chosen));
    transaction(config, w, "sshcm-restore", |d| {
        *d = Document::parse(&text);
        Ok(())
    })
}
