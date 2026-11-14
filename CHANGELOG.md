# Changelog

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
