//! Opt-in normalisation (`sshcm fmt`): the only operation that rewrites
//! style. It never changes meaning: every directive keeps its keyword and
//! its value byte for byte, and no directive is added, removed, or reordered.
//!
//! Rules:
//! - `Host`/`Match` lines and directives before the first of them are not
//!   indented; directives inside blocks use one consistent indent;
//! - known keywords get their canonical spelling (`hostname` → `HostName`);
//! - the separator becomes a single space (`Key=Value` → `Key Value`);
//! - trailing whitespace is removed; whitespace-only lines become empty;
//! - runs of blank lines collapse to one;
//! - indented comments inside a block follow the block indent; others, and
//!   lines ssh would reject, are left alone;
//! - line endings and the presence of a final newline are preserved.

use crate::model::keyword;
use crate::syntax::{BlockKind, Document, Line, LineKind};

/// Formatting choices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatOptions {
    /// Indent for directives inside `Host`/`Match` blocks.
    pub indent: String,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            indent: "    ".to_owned(),
        }
    }
}

/// Return the formatted text of `doc`.
pub fn format(doc: &Document, opts: &FormatOptions) -> String {
    let mut out = String::new();
    let mut previous_blank = false;

    for block in doc.blocks() {
        let in_block = block.kind() != BlockKind::Global;
        for idx in block.lines() {
            let line = &doc.lines()[idx];
            let text = match line.kind() {
                LineKind::Blank => {
                    if previous_blank {
                        continue;
                    }
                    String::new()
                }
                LineKind::Comment { .. } => {
                    let body = line.text().trim();
                    if in_block && !line.indent().is_empty() && block.header() != Some(idx) {
                        format!("{}{body}", opts.indent)
                    } else {
                        format!("{}{body}", line.indent())
                    }
                }
                LineKind::Invalid => line.text().to_owned(),
                LineKind::Directive(_) => {
                    let indent = if in_block && block.header() != Some(idx) {
                        opts.indent.as_str()
                    } else {
                        ""
                    };
                    directive(line, indent)
                }
            };
            previous_blank = text.is_empty();
            out.push_str(&text);
            out.push_str(line.eol().as_str());
        }
    }
    out
}

fn directive(line: &Line, indent: &str) -> String {
    let written = line.keyword().unwrap_or_default();
    let key = keyword::canonical(written).unwrap_or(written);
    let value = line.value().unwrap_or_default();
    // A value starting with `=` needs an explicit `=` separator, or the
    // lexer would take that `=` as the separator and change the value.
    let sep = if value.is_empty() {
        ""
    } else if value.starts_with('=') {
        " = "
    } else {
        " "
    };
    format!("{indent}{key}{sep}{value}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use proptest::prelude::*;

    fn fmt(s: &str) -> String {
        format(&Document::parse(s), &FormatOptions::default())
    }

    #[test]
    fn normalises_style() {
        let input = "hostname=global.example   \n\n\n\thost  a b\n  user = bob\n\t\tPORT=22\n  # note  \n\n# about c\n   Host c\r\n\tIdentityFile \"~/My Keys/k\"\r\n";
        assert_eq!(
            fmt(input),
            "HostName global.example\n\nHost a b\n    User bob\n    Port 22\n    # note\n\n# about c\nHost c\r\n    IdentityFile \"~/My Keys/k\"\r\n"
        );
    }

    #[test]
    fn keeps_values_starting_with_equals() {
        assert_eq!(
            fmt("SetEnv = =x\nSetEnv=A=b\n"),
            "SetEnv = =x\nSetEnv A=b\n"
        );
    }

    #[test]
    fn leaves_invalid_lines_and_unknown_keywords() {
        assert_eq!(
            fmt("  =orphan\nhost x\n  FooBar=1\n"),
            "  =orphan\nHost x\n    FooBar 1\n"
        );
    }

    #[test]
    fn preserves_missing_final_newline() {
        assert_eq!(fmt("Host a\n  User b"), "Host a\n    User b");
    }

    /// (lowercased keyword, value) of every directive, in order.
    fn meaning(s: &str) -> Vec<(String, String)> {
        Document::parse(s)
            .lines()
            .iter()
            .filter_map(|l| Some((l.keyword()?.to_ascii_lowercase(), l.value()?.to_owned())))
            .collect()
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2048))]

        #[test]
        fn preserves_meaning_and_is_idempotent(s in "([ \t]{0,2}(Host|host|Match|User|SetEnv|Port|# c|=x|)[ \t=]{0,3}[=a-z\"]{0,6}[ \t]{0,2}(\n|\r\n)){0,12}") {
            let once = fmt(&s);
            prop_assert_eq!(meaning(&once), meaning(&s));
            prop_assert_eq!(fmt(&once), once.clone());
        }
    }
}
