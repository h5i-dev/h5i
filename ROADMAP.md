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
- **No browser-first demo film.** `docs/demo/` still tells the box story and is
  labelled as such (`docs/demo/README.md`).

## How this document is laid out

What follows is current: the browser engine (B), policy resolution (P), the
remote runner (R), runtime detection (D) and the forum (T). Each prefix is its
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

Status: shipped, sections P1 to P4. This part was once the tail of a formal
verification effort — a Lean 4 model of the policy layer developed beside the
Rust and connected to it by differential testing. That model, its `lake`
package, its CI lane and the DRT harnesses that drove it were removed on
2026-08-28: the model cost more to keep in step with the Rust than it caught,
and nothing on any runtime path ever depended on it. What follows is the
machinery that outlived it, all of it Rust, all of it exercised by the normal
test suite. The claims below are what the code checks, not what a prover
proved, and they are written that way.

## P1. The effective configuration, dumped at the apply seam

`policy.resolved.toml` is the digested *intent*. The *enforced* state is
larger: `ResolvedPolicy` carries runtime-only, serde-skipped fields that never
enter the digest and are still applied as mounts and grants, deliberately
(`crates/h5i-sandbox/src/sandbox_policy.rs`): `ro_binds`, `home_binds`,
`private_binds`, `cache_write`, `work_readonly`, `user_egress_allow`, the
loopback port list, `box_git`. Anything that reads only the toml sees less
than what a box gets.

So there is a second serialization, `policy.effective.json`, written at box
creation, with one rule that is the whole point:

**The dump serializes the exact values handed to the mechanism appliers in
`build_confined_command`, not a parallel pretty-printer.** If the dump were
computed by separate code that re-derived "what we probably applied", every
check over it would be checking a brochure. The serializer takes the same
structs, at the seam where Landlock rules, mount calls, and the seccomp filter
are constructed, after `$WORK` expansion and after `prepare_private_paths` and
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
  fixed artifact per template, so templates are named semantics here);
- rlimits, `env_pass`, the tools allowlist.

`fs_deny` appears in the dump under resolution metadata, not under
enforcement, because it is not a kernel rule: Landlock is allowlist-only and
`fs_deny` is a preflight refusal condition on the *policy*. What can be said
about it is "resolution refuses", never "the kernel denies". Writing that
distinction into the schema keeps the artifact honest by construction.

The dump's digest is recorded in the capture manifest beside the policy
digest. That makes it tamper-evident the same way the policy already is, and
it costs one hash.

Linux kernel tiers (`process`, `supervised`) only, matching the mechanisms it
describes. `crates/h5i-sandbox/src/effective.rs` is the implementation.

## P2. The per-run translation validator

The dump is the input to a check on the resolver itself: re-derive the subset
claims from the *shipped* effective config and the declared policy,
independently of the `compute_effective` code that produced them. This is
translation validation — the same shape as checking a compiler's output for
one program rather than proving the compiler — and it catches the class of bug
where resolution silently widens a grant.

`fs_authority::validate_grants` computes one boolean per claim, recorded in
the box manifest as an `AuthorityVerdict` and rendered by `box status`:

- `fs_subset` — every effective grant is one the declared policy authorized.
- `writes_confined` — every read-write grant was declared writable (`$WORK`
  or `fs_write`).
- `cache_readonly` — no read-only overlay was left writable: the config-lock
  pin and the warm cache stay read-only. Private, home-state, and the one
  cache-rw refresh bind are writable by design and not constrained here.
- `symlink_clean` — no effective grant, and no bind source or mountpoint
  beneath the worktree, resolves out through a planted symlink on the host
  (`fs_authority::symlink_escapes`). `None` when the host was not measured.
  This one is evidence, reported separately, not part of the gate.

`AuthorityVerdict::confined()` is the gating verdict: the three
statically-decidable claims. A false there is a real config or logic bug and
is safe to fail a launch on.

**Fully opt-in.** With `H5I_FS_AUTHORITY_ENFORCE` unset the validator never
executes — no computation, no host measurement, no manifest field, no gate —
so default behavior is exactly as it was before the validator existed. Set
`H5I_FS_AUTHORITY_ENFORCE=1` to compute the verdict at box create and run,
record it, and fail closed on a violation. Earning trust before gating by
default is the discipline; flipping the default is a decision with a receipt
trail behind it, not a default to drift into.

Two bounds worth stating. `no_shared_writable` — whether a box shares a
writable-readable path with *another* live box — is not a single-run property:
it can only be decided against all live boxes under a lock or an atomic
registry snapshot, or two boxes race into a shared `/tmp` between their
checks. It is a cross-box obligation on the registry
(`effective::interferes`), reported separately. And backend representability
is not a subset question but "can this backend represent this constraint at
all": enforcement points differ per tier (kernel: nft plus the egress proxy;
microvm: msb's coarser on/off; macOS: SBPL carries no network proof), so an
unrepresentable constraint is marked unenforced, never rendered as enforced,
and never silently downgraded.

## P3. Mount realization audit: plan-check plus a read-back

A check on the plan says the plan is safe; it does not say the kernel realized
the plan. For mechanisms whose output is a syscall stream (mounts, Landlock
rulesets) there is no argv to re-parse, so the plan-level check leaves a gap —
the serializer-bug class, one layer down. `crates/h5i-sandbox/src/mount_audit.rs`
narrows it: after setup, before `exec`, the supervisor reads back the child's
realized state and diffs it against the plan; a mismatch aborts the launch and
lands in the receipt.

Two honest bounds, because "complete mediation" would overstate it:

- **It is a mount-topology and identity audit, not a full mediation.**
  `/proc/<pid>/mountinfo` exposes mount ID, parent, major/minor, mount root,
  mount point, and ro/nosuid/nodev/noexec/propagation flags — it does **not**
  expose the installed Landlock ruleset or seccomp filter. So the audit
  catches mount-topology, flag, and source-identity mismatches; it does not
  read back the fs-grant enforcement itself.
- **It detects a large slice of the TOCTOU class, not all of it.** It turns
  mount-swap and masked-path realizations (the shape of runc's 2025 CVEs)
  from "prevent perfectly" into "detect and fail closed", but a symlink race
  that leaves mount topology unchanged, or a shared source mutated after the
  read-back, is not caught here — those are prevented by construction in P4,
  and the audit is the net under that discipline, not a substitute.

To be worth its name the audit reads more than `mountinfo`: mount ID and
parent, major/minor, mount root, ro/rw and nosuid/nodev/noexec, propagation
flags, per-target object identity via `statx`/`fdinfo`, the inherited-fd
inventory, `NoNewPrivs`, and the seccomp mode.

**The audit needs an explicit exec barrier.** `Command::pre_exec` runs setup
and then execs in the same breath, with no point for a second party to look.
So the design adds a handshake: the child completes setup and **stops** (a
`SIGSTOP` or a blocking wait on a pipe), the supervisor performs the audit,
and only on success sends *go*; on mismatch it kills the child. Without that
barrier "audit before exec" has nowhere to stand.

## P4. Race-free mount construction

The audit is a net; prevention is the floor under it, and it belongs in the
setup code. Two disciplines:

- **Resolution.** Every path the privileged setup opens on the adversarial
  worktree goes through `openat2` with `RESOLVE_NO_SYMLINKS` and
  `RESOLVE_BENEATH`, then fd-relative operations only — no second lookup of a
  path already checked. That is what closes the check-vs-mount window at the
  resolution layer.
- **Mount by handle.** `openat2` alone does not remove races in path-based
  `mount(2)`, whose source and destination are re-resolved by string. Where
  the kernel allows, setup uses the fd-based mount API — `open_tree` to hold a
  mount subtree as an fd, `mount_setattr`, `move_mount` — so the object
  mounted is the object checked, by descriptor identity, not by re-walked
  path.

These are obligations on the setup's mount steps: an attacker acting *between*
two steps is the case they exist for, and the P3 read-back is what catches the
residue.

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

The step-by-step order, and what landed against each step, is in
[`docs/roadmap-history.md`](docs/roadmap-history.md).


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
what this lane's CI job sets, so a lane that exists to prove the probe loads
never passes by silently skipping it.

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

The step-by-step order, and what landed against each step, is in
[`docs/roadmap-history.md`](docs/roadmap-history.md).


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
