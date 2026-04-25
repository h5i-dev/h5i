#!/usr/bin/env bash
# experiment_claims.sh — Measure whether `h5i claims` reduces input-token usage.
#
# Hypothesis:
#   When pre-verified claims are injected into the context prompt, Claude does
#   less re-exploration on a subsequent session and therefore consumes fewer
#   input tokens (fewer Read/Grep calls, smaller per-turn context).
#
# Method:
#   Four-arm factorial — production-realistic auto-curation only. Each pre-seeded
#   arm uses a single Haiku call to write the claims and/or summaries from the
#   seeded source, then the working agent (Opus) runs the task with those
#   artifacts already cached in the prompt prefix.
#
#     CONTROL              — no pre-seeding. Baseline.
#     AUTO_HAIKU_CLM       — Haiku-curated claims only. Cross-cutting invariants
#                            ("HTTP only in {a,b,c}.py") seeded as live claims.
#     AUTO_HAIKU_SUM       — Haiku-curated summaries only. Per-file blob-keyed
#                            summaries with signatures + types + HTTP/non-HTTP marker.
#     AUTO_HAIKU_SUM_CLM   — both: Haiku writes claims AND summaries. Tests
#                            whether the two stack or saturate.
#
#   Each non-CONTROL arm pays a single ~$0.01 Haiku call at workdir setup.
#   The working session uses Opus (claude --print) — same model as CONTROL.
#
#   For each run we parse the Claude session JSONL, sum per-turn token usage,
#   count tool calls, and read out cache_creation/cache_read deltas. A
#   comparison table is printed at the end with each arm's Δ% vs CONTROL.
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

# AUTO_HAIKU arm: cheap-model claim extraction. The Haiku call is one-shot;
# it just needs to produce JSON. We invoke `claude --print --model $HAIKU_MODEL`
# rather than the Anthropic SDK directly so this script has no extra deps.
HAIKU_MODEL="${HAIKU_MODEL:-claude-haiku-4-5}"
HAIKU_TIMEOUT="${HAIKU_TIMEOUT:-90}"
HAIKU_MAX_CLAIMS="${HAIKU_MAX_CLAIMS:-5}"
# Eager-render falls back to listing-only above 10 files / 2K chars; staying at 6
# keeps Haiku-curated summaries within the inline regime.
HAIKU_MAX_SUMMARIES="${HAIKU_MAX_SUMMARIES:-6}"

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

# Hand-curated CLAIMS_SPEC / SUMMARIES_SPEC heredocs were dropped in the
# Haiku-only restructure. All non-CONTROL arms now derive their pre-seeded
# artifacts from a Haiku call against the seeded source files.

# ── Project seed ──────────────────────────────────────────────────────────────
seed_project() {
  local dir="$1"
  rm -rf "$dir"
  mkdir -p "$dir/src/api" "$dir/src/utils" "$dir/src/models" "$dir/src/storage" "$dir/src/workers"
  git -C "$dir" init -q
  git -C "$dir" config user.email "claims-exp@h5i.dev"
  git -C "$dir" config user.name  "Claims Experiment"

  : > "$dir/src/__init__.py"
  : > "$dir/src/api/__init__.py"
  : > "$dir/src/utils/__init__.py"
  : > "$dir/src/models/__init__.py"
  : > "$dir/src/storage/__init__.py"
  : > "$dir/src/workers/__init__.py"

  # ── HTTP files (4) — these are the targets ─────────────────────────────────
  cat > "$dir/src/api/client.py" <<'PYEOF'
"""HTTP client — user + post helpers."""
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

  cat > "$dir/src/api/auth.py" <<'PYEOF'
"""HTTP auth endpoints."""
import requests
from logging import getLogger

log = getLogger(__name__)

BASE = "https://api.example.com/auth"


def login(user: str, pw: str) -> dict:
    resp = requests.post(f"{BASE}/login", json={"u": user, "p": pw}, timeout=5)
    resp.raise_for_status()
    return resp.json()


def logout(token: str) -> bool:
    resp = requests.post(f"{BASE}/logout", headers={"Authorization": token}, timeout=5)
    return resp.status_code == 204


def refresh_token(token: str) -> dict:
    resp = requests.post(f"{BASE}/refresh", headers={"Authorization": token}, timeout=5)
    resp.raise_for_status()
    return resp.json()
PYEOF

  cat > "$dir/src/api/billing.py" <<'PYEOF'
"""HTTP billing endpoints."""
import requests
from logging import getLogger

log = getLogger(__name__)

BASE = "https://api.example.com/billing"


def charge_card(token: str, amount: int) -> dict:
    resp = requests.post(f"{BASE}/charge", json={"token": token, "amount": amount}, timeout=10)
    resp.raise_for_status()
    return resp.json()


def get_invoice(invoice_id: str) -> dict:
    resp = requests.get(f"{BASE}/invoices/{invoice_id}", timeout=5)
    resp.raise_for_status()
    return resp.json()
PYEOF

  cat > "$dir/src/api/notifications.py" <<'PYEOF'
"""HTTP notification endpoints."""
import requests
from logging import getLogger

log = getLogger(__name__)

BASE = "https://api.example.com/notify"


def send_email(to: str, subject: str, body: str) -> bool:
    resp = requests.post(f"{BASE}/email", json={"to": to, "subject": subject, "body": body}, timeout=5)
    return resp.status_code == 200


def send_sms(to: str, body: str) -> bool:
    resp = requests.post(f"{BASE}/sms", json={"to": to, "body": body}, timeout=5)
    return resp.status_code == 200
PYEOF

  # ── Decoy files in api/ — sound HTTP-ish but are local ─────────────────────
  cat > "$dir/src/api/metrics.py" <<'PYEOF'
"""Local Prometheus-style counters — no HTTP."""
from collections import defaultdict
from logging import getLogger

log = getLogger(__name__)

_counters: dict = defaultdict(int)


def record_request(name: str) -> None:
    _counters[name] += 1


def get_counters() -> dict:
    return dict(_counters)
PYEOF

  cat > "$dir/src/api/health.py" <<'PYEOF'
"""Local process health probes — no HTTP."""
import os
from logging import getLogger

log = getLogger(__name__)


def is_running(pid: int) -> bool:
    try:
        os.kill(pid, 0)
        return True
    except OSError:
        return False
PYEOF

  # ── Pure utils (5) — no I/O at all ─────────────────────────────────────────
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

  cat > "$dir/src/utils/crypto.py" <<'PYEOF'
"""Pure crypto helpers — no I/O."""
import hashlib
import hmac


def sha256_hex(s: str) -> str:
    return hashlib.sha256(s.encode()).hexdigest()


def hmac_sign(key: bytes, msg: bytes) -> str:
    return hmac.new(key, msg, hashlib.sha256).hexdigest()
PYEOF

  cat > "$dir/src/utils/parse.py" <<'PYEOF'
"""Pure parsers — no I/O."""
from datetime import datetime


def parse_iso(s: str) -> datetime:
    return datetime.fromisoformat(s)


def parse_csv_line(line: str) -> list:
    return [c.strip() for c in line.split(",")]
PYEOF

  cat > "$dir/src/utils/paths.py" <<'PYEOF'
"""Pure path helpers — no I/O."""
import os


def relative_to(path: str, base: str) -> str:
    return os.path.relpath(path, base)


def splitext(path: str) -> tuple:
    return os.path.splitext(path)
PYEOF

  # ── Models (4) — dataclasses ───────────────────────────────────────────────
  cat > "$dir/src/models/user.py" <<'PYEOF'
"""User dataclass."""
from dataclasses import dataclass


@dataclass
class User:
    id: int
    name: str
    email: str
PYEOF

  cat > "$dir/src/models/post.py" <<'PYEOF'
"""Post dataclass."""
from dataclasses import dataclass


@dataclass
class Post:
    id: int
    title: str
    body: str
    author_id: int
PYEOF

  cat > "$dir/src/models/invoice.py" <<'PYEOF'
"""Invoice dataclass."""
from dataclasses import dataclass


@dataclass
class Invoice:
    id: str
    amount: int
    paid: bool
PYEOF

  cat > "$dir/src/models/session.py" <<'PYEOF'
"""Session dataclass."""
from dataclasses import dataclass


@dataclass
class Session:
    token: str
    user_id: int
    expires_at: int
PYEOF

  # ── Storage (3) — local only, decoy names ──────────────────────────────────
  cat > "$dir/src/storage/cache.py" <<'PYEOF'
"""In-memory cache — no external calls."""
from logging import getLogger

log = getLogger(__name__)

_store: dict = {}


def cache_get(key: str):
    return _store.get(key)


def cache_set(key: str, value) -> None:
    _store[key] = value
PYEOF

  cat > "$dir/src/storage/db.py" <<'PYEOF'
"""Local SQLite — no HTTP."""
import sqlite3
from logging import getLogger

log = getLogger(__name__)

_conn = None


def connect(path: str):
    global _conn
    _conn = sqlite3.connect(path)
    return _conn


def execute(sql: str, params: tuple = ()) -> list:
    if _conn is None:
        raise RuntimeError("not connected")
    return _conn.execute(sql, params).fetchall()
PYEOF

  cat > "$dir/src/storage/fs.py" <<'PYEOF'
"""Pure local-filesystem helpers."""


def read_text(path: str) -> str:
    with open(path) as f:
        return f.read()


def write_text(path: str, text: str) -> None:
    with open(path, "w") as f:
        f.write(text)
PYEOF

  # ── Workers (2) — local, decoy names ───────────────────────────────────────
  cat > "$dir/src/workers/queue.py" <<'PYEOF'
"""Local deque-backed queue — no SQS."""
from collections import deque
from logging import getLogger

log = getLogger(__name__)

_q: deque = deque()


def enqueue(item) -> None:
    _q.append(item)


def dequeue():
    return _q.popleft() if _q else None
PYEOF

  cat > "$dir/src/workers/scheduler.py" <<'PYEOF'
"""Local cron-like scheduler — no HTTP."""
import threading
import time
from logging import getLogger

log = getLogger(__name__)

_stop = threading.Event()


def schedule(every_sec: int, fn) -> None:
    def loop():
        while not _stop.is_set():
            fn()
            time.sleep(every_sec)
    threading.Thread(target=loop, daemon=True).start()


def stop() -> None:
    _stop.set()
PYEOF

  # ── Top-level wiring ───────────────────────────────────────────────────────
  cat > "$dir/main.py" <<'PYEOF'
"""Entry point — wires HTTP and local helpers."""
from src.api.client import fetch_user
from src.api.auth import login
from src.api.billing import get_invoice
from src.api.notifications import send_email
from src.utils.format import format_date
from src.utils.validate import validate_email
from src.storage.cache import cache_get
from src.workers.queue import enqueue


def demo() -> None:
    if not validate_email("a@b.c"):
        return
    user = fetch_user(1)
    cache_get(str(user))
    enqueue(user)


if __name__ == "__main__":
    demo()
PYEOF

  cat > "$dir/config.py" <<'PYEOF'
"""Constants only — no I/O."""
API_BASE = "https://api.example.com"
TIMEOUT = 5
RETRIES = 3
PYEOF

  git -C "$dir" add -A
  git -C "$dir" commit -q -m "seed: 28-file project (4 HTTP, 6 decoy/local, 5 utils, 4 models, 5 storage/workers)"
}

# ── Haiku claim extraction (used by AUTO_HAIKU arm) ──────────────────────────
# Calls a small, cheap model on the seeded codebase ONCE and asks it to write
# up to $HAIKU_MAX_CLAIMS caveman-style claims as strict JSON. We dump every
# .py file in $dir into the prompt; the toy codebase is small enough that
# inlining is cheaper than any tool-use loop.
#
# Why no session transcript: hand-curated TREATMENT claims describe codebase
# INVARIANTS ("HTTP only in client.py"), not session events. Static-from-files
# is sufficient and strictly simpler.
#
# Output: prints a JSON array to stdout: [{"text": "...", "paths": [...]}, ...]
#         On failure (timeout, parse error) prints "[]" and warns to stderr.
#         Never fails the caller.
haiku_extract_claims() {
  local dir="$1"
  local files_dump prompt raw rc

  # Inline every .py file under $dir, paths relative. find -not skips .git.
  files_dump=$(cd "$dir" && find . -type f -name '*.py' \
    -not -path './.git/*' -not -path './.h5i-ctx/*' \
    | sed 's|^\./||' | sort \
    | while read -r f; do
        printf '<file path="%s">\n' "$f"
        cat "$f"
        printf '</file>\n'
      done)

  # Heredoc carrier; the codebase dump is appended at the end so the model
  # sees the rules first, the data last.
  prompt=$(cat <<EOF
You write caveman-style code orientation claims for an AI coding assistant.
A claim is one terse sentence (~30 tokens) pinning an INVARIANT fact about
the codebase: where something lives, what is pure, what is NOT in a file.
Each claim cites the file path(s) it depends on. If any cited file changes,
the claim auto-invalidates — so prefer facts that bear on file content, not
on incidental wording.

Caveman style:
- ~30 tokens per claim, hard cap.
- Drop articles ("the", "a"), copulas ("is", "are"), filler ("the file contains").
- Keep paths, identifiers, function names, types EXACT.
- Bias toward INVARIANTS and NEGATIVE facts ("X only in Y", "Y has no Z").
- Each claim should pin a fact a future session would otherwise re-derive
  via Read or Grep.

Good examples:
- "HTTP only src/api/client.py: fetch_user, create_post, delete_post."
- "src/utils/format.py: format_date, truncate. Pure, no HTTP."
- "main.py wires helpers. No direct HTTP."

Bad (do NOT produce):
- "The src/api/client.py file contains HTTP helpers." (verbose, copula)
- "fetch_user was modified to add logging." (event, not invariant)
- "Code is organized into modules." (vacuous)

Produce up to ${HAIKU_MAX_CLAIMS} claims as a STRICT JSON array. Output
nothing else — no prose before or after, no markdown fences:

[{"text": "...", "paths": ["src/foo.py"]}, ...]

CODEBASE:
${files_dump}
EOF
)

  set +e
  raw=$(printf '%s' "$prompt" | timeout --kill-after=5 "${HAIKU_TIMEOUT}" \
    claude --print --model "$HAIKU_MODEL" 2>/dev/null)
  rc=$?
  set -e

  if [ "$rc" -ne 0 ]; then
    echo "  ⚠  Haiku call exited rc=$rc (timeout or error); proceeding with no claims" >&2
    echo "[]"
    return 0
  fi

  # Parse + validate. Strip code fences if Haiku ignored the "no markdown" rule.
  # Truncate to HAIKU_MAX_CLAIMS as a belt-and-braces cap.
  python3 - "$raw" "$HAIKU_MAX_CLAIMS" <<'PYEOF'
import json, re, sys
raw = sys.argv[1].strip()
cap = int(sys.argv[2])
m = re.match(r"^```(?:json)?\s*\n?(.*?)\n?```\s*$", raw, re.DOTALL)
if m:
    raw = m.group(1).strip()
try:
    parsed = json.loads(raw)
    if not isinstance(parsed, list):
        raise ValueError("top-level JSON is not a list")
    cleaned = []
    for item in parsed:
        if not isinstance(item, dict):
            continue
        text = item.get("text")
        paths = item.get("paths") or []
        if not isinstance(text, str) or not text.strip():
            continue
        if not isinstance(paths, list) or not all(isinstance(p, str) for p in paths):
            continue
        if not paths:
            continue
        cleaned.append({"text": text.strip(), "paths": paths})
        if len(cleaned) >= cap:
            break
    print(json.dumps(cleaned))
except Exception as e:
    sys.stderr.write(f"  ⚠  Haiku output not parseable as JSON: {e}\n")
    sys.stderr.write(f"     raw (first 400 chars): {raw[:400]!r}\n")
    print("[]")
PYEOF
}

# ── Haiku per-file summary extraction ────────────────────────────────────────
# Asks Haiku to produce up to $HAIKU_MAX_SUMMARIES per-file summaries from the
# seeded source. Each summary is the production-realistic equivalent of the
# hand-curated SUMMARIES_SPEC the prior version of this script used.
#
# Output: prints a JSON array to stdout: [{"path": "src/foo.py", "text": "..."}, ...]
#         On failure, prints "[]" and warns to stderr. Never fails the caller.
haiku_extract_summaries() {
  local dir="$1"
  local files_dump prompt raw rc

  files_dump=$(cd "$dir" && find . -type f -name '*.py' \
    -not -path './.git/*' -not -path './.h5i-ctx/*' \
    | sed 's|^\./||' | sort \
    | while read -r f; do
        printf '<file path="%s">\n' "$f"
        cat "$f"
        printf '</file>\n'
      done)

  prompt=$(cat <<EOF
You write caveman-style per-file summaries for an AI coding assistant.
A summary is one terse line (~80 tokens) describing one source file,
keyed by the file's blob OID. When the file changes, the summary
auto-invalidates — so prefer facts that bear on file content, not
on incidental wording.

Caveman style:
- ~80 tokens per summary, hard cap.
- Drop articles ("the", "a"), copulas ("is", "are"), filler ("the file contains", "this module").
- Keep paths, function names, parameter types, return types, key constants EXACT.
- Mark whether the file makes HTTP calls. Be explicit ("HTTP", "NO HTTP").
- Bias toward what a future session needs without reading the file:
  signatures, return types, key URLs/constants, side-effects.

Good examples:
- "src/api/client.py | HTTP. requests to BASE='https://api.example.com'. fetch_user(id: int)→dict GET, create_post(title,body,author_id)→dict POST, delete_post(id: int)→bool DELETE. Logger \`log\` top. All 3 funcs HTTP."
- "src/utils/format.py | Pure. format_date(dt: datetime)→str (YYYY-MM-DD), truncate(s: str, n: int)→str. NO HTTP."
- "src/api/metrics.py | Local prometheus counters via defaultdict. record_request(name), get_counters()→dict. NO HTTP."

Bad (do NOT produce):
- "The file has helper functions for the API." (vague)
- "This module wraps requests..." (verbose, copula)
- "Used to handle HTTP requests" (event description, not invariant)

Pick the up-to-${HAIKU_MAX_SUMMARIES} most orientation-relevant files
from the codebase below — files an agent would benefit from understanding
without a full Read. Prefer files with HTTP boundaries, decoy files that
look HTTP-ish but aren't, and key entry points.

Output STRICT JSON ONLY — no prose, no markdown fences:
[{"path": "src/foo.py", "text": "..."}, ...]

CODEBASE:
${files_dump}
EOF
)

  set +e
  raw=$(printf '%s' "$prompt" | timeout --kill-after=5 "${HAIKU_TIMEOUT}" \
    claude --print --model "$HAIKU_MODEL" 2>/dev/null)
  rc=$?
  set -e

  if [ "$rc" -ne 0 ]; then
    echo "  ⚠  Haiku summary call exited rc=$rc; proceeding with no summaries" >&2
    echo "[]"
    return 0
  fi

  python3 - "$raw" "$HAIKU_MAX_SUMMARIES" <<'PYEOF'
import json, re, sys
raw = sys.argv[1].strip()
cap = int(sys.argv[2])
m = re.match(r"^```(?:json)?\s*\n?(.*?)\n?```\s*$", raw, re.DOTALL)
if m:
    raw = m.group(1).strip()
try:
    parsed = json.loads(raw)
    if not isinstance(parsed, list):
        raise ValueError("top-level JSON is not a list")
    cleaned = []
    seen = set()
    for item in parsed:
        if not isinstance(item, dict):
            continue
        path = item.get("path")
        text = item.get("text")
        if not isinstance(path, str) or not path.strip():
            continue
        if not isinstance(text, str) or not text.strip():
            continue
        path = path.strip()
        if path in seen:
            continue
        seen.add(path)
        cleaned.append({"path": path, "text": text.strip()})
        if len(cleaned) >= cap:
            break
    print(json.dumps(cleaned))
except Exception as e:
    sys.stderr.write(f"  ⚠  Haiku summary output not parseable as JSON: {e}\n")
    sys.stderr.write(f"     raw (first 400 chars): {raw[:400]!r}\n")
    print("[]")
PYEOF
}

# ── Seed helpers — used by AUTO_HAIKU_CLM, AUTO_HAIKU_SUM, AUTO_HAIKU_SUM_CLM ─
seed_haiku_claims() {
  local dir="$1"
  echo "  $STEP  [haiku-claims] calling Haiku ($HAIKU_MODEL)…" >&2
  local haiku_json haiku_spec n_claims
  haiku_json=$(haiku_extract_claims "$dir")
  echo "$haiku_json" > "$dir/.h5i-haiku-claims.json"
  n_claims=$(echo "$haiku_json" | python3 -c \
    "import json,sys; print(len(json.load(sys.stdin)))" 2>/dev/null || echo 0)
  echo "  [haiku-claims] produced $n_claims claim(s)" >&2

  haiku_spec=$(echo "$haiku_json" | python3 -c "
import json, sys
for item in json.load(sys.stdin):
    text = item['text'].replace('|', ' ').replace('\n', ' ')
    paths = ','.join(item['paths'])
    print(f'{text}|{paths}')
" 2>/dev/null || true)

  while IFS='|' read -r text paths; do
    [[ -z "$text" ]] && continue
    local args=()
    IFS=',' read -ra ps <<< "$paths"
    for p in "${ps[@]}"; do args+=(--path "$p"); done
    (cd "$dir" && "$H5I" claims add "$text" "${args[@]}" >/dev/null 2>&1) || {
      echo "  $FAIL  failed to record Haiku claim: $text" >&2
    }
  done <<< "$haiku_spec"
}

seed_haiku_summaries() {
  local dir="$1"
  echo "  $STEP  [haiku-summaries] calling Haiku ($HAIKU_MODEL)…" >&2
  local haiku_json haiku_spec n_summaries
  haiku_json=$(haiku_extract_summaries "$dir")
  echo "$haiku_json" > "$dir/.h5i-haiku-summaries.json"
  n_summaries=$(echo "$haiku_json" | python3 -c \
    "import json,sys; print(len(json.load(sys.stdin)))" 2>/dev/null || echo 0)
  echo "  [haiku-summaries] produced $n_summaries summary(ies)" >&2

  haiku_spec=$(echo "$haiku_json" | python3 -c "
import json, sys
for item in json.load(sys.stdin):
    path = item['path']
    text = item['text'].replace('|', ' ').replace('\n', ' ')
    print(f'{path}|{text}')
" 2>/dev/null || true)

  while IFS='|' read -r path text; do
    [[ -z "$path" ]] && continue
    (cd "$dir" && "$H5I" summary set "$path" --text "$text" >/dev/null 2>&1) || {
      echo "  $FAIL  failed to record Haiku summary for: $path" >&2
    }
  done <<< "$haiku_spec"
}

# ── h5i init + per-arm pre-seeding ────────────────────────────────────────────
# ARM is one of: CONTROL, AUTO_HAIKU_CLM, AUTO_HAIKU_SUM, AUTO_HAIKU_SUM_CLM.
#   CONTROL              — no pre-seeded claims, no pre-seeded summaries.
#   AUTO_HAIKU_CLM       — Haiku-curated claims only.
#   AUTO_HAIKU_SUM       — Haiku-curated summaries only.
#   AUTO_HAIKU_SUM_CLM   — Haiku-curated claims AND summaries (two Haiku calls).
prepare_arm() {
  local dir="$1" arm="$2"
  (cd "$dir" && "$H5I" init >/dev/null 2>&1) || true
  (cd "$dir" && "$H5I" context init --goal \
    "add logging to HTTP helpers; leave other functions untouched" >/dev/null 2>&1) || true

  case "$arm" in
    AUTO_HAIKU_CLM)
      seed_haiku_claims "$dir"
      ;;
    AUTO_HAIKU_SUM)
      seed_haiku_summaries "$dir"
      ;;
    AUTO_HAIKU_SUM_CLM)
      seed_haiku_claims "$dir"
      seed_haiku_summaries "$dir"
      ;;
    CONTROL)
      : # nothing
      ;;
  esac
}

# Map an arm to the H5I_CLAIMS_FREQUENCY value the claude subprocess should see.
# All current arms run with freq=off — pre-seeded artifacts are present (or
# absent in CONTROL), and we don't want the agent to record additional claims
# mid-session and confound the measurement.
freq_for_arm() {
  echo "off"
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
  # Collect diff across all 4 HTTP files. A pair counts only if both ENTER+EXIT
  # lines appear in the added (+) side of the diff for that function name.
  diff=$(git -C "$dir" diff "$seed" -- \
    src/api/client.py src/api/auth.py src/api/billing.py src/api/notifications.py \
    2>/dev/null || true)
  local pairs=0 fn has_enter has_exit
  # All 10 HTTP functions across the 4 HTTP files:
  #   client.py:        fetch_user, create_post, delete_post
  #   auth.py:          login, logout, refresh_token
  #   billing.py:       charge_card, get_invoice
  #   notifications.py: send_email, send_sms
  for fn in fetch_user create_post delete_post \
            login logout refresh_token \
            charge_card get_invoice \
            send_email send_sms; do
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

  # Correctness: both ENTER+EXIT logs for all 10 HTTP helpers (across 4 files).
  local correct_log_pairs
  correct_log_pairs=$(count_correct_log_pairs "$dir")
  # Fidelity: did the agent touch any of the 4 HTTP files?
  # And did it wrongly edit any non-HTTP file (decoys + utils + models + storage + workers + main + config)?
  local changed client_edited utils_edited_wrongly
  changed="$(files_changed_since_seed "$dir")"
  # `client_edited` retains its name for log-line continuity; semantically it's
  # "any of the 4 HTTP files were edited".
  client_edited=$(echo "$changed" | grep -c -E "src/api/(client|auth|billing|notifications)\.py" || true)
  # `utils_edited_wrongly` retains its name for JSONL-field continuity; semantically
  # it's "any non-HTTP source file was edited". Excludes h5i metadata and __init__.
  utils_edited_wrongly=$(echo "$changed" \
    | grep -c -E "src/(utils|models|storage|workers)/[a-z_]+\.py|src/api/(metrics|health)\.py|^main\.py$|^config\.py$" \
    || true)

  echo "  correctness: $correct_log_pairs/10 log pairs, http_files_edited=$client_edited, wrong_files=$utils_edited_wrongly" >&2

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
ok = (r.get("correct_log_pairs", 0) == 10
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
ARMS=(CONTROL AUTO_HAIKU_CLM AUTO_HAIKU_SUM AUTO_HAIKU_SUM_CLM)
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

ARM_ORDER = ["CONTROL", "AUTO_HAIKU_CLM", "AUTO_HAIKU_SUM", "AUTO_HAIKU_SUM_CLM"]
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
    perfect = sum(1 for p in all_pairs if p == 10)
    utils_wrong = sum(1 for r in rs if r.get("utils_edited_wrongly", 0) > 0)
    timed_out = sum(1 for r in rs if r.get("timed_out"))
    print(f"    {arm_name:11s}  all-10-log-pairs: {perfect}/{len(rs)}   "
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
        "AUTO_HAIKU_CLM (Haiku-curated claims only)", "AUTO_HAIKU_CLM",
        direction="savings",
        explain_win="cross-cutting invariants seeded as claims pay off in retrieval",
        explain_loss="Haiku-curated claims did not help on this task",
    )
    verdict(
        "AUTO_HAIKU_SUM (Haiku-curated summaries only)", "AUTO_HAIKU_SUM",
        direction="savings",
        explain_win="per-file orientation summaries pay off in retrieval",
        explain_loss="Haiku-curated summaries did not help on this task",
    )
    verdict(
        "AUTO_HAIKU_SUM_CLM (claims + summaries)", "AUTO_HAIKU_SUM_CLM",
        direction="savings",
        explain_win="claims + summaries stack (or saturate) above either alone",
        explain_loss="combined did not help — diminishing or interfering signals",
    )

    # Stack-vs-saturate check: does CLM + SUM beat the better of CLM-alone or
    # SUM-alone? If yes by ≥5pp, the two seeding mechanisms are complementary.
    # If the combined arm matches the best single arm, they're saturating.
    if all(succ.get(k) for k in ("AUTO_HAIKU_CLM", "AUTO_HAIKU_SUM", "AUTO_HAIKU_SUM_CLM")):
        clm = summarize([r.get("cache_read_tokens", 0) for r in succ["AUTO_HAIKU_CLM"]])
        sum_ = summarize([r.get("cache_read_tokens", 0) for r in succ["AUTO_HAIKU_SUM"]])
        both = summarize([r.get("cache_read_tokens", 0) for r in succ["AUTO_HAIKU_SUM_CLM"]])
        if c_cr['mean'] > 0:
            clm_pct = (c_cr['mean'] - clm['mean']) / c_cr['mean'] * 100.0
            sum_pct = (c_cr['mean'] - sum_['mean']) / c_cr['mean'] * 100.0
            both_pct = (c_cr['mean'] - both['mean']) / c_cr['mean'] * 100.0
            best_single = max(clm_pct, sum_pct)
            stack_gain = both_pct - best_single
            print()
            print(f"  Stack check: CLM saves {clm_pct:+.1f}%, SUM saves {sum_pct:+.1f}%, "
                  f"BOTH saves {both_pct:+.1f}%")
            if stack_gain >= 5:
                print(f"    → claims + summaries STACK (combined beats best single by {stack_gain:+.1f}pp)")
            elif stack_gain > -5:
                print(f"    → claims + summaries SATURATE (combined ≈ best single, gap {stack_gain:+.1f}pp)")
            else:
                print(f"    → claims + summaries INTERFERE (combined worse than best single by {-stack_gain:.1f}pp)")

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
echo "  Workdirs preserved:     ${WORKDIR_BASE}-{CONTROL,AUTO_HAIKU_CLM,AUTO_HAIKU_SUM,AUTO_HAIKU_SUM_CLM}-<trial>"
echo "  Inspect claims:         cat <workdir>/.git/.h5i/claims/*.json"
echo "  Inspect Haiku output:   cat <workdir>/.h5i-haiku-{claims,summaries}.json"
echo "══════════════════════════════════════════════════════════════════════════"
echo
echo "$STEP  done."
