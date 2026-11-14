//! `sshcm` command-line entry point. All logic lives in the library crate;
//! the `cli` module only parses arguments, performs I/O, and maps errors to
//! exit codes.

mod cli;

fn main() -> std::process::ExitCode {
    cli::main()
}
