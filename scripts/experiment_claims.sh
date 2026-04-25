#!/usr/bin/env bash
# experiment_claims.sh — Measure whether `h5i claims` reduces input-token usage.
#
# Hypothesis:
#   When pre-verified claims are injected into the context prompt, Claude does
#   less re-exploration on a subsequent session and therefore consumes fewer
#   input tokens (fewer Read/Grep calls, smaller per-turn context).
#
# Method:
#   Four arms on an identical seeded codebase, identical user task:
#
#     CONTROL     — no claims recorded, no summaries; H5I_CLAIMS_FREQUENCY=off.
#                   Baseline: what the task costs with none of the h5i machinery.
#     TREATMENT   — 5 hand-curated claims pre-recorded; H5I_CLAIMS_FREQUENCY=off.
#                   Upper bound on savings: retrieval-only, no recording overhead,
#                   author already decided what was worth pinning.
#     AUTO_CLAIMS — no pre-recorded claims; H5I_CLAIMS_FREQUENCY=high.
#                   Realistic cost of letting the agent record claims during a
#                   normal session. Single-session — shows the ADD overhead
#                   without the future-session retrieval benefit, so this arm
#                   is an UPPER BOUND on the cost of defaulting frequency=high.
#     SUMMARIES   — 4 pre-cached blob-OID-keyed file summaries; no claims.
#                   The agent can fetch each file's orientation via
#                   h5i_summary_get(path) instead of doing a full Read. Tests
#                   whether summaries can substitute for Reads on a task that
#                   only needs orientation, not full content.
#
#   For each run we parse the Claude session JSONL, sum per-turn token usage,
#   count tool calls, and count `h5i claims add` invocations (Bash + MCP).
#   A three-column comparison table is printed at the end.
#
# What the AUTO_CLAIMS arm does and does NOT measure:
#   DOES measure    — cost of agent-initiated claim recording on this task.
#   DOES NOT measure — future-session retrieval benefit from those claims.
#   The honest read: if AUTO_CLAIMS tokens ≈ CONTROL + small δ, the overhead is
#   cheap and the default=high bet only needs ~δ of retrieval savings in some
#   later session to break even. If AUTO_CLAIMS ≫ CONTROL, default=high has a
#   high hill to climb.
#
# Rigor built in:
#   · Per-trial wall-clock timeout (TRIAL_TIMEOUT) so a stalled claude run
#     doesn't hang the experiment.
#   · Retry-and-cap (RETRY_CAP): a trial that times out, writes to the wrong
#     files, or fails the ENTER/EXIT log-pair check is retried in a fresh
#     workdir; failures and retry counts are recorded and reported.
#   · Cyclic 3-arm order per trial (Latin-square-ish rotation) to mitigate
#     serial drift from Anthropic-side caches or backend state.
#   · MCP server mounted via --mcp-config so the agent can reach `h5i_claims_*`
#     tools natively; without this, the AUTO_CLAIMS arm would be artificially
#     discouraged from recording because it could only use the Bash form.
#   · The aggregator reports mean ± stdev [min, max] per arm and flags any
#     metric where 2·stdev ≥ |Δ| as noise-dominated.
#   · The model ID is extracted from each session JSONL and printed — so a
#     mid-experiment backend rollover is visible, not hidden in the variance.
#
# Caveat:
#   LLM outputs are stochastic. N_TRIALS=5 is the minimum for a meaningful
#   stdev; N=10 gives stable percentiles. Results are still observational,
#   not statistically significant by any formal test, but consistent deltas
#   across trials with low variance suggest the mechanism works.
#
# Usage:
#   ./scripts/experiment_claims.sh
#   N_TRIALS=10 ./scripts/experiment_claims.sh
#
# Environment variables:
#   H5I_BIN        — h5i binary path                    (default: h5i)
#   N_TRIALS       — trials per arm                     (default: 5)
#   TRIAL_TIMEOUT  — per-trial claude wall-clock cap    (default: 180 sec)
#   RETRY_CAP      — retries per failed trial           (default: 1)
#   WORKDIR_BASE   — temp workdir prefix                (default: /tmp/h5i-claims-exp-$$)
#
# Requirements:
#   h5i CLI, claude CLI, git, python3, timeout(1)

set -euo pipefail

# ── Config ────────────────────────────────────────────────────────────────────
H5I="${H5I_BIN:-h5i}"
N_TRIALS="${N_TRIALS:-5}"
TRIAL_TIMEOUT="${TRIAL_TIMEOUT:-180}"
RETRY_CAP="${RETRY_CAP:-1}"
WORKDIR_BASE="${WORKDIR_BASE:-/tmp/h5i-claims-exp-$$}"

PASS="✔"
FAIL="✖"
STEP="▶"

command -v claude >/dev/null 2>&1 || {
  echo "$FAIL  claude CLI not found in PATH — this experiment needs real runs, not synthetic."
  exit 2
}
command -v "$H5I" >/dev/null 2>&1 || {
  echo "$FAIL  h5i CLI not found (tried: $H5I). Set H5I_BIN or add h5i to PATH."
  exit 2
}
command -v timeout >/dev/null 2>&1 || {
  echo "$FAIL  timeout(1) not found — cannot cap per-trial wall time. Install coreutils."
  exit 2
}

# ── The task (identical for both arms) ────────────────────────────────────────
# The agent must edit exactly the HTTP helpers and nothing else. The claims in
# TREATMENT reveal where the HTTP helpers live, so TREATMENT should skip the
# grep/read dance that CONTROL has to perform.
TASK="Add a structured logging call to the start and end of every function that \
makes an HTTP request in this project. Use the already-imported logger \
(\`log\`). Log entry as \`log.info(\"ENTER <func_name>\")\` and exit as \
\`log.info(\"EXIT <func_name>\")\`. Do NOT modify any function that does not \
make HTTP calls. When done, print a summary of which files you edited."

# ── Claims recorded for the TREATMENT arm (caveman-style) ─────────────────────
# Each line is: <text>|<path1>,<path2>,...
# Caveman style: drop articles/copulas/fluff, keep paths + identifiers + numbers
# exact. Each claim ≈30 tokens or fewer.
read -r -d '' CLAIMS_SPEC <<'SPEC' || true
HTTP only src/api/client.py: fetch_user, create_post, delete_post.|src/api/client.py
src/utils/format.py: format_date, truncate. Pure, no HTTP.|src/utils/format.py
src/utils/validate.py: validate_email, validate_id. Pure, no HTTP.|src/utils/validate.py
main.py wires helpers. No direct HTTP.|main.py
Logger `log` at top src/api/client.py via `from logging import getLogger; log = getLogger(__name__)`.|src/api/client.py
SPEC

# ── Summaries recorded for the SUMMARIES arm (caveman-style) ──────────────────
# Each line is: <path>|<summary text> (single-line per summary).
# Caveman style: ≤80 tokens. Keep paths, identifiers, types, signatures exact.
# Drop fluff like "this file contains" / "module that does" / "of any kind".
read -r -d '' SUMMARIES_SPEC <<'SPEC' || true
src/api/client.py|HTTP client. `requests` to BASE='https://api.example.com'. Exports: fetch_user(id)→dict (GET /users/<id>), create_post(title,body,author_id)→dict (POST /posts), delete_post(id)→bool (DELETE /posts/<id>). Logger `log` bound top via `getLogger(__name__)`. No retries. All 3 funcs make HTTP.
src/utils/format.py|Pure format helpers, no I/O. Exports: format_date(dt)→str (YYYY-MM-DD), truncate(s,n)→str. No HTTP.
src/utils/validate.py|Pure validation, no I/O. Exports: validate_email(s)→bool (regex), validate_id(x)→bool (positive int). No HTTP.
main.py|Entry point. Imports fetch_user/create_post/delete_post from src.api.client + format/validate helpers. demo(user_id): validate id, fetch_user, print. No direct HTTP — delegates to client.py.
SPEC

# ── Project seed ──────────────────────────────────────────────────────────────
seed_project() {
  local dir="$1"
  rm -rf "$dir"
  mkdir -p "$dir/src/api" "$dir/src/utils"
  git -C "$dir" init -q
  git -C "$dir" config user.email "claims-exp@h5i.dev"
  git -C "$dir" config user.name  "Claims Experiment"

  cat > "$dir/src/api/__init__.py" <<'PYEOF'
PYEOF

  cat > "$dir/src/api/client.py" <<'PYEOF'
"""HTTP client helpers for the example API."""
import requests
from logging import getLogger

log = getLogger(__name__)

BASE = "https://api.example.com"


def fetch_user(user_id: int) -> dict:
    resp = requests.get(f"{BASE}/users/{user_id}", timeout=5)
    resp.raise_for_status()
    return resp.json()


def create_post(title: str, body: str, author_id: int) -> dict:
    payload = {"title": title, "body": body, "authorId": author_id}
    resp = requests.post(f"{BASE}/posts", json=payload, timeout=5)
    resp.raise_for_status()
    return resp.json()


def delete_post(post_id: int) -> bool:
    resp = requests.delete(f"{BASE}/posts/{post_id}", timeout=5)
    return resp.status_code == 204
PYEOF

  cat > "$dir/src/utils/__init__.py" <<'PYEOF'
PYEOF

  cat > "$dir/src/utils/format.py" <<'PYEOF'
"""Pure formatting helpers — no I/O."""
from datetime import datetime


def format_date(dt: datetime) -> str:
    return dt.strftime("%Y-%m-%d")


def truncate(s: str, n: int) -> str:
    if len(s) <= n:
        return s
    return s[: n - 1] + "…"
PYEOF

  cat > "$dir/src/utils/validate.py" <<'PYEOF'
"""Pure validation helpers — no I/O."""
import re

_EMAIL = re.compile(r"^[^@\s]+@[^@\s]+\.[^@\s]+$")


def validate_email(s: str) -> bool:
    return bool(_EMAIL.match(s))


def validate_id(x: int) -> bool:
    return isinstance(x, int) and x > 0
PYEOF

  cat > "$dir/main.py" <<'PYEOF'
"""Entry point that wires helpers together."""
from src.api.client import fetch_user, create_post, delete_post
from src.utils.format import format_date, truncate
from src.utils.validate import validate_id


def demo(user_id: int) -> None:
    if not validate_id(user_id):
        raise ValueError("user_id must be a positive int")
    user = fetch_user(user_id)
    name = truncate(user.get("name", ""), 40)
    print(name)


if __name__ == "__main__":
    demo(1)
PYEOF

  git -C "$dir" add -A
  git -C "$dir" commit -q -m "seed: api client + utils"
}

# ── h5i init + per-arm pre-seeding ────────────────────────────────────────────
# ARM is one of: CONTROL, TREATMENT, AUTO_CLAIMS, SUMMARIES.
#   CONTROL     — no pre-seeded claims, no pre-seeded summaries.
#   TREATMENT   — seed the 5 CLAIMS_SPEC entries as live claims pinned to HEAD.
#   AUTO_CLAIMS — no pre-seeded artefacts (the agent will record claims itself).
#   SUMMARIES   — seed the 4 SUMMARIES_SPEC entries as blob-keyed file summaries.
prepare_arm() {
  local dir="$1" arm="$2"
  (cd "$dir" && "$H5I" init >/dev/null 2>&1) || true
  (cd "$dir" && "$H5I" context init --goal \
    "add logging to HTTP helpers; leave other functions untouched" >/dev/null 2>&1) || true

  if [[ "$arm" == "TREATMENT" ]]; then
    while IFS='|' read -r text paths; do
      [[ -z "$text" ]] && continue
      local args=()
      IFS=',' read -ra ps <<< "$paths"
      for p in "${ps[@]}"; do args+=(--path "$p"); done
      (cd "$dir" && "$H5I" claims add "$text" "${args[@]}" >/dev/null 2>&1) || {
        echo "  $FAIL  failed to record claim: $text"
      }
    done <<< "$CLAIMS_SPEC"
  fi

  if [[ "$arm" == "SUMMARIES" ]]; then
    while IFS='|' read -r path text; do
      [[ -z "$path" ]] && continue
      (cd "$dir" && "$H5I" summary set "$path" --text "$text" >/dev/null 2>&1) || {
        echo "  $FAIL  failed to record summary for: $path"
      }
    done <<< "$SUMMARIES_SPEC"
  fi
}

# Map an arm to the H5I_CLAIMS_FREQUENCY value the claude subprocess should see.
#   CONTROL     — off (baseline; no claim machinery active in-session).
#   TREATMENT   — off (pre-seeded claims are already present; prevent new ones
#                      from being recorded so the measurement stays pure).
#   AUTO_CLAIMS — high (agent is actively encouraged to record claims).
#   SUMMARIES   — off (we're measuring the summary mechanism alone, not claims).
freq_for_arm() {
  case "$1" in
    AUTO_CLAIMS) echo "high" ;;
    *)           echo "off"  ;;
  esac
}

# Write an ephemeral MCP-config JSON for the claude --print subprocess so the
# h5i server is actually mounted (not just whitelisted). Resolves H5I to an
# absolute path so tests always use the binary the caller asked for.
write_mcp_config() {
  local out="$1"
  python3 - "$H5I" "$out" <<'PYEOF'
import json, shutil, sys
h5i_bin, out_path = sys.argv[1], sys.argv[2]
resolved = shutil.which(h5i_bin) or h5i_bin
json.dump(
    {"mcpServers": {"h5i": {"command": resolved, "args": ["mcp"]}}},
    open(out_path, "w"),
    indent=2,
)
PYEOF
}

# ── Locate the session JSONL Claude wrote for this run ────────────────────────
# Claude encodes the workdir path by replacing both `/` and `_` with `-`.
# Missing the `_` substitution silently breaks arm matching — "AUTO_CLAIMS"
# lands in the "-AUTO-CLAIMS-" directory, not "-AUTO_CLAIMS-".
find_claude_jsonl() {
  local workdir="$1"
  local encoded
  encoded=$(python3 -c "
import sys
p = sys.argv[1].lstrip('/').replace('/', '-').replace('_', '-')
print(p)
" "$workdir")
  # Pick the newest JSONL — should be the one just written.
  ls -t "$HOME/.claude/projects/-${encoded}"/*.jsonl 2>/dev/null | head -1 || true
}

# ── Parse a session JSONL for token + tool-call totals + model ID ────────────
parse_session() {
  local jsonl="$1"
  python3 - "$jsonl" <<'PYEOF'
import json, re, sys
jsonl = sys.argv[1]
t = {
    "input_tokens": 0,
    "output_tokens": 0,
    "cache_read_tokens": 0,
    "cache_creation_tokens": 0,
    "read_calls": 0,
    "grep_calls": 0,
    "glob_calls": 0,
    "edit_calls": 0,
    "write_calls": 0,
    "bash_calls": 0,
    "claim_adds": 0,   # count of `h5i claims add` calls (Bash or MCP)
    "summary_gets": 0, # count of `h5i_summary_get` (or Bash `summary show`) calls
    "summary_sets": 0, # count of `h5i_summary_set` (or Bash `summary set`) calls
    "assistant_turns": 0,
    "model": "",
}
try:
    with open(jsonl) as f:
        for line in f:
            try:
                m = json.loads(line)
            except json.JSONDecodeError:
                continue
            if m.get("type") != "assistant":
                continue
            t["assistant_turns"] += 1
            msg = m.get("message", {}) or {}
            # Record the first model we see — subsequent turns should match.
            if not t["model"]:
                mdl = msg.get("model")
                if mdl:
                    t["model"] = str(mdl)
            u = msg.get("usage", {}) or {}
            t["input_tokens"]          += int(u.get("input_tokens", 0) or 0)
            t["output_tokens"]         += int(u.get("output_tokens", 0) or 0)
            t["cache_read_tokens"]     += int(u.get("cache_read_input_tokens", 0) or 0)
            t["cache_creation_tokens"] += int(u.get("cache_creation_input_tokens", 0) or 0)
            for block in msg.get("content", []) or []:
                if block.get("type") != "tool_use":
                    continue
                name = block.get("name", "")
                if   name == "Read":  t["read_calls"] += 1
                elif name == "Grep":  t["grep_calls"] += 1
                elif name == "Glob":  t["glob_calls"] += 1
                elif name == "Edit":  t["edit_calls"] += 1
                elif name == "Write": t["write_calls"] += 1
                elif name == "Bash":  t["bash_calls"] += 1
                # Count claim/summary tool invocations across both transports.
                inp = block.get("input") or {}
                if name == "Bash":
                    cmd = inp.get("command", "")
                    if re.search(r"\bh5i\s+claims\s+add\b", cmd):
                        t["claim_adds"] += 1
                    if re.search(r"\bh5i\s+summary\s+(show|list)\b", cmd):
                        t["summary_gets"] += 1
                    if re.search(r"\bh5i\s+summary\s+set\b", cmd):
                        t["summary_sets"] += 1
                else:
                    if "h5i_claims_add" in name:
                        t["claim_adds"] += 1
                    if "h5i_summary_get" in name or "h5i_summary_list" in name:
                        t["summary_gets"] += 1
                    if "h5i_summary_set" in name:
                        t["summary_sets"] += 1
except FileNotFoundError:
    pass
print(json.dumps(t))
PYEOF
}

# Return the root (seed) commit OID of the workdir — the first commit made
# by seed_project. All fidelity diffs are taken against this commit so they
# survive the agent running `h5i commit` during the session.
seed_oid() {
  git -C "$1" rev-list --max-parents=0 HEAD 2>/dev/null | head -1
}

# Snapshot all file paths that changed since the seed commit (working-tree
# view). `git diff <seed>` compares seed → working-tree so it catches both
# committed *and* uncommitted edits, regardless of whether the agent ran
# `h5i commit`. Any narrower form (e.g. `seed..HEAD` or working-tree-only)
# silently drops one or the other class of change.
files_changed_since_seed() {
  local dir="$1" seed
  seed="$(seed_oid "$dir")"
  [[ -z "$seed" ]] && return 0
  git -C "$dir" diff --name-only "$seed" 2>/dev/null
}

# ── Correctness check: did the agent add BOTH enter+exit logs for all 3 HTTP helpers?
# Counts the number of HTTP helpers (0..3) that have both an `ENTER <fname>` and
# an `EXIT <fname>` log.info line in the added (+) side of the diff. Accepts any
# quoting style (f-string, plain str, single/double quotes). `git diff <seed>`
# catches both committed and uncommitted edits.
count_correct_log_pairs() {
  local dir="$1" diff seed
  seed="$(seed_oid "$dir")"
  [[ -z "$seed" ]] && { echo 0; return; }
  diff=$(git -C "$dir" diff "$seed" -- src/api/client.py 2>/dev/null || true)
  local pairs=0 fn has_enter has_exit
  for fn in fetch_user create_post delete_post; do
    has_enter=$(echo "$diff" | grep -cE "^\+.*log\.info\(.*ENTER.*${fn}" || true)
    has_exit=$(echo  "$diff" | grep -cE "^\+.*log\.info\(.*EXIT.*${fn}"  || true)
    if [ "$has_enter" -ge 1 ] && [ "$has_exit" -ge 1 ]; then
      pairs=$((pairs + 1))
    fi
  done
  echo "$pairs"
}

# ── One attempt at running an arm (no retry). Emits a JSON record on stdout. ─
run_arm_once() {
  local arm="$1" trial="$2" dir="$3"
  local freq
  freq="$(freq_for_arm "$arm")"

  echo "── [$arm · trial $trial · freq=$freq] → $dir ──────────────────────" >&2
  seed_project "$dir"
  prepare_arm "$dir" "$arm"

  # Build the prompt with the per-arm frequency hint in effect so the agent
  # sees (and is nudged by) the right policy.
  local preamble
  preamble="$(cd "$dir" && H5I_CLAIMS_FREQUENCY="$freq" "$H5I" context prompt 2>/dev/null || true)"
  local known_facts_lines policy_lines
  known_facts_lines=$(echo "$preamble" | grep -c "^## Known facts" || true)
  policy_lines=$(echo "$preamble" | grep -c "Claims frequency:" || true)
  echo "$STEP  [$arm · $trial] preamble: known_facts=$known_facts_lines, policy_hint=$policy_lines" >&2

  local full_prompt
  full_prompt="$(printf '%s\n\n---\n\n%s\n' "$preamble" "$TASK")"

  # MCP config (same for every arm — the transport should not be confounded).
  local mcp_cfg="$dir/.h5i-mcp-config.json"
  write_mcp_config "$mcp_cfg"

  echo "$STEP  [$arm · $trial] running claude --print (timeout ${TRIAL_TIMEOUT}s)…" >&2
  local start_ts rc
  start_ts=$(date +%s)
  # timeout --kill-after sends SIGKILL 10s after initial SIGTERM in case claude
  # ignores the term signal. Exit 124 == timeout fired.
  set +e
  (cd "$dir" \
    && H5I_CLAIMS_FREQUENCY="$freq" printf '%s' "$full_prompt" \
    | H5I_CLAIMS_FREQUENCY="$freq" timeout --kill-after=10 "${TRIAL_TIMEOUT}" \
        claude --print \
          --mcp-config "$mcp_cfg" \
          --strict-mcp-config \
          --allowedTools "mcp__h5i__*,Read,Write,Edit,Bash,Grep,Glob" \
      >/dev/null 2>&1)
  rc=$?
  set -e
  local elapsed=$(( $(date +%s) - start_ts ))
  local timed_out=0
  if [ "$rc" -eq 124 ] || [ "$rc" -eq 137 ]; then
    timed_out=1
    echo "  $FAIL  [$arm · $trial] claude hit ${TRIAL_TIMEOUT}s timeout (exit $rc)" >&2
  fi

  local jsonl
  jsonl=$(find_claude_jsonl "$dir")
  local parsed
  if [[ -z "$jsonl" ]]; then
    echo "  $FAIL  [$arm · $trial] no Claude JSONL found under $dir" >&2
    parsed='{"input_tokens":0,"output_tokens":0,"cache_read_tokens":0,"cache_creation_tokens":0,"read_calls":0,"grep_calls":0,"glob_calls":0,"edit_calls":0,"write_calls":0,"bash_calls":0,"assistant_turns":0,"model":""}'
  else
    parsed=$(parse_session "$jsonl")
    echo "  session: $jsonl" >&2
    echo "  elapsed: ${elapsed}s" >&2
  fi

  # Correctness: both ENTER+EXIT logs for all 3 HTTP helpers.
  local correct_log_pairs
  correct_log_pairs=$(count_correct_log_pairs "$dir")
  # Fidelity (seed..HEAD so committed edits count): did the agent touch
  # client.py? Did it wrongly edit utils/format.py or utils/validate.py?
  local changed client_edited utils_edited_wrongly
  changed="$(files_changed_since_seed "$dir")"
  client_edited=$(echo "$changed" | grep -c "src/api/client.py" || true)
  utils_edited_wrongly=$(echo "$changed" \
    | grep -c -E "src/utils/(format|validate)\.py" || true)

  echo "  correctness: $correct_log_pairs/3 log pairs, client_edited=$client_edited, utils_wrongly=$utils_edited_wrongly" >&2

  # Emit record.
  python3 - "$arm" "$trial" "$elapsed" "$client_edited" "$utils_edited_wrongly" "$correct_log_pairs" "$timed_out" "$freq" "$parsed" <<'PYEOF'
import json, sys
arm, trial, elapsed, client_edited, utils_wrong, pairs, timed_out, freq, parsed = sys.argv[1:]
rec = json.loads(parsed)
rec.update({
    "arm": arm,
    "trial": int(trial),
    "elapsed_sec": int(elapsed),
    "client_edited": int(client_edited or 0),
    "utils_edited_wrongly": int(utils_wrong or 0),
    "correct_log_pairs": int(pairs or 0),
    "timed_out": bool(int(timed_out)),
    "claims_frequency": freq,
})
print(json.dumps(rec))
PYEOF
}

# ── Is this record a successful run? (all 3 log pairs, no utils touched, no timeout)
_is_successful_record() {
  python3 - "$1" 2>/dev/null <<'PYEOF'
import json, sys
try:
    r = json.loads(sys.argv[1])
except Exception:
    sys.exit(1)
ok = (r.get("correct_log_pairs", 0) == 3
      and r.get("utils_edited_wrongly", 0) == 0
      and not r.get("timed_out", False))
sys.exit(0 if ok else 1)
PYEOF
}

# ── Append `attempts` + `final_success` to a record.
_finalize_record() {
  python3 - "$1" "$2" "$3" <<'PYEOF'
import json, sys
r = json.loads(sys.argv[1])
r["attempts"] = int(sys.argv[2])
r["final_success"] = bool(int(sys.argv[3]))
print(json.dumps(r))
PYEOF
}

# ── Run one arm with retry-and-cap. Emits exactly one finalized JSON record. ─
run_arm() {
  local arm="$1" trial="$2"
  local attempt=0 final_success=0 record=""
  local max_attempts=$((RETRY_CAP + 1))
  local dir

  while [ "$attempt" -lt "$max_attempts" ]; do
    attempt=$((attempt + 1))
    dir="${WORKDIR_BASE}-${arm}-${trial}"
    [ "$attempt" -gt 1 ] && dir="${dir}-retry${attempt}"
    record=$(run_arm_once "$arm" "$trial" "$dir")
    if _is_successful_record "$record"; then
      final_success=1
      break
    fi
    if [ "$attempt" -lt "$max_attempts" ]; then
      echo "  $FAIL  [$arm · $trial] attempt $attempt not successful — retrying" >&2
    fi
  done

  _finalize_record "$record" "$attempt" "$final_success"
}

# ── Main loop ─────────────────────────────────────────────────────────────────
echo "══════════════════════════════════════════════════════════════════════════"
echo "  h5i claims — token-reduction experiment"
echo "  N_TRIALS=$N_TRIALS   TRIAL_TIMEOUT=${TRIAL_TIMEOUT}s   RETRY_CAP=$RETRY_CAP"
echo "  WORKDIR_BASE=$WORKDIR_BASE"
echo "══════════════════════════════════════════════════════════════════════════"

RESULTS_FILE="${WORKDIR_BASE}-results.jsonl"
: > "$RESULTS_FILE"

# Cyclic 4-arm rotation per trial to mitigate serial drift (Anthropic-side
# caches, backend load, model-state drift). Each trial cycles through all
# four arms with a rotating starting offset — Latin-square-ish.
ARMS=(CONTROL TREATMENT AUTO_CLAIMS SUMMARIES)
ARM_COUNT=${#ARMS[@]}
for i in $(seq 1 "$N_TRIALS"); do
  offset=$(( (i - 1) % ARM_COUNT ))
  for k in $(seq 0 $((ARM_COUNT - 1))); do
    idx=$(( (offset + k) % ARM_COUNT ))
    run_arm "${ARMS[$idx]}" "$i" >> "$RESULTS_FILE"
  done
done

# Filter out non-JSON lines (echoed status messages) so aggregation only sees records.
RESULTS_JSON_ONLY="${RESULTS_FILE}.filtered"
grep -E '^\{.*\}$' "$RESULTS_FILE" > "$RESULTS_JSON_ONLY" || true

# ── Aggregate + print comparison ──────────────────────────────────────────────
echo
echo "══════════════════════════════════════════════════════════════════════════"
echo "  RESULTS  ($N_TRIALS trial(s) per arm)"
echo "══════════════════════════════════════════════════════════════════════════"

python3 - "$RESULTS_JSON_ONLY" <<'PYEOF'
import json, sys, statistics as stats
path = sys.argv[1]

ARM_ORDER = ["CONTROL", "TREATMENT", "AUTO_CLAIMS", "SUMMARIES"]
arms = {a: [] for a in ARM_ORDER}
with open(path) as f:
    for line in f:
        line = line.strip()
        if not line:
            continue
        try:
            rec = json.loads(line)
        except json.JSONDecodeError:
            continue
        if rec.get("arm") in arms:
            arms[rec["arm"]].append(rec)

present = [a for a in ARM_ORDER if arms[a]]
if len(present) < 2:
    print("  ✖  need ≥2 arms with ≥1 record each to compare")
    sys.exit(1)

# Successful trials only — primary analysis uses these. We report them
# separately from "all trials" so the reader can see the failure rate.
succ = {a: [r for r in rs if r.get("final_success")] for a, rs in arms.items()}

def summarize(xs):
    if not xs:
        return dict(n=0, mean=0, sd=0, lo=0, hi=0)
    return dict(
        n=len(xs),
        mean=sum(xs) / len(xs),
        sd=(stats.stdev(xs) if len(xs) > 1 else 0.0),
        lo=min(xs),
        hi=max(xs),
    )

def fmt_num(x):
    return f"{x:,.1f}" if isinstance(x, float) and x != int(x) else f"{int(x):,}"

# ── Preamble: run health ────────────────────────────────────────────────────
print("  Run health:")
for a in ARM_ORDER:
    rs = arms[a]
    if not rs:
        print(f"    {a:11s}  (no trials recorded)")
        continue
    s_rs = succ[a]
    attempts = sum(r.get("attempts", 1) for r in rs)
    timed_out = sum(1 for r in rs if r.get("timed_out"))
    models = sorted({r.get("model", "") for r in rs if r.get("model")})
    freqs = sorted({r.get("claims_frequency", "?") for r in rs})
    print(f"    {a:11s}  trials: {len(rs)}   successful: {len(s_rs)}   "
          f"total attempts: {attempts}   timed out: {timed_out}   "
          f"freq: {','.join(freqs)}")
    if not models:
        print(f"                 model: (unknown — no model field in JSONL)")
    elif len(models) == 1:
        print(f"                 model: {models[0]}")
    else:
        print(f"                 model: MIXED across trials → {models}  ⚠")

# CONTROL is the shared baseline — require it.
if not succ.get("CONTROL"):
    print()
    print("  ✖  CONTROL has zero successful trials — cannot compute deltas")
    print("      (a successful trial = all 3 ENTER+EXIT log pairs, no utils edits, no timeout)")
    sys.exit(1)

# Flag cross-arm model drift. Compare the set of observed model IDs across
# every arm — any mismatch is a confound.
model_sets = {
    a: {r.get("model", "") for r in rs if r.get("model")}
    for a, rs in arms.items() if rs
}
unique_model_sets = {frozenset(s) for s in model_sets.values() if s}
if len(unique_model_sets) > 1:
    print()
    print(f"  ⚠  model IDs differ across arms — deltas may be confounded by")
    print(f"     Anthropic-side routing, not claims alone:")
    for a, s in model_sets.items():
        print(f"       {a:11s} → {sorted(s)}")

# ── Main table: one row per metric, one column per arm with successful data,
# plus pairwise Δ% vs CONTROL. The aim is to surface:
#   TREATMENT vs CONTROL    — retrieval savings from pre-curated claims
#   AUTO_CLAIMS vs CONTROL  — realistic cost of in-session claim recording
fields = [
    ("input_tokens",          "Input tokens"),
    ("output_tokens",         "Output tokens"),
    ("cache_read_tokens",     "Cache-read tokens"),
    ("cache_creation_tokens", "Cache-write tokens"),
    ("read_calls",            "Read tool calls"),
    ("grep_calls",            "Grep tool calls"),
    ("glob_calls",            "Glob tool calls"),
    ("edit_calls",            "Edit tool calls"),
    ("bash_calls",            "Bash tool calls"),
    ("claim_adds",            "Claim `add` calls"),
    ("summary_gets",          "Summary `get` calls"),
    ("summary_sets",          "Summary `set` calls"),
    ("assistant_turns",       "Assistant turns"),
    ("elapsed_sec",           "Wall time (sec)"),
]

# Only report arms that have successful trials.
report_arms = [a for a in ARM_ORDER if succ.get(a)]
print()
print(f"  Successful trials only: " + ", ".join(
    f"{len(succ[a])} {a}" for a in report_arms
))
print()

# Build the header dynamically (variable number of arm columns).
header = f"  {'metric':22s}"
for a in report_arms:
    header += f" {a + '  mean±sd [min..max]':>42s}"
# Delta columns: every non-CONTROL arm gets a Δ% column vs CONTROL.
for a in report_arms:
    if a == "CONTROL":
        continue
    header += f" {a[:8] + ' Δ%':>10s}"
print(header)
print("  " + "-" * (len(header) - 2))

def noise_flag(ctl_sd, arm_sd, delta):
    return "⚠" if 2 * max(ctl_sd, arm_sd) >= abs(delta) and abs(delta) > 0 else " "

for key, label in fields:
    stats_per_arm = {a: summarize([r.get(key, 0) for r in succ[a]]) for a in report_arms}
    row = f"  {label:22s}"
    for a in report_arms:
        s = stats_per_arm[a]
        cell = (f"{fmt_num(s['mean'])} ± {fmt_num(s['sd'])}  "
                f"[{fmt_num(s['lo'])}..{fmt_num(s['hi'])}]")
        row += f" {cell:>42s}"
    ctl = stats_per_arm["CONTROL"]
    for a in report_arms:
        if a == "CONTROL":
            continue
        s = stats_per_arm[a]
        delta = s['mean'] - ctl['mean']
        pct = (delta / ctl['mean'] * 100.0) if ctl['mean'] else 0.0
        flag = noise_flag(ctl['sd'], s['sd'], delta)
        row += f" {f'{pct:>+7.1f}% {flag}':>10s}"
    print(row)

# ── Fidelity summary ────────────────────────────────────────────────────────
print()
print("  Fidelity across ALL attempts (including retries):")
for arm_name in ARM_ORDER:
    rs = arms[arm_name]
    if not rs:
        continue
    all_pairs = [r.get("correct_log_pairs", 0) for r in rs]
    perfect = sum(1 for p in all_pairs if p == 3)
    utils_wrong = sum(1 for r in rs if r.get("utils_edited_wrongly", 0) > 0)
    timed_out = sum(1 for r in rs if r.get("timed_out"))
    print(f"    {arm_name:11s}  all-3-log-pairs: {perfect}/{len(rs)}   "
          f"wrong files: {utils_wrong}   timed out: {timed_out}")

# ── Headline verdict ─────────────────────────────────────────────────────────
c_cr = summarize([r.get("cache_read_tokens", 0) for r in succ["CONTROL"]])
print()
if c_cr['mean'] == 0:
    print("  ℹ  no cache-read token data — JSONL format may not include usage.")
else:
    def verdict(arm_label, arm_key, direction, explain_win, explain_loss):
        """direction='savings' → positive delta_pct means arm beat CONTROL.
           direction='overhead' → positive delta_pct means arm cost MORE."""
        if not succ.get(arm_key):
            return
        a = summarize([r.get("cache_read_tokens", 0) for r in succ[arm_key]])
        raw_delta = a['mean'] - c_cr['mean']  # arm - control
        # For "savings" we report |savings| = -raw_delta / control
        if direction == "savings":
            pct = -raw_delta / c_cr['mean'] * 100.0
        else:
            pct = raw_delta / c_cr['mean'] * 100.0
        noisy = 2 * max(c_cr['sd'], a['sd']) >= abs(raw_delta)
        verb = "fewer" if direction == "savings" else "extra"
        sign_label = explain_win if pct > 0 else explain_loss
        marker = ("~" if noisy and abs(pct) > 0 else ("✔" if pct > 0 else "✖"))
        print(f"  {marker}  {arm_label}: {pct:+.1f}% cache-read tokens vs CONTROL "
              f"({sign_label})"
              + ("  ⚠ within-arm stdev ≥ |Δ|" if noisy and abs(pct) > 0 else ""))

    verdict(
        "TREATMENT (pre-curated claims)", "TREATMENT",
        direction="savings",
        explain_win="retrieval savings from pre-seeded claims",
        explain_loss="pre-seeded claims did not help",
    )
    verdict(
        "AUTO_CLAIMS (freq=high, single-session)", "AUTO_CLAIMS",
        direction="overhead",
        explain_win="recording overhead — future sessions must recover this much",
        explain_loss="AUTO_CLAIMS actually cost less than CONTROL (surprising)",
    )
    verdict(
        "SUMMARIES (pre-cached file summaries)", "SUMMARIES",
        direction="savings",
        explain_win="orientation savings from blob-keyed file summaries",
        explain_loss="pre-cached summaries did not help",
    )

    # Break-even hint: if AUTO_CLAIMS costs X% more than CONTROL, and TREATMENT
    # (post-recording) saves Y%, then the knob pays off if the agent reaps
    # TREATMENT-like savings in any future session. Printed only when both arms
    # have data.
    if succ.get("TREATMENT") and succ.get("AUTO_CLAIMS"):
        t_cr = summarize([r.get("cache_read_tokens", 0) for r in succ["TREATMENT"]])
        a_cr = summarize([r.get("cache_read_tokens", 0) for r in succ["AUTO_CLAIMS"]])
        overhead_abs = a_cr['mean'] - c_cr['mean']
        savings_abs  = c_cr['mean'] - t_cr['mean']
        if savings_abs > 0:
            break_even = overhead_abs / savings_abs
            print()
            print(f"  Break-even estimate (rough): the AUTO_CLAIMS arm pays back its "
                  f"{overhead_abs:,.0f}-token overhead after "
                  f"~{break_even:.2f} future session(s) at TREATMENT-level savings.")
            if break_even < 1:
                print(f"    → default=high pays off within the same session (good sign)")
            elif break_even < 3:
                print(f"    → default=high pays off within a few sessions (plausible)")
            else:
                print(f"    → default=high needs many future sessions to pay back (risky)")

# ── Sample-size caveat ──────────────────────────────────────────────────────
print()
n_min = min(len(succ[a]) for a in report_arms)
if n_min < 5:
    print(f"  ℹ  small-sample caveat: only {n_min} successful trial(s) in the smallest arm.")
    print(f"      Run with N_TRIALS=10 for a more trustworthy stdev.")
elif n_min < 10:
    print(f"  ℹ  {n_min} successful trials per arm — decent, but percentiles are still noisy.")
    print(f"      N=10+ recommended for pitch-grade numbers.")
PYEOF

echo
echo "══════════════════════════════════════════════════════════════════════════"
echo "  Raw per-trial records:  $RESULTS_JSON_ONLY"
echo "  Workdirs preserved:     ${WORKDIR_BASE}-{CONTROL,TREATMENT,AUTO_CLAIMS,SUMMARIES}-<trial>"
echo "  Inspect a run:          cat <workdir>/.git/.h5i/claims/*.json"
echo "══════════════════════════════════════════════════════════════════════════"
echo
echo "$STEP  done."
