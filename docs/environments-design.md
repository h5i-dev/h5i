# h5i Environments — Design

> **Status:** implemented (`src/env.rs`, `src/sandbox.rs`, `src/supervisor.rs`,
> `src/container.rs`): the workspace tier, the static `process` tier
> (Landlock/seccomp/netns), the **`supervised` tier** (seccomp-notify socket gate
> + an **airtight rootless L3/L4 `net.egress` allowlist** — slirp4netns +
> nftables + `/etc/hosts` DNS pinning; see `docs/supervisor-design.md`), the
> **rootless-podman `container` backend** (L7 `net.egress` proxy), the secrets
> broker + cgroup-v2 limits, the policy file with fail-closed gates, env sharing
> via `refs/h5i/env` + the `h5i_env_*` MCP tools. `env create` is
> **secure-by-default** (auto-picks the strongest runnable tier), and `env shell`
> opens an interactive confined session in the box. The `hardened-container`/
> `microvm` adapters, `openat2` path-allow, and GAAP stage separation remain
> future phases. Authored by `claude`, with design
> contributions from `codex` via Agent Radio (`refs/h5i/msg`), grounded in the
> reference systems under `../sandbox-design-ref/` and three 2026 security
> papers (Sandlock, EscapeBench, GAAP).

An **h5i environment** (CLI noun: `env`) is a single abstraction that unifies
what other tools split into two features — an isolated *worktree* (where an
agent's files live) and a *sandbox* (what an agent's commands may touch). h5i
owns the third thing neither of those is: **durable identity, provenance, and
the review/merge lifecycle.**

---

## 1. Competitive wedge — why this belongs in h5i

| System | What it isolates | What it does *not* bind |
|---|---|---|
| container-use (Dagger) | execution, via git-worktree + container per agent | reasoning, review evidence, durable provenance |
| OpenSandbox / E2B | execution, via container/k8s sandboxes | git-native audit, agent reasoning DAG |
| Sandlock / sandbox-runtime | syscalls, via Landlock/seccomp/bwrap | code branch, provenance, merge lifecycle |

**h5i binds isolated execution to reasoned provenance, review evidence, and a
Git-native audit trail.** That is the wedge. The feature is "container-use with
a reasoning DAG and a content-addressed evidence log," not "another sandbox."

The mechanism for that bind is the **triple fusion** (§3).

### 1.1 Comparison with existing tools and `git worktree`

Detailed, by dimension (drawn from the systems under `../sandbox-design-ref/`):

| Tool | Isolation boundary | Workspace / branch model | Network control | Provenance & audit | Reasoning bound? | Review / merge lifecycle | Footprint |
|---|---|---|---|---|---|---|---|
| **`git worktree`** (native) | **none** — same host/user, shared object store, no syscall/net confinement | native branch + linked worktree | none | git history only | no | ordinary git (manual merge/PR) | zero deps, no root, no daemon |
| **branchfs** (FUSE CoW) | filesystem only (CoW); no syscall/net | instant CoW branches, nested, commit/abort to parent | none | none | no | filesystem merge (first-writer), not semantic | FUSE daemon |
| **Sandlock / sandbox-runtime** (OS-level) | unprivileged Landlock+seccomp / bwrap+seccomp+Seatbelt | none (ephemeral; Sandlock has COW effects) | Landlock TCP / proxy allowlist (host/path) | none | no | none | lightweight, no root/daemon (proxies opt-in) |
| **container-use** (Dagger) | container per agent | git worktree + branch in a fork repo | container net + service tunneling | per-env logs/notes (not content-addressed) | no | git push/pull to fork; apply/abort | Docker + Dagger engine (daemon) |
| **OpenSandbox** | Docker/K8s; opt. gVisor/Kata/Firecracker; egress sidecar | ephemeral container/pod; volumes; pause→OCI snapshot | egress sidecar (DNS filter, nftables) | lifecycle-server logs (not git-native) | no | none (it's a sandbox platform) | K8s/Docker control plane (heavy) |
| **E2B** | managed cloud micro-VM/container | ephemeral cloud sandbox + files API | managed | none git-native | no | none | remote service + SDK (account) |
| **Firecracker / zeroboot** (microVM) | **KVM hardware** (zeroboot: CoW snapshot fork) | VM rootfs / memory snapshot | VM net config | none | no | none | KVM, jailer, VM images (infra) |
| **h5i environments** (this) | **tiered claims:** workspace → process (our Landlock/seccomp/netns) → container → hardened → microvm | **native git worktree** (default), pluggable (branchfs later) | deny\|host (process-v1); domain/HTTP allowlist on supervisor/container | **content-addressed `objects` captures, git-native, audit-reconstructable** | **yes — triple fusion (context branch)** | **`propose`/`apply`, reviewer-selected, PR brief, parallel arena** | git2 (no new deps) for workspace; small pure-Rust for process tier; opt-in shell-outs above |

Compact capability matrix (✓ full · ◐ partial/opt-in · ✗ none):

| Capability | git worktree | branchfs | Sandlock | container-use | OpenSandbox | E2B | Firecracker | **h5i env** |
|---|:--:|:--:|:--:|:--:|:--:|:--:|:--:|:--:|
| Workspace/branch isolation | ✓ | ✓ | ✗ | ✓ | ◐ | ◐ | ◐ | ✓ |
| Syscall/process sandbox | ✗ | ✗ | ✓ | ◐ | ✓ | ✓ | ✓ | ✓ |
| Network egress control | ✗ | ✗ | ✓ | ◐ | ✓ | ◐ | ◐ | ◐ |
| Hardware-grade option | ✗ | ✗ | ✗ | ✗ | ◐ | ✓ | ✓ | ◐ |
| Content-addressed provenance | ✗ | ✗ | ✗ | ◐ | ◐ | ✗ | ✗ | ✓ |
| Reasoning / context bound | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✓ |
| Git-native review lifecycle | ◐ | ✗ | ✗ | ◐ | ✗ | ✗ | ✗ | ✓ |
| Local, no daemon/root | ✓ | ✗ | ✓ | ✗ | ✗ | ✗ | ✗ | ✓ |

**Reading of the matrix.** `git worktree` gives us workspace isolation for free
but *nothing else* — no sandbox, no provenance, no reasoning bind, no richer
review than manual git. That is exactly why h5i builds *on top of* it rather
than replacing it (§4). The dedicated sandboxes (Sandlock, container-use,
OpenSandbox, microVMs) each own one or two columns but **none binds reasoning to
execution or produces a content-addressed, git-native audit trail** — the two
columns only h5i fills. h5i is also the only row that is simultaneously
**local-first, dependency-light, and review-native**, while keeping an honest
upgrade path into the stronger isolation columns via opt-in backends.

---

## 2. Threat model

We design for three escalating adversary classes and are explicit about which
tier defends against which (§7). Grounded in the reference papers:

- **EscapeBench** (*Quantifying Frontier LLM Capabilities for Container Sandbox
  Escape*): frontier models already exploit sandbox misconfiguration — exposed
  `docker.sock`, host bind mounts, privileged/`CAP_SYS_ADMIN` containers, shared
  PID namespace, and known kernel CVEs (Dirty COW CVE-2016-5195, Dirty Pipe
  CVE-2022-0847). **Takeaway: vanilla Docker is not a boundary against an
  adversarial agent.** Defense must be layered and fail-closed.
- **Sandlock** (*Confining AI Agent Code with Unprivileged Linux Primitives*):
  the correct *local* primitive is unprivileged and kernel-enforced — static
  policy compiled into **Landlock** (filesystem + TCP-port scope) and
  **seccomp-bpf** (default-deny syscalls), with a **narrow supervisor** (via
  seccomp user-notification) for the few runtime-dependent decisions. ~5 ms
  startup, ~44× faster than Docker, bare-metal throughput. No root, no daemon.
- **GAAP** (*An AI Agent Execution Environment to Safeguard User Data*): the
  data-exfiltration defense is **stage separation** — fetch-untrusted with no
  secrets ▸ process-private with no network ▸ publish-sanitized. The "lethal
  trifecta" (private data + untrusted content + network egress in one context)
  is defused by per-stage policy, not by one big box.

### Adversary classes

1. **Buggy-but-not-hostile** code (the common case): a test run, a build, an
   `npm install`. Risk is accidental damage and accidental egress. Defended by
   the `process` tier (our own confinement, §6).
2. **Prompt-injected / untrusted-content-driven** agent: may attempt
   exfiltration or destructive acts. Defended by `process` + network egress
   policy + stage separation.
3. **Adversarial frontier agent** actively seeking escape: assume it will find
   misconfigurations and try kernel races. Only `hardened-container` /
   `microvm` tiers claim defense here, and we **fail closed** rather than
   pretend Docker suffices.

**Non-goal:** we do not defend against a compromised host kernel, nor do we
promise multi-tenant hostile isolation below the `microvm` tier.

---

## 3. The triple fusion (the core idea)

An h5i environment is the fusion of three things h5i can already address:

```
   git branch / tree          ← the CODE          (refs/heads/h5i/env/<agent>/<slug>)
 + h5i context branch         ← the REASONING     (refs/h5i/context/<env>)
 + env manifest               ← POLICY + PROVENANCE (refs/h5i/env)
 ─────────────────────────────
 = an h5i environment: a Git-addressed, policy-confined, fully-observed
   unit of agent work.
```

No competitor binds the **reasoning trace** to the **sandboxed execution**.
Because each leg is content-addressed and pinned to an immutable base, an
environment is **audit-reconstructable**: you can prove *which* agent, from
*what* prompt/context, ran *what* commands, against *what* exact base tree,
under *what* policy, with *what* egress decisions, producing *what* diff and
*what* test results.

> **Honesty caveat (per codex):** "audit-reconstructable" is **not** bit-exact
> deterministic replay. We capture what ran and what diff/output resulted; we do
> *not* capture image digests, package-cache state, upstream network responses,
> or clock/randomness. The claim is provable provenance, not a time machine.

---

## 4. Leveraging native `git worktree`

**Decision: the default workspace backend is the native `git worktree`,
driven through the `git2` crate we already depend on. Zero new dependencies.**

`git2 0.20.4` already exposes everything we need:
`Repository::worktree(name, path, opts)` to create, `Repository::worktrees()`
to list, `find_worktree()`, and `Worktree::{validate, lock, prune}` for
lifecycle. h5i is already worktree-aware: `ctx.rs` keeps per-worktree pointers
at `.git/h5i/HEAD` / `.git/h5i/PINNED`.

### Why it's the right primitive

- **Shared object store, isolated working tree + index + HEAD.** Creating an
  env is cheap (no clone) yet each env gets its own checkout and staging area,
  so concurrent agents never clobber each other.
- **One branch per env.** Git refuses to check out the same branch in two
  worktrees; giving each env its own branch (`refs/heads/h5i/env/<agent>/<slug>`)
  satisfies that rule *and* gives us the "N envs from one frozen base" model for
  free.
- **Ordinary-git inspectable.** `git -C <work> status`, `git diff`,
  `git log` all just work — so an env is debuggable without h5i, and richer
  with it.
- **Native lifecycle.** `env gc` maps to `git worktree prune` / `Worktree::prune`;
  a running env holds a worktree *lock* so it can't be pruned mid-flight.

### Placement

Worktrees live at `.git/.h5i/env/<id>/work`. Putting them *under* `.git` keeps
their files invisible to the main working tree (no stray untracked files, never
caught by `git add .`). The env's branch is an ordinary ref, pushable via
`h5i share push` / `git push`. (Verified: native `git worktree add .git/.h5i/env/<id>/work`
is accepted and `list`/`status` work. Still flagged in §13 as an implementation
choice to stress-test against `prune` / `gc` / hooks before locking in — a
sibling directory remains the fallback.)

### The shared-object-store gotcha (matters for confinement)

A worktree's `.git` is a *gitlink file* pointing back into
`.git/worktrees/<id>`, and its `commondir` points at the **shared** object
store and ref database. That is fine at the `workspace` tier (trusted) but is a
hole at `process`+ tiers: a confined process that can traverse the gitlink can
reach other worktrees' refs and the repo's hooks. Two mitigations, by tier:

- **`workspace` tier:** accept the shared store (trusted, no confinement).
- **`process`+ tiers — narrow plumbing grants + mediated commit:** the confined
  filesystem view is `$WORK` + read-only system paths **plus the minimum git
  plumbing that makes the worktree a functional checkout** (`env::box_git_grants`
  — without it every `git`/`h5i` call inside the box dies on EACCES at the
  worktree's `commondir`, which bricks the agent-in-box): rw on the env's own
  `worktrees/<wt>` admin dir, the shared `objects` store (an availability
  trade: a hostile box can vandalize loose objects, recoverable from any
  clone, but cannot move any ref it isn't granted), the agent's own
  `refs/heads/h5i/env/<agent>` + its reflog dir, and `refs/h5i/context`
  (reasoning is a shared advisory record); ro on
  `HEAD`/`config`/`packed-refs`/`refs`/`info` and `~/.gitconfig`/`~/.config/git`
  (git dies — not skips — on an existing-but-unreadable global config).
  Everything else stays sealed: `.git/hooks` (EscapeBench footgun), repo
  `config` writes (`core.fsmonitor`/`hooksPath` would execute host-side),
  `refs/h5i/env` meta and the env's manifest/policy dir (a box that could
  rewrite its own policy could widen its sandbox on the next run). Net effect:
  a boxed agent can `git status`/`commit` on **its own env branch** and use
  `h5i recall context`, but cannot move `main`, plant hooks, or touch another agent's
  branches. The only road into the *parent* branch is still the host-side
  mediated commit of `propose`.
- **`container` tier — same surface as bind mounts:** the worktree's pointer
  files (`$WORK/.git` gitlink, the admin dir's `commondir`/`gitdir`) contain
  *host-absolute* paths, so the backend bind-mounts the identical plumbing
  list at its **identical host paths** inside the box (ro/rw as above; targets
  auto-created on the read-only rootfs overlay, like the shim mounts), and
  dual-mounts `$WORK` at its host path so the admin back-pointer resolves.
  Host HOME is *not* mounted (a missing `~/.gitconfig` is skippable; only an
  existing-unreadable one is fatal — the Landlock case). A comma in any path
  (unrepresentable in Podman `--mount` syntax) disables the whole set rather
  than mounting a partial `.git`.
  - **Mediated-commit invariant (critical security boundary):** h5i computes the
    diff from the worktree *filesystem* and stages/commits host-side against an
    **explicit path allowlist rooted at `$WORK`**. Every staged path is
    canonicalized and **rejected if it escapes `$WORK`** — symlinks pointing
    outside, submodule gitdirs, nested `.git` repos, hardlinks, or `..`
    traversal. The agent never drives `git` directly at this tier, so this
    filter is the only thing standing between "edited a file" and "rewrote a
    parent ref"; it must fail closed.

### Beyond worktree (optional, later)

The workspace backend is **pluggable**: `worktree` (default), and later a
BranchFS-style copy-on-write FUSE backend for instant speculative branches.
BranchFS adds a FUSE dependency and a daemon, so it is explicitly *opt-in and
later* — not part of the MVP.

---

## 5. h5i's own sandbox — minimal dependencies

**Decision: h5i ships its own confinement for the `process` tier, built
directly on unprivileged Linux kernel primitives — no container runtime, no
daemon, no root.** Containers and microVMs are *opt-in adapters* that shell out
to tools the user already has; they add no Rust dependencies.

This is essentially a minimal, embedded Sandlock. We own the policy and the
enforcement; we lean on the kernel, not on Docker.

### The primitives (all unprivileged, all in-kernel)

| Concern | Primitive | Notes |
|---|---|---|
| Filesystem scope | **Landlock** LSM (≥5.13; net rules ≥6.7) | **allowlist only** — grant rw `$WORK` + ro `/usr` `/lib` `/nix`. See the carve-out caveat below. |
| Syscall surface | **seccomp-bpf deny-list** + `PR_SET_NO_NEW_PRIVS` | v1 *denies* the dangerous set (`mount`, `ptrace`, `keyctl`, `bpf`, kernel-module, `add_key`); a default-deny *allowlist* is a later hardened profile, not v1 |
| Network | **`unshare(CLONE_NEWNET)`** | enforces only `net.mode = deny` (empty netns, loopback only) or `host` (full). **Cannot do domain/host allowlists** — that needs the `supervised` tier (now shipped) or a container backend. |
| Mount / fs view | **`unshare(CLONE_NEWNS)`** + bind mounts | restricted rootfs view; hides the shared `.git` |
| PID view | `unshare(CLONE_NEWPID)` | can't see/signal host processes (EscapeBench pid-ns escape) |
| Privilege | `unshare(CLONE_NEWUSER)` + uid/gid maps | rootless; this is how bubblewrap works |
| Resources | `setrlimit` (mem, nproc, fsize, cpu) + wall-clock kill | cooperative caps, no cgroups needed |

> **Landlock is allowlist-only — it cannot subtract a child from an allowed
> parent.** You cannot grant `$REPO` and then "deny `$REPO/.git/hooks`"; Landlock
> has no deny rules. So at the `process`+ tier the sandbox is granted **`$WORK`
> (the worktree, with `.git` excluded from the granted set) + selected read-only
> system paths — not the repo root.** `fs.deny` in a profile (§7) is therefore a
> **preflight lint + secret-scrub scope**, not a kernel mechanism: h5i validates
> that no granted parent contains a denied child and refuses the policy
> otherwise. If a profile needs the original source as a base, expose it
> host-side at setup or as a sanitized read-only snapshot without `.git`.

The `process` tier is **static** (no supervisor): `net.mode` deny|host +
Landlock allowlist grants (`$WORK` + ro system paths) + a fixed seccomp
**deny-list** + rlimits. That already covers adversary classes 1–2 for the
deny-network case. The **narrow supervisor** (seccomp user-notification) —
needed for *host-granular* egress allowlists — is the **`supervised` tier**
(now shipped: it adds the seccomp-notify socket gate and an airtight rootless
L3/L4 `net.egress` allowlist via slirp4netns + nftables; see
`docs/supervisor-design.md`), mirroring Sandlock's static/dynamic split. **If a
profile sets a non-empty `net.egress` allowlist under the `process` tier, h5i
fails closed** (the static backend cannot honor it) and directs the user to the
`supervised` or container backend.

### Dependency budget

The workspace tier needs **nothing new** (git2). For the `process` tier, the
recommended set is small, pure-Rust, no-C-build:

- **`rustix`** *or* **`nix`** — safe wrappers for `unshare` / `mount` /
  `setrlimit` / `pidfd` / `prctl`. (One dependency. `libc` is already transitive
  via tokio/git2 if we prefer raw syscalls and more `unsafe`.)
- **`landlock`** — the official, tiny Landlock crate (encodes the ABI structs so
  we don't hand-roll them). Optional; hand-rolling via raw syscalls is ~2 structs
  + 3 syscalls if we want zero crates.
- **seccomp filter** — a static `sock_filter` array installed with one `prctl`;
  small enough to hand-roll, or use the pure-Rust `seccompiler` crate.

> Honest "is it easy?": a *correct* seccomp+Landlock+namespaces sandbox is not
> trivial — EscapeBench shows misconfiguration is the dominant failure mode. The
> easy-and-safe path leans on `landlock`/`seccompiler` (they encode the kernel
> ABI correctly); the zero-crate path is more "ours" but carries more `unsafe`
> and more audit burden. **Recommendation: thin vetted crates.** We still own
> the sandbox; we just don't reimplement the kernel ABI by hand.

### Capability probing + fail-closed (mandatory)

Hosts vary wildly. *This* dev host (WSL2, kernel 6.6) shows an **empty
`/sys/kernel/security/lsm`** (Landlock likely off) but **unprivileged userns
enabled** — so the same tier is satisfiable differently on different machines.
Therefore:

1. At env creation, **probe** what the host actually supports (Landlock ABI,
   userns, seccomp, the requested container runtime).
2. If the requested isolation claim **cannot be satisfied, refuse** — never
   silently downgrade. The user gets an explicit error and may re-request a
   weaker claim.

### Cross-platform honesty

- **Linux:** full `process` tier (Landlock + seccomp + namespaces).
- **macOS:** `process` tier via the system `sandbox-exec` (Seatbelt) profile we
  generate — weaker, no new dep; or `workspace` only.
- **Windows:** `workspace` + container only.

Security claims are stated **per-OS**. We do not promise rootless parity off
Linux.

---

## 6. Isolation claims (not "security tiers")

Per codex: name the guarantees as **descriptive claims**, so we never
accidentally call Docker "secure."

| Claim | Mechanism | Defends | New deps |
|---|---|---|---|
| `isolation=workspace` | git worktree only | nothing (file isolation only) | none |
| `isolation=process` | worktree + our Landlock/seccomp/netns (§5) | adversary 1–2 | small, pure-Rust |
| `isolation=container` | rootless Podman, dropped caps, no sock | adversary 2 | external binary only |
| `isolation=hardened-container` | gVisor / Kata | adversary 2–3 | external binary only |
| `isolation=microvm` | Firecracker | adversary 3 | external binary only |

A **policy profile** requests a *minimum* claim; the resolved claim is recorded
in the manifest and in every capture. **"Secure" means `microvm` or
`hardened-container`.** Requesting more than the host can provide fails closed.

---

## 7. Policy model (`.h5i/env.toml`, checked in, fail-closed)

```toml
[profile.default]
isolation = "process"            # minimum claim; fail closed if unmet
# Landlock GRANTS (allowlist). At process+ the sandbox sees $WORK, not the repo
# root; the original source, if needed, is exposed host-side or as a sanitized
# ro snapshot without .git.
fs.read   = ["/usr", "/lib", "/nix"]   # ro system paths ($WORK is implicitly readable)
fs.write  = ["$WORK"]                  # only the env workspace
# fs.deny is NOT a kernel rule (Landlock has no deny). It is a preflight LINT +
# secret-scrub scope: h5i refuses the policy if any granted parent contains one
# of these, and uses it to scope redaction.
fs.deny   = ["~/.ssh", "~/.aws", "~/.config/gh", "$REPO/.git/hooks"]
net.mode  = "deny"               # process-v1 enforces deny|host ONLY (netns)
# net.egress is a DOMAIN ALLOWLIST — requires the supervisor or a
# container/hardened backend. Under isolation=process it FAILS CLOSED if set.
net.egress = []                  # e.g. ["pypi.org", "github.com:443"] on supervisor/container backends
secrets   = []                   # nothing inherited; grants are capability-scoped + redacted
resources = { mem = "4G", procs = 256, wall = "30m" }
tools     = ["python", "pytest", "cargo", "npm", "git"]
env.pass  = ["PATH", "HOME", "LANG"]   # env-var allowlist, not full inherit

# Stage separation (GAAP) — later; defuses the lethal trifecta. net.egress here
# implies a supervisor/container backend:
# [profile.fetch]   isolation="container" net.egress=["*"]              secrets=[]         fs.write=["$WORK/untrusted"]
# [profile.process] isolation="process"   net.mode="deny"              secrets=["DB_URL"] fs.read=["$WORK/untrusted"]
# [profile.publish] isolation="container" net.egress=["api.internal:443"] secrets=[]
```

- **Network is first-class**, equal to filesystem, but honestly scoped to the
  backend. `process` enforces only `net.mode = deny | host` (netns). A
  non-empty `net.egress` **domain allowlist requires the `supervised` tier or a
  container backend**, and h5i fails closed if it is requested under the static
  `process` tier. The `supervised` tier (shipped) resolves and pins each host to
  its IP at startup — written into a private `/etc/hosts`, so no DNS port is even
  open (kills rebinding) — and enforces the pinned IPs with an nftables
  default-drop ruleset; the container tier adds an HTTP/HTTPS CONNECT proxy (L7).
- **Secrets are never inherited wholesale.** Grants are explicit, capability-
  scoped, and their *values are redacted from captures* before the manifest is
  written (reuse existing `secrets.rs` scrubbers).
- Default profile is **deny-home, deny-secrets, deny-network** — fail-closed.
  Domain egress allowlists require a backend that can enforce them (supervisor
  or container/hardened); they are not available under the static `process`
  tier.

### The built-in `agent` profile (agent-in-box defaults)

The deny-home `default` profile is right for build/test workloads but bricks
the agent-in-box use case: `claude` and `codex` live under `$HOME`, keep their
state and credentials there, and need egress to their APIs. `agent` is a
second built-in (no `env.toml` required) that adds the minimum surface a
coding agent needs — and it is what an unspecified `--profile` **auto-picks**
when the host can enforce it (same pattern as the isolation auto-pick:
explicit = fail-closed, unspecified = best runnable; hosts that cannot enforce
the egress fall back to `default` with a printed note).

**It is scoped to a single runtime.** A Claude box that could also read
`~/.codex/auth.json` *and* egress to `api.openai.com` lets a prompt-injected
agent steal the other runtime's token and use it against an allowlisted host —
so the grants are split per runtime. `agent` resolves to the creating agent's
runtime (`$H5I_AGENT`: `codex*` → Codex, else Claude); `agent-claude` /
`agent-codex` pin one explicitly. Each variant grants **only that runtime's**
state + API:

- **ro (shared, non-secret):** `~/.local/bin`, `~/.local/lib`, `~/.nvm`, shell
  rc files, `~/.gitconfig`, `~/.config/git` — *plus* the runtime's own
  `~/.local/share/<runtime>` (its installed binary; `~/.local/bin/claude` is a
  launcher into `~/.local/share/claude/versions/…`). The blanket `~/.local`
  read was **narrowed** so the box no longer sees unrelated `~/.local/share`
  state (Jupyter `notebook_secret`, app history DBs).
- **rw (this runtime only):** Claude → `~/.claude`, `~/.claude.json`; Codex →
  `~/.codex`. Plus shared `~/.cache`, `~/.npm`, `/tmp` (host-shared at this
  tier; the container tier gives a private one).
- **net.egress (this runtime only):** Claude → `api.anthropic.com`,
  `statsig.anthropic.com`; Codex → `api.openai.com`, `auth.openai.com`,
  `chatgpt.com` — DNS-pinned + enforced (so the profile **refuses** to
  instantiate at tiers that cannot enforce it: supervised/container by design).
- `TERM`/`COLORTERM`/`USER`/`SHELL` pass through; mem 8G, procs 512

The default deny set (`~/.ssh`, `~/.aws`, `~/.config/gh`, hooks) still applies.
Deliberate trade, stated honestly: the agent can read its *own* credentials —
it cannot function without them — and the egress allowlist bounds where bytes
can go, but it gets **neither the other runtime's credentials nor egress to its
API**. A user-defined `[profile.agent-claude]` (or `[profile.agent-codex]`)
merges over the matching base (fs/env/resource fields inherit; `net.egress`
does **not** — a user profile owns its egress list).

Interactive sessions (`env shell`) additionally differ from captured runs in
two deliberate ways: they keep the caller's terminal session (no `setsid`, so
job control and TUIs work; the wall-clock kill that needed a dedicated process
group does not apply — an interactive session is operator-bounded), and the
supervised socket gate always permits **anonymous `AF_UNIX` socketpairs**
(intra-box IPC that tokio/Node need at startup; `socket(AF_UNIX)` — the
authority-smuggling vector — stays deny-by-default).

#### In-box observation & the Codex launch incantation

Interactive sessions are **observed** so the box leaves an evidence trail of
what the agent ran. Two layers, by what the host can enforce:

- **Hook (both runtimes):** a `PreToolUse`/Bash hook rewrites each tool command
  into `h5i capture run …`. On the **container** tier the Claude hook is pinned
  unkillably via injected **managed-settings** (`/etc/claude-code/…`, ro,
  ns-local). On the **process/supervised** tiers there is no managed tier to
  reach, so instead the agent's config is made immutable in-box — the project
  `.claude`/`.codex` dirs are bind-mounted read-only (blocks editing *and*
  creating a `settings.local.json` with `disableAllHooks`) and the user
  settings files are pinned read-only (`config_lock_paths`).
- **Tee-shim (container):** a generated POSIX shim shadows `/bin/sh`+`/bin/bash`
  and tees every top-level command, independent of any agent cooperation — the
  runtime-agnostic floor (catches Claude's `bash -c` and Codex's `bash -lc`).

**Codex operators must launch with `--dangerously-bypass-hook-trust`.** Codex
*skips untrusted hooks* — "Codex records trust against the hook's current hash,
so new or changed hooks are marked for review and skipped until trusted" — and
only **managed** hooks are auto-trusted. The process/supervised tiers have no
managed tier, and Codex documents **no** non-interactive way to pre-trust a
hook, so an in-box Codex session would otherwise silently skip the wrap-bash
hook. Run it as:

```bash
codex --dangerously-bypass-hook-trust      # inside `h5i env shell <name>`
```

This is deliberately left to the human operator (the trusted party) rather than
forced by h5i: with the config lockdown above, the agent cannot disable or
re-point the hook, so bypassing *trust* (not the hook itself) is safe. The
"dangerous" in the flag name is about running unreviewed hooks generally; inside
the box the policy already bounds what any hook can do (egress allowlist, fs
limits). One-time interactive `/hooks` trust on the host works too (trust
persists across sessions, hash-pinned), but the flag is the reliable per-run
form. Claude has no such gate — a present, immutable hook just runs.

---

## 8. Storage & data model (reuses existing h5i machinery)

```
refs/h5i/env/meta       # shareable env state: append-only event log (created/exec/status/proposed/
                        #   applied/aborted/removed) + manifests.jsonl + policies.jsonl, in ONE tree.
                        #   CAS append + union-merge — identical pattern to refs/h5i/msg and refs/h5i/objects.
                        #   (`…/meta`, not the bare leaf `refs/h5i/env`, so the code refs can nest beside it —
                        #    git forbids a leaf and a directory at the same ref path.)
refs/h5i/env/code/<agent>/<slug>    # the code branch ON THE WIRE — a transport remap of the local
                        #   refs/heads/h5i/env/<agent>/<slug>. A native worktree needs a real local branch,
                        #   but a remote (GitHub) renders every refs/heads/* as a branch; this hidden ns is
                        #   pushable + fetchable yet invisible in branch UIs (like GitHub's own refs/pull/*).
                        #   Beside refs/h5i/env/meta under one refs/h5i/env/ namespace. Push:
                        #   +refs/heads/h5i/env/*:refs/h5i/env/code/*. Fetch (FF-only):
                        #   refs/h5i/env/code/*:refs/heads/h5i/env/*. push also deletes any stray
                        #   refs/heads/h5i/env/* left on the remote.
refs/heads/h5i/env/<agent>/<slug>   # the code branch LOCALLY (worktree checkout); never pushed as a head
refs/h5i/context/<env>  # the env's reasoning branch — ALREADY exists (ctx.rs)
refs/h5i/objects        # the env's EVIDENCE log — ALREADY exists; every exec captured here

.git/.h5i/env/<id>/
  manifest.json         # EnvManifest (small)
  policy.resolved.toml  # the profile as actually enforced (+ resolved isolation claim)
  status                # created | running | idle | proposed | applied | aborted
  work/                 # the git worktree
  egress.jsonl          # supervisor egress decisions (process tier; pointer-summarized into captures)
```

### EnvManifest (small — points at evidence, doesn't inline it)

```rust
struct EnvManifest {
    id: String,                 // env/<agent>/<slug>
    agent: String,              // requesting agent ($H5I_AGENT)
    base_commit: String,        // immutable pinned base
    base_tree: String,
    parent_branch: String,      // git branch this env forked from / proposes back onto
    branch: String,             // the env's own code branch (refs/heads/h5i/env/...)
    parent_context_branch: String, // ctx branch to merge reasoning findings back into
    context_branch: String,     // the env's own reasoning branch (refs/h5i/context/...)
    profile: String,            // profile name
    policy_digest: String,      // sha256 of resolved policy
    isolation_claim: String,    // resolved claim (workspace|process|...)
    backend: String,            // worktree | branchfs | container | ...
    created_at: String,
    status: String,
    captures: Vec<String>,      // object ids in refs/h5i/objects — the evidence
}
```

### The `objects` fit (near-zero new plumbing)

`h5i env run` **is** `h5i capture run`, tagged. The existing `objects::Manifest`
already carries `git_tree`, `branch`, `diff_files`, `exit_code`, and `structured`
findings. We add four fields:

- `env_id` — links the capture to its environment.
- `policy_digest` — what policy was in force.
- `egress` — a **summary + pointer** (counts + a capture/object id for the full
  `egress.jsonl`), never an unbounded inline log (token-reduction principle).
- `redactions` — what was scrubbed.

`refs/h5i/objects` thus becomes the env evidence log with **no new storage
machinery**. Big logs/test output stay content-addressed captures; the manifest
holds only small pointers.

---

## 9. Lifecycle & CLI

```
h5i env create NAME [--from REV] [--profile test] [--backend auto|worktree|container]
                    [--isolation workspace|process|container|hardened-container|microvm]
h5i env run NAME -- <cmd>        # capture-wrapped, policy-enforced; records exit/resource/egress
h5i env shell NAME               # optional PTY (later)
h5i env list | status NAME | log NAME | diff NAME | inspect NAME --capture <id>
h5i env propose NAME [--style review]   # PR brief: diff + tests + captures + policy exceptions + context (pr.rs)
h5i env apply NAME [--patch|--merge]    # reviewer-selected; NEVER auto-writes parent branch
h5i env abort NAME               # stop procs, preserve manifest for forensics
h5i env gc                       # git worktree prune + reclaim; raw blobs GC via existing object policy
h5i env rm NAME [--force]        # permanent removal: worktree + code/reasoning branches + on-disk manifest,
                                 #   and strip manifest/policy from refs/h5i/env (local-only; the append-only
                                 #   `removed` event survives). --force required for a still-live env.
```

Wiring follows the existing noun-verb table in `main.rs` (the same pattern as
`h5i msg` / `h5i recall context`). MCP tools initially mirror only
`create / run / status / diff / propose / apply`.

### Semantics

- **Base is immutable and pinned** (exact tree, not "current dirty working
  tree" unless explicitly snapshotted). The parent branch must not mutate under
  active envs; if it does, h5i detects and offers rebase.
- **No automatic write into the user's branch.** `propose` is the default
  surfacing; `apply` is an explicit, reviewable step.
- **No `env commit`** — it would collide with `h5i capture commit`. Env work commits via
  ordinary git on the env branch (or the mediated commit at `process`+ tiers).
- **Parallel N envs from one frozen base ("the arena").** Default resolution is
  **reviewer comparison** (`h5i env diff` across envs → pick a winner), because
  coding work needs semantic comparison. `--race` first-success mode is opt-in
  for deterministic tasks. h5i is uniquely able to run this: `msg` coordinates
  the agents, `objects` compares each env's test results.

### Lifecycle state machine

```
created ──run──▶ running ──idle──▶ idle
                   │                 │
                   ├── propose ──▶ proposed ──apply──▶ applied
                   └── abort ───▶ aborted (manifest preserved for forensics)
                                   gc ──▶ workspace reclaimed, manifest retained
```

---

## 10. Integration with existing h5i

- **objects** → evidence layer (§8). The single biggest reuse.
- **context (`ctx.rs`)** → the env's reasoning branch; `create` forks the parent
  context, `apply` merges it back. Worktree-pointer machinery already exists.
- **msg (i5h)** → handoff/review: `claude` creates an env → `codex` reviews
  `env diff` + captures → reviewer applies. (This is the exact loop that
  produced this document.)
- **pr.rs / review.rs** → `env propose` generates the review brief.
- **compliance.rs / secrets.rs / audit** → the manifest is the audit artifact;
  secret redaction reuses existing scrubbers.

---

## 11. Rollout (prove UX before security depth)

1. **MVP — workspace tier.** git worktree + per-env branch + `refs/h5i/env`
   manifest + capture-wrapped `run` + `diff` / `status` / `propose`. No
   confinement yet. Proves the UX and the provenance triple. **Zero new deps.**
2. **`process` tier (our own sandbox).** Landlock allowlist grants
   (`$WORK` + ro system paths) + seccomp **deny-list** + netns (`net.mode`
   deny|host only) + rlimits, static policy, capability-probe + fail-closed.
   Mediated commit with the canonicalized path-allowlist invariant. Domain
   egress allowlists are explicitly *not* in this phase. (small pure-Rust deps.)
3. **Policy file + secret/network/resource gates.** Enforce per-OS; fail closed
   on unsupported claim.
4. **Container / microvm adapters.** Opt-in shell-outs (podman → gVisor/Kata →
   Firecracker). OpenSandbox's "admin picks runtime class, user API unchanged."
5. **Remote/share.** Env manifests via `h5i share push`/`pull`; env branch via normal
   git remote. Optional BranchFS COW backend for speculative branches.
6. **Supervisor + orchestration.** seccomp-notify supervisor — **shipped**: the
   `supervised` tier with the socket gate and the airtight rootless L3/L4
   `net.egress` allowlist (slirp4netns + nftables + `/etc/hosts` pinning), plus
   the parallel "arena" / reviewer comparison. Remaining: `openat2` path-allow,
   COW, and GAAP stage-separated pipeline profiles.

---

## 12. Non-goals & honest caveats

- Not multi-tenant hostile isolation below `microvm`/`hardened-container`.
- No bit-exact deterministic replay (we lack image/cache/network/clock capture).
- No cross-platform security parity — Linux is strongest; macOS/Windows weaker
  and labeled so.
- Default caches are ephemeral; package installs can taint a shared cache, so
  named cache mounts are opt-in and captured.
- A correct native sandbox is non-trivial; we mitigate by leaning on vetted ABI
  crates, probing capabilities, and failing closed.

---

## 13. Open questions

- **Name.** `env` (CLI) + "h5i environment" (prose) is the working choice for
  prior-art familiarity (container-use); `lab` / `cell` were alternatives.
- **Mediated vs. in-sandbox commit** at the `process` tier — mediated is safer
  (hides `.git`) but means the agent can't run arbitrary `git` itself. Default
  mediated; allow opt-in direct for trusted profiles.
- **Worktree placement** under `.git/.h5i/env/<id>/work` vs. a sibling dir — the
  former is invisible to the main tree but nests a worktree inside `.git`.
- **Dependency line** for the `process` tier: `landlock` + `seccompiler` + one
  of `rustix`/`nix`, vs. hand-rolled raw syscalls (zero crates, more `unsafe`).
- **Supervisor scope** *(resolved)* — the `supervised` tier ships a default-deny
  seccomp-notify **socket gate** plus an airtight **L3/L4 `net.egress` allowlist**
  (netns + nftables + DNS pinning), not Sandlock's full dynamic per-syscall layer.
  Domain allowlisting is therefore available on `supervised` and `container`; the
  static `process` tier remains `net.mode = deny | host` only. Remaining dynamic
  work (`openat2` path-allow, runtime policy patching) is deferred.
- **`fs.deny` semantics** — confirmed as a preflight lint + secret-scrub scope,
  *not* a Landlock rule (Landlock is allowlist-only and cannot subtract a child
  from a granted parent). Granted paths must avoid any parent that contains a
  denied child; otherwise the policy is refused.
