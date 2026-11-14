//! JSON export/import of `Host` blocks.
//!
//! ```json
//! { "version": 1,
//!   "hosts": [
//!     { "host": ["web", "web.example"],
//!       "options": [["HostName", "10.0.0.5"], ["IdentityFile", "~/.ssh/a"], ["IdentityFile", "~/.ssh/b"]],
//!       "file": "/home/me/.ssh/config", "line": 12 } ] }
//! ```
//!
//! Options are an ordered list of `[keyword, value]` pairs rather than an
//! object: keywords may repeat and order matters to ssh. Values are kept
//! exactly as written (quotes included), so a round trip is faithful.
//! `file`/`line` are informational and ignored on import.
//!
//! Scope: `Host` blocks only. Global options, `Match` blocks, and `Include`
//! lines are configuration *structure* rather than host data and are not
//! exported.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::hosts::block_patterns;
use crate::include::ConfigSet;
use crate::model::keyword;
use crate::model::value::split_args;

/// Current format version.
pub const VERSION: u32 = 1;

/// A whole export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Export {
    /// Format version; currently 1.
    pub version: u32,
    /// Host blocks in the order ssh reads them.
    pub hosts: Vec<HostRecord>,
}

/// One `Host` block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostRecord {
    /// The patterns on the `Host` line, unquoted (e.g. `["web-*", "!web-old"]`).
    pub host: Vec<String>,
    /// `[keyword, raw value]` pairs in file order.
    pub options: Vec<(String, String)>,
    /// Where the block came from (export only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<PathBuf>,
    /// 1-based line of the `Host` line (export only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
}

impl HostRecord {
    /// Literal aliases (no wildcards, not negated) on the `Host` line.
    pub fn aliases(&self) -> impl Iterator<Item = &str> {
        self.host
            .iter()
            .filter(|p| !p.starts_with('!') && !p.contains(['*', '?']))
            .map(String::as_str)
    }
}

/// Export every `Host` block of `set`.
pub fn export(set: &ConfigSet) -> Export {
    let hosts = set
        .blocks_in_read_order()
        .into_iter()
        .filter_map(|(fid, bi)| {
            let file = &set.files()[fid];
            let block = &file.doc.blocks()[bi];
            block_patterns(&file.doc, block)?;
            let header = block.lines().start;
            let raw = file.doc.lines()[header].value().unwrap_or_default();
            let host = split_args(raw).ok()?;
            let options = file.doc.lines()[header + 1..block.lines().end]
                .iter()
                .filter(|l| !l.is_keyword("Include"))
                .filter_map(|l| {
                    let kw = l.keyword()?;
                    let canon = keyword::canonical(kw).unwrap_or(kw);
                    Some((canon.to_owned(), l.value()?.to_owned()))
                })
                .collect();
            Some(HostRecord {
                host,
                options,
                file: Some(file.path.clone()),
                line: Some(header + 1),
            })
        })
        .collect();
    Export {
        version: VERSION,
        hosts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::{Position, add_block};
    use crate::syntax::Document;
    use pretty_assertions::assert_eq;

    const CFG: &str = "User global\nHost web \"my box\"\n  hostname 10.0.0.5\n  IdentityFile \"~/My Keys/a\"\n  IdentityFile ~/b\n  Include extra\nMatch all\n  Port 1\nHost * !jump\n  ServerAliveInterval 60\n";

    fn strip(mut e: Export) -> Export {
        for h in &mut e.hosts {
            h.file = None;
            h.line = None;
        }
        e
    }

    #[test]
    fn exports_host_blocks_only() {
        let e = export(&ConfigSet::single("config", Document::parse(CFG)));
        assert_eq!(e.hosts.len(), 2);
        assert_eq!(e.hosts[0].host, ["web", "my box"]);
        assert_eq!(
            e.hosts[0].options,
            [
                ("HostName".to_owned(), "10.0.0.5".to_owned()),
                ("IdentityFile".to_owned(), "\"~/My Keys/a\"".to_owned()),
                ("IdentityFile".to_owned(), "~/b".to_owned()),
            ]
        );
        assert_eq!(e.hosts[0].line, Some(2));
        assert_eq!(e.hosts[1].host, ["*", "!jump"]);
        assert_eq!(e.hosts[0].aliases().collect::<Vec<_>>(), ["web", "my box"]);
    }

    #[test]
    fn export_import_round_trip() {
        let first = strip(export(&ConfigSet::single("a", Document::parse(CFG))));
        let json = serde_json::to_string(&first).unwrap();
        let parsed: Export = serde_json::from_str(&json).unwrap();

        let mut doc = Document::parse("");
        for h in &parsed.hosts {
            add_block(&mut doc, &h.host, &h.options, &Position::Auto).unwrap();
        }
        let second = strip(export(&ConfigSet::single("b", doc)));
        // Import inserts before the first catch-all, so `Host *` ends last
        // either way; the records themselves are identical.
        assert_eq!(second, first);
    }
}
