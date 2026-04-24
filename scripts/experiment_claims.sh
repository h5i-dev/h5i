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
# Rigor built in:
#   · Per-trial wall-clock timeout (TRIAL_TIMEOUT) so a stalled claude run
#     doesn't hang the experiment.
#   · Retry-and-cap (RETRY_CAP): a trial that times out, writes to the wrong
#     files, or fails the ENTER/EXIT log-pair check is retried in a fresh
#     workdir; failures and retry counts are recorded and reported.
#   · Interleaved arm order per trial (odd: C→T, even: T→C) to mitigate
#     serial drift from Anthropic-side caches or backend state.
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

# ── Parse a session JSONL for token + tool-call totals + model ID ────────────
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
except FileNotFoundError:
    pass
print(json.dumps(t))
PYEOF
}

# ── Correctness check: did the agent add BOTH enter+exit logs for all 3 HTTP helpers?
# Counts the number of HTTP helpers (0..3) that have both an `ENTER <fname>` and
# an `EXIT <fname>` log.info line in the added (+) side of the diff. Accepts any
# quoting style (f-string, plain str, single/double quotes).
count_correct_log_pairs() {
  local dir="$1" diff
  diff=$(git -C "$dir" diff -- src/api/client.py 2>/dev/null || true)
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
  local arm="$1" trial="$2" record_claims="$3" dir="$4"

  echo "── [$arm · trial $trial] → $dir ─────────────────────────────────────" >&2
  seed_project "$dir"
  prepare_arm "$dir" "$record_claims"

  local preamble
  preamble="$(cd "$dir" && "$H5I" context prompt 2>/dev/null || true)"
  local known_facts_lines
  known_facts_lines=$(echo "$preamble" | grep -c "^## Known facts" || true)
  echo "$STEP  [$arm · $trial] preamble has Known-facts section: $known_facts_lines" >&2

  local full_prompt
  full_prompt="$(printf '%s\n\n---\n\n%s\n' "$preamble" "$TASK")"

  echo "$STEP  [$arm · $trial] running claude --print (timeout ${TRIAL_TIMEOUT}s)…" >&2
  local start_ts rc
  start_ts=$(date +%s)
  # timeout --kill-after sends SIGKILL 10s after initial SIGTERM in case claude
  # ignores the term signal. Exit 124 == timeout fired.
  set +e
  (cd "$dir" && printf '%s' "$full_prompt" \
    | timeout --kill-after=10 "${TRIAL_TIMEOUT}" \
        claude --print \
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
  # Fidelity: did the agent touch client.py? Did it wrongly edit utils?
  local client_edited utils_edited_wrongly
  client_edited=$(git -C "$dir" diff --name-only | grep -c "src/api/client.py" || true)
  utils_edited_wrongly=$(git -C "$dir" diff --name-only \
    | grep -c -E "src/utils/(format|validate)\.py" || true)

  echo "  correctness: $correct_log_pairs/3 log pairs, client_edited=$client_edited, utils_wrongly=$utils_edited_wrongly" >&2

  # Emit record.
  python3 - "$arm" "$trial" "$elapsed" "$client_edited" "$utils_edited_wrongly" "$correct_log_pairs" "$timed_out" "$parsed" <<'PYEOF'
import json, sys
arm, trial, elapsed, client_edited, utils_wrong, pairs, timed_out, parsed = sys.argv[1:]
rec = json.loads(parsed)
rec.update({
    "arm": arm,
    "trial": int(trial),
    "elapsed_sec": int(elapsed),
    "client_edited": int(client_edited or 0),
    "utils_edited_wrongly": int(utils_wrong or 0),
    "correct_log_pairs": int(pairs or 0),
    "timed_out": bool(int(timed_out)),
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
  local arm="$1" trial="$2" record_claims="$3"
  local attempt=0 final_success=0 record=""
  local max_attempts=$((RETRY_CAP + 1))
  local dir

  while [ "$attempt" -lt "$max_attempts" ]; do
    attempt=$((attempt + 1))
    dir="${WORKDIR_BASE}-${arm}-${trial}"
    [ "$attempt" -gt 1 ] && dir="${dir}-retry${attempt}"
    record=$(run_arm_once "$arm" "$trial" "$record_claims" "$dir")
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

# Interleave arm order across trials to mitigate serial drift (Anthropic-side
# caches, backend load, or model-state drift within a single experiment run).
for i in $(seq 1 "$N_TRIALS"); do
  if [ $((i % 2)) -eq 1 ]; then
    run_arm CONTROL   "$i" 0 >> "$RESULTS_FILE"
    run_arm TREATMENT "$i" 1 >> "$RESULTS_FILE"
  else
    run_arm TREATMENT "$i" 1 >> "$RESULTS_FILE"
    run_arm CONTROL   "$i" 0 >> "$RESULTS_FILE"
  fi
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

if not arms["CONTROL"] or not arms["TREATMENT"]:
    print("  ✖  not enough trials recorded — both arms need ≥1 record")
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
for a, rs in arms.items():
    s_rs = succ[a]
    attempts = sum(r.get("attempts", 1) for r in rs)
    timed_out = sum(1 for r in rs if r.get("timed_out"))
    models = sorted({r.get("model", "") for r in rs if r.get("model")})
    print(f"    {a:10s}  trials: {len(rs)}   successful: {len(s_rs)}   "
          f"total attempts: {attempts}   timed out: {timed_out}")
    if not models:
        print(f"                model: (unknown — no model field in JSONL)")
    elif len(models) == 1:
        print(f"                model: {models[0]}")
    else:
        print(f"                model: MIXED across trials → {models}  ⚠")

# If either arm has zero successful trials, we can't compute the headline.
if not succ["CONTROL"] or not succ["TREATMENT"]:
    print()
    print("  ✖  at least one arm has zero successful trials — no valid comparison")
    print("      (a successful trial = all 3 ENTER+EXIT log pairs, no utils edits, no timeout)")
    sys.exit(1)

# Flag cross-arm model drift. If CONTROL saw model X and TREATMENT saw Y, the
# delta may be confounded by Anthropic-side routing rather than claims.
ctl_models = {r.get("model", "") for r in arms["CONTROL"] if r.get("model")}
trt_models = {r.get("model", "") for r in arms["TREATMENT"] if r.get("model")}
if ctl_models and trt_models and ctl_models != trt_models:
    print()
    print(f"  ⚠  model IDs differ across arms (CONTROL={ctl_models}, TREATMENT={trt_models}) —")
    print(f"     the delta may be confounded by Anthropic-side routing, not claims alone.")

# ── Main table: one row per metric, mean±sd [min, max] per arm, delta, noise flag
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

print()
print(f"  Successful trials only ({len(succ['CONTROL'])} CONTROL, {len(succ['TREATMENT'])} TREATMENT):")
print()
print(f"  {'metric':22s} {'CONTROL  mean±sd  [min..max]':>42s} {'TREATMENT  mean±sd  [min..max]':>42s} {'Δ%':>7s} {'noise?':>8s}")
print(f"  {'-'*22} {'-'*42} {'-'*42} {'-'*7} {'-'*8}")

def row(key, label):
    c = summarize([r.get(key, 0) for r in succ["CONTROL"]])
    t = summarize([r.get(key, 0) for r in succ["TREATMENT"]])
    c_cell = f"{fmt_num(c['mean'])} ± {fmt_num(c['sd'])}  [{fmt_num(c['lo'])}..{fmt_num(c['hi'])}]"
    t_cell = f"{fmt_num(t['mean'])} ± {fmt_num(t['sd'])}  [{fmt_num(t['lo'])}..{fmt_num(t['hi'])}]"
    delta = t['mean'] - c['mean']
    pct = (delta / c['mean'] * 100.0) if c['mean'] else 0.0
    # Noise flag: 2·max(sd) ≥ |delta|  →  variance comparable to effect
    noisy = "⚠ noise" if 2 * max(c['sd'], t['sd']) >= abs(delta) and abs(delta) > 0 else ""
    print(f"  {label:22s} {c_cell:>42s} {t_cell:>42s} {pct:>+6.1f}% {noisy:>8s}")

for key, label in fields:
    row(key, label)

# ── Fidelity summary ────────────────────────────────────────────────────────
print()
print("  Fidelity across ALL attempts (including retries):")
for arm_name, rs in arms.items():
    all_pairs = [r.get("correct_log_pairs", 0) for r in rs]
    perfect = sum(1 for p in all_pairs if p == 3)
    utils_wrong = sum(1 for r in rs if r.get("utils_edited_wrongly", 0) > 0)
    timed_out = sum(1 for r in rs if r.get("timed_out"))
    print(f"    {arm_name:10s}  all-3-log-pairs: {perfect}/{len(rs)}   "
          f"wrong files: {utils_wrong}   timed out: {timed_out}")

# ── Headline verdict ─────────────────────────────────────────────────────────
c_cr = summarize([r.get("cache_read_tokens", 0) for r in succ["CONTROL"]])
t_cr = summarize([r.get("cache_read_tokens", 0) for r in succ["TREATMENT"]])
print()
if c_cr['mean'] == 0:
    print("  ℹ  no cache-read token data — JSONL format may not include usage.")
else:
    delta_pct = (c_cr['mean'] - t_cr['mean']) / c_cr['mean'] * 100.0
    # Is the effect robust given the variance?
    noisy = 2 * max(c_cr['sd'], t_cr['sd']) >= abs(c_cr['mean'] - t_cr['mean'])
    if delta_pct > 0 and not noisy:
        print(f"  ✔  TREATMENT used {delta_pct:.1f}% fewer cache-read tokens on average")
        print(f"     (effect > 2·stdev — unlikely to be pure noise).")
    elif delta_pct > 0 and noisy:
        print(f"  ~  TREATMENT used {delta_pct:.1f}% fewer cache-read tokens, but the")
        print(f"     within-arm stdev is comparable to the delta — needs more trials.")
    else:
        print(f"  ✖  TREATMENT used {-delta_pct:.1f}% MORE cache-read tokens — hypothesis not supported.")

# ── Sample-size caveat ──────────────────────────────────────────────────────
print()
n_min = min(len(succ["CONTROL"]), len(succ["TREATMENT"]))
if n_min < 5:
    print(f"  ℹ  small-sample caveat: only {n_min} successful trial(s) in the smaller arm.")
    print(f"      Run with N_TRIALS=10 for a more trustworthy stdev.")
elif n_min < 10:
    print(f"  ℹ  {n_min} successful trials per arm — decent, but percentiles are still noisy.")
    print(f"      N=10+ recommended for pitch-grade numbers.")
PYEOF

echo
echo "══════════════════════════════════════════════════════════════════════════"
echo "  Raw per-trial records:  $RESULTS_JSON_ONLY"
echo "  Workdirs preserved:     ${WORKDIR_BASE}-{CONTROL,TREATMENT}-<trial>"
echo "  Inspect a run:          cat <workdir>/.git/.h5i/claims/*.json"
echo "══════════════════════════════════════════════════════════════════════════"
echo
echo "$STEP  done."
