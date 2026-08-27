<h1 align="center">h5i-browser-light</h1>

<p align="center"><strong>A headless browser for agents, where every request is policy-checked and receipted before it reaches the wire.</strong></p>

<p align="center">
  <a href="https://github.com/h5i-dev/h5i/blob/main/LICENSE"><img alt="Apache-2.0" src="https://img.shields.io/github/license/h5i-dev/h5i?color=blue"></a>
  <a href="https://github.com/h5i-dev/h5i/actions/workflows/test.yaml"><img alt="tests" src="https://github.com/h5i-dev/h5i/actions/workflows/test.yaml/badge.svg"></a>
  <a href="https://github.com/h5i-dev/h5i/releases"><img alt="release" src="https://img.shields.io/github/v/release/h5i-dev/h5i?label=release"></a>
</p>

Other headless browsers watch their own network. This one **is** its network: the
engine is the HTTP client, so the request log is not an observation that can miss
something, it is the thing that decides whether bytes move at all. No receipt, no
request.

```bash
h5i browser open https://docs.rs/ --allow docs.rs

h5i browser snapshot                   # the page, with @ref handles
h5i browser extract '{"crates": ["h3 a"]}'
h5i browser requests                   # everything it fetched, and what was refused
```

**This crate is the engine, not the interface.** It used to ship as its own
binary, `h5i-browser-light`; it is linked into `h5i` now, which execs itself to
render a page. What an agent drives is `h5i browser` — the surface that knows
about session names, placement, the control lock and the audit. Everything below
describes what the engine underneath does.

## Highlights

- **The engine is the HTTP client.** [The request log](#the-request-log) is
  complete by construction, every redirect hop is its own decision, and a fetch
  that cannot be receipted does not happen.
- [**Deterministic waits.**](#waiting) A virtual clock, so two runs of a page
  answer identically and a page's own `setTimeout(1000)` costs you nothing.
- [**Refs that cannot silently go stale.**](#acting-on-a-page) A handle from an
  old reading is refused by name rather than acted on.
- [**Credentials you can use and cannot read.**](#logging-in) The model names a
  secret; the engine substitutes it on the way into the field.
- [**Structured extraction and markdown**](#reading-a-page), so an agent does not
  pay for three hundred lines of outline to find five titles.
- [**Live connections**](#live-connections) with every frame receipted, not just
  the handshake.
- [**Honest about its limits.**](#what-it-cannot-do) Missing APIs are named, not
  stubbed. Unfinished pages say so.

## Install

```bash
curl -fsSL https://h5i.dev/install.sh | sh

h5i skill install       # teach an agent to drive it
h5i __engine doctor     # fonts, proxy, allowlist
```

One binary, engine included. The script verifies the published checksum before it
installs anything.

`h5i __engine` is the engine's own CLI, hidden because it is not the interface.
Reach for it only for something `h5i browser` genuinely does not offer — a
one-shot render, the font `doctor`, a replay.

Not on crates.io, and that is not an oversight: the crate depends on `boa` by git
revision, because no published version's ICU requirements can coexist with the
ones `parley` pulls through blitz. Now that `h5i` links this crate, that applies
to `h5i` as well — a release archive is how anyone gets either.
`scripts/check_boa_release.sh` fails the build the day that stops being true.

## Quick start

`open` renders a page and exits, so two `open`s share nothing. Anything
interactive needs a resident session:

```bash
h5i browser open http://localhost:3000    # loopback needs no --allow
h5i browser snapshot
h5i browser click @e1
```

`serve` advertises itself in a per-user runtime directory, so the `session` verbs
find it with no flags. Everything remote needs an explicit `--allow`; with none,
nothing remote is reachable.

## Features

#### Reading a page

```bash
h5i browser snapshot          # outline with @ref handles, to act on
h5i browser markdown          # prose, lists, tables, to read
h5i browser extract '{"rows": [{"selector": "tr.item", "limit": 5,
  "fields": {"name": ".title", "url": {"selector": "a", "attr": "href"}}}]}'
```

An empty array is a result. If *nothing* in an `extract` schema matched you get
an error rather than an object full of nulls, because that is a mistake to
correct rather than an answer to use.

Everything a page supplied comes back inside a fence marked as data, not
instructions. That boundary is where attacker-controlled text meets a model
deciding what to do next, and a page cannot forge its way out of it.

#### The request log

```console
$ h5i browser requests
   200 GET https://docs.rs/blitz/ (12043 bytes, 84ms)
DENIED GET https://telemetry.example.com/collect
```

No other browser answers this completely. Chromium's list is an observation made
from beside the network and fails open; Obscura's CDP events are batched and
emitted after navigation finishes, so anything watching live sees a compressed,
out-of-time picture; Lightpanda has no equivalent. Here the list is the decision
record written before the bytes moved.

#### Acting on a page

```bash
h5i browser click @e2
h5i browser type @e1 "search terms"
h5i browser submit @e3
h5i browser scroll 400
```

A `@ref` belongs to the snapshot that minted it. `e1` means "the first actionable
thing in *that* reading", so if the page moves the session refuses the ref
instead of acting on whatever that number points at now:

```json
{"ok": false, "code": "stale-ref",
 "error": "`@e2` came from a snapshot this page has moved on from…"}
```

Every snapshot also returns a verified CSS selector beside each ref, for a handle
that survives a navigation. Typing and scrolling renumber nothing, so a form
still fills and submits without a re-read between steps.

#### Waiting

```bash
h5i browser wait-for --selector '#results'
h5i browser wait-for --text 'Signed in'
h5i browser wait-for-script 'document.querySelectorAll("li").length > 3'
```

Three answers, not two:

| `end` | means |
| --- | --- |
| `met` | it is there |
| `quiescent` | it is not, and the page has nothing left to run, so waiting cannot change it |
| `budget` | it is not, and the page was still working, so it may yet appear |

The middle one is the one worth having. Because the engine settles a page to
quiescence before answering any verb, this returns a decision rather than a
sleep, so polling it in a loop does no good.

#### Logging in

Never type a credential as a literal. Set it in the environment the session is
opened in and name it:

```bash
H5I_SECRET_ACME_PASS=… h5i browser open https://acme.example --allow acme.example

h5i browser env                       # names only, never values
h5i browser type @e2 '$H5I_SECRET_ACME_PASS'
```

The value is substituted on the way into the field and the reply echoes the
**placeholder**, so the credential never enters the agent's context. No verb
returns a credential's value, a password field reports a mask rather than what it
holds, and anything a page reflects back is scrubbed on the way out.

For a flow the engine cannot drive, `session login` hands the page to a human and
refuses every read until they end it. It does not withhold frames, and
[says so](DESIGN.md#logging-in) rather than implying otherwise.

#### Live connections

`WebSocket` and `EventSource` are real objects over real connections, and **every
frame is receipted**, not just the handshake. A dev server's hot-reload channel is
the case they are for.

`ws://` and `wss://` both work: the socket owns its transport, so TLS needs
nothing from the HTTP client. A remote socket of either kind is refused while an
egress proxy is configured, because a raw socket would not go through it and
that objection was never about encryption. A page holding a live connection is
the one page here that is not deterministic, and `snapshot` reports
`open_sockets` when that is true.

#### When a verb refuses

Every failure carries a `code`, prose naming the recovery, and `retryable`.
`stale-ref`, `no-such-ref` and `no-match` are yours to fix; `refused` and
`no-script` are not. A selector a model can correct and an allowlist it cannot
are different answers, and reporting the first the way the second is reported
ends a self-correction loop instead of prompting it.

## What is guaranteed, and where

| | Bare host | Inside an [h5i](https://github.com/h5i-dev/h5i) box |
| --- | --- | --- |
| Allowlist on every request and redirect hop | yes | yes |
| Fail-closed receipts (`--receipts`) | yes | yes |
| The fence, deterministic settles, credential indirection | yes | yes |
| The agent cannot go around the browser | **no** | yes |

A standalone run is not sandboxed and this file will not say it is. What it is: a
browser whose entire network activity is in a log you can read.

## What it cannot do

- **Not a Chromium replacement.** Docs-grade pages are the compatibility bar.
  Video, WebGL and heavy React apps belong on the Chromium path.
- **JavaScript is opt-in and limited.** Off unless `serve` is given `--script`. A
  page that renders only via script comes back empty, which is a routing signal:
  ask `capabilities` rather than guessing.
- **No iframes and no file uploads.** Each refused by name with the reason
  rather than failing obscurely.
- **Not the fastest or the most conformant.** It is an interpreter, roughly 1.3x
  Chromium's wall time on real pages. Speed was never the claim.

When a page needs something absent, the snapshot names it:

```
note: this page used Web APIs this engine does not have
      (IntersectionObserver x1). What depends on them did not run.
```

## How it is built

Assembled, not written from scratch. The parts that are ours are the parts that
make it h5i's.

| Concern | Component |
| --- | --- |
| HTML parsing, DOM | `blitz-html`, `blitz-dom` |
| CSS, layout | Stylo, Taffy |
| Paint | `blitz-paint`, `vello_cpu` (a box has no GPU) |
| Text, fonts | `parley`, `fontique` |
| JavaScript | Boa, behind `--script` |
| Policy, receipts, HTTP, the agent surface | this crate |

## More

- [DESIGN.md](DESIGN.md): why the engine is shaped this way, and what each shape
  cost: the CONNECT-gate argument, the cookie narrowings, the settle loop, the
  fence's tested property, and the measurements with their caveats.
- [ROADMAP.md](../../ROADMAP.md): §12 and §B1 to §B15 are the authority on scope
  and order.
- `h5i browser --help` is the authoritative flag reference; `h5i __engine --help`
  is the engine's own.

## License

Apache-2.0. See [LICENSE](../../LICENSE).
