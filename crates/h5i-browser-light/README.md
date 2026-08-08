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
h5i-browser-light open <url|path> [--allow ORIGIN]... [--screenshot PATH]
                                  [--receipts PATH] [--text] [--json]
h5i-browser-light capabilities     # what this engine can do, as JSON
h5i-browser-light doctor           # fonts, proxy, allowlist, client
```

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

Tier 1 of ROADMAP M10: static render, snapshot, screenshot, receipts. Tier 2
(live view via the CDP screencast domain, so the existing viewer stack follows
this engine unchanged) and Tier 3 (policy-gated script) are not built.
