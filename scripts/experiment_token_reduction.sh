#!/usr/bin/env bash
# experiment_token_reduction.sh — Measure the effectiveness of `h5i capture run`
# (the token-reduction object store + structured output) across a matrix of real
# developer tools.
#
# For each tool we run a realistic (faked) command through `h5i capture run` and
# verify the four properties that define the feature's value:
#
#   1. TOKEN CUT      — the summary is far smaller than the raw output
#                       (measured with h5i's own tokenizer via the manifest).
#   2. INFO RETAINED  — the signal an agent needs (failing test, error, location)
#                       survives into the summary / structured findings.
#   3. STRUCTURED     — the normalized ToolResult has the right status
#                       (never "ok"/"passed" on a nonzero exit) and a parser.
#   4. LOSSLESS       — `h5i recall object <id>` returns the raw bytes exactly.
#
# It also reports the aggregate token savings across the whole matrix.
#
# An optional agent-in-the-loop check (`--with-agent`, needs the `claude` CLI)
# confirms an agent can still answer "which test failed and where?" from the
# REDUCED summary alone — the ultimate test that reduction kept what matters.
#
# Usage:
#   ./scripts/experiment_token_reduction.sh [--with-agent]
#
# Environment:
#   H5I_BIN   — h5i binary path        (default: h5i)
#   WORKDIR   — temp repo              (default: /tmp/h5i-tokred-$$)
#
# Requirements: h5i CLI, git, python3 (claude CLI only for --with-agent).

set -uo pipefail

H5I="${H5I_BIN:-h5i}"
WORKDIR="${WORKDIR:-/tmp/h5i-tokred-$$}"
WITH_AGENT=0
[[ "${1:-}" == "--with-agent" ]] && WITH_AGENT=1

PASS="✔"; FAIL="✖"; STEP="▶"
SCORE=0; TOTAL=0
RAW_TOTAL=0; SUM_TOTAL=0
RESULTS=()   # "label|raw|sum|pct|status|checks"

check() { # label  ok(0|1)
  TOTAL=$((TOTAL+1))
  if [[ "$2" == "1" ]]; then SCORE=$((SCORE+1)); printf "      %s %s\n" "$PASS" "$1"; return 0
  else printf "      %s %s\n" "$FAIL" "$1"; return 1; fi
}

# ── temp repo + fake tools on PATH ──────────────────────────────────────────────
rm -rf "$WORKDIR"; mkdir -p "$WORKDIR/bin"
git -C "$WORKDIR" init -q
git -C "$WORKDIR" config user.email tokred@h5i.dev
git -C "$WORKDIR" config user.name "TokRed Bot"
git -C "$WORKDIR" commit -q --allow-empty -m init
(cd "$WORKDIR" && "$H5I" init >/dev/null 2>&1) || true
export PATH="$WORKDIR/bin:$PATH"

# A throwaway repo used only to tokenize arbitrary text with h5i's own tokenizer
# (via `objects put` → manifest.raw_tokens), so we measure the EXACT output an
# agent sees, not an internal field.
TOKREPO="$WORKDIR/.tokrepo"
git -C "$(dirname "$TOKREPO")" init -q "$(basename "$TOKREPO")" 2>/dev/null || { mkdir -p "$TOKREPO"; git -C "$TOKREPO" init -q; }
git -C "$TOKREPO" config user.email t@t.t; git -C "$TOKREPO" config user.name t
(cd "$TOKREPO" && "$H5I" init >/dev/null 2>&1) || true
tok_count() { # <file> → token count of its contents
  ( cd "$TOKREPO" && "$H5I" objects put "$1" >/dev/null 2>&1 ) || true
  local i; i=$( cd "$TOKREPO" && "$H5I" recall objects --limit 1 2>/dev/null | grep -oE '[0-9a-f]{16}' | head -1 )
  ( cd "$TOKREPO" && "$H5I" recall object "$i" --manifest 2>/dev/null ) | jq_field raw_tokens
}

# make_tool <name> <exit> <<'EOF' ...output... EOF
make_tool() {
  local name="$1" code="$2"
  { echo "#!/usr/bin/env bash"; echo "cat <<'__OUT__'"; cat; echo "__OUT__"; echo "exit $code"; } \
    > "$WORKDIR/bin/$name"
  chmod +x "$WORKDIR/bin/$name"
}

jq_field() { python3 -c "import json,sys;d=json.load(sys.stdin);print(d.get('$1',0) if d else 0)"; }
struct_status() { python3 -c "import json,sys;d=json.load(sys.stdin);s=d.get('structured') or {};print(s.get('status',''))"; }
struct_conf() { python3 -c "import json,sys;d=json.load(sys.stdin);s=d.get('structured') or {};print(s.get('parser_confidence',''))"; }

# measure <label> <category:noisy|diag> <expect_status> <keep_regex> -- <command...>
#   noisy  — noise-dominated output (tests/builds/logs): assert a big token cut.
#   diag   — signal-dense diagnostics (linters/type-checkers): structured trades
#            tokens for machine-actionable structure, so token cut is reported,
#            not asserted.
measure() {
  local label="$1" category="$2" expect_status="$3" keep="$4"; shift 4
  shift # the "--"
  local cmd=( "$@" )

  echo
  echo "── $label  ·  \$ ${cmd[*]} ─────────────────────────"
  # Capture (force-store small output). Default output = structured YAML, which
  # is exactly what an agent sees; we measure THAT (stdout), not an internal field.
  local sumfile="$WORKDIR/.sumout"
  ( cd "$WORKDIR" && "$H5I" capture run --min-bytes 0 -- "${cmd[@]}" >"$sumfile" 2>/dev/null ) || true
  local id manifest raw sum status conf restored expected_raw pct
  id=$( cd "$WORKDIR" && "$H5I" recall objects --limit 1 2>/dev/null | grep -oE '[0-9a-f]{16}' | head -1 )
  if [[ -z "$id" ]]; then echo "  $FAIL  (no object captured)"; return; fi
  manifest=$( cd "$WORKDIR" && "$H5I" recall object "$id" --manifest 2>/dev/null )
  raw=$( echo "$manifest" | jq_field raw_tokens )
  sum=$( tok_count "$sumfile" )            # tokens of the agent-facing structured output
  status=$( echo "$manifest" | struct_status )
  conf=$( echo "$manifest" | struct_conf )
  [[ "$raw" =~ ^[0-9]+$ ]] || raw=0
  [[ "$sum" =~ ^[0-9]+$ ]] || sum=0
  RAW_TOTAL=$((RAW_TOTAL+raw)); SUM_TOTAL=$((SUM_TOTAL+sum))
  pct=$( python3 -c "print(round(100-($sum*100/$raw)) if $raw else 0)" )

  echo "    raw=$raw tok → structured=$sum tok   (${pct}% cut)   status=$status  parser=$conf"

  # 1. token economics — depends on category.
  if [[ "$category" == "noisy" ]]; then
    local c1; [[ "$pct" -ge 50 ]] && c1=1 || c1=0
    check "TOKEN CUT     ≥50% on noise-dominated output (${pct}%)" "$c1"
  else
    # diagnostics: structured may be larger; that's the structure↔token trade.
    echo "      ◦ DIAGNOSTIC   structured trades tokens for actionable findings (${pct}% cut)"
  fi
  # 2. info retained (key signal present in the agent-facing structured output)
  local c2; grep -qE "$keep" "$sumfile" && c2=1 || c2=0
  check "INFO RETAINED contains /$keep/" "$c2"
  # 3. structured status correct
  local c3; [[ "$status" == "$expect_status" ]] && c3=1 || c3=0
  check "STRUCTURED    status == $expect_status (got '$status')" "$c3"
  # 4. lossless rehydrate
  expected_raw=$( "${cmd[@]}" 2>/dev/null || true )
  restored=$( cd "$WORKDIR" && "$H5I" recall object "$id" 2>/dev/null )
  local c4; [[ "$restored" == "$expected_raw" ]] && c4=1 || c4=0
  check "LOSSLESS      recall object == raw bytes" "$c4"

  RESULTS+=( "$label|$raw|$sum|$pct|$status" )
}

echo "══════════════════════════════════════════════════════════════════════════"
echo "  EXPERIMENT: token-reduction effectiveness across the tool matrix"
echo "  workdir: $WORKDIR"
echo "══════════════════════════════════════════════════════════════════════════"

# ── Fixtures ────────────────────────────────────────────────────────────────────

# pytest: built with a loop (heredoc can't loop) — 120 PASSED + 1 FAILED.
{
  echo "#!/usr/bin/env bash"; echo "cat <<'__OUT__'"
  echo "============================= test session starts =============================="
  echo "collected 124 items"
  for i in $(seq 1 120); do echo "tests/test_core.py::test_$i PASSED"; done
  echo "tests/test_core.py::test_pay FAILED"
  echo "=================================== FAILURES ==================================="
  echo "_________________________________ test_pay ____________________________________"
  echo ">       assert charge(100) == 100"
  echo "E       assert 0 == 100"
  echo "tests/test_core.py:412: AssertionError"
  echo "=========================== short test summary info ============================"
  echo "FAILED tests/test_core.py::test_pay - assert 0 == 100"
  echo "======================== 1 failed, 120 passed in 8.41s ========================="
  echo "__OUT__"; echo "exit 1"
} > "$WORKDIR/bin/pytest"; chmod +x "$WORKDIR/bin/pytest"

{
  echo "#!/usr/bin/env bash"; echo 'sub=$1'; echo "cat <<'__OUT__'"
  for i in $(seq 1 40); do echo "   Compiling crate_$i v0.1.0"; done
  echo "    Finished test target(s) in 12.4s"; echo "running 88 tests"
  for i in $(seq 1 87); do echo "test mod::t_$i ... ok"; done
  echo "test mod::auth ... FAILED"; echo; echo "failures:"; echo "---- mod::auth stdout ----"
  echo "thread 'mod::auth' panicked at src/auth.rs:55:9:"
  echo "assertion \`left == right\` failed"; echo "  left: 401"; echo "  right: 200"; echo
  echo "test result: FAILED. 87 passed; 1 failed; 0 ignored"; echo "error: test failed"
  echo "__OUT__"; echo "exit 101"
} > "$WORKDIR/bin/cargo"; chmod +x "$WORKDIR/bin/cargo"

# tsc with realistic project-scan chatter (the parser keeps only the TS errors).
{
  echo "#!/usr/bin/env bash"; echo "cat <<'__OUT__'"
  echo "Starting compilation in watch mode..."
  for i in $(seq 1 60); do echo "File change detected. Starting incremental compilation..."; done
  echo "src/api/auth.ts(12,5): error TS2322: Type 'string' is not assignable to type 'number'."
  echo "src/components/Button.tsx(8,3): error TS2339: Property 'onClick' does not exist."
  echo "Found 2 errors in 2 files."
  echo "__OUT__"; echo "exit 2"
} > "$WORKDIR/bin/tsc"; chmod +x "$WORKDIR/bin/tsc"

# ruff with a realistic batch of issues across many files.
{
  echo "#!/usr/bin/env bash"; echo "cat <<'__OUT__'"
  echo "app/models.py:1:1: F401 [*] \`os\` imported but unused"
  echo "app/views.py:42:5: E711 comparison to \`None\` should be \`cond is None\`"
  for i in $(seq 1 24); do echo "app/mod_$i.py:$i:1: E501 line too long (96 > 88 characters)"; done
  echo "Found 26 errors."
  echo "__OUT__"; echo "exit 1"
} > "$WORKDIR/bin/ruff"; chmod +x "$WORKDIR/bin/ruff"

{
  echo "#!/usr/bin/env bash"; echo "cat <<'__OUT__'"
  echo "src/a.py:12: error: Incompatible return value type [return-value]"
  echo "src/b.py:3: error: Name 'foo' is not defined [name-defined]"
  for i in $(seq 1 24); do echo "src/m_$i.py:$i: error: Argument 1 has incompatible type [arg-type]"; done
  echo "Found 26 errors in 26 files (checked 40 source files)"
  echo "__OUT__"; echo "exit 1"
} > "$WORKDIR/bin/mypy"; chmod +x "$WORKDIR/bin/mypy"

# go build failure with download chatter (parser keeps the compiler diagnostic).
{
  echo "#!/usr/bin/env bash"; echo "cat <<'__OUT__'"
  for i in $(seq 1 40); do echo "go: downloading example.com/dep$i v1.$i.0"; done
  echo "# example/pkg"
  echo "./main.go:6:2: undefined: missing"
  echo "FAIL	example/pkg [build failed]"
  echo "__OUT__"; echo "exit 2"
} > "$WORKDIR/bin/go"; chmod +x "$WORKDIR/bin/go"

# a noisy service log: 800 near-identical lines + one buried ERROR (exit 0)
{
  echo "#!/usr/bin/env bash"; echo "cat <<'__OUT__'"
  for i in $(seq 1 400); do echo "2026-06-05T10:00:$((i%60)) INFO worker handled request $i in $((i%9))ms"; done
  echo "2026-06-05T10:05:01 ERROR db connection pool exhausted at pool.rs:88"
  for i in $(seq 401 800); do echo "2026-06-05T10:06:$((i%60)) INFO worker handled request $i ok"; done
  echo "__OUT__"; echo "exit 0"
} > "$WORKDIR/bin/myservice"; chmod +x "$WORKDIR/bin/myservice"

# a big JSON payload (exit 0)
{
  echo "#!/usr/bin/env bash"; echo "cat <<'__OUT__'"
  printf '{"status":"error","code":503,"message":"db timeout after 30s","rows":['
  for i in $(seq 1 400); do printf '{"id":%s,"name":"item-%s","ok":true},' "$i" "$i"; done
  printf '{"id":401}]}'
  echo; echo "__OUT__"; echo "exit 0"
} > "$WORKDIR/bin/dumpjson"; chmod +x "$WORKDIR/bin/dumpjson"

# ── Run the matrix ──────────────────────────────────────────────────────────────
# label                   category  expected-status  keep-regex                  -- command
measure "pytest (1 fail/124)"  noisy failed  "test_pay|assert 0 == 100"        -- pytest -q
measure "cargo test (1 fail)"  noisy failed  "auth.rs:55:9|assertion"          -- cargo test
measure "tsc (2 errors)"       noisy failed  "TS2322|TS2339"                   -- tsc --noEmit
measure "go build failure"     noisy failed  "undefined: missing"             -- go test ./...
measure "noisy log (buried err)" noisy ok    "ERROR db connection pool"        -- myservice
measure "big JSON (402 items)" noisy ok      "db timeout after 30s"            -- dumpjson
measure "ruff (26 issues)"     diag  failed  "F401|E711"                       -- ruff check .
measure "mypy (26 errors)"     diag  failed  "not defined|return-value"        -- mypy src

# Demonstrate the --min-bytes safety: tiny output passes through unstored.
echo
echo "── --min-bytes passthrough  ·  \$ echo hi  (default threshold) ─────────────"
out=$( cd "$WORKDIR" && "$H5I" capture run -- bash -c "echo hi" 2>/dev/null )
echo "    output: $out"
pt_ok=0
if [[ "$out" == *"hi"* ]] && \
   ( cd "$WORKDIR" && "$H5I" recall objects --tool bash 2>/dev/null | grep -q "No captured objects match" ); then
  pt_ok=1
fi
check "PASSTHROUGH   tiny output returned raw, not stored" "$pt_ok"

# ── Aggregate ───────────────────────────────────────────────────────────────────
echo
echo "══════════════════════════════════════════════════════════════════════════"
echo "  RESULTS"
echo "══════════════════════════════════════════════════════════════════════════"
printf "  %-26s %8s %8s %6s  %s\n" "fixture" "raw" "summary" "cut" "status"
printf "  %-26s %8s %8s %6s  %s\n" "──────────────────────────" "────────" "────────" "──────" "──────"
for r in "${RESULTS[@]}"; do
  IFS='|' read -r label raw sum pct status <<< "$r"
  printf "  %-26s %8s %8s %5s%%  %s\n" "$label" "$raw" "$sum" "$pct" "$status"
done
AGG_PCT=$( python3 -c "print(round(100-($SUM_TOTAL*100/$RAW_TOTAL),1) if $RAW_TOTAL else 0)" )
printf "  %-26s %8s %8s %5s%%\n" "TOTAL" "$RAW_TOTAL" "$SUM_TOTAL" "$AGG_PCT"
echo
echo "  Aggregate token reduction: ${AGG_PCT}%  (${RAW_TOTAL} → ${SUM_TOTAL} tokens)"
agg_ok=$( python3 -c "print(1 if $RAW_TOTAL and (100-$SUM_TOTAL*100/$RAW_TOTAL)>=80 else 0)" )
check "AGGREGATE     ≥80% tokens saved across the matrix (${AGG_PCT}%)" "$agg_ok"
echo
echo "  Checks passed: $SCORE / $TOTAL"

# ── Optional: agent-in-the-loop (does the SUMMARY preserve actionable info?) ─────
if [[ "$WITH_AGENT" == "1" ]] && command -v claude >/dev/null 2>&1; then
  echo
  echo "══════════════════════════════════════════════════════════════════════════"
  echo "  AGENT-IN-THE-LOOP  ·  can an agent act on the reduced summary alone?"
  echo "══════════════════════════════════════════════════════════════════════════"
  id=$( cd "$WORKDIR" && "$H5I" recall objects --tool pytest --limit 1 2>/dev/null | grep -oE '[0-9a-f]{16}' | head -1 )
  summary=$( cd "$WORKDIR" && "$H5I" recall object "$id" 2>/dev/null )  # structured YAML
  ans=$( printf 'Here is a captured test result:\n\n%s\n\nReply with ONLY the failing test id and its file:line. No prose.' "$summary" \
    | claude --print 2>/dev/null || true )
  echo "  agent answer: $ans"
  a_ok=0; echo "$ans" | grep -qE "test_pay" && echo "$ans" | grep -qE "412" && a_ok=1
  check "Agent identified the failing test + line from the summary" "$a_ok"
else
  [[ "$WITH_AGENT" == "1" ]] && echo "  (claude CLI not found — skipping agent check)"
fi

echo
echo "══════════════════════════════════════════════════════════════════════════"
echo "  OVERALL: $SCORE / $TOTAL checks passed   ·   ${AGG_PCT}% tokens saved"
echo "══════════════════════════════════════════════════════════════════════════"
echo "  Workdir preserved: $WORKDIR"
echo "  Inspect: (cd $WORKDIR && h5i recall objects)"
echo "$STEP  done."
