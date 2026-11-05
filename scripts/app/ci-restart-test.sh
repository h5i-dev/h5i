#!/usr/bin/env bash
# Restart the test database under load (h5i-app-pg tests/restart.rs, A6).
# It restarts the server, so it runs alone, after ci-rust-tests.sh. Needs
# H5I_APP_TEST_DATABASE_URL, H5I_APP_TEST_ADMIN_URL and
# H5I_APP_TEST_RESTART_CMD (a command that restarts that server, e.g.
# `docker restart <container>`). Fails if the test skipped or did not run.
set -uo pipefail
: "${H5I_APP_TEST_DATABASE_URL:?set H5I_APP_TEST_DATABASE_URL}"
: "${H5I_APP_TEST_ADMIN_URL:?set H5I_APP_TEST_ADMIN_URL}"
: "${H5I_APP_TEST_RESTART_CMD:?set H5I_APP_TEST_RESTART_CMD}"
export H5I_APP_TEST_REQUIRE_DB=1
log=$(mktemp)
root=$(cd "$(dirname "$0")/../.." && pwd)
(cd "$root" && cargo test --locked -p h5i-app-pg --features testing --test restart "$@" -- --ignored --nocapture 2>&1) | tee "$log"
status=${PIPESTATUS[0]}
if grep -q "skipping" "$log" || ! grep -q "test database_restart_under_load ... ok" "$log"; then
  echo "error: the database restart test skipped or did not run"
  exit 1
fi
exit "$status"
