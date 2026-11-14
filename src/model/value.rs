//! Splitting a raw directive value into arguments, exactly as OpenSSH's
//! `argv_split()` does (misc.c), and quoting arguments for writing.

use std::borrow::Cow;

/// A value could not be split into arguments.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SplitError {
    /// A `"` or `'` was opened and never closed.
    #[error("unterminated {0} quote")]
    UnterminatedQuote(char),
}

/// Split `value` into arguments.
///
/// Rules (mirroring OpenSSH):
/// - arguments are separated by spaces/tabs;
/// - `"…"` and `'…'` group text, and the quotes are removed;
/// - `\` escapes `"`, `'`, `\`, and (outside quotes) a space; other `\` are literal;
/// - a `#` at the start of an argument begins a comment that ends the line.
///
/// # Errors
/// Returns [`SplitError::UnterminatedQuote`] if a quote is never closed.
pub fn split_args(value: &str) -> Result<Vec<String>, SplitError> {
    let mut args = Vec::new();
    let mut chars = value.chars().peekable();

    loop {
        // Skip separators between arguments.
        while chars.next_if(|c| matches!(c, ' ' | '\t')).is_some() {}
        match chars.peek() {
            None | Some('#') => break,
            Some(_) => {}
        }

        let mut arg = String::new();
        let mut quote: Option<char> = None;
        while let Some(&c) = chars.peek() {
            match (c, quote) {
                ('\\', _) => {
                    chars.next();
                    match chars.peek() {
                        Some(&n @ ('"' | '\'' | '\\')) => {
                            arg.push(n);
                            chars.next();
                        }
                        Some(' ') if quote.is_none() => {
                            arg.push(' ');
                            chars.next();
                        }
                        _ => arg.push('\\'),
                    }
                    continue;
                }
                (' ' | '\t', None) => break,
                ('"' | '\'', None) => quote = Some(c),
                (q, Some(open)) if q == open => quote = None,
                _ => arg.push(c),
            }
            chars.next();
        }
        if let Some(q) = quote {
            return Err(SplitError::UnterminatedQuote(q));
        }
        args.push(arg);
    }
    Ok(args)
}

/// Quote `arg` so that [`split_args`] reads it back as a single argument.
pub fn quote(arg: &str) -> Cow<'_, str> {
    let needs = arg.is_empty()
        || arg.starts_with('#')
        || arg
            .chars()
            .any(|c| matches!(c, ' ' | '\t' | '"' | '\'' | '\\'));
    if !needs {
        return Cow::Borrowed(arg);
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    for c in arg.chars() {
        if matches!(c, '"' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn s(v: &str) -> Vec<String> {
        split_args(v).unwrap()
    }

    #[test]
    fn plain_and_whitespace() {
        assert_eq!(s("a b\t c"), ["a", "b", "c"]);
        assert_eq!(s(""), Vec::<String>::new());
        assert_eq!(s("   "), Vec::<String>::new());
    }

    #[test]
    fn quotes_group_and_are_removed() {
        assert_eq!(s(r#""~/My Keys/id" x"#), ["~/My Keys/id", "x"]);
        assert_eq!(s("'a b'c"), ["a bc"]);
        assert_eq!(s(r#"a"b c"d"#), ["ab cd"]);
        assert_eq!(s(r#""it's""#), ["it's"]);
        assert_eq!(s(r#""""#), [""]);
    }

    #[test]
    fn escapes() {
        assert_eq!(s(r"a\ b"), ["a b"]);
        assert_eq!(s(r#"\"x\""#), [r#""x""#]);
        assert_eq!(s(r"C:\path"), [r"C:\path"]); // unrecognised escape is literal
        assert_eq!(s(r#""a\ b""#), [r"a\ b"]); // `\ ` inside quotes is literal
    }

    #[test]
    fn comments_end_the_line_only_at_token_start() {
        assert_eq!(s("a # trailing comment"), ["a"]);
        assert_eq!(s("a#b"), ["a#b"]);
        assert_eq!(s("# all comment"), Vec::<String>::new());
    }

    #[test]
    fn unterminated_quote() {
        assert_eq!(
            split_args(r#"a "b"#),
            Err(SplitError::UnterminatedQuote('"'))
        );
    }

    #[test]
    fn quote_only_when_needed() {
        assert_eq!(quote("plain"), "plain");
        assert_eq!(quote("a b"), r#""a b""#);
        assert_eq!(quote(""), r#""""#);
        assert_eq!(quote("#x"), r##""#x""##);
    }

    proptest! {
        #[test]
        fn quote_round_trips(arg in any::<String>().prop_filter("no NUL/newline", |a| !a.contains(['\n', '\0']))) {
            let quoted = quote(&arg);
            prop_assert_eq!(split_args(&quoted).unwrap(), vec![arg]);
        }
    }
}
