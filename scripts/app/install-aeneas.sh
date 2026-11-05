#!/usr/bin/env bash
# Install Charon and Aeneas at the pinned commit from Aeneas's own release
# (built by its CI from that commit), instead of building them with Nix
# (~19 minutes per job, and it hangs when the Actions cache service fails).
#
# Usage: install-aeneas.sh <dest-dir>
# Puts charon, charon-driver and aeneas in <dest-dir>, installs the nightly
# Rust toolchain Charon's driver links against (from the bundled
# rust-toolchain file), and prints the directory to add to PATH.
#
# The release tag must name AENEAS_REV's commit, and the archive must have
# the pinned SHA-256. Bump all three together (and pins.rs, the lakefiles);
# `extract --check` then shows whether the new tools reproduce the Lean.
set -euo pipefail
: "${AENEAS_REV:?set AENEAS_REV}"
dest=${1:?usage: install-aeneas.sh <dest-dir>}

RELEASE=nightly-2026.09.28-b86120d
SHA256=605bb2c4abcc7678642669ec8db94dc88fba0aa082b04d5fdafb0cc83fae56cd
ASSET=aeneas-linux-x86_64.tar.gz

case "$RELEASE" in
  *-"${AENEAS_REV:0:7}") ;;
  *) echo "error: release $RELEASE is not built from AENEAS_REV $AENEAS_REV" >&2; exit 1 ;;
esac

mkdir -p "$dest"
archive="$dest/$ASSET"
# A few retries with backoff for transient errors; no tight loops.
curl -fsSL --retry 3 --retry-delay 10 --retry-all-errors --max-time 600 \
  -o "$archive" "https://github.com/AeneasVerif/aeneas/releases/download/$RELEASE/$ASSET"
echo "$SHA256  $archive" | sha256sum -c - >&2
tar -xzf "$archive" -C "$dest" aeneas charon charon-driver rust-toolchain
rm -f "$archive"

# The toolchain Charon was built against: its channel and components.
channel=$(sed -n 's/^channel *= *"\(.*\)"/\1/p' "$dest/rust-toolchain")
[ -n "$channel" ] || { echo "error: no channel in $dest/rust-toolchain" >&2; exit 1; }
rustup toolchain install "$channel" --profile minimal \
  --component rustc-dev,llvm-tools,rust-src,miri >&2

"$dest/charon" version >&2
echo "$dest"
