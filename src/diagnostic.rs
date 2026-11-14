//! A message about a specific line of a specific config file.

use std::fmt;
use std::path::PathBuf;

use serde::Serialize;

/// Something worth telling the user about a config line: a limitation of
/// resolution, a problem following an `Include`, and (later) lint findings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    /// The file the line is in.
    pub file: PathBuf,
    /// 1-based line number.
    pub line: usize,
    /// Human-readable explanation.
    pub message: String,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.file.display(), self.line, self.message)
    }
}
