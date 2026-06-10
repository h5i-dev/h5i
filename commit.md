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

## Commit 6a28d8cc — 2026-06-10 03:23 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary
wait4-based reaper records rusage (wall/cpu/peak-RSS) per run, surfaced in CLI + exec event detail + run output. Security: exec event detail now redacts the command (was leaking secrets passed as args into refs/h5i/env); tools allowlist now ENFORCED at run (non-empty list refuses unlisted argv[0] basename, fail-closed). Completeness: h5i env compare <names...> [--json] ranks envs from one base by diffstat + latest-run exit/test-status (reuses objects structured results), warns when bases differ — the reviewer-comparison arena. +9 tests (4 sandbox unit: resources, tools enforce/empty; 5 integration: event redaction+resources, tools enforcement, compare rank+json, compare split-base warn). Full suite 884 green, clippy clean.

### This Commit's Contribution


---

## Commit 6a28dcb2 — 2026-06-10 03:40 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution
Root cause: GitHub Actions runners (Ubuntu 24.04, AppArmor-restricted unprivileged userns) report Landlock+userns+seccomp present, so capability-bit gating didn't skip — but exec under the full confinement stack returns EACCES, failing 3 process-tier tests. Fix: sandbox::verify_exec(policy) runs a trivial confined  (tool-allowlist bypassed) to functionally verify the tier; env create calls it after resolve and fails closed with a clear 'not functional — re-request --isolation workspace' message BEFORE creating any on-disk state (was cryptic EACCES at every run). env probe now prints 'process tier runnable = yes|no'. Tests gate on process_tier_runnable() (cached; succeeds iff a process create succeeds) instead of raw bits; process_claim_all_or_nothing uses the same source of truth. Verified the not-runnable path by simulating restrictive Landlock grants. Full suite 884 green on dev (process tier runnable), clippy clean.

---

## Commit 6a28dcda — 2026-06-10 03:41 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary
Root cause: GitHub Actions runners (Ubuntu 24.04, AppArmor-restricted unprivileged userns) report Landlock+userns+seccomp present, so capability-bit gating didn't skip — but exec under the full confinement stack returns EACCES, failing 3 process-tier tests. Fix: sandbox::verify_exec(policy) runs a trivial confined  (tool-allowlist bypassed) to functionally verify the tier; env create calls it after resolve and fails closed with a clear 'not functional — re-request --isolation workspace' message BEFORE creating any on-disk state (was cryptic EACCES at every run). env probe now prints 'process tier runnable = yes|no'. Tests gate on process_tier_runnable() (cached; succeeds iff a process create succeeds) instead of raw bits; process_claim_all_or_nothing uses the same source of truth. Verified the not-runnable path by simulating restrictive Landlock grants. Full suite 884 green on dev (process tier runnable), clippy clean.

### This Commit's Contribution


---

## Commit 6a28e117 — 2026-06-10 03:59 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution
Security: seccomp now blocks io_uring_setup/enter/register (major kernel escape surface that also bypasses seccomp for submitted ops); policy resources gain opt-in fsize (RLIMIT_FSIZE disk-bomb cap) and cpu (RLIMIT_CPU spin backstop). Maturity: env status is now a human view (lifecycle, enforced policy incl. net/mem/procs/wall/fsize/cpu/tools, evidence, base drift) with --json for the manifest. Completeness (§9): base-drift detection (drift(): UpToDate/ParentAhead{n}/Diverged/ParentGone via graph_descendant_of + ahead_behind) surfaced in status + propose brief; new h5i env rebase folds the advanced parent in via 3-way merge onto the env work, refuses on conflict (base untouched), re-pins base_commit/base_tree, refreshes the worktree. +7 tests (3 sandbox unit: fsize/cpu parse+default-off, digest sensitivity, io_uring-in-denylist; 4 integration: drift→rebase→re-pin→apply, conflict refusal keeps base, status --json, fsize disk-bomb cap gated). Full suite 891 green, clippy clean.

---

## Commit 6a28e134 — 2026-06-10 03:59 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary
Security: seccomp now blocks io_uring_setup/enter/register (major kernel escape surface that also bypasses seccomp for submitted ops); policy resources gain opt-in fsize (RLIMIT_FSIZE disk-bomb cap) and cpu (RLIMIT_CPU spin backstop). Maturity: env status is now a human view (lifecycle, enforced policy incl. net/mem/procs/wall/fsize/cpu/tools, evidence, base drift) with --json for the manifest. Completeness (§9): base-drift detection (drift(): UpToDate/ParentAhead{n}/Diverged/ParentGone via graph_descendant_of + ahead_behind) surfaced in status + propose brief; new h5i env rebase folds the advanced parent in via 3-way merge onto the env work, refuses on conflict (base untouched), re-pins base_commit/base_tree, refreshes the worktree. +7 tests (3 sandbox unit: fsize/cpu parse+default-off, digest sensitivity, io_uring-in-denylist; 4 integration: drift→rebase→re-pin→apply, conflict refusal keeps base, status --json, fsize disk-bomb cap gated). Full suite 891 green, clippy clean.

### This Commit's Contribution


---

## Commit 6a294441 — 2026-06-10 11:02 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution
Makes the multi-agent review loop real. refs/h5i/env now holds events.jsonl + manifests.jsonl + policies.jsonl, written in one CAS commit per change (append_env_commit); union_merge_commits reconciles all three (events append-only, manifests newest-updated_at wins so apply-on-B propagates back, policies immutable). EnvManifest gains updated_at. materialize_from_ref writes pulled manifests/policies to disk; diff/compare fall back to base..branch-tip when the worktree is absent (remote env); run/propose/rebase give a clear 'lives on another clone' error. Wiring: h5i push adds refs/h5i/env + wildcard refs/heads/h5i/env/*; h5i pull now calls sync_one(ENV_REF) (was never called!) + fetches env branches + materializes; setup-remote adds both patterns. Tests: +4 (unit upsert_jsonl; integration env_ref_holds_blobs, and the full two-clones loop: A creates+runs+proposes+pushes, B pulls+lists+diffs-from-branch+inspects+applies, applied status round-trips back to A). Full suite 894 green, clippy clean.

---

## Commit 6a29445f — 2026-06-10 11:02 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary
Makes the multi-agent review loop real. refs/h5i/env now holds events.jsonl + manifests.jsonl + policies.jsonl, written in one CAS commit per change (append_env_commit); union_merge_commits reconciles all three (events append-only, manifests newest-updated_at wins so apply-on-B propagates back, policies immutable). EnvManifest gains updated_at. materialize_from_ref writes pulled manifests/policies to disk; diff/compare fall back to base..branch-tip when the worktree is absent (remote env); run/propose/rebase give a clear 'lives on another clone' error. Wiring: h5i push adds refs/h5i/env + wildcard refs/heads/h5i/env/*; h5i pull now calls sync_one(ENV_REF) (was never called!) + fetches env branches + materializes; setup-remote adds both patterns. Tests: +4 (unit upsert_jsonl; integration env_ref_holds_blobs, and the full two-clones loop: A creates+runs+proposes+pushes, B pulls+lists+diffs-from-branch+inspects+applies, applied status round-trips back to A). Full suite 894 green, clippy clean.

### This Commit's Contribution


---

## Commit 6a29781b — 2026-06-10 14:43 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution
Makes the flagship env feature agent-native (h5i's whole premise: agents call tools, not Bash). Added 11 MCP tools — h5i_env_create/run/list/status/diff/inspect/compare/propose/apply/rebase/abort — to tool_definitions() with agent-guiding descriptions, tool_env_* handlers reusing env.rs (open repo + materialize_from_ref + resolve claude, return JSON/patch/render), and dispatch arms in call_tool. status includes drift; run reports exit/resources/structured; diff works on pulled remote envs. Raised lib recursion_limit to 512 (large json! literal). Tests: +4 mcp unit (advertised, full create→run→inspect→diff→propose→apply lifecycle, compare, unknown-env error) and updated the tool-count + tools-list tests 29→40. Updated .claude/h5i.md to steer agents to h5i_env_* for risky/exploratory work. Full suite 898 green, clippy clean.

---

