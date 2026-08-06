# h5i Manual

Command reference for h5i. **New here? Read [What h5i is](#what-h5i-is) and
[The loop](#the-loop) first** — they give the mental model before the
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

1. **A disposable workspace.** The code is copied into the box — a pull request,
   an existing repository, a fresh project — along with everything it pulls in.
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

- **denied egress attempts** — the box tried to reach hosts the policy refused
- **what ran** — every command, its lane, its exit code
- **what the browser saw** — console errors, uncaught exceptions and failed
  requests, observed by h5i rather than reported by the agent
- **viewer sessions** — including whether a human took the controls
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
- Mounted **read-only** into an agent box. That costs nothing in correctness —
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
you picked — the policy that was actually enforced, the services it declares,
its diffstat against the pinned base, and a flight recorder with one row per
receipt across five lanes (FS, NET, PROC, RES, PAGE). Click a row for the
rendered receipt, the same text `h5i box inspect` prints.

**It cannot drive anything.** Every route is a `GET`. `shell`, `run`, `export`,
`propose`, `apply` and `rm` stay in the CLI, where a human types them, so there
is no mutating surface to guard and no way to turn the console into a remote
control for someone's boxes.

**What guards it.** The server binds `127.0.0.1` and nothing else. The URL
carries a token minted for this session and kept in memory — never written to
disk, so no box can read it — which the page trades for a `SameSite=Strict`
cookie on first load. Requests from another origin are refused outright.

**What the colours mean.** Red is the only one that makes a claim about the
boundary: the egress allowlist *refused* a destination, host-observed by the
proxy. Amber is something to look at — a run exited non-zero, the wall-clock
limit killed one, or the in-box browser reported errors — and says nothing about
containment. Grey means the evidence is weak: either the tier is `workspace` and
nothing was confined, or every receipt came from the in-box shim and so is the
box's own account. Each run row is labelled `host-observed` or `box-claimed` for
the same reason. Nothing on the screen is a score.

The console is a default-on cargo feature. `cargo build --no-default-features`
drops it, along with axum, tokio and the build script's need for Node — and the
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
mode   = "deny"
egress = ["api.github.com"]
unix   = false          # AF_UNIX sockets; see below

[profile.review.resources]
mem   = "4G"
procs = 256
wall  = "30m"
```

### Isolation tiers

| Tier | What it is | Network scoping |
|---|---|---|
| `workspace` | No confinement; just a separate worktree. | none |
| `process` | Landlock + seccomp + namespaces, with a supervisor and a private pid namespace. | deny or host |
| `supervised` | Adds a private netns with an nftables egress allowlist pinned to resolved IPs, DNS pinned by a hosts file, and a seccomp-notify gate on `socket()`. | **L3/L4** |
| `container` | Rootless Podman: a portable image, with a CONNECT-proxy egress allowlist. | L7 |
| `microvm` | A hardware-isolated guest with its own kernel, booted by [microsandbox](https://microsandbox.dev) (`msb`) from the same OCI images. Egress rules are evaluated by the VM's network stack. | **L3/L4** |

`auto` (the default) picks the strongest tier the host can actually run. An
explicit tier **fails closed** if the host cannot satisfy it — h5i never
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
  repo-level `[container] image` — the same images the container tier runs,
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
by Landlock; and `/tmp` — where `.X11-unix`, `tmux-*` and an ssh-agent live — is
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

  ```toml
  [[profile.review.auth]]
  host           = "api.github.com"
  credential_env = "GITHUB_TOKEN"   # read on the host, never in the box
  base_url_var   = "GH_HOST"        # what the client reads
  ```

  The limit is real and worth knowing before you declare one: it binds clients
  you can point at another origin, so a plain `curl https://api.github.com`
  still goes nowhere. A TLS-terminating forward proxy would lift that, at the
  cost of a CA the box trusts, and it is deliberately not built.

  Restricting *what* the box may do with a credential is authorization, and it
  belongs where it is already solved: a fine-grained token scoped to one
  repository and the operations you meant.

- **Per-box HOME state** is a copy of the host agent's config, seeded once and
  never written back, with credential-shaped entries stripped at any depth
  (`credentials*`, `.netrc`, ssh keys, `*.pem`/`*.key`/`*.p12`) — keeping only
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
right after the command, in the same box under the same policy — so the timing
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
- **Chrome runs with its own sandbox off.** On Linux, h5i's seccomp deny-list
  blocks the namespace syscalls Chrome's sandbox needs, at every tier. h5i's box
  is the boundary; Chrome's is not available inside it. That is one layer fewer
  than a browser on the host has. The browser profile has not been exercised on
  macOS at all, so treat it as unsupported there rather than as working.
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

- `h5i <command> --help` — the authoritative flag reference
- `man h5i` — the terse CLI reference
- [`skills/h5i/`](skills/h5i/) — the agent-facing skill (`h5i skill show`)
- [`ROADMAP.md`](ROADMAP.md) — what is built and what is not
- [`SECURITY.md`](SECURITY.md) — reporting a vulnerability
