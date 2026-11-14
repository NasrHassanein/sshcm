//! `sshcm` — manage OpenSSH client configuration files without damaging them.
//!
//! The core guarantee is a lossless round trip: parsing a file and rendering it
//! back yields identical bytes, and edits touch only the lines they must.
//!
//! - [`syntax`]: lossless lexer, CST, and parser.
//! - [`model`]: keywords, argument splitting, host patterns.
//! - [`include`](mod@include): following `Include` into a [`ConfigSet`](crate::include::ConfigSet).
//! - [`hosts`]: per-block queries (`list`, `show`).
//! - [`resolve`]: effective configuration for a host (`resolve`).

#![forbid(unsafe_code)]

// The safety guarantees rely on Unix semantics: file modes, atomic rename
// over an existing file, directory fsync, and OpenSSH-portable behaviour.
#[cfg(not(unix))]
compile_error!("sshcm supports Unix-like systems (Linux, macOS) only");

pub mod diagnostic;
pub mod edit;
pub mod format;
pub mod hosts;
pub mod include;
pub mod interchange;
pub mod io;
pub mod lint;
pub mod model;
pub mod resolve;
pub mod syntax;
