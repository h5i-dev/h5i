# Borrowing from docker-agent to improve `h5i`

> Working notes. Ideas mined from `../sandbox/docker-agent` (Docker's
> **docker-agent**, the `docker agent` CLI plugin, internally still `cagent`:
> a Go multi-agent runtime, ~150 `pkg/` subpackages) for `h5i env`, the
> provenance store, and a future orchestration UI. Companions:
> [`comparison-sandbox.md`](comparison-sandbox.md) for the landscape,
> [`borrowing-from-shepherd.md`](borrowing-from-shepherd.md) and
> [`borrowing-from-governance-planes.md`](borrowing-from-governance-planes.md)
> for the sibling idea-mines.

## TL;DR

docker-agent and `h5i` sit on **opposite ends of the purpose axis**.

- **docker-agent** *is* the agent: a declarative YAML runtime (agents, model
  providers, MCP toolsets, RAG, delegation/handoff, OCI-packaged agents) with
  a full Bubble Tea chat TUI. It runs the model loop itself.
- **`h5i`** wraps *other people's* agents: real agent CLIs run in a confined
  git worktree, and everything they do becomes provenance in `refs/h5i/*`.

So there is almost no product overlap, but four docker-agent subsystems map
directly onto `h5i`'s core concerns and are worth mining:

| docker-agent subsystem | `h5i` counterpart |
|---|---|
| `pkg/worktree` (throwaway worktrees, PR checkout, `worktree_create` hook) | `env` worktree backend |
| `pkg/sandbox` + `kit` (sandbox VM, default-deny egress, boundary redaction) | isolation tiers + egress proxy + `prepare_home_state` |
| `pkg/snapshot` + `pkg/app/undo` (shadow-git checkpoints, per-turn undo) | env branch + captures + CRDT deltas |
| `pkg/session` / transcript / hooks / OTel (event-level audit) | captures, context traces, git notes |

Two things to state up front so we calibrate correctly:

1. **`h5i` is ahead on enforcement depth.** docker-agent's isolation is
   all-or-nothing: either trust the host (permissions prompts + `--yolo`) or
   copy the whole run into a disposable sandbox VM via the separate
   `docker sandbox` plugin. There is no Landlock/seccomp/rlimit story, no
   process tier, no per-env credential isolation on the host. `h5i`'s
   workspace/process/supervised tiers have no analog there, and its VM story
   maps to our reserved external `microvm` backend slot.
2. **docker-agent is ahead on live UX.** Its Kanban board drives many
   parallel agents (each on its own worktree + tmux session) through a
   per-run control plane, with reattach, crash recovery, and resume. `h5i`'s
   arena (`env compare`) is post-hoc ranking; we have no live view of running
   envs at all. This is the single biggest thing to borrow.

## Borrow: system (isolation and safety mechanics)

### 1. Explained, user-extensible egress holes

docker-agent's sandbox is default-deny egress, like our container proxy, but
the *ergonomics* around the denial are much better (`cmd/root/sandbox.go`,
`allowSandboxHosts`):

- Holes are punched **minimally and programmatically**: always the model
  gateway, plus per-toolset package hosts resolved from the aqua registry
  (Go module proxy for `go_install` tools, GitHub releases, etc.).
- **Every opened host is printed to the user** at session start, so a
  `403 Blocked by network policy` is self-diagnosing.
- A blocked host has a one-line durable fix: `docker agent sandbox allow
  <host>` appends to a **persistent user-level allowlist**.
- An agent can declare its own needs in config (`runtime.network_allowlist`).

`h5i` today: `net.egress` is a fail-closed profile field; a 403 from the
CONNECT proxy is silent from the box's perspective and editing `.h5i/env.toml`
is the only remedy. **Borrow:** (a) print the effective allowlist at
`env run`/`shell` start (we already render it in `env capabilities`; surface
it at session start too), (b) log denied hosts as capture findings so
`recall search` answers "what did the box try to reach?", (c) an
`h5i env allow <host>` persistent user allowlist, merged into the profile and
recorded in the manifest so it stays audit-visible.

### 2. Kit-style redaction at the *inbound* boundary

The "kit" (`pkg/sandbox/kit/kit.go`) stages every host resource the sandbox
needs (skills, AGENTS.md/CLAUDE.md, sub-agent configs) into a content-hashed,
read-only bundle, and **every text file is secret-scrubbed with portcullis
before it enters the sandbox**. Symlinks that escape the staged root are
dropped, and the manifest omits host source paths so the box cannot learn the
host layout. Redaction also runs on three runtime legs: tool args, outgoing
LLM messages, tool output.

`h5i` today: we redact *outbound* evidence (captures, exec events) but the
*inbound* leg is unfiltered. `prepare_home_state` copies the real
`~/.claude*`/`~/.codex` into the per-env HOME verbatim, so any secret in
those files rides into the box. **Borrow:** run our existing redaction rules
over the copy-in seed (and any future skill/config staging), and record the
redaction count in the manifest the way captures already record
`redactions`. Content-hash the staged state so re-seeding is cheap and
tamper-evident.

### 3. Shadow-git snapshots and per-turn undo

`pkg/snapshot` keeps a **separate shadow git repo per worktree** (plumbing
only: `write-tree`/`read-tree`) and checkpoints workspace state per agent
turn. That powers file-level `Diff`/`Patch` views and `UndoLastSnapshot`:
restore the workspace to the checkpoint before the last turn, without
touching the real repo's history or index. Respects `.gitignore`, skips huge
files, GCs after 7 days.

`h5i` today: the env branch records *commits* and CRDT deltas record file
edits, but there is no cheap "workspace tree as of run N" object, so undo is
all-or-nothing (`abort`). **Borrow:** a `write-tree` checkpoint into the
env's own object store at the start and end of every `env run`/`shell`
session (we already hold `run.lock` there), giving `env diff <name>
--run <n>` and an `env undo` that rolls the worktree back one session. This
is the cheap git-native 80% of Shepherd's reversible effect log
(borrowing-from-shepherd #1) and could ship long before that design doc.

## Borrow: architecture

### 4. A per-run control plane and run registry

Every docker-agent run can expose a control plane (`--listen`, unix socket):
external processes subscribe to the live event stream, steer, send
follow-ups, and resume. A **run registry** records `{PID, addr, sessionID,
agent, startedAt}` so any tool can discover live runs. The board is *only* a
client of this plane, which is why it survives restarts and can reattach.

`h5i` today: a running `env shell` is opaque; observers get torn reads of the
worktree via `--readonly` mounts, and evidence appears only at session end
(spool ingest). **Borrow:** a tiny per-env control socket owned by the
`run.lock` holder that streams capture events as they spool, plus a registry
file under `.git/.h5i/` mapping live envs to `{pid, socket, started_at}`.
This is the prerequisite for any live UI (#6) and would also let `msg watch`
style dashboards show env activity in real time. PID-identity checks (not
timestamps) decide staleness, which matches our existing lock philosophy.

### 5. Versioned config schema with frozen migrations

docker-agent's `agent-schema.json` is versioned (v12 current); old versions
are **frozen** with migrations, `latest/` is the only editable schema, and
the JSON Schema is published so editors validate YAML as you type. Reusable
named `toolsets`/`mcps`/`commands` groups keep configs DRY, and
`runtime.sandbox: true` bakes the isolation requirement into the shareable
config itself.

`h5i` today: `.h5i/env.toml` has no `version` field, no migration path, and
no published schema. Profiles are already checked in and shareable, so a
breaking profile change silently breaks other clones. **Borrow:** a
`version` key in `env.toml` (refuse-on-newer, migrate-on-older), a published
JSON Schema for editor validation, and consider letting a profile declare
`isolation = "container"` as a *requirement* the way `runtime.sandbox` does
(we already refuse rather than downgrade, so this is mostly documentation
surface).

### 6. Approval decisions as first-class audit events

docker-agent's hook taxonomy includes explicitly *observational* events:
`on_tool_approval_decision` (who approved what: allow/deny/canceled, and the
source of the decision) and `on_agent_switch` (which agent ran which tools).
Audit pipelines get structured "who authorized this" records for free.

`h5i` today: captures record what *ran*; nothing records what the human
*approved or declined*. For the review story (`propose`/`apply` is
reviewer-selected, never automatic) this is a real gap: the apply event says
what happened but not the decision trail. **Borrow:** capture
approval/decline decisions (from `env apply`/`abort`/`rm --force`, and from
agent-harness permission hooks where available) as typed events in
`events.jsonl`, so `env log` shows the authorization trail next to the
execution trail.

### 7. Worktree ergonomics: create hook and PR checkout

Three small `pkg/worktree` touches worth copying directly:

- **`worktree_create` hook**: runs once inside the fresh checkout before the
  session starts; the documented use is copying untracked files git will not
  carry (`.env`, local config) and warming caches. This is a real pain point
  for `env create` today: a fresh worktree lacks the untracked local state
  most builds need. A `[hooks] on_create = "<cmd>"` in the profile
  (policy-confined, captured like any run) closes it.
- **PR worktrees**: `--worktree-pr <n|URL>` delegates to `gh pr checkout` in
  a detached worktree so commits push back to the PR, forks handled. An
  `h5i env create --pr <n>` would make the cross-agent review loop (claude
  proposes, codex reviews on another clone) work against GitHub PRs too.
- **`NewCommits` status**: their worktree status distinguishes
  modified/untracked from "HEAD moved since creation" and warns before
  discarding. We track base drift; we should equally refuse a careless `rm`
  when the env branch has unproposed commits (today `--force` is the only
  gate).

## Borrow: UI design

### 8. The Kanban board: the flagship pattern (the big one)

`docker agent board` is the single most relevant UX in the whole repo:

- Each **card is one agent** on its own isolated worktree, running in its own
  tmux session. Columns form a **pipeline** (default Dev, Review, Push, Done)
  and each column carries a **prompt**: moving a card forward *sends that
  column's prompt to the card's agent* ("Review the local changes, fix bugs",
  "commit, rebase, push, open a PR with `gh`"). Columns and prompts are
  user-editable and persisted.
- The board drives and observes agents purely through the per-run control
  plane (#4). Quitting the board leaves agents running; restarting
  reattaches. A dead agent is relaunched *resuming the same conversation and
  worktree*; three consecutive launch failures turn the card red for
  inspection instead of relaunching forever.
- Per-card actions: attach, view worktree diff, open in editor, open shell,
  delete card + session + worktree + branch.

`h5i` already has every ingredient except the view: envs are the cards
(worktree + branch + policy + evidence), `env compare` is the ranking,
`propose`/`apply` is the pipeline's right edge, and i5h messages are the
inter-column handoffs. **Borrow:** an `h5i board` (or web-dashboard page,
since the `web` feature already ships axum) where cards are envs, columns map
to the env lifecycle (`created → running → proposed → applied`), a forward
move sends the column prompt to the env's agent via i5h, and card actions
wrap `env shell` / `env diff` / `env rm`. Deliberately thin: state lives in
the existing refs and manifests, the board is a client. This turns the
"arena" from a CLI table into the product's face.

### 9. Smaller UX sparks

- **Two-tier UI split**: a full interactive TUI and a separate lean streaming
  renderer for headless/`--exec` runs, instead of one UI degrading. Our CLI
  output could adopt the same split (rich `status` vs `--json` is halfway
  there).
- **HTML session export**: rendering a session to a styled standalone HTML
  file. An `h5i env report <name> --html` that packages the review brief,
  diff, captures, and context trace into one shareable file would make
  `propose` output portable to reviewers who never run `h5i`.
- **Doctor command**: `docker agent doctor` bundles environment diagnostics.
  `env probe`/`capabilities` already cover the sandbox half; a top-level
  `h5i doctor` (hooks wired? `$H5I_AGENT` set? refs healthy? binary at
  `/usr/local/bin/h5i` current?) would cut most setup-support round trips.
- **Executable config shebang**: `#!/usr/bin/env docker agent run` makes an
  agent YAML directly runnable. Cute, low-cost: `.h5i/env.toml` is not a
  per-file unit the same way, but a profile-pinned `h5i env shell` launcher
  script is worth considering for team onboarding.

## Not worth chasing

- **The agent runtime itself** (model providers, RAG, chat TUI, delegation,
  ACP/A2A protocols, OCI-packaged agents). This is docker-agent's whole
  product and the opposite of `h5i`'s bet: we wrap real agent CLIs rather
  than reimplement them. Adopting any of it would blur the positioning.
- **The sandbox-VM backend.** Their `--sandbox` re-execs the entire process
  inside a disposable VM via the `docker sandbox`/`sbx` plugin, with `--yolo`
  auto-injected inside (no host left to protect). That is our reserved
  `hardened-container`/`microvm` slot: an external backend to integrate,
  not machinery to build. The *ideas* around its boundary (kit redaction,
  explained egress) are already borrowed above.
- **SQLite session store.** Their transcript lives in SQLite; ours lives in
  git refs, which is the point (shareable via `share push`, mergeable,
  clone-portable). No reason to move.

## Recommendation

- **Do first (small, clearly worth it):** #1 (explained egress + `env allow`
  + denied-host findings), #7 (`on_create` hook, `--pr`, unproposed-commit
  guard on `rm`), and the `h5i doctor` spark from #9.
- **Do next (medium, high leverage):** #3 (per-session shadow snapshots +
  `env undo`), #2 (redact the HOME copy-in seed), #6 (approval-decision
  events).
- **Design doc (the strategic one):** #4 + #8 together: the per-env control
  plane and the board are one feature, and they are the answer to "what does
  `h5i` look like when five agents work in parallel," which today is our
  weakest surface next to docker-agent's strongest.
- **Track, don't build:** their sandbox-VM boundary work, as reference
  material for the external `microvm` backend when we get there.
