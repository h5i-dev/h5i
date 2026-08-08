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
if the log is not running, the request does not happen, and script for
untrusted origins is absent by construction rather than disabled by flag.

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
3. **The reading engine.** A crate of ours: Blitz and Stylo for parse and
   layout, a capped JS engine for the minority of pages that need script, and
   fetch wired directly into the egress proxy's stack so the receipt *is* the
   network log. Beyond fail-closed logging, an owned engine can bind what no
   mediator can: a human-approved form submission mints a **single-use
   capability** for that origin and those fields, page script cannot spend it,
   and every request carries its provenance (agent, human, or page script) as
   a structural fact instead of an inference over event timing. **Assembled,
   not written**: the component stack is the same open-source Rust Kitesurf
   builds on, and the build-versus-adopt call waits for Kitesurf's open-source
   drop before choosing which pieces are ours.

What we do not do is write a rendering browser. Section 7's argument is
unchanged: Chromium plus agent-browser stays the fidelity path. The light
engine earns its place only as the reading path, on the strength of receipts
and a containment story Chromium structurally cannot give us.

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

**M9. Second engine — proposed (7.1).** `engine` as a profile field passed
through to agent-browser, Lightpanda as the first non-Chromium value. Exit
criteria: a `browser-lite` box with no Chromium installed answers `doctor`,
snapshots a real documentation site, and the full loop's failure modes on a
light engine are written down here. No routing yet: one engine per box.

**M10. The reading engine — proposed, gated.** The h5i-native crate of 7.1
step 3. With M8's Fetch lane already delivering best-effort request receipts
on Chromium, the engine's case is what mediation cannot give: fail-closed
logging (no running log, no request), script absent by construction for
untrusted origins, and the single-use form-submission capability with
structural provenance. Gated on two things: M9's findings say a light engine
is actually usable for the reading half of the loop, and Kitesurf's
open-source release has landed so the build-versus-adopt call is made with the
code on the table, not the blog post.

**M11. The developer-mode viewer — proposed, after M8.** The terminal
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

   **Candidate answer, 2026-08-07: the mediated socket (7.2, M8).** The
   daemon's NDJSON control socket is a fourth option the original list missed,
   and it is the only one the agent cannot route around: a PATH shim can be
   bypassed by calling the binary by path, a convention enforces nothing, and
   the socket is the one door every verb walks through. The open part is now
   the sidecar lifecycle, not the interception point.

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
4. **How origin routing selects an engine (7.1 step 2).** agent-browser is one
   engine per daemon session, so "loopback gets Chromium, the web gets the
   light engine" needs either two sessions with h5i choosing at navigate time,
   or the mediation layer of 7.2 doing the choosing per action. The second is
   cleaner and is one more reason M8 goes first, but neither is designed, and
   the policy surface (where in the profile the routing rule lives, and what
   its default is) is unwritten.
5. **Build versus adopt for the reading engine (M10).** Kitesurf's announced
   open-sourcing decides how much of the M10 crate is ours to write. Until
   that drop, the only commitment is the shape: fetch through our proxy,
   receipts as the network log, script off by default for untrusted origins.
