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

## Commit 6a29783d — 2026-06-10 14:44 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary
Makes the flagship env feature agent-native (h5i's whole premise: agents call tools, not Bash). Added 11 MCP tools — h5i_env_create/run/list/status/diff/inspect/compare/propose/apply/rebase/abort — to tool_definitions() with agent-guiding descriptions, tool_env_* handlers reusing env.rs (open repo + materialize_from_ref + resolve claude, return JSON/patch/render), and dispatch arms in call_tool. status includes drift; run reports exit/resources/structured; diff works on pulled remote envs. Raised lib recursion_limit to 512 (large json! literal). Tests: +4 mcp unit (advertised, full create→run→inspect→diff→propose→apply lifecycle, compare, unknown-env error) and updated the tool-count + tools-list tests 29→40. Updated .claude/h5i.md to steer agents to h5i_env_* for risky/exploratory work. Full suite 898 green, clippy clean.

### This Commit's Contribution


---

## Commit 6a298068 — 2026-06-10 15:19 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution
Design phase 4. New src/container.rs: probe() (rootless podman, else docker); build_run_argv() = hardened podman run (--rm, --cap-drop=ALL, no-new-privileges, --read-only + tmpfs /tmp, -v $WORK:/work, --userns=keep-id, --memory/--pids-limit, env allowlist, no docker.sock, --name for timeout cleanup); net.mode deny→--network=none, host→default, net.egress→DNS-pinned host-side HTTP/HTTPS CONNECT allowlist proxy (AllowList exact/.wildcard/:port, fail-closed 403), container reaches it via slirp4netns 10.0.2.2 + HTTP(S)_PROXY. Honest L7 scoping documented. sandbox wiring: HostCaps.container_runtime, resolve() allows container (needs runtime+image, fail closed) and net.egress under container (was process-only refuse), Profile.image + ContainerToml, run() Container arm. env probe shows container runtime + claim. Verified REAL on podman 4.9.3: workspace mount + uid keep-id + net deny block + egress allowlist (example.com reachable, google blocked). Tests: 9 unit (allowlist decision, live proxy 403/gate, argv hardening, policy) + 5 integration (3 podman-gated real runs, fail-closed image/egress). Full suite 913 green, clippy clean.

---

## Commit 6a298091 — 2026-06-10 15:19 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary
Design phase 4. New src/container.rs: probe() (rootless podman, else docker); build_run_argv() = hardened podman run (--rm, --cap-drop=ALL, no-new-privileges, --read-only + tmpfs /tmp, -v $WORK:/work, --userns=keep-id, --memory/--pids-limit, env allowlist, no docker.sock, --name for timeout cleanup); net.mode deny→--network=none, host→default, net.egress→DNS-pinned host-side HTTP/HTTPS CONNECT allowlist proxy (AllowList exact/.wildcard/:port, fail-closed 403), container reaches it via slirp4netns 10.0.2.2 + HTTP(S)_PROXY. Honest L7 scoping documented. sandbox wiring: HostCaps.container_runtime, resolve() allows container (needs runtime+image, fail closed) and net.egress under container (was process-only refuse), Profile.image + ContainerToml, run() Container arm. env probe shows container runtime + claim. Verified REAL on podman 4.9.3: workspace mount + uid keep-id + net deny block + egress allowlist (example.com reachable, google blocked). Tests: 9 unit (allowlist decision, live proxy 403/gate, argv hardening, policy) + 5 integration (3 podman-gated real runs, fail-closed image/egress). Full suite 913 green, clippy clean.

### This Commit's Contribution


---

## Commit 6a298203 — 2026-06-10 15:25 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29878b — 2026-06-10 15:49 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29883e — 2026-06-10 15:52 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a298919 — 2026-06-10 15:56 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2989a7 — 2026-06-10 15:58 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2989e7 — 2026-06-10 15:59 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a298a17 — 2026-06-10 16:00 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution
Inventoried telemetry (env events.jsonl, capture manifests w/ env_id+policy_digest, rusage; denials currently silent: no proxy verdict log, no syscall names). Consulted codex (msg #9b60c3ce): converged on Sandbox workbench mode = fleet table + per-env five-lane timeline (FS/NET/PROC/RESOURCE/PROVENANCE) + explainable Boundary Pressure score, read-only v1, deterministic classifiers. Pre-req telemetry: wire CONNECT proxy verdicts into existing EgressSummary capture field, emit mediated-commit violation events, deterministic command/output scanner. Design delivered to user; no code written yet.

---

## Commit 6a298a4e — 2026-06-10 16:01 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary
Inventoried telemetry (env events.jsonl, capture manifests w/ env_id+policy_digest, rusage; denials currently silent: no proxy verdict log, no syscall names). Consulted codex (msg #9b60c3ce): converged on Sandbox workbench mode = fleet table + per-env five-lane timeline (FS/NET/PROC/RESOURCE/PROVENANCE) + explainable Boundary Pressure score, read-only v1, deterministic classifiers. Pre-req telemetry: wire CONNECT proxy verdicts into existing EgressSummary capture field, emit mediated-commit violation events, deterministic command/output scanner. Design delivered to user; no code written yet.

### This Commit's Contribution


---

## Commit 6a299eac — 2026-06-10 17:28 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution
Phase0a: container.rs proxy verdicts -> EgressSummary{allowed,denied,hosts[],hosts_truncated} on capture manifest. Phase0b: mediated_commit emits durable 'violation' EnvEvent on nested-.git/symlink/.. boundary trips. Phase0c: src/risk.rs deterministic classifier (11 unit tests). Phase1: server.rs read-only /api/envs|/api/env/*|probe (build_router extracted; tests/sandbox_api.rs 3 integration tests boot real router). Phase2: web/src/SandboxView.tsx Workbench 'sandbox' mode - top-strip vitals, fleet table w/ pressure badges+filters, five-lane timeline, enforced-policy panel; theme.css sbx-* styles. All 725 lib + integration tests pass, clippy clean. Live-validated 3 regimes: workspace->grey weak, process->amber pressure, violation->red blocked.

---

## Commit 6a299ecc — 2026-06-10 17:28 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary
Phase0a: container.rs proxy verdicts -> EgressSummary{allowed,denied,hosts[],hosts_truncated} on capture manifest. Phase0b: mediated_commit emits durable 'violation' EnvEvent on nested-.git/symlink/.. boundary trips. Phase0c: src/risk.rs deterministic classifier (11 unit tests). Phase1: server.rs read-only /api/envs|/api/env/*|probe (build_router extracted; tests/sandbox_api.rs 3 integration tests boot real router). Phase2: web/src/SandboxView.tsx Workbench 'sandbox' mode - top-strip vitals, fleet table w/ pressure badges+filters, five-lane timeline, enforced-policy panel; theme.css sbx-* styles. All 725 lib + integration tests pass, clippy clean. Live-validated 3 regimes: workspace->grey weak, process->amber pressure, violation->red blocked.

### This Commit's Contribution


---

## Commit 6a299f39 — 2026-06-10 17:30 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a299f8d — 2026-06-10 17:31 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29a099 — 2026-06-10 17:36 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29a160 — 2026-06-10 17:39 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29a272 — 2026-06-10 17:44 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29a9e1 — 2026-06-10 18:16 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29ace8 — 2026-06-10 18:28 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29b056 — 2026-06-10 18:43 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29b16b — 2026-06-10 18:48 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29b275 — 2026-06-10 18:52 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29b411 — 2026-06-10 18:59 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29b4ff — 2026-06-10 19:03 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29b613 — 2026-06-10 19:08 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29b68e — 2026-06-10 19:10 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29b72a — 2026-06-10 19:12 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29b7a2 — 2026-06-10 19:14 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29b848 — 2026-06-10 19:17 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29b8df — 2026-06-10 19:19 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29b9a8 — 2026-06-10 19:23 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29ba12 — 2026-06-10 19:25 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29bac2 — 2026-06-10 19:28 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29baee — 2026-06-10 19:28 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29bcbd — 2026-06-10 19:36 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29bd8f — 2026-06-10 19:39 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29bdec — 2026-06-10 19:41 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29bed3 — 2026-06-10 19:45 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29bf97 — 2026-06-10 19:48 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29c289 — 2026-06-10 20:01 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29c398 — 2026-06-10 20:05 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29c7f4 — 2026-06-10 20:24 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29c860 — 2026-06-10 20:26 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29c91f — 2026-06-10 20:29 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29cc92 — 2026-06-10 20:44 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29cde3 — 2026-06-10 20:49 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29cea0 — 2026-06-10 20:52 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29cee4 — 2026-06-10 20:53 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29cfc1 — 2026-06-10 20:57 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29d0a9 — 2026-06-10 21:01 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29d1d8 — 2026-06-10 21:06 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29d283 — 2026-06-10 21:09 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29d2bb — 2026-06-10 21:10 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29d344 — 2026-06-10 21:12 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29d56b — 2026-06-10 21:21 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29d85b — 2026-06-10 21:34 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29daf6 — 2026-06-10 21:45 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29dc67 — 2026-06-10 21:51 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29ded2 — 2026-06-10 22:01 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29dfa5 — 2026-06-10 22:05 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29f14f — 2026-06-10 23:20 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29f203 — 2026-06-10 23:23 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29f231 — 2026-06-10 23:24 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29f293 — 2026-06-10 23:26 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29f2b7 — 2026-06-10 23:26 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a29f31a — 2026-06-10 23:28 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2a0775 — 2026-06-11 00:55 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2a095b — 2026-06-11 01:03 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2a0a14 — 2026-06-11 01:06 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2a0ab1 — 2026-06-11 01:09 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2a0c97 — 2026-06-11 01:17 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2a0e88 — 2026-06-11 01:25 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2a108c — 2026-06-11 01:34 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2a10b6 — 2026-06-11 01:34 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2a1166 — 2026-06-11 01:37 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2a12ed — 2026-06-11 01:44 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2a1371 — 2026-06-11 01:46 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2a1428 — 2026-06-11 01:49 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2a148a — 2026-06-11 01:51 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2a1613 — 2026-06-11 01:57 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2a16e0 — 2026-06-11 02:01 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2a1a41 — 2026-06-11 02:15 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2a1a9e — 2026-06-11 02:17 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2a1bc0 — 2026-06-11 02:21 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2a2499 — 2026-06-11 02:59 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2a25fa — 2026-06-11 03:05 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2a269c — 2026-06-11 03:08 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2a2797 — 2026-06-11 03:12 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2a28ef — 2026-06-11 03:18 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2a29b3 — 2026-06-11 03:21 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2a29d0 — 2026-06-11 03:21 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2a9fd9 — 2026-06-11 11:45 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2aa1bf — 2026-06-11 11:53 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution
docs/index.html hero WebGL rework: (1) 1-LSB hash dither before gl_FragColor kills 8-bit banding; (2) canvas shrunk to top 80% of hero (CSS mask already hid the bottom), mask ellipse rescaled 78%->97.5%, shader remaps uv to hero space (vy=uv.y*0.8+0.2, asp*0.8) so visuals unchanged; (3) warp fbm calls (q) now 3 octaves via fbm3, final n keeps 4 -> 12->10 noise evals/px; (4) fixed 1.2M pixel budget in resize() replaces fixed SCALE=0.7, scale cap 0.8, DPR cap 1.25 kept; (5) u_t wrapped mod 3600 for mediump. Verified via headless chromium + swiftshader screenshot: renders, no seam, no banding. Not git-committed yet.

---

## Commit 6a2aa1cd — 2026-06-11 11:53 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary
docs/index.html hero WebGL rework: (1) 1-LSB hash dither before gl_FragColor kills 8-bit banding; (2) canvas shrunk to top 80% of hero (CSS mask already hid the bottom), mask ellipse rescaled 78%->97.5%, shader remaps uv to hero space (vy=uv.y*0.8+0.2, asp*0.8) so visuals unchanged; (3) warp fbm calls (q) now 3 octaves via fbm3, final n keeps 4 -> 12->10 noise evals/px; (4) fixed 1.2M pixel budget in resize() replaces fixed SCALE=0.7, scale cap 0.8, DPR cap 1.25 kept; (5) u_t wrapped mod 3600 for mediump. Verified via headless chromium + swiftshader screenshot: renders, no seam, no banding. Not git-committed yet.

### This Commit's Contribution


---

## Commit 6a2aaf73 — 2026-06-11 12:52 UTC

### Branch Purpose
implement h5i env (worktree+sandbox) per docs/environments-design.md: phase 1 workspace tier + phase 2 process confinement, with tests

### Previous Progress Summary


### This Commit's Contribution
Root cause: 30fps gate made flow features jump ~2px/frame (choppy). Replaced with <=66fps presentation + adaptive pixel-budget governor (sheds resolution if GPU cannot hold ~60fps, floor 0.15M px, first 3s ignored to dodge load jank) + quintic noise fade (removes grid-aligned creases). Verified headless: no JS errors, governor holds full res through load and sheds only under sustained slowness. Uncommitted in docs/index.html; user to eyeball on real GPU, revert-to-static option open.

---

