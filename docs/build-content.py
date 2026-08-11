"""Build the hand-written guides and essays into the static docs tree."""

from pathlib import Path
import json
import shutil

ROOT = Path(__file__).parent
TODAY = "2026-08-10"

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


def head(title, description, canonical, schema, kind="article", rss=False):
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
<meta property="og:url" content="{canonical}"><meta property="og:image" content="https://h5i.dev/_static/sandbox-ui-demo.png">
<meta name="twitter:card" content="summary_large_image"><meta name="twitter:title" content="{title}">
<meta name="twitter:description" content="{description}"><meta name="twitter:image" content="https://h5i.dev/_static/sandbox-ui-demo.png">
<script type="application/ld+json">{data}</script>
<link rel="preconnect" href="https://fonts.googleapis.com"><link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link href="https://fonts.googleapis.com/css2?family=Archivo:wght@700;800;900&amp;family=Space+Grotesk:wght@300;400;500;700&amp;family=Space+Mono:wght@400;700&amp;display=swap" rel="stylesheet">
<link rel="stylesheet" href="/_static/blog.css"><link rel="stylesheet" href="/_static/highlight.css">
</head>"""


def terminal(label, text):
    return f"""<div class="terminal"><div class="terminal-bar"><span class="terminal-path">{label}</span></div>
<div class="terminal-body"><pre><code>{text}</code></pre></div></div>"""


def schema_for(item):
    url = f"https://h5i.dev/{item['section']}/{item['slug']}/"
    graph = [
        {"@type": "TechArticle", "headline": item["h1"], "description": item["description"],
         "author": {"@type": "Organization", "name": "h5i-dev"},
         "publisher": {"@type": "Organization", "name": "h5i"},
         "datePublished": TODAY, "dateModified": TODAY, "mainEntityOfPage": url},
        {"@type": "BreadcrumbList", "itemListElement": [
            {"@type": "ListItem", "position": 1, "name": "Home", "item": "https://h5i.dev/"},
            {"@type": "ListItem", "position": 2, "name": item["section"].title(), "item": f"https://h5i.dev/{item['section']}/"},
            {"@type": "ListItem", "position": 3, "name": item["h1"], "item": url},
        ]},
        {"@type": "FAQPage", "mainEntity": [
            {"@type": "Question", "name": q, "acceptedAnswer": {"@type": "Answer", "text": a}}
            for q, a in item["faq"]
        ]},
    ]
    return {"@context": "https://schema.org", "@graph": graph}


def article_page(item):
    url = f"https://h5i.dev/{item['section']}/{item['slug']}/"
    faq = "".join(
        f'<details class="faq-item"><summary>{q}</summary><div class="faq-answer">{a}</div></details>'
        for q, a in item["faq"]
    )
    nxt = item["next"]
    return f"""{head(item['title'], item['description'], url, schema_for(item))}
<body>{NAV}<main class="article-wrap"><article class="post">
<header><div class="post-eyebrow">{item['eyebrow']} &middot; {TODAY}</div>
<h1>{item['h1']}</h1><p class="post-deck">{item['deck']}</p>
<div class="post-meta"><span>{item['time']} read</span><span>{item['tags']}</span></div></header>
{item['body']}
<h2 id="faq">Questions that come up</h2><div class="faq-list">{faq}</div>
<a class="next-up" href="{nxt[0]}"><span class="label">{nxt[1]}</span><h3>{nxt[2]}</h3><p>{nxt[3]}</p></a>
<div class="post-cta"><h3>{item['cta'][0]}</h3><p>{item['cta'][1]}</p>
<div class="hero-actions"><a class="btn btn-primary" href="{item['cta'][2]}">{item['cta'][3]}</a></div></div>
</article></main>{FOOTER}</body></html>"""


FIRST_BOX = {
    "section": "guides", "slug": "first-box", "eyebrow": "Guide 01 / Start here",
    "time": "7 min", "tags": "Install &middot; Create &middot; Export",
    "title": "Your first h5i box | h5i",
    "h1": "Take one coding task from prompt to reviewed patch",
    "description": "Create your first h5i sandbox, run an agent inside it, inspect the diff and execution record, then export or apply the result.",
    "deck": "The useful unit is not a sandboxed command. It is the whole coding session: repository, agent, shell, dependencies, dev server, and browser inside one disposable boundary.",
    "body": f"""
<div class="callout"><strong>Outcome.</strong> In about ten minutes you will create a named box, work inside it, inspect what changed and what ran, then choose whether the patch leaves the boundary.</div>
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
<p>A box is cheap because it is disposable. Keep the export. Remove the execution environment.</p>""",
    "faq": [
        ("Does h5i change my current checkout?", "A box created from the current repository uses its own Git worktree and branch. Your current checkout is not where the agent works. Only an explicit apply step lands a proposed local change."),
        ("Which isolation tier should I use first?", "Leave the tier on auto for the first run, then read h5i box status. An explicit tier fails closed if the host cannot provide it; h5i never silently substitutes a weaker tier."),
        ("Can I use Codex instead of Claude Code?", "Yes. Replace agent-claude with agent-codex and run codex inside the shell. Keep one runtime per box so credentials and configuration remain scoped."),
    ],
    "next": ("/guides/review-a-pull-request/", "Next guide", "Run an untrusted pull request", "Use a detached box when the code did not originate in your repository."),
    "cta": ("Make the box the default place agents work", "The boundary only helps when the whole session starts inside it.", "/manual/#h5i-box", "Open the box reference"),
}


REVIEW_PR = {
    "section": "guides", "slug": "review-a-pull-request", "eyebrow": "Guide 02 / Untrusted code",
    "time": "6 min", "tags": "Pull request &middot; Detached box &middot; Review",
    "title": "Review a pull request in an h5i box | h5i",
    "h1": "Run the pull request before you trust the pull request",
    "description": "Fetch an untrusted pull request into a detached h5i box, build and exercise it, inspect denied activity, and export a review bundle.",
    "deck": "A diff shows the final tree. It cannot show what an install script attempted, what the branch contacted, or whether the tests ever ran. A detached box lets you find out without giving the branch your machine.",
    "body": f"""
<div class="callout"><strong>Boundary first.</strong> A pull-request box gets its own repository, drops the inherited <code>origin</code>, and cannot be applied or rebased into the parent. External code leaves only through <code>export</code>.</div>
<h2 id="create">1. Create a detached box</h2>
{terminal('repository root', '''$ h5i box create review-1234 --pr 1234 --profile agent-claude
$ h5i box status review-1234''')}
<p><code>--pr</code> accepts a number, <code>#number</code>, or pull-request URL. h5i fetches the head on the host, pins it, then gives the box an independent repository with no inherited network remote.</p>
<h2 id="baseline">2. Read the boundary before the branch</h2>
<p>Confirm three things in <code>status</code>: the box is detached, the requested isolation tier is enforced, and network access is no broader than the review needs. Do this before running a package manager; install hooks are code execution.</p>
{terminal('host', '''$ h5i box capabilities review-1234 --json
$ h5i box secrets review-1234''')}
<p><code>secrets</code> shows declared grants and dry-run resolution, never secret values. A review that needs no authenticated service should have no grant.</p>
<h2 id="run">3. Build and test inside the box</h2>
{terminal('host, then box', '''$ h5i box shell review-1234
box$ npm ci
box$ npm test
box$ npm run dev
# In another host terminal: h5i box view review-1234
box$ exit''')}
<p>Use the project's real install and test commands. If it is a web change, start the server in the same session and drive the isolated browser. The app, browser, and agent then agree on what <code>localhost</code> means.</p>
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
<p>You can apply an accepted patch wherever you choose with <code>git apply --3way</code>. h5i refuses <code>box apply</code> for this detached box by design.</p>""",
    "faq": [
        ("Do GitHub credentials enter the box?", "No. The host fetches the pull-request head. The detached box receives the code, not the host's SSH key, GitHub token, or inherited origin remote."),
        ("Why not check out the branch in a normal worktree?", "A worktree separates checkouts, not authority. Package scripts would still run with your user's filesystem, network, sockets, and credentials unless another boundary removes them."),
        ("Can an agent perform the review?", "Yes. Run it inside the box and ask it to build, test, inspect the browser, and write findings. The execution record remains separate from the agent's self-report."),
    ],
    "next": ("/guides/write-a-box-policy/", "Next guide", "Write the boundary down", "Turn filesystem, network, and resource assumptions into a checked-in profile."),
    "cta": ("Review behavior, not just text", "Give untrusted code somewhere safe to execute before you decide whether to take it.", "/manual/#h5i-box-export", "Read about export"),
}


POLICY = {
    "section": "guides", "slug": "write-a-box-policy", "eyebrow": "Guide 03 / Policy",
    "time": "8 min", "tags": "Isolation &middot; Egress &middot; Resources",
    "title": "Write an h5i box policy | h5i", "h1": "Write down what the agent may reach",
    "description": "Define an h5i profile with an explicit isolation tier, filesystem grants, default-deny networking, and resource limits, then verify it.",
    "deck": "Permission prompts ask the agent to police itself. A box policy is resolved before the agent starts, enforced outside its process, and digested into every receipt.",
    "body": f"""
<div class="callout"><strong>Start narrow.</strong> Grant the workspace, the system paths required to run, the destinations required for the task, and a finite wall clock. Add authority only after a refusal explains why it is needed.</div>
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
<h2 id="denials">4. Let denials guide refinement</h2>
{terminal('host', '''$ h5i box run policy-check -- npm test
$ h5i box log policy-check
$ h5i box export policy-check --out ./policy-check-report''')}
<p>A denied registry host may justify one more destination. A denied telemetry host usually does not. Treat each addition as a reviewable transfer of authority, not a way to make the error disappear.</p>
<div class="callout warn"><strong>Do not turn on Unix sockets casually.</strong> <code>unix = true</code> permits <code>AF_UNIX</code> sockets, which can carry file descriptors through <code>SCM_RIGHTS</code>. The browser profile needs this; most build profiles do not.</div>
<h2 id="commit">5. Commit the policy with the code</h2>
<p>A checked-in profile gives reviewers one file to discuss. At creation, h5i resolves machine-specific values, serializes the result, hashes it, and puts that digest on the receipts. The repository states the intended boundary; the receipt names the boundary that actually ran.</p>""",
    "faq": [
        ("What happens if my machine cannot provide the requested tier?", "Creation fails before a partial box is left behind. Explicit isolation requests are never silently downgraded."),
        ("Why is container egress weaker than supervised egress?", "The container tier uses an HTTP/HTTPS proxy allowlist, so software that ignores proxy settings can bypass that L7 route. The supervised tier enforces destination access in a private network namespace at L3/L4."),
        ("Are memory and process limits enforced on macOS?", "Not at the process and supervised tiers. h5i marks those values instead of claiming enforcement. Use container or microvm when a hard memory or process ceiling is required."),
    ],
    "next": ("/blog/choosing-agent-isolation/", "Design rationale", "Five tiers, five different promises", "Read the threat-model argument behind the ladder."),
    "cta": ("Make authority reviewable", "A small policy file is easier to reason about than a trail of permission clicks.", "/manual/#policy", "Open the policy reference"),
}


BROWSER = {
    "section": "guides", "slug": "watch-the-browser", "eyebrow": "Guide 04 / Browser",
    "time": "5 min", "tags": "Dev server &middot; Viewer &middot; Control lock",
    "title": "Watch an agent's browser in an h5i box | h5i",
    "h1": "Watch the page, then take the controls",
    "description": "Run a dev server and browser inside an h5i box, watch it through a loopback-only viewer, and safely transfer control from agent to human.",
    "deck": "The browser belongs inside the same boundary as the code and dev server. You still need a way to see it—and a handoff that cannot turn a stale page reference into the wrong click.",
    "body": f"""
<div class="callout"><strong>The shape.</strong> Frames flow out of the box. Input flows in only for the control-lock holder. The browser's stream port is never published on the host.</div>
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
<h2 id="take">4. Take control explicitly</h2>
{terminal('host', '''$ h5i browser status browser-demo
$ h5i browser take browser-demo
# interact in the viewer
$ h5i browser release browser-demo''')}
<p>Taking control invalidates every page handle the agent held. When control returns, the agent must take a new snapshot before it can act. A stale handle is refused instead of being resolved against a page that may have changed under human hands.</p>
<h2 id="review">5. Review browser evidence with the code</h2>
{terminal('host', '''$ h5i box export browser-demo --out ./browser-review
$ less ./browser-review/report.md''')}
<p>The report can include console errors, uncaught exceptions, failed requests, and viewer sessions. It can show that a human took over; it cannot claim the page was correct merely because someone viewed it.</p>""",
    "faq": [
        ("Does h5i publish the box's browser port?", "No. h5i enters the box's network namespace by process id, connects from inside, and hands the socket back out through a loopback-only authenticated viewer."),
        ("Why do page references become stale after a handoff?", "A human can change navigation, focus, and DOM state. Invalidating old handles forces the agent to observe the new page before acting, preventing a stale reference from targeting the wrong element."),
        ("Can I watch over SSH?", "Yes, with h5i box view --term in a terminal that supports the Kitty graphics protocol. This path does not bind a port."),
    ],
    "next": ("/blog/the-environment-is-the-sandbox/", "Read the principle", "The environment is the sandbox", "Why the browser, server, shell, and agent must share one boundary."),
    "cta": ("Put localhost inside the boundary", "Let the agent exercise the same application you are watching without publishing its internal ports.", "/manual/#h5i-box-view", "Open the viewer reference"),
}


ENVIRONMENT = {
    "section": "blog", "slug": "the-environment-is-the-sandbox",
    "eyebrow": "Essay / Architecture", "time": "8 min", "tags": "Sandbox &middot; Agent loop &middot; Browser",
    "title": "The environment is the sandbox | h5i", "h1": "The environment is the sandbox",
    "description": "Coding agents do not execute one risky command. They operate a development environment, so that whole environment must become the security boundary.",
    "deck": "Sandboxing one shell command was the right idea at the wrong scale. A coding agent operates a repository, package manager, compiler, dev server, and browser. Leave one outside and the boundary has a door in it.",
    "body": """
<div class="callout"><strong>The claim.</strong> The unit of isolation for a coding agent is the complete development environment—not the model process, not the shell command, and not the Git checkout.</div>
<p>Command wrappers fit the world they were designed for. A program receives input, performs one bounded action, and returns output. You can put a wall around that moment.</p>
<p>A coding agent does not live in that world. It reads a repository, edits several files, invokes a package manager, starts a compiler, watches tests, launches a server, opens a browser, reads the console, and tries again. The work is a loop. Its children are part of the work.</p>
<p>If the agent is confined but its package scripts are not, the scripts own the machine. If the shell is confined but the browser uses your normal profile, the page inherits your sessions. If the repository is a worktree but the process still sees your home directory, checkout separation has been mistaken for authority separation.</p>
<h2 id="wrong-units">Three boundaries that are too small</h2>
<h3>The model process</h3>
<p>Watching only the agent executable assumes all consequential actions pass through its tool protocol. They do not. A build tool can spawn a compiler, which can invoke a linker, which can execute a helper. An install hook may run before the agent sees its next prompt. The process tree, not the first process, is the relevant object.</p>
<h3>The command</h3>
<p>Wrapping <code>npm test</code> helps only if every route to <code>npm test</code> uses the wrapper. An autonomous session makes hundreds of calls. Security that depends on the agent remembering the prefix is a convention, not a boundary.</p>
<h3>The checkout</h3>
<p>A Git worktree answers where edits land. It says nothing about <code>~/.ssh</code>, cloud credentials, Unix sockets, the host network, or a browser profile. Git separates trees. It does not separate authority.</p>
<h2 id="complete">What belongs inside?</h2>
<p>Put every component that can execute code or carry session state on the same side:</p>
<ul><li><strong>Workspace:</strong> a disposable checkout with a pinned base.</li><li><strong>Agent and shell:</strong> one supervised process tree, including every child.</li><li><strong>Toolchain and dependencies:</strong> compilers, package managers, hooks, and caches.</li><li><strong>Dev server:</strong> reachable on the box's loopback, not accidentally published.</li><li><strong>Browser:</strong> a fresh profile that shares the box's network view.</li></ul>
<p>That turns a scattered list of dangerous operations into one object with a lifecycle: create, work, inspect, export, remove.</p>
<h2 id="output">A boundary needs an output gate</h2>
<p>Containment is incomplete if the agent can write directly back to the repository you care about. The useful asymmetry is broad freedom inside and a narrow, human-operated path out.</p>
<p>h5i exports three artifacts: a path-validated patch, a human-readable report, and an execution receipt. The box cannot decide that its own result is acceptable. It can propose. A person chooses whether to carry the patch across.</p>
<blockquote><p>Autonomy inside. Judgment at the boundary.</p></blockquote>
<h2 id="cheap">The boundary has to be cheap</h2>
<p>If creating a box is a ceremony reserved for obviously dangerous work, ordinary work remains uncontained. That is why lightweight tiers matter. Under 200 milliseconds changes the decision from “is this risky enough?” to “why would this run anywhere else?”</p>
<p>Stronger boundaries still have a place. A container buys a portable filesystem. A microVM buys a separate kernel. The everyday path and the hostile-code path need not pay the same startup cost, but they should share the same lifecycle and output gate.</p>
<h2 id="test">A practical test</h2>
<p>Ask five questions of any agent sandbox:</p>
<ol><li>Where do package scripts execute?</li><li>Which home directory and credentials can they see?</li><li>Where does the dev server listen?</li><li>Which browser profile opens the page?</li><li>Can the agent write the accepted result directly?</li></ol>
<p>If those answers cross the boundary in different directions, the sandbox is smaller than the work.</p>""",
    "faq": [
        ("Is a Git worktree an agent sandbox?", "No. A worktree separates checkouts and branches. It does not constrain the process tree, filesystem reads, credentials, sockets, network destinations, or browser state."),
        ("Why does the browser need to be inside?", "The browser executes untrusted page code and holds session state. Keeping it beside the dev server gives both the same isolated localhost while preventing the agent from inheriting a user's normal browser profile."),
    ],
    "next": ("/blog/choosing-agent-isolation/", "Read next", "Five tiers, five promises", "Choose an isolation mechanism by the threat it changes."),
    "cta": ("Try the whole loop once", "Create a box, do one real task, and review the patch beside the execution record.", "/guides/first-box/", "Follow the first-box guide"),
}


TIERS = {
    "section": "blog", "slug": "choosing-agent-isolation", "eyebrow": "Essay / Threat model",
    "time": "9 min", "tags": "Landlock &middot; Containers &middot; MicroVMs",
    "title": "How to choose isolation for a coding agent | h5i", "h1": "Five tiers, five different promises",
    "description": "Choose coding-agent isolation by threat model: checkout separation, process confinement, L3/L4 egress control, portable containers, or a separate kernel.",
    "deck": "Isolation is not a single strength meter. A container can improve portability while weakening network control; a microVM can strengthen the kernel boundary while producing thinner egress evidence.",
    "body": """
<div class="callout"><strong>The short answer.</strong> Use <code>process</code> for fast local confinement, <code>supervised</code> when off-list network access must fail at L3/L4, <code>container</code> when the image matters, and <code>microvm</code> when sharing the host kernel is unacceptable. <code>workspace</code> is separation, not confinement.</div>
<h2 id="not-ladder">Why “stronger” is not one dimension</h2>
<p>Sandbox comparisons often collapse everything into a ladder. That hides the decision you actually have to make. Filesystem reach, network enforcement, kernel sharing, portability, startup time, and observability move independently.</p>
<p>A rootless container has a clean image and dropped capabilities, but an HTTP proxy cannot bind a program that ignores proxy variables. A supervised host process shares the kernel, but nftables in a private network namespace can stop that same program at the packet layer. Neither sentence fits a single score.</p>
<h2 id="tiers">What each tier changes</h2>
<div class="tbl-wrap"><table class="data"><thead><tr><th>Tier</th><th>What becomes true</th><th>What stays false</th></tr></thead><tbody>
<tr><th>workspace</th><td>The agent edits a separate Git worktree.</td><td>Nothing confines the process.</td></tr>
<tr><th>process</th><td>Filesystem allowlists, syscall denials, namespaces, and limits constrain a process tree.</td><td>The host kernel is shared; destination allowlisting is not L3/L4.</td></tr>
<tr><th>supervised</th><td>A private network namespace, pinned DNS, nftables, and a socket gate enforce destination policy.</td><td>The host kernel is still shared.</td></tr>
<tr><th>container</th><td>A rootless, read-only, image-based environment improves portability.</td><td>Its proxy allowlist binds only proxy-respecting traffic.</td></tr>
<tr><th>microvm</th><td>The guest has its own kernel and evaluates egress in its network stack.</td><td>Startup is heavier and per-request egress evidence is thinner.</td></tr>
</tbody></table></div>
<h2 id="process">The everyday default</h2>
<p>The process tier is aimed at the common failure: an agent or dependency script reads or writes somewhere it should not, spawns too much work, or calls a dangerous syscall. On Linux, Landlock and seccomp do most of the work. The important property is inheritance: the policy follows the process tree.</p>
<p>This is not a claim against a targeted kernel exploit. The kernel enforcing the rule is the same kernel the confined process attacks.</p>
<h2 id="supervised">When network destination matters</h2>
<p>Use supervised isolation when “only these destinations” must describe packets, not cooperative application behavior. The box receives a private network namespace. DNS answers are pinned. nftables admits resolved addresses from the policy. A seccomp notification gate controls socket creation.</p>
<p>That design closes the obvious proxy escape: clear <code>HTTPS_PROXY</code>, open a raw socket, and dial the address directly. At supervised, the packet still meets the boundary.</p>
<h2 id="container">What a container is actually for</h2>
<p>The container tier is for repeatable images and filesystem portability. That is valuable. It is simply a different value from stronger egress.</p>
<p>Because its allowlist is an HTTP/HTTPS proxy, a compliant package manager is constrained and a program that bypasses the proxy is not. Call this L7 scoping. Do not describe it as general network isolation.</p>
<h2 id="microvm">When the kernel must move inside</h2>
<p>A microVM changes the deepest assumption. The untrusted process attacks a guest kernel; the hypervisor remains between it and the host kernel. Choose it for hostile code or environments where shared-kernel containment is outside the risk budget.</p>
<p>The trade is visible. Booting a kernel costs more. Hardware virtualization must exist. And an in-guest packet filter may drop denied traffic without producing the request-by-request summary a proxy can record. Stronger enforcement can mean thinner evidence.</p>
<h2 id="fail-closed">Why capability checks must execute</h2>
<p>A binary, kernel feature, or device node can exist while policy still prevents it from working. A useful probe runs a minimal confined action and reports whether the claim is satisfiable. Then an explicit request must fail closed. Silently replacing <code>microvm</code> with <code>process</code> would keep the command running by changing the security claim underneath it.</p>
<p>The honest interface is boring: probe, choose, create, inspect the resolved policy.</p>""",
    "faq": [
        ("Is microVM always the best tier?", "No. It gives the strongest kernel boundary, but costs more to start and currently provides thinner denied-egress evidence. Choose it when a separate kernel is the property the task requires."),
        ("Is a container stronger than process isolation?", "Not in every dimension. It improves image portability. In h5i, its L7 proxy allowlist is weaker against raw sockets than the supervised tier's L3/L4 enforcement."),
    ],
    "next": ("/guides/write-a-box-policy/", "Put it into practice", "Write a box policy", "Turn the threat model into a profile the repository can review."),
    "cta": ("Ask the host what it can enforce", "Run the functional probe before choosing a tier by name.", "/manual/#isolation-tiers", "Compare the tiers"),
}


EVIDENCE = {
    "section": "blog", "slug": "evidence-for-agent-work", "eyebrow": "Essay / Review",
    "time": "8 min", "tags": "Receipts &middot; Audit &middot; Diffs",
    "title": "What counts as evidence for agent-written code? | h5i", "h1": "A transcript is not an audit trail",
    "description": "Review agent-written code with evidence collected outside the agent: the patch, observed execution, denied activity, and explicit gaps in observation.",
    "deck": "The agent can explain what it did in perfect prose. That account is useful—and it is still testimony from the subject of the review. Evidence begins where self-report ends.",
    "body": """
<div class="callout"><strong>The test.</strong> Point at a line in the record and ask: <em>could the agent have written this?</em> If yes, keep it as testimony. Do not promote it into independent evidence.</div>
<h2 id="diff">A diff records state, not execution</h2>
<p>A Git diff is excellent at its job. It describes how one tree differs from another, composes through merges, and gives reviewers a stable object to discuss.</p>
<p>It cannot show a failed network connection, a read that left no edit, an install hook that ran, a test that was skipped, or a browser exception that did not change a file. Those are events. Tree comparison has no place to store them.</p>
<p>Human review papered over that gap with conversation. The author remembered the work and answered questions. An autonomous agent removes that memory while making the replacement self-report unusually fluent.</p>
<h2 id="source">The source of a record sets its value</h2>
<p>Suppose a transcript says “ran <code>cargo test</code>, exit 0.” The line may be true. If the agent or its in-process tool shim produced it, the line is the subject's account of itself.</p>
<p>Now suppose the supervisor that launched the child waits for it and records the exit status. The words can be identical. The evidence is different because the child did not author the observation.</p>
<p>This distinction must travel with every event. Flatten host-observed and box-claimed records into one log and no downstream dashboard can reconstruct the trust boundary.</p>
<h2 id="storage">Evidence must live outside the subject's write path</h2>
<p>External observation is not enough if the result is written somewhere the agent can edit. The storage layout matters.</p>
<p>In h5i, the box can stage capture data in its spool. The host ingests it into receipt storage outside every grant the box holds. Host-observed process exits and proxy decisions are already produced there. The box cannot rewrite an earlier receipt without first escaping the boundary.</p>
<p>This is local protection, not notarization. A user with control of the host can edit local files. The claim is narrower: the code under review cannot silently rewrite the record of its own run.</p>
<h2 id="lanes">Keep evidence grades visible</h2>
<div class="tbl-wrap"><table class="data"><thead><tr><th>Record</th><th>Observer</th><th>What it supports</th></tr></thead><tbody>
<tr><th>Patch</th><td>Git tree comparison</td><td>What changed</td></tr>
<tr><th>Process exit</th><td>Host supervisor</td><td>What command ended and how</td></tr>
<tr><th>Egress refusal</th><td>Boundary or proxy</td><td>Which destination was denied</td></tr>
<tr><th>Browser error</th><td>In-box browser capture</td><td>What the instrumented page reported</td></tr>
<tr><th>Agent proposal</th><td>Agent</td><td>What the agent says the result means</td></tr>
</tbody></table></div>
<p>These records belong together. They do not deserve the same color, ordering, or confidence.</p>
<h2 id="absence">Absence must not impersonate success</h2>
<p>A missing browser section can mean no errors, no browser, or a failed capture. An empty egress summary can mean no attempts or a tier whose packet filter does not report them. Good evidence formats name the difference.</p>
<p>This is the hardest discipline in audit UI: make uncertainty visible even when it makes the product look less complete. Grey is information. “Unavailable” is a result. Silence is ambiguity.</p>
<h2 id="review">A better review order</h2>
<ol><li>Start with boundary refusals and failed execution.</li><li>Confirm the meaningful build and test commands actually ran.</li><li>Read browser and resource observations, including unavailable sections.</li><li>Review the patch against the pinned base.</li><li>Read the agent's explanation last.</li></ol>
<p>This order does not replace code review. It stops eloquent testimony from framing the evidence before you see it.</p>""",
    "faq": [
        ("Is an h5i receipt tamper-proof?", "It is protected from the box, not from a user who controls the host. h5i stores ingested receipts outside every filesystem grant held by the box; it does not provide third-party notarization."),
        ("Why keep agent-reported records at all?", "They provide useful detail that an external observer may not have. The requirement is to label their source and compare them with host-observed events, not to discard testimony."),
    ],
    "next": ("/guides/review-a-pull-request/", "Use the method", "Review a pull request by running it", "Read the report in an evidence-first order."),
    "cta": ("Put the record beside the patch", "Export both, then review each artifact for the question it can actually answer.", "/manual/#receipts", "Read the receipt reference"),
}


INJECTION = {
    "section": "blog", "slug": "prompt-injection-is-a-boundary-problem",
    "eyebrow": "Essay / Security", "time": "8 min", "tags": "Prompt injection &middot; Least authority &middot; Egress",
    "title": "Prompt injection is a boundary problem | h5i", "h1": "Assume the prompt injection worked",
    "description": "Prompt-injection defenses should bound a compromised coding agent's authority: filesystem reach, credentials, sockets, network destinations, and output.",
    "deck": "Detection asks hostile text to reveal that it is hostile. Containment asks a simpler question: if the agent follows every instruction in the repository, what can the resulting process still reach?",
    "body": """
<div class="callout danger"><strong>The operating assumption.</strong> The agent read a malicious instruction, believed it, and is now using every tool exactly as designed. Build the boundary for that case.</div>
<p>Prompt injection is often treated as a classification problem. Find the suspicious sentence. Score the page. Ask another model whether the instruction looks malicious. Block the obvious phrasing.</p>
<p>Those controls can reduce noise. They cannot define the security boundary, because the attacker chooses the text and can iterate against the same cues the detector uses. A repository can hide instructions in documentation, generated files, issue text, tool output, test failures, or a web page the agent opens.</p>
<p>The durable control begins after detection fails.</p>
<h2 id="capabilities">Translate the compromise into capabilities</h2>
<p>Do not ask what the injected agent intends. Ask what its process can do:</p>
<ul><li>Which host paths can it read or write?</li><li>Which credentials exist in its environment or home directory?</li><li>Which network destinations and address families can it reach?</li><li>Which sockets let it borrow authority from another host process?</li><li>Can it write directly into the repository or artifact you will trust?</li></ul>
<p>Each answer should be enforced by something outside the agent.</p>
<h2 id="credentials">A key inside the box is already compromised</h2>
<p>Environment variables and dotfiles are convenient credential delivery systems. They are also readable bytes in the compromised process's authority domain.</p>
<p>A credential broker changes the shape. The real key stays on the host. The box receives a route to a narrow proxy, and the proxy injects authentication only for the allowed service. Scope that route to one runtime. A Claude box should not be able to turn an OpenAI key into a laundering channel merely because both agents are installed on the host.</p>
<p>This does not stop the model service from receiving source included in a legitimate prompt. Source confidentiality against the model is a separate decision: use a self-hosted model or remove model egress.</p>
<h2 id="network">An allowlist is only as strong as its layer</h2>
<p>Proxy variables constrain cooperative applications. A compromised process can clear them and open a socket. If off-list destinations must be unreachable, enforcement has to meet raw traffic: a private network namespace and packet rules, or a VM network stack.</p>
<p>Name the layer. L7 proxy scoping and L3/L4 destination enforcement are not interchangeable promises.</p>
<h2 id="sockets">Local sockets are network authority too</h2>
<p>Unix sockets disappear from many threat models because they do not look like internet access. They can connect the box to SSH agents, desktop services, container daemons, and other privileged processes. Some can carry open file descriptors.</p>
<p>Deny the address family by default. Grant it only to profiles that need it, and keep host sockets outside filesystem grants. A browser control daemon may justify one scoped socket. A test runner usually does not.</p>
<h2 id="output">The final capability is acceptance</h2>
<p>A compromised agent that cannot read secrets or dial arbitrary hosts can still produce a malicious patch. Containment limits blast radius during execution; it does not certify the output.</p>
<p>That is why the box should not merge its own work. Export a path-validated patch and evidence bundle. Review them outside. The human-operated output gate is part of the security design, not workflow polish.</p>
<h2 id="success">What success looks like</h2>
<p>Success is not “the detector found every injection.” Success is that an injected agent encountered the same narrow world as a cooperative one:</p>
<ul><li>the host filesystem was absent except for explicit grants;</li><li>reusable credentials never entered;</li><li>off-list destinations were refused at the claimed layer;</li><li>the process could not reach ambient host sockets;</li><li>the result still required an external decision.</li></ul>
<p>The injection may succeed as language. It fails as authority.</p>""",
    "faq": [
        ("Does sandboxing prevent source code from reaching the model?", "No. A coding agent can include source in an allowed model request. Preventing that requires a self-hosted model or a policy with no model egress."),
        ("Are permission prompts still useful inside a box?", "They can improve usability and catch mistakes, but they are not the security boundary. A prompt-injected agent can approve or bypass its own application-level permissions; the box policy remains outside it."),
    ],
    "next": ("/guides/write-a-box-policy/", "Build the boundary", "Write down what the agent may reach", "Create a fail-closed profile for filesystem, network, and resources."),
    "cta": ("Design for the compromised session", "A narrow box makes prompt-injection success less consequential.", "/guides/write-a-box-policy/", "Write a policy"),
}


ARTICLES = [FIRST_BOX, REVIEW_PR, POLICY, BROWSER, ENVIRONMENT, TIERS, EVIDENCE, INJECTION]


def index_page(section, items):
    guides = section == "guides"
    title = "h5i guides: from first box to reviewed patch" if guides else "The h5i blog: boundaries, evidence, and agent work"
    description = ("Four practical h5i guides: create a first box, review an untrusted pull request, write a policy, and watch the isolated browser." if guides else "Four durable essays on coding-agent sandboxes: the environment boundary, isolation tiers, audit evidence, and prompt-injection containment.")
    h1 = "One path from first box to deliberate boundary" if guides else "Fewer posts. Sharper arguments."
    deck = ("Start at the top and follow the sequence. Each guide has one outcome, commands you can run, a verification step, and the point where human judgment belongs." if guides else "The blog is not a changelog and not a keyword warehouse. These essays explain the design decisions that stay true when commands and releases change.")
    url = f"https://h5i.dev/{section}/"
    schema = {"@context": "https://schema.org", "@type": "ItemList", "name": title,
              "itemListElement": [{"@type": "ListItem", "position": i + 1, "url": f"https://h5i.dev/{section}/{x['slug']}/", "name": x["h1"]} for i, x in enumerate(items)]}
    rows = ""
    for i, item in enumerate(items, 1):
        label = f"Step {i:02d}" if guides else f"Essay {i:02d}"
        rows += f"""<a class="post-card{' featured' if i == 1 else ''}" href="/{section}/{item['slug']}/">
<div class="card-meta"><span>{label}</span><span>{item['time']}</span></div>
<h2>{item['h1']}</h2><p>{item['description']}</p></a>"""
    return f"""{head(title, description, url, schema, kind="website", rss=not guides)}
<body>{NAV}<section class="index-hero"><div class="post-eyebrow">{"Field guides" if guides else "Design essays"}</div>
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
    },
    "guides": {
        "ai-code-review-audit": "review-a-pull-request", "ai-code-provenance": "first-box",
        "secure-api-tokens-in-agent-box": "write-a-box-policy", "prompt-injection-detection-for-agents": "write-a-box-policy",
        "claude-code-memory": "first-box", "codex-claude-code-collaboration": "first-box",
        "git-blame-for-ai-code": "review-a-pull-request", "token-reduction-capture-run": "first-box",
    },
}


def redirect_page(section, new):
    target = f"/{section}/{new}/"
    return f"""<!DOCTYPE html><html lang="en"><head><meta charset="utf-8">
<meta name="robots" content="noindex"><link rel="canonical" href="https://h5i.dev{target}">
<meta http-equiv="refresh" content="0; url={target}"><title>Article moved | h5i</title></head>
<body><p>This article was replaced during the documentation rewrite. <a href="{target}">Read the new article.</a></p></body></html>"""


def build():
    for section in ("blog", "guides"):
        base = ROOT / section
        for child in base.iterdir():
            if child.is_dir():
                shutil.rmtree(child)
        selected = [item for item in ARTICLES if item["section"] == section]
        (base / "index.html").write_text(index_page(section, selected))
        for item in selected:
            out = base / item["slug"]
            out.mkdir()
            (out / "index.html").write_text(article_page(item))
        for old, new in REDIRECTS[section].items():
            out = base / old
            out.mkdir(exist_ok=True)
            (out / "index.html").write_text(redirect_page(section, new))

    core = [("", "1.0"), ("features/", "0.9"), ("manual/", "0.9"), ("workflows/", "0.9"),
            ("guides/", "0.8"), ("blog/", "0.8"), ("pitch/", "0.6"), ("demo/", "0.6")]
    urls = core + [(f"{item['section']}/{item['slug']}/", "0.7") for item in ARTICLES]
    rows = "\n".join(f"  <url><loc>https://h5i.dev/{path}</loc><lastmod>{TODAY}</lastmod><priority>{priority}</priority></url>" for path, priority in urls)
    (ROOT / "sitemap.xml").write_text(f'<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n{rows}\n</urlset>\n')

    posts = [item for item in ARTICLES if item["section"] == "blog"]
    items = "\n".join(f"""    <item><title>{item['h1']}</title><link>https://h5i.dev/blog/{item['slug']}/</link>
      <guid isPermaLink="true">https://h5i.dev/blog/{item['slug']}/</guid><pubDate>Mon, 10 Aug 2026 12:00:00 GMT</pubDate>
      <description>{item['description']}</description></item>""" for item in posts)
    (ROOT / "feed.xml").write_text(f"""<?xml version="1.0" encoding="UTF-8"?><rss version="2.0"><channel>
<title>The h5i Blog</title><link>https://h5i.dev/blog/</link>
<description>Design essays on boundaries, evidence, and autonomous coding work.</description>
<language>en-us</language><lastBuildDate>Mon, 10 Aug 2026 12:00:00 GMT</lastBuildDate>
{items}</channel></rss>""")

    (ROOT / "llms.txt").write_text("""# h5i

> h5i ("high-five") is an open-source integrated sandbox for AI coding agents. It places the agent, workspace, shell, dependencies, dev server, and isolated browser inside one disposable boundary. Host files and reusable credentials stay outside. A human exports a reviewable patch and execution record when the work is done.

## Start here

- [Features](https://h5i.dev/features/): Product overview, isolation tiers, browser, output gate, and console.
- [First box](https://h5i.dev/guides/first-box/): Install h5i and take one task from box creation to a reviewed patch.
- [Workflow](https://h5i.dev/workflows/): The complete box, work, inspect, export, apply loop.
- [Manual](https://h5i.dev/manual/): Authoritative command, policy, receipt, and limitation reference.

## Guides

1. [Take one coding task from prompt to reviewed patch](https://h5i.dev/guides/first-box/): Create, work, inspect, export, and remove a local box.
2. [Run the pull request before you trust the pull request](https://h5i.dev/guides/review-a-pull-request/): Execute external code in a detached box and review evidence before prose.
3. [Write down what the agent may reach](https://h5i.dev/guides/write-a-box-policy/): Define filesystem, network, isolation, and resource policy in .h5i/env.toml.
4. [Watch the page, then take the controls](https://h5i.dev/guides/watch-the-browser/): Run the browser beside the dev server and transfer control without stale handles.

## Design essays

- [The environment is the sandbox](https://h5i.dev/blog/the-environment-is-the-sandbox/): The isolation unit is the entire development session, not one command or checkout.
- [Five tiers, five different promises](https://h5i.dev/blog/choosing-agent-isolation/): Choose process, supervised, container, or microVM isolation by the property required.
- [A transcript is not an audit trail](https://h5i.dev/blog/evidence-for-agent-work/): Separate host-observed evidence, box-claimed records, Git state, and agent testimony.
- [Assume the prompt injection worked](https://h5i.dev/blog/prompt-injection-is-a-boundary-problem/): Bound a compromised session's filesystem, credentials, sockets, egress, and output.

## Core model

- A box is a complete disposable development environment for one agent.
- Five tiers: workspace, process, supervised, container, microvm.
- Explicit isolation requests fail closed; h5i never silently downgrades.
- supervised and microvm enforce egress at L3/L4. container uses an L7 proxy allowlist.
- Model credentials remain host-side and are injected by a runtime-scoped proxy.
- h5i box export produces patch.diff, report.md, and receipt.json.
- Every receipt names the policy digest and the observer lane.
- h5i is local-first, Apache-2.0, and requires no hosted sandbox or SaaS account.

## Honest limits

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

Every canonical article needs one H1; descriptive metadata; canonical, Open
Graph, and Twitter tags; TechArticle and BreadcrumbList JSON-LD; visible FAQ
text when FAQPage data is present; useful internal links; a current
dateModified; and inclusion in sitemap.xml. Blog essays also enter feed.xml.

Before publishing, remove repeated setup, claims without mechanisms, invented
precision, and references to features the manual no longer documents. Then
read the opening callout and every heading without the body. They should still
tell the whole story.
""")


if __name__ == "__main__":
    build()
