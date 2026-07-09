# `h5i env` vs. the agent-sandbox landscape (2026)

A full competitive read of `h5i env` against the sandboxing tools that AI
coding agents actually run inside, plus a prioritized set of ideas worth
borrowing and a differentiation strategy. This replaces the earlier
`comparison.md` and `borrowing-from-coasts.md`: it widens the reference set
from 7 projects to 19 local checkouts under `../sandbox`, adds the two
first-party agent harnesses (Claude Code and Codex, whose own sandboxes h5i
must position against), and folds in mid-2026 web research on traction and
end-user pain.

> Method. Nineteen `../sandbox` projects surveyed from source (README, docs,
> architecture files, representative code). Codex analyzed from `../codex`
> (the Rust workspace). Claude Code and the wider market covered by web
> research, with all GitHub star counts re-verified live on 2026-07-09.
> Companion docs still live: [`borrowing-from-shepherd.md`](borrowing-from-shepherd.md)
> and [`borrowing-from-governance-planes.md`](borrowing-from-governance-planes.md)
> go deeper on two idea-mines summarized here.

`h5i env` is the "triple fusion" of a code branch (a native git worktree), a
reasoning/context branch, and a policy-confined, fully-observed execution. The
tiers are `workspace → process → supervised → container`, with
`hardened-container` / `microvm` reserved as external backends (enum slot
present, fail-closed, not built). Provenance is a secret-redacted structured
capture (token-reduced summary plus raw blob) written to `refs/h5i/*`,
shareable cross-clone via `share push`/`pull`. The review loop is
propose / apply / compare (the "arena") / rebase with a mediated,
path-escape-defended commit. Cross-agent coordination rides `refs/h5i/msg`
(i5h). A secrets broker gives scoped, redacted-from-evidence grants. MCP tools
mirror the CLI.

---

## 1. The reference set

Grouped by what each project fundamentally *is*, because the landscape is not
one category. Star counts are GitHub, verified 2026-07-09.

### First-party agent harnesses (the real baseline)

| Project | What it is | Sandbox today |
|---|---|---|
| **Claude Code** (Anthropic) | The agent CLI most h5i users already run. | `/sandbox` mode + `sandbox-runtime`: OS-primitive fs/net confinement, L7 proxy allowlist, no container. macOS Seatbelt, Linux bwrap+socat, Windows alpha. |
| **Codex** (OpenAI, 96k★) | The other big harness. | Per-command sandbox: Seatbelt / bubblewrap (primary since 0.115) / Windows restricted-token; L7 MITM egress proxy; Starlark execpolicy; LLM "guardian" reviewer. |

### OS-level confinement (h5i's process/supervised peers)

| Project | Stars | Lang | One-line |
|---|---|---|---|
| **sandbox-runtime** ("srt", Anthropic) | 4,610 | TS | The OS-primitive sandbox behind Claude Code: bwrap/Seatbelt/WFP, host-canonicalizing L7 proxy, optional MITM. |
| **fence** | 842 | Go | Container-free per-command sandbox; net-deny default; argv-aware seccomp-notify exec gate. |
| **secure-exec** (Rivet) | 929 | Rust/TS | In-process V8-isolate runtime; virtual POSIX FS (blake3-chunked CoW); language-level, not OS-level. |

### Container / platform sandboxes

| Project | Stars | Lang | One-line |
|---|---|---|---|
| **OpenSandbox** (Alibaba-origin) | 11,927 | Python | General agent-sandbox platform; container default + gVisor/Kata/Firecracker/CLH; L3/L4+L7 egress with runtime `/policy` + deny webhook. |
| **container-use** (Dagger) | 3,906 | Go | Containerized dev env per git branch, driven by MCP; git-notes provenance; multi-scheme secrets. |
| **sandboxd** | 714 | Go | Self-hosted app-builder backend; hardened runc + nftables egress + SQLite audit; preview URLs. |
| **CubeSandbox** (Tencent) | 9,286 | Rust | Cluster microVM service (RustVMM); eBPF L3/L4 + Lua L7 MITM with per-request redacted JSONL audit; E2B-compatible. |

### microVM / VM substrates

| Project | Stars | Lang | One-line |
|---|---|---|---|
| **E2B** | 12,913 | Python | Firecracker-class microVM sandboxes; egress allow/deny + per-domain header injection; pause/resume/snapshot. |
| **firecracker** (AWS) | very high | Rust | The KVM microVM VMM. Reference floor for any future h5i `microvm` tier (jailer recipe, snapshot uniqueness). |
| **boxlite** | 2,142 | Rust | Embeddable micro-VM (libkrun, KVM/HVF); daemonless; MITM CA placeholder-secret injection; multi-lang SDKs. |
| **SmolVM** | 4,316 | Rust | Disposable microVM (Firecracker/QEMU); Windows/browser guests; coding-agent presets; the best `microvm` shell-out candidate. |
| **tensorlake** | 958 | Python | Hosted sandbox cloud; the richest git-native idea-mine (chunked push, server-side 3-way merge, leased-ref overlay workspaces). |
| **zeroboot** | 2,397 | Rust | Sub-ms CoW KVM VM forking from a warm template; stateless, no net; the density/speed reference. Stale since Mar 2026. |
| **CubeSandbox** | (above) | | (also a microVM substrate) |

### Workspace / filesystem primitives

| Project | Stars | Lang | One-line |
|---|---|---|---|
| **branchfs** | 94 | Rust | FUSE copy-on-write speculative branching; `@branch` virtual paths; rootless; a possible h5i workspace-tier substrate. |

### Frameworks / orchestration (adjacent, mostly non-competing)

| Project | Stars | Lang | One-line |
|---|---|---|---|
| **eve** (Vercel) | 3,358 | TS | Durable filesystem-first agent framework; pluggable sandbox (Vercel/Docker/microsandbox); firewall credential brokering; HITL approval policies. |
| **moltis** | 2,768 | Rust | Persistent personal-agent server; multi-backend sandbox router (incl. shipping Firecracker tier); vault-encrypted secrets; per-session egress approval. |
| **OpenRath** | 1,079 | Python | PyTorch-like multi-agent framework; session fork/detach/merge lineage; crash-safe JSONL persistence; delegates isolation to OpenSandbox. |
| **shepherd** | 1,229 | Python | Reversible execution traces (typed effect log with per-effect `reverse()`); single-reviewer settlement. Closest conceptual neighbor on provenance. |

Two projects deserve a label up front. **secure-exec** isolates at the V8
language boundary, not the OS, so it trades "run any native binary" for
portability and density; it competes on a different axis. **eve**, **moltis**,
and **OpenRath** are agent *frameworks* whose sandbox is one subsystem; they
compose with h5i more than they compete.

---

## 2. What is being sandboxed? (the load-bearing axis)

The mechanism grouping above (OS-primitive, container, microVM) is the *how*.
The more important question for positioning is the *what*: **what runs inside
the box?** The field splits into two purposes that barely compete, and h5i is
squarely on one side.

**A. Code-execution sandboxes (run the artifact).** The agent is trusted and
runs *outside* the box; the box executes the *code the agent generated* or a
tool call. The threat model is "untrusted code, trusted orchestrator." The
interaction model is a programmatic SDK called mid-turn (`sandbox.run(code)`),
usually ephemeral and high-fan-out. The virtues that matter are cold-start
latency, density, strong untrusted-code isolation, and pause/resume, which is
exactly why these tools obsess over millisecond fork times and microVM
boundaries.

> E2B, zeroboot, secure-exec, tensorlake, CubeSandbox, sandboxd (runs the
> generated app), boxlite and SmolVM when driven as an API "disposable
> computer," branchfs (a filesystem for the generated changes).

**B. Agent-in-the-box sandboxes (run the agent).** The *agent process itself*
(a coding CLI like Claude Code or Codex) runs *inside* the box, so every file
edit, shell command, and subprocess it spawns is contained by construction. The
threat model is "a fallible or prompt-injectable agent working on a real repo,"
not malicious bytes. The interaction model is: launch a session in the box,
work in a persistent worktree, review after. The virtues that matter are
auditability, a review loop, credential and worktree provisioning, and
staying-in-the-box ergonomics.

> **h5i env** (agent-in-box profile), container-use, coasts, moltis, and
> SmolVM/boxlite's coding-agent presets (`smolvm claude start`, SkillBox).

**A middle band C. Cooperative per-command confinement.** The agent runs on the
host, but each *command it issues* is wrapped and gated per call. This is
Claude Code `/sandbox`, Codex's per-exec sandbox, and fence. It is agent-in-box
in spirit (it contains the agent's actions) without moving the agent process
into the box. h5i spans B and C: `env shell` puts the whole agent in the box,
while the wrap-bash hook and tee-shim observe per-command.

**Why this axis is the real differentiator, not a taxonomy footnote:**

1. **It re-scopes the competition.** E2B, zeroboot, secure-exec, and tensorlake
   are not h5i's rivals; they are substrates an agent *inside* h5i could call as
   a tool to run generated code. h5i's true peers on purpose are container-use,
   Claude Code, Codex, coasts, moltis, and Sculptor. The comparison matrix in
   the next section mixes both purposes on purpose (to show the full field), but
   the "who does h5i actually displace" answer is class B/C only.
2. **It explains why the provenance moat exists at all.** You can only capture
   prompts, a reasoning/context branch, cross-agent messages, and a
   session-level review loop *if the agent is what is in the box*. A
   code-execution sandbox has nothing to audit but the code's stdout. h5i's
   entire differentiator set is inseparable from the agent-in-box purpose, which
   is why no class-A tool competes on it.
3. **It reframes h5i's "gaps."** The no-microVM and no-snapshot/fast-fork gaps
   in §6 are largely *class-A virtues*: untrusted-code isolation and ms-fork
   density serve "run this generated snippet safely," not "contain the coding
   agent auditably." They are mostly cross-purpose, not h5i falling behind on
   its own axis. The honest exception is the **arena** (N candidate envs), where
   cheap fork genuinely would help, so that one class-A virtue does bleed into
   class B and is worth borrowing there specifically.
4. **The two purposes compose.** The clean end state is an agent running
   in-box under h5i (class B) that reaches for a class-A code-execution sandbox
   (E2B, a future h5i `microvm` tier) when it needs to run *untrusted generated
   code* at arm's length. h5i governs and audits; the class-A box provides the
   hardware boundary for the risky snippet. Positioning them as rivals is a
   category error; the microVM borrow in §10 is really "give class B an on-demand
   class-A escape hatch."

---

## 3. TL;DR positioning

The landscape has three concerns, and **`h5i env` is still the only entry that
spans all three at once**: a *git-worktree agent environment with a review
loop*, *real rootless kernel confinement across graduated tiers*, and
*structured, shareable, git-native provenance*.

What changed since the last comparison is that the *confinement* and *egress*
frontiers moved fast, and both first-party harnesses moved with them:

- **Codex is no longer "Seatbelt or Landlock, network on/off."** It ships an L7
  MITM CONNECT proxy with DNS-rebind defense, a Starlark **execpolicy** engine
  (allow/prompt/forbidden command rules), a native Windows restricted-token +
  WFP tier, and an LLM **guardian** reviewer. On Linux it migrated to
  bubblewrap (0.115) with Landlock+seccomp as legacy fallback.
- **Claude Code / srt** added session-persistent domain approvals, credential
  masking, and an experimental TLS-terminating proxy, on top of a proxy that
  now canonicalizes hosts (defeats `inet_aton` shorthand and null-byte
  truncation) before allowlist matching.
- **The market converged on microVMs and on snapshot/branch-a-running-VM.**
  E2B, CubeSandbox, SmolVM, boxlite, tensorlake, zeroboot, GKE Agent Sandbox,
  Morph, Modal, Cloudflare all ship a hardware or userspace-kernel boundary and
  most ship pause/resume or fork. h5i's strongest tier is still rootless
  supervised/container: no VM.
- **Firewall-level credential injection converged independently** across E2B,
  eve, boxlite, CubeSandbox, and Codex/srt: inject the secret as a header for
  exactly one allowlisted host so it never enters the process. This is the same
  target h5i's secrets broker aims at, and it argues the broker and the L7
  egress allowlist should be **one mechanism**.

But on the thing h5i is actually about, the field is thin. Provenance and audit
is the **most fragmented, least-served** area in the whole landscape. Only
**shepherd** (reversible traces) and **tensorlake** (git-native artifact store +
server-side merge) are real neighbors, and neither is local-first, rootless,
and policy-tiered the way h5i is. That is the whitespace to own.

**Isolation strength, weakest to strongest:**

```
branchfs / secure-exec-VFS (fs only)  →  h5i workspace
  →  fence ≈ srt ≈ h5i process/supervised  (OS primitives, rootless)
  →  container-use ≈ sandboxd ≈ h5i container  (rootless/hardened OCI)
  →  OpenSandbox (container … optional gVisor/Kata)
  →  E2B / firecracker / boxlite / SmolVM / CubeSandbox / zeroboot / tensorlake  (micro/userspace-kernel VM)
```

h5i sits in the rootless-OS-primitives-to-hardened-container band and does not
reach the VM band by design. Its edge is orthogonal to that band.

---

## 4. Comparison matrix

Columns chosen where projects genuinely differ. "n/a" means the concept does
not apply to that project's shape.

| Dimension | **h5i env** | Codex | Claude Code / srt | container-use | OpenSandbox | CubeSandbox | E2B | SmolVM / boxlite | tensorlake | shepherd |
|---|---|---|---|---|---|---|---|---|---|---|
| **Isolation** | tiered rootless: worktree → process (Landlock+seccomp+netns+cgroup) → supervised (+seccomp-notify gate) → container (podman). No VM. | per-command Seatbelt / bwrap+seccomp / Win restricted-token | per-command Seatbelt / bwrap+socat / Win WFP | Docker via Dagger | container; optional gVisor/Kata/FC/CLH | KVM microVM (RustVMM) | Firecracker microVM (hosted) | KVM/HVF microVM | hosted Firecracker+Docker | Seatbelt / Landlock (rootless) |
| **Rootless** | ✅ all tiers | ✅ | ✅ | ⚠️ root-in-container | ⚠️ runtime-dependent | ❌ host service | host ✅ (managed) | needs /dev/kvm | n/a (hosted) | ✅ |
| **FS / branch model** | **native git worktree per env** + mediated commit | user checkout, `.git` ro-pinned | user checkout, `.git` mandatory-deny | **git branch + container** | container fs + snapshot-to-OCI | reflink CoW rootfs snapshots | volumes + snapshot + in-VM git | isolated ext4 + diff snapshot | **git-native chunked store + 3-way merge + leased-ref overlay** | git-worktree retained changeset |
| **Egress control** | supervised = **L3/L4 nftables allowlist** (un-bypassable rootless); container = **L7 DNS-pinned CONNECT allowlist** | **L7 MITM** + DNS-rebind defense + method-limit | **L7 proxy** (host-canonicalized; MITM experimental) | ❌ none | **L3/L4 + L7 MITM**, runtime `/policy` + deny webhook | **eBPF L3/L4 + Lua L7 MITM** + inject | allow/deny + per-domain header **transform** | resolve-once nftables (SmolVM) / MITM CA (boxlite) | L3/L4 IP-CIDR (DOCKER-USER) | broker built, **not wired** |
| **Provenance** | **structured captures + event log + policy digest + egress verdicts, in shareable git refs** | session JSONL rollouts (local) | in-memory violation ring | git notes (free-form) | state + OTel + deny webhook | **per-request redacted JSONL audit** | metrics + info | metrics / benchmarks only | **operations audit log + structured sandbox logs** | **typed reversible effect DAG** |
| **Review loop** | **propose / apply / compare (arena) / rebase; mediated commit; cross-clone** | ❌ (guardian = auto-approve) | ❌ | merge / apply | ❌ | ❌ | ❌ | ❌ | server-side merge + preflight | retained → select / discard |
| **Secrets** | **broker: scoped + redact-from-evidence + fingerprint** | regex redaction | `credentials` deny + mask | 4-scheme refs, log-stripped | env / token | **inject-at-proxy** (vault-side) | **transform** inject | env / MITM placeholder | project secret store | via type signature |
| **Snapshot / pause-resume** | ❌ (persistent worktree) | ❌ (Codex Cloud only) | ❌ (managed cloud only) | ❌ | pause/resume + snapshot | snapshot/clone/rollback | **pause/resume/snapshot** | full/diff snapshot | live migrate/clone/suspend | ❌ |
| **Cross-agent messaging** | **i5h over `refs/h5i/msg`** | inter-agent comm (local) | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | structural (parent facts) |
| **Cross-platform** | Linux (confined tiers) | **mac/Linux/Win** | **mac/Linux/Win(alpha)** | mac/Linux | Linux | Linux | Linux guest | Linux+KVM / mac | hosted | mac/Linux |

Reading the matrix: h5i owns four columns outright (git-worktree branch model,
git-refs provenance, the full review loop, cross-agent messaging) and is
competitive-to-behind on the rest. The two first-party harnesses now match or
beat h5i on **egress L7 depth** and **cross-platform reach**, and the microVM
crowd beats it on **isolation ceiling** and **snapshot/pause-resume**.

---

## 5. Where h5i env is genuinely distinctive

1. **Provenance depth plus shareability, and it is the market whitespace.**
   Every run is a structured, token-reduced capture with a raw blob, the
   enforced policy digest, redaction record, and egress verdicts, appended to
   git refs that `push`/`pull` for a cross-clone review loop. Across all 21
   projects only tensorlake (hosted, proprietary core) and shepherd (local, no
   wired enforcement) come close, and the wider market research confirms
   provenance/audit is the least-standardized, least-served area. Git AI and
   agentdiff validate the git-notes attribution niche; none of them adds
   confinement or a review loop.
2. **The graduated rootless tier ladder with fail-closed refusal.** Nobody else
   offers workspace → process → supervised → container as *one* tool that
   **refuses rather than silently downgrades**. Codex silently retries outside
   the sandbox on failure; srt has an agent that can autonomously set
   `dangerouslyDisableSandbox: true`; SmolVM soft-degrades to subprocess
   networking. h5i's `verify_exec` functional self-test and per-env `doctor`
   are the opposite posture, and that posture is exactly what the "binary trap"
   pain points ask for.
3. **A real review loop.** propose / apply plus compare (the arena), rebase,
   mediated-commit path-escape defense, and policy-digest pinning. Codex's
   guardian is auto-approval, not human-mediated diff review; container-use
   stops at merge/apply. h5i is the only entry that stages a *reviewable diff*
   across N candidate environments and ranks them.
4. **Cross-agent messaging over git refs (i5h).** No competitor ships a
   shareable, refs-native message bus for the "claude proposes, codex reviews
   and applies on another clone" loop. shepherd coordinates structurally;
   everyone else has nothing.
5. **Un-bypassable rootless L3/L4 egress at the supervised tier.** The
   `net.egress` allowlist (slirp4netns + nftables default-drop + `/etc/hosts`
   DNS pinning) holds even against code running as root in its own userns,
   because the seccomp socket gate denies `AF_NETLINK`. Stronger than the L7
   proxy tier that a raw socket bypasses, and all rootless with no VM.
6. **A secrets broker that redacts the value from the captured evidence** and
   audits by fingerprint, beyond the env-var secrets of most peers.

---

## 6. Where h5i env is behind (honest gaps)

Read gaps 1 and 2 through the lens of §2: they are largely *class-A*
(code-execution) virtues, not places h5i trails on its own agent-in-box axis.
They matter to h5i mainly where the two purposes meet (the arena, and an
on-demand hardware boundary for untrusted generated code), which is how the
borrow list frames them.

1. **No microVM tier.** For genuinely hostile code, E2B / firecracker / boxlite
   / SmolVM / CubeSandbox / zeroboot / Kata-via-OpenSandbox give a hardware or
   userspace-kernel boundary h5i cannot match; its strongest tier is rootless
   supervised/container. The enum slot is reserved and fails closed. This is the
   single highest-leverage next move, and there are now good Rust references to
   study or shell out to (SmolVM and boxlite locally, firecracker's jailer for
   the recipe).
2. **No snapshot / pause-resume / fast-fork.** E2B (pause/resume), OpenSandbox
   (rootfs-snapshot hibernate), CubeSandbox (snapshot/clone/rollback), zeroboot
   (~0.8 ms fork), tensorlake (live migrate). "Branch a running VM" is the 2026
   feature race and h5i is absent from it. Its persistent-worktree model is
   better for iterative review, worse for ephemeral scale and retry loops.
3. **Egress L7 depth trails the frontier.** The supervised L3/L4 allowlist is
   real and un-bypassable, but the container-tier L7 proxy is host-allowlist
   only. Codex, CubeSandbox, and srt now do **TLS-terminating MITM** with
   per-request method/path/host inspection (block "allowed github.com but pulls
   only, not pushes") and, in OpenSandbox, **runtime policy patching** plus a
   **denied-host webhook**. h5i's allowlist is fixed at run start.
4. **Confined tiers are Linux-only.** Codex and srt both ship macOS (Seatbelt)
   and emerging Windows (restricted-token + WFP) confinement; h5i's workspace
   tier is cross-platform but unconfined. This matters because the users are on
   all three platforms.
5. **No firewall credential injection.** The convergent pattern (inject the
   secret at the proxy, keep it out of the process) is shipping in five peers
   and h5i's broker injects into the box instead.

---

## 7. Security posture: an honest read (untrusted-code threat model)

*The single question that decides everything: does the sandbox share the host
kernel?* Every reference tool sorts on this.

- **Separate-kernel / userspace-kernel:** firecracker, E2B, boxlite, SmolVM,
  CubeSandbox, zeroboot, tensorlake, OpenSandbox+Kata/gVisor. A guest kernel or
  a syscall-interception layer means a host-kernel LPE is not reachable from
  inside. A *different category* of containment.
- **Shared-kernel:** Codex, Claude Code / srt, container-use, sandboxd, fence,
  and **every runnable `h5i env` tier**. Confinement is Landlock + seccomp +
  namespaces (+ rootless podman). A host-kernel exploit reachable through the
  permitted syscalls is a full escape. This is the ceiling of the class, not an
  h5i defect.

**Where h5i lands:**

- **Against the separate-kernel class: h5i does not reach it, by design.** For
  genuinely hostile code that may carry a kernel exploit, h5i's runnable tiers
  are categorically weaker than a microVM.
- **Against the shared-kernel peers: roughly on par, and ahead on discipline.**
  vs **srt** (the closest analog): srt leans on seccomp *allow*-listing and
  deny-by-default proxy networking at its base tier, while h5i's default
  `process` tier uses a seccomp *deny*-list and all-or-nothing
  `net.mode = deny|host`. h5i's `supervised` tier closes most of that gap
  (seccomp-notify socket gate + nftables L3/L4 allowlist) but refuses on hosts
  without cgroup delegation + rootless nftables, so a typical host runs the
  slightly-weaker `process` tier. vs **Codex**: comparable enforcement, but
  Codex's default fail-open on non-managed `danger-full-access` and its
  heuristic sandbox-denial detection are weaker than h5i's fail-closed refusal.
  vs **container-use / vanilla Docker**: h5i's rootless `--cap-drop=ALL`,
  read-only-rootfs, `no-new-privs`, userns container is a more hardened OCI
  config; comparable-to-ahead.

**Residual `process`-tier items** (all inherent to shared-kernel sandboxing,
all documented): seccomp is a **deny-list not an allow-list**;
`clone(CLONE_NEWUSER)` is **not argument-filtered**; and on hosts without
cgroup delegation the memory cap falls back to `RLIMIT_DATA`. The deny-list to
allow-list move is the `process`-tier hardening with the most leverage, and
Codex's Chromium-derived Seatbelt profiles plus fence's 27-syscall list are
concrete references for it.

**Honest one-liner.** For untrusted code, `h5i env` today is *a solid
shared-kernel sandbox, peer to Codex and srt and ahead of them on fail-closed
discipline and provenance, but not Firecracker/gVisor-level isolation.*
Reaching the top class means implementing the reserved `microvm` /
`hardened-container` tier (shell out to SmolVM/boxlite or Firecracker, exactly
as OpenSandbox shells out to Kata), not further patching `process`.

---

## 8. What the whole field converged on (read these as signals)

Five patterns showed up independently across many projects. Convergence is
evidence they are right.

1. **Firewall-level credential injection.** E2B `transform` rules, eve
   `transform` policy, boxlite MITM-CA placeholders, CubeSandbox inject rules,
   Codex/srt credential masking: all inject the secret as a header for one
   allowlisted host so it never enters the process. **Implication:** h5i's
   secrets broker and L7 egress allowlist should become one mechanism. A grant
   becomes a per-host injection rule, fingerprint-audited, and the value never
   reaches the box, which delivers "redacted from evidence" for free at the
   wire.
2. **The microVM is the default isolation ceiling.** Two local Rust references
   (SmolVM, boxlite) and one recipe (firecracker jailer). **Implication:** fill
   the reserved slot by shelling out, not by hardening `process` further.
3. **Snapshot / branch-a-running-VM is the 2026 race.** **Implication:** even
   without a VM, h5i can offer a cheaper analog: fork-from-warm-worktree, or a
   blake3-chunked CoW capture store (secure-exec, tensorlake) that makes the
   arena's N-from-one-base cheap.
4. **Host-canonicalization is mandatory for any allowlist.** The SOCKS5
   null-byte bypass (`attacker.com\x00.google.com`) defeated Claude Code's
   wildcard allowlist across ~130 versions. srt's `canonicalizeHost` /
   `isValidHost` (reject zone-IDs, null bytes, CRLF; normalize `inet_aton` and
   hex/octal octets before matching) and Codex's post-CONNECT non-public-IP
   re-check are the fix. **Implication:** h5i's DNS-pinned proxy must
   canonicalize before matching or it leaks the same way.
5. **Mandatory-deny paths independent of the write policy.** srt, fence, and
   Codex all hard-block `.git/hooks`, `.git/config`, `.gitconfig`, shell rc
   files, and agent config dirs *even inside a writable root*, because
   config-poisoning (writing `.claude/settings.json` to disable hooks) is the
   shared cross-vendor escape. **Implication:** pair h5i's mediated-commit
   path-escape defense with an always-on write-block on these persistence
   vectors inside the worktree.

---

## 9. End-user pain points (what "truly useful" has to answer)

From issue trackers, HN, and vendor postmortems. These decide adoption, and
several map directly onto things h5i already does.

1. **The binary trap is the #1 articulated gap.** "Approve everything (breaks
   flow) or `--dangerously-skip-permissions` (no guardrails), no middle ground."
   Users approve ~93% of prompts (Anthropic's own figure), so prompts rarely
   change the outcome, they just interrupt. **h5i's answer:** declarative
   per-project policy tiers that enforce without per-action prompts, with
   fail-closed `verify_exec` so the middle ground is real, not aspirational.
2. **YOLO culture has caused real damage.** The `rm -rf ~/` home-wipe reports,
   Codex `rm -rf *` under full-auto. Advice consensus is "sandbox it, then
   YOLO," which spawned the whole wrapper ecosystem. **h5i's answer:** the box
   *is* the YOLO-safe surface, and it records what happened.
3. **Silent failures drive abandonment.** git exit 128, Node `fetch()`
   bypassing the DNS shim while curl works, EPERM with no prompt-to-allowlist.
   Many such issues closed "not planned," which trains "just turn it off." The
   escape hatch *is* the abandonment path. **h5i's answer:** name the exact
   recovery command in every fail-closed refusal (borrow SmolVM's error
   doctrine), and give JSON callers the same text so agents self-recover.
4. **Worktrees won as the substrate, but "git isolation without env
   provisioning is half a feature."** This is the strongest single signal.
   Claude Code shipped `--worktree`, Codex added worktrees, and the reactions
   were: `node_modules`/`.venv`/`target` don't carry over (30 to 90s and
   hundreds of MB per session), no cross-worktree isolation, port collisions.
   An entire tool niche (Conductor, Crystal, claude-squad, vibe-kanban,
   Sculptor, container-use) exists to paper over this. **h5i's answer, already
   built:** worktree-per-env + per-env HOME credential isolation +
   `private_paths` per-inode isolation over `target/`/`.next`/caches + declared
   services + injected ports. h5i already fixes the exact gaps the incumbents
   left open. This should be led with.
5. **Config-poisoning is the shared cross-vendor escape.** Writable
   `.claude/settings.json` / `.codex/config.toml` inside the sandbox lets an
   agent inject commands that run on next restart. **h5i's answer, already
   built:** sealed manifest + interactive-session config lockdown (ro binds on
   the project config dirs and user settings files). This directly answers a
   documented vuln class.
6. **Provenance demand is real but structured-metadata-shaped, not
   credit-shaped.** "Remove Co-Authored-By: Claude" guides outnumber defenses,
   but Fedora (`Assisted-by:`), QEMU (`AI-used-for:`), maintainers, enterprises,
   and incident responders all want *session-level auditable records*. The
   Replit prod-DB-deletion postmortem hinged on the action log. **h5i's
   answer:** its captures are exactly that structured, reviewable session record
   (not a co-author vanity tag), and they are the differentiator to lead with
   for the maintainer / compliance / incident-response constituencies.

---

## 10. Ideas worth borrowing (prioritized, with shipped status)

Everything from the old `borrowing-from-coasts.md` fleet/services/ports/secrets
track has **shipped v1**: `env list --json`, `env doctor`, `env secrets` (broker
with gated `command:` extractor), `env service` (daemon-free, logs-as-captures),
`env ports`, and `private_paths` per-inode isolation. So the coasts ergonomics
layer is done; the items below are the *new* frontier from this wider survey.

### Tier 1: highest leverage, on-thesis

- **Unify the secrets broker with the L7 egress allowlist (firewall credential
  injection).** The convergent pattern across five peers plus Codex. A `[secrets.*]`
  grant becomes a per-host header-injection rule on the container tier's CONNECT
  proxy; the value never enters the box; the rule is fingerprint-audited in the
  capture. Study E2B `transform` and CubeSandbox `Inject(header, format, secret)`.
- **Fill the reserved `microvm` tier by shelling out to SmolVM or boxlite.**
  Both are local Rust runtimes with clean lifecycle CLIs. Needs: `/dev/kvm`
  probing folded into the fail-closed prober; worktree mapped via a mediated
  file-sync (not raw 9p) to preserve path-escape defense; h5i's DNS-pinned proxy
  replacing their weaker egress; and h5i's observation shim wrapping exec so
  provenance survives (both emit little). Use the **firecracker jailer recipe**
  (fd-scrub, env-wipe, exec-file-copy, pivot_root, jail-local device nodes,
  last-moment privilege drop) and respect the **snapshot-uniqueness reseed**
  discipline (fresh entropy + fresh secret fingerprints per clone).
- **Host-canonicalization + mandatory-deny paths.** Borrow srt's
  `canonicalizeHost`/`isValidHost` before allowlist matching, and the
  srt/fence/Codex mandatory-deny list (`.git/hooks`, `.git/config`, `.gitconfig`,
  `.claude/`, `.mcp.json`, shell rc) enforced even inside writable roots. Also
  bake cloud-metadata IPs (`169.254.169.254`, `metadata.google.internal`) into
  the egress deny defaults (fence `code.json`). Small, pure hardening.
- **Per-request egress audit schema + denied-host events.** Adopt CubeSandbox's
  per-request redacted JSONL shape (`security_event` on deny, `tls_handshake` on
  pre-decrypt failure) and moltis's tuple (domain, port, protocol, action,
  bytes_sent/received, duration_ms, approval_source) into the event log, plus
  OpenSandbox's denied-host webhook for out-of-band alerting. Answers "why was
  this connection blocked" in provenance.

### Tier 2: strong fit, more work

- **L7 MITM inspection tier (opt-in).** Codex, CubeSandbox, and srt all do
  TLS-terminating CONNECT with per-request method/path/host rules. Model the
  container tier on Codex's seccomp `ProxyRouted` + MITM combo (study
  `network-proxy/src/{mitm,policy}.rs`): allows blocking "github.com pulls only,
  not pushes," the exfil vector everyone flags.
- **Runtime egress policy + interactive domain approval.** OpenSandbox's
  authenticated `PATCH /policy` (with an operator floor tenant policy can't
  override) and moltis's `NeedsApproval` hold-connection-then-remember flow. Let
  an agent request egress additions mid-run, recording each approval and its
  source. Removes the "fixed at run start" limitation.
- **Content-addressed chunked capture store (blake3 CoW).** secure-exec's VFS
  and tensorlake's chunked push: dedupe near-identical run captures, get cheap
  fork/snapshot of env state (the substrate for arena N-from-one-base), and
  crash-safety via blocks-before-metadata ordering. tensorlake's **resumable
  detached commit jobs** make large raw-blob evidence interruption-safe.
- **Starlark execpolicy for the command layer.** Codex's `execpolicy` crate:
  `prefix_rule(pattern, decision=allow|prompt|forbidden, ...)`, strictest-wins,
  `host_executable(name, paths)` anti-spoofing pins, load-time validation, CLI
  check. A mature, testable classification layer that maps onto h5i's manifest
  and feeds the derived-classification idea below. Pairs with fence's
  **argv-aware seccomp-notify exec gate** (deny `git push` without denying all
  `git`), which extends h5i's existing supervised seccomp-notify to
  `execve`/`execveat`.

### Tier 3: from the companion docs (still open)

- **Decision BOM** (`h5i recall bom <sha>`): a reconstructible provenance card
  synthesizing the four dimensions with an honest completeness level. Highest
  value / lowest risk; pure read model. See `borrowing-from-governance-planes.md`.
- **Ed25519 agent identity + hash-chained append logs**: make the "untrusted
  messages" posture enforceable and every recording forgery-evident. Same doc.
- **Derived (not self-reported) action classification** feeding a
  `policy simulate` that replays captures against a candidate profile. Same doc.
- **Typed reversible effect log** (shepherd): per-effect `reverse()` for
  partial/selective undo and replay, which h5i structurally cannot do today.
  Plus `env diff --read <file>` (smoke-test one candidate before applying), a
  run-journal WAL + `env repair`, and a deterministic offline provider for
  keyless CI. See `borrowing-from-shepherd.md`.

### Smaller steals (cheap, concrete)

- **Fail-closed boot ordering** (sandboxd): repopulate egress rules from
  persisted state *before* opening the listener; abort the env if any nft call
  fails rather than run with no rules.
- **VM/env-to-env default-deny** (SmolVM nftables TAP-to-TAP): the arena's N
  parallel envs should default-deny cross-env traffic.
- **`limit_warning` at ~80% with re-arm hysteresis** (secure-exec): emit an
  "approaching cap" event before the terminal kill, with stable limit names.
- **`config import` learning loop** (container-use): let an env's setup/manifest
  deltas be reviewed and folded back into the repo default.
- **`--linux-features` / machine-readable capability JSON** (fence, SmolVM
  `smolvm-core`): structured per-host capability report explaining *why* a tier
  was refused. Extends `env doctor`.
- **Guardian-style auto-triage** (Codex): a fail-closed LLM reviewer for
  low-risk proposals in the propose/apply loop, with the decision **bound into
  the capture** (which Codex does not do).
- **Positive isolation self-test per tier** (zeroboot Phase 5): plant a secret
  in env A, assert env B cannot read it, both directions. Add to the prober
  alongside capability presence.

---

## 11. Bottom line and differentiation strategy

`h5i env` is best-in-class on **git-native provenance, the review loop,
cross-agent messaging, and fail-closed rigor**; competitive on **rootless
kernel confinement** (peer to Codex and srt, ahead of container-use); and at
parity-to-behind on **egress L7 depth**, with honest gaps at the **isolation
ceiling** (no VM tier), **ephemeral-scale lifecycle** (no snapshot/pause-resume),
and **cross-platform confinement** (Linux only).

The market taught four things. First (§2), h5i is an **agent-in-the-box** tool,
so most of the microVM/snapshot field is a *different purpose* (run generated
code, not the agent) and composes with h5i rather than competing; positioning
against E2B or zeroboot on isolation strength is a category error. Second,
isolation *within h5i's own class* is a crowded, well-served frontier where two
first-party harnesses already compete, so h5i should *reach parity by shelling
out* (microVM escape hatch, MITM egress, host-canonicalization) rather than try
to out-isolate them. Third, **provenance and the review loop are the real
whitespace**, thinly served by shepherd and tensorlake alone, and they only
exist *because* h5i runs the agent, not the artifact. Fourth, the loudest
end-user pain, worktrees that don't actually work, is a gap h5i has **already
closed** (per-env HOME isolation, `private_paths`, services, ports, config
lockdown) and under-markets.

So the strategy is:

1. **Lead with the third option to the binary trap and with worktrees that
   actually work.** These are shipped, they answer the two most-cited pains, and
   no incumbent combines them with provenance.
2. **Own provenance as the durable moat.** Ship the Decision BOM and the
   signed/hash-chained logs so the captures become *provable*, not merely
   recorded, for the maintainer / compliance / incident-response buyers who are
   actively asking.
3. **Reach isolation parity by composition, not competition.** Fill the reserved
   microVM slot via SmolVM/boxlite, unify secrets with L7 egress injection, and
   add MITM inspection + host-canonicalization, so h5i is *also* a credible
   confinement story without pretending to be Firecracker.
4. **Stay honest.** Keep the fail-closed refusal, keep the L7-is-honest-not-
   airtight framing, and keep local-first / no-daemon / rootless as the columns
   no competitor here owns all four of at once.
