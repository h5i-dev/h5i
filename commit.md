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

