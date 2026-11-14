//! Turns text into a [`Document`]: split into lines (preserving terminators),
//! lex each line, then index the lines into blocks.

use super::cst::{Block, BlockKind, Document, Eol, Line};

/// Split `input` into `(text, eol)` pairs. Only `\n` and `\r\n` terminate
/// lines; a lone `\r` stays part of the text. Empty input yields no lines.
fn split_lines(input: &str) -> impl Iterator<Item = (&str, Eol)> {
    let mut rest = input;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let item = match rest.find('\n') {
            Some(nl) => {
                let (text, eol) = match rest[..nl].strip_suffix('\r') {
                    Some(t) => (t, Eol::CrLf),
                    None => (&rest[..nl], Eol::Lf),
                };
                rest = &rest[nl + 1..];
                (text, eol)
            }
            None => (std::mem::take(&mut rest), Eol::None),
        };
        Some(item)
    })
}

fn header_kind(line: &Line) -> Option<BlockKind> {
    if line.is_keyword("Host") {
        Some(BlockKind::Host)
    } else if line.is_keyword("Match") {
        Some(BlockKind::Match)
    } else {
        None
    }
}

pub(super) fn parse(input: &str) -> Document {
    let lines: Vec<Line> = split_lines(input).map(|(t, e)| Line::new(t, e)).collect();
    let blocks = index_blocks(&lines);
    Document { lines, blocks }
}

/// Group lines into blocks: a global block, then one per `Host`/`Match` header.
pub(super) fn index_blocks(lines: &[Line]) -> Vec<Block> {
    let mut blocks = vec![Block {
        kind: BlockKind::Global,
        lines: 0..0,
    }];
    for (i, line) in lines.iter().enumerate() {
        if let Some(kind) = header_kind(line) {
            blocks.push(Block { kind, lines: i..i });
        }
        // Every line extends the most recent block.
        blocks
            .last_mut()
            .expect("global block always exists")
            .lines
            .end = i + 1;
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn split(s: &str) -> Vec<(&str, Eol)> {
        split_lines(s).collect()
    }

    #[test]
    fn split_preserves_terminators() {
        assert_eq!(split(""), vec![]);
        assert_eq!(split("a"), vec![("a", Eol::None)]);
        assert_eq!(split("a\n"), vec![("a", Eol::Lf)]);
        assert_eq!(
            split("a\r\nb\nc"),
            vec![("a", Eol::CrLf), ("b", Eol::Lf), ("c", Eol::None)]
        );
        assert_eq!(split("\n\n"), vec![("", Eol::Lf), ("", Eol::Lf)]);
        // Lone CR is not a terminator.
        assert_eq!(split("a\rb\n"), vec![("a\rb", Eol::Lf)]);
        assert_eq!(split("\r"), vec![("\r", Eol::None)]);
    }

    #[test]
    fn blocks_cover_every_line() {
        let doc = Document::parse(
            "# global\nUser me\n\nHost a\n  Port 1\n# about b\nhost=b\nMatch all\n  User x\n",
        );
        let kinds: Vec<_> = doc.blocks().iter().map(|b| (b.kind(), b.lines())).collect();
        assert_eq!(
            kinds,
            vec![
                (BlockKind::Global, 0..3),
                (BlockKind::Host, 3..6),
                (BlockKind::Host, 6..7),
                (BlockKind::Match, 7..9),
            ]
        );
    }

    #[test]
    fn global_block_is_empty_when_file_starts_with_host() {
        let doc = Document::parse("Host a\n");
        assert_eq!(doc.blocks()[0].lines(), 0..0);
        assert_eq!(doc.blocks()[0].header(), None);
        assert_eq!(doc.blocks()[1].header(), Some(0));
    }

    #[test]
    fn hostname_is_not_a_header() {
        // `HostName` starts with "Host" but is a different keyword.
        let doc = Document::parse("Host a\n  HostName example.com\n");
        assert_eq!(doc.blocks().len(), 2);
    }

    #[test]
    fn preferred_eol() {
        assert_eq!(Document::parse("").preferred_eol(), Eol::Lf);
        assert_eq!(Document::parse("a").preferred_eol(), Eol::Lf);
        assert_eq!(Document::parse("a\r\nb\n").preferred_eol(), Eol::CrLf);
    }
}
