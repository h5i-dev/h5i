# ROADMAP: h5i as a contained agentic development environment

Status: proposal, 2026-08-05. Supersedes the "auditable workspaces / provenance"
positioning for the product surface. Design docs under `roadmap/` stay as
history for the parts we keep. Decisions already taken are in section 10, what
is still open is in section 11.

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
4. **Browser in the box, two interfaces.** Chromium and its profile live inside.
   The agent drives it through a CLI (Playwright underneath). The human watches
   and takes over through a Neko style pixel stream. The host browser never
   connects to the target app.
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
  `msg`, `pr`, `notes`, `push`/`pull`/`share`, `serve`, `resume`, `status`,
  `doctor`, `migrate-remote`, `setup-remote`.
- Modules: `repository.rs`, `blame.rs`, `metadata.rs`, `ctx.rs`, `msg.rs`,
  `team.rs`, `memory.rs`, `prompt_score.rs`, `attention.rs`, `recap.rs`,
  `review.rs`, `risk.rs`, `rules.rs`, `compliance.rs`, `radio.rs`, `pr.rs`,
  `lfs.rs`, `session_log.rs`, `ui.rs`, `server.rs`, `vibe.rs`, `resume.rs`.
- The whole `crates/h5i-orchestra/` crate.
- The embedded web dashboard `web/`, the `plugin/` directory, the `web` cargo
  feature, and the axum dependency.
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
h5i dev .                    # snapshot the current repo into a box
h5i dev <repo-url>           # clone an external repo into a box
h5i dev <pr-url|#N>          # review a PR in a box
h5i dev --new                # empty box, agent builds from zero

h5i dev ls | status <name> | rm <name> | gc
h5i dev shell <name>         # attach a confined interactive session
h5i dev run <name> -- <cmd>  # one policy enforced command
h5i dev view <name>          # open the human viewer for this box
h5i dev export <name>        # inspect and emit patch, report, receipt
h5i dev probe                # host capability report
h5i dev allow <host>         # persistent egress allowlist entry
h5i dev cache ls|refresh|rm  # per project warm dependency caches

h5i browser open <url>
h5i browser snapshot         # accessibility tree plus actionable handles
h5i browser click @button-3
h5i browser fill @input-1 "test@example.com"
h5i browser wait "[data-testid=dashboard]"
h5i browser screenshot [--out <path>]
h5i browser console          # console messages since last read
h5i browser requests         # network log, failures first
h5i browser control status|take|release

h5i skill install|show|path  # write or print the embedded agent skill
```

`h5i env *` stays as a hidden alias through one release, then is removed.
`h5i browser *` is meant to be run **inside** the box by the agent, and is
proxied from the host for debugging.

Snapshot output is an accessibility tree, not HTML, because the consumer is a
model with a token budget:

```
URL: http://localhost:3000/login

@input-1   textbox "Email"
@input-2   textbox "Password"
@button-3  button  "Sign in"
```

Handles are stable within a snapshot and invalidated by navigation. A stale
handle is an error, never a silent mis-click.

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
  `h5i dev`, and the copy in model is container only at first.

### 5.2 Pod layout

A box is a Podman **pod** with a shared network namespace and one shared work
volume:

```
pod h5i-dev-<name>   (rootless, slirp4netns, no host net)
├── agent      : agent image, /work rw, no X, runs Claude/Codex, build, tests, dev server
├── desktop    : Xorg dummy + Chromium + h5i-view + browser control daemon, /work ro
└── (host side): egress allowlist proxy, credential broker, viewer port on 127.0.0.1
```

Why two containers instead of one: the desktop image carries an X server and a
browser, which is a large attack surface and a large image to force on every
agent box. Splitting keeps the agent image slim and lets a headless run skip the
desktop entirely. The shared netns is what
makes it work: the dev server on `localhost:3000` in `agent` is `localhost:3000`
in `desktop`, with no port publishing and no host exposure.

Single container mode stays available as `--browser=inline` for hosts where a
pod is not practical.

### 5.3 Browser control

- Playwright drives Chromium **inside** `desktop`, attached to the same X
  display the viewer captures, so the human sees exactly what the agent is
  doing.
- The CDP port is never published. `h5i browser` talks to a small in pod control
  daemon over the pod's loopback, and that daemon owns the Playwright session.
- Chromium runs with a fresh profile created in the box. No host profile, no
  host cookie jar, no host extension.
- Downloads, uploads, and clipboard resolve inside the box. A download lands in
  `/work/.h5i/downloads` and is subject to the export gate like any other file.
- Chromium's egress goes through the same allowlist proxy as everything else,
  with `NO_PROXY` covering pod loopback so the dev server is always reachable.

### 5.4 Control lock

Neko's `/api/room/control/{request,release,take,give,reset}` is the right
arbitration model, and we reimplement it (7.) rather than invent a different
one. Borrowing the semantics is free; borrowing the Go runtime is not.

- The agent holds control by default.
- A human interaction in the viewer takes control. Automation pauses at the next
  command boundary, and `h5i browser <verb>` returns a typed "control held by
  human" error rather than fighting for the pointer.
- On release, the agent must re snapshot before acting, because the DOM it
  remembers is stale.
- Exactly one automation client per box. Multi agent shared control is out of
  scope.

### 5.5 Credentials

- Model API: the key stays on the host. `auth_proxy.rs` already injects it into
  outbound requests from the box, scoped per runtime, so a Claude box cannot
  reach the OpenAI credential or vice versa. Keep, make it default on rather
  than opt in.
- GitHub: the box gets **capabilities**, not a token. A host side helper serves
  a fixed verb set (fetch a PR head, read issue text, open a draft PR) and
  refuses everything else. Raw `gh` and raw tokens never enter the box.
- Everything else routes through `secrets_broker.rs` with a per grant record.
- Per env HOME state already exists (`prepare_home_state`, `policy.home_binds`)
  and is a copy of the host agent config, seeded once, never written back.
  Audit what that copy actually carries and strip anything credential shaped
  from the seed rather than trusting the copy.

### 5.6 Output gate

`h5i dev export` produces, after showing a summary and asking:

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

So the receipt is append only *by construction*, not by convention:

- Records are written to an inherited **file descriptor**, not to a path. The
  box's init process holds the write end of a pipe to a host side collector; the
  shim writes framed records into it. There is no file in the box to open, seek,
  or truncate, and a fresh `open()` of the same fd number gets nothing.
- The host collector timestamps and orders records. Ordering is the host's, not
  the box's.
- Two fields are host observed and never taken from the box: process exits and
  resource usage come from the supervisor's `wait4`, and egress comes from the
  allowlist proxy's own log. If the in box stream and the host observation
  disagree, the export says so.

What an agent can still do is write *extra* records, or stop writing. Both are
visible: a gap between host observed exits and box reported commands is itself a
receipt finding. We do not claim more than that, and the doc will say it in
these terms.

### 5.8 Warm caches

Cold dependency install is the difference between a 20 second box and a four
minute box, so caches are in scope rather than deferred.

- One named volume per project and ecosystem, keyed by a hash of the lockfile
  set: `h5i-cache-<project>-<eco>`.
- Mounted **read only** into the agent box at the ecosystem's cache path
  (`~/.cargo/registry`, `~/.npm`, `~/.cache/uv`, …). A read only cache is a
  correctness problem for nothing: every package manager falls back to fetching
  what it cannot find.
- Writing to a cache happens only in a dedicated `h5i dev cache refresh` box,
  which runs the install step alone, with egress narrowed to the registry hosts
  and no agent inside it. The cache is populated by a build, never by an agent
  session.
- `h5i dev cache ls|refresh|rm` are the whole surface. A box records which cache
  volume and which digest it used, in the receipt.

This keeps the property that matters: no mutable surface is shared between an
agent box and anything else.

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

## 7. The viewer: Neko's core, reimplemented in Rust

We do not vendor or fork Neko. We reimplement its core as a crate,
`crates/h5i-view`, and treat the upstream Go project at `~/Ref/neko` as the
reference design and the protocol source.

Why reimplement rather than pin an image:

- **One binary.** The desktop container would otherwise need Go, GStreamer,
  PulseAudio, supervisord, and Neko's config surface. Our viewer is the same
  `h5i` binary that is already in the image, so the desktop container is X plus
  Chromium plus `h5i view serve`.
- **Scope is much smaller than Neko's.** Neko is a multi user watch party with
  audio, chat, emotes, member management, plugins, and file dialogs. We need one
  screen, one human, one automation client, no audio, no accounts.
- **The boundary is ours.** The viewer sits inside the security story: loopback
  only, one token, no clipboard bridge unless asked. That is easier to guarantee
  in our own code than to audit into someone else's.

Crate shape:

```
crates/h5i-view/
  capture.rs    # X11 capture, XShm plus XDamage dirty rects   (x11rb)
  encode.rs     # M4: JPEG tiles. M5: VP8 via libvpx.
  input.rs      # pointer and key injection                    (XTEST)
  control.rs    # the control lock, Neko's request/take/give/release semantics
  transport.rs  # M4: WebSocket frames. M5: WebRTC             (webrtc-rs)
  serve.rs      # loopback HTTP plus WS, single token, no auth surface
```

Staging is deliberate: a dirty rect JPEG viewer over a WebSocket is a few
hundred lines and is *enough* for "watch the agent work and take over a form".
WebRTC and VP8 buy latency, and they can land after the loop is proven. What we
borrow from Neko on day one is the protocol and control semantics, not the
pipeline.

Explicitly not in scope: audio, multi user rooms, chat and emotes, member
management, broadcast to RTMP, plugins, and the Neko web client. Our client is a
single page served from the binary.

## 8. Phases

Each phase ends with a green `cargo test` and a demo that runs on a stock
rootless Podman host.

**M0. Freeze and branch.** Tag the last provenance release. Open `dev` as the
integration branch. Land this roadmap.

**M1. Amputation.** Delete section 3.2. Extract `receipt.rs` and `redact.rs`,
cut `env.rs` free of `objects`, `ctx`, `msg`, `team`, `repository`. Exit: `h5i
env create/run/shell/status/diff/propose/apply/rm/gc` all work with no git notes
and no context refs, clippy clean with `--all-targets --all-features`, the
binary builds without the `web` feature because the feature is gone.

**M2. `h5i dev` and copy in.** New command surface with `env` aliased. Copy in
workspace for the container tier, four sources (`.`, repo URL, PR, `--new`).
Export gate replacing `propose`/`apply`, with the fd based receipt stream from
5.7. Exit: a PR reviewed end to end with the host repo mounted nowhere.

**M3. Agent in box hardening.** Broker default on, GitHub capability helper,
credential seed audit, no host creds reachable from a box, verified by a test
that asserts denial rather than by inspection. Warm cache volumes and `h5i dev
cache` land here, since a slow box is what makes people mount their host repo
instead.

**M4. Browser.** Desktop sidecar container, Playwright control daemon, the `h5i
browser` verb set, accessibility snapshot format, console and network capture
into the receipt. Headless: no viewer yet. Exit: an agent fixes a real UI bug
using only `h5i browser` output as its feedback.

**M5. Viewer.** `crates/h5i-view` at the JPEG plus WebSocket stage, `h5i dev
view`, loopback only port with a single token, control lock and the pause and
resume protocol, session recording into the export. Exit: a human takes over
mid run, finishes a form, hands control back, and the agent continues from a
fresh snapshot.

**M6. Skill and story.** `skills/h5i/` written against the real surface, `h5i
skill install` and its embedded copy, `npx skills add h5i-dev/h5i`, docs site
rewrite, one demo video of the full loop, an install that assumes nothing but
rootless Podman.

A stub `h5i skill install` lands earlier, in M2, because from M2 on every box
bootstrap wants to call it. M6 is when the content is written for real.

**Post M6.** WebRTC and VP8 in `h5i-view`, microVM backend, macOS.

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
- **Shared kernel.** Podman and the kernel tiers share the host kernel. Good
  against a runaway agent and against careless dependency code. Not a claim
  against a targeted kernel exploit. A microVM backend is the answer, and it is
  post M6.
- **L7 egress scoping.** The allowlist proxy blocks proxy respecting tooling.
  Airtight L3 or L4 belongs to the hardened tier.
- **Linux first.** Rootless Podman on Linux and WSL2. macOS needs a VM layer,
  and it is not in these six phases.
- **Cost.** A desktop sidecar is heavyweight in RAM and CPU. Headless boxes must
  stay first class, and the browser must be opt in per box.

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
- **The viewer is our own Rust crate.** Neko is the reference, not a
  dependency. Reimplementing the core keeps the desktop image to X plus
  Chromium plus the `h5i` binary, and keeps the security boundary in code we
  own (7.).
- **Warm caches are in scope.** Read only per project cache volumes, written
  only by a dedicated refresh box with no agent in it (5.8).
- **The receipt may be generated in the box**, provided the agent cannot rewrite
  it. That is bought with an inherited fd instead of a file, plus two host
  observed fields for cross checking (5.7).

## 11. Still open

1. **MCP.** Drop `mcp.rs` entirely, or keep a slim server exposing exactly
   `h5i_dev_*` plus `h5i_browser_*`? A host side agent driving a box contradicts
   "the agent is in the box", so the default answer is drop, and the skill is
   the interface instead. Revisit only if the browser verbs prove awkward over
   plain CLI.
2. **Snapshot handle stability.** Handles invalidate on navigation, but SPAs
   mutate the DOM without navigating. Re snapshot on every verb is safe and
   chatty. A cheap DOM revision counter is better and needs a design.
3. **First buyer workflow.** The positioning is broad enough to become a
   platform pitch, which sells to nobody. The launch message should be one
   workflow: run untrusted or AI generated code, see it in a real browser, keep
   it off your machine.
