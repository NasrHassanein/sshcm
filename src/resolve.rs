//! Effective configuration for a host: OpenSSH's first-match-wins evaluation.
//!
//! Blocks are walked in the order ssh reads them, descending into
//! `Include`d files where the `Include` line appears. A directive applies if
//! its block is active for the host; for most keywords the first applicable
//! value wins, while list-like keywords (see [`Merge::Accumulate`]) collect
//! every value.
//!
//! `Include` semantics (verified against OpenSSH 10.2):
//! - an included file is read only if the block containing the `Include`
//!   applies; it then starts out active, and its own `Host`/`Match` lines
//!   switch activity within that file only;
//! - after the included file, the including block's activity is restored, so
//!   options after the `Include` line still apply.
//!
//! Limitations (reported as warnings, never silently guessed): `Match`
//! criteria other than `all`, `host`, `originalhost`, `user` and `localuser`
//! cannot be evaluated offline and are treated as not matching.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::diagnostic::Diagnostic;
use crate::include::{ConfigSet, MAX_DEPTH};
use crate::model::keyword::{self, Merge};
use crate::model::pattern::HostPatterns;
use crate::model::value::split_args;
use crate::syntax::{BlockKind, Line};

/// Facts about the local environment that resolution may need.
#[derive(Debug, Clone)]
pub struct ResolveContext {
    /// The local user name (default for `User`, and for `Match localuser`).
    pub local_user: String,
}

/// One effective setting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Setting {
    /// Canonical keyword spelling (or as written, if unknown).
    pub keyword: String,
    /// Arguments after OpenSSH-style splitting (quotes removed).
    pub args: Vec<String>,
    /// The file the value came from.
    pub file: PathBuf,
    /// 1-based line number in that file.
    pub line: usize,
}

/// The result of resolving a host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Resolution {
    /// The host name that was resolved, as given.
    pub host: String,
    /// Effective settings, in the order OpenSSH obtained them.
    pub settings: Vec<Setting>,
    /// Places where the result may differ from what `ssh` would do.
    pub warnings: Vec<Diagnostic>,
}

impl Resolution {
    /// The first value of `keyword` (case-insensitive), if set.
    pub fn get(&self, keyword: &str) -> Option<&Setting> {
        self.settings
            .iter()
            .find(|s| s.keyword.eq_ignore_ascii_case(keyword))
    }

    fn first_arg(&self, keyword: &str) -> Option<&str> {
        self.get(keyword)
            .and_then(|s| s.args.first())
            .map(String::as_str)
    }

    fn warn(&mut self, file: &Path, line: usize, message: impl Into<String>) {
        self.warnings.push(Diagnostic {
            file: file.to_owned(),
            line,
            message: message.into(),
        });
    }
}

/// Compute the effective configuration for `host`.
pub fn resolve(set: &ConfigSet, host: &str, ctx: &ResolveContext) -> Resolution {
    let mut res = Resolution {
        host: host.to_owned(),
        settings: Vec::new(),
        warnings: Vec::new(),
    };
    walk(set, 0, host, ctx, &mut res, 0);
    res
}

/// Evaluate file `fid`, which is only reached when the block that included
/// it applies (or it is the root).
fn walk(
    set: &ConfigSet,
    fid: usize,
    host: &str,
    ctx: &ResolveContext,
    res: &mut Resolution,
    depth: usize,
) {
    let file = &set.files()[fid];
    let (path, doc) = (file.path.as_path(), &file.doc);

    for block in doc.blocks() {
        let range = block.lines();
        let header = block.header().map(|i| &doc.lines()[i]);

        let active = match (block.kind(), header) {
            (BlockKind::Global, _) => true,
            (BlockKind::Host, Some(h)) => {
                match HostPatterns::from_host_value(h.value().unwrap_or_default()) {
                    Ok(p) => p.matches(host),
                    Err(e) => {
                        res.warn(path, range.start + 1, format!("Host line ignored: {e}"));
                        false
                    }
                }
            }
            (BlockKind::Match, Some(h)) => {
                let line_no = range.start + 1;
                eval_match(h.value().unwrap_or_default(), host, ctx, res, path, line_no)
            }
            (_, None) => unreachable!("Host/Match blocks always have a header"),
        };
        if !active {
            continue;
        }

        let body_start = range.start + usize::from(header.is_some());
        for idx in body_start..range.end {
            let line = &doc.lines()[idx];
            if !line.is_keyword("Include") {
                apply(line, path, idx + 1, res);
                continue;
            }
            match set.included_by(fid, idx) {
                // Problems loading the include are in `set.diagnostics()`.
                Some(children) if depth < MAX_DEPTH => {
                    for &child in children {
                        walk(set, child, host, ctx, res, depth + 1);
                    }
                }
                Some(_) => res.warn(path, idx + 1, "Include nested too deeply; not followed"),
                None => res.warn(
                    path,
                    idx + 1,
                    "Include not followed; included settings are missing",
                ),
            }
        }
    }
}

/// Apply one directive line from an active block.
fn apply(line: &Line, file: &Path, line_no: usize, res: &mut Resolution) {
    let (Some(kw), Some(value)) = (line.keyword(), line.value()) else {
        return; // blank, comment, or invalid
    };
    let args = match split_args(value) {
        Ok(a) => a,
        Err(e) => {
            res.warn(file, line_no, format!("{kw} ignored: {e}"));
            return;
        }
    };
    let keyword = keyword::canonical(kw).map_or_else(|| kw.to_owned(), str::to_owned);
    let already_set = res.get(&keyword).is_some();
    if keyword::merge_rule(&keyword) == Merge::Accumulate || !already_set {
        res.settings.push(Setting {
            keyword,
            args,
            file: file.to_owned(),
            line: line_no,
        });
    }
}

/// Evaluate a `Match` line. All criteria must hold (logical AND).
fn eval_match(
    criteria: &str,
    host: &str,
    ctx: &ResolveContext,
    res: &mut Resolution,
    file: &Path,
    line_no: usize,
) -> bool {
    let tokens = match split_args(criteria) {
        Ok(t) => t,
        Err(e) => {
            res.warn(file, line_no, format!("Match line ignored: {e}"));
            return false;
        }
    };

    let mut tokens = tokens.iter();
    let mut all_hold = true;
    while let Some(tok) = tokens.next() {
        let (negate, name) = match tok.strip_prefix('!') {
            Some(n) => (true, n.to_ascii_lowercase()),
            None => (false, tok.to_ascii_lowercase()),
        };
        if name == "all" {
            all_hold &= !negate;
            continue;
        }

        let subject: Option<String> = match name.as_str() {
            // `host` sees HostName as set so far (with %h expanded).
            "host" => Some(res.first_arg("HostName").map_or_else(
                || host.to_owned(),
                |h| h.replace("%h", host).replace("%%", "%"),
            )),
            "originalhost" => Some(host.to_owned()),
            "user" => Some(res.first_arg("User").unwrap_or(&ctx.local_user).to_owned()),
            "localuser" => Some(ctx.local_user.clone()),
            _ => None,
        };

        let Some(subject) = subject else {
            res.warn(
                file,
                line_no,
                format!("cannot evaluate `Match {name}` offline; block treated as not matching"),
            );
            // Skip its argument, if any, and fail the block.
            if !matches!(name.as_str(), "canonical" | "final") {
                tokens.next();
            }
            all_hold = false;
            continue;
        };

        let Some(list) = tokens.next() else {
            res.warn(
                file,
                line_no,
                format!("`Match {name}` is missing its pattern list"),
            );
            return false;
        };
        let hit = HostPatterns::from_comma_list(list).matches(&subject);
        all_hold &= hit != negate;
    }
    all_hold
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::Document;
    use pretty_assertions::assert_eq;

    fn single(cfg: &str) -> ConfigSet {
        ConfigSet::single("config", Document::parse(cfg))
    }

    fn ctx() -> ResolveContext {
        ResolveContext {
            local_user: "me".into(),
        }
    }

    fn summary(cfg: &str, host: &str) -> Vec<(String, String)> {
        resolve(&single(cfg), host, &ctx())
            .settings
            .into_iter()
            .map(|s| (s.keyword, s.args.join(" ")))
            .collect()
    }

    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter()
            .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
            .collect()
    }

    #[test]
    fn first_match_wins_and_wildcard_defaults() {
        let cfg = "Host web\n  User deploy\n  Port 2200\nHost *\n  User root\n  Compression yes\n";
        assert_eq!(
            summary(cfg, "web"),
            pairs(&[("User", "deploy"), ("Port", "2200"), ("Compression", "yes")])
        );
        assert_eq!(
            summary(cfg, "other"),
            pairs(&[("User", "root"), ("Compression", "yes")])
        );
    }

    #[test]
    fn earlier_wildcard_shadows_later_specific() {
        let cfg = "Host *\n  User root\nHost web\n  User deploy\n";
        assert_eq!(summary(cfg, "web"), pairs(&[("User", "root")]));
    }

    #[test]
    fn identity_files_accumulate() {
        let cfg = "Host a\n  IdentityFile ~/k1\nHost *\n  IdentityFile \"~/My Keys/k2\"\n";
        assert_eq!(
            summary(cfg, "a"),
            pairs(&[("IdentityFile", "~/k1"), ("IdentityFile", "~/My Keys/k2")])
        );
    }

    #[test]
    fn keywords_are_canonicalised_and_case_insensitive() {
        let cfg = "hostname=x.example\nHOSTNAME y\n";
        assert_eq!(summary(cfg, "any"), pairs(&[("HostName", "x.example")]));
    }

    #[test]
    fn match_host_sees_hostname_set_so_far() {
        let cfg =
            "Host short\n  HostName %h.corp.example\nMatch host *.corp.example\n  User corp\n";
        assert_eq!(
            summary(cfg, "short"),
            pairs(&[("HostName", "%h.corp.example"), ("User", "corp")])
        );
    }

    #[test]
    fn match_negation_and_conjunction() {
        let cfg = "Match originalhost a,b !user root\n  Port 1\n";
        assert_eq!(summary(cfg, "a"), pairs(&[("Port", "1")]));
        assert_eq!(summary(cfg, "c"), pairs(&[]));
        let cfg = "Match originalhost a user root\n  Port 1\n";
        assert_eq!(summary(cfg, "a"), pairs(&[])); // local user is "me"
    }

    #[test]
    fn unsupported_match_warns() {
        let r = resolve(
            &single("Match exec \"true\"\n  Port 1\nMatch all\n  Port 2\n"),
            "x",
            &ctx(),
        );
        assert_eq!(r.get("Port").unwrap().args, ["2"]);
        assert_eq!(r.warnings.len(), 1);
        assert_eq!(r.warnings[0].line, 1);
    }

    #[test]
    fn unfollowed_include_warns() {
        let r = resolve(&single("Include x\nUser a\n"), "h", &ctx());
        assert_eq!(r.warnings[0].line, 1);
        assert_eq!(r.get("User").unwrap().line, 2);
    }
}
