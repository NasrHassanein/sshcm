//! Line-level lexer.
//!
//! Classifies a single physical line (with its line terminator already removed)
//! into byte spans. The lexer never allocates and never rejects input: anything
//! it cannot make sense of becomes [`LineKind::Invalid`] so the original bytes
//! are still preserved verbatim by the CST.

use std::ops::Range;

/// A byte range into a line's text.
pub type Span = Range<usize>;

/// Whitespace as OpenSSH defines it (`WHITESPACE` in `misc.h`), minus `\n`,
/// which never appears inside a line.
pub(crate) fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r')
}

/// The syntactic category of one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineKind {
    /// Empty, or whitespace only.
    Blank,
    /// A full-line comment. `body` starts at the `#`.
    Comment {
        /// Leading whitespace.
        indent: Span,
        /// The comment text, including the leading `#`.
        body: Span,
    },
    /// A `Keyword value…` line.
    Directive(DirectiveSpans),
    /// A non-blank line that is not a comment and has no keyword
    /// (e.g. it starts with `=`). Kept verbatim; reported by lint.
    Invalid,
}

/// Byte spans of the parts of a directive line, in order. Concatenating the
/// spans `indent ++ key ++ sep ++ value ++ trailing` yields the whole line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectiveSpans {
    /// Leading whitespace.
    pub indent: Span,
    /// The keyword, exactly as written (case preserved).
    pub key: Span,
    /// Separator between keyword and value: whitespace and/or a single `=`.
    pub sep: Span,
    /// The raw value, with trailing whitespace excluded. May be empty.
    pub value: Span,
    /// Trailing whitespace.
    pub trailing: Span,
}

/// Classify one line. `line` must not contain `\n`.
pub fn classify(line: &str) -> LineKind {
    debug_assert!(!line.contains('\n'), "classify() takes a single line");

    let Some(content_start) = line.find(|c| !is_ws(c)) else {
        return LineKind::Blank;
    };
    let rest = &line[content_start..];

    if rest.starts_with('#') {
        return LineKind::Comment {
            indent: 0..content_start,
            body: content_start..line.len(),
        };
    }

    // Keyword: up to the first whitespace or '='.
    let key_end = content_start + rest.find(|c| is_ws(c) || c == '=').unwrap_or(rest.len());
    if key_end == content_start {
        return LineKind::Invalid;
    }

    // Separator: ws* '='? ws*
    let after_ws = |from: usize| {
        from + line[from..]
            .find(|c| !is_ws(c))
            .unwrap_or(line.len() - from)
    };
    let mut sep_end = after_ws(key_end);
    if line[sep_end..].starts_with('=') {
        sep_end = after_ws(sep_end + 1);
    }

    // Value: everything up to trailing whitespace. `line[sep_end..]` begins
    // with non-whitespace (or is empty), so `value_end >= sep_end`.
    let value_end = sep_end + line[sep_end..].trim_end_matches(is_ws).len();

    LineKind::Directive(DirectiveSpans {
        indent: 0..content_start,
        key: content_start..key_end,
        sep: key_end..sep_end,
        value: sep_end..value_end,
        trailing: value_end..line.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// Classify and return (key, sep, value, trailing) slices for readability.
    fn parts(line: &str) -> (&str, &str, &str, &str, &str) {
        let LineKind::Directive(d) = classify(line) else {
            panic!("not a directive: {line:?}");
        };
        (
            &line[d.indent],
            &line[d.key],
            &line[d.sep],
            &line[d.value],
            &line[d.trailing],
        )
    }

    #[test]
    fn blank_lines() {
        assert_eq!(classify(""), LineKind::Blank);
        assert_eq!(classify("  \t "), LineKind::Blank);
        assert_eq!(classify("\r"), LineKind::Blank);
    }

    #[test]
    fn comments() {
        assert_eq!(
            classify("  # hello"),
            LineKind::Comment {
                indent: 0..2,
                body: 2..9
            }
        );
        assert_eq!(
            classify("#"),
            LineKind::Comment {
                indent: 0..0,
                body: 0..1
            }
        );
    }

    #[test]
    fn separator_styles() {
        assert_eq!(parts("Port 22"), ("", "Port", " ", "22", ""));
        assert_eq!(parts("Port=22"), ("", "Port", "=", "22", ""));
        assert_eq!(parts("Port = 22"), ("", "Port", " = ", "22", ""));
        assert_eq!(
            parts("\tPort\t=\t22\t"),
            ("\t", "Port", "\t=\t", "22", "\t")
        );
        assert_eq!(
            parts("    HostName example.com  "),
            ("    ", "HostName", " ", "example.com", "  ")
        );
    }

    #[test]
    fn only_one_equals_is_separator() {
        // The second '=' belongs to the value.
        assert_eq!(parts("SetEnv=FOO=bar"), ("", "SetEnv", "=", "FOO=bar", ""));
        assert_eq!(parts("SetEnv = =x"), ("", "SetEnv", " = ", "=x", ""));
    }

    #[test]
    fn values_keep_internal_whitespace_and_quotes() {
        assert_eq!(
            parts(r#"IdentityFile "~/My Keys/id_ed25519"  "#),
            ("", "IdentityFile", " ", r#""~/My Keys/id_ed25519""#, "  ")
        );
        assert_eq!(parts("Host a b  c"), ("", "Host", " ", "a b  c", ""));
    }

    #[test]
    fn keyword_without_value() {
        assert_eq!(parts("Host"), ("", "Host", "", "", ""));
        assert_eq!(parts("Host=  "), ("", "Host", "=  ", "", ""));
        assert_eq!(parts("Host   "), ("", "Host", "   ", "", ""));
    }

    #[test]
    fn crlf_remnant_is_trailing_whitespace() {
        assert_eq!(parts("User bob\r"), ("", "User", " ", "bob", "\r"));
    }

    #[test]
    fn invalid_lines() {
        assert_eq!(classify("=value"), LineKind::Invalid);
        assert_eq!(classify("  = value"), LineKind::Invalid);
    }

    #[test]
    fn non_ascii_is_preserved() {
        assert_eq!(parts("User jürgen"), ("", "User", " ", "jürgen", ""));
    }
}
