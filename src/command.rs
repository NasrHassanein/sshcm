//! Turning an `ssh …` command line into a `Host` record.
//!
//! The mapping, and ssh's precedence when a setting is given twice, were
//! probed with `ssh -G -F /dev/null <args>` (OpenSSH 10.2), and
//! `tests/parity.rs` checks that the generated record means exactly what
//! the command meant. ssh has three behaviours:
//!
//! - **first wins**: `-p`, `-l`, `user@`, `ssh://user@host:port`, `-P`, and
//!   every `-o` (a later value is ignored);
//! - **last wins**, overriding even an earlier `-o`: `-c -m -b -B -e -S -w -I`
//!   and the switches `-4/-6 -A/-a -X/-x/-Y -K/-k -C -g -M`;
//! - **accumulate**: `-i`, `-L/-R/-D`, and `-o` for list keywords.
//!
//! Flags that only affect one invocation (`-v -q -f -N -n -t -T -s …`) or
//! have no config form (`-E -W -F -O -Q -G -V`) are not written; they are
//! reported in [`ParsedCommand::skipped`]. So is a remote command.

use std::fmt;

use crate::model::forward::{Forward, ForwardKind};
use crate::model::keyword::{self, Merge};
use crate::model::value::{quote, split_args};

/// Why a command could not be turned into a record.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CommandError {
    /// Unbalanced quotes in the command string.
    #[error("unterminated {0} quote in the command")]
    UnterminatedQuote(char),
    /// No destination host.
    #[error("no destination host in the command")]
    NoDestination,
    /// An option needs an argument and has none.
    #[error("`-{0}` needs an argument")]
    MissingArgument(char),
    /// An option ssh does not know.
    #[error("unknown ssh option `-{0}`")]
    UnknownOption(char),
    /// A value ssh would reject.
    #[error("{0}")]
    Invalid(String),
}

/// A flag that was recognised but deliberately not written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    /// The flag as given, e.g. `-v` or `-E /tmp/log`.
    pub flag: String,
    /// Why it is not part of the record.
    pub reason: &'static str,
}

/// The result of parsing an `ssh` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCommand {
    /// The destination host, as ssh would use it for `HostName`.
    pub host: String,
    /// `(keyword, value)` pairs in the order they should be written.
    pub options: Vec<(String, String)>,
    /// Flags left out of the record, with reasons.
    pub skipped: Vec<Skipped>,
    /// A command to run remotely, if one was given (not written).
    pub remote_command: Option<String>,
}

impl ParsedCommand {
    /// Options for a block named `alias`: `HostName` first when the alias
    /// differs from the destination host.
    pub fn options_for(&self, alias: &str) -> Vec<(String, String)> {
        let mut out = Vec::with_capacity(self.options.len() + 1);
        if alias != self.host {
            out.push(("HostName".to_owned(), quote(&self.host).into_owned()));
        }
        out.extend(self.options.iter().cloned());
        out
    }
}

// ---------------------------------------------------------------------------
// Shell word splitting
// ---------------------------------------------------------------------------

/// Split a command string into words like a POSIX shell, without any
/// expansion (`~` and `$VAR` stay literal; ssh config understands `~`).
///
/// # Errors
/// An unterminated quote.
pub fn shell_split(s: &str) -> Result<Vec<String>, CommandError> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => word.push(c),
                        None => return Err(CommandError::UnterminatedQuote('\'')),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(n @ ('"' | '\\' | '$' | '`')) => word.push(n),
                            Some(n) => {
                                word.push('\\');
                                word.push(n);
                            }
                            None => return Err(CommandError::UnterminatedQuote('"')),
                        },
                        Some(c) => word.push(c),
                        None => return Err(CommandError::UnterminatedQuote('"')),
                    }
                }
            }
            '\\' => {
                in_word = true;
                if let Some(n) = chars.next() {
                    word.push(n);
                }
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    Ok(words)
}

// ---------------------------------------------------------------------------
// Option accumulation with ssh's precedence rules
// ---------------------------------------------------------------------------

/// Ordered options with ssh's three precedence behaviours.
#[derive(Default)]
struct Options(Vec<(String, String)>);

impl Options {
    fn has(&self, k: &str) -> bool {
        self.0.iter().any(|(key, _)| key.eq_ignore_ascii_case(k))
    }
    /// Set unless already set (`-p`, `-l`, `-o`, …).
    fn first(&mut self, k: &str, v: String) {
        if !self.has(k) {
            self.0.push((k.to_owned(), v));
        }
    }
    /// Set, replacing any earlier value in place (`-c`, `-A`, …).
    fn last(&mut self, k: &str, v: String) {
        match self
            .0
            .iter_mut()
            .find(|(key, _)| key.eq_ignore_ascii_case(k))
        {
            Some(slot) => slot.1 = v,
            None => self.0.push((k.to_owned(), v)),
        }
    }
    /// Always add (`-i`, forwards).
    fn add(&mut self, k: &str, v: String) {
        self.0.push((k.to_owned(), v));
    }
}

/// ssh's flags that take no argument, and those that take one (from
/// ssh.c's getopt string).
const NO_ARG: &str = "46AaCfGgKkMNnqsTtVvXxYy";
const WITH_ARG: &str = "BbcDEeFIiJLlmOoPpQRSWw";

/// Order in which well-known keywords are written; the rest follow in
/// command order.
const LEADING: &[&str] = &["User", "Port", "IdentityFile", "ProxyJump"];

fn validate_port(p: &str) -> Result<String, CommandError> {
    match p.parse::<u16>() {
        Ok(n) if n > 0 => Ok(n.to_string()),
        _ => Err(CommandError::Invalid(format!("bad port `{p}`"))),
    }
}

/// Parse `ssh` command-line words (an optional leading `ssh` is ignored).
///
/// # Errors
/// Unknown options, missing arguments, invalid ports or `-o` keywords, a
/// second `-J`, or no destination.
pub fn parse_ssh_command(words: &[String]) -> Result<ParsedCommand, CommandError> {
    let mut words = words.iter().map(String::as_str).peekable();
    if words
        .peek()
        .is_some_and(|w| *w == "ssh" || w.ends_with("/ssh"))
    {
        words.next();
    }

    let mut opts = Options::default();
    let mut skipped = Vec::new();
    let mut host: Option<String> = None;
    let mut remote: Vec<&str> = Vec::new();
    let mut control_master_flags = 0;
    let mut options_done = false;

    while let Some(word) = words.next() {
        if host.is_some() && options_done {
            remote.push(word);
            continue;
        }
        if word == "--" && !options_done {
            options_done = true;
            continue;
        }
        let Some(cluster) = word
            .strip_prefix('-')
            .filter(|c| !c.is_empty() && !options_done)
        else {
            if host.is_none() {
                // Applied here, in command order: `user@` and `ssh://…:port`
                // obey first-wins against -l/-p before and after it.
                host = Some(apply_destination(word, &mut opts)?);
            } else {
                // ssh stops option parsing at the first word after the
                // destination that is not an option.
                remote.push(word);
                options_done = true;
            }
            continue;
        };

        for (i, flag) in cluster.char_indices() {
            if WITH_ARG.contains(flag) {
                let rest = &cluster[i + flag.len_utf8()..];
                let arg = if rest.is_empty() {
                    words
                        .next()
                        .ok_or(CommandError::MissingArgument(flag))?
                        .to_owned()
                } else {
                    rest.to_owned()
                };
                apply_with_arg(flag, &arg, &mut opts, &mut skipped)?;
                break;
            }
            if !NO_ARG.contains(flag) {
                return Err(CommandError::UnknownOption(flag));
            }
            apply_switch(flag, &mut opts, &mut skipped, &mut control_master_flags);
        }
    }

    let host = host.ok_or(CommandError::NoDestination)?;

    // Conventional order first, then everything else in command order.
    let mut options = opts.0;
    options.sort_by_key(|(k, _)| LEADING.iter().position(|l| l == k).unwrap_or(LEADING.len()));

    let remote_command = (!remote.is_empty()).then(|| {
        remote
            .iter()
            .map(|w| quote(w).into_owned())
            .collect::<Vec<_>>()
            .join(" ")
    });
    Ok(ParsedCommand {
        host,
        options,
        skipped,
        remote_command,
    })
}

const ONE_OFF: &str = "affects a single invocation";
const NO_CONFIG: &str = "has no ssh_config equivalent";

fn apply_switch(flag: char, opts: &mut Options, skipped: &mut Vec<Skipped>, cm: &mut u32) {
    let yes = || "yes".to_owned();
    match flag {
        '4' => opts.last("AddressFamily", "inet".into()),
        '6' => opts.last("AddressFamily", "inet6".into()),
        'A' => opts.last("ForwardAgent", yes()),
        'a' => opts.last("ForwardAgent", "no".into()),
        'C' => opts.last("Compression", yes()),
        'g' => opts.last("GatewayPorts", yes()),
        'K' => {
            opts.last("GSSAPIAuthentication", yes());
            opts.last("GSSAPIDelegateCredentials", yes());
        }
        'k' => opts.last("GSSAPIDelegateCredentials", "no".into()),
        'M' => {
            *cm += 1;
            let v = if *cm >= 2 { "ask" } else { "yes" };
            opts.last("ControlMaster", v.into());
        }
        'X' => opts.last("ForwardX11", yes()),
        'x' => opts.last("ForwardX11", "no".into()),
        'Y' => {
            opts.last("ForwardX11", yes());
            opts.last("ForwardX11Trusted", yes());
        }
        'v' | 'q' | 'f' | 'N' | 'n' | 't' | 'T' | 's' | 'y' => skipped.push(Skipped {
            flag: format!("-{flag}"),
            reason: ONE_OFF,
        }),
        // -G (print config) and -V (version) don't connect at all.
        _ => skipped.push(Skipped {
            flag: format!("-{flag}"),
            reason: NO_CONFIG,
        }),
    }
}

fn apply_with_arg(
    flag: char,
    arg: &str,
    opts: &mut Options,
    skipped: &mut Vec<Skipped>,
) -> Result<(), CommandError> {
    let q = |s: &str| quote(s).into_owned();
    match flag {
        'p' => {
            let port = validate_port(arg)?;
            opts.first("Port", port);
        }
        'l' => opts.first("User", q(arg)),
        'i' => opts.add("IdentityFile", q(arg)),
        'J' => {
            if opts.has("ProxyJump") {
                return Err(CommandError::Invalid(
                    "only a single -J is allowed (use commas to separate jump hosts)".into(),
                ));
            }
            opts.first("ProxyJump", q(arg));
        }
        'P' => opts.first("Tag", q(arg)),
        'c' => opts.last("Ciphers", q(arg)),
        'm' => opts.last("MACs", q(arg)),
        'b' => opts.last("BindAddress", q(arg)),
        'B' => opts.last("BindInterface", q(arg)),
        'e' => opts.last("EscapeChar", q(arg)),
        'S' => opts.last("ControlPath", q(arg)),
        'I' => opts.last("PKCS11Provider", q(arg)),
        'w' => {
            opts.last("Tunnel", "point-to-point".into());
            opts.last("TunnelDevice", q(arg));
        }
        'L' | 'R' | 'D' => {
            let kind = match flag {
                'L' => ForwardKind::Local,
                'R' => ForwardKind::Remote,
                _ => ForwardKind::Dynamic,
            };
            let f = Forward::from_cli(kind, arg)
                .map_err(|e| CommandError::Invalid(format!("-{flag} {arg}: {e}")))?;
            opts.add(f.keyword(), f.to_config_value());
        }
        'o' => apply_dash_o(arg, opts)?,
        'E' | 'W' | 'F' | 'O' | 'Q' => skipped.push(Skipped {
            flag: format!("-{flag} {arg}"),
            reason: if flag == 'O' || flag == 'Q' {
                ONE_OFF
            } else {
                NO_CONFIG
            },
        }),
        _ => unreachable!("every WITH_ARG flag is handled"),
    }
    Ok(())
}

/// `-o Key=Value` or `-o "Key Value"`: first wins, except list keywords,
/// which accumulate.
fn apply_dash_o(arg: &str, opts: &mut Options) -> Result<(), CommandError> {
    let arg = arg.trim();
    let split = arg
        .find(|c: char| c == '=' || c.is_whitespace())
        .ok_or_else(|| CommandError::Invalid(format!("-o {arg}: expected Key=Value")))?;
    let (key, value) = (
        &arg[..split],
        arg[split + 1..].trim_start_matches(['=', ' ', '\t']).trim(),
    );
    let Some(kw) = keyword::canonical(key) else {
        let hint = keyword::suggest(key)
            .map(|s| format!(" (did you mean `{s}`?)"))
            .unwrap_or_default();
        return Err(CommandError::Invalid(format!(
            "-o {arg}: unknown keyword `{key}`{hint}"
        )));
    };
    if matches!(kw, "Host" | "Match" | "Include") {
        return Err(CommandError::Invalid(format!(
            "-o {arg}: `{kw}` cannot be set with -o"
        )));
    }
    if value.is_empty() {
        return Err(CommandError::Invalid(format!("-o {arg}: missing value")));
    }
    // The same ssh-verified rules `lint` applies (values and forwards), so
    // `parse view` never prints a record ssh would reject.
    let args = split_args(value).map_err(|e| CommandError::Invalid(format!("-o {arg}: {e}")))?;
    if let Some(msg) = crate::lint::check_value(kw, &args) {
        return Err(CommandError::Invalid(format!("-o {arg}: {msg}")));
    }
    if let Some(kind) = ForwardKind::from_keyword(kw)
        && let Err(e) = Forward::from_config(kind, &args)
    {
        return Err(CommandError::Invalid(format!("-o {arg}: {e}")));
    }
    if kw == "Port" {
        let port = validate_port(value)?;
        opts.first(kw, port);
    } else if keyword::merge_rule(kw) == Merge::Accumulate {
        opts.add(kw, value.to_owned());
    } else {
        opts.first(kw, value.to_owned());
    }
    Ok(())
}

/// `[user@]host` or `ssh://[user@]host[:port]`; returns the host.
fn apply_destination(dest: &str, opts: &mut Options) -> Result<String, CommandError> {
    let (userhost, port) = match dest.strip_prefix("ssh://") {
        Some(rest) => {
            let rest = rest.trim_end_matches('/');
            if rest.contains('[') {
                return Err(CommandError::Invalid(format!(
                    "`{dest}`: bracketed addresses are not supported in ssh:// URIs"
                )));
            }
            match rest.rsplit_once(':') {
                Some((uh, p)) => (uh, Some(validate_port(p)?)),
                None => (rest, None),
            }
        }
        None => (dest, None),
    };
    let (user, host) = match userhost.rsplit_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, userhost),
    };
    if host.is_empty() {
        return Err(CommandError::NoDestination);
    }
    if let Some(u) = user.filter(|u| !u.is_empty()) {
        opts.first("User", quote(u).into_owned());
    }
    if let Some(p) = port {
        opts.first("Port", p);
    }
    Ok(host.to_owned())
}

impl fmt::Display for Skipped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "`{}` {}", self.flag, self.reason)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn parse(cmd: &str) -> ParsedCommand {
        parse_ssh_command(&shell_split(cmd).unwrap()).unwrap()
    }

    fn err(cmd: &str) -> String {
        parse_ssh_command(&shell_split(cmd).unwrap())
            .unwrap_err()
            .to_string()
    }

    fn opts(cmd: &str) -> Vec<String> {
        parse(cmd)
            .options
            .into_iter()
            .map(|(k, v)| format!("{k} {v}"))
            .collect()
    }

    #[test]
    fn shell_splitting() {
        let s = |x| shell_split(x).unwrap();
        assert_eq!(
            s(r#"ssh -i "~/My Keys/k" 'a b' c\ d"#),
            ["ssh", "-i", "~/My Keys/k", "a b", "c d"]
        );
        assert_eq!(
            s(r#"-o "ProxyCommand=ssh -W %h:%p j""#),
            ["-o", "ProxyCommand=ssh -W %h:%p j"]
        );
        assert_eq!(s(r#""a\"b" '' x"#), [r#"a"b"#, "", "x"]);
        assert_eq!(s("  "), Vec::<String>::new());
        assert_eq!(
            shell_split("ssh 'x"),
            Err(CommandError::UnterminatedQuote('\''))
        );
    }

    #[test]
    fn the_users_examples() {
        let p = parse("ssh -i ~/.ssh/id_rsa user@host");
        assert_eq!(p.host, "host");
        assert_eq!(
            opts("ssh -i ~/.ssh/id_rsa user@host"),
            ["User user", "IdentityFile ~/.ssh/id_rsa"]
        );
        assert_eq!(
            opts("ssh -i ~/.ssh/id_rsa user@host -p 22"),
            ["User user", "Port 22", "IdentityFile ~/.ssh/id_rsa"]
        );
        assert_eq!(
            parse("ssh user@db.example.com").options_for("db"),
            [
                ("HostName".to_owned(), "db.example.com".to_owned()),
                ("User".to_owned(), "user".to_owned())
            ]
        );
    }

    #[test]
    fn getopt_forms() {
        assert_eq!(opts("ssh -p22 -lbob h"), ["User bob", "Port 22"]);
        assert_eq!(opts("ssh -AC h"), ["ForwardAgent yes", "Compression yes"]);
        assert_eq!(opts("ssh -Ai/k h"), ["IdentityFile /k", "ForwardAgent yes"]);
        assert_eq!(opts("ssh -oUser=x h"), ["User x"]);
        assert_eq!(opts("ssh -- h"), Vec::<String>::new());
        assert_eq!(err("ssh -p"), "`-p` needs an argument");
        assert_eq!(err("ssh -Z h"), "unknown ssh option `-Z`");
        assert_eq!(err("ssh -v"), "no destination host in the command");
    }

    #[test]
    fn precedence_matches_ssh() {
        // first wins
        assert_eq!(opts("ssh -l bob alice@h"), ["User bob"]);
        assert_eq!(opts("ssh alice@h -l bob"), ["User alice"]);
        assert_eq!(opts("ssh -p 1 -p 2 h"), ["Port 1"]);
        assert_eq!(opts("ssh -o Port=3 -p 1 h"), ["Port 3"]);
        assert_eq!(opts("ssh -p 1 ssh://h:2"), ["Port 1"]);
        assert_eq!(opts("ssh ssh://h:2 -p 1"), ["Port 2"]);
        assert_eq!(opts("ssh -P t1 -P t2 h"), ["Tag t1"]);
        assert_eq!(opts("ssh -J j1 -o ProxyJump=j2 h"), ["ProxyJump j1"]);
        // last wins, even over an earlier -o
        assert_eq!(opts("ssh -A -a h"), ["ForwardAgent no"]);
        assert_eq!(opts("ssh -o ForwardAgent=yes -a h"), ["ForwardAgent no"]);
        assert_eq!(opts("ssh -a -o ForwardAgent=yes h"), ["ForwardAgent no"]);
        assert_eq!(
            opts("ssh -o Ciphers=aes256-ctr -c aes128-ctr h"),
            ["Ciphers aes128-ctr"]
        );
        assert_eq!(opts("ssh -4 -6 h"), ["AddressFamily inet6"]);
        assert_eq!(
            opts("ssh -Y -x h"),
            ["ForwardX11 no", "ForwardX11Trusted yes"]
        );
        assert_eq!(
            opts("ssh -K -k h"),
            ["GSSAPIAuthentication yes", "GSSAPIDelegateCredentials no"]
        );
        assert_eq!(opts("ssh -M -M h"), ["ControlMaster ask"]);
        assert_eq!(
            opts("ssh -o ControlMaster=auto -M h"),
            ["ControlMaster yes"]
        );
        // accumulate
        assert_eq!(
            opts("ssh -i /a -i /b h"),
            ["IdentityFile /a", "IdentityFile /b"]
        );
        // errors ssh also raises
        assert!(err("ssh -J a -J b h").contains("only a single -J"));
        assert!(err("ssh -o ProxyJump=a -J b h").contains("only a single -J"));
        assert_eq!(err("ssh -p 0 h"), "bad port `0`");
        assert_eq!(err("ssh ssh://h:abc"), "bad port `abc`");
    }

    #[test]
    fn mapping_table() {
        assert_eq!(
            opts(
                "ssh -4 -g -e none -b 10.0.0.1 -B eth0 -S /tmp/cp -m hmac-sha2-256 -w 1:2 -P tag -I /lib/p11.so h"
            ),
            [
                "AddressFamily inet",
                "GatewayPorts yes",
                "EscapeChar none",
                "BindAddress 10.0.0.1",
                "BindInterface eth0",
                "ControlPath /tmp/cp",
                "MACs hmac-sha2-256",
                "Tunnel point-to-point",
                "TunnelDevice 1:2",
                "Tag tag",
                "PKCS11Provider /lib/p11.so",
            ]
        );
        assert_eq!(
            opts("ssh -J bastion -L 8080:db:80 -R 1080 -D 127.0.0.1:1081 u@h"),
            [
                "User u",
                "ProxyJump bastion",
                "LocalForward 8080 db:80",
                "RemoteForward 1080",
                "DynamicForward 127.0.0.1:1081",
            ]
        );
        assert_eq!(
            opts(r#"ssh -i "/My Keys/k" h"#),
            [r#"IdentityFile "/My Keys/k""#]
        );
        assert_eq!(
            opts(r#"ssh -o "ProxyCommand ssh -W %h:%p j" h"#),
            ["ProxyCommand ssh -W %h:%p j"]
        );
        assert_eq!(
            opts("ssh -o 'LocalForward 1 a:1' -o 'LocalForward=2 b:2' h"),
            ["LocalForward 1 a:1", "LocalForward 2 b:2"]
        );
    }

    #[test]
    fn destinations() {
        assert_eq!(
            (parse("ssh a@b@h").host, opts("ssh a@b@h")),
            ("h".into(), vec!["User a@b".into()])
        );
        assert_eq!(parse("ssh u@[::1]").host, "[::1]");
        assert_eq!(opts("ssh ssh://alice@h:2022"), ["User alice", "Port 2022"]);
        assert_eq!(parse("ssh ssh://h/").host, "h");
        assert!(err("ssh ssh://u@[::1]:22").contains("not supported"));
        assert_eq!(parse("/usr/bin/ssh h").host, "h");
        assert_eq!(parse("h").host, "h", "leading `ssh` is optional");
    }

    #[test]
    fn one_off_flags_and_remote_command_are_reported() {
        let p = parse("ssh -vvv -N -f -E /tmp/log -W db:22 -t h uptime -a 'x y'");
        assert_eq!(p.options, Vec::<(String, String)>::new());
        let flags: Vec<_> = p.skipped.iter().map(|s| s.flag.as_str()).collect();
        assert_eq!(
            flags,
            [
                "-v",
                "-v",
                "-v",
                "-N",
                "-f",
                "-E /tmp/log",
                "-W db:22",
                "-t"
            ]
        );
        assert_eq!(p.remote_command.as_deref(), Some(r#"uptime -a "x y""#));
        // Options after the destination still count, until the command starts.
        assert_eq!(opts("ssh h -p 2222 -A"), ["Port 2222", "ForwardAgent yes"]);
    }

    #[test]
    fn dash_o_validation() {
        assert!(err("ssh -o Bogus=1 h").contains("unknown keyword `Bogus`"));
        assert!(err("ssh -o Hostnme=x h").contains("did you mean `HostName`"));
        assert!(err("ssh -o Host=x h").contains("cannot be set with -o"));
        assert!(err("ssh -o User= h").contains("missing value"));
        assert!(err("ssh -o Port=99999 h").contains("Port 99999"));
        assert!(err("ssh -o Compression=true h").contains("must be one of yes, no"));
        assert!(err("ssh -o LocalForward=8080:db:80 h").contains("LocalForward"));
    }
}
