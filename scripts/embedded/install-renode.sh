#!/usr/bin/env bash
# Install the portable Linux release of Renode (https://github.com/renode/renode)
# under ~/.local/opt/renode, pinned by version and SHA-256. Idempotent.
#
#   scripts/embedded/install-renode.sh        # then: ~/.local/opt/renode/renode --version
#
# RENODE_PREFIX overrides the install directory.
set -euo pipefail

VERSION=1.17.0
SHA256=4ba7c68b59e2447f188ef4b4b112fcccd2802582c460beedceb63b84e5605a5f
URL="https://github.com/renode/renode/releases/download/v${VERSION}/renode-${VERSION}.linux-portable.tar.gz"
PREFIX="${RENODE_PREFIX:-$HOME/.local/opt/renode}"

if [ -x "$PREFIX/renode" ] && [ "$(cat "$PREFIX/.h5i-version" 2>/dev/null)" = "$VERSION" ]; then
    echo "renode $VERSION already installed in $PREFIX"
    exit 0
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
echo "downloading $URL"
curl -fsSL -o "$tmp/renode.tgz" "$URL"
echo "$SHA256  $tmp/renode.tgz" | sha256sum -c -
mkdir -p "$tmp/x"
tar xzf "$tmp/renode.tgz" -C "$tmp/x"
rm -rf "$PREFIX"
mkdir -p "$(dirname "$PREFIX")"
mv "$tmp/x/renode_${VERSION}-portable" "$PREFIX"
echo "$VERSION" > "$PREFIX/.h5i-version"
echo "installed renode $VERSION in $PREFIX"
