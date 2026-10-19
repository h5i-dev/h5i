#!/usr/bin/env python3
"""Generate docs/manual/index.html from docs/MANUAL.md.

The /manual/ page is RENDERED OUTPUT, not hand-edited. Edit docs/MANUAL.md, then
regenerate:

    pip install markdown        # one-time dependency (python-markdown)
    python3 scripts/gen_manual.py   # run from the repo root

It wraps the rendered manual in the site shell (nav, footer, dark/red theme)
with a sticky sidebar TOC + scrollspy, and uses a GitHub-compatible heading
slugify so docs/MANUAL.md's in-doc cross-references resolve.
"""
import posixpath
import re, subprocess, sys
from pathlib import Path

import markdown

src = open("docs/MANUAL.md", encoding="utf-8").read()

# Drop the hand-written "## Table of Contents" block (we render a styled sidebar instead)
src = re.sub(r'\n## Table of Contents\n.*?(?=\n## )', '\n', src, count=1, flags=re.S)

import re as _re
def gh_slug(value, sep):
    v=value.strip().lower()
    v=_re.sub(r'[\u2000-\u206f\u2e00-\u2e7f\\\'!"#$%&()*+,./:;<=>?@\[\]^`{|}~]','',v)
    return v.replace(' ', sep)
md = markdown.Markdown(extensions=["fenced_code","tables","toc","sane_lists","attr_list"],
                       extension_configs={"toc":{"permalink":False,"slugify":gh_slug,"separator":"-"}})
body = md.convert(src)

# Guard against the two ways this renderer silently produces wrong HTML from markdown that looks
# fine in an editor and on GitHub.
def _check(body):
    problems = []
    for m in re.finditer(r'<p>(?:(?!</p>).)*?\n\s*[-*+] ', body, re.S):
        problems.append(f"list item swallowed into a paragraph near: "
                        f"{re.sub(r'<[^>]+>', '', m.group(0))[:70].strip()!r}")
    if '```' in body:
        i = body.index('```')
        problems.append(f"unrendered code fence (indented inside a list?) near: "
                        f"{re.sub(chr(60) + '[^>]+' + chr(62), '', body[i:i + 70]).strip()!r}")
    if problems:
        raise SystemExit("docs/MANUAL.md renders to broken HTML:\n  - "
                         + "\n  - ".join(problems)
                         + "\nSee the note above this check in scripts/gen_manual.py.")

_check(body)

# `docs/MANUAL.md` is a repository file, so its links are relative to `docs/`.
# The rendered page is served from `/manual/`, where those links resolve against
# the wrong root: `ROADMAP.md` becomes `/manual/ROADMAP.md`, and the See also
# section 404s. Point every relative link at the file on GitHub instead, which
# is the copy a reader following a source-tree reference wants, and is right
# whether or not the target happens to be published under `docs/`.
REPO = "https://github.com/h5i-dev/h5i"

def link_out(body):
    def rewrite(m):
        href = m.group(1)
        if re.match(r"[a-z]+:|//|/|#", href):
            return m.group(0)
        path, _, frag = href.partition("#")
        target = posixpath.normpath(posixpath.join("docs", path))
        if not Path(target).exists():
            raise SystemExit(f"docs/MANUAL.md links to {path!r}, which is not in "
                             f"the repository (resolved to {target!r}).")
        kind = "tree" if Path(target).is_dir() else "blob"
        url = f"{REPO}/{kind}/main/{target}" + ("/" if path.endswith("/") else "")
        return f'href="{url}{"#" + frag if frag else ""}"'
    return re.sub(r'href="([^"]*)"', rewrite, body)

body = link_out(body)

# The page is three columns, after a docs site rather than a blog post: the
# chapters on the left (grouped, numbered), the text in the middle, and on the
# right the sections of whichever chapter is being read plus the page's facts.
GROUPS = [
    ("Start here", ["what-h5i-is", "the-engagement", "install", "command-groups",
                    "scope-and-identity"]),
    ("Red-teaming", ["h5i-recon", "h5i-browser", "h5i-websec", "h5i-project", "h5i-test"]),
    ("Workspace", ["containment-optional", "the-console", "h5i-app", "h5i-skill",
                   "h5i-plugin"]),
    ("Reference", []),  # policy, receipts, limits, files, and anything new
]

chapters = []
def collect(tokens):
    for t in tokens:
        if t["level"] == 2:
            chapters.append(t)
        elif t["level"] < 2:
            collect(t.get("children", []))
collect(md.toc_tokens)
num = {t["id"]: f"{i + 1:02d}" for i, t in enumerate(chapters)}

def group_of(cid):
    for name, ids in GROUPS:
        if cid in ids:
            return name
    return GROUPS[-1][0]

nav_html = []
for name, _ in GROUPS:
    members = [t for t in chapters if group_of(t["id"]) == name]
    if not members:
        continue
    nav_html.append(f'<div class="mn-group"><p class="mn-head">{name}</p>')
    for t in members:
        nav_html.append(f'<a href="#{t["id"]}"><span class="mn-n">{num[t["id"]]}</span>'
                        f'<span>{t["name"]}</span></a>')
    nav_html.append('</div>')
nav_html = "\n".join(nav_html)

page_toc = []
for t in chapters:
    kids = [c for c in t.get("children", []) if c["level"] == 3]
    page_toc.append(f'<div class="ma-sec" data-sec="{t["id"]}">'
                    f'<a href="#{t["id"]}">{t["name"]}</a>')
    page_toc.extend(f'<a class="t3" href="#{c["id"]}">{c["name"]}</a>' for c in kids)
    page_toc.append('</div>')
page_toc = "\n".join(page_toc)

# The chapter number sits above each heading, the same number the left column shows.
body = re.sub(r'<h2 id="([^"]+)">',
              lambda m: f'<h2 id="{m.group(1)}" data-n="{num.get(m.group(1), "")}">', body)
# The title moves into the page head, which carries the facts beside it.
body = re.sub(r'<h1[^>]*>.*?</h1>\s*', '', body, count=1, flags=re.S)

# Every block gets a label strip and a copy button, rendered here rather than
# injected by script so the page does not shift once it loads.
LABELS = {"bash": "terminal", "console": "terminal", "sh": "terminal", "text": "output"}
def codeblock(m):
    lang = m.group(1) or ""
    label = LABELS.get(lang, lang or "text")
    return (f'<div class="codeblock"><div class="codeblock-head"><span>{label}</span>'
            f'<button class="copy-btn" type="button">Copy</button></div>'
            f'<pre><code{m.group(0)[len("<pre><code"):]}')
body = re.sub(r'<pre><code(?: class="language-(\w+)")?>', codeblock, body)
body = body.replace('</code></pre>', '</code></pre></div>')

# Facts for the head and the right column. The date is the one build-content.py
# records for this page; fingerprints ignore dates, so this does not feed back.
_hist = Path("docs/build-content.py").read_text(encoding="utf-8")
updated = re.search(r'"manual/": \("(\d{4}-\d{2}-\d{2})"', _hist).group(1)
minutes = round(len(re.findall(r"\w+", src)) / 230)

HEADER = f'''<header class="manual-head">
      <p class="crumbs"><a href="/">h5i</a><i>/</i><a href="/guides/">docs</a><i>/</i><span>manual</span></p>
      <p class="post-eyebrow">CLI reference</p>
      <h1>h5i Manual</h1>
      <p class="manual-meta"><span>Updated <time datetime="{updated}">{updated}</time></span><i>|</i><span>{minutes} min read</span><i>|</i><span class="ok">{len(chapters)} chapters</span></p>
      <details class="manual-mobile-toc"><summary>Chapters</summary>
{nav_html}
      </details>
    </header>'''

META = f'''<p class="ma-head">Metadata</p>
    <dl class="ma-meta">
      <div><dt>Updated</dt><dd>{updated}</dd></div>
      <div><dt>Chapters</dt><dd>{len(chapters)}</dd></div>
      <div><dt>Reading</dt><dd>{minutes} min</dd></div>
      <div><dt>Flags</dt><dd><code>--help</code></dd></div>
      <div><dt>Source</dt><dd><a href="{REPO}/blob/main/docs/MANUAL.md">MANUAL.md &#8599;</a></dd></div>
    </dl>'''

HEAD = '''<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>h5i manual: build, verify, and hack web apps</title>
  <meta name="description" content="h5i CLI reference for building Rust web apps, verifying kernel logic with Lean 4, and red-teaming running apps with browser, recon, websec, and test commands.">
  <meta name="keywords" content="h5i manual, h5i cli reference, h5i browser, h5i browser open, h5i browser snapshot, h5i browser audit, agent browser cli, h5i box, isolation tiers, env.toml policy, egress allowlist, credential broker, execution receipt, h5i app, h5i-app, lean 4, claude code, codex">
  <meta name="author" content="h5i-dev">
  <meta name="theme-color" content="#D21C1C">
  <meta name="color-scheme" content="dark">
  <meta name="robots" content="index, follow, max-image-preview:large">
  <link rel="canonical" href="https://h5i.dev/manual/">
  <link rel="sitemap" type="application/xml" href="/sitemap.xml">
  <link rel="icon" type="image/png" href="/_static/logo.png">
  <link rel="apple-touch-icon" href="/_static/logo.png">
  <meta property="og:type" content="article">
  <meta property="og:site_name" content="h5i">
  <meta property="og:title" content="h5i manual: build, verify, and hack web apps">
  <meta property="og:description" content="h5i CLI reference for building Rust web apps, verifying kernel logic with Lean 4, and red-teaming running apps with browser, recon, websec, and test commands.">
  <meta property="og:url" content="https://h5i.dev/manual/">
  <meta property="og:image" content="https://h5i.dev/_static/social-card.png">
  <meta property="og:image:alt" content="The h5i logo beside the words Agent-Native Web-Security">
  <meta property="og:locale" content="en_US">
  <meta name="twitter:card" content="summary_large_image">
  <meta name="twitter:title" content="h5i manual: build, verify, and hack web apps">
  <meta name="twitter:description" content="h5i CLI reference for building Rust web apps, verifying kernel logic with Lean 4, and red-teaming running apps with browser, recon, websec, and test commands.">
  <meta name="twitter:image" content="https://h5i.dev/_static/social-card.png">
  <meta name="twitter:image:alt" content="The h5i logo beside the words Agent-Native Web-Security">

  <script type="application/ld+json">
  {"@context":"https://schema.org","@graph":[
    {"@type":"TechArticle","@id":"https://h5i.dev/manual/#article",
     "headline":"h5i manual: build, verify, and hack web apps",
     "description":"h5i CLI reference for building Rust web apps, verifying kernel logic with Lean 4, and red-teaming running apps with browser, recon, websec, and test commands.",
     "url":"https://h5i.dev/manual/",
     "inLanguage":"en",
     "isPartOf":{"@type":"WebSite","name":"h5i","url":"https://h5i.dev/"},
     "about":{"@id":"https://h5i.dev/#app"},
     "author":{"@type":"Organization","name":"h5i-dev"},
     "publisher":{"@type":"Organization","name":"h5i","logo":{"@type":"ImageObject","url":"https://h5i.dev/_static/logo.png"}},
     "proficiencyLevel":"Beginner",
     "keywords":"h5i browser, agent browser CLI, browser audit log, h5i box, isolation tiers, microvm, env.toml policy, egress allowlist, credential broker, execution receipt, h5i app, Lean 4"},
    {"@type":"BreadcrumbList","@id":"https://h5i.dev/manual/#breadcrumb","itemListElement":[
      {"@type":"ListItem","position":1,"name":"Home","item":"https://h5i.dev/"},
      {"@type":"ListItem","position":2,"name":"Manual","item":"https://h5i.dev/manual/"}]}
  ]}
  </script>

  <link rel="preconnect" href="https://fonts.googleapis.com">
  <link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
  <link href="https://fonts.googleapis.com/css2?family=Archivo:wdth,wght@62..125,100..900&family=IBM+Plex+Mono:wght@400;500;600&display=swap" rel="stylesheet" media="print" onload="this.media='all'">
  <noscript><link href="https://fonts.googleapis.com/css2?family=Archivo:wdth,wght@62..125,100..900&family=IBM+Plex+Mono:wght@400;500;600&display=swap" rel="stylesheet"></noscript>
__STYLE__
</head>
<body>
__NAV__
<main class="manual-wrap">
  <aside class="manual-nav" aria-label="Manual chapters">
__NAV_TOC__
    <div class="mn-foot">
      <a href="/guides/">Guides</a>
      <a href="/guides/cheatsheet/">Cheatsheet</a>
    </div>
  </aside>
  <div class="manual-main">
  <article class="manual-body">
    __HEADER__
__BODY__
  </article>
  </div>
  <aside class="manual-aside" aria-label="On this page">
    <p class="ma-head">On this page</p>
    <nav class="ma-toc">
__PAGE_TOC__
    </nav>
    __META__
  </aside>
</main>
__FOOTER__
__SCRIPT__
</body>
</html>
'''

STYLE = '''  <link rel="stylesheet" href="/_static/blog.css">
  <link rel="stylesheet" href="/_static/highlight.css">
  <link rel="stylesheet" href="/_static/manual.css">'''

NAV = '''<nav>
  <a class="nav-logo" href="/">
    <img src="/_static/logo.png" alt="h5i">
    <span>h5i</span>
  </a>
  <ul class="nav-links">
    <li><a href="/features/">Features</a></li>
    <li><a href="/guides/">Guides</a></li>
    <li><a href="/manual/">Manual</a></li>
    <li><a href="/blog/">Blog</a></li>
    <li><a href="https://github.com/h5i-dev/h5i" class="nav-cta">GitHub &rarr;</a></li>
  </ul>
</nav>'''

FOOTER = '''<footer>
  <div class="footer-inner">
    <div class="footer-brand">
      <img src="/_static/logo.png" alt="h5i">
      <span>h5i<span class="red"> / high-five</span></span>
    </div>
    <nav class="footer-links">
      <a href="https://github.com/h5i-dev/h5i">GitHub</a>
      <a href="/features/">Features</a>
      <a href="/guides/">Guides</a>
      <a href="/blog/">Blog</a>
      <a href="/manual/">Manual</a>
      <a href="https://github.com/h5i-dev/h5i/issues">Issues</a>
      <a href="https://github.com/h5i-dev/h5i/blob/main/LICENSE">License</a>
    </nav>
    <div class="footer-legal">Apache 2.0 · Built with Rust</div>
  </div>
</footer>'''

SCRIPT = '''<script src="/_static/blog.js" defer></script>
<script src="/_static/highlight.js" defer></script>
<script src="/_static/manual.js" defer></script>'''

page = (HEAD.replace("__STYLE__", STYLE).replace("__NAV__", NAV)
        .replace("__NAV_TOC__", nav_html).replace("__PAGE_TOC__", page_toc)
        .replace("__HEADER__", HEADER).replace("__META__", META).replace("__BODY__", body)
        .replace("__FOOTER__", FOOTER).replace("__SCRIPT__", SCRIPT))
open("docs/manual/index.html", "w", encoding="utf-8").write(page)
print("wrote docs/manual/index.html  bytes:", len(page))
print("h2 sections:", body.count("<h2"), "| h3:", body.count("<h3"), "| code blocks:", body.count("<pre"), "| tables:", body.count("<table"))

# Stamp this file's `_static` links straight away. The generator that writes a
# page is the only thing that knows it just went stale, and leaving that to a
# separate command means anyone who regenerates the manual on its own leaves a
# tree CI rejects, having run exactly the command the docs told them to.
_stamp = Path(__file__).resolve().parent / "stamp_assets.py"
subprocess.run([sys.executable, str(_stamp)], check=True, stdout=subprocess.DEVNULL)
