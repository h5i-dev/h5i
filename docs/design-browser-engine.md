# Design: the browser engine

> A pure-Rust browser that lives inside the agent's own sandbox, renders on
> demand, and can prove what it did.

Sections B1 to B5. The code is `crates/h5i-browser-light`.

## In one screen

- Blitz owns the DOM, Stylo the CSS, vello_cpu the raster, Boa the script.
- The engine *is* the HTTP client, so the receipt is the network rather than an
  observation of it.
- Web Platform Tests, core tier: 75.7%. A production React build is not cleared.
- No iframes, no vendored engine crates, and a long list of things that will
  never be built (B4).

Part of the h5i design set. The roadmap, and what is next, is
[`ROADMAP.md`](../ROADMAP.md). Superseded positioning and the build logs are in
[`roadmap-history.md`](roadmap-history.md).

---

Section 12 of [`roadmap-history.md`](roadmap-history.md) records the *decision*
to build a local engine that runs script; these sections are where it got to.


## The session surface

### The id is not the interface

`h5i browser open` makes a session and points the *default* at it; every verb
that follows lands there. The opaque id (`br_7k2xqa`) is in the record, in
`--json` and in the receipts, because a durable reference has to survive a
rename. It is not what anyone types. Names are for running several at once
(`--session auth`), and a name is comfortable precisely because it is *not* an
identity, so it can be reused once its session has ended.

Two rules fell out of building it, both about not moving under an agent:

- No "if only one is live, use it". It reads as helpful and silently redirects
  the next verb the moment a second session exists.
- The default outlives the session it names, so the next bare verb can say *"the
  session you were on was closed"* rather than *"no session is open"*. Only a
  pointer to a record that is gone is dropped.

### What is agent-facing, and what is not

| concept | agent-facing? |
| --- | --- |
| session | yes, but usually implicitly: `open` sets the default and verbs follow it. |
| session name | yes, for running several at once (`--session auth`). |
| session id | no. Durable reference, in `--json` and receipts; never typed. |
| tab | yes, when there is more than one page in a session. |
| box | yes, but as a *placement*, never as part of a session's definition. |
| connection, worker, CDP session | no. Internal, and deliberately unnamed. |

The rule the table encodes: a thing that is a session's own implementation does
not get a name in the CLI, and a thing that stands beside a session does.

### Built, 2026-08-27

- `browser_session`: the host-owned registry. Ids never reused, five states,
  endings written down, `EXIT_SESSION_GONE`, host-named artifacts, and the
  scrubber every relayed answer goes through.
- `h5i browser` as the front door: `start`, `list`, `status`, `close`, the
  fourteen session verbs, and the control lock moved onto the session.
- `--in <box>`: the engine runs as a *service*, since the writer lock would
  otherwise shut every later verb out of its own box, and verbs arrive over a
  Unix socket, since every `box run` gets a fresh netns and a port cannot be
  reached from the next run. Preflighted, so a box that cannot hold a session
  says why before anything starts.
- `env::service_start_with_def` and the engine's `--control-socket`.

### Open, and honest about it

- Supervised and container cannot hold a resident process (h5i-sandbox's
  `spawn_background`, "Idea 3.5"), and they are also the two tiers that enforce
  an egress allowlist on Linux, so the only tier that both holds a session and
  earns `host-observed` is `microvm`. Closing this is the highest-value
  remaining work: it makes the central claim reachable on an ordinary Linux box.
- One session per box. A second would need per-session service names and stream
  files.

---

## B1. Where it stands, 2026-08-28

Built and driven end to end: render, snapshot, screenshot and receipts, with
Blitz owning the DOM, Stylo the CSS and vello_cpu the raster. A resident session
several viewers and a control channel share. Cookies over a public suffix list,
persisted only when h5i asks by name. A fenced snapshot, so page text reaches an
agent labelled as data. An action log, and a replay that re-executes it.
JavaScript through Boa, with events, timers and microtasks on a virtual clock
and `fetch` through the broker.

Web Platform Tests, core tier, full fresh sweep 2026-08-28:

    core tier      75.7% (88,199 / 116,471) with the vendored :has() stylo,
                   ~74.5% after its removal. 80% is the next target.
    html/semantics 9,623 · html/dom 62,092 · css/selectors 3,090
    css-conditional 1,601 · custom-elements 2,414 · dom 3,278 · domparsing 384

The `html/dom` figure is bimodal, 58,313 on a loaded machine against 62,183 on
an idle one, because one idlharness file times out. Run the gate on an idle box
before reading a regression into it.

Not cleared: a production React build, which §12.4 of the history document sets
as the bar. What runs is a hand-written application of the right shape.

Where the next ~5,000 subtests live, measured and ranked: the idlharness file
itself (2,628 failing, mostly capability interfaces this engine refuses to
fake), html/semantics' script/img/media/dialog clusters (~6,400), dom's
XML-document family (~600), cssom serialization and scroll geometry, and the
fetch/api JS surface (~400 reachable without wptserve).

---

## B2. Architecture, and the constraints that chose it

Four decisions the compiler or the dependency graph made rather than preference.

*One thread owns the page.* `Page` is not `Send`: Blitz's `BaseDocument` holds
an `Arc<dyn HtmlParserProvider>` and a `Box<dyn FontMetricsProvider>`, neither
thread-safe, so there is no `Arc<Mutex<Session>>` to be had. The page has a
single owning loop and everything else reaches it by channel.

*The Rust DOM is the single source of truth.* Every JS object naming a node is a
wrapper over a `NodeId`. A second tree would let the snapshot, the paint, the
events and the script state drift apart.

*The object model lives in a JavaScript prelude.* Listeners, timer callbacks and
promise resolvers are GC-managed, and holding them Rust-side means tracing them
through Boa's collector. Putting them where Boa already owns their lifetime
leaves a Rust surface of about twenty primitives taking ids and strings.
Compiled once per thread rather than per realm.

*Boa is a fork, pinned by revision.* `boa_engine` and `boa_gc` come from
`h5i-dev/boa` at 0.22.0 (`Cargo.toml`), carrying one commit that adds
`bind_to_realm` so a compiled prelude can be reused across realms. The older
0.19 pin, and the ICU clash with parley that forced it, are gone;
`scripts/check_boa_release.sh` asks crates.io on every CI run whether a
published boa would do, and fails the build the day one would. Patch both crates
together, and note that a `Gc` in a `thread_local` must be `ManuallyDrop` or the
thread aborts at exit.

---

## B3. Security: what script bought and what it cost

Loopback is reachable from a loopback document. `Policy::check` took only a URL,
and loopback is allowed unconditionally because the box's dev server is the
point. Before script an untrusted page could *cause* a loopback request but not
read the response; with `--script` it could `fetch` the dev server, read the
body, and POST it anywhere in `net.egress`: a read primitive against the code
the agent is working on, past a proxy that never sees loopback.
`Policy::check_from(url, document)` closed it. This was a *logic* bug, and Rust
prevents none of them. "Fewer memory bugs" is honest; "safer browser" is earned
by the origin model, not the language.

Site isolation is the one thing the box does not replace. Chromium's process
model contains a compromised renderer against filesystem, network privilege,
crashes and cross-origin theft; the box covers the first three at a stronger
boundary and says nothing about two origins sharing one address space. The
answer is `Jar::retain_origin`: the jar is cleared on cross-origin navigation,
so one session holds one origin's cookies and a page is never in the same
address space as another origin's session. Leaving an origin drops its login,
and the snapshot says so rather than letting the agent discover it by being
logged out. `document.cookie` additionally withholds `HttpOnly`.

The gate is still honoured: `capabilities.javascript` reports the running
configuration, script is opt-in, and with it off `<script>` elements are inert.
The same-origin policy proper lives in `cors.rs`, added once the `Domain`
attribute turned an unauthenticated cross-origin read into an authenticated one.

---

## B4. What this browser deliberately is not

A disposable sandbox removes most of a browser's surface as a *requirement*, not
as a compromise. None of the following is planned, and each should be refused in
review rather than re-argued.

Never: tabs, bookmarks, history UI, downloads manager, password saving,
autofill, extensions, sync, printing, DRM/EME, WebRTC, WebTransport, WebGPU,
WebXR, Bluetooth/USB/Serial/HID/MIDI, camera, microphone, geolocation, sensors,
desktop notifications, push, background sync, Service Workers, Cache Storage,
File System Access, popups, multiple windows, picture-in-picture, fullscreen,
XSLT, FTP.

Simplified rather than absent, and always in memory:

* cookies: session lifetime, persisted only when h5i passes `--cookie-jar`
* `localStorage`/`sessionStorage`: small maps, never a file
* history: the current page and a short navigation list
* clipboard: a sandbox-local buffer, never the host's
* dialogs: `alert` to the console, `confirm` from policy, `prompt` refused
* downloads: handed up to h5i as a response, never written as a file

Not cut, because cutting them makes this a static HTML renderer rather than a
browser: DOM mutation and query, CSS cascade with flex/grid/position/overflow,
click/input/change/submit/focus/keyboard, promises and microtasks and timers,
`fetch` with redirects and TLS, ES modules, forms, images, web fonts,
navigation, the rendered result, and console plus exception capture.

No iframes. Not "same-origin only": none. Each iframe is a second document, a
second script realm and a navigation boundary. It is a second browser.

No vendored engine crates. A 5.6MB in-tree copy of stylo bought `:has()` in
stylesheets and was reversed by owner decision on 2026-08-28: no WPT arithmetic
pays for a fork carried across every stylo bump. The query half of `:has()` is
evaluated in the prelude instead (`withHasMarkers`), so `querySelector`,
`querySelectorAll`, `matches` and `closest` keep it. Stylesheet rules using
`:has()` stay lost until Blitz depends on stylo >= 0.20.

---

## B5. The rule that produced all of it

Nothing is built until a page asks for it, and an instrument that cannot name
what is missing is fixed before anything it failed to name.

The claim is deliberately not speed: this class of engine is slower than
Chromium in wall time, and anyone can beat a benchmark table by shipping less
browser. What no one else can copy back is proving what the engine did, because
that depends on the engine *being* the HTTP client rather than being watched by
one.

Sections B1 to B22 of [`roadmap-history.md`](roadmap-history.md) carry the build
log: the corpus runs, the WPT campaigns, the reference engines read, and the
reversals.
