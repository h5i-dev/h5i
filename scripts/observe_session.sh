#!/usr/bin/env bash
# observe_session.sh — spin up a fresh h5i project, run a real Claude session
# with a dummy task, then report exactly which h5i tools Claude called and in
# what order.  Use this to verify that CLAUDE.md instructions are being followed.
#
# Usage:
#   ./scripts/observe_session.sh [TASK_DESCRIPTION]
#
# Environment variables (set by observe_multi.sh or manually):
#   OBSERVE_WORKDIR  — override the temp directory  (default: /tmp/h5i-observe-$$)
#   OBSERVE_LABEL    — short name shown in output    (default: "session")
#   OBSERVE_OUTFILE  — write full output to this file instead of stdout
#   SEED_FILE        — path to a file whose content is copied into the project
#                      as the seed source file; basename is preserved.
#                      If unset, the default Python REST-client stub is used.
#
# Requirements:
#   - claude CLI in PATH  (Claude Code)
#   - h5i CLI in PATH     (this repo, `cargo install --path .`)
#   - git, python3

set -euo pipefail

H5I_BIN="${H5I_BIN:-h5i}"
WORKDIR="${OBSERVE_WORKDIR:-/tmp/h5i-observe-$$}"
LABEL="${OBSERVE_LABEL:-session}"
TASK="${1:-Add retry logic with exponential backoff to all three API functions in main.py. Use a max of 3 retries, starting with a 0.5 s delay, doubling each time.}"

# Redirect everything to OBSERVE_OUTFILE if set
if [[ -n "${OBSERVE_OUTFILE:-}" ]]; then
  exec > "$OBSERVE_OUTFILE" 2>&1
fi

# ── 1. Seed project ───────────────────────────────────────────────────────────
echo "▶  [$LABEL] Creating project at $WORKDIR"
rm -rf "$WORKDIR"
mkdir -p "$WORKDIR"

git -C "$WORKDIR" init -q
git -C "$WORKDIR" config user.email "observe@h5i.dev"
git -C "$WORKDIR" config user.name  "Observe Bot"

if [[ -n "${SEED_FILE:-}" && -f "$SEED_FILE" ]]; then
  cp "$SEED_FILE" "$WORKDIR/$(basename "$SEED_FILE")"
  git -C "$WORKDIR" add "$(basename "$SEED_FILE")"
else
  cat > "$WORKDIR/main.py" << 'PYEOF'
import requests

def fetch_user(user_id: int) -> dict:
    resp = requests.get(f"https://api.example.com/users/{user_id}")
    return resp.json()

def create_post(title: str, body: str, author_id: int) -> dict:
    resp = requests.post("https://api.example.com/posts", json={
        "title": title,
        "body": body,
        "authorId": author_id,
    })
    return resp.json()

def delete_post(post_id: int) -> bool:
    resp = requests.delete(f"https://api.example.com/posts/{post_id}")
    return resp.status_code == 204
PYEOF
  git -C "$WORKDIR" add main.py
fi

git -C "$WORKDIR" commit -q -m "initial project"

# ── 2. h5i init ───────────────────────────────────────────────────────────────
echo "▶  [$LABEL] h5i init"
(cd "$WORKDIR" && "$H5I_BIN" init 2>&1) | grep -E "^✔|^✖|^warn" || true

# ── 3. Run Claude session ─────────────────────────────────────────────────────
echo "▶  [$LABEL] Running Claude (task: ${TASK:0:72}…)"
(cd "$WORKDIR" && echo "$TASK" \
  | claude --print \
    --allowedTools "mcp__h5i__*,Read,Write,Edit,Bash" \
  2>&1) || true

# ── 4. Find the session JSONL ─────────────────────────────────────────────────
ENCODED_DIR=$(python3 -c "
import sys
p = sys.argv[1].lstrip('/').replace('/', '-')
print(p)
" "$WORKDIR")
SESSION_DIR="$HOME/.claude/projects/-${ENCODED_DIR}"
JSONL=$(ls -t "$SESSION_DIR"/*.jsonl 2>/dev/null | head -1 || true)

if [[ -z "$JSONL" ]]; then
  echo "✖  [$LABEL] No session JSONL found under $SESSION_DIR" >&2
  exit 1
fi

echo
echo "── [$LABEL] session log: $JSONL"

# ── 5. Tool call log ──────────────────────────────────────────────────────────
echo
echo "── [$LABEL] tool calls ───────────────────────────────────────────────────"
python3 - "$JSONL" << 'EOF'
import json, sys
calls = []
with open(sys.argv[1]) as f:
    for line in f:
        msg = json.loads(line)
        if msg.get("type") == "assistant":
            for block in msg.get("message", {}).get("content", []):
                if block.get("type") == "tool_use":
                    inp = block.get("input", {})
                    detail = (
                        inp.get("command", "")[:88]
                        or inp.get("file_path", "")
                        or str(inp)[:88]
                    )
                    calls.append((block["name"], detail))
for i, (name, detail) in enumerate(calls, 1):
    print(f"  {i:2}. {name}")
    print(f"      {detail}")
EOF

# ── 6. Checklist ──────────────────────────────────────────────────────────────
echo
echo "── [$LABEL] checklist ────────────────────────────────────────────────────"
python3 - "$JSONL" << 'EOF'
import json, sys
calls = []
with open(sys.argv[1]) as f:
    for line in f:
        msg = json.loads(line)
        if msg.get("type") == "assistant":
            for block in msg.get("message", {}).get("content", []):
                if block.get("type") == "tool_use":
                    calls.append((block["name"], str(block.get("input", {}))))

def bash_has(p): return any("Bash" in n and p in i for n,i in calls)
def any_has(p):  return any(p in n+i for n,i in calls)

def count_kind(k): return sum(1 for n,i in calls if "Bash" in n and f"--kind {k}" in i)

checks = [
    ("h5i context init",              bash_has("context init")),
    ("Read before edit",              any_has("Read")),
    ("≥1 OBSERVE trace",             count_kind("OBSERVE") >= 1),
    ("≥1 THINK with rejection",      any("THINK" in i and ("over " in i or "reject" in i or "instead" in i or "rather than" in i or "chose" in i) for n,i in calls if "Bash" in n)),
    ("≥1 ACT trace",                 count_kind("ACT") >= 1),
    ("ACT count ≥ files edited",     count_kind("ACT") >= sum(1 for n,_ in calls if n in ("Edit","Write"))),
    ("NOTE fired (risk/todo/limit)", bash_has("NOTE")),
    ("h5i context commit",           bash_has("context commit")),
    ("git add before h5i commit",    bash_has("git add")),
    ("h5i commit --model --agent",   bash_has("h5i commit") and bash_has("--model")),
    ("h5i notes analyze",            bash_has("notes analyze")),
]
passed = sum(1 for _,ok in checks if ok)
for label, ok in checks:
    print(f"  {'✔' if ok else '✖'}  {label}")
print(f"\n  {passed}/{len(checks)} checks passed")
# Emit machine-readable score for aggregation
print(f"\nSCORE:{passed}/{len(checks)}")
EOF

echo
echo "▶  [$LABEL] done."
