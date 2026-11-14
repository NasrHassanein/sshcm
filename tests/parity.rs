//! Resolution parity with OpenSSH: for each fixture and host, `resolve()` must
//! agree with `ssh -G` on every option the file sets explicitly whose value
//! OpenSSH prints verbatim. Skipped (with a note) if `ssh` is not installed.

use std::path::Path;
use std::process::{Command, Stdio};

use sshcm::include::{ConfigSet, IncludeContext};
use sshcm::resolve::{ResolveContext, resolve};
use sshcm::syntax::Document;

/// (fixture, hosts to probe). Fixtures with `Include` or `Match exec` are
/// excluded: ssh would read the real filesystem or run commands.
const CASES: &[(&str, &[&str])] = &[
    ("basic", &["github.com", "bastion", "unknown-host"]),
    ("separators", &["a", "b", "c", "d"]),
    ("crlf", &["win", "other"]),
    ("no_trailing_newline", &["last", "nope"]),
    ("mixed_eol", &["mixed", "x"]),
];

/// Options compared: printed verbatim by `ssh -G` (no yes→true rewriting).
const COMPARED: &[&str] = &["hostname", "user", "port"];

fn ssh_g(file: &Path, host: &str) -> Option<Vec<(String, String)>> {
    let out = Command::new("ssh")
        .args(["-G", "-F"])
        .arg(file)
        .args(["--", host])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    assert!(
        out.status.success(),
        "ssh -G failed for {host} in {}",
        file.display()
    );
    Some(
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .filter_map(|l| l.split_once(' '))
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect(),
    )
}

#[test]
fn resolve_matches_ssh_g() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let ctx = ResolveContext {
        local_user: std::env::var("USER").unwrap_or_default(),
    };
    let mut compared = 0;

    for (fixture, hosts) in CASES {
        let path = dir.join(format!("{fixture}.config"));
        let doc = Document::parse(&std::fs::read_to_string(&path).unwrap());
        let set = ConfigSet::single(&path, doc);

        for host in *hosts {
            let Some(ssh) = ssh_g(&path, host) else {
                eprintln!("note: `ssh` not available; skipping parity test");
                return;
            };
            let ours = resolve(&set, host, &ctx);
            for key in COMPARED {
                let Some(setting) = ours.get(key) else {
                    continue;
                };
                let value = setting.args.join(" ");
                if value.contains('%') {
                    continue; // ssh expands tokens; we report them raw
                }
                let theirs = ssh.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
                assert_eq!(
                    Some(value.as_str()),
                    theirs,
                    "{fixture}: `{key}` for host `{host}` (ours from line {})",
                    setting.line
                );
                compared += 1;
            }
        }
    }
    assert!(
        compared > 10,
        "parity test compared too little ({compared})"
    );
}

/// Multi-file parity: includes at top level, inside a `Host` block
/// (conditional), globals inside an included file, and a `Host` block in a
/// file included from an inactive block. Paths are absolute because ssh
/// resolves relative ones against the real `~/.ssh`.
#[test]
fn include_resolution_matches_ssh_g() {
    use std::fs;
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    fs::create_dir_all(d.join("conf.d")).unwrap();
    let root = d.join("config");
    fs::write(
        &root,
        format!(
            "Include {d}/conf.d/*.conf\n\nHost bastion\n    HostName 203.0.113.10\n    Include {d}/bastion-extra\n    User jump\n\nHost *\n    User fallback\n    Port 22\n",
            d = d.display()
        ),
    )
    .unwrap();
    fs::write(
        d.join("conf.d/10-work.conf"),
        "Host work\n    HostName work.example\n    User alice\n",
    )
    .unwrap();
    fs::write(
        d.join("conf.d/20-home.conf"),
        "User homeglobal\nHost nas\n    HostName 192.168.1.10\n    Port 2200\n",
    )
    .unwrap();
    fs::write(
        d.join("bastion-extra"),
        "Port 2222\nHost work\n    HostName never-applies\n",
    )
    .unwrap();

    let ctx = IncludeContext::for_home(d.to_owned(), "me".into());
    let set = ConfigSet::load(&root, &ctx).unwrap();
    assert_eq!(set.files().len(), 4);
    assert_eq!(set.diagnostics(), &[]);
    let rctx = ResolveContext {
        local_user: std::env::var("USER").unwrap_or_default(),
    };

    let mut compared = 0;
    for host in ["work", "nas", "bastion", "other"] {
        let Some(ssh) = ssh_g(&root, host) else {
            eprintln!("note: `ssh` not available; skipping parity test");
            return;
        };
        let ours = resolve(&set, host, &rctx);
        for key in COMPARED {
            let theirs = ssh.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
            let mine = ours.get(key).map(|s| s.args.join(" "));
            // ssh fills defaults (e.g. hostname = host); compare where we set it.
            if let Some(mine) = mine {
                assert_eq!(Some(mine), theirs, "`{key}` for `{host}`");
                compared += 1;
            }
        }
    }
    assert!(compared >= 10, "compared only {compared}");
}

/// Forwarding parity: for every host in `forwards.config`, the forwards we
/// resolve, printed in ssh's normalized form, must equal `ssh -G` output
/// exactly. `ssh -G` also prints a phantom `dynamicforward <listen>` for
/// each local forward whose target is a Unix socket (a dump quirk, not a
/// real forward); those lines are subtracted before comparing.
#[test]
fn forward_resolution_matches_ssh_g() {
    use sshcm::model::forward::{Endpoint, Forward, ForwardKind};

    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/forwards.config");
    let set = ConfigSet::single(
        &path,
        Document::parse(&std::fs::read_to_string(&path).unwrap()),
    );
    let rctx = ResolveContext {
        local_user: "me".into(),
    };
    let mut compared = 0;

    for host in ["db", "jump", "docker", "other"] {
        let Some(ssh) = ssh_g(&path, host) else {
            eprintln!("note: `ssh` not available; skipping forwarding parity");
            return;
        };
        let ours: Vec<Forward> = resolve(&set, host, &rctx)
            .settings
            .iter()
            .filter_map(|s| {
                Forward::from_config(ForwardKind::from_keyword(&s.keyword)?, &s.args).ok()
            })
            .collect();
        let phantoms: Vec<String> = ours
            .iter()
            .filter(|f| {
                f.kind == ForwardKind::Local && matches!(f.target, Some(Endpoint::Socket { .. }))
            })
            .map(|f| f.ssh_g_form().split(' ').next().unwrap().to_owned())
            .collect();

        for kind in [
            ForwardKind::Local,
            ForwardKind::Remote,
            ForwardKind::Dynamic,
        ] {
            let key = kind.keyword().to_ascii_lowercase();
            let mut theirs: Vec<String> = ssh
                .iter()
                .filter(|(k, _)| *k == key)
                .map(|(_, v)| v.clone())
                .collect();
            if kind == ForwardKind::Dynamic {
                for p in &phantoms {
                    if let Some(i) = theirs.iter().position(|v| v == p) {
                        theirs.remove(i);
                    }
                }
            }
            let mine: Vec<String> = ours
                .iter()
                .filter(|f| f.kind == kind)
                .map(Forward::ssh_g_form)
                .collect();
            assert_eq!(mine, theirs, "{key} for `{host}`");
            compared += mine.len();
        }
    }
    assert_eq!(
        compared, 10,
        "every forward in the fixture was compared (db 2, jump 5, docker 3)"
    );
}

/// `parse`: for each command, OpenSSH must resolve the generated record
/// exactly as it resolves the original command line:
/// `ssh -G -F /dev/null <command>` == `ssh -G -F <record> <host>`.
/// This is the strongest possible check of the flag mapping and of ssh's
/// precedence rules (first-wins vs last-wins vs accumulate).
#[test]
fn parse_record_means_what_the_command_meant() {
    use sshcm::command::{parse_ssh_command, shell_split};
    use sshcm::edit::render_host_block;

    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("id_test");
    std::fs::write(&key, "").unwrap();
    let key = key.display().to_string();

    let commands = [
        format!("ssh -i {key} user@host"),
        format!("ssh -i {key} user@host -p 22"),
        "ssh -l bob alice@h -p 2222 -p 1".to_owned(),
        "ssh alice@h -l bob".to_owned(),
        "ssh -A -a -X -Y -4 -C -g h".to_owned(),
        "ssh -o ForwardAgent=yes -a -o Ciphers=aes256-ctr -c aes128-ctr h".to_owned(),
        "ssh -J bastion -L 8080:db:80 -R 1080 -D 127.0.0.1:1081 u@h".to_owned(),
        "ssh -K -k -M -M h".to_owned(),
        "ssh -e none -b 10.0.0.1 -B eth0 -S /tmp/cp -m hmac-sha2-256 -w 1:2 -P tag h".to_owned(),
        "ssh ssh://alice@h:2022".to_owned(),
        "ssh -o 'ProxyCommand ssh -W %h:%p j' -o User=x -l y h".to_owned(),
        "ssh a@b@h".to_owned(),
        "ssh h -p 2222 -A".to_owned(),
        "ssh -o 'LocalForward 1 a:1' -L 2:b:2 h".to_owned(),
        "ssh -o ControlMaster=auto -M -o AddressFamily=inet6 -4 -x -X -6 h".to_owned(),
    ];

    let ssh_g = |args: &[&str]| -> Option<String> {
        let out = Command::new("ssh")
            .arg("-G")
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        assert!(out.status.success(), "ssh -G {args:?} failed");
        Some(String::from_utf8(out.stdout).unwrap())
    };

    for cmd in &commands {
        let words = shell_split(cmd).unwrap();
        let parsed = parse_ssh_command(&words).unwrap();
        let record = render_host_block(
            &Document::parse(""),
            &parsed.host,
            &parsed.options_for(&parsed.host),
        )
        .unwrap();
        let file = dir.path().join("record");
        std::fs::write(&file, &record).unwrap();

        let mut cli_args = vec!["-F", "/dev/null"];
        cli_args.extend(words[1..].iter().map(String::as_str));
        let Some(from_command) = ssh_g(&cli_args) else {
            eprintln!("note: `ssh` not available; skipping parse parity");
            return;
        };
        let from_record = ssh_g(&["-F", file.to_str().unwrap(), &parsed.host]).unwrap();
        // ssh -G dump quirk: compression enabled by the `-C` flag prints as
        // `compression UNKNOWN`, while `Compression yes` prints `yes`. Same
        // setting; normalize only that line, compare everything else as is.
        let from_command = from_command.replace("compression UNKNOWN\n", "compression yes\n");
        assert_eq!(from_record, from_command, "`{cmd}` vs record:\n{record}");
    }
}
