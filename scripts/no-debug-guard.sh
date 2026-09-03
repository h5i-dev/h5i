#!/usr/bin/env bash
# Make a dev-profile build of this workspace impossible, by parking a regular
# file where cargo wants to create the target/debug directory. cargo then stops
# with "failed to create directory ... File exists" before it compiles anything.
#
# A full dev-profile build of this workspace is very large, so a machine that
# cannot spare the space wants the checkout pinned to release-only. Unlike
# scripts/deny-debug-build.py, which only Claude Code obeys, this works against
# every tool: Codex, an editor, a stray shell, anything.
#
#   scripts/no-debug-guard.sh          # or `on`: install the guard
#   scripts/no-debug-guard.sh off      # maintainer escape hatch
#   scripts/no-debug-guard.sh status
set -euo pipefail
cd "$(dirname "$0")/.."

guard=target/debug

case "${1:-on}" in
  on)
    if [ -d "$guard" ]; then
      echo "target/debug exists as a directory ($(du -sh "$guard" | cut -f1))."
      echo "Remove it yourself first, then re-run: rm -rf $guard"
      exit 1
    fi
    mkdir -p target
    printf '%s\n' \
      "Not a directory, on purpose: this checkout is release-only." \
      "Everything here is built with --release. See CLAUDE.md." \
      "Maintainer escape hatch: scripts/no-debug-guard.sh off" > "$guard"
    echo "guard on: dev-profile cargo builds now fail immediately"
    ;;
  off)
    [ -f "$guard" ] && rm -f "$guard"
    echo "guard off: dev-profile builds are possible again"
    ;;
  status)
    if [ -f "$guard" ]; then echo "on"; elif [ -d "$guard" ]; then echo "off (target/debug is a real directory)"; else echo "off"; fi
    ;;
  *)
    echo "usage: $0 [on|off|status]" >&2; exit 2 ;;
esac
