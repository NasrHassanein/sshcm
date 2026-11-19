//! Input checks run before any edit, so a refused edit never leaves the
//! document half-changed and nothing written can break the file's line
//! structure.

use super::EditError;
use crate::model::keyword;

/// An alias for `add`, `rename`, or `cp`: a literal host name, since the
/// commands that look it up later match it literally.
pub(super) fn validate_alias(alias: &str) -> Result<(), EditError> {
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

/// A `Host` pattern list for `import`. Unlike aliases, patterns may use
/// wildcards, `!`, and spaces (they are quoted when written), but the list
/// needs at least one positive pattern or the block could never apply.
pub(super) fn validate_patterns(patterns: &[String]) -> Result<(), EditError> {
    for p in patterns {
        let bad = |reason| EditError::InvalidAlias {
            alias: p.clone(),
            reason,
        };
        if p.is_empty() || p == "!" {
            return Err(bad("pattern is empty"));
        }
        // Control characters could break the line structure.
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
    Ok(())
}

/// The canonical spelling of a keyword that may be set as an option:
/// known to ssh, and not one that shapes the file (`Host`, `Match`,
/// `Include`), which have their own commands.
pub(super) fn settable_keyword(name: &str) -> Result<&'static str, EditError> {
    let Some(kw) = keyword::canonical(name) else {
        return Err(EditError::UnknownKeyword {
            name: name.to_owned(),
            hint: keyword::hint(name),
        });
    };
    if keyword::is_structural(kw) {
        return Err(EditError::ReservedKeyword(kw));
    }
    Ok(kw)
}

/// Like [`settable_keyword`], but also accepts a well-formed keyword ssh
/// doesn't know, kept as written: imported data may come from a config that
/// relies on `IgnoreUnknown`.
fn importable_keyword(name: &str) -> Result<String, EditError> {
    match settable_keyword(name) {
        Ok(kw) => Ok(kw.to_owned()),
        Err(EditError::UnknownKeyword { .. })
            if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            Ok(name.to_owned())
        }
        Err(e) => Err(e),
    }
}

/// A value to write after a keyword: non-empty, a single line, and without
/// surrounding whitespace (which the lexer would not keep as part of it).
pub(super) fn validate_value(key: &str, value: &str) -> Result<(), EditError> {
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

/// `(keyword, value)` options for a new block, with each keyword checked
/// by `keyword_check` and replaced by its canonical spelling.
fn checked<K: Into<String>>(
    options: &[(String, String)],
    keyword_check: impl Fn(&str) -> Result<K, EditError>,
) -> Result<Vec<(String, String)>, EditError> {
    options
        .iter()
        .map(|(k, v)| {
            let kw: String = keyword_check(k)?.into();
            validate_value(&kw, v)?;
            Ok((kw, v.clone()))
        })
        .collect()
}

/// Options for `add`: every keyword must be settable.
pub(super) fn checked_options(
    options: &[(String, String)],
) -> Result<Vec<(String, String)>, EditError> {
    checked(options, settable_keyword)
}

/// Options for `import`: unknown (but well-formed) keywords are allowed.
pub(super) fn importable_options(
    options: &[(String, String)],
) -> Result<Vec<(String, String)>, EditError> {
    checked(options, importable_keyword)
}
