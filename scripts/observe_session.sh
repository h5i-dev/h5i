#!/usr/bin/env bash
# observe_session.sh — spin up a fresh h5i project, run a real Claude session
# with a dummy task, then report exactly which h5i tools Claude called and in
# what order.  Use this to verify that CLAUDE.md instructions are being followed.
#
# Usage:
#   ./scripts/observe_session.sh [TASK_DESCRIPTION]
#
# If TASK_DESCRIPTION is omitted a default Python retry-logic task is used.
#
# Requirements:
#   - claude CLI in PATH  (Claude Code)
#   - h5i CLI in PATH     (this repo, `cargo install --path .`)
#   - git, python3
#
# Output:
#   1. Numbered list of tool calls made by Claude during the session
#   2. A pass/fail checklist against h5i's expected workflow
#   3. The session JSONL path for deeper inspection

set -euo pipefail

H5I_BIN="${H5I_BIN:-h5i}"
WORKDIR="${OBSERVE_WORKDIR:-/tmp/h5i-observe-$$}"
TASK="${1:-Add retry logic with exponential backoff to all three API functions in main.py. Use a max of 3 retries, starting with a 0.5 s delay, doubling each time.}"

# ── 1. Seed project ───────────────────────────────────────────────────────────
echo "▶  Creating demo project at $WORKDIR"
rm -rf "$WORKDIR"
mkdir -p "$WORKDIR"

git -C "$WORKDIR" init -q
git -C "$WORKDIR" config user.email "observe@h5i.dev"
git -C "$WORKDIR" config user.name  "Observe Bot"

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
git -C "$WORKDIR" commit -q -m "initial project"

# ── 2. h5i init (writes CLAUDE.md + .claude/h5i.md) ─────────────────────────
echo "▶  Running h5i init"
(cd "$WORKDIR" && "$H5I_BIN" init) | grep -v "^  " || true   # suppress verbose tips

# ── 3. Run Claude session ─────────────────────────────────────────────────────
echo "▶  Running Claude session (task: ${TASK:0:80}…)"
echo "   allowed tools: mcp__h5i__*, Read, Write, Edit, Bash"
echo

(cd "$WORKDIR" && echo "$TASK" \
  | claude --print \
    --allowedTools "mcp__h5i__*,Read,Write,Edit,Bash" \
  2>&1) || true   # don't abort if claude exits non-zero

# ── 4. Find the session JSONL ─────────────────────────────────────────────────
ENCODED_DIR=$(python3 - "$WORKDIR" << 'EOF'
import sys, re
p = sys.argv[1].lstrip("/").replace("/", "-")
print(p)
EOF
)
SESSION_DIR="$HOME/.claude/projects/-${ENCODED_DIR}"
JSONL=$(ls -t "$SESSION_DIR"/*.jsonl 2>/dev/null | head -1 || true)

if [[ -z "$JSONL" ]]; then
  echo "✖  No session JSONL found under $SESSION_DIR" >&2
  exit 1
fi

echo
echo "── Session log: $JSONL"

# ── 5. Extract tool calls ──────────────────────────────────────────────────────
echo
echo "── Tool calls ────────────────────────────────────────────────────────────"
python3 - "$JSONL" << 'EOF'
import json, sys

calls = []
with open(sys.argv[1]) as f:
    for line in f:
        msg = json.loads(line)
        if msg.get("type") == "assistant":
            for block in msg.get("message", {}).get("content", []):
                if block.get("type") == "tool_use":
                    name = block["name"]
                    inp  = block.get("input", {})
                    # summarise the most useful field per tool
                    detail = (
                        inp.get("command", "")[:90]
                        or inp.get("file_path", "")
                        or inp.get("path", "")
                        or str(inp)[:90]
                    )
                    calls.append((name, detail))

for i, (name, detail) in enumerate(calls, 1):
    print(f"  {i:2}. {name}")
    print(f"      {detail}")
EOF

# ── 6. Checklist ───────────────────────────────────────────────────────────────
echo
echo "── Workflow checklist ────────────────────────────────────────────────────"
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

def any_matches(pattern):
    return any(pattern in name + inp for name, inp in calls)

def bash_matches(pattern):
    return any("Bash" in name and pattern in inp for name, inp in calls)

checks = [
    ("h5i context init",      bash_matches("context init")),
    ("Read before edit",      any_matches("Read")),
    ("h5i context trace OBSERVE", bash_matches("OBSERVE")),
    ("h5i context trace THINK",   bash_matches("THINK")),
    ("h5i context trace ACT",     bash_matches("ACT")),
    ("h5i context commit",        bash_matches("context commit")),
    ("git add before h5i commit", bash_matches("git add")),
    ("h5i commit --model --agent",bash_matches("h5i commit") and bash_matches("--model")),
    ("h5i notes analyze",         bash_matches("notes analyze")),
]

passed = 0
for label, ok in checks:
    mark = "✔" if ok else "✖"
    print(f"  {mark}  {label}")
    if ok:
        passed += 1

print()
print(f"  {passed}/{len(checks)} checks passed")
EOF

echo
echo "▶  Done. Re-run anytime to re-observe from scratch."
echo "   Session JSONL: $JSONL"
