#!/usr/bin/env bash
# Rewrite `Source:` paths in extracted Lean to be relative to the repository
# root. Charon writes them relative to the examples workspace, or absolute for
# crates outside it.
set -euo pipefail
file=$1 root=$2
sed -i -e "s|Source: '$root/|Source: '@|" -e "s|Source: '\([^@]\)|Source: 'examples/app/\1|" -e "s|Source: '@|Source: '|" "$file"
