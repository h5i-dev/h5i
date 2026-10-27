"""Build the hand-written guides and essays into the static docs tree.

Rewrites every page it owns, which drops the `?v=` cache-busting stamps on
`_static` links, then re-stamps them before exiting. Running this on its own
leaves a consistent tree.
"""

from datetime import datetime, timezone
from pathlib import Path
import hashlib
import json
import re
import shutil
import subprocess
import sys

ROOT = Path(__file__).parent
sys.path.insert(0, str(ROOT))
from guide_hubs import GUIDES
from blog_hubs import BLOG

# When the documentation rewrite published this set of pages. A real
# publication date, so it is genuinely a constant.
PUBLISHED = "2026-08-21"

# The last date each published page's content changed, beside a fingerprint of
# what it contained on that date.
#
# `lastmod` and `dateModified` are worth publishing only if they move when the
# page moves, and a single hand-maintained date does not: this file pinned
# every date on the site to the publication date and stayed there through
# thirteen commits, telling every crawler nothing had ever changed. It cannot
# be derived from git either, because CI regenerates this tree from a shallow
# checkout and byte-compares the result, so a date read from history would
# differ from the committed one and fail the build.
#
# So the date is written down, and `verify_dates()` re-derives every
# fingerprint at the end of the build and refuses to finish if one moved
# without its date. Changing a page's content and forgetting the date is a
# build error, not a silent regression.
PAGE_HISTORY = {
    "": ("2026-10-23", "f94508fae7179efa"),
    "features/": ("2026-10-23", "6aa05fb9e51b2e6e"),
    "manual/": ("2026-10-27", "1748c16695d291d8"),
    "pitch/": ("2026-10-23", "e226e08efc54cc1a"),
    "demo/": ("2026-10-22", "9dae5fe7a3dbf01b"),
    "guides/": ("2026-10-23", "7c6c8d7fc6200fbc"),
    "guides/build-and-verify-a-web-app/": ("2026-10-23", "43f4cde959997b02"),
    "guides/red-team-a-web-app/": ("2026-10-21", "3af532e9ff5592f3"),
    "guides/advanced-web-security-testing/": ("2026-10-21", "ee29cf39a56e07d5"),
    "guides/security-regression-testing/": ("2026-10-21", "b7a8f9ea68eea371"),
    "guides/sandboxed-agent-workflows/": ("2026-10-21", "98e554bde6b6bd58"),
    "blog/": ("2026-10-21", "6b670194b9694f37"),
    "blog/building-a-verifiable-web-framework/": ("2026-10-21", "fba684d83eaebd67"),
    "blog/finding-bugs-verifying-rust-oss/": ("2026-10-21", "ee2d74b97bf0df5f"),
    "blog/ai-powered-web-security-testing/": ("2026-10-21", "a380de1c91db2206"),
    "blog/sandboxing-ai-agents/": ("2026-10-21", "2b9ca59124b152ef"),
    "blog/auditing-agent-workflows/": ("2026-10-21", "969ae1325375f6d0"),
}

# The pages this script does not write. They are fingerprinted off disk.
HAND_WRITTEN = ("", "features/", "manual/", "pitch/", "demo/")

_VOLATILE = (
    re.compile(r"\?v=[0-9a-f]+"),                                  # asset stamps
    re.compile(r"\d{4}-\d{2}-\d{2}"),                              # any ISO date
    re.compile(r"\w{3}, \d{2} \w{3} \d{4} \d{2}:\d{2}:\d{2} GMT"),  # any RSS date
)


def fingerprint(html):
    """A page's content hash, blind to the things that are not its content.

    Asset stamps move whenever a stylesheet does and dates move whenever this
    guard fires, so hashing either would make the check either too loud or
    self-triggering.
    """
    for pattern in _VOLATILE:
        html = pattern.sub("", html)
    return hashlib.sha256(html.encode()).hexdigest()[:16]


def modified(path):
    """The recorded last-changed date for a published page."""
    return PAGE_HISTORY[path][0]


def verify_dates(generated):
    """Refuse to finish a build that changed a page without dating the change."""
    today = datetime.now(timezone.utc).date().isoformat()
    seen = dict(generated)
    for path in HAND_WRITTEN:
        seen[path] = (ROOT / path / "index.html").read_text()

    stale = []
    for path, html in sorted(seen.items()):
        recorded_date, recorded_hash = PAGE_HISTORY[path]
        actual = fingerprint(html)
        if actual != recorded_hash:
            stale.append((path, recorded_date, actual))
    if not stale:
        return

    print("docs: page content changed without a date to go with it.\n", file=sys.stderr)
    print("Update PAGE_HISTORY in docs/build-content.py, then rebuild:\n", file=sys.stderr)
    for path, recorded_date, actual in stale:
        was = "" if recorded_date == today else f"   # was {recorded_date}"
        print(f'    "{path}": ("{today}", "{actual}"),{was}', file=sys.stderr)
    print(f"\nThese dates become <lastmod> in sitemap.xml and dateModified in the", file=sys.stderr)
    print("page schema, so they have to describe the content actually shipping.", file=sys.stderr)
    sys.exit(1)


def rfc822(day):
    """A YYYY-MM-DD date as the RFC-822 stamp RSS requires."""
    stamp = datetime.strptime(day, "%Y-%m-%d").replace(hour=12, tzinfo=timezone.utc)
    return stamp.strftime("%a, %d %b %Y %H:%M:%S GMT")

NAV = """<nav class="blog-nav">
  <a class="nav-logo" href="/"><img src="/_static/logo.png" alt="h5i"><span>h5i</span></a>
  <ul class="nav-links">
    <li><a href="/features/">Features</a></li><li><a href="/guides/">Guides</a></li>
    <li><a href="/manual/">Manual</a></li><li><a href="/blog/">Blog</a></li>
    <li><a href="https://github.com/h5i-dev/h5i" class="nav-cta">GitHub &rarr;</a></li>
  </ul>
</nav>"""

FOOTER = """<footer class="blog-footer"><div class="blog-footer-inner">
  <div class="brand">h5i<span class="red"> / high-five</span></div>
  <nav class="links"><a href="/">Home</a><a href="/guides/">Guides</a><a href="/blog/">Blog</a><a href="/manual/">Manual</a><a href="https://github.com/h5i-dev/h5i">GitHub</a></nav>
  <div class="legal">Apache 2.0 &middot; Built with Rust</div>
</div></footer>
<script src="/_static/blog.js" defer></script><script src="/_static/highlight.js" defer></script>"""


# One social card for the whole generated tree, and the one sentence that
# describes it. An og:image without an og:image:alt is an unlabelled image
# everywhere the card is rendered.
SOCIAL_IMAGE = "https://h5i.dev/_static/social-card.png"
SOCIAL_ALT = "The h5i logo beside the words Agent-Native Web-Security"


def head(title, description, canonical, schema, kind="article", rss=False,
         social_image=SOCIAL_IMAGE, social_alt=SOCIAL_ALT):
    data = json.dumps(schema, indent=2, ensure_ascii=False).replace("</", "<\\/")
    feed = '<link rel="alternate" type="application/rss+xml" title="The h5i Blog" href="/feed.xml">' if rss else ""
    return f"""<!DOCTYPE html><html lang="en"><head>
<meta charset="UTF-8"><meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>{title}</title><meta name="description" content="{description}">
<meta name="author" content="h5i-dev"><meta name="theme-color" content="#D21C1C">
<meta name="color-scheme" content="dark"><meta name="robots" content="index, follow, max-image-preview:large">
<link rel="canonical" href="{canonical}">{feed}<link rel="icon" type="image/png" href="/_static/logo.png">
<meta property="og:type" content="{kind}"><meta property="og:site_name" content="h5i">
<meta property="og:title" content="{title}"><meta property="og:description" content="{description}">
<meta property="og:url" content="{canonical}"><meta property="og:image" content="{social_image}">
<meta property="og:image:alt" content="{social_alt}"><meta property="og:locale" content="en_US">
<meta name="twitter:card" content="summary_large_image"><meta name="twitter:title" content="{title}">
<meta name="twitter:description" content="{description}"><meta name="twitter:image" content="{social_image}">
<meta name="twitter:image:alt" content="{social_alt}">
<script type="application/ld+json">{data}</script>
<link rel="preconnect" href="https://fonts.googleapis.com"><link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link href="https://fonts.googleapis.com/css2?family=Archivo:wdth,wght@62..125,100..900&amp;family=IBM+Plex+Mono:wght@400;500;600&amp;display=swap" rel="stylesheet">
<link rel="stylesheet" href="/_static/blog.css"><link rel="stylesheet" href="/_static/highlight.css">
</head>"""


def terminal(label, text):
    # Escape the body. Every angle bracket in these blocks is a placeholder a
    # reader is meant to see (`<thread>`, `<digest>`), and an unescaped one is
    # parsed as an unknown element and rendered as nothing, which silently
    # drops the argument from a command somebody is about to copy.
    body = text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")
    return f"""<div class="terminal"><div class="terminal-bar"><span class="terminal-path">{label}</span></div>
<div class="terminal-body"><pre><code>{body}</code></pre></div></div>"""


def schema_for(item):
    url = f"https://h5i.dev/{item['section']}/{item['slug']}/"
    graph = [
        {"@type": "TechArticle", "headline": item["h1"], "description": meta_description(item),
         "author": {"@type": "Organization", "name": "h5i-dev"},
         "publisher": {"@type": "Organization", "name": "h5i"},
         "datePublished": item.get("published", PUBLISHED),
         "dateModified": modified(f"{item['section']}/{item['slug']}/"),
         "image": item.get("social_image", SOCIAL_IMAGE), "inLanguage": "en", "isPartOf": {"@id": "https://h5i.dev/#website"},
         "mainEntityOfPage": url},
        {"@type": "BreadcrumbList", "itemListElement": [
            {"@type": "ListItem", "position": 1, "name": "Home", "item": "https://h5i.dev/"},
            {"@type": "ListItem", "position": 2, "name": item["section"].title(), "item": f"https://h5i.dev/{item['section']}/"},
            {"@type": "ListItem", "position": 3, "name": item["h1"], "item": url},
        ]},
    ]
    if item.get("faq"):
        graph.append({"@type": "FAQPage", "mainEntity": [
            {"@type": "Question", "name": q, "acceptedAnswer": {"@type": "Answer", "text": a}}
            for q, a in item["faq"]
        ]})
    return {"@context": "https://schema.org", "@graph": graph}


def meta_description(item):
    """The search-snippet line for a page.

    `description` doubles as the visible card blurb and the RSS summary, where
    a long sentence is worth having. A snippet is cut around 160 characters, so
    a page whose blurb runs past that carries a `meta` line written to fit.
    """
    return item.get("meta", item["description"])


def article_page(item):
    url = f"https://h5i.dev/{item['section']}/{item['slug']}/"
    faq = "".join(
        f'<details class="faq-item"><summary>{q}</summary><div class="faq-answer">{a}</div></details>'
        for q, a in item.get("faq", ())
    )
    faq = f'<h2 id="faq">Questions that come up</h2><div class="faq-list">{faq}</div>' if faq else ""
    nxt = item["next"]
    return f"""{head(item['title'], meta_description(item), url, schema_for(item),
                    social_image=item.get('social_image', SOCIAL_IMAGE),
                    social_alt=item.get('social_alt', SOCIAL_ALT))}
<body>{NAV}<main class="article-wrap"><article class="post">
<header><div class="post-eyebrow">{item['eyebrow']} &middot; {item.get('published', PUBLISHED)}</div>
<h1>{item['h1']}</h1><p class="post-deck">{item['deck']}</p>
<div class="post-meta"><span>{item['time']} read</span><span>{item['tags']}</span></div></header>
{item['body']}
{faq}
<a class="next-up" href="{nxt[0]}"><span class="label">{nxt[1]}</span><h3>{nxt[2]}</h3><p>{nxt[3]}</p></a>
<div class="post-cta"><h3>{item['cta'][0]}</h3><p>{item['cta'][1]}</p>
<div class="hero-actions"><a class="btn btn-primary" href="{item['cta'][2]}">{item['cta'][3]}</a></div></div>
</article></main>{FOOTER}</body></html>"""


# Blog articles live in blog_hubs.py.


ARTICLES = GUIDES + BLOG


def index_page(section, items):
    guides = section == "guides"
    title = "h5i guides: build, verify, test, and sandbox web apps" if guides else "h5i Blog: framework design, formal verification, and agent security"
    description = ("Five task guides for building and verifying Rust web apps, red-teaming running apps, regression testing in CI/CD, and sandboxing agent workflows."
                   if guides else "Five engineering essays on the verifiable Rust framework, Lean 4 case studies, web security testing, agent isolation, and execution evidence.")
    # A hub heading names the section. It is the one page on the site whose
    # job is to be a label, so the argument goes in the deck.
    h1 = "h5i Guides" if guides else "h5i Blog"
    deck = ("Start by building and formally verifying a Rust web app. Then test the running application, turn confirmed findings into CI checks, and run agent work inside a reviewed sandbox."
            if guides else
            "Engineering notes on verifiable web applications, formal verification, and agent-driven security testing.")
    url = f"https://h5i.dev/{section}/"
    # A hub is a page in its own right. Without CollectionPage and a breadcrumb
    # it is the one level of the site with no trail, while every article under
    # it has one.
    schema = {"@context": "https://schema.org", "@graph": [
        {"@type": "CollectionPage", "@id": f"{url}#page", "url": url, "name": title,
         "description": description, "inLanguage": "en", "dateModified": modified(f"{section}/"),
         "isPartOf": {"@id": "https://h5i.dev/#website"},
         "about": {"@id": "https://h5i.dev/#app"}, "primaryImageOfPage": SOCIAL_IMAGE},
        {"@type": "BreadcrumbList", "@id": f"{url}#breadcrumb", "itemListElement": [
            {"@type": "ListItem", "position": 1, "name": "Home", "item": "https://h5i.dev/"},
            {"@type": "ListItem", "position": 2, "name": section.title(), "item": url},
        ]},
        {"@type": "ItemList", "@id": f"{url}#list", "name": title,
         "itemListElement": [{"@type": "ListItem", "position": i + 1, "url": f"https://h5i.dev/{section}/{x['slug']}/", "name": x["h1"]} for i, x in enumerate(items)]},
    ]}
    rows = ""
    for i, item in enumerate(items, 1):
        label = item["eyebrow"].split(" / ")[0] if guides else f"{i:02d} / {item['eyebrow'].split(' / ')[-1]}"
        rows += f"""<a class="post-card{' featured' if i == 1 else ''}" href="/{section}/{item['slug']}/">
<div class="card-meta"><span>{label}</span><span>{item['time']}</span></div>
<h2>{item['h1']}</h2><p>{item['description']}</p></a>"""
    return f"""{head(title, description, url, schema, kind="website", rss=not guides)}
<body>{NAV}<section class="index-hero"><div class="post-eyebrow">{len(items)} {"task guides" if guides else "engineering essays"}</div>
<h1>{h1}</h1><p>{deck}</p></section><section class="post-list">{rows}</section>{FOOTER}</body></html>"""


REDIRECTS = {
    "blog": {
        "ai-pentesting-tools": "ai-powered-web-security-testing",
        "burp-suite-vs-h5i-for-ai-agents": "ai-powered-web-security-testing",
        "owasp-zap-vs-h5i-for-ai-agents": "ai-powered-web-security-testing",
        "caido-vs-h5i-for-ai-agents": "ai-powered-web-security-testing",
        "the-h5i-loop": "auditing-agent-workflows",
        "the-environment-is-the-sandbox": "sandboxing-ai-agents",
        "choosing-agent-isolation": "sandboxing-ai-agents",
        "prompt-injection-is-a-boundary-problem": "sandboxing-ai-agents",
        "evidence-for-agent-work": "auditing-agent-workflows",
        "agent-sandbox-env": "sandboxing-ai-agents", "what-is-ai-aware-version-control": "sandboxing-ai-agents",
        "orchestration-patterns-beyond-ensemble": "sandboxing-ai-agents", "git-notes-vs-h5i-ai-coding-workflows": "sandboxing-ai-agents",
        "sandboxing-ai-agents-foundations": "sandboxing-ai-agents", "sandboxing-ai-agents-implementation": "sandboxing-ai-agents",
        "sandboxing-ai-agents-landscape": "sandboxing-ai-agents", "sandboxing-ai-agents-h5i": "sandboxing-ai-agents",
        "auditable-workspaces-for-ai-agents": "auditing-agent-workflows", "why-git-diffs-are-not-enough-for-ai-generated-code": "auditing-agent-workflows",
        "structured-tool-output-schema": "auditing-agent-workflows", "uncertainty-heatmap": "auditing-agent-workflows",
        "track-claude-code-prompts-diffs-git": "auditing-agent-workflows", "from-git-blame-to-ai-blame": "auditing-agent-workflows",
        "pr-body-ai-code-review": "auditing-agent-workflows", "review-code-written-by-ai-agents": "auditing-agent-workflows",
        "auditing-ai-generated-code": "auditing-agent-workflows", "prompt-injection-in-agent-traces": "sandboxing-ai-agents",
        "cve-2026-33068-bypass-permissions-settings": "sandboxing-ai-agents",
        "cve-2025-59536-startup-trust-dialog": "sandboxing-ai-agents",
        "claude-code-hooks-vs-git-hooks": "auditing-agent-workflows", "programmable-agent-orchestration-edsl": "sandboxing-ai-agents",
        "write-your-first-orchestra-score": "sandboxing-ai-agents", "context-dag-versioned-agent-reasoning": "auditing-agent-workflows",
        "persistent-memory-for-claude-code": "sandboxing-ai-agents", "token-reduction-object-store": "sandboxing-ai-agents",
        "git-communication-layer-ai-agents": "sandboxing-ai-agents", "i5h-agent-to-agent-messaging": "sandboxing-ai-agents",
        "prompt-maturity-score": "sandboxing-ai-agents", "agent-ensembles-with-h5i-team": "sandboxing-ai-agents",
        "agents-share-information-never-permissions": "sandboxing-ai-agents",
    },
    "guides": {
        "drive-a-browser-session": "red-team-a-web-app",
        "authorized-web-security-testing": "red-team-a-web-app",
        "recon-and-idor": "red-team-a-web-app",
        "broken-access-control": "red-team-a-web-app",
        "jwt-attacks": "red-team-a-web-app",
        "browser-as-a-weapon": "red-team-a-web-app",
        "sql-injection": "advanced-web-security-testing",
        "injection-beyond-sql": "advanced-web-security-testing",
        "ssrf-and-file-attacks": "advanced-web-security-testing",
        "http-protocol-attacks": "advanced-web-security-testing",
        "logic-time-and-crypto": "advanced-web-security-testing",
        "exploit-chains": "advanced-web-security-testing",
        "cheatsheet": "advanced-web-security-testing",
        "first-box": "sandboxed-agent-workflows",
        "review-a-pull-request": "sandboxed-agent-workflows",
        "write-a-box-policy": "sandboxed-agent-workflows",
        "watch-the-browser": "sandboxed-agent-workflows",
        "ai-code-review-audit": "sandboxed-agent-workflows",
        "ai-code-provenance": "sandboxed-agent-workflows",
        "secure-api-tokens-in-agent-box": "sandboxed-agent-workflows",
        "prompt-injection-detection-for-agents": "sandboxed-agent-workflows",
        "claude-code-memory": "sandboxed-agent-workflows",
        "codex-claude-code-collaboration": "sandboxed-agent-workflows",
        "git-blame-for-ai-code": "sandboxed-agent-workflows",
        "token-reduction-capture-run": "sandboxed-agent-workflows",
        "run-a-forum": "sandboxed-agent-workflows",
    },
}


# Retired top-level pages. `/workflows/` was a sixth section holding one page,
# the end-to-end loop, which is an essay and now lives in the blog as one.
TOP_REDIRECTS = {"workflows": "/blog/auditing-agent-workflows/"}


def redirect_page(target):
    # No `noindex` here, deliberately. There is no server-side 301 on a static
    # host, so an instant meta refresh plus a canonical is the only way to tell
    # a crawler this URL became that one. `noindex` would drop the old URL
    # instead of folding it into the new one, throwing away whatever the
    # retired page had earned. These stubs are kept out of the sitemap, the
    # feed, llms.txt and every index, so nothing invites a crawler to them.
    return f"""<!DOCTYPE html><html lang="en"><head><meta charset="utf-8">
<meta name="robots" content="noarchive"><link rel="canonical" href="https://h5i.dev{target}">
<meta http-equiv="refresh" content="0; url={target}"><title>Article moved | h5i</title></head>
<body><p>This page moved during the documentation rewrite. <a href="{target}">Read the page that replaced it.</a></p></body></html>"""


def build():
    generated = {}
    for section in ("blog", "guides"):
        base = ROOT / section
        # list(), not the generator: removing entries while iterating the
        # directory skips some of them, and a survivor makes the mkdir below
        # fail on a slug this build owns.
        for child in list(base.iterdir()):
            if child.is_dir():
                shutil.rmtree(child)
        selected = [item for item in ARTICLES if item["section"] == section]
        generated[f"{section}/"] = index_page(section, selected)
        (base / "index.html").write_text(generated[f"{section}/"])
        for item in selected:
            out = base / item["slug"]
            out.mkdir()
            generated[f"{section}/{item['slug']}/"] = article_page(item)
            (out / "index.html").write_text(generated[f"{section}/{item['slug']}/"])
        for old, new in REDIRECTS[section].items():
            out = base / old
            out.mkdir(exist_ok=True)
            (out / "index.html").write_text(redirect_page(f"/{section}/{new}/"))

    for old, target in TOP_REDIRECTS.items():
        out = ROOT / old
        out.mkdir(exist_ok=True)
        (out / "index.html").write_text(redirect_page(target))

    # Every page is written by now, so the recorded dates can be checked
    # against what actually shipped before they are published as `lastmod`.
    verify_dates(generated)

    core = [("", "1.0"), ("features/", "0.9"), ("manual/", "0.9"),
            ("guides/", "0.8"), ("blog/", "0.8"), ("pitch/", "0.6"), ("demo/", "0.6")]
    urls = [(path, priority, modified(path)) for path, priority in core]
    urls += [(f"{item['section']}/{item['slug']}/", "0.7", modified(f"{item['section']}/{item['slug']}/"))
             for item in ARTICLES]
    rows = "\n".join(f"  <url><loc>https://h5i.dev/{path}</loc><lastmod>{lastmod}</lastmod><priority>{priority}</priority></url>" for path, priority, lastmod in urls)
    (ROOT / "sitemap.xml").write_text(f'<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n{rows}\n</urlset>\n')

    posts = [item for item in ARTICLES if item["section"] == "blog"]
    items = "\n".join(f"""    <item><title>{item['h1']}</title><link>https://h5i.dev/blog/{item['slug']}/</link>
      <guid isPermaLink="true">https://h5i.dev/blog/{item['slug']}/</guid><pubDate>{rfc822(item.get('published', PUBLISHED))}</pubDate>
      <description>{item['description']}</description></item>""" for item in posts)
    (ROOT / "feed.xml").write_text(f"""<?xml version="1.0" encoding="UTF-8"?><rss version="2.0"><channel>
<title>The h5i Blog</title><link>https://h5i.dev/blog/</link>
<description>Engineering essays on verifiable Rust web applications, Lean 4 proofs, web security testing, and agent isolation.</description>
<language>en-us</language><lastBuildDate>{rfc822(max(modified(f"blog/{item['slug']}/") for item in posts))}</lastBuildDate>
<atom:link xmlns:atom="http://www.w3.org/2005/Atom" href="https://h5i.dev/feed.xml" rel="self" type="application/rss+xml"/>
{items}</channel></rss>""")

    (ROOT / "llms.txt").write_text("""# h5i

> h5i ("high-five") is an Axum-based Rust web framework for building web applications and formally verifying core logic with Lean 4. It includes optional GPU acceleration and development infrastructure for red-teaming, CI/CD testing, sandboxed workflows, and a local dashboard.
>
> h5i-app keeps application decisions in composable Rust kernels that can be translated into Lean 4. The h5i CLI provides project tooling, browser automation, HTTP inspection, reconnaissance, security regression testing, sandboxing, and a dashboard. Its security tools also work with existing apps that do not use h5i-app.

## Start here

- [h5i GitHub](https://github.com/h5i-dev/h5i): Source, installation, and examples.
- [h5i-app framework](https://github.com/h5i-dev/h5i/blob/main/crates/h5i-app/README.md): Build Rust web apps with verifiable kernel logic.
- [Features](https://h5i.dev/features/): Overview of the framework and development tools.
- [Guides](https://h5i.dev/guides/): Five task workflows, from Rust app development to sandboxed agent work.
- [Manual](https://h5i.dev/manual/): Commands, configuration, policies, and limitations.

## 1. Verifiable Web Framework

Use h5i-app when building Rust web applications with business rules, authorization, or data isolation properties that you want to state and prove.

- **Rust and Axum:** Keep application decisions in composable Rust kernels and connect them to HTTP handlers.
- **Formal verification:** Translate supported kernel logic into Lean 4 with Charon and Aeneas, then prove stated properties.
- **Optional GPU acceleration:** Run supported Rust decision kernels on GPUs for suitable batch workloads.

### Project Tooling

- `cargo add h5i-app` adds the framework to a Rust project; `h5i app new counter` scaffolds a new project.
- `h5i app extract` translates kernel logic to Lean 4; `h5i app prove` checks the proofs.
- `h5i app lint` checks route authentication structure; `h5i app mutate` tests whether proofs detect intentional kernel bugs.
- See the [h5i app manual](https://h5i.dev/manual/#h5i-app), [verification trust model](https://github.com/h5i-dev/h5i/blob/main/docs/app/TRUST.md), and [GPU backend report](https://github.com/h5i-dev/h5i/blob/main/docs/app/GPU.md).
- Follow the [build and verify guide](https://h5i.dev/guides/build-and-verify-a-web-app/) for the complete workflow.

## 2. Development Infrastructure

Use these tools to investigate and test running applications or to run agent work in a restricted environment. The security tools work with any web framework.

### Agent-Native Red-Teaming

- [`h5i browser`](https://h5i.dev/manual/#h5i-browser): Navigate a running app and inspect authenticated workflows.
- [`h5i recon`](https://h5i.dev/manual/#h5i-recon): Discover endpoints and map the exposed attack surface.
- [`h5i websec`](https://h5i.dev/manual/#h5i-websec): Inspect, modify, replay, and compare captured HTTP traffic.
- Start with [Red-Team a Web App](https://h5i.dev/guides/red-team-a-web-app/) or continue to [advanced testing](https://h5i.dev/guides/advanced-web-security-testing/). Use these tools only against systems you own or are explicitly authorized to test.

### CI/CD & Regression Testing

- [`h5i test`](https://h5i.dev/manual/#h5i-test): Save confirmed security findings as versioned tests, replay them in CI/CD, and check API coverage against an OpenAPI description.
- Follow the [security regression guide](https://h5i.dev/guides/security-regression-testing/) to write a test and run it against staging.

### Sandbox

- [`h5i box`](https://h5i.dev/manual/#containment-optional): Run an agent and its tools in a disposable environment with configured filesystem and network restrictions. Start with the [sandboxed workflows guide](https://h5i.dev/guides/sandboxed-agent-workflows/).

### Dashboard

- [`h5i ui`](https://h5i.dev/manual/#the-console): Inspect browser sessions, HTTP traffic, and agent activity in the local console.

## Installation

- Framework: `cargo add h5i-app`.
- CLI with security plugins: `curl -fsSL https://h5i.dev/install.sh | sh -s -- --websec --recon --test`.
- Coding agent skill: `h5i skill install` or `npx skills add h5i-dev/h5i`. See the [skill manual](https://h5i.dev/manual/#h5i-skill).

## Important limitations

- Lean 4 proofs establish stated properties of extracted kernel logic under explicit assumptions. They do not prove external database behavior, deployment infrastructure, or GPU execution correctness.
- CPU/GPU differential tests check backend behavior separately; they are not Lean proofs of GPU equivalence.
- Recon results and HTTP response differences are evidence to investigate, not automatic vulnerability findings.
- Security testing requires authorization and a restricted scope. Sandbox guarantees depend on the selected isolation tier; see the [policy manual](https://h5i.dev/manual/#policy).
""")

    (ROOT / "content-style-guide.md").write_text("""# h5i editorial guide

The documentation has four jobs. Product pages answer what h5i is. Guides help
a reader finish a task. The manual defines commands and fields. Blog essays
explain durable design choices. Do not make one page perform another layer's
job.

## Keep the collection small

A new page needs a job no existing page can do.

- Extend a guide when the reader is still pursuing the same outcome.
- Extend the manual when the material defines a command, field, or limit.
- Extend an essay when the material supports the same central claim.
- Add a page only for a genuinely different reader, outcome, or argument.

Never split one subject into a series to manufacture volume. Redirect retired
URLs to the closest replacement. Keep redirects out of indexes, feeds, the
sitemap, and llms.txt.

## Voice

h5i is confident, concrete, and honest about boundaries.

1. State the claim early.
2. Name the command, mechanism, or limitation that supports it.
3. Prefer short sentences at the moment the argument turns.
4. Use contrast when it clarifies a boundary: state versus execution,
   testimony versus observation, portability versus network enforcement.
5. Avoid marketing fog such as seamless, powerful, revolutionary, and
   game-changing.

Use h5i in lowercase. A disposable environment is a box. The security property
is a boundary or confinement. Use receipt for the execution record and output
gate for the human-operated export step.

Say host-observed for what this machine recorded and box-claimed for what the
box itself reported. Never merge the two into one label.

Do not resurrect removed product language. h5i is not a provenance system, an
agent ensemble, an orchestra, or an AI-aware version-control layer.

## Guides

A guide is imperative and outcome-shaped. It contains:

1. A short explanation of why the task needs a box.
2. An outcome callout.
3. Numbered steps with imperative headings.
4. Commands that match the current manual.
5. A check after every consequential action.
6. The security gotcha most likely to change the decision.
7. A stopping point: export, apply, or remove.
8. Links to the relevant manual section and the next guide.

Do not narrate product history in a guide. Do not hide prerequisites in the
third step. Do not show fictional output as if it came from a real run.

## Blog essays

An essay earns its place by making one durable argument:

1. Claim: one self-contained answer in the opening callout.
2. Tension: the familiar approach and the limit it reaches.
3. Mechanism: the concrete design choice that changes the result.
4. Tradeoff: what the design does not solve or makes worse.
5. Practical test: questions the reader can apply elsewhere.

The blog is not a changelog, vulnerability feed, benchmark archive, or release
announcement surface.

## Editorial depth

Published essays should normally reach 1,800–2,800 words. Guides should usually
reach 1,000–1,500 words without delaying the first runnable command. Word count
is a floor for developed reasoning, not a target to pad.

Every canonical page needs at least one useful visual: an architecture diagram,
evidence screenshot, decision table, or workflow figure. The visual must teach
a relationship the prose would otherwise make the reader reconstruct.

An essay should include a concrete failure or run, implementation-level
mechanism, the tradeoff that mechanism introduces, and sources. A guide should
include expected evidence, common failure modes, and a clear stopping point.

## Claims and limits

Name the layer and the observer.

- supervised and microvm enforce egress at L3/L4.
- container uses an L7 proxy allowlist.
- Every tier below microvm shares the host kernel.
- A host-observed exit is evidence. An agent-authored summary is testimony.
- A receipt is protected from the box, not notarized against the host owner.
- Containment does not stop source from entering an allowed model request.

If a section is unavailable, say why. Absence must not impersonate success.

## Page mechanics

Every canonical article needs one H1 and a contiguous heading outline;
descriptive metadata; canonical, Open Graph, and Twitter tags, including an
image alt; TechArticle and BreadcrumbList JSON-LD; visible FAQ text when
FAQPage data is present; useful internal links; a current dateModified; and
inclusion in sitemap.xml. Blog essays also enter feed.xml.

A title should fit in about 60 characters and a meta description in about 160,
because that is where a search result cuts them. When a page's card blurb is
worth more room than that, give it a shorter `meta` line as well.

Dates are not decorative. `PAGE_HISTORY` in this file records each page's last
content change beside a fingerprint of what it contained, and the build refuses
to finish when a fingerprint moves without its date, so `lastmod` and
`dateModified` cannot quietly describe a version that no longer ships.

One entity, one @id. The product is `https://h5i.dev/#app` and the site is
`#website` on every page that names them; a page-scoped node (`#faq`,
`#breadcrumb`, `#webpage`) is scoped to that page's URL. Two pages describing
one @id differently, or one page carrying two BreadcrumbLists, leaves a crawler
picking between them. A breadcrumb is the trail to the page it sits on, so the
home page has none.

Before publishing, remove repeated setup, claims without mechanisms, invented
precision, and references to features the manual no longer documents. Then
read the opening callout and every heading without the body. They should still
tell the whole story.
""")


if __name__ == "__main__":
    build()
    # Stamp what was just rewritten. `build()` reissues every page it owns with
    # bare `_static` links, so without this a plain `python3 build-content.py`
    # leaves a tree CI rejects. The generator that stales a page is the one that
    # should un-stale it; nobody should have to remember a second command.
    subprocess.run(
        [sys.executable, str(ROOT.parent / "scripts" / "stamp_assets.py")],
        check=True,
        stdout=subprocess.DEVNULL,
    )
