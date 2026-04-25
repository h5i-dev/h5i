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
# Write an ephemeral MCP config so `claude --print` actually mounts the h5i
# server (not just allows the wildcard). We pin the exact H5I_BIN so test
# sessions always use the binary the caller asked for, not whatever is on PATH.
MCP_CONFIG="$WORKDIR/.h5i-mcp-config.json"
python3 - "$H5I_BIN" "$MCP_CONFIG" <<'EOF'
import json, sys, shutil
h5i_bin, out = sys.argv[1], sys.argv[2]
resolved = shutil.which(h5i_bin) or h5i_bin
json.dump(
    {"mcpServers": {"h5i": {"command": resolved, "args": ["mcp"]}}},
    open(out, "w"),
    indent=2,
)
EOF

echo "▶  [$LABEL] Running Claude (task: ${TASK:0:72}…)"
(cd "$WORKDIR" && echo "$TASK" \
  | claude --print \
    --mcp-config "$MCP_CONFIG" \
    --strict-mcp-config \
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

# ── 6. Claims analysis ────────────────────────────────────────────────────────
echo
echo "── [$LABEL] claims ───────────────────────────────────────────────────────"
python3 - "$JSONL" "$WORKDIR" << 'EOF'
import json, pathlib, re, sys

jsonl_path, workdir = sys.argv[1], sys.argv[2]

invocations = []  # (sub, text_or_None, paths_list, raw_cmd, via)
with open(jsonl_path) as f:
    for line in f:
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        if msg.get("type") != "assistant":
            continue
        for block in msg.get("message", {}).get("content", []) or []:
            if block.get("type") != "tool_use":
                continue
            name = block.get("name", "")
            inp  = block.get("input") or {}
            # MCP form: mcp__<server>__h5i_claims_{add,list,prune}
            if "h5i_claims_" in name:
                sub = name.split("h5i_claims_", 1)[1]
                if sub not in ("add", "list", "prune"):
                    continue
                text  = inp.get("text") if sub == "add" else None
                paths = inp.get("paths") or [] if sub == "add" else []
                invocations.append((sub, text, paths, json.dumps(inp), "mcp"))
                continue
            # Bash form: plain `h5i claims ...`
            if name != "Bash":
                continue
            cmd = inp.get("command", "")
            m = re.search(r"\bh5i\s+claims\s+(add|list|prune)\b", cmd)
            if not m:
                continue
            sub = m.group(1)
            text, paths = None, []
            if sub == "add":
                tm = (re.search(r'claims\s+add\s+"((?:[^"\\]|\\.)*)"', cmd)
                      or re.search(r"claims\s+add\s+'((?:[^'\\]|\\.)*)'", cmd))
                if tm:
                    text = tm.group(1)
                paths = re.findall(r"--path(?:\s+|=)([^\s'\"]+)", cmd)
            invocations.append((sub, text, paths, cmd, "bash"))

adds   = [i for i in invocations if i[0] == "add"]
lists  = [i for i in invocations if i[0] == "list"]
prunes = [i for i in invocations if i[0] == "prune"]

def _fmt_via(rs):
    vias = [r[4] for r in rs]
    return f"{len(rs)} ({vias.count('mcp')} mcp, {vias.count('bash')} bash)" if rs else "0"

print(f"  invocations: add={_fmt_via(adds)}  list={_fmt_via(lists)}  prune={_fmt_via(prunes)}")
if not invocations:
    print("  (no `h5i claims` invocations — optional for this task)")

for n, (_, text, paths, _, via) in enumerate(adds, 1):
    print(f"  #{n} add  (via {via}):")
    print(f"      text  : {text if text is not None else '<could not parse — check raw>'}")
    print(f"      paths : {', '.join(paths) if paths else '<none — will have failed>'}")

claims_dir = pathlib.Path(workdir) / ".git" / ".h5i" / "claims"
files = sorted(claims_dir.glob("*.json")) if claims_dir.exists() else []
print()
print(f"  recorded on disk: {len(files)} file(s) under .git/.h5i/claims/")
for p in files:
    try:
        j = json.loads(p.read_text())
        ev = ", ".join(j.get("evidence_paths", []))
        print(f"    [{j.get('id','?')}] {j.get('text','')[:72]}")
        print(f"           ↳ {ev}")
    except Exception as e:
        print(f"    ! failed to parse {p.name}: {e}")

# Per-attempt verdict: did each `add` attempt end up on disk?
if adds:
    recorded_texts = []
    for p in files:
        try:
            recorded_texts.append(json.loads(p.read_text()).get("text", ""))
        except Exception:
            pass
    print()
    print("  per-attempt verdict:")
    for n, (_, text, paths, _, via) in enumerate(adds, 1):
        if text is None:
            verdict = "?  could not parse text"
        elif not paths:
            verdict = "✖  missing paths (add requires ≥1 evidence path)"
        elif text in recorded_texts:
            verdict = "✔  recorded"
        else:
            verdict = "✖  attempted but NOT on disk (likely errored)"
        print(f"    #{n} ({via}) {verdict}")
EOF

# Authoritative live/stale summary from the CLI itself.
if [[ -d "$WORKDIR/.git/.h5i/claims" ]] \
    && compgen -G "$WORKDIR/.git/.h5i/claims/*.json" >/dev/null; then
  echo
  echo "  live/stale (h5i claims list):"
  (cd "$WORKDIR" && "$H5I_BIN" claims list 2>&1 | sed 's/^/    /')
fi

# ── 7. Checklist ──────────────────────────────────────────────────────────────
echo
echo "── [$LABEL] checklist ────────────────────────────────────────────────────"
python3 - "$JSONL" "$WORKDIR" << 'EOF'
import json, pathlib, re, sys

jsonl_path, workdir = sys.argv[1], sys.argv[2]

calls = []
with open(jsonl_path) as f:
    for line in f:
        msg = json.loads(line)
        if msg.get("type") == "assistant":
            for block in msg.get("message", {}).get("content", []):
                if block.get("type") == "tool_use":
                    calls.append((block["name"], str(block.get("input", {}))))

# Detection predicates must accept BOTH forms: Bash (`h5i context init ...`)
# and MCP (`mcp__h5i__h5i_context_init`). The MCP form puts args in the input
# dict rather than a command string, so we match tool name + the serialised
# input when looking for keys like "kind" or "model".
def called(bash_sub, mcp_tool):
    """Either Bash `h5i <bash_sub>` or MCP tool `mcp__h5i__<mcp_tool>`."""
    for name, inp in calls:
        if "Bash" in name and f"h5i {bash_sub}" in inp:
            return True
        if name.endswith(mcp_tool) or f"__{mcp_tool}" in name:
            return True
    return False

def trace_count(kind):
    """Count h5i context trace entries of `kind` (Bash `--kind K` or MCP param)."""
    n = 0
    for name, inp in calls:
        if "Bash" in name and f"--kind {kind}" in inp:
            n += 1
        elif "h5i_context_trace" in name and f"'kind': '{kind}'" in inp:
            n += 1
        elif "h5i_context_trace" in name and f'"kind": "{kind}"' in inp:
            n += 1
    return n

def any_has(p):  return any(p in n+i for n,i in calls)

REJECTION_WORDS = ("over ", "reject", "instead", "rather than", "chose")
def think_with_rejection():
    for name, inp in calls:
        is_think = (
            ("Bash" in name and "--kind THINK" in inp)
            or ("h5i_context_trace" in name
                and ("'kind': 'THINK'" in inp or '"kind": "THINK"' in inp))
        )
        if is_think and any(w in inp for w in REJECTION_WORDS):
            return True
    return False

def commit_with_model():
    # Bash form: `h5i commit ... --model`. MCP form: h5i_commit with a model arg.
    for name, inp in calls:
        if "Bash" in name and "h5i commit" in inp and "--model" in inp:
            return True
        if "h5i_commit" in name and ("'model'" in inp or '"model"' in inp):
            return True
    return False

checks = [
    ("h5i context init",             called("context init", "h5i_context_init")),
    ("Read before edit",             any_has("Read")),
    ("≥1 OBSERVE trace",             trace_count("OBSERVE") >= 1),
    ("≥1 THINK with rejection",      think_with_rejection()),
    ("≥1 ACT trace",                 trace_count("ACT") >= 1),
    ("ACT count ≥ files edited",
        trace_count("ACT") >= sum(1 for n,_ in calls if n in ("Edit","Write"))),
    ("NOTE fired (risk/todo/limit)", trace_count("NOTE") >= 1),
    ("h5i context commit",           called("context commit", "h5i_context_commit")),
    ("git add before h5i commit",
        any("Bash" in n and re.search(r"\bgit\b.*\badd\b", i) for n,i in calls)),
    ("h5i commit --model --agent",   commit_with_model()),
    ("h5i notes analyze",            called("notes analyze", "h5i_notes_analyze")),
]
passed = sum(1 for _,ok in checks if ok)
for label, ok in checks:
    print(f"  {'✔' if ok else '✖'}  {label}")
print(f"\n  {passed}/{len(checks)} checks passed")

# ── Optional claims checks (only meaningful when claims were attempted) ─────
bash_adds = sum(
    1 for n, c in calls
    if "Bash" in n and re.search(r"\bh5i\s+claims\s+add\b", c)
)
mcp_adds = sum(1 for n, _ in calls if "h5i_claims_add" in n)
add_count = bash_adds + mcp_adds
claims_dir = pathlib.Path(workdir) / ".git" / ".h5i" / "claims"
recorded = list(claims_dir.glob("*.json")) if claims_dir.exists() else []

opt_checks = []
if add_count == 0:
    opt_checks.append(("h5i claims used (optional)", None))
else:
    opt_checks.append(("h5i claims used (optional)", True))
    # Every add attempt should leave a file on disk.
    opt_checks.append(("all add attempts recorded",
                       len(recorded) >= add_count))
    # No claim should be stale immediately after the session.
    all_live = True
    for p in recorded:
        try:
            j = json.loads(p.read_text())
            # We don't recompute evidence_oid here — we trust the recorded one
            # matches HEAD unless files were subsequently edited. Heuristic:
            # claims recorded during the same session with no later edits.
            # The authoritative answer is in the `h5i claims list` output above.
        except Exception:
            all_live = False
    opt_checks.append(("all recorded claims parse cleanly", all_live))

print()
print("  (optional) claims checks:")
scored, total = 0, 0
for label, ok in opt_checks:
    if ok is None:
        print(f"  ·  {label} — skipped (no attempts)")
        continue
    total += 1
    if ok:
        scored += 1
    print(f"  {'✔' if ok else '✖'}  {label}")
if total:
    print(f"\n  {scored}/{total} optional claims checks passed")

# Emit machine-readable score (core only, unchanged for aggregators).
print(f"\nSCORE:{passed}/{len(checks)}")
EOF

echo
echo "▶  [$LABEL] done."
