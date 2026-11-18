//! The table of `ssh_config(5)` keywords.
//!
//! Keywords are matched case-insensitively. Each known keyword has a canonical
//! spelling (as in the man page) and a merge rule used by resolution.

/// How repeated occurrences of a keyword combine during resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Merge {
    /// The first value obtained wins; later ones are ignored (the common case).
    FirstWins,
    /// Every occurrence adds to a list (e.g. `IdentityFile`, `LocalForward`).
    Accumulate,
}

/// Keywords whose occurrences accumulate rather than first-wins.
const ACCUMULATE: &[&str] = &[
    "CertificateFile",
    "DynamicForward",
    "IdentityFile",
    "LocalForward",
    "RemoteForward",
    "SendEnv",
    "SetEnv",
];

/// Every keyword OpenSSH 10.x accepts in a client config, plus a few widely
/// used vendor or deprecated spellings (`UseKeychain` on macOS, old
/// `*KeyTypes` aliases) so they are not reported as unknown.
const KNOWN: &[&str] = &[
    "AddKeysToAgent",
    "AddressFamily",
    "BatchMode",
    "BindAddress",
    "BindInterface",
    "CanonicalDomains",
    "CanonicalizeFallbackLocal",
    "CanonicalizeHostname",
    "CanonicalizeMaxDots",
    "CanonicalizePermittedCNAMEs",
    "CASignatureAlgorithms",
    "CertificateFile",
    "ChallengeResponseAuthentication",
    "ChannelTimeout",
    "CheckHostIP",
    "Ciphers",
    "ClearAllForwardings",
    "Compression",
    "ConnectionAttempts",
    "ConnectTimeout",
    "ControlMaster",
    "ControlPath",
    "ControlPersist",
    "DynamicForward",
    "EnableEscapeCommandline",
    "EnableSSHKeysign",
    "EscapeChar",
    "ExitOnForwardFailure",
    "FingerprintHash",
    "ForkAfterAuthentication",
    "ForwardAgent",
    "ForwardX11",
    "ForwardX11Timeout",
    "ForwardX11Trusted",
    "GatewayPorts",
    "GlobalKnownHostsFile",
    "GSSAPIAuthentication",
    "GSSAPIDelegateCredentials",
    "HashKnownHosts",
    "Host",
    "HostbasedAcceptedAlgorithms",
    "HostbasedAuthentication",
    "HostbasedKeyTypes",
    "HostKeyAlgorithms",
    "HostKeyAlias",
    "HostName",
    "IdentitiesOnly",
    "IdentityAgent",
    "IdentityFile",
    "IgnoreUnknown",
    "Include",
    "IPQoS",
    "KbdInteractiveAuthentication",
    "KbdInteractiveDevices",
    "KexAlgorithms",
    "KnownHostsCommand",
    "LocalCommand",
    "LocalForward",
    "LogLevel",
    "LogVerbose",
    "MACs",
    "Match",
    "NoHostAuthenticationForLocalhost",
    "NumberOfPasswordPrompts",
    "ObscureKeystrokeTiming",
    "PasswordAuthentication",
    "PermitLocalCommand",
    "PermitRemoteOpen",
    "PKCS11Provider",
    "Port",
    "PreferredAuthentications",
    "ProxyCommand",
    "ProxyJump",
    "ProxyUseFdpass",
    "PubkeyAcceptedAlgorithms",
    "PubkeyAcceptedKeyTypes",
    "PubkeyAuthentication",
    "RefuseConnection",
    "RekeyLimit",
    "RemoteCommand",
    "RemoteForward",
    "RequestTTY",
    "RequiredRSASize",
    "RevokedHostKeys",
    "SecurityKeyProvider",
    "SendEnv",
    "ServerAliveCountMax",
    "ServerAliveInterval",
    "SessionType",
    "SetEnv",
    "StdinNull",
    "StreamLocalBindMask",
    "StreamLocalBindUnlink",
    "StrictHostKeyChecking",
    "SyslogFacility",
    "Tag",
    "TCPKeepAlive",
    "Tunnel",
    "TunnelDevice",
    "UpdateHostKeys",
    "UseKeychain",
    "User",
    "UserKnownHostsFile",
    "VerifyHostKeyDNS",
    "VersionAddendum",
    "VisualHostKey",
    "WarnWeakCrypto",
    "XAuthLocation",
];

/// The canonical spelling of `name`, if it is a known keyword.
pub fn canonical(name: &str) -> Option<&'static str> {
    KNOWN.iter().copied().find(|k| k.eq_ignore_ascii_case(name))
}

/// The closest known keyword to a misspelt `name`, for "did you mean" hints.
pub fn suggest(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    let max = 2.max(name.len() / 4);
    KNOWN
        .iter()
        .map(|k| (levenshtein(&lower, &k.to_ascii_lowercase()), *k))
        .filter(|(d, _)| *d <= max)
        .min_by_key(|(d, _)| *d)
        .map(|(_, k)| k)
}

/// A ` (did you mean `X`?)` suffix for an error about the unknown keyword
/// `name`, or an empty string when nothing is close enough.
pub fn hint(name: &str) -> String {
    suggest(name)
        .map(|s| format!(" (did you mean `{s}`?)"))
        .unwrap_or_default()
}

/// True for `Host`, `Match`, and `Include` (given in canonical spelling):
/// keywords that shape the file's structure rather than set an option, so
/// they can't be set like an option and never "win" like one.
pub fn is_structural(canonical: &str) -> bool {
    matches!(canonical, "Host" | "Match" | "Include")
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let substitute = prev[j] + usize::from(ca != *cb);
            cur.push(substitute.min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// The merge rule for `name` (unknown keywords are treated as first-wins).
pub fn merge_rule(name: &str) -> Merge {
    if ACCUMULATE.iter().any(|k| k.eq_ignore_ascii_case(name)) {
        Merge::Accumulate
    } else {
        Merge::FirstWins
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hint_and_structural() {
        assert_eq!(hint("Hostnme"), " (did you mean `HostName`?)");
        assert_eq!(hint("Zzzzzzzzzz"), "");
        assert!(is_structural("Include"));
        assert!(!is_structural("HostName"));
    }

    #[test]
    fn canonical_is_case_insensitive() {
        assert_eq!(canonical("hostname"), Some("HostName"));
        assert_eq!(canonical("IDENTITYFILE"), Some("IdentityFile"));
        assert_eq!(canonical("NotAThing"), None);
    }

    #[test]
    fn suggestions() {
        assert_eq!(suggest("Hostnme"), Some("HostName"));
        assert_eq!(suggest("identityfiles"), Some("IdentityFile"));
        assert_eq!(suggest("prot"), Some("Port"));
        assert_eq!(suggest("CompletelyDifferent"), None);
    }

    #[test]
    fn merge_rules() {
        assert_eq!(merge_rule("identityfile"), Merge::Accumulate);
        assert_eq!(merge_rule("Port"), Merge::FirstWins);
        assert_eq!(merge_rule("Unknown"), Merge::FirstWins);
    }

    #[test]
    fn table_has_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for k in KNOWN {
            assert!(seen.insert(k.to_ascii_lowercase()), "duplicate keyword {k}");
        }
        for k in ACCUMULATE {
            assert!(canonical(k).is_some(), "{k} missing from KNOWN");
        }
    }
}
