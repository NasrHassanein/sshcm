//! Edit guarantees on real-world fixtures:
//! - `add` then `rm` restores the file byte for byte (at every position);
//! - edits only touch the lines they must (golden snapshots).

use std::path::Path;

use pretty_assertions::assert_eq;
use sshcm::edit::{self, Position};
use sshcm::hosts::host_entries;
use sshcm::syntax::Document;

fn fixtures() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut out: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "config"))
        .map(|p| {
            let name = p.file_stem().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read_to_string(&p).unwrap())
        })
        .collect();
    out.sort();
    out
}

fn fixture(name: &str) -> String {
    fixtures().into_iter().find(|(n, _)| n == name).unwrap().1
}

fn opts() -> Vec<(String, String)> {
    vec![
        ("HostName".into(), "10.9.8.7".into()),
        ("User".into(), "tester".into()),
    ]
}

#[test]
fn add_then_rm_is_identity_on_every_fixture() {
    for (name, original) in fixtures() {
        let doc = Document::parse(&original);
        let mut positions = vec![Position::Auto];
        for e in host_entries(&doc) {
            if let [alias] = e.patterns.as_slice()
                && !alias.contains(['*', '?', '!'])
            {
                positions.push(Position::Before(alias.clone()));
            }
        }

        for pos in positions {
            let mut d = doc.clone();
            edit::add_host(&mut d, "sshcm-probe", &opts(), &pos).unwrap();
            assert_ne!(d.render(), original, "{name} {pos:?}: add changed nothing");
            edit::remove_host(&mut d, "sshcm-probe").unwrap();
            assert_eq!(d.render(), original, "{name} {pos:?}: rm did not undo add");
        }
    }
}

#[test]
fn set_then_unset_new_key_is_identity() {
    for (name, original) in fixtures() {
        let doc = Document::parse(&original);
        for e in host_entries(&doc) {
            let Some(alias) = e.patterns.iter().find(|p| !p.contains(['*', '?', '!'])) else {
                continue;
            };
            let mut d = doc.clone();
            edit::set(&mut d, alias, "ConnectTimeout", "7", false).unwrap();
            edit::unset(&mut d, alias, "ConnectTimeout").unwrap();
            assert_eq!(d.render(), original, "{name}: set/unset on `{alias}`");
        }
    }
}

/// Apply `f` to a fixture and snapshot a unified diff of the result, so the
/// snapshot shows exactly which lines changed.
fn snapshot_edit(
    name: &str,
    fixture_name: &str,
    f: impl FnOnce(&mut Document) -> Result<(), edit::EditError>,
) {
    let original = fixture(fixture_name);
    let mut doc = Document::parse(&original);
    f(&mut doc).unwrap();
    let edited = doc.render();
    // `&str` arguments work with both similar 2.x and 3.x signatures.
    let diff = similar::TextDiff::from_lines(original.as_str(), edited.as_str())
        .unified_diff()
        .context_radius(2)
        .to_string();
    insta::assert_snapshot!(name, diff);
}

#[test]
fn golden_add_before_catch_all() {
    snapshot_edit("add_before_catch_all", "basic", |d| {
        edit::add_host(d, "db", &opts(), &Position::Auto)
    });
}

#[test]
fn golden_add_after() {
    snapshot_edit("add_after_github", "basic", |d| {
        edit::add_host(d, "db", &opts(), &Position::After("github.com".into()))
    });
}

#[test]
fn golden_set_preserves_separator_style() {
    snapshot_edit("set_existing_equals_style", "separators", |d| {
        edit::set(d, "a", "port", "2222", false)?;
        edit::set(d, "a", "user", "carol", false)
    });
}

#[test]
fn golden_set_new_key_uses_block_indent() {
    snapshot_edit("set_new_key_tab_indent", "comments_and_globals", |d| {
        edit::set(d, "work-legacy", "User", "legacy", false)
    });
}

#[test]
fn golden_rm_takes_intro_and_trailing_comments() {
    snapshot_edit("rm_with_comments", "comments_and_globals", |d| {
        edit::rename(d, "work-legacy", "legacy")?;
        // `work-*` is a wildcard, not an alias, so remove via the other
        // block's alias to show comment handling around a block boundary.
        edit::remove_host(d, "legacy")
    });
}

#[test]
fn golden_rm_one_of_two_patterns() {
    snapshot_edit("rm_one_pattern", "separators", |d| {
        edit::remove_host(d, "b")
    });
}

#[test]
fn golden_crlf_add() {
    snapshot_edit("crlf_add", "crlf", |d| {
        edit::add_host(d, "new", &opts(), &Position::Auto)
    });
}

#[test]
fn golden_copy() {
    snapshot_edit("copy_bastion", "basic", |d| {
        edit::copy(d, "bastion", "bastion2")
    });
}

// ---------------------------------------------------------------------------
// Forwarding
// ---------------------------------------------------------------------------

use sshcm::model::forward::{Forward, ForwardKind, parse_listen};

fn fwd(kind: ForwardKind, spec: &str) -> Forward {
    Forward::from_cli(kind, spec).unwrap()
}

#[test]
fn forward_add_then_rm_is_identity_on_every_fixture() {
    for (name, original) in fixtures() {
        let doc = Document::parse(&original);
        for e in host_entries(&doc) {
            let Some(alias) = e.patterns.iter().find(|p| !p.contains(['*', '?', '!'])) else {
                continue;
            };
            let mut d = doc.clone();
            edit::add_forward(&mut d, alias, &fwd(ForwardKind::Local, "59999:localhost:1"))
                .unwrap();
            edit::add_forward(&mut d, alias, &fwd(ForwardKind::Dynamic, "59998")).unwrap();
            edit::remove_forward(
                &mut d,
                alias,
                ForwardKind::Local,
                &parse_listen(ForwardKind::Local, "59999").unwrap(),
            )
            .unwrap();
            edit::remove_forward(
                &mut d,
                alias,
                ForwardKind::Dynamic,
                &parse_listen(ForwardKind::Dynamic, "59998").unwrap(),
            )
            .unwrap();
            assert_eq!(d.render(), original, "{name}: forward add/rm on `{alias}`");
        }
    }
}

#[test]
fn golden_forward_add_uses_block_style() {
    snapshot_edit("forward_add_tab_indent", "comments_and_globals", |d| {
        edit::add_forward(
            d,
            "work-legacy",
            &fwd(ForwardKind::Local, "8080:localhost:80"),
        )?;
        edit::add_forward(d, "work-legacy", &fwd(ForwardKind::Remote, "9000"))?;
        edit::add_forward(d, "work-legacy", &fwd(ForwardKind::Dynamic, "[::1]:1080"))
    });
}

#[test]
fn golden_forward_rm_removes_only_the_named_tunnel() {
    snapshot_edit("forward_rm_one", "forwards", |d| {
        edit::remove_forward(
            d,
            "docker",
            ForwardKind::Local,
            &parse_listen(ForwardKind::Local, "2375").unwrap(),
        )?;
        edit::remove_forward(
            d,
            "jump",
            ForwardKind::Remote,
            &parse_listen(ForwardKind::Remote, "*:1090").unwrap(),
        )
    });
}
