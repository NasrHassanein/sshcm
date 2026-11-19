//! What values ssh accepts for single-valued keywords.
//!
//! Each rule was verified against `ssh -G` (see `tests/lint_parity.rs`), so
//! a value rejected here is one ssh rejects too. Used by `lint` (error
//! E002) and by `parse` for `-o` options, so `parse view` never prints a
//! record ssh would refuse. Keywords without a rule accept any value.

/// The kind of value a keyword takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    /// A port number from 1 to 65535, or a service name such as `ssh`.
    Port,
    /// `yes`, `no`, `true`, or `false` (any case).
    Flag,
    /// One of a fixed set of words (any case).
    Choice(&'static [&'static str]),
    /// A whole number of at least `min`.
    Integer {
        /// The smallest accepted value.
        min: u32,
    },
    /// A time interval in OpenSSH's syntax: `30`, `90s`, `1h30m`.
    Time,
}

/// Keywords that take [`Rule::Flag`].
const FLAGS: &[&str] = &[
    "BatchMode",
    "CanonicalizeFallbackLocal",
    "CheckHostIP",
    "ClearAllForwardings",
    "EnableEscapeCommandline",
    "EnableSSHKeysign",
    "ExitOnForwardFailure",
    "ForkAfterAuthentication",
    "ForwardX11",
    "ForwardX11Trusted",
    "GatewayPorts",
    "GSSAPIAuthentication",
    "GSSAPIDelegateCredentials",
    "HashKnownHosts",
    "HostbasedAuthentication",
    "IdentitiesOnly",
    "KbdInteractiveAuthentication",
    "NoHostAuthenticationForLocalhost",
    "PasswordAuthentication",
    "PermitLocalCommand",
    "ProxyUseFdpass",
    "StdinNull",
    "StreamLocalBindUnlink",
    "TCPKeepAlive",
    "VisualHostKey",
];

/// Keywords that take [`Rule::Choice`], with their accepted words.
const CHOICES: &[(&str, &[&str])] = &[
    ("AddressFamily", &["any", "inet", "inet6"]),
    ("Compression", &["yes", "no"]),
    (
        "ControlMaster",
        &["yes", "no", "true", "false", "ask", "auto", "autoask"],
    ),
    (
        "LogLevel",
        &[
            "quiet", "fatal", "error", "info", "verbose", "debug", "debug1", "debug2", "debug3",
        ],
    ),
    (
        "RequestTTY",
        &["yes", "no", "true", "false", "force", "auto"],
    ),
    (
        "StrictHostKeyChecking",
        &["yes", "no", "true", "false", "ask", "accept-new", "off"],
    ),
    ("VerifyHostKeyDNS", &["yes", "no", "true", "false", "ask"]),
];

/// Keywords that take [`Rule::Integer`], with their minimum.
const INTEGERS: &[(&str, u32)] = &[
    ("CanonicalizeMaxDots", 0),
    ("ConnectionAttempts", 1),
    ("NumberOfPasswordPrompts", 0),
    ("ServerAliveCountMax", 0),
];

/// Keywords that take [`Rule::Time`].
const TIMES: &[&str] = &["ConnectTimeout", "ForwardX11Timeout", "ServerAliveInterval"];

/// The rule for `keyword` (canonical spelling), or `None` if ssh accepts
/// any value for it, or any number of values.
pub fn rule(keyword: &str) -> Option<Rule> {
    if keyword == "Port" {
        Some(Rule::Port)
    } else if FLAGS.contains(&keyword) {
        Some(Rule::Flag)
    } else if let Some(&(_, words)) = CHOICES.iter().find(|(k, _)| *k == keyword) {
        Some(Rule::Choice(words))
    } else if let Some(&(_, min)) = INTEGERS.iter().find(|(k, _)| *k == keyword) {
        Some(Rule::Integer { min })
    } else if TIMES.contains(&keyword) {
        Some(Rule::Time)
    } else {
        None
    }
}

impl Rule {
    /// Whether ssh accepts `value` under this rule.
    pub fn accepts(self, value: &str) -> bool {
        let lower = value.to_ascii_lowercase();
        match self {
            // A leading digit means ssh reads it as a number; anything else
            // is looked up as a service name when connecting.
            Self::Port => {
                !value.starts_with(|c: char| c.is_ascii_digit())
                    || value.parse::<u16>().is_ok_and(|p| p > 0)
            }
            Self::Flag => matches!(lower.as_str(), "yes" | "no" | "true" | "false"),
            Self::Choice(words) => words.contains(&lower.as_str()),
            Self::Integer { min } => value.parse::<u32>().is_ok_and(|n| n >= min),
            Self::Time => is_time(value),
        }
    }

    /// What a value must look like, for error messages.
    pub fn expected(self) -> String {
        match self {
            Self::Port => "a number from 1 to 65535 or a service name".to_owned(),
            Self::Flag => "yes or no".to_owned(),
            Self::Choice(words) => format!("one of {}", words.join(", ")),
            Self::Integer { .. } => "a whole number".to_owned(),
            Self::Time => "a time such as 30, 90s, or 5m".to_owned(),
        }
    }
}

/// Check the arguments of `keyword` (canonical spelling) against its rule.
/// Returns an error message, or `None` if ssh would accept them (or the
/// keyword has no rule).
pub fn check_value(keyword: &str, args: &[String]) -> Option<String> {
    let rule = rule(keyword)?;
    match args {
        [] => Some(format!("`{keyword}` needs a value")),
        [value] => (!rule.accepts(value))
            .then(|| format!("`{keyword} {value}`: must be {}", rule.expected())),
        _ => Some(format!(
            "`{keyword}` takes a single value, got {}",
            args.len()
        )),
    }
}

/// OpenSSH `convtime` syntax: one or more `<digits>[smhdw]` groups (the unit
/// may be omitted, meaning seconds).
fn is_time(v: &str) -> bool {
    let mut rest = v;
    if rest.is_empty() {
        return false;
    }
    while !rest.is_empty() {
        let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits == 0 {
            return false;
        }
        rest = &rest[digits..];
        rest = rest
            .strip_prefix(['s', 'S', 'm', 'M', 'h', 'H', 'd', 'D', 'w', 'W'])
            .unwrap_or(rest);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(kw: &str, args: &[&str]) -> Option<String> {
        let args: Vec<String> = args.iter().map(|&a| a.to_owned()).collect();
        check_value(kw, &args)
    }

    #[test]
    fn every_listed_keyword_is_known() {
        let listed = FLAGS
            .iter()
            .chain(CHOICES.iter().map(|(k, _)| k))
            .chain(INTEGERS.iter().map(|(k, _)| k))
            .chain(TIMES);
        for k in listed {
            assert_eq!(super::super::keyword::canonical(k), Some(*k), "{k}");
        }
    }

    #[test]
    fn messages() {
        assert_eq!(check("HostName", &["a", "b"]), None, "no rule, no check");
        assert_eq!(check("Port", &["ssh"]), None);
        assert_eq!(
            check("Port", &["0"]).unwrap(),
            "`Port 0`: must be a number from 1 to 65535 or a service name"
        );
        assert_eq!(
            check("BatchMode", &["maybe"]).unwrap(),
            "`BatchMode maybe`: must be yes or no"
        );
        assert_eq!(
            check("AddressFamily", &["ipx"]).unwrap(),
            "`AddressFamily ipx`: must be one of any, inet, inet6"
        );
        assert_eq!(
            check("ConnectionAttempts", &["0"]).unwrap(),
            "`ConnectionAttempts 0`: must be a whole number"
        );
        assert_eq!(check("ServerAliveCountMax", &["0"]), None);
        assert_eq!(
            check("Port", &["1", "2"]).unwrap(),
            "`Port` takes a single value, got 2"
        );
        assert_eq!(check("Port", &[]).unwrap(), "`Port` needs a value");
    }

    #[test]
    fn time_syntax() {
        for ok in ["30", "90s", "5M", "1h30m", "2w", "0"] {
            assert!(is_time(ok), "{ok}");
        }
        for bad in ["", "m", "1x", "1.5h", "-1", "1 m"] {
            assert!(!is_time(bad), "{bad}");
        }
    }
}
