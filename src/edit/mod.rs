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
//! we use a presentation-oriented notion instead (see `layout::block_extent`):
//!
//! - **intro comments**: comments directly above a header (no blank line in
//!   between), not indented deeper than the header, describe that host;
//! - **trailing comments**: comments directly after the block's last option
//!   (before any blank line) are usually commented-out options of the block;
//! - blank lines belong to nobody; spacing is handled by `insert_block` and
//!   `remove_range`, which are designed so that `rm` exactly undoes `add`.
//!
//! # Layout of this module
//!
//! - this file: the public operations, and finding/rewriting `Host` lines;
//! - `validate`: checks on aliases, patterns, keywords, and values, all run
//!   before the document is touched;
//! - `layout`: block geometry, insertion and removal with blank-line spacing;
//! - `style`: the indentation and separators new lines copy from the file.

mod layout;
mod style;
mod validate;

use crate::hosts::{block_patterns, find_alias, forward_of};
use crate::model::forward::{Endpoint, Forward, ForwardKind};
use crate::model::keyword::{self, Merge};
use crate::model::value::{quote, split_args};
use crate::syntax::{BlockKind, Document, Line};

use layout::{
    after_last_option, block_extent, header_of, insert_block, intro_start, keyword_lines,
    option_range, preserving_final_newline, remove_lines, remove_range,
};
use style::Style;
use validate::{
    checked_options, importable_options, settable_keyword, validate_alias, validate_patterns,
    validate_value,
};

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
    /// `forward add` for a listen endpoint the block already forwards.
    #[error("`{alias}` already has a forward listening on {listen} ({existing})")]
    ForwardExists {
        /// The alias.
        alias: String,
        /// The listen endpoint, e.g. `-L 8080`.
        listen: String,
        /// The existing forward, e.g. `L8080→db:80`.
        existing: String,
    },
    /// `forward rm` found no matching forward.
    #[error("`{alias}` has no forward listening on {listen}")]
    ForwardNotFound {
        /// The alias.
        alias: String,
        /// The listen endpoint, e.g. `-L 8080`.
        listen: String,
    },
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
    let options = checked_options(options)?;
    insert_host_block(doc, &[alias.to_owned()], &options, position)
}

/// The text `add_host` would insert for `alias`, in this document's style
/// (indent, separator, `Host` spelling, line endings), without blank-line
/// separators. Used by `parse view` so it prints exactly what `parse add`
/// writes.
///
/// # Errors
/// If the alias is invalid, a keyword is unknown/reserved, or a value is
/// invalid. Whether the alias already exists is not checked.
pub fn render_host_block(
    doc: &Document,
    alias: &str,
    options: &[(String, String)],
) -> Result<String, EditError> {
    validate_alias(alias)?;
    let options = checked_options(options)?;
    Ok(host_block_lines(doc, &[alias.to_owned()], &options)
        .iter()
        .map(Line::to_string)
        .collect())
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
    validate_patterns(patterns)?;
    let options = importable_options(options)?;
    insert_host_block(doc, patterns, &options, position)
}

/// Insert a new `Host` block at `position`, separated from its neighbours
/// the way the file separates blocks.
fn insert_host_block(
    doc: &mut Document,
    patterns: &[String],
    options: &[(String, String)],
    position: &Position,
) -> Result<(), EditError> {
    let at = match position {
        Position::Auto => (0..doc.blocks().len())
            .find(|&bi| is_catch_all(doc, bi))
            .map_or(doc.lines().len(), |bi| intro_start(doc, header_of(doc, bi))),
        Position::Before(a) => intro_start(doc, header_of(doc, first_block(doc, a)?)),
        Position::After(a) => block_extent(doc, first_block(doc, a)?).end,
    };

    let lines = host_block_lines(doc, patterns, options);
    insert_block(doc, at, lines, Style::of(doc).separated);
    Ok(())
}

/// The lines of a new `Host` block in `doc`'s style. Shared by insertion and
/// `render_host_block`, so what `parse view` shows is what `add` writes.
fn host_block_lines(
    doc: &Document,
    patterns: &[String],
    options: &[(String, String)],
) -> Vec<Line> {
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
    lines
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
    let options = option_range(doc, bi);
    let existing = keyword_lines(doc, options.clone(), kw);

    // Lines matched by keyword are directives, so `with_value` succeeds.
    let replacement = existing
        .first()
        .filter(|_| !append)
        .and_then(|&i| Some((i, doc.lines()[i].with_value(value)?)));

    preserving_final_newline(doc, |doc| {
        if let Some((first, line)) = replacement {
            // Removing the later duplicates first leaves `first` in place.
            remove_lines(doc, &existing[1..]);
            doc.replace_line(first, line);
        } else {
            let at = after_last_option(doc, options);
            let (indent, sep) = Style::of(doc).for_block(doc, bi);
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
    let hits = keyword_lines(doc, option_range(doc, bi), key);
    if hits.is_empty() {
        return Err(EditError::KeyNotSet {
            alias: alias.to_owned(),
            key: key.to_owned(),
        });
    }
    preserving_final_newline(doc, |doc| remove_lines(doc, &hits));
    Ok(())
}

/// Add a forward to the block for `alias`, after its last option.
///
/// # Errors
/// If the alias is missing, or the block already forwards the same kind on
/// the same listen endpoint (ssh would fail with "Address already in use").
pub fn add_forward(doc: &mut Document, alias: &str, forward: &Forward) -> Result<(), EditError> {
    let bi = first_block(doc, alias)?;
    if let Some(existing) = block_forwards(doc, bi)
        .into_iter()
        .map(|(_, f)| f)
        .find(|f| f.listens_on(forward.kind, &forward.listen))
    {
        return Err(EditError::ForwardExists {
            alias: alias.to_owned(),
            listen: format!("-{} {}", forward.kind.flag(), forward.listen),
            existing: existing.short(),
        });
    }
    set(
        doc,
        alias,
        forward.keyword(),
        &forward.to_config_value(),
        true,
    )
}

/// Remove the forward(s) of `kind` listening on `listen` from the block for
/// `alias`, leaving the block's other forwards alone.
///
/// # Errors
/// If the alias is missing or no such forward exists.
pub fn remove_forward(
    doc: &mut Document,
    alias: &str,
    kind: ForwardKind,
    listen: &Endpoint,
) -> Result<(), EditError> {
    let bi = first_block(doc, alias)?;
    let hits: Vec<usize> = block_forwards(doc, bi)
        .into_iter()
        .filter(|(_, f)| f.listens_on(kind, listen))
        .map(|(i, _)| i)
        .collect();
    if hits.is_empty() {
        return Err(EditError::ForwardNotFound {
            alias: alias.to_owned(),
            listen: format!("-{} {listen}", kind.flag()),
        });
    }
    preserving_final_newline(doc, |doc| remove_lines(doc, &hits));
    Ok(())
}

/// `(line index, forward)` for every parsable forward in block `bi`.
fn block_forwards(doc: &Document, bi: usize) -> Vec<(usize, Forward)> {
    option_range(doc, bi)
        .filter_map(|i| Some((i, forward_of(&doc.lines()[i])?.ok()?)))
        .collect()
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
            let header = header_of(doc, bi);
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
        let header = header_of(doc, bi);
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
    let header = header_of(doc, bi);
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
// Finding and rewriting Host lines
// ---------------------------------------------------------------------------

/// The first block naming `alias` literally: the one ssh reads first.
fn first_block(doc: &Document, alias: &str) -> Result<usize, EditError> {
    find_alias(doc, alias)
        .first()
        .copied()
        .ok_or_else(|| EditError::AliasNotFound(alias.to_owned()))
}

/// Refuse to create `alias` if a block already names it.
fn ensure_absent(doc: &Document, alias: &str) -> Result<(), EditError> {
    match find_alias(doc, alias).first() {
        Some(&bi) => Err(EditError::AliasExists {
            alias: alias.to_owned(),
            line: header_of(doc, bi) + 1,
        }),
        None => Ok(()),
    }
}

/// `Host *` (with `*` as a positive pattern) or `Match all`: a block of
/// defaults for every host, which new blocks must go before.
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

/// The patterns on the `Host` line at `header`, unquoted.
fn header_tokens(doc: &Document, header: usize) -> Result<Vec<String>, EditError> {
    split_args(doc.lines()[header].value().unwrap_or_default())
        .map_err(|_| EditError::UnparsableHost(header + 1))
}

/// Replace the patterns on the `Host` line at `header`, quoting as needed;
/// the line's indent, keyword spelling, and separator are kept.
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
    fn forwards_add_and_remove_precisely() {
        use crate::model::forward::parse_listen;
        let input = "Host db\n\tHostName 10.0.0.5\n\tLocalForward 9000 a:1\n\nHost *\n";
        let fwd = |k, s| Forward::from_cli(k, s).unwrap();
        let out = edited(input, |d| {
            add_forward(d, "db", &fwd(ForwardKind::Local, "5432:localhost:5432"))?;
            add_forward(d, "db", &fwd(ForwardKind::Dynamic, "1080"))
        });
        assert_eq!(
            out,
            "Host db\n\tHostName 10.0.0.5\n\tLocalForward 9000 a:1\n\tLocalForward 5432 localhost:5432\n\tDynamicForward 1080\n\nHost *\n"
        );

        // Removing one forward leaves the others, and add+rm is the identity.
        let mut d = Document::parse(&out);
        remove_forward(
            &mut d,
            "db",
            ForwardKind::Local,
            &parse_listen(ForwardKind::Local, "5432").unwrap(),
        )
        .unwrap();
        remove_forward(
            &mut d,
            "db",
            ForwardKind::Dynamic,
            &parse_listen(ForwardKind::Dynamic, "1080").unwrap(),
        )
        .unwrap();
        assert_eq!(d.render(), input);

        let mut d = Document::parse(input);
        assert!(matches!(
            add_forward(&mut d, "db", &fwd(ForwardKind::Local, "9000:b:2")),
            Err(EditError::ForwardExists { .. })
        ));
        assert!(
            matches!(
                remove_forward(
                    &mut d,
                    "db",
                    ForwardKind::Remote,
                    &parse_listen(ForwardKind::Remote, "9000").unwrap()
                ),
                Err(EditError::ForwardNotFound { .. })
            ),
            "kind must match too"
        );
        assert_eq!(d.render(), input);
    }

    #[test]
    fn render_matches_what_add_inserts() {
        let input = "Host a\n\tUser x\r\n";
        let opts = kv(&[("HostName", "h.example"), ("Port", "2222")]);
        let doc = Document::parse(input);
        let shown = render_host_block(&doc, "new", &opts).unwrap();
        assert_eq!(shown, "Host new\n\tHostName h.example\n\tPort 2222\n");
        let mut d = doc.clone();
        add_host(&mut d, "new", &opts, &Position::Auto).unwrap();
        assert!(d.render().contains(&shown), "{:?}", d.render());
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
