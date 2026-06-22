## TUI-First Product Direction

Although h5i can expose lower-level CLI commands for scripting, the primary user experience for AI agent teams should be a **TUI** rather than a long `h5i ensemble ...` command interface.

The core workflow is inherently visual and supervisory:

* multiple agents working in parallel
* each agent isolated in its own workspace
* different task states across agents
* pending review requests
* permissioned communication approvals
* test results and risk flags
* candidate patches
* final winner selection

This is difficult to understand from CLI output alone. A TUI makes the product feel like an **agent team control room**.

Recommended command:

```bash
h5i team
```

or:

```bash
h5i board
```

The TUI should become the main surface where humans coordinate AI agent teams.

---

## Agent Team Control Room

The TUI should show each agent workspace as a separate lane.

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ h5i Team Room: fix-auth-bug                                                  │
├───────────────┬───────────────┬───────────────┬──────────────────────────────┤
│ Claude        │ Codex         │ Gemini        │ Human / Judge                 │
│ workspace A   │ workspace B   │ workspace C   │                              │
├───────────────┼───────────────┼───────────────┼──────────────────────────────┤
│ Implementing  │ Tests failed   │ Ready review  │ Round 1 / 3                  │
│ 3 files       │ 1 failing test │ 5 files       │                              │
│ cargo test... │ needs fix      │ submitted     │                              │
├───────────────┴───────────────┴───────────────┴──────────────────────────────┤
│ Pending actions:                                                             │
│  [1] Send instruction to all agents                                           │
│  [2] Open review phase                                                        │
│  [3] Approve Claude -> Codex review access                                    │
│  [4] Compare candidate patches                                                │
│  [5] Apply winning patch                                                      │
└──────────────────────────────────────────────────────────────────────────────┘
```

The user should be able to:

```text
- send a task to all agents
- send a private instruction to one workspace
- inspect each agent’s diff
- inspect test results and command logs
- approve or deny cross-agent review
- open the next review/improvement round
- compare candidates side by side
- select or approve the final patch
```

This makes h5i understandable even to users who do not think in terms of Git refs, worktrees, or orchestration commands.

---

## TUI as the Human-in-the-Loop Layer

The TUI should reinforce h5i’s main philosophy:

> Humans can supervise every workspace.
> Agents cannot freely communicate unless h5i opens an audited channel.

In the TUI, cross-agent communication should appear as explicit permission requests.

Example:

```text
Claude requests access to review Codex's patch.

Artifacts requested:
  - diff
  - test result
  - execution summary
  - risk flags

Not included:
  - raw logs
  - private human messages
  - Codex workspace filesystem

Approve? [y/N]
```

This makes permissioned communication visible and easy to understand.

Instead of saying:

```text
agent-to-agent communication is mediated by refs/h5i/ensemble/...
```

the product can say:

> Claude wants to review Codex’s patch. Approve?

This is much more intuitive.

---

## CLI vs TUI

h5i should still provide CLI primitives for automation, CI, and power users.

However, the product should be designed as:

```text
TUI first
CLI second
Git refs underneath
```

Recommended layering:

```text
h5i team / h5i board
  = main human-facing control room

h5i team create / h5i team run / h5i team compare
  = scriptable CLI interface

refs/h5i/team/<run-id>/...
  = underlying Git-backed evidence and coordination layer
```

The CLI should not be the main explanation of the product. It should be the automation layer behind the TUI.

---

## Recommended Naming

Avoid making `ensemble` the primary user-facing command.

`ensemble` is technically accurate, but it sounds academic and is less obvious to non-programmers.

Better user-facing names:

```text
h5i team
h5i board
h5i room
h5i cockpit
h5i studio
```

Recommended default:

```bash
h5i team
```

Why:

* immediately understandable
* matches “AI agent teams”
* works for both technical and non-technical users
* does not overemphasize the implementation detail
* naturally supports single-agent and multi-agent workflows

Possible tagline:

> **h5i team: a terminal control room for AI agent workspaces.**

Alternative:

> **A TUI for supervising AI agent teams in isolated, auditable workspaces.**

---

## Revised Product Message

Recommended product message:

```text
h5i gives every AI coding agent its own isolated workspace.

Open the h5i Team Room to send tasks, supervise progress, approve cross-agent
reviews, compare patches, and merge the best result with a complete audit trail.
```

Shorter version:

```text
Auditable workspaces for AI agent teams.

A terminal control room where agents work independently, review through
permissioned evidence channels, and converge on the best patch.
```

---

## Revised Workflow

Instead of centering the workflow around:

```bash
h5i ensemble create ...
h5i ensemble run ...
h5i ensemble review ...
h5i ensemble compare ...
```

the main workflow should be:

```bash
h5i team
```

Inside the TUI:

```text
1. Create task
2. Choose agents
3. Launch isolated workspaces
4. Send task to all agents
5. Watch implementation progress
6. Freeze submissions
7. Open permissioned review phase
8. Approve cross-agent review channels
9. Run improvement rounds
10. Compare candidates
11. Select winner
12. Apply patch
13. Export audit report / PR brief
```

The command line can still support equivalent actions:

```bash
h5i team create fix-auth --agents claude,codex,gemini
h5i team run fix-auth
h5i team review fix-auth --all-pairs
h5i team compare fix-auth
h5i team apply fix-auth --winner claude
```

But these should be described as automation commands, not the main product experience.

---

## MVP Roadmap: TUI-First

### Phase 1: Team Room Viewer

Goal: visualize multiple workspaces.

Features:

```text
- list active agent workspaces
- show each workspace status
- show current branch / worktree path
- show latest command
- show changed files
- show test status
- show last h5i event
```

This can work even before full automation exists.

---

### Phase 2: Human Broadcast and Direct Messages

Goal: let the human send instructions from one place.

Features:

```text
- send task to all workspaces
- send instruction to one workspace
- record every message in h5i refs
- show message history per workspace
```

This makes h5i feel like a real coordination layer.

---

### Phase 3: Submission and Compare View

Goal: compare independent agent attempts.

Features:

```text
- freeze each workspace submission
- show side-by-side diffs
- show tests and logs
- show risk flags
- let human select candidate patch
```

This is likely the first strong demo.

---

### Phase 4: Permissioned Review Flow

Goal: make cross-agent review explicit.

Features:

```text
- request review from one agent to another
- show approval prompt before sharing artifacts
- expose only selected evidence
- record review comments
- route feedback back to original workspace
```

This is the key h5i-specific differentiator.

---

### Phase 5: Automated Agent Team Runs

Goal: support full multi-agent loops.

Features:

```text
- h5i worker process
- task polling
- leases and locks
- round-based execution
- automatic review assignment
- convergence detection
- judge / human approval flow
```

Automation should come after the TUI makes the workflow understandable.

---

## Design Principle

The TUI should make h5i feel less like a command runner and more like a control room.

The core product should not be:

> Run this complicated multi-agent command.

It should be:

> Open h5i, see your AI agent team, and control what happens next.

In short:

> **h5i should be the terminal control room for auditable AI agent teams.**

