# ROADMAP: h5i as a contained agentic development environment

Status: in progress, 2026-08-05. Supersedes the "auditable workspaces /
provenance" positioning for the product surface. Design docs under `roadmap/`
stay as history for the parts we keep.

This document has five parts:

- **The environment**, sections 1 to 12. Scope, architecture, phases and the
  decisions behind them. Decisions already taken are in section 10; what is
  still open is in section 11.
- **The browser engine**, sections B1 to B15. The work on
  `crates/h5i-browser-light`, formerly `ROADMAP_BROWSER.md`. Those sections
  carry a `B` prefix so the two numbering schemes never collide. Section 12
  stays the authority on the engine's *scope and why*; the B sections are the
  authority on *order*.
- **Formal verification**, sections V1 to V6. A Lean 4 model of the policy
  layer beside the Rust, connected by differential testing, Cedar-style. M16
  is its milestone stub; the V sections are the authority on design and order.
- **The remote runner**, sections R1 to R13. Placing a box on a second Linux
  machine over SSH while the control plane, the repo, and the credentials stay
  local. M17 is its milestone stub; the R sections are the authority on design
  and order.
- **Runtime detection**, sections D1 to D14. An eBPF collector that watches a
  run from the kernel, so the receipt carries a lane that is neither at the
  boundary of the box nor inside it. M18 is its milestone stub; the D sections
  are the authority on design and order.
- **The forum**, sections T1 to T12. Mediated collaboration between boxed
  agents: they share information through a host-owned forum and never share
  authority. This is the product's second half — the first is one contained
  box, this is what happens when there are several — and the T sections are the
  authority on its design and order.

**M0 through M5 are built. M6 is mostly built. M7 (the terminal viewer) is
built but undriven.** What is not done, stated plainly so it is not read as
finished:

- The exit criteria for M4, M5 and M7 have none of them been demonstrated with
  a real agent or a real person in the loop. Every piece each of them needs is
  built and tested.
- The control lock is not enforced on the agent's side (section 11.1).
- `npx skills add` is unverified, for lack of a Node 22 runtime.
- There is no demo video.
- `/blog/` and `/pitch/` still argue the old positioning.

M7 earns a read for one thing beyond its own feature. It found that **every
human takeover through the web viewer had been silently doing nothing** since
M5, for the same reason two of M4's findings existed: a message the other side
never dispatches looks exactly like enforcement and enforces nothing (5.10.1).

---

## 1. The new one-liner

> Give coding agents full autonomy to build and test web apps inside a
> disposable environment, without exposing your machine or your credentials.

The product is a **contained agentic development environment**: a throwaway box
that holds the code, the agent, the toolchain, the dev server, and a real
browser, with nothing of the host inside it and nothing leaving it except a
reviewed patch.

PR review stays as the sharpest demo and the first buyer workflow ("run outside
or AI generated code safely"), but it is no longer the product boundary. A PR is
just one way to fill the box.

## 2. Five components

Everything we build maps to exactly one of these. Anything that maps to none is
out of scope.

1. **Disposable workspace.** The code is *copied* into the box: a PR, an
   existing repo, a fresh project, and every dependency it pulls. No host
   directory is mounted read write into the agent's reach.
2. **Sandboxed coding agent.** Claude Code or Codex, its child processes, MCP
   servers, package managers, builds, tests, and the dev server all run inside
   the same boundary. A runaway agent stays in the box.
3. **Credential and network broker.** No SSH key, GitHub token, model API key,
   cloud credential, Docker socket, or personal browser profile enters the box.
   A host side broker authenticates the calls the policy allows, and egress is
   an allowlist.
4. **Browser in the box, two interfaces.** Chrome and its profile live inside.
   The agent drives it through a CLI; the human watches the same viewport and
   can take over. The host browser never connects to the target app.
5. **Output gate.** At the end you export a patch, a report, screenshots, and an
   execution receipt, after inspection. The agent has no direct write path to
   the host.

The value is not any single one of these. It is that code, agent, dev server,
browser, and export sit inside one boundary that both the agent and the human
can operate.

## 3. Scope cut

### 3.1 What survives

- `crates/h5i-sandbox/` in full: `sandbox.rs`, `sandbox_policy.rs`,
  `container.rs`, `supervisor.rs`, `seccomp_notify.rs`, `cgroup.rs`,
  `secrets.rs`, `secrets_broker.rs`, `auth_proxy.rs`. This is the moat.
- `crates/h5i-core/src/env.rs` as the lifecycle engine, minus its couplings
  (see 3.3).
- The container profile model: `.h5i/env.toml`, isolation tiers, `net.egress`
  allowlist plus the host side CONNECT proxy, resource limits, env allowlist,
  the tee shim observation path.
- `containers/Containerfile.agent-{claude,codex}` as the base for the agent box.

### 3.2 What goes

Every subsystem that exists to record provenance rather than to contain
execution:

- CLI surfaces: `capture`, `recall` (log, blame, objects, search, context,
  memory), `audit`, `compliance`, `maturity`, `vibe`, `team`, `orchestra`,
  `msg`, `pr`, `notes`, `push`/`pull`/`share`, `serve`, `mcp`, `resume`,
  `status`, `doctor`, `migrate-remote`, `setup-remote`.
- Modules: `repository.rs`, `blame.rs`, `metadata.rs`, `ctx.rs`, `msg.rs`,
  `team.rs`, `memory.rs`, `prompt_score.rs`, `attention.rs`, `recap.rs`,
  `review.rs`, `risk.rs`, `rules.rs`, `compliance.rs`, `radio.rs`, `pr.rs`,
  `lfs.rs`, `session_log.rs`, `ui.rs`, `server.rs`, `vibe.rs`, `resume.rs`,
  `mcp.rs`.
- The MCP server in full, including the `h5i_env_*` tool family. 3.0k lines in
  `mcp.rs` plus its CLI entry point, and with it the `#![recursion_limit =
  "512"]` in `lib.rs`, which exists only for that file's `json!` literal.
- The whole `crates/h5i-orchestra/` crate.
- The embedded web dashboard `web/`, the `plugin/` directory, the `web` cargo
  feature, and the axum dependency.

  **Partly reversed, 2026-08-05.** The dashboard's twelve provenance views are
  gone for good, but its *Sandbox* view was the one screen that described boxes
  rather than commits, and losing it left no way to see the fleet at a glance.
  `web/`, the `web` feature and axum are back, scoped to that one screen and
  nothing else: `h5i ui`, read-only (every route is a GET), loopback-only and
  token-gated, built on manifests, the resolved policy, the env event log and
  `receipt.rs`. `risk.rs` is *not* back. The badges are arithmetic over
  receipts, so nothing on the screen is a score. See `crates/h5i-core/src/server.rs`.
- Git notes, `refs/h5i/{notes,context,memory,msg,team}`, and the sharing
  machinery over them.

Rough size: the workspace is ~95k lines of Rust today, of which `h5i-core` is
~63k and the root binary ~14k. The target after the cut is a binary plus two
crates in the 20k to 25k range.

### 3.3 The couplings that make this real work

`env.rs` is 9.2k lines and does not stand alone today. It reaches into:

| Coupling | Where | Replacement |
| --- | --- | --- |
| `crate::objects` evidence captures | run/shell capture, `ingest_shell_spool` | new `receipt.rs`: a box local, append only JSONL of commands, exits, timings, egress hosts, file diffs, with the same secret redaction |
| `crate::ctx` reasoning branch | `pin_worktree_context`, `fork_branch_no_switch`, context merge on apply | drop. The reasoning branch is a provenance feature, not a containment feature |
| `crate::msg` / `crate::team` | env inbox, `submit`, `submit_review`, `record_agent_reply` | drop with the orchestra |
| `crate::repository` git notes | `H5I_NOTES_REF` reads and writes on propose/apply/inspect | drop. Export carries the receipt as a file, not as a note |
| `crate::msg::sanitize_display` | terminal injection defense on untrusted strings | keep, move into a small `redact.rs` next to the receipt writer |

The mediated commit path validation in `env.rs` (canonicalized `$WORK`
allowlist, nested `.git` rejection, symlink escape rejection, gitlink round
trip) is not provenance. It **is** the output gate, and it moves to the export
path unchanged.

## 4. Command surface

```
h5i box .                    # snapshot the current repo into a box
h5i box <repo-url>           # clone an external repo into a box
h5i box --pr <N|url>         # review a PR in a box
h5i box --new                # empty box, agent builds from zero

h5i box ls | status <name> | rm <name> | gc
h5i box shell <name>         # attach a confined interactive session
h5i box run <name> -- <cmd>  # one policy enforced command
h5i box view <name>          # open the human viewer for this box
h5i box export <name>        # inspect and emit patch, report, receipt
h5i box probe                # host capability report
h5i box allow <host>         # persistent egress allowlist entry
h5i box cache ls|refresh|rm  # per project warm dependency caches

h5i browser status|take|release   # the control lock, and who holds it
h5i browser url                  # the viewer URL for this box

h5i skill install|show|path      # write or print the embedded agent skill

h5i ui                           # the box console: the whole fleet, read-only
```

`h5i dev *` and `h5i env *` stay as hidden aliases through one release, then
are removed. The command is `box` because that is the noun everything else uses.

The non-interactive lifecycle verbs (create, run, export, ls, status, and the
rest of the reporting set) take `--json` and emit a stable envelope on stdout,
human notes on stderr: that contract is the programmable surface an SDK would
wrap, and it is specified in 6.2.

**Driving the browser is `agent-browser`, not `h5i`** (7.). Inside the box the
agent runs it directly:

```bash
agent-browser open http://localhost:3000
agent-browser snapshot                     # accessibility tree with @refs
agent-browser click @e2
agent-browser fill @e3 "test@example.com"
agent-browser screenshot shot.png
```

h5i's own browser surface is deliberately three verbs: the control lock, and
the viewer URL. Wrapping forty automation verbs would buy nothing but drift.

## 5. Architecture

### 5.1 Workspace: copy in, not mount

Today `$WORK` is a real git worktree on the host, bind mounted into the
container at `/work`. That is convenient and it is the wrong boundary for this
product: a container escape or a careless `--mount` reaches host files, and the
worktree keeps the host repo's `.git` in the blast radius.

Target: the box gets a **copy**. Sources are a `git archive` of the working
tree, a clone of a URL, a fetched `refs/pull/<n>/head`, or nothing. The copy
lives in a container volume owned by the box. The host repo is opened read only
at creation time, and never again during the run.

Consequences to design for:

- Round trip is by patch, not by shared inode. Export writes a patch the host
  applies. Nothing writes into the host repo without a human step.
- Dependency install happens in the box, once, and the box keeps its own package
  caches. Warm caches across boxes are a committed feature, designed in 5.8.
- The kernel tiers (`process`, `supervised`) keep the worktree backend, since
  they have no volume abstraction. `container` becomes the default tier for
  `h5i box`, and the copy in model is container only at first.

### 5.2 Where the browser runs

The first draft of this section put the browser in a second container and
reached for a Podman **pod** to share a network namespace with the agent. That
was the wrong starting point, for two reasons found while planning M4.

**One: the supervised tier already has stronger network scoping than the
container tier.** `supervised` puts the box in a private network namespace and
enforces `net.egress` with **nftables rules pinned to resolved IPs**, DNS
pinned by a hosts file, and a seccomp-notify gate on `socket()`. That is L3/L4:
a program that ignores proxy settings still cannot reach an off-list address.
The container tier's allowlist is an HTTP/HTTPS CONNECT proxy, which only binds
proxy-respecting tooling. The container tier buys **portability**, not tighter
network control, and this document said the opposite for two drafts.

**Two: one box is simpler than two.** Put the browser in the *same* box as the
agent and the dev server on `localhost:3000` is reachable with no netns
sharing, no pod, no port publishing and no second image. At the kernel tiers
the "image" is the host filesystem under Landlock grants, so the browser is a
profile change: grant Chrome and the agent-browser binary, and launch the
daemon inside the box.

So M4 targets a single supervised box:

```
box (supervised)   private netns, nftables egress allowlist, Landlock, seccomp
├── the agent, its builds and tests, the dev server on localhost:3000
├── headless Chrome (no X: --headless=new)
└── the agent-browser daemon, driven over the CLI
```

The container tier is the **portability** path, and once the browser lives in
the same box it costs almost nothing: an image with Chrome and agent-browser
(`containers/Containerfile.browser`), a `/dev/shm` big enough for a renderer,
and the host-path grants skipped because the image provides them. All three are
built. The pod split was the expensive part, and dropping it took the cost with
it.

One thing this costs, and it should be stated: our seccomp deny-list blocks the
namespace syscalls Chrome's own sandbox needs, so Chrome runs with
`--no-sandbox` inside the box. h5i's box is the boundary, not Chrome's; the
same is true under rootless Podman. It is a real reduction in defence in depth
and it belongs in the limits section rather than in a footnote.

### 5.3 Browser control

The automation itself is **agent-browser** (7.), running inside the `browser`
container. h5i does not reimplement clicking. What h5i owns is everything
around it:

- The daemon and its CDP port stay inside the box's network namespace. Nothing
  is published to the host; the human viewer reaches the stream through an
  h5i-owned forward with a per box token (5.9).
- Chrome runs with a **fresh profile created in the box**. No host profile, no
  host cookie jar, no host extension, no host history.
- Chrome's egress is the box's egress: at `supervised` that is the nftables
  allowlist, which needs no cooperation from Chrome at all. Loopback stays open
  so the dev server is always reachable. agent-browser's own
  `--allowed-domains` is set from the same policy as a second, in-process
  layer.
- **AI features off.** agent-browser's `chat` and the dashboard's AI panel send
  page content to an external gateway. In a box that is an exfiltration path
  with a friendly name, so `AI_GATEWAY_API_KEY` is never injected and `chat` is
  refused by policy.
- Downloads, uploads, and clipboard resolve inside the box. A download lands in
  `/work/.h5i/downloads` and is subject to the export gate like any other file.
- Every browser command, plus console and network errors, lands in the receipt.

### 5.4 Control lock

Neither agent-browser nor CDP arbitrates between two clients: the agent's CLI
session and a human typing into the stream can both dispatch input at the same
moment, and the result is a mess neither of them can reason about. The lock is
h5i's, and Neko's `request / release / take / give / reset` is the semantic
model we copy.

- The agent holds control by default.
- A human interaction in the viewer takes control. Automation pauses at the next
  command boundary, and the next agent browser command returns a typed "control
  held by human" error rather than fighting for the pointer.
- On release, the agent must re snapshot before acting, because the DOM it
  remembers is stale.
- Exactly one automation client per box. Multi agent shared *control* is out of
  scope: two clients steering one browser is a race with no arbiter, and the
  control lock exists precisely so it cannot happen. This says nothing about
  several boxes coordinating — see part T, whose whole design is that agents
  exchange information and never share a driver, a credential, or a grant.

### 5.5 Credentials

- Model API: the key stays on the host. `auth_proxy.rs` already injects it into
  outbound requests from the box, scoped per runtime, so a Claude box cannot
  reach the OpenAI credential or vice versa. Keep, make it default on rather
  than opt in.
- **Any other service: the same mechanism, generalized.** An earlier draft of
  this section proposed a GitHub "capability helper": a host side process
  serving a fixed verb set (fetch a PR head, read issue text, open a draft
  PR). That is the wrong shape for this repository. It overfits h5i to one
  vendor, and the next request is GitLab, then Jira, then whatever else, each
  adding vendor code to a tool whose job is the boundary.

  The general primitive is the one already here: a **host side proxy that
  injects a credential for an allowlisted host and never lets the box hold
  it**. Generalize `auth_proxy.rs` from "the model API" to "any host named in
  the profile, with a credential resolved host side", and GitHub becomes a
  policy entry rather than a feature:

  ```toml
  [profile.review.net]
  egress = ["api.github.com"]
  [profile.review.auth."api.github.com"]
  header = "Authorization: Bearer ${GITHUB_TOKEN}"   # resolved on the host
  ```

  **The shape this has to take is a decision, not a detail.** `auth_proxy.rs`
  today is a *reverse* proxy: the box is handed a base-URL override
  (`ANTHROPIC_BASE_URL`) pointing at a loopback listener that injects the real
  credential and forwards to one pinned upstream host. Generalizing it has two
  candidate shapes, and they are not equivalent:

  1. **Reverse proxy per grant** (small, honest, limited). The profile names a
     host, the env var holding the credential host side, and the base-URL
     variable the client respects. Nothing new is invented: it is the existing
     mechanism with the hard-coded runtime table replaced by profile data. The
     limit is real and must be stated: it only works for clients you can point
     at a different origin, so `curl https://api.github.com` still goes nowhere.
  2. **Forward proxy with header injection** (general, expensive). `HTTPS_PROXY`
     plus per-host injection means terminating TLS in the proxy, which means a
     CA the box trusts. That is a substantially larger security surface: a box
     that trusts an h5i CA is a box whose TLS you have taken responsibility for.

  Option 1 is the one to build first, precisely because its limit is legible.
  Option 2 should not be reached for until something concrete needs it, and
  when it is, it needs its own design note rather than an afternoon.

  Restricting *what* the box may do with that credential is authorization, and
  it belongs where it is already solved: a fine-grained token, scoped to one
  repository and the operations you meant. If a generic rule is wanted later it
  is a method/path condition on the proxy, still policy data, not vendor code.
  Vendor ergonomics (a friendly `gh`-shaped CLI) belong in a separate tool.

  Worth noting how little is left after that: `h5i box <pr>` already fetches
  the PR head **host side, before the box exists**, so the demo workflow needs
  no credential in the box at all.
- Everything else routes through `secrets_broker.rs` with a per grant record.
- Per env HOME state already exists (`prepare_home_state`, `policy.home_binds`)
  and is a copy of the host agent config, seeded once, never written back.
  Audit what that copy actually carries and strip anything credential shaped
  from the seed rather than trusting the copy.

### 5.6 Output gate

`h5i box export` produces, after showing a summary and asking:

- `patch.diff`: the tree diff, path allowlisted to `$WORK`, no nested `.git`, no
  symlink escapes, no agent introduced gitlinks. Reuses today's mediated commit
  validation.
- `report.md`: what the agent says it did.
- `screenshots/`: what the browser actually rendered.
- `receipt.json`: commands with exit codes and wall or CPU or RSS, egress hosts
  contacted and which were denied, files touched, policy digest, image digest,
  and the tier that was actually enforced.

Secret redaction and size caps apply to all four. The receipt is the honest
part of the export: it is generated from observation, not from the agent's
account of itself.

### 5.7 Receipt integrity

The receipt is generated **inside** the box, by the shell shim and the browser
daemon. That is acceptable, and the only property that has to hold is that the
agent cannot rewrite what has already been recorded.

Today that property comes from **sealing, not from an fd**, and it is already
true:

- The box's write window under its env directory is exactly `<env>/spool`. The
  receipt log (`<env>/receipt.jsonl`) and the stored payloads
  (`<env>/receipts/`) are siblings of that spool, outside every grant. The box
  stages a record; the host ingests it. There is no path from inside to a
  record the host has already written. (`prepare_env_capture_spool`, pinned by
  a unit test and an integration test.)
- Ordering and timestamps are the host's, applied at ingest.
- Two fields are host observed and never taken from the box: process exits and
  resource usage come from the supervisor's `wait4`, and egress comes from the
  allowlist proxy's own log.
- Every record carries the **lane** that observed it (`host-env-run` is host
  observed; `tee-shim` and `inbox-capture` are box claimed), so the two kinds
  of evidence never blur together.

What an agent can still do is stage *extra* records, or stop writing. Both are
visible: a gap between host observed exits and box reported commands is itself
a receipt finding. We do not claim more than that, and the docs say it in these
terms.

An inherited-fd writer (records streamed to a host collector over a pipe, no
file in the box at all) remains the stronger design, because it also removes
the staging window. It is worth doing when the container tier's spool becomes
the common path, not before: the sealing argument above already covers the
"cannot rewrite" half, which is the half that matters for trust.

### 5.8 Warm caches

Cold dependency install is the difference between a 20 second box and a four
minute box, so caches are in scope rather than deferred.

- One cache per project and ecosystem, keyed by a digest of that ecosystem's
  lockfile set, under `.git/.h5i/cache/<eco>/<key>/`. **Built** (`h5i box cache
  ls|mounts|rm`, `crates/h5i-core/src/cache.rs`, unit tested). A cache whose
  key no longer matches the project is listed as stale and never handed to a
  box: packages resolved for a different dependency set are a silent, hard to
  explain wrong answer.
- Mounted **read only** into the agent box at the ecosystem's cache path
  (`~/.cargo/registry`, `~/.npm`, `~/.cache/uv`, …). A read only cache is a
  correctness problem for nothing: every package manager falls back to fetching
  what it cannot find. **Built**: `ResolvedPolicy::ro_binds` (runtime-only, never
  serialized, so it cannot move a pinned digest) is applied as `MS_BIND` then
  `MS_REMOUNT | MS_RDONLY` on the kernel tiers and as `--mount ...,ro` at the
  container tier. `h5i box cache mounts` prints exactly what a box would get.
- Writing to a cache happens only in a dedicated `h5i box cache refresh` box,
  which runs the install step alone, with egress narrowed to the registry hosts
  and no agent inside it. The cache is populated by a build, never by an agent
  session. **Built**: `ResolvedPolicy::cache_write` is a single optional
  writable bind, produced only by `h5i box cache refresh` and reachable from no
  profile, so an agent box cannot make its own cache writable. The bind targets
  the same path the read-only mount later exposes, so what is fetched is
  exactly what a later box sees, and the throwaway box is removed whether the
  fetch succeeded or not.

  One thing refresh cannot do for you: no built-in profile fits it. `default`
  denies network (it is the build/test profile) and the agent profiles grant a
  model API instead, so a refresh box needs a project-declared profile whose
  egress is the registry hosts and nothing else. `refresh` refuses with that
  profile written out, ready to paste, rather than creating a box whose fetch
  could not have worked.
- `h5i box cache ls|refresh|rm` are the whole surface. A box records which cache
  volume and which digest it used, in the receipt.

This keeps the property that matters: no mutable surface is shared between an
agent box and anything else.

### 5.9 The viewer forward

agent-browser's stream server assumes a friendly localhost: connect to the
WebSocket and you can both watch and type. Inside a pod that is fine, because
nothing else is in the pod. It is not fine on a developer machine with a
browser on it.

So the port is never published. `h5i box view` starts a small forward the host
owns. It reaches into the box's private network namespace the same way the
supervisor already does (h5i is the parent process and holds the pid), rather
than by opening a hole in the netns:

- It binds `127.0.0.1` only, on a port h5i chose, and prints the URL.
- Every connection presents a **per box token**, minted at box creation and
  never written into the box.
- It refuses cross origin WebSocket handshakes, so a page the human happens to
  have open cannot reach into a running box.
- It enforces the control lock (5.4) on the input direction: frames flow out
  always, input flows in only for the holder.

That is the whole trusted surface between the human and the box, and it is
about as small as this can be made.

### 5.10 The terminal viewer

**Built** (`crates/h5i-core/src/termview/`, `h5i box view --term`). The web
viewer reaches the human through their host browser, which leaves one awkward
beat in the story: everything runs in the box, except the watching, which
happens in the most credential-laden program on the host. The terminal viewer
closes that beat. It renders the boxed viewport in the terminal itself, in a
split pane next to the agent, and over SSH when the box host is remote. It is
also the demo the launch needs: one recording showing agent, dev server, and
boxed browser in a single terminal frame.

The line for the story is: **the browser is untrusted, the terminal is the
trusted path.**

**It is not a client of the forward, and that changed during the build.** The
first plan had the TUI connect to `h5i box view` over loopback with the per-box
token. That is a listener and a credential bought for nothing: the viewer runs
in the same process as the CLI the human typed, so it can do what the forward
does: fork, enter the box's user and network namespaces by pid, connect, and
take the socket back over `SCM_RIGHTS`. So `--term` binds no port, mints no
token, and serves no page. There is nothing for another local process to
connect to, so there is nothing to authenticate. The forward keeps its token
because it must listen; this does not.

What it is made of, and what each part is for:

- **`ws.rs`**: a WebSocket client, roughly the RFC 6455 subset one connection
  to one server needs. Everything the box sends is untrusted: reserved opcodes
  and reserved bits are refused, a masked server frame is refused, lengths are
  capped before they become allocations, and fragmented messages cannot grow
  past the cap across frames.
- **`proto.rs`**: the stream's messages. Pinned to what agent-browser actually
  dispatches (`input_mouse` / `input_keyboard` / `input_touch`) rather than to
  what the DOM calls them, for reasons in the bug note below.
- **`image.rs`**: `zune-jpeg`, which forbids unsafe code, with dimensions
  capped before decode. Frames are scaled to the pixel size they will actually
  be displayed at, because every byte crosses a PTY and over SSH that is the
  whole cost of the viewer.
- **`kitty.rs`**: the graphics protocol, generated **by the viewer and only by
  the viewer**. `q=2` on every render command, so the terminal's replies never
  land in the middle of the keystrokes being translated into page input. Direct
  transmission only: the file and shared-memory mediums are faster and only
  work when the terminal is on this machine.
- **`input.rs`**: terminal bytes to CDP events, including the two places a
  terminal and a browser genuinely disagree: a terminal reports presses with no
  releases (so the pair is synthesized, and press-and-hold does not work), and
  it reports cells rather than pixels (so clicks map through the placement, at
  cell resolution).
- **`status.rs`**: the row the page cannot reach.
- **`term.rs`**: raw mode, alternate screen, mouse and bracketed paste, all
  behind an RAII guard that restores on every path out.

Three properties worth stating plainly:

- **The viewer generates every escape sequence.** The box supplies compressed
  pixels inside a WebSocket message and nothing else. Terminal output is an
  escape surface (OSC 52 clipboard writes, title and hyperlink control, the
  graphics protocol's own file-reading mediums, parser bugs), and no byte from
  the box reaches the PTY. This is `sanitize_display` applied to pixels instead
  of strings.
- **A trusted status line.** Row one is the viewer's: box, mode, lock holder,
  origin, egress, console errors. A page cannot draw there, and it cannot be
  clicked through into the page either. The origin is sanitized on the way in
  (escape sequences *and* bidi overrides, which needed a fix in `redact.rs`, since
  they are not control characters and the existing pass let them through) and
  it is never the field that gets truncated: a URL too long for the row loses
  its path, and an origin too long for the row is cut from the **left**, since
  shortening `bank.example.evil.test` from the right is the spoof itself.
- **Two modes, because a terminal makes them natural.** VIEW is read-only and
  leaves the mouse to the terminal, so selection and scrollback still work.
  INTERACT takes the control lock: reaching for the controls *is* taking them,
  which is the lock's own rule and the only sensible one here, since the
  terminal is busy being the viewer and there is no other window to run
  `h5i browser take` in. `Ctrl-]` is reserved to get back out, because raw mode
  hands the viewer every other key.

**Still open, and deliberately not built yet.** LOGIN mode, withholding frames
and snapshots from the agent while a human types a credential, rests on the
agent-side enforcement decision in 11.1, and shipping it as advisory would
overstate it. Pixel-resolution mouse reporting (`?1016`) would place clicks
better, but a terminal that does not support it keeps reporting cells with no
way to tell, which is the quiet-wrong-answer shape this codebase keeps getting
bitten by. The file and shared-memory transmission mediums are the local
fast path. tmux passthrough is untested.

And the claim, at its real size: this shrinks the TCB of *watching*, it does
not add a boundary. "The box cannot send escape sequences to your terminal"
already held for the web viewer, because the box cannot reach the PTY at all.
The delta is that a small Rust module plus a memory-safe JPEG decoder replaces
a host Chrome tab as the thing doing the watching, plus the status line and the
mode model that only a terminal makes possible. The stronger "entirely
untrusted guest" framing waits for the microVM backend, like every other claim
of that shape.

**terminal-browser (zenbu-labs, MIT) is the reference, not the base.** Its
architecture runs Chromium *on the host* (Electron offscreen rendering, a
native input helper, macOS only today), which is the trust inversion of ours,
and its hard problems are the ones h5i has already solved on the other side of
the boundary. What we took is the Kitty graphics rendering technique and the UX
patterns; what we did not take is Electron on the host, a host Chromium, or an
input helper.

#### 5.10.1 The bug this work found in the web viewer

`viewer.html` sent `mousedown`, `keydown`, `wheel`, the DOM event names.
agent-browser's stream server dispatches on `input_mouse`, `input_keyboard` and
`input_touch`, and falls through to `_ => {}` for everything else. So **every
human takeover through the web viewer was a no-op**, and silently: the socket
stayed healthy, frames kept arriving, and the forward counted the input frames
as forwarded. The receipt would have recorded "a human drove this box" for a
session in which nothing a human did reached the page.

M5 verified the *gate* (input dropped without the lock, forwarded with it) and
that is exactly what it verified; nothing checked that a forwarded frame moved
anything. It is the same class as the M4 findings: a variable the tool never
reads, a message the server never dispatches. Both look like enforcement and
enforce nothing.

Fixed, with the correct message names, CDP's string button names, a
`clickCount` (a press with zero is not a click as far as Chrome is concerned),
and `text` omitted rather than nulled on key-up. Pinned by a test that reads the
page and refuses a DOM event name. The stale control indicator was fixed at the
same time and for a related reason: with input working, a display permanently
reading "agent" would tell someone who had just taken the lock that it had
failed. There is no channel to push updates on, since the stream is a straight relay,
so the holder is stamped into the page at serve time and the page says that
is what it is.

### 5.11 Share: the first inbound path (built, 2026-08-10)

Everything else in this document is about what leaves the box. Share is about
what comes in: a second person, on their own machine, trying the web app the
agent built while it still runs inside the box. The demand is the ngrok use
case ("here, click around") without the part where a tunnel URL quietly
exposes a dev server that was never meant to face the internet, and without
standing up an account, a domain, or a server of ours.

**Port sharing, not viewer sharing, and that is a use-case decision.** Two
shapes were on the table. Sharing the *viewer*, the agent-browser stream of
5.9 carried over the network instead of loopback, reuses the forward and the
terminal viewer almost whole, but it ships pixels: one viewport, one control
lock, no independent navigation, no devtools on the other end, no feel for the
app's own latency. Sharing the *port* puts the real app in the other person's
own browser, which is what "try it" means. The viewer share is a different
feature (a joint review session), not a cheaper version of this one, so it is
not a prerequisite and does not gate this; if it lands later, it lands on the
same bridge.

**The bridge is the feature; transports are plugins under it.** `h5i box
share` starts a host-side process with three jobs, none of which depend on how
the bytes travel:

- **Reach the dev server.** The box's port is never published. The bridge
  enters the box's network namespace by pid and dials loopback per connection,
  exactly the seam the viewer forward and the terminal viewer already use
  (5.9, 5.10). Nothing inside the box learns *who* is visiting: a quick tunnel
  hands its origin `Cf-Connecting-Ip`, `Cf-Ipcountry` and `X-Forwarded-For`,
  and the gate drops every one of them before the box sees a byte, because a
  person who clicked a link agreed to look at a page and not to identify
  themselves to somebody else's agent. What the box can tell is that it is
  behind a proxy: `Host` and `X-Forwarded-Proto` stay, because a dev server
  builds its URLs out of them and a share that broke every link on the page
  would not get used. The netns
  gains no hole, and the box's egress policy is untouched: the bridge is a
  host process, outside the boundary, like the CONNECT proxy.
- **Hold the capability.** A ticket minted at share time is the whole access
  model: it names the box, the port, an expiry, and a secret; possession is
  authorization, and possession is all of it: a ticket is a bearer capability,
  so forwarding the text admits everyone it reaches under the one grant.
  Measured, because this line used to claim the opposite: two `h5i join`
  processes on one ticket both reached the dev server and both appear in the
  receipt against the same grant. Mint one ticket per person if you want
  `h5i box share revoke` to cut off one person rather than all of them; `stop`
  ends the session for everyone. No account on either side. (As shipped, minting a
  second ticket works on `--tunnel` shares only; see 5.11.1.)
- **Write the ingress receipt.** Every lane in 5.7 observes egress. This is
  the first inbound evidence: peer, connection times, requests proxied, bytes,
  and the transport actually used (direct, relayed, tunnel), in the same
  receipt the export already carries. A share session that left no record
  would be the one part of a box's life the receipt is silent about, which is
  exactly the kind of gap this document exists to refuse.
- **Measure time with a clock nobody can move.** Ticket expiry and the
  session length are elapsed time, not wall-clock subtraction. A backward NTP
  step was measured putting an hour back onto every live grant and writing a
  receipt that read `0s` for a two-minute session with `closed` before
  `opened`. The timestamps in a receipt are still clock readings and can be
  wrong; the receipt says so on a `clock` line when they are, because an
  evidence artifact that quietly clamps an absurdity to a plausible number is
  worse than one that admits it.

**Transport one: iroh.** Peer-to-peer QUIC, end-to-end encrypted, NAT
traversal with public relays as fallback for the hard cases; the relay sees
addresses and volume, never plaintext. The ticket carries the node addressing,
so there is nothing to configure. `--direct-only` refuses to move application
bytes over a relay: a peer that cannot get a direct path is turned away, and the
share stays up for anyone who can.
The other end runs `h5i join <ticket>`, which terminates the QUIC connection
and serves the app on the joiner's loopback, and that listener repeats 5.9's
lesson on someone else's machine: a bare local port is reachable by every page
and process the joiner has open, so the local URL carries a token and the
proxy refuses without it. iroh is a real dependency tree (QUIC, TLS), so it
is a cargo feature in the `web` pattern: default on, and a build without it
loses `share`/`join` and nothing else.

**Transport two: Cloudflare quick tunnel, because the joiner may not be a
developer.** P2P requires `h5i` on both ends, and the person you most want
clicking the prototype (a designer, a PM, a customer) will not install a
CLI. `h5i box share --tunnel` shells out to `cloudflared` and hands back a
plain URL any browser opens. The same bridge still fronts it: the URL embeds
the ticket token, the bridge checks it and the expiry on every request, and
revocation still works mid-session. The capability degrades from "hold the
secret" to "hold the link", not to nothing. The honest costs, which the docs
must state rather than blur: TLS terminates at Cloudflare, so this mode is
not end-to-end and Cloudflare can read the traffic; `cloudflared` is an
external binary we neither pin nor ship; and quick tunnels are explicitly not
a production service (concurrency caps, no SSE). It is the no-install mode,
not the default mode.

**What the joiner is exposed to, stated up front.** The app being shared is
agent-written, untrusted code, and port sharing renders it in the joiner's own
browser. That is the point, and it is also the exposure, the same one as
clicking any link a colleague sends. One asymmetry is worth writing down: in
P2P mode the app is served from the joiner's loopback, and a loopback origin
is exempt from the browser's private-network protections, so a hostile page
has an easier path at the joiner's own local services than the same page on a
public origin would. Tunnel mode, ironically, keeps those protections, because
the origin is public. `h5i join --isolated`, opening the proxy in a box of the
joiner's own, is the strong answer for a joiner who has h5i anyway; it should
exist, and it should not be pretended that the no-install audience will use
it.

**What this is not.** Not a deployment path: sessions are bounded by the
ticket's expiry and die with the bridge. Not a relay business: the public iroh
relays are someone else's rate-limited infrastructure, fine for fallback and
for measuring how often fallback actually happens, and running or selling
relay capacity is a SaaS with an abuse desk attached, out of scope by the
same decision that says no server (10.). And not the old `share`: 3.2 deleted
a `push`/`pull`/`share` that moved git notes between repositories; this one is
`h5i box share`, on the box noun, and the collision ends there.

The surface, as built:

```
h5i box share <name> [--port 3000] [--expire 60m] [--label alex]
h5i box share <name> --direct-only               # refuse relayed app bytes
h5i box share <name> --tunnel                    # cloudflared; plain URL
h5i box share ls|status|grant|revoke|stop
h5i join <ticket> [--port N]                     # the other machine
```

#### 5.11.1 What shipped, and what it cost to be honest about

`crates/h5i-share/`, ~12k lines with 187 tests, behind a default-on `share`
feature on the binary and a default-on `p2p` feature inside the crate (iroh 1.0,
`tls-ring` only). A `--no-default-features` build has no `share` verb rather
than a broken one, and `--no-default-features` on the crate alone keeps the
tunnel transport with no QUIC stack compiled in.

Four decisions made during the build that the proposal above did not contain:

- **The fork into the box happens once, at startup, and the helper stays.** The
  viewer forward forks per connection, which is fine for one WebSocket and wrong
  for a share: a share runs an async runtime, and `fork()` in a process with a
  thread pool inherits one thread plus whatever locks the others held. So
  `Dialer::spawn` runs while the process is still single-threaded and keeps a
  helper alive in the box's namespaces, answering a one-byte "connect me" over a
  socketpair. Belt and braces on top: everything below the fork is
  allocation-free (stack-built `/proc/<pid>/ns/…`, `SocketAddr` rather than
  `(&str, u16)`), so a caller who ignores the ordering rule gets a helper that
  cannot deadlock rather than one that does so occasionally.
- **A box with no network of its own is refused, not shared.** Without one,
  "the box's port 3000" and "this machine's port 3000" are the same port, and
  sharing it would publish whatever happened to be listening. This is the one
  refusal in the feature that exists purely because the alternative is a silent
  wrong answer. The condition checked is "is there a process of this box in a
  network namespace of its own", not a list of tiers: a `process`-tier box gets
  one when its profile denies egress and shares the host's when it does not, so
  a tier list would be advice that is wrong half the time. The message names
  which of the two things is missing: no session, or no network.
- **Authorization is per connection, read from disk, and revocation has a
  watchdog.** `share revoke` runs in a different process, so a cached grant table
  would be a revoke that appeared to work. On the P2P path it is per *stream*,
  which means one TCP connection into the box: a revoke stops the next one. For
  the connections already open, a one-second watchdog closes them. Without that
  second half, revoking would work on everyone except the person actually there.
- **A share carries at most 64 connections into the box.** Refused rather than
  queued, because a queue turns a flood into latency for the person who is
  legitimately using the share and hides that anything happened; and answered
  with a `503`, not a `401`, because "your link is bad" is the wrong thing to
  tell someone whose link is fine. The count goes in the receipt on its own
  line, so load and credential failures never read as each other.
- **One connection carries one request, because connection pools are shared.**
  Both HTTP fronts gate a connection when its first request arrives, which is
  equivalent to gating every request only if a connection cannot carry a second
  one. It can: `cloudflared` pools connections to the origin and reuses them for
  the next request from *any* visitor, and browsers pool per origin the same
  way, so an unauthorized request could ride in on a connection someone else's
  credential opened.

  The first version of this sent `Connection: close` upstream and called it
  enforcement. It was not: the box runs agent-written code, and a dev server
  that answers keep-alive leaves the front holding an ungated pipe. The second
  review caught exactly that, so the front now reads the head plus the declared
  body and then **stops reading the client**, which needs nothing from the box.
  Two things fall out. A chunked request body has to be *parsed* rather than
  just copied, because forwarding one request means knowing where it ends and a
  chunk stream only says so in its own framing. It was refused with a `501`
  for two rounds, which meant no streamed upload worked at all. And an upgrade
  earns its two-way pipe only after the box answers `101`,
  with the request required to carry both `Upgrade` and `Connection: upgrade`.
  A lone `Upgrade:` header is something any client can attach to a request that
  will never upgrade, and it was an opt-out from the whole rule.

  A third round found the other half: the *response* was relayed untouched, so a
  box answering keep-alive told the visitor's browser to reuse a connection this
  proxy would never read again: an intermittent hang, and a `502` for every
  POST a client will not retry. The response head is rewritten to `close` now
  and framed by its `Content-Length`. Security intact throughout; liveness had
  been traded away silently.

  All three rounds are worth recording together: nothing in the suite would have
  caught any of them, because nothing in the suite pools connections. The tests
  that pin them run against a dev server written specifically to ignore
  `Connection: close`.

- **Per-port cookies fixed one leak and opened another.** Naming the joiner's
  cookie after its port stopped two `h5i join` sessions logging each other out.
  It also meant a *second* share's credential was, from any given front's point
  of view, just another cookie, and cookies ignore the port, so the browser
  sent both to both, and each front dutifully forwarded the other's to
  agent-written code. Reading our own cookie by exact name and dropping every
  cookie whose name starts with the share prefix are two different rules, and
  the difference was the one property the gate exists for. The test that had
  been written for the first fix asserted the leak as correct behaviour, which
  is the more useful lesson: a test can pin a bug as a feature.
- **Revocation is per grant all the way down.** The watchdogs first asked
  whether the *share* was spent, which is true only when no grant admits
  anybody, so revoking one peer while another was still connected left the
  revoked peer's open streams running, and the CLI printed "any connection that
  peer had is dropped within a second" while that was false. Each connection now
  watches the grant that admitted it. Same class of thing as `--direct-only`,
  which was checked once at setup and never again: a direct path can die and
  iroh will fall back to a relay, so a promise checked once is a preference.
  Both are polled for the life of the connection now.
- **Streams are served concurrently, and that was a real bug first.** The first
  cut awaited each stream to completion before accepting the next, which
  serialises every share behind whichever connection is longest-lived, for a
  dev server, the hot-reload socket that never ends. Found by the in-process
  end-to-end test hanging, which is the argument for having written it.

**Not built, deliberately.** `h5i join --isolated` (opening the shared page in a
box of the joiner's own) is designed in this section and has no implementation;
the warning at join time is what stands in for it today. Viewer sharing is not
built and was never in this milestone.

**Not built, and it is a gap rather than a choice.** `h5i box share grant` mints
a second ticket for a *tunnel* share only. A P2P ticket needs the running
endpoint's addressing, and only the serving process has it, so the verb refuses
rather than handing out a ticket that names nowhere. The procedure that works is
to stop the share and start a fresh one, reissuing tickets to everybody
including the peer already connected; a *second concurrent* share is refused by
`session::claim`, so it is not a way round this. Closing it needs
the serving process to answer a request from another process, which is a channel
this feature does not otherwise need, so it waits for someone to want it.

## 6. Distribution: the CLI is the product, the skill is the interface

`h5i` is a single Rust binary with no server, no daemon, and no SaaS. That makes
the distribution story short, and it means the **agent facing interface is a
skill**, following the pattern already used by `h5i-db`:

```bash
npx skills add h5i-dev/h5i     # installs the skill from skills/h5i/
```

Repository layout to converge on:

```
skills/h5i/
  SKILL.md                    # one page: when to reach for a box, the loop, the guardrails
  references/
    boxes.md                  # create, run, shell, status, export lifecycle
    browser.md                # the h5i browser verb set and the snapshot format
    policy.md                 # profiles, egress, caches, what is and is not enforced
    export.md                 # the output gate and reading a receipt
    troubleshooting.md        # probe output, tier fallbacks, common denials
```

Notes on shape:

- **The skill replaces `.claude/h5i.md` and `plugin/`.** Both are Claude Code
  specific and predate the pivot. One skill, runtime neutral, is the single
  place the usage rules live.
- **Two audiences, one skill.** The host side agent needs "make a box, hand work
  to it, read the export". The in box agent needs "you are inside a box, here is
  `h5i browser`, here is what is denied and why". SKILL.md routes between them by
  checking `$H5I_BOX` rather than shipping two skills.
- **The skill does not install the binary.** `npx skills add` writes Markdown.
  The binary keeps `install.sh` plus prebuilt release artifacts, and the agent
  images bake it in. SKILL.md's first line has to handle "the binary is missing"
  without guessing.
- Skill prose is under a total budget, the way `h5i-db`'s is: SKILL.md stays
  around 100 to 150 lines and pushes detail into `references/`, which are loaded
  only when needed.

### 6.1 The binary carries the skill

`skills/h5i/` is embedded into the binary at build time (`include_str!` over the
directory), and the CLI can write it back out:

```bash
h5i skill install [--target <dir>] [--runtime claude|codex|cursor]
h5i skill show [<reference>]      # print SKILL.md or one reference page to stdout
h5i skill path                    # where an install would write
```

This is how the in box agent gets the skill, and it removes the two bad options:
nothing is baked into the image, and nothing is copied from host to box.

What it buys beyond convenience:

- **No version drift.** A skill that documents flags the installed binary does
  not have is worse than no skill. Embedding makes the skill a property of the
  binary, and `h5i skill install` stamps the version it wrote.
- **The in box copy can be box specific.** `h5i skill install` inside a box
  knows the tier that was actually enforced, the egress allowlist, the cache
  mounts, and whether a desktop is attached. It can render the policy section
  with the real values instead of describing the general case. An agent that is
  told exactly what is denied stops trying to work around it.
- **`h5i skill show` is a cheap in context lookup.** A reference page on demand,
  from the binary, with no file to find.
- **Bootstrap becomes one line.** Box creation runs `h5i skill install` as part
  of its own setup, alongside the profile and the shell rc it already writes.

`npx skills add h5i-dev/h5i` stays as the front door for people who do not have
the binary yet. Same bytes, since both come from `skills/h5i/` in this repo, and
a test asserts the embedded copy matches the checked in one.

### 6.2 The programmable surface: a JSON contract first, an SDK second

The model is `remote-agent-browser` (Vercel Labs, `~/Ref/remote-agent-browser`).
Its whole SDK is about 1,200 lines of TypeScript: spawn a command in the
sandbox, parse the JSON envelope, hand back a typed result. The programmable
experience developers like lives almost entirely in the CLI's machine readable
output, not in the wrapper. That is the order of work here too: the contract is
the product, the SDK is packaging.

**No daemon.** The SDK spawns the `h5i` binary as a subprocess and parses
`--json` output. This keeps the "one Rust binary, no server" decision intact,
and it does not conflict with the No MCP decision: MCP was cut because it put a
host side *agent* inside the box's interface. An SDK puts the developer's
*orchestrator* on the host and the agent in the box, which is the direction
`box run` already serves.

**The JSON contract.** Every lifecycle verb an SDK would call takes `--json`
and emits a stable envelope on stdout, with human notes on stderr. As of
2026-08-05 that covers the full loop: `create` (the manifest, same shape as
`status --json`, plus the workspace path), `run` (box id, policy digest,
capture id, exit code, timing, peak rss, the recorded redacted output, and the
full receipt record; the exit code still passes through), `export` (the export
summary: files changed, patch bytes, receipts, denied egress count,
redactions), plus the verbs that already had it: `list`, `status`, `diff`,
`log`, `inspect`, `compare`, `capabilities`, `doctor`, `secrets`, `ports`,
`allow`, and `--version`. This is scriptable today from shell, CI, or an
agent's Bash tool, with no SDK at all.

**The SDK mapping is mechanical.** `create()` is `box create --json`, `exec()`
is `box run --json`, `browser.run([...])` is `box run -- agent-browser ...`
(the daemon is already in the box, so no new host-to-box channel exists),
`diff()` is `box diff --json`, `export()` is `box export --json`, `events()` is
`box log --json`, `close()` is `box rm`. One deliberate omission: an
`agent.run(prompt)` that shells out to a per-call headless `claude -p` is the
wrong primitive. If the SDK grows an agent handle it should hold a resident
session (a `box shell` it can send to and wait on), and the first release can
ship without it: exec, browser, diff, and export are enough for the derivative
projects that matter (PR screenshot bots, dependency evaluators, visual
regression CI, browser-use evals).

**Sequencing.** The contract lands now. `@h5i/sdk` (TypeScript, a postinstall
that fetches the release binary the way esbuild and biome do) waits until the
first buyer workflow has been demonstrated end to end, because the SDK
amplifies a story that has to exist first. Python follows demand, not the
roadmap. The acquisition logic: every third party repository whose README says
`npm install @h5i/sdk` is distribution, and the closest fit between the
boundary's value and an SDK consumer is CI (run an untrusted PR, build it,
drive it in the box's browser, post the screenshots and the receipt).

## 7. The browser layer: agent-browser, not a viewer of our own

An earlier draft of this roadmap had us reimplementing Neko's capture, encode
and input core in Rust so the human could watch the box. That plan is dropped.
**agent-browser** (Vercel Labs, Apache-2.0, `~/Ref/agent-browser`) already is
both halves of what we needed, and it is a native Rust CLI:

- **Automation.** `open / snapshot / click / fill / press / hover / select /
  scroll / screenshot / eval / wait`, plus semantic locators (`find role button
  click --name "Submit"`). Snapshots are accessibility trees with `@e2` style
  refs, which is exactly the token-cheap shape a model needs. It speaks CDP
  directly: no Playwright, no Node at runtime.
- **The human viewer.** Every session runs a WebSocket server that streams the
  viewport as JPEG frames (CDP `Page.startScreencast`) **and accepts input
  events back** (`Input.dispatchMouseEvent` / `KeyEvent` / `TouchEvent`). Their
  own words for it are "pair browsing where a human can watch and interact
  alongside an AI agent". Frame quality, size and rate are tunable per box.

What that buys us, beyond not writing it:

- **The desktop stack disappears.** No Xorg, no GStreamer, no PulseAudio, no
  window manager, no supervisord. Headless Chrome plus one binary. The browser
  container drops from a Neko-runtime-sized image to something an agent box can
  reasonably carry, and the attack surface shrinks with it.
- **One less protocol to design.** Frames and input already have a defined
  WebSocket message format, and there is a reference client (their dashboard) to
  check ours against.
- **It matches our distribution model.** Native Rust, installable via cargo or
  npm, and it ships a skill of its own.

What stays ours, and it is not small:

1. **The boundary.** The stream port and the CDP port never leave the box's
   network namespace. The viewer is reached through an h5i-owned forward with a
   per box token (5.9). agent-browser assumes a friendly localhost; we do not.
2. **The control lock** (5.4). Two clients can dispatch input at once and
   nothing upstream arbitrates. That is ours to enforce.
3. **Policy.** Fresh profile, proxy settings, `--allowed-domains` derived from
   `net.egress`, AI chat disabled, downloads landing under the export gate.
4. **Receipts.** Browser commands, console errors and failed requests are
   evidence and belong in the receipt like any other observation.

The cost is a third-party dependency on the critical path. We pin a version,
depend on the **CLI** surface rather than internal APIs, and keep the fallback
in view: it is Apache-2.0 Rust, so vendoring or forking stays available if the
project moves somewhere we cannot follow.

**Neko is not gone, it is deferred.** CDP screencast shows the page viewport and
nothing else. The day the product needs a real desktop (a native app under
test, browser chrome, a file picker, devtools as a human sees them), an X plus
streaming tier comes back, and Neko is the reference design for it. Nothing in
the boundary, the lock or the receipt changes when it does.

### 7.1 Two engines, one policy (proposed, 2026-08-07)

A survey on 2026-08-07 (Cloudflare's Kitesurf announcement, Lightpanda, and a
read of agent-browser upstream) changed what "the browser" can mean here.
Kitesurf is a browser engine rewritten for agents: Rust (Blitz for HTML and
layout, Stylo for CSS, Boa for JS) compiled to WASM and run as disposable
isolates behind a single egress worker that owns every network request and
every cookie jar. It passes 215,000+ Web Platform Tests, speaks CDP and MCP,
treats every page load as untrusted input, keeps no persistent authenticated
sessions, and Cloudflare says it will be open-sourced for self-hosting.
Lightpanda (Zig, html5ever plus V8, CDP) is the same camp, and agent-browser
upstream already selects between Chrome and Lightpanda with `--engine`. The
part that matters to us is not the cloud: it is that non-Chromium engines an
agent can actually use now exist, and the ecosystem has converged on CDP as the
interface, so an engine swap does not orphan the tooling.

The h5i-shaped observation: **our egress proxy is a blind CONNECT gate, so
browser receipts can only name hosts.** The proxy sees
`CONNECT docs.example.com:443` and nothing else; the evidence for "what did
the agent read" is whatever the post-run console drain catches. An engine whose
network stack *is* our proxy inverts that. Every request and response becomes
first-class evidence, per-request policy needs no MITM CA because we are the
client, and for untrusted origins script execution can be off entirely, which
removes the delivery channel for most page-borne prompt injection instead of
trying to filter it.

Stated precisely, because Chromium can get partway there: CDP's Fetch domain
lets a mediator pause every request Chromium makes and allow, deny, rewrite or
record it, so request-level receipts and per-request policy are available on
the Chromium path too, through the M8 sidecar (7.2). What a mediator cannot
make them is **fail-closed**: attach races, freshly created targets and
workers, event buffer limits and disconnects all mean CDP coverage is
monitored rather than guaranteed. The engine's claim is narrower and stronger:
if the log is not running, the request does not happen, and a page script is
never evaluated unless a profile line granted it, checked before evaluation
rather than filtered after. With a JS engine in the binary the honest words
are "off by default, gated by policy"; "absent by construction" is reserved
for a `--no-js` feature build, if one is ever worth cutting.

The model this points at is **two engines, routed by origin, one policy**:

- **Loopback, the agent's own dev server: Chromium.** Verifying that a modern
  app renders, hot-reloads and runs its client-side code is the hardest compat
  case there is, and the content is the agent's own code. Fidelity wins, and
  today's stack stays exactly as built.
- **The untrusted web (docs, search, issue trackers): the light engine.**
  Reading rarely needs a JIT, receipts matter more than pixels, and the model
  wants a tree, not a frame. Containment wins.

Two things fall out of this split without being built. **Video and WebGL
never enter the light engine's scope**: a coding agent testing a video player
is testing its own app, which is loopback, which is the Chromium path, where
both already work. Kitesurf has to name them as gaps because it has no
Chromium half; we do. And **authenticated sessions, Kitesurf's other stated
gap, are answered by the control lock we already ship**: the agent hits a
login wall, the human takes the viewer, logs in, hands back, and the agent
resumes from a fresh snapshot (5.4). Watching stays the default posture and
input stays an explicit take; a local-first tool with a human present should
use that human, not imitate a cloud service that cannot have one.

The routing rule lives next to `net.egress` in the profile, not in the agent's
moment-to-moment choice: the agent must not get to pick the weaker-policy
engine for a hostile page. Two degenerate profiles fall out for free:
`browser` as it exists today (Chromium only, nothing changes), and a
`browser-lite` with no Chromium at all: no Chrome preflight, no 12 GiB limit,
plausible at the microvm tier where the Chromium stack has never been proven.

The staged path, cheapest first:

1. **Engine as a profile knob.** agent-browser already abstracts the engine;
   a profile field that sets `--engine lightpanda` costs almost nothing and
   changes no seam we own (socket dir, egress env, evidence drain all stay).
   What it buys is the real data: where a light engine actually breaks on our
   loop, before we bet anything on one.
2. **Origin routing.** agent-browser is one engine per daemon session, so
   per-origin routing means either two sessions with h5i choosing at navigate
   time, or our own layer in front (7.2 builds that layer for other reasons).
   This step is a design decision, not a big build, but it is honest to say
   the session model does not give it to us for free.
3. **The lightweight visual engine.** A crate of ours: Blitz and Stylo for
   parse, layout and paint, Boa (off by default, policy-gated) for the
   minority of pages that need script, and fetch wired directly into the
   egress proxy's stack so the receipt *is* the network log. Beyond fail-closed logging, an owned engine can bind what no
   mediator can: a human-approved form submission mints a **single-use
   capability** for that origin and those fields, page script cannot spend it,
   and every request carries its provenance (agent, human, or page script) as
   a structural fact instead of an inference over event timing. **Assembled,
   not written**: the component stack is the same open-source Rust Kitesurf
   builds on, and the build-versus-adopt call waits for Kitesurf's open-source
   drop before choosing which pieces are ours.

What we do not do is write HTML, CSS, JS or rasterization primitives from
scratch: the engine is assembled from Blitz, Stylo and Boa, focused on what
an agent needs. Section 7's argument is unchanged: Chromium plus
agent-browser stays the fidelity path, docs-grade pages are the light
engine's compatibility bar rather than React, and the light engine earns its
place on the strength of receipts and a containment story Chromium
structurally cannot give us.

**Superseded in part, 2026-08-08.** The survey above is accurate and still
worth reading; its *conclusion* is not. "Two engines, one policy" assumed
routing lets a box avoid Chromium, and it does not:
`sandbox_policy::browser_read_grants()` chains every engine's candidates, so an
`h5i-light` box grants Chrome's and agent-browser's paths anyway and the
environment still installs and updates Chromium. Routing saves runtime RSS and
nothing else. §12 records the decision that replaced it: one local engine that
runs script and renders on demand, with Chromium kept as the fidelity fallback
rather than as the answer to "what about JavaScript".

### 7.2 Owning the daemon socket (proposed; the interception point open item 1 asks for)

Open item 1 records that the control lock is advisory because no h5i process
sits between the agent and agent-browser. The upstream read says the
interception point exists and is small: the daemon's entire control surface is
newline-delimited JSON over one filesystem-bound `AF_UNIX` socket,
`{"id", "action", ...}` in, `{"success", "data", "error"}` out, one line each
way, every action serialized under a single mutex. That is a protocol a
supervisor can hold in a few hundred lines.

The shape: the daemon's real socket moves to a path the box has no grant for,
and the path the box is given (`AGENT_BROWSER_SOCKET_DIR`) carries an
h5i-owned listener that forwards line by line. The design consequence is that
the daemon stops being an in-box child and becomes an h5i-launched sidecar,
because a daemon spawned by the agent's own CLI can only ever bind where the
agent can also reach. That is the shape the macOS shim already has (h5i
launches Chrome itself and attaches agent-browser to it), so it is a
convergence, not a fork; and it must not move the boundary: the sidecar stays
in the box's netns, under the box's egress, with nothing published.

What one mediated socket buys, in order of value:

1. **The lock becomes real.** `control::check` runs on every mutating verb,
   and `HeldByHuman` / `NeedsResnapshot` come back as the daemon's own typed
   error. Read-only verbs pass untouched, which is exactly the split 5.4
   wants: watching never collides.
2. **Per-action receipts.** navigate, click, fill, eval land in the receipt as
   they happen, with their arguments. Today's evidence is a post-run console
   drain; this is the action log, and it is the browser-side analogue of the
   egress tally.
3. **A browser action policy.** Upstream's `ActionPolicy` (allow / deny /
   confirm over action names, about 200 lines) is the right vocabulary, worth
   adopting as a per-profile manifest: `eval` deniable, `credentials_*` and
   `state_*` deniable, and a `confirm` tier for consequential actions, which
   is where the whole field landed on injection containment (per-site grants
   plus human confirmation). The confirm channel is the viewer.
4. **The CDP side of the same sidecar.** Owning the daemon means owning its
   browser, so the sidecar can attach CDP `Fetch.requestPaused` and give the
   Chromium path request-level evidence and per-request policy: method,
   origin, initiator and verdict in the receipt, not just the CONNECT line.
   This lane is recorded as best-effort, because CDP coverage fails open
   (7.1); the fail-closed version of the same lane is M10's reason to exist.

The honest costs: an h5i process on the browser hot path; a dependency on the
daemon's wire protocol, which is an internal surface, against section 7's
stated preference for the CLI boundary (mitigated the same way: pinned,
forkable, and the protocol is one page); and the sidecar launch is new
lifecycle code where today the daemon manages itself.

## 8. Phases

Each phase ends with a green `cargo test` and a demo that runs on a stock
rootless Podman host.

> **Suite status, 2026-08-05.** `cargo test --lib` is green across the
> workspace (391 tests, of which 66 are the terminal viewer's) and clippy is
> clean with `--all-targets --all-features` and with `--no-default-features`. Three `env_integration` tests fail on this WSL2 host with a
> worktree-stat error (`box_git_grants_stay_fail_closed_outside_env_namespace`,
> `box_git_status_and_commit_work_inside_process_tier`,
> `process_tier_confines_fs_and_network`). They fail identically at
> `e4488b064`, before any of this work, so it is host drift rather than a
> regression, but it is drift nobody has diagnosed, and the process tier is
> not actually covered here until someone does.

### M0. Freeze and branch: done

`dev` is the integration branch and this
roadmap is on it.

### M1. Amputation: done

Section 3.2 is deleted (~77k lines). `receipt.rs`,
`refstore.rs`, `redact.rs`, `source.rs` extracted; `env.rs` is free of
`objects`, `ctx`, `msg`, `team` and `repository`. The whole lifecycle works
with no git notes and no context refs, clippy is clean over the workspace, and
the `web` feature is gone rather than off.

### M2. `h5i box` and copy in: done

New command surface with `env` aliased
(short form, `ls`, hidden alias). Export gate replacing `propose`/`apply`
(patch + report + receipt bundle, refuses to overwrite). `h5i skill install`
from the embedded skill. Receipt integrity by sealing, with the test that pins
it (5.7). All four sources: this repository, a pull request, a repository URL,
and `--new`.

The copy-in landed as **detached boxes**: for a URL or `--new`, the box gets a
git repository of its own inside its directory, the host repository is never
touched (no branch, no worktree, no objects), the inherited `origin` remote is
dropped so the box cannot reach a network handle nobody granted it, and `apply`
and `rebase` refuse and point at `export`. That is the boundary the phase was
for, and it holds on every tier rather than only under a container volume.

### M3. Agent in box hardening: done

Warm caches in full: the store, the
lockfile keying, the staleness rule, `h5i box cache ls|mounts|rm|refresh` and
the **read-only mount** on every tier are built and tested (5.8). (An earlier
revision of this line said `refresh` was not built; it landed in e75020358,
with the writable bind reachable from no profile and the refusal that names the
registry-only profile it demands.)
Also done: the credential-seed audit (the per-box HOME copy now drops
credential-shaped entries at any depth (`credentials*`, `.netrc`, ssh keys,
`*.pem`/`*.key`/`*.p12`), keeping only the runtime's own token, which it cannot
function without), and the credential proxy, which was already default-on but
did not engage for a `browser` box.
Also done: profile-declared authenticated egress (5.5, option 1): a reverse
proxy per grant, the credential resolved host side and never placed in the box,
part of the pinned digest, and fail-closed when the host-side variable is
unset. GitHub is a policy entry, not a feature. Option 2 (a TLS-terminating
forward proxy) stays unbuilt and unneeded.

### M4. Browser: done

The live runs were worth more than the code around them. What they found, in
order:

1. ~~**The `supervised` + agent-profile `EINVAL` happens when the box's
   workspace is under `/tmp`**, because the agent profile redirects `/tmp` to a
   per-env scratch and that shadows the worktree.~~ **Wrong, and withdrawn.** A
   `create`-time refusal for that layout was written and it rejected this
   suite's own fixtures: every `tempfile` repo is under `/tmp`. Checked
   directly instead: a supervised box whose workspace is under `/tmp` sees its
   workspace, runs commands, and drives the full browser loop. The bind-ordering
   fix in 86dddafe0 (mount `/tmp` last) had already handled it. The working
   behaviour is now pinned by a test rather than guarded by a phantom.
2. With that out of the way, a `browser` box at `supervised` creates and runs,
   and `agent-browser --version` answers from inside it.
3. `agent-browser`'s daemon put its control socket in `$XDG_RUNTIME_DIR`
   (`/run/user/<uid>`), which no box has a write grant for, and failed with
   "Failed to create socket directory: Permission denied" long after create
   said everything was fine. **Fixed**: `AGENT_BROWSER_SOCKET_DIR` now points at
   the box's own `/tmp`, which every tier grants and the kernel tiers make
   per-env.
4. `agent-browser doctor` **from inside a box** is the tool for the rest of
   this, and it immediately caught a bug in our own policy: pinning
   `AI_GATEWAY_API_KEY` to an empty string *enabled* chat, because
   agent-browser tests for the variable's presence. The box reported
   "AI_GATEWAY_API_KEY present (chat enabled)", the opposite of the intent.
   **Fixed** by not injecting it at all (it is not in `env.pass` either, so it
   is absent), and verified from inside: "chat command disabled".
5. Doctor also confirms the profile's grants work: **Chromium 130 is found** at
   the granted `~/.cache/ms-playwright` path.
6. **The daemon exited during startup with no output, and it was our socket
   gate, not Chrome.** The supervised tier notifies on `socket()` and denied
   `AF_UNIX` unconditionally, with no way for a profile to ask for it; the
   daemon's control socket is a filesystem-bound `AF_UNIX` listener, so it got
   `EPERM` on the first thing it did. `Profile::unix_sockets`
   (`[profile.X.net] unix = true`) is that way to ask, and `browser` sets it.

   The grant is narrower than it sounds, which is why it can exist: abstract
   sockets are scoped by the private netns, filesystem-bound ones by Landlock,
   and `/tmp`, where `.X11-unix`, `tmux-*` and an ssh-agent live, is a per-env
   scratch at the kernel tiers. The residual is a host socket under a granted
   path, so it stays opt-in per profile and lands in the digest.

   The silence was upstream's, and worth recording: the daemon redirects its own
   stderr to `/dev/null` before failing **unless** `AGENT_BROWSER_DEBUG` is set,
   in which case it writes to `$AGENT_BROWSER_SOCKET_DIR/<session>.log`. That log
   is the only place the real error appears; `--debug` alone does nothing.

7. **Two variables we set were not variables agent-browser reads.**
   `AGENT_BROWSER_HEADLESS` does not exist (it is `AGENT_BROWSER_HEADED`, and
   headless is what a falsey value means), and neither does
   `AGENT_BROWSER_DISABLE_CHAT`: chat is gated on `AI_GATEWAY_API_KEY`
   presence alone. A variable the tool never reads reviews as enforcement while
   enforcing nothing, so both are gone and the tests assert their absence.

Nothing here could have been found by reading the code, which is the argument
for driving the loop before building more on top of it.

That gap, **no test in the suite ran an agent-family profile at
`supervised`**, the only kernel tier that can host an agent or browser box
(`process` refuses the egress the profile needs), is why both surprises were
available to find, and it is now closed. The test that would have caught the
daemon failure asserts both directions: a `browser` box binds a
filesystem-bound `AF_UNIX` listener, and a `default` box on the same host and
tier still gets `EPERM`, so the grant cannot silently become tier-wide.

Built: the `browser` built-in profile (the agent profile plus the browser
surface, runtime scoping intact, egress never wider than the agent's),
host-path discovery for the kernel tiers with a fail-closed create that names
what to install, `containers/Containerfile.browser` for the container tier, and
`/dev/shm` sized from the policy so a renderer does not die on Podman's 64 MiB
default. **No pod, no second image, no Podman requirement** (5.2).
`--allowed-domains` is derived from the enforced `net.egress` (plus loopback,
which never appears in an allowlist but is the whole point of a dev server),
headless is pinned through the variable that actually exists, and the AI gateway
is refused by absence, the only mechanism upstream has.

**Browser evidence in the receipt** is built (`crates/h5i-core/src/browser.rs`).
After a run that drove the browser, h5i asks the page what happened and records
the console errors, uncaught exceptions and failed requests, then surfaces them
in `report.md` above the agent-authored proposal. Four properties make it
evidence rather than decoration: h5i picks the moment, a host-side cursor keyed
to a session fingerprint keeps each record to its own slice, a browser command
with no browser to ask is recorded `unavailable` rather than as a clean page,
and a host-side socket check keeps the drain from *starting* a browser just to
report an empty console.

Verified live on a supervised browser box against a page that logs a console
error, throws a `TypeError` and fetches a missing URL: all four findings reach
the receipt, the export bundle and `report.md`.

Exit criterion **not yet demonstrated**: an agent fixing a real UI bug using
only agent-browser output as its feedback. Every piece it needs is built and
proven by hand; nobody has run the loop with an agent in it.

### M5. Viewer: done

The control lock
(`crates/h5i-core/src/control.rs`, `h5i browser status|take|release`): the
agent holds control by default, a human *takes* it rather than asking, and
handing it back sets a stale-handle flag that refuses the agent's next mutating
action until it re-snapshots. Read-only verbs stay available throughout,
because watching never collides. Nothing upstream arbitrates this, which is
why it is ours.

The forward (`crates/h5i-core/src/view.rs`, `h5i box view`, `h5i browser url`)
serves the agent-browser stream to loopback. The box's port is never published:
h5i enters the box's user and network namespaces by pid, connects from inside,
and hands the socket back over `SCM_RIGHTS`, the fd-handoff the supervisor
already uses. All four gates verified live against a supervised browser box:
loopback only; a per-box token minted at create and kept outside every path the
box can read or write (401 without, 401 on a wrong one); cross-origin
handshakes refused (403) even with a valid token; and the control lock on the
input direction, with input *dropped* rather than rejected so someone who clicks
before taking control keeps a live viewer. Sessions land in the receipt and the
export under a `viewer` lane, and the report calls out a session where a human
drove.

Three bugs found, all the same kind: quiet failures producing a plausible
wrong answer rather than an error, and each worth remembering:

- The live registry records h5i's **host-side** pid, which is in the host's
  netns. Entering it succeeds, finds nothing listening, and reads as a broken
  box. Fixed by walking the session's process tree for the first descendant
  whose netns differs from ours.
- A stray CRLF in the relayed handshake is not a protocol error the server
  reports. It is two bytes read as the start of the client's first frame, after
  which the handshake completes and the viewer hangs.
- Returning `Result<u64>` from the input pump discarded the forwarded-input
  count on the error path, which is exactly the path a human takes by closing
  the tab. The export would have recorded them as never having touched the box.

Exit criterion **not yet demonstrated end to end**: a human takes over mid-run,
finishes a form, hands control back, and the agent continues from a fresh
snapshot. The takeover, the input gating and the stale-handle refusal are each
verified; a real person finishing a real form is not something this session
could run.

### M6. Skill and story: mostly done

`skills/h5i/` is written against the
real surface and the binary carries it; the missing fifth page
(`references/browser.md`) is written. The README, MANUAL.md, `man/h5i.1` and
`docs/manual/index.html` all describe the product that exists. The manual was
3,900 lines of `capture`/`recall`/`audit`/`team`/`mcp`, and is rewritten around
the boundary. The landing page is rewritten too, and the embedded mock of the
deleted `h5i serve` workbench is gone with its CSS and its film driver. Seven
guides plus `/features/` and `/workflows/` teach deleted commands, so each
carries a banner and is `noindex` rather than being quietly left.

**Remaining**: `npx skills add h5i-dev/h5i` is still unverified: the `skills`
CLI needs Node >= 22.20 and no such runtime was available to test it; the repo
layout and frontmatter were checked against what the CLI discovers. There is no
demo video. `/blog/` and `/pitch/` still argue the old positioning, and
rewriting them means choosing the launch message, which is open question 2.

### M7. Terminal viewer: built and driven

`h5i box view --term` (5.10). The
module is unit tested (the WebSocket client also round-trips over a real
socket), clippy is clean, and the web viewer's silent input bug is fixed and
pinned (5.10.1).

The loop was driven end to end against a live supervised `browser` box serving
a page on its own loopback, on a pty with a real window size: watch, `i` to take
the lock, type and click, `Ctrl-]` to hand it back, `q` to leave. What that
proved, in order: `connect_in_netns` into the box, the WebSocket handshake, real
JPEG frames decoded and scaled (an 800×400 image in 100×20 cells, aspect
preserved, 624 continuation chunks), the previous frame deleted only after the
next was placed, the control lock flipping to `human` and back to `agent`, mouse
tracking taken and returned, the alternate screen restored on a clean exit, and
the status line picking up the page's real URL. The receipt recorded `12
frame(s) forwarded to the page` ("hello" as ten key events plus a press and a
release) and flagged `a human drove this box` from the input count, which is
the take-and-hand-back case that comparing the holder at open and close would
have missed.

Two things it found that no amount of reading would have. A pty with no window
size makes `TIOCGWINSZ` succeed and report zeroes, and the viewer dutifully
scaled the page into one cell and transmitted a **1×1 pixel image** with no
error anywhere; `Size::or_fallback` now supplies 80×24. And the first harness
stopped reading before sending `q`, which is why it looked as though the
alternate screen was never exited, worth remembering as a way to mistake a
test artifact for a terminal-corrupting bug.

Still **not demonstrated**: a real person, at a real Kitty-protocol terminal,
looking at the page. Everything under it is exercised; the last inch is a human
being, and a tool shell is not a TTY.

**Post M7.** A full-desktop tier when something needs more than a page viewport
(X plus streaming, Neko as the reference design), microVM backend, macOS.

### M8. The mediated socket: proposed (7.2)

The agent-browser daemon becomes
an h5i-launched sidecar and its socket path is h5i's listener. Exit criteria:
an agent's `agent-browser click` during a human takeover is refused with the
typed error, not advised; every mutating verb appears in the receipt with its
arguments; a profile denies `eval` and the denial lands in the receipt; and
the Fetch evidence lane (7.2 item 4) shows a granted request with its
initiator and a denied one with its verdict, marked best-effort. This closes
open item 1 and is worth doing before any engine work, because the mediation
layer is where origin routing (7.1) would live anyway.

### M9. Second engine: proposed (7.1)

`engine` as a profile field, pinned
in the digest with its version like any other policy choice, Lightpanda as
the first non-Chromium value. The knob's real shape is "any CDP endpoint",
not "Lightpanda": agent-browser already drives engines over CDP, so this is
the slot M10's binary later fills with no new plumbing, and agent-browser
stays the one automation surface for every engine behind it. The subset an
engine must speak is automation plus the screencast domain
(`Page.startScreencast` / `screencastFrame` / `screencastFrameAck`), because
the whole shipped viewer stack, stream server through terminal panes, sits
downstream of that domain and follows any engine that implements it. **No silent
fallback**: an unsupported page fails closed and names the retry ("this page
needs MediaSource; recreate with `--browser chromium`"), because a fallback
to Chromium is not an optimization, it is a security-policy change: an API
absent by construction in one engine exists in the other, so the box's
capability surface must not move without a create. The engine and its
version land in the receipt. Exit criteria: a `browser-lite` box with no
Chromium installed answers `doctor`, snapshots a real documentation site,
and the full loop's failure modes on a light engine are written down here.
No routing yet: one engine per box.

### M10. The lightweight visual engine: tier 1 built, 2026-08-07

`crates/h5i-browser-light`, a standalone binary: static render, agent
snapshot, screenshot, and the fail-closed request log. Blitz + Stylo +
vello_cpu assembled behind our own broker; 42 tests; clippy clean with and
without default features. Driven live against a local page and a real site.

Built **ahead of its gates**, and that should be said plainly: M9 has not run,
so there is no compatibility data yet, and Kitesurf's open-source drop has not
landed, so the build-versus-adopt call was made without it. What that buys is
a working artifact to measure instead of a design to argue about; what it
costs is that tier 2/3 scope is still guesswork, and the CI lockfile grew by
137 packages to carry the engine.

Two findings from driving it, neither available by reading:

1. **A denied resource must be *completed*, not dropped.** Blitz counts a
   resource pending until its `NetHandler` is called, and `paint_scene`
   refuses to paint while any critical resource is pending, so the obvious
   way to write a deny (return without calling the handler) renders every page
   permanently blank. Fail-closed means "completed with nothing", and there is
   a test on it.
2. **`system-fonts` is a build-time native dependency.** Blitz's font
   discovery pulls `yeslogic-fontconfig-sys`, which needs libfontconfig
   headers to compile: portable engine, non-portable build. Fonts are
   discovered and registered at runtime instead, which also makes "no fonts"
   a state `doctor` reports rather than a blank screenshot nobody can explain.

#### Tier 2, and the numbers

**Tier 2 built, same day.** `h5i-browser-light serve`: a WebSocket that speaks
the viewers' own format (base64 JPEG in a JSON envelope, `status` carrying the
viewport, `config`/`ack` pacing), plus a `.stream` file so the existing
discovery finds it. Scroll and link-click work; a click on a refused link
returns `page_error` and keeps the page. Frames are driven by change, not by a
clock: with no script, at rest the process sends nothing, and an `ack` alone
never produces a frame. Verified with a protocol-level client that does its own
handshake and masking, so the compatibility check is against the spec rather
than against our own encoder.

**The numbers, measured rather than hoped for.** Median of 5 after a warm-up,
same host, self-contained local pages, peak summed RSS across the process tree:

| | one-shot, 39 KB docs page | idle with a page held open |
| --- | --- | --- |
| h5i-browser-light | 72 ms / 33 MB | 37.5 MB |
| chromium `headless_shell` | 356 ms / 479 MB | 383.8 MB |
| chromium (full) | 644 ms / 799 MB | — |

That is ~5x faster and ~15x lighter one-shot, ~10x lighter at rest, so the
memory exit criterion is met in both the states it named. Three caveats travel
with it and belong in any repetition of it: cold start is included and
dominates Chromium's time (fair for a one-shot agent invocation, not a
steady-state throughput claim); the pages carry no JavaScript, so Chromium is
paying for an engine it is not using and a script-driven page would reverse the
comparison entirely; and software rasterisation will narrow the time gap on
heavy CSS.

Still open at this tier: no script; input stops at scrolling and link clicks
(no typing, no form submission); and the live view has been driven by a
protocol-level test client rather than by `h5i box view` against a real box.
What is missing in that last one is the run, not the plumbing.
`H5I_BROWSER_STREAM_FILE` puts the `.stream` under the box's `agent-browser`
directory, which is where the viewers' discovery already scans.

#### What landed after, 2026-08-08

**Tier 2's open item closed.** The live view has now been driven by
`h5i box view` against a real `h5i-light` box rather than by a protocol-level
client: the forward attaches and renders, the console's frame relay pulls a
1280x720 JPEG through the same session, input is dropped while the agent holds
the control lock and flows the moment a human takes it, so the lock is enforced
on an engine with no mediator behind it, which had never been checked. Two
defects fell out of the run, both fixed:

1. **A readable file could fail to open.** `open ./page.html` reported "invalid
   path" when `canonicalize` failed, because the fallback handed a *relative*
   path to `Url::from_file_path`, which refuses one. The walk fails for a
   working directory the box can reach by fd and not by name, which is any
   repo under `/tmp`, since that is the directory the supervised tier
   overmounts. The message named the path when the problem was the walk.
2. **`serve` accepted one viewer at a time.** The accept loop handled
   connections sequentially, so opening the console's page tab left
   `h5i box view` hanging in the backlog with no error. Two viewers could not
   coexist, which nothing had tried.
3. **Scrolling only ever worked on unstyled pages.** The scroll range came from
   the root element's `size.height`, which a stylesheet saying
   `html, body { height: 100% }` pins to the viewport while the article
   overflows it, so Blitz reported Wikipedia's 16477px page as 720px and every
   scroll clamped to zero. The fix reads `size.height.max(content_size.height)`,
   which is the same formula Blitz's own `scroll_viewport_by` uses. Every local
   test page was unstyled, so the whole suite agreed with the bug. Found by
   pointing the thing at Wikipedia, which is the entire argument for doing that.

**The resident session, 2026-08-08 (§12.1).** `serve` now holds a page
that several viewers and a control channel share, and
`h5i-browser-light session status|snapshot|navigate|click` drives it. A control
verb that moves the page broadcasts to every viewer, so the live view shows the
page *the agent* is driving, so the caveat M11a's page pane had to print is gone
for this engine. Ack pacing moved from a structural accident ("one frame per
client message") to per-viewer state, holding the *newest* frame rather than
queueing a backlog, and nothing is encoded at all when no one is watching.

The architecture was chosen by the compiler, not by preference: **`Page` is not
`Send`**: `BaseDocument` holds an `Arc<dyn HtmlParserProvider>` and a
`Box<dyn FontMetricsProvider>`, so the obvious `Arc<Mutex<Session>>` does not
exist. One thread owns the page and everything else reaches it by channel. That
is the shape a multi-driver session wants regardless; here it was not optional.

**Untrusted-content marking, 2026-08-08 (§12.1).** The rendered snapshot
now fences page content and names it as data. Pulled ahead of its position in
the list because §11 called it "the only item on this list whose absence is a
live hole rather than a missing feature" while ranking it fourth, and it depends
on nothing. Writing the test found the hole that made the fence worth having:
`href` was the one page-derived field the walker did not collapse, and an HTML
attribute value may contain a literal newline, so the field that could forge
the fence was the field nobody had thought of as text.

**The agent-actions pane had no source on this engine, 2026-08-08.** Found by
someone running an agent in an `h5i-light` box and noticing the pane stayed
empty while the agent worked. It was empty *by construction*: the pane is fed by
`browser-actions.jsonl`, which the mediator writes, and
`engage_browser_mediation` returns `None` for any engine agent-browser cannot
drive. Before the resident session that was harmless: there were no verbs to
miss. Adding verbs made it a monitoring surface that silently under-reported,
which is the failure this codebase keeps writing tests against.

`serve` now writes its own action log (`$H5I_BROWSER_ACTIONS`), ingested as a
fourth source into `BoxStream::poll` and rendered **box-claimed**, not
host-observed. That distinction is the point rather than a caveat: h5i sits on
no socket between an agent and this engine because the engine *is* the browser,
and a row claiming otherwise would launder the box's own account into evidence
h5i gathered. The pane note is engine-aware for the same reason. Each verb is
recorded before it runs and again after (no record, no action), which is a
guarantee against accident, not against a box that has decided to lie.

Measured before shipping, because it sits on the verb path: **7µs per verb**
against **42ms** for the single frame encode a scroll triggers when a viewer is
attached. 0.017% of one frame.

**§11 items 5.2a and 5.5 built, 2026-08-08.** Typing, form submission and a
cookie jar, shipped together because separately none of them reaches a login.
Verified end to end against a real login site: type into two fields, submit a
POST, follow the 303, hold the session cookie, and come back to `welcome alice`
on a later navigate.

Blitz owns the form submission algorithm and dispatches to a navigation
provider, so the engine hands it one that *captures* the request rather than
performing it: the encoding is upstream's, the wire stays ours, and a
submission is policy-checked and receipted like everything else. `Broker::send`
generalises `fetch` rather than sitting beside it, because every guarantee lives
in that loop and a POST that took a shortcut would be the one request with no
receipt.

The cookie jar is deliberately narrower than a browser's, and §12's LOGIN-mode warning
is why the narrowings arrived with it rather than after:

- **Host-only.** `Domain` is ignored. Honouring it correctly needs a public
  suffix list; without one, `evil.co.uk` can set a cookie for `co.uk`. The cost
  is that cross-subdomain logins do not persist. That is a missing feature;
  sending a session cookie to the wrong origin would be a vulnerability.
- **In memory only**, so restarting the session is a complete logout.
- **Never readable by the agent**: no verb returns a value, `status` reports a
  count, and the request log records how many cookies crossed rather than which.
  A credential in a receipt is a credential in every export it reaches.
- `Secure` and the `__Secure-`/`__Host-` prefixes enforced, and a redirected
  POST downgraded to a bodyless GET on 301/302/303 so a password is not replayed
  to whatever a server names next.

Two bugs the tests caught rather than review. The request-path matcher used
RFC 6265's *default-path* derivation, which exists only to fill in a missing
`Path` attribute, so a cookie set at `Path=/admin` was never sent to `/admin`.
And `scroll_height` was tried for the scroll range before the fix above: taffy
measures overflow *within* a box, which is zero for an unstyled page whose root
simply grew.

#### Still open, and one correction

**LOGIN mode is not built**, and it is the one item this entry was
warned about: §12 pairs LOGIN mode with cookies precisely because a session
with cookies is the first version of this browser where a stolen credential is
worth having. Until it lands, a human taking over to type a password does so on
a page the agent can still snapshot. File uploads are dropped rather than read,
which is a deliberate refusal to acquire filesystem reach. Tier 3 (policy-gated
script) is now in scope, and the cost of putting it there is §12.5.

**Corrected 2026-08-08.** This entry also said "nothing wires h5i to this
engine yet: M9's `--engine` knob does not exist, so using it in a box is
still manual", which was true the day tier 2 shipped and stopped being true
three commits later on the same branch. `--engine h5i-light`, or
`[profile.X] engine`, pins the engine in `policy.resolved.toml` and so in the
digest; `browser_env` hands that engine `H5I_BROWSER_ALLOW` (the box's own
`net.egress`, loopback included) and `H5I_BROWSER_RECEIPTS` pointing at the
box's spool, and skips the agent-browser shim, whose job is to launch Chrome
and attach a driver, neither of which applies to an engine h5i runs itself.
Using this engine in a box is a create-time flag. The entry is left standing
rather than edited away because the gap it records is the real lesson: a
milestone's "still open" list ages against the commits that follow it.

The original entry follows.

**M10 (as proposed).** The h5i-native engine of 7.1 step 3. With M8's Fetch lane already delivering best-effort
request receipts on Chromium, the engine's case is what mediation cannot
give: fail-closed logging (no running log, no request), script off by
default and checked by policy before evaluation, and the single-use
form-submission capability with structural provenance.

The shape is a **standalone binary**, a workspace crate with its own bin,
not a library h5i links. h5i launches it as a process, hands it the egress
proxy endpoint and a receipts channel, and the engine answers with a
capability manifest (`javascript`, `screenshot`, `video: false`, ...) so h5i
never guesses at what is unimplemented. It speaks the CDP subset the M9 knob
defines, automation plus the screencast domain, so it plugs into the
existing driver, the M8 mediation and the whole viewer stack with no new
plumbing, and fail-closed becomes a protocol property: no receipts channel,
no fetch. This is the agent-browser pattern applied to our own
component (a pinned binary behind a protocol boundary, section 7), and it
prices the risk correctly: if the engine fails, h5i is untouched; if it
succeeds, it can stand as a product of its own. One honesty requirement
travels with the standalone story: bare on a host, outside a box, it is
just a light browser, and its containment claims are made only where the
proxy and the receipt store exist.

The build is **tiered**, each tier shipping value on its own so the hardest
one can slip without taking the milestone with it:

1. **Static render.** Blitz and Stylo parse and lay out, `captureScreenshot`
   and the snapshot verbs work, no JS. Docs-grade reading with full receipts
   is already useful and already demo-able here.
2. **Live view.** The screencast domain with adaptive frames: zero at rest, a
   frame on mutation, 20-30 fps under animation, latest-frame-wins with
   `screencastFrameAck` as the backpressure. The light engine's viewer is
   read-only, and that is not a walk-back of 5.4: the control lock and human
   takeover stay on the Chromium path, which is where login walls route
   anyway (7.1).
3. **Script, policy-gated.** Boa, with the event loop owned by the host
   process (network completions, timers, microtasks, rAF, then
   style/layout/paint, then the frame), and `fetch` registered as a host
   function so a page script never holds a socket: every request goes policy
   check, receipt append, then the wire. Off by default; the grant is a
   profile line pinned in the digest, and the capability manifest reports
   `javascript: false` while it is absent. The cost lives in the web
   bindings (DOM, events, timers, observers), not in Boa, and Test262
   conformance says nothing about the web platform, which is why this tier
   is last.

Exit criteria are numbers, not adjectives: less memory than headless
Chromium on the same page, both at rest and while screencasting; roughly
100ms from action to updated frame locally; rest-state CPU near zero.
"Rust, therefore light" is a hypothesis until measured.

Gated on two things: M9's findings say a light engine is actually usable for
the reading half of the loop, and Kitesurf's open-source release has landed so
the build-versus-adopt call is made with the code on the table, not the blog
post.

### M11. The developer-mode viewer: built, 2026-08-07

`d` in the terminal viewer splits the screen: the page keeps the top, and a
console/error pane takes the bottom third. What it shows was already arriving
and being thrown away. `ConsoleError` and `PageError` carried their text and the
viewer kept only a counter. Page text is passed through `sanitize_display`
before it is drawn, because a console message is untrusted input and would
otherwise repaint the viewer's own chrome.

The layout and the pane renderer are pure functions (`termview/panes.rs`) with
the split, the truncation, the bounded buffer and the sanitising all tested.
`App` stays the thin thing that positions and writes, which is why any of it is
testable at all. A terminal shorter than 16 rows keeps the whole page rather
than showing two useless slivers.

Not built: a per-request network pane. Nothing on the viewer's stream carries
requests, and the mediator's records are host-side, so that needs a source
rather than a layout.

**M11 (as proposed).** The terminal
viewer's default becomes a developer view rather than a page view: for a
coding agent's overseer, the rendered page alone is the least informative
pane. Something like

```
┌───────── page ──────────┬────── snapshot ───────┐
│    rendered frames      │ e12 button "Submit"   │
│                         │ e13 textbox "Email"   │
├──────── console ────────┼────── network ────────┤
│ TypeError at App.tsx:42 │ 200 GET  /api/user    │
│                         │ 500 POST /api/save    │
├─────────────────── actions ─────────────────────┤
│ click @e12 · fill @e13 "a@b.c" · snapshot       │
└─────────────────────────────────────────────────┘
```

The composition is cheap because every pane's source exists or arrives with
M8: termview already decodes frames (5.10), the drain already collects
console and network errors, and the mediated socket plus the Fetch lane turn
per-action and per-request evidence into live streams instead of a post-run
artifact. Input keeps the lock's semantics untouched: watching is the
default, `i` still takes control, and the takeover is how a login wall gets
answered (7.1). This is also the demo surface the open item 2 candidate
wants: the receipts, watched live.

Full loop the demo has to show:

```
agent edits code -> starts dev server -> opens the app with h5i browser
  -> reads the accessibility tree -> clicks and fills -> reads console and
  network errors -> screenshots -> fixes the code -> human watches or takes over
  -> export patch, report, screenshots, receipt
```

### M11a. The browser terminal: the event model and the evidence panes, built 2026-08-08

The half this entry called durable, and said would land
first, has: `browser_events` is the one stream, and the console reads it.

* **The model** (`crates/h5i-core/src/browser_events.rs`). Every event carries
  its lane *and* its grade, kept apart because they answer different questions
  and the interesting case needs both: our own engine's request log is
  **box-claimed** (written inside the box) and **fail-closed** (the engine will
  not fetch what it cannot record). Chromium's Fetch lane is box-claimed and
  best-effort. One "trusted" flag could not have said that. `caused_by` is set
  only where the source carries the link (a response to its request by
  sequence number, a refusal to the action that provoked it) and nowhere else,
  so no arrow on the screen is drawn from two things merely having happened at
  about the same time. Ingest sanitises every box string once, here, rather than
  in each renderer, because M11b writes this same text straight to a PTY.
* **Three real sources**, no placeholders: the light engine's request log, the
  mediator's actions, and the drained page evidence.
* **The mediator now writes its actions as data.** They were only ever on the
  receipt as *rendered text*, so a reader wanting them back would have had to
  parse a display format, the quiet-wrong-answer shape this file keeps
  recording. `browser-actions.jsonl` sits beside `receipt.jsonl`, host-side,
  where the box cannot write, and the round trip is pinned by a test.
* **In `h5i ui`, on the console's own terms.** One `GET`, the same token gate,
  no second web surface. Every row shows its lane and grade as words rather than
  as a colour, selecting a row lights what it caused and what caused it, the
  network pane names its engine's evidence grade in its header, and a dropped
  count is rendered rather than hidden.

#### Driven against a real box

**Not only a test client**, which is the gap M10 recorded and
this milestone was gated on not repeating. The real engine opened a page with
two refused subresources, wrote its own log into the box's `/tmp`, and the
console served the stream: both denials as `box-claimed` / `fail-closed`, each
with a `policy-verdict` naming the request that caused it; the cursor returning
only the tail on the next poll; an unauthenticated request refused with 401. Two
guards checked by making them fail: a Chromium box with that same log planted in
its `/tmp` yields **nothing**, because only our engine's log may wear the
fail-closed grade, and the mediator's sidecar shows up `host-observed` on the
box that has one.

**The finding, which cost the first live attempt.** `ResolvedPolicy::home_binds`
is `#[serde(skip)]`, so `host_tmp_root`, correct for a live run and the
only caller it had, returns `None` for **every** policy loaded back from disk.
The console asked a live-run question of a stored policy and got a silently
empty stream for a session that had one: enforcement-shaped code answering
"nothing to show" instead of "I cannot tell". The reader now takes the path from
`private_tmp_backing`, the same function that placed it.

#### Second pass, same day: its own tab, and the reader made honest

* **The stream is incremental and session-aware, which was a bug fix rather
  than an optimisation.** The first reader re-parsed every source per poll and
  numbered from 1 each time: stable only while files grow by appending, and
  they do not: every run clears the box's private `/tmp`, so a second browser
  run restarts the request log at zero bytes and restarts the numbering with
  it. A console tab open across two runs would have kept its cursor and
  silently dropped the head of the new session. The console now holds a byte
  offset per source, notices a file that got *shorter*, and emits
  `session-reset` as a visible row; ids never restart. Pinned by five tests
  driven against real files, including the partial-line and vanished-file
  cases, and confirmed live: with a viewer holding a stale cursor, a second run
  produced the reset row and then the new session's events, where before it
  produced nothing.
* **A per-box Browser tab.** Evidence is a scroll of what happened; the browser
  terminal is a live instrument, and wedging it between Services and the
  timeline gave it a few hundred pixels. It now takes the pane. The tab appears
  only for a browser box, and selecting another box returns to Evidence rather
  than showing a browser view of something that has no browser.
* **A page pane that says what it cannot show.** It reports whether a live view
  is running in the box (the same `.stream` discovery `h5i box view` uses) and
  names the command to attach, because the console watches and the *forward*
  carries pixels and input with the control lock on it. For an `h5i-light` box
  it states the engine-level caveat plainly: that engine has no resident
  session, so each `open` renders its own page and exits, and a live view shows
  **that** process's page rather than the one the agent is driving. An
  unlabelled viewport there would have been the most convincing wrong answer on
  the screen.

One bug this pass created and caught before it shipped, worth recording because
it is the same shape twice: the new `session-reset` event was added server-side
while the console's own union type and pane router still knew six kinds, so the
row was dropped silently in the browser, the swallow that had just been fixed
one layer down, moved one layer up. Found by grepping the *served bundle* for
the divider text rather than by trusting a green typecheck, which could not see
it: an unknown variant simply matched no case.

#### Third pass: the console carries pixels

The page pane shows the box's page,
rendered by our own engine inside the box. The frame lane is joined, and the way
it is joined is the point:

* **A reader, not a proxy.** A background thread per watched box enters the
  box's user and network namespaces by pid, connects to the stream server, and
  reads, the same route `h5i box view --term` takes (`view::connect_in_netns`),
  reusing the same hardened WebSocket client (`termview::ws`, which refuses
  reserved opcodes, masked server frames and oversized lengths). Nothing new
  listens; the box gains no reachability it did not have.
* **The console's structural guarantee survives.** Every route is still a `GET`,
  because the frame is served *as* a `GET` returning `image/jpeg`, with `nosniff` so
  crafted bytes cannot be re-read as anything else, `no-store` so a frame of
  somebody's page does not settle into a disk cache. And the relay is
  one-directional by construction: the only messages it can send upstream are
  `config` and `ack`, there is no path from an HTTP request to a write on that
  socket, and a test greps this module for `input_*` so the day someone adds one
  the build says so. Typing into a page still has exactly one door: the forward,
  which enforces the control lock.
* **Change-driven, end to end.** The stream reports the newest frame's sequence
  number and the page keys its `<img>` on it, so an unchanged page is zero
  requests rather than a timer redrawing a still picture, the engine's own rule,
  carried up to the browser.
* **The picture is labelled.** A frame is **box-claimed**: the box's rendering of
  its own page. Nothing derived from it reaches the trusted status row, and the
  `h5i-light` caveat sits under the image rather than being left for a reader to
  infer: that engine has no resident session, so a served view shows the page
  the *serving* process opened, which need not be the one the agent is driving.

Driven end to end rather than asserted: the engine served a page inside a
supervised box, the console found the `.stream`, crossed the namespace, and
returned a 1280×720 JPEG at `frame_seq 2` with the right headers; stopping the
in-box server flipped `live_view` to false, dropped the relay, and the frame
route went to 204. One test was rewritten on the way: clippy caught it
comparing two constants, which is a tautology that would have passed with the
size check deleted; it now drives the real decoder with real base64.

#### Still open, and none of it is dressing

The
accessibility snapshot has no live source (it is a CLI verb today). Takeover is
not wired here: the console remains read-only and input still goes through
`h5i box view`, so the read-only-by-default / interact-under-the-lock rule below
is *stated* by this milestone and *enforced* by the forward, which is one
surface short of the exit criterion. Nothing links an agent action to the
requests it caused, since neither the mediator's records nor the engine's log carries
the other's id, so "selecting an action surfaces its correlated request" holds
only for the verdict it provoked, and closing it is a change at the *sources*,
not in the viewer. M11b has not started, so the claim that two readers agree is
untested. The original entry follows.

**M11a (as proposed).** M11 put
the developer view in the terminal; this puts the full one where it can
actually breathe, inside `h5i ui`. The design motif is a trading terminal.
Hyperliquid is the reference, the way terminal-browser was for 5.10: what we
take is the information model (peer panes of equal rank, change-driven row
highlights, an always-on status bar), not the skin. The reasoning is the same
one M11 recorded: for an agent's overseer the rendered page is the *least*
informative pane, so page viewport, accessibility snapshot, agent actions,
network requests, console, and policy verdicts sit side by side at equal rank:
what the agent saw, what it did, what moved on the wire, and what h5i
refused, in one view.

**One web surface, not a second one.** This lives in the existing console:
same axum server, same embedded bundle, same `web` feature, same loopback
bind. The console's own rule, every route is a GET, stands; the live data
and the input direction ride the per-box forward that already exists (5.9),
with its per-box token and its lock check on input. The console gains a view,
not a write path.

**Not a read-only browser.** The viewer is read-only by default, interactive
only while holding the control lock (5.4), and taking the lock is itself a
recorded policy event: the takeover and the window in which human input
flowed belong in the receipt next to the verbs the mediator refused during
it. This is the terminal viewer's VIEW/INTERACT model (5.10) given a second
skin, not a new input policy; a viewer that could never take over would
delete M5's takeover story, and one that could always type would delete the
lock.

**The durable half is the event model, and it lands first.** One stream from
the browser runtime (frames, snapshots, actions, requests, console, policy
verdicts, metrics), with every event stamped with its session, ordinal,
timestamp, kind, a `caused_by` back-reference, and its **lane**:
host-observed or box-claimed, the same two kinds of claim the receipt
already keeps apart. The web view, the terminal view, and the exported
receipt all read this one stream, which is what makes the viewer a live
receipt rather than a dashboard that happens to resemble one: selecting an
action shows the request, console output, and verdict that carry its id.
The panes inherit the honesty rules with the data: the status bar shows
host-derived values only (box-claimed metrics are labeled, not promoted),
and the network pane names its evidence grade per engine: h5i-light's
fail-closed request log is authoritative, the Chromium path's Fetch lane is
best-effort, and a pane that showed both alike would read as enforcement
where there is none. Update budgets are per pane, not global: the viewport
is change-driven (the light engine idles at zero frames by design; ~30fps
is a Chromium screencast ceiling, not a target), status ticks slowly, rows
batch, histories are bounded rings.

The host browser trusts this page with nothing new: it renders pixels and
structured events, target HTML never enters the viewer's DOM, box strings
render as text (`sanitize_display`'s rule, applied in a second place), and
the CSP names no external origin.

Exit criteria: the console shows a live box with every pane labeled by lane;
selecting an action surfaces its correlated request, console output, and
verdict; a takeover started from the viewer types into the page and lands in
the receipt as a policy event alongside the agent verbs refused during it;
the network pane states its evidence grade per engine; and the TUI showing
the same session shows the same events, because divergence between the two
viewers is a bug in the model, not a difference of skin.

Gated on the shared event stream existing (this milestone's own first half)
and on M10's open item being closed first, the live view driven by a real
`h5i box view` against a real box, because a polished terminal over a
stream never exercised end to end inverts this file's own priorities.

### M11b. Terminal watch mode: proposed, 2026-08-08

The shipped terminal
viewer (5.10, M7, M11) re-pointed at the same event stream and kept,
deliberately smaller: viewport, trusted status row, latest actions, console
errors, denied requests, panes cycled rather than tiled. It is the SSH and
demo surface ("or stay entirely inside the terminal"), and it does not
chase pane parity with M11a: the investment moves to the web view, and the
TUI's job is to watch, take the lock when a login wall demands it, and prove
the event model has two independent readers. Nothing shipped is discarded.

### M11c. Two audit surfaces: a decision stream, and the page beside its cost. Proposed, 2026-08-19

M11a built the event model and the console's evidence panes; M11b keeps a
smaller terminal viewer over the same stream. Both are surfaces for **watching a
box**. This entry adds the two that are missing, and they are missing in
different directions: one is for the person running the box, the other is for
the person reading the page.

The framing that produced both: **a log is not an answer.** `receipt.jsonl` with
two thousand rows is evidence and nobody reads it. The questions a human
actually arrives with are few — did anything leave, was anything refused, what
did the agent read before it wrote this, is this record real — and an audit
surface should be shaped like those questions rather than like the storage.
Only the first two are in this milestone.

#### 1. `h5i box watch`: one line per decision

A non-interactive stream of policy decisions as they are made. Not a viewer:
no viewport, no panes, no cycling, no lock. It is the `tail -f` of the receipt
and it is meant to be piped, grepped and left running in a second pane.

```
$ h5i box watch mybox
14:02:11  net      ALLOW  GET   https://docs.rs/blitz/0.3.0/  (12 KB, 84ms)
14:02:11  net      DENY   GET   https://telemetry.example.com/collect
                                 net.egress does not list this host; nothing left
14:02:12  browser  click  @e3 "Sign in"
14:02:12  net      ALLOW  POST  https://app.local/login  (cookies: 1)
14:02:13  exec     cargo test
```

**This is distinct from M11b and does not replace it.** M11b is a pane-based
TUI with a viewport, which is a thing you sit in front of. This is a line
stream, which is a thing you leave running. The distinction is worth keeping
because the reason to build it is behavioural rather than technical: trust in a
sandbox is built by watching it once, seeing it behave, and then stopping. A
surface that must be opened and attended to is a surface that is not used after
the first week, and `--deny-only` is the form that can be left on forever.

Requirements that follow from the rest of this file:

* **Third reader of one stream.** M11a's whole point is that `browser_events`
  is the single stream; M11b was gated on proving it had two independent
  readers. This is the third, and it must consume the same model, including
  `lane` and `grade` as words. A row whose grade is `box-claimed` says so here
  too; the terse format is not licence to drop the qualifier that makes the row
  honest.
* **Sanitised once, at ingest.** Already true (M11a), and load-bearing here
  because this writes box-supplied strings straight to a terminal, exactly as
  M11b does.
* **Refusals are the headline.** `--deny-only` is the flag; a run that refused
  nothing should be able to say that in one line rather than in silence.
* **`--json` is the record, the default is the answer.** The same split the
  session verbs use.

#### 2. The page beside what it cost

The console draws a network pane and a viewport. It does not draw them
*together*, and for this engine that is the picture no other browser can
produce: the rendered page, and directly beneath it every request that was made
to render it, each with its verdict. "What did looking at this page cost, and
what was refused while I looked" is one glance rather than two panes and a
correlation done by eye. `caused_by` (M11a) already carries the links needed to
draw it.

Second, and smaller to build than to decide: **draw the fence.** The snapshot
wraps page content in `--- BEGIN/END UNTRUSTED PAGE CONTENT ---` (§12.1) because
that is the moment attacker-controlled text reaches something deciding what to
do next. The console currently renders page-derived text without that boundary
being visible, so the human reader is given *less* framing than the model is.
Rendering the same fence in the UI costs almost nothing and removes an
asymmetry that is hard to defend once noticed.

Neither of these is a new evidence source. Both are M11a's stream, arranged so
the arrangement itself carries the argument.

#### What was considered and rejected: putting receipts in git

The tempting version of "make the evidence live where review already happens"
is a receipt digest in a commit trailer, or the bundle summary in
`git notes --ref=h5i`. It is cheap, it survives forever, and it appears in the
pull request without anyone opening a tool.

**Refuse it.** Two reasons, and the second is the one that settles it:

1. It is a second export path that does not pass the export gate. §5.6's
   redaction and size caps apply to `h5i box export`; a note written beside it
   inherits none of that, and receipts carry URLs, hostnames and query strings
   that are exactly where a token ends up.
2. **It is unretractable.** A note or trailer that has been pushed is on every
   clone and in every fork, and a force-push does not recall it. Every other
   evidence path in this design is a local artifact that a person chooses to
   hand over. This one publishes by default, and "the agent's record leaked the
   credential the agent was careful with" is a failure this project should not
   be able to have.

Recorded here so it is refused in review rather than re-argued. If the pull
request really is the right place for this, the thing to put there is a
reference to a bundle, produced by the gate, that someone chose to share.

#### Order, and what this does not include

`h5i box watch` first: it is small, it consumes a stream that already exists,
and it is the surface that makes the guarantee visible day to day. The paired
view second. The fence line can land with either.

Deliberately **not** in this milestone, and both larger than it:

* **`h5i box why <name>`**, a provenance query rather than a log reader
  (`--reached <host>`, `--wrote <path>`). The interesting one, and the one that
  has to be honest about a hard limit: what the receipt holds is *temporal
  co-occurrence*, not causation. "The agent read these pages before it wrote
  this file" is true and useful; "these pages caused this file" is not
  something the record supports. It is buildable only if it says which of the
  two it is, in the output, every time. Left out until that wording is settled,
  because the failure mode of getting it wrong is this project's worst one.
* **`h5i verify <bundle>`**, the third-party check, which needs §B11.5.16's
  signed receipt before it means anything. Its UX shape is a one-line verdict
  followed by an explicit statement of what the record does **not** cover, which
  is the same instinct as `capabilities` (§12) and `unsupported()` (§B8.4)
  applied to the audit trail. That paragraph is the differentiating part of the
  feature, not a caveat attached to it.

### M12. Share: built, 2026-08-10 (5.11, 5.11.1)

The bridge first, because it
is the part both transports share and the part that touches the boundary:
netns dial-in, the grant table with mint / verify / expire / revoke, the HTTP
gate, and the ingress receipt lane. Then iroh and `h5i join`. Then the quick
tunnel on the same bridge. Viewer sharing was explicitly not in this milestone
and is not built.

#### What was verified, and how

**What is demonstrated, and by what.** The suite covers the whole P2P chain
end to end in-process (QUIC handshake, greeting, grant table, the dialer's fd
handoff, the byte pump) with a wrong ticket refused on the same connection and
a revoke written by another process stopping the next one. The tunnel front is
driven exactly as `cloudflared` drives it, including against a dev server
written to ignore `Connection: close`, which is what pins the one-request rule.
The gate's promise that the share credential never reaches the box is pinned by
reading the rewritten head.

**Run for real on 2026-08-10, and this is the part that was open.** A live
`supervised` box with a dev server inside it, shared over iroh, joined from a
second `h5i` process, and fetched with `curl`:

- the invite bounced to a cookie (`h5i_share_40959`, port-scoped as designed)
  and the box's HTML came back through the joiner's loopback proxy;
- the path was **direct**, hole punching rather than a relay, with the endpoint's
  real addresses in the ticket;
- a request with no cookie and one with a wrong cookie both got `401`, and
  neither reached the box;
- `h5i box share revoke` from a third process cut the peer off; the joiner
  printed the sharer's own close reason rather than a transport error;
- the export's receipt named the peer, the grant and its label, the window, the
  connection count, the byte counts and the path:
  `08e03775419e… via direct — grant 38bd63e2 (reviewer), 14s, 1 connection,
  97 in / 412 out`. The connection count is *one*, because the redirect and
  both refusals were answered by the joiner's own proxy and never crossed,
  which is the gate working, visible in the evidence.

**Re-run on 2026-08-10 after the third round of fixes**, because that round
rewrote the response path: seven sequential requests each on their own
connection (the receipt says so), `Connection: close` in every answer, and a
`HEAD` returning in five milliseconds where the version before it would have
waited three hundred seconds for a body that a `HEAD` never has.

**The tunnel, run for real on 2026-08-10.** `cloudflared` was installed and a
quick tunnel carried live traffic over the internet: the invite link bounced
into a `Secure` cookie, `GET`, `HEAD` and a 300 KB `POST` all came back from the
box, an anonymous request and one with a wrong token both got `401`, and the
receipt named the transport, the grant, six connections, 678 KB in and 676 KB
out, one refusal, and the "not end-to-end encrypted" note.

Two things the live run found that no test would have. A `POST` answered `501`
and the first diagnosis, that `cloudflared` chunks every body and the proxy
refused chunked, was **wrong**: the `501` came from the box's own
`python -m http.server`, which has no `do_POST`. (Chunked request bodies are
forwarded now rather than refused, which is a real improvement and was reachable
from a direct client; it was not what that `501` was.) And killing the box's
session left the share answering `502` forever with nothing said, because the
dialer's helper lives *inside* the box's network namespace and keeps it alive
after everything else in it has gone, so a box restarted afterwards gets a new
namespace the share can never reach. The share now notices and ends.

**The whole response matrix, run over both transports on 2026-08-10.** A dev
server in a box answering a page, a `304`, a `HEAD`, a chunked response, a form
`POST`, a chunked `POST` and an `Expect: 100-continue` upload, every shape the
framing code had to be rewritten twice to get right, with an anonymous request
refused alongside them. All of it in single-digit milliseconds on the P2P path.
The two receipts record seven and six connections, which is one per request,
which is the one-request rule visible in the evidence.

**Hot reload, run for real on 2026-08-10, over both transports.** A dev server
in a box answering a `Sec-WebSocket-Key` handshake with a genuine `101`, driven
from a client that speaks the frame format: `echo:reload-please` came back
through a Cloudflare quick tunnel, and again over a direct P2P path. Both
receipts record it as two connections, which is what a page plus a socket is.

**Two peers and a per-person revoke, run for real on 2026-08-10.** A tunnel
share with two grants: both admitted, `share revoke` on one, and the other kept
working: `200` and `401` from the same URL a second apart. The receipt lists
them separately by grant and label, with the revoked one's traffic still counted
and the refusal recorded as revoked rather than unknown. That is the property
the whole grant model exists for and it had never been exercised outside a test.

Also verified live, and worth recording because it was a defect this branch
introduced and fixed: two `h5i join` sessions on one machine, a browser holding
both of their cookies, and the box seeing neither, only the app's own `sid=9`.

#### The review rounds, and what each lens found

**Rounds 8 to 10, and what live running kept finding.** A ticket expiring on
its own, neither revoked nor interrupted, ends the share, writes the receipt,
clears the record, and now tells the joiner why; that path was verified twice
because the first fix for it was inert. A dev server that rejects a request
before reading its body has its own answer relayed rather than replaced. And a
`--tunnel` share with two grants had one of them revoked while the other kept
working.

**Rounds 11 to 14, and the two that would have bitten a real user.** A client
that sends its request and then shuts down its write side, which is legal HTTP/1.1 and
what anything built out of one write and one read does, had that EOF read as
"the visitor left", so the relay stopped on the spot: a 2 MB download arrived as
63 bytes, with a clean close and nothing recorded anywhere. And `h5i join` was
hung up on by the sharer thirty seconds after connecting, because the sharer
drops a connection that has never authorized a stream and the joiner did not
open one until somebody visited the page, so the ordinary sequence (send a
ticket, they join, *then* they open the browser) killed itself. The joiner now
presents its ticket once at connect time, which fixes that and makes "joined" a
statement about the ticket rather than about the network: a revoked ticket fails
at `join` instead of at the first page load.

The same rounds found `share status` rendering every share as `0m left` for its
final minute, one column away from `expired`; `share grant` racing `share stop`
closely enough to bring a stopped share back to life; a `revoke` on a crashed
share reporting that connections had been dropped; a reused pid producing a
share that could be neither stopped nor restarted by any verb; and Ctrl-C being
swallowed for the whole six-second teardown on three of the four ways a share
ends.

**Round 15, and the fix that was worse than the bug.** Making Ctrl-C responsive
during the teardown was done by arming the hard-exit watcher after the select.
On the three exits where no signal had been delivered yet, that meant the
operator's *first* Ctrl-C hit a watcher built for their second: it printed
"interrupted again", threw the receipt away and exited. Pressing Ctrl-C once to
get a prompt back destroyed the one artifact this feature exists to produce, and
said they had done it twice. An interrupt during the ending now means "stop
waiting", not "stop recording"; only a second one exits without a receipt.
Verified live three times out of three.

The same round found that the join-time ticket check, itself a fix from the
previous round, went the whole way into the box, costing a connection to the
dev server and one of the share's 64 slots per join; that a new joiner against
an un-updated sharer would be told its ticket was revoked, forever, because the
greeting changed without the ALPN changing; and that `clear`, `clear_now` and
`forget` deleted whatever record was on disk without checking whose it was,
which a `stop --force` followed by a fresh `share` turns into one process
deleting another's grant table after its ticket has been given to a human.

**Round 16 was a fuzzer rather than a reader**, on the argument that fifteen
rounds of adversarial reading had started mostly finding the previous round's
work. `crates/h5i-share/src/fuzz.rs` generates request and response heads from a
grammar seeded with every awkward token the earlier rounds turned up, mutates
them, and asserts the properties the rest of the crate is entitled to assume:
the credential never reaches the box, a redirect never leaves the origin, one
framing and one `Connection` on the way out, line discipline in both
directions. It is deterministic, prints the seed for any failure, and
`H5I_FUZZ_ROUNDS` turns it into a soak.

Twenty million heads found two defects a person had not: `split_cookie` applied
its "nothing named like ours goes upstream" rule on the branch where a cookie
has an `=` and not on the branch where it does not, and a response head with no
status line at all was relayed with a *header* promoted into the status line's
place, which a browser reports as a protocol error and nobody can trace. Two of
the four failures it reported were the invariants being wrong rather than the
code, which is its own kind of useful: `007` is a legal `Content-Length` and a
cookie named `999h5i_share` is somebody else's.

**Rounds 16 to 26 changed the kind of reader**, on the argument that fifteen
rounds of adversarial reading had started mostly finding the previous round's
work. A fuzzer, an end-to-end script that automates the live checks that had
been done by hand for five rounds, a leak hunt, a flake hunt, the two capacity
ceilings nothing had ever driven, an accounting sweep of every counter, and,
the one that found most, a review from the **joiner's** side, asking what a
hostile *sharer* can do to the person who pasted their ticket.

That last direction had never been examined. It found that the joiner's
handshake had no deadline on any of its three steps, so a sharer who simply
never answered left `h5i join` hung with nothing printed at all; that a page
served on the joiner's loopback could register a service worker, which outlives
the share and keeps control of that address afterwards; that a ticket's
addressing went to iroh unexamined, so one naming `127.0.0.1:2375` made the
joiner dial a service on its own machine; and that the QUIC close reason, which
the sharer chooses, was printed to the joiner's terminal unsanitised, the same
escape-injection the `box_id` fix had just closed, through the field next to it.

The fuzzer needed a round of its own, too. Measured against the real parser,
1.9% of its heads were parseable, **none** of two million carried both framings,
and about one per run carried a credential, so "twenty million heads pass" was
true and meant almost nothing. Sampling the line ending once per head rather
than once per line, and leaving two thirds of heads unmutated, took those to
18%, 0.8% and 0.8%; the test now asserts floors on all three, so a generator
that stops reaching the code fails instead of passing.

**Rounds 27 to 36** kept changing the lens. Two more directions had never been
looked at, and both paid: a review from the **joiner's** side (what a hostile
*sharer* can do to the person who pasted the ticket) and one of **how a live
share interacts with the rest of h5i**: the lifecycle verbs, the export, the
console, and the fact that a share holds a box's namespace open.

The worst thing either found: **a share of a box at the `process` tier with a
profile that denies egress can never work, and the docs recommended exactly
that configuration.** Such a box gets a network namespace of its own with no
loopback brought up in it, so nothing inside can reach even itself. The share
started, printed a ticket, and left both people reading messages about a dev
server that was running the whole time. It is refused now, by name, and the
MANUAL and the skill no longer name that tier as an option.

Second: **a share pins one namespace at startup and only asked whether the box
had *any* session.** Every session gets a new namespace, so somebody who exits
a shell and starts another, or who has a read-only observer attached while
they restart, left the share serving a namespace nothing was in, with
`share ls` reporting it healthy. It compares the namespace now.

Third, and the same argument for the third time: the wire had four reply codes
and no way to say "h5i cannot reach the box". The receipt learned to tell that
apart from "your dev server is down" in round 19; the joiner's browser was
still being told to go and ask the sharer to start a server that was running.

**The three findings rounds 27-36 recorded and did not fix** are fixed now, and
each was verified by reproducing it first.

`cloudflared` outlived a `SIGKILL` of the share by more than twenty seconds,
with its public `trycloudflare.com` hostname still registered and still
pointing at a loopback port that had just been freed, so for that window
anything on the machine that bound it was on the public internet under a
hostname h5i minted. `kill_on_drop` is a destructor and `SIGKILL` skips
destructors; `PR_SET_PDEATHSIG` is the kernel doing it instead. Measured: gone
in one second, against twenty-plus with the change removed.

macOS carried that hazard in full until it was closed the only way Darwin
allows. `PR_SET_PDEATHSIG` is something a process asks *for itself*, so Linux
sets it in the child between fork and exec; nothing can ask it on behalf of a
binary h5i does not compile. So a watchdog process waits on `kqueue` for either
the share or `cloudflared` to exit, and kills the tunnel if the share went
first, a separate process precisely because a `SIGKILL` cannot skip what is
not running the share's code. Both pids are watched, not just the share's: a
watchdog armed on one pid alone would outlive the tunnel it was guarding and
eventually `SIGKILL` whatever inherited the recycled number. Measured on macOS
the same way: the public hostname survived the full ten-second observation
window before, and the tunnel is gone within 250 ms after.

`h5i box rm` did not know what a share was. A shared box is almost always also
`running`, so the operator was told to abort the box and never that somebody
outside was connected to it, and the check has to sit *above* the status guard
or it is unreachable. Worse, a share that outlived the removal wrote its
receipt afterwards, and `receipt::append` creates the directory it writes into:
the box came back as a receipt log and a payload under a path with no manifest,
which every tool answers "no environment named that" for and only `rm -rf`
clears. The receipt is skipped when the box is gone, which loses it. The right
trade, since it is evidence about something that no longer exists.

And the console showed nothing at all while a box was open to somebody. The
receipt lands when the share *ends*, so the one lane that lets somebody **in**
was the one lane the console could not see while it was open. `shared_now` is
on the box row now.

The pattern across all fifteen rounds is worth recording, because it is the
argument for having run them: **every round found real defects in the previous
round's fixes**, and five of the sharpest were fixes that did nothing at all: a
`Connection: close` the box could ignore, a shutdown signal that was sent after
the shutdown, a flag that recorded truncation for the rarest of the four ways a
response gets cut short, and a linger drain whose two dedicated tests both
passed with it deleted, and a signal handler armed for a second Ctrl-C that
caught the first. That linger drain is now documented as what it is: bounded,
kept for the sake of the intermediary on a tunnel share, and not a thing we can
show changes what a visitor receives on Linux.

`--direct-only` has been run, and it does what it says on the half that can be
run here: the share starts, the peer gets a direct path, traffic flows, and the
receipt records `via direct`.

**macOS now has a route, and it is a different argument rather than the same
one ported.** A Seatbelt box has no namespace to enter and binds the host's
loopback, so "the box's port 3000" and "this machine's port 3000" are one port,
which is why an earlier macOS arm, deleted in round 51, was wrong to connect
to it and call whatever answered the box. What replaces it (`share::owner`)
asks Darwin which process holds the listening socket and shares it only when
that process is in the box's tree; a stranger, or a second process sharing the
address, is refused and named. The check is redone on every dial, so a dev
server that exits cannot have its share inherited by whatever claims the port
next.

That this is not a theoretical hazard was demonstrated by the machine it was
written on: port 3000 was held by the box's `python3 -m http.server` on `::`
*and* by an unrelated `serve.py` on `127.0.0.1`, and a plain loopback connect
reached the stranger. Run end to end on macOS: share, `h5i join`, a direct
QUIC path, and the visitor receiving the box's directory listing rather than
the stranger's page. The three outcomes were each exercised: the box's port
shared, a stranger's port refused by name, and an empty port warned about
rather than refused. Boxes at the `container` and `microvm` tiers on macOS live
inside a VM where no host process holds the port; they are refused with that
reason rather than "nothing is listening".

**Nine review rounds over that route (36–44) found six defects, and the shape
of them is the point.** Only one was in the reasoning; the rest were in what
the code could *see*.

- The pid-identity hardening added for `session_pid` turned `h5i box share`
  **off** on macOS entirely: `proc_start_ticks` read `/proc`, answered `None`
  everywhere else, and the verified reader skips records that cannot prove
  themselves. Both halves were individually correct and tested; no test held
  the three together, and the platform where it broke was the one CI cannot
  run the command on.
- A pid that changed hands between the tree snapshot and the socket scan was
  vouched for by the snapshot. Re-asked upwards from the winner now.
- Both kernel scans sized their buffer once and added fixed slack. The kernel
  never says "there was more", and both lists are ordered, so a process that
  opens descriptors faster than the guess can push its own listening socket off
  the end of the scan, and a listener h5i cannot see is one it cannot refuse.
  Both grow until the answer provably fits.
- The refusal named the offending process, and that name is the executable's
  file name, chosen by whoever started it. A binary named with a literal `ESC`
  wrote escape sequences into the sentence an operator reads while deciding
  whether their port has been taken. Sanitised through the same helper the rest
  of the repository already used.
- A newborn child inherits its parent's descriptors across `fork`, so between
  `fork` and `exec` it really does hold the dev server's listening socket,
  and judged against a snapshot taken microseconds earlier it is a *stranger*
  co-holding the box's address, which is a refusal. A busy box therefore
  refused its own visitors in proportion to how busy it was. Found by a
  concurrency test on its first run, with `/usr/bin/true` reported as
  co-holding a port.
- And the module's own note claimed a refusal it could not make: a listener
  belonging to another user is never *attributed* to the box, which is safe,
  but neither can it be counted as a competitor, so "unambiguous" rested in
  part on not having seen what this process may not see. Recorded as a limit
  rather than argued away.

The through-line: on Linux the namespace makes the guarantee true by
construction, and there is nothing to observe. Here it is established by
observation, and every defect but one was the observation being incomplete,
which is the failure mode this approach has and the namespace does not, and is
worth stating plainly wherever the two are compared.

#### Still not demonstrated

The two h5i processes were on one machine: a real
direct QUIC path through the host's network stack, but not two machines on two
networks. On macOS the two-machine half is likewise untried, and `SO_REUSEPORT`
contention is covered by unit tests over the decision rule rather than by two
real processes racing for one address. And `--direct-only` has never been
exercised against a hole punch that actually *fails*. The refusal is the half
that matters and it needs two hostile NATs to reach. Those are what remains of
the exit criteria.

### M13. The microvm tier, warmed: steps 1 and 2 built 2026-08-13, step 3 proposed

The tier shipped correct and slow: one `msb run` per command, a full guest boot
each time, torn down on drop, and — as 9. said until today — never booted end
to end anywhere. A reading of forkd (deeplethe/forkd, a Firecracker runtime
whose whole premise is fork-from-warm: each child `mmap`s a warmed parent's
snapshot memory copy-on-write and spawns in ~100 ms instead of booting)
sharpened what to do about that into three steps, in order, each gated on the
one before.

**First, demonstrate and measure: done, and it moved the plan.** The tier
boots a real guest — a microvm box creates, runs, enforces its allowlist in
the guest netstack, and exits 0 — so the "not yet demonstrated end to end"
caveat is retired. The numbers are in `docs/benchmarks/microvm-boot.md`,
taken with `scripts/bench_env_overhead.py`, which is committed so they can be
re-derived rather than trusted. What it borrowed from forkd was the
discipline, not a mechanism: every sample kept, the tier that could not run
recorded with the probe's own refusal, and the null results written down.

Three results changed what steps 2 and 3 should be:

- **461 ms of fixed cost per command, and almost none of it is isolation.**
  Subtracting each tier's own no-op cost, the VM's per-syscall charge is
  −7.3 ms — noise around zero — and it does not slow CPU-bound work. The tier
  is not a slow place to work, it is an expensive place to start, which is the
  cost profile that amortises and the reason reuse leads.
- **A third of that tax is the memory cap — and it belongs to step 2, not
  before it.** Adding one h5i behaviour at a time to a bare `msb run` found
  mounts nearly free (+17 ms for the full 16-mount set over a 74 MiB
  `.git/objects`), egress rules free, and the preload script +9 ms — all three
  inside the control's own 230–245 ms run-to-run drift, so read as no
  measurable cost — but the profile's **8 GiB `mem_bytes` costs +154 ms on
  every command**, scaling at roughly 20 ms per GiB.

  This was first written up here as a free win available today. **That was
  wrong, and testing it is what corrected it.** Guest RAM *is* the memory
  limit at this tier, so simply lowering the number trades enforcement
  headroom for latency. The trade can be avoided — `msb` takes a
  `--max-memory` hotplug ceiling independently of `--memory`, and booting
  `512M` with `--max-memory 8G` costs 237 ms against 384 ms — but
  `--max-memory` **does not grow anything by itself** (a 512M/max-8G guest
  fails a 1.5 GiB allocation exactly like a plain 512M one). Growth takes an
  explicit `msb modify --memory`, which works on a live guest in ~9 ms, is
  asynchronous ("converging"), and **keeps the cap honest**: 6 GiB against a
  4 GiB ceiling still fails with `MemoryError`. But `modify` needs a named,
  running sandbox, and today's tier destroys the guest after one command, so
  there is no moment to issue it. The 141 ms is real, recoverable, and
  reachable only once guests persist.
- **The ordering inverts on macOS.** `microvm` runs a realistic short command
  in 474 ms against `process` at 1604 ms and `supervised` at 1629 ms, because
  those two add ~1.5 s to Python startup under Seatbelt while the VM adds
  none. The strongest tier is the quickest of the three here. That Seatbelt
  cost is undiagnosed, is not a fixed cost reuse can hide, and belongs to its
  own investigation rather than to this milestone — but it means "microvm is
  the slow tier" was never quite the right framing on this platform.

**Second, amortize the boot: session-scoped guest reuse. Built 2026-08-13, and
it delivered 10.7×.** Fixed cost per command fell from **461.0 ms to 43.0 ms**,
which makes `microvm` the *cheapest* of the four tiers on this host —
`workspace` is 53.0 ms, `process` 62.9 ms, `supervised` 98.9 ms. The strongest
boundary is now also the least expensive to cross, because what is left at
every tier is h5i's own CLI overhead and this path has the least host-side
machinery to set up.

`crates/h5i-sandbox/src/microvm.rs` grew a warm path beside the one-shot one,
which stays and is still reachable by `H5I_MICROVM_NO_REUSE=1` — the escape
hatch, and the per-command-freshness option 9. promises. **The guest name is a
SHA-256 over its own create argv** (`h5i-<box>-<digest12>`), which is what makes
the fail-closed rule structural instead of a check somebody has to remember:
image, mounts, memory or egress change → different name → new guest → the old
one reaped, so a box can never be served a guest still enforcing a policy it no
longer has. Verified end to end: widening a box's allowlist rotated it from
`…08839c…` to `…a4d7c6…`, the old guest was reaped, and the new one enforced
the wider list. Deliberately *not* the pinned policy digest, which excludes the
runtime-only mounts by design.

Per-run credentials reach the guest through one small host-owned directory
mounted at `/.h5i/run` rather than `msb exec -e`, preserving the property this
module exists for — no value ever appears in a host command line — and the
staged script is unlinked when the run ends. `cache_write` runs stay one-shot,
being the only ones whose mount set differs from the box's.

Two things the build found that the design had not:

- **The 25 ms completion poll became the dominant cost** once the boot was
  gone. It was flagged in the very first benchmark (2026-07-19) as inflating a
  4 ms command to 30 ms, and it turned a 9 ms exec into 35 ms. A backoff
  (1 ms doubling to 25 ms) brought h5i's reported wall to 10 ms, matching the
  runtime's own cost, and the fixed cost from 65.5 ms to 43.0 ms.
- **The orphan sweep had never reaped anything.** `marker_path("").parent()`
  walked up past the marker directory (joining an empty component leaves a
  trailing separator), so it scanned `/tmp` for names that live one level down,
  matched nothing, and said nothing, being best-effort throughout. Harmless
  while guests died with their process; not harmless now that they outlive it.
  Fixed, with a test.

A security review of the branch reported no exploitable finding, and closed two
candidates for reasons worth keeping: the unvalidated-name path into
`msb remove --force` was **pre-existing and strictly more reachable before**
this work (the `parent()` bug meant the sweep read `/tmp` itself, so a marker
needed no directory squat at all), and the 48-bit name digest is not grindable
by anyone in the threat model — `sanitize_label`'s output is not in the hash
input, so the box-controlled half of the name buys no freedom over it, and a
colliding guest would mount a different workspace and fail loudly rather than
run under a laxer allowlist.

It did surface a real multi-user correctness bug, now fixed. **Markers decided
which VMs get destroyed while living in a directory shared between logins.** On
a shared Linux host whoever ran the tier first owned `/tmp/h5i-msb-live`;
everyone else's marker writes then failed silently, so their guests were never
reaped — and worse, their sweeps read the *first* user's markers and saw
`exists() == false` for a workspace under a home they cannot traverse,
concluding a live box was gone and removing its VM. Three changes: the marker
directory is now per-user (`$XDG_RUNTIME_DIR/h5i/msb-live`, falling back to a
uid-scoped temp path) and is refused unless it is a real directory this user
owns that nobody else can write; `box_is_gone` distinguishes a definite
`NotFound` from "cannot look", so an unreadable workspace costs a leaked VM
rather than a destroyed one; and only names shaped like the ones this module
emits (`h5i-` plus lowercase alphanumerics and dashes) may reach the runtime at
all, which also means no marker can present a flag-shaped argument to `msb`.

The original analysis, and what it was measured against:

**The prerequisite was answered — `msb` supports this, and it is measured.** `msb create --name X`
boots a guest detached and `msb exec X -- cmd` attaches to it over its agent
relay socket without booting. Measured on the same host: **233.9 ms cold per
command against 8.4 ms warm, 28× on the `msb` primitive alone**, and the warm
path is independent of guest size (8.9 ms into an 8 GiB guest, the same as
into a 512 MiB one), so reuse absorbs the memory cost of the previous
paragraph as well. Against h5i's 461 ms of fixed cost, a warm guest reachable
in ~9 ms is roughly **50×**. State persists across execs as expected.

So the shape is: boot on a box's first command, exec later commands into the
same guest, tear down with the box or an idle timer. This is backend-neutral
and it is the only speed move that works on macOS, where libkrun has no
snapshot or restore and fork-from-warm cannot exist. It is also the
architectural unlock for three gaps 9. lists as costs: a persistent guest is
what background services (`box service`), port-based share, and an in-guest
tee shim each require before they can be built at this tier.

**The semantics question is decided, and 9. now states it: reuse is the
default.** Reuse means commands in a box stop getting a pristine guest each
time, which reads as a weakening until you notice that `workspace`,
`process`, `supervised` and `container` have all always shared state across a
box's commands — the worktree is the whole point of a box. Per-command
amnesia at `microvm` is an artifact of shelling to a one-shot `msb run`, not a
promise the tier made, so ending it is alignment rather than a loss. The
boundary that carries the security claim is box↔host and box↔box, and neither
changes: separate boxes still get separate guests. Recreation per command
stays available for anyone who wants today's behaviour; it just stops being
the only option. The one hard requirement is the digest rule in 9. — a guest
whose policy has changed underneath it is recreated, never reused.

Not borrowed from forkd here, deliberately. Their answer to the same question
is "fork a fresh child per task", which needs a memory-fork primitive `msb`
does not have (its snapshots are disk-only and offline) and economics we do
not have (~20–100 ms to re-create, against our 237–460 ms). It is also the
weakest part of their implementation: `DESIGN.md:118-128` describes per-child
overlayfs in the present tense, `grep` finds no overlayfs anywhere in the
shipped code, and children in fact share one read-write rootfs file whose
writes are cross-visible and durable — a cross-sandbox channel their
`SECURITY.md` does not mention. The transferable lesson is the one their
`/tmp`-as-tmpfs convention encodes (name one place where guest-local writable
state belongs) plus the negative one: a design doc that drifts into the
present tense about unbuilt behaviour is how that happens.

It should carry the memory trick from step 1, because a persistent guest is
what makes it usable: `msb create --memory 512M --max-memory <ceiling>` boots
at 237 ms instead of 384 ms, one `msb modify --memory <ceiling>` at ~9 ms
restores the enforced cap, and every exec after that is ~9 ms. That removes
the +154 ms from the one boot per box which reuse alone cannot amortise, and
the enforcement claim survives it. Two constraints the measurements attach to
it: the resize is asynchronous, so a box whose first command immediately wants
4 GiB may meet a guest that has not converged yet and needs the modify issued
at create time rather than lazily; and **an exec can hang** (see the anomaly in
`docs/benchmarks/microvm-boot.md` — twice in ~100, undiagnosed, unreproduced
in 6 controlled attempts), so exec needs its own deadline the way `wait_vm`
already gives `msb run` a host-side backstop.

**Six prerequisites measured before writing any of it (2026-08-13), because
two assumptions had already died that way.** None reshaped the design; one
resized it. **A state check costs ~7.5 ms** (`msb list --format json`, the
cheapest of list/status/ping), which is the same order as the exec it guards,
so checking before every command roughly *doubles* per-command cost to ~16 ms
rather than being free — still ~29× better than today, but it means one list
per run, not one per decision, and it is the reason to keep guest state in the
box manifest rather than re-derive it. **Mounts are live in both directions**:
a host write after boot is visible inside a running guest and vice versa,
which is what lets per-run credentials go through the existing `/.h5i/spool`
mount instead of `msb exec -e`. **`--timeout` enforces exactly** (2 s killed at
2.02 s, `rc=1`, "exec timed out after 2s"), so the profile wall clock survives
the switch. **`--tty` works under a real pty** (guest reports `/dev/pts/0`,
`TERM=xterm-256color`) and `--no-tty` under a pipe, so `box shell` and captured
runs both keep their current shapes. **Eight concurrent execs into one guest**
all returned their own correct output in 26 ms of wall clock, so a single warm
guest is not a serialization point — which is what later makes `box service`
and the browser sidecar plausible here. And **names take 128 characters,
dots and underscores, but reject `/`**, so a box id like `env/human/slug` must
be sanitized before it can carry a policy digest in the guest name.

**h5i must track guest state, and `msb exec`'s auto-start is a trap.** An
earlier draft of this section said the opposite, on the strength of upstream's
"exec auto-starts a stopped sandbox" — measurement corrected it. Exec into a
**running** guest is 8.5–9.3 ms. Exec into a **stopped** one is ~236 ms *and
leaves it stopped*, so it is a one-shot boot wearing the fast path's name:
every later exec pays the same again and the guest never re-warms. An explicit
`msb start` (~143 ms) is what returns it to `running`, after which execs are
9.3 ms again. So an idle timeout that stops a guest silently reverts the tier
to its current per-command cost, permanently, until something starts it — the
reuse design has to own the state machine rather than lean on exec's
convenience.

Four more things the upstream check turned up that the design has to carry.
`--idle-timeout` and `--max-duration` exist but **have no default**, so a
detached guest outlives its box unless h5i sets one — the orphan-marker sweep
becomes load-bearing rather than a backstop, and `msb touch` is what keeps a
guest alive during an active session. `msb exec` takes `-e KEY=value` on argv,
which is the same `/proc/<pid>/cmdline` exposure the preload script exists to
avoid, so that mechanism carries over unchanged and costs ~9 ms. There is **no
daemon** — 0.2.x's `msb server` is gone and each sandbox is its own detached
host process, so a pool's ceiling is host RAM. And the upstream repo has moved
from `microsandbox/microsandbox` to `superradcompany/microsandbox`, so the
references in our docs and error strings will rot. The lifecycle shape to
extend is `SandboxGuard` and the orphan-marker sweep in
`crates/h5i-sandbox/src/microvm.rs`, which already own naming and cleanup.

**Third, on Linux only, a second backend: fork-from-warm — and step 1 lowered
its priority.** Reuse gets a command to ~9 ms, so what remains for
fork-from-warm is the *first* command of a box and fan-out across many boxes
at once, not the steady state. It should be judged on that narrower prize
rather than on the 50× headline, which step 2 already collects. Worth noting
for the same reason: `msb` does have snapshots, but they are **disk-only and
offline** — a stopped sandbox, no memory image — so the warm-fork primitive
cannot be built on the macOS backend even in principle, and this step stays a
Linux-only second backend rather than something to retrofit.

The prize is a prewarmed agent snapshot — a parent guest with the agent CLI,
node, and toolchain already resident — so a microvm box's *first* command
skips a boot plus an agent cold start. forkd's pack format (sha256-pinned
snapshot bundles on a serverless registry) is a distribution story parallel
to our OCI images. The adapter seams are
narrow and already isolated: the `Runtime` enum, the pure and fully-tested
`build_run_argv`, and the two dispatch sites in `sandbox.rs`. The catch is
disqualifying until fixed: forkd has no default-deny egress — its own
README says so — and this tier advertises `egress_enforced_l3`, so under
the fail-closed rule a forkd backend refuses every profile with an egress
allowlist, which is most of them. The candidate closure is the netns forkd
already gives each child: a netns is a natural L3 enforcement point, and
the same egress-rule grammar the msb translation compiles from
(`container::parse_egress_rule`) can compile to nftables rules programmed
into it. That fix is upstreamable, the way forkd upstreamed its own
Firecracker `MAP_SHARED` patch.

**Not borrowed, deliberately.** The live-BRANCH stack — vendored
Firecracker, userfaultfd write-protect, a seccomp workaround — is the
highest-maintenance part of forkd and "start boxes fast" does not need it:
plain restore-from-warm-snapshot works on stock Firecracker. KSM tuning is
skipped on forkd's own negative result, and hugepages wait until the first
step's measurements say the tail matters. And the core CoW primitive does
not port to macOS at all — forkd's design doc rules macOS out because the
mechanism *is* the host kernel's copy-on-write over `mmap(MAP_PRIVATE)` —
so the macOS story stays msb plus reuse, and the platform split is stated,
not smoothed over.

### M14. `box service` at the microvm tier: built, 2026-08-14

Services are the first of the three things M13 step 2 unlocks, and the one the
other two wait on: a dev server has to exist before it can be shared or driven
by a browser. `spawn_background` refuses every tier but `workspace` and
`process` today, and the reason is not a missing feature — it is that **every
mechanism in the service machinery is a host-process mechanism**, and a guest
process is not a host process.

| What a service needs | How it works today | Why it does not survive the boundary |
|---|---|---|
| Identity | a host pid from `spawn_background` | a guest process has no host pid |
| Liveness | `pid_alive(rec.pid)` | asks the host about a guest pid |
| Stop | `killpg` after `setsid` | cannot signal into the guest |
| Logs | the child writes a host file directly | the guest cannot see a host path |
| Ports | a *host* port allocated and injected as `PORT` | the server binds inside the guest; nothing listens on the host |

**The one that is dangerous rather than merely absent** is identity, and it
decides the shape of the whole design. `ServiceRecord.pid` is a host pid, and a
guest pid put in the same field is not a different value — it is the *same
number in a different namespace*. `pid_alive` would answer about an unrelated
host process, and `service_stop`'s `killpg` would signal an unrelated host
**process group**. So the record has to say where the pid lives, every consumer
has to dispatch on that, and the host signal path has to **refuse** a guest
record rather than fall through to the pid it cannot interpret.

The design, then:

**A record says which world its pid belongs to.** `ServiceRecord` gains a
runtime discriminator carrying the guest name, `serde`-defaulted to the host
variant so records written before this still parse. That name is also the
cheapest liveness precondition there is: if it is not the box's *current* guest
name, the service is dead by construction, because a policy change rotated the
guest — no exec required to know it. Which is the second thing to state plainly:
**rotating a guest kills its services**, since the guest is the machine they
run on, and the records must be invalidated when it happens.

**Launch is an exec that detaches.** `msb exec <guest> -- sh -c 'cd /work &&
setsid nohup sh -c "<cmd>" >/.h5i/services/<name>.log 2>&1 & echo $!'`, taking
the printed guest pid. `setsid` makes it a session leader, so a later
`kill -TERM -<pid>` reaps the whole descendant tree — the same semantics the
kernel tiers get from `killpg`, which is why the same `setsid` appears in
`spawn_background`. Measured to survive: a server started this way answered
after unrelated execs in between (`docs/benchmarks/microvm-exec-tunnel.md`).

**Logs go through a mount, not a pipe.** `<env_dir>/services` mounted
read-write at `/.h5i/services`, the guest redirecting into it, the host reading
the same file. `service logs` and the stop-time capture ingest then work
unchanged. The content is box-written and therefore untrusted, which the
existing ingest already assumes — this changes who writes the bytes, not how
they are treated.

**Stop is the same escalation, one exec away.** `kill -TERM -<pgid>`, wait,
escalate to `-KILL`, then ingest the log as evidence exactly as today.

**Ports are guest ports, and `box ports` must say so.** This is the one place
the user-visible semantics genuinely differ, so it should not be papered over.
Two consequences, one of them a simplification:

- *Dynamic allocation stops being necessary.* Host ports are allocated per env
  because concurrent boxes share one host network and would collide. Each
  microvm box has its own network stack, so nothing can collide: the service
  binds the port its definition declares, and `PORT` is injected as that.
- *Nothing on the host listens.* `box ports` at this tier is reporting a port
  inside a machine, and reachability is `box share` — over the exec tunnel
  measured in `docs/benchmarks/microvm-exec-tunnel.md`, not over a published
  port.

**Publishing the port with `msb -p` was considered and rejected.** It is
create-time only, so the set of published ports becomes part of the guest's
identity: changing a service definition would rotate the guest and kill every
service running in it, and a box would carry an ingress hole for its whole life
against the possibility of a share that most boxes never ask for. The tunnel
opens nothing and works even on a `--no-net` box, which is the property worth
protecting.

**What does not change**, and deliberately: the service definition
(`[service.<name>]` in `.h5i/env.toml`, digest-pinned), the records directory,
the event log, the capture ingest at stop, and the whole CLI surface. Only the
execution backend is new — `spawn_background` grows a microvm arm and a return
type that can carry a guest name, rather than a second service subsystem.

**Built, and verified end to end**: a declared service starts in the box's warm
guest, `service status` reports it running, a dev server it starts answers
`HTTP 200` from inside the box, a `box run` in between leaves it untouched,
`service logs` reads the guest's log through the mount, and `service stop`
reaps it and captures the log as evidence.

Four things the build found that the design had not, three of them the same
mistake wearing different clothes — *assuming a host mechanism survives the
boundary*:

- **The guest's identity is only as stable as the policy that builds it.**
  `service_start` prepared a *different* policy than `run`: no capture spool,
  no inbox, no cache mounts, no user egress. Different mounts, different create
  argv, different guest — so starting a service created a second guest and
  reaped the one `box run` was using, and the next `box run` reaped it straight
  back, killing the service every time. Fixed by extracting `prepare_box_reach`
  so both paths grant the same reach from one definition. Then it happened
  *again*, two mounts smaller: the agent-config lockdown mounts are emitted
  only when those files exist, and `run` creates them through
  `ProtectedHookConfigGuard` while `service_start` did not. The lesson is
  sharper than "call the same functions": **anything that makes the create argv
  depend on transient state makes the guest unstable**, and there is now an
  `H5I_DEBUG_MICROVM_ARGV=1` hatch that prints the argv, because the diff
  between two of them is the only thing that shows which element moved.
- **`kill` is a shell builtin, not a binary.** `msb exec … -- kill -0 <pid>`
  returns 127 in a slim image, so every service read as dead and — worse — the
  stop path signalled nothing at all while reporting success. Both go through
  `sh -c` now.
- **`$!` after `setsid` is the wrong pid.** `setsid` forks whenever it must
  create a new session, so `$!` names a parent that exits immediately; the
  recorded pid was dead on arrival, and once the number was recycled it named
  an unrelated process for the stop path to signal. The service now writes its
  own `$$` to a pidfile and then `exec`s, so the recorded pid *is* the service
  and *is* the session leader `kill -TERM -<pid>` reaps as a group.

An adversarial pass over the result found three more, the first of which was
shipping a silent data-loss bug:

- **The idle timeout killed the services.** A guest is created with
  `--idle-timeout 30m`, and `msb` measures idleness in *commands* — it cannot
  see that a dev server inside is busy serving. So a service died 30 minutes
  after the operator's last h5i command, while still handling traffic, and the
  box looked fine. Measured rather than reasoned about: a guest with a 20 s
  bound stopped at ~25 s and took its service with it. A box that declares
  services now gets **no** idle bound (`ResolvedPolicy::hosts_services`, read
  from the pinned `[service.*]` set, which is known at create time — the bound
  cannot be changed later). Such a guest is reclaimed by `box rm` and by the
  sweep instead. The two are different guests by name, which is right: whether
  a box may be stopped is part of what its guest is.
- **Nothing had a deadline.** `service_alive`, `guest_state`, and guest
  create/start all blocked forever. Given an `msb exec` that has been seen to
  hang — rarely, still undiagnosed — `box service status` would hang with it,
  with no way out but Ctrl-C. All of them now run under `run_bounded`; a query
  that overruns reads as "not running", which is the safe direction.
- **The escape hatch broke services silently.** With `H5I_MICROVM_NO_REUSE=1`,
  a service would have been started in a warm guest while every `box run` got
  its own throwaway one — so the box could never reach its own service, and
  nothing would look wrong. Starting a service now refuses under that flag and
  says why.

A second review round, aimed at the fixes the first one produced, found nine
more. Two are worth stating because they are the same mistake at different
depths, and both were introduced *by* a fix:

- **"I could not read the answer" is not "there is nothing there."** Round one
  fixed `guest_state` so a failed or timed-out `msb list` no longer read as
  `Absent` — because `Absent` is answered with `create --replace`, which
  destroys a live guest and every service in it. Round two found the same bug
  one layer down: `parse_guest_state` still returned `Absent` when the output
  parsed as anything other than an array, so a banner line on stdout would have
  done it on *every command*. Worse, the unit test asserted that behaviour, so
  the bug had a test defending it. Only a well-formed list that does not name
  the guest is `Absent` now; everything else is `Unknown`.
- **A guest name is not a guest life.** A guest keeps its name across
  `stop`/`start` and restarts its pids from 1, so a stale record naming pid 42
  could match an unrelated process in the guest's next life — refusing a start
  that should succeed, and signalling a process group that was never ours. The
  record now carries the guest's kernel boot id, and a mismatch reads as dead.

The rest: the service launcher was the last runtime call without a deadline;
`wait_exec` joined its reader threads after killing the child, reintroducing
the hang its own deadline exists to prevent; crashed runs left brokered
credentials in a directory the long-lived guest can read, so it is swept before
each use; `live_service_ports` still called host `pid_alive` on a record that
may hold a guest pid, safe only by an accident of ordering; `env shell` was the
one entry point never routed through `prepare_box_reach`, leaving the
"one construction site" invariant true only by coincidence; and the benchmark
harness resolved workload binaries on the host and executed them in the guest,
which would abort the sweep for the very tier it exists to measure.

Then, and only then, share (M15): the tunnel is measured and the isolation
property is verified, but its remaining unknown is the in-guest forwarder — no
slim image carries `nc` or `socat`, and `/dev/tcp` is a bash builtin — so a
small static binary staged into a mounted directory is the first thing that
work has to decide.

### M16. The Lean model beside the Rust: steps 1 to 5 built 2026-08-15

> **Superseded in part, 2026-08-16.** M16 as recorded below shipped V1–V6.
> Building it showed `compile_sound` was near-trivial and the whole-config
> twin cost more than it caught, so the effort pivoted to an attack-driven
> filesystem authority machine (§VF). `Model`/`Input`/`Theorems`/`Phase`/
> `Refinement.lean` and `effective_drt` were retired; `Landlock`,
> `interferesCheck`, `Predict`, `Seatbelt`, and the probes were kept. The
> record below stands as what M16 built; §VF is the live direction.

**Steps 4 and 5 built, 2026-08-15.** Step 4 (`lean/H5iSpec/Refinement.lean`)
is L2: `compileLandlock` mirrors what `build_confined_command` builds from
the dump's grant lists, and the two directions are separate theorems —
`compile_sound` (the compiled ruleset never admits an access the resolved
policy denies, for every input and every world) and
`compile_complete_of_world_full` (it admits everything the policy grants,
exactly when no grant path was missing from the host; when one was, the run
is narrower on purpose and `skipped_missing` already says so). Step 5
(`lean/H5iSpec/Noninterference.lean`) is L3: a two-box shared-filesystem
semantics, the `noninterference` unwinding theorem (a box's writes are
invisible to a box it shares no writable-readable path with, over all trace
pairs), and the side condition made decidable — `interferesCheck` scans rule
pairs for overlapping scopes (prefix comparability), `interferesCheck_sound`
ties a clean check to the theorem, and the instances are concrete: two
agent-profile boxes fail the check through host-shared `/tmp` and really do
interfere; two workspace-only boxes pass it and provably cannot. The probes
close the loop model-to-kernel: `h5i-spec --predict` derives from a real
box's `policy.effective.json` what the kernel must allow and deny, and
`tests/effective_probes.rs` runs those accesses in a real process-tier box —
seven of seven agree on this host. The probe harness's own first failure was
educational and is now a comment: a test repo under `/tmp` vanishes behind
the box's private-tmp bind, which was exactly the bind-semantics gap the
prediction layer named — until the follow-ons below closed it.

**The four follow-ons, built 2026-08-15.**

- *The profile-corpus sweep.* `builtin_and_repo_profiles_agree` runs the
  builtin profile family plus this repo's own `.h5i/env.toml` profiles —
  five profiles, the agent and browser ones carrying dozens of tilde-laden
  real-host grants — through the same Rust-versus-Lean diff, with the world
  taken from read-only stats of the real host.
- *The HOME-controlled lane.* `interactive_and_tilde_cases_agree`
  re-executes itself as a child whose `$HOME` is a disposable directory, so
  it can generate what the other lanes must not touch: `interactive` shapes
  (config-lock binds, whose `$HOME` config files it materializes per case)
  and `~` grants. 200 cases; a deliberate mutation of the model's
  config-lock file order produced 48 mismatches before being reverted, so
  the lane has teeth.
- *Bind semantics in the prediction layer.* `H5iSpec/Predict.lean`:
  accesses beneath a bind target are judged on the path rebased into the
  bind's source subtree, a read-only remount denies writes before Landlock
  is consulted (`ro_bind_denies_write` — the theorem behind the config-lock
  pin), and away from every bind the prediction is exactly the compiled
  ruleset, so `compile_sound` bounds it. The probes now predict against the
  *run-shape* dump (binds are runtime state; a warmup run writes them) and
  probe the private-`/tmp` redirect itself. Extended the same day with
  **nesting and existence**: resolution recurses through the bind stack
  (later shallower binds shadow earlier deeper ones, chained
  source-under-target paths resolve through both — the four `nested_*`
  facts pin it), and each verdict is `{allow, real, check}` — the
  mechanisms' permission, the resolved host object, and what must exist
  there — with the harness supplying the existence facts by stat'ing
  `real`. That split let the probes cover what permission alone cannot: a
  host `/tmp` file predicted invisible behind the private-tmp bind (the
  confusion that broke the harness's first version, now a passing
  prediction), and a same-run scratch round-trip. Eleven probes, eleven
  agreements — after the suite caught one more true fact the honest way:
  the private-tmp scratch is wiped per run (`prepare_private_tmp`), env
  lifecycle above the mount layer, so existence facts are valid only
  within the invocation that measured them. Named as an exclusion in the
  layer's docs.
- *The `fs_overlap` receipt.* `effective::interferes` in Rust mirrors the
  Lean `interferesCheck` (differentially tested against it,
  `rust_and_lean_interferes_agree`), and every kernel-tier run and shell
  record now carries `fs_overlap`: the other materialized boxes whose
  effective grants overlap this box's, each with the shared path. Empty is
  the strong answer — by `interferesCheck_sound` plus `noninterference`,
  such boxes cannot influence each other through their granted filesystems
  — and the field's docs state the claim's exact scope (grants only; binds
  and the network are not covered). Honesty note the integration test pins:
  two boxes on one repo DO overlap, through the shared git plumbing
  `grant_box_git` writes into both, and the receipt says so rather than
  smoothing it.

**The second round, built 2026-08-15**: the three items the first round left
named.

- *The Seatbelt refinement.* `lean/H5iSpec/Seatbelt.lean` models the
  file-rule fragment `seatbelt::build_profile` emits, in its exact order,
  under SBPL's real semantics — `(deny default)`, last match wins. That is
  the opposite regime from Landlock (denies exist here), and the theorem
  says so: `fs_deny_wins` proves the generator's deny tail beats every
  grant, i.e. `fs.deny` is genuinely enforced on Seatbelt where on Linux it
  is a resolution lint. The generator is pure and compiles on Linux, so
  `tests/seatbelt_drt.rs` runs it here, parses the file rules out of the
  generated SBPL, and diffs them structurally against the model — 100
  policies green, and a mutation (dropping one path from the mirrored
  system-read list) fails all 100. Named gaps: the network/mach/sysctl
  sections, and `macos_developer_reads` is host-measured (empty on Linux;
  an on-mac sweep would exercise it).
- *Symlinks and procfs in the prediction layer.* Symlinks are chased
  fuel-bounded (`MAXSYMLINKS`), each hop re-entering in-box resolution
  through the binds, with the verdict taken at the resolved object — which
  is Landlock's actual behaviour and why `symlink_no_smuggle` holds: a link
  planted in the granted worktree confers nothing on its ungranted target.
  Under a pidns, `/proc` is the fresh private procfs with its read-only
  Landlock re-grant, so reads pass and writes fail whatever the grant lists
  say about the host's `/proc`, and existence is namespace-local
  (`box-local` checks carry the harness's a-priori knowledge). Probes now
  cover both: sixteen probes, sixteen agreements — among them the
  host-pid-invisible probe, which turns the tier's PID-view design claim
  into a measured fact.
- *Console rendering of `fs_overlap`.* `Signals` carries the newest
  run/shell receipt's overlap list — latest-record semantics, so a departed
  box's overlap clears rather than lingers, and a box-claimed lane can
  never update it — and the box pane renders it as a standing-property note
  beside `weak_isolation`, never folded into the verdict: overlap is
  policy, not enforcement firing. The export bundle's report gains the same
  line, so a reviewer applying a patch knows the box did not run alone.

**Adversarial self-review of the whole chapter, 2026-08-15.** The question
asked of every change: did it widen the sandbox, add an exploitable path, or
prove something vacuous. Findings, each verified in code rather than argued:

- **No widening found in the enforcement refactor.** `build_confined_command`
  still runs the re-probe and `resolve` gate; seccomp, rlimits, uid maps,
  no-new-privs, pidns and the child's mount sequence are untouched; the
  grant-set computation is list-for-list equivalent (order, `$WORK`
  stripping, exists-filter, readonly flip), evidenced by the unit suites,
  the kernel probes, and the DRT mutations.
- **Fixed: silent non-UTF-8 mangling.** The dump layer serializes paths as
  UTF-8; the refactor made enforcement consume those strings, so a
  non-UTF-8 workspace or bind path would round-trip mangled. Every failure
  mode was verified fail-closed — `path_beneath_rules` silently *skips* an
  unopenable path (checked in the crate's source), so a mangled worktree
  grant meant a box confined without its worktree, and a mangled bind meant
  a refused mount — but silent-and-confusing is not a boundary story.
  `build_confined_command` now refuses non-UTF-8 work and bind paths
  explicitly, before anything is computed.
- **Fixed: one more duplicated formula.** `want_netns` was still computed
  beside the dump's `namespaces.net` — the exact drift the apply-seam rule
  forbids. Enforcement now consumes the effective config's answer.
- **Fixed: two honesty overclaims.** `fs_overlap` said "on this host" while
  the scan is per-repository (`env::list` walks this repo's `.h5i/env`) —
  a box of a different repo on the same host is outside it; the docs now
  say so, along with the shape-staleness bound (a neighbor's dump reflects
  its latest invocation, so a readonly-shell dump under-reports until the
  next run). And `pidns_proc_read_allowed` reads as "any /proc read
  succeeds"; its docstring now states the layer-wide rule that `allow`
  means the sandbox mechanisms permit — DAC and procfs's own checks apply
  on top.
- **Checked and sound, for the record:** `effective_out` is serde-skipped,
  so a tampered `policy.resolved.toml` cannot redirect the dump write; the
  dump path's parent is not box-writable on any tier, so no symlink can be
  planted under the host-side write; box-claimed lanes can never set
  `fs_overlap` (unit-tested); receipt identity (`run_id`) is unchanged by
  the new fields; overlap detection covers bind backings because h5i pushes
  them onto `fs_write`; the console renders overlap strings through React's
  escaping; and no Lean code is linked into or executed by the product.

A Lean 4 model of the policy layer, developed beside the Rust and never linked
into it, connected by differential testing over a machine-readable dump of the
effective configuration. The design, the theorems, and the order live in
sections V1 to V6. M16 does not depend on M15 or on any browser work; its only
touch on the existing code is the dump (V2).

**Step 1 (the dump) is built and driven, 2026-08-15.** `compute_effective`
(`crates/h5i-sandbox/src/effective.rs`) is the single computation
`build_confined_command` now consumes for its Landlock path sets and bind
lists, so the dump and the enforcement cannot drift; every serde-skipped
`ResolvedPolicy` field is in the dump or excluded by name with a reason in the
module docs. `env create` writes the baseline and pins its digest in the env
manifest; each kernel-tier run and shell rewrites the file at the apply seam
and pins that run's digest in its capture record. Driven end to end on the
process tier by `effective_config_written_at_create_and_pinned_per_run`
(tests/env_integration.rs), on a host where the tier actually enforces.

Exit criteria for the first cut:

- `policy.effective.json` is written at box creation from the same values the
  mechanism appliers receive, and its digest is recorded in the capture
  manifest (V2). **Done, as above.**
- A `lean/` package builds in CI and its executable model agrees with the Rust
  resolver on the `examples/` corpus plus 10k generated profiles, with every
  mismatch either fixed or checked in as a named regression (V4). **Step 2
  built, 2026-08-15**: `lean/` (Lean core only, no mathlib; toolchain pinned
  v4.29.1) holds the schema mirror, the executable model, and five theorems
  checked on every `lake build` — among them `readonly_work_not_rw`, which
  came out *conditional*: `work_readonly` alone does not keep `$WORK` out of
  the rw grants when an `fs_write` entry spells the workspace path; the
  caller obligation `env::shell` discharges in prose is now a stated
  hypothesis. The DRT harness (`tests/effective_drt.rs`) generates policies
  with their filesystem world materialized in a tempdir, runs both sides,
  and diffs null-stripped JSON; green at 2000 cases locally, and a mutation
  test (the `/tmp` bind-order rule removed from the model) was caught within
  the first handful of cases before being restored. CI:
  `.github/workflows/lean-drt.yml`, a separate non-gating lane at 5000
  cases; the harness skips loudly for contributors without a Lean toolchain.
  The generator gaps this text originally named — `interactive` shapes, `~`
  grants, the profile-corpus sweep — closed the same day; see the follow-ons
  paragraph below.
- The Landlock fragment of the mechanism semantics is mechanized, and the
  conditional phase-transition theorem is machine-checked, including the
  counterexample the agent profile's shared `/tmp` provides (V3). **Step 3
  built, 2026-08-15**: `lean/H5iSpec/Landlock.lean` is L0 — rulesets as
  allowlists over path-beneath scopes, domains as intersecting stacks,
  `restrict_narrows` and `deny_persists`. `lean/H5iSpec/Phase.lean` is the
  machine: fds as capabilities with rights fixed at open, `restrict_self`
  as the phase transition, and the conditional theorem in both directions —
  `phase_confidentiality` proves install-phase denial confines through
  every later phase (invariant induction over reachability), and
  `run_deny_insufficient` exhibits the fd-smuggle trace: a reachable state
  whose domain denies the secret and which reads it anyway. The shared
  `/tmp` footgun is `shared_tmp_survives`, a `decide`-closed fact: a grant
  present in every layer survives the intersection, so narrowing bounds the
  boundary from above and never promises it moved. All checked on every
  `lake build`, so the Lean CI lane carries them.

### M17. The remote runner: R13.1 built, 2026-08-16

A box placed on a second Linux machine, driven from the local h5i over SSH.
Not a new isolation tier: a second axis, *placement*, beside the tier the box
already declares. The repo, the manifest, policy resolution, receipts, and
every credential stay on this machine; the other machine runs the same `h5i`
binary as a per-connection worker under an SSH forced command, executes the
container tier there, and hands back a quarantined git bundle the host turns
into the same reviewable patch as today. The design authority is sections R1
to R13, including the four sub-milestones (R13) and the decision points named
there. **R13.1 — the crate, the protocol, pairing and probing — is built and
verified against a real sshd**; R13.2 to R13.4 are not.

### M18. Runtime detection: a kernel-observed lane, 2026-08-19

An eBPF collector that watches a run from the kernel and puts what it saw in
the run's receipt. Every evidence lane h5i had until now sits either at the
boundary of the box (h5i as the parent process, the CONNECT proxy) or inside
it (the tee shim, the browser), so each is defeated by the box declining to
cooperate or by work happening below the outermost command. This lane is
neither: the kernel reports `execve`, `connect` and `openat` whether or not
anything in the box wanted them reported. It is observation only — nothing
here can deny anything, and denial stays with Landlock, seccomp, the netns
and the egress proxy — and it is `enabled = false` by default because it
needs `CAP_BPF`, which an ordinary install does not have. The design
authority is sections D1 to D14. **All five sub-milestones (D14) are built.**
What is not demonstrated: the live attach path has been exercised only where
the capability exists, so on a stock unprivileged install the honest answer
this ships with is the `unavailable` block naming the missing capability.

## 9. Limits we state up front

Being explicit about these is a feature, since the claim is a security claim.

- **The agent can send your source to the model API.** Containment stops the
  agent from touching the host. It does not stop it from putting private code in
  a prompt. That is a separate control (self hosted model, or no model egress at
  all) and we will not imply otherwise.
- **Shared kernel, unless you pick the microVM tier.** Podman and the kernel
  tiers share the host kernel. Good against a runaway agent and against careless
  dependency code. Not a claim against a targeted kernel exploit. The answer
  ships as `isolation=microvm`, a microsandbox (`msb`) backend that boots the
  same OCI images into a guest with its own kernel and filters egress by address
  in the VM's network stack. What it costs is honest and stated in MANUAL.md: it
  needs host virtualization (`/dev/kvm`, or Apple Silicon), it produces no
  per-request egress tally, and it does not yet route the authenticated-egress
  credential proxy. **Demonstrated end to end 2026-08-13** on Apple Silicon
  with `msb` 0.6.8: a box creates, runs, enforces its allowlist in the guest
  netstack, and exits 0. Two costs are now measured rather than assumed
  (`docs/benchmarks/microvm-boot.md`). It **was** a full boot per command,
  461 ms of it, because the guest was torn down after each one; **M13 step 2
  (built 2026-08-13) gives each box one guest instead, and the per-command cost
  is now 43 ms** — the cheapest of the four tiers on this host. What that costs
  is stated one bullet down: a box's commands share a guest, as they already
  shared everything at every other tier. The **8 GiB memory cap still costs
  ~154 ms** at roughly 20 ms per GiB, but it is now paid once per box rather
  than once per command, which is why recovering it via `msb`'s hotplug
  (`--max-memory` plus a live `msb modify`) was measured, understood, and then
  deliberately not built: it trades an async convergence window for a one-time
  saving. The Linux/KVM path remains unmeasured.
- **A box is the trust domain, not a command.** Successive commands in one box
  share state, and that is the point rather than a leak: the workspace
  persists, which is what a box *is* — a worktree plus a branch plus an agent
  session, whose commands are meant to be related (build, then test, then
  commit). So the boundary we claim is box↔host and box↔box. It is never
  command↔command, at any tier. An agent that leaves a file behind or a
  process running will meet them again on its next command, and a run that
  depends on the previous one having happened is a supported way to use a box,
  not a misuse of it.

  **The `microvm` tier joined them on 2026-08-13** (M13 step 2). It used to
  boot a guest per command and destroy it after, so guest-local state did not
  survive to the next command — an artifact of shelling to a one-shot `msb
  run` rather than a promise the tier made. Now a box gets one guest, so its
  commands share `/tmp`, the process table, and anything written outside the
  mounted workspace, exactly as they already did everywhere else.
  `H5I_MICROVM_NO_REUSE=1` restores a fresh guest per command for anyone who
  wants it.

  Three things hold. The durable work product lives in `/work`, a host mount,
  so it outlives the guest either way. **Reuse is scoped to one box under one
  configuration**: the guest's name is a hash of the argv that created it, so a
  changed profile, allowlist, image or mount set resolves to a different guest
  and the previous one is reaped — a box cannot be served a guest still
  enforcing a policy it no longer has, and this is structural rather than a
  check that could be forgotten. The corollary is worth stating: **guest-local
  state does not survive a policy change**, because that is a different guest
  by construction. And separate boxes still get separate guests, so nothing
  about box↔box isolation changes.
- **A microvm box that declares a service keeps its guest until you remove it.**
  A guest is normally stopped after 30 minutes idle. A box whose
  `.h5i/env.toml` declares any `[service.*]` gets no such bound, because `msb`
  measures idleness in commands and cannot see a dev server busy serving —
  the bound would kill the service it was meant to protect, and it is fixed
  when the guest is created, so it cannot be lifted later. The cost is real
  and stated rather than hidden: such a box holds its `mem_bytes` allocation
  from its first command until `box rm`, **even if the service is never
  started**. Declared is the signal because started is not knowable in time.
  `box rm` and the orphan sweep are what reclaim it.
- **The container tier's egress scoping is L7.** Its allowlist is a proxy, so
  it binds proxy respecting tooling only. The `supervised` tier enforces at
  L3/L4 with nftables and does not have that hole, which is why M4 starts
  there.
- **Chrome runs with its own sandbox off.** Our seccomp deny list blocks the
  namespace syscalls Chrome's sandbox needs, at every tier. h5i's box is the
  boundary; Chrome's is not available inside it. That is one layer fewer than a
  browser on the host has.
- **Linux and macOS, by different means.** Linux confines with Landlock, seccomp
  and namespaces; macOS confines with Seatbelt, and its `supervised` tier gets
  its egress allowlist from the same host side proxy the container tier uses,
  pinned by an SBPL rule that leaves the box no other outbound route. Two real
  gaps on macOS: no syscall filter (Darwin has no seccomp) and no memory cap
  (no cgroups, and `RLIMIT_AS` is not enforced against an mmap'd heap). Rootless
  Podman runs natively on Linux and WSL2, and through a `podman machine` VM on
  macOS.
- **Cost.** A Chrome sidecar is still real RAM and CPU, even headless. Headless
  boxes must stay first class, and the browser must be opt in per box.
- **The viewport is not a desktop.** CDP screencast shows the page. Native
  dialogs, browser chrome and anything outside the tab are invisible until the
  full-desktop tier lands.
- **A dependency on the critical path.** agent-browser is someone else's
  release cadence. Pinned, CLI-boundary, forkable, but not ours.
- **A share is a door, and `--tunnel` is a wider one.** `h5i box share` is the
  only path that lets traffic *into* a box, and it is opt-in, expiring and
  revocable per peer. The peer-to-peer transport is end-to-end encrypted and a
  relay, if one is needed, moves sealed packets. `--tunnel` is not end to end:
  Cloudflare terminates TLS and can read the traffic. That is a reasonable trade
  for a prototype and it is never ours to assume, so it is printed when the
  share starts and written into the box's receipt, not just documented here.
  Two smaller edges: authorization is checked when a connection is established
  and live connections are dropped within about a second of a revoke rather than
  instantly; and the page a joiner opens is agent-written code running in their
  browser, on a loopback origin that browsers exempt from private-network
  protections, which is the one place `--tunnel`, on a public origin, is
  actually the safer of the two.
- **Browser mediation is enforcement, not containment.** The socket mediator
  (M8) decides every verb the agent's CLI sends, which is the threat the
  control lock was written for: an agent that does not know a human took the
  wheel. It is not a boundary against an agent that goes looking, because the
  daemon runs *inside* the box and a box has no internal privilege boundary.
  Landlock grants are per-box, not per-process, so any socket the daemon can
  bind the agent can reach directly. Moving the daemon outside the box would
  close that and break the reason boxes exist: it could no longer reach the
  dev server on the box's own loopback.

## 10. Decisions taken

- **The name stays `h5i`.** The five dimensions story goes away, the name does
  not. Domain, install base, and binary name carry over untouched.
- **Distribution is the CLI plus a skill.** One Rust binary, no server, and
  `skills/h5i/` installed with `npx skills add h5i-dev/h5i`, mirroring
  `h5i-db`. The repo layout changes to make that the front door (6.).
- **The binary emits the skill.** `skills/h5i/` is embedded at build time and
  `h5i skill install` writes it out, which is how it reaches the inside of a
  box. Version drift disappears, and the in box copy can be rendered with that
  box's actual policy (6.1).
- **The browser layer is agent-browser** (Apache-2.0, native Rust), for both
  automation and the human viewport stream. We do not reimplement Neko's core,
  and the whole X/GStreamer/PulseAudio stack drops out of the design. h5i keeps
  the boundary, the control lock, the policy and the receipts (7.). A
  full-desktop tier, with Neko as its reference, is deferred until something
  actually needs more than a page viewport.
- **Warm caches are in scope.** Read only per project cache volumes, written
  only by a dedicated refresh box with no agent in it (5.8).
- **The receipt may be generated in the box**, provided the agent cannot
  rewrite it. That is bought today by sealing (the receipt store sits outside
  every write grant the box has) plus two host observed fields for cross
  checking. The inherited-fd writer stays on the table as the stronger form
  (5.7).
- **`AF_UNIX` is a profile grant, not a tier property.** The supervised
  tier's `socket()` gate denies the family by default, because `SCM_RIGHTS`
  passes file descriptors. A profile opts in (`[profile.X.net] unix = true`),
  and it is pinned in the digest. Granting it tier-wide to make one daemon work
  would have widened every box to buy one; the `browser` profile asks, and
  nothing else does.
- **The programmable surface is the CLI's JSON contract, not a daemon.** An SDK
  is a thin subprocess wrapper around the binary (the `remote-agent-browser`
  shape, about 1,200 lines), published only after the first buyer workflow is
  demonstrated. `create`/`run`/`export` gained `--json` on 2026-08-05, which
  closes the loop the contract needs (6.2).
- **The terminal viewer is an in-process client of the box's stream, and
  terminal-browser is a reference, not a base** (5.10). It enters the box's
  namespaces the way the forward does and takes the socket over `SCM_RIGHTS`,
  so it binds no port and needs no token. The forward's token exists because
  the forward has to listen, and this does not. The host gains one module and
  three small dependencies: no Electron, no host Chromium, no input helper.
  The box side is unchanged, so nothing in the boundary or the policy moved.
  Both viewers write one receipt lane through one function, because two
  near-identical formats is how an export ends up describing the same session
  two different ways.
- **No MCP.** `mcp.rs` and the `h5i_env_*` tools go with the rest. The premise
  of MCP here was a host side agent reaching into a box, which is the shape this
  product exists to eliminate. The agent is inside the box, and inside the box
  the interface is the CLI plus the skill. There is no second interface to keep
  in sync, and no tool schema to drift from the flags.

## 11. Still open

1. **Enforcing the control lock on the agent's side.** The lock is designed and
   the viewer honours it: input from a human reaches the page only while they
   hold it. What is *not* wired is the other direction: `control::check` exists
   and returns `HeldByHuman` / `NeedsResnapshot` with the message an agent
   should see, and nothing calls it. So an agent running `agent-browser click`
   during a human takeover is not refused today; it is only told, if it asks.
   The gap is the interception point, not the policy: there is no h5i process
   between the agent and `agent-browser` at the kernel tiers, so this needs a
   decision about where the check lives (a PATH shim, a skill-level convention,
   or accepting that it is advisory) rather than more code in `control.rs`.

   **Answered, 2026-08-07: the mediated socket (7.2, M8), built.** The
   daemon's NDJSON control socket was a fourth option the original list
   missed, and the only one the agent cannot route around: a PATH shim can be
   bypassed by calling the binary by path, a convention enforces nothing, and
   the socket is the one door every verb walks through.
   `crates/h5i-core/src/browser_proxy.rs` decides every line against
   `control::check` and a per-profile action policy, answers refusals in the
   daemon's own shape, and records each action into a host-observed
   `browser-proxy` receipt lane. **Verified against the real `agent-browser`
   CLI** (`tests/browser_mediation.rs`): a read passes through and returns the
   real page, a denied `eval` is refused and never evaluated, and a click
   during a human takeover is refused while reads keep working.

   Three findings, none available by reading:

   - **`__agent_browser_internal_shutdown` is an escape hatch, not an
     action.** The CLI sends it when it decides the running daemon does not
     match the options it wants, then starts its own. Forwarded naively it
     kills the daemon we mediate and the replacement is the agent's, on a
     socket we do not own: mediation gone, with no error anywhere. It is
     refused unconditionally.
   - **`launch` is not a page change.** The CLI prefixes every command with
     it, so classifying it as mutating refuses it during a takeover and takes
     every read-only verb down with it, the opposite of 5.4's rule that
     watching never collides.
   - **The daemon's config fingerprint covers its options, not its path**, so
     the real daemon can run on a path the box cannot reach with the mediator
     in front, provided h5i launches it with the environment the box's CLI
     will compute and mirrors `.version`/`.config` into the box-visible dir.

   **The lifecycle landed too, and is enforced by default.** The daemon is
   started by the *shim* rather than by h5i directly, which is what makes the
   split possible at all: the shim already runs inside the box and invokes the
   real binary twice, so it starts the daemon on a private path
   (`/tmp/agent-browser-daemon`), mirrors the `.version`/`.config`/`.stream`
   files the CLI checks, and then execs the CLI against the mediated path.
   h5i's listener binds *before* the box runs. Waiting for a daemon first
   would mean the box's own first call finds the mediated path empty and
   starts an unmediated daemon on it, then connects upstream lazily.

   Verified in a real supervised box: `agent-browser open` works through the
   chain, the real daemon's socket lives in the private directory while the
   visible one holds only mirrored files, a read passes through, and
   `agent-browser eval` comes back
   `✗ \`evaluate\` is denied for this box by its profile's browser action
   policy (fail-closed)` with `browser mediation (2 action(s), 1 refused)` on
   the receipt log.

   Two more findings from that run. **Not every agent-browser word is a
   command**. `url` and `status` are not, and using one to start the daemon
   fails silently and leaves no daemon and no clue; `open about:blank` is the
   cheap start that works. And **a box whose repo lives under `/tmp` cannot
   see its own shim**: the per-env `/tmp` scratch shadows the host path the
   shim sits on, `agent-browser` falls through to the system binary, and
   mediation is bypassed with nothing to indicate it. That is the same
   shadowing the M4 notes record, arriving somewhere new.

   Related and now much smaller: **snapshot handle staleness across a takeover**
   is modelled: `needs_resnapshot` is set on the take, survives a session that
   never hands back, and clears only on an actual snapshot. It rests on the same
   unenforced check.
2. **First buyer workflow.** The positioning is broad enough to become a
   platform pitch, which sells to nobody. The launch message should be one
   workflow: run untrusted or AI generated code, see it in a real browser, keep
   it off your machine.

   **Candidate, 2026-08-07: name the runtime.** "h5i Browser: the browser
   that runs where your coding agent runs." Kitesurf and Lightpanda are
   browsers for agents browsing the web; this is a browser runtime for coding
   agents building the web, and the demo is the full loop of section 8, which
   already exists. It is packaging over M4-M7 plus M8, not new engineering,
   with two constraints held from section 10: no separate binary (the surface
   stays `h5i browser`, one-binary decision) and the wording is "an
   agent-native browser runtime powered by Chromium", never an engine claim,
   until M10 makes one true. The demo surface is M11's developer view.
3. **Publishing `@h5i/sdk`.** Blocked on item 2 by decision, not by code: the
   JSON contract it wraps is complete (6.2). First release scope is
   `create`/`exec`/`browser`/`diff`/`export`/`close`, TypeScript only, binary
   fetched on postinstall. No `agent.run()` until the resident session shape is
   settled, and Python only when someone asks for it.
4. **How engine selection grows from explicit to routed (7.1 step 2).**
   **Deprioritised 2026-08-08 (§12.3): a box picks one engine at creation and
   keeps it.** The first step is **built (M9, 2026-08-07)**: `[profile.X] engine = "..."` and
   `--engine`, pinned in the digest, refusing by name when the engine's
   tooling is absent, with no `auto` and no fallback. One correction the build
   forced: 7.1 claimed the knob's shape is "any CDP endpoint … the slot M10's
   binary later fills with no new plumbing", and that is **wrong**:
   `h5i-browser-light` does not speak CDP, so agent-browser cannot drive it
   and h5i runs it directly (`BrowserEngine::driven_by_agent_browser`). The
   remaining sequence is unchanged: a `--browser auto` heuristic as a later
   explicit opt-in, and per-origin routing last. What is not designed
   is the routing step itself: agent-browser is one engine per daemon
   session, so "loopback gets Chromium, the web gets the light engine" needs
   either two sessions with h5i choosing at navigate time, or the mediation
   layer of 7.2 doing the choosing per action. The second is cleaner and is
   one more reason M8 goes first. Wherever it lands, routing inherits M9's
   rule: an engine switch is a policy change, so it belongs in the digest and
   the receipt, never in a silent fallback.
5. **Build versus adopt for the lightweight visual engine (M10).** Kitesurf's announced
   open-sourcing decides how much of the M10 crate is ours to write. Until
   that drop, the only commitment is the shape: fetch through our proxy,
   receipts as the network log, script off by default for untrusted origins.
6. **The microvm plan's gating questions (M13): one answered, one still
   open, one new.**

   **Answered 2026-08-13: `msb` holds a sandbox open and execs into it.**
   `msb create --name X` boots detached, `msb exec X -- cmd` attaches over
   the guest's agent relay socket without booting, and `exec` auto-starts a
   stopped sandbox so h5i need not track guest state. Measured at 8.4 ms warm
   against 233.9 ms cold. The reuse step needs no upstream ask; what it needs
   is an idle timeout, because `--idle-timeout` has no default and a detached
   guest otherwise outlives its box.

   **Still open: default-deny egress inside forkd's per-child netns.**
   Whether the existing egress-rule grammar can compile to nftables rules
   programmed into it. Without that, a forkd backend fails closed against
   every profile with an egress allowlist and is not worth carrying. This one
   needs a Linux host with KVM, which the 2026-08-13 run did not provide —
   everything measured so far is macOS, and the Linux path is untested.

   **New, and not a microvm question at all: why `process` and `supervised`
   add ~1.5 s to Python startup on macOS.** Found while benchmarking
   something else. It is not a fixed cost, so no amount of reuse hides it,
   and it lands on the tiers macOS users get by default. The suspects are the
   `/usr/bin/python3` Command Line Tools shim and SBPL evaluation over a
   startup that opens hundreds of files; neither is established. Whether it
   reproduces with a non-system interpreter is the cheapest next probe.

## 12. The browser: a local engine that runs script, and the order to build it

> **The work is in [the browser engine sections](#the-browser-engine)**, B1 to
> B14, as of 2026-08-09. This section stays the authority on *scope and why*;
> those are the authority on *order*, and carry the bindings backlog, the
> security items script introduced, and the assessment of Thalora as a source to
> read rather than adopt.

**Rewritten 2026-08-08.** The previous version of this section ordered script
*last* and argued it should wait for the microVM tier. That order has been
reconsidered, and the reasoning that changed it is below. This is a deliberate
change of direction, not an accretion: it widens §3's scope cut, which said keep
`env` and `sandbox` and cut everything else. Read §12.5 before treating any of
it as approved.

### 12.1 What the previous sequence produced

Its first four items are built. Recorded here because the sequence worked, and
because what it found is the argument for the next one.

* **A resident session** (old item 1). `serve` holds a page several viewers and
  a control channel share. Built 2026-08-08, along with the finding that made it
  the only possible shape: `Page` is not `Send`, so one thread owns the page and
  everything else reaches it by channel.
* **A real input surface and an agent interface** (old item 2). `session
  status|snapshot|navigate|scroll|type|submit|click`, plus a cookie jar, so a
  login works end to end. The skill is engine-aware.
* **Untrusted-content marking** (old item 4), pulled forward because it was the
  only item whose absence was a live hole. The snapshot is fenced, and the fence
  rests on a tested one-line invariant rather than on a secret.

Two items remain from that list and survive into this one unchanged in
substance: **action-to-request correlation** (old item 3) and **LOGIN mode with
takeover as a recorded event** (old item 5).

### 12.2 The decision: a local, stateful browser that runs script

Three things moved.

**Two engines does not avoid Chromium.** 7.1 answered "what about script" with
routing: light engine for reading, Chromium for the rest. But
`browser_read_grants()` chains *every* engine's candidates, so an `h5i-light`
box grants Chrome's and agent-browser's paths anyway, and the environment still
installs Chromium, still updates it, still carries its surface. Routing saves
runtime RSS and nothing else. That is the fact that undercuts the two-engine
answer, and it is checkable in this repository rather than a matter of opinion.

**The local position is unoccupied.** Kitesurf is cloud-first: it runs on
Cloudflare Workers and depends on Dynamic Workers, Worker-to-Worker RPC and
Static Assets. The open-sourcing language is "customers can deploy it to their
own Cloudflare account", which is not "runs as a local binary inside a
disposable sandbox next to the repository". Lightpanda runs script and does not
render. Nobody is building a browser that runs script, renders on demand, and
lives inside the coding agent's own sandbox.

**Being one process is an advantage we can take and they cannot.** Kitesurf
serialises a scene from its page realm to a separate renderer because that split
*is* part of its security model. Ours is not: the box is the boundary. So DOM,
style tree, layout tree, display list, tile cache and semantic tree can live in
one process and update incrementally, which is exactly where their own numbers
say the cost is. Cloudflare reports Kitesurf using 3-7x less CPU and memory than
Chromium while being 1.7-1.8x *slower* in wall time, dominated by rasterisation
and image encoding. Measured here independently, release build, 1280x720: encode
5.9ms, alpha flatten 0.84ms, rasterise 1.2ms, whole page load 2.97ms. Two
implementations of the same stack landing on the same bottleneck is evidence
that it is structural.

So the claim is not speed. By Kitesurf's own wall-time numbers this class of
engine is slower than Chromium, and a benchmark table is a claim anyone can beat
by shipping less browser. The claim is the closed loop:

> The agent clicked Add. That click caused exactly one request, `POST
> /api/items`. Here is the receipt, written before the request went out. Here is
> the DOM delta it produced. Here is the frame the human saw.

Chromium cannot produce that line, because its Fetch lane is best-effort and its
records are host-observed at best. Kitesurf cannot easily produce it either,
because the causal link spans its renderer split. It falls out here almost for
free, for one reason: **script makes the receipts story stronger, not weaker.**
Once `fetch` and XHR route through the existing broker, script-initiated traffic
becomes policy-checked and receipted like everything else, which is the lane
where every other engine's evidence is thinnest.

**Engine choice: Boa, chosen rather than benchmarked into.** An earlier draft of
this section made a three-engine shootout the first milestone. That was wrong,
and it is worth saying why rather than quietly dropping it: the urgent thing is a
real browser that works inside the sandbox, and the shootout was a proxy for a
question the vertical slice answers directly. Build the slice, run a real
application, and you have the number the benchmark was estimating.

Boa is the right first engine for a reason stronger than "easiest". It is pure
Rust, so it adds no C toolchain to a build this project has repeatedly paid to
keep hermetic: `system-fonts` was turned off to avoid libfontconfig, and the
cross-check matrix compiles this workspace for windows-msvc, darwin and musl.
That last one is not theoretical. `ring`'s C build already blocks cross-checking
to windows-msvc from a Linux host, and QuickJS or V8 would add another
dependency of exactly that kind to the one crate that is meant to be portable.
Boa costs nothing there.

What Boa costs instead is speed, and the cost should be stated: it is an
interpreter with no JIT, QuickJS generally benchmarks ahead of it, and V8 is an
order of magnitude beyond both. Kitesurf uses V8 for page script and Boa mainly
for `eval`, so Boa carries no precedent for web-app compatibility. A React
production bundle does one large burst of compute at hydration, and that is
where an interpreter is worst.

So the engine sits behind a seam, and the trigger for revisiting it is a
measurement from the real thing rather than a schedule item: **if hydration of
the target application is slow enough to make the Chromium comparison
embarrassing, that is the signal to swap.** Not before. The swap is affordable
precisely because of the next paragraph.

**The asset is the bindings layer, not the engine.** The Rust DOM is the single
source of truth and JS objects are thin wrappers over stable `NodeId`s. A second
tree inside the JS engine would let the snapshot, the paint, the events and the
script state drift apart, and every bug after that is unfixable. Done this way,
swapping Boa for QuickJS or V8 later costs the embedding glue and keeps the
bindings.

### 12.3 What this is not

Named so they can be refused in review rather than argued about each time. None
of these is in scope for the first version, and the README should say **limited
JavaScript preview** rather than "JavaScript support":

CDP and Playwright compatibility. The plugin API. Iframes. Service workers.
WebSocket. Canvas and WebGL. Media. Chrome extensions. Pixel-perfect rendering.
Vite's dev server, HMR and `import.meta.hot`. Cross-origin authenticated
browsing.

CDP is worth its own decision later rather than inheritance now: the argument
for it is not ecosystem access but that agent-browser could then drive this
engine, collapsing two agent interfaces into one. The argument against is that
it is a second full surface next to a verb set that already exists and works.

**And routing, which is the deliberate one.** §11 item 4 sketches a sequence
that ends in per-origin routing: loopback to Chromium, the open web to the light
engine. That is now **low priority, and not a goal of this direction at all.**
A box picks one engine when it is created, and lives with it.

The reason is that two browsers in one box is both heavy and strange. Heavy is
obvious: Chromium's install, its updates and its surface, carried for a box that
may never launch it. Strange is the part worth writing down, because the code
already shows it. `sandbox_policy::browser_read_grants()` grants **every**
engine's binaries rather than the pinned one, on the argument that the engine is
enforced by what h5i launches. But an agent inside the box can invoke the other
binary itself, which `browser_light_env` already concedes when it keeps
`AGENT_BROWSER_ALLOWED_DOMAINS` set for an engine that never reads it: "if it
does, this is the only thing standing between it and any host on the internet".
So today the pin is a **launch choice, not a boundary**, and a box pinned to
`h5i-light` still carries a Chromium an agent could start.

Committing to one engine per box is what makes that honest. It narrows what a
browser box installs, lets the grant list follow the pin, and turns the engine
from something h5i happens to launch into something the box cannot step outside
of. Whether the grants should actually narrow is a real question with a real
counter-argument in that function's comment, about keeping the digest
independent of host discovery. It is not settled here. It is only unblocked
here, because it cannot even be asked while routing is a goal.

If routing returns, it returns as an explicit opt-in after the engine can carry
a real application on its own. Building it earlier means paying the two-browser
cost permanently to avoid finishing the one engine that would remove it.

### 12.4 The order

Items 1 to 3 are together what "JavaScript support" means to someone using this.
They are numbered apart because each carries its own design decision, not
because any of them is optional: 1 makes script run, 2 says when its result is
safe to read, 3 is script's network.

1. **Embed Boa, and build the bindings layer, against a production React
   build.** Embedding is the small half: a dependency, a `Context`, and
   evaluating `<script>` text. The bindings are the work, and the reason this
   milestone is named after them. Not a hand-written
   `addEventListener` demo, which proves nothing about the shape of the problem,
   and not the Vite dev server, which drags in WebSocket, HMR and native ESM in
   one step. The surface is roughly: `window`, `document`, `Node`/`Element`/
   `Text`, creation and insertion and removal, attributes, `classList`,
   `querySelector`, `textContent`, events with capture and bubble, `click`/
   `input`/`submit`, promises and the microtask queue, timers,
   `requestAnimationFrame`, `fetch`/`Response`/`Headers`/`URL`, `location`,
   `history`, `console`, `performance`, and invalidation of style, layout and
   paint on mutation. Missing APIs are **logged as unsupported and surfaced in
   the snapshot**, never silently stubbed: an agent needs to know the outline is
   incomplete at the moment it reads it, which is the same rule the fence
   follows.

2. **Quiescence, reported rather than guessed.** "Run JS until settled" is a
   subsystem, not a phrase. No pending microtasks, no timer due inside a stated
   window, no in-flight brokered request, and a hard timeout. Playwright
   deprecated `networkidle` for good reasons. The snapshot states which it was:
   settled after 340ms, or still busy at cutoff. A snapshot that quietly
   returned early is a wrong answer that looks like a right one.

3. **`fetch` through the broker, and the correlation that falls out of it.**
   Old item 3, and in this design it stops being extra work: the engine is the
   one component that knows a click caused a request, and `browser_events`
   already carries `caused_by` for exactly this and currently only wires
   request to response. This is the differentiator, and it is a field we will
   already be holding.

4. **LOGIN mode, and takeover as a recorded policy event.** Old item 5, now
   overdue rather than pending: the cookie jar it was supposed to arrive with
   shipped on 2026-08-08 without it. Until it lands, a human taking over to type
   a password does so on a page the agent can still snapshot.

5. **The comparison, run and published with its caveats.** Same app, same host,
   against Chromium: startup, peak RSS, navigate-to-ready, click-to-DOM-update,
   click-to-visible-frame, idle CPU, binary size. Publish the losses too. If
   click-to-visible-frame is worse, that is a finding about raster and encode
   and it belongs next to the memory win, not behind it. This is also where the
   engine question gets answered: click-to-DOM-update on a real application is
   the number that says whether Boa stays, so the shootout that used to be
   milestone one happens here, once, against something that matters.

### 12.4a Built, 2026-08-09: items 1 to 3

The vertical slice runs. An agent clicks, the page's script executes, its
`fetch` goes through the broker and is receipted, the DOM changes, and the
change is in the outline the agent reads:

```
$ h5i-browser-light session click @e1
{"ok":true,"ref":"@e1","requests":["http://localhost:8231/api/item"],
 "settled":"settled after 0ms"}
```

with all three legs in the request log: the navigation, `/app.js` fetched
*before* it ran, and the `/api/item` the click caused.

**What the shape turned out to be.** The Rust DOM is the single source of truth
and JS objects are wrappers over `NodeId`s, as 12.2 required. What 12.2 did not
anticipate is that the object model itself belongs in a **JavaScript prelude**
rather than in Rust: event listeners, timer callbacks and promise resolvers are
GC-managed values, and holding them on the Rust side means tracing them through
Boa's collector. Putting them where Boa already owns their lifetime left a Rust
surface of about twenty primitives taking ids and strings, and made
capture/bubble propagation ordinary code instead of a lifetime problem.

**Quiescence is a virtual clock.** Promise jobs and timers drain against a clock
the engine advances, not the wall: a page's `setTimeout(1000)` costs an agent
nothing, and two runs of the same page settle identically. That was chosen for
determinism and turned out to matter more than the speed. It is the same
argument as §12.4's "reported rather than guessed", applied to time itself.

**Two things bit, and neither was performance.**

1. **Boa does not compose with our tree.** Boa 0.20+ needs `icu_normalizer
   ~2.0`; `parley`, which Blitz pulls for text, needs `^2.1.1`. Disjoint and
   semver-compatible, so Cargo must unify and cannot. Boa 0.19 uses the 1.x
   line, which is semver-*incompatible* and therefore allowed to coexist, so the
   pin is 0.19 and the build carries two ICU stacks. Upstream has already moved
   `main` to `~2.2.0`, so this unwinds on their next release. Worth noting that
   the first thing to bite was dependency composition rather than speed, which
   is the argument for building before benchmarking, made by accident.
2. **A test hung and looked like a slow build.** Its fake server accepted two
   connections while the test made one, so `join` waited forever. It read as
   compile time, and was diagnosed as compile time, until the user checked. The
   same pattern had already shipped in the cookie tests, where it worked only
   because that test happened to make exactly two requests.

**Boa 0.19's conformance was checked rather than assumed**, since it is two
releases behind: eighteen syntax cases a bundler actually emits (optional
chaining, nullish coalescing, class and private fields, generators,
`Symbol.iterator`, `Proxy`/`Reflect`, spread, destructuring) all run, and
microtasks drain.

**Not cleared: a production React build.** §12.4 item 1 sets that as the bar and
what runs today is a hand-written application. The gaps that will stop React
first, in order: no ES modules or `import`; `MutationObserver`,
`IntersectionObserver` and `ResizeObserver` report themselves missing rather
than working; `getBoundingClientRect` returns zeros and says so. Each is
recorded in the snapshot when a page asks for it, so the next attempt starts
from a list rather than a guess.

### 12.5 The gate that is not a milestone

`capabilities.javascript` flipping to `true` is a change to the box's threat
model, and it must be an explicit decision rather than the consequence of a
prototype working.

**What it spends.** Today the strongest security property this engine has is
that no JavaScript engine is linked into it at all, so page-borne prompt
injection has no delivery channel *by construction* rather than by filtering.
The moment script runs, that sentence must stop being used, and the
untrusted-content fence goes from a second line of defence to the only one.

**Site isolation is the one thing the box does not replace.** Chromium's process
model exists to contain a compromised renderer: filesystem, network privilege,
crash isolation, and cross-origin theft. The box covers the first three at a
stronger boundary than a renderer sandbox. It does not cover the fourth, because
it protects the host from the box and says nothing about origin A and origin B
sharing one address space. That did not matter while the engine held nothing
worth stealing. It matters now: the cookie jar shipped on 2026-08-08, and script
is what puts attacker-controlled input in the same process as it. Blitz and
Stylo being Rust is the current mitigation, and adding a JS engine written in C
or C++ is precisely what erodes it. Cheap options exist and one must be chosen
before this ships: one origin per session, clearing the jar across origins, or
keeping the jar out of the process that runs script.

**The gate is honoured so far.** `capabilities.javascript` reports the *running*
configuration, script is opt-in behind `--script`, and with it off a page's
`<script>` elements are inert exactly as they were. Nothing has flipped by
default, and nothing should until the rest of this subsection is answered.

**Limits belong to the box, not to the engine's good behaviour.** Reliable
in-engine interruption of a runaway script is hard; a wall-clock deadline and a
memory ceiling enforced from outside are not. `builtin_browser` currently sets
`mem_bytes` to 12GB and `max_procs` to 1024, both sized for a Chrome that spawns
renderer processes. An `h5i-light` box running script should die at a few
hundred megabytes.

**And the containment underneath is still the weaker story.** The mediator is
enforcement against a compliant agent, not containment against an evasive one
(7.2, §9). Running untrusted script inside that is the step that makes the
system less safe than it is today. The previous version of this section
concluded that this waits on the microVM tier. That conclusion has not been
refuted by anything above; it has been *outvoted* by the judgement that an agent
browser which cannot run script is not a product. Both halves of that sentence
should stay written down.

---

# The browser engine

Status: 2026-08-09. The forward plan for `crates/h5i-browser-light`. Section 12
above records the *decision* to build a local engine that runs script, and why.
These sections are the work. Where the two disagree, §12 is the authority on
scope and these are the authority on order.

> **A pure-Rust browser that lives inside the agent's own sandbox, renders on
> demand, and can prove what it did.**

Three claims, and only the third is unique. Pure Rust is a real property (no C
toolchain, a smaller memory-bug surface), but it is a means. Rendering on demand
is what separates this from Lightpanda. **Proving what it did** is the one
nobody else can copy back, because it depends on the engine *being* the HTTP
client rather than being watched by one.

The claim is deliberately not speed. By Kitesurf's own numbers this class of
engine is slower than Chromium in wall time, and a benchmark table is something
anyone can beat by shipping less browser.

## B1. Where it is, 2026-08-09

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

Not cleared: **a production React build**, which §12.4 sets as the
bar. What runs is a hand-written application of the right shape.

---

## B2. Architecture, and the constraints that chose it

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

## B3. Security: what script bought and what it cost

### B3.1 The loopback hole: **closed 2026-08-09**

`Policy::check` took only a URL, and loopback is allowed unconditionally by
default because the box's dev server is the point. Before script, an untrusted
page could *cause* a loopback request but not read the response. With `--script`
it could `fetch` the dev server, read the body, and POST it anywhere in
`net.egress`: a read primitive against the code the agent is working on, past
the egress proxy that never sees loopback.

Closed by `Policy::check_from(url, document)`: loopback is reachable **from a
loopback document**. A page served by the dev server may talk to it; a page from
the open web may not. Tested both directions
(`a_web_page_cannot_read_the_dev_server_and_never_reaches_the_wire`,
`the_dev_servers_own_page_still_reaches_it`).

Worth keeping in front of the reader: this was a **logic** bug, and Rust
prevents none of them. "Fewer memory bugs" is honest; "safer browser" is earned
by the origin model, not the language.

### B3.2 Site isolation is the one thing the box does not replace

Chromium's process model exists to contain a compromised renderer: filesystem,
network privilege, crash isolation, and cross-origin theft. The box covers the
first three at a stronger boundary than a renderer sandbox. It does not cover
the fourth. It protects the host from the box and says nothing about two
origins sharing one address space.

That did not matter while the engine held nothing worth stealing. The cookie jar
shipped on 2026-08-08 and script on 2026-08-09, so it mattered.

**Answered 2026-08-09, by the second of the three options**: the jar is cleared
on cross-origin navigation (`Jar::retain_origin`), so one session holds one
origin's cookies and a page can never be in the same address space as another
origin's session. The cost is stated where a user meets it: leaving an origin
drops its login, and the snapshot says so rather than letting the agent discover
it by being logged out. `document.cookie` additionally withholds `HttpOnly`,
which is the line between what the wire carries and what script may read.

### B3.3 The gate, still honoured

`capabilities.javascript` reports the *running* configuration; script is opt-in;
with it off, `<script>` elements are inert exactly as before. Nothing has
flipped by default and nothing should until 3.1 and 3.2 are answered. See
§12.5.

---

## B4. Three things that were wrong rather than missing: **all fixed**

"Missing" is honest and reports itself. These were worse: they corrupted a page
while looking like they worked, which is the failure mode the fence and the
unsupported-API log exist to prevent, and they polluted every measurement taken
before they were fixed. Kept here because the *class* is the lesson, not the
three bugs.

1. ~~`innerHTML` getter returned `textContent`~~: all markup stripped, so
   `el.innerHTML = el.innerHTML` destroyed the subtree. Now a real serialisation.
   The root cause was upstream of the getter: `DocumentConfig` never set an
   `html_parser_provider`, so `set_inner_html` silently did nothing.
2. ~~`createDocumentFragment()` returned a `<div>`~~: appending a fragment
   injected a real element that broke `.parent > .child` and layout. Now a real
   fragment, and one that can be searched (§B8.6).
3. ~~`Element.style` did not exist~~: `el.style.display = 'none'` threw and
   killed the script at that line. Now a real `StyleDeclaration`.

The same class keeps recurring and is worth naming: **a plausible answer is
worse than no answer.** `matchMedia` returning false to everything, `scrollTop`
computed from the bounding rect, `structuredClone` via a JSON round trip, and
`clientHeight` for `documentElement` were all this bug wearing different clothes.

---

## B5. The bindings backlog

Ordered by what blocks real applications first. Cross-referenced against
Thalora's surface (§B7) where that project has already mapped the ground, and
marked **cheap** where Blitz or Stylo already holds the answer and we are merely
refusing to give it.

### Tier A: blocks nearly everything modern

| | why | note |
| --- | --- | --- |
| ~~ES modules and `import()`~~ | every production bundle ships `<script type="module">` | **built**, through the broker; bare specifiers are refused rather than rewritten to a CDN |
| ~~`Element.style` (CSSOM)~~ | `el.style.display = 'none'` is ubiquitous | **built** |
| ~~`getBoundingClientRect`~~ | every popover, dropdown, drag and virtual list | **built**: Blitz computes `final_layout` already |
| ~~`getComputedStyle`~~ | feature detection and measurement | **built**, via Stylo's `to_css_string`, not `Debug` |
| ~~`MutationObserver`~~ | frameworks depend on it | **built**. The semantic delta went its own way in the end: diffing two outlines, not observing mutations (§B8.7) |
| ~~`IntersectionObserver`, `ResizeObserver`~~ | lazy loading, virtual lists, responsive components | **built 2026-08-09**, driven from the settle loop (§B8.2) |
| ~~`localStorage` / `sessionStorage`~~ | absence throws or breaks init paths | **built**, deliberately non-persistent; see §B6 |
| ~~`history.pushState`~~ | SPA routing | **built**, and it moves `location` with it. For a while it did not, so a router reading its own route back got the page it had already left |

### Tier B: blocks a large fraction of real applications

All built, most of it driven by §B8 rather than by this list:

* Real event types: `MouseEvent`, `KeyboardEvent`, `InputEvent`, `CustomEvent`
  with `detail`, plus `on*` handler properties.
* Form semantics: `input`/`change` on typing, checkbox, radio, `select` with a
  live `selectedIndex`, `FormData`.
* `closest()`, `matches()`, `dataset`, `cloneNode`, `insertAdjacentHTML`, a real
  `DOMTokenList` over whichever attribute holds the tokens.
* `AbortController`, `Headers`, `Request`, and **concurrent `fetch`**: six on
  the wire at once, so an SPA's fan-out is no longer a waterfall of our making.
* `window.scrollTo`, `scrollY`, and the viewport dimensions, which nothing had
  ever exposed.

### Tier C: the tail

Built since, because a real page asked: **custom elements** (define, upgrade
existing markup, the lifecycle callbacks), `TextEncoder`/`TextDecoder`,
`structuredClone`, `crypto.getRandomValues` and `randomUUID` over the OS CSPRNG,
`XMLHttpRequest` over the same queue `fetch` uses.

Still absent, and still unscheduled: Canvas 2D, WebSocket, Workers,
**WebAssembly**, Shadow DOM, SVG DOM, Streams. Shadow DOM is the interesting
one. The application corpus includes two design-system sites that use it, and
neither asked for it, because their documentation pages are server-rendered.
That is the rule working: nothing here is added until a page in §B8 needs it.

---

## B6. What this browser deliberately is not

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

* cookies: session lifetime only, destroyed with the process
* `localStorage`/`sessionStorage`: small maps, never a file
* history: the current page and a short navigation list
* clipboard: a sandbox-local buffer, never the host's
* dialogs: `alert` to the console, `confirm` from policy, `prompt` refused
* downloads: handed up to h5i as a response, never written as a file

**Not cut, because cutting them makes this a static HTML renderer rather than a
browser**: DOM mutation and query, CSS cascade with flex/grid/position/overflow,
click/input/change/submit/focus/keyboard, promises and microtasks and timers,
`fetch` with redirects and TLS, **ES modules**, forms, images, web fonts,
navigation, the rendered result, and console plus exception capture.

**No iframes.** Not "same-origin only": none. Each iframe is a second document,
a second script realm and a navigation boundary. It is not a feature, it is a
second browser.

---

## B7. Thalora: read it, do not adopt it

`Brainwires/thalora-web-browser` (MIT, 216k lines of Rust, Boa-based, built for
agents) is the same thesis and worth reading closely. It is proof that this much
*can* be built on Boa. It is not evidence that this architecture gets you there
faster, and three of its choices are worth studying specifically as things not
to repeat.

### B7.1 Why it cannot be a dependency

1. **It is built on Boa's internals, not Boa's public API.** Its `Document` uses
   `IntrinsicObject`, `BuiltInBuilder` and `StandardConstructors`, which upstream
   Boa declares `pub(crate)`. That is why `engines/boa` is a submodule pointing
   at their own fork. Using their bindings means owning a fork of a JavaScript
   engine and its security updates.
2. **Its DOM is its own**: `html5ever` plus `taffy`, state in
   `Arc<Mutex<HashMap<..>>>`. Our bindings sit on Blitz's `BaseDocument`, which
   is also what Stylo styles and what we paint. Porting means rewriting the body
   of every binding; only the shape transfers.
3. **It does not paint.** No rasteriser, no screenshot: `taffy` is layout only.
   The visual half, which is what makes `h5i ui` possible and separates us from
   Lightpanda, is not in there.

It also uses hand-rolled CSS over `taffy` where we get **Stylo**, Firefox's
production cascade, through Blitz. Moving toward their stack would be a
compatibility downgrade.

### B7.2 Three cautionary findings, checked against the source

**It has the dual-DOM problem this design exists to avoid.** JavaScript mutates
Boa-side element data; layout runs over a *separately re-parsed* tree:
`renderer/layout_bridge.rs:212` calls `scraper::Html::parse`, and the CSS path
builder walks scraper's `ElementRef`. So the DOM script sees and the DOM that is
laid out are not one tree, synchronised through serialised HTML. That is exactly
the drift §B2 refuses, and it is the strongest available argument for the
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
by the engine itself. **When we build ES modules (§B5 Tier A), every module fetch
goes through the same broker as HTML, `fetch`, images and fonts, and a bare
specifier that does not resolve is an error the agent reads, not a silent trip
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

### B7.3 What it is genuinely worth

Its module inventory is the best available map of which Web APIs an agent
browser needs, written by someone who did the work: `dom/` is 25k lines,
`events/` 7.6k, `storage/` 12k, with a file per API. §B5 cites it per row for
exactly that reason.

The right way to use it: **extract the backlog and the test cases, not the
code.** For each API we take from their list, find the matching Web Platform
Test and make that our test, so our compatibility claim rests on the standard
rather than on their implementation. Their Boa binding *patterns* are worth
reading; their DOM, network and renderer architecture is not worth adopting.

## B8. Measure, then build

Which APIs matter cannot be answered from a chair, and the instrument already
exists: every unsupported call is counted and surfaced in the snapshot.

**The corpus run.** Point the engine at fifty real sites with `--script`,
collect the ranked counts, and let the priority order write itself:

```
note: this page used Web APIs this engine does not have
      (Element.style x41, MutationObserver x6, closest x4)
```

An afternoon, and it turns §B5 from a considered guess into a table. It must
happen *after* §B4, or the results measure our own bugs.

Where the corpus and Thalora's inventory agree, build it. Where they disagree,
the corpus wins: it is this decade's web, not a specification of it.

### B8.1 First run, 2026-08-09

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
  ran regardless of `type`, so pages embedding state as JSON, github.com among
  them, had it parsed as JavaScript, filling the console with syntax errors that
  blamed the page.
* **HTTP errors were rendered as the page.** crates.io answered 404, the engine
  rendered the error body, and the outline came back empty with nothing anywhere
  saying why. The status was in the request log and nowhere an agent looks.
* **Missing APIs did not name themselves.** A global we never defined threw a
  bare `ReferenceError`; a method on a half-defined object threw
  `TypeError: not a callable function`. Neither reached the unsupported list, so
  the measurement could not see them: the method depends on missing things
  reporting themselves, and they were not.

**The headline result: for the pages agents actually read, script adds nothing
to the outline.** Not one of 28 sites rendered materially more with `--script`
than without. Docs, references and wikis are server-rendered; script adds
interactivity, not content. That is a real finding about the workload and it
argues the reading case was close to solved before any of this.

Two caveats keep it from being stronger than it is. The harness allows only the
page's own host and a few common CDNs, so **17 cross-origin scripts were denied
by policy** and those bundles never ran, so the script-heavy end of the corpus is
therefore under-tested. And the remaining 13 TypeErrors and 6 ReferenceErrors are
still anonymous: they come from pages touching DOM properties we return
null/undefined for, which the `missingApi` list does not cover because they are
not globals.

**What the corpus asks for next**, in its own order: `matchMedia` (answered now,
still recorded), `document.cookie`, `IntersectionObserver`, `setInterval`.

`document.cookie` is the interesting one, because it looked like a deliberate
refusal and turned out to be a false choice. See §B8.2.

### B8.2 Second run, same day: the list is empty, and that is not the same as done

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
`missingApi`, which covers globals, cannot name them. The instrument now
reports nothing because it cannot see what is left, which is a different fact
from there being nothing left. Naming those is the next measurement problem, and
it has to be solved before another run means much.

### B8.3 Fixing the instrument, which was the actual next task

Two blind spots, closed:

* **Unknown properties on objects we own.** `wrap()` and `document` now return a
  `Proxy` whose `get` records a name that is on neither the prototype chain nor
  the object itself. A property we implement takes the plain path, and so does
  an expando the page assigned and reads back, so **a working page records
  nothing at all**: the list stays a list of gaps rather than a log of traffic.
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
`customElements.define`, which the list now names. A count going up because
pages reach deeper is the shape of a real measurement.

Two things the remaining list should not be misread as:

* **`$` is not an engine gap.** It is jQuery, from a CDN the corpus policy
  denied. The page is right to fail; the fix is a policy decision about asset
  hosts, not a binding.
* **The residual `TypeError`s are mostly selector misses**: `querySelector`
  returning null for markup that genuinely is not there. That is correct
  behaviour, reported honestly, and no amount of API work removes it. Naming
  *where* it happened needs source positions from Boa, which is a separate job.

### B8.4 Answering the named list, and what it caught in the answers

Everything §B8.3 surfaced is built. In order of what they were worth:

* **Custom elements, for real.** `define` upgrades the markup already on the
  page, delivers the initial values of `observedAttributes`, and runs
  `connectedCallback` once the node is genuinely in the tree. Defining without
  upgrading would have been the worse kind of half-support: a page that renders
  its markup server-side and defines its components in a deferred bundle, which is most
  of them, would register everything, see no error, and render nothing. The id
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
  window, so the idiom read "already at the bottom" everywhere.
* **`window.innerWidth`/`innerHeight`/`scrollY`** and the scroll methods, which
  nothing had ever exposed. This one the instrument could not have found:
  nothing wraps the global object, so they were simply undefined, and a layout
  that measures instead of asking `matchMedia` got `NaN` out of its own
  arithmetic. Found while chasing an unrelated scroll bug.
* `compareDocumentPosition`, `contains`, `getRootNode`, `isConnected`,
  `defaultValue`, `getElementsByTagName`, `getElementsByName`, `importNode`,
  `createNodeIterator`/`createTreeWalker`, and `implementation`, which names
  `createHTMLDocument` as refused rather than handing back a broken document,
  because a second document really is out of reach when there is one tree.

**The run after that caught three bugs in the answers themselves**, which is the
argument for the instrument in one line:

| reported | what it actually was |
|---|---|
| `Element._h5iConnected` | *our own* bookkeeping flag, stored on the node, read before it was set |
| `Element.tagName` | a page reading `tagName` off a **text** node; every node was labelled "Element" |
| `$`, still | jQuery that *loaded and threw*, not one that was refused |

All three are fixed: the flag moved off the nodes, labels follow the node's
actual type, and a script that throws is recorded as not-run alongside one that
was refused: its globals are undefined either way.

That left one ask, `Text.tagName`, and it was a false positive worth a rule:
**a gap is only a gap if a real browser would have answered.** An element
property read off a text node returns undefined in every engine there is, so
claiming it would have sent us building something that does not exist. The
proxy now stays quiet in exactly that case, and `document.namespaceURI` and
`ownerDocument` are defined-as-undefined and null for the same reason.

### B8.5 Where the corpus stands

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

One page also rendered materially more *with* script for the first time: the
Rust book, 35 lines to 171, which is the first evidence in this file that
running script buys an agent anything at all on a real documentation site.

What the four turned into:

* **`matchMedia` answers from the real viewport.** Returning `false` to
  everything is not neutral: a responsive layout asks and then commits to the
  branch it was told, so a wrong answer is a wrong page rather than a missing
  feature. `min-width`, `max-width`, `orientation` and `prefers-color-scheme`
  have correct answers at a fixed viewport with a known scheme; a feature
  outside that set still records itself.
* **`document.cookie` exists, and honours `HttpOnly`.** The earlier framing,
  that exposing it would break "an agent can be logged in without reading the
  credential", was a false choice, because a browser has the same problem and
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

### B8.6 A second corpus: applications, not documents

The document corpus reached zero asks and zero anonymous errors, and then
stopped being informative, **because four of its 28 pages still rendered
nothing and not one of them was a missing API**:

| site | why | not |
|---|---|---|
| crates.io | server answered **404** to a request that sent no `Accept` | an API gap |
| stackoverflow | **403** bot wall, rendering as one line | an API gap |
| json.org | a `<meta refresh>` this engine never followed | an API gap |
| vitejs.dev | redirected to vite.dev, correctly refused, unhelpfully explained | an API gap |

That inverted the plan: the next frontier was the network layer and the honesty
of the report around it, not more bindings. All four are fixed (§B8.8), and
crates.io answers 200 and json.org renders 299 lines instead of 1.

So the corpus was **pointed at applications instead** (SPAs, interactive demos,
design systems) because a documentation corpus will never ask for routing,
storage or template cloning when it contains nothing that does them. It named,
immediately and specifically:

* **`<template>.content`**, and this was not a small gap. Its absence made
  `template.content.cloneNode(true)` throw `cannot convert 'null' or 'undefined'
  to object`, which was the *entire text* of **fifteen module failures**. Clone,
  query, fill, append is how every framework renders a row.
* **Scoped selector queries that do not scope.** `query_selector_all` always
  starts at the document root and the engine narrowed by ancestry afterwards, so
  a **detached** subtree was invisible, which is every cloned template before it
  is inserted, exactly when a framework searches one. Stylo's fast path consults
  the document's id and class caches and reports "handled, nothing found" rather
  than falling through, so scoped queries now walk the subtree and match element
  by element. `matches()` had the same bug and answered false for anything
  detached.
* **`location.pathname`**, which was undefined, and `pushState`, which never
  moved the address at all.
* `relList`, `attributes`, `firstElementChild`, `getAnimations`,
  `document.contentType`, `meta.content`, `on*` handlers.

### B8.7 What the instrument caught in its own reflection, twice more

* **A framework's private field is not an API gap.** Solid reads
  `document._$DX_DELEGATE` before setting it, and the ask list carried it as
  something this engine was missing. No web platform property begins with `_` or
  `$`.
* **"module failed" names nothing**: the same anonymity §B8.3 removed from
  script errors, one level up. Modules now carry their specifier into the
  failure. The reporting proxy also watches `location`, `history`, `navigator`,
  `performance`, the storages and `crypto`, which is where the last unnamed
  failures were hiding.

**The corpus now lives in the repository**, after a crash took the only copy
along with the scratchpad it sat in. `corpus/run.py` is the network instrument;
`tests/corpus.rs` is the part CI runs: the same patterns against local
fixtures, asserting the two properties that matter, and it found two real bugs
the moment it was written.

Applications corpus: 20/20 load, one ask left, **zero anonymous errors**.
Fourteen module failures remain, each now attributed to a named bundle. Going
further needs source positions, which is the concrete cost of the Boa
constraint below and the clearest argument for revisiting it.

### B8.8 The network layer

Not bindings, and the reason four pages read as empty:

* **Request fidelity.** No `Accept`, no `Accept-Language`, and a user agent that
  named only the crate. The agent string is honest rather than imitative. It
  names this engine and does not claim to be Chrome, and is now one constant
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
* **`fetch` is concurrent**: six on the wire at once, the browsers' per-host
  figure, chosen so a page with two hundred images cannot become two hundred
  threads inside a box with a memory ceiling. Waiting on the wire uses *real*
  time against its own budget, since the virtual clock is free to advance and a
  round trip is not.

### B8.9 What it costs, measured

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

1. **A page with no script no longer builds a realm.** That costs ~15 ms, 114
   KiB of prelude parsed and evaluated, and a page with nothing to run was
   paying all of it for a realm never asked a question. It is also reported
   correctly now: "had none to run" is a different fact from "script is off",
   and a page with no script is *settled* rather than unknown.
2. **Collections are no longer watched.** Wrapping a query result in the
   reporting proxy cost **3.9x on iteration**, 674 µs against 174 µs for a
   400-node result, because every index read goes through a trap and
   `for (const el of query)` is the hottest line in DOM code. An array already
   answers everything a `NodeList` does except `item` and `namedItem`, which are
   implemented, so the naming it bought was small and the price was not.
3. **`matches()` is a direct predicate.** It had been asking the *parent* for
   all matching descendants and checking membership, which made `closest()`
   walk a subtree per ancestor: quadratic on any page whose framework calls it
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
Reusing one across navigations would remove it, and is *not* safe: a page could
leave state for whatever loads next, which is the same reason the cookie jar is
cleared across origins.

**Measured and rejected**, twice, and both are recorded so nobody tries again:
precomputing the set of known property names so the reporting trap does a hash
lookup instead of walking the prototype chain changed nothing (the cost is Boa
dispatching into a JavaScript trap at all); and raising the loop bound from 5 to
50 million turned a site that returned in three minutes into one that had not
returned in four.

### B8.10 Source positions, and what they found

Boa 0.21 maps a program counter back to a source position. It is pinned by
**revision of upstream `main`**, not by release: the 0.21.1 release pins three
icu crates to `~2.0.0`, which excludes what parley requires, and parley arrives
through blitz. Upstream relaxed those pins after the release, so a pinned commit
needs no fork and no patched source, and buys five months of engine and parser
fixes over a five-month-old tag, which turned out to matter.

Two other routes were tried and rejected with evidence. **Vendoring** the two
crates worked and cost 7.5 MB and 508 files for a two-line change. **Forking**
at `v0.21.1` plus one commit also worked, and is one commit, one file, six
lines, but it is a fork to carry, and upstream `main` had already made the same
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
| `EventTarget is not defined` | a real base class, independent of the tree; a store is not a node |
| `HTMLAnchorElement`, `HTMLButtonElement`, `HTMLTemplateElement`, … | the per-tag constructor family, all aliasing `Element` |
| `Invalid URL: /assets/…` | `import.meta.url`, which bundlers resolve every sibling asset against |
| `RuntimeLimit: exceeded recursive calls` | Boa's 512-frame default, which Next.js exceeded while merely initialising |
| `DOMParser is not defined` | parse-to-subtree, with no script inside it running |
| `not a callable function` | collections that were not collections; see below |

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

### B8.11 Three things that are not ours, stated plainly

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

   All four are valid JavaScript: node runs them, as script and as module. The
   asymmetry is the finding: **`var` handles it and `let`/`const` do not**, so
   this is a defect in the lexical-declaration path rather than a deliberate
   choice about semicolon insertion. Per the grammar a `,` continues a
   `BindingList`, so it is not an offending token and no semicolon may be
   inserted.

   Confirmed with this engine entirely out of the path: `Context::default()`,
   `Source::from_bytes`, no host, no module loader, no HTML, so it is not ours.
   Minified bundles that keep `/*! @license */` comments between declarators
   produce exactly this shape, which is how lit.dev fails.

   Not fixable here, and not worth working around: rewriting a page's own source
   would move every line number we just gained and could corrupt string
   literals, the plausible-wrong answer again. What *is* ours is that the
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
     and `get_cancellation_token` hands it out as an `Arc<AtomicBool>`, so a
     watchdog thread can set it, which is the only wall-clock lever the engine
     offers. A page building 200,000 promise jobs is now stopped at 15 seconds,
     renders what it had, and says so in the engine's own voice. This is the
     shape a promise-driven page actually has.
   * **One long job.** lit.dev looked like the other shape: a module graph
     evaluating depth-first inside a *single* job, beyond any token check.

   **That second diagnosis was wrong, and wrong in the most useful direction.**
   The page was not pathological; *this engine* was slow enough to make it look
   that way. `appendChild` into the document cost 40 µs against 13 µs for a
   detached one, because every insertion walked to the root to ask whether it
   was connected and then walked the inserted subtree looking for custom
   elements, on pages that had defined none. An early return when nothing is
   defined, and a native `isConnected` that walks in Rust instead of one call
   per ancestor, took it to **7 µs, the same as the detached case**.

   lit.dev went from three and a half minutes to fifty seconds, material-web
   from a timeout to forty-five, and both now *return*. A second pass on the
   mutation-record path: the old value of an attribute was read from the tree,
   and a record object with two arrays allocated, on every write, whether or not
   anything was observing, took the hot operations to:

   ```
   createElement    5.5 µs      textContent  2.0 µs
   setAttribute     4.0 µs      appendChild  4.0 µs
   ```

   from 7 / 8.5 / 18 / 40.5 µs before either pass.

   **And then the sites did not get faster**, which is the part worth writing
   down. lit.dev renders in 0.27s without script and 46s with it, of which 0.5s
   is network; the DOM is no longer where the time goes. Nor are the budgets: a
   shared deadline across the script phase and the settle, which used to add up,
   changed nothing either, because the time is inside a *single* evaluation
   that neither a between-jobs token nor a between-scripts budget can interrupt.

   So the original diagnosis was half right and recorded too confidently in both
   directions. The engine was slow enough to turn a heavy page into a hang, and
   fixing that was worth four times on the hot path; what is left really is one
   uninterruptible unit of work, and bounding it needs an interrupt inside the
   interpreter loop. That is still upstream, and it is now the only thing
   standing between this engine and a page like lit.dev.
3. **Total CPU is unbounded.** Boa exposes no wall-clock interrupt, so the
   engine bounds what it can (one loop, recursion depth, stack size) and a
   caller that cannot wait must impose its own timeout. Raising the loop bound
   from 5 to 50 million turned a site that returned in three minutes into one
   that had not returned in four; the bound stays low enough to return, and
   trips are reported so a thin outline is explained rather than mysterious.

Both limits had to move together: raising the frame count alone changed nothing,
because the *stack size* was what a deep call actually hit.

### B8.12 A page's own errors, made legible

`console.error(someError)` rendered as `{}`, because an Error has no enumerable
own properties and the console used `JSON.stringify`. remix.run produced **1487
lines saying exactly that**, and the message, the one part an agent needed,
was what got thrown away. Errors now render as name, message and trace;
functions and DOM nodes say what they are; and an object that stringifies to
`{}` reports its constructor rather than an empty shape.

---

### B8.13 Insertion was not moving nodes, which is what a keyed diff is made of

preactjs.com rendered 178 lines without script and 65 with it, with no errors
and nothing on the unsupported list: its shell and its sidebar, and nothing
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
parented, so every *move* was a deletion. That is the operation a keyed diff is
built out of: preact reorders by re-inserting nodes it already holds, and each
reorder threw one away until the article was gone.

Detaching first fixes it, and preactjs.com now reads **178 lines with script,
matching its prerendered reading exactly**.

Two things worth keeping from how it was found. The failure was invisible to
every instrument in this project (no error, no unnamed API, no anonymous
console line) because nothing was *wrong* from the page's point of view; it
asked for a move and got a deletion. And the fixture harness had been running
every page's scripts twice, since `PageFactory::from_html` already runs them:
harmless for a script that assigns, wrong for one that appends. Both were found
by writing a test that appends.

---

### B8.14 Shadow DOM, flattened, and where the interrupt actually is

**Shadow DOM is built**, after two sites asked for `Element.shadowRoot` once the
performance work let them run far enough to want it. That is the rule this file
keeps: nothing is built until a page asks, and lit.dev and material-web asked.

This engine has one tree and blitz has no notion of a shadow one, so a shadow
root is a **view of the host element** and everything a component renders into
it lands in the host. The trade is stated rather than discovered:

* **Kept**: the content renders and is therefore readable, `host` and `mode`
  answer, `nodeType` is 11, a closed root is not handed out, and light children
  are projected into a `<slot>` if the component declares one, otherwise held
  aside, because a browser stops rendering them and showing a component's input
  beside its output would be worse than showing neither.
* **Lost**: encapsulation. `document.querySelector` reaches inside a shadow root
  here and would not in a browser, and styles do not scope.

That is the same flattening a browser's own accessibility tree performs, and for
an engine whose product is a readable account of a page it is the right half to
keep.

**The interrupt exists, and not where it is needed.** §B8.11 recorded that Boa
exposes no way to stop a running evaluation. That was wrong:
`Script::evaluate_async_with_budget` is public, and the VM yields to the caller
every N instructions: a real interrupt, for classic scripts. `Module` has only
`evaluate()`, with no budgeted variant, and lit.dev is modules end to end. So
the mechanism is there, the upstream ask has a precise shape,
`Module::evaluate_async_with_budget`, and until it exists a module graph is
still one uninterruptible unit.

---

### B8.15 A review pass: what it found in its own work

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
a person, so those are filtered, alongside the `_` and `$` prefixes already
filtered for the same reason.

**Where the application corpus stands after all of it:** 20/20 load, 17 usable
outlines, 2 render materially more with script, **0 render less**, 0 anonymous
errors, and **1 site** that cannot be read with script at all: lit.dev, whose
module graph is the one uninterruptible unit left (§B8.14).

---

### B8.16 The "cosmetic" duplication was text nodes being immutable

preactjs.com rendered its version as `v11.0.0-beta.111.0.0-beta.1`. It looked
cosmetic and was filed that way. It was not.

Reproduced with real preact against the page's actual markup: a single text
node `v1.0.0` hydrated against a vnode with two text children, which is what a
prerendered page gives a component that renders `v{version}`:

```
before   kids=1  text="v1.0.0"
after    kids=2  datas=["v1.0.0", "1.0.0"]      <- ours
after    kids=2  datas=["v", "1.0.0"]           <- a browser
```

Preact assigns `dom.data = 'v'` to the node it is reusing. **That write did
nothing**, because writing to a text node took the path meant for elements:
clear the children, which a text node has none of, and append a new text child, which
is meaningless. Blitz has `set_node_text` for exactly this and it was never
called.

So text nodes were immutable, and that is the single most common mutation any
reactive UI performs: every framework updates text by assigning `.data` or
`.nodeValue` to a node it already holds. The duplication was one visible symptom
of a general failure to apply text updates at all.

preactjs.com now reads **178 lines with script, matching its prerendered
reading**, and shows `v11.0.0-beta.2`, the version it *fetched*, where before it
showed the stale prerendered `beta.1` twice. The update applies now.

Worth noting how it was found: not by reading the DOM code, but by reproducing
the page's exact shape against the real library and comparing what each engine
ends up with. The bug was three layers below where it showed.

---

### B8.17 Measured against Chromium

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
the memory, in one process rather than seven, from a binary a ninth the size,
and it is *slower*, because Chromium has a JIT and this has an interpreter.
Anyone quoting the memory figure without the speed one is quoting half a
measurement.

They are also not a claim of equivalence, and the corpus in §B8.6 is the reason:
of twenty applications, this engine reads seventeen usefully and **one not at
all**. Chromium reads all twenty. The right sentence is "a seventh of the memory
for the pages it can read", and the second half of that is doing real work.

The comparison deliberately records what each run actually *read*, so a run that
produced nothing cannot appear as a fast, small success. The counts are not
comparable to each other. Ours is a summarised outline capped at 300 lines,
Chromium's is a raw DOM dump, and they are there to prove each engine did the
work, not to be divided by one another.

Worth stating for anyone reaching for these in a comparison: this is one page
per process, which is how an agent reads. A long-lived Chromium amortises its
browser and GPU processes across many tabs and would look better per page.

---

### B8.18 Two more corpora, and the crash they found

Two writing systems' worth of blind spot, and a shape of page neither corpus
contained.

**International**: fourteen pages in CJK, Arabic, Hebrew, Persian, Thai,
Devanagari, Greek, Cyrillic and Vietnamese. Text shaping, bidi and CJK line
breaking all run through parley, and every page measured until now was Latin: in
an engine whose entire product is extracted text, none of it had ever been
exercised. **14/14 load, 14 usable outlines, zero errors, zero anonymous
errors**, and the extracted text is correct, checked character by character
rather than by line count, because a corpus that counts lines would happily
report three hundred lines of mojibake.

**Structures**: big tables, forms, search results, plain RFCs, and markup old
enough to predate the conventions the rest of the web settled on. This one paid
immediately.

**The GNU bash manual crashed the engine.** One megabyte of single-page HTML,
and blitz panics with `attempt to subtract with overflow` in layout
construction. A panic is the one outcome an agent cannot act on: not a thin
page, not an error it can read, but a dead process and no answer at all.

Layout now runs behind a guard. The panic is caught, the document is read in
whatever state layout reached, and the snapshot says so: the page returns **500
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

### B8.19 Two of the three were worth building; one was not an API

`document.write`, `CSSStyleSheet` and `document.respec` came out of the
structures corpus. Checking each before building it turned out to matter.

**`document.respec` is not a web API.** The W3C pages call
`document.respec.ready.then(...)`: it is ReSpec's own global, a page expando in
the same class as Solid's `_$DX_DELEGATE`, and implementing it would have been
implementing someone's variable name. It stays reported, and the ask list
carrying it is the cost of a filter that cannot know every library's field.

**`document.write` is emulated where it can be and refused where it cannot.** A
browser inserts at the parser's position; this engine parses the whole document
before running anything, so that position does not exist, but `currentScript`
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
`<style>display:none</style>` did not hide anything, because **the outline does
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
for instructions aimed at whatever is reading, and the fence in §B1 exists for
exactly that threat and this walks around it. It is the next thing to fix, and
it deserves care rather than a quick filter: content revealed later by script,
and the difference between `display: none` and off-screen accessibility text,
both decide whether a filter helps or quietly deletes the page.

---

### B8.20 Driving a page, and the sentence that contradicted itself

**Every corpus until now loaded a page and read it. None clicked anything.** An
agent's loop is read, act, read the difference, so two thirds of what this
engine is for went unmeasured, while the session verbs, the semantic delta and
the action-to-request correlation were all built and tested only in isolation.

`tests/corpus.rs` now drives as well as reads. Four fixtures, each asserting on
what the *delta* reports rather than on the page, because a change nobody can
see is the same as no change:

* typing into a field and submitting adds an item, and the delta names the new
  item without reporting the rest of the page as replaced;
* clicking a filter that rewrites a list reports the items that went and **not**
  the footer that did not;
* clicking something inert reports *no change*, which is a result an agent needs
  rather than the page handed back to be re-read;
* a router click moves the view and the address together, while the document's
  own URL stays put: the router moved, not the fetch.

They pass, which is worth stating plainly: the interaction path works, and it
had never been measured end to end.

**And `<noscript>` was in the outline.** A browser shows that content only when
script is off; this engine showed it always. So a page whose script ran
perfectly still handed an agent the sentence *"JavaScript is disabled in your
browser"*, not a cosmetic slip but a direct contradiction of the reading it
appeared in. crates.io's **entire outline was that sentence**.

crates.io now reports zero lines and a note saying so, which is the honest
answer: its SvelteKit app really does render nothing here. Why it does remains
undiagnosed: the entry shape reproduces perfectly in isolation, dynamic
`import()`, `currentScript.parentElement` and all 75 subresources check out
individually, and it is better recorded as unexplained than as fixed.

pypi's search page joins the challenge matcher, which also normalises
typographic apostrophes: pypi writes "couldn't" with U+2019, and a matcher that
only knew `'` would have missed it while looking like it had checked.

---

### B8.21 Hidden content is no longer read, and Chromium settled the argument

The outline carried `display: none` content, the `hidden` attribute, and
`visibility: hidden`. Two problems, and the second is the serious one: the
outline claims to be an account of what a page *shows*, and invisible text is the
classic vehicle for instructions aimed at whatever is reading it, the threat the
untrusted-content fence exists for, walked around by text a human never meets.

`display: none` and `hidden` are filtered now, asked of the style engine rather
than re-derived: a node with no primary styles is not rendered, and a node with
styles can still resolve to `display: none`, which is the common case because it
is what a stylesheet says. The first attempt checked only the former and filtered
the attribute while missing every CSS rule; the difference between the two took
a probe to find.

**`visibility: hidden` is deliberately kept.** That content occupies its space,
is routinely toggled by script, and is a shape off-screen accessibility text
sometimes takes; filtering it would risk deleting page content to fix a smaller
problem.

**The measurement then produced an alarming number, and it was right.** The Rust
book fell from 171 lines to **6**. That is the failure mode this change was
warned against, silently deleting a page, so it was checked against Chromium
rather than reasoned about: Chromium's DOM for the same page carries
`<html class="js light">` and **no `sidebar-visible` class**, so mdBook's sidebar
is not shown there either.

The six lines are the chapter: its heading, its opening paragraph, its list. The
165 that went were navigation **no reader ever sees**, and this engine had been
handing them to agents as page content. A number that looks like a regression is
worth checking against a browser before it is treated as one, and worth checking
before it is treated as a success, which is the same discipline pointing the
other way.

---

## B10. What is next, 2026-08-09

> **Superseded in part by §B11.** This section is the queue as it stood before
> Kitesurf was re-read against a built engine; §B11.5 is the current one. Kept
> because items 1 and 2 record how they were closed, and because item 4 is a
> useful example of the rule working: Shadow DOM was listed here as "if and when
> a page asks", a page asked, and §B8.14 built it.

Tiers 0 through 4 of the plan this section replaces are done. What the work
itself surfaced, in the order the evidence supports:

1. ~~The fourteen module failures~~: **four left** (§B8.10), each with a stack
   trace. Two are the Boa parser bug of §B8.11 and are upstream's to fix.
2. ~~Boa 0.21~~: **done**, pinned by revision on the dependency itself rather
   than through `[patch.crates-io]`: `=1.0.0-dev` *looked* like a pin and pinned
   nothing, since upstream's `main` carries that version string while changing
   daily. The commit hash now sits in the manifest of the crate that depends on
   it, where a reader looks for it, and nothing else in the workspace depends on
   boa so the patch indirection bought nothing (§B8.10).
   The pin should move to a release when boa cuts one, and the `[patch]` block
   deleted then. That is no longer a thing to remember:
   `scripts/check_boa_release.sh` asks crates.io on every CI run whether a
   published boa's icu requirements have stopped clashing with blitz's parley,
   and fails the build the day one has. It reads parley's requirement from the
   lockfile rather than assuming it, so it stays true when blitz moves, and it
   has a floor at 0.21, the first version with source positions, because
   older releases predate the icu dependency and so "do not clash" while being
   unusable. The first draft recommended 0.17 for exactly that reason.
3. **Two sites that now time out**, lit.dev and material-web, because they get
   further than they used to. Either the engine gets faster or the corpus learns
   to report a partial render as a result rather than a failure.
3. **The realm costs ~20ms to start** and is rebuilt per page. A resident
   session that reuses one realm across navigations would remove it from every
   step after the first. Measured, not guessed; see §B8.9.
4. **Shadow DOM**, if and when a page in §B8 asks. Two design-system sites in the
   application corpus use it and neither asked, because their docs pages are
   server-rendered. Adding it now would be building for a page we have not met.
5. **A corpus that needs a login.** Everything measured so far is public, so
   LOGIN mode and the cookie jar are tested but not *exercised* against a real
   session-gated application. That is the next honest extension of §B8, and the
   one most likely to find something surprising.

The rule that produced everything above stays: **nothing is built until a page
asks for it, and an instrument that cannot name what is missing is fixed before
anything it failed to name.**

---

## B11. Kitesurf, re-read against a built engine, 2026-08-09

§7.1 surveyed Kitesurf on 2026-08-07 and drew the routing rule from
it: two engines, by origin, one policy. That section remains the authority on
*position*. This one is narrower and later. The engine now exists, so the
question is no longer "what does this mean for scope" but **"what does the
comparison change about the order of work"**, which is what this file is for.

### B11.1 The stack is less shared than it looks

Read casually, Kitesurf is this engine with a Cloudflare account attached: Blitz
for HTML and layout, Stylo for CSS, Parley for text shaping, Rust throughout.
The JS is the exception and it is the important one. **Page script runs on V8**,
because a Worker already is V8; Boa appears only for `eval`, as a stand-in until
Workers exposes dynamic evaluation natively. §B7.1 recorded this and
it stands.

Three things follow, and the first two are corrections to a comparison that is
tempting to make and wrong:

* **The wall-time figures are not comparable.** Kitesurf reports 1.7-1.8x slower
  than Chromium; §B8.17 measured this engine at roughly 1.3x. That is not a win.
  Theirs includes an isolate boundary and a WASM-compiled DOM; ours includes an
  interpreter where theirs has a JIT. Different corpora, different hardware,
  different bottlenecks. Neither number bounds the other and neither should be
  quoted against the other.
* **Boa still carries no precedent.** The hope that Kitesurf's success validated
  Boa for real web applications does not survive reading what Kitesurf runs
  script on. It does not use Boa for that. This engine is the precedent, which
  means §B8's corpus is not a nice-to-have measurement, it is the only evidence
  that exists. The swap trigger at §B7.1 is unchanged.
* **Memory is the comparison that survives.** Kitesurf reports 4.7-7.0x less
  than Chromium; §B8.17 measured 7.4x. Those are close, measured the same way,
  and both are large. This is the number to state.

### B11.2 What the comparison does not change

Three of Kitesurf's stated gaps are already answered here and should not be
re-opened as work:

* **Video and WebGL.** Not in scope for the light engine, and not a gap, because
  a coding agent testing a video player is testing its own application, which is
  loopback, which routes to Chromium. Kitesurf must name these because it has no
  Chromium half. We do (§B7.1).
* **Persistent authenticated sessions.** Kitesurf cannot have them; this is the
  one place where "there is a human at this machine" is a capability and not a
  limitation. `session login` hands the page to the person at the viewer and
  takes it back (§B5.4, §B8.20). Answered, though see 11.6.
* **Speed.** Never the claim, for the reason at the top of this file: shipping
  less browser beats any benchmark table, so a benchmark table is not a moat.

### B11.3 What it does change: two gaps, and one advantage never stated

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

### B11.4 MCP: decided against, 2026-08-09

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

### B11.5 The queue

Ordered by what the evidence supports, not by size.

**First, because it is the least-verified thing we claim.**

1. **A corpus that needs a login.** Unchanged from §B10.5 and now more urgent, not
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
   (§B8.3) does not get to make an exception for its own conformance.

**Third, the interoperability work, sized once 2 has told us what we can claim.**

4. **A CDP subset over WebSocket.** The useful floor: `Target` attach/create,
   `Page.navigate|captureScreenshot|loadEventFired`,
   `Runtime.evaluate|callFunctionOn|consoleAPICalled`,
   `DOM.getDocument|querySelector|getBoxModel`,
   `Input.dispatchMouseEvent|dispatchKeyEvent`, `Network` request/response
   events plus cookie get and set, `Emulation.setDeviceMetricsOverride`.
5. **The unimplemented half of CDP must be loud.** A partial protocol that
   answers to the name of the whole one is the `missingApi` lie at protocol
   scale (§B8.4): Playwright will call methods we do not have, and a silent or
   plausible answer there is worse than an error, for exactly the reason a
   plausible wrong answer is worse than no answer anywhere else in this engine.
   An unimplemented method returns a named error and the conformance list is
   published.
6. **REST quick actions**: screenshot, extract, PDF. Nearly free once 4 exists.

**Fourth, the gaps the corpus itself found.** These are §B8's list and are ordered
by how many pages asked.

7. Boa `Module::evaluate_async_with_budget` (lit.dev evaluates unbounded, §B8.14),
   the Boa `let`/`const` parser bug (§B8.11), the blitz layout panic (§B8.18). All
   three are upstream's and all three are filed.
8. **Canvas 2D**, the largest single missing API by corpus demand.
9. **WebSocket and EventSource.** A live application shows nothing without them.
10. **IndexedDB**, in memory only, consistent with §B6's storage line.
11. **`getComputedStyle` answers almost nothing** (`color` came back empty). It
    is implemented far enough to look implemented, which §B8.3 established is the
    worst state for anything in this engine to be in.
12. **crates.io renders nothing** and the cause is still unknown. SvelteKit-
    shaped; the entry path was verified working in isolation, so the failure is
    somewhere the isolation removed.

**Fifth, performance, none of which is urgent.**

13. **Reuse the realm across navigations.** ~20ms per page, rebuilt every time,
    measured in §B8.9.
14. **Cache the prelude's bytecode.** Three thousand lines of JavaScript parsed
    per realm.
15. There is no JIT and there will not be one. The cost is stated in 11.1 and
    the answer to it is 11.3's reach and §B8.17's memory, not a faster
    interpreter.

**Sixth, the moat, which is mostly already built and under-described.**

16. **Receipts as a checkable artifact.** The one thing Kitesurf's announcement
    does not address at all. Today the guarantee is "no receipt, no request" and
    it is true; what it is not is *verifiable by someone who does not trust the
    binary that wrote it*.
17. **Measure and state the delta snapshot** (§B8.20). No comparable engine
    appears to have one, and re-reading three hundred lines after every click is
    the shape everyone else's agent loop is stuck in.

### B11.6 Two conflicts to settle deliberately

Both are cases where §B6's "never" list collides with something 11.5 wants. Each
should be decided in writing rather than discovered in a corpus run.

**Login flows use iframes and popups; §B6 refuses both.** The strongest claim in
11.2 is persistent authenticated sessions, and real-world OAuth is an iframe or
a popup almost every time. §B5.4's human handoff sidesteps part of this, because
a person at the viewer can complete a flow the engine could not drive, but it
does not help when the flow needs a second browsing context to *exist* at all.
Either §B6 gains a narrow, argued exception for authentication boundaries, or the
login claim is honestly scoped down to form posts. It cannot stay as it is: item
11.5.1 will decide this whether or not it is decided first, and it is better
written down in advance.

**PDF.** §B6 refuses "printing", by which it meant the print UI, and item 11.5.6
wants `printToPDF`. These are not the same feature: one is chrome around a page,
the other is a serialisation of it, and an agent asked to keep a record of what
it read wants the second. Recommended as an exception, on the grounds that the
raster path (`blitz-paint`, vello_cpu) already produces everything it needs.

---

## B12. Running WPT, 2026-08-09 to 08-10

§B11.5.2 argued that seventy corpus pages cannot answer "what fraction of the
platform is implemented", and put conformance ahead of the capabilities it
would measure. This is that work. The rule it operates under is §B8's: **an
instrument that cannot name what is missing is fixed before anything it failed
to name**, and the instrument needed fixing three times before its numbers were
worth quoting.

### B12.1 The instrument

`wpt/serve.py` serves a WPT checkout and substitutes one file.
`resources/testharnessreport.js` is shipped by WPT as an empty seam for a vendor
to fill, so ours fills it and the results come back through the console, which
`open --json` already reports. **Nothing was added to the engine to make it
measurable.** An instrument that requires the subject to grow a port for it is
measuring something other than the subject.

`wpt/run.py` keeps six outcomes apart and lets only three contribute subtests:

| | |
| --- | --- |
| `ok` / `harness_error` / `harness_timeout` | the harness reported. Real data. |
| `no_report` | the engine exited cleanly and the harness never reported. **Unmeasured, not zero.** |
| `engine_timeout` / `engine_crash` | unmeasured. |

`no_report` is the bucket worth chasing: it is where one engine gap stops a file
before it can say what it failed, so emptying it moves the score in steps rather
than in ones. Every fix in 12.2 came out of it.

`wpt/sweep.sh` runs one directory at a time and `wpt/merge.py` totals them.
Chunked for a reason learned the hard way: a single process holding two hours of
results loses all of them when something kills it, and something did. Each test
process also runs under an address-space cap, because several WPT files allocate
until something gives and without a cap the kernel picks the victim, which on
this 8 GiB box was the whole session rather than the test.

### B12.2 Twenty files, four bugs, and a suite that scored zero

The first twenty files scored **0**. Not because the engine was that far off:
one missing binding stopped testharness.js before its first assertion.

* **`self` was undefined.** testharness walks `w != w.parent` from `self` before
  it can run anything. Added with `parent`, `top`, `frames`, `length`,
  `frameElement` and `opener`, not stubs, because §B6 refuses iframes and
  popups, so this document is always a top-level context and every value is what
  a real browser reports for one.
* **The load lifecycle was never fired.** No `DOMContentLoaded`, no `load`, and
  `document.readyState` was the constant `"complete"`. That constant is exactly
  why four corpora never caught it: it makes the *common* idiom work (read
  `readyState === "loading"`, otherwise initialise now), so every page took the
  immediate branch and nothing looked wrong. The other branch never arrived.
  testharness gates every result it will ever report on one `load` listener with
  no readyState fallback, so it scored nothing while looking merely slow.
* **`insertBefore` with an unparented reference node killed the process**, which
  WPT does on purpose. A panic is not a DOM error: it takes the page, the
  snapshot and the receipts with it.
* **`insertAdjacentText` was missing**, which blocked eight files at once
  because testharness renders its own results table with it.

Twenty files went 0 → 199. The fifth fix is the one that found the fourth:
**timer errors now carry a stack**. They said only "timer threw" and withheld
the one thing a caller needs. That has since been applied to all eight callbacks
that swallow an error: a listener, a timer, an observer are each detached from
whatever scheduled them, so the message is all the reader gets.

### B12.3 What the instrument was getting wrong about itself

Two corrections, both of which made the engine look worse than it is:

**276 of 1,503 files never load testharness.js.** Reftests compare renderings
and crashtests only have to not crash; neither can report a result no matter how
well the engine runs it. They were sitting in the unmeasured bucket looking like
engine failures. Counted and named separately, unmeasured fell from 643 to 367
without a single test changing behaviour.

**A large share of WPT is not on disk.** `x.any.js` becomes `x.any.html`,
`x.any.worker.html` and more at serve time, and a static server cannot produce
them. 3,833 such endpoints are skipped and the count is printed, so the
denominator is never mistaken for "all of WPT".

### B12.4 The baseline, and what it asked for

First full on-disk sweep: **33,754 subtests passing of 212,028 scored**, 25,393
files, of which 16,857 reported and 8,536 did not. 36,450 further files were
skipped as unscoreable and 3,833 as generated. §B12.8 records where that number
went and, more usefully, how much of the gap was measurement rather than engine.

The most valuable output is not the score, it is the demand list: every API the
tests asked for and this engine does not have, counted. The top of it:

```
3944  Element.hasChildNodes        1197  getComputedStyle(margin-left)
2208  Element.sheet                1012  getComputedStyle(scale)
1571  document.styleSheets          972  Element.offsetTop
1501  Element.getContext            926  getComputedStyle(z-index)
1468  Element.setHTMLUnsafe         863  navigator.serviceWorker
```

`hasChildNodes` is one line and was asked for 3,944 times, more than twice
anything else. Nothing in four hand-picked corpora used it and everything in the
DOM test suite does. That is the case for a conformance suite in one sentence.

### B12.5 What was built, and what was deliberately not

**Typed reflection.** `dir` is an enumerated attribute whose IDL getter answers
"" for anything that is not one of its keywords, so `setAttribute("dir", "5%")`
reads back as "" in a browser and read back as "5%" here. WPT sets every
reflected attribute to sixty-odd hostile values and checks exactly that, which
is how an engine scores zero on an attribute it believed it had. There is now
one `reflect()` with a type per shape (string, nullable, bool, long, ulong,
enumerated, url), and `long` implements the spec's rules for parsing integers,
which are not `Number()`.

**Per-tag interfaces.** Sixty tags now carry their own class and the spec's
reflection table, because `colSpan` belongs to `<td>` and hanging it on every
element makes `"colSpan" in div` true, the same lie the removed `missingApi`
stubs told.

**Every computed longhand.** The note in `computed_style` claimed Stylo had no
generic accessor to bind against, so six properties were hand-listed and
everything else answered "". That was a wrong belief about the dependency, not a
considered scope: `computed_value_to_string` does exactly this. `color` came
back empty (§B11.5.11) and now returns `rgb(0, 0, 0)`.

**Not chased at the time: the legacy CJK encoding tests.** ~~They need legacy
encoder tables in the URL serialiser, wptserve variants, and `<iframe>`, which §B6
refuses outright: the clearest opportunity this suite offers to move a number
without improving the engine for anyone.~~

**That paragraph was wrong on all three counts, and §B12.10 is the correction.**
The struck text is kept because the shape of the error is worth more than the
conclusion was:

* **~17,000 was the wrong size.** Measured before the generated endpoints and
  before the timeout fix; the block is **220,367** unpassed subtests, the
  largest in WPT by a factor of two.
* **`<iframe>` is not required.** The `iframe { display:none }` in those files is
  dead boilerplate from a shared template. 162,892 of the subtests are `-href-`
  tests: build an `<a href>` in a euc-jp document and read `.href` back. No
  iframe, no form, no `.py` handler.
* **It is a real feature, not a scoring artefact.** This engine ignores
  `<meta charset>` outright: `document.characterSet` is `undefined`, and a
  euc-jp page's URLs are percent-encoded as UTF-8. An agent reading a legacy
  Japanese page gets the wrong answer today. "Without improving the engine for
  anyone" was simply false.

The error was reading a *sample* failure message and generalising from the file
name around it, rather than asking what the assertion needed. Three sentences of
confident scope-cutting, none of them checked.

### B12.6 Reading the number honestly

Three things move this score and only one of them is engineering:

1. **Implementing more.** On a fixed nine-directory sample, 5,345 → 6,876.
   html/dom alone, 3,223 → 6,035 across the two reflection commits.
2. **Measuring more.** Going from nine directories to all 223 took the total to
   33,754 without a line of engine code. This is legitimate, since Kitesurf's 215,000
   is across all of WPT too, but it is not improvement, and a report that
   blurred the two would be worth nothing.
3. **Counting more honestly**, which moves it *down* as often as up.

So the targets are worth restating in those terms, and the restatement below is
the *original* one, kept because it was wrong in an instructive way:

> **10,000 is passed**, and mostly by (2). **50,000** is reachable by (1), the
> demand list is mechanical work, and 8,536 files still report nothing at all.
> **100,000** is not reachable on this path. It needs the generated endpoints,
> which means serving what wptserve serves, and whole subsystems this engine
> does not have and mostly should not: canvas, service workers, XSLT.

All three targets were passed, and the last one was passed without any of the
subsystems that paragraph said it required. §B12.8 is why.

### B12.7 What is next

1. **Empty the `no_report` bucket.** 8,536 files report nothing; the causes are
   already grouped by the runner and the top few will cover most of them.
2. **CSSOM**: `Element.sheet`, `document.styleSheets`. 3,779 asks between them.
3. **The remaining computed values**: shorthands, custom properties, and the
   layout-dependent resolutions Stylo alone cannot know.
4. **A CI gate on regression**, not on an absolute number: the baseline is
   committed, and a change that drops it should have to say why.

### B12.8 Where the number actually was, 2026-08-10

> Superseded by §B13.2: the total is now 333,690. This section stays as written
> because what it says about *why* the number moved is unchanged, and because
> §B13.3 is the same lesson arriving a second time: a large number that comes
> from one place has to say so.

**117,331 subtests passing of 585,474 scored**, 26,052 files, 25,252 of which
report. That is up from 33,754, and it is worth being exact about how much of
that is the engine getting better and how much is this file learning to read.

Three changes account for most of it, and only one is an engine change.

**The engine was being killed while testharness drew a table.**
`html/dom/reflection-tabular.html` took 40.6 seconds and scored **zero**, because
the harness's process timeout fired first. Those forty seconds were not tests:
testharness renders one DOM row per subtest into `#log` when it finishes, and
that file has forty thousand of them. The tests themselves are about half a
second of DOM work.

`setup({ output: false })`, a documented harness setting and what the official
WPT runner uses, turns the rendering off. Results already came back through the
completion callback, so the table was pure overhead.

    reflection-tabular   40.6s → 1.98s
    html/dom, whole dir  minutes → 26s
    html/dom passing     6,234 → 43,429

The timeout had been raised to 120 seconds an hour earlier, on the reading that
this engine needed more wall clock than a JIT to run the same test. That reading
was true and irrelevant: a generous timeout was paying a harness cost rather than
removing it. **The first measurement said "this engine is too slow for these
tests"; the second said "these tests spend their time drawing a table nobody
reads".** Only one of those is about the engine, and tens of thousands of
subtests were written off on the strength of the wrong one.

**A computed style did not declare its properties.** `"color" in
getComputedStyle(el)` was false for every property: the object is a proxy with
only a `get` trap, and `in` asks `has`. WPT's `test_computed_value` asserts
exactly that on its first line and is *the* helper for CSS parsing tests, so
thousands of subtests failed before comparing a value. css-color went 1,213 →
4,509 without one line of colour code changing: Stylo already supported
`color-mix()`, `oklch()`, relative colours and `color()`, and this engine was
already serialising them correctly. The tests could not get far enough to look.

**Style was never recomputed on demand** (§B12.5's list), which is what the
CSSOM tests needed and what any page that builds its DOM in script needs.

The pattern across all three, and across §B8's history: **a large failure cluster
usually has one cheap structural cause, not N expensive ones.** Three for three
here. An hour of reading actual failure messages has repeatedly been worth more
than a week of implementing what the failure count seemed to ask for.

#### What this does and does not claim

It does not claim the engine is fast. A 40-second file becoming a 2-second file
is the harness no longer being measured; the engine is still an interpreter and
still around 1.3x Chromium's wall time on real pages (§B8.17). Conformance
measured with a fair harness and speed on real pages are separate claims and
should stay separate.

It does not claim parity with a browser. 453,864 subtests still fail, and the
largest blocks are named in §B12.5 and §B12.10: legacy document encodings (in
progress), the combinatorial half of `execCommand`, and the multi-origin
security suites that need wptserve's Python handlers.

It does claim that the number is honest. Nothing was counted that was not run,
`NOTRUN` and `TIMEOUT` are reported separately from `FAIL`, files that cannot be
scored are named rather than blamed on the engine, and every subtest counted here
was already passing before the harness stopped killing it.

### B12.9 The gate, and why it is not in CI

`wpt/gate.sh` runs five directories against a committed floor in
`wpt/baseline.json`. It is a **local** instrument, run before a change that
touches the engine's DOM or CSS surface, not a CI job, and the first attempt to
make it one is worth recording, because it failed for a reason that is not about
runtime.

A pass count is only a floor if the corpus is fixed. WPT is not: the CI runner
sparse-checked-out its own revision and scored `encoding` out of 142,445
subtests where this machine scored it out of 229,349. Both numbers were right
about different corpora. Comparing a count against a moving upstream measures
upstream, and would have failed builds that changed nothing.

Wall-clock made it worse rather than caused it: several of those directories
only score what they score because large files finish inside a timeout, so a
slower runner loses subtests without anything regressing.

So CI keeps the *behaviours* instead, hermetically. `src/script/tests.rs` has a
"what WPT found" block: the lifecycle firing, named globals, typed reflection,
per-tag properties, computed style declaring itself and recomputing on demand,
stylesheet rules that write back, `TextDecoder` validating its label, unhandled
rejections reported, and the two crashes a page could use to kill the engine.
Those are fixed things, they run in a second, and they fail only when the engine
changes.

The floor still exists for the case it was built for: this branch gave back
3,142 subtests in `html` to a settle-loop rewrite, and nothing caught it but a
manual diff. `wpt/gate.sh` is what to run before believing a refactor was free.

---

## B13. Legacy document encodings, and a number that needs a caveat

§B12.5 wrote the legacy CJK encoding tests off in three sentences. All three were
wrong, the correction is recorded in place there, and this section is what
happened when the work was actually done.

### B13.1 What was missing

This engine decoded every document as UTF-8. A page served as euc-jp came out as
replacement characters, `document.characterSet` did not exist, and a link's
query was percent-encoded from the wrong bytes. An agent reading a legacy
Japanese page got the wrong answer and was told nothing about it.

`src/encoding.rs` settles the two things a document's encoding decides.

**Which encoding.** BOM, then the transport's `Content-Type`, then the markup's
`<meta charset>` or `<meta http-equiv>`, then UTF-8. The prescan stops at 1024
bytes because the HTML standard's does, and that bound is load bearing rather
than an optimisation: a declaration further down cannot be honoured, because by
then a parser has committed. Agreeing with a browser about pages that declare
too late is the point.

**How a query is encoded.** The URL Standard encodes a query with the
*document's* encoding, and a code point that encoding cannot represent becomes
an HTML numeric character reference. `丂` in a euc-jp page is `%26%2319970%3B`,
where this engine answered `%E4%B8%82`, the right escape of the wrong bytes,
which is the shape of wrong answer that is hardest to notice.

That needed the per-character encoder rather than `encoding_rs::encode`. The
bulk call renders an unmappable code point as the literal `&#19970;`, and `&`,
`#` and `;` are not in the query percent-encode set, so they pass through and
the answer becomes `&%2319970;`. The URL Standard appends `%26%23`, the decimal
value and `%3B` (the reference *already* percent-encoded) precisely so a
generated reference cannot be mistaken for a real separator.

Also found on the way: local files were loaded with `read_to_string`, which
**refuses** a file that is not valid UTF-8, exactly the file this path most
needs to open.

### B13.2 The number, and why it is checked rather than quoted

**333,690 subtests passing of 584,707 scored**, from 117,331. 26,052 files run,
25,249 of which report.

That is a large enough jump in one commit to deserve disbelief, so it was
checked three ways before being written down.

* **Nothing is counted twice.** 25,247 distinct test files across the sweep,
  none run more than once. The largest single file reports 21,269 subtests under
  21,269 distinct names.
* **The answers are right.** The same page was run in Chromium 1140 and the
  output is byte-identical, including two cases where Python's own `euc_jp`
  codec *disagrees*: Python encodes U+4E02 into JIS X 0212, and WHATWG's euc-jp
  decodes that plane but never encodes to it. Matching the browser rather than
  the naive codec is not something reached by accident.
* **The engine is doing it, not the harness.** `document.characterSet` reports
  EUC-JP, the text decodes as itself, and an unmappable code point becomes a
  numeric reference, each asserted in `src/script/tests.rs`.

### B13.3 The caveat that has to travel with it

**70% of every passing subtest comes from twenty files.** The top eleven are all
`*-encode-href-*.html`: one behaviour (encode a character into a URL query)
repeated once per codepoint across the CJK range, for five encodings.

| framing | subtests |
| --- | --- |
| Headline total | 333,690 |
| **Excluding the encoding directory** | **107,904** |
| Excluding just the CJK block | 116,428 |
| From the top twenty files alone | 235,977 |

Files that pass *completely*: **1,882** of the 20,506 with any scored subtest.

So 333,690 is true and describes one feature with an enormous test count rather
than broad platform coverage. Both halves of that sentence have to be said
together, and §B12.6's rule applies unchanged: implementing more, measuring more
and counting more honestly are three different things, and only the first is
engineering.

**The Kitesurf comparison: withdrawn, 2026-08-19.** This section previously
worked an arithmetic argument to the conclusion that Kitesurf's stated
"215,000+ tests passing" could not include the CJK block, and therefore that a
like-for-like reading put this engine at about half their breadth. The argument
was wrong, and it is worth keeping the wreckage because the mistake is a tidy
example of the thing this file keeps warning about.

It ran: the CJK `encode-href` block is 217,263 subtests, which is larger than
Kitesurf's whole stated total, so they cannot be passing it. That inference
treats a block as **pass-all-or-none**. Nothing requires an engine to pass every
subtest in a directory, and partial coverage is the normal case for all of them,
including this one. An engine passing 150,000 of that block has a total entirely
consistent with 215,000 and a number that includes CJK encoding. The premise
does not support the conclusion, and the like-for-like table built on it does
not stand.

What actually follows is narrower and less satisfying: **the two numbers are not
comparable in either direction.** Two reasons, and either is sufficient. Their
harness is not this one, and this one cannot reach workers, `.py` handlers or
TLS, so it scores 584,707 subtests where a full wptserve run reaches roughly two
million: the denominators are different corpora. And the composition of their
number is not published, so subtracting our CJK block while leaving theirs in
place would be a comparison rigged in our own favour, which is the same error in
the other direction.

So there is no defensible comparison here, and this file should not have printed
one. **The claim that survives is entirely about this engine and needs no
competitor at all:** 333,690 is true, 65% of it is one block, and both halves
have to be said together.

The failure mode is §B12.8's, arriving in a new place. That entry recorded that
a large failure cluster usually has one cheap structural cause, and that an hour
of reading actual failure messages beats a week of implementing what a count
seemed to ask for. This is the same lesson pointed at a *comparison*: an
arithmetic argument that felt conclusive was doing the work that reading the
other engine's published methodology should have done. Comparative claims about
someone else's number need their methodology, not our calculator.

### B13.4 What this is worth, plainly

The engineering is worth having on its own: a legacy page now reads correctly,
which is a real capability an agent needs and did not have. The score is a
consequence, not the reason, and the twenty-file concentration is the reason to
say so out loud rather than let a headline imply 333,690 distinct capabilities.

---

## B14. Reviewing the engine against a real browser, 2026-08-10

The reviews in §B8 and §B11 read code and reasoned about specs. This one diffed
behaviour against Chromium 1140, and the difference in yield is the finding
worth keeping: **thirteen bugs in an afternoon, four of them in code whose
comments explicitly argued for the wrong answer.** Reasoning about what an
engine should do had been checking the reasoning, not the engine.

The method is a page of one-assertion-per-line probes, run in both engines and
compared. `wpt/` finds gaps against a specification; this finds disagreements
with the thing the user will actually compare against.

### B14.1 The encoding work, three days old and already wrong

* **Existing percent-escapes were destroyed.** The query was decoded after
  parsing and re-encoded, which cannot work: once `url::Url` has run, an
  author's `%41` and a `%E4%B8%82` the parser made from a raw `丂` are both just
  `%XX`. `?x=%41` became `?x=A`; `?100%25` became `?100%`, an escape the page
  wrote turned into an invalid one. Now encoded from the raw text before any
  parser touches it.
* **An undeclared legacy page was destroyed**: the worst of the three, because
  it is the document the module exists to rescue. Undeclared bytes fell back to
  UTF-8, so a windows-1252 page had every high byte replaced by U+FFFD:
  `café naïve` read as `caf<?> na<?>ve`. The fallback is now asymmetric on
  purpose, and the asymmetry is the point: windows-1252 read as UTF-8 loses the
  text outright, while UTF-8 read as windows-1252 is mojibake but lossless.
  Given a guess must be made, take the recoverable wrong answer.
* An empty query reported `"?"` where a browser reports `""`.

### B14.2 A code block is not one long line

Every `<pre>` arrived as a single run-on line, which for an engine that reads
documentation is a poor reading of the thing it reads most.

The fix is not to stop collapsing. `Snapshot::render`'s fence rests on **no
page-derived value spanning a line**, because a value that can start a line can
forge the closing marker. So a `<pre>` is split on its own breaks and each piece
becomes an outline line, collapsed individually with its own indent and `- `.
The invariant is untouched and the structure survives, verified by putting the
literal closing marker inside a `<pre>` and watching it come back as
`[fence marker removed]`.

### B14.3 Nine more, from sixty-four assertions

`{ once: true }` was read at registration and never consulted at dispatch, so
listeners fired every time, and the same handler registered twice made two
listeners where a browser makes one. An invalid selector answered `null` instead
of throwing, which is indistinguishable from "no such element", so a page with a
typo took its not-found branch and never learned why. `textContent = null` wrote
the four characters `null`. `tabIndex` answered -1 for links and buttons, telling
a page nothing was focusable. `isEqualNode`, `normalize` and `isSameNode` were
absent and `compareDocumentPosition` called connected nodes disconnected. Style
serialisation lost its trailing semicolon, and emptying a declaration removed
the attribute instead of leaving `""`.

63 of 64 cases now match Chromium exactly.

### B14.4 The one that is not a bug: `Intl`

`Intl` is undefined, so `toLocaleString()` answers `1234.5` where a browser
answers `1,234.5`, and `toLocaleDateString()` returns a full date string rather
than `12/31/1969`. A page that formats numbers or dates for display shows
different text to this engine than to a person.

Enabling boa's `intl_bundled` **does not build**: it wants `icu_provider 2.2`,
which conflicts with what parley already pins through blitz. That is the same
disjoint-ICU wall that dictated the boa revision pin in the first place (§B12.2),
arriving from the other side. It is recorded here rather than filed as a task,
because nothing in this repository can move it: it needs the two ICU lines
upstream to converge.

### B14.5 What this says about how to look

Three review passes on this engine have now found bugs at very different rates.
Reading code found some. Running a conformance suite found more, and found the
*instrument's* faults as a side effect. Diffing against a browser found the most
per hour, and, the part worth internalising, **it found bugs in code whose own
comments had reasoned carefully to the wrong conclusion.** A comment cannot
falsify itself. Another implementation can.

---

## B15. Two more reference engines, and the bug reading them found, 2026-08-19

§B11 read Kitesurf against a built engine and asked what the comparison changed
about the *order* of work. This section does the same for two engines that are
closer to us in purpose than Kitesurf is: **Lightpanda** (`~/Ref/browser`, Zig,
V8 plus html5ever, CDP and MCP) and **Obscura** (`~/Ref/obscura`, Rust, V8 via
deno_core, CDP and MCP, ~132k lines across nine crates). Both describe
themselves as headless browsers *for AI agents*. Neither has receipts, a policy
layer, or a box; both have an agent-driving surface several times the size of
ours.

The comparison is therefore lopsided in a useful way. It says almost nothing
about the engine and a great deal about **the verbs on top of it**, which is
where the honest reading is that we are behind.

### B15.1 What the reading found first: a ref resolves against a page the agent never saw

Before any of the design comparison, the read found a defect in our own control
channel, and it is the kind §B8.3 singles out as the worst state for anything
here to be in: a plausible wrong answer that looks like a right one.

`type`, `submit` and `click` each take a **fresh** snapshot at action time and
resolve the agent's `@ref` against that (`stream.rs:863`, `:885`, `:917`):

```rust
let snapshot = session.page.snapshot();
let Some(entry) = snapshot.resolve(reference) else { ... };
```

References are minted by walk order (`snapshot.rs:590`):

```rust
let id = format!("e{}", self.next_ref);
self.next_ref += 1;
```

So `e5` does not name an element. It names **the fifth actionable thing in this
walk**. The agent read snapshot *N* and is acting against snapshot *N+1*, taken
now. If anything moved in between — a settle that ran, a script mutation, an
element inserted earlier in document order — `e5` resolves to a *different
element*, `click` succeeds, and the reply says `{"ok": true, "ref": "e5"}`.
Nothing anywhere detects it.

There is no memory-safety problem: the node id is freshly minted, so the click
lands on a real node. That is precisely what makes it bad. The failure is
silent, it is indistinguishable from success, and the engine's whole claim is
that it does not hand an agent a plausible lie.

**The minimum fix** is a generation counter: stamp each snapshot, return it,
require it back on any verb taking a `@ref`, and refuse a mismatch by name
rather than acting on it. That converts a silent wrong action into a loud one
and is a small change.

**The right fix** is two handle types, which is §B15.4.

This is also a comment on method. §B14.5 ranked three ways of looking and put
"diff against another implementation" first, because a comment cannot falsify
itself. Reading two *other* agent-facing APIs found this in an afternoon, and
neither the corpus (§B8) nor WPT (§B12) would ever have found it: the page is
conformant, the render is right, and the wrong element is clicked.

### B15.2 The two engines, and what is not comparable

Stated first, so nothing below is quoted against the wrong baseline.

* **Neither is a fair speed or conformance comparison.** Both run V8. Obscura
  ships a 14.5k-line `bootstrap.js` baked into a V8 startup snapshot; Lightpanda
  hand-writes its DOM in Zig against V8 directly. We run Boa, an interpreter,
  for the reason §B11.1 gives. None of the three numbers bounds another.
* **Neither has our reach.** §B11.3 named this as an advantage never written
  down; it survives contact with two more engines. Obscura's SSRF gate denies
  loopback and RFC1918 *by default* (`client.rs:573`, installed as reqwest's own
  DNS resolver), which is the correct default for a scraper and the exact
  opposite of what a coding agent needs. Ours allows loopback by default because
  loopback is the dev server (`--no-loopback` takes it away).
* **Neither has receipts**, and the shape of what they do have is instructive.
  Obscura's CDP `Network.*` events are **batched and emitted after navigation
  completes**, reconstructed from a stored list; anything watching requests live
  sees a compressed, out-of-time picture. That is the failure mode §7.1
  predicted for any engine observing its own network from beside it rather than
  being it.

What *is* comparable is the verb surface: **8 session verbs here, 27 in
Lightpanda, 36 in Obscura.** That gap is not padding. It is the difference
between an agent that finishes a task and one that stalls and re-snapshots.

### B15.3 One verb table, and why it is a security change

Our verb set is written out three times and nothing makes the three agree: the
clap `SessionVerb` enum (`main.rs:239`), a hand-built JSON payload in
`session()` (`main.rs:~470`), and a string `match verb` (`stream.rs:715`).

Lightpanda's answer is the single best structural idea in either codebase. One
exhaustive `Tool` enum (`tools.zig:229`), and every per-tool property is an
**exhaustive switch** on the tag: `isRecorded`, `isAsync`, `needsLocator`,
`producesData`, `waitsForReadiness`, `navigatesToUrl` (`tools.zig:261-330`).
Adding a tool is a compile error until every consumer has made an explicit
choice. Four front-ends read that one table: MCP, LLM tool-calling, a slash
command REPL, and script replay. In Rust this is free.

Here it is not only tidiness. **LOGIN mode's refusal is a string allowlist**
(`stream.rs:711`):

```rust
if session.login && !matches!(verb, "status" | "login") {
```

The default is refusal, so the failure direction is safe, and a *new* verb is
refused until someone thinks about it. But the allowlist itself is two string
literals: one typo opens a read path during credential entry, and no test that
does not already know the typo will catch it. As a predicate on the enum
(`fn readable_during_login(self) -> bool`) it cannot be typoed, and the
exhaustive match forces every future verb to answer the question.

Do this first. Everything after it is cheaper once it exists, and §B15.10's MCP
decision stops being expensive to reverse.

### B15.4 Two handle types, because one of them has to be recordable

Both engines mint durable handles; ours are ordinals (§B15.1).

**Lightpanda has both kinds, deliberately.** `backendNodeId` is a registry keyed
on **DOM node pointer identity** (`cdp/Node.zig:38`), so an id survives arbitrary
mutation and resolves to the same element or to nothing. On navigation the whole
registry is reset, because every pointer in it dangles. That is the cheap
intra-page handle.

The durable one is `SelectorPath` (`browser/SelectorPath.zig:53`): the *simplest
CSS selector whose first match is the target*, built greedily from the target
outward, prepending an ancestor segment only when it shrinks the match count,
preferring `#id` then `[data-testid]`/`[name]` then a `:has()` distinguisher
found by BFS, and only then falling back to `:nth-of-type`. Each candidate is
verified with **the same query function `click` and `fill` use**, so the
selector is correct by the same resolution rule that will later resolve it.

Obscura's approach is the one to refuse: it writes `data-obscura-ref="e3"` into
the DOM (`obscura-mcp/src/lib.rs:1217`) and resolves via an attribute selector.
Cheap, and wrong for us — a receipts engine that mutates the page has a snapshot
that no longer describes the page as served.

Why two kinds rather than the better one: **the durable handle is what makes a
session replayable**, which is §B15.9. Lightpanda shapes its API around this and
says so to the model, in the guidance it ships with the protocol: *"NEVER pass
backendNodeId to click/fill/hover/selectOption/setChecked … backendNodeId calls
cannot be recorded as reusable JavaScript, so any session that uses them is not
replayable."* The recordability constraint is made visible in the API rather
than discovered at save time.

### B15.5 Waiting: the primitive we already have is the better one, and it is not exposed

We have **no `wait_for` at all**. On a script page an agent's only option is
snapshot-and-hope.

Both engines converged on the same default from opposite directions, and it is
worth recording because it is counter-intuitive: **do not wait for network idle
by default.** Lightpanda waits for `load` and says why — *"on real sites
trackers/timers keep the network from ever fully idling, so it just rides the
timeout"* (`tools.zig:1972`). Obscura had to drop its CDP default all the way to
`domcontentloaded` because full-load pushed github.com and reddit.com past the
25s mark while clients timed out at 15s (`domains/page.rs:~1030`). Idle is an
explicit escalation in both.

Our `Settled { elapsed_ms, timers_run, cut_off, pending_timers }` on a **virtual
clock** is a better primitive than either, and this file has never said so.
Theirs are wall-clock heuristics with hardcoded fudge. Obscura's adaptive settle
(`obscura-js/src/runtime.rs:1989`) is the most sophisticated version and carries
a 150ms quiet window, a 1000ms external-work grace, a 500ms observable-activity
tail, a 5000ms synchronous-task floor, and a hardcoded 5s idle deadline that
**marks the page `NetworkIdle` even when the deadline is what ended the loop**
(`page.rs:2691`). Ours is deterministic, reproducible across runs, and costs a
page's `setTimeout(1000)` nothing.

It is also *more complete than theirs on the axis their heuristics exist to
approximate*: our fetch is synchronous underneath, so there is no in-flight
request for a settle to miss. The thing they are estimating, we know.

What to build: `wait_for {selector | text, timeout}` and `wait_for_script
{expr}`, driven by the existing settle loop, plus one borrowed rule from
Lightpanda's wait predicate (`Runner.zig:287`) — **resolve when there is nothing
left to wait *on*, even if the requested milestone never arrived**, rather than
spinning to the timeout. And keep reporting, never guessing: a wait that ended
because the page went quiet without the condition holding is a different answer
from one that timed out, and both are different from success.

### B15.6 Errors that name the recovery

A refusal here is `{"ok": false, "error": "<prose>"}`. Both engines converged on
named codes plus a recovery sentence, addressed to the reader that is actually
there. Lightpanda's (`tools.zig:762`):

```
NodeNotFound: the selector or backendNodeId matched nothing on the current page.
Re-inspect the page (tree/interactiveElements) for fresh node ids, or omit
backendNodeId to target the document root.
FrameNotLoaded: no page is loaded — call goto (or pass a url) first.
```

Three things to take:

1. **A `code` field** beside the prose, so a caller branches without parsing.
   Obscura is the counter-example: every handler error becomes CDP `-32601`
   regardless of meaning (`dispatch.rs:539`), and every page failure collapses
   into `PageError::NetworkError(String)` — timeouts, DNS, SSRF blocks and
   robots.txt denials are one variant. An agent cannot branch on that.
2. **In-band versus protocol failures.** A selector that matched nothing in an
   `extract` is content the model should read and fix; a policy refusal is a
   protocol error. Lightpanda splits exactly there (`mcp/tools.zig`), and
   returning the first as an error kills the self-correction loop.
3. **Pre-parse diagnostics** that name the offending field and list the valid
   values (`diagnoseArgs`, `tools.zig:2060`): `state: "fast"` should produce
   *"invalid state 'fast'. Expected one of: load, domcontentloaded, …"* rather
   than a raw parse failure.

One small accommodation worth copying verbatim: Lightpanda treats
`backendNodeId: 0` as omitted, because zero-filling models send `0` for unset
(`tools.zig:2098`).

### B15.7 The verbs that are missing, and the one nobody else can have

Ranked by how often an agent loop stalls without them: `select_option`, `press`
(a key), `set_checked`, `back` / `forward` / `reload`, `get_attribute`, `count`,
`find_element {role, name}`, `links`, `console`.

And then **`requests`**, which is ours alone. Exposing the request log through
the control channel is a verb no other engine can offer honestly, because no
other engine *is* the HTTP client. Lightpanda has no equivalent. Obscura's is
the batched, after-the-fact reconstruction of §B15.2. Ours is the decision
record that was written before the bytes moved, and the agent driving the page
should be able to read it without leaving the session.

This is the same argument §12 made for the engine existing at all, arriving at
the verb layer. It also closes a gap in the agent's own loop: today an agent
that wants to know whether its click caused a request has to be running with
`--script` and read the `requests` field of the click reply, or go find the
receipts file.

### B15.8 Extraction, and a markdown view

Both engines have a selector-to-JSON extraction DSL and both have markdown.
Ours has neither, and the token economics of an agent loop say both matter.

Lightpanda's `extract` is the better design (`tools.zig:917`): field name to
selector, `[...]` for all matches, `{"selector":…, "attr":…}` for an attribute
with `href`/`src` resolved absolute, `[{selector, fields:{…}}]` for one object
per match with relative sub-selectors, and `limit`. One rule is worth copying
exactly: an empty array is a valid result, but **if every top-level key comes
back null it throws**, in-band, with

```
extract: no schema selector matched any element — inspect the page with
tree/markdown and retry with corrected selectors
```

An unmatched schema is a mistake the model should be told about; an empty result
set is not.

Markdown is a denser read than the a11y outline for the "read the untrusted web"
case that is this engine's stated purpose, and it is cheap over the Blitz DOM.
Note the two gaps in Obscura's converter (`obscura-js/src/markdown.rs:7`) so we
do not reproduce them: no GFM header separator row is ever emitted, so its
tables are not valid markdown, and ordered-list items all render as literal
`1. `. Whatever we emit, the fence of §12.1 applies to it unchanged.

### B15.9 Credentials by indirection, which is the answer LOGIN mode is not

Lightpanda's `$LP_*` scheme is the strongest single idea in either engine for
our threat model, and it is better than what we have.

End to end: only the `LP_` namespace is readable (`tools.zig:2166`); `getEnv`
with no argument returns **the names, never the values**; substitution happens
*inside the browser process* so the secret never enters model context; `fill`
echoes the **placeholder** back in its result rather than the value
(`tools.zig:626`); and the recorder reverse-substitutes on every append,
iterating by value length descending (`tools.zig:2221`) so a short secret that
is a substring of a longer one cannot leak a suffix. There is a test asserting
a prompt-injected `fill('$SECRET')` cannot exfiltrate a non-`LP_` variable.

This matters more here than there, because **it has no hole and LOGIN mode
does.** §12's LOGIN mode is honest about being half built: it refuses the
documented read path but does not withhold frames, and the README says plainly
that an agent that goes looking can attach to the viewer socket and watch the
same pixels. There is no moment in the indirection scheme when the secret is on
screen, so there is nothing to watch.

It also fits the rule the cookie jar already follows. `session status` reports a
cookie *count* and never a value; the request log records how many cookies
crossed and never which. A credential used by name and never by value is the
same rule at the input side, and the receipt can say `used $H5I_ACME_PASSWORD`
without that being a credential in every export the receipt reaches.

The two compose rather than compete. LOGIN mode stays for interactive OAuth this
engine cannot drive at all (and §B11.6's iframe/popup conflict is still
unsettled and still has to be decided in writing). `$H5I_*` covers form posts,
which is most of what an agent meets.

### B15.10 The moat: a recording that replays deterministically

Lightpanda records every state-mutating verb into replayable JavaScript
(`script/Recorder.zig`), with three mechanisms worth taking: an emit-once
preamble; a one-step rewrite window that downgrades a preceding `goto` to
`domcontentloaded` when the next command supersedes the wait; and secret
scrubbing on every append. Recording is *filtered* — a verb that used an
ephemeral handle is dropped, so an unreplayable session simply produces a
shorter script rather than a broken one.

We already write `$H5I_BROWSER_ACTIONS`, and §12.1 already makes the guarantee
that each verb is recorded before it runs and again after. Making that log
**replayable** is a small step from where it stands, and it buys something
neither engine can have:

**Our settle runs on a virtual clock, so a replay is deterministic.** Both of
theirs are wall-clock, so a replay is a re-run with different timing and a
different answer. A recorded run, plus the request log it produced, plus a
replay that lands identically, is a browser session that can be **re-executed
and diffed**. That is the browser-side form of what §B11.5.16 wants from
receipts — an artifact checkable by someone who does not trust the binary that
wrote it — and it is a stronger position than any benchmark table, for the
reason at the top of this file.

It depends on §B15.4. A recording made of ordinals replays into a different
page; a recording made of verified selectors replays into the same one.

### B15.11 Two decisions to make deliberately, not discover

**MCP.** §B11.4 decided against it: the agent runs in the same box, and
`h5i-browser-light session snapshot` is already a tool it can call, so a
protocol server would wrap the CLI in a socket for a caller that can already
call the CLI. That argument is unchanged and still correct **for h5i's own
boxed agent**. What the comparison adds is that both of these engines ship MCP
as their *primary* agent surface, so it is also how anything outside h5i would
ever drive this engine. The recommendation is to keep the decision and note
that §B15.3 defuses it: with one verb table carrying schemas, an MCP server is
a few hundred lines over it. The reopening condition at §B11.4 stands as
written.

**CDP.** §B11.5.4 ranks a subset third, and Obscura is a detailed and
discouraging cost estimate. The protocol is the small part; the compatibility
is the work. Distinct session ids per attach because a target can carry two
client sessions (`dispatch.rs:239`); `canAccessOpener` in every `TargetInfo` or
chromiumoxide panics; rewriting the main document's `requestId` to the
`loaderId` because Puppeteer identifies the navigation response that way, *and*
aliasing the stored body so `getResponseBody(loaderId)` resolves; a required
event ordering with `requestWillBeSent` before `frameNavigated`; execution
context ids cleared and reseeded per navigation, with an invented
`__puppeteer_utility_world__` if the client registered none. Each of those is a
client bug worked around, not a protocol feature.

And the failure mode is exactly the one §B11.5.5 predicted, in the shipped code:
`DOM.setAttributeValue` and `DOM.removeNode` are **silent no-ops**
(`domains/dom.rs:222`), and `DOMSnapshot.captureSnapshot` returns **synthetic
geometry** — every node a 1280x18 box stacked vertically (`domsnapshot.rs:232`)
— in a build where real layout exists a call away. An agent framework that
trusts those bounds gets garbage and is told nothing. That is the `missingApi`
lie at protocol scale, and it is what a partial CDP costs when the conformance
list is not published first.

Recommendation: CDP moves *behind* the agent-loop work in §B15.12, and if it is
built, the conformance list ships before the endpoint does.

### B15.12 What not to copy

**Obscura's stealth stack**, in full. Half of `bootstrap.js`'s fingerprint layer
is a seeded PRNG producing plausible-but-false GPU strings, canvas noise,
battery levels and heap sizes, plus a `Function.prototype.toString` patch that
masks itself and a `getOwnPropertyNames` filter that hides its own globals. It
is competent and it is the exact inverse of this engine's thesis: every one of
those is a plausible lie a page cannot detect, engineered so it cannot be
detected. We are not evading anyone, and a receipts engine that spoofs its own
identity has given up the argument.

The **catalogue** is worth keeping in one direction only. It is a list of what
pages actually probe for, and a page reading `WEBGL_debug_renderer_info` or
enumerating `navigator.plugins` is telling us something about itself. That
belongs in `unsupported()` as a routing signal (§B8.4), which is machinery we
already have.

Also not to copy: **`DOM.setAttributeValue` as a no-op** and **synthetic
DOMSnapshot geometry** (§B15.11); **writing refs into the DOM** (§B15.4);
**batched network events** (§B15.2); and Obscura's documented-but-absent
`localStorage` persistence, where dropping the JS isolate on every navigation
means web storage does not survive a same-origin navigation, let alone a
restart, while `docs/Persist-cookies-and-storage.md` promises a file. §B6 already
commits us to in-memory storage; the lesson is that the *documentation* has to
say so.

### B15.12a The performance items, measured: all three answers are no

§B11.5.13 and §B11.5.14 list two performance items — reuse the realm across
navigations (~20ms a page, §B8.9), and cache the prelude's bytecode (three
thousand lines of JavaScript parsed per realm). Both were attempted. Neither
should be built, and a third optimisation that looked obvious was measured and
reverted. Recorded together because the pattern is the point.

**Realm reuse: refused, on grounds §B11.5 did not weigh.** A realm carries
everything the previous document's script put in it — globals, patched
prototypes, retained closures. Reusing one across a navigation means a page can
set attacker-controlled state, cause a navigation, and have that state visible
to the document it navigated to. That is a boundary this engine would be
removing to save twenty milliseconds. Obscura, a far larger engine in the same
space, drops and recreates its entire JS runtime on every navigation for exactly
this reason, and says so in the code. The note now lives on `Page::run_scripts`
so the item is refused in review rather than re-attempted.

**Prelude bytecode caching: not buildable with this Boa**, for a checkable
reason rather than a hard one. `boa_engine::Script::parse` interns identifiers
into *the context's own* interner (`context.interner_mut()`) and binds the
result to that context's realm; every page builds a fresh `Context`. A parsed
script is not a portable artifact, so there is nothing to cache across pages.
Revisit if Boa grows a shared interner or a serialisable code block. The note
lives at the prelude's eval site.

**And the one that looked free was measured and was not there.** The settle loop
made *five* separate `context.eval` calls per round, three of them on the hot
path and one building its source with `format!`. Combining the three into a
single prelude hook is obviously less work, so it was built — and then measured
against the corpus, three runs each way:

    before   9.87s  9.62s  9.66s
    after    9.86s  9.82s  9.93s

No gain, inside noise, possibly worse. Parsing a twenty-character string is not
what a page load costs, and the change added a packed-integer protocol between
Rust and JS for nothing. Reverted.

The lesson is §B8's own, arriving from the other direction: **the rule against
building what no page asked for applies to performance too.** All three of these
were reasoned from the shape of the code rather than from a measurement, and all
three were wrong — two dangerous, one merely useless. The ceiling on this whole
area is small anyway: the corpus runs 35 pages in 9.7s, so a realm at 20ms is
about 7% of the total even if it were free.

### B15.13 The queue: built, 2026-08-19

All nine landed, plus `h5i box watch` and the console work of M11c. What follows
is the queue as written, with what each turned into. Three of them produced a
different answer than the one they were specified with, and those are the
entries worth reading.

**Item 1 was a live defect, and the fix is narrower than it looks.** A ref is now
honoured only against the reading it was served in. The check is an equality
test on one ref, not a proof the document is unchanged, and the code says so:
it catches every case where the *handle* has come to mean something else, which
is the failure that was silent, and claims nothing more. Typing and scrolling
renumber nothing, so the login loop still runs without a re-read between steps.

**Item 3 turned out to be a different feature than specified.** Because the
settle runs on a virtual clock *and* runs to quiescence, a page's own
`setTimeout(1000)` has already fired by the time any verb is served. `wait_for`
therefore does not usually wait — it **answers**, with three outcomes rather
than two: found; not found and the page has nothing left to run, so waiting
cannot change it; not found and the page was still working. The middle one is
the one worth having, and collapsing it into "timed out" would be the same lie
this file refuses elsewhere.

**Item 8's cost was the receipt schema, not the transport.** A socket carrying
four hundred messages could have been honoured by receipting the handshake
alone, and this engine's central claim would then have quietly stopped covering
the bytes after it. Every frame is receipted, written as an ordinary
request/response pair with `WS-SEND`/`WS-RECV` as the method — so the console,
`box watch` and the export bundle all show socket traffic with **no changes to
any of them**. `wss://` and remote `ws://` behind a proxy are refused by name;
SSE reconnection is refused because an engine that silently re-dialled would be
making requests the agent never asked for.

**Item 9 produced three negative results**, recorded in §B15.12a: realm reuse
refused on security grounds the queue had not weighed, prelude caching not
buildable with this Boa for a checkable reason, and an obvious-looking loop
optimisation measured and reverted.

Two things were found on the way that were nobody's item. A **password field's
value was read straight back out by `snapshot`**, so a credential typed by a
human during LOGIN mode was readable by the agent the moment that mode ended —
the mode's whole purpose, defeated one verb later. And the console showed
page-derived text to a person with no fence around it, while fencing the same
text for the model.

Still open, and unchanged: §B11.5.1 (a corpus that needs a login) is now the
oldest thing on this list, and §B15.9's credential work changes what it would be
testing. §B15.10's replay is the natural next build, and item 5's durable
selector was the dependency it was waiting on.

#### The queue as written

Ordered by leverage, not size. Items 1 and 2 make everything after them cheaper.

1. **Snapshot generation counter, and refuse a stale `@ref`.** §B15.1. Small,
   and it converts a silent wrong action into a named refusal.
2. **One verb table with predicates**, replacing the three hand-kept copies, and
   LOGIN mode's allowlist becomes one of the predicates. §B15.3.
3. **`wait_for` / `wait_for_script`**, over the settle loop, with "resolve when
   nothing is left to wait on" and a reported reason. §B15.5.
4. **An error taxonomy**: a `code` field, a recovery sentence, in-band versus
   protocol split, pre-parse diagnostics. §B15.6.
5. **A durable handle** (`SelectorPath`-style, verified with the same query
   function the actions use) reported beside the ordinal ref. §B15.4.
6. **The missing verbs**, `requests` first because it is the one that is ours.
   §B15.7.
7. **`extract` and a markdown view.** §B15.8.
8. **`$H5I_*` credential indirection**, and the receipt line that names a
   credential without carrying it. §B15.9.
9. **Replay**: the action log becomes a script, and a replay is diffed against
   the request log it reproduces. §B15.10. Depends on 5.

Items 10 and beyond are §B11.5's existing queue, minus its two performance
entries, which §B15.12a closes as refused and unbuildable respectively. Nothing here
displaces §B11.5.1 (a corpus that needs a login), which remains the
least-verified thing this file claims — and §B15.9 changes what that corpus is
testing, so it should be built after item 8 rather than before it.

---

## B16. Lightpanda below the verb line, 2026-08-26

§B15 read Lightpanda's agent surface — the tool table, the recorder, the
selector path, the credential indirection — and its queue is built. This read
is the other half: the engine underneath. The load pipeline, the settle loop,
the network stack, the memory strategy and the protocol servers, read
systematically against our own with both catalogued at the same depth.

Facts to pin first, because they change what the comparison is allowed to
claim:

* **Lightpanda has no layout engine.** What it has is a deliberate fake:
  elements are 5×5 boxes, a node's `y` is its document-order index times five
  pixels, `<body>` is 1920 × 100,000,000, and the comments on
  `contentWidth`/`contentHeight` (`Element.zig:1533`) are candid that the two
  are mutually contradictory on purpose — each axis independently assumes the
  arrangement that *produces* overflow, because under-reporting overflow is
  what wedges measure-then-mutate loops. `Page.captureScreenshot` returns an
  **embedded static PNG** (`cdp/domains/page.zig:23`), and `printToPDF` an
  embedded PDF. That is §B15.11's `missingApi` lie at protocol scale, shipped,
  in the second of the two engines we have now read. Among the three of us,
  real pixels are ours alone.
* **Its settle runs on the wall clock.** A page's `setTimeout(1000)` costs a
  Lightpanda caller a real second; two runs of one page can differ. §B15.10's
  replay-and-diff position rests on our virtual clock and nothing in this read
  weakens it.
* **Its network stack is libcurl** — the multi interface, nghttp2, BoringSSL,
  brotli — with a browser-shaped policy layer on top. It did not write an HTTP
  client, which is the correct decision for its goals and unavailable for
  ours: our client *is* the receipt mechanism.

So the engine-level comparison is lopsided in the opposite direction from
§B15's: there the verbs were behind and the engine was fine; here the verbs
are settled and what the reading found is in the load path. Mostly in ours.

### B16.1 Three costs in our own load path, found by contrast

The method note of §B15.1 repeats: reading another implementation found in an
afternoon what neither the corpus nor WPT would ever surface, because a slow
page is conformant and renders correctly.

**1. We negotiate no compression.** reqwest is built with
`default-features = false, features = ["blocking", "rustls-tls"]`
(`Cargo.toml`), so the `gzip`/`brotli` features are off, and no code path sets
`Accept-Encoding` — the string does not occur in `src/`. Every document,
stylesheet and bundle this engine has ever fetched arrived identity-encoded,
commonly three to five times its compressed size. Lightpanda ships brotli,
gzip and deflate through curl and thinks about it never. Nothing in our design
argues for this; it is not a trade, it is an omission the receipts question
never noticed because a receipt records that bytes moved, not that three times
too many did.

**2. Subresources are fetched one at a time, on the parse thread, over
HTTP/1.1.** `BrokerNet::fetch` (`net.rs:640`) is the whole adapter: Blitz asks
for a resource, the broker blocks on the wire, the handler completes before
returning. N subresources are N sequential round trips, and with the `http2`
feature absent there is no multiplexing to soften it. The Cargo comment states
this as a chosen shape — "a browser that fetches one subresource at a time is
a browser whose receipt order is its request order" — and §B16.2 argues that
sentence defends the claim at the wrong place.

**3. Fonts are re-read from disk on every navigation.** `PageFactory::fonts()`
(`engine.rs:1325`) calls `fonts::load` fresh, which `fs::read`s each candidate
file and builds a new parley `Collection`, and it is called from all four page
construction paths (`engine.rs:1362`, `:1433`, `:1451`, `:1461`) — up to the
24-font budget of files per page load, for a font set that cannot change
between navigations of one session. Unlike item 2 this has no comment arguing
for it. It is bug-shaped: `FontSetup` is not shared because nothing made it
shareable.

Per §B15.12a's own lesson, none of these carries a promised number. Each entry
in §B16.10 names the measurement that gates it; the corpus instrument (§B8)
measures pages end to end and is the right harness, run against the network
corpus rather than local fixtures, since two of the three are network effects.

### B16.2 The preload scanner, and what "serial" actually protects

Lightpanda buffers the whole document before parsing — the same
buffer-then-parse shape we have, so no gap either way there — and then runs a
**preload scanner** first: a tokenizer-only pass over the complete HTML
(`src/html5ever/prescan.rs`, ~200 lines, modelled on Servo's
`dom/servoparser/prefetch.rs`) that reports every `<script src>`, module
preload and the first `<base href>`, so their transfers start before the tree
builder reaches them. The comment beside it names the failure it removes:
without this, N large blocking scripts download serially.

That is exactly our shape, minus the fix. And the receipt argument for keeping
it does not hold at the layer it is made. The engine's claim is **no receipt,
no request**: the decision record is written before any bytes move. That is a
claim about *ordering of decision and dispatch per request*, not about
requests being in flight one at a time. A prescan pass that walks the
document's resource list, policy-checks each URL, writes each receipt, and
only then lets transfers overlap, preserves the claim exactly — the receipt
log becomes the decision order, which it already is. Redirects stay per-hop
policy-checked per transfer, unchanged. What changes is only that transfer N+1
no longer waits for transfer N's bytes.

The mechanical route does not even need Blitz's `NetProvider` to become
async: the prescan primes the broker, transfers run on a small pool (the JS
`fetch` path already runs six in flight through the shared client,
`host.rs:203`), and `BrokerNet::fetch` becomes "join the transfer that is
already running, or start one" instead of "start one now and wait". The same
prescan output also answers `<link rel=preload>` for free.

If the decision goes the other way — serial is kept — then the Cargo comment
should say what it is actually buying, because "receipt order" is not it.

### B16.3 The settle loop: name the page that will never finish

Two rules from Lightpanda's scheduler are worth taking because they are about
honesty, not speed. A task that reschedules itself **never blocks completion**
(`Scheduler.zig:137`: "a task that endlessly reschedules itself would keep the
page alive forever"), and once timer nesting reaches depth ten, further
reschedules stop blocking too (`Timers.zig:20`) — the comment names
`requestAnimationFrame` loops as the common case.

Our virtual clock makes the *cost* of this problem zero — a self-rescheduling
timer burns no wall time — but not the *answer*. A page whose only remaining
work is a self-rescheduling interval rides `SETTLE_BUDGET_MS` to the cut-off
(`script/mod.rs:773`) and every `wait_for` on it answers `budget`: "the page
was still working, so it may yet appear". For an animation loop that is a
plausible lie. The page is not on its way anywhere; the condition will not be
met by waiting; the honest answer is the middle one.

The fix is not to copy `blocks_done` — collapsing "only periodic work
remains" into `quiescent` would be its own small lie, since a repeating timer
*can* change the DOM. It is to detect the state (every pending timer is a
repeat, or past a nesting depth, and no fetch is outstanding) and report it as
what it is: a fourth `end` beside `met`/`quiescent`/`budget`, or `quiescent`
with a named caveat, in the same spirit as `open_sockets`. Which of those two
shapes is right should be decided when it is built; what §B15.13's item 3
established is only that the distinction must not be erased.

### B16.4 The snapshot economy

Lightpanda's semantic tree is aggressively pruned for model context, and three
of its heuristics (`SemanticTree.zig:200-233`, `:524`) transfer directly:

* a **structural role** (generic, list, row, cell, navigation …) whose
  computed name is just its descendants' text concatenated, with no explicit
  `aria-label`, emits no name — otherwise every wrapper div hoists its
  subtree's text and the real text nodes then look redundant;
* a StaticText child whose text is a substring of its parent's name is
  dropped;
* a named leaf-semantic node — link, button, heading — does not walk its
  children at all.

Ours caps at 500 lines and truncates; theirs compresses before it ever needs
to cap. The difference is the difference between a snapshot that fits and one
that fits *and still contains the bottom of the page*. This is measurable in
the corpus harness (outline bytes per page, before and after) and should be.

The second economy is turns, not tokens: every Lightpanda read tool accepts an
optional `url` and navigates before reading, and its model guidance says to
prefer `markdown {url}` over `goto`-then-`markdown` — one round trip where an
agent otherwise spends two. On our side that is a `url` argument on the read
verbs, and with §B15.3's table built it is an exhaustive-match question each
verb must answer rather than a scattering of flag code.

Not taken from the same file: their per-session loading knobs
(`LP.configureLoading` — skip subframes, workers, external stylesheets). We
have no subframes or workers to skip, and stylesheet loading is what makes our
visibility filtering true rather than approximate. If a cheap text-only read
mode is ever wanted, it should be argued on its own, not imported.

### B16.5 Cookies: the PSL is a table, not a service

The `Domain` attribute was refused (§12's cookie narrowings) because honouring
it without a public suffix list lets `evil.co.uk` set a cookie for `co.uk`,
and the stated cost was real: a site that authenticates at `example.com` and
serves from `www.example.com` logs out between requests. §B11.5.1's login
corpus will hit this on its first multi-subdomain target.

Lightpanda shows the missing piece is small. Its PSL is a **generated static
table** compiled into the binary (`src/data/public_suffix_list.zig`, a
comptime perfect-hash set regenerated by a script), consulted for both the
Domain check and SameSite's registrable-domain computation, with the
label-boundary check that stops `attackerexample.com` matching `example.com`
(`Cookie.zig:324`). No fetch, no file, no staleness at runtime. In Rust the
same shape is the `psl` crate or a `phf` table generated in CI.

With that in hand, `Domain` can be honoured under the same fail-closed rules
(reject public suffixes, reject non-suffix boundaries), and the other three
narrowings stay exactly as written: in memory, never readable by an agent,
`Secure`/prefixes enforced. This closes a stated cost without reopening a
stated principle.

### B16.6 The allowlist checks a name; the wire connects to an address

Our policy layer decides on origins — names. Lightpanda's SSRF guard runs at
a different layer: curl's open-socket callback hands it the **resolved
sockaddr**, and the CIDR check runs there (`network/http.zig:240`), which
means a hostname that passes every name-level check and then resolves to
loopback or RFC1918 space is still refused. A name-level allowlist cannot do
that: DNS rebinding is precisely an allowed name resolving somewhere the
policy never saw.

For us the exposure is narrow — inside a box the egress proxy is the
enforcement point, and loopback is deliberately allowed — but the engine also
runs bare, the README's request-log claims apply there too, and "the receipt
says `docs.example.com` while the bytes went to `10.0.0.1`" is exactly the
plausible-wrong-record this file refuses everywhere else. reqwest does not
expose a socket hook, but it does expose `resolve()` overrides: resolve first,
check the addresses against the policy, pin the checked answer for the
request. That keeps check and connection on the same addresses, which is the
property the socket hook provides.

### B16.7 `wss://`, and a reason that was narrower than stated

The refusal of `wss://` says "it needs a raw TLS stream the HTTP client here
does not expose", which is true of reqwest and was quietly generalised into a
property of the engine. Lightpanda gets `wss://` for free because its socket
owns its transport — the WebSocket easy handle carries TLS, ALPN and proxying
itself. The same shape exists in our ecosystem: `tungstenite` over a rustls
stream is a socket that owns its transport, and the front half —
`authorise_socket`, receipt, then dial — is unchanged, as is the per-frame
receipting.

What this does *not* change: a remote `ws://` or `wss://` is still refused
whenever an egress proxy is configured, because a raw socket steps around the
proxy that carries the box's allowlist, and that argument never depended on
TLS. What it opens is `wss://` to loopback (dev servers behind local TLS) and
remote `wss://` on bare-host runs, where today the refusal message blames a
missing capability rather than a policy. Low urgency; recorded because the
stated reason was implementation-specific and the file should not carry it as
architecture.

### B16.8 Notes for the CDP item, still queued behind the agent loop

§B15.11 kept CDP behind the agent-loop work and required the conformance list
to ship before the endpoint. This read adds two notes to that file, one
mechanical and one confirming:

* Lightpanda counts every CDP method it does not implement
  (`cdp_unknown_commands`, `CDP.zig:344`, surfaced in its metrics endpoint).
  That is the conformance list's live complement: the published list says what
  is honestly absent, the counter says which absences real clients actually
  hit, in what volume. If CDP is built, both ship together.
* Its compatibility layer is a catalogue of client bugs worked around — a
  fake startup target because Puppeteer expects one, TCP keepalive instead of
  WebSocket ping because go-rod panics on pings and chromedp logs them as
  malformed, `Page.getFrameTree` shaped for Stagehand — confirming Obscura's
  discouraging cost estimate from the second source. The protocol is the small
  part; the clients are the work.

Also seen and noted, not taken: **WebMCP** (`navigator.modelContext` — pages
declaring their own tool manifests to the browser, surfaced as CDP events).
It is a bet that websites will ship agent-facing tools, and it is cheap for
Lightpanda because its whole surface is protocol-shaped. For us it is a new
inbound channel from untrusted page content to the agent, which is the
boundary this engine exists to harden. Reopen if the corpus ever meets a page
that ships one.

### B16.9 What not to copy, this pass

**Silent canvas stubs.** Lightpanda ships 61 `.noop = true` bridge functions —
`fillRect`, `arc`, `save`/`restore` — so canvas code runs and draws nothing,
silently. §B8.4 already names silent stubbing as the worst state for anything
here. But the comparison does sharpen §B11.5.8 (Canvas 2D, the largest
corpus-demand item): both reference engines fake or stub canvas because
neither has a rasteriser. We have one — the paint path is `blitz-paint` over
vello_cpu — so a *real* Canvas 2D is cheaper for this engine than for either
of them, and when the corpus item is paid it should be paid for real, not with
their stubs.

**The pseudo-layout.** It is their load-bearing necessity, not a model for an
engine that has Taffy. If a skip-layout fast path is ever proposed here, the
`contentWidth`/`contentHeight` comments are the spec for what a fake must
guarantee to avoid wedging real pages — and the fact that the spec is that
subtle is the argument for not building one.

**Wall-clock settling, SQLite-backed persistence, phone-home telemetry.** The
first would trade away determinism (§B15.10's replay position), the second is
§B6's storage line, the third is not what this engine is.

**Missing-API stack traces**: Lightpanda's unknown-property interceptor
records the JS stack of the first occurrence. Our `unsupported()` machinery
already exists and already ranks by count; first-seen stacks are a debug-build
nicety to remember, not an item.

### B16.10 The queue

Every item carries its gate. Per §B15.12a, the performance entries are built
*after* their before-measurement exists, and reverted if the after fails to
move it; the honesty and capability entries are gated by the rule of §B8 —
a page, or a stated claim, has to be asking.

1. **Negotiate compression.** Enable reqwest's `gzip` and `brotli`; decide
   and document what the receipt's byte count means afterwards (wire bytes
   and decoded bytes are different facts; the receipt should name the one it
   records, and arguably both). Gate: network-corpus wall clock and bytes,
   before and after. §B16.1.
2. **Load fonts once per factory.** Share one `FontSetup` across navigations
   of a session. Gate: measure the per-navigation cost first so the record
   has a number; this one is a defect fix regardless. §B16.1.
3. **Prescan and overlap subresource transfers**, HTTP/2 on. Receipts stay
   decision-ordered; per-hop redirect checks unchanged; the prescan output
   also serves `<link rel=preload>`. If refused, rewrite the Cargo comment to
   say what serial actually buys. Gate: a network-corpus page with many
   subresources, before and after. §B16.2.
4. **Name the page that will never finish.** "Only self-rescheduling work
   remains" becomes a reported settle outcome rather than `budget`. Gate:
   a corpus fixture with an animation loop, asserting the answer. §B16.3.
5. **Snapshot pruning**: structural-name suppression, StaticText dedup,
   leaf short-circuit. Gate: outline bytes per corpus page, before and
   after, with a diff review that the dropped lines were in fact redundant.
   §B16.4.
6. **`url` on the read verbs** — navigate and read in one round trip, as a
   verb-table question. §B16.4.
7. **`Domain` cookies over a compiled PSL**, SameSite computed on registrable
   domains; the other cookie narrowings unchanged. Gate: §B11.5.1's login
   corpus meeting a multi-subdomain site — which it will. §B16.5.
8. **Resolve-then-check-then-pin** for bare-host runs, so the address the
   policy checked is the address the socket dials. §B16.6.
9. **`wss://` over an owned transport**, when a page asks; the proxy-bypass
   refusal for remote sockets stands. §B16.7.

Nothing here displaces §B15.13's two open items: replay (§B15.10) remains the
natural next build on the verb side, and §B11.5.1's login corpus remains the
oldest and least-verified claim in this file — item 7 above is best built
against it, not before it.

### B16.11 What was built, 2026-08-26

Nine capabilities, in one pass, plus §B15.10's replay which this work made
buildable. What is *not* here is §B16.10's items 1 to 3 — compression, the
preload scan, the font fix — because each is gated on a before-measurement and
§B15.12a's lesson is that a performance change reasoned from the shape of the
code is a change that gets reverted.

**The settle escape hatch produced a fourth answer rather than the copied
one.** Lightpanda's rule is that a self-arming task stops blocking completion;
adopting only that would have folded an animating page into `quiescent`, which
claims nothing can change, and a repeating timer changes the DOM. So `periodic`
is a fourth `end` beside `met`/`quiescent`/`budget`. Two existing tests asserted
the old behaviour on `function again(){ setTimeout(again, 1) } again()` — the
exact shape the hatch exists for — and were rewritten; the "page that never
settles" case they were really testing now uses a timer past the budget, which
is a page that genuinely ran out of time.

**The snapshot work turned out to be a correctness fix, not a token one.**
`text_content()` concatenates a subtree, so a list item wrapping a heading, a
paragraph and a link reported one line reading `TitleBody textRead more` and
then suppressed all three as prose it claimed to have said. It had said them
simultaneously, unreadably, in an outline whose purpose is structure. Scoped to
*block* descendants: `<p>see <a>here</a></p>` and `<h2><a>Section</a></h2>` are
shapes the existing prose rule reads well and neither changed.

**Two of the nine found existing bugs.** `wss://` surfaced that `ws://` and
`wss://` were never mapped to their HTTP twins, so an allowed remote socket was
refused for "could not derive an origin" — a denial whose reason pointed at the
URL when the answer was the allowlist; it had stayed hidden because the proxy
rule refuses remote sockets first inside a box. And Canvas needed a user-agent
rule before anything reached the page: an inline `<canvas>` measured zero by
zero in Blitz, so the drawing worked, the pixels existed, `toDataURL` returned
them, and the rendered page was blank.

**Canvas is the clearest case of not copying the conclusion.** Both reference
engines fake it — Lightpanda with sixty-one silent no-ops — because neither has
a rasteriser. This one does, so what is implemented rasterises through
`vello_cpu` and composites into the page, and what is not reports itself by
name through the same channel as every other missing Web API. That split is the
whole difference between this and a stub, and it is enforced by the bridge
answering `false` for an operation it does not know rather than returning
quietly. The unbuilt operations are *present* rather than absent, which inverts
the usual rule for one reason: canvas drawing is incremental, and a throw on the
fourth of thirty calls loses the other twenty-six.

**`Domain` cookies paid off a stated cost.** §12's refusal was correct while
there was no list; the list is a compiled-in table, so the refusal cost more
than it bought. Four rules replace it, and the one that matters is the label
boundary — `attackerexample.com` may not claim `example.com`, which a bare
suffix test allows.

**The address check strengthens the engine's own claim rather than adding a
feature.** The allowlist decides about a name and the bytes go to an address;
Lightpanda closes that at curl's open-socket hook, and reqwest has none, so the
checked addresses are *pinned* through a custom resolver instead. A name that is
not in the map fails closed rather than being looked up, because failing open
there means connecting somewhere nobody approved.

Also shipped: `--url` on every read verb; a `structured` verb; batch
`open <url>...` sharing one broker, jar and font set; and an unknown-verb
counter, which is the one item here that exists to tell us what to build next
rather than to do something.

**A test was strengthened on the way past.** The skill's "teaches the verbs this
binary has" check matched a bare verb name, so `script` passed on the `--script`
flag and `structured` on the phrase "structured data" — two verbs an agent could
not have found were reported as documented. It now requires `session <verb>` as
a command.

Still open and unchanged: §B16.10 items 1 to 3 with their gates, §B11.5.1's
login corpus, and the §B11.6 iframe/popup question, which none of this touched.

---

# Formal verification: a Lean model beside the Rust

Status: proposed, 2026-08-15. Milestone stub: M16. Mode: the model is a
sibling of the implementation, never a dependency of it. No Lean code is
linked into the `h5i` binary, no Lean runs inside a box, and nothing on any
runtime path changes. Enforcement stays where it is today: in the kernel,
installed once at box setup. Verification touches only policy resolution,
which runs once per `env create`.

> **Pivot, 2026-08-16.** M16 shipped V1–V6 as written below, and building it
> taught us where the effort was misspent. The refinement theorem
> (`compile_sound`) turned out near-trivial: the resolver's output and the
> Landlock rule list are almost the same shape, so the proof was
> list-membership bookkeeping. Meanwhile the bugs that actually escape a
> sandbox of this kind — surveyed in §VF — are not policy-compilation errors
> at all; they are symlink and mount races on an adversarially-prepared
> filesystem (runc's 2025 breakout CVEs, the virtiofsd family) and
> configuration injection. The whole-config differential twin
> (`Model.lean` + `effective_drt`) also cost more to maintain than it caught:
> it mirrored a host-dependent pipeline field-for-field.
>
> So the effort turns from *modeling the resolver* to *an attack-driven
> filesystem authority machine* (§VF, "H5iFs"), whose theorems hold only
> after a defense is added and which doubles as the semantics of a per-run
> **translation validator** on the actual `policy.effective.json`. V1–V6
> stand below as the record of what M16 built; §VF supersedes their
> forward-looking parts. What was kept, retired, and why is in §VF.0 and in
> `lean/README.md`. **The phase machinery (V3 L3, `Phase.lean`) is retired,
> not merely paused: h5i has no in-process phase transition and the
> cache-refresh/run split is phase *separation* across two boxes, not the
> fd-carrying transition `Phase.lean` modeled — the V1/V3 text calling it an
> in-repo phase instance was wrong.**

## V1. What is being claimed, and why an interactive prover

The precedent is Cedar, AWS's authorization language: a production Rust
implementation, an executable formal model of the same semantics in Lean 4,
metatheorems proved against the model, and millions of differential tests
checking that the two agree ("verification-guided development", Amazon
Verified Permissions ships on it). Cedar is h5i minus the operating system:
its policies denote over an abstract request, and the story ends there. Ours
bottoms out in Landlock rule sets, seccomp filters, and mount tables, and that
is where the open ground is:

- seccomp has been formalized once, at the JIT level, in Coq (Jitk, OSDI
  2014). iptables has Isabelle semantics (Diekmann). seL4 proved intransitive
  noninterference for a whole kernel. **Landlock has no mechanized semantics
  at all, and neither does the composition of Landlock with seccomp and mount
  namespaces.** That composition is exactly what `build_confined_command`
  emits.
- No published system treats **phase-aware** sandbox policy: a policy that is
  deliberately different during dependency installation than during the run,
  with a transition between them. *(Retired 2026-08-16: h5i's cache-refresh/
  run split is phase **separation** across two boxes, not the in-process,
  fd-carrying transition this motivated. `Phase.lean` modeled a feature h5i
  does not have; see the chapter banner and §VF.0. The remaining V-text below
  is the M16 record.)*

Why an interactive prover rather than a solver: every property worth claiming
here is quantified over all policies, all reachable states, or pairs of
traces. Refinement ("the compiled mechanisms admit no trace the policy
forbids") and noninterference ("box A's secrets never influence box B's
observations") are not per-instance queries. And once the semantics is in
Lean, the per-instance facts a solver would have provided fall out for free:
a concrete policy against a decidable semantics is discharged by `decide`.
One toolchain, no gap between the checker and the theorems.

What a green result means, stated exactly, because the pieces compose and
none substitutes for another:

1. The DRT harness green (V4) means: the Rust resolver and the Lean model
   compute the same effective configuration on every input tried.
2. The refinement theorem (V3, L2) means: the model's compilation of any
   policy is sound against the model's mechanism semantics.
3. The conformance probes (V4) mean: on the hosts we run them, the mechanism
   semantics predicts what the kernel actually does, for the behaviours
   probed.

Together they say the deployed configuration enforces the written policy, up
to the trusted base in V5. Separately each is much less, and this chapter
will not blur them.

## V2. The one Rust change: dump the effective configuration at the apply seam

`policy.resolved.toml` is the digested *intent*. The *enforced* state is
larger: `ResolvedPolicy` carries runtime-only, serde-skipped fields that never
enter the digest and are still applied as mounts and grants, deliberately
(`crates/h5i-sandbox/src/sandbox_policy.rs:1899`): `ro_binds`, `home_binds`,
`private_binds`, `cache_write`, `work_readonly`, `user_egress_allow`, the
loopback port list, `box_git`. A model that reads only the toml verifies less
than what a box gets.

So the single change to the existing code is a second serialization,
`policy.effective.json`, written at box creation, with one rule that is the
whole point:

**The dump serializes the exact values handed to the mechanism appliers in
`build_confined_command`, not a parallel pretty-printer.** If the dump is
computed by separate code that re-derives "what we probably applied", the
model verifies a brochure. The serializer takes the same structs, at the seam
where Landlock rules, mount calls, and the seccomp filter are constructed,
after `$WORK` expansion and after `prepare_private_paths` and
`prepare_home_state` have run.

Contents, version 1 of a versioned schema, canonically ordered so the digest
is stable:

- the tier actually selected, and the claim it resolved from;
- Landlock grants as absolute paths with their access-right sets, read and
  write separately, `$WORK` expanded;
- every bind, with source, target, and writability: the ro binds, home
  binds, private binds, and the single `cache_write` if present;
- net mode, egress allowlist including host-side extras, the loopback port
  list, and the AF_UNIX flag;
- the seccomp template identifier and its parameters (the filter itself is a
  fixed artifact per template; the model treats templates as named
  semantics, V3);
- rlimits, `env_pass`, the tools allowlist.

`fs_deny` appears in the dump under resolution metadata, not under
enforcement, because it is not a kernel rule: Landlock is allowlist-only and
`fs_deny` is a preflight refusal condition on the *policy*. The model gives
it exactly that semantics, so what gets proved about it is "resolution
refuses", never "the kernel denies". Writing that distinction into the schema
keeps the model honest by construction.

The dump's digest is recorded in the capture manifest beside the policy
digest. That makes the verified artifact tamper-evident the same way the
policy already is, and it costs one hash.

## V3. The Lean development, layer by layer

A `lean/` package (lake project, built in CI, pinned toolchain). Four layers,
each meaningful without the ones above it.

**L0, mechanism semantics.** Small-step operational semantics of the
contracts h5i composes, at the level of their documented behaviour, not
kernel C:

- *Landlock*: a rule set is an allowlist of access rights over path
  prefixes; domains nest and **a nested domain can only intersect, never
  widen**; and the rights of a file descriptor are fixed at `open` and
  travel with the fd afterwards, through `fork`, `exec`, and `SCM_RIGHTS`.
  Paths are component lists; symlinks are out of scope in v1 and listed in
  V5, not silently ignored.
- *Mounts*: per-namespace tables, bind then read-only remount, private
  propagation. The interesting lemma is that a bind can re-expose a path a
  Landlock grant did not name, which is why the two are modeled together or
  not usefully at all.
- *seccomp*: a stack of pure functions over syscall number and arguments,
  most-restrictive result wins. h5i ships fixed filter templates, so the
  model gives each template a name and a denotation rather than modeling
  BPF.
- *Process state*: processes with fd tables and domain stacks; spawn
  inherits both.

The fd-as-capability rule is the scientific core. It is why phase
transitions are dangerous, it has never been mechanized, and every theorem
in L3 leans on it.

**L1, policy denotation.** The v1 `Profile` subset (fs grants, deny
preconditions, net mode, the unix flag, tools) denotes a predicate over
abstract actions: read p, write p, connect h, spawn t. Phases are policy
transformers with an explicit transition action. senv's install/run is the
motivating client; h5i's cache-refresh/run split is the in-repo instance,
so the phase machinery is exercised without waiting on senv.

**L2, compilation and refinement.** A pure `compile : Profile → MechConfig`
mirroring the decisions `build_confined_command` takes, and per backend the
theorem: every trace the L0 semantics admits under `compile p` maps to
actions `p` allows. Trace inclusion, proved as a simulation. Kernel tier
first. Container, microvm, and Seatbelt each get their own refinement later
or stay DRT-only, and the claim is **sound under-approximation per backend,
never cross-backend equivalence**: the tiers genuinely differ (private
`/tmp` on container, loopback semantics on macOS) and a theorem that denied
that would be false.

**L3, hyperproperties.** Three, in order of what they teach:

- *Monotone narrowing.* A phase transition never widens effective
  permissions. Under Landlock's intersection rule this is nearly free at
  the mechanism level; the content is that mounts and inherited fds do not
  break it.
- *The conditional fd theorem.* "Run phase forbids credentials" is **not**
  implied by the run-phase policy: an fd opened during install keeps its
  rights across the transition. The theorem comes out conditional: run-phase
  confidentiality holds if and only if the install phase could not open the
  resource, or the transition is an exec boundary that closes the fds. That
  conditional is a design output, not a caveat: it says senv must deny
  credentials in install too, or the transition must be an exec with
  close-on-exec discipline the dump can attest.
- *Box-to-box noninterference.* Two boxes whose writable grant sets are
  disjoint cannot influence each other, seL4-style, by unwinding
  conditions. Also conditional, and **the side condition is false today for
  two agent-profile boxes**, which share host `/tmp` by design. The theorem
  does not condemn that choice; it turns it into a checkable disjointness
  obligation per host, and the console can count it like any other receipt.

Nothing in v1 proves anything about the browser, the share tunnel, or the
viewer. The policy layer is the subject.

## V4. The differential harness, and the probes that check the model itself

Two loops, in the two directions a model can be wrong.

**Model versus Rust (drift).** A generator (proptest on the Rust side)
produces profiles, including the adversarial shapes that found real bugs in
this repo's history: grants overlapping a deny parent, `$WORK`-relative
escapes, an egress list on the process tier (must fail closed), home-state
redirects colliding with explicit grants. The Lean model compiles to a native
executable that reads a profile as JSON and emits its resolution and its
`MechConfig`; the harness diffs both against the Rust resolver's output and
the `policy.effective.json` dump. Corpus: everything under `examples/`, plus
generated cases, plus every past mismatch checked in as a named regression.
Cedar's experience, which we adopt as a working assumption, is that this loop
finds bugs in both directions. The CI job is separate and non-gating until it
has been quiet for a while; then it gates.

**Model versus kernel (fidelity).** The semantics is executable, so `#eval`
on an action trace predicts allow or deny. The harness emits those
predictions as small probe programs and runs them inside real boxes,
comparing outcome to prediction. This is `sandbox::verify_exec` generalized:
that function exists because mechanism-present is not mechanism-works, and
the same discipline applies to a model. Linux first; Seatbelt probes read
their denials from `log show`, which is the only place Seatbelt puts them.

## V5. What is not modeled, stated up front

The trusted base, in the same spirit as section 9:

- **The kernel.** Landlock, seccomp, and namespaces are assumed to implement
  their documented contracts. The probes sample this assumption; they do not
  discharge it. Kernel bugs and side channels are out of scope.
- **Symlinks**, in v1. Landlock resolves them at access time and the model
  does not, yet. Until it does, the refinement theorem is stated over
  symlink-free traces and says so in its hypotheses.
- **/proc and ptrace**, beyond what the seccomp templates already block.
- **Container and microvm tiers.** Their enforcement runs through an OCI
  runtime and a guest kernel the model does not describe. They stay in the
  DRT loop (the resolver is shared) but carry no refinement claim until
  someone writes their L0.
- **The Lean toolchain and its extraction**, as with any mechanized proof.
- **Model drift.** The model is hand-written against the Rust; the DRT loop
  is the control, and a quiet DRT job is evidence, not proof. The upgrade
  path that removes this line is translating the resolver itself
  (Aeneas-style, Rust to Lean); it is deliberately not in scope for M16,
  because it requires carving the resolution logic into a pure core and
  nothing above requires that to start.

## V6. The order

1. **The dump.** `policy.effective.json` at the apply seam, schema v1,
   digest into the capture manifest. Small, pure Rust, useful on its own for
   debugging. Exit: every serde-skipped field of `ResolvedPolicy` is either
   in the dump or named in the schema as excluded, with a reason. **Built and
   driven, 2026-08-15 — see M16 for what shipped and where.**
2. **The model, executable.** `lean/` package, L1 for the fs and net
   subset, the JSON interface, DRT over `examples/` plus 10k generated
   profiles. Exit: the M16 criterion, zero unexplained diffs. **Built and
   driven, 2026-08-15 — see M16; the profile-corpus sweep and the
   interactive/tilde lanes landed with the follow-ons the same day.**
3. **The Landlock fragment.** L0 domains, intersection, fd rights, and the
   two phase theorems, including the shared-`/tmp` counterexample as a Lean
   example, not prose. Exit: theorems check in CI. **Built, 2026-08-15 —
   see M16.**
4. **Refinement.** L2 for the kernel tier over the v1 action alphabet.
   **Built, 2026-08-15 — see M16**: soundness unconditional, completeness
   conditional on a full world, both machine-checked.
5. **Noninterference and probes.** L3 with unwinding conditions; probe
   generation running against real boxes on Linux in CI. **Built,
   2026-08-15 — see M16**: the unwinding theorem, the decidable
   `interferesCheck` with soundness, the agent-`/tmp` and disjoint-box
   instances, and `--predict`-driven probes green on a real process-tier
   box. The probes run in the Lean CI lane and skip loudly on runners that
   cannot host the tier.

Steps 1 and 2 are weeks, not months, and step 3 is the first thing worth
writing up. Everything after that earns its own status line here when it
exists, in this document's usual voice: built when driven, proposed until
then.

# VF. Verified workspace authority: the H5iFs machine

Status: proposed, 2026-08-16. This chapter is the post-pivot direction (see
the banner at the top of the verification chapter). It replaces "model the
resolver, differential-test the twin" with "model the thing that actually
mediates host filesystem authority, prove it confines an adversary, and make
that model the semantics a per-run validator checks the real output against."
The title says *authority*, not *filesystem*: until the optional backend of
VF.6 exists, what is verified is an authority model and a validator, not a
filesystem implementation.

The claim it builds toward, stated at the strength the design actually
supports — no stronger:

> For every Linux **process/supervised-tier** run the validator covers, a
> checked validator confirms that the filesystem authority the box's
> **effective plan** would grant — under an adversary who prepared the
> worktree and drives the box — is no greater than the declared source
> policy, and that a mount-realization audit found the **observed** mount
> state consistent with that plan before exec.

Three levels are kept distinct throughout, because collapsing them is the
overstatement the pivot exists to avoid:

- **SourcePolicy** — what the user declared.
- **EffectivePlan / MountPlan** — what h5i intends to hand the backend
  (`policy.effective.json` plus the ordered mount manifest). The validator
  (VF.4) reasons about *this*.
- **ObservedMountState** — what the kernel actually realized, read back
  before exec. The auditor (VF.5) checks *this* against the plan.

The validator proves a property of the plan; it does not by itself prove the
kernel granted exactly the plan. That gap is what VF.5 narrows and VF.8
states. Seatbelt, container, and microVM tiers are **not** covered at this
strength — they are named where they touch this and otherwise out of scope.

## VF.0. What was kept, retired, and why

Retired to git history (the pivot banner explains the reasoning):

- `H5iSpec/Model.lean`, `Input.lean`, `Theorems.lean` — the hand-ported twin
  of `compute_effective` and its over-all-inputs theorems. Verified a
  re-implementation, not the shipped output.
- `H5iSpec/Refinement.lean` — `compile_sound` /
  `compile_complete_of_world_full`. Near-trivial because spec and output
  share a shape; `parsePath`/`compileLandlock` (the useful parts) moved into
  `Landlock.lean`.
- `H5iSpec/Phase.lean` — the fd-smuggling phase machine. Modeled an
  in-process install→run transition h5i does not have. Its two lessons are
  preserved as prose in §VF.3 (fds carry rights past a restriction; shared
  `/tmp` breaks box separation) and can return if a same-process phase
  feature is ever built.
- `tests/effective_drt.rs` — the whole-config differential harness. The one
  lane worth keeping (the interference checker) was extracted to
  `tests/interferes_drt.rs`.

Kept, because each is either the base layer the new machine extends or a spec
the *product* runs against today:

- `H5iSpec/Landlock.lean` — L0 rulesets, intersecting domains,
  `restrict_narrows`, `deny_persists`, and now `parsePath`/`compileLandlock`.
  The H5iFs machine is built on top of this, not beside it.
- `H5iSpec/Noninterference.lean`, trimmed to `interferesCheck` +
  `interferesCheck_sound` + the agent-`/tmp` and disjoint-box instances. This
  is the specification of the product-live `effective::interferes`
  (`env.rs`), differential-tested by `interferes_drt`.
- `H5iSpec/Predict.lean` — the bind/symlink/procfs prediction layer that
  `effective_probes.rs` holds a real box to. Its semantics is the seed for
  H5iFs's resolver (§VF.2); it is absorbed, then retired, not rewritten.
- `H5iSpec/Seatbelt.lean` + `seatbelt_drt.rs` — `fs_deny_wins`, a live
  property of the macOS backend.

Known cost, stated precisely. The retired `effective_drt` diffed the whole
`compute_effective` output against the twin; deleting the twin ends that
differential check. Note two things so the gap is neither hidden nor
overstated:

- **What still covers `compute_effective`.** Its filesystem-relevant output
  is still held to the *kernel* by `effective_probes` (via `Predict`), and
  `effective::interferes` over its output is still diffed against Lean by
  `interferes_drt`. The fs surface is not uncovered.
- **What lost coverage.** The non-fs fields — net mode, seccomp template,
  rlimits, `env_pass`, tools — no longer have a differential check against
  any spec. These were exactly the DRT-only, no-refinement-theorem fields.

Un-deleting `effective_drt` is not the fix: the harness diffs against
`Model.lean`, so keeping it means keeping the twin — reverting the pivot.
The fix is VF.4, whose validator restores a spec check on the *shipped*
plan, fields included. Until then this gap is deliberate and named here.

## VF.1. Why a filesystem, and why "authority", not "storage"

The survey (§VF.9) points at where verified-filesystem effort has gone.
Among the systems surveyed here — FSCQ, DFSCQ, Yggdrasil, Perennial,
DaisyNFS — the target is *functional correctness and crash safety* for a
filesystem whose caller is a **benign** POSIX client; the adversary is power
loss and concurrency. One of them (SFSCQ/DiskSec) proves a security property,
confidentiality, and among those surveyed none formulates **authority
confinement against a hostile caller** (not a claim about the whole
literature — VF.9 flags the systematic review owed before any external
claim). h5i's problem is the dual of the classic one:

- storage semantics can be *simple* — the box is disposable, so crash
  consistency is out of scope entirely (if the daemon dies, throw the box
  away). This is the single assumption that moves the effort from
  person-years to something a small team can hold; it is a scope decision
  backed by the field's own effort numbers (BilbyFs: 13k proof lines for 1k
  of code, almost all of it crash/refinement).
- the **caller is the adversary**. The agent in run N writes the worktree
  freely; the privileged setup for run N+1 (canonicalize → make mountpoints →
  bind → `restrict_self`) then traverses that worktree. Planting
  `evil → ~/.ssh` or a procfs symlink is the textbook runc attack, reproduced
  in h5i's own threat model.

So the object to verify is not a general filesystem. It is the **authority
machine**: the layer that decides, for each filesystem operation, which host
object it reaches and whether policy permits it — over symlinks, hardlinks,
ordered mounts, and inherited fds. "ext4 replacement" is a non-goal forever.

## VF.2. The model: `lean/H5iFs/`

A small filesystem modeled as an object graph, not a byte store. File
*contents* are opaque values (`Nat`); the machinery is names, identity,
resolution, and rights. The design rule that keeps it from being `decide`-dead
and keeps it executable (both required — the model must run as the validator's
semantics and probe oracle): **finite maps (association lists with a sortedness
invariant), no function-typed state fields, and the house no-mathlib rule.**

Core state, sketched (final shapes land with the code):

```
abbrev NodeId := Nat            -- opaque model-internal identity, NOT an inode
inductive NodeKind | file | dir | symlink (target : Path)
structure Meta where            -- integrity is more than content (VF.3)
  mode, uid, gid : Nat
  suid, sgid, exec : Bool
  nlink : Nat
  xattrs : List (String × Nat)
structure FsState where
  nodes   : List (NodeId × NodeKind)          -- finite, sorted; NOT NodeId → _
  entries : List ((NodeId × String) × NodeId) -- dir/name ↦ child; hardlink = two entries, one child
  content : List (NodeId × Nat)
  meta    : List (NodeId × Meta)
```

`NodeId` is a model-internal identity, **not** a Linux inode number: inode
numbers are unique only within a device and are reused, so when the model
touches the host it does so through an `ObjectKey := MountId × DeviceId ×
Inode × Generation?`, and equality of host objects is equality of keys, never
of inode alone. The base snapshot is a `BaseProjection` — the nodes,
directory entries, **and** metadata that must be invariant — not a bare
`List NodeId`: an immutable base whose *contents* are pinned is still broken
if a `base` directory entry can be renamed or unlinked.

Five amplifiers it must model, each because an attack needs it:

1. **Symlinks**, with every case that has bitten a real system spelled out,
   not "symlinks": relative vs absolute targets, a symlinked *ancestor* (not
   just final component), `.`/`..`, loops (fuel-bounded), the `/proc/self/fd`
   magic-link family, and target resolution that crosses a mount. A naive
   string-prefix check passes `/work/evil → /home/user/.ssh`; resolution
   lands outside the grant. Both are machine-checked.
2. **Hardlinks and rename.** Several entries name one `NodeId`, so
   `/work/allowed` and `/secret/alias` are one object. Rights are reasoned
   about on **object identity**, not path — the single most important
   structural break from every kept file, which is path-based.
3. **Ordered mounts and shadowing.** `/work` rw, then `/work/.claude/…` ro:
   reverse the mount order and the ro overlay becomes writable again. Not
   hypothetical — `home_binds_in_mount_order` in `sandbox.rs` sorts binds by
   hand to avoid exactly this, and the config-lock is a real ro-over-rw
   overmount. The verdict is `effectivePermission mounts path`, over the mount
   *order*, and the theorem is `protected → effectivePermission ≤ ReadOnly`.
4. **File descriptors as capabilities.** Open `~/.ssh/id_rsa`, then
   `restrict_self`, then read from the saved fd. Landlock fixes rights at
   `open`; the restriction does not revisit them. The model carries an fd
   table with rights; the invariant is `NoForbiddenFd` on the **ready** state
   (after setup closes fds), not on the initial one — a forbidden fd in the
   *initial* state is fine if setup closes it, and only `FD_CLOEXEC` fds close
   at exec on their own, so setup must either close explicitly or carry a
   CLOEXEC invariant. Naming this as an initial-state condition would be
   wrong.

The **operation alphabet is fixed up front**, so nothing security-relevant
hides in an unmodeled op: `lookup/open/read/write/truncate`,
`create/mkdir/unlink/rmdir`, `rename/link/symlink`, `execute`,
`chmod/chown/setxattr`, `dup` and fd-relative operations (`ftruncate`,
`*at`), and `mmap` only if the daemon of VF.6 needs it. And Landlock is
modeled with its **real** right set — `execute`, `read-file`/`read-dir`,
`remove-file`/`remove-dir`, `make-*`, `refer`, `truncate`, ioctl-dev — not a
two-symbol read/write, and the rights bound to an fd at open are distinguished
from the rights checked per path.

**The adversary acts during setup, not only before it.** The threat is TOCTOU:
a symlink swapped between the check and the mount. So the model is not a fixed
`initial` state plus attacker ops afterward, but an interleaved schedule —

```
inductive SetupEvent
  | h5i      (op : SetupOp)          -- mount, close-fd, no_new_privs, restrict, …
  | attacker (op : AllowedMutation)  -- what a worktree writer can do, between h5i's steps
```

— and the theorems quantify over an arbitrary well-formed adversarial
`initial` **and** an arbitrary attacker interleaving. A model that only
quantified over the starting state cannot even express the check-vs-mount race
that is the entire runc-CVE class; this is the half of the threat model the
classic verified filesystems omit, and it is what makes the setup steps
(symlinked-ancestor refusal, fd closing, race-free mount construction — VF.5b)
do real work instead of unfolding to `True`.

The **setup state machine models the real order and names what it abstracts.**
The actual `build_confined_command` path also sets up namespaces, an egress
helper, a PID-namespace supervisor, private procfs, rlimits, and a
notification listener, and `close_inherited_fds()` runs on a specific
supervisor path. The v1 machine covers `mount → close inherited fds →
no_new_privs → Landlock → seccomp → exec` and lists the steps it omits, rather
than implying the abstraction is the whole pipeline.

## VF.3. The theorems, integrity before confidentiality

Integrity before confidentiality, because integrity needs no observation
model. But the central theorem is **not** final-state equality — that misses
the write-then-restore attack, where a forbidden object is changed and put
back. It is **trace-level authority safety**: every effect a trace produces is
one the policy permits.

```
theorem every_effect_authorized
    (hadv   : Adversarial initial)                 -- any hostile worktree
    (hsetup : setup config initial sched = .ready ready)  -- setup accepted
    (hrun   : Runs ready attackerTrace final) :
    ∀ e ∈ effectsOf attackerTrace, PolicyAllows policy e
```

`ProtectedProjection` equality — that no object outside the writable grants
changes — is then a **corollary**, and it must project more than content:
integrity is broken content-unchanged by a `rename`, an `unlink`, a `chmod`,
or an xattr edit. The projection pins content **and** directory entries,
ownership/mode/executable bit, link count, symlink target, and the relevant
xattrs.

Two framings that keep the theorem from being vacuous:

- **Setup is executable and total: reject-or-confine.** Not a `ValidSetup`
  hypothesis (which lets the theorem hold whenever setup is impossible), but a
  function that returns `.rejected` or a `.ready` state, with
  `setup_rejects_or_confines : setup config initial sched = .ready r →
  ReadyStateSafe policy r`. An adversary that can force setup to a state the
  theorem does not cover forces a *rejection*, not a silent pass.
- **The fd condition is on `ready`/`ExecState`, never `initial`** (VF.2).

**The attack menu is the specification.** Each item is entered first as a
counterexample against a deliberately-weak setup, then defeated by adding the
defense; the theorem is the *reward* for the defense, never a consequence of
unfolding definitions:

| # | Attack | Defense the theorem forces |
|---|--------|----------------------------|
| 1 | string-prefix path escape | resolve, then judge the resolved object |
| 2 | symlink / symlinked-ancestor escape | resolution follows links, verdict at the target |
| 3 | pre-opened fd past a restriction | `NoForbiddenFd` on `ready`, explicit close or CLOEXEC |
| 4 | parent-allow / child-deny | per-object rights in the ideal model; validator **rejects as unrepresentable** on a backend that can't express it (below) |
| 5 | rw mount shadows a ro overlay | `effectivePermission` over mount order |
| 6 | rename/hardlink authority amplification | rights on `ObjectKey`, not path |
| 7 | check-vs-mount TOCTOU during setup | race-free mount construction (VF.5b), interleaving-quantified |

**Ideal semantics and backend expressiveness are separate layers.** Attack #4
is where this bites: per-object rights in the H5iFs *ideal* model do not make
a "parent readable, one child denied" policy *implementable* on the Landlock
backend, which grants over path-beneath scopes and has no deny rule. So the
validator carries a representability check: a source policy the selected
backend cannot express is **rejected as unrepresentable**, never compiled to
something weaker and reported as enforced. This is the filesystem instance of
the project-wide rule that nothing unenforceable is rendered as enforced.

**Confidentiality is later, and stated as noninterference, not object
inclusion.** "The trace only touched readable objects" is too weak: what
leaks is not the object set but the *observations* — read results, error
codes (ENOENT vs EACCES distinguishes existence), and metadata. The honest
form is two-run: `LowEquivalent policy init₁ init₂ → observations trace₁ =
observations trace₂`, over an observation function that includes responses,
content, errors, and metadata. DiskSec's sealed-content factoring is a
**candidate** technique to keep this tractable, not a given — H5iFs has path,
error, and metadata channels DiskSec's block model did not, so its
applicability is something VF's confidentiality step must establish, not
assume.

## VF.4. The payoff: a per-run translation validator

The H5iFs model is not a separate toy; it is the **semantics of a validator on
the real plan.** The existing `policy.effective.json` (V2) plus a *mount
manifest* (the ordered mounts + classes the backend was handed) feed a
decidable checker. Its effects are **filesystem** effects only — `Read p`,
`Write p`, `Execute p`, `Remove p`, `Create p` — so `≤ ReadOnly`,
deny-not-circumvented, and writable-set-limited are expressible;
`Connect`/`ReceiveSecret` are **out of scope here** and belong to a future
`AuthorityEffect` layer that composes an fs checker with network/secret
checkers. And its world input is not the whole host filesystem (unenumerable)
but a finite `WorldEvidence`:

```
structure WorldEvidence where            -- what the harness measured, not the whole FS
  paths     : List (Path × ObjectKey)    -- relevant paths and their measured identity
  symlinks  : List (Path × Path)         -- link → target facts
  mounts    : List MountFact
  complete  : CompletenessWitness        -- the closure condition making the finite set sufficient

def validate (policy : SourcePolicy) (plan : EffectivePlan)
             (ev : WorldEvidence) : ValidateResult    -- .ok | .rejected reason

theorem validate_sound :
  validate policy plan ev = .ok →
  ∀ e, PlanAdmits plan ev e → PolicyAllows policy e
```

Deployment (settled points from the design discussion, carried here so they
are not relitigated):

- **Execution shape, stated without contradiction.** What runs *per run* is
  the **unproven Rust port** of the checker; it writes its verdict to the
  receipt. The proof lives in CI: the **real Lean checker** runs over every
  generated case, and a **checker-level DRT** (`interferes_drt` is the
  template — small surface, strong sampling) holds the port to it. So the
  honest phrasing is "the shipped plan conforms, per run, to a Lean-proved
  specification, checked by a Rust port that CI differential-tests against the
  proof" — **not** "a machine-checked proof runs on every run." The residual
  trust is the port; the DRT is its control, not its proof. (The stronger
  option — compile the Lean checker and run *it* pre-exec — stays open in
  VF.7 if the port's residual trust proves unacceptable.)
- **Fail-closed by type.** The plan is a `ValidatedPlan` typestate: `spawn`
  accepts only a validated plan, so "the run went through the validator" is a
  Rust type obligation, and `validate_sound` is what the type is worth. This
  is the cheap half of mediation; that the kernel *realized* the plan is VF.5.
- **Parse the real output.** The checker reads the *emitted* argv/rule text
  where it can (the msb/Seatbelt argument strings), not just the in-memory
  struct, so a serializer bug can't slip past — the Kata `source=/` injection
  is the cautionary case.
- **Receipt.** `validated=ok`, the policy and plan digests, the checker
  version, the backend, and the per-claim booleans (`fs_subset`,
  `writes_confined`, `cache_readonly`), rendered in `box status` the way the
  console already counts receipts.
- **`no_shared_writable` is not a single-run property.** Whether a box shares
  a writable-readable path with *another* live box can only be decided against
  all live boxes, under a global lock or an atomic registry snapshot at box
  create/exit — otherwise two boxes race into a shared `/tmp` between their
  checks. It is a cross-box obligation on the registry, reported separately,
  not a field the per-run validator can set alone.
- **Backend representability, honestly.** The check is not "egress subset" but
  "can this backend *represent* this constraint at all." Enforcement points
  differ per tier (kernel: nft + egress proxy; microvm: msb's coarser on/off;
  macOS: SBPL carries no network proof), so an unrepresentable constraint is
  rejected or marked unenforced — never rendered as enforced. No silent tier
  downgrade.

## VF.5. Mount realization audit: plan-check plus a read-back

`validate_sound` says "the plan is safe"; it does not say "the kernel realized
the plan." For mechanisms whose output is a syscall stream (mounts, Landlock
rulesets) there is no argv to re-parse, so the plan-checker leaves a gap — the
serializer-bug class, one layer down. Narrow it with a **mount realization
audit**: after setup, before `exec`, the supervisor reads back the child's
realized state and diffs it against the `ValidatedPlan`; a mismatch aborts the
launch and lands in the receipt.

Two honest bounds on what this buys, because "complete mediation" would
overstate it:

- **It is a mount-topology/identity audit, not a full mediation.**
  `/proc/<pid>/mountinfo` exposes mount ID, parent, major/minor, mount root,
  mount point, and ro/nosuid/nodev/noexec/propagation flags — it does **not**
  expose the installed Landlock ruleset or seccomp filter. So the audit
  catches mount-topology, flag, and source-identity mismatches; it does not
  read back the fs-grant enforcement itself.
- **It detects a large slice of the TOCTOU class, not all of it.** It turns
  mount-swap and masked-path realizations (the shape of runc's 2025 CVEs)
  from "prevent perfectly" into "detect and fail closed," but a symlink race
  that leaves mount topology unchanged, or a shared source mutated after the
  read-back, is not caught here — those are prevented by construction in
  VF.5b, and the audit is the net under that discipline, not a substitute.

To make it worth its name the audit reads more than `mountinfo`: mount ID and
parent, major/minor, mount root, ro/rw and nosuid/nodev/noexec, propagation
flags, per-target object identity via `statx`/`fdinfo`, the inherited-fd
inventory, `NoNewPrivs`, and the seccomp mode. It shares the H5iFs semantics
with the validator — one checks the plan, the other checks what was realized.

**The audit needs an explicit exec barrier.** Today `Command::pre_exec` runs
setup and then execs in the same breath, with no point for a second party to
look. So the design adds a handshake: the child completes setup and **stops**
(a `SIGSTOP` or a blocking wait on a pipe), the supervisor performs the audit,
and only on success sends *go*; on mismatch it kills the child. Without this
barrier "audit before exec" has nowhere to stand.

## VF.5b. Race-free mount construction

The audit is a net; prevention is the floor under it, and it belongs in the
setup code, not in a theorem. Two disciplines:

- **Resolution.** Every path the privileged setup opens on the adversarial
  worktree goes through `openat2` with `RESOLVE_NO_SYMLINKS` and
  `RESOLVE_BENEATH`, then fd-relative operations only — no second lookup of a
  path already checked. This is what closes the check-vs-mount window at the
  resolution layer.
- **Mount by handle.** `openat2` alone does not remove races in path-based
  `mount(2)`, whose source and destination are re-resolved by string. Where
  the kernel allows, setup uses the fd-based mount API — `open_tree` to hold a
  mount subtree as an fd, `mount_setattr`, `move_mount` — so the object
  mounted is the object checked, by descriptor identity, not by re-walked
  path.

The setup state machine (VF.2) treats these as obligations on its mount steps;
VF.5's interleaving quantifier is what turns "we call `openat2`" into a
property that survives an attacker acting between the steps.

## VF.6. Optional product: `verified-cow` workspace backend

If the model matures, it becomes a real backend, not a permanent toy — but the
target is a **workspace filesystem for `/work`, not a general FS.** A
copy-on-write view over an immutable base snapshot plus a writable delta,
exported as a git patch:

```
[workspace]
backend = "host"          # today: live host sharing, the default
# backend = "verified-cow" # isolated overlay with a verified core
```

Constrained from the start, which is what makes it tractable: base read-only,
writes only to the delta, nothing outside `/work` representable, no device
nodes, no setuid/setgid, no dangerous xattrs, symlinks confined to the
workspace, host paths never re-resolved per request, output is a git patch.

Crash safety, stated precisely, because "no crash safety, ever" is too broad:
the disposable **delta** carries no durability or recovery guarantee (a
crashed daemon means a discarded box), but "a daemon crash cannot corrupt the
immutable base or touch the host" is a **safety requirement that holds across
crashes**, not something disposability waives.

**Concurrency is not merely deferred — it is designed out of v1.** A FUSE
backend cannot ship as "sequential model, concurrency out of scope": the
daemon serves concurrent requests. The v1 daemon serializes every request
through a single queue and makes that serialization point the model boundary
(the DaisyNFS move — concentrate concurrency in one place), and the FUSE
cache, writeback, and mmap paths are disabled or restricted so there is no
second, unmodeled ordering.

The executable core is `handle : FsState → Request → Except Err (FsState ×
Response × List StorageEffect)`, with `handle_preserves_wellformed`,
`effects_are_authorized`, and `immutable_base_unchanged` (over the
`BaseProjection`, not content alone). The honest label is **"Lean-verified
filesystem authorization and state-transition core,"** not "fully verified
filesystem", and not "verified backend" at all until it is settled *how* the
core executes — Lean-generated core, Rust port under DRT, or Verus (VF.7) —
each of which licenses a different claim. The unverified Rust adapter that
translates FUSE requests to `Request`s must hold no ambient authority beyond
the base/delta fds and whatever pre-opened control fds it needs (`Landlock`ed
to exactly those, no path-opening API), and is fuzzed at the protocol layer.

**Patch export is its own security boundary.** The output is a git patch
applied on the host, and `git apply` has traversed outside the working tree
via a patch that creates a symlink then writes through it (CVE-2023-23946). So
export is not an afterthought: a safe exporter/validator, with
`exported_effects_confined` — every effect an applied patch produces stays
within `/work` and follows no symlink out of it — before any host apply.

**This backend does not change the current UX or default.** Live host sharing
of the disposable worktree stays the default; `verified-cow` is the
patch-only, feature-restricted tier for hostile packages or strong
prompt-injection exposure. Host-side viewing is `box diff` / `box export`, not
a live editor mount, at least at first.

## VF.7. Two decision points, named not resolved

The order below defers these on purpose; they are called out so the choice is
deliberate when it arrives.

- **Verification boundary (DaisyNFS's lesson).** DaisyNFS chose the NFS
  protocol as its boundary, not the VFS/FUSE hooks, precisely to get
  coarse-grained operations and dodge the kernel-callback swamp. If H5iFs
  grows a daemon, its boundary is chosen for *provability* — a FUSE-lowlevel
  subset with the cache off, or a 9p-style protocol — not for feature
  completeness. The unverified adapter stays thin by construction.
- **Lean vs Verus for the daemon.** The semantics and validator layers are
  Lean, continuous with the kept `H5iSpec`. But a *daemon implementation*
  wants either Coq/Lean-style extraction (a gap) or Verus, which verifies the
  shipping Rust directly (two OSDI'24 best papers, industrial use). The
  read today: Lean for semantics/validator; revisit Verus at the daemon stage
  so the executable core has no extraction gap. Not decided now.

Also settled, so it is not reopened: **no delta crash-recovery** (disposable
boxes; base+host confinement still holds across crashes — VF.6); **no
in-process phase system** (Landlock only narrows, the wanted direction is
widening, which belongs to a broker or a second box; an exec boundary closes
only `FD_CLOEXEC` fds, so the transition still needs an explicit close
discipline — VF.2); **no verified microVM/VMM** (SeKVM/VeriSMo scale for the
wrong surface — the microVM/container escape vulnerabilities *relevant here*
are filesystem-sharing and configuration bugs, virtiofsd and `source=/`
injection among them, which the validator's mount-manifest check addresses;
this is not a claim that VMM-side vulnerabilities do not exist).

## VF.8. What is not modeled, stated up front

The trusted base, same spirit as V5 and section 9:

- **The kernel.** Landlock, namespaces, and `openat2` resolution flags are
  assumed to honor their contracts. The conformance probes (kept from V4)
  sample this; they do not discharge it. This is *more* important post-pivot,
  because the validator's soundness is conditional on the H5iFs semantics
  matching the kernel — a validator that is provably sound against a wrong
  semantics is wrong soundly.
- **The FUSE protocol adapter** (if VF.6 is built): unverified, kept thin and
  authority-starved, fuzzed, not proved.
- **Concurrency.** The v1 model is sequential. A daemon serving concurrent
  requests needs the DaisyNFS move (concentrate concurrency in one verified
  layer) before any concurrent theorem means anything.
- **Timing, cache, and metadata side channels; the network egress path;
  the seccomp filter semantics.** As in V5.
- **The Lean toolchain and the Rust port of the checker.** The checker-level
  DRT is the control on the port, not a proof of it.

## VF.9. Related work this rests on

The survey that reset the direction (2026-08-16). Each entry states what the
work *proves* and how H5iFs differs, so it is not read as "we do what they do,
harder":

- **FSCQ (SOSP'15), DFSCQ (SOSP'17), Yggdrasil (OSDI'16).** Prove functional
  correctness and crash consistency of a real FS against a benign caller.
  H5iFs proves neither — it drops crash safety entirely and targets authority
  under a hostile caller. Their cost is the point: BilbyFs's 13:1
  proof-to-code ratio, almost all crash/refinement, is what dropping it buys.
- **Perennial/GoJournal, DaisyNFS (OSDI'22).** Prove a concurrent, crash-safe
  FS by confining concurrency and crash to one verified transaction layer and
  reasoning sequentially above it. H5iFs borrows the *technique* (serialize at
  one boundary, VF.6) and the boundary-choice lesson (VF.7), not the crash or
  concurrency proofs.
- **SFSCQ/DiskSec (OSDI'18).** Proves confidentiality (data noninterference)
  for a FS via sealed blocks. Closest in spirit; differs in caller model
  (benign) and in channels (block contents, where H5iFs also has path, error,
  and metadata channels — VF.3). Its factoring is a *candidate*, not imported.
- **SibylFS (SOSP'15), Metis/RefFS (FAST'24).** Executable reference model
  differential-tested against real filesystems (no theorem about a caller).
  H5iFs being executable ⇒ it is the oracle for the attack suite against live
  backends before any daemon exists — the same use, an adversarial corpus.
- **Verus (SOSP'24; two OSDI'24 best papers; Atmosphere SOSP'25).** Verifies
  shipping Rust directly, closing the extraction gap. The VF.7 decision point
  for the daemon; not adopted for the semantics/validator layers.
- **WaVe (S&P'23).** Verifies a sandbox runtime's syscall boundary
  (memory/fs/network isolation) with an explicit OS spec, but no
  filesystem-*state* semantics. H5iFs is the missing fs-state model under a
  claim like WaVe's.
- **The threat, real and recent.** runc 2025 breakouts
  (CVE-2025-31133/52565/52881, masked-path and mount races), CVE-2021-30465
  (symlink-exchange mount swap), virtiofsd (CVE-2020-35517 device nodes,
  CVE-2022-0358 SGID, Kata CVE-2026-44210 `source=/` injection), git
  CVE-2023-23946 (`git apply` symlink traversal — the export boundary, VF.6).
  None is a policy-compilation bug; all live in the filesystem-authority
  surface H5iFs models.

The positioning, stated as a search result rather than a proof of absence:
**we have not found prior work combining authority confinement for a
disposable workspace filesystem under an adversarial caller with a model that
doubles as a per-run validator on shipped output.** A systematic literature
review is owed before that is claimed externally; VF.1's "among systems
surveyed here" hedge stands until it is done.

## VF.10. The order

1. **`lean/H5iFs/` semantics.** Object graph, symlink/hardlink/rename, ordered
   mounts, fd table, setup state machine — built as the validator's semantics
   module from line one, not a standalone toy. Absorb `Predict.lean`'s
   resolver. Exit: the 7 attacks (VF.3) are each expressible as a
   counterexample against a weak setup, and `every_effect_authorized` holds
   over an adversarial initial state and attacker interleaving once the
   defenses are in. Theorems check in CI.
   **Built, 2026-08-16** (26 theorems, no `sorry`/`admit`, checked by
   `lake build` — H5iFs is a default target). `Core` (object graph +
   symlink-aware, fuel-bounded resolution + well-formedness), `Mount` (ordered
   mounts, `effectivePermission`, top-mount lemma), `Fd` (descriptors as
   capabilities, `closeForbidden` invariant, two proved lemmas). `Attacks`
   machine-checks six amplifiers by `decide`: #1 prevented by the component-
   path representation, #2 symlink escape, #3 fd smuggling, #5 mount-order
   shadowing, #6 hardlink amplification, #7 setup TOCTOU. `Setup` models the
   interleaved `SetupEvent` schedule and proves `runFrozen_sound` — the
   `setup_rejects_or_confines` result — over *any* attacker interleaving, with
   the check-then-use counterexample beside it. `Theorems` proves the central
   trace-level `every_effect_authorized` and the `integrity_outside_writable`
   corollary (the content half of the `ProtectedProjection`). Still deferred,
   and correctly so: #4 (parent-allow/child-deny) is a validator
   representability check (step 2), and the full `ProtectedProjection` beyond
   content lands with the validator's metadata handling.
2. **The mount manifest + validator.** Extend `EffectiveConfig` with ordered
   mounts and classes; write `validate` + `validate_sound`; run the real Lean
   checker over CI cases; the first two claims (`writes_confined`,
   `cache_readonly`) into the receipt. Exit: validator green on the corpus and
   on this repo's own profiles, `ValidatedPlan` typestate in place so `spawn`
   cannot bypass it.
   **Lean spec built, 2026-08-16** (`H5iFs/Validate.lean`): `EffectivePlan`
   (ro/rw grant paths + ordered mounts), `EffectivePlan.authority` (resolve
   each grant through the measured world into the induced object authority),
   `validate` (that authority is a policy subset), and `validate_sound`
   straight from `every_effect_authorized`. The `cache_readonly` core is two
   lemmas — `writable_only_from_rw` and `write_effect_needs_rw` (no write under
   a non-rw mount). Accept/reject are `decide`-checked on the `Attacks` world:
   a benign `/work` plan is accepted; a plan whose grant resolves through the
   planted symlink or hard link to the secret is rejected. **Remaining:** the
   Rust half — extend `EffectiveConfig`/mount manifest, `WorldEvidence` with
   its completeness witness, the `--validate` mode + Rust port, checker-level
   DRT, `ValidatedPlan` typestate, and the receipt fields (step 3).
3. **The Rust port + checker-level DRT.** Port `validate` to Rust behind the
   receipt; differential-test it against the Lean checker (`interferes_drt`
   pattern). Exit: zero mismatches over the sweep; receipt renders in
   `box status`.
   **Port + DRT built, 2026-08-16.** `h5i-spec --validate` exposes the Lean
   checker (JSON: `{policy, world, plan}` → verdict array).
   `h5i_sandbox::fs_authority` is the Rust port — `FsState`, symlink-aware
   fuel-bounded `resolve`, and `validate`, mirroring `Core`/`Validate` line for
   line — with unit tests for the accept/reject/loop cases.
   `tests/validate_drt.rs` diffs the port against the Lean checker over 200
   generated worlds (symlinks, aliases, loops) per run, green across seeds and
   wired into the Lean CI lane.
   **Production wire built, 2026-08-16.** The manifest already existed
   (`EffectiveConfig.binds` carry `BindKind` classes and order; `landlock.ro`/
   `rw`). `effective::validate_effective` re-checks the shipped config against
   the declared policy: `fs_subset`/`writes_confined` (the effective grants are
   the declared grants minus the exists-filter — a translation-validation that
   catches a `compute_effective` divergence), `cache_readonly` (the config-lock
   and cache-ro *overlays* stay read-only; private/home-state/cache-rw are
   writable by design), and `symlink_clean` (host measurement: no landlock
   grant and no bind source or mountpoint beneath the worktree canonicalizes
   out through a planted symlink — §VF.5; the bind mountpoints, where the
   config-lock and private binds sit, are the runc-class surface). A drift
   guard (`compute_effective_output_passes_the_validator`) runs
   `validate_effective` over `compute_effective`'s output in CI, so
   `declared_grants` cannot silently diverge. The
   verdict is recorded in the `EnvManifest` (`fs_authority`), rendered in
   `box status` (the `authorit:` line) and the console badge
   (`authority_unconfined` colors the verdict). The gate lives at the single
   spawn chokepoint (`build_confined_command`, after `compute_effective`) and,
   when active, **fails closed** on `!confined()` — an invariant a legit config
   always passes, verified by the real `home_bind_shadows` confined-run test —
   so a run cannot bypass validation. **Fully opt-in for now** (`enforce_enabled`,
   env `H5I_FS_AUTHORITY_ENFORCE=1`): with it unset — the default — the
   validator never runs (no host measurement, no manifest field, no gate, zero
   overhead), so default behavior is byte-for-byte as before this work. This is
   the §V4 gating discipline made explicit: the new gate earns trust opt-in
   before it ever becomes the default.
   **Honest gap:** the production check resolves via OS `canonicalize`, not the
   proved `FsState.resolve`; feeding a measured `FsState` through the proved
   `validate` (fully closing model↔production, and the finite-evidence
   completeness witness) remains future work, sampled meanwhile by the
   conformance probes (§VF.8).
4. **The mount realization audit + exec barrier.** Read-back diff against the
   `ValidatedPlan` before exec (VF.5), behind the stop/audit/go handshake,
   sharing the H5iFs semantics; abort-and-record on mismatch. Exit: a planted
   mount mismatch is detected and fails the launch on a real process-tier box
   (the probe harness, generalized).
   **Audit core built, 2026-08-16** (`mount_audit`): the pure parse +
   diff — `audit_mounts(expected, mountinfo)` returns `Missing` and
   `WritableButExpectedRo` mismatches, last-mount-wins per target (the §VF.2
   rule), with `expected_mounts(cfg)` deriving the plan from the bind manifest
   and `audit_pid(pid, ...)` reading a stopped child's `/proc/<pid>/mountinfo`.
   Five unit tests over synthetic `mountinfo` (consistent, ro-realized-rw,
   missing, stacked-topmost, narrower-ro-ok). **Remaining, and flagged as
   risky:** the stop/audit/go handshake in the live `pre_exec` path (`SIGSTOP`
   the child after setup, audit, send *go* or kill) — delicate exec-path
   surgery that needs a real process-tier box to validate, not landed here.
5. **`verified-cow` spike (optional), two stages.**
   *Stage 1:* read-only FUSE prototype over base+delta, executable core
   reusing `handle`; the agent can build and test but not write. Exit: source
   reads and a build run through the overlay.
   *Stage 2:* writable copy-on-write, single-queue serialized; agent edits
   land in the delta and surface in `box diff`; patch export behind
   `exported_effects_confined`. Exit: `box diff`/`box export` round-trips a
   real edit safely, and a hold/go decision on the Lean-vs-Verus boundary
   (VF.7) with the daemon's real shape in hand. No commitment past this
   without it.

Step 1 is the first thing worth writing up on its own, and the first that is
honest about being attack-driven rather than definition-driven. Everything
after earns its status line here when it is driven, in this document's usual
voice.

---

# The remote runner

Status: R13.1 built, 2026-08-16, on a design proposed and twice revised the
same day. R13.2 to R13.4 are not built; what R13.1 established, and the four
things building it found that the design had not, are recorded there. M17 is the milestone stub; these sections are the
authority on design and order. The design was drawn against two reference
codebases read in full for this purpose: the E2B spec repo (the envd
protobufs and OpenAPI, two client SDKs) and bhatti (a Go single-node microVM
sandbox service). R2 records what was taken and what was refused. The
same-day revision moved the design from "a runner is a machine with rootless
podman" to the capability model R1 now states, made the export quarantine a
real one (R9), replaced the runner's name with a cryptographic identity
(R6), and fixed an exit criterion that contradicted R12. A second pass the
same day separated `HELLO` from `PROBE` (static against dynamic, identity
riding in neither), made create crash-safe and idempotent (R7), gave R13.1
its failure-mode exits, and chased the last of the pre-identity wording out
of R6 and R13.4.

> **The box's boundary becomes a machine you own and can afford to lose. The
> product does not move: the repo, the policy, the credentials, and the patch
> gate stay here.**

## R1. Placement, not a tier

The idea arrived as "run h5i boxes on a Raspberry Pi". That framing is wrong
in a useful way: nothing in it is about the Pi. What it actually asks for is a
second axis on every box, *where it runs*, orthogonal to the tier it already
declares:

```
placement:  local | runner:<name>
isolation:  workspace | process | supervised | container | microvm
```

The rule that holds the axis together: **a runner requires Linux and the h5i
protocol, nothing else. Everything past that (isolation tiers, container
runtime, KVM, memory, storage, persistence, its own internet route) is an
advertised capability, and a capability the runner lacks is a refusal, never
a silent weakening.** A box that asks for `container` on a runner that
advertises only the kernel tiers fails with the capability named, exactly as
`IsolationRequest::Claim` refuses rather than downgrades today. There is no
fallback ladder across machines.

The MVP *builds* one cell: `runner × container`. The kernel tiers on a
runner are coherent, not a different product: the worker runs the same
`h5i-sandbox`, so Landlock, seccomp, and namespaces apply to a copied-in
workspace on the runner as well as they apply to a worktree here. What
defers them is real work, not principle: the kernel tiers assume the
worktree backend even locally (5.1 says so), so `runner × process` and
`runner × supervised` wait on a copy-in workspace path those tiers do not
have yet anywhere. `runner × microvm` waits until the container cell has
earned it. One honesty note for when they land: on a sacrificial runner the
tier protects the runner's *other* boxes and its own state machinery. The
machine boundary is what protects you, and a weak tier on a strong boundary
is a legitimate configuration for weak hardware, not a security downgrade of
the product.

A Pi is then nothing but a cheap instance of "a Linux machine with sshd",
and belongs in a demo, not in the design. No device class is named anywhere
in this part on purpose: the capability report, not the hardware, is the
vocabulary.

What this buys, stated as the security claim it is: the agent's execution
moves to hardware whose compromise you have priced in, while everything the
product refuses to expose (the working tree, the credentials, the receipts
store, the apply step) stays on the machine that never runs agent code. The
five components of section 2 are unchanged; the boundary of components 1 and
2 is now a network hop wide. And the honest converse, in the spirit of
section 9: this does not make the *box* harder to escape. It changes what an
escape reaches.

What this is not: a hosted sandbox service, a scheduler, a fleet. One
developer, machines they own, `~/.ssh` already knowing how to reach them.
Against Coder, Gitpod, or a self-hosted E2B the differentiator was never the
remoting; it is that the far end returns a reviewable patch and evidence, not
a live filesystem you trust by default.

## R2. Related work: take the wire shapes, refuse the planes

**E2B** (spec repo). Taken: the exec stream's discipline. A mandatory first
frame acknowledging the spawn, separate from output, so a short handshake
timeout can be cleared before the long stream timeout starts; input, resize,
and signals as separate calls addressed by process id rather than a
client-side stream; keepalive cadence declared by the client at request time
and echoed as frames in the same stream; capability gating by comparing the
peer's version against named constants instead of a negotiation handshake,
so the constants file doubles as the protocol changelog. Refused: the entire
plane. Control-plane REST, envd-in-guest HTTP, tokens minted at create,
Connect-over-HTTP framing. All of it exists because E2B's client and sandbox
meet across the public internet. Ours meet across an SSH session we already
authenticated.

**bhatti**. Taken: the agent frame protocol, nearly verbatim (R5); file
transfer reusing the same stdio frames instead of a second mechanism; create
errors that carry the tail of the far-side log, because a remote boot failure
with no log is the worst debugging position there is; server-side default and
maximum on every exec timeout; the shutdown posture that prefers an un-reaped
live box to an unrecoverable dead one. Refused: the resident daemon, the
bearer-token HTTP listener, the WebSocket TTY relay, the multi-user quota and
rate-limit machinery, the three-tier thermal state machine. One finding from
that codebase is load-bearing here: bhatti moved its internal API off
loopback TCP onto a unix socket after a sandbox reached the daemon's loopback
listener, and its CLI now silently prefers the socket. The forced command
over SSH stdio is the end of that trajectory: no listener anywhere, of any
kind, ever.

## R3. The cut: the worker is h5i

The tempting shape is a small `h5i-worker` that drives podman while the real
logic stays here. That cut is wrong three times over:

- **Argv is path-laden.** `container::build_run_argv` is pure, but it is full
  of local paths: the work dir, the spool, the preload script. Built here it
  reasons about another machine's filesystem. Built there it needs the
  policy-to-argv logic, and that logic *is* `h5i-sandbox`.
- **The egress proxy must run where podman runs.** The container tier wires
  `HTTPS_PROXY` to `HOST_ROUTE.host_addr`, the slirp4netns address that means
  "the machine podman runs on". If the far side runs the existing
  `container::run` path unchanged, the CONNECT proxy spawns on its loopback
  and every constant stays correct. The MVP therefore needs **zero egress
  redesign**: the allowlist compiled from the resolved policy is enforced on
  the runner by code that already exists and is already tested.
- **The binary is already the distribution.** Boxes exec
  `/usr/local/bin/h5i` today; "install h5i on the runner" is the same
  operational posture, and it removes a second cross-compiled artifact.
  This is an MVP decision, not a permanent constraint: the workspace is
  already feature-layered, so a slim worker build (the sandbox, the codec,
  and nothing web- or browser-shaped) is a cargo feature set away when a
  small-memory runner wants it. The protocol never learns the difference.

So the split is:

```
this machine (control plane)          runner (worker)
  repo, worktrees, env branches         the isolation backend it advertises
  manifests, policy resolution          the box volume (the only copy
  receipts store, the console             of the source over there)
  credentials, secrets broker           the egress CONNECT proxy
  export gate, apply                    a state dir with lease files
  h5i runner pair/probe/gc              h5i runner serve-stdio
```

The worker is the same `h5i` binary, one process per SSH session, stateless
across invocations: box state lives in podman and the state dir, not in a
daemon. On this side, placement is consulted at the three dispatch sites in
`crates/h5i-sandbox/src/sandbox.rs` (`run_with_env`, `spawn_background`,
`run_interactive`) *before* the tier match. No backend trait is invented for
this; two variants and three match arms, in the same spirit as
`IsolationClaim::image_backed` preferring properties over a registry.

## R4. Transport: SSH, a forced command, one session per RPC

The transport decision is mostly a list of things not built:

- **No custom listener, no TLS, no tokens.** The runner's `authorized_keys`
  gets one line: `restrict,command="h5i runner serve-stdio" ssh-ed25519 ...`,
  against a dedicated keypair generated at pair time. `restrict` kills shell,
  port forwarding, agent forwarding, X11, and pty allocation in one word.
  The key can do exactly one thing: speak our frames on stdio.
- **The client shells out to `ssh`**, it does not link an SSH library. That
  inherits the user's `~/.ssh/config`, agent, and ProxyJump. The invocation
  is pinned hard: the pair key with `IdentitiesOnly=yes`, a per-runner
  `UserKnownHostsFile` whose host key was recorded at pair time,
  `StrictHostKeyChecking=yes` forever after. That last pair of options is
  the mutual authentication the share ticket model was never designed to
  provide: we authenticate to the runner with the pair key, the runner
  authenticates to us with its pinned host key.
- **One SSH session is one RPC.** Concurrency is OpenSSH's ControlMaster
  multiplexing sessions over one TCP connection (about ten milliseconds per
  session against a warm master), not an in-protocol channel layer. This
  deletes request ids, channel numbers, and interleaving bugs from the MVP
  protocol entirely. A concurrent `box shell`, `env run`, and file pull is
  three sessions, each running its own short-lived worker process.
- **The pty rides in frames, not in SSH.** `restrict` disables pty
  allocation and nothing re-enables it; the worker allocates the pty around
  `podman exec` and forwards bytes and resizes as frames. One transport
  shape for everything.

WAN comes later and is not this transport: R12.

## R5. The frame protocol

bhatti's frame, kept because two hundred lines that survived production beat
anything designed fresh: `[u32 BE length][u8 type][payload]`, length excludes
the prefix, hard 1 MiB cap, every frame assembled in one buffer and written
with one write. JSON payloads for control types, raw bytes for stdio. The
codec module is transport-free, in the same discipline as `h5i-share`'s
`wire.rs`: testable over an in-memory pipe in a build with no SSH near it.

```
0x01 HELLO        0x02 HELLO_ACK      0x0E ERROR       0x0F KEEPALIVE
0x10 PROBE        0x11 CAPABILITIES
0x20 CREATE_BOX   0x21 DATA           0x22 DATA_DONE   0x23 CREATE_RESULT
0x30 EXEC         0x31 EXEC_STARTED   0x32 STDOUT      0x33 STDERR
0x34 PTY_OUT      0x35 STDIN          0x36 PTY_IN      0x37 RESIZE
0x38 SIGNAL       0x39 CLOSE_STDIN    0x3A EXIT
0x40 EXPORT_BOX   0x41 EXPORT_RESULT
0x50 DESTROY_BOX  0x51 LIST_BOXES     0x52 GC
```

The semantics worth writing down, each with its source:

- **`EXEC_STARTED` is the mandatory first frame** of an exec stream (E2B's
  `StartEvent`). "It spawned" and "here is output" are different facts; the
  first gets a short handshake timeout that is cleared when it lands, the
  stream then lives under the long timeout, and reads under an idle clock.
  Three clocks, never one.
- **`EXIT` carries what the receipt needs**: exit code, wall and cpu time,
  max RSS, and the `EgressSummary` from the worker-side `ProxyHandle`. The
  same struct the local path produces, so the receipt writer does not fork.
- **`ERROR` on create carries the tail of the worker-side log** (bhatti's
  lesson, bought with bug reports).
- **`HELLO` is static, `PROBE` is dynamic, and neither does the other's
  job.** `HELLO`/`HELLO_ACK` exchange what never changes within an install:
  protocol version, h5i version, arch. There is no negotiation; the lower
  protocol version governs and both sides gate features by named version
  constants, E2B-style, so a worker too old fails at probe time with the
  version in the message, not mid-create. Everything that drifts (memory,
  disk headroom, whether podman is present, the verified tiers, egress)
  belongs to `PROBE`'s `CAPABILITIES` reply and nowhere else.
- **Identity never rides in a frame.** `runner_id` is computed on this side
  from the host key the SSH handshake verified against the pinned
  known_hosts. The worker may echo it in `HELLO_ACK` as a sanity check, and
  the echo is never identity-bearing: a value the peer asserts about itself
  is exactly the thing pinning exists to make irrelevant.
- **File and bundle transfer reuse `DATA`/`DATA_DONE`** behind a JSON header
  frame, and `DATA_DONE` carries the SHA-256 the receiver must verify before
  acting on anything it received. No second transfer mechanism.
- **Limits are per RPC, not just per frame.** The 1 MiB frame cap bounds one
  message; nothing stops a peer streaming frames forever. Every RPC class
  carries a receiver-enforced total: bytes and wall time for a bundle or
  artifact transfer, bytes for an exec's captured output, object count where
  objects are what is being counted (R9). Like the exec timeout, the
  receiving side clamps to its own defaults and hard maxima; the sender's
  declared size is a claim, and the receiver aborts the RPC the moment the
  claim is exceeded.
- Commands are argv arrays end to end. A shell is something a caller asks
  for by name, never something the protocol implies.

## R6. Pairing, probing, and where runner config lives

```
h5i runner pair pi5 user@192.168.1.50
h5i runner probe pi5
h5i runner list | gc <name> | unpair <name>
```

`pair` does four things: generates the dedicated Ed25519 keypair into the
runner's state dir at mode 0600; installs the forced-command line, over
existing SSH access when the user has it, otherwise by printing the exact
line to paste; records the host key into the per-runner known_hosts file
(trust on first use at pair, strict forever after); and runs the `HELLO`
handshake and a first `PROBE`, storing the worker's version and its
capability report. Pairing succeeds
against **any Linux machine that speaks the protocol**: the only hard
failure is no `h5i` on the far side (with the install command in the error).
Everything else lands in the capability report:

```json
{
  "arch": "aarch64",
  "memory_mb": 512,
  "workspace_mb": 4096,
  "isolation": ["process", "supervised"],
  "container": false,
  "kvm": false,
  "persistent_boxes": true,
  "own_egress": true
}
```

Whether podman is present is this report's business, and `box create`'s to
enforce: a create naming a tier the runner does not advertise is refused
with the capability named, per R1. Pair records the report; it does not
judge it.

**Identity is the key, not the name.** `pi5` is a label, and a label can be
re-paired to a different machine tomorrow; digesting it into a manifest
binds the box to nothing. The runner's identity is
`runner_id = SHA-256(host public key)`, computed from the key pinned at
pair time. The manifest and every receipt record `runner_id`; the display
name exists for humans and command lines only. A reinstalled machine with a
fresh host key is a fresh identity, and that is correct: it *is* a
different trust anchor, whatever its label says.

**The account is part of the boundary.** The forced command's `restrict`
binds *our key*, not the machine: every other key, account, and sshd
setting is whatever the runner's admin left there. So pairing documentation
specifies a dedicated OS user, and `pair` offers to create it: no password
login, no sudo, no supplementary groups, no access to anything secret on
the runner, a clean environment, and the forced command by absolute path.
`probe` warns on the violations it can see from the far side. None of this
is enforcement h5i can promise; all of it is the difference between "the
pair key is constrained" and "the account is", and the docs must not
conflate the two.

Runner config is **host-scoped, never in the repo**. `.h5i/env.toml` is
checked in; which machines *this* developer can reach is a fact about this
machine, exactly like the user egress allowlist, and lives beside it. A
profile may later carry a human-facing runner *label*; the label resolves
to `runner_id` before the manifest is authored, and only `runner_id` is
identity-bearing and digested. The label and the resolved endpoint stay out
of every digest, in the same way `ResolvedPolicy` keeps runtime state out
of the pinned digest today.

`probe` is `box probe` one machine over: the worker runs the existing local
probes (`container::probe`, kernel capabilities, disk headroom on the state
partition) and returns the same `capabilities_report` shape under the
runner's identity. For **every isolation tier the runner advertises**, probe
must end by running `verify_exec` functionally: a throwaway container where
`container` is claimed, a confined exec where the kernel tiers are. A
runner that advertises less probes clean with less; a runner whose
advertisement its own kernel cannot back gets the advertisement corrected,
loudly. Present bits are not a working confined exec; this codebase has
paid for that lesson once already and the probe is where it stays paid.

## R7. Create: copy in, one machine over

Remote create is section 5.1 implemented at distance, and it *dissolves* the
hardest local problem instead of carrying it: the identical-path git-plumbing
binds exist only because a local box shares the host repo's worktree inodes.
A remote box shares nothing, so they simply do not apply.

1. Create first checks the request against the runner's capability report:
   a tier the runner does not advertise, a workspace larger than
   `workspace_mb`, a resource floor above `memory_mb`, each is a refusal
   with the capability named. The stored report is a cache of the last
   `PROBE`; the client-side check exists for good error messages, and the
   worker refusing at create time is the enforcement. Then the front half of `env::create` runs
   unchanged: pin `base_commit` and `base_tree`, create the env branch,
   write the manifest. No worktree. The manifest grows `runner_id` (R6)
   beside `backend`, with the display name stored beside it for humans: the
   box is bound to the machine, not to the label.

   **Corrected, 2026-08-16.** An earlier draft said "inside the digested and
   validated field set". There is no digest *over* `EnvManifest` — its four
   `*_digest` fields are digests of other artifacts that it pins. The set that
   exists is the one `validate_imported_manifest` enumerates, and `runner_id`
   belongs in its object-id loop beside `base_commit`, `base_tree` and
   `policy_digest`: a 64-character hex check, fail-closed, rather than being
   left to `sanitize_display` on the way to a terminal.
2. This side builds a **git bundle**: `base_commit` (shallow allowed, as the
   `clone:` source already accepts) plus, when the box starts from dirty
   state, one synthetic commit of that state. A bundle rather than a tar
   because the bundle *is* the base identity, verifiable on receipt, and
   incremental when a later phase re-syncs.
3. `CREATE_BOX` carries the box id, image, limits, the serialized resolved
   policy, and the bundle digest; the bundle follows as `DATA` frames. The
   worker verifies the digest and materialises the bundle into a box-owned
   directory, never a bind mount of anything on the runner.

   **Corrected, 2026-08-16, by building it.** This step said the worker "runs
   the existing warm-container create". There is no such thing: the container
   tier is `podman run --rm` per command and has no warm form at all — the
   create-once/exec-many design exists only on the microvm tier
   (`build_create_argv`, `build_exec_argv`, `guest_name`). So a remote create
   makes the box — the source, the policy, the lease — and the container is
   made when there is something to run in it (R13.3). That is also the better
   shape for the hardware this is aimed at: a warm container idling on a small
   runner costs memory for nothing. When it lands, `microvm::guest_name`'s rule
   is the one to copy — the container's name is a digest of its own create
   argv, so a config change forces a fresh one by construction.
4. `CREATE_RESULT` echoes **the digest of the policy the worker actually
   enforced**, and this side refuses to mark the box live unless it matches
   `policy_digest`. Cheap, and it converts "the worker silently ran an older
   policy" from a possibility into a detected fault.

Create is crash-safe by state, not by hope. The worker builds under
`creating/<operation_id>` and an atomic rename to `live/<box_id>` is the
one moment a box exists; there is no state in between for a crash to
invent. A re-sent `CREATE_BOX` whose request digest matches an existing box
returns the existing result (bhatti's idempotent create, with the marker),
so "the worker finished but the response never arrived" costs a retry, not
a duplicate; a matching id with a different digest is refused. Orphaned
`creating/` entries carry a short fixed TTL of their own and fall to the
normal sweep, because a lease nobody ever refreshed is exactly what an
interrupted create leaves behind.

Secrets keep the microvm tier's argv discipline: nothing secret in remote
argv or environment visible in the runner's process table. In the MVP that is
enforced the simple way; see R12.

## R8. Exec and shell

`env::run` and `env::shell` reach the placement check and become an `EXEC`
RPC: argv, cwd, the already-filtered env, an optional pty size, and a
timeout that the worker clamps to its own default and hard maximum. The
worker runs the existing `container::run` or `run_interactive` against the
warm container; output streams back as `STDOUT`/`STDERR` frames, or `PTY_OUT`
when a pty was asked for; `STDIN`/`PTY_IN`, `RESIZE`, and `SIGNAL` flow
forward on the same session. Pty against pipes is one flag on the same RPC,
discriminated by frame type; in pty mode there is no `CLOSE_STDIN`, there is
Ctrl-D, because that is what a terminal is.

Disconnect semantics, stated so nobody discovers them: the **container**
survives a dropped session (it is a detached warm container); the **exec**
dies with its session, which is what happens locally when h5i is killed
mid-run. Reattachable execs are a later capability the frame layout already
leaves room for, and R12 keeps them there.

Concurrency rules, stated for the same reason. Worker invocations are
separate processes, so the lock is a file lock in the box's state dir, in
the spirit of the share gate `export::export` already holds: `CREATE_BOX`,
`DESTROY_BOX`, and `EXPORT_BOX` take it **exclusive**; `EXEC` takes it
**shared**. An export attempted while execs hold the lock is refused with
the live execs named, because an export racing a build reads a torn tree
and a torn tree that passes validation is worse than a refused RPC. Nothing
waits silently; every refusal says who holds the lock.

## R9. Export: quarantine the objects, author the commit here

Export is the trust boundary, so this section is the careful one. The good
news is that `env::diff` already has a no-worktree branch, diffing
`base_tree` against the env branch tip through the object store; it was built
for boxes whose worktree is elsewhere, which is now literally the case.

1. `EXPORT_BOX`: the worker commits the box's current tree in the runner-side
   clone and returns a bundle of `base_commit..tip`, an archive of the
   exportable untracked artifacts, and its receipt spool.
2. This side unpacks the bundle into a **throwaway bare repository with its
   own object database**, never directly into the host repo. A ref
   namespace is not a quarantine: fetching writes the untrusted objects
   into the shared object store, and a ref only quarantines reachability.
   The throwaway repo gets `git bundle verify`, `transfer.fsckObjects`, and
   the structural checks that only make sense before anything is trusted:
   total bundle size and object count against the R5 RPC limits, a blob
   size ceiling, path length, symlink and hardlink entries flagged for the
   scans below, and no tree entry that traverses (`..`, absolute, or
   `.git`, on top of what fsck already refuses). Objects from a machine we
   have agreed may be compromised never enter the host repo's object
   database before validation; the same posture the `clone:` source takes
   toward a PR, made literal.
3. The host takes the **tip tree, not the commits**. The mediated-commit
   scans (`scan_nested_git`, the staged-path allowlist, the private-path
   skips) run against the `base_tree` to fetched-tree diff inside the
   throwaway repo, violations are filtered, and only then are the surviving
   tree's objects materialized into the host repo and written as **one
   host-authored mediated commit** on the env branch. The runner's history
   and authorship are discarded by construction: the host repo only ever
   contains commits the host itself wrote, and only objects a passed scan
   reached. This needs `mediated_commit` refactored to accept a tree source
   instead of a worktree, and that refactor is the single invasive change
   to existing code in this whole part.
4. Downstream is untouched: `PROPOSED`, `export::export`'s bundle, the apply
   gates, patch-mode squash. A remote box that cannot complete the fetch
   degrades to exactly the detached-box posture that already exists:
   export-only, no apply.

## R10. Evidence: the runner-observed lane

The two-axis honesty model already contains the right question. A remote
execution observed by the worker is host-observed *from the runner's point of
view*, and it arrives here over a wire. Folding it into `HOST_OBSERVED_LANES`
would overclaim: a compromised runner kernel can forge it. Calling it
box-claimed would underclaim: the box cannot edit it, and the channel it
arrived on is mutually authenticated with pinned keys.

So it is a third thing with an honest name: **`runner-observed`**. Observed
from outside the box, by an h5i we authenticated, on hardware we do not
control. The console renders it as its own tier between the two, and the
limits section gets one sentence that is the entire security claim of this
part: *runner-observed collapses to box-claimed exactly when the runner host
is compromised, and the runner host is the machine you chose to be able to
lose.* The `Grade` axis is unchanged and still orthogonal.

Receipts are written on this side, into the same append-only store, from the
`EXIT` and `EXPORT_RESULT` payloads. No signing is added, because none exists
locally either and a signature from a machine the threat model already
sacrifices is not evidence; the 5.7 fd-collector direction is the honest
future for both placements at once.

## R11. Lifecycle without a daemon

No resident process on the runner means nothing is there to watch a clock,
so the reaper is opportunistic:

- Every box carries a **lease**: a file in the runner state dir and a label
  on the container. Default TTL two hours, hard TTL twelve, refreshed by any
  RPC that touches the box.
- **Every worker invocation reaps expired boxes before doing its own work**,
  the same sweep-on-entry pattern `sweep_invalid_worktree_registrations`
  uses, plus an explicit `h5i runner gc`.
- Reaping stops the container, snapshots a partial export bundle and the
  receipt spool into the state dir, and deletes after a grace window. The
  bhatti posture holds: when the snapshot fails, keep the box and say so.
  An un-reaped live box beats an unrecoverable dead one.
- There is no heartbeat protocol, because there is no daemon to keep alive.
  "Disconnect grace" is trivially infinite for the container and zero for
  the exec, and both of those are the behaviors R8 already chose.

Persistence is a capability, not a requirement. A `persistent_boxes: true`
runner keeps containers and state across disconnects and reboots; a
`persistent_boxes: false` runner (read-only OS, tmpfs workspace, one
microSD) loses every box at reboot, and the protocol treats that as a lease
that expired early: the next contact reaps the record, and anything not yet
exported is honestly gone. Same protocol, same lifecycle, different
advertised storage. Separate filesystems for OS and box storage, so a box
that fills its disk takes the state partition and not the machine, is the
recommended shape on persistent runners: a pairing-time check with a
warning, not something h5i can enforce.

## R12. What the MVP refuses, and what comes later

Refused, fail-closed, with the reason in the error:

- **Profiles that need the secrets broker or the auth proxy.** Both exist to
  keep secret values on this machine; shipping the values to the runner to
  keep the feature working would invert the point. The later design is a
  credential channel: a dedicated long-lived session carrying muxed
  connections from the runner-side proxy back to the auth proxy here, so
  real credentials still never leave. That channel is the one place a mux
  enters the protocol, which is exactly why it is not in the MVP. Until it
  exists, **no agent that needs model credentials runs on a runner**, and
  R13's exit criteria are written accordingly.
- **Any request past the runner's advertised capabilities**, per R1: a tier
  it does not advertise, a workspace it cannot hold, a persistence it does
  not have. The MVP worker advertises `container` only; the kernel tiers
  and microvm join the advertisement when their milestones land, and until
  then asking for them is this same refusal.

Assumed, and stated so it is priced: **the MVP runner has its own outbound
internet**. Image pulls and package installs leave through the runner's own
CONNECT proxy under the box's allowlist (`own_egress: true` in the
capability report). A runner with no default route, the cable-only
appliance configuration, is not a supported MVP topology; it becomes one
when brokered egress lands below, and pretending otherwise earlier would
just move the failure somewhere quieter.

Deferred with their shape already known:

- **Brokered no-network egress.** The container gets no network at all; its
  only egress is a proxy whose upstream is the credential channel above.
  Raw sockets then fail closed instead of bypassing the CONNECT proxy, which
  closes the limitation section 9 states for the container tier today. When
  it lands it lands for *local* boxes too; the runner is just where the
  design pressure came from.
- **WAN transport over iroh.** `h5i-share`'s wire module says ALPN is where
  two protocols are meant to disagree before either speaks; a runner ALPN
  beside the share ALPN, with the pair keys doing authentication above it,
  reuses the existing QUIC stack without touching the ticket model. The
  runner dials out, so no router configuration.
- **The kernel tiers on a runner.** The blocker is the copy-in workspace
  path those tiers lack even locally (R1); when 5.1's copy-in lands for
  them, `runner × process` and `runner × supervised` are an advertisement
  change plus their probe, and they open the door to runners too small for
  a container runtime.
- **Reattachable execs** (bhatti's sessions, E2B's `Connect`), **runner
  pools**, and **re-sync of a live box's source** (the bundle transfer is
  already incremental-capable).

## R12b. What an adversarial review of the built code found

Eighteen rounds against the branch, 2026-08-17, with the threat model the
rest of this part states: **the runner may be compromised**, so the
interesting direction is runner to host. Thirty-seven findings, all fixed.
The two that mattered most were both things the design had reasoned about
correctly and the code had not implemented that way.

**A box could execute on the runner host.** The export staged a box's work
by running `git add` inside the box's own repository. git executes
`core.fsmonitor` and `filter.<name>.clean` out of repository config; the box
owns that config because its workspace is bind-mounted rw; and
`core.hooksPath=/dev/null`, which this module did set, covers neither. Any
box with a shell could run a command as the runner user: a complete escape
from the thing this part exists to contain. The fix is not another flag: it is not
invoking the CLI in a tree whose configuration is hostile. libgit2
implements neither mechanism, and the thin bundle is written from a packfile
rather than by `git bundle create`.

**A refspec is not a limit on what a fetch writes.** git follows tags by
default, so a bundle crafted by a compromised runner put an attacker-named
`refs/tags/*` and a runner-authored tag object, with a chosen tagger and a
chosen message, into the host repository, on the success path, past every check
the quarantine performs. R9's sentence about the host only ever containing
commits it authored was false for tags. `--no-tags` and
`--no-write-fetch-head` close it.

Three more were gaps between what a section said and what the code did:

- **The capability gate checked a field that decided nothing.** `create`
  validated the tier a request *declared*, while `run_with_env` dispatches on
  the tier its *policy* carries, so a box could be declared, recorded and
  displayed as `container` and run every command unconfined.
- **R12's refusal of credential-bearing profiles was written down and never
  implemented.** Values never crossed, since a grant carries a name and a
  source descriptor, but the runner resolves those descriptors against its own
  environment, so a box could be handed the runner's credential in place of
  the user's.
- **The SSH argv's claim not to depend on `~/.ssh/config` was false.** No
  `-F`, and `GlobalKnownHostsFile` unpinned while ssh consults both host-key
  files. A hostile config redirected every RPC to another machine with the
  pin apparently intact. That breaks the attestation, not merely the
  transport, because `runner_id` is what a manifest and a receipt record.

The rest were smaller and are in the log: a frame cap that governed whole
sessions, so any command with real output failed; a blob ceiling that
inflated the object it was measuring and failed open; a worker with no clock;
a `doctor` that probed this machine about a box confined elsewhere; a `diff`
that answered "changed nothing" for work that had not come home; and a
`create` that failed deterministically for any repository declaring a
persona.

Several of the fixes were themselves wrong, which is the part of this worth
generalising. Two were caught by the tests they broke: an idle timeout
polling under a buffered reader, and a budget restored on only the
successful path. Three more survived until a round was spent reviewing the
*fixes* rather than the code, and one of those was the second-worst finding
of the whole review:

- **The move to libgit2 was half a fix.** It closed the half of the
  hostile-config class that *executes* commands and left the half that
  *redirects*. libgit2 honours `core.worktree`, so a box could point the
  export's staging at any path the runner user can read and have another
  box's workspace packed into its own bundle and shipped to the owner; and a
  `.git` file carrying a `gitdir:` pointer makes the export commit into a
  different repository. The lesson is narrow and worth keeping: "this library
  does not run commands" answers a smaller question than "this library does
  not act on hostile configuration".
- **One fix's commit message described work its diff never did.** The
  `authorized_keys` check was claimed to match whole lines and did not; the
  branch that claimed to refuse was unreachable. A false claim in a commit
  message is worse than the bug, because it is what the next reader trusts.
- **One fix reverted an older one.** Setting `service_digest` to `None` for a
  runner box re-armed the legacy-env sentinel a previous security fix had
  closed, under a comment still asserting the invariant held.

That is the argument for the fuzz harnesses this round added over the codec
and the worker's state machine, and for spending a round on the fixes rather
than only on the code. Reviewing a patch is not the same activity as
reviewing a system, and the second one does not subsume the first.

## R13. The order

Each step is small enough to land alone and each has an exit that is a
demonstration, not a diff.

- **R13.1 Pair and probe.** New crate `crates/h5i-runner` beside `h5i-share`
  (codec, typed messages, client, worker loop, and a `Transport` trait with
  `SshTransport` and `ChildProcessTransport`), feature-gated like `share`;
  `src/cli/runner.rs` on the `share.rs` template; `serve-stdio` in the same
  binary. Exit: `pair` then `probe` against a real second machine returns a
  capabilities report with a functional `verify_exec`, and the whole
  handshake also runs in CI with no sshd via the child-process transport.
  The exit is as much the failure modes as the happy path, tested where
  the child-process transport makes them cheap: an oversized frame, a
  truncated frame, an unknown frame type, a message out of order, an RPC
  total-byte limit exceeded, a `HELLO` that never arrives, a version
  mismatch, capability values that are hostile or absurd (clamped or
  refused, never stored), and a disconnect mid-transfer that leaves
  nothing behind. A codec born with its failure modes tested does not
  acquire them later as bug reports.

  **Built, 2026-08-16** (`crates/h5i-runner`, `src/cli/runner.rs`,
  `tests/runner_protocol.rs`): 92 unit tests and 17 integration tests, the
  latter against the real binary over a real process boundary. Pairing,
  probing, listing and unpairing all run end to end over real SSH against
  a real sshd, and the security properties were measured rather than
  assumed. With the pair key: a shell request returns nothing, and a
  forwarded port carries no bytes while the same forward on an
  unrestricted key returns the sshd banner — `restrict` is doing what the
  section claims. The `SHA256:` fingerprint h5i prints is byte-identical
  to `ssh-keygen -lf` on the machine, which is the only check pairing's
  trust-on-first-use ever gets, and it is a test rather than a hope.
  Session-per-RPC is cheap as R4 assumed: five multiplexed sessions in
  39 ms, about 8 ms each, against 343 ms each without a master.

  Four things the build found that the design had not:

  - **The watchdog kills a child, not a process group.** A reader
    unblocks when the last holder of the pipe's write end closes it, so a
    child that leaves a grandchild holding it keeps blocking past the
    kill. Both real transports are single-process by construction, so the
    kill is sufficient — but it is a property of *those transports*, not
    of the watchdog, and it is now written where the next transport will
    read it.
  - **The receiver's budget is the one that governs, and it is not the
    format's.** A control session refuses at 256 KiB, well under the
    1 MiB frame ceiling. Both sides of that boundary are pinned, because
    a cap is where an off-by-one lives.
  - **`CARGO_PKG_VERSION` in a library is the library's version.** The
    worker reported `0.1.0` to an operator running h5i 0.3.4, in the one
    field whose whole job is answering "which h5i is over there". The
    binary now supplies its own.
  - **A control socket path can be too long to be a socket.** Unix socket
    paths cap around a hundred bytes and a deep `$XDG_CONFIG_HOME`
    exceeds it, so multiplexing is declined rather than guessed at when
    it would not fit. Losing latency is better than an obscure OpenSSH
    error on someone else's machine.
- **R13.2 Create and destroy.** Bundle transfer, digest verification,
  leases and `gc`, the `creating/` to `live/` state machine and idempotent
  re-send (R7). Exit: `box create --runner`, `box ls` showing placement,
  destroy and gc leave the runner clean, a kill -9 of the client mid-create
  leaves only a `creating/` entry the next invocation reaps, and re-sending
  the same create after a lost `CREATE_RESULT` returns the same box instead
  of a second one.

  **Worker side built, 2026-08-16** (`boxstore`, `source`, the create,
  destroy, list and gc handlers, `h5i runner boxes|destroy|gc`): 139 unit
  tests and 24 integration tests against the real binary, plus an opt-in
  `H5I_TEST_RUNNER_SSH` test that runs the whole cycle over real SSH — a
  repository bundled here, streamed across an SSH session, checked out on
  the far side at the pinned commit, re-sent idempotently, destroyed. The
  source is a `git bundle` and neither side pollutes a branch namespace to
  build or read one; `git clone` cannot be used because it only sees
  `refs/heads/*`, so building a bundle would mean creating a branch in the
  repository we are supposed to be only reading. Bundles carry full history:
  `git bundle create` has no `--depth` (checked against git 2.43), and this
  is the first thing to revisit when the transfer becomes the slow part.

  **Complete, 2026-08-17.** `h5i box create --runner <name>` places a box on
  a paired runner: the base is pinned, the branch created and the policy
  resolved and digested here, the source goes across as a bundle, and the
  manifest records `runner_id` — the runner's host-key hash — beside a
  display name that is never identity. `box ls` shows `on=<runner>`, and
  `box rm` removes both sides. Verified against a real sshd end to end: the
  box's source arrives at the identical commit, `h5i runner boxes` shows it
  from the runner's own side, and removal clears both.

  The seam is a trait, `h5i_core::placement::RemoteRunner`, implemented in
  the binary over `h5i-runner`. `h5i-core` gets no dependency on the runner
  protocol, which matters because a later milestone will want the *worker*
  reaching for receipts and export — a dependency the other way would be a
  cycle waiting to happen. It also makes the remote create path testable in
  `h5i-core` against a fake that opens no connection.

  Operations that need a local workspace refuse a runner box by name rather
  than failing on a missing directory, because a message about a directory
  sends someone looking for a bug that is not there.

  Five things building it found:

  - **The container tier has no warm form**, which R7 assumed it did. See
    the correction there.
  - **A budget has to be per RPC.** A handshake is bytes and a bundle is
    megabytes, and one budget covering both has to be as loose as the looser
    of the two. `FrameReader::begin_rpc` resets the limits and the counters
    together; a reader whose bound silently changed with the traffic would
    have no bound at all.
  - **Reading a peer's stderr means waiting for the drain thread.** A child
    can write its diagnosis and exit while the draining thread is still
    between a `read` and the buffer, so reading the buffer without joining
    returns an empty string exactly when the message matters most. It passed
    locally for a whole milestone before more work made the race visible.
  - **`rm` has to remove this side first.** The tidier-looking order — clear
    the runner, then the local record — is wrong, because `rm` refuses a live
    box: the runner's copy was destroyed and the user was then told the
    removal had failed, leaving a local record pointing at nothing. Local
    first means the only remaining failure is an orphan on the runner, which
    is exactly what a lease is for. Found by running it, not by reading it.
  - **The effective baseline is about a local invocation.** It describes the
    Landlock grants and binds a kernel-tier run would apply on *this*
    machine, against a work directory a runner box does not have here.
    Computing one would be describing a confinement nobody is going to
    enforce.
- **R13.3 Exec.** Captured and interactive, the three clocks, the per-box
  locks, receipts in the `runner-observed` lane with the worker's egress
  summary. Exit: a real project's build and test suite runs on the runner
  from `env run` under its egress allowlist, `box shell` is usable over a
  deliberately laggy link, and the receipt log shows the lane and the
  egress evidence. Deliberately **not** an agent: an agent profile needs
  model credentials, R12 refuses to ship them, and an exit criterion that
  contradicts R12 is how the credential channel would end up rushed. The
  agent-on-a-runner demonstration belongs to the credential channel's own
  milestone.

  **Captured exec built, 2026-08-17.** `h5i box run <box> -- <cmd>` runs on
  the runner and comes home with an exit code, timings and the runner's own
  egress summary, filed under `runner-observed` — its own lane in `Signals`,
  neither host-observed nor box-claimed, for the reason R10 gives. The
  worker calls `sandbox::run_with_env` directly, which is the R3 cut paying
  off: the confinement that runs there is the product's own, not a
  reimplementation. Per-box locks are `flock` (create/destroy/export
  exclusive, exec shared), so an export beside a running build is refused
  rather than reading a torn tree — and the kernel releases the lock with
  the process, which is what stops a worker killed mid-exec from wedging a
  box forever. Verified over real SSH: output returns, exit codes
  propagate, receipts land.

  **Two pieces of this milestone are NOT built, and neither is disguised.**

  - *Output is captured, not streamed.* `run_with_env` is the function the
    local path calls and it captures; a long build says nothing until it
    finishes. The frames are already the right shape — `EXEC_STARTED` goes
    out before the run, and output arrives as `STDOUT`/`STDERR` chunks — so
    a streaming runner inside `h5i-sandbox` would send the same frames
    earlier. That is surgery on the local path's most load-bearing
    function and was deliberately not bundled in here.
  - *`box shell` on a runner does not work.* Interactive means a pty, and a
    pty means bidirectional streaming, resize and signals — the
    `PTY_IN`/`PTY_OUT`/`RESIZE`/`SIGNAL` frames are declared and refused.
    The "usable over a laggy link" half of the exit criterion is therefore
    **not met**, and stays open rather than being quietly reworded.
- **R13.4 Export. Built, 2026-08-17.** Exit met: a change made through
  `box run` on the runner round-trips to a host-authored mediated commit,
  survives the violation scans, and applies through the unchanged gates —
  demonstrated end to end against a real sshd, including a planted nested
  repository that is refused fail-closed with the branch left untouched.

  Two things turned out to be much smaller than the plan assumed, and one
  larger:

  - **`diff` and `apply` needed no changes at all.** `diff` already picks
    an object-store branch when there is no worktree, and every gate in
    `apply` is object-store work. Once the fetched tree lands on the env
    branch, the whole downstream is the local path unchanged.
  - **The `mediated_commit` refactor was not needed either.** The tree
    arrives already committed by the worker, so what this side needs is not
    a tree-source variant of a function that stages a worktree — it is the
    scans, run against a tree, and a commit. Those live in
    `h5i_core::quarantine`, and the local `mediated_commit` is untouched.
    R13's scope valve therefore never had to be pulled.
  - **The quarantine was the real work**, and it is the part R9 cares
    about. The bundle is unpacked into a throwaway *bare* repository with
    its own object database — a ref namespace withholds reachability, not
    presence, so it is not a quarantine — and the structural checks run
    there: object and entry counts, a blob ceiling, path length, traversal,
    nested `.git`, and gitlinks the base did not have. Only then does the
    surviving tree cross, carried by a commit that is discarded on arrival
    so that **this side authors the commit**. Verified: the runner's own
    carrier commit appears nowhere in the host repository's history.

  The bundle home is **thin** (`base..tip`), which is why the quarantine is
  seeded with the base from the repository we own before the untrusted
  bundle is fetched. An export therefore costs what was *done* in the box
  rather than what the history weighs — the asymmetry the outbound
  direction cannot have, because the far side starts with nothing.

  (Superseded exit text, kept for the record: a change made through `box
  shell` or `env run` on the runner round-trips to a host-authored mediated
  commit, survives the violation
  scans (a planted nested-git and a private-path write are both filtered and
  named), and applies through the unchanged gates.

Decision points, named not resolved, in the VF.7 discipline:

1. **The lane name.** `runner-observed` as a third lane string, against
   overloading `Grade` to express transport trust. The third string is
   recommended: the two axes are orthogonal today and should stay so.
2. **The runner in the digest.** Digesting `runner` binds a box permanently
   to its runner name and forecloses migrating a box between runners without
   export and re-create. Recommended anyway: identity over convenience, and
   export/re-create *is* the migration story this product believes in.

   **Answered, 2026-08-16, by dissolving the premise.** The digest never
   holds the name; it holds `runner_id`, the hash of the pinned host key
   (R6). That keeps the binding (to the machine, which is what the digest
   was reaching for) and drops the false one (to a label anyone can
   re-point at different hardware). The migration answer is unchanged:
   export and re-create.
3. **R13.4's scope valve.** The tree-source refactor is the only invasive
   change; if it fights back, the MVP ships export-only (the detached-box
   posture) and apply lands behind the refactor later. Nothing upstream of
   R13.4 depends on which way this goes.

   **Answered, 2026-08-17: never needed.** The refactor was predicated on
   this side having to build a tree from a worktree it does not have. It
   does not have to: the worker commits, and what arrives is a tree. So the
   work was the quarantine and the scans, `mediated_commit` was left alone,
   and apply landed with the rest. The valve was never pulled because there
   was nothing to valve.

---

# Runtime detection: a kernel-observed lane

Status: designed and built, 2026-08-19. Sections D1 to D14. M18 is its
milestone stub; these sections are the authority on design and order.

The confinement layer answers "what was the box *allowed* to do". This part
answers a different question — "what did it actually *do*" — and answers it
from a place the box cannot reach. Everything here is additive: no policy
decision changes, no syscall is ever blocked by this code, and a host that
cannot run it loses nothing it had.

## D1. What is being claimed

The claim is exactly one sentence, and it is deliberately narrow:

> For a run whose receipt carries a `runtime` block with `coverage = "full"`,
> the listed detections are the ones that fired on events the **kernel**
> reported for that box's processes, and `events_lost = 0` means no event was
> dropped between the kernel and the record.

What is **not** claimed:

- Not that the list is complete for the *behaviour*. A signature only fires on
  what it models. A box that does something nobody wrote a rule for produces a
  clean detection list and a nonzero event count, and the record says so by
  carrying both numbers rather than a verdict.
- Not that it is enforcement. Nothing here can deny anything. Denial is
  Landlock, seccomp, the netns and the egress proxy, and it stays there (D12).
- Not that it survives a kernel-level adversary. A box that already has
  CAP_SYS_ADMIN on the host kernel can unload the programs. h5i's boxes do not
  have it, and if one did, the eBPF lane is not the thing you lost.
- Not that absence of the block means the run was clean. Absence means the
  detector did not run, and the block is written even when it could not
  attach, carrying the reason.

## D2. The lane problem this fixes

h5i already sorts its evidence into lanes, and the sorting is load-bearing:
`host-env-run` is what h5i itself observed by being the parent process,
`tee-shim` is what a shim *inside* the box wrote to a spool, `shell-egress` is
what the CONNECT proxy refused, `runner-observed` is what a paired machine
reported over an authenticated channel (R10). The receipt keeps them
distinguishable forever because they are not equally trustworthy.

Reading down that list, the honest summary of what h5i can see inside a box
today is:

| lane | who observed it | what it covers | what defeats it |
|---|---|---|---|
| `host-env-run` | h5i, as parent | argv, exit code, rusage, wall clock | nothing — but it sees only the *outermost* command |
| `tee-shim` | a shim in the box | interactive shell commands | `exec` without the shim, a script, any child that does its own work |
| `shell-egress` | the CONNECT proxy | HTTP(S) the box routed through the proxy | anything that dials a socket directly |
| `browser` | the browser in the box | console, page errors, failed requests | closing the browser |
| `runner-observed` | the paired worker | the same as the above, one machine over | the same as the above |

The gap is a single shape repeated four times: **every lane above either sits
at the boundary of the box or lives inside it.** The boundary lanes see the
first process and the traffic that chose to go through h5i. The in-box lane
sees what the box chose to report. Between them sits everything an agent's
build actually does — the four hundred processes `npm ci` forks, the
`postinstall` that reads `~/.aws/credentials` because the profile granted the
directory, the test that dials a hardcoded IP because `net.mode` is `proxy`
and the proxy only ever sees names.

A kernel-observed lane closes that shape rather than one instance of it. The
kernel sees every `execve` whether or not a shim wrapped it, every `connect`
whether or not it spoke HTTP, and every `openat` whether or not the opener
wanted to be seen. It is the first h5i lane that is neither at the boundary
nor inside the box, and it is the only one that cannot be defeated by the
box declining to cooperate.

That is the auditability argument, and it is worth stating what it buys
concretely, because "more visibility" is not a feature:

1. **Grants that are wider than the behaviour.** `fs_read` on `$HOME` is a
   grant; `openat("$HOME/.aws/credentials")` is a fact. A profile can now be
   tightened against what the box *used*, not against what someone guessed.
2. **The proxy's blind spot.** `net.mode = "proxy"` promises an allowlist; it
   delivers an allowlist *for clients that use the proxy*. On the workspace
   tier there is no netns, and a direct `connect(2)` to a literal address goes
   nowhere near it. That is a limit SECURITY.md states and nothing observed.
   Now something does.
3. **The shim's blind spot.** `tee-shim` is box-claimed by construction and
   the roadmap has always said so. Now there is a second opinion on the same
   run from a lane that is not.

## D3. Related work: Tracee and Tetragon, and what not to take

Both references solve this problem at a scale h5i does not have, and both
carry design decisions that are right for a cluster agent and wrong here.

**Tracee** (`../../Ref/tracee`) is the closer relative: a syscall-centric
collector with a signature engine on top, and its events-plus-signatures
split is exactly the shape adopted here (D7, D9). What is taken:

- The split between a **collector** that knows only about events and a
  **signature layer** that knows only about semantics. Rules never touch a
  ring buffer; the collector never knows what a credential file is.
- The insistence that a dropped event is reported, not smoothed over. Tracee
  counts losses per buffer and surfaces them; the receipt here carries
  `events_lost` next to `events_seen` for the same reason a truncated raw
  payload is marked truncated.
- Argument capture at `sys_enter` with an explicit, bounded string budget,
  rather than chasing pointers into user memory without a cap.

What is refused:

- **The event catalogue.** Tracee instruments hundreds of events, has a
  policy language to select among them, and needs CO-RE plus a full BTF
  toolchain to do it. h5i instruments twelve tracepoints and no more (D5, D7).
  A detector that costs a second toolchain is a detector nobody builds.
- **The daemon.** Tracee runs as a service and streams. h5i has no daemon by
  design (R11 argued the same thing for the runner), and the unit of
  observation here is a run, not a host.

**Tetragon** (`../../Ref/tetragon`) contributes one idea and one warning. The
idea is **process-lineage-as-first-class**: an event is not interesting on its
own, it is interesting because of the process tree it sits in, so the tree is
maintained in the kernel rather than reconstructed by racing `/proc` in
userspace. That is exactly the scope mechanism in D6, and the reason it is a
kernel-side map instead of a userspace `procfs` walk: by the time userspace
reads `/proc/<pid>`, a short-lived `postinstall` script is already gone.

The warning is enforcement. Tetragon can kill a process from a hook, and its
documentation is careful about the race between observing and acting. h5i does
not take that: enforcement stays in the mechanisms that fail closed by
construction, and this lane is observation only (D12). A detector that
sometimes blocks is a policy layer with unclear semantics, and h5i already has
a policy layer with clear ones.

## D4. Why aya, and why the probe is C

**The loader is `aya`** (`../../Ref/aya`). It is pure Rust: no `libbpf`, no
`libelf`, no `bindgen`, no C toolchain at *link* time, and no new
cross-compilation story for the musl and Darwin targets the release matrix
already builds. The alternatives were `libbpf-rs` (drags in libbpf, libelf and
zlib as native link-time dependencies, which the aarch64-musl `cross` target
would have to grow) and hand-rolling `bpf(2)` (about two thousand lines of
ELF parsing and map plumbing that aya has already had reviewed).

**The probe itself is C**, compiled by `clang -target bpf` in the crate's
build script, and this is the decision most likely to be questioned, because
aya has a perfectly good Rust eBPF frontend. It is C for three reasons:

1. `aya-ebpf` requires a **nightly** toolchain and the `bpf-linker` binary.
   h5i builds on stable, and `dtolnay/rust-toolchain@stable` is in every CI
   job. Adding a nightly toolchain plus a cargo-installed linker to the build
   of an *optional* observability feature is a poor trade.
2. The probe is ~350 lines of straight-line code with no allocation, no
   generics and no error handling — the part of the system where C's
   disadvantages are smallest and its toolchain's ubiquity is largest.
3. Every reference implementation writes its probes in C, so the code is
   reviewable against them line for line.

The build script is honest about the toolchain rather than demanding it. No
`clang` that can target BPF means the object is not built, the crate still
compiles, and the loader reports `unavailable` with the reason "built without
the eBPF object". `H5I_BPF_REQUIRE=1` turns that into a build failure, which is
what this lane's CI job sets, in the shape `H5I_DRT_REQUIRE` already
established for the Lean lane.

The released binaries do **not** carry the probe, and that is stated rather
than left to be discovered. The release matrix cross-builds musl targets inside
containers with no LLVM, and putting a BPF-capable clang into four images — to
ship a feature that *also* needs `CAP_BPF` on the user's machine — is work that
should follow somebody wanting it rather than precede them. `h5i box detect
probe` reports the consequence in one line and prints the
`cargo install --path . --features bpf` that fixes it.

## D5. No CO-RE: the stable-ABI cut

CO-RE (Compile Once, Run Everywhere) exists because reading kernel structures
from a probe is not portable: `task_struct` changes shape between kernels, so
libbpf rewrites field offsets at load time using the running kernel's BTF.
Every reference implementation depends on it, and it costs a `vmlinux.h`
generated by `bpftool` at build time (three megabytes of generated header),
BTF at runtime, and a relocating loader.

h5i does not pay any of that, because of a deliberate cut:

> **The probe reads no kernel structure.** It reads only syscall tracepoint
> arguments, which are a stable kernel ABI, and calls only helpers whose
> signatures are stable.

Concretely, everything the probe touches is on this list and nothing else:

- The `syscalls/sys_enter_*` tracepoint context, whose layout is fixed
  (`u64 pad; long id; unsigned long args[6];`) and is the documented,
  ABI-stable format for every syscall entry tracepoint.
- The `sched/sched_process_fork` and `sched/sched_process_exit` contexts,
  read through their published field offsets, which the loader **verifies at
  attach time** by parsing `/sys/kernel/tracing/events/.../format` rather
  than assuming. A kernel that moved a field is refused, not misread.
- `bpf_get_current_pid_tgid`, `bpf_get_current_uid_gid`,
  `bpf_get_current_comm`, `bpf_ktime_get_ns`, `bpf_get_current_cgroup_id`,
  `bpf_get_ns_current_pid_tgid`, `bpf_probe_read_user`,
  `bpf_probe_read_user_str`, `bpf_ringbuf_reserve/submit/discard`,
  and the map accessors. All stable since 5.8 at the latest, which is the
  floor the loader checks for (ring buffer support) and the floor stated in
  the limits.

What the cut costs, stated up front: no `task_struct` walking, so no parent
`comm` without keeping it ourselves, no cgroup *path* (only the id), no
mount-namespace inode, no file inode on `openat` (only the path string the
caller passed, which is a caller-controlled string and is labelled as such in
the record). Those are real losses. They buy a probe that loads on any kernel
from 5.8 to whatever ships next, with no build-time kernel headers and no
runtime BTF, which is the difference between a feature that works on a user's
WSL2 kernel and one that works on the maintainer's laptop.

## D6. Scope: which events belong to which box

The hard problem in a per-run detector is not collecting events; it is knowing
which of the host's events are the box's. Getting this wrong in the permissive
direction reports the user's own editor as box activity, and getting it wrong
in the restrictive direction misses the interesting child.

Three mechanisms were considered. **One is implemented**, and the reason the
other two are not is a single constraint that is worth stating plainly, because
it is not obvious until you try:

> The scope has to be decided **before the payload exists**. A scope programmed
> after the child is spawned has already missed the `execve` that named it,
> which is the most valuable single event of the run.

- **cgroup id** (`bpf_get_current_cgroup_id`) is exact, cheap and immune to pid
  reuse, and it is unusable here: the run's cgroup is created *inside* the
  spawn path (`sandbox::make_run_cgroup`), so it does not exist when the scope
  must be programmed. On most hosts it does not exist at all — cgroup
  delegation is unavailable without a systemd user manager that grants it, and
  `cgroup.rs` says so at length.
- **pid namespace** (`bpf_get_ns_current_pid_tgid`) has the same defect for the
  same reason, one level up: the inode comes from `/proc/<pid>/ns/pid` of a
  process that has not been forked yet.
- **The process tree** is the one thing that *is* knowable in advance, because
  h5i is already running. This is the Tetragon idea (D3): lineage maintained in
  the kernel rather than reconstructed by racing `/proc`, because by the time
  userspace reads `/proc/<pid>` the forty-millisecond `postinstall` is gone.

So the scope is `pidtree`, seeded with **every task of the h5i process** (all of
them, not just the main thread: `Command::spawn` can be called from any thread,
and a tree seeded with one would miss a payload spawned from a worker). The
kernel grows the set on `sched_process_fork` and prunes it on
`sched_process_exit`.

Seeding from h5i's own tree leaves two holes, and the probe's state machine
closes both. They are worth describing because each was a wrong answer first:

1. **h5i's own threads are not the box.** A new task forked from something in
   the set is `PENDING` until its first event, and that event settles it: a
   task whose tid equals its tgid leads its own thread group and is therefore a
   *process* — the payload, or something the payload spawned — while anything
   else is one of h5i's threads and is marked `SELF` and never reported again.
   That test is exact, costs one comparison, and needs no kernel structure.
2. **h5i's own bootstrap is not the box either.** Between the fork and the
   exec, the child is still running h5i's `pre_exec` code: applying Landlock,
   opening the ruleset paths, setting rlimits. Attributing those `openat`s to
   the box would report h5i's confinement machinery as the box's behaviour. So
   a task in the tree is `PRE` until its `execve`, and in that state only the
   exec itself (and the tree bookkeeping) is emitted. A child *inherits* its
   parent's post-exec state, so a fork-only worker — Python multiprocessing, a
   shell subshell — is not silently muted for never having execed.

The `mode` field in the config map is where a cgroup or namespace filter would
go, and the probe is written so that adding one is additive. The natural
consumer is the privilege-separated collector of D13.1, which attaches out of
band and *can* therefore resolve either. Nothing in v1 uses it, so nothing in
v1 ships it.

**Coverage.** The tiers are not equally covered, and the record says so per run
rather than in a footnote:

| tier | coverage | why |
|---|---|---|
| workspace | `full` | the payload is a direct descendant of h5i |
| process | `full` | same, plus everything it spawns |
| supervised | `full` | same, and the supervisor is in the tree too |
| container | `partial` | Podman's `conmon` double-forks and reparents, so the workload leaves h5i's tree; what stays visible is the runtime's own activity on the host |
| microvm | `none` | the workload runs against a *guest* kernel; a host probe cannot see its syscalls at all, and pretending otherwise would be the worst available failure |
| anything else | `none` | an unknown tier is uncovered, never assumed covered — guessing permissively is the one mistake that turns an absence of evidence into a clean bill of health |

`partial` and `none` are written into the receipt as facts, each with its
reason attached. A reviewer reading a container-tier record sees
`coverage: partial` and the sentence explaining it, which is the difference
between "we looked and found nothing" and "we could not look".

One honest consequence of seeding from h5i's own tree: any process h5i spawns
*during the run window* is inside the scope. On the kernel tiers that is the
payload and nothing else, because the window opens immediately before
`run_with_env` and closes immediately after it. On the container tier it is
also the container runtime, which is exactly why that tier is `partial` rather
than wrong.

## D7. The event model and the wire format

Twelve tracepoints, one fixed-size event struct, one ring buffer. The struct
is `#[repr(C)]` on the Rust side and a plain `struct` in the probe, with a
compile-time assertion on each side that they agree in size, plus a runtime
magic-and-version word in every event so a mismatched pair is detected at the
first record rather than silently misparsed.

The events, and the syscalls behind them:

| kind | source tracepoints | captured |
|---|---|---|
| `Exec` | `sys_enter_execve`, `sys_enter_execveat` | path, first argument, argc |
| `Open` | `sys_enter_openat`, `sys_enter_openat2` | path, flags, write-intent bit |
| `Connect` | `sys_enter_connect` | family, IPv4/IPv6 address, port |
| `Socket` | `sys_enter_socket` | family, type, protocol |
| `Ptrace` | `sys_enter_ptrace` | request, target pid |
| `Bpf` | `sys_enter_bpf` | command |
| `Nsop` | `sys_enter_unshare`, `sys_enter_setns` | flags |
| `Module` | `sys_enter_init_module`, `sys_enter_finit_module` | — |
| `Memfd` | `sys_enter_memfd_create` | name |
| `Mount` | `sys_enter_mount`, `sys_enter_pivot_root` | target path |
| `Fork` | `sched_process_fork` | child pid |
| `Exit` | `sched_process_exit` | — |

Every event carries `ts_ns`, `pid`, `tgid`, `ppid` where known, `uid`,
`comm[16]`, and one 256-byte payload area interpreted per kind. Fixed size
throughout: a variable-size ring buffer record would need a second length
field the verifier has to be convinced about, for a saving that does not
matter at these volumes.

**Volume control lives in the kernel, not in userspace.** `openat` is the
loudest syscall a build makes, and shipping every one of them to userspace to
throw away 99% is how a detector becomes a performance problem people turn
off. So the probe filters `Open` in-kernel to two cases: write intent
(`O_WRONLY|O_RDWR|O_CREAT|O_TRUNC|O_APPEND`), or a path whose first bytes
match one of a small set of prefixes loaded into a map from userspace. The
prefix set is the credential-path list the signatures care about (D9), pushed
down so the rule's own vocabulary decides what the kernel sends.

## D8. The ring buffer, loss, and back pressure

`BPF_MAP_TYPE_RINGBUF`, 256 KiB by default, read by a dedicated thread that
`poll(2)`s the map fd and hands decoded events to the session over a channel.
The buffer size is a policy knob because a `cargo build` and a `sleep 1` do
not need the same buffer.

Loss is counted, never hidden. A `bpf_ringbuf_reserve` that fails increments a
per-CPU counter in a second map; the session reads that counter at stop time
and puts it in the record as `events_lost`. A run with a nonzero
`events_lost` is not a failed run and not a clean one — it is a run whose
detection list is a lower bound, and the console renders it that way.

The reader thread is bounded by the run: it starts before the child spawns,
stops when the run returns, and is joined with a timeout so a wedged reader
can never outlive the command it was watching. Its channel is bounded too;
when userspace cannot keep up, the *channel* drops and counts, so a slow
consumer degrades the same way a full kernel buffer does, into a number in the
record.

## D9. The signatures

A signature is a pure function from an event stream to zero or more
detections. No I/O, no clock, no allocation per event beyond what it stores,
and therefore unit-testable against synthetic streams — which is how all of
them are tested, since attaching a probe needs privileges CI does not have.

Seventeen rules ship, in five families (`net`, `secret`, `exec`, `priv`,
`kernel`, `mount` — the last two are one family of concern split by what they
name). Each has a stable id, a severity
(`info`, `notice`, `alert`), a one-line human description, and a bounded
exemplar list so a flood becomes a count rather than a megabyte.

**Network** — the family that matters most, because it is the one the egress
proxy structurally cannot see:

- `net.direct-egress` (**alert**) — `connect(2)` to a routable address on a
  box whose network policy is an allowlist or a denial. This is the allowlist
  being routed around, and on the workspace tier (no netns) it is the *only*
  thing that would notice. It reports the **attempt**: the probe sees the
  syscall going in, not the answer coming back, so a connect a network
  namespace refused looks exactly like one that succeeded. On a
  `net.mode = deny` box that is the useful reading — the box tried.
- `net.raw-socket` (**alert**) — `AF_PACKET`, or `SOCK_RAW` on any family.
- `net.unix-socket` (**notice**) — `AF_UNIX` connect on a profile that did
  not set `unix_sockets`. The supervised tier denies these already; on the
  other tiers nothing did.
- `net.dns-direct` (**notice**) — connect to port 53 or 853.

**Credentials and secrets** — the family the filesystem grants cannot express,
because a grant is a directory and a credential is a file inside it:

- `secret.read` (**alert**) — an open of `.ssh/`, `.aws/credentials`,
  `.config/gh/`, `.git-credentials`, `.netrc`, `.kube/config`,
  `.docker/config.json`, `.npmrc`, `.pypirc`, `.gnupg/`.
- `secret.dotenv` (**notice**) — an open of a `.env`-family file outside the
  workspace.
- `secret.proc-environ` (**alert**) — an open of `/proc/<pid>/environ` for a
  pid outside the box, which is the classic read of the host's secrets.
- `secret.h5i-state` (**alert**) — a write-intent open under the box's own
  `.h5i/` control directory, which is the box editing its own evidence.

**Execution** — the family that catches the shape of a supply-chain payload:

- `exec.from-tmp` (**notice**) — exec of a path under `/tmp`, `/var/tmp` or
  `/dev/shm`.
- `exec.memfd` (**alert**) — `memfd_create` followed by an exec of
  `/proc/self/fd/<n>` or `/proc/<pid>/fd/<n>` in the same process: fileless
  execution, and the reason `Memfd` is collected at all.
- `exec.interpreter-pipe` (**notice**) — a shell exec whose first argument is
  `-c` and whose command line contains a download-and-pipe shape.
- `exec.package-manager` (**info**) — npm/pip/cargo/gem/go invoked; not
  suspicious, and present because "what installed things" is the first
  question asked of any supply-chain incident.

**Privilege and kernel** — the family that is uninteresting until it is not:

- `priv.ptrace` (**alert**) — any `ptrace` attach to a process the box did
  not spawn.
- `priv.namespace` (**notice**) — `unshare`/`setns`. The supervised tier
  denies `unshare` outright; this is what the other tiers get.
- `kernel.bpf` (**alert**) — the box calling `bpf(2)`.
- `kernel.module` (**alert**) — `init_module`/`finit_module`.
- `mount.change` (**notice**) — `mount`/`pivot_root` inside the box.

Rules are data, not code paths: the engine holds a table, and `h5i box detect
rules` prints it, so what the detector looks for is inspectable without
reading Rust.

## D10. Where it lands

**The receipt.** A new optional `runtime` block on `ExecRecord`, appended last
and `skip_serializing_if` empty, so every existing record's shape and every
pinned digest is unchanged — the discipline `unix_sockets`, `loopback_ports`
and `engine` established on `Profile`. It carries the lane string
(`kernel-bpf`), the scope kind, the coverage, `events_seen`, `events_lost`,
the detections, and `unavailable` with a reason when the detector could not
attach. `source` on the record itself does **not** change: the run is still
`host-env-run`, and the kernel lane is a block inside it, because the record
is about the command and the block is a second observer of the same command.

**The console.** `h5i ui` gains a runtime row per record, badged by the
highest severity present, grey when the detector did not run. This obeys the
console's honesty model (`box-console-honesty-model`): it is counting over
receipts, not scoring, and grey means "no evidence", never "clean".

**The export.** The export report renders the detections for every record it
carries, and an export whose records have `coverage: none` says that in the
report rather than showing an empty list.

**The CLI.** `h5i box detect probe` (what this host can do, and the exact
command to fix what it cannot), `h5i box detect rules` (the table), and
`h5i box detect show <name>` (the detections across a box's records, with
`--json`).

## D11. Policy surface

```toml
[profile.agent.detect]
enabled = true      # attach the probe for runs under this profile
require  = false    # refuse to run when the probe cannot attach
buffer_kb = 256     # ring buffer size
rules = ["*"]       # rule ids or families to enable; "*" is all
```

Four fields, all optional, all appended last on `Profile` so no existing
profile's canonical serialization or pinned digest moves. `enabled` defaults
to false: turning on a kernel facility for every user by default, when it
needs privileges most users have not granted, would produce a fleet of
`unavailable` blocks and teach everyone to ignore them.

`require = true` is the fail-closed switch and it means what it says: if the
probe cannot attach, `h5i box run` refuses, with the probe's reason. That is
the setting for the "I am running somebody else's dependency tree" case, and
it is off by default because the failure mode of a mandatory detector on a
laptop kernel is a tool that does not start.

### D11.1. Opt-in, at three layers, and the one that is easy to get wrong

"Is this optional?" has to have exactly one answer, and it takes three
defaults to give it one:

| Layer | Switch | Default | What it decides |
|---|---|---|---|
| build | `h5i/bpf` → `h5i-core/bpf` → `h5i-bpf/load` | **off** | whether the binary carries aya and a compiled probe at all |
| host | `CAP_BPF` + `CAP_PERFMON` | not granted | whether it can attach |
| policy | `[profile.X.detect] enabled` | **false** | whether a given box is watched |

What is *not* optional is the evidence types: `h5i-core` depends on `h5i-bpf`
unconditionally with `default-features = false`, so a build with no collector
can still read and render a receipt written by one that had it. A feature flag
that changed a serialized record's shape would make yesterday's evidence
unreadable after an upgrade, which is a worse failure than the one it saves.

The subtle layer is the **crate's own default**. `h5i-bpf` was written with
`default = ["load"]`, so that the main `clippy --workspace --all-targets` job
would lint the loader. Cargo unifies features across a workspace build, so the
consequence was that `cargo build --workspace` pulled aya and ran a clang
invocation for every contributor, while `cargo install --path .` did not —
"optional" had two answers depending on how you built. The default is now `[]`,
and the dedicated CI job passes `--features bpf` explicitly, which lints and
tests the same code without making it arrive uninvited.

## D12. What it refuses to do

- **No enforcement, in any form.** No `bpf_send_signal`, no
  `bpf_override_return`, no LSM programs. Not "not yet": a detector that can
  block has to answer for the gap between observing an argument and the
  kernel using it (the TOCTOU that makes syscall-argument enforcement unsound
  in general), and h5i has a policy layer that does not have that gap. The
  two must not be confused, and the way to keep them unconfused is that this
  one has no verb.
- **No BPF LSM.** `CONFIG_BPF_LSM=y` is common but `lsm=…,bpf` on the kernel
  command line is not, so an LSM-based collector would be unavailable on most
  hosts including this one. Syscall tracepoints work everywhere.
- **No CO-RE, no `vmlinux.h`, no BTF requirement** (D5).
- **No daemon, no persistent attachment.** The probe is loaded for a run and
  unloaded when the run ends. Nothing survives the command.
- **No privilege escalation of its own.** h5i does not `sudo`, does not
  install a helper, and does not ask for setuid. It uses the capabilities the
  process already has, and when it does not have them it says which ones and
  how to grant them.

## D13. Limits, stated up front

1. **It needs `CAP_BPF` and `CAP_PERFMON`** (or root). h5i runs as an ordinary
   user, so on a stock install the answer is `unavailable: missing CAP_BPF`
   plus the one-line `setcap` command. A privilege-separated collector — a
   tiny setcap'd helper that owns the probe and streams events over a socket,
   so the rest of h5i keeps no capabilities — is the right long-term shape and
   is **not built here**. The seam for it is real rather than aspirational:
   `Watch` is one type for "watching" and "could not watch", every caller goes
   through it, and the probe's config map already carries a `mode` field for
   the cgroup and namespace scopes such a collector could resolve (D6). What
   would change is `session.rs` and nothing above it.
2. **Linux 5.8 or newer**, for `BPF_MAP_TYPE_RINGBUF`. Older kernels get
   `unavailable`, never a silent fallback to perf buffers.
3. **`sys_enter` arguments are the caller's, not the kernel's resolution.** A
   path in an `Open` event is the string the process passed; symlinks,
   relative paths against an `openat` dirfd, and races between the argument
   read and the kernel's use of it are all unresolved. Every rule that matches
   on a path is therefore a *heuristic over caller-supplied strings*, and the
   record labels the path field as such. Argument capture at `sys_enter` is
   what makes the probe CO-RE-free, and this is the price.
4. **The container tier is `partial` and the microVM tier is `none`** (D6).
5. **Pid reuse can, in principle, admit a foreign process** into a `pidtree`
   scope, between an exit h5i has not yet seen and a fork the kernel reuses
   the pid for. The window is a single scheduler quantum and the mitigation
   (a per-pid generation counter) costs more than the exposure is worth for
   an observation-only lane. Stated, not fixed.
6. **A box with CAP_SYS_ADMIN on the host kernel defeats it.** No h5i box has
   that; the sentence exists so nobody has to work out whether it does.
7. **`sys_enter` sees attempts, not outcomes.** The probe is on the way *in* to
   the syscall, so a `connect` the network namespace refused, an `openat` that
   returned `EACCES` and a `ptrace` the kernel denied all look exactly like the
   ones that succeeded. Attaching `sys_exit` as well would fix this and would
   double the event volume for a distinction that, on a *confined* box, is
   usually the less interesting half: "the box tried to reach 8.8.8.8" is the
   finding, and whether Landlock or the netns stopped it is already answered by
   the policy. The rules that could be misread because of this say so in their
   own text.
8. **The read-only `openat` feed is filtered in the kernel**, to write intent
   plus a bounded set of path prefixes plus a `/.env` scan (D7). A read of a
   credential path nobody listed is not collected and therefore cannot fire a
   rule. `[detect] open_all = true` removes the filter and is honest about what
   it costs: a `cargo build` produces six figures of `openat`.

## D14. The order

1. **D14.1 — The crate skeleton.** `crates/h5i-bpf`: probe, event model,
   rules and evidence types, all pure Rust, compiling on every target in the
   release matrix. No aya yet. *Exit: `cargo clippy --workspace --all-targets`
   green on Linux and Darwin; the rules engine unit-tested against synthetic
   event streams.*
2. **D14.2 — The probe.** `bpf/h5i_detect.bpf.c` plus the build script that
   compiles it when `clang` can target BPF, stubs it when it cannot, and hard
   fails under `H5I_BPF_REQUIRE=1`. *Exit: the object builds on this host and
   `bpftool`-less verification that its sections and maps are what the loader
   expects.*
3. **D14.3 — The loader.** aya session: load, verify tracepoint formats, program
   the scope, attach, read the ring buffer, stop and account. Linux only, and
   behind `h5i-bpf/load`, which is **off by default like every other switch in
   this lane** — see D11 on why the crate default is the one that is easiest to
   get wrong. The dedicated CI job is what asks for it, and therefore what lints
   and tests it. *Exit: `detect probe` reports the host truthfully — including
   "missing CAP_BPF" on an unprivileged one — and the live attach test passes as
   root.*
4. **D14.4 — The run seam.** Wire the session around `sandbox::run_with_env`
   in `env run` and `env shell`, resolve the scope per tier, and put the block
   in the receipt. *Exit: a run under a `detect`-enabled profile carries a
   `runtime` block; a run on a host without the capability carries the block
   with its reason; `require = true` refuses.*
5. **D14.5 — The surfaces.** Policy parsing, `h5i box detect` verbs, the
   console row, the export report, `box status`, `box capabilities`, MANUAL,
   SECURITY, and the generated manuals regenerated. *Exit: the docs job is
   green, which is the only way the CLI and the manual can agree.*

### D14.6. What was demonstrated, and what was not

All five steps are built, 2026-08-19. Stated precisely, because "built" and
"demonstrated" are not the same word:

**Demonstrated.**

- The probe compiles with `clang -target bpf -O2 -g -Wall -Werror` and produces
  the seventeen tracepoint programs, the five maps, the `license` section and
  `.BTF`. `tests/detect_integration.rs` builds it under `H5I_BPF_REQUIRE=1`,
  which is the setting that turns "no clang" into a build failure.
- The wire contract is held from both ends: a compile-time size assertion on
  the Rust struct, a magic-and-version check on every record, and a test that
  parses the C header and compares every constant and every event-kind number
  against the Rust enum. A third test parses the probe source and fails if it
  declares a tracepoint program the loader's attach table does not name.
- The rules engine is tested against synthetic event streams — every rule
  fires, and a table-driven test fails if a rule is listed in the catalogue and
  unreachable from `observe`. Both directions of each judgement call are
  covered: a LAN address *is* egress, loopback is not, the proxy's own endpoint
  is not, a granted `unix_sockets` profile is not reported for using them, a
  box reading *its own* `/proc/<pid>/environ` is not reported.
- The end-to-end wiring is tested on a host with **no** `CAP_BPF`, which is the
  common case and the one most likely to be got wrong: a profile that did not
  ask carries no block, a profile that asked carries a block with the reason,
  `require = true` refuses the run and names the setting, a misspelled rule id
  surfaces in the receipt, and enabling detection changes the pinned policy
  digest.

**Not demonstrated.** The attach itself. Loading a program and binding it to a
tracepoint needs `CAP_BPF` and `CAP_PERFMON`, which this machine's h5i does not
have and which `cargo test` should not acquire. That path lives in
`crates/h5i-bpf/tests/live_attach.rs`, behind `H5I_BPF_LIVE=1`, and it skips
*loudly* — printing the reason — rather than passing quietly. Until somebody
runs it on a host with the capability, the honest claim is: the probe compiles,
the loader is written against a pinned aya, the verifier has not seen it.

Two specific things that first run is checking, both stated so a failure is
diagnosable rather than surprising:

1. **The verifier's opinion of the `openat` program**, which is the big one at
   roughly ten thousand instructions after the prefix loop and the `/.env` scan
   are unrolled. Well inside the one-million limit, and the largest unverified
   thing here.
2. **The tracepoint field offsets**, which the loader checks against
   `/sys/kernel/tracing/events/.../format` when that file is readable. It
   usually is not (tracefs is root-only), in which case the documented layout
   is used and the check is skipped — never silently, the probe report says
   `tracefs = no` and what that costs.

---

# Part 6 — The forum

Sections T1 to T12. Built 2026-08-20 on branch `zero-trust`.

## T1 The claim

h5i's first half is one contained box. This is the second: several of them,
working on the same repository, without the containment becoming decorative the
moment they talk to each other.

The one-liner is *zero-trust collaboration for agent teams*, and the invariant
underneath it is:

> Agents can share information, never permissions.

Stated so it can be checked rather than admired: **a message may change what a
peer decides; it can never change what that peer's sandbox is able to do.**

## T2 The threat this exists for

A single agent's sandbox bounds a single agent's blast radius. Put three agents
in three sandboxes and let them talk, and the bound quietly stops holding — not
because any sandbox failed, but because authority *composed*:

```
hostile input
   ↓
agent A is influenced
   ↓  a message, an artifact, a shared file
agent B acts on it
   ↓  using B's own grants, which A never had
the effect is A's intent with B's authority
```

No escape happened. Nothing was exploited. A persuaded B, and B was allowed to
do the thing. This is the failure mode a sandbox per agent does not address, and
it is the only one this part is about.

What follows from that framing, and is worth being explicit about because it is
the difference between a claim we can keep and one we cannot:

**We do not claim to detect a hostile message.** No classifier, no
prompt-injection filter, no moderation. Those are all attempts to make the *text*
safe, and the text is not the thing under our control. What is under our control
is what a persuaded agent can then reach — and the answer is: exactly what it
could reach before the conversation, because nothing on this path carries a
capability.

## T3 What was already built

The scope cut of 2026-08-05 (§3.2, M1) removed `msg`, `team`, `radio` and the
orchestra crate. It did **not** remove the confinement-side plumbing those
things used, which is still in the tree, still tested on every tier, and had
been sitting with no writer at the other end:

| seam | where | state before this part |
|---|---|---|
| read-only inbox | `env::prepare_env_inbox`, `BOX_INBOX_MOUNT` | mounted on every tier, tested, never written to |
| the box's write window | `env::ingest_shell_spool`, `$H5I_ENV_CAPTURE_SPOOL` | drained after every session, two record families |
| identity injection | `env::team_binding`, `team_identity_env` | reads two files, nothing wrote them |
| concurrent ref append | `refstore` (CAS + jittered backoff + union merge) | live, used by `refs/h5i/env/meta` |

So the forum is not a reconstruction of what was cut. It is a writer for seams
that already exist, plus a store, plus a surface. That is why it is small.

## T4 The shape: file-mediated, not networked

A box has exactly two forum-shaped holes, and they are the two above:

```
box A                    host                     box B
  /.h5i/inbox  ←──── tender ──── refs/h5i/forum ──── tender ────→  /.h5i/inbox
  spool/       ────→                                       ←────  spool/
```

**No socket, no port, no token, no HTTP.** This was a deliberate reversal of the
obvious design (a small local service with per-box bearer tokens). The obvious
design has a credential in every box; this one has nothing to steal and nowhere
to connect. The strongest access control available here turned out to be the
absence of an API, and it costs less code than the alternative rather than more.

## T5 Identity: the box writes *what*, never *who*

The staged record has fields for a thread, a kind, a body, and attachments. It
has **no field** for a sender, a role, a box id, or a policy digest — those are
stamped by the host from the env directory the record was found in.

This is the same rule the deleted `team.rs` wrote down, and it is kept verbatim
because it is the cheapest enforcement in the system: a field that does not
exist in the wire format cannot be forged. A record containing
`"sender": "human"` is not rejected; it is simply not read, and the post lands
attributed to whichever box staged it.

Host-side binding is two files in the env directory — outside every grant the
box has — consumed by the injection path that already existed. A box can be told
who it is, and can never tell itself something else.

## T6 The ceiling: refused, never downgraded

A thread names a profile every participant must be confined **under**. At attach,
the box's enforced policy — its digest-verified `policy.resolved.toml`, not a
profile re-resolved from a worktree an agent could have edited — is checked as a
subset across every dimension that widens reach: net mode and egress, secret
grants, authenticated egress, fs read and write, AF_UNIX, loopback ports, and
host-side secret extractors.

Two decisions inside that, both taken against the more obvious alternative:

**Static, not a live intersection.** Computing each participant's authority as
the intersection of everyone currently in the room is safe and unusable: an
observer joining would strip write access from the agent doing the work, and a
long task would not be reproducible hour to hour. A ceiling fixed by a human at
creation, checked once per box, gives the same guarantee with none of that —
and because a box's resolved policy cannot change while it exists, one check at
attach holds for the box's whole life.

**Refused, not re-confined.** A box over the ceiling is turned away rather than
quietly weakened to fit. Same reasoning as `placement`: a capability the other
side cannot satisfy is a refusal, never a silent downgrade. Attaching has to
keep meaning "runs the way you configured it".

## T7 Liveness, and why there is still no daemon

R11 records that h5i has no resident process by decision. The forum does not
change that, and the reasoning is worth stating because a message forum is
exactly the kind of thing that usually demands one.

**Host side.** A box that is running already has a host process supervising it —
holding its run lock, owning its egress proxy. The tender is a thread inside
that process, started with the session and stopped with it. Nothing is
installed, nothing outlives the run, and there is no second lifecycle. A box
that is not running has nothing to deliver to.

**Box side.** `h5i forum wait` blocks on a directory the box already has
mounted. No hook, no `settings.json` edit, no runtime-specific integration —
which matters because the two runtimes h5i targets do not have the same hook
surface, and because a coordination layer that needs the user to install
something is one most users will not install.

The honest cost: an idle box's inbox goes stale until something runs in it or a
human touches the forum. For collaborating agents — running, by definition —
that gap does not arise. If it ever does, the fix is a foreground
`h5i forum serve` looping the same function, a sibling of `h5i ui`, and not a
background daemon. Deliberately not built yet (T12).

## T8 Storage: one ref per thread

```
refs/h5i/forum/meta            roster.json
refs/h5i/forum/threads/<id>    thread.json + posts.jsonl + attach-<digest>
```

Git refs rather than the workspace's first SQLite dependency: the concurrent
append machinery already exists and is tested, and the union merge that
reconciles `refs/h5i/env/meta` across clones has the same shape here.

**Correction, 2026-08-20.** This section originally claimed cross-clone sync came
"for free". It did not: `union_merge_thread` and `union_merge_roster` had no
callers, and neither did `env`'s own `union_merge_commits` — the push/pull that
used it was cut in M1. The forum was single-machine, and the merge was code
nobody ran. T13 is what makes the claim true.

Per-thread rather than one shared log, which was the first design and was wrong:
appending rewrites the blob it appends to, so a single log means every post
rewrites the whole forum's history and reading one conversation means parsing
all of them. Per-thread refs bound both costs by the size of one thread,
localise CAS contention to the thread being posted to, make the thread list a
ref enumeration whose tip timestamps are the activity order, and let `close`
keep one conversation's history from being rewritten by traffic in another.

`posts.jsonl` is strictly append-only, which is what makes union merge sound.
Thread *status* is therefore a projection over the posts, never a stored field —
the same event-sourced shape `team.rs` used, and the reason nothing has to be
mutated and nothing can disagree with the log.

## T9 Refusals are recorded, not swallowed

A revoked box's post is posted **carrying its refusal**, not dropped. An
oversized body is truncated and says so; an attachment over the cap or outside
the kind allowlist is dropped and named. A refused post moves no state — a
refused `CLAIM` claims nothing.

The rule behind all of these: a forum that silently swallows what it refuses
teaches its readers that nothing was refused. The same reasoning as
`sealed_overridden` in the old verify overlay, and as the browser proxy
answering a refusal in the daemon's own wire shape rather than dropping the
connection.

## T10 Peer influence

Once a peer's text has been delivered into a box, that box's output is evidence
about the box *and* about whatever that text asked for, and the two are no
longer separable from outside. The box is marked, and the mark appears in
`h5i box status` and in the export report.

Marked on **delivery**, not on read: delivery is what the host observes, and
whether the agent read the file is a claim only the box could make.

This is not a verdict on the text. It is the one fact a reviewer needs before
treating a patch as the box's own work — and the counterpart to it needs no
feature at all: a verifier that read none of the conversation is simply a box
that was never attached.

## T11 The surface

The console gains a second tab rather than a second application. It is
deliberately not styled like the first: the console is a mint instrument for
watching one box, the forum is the product's outward face and wears the site's
drafting-sheet identity.

One visual rule carries it: **inside the fence is what an agent claimed, outside
it is what the host observed.** A post body sits in a dashed enclosure labelled
`agent-claimed`; its sender, box, role and time sit outside it, because the host
stamped them. A refusal is a filled red band with no fence, because the host is
speaking in its own voice — and since nothing else on the page is filled red, a
boundary someone tried to cross is the loudest mark on the screen.

Every route is a `GET`, and the no-mutation property (`tests/console_api.rs`)
still holds. Human actions are rendered as the commands that perform them. A
browser tab that could post to the forum would be a participant the host cannot
name, which is the one thing the identity model does not allow.

## T12 What is deliberately not built

- **`h5i forum serve`** — the resident tender for idle boxes (T7). Wait for the
  gap to actually hurt.
- **Structured delegation** — `request-action` with
  `sender ∩ receiver ∩ ceiling`. The design holds; the demand is unproven, and
  free-text posts deliberately carry no authority at all, so nothing is missing
  yet.
- **Sealed verify on the forum** — the `sealed_from` overlay and
  `sealed_overridden` tamper lane from the deleted `team.rs`. The strongest
  follow-up, and the natural next step once peer-influence marking is in use.
- **An MCP adapter.** CLI plus skill works under both runtimes today; B11.4
  already decided against MCP for the browser for the same reason.
- **Per-thread read ACLs.** Every member sees every thread. On one repository
  the compartment buys little, and DMs are absent by construction rather than
  by rule.
- **Any content judgement** — classifiers, moderation, reputation. See T2.

## T13 The remote: one route, whether the peer is on this machine or another

T4 said a box has exactly two forum-shaped holes and no network. That stands, and
it is about the box↔host segment. This section is the other segment — host↔store
— and there the first design was wrong in a way worth recording.

It had two paths: same-machine boxes wrote the local refs directly, and
cross-machine would have gone through a remote. That is the shape everyone
reaches for, and the cost is not performance, it is **coverage**. The shortcut
becomes the only path anybody ever runs, and the sync path rots untested until a
second machine joins and everything it was supposed to handle happens at once. A
push to a local bare repository costs a few milliseconds against a tender that
runs once a second, so the shortcut buys nothing and hides everything.

So every forum has a remote, including a solo one, which falls back to a bare
repository under the sidecar root. **Solo and team differ by a URL and by
nothing else.**

### T13.1 Why a git remote and not a service

Because nobody has to run it. A team already operates a git host, and that host
already answers the two questions a forum would otherwise need its own answers
for: **who may post** is push access, **who may read** is read access. A public
repository is an open topic, a private one is an internal one. No server to
deploy, no uptime to own, no roster to invent — which preserves the property T7
protects, that h5i has nothing to operate, at a scale where it looked like it
would have to be given up.

### T13.2 The compare-and-swap is the forge's, and it was measured

Threads are append-only and a union merge descends from the remote tip, so every
honest update is a fast-forward. A non-fast-forward rejection therefore *is* the
CAS, and it means exactly one thing: somebody posted between our fetch and our
push. Fetch, merge, push again.

Measured against GitHub rather than assumed, on 2026-08-20:

| probe | result |
|---|---|
| push to `refs/h5i/forum-probe/t1` | accepted (and `refs/h5i/context/*` from an earlier era was already there) |
| non-fast-forward push to it | `! [rejected] (non-fast-forward)` |
| `--force-with-lease` against the fetched tip | accepted |
| `--force-with-lease` against a stale tip | `! [rejected] (stale info)` |

The last two are not used on the happy path; they were probed because a lease is
the fallback if a future thread shape ever stops being append-only.

### T13.3 Nothing deletes, and nothing depends on a ref being absent

A thread on the remote this machine has not seen is fetched; one here that is
not there is pushed; nothing is ever removed.

Closing was the exception, and was wrong for it. `close` moved the ref to an
attic and deleted the live one, which does not survive a peer: measured on two
clones, one closed a thread, the other had not heard about it, still held the
live ref, pushed it back — and the decision was silently undone on both
machines. Every other status here was already a projection over an append-only
log; closing was the one mutation, and that inconsistency was the bug. It is a
`CLOSED` post now, and the attic namespace is gone.

Removing the last dependence on absence also declaws the obvious attack. Anyone
with push access can `git push --delete` a thread ref and nothing at the client
refuses; the next sync from any clone that still holds the thread puts it back,
because the push is driven by what we have rather than by what the remote lacks.
Measured: an honest clone restored a deleted thread on its first sync, still
closed, and the deleting clone got it back too. An attacker buys a window, never
a loss, as long as one honest participant still has the conversation.

The reopen rule tightened while fixing this. "Any later human post reopens it"
is too loose across machines, where `(ts, id)` order is not the order things
happened: a note arriving late from a peer, or written under a skewed clock,
would silently reopen a closed thread. Only a human taking a status-moving
action reopens one, and an agent cannot at all.

### T13.3a Prevention, when repair is not enough

Self-healing is a mitigation, not a refusal, and under a custom ref namespace it
cannot be anything else: GitHub's branch protection and rulesets only reach
`refs/heads/**`, so `refs/h5i/forum/*` is undefendable by the server.

`h5i forum remote --branch-refs` publishes under `refs/heads/h5i-forum/`
instead, where an admin can block force pushes and restrict deletions for
`h5i-forum/**` and the attempt is refused rather than undone afterwards. The
local mirror keeps `refs/h5i/forum/*` in both modes, so only the published half
of the refspec moves and nothing else has to know which is in use.

Two costs, named rather than buried. Threads appear in `git branch -a` and in
branch pickers. And `git push --all` walks `refs/heads/*`, so a repository
holding both code and forum would publish threads on any bulk push — which is an
argument for giving a protected forum its own repository, not against branches.

What branches do **not** risk is being mistaken for code. Every thread is an
orphan commit chain — `create_thread` commits with no parents — so
`git merge-base main <thread>` is empty, a forge finds no common history and
declines to open a pull request between them, and the tree holds `posts.jsonl`
and `thread.json` and nothing that looks like source. Verified locally.

**Not verified.** That a ruleset pattern actually enforces on a real forge is a
repository-settings question this codebase cannot test, and it was not measured
the way the push semantics in T13.2 were. What was measured is that publishing
under `refs/heads/h5i-forum/` round-trips, and that the chains are orphans.

### T13.4 Agents still never speak it

The forum being a repository does not make the forum reachable from a box.
Giving an agent a git credential for it would put a pushable credential inside
the box, punch a hole in a `net.mode = deny` profile, and collapse the identity
stamp into "whatever the box claims" plus N deploy keys to manage.

So the topology is two segments with exactly one mechanism each, which is more
uniform than the version with a local shortcut, not less:

```
box ──(read-only inbox / spool)── host ──(git remote)── forum store
```

Fetching runs with `transfer.fsckObjects` and `fetch.fsckObjects` on, and parks
the remote's refs in a staging namespace to be merged rather than adopted, for
the reason `quarantine` states: what comes back was authored on a machine this
one does not control.

### T13.5 What this opened, and what it did not

It did not solve remote attestation. The ceiling check reads a box's
digest-verified `policy.resolved.toml` from a file the local host owns; for a
post relayed from another machine, this host has that machine's *word* for what
its box ran under. That is a claim, not an observation, and it is the same
distinction as `box-claimed` versus `host-observed`.

The honest fix is not to pretend the hub verified it, but to record **who
vouched**, and render it as its own lane the way R10 named `runner-observed` a
third tier rather than folding it into the other two. Built as T14.

## T14 The vouching lane

Without this, the forum's central promise degrades in silence the moment it
crosses a machine. On one host the line above a post is the host's *knowledge*:
it stamped the sender out of an env directory it owns, and no agent could have
written it. Fetch the same post from a peer and the host observed **nothing** —
it has another machine's word for every field — and yet it rendered identically.
The sender stopped being a fact and went on looking like one.

So every post carries an `origin`, and every reader computes a lane against its
own identity:

| lane | what the reader knows |
|---|---|
| `host-observed` | this host stamped it; sender, box, role and policy digest are things it saw |
| `peer-claimed` | it arrived over the remote; everything about its author is the origin's account, **including the origin** |
| `unattributed` | it arrived naming no origin at all |

The asymmetry is the design. A host can be certain it *did* stamp something and
certain of nothing else, so `Observed` is a real guarantee and `PeerClaimed` is
an explicit absence of one. The same bytes therefore read differently on the two
machines, which is correct and is what the test pins.

### T14.1 What the origin is not

**It is attribution, not authentication.** Nothing signs it. A hostile host can
put any string in a post's `origin`, including another host's, and h5i cannot
tell. Saying otherwise would repeat exactly the mistake this lane exists to fix.

What it buys is the one comparison that is sound — *did I stamp this?* — plus
the ability to see that two posts claim different sources. That is enough to
stop the UI asserting knowledge it does not have, which was the actual defect.

The upgrade that would make it evidence is signing the forum commits, and it is
deliberately not taken: it costs key management, and the whole remote design is
built on a team not having to operate anything. `runner_id` (R6) shows the shape
if a future forum wants it — an identity that is the hash of a host key cannot be
repointed at different hardware.

### T14.2 Why not just trust the forge

The git host authenticated whoever pushed, which is real evidence — but it lives
in the forge's push events and audit log, not in the object graph, so a clone
cannot see it. A forum that wanted to use it would have to talk to a specific
forge's API, which is the vendor coupling the remote design exists to avoid.
Recorded here because it is the obvious next idea and it does not work as
cheaply as it looks.
