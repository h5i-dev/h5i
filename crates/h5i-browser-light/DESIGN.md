# h5i-browser-light: design notes

ROADMAP.md is the authority on scope and order (§12, §B1 to §B15). This file is
narrower: why the engine is shaped the way it is, and what each shape cost.

---

## Why this exists

A browser engine for AI agents that read thousands of pages at a time, or that
open untrusted pages carrying prompt injections.

h5i's egress proxy sees `CONNECT docs.example.com:443` and nothing more. CDP's
Fetch domain can pause and record a request, but fails open: attach races, fresh
targets and workers, buffer limits and disconnects all leave gaps. Here the
engine *is* the HTTP client, so:

- No receipt, no request. The record is written before any bytes move, and a
  sink that refuses to record refuses to fetch.
- Every redirect hop is policy-checked, so an allowed origin cannot bounce to a
  denied one.
- No JIT. Too much machinery to keep free of exploitable bugs.

## Measurements

Same machine (aarch64, WSL2), median of 7 interleaved runs after a discarded
warm-up, on a local `file://` page. Memory is peak summed RSS across the process
tree at 5 ms intervals; `/usr/bin/time -v` reports only the largest single
process and badly undercounts a multi-process browser.

| | small (1 KB, no script) | docs (22 KB, no script) | app (script-built, 400 elements) |
| --- | --- | --- | --- |
| h5i-browser-light | **46 ms / 51.7 MB** | **59 ms / 65.6 MB** | 248 ms / **87.4 MB** |
| chromium `headless_shell` | 172 ms / 456.8 MB | 176 ms / 461.5 MB | **176 ms** / 464.4 MB |
| chromium (full) | 672 ms / 1150.7 MB | 758 ms / 1153.8 MB | 824 ms / 1150.6 MB |

Both engines were checked to produce identical output on the script page before
anything was timed. Caveats:

1. Cold start is included and dominates Chromium's time. Not a steady-state
   throughput comparison.
2. Script-driven pages are slower here, which is what the third column is for.
   `--script` costs nothing on a page with no script (46 ms against 45 ms), and
   44 ms -> 248 ms on the app page.
3. Rendering is software. Complex CSS narrows the gap.
4. Trust memory most: it follows from the architecture (one process, no
   renderer, no GPU process) rather than from a workload.
5. These replace a table claiming 5x faster and 15x lighter, measured before
   this engine had a JavaScript engine (updated 2026/8/31). Memory roughly
   doubled since (31 MB -> 52-66 MB), which is what Boa and a 281 KiB prelude
   cost. Date a measurement.

## What it is not

- Not a Chromium replacement. Docs-grade pages are the compatibility bar; send
  React/Vite apps, video, WebGL and authenticated sessions down the Chromium
  path.
- JavaScript runs, and it is the slow half, so a script-driven page is the one
  case `headless_shell` can be *faster* on. Route by what a page costs, not by
  whether it has a `<script>` tag, and ask `capabilities`: the §B6 refusals are
  real, with no workers, no second browsing context, no media pipeline.
- Containment claims belong to the box. Bare on a host there is no egress proxy
  and no receipt store, and this is a light browser with a request log.

## Usage

```
h5i-browser-light open  <url|path>... [--allow ORIGIN]... [--screenshot PATH]
                                      [--receipts PATH] [--text] [--json]
h5i-browser-light serve <url|path> [--addr 127.0.0.1:0] [--stream-file PATH]
                                   [--control-file PATH]
h5i-browser-light session status | snapshot | navigate <url> | scroll <px>
                           | type <@ref|--selector CSS> <text>
                           | submit <@ref|--selector CSS>
                           | click <@ref|--selector CSS>
                           | wait-for --selector <css> | wait-for-script <expr>
                           | requests [--since <seq>] | markdown | extract <schema>
                           | structured | script [--save PATH] | env
h5i-browser-light session snapshot|markdown|extract|structured [--url URL]
h5i-browser-light replay <script.json>        # a recording, run without a model
h5i-browser-light open|serve ... [--script]   # limited JavaScript preview
h5i-browser-light capabilities     # what this engine can do, as JSON
h5i-browser-light doctor           # fonts, proxy, allowlist, client
```

### Refs, and the reading they came from

A `@ref` names *a position in the snapshot that minted it*, not a durable
handle, and is honoured only against the reading it was served in. Without that
check a page that moved between the snapshot and the click resolved `@e5` to a
different element, acted on it, and replied `ok`.

```
$ h5i-browser-light session click @e2
{"ok":false,"code":"stale-ref","retryable":true,
 "error":"`@e2` came from a snapshot this page has moved on from: it now names
          a button \"Add\". … Take a fresh `snapshot` and use its refs."}
```

An equality check on one ref, not a proof the document is unchanged: a page that
mutates something the walk does not record still passes. Typing and scrolling
renumber nothing, so a login loop needs no re-read between steps.

### The durable handle

`snapshot` reports a `refs` array beside the outline, whose selector survives
the reading `@e3` does not. That is what a recording replays into.

```json
{"id": "e3", "role": "button", "name": "Sign in", "selector": "#go"}
```

Built the way Lightpanda's is: the element's own segment, ancestors prepended
only when they shrink the match count, then a strict `a > b > c` chain as a
fallback. Ids are checked rather than trusted, since duplicate ids are legal and
`#dup` names the first one. Every candidate is verified with the matcher the
action verbs use; where nothing verifies the field is `null`, because a selector
that resolves elsewhere looks like a handle and is not one.

Only `snapshot` computes selectors: a tree walk per ref in the action verbs
would put the cost on every click.

Not built: `:has()` disambiguation before `:nth-of-type`. It needs `:has()` in
the borrowed selector parser, which is unverified here, and emitting selectors
the matcher then rejects is the failure this avoids.

### When a verb refuses

Every failure carries a `code`, prose naming the recovery, and `retryable`,
which says whether this is the caller's to fix at all. A selector a model can
correct and an allowlist it cannot are different answers, and reporting the
first like the second ends a self-correction loop instead of prompting it.

| code | means |
| --- | --- |
| `unknown-verb` | not a verb this session has; the message lists the ones it does |
| `bad-request` | a missing or malformed argument |
| `no-snapshot` | a `@ref` was named before any snapshot was served |
| `no-such-ref` | the ref is not on this page at all |
| `stale-ref` | the ref is on the page and means something else now |
| `wrong-role` | the ref is the wrong kind of thing for this verb |
| `refused` | the policy said no |
| `login-mode` | LOGIN mode is on and this verb reads the page |
| `timeout` / `no-match` / `internal` | as named |

The verbs live in one table (`src/verbs.rs`) and every per-verb property is an
exhaustive match on it, so a new verb does not compile until it has answered
each question, including which verbs LOGIN mode admits.

### The resident session

`open` renders its own page and exits. `serve` holds a page open and is the
thing an agent drives:

```
$ h5i-browser-light serve http://localhost:3000 &
$ h5i-browser-light session snapshot
$ h5i-browser-light session click @e1
```

Those verbs act on the page the viewers are watching, so the live view shows the
page *the agent* is driving. Several viewers and control clients can attach at
once. The control port is advertised beside the stream port (`<name>.control`
next to `<name>.stream`), so inside a box these verbs need no flags.

`Page` is not `Send`: Blitz's `BaseDocument` holds an
`Arc<dyn HtmlParserProvider>` and a `Box<dyn FontMetricsProvider>`, so there is
no `Arc<Mutex<Session>>` to be had. One owning thread, everything else by
channel, and no interleaving to reason about.

### What the agent did, recorded

`serve` writes its own action log (`$H5I_BROWSER_ACTIONS`, set for you inside a
box), which feeds the console's *agent actions* pane. The rows are marked
box-claimed rather than host-observed: unlike agent-browser, there is no
mediating socket here, and nothing written inside a box can be more than the
box's own account.

Each verb is recorded *before* it runs and again after: no record, no action,
the rule the request log enforces for fetches. That guards against accident (a
bad path, a full disk), not against a box that has decided to lie. It costs 7µs
per verb against the 42ms a single frame encode takes.

### Logging in

```
$ h5i-browser-light session snapshot
- textbox "username" [ref=e1]
- textbox "password" [ref=e2]
- button "Sign in" [ref=e3]
$ h5i-browser-light session type @e1 alice
$ h5i-browser-light session type @e2 hunter2
$ h5i-browser-light session submit @e3
url: http://localhost:8123/members
```

Blitz owns the form submission algorithm and dispatches the result to a
navigation provider. This engine hands it one that *captures* the request
instead of performing it, so a submission is policy-checked and receipted like
any other. File inputs are dropped rather than read: filling one would mean this
browser quietly acquiring the ability to read the box's filesystem.

`type` replaces the field rather than appending, so a retry after a failed
submit does not produce `alicealice`. The snapshot reads values back from the
editor rather than the `value` attribute, or `type` then `snapshot` would look
like it had silently failed.

### Waiting, and the third answer

```
$ h5i-browser-light session wait-for --selector '#results'
$ h5i-browser-light session wait-for --text 'Signed in'
$ h5i-browser-light session wait-for-script 'document.querySelectorAll("li").length > 3'
```

Both reference engines wait on a wall clock with hard-coded fudge. The settle
here runs on a *virtual* clock, so a page's own `setTimeout(1000)` has already
fired by the time any verb is served. `wait_for` does not usually wait. It
*answers*:

| `end` | means |
| --- | --- |
| `met` | it is there |
| `quiescent` | it is not, and the page has nothing left to run, so waiting cannot change this |
| `periodic` | it is not, and the only work left re-arms itself, so the page is running but not arriving |
| `budget` | it is not, and the page was still working towards something, so it may yet appear |

The middle two are the ones worth having. Collapsing either into "timed out" is
the lie this engine refuses elsewhere: a page that finished and a page that was
cut off are not the same fact. `periodic` exists because
`requestAnimationFrame` is a `setTimeout` here, so an animation loop presented a
fresh one-shot timer every frame and answered `budget` about a page that would
still be looping tomorrow. Lightpanda's fix, adapted: the timers still *fire*,
they stop *counting*. Not copied is folding this into `quiescent`, which would
claim nothing can change when a repeating timer can change the DOM.

`wait_for_script` needs `--script` and says so as a routing answer rather than a
failed condition. A condition that throws counts as *not yet*: a page mid-build
throws on the way to values it has not made.

### Reading a page cheaply

```
$ h5i-browser-light session markdown
$ h5i-browser-light session extract '{"rows": [{"selector": "tr.item", "limit": 5,
    "fields": {"name": ".title", "url": {"selector": "a", "attr": "href"}}}]}'
```

`markdown` is the page as a reader reads it, with no `@ref` handles: the outline
is to be *acted on*, this is to be *read*. Three details with tests behind them:
tables carry the `|---|---|` separator that makes them GFM, ordered lists carry
their real numbers, and nested lists carry their indent.

`extract` answers a schema instead of making a model transcribe prose. Keys are
output names, values selector specs: `"h1"` for the first match's text, `["a"]`
for every match, `{"selector":"a","attr":"href"}` for an attribute (`href` and
`src` come back absolute), `[{"selector":"li","fields":{…}}]` for one object per
match with sub-selectors scoped to it.

One rule matters more than the syntax: an empty array is a result, a schema
where nothing matched is an error. "There were no rows" is the page's answer;
"none of your selectors match" is the caller's mistake, and an object full of
nulls would be a wrong answer that looks right. Both verbs are fenced, being
page content reaching something about to decide what to do next.

### The request log, from inside the session

```
$ h5i-browser-light session requests
   200 GET https://docs.rs/blitz/ (12043 bytes, 84ms)
DENIED GET https://telemetry.example.com/collect
$ h5i-browser-light session requests --since 41   # only what is new
```

No other engine answers this without qualifying it: Chromium's list is an
observation made from beside the network and fails open, Obscura's CDP events
are batched after navigation completes, and Lightpanda has no equivalent. Here
it is the decision record written before the bytes moved. If a request is not in
the list, it did not happen.

`denied` counts over the whole session rather than the `--since` window, because
"nothing was refused" is a claim about the session.

### Cookies, and the narrowings that make them safe

Cookies are the first thing this engine holds that is worth stealing, so the
limits arrived with them:

- `Domain` honoured, over a compiled-in public suffix list. All four rules must
  pass: the domain must not be a public suffix (`Domain=co.uk` is refused), the
  setter must be within it on a label boundary (`attackerexample.com` may not
  claim `example.com`, which a bare suffix test would have allowed), an IP host
  may not widen at all, and `__Host-` forbids the attribute outright. The list
  is compiled in rather than fetched, so nothing depends on the network to
  decide where a credential may go, and it goes stale safely: it only grows, so
  an old copy refuses suffixes it has not heard of.
- In memory, never on disk. The jar dies with the process.
- Never readable by the agent. No verb returns a value; `session status` reports
  a *count*, and the request log records how many cookies crossed rather than
  which, because a credential in a receipt is a credential in every export that
  receipt reaches.
- `Secure` enforced, `__Secure-`/`__Host-` prefixes enforced at store time, and
  a redirected POST downgraded to a bodyless GET on 301/302/303 so a password is
  not replayed to wherever a server points next.

### Credentials the agent can use and cannot read

```
$ H5I_SECRET_ACME_PASS=hunter2 h5i-browser-light serve https://acme.test/ &
$ h5i-browser-light session env
H5I_SECRET_ACME_PASS          # the name. never the value
$ h5i-browser-light session type @e2 '$H5I_SECRET_ACME_PASS'
{"ok":true,"ref":"@e2","used":["H5I_SECRET_ACME_PASS"]}
```

The model names a credential, the engine resolves it on the way into the field,
and the reply echoes the *placeholder*. The value never enters the model's
context, so it cannot be repeated back, summarised, or carried into what the
agent does next. No verb returns a credential's value.

Only the `H5I_SECRET_` namespace is reachable. h5i already uses `H5I_*` for
engine configuration, and making those substitutable would let a page-bound
`type` put the receipts path into a form. A denylist works until somebody adds a
variable; a prefix allowlist fails closed.

Substitution happens for `type` and nothing else, as a predicate on the verb
table rather than a decision at the call site. Resolving a placeholder into a
selector, a URL or a wait condition would put the value where it can be read
back: out of the DOM, out of the request log, out of an error message.

`input[type=password]` reports a fixed-width mask rather than its value, which
`snapshot` used to read straight back out: a credential a *human* typed during
LOGIN mode was readable by the agent the moment the mode ended. Fixed width,
because the real length is weak evidence but still evidence. Whether the field
is filled stays visible.

LOGIN mode (5.10) is half built. `session login` refuses every control verb that
reads the page, so a credential typed during it is not in a snapshot the agent
asked for. It does *not* withhold frames: the person typing has to see the page,
and the viewer socket is inside the box, where there is no privilege boundary.
The mode refuses the documented path, which is the threat it was written for,
and the refusal text says so rather than implying containment.

Two verbs pass through: `status`, so the agent can tell when the mode ends, and
`login` itself. `requests` is refused during a login because it names URLs a
login flow visited, and `status` used to name the one the flow is *on*: an OAuth
callback carries its `code` in the query, a magic link and a password reset
carry their token in the path. It reports the origin instead.

### JavaScript, as a limited preview

Off by default. `--script` turns it on, and `capabilities --script` reports what
that configuration can do, because h5i routes on whether *this* invocation runs
script rather than whether the binary could.

```
$ h5i-browser-light serve http://localhost:3000 --script
$ h5i-browser-light session click @e1
{"ok":true,"ref":"@e1","requests":["http://localhost:3000/api/item"],
 "settled":"settled after 0ms"}
```

That reply is the point of the whole engine. The agent clicked, the page's
script ran, its `fetch` went through the same broker, the DOM changed, and the
new item is in the next snapshot. `requests` is the causal link, stamped by the
one component that knows it, and the log shows all three legs:

```
200 navigation  /index.html
200 subresource /app.js          <- the script file, fetched before it ran
200 subresource /api/item        <- what the click caused
```

Boa provides the language; the browser is ours. The Rust DOM is the single
source of truth and every JS object naming a node wraps a `NodeId`, so the
snapshot, the paint, the events and the script state cannot drift apart. The
object model lives in a JavaScript prelude rather than in Rust, because
listeners, timer callbacks and promise resolvers are GC-managed and the engine
that owns their lifetime should keep owning it. The Rust surface underneath is
about twenty primitives taking ids and strings.

Settling is reported, not guessed: "run until settled" drains promise jobs and
timers on a *virtual* clock, so two runs settle identically, and a page that
never settles is cut off at a budget and says so. Missing APIs are named, never
stubbed silently, because that is the routing signal: without it an agent cannot
tell an empty page from one that needed Chromium.

```
note: still busy after 2000ms (1 timers pending) — this page had not finished
note: this page used Web APIs this engine does not have
      (Element.getBoundingClientRect x3, IntersectionObserver x1). What depends
      on them did not run; the chromium engine has them.
```

ES modules work, and `import "lodash"` does not become a request to a CDN: a
bare specifier is refused by name, because a loader that rewrites one is an
engine choosing destinations the page never named. Module fetches go through the
same broker, carry the document origin, and appear in the request log.

They are also `cors` requests, which a classic `<script src>` beside them is
not; that difference is the spec's, and it is the whole of why JSONP exists.
Fetching one the classic way meant a cross-origin module was parsed and
*evaluated in the page's realm* without the server ever being asked, the one
thing the CORS rule on module scripts exists to refuse. Both `type="module" src`
and dynamic `import()` ask now, with the same-origin credentials a module script
without `crossorigin` gets.

### Live connections, and the caveat that travels with them

`WebSocket` and `EventSource` are real objects over real connections, not names
that answer feature detection: the rule here is *absent, not stubbed*. They were
built for reach rather than coverage. A cloud browser cannot open
`localhost:3000`, and a dev server's hot-reload channel is a WebSocket, so the
place this engine alone can reach was also the place it rendered a half-built
page.

Every frame is receipted. Receipting the handshake alone would let the central
claim quietly stop covering the bytes after it, exactly the CONNECT-gate
blindness this engine exists to remove. Frames are ordinary request/response
pairs with `WS-SEND`, `WS-RECV` or `SSE-RECV` as the method, so the console,
`h5i box watch` and the export bundle show socket traffic unchanged.

`wss://` works. The old refusal ("needs a raw TLS stream the HTTP client here
does not expose") was true of `reqwest` and wrongly generalised into a property
of the engine: a socket that owns its transport needs nothing from the HTTP
client. It carries `rustls` directly, already in the tree through `reqwest`'s
own TLS. One transport type serves both schemes, because a parallel path for the
encrypted one is where the two drift and only one keeps getting the receipt rule
right. The TLS half shares its connection between reader and writer under a lock
(a TLS connection is one piece of state and cannot be `try_clone`d the way a
`TcpStream` can) with a short read timeout so the reader drops the lock often
enough for a send to get in.

The same-origin policy reaches both. `EventSource` is a `cors` request in every
browser and this engine sent it as the agent's own: no `Origin`, no
`Access-Control-Allow-Origin` check, session cookies attached, so two allowed
origins and a script on either could open the other's stream and read it. It is
a `cors` request now, and an answer that is not `text/event-stream` is refused:
without that the line parser reads *any* body, and every line beginning `data:`
in someone else's document is a message the page receives.

CORS does not apply to a WebSocket, so `Origin` is the *only* thing a server has
to tell a page's socket from a program's, and sending none is precisely the
shape a cross-site WebSocket hijack takes. The handshake carries the document's
origin now (`null` for a document that has none), and a socket the *agent* named
carries none, because there is no document behind it. The address is checked
too: the pinning resolver cannot reach a client that calls `TcpStream::connect`
itself, so the socket asks for the addresses the policy already approved.

One refusal stands: a remote socket is refused whenever an egress proxy is
configured, `wss://` included. A raw socket would not go through the proxy, and
inside a box that proxy is how the allowlist stays in the path. TLS buys no
exemption: the objection was never that the bytes were readable, it was that the
connection is not the proxy's to see. Loopback is exempt because the proxy
already excludes it.

One caveat. A page holding a live connection is the one thing here that is *not*
deterministic: messages arrive on wall-clock time, so two reads can differ
without the agent having acted. `snapshot` and `status` report `open_sockets`.
Delivery happens when a verb runs rather than the instant a frame lands, since
the session has no pump at rest, which is what makes it free when nobody is
driving. Reconnection is deliberately not built: an engine that silently
re-dialled would make requests the agent never asked for.

What is not there: `IntersectionObserver` and `ResizeObserver` report themselves
missing; `fetch` is synchronous underneath, so two requests run in order rather
than at once, and `AbortController` cannot cancel one in flight; no iframes,
workers, WebGL or WebAssembly. Those are also what will stop React first: a
production build is not yet verified (ROADMAP §12.4 sets that bar) and what runs
today is a hand-written application of the shape above.

Boa is a fork at 0.22.0, pinned by revision: `boa_engine` and `boa_gc` are
patched to `h5i-dev/boa` in the workspace `Cargo.toml`, one commit ahead with
`Script::bind_to_realm`, which compiles the prelude once and runs it in many
realms. The old 0.19 pin and the `icu_normalizer` clash with parley that forced
it are gone, which is what `scripts/check_boa_release.sh` was written to catch.
Patch both crates together: this crate depends on `boa_gc` directly, and a
second copy would make the cancellation token two incompatible types with the
same name.

### The snapshot is fenced

Page content is wrapped in `--- BEGIN/END UNTRUSTED PAGE CONTENT ---` and
labelled as data. This is where attacker-controlled text reaches a model
deciding what to do next, and the other defences do not cover it:
`sanitize_display` protects a viewer's chrome, and running no script removes the
commonest delivery *channel*. Neither says anything at the moment of reading.

The fence rests on a tested property rather than a secret: no page-derived value
may span a line, so a page that writes the closing marker into its own text gets
it back as quoted content on a `- ` line. A marker written inline becomes
`[fence marker removed]`, the only content this engine removes.

### The live view

`serve` opens a WebSocket speaking the format h5i's viewers already use, so `h5i
box view` and `h5i box view --term` attach unchanged: base64 JPEG frames in a
JSON envelope, a `status` message carrying the viewport, and `config`/`ack`
pacing. `--stream-file` writes the bound port where the viewers look
(`<env>/tmp/agent-browser/*.stream`).

Frames are driven by change, not by a clock: one is produced when a scroll
actually moved or a navigation landed, and at rest the process is idle rather
than re-encoding an identical JPEG thirty times a second. A click the policy
refuses returns a `page_error` and keeps the current page rather than going
blank.

The allowlist is fail-closed: with no `--allow`, nothing remote is reachable.
Loopback is allowed by default because it is the dev server, and `--no-loopback`
takes that away. `$H5I_EGRESS_PROXY` is picked up automatically so the sandbox's
own allowlist stays in the path.

### Fonts

Fonts are discovered at runtime rather than linked at build time: Blitz's
`system-fonts` would add a build-time dependency on libfontconfig, which breaks
a hermetic build for a font list. A host with no fonts renders pages but draws
no text, and `doctor` says so rather than leaving you a blank screenshot.
`--font-file` and `--font-dir` override the search.

## Composition

Assembled, not written from scratch:

| Concern | Component |
| --- | --- |
| HTML parsing, DOM | `blitz-html`, `blitz-dom` |
| CSS, style resolution | Stylo (via `blitz-dom`) |
| Layout | Taffy (via `blitz-dom`) |
| Paint, rasterisation | `blitz-paint`, `vello_cpu` (CPU: a box has no GPU) |
| Text, fonts | `parley`, `fontique` |
| Policy, receipts, HTTP | this crate |

Ours are the parts that make it h5i's: the policy, the receipts, the
fail-closed broker, and the agent-facing snapshot.

## Status

Tiers 1 and 2 of ROADMAP M10: static render, snapshot, screenshot, receipts, a
live view h5i's viewers attach to, the resident session and its verbs (§12.1),
and JavaScript behind `--script`. Tier 3, policy-gated script, is deliberately
unbuilt; ROADMAP §12 is the plan and §12.5 is what it costs. Not yet done: the
frame half of LOGIN mode, and file uploads, which are dropped rather than read.

Pin a box to this engine with `h5i box create --profile browser --engine
h5i-light`, or `[profile.browser] engine = "h5i-light"`. Such a box gets
`H5I_BROWSER_ALLOW` (its own `net.egress`) and `H5I_BROWSER_RECEIPTS` (a path
inside the box), and none of agent-browser's variables.

Driven against a real box on 2026-08-08: `h5i box view`'s forward and the
console's frame relay both attach and render, input is dropped while the agent
holds the control lock and flows once a human takes it, and a control-channel
navigation reaches every attached viewer.

### What a reading of Lightpanda changed, 2026-08-26

ROADMAP §B16 is the write-up. What landed here: the fourth wait outcome above; a
snapshot that no longer lets a wrapper swallow the block beneath it; `--url` on
the read verbs; `Domain` cookies; an address-level rebinding check, so the
receipt cannot name a host the bytes never reached; record-and-replay over
durable selectors; a real Canvas 2D; `wss://`; a `structured` verb; and a
counter for verb names callers asked for and this engine does not have.

Three of those were *not* Lightpanda's ideas but its absences, found by reading
its code beside ours: it fakes canvas with sixty-one silent no-ops, it pays
wall-clock time for every timer, and it has no receipts. The comparison was most
useful for the three costs it found in *our* load path, which are §B16.10's
queue and are not built yet.
