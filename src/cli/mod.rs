//! The command-line front end: argument parsing, file I/O, dispatch, and
//! exit codes. Everything interesting lives in the library.

mod args;
mod commands;
mod mutate;
mod ops;
mod table;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow};
use clap::{CommandFactory, Parser};
use sshcm::edit::{self, EditError, Position};
use sshcm::format::FormatOptions;
use sshcm::hosts::find_alias_in;
use sshcm::include::{ConfigSet, IncludeContext};
use sshcm::io::lock::LockError;
use sshcm::model::value::quote;
use sshcm::syntax::Document;

use args::{BackupAction, Cli, Command};
use commands::{display, format_diagnostics};
use mutate::{ValidationFailed, transaction};

/// Exit codes (stable once released). 2 is clap's usage error.
mod exit {
    pub const ERROR: u8 = 1;
    pub const NOT_FOUND: u8 = 3;
    pub const LINT: u8 = 4;
    pub const VALIDATION: u8 = 5;
    pub const LOCKED: u8 = 6;
}

/// No Host block names the requested alias.
#[derive(Debug, thiserror::Error)]
#[error("no Host block names `{0}` (wildcard patterns don't count; try `sshcm resolve`)")]
pub struct NotFound(pub String);

fn config_path(cli: &Cli) -> Result<PathBuf> {
    if let Some(p) = &cli.file {
        return Ok(p.clone());
    }
    let home = home::home_dir().context("cannot determine home directory; pass --file")?;
    Ok(home.join(".ssh").join("config"))
}

fn local_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".to_owned())
}

/// Load `path` and everything it includes. With `allow_missing`, a missing
/// root is treated as empty (so `add` can create the file).
fn load_set(path: &Path, allow_missing: bool) -> Result<ConfigSet> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if allow_missing && e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let home = home::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    let ctx = IncludeContext::for_home(home, local_user());
    let set = ConfigSet::with_root(path, Document::parse(&text), &ctx);
    for w in format_diagnostics(set.diagnostics()) {
        eprintln!("warning: {w}");
    }
    Ok(set)
}

/// The file whose block for `alias` ssh reads first; edits go there.
fn owner(set: &ConfigSet, alias: &str) -> Result<PathBuf> {
    let found = find_alias_in(set, alias);
    let Some(&(fid, _)) = found.first() else {
        return Err(EditError::AliasNotFound(alias.to_owned()).into());
    };
    let path = set.files()[fid].path.clone();
    let others: BTreeSet<usize> = found
        .iter()
        .map(|&(f, _)| f)
        .filter(|&f| f != fid)
        .collect();
    for f in others {
        eprintln!(
            "note: `{alias}` is also defined in {}; only {} is changed",
            display(&set.files()[f].path),
            display(&path)
        );
    }
    Ok(path)
}

/// Refuse to create `alias` if any file in the set already defines it.
fn ensure_absent(set: &ConfigSet, alias: &str) -> Result<()> {
    let Some(&(fid, bi)) = find_alias_in(set, alias).first() else {
        return Ok(());
    };
    let line = set.files()[fid].doc.blocks()[bi].lines().start + 1;
    if fid == 0 {
        return Err(EditError::AliasExists {
            alias: alias.to_owned(),
            line,
        }
        .into());
    }
    Err(anyhow!(
        "`{alias}` already exists ({}:{line})",
        display(&set.files()[fid].path)
    ))
}

/// The options and position for `add`, from its flags: the dedicated
/// flags first (in `ssh_config`'s conventional order), then `-o` options.
fn add_args(cmd: &Command) -> (Vec<(String, String)>, Position) {
    let Command::Add {
        hostname,
        user,
        port,
        identity,
        options,
        before,
        after,
        ..
    } = cmd
    else {
        unreachable!("add_args is only called for `add`");
    };
    let mut opts: Vec<(String, String)> = [
        ("HostName", hostname.clone()),
        ("User", user.clone()),
        ("Port", port.map(|p| p.to_string())),
        (
            "IdentityFile",
            identity.as_deref().map(|i| quote(i).into_owned()),
        ),
    ]
    .into_iter()
    .filter_map(|(k, v)| v.map(|v| (k.to_owned(), v)))
    .collect();
    opts.extend(options.iter().cloned());
    let position = match (before, after) {
        (Some(a), _) => Position::Before(a.clone()),
        (_, Some(a)) => Position::After(a.clone()),
        _ => Position::Auto,
    };
    (opts, position)
}

fn run(cli: &Cli) -> Result<()> {
    // Commands that don't touch a config file.
    match &cli.command {
        Command::Completions { shell } => {
            clap_complete::generate(*shell, &mut Cli::command(), "sshcm", &mut std::io::stdout());
            return Ok(());
        }
        Command::Manpage => {
            clap_mangen::Man::new(Cli::command()).render(&mut std::io::stdout())?;
            return Ok(());
        }
        _ => {}
    }

    let path = config_path(cli)?;
    let w = &cli.write;
    match &cli.command {
        Command::List { .. }
        | Command::Show { .. }
        | Command::Resolve { .. }
        | Command::DumpSyntax => print!("{}", read_command(cli, &path)?),
        Command::Lint {
            strict,
            json,
            no_file_checks,
        } => ops::lint(
            &load_set(&path, false)?,
            &local_user(),
            *strict,
            *json,
            !*no_file_checks,
        )?,
        Command::Fmt {
            check,
            indent,
            tabs,
            all,
        } => {
            let opts = FormatOptions {
                indent: if *tabs {
                    "\t".to_owned()
                } else {
                    " ".repeat(*indent)
                },
            };
            ops::fmt(&load_set(&path, false)?, &opts, *check, *all, w)?;
        }
        Command::Export => ops::export_json(&load_set(&path, false)?)?,
        Command::Import {
            source,
            skip_existing,
        } => ops::import(&path, &load_set(&path, true)?, source, *skip_existing, w)?,
        Command::Backup { action } => match action {
            BackupAction::List => ops::backup_list(&path)?,
            BackupAction::Restore { id } => ops::backup_restore(&path, id, w)?,
        },
        Command::Completions { .. } | Command::Manpage => unreachable!("handled above"),
        _ => edit_command(cli, &path)?,
    }
    Ok(())
}

/// `list`, `show`, `resolve`, `dump-syntax`: return the text to print.
fn read_command(cli: &Cli, path: &Path) -> Result<String> {
    let path = path.to_owned();
    Ok(match &cli.command {
        Command::List { pattern, json } => {
            commands::list(&load_set(&path, false)?, pattern.as_deref(), *json)?
        }
        Command::Show { alias, json } => commands::show(&load_set(&path, false)?, alias, *json)?,
        Command::Resolve {
            host,
            via_ssh: true,
            ..
        } => commands::resolve_via_ssh(&path, host)?,
        Command::Resolve { host, json, .. } => {
            let set = load_set(&path, false)?;
            let (out, warnings) = commands::resolve(&set, host, &local_user(), *json)?;
            for w in warnings {
                eprintln!("warning: {w}");
            }
            out
        }
        Command::DumpSyntax => {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            commands::dump_syntax(&Document::parse(&text))
        }
        _ => unreachable!("not a read command"),
    })
}

/// `add`, `set`, `unset`, `rm`, `rename`, `cp`.
fn edit_command(cli: &Cli, path: &Path) -> Result<()> {
    let path = path.to_owned();
    let w = &cli.write;
    match &cli.command {
        Command::Add { alias, .. } => {
            ensure_absent(&load_set(&path, true)?, alias)?;
            let (opts, position) = add_args(&cli.command);
            transaction(&path, w, alias, |d| {
                edit::add_host(d, alias, &opts, &position)
            })?;
        }
        Command::Set {
            alias,
            settings,
            append,
        } => {
            let target = owner(&load_set(&path, false)?, alias)?;
            transaction(&target, w, alias, |d| {
                settings
                    .iter()
                    .try_for_each(|(k, v)| edit::set(d, alias, k, v, *append))
            })?;
        }
        Command::Unset { alias, keys } => {
            let target = owner(&load_set(&path, false)?, alias)?;
            transaction(&target, w, alias, |d| {
                keys.iter().try_for_each(|k| edit::unset(d, alias, k))
            })?;
        }
        Command::Rm { alias } => {
            let target = owner(&load_set(&path, false)?, alias)?;
            transaction(&target, w, alias, |d| edit::remove_host(d, alias))?;
        }
        Command::Rename { old, new } => {
            let set = load_set(&path, false)?;
            ensure_absent(&set, new)?;
            let target = owner(&set, old)?;
            transaction(&target, w, new, |d| edit::rename(d, old, new))?;
        }
        Command::Cp { src, dst } => {
            let set = load_set(&path, false)?;
            ensure_absent(&set, dst)?;
            let target = owner(&set, src)?;
            transaction(&target, w, dst, |d| edit::copy(d, src, dst))?;
        }
        _ => unreachable!("not an edit command"),
    }
    Ok(())
}

fn exit_code(e: &anyhow::Error) -> u8 {
    if e.downcast_ref::<NotFound>().is_some()
        || matches!(
            e.downcast_ref::<EditError>(),
            Some(EditError::AliasNotFound(_) | EditError::KeyNotSet { .. })
        )
    {
        exit::NOT_FOUND
    } else if e.downcast_ref::<ops::LintFailed>().is_some() {
        exit::LINT
    } else if e.downcast_ref::<ValidationFailed>().is_some() {
        exit::VALIDATION
    } else if matches!(e.downcast_ref::<LockError>(), Some(LockError::Timeout(..))) {
        exit::LOCKED
    } else {
        exit::ERROR
    }
}

/// Parse arguments, run, and map errors to exit codes.
pub fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(exit_code(&e))
        }
    }
}
