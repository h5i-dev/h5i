#!/usr/bin/env python3
"""Stamp `docs/` asset links with a hash of the file they point at.

`docs/` is published verbatim: no bundler, no build step, so `_static/blog.css`
is a stable URL whose *contents* change. GitHub Pages serves it with
`cache-control: max-age=600`, so anyone who visited in the last ten minutes gets
the new HTML against their cached copy of the old stylesheet. The console figure
on the front page then renders as a flat column of unstyled text, which reads
exactly like a broken page rather than like a caching artefact — it cost three
rounds of "it looks corrupted" before the cause was found.

Content hashing is the standard fix and normally a bundler's job. There is no
bundler here on purpose, so this does the one part that matters: every
`/_static/x.css` reference carries `?v=<hash of x.css>`. Change the file and
every page pointing at it changes with it, so the cached copy is never consulted
for the new URL. Leave the file alone and the stamp is byte-identical, which is
what lets CI diff it.

Run after `build-content.py`, which regenerates the pages this rewrites:

    python3 docs/build-content.py && python3 scripts/stamp_assets.py
"""

import hashlib
import re
import sys
from pathlib import Path

DOCS = Path(__file__).resolve().parent.parent / "docs"
# Long enough that a collision is not a practical concern, short enough that the
# URLs stay readable in `view-source`.
HASH_LEN = 10
# Only what a page can cache and get wrong. Images are content-addressed by
# their own names in practice, and a stale image is a cosmetic problem rather
# than a page that cannot lay itself out.
STAMPED = (".css", ".js")

LINK = re.compile(
    r'(?P<attr>href|src)="(?P<path>/_static/(?P<name>[^"?]+\.(?:css|js)))(?:\?v=[^"]*)?"'
)


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()[:HASH_LEN]


def main() -> int:
    if not DOCS.is_dir():
        print(f"no docs directory at {DOCS}", file=sys.stderr)
        return 2

    stamps: dict[str, str] = {}
    for asset in sorted(DOCS.joinpath("_static").iterdir()):
        if asset.suffix in STAMPED and asset.is_file():
            stamps[asset.name] = digest(asset)

    missing: set[str] = set()
    changed = 0

    def stamp(match: re.Match[str]) -> str:
        name = match["name"]
        if name not in stamps:
            # A reference to something that is not there is a broken page, and
            # silently leaving it unstamped would hide that.
            missing.add(name)
            return match[0]
        return f'{match["attr"]}="{match["path"]}?v={stamps[name]}"'

    for page in sorted(DOCS.rglob("*.html")):
        if "node_modules" in page.parts:
            continue
        before = page.read_text()
        after = LINK.sub(stamp, before)
        if after != before:
            page.write_text(after)
            changed += 1

    if missing:
        for name in sorted(missing):
            print(f"::error::docs reference /_static/{name}, which does not exist", file=sys.stderr)
        return 1

    print(f"stamped {len(stamps)} assets across {changed} changed page(s)")
    for name, value in sorted(stamps.items()):
        print(f"  {name:24} {value}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
