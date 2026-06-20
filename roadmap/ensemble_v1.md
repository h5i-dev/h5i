# Proposal: Auditable Workspaces for AI Agent Teams

## 1. Summary

h5i should evolve from an “auditable workspace for a single AI coding agent” into an infrastructure layer for **AI agent teams**.

The core positioning should remain:

> **Auditable workspaces for AI agent teams.**

Each AI agent gets its own isolated Git workspace. h5i records what the agent was asked to do, what it observed, what commands it ran, what files it changed, what tests it executed, and how other agents reviewed its work.

On top of this, h5i can support **agent ensemble workflows**: multiple agents independently implement the same task in separate workspaces, review each other’s patches through permissioned channels, improve their solutions over several rounds, and converge on the best final patch with a complete audit trail.

The key idea is not simply “multi-agent coding.” The key idea is:

> **Sealed workspaces, permissioned reviews, auditable convergence.**

---

## 2. Motivation

AI coding agents increasingly do more than edit files. They run commands, inspect logs, retry tests, modify environments, and make decisions that are not visible in the final Git diff.

When multiple agents are used together, the problem becomes larger:

* Which agent saw which context?
* Which commands did each agent run?
* Did one agent accidentally contaminate another agent’s solution?
* Did agents independently explore different implementations, or did they copy each other too early?
* Which review comments led to the final patch?
* Why was one implementation chosen over another?
* Can a human reviewer audit the full process after the fact?

Existing Git workflows track the final diff. They do not track the agent execution process.

h5i can fill this gap by treating each agent workspace as an auditable execution environment.

---

## 3. Core Positioning

Recommended top-level positioning:

> **h5i: Auditable workspaces for AI agent teams.**

Alternative shorter variants:

> **Auditable workspaces for AI agents.**

> **Run AI agent teams in isolated Git workspaces.**

> **Sealed workspaces. Permissioned reviews. Auditable merges.**

Recommended README headline:

```md
# h5i

Auditable workspaces for AI agent teams.

h5i gives each coding agent its own isolated Git workspace, records what it saw,
what it ran, what it changed, and how other agents reviewed it.

Run one agent safely, or run a team of agents that implement, review, improve,
and converge on the best patch with a full audit trail.
```

---

## 4. Conceptual Model

h5i should be understood as a workspace and evidence layer, not as a replacement for Claude Code, Codex, Cursor, Gemini CLI, or other agents.

The model is:

```text
h5i
= auditable workspace layer for AI agents

h5i env
= one isolated auditable workspace

h5i msg / refs
= Git-backed communication and evidence layer

h5i ensemble
= multiple auditable workspaces working on the same task
```

An agent team is composed of multiple isolated workspaces:

```text
agent team
  = agent A in workspace A
  + agent B in workspace B
  + agent C in workspace C
  + h5i-mediated messages, reviews, logs, and verdicts
```

Each workspace is an “evidence room”:

```text
workspace
  - sandboxed Git worktree
  - prompt and model metadata
  - command logs
  - test results
  - file diffs
  - policy decisions
  - review comments
  - final contribution to the merge decision
```

---

## 5. Agent Ensemble Workflow

A typical h5i ensemble workflow:

```text
1. Human creates a task.
2. h5i sends the task to multiple isolated agent workspaces.
3. Each agent independently implements a solution.
4. Each agent submits its patch, rationale, logs, and test results.
5. h5i opens a permissioned review phase.
6. Agents review each other’s patches through read-only evidence channels.
7. Agents receive review feedback and improve their own patches.
8. h5i repeats review/improvement rounds until convergence or a max round limit.
9. h5i compares the final candidates.
10. A human, policy, or judge agent selects the winning patch.
11. h5i applies the winning patch and preserves the full audit trail.
```

Example CLI:

```bash
h5i ensemble create fix-auth-bug \
  --agents claude,codex,gemini \
  --rounds 3 \
  --review all-pairs \
  --judge tests,review,human
```

Possible commands:

```bash
h5i ensemble create fix-auth --agents claude,codex
h5i ensemble prompt fix-auth "Fix the OAuth token refresh bug. Run cargo test."
h5i ensemble run fix-auth
h5i ensemble review fix-auth --all-pairs
h5i ensemble compare fix-auth
h5i ensemble apply fix-auth --winner claude
```

---

## 6. Communication Model

The communication model should be permissioned by default.

Recommended principle:

> **Humans can address any workspace. Agents cannot talk to each other unless h5i explicitly opens a channel.**

Default policy:

```text
human -> agent: allowed
agent -> human: allowed

agent -> agent: denied by default
agent -> other workspace filesystem: denied
agent -> other workspace raw logs: denied
agent -> other workspace private messages: denied
```

Allowed only through h5i-mediated channels:

```text
- review request
- patch summary
- test result
- diff
- structured feedback
- risk flag
- vote / verdict
```

This prevents accidental cross-contamination between agents and preserves independent exploration.

Without this restriction, one agent’s implementation can prematurely influence another agent’s solution. The ensemble becomes a group chat instead of a set of independent attempts. It also creates risks around secret leakage, unsafe logs, and untracked influence.

h5i should make cross-agent communication explicit, structured, and auditable.

---

## 7. Phase-Based Communication

The best design is phase-based.

```text
Phase 1: Independent Implementation
- Each agent receives the same task.
- Agents cannot see each other’s work.
- Each agent works in its own isolated workspace.

Phase 2: Sealed Submission
- Each agent submits a patch, test result, rationale, and execution summary.
- Submissions are recorded by h5i.
- Agents still cannot see each other’s work.

Phase 3: Permissioned Review
- h5i opens selected artifacts for review.
- Agent A may review Agent B’s diff and summarized evidence.
- Review is read-only.
- Raw workspace access is not allowed by default.

Phase 4: Improvement Round
- Each agent receives selected review feedback.
- Agents improve their own patches in their own workspaces.
- New evidence is recorded.

Phase 5: Judge / Merge
- h5i compares final candidates.
- Winner is selected by tests, review score, policy checks, and/or human decision.
- Winning patch is applied.
- Full audit trail remains available.
```

A useful slogan:

> **Agents work independently, communicate through evidence.**

---

## 8. Git-Backed Evidence Structure

h5i can use Git refs as the underlying coordination and evidence layer.

Possible structure:

```text
refs/h5i/ensemble/<run-id>/task
refs/h5i/ensemble/<run-id>/policy
refs/h5i/ensemble/<run-id>/agents/<agent-id>/submission
refs/h5i/ensemble/<run-id>/agents/<agent-id>/summary
refs/h5i/ensemble/<run-id>/agents/<agent-id>/logs
refs/h5i/ensemble/<run-id>/agents/<agent-id>/tests
refs/h5i/ensemble/<run-id>/reviews/<reviewer>/<target>
refs/h5i/ensemble/<run-id>/rounds/<round-id>
refs/h5i/ensemble/<run-id>/verdict
```

Each agent worker can periodically check the Git-backed task queue.

A minimal implementation can use polling. A more robust implementation can introduce an `h5i worker` process with leases, idempotent task IDs, and structured events.

Example:

```bash
h5i worker run --agent claude --env run-123-claude
h5i worker run --agent codex  --env run-123-codex
```

h5i does not need to control the agent through raw TTY manipulation. Agents can receive tasks and submit results through Git-backed refs, files, or structured h5i messages.

---

## 9. Policy Model

The ensemble should be governed by explicit policy.

Example policy:

```yaml
communication:
  human_to_agent: allow
  agent_to_human: allow

  agent_to_agent:
    default: deny
    allowed_channels:
      - review
      - patch_summary
      - test_result
      - vote

  raw_logs:
    default: deny
    allow_if: human_approved

  cross_workspace_filesystem:
    default: deny

review:
  mode: all_pairs
  artifacts:
    - diff
    - test_result
    - execution_summary
    - risk_flags
  raw_logs: summarized_only

judge:
  signals:
    - tests_passed
    - review_findings
    - diff_size
    - risk_flags
    - human_approval

stop_conditions:
  max_rounds: 3
  require_tests_pass: true
  stop_if_no_material_diff: true
  stop_if_no_high_severity_findings: true
```

Important default:

> Agents should review patches and evidence, not directly inspect or modify each other’s live workspaces.

---

## 10. Why This Is Different from Normal Multi-Agent Coding

Normal multi-agent coding frameworks often emphasize agent conversation:

```text
agents talk -> code changes -> final answer
```

h5i should emphasize auditable, isolated collaboration:

```text
agents work in sealed workspaces
-> h5i records prompts, commands, diffs, tests, logs, reviews, and policies
-> agents review each other through permissioned evidence channels
-> h5i explains why the winning patch was merged
```

The differentiation is not merely “more agents.”

The differentiation is:

```text
- isolated workspaces
- permissioned communication
- Git-backed evidence
- reviewable convergence
- auditable final merge
```

This makes h5i useful not only for productivity, but also for safety, compliance, debugging, and engineering management.

---

## 11. MVP Roadmap

### Phase 1: Ensemble Compare

Goal: compare multiple independent agent attempts.

Features:

```text
- create multiple h5i workspaces for the same task
- run agents manually or semi-automatically
- collect diffs, logs, tests, and summaries
- show side-by-side comparison
- allow human to select winner
```

Possible command:

```bash
h5i ensemble compare <run-id>
```

This phase does not require full automation.

---

### Phase 2: Permissioned Review

Goal: let agents review each other’s patches without direct workspace access.

Features:

```text
- freeze each agent’s submission
- expose only selected artifacts
- ask each agent to review another agent’s patch
- record review comments
- return selected feedback to original agents
```

Possible command:

```bash
h5i ensemble review <run-id> --all-pairs
```

---

### Phase 3: Improvement Rounds

Goal: enable iterative implementation/review/improvement loops.

Features:

```text
- round-based workflow
- review feedback routed to each agent
- agents update their own patches
- h5i records evolution across rounds
- convergence detection
```

Possible command:

```bash
h5i ensemble run <run-id> --rounds 3
```

---

### Phase 4: Worker-Based Automation

Goal: avoid manual terminal management.

Features:

```text
- h5i worker process
- task polling
- leases / locks
- retry handling
- idempotent task execution
- agent-specific adapters
```

Possible command:

```bash
h5i worker run --agent claude --env workspace-claude
```

---

### Phase 5: Policy and Governance

Goal: make agent team behavior safe and configurable.

Features:

```text
- communication policies
- artifact visibility policies
- review policies
- merge policies
- secret redaction
- risk scoring
- audit report generation
```

---

## 12. Product Language

Recommended main tagline:

> **Auditable workspaces for AI agent teams.**

Recommended subheading:

> Run multiple coding agents in isolated Git workspaces. Let them implement, review, improve, and merge the best patch with full evidence.

More technical version:

> h5i gives every agent a sealed Git worktree, controls what agents can see about each other, and records the prompts, commands, logs, tests, reviews, and decisions behind the final merge.

More catchy version:

> **Run agent tournaments in sandboxed Git workspaces.**

More safety-oriented version:

> **Sealed workspaces. Permissioned reviews. Auditable merges.**

More explanatory non-programmer version:

> h5i lets AI agents work as a team without stepping on each other. Each agent gets its own workspace, and every action, review, and final decision is recorded.

---

## 13. Key Design Principles

1. **Workspace is the core primitive.**
   Keep “auditable workspace” as the main concept. Agent teams are built from multiple workspaces.

2. **Agents should be isolated by default.**
   Each agent should work in its own sandboxed Git worktree.

3. **Cross-agent communication should be permissioned.**
   Humans can message any workspace, but agents should not freely talk to each other.

4. **Review should happen through evidence, not live access.**
   Agents review diffs, summaries, tests, logs, and risk flags.

5. **The final merge should be explainable.**
   h5i should record why one patch won over another.

6. **h5i should orchestrate evidence, not replace agents.**
   Claude Code, Codex, Gemini CLI, Cursor, and other tools remain the agents. h5i provides the auditable coordination layer.

7. **Start with human-in-the-loop.**
   The first version can let humans choose the winner. Full automatic judging can come later.

---

## 14. Final Recommendation

h5i should keep its core identity as:

> **Auditable workspaces for AI agents.**

But the stronger, more future-facing version is:

> **Auditable workspaces for AI agent teams.**

The “agent team” framing is easier to understand than “ensemble” for non-programmers, while still allowing the product to support ensemble-style workflows internally.

Recommended architecture:

```text
single agent:
  h5i env = one auditable workspace

multiple agents:
  h5i ensemble = many sealed workspaces + permissioned reviews + auditable convergence
```

Recommended product message:

```text
h5i gives each AI coding agent its own isolated workspace.

Agents can independently implement the same task, review each other’s patches
through permissioned evidence channels, improve over multiple rounds, and merge
the best result with a complete audit trail.
```

In short:

> **h5i should become the auditable coordination layer for AI agent teams.**

