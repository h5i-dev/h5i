# ROADMAP: h5i as a contained agentic development environment

Status: in progress, 2026-08-05. Supersedes the "auditable workspaces /
provenance" positioning for the product surface. Design docs under `roadmap/`
stay as history for the parts we keep. Decisions already taken are in section
10, what is still open is in section 11.

**M0 through M5 are built. M6 is mostly built. M7 (the terminal viewer) is
built but undriven.** What is not done, stated plainly so it is not read as
finished: the exit criteria for M4, M5 and M7 have none of them been
demonstrated with a real agent or a real person in the loop (every piece each
needs is built and tested); the control lock is not enforced on the agent's
side (section 11.1); `npx skills add` is unverified for lack of a Node 22
runtime; there is no demo video; and `/blog/` and `/pitch/` still argue the old
positioning.

One thing M7 is worth reading for beyond its own feature: it found that **every
human takeover through the web viewer had been silently doing nothing** since
M5, for the same reason two of M4's findings existed — a message the other side
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
  `receipt.rs`. `risk.rs` is *not* back — the badges are arithmetic over
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
- Exactly one automation client per box. Multi agent shared control is out of
  scope.

### 5.5 Credentials

- Model API: the key stays on the host. `auth_proxy.rs` already injects it into
  outbound requests from the box, scoped per runtime, so a Claude box cannot
  reach the OpenAI credential or vice versa. Keep, make it default on rather
  than opt in.
- **Any other service: the same mechanism, generalized.** An earlier draft of
  this section proposed a GitHub "capability helper" — a host side process
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
     limit is real and must be stated — it only works for clients you can point
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
  is a method/path condition on the proxy — still policy data, not vendor code.
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
does — fork, enter the box's user and network namespaces by pid, connect, and
take the socket back over `SCM_RIGHTS`. So `--term` binds no port, mints no
token, and serves no page. There is nothing for another local process to
connect to, so there is nothing to authenticate. The forward keeps its token
because it must listen; this does not.

What it is made of, and what each part is for:

- **`ws.rs`** — a WebSocket client, roughly the RFC 6455 subset one connection
  to one server needs. Everything the box sends is untrusted: reserved opcodes
  and reserved bits are refused, a masked server frame is refused, lengths are
  capped before they become allocations, and fragmented messages cannot grow
  past the cap across frames.
- **`proto.rs`** — the stream's messages. Pinned to what agent-browser actually
  dispatches (`input_mouse` / `input_keyboard` / `input_touch`) rather than to
  what the DOM calls them, for reasons in the bug note below.
- **`image.rs`** — `zune-jpeg`, which forbids unsafe code, with dimensions
  capped before decode. Frames are scaled to the pixel size they will actually
  be displayed at, because every byte crosses a PTY and over SSH that is the
  whole cost of the viewer.
- **`kitty.rs`** — the graphics protocol, generated **by the viewer and only by
  the viewer**. `q=2` on every render command, so the terminal's replies never
  land in the middle of the keystrokes being translated into page input. Direct
  transmission only: the file and shared-memory mediums are faster and only
  work when the terminal is on this machine.
- **`input.rs`** — terminal bytes to CDP events, including the two places a
  terminal and a browser genuinely disagree: a terminal reports presses with no
  releases (so the pair is synthesized, and press-and-hold does not work), and
  it reports cells rather than pixels (so clicks map through the placement, at
  cell resolution).
- **`status.rs`** — the row the page cannot reach.
- **`term.rs`** — raw mode, alternate screen, mouse and bracketed paste, all
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
  (escape sequences *and* bidi overrides, which needed a fix in `redact.rs` —
  they are not control characters, so the existing pass let them through) and
  it is never the field that gets truncated: a URL too long for the row loses
  its path, and an origin too long for the row is cut from the **left**, since
  shortening `bank.example.evil.test` from the right is the spoof itself.
- **Two modes, because a terminal makes them natural.** VIEW is read-only and
  leaves the mouse to the terminal, so selection and scrollback still work.
  INTERACT takes the control lock — reaching for the controls *is* taking them,
  which is the lock's own rule and the only sensible one here, since the
  terminal is busy being the viewer and there is no other window to run
  `h5i browser take` in. `Ctrl-]` is reserved to get back out, because raw mode
  hands the viewer every other key.

**Still open, and deliberately not built yet.** LOGIN mode — withholding frames
and snapshots from the agent while a human types a credential — rests on the
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

`viewer.html` sent `mousedown`, `keydown`, `wheel` — the DOM event names.
agent-browser's stream server dispatches on `input_mouse`, `input_keyboard` and
`input_touch`, and falls through to `_ => {}` for everything else. So **every
human takeover through the web viewer was a no-op**, and silently: the socket
stayed healthy, frames kept arriving, and the forward counted the input frames
as forwarded — the receipt would have recorded "a human drove this box" for a
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
failed. There is no channel to push updates on — the stream is a straight relay
— so the holder is stamped into the page at serve time and the page says that
is what it is.

### 5.11 Share: the first inbound path (built, 2026-08-10)

Everything else in this document is about what leaves the box. Share is about
what comes in: a second person, on their own machine, trying the web app the
agent built while it still runs inside the box. The demand is the ngrok use
case — "here, click around" — without the part where a tunnel URL quietly
exposes a dev server that was never meant to face the internet, and without
standing up an account, a domain, or a server of ours.

**Port sharing, not viewer sharing, and that is a use-case decision.** Two
shapes were on the table. Sharing the *viewer* — the agent-browser stream of
5.9, carried over the network instead of loopback — reuses the forward and the
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
  (5.9, 5.10). Nothing inside the box learns it is being shared, the netns
  gains no hole, and the box's egress policy is untouched — the bridge is a
  host process, outside the boundary, like the CONNECT proxy.
- **Hold the capability.** A ticket minted at share time is the whole access
  model: it names the box, the port, an expiry, and a secret; possession is
  authorization. One ticket admits one peer — share with two people by minting
  two — so revocation (`h5i box share revoke`) is per person, and `stop` ends
  the session for everyone. No account on either side. (As shipped, minting a
  second ticket works on `--tunnel` shares only; see 5.11.1.)
- **Write the ingress receipt.** Every lane in 5.7 observes egress. This is
  the first inbound evidence: peer, connection times, requests proxied, bytes,
  and the transport actually used (direct, relayed, tunnel), in the same
  receipt the export already carries. A share session that left no record
  would be the one part of a box's life the receipt is silent about, which is
  exactly the kind of gap this document exists to refuse.

**Transport one: iroh.** Peer-to-peer QUIC, end-to-end encrypted, NAT
traversal with public relays as fallback for the hard cases; the relay sees
addresses and volume, never plaintext. The ticket carries the node addressing,
so there is nothing to configure. `--direct-only` refuses to move application
bytes over a relay: a peer that cannot get a direct path is turned away, and the
share stays up for anyone who can.
The other end runs `h5i join <ticket>`, which terminates the QUIC connection
and serves the app on the joiner's loopback — and that listener repeats 5.9's
lesson on someone else's machine: a bare local port is reachable by every page
and process the joiner has open, so the local URL carries a token and the
proxy refuses without it. iroh is a real dependency tree (QUIC, TLS), so it
is a cargo feature in the `web` pattern: default on, and a build without it
loses `share`/`join` and nothing else.

**Transport two: Cloudflare quick tunnel, because the joiner may not be a
developer.** P2P requires `h5i` on both ends, and the person you most want
clicking the prototype — a designer, a PM, a customer — will not install a
CLI. `h5i box share --tunnel` shells out to `cloudflared` and hands back a
plain URL any browser opens. The same bridge still fronts it: the URL embeds
the ticket token, the bridge checks it and the expiry on every request, and
revocation still works mid-session — the capability degrades from "hold the
secret" to "hold the link", not to nothing. The honest costs, which the docs
must state rather than blur: TLS terminates at Cloudflare, so this mode is
not end-to-end and Cloudflare can read the traffic; `cloudflared` is an
external binary we neither pin nor ship; and quick tunnels are explicitly not
a production service (concurrency caps, no SSE). It is the no-install mode,
not the default mode.

**What the joiner is exposed to, stated up front.** The app being shared is
agent-written, untrusted code, and port sharing renders it in the joiner's own
browser — that is the point, and it is also the exposure, the same one as
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
relay capacity is a SaaS with an abuse desk attached — out of scope by the
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
  network namespace of its own", not a list of tiers — a `process`-tier box gets
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
  enforcement. It was not — the box runs agent-written code, and a dev server
  that answers keep-alive leaves the front holding an ungated pipe. The second
  review caught exactly that, so the front now reads the head plus the declared
  body and then **stops reading the client**, which needs nothing from the box.
  Two things fall out. A chunked request body has to be *parsed* rather than
  just copied, because forwarding one request means knowing where it ends and a
  chunk stream only says so in its own framing — it was refused with a `501`
  for two rounds, which meant no streamed upload worked at all. And an upgrade
  earns its two-way pipe only after the box answers `101`,
  with the request required to carry both `Upgrade` and `Connection: upgrade` —
  a lone `Upgrade:` header is something any client can attach to a request that
  will never upgrade, and it was an opt-out from the whole rule.

  A third round found the other half: the *response* was relayed untouched, so a
  box answering keep-alive told the visitor's browser to reuse a connection this
  proxy would never read again — an intermittent hang, and a `502` for every
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
  of view, just another cookie — and cookies ignore the port, so the browser
  sent both to both, and each front dutifully forwarded the other's to
  agent-written code. Reading our own cookie by exact name and dropping every
  cookie whose name starts with the share prefix are two different rules, and
  the difference was the one property the gate exists for. The test that had
  been written for the first fix asserted the leak as correct behaviour, which
  is the more useful lesson: a test can pin a bug as a feature.
- **Revocation is per grant all the way down.** The watchdogs first asked
  whether the *share* was spent, which is true only when no grant admits
  anybody — so revoking one peer while another was still connected left the
  revoked peer's open streams running, and the CLI printed "any connection that
  peer had is dropped within a second" while that was false. Each connection now
  watches the grant that admitted it. Same class of thing as `--direct-only`,
  which was checked once at setup and never again: a direct path can die and
  iroh will fall back to a relay, so a promise checked once is a preference.
  Both are polled for the life of the connection now.
- **Streams are served concurrently, and that was a real bug first.** The first
  cut awaited each stream to completion before accepting the next, which
  serialises every share behind whichever connection is longest-lived — for a
  dev server, the hot-reload socket that never ends. Found by the in-process
  end-to-end test hanging, which is the argument for having written it.

**Not built, deliberately.** `h5i join --isolated` (opening the shared page in a
box of the joiner's own) is designed in this section and has no implementation;
the warning at join time is what stands in for it today. Viewer sharing is not
built and was never in this milestone.

**Not built, and it is a gap rather than a choice.** `h5i box share grant` mints
a second ticket for a *tunnel* share only. A P2P ticket needs the running
endpoint's addressing, and only the serving process has it — so adding a second
peer to a P2P share means starting a second share. The verb refuses with that
sentence rather than handing out a ticket that names nowhere. Closing it needs
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
nothing else. The day the product needs a real desktop — a native app under
test, browser chrome, a file picker, devtools as a human sees them — an X plus
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
> regression — but it is drift nobody has diagnosed, and the process tier is
> not actually covered here until someone does.

**M0. Freeze and branch — done.** `dev` is the integration branch and this
roadmap is on it.

**M1. Amputation — done.** Section 3.2 is deleted (~77k lines). `receipt.rs`,
`refstore.rs`, `redact.rs`, `source.rs` extracted; `env.rs` is free of
`objects`, `ctx`, `msg`, `team` and `repository`. The whole lifecycle works
with no git notes and no context refs, clippy is clean over the workspace, and
the `web` feature is gone rather than off.

**M2. `h5i box` and copy in — done.** New command surface with `env` aliased
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

**M3. Agent in box hardening — done.** Warm caches in full: the store, the
lockfile keying, the staleness rule, `h5i box cache ls|mounts|rm|refresh` and
the **read-only mount** on every tier are built and tested (5.8). (An earlier
revision of this line said `refresh` was not built; it landed in e75020358,
with the writable bind reachable from no profile and the refusal that names the
registry-only profile it demands.)
Also done: the credential-seed audit (the per-box HOME copy now drops
credential-shaped entries at any depth — `credentials*`, `.netrc`, ssh keys,
`*.pem`/`*.key`/`*.p12` — keeping only the runtime's own token, which it cannot
function without), and the credential proxy, which was already default-on but
did not engage for a `browser` box.
Also done: profile-declared authenticated egress (5.5, option 1) — a reverse
proxy per grant, the credential resolved host side and never placed in the box,
part of the pinned digest, and fail-closed when the host-side variable is
unset. GitHub is a policy entry, not a feature. Option 2 (a TLS-terminating
forward proxy) stays unbuilt and unneeded.

**M4. Browser — done.**

The live runs were worth more than the code around them. What they found, in
order:

1. ~~**The `supervised` + agent-profile `EINVAL` happens when the box's
   workspace is under `/tmp`**, because the agent profile redirects `/tmp` to a
   per-env scratch and that shadows the worktree.~~ **Wrong, and withdrawn.** A
   `create`-time refusal for that layout was written and it rejected this
   suite's own fixtures — every `tempfile` repo is under `/tmp`. Checked
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
   "AI_GATEWAY_API_KEY present (chat enabled)" — the opposite of the intent.
   **Fixed** by not injecting it at all (it is not in `env.pass` either, so it
   is absent), and verified from inside: "chat command disabled".
5. Doctor also confirms the profile's grants work: **Chromium 130 is found** at
   the granted `~/.cache/ms-playwright` path.
6. **The daemon exited during startup with no output — and it was our socket
   gate, not Chrome.** The supervised tier notifies on `socket()` and denied
   `AF_UNIX` unconditionally, with no way for a profile to ask for it; the
   daemon's control socket is a filesystem-bound `AF_UNIX` listener, so it got
   `EPERM` on the first thing it did. `Profile::unix_sockets`
   (`[profile.X.net] unix = true`) is that way to ask, and `browser` sets it.

   The grant is narrower than it sounds, which is why it can exist: abstract
   sockets are scoped by the private netns, filesystem-bound ones by Landlock,
   and `/tmp` — where `.X11-unix`, `tmux-*` and an ssh-agent live — is a per-env
   scratch at the kernel tiers. The residual is a host socket under a granted
   path, so it stays opt-in per profile and lands in the digest.

   The silence was upstream's, and worth recording: the daemon redirects its own
   stderr to `/dev/null` before failing **unless** `AGENT_BROWSER_DEBUG` is set,
   in which case it writes to `$AGENT_BROWSER_SOCKET_DIR/<session>.log`. That log
   is the only place the real error appears; `--debug` alone does nothing.

7. **Two variables we set were not variables agent-browser reads.**
   `AGENT_BROWSER_HEADLESS` does not exist (it is `AGENT_BROWSER_HEADED`, and
   headless is what a falsey value means), and neither does
   `AGENT_BROWSER_DISABLE_CHAT` — chat is gated on `AI_GATEWAY_API_KEY`
   presence alone. A variable the tool never reads reviews as enforcement while
   enforcing nothing, so both are gone and the tests assert their absence.

Nothing here could have been found by reading the code, which is the argument
for driving the loop before building more on top of it.

That gap — **no test in the suite ran an agent-family profile at
`supervised`**, the only kernel tier that can host an agent or browser box
(`process` refuses the egress the profile needs) — is why both surprises were
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
is refused by absence — the only mechanism upstream has.

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

**M5. Viewer — done.** The control lock
(`crates/h5i-core/src/control.rs`, `h5i browser status|take|release`): the
agent holds control by default, a human *takes* it rather than asking, and
handing it back sets a stale-handle flag that refuses the agent's next mutating
action until it re-snapshots — read-only verbs stay available throughout,
because watching never collides. Nothing upstream arbitrates this, which is
why it is ours.

The forward (`crates/h5i-core/src/view.rs`, `h5i box view`, `h5i browser url`)
serves the agent-browser stream to loopback. The box's port is never published:
h5i enters the box's user and network namespaces by pid, connects from inside,
and hands the socket back over `SCM_RIGHTS` — the fd-handoff the supervisor
already uses. All four gates verified live against a supervised browser box:
loopback only; a per-box token minted at create and kept outside every path the
box can read or write (401 without, 401 on a wrong one); cross-origin
handshakes refused (403) even with a valid token; and the control lock on the
input direction, with input *dropped* rather than rejected so someone who clicks
before taking control keeps a live viewer. Sessions land in the receipt and the
export under a `viewer` lane, and the report calls out a session where a human
drove.

Three bugs found, all the same kind — quiet failures producing a plausible
wrong answer rather than an error, and each worth remembering:

- The live registry records h5i's **host-side** pid, which is in the host's
  netns. Entering it succeeds, finds nothing listening, and reads as a broken
  box. Fixed by walking the session's process tree for the first descendant
  whose netns differs from ours.
- A stray CRLF in the relayed handshake is not a protocol error the server
  reports — it is two bytes read as the start of the client's first frame, after
  which the handshake completes and the viewer hangs.
- Returning `Result<u64>` from the input pump discarded the forwarded-input
  count on the error path, which is exactly the path a human takes by closing
  the tab. The export would have recorded them as never having touched the box.

Exit criterion **not yet demonstrated end to end**: a human takes over mid-run,
finishes a form, hands control back, and the agent continues from a fresh
snapshot. The takeover, the input gating and the stale-handle refusal are each
verified; a real person finishing a real form is not something this session
could run.

**M6. Skill and story — mostly done.** `skills/h5i/` is written against the
real surface and the binary carries it; the missing fifth page
(`references/browser.md`) is written. The README, MANUAL.md, `man/h5i.1` and
`docs/manual/index.html` all describe the product that exists — the manual was
3,900 lines of `capture`/`recall`/`audit`/`team`/`mcp`, and is rewritten around
the boundary. The landing page is rewritten too, and the embedded mock of the
deleted `h5i serve` workbench is gone with its CSS and its film driver. Seven
guides plus `/features/` and `/workflows/` teach deleted commands, so each
carries a banner and is `noindex` rather than being quietly left.

**Remaining**: `npx skills add h5i-dev/h5i` is still unverified — the `skills`
CLI needs Node >= 22.20 and no such runtime was available to test it; the repo
layout and frontmatter were checked against what the CLI discovers. There is no
demo video. `/blog/` and `/pitch/` still argue the old positioning, and
rewriting them means choosing the launch message, which is open question 2.

**M7. Terminal viewer — built and driven.** `h5i box view --term` (5.10). The
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
frame(s) forwarded to the page` — "hello" as ten key events plus a press and a
release — and flagged `a human drove this box` from the input count, which is
the take-and-hand-back case that comparing the holder at open and close would
have missed.

Two things it found that no amount of reading would have. A pty with no window
size makes `TIOCGWINSZ` succeed and report zeroes, and the viewer dutifully
scaled the page into one cell and transmitted a **1×1 pixel image** with no
error anywhere; `Size::or_fallback` now supplies 80×24. And the first harness
stopped reading before sending `q`, which is why it looked as though the
alternate screen was never exited — worth remembering as a way to mistake a
test artifact for a terminal-corrupting bug.

Still **not demonstrated**: a real person, at a real Kitty-protocol terminal,
looking at the page. Everything under it is exercised; the last inch is a human
being, and a tool shell is not a TTY.

**Post M7.** A full-desktop tier when something needs more than a page viewport
(X plus streaming, Neko as the reference design), microVM backend, macOS.

**M8. The mediated socket — proposed (7.2).** The agent-browser daemon becomes
an h5i-launched sidecar and its socket path is h5i's listener. Exit criteria:
an agent's `agent-browser click` during a human takeover is refused with the
typed error, not advised; every mutating verb appears in the receipt with its
arguments; a profile denies `eval` and the denial lands in the receipt; and
the Fetch evidence lane (7.2 item 4) shows a granted request with its
initiator and a denied one with its verdict, marked best-effort. This closes
open item 1 and is worth doing before any engine work, because the mediation
layer is where origin routing (7.1) would live anyway.

**M9. Second engine — proposed (7.1).** `engine` as a profile field, pinned
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

**M10. The lightweight visual engine — tier 1 built, 2026-08-07.**
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
   refuses to paint while any critical resource is pending — so the obvious
   way to write a deny (return without calling the handler) renders every page
   permanently blank. Fail-closed means "completed with nothing", and there is
   a test on it.
2. **`system-fonts` is a build-time native dependency.** Blitz's font
   discovery pulls `yeslogic-fontconfig-sys`, which needs libfontconfig
   headers to compile — portable engine, non-portable build. Fonts are
   discovered and registered at runtime instead, which also makes "no fonts"
   a state `doctor` reports rather than a blank screenshot nobody can explain.

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
What is missing in that last one is the run, not the plumbing —
`H5I_BROWSER_STREAM_FILE` puts the `.stream` under the box's `agent-browser`
directory, which is where the viewers' discovery already scans.

**Tier 2's open item closed, 2026-08-08.** The live view has now been driven by
`h5i box view` against a real `h5i-light` box rather than by a protocol-level
client: the forward attaches and renders, the console's frame relay pulls a
1280x720 JPEG through the same session, input is dropped while the agent holds
the control lock and flows the moment a human takes it — so the lock is enforced
on an engine with no mediator behind it, which had never been checked. Two
defects fell out of the run, both fixed:

1. **A readable file could fail to open.** `open ./page.html` reported "invalid
   path" when `canonicalize` failed, because the fallback handed a *relative*
   path to `Url::from_file_path`, which refuses one. The walk fails for a
   working directory the box can reach by fd and not by name — which is any
   repo under `/tmp`, since that is the directory the supervised tier
   overmounts. The message named the path when the problem was the walk.
2. **`serve` accepted one viewer at a time.** The accept loop handled
   connections sequentially, so opening the console's page tab left
   `h5i box view` hanging in the backlog with no error. Two viewers could not
   coexist, which nothing had tried.
3. **Scrolling only ever worked on unstyled pages.** The scroll range came from
   the root element's `size.height`, which a stylesheet saying
   `html, body { height: 100% }` pins to the viewport while the article
   overflows it — so Blitz reported Wikipedia's 16477px page as 720px and every
   scroll clamped to zero. The fix reads `size.height.max(content_size.height)`,
   which is the same formula Blitz's own `scroll_viewport_by` uses. Every local
   test page was unstyled, so the whole suite agreed with the bug. Found by
   pointing the thing at Wikipedia, which is the entire argument for doing that.

**The resident session, 2026-08-08 (§12.1).** `serve` now holds a page
that several viewers and a control channel share, and
`h5i-browser-light session status|snapshot|navigate|click` drives it. A control
verb that moves the page broadcasts to every viewer, so the live view shows the
page *the agent* is driving — the caveat M11a's page pane had to print is gone
for this engine. Ack pacing moved from a structural accident ("one frame per
client message") to per-viewer state, holding the *newest* frame rather than
queueing a backlog, and nothing is encoded at all when no one is watching.

The architecture was chosen by the compiler, not by preference: **`Page` is not
`Send`** — `BaseDocument` holds an `Arc<dyn HtmlParserProvider>` and a
`Box<dyn FontMetricsProvider>` — so the obvious `Arc<Mutex<Session>>` does not
exist. One thread owns the page and everything else reaches it by channel. That
is the shape a multi-driver session wants regardless; here it was not optional.

**Untrusted-content marking, 2026-08-08 (§12.1).** The rendered snapshot
now fences page content and names it as data. Pulled ahead of its position in
the list because §11 called it "the only item on this list whose absence is a
live hole rather than a missing feature" while ranking it fourth, and it depends
on nothing. Writing the test found the hole that made the fence worth having:
`href` was the one page-derived field the walker did not collapse, and an HTML
attribute value may contain a literal newline — so the field that could forge
the fence was the field nobody had thought of as text.

**The agent-actions pane had no source on this engine, 2026-08-08.** Found by
someone running an agent in an `h5i-light` box and noticing the pane stayed
empty while the agent worked. It was empty *by construction*: the pane is fed by
`browser-actions.jsonl`, which the mediator writes, and
`engage_browser_mediation` returns `None` for any engine agent-browser cannot
drive. Before the resident session that was harmless — there were no verbs to
miss. Adding verbs made it a monitoring surface that silently under-reported,
which is the failure this codebase keeps writing tests against.

`serve` now writes its own action log (`$H5I_BROWSER_ACTIONS`), ingested as a
fourth source into `BoxStream::poll` and rendered **box-claimed**, not
host-observed. That distinction is the point rather than a caveat: h5i sits on
no socket between an agent and this engine because the engine *is* the browser,
and a row claiming otherwise would launder the box's own account into evidence
h5i gathered. The pane note is engine-aware for the same reason. Each verb is
recorded before it runs and again after — no record, no action — which is a
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
performing it — the encoding is upstream's, the wire stays ours, and a
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
RFC 6265's *default-path* derivation — which exists only to fill in a missing
`Path` attribute — so a cookie set at `Path=/admin` was never sent to `/admin`.
And `scroll_height` was tried for the scroll range before the fix above: taffy
measures overflow *within* a box, which is zero for an unstyled page whose root
simply grew.

**Still open. LOGIN mode is not built**, and it is the one item this entry was
warned about: §12 pairs LOGIN mode with cookies precisely because a session
with cookies is the first version of this browser where a stolen credential is
worth having. Until it lands, a human taking over to type a password does so on
a page the agent can still snapshot. File uploads are dropped rather than read,
which is a deliberate refusal to acquire filesystem reach. Tier 3 (policy-gated
script) is now in scope, and the cost of putting it there is §12.5.

**Corrected 2026-08-08.** This entry also said "nothing wires h5i to this
engine yet — M9's `--engine` knob does not exist, so using it in a box is
still manual", which was true the day tier 2 shipped and stopped being true
three commits later on the same branch. `--engine h5i-light`, or
`[profile.X] engine`, pins the engine in `policy.resolved.toml` and so in the
digest; `browser_env` hands that engine `H5I_BROWSER_ALLOW` (the box's own
`net.egress`, loopback included) and `H5I_BROWSER_RECEIPTS` pointing at the
box's spool, and skips the agent-browser shim, whose job is to launch Chrome
and attach a driver — neither of which applies to an engine h5i runs itself.
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

**M11. The developer-mode viewer — built, 2026-08-07.** `d` in the terminal viewer splits the screen: the page keeps the top, and a console/error pane takes the bottom third. What it shows was already arriving and being thrown away — `ConsoleError` and `PageError` carried their text and the viewer kept only a counter. Page text is passed through `sanitize_display` before it is drawn, because a console message is untrusted input and would otherwise repaint the viewer's own chrome. The layout and the pane renderer are pure functions (`termview/panes.rs`) with the split, the truncation, the bounded buffer and the sanitising all tested; `App` stays the thin thing that positions and writes, which is why any of it is testable at all. A terminal shorter than 16 rows keeps the whole page rather than showing two useless slivers. Not built: a per-request network pane — nothing on the viewer's stream carries requests, and the mediator's records are host-side, so that needs a source rather than a layout.

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

**M11a. The browser terminal — the event model and the evidence panes are
built, 2026-08-08.** The half this entry called durable, and said would land
first, has: `browser_events` is the one stream, and the console reads it.

* **The model** (`crates/h5i-core/src/browser_events.rs`). Every event carries
  its lane *and* its grade, kept apart because they answer different questions
  and the interesting case needs both: our own engine's request log is
  **box-claimed** (written inside the box) and **fail-closed** (the engine will
  not fetch what it cannot record). Chromium's Fetch lane is box-claimed and
  best-effort. One "trusted" flag could not have said that. `caused_by` is set
  only where the source carries the link — a response to its request by
  sequence number, a refusal to the action that provoked it — and nowhere else,
  so no arrow on the screen is drawn from two things merely having happened at
  about the same time. Ingest sanitises every box string once, here, rather than
  in each renderer, because M11b writes this same text straight to a PTY.
* **Three real sources**, no placeholders: the light engine's request log, the
  mediator's actions, and the drained page evidence.
* **The mediator now writes its actions as data.** They were only ever on the
  receipt as *rendered text*, so a reader wanting them back would have had to
  parse a display format — the quiet-wrong-answer shape this file keeps
  recording. `browser-actions.jsonl` sits beside `receipt.jsonl`, host-side,
  where the box cannot write, and the round trip is pinned by a test.
* **In `h5i ui`, on the console's own terms.** One `GET`, the same token gate,
  no second web surface. Every row shows its lane and grade as words rather than
  as a colour, selecting a row lights what it caused and what caused it, the
  network pane names its engine's evidence grade in its header, and a dropped
  count is rendered rather than hidden.

**Driven against a real box, not only a test client** — the gap M10 recorded and
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
is `#[serde(skip)]`, so `host_tmp_root` — correct for a live run, which is the
only caller it had — returns `None` for **every** policy loaded back from disk.
The console asked a live-run question of a stored policy and got a silently
empty stream for a session that had one: enforcement-shaped code answering
"nothing to show" instead of "I cannot tell". The reader now takes the path from
`private_tmp_backing`, the same function that placed it.

**Second pass, same day: its own tab, and the reader made honest.**

* **The stream is incremental and session-aware, which was a bug fix rather
  than an optimisation.** The first reader re-parsed every source per poll and
  numbered from 1 each time — stable only while files grow by appending, and
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
row was dropped silently in the browser — the swallow that had just been fixed
one layer down, moved one layer up. Found by grepping the *served bundle* for
the divider text rather than by trusting a green typecheck, which could not see
it: an unknown variant simply matched no case.

**Third pass: the console carries pixels.** The page pane shows the box's page,
rendered by our own engine inside the box. The frame lane is joined, and the way
it is joined is the point:

* **A reader, not a proxy.** A background thread per watched box enters the
  box's user and network namespaces by pid, connects to the stream server, and
  reads — the same route `h5i box view --term` takes (`view::connect_in_netns`),
  reusing the same hardened WebSocket client (`termview::ws`, which refuses
  reserved opcodes, masked server frames and oversized lengths). Nothing new
  listens; the box gains no reachability it did not have.
* **The console's structural guarantee survives.** Every route is still a `GET`,
  because the frame is served *as* a `GET` returning `image/jpeg` — `nosniff` so
  crafted bytes cannot be re-read as anything else, `no-store` so a frame of
  somebody's page does not settle into a disk cache. And the relay is
  one-directional by construction: the only messages it can send upstream are
  `config` and `ack`, there is no path from an HTTP request to a write on that
  socket, and a test greps this module for `input_*` so the day someone adds one
  the build says so. Typing into a page still has exactly one door: the forward,
  which enforces the control lock.
* **Change-driven, end to end.** The stream reports the newest frame's sequence
  number and the page keys its `<img>` on it, so an unchanged page is zero
  requests rather than a timer redrawing a still picture — the engine's own rule,
  carried up to the browser.
* **The picture is labelled.** A frame is **box-claimed**: the box's rendering of
  its own page. Nothing derived from it reaches the trusted status row, and the
  `h5i-light` caveat sits under the image rather than being left for a reader to
  infer — that engine has no resident session, so a served view shows the page
  the *serving* process opened, which need not be the one the agent is driving.

Driven end to end rather than asserted: the engine served a page inside a
supervised box, the console found the `.stream`, crossed the namespace, and
returned a 1280×720 JPEG at `frame_seq 2` with the right headers; stopping the
in-box server flipped `live_view` to false, dropped the relay, and the frame
route went to 204. One test was rewritten on the way — clippy caught it
comparing two constants, which is a tautology that would have passed with the
size check deleted; it now drives the real decoder with real base64.

**Still open, and none of it is dressing.** The
accessibility snapshot has no live source (it is a CLI verb today). Takeover is
not wired here: the console remains read-only and input still goes through
`h5i box view`, so the read-only-by-default / interact-under-the-lock rule below
is *stated* by this milestone and *enforced* by the forward, which is one
surface short of the exit criterion. Nothing links an agent action to the
requests it caused — neither the mediator's records nor the engine's log carries
the other's id — so "selecting an action surfaces its correlated request" holds
only for the verdict it provoked, and closing it is a change at the *sources*,
not in the viewer. M11b has not started, so the claim that two readers agree is
untested. The original entry follows.

**M11a (as proposed).** M11 put
the developer view in the terminal; this puts the full one where it can
actually breathe, inside `h5i ui`. The design motif is a trading terminal —
Hyperliquid is the reference, the way terminal-browser was for 5.10: what we
take is the information model (peer panes of equal rank, change-driven row
highlights, an always-on status bar), not the skin. The reasoning is the same
one M11 recorded: for an agent's overseer the rendered page is the *least*
informative pane, so page viewport, accessibility snapshot, agent actions,
network requests, console, and policy verdicts sit side by side at equal rank
— what the agent saw, what it did, what moved on the wire, and what h5i
refused, in one view.

**One web surface, not a second one.** This lives in the existing console:
same axum server, same embedded bundle, same `web` feature, same loopback
bind. The console's own rule — every route is a GET — stands; the live data
and the input direction ride the per-box forward that already exists (5.9),
with its per-box token and its lock check on input. The console gains a view,
not a write path.

**Not a read-only browser.** The viewer is read-only by default, interactive
only while holding the control lock (5.4), and taking the lock is itself a
recorded policy event — the takeover and the window in which human input
flowed belong in the receipt next to the verbs the mediator refused during
it. This is the terminal viewer's VIEW/INTERACT model (5.10) given a second
skin, not a new input policy; a viewer that could never take over would
delete M5's takeover story, and one that could always type would delete the
lock.

**The durable half is the event model, and it lands first.** One stream from
the browser runtime — frames, snapshots, actions, requests, console, policy
verdicts, metrics — with every event stamped with its session, ordinal,
timestamp, kind, a `caused_by` back-reference, and its **lane**:
host-observed or box-claimed, the same two kinds of claim the receipt
already keeps apart. The web view, the terminal view, and the exported
receipt all read this one stream, which is what makes the viewer a live
receipt rather than a dashboard that happens to resemble one: selecting an
action shows the request, console output, and verdict that carry its id.
The panes inherit the honesty rules with the data: the status bar shows
host-derived values only (box-claimed metrics are labeled, not promoted),
and the network pane names its evidence grade per engine — h5i-light's
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
and on M10's open item being closed first — the live view driven by a real
`h5i box view` against a real box — because a polished terminal over a
stream never exercised end to end inverts this file's own priorities.

**M11b. Terminal watch mode — proposed, 2026-08-08.** The shipped terminal
viewer (5.10, M7, M11) re-pointed at the same event stream and kept,
deliberately smaller: viewport, trusted status row, latest actions, console
errors, denied requests, panes cycled rather than tiled. It is the SSH and
demo surface — "or stay entirely inside the terminal" — and it does not
chase pane parity with M11a: the investment moves to the web view, and the
TUI's job is to watch, take the lock when a login wall demands it, and prove
the event model has two independent readers. Nothing shipped is discarded.

**M12. Share — built, 2026-08-10 (5.11, 5.11.1).** The bridge first, because it
is the part both transports share and the part that touches the boundary:
netns dial-in, the grant table with mint / verify / expire / revoke, the HTTP
gate, and the ingress receipt lane. Then iroh and `h5i join`. Then the quick
tunnel on the same bridge. Viewer sharing was explicitly not in this milestone
and is not built.

**What is demonstrated, and by what.** The suite covers the whole P2P chain
end to end in-process — QUIC handshake, greeting, grant table, the dialer's fd
handoff, the byte pump — with a wrong ticket refused on the same connection and
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
- the path was **direct** — hole punching, not a relay — with the endpoint's
  real addresses in the ticket;
- a request with no cookie and one with a wrong cookie both got `401`, and
  neither reached the box;
- `h5i box share revoke` from a third process cut the peer off; the joiner
  printed the sharer's own close reason rather than a transport error;
- the export's receipt named the peer, the grant and its label, the window, the
  connection count, the byte counts and the path:
  `08e03775419e… via direct — grant 38bd63e2 (reviewer), 14s, 1 connection,
  97 in / 412 out`. The connection count is *one*, because the redirect and
  both refusals were answered by the joiner's own proxy and never crossed —
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
and the first diagnosis — that `cloudflared` chunks every body and the proxy
refused chunked — was **wrong**: the `501` came from the box's own
`python -m http.server`, which has no `do_POST`. (Chunked request bodies are
forwarded now rather than refused, which is a real improvement and was reachable
from a direct client; it was not what that `501` was.) And killing the box's
session left the share answering `502` forever with nothing said, because the
dialer's helper lives *inside* the box's network namespace and keeps it alive
after everything else in it has gone — so a box restarted afterwards gets a new
namespace the share can never reach. The share now notices and ends.

**The whole response matrix, run over both transports on 2026-08-10.** A dev
server in a box answering a page, a `304`, a `HEAD`, a chunked response, a form
`POST`, a chunked `POST` and an `Expect: 100-continue` upload — every shape the
framing code had to be rewritten twice to get right — with an anonymous request
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
working — `200` and `401` from the same URL a second apart. The receipt lists
them separately by grant and label, with the revoked one's traffic still counted
and the refusal recorded as revoked rather than unknown. That is the property
the whole grant model exists for and it had never been exercised outside a test.

Also verified live, and worth recording because it was a defect this branch
introduced and fixed: two `h5i join` sessions on one machine, a browser holding
both of their cookies, and the box seeing neither — only the app's own `sid=9`.

**Rounds 8 to 10, and what live running kept finding.** A ticket expiring on
its own — not revoked, not interrupted — ends the share, writes the receipt,
clears the record, and now tells the joiner why; that path was verified twice
because the first fix for it was inert. A dev server that rejects a request
before reading its body has its own answer relayed rather than replaced. And a
`--tunnel` share with two grants had one of them revoked while the other kept
working.

**Rounds 11 to 14, and the two that would have bitten a real user.** A client
that sends its request and then shuts down its write side — legal HTTP/1.1, and
what anything built out of one write and one read does — had that EOF read as
"the visitor left", so the relay stopped on the spot: a 2 MB download arrived as
63 bytes, with a clean close and nothing recorded anywhere. And `h5i join` was
hung up on by the sharer thirty seconds after connecting, because the sharer
drops a connection that has never authorized a stream and the joiner did not
open one until somebody visited the page — so the ordinary sequence (send a
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
during the teardown was done by arming the hard-exit watcher after the select —
and on the three exits where no signal had been delivered yet, that meant the
operator's *first* Ctrl-C hit a watcher built for their second: it printed
"interrupted again", threw the receipt away and exited. Pressing Ctrl-C once to
get a prompt back destroyed the one artifact this feature exists to produce, and
said they had done it twice. An interrupt during the ending now means "stop
waiting", not "stop recording"; only a second one exits without a receipt.
Verified live three times out of three.

The same round found that the join-time ticket check — itself a fix from the
previous round — went the whole way into the box, costing a connection to the
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
ceilings nothing had ever driven, an accounting sweep of every counter, and —
the one that found most — a review from the **joiner's** side, asking what a
hostile *sharer* can do to the person who pasted their ticket.

That last direction had never been examined. It found that the joiner's
handshake had no deadline on any of its three steps, so a sharer who simply
never answered left `h5i join` hung with nothing printed at all; that a page
served on the joiner's loopback could register a service worker, which outlives
the share and keeps control of that address afterwards; that a ticket's
addressing went to iroh unexamined, so one naming `127.0.0.1:2375` made the
joiner dial a service on its own machine; and that the QUIC close reason, which
the sharer chooses, was printed to the joiner's terminal unsanitised — the same
escape-injection the `box_id` fix had just closed, through the field next to it.

The fuzzer needed a round of its own, too. Measured against the real parser,
1.9% of its heads were parseable, **none** of two million carried both framings,
and about one per run carried a credential — so "twenty million heads pass" was
true and meant almost nothing. Sampling the line ending once per head rather
than once per line, and leaving two thirds of heads unmutated, took those to
18%, 0.8% and 0.8%; the test now asserts floors on all three, so a generator
that stops reaching the code fails instead of passing.

**Rounds 27 to 36** kept changing the lens. Two more directions had never been
looked at, and both paid: a review from the **joiner's** side (what a hostile
*sharer* can do to the person who pasted the ticket) and one of **how a live
share interacts with the rest of h5i** — the lifecycle verbs, the export, the
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
a shell and starts another — or who has a read-only observer attached while
they restart — left the share serving a namespace nothing was in, with
`share ls` reporting it healthy. It compares the namespace now.

Third, and the same argument for the third time: the wire had four reply codes
and no way to say "h5i cannot reach the box". The receipt learned to tell that
apart from "your dev server is down" in round 19; the joiner's browser was
still being told to go and ask the sharer to start a server that was running.

**The three findings rounds 27-36 recorded and did not fix** are fixed now, and
each was verified by reproducing it first.

`cloudflared` outlived a `SIGKILL` of the share by more than twenty seconds,
with its public `trycloudflare.com` hostname still registered and still
pointing at a loopback port that had just been freed — so for that window
anything on the machine that bound it was on the public internet under a
hostname h5i minted. `kill_on_drop` is a destructor and `SIGKILL` skips
destructors; `PR_SET_PDEATHSIG` is the kernel doing it instead. Measured: gone
in one second, against twenty-plus with the change removed.

`h5i box rm` did not know what a share was. A shared box is almost always also
`running`, so the operator was told to abort the box and never that somebody
outside was connected to it — and the check has to sit *above* the status guard
or it is unreachable. Worse, a share that outlived the removal wrote its
receipt afterwards, and `receipt::append` creates the directory it writes into:
the box came back as a receipt log and a payload under a path with no manifest,
which every tool answers "no environment named that" for and only `rm -rf`
clears. The receipt is skipped when the box is gone, which loses it — the right
trade, since it is evidence about something that no longer exists.

And the console showed nothing at all while a box was open to somebody. The
receipt lands when the share *ends*, so the one lane that lets somebody **in**
was the one lane the console could not see while it was open. `shared_now` is
on the box row now.

The pattern across all fifteen rounds is worth recording, because it is the
argument for having run them: **every round found real defects in the previous
round's fixes**, and five of the sharpest were fixes that did nothing at all — a
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

**Still not demonstrated.** The two h5i processes were on one machine: a real
direct QUIC path through the host's network stack, but not two machines on two
networks. `h5i box share` is also Linux-only in practice — `view::box_pid` finds
nothing on macOS, so the command refuses there, and the Seatbelt path through
the dialer has never run. And `--direct-only` has never been exercised against a hole punch that
actually *fails* — the refusal is the half that matters and it needs two hostile
NATs to reach. Those two are what remains of the exit criteria.

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
  credential proxy. **Not yet demonstrated end to end** — this development host
  has no nested virtualization, so the adapter is unit-tested against its argv
  and rule translation and has never booted a real guest here.
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
  protections — which is the one place `--tunnel`, on a public origin, is
  actually the safer of the two.
- **Browser mediation is enforcement, not containment.** The socket mediator
  (M8) decides every verb the agent's CLI sends, which is the threat the
  control lock was written for: an agent that does not know a human took the
  wheel. It is not a boundary against an agent that goes looking, because the
  daemon runs *inside* the box and a box has no internal privilege boundary —
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
  rewrite it. That is bought today by sealing — the receipt store sits outside
  every write grant the box has — plus two host observed fields for cross
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
  so it binds no port and needs no token — the forward's token exists because
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
   hold it. What is *not* wired is the other direction — `control::check` exists
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
     socket we do not own — mediation gone, with no error anywhere. It is
     refused unconditionally.
   - **`launch` is not a page change.** The CLI prefixes every command with
     it, so classifying it as mutating refuses it during a takeover and takes
     every read-only verb down with it — the opposite of 5.4's rule that
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
   h5i's listener binds *before* the box runs — waiting for a daemon first
   would mean the box's own first call finds the mediated path empty and
   starts an unmediated daemon on it — and connects upstream lazily.

   Verified in a real supervised box: `agent-browser open` works through the
   chain, the real daemon's socket lives in the private directory while the
   visible one holds only mirrored files, a read passes through, and
   `agent-browser eval` comes back
   `✗ \`evaluate\` is denied for this box by its profile's browser action
   policy (fail-closed)` with `browser mediation (2 action(s), 1 refused)` on
   the receipt log.

   Two more findings from that run. **Not every agent-browser word is a
   command** — `url` and `status` are not, and using one to start the daemon
   fails silently and leaves no daemon and no clue; `open about:blank` is the
   cheap start that works. And **a box whose repo lives under `/tmp` cannot
   see its own shim**: the per-env `/tmp` scratch shadows the host path the
   shim sits on, `agent-browser` falls through to the system binary, and
   mediation is bypassed with nothing to indicate it. That is the same
   shadowing the M4 notes record, arriving somewhere new.

   Related and now much smaller: **snapshot handle staleness across a takeover**
   is modelled — `needs_resnapshot` is set on the take, survives a session that
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
   binary later fills with no new plumbing", and that is **wrong** —
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

## 12. The browser: a local engine that runs script, and the order to build it

> **The work is in [`ROADMAP_BROWSER.md`](ROADMAP_BROWSER.md)** as of 2026-08-09.
> This section stays the authority on *scope and why*; that document is the
> authority on *order*, and carries the bindings backlog, the security items
> script introduced, and the assessment of Thalora as a source to read rather
> than adopt.

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
determinism and turned out to matter more than the speed — it is the same
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
