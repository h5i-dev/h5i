# h5i-browser-light: design notes

ROADMAP.md is the authority on scope and order (§12, §B1 to §B15). This file
covers why the engine is shaped the way it is.

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

Both engines produce identical output on the script page. Caveats:

1. Cold start is included and dominates Chromium's time. Not a steady-state
   throughput comparison.
2. `--script` costs nothing on a page with no script (46 ms against 45 ms), and
   44 ms -> 248 ms on the app page.
3. Rendering is software. Complex CSS narrows the gap.
4. Trust memory most: it follows from the architecture (one process, no
   renderer, no GPU process).
5. These replace a table claiming 5x faster and 15x lighter, from before this
   engine had JavaScript (updated 2026/8/31). Memory roughly doubled since (31
   MB -> 52-66 MB): Boa and a 281 KiB prelude. Date a measurement.

## What it is not

- Not a Chromium replacement. Docs-grade pages are the compatibility bar; send
  React/Vite apps, video, WebGL and authenticated sessions down the Chromium
  path.
- JavaScript runs, and it is the slow half, so a script-driven page is the one
  case `headless_shell` can be *faster* on. Ask `capabilities`: the §B6 refusals
  are real, with no workers, no second browsing context, no media pipeline.
- Containment claims belong to the box. Bare on a host there is no egress proxy
  and no receipt store.

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
check, a page that moved between snapshot and click resolved `@e5` to a
different element and replied `ok`.

```
$ h5i-browser-light session click @e2
{"ok":false,"code":"stale-ref","retryable":true,
 "error":"`@e2` came from a snapshot this page has moved on from: it now names
          a button \"Add\". … Take a fresh `snapshot` and use its refs."}
```

It is an equality check on one ref, not a proof the document is unchanged: a
page that mutates something the walk does not record still passes. Typing and
scrolling renumber nothing, so a login loop needs no re-read between steps.

### The durable handle

`snapshot` reports a `refs` array beside the outline, whose selector survives
the reading `@e3` does not. That is what a recording replays into.

```json
{"id": "e3", "role": "button", "name": "Sign in", "selector": "#go"}
```

Built the way Lightpanda's is: the element's own segment, ancestors prepended
only when they shrink the match count, then a strict `a > b > c` chain as a
fallback. Ids are checked rather than trusted: duplicate ids are legal and
`#dup` names the first one. Every candidate is verified with the action verbs'
matcher; where nothing verifies, the field is `null`. Only `snapshot` computes
selectors: a tree walk per ref would cost every click.

Not built: `:has()` disambiguation before `:nth-of-type`. It needs `:has()` in
the borrowed selector parser, which is unverified here.

### When a verb refuses

Every failure carries a `code`, prose naming the recovery, and `retryable`:
whether it is the caller's to fix.

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

Every per-verb property is an exhaustive match on one table (`src/verbs.rs`),
including which verbs LOGIN mode admits.

### The resident session

`open` renders its own page and exits; `serve` holds a page open for an agent to
drive:

```
$ h5i-browser-light serve http://localhost:3000 &
$ h5i-browser-light session snapshot
$ h5i-browser-light session click @e1
```

Those verbs act on the page the viewers are watching. Several viewers and
control clients can attach at once. The control port is advertised beside the
stream port (`<name>.control` next to `<name>.stream`), so inside a box these
verbs need no flags.

`Page` is not `Send`: Blitz's `BaseDocument` holds an
`Arc<dyn HtmlParserProvider>` and a `Box<dyn FontMetricsProvider>`, so there is
no `Arc<Mutex<Session>>` to be had. One owning thread, everything else by
channel.

### What the agent did, recorded

`serve` writes its own action log (`$H5I_BROWSER_ACTIONS`, set for you inside a
box), which feeds the console's *agent actions* pane. The rows are marked
box-claimed rather than host-observed.

Each verb is recorded *before* it runs and again after. That guards against
accident (a bad path, a full disk), not against a box that lies. It costs 7µs
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

Blitz owns form submission and dispatches to a navigation provider; this engine
hands it one that *captures* the request instead of performing it, so a
submission is policy-checked and receipted. File inputs are dropped rather than
read.

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

The settle runs on a *virtual* clock, so a page's own `setTimeout(1000)` has
already fired by the time any verb is served. `wait_for` answers:

| `end` | means |
| --- | --- |
| `met` | it is there |
| `quiescent` | it is not, and the page has nothing left to run, so waiting cannot change this |
| `periodic` | it is not, and the only work left re-arms itself, so the page is running but not arriving |
| `budget` | it is not, and the page was still working towards something, so it may yet appear |

`periodic` exists because `requestAnimationFrame` is a `setTimeout` here: an
animation loop re-armed a one-shot timer every frame and answered `budget`
forever. Lightpanda's fix, adapted: the timers still *fire*, they stop
*counting*. Not copied is folding this into `quiescent`, which would claim
nothing can change when a repeating timer can change the DOM.

`wait_for_script` needs `--script` and says so as a routing answer rather than a
failed condition. A condition that throws counts as *not yet*.

### Reading a page cheaply

```
$ h5i-browser-light session markdown
$ h5i-browser-light session extract '{"rows": [{"selector": "tr.item", "limit": 5,
    "fields": {"name": ".title", "url": {"selector": "a", "attr": "href"}}}]}'
```

`markdown` is the page as a reader reads it, with no `@ref` handles. Three
details with tests behind them: tables carry the `|---|---|` separator that
makes them GFM, ordered lists carry their real numbers, and nested lists carry
their indent.

`extract` answers a schema. Keys are output names, values selector specs: `"h1"`
for the first match's text, `["a"]` for every match,
`{"selector":"a","attr":"href"}` for an attribute (`href` and `src` come back
absolute), `[{"selector":"li","fields":{…}}]` for one object per match with
sub-selectors scoped to it. An empty array is a result, a schema where nothing
matched is an error. Both verbs are fenced.

### The request log, from inside the session

```
$ h5i-browser-light session requests
   200 GET https://docs.rs/blitz/ (12043 bytes, 84ms)
DENIED GET https://telemetry.example.com/collect
$ h5i-browser-light session requests --since 41   # only what is new
```

If a request is not in the list, it did not happen. `denied` counts over the
whole session rather than the `--since` window, because "nothing was refused" is
a claim about the session.

### Cookies, and the narrowings that make them safe

- `Domain` honoured, over a compiled-in public suffix list. All four rules must
  pass: the domain must not be a public suffix (`Domain=co.uk` is refused), the
  setter must be within it on a label boundary (`attackerexample.com` may not
  claim `example.com`), an IP host may not widen at all, and `__Host-` forbids
  the attribute outright. The list is compiled in rather than fetched, and goes
  stale safely: it only grows.
- In memory, never on disk. The jar dies with the process.
- Never readable by the agent. No verb returns a value; `session status` reports
  a *count*, and the request log records how many cookies crossed rather than
  which.
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
and the reply echoes the *placeholder*. No verb returns a credential's value.

Only the `H5I_SECRET_` namespace is reachable: the rest of `H5I_*` is engine
configuration, and a prefix allowlist fails closed where a denylist would not.
Substitution happens for `type` and nothing else, as a predicate on the verb
table.

`input[type=password]` reports a fixed-width mask rather than its value, so a
credential a *human* typed during LOGIN mode is not readable by the agent once
the mode ends. Whether the field is filled stays visible.

LOGIN mode (5.10) is half built. `session login` refuses every control verb that
reads the page, so a credential typed during it is not in a snapshot the agent
asked for. It does *not* withhold frames: the person typing has to see the page,
and the viewer socket is inside the box, where there is no privilege boundary.

Two verbs pass through, `status` and `login` itself. `requests` is refused
during a login because it names URLs a login flow visited, and `status` reports
an origin rather than a URL: an OAuth callback carries its `code` in the query,
a magic link and a password reset carry their token in the path.

### JavaScript, as a limited preview

Off by default. `--script` turns it on, and `capabilities --script` reports what
that configuration can do: h5i routes on the invocation, not the binary.

```
$ h5i-browser-light serve http://localhost:3000 --script
$ h5i-browser-light session click @e1
{"ok":true,"ref":"@e1","requests":["http://localhost:3000/api/item"],
 "settled":"settled after 0ms"}
```

`requests` is the causal link, and the log shows all three legs:

```
200 navigation  /index.html
200 subresource /app.js          <- the script file, fetched before it ran
200 subresource /api/item        <- what the click caused
```

The Rust DOM is the single source of truth and every JS object naming a node
wraps a `NodeId`. The object model lives in a JavaScript prelude rather than in
Rust, because listeners, timer callbacks and promise resolvers are GC-managed.
The Rust surface underneath is about twenty primitives taking ids and strings.

Settling is reported: "run until settled" drains promise jobs and timers on a
*virtual* clock, so two runs settle identically, and a page that never settles
is cut off at a budget and says so. Missing APIs are named, never stubbed
silently.

```
note: still busy after 2000ms (1 timers pending) — this page had not finished
note: this page used Web APIs this engine does not have
      (Element.getBoundingClientRect x3, IntersectionObserver x1). What depends
      on them did not run; the chromium engine has them.
```

ES modules work, and `import "lodash"` does not become a request to a CDN: a
bare specifier is refused by name. Module fetches go through the same broker,
carry the document origin, and appear in the request log.

They are also `cors` requests, which a classic `<script src>` beside them is
not: fetched the classic way, a cross-origin module is *evaluated in the page's
realm* without the server ever being asked. Both `type="module" src` and dynamic
`import()` ask, with the same-origin credentials a module script without
`crossorigin` gets.

### Live connections, and the caveat that travels with them

`WebSocket` and `EventSource` are real objects over real connections, not names
that answer feature detection: the rule here is *absent, not stubbed*.

Every frame is receipted, not just the handshake. Frames are ordinary
request/response pairs with `WS-SEND`, `WS-RECV` or `SSE-RECV` as the method, so
the console, `h5i box watch` and the export bundle show socket traffic
unchanged.

`wss://` works: a socket that owns its transport carries `rustls` directly,
already in the tree through `reqwest`'s own TLS. One transport type serves both
schemes. The TLS half shares its connection between reader and writer under a
lock (a TLS connection is one piece of state and cannot be `try_clone`d the way
a `TcpStream` can) with a short read timeout so the reader drops the lock often
enough for a send to get in.

`EventSource` is a `cors` request, not the agent's own: sending it without an
`Origin` and with session cookies attached let two allowed origins read each
other's streams. An answer that is not `text/event-stream` is refused too, or
the line parser reads *any* body and every line beginning `data:` in someone
else's document is a message the page receives.

CORS does not apply to a WebSocket; `Origin` is all a server has to tell a
page's socket from a program's. The handshake carries the document's origin
(`null` for a document that has none), and a socket the *agent* named carries
none. The address is checked too: the pinning resolver cannot reach a client
that calls `TcpStream::connect` itself, so the socket asks for the addresses the
policy already approved.

One refusal stands: a remote socket is refused whenever an egress proxy is
configured, `wss://` included. A raw socket would not go through the proxy, and
that proxy is how a box's allowlist stays in the path. Loopback is exempt
because the proxy already excludes it.

One caveat: a page holding a live connection is not deterministic. Messages
arrive on wall-clock time, so two reads can differ without the agent having
acted. `snapshot` and `status` report `open_sockets`. Delivery happens when a
verb runs rather than the instant a frame lands. Reconnection is deliberately
not built.

What is not there: `IntersectionObserver` and `ResizeObserver` report themselves
missing; `fetch` is synchronous underneath, so two requests run in order rather
than at once, and `AbortController` cannot cancel one in flight; no iframes,
workers, WebGL or WebAssembly. Those are also what will stop React first: a
production build is not yet verified (docs/roadmap-history.md §12.4 sets that
bar) and what runs today is a hand-written application of the shape above.

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
labelled as data. `sanitize_display` protects a viewer's chrome, not this
moment.

The fence rests on a tested property: no page-derived value may span a line, so
a page that writes the closing marker into its own text gets it back as quoted
content on a `- ` line. A marker written inline becomes
`[fence marker removed]`, the only content this engine removes.

### The live view

`serve` opens a WebSocket speaking the format h5i's viewers already use, so
`h5i box view` and `h5i box view --term` attach unchanged: base64 JPEG frames in
a JSON envelope, a `status` message carrying the viewport, and `config`/`ack`
pacing. `--stream-file` writes the bound port where the viewers look
(`<env>/tmp/agent-browser/*.stream`).

Frames are driven by change: one is produced when a scroll actually moved or a
navigation landed, and at rest the process is idle. A click the policy refuses
returns a `page_error` and keeps the current page rather than going blank.

The allowlist is fail-closed: with no `--allow`, nothing remote is reachable.
Loopback is allowed by default because it is the dev server, and `--no-loopback`
takes that away. `$H5I_EGRESS_PROXY` is picked up automatically.

### Fonts

Fonts are discovered at runtime rather than linked at build time: Blitz's
`system-fonts` would add a build-time dependency on libfontconfig and break a
hermetic build. A host with no fonts renders pages but draws no text, and
`doctor` says so. `--font-file` and `--font-dir` override the search.

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

## Status

Tiers 1 and 2 of docs/roadmap-history.md M10: static render, snapshot,
screenshot, receipts, a live view h5i's viewers attach to, the resident session
and its verbs (§12.1), and JavaScript behind `--script`. Tier 3, policy-gated
script, is deliberately unbuilt; docs/roadmap-history.md §12 is the plan and
§12.5 is what it costs. Not yet done: the frame half of LOGIN mode, and file
uploads, which are dropped rather than read.

Pin a box to this engine with
`h5i box create --profile browser --engine h5i-light`, or
`[profile.browser] engine = "h5i-light"`. Such a box gets `H5I_BROWSER_ALLOW`
(its own `net.egress`) and `H5I_BROWSER_RECEIPTS` (a path inside the box), and
none of agent-browser's variables.

Driven against a real box on 2026-08-08: `h5i box view`'s forward and the
console's frame relay both attach and render, and a control-channel navigation
reaches every attached viewer.

### What a reading of Lightpanda changed, 2026-08-26

docs/roadmap-history.md §B16 is the write-up. What landed here: the fourth wait
outcome above; a snapshot that no longer lets a wrapper swallow the block
beneath it; `--url` on the read verbs; `Domain` cookies; an address-level
rebinding check; record and replay over durable selectors; a real Canvas 2D;
`wss://`; a `structured` verb; and a counter for verb names callers asked for
and this engine does not have.

The comparison was most useful for the three costs it found in *our* load path,
which are §B16.10's queue and are not built yet.
