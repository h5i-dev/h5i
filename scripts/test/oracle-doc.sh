#!/bin/sh
set -eu

status="$(jq -r '.responses.attack.status' "$H5I_TEST_RESULT")"
body="$(jq -r '.responses.attack.body' "$H5I_TEST_RESULT")"
expected="${H5I_EXPECT_DOC_STATUS:-403}"

test "$status" = "$expected"
grep -Fq 'not yours' "$H5I_TEST_ARTIFACTS/$body"
