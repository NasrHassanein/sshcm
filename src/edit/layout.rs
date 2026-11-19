//! Block geometry for edits: which lines a block owns, where new lines go,
//! and how blank-line spacing is kept when blocks are inserted or removed.
//!
//! See the module docs of [`crate::edit`] for why "the lines of a block"
//! here differs from the parser's [`crate::syntax::Block::lines`].

use std::ops::Range;

use crate::syntax::{Document, Eol, Line};

/// Index of block `bi`'s header line. `bi` must be a `Host` or `Match`
/// block (every block an edit targets is one).
pub(super) fn header_of(doc: &Document, bi: usize) -> usize {
    doc.blocks()[bi].lines().start
}

/// Start of the comment run that introduces the header at `header`: comment
/// lines directly above it (no blank line between) that are not indented
/// deeper than the header itself (deeper ones are commented-out options of
/// the previous block).
pub(super) fn intro_start(doc: &Document, header: usize) -> usize {
    let lines = doc.lines();
    let depth = lines[header].indent().len();
    let mut start = header;
    while start > 0 && lines[start - 1].is_comment() && lines[start - 1].indent().len() <= depth {
        start -= 1;
    }
    start
}

/// The lines that make up block `bi` for removal: intro comments, header,
/// body up to the last option, and comments directly after it — but not
/// blank lines or the next block's intro comments.
pub(super) fn block_extent(doc: &Document, bi: usize) -> Range<usize> {
    let block = &doc.blocks()[bi];
    let header = block.lines().start;
    let limit = doc.blocks().get(bi + 1).map_or(block.lines().end, |next| {
        intro_start(doc, next.lines().start)
    });
    let mut end = after_last_option(doc, header + 1..limit);
    while end < limit && doc.lines()[end].is_comment() {
        end += 1;
    }
    intro_start(doc, header)..end
}

/// The lines after block `bi`'s header that an edit treats as the block's
/// options: through its trailing comments (often commented-out options),
/// but not the next block's intro comments.
pub(super) fn option_range(doc: &Document, bi: usize) -> Range<usize> {
    header_of(doc, bi) + 1..block_extent(doc, bi).end
}

/// Where a new option goes in `range`: just after its last directive (or
/// invalid line), so it lands above any trailing comments and blank lines;
/// `range.start` if there is none.
pub(super) fn after_last_option(doc: &Document, range: Range<usize>) -> usize {
    range
        .clone()
        .rev()
        .find(|&i| doc.lines()[i].is_content())
        .map_or(range.start, |i| i + 1)
}

/// Indices in `range` of lines whose keyword is `kw` (case-insensitive).
pub(super) fn keyword_lines(doc: &Document, range: Range<usize>, kw: &str) -> Vec<usize> {
    range.filter(|&i| doc.lines()[i].is_keyword(kw)).collect()
}

/// Delete the lines at `indices`, which must be in ascending order. Works
/// back to front, so each index is still valid when its line is removed.
pub(super) fn remove_lines(doc: &mut Document, indices: &[usize]) {
    for &i in indices.iter().rev() {
        doc.splice(i..i + 1, []);
    }
}

/// Insert a block's lines at `at`, adding blank-line separators in a way
/// that [`remove_range`] reverses exactly.
///
/// `separated` says whether the file separates blocks with blank lines.
/// What is added around the block, by whether the lines just before and
/// after `at` are blank (the start and end of the file count as blank):
///
/// | blank before | blank after | separated file | compact file |
/// |---|---|---|---|
/// | no  | no  | nothing (flush with neighbours) | nothing |
/// | no  | yes | one blank line before           | nothing |
/// | yes | no  | one blank line after            | nothing |
/// | yes | yes | one blank line after            | one blank line after |
///
/// Nothing is added to an empty file. The table follows the *local* style:
/// a block inserted between two lines that touch stays flush with them,
/// even in a separated file.
pub(super) fn insert_block(doc: &mut Document, at: usize, mut block: Vec<Line>, separated: bool) {
    preserving_final_newline(doc, |doc| {
        let eol = doc.preferred_eol();
        for line in &mut block {
            *line = Line::new(line.text(), eol);
        }
        let len = doc.lines().len();
        let blank = |i: usize| doc.lines()[i].is_blank();

        let prev_blank = at == 0 || blank(at - 1);
        let next_blank = at == len || blank(at);
        let lead = separated && !prev_blank && next_blank;
        let trail = len > 0 && prev_blank && (separated || next_blank);

        let mut new = Vec::with_capacity(block.len() + 2);
        if lead {
            new.push(Line::blank(eol));
        }
        new.append(&mut block);
        if trail {
            new.push(Line::blank(eol));
        }
        doc.splice(at..at, new);
    });
}

/// Remove `range`, then drop one now-redundant blank line so spacing
/// returns to what it was before the block was inserted.
pub(super) fn remove_range(doc: &mut Document, range: Range<usize>) {
    let at = range.start;
    doc.splice(range, []);
    let len = doc.lines().len();
    let blank = |i: usize| doc.lines()[i].is_blank();
    let blank_before = at > 0 && blank(at - 1);

    if (at == 0 || blank_before) && at < len && blank(at) {
        doc.splice(at..at + 1, []);
    } else if blank_before && at == len {
        doc.splice(at - 1..at, []);
    }
}

/// Run `f`, then restore the document's final-newline convention: every
/// line but the last gets a terminator, and the last has one iff it had one
/// before (an empty document counts as "had one").
pub(super) fn preserving_final_newline<T>(
    doc: &mut Document,
    f: impl FnOnce(&mut Document) -> T,
) -> T {
    let had_final_newline = doc.lines().last().is_none_or(|l| l.eol() != Eol::None);
    let out = f(doc);
    let eol = doc.preferred_eol();
    let n = doc.lines().len();
    for i in 0..n.saturating_sub(1) {
        if doc.lines()[i].eol() == Eol::None {
            doc.set_eol(i, eol);
        }
    }
    if n > 0 {
        let last = doc.lines()[n - 1].eol();
        match (had_final_newline, last) {
            (true, Eol::None) => doc.set_eol(n - 1, eol),
            (false, _) => doc.set_eol(n - 1, Eol::None),
            (true, _) => {}
        }
    }
    out
}
