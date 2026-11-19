//! Read-only queries over `Host` blocks: listing and lookup by alias.
//!
//! Unlike [`crate::resolve`], these look at each block *in isolation*: they
//! report what a block says, not what a connection would end up using.

use std::path::PathBuf;

use serde::Serialize;

use crate::include::ConfigSet;
use crate::model::forward::{Forward, ForwardError, ForwardKind};
use crate::model::pattern::HostPatterns;
use crate::model::value::split_args;
use crate::syntax::{Block, BlockKind, Document, Line};

/// A summary of one `Host` block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostEntry {
    /// Index into [`Document::blocks`].
    #[serde(skip)]
    pub block: usize,
    /// The patterns on the `Host` line, as written (including any `!`).
    pub patterns: Vec<String>,
    /// 1-based line number of the `Host` line.
    pub line: usize,
    /// First `HostName` in the block.
    pub hostname: Option<String>,
    /// First `User` in the block.
    pub user: Option<String>,
    /// First `Port` in the block.
    pub port: Option<String>,
    /// Every `IdentityFile` in the block.
    pub identity_files: Vec<String>,
    /// Every parsable `LocalForward`/`RemoteForward`/`DynamicForward`, in
    /// order (malformed ones are reported by `lint`, not listed).
    pub forwards: Vec<Forward>,
    /// `ForwardAgent` is enabled (`yes`, a socket path, or `$ENV`).
    pub forward_agent: bool,
    /// `ForwardX11` is enabled.
    pub forward_x11: bool,
}

/// The forward a directive line describes: `None` if the line is not a
/// forwarding keyword, `Some(Err)` if it is but ssh would reject it.
pub fn forward_of(line: &Line) -> Option<Result<Forward, ForwardError>> {
    let kind = ForwardKind::from_keyword(line.keyword()?)?;
    let args = match split_args(line.value().unwrap_or_default()) {
        Ok(a) => a,
        Err(e) => return Some(Err(ForwardError::Bad(e.to_string()))),
    };
    Some(Forward::from_config(kind, &args))
}

fn enabled(value: Option<String>, off: &[&str]) -> bool {
    value.is_some_and(|v| !off.iter().any(|o| v.eq_ignore_ascii_case(o)))
}

/// The `Host` line's patterns of `block`, or `None` for non-`Host` blocks or
/// unparsable `Host` lines.
pub fn block_patterns(doc: &Document, block: &Block) -> Option<HostPatterns> {
    if block.kind() != BlockKind::Host {
        return None;
    }
    let header = &doc.lines()[block.header()?];
    HostPatterns::from_host_value(header.value()?).ok()
}

/// Summarise every `Host` block of one document, in file order.
pub fn host_entries(doc: &Document) -> Vec<HostEntry> {
    doc.blocks()
        .iter()
        .enumerate()
        .filter(|(_, b)| b.kind() == BlockKind::Host)
        .map(|(idx, _)| entry(doc, idx))
        .collect()
}

/// A [`HostEntry`] together with the file it is in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocatedEntry {
    /// Index into [`ConfigSet::files`].
    #[serde(skip)]
    pub file_index: usize,
    /// The file's path.
    pub file: PathBuf,
    /// The block summary.
    #[serde(flatten)]
    pub entry: HostEntry,
}

/// Summarise every `Host` block of a [`ConfigSet`], in the order ssh reads
/// them (so an included file's hosts appear where its `Include` line is).
pub fn host_entries_in(set: &ConfigSet) -> Vec<LocatedEntry> {
    set.blocks_in_read_order()
        .into_iter()
        .filter_map(|(fid, bi)| {
            let file = &set.files()[fid];
            (file.doc.blocks()[bi].kind() == BlockKind::Host).then(|| LocatedEntry {
                file_index: fid,
                file: file.path.clone(),
                entry: entry(&file.doc, bi),
            })
        })
        .collect()
}

fn entry(doc: &Document, idx: usize) -> HostEntry {
    let b = &doc.blocks()[idx];
    let header = b.lines().start;
    let raw = doc.lines()[header].value().unwrap_or_default();
    let patterns =
        split_args(raw).unwrap_or_else(|_| raw.split_whitespace().map(str::to_owned).collect());

    let body = &doc.lines()[b.body()];
    let values = |kw: &'static str| {
        body.iter()
            .filter(move |l| l.is_keyword(kw))
            .filter_map(|l| {
                let v = l.value()?;
                Some(split_args(v).map_or_else(|_| v.to_owned(), |a| a.join(" ")))
            })
    };

    HostEntry {
        block: idx,
        patterns,
        line: header + 1,
        hostname: values("HostName").next(),
        user: values("User").next(),
        port: values("Port").next(),
        identity_files: values("IdentityFile").collect(),
        forwards: body
            .iter()
            .filter_map(forward_of)
            .filter_map(Result::ok)
            .collect(),
        forward_agent: enabled(values("ForwardAgent").next(), &["no", "false"]),
        forward_x11: enabled(values("ForwardX11").next(), &["no", "false"]),
    }
}

/// Indices of `Host` blocks naming `alias` literally (not via wildcard).
/// More than one result means the alias is defined twice; only the first
/// can ever take effect for options both blocks set.
pub fn find_alias(doc: &Document, alias: &str) -> Vec<usize> {
    doc.blocks()
        .iter()
        .enumerate()
        .filter(|(_, b)| block_patterns(doc, b).is_some_and(|p| p.contains_alias(alias)))
        .map(|(i, _)| i)
        .collect()
}

/// `(file, block)` of every `Host` block in a [`ConfigSet`] naming `alias`
/// literally, in the order ssh reads them: the first is the one that wins.
pub fn find_alias_in(set: &ConfigSet, alias: &str) -> Vec<(usize, usize)> {
    set.blocks_in_read_order()
        .into_iter()
        .filter(|&(fid, bi)| {
            let doc = &set.files()[fid].doc;
            block_patterns(doc, &doc.blocks()[bi]).is_some_and(|p| p.contains_alias(alias))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const CFG: &str = "User global\nHost a \"b c\"\n  HostName a.example\n  IdentityFile k1\n  IdentityFile k2\nHost *.x !a\n  Port 2\nHost a\n";

    #[test]
    fn entries() {
        let doc = Document::parse(CFG);
        let e = host_entries(&doc);
        assert_eq!(e.len(), 3);
        assert_eq!(e[0].patterns, ["a", "b c"]);
        assert_eq!(e[0].line, 2);
        assert_eq!(e[0].hostname.as_deref(), Some("a.example"));
        assert_eq!(e[0].identity_files, ["k1", "k2"]);
        assert_eq!(e[0].user, None); // global User is not part of the block
        assert_eq!(e[1].patterns, ["*.x", "!a"]);
        assert_eq!(e[1].port.as_deref(), Some("2"));
    }

    #[test]
    fn forwards_and_agent_x11() {
        let doc = Document::parse(
            "Host a\n  LocalForward 8080 db:80\n  LocalForward 8080:bad\n  RemoteForward 1080\n  ForwardAgent ~/agent.sock\n  ForwardX11 no\nHost b\n  ForwardX11 yes\n",
        );
        let e = host_entries(&doc);
        let shorts: Vec<_> = e[0].forwards.iter().map(Forward::short).collect();
        assert_eq!(
            shorts,
            ["L8080→db:80", "R1080(socks)"],
            "malformed line skipped"
        );
        assert!(e[0].forward_agent, "a socket path enables agent forwarding");
        assert!(!e[0].forward_x11);
        assert!(!e[1].forward_agent);
        assert!(e[1].forward_x11);
    }

    #[test]
    fn alias_lookup_is_literal() {
        let doc = Document::parse(CFG);
        assert_eq!(find_alias(&doc, "a"), [1, 3]);
        assert_eq!(find_alias(&doc, "b c"), [1]);
        assert_eq!(find_alias(&doc, "y.x"), Vec::<usize>::new()); // wildcard, not alias
    }
}
