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
use crate::model::value::split_args;
use crate::resolve::{ResolveContext, resolve};
use crate::syntax::{BlockKind, LineKind};

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
    shadowed_options(set, ctx, &mut out);
    forward_conflicts(set, ctx, &mut out);

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

fn line_checks(
    set: &ConfigSet,
    fid: usize,
    ignored: &[String],
    ctx: &LintContext,
    out: &mut Vec<Finding>,
) {
    let file = &set.files()[fid];
    let doc = &file.doc;
    let path = file.path.as_path();

    for block in doc.blocks() {
        for idx in block.lines() {
            let line = &doc.lines()[idx];
            let n = idx + 1;
            let (kw, value) = match line.kind() {
                LineKind::Invalid => {
                    out.push(finding("E003", path, n, "line has no keyword".into()));
                    continue;
                }
                LineKind::Directive(_) => (
                    line.keyword().unwrap_or_default(),
                    line.value().unwrap_or_default(),
                ),
                LineKind::Blank | LineKind::Comment { .. } => continue,
            };
            let is_ignored = ignored
                .iter()
                .any(|p| wildcard_match(p, &kw.to_ascii_lowercase()));

            let Some(canon) = keyword::canonical(kw) else {
                if !is_ignored {
                    let hint = keyword::hint(kw);
                    out.push(finding(
                        "E001",
                        path,
                        n,
                        format!("unknown keyword `{kw}`{hint}"),
                    ));
                }
                continue;
            };
            if canon == "UseKeychain" && !is_ignored {
                out.push(finding("W006", path, n, "`UseKeychain` is only understood by macOS's ssh; add `IgnoreUnknown UseKeychain` before it so other systems accept this file".into()));
            }

            let args = match split_args(value) {
                Ok(a) => a,
                Err(e) => {
                    out.push(finding("E003", path, n, format!("`{kw}`: {e}")));
                    continue;
                }
            };
            if args.is_empty() {
                out.push(finding("E002", path, n, format!("`{canon}` needs a value")));
                continue;
            }
            if let Some(msg) = check_value(canon, &args) {
                out.push(finding("E002", path, n, msg));
            }
            if let Some(kind) = ForwardKind::from_keyword(canon)
                && let Err(e) = Forward::from_config(kind, &args)
            {
                out.push(finding("E002", path, n, format!("`{canon}`: {e}")));
            }
            if canon == "Include" && block.kind() != BlockKind::Global {
                out.push(finding("W005", path, n, "`Include` inside a Host/Match block only takes effect when that block applies; move it above the first Host line if it is meant to be global".into()));
            }
            if canon == "IdentityFile"
                && ctx.check_files
                && let Some(msg) = identity_problem(&args[0], &ctx.home)
            {
                out.push(finding("W003", path, n, msg));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Value rules (each verified against `ssh -G`; see tests/lint_parity.rs)
// ---------------------------------------------------------------------------

/// Flags accepting yes/no/true/false.
const FLAGS: &[&str] = &[
    "BatchMode",
    "CanonicalizeFallbackLocal",
    "CheckHostIP",
    "ClearAllForwardings",
    "EnableEscapeCommandline",
    "EnableSSHKeysign",
    "ExitOnForwardFailure",
    "ForkAfterAuthentication",
    "ForwardX11",
    "ForwardX11Trusted",
    "GatewayPorts",
    "GSSAPIAuthentication",
    "GSSAPIDelegateCredentials",
    "HashKnownHosts",
    "HostbasedAuthentication",
    "IdentitiesOnly",
    "KbdInteractiveAuthentication",
    "NoHostAuthenticationForLocalhost",
    "PasswordAuthentication",
    "PermitLocalCommand",
    "ProxyUseFdpass",
    "StdinNull",
    "StreamLocalBindUnlink",
    "TCPKeepAlive",
    "VisualHostKey",
];

/// Single-valued keywords with a fixed set of choices (case-insensitive).
const CHOICES: &[(&str, &[&str])] = &[
    ("AddressFamily", &["any", "inet", "inet6"]),
    ("Compression", &["yes", "no"]),
    (
        "ControlMaster",
        &["yes", "no", "true", "false", "ask", "auto", "autoask"],
    ),
    (
        "LogLevel",
        &[
            "quiet", "fatal", "error", "info", "verbose", "debug", "debug1", "debug2", "debug3",
        ],
    ),
    (
        "RequestTTY",
        &["yes", "no", "true", "false", "force", "auto"],
    ),
    (
        "StrictHostKeyChecking",
        &["yes", "no", "true", "false", "ask", "accept-new", "off"],
    ),
    ("VerifyHostKeyDNS", &["yes", "no", "true", "false", "ask"]),
];

/// Non-negative integers (`ConnectionAttempts` must also be positive).
const INTEGERS: &[&str] = &[
    "CanonicalizeMaxDots",
    "ConnectionAttempts",
    "NumberOfPasswordPrompts",
    "ServerAliveCountMax",
];

/// Time values: `30`, `1m`, `1h30m` (units s, m, h, d, w).
const TIMES: &[&str] = &["ConnectTimeout", "ForwardX11Timeout", "ServerAliveInterval"];

pub(crate) fn check_value(kw: &str, args: &[String]) -> Option<String> {
    let single_valued = kw == "Port"
        || FLAGS.contains(&kw)
        || INTEGERS.contains(&kw)
        || TIMES.contains(&kw)
        || CHOICES.iter().any(|(k, _)| *k == kw);
    if !single_valued {
        return None;
    }
    if args.len() > 1 {
        return Some(format!("`{kw}` takes a single value, got {}", args.len()));
    }
    let v = args[0].as_str();
    let lower = v.to_ascii_lowercase();

    let ok = if kw == "Port" {
        // A number 1-65535, or a service name such as `ssh`.
        if v.starts_with(|c: char| c.is_ascii_digit()) {
            v.parse::<u16>().is_ok_and(|p| p > 0)
        } else {
            true
        }
    } else if FLAGS.contains(&kw) {
        matches!(lower.as_str(), "yes" | "no" | "true" | "false")
    } else if let Some((_, choices)) = CHOICES.iter().find(|(k, _)| *k == kw) {
        choices.contains(&lower.as_str())
    } else if INTEGERS.contains(&kw) {
        v.parse::<u32>()
            .is_ok_and(|n| kw != "ConnectionAttempts" || n > 0)
    } else {
        is_time(v)
    };
    (!ok).then(|| match kw {
        "Port" => format!("`Port {v}`: must be a number from 1 to 65535 or a service name"),
        _ if FLAGS.contains(&kw) => format!("`{kw} {v}`: must be yes or no"),
        _ if INTEGERS.contains(&kw) => format!("`{kw} {v}`: must be a whole number"),
        _ if TIMES.contains(&kw) => format!("`{kw} {v}`: must be a time such as 30, 90s, or 5m"),
        _ => {
            let choices = CHOICES
                .iter()
                .find(|(k, _)| *k == kw)
                .map_or(&[][..], |(_, c)| c);
            format!("`{kw} {v}`: must be one of {}", choices.join(", "))
        }
    })
}

/// OpenSSH `convtime` syntax: one or more `<digits>[smhdw]` groups.
fn is_time(v: &str) -> bool {
    let mut rest = v;
    if rest.is_empty() {
        return false;
    }
    while !rest.is_empty() {
        let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits == 0 {
            return false;
        }
        rest = &rest[digits..];
        rest = rest
            .strip_prefix(['s', 'S', 'm', 'M', 'h', 'H', 'd', 'D', 'w', 'W'])
            .unwrap_or(rest);
    }
    true
}

// ---------------------------------------------------------------------------
// Cross-block rules
// ---------------------------------------------------------------------------

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

/// Options in a `Host` block that never take effect for the block's own
/// aliases because an earlier line (e.g. in `Host *`) sets them first.
fn shadowed_options(set: &ConfigSet, ctx: &LintContext, out: &mut Vec<Finding>) {
    let rctx = ResolveContext {
        local_user: ctx.local_user.clone(),
    };
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
        let res = resolve(set, &alias, &rctx);
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

/// Two forwards that apply to the same host and listen on the same
/// endpoint: the second fails with "Address already in use" (or aborts the
/// connection under `ExitOnForwardFailure`). Local and dynamic forwards
/// share the local side; remote forwards the remote side. Endpoints are
/// compared as written (`8080` and `localhost:8080` are not unified), so
/// there are no false positives; remote port 0 (server-assigned) never
/// conflicts.
fn forward_conflicts(set: &ConfigSet, ctx: &LintContext, out: &mut Vec<Finding>) {
    let rctx = ResolveContext {
        local_user: ctx.local_user.clone(),
    };
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
            let res = resolve(set, &alias, &rctx);
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
    fn time_syntax() {
        for ok in ["0", "30", "1m", "1h30m", "2W"] {
            assert!(is_time(ok), "{ok}");
        }
        for bad in ["", "m", "-1", "1x", "1.5m"] {
            assert!(!is_time(bad), "{bad}");
        }
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
