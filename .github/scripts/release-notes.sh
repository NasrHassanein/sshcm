#!/usr/bin/env bash
# Print GitHub release notes for a version: its CHANGELOG.md section plus
# download/verify instructions. Fails if CHANGELOG.md has no such section,
# so a release can't be published without a changelog entry.
#
# Usage: .github/scripts/release-notes.sh v0.1.0   (or 0.1.0)
set -euo pipefail

tag="v${1#v}"
version="${tag#v}"
changelog="$(dirname "$0")/../../CHANGELOG.md"

# Lines after "## <version>" (exact version, optionally followed by " — date")
# up to the next "## " heading.
section="$(awk -v v="$version" '
    index($0, "## " v " ") == 1 || $0 == "## " v { found = 1; next }
    found && /^## / { exit }
    found { print }
' "$changelog")"

if [[ -z "${section//[[:space:]]/}" ]]; then
    echo "error: CHANGELOG.md has no '## $version' section" >&2
    exit 1
fi

# Trim leading/trailing blank lines from the section.
section="$(printf '%s\n' "$section" | sed -e '/./,$!d' | sed -e ':a' -e '/^\n*$/{$d;N;ba' -e '}')"

cat <<EOF
$section

## Download

| Platform | x86_64 | aarch64 |
|---|---|---|
| Linux (static, any distro) | \`sshcm-$tag-x86_64-unknown-linux-musl.tar.gz\` | \`sshcm-$tag-aarch64-unknown-linux-musl.tar.gz\` |
| macOS | \`sshcm-$tag-x86_64-apple-darwin.tar.gz\` | \`sshcm-$tag-aarch64-apple-darwin.tar.gz\` (Apple silicon) |

Each archive contains the \`sshcm\` binary, shell completions (bash, zsh,
fish, elvish, PowerShell), and a man page (\`sshcm.1\`).

\`\`\`sh
sha256sum -c sshcm-$tag-<target>.tar.gz.sha256      # macOS: shasum -a 256 -c …
tar xzf sshcm-$tag-<target>.tar.gz
install -m 755 sshcm-$tag-<target>/sshcm ~/.local/bin/
\`\`\`

Or build from source (Rust 1.89+): \`cargo install --git https://github.com/NasrHassanein/sshcm --tag $tag\`
EOF
