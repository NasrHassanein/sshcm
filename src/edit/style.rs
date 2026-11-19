//! Style detection: new lines copy the formatting the file already uses.

use super::layout::intro_start;
use crate::syntax::{Block, Document, Line};

/// Formatting conventions observed in the document, used for new lines.
pub(super) struct Style {
    /// `Host` as spelled on existing `Host` lines (e.g. `host`).
    pub header_keyword: String,
    /// Separator after `Host` (e.g. `" "` or `"="`).
    pub header_sep: String,
    /// Indent of options inside blocks.
    pub indent: String,
    /// Separator between an option's keyword and value.
    pub sep: String,
    /// Whether blocks are separated by blank lines.
    pub separated: bool,
}

/// Fallback indent, for files with no indented options to copy.
const DEFAULT_INDENT: &str = "    ";

impl Style {
    pub(super) fn of(doc: &Document) -> Self {
        let lines = doc.lines();
        let headers: Vec<usize> = doc.blocks().iter().filter_map(Block::header).collect();

        let host_header = headers
            .iter()
            .map(|&h| &lines[h])
            .find(|l| l.is_keyword("Host"));
        let (header_keyword, header_sep) = host_header.map_or_else(
            || ("Host".to_owned(), " ".to_owned()),
            |l| {
                let sep = l.separator().filter(|s| !s.is_empty()).unwrap_or(" ");
                (l.keyword().unwrap_or("Host").to_owned(), sep.to_owned())
            },
        );

        // The first option of any Host/Match block (the global block's
        // options are not indented, so they say nothing about blocks).
        let (indent, sep) = doc
            .blocks()
            .iter()
            .skip(1)
            .find_map(|b| first_option(&lines[b.body()]))
            .map_or_else(
                || (DEFAULT_INDENT.to_owned(), " ".to_owned()),
                indent_and_sep,
            );

        // Compact only with positive evidence: some header directly follows
        // other content, and none is preceded by a blank line.
        let preceded_by = |blank: bool| {
            headers.iter().any(|&h| {
                let s = intro_start(doc, h);
                s > 0 && lines[s - 1].is_blank() == blank
            })
        };
        let separated = preceded_by(true) || !preceded_by(false);

        Self {
            header_keyword,
            header_sep,
            indent,
            sep,
            separated,
        }
    }

    /// Indent and separator for a new option in block `bi`: copy the
    /// block's own first option, falling back to the file's style.
    pub(super) fn for_block(self, doc: &Document, bi: usize) -> (String, String) {
        first_option(&doc.lines()[doc.blocks()[bi].body()])
            .map_or((self.indent, self.sep), indent_and_sep)
    }
}

/// The first directive in `lines` that has a separator to copy (`Host`
/// alone on a line has none).
fn first_option(lines: &[Line]) -> Option<&Line> {
    lines
        .iter()
        .find(|l| l.keyword().is_some() && !l.separator().unwrap_or_default().is_empty())
}

fn indent_and_sep(line: &Line) -> (String, String) {
    (
        line.indent().to_owned(),
        line.separator().unwrap_or(" ").to_owned(),
    )
}
