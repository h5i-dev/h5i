# Design Overview: `h5i team` — Auditable Agent Ensembles

> Status: design overview (v3). Supersedes `ensemble_v1.md` (CLI/refs sketch) and
> `ensemble_v2.md` (TUI-first). Consolidates a survey of the existing `env`,
> `serve`, `msg`, and `objects` code paths with four rounds of i5h review from
> Codex (#37f12392, #65897b5d, #80c46ba6, #3033bc37 — persona model). The §4a
> communication + finalization policy layer is self-reviewed; a 5th Codex pass
> (#ed97ae41) is pending its availability.
>
> One sentence: **`h5i team` is a deterministic, Git-backed evidence-publication
> workflow over existing envs — not an agent-orchestration daemon, and not a chat
> room.**

## 1. Problem & non-goals

When several coding agents attack one task, Git tracks only the final diff. It
does not track *which agent saw what*, what each ran, whether one contaminated
another, which review led to the merge, or why one candidate won. `h5i team`
fills that gap by treating each agent's run as an isolated, auditable env and
adding a phased, permissioned protocol to publish and compare evidence across
them. ("Sealed" is reserved for the *sealed submission* and for envs whose
`probe` actually supports a hard sandbox seal — see §6; at workspace tier the
isolation is workflow/interface isolation, not a kernel seal.)

**Design stance: auditable convergence, minimal human labor.** The default flow
keeps agents independent and produces an explained verdict *without requiring a
human to review anything*. Two run-level policies (§4a) let the user dial this:
a **communication policy** (sealed by default; opt-in agent discussion) and a
**finalization policy** (who/what picks the winner — a rule, a vote, a judge
agent, or a human). Whatever is chosen, the convergence stays **auditable**:
every message, metric, and decision is a recorded, explainable event.

**Non-goals (explicit):**

- Not *uncontrolled* agent chat. Group chat is **off by default** (it erodes
  independent exploration); when a user turns on discussion it is an explicit,
  fully-logged mode, and candidates produced under it are marked non-independent.
- Not *opaque* judging. Automated finalization is supported and is the point
  (humans shouldn't have to review) — but every verdict, human or machine,
  records its method + evidence + rationale. The non-goal is an *unexplained*
  decision, not an *automated* one.
- Not a new sandbox, runner, message bus, or artifact store. It orchestrates
  the ones we have (`env`, `msg`, `objects/capture`, `serve`).
- Not a required daemon. Coordination state is in Git refs, discovered by
  polling. Workers/leases are P4, optional.
- Not a TUI. The rich UX lives in `h5i serve` (browser); CLI is the automation
  surface with a small `status --watch` for live monitoring.

## 2. Concepts & invariants

**Naming:** user-facing command is **`h5i team`**. The internal module / ref
namespace may be `team`; "ensemble" is the workflow noun, never a second
first-class command.

**Conceptual model:**

```
h5i env   = one isolated, auditable workspace (exists today; tier-dependent)
h5i team  = N envs working the SAME task + a phased, permissioned
            evidence-exchange protocol over them

TeamRun = grouping of envs + phase policy + published artifacts + verdict
```

`env` keeps owning worktree isolation, sandbox tier, command capture, and
propose/apply mechanics. `team` owns *coordination only*: roster, phases,
artifact grants, compare, verdict. No second execution stack.

**A roster member is a persona-bound env identity.** `agent_id` is the durable
**actor**; `runtime`/`model` are execution **adapters**; `persona_digest` freezes
the resolved instruction bundle that produced a submission. A team agent is thus
`(runtime, model, persona)` where *persona* = a system prompt and/or skill set +
a role label. This is first-class: a roster can be **three Claudes with different
system prompts** (`architect`, `implementer`, `skeptic`), a Claude+Codex mix, or
one model under two skills. It sharpens provenance: the audit records *which
configuration produced which candidate*, so "the skeptic's patch beat the
implementer's" is an auditable fact. Persona is treated as **immutable run
input, never an execution authority.** Implications:

- **Actor identity, not vendor identity.** `agent_id` is the actor key for every
  team event; `runtime`/`model` are attributes. Three Claudes are three distinct
  event actors and env identities. Dispatch targets the persona `agent_id`, never
  bare `claude`/`codex` — otherwise ACK/DONE attribution collapses.
- **Naming.** Distinct personas on the same runtime get distinct `agent_id`s and
  env slugs (`env/claude-architect/<run>`, `env/claude-skeptic/<run>`) — reuse
  the existing env agent/slug validation; never derive ref/path names from
  display labels.
- **Independence applies to persona text too.** A persona prompt is task input:
  before `review` it must not contain peer outputs, summaries, branches, or prior
  candidate facts. If a human edits a persona between rounds using review
  feedback, that is a **new `persona_digest` for the next round** — recorded as an
  *improvement-round* input, not as the original independent attempt.
- **Local isolation only.** Per-env `home/` copies stop N same-runtime boxes from
  racing the shared `~/.claude`/`~/.codex` session+token files, and the
  runtime-scoped `agent-claude`/`agent-codex` profile still applies per member.
  But the API **account/session may still be shared provider-side** — claim only
  local workspace/home/sandbox isolation, *not* independent vendor accounts
  (unless separately configured).
- **Persona application** is recorded as provenance and materialized at dispatch
  (P1), **fail-closed** — see §5.

**Invariants (non-negotiable):**

1. **Independence (default; policy-relaxable).** Under the default sealed
   communication policy, before the `review` phase an agent cannot read another
   agent's diff, summary, logs, worktree path, private instructions, or branch
   tip *through the team interface*. (Envs already don't read each other.) If the
   user enables a discussion policy (§4a), this is relaxed **only** within the
   declared discussion scope, every exchange is logged, and affected candidates
   are stamped `independent=false` so the audit never mistakes a discussed patch
   for an independent attempt.
2. **Publication.** A peer artifact becomes visible only via an explicit phase
   transition or grant **event recorded in the team log** — never implicitly.
3. **Host-mediated evidence.** Boxed agents must not mutate team coordination
   refs. `env` already withholds `refs/h5i/env`, claims, msg, and on-disk h5i
   stores from the box; preserve that. `team submit/review/grant/apply` are
   host-side commands (or trusted wrappers), not in-box operations.
4. **Immutability.** A submission points at **frozen** evidence — commit oid,
   tree oid, diff oid, capture ids, summary object — not "current env HEAD." If
   the env mutates afterward, the submitted candidate stays reviewable exactly
   as submitted.
5. **Apply.** Applying a winner **replays the recorded submitted patch/commit
   into a fresh controlled target**, never trusting a still-mutating worktree.
6. **Audit.** Every transition records actor, timestamp, phase-before,
   phase-after, reason, and affected artifacts. Human overrides are first-class
   events, not prose comments.
7. **Explainable verdict.** Whether the winner is chosen by a human, a rule, a
   vote, or a judge agent, the `verdict` event records the **method, the inputs
   it consumed (metric values / ballots / judge rationale), and the losers'
   reasons**. An automated decision is allowed; an unexplained one is not. The
   `actor` of an auto-verdict is the policy/agent that decided, never a human who
   didn't.

## 3. State machine

```
draft ─► dispatched ─► independent_work ─► sealed_submit ─► review ─┐
                                              │ (discuss policy)    │
                                              ▼                     │
   applied | closed ◄─ verdict ◄─ compare ◄─ discuss? ◄─ improve ◄──┘
                          ▲                              (loop ≤ max_rounds)
                          └─ finalization policy: rule | vote | judge | human
```

- Transitions are **monotonic** except a documented `reopen` event; reopen must
  invalidate / supersede affected grants and submissions.
- Illegal transitions are rejected. An admin override is allowed but recorded as
  an explicit event (per invariant 6).
- A phase transition is what *freezes and publishes* the relevant artifacts.
  Until `sealed_submit` completes, each env is private.
- `discuss` is **optional**, gated by the communication policy (§4a). It only
  ever runs *after* `sealed_submit` (so the first attempt is always independent),
  is fully logged, and stamps any subsequent candidate `independent=false`.
- `verdict` may be reached **without a human** per the finalization policy (§4a);
  the transition still records method + evidence (invariant 7).

| Phase | What happens | Backed by |
|---|---|---|
| draft | run created, roster + policy fixed | `team create` + `team add-env` |
| dispatched | task sent to each agent (no execution implied) | `team dispatch` → i5h per agent |
| independent_work | agents edit in their own env | `env run` / `env shell` (captures accrue) |
| sealed_submit | each agent **freezes** an immutable candidate | `team submit` (snapshot oids + captures) |
| review | reviewers see *granted* artifacts only | grants + i5h `REVIEW_REQUEST` |
| discuss *(opt-in)* | agents exchange messages within declared scope | i5h thread scoped to run; every msg logged |
| improve | feedback routed back; agents revise; new round | i5h replies → new `env run` cycle |
| compare | candidates shown side by side + metrics gathered | `team compare` → `env::compare` + findings |
| verdict | winner chosen by policy (rule/vote/judge/human) | `team finalize` → recorded verdict event |
| applied | winner *replayed* into target, provenance stamped | `team apply` → fresh merge of submission |

## 4. Ref & storage model

Mirror the proven CAS-append pattern from `msg.rs` (`append_message_cas`,
`MAX_ATTEMPTS=64`) and `objects.rs` (`append_manifest`): union-merge, dedupe by
event id / idempotency key, deterministic sort (causal parent → ts → id).

**One ref per run** (not one global ref) to reduce write contention across
independent runs and keep phase reconstruction local:

```
refs/h5i/team/<run-id>            # the run's event log = source of truth
  events.jsonl                    # append-only: lifecycle, phase transitions,
                                  # grants, submissions, reviews, verdicts
runs enumerated by listing refs/h5i/team/*  (no global mutable index)
```

The event log is the source of truth; everything else is derived. **Only events
are stored.** `phase`, `current_round`, `verdict`, and each agent's `state` are
**folded from the event stream** (optionally cached as a materialized view tagged
with its source event id) — they are *not* authoritative mutable fields. Storing
them as mutable state would let reopen/override/union-merge leave stale values.
The structs below are therefore the **projected API shape**; on disk, the
mutable-looking fields are computed.

**Bulky artifacts never live in team refs** — diffs/logs/test output are stored as h5i
`objects`/captures; the team log carries **pointers + digests** only. Per-agent
artifact refs are acceptable *only if immutable / content-addressed*; mutable
per-agent state refs are avoided (they make phase + grant auditing harder).

```rust
struct TeamRun {
    id: String,                 // validated ref/path-safe slug (NOT a display name)
    name: String,
    base_oid: String,           // shared base across agents (apples-to-apples)
    created_by: String, created_at: String,
    phase: String, max_rounds: u32, current_round: u32,
    policy: TeamPolicy,         // sealed default, grantable artifact kinds
    roster: Vec<TeamAgent>,
    verdict: Option<Verdict>,
}
struct TeamAgent {
    agent_id: String,           // validated ref/path-safe key, e.g. "claude-architect"
    display_label: String,      // human label; NOT a ref name
    env_id: String,             // env/<agent_id>/<slug>
    // persona = the part that makes two same-runtime members different:
    runtime: String,            // claude | codex
    model: Option<String>,      // e.g. claude-opus-4-8
    role: Option<String>,       // architect | implementer | skeptic | ...
    persona: PersonaSpec,       // RESOLVED + frozen at add-env time (not paths)
    persona_digest: String,     // sha256(canonical JSON, sorted keys, version field) over the
                                // RESOLVED bundle: runtime, model, role, prompt BYTES, skill ids
                                // + content digests/versions, tool/profile selection, persona env
                                // knobs. Hash bytes, never a path. Reproducible == same digest.
    isolation_claim: String, policy_digest: String,
    state: String,              // working|submitted|reviewing|revised (DERIVED from events)
    branch_ref: String, worktree_known_local: bool,
    latest_submission_id: Option<String>,
}
struct PersonaSpec {            // RESOLVED bundle (frozen at add-env), not references
    system_prompt: Option<String>,   // or system_prompt_ref → objects (large prompts)
    append_system_prompt: Option<String>,
    skills: Vec<SkillPin>,           // skill names alone are NOT reproducible
    tool_profile: Option<String>,
}
struct SkillPin {               // skills are code/instructions — pin content, not just a name
    name: String, repo_commit: Option<String>, path: Option<String>,
    blob_oid: Option<String>, content_digest: String,
}
struct TeamEvent {              // append-only, the audit spine; the ONLY stored record
    id: String, ts: String, actor: String,
    parent_event_id: Option<String>, // causal anchor where order matters (phase races)
    kind: String,               // dispatched|submitted|granted|reviewed|verdict|reopen|override|...
    run_id: String, round: u32,
    phase_before: Option<String>, phase_after: Option<String>,
    payload: Json, idempotency_key: String,
}
struct TeamArtifact {           // immutable candidate bundle
    id: String, kind: String,   // diff|summary|tests|risk
    owner_agent: String, round: u32,
    persona_digest: String,     // which configuration produced this candidate
    persona_event_id: String,   // WHERE in the timeline that config entered (digest proves
                                // content equality; this anchors it across reopen/override/update)
    commit_oid: String, tree_oid: String,
    capture_ids: Vec<String>, diff_stat: DiffStat,
    metrics: Map<String, f64>,  // finalization inputs gathered at compare (tests/loc/risk/bench)
    independent: bool,          // false if produced after a discuss phase (provenance honesty)
    visibility: String, digest: String,
}
struct Grant {                  // unit of cross-agent visibility
    reviewer: String, target: String, round: u32,
    artifact_kinds: Vec<String>,// default diff+summary+tests; NEVER raw capture bodies
    artifact_ids: Vec<String>,
    phase_bound: String, granted_by: String,
}
struct Review { reviewer: String, target: String, round: u32,
    findings: Vec<String>, risks: Vec<String>, suggested_changes: Vec<String>,
    referenced_artifacts: Vec<String> }
struct Verdict {                // explains itself whether human OR machine decided (invariant 7)
    selected_submission: Option<String>, // None == no_verdict (no candidate cleared the rule)
    method: String,             // "rule:tests,smallest-diff" | "vote" | "judge:<id>" | "human"
    decided_by: String,         // the policy/judge/human actor that decided
    metric_values: Map<String, Map<String, f64>>, // per-candidate inputs the decision used
    ballots: Vec<Ballot>,       // for vote/judge: voter, choice, rationale
    rejected: Vec<(String, String)>, // (submission, reason) for every loser
    human_approved_by: Option<String>, // only set under FinalizationPolicy::Human
}
```

**Ordering & merge determinism.** Events sort by `parent_event_id` causal chain
→ `ts` → `id` (wall clock + id alone is insufficient for phase-transition races
after union-merge). The fold enforces legal transitions; when union-merge yields
**ambiguous concurrent decisions** (e.g. two clones each emit a `verdict`), the
fold marks the run **conflicted** and requires a human `override` event to
resolve — it does not silently pick a winner.

**Run enumeration is defensive.** Runs are discovered by listing refs under
`refs/h5i/team/` and validating each refname segment; malformed refs are ignored,
never panicked on. There is no global mutable index to contend on.

**Union-merge for team events is required before any remote-sharing claim** —
same class of problem already solved for `msg` and `objects`. `h5i share
push/pull` then carries a whole run to another clone (enables the
propose-on-clone-A / review-on-clone-B loop).

## 4a. Run policy: communication & finalization

`TeamPolicy` is fixed at `create` and is what makes a run hands-off. Two knobs
matter most; both default safe and both keep the audit complete.

```rust
struct TeamPolicy {
    grantable_artifacts: Vec<String>,   // diff,summary,tests (never raw bodies); §6
    communication: CommunicationPolicy,
    finalization: FinalizationPolicy,
}

enum CommunicationPolicy {
    Sealed,                              // DEFAULT: no agent↔agent until review grants
    Discuss {                           // opt-in: agents may talk, AFTER sealed_submit
        scope: DiscussScope,            // FreeForAll | Pairs(reviewer→target) | Moderated(judge)
        max_messages: u32,              // bounded so it can't loop forever (hands-off)
        share_artifacts: Vec<String>,   // what discussion may reference (diff/summary/tests)
    },
}

enum FinalizationPolicy {               // who/what picks the winner — minimize human labor
    Human,                              // explicit approval (opt-in now, not the default)
    Rule(Vec<Metric>),                  // deterministic ordering, e.g. [TestsPass, FewestLoc]
    Vote { electorate: Electorate, tie_break: Box<FinalizationPolicy> },
    Judge { agent_id: String, rubric: Option<String>, tie_break: Box<FinalizationPolicy> },
}

enum Metric {                           // rule inputs, all from EXISTING evidence
    TestsPass,                          // from a tests capture (pass/fail, count)
    FewestLoc,                          // from diff_stat
    SmallestDiff,                       // files + churn from diff_stat
    LowestRisk,                         // from risk.rs classification of the env captures
    FastestBench(String),               // a named benchmark capture's metric
    Custom(String),                     // a user expression over the above
}
enum Electorate { AllAgents, AllExcept(String), Named(Vec<String>) }
```

**Recommended hands-off default:** `communication = Sealed`,
`finalization = Rule([TestsPass, SmallestDiff])` — candidates that pass tests
win, ties broken by the smallest change, **no human in the loop**, and the
verdict event still spells out the metric values. The user overrides per run
(`--finalize vote`, `--finalize judge:claude-skeptic`, `--finalize human`,
`--discuss pairs --max-messages 8`).

**Guards that keep automation honest** (so "minimal labor" ≠ "ungoverned"):

- **Metrics come only from recorded evidence** (captures / diff_stat / risk), so
  a verdict is reproducible from the audit trail, not from live state.
- **Gaming defense:** rule metrics are adversarial-aware — `FewestLoc`/`SmallestDiff`
  alone are gameable (delete tests, stub features), so they are only ever
  *tie-breakers after* `TestsPass` (and optionally a risk-floor). A judge rubric
  should be published in the verdict for inspection.
- **Vote integrity:** under a `Discuss` policy, votes can collude — a voter may
  not vote for a candidate it co-authored or discussed into; record each ballot
  + rationale; a tie falls through to the declared `tie_break`.
- **Judge conflict of interest:** a `Judge` finalizer must **not** be a roster
  contender. If the desired judge persona is also competing, either run it as a
  separate non-contending roster member (its own `agent_id`) or exclude its own
  candidate from its decision; the verdict records the judge's runtime/model so a
  same-runtime bias (judge favoring fellow Claude candidates) is auditable.
- **No-winner is a valid outcome:** if no candidate clears the rule (e.g. none
  pass tests), the run records `no_verdict` and notifies — it does **not** apply
  a loser. This is the one place a human may be pinged, by choice.
- **Bounded discussion:** `max_messages` and a per-run discussion deadline keep a
  hands-off run from looping; exhaustion advances to `compare` automatically.

## 5. CLI MVP & serve views

CLI is the boring, scriptable automation layer — **the headline UX is serve.**
Each command maps onto an existing primitive; team adds no new execution.

```bash
# P0 — manual ensemble over EXISTING envs (no agent automation)
h5i team create <name> --base HEAD [--rounds 1]
      [--finalize rule:tests,smallest-diff | vote | judge:<agent-id> | human]
      [--discuss off | pairs | free | moderated:<judge>] [--max-messages N]
      # default: --finalize rule:tests,smallest-diff --discuss off  (hands-off, no human)
h5i team add-env <team> <env> --as <agent-id>     # group already-created envs; agent-id is the
      [--runtime claude|codex] [--model M]        # ref-safe persona key (claude-architect, ...)
      [--role architect] [--skill code-review,...] # persona recorded as provenance (persona_digest)
      [--system-prompt-file F]
h5i team status <team> [--json] [--watch]
h5i team submit <team> --agent <id> [--commit OID] [--tests-capture ID] [--summary-file F]
h5i team freeze <team> [--allow-missing]          # → sealed_submit; REFUSES if any roster
                                                  # member lacks a submission, unless
                                                  # --allow-missing records abstentions/timeouts
h5i team compare <team> [--json]                  # candidates side by side + gathered metrics;
                                                  # advisory only — does NOT pick the winner
h5i team finalize <team> [--dry-run]              # apply the run's finalization policy → verdict
                                                  # event (method + metric values/ballots/rationale +
                                                  # losers' reasons). --dry-run shows what would win.
                                                  # no candidate clears the rule → records no_verdict

# P1 — dispatch + grants
h5i team dispatch <team> --prompt-file F          # i5h sends; receipt/progress counts ONLY via
                                                  # ACK/DONE threaded to the dispatch id
                                                  # (unthreaded msgs shown but never advance state)
                                                  # applies each member's persona at launch:
                                                  #   claude → --append-system-prompt + skills
                                                  #   codex  → profile/system-prompt config
                                                  # FAIL-CLOSED: if a runtime can't apply the
                                                  # persona exactly (missing skill / unsupported
                                                  # append / profile not found) → refuse that member
                                                  # or record a `blocked` event; never silently
                                                  # degrade to a generic run.
                                                  # P0: persona is recorded only (human launches)
h5i team grant-review <team> --reviewer A --target B --artifacts diff,summary,tests
h5i team review submit <team> --reviewer A --target B --file F

# P2 — opt-in discussion + automated finalization
h5i team discuss <team> [--scope pairs|free|moderated] [--max-messages N]
                                                  # open the logged discussion phase (post-submit);
                                                  # routes i5h within the run; stamps later
                                                  # candidates independent=false; bounded by N
# P2 — rounds + state-machine enforcement (serve permission/verdict views)
# P3 — apply winner
h5i team apply <team> --winner <submission-id>    # replay recorded patch into fresh target;
                                                  # records submission id + resulting target commit oid;
                                                  # on conflict records a conflict event, never mutates
                                                  # the winning artifact + audit report
```

| team command | reuses |
|---|---|
| add-env / create | `env::find` / manifests; write `TeamRun` |
| submit / freeze | snapshot env commit/tree/diff + capture ids → immutable `TeamArtifact` |
| dispatch | `msg::send` (kind ASK/handoff) + `dispatched` event |
| grant-review | `Grant` event + scoped `msg::send` (REVIEW_REQUEST) referencing artifact ids only |
| compare | `env::compare(run.env_ids)` arena + join review findings/risk + gather `Metric`s |
| discuss | `msg::send` within a run-scoped i5h thread; every msg → `TeamEvent` + capture |
| finalize | evaluate `FinalizationPolicy` over recorded metrics/ballots → `verdict` event |
| apply | replay submission into fresh target (3-way), then `env`-style provenance note |

**serve** is already axum JSON API + React SPA (`web/`) with a `Mode` switcher
in `Workbench.tsx`. Add a **Team** mode + handlers in `server.rs` /
`build_router()` (standard `spawn_blocking(H5iRepository::open)` pattern):

```
GET /api/teams                 -> list (phase, agent count, risk roll-up)
GET /api/team/:id              -> board (lanes) + roster + verdict
GET /api/team/:id/compare      -> candidate diffs/tests/findings/risk/score
GET /api/team/:id/timeline     -> per-agent OBSERVE/THINK/ACT/capture by phase
```

Reuse env detail/replay/captures APIs; the team API just *joins* env summaries
with team artifacts. Client: `web/src/TeamView.tsx`, a `Mode = "team"` branch,
`api.team*()` in `api.ts`, a nav button — exactly the wiring every mode uses.
Web views (not TUI clones): **Board, Compare, Timeline, Permissions, Verdict.**
Each board lane is labeled by **persona** (role + runtime/model badge, e.g.
"architect · claude-opus-4-8"), not just the backend — so a roster of three
Claudes reads as three distinct contenders; Compare/Verdict link the winning
candidate back to its `persona_digest`.

## 6. Security & privacy model

```
human -> agent : allow        agent -> human : allow
agent -> agent : deny by DEFAULT — allowed only under a Discuss policy, post-submit,
                 within declared scope, fully logged (candidates → independent=false)
peer artifacts : deny until an explicit, logged Grant exists
raw logs / private human msgs / peer worktree / peer branch tip : deny (no MVP grant path)
```

- **Discussion is opt-in and bounded.** When `communication = Discuss`, agent↔agent
  routes through a run-scoped i5h thread *after* `sealed_submit` (so the first
  attempt is always independent), every message is a logged `TeamEvent` + capture,
  it is capped by `max_messages`/deadline, and any candidate revised afterward is
  stamped `independent=false`. Discussion may reference only the policy's
  `share_artifacts` — never raw bodies or peer worktrees.
- **Automation is governed, not blind.** A machine `verdict` consumes only
  recorded metrics/ballots and writes its full reasoning (invariant 7); a tie or
  no-clear-winner falls through to the declared tie-break or `no_verdict` rather
  than applying a loser. Gameable metrics (`FewestLoc`/`SmallestDiff`) are only
  tie-breakers behind `TestsPass`.
- **Default grant = diff + summary + test *status*** — never raw capture bodies
  (captures may carry secrets). Raw-log sharing has no MVP path.
- **Every lane shows its env's *actual* isolation tier** from `env probe`. A
  workspace-tier env is labeled workspace-tier; never visually imply sealed
  sandboxing the probe doesn't support. The team layer adds **no** new security
  guarantees beyond what each env provides.
- **Review bundles are passed by artifact id**, materialized host-side — never
  by peer worktree path.
- **Grants are phase-bound *and* artifact-bound.** A grant for round-1 Codex diff
  must not auto-reveal round-2 Codex diff; each round/artifact needs its own
  grant event. Reopen supersedes affected grants (see §7).
- **Persona bodies are not granted to peers by default.** A reviewer needs the
  diff/summary/test status, not a peer's hidden system prompt — the persona text
  is itself a leakage channel. serve shows role/runtime/model + `persona_digest`;
  the full persona body is a human/admin evidence view, granted only explicitly.
- **Validate run id / agent id / artifact id** for ref+path safety exactly as
  `env` validates agent/slug. **Display names are not ref names.**

## 7. Failure modes & recovery

- **Ref contention.** Many agents → CAS retries on one ref. One-ref-per-run
  bounds the blast radius; idempotency keys make event appends safe to retry.
- **Mutable-HEAD ambiguity.** Compare must never read a live env HEAD as a
  candidate — only a frozen submission (invariant 4). `submit` snapshots first.
- **Concurrent apply.** Winner application happens in one controlled workspace
  by replaying the recorded submission (invariant 5), respecting the per-env
  `run.lock` busy pattern when snapshotting (as review/apply already do).
- **Remote clones.** Env worktrees are local; submitted artifacts must remain
  reviewable without the originating worktree. `team status` distinguishes
  **"artifact available"** from **"workspace local."**
- **Logs/secrets in refs.** Forbidden — pointers + digests only; bodies stay in
  `objects`/captures. Note: captures preserve **raw recoverability** — secret
  scanning / risk classification can *flag* sensitive content but does **not**
  make raw sharing safe. Therefore team grants must not expose raw capture
  bodies by default (see §6).
- **GC / retention.** A `TeamRun` needs explicit `close`/`archive` and a stated
  retention story for envs/worktrees/captures *before* we call it auditable.
  `team gc` fans out to `env::gc`; closing a run is a recorded event, not a
  deletion of the audit trail. Watch disk: N envs ⇒ N worktrees + N per-env
  `home/` credential copies — document a roster-size guardrail.
- **Reopen.** A reopened phase must supersede affected grants/submissions with
  explicit events so the timeline stays reconstructable.

## 8. Roadmap

- **P0 — manual ensemble + compare.** `create / add-env / status / submit /
  freeze / compare` over **existing** envs, no automation. `compare` reuses the
  env arena and powers a serve Team board + compare view. First strong demo.
- **P1 — dispatch + grants + board.** `dispatch` (i5h), `grant-review` /
  `review submit`, serve board + timeline + permission view.
- **P2 — rounds + hands-off finalization.** Improvement loop, state-machine
  enforcement, convergence/stop conditions (`max_rounds`, no-material-diff,
  tests-pass); `team finalize` with `Rule` metrics (the no-human default) + the
  opt-in `team discuss` phase; serve verdict + permission views. **This is the
  slice that delivers "minimal human labor."**
- **P3 — apply + audit.** `team apply` (replay submission, conflict runbook) +
  exported PR/audit brief — the "merged with proof" headline. Adds `vote` /
  `judge` finalization on top of `Rule`.
- **P4 — optional automation.** Only on real demand: `h5i worker` with
  leases/idempotent task ids polling refs, so a run can go
  dispatch→submit→finalize→apply end-to-end untouched. No leases in P0; no
  required daemon.

## 9. Headline

> **`h5i team` runs isolated agent workspaces through phased evidence exchange,
> then converges on a winner by a user-chosen, fully-auditable policy — a rule, a
> vote, a judge agent, or a human — so the team ships with proof and the human
> barely lifts a finger.**
