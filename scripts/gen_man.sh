#!/usr/bin/env bash
# Generate docs/man/man1/h5i.1 from the h5i CLI definition (clap_mangen).
#
# The man page is RENDERED OUTPUT, not hand-edited. It is derived from the clap
# command tree in src/lib.rs (subcommand + flag doc comments), so it never
# drifts from the actual CLI.
#
# ONE copy, under docs/, because docs/ is published verbatim: the file the site
# serves at https://h5i.dev/man/man1/h5i.1 is the file the repository has. This
# is the opposite of install.sh, which is duplicated into docs/ and diffed by
# CI — that one has to answer at exactly /install.sh, so a second copy is the
# price. A man page can answer at any URL, so it pays nothing. The man1/ subdir
# is kept so `MANPATH=$PWD/docs/man man h5i` works and packagers see the
# layout they expect. To update it, edit the doc comments on the clap
# `Commands` / `#[arg(...)]` definitions, then regenerate:
#
#     ./scripts/gen_man.sh
#
# The renderer is examples/gen_man.rs, not a subcommand: the page is build
# output, and putting a roff generator in the shipped binary to serve a file
# the site can publish is a cost every user pays for nobody. Readers get it
# with `curl -fsSL https://h5i.dev/man/man1/h5i.1` instead.
#
# (The narrative /manual/ page is separate: it renders from MANUAL.md via
#  scripts/gen_manual.py. The man page is the terse CLI reference; MANUAL.md is
#  the long-form guide the man page's SEE ALSO points at.)
set -euo pipefail
cd "$(dirname "$0")/.."

# `--locked`: the committed page has to be reproducible from the committed
# lockfile, which is the claim the CI freshness gate makes. Resolving a newer
# clap here could change the rendering and turn the gate into noise.
cargo run --quiet --locked --example gen_man > docs/man/man1/h5i.1

# From the rendered `.TH` line, so no second build of the binary is needed.
version="$(sed -n 's/^\.TH h5i 1  "h5i \(.*\)" *$/\1/p' docs/man/man1/h5i.1)"
lines="$(wc -l < docs/man/man1/h5i.1)"
echo "wrote docs/man/man1/h5i.1  (${lines} lines, h5i ${version})"

# Optional lint: warn (do not fail) if groff finds -Tascii issues.
if command -v groff >/dev/null 2>&1; then
  warns="$(groff -man -Tascii -ww docs/man/man1/h5i.1 2>&1 >/dev/null | wc -l)"
  echo "groff -Tascii warnings: ${warns}"
fi
