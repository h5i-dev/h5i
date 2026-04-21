#!/usr/bin/env bash
# experiment_handoff.sh — Verify bi-directional context handoff between Claude and Codex.
#
# Two scenarios run in sequence:
#
#   Scenario A: Claude → Codex
#     1. Claude does a coding task and records OBSERVE/THINK/ACT traces + checkpoint.
#     2. `h5i codex prelude` is captured — this is exactly what Codex sees at start.
#     3. Codex runs (or a synthetic JSONL is injected); `h5i codex sync` imports traces.
#     4. Checklist verifies Codex traces appear alongside Claude's in the shared context.
#
#   Scenario B: Codex → Claude  (vice versa)
#     1. Codex runs first; `h5i codex finish` syncs traces and checkpoints context.
#     2. `h5i hook session-start` is captured — this is what Claude sees at start.
#     3. Claude continues the task; checklist verifies it loaded and extended the context.
#
# When Codex is not in PATH or SYNTHETIC=1, a pre-baked JSONL is injected instead of
# a live Codex run so the entire handoff plumbing is still exercised.
#
# Usage:
#   ./scripts/experiment_handoff.sh [--synthetic]
#
# Environment variables:
#   H5I_BIN    — h5i binary path              (default: h5i)
#   CODEX_CMD  — Codex invocation prefix      (default: "codex --approval-mode full-auto")
#   SYNTHETIC  — skip real Codex run (0|1)    (default: 0; auto-set if codex not in PATH)
#   WORKDIR_A  — temp dir for Scenario A      (default: /tmp/h5i-handoff-A-$$)
#   WORKDIR_B  — temp dir for Scenario B      (default: /tmp/h5i-handoff-B-$$)
#
# Requirements:
#   h5i CLI, claude CLI, git, python3
#   codex CLI (optional — synthetic mode used if absent)

set -euo pipefail

# ── Config ────────────────────────────────────────────────────────────────────
H5I="${H5I_BIN:-h5i}"
CODEX_CMD="${CODEX_CMD:-codex --approval-mode full-auto}"
SYNTHETIC="${SYNTHETIC:-0}"

[[ "${1:-}" == "--synthetic" ]] && SYNTHETIC=1

if [[ "$SYNTHETIC" != "1" ]] && ! command -v codex &>/dev/null; then
  echo "  ℹ  codex not found in PATH — switching to synthetic mode"
  SYNTHETIC=1
fi

WORKDIR_A="${WORKDIR_A:-/tmp/h5i-handoff-A-$$}"
WORKDIR_B="${WORKDIR_B:-/tmp/h5i-handoff-B-$$}"

TASK_CLAUDE_A="Analyze main.py and find all functions that make HTTP requests without any \
error handling. Record what you observe using h5i context traces, write down your plan, \
and add a simple try/except around each bare requests call."

TASK_CODEX_A="The previous agent added try/except blocks in main.py. Read the file, \
write a retry helper that wraps those calls with exponential backoff (max 3 retries), \
and replace the bare try/except blocks with it."

TASK_CODEX_B="Read main.py and add input validation to fetch_user: reject non-positive \
user_id values. Record what you read and what you changed."

TASK_CLAUDE_B="Another agent already added input validation to fetch_user in main.py. \
Check h5i context for what was done, then extend the same validation pattern to \
create_post (validate title is non-empty) and delete_post (validate post_id > 0)."

# ── Helpers ───────────────────────────────────────────────────────────────────
PASS="✔"
FAIL="✖"
STEP="▶"

count_lines() { echo "$1" | grep -c "$2" 2>/dev/null || true; }

check() {
  local label="$1" ok="$2"
  if [[ "$ok" == "1" ]]; then
    echo "  $PASS  $label"
    return 0
  else
    echo "  $FAIL  $label"
    return 1
  fi
}

seed_project() {
  local dir="$1"
  rm -rf "$dir"
  mkdir -p "$dir"
  git -C "$dir" init -q
  git -C "$dir" config user.email "handoff@h5i.dev"
  git -C "$dir" config user.name  "Handoff Bot"
  cat > "$dir/main.py" << 'PYEOF'
import requests

def fetch_user(user_id: int) -> dict:
    resp = requests.get(f"https://api.example.com/users/{user_id}")
    return resp.json()

def create_post(title: str, body: str, author_id: int) -> dict:
    resp = requests.post("https://api.example.com/posts", json={
        "title": title, "body": body, "authorId": author_id,
    })
    return resp.json()

def delete_post(post_id: int) -> bool:
    resp = requests.delete(f"https://api.example.com/posts/{post_id}")
    return resp.status_code == 204
PYEOF
  git -C "$dir" add main.py
  git -C "$dir" commit -q -m "initial"
}

# Injects a minimal Codex session JSONL that records a read + a patch edit of main.py.
# The CWD field in session_meta must match workdir exactly for codex.rs to pick it up.
inject_synthetic_codex_session() {
  local workdir="$1"
  local tag="${2:-default}"
  local session_dir="$HOME/.codex/sessions/synthetic-handoff"
  mkdir -p "$session_dir"
  local session_file="$session_dir/h5i-handoff-${tag}.jsonl"
  python3 - "$workdir" "$session_file" << 'PYEOF'
import json, sys
workdir, outfile = sys.argv[1], sys.argv[2]
events = [
    # session_cwd_matches scans the first 40 lines for /payload/cwd
    {"type": "session_meta", "payload": {"cwd": workdir}},
    # OBSERVE: read + search
    {"type": "event_msg", "payload": {
        "type": "exec_command_end",
        "parsed_cmd": [
            {"type": "read",   "path": f"{workdir}/main.py"},
            {"type": "search", "path": workdir, "query": "def fetch_user"},
        ],
    }},
    # ACT: apply_patch — Codex's patch dialect (*** Update File / Add File / Delete File)
    {"type": "response_item", "payload": {
        "type": "function_call",
        "name": "apply_patch",
        "arguments": (
            "*** Begin Patch\n"
            "*** Update File: main.py\n"
            "@@ fetch_user @@\n"
            "+    if user_id <= 0:\n"
            "+        raise ValueError('user_id must be positive')\n"
            "*** End Patch\n"
        ),
    }},
]
with open(outfile, "w") as f:
    for e in events:
        f.write(json.dumps(e) + "\n")
print(f"    synthetic session → {outfile}")
PYEOF
}

# Returns "1" if Claude's session JSONL shows a Bash call containing $needle.
claude_bash_has() {
  local jsonl="$1" needle="$2"
  python3 - "$jsonl" "$needle" << 'PYEOF'
import json, sys
jsonl, needle = sys.argv[1], sys.argv[2]
with open(jsonl) as f:
    for line in f:
        try: msg = json.loads(line)
        except json.JSONDecodeError: continue
        if msg.get("type") == "assistant":
            for block in msg.get("message", {}).get("content", []):
                if block.get("type") == "tool_use" and "Bash" in block["name"]:
                    if needle in str(block.get("input", {})):
                        print("1"); sys.exit(0)
print("0")
PYEOF
}

# Returns the path to the most recent Claude session JSONL for $workdir, or "".
find_claude_jsonl() {
  local workdir="$1"
  local encoded
  encoded=$(python3 -c "
import sys
p = sys.argv[1].lstrip('/').replace('/', '-')
print(p)
" "$workdir")
  ls -t "$HOME/.claude/projects/-${encoded}"/*.jsonl 2>/dev/null | head -1 || true
}

# ═══════════════════════════════════════════════════════════════════════════════
# SCENARIO A: Claude first → Codex picks up the context
# ═══════════════════════════════════════════════════════════════════════════════
echo
echo "══════════════════════════════════════════════════════════════════════════"
echo "  SCENARIO A  ·  Claude makes changes → Codex resumes from context"
echo "══════════════════════════════════════════════════════════════════════════"

seed_project "$WORKDIR_A"

echo "$STEP  [A] h5i init"
(cd "$WORKDIR_A" && "$H5I" init 2>&1) | grep -E "^✔|^✖|^warn" || true

echo "$STEP  [A] h5i context init"
(cd "$WORKDIR_A" && "$H5I" context init \
  --goal "Improve error handling and retry logic in main.py") 2>&1 || true

# ── A.1: Claude session ────────────────────────────────────────────────────────
echo
echo "▷  [A.1] Running Claude  (${TASK_CLAUDE_A:0:72}…)"
(cd "$WORKDIR_A" && printf '%s\n' "$TASK_CLAUDE_A" \
  | claude --print \
    --allowedTools "mcp__h5i__*,Read,Write,Edit,Bash" \
  2>&1) || true

# Fallback: if Claude didn't checkpoint context, do it manually so Codex has something to load.
(cd "$WORKDIR_A" && "$H5I" context commit "Claude session complete" \
  --detail "analyzed main.py; added try/except to HTTP calls" 2>/dev/null) || true

CTX_A=$(cd "$WORKDIR_A" && "$H5I" context show --trace 2>/dev/null || true)
A_THINK=$(count_lines "$CTX_A" "THINK:")
A_OBSERVE=$(count_lines "$CTX_A" "OBSERVE:")
A_CHECKPOINT=$(count_lines "$CTX_A" "\[.*\]")  # context commits look like [sha] message

echo
echo "── [A.1] Context after Claude ────────────────────────────────────────────"
echo "$CTX_A" | head -35

# ── A.2: Prelude — what Codex would see ───────────────────────────────────────
echo
echo "── [A.2] h5i codex prelude  (what Codex sees on start) ──────────────────"
PRELUDE_A=$(cd "$WORKDIR_A" && "$H5I" codex prelude 2>/dev/null || true)
echo "$PRELUDE_A"
A_PRELUDE_NONEMPTY=$(count_lines "$PRELUDE_A" "Context workspace active\|THINK\|ACT\|TODO\|prior reasoning")

# ── A.3: Codex session + sync ─────────────────────────────────────────────────
echo
if [[ "$SYNTHETIC" == "1" ]]; then
  echo "$STEP  [A.3] Injecting synthetic Codex session (SYNTHETIC=1)"
  inject_synthetic_codex_session "$WORKDIR_A" "scenario-a"
else
  echo "$STEP  [A.3] Running Codex  (${TASK_CODEX_A:0:72}…)"
  # Prepend the prelude so Codex gets the prior context in its first message.
  FULL_PROMPT_A="$(cd "$WORKDIR_A" && "$H5I" codex prelude 2>/dev/null || true)

${TASK_CODEX_A}"
  (cd "$WORKDIR_A" && $CODEX_CMD "$FULL_PROMPT_A" 2>&1) || true
fi

echo "$STEP  [A.3] h5i codex sync"
SYNC_A=$(cd "$WORKDIR_A" && "$H5I" codex sync 2>/dev/null || true)
echo "  $SYNC_A"

CTX_A_POST=$(cd "$WORKDIR_A" && "$H5I" context show --trace 2>/dev/null || true)
A_POST_OBSERVE=$(count_lines "$CTX_A_POST" "OBSERVE:")
A_POST_ACT=$(count_lines "$CTX_A_POST" "ACT:")

echo
echo "── [A.3] Context after Codex sync ────────────────────────────────────────"
echo "$CTX_A_POST" | tail -25

# ── A: Checklist ──────────────────────────────────────────────────────────────
echo
echo "── [A] Checklist ─────────────────────────────────────────────────────────"
A_SCORE=0; A_TOTAL=6
_ac() { if check "$1" "$2"; then ((A_SCORE++)) || true; fi; }

_ac "Claude left ≥1 THINK trace"           "$([[ $A_THINK   -ge 1 ]] && echo 1 || echo 0)"
_ac "Claude left ≥1 OBSERVE trace"         "$([[ $A_OBSERVE -ge 1 ]] && echo 1 || echo 0)"
_ac "Context has ≥1 checkpoint after Claude" "$([[ $A_CHECKPOINT -ge 1 ]] && echo 1 || echo 0)"
_ac "Prelude output is non-empty / shows context" "$([[ $A_PRELUDE_NONEMPTY -ge 1 ]] && echo 1 || echo 0)"
_ac "Codex sync added ≥1 OBSERVE trace"    "$([[ $A_POST_OBSERVE -ge $((A_OBSERVE + 1)) ]] && echo 1 || echo 0)"
_ac "Codex sync added ≥1 ACT trace"        "$([[ $A_POST_ACT -ge 1 ]] && echo 1 || echo 0)"

echo
echo "  A score: $A_SCORE/$A_TOTAL"

# ═══════════════════════════════════════════════════════════════════════════════
# SCENARIO B: Codex first → Claude picks up the context  (vice versa)
# ═══════════════════════════════════════════════════════════════════════════════
echo
echo "══════════════════════════════════════════════════════════════════════════"
echo "  SCENARIO B  ·  Codex makes changes → Claude resumes from context"
echo "══════════════════════════════════════════════════════════════════════════"

seed_project "$WORKDIR_B"

echo "$STEP  [B] h5i init"
(cd "$WORKDIR_B" && "$H5I" init 2>&1) | grep -E "^✔|^✖|^warn" || true

echo "$STEP  [B] h5i context init"
(cd "$WORKDIR_B" && "$H5I" context init \
  --goal "Add input validation to all HTTP functions in main.py") 2>&1 || true

# ── B.1: Codex session + finish ───────────────────────────────────────────────
echo
if [[ "$SYNTHETIC" == "1" ]]; then
  echo "$STEP  [B.1] Injecting synthetic Codex session (SYNTHETIC=1)"
  inject_synthetic_codex_session "$WORKDIR_B" "scenario-b"
else
  echo "$STEP  [B.1] Running Codex  (${TASK_CODEX_B:0:72}…)"
  (cd "$WORKDIR_B" && $CODEX_CMD "$TASK_CODEX_B" 2>&1) || true
fi

echo "$STEP  [B.1] h5i codex finish"
FINISH_B=$(cd "$WORKDIR_B" && "$H5I" codex finish \
  --summary "Codex: added input validation to fetch_user in main.py" 2>/dev/null || true)
echo "  $FINISH_B"

CTX_B=$(cd "$WORKDIR_B" && "$H5I" context show --trace 2>/dev/null || true)
B_OBSERVE=$(count_lines "$CTX_B" "OBSERVE:")
B_ACT=$(count_lines "$CTX_B" "ACT:")
B_CHECKPOINT=$(count_lines "$CTX_B" "\[.*\]")

echo
echo "── [B.1] Context after Codex + finish ────────────────────────────────────"
echo "$CTX_B" | head -35

# ── B.2: session-start — what Claude would see ────────────────────────────────
echo
echo "── [B.2] h5i hook session-start  (what Claude sees on start) ────────────"
SESSION_START_B=$(cd "$WORKDIR_B" && "$H5I" hook session-start 2>/dev/null || true)
echo "$SESSION_START_B"
B_SESSION_START_NONEMPTY=$(count_lines "$SESSION_START_B" "Context workspace active\|THINK\|ACT\|TODO\|prior reasoning")

# ── B.3: Claude session ────────────────────────────────────────────────────────
echo
echo "▷  [B.3] Running Claude  (${TASK_CLAUDE_B:0:72}…)"
(cd "$WORKDIR_B" && printf '%s\n' "$TASK_CLAUDE_B" \
  | claude --print \
    --allowedTools "mcp__h5i__*,Read,Write,Edit,Bash" \
  2>&1) || true

JSONL_B=$(find_claude_jsonl "$WORKDIR_B")

if [[ -n "$JSONL_B" ]]; then
  echo "  session log: $JSONL_B"
  B_USED_CONTEXT=$(claude_bash_has "$JSONL_B" "context")
  B_READ_MAIN=$(python3 - "$JSONL_B" << 'PYEOF'
import json, sys
with open(sys.argv[1]) as f:
    for line in f:
        try: msg = json.loads(line)
        except json.JSONDecodeError: continue
        if msg.get("type") == "assistant":
            for block in msg.get("message", {}).get("content", []):
                if block.get("type") == "tool_use" and block["name"] in ("Read","Edit","Write"):
                    if "main.py" in str(block.get("input", {})):
                        print("1"); sys.exit(0)
print("0")
PYEOF
  )
  # Check if Claude extended context beyond what Codex left
  CTX_B_POST=$(cd "$WORKDIR_B" && "$H5I" context show --trace 2>/dev/null || true)
  B_POST_THINK=$(count_lines "$CTX_B_POST" "THINK:")
  B_POST_ACT=$(count_lines "$CTX_B_POST" "ACT:")
else
  echo "  $FAIL  No Claude session JSONL found under $WORKDIR_B"
  B_USED_CONTEXT="0"
  B_READ_MAIN="0"
  B_POST_THINK=0
  B_POST_ACT=$B_ACT  # unchanged
fi

# ── B: Checklist ──────────────────────────────────────────────────────────────
echo
echo "── [B] Checklist ─────────────────────────────────────────────────────────"
B_SCORE=0; B_TOTAL=6
_bc() { if check "$1" "$2"; then ((B_SCORE++)) || true; fi; }

_bc "Codex sync imported ≥1 OBSERVE trace"        "$([[ $B_OBSERVE -ge 1 ]] && echo 1 || echo 0)"
_bc "Codex sync imported ≥1 ACT trace"            "$([[ $B_ACT     -ge 1 ]] && echo 1 || echo 0)"
_bc "Codex finish created a context checkpoint"   "$([[ $B_CHECKPOINT -ge 1 ]] && echo 1 || echo 0)"
_bc "session-start output shows prior context"    "$([[ $B_SESSION_START_NONEMPTY -ge 1 ]] && echo 1 || echo 0)"
_bc "Claude used h5i context commands"            "$B_USED_CONTEXT"
_bc "Claude left ≥1 new ACT or THINK trace"       "$([[ $B_POST_ACT -gt $B_ACT || $B_POST_THINK -ge 1 ]] && echo 1 || echo 0)"

echo
echo "  B score: $B_SCORE/$B_TOTAL"

# ═══════════════════════════════════════════════════════════════════════════════
# Summary
# ═══════════════════════════════════════════════════════════════════════════════
TOTAL=$((A_SCORE + B_SCORE))
MAX=$((A_TOTAL + B_TOTAL))

echo
echo "══════════════════════════════════════════════════════════════════════════"
echo "  OVERALL: $TOTAL/$MAX checks passed"
printf "  %-36s  %d/%d\n" "Scenario A (Claude→Codex):" "$A_SCORE" "$A_TOTAL"
printf "  %-36s  %d/%d\n" "Scenario B (Codex→Claude):" "$B_SCORE" "$B_TOTAL"
echo "══════════════════════════════════════════════════════════════════════════"
echo
echo "  Workdirs preserved for inspection:"
echo "    A: $WORKDIR_A"
echo "    B: $WORKDIR_B"
echo
echo "  Inspect context at any time:"
echo "    (cd $WORKDIR_A && h5i context show --trace)"
echo "    (cd $WORKDIR_B && h5i context show --trace)"
echo
echo "$STEP  done."
