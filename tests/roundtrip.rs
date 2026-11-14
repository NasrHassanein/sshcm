//! The lossless round-trip guarantee: `render(parse(s)) == s` for all inputs.

use std::fmt::Write as _;
use std::path::Path;

use pretty_assertions::assert_eq;
use proptest::prelude::*;
use sshcm::syntax::{BlockKind, Document, LineKind};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn fixtures() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut out: Vec<_> = std::fs::read_dir(&dir)
        .expect("fixtures dir")
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "config"))
        .map(|p| {
            let name = p.file_stem().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read_to_string(&p).expect("utf-8 fixture"))
        })
        .collect();
    out.sort();
    assert!(!out.is_empty(), "no fixtures found in {}", dir.display());
    out
}

/// A compact, human-readable description of how a document was parsed.
fn outline(doc: &Document) -> String {
    let mut s = String::new();
    for b in doc.blocks() {
        let kind = match b.kind() {
            BlockKind::Global => "Global",
            BlockKind::Host => "Host",
            BlockKind::Match => "Match",
        };
        writeln!(s, "{kind} {:?}", b.lines()).unwrap();
        for line in doc.block_lines(b) {
            let desc = match line.kind() {
                LineKind::Blank => "blank".to_owned(),
                LineKind::Comment { .. } => format!("comment {:?}", line.text().trim_start()),
                LineKind::Invalid => format!("INVALID {:?}", line.text()),
                LineKind::Directive(_) => format!(
                    "{:?} sep={:?} value={:?}",
                    line.keyword().unwrap(),
                    line.separator().unwrap(),
                    line.value().unwrap()
                ),
            };
            writeln!(s, "  [{:?}] {desc}", line.eol()).unwrap();
        }
    }
    s
}

#[test]
fn fixtures_round_trip_byte_for_byte() {
    for (name, text) in fixtures() {
        assert_eq!(Document::parse(&text).render(), text, "fixture {name}");
    }
}

#[test]
fn fixtures_parse_structure() {
    for (name, text) in fixtures() {
        insta::assert_snapshot!(name.clone(), outline(&Document::parse(&text)));
    }
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

/// Blocks start with the global block, are contiguous, and cover every line.
fn assert_blocks_partition(doc: &Document) {
    let blocks = doc.blocks();
    assert_eq!(blocks[0].kind(), BlockKind::Global);
    assert_eq!(blocks[0].lines().start, 0);
    for w in blocks.windows(2) {
        assert_eq!(w[0].lines().end, w[1].lines().start);
    }
    assert_eq!(blocks.last().unwrap().lines().end, doc.lines().len());
}

fn keyword() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("Host".to_owned()),
        Just("host".to_owned()),
        Just("HOST".to_owned()),
        Just("Match".to_owned()),
        Just("HostName".to_owned()),
        Just("IdentityFile".to_owned()),
        "[A-Za-z][A-Za-z0-9]{0,15}",
    ]
}

/// One realistic directive line, plus the keyword and value it must lex to.
fn directive() -> impl Strategy<Value = (String, String, String)> {
    (
        "[ \t]{0,4}",
        keyword(),
        prop_oneof![
            Just(" "),
            Just("\t"),
            Just("="),
            Just(" = "),
            Just("\t=\t"),
            Just("  ="),
            Just("=  ")
        ],
        // Values: no leading whitespace or '=', no trailing whitespace.
        prop_oneof![Just(String::new()), "[^\\s=][^\r\n]{0,30}[^\\s]"],
        "[ \t]{0,3}",
    )
        .prop_map(|(indent, key, sep, value, trailing)| {
            (format!("{indent}{key}{sep}{value}{trailing}"), key, value)
        })
}

fn eol() -> impl Strategy<Value = &'static str> {
    prop_oneof![Just("\n"), Just("\r\n")]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2048))]

    #[test]
    fn any_string_round_trips(s in any::<String>()) {
        let doc = Document::parse(&s);
        prop_assert_eq!(doc.render(), s);
        assert_blocks_partition(&doc);
    }

    #[test]
    fn any_config_ish_string_round_trips(s in "[ \t\r\n=#a-zA-Z0-9\"]{0,200}") {
        let doc = Document::parse(&s);
        prop_assert_eq!(doc.render(), s);
        assert_blocks_partition(&doc);
    }

    #[test]
    fn structured_lines_classify_correctly(
        lines in prop::collection::vec(
            prop_oneof![
                3 => directive().prop_map(Some),
                1 => "[ \t]{0,3}".prop_map(|ws| Some((ws, String::new(), String::new()))),
                1 => "[ \t]{0,3}#[^\r\n]{0,20}".prop_map(|c| Some((c, String::new(), String::new()))),
            ],
            0..40,
        ),
        eols in prop::collection::vec(eol(), 40),
        final_eol in any::<bool>(),
    ) {
        let lines: Vec<_> = lines.into_iter().flatten().collect();
        let mut input = String::new();
        for (i, (text, _, _)) in lines.iter().enumerate() {
            input.push_str(text);
            if i + 1 < lines.len() || final_eol {
                input.push_str(eols[i]);
            }
        }

        let doc = Document::parse(&input);
        prop_assert_eq!(doc.render(), input.clone());
        // An empty, unterminated final "line" contributes zero bytes, so it
        // does not exist in the parsed document.
        let phantom = !final_eol && lines.last().is_some_and(|(t, _, _)| t.is_empty());
        prop_assert_eq!(doc.lines().len(), lines.len() - usize::from(phantom));
        assert_blocks_partition(&doc);

        for (line, (_, key, value)) in doc.lines().iter().zip(&lines) {
            if key.is_empty() {
                let ok = matches!(line.kind(), LineKind::Blank | LineKind::Comment { .. });
                prop_assert!(ok, "expected blank/comment, got {:?}", line);
            } else {
                prop_assert_eq!(line.keyword(), Some(key.as_str()));
                prop_assert_eq!(line.value(), Some(value.as_str()));
            }
        }

        let headers = doc.lines().iter().filter(|l| l.is_keyword("host") || l.is_keyword("match")).count();
        prop_assert_eq!(doc.blocks().len(), headers + 1);
    }
}
