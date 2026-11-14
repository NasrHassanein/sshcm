# Changelog

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
