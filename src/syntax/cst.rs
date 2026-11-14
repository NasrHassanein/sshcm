//! Concrete syntax tree: a [`Document`] is a flat list of [`Line`]s plus an
//! index of [`Block`]s over them. Every line owns its exact original text, so
//! [`Document::render`] reproduces the input byte for byte.

use std::fmt;
use std::ops::Range;

use super::lexer::{DirectiveSpans, LineKind, classify};

/// A line terminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Eol {
    /// `\n`
    Lf,
    /// `\r\n`
    CrLf,
    /// No terminator (last line of a file without a trailing newline).
    None,
}

impl Eol {
    /// The terminator's text.
    pub const fn as_str(self) -> &'static str {
        match self {
            Eol::Lf => "\n",
            Eol::CrLf => "\r\n",
            Eol::None => "",
        }
    }
}

/// One physical line: its text (without terminator), its terminator, and its
/// lexical classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    text: String,
    eol: Eol,
    kind: LineKind,
}

impl Line {
    /// Lex `text` (which must not contain `\n`) into a line.
    pub fn new(text: impl Into<String>, eol: Eol) -> Self {
        let text = text.into();
        let kind = classify(&text);
        Self { text, eol, kind }
    }

    /// The line's text, without terminator.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The line's terminator.
    pub fn eol(&self) -> Eol {
        self.eol
    }

    /// The lexical classification.
    pub fn kind(&self) -> &LineKind {
        &self.kind
    }

    fn spans(&self) -> Option<&DirectiveSpans> {
        match &self.kind {
            LineKind::Directive(d) => Some(d),
            _ => None,
        }
    }

    /// The keyword as written (case preserved), if this is a directive.
    pub fn keyword(&self) -> Option<&str> {
        self.spans().map(|d| &self.text[d.key.clone()])
    }

    /// The raw value (quotes and inner whitespace preserved), if this is a directive.
    pub fn value(&self) -> Option<&str> {
        self.spans().map(|d| &self.text[d.value.clone()])
    }

    /// The separator as written (e.g. `" "`, `"="`, `" = "`), if this is a directive.
    pub fn separator(&self) -> Option<&str> {
        self.spans().map(|d| &self.text[d.sep.clone()])
    }

    /// Leading whitespace of a directive or comment; empty otherwise.
    pub fn indent(&self) -> &str {
        match &self.kind {
            LineKind::Directive(d) => &self.text[d.indent.clone()],
            LineKind::Comment { indent, .. } => &self.text[indent.clone()],
            LineKind::Blank | LineKind::Invalid => "",
        }
    }

    /// True if this directive's keyword equals `kw`, ignoring ASCII case
    /// (OpenSSH keywords are case-insensitive).
    pub fn is_keyword(&self, kw: &str) -> bool {
        self.keyword().is_some_and(|k| k.eq_ignore_ascii_case(kw))
    }

    /// True for blank (empty or whitespace-only) lines.
    pub fn is_blank(&self) -> bool {
        matches!(self.kind, LineKind::Blank)
    }

    /// True for full-line comments.
    pub fn is_comment(&self) -> bool {
        matches!(self.kind, LineKind::Comment { .. })
    }

    /// True for lines that carry configuration content: directives, and
    /// invalid lines (which are content OpenSSH would reject, not decoration).
    pub fn is_content(&self) -> bool {
        matches!(self.kind, LineKind::Directive(_) | LineKind::Invalid)
    }

    /// Build a directive line from its parts.
    pub fn directive(indent: &str, key: &str, sep: &str, value: &str, eol: Eol) -> Self {
        Self::new(format!("{indent}{key}{sep}{value}"), eol)
    }

    /// An empty line.
    pub fn blank(eol: Eol) -> Self {
        Self::new(String::new(), eol)
    }

    /// A copy of this directive with only its value replaced; indent,
    /// keyword, separator, and trailing whitespace are kept byte for byte.
    /// Returns `None` if this line is not a directive.
    pub fn with_value(&self, value: &str) -> Option<Self> {
        debug_assert!(!value.contains(['\n', '\r']), "value must be a single line");
        let d = self.spans()?;
        // `Host` with no separator and no value: a separator must be added.
        let sep = if d.sep.is_empty() && !value.is_empty() {
            " "
        } else {
            ""
        };
        let text = format!(
            "{}{sep}{value}{}",
            &self.text[..d.value.start],
            &self.text[d.value.end..]
        );
        Some(Self::new(text, self.eol))
    }
}

impl fmt::Display for Line {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)?;
        f.write_str(self.eol.as_str())
    }
}

/// What opened a block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlockKind {
    /// Directives before the first `Host`/`Match` line. Always present (may be empty).
    Global,
    /// A `Host pattern…` block.
    Host,
    /// A `Match criteria…` block.
    Match,
}

/// A contiguous run of lines governed by one header.
///
/// Following OpenSSH, a block extends from its header up to (not including)
/// the next `Host`/`Match` line. Comments and blank lines that visually
/// precede the *next* header therefore belong to *this* block; deciding
/// whether such a comment "describes" the next host is an editing concern,
/// not a parsing one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub(crate) kind: BlockKind,
    pub(crate) lines: Range<usize>,
}

impl Block {
    /// The block's kind.
    pub fn kind(&self) -> BlockKind {
        self.kind
    }

    /// Index range into [`Document::lines`], header included.
    pub fn lines(&self) -> Range<usize> {
        self.lines.clone()
    }

    /// Index of the header line (`None` for the global block).
    pub fn header(&self) -> Option<usize> {
        match self.kind {
            BlockKind::Global => None,
            BlockKind::Host | BlockKind::Match => Some(self.lines.start),
        }
    }
}

/// A parsed ssh config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub(crate) lines: Vec<Line>,
    pub(crate) blocks: Vec<Block>,
}

impl Document {
    /// Parse `input`. Never fails: unrecognised lines are kept verbatim.
    pub fn parse(input: &str) -> Self {
        super::parser::parse(input)
    }

    /// Reproduce the document's text. For an unmodified document this is
    /// byte-identical to the parser input.
    pub fn render(&self) -> String {
        self.to_string()
    }

    /// All lines, in file order.
    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    /// All blocks, in file order. The first is always [`BlockKind::Global`].
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    /// Lines belonging to `block`.
    pub fn block_lines(&self, block: &Block) -> &[Line] {
        &self.lines[block.lines()]
    }

    /// The terminator to use for newly inserted lines: the first one found
    /// in the file, or `\n` for an empty / single-unterminated-line file.
    pub fn preferred_eol(&self) -> Eol {
        self.lines
            .iter()
            .map(Line::eol)
            .find(|e| *e != Eol::None)
            .unwrap_or(Eol::Lf)
    }

    /// Replace the lines in `range` with `lines` (either may be empty) and
    /// re-index blocks.
    pub fn splice(&mut self, range: Range<usize>, lines: impl IntoIterator<Item = Line>) {
        self.lines.splice(range, lines);
        self.reindex();
    }

    /// Replace one line and re-index blocks (the line may change from or to
    /// a `Host`/`Match` header).
    pub fn replace_line(&mut self, index: usize, line: Line) {
        self.lines[index] = line;
        self.reindex();
    }

    /// Change one line's terminator. Does not affect block structure.
    pub fn set_eol(&mut self, index: usize, eol: Eol) {
        self.lines[index].eol = eol;
    }

    fn reindex(&mut self) {
        self.blocks = super::parser::index_blocks(&self.lines);
    }
}

impl fmt::Display for Document {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.lines.iter().try_for_each(|l| fmt::Display::fmt(l, f))
    }
}
