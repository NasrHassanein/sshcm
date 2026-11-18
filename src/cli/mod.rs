//! The command-line front end: argument parsing, file I/O, dispatch, and
//! exit codes. Everything interesting lives in the library.

mod args;
mod commands;
mod mutate;
mod ops;
mod ssh;
mod table;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow, bail};
use clap::{CommandFactory, Parser};
use sshcm::command::{parse_ssh_command, shell_split};
use sshcm::edit::{self, EditError, Position};
use sshcm::format::FormatOptions;
use sshcm::hosts::find_alias_in;
use sshcm::include::{ConfigSet, IncludeContext};
use sshcm::io::lock::LockError;
use sshcm::model::forward::{Forward, parse_listen};
use sshcm::model::value::quote;
use sshcm::syntax::Document;

use args::{BackupAction, Cli, Command, ForwardAction, ForwardSpecs, ParseAction, WriteOpts};
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
fn add_args(cmd: &Command) -> Result<(Vec<(String, String)>, Position)> {
    let Command::Add {
        hostname,
        user,
        port,
        identity,
        options,
        before,
        after,
        forwards,
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
    for f in parse_forwards(forwards)? {
        opts.push((f.keyword().to_owned(), f.to_config_value()));
    }
    Ok((opts, placement(before.as_deref(), after.as_deref())))
}

/// Where to insert a new block, from the `--before` / `--after` flags
/// (clap makes them mutually exclusive).
fn placement(before: Option<&str>, after: Option<&str>) -> Position {
    match (before, after) {
        (Some(a), _) => Position::Before(a.to_owned()),
        (_, Some(a)) => Position::After(a.to_owned()),
        (None, None) => Position::Auto,
    }
}

/// Parse `-L/-R/-D` specs (ssh command-line syntax).
fn parse_forwards(specs: &ForwardSpecs) -> Result<Vec<Forward>> {
    specs
        .iter()
        .map(|(kind, spec)| {
            Forward::from_cli(kind, spec).with_context(|| format!("-{} {spec}", kind.flag()))
        })
        .collect()
}

/// `forward list | add | rm`. Edits go to the file that defines the alias.
fn forward_command(path: &Path, w: &WriteOpts, action: &ForwardAction) -> Result<()> {
    let need_specs = |specs: &ForwardSpecs| {
        if specs.is_empty() {
            bail!("give at least one of -L, -R, -D");
        }
        Ok(())
    };
    match action {
        ForwardAction::List { alias, json } => {
            print!(
                "{}",
                commands::forward_list(&load_set(path, false)?, alias.as_deref(), *json)?
            );
        }
        ForwardAction::Add { alias, specs } => {
            need_specs(specs)?;
            let forwards = parse_forwards(specs)?;
            let target = owner(&load_set(path, false)?, alias)?;
            transaction(&target, w, alias, |d| {
                forwards
                    .iter()
                    .try_for_each(|f| edit::add_forward(d, alias, f))
            })?;
        }
        ForwardAction::Rm { alias, specs } => {
            need_specs(specs)?;
            let listens = specs
                .iter()
                .map(|(kind, spec)| {
                    parse_listen(kind, spec)
                        .map(|e| (kind, e))
                        .with_context(|| format!("-{} {spec}", kind.flag()))
                })
                .collect::<Result<Vec<_>>>()?;
            let target = owner(&load_set(path, false)?, alias)?;
            transaction(&target, w, alias, |d| {
                listens
                    .iter()
                    .try_for_each(|(kind, listen)| edit::remove_forward(d, alias, *kind, listen))
            })?;
        }
    }
    Ok(())
}

/// The ssh command's words: one quoted argument is shell-split; several
/// arguments are words the shell already split.
fn command_words(args: &[String]) -> Result<Vec<String>> {
    match args {
        [one] => Ok(shell_split(one)?),
        [first, ..] if first.contains(char::is_whitespace) => bail!(
            "the ssh command must be one quoted argument, with sshcm options before it \
             (e.g. `sshcm parse add -y \"ssh user@host\"`)"
        ),
        words => Ok(words.to_vec()),
    }
}

/// `parse view | add`.
fn parse_command(path: &Path, w: &WriteOpts, action: &ParseAction) -> Result<()> {
    let (alias, command) = match action {
        ParseAction::View { alias, command } | ParseAction::Add { alias, command, .. } => {
            (alias, command)
        }
    };
    let parsed = parse_ssh_command(&command_words(command)?).context("parsing the ssh command")?;
    let alias = alias.clone().unwrap_or_else(|| parsed.host.clone());
    let options = parsed.options_for(&alias);

    for s in &parsed.skipped {
        eprintln!("note: {s}; not written to the config");
    }
    if let Some(cmd) = &parsed.remote_command {
        eprintln!(
            "note: remote command `{cmd}` is not part of the record; `ssh {alias}` opens a shell"
        );
    }

    let set = load_set(path, true)?;
    match action {
        ParseAction::View { .. } => {
            if let Err(e) = ensure_absent(&set, &alias) {
                eprintln!("note: {e:#}; `parse add` would refuse it (use --as NAME)");
            }
            print!(
                "{}",
                edit::render_host_block(&set.root().doc, &alias, &options)?
            );
        }
        ParseAction::Add { before, after, .. } => {
            if let Err(e) = ensure_absent(&set, &alias) {
                bail!("{e:#}; use --as NAME to add it under another alias");
            }
            let position = placement(before.as_deref(), after.as_deref());
            transaction(path, w, &alias, |d| {
                edit::add_host(d, &alias, &options, &position)
            })?;
        }
    }
    Ok(())
}

fn run(cli: &Cli) -> Result<()> {
    // Commands that don't touch a config file.
    match &cli.command {
        Command::Completions { shell } => {
            // `generate` panics on write errors, so render into memory first.
            let mut out = Vec::new();
            clap_complete::generate(*shell, &mut Cli::command(), "sshcm", &mut out);
            return write_stdout(&out);
        }
        Command::Manpage => {
            let mut out = Vec::new();
            clap_mangen::Man::new(Cli::command()).render(&mut out)?;
            return write_stdout(&out);
        }
        _ => {}
    }

    let path = config_path(cli)?;
    let w = &cli.write;
    match &cli.command {
        Command::List { .. }
        | Command::Show { .. }
        | Command::Resolve { .. }
        | Command::DumpSyntax => write_stdout(read_command(cli, &path)?.as_bytes())?,
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
        Command::Forward { action } => forward_command(&path, w, action)?,
        Command::Parse { action } => parse_command(&path, w, action)?,
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
        } => ssh::effective_config(&path, host)?,
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
            let (opts, position) = add_args(&cli.command)?;
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
            Some(
                EditError::AliasNotFound(_)
                    | EditError::KeyNotSet { .. }
                    | EditError::ForwardNotFound { .. }
            )
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

/// Write `bytes` to stdout. A reader that closed the pipe early (`| head`)
/// is not an error: the output simply ends there, as with C tools.
fn write_stdout(bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    match out.write_all(bytes).and_then(|()| out.flush()) {
        Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => Err(e.into()),
        _ => Ok(()),
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
