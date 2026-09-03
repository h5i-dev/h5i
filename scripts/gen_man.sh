#!/usr/bin/env bash
# Generate docs/man/man1/h5i.1 from the h5i CLI definition (clap_mangen).
set -euo pipefail
cd "$(dirname "$0")/.."

# `--locked`: the committed page has to be reproducible from the committed
# lockfile, which is the claim the CI freshness gate makes. Resolving a newer
# clap here could change the rendering and turn the gate into noise.
# `--release`: dev-profile builds are guarded off locally (see CLAUDE.md). CI
# has no such guard and no warm release cache, so it sets H5I_MAN_DEV_PROFILE=1.
if [ "${H5I_MAN_DEV_PROFILE:-0}" = 1 ]; then profile=(); else profile=(--release); fi
cargo run --quiet --locked "${profile[@]}" --example gen_man > docs/man/man1/h5i.1

# From the rendered `.TH` line, so no second build of the binary is needed.
version="$(sed -n 's/^\.TH h5i 1  "h5i \(.*\)" *$/\1/p' docs/man/man1/h5i.1)"
lines="$(wc -l < docs/man/man1/h5i.1)"
echo "wrote docs/man/man1/h5i.1  (${lines} lines, h5i ${version})"

# Optional lint: warn (do not fail) if groff finds -Tascii issues.
if command -v groff >/dev/null 2>&1; then
  warns="$(groff -man -Tascii -ww docs/man/man1/h5i.1 2>&1 >/dev/null | wc -l)"
  echo "groff -Tascii warnings: ${warns}"
fi
