#!/usr/bin/env bash
# Print the h5i-app projects of one CI shard, one directory per line.
#
# Usage: ci-shard.sh <index> <count> <dir>...
# Finds every h5i-app.toml under the dirs the way `--all` does (skipping
# target, .lake, .git and node_modules), sorts them, and keeps every
# <count>-th from <index>. The shards partition the projects, so a project
# added later is checked by exactly one shard without editing CI.
set -euo pipefail
index=${1:?usage: ci-shard.sh <index> <count> <dir>...}
count=${2:?usage: ci-shard.sh <index> <count> <dir>...}
shift 2
[ "$#" -gt 0 ] || { echo "error: no directories" >&2; exit 1; }
find "$@" \( -name target -o -name .lake -o -name .git -o -name node_modules \) -prune \
  -o -name h5i-app.toml -type f -print \
  | xargs -r -n1 dirname | LC_ALL=C sort \
  | awk -v i="$index" -v n="$count" '(NR - 1) % n == i'
