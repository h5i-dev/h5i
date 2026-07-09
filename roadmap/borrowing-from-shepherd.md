# Borrowing from Shepherd to improve `h5i`

> Working notes. Ideas mined from `../shepherd/` (the
> [Shepherd](https://shepherd-agents.ai/) project,
> [arXiv:2605.10913](https://arxiv.org/abs/2605.10913)) for `h5i env` and the
> provenance store. See [`environments-design.md`](environments-design.md) for the
> current env design, [`comparison-sandbox.md`](comparison-sandbox.md) for
> the landscape comparison and sibling idea-mine, and `sandbox-production-roadmap` (memory) for the shipped
> and remaining sandbox roadmap.

## TL;DR

Shepherd and `h5i` solve **adjacent but different** problems, which is what makes
Shepherd a good idea-mine.

- **Shepherd** = a Python-embedded *meta-agent framework* optimized for
  **reversibility and single-reviewer settlement**. A task is a bodyless Python
  function whose signature *is* the permission surface; the agent's work comes
  back as a held "retained output"; a human `select` / `release` / `discard`s it.
- **`h5i`** = a git-native *audit and multi-agent-coordination* substrate. Real
  agent CLIs run in a confined worktree, everything is provenance in
  `refs/h5i/*`, and agents coordinate over a shared message bus (i5h).

They barely overlap in code but heavily overlap in intent. Two things worth
stating up front so we don't over-correct:

1. **`h5i` is already ahead of Shepherd on enforcement that is actually wired.**
   Shepherd *names* seccomp / rlimits / an egress broker but leaves them
   unimplemented or unwired: its `EgressBroker` is built and unit-tested but "not
   wired here" (the `jail.loopback_only_port == broker.bound_port` seam is a
   signposted follow-up), `ResourceLimits` is a declared dataclass nothing
   consumes, and seccomp is a docstring in the backend family, not code. `h5i`
   ships all three (seccomp deny-list, `RLIMIT_DATA`/NPROC/CPU/FSIZE, the
   container egress allowlist proxy) plus per-env HOME credential isolation and
   interactive-session config lockdown.
2. **Shepherd has no cross-agent messaging.** Coordination is structural (a parent
   publishes execution-relation facts, children are adopted/abandoned) and
   reviewed by a human. `h5i`'s i5h channel (`refs/h5i/msg`) has no analog on the
   Shepherd side. Keep it as a differentiator.

So this is not "catch up to Shepherd." It is five specific places where
Shepherd's model is genuinely richer than `h5i`'s today.

## How the overlapping surfaces line up

| Concern | Shepherd | `h5i` today |
|---|---|---|
| Sandboxed agent work | jailed retained run (Seatbelt / Landlock) | `env` worktree + process/supervised/container tiers |
| Permission surface | bodyless task signature `May[GitRepo, ReadWrite]` compiled to writable roots | `.h5i/env.toml` profile; `env capabilities` / `status` render the effective enforced policy |
| Review / settlement | retained output → `run select` / `release` / `discard` (consume-once, receipted) | `env propose` → `apply` (3-way, conflict-refusing) / `abort`, each an audit event |
| Trace of record | typed frozen `Effect` stream, per-effect `reverse()` | git history + captures + CRDT deltas + context traces |
| Provenance store | content-addressed DAG with typed multi-role edges (commons-vcs) | JSON records in git notes (`H5iCommitRecord`, `AiMetadata`) |
| Crash recovery | operation-journal WAL + `run repair`, PID-identity locks | `flock` on `run.lock` (no forward-recovery) |
| Multi-agent coordination | structural (parent facts, adopt/abandon) | i5h message bus (`refs/h5i/msg`) |

Already-grounded against `h5i` code: `env apply` performs a conflict-refusing
3-way merge (`src/env.rs`, `apply`), `abort`/`applied`/`removed` all append audit
events, and base drift is already tracked (`Drift`) and surfaced in `env status`.
So the "settlement" and "base drift" rows are **not** gaps — `h5i` has analogs.

## The five borrowable ideas, ranked

### 1. A typed, reversible **effect log** with per-effect `reverse()` — the big one

Shepherd records every boundary crossing as a frozen typed `Effect`
(`shepherd_core/effects/effects.py`; ~30 types, a discriminated union keyed on a
`Literal effect_type`). Result effects store **both pre- and post-state** and
expose `reverse()`:

- `FileCreate.reverse() -> FileDelete` (carries `had_content`)
- `FileDelete.reverse() -> FileCreate`
- `FilePatch.reverse() -> FilePatch` (swaps `old_content` / `new_content`)

The trace is complete-by-construction and *mechanically invertible per effect*.
Ordering is by `timestamp` plus a scope tree (`scope_id` / `parent_scope_id`).

- **`h5i` today:** `apply` / `abort` are coarse — a whole-branch 3-way merge or
  discard the env. Captures and CRDT deltas record *that* changes happened, but
  there is no typed, invertible per-file effect you can selectively roll back
  after apply.
- **Borrow:** a typed effect journal for an env's changes would let `h5i` do
  **partial / selective reversal** ("undo just this file from an applied env")
  and **replay**, not just all-or-nothing. This is the single most differentiated
  capability Shepherd has. Worth a design doc before any code.

### 2. Content-addressed DAG with **typed multi-role edges** (commons-vcs)

Shepherd's `commons-vcs` is not a git wrapper — it is a content-addressed object
kernel (`commons_vcs/kernel.py`, `_types.py`) where **edges carry roles**
(`prior`, `cause`, `witness`, `evidence`, …) and are *part of object identity*
(reordering edges changes the digest). Verification walks are **scoped to a role
subset**: `verify(head, trust_root, walk={"cause"})` re-derives state,
`walk={"witness"}` audits provenance, with collect-all (non-short-circuiting)
failure records. There is an O(1) inverse-edge index (`cited_by`) and a canonical
byte encoding (`commons.canonical.v1`) for cross-language digests. Git is used
purely as an object store plus atomic-ref transactor (blobs at
`refs/commons-vcs/objects/sha256/<hex>`), not for its commit-graph semantics.

- **`h5i` today:** provenance is flat JSON blobs in git notes (`H5iCommitRecord`,
  `AiMetadata`). Rich, but not a queryable graph.
- **Borrow:** `h5i`'s four dimensions (temporal / intentional / empirical /
  associative) map *exactly* onto edge roles. Modeling provenance as a typed-edge
  graph would let `h5i` answer "what evidence / tests support this commit?" or
  "what prompt caused this line?" as a role-scoped walk instead of ad-hoc note
  parsing. Biggest lift here; only pursue if we are already reworking the
  notes-based store.

### 3. **Operation journal (WAL) + `repair`** for crash recovery

Shepherd keeps an append-only, hash-linked journal per operation in
`refs/vcscore/ops/<family>/<operation_id>`
(`vcs-core/.../_world_operation_journal.py`) with a lifecycle state machine
(`open → closed → archived`) and **recovers from interrupted runs**:
`_world_recovery.py` drives an open journal forward to publication or archives a
failed op; `shepherd run repair` reclaims orphaned operations; session locks are
**process-identity-authoritative** (handles PID reuse) rather than time-based.

- **`h5i` today:** env runs are serialized by a `flock` on `run.lock`, but there
  is no visible forward-recovery of a run that died mid-flight — a crashed
  `env run` likely leaves stale state you clean up by hand.
- **Borrow:** an `h5i env repair` that reads a run journal and either completes or
  cleanly aborts an interrupted run, plus PID-identity lock reclaim. Lean
  robustness win that fits the existing lock model.

### 4. `env diff --read <file>` — **smoke-test one candidate file without applying**

Shepherd's headline demo:
`shepherd run changeset --latest --read donut.py | python3 -` runs a proposed
file *straight out of the retained output*, before selecting anything
(`shepherd_dialect/.../changesets.py`, a read-only view over the candidate — it
"does not own custody and cannot settle").

- **`h5i` today:** `env diff` / `env inspect` show the proposal, but you cannot
  cheaply cat / pipe a single candidate file from the env branch to test it in
  isolation.
- **Borrow:** `h5i env diff <name> --read <path>` — emit one file's proposed bytes
  to stdout. Tiny, high-value reviewer ergonomic: "try it before you apply it."
  Roughly an afternoon.

### 5. A **deterministic offline provider** for keyless demos / CI

Shepherd ships a `static` provider (`workspace_control/runtime_provider.py`) so
the entire retain / select machinery runs with no API key — its offline
quickstart and much of its test surface need no live model.

- **`h5i` today:** `env run` needs a real agent CLI; env lifecycle tests are
  capability-gated and skip.
- **Borrow:** a fake deterministic "agent" backend for `env run` would make demos
  keyless and let env lifecycle tests run in CI without a live model.

## One to *not* chase

Shepherd's "**the signature is the permission surface**" (`May[GitRepo,
ReadWrite]` is literally `typing.Annotated`; parsed by
`gitrepo_grant_descriptor_from_may_annotation`, lowered to Landlock/Seatbelt
writable roots) is elegant, but it fits its Python-embedded model, not `h5i`'s.
`h5i` already has the equivalent surface — `.h5i/env.toml` profiles plus
`env capabilities` / `status` rendering the *effective enforced* policy.

The one sub-idea worth keeping in mind is Shepherd's **per-binding grants**
(`docs` read-only + `backend` read-write in one run, clamped against a `may`
ceiling, with a `HeterogeneousBindingAuthorityError` tripwire that refuses to
collapse mixed grants to a scalar). `h5i`'s profile is whole-env, so finer-grained
per-path / per-mount grant tiers *could* be a future refinement — but
`fs.read/write/deny` already covers much of it.

## Recommendation

- **Do first (small, clearly worth it):** #4 (`--read` one candidate) and #3
  (run journal + `env repair`).
- **Design doc next (genuinely powerful):** #1 (reversible effect log) — it
  unlocks partial-undo and replay that `h5i` structurally cannot do today.
- **Only if reworking provenance:** #2 (typed-edge DAG).
- **Nice-to-have for testing/onboarding:** #5 (deterministic provider).
