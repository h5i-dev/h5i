# ROADMAP: h5i as a secure, auditable browser for AI agents

Status: in progress, 2026-08-27. **This document is the scope authority.** It
supersedes the "contained agentic development environment" positioning, which
itself superseded "auditable workspaces / provenance". Both are kept in
[`docs/roadmap-history.md`](docs/roadmap-history.md), because both describe real
machinery that is still shipped and tested. What changed is which part is the
product.

The one-liner:

> Give an AI agent a browser it can drive and you can audit. Every request is
> policy-checked and written down before the bytes move, and the fetch is
> refused when the record cannot be written.

Nothing was thrown away to get here. The engine (`crates/h5i-browser-light`),
the fail-closed request broker, the egress proxy, the receipt lanes, the control
lock and the box tiers were all built for the environment story and all are
essential to this one. The pivot puts the piece with no equivalent elsewhere at
the front: Playwright and Puppeteer drive a browser, and neither can tell you
what it reached, because neither *is* the HTTP client.

## The three decisions this pivot rests on

1. **The sandbox is opt-in.** `h5i browser open` runs on the host like any other
   headless browser, and h5i says so on the placement line rather than letting
   the word "browser" imply a boundary. Requiring a box up front would make
   hello-world fail on CI, under AppArmor, on macOS and inside a container,
   which is fatal for adoption and buys nothing the record does not already
   give. Containment is `--in <box>`.
2. **The box stays a separate, orthogonal surface.** It is not the browser's
   implementation detail, so it stays agent-facing. `h5i box run -- h5i browser
   open` is ordinary composition, and `--in` is sugar over the same placement.
3. **The lane is earned, not assumed.** A boxed session is `host-observed` only
   when something outside the engine decides what may leave. A box that lets the
   browser reach the whole network corroborates nothing and keeps the
   `engine-claimed` label. See `browser_session::Session::lane_for`.

## The id is not the interface

`h5i browser open` makes a session and points the **default** at it; every verb
that follows lands there. The opaque id (`br_7k2xqa`) is in the record, in
`--json` and in the receipts, because a durable reference has to survive a
rename. It is not what anyone types.

Demanding one on every verb is the shape of a remote-browser HTTP API
(Browserbase returns an `id` and a `connectUrl` for exactly this reason), where
the id exists because the client and the browser share nothing else. Playwright
never shows one because the caller holds the object, and a local CLI is nearer
to Playwright: it shares a filesystem with the browser, so the id can stay where
it belongs. Names are for running several at once (`--session auth`), and a name
is comfortable precisely because it is *not* an identity, so it can be reused
once the session it named has ended.

Two rules fell out of building it, both about not moving under an agent:

- **No "if only one is live, use it".** It reads as helpful and silently
  redirects the next verb the moment a second session exists.
- **The default outlives the session it names.** Following a pointer to a closed
  session is what lets the next bare verb say *"the session you were on was
  closed"* rather than *"no session is open"*. Only a pointer to a record that
  is gone is dropped.

## What is agent-facing, and what is not

| concept | agent-facing? |
| --- | --- |
| session | yes, but usually implicitly: `open` sets the default and verbs follow it. |
| session name | yes, for running several at once (`--session auth`). |
| session id | no. Durable reference, in `--json` and receipts; never typed. |
| tab | yes, when there is more than one page in a session. |
| box | yes, but as a *placement*, never as part of a session's definition. |
| connection, worker, CDP session | no. Internal, and deliberately unnamed. |

The rule the table encodes: a thing that is a session's own implementation does
not get a name in the CLI, and a thing that stands beside a session does.

## Built, 2026-08-27

- `browser_session`: the host-owned registry. Ids that are never reused, five
  states, endings written down, `EXIT_SESSION_GONE`, host-named artifacts, and
  the scrubber every relayed answer goes through.
- `h5i browser` as the front door: `start`, `list`, `status`, `close`, the
  fourteen session verbs, and the control lock moved onto the session.
- `--in <box>`: the engine runs as a *service*, since the writer lock would
  otherwise shut every later verb out of its own box, and verbs are carried in
  over a Unix socket, since every `box run` gets a fresh netns and a port cannot
  be reached from the next run. Preflighted, so a box that cannot hold a session
  says why before anything starts.
- `env::service_start_with_def` and the engine's `--control-socket`, both added
  for the above.

## Open, and honest about it

- **Supervised and container cannot hold a resident process** (h5i-sandbox's
  `spawn_background`, "Idea 3.5"). Those are also the two tiers that enforce an
  egress allowlist on Linux, so today the only tier that both holds a session
  and earns `host-observed` is `microvm`. Closing this is the highest-value
  piece of remaining work: it is what makes the product's central claim
  reachable on an ordinary Linux box.
- **One session per box.** One resident engine, one service name. Enough for
  now; a second would need per-session service names and stream files.

## How this document is laid out

What follows is current: the browser engine (B), policy resolution (P), the
remote runner (R) and runtime detection (D). Each prefix is its
own numbering, so nothing collides with the history document, which holds the
superseded environment positioning (sections 1 to 12) and the engine's build log
(B1 to B22). Live code cites section numbers in both files.

---

# The browser engine

`crates/h5i-browser-light`. Section 12 of
[`docs/roadmap-history.md`](docs/roadmap-history.md) records the *decision* to
build a local engine that runs script; these sections are where it got to.

> **A pure-Rust browser that lives inside the agent's own sandbox, renders on
> demand, and can prove what it did.**

Only the third claim is unique. Pure Rust is a real property, no C toolchain and
a smaller memory-bug surface, but it is a means, and rendering on demand is what
separates this from Lightpanda.

## B1. Where it stands, 2026-08-28

Built and driven end to end: render, snapshot, screenshot and receipts, with
Blitz owning the DOM, Stylo the CSS and vello_cpu the raster. A resident session
several viewers and a control channel share. Cookies over a public suffix list,
persisted only when h5i asks by name. A fenced snapshot, so page text reaches an
agent labelled as data. An action log, and a replay that re-executes it.
JavaScript through Boa, with events, timers and microtasks on a virtual clock
and `fetch` through the broker.

Web Platform Tests, core tier, full fresh sweep 2026-08-28:

    core tier      75.7% (88,199 / 116,471) with the vendored :has() stylo,
                   ~74.5% after its removal. 80% is the next target.
    html/semantics 9,623 · html/dom 62,092 · css/selectors 3,090
    css-conditional 1,601 · custom-elements 2,414 · dom 3,278 · domparsing 384

The `html/dom` figure is bimodal, 58,313 on a loaded machine against 62,183 on
an idle one, because one idlharness file times out. Run the gate on an idle box
before reading a regression into it.

Not cleared: a production React build, which §12.4 of the history document sets
as the bar. What runs is a hand-written application of the right shape.

Where the next ~5,000 subtests live, measured and ranked: the idlharness file
itself (2,628 failing, mostly capability interfaces this engine refuses to
fake), html/semantics' script/img/media/dialog clusters (~6,400), dom's
XML-document family (~600), cssom serialization and scroll geometry, and the
fetch/api JS surface (~400 reachable without wptserve).

---

## B2. Architecture, and the constraints that chose it

Four decisions the compiler or the dependency graph made rather than preference.
Each will look arbitrary later.

**One thread owns the page.** `Page` is not `Send`: Blitz's `BaseDocument` holds
an `Arc<dyn HtmlParserProvider>` and a `Box<dyn FontMetricsProvider>`, neither
thread-safe, so there is no `Arc<Mutex<Session>>` to be had. The page has a
single owning loop and everything else reaches it by channel, which is the right
shape for a multi-driver session anyway.

**The Rust DOM is the single source of truth.** Every JS object naming a node is
a wrapper over a `NodeId`. A second tree inside the engine would let the
snapshot, the paint, the events and the script state drift apart, with nothing
downstream able to say which was right.

**The object model lives in a JavaScript prelude.** Listeners, timer callbacks
and promise resolvers are GC-managed, and holding them Rust-side means tracing
them through Boa's collector. Putting them where Boa already owns their lifetime
leaves a Rust surface of about twenty primitives taking ids and strings, and
turns event propagation into ordinary code rather than a lifetime problem. The
prelude is compiled once per thread rather than per realm.

**Boa is a fork, pinned by revision.** `boa_engine` and `boa_gc` come from
`h5i-dev/boa` at 0.22.0 (`Cargo.toml`), carrying one commit that adds
`bind_to_realm` so a compiled prelude can be reused across realms. The older
0.19 pin, and the ICU version clash with parley that forced it, are gone;
`scripts/check_boa_release.sh` still asks crates.io on every CI run whether a
published boa would do, and fails the build the day one would. Patch both
`boa_engine` and `boa_gc` together, and note that a `Gc` in a `thread_local`
must be `ManuallyDrop` or the thread aborts at exit.

---

## B3. Security: what script bought and what it cost

**Loopback is reachable from a loopback document.** `Policy::check` took only a
URL, and loopback is allowed unconditionally because the box's dev server is the
point. Before script an untrusted page could *cause* a loopback request but not
read the response; with `--script` it could `fetch` the dev server, read the
body, and POST it anywhere in `net.egress`, which is a read primitive against
the code the agent is working on, past a proxy that never sees loopback.
`Policy::check_from(url, document)` closed it: a page served by the dev server
may talk to it, a page from the open web may not. This was a *logic* bug, and
Rust prevents none of them. "Fewer memory bugs" is honest; "safer browser" is
earned by the origin model, not the language.

**Site isolation is the one thing the box does not replace.** Chromium's process
model contains a compromised renderer against filesystem, network privilege,
crashes and cross-origin theft. The box covers the first three at a stronger
boundary and says nothing about two origins sharing one address space. The
answer is `Jar::retain_origin`: the jar is cleared on cross-origin navigation, so
one session holds one origin's cookies and a page is never in the same address
space as another origin's session. Leaving an origin drops its login, and the
snapshot says so rather than letting the agent discover it by being logged out.
`document.cookie` additionally withholds `HttpOnly`.

**The gate is still honoured.** `capabilities.javascript` reports the running
configuration, script is opt-in, and with it off `<script>` elements are inert.

The same-origin policy proper lives in `cors.rs`, added once the `Domain`
attribute turned an unauthenticated cross-origin read into an authenticated one.

---

## B4. What this browser deliberately is not

A disposable sandbox removes most of a browser's surface as a *requirement*, not
as a compromise. None of the following is planned, and each should be refused in
review rather than re-argued.

**Never**: tabs, bookmarks, history UI, downloads manager, password saving,
autofill, extensions, sync, printing, DRM/EME, WebRTC, WebTransport, WebGPU,
WebXR, Bluetooth/USB/Serial/HID/MIDI, camera, microphone, geolocation, sensors,
desktop notifications, push, background sync, Service Workers, Cache Storage,
File System Access, popups, multiple windows, picture-in-picture, fullscreen,
XSLT, FTP.

**Simplified rather than absent**, and always in memory:

* cookies: session lifetime, persisted only when h5i passes `--cookie-jar`
* `localStorage`/`sessionStorage`: small maps, never a file
* history: the current page and a short navigation list
* clipboard: a sandbox-local buffer, never the host's
* dialogs: `alert` to the console, `confirm` from policy, `prompt` refused
* downloads: handed up to h5i as a response, never written as a file

**Not cut, because cutting them makes this a static HTML renderer rather than a
browser**: DOM mutation and query, CSS cascade with flex/grid/position/overflow,
click/input/change/submit/focus/keyboard, promises and microtasks and timers,
`fetch` with redirects and TLS, ES modules, forms, images, web fonts,
navigation, the rendered result, and console plus exception capture.

**No iframes.** Not "same-origin only": none. Each iframe is a second document,
a second script realm and a navigation boundary. It is not a feature, it is a
second browser.

**No vendored engine crates.** A 5.6MB in-tree copy of stylo bought `:has()` in
stylesheets and was reversed by owner decision on 2026-08-28: no WPT arithmetic
pays for a fork this project carries across every stylo bump. The query half of
`:has()` is evaluated in the prelude instead (`withHasMarkers`), so
`querySelector`, `querySelectorAll`, `matches` and `closest` keep it. Stylesheet
rules using `:has()` stay lost until Blitz depends on stylo >= 0.20.

---

## B5. The rule that produced all of it

**Nothing is built until a page asks for it, and an instrument that cannot name
what is missing is fixed before anything it failed to name.**

The claim is deliberately not speed. This class of engine is slower than
Chromium in wall time, and a benchmark table is something anyone can beat by
shipping less browser. What no one else can copy back is proving what the engine
did, because that depends on the engine *being* the HTTP client rather than
being watched by one.

Sections B1 to B22 of
[`docs/roadmap-history.md`](docs/roadmap-history.md) carry the build log: the
corpus runs, the WPT campaigns, the reference engines that were read, and the
reversals.

---

# Policy resolution and the authority validator

Status: shipped, P1 to P4. This was once the tail of a formal verification
effort whose Lean 4 model was removed on 2026-08-28: it cost more to keep in
step with the Rust than it caught, and nothing on a runtime path depended on it.
What follows outlived it, all Rust and all exercised by the normal test suite,
so the claims below are what the code checks rather than what a prover proved.

## P1. The effective configuration, dumped at the apply seam

`policy.resolved.toml` is the digested *intent*. The enforced state is larger:
`ResolvedPolicy` carries serde-skipped fields that never enter the digest and
are still applied as mounts and grants (`ro_binds`, `home_binds`,
`private_binds`, `cache_write`, `work_readonly`, `user_egress_allow`, the
loopback ports, `box_git`). So a reader of the toml alone sees less than a box
gets, and `policy.effective.json` is written at box creation to close that.

**The dump serializes the exact values handed to the mechanism appliers in
`build_confined_command`, never a parallel pretty-printer.** Re-derived by
separate code it would be a brochure, and every check over it would be checking
the brochure. It takes the same structs at the seam where Landlock rules, mount
calls and the seccomp filter are constructed, after `$WORK` expansion and after
`prepare_private_paths` and `prepare_home_state` have run.

Version 1 of a versioned schema, canonically ordered so the digest is stable:
the tier selected and the claim it resolved from; Landlock grants as absolute
paths with read and write rights separately; every bind with source, target and
writability; net mode, egress allowlist with host-side extras, loopback ports
and the AF_UNIX flag; the seccomp template identifier and parameters, the filter
being a fixed artifact per template; rlimits, `env_pass` and the tools
allowlist.

`fs_deny` appears under resolution metadata rather than enforcement, because
Landlock is allowlist-only and `fs_deny` is a preflight refusal on the *policy*.
What can be said is "resolution refuses", never "the kernel denies", and putting
that in the schema keeps the artifact honest by construction.

The dump's digest goes in the capture manifest beside the policy digest, so it
is tamper-evident for the cost of one hash. Linux kernel tiers only, matching
the mechanisms it describes (`crates/h5i-sandbox/src/effective.rs`).

## P2. The per-run translation validator

The dump feeds a check on the resolver itself: re-derive the subset claims from
the *shipped* effective config and the declared policy, independently of the
`compute_effective` code that produced them. Translation validation, the same
shape as checking a compiler's output for one program rather than proving the
compiler, and it catches resolution silently widening a grant.

`fs_authority::validate_grants` records one boolean per claim in the box
manifest as an `AuthorityVerdict`, rendered by `box status`:

- `fs_subset` — every effective grant was authorized by the declared policy.
- `writes_confined` — every read-write grant was declared writable.
- `cache_readonly` — the config-lock pin and warm cache stay read-only. Private,
  home-state and the one cache-rw refresh bind are writable by design.
- `symlink_clean` — no grant, bind source or mountpoint beneath the worktree
  resolves out through a planted symlink. `None` when the host was not measured.
  Evidence, reported separately, not part of the gate.

`AuthorityVerdict::confined()` gates on the three statically-decidable claims,
where a false is a real bug and safe to fail a launch on.

**Fully opt-in.** With `H5I_FS_AUTHORITY_ENFORCE` unset nothing executes: no
computation, no host measurement, no manifest field, no gate. Setting it to `1`
computes the verdict at create and run and fails closed. Earning trust before
gating by default is the discipline, so flipping the default is a decision with
a receipt trail rather than a drift.

Two bounds. `no_shared_writable`, whether a box shares a writable path with
another live box, is not a single-run property: deciding it needs a lock or an
atomic registry snapshot over all live boxes, or two boxes race into a shared
`/tmp` between their checks. It is a cross-box obligation on the registry
(`effective::interferes`). And backend representability is not a subset question
but whether a backend can express a constraint at all, since enforcement points
differ per tier (kernel: nft plus the proxy; microvm: msb's coarser on/off;
macOS: SBPL carries no network proof). An unrepresentable constraint is marked
unenforced, never rendered as enforced and never silently downgraded.

## P3. Mount realization audit: plan-check plus a read-back

A check on the plan says the plan is safe, not that the kernel realized it. For
mechanisms whose output is a syscall stream there is no argv to re-parse, so the
plan-level check leaves the serializer-bug class one layer down.
`mount_audit.rs` narrows it: after setup and before `exec` the supervisor reads
back the child's realized state and diffs it against the plan, aborting the
launch and landing in the receipt on a mismatch. It reads mount ID and parent,
major/minor, mount root, ro/rw and nosuid/nodev/noexec, propagation flags,
per-target object identity via `statx`/`fdinfo`, the inherited-fd inventory,
`NoNewPrivs` and the seccomp mode.

Two bounds, because "complete mediation" would overstate it. It is a
mount-topology and identity audit: `/proc/<pid>/mountinfo` does not expose the
installed Landlock ruleset or seccomp filter, so the fs-grant enforcement itself
is not read back. And it detects a large slice of the TOCTOU class rather than
all of it, turning mount-swap and masked-path realizations (the shape of runc's
2025 CVEs) from "prevent perfectly" into "detect and fail closed", while a
symlink race leaving topology unchanged is prevented by P4 instead.

**The audit needs an explicit exec barrier.** `Command::pre_exec` runs setup and
execs in the same breath, with no point for a second party to look. So the child
completes setup and **stops**, on a `SIGSTOP` or a blocking wait on a pipe, the
supervisor audits, and only on success sends *go*. Without that barrier "audit
before exec" has nowhere to stand.

## P4. Race-free mount construction

The audit is a net; prevention is the floor under it. Two disciplines in the
setup code:

- **Resolution.** Every path the privileged setup opens on the adversarial
  worktree goes through `openat2` with `RESOLVE_NO_SYMLINKS` and
  `RESOLVE_BENEATH`, then fd-relative operations only, so a path already checked
  is never looked up twice.
- **Mount by handle.** `openat2` alone does not remove races in path-based
  `mount(2)`, whose source and destination are re-resolved by string. Where the
  kernel allows, setup uses `open_tree`, `mount_setattr` and `move_mount`, so
  the object mounted is the object checked by descriptor identity.

An attacker acting *between* two steps is the case these exist for, and P3's
read-back catches the residue.

---

# The remote runner

Status: R13.1 built, 2026-08-16. R13.2 to R13.4 are not built. These sections
are the authority on design; the order and what landed against it are in
[`docs/roadmap-history.md`](docs/roadmap-history.md). The design was drawn
against two codebases read in full: the E2B spec repo and bhatti, a Go
single-node microVM sandbox service.

> **The box's boundary becomes a machine you own and can afford to lose. The
> product does not move: the repo, the policy, the credentials, and the patch
> gate stay here.**

## R1. Placement, not a tier

A second axis on every box, orthogonal to the tier it already declares:

```
placement:  local | runner:<name>
isolation:  workspace | process | supervised | container | microvm
```

**A runner requires Linux and the h5i protocol, nothing else. Everything past
that (isolation tiers, container runtime, KVM, memory, storage, persistence,
its own internet route) is an advertised capability, and a capability the
runner lacks is a refusal, never a silent weakening.** There is no fallback
ladder across machines.

The MVP builds one cell, `runner × container`. The kernel tiers are coherent
there and deferred for real work rather than principle: they assume the
worktree backend even locally, so they wait on a copy-in workspace path that
does not exist anywhere yet. `runner × microvm` waits until the container cell
has earned it. When they land, note that on a sacrificial runner the tier
protects the runner's *other* boxes and its own state machinery; the machine
boundary is what protects you, so a weak tier on a strong boundary is a
legitimate configuration for weak hardware.

No device class is named anywhere on purpose: the capability report, not the
hardware, is the vocabulary.

The security claim is that the agent's execution moves to hardware whose
compromise you have priced in, while the working tree, the credentials, the
receipts store and the apply step stay on the machine that never runs agent
code. The converse is that this does not make the box harder to escape; it
changes what an escape reaches.

Not a hosted sandbox service, a scheduler or a fleet: one developer, machines
they own. Against Coder or a self-hosted E2B the differentiator was never the
remoting, it is that the far end returns a reviewable patch and evidence rather
than a live filesystem you trust by default.

## R2. Related work: take the wire shapes, refuse the planes

Two codebases were read in full, and what each contributed is in
[`docs/roadmap-history.md`](docs/roadmap-history.md). The decisions they
produced are R4's and R5's; one finding is load-bearing enough to sit here.

bhatti moved its internal API off loopback TCP onto a unix socket after a
sandbox reached the daemon's loopback listener. The forced command over SSH
stdio is the end of that trajectory: **no listener anywhere, of any kind,
ever.**

What both references were refused is the same thing twice: the plane.
Control-plane REST, an in-guest HTTP daemon, tokens minted at create, a
bearer-token listener and a WebSocket relay all exist because their clients and
sandboxes meet across the public internet. Ours meet across an SSH session we
already authenticated.

## R3. The cut: the worker is h5i

A thin `h5i-worker` driving podman while the real logic stays here is wrong
three times over. **Argv is path-laden**: `container::build_run_argv` is pure
but full of local paths, so built here it reasons about another machine's
filesystem and built there it needs the policy-to-argv logic, which *is*
`h5i-sandbox`. **The egress proxy must run where podman runs**, since the
container tier wires `HTTPS_PROXY` to the slirp4netns address meaning "the
machine podman runs on"; running the existing `container::run` path unchanged
means the MVP needs zero egress redesign. **The binary is already the
distribution**, since boxes exec `/usr/local/bin/h5i` today. That last is an MVP
decision rather than a permanent constraint: a slim worker build is a cargo
feature set away, and the protocol never learns the difference.

```
this machine (control plane)          runner (worker)
  repo, worktrees, env branches         the isolation backend it advertises
  manifests, policy resolution          the box volume (the only copy
  receipts store, the console             of the source over there)
  credentials, secrets broker           the egress CONNECT proxy
  export gate, apply                    a state dir with lease files
  h5i runner pair/probe/gc              h5i runner serve-stdio
```

The worker is stateless across invocations: box state lives in podman and the
state dir, not in a daemon. On this side, placement is consulted at the three
dispatch sites in `sandbox.rs` *before* the tier match. No backend trait is
invented for it.

## R4. Transport: SSH, a forced command, one session per RPC

Mostly a list of things not built.

- **No custom listener, no TLS, no tokens.** The runner's `authorized_keys`
  gets one line, `restrict,command="h5i runner serve-stdio" ssh-ed25519 …`,
  against a dedicated keypair generated at pair time. `restrict` kills shell,
  port forwarding, agent forwarding, X11 and pty allocation in one word.
- **The client shells out to `ssh`** rather than linking a library, inheriting
  `~/.ssh/config`, the agent and ProxyJump. The invocation is pinned hard: the
  pair key with `IdentitiesOnly=yes`, a per-runner `UserKnownHostsFile` recorded
  at pair time, `StrictHostKeyChecking=yes` forever after. That is the mutual
  authentication the share ticket model was never designed to provide.
- **One SSH session is one RPC.** Concurrency is OpenSSH's ControlMaster, about
  ten milliseconds per session against a warm master, which deletes request ids,
  channel numbers and interleaving bugs from the MVP protocol entirely.
- **The pty rides in frames, not in SSH.** `restrict` disables pty allocation
  and nothing re-enables it; the worker allocates the pty around `podman exec`.

WAN comes later and is not this transport: R12.

## R5. The frame protocol

bhatti's frame, kept because two hundred lines that survived production beat
anything designed fresh: `[u32 BE length][u8 type][payload]`, length excluding
the prefix, a hard 1 MiB cap, every frame written with one write. JSON payloads
for control types, raw bytes for stdio. The codec is transport-free, like
`h5i-share`'s `wire.rs`, so it is testable over an in-memory pipe.

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

- **`EXEC_STARTED` is the mandatory first frame** of an exec stream. "It
  spawned" and "here is output" are different facts, so the first gets a short
  handshake timeout, the stream then lives under the long timeout, and reads run
  under an idle clock. Three clocks, never one.
- **`EXIT` carries what the receipt needs**: exit code, wall and cpu time, max
  RSS, and the `EgressSummary` from the worker-side `ProxyHandle`, in the same
  struct the local path produces so the receipt writer does not fork.
- **`ERROR` on create carries the tail of the worker-side log.**
- **`HELLO` is static, `PROBE` is dynamic, and neither does the other's job.**
  `HELLO` exchanges what never changes within an install; there is no
  negotiation, the lower protocol version governs, and both sides gate features
  by named constants so a worker too old fails at probe time rather than
  mid-create. Everything that drifts belongs to `CAPABILITIES` and nowhere else.
- **Identity never rides in a frame.** `runner_id` is computed on this side from
  the host key SSH verified against pinned known_hosts. The worker may echo it
  as a sanity check, and the echo is never identity-bearing: a value the peer
  asserts about itself is what pinning exists to make irrelevant.
- **Transfer reuses `DATA`/`DATA_DONE`** behind a JSON header frame, with
  `DATA_DONE` carrying the SHA-256 the receiver verifies before acting.
- **Limits are per RPC, not just per frame.** The frame cap bounds one message
  and nothing stops a peer streaming forever, so every RPC class carries a
  receiver-enforced total. The sender's declared size is a claim, and the
  receiver aborts the moment it is exceeded.
- Commands are argv arrays end to end. A shell is asked for by name, never
  implied by the protocol.

## R6. Pairing, probing, and where runner config lives

```
h5i runner pair pi5 user@192.168.1.50
h5i runner probe pi5
h5i runner list | gc <name> | unpair <name>
```

`pair` generates the dedicated Ed25519 keypair at mode 0600, installs the
forced-command line (over existing SSH access, or by printing the line to
paste), records the host key into the per-runner known_hosts (trust on first
use at pair, strict forever after), and runs `HELLO` plus a first `PROBE`. It
succeeds against **any Linux machine that speaks the protocol**: the only hard
failure is no `h5i` on the far side. Everything else lands in the report:

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

Pair records the report and does not judge it; `box create` enforces it.

**Identity is the key, not the name.** A label can be re-paired to a different
machine tomorrow, so `runner_id = SHA-256(host public key)` is what the manifest
and every receipt record. A reinstalled machine with a fresh host key is a fresh
identity, which is correct: it *is* a different trust anchor.

**The account is part of the boundary.** `restrict` binds *our key*, not the
machine; every other key, account and sshd setting is whatever the admin left.
So the docs specify a dedicated OS user and `pair` offers to create it: no
password login, no sudo, no supplementary groups, a clean environment, the
forced command by absolute path. `probe` warns on what it can see. None of this
is enforcement h5i can promise, and the docs must not conflate "the pair key is
constrained" with "the account is".

Runner config is **host-scoped, never in the repo**: which machines *this*
developer can reach is a fact about this machine, like the user egress
allowlist. A profile may later carry a human-facing label, which resolves to
`runner_id` before the manifest is authored; only `runner_id` is digested.

`probe` is `box probe` one machine over, and for **every tier the runner
advertises** it must end by running `verify_exec` functionally. A runner whose
advertisement its own kernel cannot back gets it corrected, loudly. Present bits
are not a working confined exec, and the probe is where that lesson stays paid.

## R7. Create: copy in, one machine over

Remote create *dissolves* the hardest local problem instead of carrying it: the
identical-path git-plumbing binds exist only because a local box shares the host
repo's worktree inodes, and a remote box shares nothing.

1. Create checks the request against the capability report, refusing with the
   capability named. The stored report is a cache of the last `PROBE` and the
   client-side check exists for good errors; the worker refusing at create time
   is the enforcement. Then the front half of `env::create` runs unchanged: pin
   `base_commit` and `base_tree`, create the env branch, write the manifest. No
   worktree. The manifest carries `runner_id` in
   `validate_imported_manifest`'s object-id loop beside `base_commit`,
   `base_tree` and `policy_digest`, as a 64-character hex check, fail-closed.
   The display name sits beside it for humans: the box is bound to the machine,
   not the label.
2. This side builds a **git bundle**: `base_commit`, shallow allowed, plus one
   synthetic commit when the box starts from dirty state. A bundle rather than a
   tar because the bundle *is* the base identity, verifiable on receipt and
   incremental when a later phase re-syncs.
3. `CREATE_BOX` carries the box id, image, limits, serialized resolved policy
   and bundle digest, with the bundle following as `DATA` frames. The worker
   verifies the digest and materialises into a box-owned directory, never a bind
   mount of anything on the runner. A remote create makes the box (source,
   policy, lease); the container is made when there is something to run in it
   (R13.3), since the container tier is `podman run --rm` per command and has no
   warm form. That also suits the hardware this aims at, a warm container idling
   on a small runner costing memory for nothing. When it lands, copy
   `microvm::guest_name`'s rule: the container's name is a digest of its own
   create argv, so a config change forces a fresh one by construction.
4. `CREATE_RESULT` echoes **the digest of the policy the worker actually
   enforced**, and this side refuses to mark the box live unless it matches.
   Cheap, and it converts "the worker silently ran an older policy" from a
   possibility into a detected fault.

Create is crash-safe by state, not by hope. The worker builds under
`creating/<operation_id>` and an atomic rename to `live/<box_id>` is the one
moment a box exists, so there is no state in between for a crash to invent. A
re-sent `CREATE_BOX` whose request digest matches returns the existing result,
so a lost response costs a retry rather than a duplicate; a matching id with a
different digest is refused. Orphaned `creating/` entries carry a short TTL and
fall to the normal sweep.

Secrets keep the microvm tier's argv discipline: nothing secret in remote argv
or environment visible in the runner's process table.

## R8. Exec and shell

`env::run` and `env::shell` become an `EXEC` RPC carrying argv, cwd, the
already-filtered env, an optional pty size, and a timeout the worker clamps to
its own default and maximum. Output streams back as `STDOUT`/`STDERR`, or
`PTY_OUT` when a pty was asked for. Pty against pipes is one flag on the same
RPC, discriminated by frame type; in pty mode there is no `CLOSE_STDIN`, there
is Ctrl-D, because that is what a terminal is.

Disconnect semantics: the **container** survives a dropped session, the **exec**
dies with it, which is what happens locally when h5i is killed mid-run.
Reattachable execs are a later capability the frame layout leaves room for.

Concurrency: worker invocations are separate processes, so the lock is a file
lock in the box's state dir. `CREATE_BOX`, `DESTROY_BOX` and `EXPORT_BOX` take
it **exclusive**, `EXEC` takes it **shared**. An export attempted while execs
hold it is refused with the live execs named, because an export racing a build
reads a torn tree, and a torn tree that passes validation is worse than a
refused RPC. Nothing waits silently.

## R9. Export: quarantine the objects, author the commit here

Export is the trust boundary, so this is the careful one. `env::diff` already
has a no-worktree branch that diffs `base_tree` against the env branch tip
through the object store.

1. `EXPORT_BOX`: the worker commits the box's tree in the runner-side clone and
   returns a bundle of `base_commit..tip`, an archive of exportable untracked
   artifacts, and its receipt spool.
2. This side unpacks into a **throwaway bare repository with its own object
   database**, never the host repo. A ref namespace is not a quarantine:
   fetching writes untrusted objects into the shared store, and a ref only
   quarantines reachability. The throwaway gets `git bundle verify`,
   `transfer.fsckObjects`, and the structural checks that only make sense before
   anything is trusted: bundle size and object count against the R5 limits, a
   blob ceiling, path length, symlink and hardlink entries flagged, and no tree
   entry that traverses.
3. The host takes the **tip tree, not the commits**. The mediated-commit scans
   run against the `base_tree`-to-fetched-tree diff inside the throwaway,
   violations are filtered, and only the surviving tree's objects are
   materialized into the host repo as **one host-authored mediated commit**. The
   runner's history and authorship are discarded by construction. This needs
   `mediated_commit` refactored to accept a tree source instead of a worktree,
   the single invasive change to existing code in this part.
4. Downstream is untouched. A remote box that cannot complete the fetch degrades
   to the detached-box posture that already exists: export-only, no apply.

## R10. Evidence: the runner-observed lane

A remote execution observed by the worker is host-observed *from the runner's
point of view*, and it arrives here over a wire. Folding it into
`HOST_OBSERVED_LANES` would overclaim, since a compromised runner kernel can
forge it; calling it box-claimed would underclaim, since the box cannot edit it
and the channel is mutually authenticated with pinned keys.

So it is a third thing with an honest name, **`runner-observed`**: observed from
outside the box, by an h5i we authenticated, on hardware we do not control. The
entire security claim of this part is one sentence: *runner-observed collapses
to box-claimed exactly when the runner host is compromised, and the runner host
is the machine you chose to be able to lose.* The `Grade` axis is unchanged.

Receipts are written on this side from the `EXIT` and `EXPORT_RESULT` payloads.
No signing is added, because none exists locally either and a signature from a
machine the threat model already sacrifices is not evidence.

## R11. Lifecycle without a daemon

No resident process means nothing watches a clock, so the reaper is
opportunistic. Every box carries a **lease**, a file in the state dir and a
label on the container, default TTL two hours and hard TTL twelve, refreshed by
any RPC that touches the box. **Every worker invocation reaps expired boxes
before doing its own work**, the same sweep-on-entry pattern
`sweep_invalid_worktree_registrations` uses, plus an explicit `h5i runner gc`.
Reaping stops the container, snapshots a partial export bundle and the receipt
spool, and deletes after a grace window; when the snapshot fails it keeps the
box and says so. There is no heartbeat protocol, because there is no daemon to
keep alive.

Persistence is a capability, not a requirement. A `persistent_boxes: false`
runner (read-only OS, tmpfs workspace, one microSD) loses every box at reboot,
and the protocol treats that as a lease that expired early: the next contact
reaps the record and anything not yet exported is honestly gone. Separate
filesystems for OS and box storage is the recommended shape on persistent
runners, so a box that fills its disk takes the state partition rather than the
machine; a pairing-time warning, not something h5i can enforce.

## R12. What the MVP refuses, and what comes later

Refused, fail-closed, with the reason in the error:

- **Profiles that need the secrets broker or the auth proxy.** Both exist to
  keep secret values on this machine, and shipping them to the runner to keep
  the feature working would invert the point. The later design is a credential
  channel: a dedicated long-lived session carrying muxed connections from the
  runner-side proxy back to the auth proxy here. That channel is the one place a
  mux enters the protocol, which is why it is not in the MVP. Until it exists,
  **no agent that needs model credentials runs on a runner**.
- **Any request past the runner's advertised capabilities**, per R1.

Assumed, and stated so it is priced: **the MVP runner has its own outbound
internet**. Image pulls and package installs leave through the runner's own
CONNECT proxy under the box's allowlist. A runner with no default route is not a
supported MVP topology; it becomes one when brokered egress lands.

Deferred, with their shape already known. **Brokered no-network egress**: the
container gets no network at all and its only egress is a proxy whose upstream
is the credential channel, so raw sockets fail closed instead of bypassing the
CONNECT proxy. When it lands it lands for local boxes too. **WAN transport over
iroh**: a runner ALPN beside the share ALPN, with the pair keys authenticating
above it, reusing the existing QUIC stack without touching the ticket model, and
the runner dials out so no router configuration. **The kernel tiers on a
runner**, blocked on the copy-in workspace path they lack even locally. And
**reattachable execs**, **runner pools**, and **re-sync of a live box's source**.

## R12b. What the adversarial review changed

Eighteen rounds against the branch, 2026-08-17, under this part's threat model:
**the runner may be compromised**, so the interesting direction is runner to
host. Thirty-seven findings, all fixed; the round-by-round record is in
[`docs/roadmap-history.md`](docs/roadmap-history.md). Four rules came out of it
and now govern the code:

- **Never invoke the git CLI in a tree whose configuration is hostile.** A box
  owns its own repository config, and git executes `core.fsmonitor` and
  `filter.<name>.clean` out of it, so staging an export with `git add` let any
  box with a shell run a command as the runner user. `core.hooksPath=/dev/null`
  covers neither mechanism. libgit2 is only half the fix: it runs no commands
  but still honours `core.worktree` and a `gitdir:` pointer, so the export's
  staging must also refuse hostile *redirection*, not only hostile execution.
- **A refspec is not a limit on what a fetch writes.** git follows tags by
  default, so a crafted bundle placed an attacker-named tag object in the host
  repository past every quarantine check. R9's "only commits the host authored"
  was false for tags until `--no-tags` and `--no-write-fetch-head`.
- **Gate on the tier the policy carries, not the one the request declares.**
  `run_with_env` dispatches on the former, so validating the latter let a box be
  recorded as `container` and run every command unconfined.
- **Pin both host-key files and pass `-F`.** ssh consults
  `GlobalKnownHostsFile` too, and a hostile `~/.ssh/config` redirected every RPC
  to another machine with the pin apparently intact. That breaks the
  attestation, not merely the transport, because `runner_id` is what a manifest
  records.

R12's refusal of credential-bearing profiles was also written down and never
implemented: values never crossed, but the runner resolves grant descriptors
against its own environment, so a box could be handed the runner's credential.

Two process lessons worth keeping. Several fixes were themselves wrong, three of
them surviving until a round was spent reviewing the *fixes* rather than the
code, so **reviewing a patch is not the same activity as reviewing a system**.
And one fix's commit message described work its diff never did, which is worse
than the bug: the message is what the next reader trusts.

## R13. The order

The step-by-step order, and what landed against each step, is in
[`docs/roadmap-history.md`](docs/roadmap-history.md).

---

# Runtime detection: a kernel-observed lane

Status: designed and built, 2026-08-19. These sections are the authority on
design; the order is in
[`docs/roadmap-history.md`](docs/roadmap-history.md).

The confinement layer answers "what was the box *allowed* to do". This answers
"what did it actually *do*", from a place the box cannot reach. Everything is
additive: no policy decision changes, no syscall is ever blocked by this code,
and a host that cannot run it loses nothing.

## D1. What is being claimed

> For a run whose receipt carries a `runtime` block with `coverage = "full"`,
> the listed detections are the ones that fired on events the **kernel**
> reported for that box's processes, and `events_lost = 0` means no event was
> dropped between the kernel and the record.

Not claimed: that the list is complete for the *behaviour*, since a signature
only fires on what it models, and a box doing something nobody wrote a rule for
produces a clean list and a nonzero event count, which is why the record carries
both numbers rather than a verdict. Not that it is enforcement; denial is
Landlock, seccomp, the netns and the egress proxy, and it stays there (D12). Not
that it survives a kernel-level adversary, since a box with `CAP_SYS_ADMIN` can
unload the programs. And not that absence of the block means a clean run:
absence means the detector did not run, so the block is written even when it
could not attach, carrying the reason.

## D2. The lane problem this fixes

h5i sorts its evidence into lanes because they are not equally trustworthy:

| lane | who observed it | what it covers | what defeats it |
|---|---|---|---|
| `host-env-run` | h5i, as parent | argv, exit code, rusage, wall clock | nothing, but it sees only the *outermost* command |
| `tee-shim` | a shim in the box | interactive shell commands | `exec` without the shim, a script, any child that does its own work |
| `shell-egress` | the CONNECT proxy | HTTP(S) the box routed through the proxy | anything that dials a socket directly |
| `browser` | the browser in the box | console, page errors, failed requests | closing the browser |
| `runner-observed` | the paired worker | the same as the above, one machine over | the same as the above |

Every one of them either sits at the boundary of the box or lives inside it.
The boundary lanes see the first process and the traffic that chose to go
through h5i; the in-box lane sees what the box chose to report. Between them
sits what an agent's build actually does: the four hundred processes `npm ci`
forks, the `postinstall` that reads `~/.aws/credentials` because the profile
granted the directory, the test that dials a hardcoded IP because the proxy only
ever sees names.

The kernel sees every `execve` whether or not a shim wrapped it, every `connect`
whether or not it spoke HTTP, and every `openat` whether or not the opener
wanted to be seen. It is the first lane that is neither at the boundary nor
inside the box, and the only one a box cannot defeat by declining to cooperate.

Three things that buys. A grant is `fs_read` on `$HOME` and a fact is
`openat("$HOME/.aws/credentials")`, so a profile can be tightened against what
the box *used*. `net.mode = "proxy"` delivers an allowlist only *for clients
that use the proxy*, and on the workspace tier a direct `connect(2)` goes
nowhere near it, which SECURITY.md states and nothing observed. And `tee-shim`
is box-claimed by construction, so there is now a second opinion from a lane
that is not.

## D3. Related work: Tracee and Tetragon, and what not to take

Both solve this at a scale h5i does not have; the full reading is in
[`docs/roadmap-history.md`](docs/roadmap-history.md). Three decisions came out
of it.

**The collector/signature split** is Tracee's: rules never touch a ring buffer
and the collector never learns what a credential file is (D7, D9). **Lineage
lives in the kernel**, not reconstructed by racing `/proc`, which is Tetragon's
idea and D6's scope mechanism, because by the time userspace reads
`/proc/<pid>` a short-lived `postinstall` is gone. And **a dropped event is
reported rather than smoothed over**, which is why `events_lost` sits beside
`events_seen`.

Refused: Tracee's event catalogue, since hundreds of instrumented events need
CO-RE plus a full BTF toolchain and a detector that costs a second toolchain is
a detector nobody builds; the daemon, since the unit of observation here is a
run, not a host; and Tetragon's enforcement, since a detector that sometimes
blocks is a policy layer with unclear semantics and h5i already has one with
clear semantics (D12).

## D4. Why aya, and why the probe is C

**The loader is `aya`**, pure Rust: no libbpf, no libelf, no bindgen, no C
toolchain at link time, and no new cross-compilation story for the musl and
Darwin targets the release matrix already builds. `libbpf-rs` would drag in
libbpf, libelf and zlib as native link-time dependencies; hand-rolling `bpf(2)`
is two thousand lines of ELF parsing aya has already had reviewed.

**The probe is C**, compiled by `clang -target bpf` in the build script, which
is the decision most likely to be questioned since aya has a Rust eBPF frontend.
Three reasons: `aya-ebpf` requires a **nightly** toolchain and the `bpf-linker`
binary, and adding both to the build of an optional feature is a poor trade; the
probe is ~350 lines of straight-line code with no allocation, generics or error
handling, where C's disadvantages are smallest; and every reference
implementation writes its probes in C, so the code is reviewable against them
line for line.

The build script is honest about the toolchain rather than demanding it. No
BPF-capable `clang` means the object is not built, the crate still compiles, and
the loader reports `unavailable` with the reason. `H5I_BPF_REQUIRE=1` turns that
into a build failure, which this lane's CI job sets, so a lane that exists to
prove the probe loads never passes by silently skipping it.

The released binaries do **not** carry the probe: the matrix cross-builds musl
targets in containers with no LLVM, and putting a BPF-capable clang into four
images, for a feature that also needs `CAP_BPF` on the user's machine, should
follow somebody wanting it. `h5i box detect probe` reports that in one line and
prints the `cargo install` that fixes it.

## D5. No CO-RE: the stable-ABI cut

CO-RE exists because `task_struct` changes shape between kernels, so libbpf
rewrites field offsets at load time using the running kernel's BTF. It costs a
`vmlinux.h` generated by `bpftool`, BTF at runtime, and a relocating loader.

> **The probe reads no kernel structure.** It reads only syscall tracepoint
> arguments, which are a stable kernel ABI, and calls only helpers whose
> signatures are stable.

Everything it touches: the `syscalls/sys_enter_*` context, whose layout is fixed
and documented; the `sched_process_fork` and `sched_process_exit` contexts, read
through published field offsets that the loader **verifies at attach time** by
parsing `/sys/kernel/tracing/events/.../format`, so a kernel that moved a field
is refused rather than misread; and the stable helpers
(`bpf_get_current_pid_tgid`, `_uid_gid`, `_comm`, `bpf_ktime_get_ns`,
`bpf_get_current_cgroup_id`, `bpf_get_ns_current_pid_tgid`,
`bpf_probe_read_user[_str]`, `bpf_ringbuf_reserve/submit/discard`, the map
accessors), all stable since 5.8, which is the floor the loader checks for.

The cut costs real things: no `task_struct` walking, so no parent `comm` without
keeping it ourselves, no cgroup *path*, no mount-namespace inode, no file inode
on `openat`. It buys a probe that loads on any kernel from 5.8 onward with no
build-time headers and no runtime BTF, which is the difference between a feature
that works on a user's WSL2 kernel and one that works on the maintainer's
laptop.

## D6. Scope: which events belong to which box

The hard problem is not collecting events, it is knowing which of the host's
events are the box's. Too permissive reports the user's own editor; too
restrictive misses the interesting child. One constraint decides it:

> The scope has to be decided **before the payload exists**. A scope programmed
> after the child is spawned has already missed the `execve` that named it,
> which is the most valuable single event of the run.

That rules out **cgroup id**, exact and cheap but created *inside* the spawn
path and on most hosts not available at all without a systemd user manager that
grants delegation; and **pid namespace**, whose inode comes from `/proc/<pid>`
of a process not yet forked. **The process tree** is the one thing knowable in
advance, because h5i is already running.

So the scope is `pidtree`, seeded with **every task of the h5i process**, not
just the main thread: `Command::spawn` can be called from any thread, and a tree
seeded with one would miss a payload spawned from a worker. The kernel grows the
set on fork and prunes it on exit. Seeding from h5i's own tree leaves two holes,
and the probe's state machine closes both:

1. **h5i's own threads are not the box.** A new task is `PENDING` until its
   first event, which settles it: a task whose tid equals its tgid leads its own
   thread group and is a *process*, while anything else is one of h5i's threads
   and is marked `SELF`. Exact, one comparison, no kernel structure.
2. **h5i's own bootstrap is not the box either.** Between fork and exec the
   child still runs `pre_exec`: applying Landlock, opening ruleset paths,
   setting rlimits. So a task is `PRE` until its `execve`, and in that state only
   the exec itself is emitted. A child *inherits* its parent's post-exec state,
   so a fork-only worker is not silently muted for never having execed.

The config map's `mode` field is where a cgroup or namespace filter would go,
and the probe is written so adding one is additive. Nothing in v1 uses it, so
nothing in v1 ships it.

| tier | coverage | why |
|---|---|---|
| workspace | `full` | the payload is a direct descendant of h5i |
| process | `full` | same, plus everything it spawns |
| supervised | `full` | same, and the supervisor is in the tree too |
| container | `partial` | Podman's `conmon` double-forks and reparents, so the workload leaves h5i's tree; what stays visible is the runtime's own activity on the host |
| microvm | `none` | the workload runs against a *guest* kernel, which a host probe cannot see at all |
| anything else | `none` | an unknown tier is uncovered, never assumed covered |

`partial` and `none` go into the receipt as facts with their reasons attached,
which is the difference between "we looked and found nothing" and "we could not
look". One consequence of seeding from h5i's tree: any process h5i spawns during
the run window is in scope. On the kernel tiers that is the payload and nothing
else; on the container tier it is also the runtime, which is why that tier is
`partial` rather than wrong.

## D7. The event model and the wire format

Twelve tracepoints, one fixed-size event struct, one ring buffer. The struct is
`#[repr(C)]` on the Rust side and a plain `struct` in the probe, with a
compile-time size assertion on each side plus a runtime magic-and-version word
in every event, so a mismatched pair is caught at the first record.

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
throughout, since a variable-size record would need a second length field to
convince the verifier about, for a saving that does not matter at these volumes.

**Volume control lives in the kernel, not userspace.** `openat` is the loudest
syscall a build makes, and shipping every one to userspace to throw away 99% is
how a detector becomes a performance problem people turn off. So the probe
filters `Open` in-kernel to write intent, or a path matching one of a small set
of prefixes loaded from userspace: the credential-path list the signatures care
about, pushed down so the rule's own vocabulary decides what the kernel sends.

## D8. The ring buffer, loss, and back pressure

`BPF_MAP_TYPE_RINGBUF`, 256 KiB by default, read by a dedicated thread that
`poll(2)`s the map fd. The size is a policy knob because a `cargo build` and a
`sleep 1` do not need the same buffer.

Loss is counted, never hidden: a failed `bpf_ringbuf_reserve` increments a
per-CPU counter that the session reads at stop time into `events_lost`. A run
with a nonzero count is neither failed nor clean, it is a run whose detection
list is a lower bound, and the console renders it that way.

The reader thread is bounded by the run, starting before the child spawns and
joined with a timeout so a wedged reader cannot outlive the command. Its channel
is bounded too, so a slow consumer degrades the same way a full kernel buffer
does: into a number in the record.

## D9. The signatures

A signature is a pure function from an event stream to zero or more detections:
no I/O, no clock, no per-event allocation beyond what it stores, and therefore
unit-testable against synthetic streams, which is how all of them are tested
since attaching a probe needs privileges CI does not have. Each has a stable id,
a severity, a one-line description, and a bounded exemplar list so a flood
becomes a count rather than a megabyte. Rules are data: `h5i box detect rules`
prints the table, so what the detector looks for is inspectable without reading
Rust.

**Network**, the family the egress proxy structurally cannot see:

- `net.direct-egress` (**alert**) — `connect(2)` to a routable address on a box
  whose network policy is an allowlist or a denial. On the workspace tier this
  is the *only* thing that would notice. It reports the **attempt**, since the
  probe sees the syscall going in, so a connect a netns refused looks like one
  that succeeded; on a `net.mode = deny` box that is the useful reading.
- `net.raw-socket` (**alert**) — `AF_PACKET`, or `SOCK_RAW` on any family.
- `net.unix-socket` (**notice**) — `AF_UNIX` connect on a profile that did not
  set `unix_sockets`. The supervised tier denies these; nothing else did.
- `net.dns-direct` (**notice**) — connect to port 53 or 853.

**Credentials**, the family a filesystem grant cannot express, because a grant is
a directory and a credential is a file inside it:

- `secret.read` (**alert**) — an open of `.ssh/`, `.aws/credentials`,
  `.config/gh/`, `.git-credentials`, `.netrc`, `.kube/config`,
  `.docker/config.json`, `.npmrc`, `.pypirc`, `.gnupg/`.
- `secret.dotenv` (**notice**) — a `.env`-family file outside the workspace.
- `secret.proc-environ` (**alert**) — `/proc/<pid>/environ` for a pid outside
  the box, the classic read of the host's secrets.
- `secret.h5i-state` (**alert**) — a write-intent open under the box's own
  `.h5i/`, which is the box editing its own evidence.

**Execution**, the shape of a supply-chain payload:

- `exec.from-tmp` (**notice**) — exec of a path under `/tmp`, `/var/tmp` or
  `/dev/shm`.
- `exec.memfd` (**alert**) — `memfd_create` then an exec of `/proc/*/fd/<n>` in
  the same process: fileless execution, and why `Memfd` is collected at all.
- `exec.interpreter-pipe` (**notice**) — a shell exec whose first argument is
  `-c` and whose command line has a download-and-pipe shape.
- `exec.package-manager` (**info**) — npm/pip/cargo/gem/go invoked. Not
  suspicious, and present because "what installed things" is the first question
  asked of any supply-chain incident.

**Privilege and kernel**, uninteresting until they are not: `priv.ptrace`
(**alert**), any attach to a process the box did not spawn; `priv.namespace`
(**notice**), `unshare`/`setns`, which the supervised tier denies outright and
the other tiers did not; `kernel.bpf` (**alert**); `kernel.module` (**alert**);
and `mount.change` (**notice**).

## D10. Where it lands

**The receipt** grows an optional `runtime` block on `ExecRecord`, appended last
and `skip_serializing_if` empty, so every existing record's shape and pinned
digest is unchanged. It carries the lane string (`kernel-bpf`), the scope kind,
the coverage, `events_seen`, `events_lost`, the detections, and `unavailable`
with a reason. `source` does **not** change: the run is still `host-env-run` and
the kernel lane is a block inside it, because the record is about the command
and the block is a second observer of it.

**The console** gains a runtime row per record, badged by the highest severity
and grey when the detector did not run, obeying the honesty model: counting over
receipts, not scoring, and grey means "no evidence", never "clean". **The
export** renders detections for every record it carries, and says so when
coverage is `none` rather than showing an empty list. **The CLI** gets
`h5i box detect probe`, `rules` and `show <name>`.

## D11. Policy surface

```toml
[profile.agent.detect]
enabled = true      # attach the probe for runs under this profile
require  = false    # refuse to run when the probe cannot attach
buffer_kb = 256     # ring buffer size
rules = ["*"]       # rule ids or families to enable; "*" is all
```

All optional, all appended last on `Profile` so no existing canonical
serialization or pinned digest moves. `enabled` defaults to false: turning on a
kernel facility that needs privileges most users have not granted would produce
a fleet of `unavailable` blocks and teach everyone to ignore them. `require =
true` is the fail-closed switch and means what it says, which is the setting for
"I am running somebody else's dependency tree" and off by default because the
failure mode of a mandatory detector on a laptop kernel is a tool that does not
start.

### D11.1. Opt-in at three layers, and the one that is easy to get wrong

| Layer | Switch | Default | What it decides |
|---|---|---|---|
| build | `h5i/bpf` → `h5i-core/bpf` → `h5i-bpf/load` | **off** | whether the binary carries aya and a compiled probe at all |
| host | `CAP_BPF` + `CAP_PERFMON` | not granted | whether it can attach |
| policy | `[profile.X.detect] enabled` | **false** | whether a given box is watched |

What is *not* optional is the evidence types: `h5i-core` depends on `h5i-bpf`
unconditionally with `default-features = false`, so a build with no collector
can still read a receipt written by one that had it. A feature flag that changed
a serialized record's shape would make yesterday's evidence unreadable after an
upgrade.

The subtle layer is the **crate's own default**. `h5i-bpf` was written with
`default = ["load"]` so the main clippy job would lint the loader, and cargo
unifies features across a workspace build, so `cargo build --workspace` pulled
aya and ran clang for every contributor while `cargo install --path .` did not.
"Optional" had two answers depending on how you built. The default is now `[]`,
and the dedicated CI job passes `--features bpf` explicitly.

## D12. What it refuses to do

- **No enforcement, in any form.** No `bpf_send_signal`, no
  `bpf_override_return`, no LSM programs. Not "not yet": a detector that can
  block has to answer for the gap between observing an argument and the kernel
  using it, the TOCTOU that makes syscall-argument enforcement unsound in
  general, and h5i has a policy layer without that gap. The way to keep the two
  unconfused is that this one has no verb.
- **No BPF LSM.** `CONFIG_BPF_LSM=y` is common but `lsm=…,bpf` on the kernel
  command line is not, so an LSM collector would be unavailable on most hosts.
- **No CO-RE, no `vmlinux.h`, no BTF requirement** (D5).
- **No daemon.** The probe is loaded for a run and unloaded when it ends.
- **No privilege escalation of its own.** No `sudo`, no helper install, no
  setuid. It uses the capabilities the process has and names the missing ones.

## D13. Limits, stated up front

1. **It needs `CAP_BPF` and `CAP_PERFMON`** (or root), so on a stock install the
   answer is `unavailable: missing CAP_BPF` plus the `setcap` command. A
   privilege-separated collector, a small setcap'd helper owning the probe and
   streaming over a socket, is the right long-term shape and is **not built**.
   The seam is real: `Watch` is one type for "watching" and "could not watch",
   every caller goes through it, and the config map already carries the `mode`
   field such a collector would need (D6). Only `session.rs` would change.
2. **Linux 5.8 or newer**, for `BPF_MAP_TYPE_RINGBUF`. Older kernels get
   `unavailable`, never a silent fallback to perf buffers.
3. **`sys_enter` arguments are the caller's, not the kernel's resolution.** A
   path is the string the process passed, so symlinks, relative paths against an
   `openat` dirfd, and races between the read and the kernel's use are all
   unresolved. Every path rule is a *heuristic over caller-supplied strings* and
   the record labels the field as such. That is the price of being CO-RE-free.
4. **The container tier is `partial` and the microVM tier is `none`** (D6).
5. **Pid reuse can in principle admit a foreign process** into a `pidtree`
   scope, between an exit h5i has not seen and a fork the kernel reuses the pid
   for. The window is a scheduler quantum and a per-pid generation counter costs
   more than the exposure is worth for an observation-only lane. Stated, not
   fixed.
6. **A box with `CAP_SYS_ADMIN` on the host kernel defeats it.** No h5i box has
   it; the sentence exists so nobody has to work that out.
7. **`sys_enter` sees attempts, not outcomes.** A `connect` the netns refused,
   an `openat` that returned `EACCES` and a denied `ptrace` all look like the
   ones that succeeded. Attaching `sys_exit` would fix it and double the event
   volume for a distinction that on a *confined* box is usually the less
   interesting half: "the box tried to reach 8.8.8.8" is the finding, and what
   stopped it is already answered by the policy.
8. **The read-only `openat` feed is filtered in the kernel** (D7), so a read of
   a credential path nobody listed is not collected and cannot fire a rule.
   `[detect] open_all = true` removes the filter and is honest about the cost: a
   `cargo build` produces six figures of `openat`.

## D14. The order

The step-by-step order, and what landed against each step, is in
[`docs/roadmap-history.md`](docs/roadmap-history.md).
