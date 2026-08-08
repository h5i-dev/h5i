# h5i-browser-light

A lightweight visual browser for coding agents. Every request is policy-checked
and receipted **before** it reaches the wire.

```
$ h5i-browser-light open https://example.com/ --allow example.com --screenshot page.png

# Example Domain
url: https://example.com/

- heading1 "Example Domain"
- paragraph "This domain is for use in documentation examples…"
- paragraph "Learn more"
  - link "Learn more" [ref=e1] -> https://iana.org/domains/example

requests:
     200 GET https://example.com/ (559 bytes, 79ms)
```

## Why this exists

h5i already drives Chromium through agent-browser, and will keep doing so for
the case Chromium is best at: rendering the agent's own dev server with full
fidelity. This engine is for the other half of the loop — reading the untrusted
web — where the priorities invert and containment matters more than pixel
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

Same machine (aarch64, WSL2), median of 5 runs after a warm-up, load a
self-contained local page and write an image. Memory is the peak **summed** RSS
across the whole process tree, sampled every 10ms — `/usr/bin/time -v` reports
only the largest single process and badly undercounts a multi-process browser.

| | small page (2 KB) | docs page (39 KB) |
| --- | --- | --- |
| h5i-browser-light | **42 ms / 31 MB** | **72 ms / 33 MB** |
| chromium `headless_shell` | 339 ms / 472 MB | 356 ms / 479 MB |
| chromium (full) | 655 ms / 797 MB | 644 ms / 799 MB |

Holding a page open with a viewer attached and nothing happening:

| | idle RSS |
| --- | --- |
| h5i-browser-light (viewer attached) | **37.5 MB** |
| chromium `headless_shell` (page loaded) | 383.8 MB |

So roughly **5x faster and 15x lighter** than `headless_shell` for a one-shot
read, and **10x lighter** sitting idle. Three caveats, because the numbers are
flattering and should not be quoted without them:

1. **Cold start is included**, and Chromium's process startup dominates its
   time figure. That is the honest shape of a one-shot agent invocation, but it
   is *not* a steady-state rendering throughput comparison, and this engine
   would not win one by that margin.
2. **The pages have no JavaScript.** Chromium is carrying a JS engine it is not
   using. On a script-driven page the comparison is meaningless in the other
   direction: this engine renders nothing at all.
3. Rendering here is software, not JIT-accelerated. Complex CSS will narrow
   the time gap.

## What it is not

Honest limits, because the claims above are security claims:

- **Not a Chromium replacement.** Docs-grade pages are the compatibility bar.
  React/Vite apps, video, WebGL and authenticated sessions belong on the
  Chromium path.
- **No JavaScript.** Pages that render only via script will come back empty.
  That is a routing signal, not a bug — ask `capabilities` rather than guessing.
- **Containment claims belong to the box.** Run bare on a host there is no
  egress proxy and no receipt store, and this is just a light browser with a
  request log. The guarantees are properties of running it inside an h5i box.

## Usage

```
h5i-browser-light open  <url|path> [--allow ORIGIN]... [--screenshot PATH]
                                   [--receipts PATH] [--text] [--json]
h5i-browser-light serve <url|path> [--addr 127.0.0.1:0] [--stream-file PATH]
                                   [--control-file PATH]
h5i-browser-light session status | snapshot | navigate <url> | click <@ref>
h5i-browser-light capabilities     # what this engine can do, as JSON
h5i-browser-light doctor           # fonts, proxy, allowlist, client
```

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

The control port is advertised beside the stream port — `<name>.control` next to
`<name>.stream` — so inside a box, where h5i sets `H5I_BROWSER_STREAM_FILE`,
these verbs need no flags.

One constraint shapes all of this: **`Page` is not `Send`.** Blitz's
`BaseDocument` holds an `Arc<dyn HtmlParserProvider>` and a
`Box<dyn FontMetricsProvider>`, so there is no `Arc<Mutex<Session>>` to be had.
The page has exactly one owning thread and everything else reaches it by
channel — which is the right shape for a session with several drivers anyway,
because it leaves no interleaving to reason about.

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
with `[fence marker removed]` — the only content this engine removes, and the
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
verbs (ROADMAP §11 item 5.1). Tier 3 (policy-gated script) is not built.

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

Not yet done: **no typing and no form submission**, so an agent cannot get past
a login form; and **no cookies of any kind**, so there is no session anywhere.
Those two are what stand between this and an engine an agent can use for
anything behind a login.
