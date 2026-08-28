#!/usr/bin/env bash
# Is there a *published* boa we could use instead of the pinned revision?
#
# h5i-browser-light depends on boa by git revision because every released version pins
# `icu_normalizer`, `icu_properties` and `icu_segmenter` to `~2.0.0`, which
# excludes the versions parley requires — and parley arrives through blitz, so
# it is not ours to choose. Upstream relaxed those pins after 0.21.1, so the
# only boa that works here is an unreleased one.
#
# That is a thing to undo, not a thing to keep, and "it is on the list" is how a
# temporary patch becomes permanent. This asks crates.io the question instead:
# the day a release ships whose icu requirements overlap parley's, this fails
# and says so. Until then it is quiet.
#
# Deliberately fails rather than warns. It fires once, it means the patch can be
# deleted, and a warning in a log nobody reads is the same as no check at all.
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
ua='User-Agent: h5i-boa-release-check'

# The check exists to force the git pin out the moment a published boa works.
# Once the pin is gone, its job is done: boa arriving from crates.io is the
# state this script was pushing toward, not something to fail about.
if ! grep -q 'boa-dev/boa' "$here/crates/h5i-browser-light/Cargo.toml"; then
  echo "boa is a plain crates.io dependency; the workaround this check guarded is gone."
  exit 0
fi

# What parley actually needs, read from the lockfile rather than assumed, so
# this stays true when blitz moves.
parley_version="$(grep -A 1 '^name = "parley"' "$here/Cargo.lock" | grep '^version' | head -1 | sed 's/.*"\(.*\)".*/\1/')"
if [ -z "$parley_version" ]; then
  echo "could not find parley in Cargo.lock; skipping"
  exit 0
fi

need="$(curl -s "https://crates.io/api/v1/crates/parley/$parley_version/dependencies" -H "$ua" \
  | python3 -c '
import json, sys, re
deps = json.load(sys.stdin).get("dependencies", [])
wanted = {}
for d in deps:
    if d["crate_id"].startswith("icu_") and d["kind"] == "normal":
        m = re.search(r"(\d+)\.(\d+)", d["req"])
        if m:
            wanted[d["crate_id"]] = (int(m.group(1)), int(m.group(2)))
print(json.dumps(wanted))')"
echo "parley $parley_version needs: $need"

# Every published boa, newest first.
versions="$(curl -s "https://crates.io/api/v1/crates/boa_engine" -H "$ua" \
  | python3 -c '
import json, sys
d = json.load(sys.stdin)
print(" ".join(v["num"] for v in d["versions"] if not v.get("yanked"))[:400])')"

# The floor is the version this engine can actually use. 0.21 is the first with
# the program-counter-to-source-position mapping that makes a page error say
# *where* it happened, and older releases predate the icu dependency entirely —
# so they "do not clash" while also being unusable, which is how the first
# version of this check cheerfully recommended 0.17.
MIN_MAJOR=0
MIN_MINOR=21

usable=""
for v in $versions; do
  case "$v" in *-*) continue ;; esac   # skip pre-releases
  major="${v%%.*}"; rest="${v#*.}"; minor="${rest%%.*}"
  if [ "$major" -eq "$MIN_MAJOR" ] && [ "$minor" -lt "$MIN_MINOR" ]; then
    continue
  fi
  verdict="$(curl -s "https://crates.io/api/v1/crates/boa_engine/$v/dependencies" -H "$ua" \
    | NEED="$need" python3 -c '
import json, os, re, sys
need = json.loads(os.environ["NEED"])
deps = json.load(sys.stdin).get("dependencies", [])
for d in deps:
    name = d["crate_id"]
    if name not in need or d["kind"] != "normal":
        continue
    req = d["req"]
    m = re.search(r"(\d+)\.(\d+)", req)
    if not m:
        continue
    major, minor = int(m.group(1)), int(m.group(2))
    want_major, want_minor = need[name]
    # `~x.y` allows only that minor; anything below what parley needs is a clash.
    if req.startswith("~") and (major, minor) < (want_major, want_minor):
        print(f"clash {name} {req}")
        break
else:
    print("ok")')"
  if [ "$verdict" = "ok" ]; then usable="$v"; break; fi
  echo "  boa_engine $v: $verdict"
done

if [ -n "$usable" ]; then
  echo
  echo "::error::boa_engine $usable is published and its icu requirements no longer clash with parley."
  echo "The git dependency in crates/h5i-browser-light/Cargo.toml exists only to work"
  echo "around that clash. Replace both boa lines with plain requirements:"
  echo "    boa_engine = { version = \"$usable\", default-features = false, features = [\"annex-b\"] }"
  echo "    boa_gc = \"$usable\""
  exit 1
fi

echo
echo "no published boa works with this parley yet; the pinned revision stays."
