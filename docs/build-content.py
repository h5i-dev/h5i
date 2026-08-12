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
    "section": "guides", "slug": "review-a-pull-request", "eyebrow": "Guide 02 / Untrusted code",
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
    "section": "guides", "slug": "write-a-box-policy", "eyebrow": "Guide 03 / Policy",
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
<ul><li><a href="/manual/#policy">Complete policy reference and built-in profiles</a>.</li><li><a href="/manual/#credentials">Credential and secret grants</a>.</li><li><a href="/blog/choosing-agent-isolation/">The threat model behind the tier choice</a>.</li><li><a href="https://github.com/h5i-dev/h5i/blob/main/docs/credential-proxy-design.md">Credential proxy design and open limits</a>.</li></ul>""",
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
<ul><li><a href="/manual/#h5i-box-view">Browser and terminal viewer reference</a>.</li><li><a href="/manual/#driving-the-browser-itself">Browser engine and control-lock commands</a>.</li><li><a href="/blog/prompt-injection-is-a-boundary-problem/">Why browser authority changes the injection threat model</a>.</li><li><a href="https://github.com/h5i-dev/h5i/tree/main/crates/h5i-browser-light">The local browser implementation</a>.</li></ul>""",
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
    "eyebrow": "Essay / Architecture", "time": "12 min", "tags": "Sandbox &middot; Agent loop &middot; Browser",
    "title": "The environment is the sandbox | h5i", "h1": "The environment is the sandbox",
    "description": "Coding agents do not execute one risky command. They operate a development environment, so that whole environment must become the security boundary.",
    "deck": "Sandboxing one shell command was the right idea at the wrong scale. A coding agent operates a repository, package manager, compiler, dev server, and browser. Leave one outside and the boundary has a door in it.",
    "body": """
<div class="callout"><strong>The claim.</strong> The unit of isolation for a coding agent is the complete development environment—not the model process, not the shell command, and not the Git checkout.</div>
<figure class="feature-figure"><img src="/_static/fast-supervised-sandbox.svg" alt="A developer starts one h5i box containing the agent, workspace, process controls, network gate, and resource limits"><figcaption>The fast path is still a complete boundary. The agent and every process it starts inherit the same filesystem, syscall, network, and resource policy.</figcaption></figure>
<p>Consider the apparently harmless task “upgrade the date library and fix the failing tests.” The agent edits one manifest and runs the package manager. The package manager resolves forty transitive dependencies. One of them executes a post-install script. The script reads the environment, probes the home directory, opens a socket, and exits successfully. The final diff contains a version bump and a lockfile. Nothing in those two files records the interesting part.</p>
<p>That is the scale mismatch. We tend to draw the risky object as the agent's shell command, while the work actually fans out into a temporary software supply chain. The command is only the first edge.</p>
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
<p>If those answers cross the boundary in different directions, the sandbox is smaller than the work.</p>
<h2 id="inheritance">The boundary has to follow the process tree</h2>
<p>A useful sandbox does not ask whether the current executable is called Claude, Codex, npm, cargo, or bash. Names are not security properties. It constrains the process tree that begins with the session.</p>
<p>That distinction matters the moment a tool delegates. A test runner starts workers. A compiler launches a linker. A package manager runs lifecycle hooks. A dev server invokes a bundler, which may invoke a native addon build. If confinement is implemented as a polite wrapper around the top-level command, the first child that does not use the wrapper has left the model.</p>
<p>h5i makes <code>box shell</code> and <code>box run</code> the entry points into a resolved policy. At the kernel tiers, filesystem and syscall restrictions are inherited. At <code>supervised</code>, the session also lives in a private network namespace with its own destination rules. At the image tiers, the whole tree starts inside the container or guest. The agent does not decide which child deserves the boundary. Children get it because they are children.</p>
<h2 id="failure-matrix">What escapes when one component stays outside</h2>
<div class="tbl-wrap"><table class="data"><thead><tr><th>Component left outside</th><th>What crosses the boundary</th><th>Why the diff stays quiet</th></tr></thead><tbody>
<tr><th>Package manager</th><td>Install scripts execute as the host user</td><td>Reads, failed probes, and network attempts need not edit the tree</td></tr>
<tr><th>Dev server</th><td>Generated code and plugins run on the host</td><td>The server may only serve or transmit data</td></tr>
<tr><th>Browser</th><td>Host cookies, extensions, downloads, and loopback become reachable</td><td>Browser state lives outside Git</td></tr>
<tr><th>Agent configuration</th><td>Ambient credentials and privileged tool routes enter the session</td><td>Authority is configuration, not source</td></tr>
<tr><th>Output step</th><td>The subject of review can approve its own result</td><td>A direct write looks like any other edit</td></tr>
</tbody></table></div>
<p>The table is why “the agent itself is sandboxed” is not enough information. Ask where the work's other interpreters run. Every package hook, compiler plugin, test fixture, web page, and browser extension is another interpreter for input you may not control.</p>
<h2 id="lifecycle">One object gives the work a reviewable lifecycle</h2>
<p>Once the environment is the object, the workflow becomes easier to reason about:</p>
<ol><li><strong>Create:</strong> freeze the Git base, resolve the profile, and hash the policy before writable state exists.</li><li><strong>Work:</strong> let the agent edit, build, run services, and use the browser within that policy.</li><li><strong>Observe:</strong> record process exits and boundary decisions outside the agent's write path.</li><li><strong>Review:</strong> compare the final tree with the pinned base and read execution evidence beside it.</li><li><strong>Export or apply:</strong> move one reviewed result across a human-operated gate.</li><li><strong>Remove:</strong> discard the workspace without turning it into a permanent pet environment.</li></ol>
<p>The frozen base and policy digest are more than metadata. They prevent the meaning of “this run” from drifting. If the parent branch moves or the policy file changes later, the box still names the code and rules it actually started with.</p>
<h2 id="not-claim">What this design does not claim</h2>
<p>A complete boundary can still have a weak tier. <code>workspace</code> gives checkout hygiene and no process confinement. Every tier below <code>microvm</code> shares the host kernel. A container's HTTP proxy cannot constrain a raw socket that ignores it. The boundary is one object; its strength still depends on the mechanism chosen for that object.</p>
<p>Containment also does not certify the patch. A malicious or simply wrong agent can produce code that passes the tests it chose to run. The output gate creates a place for judgment; it does not automate judgment away.</p>
<p>And no local sandbox can stop source from entering a model request that policy legitimately permits. If source must not leave, the answer is a self-hosted model or no model egress—not stronger language around the same allowed API call.</p>
<h2 id="economics">Security becomes normal only when disposal is economical</h2>
<p>There is an operational reason integrated environments beat a checklist of wrappers. Developers stop using expensive safety mechanisms for ordinary work. If every agent session requires building an image, negotiating a remote worker, and waiting minutes for dependencies, the box is reserved for code already known to be dangerous. Most supply-chain surprises arrive in code nobody preclassified that way.</p>
<p>The lightweight tiers attack startup cost. Warm caches attack dependency cost without creating a writable rendezvous between boxes: cache content is keyed by lockfile state, populated by a dedicated refresh job, and mounted read-only into agent work. The browser and dev server start inside the already-created boundary, so testing a web change does not require publishing a host port or attaching to a daily browser.</p>
<p>Disposal matters at the other end. A long-lived development container accumulates credentials, caches, debugging exceptions, and manual fixes until nobody can state its boundary. A box has a frozen base, one resolved policy, one purpose, and an expected end. Export what deserves to survive. Remove the rest.</p>
<p>This gives containment a property security tooling rarely gets: the safer workflow is also easier to reason about. One name identifies the workspace, policy, process tree, browser, receipts, and cleanup target. There are fewer ambient pieces for both the agent and the reviewer to misunderstand.</p>
<h2 id="sources">Sources and further reading</h2>
<ul><li><a href="/manual/#the-loop">The h5i manual: the loop</a>, for the command-level lifecycle.</li><li><a href="/manual/#isolation-tiers">Isolation tiers</a>, for the enforcement and limits of each boundary.</li><li><a href="/guides/first-box/">The first-box guide</a>, for running the complete loop on a real repository.</li><li><a href="https://github.com/h5i-dev/h5i/blob/main/README.md">The project README</a>, for the current product claim and explicit non-claims.</li></ul>""",
    "faq": [
        ("Is a Git worktree an agent sandbox?", "No. A worktree separates checkouts and branches. It does not constrain the process tree, filesystem reads, credentials, sockets, network destinations, or browser state."),
        ("Why does the browser need to be inside?", "The browser executes untrusted page code and holds session state. Keeping it beside the dev server gives both the same isolated localhost while preventing the agent from inheriting a user's normal browser profile."),
    ],
    "next": ("/blog/choosing-agent-isolation/", "Read next", "Five tiers, five promises", "Choose an isolation mechanism by the threat it changes."),
    "cta": ("Try the whole loop once", "Create a box, do one real task, and review the patch beside the execution record.", "/guides/first-box/", "Follow the first-box guide"),
}


TIERS = {
    "section": "blog", "slug": "choosing-agent-isolation", "eyebrow": "Essay / Threat model",
    "time": "13 min", "tags": "Landlock &middot; Containers &middot; MicroVMs",
    "title": "How to choose isolation for a coding agent | h5i", "h1": "Five tiers, five different promises",
    "description": "Choose coding-agent isolation by threat model: checkout separation, process confinement, L3/L4 egress control, portable containers, or a separate kernel.",
    "deck": "Isolation is not a single strength meter. A container can improve portability while weakening network control; a microVM can strengthen the kernel boundary while producing thinner egress evidence.",
    "body": """
<div class="callout"><strong>The short answer.</strong> Use <code>process</code> for fast local confinement, <code>supervised</code> when off-list network access must fail at L3/L4, <code>container</code> when the image matters, and <code>microvm</code> when sharing the host kernel is unacceptable. <code>workspace</code> is separation, not confinement.</div>
<figure class="feature-figure"><img src="/_static/microvm-sandbox.svg" alt="The h5i microVM tier places the agent and workspace behind a guest kernel and virtual network stack"><figcaption>A microVM changes the kernel trust boundary. It does not automatically win every other dimension: startup, credential routing, and evidence all have separate tradeoffs.</figcaption></figure>
<p>The tempting diagram is a staircase: worktree at the bottom, VM at the top, and one word—“security”—rising with every step. That diagram is easy to sell and bad at helping anyone choose.</p>
<p>Imagine two runs. The first builds ordinary code from your own repository but must start in a fraction of a second. The second opens a stranger's pull request containing native build scripts. The third must reproduce exactly in CI. The fourth is allowed to contact one package registry and absolutely nothing else. Those tasks want different properties. Giving all four the same “strongest” tier either wastes time or quietly misses the control that mattered.</p>
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
<p>The honest interface is boring: probe, choose, create, inspect the resolved policy.</p>
<h2 id="workspace-detail">Workspace is useful precisely because it makes no security claim</h2>
<p>The workspace tier gives the agent a separate Git worktree, branch, index, and pinned base. That prevents ordinary checkout collisions and makes comparison clean. It is excellent hygiene for a trusted tool and the wrong answer for untrusted code.</p>
<p>Calling it a sandbox would make every later decision worse. The process still runs as you. It sees the host filesystem, environment, sockets, network, and kernel. h5i keeps the rung because checkout isolation is sometimes the only requested property, and labels it as unconstrained because names should not smuggle guarantees.</p>
<h2 id="process-detail">Process confinement is the fast, inherited boundary</h2>
<p>At <code>process</code>, the session receives filesystem allowlists, syscall restrictions, namespaces, and resource controls where the host can enforce them. On Linux, Landlock makes the allowed filesystem tree explicit and seccomp removes dangerous syscall families. The important part is not any one primitive. It is that the restrictions inherit across the session's descendants.</p>
<p>This tier works well for the daily loop: edit, compile, test, repeat. It does not provide an L3/L4 destination allowlist. Network policy here is coarse—deny it or let it use the host network—and the host kernel remains shared.</p>
<h2 id="container-detail">Container is a reproducibility choice with a security boundary attached</h2>
<p>Rootless Podman lets the repository name an OCI image. h5i runs it with all capabilities dropped, no-new-privileges, a read-only root filesystem, private IPC, a bounded tmpfs, and only the intended mounts. Runs never pull: <code>--pull=never</code> makes the environment depend on the image you prepared, not on what a registry served at session start.</p>
<p>That makes the container tier compelling when “same toolchain everywhere” is the requirement. It also gives real memory and process ceilings on platforms where the host kernel tiers cannot. But its egress allowlist is a CONNECT proxy. Most package managers and HTTP clients respect it. A program opening its own raw socket does not. Portability is the primary reason to choose this rung.</p>
<h2 id="microvm-detail">MicroVM moves the shared-kernel line</h2>
<p>The microVM adapter boots a guest from the same class of OCI image, through microsandbox. The agent's process, filesystem view, and network stack sit behind a guest kernel. A kernel exploit inside the workload therefore meets the hypervisor rather than continuing directly in the host kernel it attacked.</p>
<p>That property has three concrete prerequisites: a compatible <code>msb</code> binary, usable hardware virtualization, and a pre-pulled image. If any is absent, an explicit <code>microvm</code> request refuses. The command does not “helpfully” fall back to a shared-kernel tier.</p>
<p>The cost is not only startup. Host-loopback credential grants do not currently cross into the guest, so profiles declaring them are refused. The guest network stack enforces destination rules but does not yet return a per-request deny tally, so the boundary can be stronger while the report is less detailed.</p>
<h2 id="probe-detail">What a trustworthy probe has to prove</h2>
<p>Feature detection is full of false positives. A kernel can expose Landlock while a policy prevents the final exec. A <code>/dev/kvm</code> node can exist but be unreadable. Podman can be installed and configured rootful when the tier requires rootless operation.</p>
<p><code>h5i box probe</code> separates facts from claims. It identifies mechanisms, checks prerequisites, and runs a minimal confined action for the lightweight tier. Then <code>box status</code> answers a different question: what did this particular box actually receive? Finally, <code>box doctor</code> asks whether the stored box can still keep that claim on this host today.</p>
<div class="terminal"><div class="terminal-bar"><span class="terminal-path">three questions</span></div><div class="terminal-body"><pre><code>$ h5i box probe
# What can this host enforce?
$ h5i box status review-1234
# What policy was resolved for this box?
$ h5i box doctor review-1234
# Can that box still uphold the stored claim?</code></pre></div></div>
<h2 id="decision">Choose by the first unacceptable failure</h2>
<ul><li>If host checkout collisions are the only concern, use <code>workspace</code>.</li><li>If a runaway agent or dependency must not roam the filesystem, start at <code>process</code>.</li><li>If off-list raw network traffic must fail, use <code>supervised</code>.</li><li>If a pinned image and portable toolchain matter most, use <code>container</code> and accept its L7 egress scope.</li><li>If the workload must not share the host kernel, use <code>microvm</code> and accept the heavier prerequisites.</li></ul>
<p>This is not a score. It is a threat model stated as an operational choice.</p>
<h2 id="examples">Four runs, four defensible choices</h2>
<div class="tbl-wrap"><table class="data"><thead><tr><th>Run</th><th>First unacceptable failure</th><th>Tier</th><th>Why</th></tr></thead><tbody>
<tr><th>Rename an internal function</th><td>Agent edits the developer's checkout</td><td><code>workspace</code></td><td>Trusted code and toolchain; only tree separation is required</td></tr>
<tr><th>Update dependencies in your app</th><td>Lifecycle script reads outside the worktree</td><td><code>process</code></td><td>Fast inherited filesystem and syscall confinement</td></tr>
<tr><th>Execute a stranger's pull request</th><td>Raw connection reaches an off-list host</td><td><code>supervised</code></td><td>Detached source plus packet-layer egress enforcement</td></tr>
<tr><th>Compile a hostile native fixture</th><td>Guest code exploits a shared kernel</td><td><code>microvm</code></td><td>The guest kernel and hypervisor change the trust boundary</td></tr>
</tbody></table></div>
<p>A fifth case—reproducing a precise Linux toolchain across laptops and CI—may choose <code>container</code> even though supervised has stronger network enforcement. The image is the requirement. This is exactly why a single strength score obscures more than it reveals.</p>
<h2 id="platform">The same tier name can have platform-specific limits</h2>
<p>Linux supplies Landlock, seccomp, namespaces, nftables, and cgroups. macOS uses Seatbelt for filesystem and process policy and does not have a per-box equivalent to every cgroup control. Pretending the rows are identical would turn portability into fiction.</p>
<p>h5i reports unenforced memory and process values at the macOS kernel tiers instead of listing them as active. The image tiers can supply runtime-level ceilings there. The right workflow is to inspect <code>status</code> on the machine that ran the box, not infer enforcement from the profile alone.</p>
<h2 id="sources">Sources and further reading</h2>
<ul><li><a href="/manual/#isolation-tiers">The manual's isolation-tier reference</a>, including platform-specific limits.</li><li><a href="/manual/#h5i-box">The box lifecycle</a>, including probe, capabilities, status, and doctor.</li><li><a href="/guides/write-a-box-policy/">Write a box policy</a>, for turning the choice into a checked-in profile.</li><li><a href="https://github.com/h5i-dev/h5i/tree/main/crates/h5i-sandbox">The h5i-sandbox source</a>, where the tier adapters and enforcement mechanisms live.</li></ul>""",
    "faq": [
        ("Is microVM always the best tier?", "No. It gives the strongest kernel boundary, but costs more to start and currently provides thinner denied-egress evidence. Choose it when a separate kernel is the property the task requires."),
        ("Is a container stronger than process isolation?", "Not in every dimension. It improves image portability. In h5i, its L7 proxy allowlist is weaker against raw sockets than the supervised tier's L3/L4 enforcement."),
    ],
    "next": ("/guides/write-a-box-policy/", "Put it into practice", "Write a box policy", "Turn the threat model into a profile the repository can review."),
    "cta": ("Ask the host what it can enforce", "Run the functional probe before choosing a tier by name.", "/manual/#isolation-tiers", "Compare the tiers"),
}


EVIDENCE = {
    "section": "blog", "slug": "evidence-for-agent-work", "eyebrow": "Essay / Review",
    "time": "12 min", "tags": "Receipts &middot; Audit &middot; Diffs",
    "title": "What counts as evidence for agent-written code? | h5i", "h1": "A transcript is not an audit trail",
    "description": "Review agent-written code with evidence collected outside the agent: the patch, observed execution, denied activity, and explicit gaps in observation.",
    "deck": "The agent can explain what it did in perfect prose. That account is useful—and it is still testimony from the subject of the review. Evidence begins where self-report ends.",
    "body": """
<div class="callout"><strong>The test.</strong> Point at a line in the record and ask: <em>could the agent have written this?</em> If yes, keep it as testimony. Do not promote it into independent evidence.</div>
<figure class="feature-figure"><img src="/_static/sandbox-ui-demo.png" alt="The h5i box console showing policy, diff statistics, and execution events in separate lanes"><figcaption>The useful view does not compress everything into one confidence score. Policy, tree state, process events, network decisions, and page evidence keep their own provenance.</figcaption></figure>
<p>Suppose an agent returns a twelve-line authentication fix and this summary: “Implemented token refresh, added regression coverage, all tests pass.” The prose is specific. The patch is small. The test file exists. This is exactly the kind of result that feels easy to approve.</p>
<p>Now add three observations the summary omitted: the full test suite was never run; one command exited 137 after hitting the memory ceiling; and the process tried seven times to reach a telemetry host outside policy. None proves the patch is malicious. All three change the review. The difference between the two versions of the story is not better summarization. It is independent collection.</p>
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
<p>This order does not replace code review. It stops eloquent testimony from framing the evidence before you see it.</p>
<h2 id="receipt-anatomy">What a useful receipt has to bind together</h2>
<p>An event record becomes reviewable when it answers more than “what text was printed?” At minimum it needs the command or event kind, time, exit result, observer, payload reference, and the digest of the policy in force. Without the digest, a clean-looking run can be separated from the rules that supposedly constrained it. Without the observer, testimony and observation collapse into the same JSON shape.</p>
<p>The pinned base matters for the same reason. A patch is meaningful only relative to the tree it changed. A receipt is meaningful only relative to the boundary that produced it. h5i freezes both at box creation so a later branch update or profile edit cannot rewrite the premise of an old run.</p>
<div class="terminal"><div class="terminal-bar"><span class="terminal-path">review bundle</span></div><div class="terminal-body"><pre><code>review/
├── patch.diff      # tree state against the frozen base
├── report.md       # evidence ordered for a human review
└── receipt.json    # events, observer lanes, policy digest</code></pre></div></div>
<h2 id="two-paths">Observation needs two paths, because each catches the other's lie</h2>
<p>The host supervisor can reliably observe the child it launched: start, exit status, elapsed time, and resource use. It cannot reconstruct every command typed inside a long-lived interactive shell. An in-box hook can report that detail, including nested shell commands. The box can also suppress or fabricate that hook.</p>
<p>The right response is not to choose one and pretend it is complete. Keep both. A host-observed shell exit with no matching in-box command is a gap worth showing. An in-box command claiming success beside a host-observed nonzero exit is a disagreement worth showing. Redundancy becomes evidence only when sources remain distinguishable.</p>
<h2 id="denials">Denied actions are often the most valuable rows</h2>
<p>Successful work dominates normal logs. Security review is disproportionately interested in failed attempts: a path outside the grant, a refused address family, an off-list destination, a resource ceiling, a browser verb denied at the control socket.</p>
<p>A diff cannot represent any of these because the attempt left no accepted state change. A transcript may omit them because they were noisy or because the agent moved on. The boundary that made the decision is the natural observer, and its denial should appear before the agent's proposal in the report.</p>
<p>Do not overread a denial. A build tool reaching a telemetry endpoint can be normal. Seven retries can reflect an ordinary client. The record does not assign intent. It tells the reviewer where intent is worth investigating.</p>
<h2 id="tamper-model">“Sealed” is a local, scoped claim</h2>
<p>The box's writable spool is a staging area. Receipt storage is a sibling outside its grants. The host ingests staged records and assigns ordering. A process inside the box cannot reopen yesterday's host record and change an exit code.</p>
<p>A user who owns the host can still edit the file. There is no external timestamp authority, signature service, or transparency log. Calling the receipt tamper-proof would be false. The actual claim is narrower and useful: the subject of the review has no filesystem path to its ingested history.</p>
<h2 id="worked-review">A worked review order</h2>
<p>For an unattended change, begin with the highest-consequence, lowest-interpretation evidence:</p>
<ol><li><strong>Policy and tier.</strong> Was the session actually confined, and at which layer was egress enforced?</li><li><strong>Denials and limits.</strong> What did the boundary stop? What died under a ceiling?</li><li><strong>Observed execution.</strong> Which meaningful build and test commands have externally observed exits?</li><li><strong>Page evidence.</strong> Were there console exceptions or failed requests? Was a browser even available?</li><li><strong>Patch.</strong> Does the state change match the execution story?</li><li><strong>Proposal.</strong> What does the agent believe it achieved, and where does that account diverge?</li></ol>
<p>The order is intentionally unfriendly to polished prose. By the time you read the summary, you already know which claims need proof.</p>
<h2 id="anti-patterns">Four ways evidence turns back into decoration</h2>
<h3>One risk score</h3>
<p>Combining a denied destination, a failed test, a large diff, and a box-claimed command into “risk: 72” destroys the semantics a reviewer needs. The score cannot explain whether the boundary stopped something or whether the agent merely said it did. Keep the lanes. Let the human weigh them for the task.</p>
<h3>Only successful final runs</h3>
<p>Retries contain the debugging story. A failure followed by a pass can be ordinary progress, or the agent can have weakened an assertion until it turned green. Retaining only the final exit removes the comparison that makes the patch intelligible.</p>
<h3>Unlimited payloads</h3>
<p>Raw command output can be enormous and attacker-controlled. Evidence collection needs byte caps, truncation markers, redaction, and payload references. Otherwise one verbose build can make the review artifact unusable—or push secrets into every downstream index built from it.</p>
<h3>Silence as green</h3>
<p>An empty array is not a universal success state. It may mean the observer saw no errors, the subsystem was never started, the tier cannot report that class, or collection failed. Good schemas make these states distinct before a UI assigns color.</p>
<h2 id="compare">Compare claims across artifacts, not only within one log</h2>
<p>The strongest review questions cross boundaries. The proposal says tests pass; do host-observed exits contain the meaningful suite? The patch adds a network client; does the report show new destinations or repeated refusals? The browser screenshot looks correct; were there console exceptions? The profile says no network; does status show that the resolved tier could enforce the claim?</p>
<p>This is where a bundle beats a transcript. Patch, report, receipt, and policy digest are deliberately different views. Agreement increases confidence. Disagreement tells you exactly where to look.</p>
<h2 id="sources">Sources and further reading</h2>
<ul><li><a href="/manual/#receipts">The receipt reference</a>, including observer lanes and explicit limits.</li><li><a href="/manual/#h5i-box-export">The export bundle</a>, for report ordering and path validation.</li><li><a href="/guides/review-a-pull-request/">Review a pull request in a detached box</a>, for the evidence-first workflow.</li><li><a href="https://github.com/h5i-dev/h5i/tree/main/crates/h5i-core">h5i-core</a>, where box state, policy digests, and receipt storage are implemented.</li></ul>""",
    "faq": [
        ("Is an h5i receipt tamper-proof?", "It is protected from the box, not from a user who controls the host. h5i stores ingested receipts outside every filesystem grant held by the box; it does not provide third-party notarization."),
        ("Why keep agent-reported records at all?", "They provide useful detail that an external observer may not have. The requirement is to label their source and compare them with host-observed events, not to discard testimony."),
    ],
    "next": ("/guides/review-a-pull-request/", "Use the method", "Review a pull request by running it", "Read the report in an evidence-first order."),
    "cta": ("Put the record beside the patch", "Export both, then review each artifact for the question it can actually answer.", "/manual/#receipts", "Read the receipt reference"),
}


INJECTION = {
    "section": "blog", "slug": "prompt-injection-is-a-boundary-problem",
    "eyebrow": "Essay / Security", "time": "12 min", "tags": "Prompt injection &middot; Least authority &middot; Egress",
    "title": "Prompt injection is a boundary problem | h5i", "h1": "Assume the prompt injection worked",
    "description": "Prompt-injection defenses should bound a compromised coding agent's authority: filesystem reach, credentials, sockets, network destinations, and output.",
    "deck": "Detection asks hostile text to reveal that it is hostile. Containment asks a simpler question: if the agent follows every instruction in the repository, what can the resulting process still reach?",
    "body": """
<div class="callout danger"><strong>The operating assumption.</strong> The agent read a malicious instruction, believed it, and is now using every tool exactly as designed. Build the boundary for that case.</div>
<figure class="feature-figure"><img src="/_static/browser-authority-threat-model.svg" alt="Four sources of browser authority: live sessions, extensions, localhost access, and attacker-controlled page instructions"><figcaption>The browser makes the prompt-injection problem concrete: attacker-controlled text arrives inside a client carrying ambient sessions, standing grants, and loopback reach.</figcaption></figure>
<p>A repository asks the agent to “read the setup notes before running tests.” The notes include a hidden instruction: inspect the user's SSH directory, send the interesting files to a diagnostics endpoint, then continue with the original task. Nothing in that chain requires a memory-safety exploit. Reading files, making requests, and following repository instructions are the agent's advertised capabilities.</p>
<p>The security question is therefore not whether the instruction looks suspicious to a model. It is whether the resulting process can read the directory, reach the endpoint, or carry a reusable credential there.</p>
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
<p>The injection may succeed as language. It fails as authority.</p>
<h2 id="detection">Why detection remains useful but cannot carry the boundary</h2>
<p>Filters can catch crude attacks. A reviewer can notice an instruction in a README. A second model can flag text that asks for secrets. Tool descriptions can be scanned before they enter context. These controls reduce exposure and improve triage.</p>
<p>They still operate on the attacker's representation. Rename the file, split the instruction across tool outputs, encode the payload in a test failure, or make the dangerous action look like a legitimate debugging step. A sufficiently strict filter also blocks real work, because coding routinely requires reading configuration, opening documentation, and sending authenticated requests.</p>
<p>Containment works on the action after ambiguity has ended. Whatever prose led to <code>open()</code>, <code>socket()</code>, or a write outside the workspace no longer matters to the enforcement decision.</p>
<h2 id="browser-chain">The browser chains ambient authority without exposing a token</h2>
<p>A normal browser profile is the sharpest example. It holds cookies for source control, email, CI, cloud dashboards, and internal tools. The agent never needs to read those cookies. It navigates and the browser authenticates the request automatically.</p>
<p>The same process reaches host loopback, where developer services often rely on “local only” instead of authentication. And the browser's primary input is page content controlled by someone else. Prompt injection becomes the instruction channel joining live sessions, extensions, and local services.</p>
<p>A fresh browser profile inside the box removes the inherited sessions and extensions. A private network namespace changes loopback from “the developer's machine” to “this disposable environment.” The page can still inject the agent. The injected agent finds much less authority waiting for it.</p>
<h2 id="broker">A credential broker removes the reusable secret from the compromise</h2>
<p>Model access creates an awkward exception. The box must call Anthropic or OpenAI, and the ordinary implementation puts the API key in an environment variable or credential file inside the very process we are assuming compromised.</p>
<p>h5i can instead point the client at a host-side broker. The box presents a per-run dummy token. The broker pins the upstream origin, validates origin-form request targets, strips the dummy, injects the real host credential, and creates the TLS request itself. Stealing the dummy gives an attacker no reusable API credential.</p>
<p>The broker is authentication plumbing, not authorization. A broad GitHub token remains broad when used through a broker. Fine-grained service credentials are still required. Nor can the broker stop a legitimate model call from containing private source. It removes credential possession; it does not inspect intent.</p>
<h2 id="layers">Build the response as independent layers</h2>
<div class="tbl-wrap"><table class="data"><thead><tr><th>Attack step</th><th>Boundary response</th><th>Residual risk</th></tr></thead><tbody>
<tr><th>Read host secrets</th><td>Do not grant host paths; seed a scrubbed per-box HOME</td><td>Files intentionally copied into the workspace remain readable</td></tr>
<tr><th>Steal a model key</th><td>Keep the real key behind a runtime-scoped broker</td><td>Allowed model requests can still contain source</td></tr>
<tr><th>Exfiltrate to a new host</th><td>Default-deny egress at the claimed layer</td><td>Allowed destinations remain reachable</td></tr>
<tr><th>Borrow a local daemon</th><td>Deny Unix sockets and isolate loopback by default</td><td>Explicit socket grants carry real authority</td></tr>
<tr><th>Ship a malicious patch</th><td>Require export and external review</td><td>A reviewer can still make a bad decision</td></tr>
</tbody></table></div>
<p>No row depends on the model agreeing that it has been compromised. That independence is the design.</p>
<h2 id="limits">The honest stopping point</h2>
<p>A sandbox does not solve prompt injection as a language problem. The agent may still waste time, damage its disposable workspace, produce a deceptive explanation, or create a malicious patch. A shared-kernel tier remains exposed to a targeted kernel exploit. An allowlisted service can still be abused within its authorization.</p>
<p>The goal is narrower: prevent untrusted instructions from inheriting the developer's ambient machine authority. That turns a successful injection from an account- and host-level incident into a failed or reviewable action inside a disposable box.</p>
<h2 id="usual-mitigations">Why the usual mitigations stop one layer too early</h2>
<p><strong>“I require approval for shell commands.”</strong> The dangerous action can be hidden among dozens of ordinary build and inspection calls. Approval fatigue is the predictable output of putting a human gate on a machine-rate loop. More importantly, package hooks and browser requests may occur below the command the human approved.</p>
<p><strong>“The browser verbs are read-only.”</strong> Navigation can trigger state changes through confirmation links and poorly designed GET endpoints. Reading page content is itself an exfiltration primitive once that content enters the model context. Verb names do not define authority.</p>
<p><strong>“The key is encrypted on disk.”</strong> The agent does not need to decrypt a browser cookie store if it can drive the browser that decrypts and attaches cookies. It does not need to steal an API key if an allowed client will make any request it asks for. Protecting bytes at rest does not constrain the authorized interpreter.</p>
<p><strong>“The agent runs in a container.”</strong> A container with the host browser profile mounted, a Docker socket exposed, or host loopback reachable still carries the authority that matters. Containerization is a mechanism, not a completed threat model.</p>
<p><strong>“The model vendor blocks prompt injection.”</strong> Even a strong detector cannot mediate a native install script, a compromised compiler plugin, or an ordinary request that is dangerous only because of the credential and destination available in this environment. The enforcement point has to sit where the action becomes concrete.</p>
<h2 id="incident-test">Use the compromise drill</h2>
<p>Take a real profile and assume the agent has become adversarial. Ask it to enumerate readable host paths, reusable credentials, reachable internet hosts, reachable loopback services, Unix sockets, and direct write paths to accepted code. Do not actually exfiltrate data; inspect the resolved policy and exercise harmless denials.</p>
<p>If the answer relies on “the agent would not do that,” the authority is still present. If the answer is “the syscall, packet, or export path is refused and the refusal is recorded,” the boundary can survive the model making the wrong semantic decision.</p>
<h2 id="sources">Sources and further reading</h2>
<ul><li><a href="/manual/#credentials">Credentials</a> and <a href="/manual/#af_unix-sockets">Unix sockets</a> in the manual.</li><li><a href="/guides/write-a-box-policy/">Write a box policy</a>, for expressing filesystem, network, and socket authority.</li><li><a href="/guides/watch-the-browser/">Watch the isolated browser</a>, for the fresh-profile and control-lock workflow.</li><li><a href="https://github.com/h5i-dev/h5i/blob/main/docs/credential-proxy-design.md">Credential proxy design</a>, including the origin-pinning and SSRF threat model.</li></ul>""",
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
