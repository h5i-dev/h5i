#!/usr/bin/env bash
# Publish the h5i-app library crates to crates.io, in dependency order.
#
#   scripts/app/publish-crates.sh            # dry run: package and build every crate
#   scripts/app/publish-crates.sh --publish  # upload for real (needs `cargo login`)
#
# Before anything else it checks that every publishable h5i-app crate is
# either in CRATES or in SKIP, so a new crate cannot be left out silently,
# and that they all share one version. Crates already on crates.io at that
# version are left out, so a publish that failed halfway can be re-run.
#
# All crates go through one `cargo publish` call: cargo verifies them
# together (a crate may depend on one not yet on crates.io) and uploads them
# in dependency order, waiting for each to reach the index. Builds go to a
# temporary target directory, never target/debug.
set -euo pipefail
cd "$(dirname "$0")/../.."

# Dependency order: each crate after everything it depends on.
CRATES=(
  h5i-app-std
  h5i-app-sql
  h5i-app-schema
  h5i-app-token
  h5i-app-json
  h5i-app-h1
  h5i-app-core
  h5i-app-pgsql
  h5i-app-pg
  h5i-app-http
  h5i-app-embedded
  h5i-app
)
# Publishable in Cargo.toml but deliberately not released.
SKIP=(
  h5i-app-mem
  h5i-app-gpu
)

mode=dry-run
case "${1:-}" in
  "") ;;
  --publish) mode=publish ;;
  *) echo "usage: $0 [--publish]" >&2; exit 2 ;;
esac

die() { echo "publish-crates: $*" >&2; exit 1; }

# cargo publish with several -p needs cargo 1.90 or later.
cargo_minor=$(cargo --version | sed -E 's/^cargo 1\.([0-9]+).*/\1/')
[ "$cargo_minor" -ge 90 ] || die "cargo 1.90 or later is needed ($(cargo --version))"

meta=$(cargo metadata --no-deps --format-version 1)

# Every h5i-app crate whose `publish` allows crates.io, with its version.
mapfile -t workspace < <(python3 -c '
import json, sys
for p in json.load(sys.stdin)["packages"]:
    if p["name"].startswith("h5i-app") and p["publish"] != []:
        print(p["name"], p["version"])
' <<<"$meta")

declare -A version
for line in "${workspace[@]}"; do version[${line% *}]=${line#* }; done

listed=" ${CRATES[*]} ${SKIP[*]} "
for name in "${!version[@]}"; do
  [[ $listed == *" $name "* ]] || die "$name is publishable but in neither CRATES nor SKIP"
done
for name in "${CRATES[@]}"; do
  [ -n "${version[$name]:-}" ] || die "$name is in CRATES but not a publishable workspace crate"
done

v=${version[${CRATES[0]}]}
for name in "${CRATES[@]}"; do
  [ "${version[$name]}" = "$v" ] || die "$name is ${version[$name]}, expected $v"
done

if [ "$mode" = publish ]; then
  [ -z "$(git status --porcelain)" ] || die "the working tree is not clean"
  branch=$(git branch --show-current)
  [ "$branch" = main ] || echo "publish-crates: warning: publishing from $branch, not main" >&2
fi

# Leave out what crates.io already has at this version.
todo=()
for name in "${CRATES[@]}"; do
  status=$(curl -s -o /dev/null -w '%{http_code}' -A "h5i-publish-crates" \
    "https://crates.io/api/v1/crates/$name/$v")
  case "$status" in
    200) echo "  $name $v: already on crates.io, skipped" ;;
    404) todo+=("$name") ;;
    *) die "crates.io answered $status for $name $v" ;;
  esac
done
[ ${#todo[@]} -gt 0 ] || { echo "Every crate is already on crates.io at $v."; exit 0; }

echo "Crates to $mode at $v: ${todo[*]}"
args=()
for name in "${todo[@]}"; do args+=(-p "$name"); done

if [ -z "${CARGO_TARGET_DIR:-}" ]; then
  CARGO_TARGET_DIR=$(mktemp -d)
  trap 'rm -rf "$CARGO_TARGET_DIR"' EXIT
fi
export CARGO_TARGET_DIR

if [ "$mode" = dry-run ]; then
  # A dry run may check uncommitted changes; --publish insists on a clean tree.
  cargo publish --dry-run --allow-dirty "${args[@]}"
  echo "Dry run passed. Upload with: $0 --publish"
else
  read -r -p "Upload ${#todo[@]} crates at $v to crates.io? This cannot be undone. [y/N] " ok
  [ "$ok" = y ] || die "cancelled"
  cargo publish "${args[@]}"
  echo "Published. Tag the release: git tag v$v && git push origin v$v"
fi
