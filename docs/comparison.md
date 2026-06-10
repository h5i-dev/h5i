# `h5i env` vs. the agent-sandbox landscape

A comparison of h5i's `env` feature against a set of reference projects, to
locate where h5i is distinctive, where it is behind, and which ideas are worth
borrowing. h5i `env` is the "triple fusion" of a code branch (git worktree), a
reasoning branch, and a policy-confined, fully-observed execution
(`docs/environments-design.md`); the tiers are `workspace → process → supervised
→ container` (with `hardened-container`/`microvm` reserved for external
backends).

## Reference set

| Project | One-line |
|---|---|
| **container-use** (Dagger) | MCP server giving each agent a containerized env per git branch, with human merge/apply. |
| **sandbox-runtime** (Anthropic, "srt") | Rootless OS-level sandbox (bwrap+seccomp / Seatbelt) for a single process, with proxy-based network filtering. |
| **OpenSandbox** (Alibaba) | General agent-sandbox platform; container default, optional gVisor/Kata/Firecracker; L3/L4+L7 egress. |
| **E2B** | Managed microVM sandboxes for AI-generated code; pause/resume, snapshot templates, egress allow/deny. |
| **Firecracker** (AWS) | KVM microVM monitor — the gold standard for VM-level isolation of untrusted code. |
| **zeroboot** | Rust KVM snapshot-CoW fork engine: sub-millisecond VM forks from a pre-warmed template. |
| **branchfs** | FUSE copy-on-write filesystem: zero-cost branch/commit/abort of file state for multi-agent speculation. |

(The directory also includes papers — EscapeBench, SandLock, GAAP, BranchFS — referenced where relevant.)

## TL;DR positioning

The landscape clusters into three families, and **h5i `env` is the only entry
that spans all three concerns at once**: a *git-worktree agent environment with a
review loop*, *real rootless kernel confinement*, and *structured, shareable
provenance plus a denial-observability dashboard*.

- Its closest analog on **workflow** — container-use — has much weaker isolation.
- Its closest analog on **isolation mechanism** — sandbox-runtime — has no
  environment/provenance/review model at all (it confines a single process).
- The strongest-isolation tools — Firecracker / E2B / zeroboot / Kata-via-OpenSandbox
  — are microVM *substrates* with no git-review or provenance story, and they
  need root/KVM.

**Isolation strength, weakest → strongest:**

```
branchfs (none) → h5i workspace → sandbox-runtime ≈ h5i process/supervised
  → container-use ≈ h5i container → OpenSandbox (container … optional gVisor/Kata)
  → E2B / Firecracker / zeroboot (microVM)
```

## Comparison matrix

| Dimension | **h5i env** | container-use | sandbox-runtime | OpenSandbox | E2B | Firecracker / zeroboot | branchfs |
|---|---|---|---|---|---|---|---|
| **Isolation** | tiered: worktree → **process** (Landlock+seccomp+netns+cgroup) → **supervised** (+seccomp-notify gate, netns) → container (podman). No VM. | Docker container (root-in-container) | bwrap+seccomp+nested-ns | container; **optional gVisor/Kata/FC** | microVM (managed) | KVM microVM | none (FUSE only) |
| **Rootless** | ✅ all tiers | ⚠️ root-in-container | ✅ | ⚠️ runtime-dependent | host ✅ (managed) | ❌ needs root/KVM | ✅ |
| **FS / branch model** | **native git worktree per env** + mediated commit (path-escape / nested-`.git` defense) | git branch + worktree→container | bind mounts / allowlist | container fs + rootfs snapshots | per-sandbox + snapshot templates + volumes | CoW snapshot fork | **zero-cost FUSE CoW branches** |
| **Egress control** | container = **L7 proxy allowlist** (DNS-pinned, fail-closed); supervised = airtight **net.mode=deny** today, **L3/L4 nftables allowlist pending**; + seccomp socket gate | ❌ none | **L7 proxy** allowlist (fail-closed) | **L3/L4 (DNS+nftables) + L7 MITM**, dynamic policy | CIDR/host allow+deny, per-domain HTTP transforms | host's job (TAP+nftables) / none | ❌ none |
| **Provenance** | **structured captures (token-reduced + raw) + event log + policy digests + EgressSummary + redactions, all in shareable git refs** | git notes (free-form) | in-memory violations | minimal; audit-trail **roadmap-only** | metrics only | request JSONL (zeroboot) | logs only |
| **Agent + review loop** | **worktree + context branch + policy = triple fusion; propose / apply / compare (arena) / rebase; MCP; cross-clone review via push/pull** | merge / apply; MCP | single-process; ask-callback | MCP; no git review | SDK; pause/resume; no review | none (substrate) | `@branch` multi-agent paths |
| **Secrets** | **broker: scoped + redact-from-evidence + fingerprint audit + fail-closed** | env (Dagger secrets) | none | registry / token | env / git creds | none | none |
| **Resource limits** | **cgroup v2 (memory.max / pids.max) + rlimits** | none | none | k8s extended resources | cpu / mem / disk | cgroups + rate limiters | storage quota |
| **Denial observability** | **risk classifier + "flight-recorder" dashboard, boundary-pressure scoring, violation events** | ❌ | macOS log / `strace` | denied-host webhook | error types | metrics | ENOSPC |
| **Cross-platform** | Linux; confined tiers x86-64/aarch64 | Linux/macOS (Docker) | **macOS / Linux / Windows** | Linux | Linux (guest) | Linux + KVM | Linux/macOS (FUSE) |

## Where h5i env is genuinely distinctive

1. **Provenance depth + shareability.** Every run is a structured, token-reduced
   capture with a raw blob, the enforced policy digest, redaction record, and
   egress verdicts — appended to **git refs that `push`/`pull`** for a
   cross-clone review loop. No other project here matches this; container-use's
   git-notes is the nearest and it is free-form text.
2. **Denial / escape observability is unique.** The risk classifier and the
   "flight recorder" dashboard (boundary-pressure scoring, impossible-claim
   language) have no equivalent in the set — OpenSandbox's audit trail is
   roadmap-only, srt's is in-memory, container-use has none.
3. **A richer review loop.** propose / apply **plus** compare (the "arena"),
   rebase, mediated-commit path-escape defense, and policy-digest pinning.
   container-use stops at merge/apply.
4. **Fail-closed discipline.** Refuse-don't-downgrade, "every component green or
   refuse," and dashboard copy that calls an unsatisfiable claim *impossible*
   rather than degraded — closest in spirit to Firecracker's jailer philosophy.
5. **A real secrets broker** that redacts the value *from the captured evidence*
   and audits by fingerprint — beyond the env-var secrets of
   E2B/container-use/OpenSandbox.

## Where h5i env is behind (honest gaps)

1. **No microVM tier.** For genuinely hostile code, Firecracker / E2B / zeroboot /
   Kata give a hardware boundary h5i cannot match; its strongest tier is rootless
   supervised/container. (h5i's own design reserves `hardened-container`/`microvm`
   as external backends.)
2. **Egress is behind *today*.** The container tier is L7-only (bypassable by a
   raw socket — the same honest limitation srt has). OpenSandbox already ships
   L3/L4 (DNS + nftables) and E2B ships CIDR/host allowlists. h5i's **supervised
   increment 2** (netns + nftables, already designed in
   `docs/supervisor-design.md`) is what closes this — and with the seccomp socket
   gate, per-host verdicts, and the dashboard, it would *exceed* them on
   observability.
3. **No snapshot / pause-resume / fast-fork.** E2B (pause/resume), OpenSandbox
   (rootfs-snapshot hibernate), zeroboot (~0.8 ms VM fork). h5i's
   persistent-worktree model is better for *iterative review*, worse for
   *ephemeral scale*.
4. **Confined tiers are Linux + x86-64/aarch64 only;** srt covers
   macOS/Linux/Windows (the h5i `workspace` tier is cross-platform but unconfined).

## Ideas worth borrowing (mapped to the roadmap)

- **Supervised increment 2 (egress)** — adopt **OpenSandbox's** model directly:
  DNS-proxy + nftables L3/L4, **dynamic `/policy` patching**,
  `deny.always > allow.always` precedence, and a **denied-hostname webhook** that
  feeds the dashboard's NET lane. And **srt's** proxy hardening for the *container*
  tier now: reject malformed hosts (null bytes), canonicalize `inet_aton`
  shorthand (`2852039166` → an IP) before allowlist matching.
- **Harden process/supervised** with **srt's** dual-namespace trick: a nested
  PID + user namespace applied *after* the mount setup, plus `PR_SET_DUMPABLE=0`,
  to block ptrace between the tracee and the supervisor's helpers.
- **From Firecracker:** the minimal-attack-surface principle is already the ethos;
  the transferable concrete is per-thread/role seccomp if the tracee side ever
  multi-threads.
- **A future `microvm` / `hardened-container` tier:** **zeroboot's** snapshot-CoW
  fork or **Kata via OpenSandbox's** RuntimeClass is the blueprint, and it slots
  into the existing `IsolationClaim` enum + fail-closed probe.
- **An ephemeral mode:** **E2B / OpenSandbox** pause-resume via rootfs snapshot —
  orthogonal to the worktree model, useful at scale.
- **An alternative workspace backend:** **branchfs's** zero-cost FUSE CoW
  branching (the design already says the workspace backend is pluggable) for
  lighter-than-worktree multi-agent speculation.

## Bottom line

h5i `env` is best-in-class on **provenance, the review loop, denial
observability, and fail-closed rigor**; competitive on **rootless kernel
confinement** (a peer to sandbox-runtime, ahead of container-use); and behind on
**isolation ceiling (no VM), egress (until supervised increment 2 lands), and
ephemeral-scale lifecycle**. The single highest-leverage move to close the most
visible gap is already on the roadmap — **supervised increment 2 (netns +
nftables egress)** — which would take h5i from "behind OpenSandbox/E2B on egress"
to "ahead on egress *observability*."
