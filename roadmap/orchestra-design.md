# h5i orchestra: a programmable agent-orchestration eDSL

Status: design accepted; the M1+M2 kernel is implemented in `src/orchestra/` (2026-07-10): journal + step keys + zero-re-execution resume on the team event log, `Conductor` with `step`/`freeze`/`verify`/`judge`/`apply`, `agent().hire()` + `work`/`review`/`revise`, the `VerdictPolicy` trait (built-in rule shared with `team finalize` via `team::default_verdict`/`record_verdict`), `RuntimeLauncher` (`Attach` default, `FnLauncher` for tests/embedding), and `patterns::ensemble`. Naming deviations from the sketches below (builder entry point instead of `#[orchestra::main]`, `approves(&review)` helper) are documented in the module docs. Still open per the roadmap: `gate`, `ask`, `with_materials`, headless launcher, trace rendering, CLI reimplementation over `ensemble`. Companion to `MANUAL.md` ("h5i team") and `docs/environments-design.md`.

## 1. Summary

`h5i team` today ships one topology: N independent workers on one shared task, an optional mutual peer-review round, a neutral verifier, and a hardcoded verdict rule, with the phases driven externally by shell scripts (`team-run.sh`). This proposal makes the topology programmable.

The proposal is a Rust eDSL, crate `h5i-orchestra`, with three commitments:

1. **Define-by-run.** An orchestration is an ordinary async Rust program (a "score"). There is no graph builder, no `compile()` step, no custom control-flow combinators. `if`, `for`, `tokio::join!`, and `?` are the orchestration language. The DAG is a byproduct: every operation appends events to the existing team event log, so audit, visualization, and comparison are derived from the recorded trace, never demanded up front.
2. **Durability by journaling, on git.** Every effectful step (an agent turn, a verification, a shell command) journals its result into `refs/h5i/team/<run>` through the existing CAS `append_event` machinery. Resuming a crashed or interrupted score replays completed steps from the journal without re-executing them: no re-paid agent turns, no duplicated side effects. Because the journal is a git ref, a run is shareable and resumable across clones via `h5i share push` / `pull`.
3. **Agents stay in boxes.** The eDSL adds zero new trust surface. Agents are h5i envs (sandboxed worktrees with policy, evidence capture, and mediated apply), exactly as today. The score is host-side user code, the same trust level as `team-run.sh`, which it replaces.

Everything `h5i team` does today becomes one prebuilt pattern (`patterns::ensemble`) expressible in roughly ten lines of the eDSL.

## 2. Where we are: `h5i team` today

The existing implementation (`src/team.rs`, ~3.7k lines) is already the right substrate. What it has:

- **Event-sourced state.** One git ref per run (`refs/h5i/team/<run-id>`) whose tree holds an append-only `events.jsonl`. `append_event` commits with a CAS retry loop; `project` folds events into `TeamRun`. No mutable state anywhere. This is, almost verbatim, the journal a durable-execution engine needs.
- **Roster as data.** A `TeamAgent` is a persona bound to an env (`add_env`), with runtime, model, isolation claim, and policy digest recorded. Identity is injected into the box via host-owned `team-identity` / `team-run` files.
- **Typed artifacts.** `TeamArtifact` freezes a submission as commit + tree OIDs with diffstat, capture ids, and independence/influence edges. `TeamVerification` records a neutral sandboxed re-run. Evidence is first-class.
- **Message routing.** Dispatch and review grants are i5h messages plus per-env read-only inboxes; box-to-host results travel through spools drained by `sync_outbound`.

What is hardcoded, and what this design generalizes:

| Hardcoded today | Location | Generalized to |
|---|---|---|
| Two-agent claude+codex roster | `auto_create_roster` | roster builder in the eDSL |
| Mutual review circle | `auto_peer_review` | any review topology, in user code |
| Verdict rule `VerifierTestsPass,AppliesCleanly,SmallestDiff` | `finalize` | `VerdictPolicy` trait |
| Phase progression driven by shell scripts | `team-run.sh` etc. | the score program |
| One task per run | `dispatch` | any number of tasks, rounds, branches |

## 3. What we learned before designing

### 3.1 The field (2025-2026)

We surveyed Microsoft Agent Framework (the AutoGen + Semantic Kernel merger), LangGraph, AutoGen v0.4, the OpenAI Agents SDK, CrewAI, and the durable-execution engines (Temporal, Restate, Inngest). Three findings shape this design.

**Nobody stayed purely declarative.** Every graph-first framework grew a define-by-run escape hatch (LangGraph's `@entrypoint`/`@task` functional API, MAF's experimental `@workflow`/`@step`, CrewAI's Flows), and the acknowledged ergonomics benchmark, the OpenAI Agents SDK, is pure code-first: "use built-in language features to orchestrate and chain agents, rather than needing to learn new abstractions." The converged position: explicit, durable structure for the control plane you must audit and resume, plain code for everything else.

**AutoGen is the cautionary tale.** Its unconstrained actor pub/sub was expressive but uncheckpointable and hard to control; it is now in maintenance mode, and its successor wrapped the actors in a typed, checkpointable engine. Emergent group-chat control flow lost to programmed control flow.

**Three durability designs exist, and journaling is the sweet spot.** Temporal replays deterministic workflow code against an event history (strongest, but imposes a determinism contract and versioning discipline on all user code). LangGraph/MAF checkpoint at superstep barriers and re-execute forward on resume (simplest, weakest: side effects must be idempotent and LLM calls can be re-paid). Restate/Inngest journal each named step's result and skip completed steps on resume. For agent orchestration, where a single step costs minutes and dollars, journaling wins: the determinism contract shrinks to "the cheap glue between steps," and completed agent turns are never re-run. Restate's idiomatic Rust SDK (`ctx.run(|| ...)`) proves the shape works in Rust.

We also confirmed a gap: no Rust crate combines agent-client ergonomics (rig), checkpointed orchestration (LangGraph-class), durable execution (Temporal/Restate-class), and a coding-agent substrate (worktrees, sandboxes, review gates, provenance). h5i already owns the fourth, which is the hardest to retrofit.

### 3.2 The PyTorch lesson

The user-facing question "graph DSL or eDSL?" was settled a decade ago in a different domain. Theano, Caffe, and TensorFlow 1.x asked users to describe computation to a smarter executor; Chainer and PyTorch let users perform the computation and quietly observed it. The observed-execution side won on debuggability (errors surface in the user's own stack frame, not inside `session.run`), host-language control flow (loops and conditionals just work), incremental learnability, and hackability. Performance, the static camp's whole justification, never materialized as a decisive user-visible advantage. TensorFlow 2's eager retrofit still lost, because a retrofit carries two subtly different execution modes and burned API trust. PyTorch later harvested graph benefits without losing eager semantics (torch.compile: trace what runs, fall back to eager on anything unsupported), while TorchScript, which demanded a rewrite into a typed subset, failed.

Principles we adopt, with the receipts:

1. **The host language is the control flow.** No `while_loop`, no conditional-edge API. Rust's `if`/`for`/`match`/`join!`/`select!` route agents. (Theano and TF1 died reifying control flow the host already had.)
2. **The graph is a trace of execution, not a prerequisite for it.** Record the DAG as it runs; derive replay, audit, and rendering from the trace. (Chainer/PyTorch; also exactly what the team event log already does.)
3. **Errors surface where the user wrote the code.** A failed agent turn is an `Err` at the `.await` site, carrying env id, capture id, and exit status. Never "error inside the runtime."
4. **Never require a rewrite for the advanced tier.** The same score that runs fire-and-forget is resumable and shareable with zero annotations. (TorchScript demanded a rewrite and died; TorchDynamo optimized unmodified code and won.)
5. **Worse is better.** Ship few composable primitives (hire, work, review, verify, judge, gate, step) and push the rest to userland patterns. (PyTorch's stated principle, verbatim.)
6. **Progressive disclosure of complexity.** A one-liner for the 80 percent case, patterns in the middle, traits at the bottom, one coherent spectrum. (The Keras lesson.)
7. **Escape hatches beat walled gardens.** Every layer bottoms out in things the user already knows: envs, git refs, JSONL, i5h messages, plain processes.
8. **Constraint only where the payoff is transformational.** We demand step-identity discipline (named, stable step keys) because it buys durable resume and cross-clone handoff, the analog of `grad`: something users cannot get otherwise. Nothing else is constrained.

## 4. The design

### 4.1 Shape

A user writes a normal Rust binary (a **score**) that depends on `h5i-orchestra`. The crate hands the program a `Conductor`: the handle through which agents are hired, work is dispatched, and results are judged. Running the binary creates (or resumes) a team run; every operation appends typed events to that run's log.

```
┌──────────────────────────────────────────────────────────┐
│ score (user's Rust program, host-side, trusted)          │
│   Conductor ──┬── Agent (wraps TeamAgent + env)          │
│               ├── step()/journal   (durability)          │
│               ├── verify()/judge() (evidence + verdict)  │
│               └── gate()           (human-in-the-loop)   │
├──────────────────────────────────────────────────────────┤
│ h5i-orchestra (new crate, thin)                          │
├──────────────────────────────────────────────────────────┤
│ existing h5i machinery (unchanged trust model)           │
│   team.rs   event log, roster, artifacts, verify, apply  │
│   env.rs    sandboxed worktrees, spools, inboxes         │
│   msg.rs    i5h routing, human notification              │
│   objects   evidence captures                            │
└──────────────────────────────────────────────────────────┘
```

### 4.2 A complete score

The current `h5i team` behavior, written in the eDSL:

```rust
use h5i_orchestra::prelude::*;

#[orchestra::main] // opens/creates the run, wires resume, installs panic context
async fn main(c: Conductor) -> Result<Outcome> {
    let task = c.args().task("implement `h5i pull` mirroring `h5i push`");

    // Roster: each agent is an env. hire() is journaled; on resume it
    // rebinds to the existing env instead of creating a new one.
    let claude = c.agent("claude")
        .runtime(Runtime::Claude)
        .persona("personas/implementer.md")
        .hire().await?;
    let codex = c.agent("codex")
        .runtime(Runtime::Codex)
        .persona("personas/skeptic.md")
        .hire().await?;

    // Fan-out is host-language concurrency. Each work() launches one
    // sealed, capture-wrapped session in the agent's box and resolves
    // to the frozen TeamArtifact when the agent submits.
    let (a, b) = tokio::try_join!(claude.work(&task), codex.work(&task))?;

    // Cross review, plain loop. Influence edges and independence
    // stamping happen underneath, exactly as today.
    let (mut a, mut b) = (a, b);
    for round in 1..=2 {
        let (ra, rb) = tokio::try_join!(codex.review(&a), claude.review(&b))?;
        if ra.approved() && rb.approved() { break; }
        (a, b) = tokio::try_join!(
            claude.revise(&a, &ra),
            codex.revise(&b, &rb),
        )?;
        c.note(format!("revision round {round} complete"));
    }

    // Neutral verification: fresh sandboxed worktree per artifact,
    // never the author's box.
    let verified = c.verify([&a, &b])
        .command("cargo test --quiet")
        .isolation(Tier::Container)
        .await?;

    // Pluggable verdict. tests_then_smallest_diff() reproduces
    // today's hardcoded finalize rule.
    let verdict = c.judge(&verified, policy::tests_then_smallest_diff()).await?;

    // Human gate: durable. The score can exit here and resume after
    // the human replies (i5h ASK + inbox), even on another day.
    let winner = verdict.selected()?;
    if c.gate(format!("apply {}?", winner.id)).approve().await? {
        c.apply(winner).await?;
    }
    Ok(verdict.into())
}
```

Nothing above is a new concept for an h5i user: `hire` is `env create` + `team add-env`, `work` is dispatch + sealed session + submit + sync, `review`/`revise` are the grant/review/resubmit loop, `verify`/`judge`/`apply` are `team verify`/`finalize`/`apply`. The eDSL contributes composition, typing, and durability.

### 4.3 Primitives

Deliberately few. Everything else is a pattern built from these.

| Primitive | Returns | Journaled as | Wraps |
|---|---|---|---|
| `c.agent(name)…hire()` | `Agent` | `agent_hired` | `env::create` + `team::add_env` |
| `agent.work(task)` | `Artifact` | `work_done` | dispatch, sealed session, submit, sync |
| `agent.ask::<T>(prompt)` | `T: DeserializeOwned` | `ask_done` | a session that must emit schema-valid output; no artifact |
| `agent.review(&artifact)` | `Review` | `review_done` | grant + review submit |
| `agent.revise(&artifact, &review)` | `Artifact` | `work_done` | feedback delivery + new submission |
| `agent.work(task).with_materials(arts)` | `Artifact` | `work_done` | scoped diff grants on the inputs + influence edges |
| `c.verify(arts)…` | `Vec<Verified>` | `verify_done` | `team::verify` (neutral sandbox) |
| `c.judge(&verified, policy)` | `Verdict` | `verdict` | generalized `finalize` |
| `c.apply(&artifact)` | `Applied` | `applied` | `team::apply` (mediated, gated) |
| `c.gate(question)…` | `Answer` | `gate_requested` / `gate_answered` | i5h ASK to a human + durable wait |
| `c.step(label, f)` | `T: Serialize` | `step_done` | arbitrary user effect, journaled |

Notes:

- **`ask` vs `work`.** `work` produces code (a frozen `TeamArtifact`); `ask` produces data (a typed value extracted from a schema-constrained reply). `ask` is how judge panels, routers, and summarizers are built: `let s: Scorecard = judge_agent.ask(prompt).await?;` with `Scorecard: Deserialize + JsonSchema`. The schema is enforced at the boundary and the raw transcript is still captured as evidence.
- **`with_materials` is the integration affordance.** It grants the working agent scoped read of other artifacts' diffs (the same grant machinery as review, same `GRANTABLE_ARTIFACT_KINDS`) and stamps the resulting artifact `independent=false` with influence edges to every input. This is how an integrator agent fuses N implementers' submissions: apply the granted patches in its own worktree, resolve conflicts, submit one merged artifact. A cheap escalation ladder keeps agent tokens for judgment calls only: try a mechanical `git merge` via `c.step`, fall back to `h5i resolve` per conflicted file, and hand only the semantic conflicts to the agent.
- **`step` is the universal escape hatch.** Any side effect the user wants durable (calling `gh`, fetching an issue list, running a formatter on the host) goes through `c.step("label", || async { ... })`. It executes once, journals the serialized result, and replays from the journal on resume. Restate's `ctx.run`, on git.
- **`gate` is the HITL primitive.** It sends an i5h ASK (so the human sees it in `h5i msg` and via the Stop hook), appends `gate_requested`, and then either waits (`msg wait`) or, with `--detach`, lets the score exit cleanly. Resume finds the answer in the inbox, journals `gate_answered`, and continues past the gate. Every framework surveyed made HITL first-class; ours must additionally survive process exit, and does, because the journal is the ref.
- **`VerdictPolicy` is a trait**, not a string. Built-ins reproduce today's rule; a user policy sees `&[Verified]` and returns a `Verdict` with reasons. An LLM-judge policy is just a policy that calls `agent.ask` inside.

### 4.4 The patterns layer

MAF ships five prebuilt orchestrations compiled onto its engine; we do the same, as ordinary functions in `h5i_orchestra::patterns`, each implemented in the public eDSL (readable, forkable, no privileged API):

```rust
// today's `h5i team` in one line
patterns::ensemble(&c, task).agents([claude, codex]).rounds(2).run().await?;

// others shipped at v1
patterns::pipeline(&c, [(architect, design), (implementer, build), (reviewer, check)]);
patterns::arena(&c, task).agents(roster).judge(policy).run().await?;   // env compare, ranked
patterns::map_reduce(&c, items, |agent, item| ...).reduce(|arts| ...); // fan-out over a work list
patterns::debate(&c, question).sides([a, b]).moderator(m).rounds(3);
```

This is the progressive-disclosure ladder, top to bottom:

1. **CLI, unchanged.** `h5i team auto-create` and friends keep working; internally they become invocations of `patterns::ensemble`. Casual users never see Rust.
2. **Patterns.** One function call in a five-line score.
3. **The eDSL.** Full programs like section 4.2.
4. **Traits.** `VerdictPolicy`, `RuntimeLauncher` (how a runtime's CLI is invoked in the box, generalizing `team-launch.sh`'s hardcoded claude/codex argv), `Journal` (the event-log backend, git by default).

One dialect per layer, and each layer is implemented in the layer below it. No parallel APIs.

## 5. Execution and durability model

### 5.1 Execution model: resident sessions

The score is a coordinator process, not an LLM client. It holds no model connection and no agent state; it emits instructions and grants into the run, and awaits evidence-bearing events. The LLM substrate is the resident interactive session h5i teams already use: `team-launch.sh` starts one interactive box per agent (`h5i env shell <env> -- claude …`), and the team Stop hook (`h5i team agent hook --block`) holds it alive between turns by blocking the stop, waiting on the env inbox, and injecting the next instruction as the block reason. Sessions stay warm and stateful (conversation memory, prompt cache, loaded repo context), so turn dispatch is sub-second, never a cold process boot. The eDSL does not replace this machinery; it replaces the poll loop in `team-run.sh` that feeds it.

One `agent.work(task)` call is this sequence:

1. The score appends a `work_dispatched` event and sends an i5h ASK fanned into the agent's host-owned, read-only env inbox (today's `dispatch`).
2. The agent's already-running session is parked in its Stop hook waiting on that inbox. The hook releases with the instruction, in the same session with all accumulated state.
3. The agent works in its box, commits, and runs `h5i team agent submit`, which writes the outbound spool (the box writes what, never who).
4. The score ingests the spool (`sync_outbound`), the frozen `TeamArtifact` lands as a `work_done` journal event, and the `work()` future resolves with it. The score watches the event ref via `notify` (the `watcher.rs` machinery) rather than polling.

This division of labor is also what keeps resume cheap: the journal makes the coordinator stateless-resumable, while agent statefulness lives where it already lives, in the env worktree, the per-env HOME copy (the runtime's own session files persist across runs, so an in-box session can be resumed with `--continue`), and the live session itself.

Session bring-up is the `RuntimeLauncher` trait's job, with three strategies:

- **Attach (default).** `work()` requires a live session, detected via heartbeat/lease events (the existing `worker` lease machinery). If none is live, it fails fast with a clear message ("no live session for codex; run `h5i team launch`") rather than silently degrading to headless.
- **Launch-resident.** The score spawns the interactive session itself at `hire()` time (detached: tmux pane, terminal window, or background pty) and reuses it for every turn. This internalizes `team-launch.sh`.
- **Headless.** `claude -p` / `codex exec` per turn, as an explicit opt-in for CI-style scores where no resident session can be babysat. The cost is stated plainly: a cold process boot per turn and no cross-turn state beyond what the worktree, `PERSONA.md`, the context branch, and the runtime's `--resume` session files carry. Never the default.

### 5.2 Journal

The journal is the existing event log: `events.jsonl` in `refs/h5i/team/<run>`, CAS-appended, folded on read. The eDSL adds event kinds (`score_started`, `step_done`, `gate_requested`, `gate_answered`, `agent_hired`, ...) to the open vocabulary; `project` ignores kinds it does not know, so old and new binaries coexist. Results larger than a small inline cap (4 KiB) are stored as evidence captures and referenced by id from the event, keeping the ref lean while keeping everything recallable through `h5i recall object`.

### 5.3 Step identity and resume

Every journaled operation has a **step key**: `(label, per-label sequence number)`. Labels come from the API (`agent.work` labels itself `work/<agent>`; `c.step` takes an explicit label). Sequence numbers count per label, not globally, so two concurrent branches with distinct labels produce stable keys regardless of interleaving or completion order. This is the one discipline the eDSL asks of users: steps inside unbounded concurrency must carry distinct labels (the API enforces it by construction for agent operations, and `c.step` in a loop wants `format!("fetch/{i}")`).

Resume (`h5i team resume <run>`, or just re-running the score binary with the run pinned) re-executes the score from `main`. Each operation first consults the journal by step key: hit means return the recorded result immediately (no agent launched, no cost); miss means execute live and journal. The determinism contract is therefore only: the glue code between steps must reach the same steps in the same per-label order given the same journal. Wall-clock, randomness, and environment reads that affect control flow should go through `c.step` so they are journaled too. A `score_digest` (hash of the binary) is recorded at start; resuming with a different digest logs a loud warning, and a Temporal-style `c.patched("change-id")` marker is available for deliberate mid-run upgrades. A key mismatch on replay fails closed with both the expected and found keys, pointing at the exact divergence.

This is why we do not need MAF's and LangGraph's Pregel-style superstep barriers: they buy a checkpoint boundary at the cost of a foreign execution model. Journaling puts the boundary at every step and leaves execution to tokio, which users already know how to debug. `dbg!`, breakpoints, and `RUST_LOG` work mid-score because there is nothing between the user's code and its execution.

### 5.4 Concurrency

Parallel steps append events concurrently; the CAS loop in `append_event` already serializes writers, and event folding is order-tolerant (dedup by id, sort by parent/ts/id). Env-level concurrency is unchanged: each agent works in its own worktree and branch; `run.lock`/`observers.lock` semantics are untouched.

### 5.5 Cross-clone runs

Because the journal is a ref, `h5i share push` moves a live run to another clone. The reviewer-side clone can run a score (or plain CLI) against the same run id: pulled artifacts have no worktree, so review and apply fall back to branch-tip diffs exactly as env pull does today. This enables the flagship workflow: claude's clone runs the score up to the gate, a human or a codex on another machine reviews and answers, either side resumes. No orchestration server exists, so there is nothing to keep alive; the git remote is the coordination point. None of the surveyed frameworks can do this without a hosted runtime.

## 6. Security and audit invariants (unchanged)

- Agents execute only inside envs, under the same profiles, tiers, and evidence capture as today. The eDSL launches sessions through the same `env shell` path with the same hooks (wrap-bash, managed settings, tee shim).
- `apply` remains mediated and human-gated by default; a score cannot make apply automatic without the same explicit force that the CLI requires. `c.gate` makes the approval durable, not optional.
- The score itself is host-side user code, precisely the trust level of `team-run.sh` today. It does not run in a box, and boxed agents cannot invoke it or append to its journal except through the existing spool-ingest path, which stays host-validated ("box writes what, never who").
- Incoming agent output remains untrusted: `ask`'s schema validation is an extraction boundary, not a trust boundary; display still goes through `sanitize_display`.

## 7. What we deliberately did not build

- **A graph builder API.** Rejected as the primary surface (principle 1). If a static description is ever wanted for pre-run policy review ("this score can call apply at most once"), it can be derived by running the score against a journal in dry-run mode, not by asking users to write the graph. Trace first, graph as a view.
- **A YAML/TOML topology config.** CrewAI-style declarative crews become one pattern (`patterns::from_manifest`) if demand appears; they are not the foundation, because every declarative-first framework surveyed had to bolt on code-first control later.
- **An actor/pub-sub runtime.** AutoGen v0.4 tried it; uncheckpointable emergent control flow is the failure mode our users (auditable coding agents) can least afford.
- **An embedded scripting language** (rhai/rune) to dodge the Rust compile loop. Tempting for iteration speed, but it would create a second dialect with subtly different semantics, the exact trust-burning mistake of the TF1/TF2 era. Deferred to open questions; if it ever lands, it must be a 1:1 mirror of the Rust API, generated from it.
- **An external orchestration server.** Temporal/Restate durability without the operational dependency is the whole point of journaling on git. A `Journal` trait keeps the door open for a server backend later, as the drop-in-runner seam every 2025-26 framework converged on.

## 8. Positioning

| | MAF / LangGraph | OpenAI Agents SDK | Temporal / Restate | h5i-orchestra |
|---|---|---|---|---|
| Surface | graph builder (+ functional escape) | code-first | code-first + server | code-first eDSL |
| Durability | superstep checkpoints, re-execute forward | none built-in | replay / journal, hosted | journal on a git ref |
| Resume cost | may re-run LLM calls | n/a | zero re-run | zero re-run |
| Cross-machine handoff | hosted platform | no | hosted cluster | `git push` |
| Agent isolation | none | none | none | sandboxed envs, policy, evidence |
| Typed steps | runtime type routing | Pydantic | serde | serde + JsonSchema, compile-time |

The moat is the bottom two rows: provenance-grade evidence and kernel-enforced isolation are what h5i already has and what none of the orchestration frameworks own.

## 9. Roadmap

- **M1: kernel.** `h5i-orchestra` crate; `Conductor`, journal on the team event log, step keys, resume; `agent().hire()`, `work`, `c.step`. Ensemble expressible manually. Workspace tier only. Exit: the 4.2 score runs, is killed at any point, and resumes without re-running completed agent turns.
- **M2: parity.** `review`/`revise`/`verify`/`judge`/`apply`; `patterns::ensemble` reproduces today's `team-run.sh` behavior end to end; the CLI auto-flow reimplemented over it; `team-run.sh` deprecated.
- **M3: durability surface.** `gate` with detach/resume, `patched`, score digests, `h5i team trace <run>` rendering the recorded DAG (text and dot), journal-divergence diagnostics.
- **M4: breadth.** `ask` with schema enforcement, `VerdictPolicy` and `RuntimeLauncher` traits, `with_materials` grants, `patterns::{arena, pipeline, map_reduce, debate, integrate}`, container-tier verification in scores. `integrate` packages the multi-implementer flow (fan-out, merge in an integrator env via `with_materials`, verify), and `map_reduce`'s `reduce` slot accepts an integrator agent in place of host code: the reduce step of a fan-out is exactly the conflict-resolution seat.
- **M5: distribution.** Cross-clone resume hardening, run handoff docs, multi-human gates, optional `Journal` backend seam.

## 10. Open questions

1. **Score packaging.** A score is a bin crate: fine for repo-resident orchestrations (`orchestrations/` directory, workspace members), but the cold-start story for "I want a 5-line score right now" needs `h5i team score new` scaffolding, and possibly a precompiled runner for the patterns layer so the CLI path never compiles anything.
2. **Compile-loop friction.** The honest cost of a Rust eDSL versus Python. Mitigations: keep `h5i-orchestra` dependency-light for fast builds, make patterns cover the common 80 percent through the CLI, and revisit a generated scripting mirror only if users demonstrably stall here.
3. **Concurrent gates.** Multiple simultaneous `gate`s addressed to different humans need inbox threading conventions (i5h `thread_id` exists; the UX does not yet).
4. **Journal growth.** Long runs accumulate events; we may need journal compaction (snapshot event) mirroring what delta_store does for CRDT updates.
5. **Cost accounting.** Steps should carry token/wall/cpu accounting (env exec events already do); a `c.budget()` guard analogous to resource rlimits is a natural M4+ addition.
