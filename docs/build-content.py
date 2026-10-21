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
    "": ("2026-10-21", "8fa0c01dac4cb4ee"),
    "features/": ("2026-10-21", "21f5c8dddb47df96"),
    "manual/": ("2026-10-21", "0f7e605dc087544d"),
    "pitch/": ("2026-10-21", "7cfe9803d7d11df5"),
    "demo/": ("2026-10-19", "47a41b9432d0fd60"),
    "guides/": ("2026-10-21", "888b965163695896"),
    "guides/build-and-verify-a-web-app/": ("2026-10-21", "0b0d7ae775899b60"),
    "guides/red-team-a-web-app/": ("2026-10-21", "3af532e9ff5592f3"),
    "guides/advanced-web-security-testing/": ("2026-10-21", "ee29cf39a56e07d5"),
    "guides/security-regression-testing/": ("2026-10-21", "b7a8f9ea68eea371"),
    "guides/sandboxed-agent-workflows/": ("2026-10-21", "98e554bde6b6bd58"),
    "blog/": ("2026-10-11", "467efd1f3cb9e97d"),
    "blog/the-h5i-loop/": ("2026-10-21", "b0ccf2cf55e49699"),
    "blog/the-environment-is-the-sandbox/": ("2026-10-21", "d060b28d5d1c39ce"),
    "blog/choosing-agent-isolation/": ("2026-10-21", "f49b89db5500d2ed"),
    "blog/evidence-for-agent-work/": ("2026-10-21", "f4731b432d863ec4"),
    "blog/prompt-injection-is-a-boundary-problem/": ("2026-10-21", "86cc5551587c4990"),
    "blog/ai-pentesting-tools/": ("2026-10-21", "8239e20d3f20e6fe"),
    "blog/burp-suite-vs-h5i-for-ai-agents/": ("2026-10-21", "9d333979f125115d"),
    "blog/owasp-zap-vs-h5i-for-ai-agents/": ("2026-10-21", "7436012620e8657e"),
    "blog/caido-vs-h5i-for-ai-agents/": ("2026-10-21", "a120aa463cf17c14"),
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
<p>In h5i, <code>box shell</code> and <code>box run</code> start the process tree inside a resolved policy. Their children inherit the boundary, and a box with the <code>browser</code> profile runs its Chromium inside that same boundary.</p>
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
<ul><li><a href="/blog/the-h5i-loop/">Sandbox the entire workflow</a>, for the browser-to-patch workflow.</li><li><a href="/manual/#isolation-tiers">Isolation tiers</a>, for the enforcement and limits of each boundary.</li><li><a href="/guides/sandboxed-agent-workflows/">Sandboxed Agent Workflows</a>, for running one coding task in a box.</li></ul>""",
    "faq": [
        ("Is a Git worktree an agent sandbox?", "No. A worktree separates checkouts and branches. It does not constrain the process tree, filesystem reads, credentials, sockets, network destinations, or browser state."),
        ("Why does the browser need to be inside?", "The browser executes untrusted page code and holds session state. Keeping it beside the dev server gives both the same isolated localhost while preventing the agent from inheriting a user's normal browser profile."),
    ],
    "next": ("/blog/choosing-agent-isolation/", "Choose a mechanism", "How to choose an AI agent sandbox", "Select a tier by the failure it must prevent."),
    "cta": ("Try the whole loop once", "Create a box, do one real task, and review the patch beside the execution record.", "/guides/sandboxed-agent-workflows/", "Follow the sandbox guide"),
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
<ul><li><a href="/manual/#isolation-tiers">The isolation-tier reference</a>, including platform-specific limits.</li><li><a href="/guides/sandboxed-agent-workflows/">Write a box policy</a>, for storing the chosen controls in the repository.</li><li><a href="/blog/the-environment-is-the-sandbox/">Why sandbox the entire AI agent workload</a>, for the motivation behind using one boundary.</li></ul>""",
    "faq": [
        ("Should I use microVM for every agent task?", "No. Use it when the workload must not share the host kernel. Process isolation is lighter for routine coding, supervised provides packet-level destination control, and container provides a fixed toolchain image."),
        ("Does container isolation block every off-list connection?", "No. Its HTTP and HTTPS proxy constrains software that uses the proxy. Use supervised or microvm when raw off-list connections must fail at the network boundary."),
    ],
    "next": ("/guides/sandboxed-agent-workflows/", "Put it into practice", "Write a box policy", "Turn the threat model into a profile the repository can review."),
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
<ul><li><a href="/manual/#receipts">Receipt fields and observer labels</a>.</li><li><a href="/manual/#h5i-box-export">Export bundle contents and limits</a>.</li><li><a href="/guides/sandboxed-agent-workflows/">Review a pull request by running it in a detached box</a>.</li></ul>""",
    "faq": [
        ("Is an h5i receipt tamper-proof?", "It is protected from the box, not from a user who controls the host. h5i stores ingested receipts outside every filesystem grant held by the box; it does not provide third-party notarization."),
        ("Why keep agent-reported records at all?", "They provide useful detail that an external observer may not have. The requirement is to label their source and compare them with host-observed events, not to discard testimony."),
    ],
    "next": ("/guides/sandboxed-agent-workflows/", "Use the method", "Review a pull request by running it", "Read the report in an evidence-first order."),
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
<ul><li><a href="/guides/sandboxed-agent-workflows/">Write a box policy</a>, for filesystem, network, socket, and resource controls.</li><li><a href="/manual/#credentials">Credential handling</a> and <a href="/manual/#af_unix-sockets">Unix-socket policy</a>.</li><li><a href="/guides/sandboxed-agent-workflows/">Run the browser beside the dev server inside a box</a>.</li></ul>""",
    "faq": [
        ("Does sandboxing prevent source code from reaching the model?", "No. A coding agent can include source in an allowed model request. Preventing that requires a self-hosted model or a policy with no model egress."),
        ("Are permission prompts still useful inside a box?", "They catch mistakes and are fine to keep, but they are not the security boundary. A prompt-injected agent can approve or bypass its own application-level permissions; the box policy sits outside it."),
    ],
    "next": ("/guides/sandboxed-agent-workflows/", "Build the boundary", "Write down what the agent may reach", "Create a fail-closed profile for filesystem, network, and resources."),
    "cta": ("Limit what the agent can reach", "A narrow box makes a successful injection much less consequential.", "/guides/sandboxed-agent-workflows/", "Write a policy"),
}


AI_PENTESTING_TOOLS = {
    "section": "blog", "slug": "ai-pentesting-tools", "eyebrow": "Buyer guide / Web security",
    "published": "2026-09-09", "time": "7 min", "tags": "AI pentesting &middot; DAST &middot; Agent security",
    "social_image": "https://h5i.dev/_static/ai-security-tools-map.png",
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
    "next": ("/guides/red-team-a-web-app/", "Run a bounded test", "Authorized web security testing with h5i", "Turn target scope and request budgets into an auditable agent session."),
    "cta": ("Decide what you want to delegate", "Then select the scanner, workbench, or agent session that fits it.", "/guides/red-team-a-web-app/", "Follow the red-teaming guide"),
}


BURP_COMPARISON = {
    "section": "blog", "slug": "burp-suite-vs-h5i-for-ai-agents", "eyebrow": "Comparison / Web security",
    "published": "2026-09-09",
    "social_image": "https://h5i.dev/_static/burp-vs-h5i.png",
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
{terminal('browser and HTTP traffic in one session', '$ h5i browser open https://target.example --allow target.example\n$ h5i browser snapshot\n$ h5i browser click @e3\n$ h5i browser requests\n$ h5i websec replay req_42 --set query.id=456\n$ h5i websec diff res_42 res_43')}
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
    "next": ("/guides/red-team-a-web-app/", "Try the workflow", "Run an authorized web security test", "Create a scoped session, build an endpoint ledger, and replay one captured request."),
    "cta": ("Give the agent a narrow assignment", "Make target scope, request budget, captured evidence, and the stopping point part of the run.", "/guides/red-team-a-web-app/", "Follow the red-teaming guide"),
}


ZAP_COMPARISON = {
    "section": "blog", "slug": "owasp-zap-vs-h5i-for-ai-agents", "eyebrow": "Comparison / Web security",
    "published": "2026-09-09",
    "social_image": "https://h5i.dev/_static/zap-vs-h5i.png",
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
{terminal('agent-led browser and HTTP testing', '$ h5i browser open https://target.example --allow target.example\n$ h5i browser snapshot\n$ h5i browser requests\n$ h5i websec replay req_42 --set query.id=456\n$ h5i websec diff res_42 res_43')}
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
    "cta": ("Start with one scoped session", "Use a target you are authorized to test and make the request ceiling explicit.", "/guides/red-team-a-web-app/", "Follow the testing guide"),
}


CAIDO_COMPARISON = {
    "section": "blog", "slug": "caido-vs-h5i-for-ai-agents", "eyebrow": "Comparison / Web security",
    "published": "2026-09-09",
    "social_image": "https://h5i.dev/_static/caido-vs-h5i.png",
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
{terminal('browser and HTTP traffic in one session', '$ h5i browser open https://target.example --allow target.example\n$ h5i browser snapshot\n$ h5i browser click @e3\n$ h5i browser requests\n$ h5i websec replay req_42 --set query.id=456')}
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
    "cta": ("Choose by the traffic workflow", "Decide whether the agent needs a full proxy workspace or a direct browser-to-request loop.", "/guides/red-team-a-web-app/", "Run an agent-led test"),
}


LOOP = {
    "section": "blog", "slug": "the-h5i-loop", "eyebrow": "Essay / Sandboxed workflow",
    "time": "10 min", "tags": "Browse &middot; Develop &middot; Review &middot; Apply",
    "title": "Sandbox the AI agent workflow: browse, develop, apply | h5i",
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
{terminal('repository root', '$ h5i box --profile browser --isolation process --name fix-auth\n$ h5i box status fix-auth')}
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
<p>The example names <code>process</code> because it can hold a resident browser without a heavyweight runtime. The tier changes what “inside” proves. A process-tier browser is confined for files and environment; a tier with egress enforcement outside the browser also decides what it may reach. Read <code>status</code> rather than inferring guarantees from the word sandbox.</p>
<h2 id="browse">2. Put browsing inside the same box</h2>
<p>The <code>browser</code> profile puts a Chromium, driven by agent-browser, inside the box:</p>
{terminal('host', '$ h5i box run fix-auth -- agent-browser open https://docs.rs/\n$ h5i box run fix-auth -- agent-browser snapshot\n$ h5i box run fix-auth -- agent-browser click @e3')}
<p>The browser and its fresh profile live in the named box, so the page state, cookies, downloads, and requests stay with the development environment instead of appearing in a host browser profile. h5i mediates the agent-browser socket from outside the box, so every action is recorded where the box cannot rewrite it.</p>
<p>The distinction matters in both directions. External documentation can contain instructions that influence the agent, so it should not gain more filesystem or network authority than the coding task. Later, when the browser opens <code>http://localhost:3000</code>, loopback should mean the dev server inside the box—not some unrelated service on the developer's machine.</p>
<p>What the browser may reach is the box's egress allowlist, enforced at the box's boundary. Placement and evidence strength are related, but they are not the same claim: read the receipt's lanes rather than assuming one.</p>
<h2 id="work">3. Develop and verify without crossing the boundary</h2>
<p>Enter the same named box for the coding session:</p>
{terminal('inside fix-auth', '$ h5i box shell fix-auth\nbox$ claude                          # or codex\nbox$ npm ci\nbox$ npm test\nbox$ npm run dev &')}
<p><code>shell</code> inherits stdio, and every descendant stays inside the resolved policy. The agent does not have to remember to wrap package-manager hooks, compiler workers, test processes, or the dev server separately. They are contained because they are children of the box session.</p>
<p>Now point the already-contained browser at the application. From a second host terminal:</p>
{terminal('browser in the same box', '$ h5i box run fix-auth -- agent-browser open http://localhost:3000\n$ h5i box run fix-auth -- agent-browser snapshot')}
<p>The browser and server meet on the box's loopback. The useful loop is now continuous: the agent edits, tests, starts the app, reads the page, inspects failed requests or console errors, fixes the code, and tests again. There is no host-browser detour in the middle.</p>
<p>From inside an existing <code>box shell</code>, the agent runs <code>agent-browser</code> directly; it is already in the box.</p>
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
<li><a href="/guides/sandboxed-agent-workflows/">Sandboxed Agent Workflows</a>, for running this workflow once on a real repository and observing browser work.</li>
<li><a href="/manual/#command-groups">The manual</a>, for every flag named above.</li>
</ul>""",
    "faq": [
        ("How does the browser get into the box?", "Create the box with --profile browser. Its Chromium, driven by agent-browser, runs inside the box under its resolved policy; drive it with h5i box run <box> -- agent-browser ..., or from inside a box shell."),
        ("What is the difference between export and apply?", "export writes patch.diff, report.md and receipt.json to a directory and touches nothing else, so you decide what happens next. apply lands the work directly on the parent repository and is only available when the box came from that repository. On a detached box, created from a URL, a pull request or --new, apply refuses and points at export."),
    ],
    "next": ("/blog/the-environment-is-the-sandbox/", "Why sandbox it", "Why sandbox the entire AI agent workload", "See why dependencies, tools, tests, servers, and browsers need one boundary."),
    "cta": ("Start with one box", "Run h5i box probe, create a box, and inspect what the host actually enforced.", "/guides/sandboxed-agent-workflows/", "Follow the sandbox guide"),
}


# The five chapters of the h5i web security course, whose 42 runnable labs
# live in h5i-dev/h5i-tutorial. Numbered in the book ("1. The workbench",
# "## 1.4 Replay"); here they are pages rather than a numbered part of
# anything, so the numbers are off the titles and the headings.


# The h5i web security tutorial, ten articles that walk the labs in
# h5i-dev/h5i-tutorial. Published as a numbered series whose first part is on
# Medium; here they are pages in order, so the numbers are off the titles.


ARTICLES = GUIDES + [AI_PENTESTING_TOOLS, BURP_COMPARISON, ZAP_COMPARISON, CAIDO_COMPARISON,
            LOOP, ENVIRONMENT, TIERS, EVIDENCE, INJECTION]


def index_page(section, items):
    guides = section == "guides"
    title = "h5i guides: build, verify, test, and sandbox web apps" if guides else "h5i essays: agent browsers, sandboxes, and web security"
    description = ("Five task guides for building and verifying Rust web apps, red-teaming running apps, regression testing in CI/CD, and sandboxing agent workflows."
                   if guides else "Nine essays on auditable AI browsing, sandboxing, evidence, and comparisons of h5i with Burp Suite, OWASP ZAP, and Caido for agent testing.")
    # A hub heading names the section. It is the one page on the site whose
    # job is to be a label, so the argument goes in the deck.
    h1 = "h5i Guides" if guides else "h5i Blog"
    deck = ("Start by building and formally verifying a Rust web app. Then test the running application, turn confirmed findings into CI checks, and run agent work inside a reviewed sandbox."
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
<body>{NAV}<section class="index-hero"><div class="post-eyebrow">{len(items)} {"task guides" if guides else "essays and comparisons"}</div>
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
