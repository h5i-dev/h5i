#!/usr/bin/env bash
# Generate man/man1/h5i.1 from the h5i CLI definition (clap_mangen).
#
# The man page is RENDERED OUTPUT, not hand-edited. It is derived from the clap
# command tree in src/main.rs (subcommand + flag doc comments), so it never
# drifts from the actual CLI. To update it, edit the doc comments on the clap
# `Commands` / `#[arg(...)]` definitions, then regenerate:
#
#     ./scripts/gen_man.sh
#
# (The narrative /manual/ page is separate: it renders from MANUAL.md via
#  scripts/gen_manual.py. The man page is the terse CLI reference; MANUAL.md is
#  the long-form guide the man page's SEE ALSO points at.)
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --quiet --bin h5i
./target/debug/h5i man > man/man1/h5i.1

version="$(./target/debug/h5i --version | awk '{print $NF}')"
lines="$(wc -l < man/man1/h5i.1)"
echo "wrote man/man1/h5i.1  (${lines} lines, h5i ${version})"

# Optional lint: warn (do not fail) if groff finds -Tascii issues.
if command -v groff >/dev/null 2>&1; then
  warns="$(groff -man -Tascii -ww man/man1/h5i.1 2>&1 >/dev/null | wc -l)"
  echo "groff -Tascii warnings: ${warns}"
fi
