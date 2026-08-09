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

1. **A Boa parser bug**, minimally reproduced. Any line terminator between a
   declarator's initializer and the comma is rejected:

   ```js
   const a = 1
   , b = 2;        // SyntaxError in Boa; valid JavaScript
   ```

   Automatic semicolon insertion is applying where the spec forbids it — a comma
   *can* continue a `VariableDeclarationList`, so no semicolon may be inserted.
   Minified bundles that preserve `/*! @license */` comments between declarators
   hit it, which is how lit.dev fails. Not fixable here, and not worth working
   around: stripping comments from a page's own source would move every line
   number and could corrupt string literals — the plausible-wrong answer again.
2. **Two sites exceed any reasonable timeout** (lit.dev, material-web), and the
   cause is that they now get *further*. `DOMParser` unlocked execution that used
   to fail early, and removing the lying feature-detection stubs sent pages down
   polyfill paths they had previously skipped. lit.dev went from failing in
   seconds to **seven minutes** of real work.

   A wall-clock budget on the script phase was added and does not fix it: a
   module graph evaluates inside `run_jobs`, which is one call that returns when
   it returns, so the budget never gets a turn. The budget is kept because it
   does bound a page that is slow for having *many* scripts, and its comment
   says plainly what it cannot reach. Bounding the rest needs an interrupt Boa
   does not expose. **More correct and unusably slower is still a bad trade, and
   this is the clearest open problem in the engine.**
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

## 10. What is next, 2026-08-09

Tiers 0 through 4 of the plan this section replaces are done. What the work
itself surfaced, in the order the evidence supports:

1. ~~The fourteen module failures~~ — **four left** (§8.10), each with a stack
   trace. Two are the Boa parser bug of §8.11 and are upstream's to fix.
2. ~~Boa 0.21~~ — **done**, pinned to a revision of upstream `main` (§8.10).
   The pin should move to a release when boa cuts one, and the `[patch]` block
   deleted at that point.
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
