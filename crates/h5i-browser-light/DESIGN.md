# h5i-browser-light: design notes

The argument behind the engine, kept out of the [README](README.md) so that file
can stay a README. Nothing here is required to *use* the browser; it is here
because the claims it makes are security claims, and a claim worth making is
worth being able to check.

ROADMAP.md in the repository root is the authority on scope and order (§12 and
§B1 to §B15). This file is the narrower one: why the engine is shaped the way it
is, and what each shape cost.

---

## Why this exists

h5i already drives Chromium through agent-browser, and will keep doing so for
the case Chromium is best at: rendering the agent's own dev server with full
fidelity. This engine is for the other half of the loop, reading the untrusted
web, where the priorities invert and containment matters more than pixel
fidelity.

The property that motivates a separate engine is not speed. It is that an
external observer cannot make Chromium's network activity *fail closed*. h5i's
egress proxy sees `CONNECT docs.example.com:443` and nothing more. CDP's Fetch
domain can pause and record a request, but its coverage fails open: attach
races, freshly created targets and workers, buffer limits and disconnects all
leave gaps. Here the engine *is* the HTTP client, so:

- **No receipt, no request.** The decision record is written before any bytes
  move. A sink that refuses to record is a sink that refuses to fetch.
- **Every redirect hop is a decision.** Redirects are followed by hand and each
  hop is policy-checked, so an allowed origin cannot bounce to a denied one.
- **No script in this tier.** Page script is not evaluated at all, so the
  commonest delivery channel for injected instructions is absent rather than
  filtered.

## Measurements

Same machine (aarch64, WSL2), **median of 7 interleaved runs** after a discarded
warm-up, loading a local `file://` page. Memory is the peak **summed** RSS across
the whole process tree, sampled every 5 ms: `/usr/bin/time -v` reports only the
largest single process and badly undercounts a multi-process browser.

Interleaved rather than run in blocks, because `perf/ab.py` records what happens
otherwise — the second engine inherits a warm page cache and the comparison
reads a number the mechanism cannot produce.

| | small (1 KB, no script) | docs (22 KB, no script) | app (script-built, 400 elements) |
| --- | --- | --- | --- |
| h5i-browser-light | **46 ms / 51.7 MB** | **59 ms / 65.6 MB** | 248 ms / **87.4 MB** |
| chromium `headless_shell` | 172 ms / 456.8 MB | 176 ms / 461.5 MB | **176 ms** / 464.4 MB |
| chromium (full) | 672 ms / 1150.7 MB | 758 ms / 1153.8 MB | 824 ms / 1150.6 MB |

So against `headless_shell`: **about 3x faster and 7-9x lighter on pages without
script**, and on a script-driven page **slower on time and still 5x lighter**.
Both engines were checked to produce identical output on the script page before
anything was timed.

Five caveats, because the numbers are flattering in one direction and unflattering
in the other, and neither should be quoted alone:

1. **Cold start is included**, and Chromium's process startup dominates its time
   figure. That is the honest shape of a one-shot agent invocation, but it is
   *not* a steady-state rendering throughput comparison, and this engine would
   not win one by that margin.
2. **On script-driven pages this engine is slower**, and the third column is
   there so that cannot be read past. Boa interprets where V8 compiles. Isolated:
   `--script` costs nothing on a page with no script (46 ms against 45 ms), and
   44 ms -> 248 ms on the app page — the difference is JavaScript execution, all
   of it.
3. Rendering here is software, not JIT-accelerated. Complex CSS narrows the time
   gap.
4. The memory figure is the one to trust most: it is a property of the
   architecture (one process, no renderer, no GPU process) rather than of a
   workload, and it holds across all three pages.
5. **These numbers replace an earlier table that claimed 5x faster and 15x
   lighter.** That measurement predates this engine having a JavaScript engine at
   all; h5i's own memory has roughly doubled since (31 MB -> 52-66 MB), which is
   what Boa and a 281 KiB prelude cost. The claim was not wrong when it was made
   and is wrong now, which is the reason to date a measurement.

## What it is not

Honest limits, because the claims above are security claims:

- **Not a Chromium replacement.** Docs-grade pages are the compatibility bar.
  React/Vite apps, video, WebGL and authenticated sessions belong on the
  Chromium path.
- **JavaScript runs, and it is the slow half.** This line used to read "No
  JavaScript. Pages that render only via script will come back empty", and that
  has not been true since the engine grew a Boa realm and a DOM prelude: the
  script-built page in the table above renders correctly. What is true is that
  Boa interprets where V8 compiles, so a script-driven page is the one case
  `headless_shell` is *faster* on. Route by what a page costs, not by whether it
  has a `<script>` tag — and ask `capabilities` rather than guessing, because
  the §B6 refusals are still real: no workers, no second browsing context, no
  media pipeline.
- **Containment claims belong to the box.** Run bare on a host there is no
  egress proxy and no receipt store, and this is just a light browser with a
  request log. The guarantees are properties of running it inside an h5i box.

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

A `@ref` names *a position in the snapshot that minted it*: `e1` is the first
actionable thing in that walk, not a durable handle on an element. The action
verbs each take a fresh snapshot to get a live node id, which is right on its
own and was, on its own, a bug: if the page moved in between, `@e5` resolved to
a **different element**, the click landed on it, and the reply said `ok`.

So a ref is now honoured only against the reading it was served in. The session
keeps the refs it last handed out and checks the one you name against them:

```
$ h5i-browser-light session click @e2
{"ok":false,"code":"stale-ref","retryable":true,
 "error":"`@e2` came from a snapshot this page has moved on from: it now names
          a button \"Add\". … Take a fresh `snapshot` and use its refs."}
```

This is an equality check on one ref, not a proof that the document is
unchanged: a page that mutates something the walk does not record still passes.
What it catches is every case where the handle you hold has come to mean
something else, which is the failure that used to be silent. Typing and
scrolling do not renumber anything, so the login loop still runs without a
re-read between steps.

### The durable handle

`snapshot` now reports a `refs` array beside the outline:

```json
{"id": "e3", "role": "button", "name": "Sign in", "selector": "#go"}
```

`@e3` is a position in *this* reading. The selector is a handle that survives
one, which is what a recorded session needs to replay into and what an agent
needs to come back to an element after a navigation.

It is built the way Lightpanda's is: the element's own segment, then ancestors
prepended **only when they shrink the match count**, then a strict `a > b > c`
chain as a fallback. An id is checked rather than trusted, because duplicate ids
are legal in the wild and `#dup` names the first one.

The part that makes it worth having is that **every candidate is verified with
the same matcher the action verbs use**: `querySelector` semantics, first match
must be the target. Where nothing verifies, the field is `null` rather than a
guess: a selector that resolves elsewhere is worse than no selector, because it
looks like a handle.

Selectors are computed only by the `snapshot` verb. The action verbs take their
own internal captures to get a live node id, and paying for a tree walk per ref
on each of those would put the cost on every click.

Not built: `:has()` disambiguation before falling back to `:nth-of-type`, which
produces better selectors on generated markup. It needs `:has()` support in the
borrowed selector parser, which is unverified here, and emitting selectors the
matcher then rejects would produce exactly the plausible-looking handle this
avoids.

### When a verb refuses

Every failure carries a machine-readable `code`, prose that names the recovery,
and `retryable`, which says whether this is the caller's to fix at all. A
selector a model can correct and an allowlist it cannot are different answers,
and reporting the first the way the second is reported ends a self-correction
loop instead of prompting it.

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

The verbs themselves live in one table (`src/verbs.rs`) and every per-verb
property is an exhaustive match on it, so a new verb does not compile until it
has answered each question, including which verbs LOGIN mode admits, which was
a two-literal string allowlist that a typo would have widened silently.

### The resident session

`open` renders its own page and exits, so two `open`s share nothing. `serve`
holds a page open and is the thing an agent drives:

```
$ h5i-browser-light serve http://localhost:3000 &
$ h5i-browser-light session snapshot
$ h5i-browser-light session click @e1
```

Those verbs act on the same page the viewers are watching, which is what makes
the live view show the page *the agent* is driving rather than the one the
serving process happened to open. Several viewers and several control clients
can be attached at once.

The control port is advertised beside the stream port (`<name>.control` next to
`<name>.stream`), so inside a box, where h5i sets `H5I_BROWSER_STREAM_FILE`,
these verbs need no flags.

One constraint shapes all of this: **`Page` is not `Send`.** Blitz's
`BaseDocument` holds an `Arc<dyn HtmlParserProvider>` and a
`Box<dyn FontMetricsProvider>`, so there is no `Arc<Mutex<Session>>` to be had.
The page has exactly one owning thread and everything else reaches it by
channel, which is the right shape for a session with several drivers anyway,
because it leaves no interleaving to reason about.

### What the agent did, recorded

The console's *agent actions* pane is fed by the mediated socket h5i owns in
front of agent-browser. There is no such socket here, because the engine *is*
the browser, so before this the pane rendered empty for a session an agent was
actively driving, which reads as "the agent did nothing". `serve` now writes its
own action log (`$H5I_BROWSER_ACTIONS`, set for you inside a box), and the rows
land in that pane marked **box-claimed** rather than host-observed, because
that is what they are. Nothing written inside a box can be more than the box's
own account, and the pane says so.

Each verb is recorded *before* it runs and again after: no record, no action,
the same rule the request log enforces for fetches. That is a guarantee against
accident (a bad path, a full disk), not against a box that has decided to lie.

It costs **7µs per verb**, measured against the **42ms** a single frame encode
takes (debug build, same host): 0.017% of one frame, on a path that already
does a policy check and a layout pass. Agent verbs arrive at agent pace.

### Logging in

Typing and form submission arrived together, because a session you cannot type
into stops at the first login form:

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

Blitz owns the HTML form submission algorithm: what is in the entry list, how
it is encoded, whether the method makes a query or a body. It dispatches the
result to a navigation provider. This engine hands it a provider that *captures*
the request instead of performing it, so the wire stays ours: a submission is
policy-checked and receipted like any other request. File inputs are dropped
rather than read, because filling one would mean this browser quietly acquiring
the ability to read the box's filesystem.

`type` replaces the field rather than appending, so retrying after a failed
submit does not produce `alicealice`. The snapshot reads values back from the
editor rather than the `value` attribute, or `type` then `snapshot` would look
like it had silently failed.

### Waiting, and the third answer

```
$ h5i-browser-light session wait-for --selector '#results'
$ h5i-browser-light session wait-for --text 'Signed in'
$ h5i-browser-light session wait-for-script 'document.querySelectorAll("li").length > 3'
```

Both reference engines wait on a wall clock with hard-coded fudge: a 500ms
network-idle debounce in one; a 150ms quiet window, a 1s grace, a 500ms tail and
a 5s deadline that marks the page idle even when the deadline is what ended it,
in the other. The settle here runs on a **virtual** clock, which changes what
this verb is.

Because the settle runs a page to quiescence, a page's own `setTimeout(1000)`
has already fired by the time any verb is served. So `wait_for` does not
usually wait. It **answers**, with one of four outcomes:

| `end` | means |
| --- | --- |
| `met` | it is there |
| `quiescent` | it is not, and the page has nothing left to run, so waiting cannot change this |
| `periodic` | it is not, and the only work left re-arms itself, so the page is running but not arriving |
| `budget` | it is not, and the page was still working towards something, so it may yet appear |

The middle two are the ones worth having, and collapsing either into "timed
out" is the lie this engine refuses elsewhere: a page that has finished and a
page that was cut off are not the same fact. On a page with no script at all the
answer comes back immediately, because nothing can put the element there.

**`periodic` was the last of the four to exist, and its absence was a lie the
other three told.** `requestAnimationFrame` is a `setTimeout` here, so an
animation loop presented a fresh one-shot timer every frame; the page never ran
out of pending work, rode the whole ten-second budget, and answered `budget` —
"it may yet appear" — about a page that would still be looping tomorrow.

The fix is Lightpanda's, adapted. There, a task that reschedules itself never
blocks completion and a timer chain stops blocking past a nesting depth of ten;
here the timers still *fire*, they stop *counting*. What is not copied is the
conclusion: folding this into `quiescent` would claim nothing can change, and a
repeating timer can change the DOM. It is a fourth answer because it is a fourth
fact.

`wait_for_script` needs `--script` and says so as a routing answer rather than
as a condition that failed. A condition that throws counts as *not yet*: a page
mid-build throws on the way to values it has not made, and treating that as an
error would make most useful conditions unwritable.

### Reading a page cheaply

```
$ h5i-browser-light session markdown
$ h5i-browser-light session extract '{"rows": [{"selector": "tr.item", "limit": 5,
    "fields": {"name": ".title", "url": {"selector": "a", "attr": "href"}}}]}'
```

`markdown` is the page as a reader reads it: prose, emphasis, lists, tables, no
`@ref` handles. The outline exists to be *acted on*; this exists to be *read*.
Three details the reference implementation of this gets wrong and this one does
not, each with a test: tables carry the `|---|---|` separator that makes them
GFM, ordered lists carry their real numbers rather than `1.` repeated, and
nested lists carry their indent.

`extract` answers a schema instead of making a model transcribe prose. Keys are
output names, values are selector specs: `"h1"` for the first match's text,
`["a"]` for every match, `{"selector":"a","attr":"href"}` for an attribute
(`href` and `src` come back absolute), and `[{"selector":"li","fields":{…}}]`
for one object per match with sub-selectors scoped to it.

One rule matters more than the syntax. **An empty array is a result; a schema
where nothing matched is an error.** "There were no rows" is something the page
said; "none of your selectors match this page" is a mistake the caller should
hear about, and answering it with a tidy object full of nulls would be a wrong
answer that looks like a right one. The error names `snapshot` and `markdown` as
the way to see what is actually there.

Both are fenced, because both are page content reaching something that is about
to decide what to do next.

### The request log, from inside the session

```
$ h5i-browser-light session requests
   200 GET https://docs.rs/blitz/ (12043 bytes, 84ms)
DENIED GET https://telemetry.example.com/collect
$ h5i-browser-light session requests --since 41   # only what is new
```

No other engine can answer this without qualifying it. Chromium's list is an
observation of the network made from beside it and fails open. Obscura's CDP
`Network.*` events are batched and emitted after navigation completes,
reconstructed from a stored list, so anything watching live sees a compressed,
out-of-time picture. Lightpanda has no equivalent. Here the engine *is* the HTTP
client, so this is the decision record written before the bytes moved: if a
request is not in the list, it did not happen.

`denied` counts over the whole session rather than the `--since` window, because
"nothing was refused" is a claim about the session and an agent that only ever
asks for windows should still be able to make it.

### Cookies, and the narrowings that make them safe

Cookies are the first thing this engine holds that is worth stealing, so the
limits arrived with them rather than after:

- **`Domain` honoured, over a compiled-in public suffix list.** This was refused
  until the list arrived, and the stated cost was real: a site that logs you in
  at `example.com` and serves from `www.example.com` did not stay logged in.
  Four rules stand between that and the failure the refusal was avoiding, and a
  cookie must pass all of them — the domain must not be a public suffix
  (`Domain=co.uk` is refused), the setter must be within it on a label boundary
  (`attackerexample.com` may not claim `example.com`, which a bare suffix test
  would have allowed), an IP host may not widen at all, and `__Host-` forbids
  the attribute outright.

  The list is compiled in rather than fetched, so nothing here depends on the
  network to decide where a credential may go, and it goes stale safely: the
  list only grows, so an out-of-date copy refuses suffixes it has not heard of
  rather than accepting them.
- **In memory, never on disk.** The jar dies with the process; restarting the
  session is a complete logout.
- **Never readable by the agent.** No verb returns a value. `session status`
  reports a *count*, and the request log records how many cookies crossed
  rather than which, because a credential in a receipt is a credential in
  every export that receipt reaches.
- **`Secure` enforced**, `__Secure-`/`__Host-` prefixes enforced at store time,
  and a redirected POST is downgraded to a bodyless GET on 301/302/303 so a
  password is not replayed to wherever a server points next.

### Credentials the agent can use and cannot read

```
$ H5I_SECRET_ACME_PASS=hunter2 h5i-browser-light serve https://acme.test/ &
$ h5i-browser-light session env
H5I_SECRET_ACME_PASS          # the name. never the value
$ h5i-browser-light session type @e2 '$H5I_SECRET_ACME_PASS'
{"ok":true,"ref":"@e2","used":["H5I_SECRET_ACME_PASS"]}
```

The model names a credential, the engine resolves it on the way into the field,
and the reply echoes the **placeholder**. The value never enters the model's
context, so it cannot be repeated back, summarised, or carried into whatever the
agent does next. `env` returns names and nothing else; no verb in this engine
returns a credential's value. That is the same rule the cookie jar follows by
reporting a count.

Only the `H5I_SECRET_` namespace is reachable, which is narrower than the
scheme this borrows from. h5i already uses `H5I_*` for engine configuration
(`H5I_EGRESS_PROXY`, `H5I_BROWSER_RECEIPTS`), and making those substitutable
would let a page-bound `type` put the receipts path into a form. A denylist
would work until somebody added a variable; a prefix allowlist fails closed.

Substitution happens for `type` and for nothing else, as a predicate on the verb
table rather than a decision at the call site. Resolving a placeholder into a
selector, a URL or a wait condition would put the value somewhere it can be read
back: out of the DOM, out of the request log, out of an error message.

**This closed a hole that was not part of the feature.** A password field's
value was read straight back out by `snapshot`, which meant a credential typed
by a *human* during LOGIN mode was readable by the agent the moment that mode
ended. `input[type=password]` now reports a fixed-width mask instead of what it
holds. Fixed width, because the real length is weak evidence but it is still
evidence. Whether the field is filled is still visible, which is what an agent
legitimately needs.

**LOGIN mode** (5.10) is half built, and the half matters. `session login`
refuses every control verb that reads the page, so a credential typed during it
is not in a snapshot the agent asked for. It does **not** withhold frames: the
person typing has to see the page, and the viewer socket is inside the box,
where there is no privilege boundary, so an agent that goes looking can attach
to it and watch the same pixels. The mode refuses the documented path, which is
the threat it was written for; it is not containment against an agent that is
trying, and the refusal text says so rather than implying otherwise.

### JavaScript, as a limited preview

Off by default. `--script` turns it on, and `capabilities --script` reports what
that configuration can do, because what h5i routes on is whether *this*
invocation runs script rather than whether the binary could.

```
$ h5i-browser-light serve http://localhost:3000 --script
$ h5i-browser-light session click @e1
{"ok":true,"ref":"@e1","requests":["http://localhost:3000/api/item"],
 "settled":"settled after 0ms"}
```

That reply is the point of the whole engine. The agent clicked, the page's own
script ran, its `fetch` went through the same broker as everything else, the DOM
changed, and the new item is in the next snapshot. The `requests` field is the
causal link, stamped by the one component that knows it. The request log shows
all three legs:

```
200 navigation  /index.html
200 subresource /app.js          <- the script file, fetched before it ran
200 subresource /api/item        <- what the click caused
```

Script-initiated traffic being first-class evidence is the lane where every
other engine is thinnest, and it is only available to an engine that *is* the
HTTP client.

**How it is built.** Boa provides the language; the browser is ours. The Rust
DOM is the single source of truth and every JS object naming a node is a wrapper
over a `NodeId`, so the snapshot, the paint, the events and the script state
cannot drift apart. The object model itself lives in a JavaScript prelude rather
than in Rust, because event listeners, timer callbacks and promise resolvers are
GC-managed and the engine that owns their lifetime should keep owning it. The
Rust surface underneath is about twenty primitives taking ids and strings.

**Settling is reported, not guessed.** "Run until settled" drains promise jobs
and timers on a *virtual* clock, so a page's `setTimeout(1000)` costs an agent
nothing and two runs of the same page settle identically. A page that never
settles is cut off at a budget and says so, in the outline:

```
note: still busy after 2000ms (1 timers pending) — this page had not finished
```

A snapshot that quietly returned early is a wrong answer that looks like a right
one, so that line exists rather than the silence.

**Missing APIs are named, never stubbed silently.** What the page asked for and
did not get appears outside the fence, most-used first:

```
note: this page used Web APIs this engine does not have
      (Element.getBoundingClientRect x3, IntersectionObserver x1). What depends
      on them did not run; the chromium engine has them.
```

That is the routing signal. Without it an agent cannot tell an empty page from
one that needed the other engine.

**ES modules work**, and `import "lodash"` does not become a request to a CDN.
A bare specifier is refused by name, with what would have to exist instead,
because a loader that silently rewrites one is an engine choosing destinations
the page never named, inside a sandbox whose whole claim is that every request
is policy-checked and receipted. Module fetches go through the same broker as
everything else, carry the document origin, and appear in the request log.

### Live connections, and the caveat that travels with them

`WebSocket` and `EventSource` are real objects over real connections, not names
that answer feature detection. The rule this engine already had, *absent, not
stubbed*, cost three sites their bundle when it was broken, so these arrive
working or not at all, with tests asserting the shape a page checks against.

The argument for building them is narrower than "pages use them". This engine's
stated advantage is **reach**: a cloud browser cannot open `localhost:3000`, and
for a coding agent that is most of what it needs to look at. A dev server's
hot-reload channel is a WebSocket. So the place this engine alone can reach was
also the place it rendered a half-built page.

**Every frame is receipted.** A socket open for ten minutes carrying four
hundred messages could be honoured by receipting the handshake alone, and then
the central claim would quietly stop covering the bytes after it, which is
exactly the CONNECT-gate blindness this engine exists to remove. Frames are
written as ordinary request/response pairs with `WS-SEND`, `WS-RECV` or
`SSE-RECV` as the method, each naming the protocol it actually was, so the
console, `h5i box watch` and the export bundle show socket traffic with no
changes to any of them.

**`wss://` works**, and the reason it did not is worth recording because the
reason was wrong. The refusal said it "needs a raw TLS stream the HTTP client
here does not expose", which is true of `reqwest` and had been generalised into
a property of the engine. It is not one: a socket that **owns its transport**
needs nothing from the HTTP client. Lightpanda gets `wss://` free because its
socket is a curl handle and curl carries the TLS; here the socket carries
`rustls` directly, and both crates were already in the tree through `reqwest`'s
own TLS, so this added a name to the manifest and no code to the build.

One transport type serves both schemes, because a parallel path for the
encrypted one is where the two drift and only one of them keeps getting the
receipt rule right. The TLS half shares its connection between the reader thread
and the writer under a lock — a TLS connection is one piece of state and cannot
be `try_clone`d the way a `TcpStream` can — with a short read timeout that
exists solely so the reader drops the lock often enough for a send to get in.

**One refusal stands, by name:**

- **A remote socket is refused whenever an egress proxy is configured**,
  `wss://` included. A WebSocket is a raw socket and would not go through the
  proxy, and inside a box that proxy is how the sandbox's allowlist stays in the
  path. TLS buys no exemption here: the objection was never that the bytes were
  readable, it was that the connection is not the proxy's to see. Loopback is
  exempt because the proxy already excludes loopback, so nothing in the path is
  being stepped around.

**And one honest caveat.** A page holding a live connection is the one thing
here that is *not* deterministic: messages arrive on wall-clock time, so two
reads of that page can differ without the agent having acted. `snapshot` and
`status` report `open_sockets` and say so. Delivery happens when a verb runs
rather than the instant a frame lands, because the session has no pump at rest,
which is what makes it cost nothing when nobody is driving it.

Not built, deliberately: **reconnection**. An engine that silently re-dialled
would be making requests the agent never asked for, and the receipt would show
them arriving from nowhere.

**What is not there.** `IntersectionObserver` and `ResizeObserver` report
themselves as missing. `fetch` is synchronous underneath, so two requests run in
order rather than at once, and `AbortController` cannot cancel one in flight. No
iframes, workers, WebSocket, canvas, WebGL or WebAssembly.

**Not yet verified: a production React build.** ROADMAP §12.4 sets that as the
bar and it has not been cleared. What runs today is a hand-written application
of the shape above. The gaps most likely to stop React first are the ones listed
above, in that order.

**The Boa version is pinned to 0.19 for a dependency reason, not a preference.**
Boa 0.20+ requires `icu_normalizer ~2.0`; `parley`, which Blitz pulls for text,
requires `^2.1.1`. Those ranges are disjoint and semver-compatible, so Cargo
must pick one and cannot. 0.19 uses the 1.x line, which is semver-*incompatible*
and therefore allowed to coexist, at the cost of two ICU stacks in the build.
Upstream Boa has already moved `main` to `~2.2.0`, so this unwinds when that
releases.

### The snapshot is fenced

Page content is wrapped in `--- BEGIN/END UNTRUSTED PAGE CONTENT ---` and
labelled as data. This is the point where attacker-controlled text reaches a
model that is deciding what to do next, and the engine's other defences do not
cover it: `sanitize_display` protects a viewer's chrome, and running no script
removes the commonest delivery *channel*. Neither says anything at the moment of
reading.

The fence rests on a tested property rather than a secret: no page-derived value
may span a line, so a page that writes the closing marker into its own text gets
it back as quoted content on a `- ` line. A marker written inline is replaced
with `[fence marker removed]`, the only content this engine removes, and the
words around it survive.

### The live view

`serve` opens a WebSocket that speaks the format h5i's viewers already use, so
`h5i box view` and `h5i box view --term` attach to this engine unchanged:
base64 JPEG frames in a JSON envelope, a `status` message carrying the viewport,
and `config`/`ack` pacing. `--stream-file` writes the bound port where the
viewers look for it (`<env>/tmp/agent-browser/*.stream`).

Frames are driven by change, not by a clock. Tier 1 runs no script, so nothing
moves on its own: a frame is produced when a scroll actually moved or a
navigation landed, and at rest the process is idle rather than re-encoding an
identical JPEG thirty times a second. Scrolling (wheel, PageUp/Down, arrows,
Home/End) and clicking a link both work; a click on a link the policy refuses
returns a `page_error` and keeps the current page rather than going blank.

The allowlist is fail-closed: with no `--allow`, nothing remote is reachable.
Loopback is allowed by default because it is the dev server; `--no-loopback`
takes that away. `$H5I_EGRESS_PROXY` is picked up automatically so that inside
a box the sandbox's own allowlist stays in the path.

### Fonts

Fonts are discovered and registered at runtime rather than linked at build
time: enabling Blitz's `system-fonts` would add a build-time dependency on
libfontconfig, which breaks a hermetic build for a font list. A host with no
fonts renders pages but draws no text, and `doctor` says so rather than
leaving you with a blank screenshot. `--font-file` and `--font-dir` override
the search.

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

The parts that are ours are the parts that make it h5i's: the policy, the
receipts, the fail-closed broker, and the agent-facing snapshot.

## Status

Tiers 1 and 2 of ROADMAP M10: static render, snapshot, screenshot, receipts,
and a live view h5i's viewers can attach to. Plus the resident session and its
verbs (ROADMAP §12.1). Script is not built; ROADMAP §12 is the plan and
§12.5 is what it costs.

h5i can pin a box to this engine: `h5i box create --profile browser --engine
h5i-light`, or `[profile.browser] engine = "h5i-light"`. A box pinned to it
gets `H5I_BROWSER_ALLOW` (its own `net.egress`, so the engine's allowlist is
the box's) and `H5I_BROWSER_RECEIPTS` (a path inside the box), and none of
agent-browser's variables, which this engine would not read.

Driven against a real box on 2026-08-08: `h5i box view`'s forward and the
console's frame relay both attach to an `h5i-light` box and render, input is
dropped while the agent holds the control lock and flows once a human takes it,
and a control-channel navigation reaches every attached viewer. Two defects came
out of that run and are fixed: a relative path failed when the working
directory could not be resolved by name, and `serve` accepted only one viewer at
a time, so opening the console silently blocked `h5i box view`.

Not yet done: **the frame half of LOGIN mode**, so a human taking over to type
a password is protected from the agent's *reads* and not from an agent that
attaches to the viewer socket; and no file uploads, which are dropped rather
than read. Tier 3 (policy-gated script) remains deliberately unbuilt.

`Domain` cookies were on this list and are now built, over a compiled-in public
suffix list, so a cross-subdomain session persists.

### What a reading of Lightpanda changed, 2026-08-26

ROADMAP §B16 is the write-up; what landed here is: the fourth wait outcome
above; a snapshot that no longer lets a wrapper swallow the block beneath it;
`--url` on the read verbs, so a look at a page is one round trip rather than
two; `Domain` cookies; an address-level rebinding check, so the receipt cannot
name a host the bytes never reached; record-and-replay over durable selectors;
a real Canvas 2D; `wss://`; a `structured` verb; and a counter for verb names
callers asked for and this engine does not have.

Three of those were **not** Lightpanda's ideas but its absences, found by
reading its code beside ours: it fakes canvas with sixty-one silent no-ops, it
pays wall-clock time for every timer, and it has no receipts at all. What the
comparison was most useful for was the three costs it found in *our* load path,
which are §B16.10's queue and are not built yet.
