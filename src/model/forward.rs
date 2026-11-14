//! Port and socket forwarding: `LocalForward`, `RemoteForward`, and
//! `DynamicForward`.
//!
//! Five shapes exist, all verified against `ssh -G` (OpenSSH 10.2):
//!
//! | config line | meaning |
//! |---|---|
//! | `LocalForward [bind:]port host:hostport` | local TCP port → remote host |
//! | `LocalForward 8080 /var/run/docker.sock` | either side may be a Unix socket |
//! | `RemoteForward [bind:]port host:hostport` | remote port → local host (port 0: server picks) |
//! | `RemoteForward [bind:]port` | **reverse dynamic**: SOCKS proxy on the remote side |
//! | `DynamicForward [bind:]port` | local SOCKS proxy |
//!
//! Rules ssh enforces, and so do we:
//! - an argument is a Unix socket path iff it contains `/` (`./x.sock`, `~/x.sock`);
//! - IPv6 addresses need brackets (`[::1]:80`; `::1:80` is rejected);
//! - ports are 1–65535 or a service name (`ssh`, `http`); 0 only for a remote listen port;
//! - a target needs both host and port;
//! - the config form takes listen and target as **two** arguments
//!   (`LocalForward 8080 db:5432`); the `ssh -L 8080:db:5432` form is rejected
//!   in a config file. [`Forward::from_cli`] converts the latter.

use std::fmt;

use serde::Serialize;

/// Which keyword a forward belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ForwardKind {
    /// `LocalForward` (`ssh -L`).
    Local,
    /// `RemoteForward` (`ssh -R`).
    Remote,
    /// `DynamicForward` (`ssh -D`).
    Dynamic,
}

impl ForwardKind {
    /// The config keyword.
    pub const fn keyword(self) -> &'static str {
        match self {
            Self::Local => "LocalForward",
            Self::Remote => "RemoteForward",
            Self::Dynamic => "DynamicForward",
        }
    }

    /// The `ssh` command-line flag letter.
    pub const fn flag(self) -> char {
        match self {
            Self::Local => 'L',
            Self::Remote => 'R',
            Self::Dynamic => 'D',
        }
    }

    /// The kind for a (case-insensitive) keyword.
    pub fn from_keyword(kw: &str) -> Option<Self> {
        [Self::Local, Self::Remote, Self::Dynamic]
            .into_iter()
            .find(|k| k.keyword().eq_ignore_ascii_case(kw))
    }
}

/// One side of a forward.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Endpoint {
    /// A TCP endpoint. `host` is the bind address on a listen side (`None`:
    /// ssh's default; `Some("")` or `Some("*")`: all interfaces) and the
    /// destination host on a target side (always `Some`).
    Tcp {
        /// Bind address or destination host, without brackets.
        host: Option<String>,
        /// Port number or service name, as written.
        port: String,
    },
    /// A Unix-domain socket path.
    Socket {
        /// The path, as written.
        path: String,
    },
}

impl fmt::Display for Endpoint {
    /// Config syntax: `8080`, `127.0.0.1:8080`, `[::1]:8080`, `:8080`, `/x.sock`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Socket { path } => f.write_str(path),
            Self::Tcp { host: None, port } => f.write_str(port),
            Self::Tcp {
                host: Some(h),
                port,
            } if h.contains(':') => write!(f, "[{h}]:{port}"),
            Self::Tcp {
                host: Some(h),
                port,
            } => write!(f, "{h}:{port}"),
        }
    }
}

/// A parsed forward.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct Forward {
    /// Local, remote, or dynamic.
    pub kind: ForwardKind,
    /// Where the forward listens.
    pub listen: Endpoint,
    /// Where it connects; `None` for dynamic and reverse-dynamic forwards.
    pub target: Option<Endpoint>,
}

/// A forward specification ssh would reject.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ForwardError {
    /// Wrong number of arguments for the keyword.
    #[error("{0}")]
    Arity(String),
    /// Malformed address, port, or socket (ssh: "Bad forwarding specification").
    #[error("bad forwarding specification: {0}")]
    Bad(String),
}

/// Which side of a forward is being parsed; decides the port-0 rule.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    /// A listen side; `zero_ok` for remote listens.
    Listen {
        zero_ok: bool,
    },
    Target,
}

fn bad(msg: impl Into<String>) -> ForwardError {
    ForwardError::Bad(msg.into())
}

fn check_port(port: &str, side: Side) -> Result<(), ForwardError> {
    if port.is_empty() {
        return Err(bad("missing port"));
    }
    if port.bytes().all(|b| b.is_ascii_digit()) {
        let ok = match port.parse::<u32>() {
            Ok(0) => matches!(side, Side::Listen { zero_ok: true }),
            Ok(n) => n <= 65535,
            Err(_) => false,
        };
        return if ok {
            Ok(())
        } else {
            Err(bad(format!("port {port} out of range")))
        };
    }
    // A service name (resolved by ssh via the services database).
    if port
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        Ok(())
    } else {
        Err(bad(format!("invalid port `{port}`")))
    }
}

/// Parse one endpoint in config syntax.
fn parse_endpoint(s: &str, side: Side) -> Result<Endpoint, ForwardError> {
    if s.contains('/') {
        return Ok(Endpoint::Socket { path: s.to_owned() });
    }
    let (host, port) = if let Some(rest) = s.strip_prefix('[') {
        let (inner, after) = rest
            .split_once(']')
            .ok_or_else(|| bad(format!("unclosed `[` in `{s}`")))?;
        let port = after
            .strip_prefix(':')
            .ok_or_else(|| bad(format!("missing port after `[{inner}]`")))?;
        (Some(inner.to_owned()), port.to_owned())
    } else if let Some((h, p)) = s.rsplit_once(':') {
        if h.contains(':') {
            return Err(bad(format!(
                "IPv6 address in `{s}` needs brackets, e.g. [::1]:80"
            )));
        }
        (Some(h.to_owned()), p.to_owned())
    } else {
        (None, s.to_owned())
    };
    if side == Side::Target && host.as_deref().is_none_or(str::is_empty) {
        return Err(bad(format!(
            "target `{s}` needs host:port or a socket path"
        )));
    }
    check_port(&port, side)?;
    Ok(Endpoint::Tcp { host, port })
}

impl Forward {
    /// Parse a config line's arguments (after `split_args`).
    ///
    /// # Errors
    /// The specification ssh would reject, with the reason.
    pub fn from_config(kind: ForwardKind, args: &[String]) -> Result<Self, ForwardError> {
        let kw = kind.keyword();
        let listen_side = Side::Listen {
            zero_ok: kind == ForwardKind::Remote,
        };
        match (kind, args) {
            (_, []) => Err(ForwardError::Arity(format!("`{kw}` needs a value"))),
            (ForwardKind::Local, [_]) => Err(ForwardError::Arity(
                "`LocalForward` needs a listen address and a target (e.g. `8080 localhost:80`); the `8080:localhost:80` form only works on the ssh command line".into(),
            )),
            (ForwardKind::Dynamic | ForwardKind::Remote, [listen]) => Ok(Self {
                kind,
                listen: parse_endpoint(listen, listen_side)?,
                target: None,
            }),
            (ForwardKind::Local | ForwardKind::Remote, [listen, target]) => Ok(Self {
                kind,
                listen: parse_endpoint(listen, listen_side)?,
                target: Some(parse_endpoint(target, Side::Target)?),
            }),
            (_, _) => Err(ForwardError::Arity(format!(
                "`{kw}` has too many arguments"
            ))),
        }
    }

    /// Parse `ssh -L/-R/-D` command-line syntax, e.g. `8080:db:5432`,
    /// `127.0.0.1:8080:db:5432`, `[::1]:8080:db:80`, `8080:/run/x.sock`,
    /// `/tmp/l.sock:db:5432`, or `[bind:]port` for `-D` and reverse `-R`.
    ///
    /// # Errors
    /// A malformed specification.
    pub fn from_cli(kind: ForwardKind, spec: &str) -> Result<Self, ForwardError> {
        let parts = split_colons(spec)?;
        let is_sock = |p: &str| p.contains('/');
        let join = |p: &[String]| p.join(":");
        // Split the parts into config-syntax listen and target arguments.
        let args: Vec<String> = match (kind, parts.as_slice()) {
            (ForwardKind::Dynamic, p) if p.len() <= 2 => vec![join(p)],
            (ForwardKind::Remote, p) if p.len() <= 2 && !p.iter().skip(1).any(|x| is_sock(x)) => {
                vec![join(p)] // reverse dynamic: [bind:]port
            }
            (_, [a, b]) if is_sock(a) || is_sock(b) => vec![a.clone(), b.clone()],
            (_, [a, h, p]) if is_sock(a) => vec![a.clone(), format!("{h}:{p}")],
            (_, [b, p, s]) if is_sock(s) => vec![format!("{b}:{p}"), s.clone()],
            (_, [p, h, hp]) => vec![p.clone(), format!("{h}:{hp}")],
            (_, [b, p, h, hp]) => vec![format!("{b}:{p}"), format!("{h}:{hp}")],
            _ => {
                return Err(bad(format!(
                    "`-{} {spec}`: expected {}",
                    kind.flag(),
                    match kind {
                        ForwardKind::Dynamic => "[bind:]port",
                        ForwardKind::Local => "[bind:]port:host:hostport",
                        ForwardKind::Remote => "[bind:]port:host:hostport or [bind:]port",
                    }
                )));
            }
        };
        Self::from_config(kind, &args)
    }

    /// The config keyword for this forward.
    pub const fn keyword(&self) -> &'static str {
        self.kind.keyword()
    }

    /// True for `RemoteForward` without a target (a remote SOCKS proxy).
    pub const fn is_reverse_dynamic(&self) -> bool {
        matches!(self.kind, ForwardKind::Remote) && self.target.is_none()
    }

    /// Human-readable type: `local`, `remote`, `dynamic`, `reverse-dynamic`.
    pub const fn type_name(&self) -> &'static str {
        match self.kind {
            ForwardKind::Local => "local",
            ForwardKind::Remote if self.target.is_none() => "reverse-dynamic",
            ForwardKind::Remote => "remote",
            ForwardKind::Dynamic => "dynamic",
        }
    }

    /// The value to write after the keyword (canonical config syntax).
    pub fn to_config_value(&self) -> String {
        match &self.target {
            Some(t) => format!("{} {t}", self.listen),
            None => self.listen.to_string(),
        }
    }

    /// Compact form for tables: `L8080→db:5432`, `D1080`, `R1080(socks)`.
    pub fn short(&self) -> String {
        let flag = self.kind.flag();
        match &self.target {
            Some(t) => format!("{flag}{}→{t}", self.listen),
            None if self.is_reverse_dynamic() => format!("{flag}{}(socks)", self.listen),
            None => format!("{flag}{}", self.listen),
        }
    }

    /// The value as `ssh -G` prints it (`[127.0.0.1]:8080 [db]:5432`,
    /// `9000 [socks]:0`). Service names are not resolved.
    pub fn ssh_g_form(&self) -> String {
        fn side(e: &Endpoint) -> String {
            match e {
                Endpoint::Socket { path } => path.clone(),
                Endpoint::Tcp { host: None, port } => port.clone(),
                Endpoint::Tcp {
                    host: Some(h),
                    port,
                } => format!("[{h}]:{port}"),
            }
        }
        match (&self.target, self.kind) {
            (Some(t), _) => format!("{} {}", side(&self.listen), side(t)),
            (None, ForwardKind::Remote) => format!("{} [socks]:0", side(&self.listen)),
            (None, _) => side(&self.listen),
        }
    }

    /// Whether this forward listens on `listen` (used by `forward rm`).
    pub fn listens_on(&self, kind: ForwardKind, listen: &Endpoint) -> bool {
        self.kind == kind && &self.listen == listen
    }
}

impl fmt::Display for Forward {
    /// `LocalForward 8080 db:5432`
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.keyword(), self.to_config_value())
    }
}

/// Parse a listen endpoint as given to `forward rm` (`8080`,
/// `127.0.0.1:8080`, `[::1]:8080`, `/tmp/x.sock`).
///
/// # Errors
/// A malformed endpoint.
pub fn parse_listen(kind: ForwardKind, s: &str) -> Result<Endpoint, ForwardError> {
    parse_endpoint(
        s,
        Side::Listen {
            zero_ok: kind == ForwardKind::Remote,
        },
    )
}

/// Split ssh CLI syntax on `:` outside `[...]`, keeping the brackets.
fn split_colons(spec: &str) -> Result<Vec<String>, ForwardError> {
    let mut parts = vec![String::new()];
    let mut depth = 0u8;
    for c in spec.chars() {
        match c {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            ':' if depth == 0 => {
                parts.push(String::new());
                continue;
            }
            _ => {}
        }
        parts.last_mut().expect("never empty").push(c);
    }
    if depth != 0 {
        return Err(bad(format!("unclosed `[` in `{spec}`")));
    }
    Ok(parts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use proptest::prelude::*;

    fn cfg(kw: &str, value: &str) -> Result<Forward, ForwardError> {
        let args = crate::model::value::split_args(value).unwrap();
        Forward::from_config(ForwardKind::from_keyword(kw).unwrap(), &args)
    }

    /// Every row probed with `ssh -G` (OpenSSH 10.2): (keyword, value, ssh accepts).
    const GROUND_TRUTH: &[(&str, &str, bool)] = &[
        ("LocalForward", "8080 localhost:80", true),
        ("LocalForward", "127.0.0.1:8080 db:5432", true),
        ("LocalForward", "*:8080 db:5432", true),
        ("LocalForward", ":8080 db:5432", true),
        ("LocalForward", "[::1]:8080 [2001:db8::1]:80", true),
        ("LocalForward", "[db]:8080 db:5432", true),
        ("LocalForward", "ssh localhost:80", true),
        ("LocalForward", "8080 localhost:http", true),
        ("LocalForward", "8080 /var/run/docker.sock", true),
        ("LocalForward", "/tmp/l.sock db:5432", true),
        ("LocalForward", "/tmp/l.sock /var/run/r.sock", true),
        ("LocalForward", "./rel.sock db:1", true),
        ("LocalForward", "8080 ~/x.sock", true),
        ("LocalForward", "8080:localhost:80", false),
        ("LocalForward", "8080", false),
        ("LocalForward", "0 localhost:80", false),
        ("LocalForward", "70000 localhost:80", false),
        ("LocalForward", "8080 localhost", false),
        ("LocalForward", "8080 db:0", false),
        ("LocalForward", "8080 db:65536", false),
        ("LocalForward", "8080 ::1:80", false),
        ("LocalForward", "8080 rel.sock", false),
        ("LocalForward", "8080 db:5432 extra", false),
        ("RemoteForward", "9000 localhost:3000", true),
        ("RemoteForward", "0 localhost:3000", true),
        ("RemoteForward", "[::1]:0 db:1", true),
        ("RemoteForward", "*:9000 localhost:3000", true),
        ("RemoteForward", "/tmp/r.sock localhost:3000", true),
        ("RemoteForward", "9000 /tmp/x.sock", true),
        ("RemoteForward", "1080", true),
        ("RemoteForward", "localhost:9000", true),
        ("RemoteForward", "0", true),
        ("RemoteForward", "9000 db", false),
        ("RemoteForward", "65536 db:1", false),
        ("DynamicForward", "1080", true),
        ("DynamicForward", "127.0.0.1:1080", true),
        ("DynamicForward", "[::1]:1080", true),
        ("DynamicForward", "localhost:1080", true),
        ("DynamicForward", ":1080", true),
        ("DynamicForward", "*:1080", true),
        ("DynamicForward", "/tmp/s.sock", true),
        ("DynamicForward", "socks", true),
        ("DynamicForward", "0", false),
        ("DynamicForward", "70000", false),
        ("DynamicForward", "1080 extra", false),
        ("DynamicForward", "[::1]", false),
    ];

    #[test]
    fn matches_ssh_on_every_probed_case() {
        for (kw, value, ok) in GROUND_TRUTH {
            assert_eq!(
                cfg(kw, value).is_ok(),
                *ok,
                "{kw} {value}: {:?}",
                cfg(kw, value)
            );
        }
    }

    #[test]
    fn structure() {
        let f = cfg("LocalForward", "[::1]:8080 db:5432").unwrap();
        assert_eq!(
            f.listen,
            Endpoint::Tcp {
                host: Some("::1".into()),
                port: "8080".into()
            }
        );
        assert_eq!(f.type_name(), "local");
        assert_eq!(f.short(), "L[::1]:8080→db:5432");
        assert_eq!(f.ssh_g_form(), "[::1]:8080 [db]:5432");

        let r = cfg("RemoteForward", "1080").unwrap();
        assert!(r.is_reverse_dynamic());
        assert_eq!(r.type_name(), "reverse-dynamic");
        assert_eq!(r.short(), "R1080(socks)");
        assert_eq!(r.ssh_g_form(), "1080 [socks]:0");

        let d = cfg("DynamicForward", "127.0.0.1:1080").unwrap();
        assert_eq!(
            (d.short(), d.ssh_g_form()),
            ("D127.0.0.1:1080".into(), "[127.0.0.1]:1080".into())
        );

        let s = cfg("LocalForward", "8080 /var/run/docker.sock").unwrap();
        assert_eq!(
            s.target,
            Some(Endpoint::Socket {
                path: "/var/run/docker.sock".into()
            })
        );
        assert_eq!(s.to_string(), "LocalForward 8080 /var/run/docker.sock");
    }

    #[test]
    fn cli_syntax_converts_to_config_syntax() {
        use ForwardKind::{Dynamic as D, Local as L, Remote as R};
        let c = |k, s| Forward::from_cli(k, s).map(|f| f.to_config_value());
        assert_eq!(c(L, "8080:localhost:80"), Ok("8080 localhost:80".into()));
        assert_eq!(
            c(L, "127.0.0.1:8080:db:5432"),
            Ok("127.0.0.1:8080 db:5432".into())
        );
        assert_eq!(
            c(L, "[::1]:8080:[2001:db8::1]:80"),
            Ok("[::1]:8080 [2001:db8::1]:80".into())
        );
        assert_eq!(c(L, "*:8080:db:5432"), Ok("*:8080 db:5432".into()));
        assert_eq!(
            c(L, "8080:/var/run/docker.sock"),
            Ok("8080 /var/run/docker.sock".into())
        );
        assert_eq!(
            c(L, "/tmp/l.sock:db:5432"),
            Ok("/tmp/l.sock db:5432".into())
        );
        assert_eq!(
            c(L, "/tmp/l.sock:/tmp/r.sock"),
            Ok("/tmp/l.sock /tmp/r.sock".into())
        );
        assert_eq!(
            c(L, "127.0.0.1:8080:/run/x.sock"),
            Ok("127.0.0.1:8080 /run/x.sock".into())
        );
        assert_eq!(
            c(R, "9000:localhost:3000"),
            Ok("9000 localhost:3000".into())
        );
        assert_eq!(c(R, "1080"), Ok("1080".into()));
        assert_eq!(c(R, "localhost:9000"), Ok("localhost:9000".into()));
        assert_eq!(c(R, "9000:/tmp/x.sock"), Ok("9000 /tmp/x.sock".into()));
        assert_eq!(c(D, "1080"), Ok("1080".into()));
        assert_eq!(c(D, "127.0.0.1:1080"), Ok("127.0.0.1:1080".into()));
        assert_eq!(c(D, "[::1]:1080"), Ok("[::1]:1080".into()));
        assert!(c(L, "8080").is_err());
        assert!(c(L, "8080:db").is_err());
        assert!(c(D, "a:b:c").is_err());
        assert!(c(L, "[::1:8080:db:80").is_err());
    }

    #[test]
    fn listen_lookup() {
        let f = cfg("LocalForward", "127.0.0.1:8080 db:1").unwrap();
        assert!(f.listens_on(
            ForwardKind::Local,
            &parse_listen(ForwardKind::Local, "127.0.0.1:8080").unwrap()
        ));
        assert!(!f.listens_on(
            ForwardKind::Local,
            &parse_listen(ForwardKind::Local, "8080").unwrap()
        ));
        assert!(!f.listens_on(ForwardKind::Dynamic, &f.listen));
    }

    fn host() -> impl Strategy<Value = Option<String>> {
        prop_oneof![
            Just(None),
            Just(Some(String::new())),
            Just(Some("*".to_owned())),
            "[a-z][a-z0-9.-]{0,8}".prop_map(Some),
            Just(Some("::1".to_owned())),
        ]
    }

    fn endpoint(target: bool) -> impl Strategy<Value = Endpoint> {
        prop_oneof![
            (host(), 1u16..).prop_map(move |(h, p)| Endpoint::Tcp {
                host: if target {
                    Some(h.filter(|h| !h.is_empty()).unwrap_or_else(|| "db".into()))
                } else {
                    h
                },
                port: p.to_string(),
            }),
            "/[a-z]{1,6}/[a-z]{1,6}\\.sock".prop_map(|path| Endpoint::Socket { path }),
        ]
    }

    proptest! {
        #[test]
        fn config_value_round_trips(
            kind in prop_oneof![Just(ForwardKind::Local), Just(ForwardKind::Remote), Just(ForwardKind::Dynamic)],
            listen in endpoint(false),
            target in endpoint(true),
            with_target in any::<bool>(),
        ) {
            let target = match kind {
                ForwardKind::Local => Some(target),
                ForwardKind::Remote => with_target.then_some(target),
                ForwardKind::Dynamic => None,
            };
            let f = Forward { kind, listen, target };
            let args = crate::model::value::split_args(&f.to_config_value()).unwrap();
            prop_assert_eq!(Forward::from_config(kind, &args), Ok(f));
        }
    }
}
