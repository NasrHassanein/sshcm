# sshcm

[![CI](https://github.com/NasrHassanein/sshcm/actions/workflows/ci.yml/badge.svg)](https://github.com/NasrHassanein/sshcm/actions/workflows/ci.yml)

Manage `~/.ssh/config` from the command line **without ever damaging it**.
Linux and macOS; x86_64 and aarch64.

```console
$ sshcm add db --hostname 10.0.0.5 --user deploy -o ProxyJump=bastion
--- ~/.ssh/config
+++ ~/.ssh/config
@@ -9,4 +9,9 @@
     Port 2222

+Host db
+    HostName 10.0.0.5
+    User deploy
+    ProxyJump bastion
+
 Host *
     ServerAliveInterval 60
Apply these changes to ~/.ssh/config? [y/N] y
Updated ~/.ssh/config (backup: ~/.ssh/.sshcm/backups/config.20261114T101500.123456Z.bak)
```

## Guarantees

- **Lossless.** Comments, blank lines, indentation, `Key=Value` vs
  `Key Value`, keyword case, CRLF line endings, and a missing final newline
  are preserved. Lines an edit does not need to touch are byte-for-byte
  unchanged; `add` followed by `rm` restores the original file exactly.
- **Validated before writing.** The edited file is checked with `ssh -G`
  *before* it replaces your config; if OpenSSH rejects it, nothing is
  written (exit code 5).
- **Atomic and backed up.** Writes go to a temporary file that is renamed
  into place, so a crash never leaves a half-written config. The previous
  version is saved to `.sshcm/backups/` next to the config (newest 20 kept),
  and `sshcm backup restore` brings it back.
- **Locked.** Two `sshcm` processes never interleave edits.
- **Faithful to OpenSSH.** Resolution, `Include`, and `lint` rules are
  verified against `ssh -G` by the test suite (OpenSSH 10.2).

## Install

### Download a binary

Every [release](https://github.com/NasrHassanein/sshcm/releases) has archives for:

| | x86_64 | aarch64 |
|---|---|---|
| Linux (static, any distro) | `x86_64-unknown-linux-musl` | `aarch64-unknown-linux-musl` |
| macOS | `x86_64-apple-darwin` | `aarch64-apple-darwin` (Apple silicon) |

Each archive contains the binary, shell completions, and a man page
(`sshcm.1`), with a `.sha256` checksum alongside. Builds of every commit to
`main` are also available as artifacts on the
[CI runs](https://github.com/NasrHassanein/sshcm/actions/workflows/ci.yml).

### From source

```sh
cargo install --git https://github.com/NasrHassanein/sshcm
sshcm completions bash > ~/.local/share/bash-completion/completions/sshcm   # or zsh, fish, …
```

Requires Rust 1.89+. `ssh` must be on `PATH` for validation (without it,
edits are written unvalidated, with a warning).

## Usage

| Command | What it does |
|---|---|
| `sshcm list [-p GLOB] [--json]` | Table of Host blocks (across included files) |
| `sshcm show ALIAS` | The block exactly as written, with file and line |
| `sshcm resolve HOST [--json \| --via-ssh]` | Effective settings for a host, with the line each came from |
| `sshcm add ALIAS [--hostname H] [-u USER] [-p PORT] [-i KEY] [-o K=V]… [--before A \| --after A]` | New block; goes before `Host *` by default so wildcard defaults keep working |
| `sshcm set ALIAS K=V… [--append]` | Change options in place (`--append` adds another `IdentityFile`, `LocalForward`, …) |
| `sshcm unset ALIAS KEY…` | Remove options |
| `sshcm rm ALIAS` | Remove a host and its comments; other aliases on the same `Host` line stay |
| `sshcm rename OLD NEW` / `sshcm cp SRC DST` | Rename or duplicate a block |
| `sshcm lint [--strict] [--json]` | Errors ssh would reject, and likely mistakes |
| `sshcm fmt [--check] [--indent N \| --tabs] [--all]` | Opt-in normalisation; the only command that rewrites style |
| `sshcm export` / `sshcm import FILE [--skip-existing]` | JSON interchange of Host blocks |
| `sshcm backup list` / `sshcm backup restore [latest \| N \| NAME]` | Manage backups (a restore is itself backed up) |
| `sshcm completions SHELL` | Shell completion script |

Every command that writes accepts `--dry-run` (show the diff only), `-y`
(no prompt; also implied when stdin is not a terminal), `--no-backup`, and
`--no-validate`. `-F PATH` (or `SSHCM_CONFIG`) selects another config file.

### Include

`list`, `show`, `resolve`, `lint`, and `export` follow `Include` exactly as
ssh does: relative paths are relative to `~/.ssh`, globs are read in sorted
order, and an `Include` inside a `Host`/`Match` block only applies when that
block does. Edits go to **the file that defines the alias** (`sshcm set work
Port=2200` rewrites `~/.ssh/conf.d/work.conf`); `add` and `import` write to
the main config.

### Lint rules

Every error is something `ssh` itself rejects.

| Code | | Meaning |
|---|---|---|
| E001 | error | Unknown keyword (honours `IgnoreUnknown`), with a "did you mean" hint |
| E002 | error | Missing, extra, or invalid value (`Port 0`, `Compression true`, `ConnectTimeout 5x`) |
| E003 | error | Line without a keyword, or an unterminated quote |
| E004 | error | Config writable by group/others (ssh refuses it) |
| E005 | error | `Include` problem: cycle, nesting > 16, unreadable or writable file |
| W001 | warning | Alias defined in more than one `Host` block |
| W002 | warning | Option that never applies because an earlier line (often `Host *`) sets it first |
| W003 | warning | `IdentityFile` missing, or readable by others (ssh ignores such keys) |
| W005 | warning | `Include` inside a `Host`/`Match` block |
| W006 | warning | macOS-only `UseKeychain` without `IgnoreUnknown` |

### Exit codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | Error (including `fmt --check` finding changes) |
| 2 | Invalid command-line usage |
| 3 | Alias or option not found |
| 4 | `lint` found errors (or warnings, with `--strict`) |
| 5 | ssh rejected the edited file; nothing was written |
| 6 | Another `sshcm` held the lock for too long |

## Limitations

- Windows is not supported: the safety guarantees rely on Unix file modes
  and atomic rename semantics, and the crate refuses to build elsewhere.
- `Match` criteria other than `all`, `host`, `originalhost`, `user`, and
  `localuser` (e.g. `exec`) cannot be evaluated offline; `resolve` warns and
  treats the block as not matching. `resolve --via-ssh` asks OpenSSH itself.
- When an alias is defined in several files, edits change only the one ssh
  reads first (a note names the others).
- `export`/`import` cover `Host` blocks; global options and `Match` blocks
  are left to the config file itself.

## Development

```sh
cargo test                                   # unit, property, snapshot, ssh -G parity, end-to-end
cargo clippy --all-targets -- -D warnings    # pedantic lints are on in Cargo.toml
cargo fmt --check
```

The library (`src/lib.rs`) holds all logic and does no terminal I/O; the
binary (`src/cli/`) is a thin front end. Start with `src/syntax/` (the
lossless parser) and `src/edit.rs` (how edits stay surgical).
