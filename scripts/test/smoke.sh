#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
H5I_BIN="${H5I_BIN:-$ROOT/target/release/h5i}"
H5I_TEST_BIN="${H5I_TEST_BIN:-$ROOT/target/release/h5i-test}"
PORT="${1:-18183}"
OUT="$(mktemp -d)"
export XDG_STATE_HOME="$OUT/state"

python3 "$ROOT/scripts/websec/server.py" "$PORT" &
SERVER_PID=$!
trap 'kill "$SERVER_PID" 2>/dev/null || true; rm -rf "$OUT"' EXIT

for _ in $(seq 1 50); do
  curl -fsS "http://127.0.0.1:$PORT/" >/dev/null 2>&1 && break
  sleep 0.05
done

"$H5I_BIN" plugin install test --from "$H5I_TEST_BIN" >/dev/null

"$H5I_BIN" test "$ROOT/scripts/test/cases" \
  --target "http://127.0.0.1:$PORT" \
  --openapi "$ROOT/scripts/test/openapi.yaml" \
  --output "$OUT/result"

jq -e '.status == "pass" and .coverage.percent == 50' "$OUT/result/result.json" >/dev/null
test -s "$OUT/result/junit.xml"

set +e
H5I_EXPECT_DOC_STATUS=404 "$H5I_BIN" test "$ROOT/scripts/test/cases" \
  --target "http://127.0.0.1:$PORT" \
  --output "$OUT/failed" >/dev/null
FAILED=$?
set -e
test "$FAILED" = 1
jq -e '.status == "fail" and .failed == 1' "$OUT/failed/result.json" >/dev/null

set +e
"$H5I_BIN" test "$ROOT/scripts/test/cases" \
  --target "http://127.0.0.1:$PORT" \
  --openapi "$ROOT/scripts/test/openapi.yaml" \
  --min-coverage 60 \
  --output "$OUT/gated" >/dev/null
GATED=$?
set -e
test "$GATED" = 1

echo "h5i test smoke: pass"
