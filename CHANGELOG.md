# Changelog

## 0.4.1 — 2026-11-19

Bug fixes, and a cleaner codebase.

- Fixed a crash in `sshcm parse view|add` when an `-o` value starts with
  `#`, such as `-o Port=#1`. ssh reads `#` as the start of a comment, so
  the option has no value; sshcm now says so instead of panicking.
- `sshcm forward list` and `sshcm parse view` no longer panic when their
  output is piped into a command that stops reading early, such as `head`.
- Backup file names keep the same timestamp format, now generated without
  the unmaintained `humantime` crate (RUSTSEC-2026-0542).
- Internal refactoring with no other change in behaviour: shared helpers
  are written once, the CLI dispatches each subcommand in one place,
  `edit` is split into submodules, and value checks live in one table in
  `model::rules`.

## 0.4.0 — 2026-11-14

One-line install.

- New `install.sh`: `curl -fsSL https://github.com/NasrHassanein/sshcm/releases/latest/download/install.sh | sh -s -- --shell zsh`
  (or `bash`, `fish`) downloads the latest release for the OS and CPU,
  verifies its checksum, installs `sshcm` into `~/.local/bin` with its man
  page, and installs shell completion.
- Adds `~/.local/bin` to `PATH` in `~/.bashrc` or `~/.zshrc` (or a fish
  `conf.d` snippet) only when the file doesn't already do so. Its edits sit
  in one marked block: re-running replaces it, `--uninstall` removes it.
- Also shipped inside every release archive for offline installs, and
  attached to each release on its own.
- Fixed: `sshcm completions zsh | head` (or any reader closing the pipe
  early) panicked instead of exiting quietly.

## 0.3.0 — 2026-11-14

From ssh commands to config records.

- New `sshcm parse view "SSH_COMMAND"` (alias `show`) prints the Host
  record for an `ssh …` command, in the config file's own style; `sshcm
  parse add "SSH_COMMAND"` adds it (diff, confirmation, validation, backup).
- The alias defaults to the destination host; `--as NAME` names the entry
  and adds `HostName`.
- All connection flags are mapped, including forwards, `user@host`, and
  `ssh://user@host:port`, with ssh's exact precedence for repeated settings
  (verified: OpenSSH resolves each generated record exactly like the
  original command).
- One-off flags (`-v`, `-N`, `-f`, …) and remote commands are reported, not
  written; `-o` values are checked with the same rules as `lint`.

## 0.2.0 — 2026-11-14

Forwarding support.

- New `sshcm forward list | add | rm` for every forwarding shape OpenSSH
  supports: local, remote, dynamic (SOCKS), reverse dynamic, and Unix
  sockets, with bind addresses and IPv6. Specs use `ssh -L/-R/-D` syntax
  and are written in config syntax; `forward rm` removes one tunnel by its
  listen side.
- `sshcm add` accepts `-L`, `-R`, and `-D`.
- `sshcm list` has a FORWARDS column (up to two forwards, `+N`, and `A`/`X`
  for agent and X11 forwarding); `--json` includes structured forwards.
- `lint` validates every forward exactly as ssh does (E002) and warns when
  two forwards for a host listen on the same endpoint (W007).

## 0.1.0 — 2026-11-14

First release.

- Lossless parser: rendering an unmodified file reproduces it byte for byte.
- `list`, `show`, `resolve` (first-match-wins, `Match` subset, `Include`).
- `add`, `set`, `unset`, `rm`, `rename`, `cp` with diff, confirmation,
  `ssh -G` validation before writing, atomic writes, backups, and locking.
- `Include` support: globs, `~`, `%d`, `%u`, `${VAR}`, conditional includes;
  edits go to the file that defines the alias.
- `lint` (E001–E005, W001–W006), with every error verified against `ssh -G`.
- `fmt`, `export`/`import` (JSON), `backup list|restore`, shell completions,
  man page.
- Prebuilt binaries for Linux (static musl) and macOS, x86_64 and aarch64;
  CI tests on Linux (x86_64, aarch64) and macOS (aarch64).
