#!/usr/bin/env python3
"""Plot GitHub release downloads along the release timeline.

Assumes a version is only downloaded while it is the latest release, so its
download count lands on the date the next version replaced it. The current
latest version is plotted at today.

    python3 scripts/release_downloads.py [--repo OWNER/NAME] [--out FILE]
"""

import argparse
import datetime as dt
import subprocess

import matplotlib.dates as mdates
import matplotlib.pyplot as plt

JQ = r"""
.[] | select(.draft | not) | .tag_name as $tag | .published_at as $pub |
[.assets[] | select(.name | (endswith(".tar.gz") or endswith(".zip") or endswith(".whl")))
           | .download_count] | add // 0 |
[$tag, $pub, .] | @tsv
"""


def fetch(repo):
    out = subprocess.run(
        ["gh", "api", "--paginate", f"repos/{repo}/releases?per_page=100", "--jq", JQ],
        check=True, capture_output=True, text=True,
    ).stdout
    releases = []
    for line in out.splitlines():
        tag, published, downloads = line.split("\t")
        releases.append((dt.date.fromisoformat(published[:10]), tag, int(downloads)))
    return sorted(releases)


def plot(releases, out, repo):
    # Version i's downloads are plotted when version i+1 is released.
    xs = [r[0] for r in releases[1:]] + [dt.date.today()]
    tags = [tag for _, tag, _ in releases]
    ys = [n for _, _, n in releases]

    plt.style.use("ggplot")
    fig, ax = plt.subplots(figsize=(12, 6))
    ax.plot(xs[:-1], ys[:-1], marker="o", markersize=5, linewidth=2, color="C0")
    # The latest version is still collecting downloads: dashed, hollow marker.
    ax.plot(xs[-2:], ys[-2:], linestyle="--", linewidth=1.5, color="C0")
    ax.plot(xs[-1], ys[-1], marker="o", markersize=6, markerfacecolor="white", color="C0")
    for x, y, tag in zip(xs, ys, tags):
        ax.annotate(tag, (x, y), xytext=(0, 6), textcoords="offset points",
                    ha="left", fontsize=7, color="#555555", rotation=60)

    ax.set_title(f"{repo} downloads per version (.tar.gz / .zip / .whl)")
    ax.set_xlabel("Date the version was superseded (latest: today)")
    ax.set_ylabel("Downloads")
    ax.xaxis.set_major_formatter(mdates.DateFormatter("%m/%d"))
    ax.set_ylim(bottom=0, top=max(ys) * 1.2)
    fig.autofmt_xdate()
    fig.tight_layout()
    fig.savefig(out, dpi=150)
    print(f"wrote {out}")


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--repo", default="h5i-dev/h5i")
    ap.add_argument("--out", default="release_downloads.png")
    args = ap.parse_args()

    releases = fetch(args.repo)
    if not releases:
        raise SystemExit(f"no releases in {args.repo}")
    plot(releases, args.out, args.repo)


if __name__ == "__main__":
    main()
