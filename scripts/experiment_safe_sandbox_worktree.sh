#!/usr/bin/env bash
# experiment_safe_sandbox_worktree.sh — Compare plain git worktree with h5i env.
#
# Hypothesis:
#   A plain git worktree is not a sandbox: an agent can write to the parent
#   checkout by path and corrupt reviewer state. `h5i env` is a sandbox-style
#   worktree: with an enforceable process tier, the same write outside $WORK is
#   denied and captured as env evidence.
#
# Usage:
#   ./scripts/experiment_safe_sandbox_worktree.sh
#
# Environment:
#   H5I_BIN — h5i binary path (default: h5i)
#   WORKDIR — temp root (default: /tmp/h5i-safe-worktree-$$)

set -uo pipefail

H5I="${H5I_BIN:-h5i}"
WORKDIR="${WORKDIR:-/tmp/h5i-safe-worktree-$$}"
REPO="$WORKDIR/repo"
PLAIN="$WORKDIR/plain-agent"

case "$H5I" in
  */*) H5I="$(cd "$(dirname "$H5I")" && pwd)/$(basename "$H5I")" ;;
esac

pass() { printf 'PASS: %s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
note() { printf 'NOTE: %s\n' "$*"; }
run() { printf '+ %s\n' "$*"; "$@"; }

rm -rf "$WORKDIR"
mkdir -p "$REPO"

run git init -b main "$REPO" >/dev/null
run git -C "$REPO" config user.name "h5i sandbox experiment"
run git -C "$REPO" config user.email "sandbox@h5i.test"
printf 'clean\n' > "$REPO/protected.txt"
run git -C "$REPO" add protected.txt
run git -C "$REPO" commit -m seed >/dev/null

printf '\n== Plain git worktree: parent checkout is writable ==\n'
run git -C "$REPO" worktree add -b plain-agent "$PLAIN" >/dev/null
(
  cd "$PLAIN" || exit 1
  printf 'corrupted-by-plain-worktree\n' > "$REPO/protected.txt"
)
if grep -qx 'corrupted-by-plain-worktree' "$REPO/protected.txt"; then
  pass "plain worktree command modified parent checkout: $REPO/protected.txt"
else
  fail "plain worktree did not modify parent checkout as expected"
fi

run git -C "$REPO" restore protected.txt
grep -qx 'clean' "$REPO/protected.txt" || fail "failed to reset protected.txt"

printf '\n== h5i env: outside-work write is denied by process sandbox ==\n'
if ! command -v "$H5I" >/dev/null 2>&1; then
  note "h5i binary not found: $H5I"
  note "set H5I_BIN=/path/to/h5i and rerun"
  exit 0
fi

printf '+ cd %s && %s init\n' "$REPO" "$H5I"
(cd "$REPO" && "$H5I" init >/dev/null)
if ! (cd "$REPO" && H5I_AGENT=experiment "$H5I" env create safe \
  --isolation process --audit all >/tmp/h5i-safe-worktree-create.out 2>&1
)
then
  note "process isolation is not available on this host; h5i env arm skipped"
  note "create output:"
  sed 's/^/  /' /tmp/h5i-safe-worktree-create.out
  exit 0
fi

set +e
(cd "$REPO" && H5I_AGENT=experiment "$H5I" env run safe -- \
  sh -c "printf 'corrupted-by-h5i-env\n' > '$REPO/protected.txt'" \
  >/tmp/h5i-safe-worktree-run.out 2>&1)
rc=$?
set -e

if grep -qx 'clean' "$REPO/protected.txt"; then
  pass "h5i env preserved parent checkout: $REPO/protected.txt"
else
  fail "h5i env allowed parent checkout mutation"
fi

if [ "$rc" -eq 0 ]; then
  fail "h5i env write command unexpectedly succeeded"
fi

pass "h5i env denied the jailbreak attempt (exit $rc)"
note "evidence is available with: cd $REPO && $H5I env status safe"
