#!/usr/bin/env bash
# Prove that the pinned boa fork is upstream's own v0.21.1 plus nothing but the
# icu pin relaxation.
#
# This is the whole security argument for patching an engine that runs untrusted
# page script: not "trust the fork", but "check it". The revision is pinned in
# the workspace manifest, so this checks exactly what the build uses.
set -euo pipefail

FORK_URL="https://github.com/h5i-dev/boa"
UPSTREAM_TAG="v0.21.1"

here="$(cd "$(dirname "$0")/.." && pwd)"
rev="$(grep -m1 'boa_engine = { git' "$here/Cargo.toml" | sed 's/.*rev = "\([0-9a-f]*\)".*/\1/')"
if [ -z "$rev" ]; then
  echo "::error::could not read the pinned boa revision out of Cargo.toml"
  exit 1
fi
echo "pinned revision: $rev"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

git clone --quiet --no-checkout "$FORK_URL" "$work/boa"
git -C "$work/boa" remote add upstream https://github.com/boa-dev/boa.git
git -C "$work/boa" fetch --quiet --tags upstream "$UPSTREAM_TAG"
git -C "$work/boa" fetch --quiet origin "$rev"

# One commit on top of the upstream tag, and no more.
behind_ahead="$(git -C "$work/boa" rev-list --left-right --count "$UPSTREAM_TAG...$rev")"
echo "commits (upstream-only / fork-only): $behind_ahead"
if [ "$behind_ahead" != "0	1" ]; then
  echo "::error::the fork is not exactly one commit ahead of $UPSTREAM_TAG"
  exit 1
fi

# And that commit touches one file, changing only version requirements.
files="$(git -C "$work/boa" diff --name-only "$UPSTREAM_TAG" "$rev")"
if [ "$files" != "Cargo.toml" ]; then
  echo "::error::the fork changes more than the workspace manifest:"
  echo "$files"
  exit 1
fi

changed="$(git -C "$work/boa" diff -U0 "$UPSTREAM_TAG" "$rev" -- Cargo.toml \
  | grep -E '^[-+][^-+]' || true)"
echo "--- the entire difference from upstream $UPSTREAM_TAG ---"
echo "$changed"

if printf '%s' "$changed" | grep -vqE 'icu_(normalizer|properties|segmenter) = \{ version = "(~2\.0\.0|2)"'; then
  echo "::error::the difference is not confined to the icu version requirements"
  exit 1
fi

echo
echo "the pinned fork is upstream $UPSTREAM_TAG with only the icu pins relaxed."
