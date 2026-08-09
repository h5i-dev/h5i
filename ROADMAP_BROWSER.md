# ROADMAP: the browser

Status: 2026-08-09. The forward plan for `crates/h5i-browser-light`. `ROADMAP.md`
§12 records the *decision* to build a local engine that runs script and why;
this is the work. Where the two disagree, §12 is the authority on scope and this
is the authority on order.

> **A pure-Rust browser that lives inside the agent's own sandbox, renders on
> demand, and can prove what it did.**

Three claims, and only the third is unique. Pure Rust is a real property (no C
toolchain, a smaller memory-bug surface) but it is a means. Rendering on demand
is what separates this from Lightpanda. **Proving what it did** is the one
nobody else can copy back, because it depends on the engine *being* the HTTP
client rather than being watched by one.

The claim is deliberately not speed. By Kitesurf's own numbers this class of
engine is slower than Chromium in wall time, and a benchmark table is something
anyone can beat by shipping less browser.

---

## 1. Where it is, 2026-08-09

Built and verified end to end:

* **Render, snapshot, screenshot, receipts.** Blitz owns the DOM, Stylo the CSS,
  vello_cpu the raster. Every request is policy-checked and recorded *before* it
  moves: no receipt, no request.
* **A resident session.** `serve` holds a page several viewers and a control
  channel share. `session status|snapshot|navigate|scroll|type|submit|click`.
* **Cookies**, host-only and in memory, so a login works and nothing persists.
* **A fenced snapshot**, so page text reaches an agent labelled as data.
* **An action log**, box-claimed, so `h5i ui`'s agent-actions pane has a source.
* **JavaScript, as a limited preview.** Boa plus a bindings layer; events with
  capture and bubble; timers and microtasks on a virtual clock; `fetch` through
  the broker. Opt-in behind `--script`.

The sentence the whole design exists to produce, working today:

```
$ h5i-browser-light session click @e1
{"ok":true,"ref":"@e1","requests":["http://localhost:8231/api/item"],
 "settled":"settled after 0ms"}

200 navigation  /index.html
200 subresource /app.js      <- the script file, fetched before it ran
200 subresource /api/item    <- what the click caused
```

Not cleared: **a production React build**, which `ROADMAP.md` §12.4 sets as the
bar. What runs is a hand-written application of the right shape.

---

## 2. Architecture, and the constraints that chose it

Three decisions were made by the compiler or the dependency graph rather than by
preference. They are recorded because each one will look arbitrary later.

**One thread owns the page.** `Page` is not `Send`: Blitz's `BaseDocument` holds
an `Arc<dyn HtmlParserProvider>` and a `Box<dyn FontMetricsProvider>`, neither
thread-safe. There is no `Arc<Mutex<Session>>` to be had. So the page has a
single owning loop and everything else reaches it by channel. That is the right
shape for a multi-driver session anyway; here it was not optional.

**The Rust DOM is the single source of truth.** Every JS object naming a node is
a wrapper over a `NodeId`. A second tree inside the engine would let the
snapshot, the paint, the events and the script state drift apart, with nothing
downstream able to say which was right.

**The object model lives in a JavaScript prelude.** Listeners, timer callbacks
and promise resolvers are GC-managed; holding them Rust-side means tracing them
through Boa's collector. Putting them where Boa already owns their lifetime left
a Rust surface of about twenty primitives taking ids and strings, and turned
event propagation into ordinary code instead of a lifetime problem.

**The Boa pin is 0.19 and it is a workaround.** Boa 0.20+ requires
`icu_normalizer ~2.0`; `parley`, which Blitz pulls for text, requires `^2.1.1`.
Disjoint and semver-compatible, so Cargo must unify and cannot. 0.19 uses the
1.x line, semver-*incompatible* and therefore allowed to coexist, at the cost of
two ICU stacks in the build. Upstream Boa's `main` is already at `~2.2.0`, so
this unwinds on their next release. **Exit condition: Boa releases past that
change.**

---

## 3. Security: what script bought and what it cost

### 3.1 The loopback hole — **closed 2026-08-09**

`Policy::check` took only a URL, and loopback is allowed unconditionally by
default because the box's dev server is the point. Before script, an untrusted
page could *cause* a loopback request but not read the response. With `--script`
it could `fetch` the dev server, read the body, and POST it anywhere in
`net.egress` — a read primitive against the code the agent is working on, past
the egress proxy that never sees loopback.

Closed by `Policy::check_from(url, document)`: loopback is reachable **from a
loopback document**. A page served by the dev server may talk to it; a page from
the open web may not. Tested both directions
(`a_web_page_cannot_read_the_dev_server_and_never_reaches_the_wire`,
`the_dev_servers_own_page_still_reaches_it`).

Worth keeping in front of the reader: this was a **logic** bug, and Rust
prevents none of them. "Fewer memory bugs" is honest; "safer browser" is earned
by the origin model, not the language.

### 3.2 Site isolation is the one thing the box does not replace

Chromium's process model exists to contain a compromised renderer: filesystem,
network privilege, crash isolation, and cross-origin theft. The box covers the
first three at a stronger boundary than a renderer sandbox. It does not cover
the fourth — it protects the host from the box and says nothing about two
origins sharing one address space.

That did not matter while the engine held nothing worth stealing. The cookie jar
shipped on 2026-08-08 and script on 2026-08-09, so it mattered.

**Answered 2026-08-09, by the second of the three options**: the jar is cleared
on cross-origin navigation (`Jar::retain_origin`), so one session holds one
origin's cookies and a page can never be in the same address space as another
origin's session. The cost is stated where a user meets it — leaving an origin
drops its login, and the snapshot says so rather than letting the agent discover
it by being logged out. `document.cookie` additionally withholds `HttpOnly`,
which is the line between what the wire carries and what script may read.

### 3.3 The gate, still honoured

`capabilities.javascript` reports the *running* configuration; script is opt-in;
with it off, `<script>` elements are inert exactly as before. Nothing has
flipped by default and nothing should until 3.1 and 3.2 are answered. See
`ROADMAP.md` §12.5.

---

## 4. Three things that were wrong rather than missing — **all fixed**

"Missing" is honest and reports itself. These were worse: they corrupted a page
while looking like they worked, which is the failure mode the fence and the
unsupported-API log exist to prevent, and they polluted every measurement taken
before they were fixed. Kept here because the *class* is the lesson, not the
three bugs.

1. ~~`innerHTML` getter returned `textContent`~~ — all markup stripped, so
   `el.innerHTML = el.innerHTML` destroyed the subtree. Now a real serialisation.
   The root cause was upstream of the getter: `DocumentConfig` never set an
   `html_parser_provider`, so `set_inner_html` silently did nothing.
2. ~~`createDocumentFragment()` returned a `<div>`~~ — appending a fragment
   injected a real element that broke `.parent > .child` and layout. Now a real
   fragment, and one that can be searched (§8.6).
3. ~~`Element.style` did not exist~~ — `el.style.display = 'none'` threw and
   killed the script at that line. Now a real `StyleDeclaration`.

The same class keeps recurring and is worth naming: **a plausible answer is
worse than no answer.** `matchMedia` returning false to everything, `scrollTop`
computed from the bounding rect, `structuredClone` via a JSON round trip, and
`clientHeight` for `documentElement` were all this bug wearing different clothes.

---

## 5. The bindings backlog

Ordered by what blocks real applications first. Cross-referenced against
Thalora's surface (§7) where that project has already mapped the ground, and
marked **cheap** where Blitz or Stylo already holds the answer and we are merely
refusing to give it.

### Tier A — blocks nearly everything modern

| | why | note |
| --- | --- | --- |
| ~~ES modules and `import()`~~ | every production bundle ships `<script type="module">` | **built**, through the broker; bare specifiers are refused rather than rewritten to a CDN |
| ~~`Element.style` (CSSOM)~~ | `el.style.display = 'none'` is ubiquitous | **built** |
| ~~`getBoundingClientRect`~~ | every popover, dropdown, drag and virtual list | **built** — Blitz computes `final_layout` already |
| ~~`getComputedStyle`~~ | feature detection and measurement | **built** — via Stylo's `to_css_string`, not `Debug` |
| ~~`MutationObserver`~~ | frameworks depend on it | **built**. The semantic delta went its own way in the end — diffing two outlines, not observing mutations (§8.7) |
| ~~`IntersectionObserver`, `ResizeObserver`~~ | lazy loading, virtual lists, responsive components | **built 2026-08-09**, driven from the settle loop (§8.2) |
| ~~`localStorage` / `sessionStorage`~~ | absence throws or breaks init paths | **built**, deliberately non-persistent — see §6 |
| ~~`history.pushState`~~ | SPA routing | **built**, and it moves `location` with it — for a while it did not, so a router reading its own route back got the page it had already left |

### Tier B — blocks a large fraction of real applications

All built, most of it driven by §8 rather than by this list:

* Real event types — `MouseEvent`, `KeyboardEvent`, `InputEvent`, `CustomEvent`
  with `detail` — plus `on*` handler properties.
* Form semantics: `input`/`change` on typing, checkbox, radio, `select` with a
  live `selectedIndex`, `FormData`.
* `closest()`, `matches()`, `dataset`, `cloneNode`, `insertAdjacentHTML`, a real
  `DOMTokenList` over whichever attribute holds the tokens.
* `AbortController`, `Headers`, `Request`, and **concurrent `fetch`** — six on
  the wire at once, so an SPA's fan-out is no longer a waterfall of our making.
* `window.scrollTo`, `scrollY`, and the viewport dimensions, which nothing had
  ever exposed.

### Tier C — the tail

Built since, because a real page asked: **custom elements** (define, upgrade
existing markup, the lifecycle callbacks), `TextEncoder`/`TextDecoder`,
`structuredClone`, `crypto.getRandomValues` and `randomUUID` over the OS CSPRNG,
`XMLHttpRequest` over the same queue `fetch` uses.

Still absent, and still unscheduled: Canvas 2D, WebSocket, Workers,
**WebAssembly**, Shadow DOM, SVG DOM, Streams. Shadow DOM is the interesting
one — the application corpus includes two design-system sites that use it, and
neither asked for it, because their documentation pages are server-rendered.
That is the rule working: nothing here is added until a page in §8 needs it.

---

## 6. What this browser deliberately is not

A disposable sandbox removes most of a browser's surface as a *requirement*, not
as a compromise. None of the following is planned, and each should be refused in
review rather than re-argued:

**Never**: tabs, bookmarks, history UI, downloads manager, password saving,
autofill, extensions, sync, printing, DRM/EME, WebRTC, WebTransport, WebGPU,
WebXR, Bluetooth/USB/Serial/HID/MIDI, camera, microphone, geolocation, sensors,
desktop notifications, push, background sync, Service Workers, Cache Storage,
File System Access, popups, multiple windows, picture-in-picture, fullscreen,
XSLT, FTP.

**Simplified rather than absent**, and always in memory:

* cookies — session lifetime only, destroyed with the process
* `localStorage`/`sessionStorage` — small maps, never a file
* history — the current page and a short navigation list
* clipboard — a sandbox-local buffer, never the host's
* dialogs — `alert` to the console, `confirm` from policy, `prompt` refused
* downloads — handed up to h5i as a response, never written as a file

**Not cut, because cutting them makes this a static HTML renderer rather than a
browser**: DOM mutation and query, CSS cascade with flex/grid/position/overflow,
click/input/change/submit/focus/keyboard, promises and microtasks and timers,
`fetch` with redirects and TLS, **ES modules**, forms, images, web fonts,
navigation, the rendered result, and console plus exception capture.

**No iframes.** Not "same-origin only" — none. Each iframe is a second document,
a second script realm and a navigation boundary. It is not a feature, it is a
second browser.

---

## 7. Thalora: read it, do not adopt it

`Brainwires/thalora-web-browser` (MIT, 216k lines of Rust, Boa-based, built for
agents) is the same thesis and worth reading closely. It is proof that this much
*can* be built on Boa. It is not evidence that this architecture gets you there
faster, and three of its choices are worth studying specifically as things not
to repeat.

### 7.1 Why it cannot be a dependency

1. **It is built on Boa's internals, not Boa's public API.** Its `Document` uses
   `IntrinsicObject`, `BuiltInBuilder` and `StandardConstructors`, which upstream
   Boa declares `pub(crate)`. That is why `engines/boa` is a submodule pointing
   at their own fork. Using their bindings means owning a fork of a JavaScript
   engine and its security updates.
2. **Its DOM is its own** — `html5ever` plus `taffy`, state in
   `Arc<Mutex<HashMap<..>>>`. Our bindings sit on Blitz's `BaseDocument`, which
   is also what Stylo styles and what we paint. Porting means rewriting the body
   of every binding; only the shape transfers.
3. **It does not paint.** No rasteriser, no screenshot: `taffy` is layout only.
   The visual half, which is what makes `h5i ui` possible and separates us from
   Lightpanda, is not in there.

It also uses hand-rolled CSS over `taffy` where we get **Stylo**, Firefox's
production cascade, through Blitz. Moving toward their stack would be a
compatibility downgrade.

### 7.2 Three cautionary findings, checked against the source

**It has the dual-DOM problem this design exists to avoid.** JavaScript mutates
Boa-side element data; layout runs over a *separately re-parsed* tree —
`renderer/layout_bridge.rs:212` calls `scraper::Html::parse`, and the CSS path
builder walks scraper's `ElementRef`. So the DOM script sees and the DOM that is
laid out are not one tree, synchronised through serialised HTML. That is exactly
the drift §2 refuses, and it is the strongest available argument for the
`NodeId`-wrapper rule: mutations must apply to the Blitz DOM directly, never via
an HTML string.

**Its module loader bypasses its own network layer, and invents a CDN.**
`module_loader.rs:129` builds a private `reqwest::blocking::Client`, so module
fetches never pass whatever policy the rest of the browser applies. Worse,
`module_loader.rs:103` maps bare specifiers to a CDN:

```rust
Ok(format!("https://esm.sh/{}", specifier))
```

`import "lodash"` silently becomes a request to `esm.sh`. That is not a web
standard, and in a sandbox it is an unrequested external dependency introduced
by the engine itself. **When we build ES modules (§5 Tier A), every module fetch
goes through the same broker as HTML, `fetch`, images and fonts, and a bare
specifier that does not resolve is an error the agent reads — not a silent trip
to a third party.**

**It reports a thrown exception as success.** `renderer/execution.rs:256`, after
printing the error:

```rust
Ok("undefined".to_string()) // Return success with undefined result
```

This is the failure mode this whole engine is organised against: silent-wrong is
worse than missing. Our equivalent path returns the error, surfaces it in the
page console, and the snapshot says when a page did not finish. Their README's
"Chrome 131 compatibility" and "Zero Mock Implementations" should not be read as
real-site compatibility evidence; the WASM-target stubs are honestly labelled,
but `browser/selection.rs` returns a literal `"selected text"` placeholder, and
the line above turns a broken page into a passing one.

### 7.3 What it is genuinely worth

Its module inventory is the best available map of which Web APIs an agent
browser needs, written by someone who did the work — `dom/` is 25k lines,
`events/` 7.6k, `storage/` 12k, with a file per API. §5 cites it per row for
exactly that reason.

The right way to use it: **extract the backlog and the test cases, not the
code.** For each API we take from their list, find the matching Web Platform
Test and make that our test, so our compatibility claim rests on the standard
rather than on their implementation. Their Boa binding *patterns* are worth
reading; their DOM, network and renderer architecture is not worth adopting.

## 8. Measure, then build

Which APIs matter cannot be answered from a chair, and the instrument already
exists: every unsupported call is counted and surfaced in the snapshot.

**The corpus run.** Point the engine at fifty real sites with `--script`,
collect the ranked counts, and let the priority order write itself:

```
note: this page used Web APIs this engine does not have
      (Element.style x41, MutationObserver x6, closest x4)
```

An afternoon, and it turns §5 from a considered guess into a table. It must
happen *after* §4, or the results measure our own bugs.

Where the corpus and Thalora's inventory agree, build it. Where they disagree,
the corpus wins: it is this decade's web, not a specification of it.

### 8.1 First run, 2026-08-09

28 sites: docs, references, wikis, standards, package pages, news, and a few
script-heavy ones so the failures would be honest.

```
27/28 loaded; 23 gave a usable outline (>=5 lines)
 0 rendered materially more *with* script
 0 failed to settle within budget

api                      sites  calls        console errors
matchMedia                   4      5        17  could not load https (cross-origin, denied)
document.cookie              3      7        13  TypeError
IntersectionObserver         1      1         6  ReferenceError
setInterval                  1      1
```

**It found three bugs before it found any missing APIs**, which is the argument
for running it at all:

* `<script type="application/json">` was being **executed**. Every `<script>`
  ran regardless of `type`, so pages embedding state as JSON — github.com does —
  had it parsed as JavaScript, filling the console with syntax errors that blamed
  the page.
* **HTTP errors were rendered as the page.** crates.io answered 404, the engine
  rendered the error body, and the outline came back empty with nothing anywhere
  saying why. The status was in the request log and nowhere an agent looks.
* **Missing APIs did not name themselves.** A global we never defined threw a
  bare `ReferenceError`; a method on a half-defined object threw
  `TypeError: not a callable function`. Neither reached the unsupported list, so
  the measurement could not see them — the method depends on missing things
  reporting themselves, and they were not.

**The headline result: for the pages agents actually read, script adds nothing
to the outline.** Not one of 28 sites rendered materially more with `--script`
than without. Docs, references and wikis are server-rendered; script adds
interactivity, not content. That is a real finding about the workload and it
argues the reading case was close to solved before any of this.

Two caveats keep it from being stronger than it is. The harness allows only the
page's own host and a few common CDNs, so **17 cross-origin scripts were denied
by policy** and those bundles never ran — the script-heavy end of the corpus is
therefore under-tested. And the remaining 13 TypeErrors and 6 ReferenceErrors are
still anonymous: they come from pages touching DOM properties we return
null/undefined for, which the `missingApi` list does not cover because they are
not globals.

**What the corpus asks for next**, in its own order: `matchMedia` (answered now,
still recorded), `document.cookie`, `IntersectionObserver`, `setInterval`.

`document.cookie` is the interesting one, because it looked like a deliberate
refusal and turned out to be a false choice. See §8.2.

### 8.2 Second run, same day: the list is empty, and that is not the same as done

All four were built, and the corpus now asks for nothing:

```
27/28 loaded; 23 gave a usable outline
 0 rendered materially more *with* script
 0 failed to settle

api                      sites  calls        console errors
(nothing)                                    17  could not load https (cross-origin, denied)
                                             13  TypeError
                                              6  ReferenceError
```

**An empty unsupported list beside 19 anonymous errors is a misleading result,
and it is the honest state of things.** Those errors come from pages touching
DOM *properties* that return null or undefined, not from globals, so
`missingApi` — which covers globals — cannot name them. The instrument now
reports nothing because it cannot see what is left, which is a different fact
from there being nothing left. Naming those is the next measurement problem, and
it has to be solved before another run means much.

### 8.3 Fixing the instrument, which was the actual next task

Two blind spots, closed:

* **Unknown properties on objects we own.** `wrap()` and `document` now return a
  `Proxy` whose `get` records a name that is on neither the prototype chain nor
  the object itself. A property we implement takes the plain path, and so does
  an expando the page assigned and reads back, so **a working page records
  nothing at all** — the list stays a list of gaps rather than a log of traffic.
* **Undeclared globals.** No proxy can trap `Sentry.init(...)`: it throws before
  any object is consulted. The thrown `ReferenceError` carries the name, so
  `note_error` reads it back. Only identifier-shaped names are accepted, because
  the list is read by an agent and a page must not get to write into it by
  throwing a chosen string.

The run immediately after named 15 properties where there had been fog, and a
second pass named five globals. Answering both rounds moved the errors:

| | before | naming fix | answered |
|---|---|---|---|
| named asks | 0 | 15 | 14 |
| `TypeError` | 13 | 13 | 10 |
| `ReferenceError` | 6 | 6 | 3 |

**`TypeError` went 8 → 10 partway through, and that was progress.** Exposing
`HTMLElement` let `class X extends HTMLElement` get *further* before failing, at
`customElements.define` — which the list now names. A count going up because
pages reach deeper is the shape of a real measurement.

Two things the remaining list should not be misread as:

* **`$` is not an engine gap.** It is jQuery, from a CDN the corpus policy
  denied. The page is right to fail; the fix is a policy decision about asset
  hosts, not a binding.
* **The residual `TypeError`s are mostly selector misses** — `querySelector`
  returning null for markup that genuinely is not there. That is correct
  behaviour, reported honestly, and no amount of API work removes it. Naming
  *where* it happened needs source positions from Boa, which is a separate job.

### 8.4 Answering the named list, and what it caught in the answers

Everything §8.3 surfaced is built. In order of what they were worth:

* **Custom elements, for real.** `define` upgrades the markup already on the
  page, delivers the initial values of `observedAttributes`, and runs
  `connectedCallback` once the node is genuinely in the tree. Defining without
  upgrading would have been the worse kind of half-support: a page that renders
  its markup server-side and defines its components in a deferred bundle — most
  of them — would register everything, see no error, and render nothing. The id
  reaches the constructor out of band through a construction slot, because
  `super()` takes no arguments and the class never sees the node it is
  attaching to.
* **Real comment nodes**, so a template library's anchor stays out of the
  outline an agent reads instead of appearing as stray text.
* **`scrollTop`/`scrollHeight`/`clientHeight`** answering from the document
  rather than from the element's own box, since `scrollTop + clientHeight >=
  scrollHeight` is how every bottom-of-page check is written and it has to be
  *true at the bottom*. `clientHeight` already existed, computed from the
  bounding rect, which for `documentElement` is the page height rather than the
  window — so the idiom read "already at the bottom" everywhere.
* **`window.innerWidth`/`innerHeight`/`scrollY`** and the scroll methods, which
  nothing had ever exposed. This one the instrument could not have found:
  nothing wraps the global object, so they were simply undefined, and a layout
  that measures instead of asking `matchMedia` got `NaN` out of its own
  arithmetic. Found while chasing an unrelated scroll bug.
* `compareDocumentPosition`, `contains`, `getRootNode`, `isConnected`,
  `defaultValue`, `getElementsByTagName`, `getElementsByName`, `importNode`,
  `createNodeIterator`/`createTreeWalker`, and `implementation` — which names
  `createHTMLDocument` as refused rather than handing back a broken document,
  because a second document really is out of reach when there is one tree.

**The run after that caught three bugs in the answers themselves**, which is the
argument for the instrument in one line:

| reported | what it actually was |
|---|---|
| `Element._h5iConnected` | *our own* bookkeeping flag, stored on the node, read before it was set |
| `Element.tagName` | a page reading `tagName` off a **text** node — every node was labelled "Element" |
| `$`, still | jQuery that *loaded and threw*, not one that was refused |

All three are fixed: the flag moved off the nodes, labels follow the node's
actual type, and a script that throws is recorded as not-run alongside one that
was refused — its globals are undefined either way.

That left one ask, `Text.tagName`, and it was a false positive worth a rule:
**a gap is only a gap if a real browser would have answered.** An element
property read off a text node returns undefined in every engine there is, so
claiming it would have sent us building something that does not exist. The
proxy now stays quiet in exactly that case, and `document.namespaceURI` and
`ownerDocument` are defined-as-undefined and null for the same reason.

### 8.5 Where the corpus stands

```
27/28 loaded; 23 gave a usable outline; 0 failed to settle
asks: (none)
errors: 33, of which 0 are anonymous
        17  cross-origin subresources the corpus policy denied
         3  "`$` is missing because a script this page needed did not run: ..."
        13  page errors, each prefixed with the script it came from
```

**Zero anonymous errors is the number that matters**, not the empty ask list.
Every remaining line names either a request we refused or the script that
threw. Boa 0.19 gives neither a line number nor a stack, so the script element
is the finest locus available; a real position needs engine support we do not
have, and that is now the only thing in the way of an agent debugging a page it
is reading.

One page also rendered materially more *with* script for the first time — the
Rust book, 35 lines to 171 — which is the first evidence in this file that
running script buys an agent anything at all on a real documentation site.

What the four turned into:

* **`matchMedia` answers from the real viewport.** Returning `false` to
  everything is not neutral: a responsive layout asks and then commits to the
  branch it was told, so a wrong answer is a wrong page rather than a missing
  feature. `min-width`, `max-width`, `orientation` and `prefers-color-scheme`
  have correct answers at a fixed viewport with a known scheme; a feature
  outside that set still records itself.
* **`document.cookie` exists, and honours `HttpOnly`.** The earlier framing —
  that exposing it would break "an agent can be logged in without reading the
  credential" — was a false choice, because a browser has the same problem and
  solved it: a session cookie is almost always `HttpOnly`, and that flag is
  exactly the line between what the wire carries and what script may see. The
  jar had been parsing `HttpOnly` and dropping it, which was harmless until
  script existed and is not now. Page script sees the non-`HttpOnly` cookies;
  the session stays out of reach.
* **`setInterval` repeats**, and deliberately does *not* hold the page open.
  Waiting for a perpetual timer to drain would mean a page with a clock, a
  carousel or an autosave could never be described as settled, and every
  snapshot of it would carry a "still busy" note that told an agent nothing.
  Virtual time advances only as far as pending one-shot work requires, and
  intervals fire along the way.
* **`IntersectionObserver` and `ResizeObserver`** are driven from the settle
  loop rather than a frame clock, because this engine has no frames at rest and
  an observer waiting for a repaint would never fire at all. Intersection
  reports edges rather than every settle, so a page that lazy-loads on entry is
  told once.

---

### 8.6 A second corpus: applications, not documents

The document corpus reached zero asks and zero anonymous errors, and then
stopped being informative — **because four of its 28 pages still rendered
nothing and not one of them was a missing API**:

| site | why | not |
|---|---|---|
| crates.io | server answered **404** to a request that sent no `Accept` | an API gap |
| stackoverflow | **403** bot wall, rendering as one line | an API gap |
| json.org | a `<meta refresh>` this engine never followed | an API gap |
| vitejs.dev | redirected to vite.dev, correctly refused, unhelpfully explained | an API gap |

That inverted the plan: the next frontier was the network layer and the honesty
of the report around it, not more bindings. All four are fixed (§8.8), and
crates.io answers 200 and json.org renders 299 lines instead of 1.

So the corpus was **pointed at applications instead** — SPAs, interactive demos,
design systems — because a documentation corpus will never ask for routing,
storage or template cloning when it contains nothing that does them. It named,
immediately and specifically:

* **`<template>.content`** — and this was not a small gap. Its absence made
  `template.content.cloneNode(true)` throw `cannot convert 'null' or 'undefined'
  to object`, which was the *entire text* of **fifteen module failures**. Clone,
  query, fill, append is how every framework renders a row.
* **Scoped selector queries that do not scope.** `query_selector_all` always
  starts at the document root and the engine narrowed by ancestry afterwards, so
  a **detached** subtree was invisible — which is every cloned template before it
  is inserted, exactly when a framework searches one. Stylo's fast path consults
  the document's id and class caches and reports "handled, nothing found" rather
  than falling through, so scoped queries now walk the subtree and match element
  by element. `matches()` had the same bug and answered false for anything
  detached.
* **`location.pathname`**, which was undefined, and `pushState`, which never
  moved the address at all.
* `relList`, `attributes`, `firstElementChild`, `getAnimations`,
  `document.contentType`, `meta.content`, `on*` handlers.

### 8.7 What the instrument caught in its own reflection, twice more

* **A framework's private field is not an API gap.** Solid reads
  `document._$DX_DELEGATE` before setting it, and the ask list carried it as
  something this engine was missing. No web platform property begins with `_` or
  `$`.
* **"module failed" names nothing** — the same anonymity §8.3 removed from
  script errors, one level up. Modules now carry their specifier into the
  failure. The reporting proxy also watches `location`, `history`, `navigator`,
  `performance`, the storages and `crypto`, which is where the last unnamed
  failures were hiding.

**The corpus now lives in the repository**, after a crash took the only copy
along with the scratchpad it sat in. `corpus/run.py` is the network instrument;
`tests/corpus.rs` is the part CI runs — the same patterns against local
fixtures, asserting the two properties that matter, and it found two real bugs
the moment it was written.

Applications corpus: 20/20 load, one ask left, **zero anonymous errors**.
Fourteen module failures remain, each now attributed to a named bundle. Going
further needs source positions, which is the concrete cost of the Boa
constraint below and the clearest argument for revisiting it.

### 8.8 The network layer

Not bindings, and the reason four pages read as empty:

* **Request fidelity.** No `Accept`, no `Accept-Language`, and a user agent that
  named only the crate. The agent string is honest rather than imitative — it
  names this engine and does not claim to be Chrome — and is now one constant
  shared with `navigator.userAgent`, because a page that branches on it
  server-side and again in script must see the same string twice.
* **`<meta refresh>`** is followed, with a hop limit and a visited set, and a
  refresh further out than 15 seconds is *reported* rather than followed: that is
  a page updating itself, not a redirect.
* **A refused redirect names its target.** Following it automatically would let
  a server route us out of the allowlist; saying where it wanted to go costs
  nothing.
* **Bot challenges are named**, because a challenge page renders to almost
  nothing and its outline is otherwise indistinguishable from an empty page.
* **`fetch` is concurrent** — six on the wire at once, the browsers' per-host
  figure, chosen so a page with two hundred images cannot become two hundred
  threads inside a box with a memory ceiling. Waiting on the wire uses *real*
  time against its own budget, since the virtual clock is free to advance and a
  round trip is not.

### 8.9 What it costs, measured

`cargo run --release --example perf`. Two rounds, and the second is mostly the
Boa upgrade paying for itself:

```
a DOM property read              on 0.19      on main
  plain object, no proxy            775 ns       92 ns
  watched node, known property     2460 ns      706 ns
  watched node, read from tree     6173 ns     1534 ns
```

Four times faster for nothing but a dependency bump, which is the single
strongest argument for pinning a revision over a five-month-old release.

Three things then changed on our side, each measured before and after:

1. **A page with no script no longer builds a realm.** That costs ~15 ms — 114
   KiB of prelude parsed and evaluated — and a page with nothing to run was
   paying all of it for a realm never asked a question. It is also reported
   correctly now: "had none to run" is a different fact from "script is off",
   and a page with no script is *settled* rather than unknown.
2. **Collections are no longer watched.** Wrapping a query result in the
   reporting proxy cost **3.9x on iteration** — 674 µs against 174 µs for a
   400-node result — because every index read goes through a trap and
   `for (const el of query)` is the hottest line in DOM code. An array already
   answers everything a `NodeList` does except `item` and `namedItem`, which are
   implemented, so the naming it bought was small and the price was not.
3. **`matches()` is a direct predicate.** It had been asking the *parent* for
   all matching descendants and checking membership, which made `closest()`
   walk a subtree per ancestor — quadratic on any page whose framework calls it
   in a render loop, and worth minutes on a real site.

```
reading a page                no script     script     outline
10 sections  (~90 nodes)          1.5ms     37.4ms      60 lines
100 sections (~900 nodes)        12.7ms     54.1ms     500 lines
500 sections (~4500 nodes)       69.8ms    166.2ms     500 lines

starting the script realm        15.9ms per page
queries, 200 calls each
  document.querySelectorAll        361 µs
  section.querySelectorAll           6 µs
  iterating a 400-node result      169 µs
```

The remaining fixed cost is the realm: 114 KiB of JavaScript parsed per page.
Reusing one across navigations would remove it, and is *not* safe — a page could
leave state for whatever loads next, which is the same reason the cookie jar is
cleared across origins.

**Measured and rejected**, twice, and both are recorded so nobody tries again:
precomputing the set of known property names so the reporting trap does a hash
lookup instead of walking the prototype chain changed nothing (the cost is Boa
dispatching into a JavaScript trap at all); and raising the loop bound from 5 to
50 million turned a site that returned in three minutes into one that had not
returned in four.

### 8.10 Source positions, and what they found

Boa 0.21 maps a program counter back to a source position. It is pinned by
**revision of upstream `main`**, not by release: the 0.21.1 release pins three
icu crates to `~2.0.0`, which excludes what parley requires, and parley arrives
through blitz. Upstream relaxed those pins after the release, so a pinned commit
needs no fork and no patched source — and buys five months of engine and parser
fixes over a five-month-old tag, which turned out to matter.

Two other routes were tried and rejected with evidence. **Vendoring** the two
crates worked and cost 7.5 MB and 508 files for a two-line change. **Forking**
at `v0.21.1` plus one commit also worked, and is one commit, one file, six
lines — but it is a fork to carry, and upstream `main` had already made the same
change for free.

Errors now read:

```
inline script #2: TypeError: cannot convert 'null' or 'undefined' to object
    at inner (inline script #2:2:18)
    at outer (inline script #2:3:32)
    at <main> (inline script #2:4:6)
```

The *path* mattered as much as the line: a source built from bytes carries none,
so every frame said `unknown at :2:18`, and a line number without a file is
barely better than nothing when a page has nine scripts.

**Module failures: 14 → 4.** The positions named every cause within an hour:

| named cause | fix |
| --- | --- |
| `EventTarget is not defined` | a real base class, independent of the tree — a store is not a node |
| `HTMLAnchorElement`, `HTMLButtonElement`, `HTMLTemplateElement`, … | the per-tag constructor family, all aliasing `Element` |
| `Invalid URL: /assets/…` | `import.meta.url`, which bundlers resolve every sibling asset against |
| `RuntimeLimit: exceeded recursive calls` | Boa's 512-frame default, which Next.js exceeded while merely initialising |
| `DOMParser is not defined` | parse-to-subtree, with no script inside it running |
| `not a callable function` | collections that were not collections — see below |

That last one was the instrument's blind spot again, and the most instructive.
The reporting proxy watched `document` and nodes but **not the collections and
token lists this engine builds itself**, so `querySelectorAll(...).item(0)` was
undefined and calling it produced exactly that unnamed error. Collections and
`DOMTokenList` are now watched, and immediately named their own gaps:
`createElementNS` (every framework that draws an SVG icon), `after`/`before`/
`replaceWith`/`replaceChildren`, `toggleAttribute`, `localName`, the namespaced
attribute methods, `createRange` and `elementFromPoint`.

`StyleDeclaration` is deliberately *not* watched: it answers any CSS property by
design, so it has no name it is missing, and wrapping one proxy in another
defeats the `in` check the reporting one depends on.

### 8.11 Three things that are not ours, stated plainly

1. **A Boa parser bug**, and it was worth doubting before reporting. The first
   version of this note blamed a comment; the second blamed modules. Both were
   wrong, and testing the doubt produced a far sharper bug:

   ```js
   var   a = 1
   , b = 2;        // parses
   let   a = 1
   , b = 2;        // SyntaxError: unexpected token ','
   const a = 1
   , b = 2;        // SyntaxError
   let   a
   , b;            // SyntaxError
   ```

   All four are valid JavaScript — node runs them, as script and as module. The
   asymmetry is the finding: **`var` handles it and `let`/`const` do not**, so
   this is a defect in the lexical-declaration path rather than a deliberate
   choice about semicolon insertion. Per the grammar a `,` continues a
   `BindingList`, so it is not an offending token and no semicolon may be
   inserted.

   Confirmed with this engine entirely out of the path — `Context::default()`,
   `Source::from_bytes`, no host, no module loader, no HTML — so it is not ours.
   Minified bundles that keep `/*! @license */` comments between declarators
   produce exactly this shape, which is how lit.dev fails.

   Not fixable here, and not worth working around: rewriting a page's own source
   would move every line number we just gained and could corrupt string
   literals — the plausible-wrong answer again. What *is* ours is that the
   failure names the script it came from and does not take the rest of the page
   with it, which `a_script_the_parser_cannot_read_is_named_and_does_not_take_the_page_with_it`
   pins.
2. **Two sites exceed any reasonable timeout** (lit.dev, material-web), and the
   cause is that they now get *further*. `DOMParser` unlocked execution that used
   to fail early, and removing the lying feature-detection stubs sent pages down
   polyfill paths they had previously skipped. lit.dev went from failing in
   seconds to **seven minutes** of real work.

   Two bounds were added and the second one works, for one of the two shapes a
   slow page has:

   * **Many jobs.** Boa's job executor checks a cancellation token between jobs,
     and `get_cancellation_token` hands it out as an `Arc<AtomicBool>` — so a
     watchdog thread can set it, which is the only wall-clock lever the engine
     offers. A page building 200,000 promise jobs is now stopped at 15 seconds,
     renders what it had, and says so in the engine's own voice. This is the
     shape a promise-driven page actually has.
   * **One long job.** lit.dev looked like the other shape — a module graph
     evaluating depth-first inside a *single* job, beyond any token check.

   **That second diagnosis was wrong, and wrong in the most useful direction.**
   The page was not pathological; *this engine* was slow enough to make it look
   that way. `appendChild` into the document cost 40 µs against 13 µs for a
   detached one, because every insertion walked to the root to ask whether it
   was connected and then walked the inserted subtree looking for custom
   elements — on pages that had defined none. An early return when nothing is
   defined, and a native `isConnected` that walks in Rust instead of one call
   per ancestor, took it to **7 µs, the same as the detached case**.

   lit.dev went from three and a half minutes to fifty seconds, material-web
   from a timeout to forty-five, and both now *return*. A second pass on the
   mutation-record path — the old value of an attribute was read from the tree,
   and a record object with two arrays allocated, on every write, whether or not
   anything was observing — took the hot operations to:

   ```
   createElement    5.5 µs      textContent  2.0 µs
   setAttribute     4.0 µs      appendChild  4.0 µs
   ```

   from 7 / 8.5 / 18 / 40.5 µs before either pass.

   **And then the sites did not get faster**, which is the part worth writing
   down. lit.dev renders in 0.27s without script and 46s with it, of which 0.5s
   is network; the DOM is no longer where the time goes. Nor are the budgets: a
   shared deadline across the script phase and the settle — they used to add up
   — changed nothing either, because the time is inside a *single* evaluation
   that neither a between-jobs token nor a between-scripts budget can interrupt.

   So the original diagnosis was half right and recorded too confidently in both
   directions. The engine was slow enough to turn a heavy page into a hang, and
   fixing that was worth four times on the hot path; what is left really is one
   uninterruptible unit of work, and bounding it needs an interrupt inside the
   interpreter loop. That is still upstream, and it is now the only thing
   standing between this engine and a page like lit.dev.
3. **Total CPU is unbounded.** Boa exposes no wall-clock interrupt, so the
   engine bounds what it can — one loop, recursion depth, stack size — and a
   caller that cannot wait must impose its own timeout. Raising the loop bound
   from 5 to 50 million turned a site that returned in three minutes into one
   that had not returned in four; the bound stays low enough to return, and
   trips are reported so a thin outline is explained rather than mysterious.

Both limits had to move together: raising the frame count alone changed nothing,
because the *stack size* was what a deep call actually hit.

### 8.12 A page's own errors, made legible

`console.error(someError)` rendered as `{}`, because an Error has no enumerable
own properties and the console used `JSON.stringify`. remix.run produced **1487
lines saying exactly that**, and the message — the one part an agent needed —
was what got thrown away. Errors now render as name, message and trace;
functions and DOM nodes say what they are; and an object that stringifies to
`{}` reports its constructor rather than an empty shape.

---

### 8.13 Insertion was not moving nodes, which is what a keyed diff is made of

preactjs.com rendered 178 lines without script and 65 with it, with no errors
and nothing on the unsupported list — its shell and its sidebar, and nothing
where the article should be. Four things had to be ruled out before the cause
showed itself: the content JSON arrived (35 KB, 200), `DOMParser` parsed all
31 KB of it correctly (557 elements, 108 body children), the page settled rather
than being cut off, and the walk a markup renderer performs over a parsed tree
worked exactly as it should.

The bug was one line below all of that. **Inserting a node that already had a
parent lost it:**

```
built                    ABC   (3 children)
insertBefore(C, A)       AB    (2)   <- C gone
insertBefore(A, B)       B     (1)   <- A gone
```

The DOM defines insertion as removing the node from its old parent first. This
engine skipped that, and the tree underneath drops a node inserted while still
parented — so every *move* was a deletion. That is the operation a keyed diff is
built out of: preact reorders by re-inserting nodes it already holds, and each
reorder threw one away until the article was gone.

Detaching first fixes it, and preactjs.com now reads **178 lines with script,
matching its prerendered reading exactly**.

Two things worth keeping from how it was found. The failure was invisible to
every instrument in this project — no error, no unnamed API, no anonymous
console line — because nothing was *wrong* from the page's point of view; it
asked for a move and got a deletion. And the fixture harness had been running
every page's scripts twice, since `PageFactory::from_html` already runs them:
harmless for a script that assigns, wrong for one that appends. Both were found
by writing a test that appends.

---

### 8.14 Shadow DOM, flattened — and where the interrupt actually is

**Shadow DOM is built**, after two sites asked for `Element.shadowRoot` once the
performance work let them run far enough to want it. That is the rule this file
keeps: nothing is built until a page asks, and lit.dev and material-web asked.

This engine has one tree and blitz has no notion of a shadow one, so a shadow
root is a **view of the host element** and everything a component renders into
it lands in the host. The trade is stated rather than discovered:

* **Kept**: the content renders and is therefore readable, `host` and `mode`
  answer, `nodeType` is 11, a closed root is not handed out, and light children
  are projected into a `<slot>` if the component declares one — otherwise held
  aside, because a browser stops rendering them and showing a component's input
  beside its output would be worse than showing neither.
* **Lost**: encapsulation. `document.querySelector` reaches inside a shadow root
  here and would not in a browser, and styles do not scope.

That is the same flattening a browser's own accessibility tree performs, and for
an engine whose product is a readable account of a page it is the right half to
keep.

**The interrupt exists, and not where it is needed.** §8.11 recorded that Boa
exposes no way to stop a running evaluation. That was wrong:
`Script::evaluate_async_with_budget` is public, and the VM yields to the caller
every N instructions — a real interrupt, for classic scripts. `Module` has only
`evaluate()`, with no budgeted variant, and lit.dev is modules end to end. So
the mechanism is there, the upstream ask has a precise shape —
`Module::evaluate_async_with_budget` — and until it exists a module graph is
still one uninterruptible unit.

---

### 8.15 A review pass: what it found in its own work

Going back over what had been built, rather than forward.

**Our own accessors were paying the reporting trap twice.** A getter invoked
with the proxy as `this` pays another trap for every `this._id` it reads, so
each accessor cost two. Passing the raw target as the receiver:

```
nodeType     2.15 -> 0.85 µs      tagName      1.80 -> 0.95 µs
parentNode   2.75 -> 1.55 µs      children    10.45 -> 7.75 µs
```

What it narrows is stated where the code is: a getter *defined by the page* on
its own class now runs with the target as `this`, so an unknown property read
inside one is not reported. Methods are unaffected, and the reporting that has
found real bugs has always been about properties a page reads *off* a node.

Two smaller ones on the same path: a node's kind is fixed when it is created and
was being asked of the tree on every `nodeType` read, and the document node's id
is constant and was being re-derived on every step of every upward walk.

**The ask list was being buried by generated keys.** jQuery and Sizzle stamp
elements with names like `jQuery360062973586668224961` and
`sizzle1786301869537` and read them before writing them; one corpus page
produced **5265 such "gaps"** and put them at the top of the list. No web
platform property carries a six-digit run, because it would have to be typed by
a person — so those are filtered, alongside the `_` and `$` prefixes already
filtered for the same reason.

**Where the application corpus stands after all of it:** 20/20 load, 17 usable
outlines, 2 render materially more with script, **0 render less**, 0 anonymous
errors, and **1 site** that cannot be read with script at all — lit.dev, whose
module graph is the one uninterruptible unit left (§8.14).

---

### 8.16 The "cosmetic" duplication was text nodes being immutable

preactjs.com rendered its version as `v11.0.0-beta.111.0.0-beta.1`. It looked
cosmetic and was filed that way. It was not.

Reproduced with real preact against the page's actual markup — a single text
node `v1.0.0` hydrated against a vnode with two text children, which is what a
prerendered page gives a component that renders `v{version}`:

```
before   kids=1  text="v1.0.0"
after    kids=2  datas=["v1.0.0", "1.0.0"]      <- ours
after    kids=2  datas=["v", "1.0.0"]           <- a browser
```

Preact assigns `dom.data = 'v'` to the node it is reusing. **That write did
nothing**, because writing to a text node took the path meant for elements:
clear the children — a text node has none — and append a new text child, which
is meaningless. Blitz has `set_node_text` for exactly this and it was never
called.

So text nodes were immutable, and that is the single most common mutation any
reactive UI performs: every framework updates text by assigning `.data` or
`.nodeValue` to a node it already holds. The duplication was one visible symptom
of a general failure to apply text updates at all.

preactjs.com now reads **178 lines with script, matching its prerendered
reading**, and shows `v11.0.0-beta.2` — the version it *fetched*, where before it
showed the stale prerendered `beta.1` twice. The update applies now.

Worth noting how it was found: not by reading the DOM code, but by reproducing
the page's exact shape against the real library and comparing what each engine
ends up with. The bug was three layers below where it showed.

---

### 8.17 Measured against Chromium

`corpus/compare.py`, on this machine, both engines asked to do the same job:
fetch a page, run its script, produce a readable serialisation. Peak resident
memory is sampled across the **whole process tree**, because Chromium is
multi-process and measuring only the process we launched would flatter this
engine by several hundred megabytes for nothing.

```
page                    h5i                 chromium
documentation page       59 MiB   0.6s       513 MiB   0.8s
reference page           76 MiB   1.2s       563 MiB   0.4s
wiki article             73 MiB   0.5s       585 MiB   0.6s
news front page          56 MiB   0.9s       537 MiB   0.7s
single-page app          77 MiB   0.4s       541 MiB   0.4s
framework docs site      77 MiB   1.3s       580 MiB   1.0s

median peak RSS          76 MiB              563 MiB      7.4x less
median wall               0.9s                 0.7s       ~30% slower
install size           34 MiB               302 MiB      8.9x smaller
processes per page          1                    7
```

**What these numbers are, and are not.**

They are honest about the trade: this engine holds a page in about a seventh of
the memory, in one process rather than seven, from a binary a ninth the size —
and it is *slower*, because Chromium has a JIT and this has an interpreter.
Anyone quoting the memory figure without the speed one is quoting half a
measurement.

They are also not a claim of equivalence, and the corpus in §8.6 is the reason:
of twenty applications, this engine reads seventeen usefully and **one not at
all**. Chromium reads all twenty. The right sentence is "a seventh of the memory
for the pages it can read", and the second half of that is doing real work.

The comparison deliberately records what each run actually *read*, so a run that
produced nothing cannot appear as a fast, small success. The counts are not
comparable to each other — ours is a summarised outline capped at 300 lines,
Chromium's is a raw DOM dump — and they are there to prove each engine did the
work, not to be divided by one another.

Worth stating for anyone reaching for these in a comparison: this is one page
per process, which is how an agent reads. A long-lived Chromium amortises its
browser and GPU processes across many tabs and would look better per page.

---

### 8.18 Two more corpora, and the crash they found

Two writing systems' worth of blind spot, and a shape of page neither corpus
contained.

**International** — fourteen pages in CJK, Arabic, Hebrew, Persian, Thai,
Devanagari, Greek, Cyrillic and Vietnamese. Text shaping, bidi and CJK line
breaking all run through parley, and every page measured until now was Latin: in
an engine whose entire product is extracted text, none of it had ever been
exercised. **14/14 load, 14 usable outlines, zero errors, zero anonymous
errors**, and the extracted text is correct — checked character by character
rather than by line count, because a corpus that counts lines would happily
report three hundred lines of mojibake.

**Structures** — big tables, forms, search results, plain RFCs, and markup old
enough to predate the conventions the rest of the web settled on. This one paid
immediately.

**The GNU bash manual crashed the engine.** One megabyte of single-page HTML,
and blitz panics with `attempt to subtract with overflow` in layout
construction. A panic is the one outcome an agent cannot act on: not a thin
page, not an error it can read, but a dead process and no answer at all.

Layout now runs behind a guard. The panic is caught, the document is read in
whatever state layout reached, and the snapshot says so — the page returns **500
lines and a note** where it used to return a stack trace and an exit code. The
first failure is kept rather than the last, because a later pass that happens to
survive does not undo the fact that the tree was laid out incompletely.

`AssertUnwindSafe` is the honest part of that: the document is behind a
`RefCell` a panic may leave mid-update, and reading a possibly-incomplete tree is
exactly the risk being taken in exchange for not having a dead process.

Also found and not yet built: `document.write` (caniuse), `CSSStyleSheet`,
`document.respec` (W3C specs). And pypi's search page is a JavaScript-detection
interstitial the challenge matcher does not recognise, which is a gap in the
matcher rather than in the engine.

---

### 8.19 Two of the three were worth building; one was not an API

`document.write`, `CSSStyleSheet` and `document.respec` came out of the
structures corpus. Checking each before building it turned out to matter.

**`document.respec` is not a web API.** The W3C pages call
`document.respec.ready.then(...)` — it is ReSpec's own global, a page expando in
the same class as Solid's `_$DX_DELEGATE`, and implementing it would have been
implementing someone's variable name. It stays reported, and the ask list
carrying it is the cost of a filter that cannot know every library's field.

**`document.write` is emulated where it can be and refused where it cannot.** A
browser inserts at the parser's position; this engine parses the whole document
before running anything, so that position does not exist — but `currentScript`
does, and inserting after it is the same place for the one deliberate use:
caniuse.com writes `<style>.static-only{display:none}</style>` from an inline
script. Called with no script running, a browser would implicitly `open()` and
**wipe the page**; that is refused by name instead, because the call would have
been harmless during parsing and the difference is this engine's script timing
rather than the page's intent.

**`CSSStyleSheet` is backed by a real `<style>` element**, so an adopted sheet's
rules reach Stylo rather than being remembered and ignored. `cssRules` is
deliberately left undefined: this engine does not model rules individually, and
answering an empty list for a sheet that plainly has rules is the confident
wrong answer it keeps having to refuse.

**And a bigger thing fell out of testing them.** The written
`<style>display:none</style>` did not hide anything — because **the outline does
not filter hidden content at all**. `display: none`, `visibility: hidden` and
the `hidden` attribute all appear in the reading:

```
paragraph 'visible'
paragraph 'display none'          <- a user cannot see this
paragraph 'visibility hidden'     <- nor this
paragraph 'hidden attribute'      <- nor this
```

That is a fidelity problem and a safety one. This engine's product is a faithful
account of what a page shows, and text a user cannot see is the classic vehicle
for instructions aimed at whatever is reading — the fence in §1 exists for
exactly that threat and this walks around it. It is the next thing to fix, and
it deserves care rather than a quick filter: content revealed later by script,
and the difference between `display: none` and off-screen accessibility text,
both decide whether a filter helps or quietly deletes the page.

---

### 8.20 Driving a page, and the sentence that contradicted itself

**Every corpus until now loaded a page and read it. None clicked anything.** An
agent's loop is read, act, read the difference — so two thirds of what this
engine is for went unmeasured, while the session verbs, the semantic delta and
the action-to-request correlation were all built and tested only in isolation.

`tests/corpus.rs` now drives as well as reads. Four fixtures, each asserting on
what the *delta* reports rather than on the page, because a change nobody can
see is the same as no change:

* typing into a field and submitting adds an item — and the delta names the new
  item without reporting the rest of the page as replaced;
* clicking a filter that rewrites a list reports the items that went and **not**
  the footer that did not;
* clicking something inert reports *no change*, which is a result an agent needs
  rather than the page handed back to be re-read;
* a router click moves the view and the address together, while the document's
  own URL stays put — the router moved, not the fetch.

They pass, which is worth stating plainly: the interaction path works, and it
had never been measured end to end.

**And `<noscript>` was in the outline.** A browser shows that content only when
script is off; this engine showed it always. So a page whose script ran
perfectly still handed an agent the sentence *"JavaScript is disabled in your
browser"* — not a cosmetic slip but a direct contradiction of the reading it
appeared in. crates.io's **entire outline was that sentence**.

crates.io now reports zero lines and a note saying so, which is the honest
answer: its SvelteKit app really does render nothing here. Why it does remains
undiagnosed — the entry shape reproduces perfectly in isolation, dynamic
`import()`, `currentScript.parentElement` and all 75 subresources check out
individually — and it is better recorded as unexplained than as fixed.

pypi's search page joins the challenge matcher, which also normalises
typographic apostrophes: pypi writes "couldn't" with U+2019, and a matcher that
only knew `'` would have missed it while looking like it had checked.

---

### 8.21 Hidden content is no longer read, and Chromium settled the argument

The outline carried `display: none` content, the `hidden` attribute, and
`visibility: hidden`. Two problems, and the second is the serious one: the
outline claims to be an account of what a page *shows*, and invisible text is the
classic vehicle for instructions aimed at whatever is reading it — the threat the
untrusted-content fence exists for, walked around by text a human never meets.

`display: none` and `hidden` are filtered now, asked of the style engine rather
than re-derived: a node with no primary styles is not rendered, and a node with
styles can still resolve to `display: none`, which is the common case because it
is what a stylesheet says. The first attempt checked only the former and filtered
the attribute while missing every CSS rule — the difference between the two took
a probe to find.

**`visibility: hidden` is deliberately kept.** That content occupies its space,
is routinely toggled by script, and is a shape off-screen accessibility text
sometimes takes; filtering it would risk deleting page content to fix a smaller
problem.

**The measurement then produced an alarming number, and it was right.** The Rust
book fell from 171 lines to **6**. That is the failure mode this change was
warned against — silently deleting a page — so it was checked against Chromium
rather than reasoned about: Chromium's DOM for the same page carries
`<html class="js light">` and **no `sidebar-visible` class**, so mdBook's sidebar
is not shown there either.

The six lines are the chapter: its heading, its opening paragraph, its list. The
165 that went were navigation **no reader ever sees**, and this engine had been
handing them to agents as page content. A number that looks like a regression is
worth checking against a browser before it is treated as one — and worth checking
before it is treated as a success, which is the same discipline pointing the
other way.

---

## 10. What is next, 2026-08-09

> **Superseded in part by §11.** This section is the queue as it stood before
> Kitesurf was re-read against a built engine; §11.5 is the current one. Kept
> because items 1 and 2 record how they were closed, and because item 4 is a
> useful example of the rule working: Shadow DOM was listed here as "if and when
> a page asks", a page asked, and §8.14 built it.

Tiers 0 through 4 of the plan this section replaces are done. What the work
itself surfaced, in the order the evidence supports:

1. ~~The fourteen module failures~~ — **four left** (§8.10), each with a stack
   trace. Two are the Boa parser bug of §8.11 and are upstream's to fix.
2. ~~Boa 0.21~~ — **done**, pinned by revision on the dependency itself rather
   than through `[patch.crates-io]`: `=1.0.0-dev` *looked* like a pin and pinned
   nothing, since upstream's `main` carries that version string while changing
   daily. The commit hash now sits in the manifest of the crate that depends on
   it, where a reader looks for it, and nothing else in the workspace depends on
   boa so the patch indirection bought nothing (§8.10).
   The pin should move to a release when boa cuts one, and the `[patch]` block
   deleted then. That is no longer a thing to remember:
   `scripts/check_boa_release.sh` asks crates.io on every CI run whether a
   published boa's icu requirements have stopped clashing with blitz's parley,
   and fails the build the day one has. It reads parley's requirement from the
   lockfile rather than assuming it, so it stays true when blitz moves, and it
   has a floor at 0.21 — the first version with source positions — because
   older releases predate the icu dependency and so "do not clash" while being
   unusable. The first draft recommended 0.17 for exactly that reason.
3. **Two sites that now time out**, lit.dev and material-web, because they get
   further than they used to. Either the engine gets faster or the corpus learns
   to report a partial render as a result rather than a failure.
3. **The realm costs ~20ms to start** and is rebuilt per page. A resident
   session that reuses one realm across navigations would remove it from every
   step after the first. Measured, not guessed — see §8.9.
4. **Shadow DOM**, if and when a page in §8 asks. Two design-system sites in the
   application corpus use it and neither asked, because their docs pages are
   server-rendered. Adding it now would be building for a page we have not met.
5. **A corpus that needs a login.** Everything measured so far is public, so
   LOGIN mode and the cookie jar are tested but not *exercised* against a real
   session-gated application. That is the next honest extension of §8, and the
   one most likely to find something surprising.

The rule that produced everything above stays: **nothing is built until a page
asks for it, and an instrument that cannot name what is missing is fixed before
anything it failed to name.**

---

## 11. Kitesurf, re-read against a built engine, 2026-08-09

`ROADMAP.md` §7.1 surveyed Kitesurf on 2026-08-07 and drew the routing rule from
it: two engines, by origin, one policy. That section remains the authority on
*position*. This one is narrower and later. The engine now exists, so the
question is no longer "what does this mean for scope" but **"what does the
comparison change about the order of work"**, which is what this file is for.

### 11.1 The stack is less shared than it looks

Read casually, Kitesurf is this engine with a Cloudflare account attached: Blitz
for HTML and layout, Stylo for CSS, Parley for text shaping, Rust throughout.
The JS is the exception and it is the important one. **Page script runs on V8**,
because a Worker already is V8; Boa appears only for `eval`, as a stand-in until
Workers exposes dynamic evaluation natively. `ROADMAP.md`:2010 recorded this and
it stands.

Three things follow, and the first two are corrections to a comparison that is
tempting to make and wrong:

* **The wall-time figures are not comparable.** Kitesurf reports 1.7-1.8x slower
  than Chromium; §8.17 measured this engine at roughly 1.3x. That is not a win.
  Theirs includes an isolate boundary and a WASM-compiled DOM; ours includes an
  interpreter where theirs has a JIT. Different corpora, different hardware,
  different bottlenecks. Neither number bounds the other and neither should be
  quoted against the other.
* **Boa still carries no precedent.** The hope that Kitesurf's success validated
  Boa for real web applications does not survive reading what Kitesurf runs
  script on. It does not use Boa for that. This engine is the precedent, which
  means §8's corpus is not a nice-to-have measurement, it is the only evidence
  that exists. The swap trigger at `ROADMAP.md`:2013 is unchanged.
* **Memory is the comparison that survives.** Kitesurf reports 4.7-7.0x less
  than Chromium; §8.17 measured 7.4x. Those are close, measured the same way,
  and both are large. This is the number to state.

### 11.2 What the comparison does not change

Three of Kitesurf's stated gaps are already answered here and should not be
re-opened as work:

* **Video and WebGL.** Not in scope for the light engine, and not a gap, because
  a coding agent testing a video player is testing its own application, which is
  loopback, which routes to Chromium. Kitesurf must name these because it has no
  Chromium half. We do (§7.1).
* **Persistent authenticated sessions.** Kitesurf cannot have them; this is the
  one place where "there is a human at this machine" is a capability and not a
  limitation. `session login` hands the page to the person at the viewer and
  takes it back (§5.4, §8.20). Answered, though see 11.6.
* **Speed.** Never the claim, for the reason at the top of this file: shipping
  less browser beats any benchmark table, so a benchmark table is not a moat.

### 11.3 What it does change: two gaps, and one advantage never stated

**Gap 1: CDP.** The ecosystem converged on the Chrome DevTools Protocol, and
Kitesurf speaks it, which means everything already written against Playwright,
Puppeteer and `chrome-remote-interface` works there and not here. This engine
has a bespoke JSON control channel that nothing else targets. The session state
that CDP would need already exists behind `serve`; what is missing is the wire
format and an honest account of the subset.

**Gap 2: conformance.** Kitesurf can say 215,000+ Web Platform Tests. This
engine can say seventy pages across four corpora. The corpora have been worth
every hour spent on them and they found things WPT never would, because they are
real pages. But they cannot answer "what fraction of the platform is
implemented", and that is the question every capability decision below depends
on. **This is an instrument gap before it is a capability gap**, which by this
file's own rule puts it ahead of the capabilities it would measure.

**The advantage: reach.** A cloud browser cannot open `localhost:3000`, a
staging host, an internal admin panel, or anything behind a VPN. For a *coding*
agent that is not an edge case, it is a large share of everything it needs to
look at. This has never been written down as a property of the design, and it is
a stronger and more concrete statement than "local-first" or "private": it is
not that we decline to send the page elsewhere, it is that for these pages there
is nowhere to send it from. It belongs beside receipts in how this engine is
described.

### 11.4 MCP: decided against, 2026-08-09

Kitesurf ships an MCP server and this engine will not, because the two are
answering different questions. MCP exists to give an agent a tool surface across
a process boundary it cannot cross. **Here there is no such boundary**: the
agent runs on this machine, in the same box as the engine, and
`h5i-browser-light session snapshot` is already a tool it can call. A protocol
server would wrap the CLI in a socket so that the thing on the other end could
call the CLI.

The condition that would reopen this is specific: an agent that must drive this
engine **without being able to run a subprocess**. If one appears, MCP is the
right answer for it and this decision was still right until then.

Note that CDP (11.3) is not the same call and does not fall to the same
argument. MCP would re-expose verbs the CLI already exposes to a caller that can
already call them; CDP would let a large body of *existing* software drive this
engine, none of which is going to be rewritten against our CLI.

### 11.5 The queue

Ordered by what the evidence supports, not by size.

**First, because it is the least-verified thing we claim.**

1. **A corpus that needs a login.** Unchanged from §10.5 and now more urgent, not
   less: 11.2 names authenticated sessions as an answered gap, and it is answered
   by a mechanism that has never been exercised against a real session-gated
   application. The strongest claim in this file rests on the least-tested code
   in it.

**Second, because everything after it is better informed.**

2. **Run the Web Platform Tests.** Start where the corpus already lives:
   `dom/`, `html/dom/`, `css/cssom/`. Needs a `testharness.js` driver, a
   committed baseline, and a CI gate on regression rather than on an absolute
   number.
3. **Publish the number, whatever it is.** A measured forty thousand is worth
   more than an unmeasured claim, and an engine that names what it cannot do
   (§8.3) does not get to make an exception for its own conformance.

**Third, the interoperability work, sized once 2 has told us what we can claim.**

4. **A CDP subset over WebSocket.** The useful floor: `Target` attach/create,
   `Page.navigate|captureScreenshot|loadEventFired`,
   `Runtime.evaluate|callFunctionOn|consoleAPICalled`,
   `DOM.getDocument|querySelector|getBoxModel`,
   `Input.dispatchMouseEvent|dispatchKeyEvent`, `Network` request/response
   events plus cookie get and set, `Emulation.setDeviceMetricsOverride`.
5. **The unimplemented half of CDP must be loud.** A partial protocol that
   answers to the name of the whole one is the `missingApi` lie at protocol
   scale (§8.4): Playwright will call methods we do not have, and a silent or
   plausible answer there is worse than an error, for exactly the reason a
   plausible wrong answer is worse than no answer anywhere else in this engine.
   An unimplemented method returns a named error and the conformance list is
   published.
6. **REST quick actions**: screenshot, extract, PDF. Nearly free once 4 exists.

**Fourth, the gaps the corpus itself found.** These are §8's list and are ordered
by how many pages asked.

7. Boa `Module::evaluate_async_with_budget` (lit.dev evaluates unbounded, §8.14),
   the Boa `let`/`const` parser bug (§8.11), the blitz layout panic (§8.18). All
   three are upstream's and all three are filed.
8. **Canvas 2D**, the largest single missing API by corpus demand.
9. **WebSocket and EventSource.** A live application shows nothing without them.
10. **IndexedDB**, in memory only, consistent with §6's storage line.
11. **`getComputedStyle` answers almost nothing** (`color` came back empty). It
    is implemented far enough to look implemented, which §8.3 established is the
    worst state for anything in this engine to be in.
12. **crates.io renders nothing** and the cause is still unknown. SvelteKit-
    shaped; the entry path was verified working in isolation, so the failure is
    somewhere the isolation removed.

**Fifth, performance, none of which is urgent.**

13. **Reuse the realm across navigations.** ~20ms per page, rebuilt every time,
    measured in §8.9.
14. **Cache the prelude's bytecode.** Three thousand lines of JavaScript parsed
    per realm.
15. There is no JIT and there will not be one. The cost is stated in 11.1 and
    the answer to it is 11.3's reach and §8.17's memory, not a faster
    interpreter.

**Sixth, the moat, which is mostly already built and under-described.**

16. **Receipts as a checkable artifact.** The one thing Kitesurf's announcement
    does not address at all. Today the guarantee is "no receipt, no request" and
    it is true; what it is not is *verifiable by someone who does not trust the
    binary that wrote it*.
17. **Measure and state the delta snapshot** (§8.20). No comparable engine
    appears to have one, and re-reading three hundred lines after every click is
    the shape everyone else's agent loop is stuck in.

### 11.6 Two conflicts to settle deliberately

Both are cases where §6's "never" list collides with something 11.5 wants. Each
should be decided in writing rather than discovered in a corpus run.

**Login flows use iframes and popups; §6 refuses both.** The strongest claim in
11.2 is persistent authenticated sessions, and real-world OAuth is an iframe or
a popup almost every time. §5.4's human handoff sidesteps part of this, because
a person at the viewer can complete a flow the engine could not drive, but it
does not help when the flow needs a second browsing context to *exist* at all.
Either §6 gains a narrow, argued exception for authentication boundaries, or the
login claim is honestly scoped down to form posts. It cannot stay as it is: item
11.5.1 will decide this whether or not it is decided first, and it is better
written down in advance.

**PDF.** §6 refuses "printing", by which it meant the print UI, and item 11.5.6
wants `printToPDF`. These are not the same feature: one is chrome around a page,
the other is a serialisation of it, and an agent asked to keep a record of what
it read wants the second. Recommended as an exception, on the grounds that the
raster path (`blitz-paint`, vello_cpu) already produces everything it needs.
