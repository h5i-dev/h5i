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

### 3.1 The live hole, introduced 2026-08-09

`Policy::check(&self, url: &Url)` takes **only a URL** — no origin, no
initiator — and loopback is allowed unconditionally by default, deliberately,
because the box's dev server is the point.

Before script, an untrusted page could *cause* a loopback request
(`<img src="http://127.0.0.1:3000/admin">`) but could not **read the response**.
With `--script` it can `fetch` loopback, read the body, and POST it to anything
in `net.egress`. That is a read primitive against the code the agent is working
on, and loopback explicitly bypasses the egress proxy, so the box's outer
enforcement never sees it.

**Fix, and it is the next thing to build:** loopback is reachable *from a
loopback document*. A page served by the dev server may talk to the dev server;
a page from the open web may not. That requires `check` to take the document
origin, which it does not today.

Worth stating alongside the pure-Rust claim: this is a **logic** bug, and Rust
prevents none of them. "Fewer memory bugs" is honest; "safer browser" is earned
by the origin model, not the language.

### 3.2 Site isolation is the one thing the box does not replace

Chromium's process model exists to contain a compromised renderer: filesystem,
network privilege, crash isolation, and cross-origin theft. The box covers the
first three at a stronger boundary than a renderer sandbox. It does not cover
the fourth — it protects the host from the box and says nothing about two
origins sharing one address space.

That did not matter while the engine held nothing worth stealing. The cookie jar
shipped on 2026-08-08 and script on 2026-08-09, so it matters now. Blitz and
Stylo being Rust is the current mitigation. One of these must be chosen before
this is called production-grade: one origin per session, clearing the jar across
origins, or keeping the jar out of the process that runs script.

### 3.3 The gate, still honoured

`capabilities.javascript` reports the *running* configuration; script is opt-in;
with it off, `<script>` elements are inert exactly as before. Nothing has
flipped by default and nothing should until 3.1 and 3.2 are answered. See
`ROADMAP.md` §12.5.

---

## 4. Fix first: three things that are wrong, not missing

"Missing" is honest and reports itself. These are worse: they corrupt a page
while looking like they worked, which is the failure mode the fence and the
unsupported-API log exist to prevent. They also pollute any measurement taken
before they are fixed.

1. **`innerHTML` getter returns `textContent`.** All markup is stripped, so
   `el.innerHTML = el.innerHTML` destroys the subtree.
2. **`createDocumentFragment()` returns a `<div>`.** Appending a fragment injects
   a real element that should not exist, breaking `.parent > .child` and layout.
3. **`Element.style` does not exist**, so `el.style.display = 'none'` throws.
   Loud rather than silent, but it kills the script at that line, and inline
   style is everywhere.

---

## 5. The bindings backlog

Ordered by what blocks real applications first. Cross-referenced against
Thalora's surface (§7) where that project has already mapped the ground, and
marked **cheap** where Blitz or Stylo already holds the answer and we are merely
refusing to give it.

### Tier A — blocks nearly everything modern

| | why | note |
| --- | --- | --- |
| **ES modules and `import()`** | every production bundle ships `<script type="module">`; today they do not execute at all | Boa has a `ModuleLoader`; read Thalora's for shape but **not for policy** — see §7.2 |
| **`Element.style` (CSSOM)** | `el.style.display = 'none'` is ubiquitous | Thalora: `browser/cssom.rs` |
| **`getBoundingClientRect`** | every popover, dropdown, drag and virtual list | **cheap** — Blitz computes `final_layout` already |
| **`getComputedStyle`** | feature detection and measurement | **cheap** — Stylo has it |
| **`MutationObserver`** | frameworks depend on it; and it is the natural source for our own semantic delta | Thalora: `observers/mutation_observer.rs` |
| ~~`IntersectionObserver`, `ResizeObserver`~~ | lazy loading, virtual lists, responsive components | **built 2026-08-09**, driven from the settle loop (§8.2) |
| **`localStorage` / `sessionStorage`** | in-memory maps; absence throws or breaks init paths | deliberately non-persistent, see §6 |
| **`history.pushState`** | SPA routing; without it client-side navigation silently does nothing | Thalora: `browser/history.rs` |

### Tier B — blocks a large fraction of real applications

* Real event types: `MouseEvent`, `KeyboardEvent`, `InputEvent`, `CustomEvent`
  with `detail`, plus `key`, `clientX/Y`. Thalora: `events/` is 7.6k lines and
  the best single map of what is needed here.
* Form semantics: `input`/`change` on typing, checkbox, radio, `select`,
  `FormData`. Thalora: `misc/form_data.rs`, `misc/form.rs`.
* `closest()`, `matches()`, `dataset`, `cloneNode`, `insertAdjacentHTML`,
  a real `DOMTokenList`. Thalora: `dom/domtokenlist`, `dom/element.rs`.
* `AbortController`, `Headers`, `Request`, and concurrent `fetch`. Ours is
  synchronous underneath, so two requests run in order rather than at once.
* `window.scrollTo` and scroll events.

### Tier C — the tail

Canvas 2D, WebSocket, Workers, **WebAssembly**, Shadow DOM and custom elements
(design systems use them), SVG DOM, Streams, `TextEncoder`/`TextDecoder`,
`structuredClone`, `crypto.getRandomValues`.

None of these is scheduled. Each is added when the corpus in §8 says a real page
needed it, not before.

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

## 9. What "production-grade" means here

Not "80% of the web". That number splits by an order of magnitude — reading
server-rendered pages is close to solved and mostly does not need script at all;
driving interactive applications is Kitesurf's 215,000 web platform tests, which
is quarters of work and a different product. The bar for this engine is narrower
and checkable:

1. §4 fixed, so nothing corrupts a page silently.
2. §3.1 fixed, so an untrusted page cannot read the box's dev server.
3. §3.2 answered, so cookies and script sharing an address space is a decision.
4. A production React build renders, is drivable, and reports honestly what it
   could not do.
5. Every page either works or **says which API it needed** — never a silent
   wrong answer.
6. `h5i box view`, `--term` and `h5i ui` all attach and stay attached, with the
   control lock enforced.
7. **LOGIN mode**, so a human typing a credential is not doing it on a page the
   agent can snapshot.

(7) is overdue rather than pending: it was supposed to arrive *with* cookies and
did not.

The differentiator is not on that list because it is not a browser feature:
action-to-request correlation rendered in `h5i ui`, and a semantic delta instead
of a full outline every step. The engine now stamps the causal link; the console
does not yet draw it. That is the work that makes this h5i's browser rather than
a small browser that happens to run here.
