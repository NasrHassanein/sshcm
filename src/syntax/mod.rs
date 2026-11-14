//! Lossless syntax layer: lexer, concrete syntax tree, and parser.
//!
//! Invariant: for every input string `s`, `Document::parse(s).render() == s`.

mod cst;
mod lexer;
mod parser;

pub use cst::{Block, BlockKind, Document, Eol, Line};
pub use lexer::{DirectiveSpans, LineKind, Span};
