# h5i Manual

Command reference for h5i. **New here? Read [What h5i is](#what-h5i-is) and
[The loop](#the-loop) first**: they give the mental model before the
per-command reference.

`h5i <command> --help` is always authoritative for flags. This manual explains
what the commands are *for*.

---

## What h5i is

> Give coding agents full autonomy to build and test web apps inside a
> disposable environment, without exposing your machine or your credentials.

**h5i** (pronounced *high-five*) is a **contained agentic development
environment**: a throwaway box that holds the code, the agent, the toolchain,
the dev server, and a real browser, with nothing of your machine inside it and
nothing leaving it except a patch you reviewed.

It is one Rust binary. No server, no daemon, no SaaS.

### Five parts

Everything h5i does maps to one of these:

1. **A disposable workspace.** The code is copied into the box (a pull request,
   an existing repository, a fresh project) along with everything it pulls in.
   No host directory is mounted read-write into the agent's reach.
2. **A sandboxed coding agent.** Claude Code or Codex, its child processes, its
   package managers, builds, tests and dev server all run inside the same
   boundary. A runaway agent stays in the box.
3. **A credential and network broker.** No SSH key, GitHub token, model API key,
   cloud credential or personal browser profile enters the box. A host-side
   proxy authenticates the calls the policy allows; egress is an allowlist.
4. **A browser in the box, with two interfaces.** Chrome and its profile live
   inside. The agent drives it through a CLI; a human can watch the same
   viewport and take over.
5. **An output gate.** At the end you export a patch, a report, and an execution
   receipt, after inspection. The agent has no direct write path to the host.

The value is not any one of these. It is that code, agent, dev server, browser
and export all sit inside one boundary that both the agent and the human can
operate.

### What it is not

- **Not a provenance system.** h5i used to record who wrote what, with git
  notes, blame overlays and a multi-agent orchestra. That is gone. What survives
  is containment and the receipt of what actually ran.
- **Not a defence against a targeted kernel exploit.** See [Limits](#limits).

---

## The loop

```bash
h5i box .                                  # a box from this repository
h5i box shell mybox                        # work in it (this is where an agent runs)
# inside: edit, build, start the dev server, drive the browser
h5i box export mybox --out ./review        # patch + report + receipt, for a human
git apply --3way ./review/patch.diff       # apply it where you want
```

The full loop the browser makes possible:

```
agent edits code -> starts dev server -> opens the app with agent-browser
  -> reads the accessibility tree -> clicks and fills -> reads console and
  network errors -> screenshots -> fixes the code -> human watches or takes over
  -> export patch, report, receipt
```

---

## Install

```bash
curl -fsSL https://h5i.dev/install.sh | sh     # prebuilt binary
cargo install --path .                         # from source
```

`h5i.dev/install.sh` and
`raw.githubusercontent.com/h5i-dev/h5i/main/install.sh` are the same file, and
CI fails if they ever stop being. Use the second one if you would rather the
install path not depend on the domain.

Then, so your agent knows how to use it:

```bash
h5i skill install           # writes the skill into ~/.claude/skills/h5i (or ~/.codex)
npx skills add h5i-dev/h5i  # same bytes, if you do not have the binary yet
```

---

## Command groups

| Group | What it is for |
|---|---|
| [`h5i box`](#h5i-box) | Create, run, inspect and export boxes. Almost everything. |
| [`h5i ui`](#h5i-ui) | The box console: one read-only screen over the whole fleet. |
| [`h5i browser`](#h5i-browser) | The control lock: who is driving a box's browser. |
| [`h5i skill`](#h5i-skill) | Write or print the agent skill this binary carries. |
| [`h5i join`](#h5i-box-share) | Open a box someone else is sharing, from their ticket. |
| `h5i completion` / `h5i man` | Shell completions and the man page. |

`h5i dev *` and `h5i env *` both remain as hidden aliases for `h5i box *`
through one release. The noun the product uses everywhere else is *box*, so the
command is too.

---

## h5i box

### Making a box

```bash
h5i box .                       # snapshot this repository at HEAD
h5i box --pr 1234               # a pull request (number, #number, or URL)
h5i box https://github.com/o/r  # clone an external repository
h5i box --new                   # an empty box; the agent builds from nothing
```

`h5i box [SOURCE]` is shorthand for [`h5i box create`](#h5i-box-create), and
takes the same flags. A pull request is `--pr`, not a positional: a bare number
is ambiguous with everything else a source could be, and `h5i box create`
already spelled it as a flag.

Where the code comes from decides the shape of the box:

- **This repository** → a real git worktree on its own branch, sharing the
  object store, so `h5i box apply` can land it back locally.
- **A URL, a PR, or `--new`** → a **detached** box. It gets a repository of its
  own inside its directory, this repository is neither read nor written after
  creation, and the inherited `origin` remote is dropped so the box cannot reach
  a network handle nobody granted it. `apply` and `rebase` refuse and point at
  `export`. This is the shape external code should always arrive in.

### h5i box create

```
h5i box create <NAME> [--from <rev>] [--pr <n>] [--clone <url>] [--new]
                      [--profile <p>] [--isolation <tier>] [--image <img>]
                      [--engine <chromium|lightpanda|h5i-light>]
```

The base revision is frozen at creation and pinned immutably. The policy is
resolved, digested and stored *before* any state is created on disk, so an
unsatisfiable request fails closed rather than leaving half a box behind.

| Flag | Meaning |
|---|---|
| `--from <rev>` | Base revision (default `HEAD`). |
| `--pr <n\|url>` | Fetch `refs/pull/<n>/head` and pin it as the base. Needs only `git`. |
| `--clone <url>` | Copy an external repository in. Detached. |
| `--new` | Empty box (a fresh repository with one empty commit). Detached. |
| `--profile <p>` | See [Profiles](#profiles). |
| `--isolation <tier>` | See [Isolation tiers](#isolation-tiers). |
| `--image <img>` | Base image for `isolation=container` and `isolation=microvm`. Pre-pulled; runs never pull. |
| `--engine <e>` | Browser engine for the `browser` profile: `chromium` (default), `lightpanda`, or `h5i-light`. Pinned in the digest; never falls back. See [Choosing the engine](#choosing-the-engine). |

A profile can also refuse individual browser actions, enforced by h5i on the
daemon's control socket rather than advised:

```toml
[profile.browser.browser]
deny = ["evaluate", "state"]   # a bare family name covers state_save/state_load
```

`evaluate` is arbitrary code in the page; `state_*` and `credentials_*` reach
the browser's stored secrets. A denied verb never reaches the browser, and the
refusal lands in the receipt's `browser-proxy` lane. This is enforcement
against an agent using the documented path, not containment against one that
goes looking: the daemon runs inside the box, and a box has no internal
privilege boundary.

### Working in a box

```bash
h5i box ls                            # every box on this clone
h5i box status <name>                 # policy actually enforced, evidence, base drift
h5i box run <name> -- cargo test      # one command; the exit code passes through
h5i box shell <name>                  # interactive confined session
h5i box diff <name>                   # what changed against the pinned base
h5i box log <name>                    # the box's event log
```

`h5i box shell` is the agent-in-box: stdio is inherited, so every command the
session spawns is contained by the box rather than by the agent choosing to wrap
each call.

### Services and ports

```bash
h5i box service start <name> <service>   # a declared long-lived process
h5i box service status <name>
h5i box service logs <name> <service>
h5i box ports <name>                     # the per-box dynamic port map
```

Services are declared in `.h5i/env.toml`:

```toml
[service.web]
command = "npm run dev"
port = 3000
```

Supported at the `workspace` and `process` tiers in v1. At `supervised` and
`container` the network namespace belongs to a single session, so run the dev
server inside the same `h5i box shell` as everything else.

### h5i box export

The output gate. A box has no write access to anything outside itself; this is
the only way out, and it is deliberately a human step.

```bash
h5i box export <name> --out ./review
```

Produces:

| File | What it is |
|---|---|
| `patch.diff` | The tree diff against the pinned base, path-validated: no symlink escapes, no nested `.git`, no agent-introduced gitlinks. |
| `report.md` | What ran, what the browser saw, who was at the controls, and the agent's own proposal. |
| `receipt.json` | Every observed execution, with the policy digest that was enforced. |

It refuses rather than overwrites an existing non-empty directory (`--force` to
replace). Secret redaction and size caps apply to all three.

Read `report.md` before applying. It surfaces, in this order:

- **denied egress attempts**: the box tried to reach hosts the policy refused
- **what ran**: every command, its lane, its exit code
- **what the browser saw**: console errors, uncaught exceptions and failed
  requests, observed by h5i rather than reported by the agent
- **viewer sessions**: including whether a human took the controls
- the agent's proposal

Then apply it where you want:

```bash
git apply --3way ./review/patch.diff
```

`h5i box apply <name>` still lands a proposed box onto its parent branch in this
repository, for the local case where that is what you want. It refuses for a
detached box.

### h5i box cache

Cold dependency install is the difference between a 20-second box and a
four-minute one, so warm caches are in scope.

```bash
h5i box cache ls              # caches for this project, and whether they are stale
h5i box cache mounts          # exactly what a box would get
h5i box cache refresh <eco>   # populate one, in a dedicated box with no agent in it
h5i box cache rm <eco>
```

Rules that make this safe rather than merely fast:

- One cache per project and ecosystem, keyed by a digest of that ecosystem's
  lockfiles. A cache whose key no longer matches is listed as stale and never
  handed to a box: packages resolved for a different dependency set are a
  silent, hard-to-explain wrong answer.
- Mounted **read-only** into an agent box. That costs nothing in correctness:
  every package manager falls back to fetching what it cannot find.
- Written **only** by `h5i box cache refresh`, which runs the install step alone,
  with egress narrowed to the registry hosts and no agent inside. `refresh`
  needs a project-declared profile whose egress is the registry hosts and
  nothing else, and it refuses with that profile written out ready to paste
  rather than creating a box whose fetch could not have worked.

No mutable surface is ever shared between an agent box and anything else.

### h5i box view

Watch a box's browser from the host, and take over when you want to.

```bash
h5i box view <name> [--port 7331]     # serve it to your browser
h5i box view <name> --term            # draw it in this terminal
```

The box has to be running (a live `h5i box shell` or `h5i box run`), and its
browser has to be streaming (`agent-browser stream enable`, inside the box).

This is a security boundary, not a convenience, so it is worth knowing what it
is. The box's stream port is **never published**: it stays inside the box's
private network namespace, and h5i enters that namespace by pid, connects from
inside, and hands the socket back out. Then:

- the forward binds loopback only, on a port h5i chose
- every connection carries a **per-box token**, minted at creation and never
  written anywhere the box can read
- cross-origin WebSocket handshakes are refused, so a page you have open in
  another tab cannot reach a running box
- **frames flow out always; input flows in only for the control-lock holder**

#### In the terminal

`--term` draws the page in the terminal you are already in, next to the agent.
It also works over SSH, which the browser viewer does not without forwarding a
port. It needs a terminal that speaks the Kitty graphics protocol: kitty,
Ghostty, WezTerm and Konsole all do. When yours does not, h5i says so and points
you at the browser viewer rather than filling the screen with base64.

This path binds nothing at all. There is no port, no token, and no page: the
viewer runs inside the command you just typed, enters the box the same way the
forward does, and holds the socket itself. Nothing else on the host can connect
to it, so there is nothing to authenticate.

The other direction matters as much. A terminal's output is not text: an escape
sequence can rewrite your clipboard, retitle the window, or ask the graphics
protocol to read a file. So the box never writes to your terminal. It supplies
compressed pixels inside a WebSocket message, and every escape sequence you
receive is generated by h5i.

Row one is h5i's, and a page cannot draw on it or be clicked through it. It
carries the box, the mode, who holds the control lock, the page's origin, the
egress policy, and a count of console errors. The origin is the field that never
gets shortened at the expense of the truth: a long URL loses its path, and a
host too long for the row is cut from the left, because shortening
`bank.example.evil.test` from the right is the trick itself.

Two modes:

| Key | Does |
| --- | --- |
| `i` | Take the control lock and start driving the page |
| `Ctrl-]` | Hand control back and return to watching |
| `q` | Leave |

Watching is read-only and leaves the mouse to your terminal, so selection and
scrollback still work. Driving takes the lock, because in a terminal viewer
there is no second window to run `h5i browser take` in. Leaving hands it back.
Both are recorded in the receipt, under the same lane the browser viewer uses.

Two limits worth knowing. A terminal reports key presses and not releases, so
h5i sends a press and a release together: typing works exactly, and holding a
key down does not. Clicks are placed at the resolution of a terminal cell, which
is fine for a form and coarse for a dense canvas.

### h5i box share

Let one other person try the web app in a box, from their own machine, while it
is still running inside the boundary. This is the one path in h5i that lets
something *in*, so it is worth reading before you use it.

```bash
h5i box share <name> [--port 3000] [--expire 60m] [--label alex]
h5i box share <name> --direct-only    # fail rather than relay a single byte
h5i box share <name> --tunnel         # any browser, no h5i on their side

h5i box share status <name>
h5i box share ls
h5i box share grant <name> [--label sam]     # a second ticket (--tunnel only, see below)
h5i box share revoke <name> <grant>
h5i box share stop <name>
```

The other side runs:

```bash
h5i join h5i1_eyJ2IjoxLCJib3hf…
```

**The box's port is never published.** h5i enters the box's network namespace by
pid — the same way `h5i box view` does — dials `127.0.0.1:<port>` from inside,
and passes the socket back out. Nothing binds an external address on this
machine, and the box gains no reachability it did not have. That entry happens
**once**, at startup: a small helper process lives in the box's namespaces for
the life of the share and answers "connect me", pinned to the one port you named.
Nothing on the wire, from a peer or from the shared page, can move where it
connects.

**A share needs a box with a network of its own.** Only then is "the box's port
3000" a distinct thing from this machine's port 3000; otherwise sharing it would
publish whatever happened to be listening on the host, so `h5i box share`
refuses rather than guessing. A box has one when it is running and either at the
`supervised` or `container` tier, or at `process` with a profile that denies
egress. It does not at `workspace`, or at `process` with a profile that grants
egress, because both share the host's network.

#### The ticket is the whole access model

A ticket names one box, one port, one grant and an expiry, and carries a
256-bit secret. Holding it is the authorization; there is no account on either
side. h5i keeps only the secret's SHA-256, so **a ticket is printed once and
cannot be reprinted** — mint another with `share grant`.

A ticket is a capability, not a seat: nothing marks one as used or binds it to a
person, so forwarding the text admits everyone it reaches, under the one grant.
What one ticket per person buys is that `share revoke <name> <grant>` cuts off
exactly the people you gave *that* ticket to, rather than everybody. `share grant` mints a second one, but
only for a `--tunnel` share today: a peer-to-peer ticket needs the running
endpoint's addressing and only the serving process has it, so `grant` refuses on
a P2P share and says to start a second one instead.

Authorization is re-read from disk on every connection, so a revoke from another
terminal takes effect on the next one; connections already open are dropped
within about a second, by a watchdog that asks about *that peer's own grant* —
so revoking one person cuts their live connections while everyone else's keep
working. `share stop` revokes everything; the serving process then writes its
receipt and exits on its own, which is why it is not a `kill`.

The longest a share may last is 24 hours, and the default is one.

#### The two transports

**Peer to peer (the default).** QUIC between the two h5i processes, end-to-end
encrypted, hole-punched to a direct path when the networks allow it. When they
do not, a relay carries it: the relay moves sealed packets and sees both
addresses, the timing and the volume, never the content. `--direct-only` refuses
that fallback. A peer that cannot get a direct path is turned away — **before
any application byte crosses** — and the share stays up for anyone who can,
which is the useful behaviour when one peer is behind a hostile NAT and another
is not. It keeps checking afterwards,
because a hole-punched path can die and the transport will slide onto a relay.
Be precise about the second half: that check runs once a second, so a path that
fails mid-session can carry up to about a second of traffic over a relay before
the connection is closed. Setup is a guarantee; staying direct is a short leash.
A flag that merely preferred a direct path would be worse than none, because it
would let you believe nothing was in the middle when something was.

**Quick tunnel (`--tunnel`).** A browser cannot speak QUIC to an endpoint id, so
peer to peer needs h5i on both ends. When the person you want clicking the
prototype is a designer or a customer, `--tunnel` shells out to `cloudflared`
and hands back a plain link. The costs, plainly:

- **Cloudflare terminates TLS, so this path is not end to end.** Cloudflare can
  read the traffic. That fact is written into the box's receipt, not only here.
- `cloudflared` is a binary we neither ship nor pin. If it is missing, h5i says
  so and names the alternative.
- Cloudflare quick tunnels are not a production service: they cap concurrency
  and do not carry server-sent events.

What does not change is everything under the transport. The link carries a
token, the token is checked against the same grant table on every connection,
revocation still works mid-session, and the credential is stripped before
anything reaches the box. The capability degrades from "hold the secret" to
"hold the link"; it does not degrade to nothing.

#### What the shared app can and cannot see

The token travels in the URL on the first request only. h5i answers that request
with a redirect that moves it into an `HttpOnly` cookie and sends the browser to
the same page without it — so it stays out of the address bar, out of `Referer`
on every outbound link, and out of the app's own logs. On the way to the box,
both the cookie and the query parameter are removed. The app being shared never
sees the credential that admitted its visitor.

The app's own cookies are passed through untouched, and so is its query string
with two exceptions worth knowing: a parameter literally named `h5i` is taken as
the share token and removed, and empty pairs are dropped (`?a=1&&b=2` arrives as
`?a=1&b=2`). WebSockets pass through as well: hot reload works, because a share
of a dev server that never reloaded would not be a share of a dev server.

A share carries at most 64 connections into the box at once. Past that, a
visitor gets a `503` telling them to reload rather than a `401` telling them
their link is bad, and the count of refusals lands in the receipt. The ceiling
exists because a share is a door on the open internet in tunnel mode and an
endpoint anyone may dial in P2P mode: without it, one peer — or one page opening
sockets in a loop — becomes unbounded connections into the box.

One connection carries exactly one request, and that is an authorization
control rather than a performance choice. A connection is checked when its
first request arrives, which is only the same as checking every request if it
cannot carry a second — and by default it can. `cloudflared` keeps a pool of
connections to the origin and reuses them for whatever request comes next, from
whatever visitor; browsers pool per origin the same way, which puts the
identical problem on the joiner's proxy.

So h5i reads the request's head, then exactly as many body bytes as it declared,
and then stops forwarding anything else on that connection. `Connection: close`
goes to the box as well, but that is a courtesy: the box runs agent-written code
and may decline, and the control cannot depend on it. On the way back, the
response's own `Connection` header is replaced with `close` — otherwise a
keep-alive answer would tell the visitor's browser to reuse a connection that
will never answer again — and the response is framed by its `Content-Length` so
the connection ends when the response does. A response that says two
contradictory things about its length has both of them taken off, so the visitor
is left with one framing rather than two — the chunk stream if a
`Transfer-Encoding` was the other half of the contradiction, the connection
closing if it was a second length; a response the box starts and never finishes
becomes a `502`, because relaying an unfinished head verbatim would let the box
choose when to be sanitised.

Two consequences worth knowing. A chunked request body is parsed rather than
just copied — forwarding one request means knowing where it ends, and a chunk
stream only says so in its own framing — though it is forwarded verbatim, chunk
headers and all, so the box sees the request it would have seen anyway. And an
upgrade is the exception to the one-request rule: it does become a two-way pipe,
but only after the box has actually answered `101`, and only when the request
asked for it properly with both an `Upgrade` header and a `Connection: upgrade`.
A request that merely attached an `Upgrade:` header gets no exception.

The cost is that every request is its own connection. For a dev server that
serves each module separately, a first page load is a few hundred of them, each
with its own dial into the box. It works and it is not free.

#### What the person joining is taking on

The app is agent-written code, and joining runs it in their browser. That is the
point of sharing a port rather than a picture of one, and it is also the
exposure — the same one as clicking any link a colleague sends.

One asymmetry is worth knowing, because it runs the other way from what you
would guess. In peer-to-peer mode the app is served from the joiner's own
loopback, and browsers exempt loopback origins from their private-network
protections, so a hostile page has an easier reach at that machine's local
services than the same page on a public origin would. Tunnel mode, ironically,
keeps those protections, because the origin is public.

The joiner's local proxy is gated for the same reason the viewer forward is: a
port on loopback is reachable by every process on that machine and every page in
that browser. Its URL carries a token minted **on the joining side**, which is
not the ticket secret — nothing that authorizes the share is ever handed to a
browser.

#### What lands in the receipt

Every other receipt lane observes what left a box. This one records what came
in, and it is host observed in the strongest sense available: h5i owns both ends
of the bridge, the box supplies none of it and cannot suppress it.

```
share session, 612s (p2p transport)
opened   2026-08-10T10:00:00+00:00
closed   2026-08-10T10:10:12+00:00
shared   port 3000 inside the box, never published on the host
endpoint kbcd7fq2m4xn8s6r3v9w1y5z7a2b4c6d8e0f2g4h6j8k0l2m4n
peers    1
  kbcd7fq2m4x… via direct — grant a1b2c3d4 (alex), 300s, 12 connections, 900 in / 5000 out
refused  2 attempt(s): 1 unknown ticket, 0 expired, 1 revoked
```

A box that was opened to someone and an identical box that was not are different
artifacts, and an export should not be silent about which one it came from. A
tunnel session carries the "not end-to-end encrypted" note in the same block.

### Inspecting what happened

```bash
h5i box probe                       # what this host can enforce at all
h5i box capabilities <name> --json  # what this box actually got
h5i box doctor <name>               # can it still enforce its claim? are its refs intact?
h5i box secrets <name>              # declared grants, dry-run resolution, never values
h5i box inspect <name> --capture <id>
h5i box compare <a> <b>             # boxes side by side
```

### Lifecycle

```bash
h5i box rebase <name>       # re-pin onto the parent branch's current tip
h5i box abort <name>        # stop; manifest and workspace preserved for forensics
h5i box rm <name> [--force] # remove entirely
h5i box gc                  # reclaim applied/aborted workspaces
```

### h5i box allow

```bash
h5i box allow                 # list the current entries
h5i box allow api.example.com
```

A persistent, user-level egress allowlist merged into every container-tier box
whose profile *already* sets `net.egress`. A deny-all profile is never widened.
Stored under `~/.config/h5i/`, outside every box-granted path, and it refuses to
run inside a box.

---

## h5i ui

```bash
h5i ui                  # http://127.0.0.1:8765/?token=…
h5i ui --port 0         # let the OS pick the port
h5i ui --open           # hand the URL to this desktop's browser too
```

The box console: the same fleet the commands above report on, drawn as one
screen. Left is every box with its tier, status and one signal; right is the box
you picked: the policy that was actually enforced, the services it declares,
its diffstat against the pinned base, and a flight recorder with one row per
receipt across five lanes (FS, NET, PROC, RES, PAGE). Click a row for the
rendered receipt, the same text `h5i box inspect` prints.

**It cannot drive anything.** Every route is a `GET`. `shell`, `run`, `export`,
`propose`, `apply` and `rm` stay in the CLI, where a human types them, so there
is no mutating surface to guard and no way to turn the console into a remote
control for someone's boxes.

**What guards it.** The server binds `127.0.0.1` and nothing else. The URL
carries a token minted for this session and kept in memory (never written to
disk, so no box can read it), which the page trades for a `SameSite=Strict`
cookie on first load. Requests from another origin are refused outright.

**What the colours mean.** Red is the only one that makes a claim about the
boundary: the egress allowlist *refused* a destination, host-observed by the
proxy. Amber is something to look at (a run exited non-zero, the wall-clock
limit killed one, or the in-box browser reported errors) and says nothing about
containment. Grey means the evidence is weak: either the tier is `workspace` and
nothing was confined, or every receipt came from the in-box shim and so is the
box's own account. Each run row is labelled `host-observed` or `box-claimed` for
the same reason. Nothing on the screen is a score.

The console is a default-on cargo feature. `cargo build --no-default-features`
drops it, along with axum, tokio and the build script's need for Node, and the
`h5i ui` command with it.

---

## h5i browser

Deliberately four verbs. Driving the browser is `agent-browser`'s job; what h5i
owns is arbitration between the agent and a human, because nothing upstream does
it.

```bash
h5i browser status <name> [--json]   # who holds control, and whether @refs are stale
h5i browser take <name>              # a human takes control
h5i browser release <name>           # hands it back
h5i browser url <name> [--port n]    # the viewer URL, token included
```

The rules:

- **The agent holds control by default.** A box exists to let an agent work; it
  should not have to ask.
- **A human takes control, never asks for it.** Someone reaching for the viewer
  wants the pointer now, and the agent is a program that can wait. The agent's
  mutating verbs are refused with a typed message rather than fighting for the
  pointer; read-only verbs keep working, because watching never collides.
- **Handing control back invalidates what the agent knew.** The page moved, so
  every `@ref` from its last snapshot may now point somewhere else. It must
  re-snapshot before acting, and acting first is refused rather than mis-clicked.

### Choosing the engine

`--engine` picks what runs pages in the box, and the choice is pinned in the
digest: a run never quietly falls back to a different one.

| engine | what it is |
| --- | --- |
| `chromium` (default) | A real browser. Everything works; it costs what a browser costs. |
| `lightpanda` | A third-party headless engine. |
| `h5i-light` | h5i's own engine, described below. |

#### h5i-light

An engine built for the case an agent is actually in: read a page, act on it,
read what changed. It runs in **one process** and holds a page in roughly a
seventh of the memory Chromium needs (76 MiB against 563 MiB, median over the
same pages), from a binary a ninth the size. It is about 30% *slower* — it has
an interpreter where Chromium has a JIT — and that trade is the whole point of
choosing it deliberately rather than by default.

It runs page JavaScript, follows redirects and `<meta refresh>`, keeps a cookie
jar scoped to one origin, and puts **every request through the same broker as
the rest of h5i**, so a page's own fetches are policy-checked and receipted like
anything else. What it reads is an outline with `@ref` handles, not pixels.

Two properties worth knowing before you rely on it:

- **It says what it could not do.** A page needing an API this engine lacks gets
  that API *named* in the snapshot rather than a blank space, and console errors
  carry the script and line they came from. "The page is empty" and "I could not
  read the page" are different answers, and it gives different answers.
- **It is not a complete browser, and does not pretend to be.** Of twenty
  single-page applications measured, eighteen read usefully and one not at all.
  Canvas, WebSockets, Workers and IndexedDB are absent. For a page it cannot
  read, the answer is `--engine chromium`.

Driven directly, it has its own CLI — `h5i-browser-light --help` — including two
verbs the size of a page makes worth having:

```bash
h5i-browser-light session snapshot --delta   # only what changed since last read
h5i-browser-light session login              # hand the page to the human
```

`--delta` matters because re-reading three hundred lines after every click is
the wrong shape for an agent loop. `session login` closes the page to the agent
while a person types a credential into the live view: the session it establishes
stays in the jar afterwards, and the agent can see *that* it is logged in
without ever reading the cookie that says so.

### Driving the browser itself

That is `agent-browser`, run **inside** the box, and its `--help` is the verb
table. h5i does not wrap it: forty automation verbs behind a second CLI would
buy nothing but drift.

```bash
agent-browser open http://localhost:3000
agent-browser snapshot                  # accessibility tree with @refs
agent-browser click @e2
agent-browser fill @e3 "test@example.com"
agent-browser screenshot shot.png
agent-browser stream enable             # so `h5i box view` has something to show
```

What the `browser` profile does to Chrome, and why:

- **Fresh profile, created in the box.** No host cookie jar, extension or
  history. Nothing you are logged into on the host is logged in there.
- **Chrome's egress is the box's egress**, enforced by the tier. Loopback stays
  open, because the dev server under test is the whole point.
  `--allowed-domains` is derived from the same policy as a second layer.
- **AI chat is refused.** `agent-browser chat` sends page content to an external
  gateway, which inside a box is an exfiltration path with a friendly name. The
  gateway credential is never injected, and its absence is the mechanism.
- **On macOS it is granted the host's per-user temp directory**
  (`/var/folders/<xx>/<yy>/T`), read and write. This is the widest thing any
  profile asks for and it is worth understanding before you use it. Chrome puts
  its ProcessSingleton lock socket there, and it finds that directory through
  `confstr(_CS_DARWIN_USER_TEMP_DIR)` rather than `TMPDIR`, so the per-env
  `/tmp` redirect cannot move it, and without the grant Chrome will not start.
  The cost is that the directory is shared: a browser box can read what other
  host processes leave there and can plant files they will pick up. That is
  exactly the cross-agent rendezvous point the `/tmp` redirect exists to remove,
  reintroduced for this one profile on this one platform. Other profiles, and
  every profile on Linux, are unaffected.

    Two consequences of that grant being a machine-specific absolute path. The
    pinned policy digest differs between two Macs for the same profile. That is
    harmless, because `policy.resolved.toml` is verified against the digest
    stored beside it, and both are written together at create time; nothing
    re-resolves the profile and compares. But a `browser` env created on one
    machine and pulled to another carries a grant for a directory that does not
    exist there, so Chrome will fail to start until the env is recreated.

---

## h5i skill

`skills/h5i/` is embedded in the binary at build time, so the skill cannot
document flags the installed binary does not have.

```bash
h5i skill install [--target <dir>]   # write it out
h5i skill show [<page>]              # print SKILL.md or one reference page
h5i skill path                       # where an install would write
```

This is also how the *in-box* agent gets the skill: nothing is baked into an
image, and nothing is copied from host to box.

---

## Policy

A box's policy is resolved at creation, serialized to `policy.resolved.toml`,
and **digested**. Every receipt records the digest that was actually in force,
so "what was enforced" is never a matter of trust.

### Profiles

Built-ins need no file:

| Profile | What it grants |
|---|---|
| `default` | Fail-closed build/test confinement: system paths read-only, `$WORK` read-write, no network. |
| `agent` | The agent-in-box surface, scoped to `$H5I_AGENT`'s runtime. |
| `agent-claude` / `agent-codex` | Pin one runtime: only that agent's HOME state and API egress. |
| `browser` | The agent profile plus headless Chrome and the `agent-browser` daemon. |

Runtime scoping is not cosmetic: a Claude box must not get Codex's credentials
or egress to OpenAI, because a prompt-injected agent could otherwise read the
*other* runtime's token and use it against an allowlisted host.

Custom profiles live in `.h5i/env.toml`:

```toml
[profile.review]
isolation = "supervised"

[profile.review.fs]
read  = ["/usr", "/etc"]
write = ["$WORK"]

[profile.review.net]
mode     = "deny"
egress   = ["api.github.com"]
unix     = false          # AF_UNIX sockets; see below
loopback = [3000]         # macOS only; see below

[profile.review.resources]
mem   = "4G"
procs = 256
wall  = "30m"
```

`wall` is enforced everywhere. **`mem` and `procs` are not enforced at the
`process` and `supervised` tiers on macOS**: Darwin has no cgroups, does not
enforce `RLIMIT_AS` against the mmap'd heap every modern runtime uses, and
scopes `RLIMIT_NPROC` to the whole user rather than to one box, so h5i declines
to apply a limit it cannot hold. `h5i box status` marks such a value with `*`
and says so underneath, rather than listing it as enforced. Use
`isolation = "container"` or `isolation = "microvm"` where you need a real
ceiling: both cap memory and process count in the runtime itself.

### Isolation tiers

| Tier | What it is | Network scoping |
|---|---|---|
| `workspace` | No confinement; just a separate worktree. | none |
| `process` | Landlock + seccomp + namespaces, with a supervisor and a private pid namespace. | deny or host |
| `supervised` | Everything `process` has, including the private pid namespace, plus a private netns with an nftables egress allowlist pinned to resolved IPs, DNS pinned by a hosts file, and a seccomp-notify gate on `socket()`. | **L3/L4** |
| `container` | Rootless Podman: a portable image, with a CONNECT-proxy egress allowlist. | L7 |
| `microvm` | A hardware-isolated guest with its own kernel, booted by [microsandbox](https://microsandbox.dev) (`msb`) from the same OCI images. Egress rules are evaluated by the VM's network stack. | **L3/L4** |

`auto` (the default) picks the strongest tier the host can actually run. An
explicit tier **fails closed** if the host cannot satisfy it: h5i never
silently downgrades.

Worth being clear about, because two drafts of the design got it backwards: the
container tier buys **portability**, not tighter network control. Its allowlist
is a proxy, so it binds proxy-respecting tooling only. `supervised` enforces at
L3/L4 and does not have that hole.

#### The microvm tier

`microvm` is the one tier where the boundary is a virtual machine rather than a
policy applied to a host process. A kernel exploit inside the box meets the
hypervisor, not the host kernel it just subverted. Its `net.egress` allowlist
becomes default-deny plus one address rule per allowed destination, so a raw
socket to an unlisted IP is dropped rather than merely un-proxied.

Requirements, all three, or the tier refuses:

- microsandbox's `msb` on `PATH`, version 0.6 or newer.
- Host virtualization: `/dev/kvm` openable on Linux, Apple Silicon on macOS.
  A stock WSL2 kernel and most cloud CI runners have neither.
- A base image, from `--image`, the profile's `container.image`, or the
  repo-level `[container] image`, the same images the container tier runs,
  pre-pulled with `msb pull`.

`h5i box probe` reports which of the three is missing, since "install a package"
and "enable nested virtualization" are different problems.

Two things it does **not** do yet, stated rather than left to be discovered:

- **No per-request egress tally.** The container tier's proxy sees every CONNECT
  and records allow/deny counts in the capture manifest. A netstack filter drops
  packets without reporting them, so a microvm receipt carries no egress
  summary. Stronger enforcement, thinner evidence.
- **No authenticated-egress grants.** `[[profile.X.auth]]` hands the box a base
  URL pointing at a credential proxy on the *host's* loopback, which a microVM
  guest cannot dial. A profile that declares grants is refused at this tier
  rather than handed an origin that resolves to nothing.

In-box observation works as it does under `container`: the read-only
managed-settings mount carrying the `wrap-bash` hook, and the capture spool at
`/.h5i/spool`. The container tier's tee shim has no analogue here (it depends on
self-mounting the image, which a VM has nothing to do).

### AF_UNIX sockets

`[profile.X.net] unix = true` lets the box create `AF_UNIX` sockets. Off by
default, because `SCM_RIGHTS` passes file descriptors, which is authority
smuggling.

What the grant does *not* open, which is why it can exist at all: abstract
sockets are scoped by the box's private netns; filesystem-bound ones are scoped
by Landlock; and `/tmp`, where `.X11-unix`, `tmux-*` and an ssh-agent live, is
a per-box scratch at the kernel tiers. What is left is a host socket sitting
inside a granted path, so the grant is opt-in per profile and pinned in the
digest.

The `browser` profile sets it, because the `agent-browser` daemon's control
socket is a filesystem-bound `AF_UNIX` listener.

### Credentials

- **Model API**: the key stays on the host. A reverse proxy injects it into
  outbound requests from the box, scoped per runtime, so a Claude box cannot
  reach the OpenAI credential.
- **Any other service**: the same mechanism, declared as policy:

        [[profile.review.auth]]
        host           = "api.github.com"
        credential_env = "GITHUB_TOKEN"   # read on the host, never in the box
        base_url_var   = "GH_HOST"        # what the client reads
        token_var      = "GH_TOKEN"       # where the box gets its per-run dummy

    `token_var` is required. The proxy gates every request on a per-run token, so
    the box has to be handed it in whatever variable its client already sends as
    a credential. The real credential stays on the host; the box only ever holds
    the dummy.

    The limit is real and worth knowing before you declare one: it binds clients
    you can point at another origin, so a plain `curl https://api.github.com`
    still goes nowhere. A TLS-terminating forward proxy would lift that, at the
    cost of a CA the box trusts, and it is deliberately not built.

    Restricting *what* the box may do with a credential is authorization, and it
    belongs where it is already solved: a fine-grained token scoped to one
    repository and the operations you meant.

- **Per-box HOME state** is a copy of the host agent's config, seeded once and
  never written back, with credential-shaped entries stripped at any depth
  (`credentials*`, `.netrc`, ssh keys, `*.pem`/`*.key`/`*.p12`), keeping only
  the runtime's own token, which it cannot function without.

### Secrets

Declared per profile, brokered host-side, injected for the life of one run:

```toml
[profile.review]
secrets = ["DEPLOY_KEY"]

[profile.review.secret.DEPLOY_KEY]
source = "env:H5I_SECRET_DEPLOY_KEY"   # the default for a bare name
inject = "env"                          # `file` is workspace-tier only in v1
```

The value never appears in the policy, the digest, or any receipt. `h5i box
secrets <name>` dry-runs the resolution and reports a fingerprint, never a
value. A grant that cannot be resolved fails the run closed rather than starting
a box that will fail confusingly later.

---

## Receipts

One append-only JSONL log per box, plus the raw payload of each record. A record
is generated from observation, never from the agent's account of itself.

Two properties the design depends on:

- **Append only, and sealed.** The box's write window under its own directory is
  exactly `<box>/spool`. The receipt log and the stored payloads are siblings of
  that spool, outside every grant. The box stages a record; the host ingests it.
  There is no path from inside to a record the host has already written.
- **Redacted at the boundary.** Secrets are scrubbed from the command and from
  the payload *before* either is written, and the scrub is recorded by rule id,
  never by value.

Every record carries the **lane** that observed it, so the two kinds of evidence
never blur:

| Lane | Who observed it |
|---|---|
| `host-env-run` | h5i, host-side. Exits and resource usage come from the supervisor's `wait4`; egress from the allowlist proxy's own log. |
| `viewer` | h5i's own viewer forward. The box supplies none of it. |
| `tee-shim` | The box's shell shim. Box-claimed. |
| `inbox-capture` | Staged by the box. Box-claimed. |

### What the browser saw

A run that drove the browser also carries what the page said back: console
errors, uncaught exceptions, and requests that failed. h5i collects these itself,
right after the command, in the same box under the same policy, so the timing
is not the agent's to choose. Only what is new since the last drain is recorded.

A browser command with **no browser to ask** is recorded as `unavailable`, not as
a clean page. "Nothing was looked at" is a different claim from "nothing was
wrong", and a reviewer has to be able to tell them apart.

### What we do not claim

An agent can stage *extra* records, or stop writing. Both are visible: a gap
between host-observed exits and box-reported commands is itself a finding. We do
not claim more than that.

---

## Limits

Being explicit about these is a feature, since the claim is a security claim.

- **The agent can send your source to the model API.** Containment stops the
  agent from touching the host. It does not stop it from putting private code in
  a prompt. That is a separate control (a self-hosted model, or no model egress
  at all) and we will not imply otherwise.
- **Shared kernel.** Podman and the kernel tiers share the host kernel. Good
  against a runaway agent and against careless dependency code. Not a claim
  against a targeted kernel exploit. A microVM backend is the answer, and it is
  not built.
- **The container tier's egress scoping is L7.** Its allowlist is a proxy, so it
  binds proxy-respecting tooling only.
- **An interactive session at a kernel tier shares your terminal.** `box shell`
  hands the box the terminal you launched it from, because that is what makes
  job control and every TUI work. A box shell is a nested shell, not a
  connection to somewhere else. A terminal is a two-way device, and the box gets
  both directions of it, so the residual is a list rather than a single door:
    - **Typing at your shell** (`TIOCSTI` pushes characters into the terminal's
      *input* queue, which your shell reads as if you had typed them, after the
      session ends). Whether that is closed is not h5i's to assert, and the two
      platforms answer from different places. On macOS it is the Seatbelt
      profile that subtracts the ioctl, so it holds at `process` and
      `supervised`, and **not** at `isolation=workspace`, which applies no
      profile by design, nor on a host whose Seatbelt is unusable. On Linux it
      is **your kernel's setting**, the same at every tier, since h5i does no
      ioctl filtering of its own there: 6.2 made TIOCSTI disableable via
      `CONFIG_LEGACY_TIOCSTI` and `dev.tty.legacy_tiocsti`, but upstream
      defaults that *open*. Many distros ship it closed, and a kernel older
      than 6.2 cannot close it at all. So h5i measures instead of claiming.
      `h5i box probe` prints one of:

              tty-injection= blocked at every tier
              tty-injection= blocked at the kernel tiers, possible at isolation=workspace
              tty-injection= possible at every tier

        and, when anything is open, how to close it.

    - **Reading what you type next.** The session's read grant on the terminal
      is not revoked when the shell exits, so a box process that outlives the
      session (a stray background job) can read the terminal it still holds
      open. This predates the tty ioctl grant; it is a property of sharing the
      device.
    - **Leaving the terminal in a state.** A box can set raw mode, turn echo
      off, change the line discipline, or take the terminal exclusive so other
      programs cannot open it. Recoverable (`stty sane`, or a new terminal), not
      an escape, but it is yours to recover.

    What is *not* reachable, checked rather than assumed: `TIOCCONS`
    (redirecting console output to the box's terminal) is refused by Darwin for
    a non-root process with or without a sandbox. Giving the box its own pty and
    proxying it is the fix that ends the whole list, and it is not built. The
    container and microVM tiers do not share a terminal at all.

- **Chrome runs with its own sandbox off.** On Linux, h5i's seccomp deny-list
  blocks the namespace syscalls Chrome's sandbox needs, at every tier. h5i's box
  is the boundary; Chrome's is not available inside it. That is one layer fewer
  than a browser on the host has.
- **A dev server the box runs is reachable only if its port is declared.** On
  macOS the box shares the *host's* loopback, so h5i denies outbound to it
  wholesale. Otherwise a box could reach a database or a dev server belonging
  to the host. A box that runs its own dev server and wants to point its own
  browser at it names the port: `[profile.X.net] loopback = [3000]`. Exactly
  that port is granted; everything else on loopback stays denied, and an
  undeclared port fails with `net::ERR_ACCESS_DENIED`. The port a declared
  `[service]` is running on is granted automatically while that service is
  alive, so this is only needed for a server started by hand. On Linux the box
  has its own network namespace and none of this applies.
- **On macOS the browser has no in-process domain check.** agent-browser cannot
  start Chrome from inside a Seatbelt sandbox. The failure reproduces under a
  fully permissive `sandbox-exec` profile and disappears without the sandbox, so
  it is not something a grant fixes. h5i therefore launches Chrome itself and
  attaches agent-browser to it with `--cdp`, which upstream refuses to combine
  with `--allowed-domains`. So that flag is not set for a macOS browser box: the
  tier's own egress enforcement is unchanged and still the boundary, but
  agent-browser's second, in-process domain list is gone. A page on a
  non-allowlisted host fails inside Chrome with `net::ERR_ACCESS_DENIED`.
- **A browser box's Chrome is restarted when its route out changes.** Chrome
  outlives the run that started it, and it takes its proxy address once, at
  launch, so a browser started before the box's current route (an upgrade, or a
  run whose proxy port moved) cannot reach the network through it. The box
  cannot restart it itself: a browser from a previous run is in a previous
  sandbox instance, which Seatbelt's same-sandbox signal grant does not reach.
  It is detected in the box and stopped host-side at the start of the next run,
  so the fix costs one extra run and says so rather than failing with a proxy
  error that reads like a page problem. The relaunch starts from a clean profile
  directory, so anything the old browser held (cookies, logins) is gone.
- **Two kernel mechanisms, not one.** Linux confines with Landlock, seccomp and
  namespaces. macOS confines with Seatbelt, which is default deny across
  filesystem, network, mach and sysctl in one policy, and which (unlike
  Landlock) can subtract a child from a granted parent, so the agent config lock
  is one rule there instead of a bind mount. That does not make `fs.deny`
  stronger on macOS: a denied path inside a granted parent is refused as a
  policy on every platform, so what is left is already outside every grant.
  Two things are genuinely absent on macOS: there is
  no syscall filter, because Darwin has no seccomp equivalent; and there is no
  memory or process-count cap, because Darwin has no cgroups, does not enforce
  `RLIMIT_AS` against an mmap'd heap, and scopes `RLIMIT_NPROC` to the whole
  user rather than to one box (applying it would cap your machine, not the
  box). `h5i box probe` names the mechanism and the gaps.
  Rootless Podman runs on Linux and WSL2 natively, and on macOS through a
  `podman machine` VM.
- **A macOS box shares the host's loopback.** A Linux box gets its own network
  namespace, so its loopback is private. macOS has no namespaces, so a box binds
  the host's loopback (deliberately: it is the only way a dev server in a box is
  reachable). h5i closes the outbound half of this, denying the box every
  outbound loopback destination except its own egress proxy, but the box's own
  listening ports are reachable by any local process.
- **Cost.** A Chrome sidecar is real RAM and CPU, even headless. Headless boxes
  stay first class, and the browser is opt-in per box.
- **The viewport is not a desktop.** CDP screencast shows the page. Native
  dialogs, browser chrome and anything outside the tab are invisible.
- **A dependency on the critical path.** `agent-browser` is someone else's
  release cadence. Pinned, CLI-boundary, forkable, but not ours.

---

## Files

| Path | What it is |
|---|---|
| `.h5i/env.toml` | Checked-in policy: profiles, services, container image. |
| `.git/.h5i/env/<agent>/<slug>/` | One box: its manifest, resolved policy, receipts, workspace. |
| `.git/.h5i/cache/<eco>/<key>/` | Warm dependency caches. |
| `~/.config/h5i/` | Host-side egress allowlist. Outside every box-granted path. |

---

## Environment variables

All optional; h5i ships with working defaults.

### Set by you

| Variable | Purpose |
|---|---|
| `H5I_AGENT` | Which runtime a box is scoped to (`claude`, `codex`). Decides the `agent` profile's credentials and egress. |
| `H5I_DEFAULT_ISOLATION` | Pin this clone's default tier when `--isolation` is not given. `--isolation auto` re-probes past it. |
| `H5I_SECRET_<NAME>` | Default source for a secret grant `<NAME>`. Injected for one run, redacted from evidence, audited by fingerprint. |
| `H5I_SKILL_DIR` | Where `h5i skill install` writes. |
| `H5I_CREDENTIAL_PROXY` | Turn the credential proxy off (`0`) for a box that must reach the model API directly. |
| `H5I_LOG` | `tracing_subscriber` filter for h5i's own diagnostics, e.g. `h5i_core=debug`. Goes to stderr. `RUST_LOG` is honoured as a fallback. |
| `H5I_NO_PROBE_CACHE` | Re-probe host capabilities instead of reusing the cached answer. |

### Set by h5i, inside a box

Read these to detect that you are in one; do not set them yourself.

| Variable | Meaning |
|---|---|
| `H5I_ENV_ID` | The box's id. Its presence is how the skill decides you are inside. |
| `H5I_ENV_POLICY_DIGEST` | The digest of the policy actually enforced. |
| `H5I_ENV_CAPTURE_SPOOL` | The box's only write window for staging receipt records. |
| `H5I_ENV_INBOX`, `H5I_ENV_BASE_TREE`, `H5I_ENV_AUDIT_CAPTURE` | Box plumbing. |

### Tests

| Variable | Purpose |
|---|---|
| `H5I_TEST_CONTAINER` | Opt in to the real-container integration tests (pulls an image, makes a live call). |
| `H5I_TEST_NET` | Opt in to the supervised egress allowlist end-to-end test (needs outbound network). |

---

## See also

- `h5i <command> --help`: the authoritative flag reference
- `man h5i`: the terse CLI reference
- [`skills/h5i/`](skills/h5i/): the agent-facing skill (`h5i skill show`)
- [`ROADMAP.md`](ROADMAP.md): what is built and what is not
- [`SECURITY.md`](SECURITY.md): reporting a vulnerability
