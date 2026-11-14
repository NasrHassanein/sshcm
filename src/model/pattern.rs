//! Host pattern lists, as used on `Host` lines and in `Match host …`.
//!
//! Matching is **case-sensitive**: verified against OpenSSH 10.2, where
//! `ssh -G BAR` does not match `Host bar` (and vice versa).

use super::value::{SplitError, split_args};

/// One pattern from a pattern list, e.g. `*.example.com` or `!bastion`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPattern {
    /// True for patterns written with a leading `!`.
    pub negated: bool,
    /// The wildcard pattern, without the `!`.
    pub glob: String,
}

impl HostPattern {
    fn parse(token: &str) -> Self {
        match token.strip_prefix('!') {
            Some(glob) => Self {
                negated: true,
                glob: glob.to_owned(),
            },
            None => Self {
                negated: false,
                glob: token.to_owned(),
            },
        }
    }

    /// True if the glob (ignoring negation) matches `host`.
    pub fn glob_matches(&self, host: &str) -> bool {
        wildcard_match(&self.glob, host)
    }

    /// True if this is a plain alias: not negated and no wildcards.
    pub fn is_literal(&self) -> bool {
        !self.negated && !self.glob.contains(['*', '?'])
    }
}

/// An ordered list of host patterns.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HostPatterns(Vec<HostPattern>);

impl HostPatterns {
    /// Parse the value of a `Host` line: whitespace-separated, quotes allowed.
    ///
    /// # Errors
    /// Fails if the value contains an unterminated quote.
    pub fn from_host_value(value: &str) -> Result<Self, SplitError> {
        Ok(Self(
            split_args(value)?
                .iter()
                .map(|t| HostPattern::parse(t))
                .collect(),
        ))
    }

    /// Parse a comma-separated list, as in `Match host a,!b,*.c`.
    ///
    /// OpenSSH lowercases these patterns (`match_pattern_list(…, dolower=1)`),
    /// so we do too.
    pub fn from_comma_list(list: &str) -> Self {
        Self(
            list.split(',')
                .filter(|p| !p.is_empty())
                .map(|p| HostPattern::parse(&p.to_lowercase()))
                .collect(),
        )
    }

    /// The patterns, in order.
    pub fn patterns(&self) -> &[HostPattern] {
        &self.0
    }

    /// True if `alias` appears verbatim as a non-negated, wildcard-free pattern.
    /// This is how `show`/`set`/`rm` find "the block for alias X".
    pub fn contains_alias(&self, alias: &str) -> bool {
        self.0.iter().any(|p| p.is_literal() && p.glob == alias)
    }

    /// Does this pattern list select `host`?
    ///
    /// This is the heart of `Host` block selection and follows OpenSSH's
    /// `match_pattern_list()` semantics exactly:
    /// - `Host a b c` matches if *any* positive pattern matches;
    /// - `Host * !bastion` must NOT match `bastion`;
    /// - `Host !bastion` alone matches *nothing* (not even `other`): a
    ///   negation can only veto, never select;
    /// - the order of patterns on the line does not matter.
    pub fn matches(&self, host: &str) -> bool {
        // Mirrors OpenSSH: scan every pattern; a matching negation vetoes the
        // whole list immediately, otherwise any positive match selects.
        let mut selected = false;
        for p in self.0.iter().filter(|p| p.glob_matches(host)) {
            if p.negated {
                return false;
            }
            selected = true;
        }
        selected
    }
}

/// OpenSSH-style wildcard match: `*` matches any run of characters
/// (including none), `?` matches exactly one character. No character classes.
pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    // Position of the last `*` seen in the pattern, and the text index it is
    // currently assumed to have consumed up to. Backtracking only needs the
    // most recent star, which keeps this O(|p|·|t|) worst case.
    let mut star: Option<(usize, usize)> = None;

    while ti < t.len() {
        match p.get(pi) {
            Some('*') => {
                star = Some((pi, ti));
                pi += 1;
            }
            Some(&c) if c == '?' || c == t[ti] => {
                pi += 1;
                ti += 1;
            }
            _ => match star {
                Some((sp, st)) => {
                    // Let the star swallow one more character and retry.
                    pi = sp + 1;
                    ti = st + 1;
                    star = Some((sp, st + 1));
                }
                None => return false,
            },
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(line: &str) -> HostPatterns {
        HostPatterns::from_host_value(line).unwrap()
    }

    #[test]
    fn wildcards() {
        assert!(wildcard_match("*", ""));
        assert!(wildcard_match("*", "anything"));
        assert!(wildcard_match("*.example.com", "a.example.com"));
        assert!(!wildcard_match("*.example.com", "example.com"));
        assert!(wildcard_match("web-??", "web-01"));
        assert!(!wildcard_match("web-??", "web-1"));
        assert!(wildcard_match("a*b*c", "aXXbYYc"));
        assert!(!wildcard_match("a*b*c", "aXXbYY"));
        assert!(wildcard_match("**x", "x"));
        assert!(wildcard_match("10.0.*.1", "10.0.33.1"));
    }

    #[test]
    fn wildcard_is_case_sensitive() {
        assert!(!wildcard_match("bar", "BAR"));
        assert!(!wildcard_match("FOO", "foo"));
    }

    #[test]
    fn parsing() {
        let p = host("work-* !work-legacy \"quoted name\"");
        assert_eq!(
            p.patterns(),
            [
                HostPattern {
                    negated: false,
                    glob: "work-*".into()
                },
                HostPattern {
                    negated: true,
                    glob: "work-legacy".into()
                },
                HostPattern {
                    negated: false,
                    glob: "quoted name".into()
                },
            ]
        );
        assert!(p.contains_alias("quoted name"));
        assert!(!p.contains_alias("work-legacy"));
        assert!(!p.contains_alias("work-*"));
    }

    #[test]
    fn comma_list_is_lowercased() {
        let p = HostPatterns::from_comma_list("A.example.com,!B*,,c");
        let globs: Vec<_> = p
            .patterns()
            .iter()
            .map(|p| (p.negated, p.glob.as_str()))
            .collect();
        assert_eq!(
            globs,
            [(false, "a.example.com"), (true, "b*"), (false, "c")]
        );
    }

    // ---- Tests for your `matches` implementation ----

    #[test]
    fn any_positive_pattern_selects() {
        assert!(host("a b c").matches("b"));
        assert!(host("*.example.com").matches("x.example.com"));
        assert!(!host("a b c").matches("d"));
    }

    #[test]
    fn negation_vetoes() {
        let p = host("* !bastion");
        assert!(p.matches("web"));
        assert!(!p.matches("bastion"));
        let p = host("work-* !work-legacy");
        assert!(p.matches("work-api"));
        assert!(!p.matches("work-legacy"));
    }

    #[test]
    fn order_does_not_matter() {
        assert!(!host("!bastion *").matches("bastion"));
        assert!(host("!bastion *").matches("web"));
    }

    #[test]
    fn negation_alone_selects_nothing() {
        assert!(!host("!bastion").matches("other"));
        assert!(!host("!bastion").matches("bastion"));
    }

    #[test]
    fn empty_list_selects_nothing() {
        assert!(!HostPatterns::default().matches("anything"));
    }
}
