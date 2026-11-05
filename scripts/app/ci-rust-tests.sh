#!/usr/bin/env bash
# Run the h5i-app tests (the framework crates, then the examples workspace)
# against Postgres. A database test skips when its URL is missing, but with
# H5I_APP_TEST_REQUIRE_DB set (here, always) it fails instead
# (`h5i_app_pg::test_env`): libtest hides what passing tests print, so a
# printed skip alone would go unseen. Extra arguments go to every `cargo test`, e.g. `--release`
# on a machine that cannot spare the space for this repository's dev build.
set -uo pipefail
: "${H5I_APP_TEST_DATABASE_URL:?set H5I_APP_TEST_DATABASE_URL}"
# A superuser on the same server: h5i-app-pg's fault tests (tests/faults.rs)
# terminate backends and read every backend's locks with it.
: "${H5I_APP_TEST_ADMIN_URL:?set H5I_APP_TEST_ADMIN_URL}"
export H5I_APP_TEST_REQUIRE_DB=1
log=$(mktemp)
root=$(cd "$(dirname "$0")/../.." && pwd)
status=0
# Only the h5i-app packages of the root workspace: the rest of it is the h5i
# CLI and its browser engine, which test.yaml covers.
(cd "$root" && cargo test --locked -p 'h5i-app*' --all-features "$@" 2>&1) | tee -a "$log"
[ "${PIPESTATUS[0]}" -eq 0 ] || status=1
(cd "$root/examples/app" && cargo test --workspace --locked "$@" 2>&1) | tee -a "$log"
[ "${PIPESTATUS[0]}" -eq 0 ] || status=1
# A second net, for a test that skips some other way and says so.
if grep -q "skipping" "$log"; then
  echo "error: some database tests skipped"
  exit 1
fi
exit "$status"
