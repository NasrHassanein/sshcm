#!/usr/bin/env bash
# Tests for install.sh. Every case runs against a throwaway HOME, so the real
# home directory is never touched.
#
# Usage: .github/scripts/test-install.sh [path/to/sshcm]
#   Without an argument, builds the binary with cargo.
#   SSHCM_TEST_OFFLINE=1 skips the case that downloads a real release.
#   SH selects the shell that runs install.sh (default: sh).
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
installer="$root/install.sh"
run_sh="${SH:-sh}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
failures=0
passes=0

pass() { passes=$((passes + 1)); printf '  ok   %s\n' "$*"; }
fail() { failures=$((failures + 1)); printf '  FAIL %s\n' "$*"; }
check() { # check DESCRIPTION COMMAND...
    local what="$1"
    shift
    if "$@" >/dev/null 2>&1; then pass "$what"; else fail "$what"; fi
}
count_blocks() { grep -cxF '# >>> sshcm installer >>>' "$1" 2>/dev/null || true; }

if command -v shellcheck >/dev/null; then
    echo "shellcheck"
    check "install.sh is shellcheck-clean (POSIX sh)" shellcheck -s sh "$installer"
    shellcheck -s sh "$installer" || true
else
    echo "shellcheck not installed; skipping"
fi

# --- a release-archive layout from a local build ----------------------------
bin="${1:-}"
if [[ -z "$bin" ]]; then
    cargo build --quiet --manifest-path "$root/Cargo.toml"
    bin="$root/target/debug/sshcm"
fi
archive="$work/sshcm-vTEST-local"
mkdir -p "$archive/completions"
cp "$bin" "$archive/sshcm"
cp "$installer" "$archive/install.sh"
for s in bash zsh fish; do "$bin" completions "$s" >"$archive/completions/sshcm.$s"; done
"$bin" manpage >"$archive/sshcm.1"

# Run the archived installer with a fresh environment: HOME=$1, a PATH
# without ~/.local/bin, and no XDG overrides.
new_home() { mktemp -d "$work/home.XXXXXX"; }
install_in() {
    local home="$1"
    shift
    env -i HOME="$home" PATH="/usr/local/bin:/usr/bin:/bin:/opt/homebrew/bin" SHELL=/bin/false \
        "$run_sh" "$archive/install.sh" "$@"
}

# --- per shell: fresh install, idempotent re-run, uninstall -----------------
for shell in bash zsh fish; do
    echo "$shell"
    h="$(new_home)"
    case "$shell" in
        bash) rc="$h/.bashrc" comp="$h/.local/share/bash-completion/completions/sshcm" ;;
        zsh) rc="$h/.zshrc" comp="$h/.local/share/zsh/site-functions/_sshcm" ;;
        fish) rc="$h/.config/fish/conf.d/sshcm.fish" comp="$h/.config/fish/completions/sshcm.fish" ;;
    esac
    if [[ "$shell" != fish ]]; then printf 'alias ll="ls -l"\n' >"$rc"; cp "$rc" "$work/orig"; fi

    check "$shell: install succeeds" install_in "$h" --shell "$shell"
    check "$shell: binary runs" "$h/.local/bin/sshcm" --version
    check "$shell: completion installed" test -s "$comp"
    check "$shell: man page installed" test -s "$h/.local/share/man/man1/sshcm.1"
    check "$shell: PATH line added" grep -qF '$HOME/.local/bin' "$rc"
    check "$shell: re-run succeeds" install_in "$h" --shell "$shell"
    check "$shell: exactly one block after two runs" test "$(count_blocks "$rc")" = 1

    case "$shell" in
        bash)
            check "bash: rc puts sshcm on PATH" env -i HOME="$h" PATH=/usr/bin:/bin bash -c ". \"$rc\"; command -v sshcm"
            ;;
        zsh)
            if command -v zsh >/dev/null; then
                check "zsh: completion function resolves" env -i HOME="$h" PATH=/usr/bin:/bin zsh -i -c 'whence -w _sshcm | grep -q function && command -v sshcm'
            fi
            ;;
        fish)
            if command -v fish >/dev/null; then
                check "fish: conf.d puts sshcm on PATH" env -i HOME="$h" PATH=/usr/bin:/bin fish -c 'command -v sshcm'
                check "fish: completes subcommands" sh -c "env -i HOME='$h' PATH=/usr/bin:/bin fish -c 'complete -C\"sshcm li\"' | grep -q '^list'"
            fi
            ;;
    esac

    check "$shell: uninstall succeeds" install_in "$h" --uninstall
    check "$shell: binary removed" test ! -e "$h/.local/bin/sshcm"
    check "$shell: completion removed" test ! -e "$comp"
    if [[ "$shell" == fish ]]; then
        check "fish: conf.d snippet removed" test ! -e "$rc"
    else
        check "$shell: rc restored byte for byte" cmp -s "$rc" "$work/orig"
    fi
done

# --- PATH already configured: the rc file is left alone -----------------------
echo "existing PATH setup"
h="$(new_home)"
printf 'export PATH="$HOME/.local/bin:$HOME/bin:$PATH"\n' >"$h/.bashrc"
cp "$h/.bashrc" "$work/orig"
check "bash: install succeeds" install_in "$h" --shell bash
check "bash: rc untouched when it already has ~/.local/bin" cmp -s "$h/.bashrc" "$work/orig"

h="$(new_home)"
printf 'path+=(~/.local/bin)\n' >"$h/.zshrc"
install_in "$h" --shell zsh >/dev/null
check "zsh: block has completion but no PATH line" \
    sh -c "! grep -q 'export PATH' '$h/.zshrc' && grep -q 'fpath=' '$h/.zshrc'"

h="$(new_home)"
mkdir -p "$h/.config/fish"
printf 'fish_add_path ~/.local/bin\n' >"$h/.config/fish/config.fish"
install_in "$h" --shell fish >/dev/null
check "fish: no conf.d snippet when config.fish has ~/.local/bin" test ! -e "$h/.config/fish/conf.d/sshcm.fish"

# --- --no-modify-path ------------------------------------------------------------
echo "--no-modify-path"
h="$(new_home)"
printf '# mine\n' >"$h/.bashrc"
out="$(install_in "$h" --shell bash --no-modify-path)"
check "rc untouched" test "$(cat "$h/.bashrc")" = "# mine"
check "prints the line to add" grep -qF 'export PATH="$HOME/.local/bin:$PATH"' <<<"$out"

# --- symlinked rc file, no trailing newline, missing rc ---------------------------
echo "rc file edge cases"
h="$(new_home)"
mkdir "$h/dotfiles"
printf 'echo hi\n' >"$h/dotfiles/zshrc"
ln -s dotfiles/zshrc "$h/.zshrc"
install_in "$h" --shell zsh >/dev/null
check "symlinked rc stays a symlink" test -L "$h/.zshrc"
check "the link target got the block" grep -qF '# >>> sshcm installer >>>' "$h/dotfiles/zshrc"
install_in "$h" --uninstall >/dev/null
check "symlink target restored" test "$(cat "$h/dotfiles/zshrc")" = "echo hi"

h="$(new_home)"
printf 'echo no-newline' >"$h/.bashrc"
install_in "$h" --shell bash >/dev/null
check "last line without newline is kept intact" grep -qx 'echo no-newline' "$h/.bashrc"

h="$(new_home)"
check "missing rc file is created" sh -c "env -i HOME='$h' PATH=/usr/bin:/bin $run_sh '$archive/install.sh' --shell bash && test -f '$h/.bashrc'"

h="$(new_home)"
check "unknown shell installs the binary only" sh -c "env -i HOME='$h' PATH=/usr/bin:/bin SHELL=/bin/tcsh $run_sh '$archive/install.sh' && test -x '$h/.local/bin/sshcm' && test ! -e '$h/.bashrc'"
check "unknown option is rejected" sh -c "! env -i HOME='$h' PATH=/usr/bin:/bin $run_sh '$archive/install.sh' --bogus"

# --- download mode against a real release -------------------------------------
if [[ "${SSHCM_TEST_OFFLINE:-0}" != 1 ]]; then
    echo "download (v0.3.0 from GitHub)"
    h="$(new_home)"
    # Piped, as in the README: no local archive next to the script.
    if env -i HOME="$h" PATH="/usr/local/bin:/usr/bin:/bin:/opt/homebrew/bin" \
        "$run_sh" -s -- --version v0.3.0 --shell bash <"$installer" >"$work/dl.log" 2>&1; then
        pass "piped install downloads and verifies v0.3.0"
    else
        fail "piped install downloads and verifies v0.3.0"
        cat "$work/dl.log"
    fi
    check "downloaded binary is 0.3.0" sh -c "'$h/.local/bin/sshcm' --version | grep -qx 'sshcm 0.3.0'"
    check "downloaded completion installed" test -s "$h/.local/share/bash-completion/completions/sshcm"
    check "nonexistent version fails" sh -c "! env -i HOME='$h' PATH=/usr/bin:/bin $run_sh -s -- --version v0.0.0 --shell bash <'$installer'"
fi

echo
echo "$passes passed, $failures failed"
[[ "$failures" == 0 ]]
