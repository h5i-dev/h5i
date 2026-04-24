#!/usr/bin/env bash
# experiment_claims.sh — Measure whether `h5i claims` reduces input-token usage.
#
# Hypothesis:
#   When pre-verified claims are injected into the context prompt, Claude does
#   less re-exploration on a subsequent session and therefore consumes fewer
#   input tokens (fewer Read/Grep calls, smaller per-turn context).
#
# Method:
#   Two runs on an identical seeded codebase, identical user task:
#
#     CONTROL   — no claims recorded; `h5i context prompt` has no Known-facts section.
#     TREATMENT — a handful of claims about the repo recorded beforehand;
#                 `h5i context prompt` now prepends a `## Known facts` preamble.
#
#   For each run we parse the Claude session JSONL, sum per-turn token usage,
#   and count Read/Grep/Glob tool calls. A comparison table is printed at the
#   end.
#
# Caveat:
#   A single trial is noisy — LLM outputs vary. Use N_TRIALS=3+ to average
#   across independent runs. Results are observational, not statistically
#   robust, but they're directional: consistent deltas across trials suggest
#   the mechanism works.
#
# Usage:
#   ./scripts/experiment_claims.sh
#   N_TRIALS=3 ./scripts/experiment_claims.sh
#
# Environment variables:
#   H5I_BIN       — h5i binary path       (default: h5i)
#   N_TRIALS      — trials per arm        (default: 1)
#   WORKDIR_BASE  — temp workdir prefix   (default: /tmp/h5i-claims-exp-$$)
#
# Requirements:
#   h5i CLI, claude CLI, git, python3

set -euo pipefail

# ── Config ────────────────────────────────────────────────────────────────────
H5I="${H5I_BIN:-h5i}"
N_TRIALS="${N_TRIALS:-1}"
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

# ── The task (identical for both arms) ────────────────────────────────────────
# The agent must edit exactly the HTTP helpers and nothing else. The claims in
# TREATMENT reveal where the HTTP helpers live, so TREATMENT should skip the
# grep/read dance that CONTROL has to perform.
TASK="Add a structured logging call to the start and end of every function that \
makes an HTTP request in this project. Use the already-imported logger \
(\`log\`). Log entry as \`log.info(\"ENTER <func_name>\")\` and exit as \
\`log.info(\"EXIT <func_name>\")\`. Do NOT modify any function that does not \
make HTTP calls. When done, print a summary of which files you edited."

# ── Claims recorded for the TREATMENT arm ─────────────────────────────────────
# Each line is: <text>|<path1>,<path2>,...
read -r -d '' CLAIMS_SPEC <<'SPEC' || true
HTTP helpers live only in src/api/client.py. The three functions are fetch_user, create_post, and delete_post.|src/api/client.py
src/utils/format.py contains formatting helpers (format_date, truncate); neither makes HTTP calls.|src/utils/format.py
src/utils/validate.py contains validate_email and validate_id; neither makes HTTP calls.|src/utils/validate.py
main.py wires helpers together and does not itself make HTTP calls.|main.py
The logger is exposed as `log` via `from logging import getLogger; log = getLogger(__name__)` at the top of src/api/client.py.|src/api/client.py
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

# ── h5i init + (optionally) record claims ─────────────────────────────────────
prepare_arm() {
  local dir="$1" record_claims="$2"
  (cd "$dir" && "$H5I" init >/dev/null 2>&1) || true
  (cd "$dir" && "$H5I" context init --goal \
    "add logging to HTTP helpers; leave other functions untouched" >/dev/null 2>&1) || true

  if [[ "$record_claims" == "1" ]]; then
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
}

# ── Locate the session JSONL Claude wrote for this run ────────────────────────
find_claude_jsonl() {
  local workdir="$1"
  local encoded
  encoded=$(python3 -c "
import sys
p = sys.argv[1].lstrip('/').replace('/', '-')
print(p)
" "$workdir")
  # Pick the newest JSONL — should be the one just written.
  ls -t "$HOME/.claude/projects/-${encoded}"/*.jsonl 2>/dev/null | head -1 || true
}

# ── Parse a session JSONL for token + tool-call totals ────────────────────────
parse_session() {
  local jsonl="$1"
  python3 - "$jsonl" <<'PYEOF'
import json, sys
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
    "assistant_turns": 0,
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
except FileNotFoundError:
    pass
print(json.dumps(t))
PYEOF
}

# ── Run one arm: seed → prepare → claude --print → parse JSONL ────────────────
run_arm() {
  local arm="$1" trial="$2" record_claims="$3"
  local dir="${WORKDIR_BASE}-${arm}-${trial}"

  echo
  echo "── [$arm · trial $trial] seeding $dir ───────────────────────────────────"
  seed_project "$dir"
  prepare_arm "$dir" "$record_claims"

  # The preamble Claude actually sees — same command for both arms; its output
  # differs only by whether claims were recorded.
  local preamble
  preamble="$(cd "$dir" && "$H5I" context prompt 2>/dev/null || true)"
  local known_facts_lines
  known_facts_lines=$(echo "$preamble" | grep -c "^## Known facts" || true)

  echo "$STEP  [$arm · $trial] preamble has Known-facts section: $known_facts_lines"
  local full_prompt
  full_prompt="$(printf '%s\n\n---\n\n%s\n' "$preamble" "$TASK")"

  echo "$STEP  [$arm · $trial] running claude --print …"
  local start_ts
  start_ts=$(date +%s)
  (cd "$dir" && printf '%s' "$full_prompt" \
    | claude --print \
        --allowedTools "mcp__h5i__*,Read,Write,Edit,Bash,Grep,Glob" \
      >/dev/null 2>&1) || true
  local elapsed=$(( $(date +%s) - start_ts ))

  local jsonl
  jsonl=$(find_claude_jsonl "$dir")
  if [[ -z "$jsonl" ]]; then
    echo "  $FAIL  no Claude JSONL found under $dir — aborting this trial"
    echo "{}"
    return
  fi

  local parsed
  parsed=$(parse_session "$jsonl")
  echo "  session: $jsonl"
  echo "  elapsed: ${elapsed}s"

  # Quick edit verification: did Claude actually touch client.py and no others?
  local client_edited utils_edited_wrongly
  client_edited=$(git -C "$dir" diff --name-only | grep -c "src/api/client.py" || true)
  utils_edited_wrongly=$(git -C "$dir" diff --name-only \
    | grep -c -E "src/utils/(format|validate)\.py" || true)

  # Emit one JSON blob per arm/trial for aggregation.
  python3 - "$arm" "$trial" "$elapsed" "$client_edited" "$utils_edited_wrongly" "$parsed" <<'PYEOF'
import json, sys
arm, trial, elapsed, client_edited, utils_edited_wrongly, parsed = sys.argv[1:]
rec = json.loads(parsed)
rec.update({
    "arm": arm,
    "trial": int(trial),
    "elapsed_sec": int(elapsed),
    "client_edited": int(client_edited or 0),
    "utils_edited_wrongly": int(utils_edited_wrongly or 0),
})
print(json.dumps(rec))
PYEOF
}

# ── Main loop ─────────────────────────────────────────────────────────────────
echo "══════════════════════════════════════════════════════════════════════════"
echo "  h5i claims — token-reduction experiment"
echo "  N_TRIALS=$N_TRIALS   WORKDIR_BASE=$WORKDIR_BASE"
echo "══════════════════════════════════════════════════════════════════════════"

RESULTS_FILE="${WORKDIR_BASE}-results.jsonl"
: > "$RESULTS_FILE"

for i in $(seq 1 "$N_TRIALS"); do
  run_arm CONTROL   "$i" 0 | tee -a "$RESULTS_FILE" >/dev/null
  run_arm TREATMENT "$i" 1 | tee -a "$RESULTS_FILE" >/dev/null
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
arms = {"CONTROL": [], "TREATMENT": []}
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

def mean(xs): return sum(xs) / len(xs) if xs else 0.0

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
    ("assistant_turns",       "Assistant turns"),
    ("elapsed_sec",           "Wall time (sec)"),
]

if not arms["CONTROL"] or not arms["TREATMENT"]:
    print("  ✖  not enough trials recorded — both arms need ≥1 record")
    sys.exit(1)

print(f"  {'metric':28s}  {'CONTROL':>12s}  {'TREATMENT':>12s}  {'Δ':>10s}  {'Δ%':>7s}")
print(f"  {'-'*28}  {'-'*12}  {'-'*12}  {'-'*10}  {'-'*7}")
for key, label in fields:
    c = mean([r.get(key, 0) for r in arms["CONTROL"]])
    t = mean([r.get(key, 0) for r in arms["TREATMENT"]])
    d = t - c
    pct = (d / c * 100.0) if c else 0.0
    c_str = f"{c:>12.1f}" if isinstance(c, float) and c != int(c) else f"{int(c):>12d}"
    t_str = f"{t:>12.1f}" if isinstance(t, float) and t != int(t) else f"{int(t):>12d}"
    d_str = f"{d:>+10.1f}" if isinstance(d, float) and d != int(d) else f"{int(d):>+10d}"
    print(f"  {label:28s}  {c_str}  {t_str}  {d_str}  {pct:>+6.1f}%")

# Fidelity: did the agent do the task correctly?
print()
print("  Fidelity (did the agent touch the right files?):")
for arm_name, rs in arms.items():
    client_hits  = sum(r.get("client_edited", 0) > 0 for r in rs)
    utils_wrong  = sum(r.get("utils_edited_wrongly", 0) > 0 for r in rs)
    print(f"    {arm_name:10s}  client.py edited: {client_hits}/{len(rs)}   "
          f"utils wrongly edited: {utils_wrong}/{len(rs)}")

# Verdict headline on input tokens (the primary hypothesis).
c_in = mean([r.get("input_tokens", 0) for r in arms["CONTROL"]])
t_in = mean([r.get("input_tokens", 0) for r in arms["TREATMENT"]])
print()
if c_in == 0:
    print("  ℹ  no input-token data — JSONL format may not include usage.")
elif t_in < c_in:
    drop = (c_in - t_in) / c_in * 100.0
    print(f"  ✔  TREATMENT used {drop:.1f}% fewer input tokens on average.")
else:
    rise = (t_in - c_in) / c_in * 100.0
    print(f"  ✖  TREATMENT used {rise:.1f}% MORE input tokens — hypothesis not supported by this run.")

# Honest caveat on sample size.
print()
if len(arms["CONTROL"]) < 3:
    print("  ℹ  small-sample caveat: LLM output is stochastic. Run with")
    print("      N_TRIALS=3 (or more) to see whether the delta is consistent.")
PYEOF

echo
echo "══════════════════════════════════════════════════════════════════════════"
echo "  Raw per-trial records:  $RESULTS_JSON_ONLY"
echo "  Workdirs preserved:     ${WORKDIR_BASE}-{CONTROL,TREATMENT}-<trial>"
echo "  Inspect a run:          cat <workdir>/.git/.h5i/claims/*.json"
echo "══════════════════════════════════════════════════════════════════════════"
echo
echo "$STEP  done."
