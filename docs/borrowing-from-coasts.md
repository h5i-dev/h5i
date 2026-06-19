# Borrowing from Coasts to improve `h5i env`

> Working notes — ideas mined from `../coasts/` (the [Coasts](https://coasts.dev)
> project) for the `h5i env` sandbox. See [`environments-design.md`](environments-design.md)
> for the current design and `sandbox-production-roadmap` (memory) for the
> shipped/remaining roadmap.

## Implementation status (v1)

Ideas 0, 1, 2, 3, and 3.5 below have a **shipped v1** (CLI + MCP + tests):

- **Idea 0** — `env list --json`, `env doctor` (per-env enforcement-readiness via
  `verify_exec` + policy/branch/drift health).
- **Idea 3** — `private_paths` policy field (`kind`/`persist`), per-env inode
  isolation via `pre_exec` binds (kernel tiers) + `--mount` (container).
- **Idea 1** — the broker already existed (env:/file: → env/file inject, redacted,
  fail-closed); this pass added the **gated `command:` extractor**
  (`allow_command_extractors`, pinned in the policy digest) and the `env secrets`
  legibility CLI.
- **Idea 3.5** — daemon-free `env service start|stop|status|logs` (pid registry,
  logs-as-captures on stop, start/stop events on `refs/h5i/env`). Service
  declarations are **pinned at create** into an env-local `services.json` whose
  digest is recorded in the manifest — editing the worktree config can't change
  what a service runs (verified at start, fail-closed).
- **Idea 2** — per-service port **injection** (a free host port allocated + passed
  in as `H5I_ENV_PORT_<NAME>`/`PORT`), surfaced by `env ports`. v1 is injection
  only — **no host→box forwarder**, so a port is reachable only if the service
  binds the injected value (rendered as a conditional URL, never a guarantee).
  **Deferred:** canonical `checkout` forwarders, and supervised/container services.

Still deferred: Idea 4 (PTY drive — trap), 5 (dashboard), 6 (shared services),
7 (`env init` scaffold), 8 (harness prompts).

## TL;DR

Coasts and `h5i env` solve **adjacent but different** problems, which is exactly
why Coasts is a good idea-mine:

- **Coasts** = a *runtime ergonomics* orchestrator. Run N isolated copies of a
  full multi-service dev stack in parallel, worktree-aware, with port
  management, a secrets keystore, shared infra, a background daemon, and a live
  web UI. Its explicit stance: **run the agent on the host, share the
  filesystem, keep the runtime convenient.** Confinement is a non-goal.
- **`h5i env`** = a *confinement + provenance* sandbox. Run one command (or an
  interactive agent) inside a fail-closed Landlock/seccomp/userns/container box,
  bind it to a reasoning branch + policy manifest, and produce a
  content-addressed, git-native audit trail. Its stance: **confine the agent,
  record everything, review through git.** Runtime ergonomics are thin (single
  command, single port-less process).

So the borrow is asymmetric: take Coasts' **runtime ergonomics** (secrets,
ports, services, observability, drive-the-agent APIs) and graft them onto h5i's
**confinement-first, local-first** core — *without* importing Coasts' "trust the
host-side agent" posture or its mandatory daemon/Docker dependency.

The capability matrix in `environments-design.md` §1.1 frames the divergence:
Coasts owns *"Workspace/branch isolation"* + runtime convenience; h5i uniquely
owns *"Content-addressed provenance"*, *"Reasoning/context bound"*, and *"Local,
no daemon/root"*. Every idea below is checked against "does this cost us a column
we uniquely own?"

**The sharp framing** (credit Codex): Coasts answers *"How do I run many full
dev environments on one machine?"*; `h5i env` should answer *"How do I let
agents work inside many auditable, policy-bound Git environments without losing
ports, logs, services, or reviewable provenance?"* The practical near-term win
is **not more isolation — it's making isolated environments pleasant enough that
agents and humans actually stay inside them.** Most of what follows is operator
ergonomics grafted onto h5i's trust core, not new confinement.

**The governing principle** (Codex, and the test every idea below must pass):
*Coasts teaches h5i how to make isolated workspaces livable. h5i must translate
each ergonomic feature into a **policy grant + event + capture**, or it should
not ship it.* That's the line between borrowing Coasts' ergonomics and importing
its "trust the runtime" posture.

---

## Idea 0 — The fleet mental model: `env ls --json`, `env status`, `env doctor` (HIGH, lowest risk)

**What Coasts does.** Each instance is one member of a local fleet with a
uniform verb set: `ls`, `run`, `checkout`, `ports`, `logs`, `exec`, `restart`,
`rm`, **`doctor`**. The fleet view + a per-instance dashboard make "what's alive
and is it healthy?" answerable at a glance.

**Why it fits h5i** (Codex's lead suggestion). h5i has strong *per-env*
semantics but a thin *fleet* surface. Low-risk, high-leverage additions:

- **`h5i env ls --json`** — status, agent, slug, code branch, context branch,
  backend, isolation tier, last run, capture count, dirty/base-drift summary.
  Machine-readable so orchestrators (and the arena) can consume it.
- **`h5i env status <name>`** — a high-signal one-env dashboard (policy +
  evidence + base drift already exist; make it the canonical glance).
- **`h5i env doctor <name>`** — *enforcement-readiness* check: Landlock/seccomp/
  netns/container availability, blocked host paths, missing `tools`, stale
  worktree, stale context ref, dirty parent, missing capture objects. This is a
  natural home for the existing **`sandbox::verify_exec`** functional self-test
  and the `env probe` capability output — surfaced per-env, fail-closed, with a
  clear "why this env can't enforce its claim here" message.

These cost us **no** unique column and directly serve the "make staying in the
box pleasant" goal.

---

## Idea 1 — Secrets broker (HIGH; already on the roadmap)

**What Coasts does.** `coast-secrets` is a small, shippable design we can almost
copy wholesale:

- **Pluggable extractors** resolve a secret from a source: `env` (host env
  var), `file` (host path, `~` expansion), `command` (`sh -c`, stdout → value;
  covers 1Password/Vault/etc.), `keychain` (macOS), and a **custom protocol** —
  any `coast-extractor-<name>` on `PATH` receives params as JSON on stdin and
  writes the value to stdout (exit 0 = ok). Dependency-free plugin pattern,
  same shape as h5i's `plugin/h5i-py-parser.py`.
- **Two injection targets** per secret: `env:VAR_NAME` or `file:/path/in/box`.
  File injection writes to a **tmpfs** dir on the host and bind-mounts it in.
- **Encrypted at rest**: AES-256-GCM (the `orion` crate) in a SQLite keystore;
  the key lives in the OS keychain (mac) or a `0600` file (Linux).
- **TTL + re-extraction**, secrets **never baked into the build artifact**
  (injected at instance-create time), and re-injectable without rebuild.

**Why it fits h5i.** Our `secrets=[...]` policy field currently **fails closed**
— "an env can't be given a credential safely" is named the *biggest footgun* in
the roadmap. The roadmap's own "secrets broker shape" already prescribes:
*capability-scoped, audit-only-by-default, named grants, TTL, never inherit env
wholesale, prefer temp-file/fd injection over env vars, redact captures, record
grant id not value, fail closed if a grant can't be delivered.* **Coasts'
extractor + inject + keystore model is a concrete design for making that
documented-but-unbuilt broker real** — the current code surface is
scanner/redaction-heavy, not a full broker, so frame this as *completing* the
broker, not matching an existing one. Adapt with an h5i-specific hardening pass:

> **Treat extraction as privileged provisioning, not in-sandbox code** (Codex).
> The `command`/`custom` extractors **execute arbitrary code on the host,
> outside the sandbox** — they must be gated behind explicit policy opt-in
> (`--allow-secret-command-extractors` or signed/trusted policy provenance), and
> default-allowed extractors are just `file`/`env`/platform-`keychain`.
> Extraction runs in a **broker/provisioning phase before `env run`**, never
> reachable from inside the confined workload; the result is **one-shot
> materialized** into an env-private tmpfs file, mounted **read-only**, lifetime-
> bound to the run/service. The env event records
> `{grant_id, secret_name, extractor_kind, injected_as, ttl/expires_at,
> redaction_policy, policy_digest}` — plus optionally a blinded version
> hash/HMAC for drift detection — but **never the value**.

| Coasts | h5i adaptation |
|---|---|
| Inject `env:` or `file:` | **Prefer `file:`/fd** (env vars leak via `/proc/<pid>/environ`; the process tier already re-grants `/proc` precisely to *prevent* that leak — don't reintroduce it via env-injected secrets). `env:` allowed but linted as weaker. |
| Inject and trust | **Capability-scoped grants** keyed to the identity-validated manifest (like `box_git` grants), never derived from box-writable state. |
| Value in keystore | **Record grant-id, not value, in captures/manifest**; extend the existing secret-redaction scrubber to cover injected secret values in exec captures. |
| Keystore key in `0600` file | Encrypted keystore under `.git/.h5i/`, but the **encryption key lives outside Git refs/objects** (OS keychain where available, else a `0600` local key file with a clear warning) and travels with neither `git push` nor `h5i share push` — secrets must not leave the clone. |
| TTL re-extract on `coast build --refresh` | TTL + `h5i env secrets refresh`; a grant that can't be delivered **fails the run closed**. |

**Concrete shape.** A `[secrets.<name>]` block in `.h5i/env.toml`:

```toml
[secrets.anthropic_key]
extractor = "env"          # or file | command | keychain | <custom>
var = "ANTHROPIC_API_KEY"
inject = "file:/run/h5i/secrets/anthropic"   # tmpfs file, preferred over env:
ttl = "1h"
```

Container tier mounts the tmpfs file at an **identical host path** (the same
trick `box_git` uses so gitdir pointers resolve); kernel tiers Landlock-grant
read on just that file. Every grant is a redacted capture line (`grant_id`,
`extractor`, `inject_target`, never the value).

`env` injection stays as documented **legacy/compat** (env vars leak via
`/proc`, logs, and subprocesses); `file:` is the happy path. This borrow
addresses the roadmap's #1 footgun with a proven design — but it is
security-sensitive provisioning, so design it carefully rather than rushing it.

---

## Idea 2 — Per-env dynamic ports + "checkout" → make the arena physically inspectable (HIGH)

**What Coasts does.** Every running instance gets a **dynamic port** in the
high range (49152–65535), always reachable, plus an injected
`<SERVICE>_DYNAMIC_PORT` env var. Exactly one instance can additionally hold the
**canonical ports** (`localhost:3000`, …) via `coast checkout`, which is *instant*
— it just kills/respawns lightweight `socat` forwarders, no container restart.
So you can run a web server in every worktree-instance and open each one's UI
side by side, and swap which one owns the "real" ports without reconfiguring
clients/webhooks/DB tools.

**Why it fits h5i.** Today `env run` is a single port-less command and `env
compare` ("the arena") ranks N envs by *evidence* — diffs, captures, metrics.
But you **cannot physically open** what each agent's branch produces. If three
agents each build a UI variant, the human reviewer can't see them. Borrowing
per-env ingress ports turns the arena from a metrics table into a **live
side-by-side**:

- `h5i env ports <name>` — show the dynamic port(s) the env's processes bound.
- `h5i env checkout <name>` — socat-swap canonical ports to one env for
  webhook/client/DB-tool workflows. **Make it explicit and auditable** (Codex):
  because it changes host-visible routing, every checkout/uncheckout appends an
  event to `refs/h5i/env/meta` so a reviewer has a timeline of what was reachable
  while the agent worked. Conflict-safe if the port is already occupied.
- `env compare` gains a column of dynamic URLs so a reviewer opens all variants.

Injected var naming follows Coasts (`<SERVICE>_DYNAMIC_PORT`) plus an h5i-scoped
alias (`H5I_ENV_PORT_<SERVICE>`) so env commands can self-discover their port.

**Confinement note (important):** this is **ingress** (host → box), orthogonal
to h5i's **egress** allowlist (box → internet). It does not weaken the net
policy. On the container tier it's a published port; on kernel tiers the process
already binds a host-visible port (no netns when net isn't denied), so this is
mostly bookkeeping + a socat forwarder, not new privilege. Keep it **opt-in**
per profile so the default confined `env run` stays portless.

**v1 stays daemon-free** (Codex). Coasts uses daemon + socat; h5i must not make
that mandatory. v1 only allocates/injects **dynamic** ports for services h5i
already spawns the child for, tracked via the per-env pid registry + lockfile
(Idea 3.5) — no always-on forwarder. **Checkout** (canonical forwarders) is a
later, optional layer; if the canonical port is occupied it **fails closed with
a diagnostic** and never auto-kills an unrelated listener. Note: **ports are
only coherent once h5i knows named services, so ship the service model
(Idea 3.5) first** — ports are an attribute of a declared service, not a
free-floating concept.

---

## Idea 3 — `private_paths`: per-env scratch over lock/cache dirs (MEDIUM-HIGH)

**What Coasts does.** When instances share the same project root they share
**inodes**, so inode-level `flock`/`fcntl` locks collide across instances —
Next.js `.next/dev/lock`, Cargo `target/.cargo-lock`, Gradle `.gradle/lock`, PID
files, single-writer build caches. Mount-namespace isolation does **not** fix it
(`flock` is on the inode, not the mount). Coasts' fix: a `private_paths`
Coastfile field that bind-mounts a **per-instance** directory over each declared
workspace-relative path, backed by container-local storage so each instance gets
distinct inodes. Cleared on worktree-switch, destroyed on `rm`.

**Why it fits h5i.** We've already hit this: the *"Running h5i suite in-box"*
memory notes ro cargo caches and contention when running `cargo test` inside a
box. Concurrent envs of the same repo (the whole point of the arena) will
deadlock on shared `target/` lock files. h5i already does **`pre_exec` bind
mounts** for `config_lock_paths` (ro pins on `$WORK/.claude` etc.) and
identical-path binds for `box_git` — the exact same mechanism, just inverted: a
**writable per-env overlay** dir instead of a read-only pin.

**Concrete shape** — a per-path table with `kind`/`seed`/`persist` (Codex):

```toml
[private_paths]
"target"             = { kind = "cache",   persist = true }
".next"              = { kind = "cache",   persist = false }
"node_modules/.cache"= { kind = "cache",   persist = true }
"/tmp"               = { kind = "scratch", persist = false }
```

Each becomes a `pre_exec` bind of `<.git/.h5i/env/<agent>/<slug>/private/<path>>`
over `$WORK/<path>` (same validation Coasts uses: relative, no `..`, no overlap;
plus h5i's canonicalized-path/symlink-escape checks already in mediated commit).
Paths must be **inside the workspace** unless explicitly marked host-cache *and*
policy-granted. Distinct inodes per env → no cross-env lock contention or
cross-agent cache poisoning. The policy digest + private-path map is recorded in
the manifest/events. Fail-closed on invalid paths; cleared per `persist` on
`abort`/`rm`. Shared database/cache-looking names **warn or require
`--allow-shared-state`** (Coasts' volume-warning heuristic).

**Broaden to a named volume/cache vocabulary** (Codex). Coasts separates
`isolated` vs `shared` volumes and warns loudly on shared *database-looking*
names. h5i already defines filesystem grants; the win is giving common patterns
*names + diagnostics*:

- **`cache`** — scoped-write package caches (`target/`, `.cargo`, `.npm`, `.uv`)
  shared read-mostly across envs of the same repo (faster builds).
- **`scratch`** — ephemeral per-env work, destroyed on `rm`.
- **`private`** — the per-inode isolation above (lock/build-output dirs).
- **`shared`** — only by explicit policy, with a loud warning for DB-looking
  names (a shared DB volume is part of the trust boundary; see Idea 6).
- **Snapshot seeding** — create an isolated volume from a known source at
  `env create`, then detach — useful for seeding test fixtures deterministically.

Isolated-by-default is non-negotiable; `shared` is always an explicit capability
grant recorded in provenance.

---

## Idea 3.5 — Services as first-class env artifacts, *without* a daemon (MEDIUM-HIGH)

**What Coasts does.** Services are observable: `exec`, `logs`, `services`,
`ports`, `stats`. Coasts leans on `coastd` for this.

**Why it fits h5i — and how to do it daemon-free** (Codex). `env run` is a
single short-lived command today; web/worker work needs long-lived processes.
Add a minimal process-supervision model that **stays local-first**:

- Policy manifest declares named commands: `services.web.command`,
  `services.worker.command`, optional `port`, optional `restart` policy.
- `h5i env service start|stop|restart|logs|exec <env> <service>`.
- **No daemon in v1** — a `flock` + child-pid registry under
  `.git/.h5i/env/<agent>/<slug>/services/` is enough (same lock discipline as
  the existing `run.lock`).
- **Logs are captured as h5i objects**, not ad-hoc files — linked from the env
  manifest/events, so they're searchable via `recall` and redaction applies.
- **Service start/stop events append to `refs/h5i/env/meta`** → reviewers get a
  timeline of *what was running while the agent worked*, which is provenance the
  ad-hoc-process world can't offer.

Each service still runs **inside the env's sandbox tier** under the same policy
— this is observability + lifecycle, not a confinement hole.

---

## Idea 4 — Programmatic agent-shell drive API (LATER / OPTIONAL — trap unless fully captured)

**What Coasts does.** `coast agent-shell` can spawn an agent TUI (Claude/Codex)
inside the box and **drive it programmatically**: `input "<text>"` writes to the
PTY master (text + Enter as two writes with a 25 ms gap to dodge paste-mode
artifacts), `read-output` / `read-last-lines N` return scrollback (≤512 KB
replay buffer), `session-status` checks liveness, `tty` attaches interactively.
Multiple shells per instance, exactly one **active**.

**Why it fits h5i.** `env shell` today is interactive-only — a human attaches,
or a single `-- <cmd>` runs. There's no way for an *orchestrator* to feed input
to and read output from a long-lived in-box agent. Pairing a drive API with
h5i's **i5h cross-agent messaging** would let the existing "claude proposes →
codex reviews/applies" loop run as **driven in-box agents piped over `refs/h5i/msg`**
rather than two separate human-launched clones. It also makes `env shell`
scriptable for tests of the agent-in-box profile.

**Why it's a trap for h5i v1** (Codex's pushback, which I agree with). A PTY
drive channel is an **unaudited side channel** unless every input/output frame
is itself a capture object / redacted log and policy-bound. That contradicts the
governing principle (every borrowed affordance = policy grant + event +
capture). So this is **later/optional UI work, not a near-term env improvement.**
If ever borrowed, the bar is: shell sessions are env events; input/output are
capture objects or redacted logs; **secret redaction applies to TTY output**;
and there is **no host-shell attach outside the env boundary**.

**Other caveats:** Coasts is explicit that **OAuth tokens issued for the host
get flagged/revoked when reused from inside a Linux box** — prefer API-key
injection (via Idea 1's secrets broker) for driven in-box agents.

---

## Idea 5 — Opt-in observability daemon / dashboard (MEDIUM, guard the local-first column)

**What Coasts does.** `coastd` is a long-running Unix-socket control plane with a
state DB and port manager; **Coastguard** is a React UI streaming logs, status,
and runtime events over a WebSocket (live agent-shell scrollback, secrets tab
with "Re-run Secrets", shared-services tab, MCP tab).

**Why it (partially) fits h5i.** h5i is CLI/MCP-only and effectively stateless
per-invocation; provenance is reviewed **after the fact** via `recall` /
`inspect` / `compare`. h5i already has a **default-on `web` cargo feature** (axum
dashboard, per the `web-feature-gate` memory) — so there's a foothold. A live
view could stream: in-flight `env run`s with wall/cpu/rss accounting (already
captured), the capture stream, the i5h inbox, and an **arena dashboard** (Idea 2's
side-by-side URLs + Idea's evidence columns).

**Coastguard's tab vocabulary maps cleanly** onto h5i concepts (Codex) — a good
information architecture to copy, rendering data h5i *already* has:

| Tab | h5i content |
|---|---|
| Overview | manifest, branch/context/policy digest, status, base drift |
| Runs | captured commands, exit codes, duration, object ids |
| Services | declared services, pid/status, restart count (Idea 3.5) |
| Ports | dynamic/canonical mappings (Idea 2) |
| Logs | captured service logs + command output |
| Policy | fs/net/secrets/cgroup grants + enforcement tier (+ `doctor`) |
| Handoff | propose/apply status, review brief, commits |

This makes `h5i env` legible to **reviewers**, not just operators.

**The trap (do not import):** Coasts *requires* the daemon (`coast daemon
install`, "you are responsible for starting the daemon manually… every single
time"). The capability matrix lists **"Local, no daemon/root"** as a column
**only h5i owns**. So: keep any daemon/UI **strictly opt-in and stateless-restartable**
— the CLI must remain fully functional with nothing running. The dashboard
*observes* `refs/h5i/*` and `.git/.h5i/`; it is never the source of truth and
never required.

---

## Idea 6 — Shared services for confined integration tests (LOW-MEDIUM)

**What Coasts does.** Shared Services / Shared Service Groups run Postgres/Redis
once (on the host daemon or a per-project DinD) and bridge every instance to them
over a private network, with stable virtual ports — so N instances share one DB
without N host-port collisions.

**Why it (cautiously) fits h5i.** A confined env that needs a database for
integration tests currently has no clean story: net is egress-controlled and
there are no services. An opt-in **"env service"** — a separately-confined
side container that the env reaches over a *restricted bridge* (an **ingress**
allow to one named host-side service, reusing the container tier's proxy plumbing
rather than the internet egress allowlist) — would let a confined agent run real
integration tests without punching a hole to the internet. Lower priority and
container-tier-only; design carefully so it doesn't become a general egress
bypass.

---

## Idea 7 — Lower the adoption barrier: scaffold a policy from the repo (LOW, UX)

**What Coasts does.** "Works out of the box… just a small `Coastfile`," and it
can **boot from an existing `docker-compose.yml`**. Strong onboarding.

**Why it fits h5i.** `.h5i/env.toml` is hand-written today. An `h5i env init`
that **detects language/build/test commands** and proposes fail-closed
`fs/net/resource` defaults (and a `private_paths` guess for `target/`,
`node_modules/.cache`, `.next`, …) would directly serve design §11's *"prove UX
before security depth."* Reuse the existing built-in profiles (`default`,
`agent-*`) as the scaffold's starting point. **Important** (Codex): prefer
*extending the existing env policy/manifest* over inventing a Coastfile-equivalent
— don't fork the config surface.

---

## Idea 8 — Generated harness prompts: keep agents inside the box (LOW, high leverage)

**What Coasts does.** Ships harness-specific setup docs/prompts
(`SKILLS_FOR_HOST_AGENTS.md`, `skills_prompt.txt`, per-harness install prompts)
so an agent *remembers* to run commands through the managed environment instead
of bypassing it.

**Why it fits h5i** (Codex). A confined env only helps if the agent actually
uses it. Add `h5i env harness-prompt <claude|codex|cursor>` that emits
instruction text: create/use the env, run tests via `h5i env run`, inspect
`h5i env status`, never bypass policy, use `propose`/`apply` for parent-branch
handoff. This pairs with the existing `h5i msg setup` / `.claude/h5i.md`
integration and cuts the per-session prompt burden — cheap, and it directly
moves the "agents stay in the box" needle.

---

## What NOT to borrow (posture guards)

1. **Shared-filesystem + host-side agent.** Coasts deliberately shares host
   inodes and recommends running the agent **on the host** (no confinement).
   That's the opposite of h5i's thesis. Borrow the *ergonomics*, never the trust
   model — the agent stays **in the box**.
2. **Mandatory daemon.** Keep "local, no daemon/root" — a column only h5i owns.
   Any daemon/UI is opt-in and non-authoritative.
3. **Docker/DinD-mandatory, macOS-first.** h5i's unprivileged kernel tiers
   (process/supervised) are a differentiator; don't regress to container-only.
4. **"Inject and trust" secrets.** h5i's broker must stay capability-scoped,
   redacted, fd/file-preferred, grant-id-recorded — stricter than Coasts.

---

## Suggested sequencing

Ordered to ship low-risk ergonomics first (reconciled with Codex's staging),
deferring anything that touches the trust boundary until the operability story
is proven.

0. **Fleet + doctor** (Idea 0) — `env ls --json`, better `env status`,
   `env doctor`, harness prompts (Idea 8), UI tabs over existing data. Pure
   read-only legibility, zero new confinement surface. **Free win, do first.**
1. **`private_paths` + volume vocabulary** (Idea 3) — Codex's top pick:
   highest ROI / lowest product risk, reuses the `pre_exec` bind mechanism, and
   removes the boring reason agents bypass isolation (build-cache lock contention
   across concurrent same-repo envs).
2. **Secrets broker** (Idea 1) — unblocks the roadmap's #1 footgun; concrete
   design in hand, but security-sensitive provisioning — design carefully.
3. **Services without a daemon** (Idea 3.5) — pid registry + logs-as-captures +
   service events. *Foundation for ports* — ship before Idea 2.
4. **Per-env dynamic ports** (Idea 2, stage A/B) — `env ports`, injected vars;
   opt-in, ingress-only, daemon-free, only for services h5i already spawns.
5. **Checkout / canonical forwarders** (Idea 2, stage C) — more operational
   complexity; auditable events, conflict-safe fail-closed.
6. **Shared services** (Idea 6) and **`env init` scaffold** (Idea 7) — later;
   shared state is an explicit, provenance-recorded capability.
7. **Optional daemon / UI streaming / agent-shell PTY** (Idea 4 + 5's daemon) —
   last, and only if every frame is captured/redacted/policy-bound.

---

> **Provenance.** Coasts surveyed directly (`../coasts/` README + concept docs +
> `coast-secrets`). Codex consulted via `h5i msg` over two rounds
> (#f34a73ca6585077b → #060e23775b89461a → #ceaefbaedf5ce863), folded throughout.
> Round 1 contributed the fleet/`doctor` framing (Idea 0), the daemon-free
> services model (Idea 3.5), the cache/scratch/shared volume vocabulary, harness
> prompts (Idea 8), the Coastguard tab mapping, and the sharp product framing.
> Round 2 sharpened the boundaries: extraction is **privileged host-side
> provisioning** (gate `command`/`custom` extractors behind explicit opt-in;
> `file:` is the happy path, `env:` legacy); the broker is **documented but not
> yet built** (frame as *completing* it); ports are **daemon-free in v1 and
> presuppose the service model**; `private_paths` gets a `kind`/`seed`/`persist`
> field + shared-state warnings and is the **lowest-risk first ship**; and the
> **agent-shell PTY API is a trap** unless every frame is captured/redacted/
> policy-bound (demoted to last). The governing principle — *every borrowed
> affordance = policy grant + event + capture, or don't ship it* — is Codex's.
> Strong cross-agent agreement on: **no mandatory daemon, isolated-by-default, no
> Coastfile fork, don't regress to container-only.**
