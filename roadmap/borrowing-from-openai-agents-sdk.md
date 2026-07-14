# Borrowing from the OpenAI Agents SDK to improve h5i orchestra

> Working notes. Ideas mined from `../openai-agents-python` (the OpenAI Agents
> SDK) for the orchestra eDSL (`crates/h5i-orchestra/`) and its Python SDK
> (`../h5i-python`). See [`orchestra-design.md`](orchestra-design.md) for the
> current design and its §7 non-goals, which several SDK features would
> violate; this doc respects them.

## TL;DR

The two systems orchestrate at different altitudes, which is what makes the
SDK a good idea-mine rather than a competitor.

- **OpenAI Agents SDK** = an in-process *model-call* orchestrator. An "agent"
  is instructions + tools + model settings around an LLM API call; the Runner
  loops tool calls until a final output. Multi-agent = handoffs (peer
  transfer) or agents-as-tools (manager). Durability is delegated to
  third-party engines (Temporal, Dapr, Restate, DBOS) via `RunState`
  serialization.
- **h5i orchestra** = a *session-and-sandbox* orchestrator. An agent is a
  whole resident coding-agent CLI session bound to a confined git worktree;
  every effectful op is journaled to a git ref; evidence (captures,
  verifications, independence stamps) is the coordination currency.

h5i is already **ahead** on the things it was built for. Do not re-derive
these from the SDK:

1. **Durability.** The journal on `refs/h5i/team/<run>` gives crash-resume,
   cross-clone resume (`h5i share push`), and `patched` migration natively.
   The SDK needs an external Temporal/Dapr integration for the same.
2. **HITL.** `c.gate()` is durable across process exit and answered over a
   real message channel. The SDK's approvals live inside one serialized
   `RunState`.
3. **Trust and provenance.** Independence stamps, sealed-phase materials,
   neutral re-verification in fresh worktrees, evidence-grounded judging with
   citation validation, isolation tiers that fail closed. No SDK analog at
   all.

Where the SDK is genuinely richer is the **run-loop ergonomics**: guardrails,
lifecycle hooks, streaming events, usage accounting, typed outputs, error
handling, context shaping, and provider breadth. Those are the borrowables.

## How the surfaces line up

| Concern | OpenAI Agents SDK | h5i orchestra today |
|---|---|---|
| Unit of agency | `Agent` dataclass around an LLM call | roster seat: resident CLI session + sandboxed env |
| Control flow | Python around `Runner.run` | Rust/Python around `Conductor` (define-by-run, same instinct) |
| Durability | `RunState.to_json` + external Temporal/Dapr | git-ref journal, replay kernel, `patched` |
| Multi-agent | handoffs, agents-as-tools | work/review/revise/ask turns over i5h, patterns module |
| Safety | guardrails + tripwires (text-level) | sandbox tiers, independence stamps, mediated apply (action-level) |
| HITL | `needs_approval` interruptions, resume from state | durable `gate` over i5h |
| Observability | traces/spans, ~35 exporter integrations, streaming | `team trace [--dot]`, event log, web dashboard, polling |
| Cost | `Usage` aggregation per run/request | wall/cpu/rss per exec; **no tokens** (known M5 gap) |
| Typed outputs | `output_type` strict schemas on any agent | `ask::<T>` parse-retry only; reviews are free text |
| Providers | Responses/ChatCompletions/LiteLLM, 100+ models | two runtime adapters (claude, codex), fail-closed |

## Tier 1: high power, clear fit

### 1. Turn guardrails with tripwires, upgraded to evidence-level

**SDK:** `@input_guardrail` / `@output_guardrail` functions return
`GuardrailFunctionOutput(tripwire_triggered=...)`; a tripped wire halts the
run with a typed exception. Input guards can run in parallel with the agent
or block before it starts. Tool-level guards can rewrite/reject individual
tool calls (`src/agents/guardrail.py`, `tool_guardrails.py`).

**h5i today:** validation exists only after the fact: `verify`, `judge`,
`expect_independent`. Nothing screens a task before dispatch or an artifact
at submit time, and nothing watches the turn while it runs.

**Proposal:** journaled guard hooks on the turn lifecycle:

- *input guards* run before `dispatch_turn` (task text + materials);
- *output guards* run on the completed artifact/reply (diffstat, touched
  paths, capture findings) before the score sees it;
- a tripped wire fails the turn with a typed error and appends an
  `orch_guardrail` event with the guard's findings.

The h5i-unique upgrade: guards read **observed evidence, not just text**.
The tee shim, wrap-bash captures, and `egress-denied` findings mean a guard
can assert "no denied egress during this turn", "diff does not touch
`auth/`/secrets paths, else require a gate", or "tests were actually run".
The SDK can only inspect strings; h5i can inspect behavior. This composes
with `recall search --fingerprint` for "this exact failure again" guards.

### 2. Token/cost usage accounting and budgets

**SDK:** `context.usage` aggregates requests, input/output/cached/reasoning
tokens across handoffs and tools, with per-request breakdown
(`src/agents/usage.py`). Everything downstream (evals, cost caps) hangs off
it.

**h5i today:** exec events record wall/cpu/peak-RSS; `orch_step` records
`duration_ms`. Token accounting is an acknowledged M5 gap
(`orchestra-design.md` §9).

**Proposal:** per-turn usage capture. The resident runtimes already write
usage into their session transcripts (Claude Code JSONL, codex logs); the
turn-completion path can mine the delta and attach
`{input_tokens, output_tokens, cached, reasoning, model}` to the turn's
`orch_step` payload. Then:

- `c.budget(tokens/usd)` as an rlimit-style guard: a dispatch that would
  exceed it fails closed (the design doc already sketches this);
- cost columns in `team trace`, `status`, `compare` (the arena should rank
  cost-per-passing-verification, which no SDK offers);
- budget-aware `VerdictPolicy` (prefer the cheaper of two verified winners).

h5i can go further than the SDK here because it also has rusage: report
tokens *and* compute per candidate. That makes `compare` an actual
price/performance arena.

### 3. Structured outputs beyond `ask`: typed reviews, typed gates

**SDK:** `Agent(output_type=PydanticModel)` forces strict-schema final
outputs on any agent; invalid output is a typed, recoverable error
(`agent_output.py`). Handoffs can carry validated payloads (`input_type`).

**h5i today:** `ask::<T>` has robust extraction + bounded re-ask, but it is
the only typed channel. Reviews are free text settled by the `approves()`
first-token convention (already bitten once: the delabeling fix). Gate
answers are the same convention.

**Proposal:**

- **Typed reviews:** embed a review schema in the review-turn instruction
  (`{verdict: approve|revise|block, findings: [{path, note, severity}]}`),
  validate on ingest with the existing re-ask machinery, keep the free-text
  body as the human-readable rendering. `approves()` becomes a fallback for
  legacy/manual reviews instead of the load-bearing parser.
- **Typed gates:** `gate(question).choices(["ship", "hold", "rework"])`
  renders the options in the ASK and validates the reply, so multi-outcome
  human decisions stop being string-matching.
- **Work acceptance schemas:** optional `work(...).expect::<T>()` for turns
  whose deliverable is data-plus-code (e.g. a migration report).

### 4. Streaming run events: push, not poll

**SDK:** `run_streamed()` yields semantic `RunItemStreamEvent`s
(tool_called, handoff_occurred, message_output_created...) alongside raw
deltas; UIs and nested tools consume them live (`stream_events.py`).

**h5i today:** the score polls the event log (1.5s/15s interval); the RPC
bridge is request/response only; the web dashboard re-reads state.

**Proposal:** a `conductor.watch()` event stream:

- server→client JSON-RPC notifications in `rpc.rs` (the protocol already has
  the one server→client call, `launcher.on_turn`; notifications are a small
  extension) carrying the folded semantic events: turn dispatched, spool
  drained, submission, review, verification, verdict, gate asked/answered;
- an async iterator in `h5i-python` (`async for ev in c.watch(): ...`), which
  also lets scores replace poll-loops in custom waits;
- the same feed powering live TeamView updates and `h5i team trace --follow`;
- stretch: live turn output tailing by streaming capture/spool increments,
  which the SDK cannot do at all (it streams model deltas; h5i can stream
  *observed commands*).

### 5. LLM-routed orchestration: triage pattern + conductor-as-tools

**SDK:** the two first-class multi-agent shapes are *handoffs* (a triage
agent routes to specialists) and *agents-as-tools* (a manager agent invokes
specialists as tools and keeps the conversation). Routing decisions are made
by a model, not host code (`docs/handoffs.md`, `agents_as_tools.py`).

**h5i today:** all routing is host code. That is the right default (the
journal needs deterministic labels), but it means no score can say "let a
cheap model decide which specialist gets this task" without hand-rolling an
`ask` + `match`.

**Proposal:** two layers, keeping the trust boundary intact:

- **`triage` pattern** (cheap, immediate): a router seat classifies the task
  via a typed `ask` (`{route: "backend"|"frontend"|..., reason}`), the
  pattern dispatches `work` to the chosen specialist, journaled like any
  step. Ships as a ~40-line pattern like the others.
- **Conductor ops as MCP tools** (the big one): expose a mediated subset
  (`orch_work`, `orch_ask`, `orch_review`, `orch_status`) through `mcp.rs`
  so a *lead agent in a box* can drive other seats' turns as tool calls,
  every one journaled and attributed. Mediation stays host-side: the lead
  can request turns, never `apply`, never touch the journal directly (same
  spool-ingest trust posture as submit). This gives h5i the SDK's
  manager-pattern expressiveness with an audit trail the SDK lacks.

## Tier 2: strong ergonomics

### 6. Handoff as a first-class op, with input filters

**SDK:** `handoff(agent, input_filter=...)` transfers the conversation; the
filter edits exactly what the next agent sees (e.g. `remove_all_tools`);
optional nested-history summarization collapses the transcript into one
block (`handoffs/`, `extensions/handoff_filters.py`).

**h5i today:** `pipeline` approximates handoff by passing the previous
artifact as materials; i5h already has a `handoff` message kind with
branch/context/focus links, but orchestra does not use it.

**Proposal:** `agent.handoff_to(other, artifact).with_filter(f)` where the
filter shapes the delivered context: full diff, diffstat only, summary
capture, or a custom render of `TeamRun` state. Journaled, stamped as an
influence edge (handoffs are honest non-independence). The nested-history
idea maps to delivering a *context-branch summary* instead of raw
materials: cheaper turns, same provenance.

### 7. Lifecycle hooks

**SDK:** `RunHooks`/`AgentHooks` with on_agent_start/end, on_llm_start/end,
on_tool_start/end, on_handoff; the standard seam for logging, metrics, and
custom accounting (`lifecycle.py`).

**h5i today:** hooks exist at the *runtime* level (wrap-bash, Stop hook) but
the score has no seam: you cannot run code on every turn boundary without
wrapping every call site.

**Proposal:** `ConductorBuilder::hooks(h)` with
`on_turn_start/on_turn_end/on_verify/on_verdict/on_apply/on_gate`, receiving
the same context the journal records. Never journaled (observers, not
steps), so they stay side-effect-safe under replay: on replayed steps they
fire with `replayed=true`. Python side: plain callables on the Conductor.
This is the natural attachment point for #1 guards, #2 budget checks, and
#14 exporters, so build it before those.

### 8. Typed turn-failure taxonomy + error handlers

**SDK:** a clean exception tree (`MaxTurnsExceeded`, `ModelBehaviorError`,
`ToolTimeoutError`, guardrail tripwires) plus `error_handlers={...}` on the
run for graceful fallbacks (`exceptions.py`, `run_error_handlers.py`).

**h5i today:** a turn timeout or dead session surfaces as a generic error
and the score aborts; recovery logic (retry, reassign) must be hand-written
around every await. The Python SDK has good exception types for *protocol*
errors but turn failures arrive as opaque `H5iError`s.

**Proposal:** typed turn outcomes (`Timeout`, `SessionDead`, `ParseFailed`,
`Refused`, `GuardTripped`) and a per-conductor handler map:
`on_timeout: Retry{max: 2} | Reassign{to} | Gate | Fail`. Handlers are
journaled when they fire (an `orch_recovery` event), so a resumed run
replays the same recovery path. `preflight` catches dead sessions before
the run; this catches them during.

### 9. Dynamic instructions and prompt shaping

**SDK:** `instructions` can be a function of (context, agent); `RunConfig`
has `call_model_input_filter` to edit the fully-prepared model input right
before each call; `prompt_with_handoff_instructions` standardizes the
multi-agent prelude (`agent.py`, `run_config.py`).

**h5i today:** turn instructions are static strings assembled by orchestra
around `AGENT_BOOTSTRAP`; a score wanting run-state-aware task text builds
strings by hand at every call site.

**Proposal:** accept instruction *builders* (`Fn(&TeamStatus) -> String`,
or a callable in Python) anywhere a task string is accepted, evaluated at
dispatch and journaled as the rendered text (determinism preserved: the
journal stores what was actually sent). Make the bootstrap prelude
overridable per conductor (`.bootstrap(text)`) for domain-specific standing
rules, mirroring `RECOMMENDED_PROMPT_PREFIX` composition.

### 10. Agent cloning and fleet hiring

**SDK:** `agent.clone(**overrides)` for cheap variants; examples fan out N
near-identical agents with one-liners.

**h5i today:** every seat is hired by hand; an N-agent arena means N
near-identical `c.agent(...)...hire()` blocks.

**Proposal:** `agent.clone(name)` (same builder config, fresh env) and
`c.hire_fleet(prefix, n, builder)` returning `Vec<Agent>`. Pure ergonomics,
~zero risk, big quality-of-life for arena/ensemble/map_reduce scores and
for `hire_n`-style Python examples.

### 11. Per-turn capability narrowing (the h5i-native `tool_choice`)

**SDK:** per-call tool restriction (`tool_choice`, `is_enabled`,
`needs_approval` per tool) shapes what an agent may do *this call*.

**h5i today:** capability is per-env (profile `tools` allowlist, isolation
tier), fixed at hire. A reviewer turn runs with the same write powers as a
work turn.

**Proposal:** turn-kind-scoped narrowing, which h5i can enforce *for real*
(the SDK only shapes the prompt-side tool list):

- review/ask turns run in a **read-only observer shell**
  (`env shell --readonly` already exists: shared `observers.lock`, ro
  `$WORK` mount), so a reviewer physically cannot edit the artifact;
- optional per-turn `tools`/egress overlay (a narrowed sub-policy of the
  seat's profile, never a widened one, fail-closed);
- per-turn model/effort override (`work(...).effort("high")`) for "cheap
  drafts, expensive final pass" scores, mirroring per-call ModelSettings.

### 12. Runtime adapter registry

**SDK:** `Model`/`ModelProvider` protocols with Responses, ChatCompletions,
LiteLLM (100+ models), any-llm; per-agent model mixing is trivial
(`models/`, `extensions/models/`).

**h5i today:** `LaunchResident` hard-codes two adapters (claude, codex) with
in-code argv/effort-flag knowledge; a third runtime means patching
`launcher.rs`.

**Proposal:** a declarative adapter registry (checked-in
`.h5i/runtimes.toml` or built-in table + overlay): argv template, effort
flag mapping, trust-prompt/onboarding quirks, HOME-state paths + API hosts
for the runtime-scoped sandbox profile (the `AgentRuntime` seams already
exist in `sandbox.rs`). Gemini CLI, opencode, aider become config, not
code. Fail-closed stays: an adapter that does not declare an effort flag
refuses `.effort()` exactly as today.

## Tier 3: polish and SDK breadth

### 13. `refine` pattern (generator + judge loop)

The SDK's LLM-as-judge example loops generator → evaluator until the grade
passes. h5i has `judge_panel` (one-shot scoring) and ensemble review cycles
(peer approval), but no score-threshold refinement loop. Add
`refine(c, task).worker(a).judge(b).threshold(8).max_rounds(3)`: work,
panel-score, feed ballots back as revise material, repeat. ~40 lines on
existing ops.

### 14. Trace exporters (OTel)

The SDK's processor pipeline feeds ~35 observability backends. h5i's journal
is the source of truth and should stay git-native, but `orch_step` events
map 1:1 onto OTel spans (`label#seq`, duration_ms, parent = scope prefix).
A `h5i team trace --otlp <endpoint>` exporter (or a hook-based live
exporter, see #7) buys Grafana/Jaeger/Langfuse dashboards without touching
the substrate.

### 15. Live DAG visualization

The SDK ships `draw_graph` for topology. h5i already renders `--dot`;
extend the embedded web dashboard (TeamView) to render the recorded DAG
live off the #4 event stream, with per-step duration/cost once #2 lands.

### 16. Python-side custom function tools for seats *(investigate)*

The SDK's `@function_tool` is its center of gravity. The h5i analog worth
exploring: let a score register host-side functions that boxed agents may
call mid-turn over the existing spool/inbox wire (`ask` in reverse:
`agent asks the score`). This gives seats controlled access to host data
(issue trackers, deploy status) without widening the sandbox. Needs a
careful trust story (host functions are the score author's code; requests
are untrusted input), hence investigate, not commit.

### 17. h5i-python breadth for policy/guard authors

Custom Python `VerdictPolicy`s and future guards (#1) will immediately want
the evidence surfaces the wrapper does not expose: `recall search`
(findings, fingerprints), `recall objects --env`, `capture run` (the
`step()` docstring already points at it with no binding), and `msg send`
for notifications. Add a minimal read-mostly `h5i.recall` /
`h5i.capture` / `h5i.msg` module set over the same bridge (new RPC methods)
or thin subprocess calls. Keep zero-dep.

### 18. Warm seat pools / cross-run rosters

The SDK's session-backend matrix (SQLite/Redis/encrypted/compaction) exists
because model calls are stateless. h5i's resident sessions *are* the memory;
the borrowable idea is lifecycle: a roster that outlives one run
(`c.agent("dev").reuse_from(prev_run)`), rehiring the same env + live tmux
session so a follow-up run starts warm. Combines with journal compaction
(M5) as the "long-lived team" story.

### 19. Standing approvals on gates

The SDK's `state.approve(always_approve=True)` remembers a decision for
repeated tool calls. h5i analog: `gate(...).remember(scope)` records an
approval in run state so an identical later gate in the same run
auto-passes with an audit event pointing at the original answer. Keeps
multi-round `--gate` runs from spamming the human.

### 20. Docs and examples program

Not a feature, but half the SDK's power is its `examples/agent_patterns/`
gallery + mkdocs site. h5i-python already has 8 strong examples; promote
the pattern gallery into rendered docs (mkdocs like the SDK, or mdBook on
the Rust side) with the comparison framing from this doc: every pattern
page shows the journal/trace it produces, which is the differentiator no
SDK example can show.

## Deliberately not borrowing

- **Hosted tools** (web search, file search, code interpreter, computer
  use): the resident runtimes bring their own tools; h5i's job is to confine
  and observe them, not to provide them.
- **In-process function-tool agents as the substrate:** an h5i agent is a
  whole CLI session in a sandbox by design; shrinking to raw model calls
  would forfeit the evidence layer that justifies the system.
- **Voice/realtime.**
- **Graph builders / YAML topology / orchestration server:** explicitly
  rejected in `orchestra-design.md` §7; the manifest stays params-only.
- **External durable-execution engines** (Temporal/Dapr/Restate): the
  journal is native durability; integrating one would demote h5i to a
  client of somebody else's source of truth.

## Suggested sequencing

1. **#7 lifecycle hooks** first: cheap, and #1/#2/#14 all attach to it.
2. **#2 usage/budgets** and **#3 typed reviews/gates**: both are known pain
   (M5 gap; `approves()` fragility) with contained blast radius.
3. **#4 streaming events**: unlocks Python ergonomics, dashboard, and #15.
4. **#1 evidence guardrails** and **#11 per-turn narrowing**: the two ideas
   where h5i does not just match the SDK but beats it, because enforcement
   and observation are real.
5. **#5 conductor-as-tools** and **#12 adapter registry**: the
   expressiveness/breadth keystones, each a design doc of its own.
