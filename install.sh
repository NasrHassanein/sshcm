#!/bin/sh
# Installer for sshcm (https://github.com/NasrHassanein/sshcm).
#
#   curl -fsSL https://github.com/NasrHassanein/sshcm/releases/latest/download/install.sh | sh -s -- --shell zsh
#
# Installs the sshcm binary into ~/.local/bin, makes sure that directory is on
# PATH in your shell's startup file (~/.bashrc, ~/.zshrc, or a fish conf.d
# snippet), and installs shell completion and the man page. Run from inside an
# extracted release archive, it installs the files next to it without network
# access. Everything it adds to a startup file sits between two marker lines,
# so re-running it replaces that block and --uninstall removes exactly it.

set -eu

REPO="NasrHassanein/sshcm"
MARK_START="# >>> sshcm installer >>>"
MARK_END="# <<< sshcm installer <<<"

usage() {
    cat <<'EOF'
Install sshcm into ~/.local/bin, with shell completion and PATH setup.

Usage: install.sh [OPTIONS]

Options:
  --version TAG      Install this release (e.g. v0.4.0) instead of the latest
  --shell SHELL      Set up bash, zsh or fish (default: from $SHELL)
  --no-modify-path   Don't edit shell startup files; print what to add instead
  --uninstall        Remove what this script installed
  -h, --help         Show this help

Environment:
  SSHCM_BIN_DIR      Install directory (default: ~/.local/bin)
  SSHCM_VERSION      Same as --version
EOF
}

# Messages show the home directory as ~ to keep paths short.
tilde() {
    rest="$*"
    out=""
    if [ -n "${HOME:-}" ] && [ "$HOME" != / ]; then
        while :; do
            case "$rest" in
                *"$HOME"*)
                    out="$out${rest%%"$HOME"*}~"
                    rest="${rest#*"$HOME"}"
                    ;;
                *) break ;;
            esac
        done
    fi
    printf '%s' "$out$rest"
}
say() { printf '==> %s\n' "$(tilde "$*")"; }
note() { printf '    %s\n' "$(tilde "$*")"; }
die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}
have() { command -v "$1" >/dev/null 2>&1; }

# --- arguments ----------------------------------------------------------------

version="${SSHCM_VERSION:-}"
shell_name=""
modify_path=1
uninstall=0
while [ $# -gt 0 ]; do
    case "$1" in
        --version) [ $# -ge 2 ] || die "--version needs a value"; version="$2"; shift ;;
        --version=*) version="${1#*=}" ;;
        --shell) [ $# -ge 2 ] || die "--shell needs a value"; shell_name="$2"; shift ;;
        --shell=*) shell_name="${1#*=}" ;;
        --no-modify-path) modify_path=0 ;;
        --uninstall) uninstall=1 ;;
        -h | --help) usage; exit 0 ;;
        *) usage >&2; die "unknown option: $1" ;;
    esac
    shift
done

[ -n "${HOME:-}" ] || die "HOME is not set"
if [ -n "$version" ]; then
    case "$version" in v*) ;; *) version="v$version" ;; esac
fi

BIN_DIR="${SSHCM_BIN_DIR:-$HOME/.local/bin}"
DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}"
BASH_COMPLETION="$DATA_DIR/bash-completion/completions/sshcm"
ZSH_FUNCTIONS="$DATA_DIR/zsh/site-functions"
ZSH_COMPLETION="$ZSH_FUNCTIONS/_sshcm"
FISH_COMPLETION="$CONFIG_DIR/fish/completions/sshcm.fish"
FISH_CONF="$CONFIG_DIR/fish/conf.d/sshcm.fish"
MAN_PAGE="$DATA_DIR/man/man1/sshcm.1"
BASHRC="$HOME/.bashrc"
ZSHRC="${ZDOTDIR:-$HOME}/.zshrc"

# Spell a path the way a startup file should: "$HOME/..." rather than the
# expanded home directory, so the file keeps working if it is shared.
portable() {
    case "$1" in
        "$HOME"/*) printf '$HOME/%s' "${1#"$HOME"/}" ;;
        *) printf '%s' "$1" ;;
    esac
}

tmp_root=""
cleanup() { [ -z "$tmp_root" ] || rm -rf "$tmp_root"; }
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# --- startup-file blocks --------------------------------------------------------

# Print FILE without our marked block (and the blank line written before it).
without_block() {
    awk -v s="$MARK_START" -v e="$MARK_END" '
        $0 == s { skip = 1; held = 0; next }
        skip { if ($0 == e) skip = 0; next }
        held { print ""; held = 0 }
        $0 == "" { held = 1; next }
        { print }
        END { if (held) print "" }
    ' "$1"
}

# Replace FILE's contents in place. Writing through the existing file (rather
# than renaming a new one over it) keeps its permissions, owner, and any
# symlink, e.g. a ~/.zshrc that points into a dotfiles repository.
overwrite() {
    file="$1"
    content="$2"
    cat "$content" >"$file"
    rm -f "$content"
}

remove_block() {
    file="$1"
    [ -f "$file" ] || return 0
    grep -qxF "$MARK_START" "$file" || return 0
    t="$(mktemp)"
    without_block "$file" >"$t"
    overwrite "$file" "$t"
    say "Removed the sshcm block from $file"
}

# Set FILE's marked block to the lines on stdin (removing it if empty).
write_block() {
    file="$1"
    body="$(cat)"
    if [ -z "$body" ]; then
        remove_block "$file"
        return 0
    fi
    mkdir -p "$(dirname "$file")"
    t="$(mktemp)"
    if [ -f "$file" ]; then
        without_block "$file" >"$t"
    fi
    # Keep the user's last line intact and separate our block with a blank line.
    if [ -s "$t" ]; then
        [ -z "$(tail -c 1 "$t")" ] || printf '\n' >>"$t"
        printf '\n' >>"$t"
    fi
    printf '%s\n%s\n%s\n' "$MARK_START" "$body" "$MARK_END" >>"$t"
    if [ -f "$file" ] && cmp -s "$t" "$file"; then
        rm -f "$t"
        say "$file is already set up"
    else
        [ -f "$file" ] || : >"$file"
        overwrite "$file" "$t"
        say "Updated $file"
    fi
}

# Does FILE (outside our block, ignoring comments) already put BIN_DIR on PATH?
mentions_bin_dir() {
    file="$1"
    [ -f "$file" ] || return 1
    rel="${BIN_DIR#"$HOME"/}"
    without_block "$file" | grep -v '^[[:space:]]*#' | grep -qF "$rel"
}

# --- uninstall ------------------------------------------------------------------

if [ "$uninstall" = 1 ]; then
    for f in "$BIN_DIR/sshcm" "$BASH_COMPLETION" "$ZSH_COMPLETION" "$FISH_COMPLETION" "$FISH_CONF" "$MAN_PAGE"; do
        if [ -e "$f" ]; then
            rm -f "$f"
            say "Removed $f"
        fi
    done
    remove_block "$BASHRC"
    remove_block "$ZSHRC"
    say "sshcm is uninstalled. Your ~/.ssh/config and its backups were left alone."
    exit 0
fi

# --- choose the shell ----------------------------------------------------------

if [ -z "$shell_name" ]; then
    shell_name="$(basename "${SHELL:-sh}")"
fi
case "$shell_name" in
    bash | zsh | fish) ;;
    *)
        note "Shell '$shell_name' is not one of bash, zsh, fish: installing the binary only."
        note "Re-run with --shell bash|zsh|fish to set up PATH and completion."
        shell_name=""
        ;;
esac

# --- find the files to install ---------------------------------------------------

# Local mode: this script sits in an extracted release archive (and no other
# version was asked for). When piped from curl, $0 is the shell, not a file.
src=""
script_dir=""
case "$0" in
    */*) script_dir="$(cd "$(dirname "$0")" && pwd)" ;;
    *) [ -f "$0" ] && script_dir="$(pwd)" ;;
esac
if [ -z "$version" ] && [ -n "$script_dir" ] &&
    [ -x "$script_dir/sshcm" ] && [ -d "$script_dir/completions" ]; then
    src="$script_dir"
    say "Installing from $src"
fi

download() {
    url="$1"
    out="$2"
    if have curl; then
        curl -fsSL --proto '=https' --tlsv1.2 --retry 3 -o "$out" "$url"
    elif have wget; then
        wget -q --https-only -O "$out" "$url"
    else
        die "need curl or wget to download sshcm"
    fi
}

latest_tag() {
    url="https://github.com/$REPO/releases/latest"
    if have curl; then
        final="$(curl -fsSLI --proto '=https' -o /dev/null -w '%{url_effective}' "$url")"
    elif have wget; then
        final="$(wget -q -S --spider "$url" 2>&1 | sed -n 's/^ *[Ll]ocation: *//p' | tail -n 1)"
    else
        die "need curl or wget to download sshcm"
    fi
    final="$(printf '%s' "$final" | tr -d '\r')"
    printf '%s' "${final##*/}"
}

detect_target() {
    os="$(uname -s)"
    arch="$(uname -m)"
    case "$arch" in
        x86_64 | amd64) arch=x86_64 ;;
        aarch64 | arm64) arch=aarch64 ;;
        *) die "no sshcm build for CPU '$arch' (available: x86_64, aarch64)" ;;
    esac
    case "$os" in
        Linux) printf '%s-unknown-linux-musl' "$arch" ;;
        Darwin)
            # A shell running under Rosetta reports x86_64 on Apple silicon.
            if [ "$arch" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = 1 ]; then
                arch=aarch64
            fi
            printf '%s-apple-darwin' "$arch"
            ;;
        *) die "no sshcm build for '$os' (available: Linux, macOS)" ;;
    esac
}

verify_checksum() {
    sum_file="$1"
    (
        cd "$(dirname "$sum_file")"
        if have sha256sum; then
            sha256sum -c "$(basename "$sum_file")" >/dev/null
        elif have shasum; then
            shasum -a 256 -c "$(basename "$sum_file")" >/dev/null
        else
            die "need sha256sum or shasum to verify the download"
        fi
    ) || die "checksum mismatch for $(basename "$sum_file" .sha256); not installing"
}

if [ -z "$src" ]; then
    target="$(detect_target)"
    if [ -z "$version" ]; then
        version="$(latest_tag)" || die "could not find the latest sshcm release"
        case "$version" in v*) ;; *) die "could not find the latest sshcm release" ;; esac
    fi
    name="sshcm-$version-$target"
    base="https://github.com/$REPO/releases/download/$version"
    tmp_root="$(mktemp -d)"
    say "Downloading sshcm $version for $target"
    download "$base/$name.tar.gz" "$tmp_root/$name.tar.gz" ||
        die "download failed: $base/$name.tar.gz"
    download "$base/$name.tar.gz.sha256" "$tmp_root/$name.tar.gz.sha256" ||
        die "download failed: $base/$name.tar.gz.sha256"
    verify_checksum "$tmp_root/$name.tar.gz.sha256"
    say "Checksum verified"
    tar xzf "$tmp_root/$name.tar.gz" -C "$tmp_root"
    src="$tmp_root/$name"
    [ -x "$src/sshcm" ] || die "archive has no sshcm binary"
fi

# --- install ----------------------------------------------------------------------

# Copy SRC to DEST via a temporary name and a rename, so a running sshcm (or a
# half-written file) is never seen at DEST.
put() {
    mkdir -p "$(dirname "$2")"
    cp "$1" "$2.tmp.$$"
    chmod "$3" "$2.tmp.$$"
    mv -f "$2.tmp.$$" "$2"
}

put "$src/sshcm" "$BIN_DIR/sshcm" 755
installed="$("$BIN_DIR/sshcm" --version)" || die "the installed binary does not run: $BIN_DIR/sshcm"
say "Installed $installed to $BIN_DIR/sshcm"

if [ -f "$src/sshcm.1" ]; then
    put "$src/sshcm.1" "$MAN_PAGE" 644
    say "Installed the man page to $MAN_PAGE"
fi

# The file whose PATH setup decides whether we need to add BIN_DIR.
case "$shell_name" in
    bash) path_rc="$BASHRC" ;;
    zsh) path_rc="$ZSHRC" ;;
    fish) path_rc="$CONFIG_DIR/fish/config.fish" ;;
    *) path_rc="" ;;
esac
path_on_rc=0
[ -n "$path_rc" ] && mentions_bin_dir "$path_rc" && path_on_rc=1
path_line=""
bin_portable="$(portable "$BIN_DIR")"

case "$shell_name" in
    bash)
        put "$src/completions/sshcm.bash" "$BASH_COMPLETION" 644
        say "Installed bash completion to $BASH_COMPLETION"
        if [ "$path_on_rc" = 1 ]; then
            say "$BASHRC already adds $bin_portable to PATH"
        else
            path_line="export PATH=\"$bin_portable:\$PATH\""
        fi
        if [ "$modify_path" = 1 ]; then
            printf '%s' "$path_line" | write_block "$BASHRC"
        fi
        found=0
        for f in /usr/share/bash-completion/bash_completion /etc/bash_completion \
            /opt/homebrew/etc/profile.d/bash_completion.sh /usr/local/etc/profile.d/bash_completion.sh; do
            [ -f "$f" ] && found=1
        done
        [ "$found" = 1 ] || note "Completion needs the bash-completion package (e.g. dnf/apt/brew install bash-completion)."
        if [ "$(uname -s)" = Darwin ] && ! grep -qs 'bashrc' "$HOME/.bash_profile"; then
            note "macOS login shells read ~/.bash_profile; make it source ~/.bashrc."
        fi
        rc_file="$BASHRC"
        ;;
    zsh)
        put "$src/completions/sshcm.zsh" "$ZSH_COMPLETION" 644
        say "Installed zsh completion to $ZSH_COMPLETION"
        if [ "$path_on_rc" = 1 ]; then
            say "$ZSHRC already adds $bin_portable to PATH"
        else
            path_line="export PATH=\"$bin_portable:\$PATH\""
        fi
        # Works whether this lands before or after the user's own compinit.
        zsh_block="$(
            [ -z "$path_line" ] || printf '%s\n' "$path_line"
            printf 'fpath=("%s" $fpath)\n' "$(portable "$ZSH_FUNCTIONS")"
            printf '%s\n' 'autoload -Uz _sshcm'
            printf '%s\n' 'if (( $+functions[compdef] )); then compdef _sshcm sshcm; else autoload -Uz compinit && compinit; fi'
        )"
        if [ "$modify_path" = 1 ]; then
            printf '%s' "$zsh_block" | write_block "$ZSHRC"
        fi
        rc_file="$ZSHRC"
        ;;
    fish)
        put "$src/completions/sshcm.fish" "$FISH_COMPLETION" 644
        say "Installed fish completion to $FISH_COMPLETION"
        if [ "$path_on_rc" = 1 ]; then
            say "$path_rc already adds $bin_portable to PATH"
            rm -f "$FISH_CONF"
        else
            path_line="contains -- \"$bin_portable\" \$PATH; or set -gx PATH \"$bin_portable\" \$PATH"
            if [ "$modify_path" = 1 ]; then
                mkdir -p "$(dirname "$FISH_CONF")"
                printf '%s\n%s\n%s\n' "$MARK_START" "$path_line" "$MARK_END" >"$FISH_CONF"
                say "Wrote $FISH_CONF"
            fi
        fi
        rc_file="$FISH_CONF"
        ;;
    *) rc_file="" ;;
esac

# --- summary ------------------------------------------------------------------------

if [ "$modify_path" = 0 ] && [ -n "$shell_name" ]; then
    if [ "$shell_name" = zsh ]; then
        note "Not editing startup files. Add this to $ZSHRC:"
        printf '%s\n' "$zsh_block" | sed 's/^/        /'
    elif [ -n "$path_line" ]; then
        note "Not editing startup files. Add this to $rc_file:"
        note "    $path_line"
    fi
elif [ -z "$shell_name" ]; then
    case ":$PATH:" in
        *":$BIN_DIR:"*) ;;
        *) note "Add $BIN_DIR to your PATH to run sshcm." ;;
    esac
fi

case ":$PATH:" in
    *":$BIN_DIR:"*) say "Done. Run: sshcm --help" ;;
    *)
        if [ "$shell_name" = fish ]; then
            say "Done. Open a new terminal, then: sshcm --help"
        elif [ -n "$rc_file" ]; then
            say "Done. Open a new terminal (or run: . $rc_file), then: sshcm --help"
        else
            say "Done. Run: $BIN_DIR/sshcm --help"
        fi
        ;;
esac
