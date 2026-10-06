# ROADMAP

Status: in progress, 2026-10-05. This file is the scope authority: what h5i is,
what is built, and what is deliberately not. It is meant to be read in one
sitting. The design behind each part lives in `docs/`, one file per part, and
the two superseded positionings are kept in
[`docs/roadmap-history.md`](roadmap-history.md) because both describe
machinery that is still shipped and tested.

> h5i is an agent-native web security workspace. Build with agents, red-team
> for bugs, formally verify properties: find the vulnerabilities nobody
> anticipated, and prove the rules you can state.

The two halves are two sides of one coin.

- **Red-teaming** works on any stack. An agent drives a browser and works the
  HTTP traffic directly, inside limits you set. Every request is policy-checked
  and written down before the bytes move, and the fetch is refused when the
  record cannot be written. Playwright and Puppeteer drive a browser and cannot
  tell you what it reached, because neither *is* the HTTP client. That is the
  piece with no equivalent elsewhere.
- **Formal verification** is h5i-app, an Axum-based Rust framework whose
  application logic is extracted to Lean 4 and proven there, from tenant
  isolation and authorization to the app's own state invariants. Its scope and
  its assumption ledger live in [`docs/app/ROADMAP.md`](app/ROADMAP.md) and
  [`docs/app/TRUST.md`](app/TRUST.md); `h5i app` is the CLI over it.

Nothing was thrown away to get here: the engine, the broker, the egress proxy,
the receipt lanes, the control lock and the box tiers were all built for the
environment story and all are essential to this one.

## Where it stands

| part | status | design |
| --- | --- | --- |
| browser engine | shipped. WPT core tier 75.7%; a production React build is not cleared | [`docs/design/design-browser.md`](design/design-browser.md) |
| in-terminal viewer | shipped, V1 to V8. Keyboard-driven, in the terminal or a browser | [`docs/design/design-interminal-browser.md`](design/design-interminal-browser.md) |
| policy resolution | P1 shipped; P2 shipped, opt-in; P3 and P4 designed, not built | [`docs/design/design-policy.md`](design/design-policy.md) |
| HTTP workbench | phases A and B built and benchmarked, plus experiments and findings (W22, W23); installed with `h5i plugin install websec` | [`docs/design/design-websec.md`](design/design-websec.md) |
| box console | sessions and the attention model built 2026-09-07 | [`docs/design/design-console.md`](design/design-console.md) |
| reconnaissance | phase 1 built 2026-09-07: ledger, extract, known, crawl, paths, triage, jobs. `h5i plugin install recon` | [`docs/design/design-recon.md`](design/design-recon.md) |
| security regression tests | MVP built 2026-09-13: portable flows, external oracles, JSON/JUnit, OpenAPI coverage and an optional gate. `h5i plugin install test` | [`docs/design/design-test.md`](design/design-test.md) |
| h5i-app | 0.1 published; `h5i app` new, extract, check, prove, lint, mutate, doctor. | [`docs/app/DESIGN.md`](app/DESIGN.md), [`docs/app/ROADMAP.md`](app/ROADMAP.md) |

## The three decisions the red-teaming half rests on

1. The sandbox is opt-in. `h5i browser open` runs on the host like any other
   headless browser and says so on the placement line. Requiring a box up front
   would fail hello-world on CI, under AppArmor, on macOS and in a container,
   for nothing the record does not already give. Containment is `--in <box>`.
2. The box stays a separate, orthogonal, agent-facing surface, not the browser's
   implementation detail. `h5i box run -- h5i browser open` is ordinary
   composition and `--in` is sugar over the same placement.
3. The lane is earned. A boxed session is `host-observed` only when something
   outside the engine decides what may leave; a box that lets the browser reach
   the whole network keeps `engine-claimed`. See
   `browser_session::Session::lane_for`.

## What is next, ranked

1. A tier that both holds a session and earns `host-observed`. Supervised
   and container cannot hold a resident process, and they are also the two tiers
   that enforce an egress allowlist on Linux, so today only `microvm` does both.
   This is what makes the central claim reachable on an ordinary Linux box.
2. WPT core tier to 80%. The next ~5,000 subtests are measured and ranked in
   [`docs/design/design-browser.md`](design/design-browser.md) B1.
3. More than one session per box. Needs per-session service names and stream
   files.

## What is deliberately not built

Each of these is a decision, not a gap, and should be refused in review rather
than re-argued.

- The browser will never grow tabs, extensions, Service Workers, WebRTC, iframes
  or two dozen other surfaces. The full list, and what is simplified rather than
  absent, is B4 in
  [`docs/design/design-browser.md`](design/design-browser.md).
- No vendored engine crates, by owner decision on 2026-08-28 (B4).

## How to read the design set

Each design file opens with a one-screen summary, so you can tell from the top
of it whether you need the rest.

| prefix | file | what it covers |
| --- | --- | --- |
| B1 to B5 | `docs/design/design-browser.md` | the engine, the session surface, and what it is not |
| V1 to V8 | `docs/design/design-interminal-browser.md` | the viewers: the keymap, hints, latency, the lock |
| P1 to P4 | `docs/design/design-policy.md` | resolution, the authority validator, mount realization |
| W1 to W23 | `docs/design/design-websec.md` | capture, replay, diff, experiments, findings, and the plugin the workbench ships as |
| N1 to N21 | `docs/design/design-recon.md` | the endpoint ledger, discovery, triage, and where recon stops |
| C1 to C8 | `docs/design/design-console.md` | what `h5i ui` shows, what it refuses to, and how it says which session wants you |
| T1 | `docs/design/design-test.md` | portable attack flows, external oracles, CI results and coverage |

Live code cites these section numbers. The prefixes do not collide with
[`docs/roadmap-history.md`](roadmap-history.md), which holds the superseded
environment positioning (sections 1 to 12) and the engine's build log (B1 to
B22); a `roadmap-history.md` citation always names that file.
