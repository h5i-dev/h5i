#!/usr/bin/env bash
# observe_multi.sh — run observe_session.sh concurrently across diverse tasks,
# then print an aggregated score table so you can spot where h5i workflow
# compliance breaks down by domain.
#
# Usage:
#   ./scripts/observe_multi.sh
#
# Output files: /tmp/h5i-obs-<label>.log  (one per scenario)
# Requirements: same as observe_session.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SEEDS_DIR="$SCRIPT_DIR/seeds"
OBSERVE="$SCRIPT_DIR/observe_session.sh"
LOG_DIR="${OBSERVE_LOG_DIR:-/tmp/h5i-multi-$$}"
mkdir -p "$LOG_DIR"

# ── Scenario table: label | seed file | task ─────────────────────────────────
# Format: "LABEL|SEED_FILENAME|TASK"
# SEED_FILENAME is relative to scripts/seeds/; use "-" for the default Python stub.
declare -a SCENARIOS=(
"ml-training|train.py|Add early stopping to the train() function: monitor validation loss, stop when it hasn't improved for 3 consecutive epochs, and restore the best-seen weights."

"frontend-validation|LoginForm.jsx|Add client-side form validation to LoginForm: email must contain '@', password must be at least 8 characters. Show inline error messages below each field and disable the submit button while invalid."

"backend-ratelimit|server.js|Add per-IP rate limiting to all three routes in server.js: max 10 requests per minute. Return HTTP 429 with a Retry-After header when the limit is exceeded. Use an in-memory store — no external dependencies."

"smart-contract|Vault.sol|The withdraw() function in Vault.sol is vulnerable to reentrancy. Add a ReentrancyGuard (following the checks-effects-interactions pattern) to protect it."

"data-pipeline|pipeline.py|Add fault-tolerant checkpointing to the ETL pipeline: save progress after each batch of 100 rows to a JSON checkpoint file so the pipeline can resume from where it left off after a crash."

"rust-parser|parser.rs|Add comprehensive unit tests for the JSON parser in parser.rs: cover objects, arrays, nested structures, escaped strings, edge cases (empty object, empty array, null, booleans). Use Rust's built-in #[test] framework."
)

# ── Launch all scenarios in parallel ─────────────────────────────────────────
echo "▶  Launching ${#SCENARIOS[@]} concurrent sessions in $LOG_DIR"
echo

declare -a PIDS=()
declare -a LABELS=()

for scenario in "${SCENARIOS[@]}"; do
  IFS='|' read -r label seed_file task <<< "$scenario"
  outfile="$LOG_DIR/$label.log"
  seed_path="$SEEDS_DIR/$seed_file"
  workdir="/tmp/h5i-obs-$label-$$"

  OBSERVE_WORKDIR="$workdir" \
  OBSERVE_LABEL="$label" \
  OBSERVE_OUTFILE="$outfile" \
  SEED_FILE="$seed_path" \
    bash "$OBSERVE" "$task" &

  PIDS+=("$!")
  LABELS+=("$label")
  echo "  started [$label] pid=$! → $outfile"
done

echo
echo "▶  Waiting for all sessions to finish…"
for i in "${!PIDS[@]}"; do
  wait "${PIDS[$i]}" || echo "  warn: [${LABELS[$i]}] exited non-zero"
done

echo
echo "══════════════════════════════════════════════════════════════════════════"
echo "  AGGREGATE RESULTS"
echo "══════════════════════════════════════════════════════════════════════════"

# ── Parse scores and per-check results from each log ─────────────────────────
python3 - "$LOG_DIR" "${LABELS[@]}" << 'EOF'
import sys, os, re

log_dir = sys.argv[1]
labels  = sys.argv[2:]

CHECK_NAMES = [
    "h5i context init",
    "Read before edit",
    "≥1 OBSERVE trace",
    "≥1 THINK with rejection",
    "≥1 ACT trace",
    "ACT count ≥ files edited",
    "NOTE fired (risk/todo/limit)",
    "h5i context commit",
    "git add before h5i commit",
    "h5i commit --model --agent",
    "h5i notes analyze",
]
COL = 28

results = {}  # label → list of bool
scores  = {}  # label → (passed, total)

for label in labels:
    path = os.path.join(log_dir, f"{label}.log")
    if not os.path.exists(path):
        print(f"  ✖  {label}: log not found")
        continue
    text = open(path).read()
    checks = []
    for line in text.splitlines():
        if re.match(r"\s+[✔✖]  ", line):
            checks.append("✔" in line)
    m = re.search(r"SCORE:(\d+)/(\d+)", text)
    if m:
        scores[label] = (int(m.group(1)), int(m.group(2)))
    results[label] = checks

# Header
header = f"  {'check':<{COL}}" + "".join(f"  {l[:12]:<12}" for l in labels)
print(header)
print("  " + "─" * (COL + len(labels) * 14))

# Per-check rows
for i, name in enumerate(CHECK_NAMES):
    row = f"  {name:<{COL}}"
    for label in labels:
        checks = results.get(label, [])
        val = checks[i] if i < len(checks) else None
        cell = "✔" if val else ("✖" if val is False else "?")
        row += f"  {cell:<12}"
    print(row)

# Score row
print("  " + "─" * (COL + len(labels) * 14))
score_row = f"  {'SCORE':<{COL}}"
for label in labels:
    p, t = scores.get(label, (0, 9))
    score_row += f"  {p}/{t:<10}"
print(score_row)

# Failure analysis
print()
missed = {}  # check_name → [labels that failed]
for i, name in enumerate(CHECK_NAMES):
    for label in labels:
        checks = results.get(label, [])
        if i < len(checks) and not checks[i]:
            missed.setdefault(name, []).append(label)

if missed:
    print("  Most-missed checks:")
    for name, failing in sorted(missed.items(), key=lambda x: -len(x[1])):
        print(f"    ✖  {name:<{COL}}  ({len(failing)} scenarios: {', '.join(failing)})")
else:
    print("  All checks passed across all scenarios.")
EOF

echo
echo "▶  Individual logs: $LOG_DIR/"
echo "   Tail any with: tail -40 $LOG_DIR/<label>.log"
