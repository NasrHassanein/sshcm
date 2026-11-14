//! Every lint *error* must be something OpenSSH rejects, and every line ssh
//! accepts must produce no lint error. Each case is checked against
//! `ssh -G`; skipped if ssh is not installed.

use std::process::{Command, Stdio};

use sshcm::include::ConfigSet;
use sshcm::lint::{LintContext, Severity, lint};
use sshcm::syntax::Document;

/// Lines placed inside `Host x`. Platform-specific keywords (`UseKeychain`)
/// are deliberately absent.
const CASES: &[&str] = &[
    "Port 22",
    "Port 65535",
    "Port 0",
    "Port 65536",
    "Port 22x",
    "Port ssh",
    "Port 22 23",
    "Compression yes",
    "Compression NO",
    "Compression true",
    "Compression maybe",
    "BatchMode false",
    "BatchMode yes",
    "BatchMode maybe",
    "IdentitiesOnly x",
    "CheckHostIP x",
    "TCPKeepAlive no",
    "GatewayPorts clientspecified",
    "ForwardAgent ~/agent.sock",
    "ForwardAgent yes",
    "ServerAliveInterval 60",
    "ServerAliveInterval 1m",
    "ServerAliveInterval -1",
    "ServerAliveInterval abc",
    "ConnectTimeout 0",
    "ConnectTimeout 1h30m",
    "ForwardX11Timeout 1h",
    "ForwardX11Timeout x",
    "ConnectionAttempts 0",
    "ConnectionAttempts 3",
    "ConnectionAttempts x",
    "ServerAliveCountMax 0",
    "NumberOfPasswordPrompts 0",
    "StrictHostKeyChecking accept-new",
    "StrictHostKeyChecking off",
    "StrictHostKeyChecking maybe",
    "ControlMaster autoask",
    "ControlMaster x",
    "RequestTTY force",
    "RequestTTY x",
    "LogLevel DEBUG3",
    "LogLevel loud",
    "AddressFamily inet6",
    "AddressFamily ipx",
    "VerifyHostKeyDNS ask",
    "VerifyHostKeyDNS x",
    "AddKeysToAgent confirm 1h",
    "UpdateHostKeys ask",
    "PubkeyAuthentication host-bound",
    "User",
    "HostName",
    "Compression yes no",
    "Bogus 1",
    "HostNmae x",
    "LocalForward 8080 localhost:80",
    "SendEnv LANG LC_*",
    "ProxyCommand ssh -W %h:%p bastion",
    "IdentityFile \"~/My Keys/id\"",
    "User \"unterminated",
];

fn ssh_accepts(cfg: &str) -> Option<bool> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config");
    std::fs::write(&path, cfg).unwrap();
    let status = Command::new("ssh")
        .args(["-G", "-F"])
        .arg(&path)
        .arg("x")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    Some(status.success())
}

#[test]
fn lint_errors_match_ssh_rejections() {
    let ctx = LintContext {
        home: "/nonexistent".into(),
        local_user: "me".into(),
        check_files: false,
    };
    let mut mismatches = Vec::new();
    for case in CASES {
        let cfg = format!("Host x\n  {case}\n");
        let Some(ssh_ok) = ssh_accepts(&cfg) else {
            eprintln!("note: `ssh` not available; skipping lint parity");
            return;
        };
        let set = ConfigSet::single("config", Document::parse(&cfg));
        let errors: Vec<_> = lint(&set, &ctx)
            .into_iter()
            .filter(|f| f.severity == Severity::Error)
            .collect();
        if ssh_ok != errors.is_empty() {
            mismatches.push(format!(
                "{case:?}: ssh {}, lint {:?}",
                if ssh_ok { "accepts" } else { "rejects" },
                errors.iter().map(|f| &f.message).collect::<Vec<_>>()
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "disagreements with ssh:\n{}",
        mismatches.join("\n")
    );
}
