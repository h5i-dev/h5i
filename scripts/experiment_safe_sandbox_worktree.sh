#!/usr/bin/env bash
# experiment_safe_sandbox_worktree.sh — Compare plain git worktree with h5i env.
#
# Hypothesis:
#   A plain git worktree is not a sandbox. An agent does NOT need to misbehave to
#   cause damage — it only needs to run the project's OWN tooling. Most projects
#   have a build/publish step that writes OUTSIDE the source tree (a sibling
#   dist/, a web root, ~/.cache). Run from a worktree, that write still lands
#   outside it and can overwrite untracked files git never sees.
#
#   `h5i env` is a sandbox-style worktree: with an enforceable process tier, the
#   same build's write outside $WORK is denied and captured as env evidence.
#
# This script exercises the security MECHANISM deterministically: it runs the
# project's build.sh directly (what an agent would do when asked to "build the
# site"). The companion workflow doc additionally shows, with a headless agent,
# that an agent runs build.sh unprompted.
#
# Usage:
#   ./scripts/experiment_safe_sandbox_worktree.sh
#
# Environment:
#   H5I_BIN — h5i binary path (default: h5i)
#   WORKDIR — temp root (default: /tmp/h5i-safe-worktree-$$)

set -uo pipefail

H5I="${H5I_BIN:-h5i}"
# NOT under /tmp on purpose. The agent-in-box profile (auto-selected at the
# supervised/container tiers) grants host-shared /tmp as scratch, so a precious
# file in /tmp would be writable from the box and the demo would fail on those
# tiers. A dedicated dir under $HOME is granted by NEITHER the `default` nor the
# `agent` profile (only specific ~/.cache, ~/.npm, ~/.claude subpaths are), so
# the out-of-$WORK write is denied across process AND supervised.
WORKDIR="${WORKDIR:-$HOME/h5i-worktree-experiment}"
ISOLATION="${ISOLATION:-process}"       # process | supervised | container | auto
REPO="$WORKDIR/repo"
PLAIN="$WORKDIR/plain-agent"
PUBLISHED="$WORKDIR/published"          # the shared publish dir, OUTSIDE the repo

HOMEPAGE='<h1>MY REAL HOMEPAGE</h1><p>hand-written, not in git, DO NOT LOSE</p>'

case "$H5I" in
  */*) H5I="$(cd "$(dirname "$H5I")" && pwd)/$(basename "$H5I")" ;;
esac

pass() { printf 'PASS: %s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
note() { printf 'NOTE: %s\n' "$*"; }
run()  { printf '+ %s\n' "$*"; "$@"; }

# Safety: WORKDIR is rm -rf'd. Refuse anything that isn't a dedicated subdir.
case "$WORKDIR" in
  ""|"/"|"$HOME"|"$HOME/") fail "refusing to use WORKDIR='$WORKDIR' (would wipe \$HOME or /)";;
  */h5i-*) : ;;
  *) fail "refusing WORKDIR='$WORKDIR': pick a dedicated dir whose name contains 'h5i-'";;
esac

rm -rf "$WORKDIR"
mkdir -p "$REPO/src"

# ── an ordinary project: a static-site generator whose publish step writes to a
#    sibling publish directory (../published), outside the repo, by design ──────
run git init -b main "$REPO" >/dev/null
run git -C "$REPO" config user.name "h5i sandbox experiment"
run git -C "$REPO" config user.email "sandbox@h5i.test"

printf '# Welcome\n\nHello from the site.\n' > "$REPO/src/index.md"

cat > "$REPO/build.sh" <<'EOF'
#!/bin/sh
# Render src/ and PUBLISH to the shared publish directory
# (a sibling of the repo, served by the local dev server).
set -e
mkdir -p ../published
echo "<h1>Welcome</h1><p>Hello from the site.</p>" > ../published/index.html
echo "published -> ../published/index.html"
EOF
chmod +x "$REPO/build.sh"

cat > "$REPO/README.md" <<'EOF'
# tiny-site
## Build & publish
    ./build.sh
Renders src/ and publishes the site to the shared publish directory.
EOF

run git -C "$REPO" add .
run git -C "$REPO" commit -m "seed: tiny static site generator" >/dev/null

# ── the developer's real, hand-written homepage: untracked, no backup ─────────
mkdir -p "$PUBLISHED"
printf '%s\n' "$HOMEPAGE" > "$PUBLISHED/index.html"

printf '\n== Plain git worktree: the build escapes onto an outside file ==\n'
# A worktree is placed as an ordinary directory (here a sibling of published/),
# so build.sh's ../published resolves straight onto the real homepage.
run git -C "$REPO" worktree add -b plain-agent "$PLAIN" >/dev/null
( cd "$PLAIN" && ./build.sh )

if grep -qF 'MY REAL HOMEPAGE' "$PUBLISHED/index.html"; then
  fail "plain worktree did not overwrite the outside homepage as expected"
fi
pass "plain worktree build overwrote the outside homepage: $PUBLISHED/index.html"
if [ -z "$(git -C "$PLAIN" status --short)" ]; then
  pass "git is blind to the damage: worktree status is clean"
else
  note "worktree status not clean (unexpected, but not fatal)"
fi

# reset the homepage for the h5i arm
printf '%s\n' "$HOMEPAGE" > "$PUBLISHED/index.html"

printf '\n== h5i env: the same build is denied outside $WORK ==\n'
if ! command -v "$H5I" >/dev/null 2>&1; then
  note "h5i binary not found: $H5I"
  note "set H5I_BIN=/path/to/h5i and rerun"
  exit 0
fi

printf '+ cd %s && %s init\n' "$REPO" "$H5I"
(cd "$REPO" && "$H5I" init >/dev/null)
printf '+ %s env create safe --isolation %s\n' "$H5I" "$ISOLATION"
if ! (cd "$REPO" && H5I_AGENT=experiment "$H5I" env create safe \
  --isolation "$ISOLATION" >/tmp/h5i-safe-worktree-create.out 2>&1
)
then
  note "isolation tier '$ISOLATION' is not available on this host; h5i env arm skipped"
  note "create output:"
  sed 's/^/  /' /tmp/h5i-safe-worktree-create.out
  exit 0
fi

set +e
(cd "$REPO" && H5I_AGENT=experiment "$H5I" env run safe -- ./build.sh \
  >/tmp/h5i-safe-worktree-run.out 2>&1)
rc=$?
set -e

if grep -qF 'MY REAL HOMEPAGE' "$PUBLISHED/index.html"; then
  pass "h5i env preserved the outside homepage: $PUBLISHED/index.html"
else
  fail "h5i env allowed the outside homepage to be overwritten"
fi

if [ "$rc" -eq 0 ]; then
  fail "h5i env build command unexpectedly succeeded"
fi
pass "h5i env denied the outside write (build exited $rc)"

if grep -qiF 'permission denied' /tmp/h5i-safe-worktree-run.out; then
  pass "denial was a kernel permission error (not a soft failure)"
fi
note "denied build reported:"
sed 's/^/  /' /tmp/h5i-safe-worktree-run.out
note "evidence: cd $REPO && $H5I recall objects --env safe"
