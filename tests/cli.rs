//! End-to-end tests of the `sshcm` binary.

use assert_cmd::Command;
use predicates::prelude::*;

fn fixture(name: &str) -> String {
    format!(
        "{}/tests/fixtures/{name}.config",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// An empty home directory, so fixtures with `Include ~/...` never read the
/// real user's files.
fn empty_home() -> std::path::PathBuf {
    let home = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("empty-home");
    std::fs::create_dir_all(&home).unwrap();
    home
}

fn sshcm(file: &str) -> Command {
    let mut c = Command::cargo_bin("sshcm").unwrap();
    c.env_remove("SSHCM_CONFIG")
        .env("HOME", empty_home())
        .args(["-F", file]);
    c
}

#[test]
fn list_table() {
    sshcm(&fixture("basic"))
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::starts_with("HOST "))
        .stdout(predicate::str::contains(
            "bastion     203.0.113.10  ops   2222",
        ));
}

#[test]
fn list_filter_and_json() {
    let out = sshcm(&fixture("basic"))
        .args(["list", "--pattern", "git*", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v[0]["hostname"], "github.com");
    assert_eq!(v[0]["line"], 1);
}

#[test]
fn show_prints_block_verbatim() {
    sshcm(&fixture("comments_and_globals"))
        .args(["show", "work-legacy"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Host work-legacy\n\tHostName 10.0.0.9\n",
        ));
}

#[test]
fn show_trims_trailing_comments_that_introduce_the_next_block() {
    sshcm(&fixture("basic"))
        .args(["show", "github.com"])
        .assert()
        .success()
        .stdout(predicate::str::ends_with(
            "IdentityFile ~/.ssh/id_ed25519\n",
        ));
}

#[test]
fn show_missing_alias_exits_3() {
    sshcm(&fixture("basic"))
        .args(["show", "nope"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("no Host block names `nope`"));
}

#[test]
fn resolve_shows_sources() {
    sshcm(&fixture("basic"))
        .args(["resolve", "bastion"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Port"))
        .stdout(predicate::str::contains("# line 9"))
        .stdout(predicate::str::contains("ServerAliveInterval"));
}

#[test]
fn resolve_warns_on_unsupported_match() {
    sshcm(&fixture("match_and_quotes"))
        .args(["resolve", "x.internal"])
        .assert()
        .success()
        .stderr(predicate::str::contains("cannot evaluate `Match exec`"));
}

#[test]
fn missing_file_is_an_error() {
    sshcm("/nonexistent/config")
        .arg("list")
        .assert()
        .code(1)
        .stderr(predicate::str::contains("reading /nonexistent/config"));
}

// ---------------------------------------------------------------------------
// Mutating commands (each runs against a private copy in a temp dir)
// ---------------------------------------------------------------------------

mod write {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    struct Sandbox {
        dir: tempfile::TempDir,
        config: PathBuf,
    }

    impl Sandbox {
        fn with(contents: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let config = dir.path().join("config");
            fs::write(&config, contents).unwrap();
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
            }
            Self { dir, config }
        }
        fn from_fixture(name: &str) -> Self {
            Self::with(&fs::read_to_string(fixture(name)).unwrap())
        }
        fn cmd(&self) -> Command {
            sshcm(self.config.to_str().unwrap())
        }
        fn read(&self) -> String {
            fs::read_to_string(&self.config).unwrap()
        }
        fn backups(&self) -> Vec<PathBuf> {
            let dir = self.dir.path().join(".sshcm/backups");
            fs::read_dir(dir)
                .map_or_else(|_| Vec::new(), |rd| rd.map(|e| e.unwrap().path()).collect())
        }
        fn stray_files(&self) -> Vec<String> {
            fs::read_dir(self.dir.path())
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|n| n != "config" && n != ".sshcm")
                .collect()
        }
    }

    fn ssh_available() -> bool {
        std::process::Command::new("ssh").arg("-V").output().is_ok()
    }

    #[test]
    fn add_writes_backs_up_and_validates() {
        let s = Sandbox::from_fixture("basic");
        let original = s.read();
        s.cmd()
            .args([
                "add",
                "db",
                "--hostname",
                "10.0.0.5",
                "-u",
                "ops",
                "-p",
                "2200",
            ])
            .args(["-o", "ProxyJump=bastion", "-y"])
            .assert()
            .success()
            .stdout(predicate::str::contains("+Host db"))
            .stderr(predicate::str::contains("backup:"));

        let updated = s.read();
        assert!(updated.contains("Host db\n    HostName 10.0.0.5\n    User ops\n    Port 2200\n    ProxyJump bastion\n\nHost *"));
        let backups = s.backups();
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read_to_string(&backups[0]).unwrap(), original);
        assert_eq!(
            s.stray_files(),
            Vec::<String>::new(),
            "temp files left behind"
        );
    }

    #[test]
    fn add_then_rm_restores_file_byte_for_byte() {
        for name in [
            "basic",
            "crlf",
            "comments_and_globals",
            "no_trailing_newline",
        ] {
            let s = Sandbox::from_fixture(name);
            let original = fs::read(&s.config).unwrap();
            s.cmd()
                .args(["add", "probe", "--hostname", "h", "-y"])
                .assert()
                .success();
            assert_ne!(fs::read(&s.config).unwrap(), original);
            s.cmd().args(["rm", "probe", "-y"]).assert().success();
            assert_eq!(fs::read(&s.config).unwrap(), original, "{name}");
        }
    }

    #[test]
    fn crlf_is_preserved_byte_for_byte() {
        let s = Sandbox::from_fixture("crlf");
        s.cmd()
            .args(["set", "win", "Port=2200", "-y"])
            .assert()
            .success();
        let bytes = fs::read(&s.config).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("    Port 2200\r\n"));
        assert_eq!(
            text.matches('\n').count(),
            text.matches("\r\n").count(),
            "every newline is CRLF"
        );
    }

    #[test]
    fn dry_run_writes_nothing() {
        let s = Sandbox::from_fixture("basic");
        let original = s.read();
        s.cmd()
            .args(["set", "bastion", "Port=1", "--dry-run"])
            .assert()
            .success()
            .stdout(predicate::str::contains("-    Port 2222\n+    Port 1\n"))
            .stderr(predicate::str::contains("Dry run"));
        assert_eq!(s.read(), original);
        assert_eq!(s.backups(), Vec::<PathBuf>::new());
    }

    #[test]
    fn ssh_rejection_blocks_the_write() {
        if !ssh_available() {
            eprintln!("note: ssh not installed; skipping");
            return;
        }
        let s = Sandbox::from_fixture("basic");
        let original = s.read();
        s.cmd()
            .args(["set", "bastion", "Port=not-a-port", "-y"])
            .assert()
            .code(5)
            .stderr(predicate::str::contains("ssh rejected the edited config"))
            .stderr(predicate::str::contains("<edited config>"));
        assert_eq!(s.read(), original);
        assert_eq!(s.backups(), Vec::<PathBuf>::new());
        assert_eq!(s.stray_files(), Vec::<String>::new());

        // The escape hatch works.
        s.cmd()
            .args(["set", "bastion", "Port=not-a-port", "-y", "--no-validate"])
            .assert()
            .success();
        assert!(s.read().contains("Port not-a-port"));
    }

    #[test]
    fn edit_errors_have_exit_codes() {
        let s = Sandbox::from_fixture("basic");
        s.cmd().args(["rm", "nope", "-y"]).assert().code(3);
        s.cmd()
            .args(["unset", "bastion", "ProxyJump", "-y"])
            .assert()
            .code(3);
        s.cmd()
            .args(["add", "bastion", "-y"])
            .assert()
            .code(1)
            .stderr(predicate::str::contains("already exists (Host line 6)"));
        s.cmd()
            .args(["set", "bastion", "Prot=1", "-y"])
            .assert()
            .code(1)
            .stderr(predicate::str::contains("did you mean `Port`?"));
        assert_eq!(
            s.backups(),
            Vec::<PathBuf>::new(),
            "failed edits never back up"
        );
    }

    #[test]
    fn creates_a_missing_config_with_private_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("fresh/config");
        sshcm(config.to_str().unwrap())
            .args(["add", "first", "--hostname", "1.2.3.4", "-y"])
            .assert()
            .success();
        assert_eq!(
            fs::read_to_string(&config).unwrap(),
            "Host first\n    HostName 1.2.3.4\n"
        );
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&config).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn symlinked_config_stays_a_symlink() {
        let s = Sandbox::from_fixture("basic");
        let link = s.dir.path().join("link-config");
        std::os::unix::fs::symlink(&s.config, &link).unwrap();
        sshcm(link.to_str().unwrap())
            .args(["set", "bastion", "User=root", "-y"])
            .assert()
            .success();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(s.read().contains("    User root\n"));
    }

    #[test]
    fn rename_cp_unset_round_trip() {
        let s = Sandbox::from_fixture("basic");
        s.cmd()
            .args(["cp", "bastion", "bastion2", "-y"])
            .assert()
            .success();
        s.cmd()
            .args(["rename", "bastion2", "jump", "-y"])
            .assert()
            .success();
        s.cmd()
            .args(["unset", "jump", "Port", "User", "-y"])
            .assert()
            .success();
        s.cmd()
            .args(["show", "jump"])
            .assert()
            .success()
            .stdout(predicate::str::ends_with(
                "Host jump\n    HostName 203.0.113.10\n",
            ));
    }
}

// ---------------------------------------------------------------------------
// Include: reads span files, edits go to the file that owns the alias
// ---------------------------------------------------------------------------

mod include {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    /// A fake home: ~/.ssh/config including ~/.ssh/conf.d/*.conf.
    struct Home {
        dir: tempfile::TempDir,
    }

    impl Home {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let ssh = dir.path().join(".ssh");
            fs::create_dir_all(ssh.join("conf.d")).unwrap();
            fs::write(
                ssh.join("config"),
                "Include conf.d/*.conf\n\nHost root-host\n    HostName 10.0.0.1\n\nHost *\n    User fallback\n",
            )
            .unwrap();
            fs::write(
                ssh.join("conf.d/work.conf"),
                "Host work\n    HostName work.example\n    User alice\n",
            )
            .unwrap();
            Self { dir }
        }
        fn ssh(&self) -> PathBuf {
            self.dir.path().join(".ssh")
        }
        fn read(&self, rel: &str) -> String {
            fs::read_to_string(self.ssh().join(rel)).unwrap()
        }
        fn cmd(&self) -> Command {
            let mut c = Command::cargo_bin("sshcm").unwrap();
            c.env_remove("SSHCM_CONFIG")
                .env("HOME", self.dir.path())
                .arg("-F")
                .arg(self.ssh().join("config"));
            c
        }
    }

    #[test]
    fn list_spans_files_in_read_order() {
        let h = Home::new();
        h.cmd()
            .arg("list")
            .assert()
            .success()
            .stdout(predicate::str::contains("SOURCE"))
            .stdout(predicate::function(|out: &str| {
                let work = out.find("~/.ssh/conf.d/work.conf:1").unwrap();
                let root = out.find("~/.ssh/config:3").unwrap();
                work < root // the Include comes first in the root file
            }));
    }

    #[test]
    fn show_and_resolve_report_the_included_file() {
        let h = Home::new();
        h.cmd()
            .args(["show", "work"])
            .assert()
            .success()
            .stdout(predicate::str::starts_with(
                "# ~/.ssh/conf.d/work.conf:1\nHost work\n",
            ));
        h.cmd()
            .args(["resolve", "work"])
            .assert()
            .success()
            .stdout(predicate::str::contains("# ~/.ssh/conf.d/work.conf:3"))
            .stdout(predicate::str::contains("User      alice"));
    }

    #[test]
    fn set_edits_the_owning_file_only() {
        let h = Home::new();
        let root_before = h.read("config");
        h.cmd()
            .args(["set", "work", "Port=2200", "-y"])
            .assert()
            .success();
        assert_eq!(h.read("config"), root_before);
        assert!(
            h.read("conf.d/work.conf")
                .ends_with("    User alice\n    Port 2200\n")
        );
        assert!(Path::new(&h.ssh().join("conf.d/.sshcm/backups")).is_dir());
    }

    #[test]
    fn add_refuses_alias_defined_in_an_included_file() {
        let h = Home::new();
        h.cmd()
            .args(["add", "work", "-y"])
            .assert()
            .code(1)
            .stderr(predicate::str::contains(
                "`work` already exists (~/.ssh/conf.d/work.conf:1)",
            ));
    }

    #[test]
    fn add_goes_to_the_root_file() {
        let h = Home::new();
        h.cmd()
            .args(["add", "new", "--hostname", "n", "-y"])
            .assert()
            .success();
        assert!(
            h.read("config")
                .contains("Host new\n    HostName n\n\nHost *")
        );
    }

    #[test]
    fn include_problems_are_warnings_not_errors() {
        let h = Home::new();
        fs::write(
            h.ssh().join("conf.d/loop.conf"),
            "Include conf.d/loop.conf\n",
        )
        .unwrap();
        h.cmd()
            .arg("list")
            .assert()
            .success()
            .stderr(predicate::str::contains("Include cycle"))
            .stdout(predicate::str::contains("work"));
    }
}

// ---------------------------------------------------------------------------
// lint, fmt, export/import, backups, completions
// ---------------------------------------------------------------------------

mod whole_config {
    use super::*;
    use std::fs;

    fn sandbox(contents: &str) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        fs::write(&config, contents).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let p = config.to_str().unwrap().to_owned();
        (dir, p)
    }

    #[test]
    fn lint_reports_errors_with_exit_4() {
        let (_d, cfg) = sandbox("Host a\n  Port 0\n  Hostnme x\n");
        sshcm(&cfg)
            .arg("lint")
            .assert()
            .code(4)
            .stdout(predicate::str::contains(":2: error[E002]: `Port 0`"))
            .stdout(predicate::str::contains(
                ":3: error[E001]: unknown keyword `Hostnme` (did you mean `HostName`?)",
            ));
    }

    #[test]
    fn lint_warnings_pass_unless_strict() {
        let (_d, cfg) = sandbox("Host *\n  User root\nHost a\n  User me\n");
        sshcm(&cfg)
            .arg("lint")
            .assert()
            .success()
            .stdout(predicate::str::contains("W002"));
        sshcm(&cfg).args(["lint", "--strict"]).assert().code(4);
        let out = sshcm(&cfg)
            .args(["lint", "--json"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v[0]["code"], "W002");
        assert_eq!(v[0]["severity"], "warning");
    }

    #[test]
    fn clean_fixture_lints_clean() {
        sshcm(&fixture("basic"))
            .args(["lint", "--no-file-checks"])
            .assert()
            .success()
            .stderr(predicate::str::contains("No problems found."));
    }

    #[test]
    fn fmt_check_then_apply() {
        let (_d, cfg) = sandbox("host a\n\tuser=bob   \n\n\n\nHost b\n  port 22\n");
        sshcm(&cfg)
            .args(["fmt", "--check"])
            .assert()
            .code(1)
            .stdout(predicate::str::contains("+Host a"))
            .stderr(predicate::str::contains("1 file(s) would be reformatted"));
        sshcm(&cfg).args(["fmt", "-y"]).assert().success();
        assert_eq!(
            fs::read_to_string(&cfg).unwrap(),
            "Host a\n    User bob\n\nHost b\n    Port 22\n"
        );
        sshcm(&cfg).args(["fmt", "--check"]).assert().success();
    }

    #[test]
    fn export_import_round_trip_through_cli() {
        let exported = sshcm(&fixture("basic"))
            .arg("export")
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let (dir, _) = sandbox("");
        let json = dir.path().join("hosts.json");
        fs::write(&json, &exported).unwrap();
        let fresh = dir.path().join("fresh-config");

        sshcm(fresh.to_str().unwrap())
            .args(["import", json.to_str().unwrap(), "-y"])
            .assert()
            .success();
        let again = sshcm(fresh.to_str().unwrap())
            .arg("export")
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();

        let hosts = |b: &[u8]| -> Vec<serde_json::Value> {
            let v: serde_json::Value = serde_json::from_slice(b).unwrap();
            v["hosts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|h| serde_json::json!([h["host"], h["options"]]))
                .collect()
        };
        assert_eq!(hosts(&again), hosts(&exported));

        // Importing again conflicts on every host.
        sshcm(fresh.to_str().unwrap())
            .args(["import", json.to_str().unwrap(), "-y"])
            .assert()
            .code(1)
            .stderr(predicate::str::contains(
                "already defined: github.com, bastion, *",
            ));
        sshcm(fresh.to_str().unwrap())
            .args(["import", json.to_str().unwrap(), "--skip-existing", "-y"])
            .assert()
            .success()
            .stderr(predicate::str::contains("Nothing to import."));
    }

    #[test]
    fn backup_list_and_restore() {
        let (_d, cfg) = sandbox("Host a\n    User one\n");
        sshcm(&cfg)
            .arg("backup")
            .arg("list")
            .assert()
            .success()
            .stderr(predicate::str::contains("No backups"));
        sshcm(&cfg)
            .args(["set", "a", "User=two", "-y"])
            .assert()
            .success();
        sshcm(&cfg)
            .args(["set", "a", "User=three", "-y"])
            .assert()
            .success();
        sshcm(&cfg)
            .args(["backup", "list"])
            .assert()
            .success()
            .stdout(predicate::str::contains("  1  config."))
            .stdout(predicate::str::contains("  2  config."));

        // Backup 2 (older) holds "one"; restoring it is itself backed up.
        sshcm(&cfg)
            .args(["backup", "restore", "2", "-y"])
            .assert()
            .success();
        assert_eq!(fs::read_to_string(&cfg).unwrap(), "Host a\n    User one\n");
        sshcm(&cfg)
            .args(["backup", "restore", "-y"])
            .assert()
            .success();
        assert_eq!(
            fs::read_to_string(&cfg).unwrap(),
            "Host a\n    User three\n"
        );
        sshcm(&cfg)
            .args(["backup", "restore", "99"])
            .assert()
            .code(1);
    }

    #[test]
    fn completions_and_manpage() {
        Command::cargo_bin("sshcm")
            .unwrap()
            .args(["completions", "bash"])
            .assert()
            .success()
            .stdout(predicate::str::contains("_sshcm()"));
        Command::cargo_bin("sshcm")
            .unwrap()
            .arg("manpage")
            .assert()
            .success()
            .stdout(predicate::str::contains(".TH sshcm"));
    }
}

/// Editing a file ssh already rejects (here: not an ssh config at all) is
/// refused, so a mistaken `-F` can never corrupt an unrelated file.
#[test]
fn refuses_to_edit_a_file_ssh_rejects() {
    if std::process::Command::new("ssh")
        .arg("-V")
        .output()
        .is_err()
    {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let not_ssh = dir.path().join("data.json");
    std::fs::write(&not_ssh, "{\n  \"hosts\": []\n}\n").unwrap();
    sshcm(not_ssh.to_str().unwrap())
        .args(["add", "x", "--hostname", "h", "-y"])
        .assert()
        .code(5)
        .stderr(predicate::str::contains("ssh already rejects"))
        .stderr(predicate::str::contains("sshcm lint"));
    assert_eq!(
        std::fs::read_to_string(&not_ssh).unwrap(),
        "{\n  \"hosts\": []\n}\n"
    );
}

// ---------------------------------------------------------------------------
// Forwarding
// ---------------------------------------------------------------------------

mod forwarding {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn sandbox_from(name: &str) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        fs::copy(fixture(name), &config).unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        let p = config.to_str().unwrap().to_owned();
        (dir, p)
    }

    #[test]
    fn list_shows_forwards_column_with_truncation_and_flags() {
        sshcm(&fixture("forwards"))
            .arg("list")
            .assert()
            .success()
            .stdout(predicate::str::contains("FORWARDS"))
            .stdout(predicate::str::contains(
                "L5432→localhost:5432 L127.0.0.1:6379→cache.internal:6379 ",
            ))
            .stdout(predicate::str::contains("D1080 D[::1]:1081 +3 A"))
            .stdout(predicate::str::contains(
                "L2375→/var/run/docker.sock L/tmp/pg.sock→db.internal:5432 +1 X",
            ));
    }

    #[test]
    fn list_json_has_structured_forwards() {
        let out = sshcm(&fixture("forwards"))
            .args(["list", "--json"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        let jump = &v[1];
        assert_eq!(jump["patterns"][0], "jump");
        assert_eq!(jump["forward_agent"], true);
        assert_eq!(jump["forwards"].as_array().unwrap().len(), 5);
        assert_eq!(jump["forwards"][4]["kind"], "remote");
        assert_eq!(jump["forwards"][4]["target"], serde_json::Value::Null);
        assert_eq!(jump["forwards"][4]["listen"]["host"], "*");
    }

    #[test]
    fn forward_list_shows_every_forward_with_types() {
        sshcm(&fixture("forwards"))
            .args(["forward", "list", "jump"])
            .assert()
            .success()
            .stdout(predicate::str::contains("reverse-dynamic  *:1090"))
            .stdout(predicate::str::contains("(SOCKS proxy on remote)"))
            .stdout(predicate::str::contains(
                "remote           0           localhost:3001",
            ));
        sshcm(&fixture("forwards"))
            .args(["forward", "list", "nope"])
            .assert()
            .code(3);
        let out = sshcm(&fixture("forwards"))
            .args(["forward", "list", "--json"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 10);
        assert_eq!(v[0]["config"], "LocalForward 5432 localhost:5432");
    }

    #[test]
    fn forward_add_writes_config_syntax_and_rm_undoes_it() {
        let (_d, cfg) = sandbox_from("basic");
        let original = fs::read_to_string(&cfg).unwrap();
        sshcm(&cfg)
            .args([
                "forward",
                "add",
                "bastion",
                "-L",
                "127.0.0.1:5432:db:5432",
                "-D",
                "1080",
                "-y",
            ])
            .assert()
            .success()
            .stdout(predicate::str::contains(
                "+    LocalForward 127.0.0.1:5432 db:5432",
            ))
            .stdout(predicate::str::contains("+    DynamicForward 1080"));
        sshcm(&cfg)
            .args(["forward", "add", "bastion", "-D", "1080", "-y"])
            .assert()
            .code(1)
            .stderr(predicate::str::contains(
                "already has a forward listening on -D 1080",
            ));
        sshcm(&cfg)
            .args([
                "forward",
                "rm",
                "bastion",
                "-L",
                "127.0.0.1:5432",
                "-D",
                "1080",
                "-y",
            ])
            .assert()
            .success();
        assert_eq!(fs::read_to_string(&cfg).unwrap(), original);
        sshcm(&cfg)
            .args(["forward", "rm", "bastion", "-D", "1080", "-y"])
            .assert()
            .code(3);
    }

    #[test]
    fn add_accepts_forward_flags_and_bad_specs_fail_early() {
        let (_d, cfg) = sandbox_from("basic");
        sshcm(&cfg)
            .args([
                "add",
                "db",
                "--hostname",
                "10.0.0.5",
                "-L",
                "5432:localhost:5432",
                "-R",
                "9000",
                "-y",
            ])
            .assert()
            .success();
        assert!(fs::read_to_string(&cfg).unwrap().contains(
            "Host db\n    HostName 10.0.0.5\n    LocalForward 5432 localhost:5432\n    RemoteForward 9000\n"
        ));
        sshcm(&cfg)
            .args(["forward", "add", "db", "-L", "8080:localhost", "-y"])
            .assert()
            .code(1)
            .stderr(predicate::str::contains("-L 8080:localhost"));
        sshcm(&cfg)
            .args(["forward", "add", "db", "-y"])
            .assert()
            .code(1)
            .stderr(predicate::str::contains("at least one of -L, -R, -D"));
    }

    #[test]
    fn lint_reports_forward_conflicts() {
        let (_d, cfg) = sandbox_from("forwards");
        sshcm(&cfg)
            .args(["forward", "add", "db", "-D", "5432", "-y"])
            .assert()
            .success();
        sshcm(&cfg)
            .args(["lint", "--no-file-checks"])
            .assert()
            .success()
            .stdout(predicate::str::contains("warning[W007]"))
            .stdout(predicate::str::contains("same local endpoint `5432`"));
    }
}

// ---------------------------------------------------------------------------
// parse view | add
// ---------------------------------------------------------------------------

mod parse {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn sandbox_from(name: &str) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        fs::copy(fixture(name), &config).unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        let p = config.to_str().unwrap().to_owned();
        (dir, p)
    }

    #[test]
    fn view_prints_the_record_for_the_users_example() {
        sshcm(&fixture("basic"))
            .args(["parse", "view", "ssh -i ~/.ssh/id_rsa user@host"])
            .assert()
            .success()
            .stdout("Host host\n    User user\n    IdentityFile ~/.ssh/id_rsa\n")
            .stderr("");
    }

    #[test]
    fn show_alias_as_name_and_unquoted_form() {
        sshcm(&fixture("basic"))
            .args([
                "parse",
                "show",
                "--as",
                "db",
                "ssh alice@db.example.com -p 2222",
            ])
            .assert()
            .success()
            .stdout("Host db\n    HostName db.example.com\n    User alice\n    Port 2222\n");
        sshcm(&fixture("basic"))
            .args(["parse", "view", "ssh", "-p", "2200", "bob@h"])
            .assert()
            .success()
            .stdout("Host h\n    User bob\n    Port 2200\n");
    }

    #[test]
    fn view_uses_the_files_style() {
        // comments_and_globals.config indents with tabs.
        sshcm(&fixture("comments_and_globals"))
            .args(["parse", "view", "ssh -p 22 u@h"])
            .assert()
            .success()
            .stdout("Host h\n\tUser u\n\tPort 22\n");
    }

    #[test]
    fn one_off_flags_and_remote_command_become_notes() {
        sshcm(&fixture("basic"))
            .args(["parse", "view", "ssh -v -N -A h uptime"])
            .assert()
            .success()
            .stdout("Host h\n    ForwardAgent yes\n")
            .stderr(predicate::str::contains(
                "`-v` affects a single invocation; not written",
            ))
            .stderr(predicate::str::contains("`-N` affects a single invocation"))
            .stderr(predicate::str::contains(
                "remote command `uptime` is not part of the record",
            ));
    }

    #[test]
    fn add_writes_the_users_second_example() {
        let (dir, cfg) = sandbox_from("basic");
        sshcm(&cfg)
            .args(["parse", "add", "-y", "ssh -i ~/.ssh/id_rsa user@host -p 22"])
            .assert()
            .success()
            .stdout(predicate::str::contains(
                "+Host host\n+    User user\n+    Port 22\n+    IdentityFile ~/.ssh/id_rsa\n",
            ));
        let text = fs::read_to_string(&cfg).unwrap();
        assert!(text.contains(
            "Host host\n    User user\n    Port 22\n    IdentityFile ~/.ssh/id_rsa\n\nHost *"
        ));
        assert!(dir.path().join(".sshcm/backups").is_dir());

        // Adding it again is refused, with a hint.
        sshcm(&cfg)
            .args(["parse", "add", "-y", "ssh user@host"])
            .assert()
            .code(1)
            .stderr(predicate::str::contains("`host` already exists"))
            .stderr(predicate::str::contains("use --as NAME"));
        sshcm(&cfg)
            .args(["parse", "add", "-y", "--as", "host2", "ssh user@host"])
            .assert()
            .success();
        assert!(
            fs::read_to_string(&cfg)
                .unwrap()
                .contains("Host host2\n    HostName host\n    User user\n")
        );
    }

    #[test]
    fn bad_input_fails_before_anything_is_written() {
        let (_d, cfg) = sandbox_from("basic");
        let original = fs::read_to_string(&cfg).unwrap();
        for (cmd, msg) in [
            ("ssh -o Bogus=1 h", "unknown keyword `Bogus`"),
            ("ssh -v", "no destination host"),
            ("ssh -J a -J b h", "only a single -J"),
            ("ssh 'unterminated h", "unterminated ' quote"),
        ] {
            sshcm(&cfg)
                .args(["parse", "add", "-y", cmd])
                .assert()
                .code(1)
                .stderr(predicate::str::contains(msg));
        }
        sshcm(&cfg)
            .args(["parse", "add", "ssh user@x", "-y"])
            .assert()
            .code(1)
            .stderr(predicate::str::contains("must be one quoted argument"));
        assert_eq!(fs::read_to_string(&cfg).unwrap(), original);
    }
}
