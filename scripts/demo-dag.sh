#!/usr/bin/env bash
# demo-dag.sh — seeds a realistic multi-branch reasoning DAG for screenshots.
#
# Usage:
#   cd /tmp/demo-repo && git init && h5i init
#   /path/to/h5i/scripts/demo-dag.sh
#   h5i context dag
#
# The scenario: an AI agent implementing OAuth2 token refresh with configurable
# TTL, delegating RFC research to a subagent scope, then merging findings back.

set -euo pipefail
H5I="${H5I:-h5i}"

echo "── Seeding demo DAG ─────────────────────────────────────────────────"

# ── Initialize workspace ───────────────────────────────────────────────────────
$H5I context init --goal "Implement OAuth2 token refresh with configurable TTL"

# ── Phase 1: Discovery (main branch) ──────────────────────────────────────────
sleep 1
$H5I context trace --kind OBSERVE \
  "src/auth.rs:44 — token TTL hardcoded to 3600 s, no config path found"
sleep 1
$H5I context trace --kind OBSERVE \
  "src/config.rs exists but has no [auth] section yet"
sleep 1
$H5I context trace --kind THINK \
  "configurable TTL required: prod needs 24 h, dev needs 1 h — 24× divergence"

$H5I context commit "Discovery complete" \
  --detail "TTL is hardcoded; config.rs needs an [auth] section; RFC constraints unknown"

# ── Phase 2: Subagent scope — RFC 6749 research ───────────────────────────────
$H5I context scope investigate-rfc \
  --purpose "verify RFC 6749 constraints on refresh token lifetime and rotation"

sleep 1
$H5I context trace --kind OBSERVE \
  "RFC 6749 §6: refresh_token lifetime MUST NOT exceed original access_token TTL"
sleep 1
$H5I context trace --kind OBSERVE \
  "OAuth 2.0 Security BCP §4.14: rotate refresh tokens on each use to detect replay"
sleep 1
$H5I context trace --kind THINK \
  "safe default: refresh_ttl = token_ttl / 2; always rotate on use — no config needed"

$H5I context commit "RFC research complete" \
  --detail "refresh window = TTL/2; rotation mandatory per BCP §4.14"

# ── Phase 3: Back to main — implementation ────────────────────────────────────
$H5I context checkout main

sleep 1
$H5I context trace --kind THINK \
  "add config.token_ttl_secs + config.refresh_ttl_secs; default refresh = TTL/2"
sleep 1
$H5I context trace --kind ACT \
  "config.token_ttl_secs and config.refresh_ttl_secs added · src/config.rs:18–21"
sleep 1
$H5I context merge scope/investigate-rfc

sleep 1
$H5I context trace --kind THINK \
  "middleware should auto-refresh 60 s before window closes, rotate token transparently"
sleep 1
$H5I context trace --kind ACT \
  "src/auth/middleware.rs — auto-refresh interceptor implemented; rotates on every use"
sleep 1
$H5I context trace --kind ACT \
  "src/auth.rs:44 — replaced hardcoded 3600 with config.token_ttl_secs"

$H5I context commit "Token refresh implemented" \
  --detail "configurable TTL, refresh=TTL/2, rotation on use, middleware auto-refresh"

echo ""
echo "── Done. Running: h5i context dag ───────────────────────────────────"
echo ""
$H5I context dag
