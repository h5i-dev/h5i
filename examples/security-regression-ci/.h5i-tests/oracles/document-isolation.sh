#!/bin/sh
set -eu

status="$(jq -r '.responses.attack.status' "$H5I_TEST_RESULT")"
body="$(jq -r '.responses.attack.body' "$H5I_TEST_RESULT")"
forbidden_marker="${H5I_FORBIDDEN_MARKER:?set H5I_FORBIDDEN_MARKER}"

case "$status" in
  403|404) ;;
  *) exit 1 ;;
esac

if grep -Fq "$forbidden_marker" "$H5I_TEST_ARTIFACTS/$body"; then
  exit 1
fi
