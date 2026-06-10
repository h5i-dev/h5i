# Branch: impl-env-sandbox

**Purpose:** implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

_Commits will be appended below._

## Commit 6a28cf21 — 2026-06-10 02:42 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution
New src/env.rs (triple fusion: code branch + ctx branch + manifest; refs/h5i/env CAS event log; native git worktree backend under .git/.h5i/env; mediated commit with canonicalized $WORK path allowlist; propose/apply lifecycle) and src/sandbox.rs (policy profiles from .h5i/env.toml, fail-closed capability probing, Landlock+seccomp+netns+rlimits process tier). objects::Manifest gained env_id/policy_digest/egress/redactions. CLI: h5i env create/run/probe/list/status/log/diff/propose/apply/abort/gc; h5i pull union-merges refs/h5i/env. 24 new unit tests + 18 integration tests (kernel confinement verified live on this host, Landlock ABI 3). Remaining: container/microvm adapters, seccomp-notify supervisor (domain egress), stage separation, macOS Seatbelt.

---

## Commit 6a28cf70 — 2026-06-10 02:44 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary
New src/env.rs (triple fusion: code branch + ctx branch + manifest; refs/h5i/env CAS event log; native git worktree backend under .git/.h5i/env; mediated commit with canonicalized $WORK path allowlist; propose/apply lifecycle) and src/sandbox.rs (policy profiles from .h5i/env.toml, fail-closed capability probing, Landlock+seccomp+netns+rlimits process tier). objects::Manifest gained env_id/policy_digest/egress/redactions. CLI: h5i env create/run/probe/list/status/log/diff/propose/apply/abort/gc; h5i pull union-merges refs/h5i/env. 24 new unit tests + 18 integration tests (kernel confinement verified live on this host, Landlock ABI 3). Remaining: container/microvm adapters, seccomp-notify supervisor (domain egress), stage separation, macOS Seatbelt.

### This Commit's Contribution


---

## Commit 6a28d57a — 2026-06-10 03:09 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution
Phase-1-3 maturation. Security: env-run captures redact secrets from raw blob + summary + cmd before content-addressing (objects::capture redact flag, reuses secrets.rs); expanded seccomp deny-list (name_to_handle_at, fanotify, quotactl, clock_adjtime, NUMA mempolicy, x86 port-I/O); always create userns+IPC+UTS namespaces at process tier (+netns on deny); setsid + process-group SIGKILL so wall-clock kill reaps descendants; timed-out run exits 124; signal-killed run exits 1 (was silently 0). Rigor: flock run-lock serializes concurrent runs; git worktree locked at create, gc unlocks+prunes; new env inspect verb (capture scoped to its env). Tests: +6 (2 unit redaction, 4 integration: redaction-no-leak, inspect+foreign-refuse, concurrent-run-serialized, descendant-reap, host-net-still-confines). Full suite 877 green, clippy clean.

---

## Commit 6a28d59c — 2026-06-10 03:10 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary
Phase-1-3 maturation. Security: env-run captures redact secrets from raw blob + summary + cmd before content-addressing (objects::capture redact flag, reuses secrets.rs); expanded seccomp deny-list (name_to_handle_at, fanotify, quotactl, clock_adjtime, NUMA mempolicy, x86 port-I/O); always create userns+IPC+UTS namespaces at process tier (+netns on deny); setsid + process-group SIGKILL so wall-clock kill reaps descendants; timed-out run exits 124; signal-killed run exits 1 (was silently 0). Rigor: flock run-lock serializes concurrent runs; git worktree locked at create, gc unlocks+prunes; new env inspect verb (capture scoped to its env). Tests: +6 (2 unit redaction, 4 integration: redaction-no-leak, inspect+foreign-refuse, concurrent-run-serialized, descendant-reap, host-net-still-confines). Full suite 877 green, clippy clean.

### This Commit's Contribution


---

## Commit 6a28d8b2 — 2026-06-10 03:23 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution
wait4-based reaper records rusage (wall/cpu/peak-RSS) per run, surfaced in CLI + exec event detail + run output. Security: exec event detail now redacts the command (was leaking secrets passed as args into refs/h5i/env); tools allowlist now ENFORCED at run (non-empty list refuses unlisted argv[0] basename, fail-closed). Completeness: h5i env compare <names...> [--json] ranks envs from one base by diffstat + latest-run exit/test-status (reuses objects structured results), warns when bases differ — the reviewer-comparison arena. +9 tests (4 sandbox unit: resources, tools enforce/empty; 5 integration: event redaction+resources, tools enforcement, compare rank+json, compare split-base warn). Full suite 884 green, clippy clean.

---

