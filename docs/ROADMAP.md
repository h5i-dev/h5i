# ROADMAP

Status: in progress, 2026-10-19. This file is the scope authority: what h5i is,
what is built, and what is deliberately not. It is meant to be read in one
sitting. The design behind each part lives in `docs/`, one file per part.

> h5i unifies web development, formal verification with Lean 4, and
> red-teaming around a composable Rust framework. Build. Verify. Hack.

These three parts work together:

- **Build** with h5i-app, an Axum-based Rust framework that keeps critical
  application logic in composable kernels.
- **Formal verification** extracts that kernel logic to Lean 4 and proves
  stated properties, from tenant isolation and authorization to the app's own
  state invariants. Its scope and assumption ledger live in
  [`docs/design/design-app-roadmap.md`](design/design-app-roadmap.md) and
  [`docs/design/design-app-trust.md`](design/design-app-trust.md); `h5i app` is the CLI over it.
- **Red-teaming** works on any stack. An agent drives Chromium (through
  agent-browser) and works the HTTP traffic directly, inside limits you set.
  The browser goes through a capture proxy h5i runs for the session: every
  request is policy-checked and written down before the bytes move, and it is
  refused when the record cannot be written. Playwright and Puppeteer drive a
  browser and cannot tell you what it reached; h5i owns the path every byte
  takes. That is the piece with no equivalent elsewhere.

On 2026-10-19 h5i's own browser engine was removed. Benchmarks showed the proxy
lane finds what the engine found, and a real Chromium renders what the engine
could not. What made the engine worth having was its network half (the broker,
the receipt lanes, the message store, resend), and that is kept, now under the
capture proxy. The egress proxy, the control lock and the box tiers are
unchanged.

## Where it stands

| part | status | design |
| --- | --- | --- |
| browser sessions | Chromium via agent-browser, captured by a per-session proxy (`proxy-observed`); h5i's own engine removed 2026-10-19 | [`docs/MANUAL.md`](MANUAL.md#h5i-browser); the engine's design, superseded, in [`docs/design/design-browser.md`](design/design-browser.md) |
| in-terminal viewer | shipped, V1 to V8, for box browsers. Keyboard-driven, in the terminal or a browser | [`docs/design/design-interminal-browser.md`](design/design-interminal-browser.md) |
| policy resolution | P1 shipped; P2 shipped, opt-in; P3 and P4 designed, not built | [`docs/design/design-policy.md`](design/design-policy.md) |
| HTTP workbench | phases A and B built and benchmarked, plus experiments and findings (W22, W23); installed with `h5i plugin install websec` | [`docs/design/design-websec.md`](design/design-websec.md) |
| box console | sessions and the attention model built 2026-09-07 | [`docs/design/design-console.md`](design/design-console.md) |
| reconnaissance | phase 1 built 2026-09-07: ledger, extract, known, crawl, paths, triage, jobs. `h5i plugin install recon` | [`docs/design/design-recon.md`](design/design-recon.md) |
| security regression tests | MVP built 2026-09-13: portable flows, external oracles, JSON/JUnit, OpenAPI coverage and an optional gate. `h5i plugin install test` | [`docs/design/design-test.md`](design/design-test.md) |
| h5i-app | 0.1 published; `h5i app` new, extract, check, prove, lint, mutate, doctor. | [`docs/design/design-app.md`](design/design-app.md), [`docs/design/design-app-roadmap.md`](design/design-app-roadmap.md) |
| framework-proven authorization (h5i-app) | built 2026-10-26: the guard and its theorem in h5i-app-std, `[authz]` in `h5i app`, the forum example; a Cedar front end is not built | [`docs/design/design-app-authz.md`](design/design-app-authz.md) |

## The three decisions the red-teaming workflow rests on

1. The sandbox is opt-in. `h5i browser open` runs Chromium on the host like any
   other headless browser and says so on the placement line. Requiring a box up
   front would fail hello-world on CI, under AppArmor, on macOS and in a
   container, for nothing the record does not already give.
2. The box stays a separate, orthogonal, agent-facing surface, not the browser's
   implementation detail. A box's `browser` profile confines its own Chromium.
3. The lane is named, never upgraded. A session's requests are
   `proxy-observed`: h5i saw them at its proxy, and the browser itself is not
   confined. See `browser_session::Session::lane_for`.

## What is next, ranked

1. Trust the session CA instead of `--ignore-https-errors`, now that
   agent-browser has `--ca-cert`.
2. A capture session inside a box, so the browser that is captured is also the
   browser that is confined.
3. More than one session per box. Needs per-session service names and stream
   files.

## What is deliberately not built

Each of these is a decision, not a gap, and should be refused in review rather
than re-argued.

- h5i does not build a browser. The browser is Chromium; h5i owns the proxy in
  front of it, the record and the workbench (decided 2026-10-19).

## How to read the design set

Each file states how the part works now and its limits; history stays in git.
Code cites the numbered sections, so their numbers are stable.

| file | sections | covers |
| --- | --- | --- |
| [`design-browser.md`](design/design-browser.md) | | Chromium behind the capture proxy, policy, cookies, credentials |
| [`design-interminal-browser.md`](design/design-interminal-browser.md) | V1 to V8 | the viewers: keymap, hints, latency, the lock |
| [`design-policy.md`](design/design-policy.md) | P1 to P4 | resolution, the authority validator, mount realization |
| [`design-credential-proxy.md`](design/design-credential-proxy.md) | | the credential-injecting egress proxy |
| [`design-websec.md`](design/design-websec.md) | W1 to W23 | capture, replay, diff, experiments, findings, the workbench plugin |
| [`design-recon.md`](design/design-recon.md) | N1 to N21 | the endpoint ledger, discovery, triage |
| [`design-flow-and-verdict.md`](design/design-flow-and-verdict.md) | F1 to F10 | flows, verdict sources, the Nuclei importer |
| [`design-test.md`](design/design-test.md) | | portable regression tests, external oracles, coverage |
| [`design-project.md`](design/design-project.md) | P1 to P12 | projects, findings, evidence, reports |
| [`design-console.md`](design/design-console.md) | C1 to C12 | what `h5i ui` shows and masks, attention states |
| [`design-app.md`](design/design-app.md) | | h5i-app: architecture, layout, proof patterns, checks |
| [`design-app-trust.md`](design/design-app-trust.md) | A1 to A11 | what h5i-app proves and what it assumes |
| [`design-app-authz.md`](design/design-app-authz.md) | | framework-proven authorization |
| [`design-app-console.md`](design/design-app-console.md) | | reviewing an app in `h5i ui` |
| [`design-app-numbers.md`](design/design-app-numbers.md) | | proof effort, check times, performance |
| [`design-app-executors.md`](design/design-app-executors.md) | | the RAM and CUDA executors |
| [`design-app-roadmap.md`](design/design-app-roadmap.md) | | what is left for h5i-app, the assumption ledger |
