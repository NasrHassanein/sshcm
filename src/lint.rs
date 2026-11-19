//! Static checks for a [`ConfigSet`].
//!
//! Every *error* is something OpenSSH itself rejects (verified with `ssh -G`
//! by `tests/lint_parity.rs`), so `sshcm lint` failing means `ssh` would
//! fail. *Warnings* are valid configurations that almost certainly do not do
//! what the author intended.
//!
//! | code | severity | meaning |
//! |------|----------|---------|
//! | E001 | error    | unknown keyword |
//! | E002 | error    | missing, extra, or invalid value |
//! | E003 | error    | unparsable line (no keyword, unterminated quote) |
//! | E004 | error    | config file writable by group/others (ssh refuses it) |
//! | E005 | error    | `Include` problem (cycle, depth, unreadable, permissions) |
//! | W001 | warning  | alias defined in more than one `Host` block |
//! | W002 | warning  | option that never applies because an earlier line wins |
//! | W003 | warning  | `IdentityFile` missing or readable by others |
//! | W005 | warning  | `Include` inside a `Host`/`Match` block (conditional) |
//! | W006 | warning  | macOS-only keyword without `IgnoreUnknown` |
//! | W007 | warning  | two forwards for a host listen on the same endpoint |

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::hosts::block_patterns;
use crate::include::ConfigSet;
use crate::model::forward::{Endpoint, Forward, ForwardKind};
use crate::model::keyword::{self, Merge};
use crate::model::pattern::wildcard_match;
use crate::model::rules;
use crate::model::value::split_args;
use crate::resolve::{Resolution, ResolveContext, resolve};
use crate::syntax::{BlockKind, Line, LineKind};

/// How serious a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Valid for ssh, but probably a mistake.
    Warning,
    /// ssh rejects the configuration.
    Error,
}

/// One lint finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    /// Stable rule code, e.g. `E002`.
    pub code: &'static str,
    /// Error or warning.
    pub severity: Severity,
    /// File the finding is in.
    pub file: PathBuf,
    /// 1-based line (0 for whole-file findings).
    pub line: usize,
    /// Explanation.
    pub message: String,
}

/// Environment for checks that look outside the config text.
#[derive(Debug, Clone)]
pub struct LintContext {
    /// Home directory, for expanding `~` in `IdentityFile`.
    pub home: PathBuf,
    /// Local user name (for resolution in W002).
    pub local_user: String,
    /// Whether to check the filesystem (key files, permissions).
    pub check_files: bool,
}

/// Run every check. Findings are sorted by file order, then line.
pub fn lint(set: &ConfigSet, ctx: &LintContext) -> Vec<Finding> {
    let mut out = Vec::new();
    let ignored = ignore_unknown_patterns(set);

    for (fid, file) in set.files().iter().enumerate() {
        line_checks(set, fid, &ignored, ctx, &mut out);
        if ctx.check_files && fid == 0 && crate::io::writable_by_others(&file.path) {
            out.push(finding(
                "E004",
                &file.path,
                0,
                "file is writable by group or others; ssh refuses to use it (\"Bad owner or permissions\")".into(),
            ));
        }
    }
    for d in set.diagnostics() {
        out.push(finding("E005", &d.file, d.line, d.message.clone()));
    }
    duplicate_aliases(set, &mut out);
    let mut resolved = Resolutions::new(set, &ctx.local_user);
    shadowed_options(set, &mut resolved, &mut out);
    forward_conflicts(set, &mut resolved, &mut out);

    let order: HashMap<&Path, usize> = set
        .files()
        .iter()
        .enumerate()
        .map(|(i, f)| (f.path.as_path(), i))
        .collect();
    out.sort_by_key(|f| {
        (
            order.get(f.file.as_path()).copied().unwrap_or(usize::MAX),
            f.line,
            f.code,
        )
    });
    out
}

fn finding(code: &'static str, file: &Path, line: usize, message: String) -> Finding {
    let severity = if code.starts_with('E') {
        Severity::Error
    } else {
        Severity::Warning
    };
    Finding {
        code,
        severity,
        file: file.to_owned(),
        line,
        message,
    }
}

/// Patterns from every `IgnoreUnknown` line (comma-separated lists).
fn ignore_unknown_patterns(set: &ConfigSet) -> Vec<String> {
    set.files()
        .iter()
        .flat_map(|f| f.doc.lines())
        .filter(|l| l.is_keyword("IgnoreUnknown"))
        .filter_map(|l| l.value())
        .flat_map(|v| {
            v.split([',', ' ', '\t'])
                .map(str::to_ascii_lowercase)
                .collect::<Vec<_>>()
        })
        .filter(|p| !p.is_empty())
        .collect()
}

/// Per-line checks for file `fid`: each line on its own, without looking
/// at the rest of the configuration.
fn line_checks(
    set: &ConfigSet,
    fid: usize,
    ignored: &[String],
    ctx: &LintContext,
    out: &mut Vec<Finding>,
) {
    let file = &set.files()[fid];
    let doc = &file.doc;
    for block in doc.blocks() {
        for idx in block.lines() {
            let line = &doc.lines()[idx];
            let mut push = |code, message| out.push(finding(code, &file.path, idx + 1, message));
            match line.kind() {
                LineKind::Invalid => push("E003", "line has no keyword".into()),
                LineKind::Directive(_) => {
                    directive_checks(line, block.kind(), ignored, ctx, &mut push);
                }
                LineKind::Blank | LineKind::Comment { .. } => {}
            }
        }
    }
}

/// Checks on one directive line in a block of kind `block`. Findings are
/// reported through `push(code, message)`. Once a check finds the line
/// unusable (unknown keyword, bad quoting, no value), later checks are
/// skipped, since they would only repeat the problem.
fn directive_checks(
    line: &Line,
    block: BlockKind,
    ignored: &[String],
    ctx: &LintContext,
    push: &mut impl FnMut(&'static str, String),
) {
    let kw = line.keyword().unwrap_or_default();
    let value = line.value().unwrap_or_default();
    let is_ignored = ignored
        .iter()
        .any(|p| wildcard_match(p, &kw.to_ascii_lowercase()));

    let Some(canon) = keyword::canonical(kw) else {
        if !is_ignored {
            let hint = keyword::hint(kw);
            push("E001", format!("unknown keyword `{kw}`{hint}"));
        }
        return;
    };
    if canon == "UseKeychain" && !is_ignored {
        push("W006", "`UseKeychain` is only understood by macOS's ssh; add `IgnoreUnknown UseKeychain` before it so other systems accept this file".into());
    }

    let args = match split_args(value) {
        Ok(a) => a,
        Err(e) => return push("E003", format!("`{kw}`: {e}")),
    };
    if args.is_empty() {
        return push("E002", format!("`{canon}` needs a value"));
    }
    if let Some(msg) = rules::check_value(canon, &args) {
        push("E002", msg);
    }
    if let Some(kind) = ForwardKind::from_keyword(canon)
        && let Err(e) = Forward::from_config(kind, &args)
    {
        push("E002", format!("`{canon}`: {e}"));
    }
    if canon == "Include" && block != BlockKind::Global {
        push("W005", "`Include` inside a Host/Match block only takes effect when that block applies; move it above the first Host line if it is meant to be global".into());
    }
    if canon == "IdentityFile"
        && ctx.check_files
        && let Some(msg) = identity_problem(&args[0], &ctx.home)
    {
        push("W003", msg);
    }
}

// ---------------------------------------------------------------------------
// Cross-block rules
// ---------------------------------------------------------------------------

/// Resolved settings per alias, computed on first use. W002 and W007 both
/// resolve the aliases of every `Host` block; resolution walks the whole
/// configuration, so each alias is resolved only once.
struct Resolutions<'a> {
    set: &'a ConfigSet,
    ctx: ResolveContext,
    cache: HashMap<String, Resolution>,
}

impl<'a> Resolutions<'a> {
    fn new(set: &'a ConfigSet, local_user: &str) -> Self {
        Self {
            set,
            ctx: ResolveContext {
                local_user: local_user.to_owned(),
            },
            cache: HashMap::new(),
        }
    }

    fn get(&mut self, alias: &str) -> &Resolution {
        self.cache
            .entry(alias.to_owned())
            .or_insert_with(|| resolve(self.set, alias, &self.ctx))
    }
}

/// W001: a literal alias named by more than one `Host` block.
fn duplicate_aliases(set: &ConfigSet, out: &mut Vec<Finding>) {
    let mut first: HashMap<String, (PathBuf, usize)> = HashMap::new();
    for (fid, bi) in set.blocks_in_read_order() {
        let file = &set.files()[fid];
        let block = &file.doc.blocks()[bi];
        let Some(patterns) = block_patterns(&file.doc, block) else {
            continue;
        };
        let line = block.lines().start + 1;
        for p in patterns.patterns().iter().filter(|p| p.is_literal()) {
            match first.get(&p.glob) {
                Some((f, l)) => out.push(finding(
                    "W001",
                    &file.path,
                    line,
                    format!("`{}` is already defined at {}:{l}; for options set in both, the first wins", p.glob, f.display()),
                )),
                None => {
                    first.insert(p.glob.clone(), (file.path.clone(), line));
                }
            }
        }
    }
}

/// W002: options in a `Host` block that never take effect for the block's own
/// aliases because an earlier line (e.g. in `Host *`) sets them first.
fn shadowed_options(set: &ConfigSet, resolved: &mut Resolutions<'_>, out: &mut Vec<Finding>) {
    for (fid, bi) in set.blocks_in_read_order() {
        let file = &set.files()[fid];
        let block = &file.doc.blocks()[bi];
        let Some(patterns) = block_patterns(&file.doc, block) else {
            continue;
        };
        let Some(alias) = patterns
            .patterns()
            .iter()
            .find(|p| p.is_literal())
            .map(|p| p.glob.clone())
        else {
            continue;
        };
        if !patterns.matches(&alias) {
            continue; // e.g. `Host a !a`
        }
        let res = resolved.get(&alias);
        for idx in block.body() {
            let line = &file.doc.lines()[idx];
            let Some(kw) = line.keyword().and_then(keyword::canonical) else {
                continue;
            };
            if keyword::merge_rule(kw) == Merge::Accumulate || keyword::is_structural(kw) {
                continue;
            }
            if let Some(winner) = res.get(kw)
                && (winner.file != file.path || winner.line != idx + 1)
            {
                out.push(finding(
                    "W002",
                    &file.path,
                    idx + 1,
                    format!(
                        "`{kw}` here never applies to `{alias}`: {}:{} sets it first",
                        winner.file.display(),
                        winner.line
                    ),
                ));
            }
        }
    }
}

/// W007: two forwards that apply to the same host and listen on the same
/// endpoint: the second fails with "Address already in use" (or aborts the
/// connection under `ExitOnForwardFailure`). Local and dynamic forwards
/// share the local side; remote forwards the remote side. Endpoints are
/// compared as written (`8080` and `localhost:8080` are not unified), so
/// there are no false positives; remote port 0 (server-assigned) never
/// conflicts.
fn forward_conflicts(set: &ConfigSet, resolved: &mut Resolutions<'_>, out: &mut Vec<Finding>) {
    let mut reported = std::collections::HashSet::new();
    for (fid, bi) in set.blocks_in_read_order() {
        let file = &set.files()[fid];
        let Some(patterns) = block_patterns(&file.doc, &file.doc.blocks()[bi]) else {
            continue;
        };
        for alias in patterns
            .patterns()
            .iter()
            .filter(|p| p.is_literal())
            .map(|p| p.glob.clone())
        {
            let res = resolved.get(&alias);
            let mut seen: HashMap<(bool, Endpoint), (PathBuf, usize)> = HashMap::new();
            for s in &res.settings {
                let Some(kind) = ForwardKind::from_keyword(&s.keyword) else {
                    continue;
                };
                let Ok(f) = Forward::from_config(kind, &s.args) else {
                    continue;
                };
                let remote_side = kind == ForwardKind::Remote;
                if remote_side && matches!(&f.listen, Endpoint::Tcp { port, .. } if port == "0") {
                    continue;
                }
                match seen.get(&(remote_side, f.listen.clone())) {
                    Some((pf, pl)) if reported.insert((s.file.clone(), s.line)) => out.push(finding(
                        "W007",
                        &s.file,
                        s.line,
                        format!(
                            "for `{alias}`, this forward listens on the same {} endpoint `{}` as {}:{pl}; ssh fails with \"Address already in use\"",
                            if remote_side { "remote" } else { "local" },
                            f.listen,
                            pf.display()
                        ),
                    )),
                    Some(_) => {}
                    None => {
                        seen.insert((remote_side, f.listen.clone()), (s.file.clone(), s.line));
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Filesystem checks
// ---------------------------------------------------------------------------

fn identity_problem(raw: &str, home: &Path) -> Option<String> {
    if raw.contains('%') || raw.contains("${") || raw.eq_ignore_ascii_case("none") {
        return None; // expanded per connection; cannot check statically
    }
    let path = match raw.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(raw),
    };
    if !path.is_absolute() {
        return None; // relative to ssh's working directory
    }
    let Ok(meta) = std::fs::metadata(&path) else {
        return Some(format!("IdentityFile {raw} does not exist"));
    };
    if meta.permissions().mode() & 0o077 != 0 {
        return Some(format!(
            "IdentityFile {raw} is accessible by group or others; ssh ignores such keys (\"UNPROTECTED PRIVATE KEY FILE\"); run chmod 600"
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::Document;

    fn codes(cfg: &str) -> Vec<(&'static str, usize)> {
        let set = ConfigSet::single("config", Document::parse(cfg));
        let ctx = LintContext {
            home: "/nonexistent".into(),
            local_user: "me".into(),
            check_files: false,
        };
        lint(&set, &ctx)
            .into_iter()
            .map(|f| (f.code, f.line))
            .collect()
    }

    #[test]
    fn clean_config_has_no_findings() {
        assert_eq!(
            codes(
                "Host a\n  HostName a.example\n  Port 2222\n\nHost *\n  ServerAliveInterval 1m\n"
            ),
            []
        );
    }

    #[test]
    fn unknown_keywords_respect_ignore_unknown() {
        assert_eq!(codes("Hostnme x\n"), [("E001", 1)]);
        assert_eq!(codes("IgnoreUnknown Foo*,Bar\nFooThing 1\nBar 2\n"), []);
    }

    #[test]
    fn values() {
        assert_eq!(
            codes(
                "Host a\n  Port 0\n  Compression true\n  User\n  BatchMode maybe\n  ConnectTimeout 5x\n  Port 1 2\n"
            ),
            [
                ("E002", 2),
                ("E002", 3),
                ("E002", 4),
                ("E002", 5),
                ("E002", 6),
                ("E002", 7),
                ("W002", 7), // second `Port` in the block: line 2 wins
            ]
        );
    }

    #[test]
    fn duplicates_and_shadowing() {
        let cfg =
            "Host *\n  User root\nHost a\n  User me\n  User again\n  Port 22\nHost a\n  Port 2\n";
        assert_eq!(
            codes(cfg),
            [("W002", 4), ("W002", 5), ("W001", 7), ("W002", 8)]
        );
    }

    #[test]
    fn invalid_lines_and_quotes() {
        assert_eq!(codes("=x\nUser \"open\n"), [("E003", 1), ("E003", 2)]);
    }

    #[test]
    fn forward_specs_and_conflicts() {
        assert_eq!(
            codes(
                "Host a\n  LocalForward 8080:db:80\n  DynamicForward 0\n  RemoteForward 9000 db\n"
            ),
            [("E002", 2), ("E002", 3), ("E002", 4)]
        );
        // Local + dynamic share the local side; Host * forwards count too.
        let cfg = "Host a\n  LocalForward 1080 db:80\n  RemoteForward 1080 x:1\nHost *\n  DynamicForward 1080\n  RemoteForward 0 y:1\n  RemoteForward 0 z:1\n";
        assert_eq!(codes(cfg), [("W007", 5)]);
    }

    #[test]
    fn include_in_block_and_use_keychain() {
        assert_eq!(
            codes("Host a\n  Include x\nUseKeychain yes\n"),
            [("W005", 2), ("W006", 3)]
        );
        assert_eq!(codes("IgnoreUnknown UseKeychain\nUseKeychain yes\n"), []);
    }
}
