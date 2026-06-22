#!/usr/bin/env bash
#
# team-run.sh — drive a full hands-off h5i team cycle:
#
#   individual implementation → peer review → improvement → neutral verdict
#
# It ties the pieces together on the host while the agents stay in their boxes:
#   launch agents (team-launch.sh)
#     → live-ingest submissions (h5i team sync) until every agent has submitted
#     → freeze (seal the independent first attempts)
#     → grant mutual review + send review prompts (team-review.sh)
#     → live-ingest until every agent has revised (re-submitted)
#     → verify each candidate with the neutral sandboxed verifier
#     → finalize (the verdict) and optionally apply the winner.
#
# `h5i team sync` is the keystone: it ingests each box's staged submissions and
# reviews WITHOUT the box exiting, so the round advances while the team Stop hook
# keeps boxes alive waiting for the next step — no relaunching.
#
# Prereq — install the team Stop hook once so boxes stay alive between turns:
#   h5i hook setup --write --team
#
# Usage:
#   scripts/team-run.sh [options] <team>
#
# Options:
#   --task <file>       Task to dispatch at launch (passed to team-launch.sh).
#   --verify-cmd <cmd>  Test command the neutral verifier runs, e.g.
#                       --verify-cmd "pytest -q". Without it the cycle stops
#                       after the improvement round and prints the judge steps.
#   --apply             Apply the winning patch after the verdict (default: judge
#                       only — finalize and print the verdict, you apply).
#   --no-launch         Don't launch boxes (assume they're already running).
#   --poll <secs>       Poll interval while waiting (default 15).
#   --timeout <secs>    Per-phase wait budget before proceeding anyway (default 1800).
#   -n, --dry-run       Print the plan; change nothing.
#   -h, --help          This help.
#
# Requires: h5i, jq (+ team-launch.sh / team-review.sh siblings).
set -euo pipefail

H5I="${H5I:-h5i}"
TASK=""
VERIFY_CMD=""
APPLY=0
NO_LAUNCH=0
POLL=15
TIMEOUT=1800
DRY=0
TEAM=""

die() { echo "team-run: $*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --task) TASK="${2:-}"; shift 2 ;;
    --verify-cmd) VERIFY_CMD="${2:-}"; shift 2 ;;
    --apply) APPLY=1; shift ;;
    --no-launch) NO_LAUNCH=1; shift ;;
    --poll) POLL="${2:-}"; shift 2 ;;
    --timeout) TIMEOUT="${2:-}"; shift 2 ;;
    -n|--dry-run) DRY=1; shift ;;
    -h|--help) awk 'NR>1 && /^#/{sub(/^# ?/,""); print; next} NR>1{exit}' "$0"; exit 0 ;;
    -*) die "unknown option: $1" ;;
    *) [ -z "$TEAM" ] && TEAM="$1" || die "unexpected argument: $1"; shift ;;
  esac
done

[ -n "$TEAM" ] || die "usage: team-run.sh [options] <team>"
command -v "$H5I" >/dev/null 2>&1 || die "h5i not found (set \$H5I)"
H5I="$(command -v "$H5I")"
command -v jq >/dev/null 2>&1 || die "jq is required"
[ -z "$TASK" ] || [ -f "$TASK" ] || die "task file not found: $TASK"
HERE="$(cd "$(dirname "$0")" && pwd)"

run() { if [ "$DRY" = 1 ]; then printf '  +'; printf ' %q' "$@"; printf '\n'; else "$@"; fi; }

status_json() { "$H5I" team status "$TEAM" --json 2>/dev/null; }
sync_now() {
  if [ "$DRY" = 1 ]; then echo "  + $H5I team sync $TEAM"; else "$H5I" team sync "$TEAM" >/dev/null 2>&1 || true; fi
}

# Agents that have NOT yet submitted anything (empty = all have submitted).
missing_submitters() {
  status_json | jq -r '
    [.run.submissions[].owner_agent] as $owners
    | .run.agents[].agent_id
    | select(($owners | index(.)) | not)'
}
# "agent<TAB>latest_submission_id" per agent — the revision fingerprint.
latest_map() { status_json | jq -r '.run.agents[] | "\(.agent_id)\t\(.latest_submission_id // "none")"'; }

# wait_until <label> <predicate-fn-name>: poll (ingesting each tick) until the
# predicate passes or the per-phase timeout elapses (then proceed anyway).
wait_until() {
  local label="$1" pred="$2" waited=0
  if [ "$DRY" = 1 ]; then echo "  (dry-run) would wait for: $label"; return 0; fi
  echo "→ waiting: $label (poll ${POLL}s, timeout ${TIMEOUT}s) ..."
  while true; do
    sync_now
    if "$pred"; then echo "  ✓ $label"; return 0; fi
    if [ "$waited" -ge "$TIMEOUT" ]; then echo "  ! timed out — proceeding with what's in"; return 1; fi
    sleep "$POLL"; waited=$((waited + POLL))
  done
}

# ── 0. Launch ────────────────────────────────────────────────────────────────
if [ "$NO_LAUNCH" = 0 ]; then
  launch="$HERE/team-launch.sh"
  [ -x "$launch" ] || die "team-launch.sh not found next to this script (or use --no-launch)"
  echo "launching agents for team '$TEAM' ..."
  if [ -n "$TASK" ]; then run env H5I="$H5I" "$launch" --task "$TASK" "$TEAM"
  else run env H5I="$H5I" "$launch" "$TEAM"; fi
fi

# ── 1. Implementation: wait until everyone has submitted ─────────────────────
pred_all_submitted() { [ -z "$(missing_submitters)" ]; }
wait_until "every agent has submitted a first attempt" pred_all_submitted || true

# ── 2. Freeze + open the peer-review round ───────────────────────────────────
SNAP="$(latest_map || true)"   # fingerprint before review, to detect revisions
review="$HERE/team-review.sh"
[ -x "$review" ] || die "team-review.sh not found next to this script"
echo "freezing + granting mutual review ..."
run env H5I="$H5I" "$review" "$TEAM"

# ── 3. Improvement: wait until every agent has revised (re-submitted) ────────
pred_all_revised() {
  local now; now="$(latest_map)"
  local a old new
  while IFS=$'\t' read -r a old; do
    [ -z "$a" ] && continue
    new="$(printf '%s' "$now" | awk -F'\t' -v a="$a" '$1==a{print $2}')"
    [ "$new" != "$old" ] || return 1
  done <<< "$SNAP"
  return 0
}
wait_until "every agent has revised after review" pred_all_revised || true
sync_now   # final drain before judging

# ── 4. The neutral judge ─────────────────────────────────────────────────────
if [ -z "$VERIFY_CMD" ]; then
  echo
  echo "implementation + peer review + improvement done. No --verify-cmd given, so"
  echo "run the neutral judge yourself:"
  echo "  for a in \$($H5I team status $TEAM --json | jq -r '.run.agents[].agent_id'); do"
  echo "    $H5I team verify $TEAM --agent \"\$a\" -- <test-cmd>; done"
  echo "  $H5I team finalize $TEAM   # then:  $H5I team apply $TEAM"
  exit 0
fi

echo "verifying each candidate with: $VERIFY_CMD"
# shellcheck disable=SC2206
VCMD=($VERIFY_CMD)
for a in $(status_json | jq -r '.run.agents[].agent_id'); do
  echo "  verify $a ..."
  run "$H5I" team verify "$TEAM" --agent "$a" -- "${VCMD[@]}"
done

echo "finalizing (neutral verdict) ..."
run "$H5I" team finalize "$TEAM"

if [ "$APPLY" = 1 ]; then
  echo "applying the winner ..."
  run "$H5I" team apply "$TEAM"
else
  echo "verdict recorded. Apply the winner with:  $H5I team apply $TEAM"
fi

# finalize/apply fan a TEAM_DONE signal into each inbox, releasing the boxes.
echo "done. $H5I team status $TEAM   ·   $H5I team compare $TEAM"
