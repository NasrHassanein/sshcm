//! Surgical edits to a [`Document`].
//!
//! Every operation changes the minimum set of lines and copies the
//! surrounding style (indentation, separator, keyword spelling, line endings,
//! presence of a final newline). Lines it does not need to touch are never
//! re-rendered.
//!
//! # Which lines belong to a block
//!
//! The parser assigns lines to blocks the way OpenSSH reads them, so a comment
//! sitting above `Host b` technically lives in the block before it. For edits
//! we use a presentation-oriented notion instead (see `block_extent`):
//!
//! - **intro comments**: comments directly above a header (no blank line in
//!   between), not indented deeper than the header, describe that host;
//! - **trailing comments**: comments directly after the block's last option
//!   (before any blank line) are usually commented-out options of the block;
//! - blank lines belong to nobody; spacing is handled by `insert_block` and
//!   `remove_range`, which are designed so that `rm` exactly undoes `add`.

use std::ops::Range;

use crate::hosts::{block_patterns, find_alias};
use crate::model::keyword::{self, Merge};
use crate::model::value::{quote, split_args};
use crate::syntax::{BlockKind, Document, Eol, Line};

/// Why an edit was refused. The document is unchanged when one is returned.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    /// No `Host` block names the alias literally.
    #[error("no Host block names `{0}`")]
    AliasNotFound(String),
    /// The alias is already defined.
    #[error("`{alias}` already exists (Host line {line})")]
    AliasExists {
        /// The alias.
        alias: String,
        /// 1-based line of the existing `Host` line.
        line: usize,
    },
    /// The alias is not usable as a literal host alias.
    #[error("invalid alias `{alias}`: {reason}")]
    InvalidAlias {
        /// The rejected alias.
        alias: String,
        /// Why.
        reason: &'static str,
    },
    /// The keyword is not an `ssh_config` keyword.
    #[error("unknown keyword `{name}`{hint}")]
    UnknownKeyword {
        /// As given.
        name: String,
        /// A " (did you mean …?)" suffix, or empty.
        hint: String,
    },
    /// The keyword changes block structure and has its own command.
    #[error("`{0}` cannot be set as an option; use add, rm, or rename")]
    ReservedKeyword(&'static str),
    /// The value is empty or would break the line structure.
    #[error("invalid value for `{key}`: {reason}")]
    InvalidValue {
        /// The keyword.
        key: String,
        /// Why.
        reason: &'static str,
    },
    /// `--append` was used with a keyword that only takes one value.
    #[error(
        "`{0}` takes a single value; --append only applies to list keywords such as IdentityFile"
    )]
    NotAList(&'static str),
    /// `unset` found nothing to remove.
    #[error("`{key}` is not set in the `{alias}` block")]
    KeyNotSet {
        /// The alias.
        alias: String,
        /// The keyword.
        key: String,
    },
    /// The `Host` line could not be parsed (e.g. unterminated quote).
    #[error("cannot parse the Host line at line {0}")]
    UnparsableHost(usize),
}

/// Where `add` puts a new block.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Position {
    /// Before the first catch-all block (`Host *` or `Match all`) so its
    /// defaults keep applying, otherwise at the end of the file.
    #[default]
    Auto,
    /// Before the block for this alias (and its intro comments).
    Before(String),
    /// After the block for this alias.
    After(String),
}

// ---------------------------------------------------------------------------
// Public operations
// ---------------------------------------------------------------------------

/// Add a new `Host alias` block with `options` (keyword, raw value) pairs.
///
/// # Errors
/// If the alias is invalid or exists, a keyword is unknown/reserved, a value
/// is invalid, or a `Before`/`After` alias does not exist.
pub fn add_host(
    doc: &mut Document,
    alias: &str,
    options: &[(String, String)],
    position: &Position,
) -> Result<(), EditError> {
    validate_alias(alias)?;
    ensure_absent(doc, alias)?;
    let options = options
        .iter()
        .map(|(k, v)| {
            let kw = settable_keyword(k)?;
            validate_value(kw, v)?;
            Ok((kw.to_owned(), v.clone()))
        })
        .collect::<Result<Vec<_>, EditError>>()?;
    insert_host_block(doc, &[alias.to_owned()], &options, position)
}

/// Add a `Host` block with any pattern list (e.g. `*` or `web-* !web-old`),
/// as used by `import`. Unknown keywords are allowed, since imported data
/// may come from a config that uses `IgnoreUnknown`; values are still
/// checked so they cannot break the line structure.
///
/// # Errors
/// If a pattern is malformed, the list has no positive pattern, a keyword
/// is reserved, or a value is invalid.
pub fn add_block(
    doc: &mut Document,
    patterns: &[String],
    options: &[(String, String)],
    position: &Position,
) -> Result<(), EditError> {
    for p in patterns {
        let bad = |reason| EditError::InvalidAlias {
            alias: p.clone(),
            reason,
        };
        if p.is_empty() || p == "!" {
            return Err(bad("pattern is empty"));
        }
        // Spaces are fine (the pattern is quoted when written); control
        // characters could break the line structure.
        if p.chars().any(char::is_control) {
            return Err(bad("pattern contains control characters"));
        }
    }
    if !patterns.iter().any(|p| !p.starts_with('!')) {
        return Err(EditError::InvalidAlias {
            alias: patterns.join(" "),
            reason: "a Host line needs at least one pattern without `!`",
        });
    }
    let options = options
        .iter()
        .map(|(k, v)| {
            let kw = match settable_keyword(k) {
                Ok(kw) => kw.to_owned(),
                Err(EditError::UnknownKeyword { .. })
                    if !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric()) =>
                {
                    k.clone()
                }
                Err(e) => return Err(e),
            };
            validate_value(&kw, v)?;
            Ok((kw, v.clone()))
        })
        .collect::<Result<Vec<_>, EditError>>()?;
    insert_host_block(doc, patterns, &options, position)
}

fn insert_host_block(
    doc: &mut Document,
    patterns: &[String],
    options: &[(String, String)],
    position: &Position,
) -> Result<(), EditError> {
    let at = match position {
        Position::Auto => (0..doc.blocks().len())
            .find(|&bi| is_catch_all(doc, bi))
            .map_or(doc.lines().len(), |bi| {
                intro_start(doc, doc.blocks()[bi].lines().start)
            }),
        Position::Before(a) => {
            let bi = first_block(doc, a)?;
            intro_start(doc, doc.blocks()[bi].lines().start)
        }
        Position::After(a) => block_extent(doc, first_block(doc, a)?).end,
    };

    let style = Style::of(doc);
    let eol = doc.preferred_eol();
    let header: Vec<_> = patterns.iter().map(|p| quote(p)).collect();
    let mut lines = vec![Line::directive(
        "",
        &style.header_keyword,
        &style.header_sep,
        &header.join(" "),
        eol,
    )];
    lines.extend(
        options
            .iter()
            .map(|(k, v)| Line::directive(&style.indent, k, &style.sep, v, eol)),
    );
    insert_block(doc, at, lines, style.separated);
    Ok(())
}

/// Set `key` to `value` in the block for `alias`.
///
/// If the key is present, its first occurrence's value is replaced in place
/// and any further occurrences in that block are removed, so the block ends
/// up with exactly this value. Otherwise a line is added after the block's
/// last option. With `append`, a list keyword (e.g. `IdentityFile`) gets an
/// additional line instead.
///
/// # Errors
/// If the alias is missing, the keyword is unknown/reserved, the value is
/// invalid, or `append` is used with a single-valued keyword.
pub fn set(
    doc: &mut Document,
    alias: &str,
    key: &str,
    value: &str,
    append: bool,
) -> Result<(), EditError> {
    let kw = settable_keyword(key)?;
    validate_value(kw, value)?;
    if append && keyword::merge_rule(kw) != Merge::Accumulate {
        return Err(EditError::NotAList(kw));
    }
    let bi = first_block(doc, alias)?;
    let header = doc.blocks()[bi].lines().start;
    let extent = block_extent(doc, bi);
    let existing: Vec<usize> = (header + 1..extent.end)
        .filter(|&i| doc.lines()[i].is_keyword(kw))
        .collect();

    // Lines matched by keyword are directives, so `with_value` succeeds.
    let replacement = existing
        .first()
        .filter(|_| !append)
        .and_then(|&i| Some((i, doc.lines()[i].with_value(value)?)));

    preserving_final_newline(doc, |doc| {
        if let Some((first, line)) = replacement {
            for &i in existing.iter().skip(1).rev() {
                doc.splice(i..i + 1, []);
            }
            doc.replace_line(first, line);
        } else {
            let at = (header + 1..extent.end)
                .rev()
                .find(|&i| doc.lines()[i].is_content())
                .map_or(header + 1, |i| i + 1);
            let (indent, sep) = Style::of(doc).for_block(doc, header);
            let line = Line::directive(&indent, kw, &sep, value, doc.preferred_eol());
            doc.splice(at..at, [line]);
        }
    });
    Ok(())
}

/// Remove every occurrence of `key` from the block for `alias`. Unknown
/// keywords are accepted so junk lines can be cleaned up.
///
/// # Errors
/// If the alias is missing or the key does not occur in its block.
pub fn unset(doc: &mut Document, alias: &str, key: &str) -> Result<(), EditError> {
    let bi = first_block(doc, alias)?;
    let header = doc.blocks()[bi].lines().start;
    let extent = block_extent(doc, bi);
    let hits: Vec<usize> = (header + 1..extent.end)
        .filter(|&i| doc.lines()[i].is_keyword(key))
        .collect();
    if hits.is_empty() {
        return Err(EditError::KeyNotSet {
            alias: alias.to_owned(),
            key: key.to_owned(),
        });
    }
    preserving_final_newline(doc, |doc| {
        for &i in hits.iter().rev() {
            doc.splice(i..i + 1, []);
        }
    });
    Ok(())
}

/// Remove `alias` from every `Host` line naming it. A block whose `Host` line
/// names other patterns too keeps them; a block left with none is deleted
/// together with its intro and trailing comments.
///
/// # Errors
/// If no block names the alias.
pub fn remove_host(doc: &mut Document, alias: &str) -> Result<(), EditError> {
    let blocks = find_alias(doc, alias);
    if blocks.is_empty() {
        return Err(EditError::AliasNotFound(alias.to_owned()));
    }
    preserving_final_newline(doc, |doc| -> Result<(), EditError> {
        // Back to front, so earlier indices stay valid.
        for &bi in blocks.iter().rev() {
            let header = doc.blocks()[bi].lines().start;
            let tokens = header_tokens(doc, header)?;
            let kept: Vec<&String> = tokens.iter().filter(|t| *t != alias).collect();
            if kept.is_empty() {
                let range = block_extent(doc, bi);
                remove_range(doc, range);
            } else {
                rewrite_header(doc, header, kept);
            }
        }
        Ok(())
    })
}

/// Rename `old` to `new` on every `Host` line naming it.
///
/// # Errors
/// If `new` is invalid or exists, or `old` does not exist.
pub fn rename(doc: &mut Document, old: &str, new: &str) -> Result<(), EditError> {
    validate_alias(new)?;
    ensure_absent(doc, new)?;
    let blocks = find_alias(doc, old);
    if blocks.is_empty() {
        return Err(EditError::AliasNotFound(old.to_owned()));
    }
    for bi in blocks {
        let header = doc.blocks()[bi].lines().start;
        let tokens = header_tokens(doc, header)?;
        let renamed: Vec<String> = tokens
            .into_iter()
            .map(|t| if t == old { new.to_owned() } else { t })
            .collect();
        rewrite_header(doc, header, renamed.iter());
    }
    Ok(())
}

/// Duplicate the block for `src` as `Host dst`, right after it.
///
/// # Errors
/// If `dst` is invalid or exists, or `src` does not exist.
pub fn copy(doc: &mut Document, src: &str, dst: &str) -> Result<(), EditError> {
    validate_alias(dst)?;
    ensure_absent(doc, dst)?;
    let bi = first_block(doc, src)?;
    let header = doc.blocks()[bi].lines().start;
    let end = block_extent(doc, bi).end;

    let renamed = doc.lines()[header]
        .with_value(&quote(dst))
        .ok_or(EditError::UnparsableHost(header + 1))?;
    let mut lines = doc.lines()[header..end].to_vec();
    lines[0] = renamed;
    insert_block(doc, end, lines, Style::of(doc).separated);
    Ok(())
}

// ---------------------------------------------------------------------------
// Block geometry
// ---------------------------------------------------------------------------

/// Start of the comment run that introduces the header at `header`: comment
/// lines directly above it (no blank line between) that are not indented
/// deeper than the header itself (deeper ones are commented-out options of
/// the previous block).
fn intro_start(doc: &Document, header: usize) -> usize {
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
fn block_extent(doc: &Document, bi: usize) -> Range<usize> {
    let block = &doc.blocks()[bi];
    let header = block.lines().start;
    let limit = doc.blocks().get(bi + 1).map_or(block.lines().end, |next| {
        intro_start(doc, next.lines().start)
    });
    let lines = doc.lines();

    let mut end = (header + 1..limit)
        .rev()
        .find(|&i| lines[i].is_content())
        .map_or(header + 1, |i| i + 1);
    while end < limit && lines[end].is_comment() {
        end += 1;
    }
    intro_start(doc, header)..end
}

/// Insert a block's lines at `at`, adding blank-line separators in a way
/// that [`remove_range`] reverses exactly.
fn insert_block(doc: &mut Document, at: usize, mut block: Vec<Line>, separated: bool) {
    preserving_final_newline(doc, |doc| {
        let eol = doc.preferred_eol();
        for line in &mut block {
            *line = Line::new(line.text(), eol);
        }
        let len = doc.lines().len();
        let blank = |i: usize| doc.lines()[i].is_blank();

        // Spacing follows the *local* style: between two directly adjacent
        // lines (no blank between them) the block is inserted flush, even in
        // a file that otherwise separates blocks with blank lines.
        let prev_blank = at == 0 || blank(at - 1);
        let next_blank = at == len || blank(at);
        // In compact files nothing is added, except when blank lines already
        // surround the insertion point: then one trailing blank keeps the
        // edit reversible by `remove_range`.
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
fn remove_range(doc: &mut Document, range: Range<usize>) {
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
fn preserving_final_newline<T>(doc: &mut Document, f: impl FnOnce(&mut Document) -> T) -> T {
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

// ---------------------------------------------------------------------------
// Style detection
// ---------------------------------------------------------------------------

/// Formatting conventions observed in the document, used for new lines.
struct Style {
    header_keyword: String,
    header_sep: String,
    indent: String,
    sep: String,
    /// Whether blocks are separated by blank lines.
    separated: bool,
}

impl Style {
    fn of(doc: &Document) -> Self {
        let lines = doc.lines();
        let headers: Vec<usize> = doc
            .blocks()
            .iter()
            .filter_map(crate::syntax::Block::header)
            .collect();

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

        let body = doc.blocks().iter().skip(1).find_map(|b| {
            doc.block_lines(b)
                .iter()
                .skip(1)
                .find(|l| l.keyword().is_some() && !l.separator().unwrap_or_default().is_empty())
        });
        let (indent, sep) = body.map_or_else(
            || ("    ".to_owned(), " ".to_owned()),
            |l| {
                (
                    l.indent().to_owned(),
                    l.separator().unwrap_or(" ").to_owned(),
                )
            },
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

    /// Indent and separator for a new option in the block at `header`: copy
    /// the block's own first option, falling back to the file's style.
    fn for_block(self, doc: &Document, header: usize) -> (String, String) {
        let bi = doc
            .blocks()
            .iter()
            .position(|b| b.header() == Some(header))
            .expect("header indexes a block");
        doc.block_lines(&doc.blocks()[bi])
            .iter()
            .skip(1)
            .find(|l| l.keyword().is_some() && !l.separator().unwrap_or_default().is_empty())
            .map_or((self.indent, self.sep), |l| {
                (
                    l.indent().to_owned(),
                    l.separator().unwrap_or(" ").to_owned(),
                )
            })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn first_block(doc: &Document, alias: &str) -> Result<usize, EditError> {
    find_alias(doc, alias)
        .first()
        .copied()
        .ok_or_else(|| EditError::AliasNotFound(alias.to_owned()))
}

fn ensure_absent(doc: &Document, alias: &str) -> Result<(), EditError> {
    match find_alias(doc, alias).first() {
        Some(&bi) => Err(EditError::AliasExists {
            alias: alias.to_owned(),
            line: doc.blocks()[bi].lines().start + 1,
        }),
        None => Ok(()),
    }
}

/// `Host *` (with `*` as a positive pattern) or `Match all`.
fn is_catch_all(doc: &Document, bi: usize) -> bool {
    let block = &doc.blocks()[bi];
    match block.kind() {
        BlockKind::Host => block_patterns(doc, block)
            .is_some_and(|p| p.patterns().iter().any(|p| !p.negated && p.glob == "*")),
        BlockKind::Match => block
            .header()
            .and_then(|h| doc.lines()[h].value())
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("all")),
        BlockKind::Global => false,
    }
}

fn header_tokens(doc: &Document, header: usize) -> Result<Vec<String>, EditError> {
    split_args(doc.lines()[header].value().unwrap_or_default())
        .map_err(|_| EditError::UnparsableHost(header + 1))
}

fn rewrite_header<'a>(
    doc: &mut Document,
    header: usize,
    tokens: impl IntoIterator<Item = &'a String>,
) {
    let value: Vec<_> = tokens.into_iter().map(|t| quote(t)).collect();
    let line = doc.lines()[header]
        .with_value(&value.join(" "))
        .expect("header is a directive");
    doc.replace_line(header, line);
}

fn validate_alias(alias: &str) -> Result<(), EditError> {
    let reason = if alias.is_empty() {
        "it is empty"
    } else if alias.contains(['*', '?']) {
        "it contains a wildcard (aliases must be literal names)"
    } else if alias.starts_with('!') {
        "it starts with `!`"
    } else if alias.chars().any(char::is_whitespace) {
        "it contains whitespace"
    } else if alias.chars().any(char::is_control) {
        "it contains control characters"
    } else {
        return Ok(());
    };
    Err(EditError::InvalidAlias {
        alias: alias.to_owned(),
        reason,
    })
}

fn settable_keyword(name: &str) -> Result<&'static str, EditError> {
    let Some(kw) = keyword::canonical(name) else {
        let hint = keyword::suggest(name)
            .map(|s| format!(" (did you mean `{s}`?)"))
            .unwrap_or_default();
        return Err(EditError::UnknownKeyword {
            name: name.to_owned(),
            hint,
        });
    };
    match kw {
        "Host" | "Match" | "Include" => Err(EditError::ReservedKeyword(kw)),
        _ => Ok(kw),
    }
}

fn validate_value(key: &str, value: &str) -> Result<(), EditError> {
    let reason = if value.trim().is_empty() {
        "it is empty (use `unset` to remove an option)"
    } else if value.chars().any(|c| c.is_control() && c != '\t') {
        "it contains a line break or control character"
    } else if value != value.trim() {
        "it has leading or trailing whitespace"
    } else {
        return Ok(());
    };
    Err(EditError::InvalidValue {
        key: key.to_owned(),
        reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn kv(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect()
    }

    fn edited(input: &str, f: impl FnOnce(&mut Document) -> Result<(), EditError>) -> String {
        let mut doc = Document::parse(input);
        f(&mut doc).unwrap();
        doc.render()
    }

    #[test]
    fn add_goes_before_catch_all_and_copies_style() {
        let input = "Host a\n\tUser x\n\nHost *\n\tPort 22\n";
        let out = edited(input, |d| {
            add_host(d, "new", &kv(&[("hostname", "n.example")]), &Position::Auto)
        });
        assert_eq!(
            out,
            "Host a\n\tUser x\n\nHost new\n\tHostName n.example\n\nHost *\n\tPort 22\n"
        );
    }

    #[test]
    fn add_to_empty_file() {
        let out = edited("", |d| {
            add_host(d, "a", &kv(&[("User", "u")]), &Position::Auto)
        });
        assert_eq!(out, "Host a\n    User u\n");
    }

    #[test]
    fn add_in_compact_file_stays_compact() {
        let input = "Host a\n  User x\nHost b\n  User y\n";
        let out = edited(input, |d| add_host(d, "c", &[], &Position::Auto));
        assert_eq!(out, "Host a\n  User x\nHost b\n  User y\nHost c\n");
    }

    #[test]
    fn add_keeps_missing_final_newline() {
        let out = edited("Host a\n  User x", |d| {
            add_host(d, "b", &[], &Position::Auto)
        });
        assert_eq!(out, "Host a\n  User x\n\nHost b");
    }

    #[test]
    fn add_uses_crlf_in_crlf_files() {
        let out = edited("Host a\r\n  User x\r\n", |d| {
            add_host(d, "b", &[], &Position::Auto)
        });
        assert_eq!(out, "Host a\r\n  User x\r\n\r\nHost b\r\n");
    }

    #[test]
    fn intro_comments_move_with_their_block() {
        let input = "Host a\n  User x\n  # Port 22\n\n# the b host\nHost b\n  User y\n";
        // `# Port 22` is indented: a commented-out option of `a`.
        // `# the b host` introduces `b`.
        assert_eq!(
            edited(input, |d| remove_host(d, "b")),
            "Host a\n  User x\n  # Port 22\n"
        );
        assert_eq!(
            edited(input, |d| remove_host(d, "a")),
            "# the b host\nHost b\n  User y\n"
        );
    }

    #[test]
    fn rm_one_of_several_patterns() {
        assert_eq!(
            edited("host  b  c\n  Port 1\n", |d| remove_host(d, "b")),
            "host  c\n  Port 1\n"
        );
    }

    #[test]
    fn set_replaces_value_in_place() {
        let input = "Host a\n  Port = 22   \n  User x\n";
        assert_eq!(
            edited(input, |d| set(d, "a", "port", "2222", false)),
            "Host a\n  Port = 2222   \n  User x\n"
        );
    }

    #[test]
    fn set_adds_after_last_option_not_after_trailing_blank() {
        let input = "Host a\n\tUser x\n\nHost b\n";
        assert_eq!(
            edited(input, |d| set(d, "a", "Port", "1", false)),
            "Host a\n\tUser x\n\tPort 1\n\nHost b\n"
        );
    }

    #[test]
    fn set_collapses_duplicates_and_append_adds() {
        let input = "Host a\n  IdentityFile k1\n  IdentityFile k2\n";
        assert_eq!(
            edited(input, |d| set(d, "a", "IdentityFile", "k3", false)),
            "Host a\n  IdentityFile k3\n"
        );
        assert_eq!(
            edited(input, |d| set(d, "a", "IdentityFile", "k3", true)),
            "Host a\n  IdentityFile k1\n  IdentityFile k2\n  IdentityFile k3\n"
        );
    }

    #[test]
    fn unset_removes_all_occurrences() {
        let input = "Host a\n  User x\n  Port 1\n  user y\n";
        assert_eq!(
            edited(input, |d| unset(d, "a", "USER")),
            "Host a\n  Port 1\n"
        );
    }

    #[test]
    fn rename_and_copy() {
        let input = "Host a b\n  User x\n";
        assert_eq!(
            edited(input, |d| rename(d, "a", "z")),
            "Host z b\n  User x\n"
        );
        assert_eq!(
            edited("Host a\n  User x\n\nHost *\n", |d| copy(d, "a", "c")),
            "Host a\n  User x\n\nHost c\n  User x\n\nHost *\n"
        );
    }

    #[test]
    fn errors() {
        let mut d = Document::parse("Host a\n  User x\n");
        let before = d.clone();
        assert!(matches!(
            add_host(&mut d, "a", &[], &Position::Auto),
            Err(EditError::AliasExists { line: 1, .. })
        ));
        assert!(matches!(
            add_host(&mut d, "w*", &[], &Position::Auto),
            Err(EditError::InvalidAlias { .. })
        ));
        assert_eq!(
            set(&mut d, "a", "Hostnme", "x", false)
                .unwrap_err()
                .to_string(),
            "unknown keyword `Hostnme` (did you mean `HostName`?)"
        );
        assert!(matches!(
            set(&mut d, "a", "Host", "x", false),
            Err(EditError::ReservedKeyword("Host"))
        ));
        assert!(matches!(
            set(&mut d, "a", "User", "x\nHost evil", false),
            Err(EditError::InvalidValue { .. })
        ));
        assert!(matches!(
            set(&mut d, "a", "Port", "1", true),
            Err(EditError::NotAList("Port"))
        ));
        assert!(matches!(
            unset(&mut d, "a", "Port"),
            Err(EditError::KeyNotSet { .. })
        ));
        assert!(matches!(
            remove_host(&mut d, "zz"),
            Err(EditError::AliasNotFound(_))
        ));
        assert_eq!(d, before, "failed edits must not change the document");
    }
}
