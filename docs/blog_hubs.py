"""The five canonical engineering essays on h5i."""


BLOG = [
    {
        "section": "blog", "published": "2026-10-21", "slug": "building-a-verifiable-web-framework",
        "eyebrow": "Engineering / Framework", "time": "7 min",
        "tags": "Rust &middot; Axum &middot; Lean 4",
        "title": "Building a Verifiable Web Framework in Rust | h5i",
        "h1": "Building a Verifiable Web Framework in Rust",
        "description": "Why h5i-app puts a small Rust decision kernel between Axum handlers and storage, then translates that kernel to Lean 4 for proofs.",
        "deck": "The difficult part of verifying a web app is deciding which code and assumptions a theorem actually covers. h5i-app makes that boundary visible in the architecture.",
        "body": """
<div class="callout"><strong>The design constraint.</strong> Keep ordinary Rust and Axum for the web server, but make application decisions small enough to translate and prove. h5i-app does that with a Rust kernel between HTTP and storage.</div>
<figure class="feature-figure"><img src="/_static/calculator-rust-lean-proof.svg" alt="A Rust calculator kernel becomes a Lean 4 definition checked against a separate specification"><figcaption>Rust supplies the implementation. The Lean specification supplies the rule to check.</figcaption></figure>
<h2 id="problem">The problem with proving a handler helper</h2>
<p>An Axum handler can authenticate, authorize, query PostgreSQL, update rows, and render a response in one place. You can prove a helper function correct and still accidentally bypass it in another route. You can also authorize one object and issue a database write that touches more objects. The architecture has to constrain the path from request to committed change.</p>
<div class="tbl-wrap"><table class="data"><thead><tr><th>Layer</th><th>Responsibility</th><th>How it is checked</th></tr></thead><tbody>
<tr><th>Axum and auth</th><td>Parse HTTP and identify the actor</td><td>Trusted boundary; test the running route</td></tr>
<tr><th>Rust kernel</th><td>Decide the write and reply from actor, snapshot, and command</td><td>Translate with Aeneas; prove properties in Lean 4</td></tr>
<tr><th>Storage path</th><td>Apply writes and compile supported SQL</td><td>Extracted components and modeled SQL proofs; PostgreSQL remains trusted</td></tr>
</tbody></table></div>
<p>The kernel is a <em>decision surface</em>, not an HTTP framework hidden inside Lean. Axum still handles transport. The engine loads the snapshot, invokes the kernel, and applies its proposed write. The table shows why a theorem about kernel logic must be stated with a trust boundary.</p>
<h2 id="kernel">A concrete Rust decision</h2>
<p>The <a href="https://github.com/h5i-dev/h5i/blob/main/examples/app/calculator/kernel/src/lib.rs">calculator kernel</a> stores one value per user. Its <code>transition</code> matches commands; these two branches show the essential distinction:</p>
<pre><code class="language-rust">Command::Set { value } =&gt; Ok((
    Some(Memory { user: actor.user, value: *value }),
    Reply::Value(*value),
)),
Command::Get =&gt; Ok((
    None,
    Reply::Value(memory_of(&amp;snap.memories, actor.user)),
)),</code></pre>
<p><code>Set</code> proposes a write to <code>actor.user</code>; <code>Get</code> proposes no write. Neither branch takes a database handle or an HTTP request. That restricted shape lets the same Rust implementation become a Lean definition. The full kernel also handles arithmetic errors and an <code>Apply</code> command.</p>
<p>Extractability is a tradeoff. The kernel uses the Rust subset and framework crates supported by Charon and Aeneas; I/O stays in the application shell. This restriction makes the proof surface reviewable, but a handler that bypasses the engine would undermine the intended path. The framework and code review both matter.</p>
<h2 id="theorem">What the Lean theorem actually says</h2>
<p>After extraction, the <a href="https://github.com/h5i-dev/h5i/blob/main/examples/app/calculator/proofs/Proofs.lean">calculator proof</a> states that every successful write belongs to the caller:</p>
<pre><code class="language-lean">theorem writes_own_memory (a : Principal) (s : Snapshot) (c : Command) :
    transition a s c ⦃ r =&gt; ∀ w reply, r = .Ok (w, reply) →
      ∀ m, w = some m → m.user = a.user ⦄ := by
  unfold transition
  cases c &lt;;&gt; step* &lt;;&gt; (try split) &lt;;&gt; simp_all</code></pre>
<p>The quantified command matters: this is a claim over all successful kernel commands under the model, not a handful of sample requests. It still says nothing by itself about whether an HTTP route supplied the right principal, whether a separate read route skipped the kernel, or whether a GPU executes equivalent code.</p>
<p>Kernel proofs describe a proposed write. Storage proofs address the modeled effect of applying and compiling writes. For tenant isolation, both are needed: a kernel unable to name another tenant would not excuse an SQL statement that omitted its tenant scope. The <a href="https://github.com/h5i-dev/h5i/blob/main/docs/app/TRUST.md">trust model</a names the proven components and the assumptions between them.</p>
<h2 id="running-app">Returning to the running web app</h2>
<p>The server connects that decision path to Axum. In the calculator example, the application engine is passed to <code>H5iApp</code>, and the resulting router is merged with the ordinary health route:</p>
<pre><code class="language-rust">let app = H5iApp::new(engine, auth);
let router = Router::new()
    .route("/healthz", get(|| async { "ok" }))
    .merge(rpc_router(app));</code></pre>
<p>This bridge is where the proof meets a running service. Authentication, snapshot loading, transaction behavior, external dependencies, and PostgreSQL behavior still have to satisfy their documented assumptions. Integration tests and red-teaming examine those running boundaries. The <a href="/guides/build-and-verify-a-web-app/">build and verify guide</a> covers the commands for creating and checking a project.</p>
<h2 id="proof-quality">A proof can pass for the wrong reason</h2>
<p>Proving that <code>Get</code> writes nothing is useful, but it does not check whether <code>Apply</code> writes the correct user's row. Proof validation asks whether the specification covers the risky behavior. A deliberate mutation that redirects a write should break a relevant ownership theorem; if it does not, the missing sensitivity deserves investigation. Mutation testing gives feedback about the proof suite, not a new theorem about the whole deployment.</p>
<p>The result is a Rust web app with a deliberately narrow formal core. The strength of this design is that a reviewer can point to the exact implementation, theorem, storage semantics, and remaining trust boundary instead of calling the entire server “verified.”</p>""",
        "next": ("/blog/finding-bugs-verifying-rust-oss/", "Field results", "I Tried to Verify Popular Rust OSS with Lean 4, but Found Bugs Instead", "See what failed proof attempts revealed in existing applications."),
        "cta": ("Build a small verified app", "Follow the calculator example from a Rust kernel through a Lean 4 proof.", "/guides/build-and-verify-a-web-app/", "Read the guide"),
    },
    {
        "section": "blog", "published": "2026-10-21", "slug": "finding-bugs-verifying-rust-oss",
        "eyebrow": "Engineering / Formal Verification", "time": "7 min",
        "tags": "Lean 4 &middot; Rust OSS &middot; Case studies",
        "title": "I Tried to Verify Popular Rust OSS with Lean 4, but Found Bugs Instead | h5i",
        "h1": "I Tried to Verify Popular Rust OSS with Lean 4, but Found Bugs Instead",
        "description": "Failed proof attempts led to confirmed Rust OSS bugs: an unscoped delete, a playlist metadata leak, and unbounded policy expansion.",
        "deck": "A failed theorem is a question, not a finding. Following that question into the original code uncovered three different ways an intended invariant could fail.",
        "body": """
<div class="callout"><strong>The experiment.</strong> Port selected Rust OSS behavior into an extractable h5i-app kernel, state a security property, and investigate why a proof does not go through. A failed proof is a lead; only reproduction against upstream establishes a bug.</div>
<h2 id="workflow">From proof attempt to upstream finding</h2>
<div class="tbl-wrap"><table class="data"><thead><tr><th>Step</th><th>Question</th><th>Evidence</th></tr></thead><tbody>
<tr><th>1. Specify</th><td>What should hold for every relevant actor and state?</td><td>A reviewable Lean property</td></tr>
<tr><th>2. Attempt</th><td>Does the translated Rust satisfy it?</td><td>A proof, counterexample, or unresolved obligation</td></tr>
<tr><th>3. Compare</th><td>Does the port preserve the upstream path?</td><td>Source comparison and differential tests</td></tr>
<tr><th>4. Reproduce</th><td>Can the original project exhibit the behavior?</td><td>A focused upstream test and report</td></tr>
</tbody></table></div>
<p>The <a href="https://github.com/h5i-dev/h5i-app-bench">benchmark repository</a> contains published ports and their proof projects. For example, its documented checks for the RustFS port and the full corpus are:</p>
<pre><code class="language-bash">h5i app doctor ports/rustfs
h5i app extract --check ports/rustfs
h5i app check --all ports</code></pre>
<p>These commands check the port and proof workspace; they do not reproduce any vulnerability. The last command is a strict gate and can fail on unfinished proofs. The RustiCal case discussed below was still being prepared for publication when the <a href="https://medium.com/@Koukyosyumei/i-tried-to-formally-verify-popular-rust-projects-with-lean-4-but-found-more-bugs-than-i-expected-5badd0017b7c">research account</a> appeared. Distinguishing a port from its original application is essential: an omitted upstream check can create a false counterexample in the port.</p>
<h2 id="rustical">RustiCal: the checked object and deleted rows diverged</h2>
<p>A calendar object is identified by principal, calendar ID, and object ID. The hard-delete path authorized the caller's own object, but the database operation selected rows using only the latter two identifiers. In simplified SQL, the vulnerable scope was:</p>
<pre><code class="language-sql">DELETE FROM calendarobjects
WHERE cal_id = ? AND id = ?;
-- The principal is missing from the selection.</code></pre>
<p>The property worth proving is a frame condition: after Alice deletes one of her objects, Bob's objects are unchanged. The telling reproduction uses Alice and Bob with the same local calendar and object names. A direct attempt to delete Bob's object can be denied, yet deleting Alice's matching object can still remove Bob's row because the write is wider than the authorization decision. The <a href="https://github.com/lennart-k/rustical/security/advisories/GHSA-9cp5-7crm-9cf7">RustiCal advisory</a> documents the confirmed issue.</p>
<h2 id="oxicloud">OxiCloud: playlist access did not imply file access</h2>
<p>The playlist path crossed a different boundary. A user could modify a playlist they owned, refer to another user's private file, and then receive that file's metadata in a track response. The permissions are on two objects, not one:</p>
<pre><code class="language-text">can edit playlist  ≠  can read every referenced file
             │
             └── response may include filename, size, MIME type, metadata</code></pre>
<p>A property over playlist ownership alone would pass while the leak remained. The useful property quantifies over the response: every file represented in the returned tracks must be readable by the caller. The <a href="https://github.com/AtalayaLabs/OxiCloud/pull/796">upstream fix discussion</a> shows how the missing file check was addressed.</p>
<h2 id="rustfs">RustFS: the outer limit did not bound the inner work</h2>
<p>RustFS expanded policy variables through nested substitution. An iteration cap guarded an outer loop, but a nested scan could rebuild an expression and continue without consuming that cap. A tiny input could drive disproportionate resolver work. A useful termination argument must show that every substitution decreases a measure or consumes a shared budget; merely finding a counter in the outer loop is insufficient.</p>
<p>The benchmark's <a href="https://github.com/h5i-dev/h5i-app-bench/blob/main/ports/rustfs/kernel/src/awsvars.rs">ported resolver</a> models the corrected resume point explicitly:</p>
<pre><code class="language-rust">pub struct Pending {
    pub text: Vec&lt;u8&gt;,
    pub resume: usize,
}
// A newly substituted placeholder waits for a bounded later pass.</code></pre>
<p>The <code>resume</code> position stops one scan from immediately restarting at the beginning of its newly substituted text. The <a href="https://github.com/rustfs/rustfs/security/advisories/GHSA-9wgh-5r68-49p7">RustFS advisory</a> records the upstream denial-of-service condition; the port's code shows the related model used for differential checks.</p>
<h2 id="lesson">What the three cases teach</h2>
<p>They fail for different reasons: a write scope wider than its check, a reference to an object with its own permission, and a nested computation outside its intended bound. The common method is to state the invariant, inspect the exact implementation path, and reproduce any suspected failure in the original application before calling it a finding.</p>
<p>Formal verification did not automatically diagnose these bugs. An unsuccessful proof could also mean a missing lemma, a bad model, or unsupported Rust. Its value was forcing a precise question that could be tested against real behavior. This article is an original synthesis of the <a href="https://medium.com/@Koukyosyumei/i-tried-to-formally-verify-popular-rust-projects-with-lean-4-but-found-more-bugs-than-i-expected-5badd0017b7c">original research account</a>, the <a href="https://github.com/h5i-dev/h5i-app-bench">benchmark</a>, and the linked upstream reports.</p>""",
        "next": ("/blog/ai-powered-web-security-testing/", "Running systems", "AI-Powered Web Security Testing: h5i vs. Burp, ZAP, and Caido", "Compare tools for investigating the behavior of a deployed application."),
        "cta": ("See how the framework is built", "Read the architecture behind the Rust kernels and Lean 4 proofs.", "/blog/building-a-verifiable-web-framework/", "Read the framework essay"),
    },
    {
        "section": "blog", "published": "2026-10-21", "slug": "ai-powered-web-security-testing",
        "eyebrow": "Engineering / Red-Teaming", "time": "7 min",
        "tags": "Burp Suite &middot; OWASP ZAP &middot; Caido &middot; h5i",
        "title": "AI-Powered Web Security Testing: h5i vs. Burp, ZAP, and Caido | h5i",
        "h1": "AI-Powered Web Security Testing: h5i vs. Burp, ZAP, and Caido",
        "description": "Compare Burp Suite, OWASP ZAP, Caido, and h5i by the investigation each supports: manual proxy work, scanning, agent tooling, and browser-to-HTTP testing.",
        "deck": "These tools overlap, but the useful comparison is the work a tester wants to delegate and the evidence they need to keep.",
        "body": """
<div class="callout"><strong>Scope.</strong> This comparison concerns testing running web applications. h5i-app, the Rust framework for building and formally verifying application logic, has a different role.</div>
<h2 id="question">Start with the investigator's loop</h2>
<p>A useful web security workflow starts with a page or API, finds a request worth testing, changes one condition, observes the result, and keeps enough context to reproduce it. The products below can all participate. They differ in what they make easy for a human tester, an automated scanner, or an external coding agent.</p>
<div class="tbl-wrap"><table class="data"><thead><tr><th>First task</th><th>Useful starting point</th><th>What to keep</th></tr></thead><tbody>
<tr><th>Deep manual investigation</th><td>Burp Suite or Caido proxy workspace</td><td>Request history and human notes</td></tr>
<tr><th>Repeat a defined scan</th><td>OWASP ZAP automation plan</td><td>Context, scan configuration, alerts</td></tr>
<tr><th>Agent follows a browser workflow</th><td>h5i CLI browser and HTTP tools</td><td>Observed requests, mutations, responses</td></tr>
</tbody></table></div>
<h2 id="burp">Burp Suite: depth in a human testing platform</h2>
<p>Burp's proxy-centered workflow puts captured traffic beside Repeater, Intruder, scanners, extensions, and manual investigation tools. It is a strong fit when a person wants a mature testing workspace and broad specialized capabilities. Burp also has AI features, including Burp AT, so agent support is not unique to h5i. The choice is whether the testing workspace or a CLI-driven browser and HTTP session should lead the investigation. See <a href="https://portswigger.net/burp/documentation">Burp's documentation</a> for current edition-specific capabilities.</p>
<figure class="feature-figure"><img src="/_static/burp-vs-h5i.svg" alt="Burp's proxy-centered testing workspace and h5i's CLI browser and HTTP session, both available to agent workflows"><figcaption>Burp offers a broad proxy workspace and Burp AT; h5i exposes a browser-to-request loop through CLI tools.</figcaption></figure>
<h2 id="zap">OWASP ZAP: repeatable scanning</h2>
<p>ZAP is especially useful when the question is whether a site passes repeatable automated checks. Its automation framework and CI integrations can run a known scan plan without a live agent deciding every next request. An agent can also drive ZAP's API. A scanner finding still needs human interpretation; authentication and business logic often require a more directed investigation. See the <a href="https://www.zaproxy.org/docs/">ZAP documentation</a>.</p>
<figure class="feature-figure"><img src="/_static/zap-vs-h5i.svg" alt="A configured ZAP scan plan beside an h5i browser observation and request replay loop"><figcaption>A scan plan and a directed investigation answer different questions; ZAP can also be agent-driven.</figcaption></figure>
<h2 id="caido">Caido: a proxy workspace with agent access</h2>
<p>Caido combines a modern interactive proxy with extensibility and agent-facing interfaces, including <a href="https://docs.caido.io/app/tutorials/skills">Caido Skills</a>. It suits teams that want the proxy as the shared source of request history while a human or agent uses its testing tools. The exact agent permissions and integrations depend on the installed setup.</p>
<figure class="feature-figure"><img src="/_static/caido-vs-h5i.svg" alt="Caido's proxy workspace with Replay, Automate and skills beside h5i's browser, recon and websec CLI session"><figcaption>Both support agents. Choose whether the proxy workspace or the terminal should hold the investigation.</figcaption></figure>
<h2 id="h5i">h5i: browser and HTTP control from a CLI</h2>
<p>h5i starts from the agent's sequence of actions: navigate in a browser, inspect its requests, discover endpoints with recon, edit and replay selected HTTP traffic, and retain the observed request and response context. It does not require the target application to use h5i-app. This makes it a natural fit when a coding agent already has a terminal and needs to investigate an authenticated workflow rather than operate a separate desktop proxy.</p>
<p>Its strength is the continuity between page behavior and request testing. Its limit is equally important: a request difference is evidence to investigate, not an automatic vulnerability finding. A broad scanner or manual specialist tool may be a better first choice for other jobs. The <a href="/guides/red-team-a-web-app/">red-teaming guide</a> shows the runnable workflow.</p>
<h2 id="example">One suspected IDOR, four working styles</h2>
<p>Suppose a tester notices that an invoice URL contains an object identifier. The investigative question is whether Alice can read Bob's invoice. Each tool offers a different path to the same comparison:</p>
<ul><li><strong>Burp:</strong> capture the request, change the identifier in Repeater, and inspect the two responses.</li><li><strong>Caido:</strong> keep the captured request and replay in its proxy workspace, potentially shared with an agent.</li><li><strong>ZAP:</strong> run a planned check, then add identity-specific context and interpret any alert.</li><li><strong>h5i:</strong> navigate as Alice, select the browser-observed request, replay one mutation, and retain both responses.</li></ul>
<p>For the h5i path, the relevant commands look like this; the request and response IDs are examples, and the target must be authorized:</p>
<pre><code class="language-bash">h5i browser open https://target.example --allow target.example
h5i websec requests
h5i websec replay req_42 --set query.id=456
h5i websec diff res_42 res_43</code></pre>
<p>The decisive evidence is not that two responses have different lengths. It is whether an unauthorized principal receives data or performs an operation that policy forbids. The tester must control sessions and compare the actual protected resource. This is where product capability and test design separate: no interface can infer the intended ownership rule from an HTTP status code alone.</p>
<h2 id="boundaries">Agent access changes the trust boundary</h2>
<p>An agent that can browse authenticated pages and replay requests can also make unintended changes or send sensitive material. The tool interface should make scope, credentials, and retained evidence clear. Proxy integrations expose their own permissions; a CLI exposes commands to the agent's existing runtime. Neither form is inherently safe. For high-risk tasks, run the investigation in a restricted environment and review the request history before treating the agent's report as a finding.</p>
<h2 id="choose">Choose by task, then combine where useful</h2>
<ul><li><strong>Deep manual investigation:</strong> Burp or Caido gives a rich proxy workspace.</li><li><strong>Scheduled automated scans:</strong> ZAP provides a repeatable scan plan.</li><li><strong>Agent-led browser-to-request exploration:</strong> h5i exposes the loop directly to CLI tools.</li></ul>
<p>These are choices about workflow, not a ranking of security coverage. A team can use ZAP for recurring scans, Burp or Caido for focused manual work, and h5i for agent-driven reproduction. Confirmed h5i findings can become versioned regression tests; see <a href="/guides/security-regression-testing/">Security Regression Testing</a>. Test only systems you own or are authorized to assess.</p>""",
        "next": ("/blog/sandboxing-ai-agents/", "Contain the work", "Sandboxing AI Agents: Isolation, Security, and Prompt Injection", "Consider what an investigating or coding agent can reach."),
        "cta": ("Run an authorized investigation", "Use the browser, recon, and HTTP tools against an application in scope.", "/guides/red-team-a-web-app/", "Read the guide"),
    },
    {
        "section": "blog", "published": "2026-10-21", "slug": "sandboxing-ai-agents",
        "eyebrow": "Engineering / Agent Security", "time": "8 min",
        "tags": "Isolation &middot; Prompt injection &middot; Threat model",
        "title": "Sandboxing AI Agents: Isolation, Security, and Prompt Injection | h5i",
        "h1": "Sandboxing AI Agents: Isolation, Security, and Prompt Injection",
        "description": "Why an agent sandbox must include child processes and browsers, how isolation tiers differ, and what prompt injection can still reach.",
        "deck": "A coding agent runs a workload, not a single executable. The security boundary must follow that workload through dependencies, tests, local servers, and browser sessions.",
        "body": """
<div class="callout"><strong>The threat model.</strong> A repository, dependency, tool result, or web page may be untrusted. The agent may follow malicious instructions or simply execute code whose behavior it did not inspect.</div>
<h2 id="workload">Contain the process tree</h2>
<p>One task can start a package manager, install scripts, compiler plugins, tests, a dev server, and a browser. Wrapping only the agent executable leaves an escape route through its children. A Git worktree separates the checkout but grants no filesystem or network confinement. The outer boundary needs to cover all processes involved in the task, while the policy can grant narrower permissions to particular tools.</p>
<p>A browser belongs inside that model. It executes page code, carries session state, and may connect to a local dev server. A browser running on the host can expose the developer's normal profile or reach a different <code>localhost</code> than the boxed server. A fresh browser profile inside the same task boundary avoids that mismatch.</p>
<p>Consider an apparently harmless dependency update. The agent runs the package manager; an install script reads environment variables, then tests start a local service, then the agent opens a documentation page in a browser. A final diff may contain only one lockfile change. The security question is which of those subprocesses could see the developer's home directory or contact the network. The scope of the box must be defined before the first command, not added after a suspicious step appears.</p>
<h2 id="tiers">Choose isolation by the failure to prevent</h2>
<p>Workspace separation addresses accidental edits to the parent checkout. Process isolation restricts host file access. Supervised execution can enforce network rules on raw traffic. A container also fixes a toolchain image; a microVM adds a guest kernel boundary at greater cost. These are different guarantees, and availability depends on the host. Check the selected tier and enforced policy for the actual run; the <a href="/manual/#isolation-tiers">isolation reference</a> records the limits.</p>
<figure class="feature-figure"><img src="/_static/fast-supervised-sandbox.svg" alt="Agent, repository, tools, development server, and browser inside one policy-controlled box, with a reviewed export outside"><figcaption>The boundary follows the task; the proposed result crosses out through review.</figcaption></figure>
<p>Even strong local isolation cannot protect a secret sent to an allowed model endpoint. Credentials should remain outside the writable box and be supplied only through constrained host-side mechanisms. If source must not leave the machine, model egress must be disabled or served locally.</p>
<p>A tier name alone is not evidence of enforcement. The host may lack the relevant Linux feature, a network rule may be unavailable, or the run may have fallen back to a weaker mode. The run's status should report what was actually enforced. Different tasks justify different costs: a routine local edit may value quick startup, while running an unknown pull request may justify a separate guest kernel and a stricter outbound policy.</p>
<h2 id="injection">Prompt injection is an authority problem</h2>
<p>A malicious README, issue comment, page, or tool result can tell the agent to read credentials, change a policy file, or send data away. Detecting the sentence is useful but fragile: the attacker can rephrase it. The stronger control is to keep the requested action outside the agent's authority. A page can suggest exfiltration, but the box should lack access to host credentials and disallow the destination.</p>
<p>Policy must be resolved outside the workload's writable paths. Otherwise the agent can rewrite the rule that is supposed to constrain it. Network controls must account for local services and DNS behavior as well as public hosts. Denied actions are worth recording because an attempted escape may leave no Git diff.</p>
<p>Imagine a malicious page instructing the agent to "verify setup" by reading a token file and sending it to an attacker-controlled endpoint. A classifier may or may not recognize the instruction as hostile. The durable defenses are that the token file is inaccessible and the destination is disallowed. If the agent is permitted to read the token for the legitimate task, policy design becomes harder: it must narrow where that token can be used, and the run needs close review. Isolation reduces available authority; it does not magically classify intent.</p>
<h2 id="limits">Containment leaves other risks</h2>
<p>A confined agent can still write vulnerable code, choose weak tests, or produce a persuasive but inaccurate report. Human review and application-level tests remain necessary. Host-kernel tiers also continue to trust the host kernel; a microVM moves, but does not eliminate, the trusted computing base.</p>
<p>The practical design is to let the agent work in a disposable environment, then cross a narrow review boundary with a proposed patch and execution record. The <a href="/guides/sandboxed-agent-workflows/">sandboxed workflow guide</a> has the commands and policy examples. The companion <a href="/blog/auditing-agent-workflows/">audit essay</a> explains what the reviewer can infer from the records.</p>""",
        "next": ("/blog/auditing-agent-workflows/", "Review the run", "Auditing AI Agent Workflows with Execution Evidence", "Examine what an execution record can and cannot establish."),
        "cta": ("Run one bounded task", "Use a disposable box and review its policy before the agent starts.", "/guides/sandboxed-agent-workflows/", "Read the guide"),
    },
    {
        "section": "blog", "published": "2026-10-21", "slug": "auditing-agent-workflows",
        "eyebrow": "Engineering / Execution Evidence", "time": "7 min",
        "tags": "Agent workflows &middot; Evidence &middot; Review",
        "title": "Auditing AI Agent Workflows with Execution Evidence | h5i",
        "h1": "Auditing AI Agent Workflows with Execution Evidence",
        "description": "A patch shows what changed, but reviewing agent work also needs commands, browser activity, network decisions, missing evidence, and a controlled export.",
        "deck": "A clean diff says little about the processes that produced it. Review needs records from outside the agent's writable workspace and a clear account of their limits.",
        "body": """
<div class="callout"><strong>The review question.</strong> What did the workload do while producing this proposal, and which parts of that account could the workload itself change?</div>
<h2 id="diff">The diff is one piece of evidence</h2>
<p>A Git diff shows persistent source changes. It cannot show that an install script tried to contact an unknown host, a test accessed a local service, or a browser opened an authenticated page. An agent-written summary may describe those events, but it is testimony from the subject of review. The useful record pairs the patch with independently captured execution facts.</p>
<p>Consider a small agent change in this repository. The host creates a box pinned to the current revision; the agent works in its shell, and the test runs under the same box policy:</p>
<pre><code class="language-bash">h5i box create review-demo --from HEAD --profile agent-claude
h5i box status review-demo
h5i box shell review-demo
# Inside the box: ask the agent to make the change, then exit.
h5i box run review-demo -- cargo test</code></pre>
<p>This sequence gives the later review a base revision, an enforced policy, and an observed test exit. It does not say that the patch is correct or that the test covers it. The <a href="/guides/sandboxed-agent-workflows/">workflow guide</a> covers profile setup and the full procedure.</p>
<h2 id="lanes">Ask each record the question it can answer</h2>
<p>Command records show invocations, exits, and relevant output; browser records show navigation and requests; policy decisions show allowed and denied connections; a patch shows proposed source changes. A receipt can connect these records to one named run and report the isolation tier. None of these alone proves that the code is correct or that the agent understood the task.</p>
<p>Before reading the agent's explanation, inspect the run from the host:</p>
<pre><code class="language-bash">h5i box status review-demo
h5i box log review-demo
h5i box diff review-demo</code></pre>
<p><code>status</code> reports the boundary that was actually enforced, <code>log</code> shows recorded events, and <code>diff</code> shows proposed source changes. During a live run, <code>h5i box watch review-demo --deny-only</code> can stream refusals until stopped. If the agent said “all tests passed,” compare that claim with the recorded <code>cargo test</code> exit before reading the patch.</p>
<div class="tbl-wrap"><table class="data"><thead><tr><th>Observation</th><th>Supports</th><th>Does not establish</th></tr></thead><tbody>
<tr><th>Test exited 0</th><td>The observed command completed successfully</td><td>The changed behavior was covered</td></tr>
<tr><th>Denied connection</th><td>A policy-controlled attempt was refused</td><td>What the process intended to do</td></tr>
<tr><th>Browser request and response</th><td>What that session observed</td><td>Whether application policy allowed the result</td></tr>
<tr><th>Patch against pinned base</th><td>Which files the run proposes to change</td><td>What the workload did before producing it</td></tr>
</tbody></table></div>
<p>The distinction between observation and interpretation is especially important for web testing. A browser record can show the request that reached an invoice endpoint and the response it received. It cannot, without an application policy, establish that the response was unauthorized. Likewise, a command exit code can show that a test process returned zero; it cannot establish that the test covered the changed behavior. The report should point back to each source record instead of collapsing them into one confident sentence.</p>
<p>Absence also matters. If browser recording was unavailable, the report should say so. Otherwise a blank browser log can be mistaken for a session with no browser activity. A denied request deserves a place in the review, even when it never changed a file.</p>
<p>A useful receipt therefore includes not just successful events but the conditions under which events were captured: the chosen isolation tier, network-policy status, evidence lanes, and known gaps. If a browser crashed, quietly starting a fresh session would erase a meaningful boundary in the story. Recording the end of that session makes the gap visible to the reviewer.</p>
<h2 id="boundary">Keep observation and acceptance outside</h2>
<p>If the agent can edit its audit trail, the trail is merely another output file. Store records outside the box's writable paths and resolve its policy outside them too. The box can propose a patch; a host-side export gate packages the patch, report, and receipt for human review. Applying that patch to the parent repository is a separate decision.</p>
<p>If the task used a browser, keep it and the local dev server in the same box so their requests belong to the same boundary. <code>h5i ui</code> lets a reviewer inspect active sessions and records; its display is a view of captured evidence, not an additional guarantee.</p>
<p>Export is more than file copying. It is the point where proposed work leaves a lower-trust workspace and becomes something a person may apply elsewhere. The record should identify the base revision so the reviewer can recognize drift, and the patch should remain separate from the evidence explaining how it was produced. A detached pull-request workspace cannot safely apply its own changes to a parent checkout by assumption; the reviewer must choose the destination and reconcile conflicts.</p>
<pre><code class="language-bash">h5i box export review-demo --out ./review-demo
git apply --check ./review-demo/patch.diff</code></pre>
<p>Read <code>review-demo/report.md</code> beside <code>review-demo/receipt.json</code> and <code>review-demo/patch.diff</code>. The check asks whether Git can apply the patch; it does not apply it or approve the change. A failed check may simply mean the destination has moved since the pinned base.</p>
<h2 id="order">A useful review order</h2>
<ol><li>Check the enforced isolation tier, policy, and any missing evidence.</li><li>Read denied network or file actions and failed commands.</li><li>Inspect browser actions, requests, and any human takeovers.</li><li>Compare executed tests and outcomes with the agent's claims.</li><li>Only then review the diff and decide what to import.</li></ol>
<p>This order keeps the proposal from framing the evidence before the reviewer has seen the run. It also exposes a common mismatch: a plausible patch with no test execution or a successful test report that actually belongs to an earlier state.</p>
<h2 id="limits">The limits of a receipt</h2>
<p>Host-held evidence prevents the boxed workload from rewriting its own account. It does not prove that the host owner did not alter the bundle later, and it does not attest to facts no sensor captured. The isolation tier and evidence lanes must therefore travel with the report. A claim such as "nothing suspicious happened" is too broad; "no denied egress was recorded while this network policy was enforced" is reviewable.</p>
<p>There is also a practical cost. Capturing more detail can expose secrets in logs and create more material for a reviewer to inspect. A good system keeps the evidence needed to answer the task's security questions, restricts who can read it, and reports where collection was incomplete. The point is an honest account with a bounded claim, not an ever-growing transcript.</p>
<p>Execution evidence makes agent work easier to inspect and reject. It does not turn a human approval into a formality. The reviewer still decides whether the code, the observed behavior, and the evidence gaps are acceptable.</p>""",
        "next": ("/blog/building-a-verifiable-web-framework/", "Return to the application", "Building a Verifiable Web Framework in Rust", "Read how application logic is structured for formal proofs."),
        "cta": ("Review a complete run", "Create a disposable box, inspect its record, and export only the result you accept.", "/guides/sandboxed-agent-workflows/", "Read the guide"),
    },
]
