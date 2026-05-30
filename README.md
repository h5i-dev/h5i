# h5i

<p align="center">
  <a href="https://github.com/Koukyosyumei/h5i" target="_blank">
    <img src="./assets/logo.svg" alt="h5i logo" height="126">
  </a>
</p>

<p align="center">
  <a href="https://github.com/Koukyosyumei/h5i/actions/workflows/test.yaml"><img alt="tests" src="https://github.com/h5i-dev/h5i/actions/workflows/test.yaml/badge.svg"></a>
  <a href="https://github.com/Koukyosyumei/h5i/LICENSE"><img alt="Apache-2.0" src="https://img.shields.io/github/license/h5i-dev/h5i?color=blue"></a>
  <a href="https://github.com/Koukyosyumei/h5i/stargazers"><img alt="GitHub stars" src="https://img.shields.io/github/stars/Koukyosyumei/h5i?style=social"></a>
</p>

**h5i is version control for the AI era** — a next-generation, AI-aware Git sidecar for Claude, Codex, and your other coding agents. It records what each agent was asked to do, which files it read and edited, what it decided, what it skipped, and which risks reviewers should inspect first — then versions that context alongside your code in dedicated `refs/h5i/*` refs, so the next agent (and your reviewers) pick up exactly where the last one left off.

### ⚡ The new feature (v.0.1.5): Agent Radio

Because that context already lives in Git, your agents can also **talk to each other through it**. `h5i msg` is a cross-agent message channel stored in `refs/h5i/msg` — typed, operational handoffs (`ASK` · `REVIEW_REQUEST` · `RISK` · `DONE`), not chat. Claude asks, Codex reviews, risks get flagged and resolved — all on a wire that survives clones, machines, and branches and **union-merges with nothing lost**.

<p align="center">
  <img src="./assets/h5i-msg-demo.gif" alt="h5i msg watch — a live claude ↔ codex code review streaming over refs/h5i/msg" width="95%">
</p>

<p align="center"><sub><code>h5i msg watch</code> — a live claude ↔ codex code review, streamed straight off <code>refs/h5i/msg</code>. <a href="#agent-radio--agents-that-talk-over-git">Jump to Agent Radio ↓</a></sub></p>

---

### The foundation: a versioned record of every agent's work

Under that messaging layer is the real engine — h5i captures each session as a reasoning DAG (goal → milestones → OBSERVE / THINK / ACT traces) and versions it next to your code.

<p align="center">
  <img src="./assets/h5i-concept.svg" alt="h5i context DAG view" width="95%">
</p>

When a branch is ready for review, h5i surfaces all of it where reviewers already work — on the pull request.


<table>
<tr>

<td width="38%" valign="top">

**The AI Pull Request Brief:**

```bash
h5i share pr post
```

---
**🔎 Review focus** 

The exact files to open first, ranked by where the agent spent its compute.

---
**🎯 Goal & Intent**

The goal agents were tasked to solve.

---
**📌 Reviewer checklist**

Actionable verification steps tailored for this specific diff.

---
**🧠 Reasoning**

The OBSERVE / THINK / ACT steps.

---
**🛡️ Security & Duplicated Code**

Automated check for credential leaks, blind edits, and copy-pasted blocks.

---
**🤖 AI Provenance**

Track the prompt, model names, and commit lineage.

<td width="62%" align="center">

<img
  src="assets/pr-demo.svg"
  alt="h5i review brief"
  width="100%"
/>
</td>

</tr>
</table>

</br>

---

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/Koukyosyumei/h5i/main/install.sh | sh
```

Or build from source:

```bash
cargo install --git https://github.com/Koukyosyumei/h5i h5i-core
```

---

## 60-Second Flow

Initialize h5i in an existing Git repo:

```bash
h5i init
```

For Claude Code hooks and MCP tools:

```bash
h5i hook setup
```

For Codex sessions:

```bash
h5i codex prelude
h5i codex sync
h5i codex finish --summary "implemented retry-aware API client"
```

Task-aware context recall is a generic recall command, off unless invoked:

```bash
h5i recall context smart --query "retry-aware API client" --limit 5
```

Commit with AI provenance:

```bash
h5i capture commit -m "switch session store to Redis" \
  --model claude-sonnet-4-6 \
  --agent claude-code \
  --prompt "sessions need to survive process restarts"
```

Post the PR review brief:

```bash
h5i share pr post --style review      # upsert sticky PR comment
h5i share pr body --style review      # render markdown for CI
h5i share pr post --style replay      # make the Mermaid DAG the hero
```

`h5i share pr post` requires the GitHub CLI (`gh`) to be installed and authenticated
(`gh auth status` clean). Use `h5i share pr body` when CI should render markdown
without posting through `gh`.

The comment also folds in a collapsed **💬 Agent coordination** section: the
branch-relevant cross-agent message threads from `refs/h5i/msg`. It is
disclosure-safe by default — only review-typed messages (`REVIEW_REQUEST`,
`RISK`, `HANDOFF`, `ASK`, …) show a secret-redacted excerpt; `FYI`/free-text are
metadata-only. Use `--no-msg` to drop it, `--msg-bodies` to include every kind's
excerpt, or `--msg-limit N` to cap threads.

Sync h5i sidecar refs with teammates:

```bash
h5i share push
h5i share pull
```

### Our Choices

- **Shared context is the product** - PR comments, the dashboard, and terminal preludes are views over the same versioned agent context.
- **Recorded, not guessed** - h5i stores prompts, model metadata, file observations, decisions, tests, and risk signals instead of trying to infer intent from a diff.
- **Git-native sidecar refs** - provenance lives in `refs/h5i/*`, separate from your working tree and pushable with your repo.
- **Context survives handoff** - branch goals, milestones, TODOs, and OBSERVE / THINK / ACT traces can be restored by the next agent.
- **Review signals should lead** - credential leaks, duplicate code, blind edits, and sensitive files are surfaced before the full audit trail.

### PR Body Styles

| Style | Best for |
|---|---|
| `review` | Default reviewer-friendly brief: triage first, reasoning highlights last, DAG collapsed. |
| `receipt` | Screenshot-friendly provenance card with punchline stats. |
| `detective` | Narrative: goal, numbers, considered alternatives, key insight, shipped work. |
| `replay` | Mermaid reasoning DAG promoted above the fold. |
| `minimal` | Quiet internal provenance with little presentation chrome. |

---

## What h5i Records

| Signal | Example |
|---|---|
| Prompt | `sessions need to survive process restarts` |
| Model + agent | `claude-sonnet-4-6` via `claude-code` |
| File observations | `OBSERVE src/pr.rs` before editing PR output |
| Reasoning traces | `THINK`, `NOTE`, TODOs, risks, deferrals |
| Test evidence | `cargo test`, `go test`, custom runner output |
| Claims | Verified repo facts that auto-invalidate when files change |
| Review signals | Credential leaks, duplicate code, blind edits, sensitive files |

---

## What It Looks Like Locally

The context DAG shows how the work unfolded:

```bash
h5i recall context show
```

<p align="center">
  <img src="./assets/screenshot_h5i_dag.png" alt="h5i context DAG view" width="95%">
</p>

The dashboard makes AI provenance browsable:

```bash
h5i serve        # http://localhost:7150
```

<p align="center">
  <img src="./assets/screenshot_h5i_server.png" alt="h5i web dashboard showing AI commit timeline and context details" width="95%">
</p>

---

## Commands That Matter

| Command | Use it for |
|---|---|
| `h5i share pr post --style review` | Post the sticky reviewer-first PR body. |
| `h5i capture commit` | Commit code with prompt, model, tests, decisions, and provenance. |
| `h5i recall context` | Restore branch goals, milestones, reasoning traces, and TODOs. |
| `h5i capture claim` | Save verified repo facts that auto-invalidate when evidence changes. |
| `h5i audit review` | Find commits that deserve extra human attention. |
| `h5i audit vibe` | Audit an inherited repo's AI footprint and risk signals. |
| `h5i serve` | Open the local provenance dashboard. |

`h5i` organizes commands around four nouns:

| Noun | Use it for |
|---|---|
| `h5i capture` | Record provenance: commits, claims, memory snapshots. |
| `h5i recall` | Read history: logs, blame, context, notes, claims, memory. |
| `h5i audit` | Assess risk: review, scan, compliance, policy, vibe. |
| `h5i share` | Publish: push, pull, PR comments, memory. |

---

## Agent Radio — agents that talk over Git

> **h5i's killer feature.** Everything above versions an agent's *context*; Agent
> Radio lets agents *coordinate* over that same Git-native substrate.

`h5i msg` is a cross-agent message channel stored **in Git**, not in a local
database. Because the log lives in `refs/h5i/msg`, a conversation survives
clones, machines, and branches — it travels with `h5i share push` / `pull`,
and divergent sends from two machines **union-merge** with no message lost.

Bare `h5i msg` opens the inbox dashboard:

```text
┌─ H5I AGENT RADIO ──────────────────────────────────────────────────────┐
│ repo h5i   branch communication   agent codex   unread 2               │
├─ INBOX — 2 unread ─────────────────────────────────────────────────────┤
│  1 22:14  claude → codex  [review] #25d2d86b3944ad9a                   │
│      Please review the auth refactor before I open the PR              │
│  2 22:16  reviewer → codex  [risk] #3a8f63268e9f3d44                   │
│      Check token refresh behavior after h5i pull                       │
├─ GIT PROOF ────────────────────────────────────────────────────────────┤
│ ref refs/h5i/msg · 34 messages · tip #c6d2c03 · last activity 14s ago  │
└────────────────────────────────────────────────────────────────────────┘
  actions:  reply <n> "…"   send <agent> "…"   watch   history
```

The two-terminal demo (identity is per-agent via `$H5I_AGENT`, so both can share
one clone without colliding):

```bash
# Terminal 1 — Claude requests a review
h5i msg setup claude     # once: env H5I_AGENT=claude + turn-delivery hook
h5i msg review --branch auth-refactor codex Review before I open the PR
h5i share push           # only when sharing across clones/machines

# Terminal 2 — Codex
H5I_AGENT=codex codex     # launch Codex with its identity
h5i share pull
h5i codex sync           # Codex auto-delivery surfaces the review
h5i msg done 1 Found a stale refresh-token cache in src/auth.rs:88
```

| Command | Use it for |
|---|---|
| `h5i msg` | Inbox dashboard (header · inbox · Git proof). |
| `h5i msg as <name>` | Set this repo's agent identity. |
| `h5i msg send <agent> <text>` | Send a message (`all` to broadcast). |
| `h5i msg reply <n> <text>` | Reply to a numbered message (threaded). |
| `h5i msg watch` | Live stream of incoming messages. |
| `h5i msg history` / `team` | Full log / known agents. |
| `h5i msg replay` | Replay the log as a live feed (pause between messages). |

Messages follow the **i5h protocol** ([docs/i5h-protocol.md](docs/i5h-protocol.md)) —
typed, operational handoffs rather than chat. Typed verbs set the message kind
and structured fields:

| Verb | Kind | Notable flags |
|---|---|---|
| `h5i msg ask <agent> <text>` | `ASK` | — |
| `h5i msg review <agent> <text>` | `REVIEW_REQUEST` | `--branch --focus --risk --pr` |
| `h5i msg risk <agent> <text>` | `RISK` | `--focus --priority` |
| `h5i msg handoff <agent> <text>` | `HANDOFF` | `--branch --context --focus` |
| `h5i msg ack\|done\|decline <n> [text]` | `ACK` / `DONE` / `DECLINE` | threaded reply to message `<n>` |

**Setup is one line per agent.** Identity is per-agent (via `$H5I_AGENT`), not
per-command — no `--as` needed in normal use:

```bash
h5i msg setup claude          # Claude Code: sets env H5I_AGENT + turn-delivery Stop hook
H5I_AGENT=codex codex         # Codex: just launch with the identity in its env
```

`h5i msg setup` writes `./.claude/settings.json` by default (per-project) with an
autonomous turn hook (the agent handles incoming messages); pass `--scope user`
for all projects, or `--no-block` for a notify-only hook. It's idempotent. Add
`--plain` to any read command for greppable output; hook output is framed as
untrusted collaborator input, never as instructions.

> Agent messaging that survives clones, machines, and branches — because it is stored in Git.

---

## Token Savings With Claims

Agents waste tokens rediscovering facts they already proved. `h5i capture claim` records a fact with the exact evidence files that support it:

```bash
h5i capture claim "HTTP only src/api/client.py: fetch_user, create_post, delete_post." \
  --path src/api/client.py
```

If an evidence file changes, the claim becomes stale and stops being injected into future context.

Controlled experiment at N=10 trials per arm (`./scripts/experiment_claims.sh`), single model `claude-opus-4-7`, MCP server mounted, fidelity 10/10 in every arm:

| Metric | No claims, mean +/- sd | With claims, mean +/- sd | Delta |
|---|---:|---:|---:|
| Cache-read tokens | 528,136 +/- 101,765 | 165,722 +/- 105,423 | **-68.6%** |
| Read tool calls | 5.2 +/- 1.1 | 1.0 +/- 0 | -80.8% |
| Assistant turns | 16.5 +/- 2.8 | 6.1 +/- 3.2 | -63.0% |
| Wall time | 46 +/- 15 s | 20 +/- 7 s | -55.6% |
| Fidelity | 10/10 | 10/10 | unchanged |

Full methodology and raw results: [scripts/experiment_claims_results.md](scripts/experiment_claims_results.md).

---

## Storage Model

h5i is a pure Git sidecar. It uses dedicated refs, so it does not pollute your working tree or normal branch graph.

| Ref | What lives there |
|---|---|
| `refs/h5i/notes` | Per-commit metadata: model, agent, prompt, tokens, tests, decisions, risk signals. |
| `refs/h5i/context` | The reasoning workspace as a DAG: goal, milestones, traces, branches, restores. |
| `refs/h5i/ast` | AST snapshots for structural blame and semantic diffs. |
| `refs/h5i/checkpoints/<agent>` | Per-agent memory snapshots. |
| `refs/h5i/msg` | Cross-agent message log (append-only, union-merged on pull). |

Because these are Git objects, they are content-addressed, deduplicated, pushable, fetchable, and survive `git gc`.

---

## When To Use h5i

Use h5i when:

- AI agents write production code in your repo
- reviewers need to know what the agent saw before editing
- long tasks span multiple sessions or multiple agents
- security-sensitive work needs provenance
- future agents should reuse verified facts instead of rediscovering them
- you inherit a repo and need a fast AI-risk map

You probably do not need h5i for tiny throwaway scripts.

---

## Documentation

- [Manual](MANUAL.md) - full command reference
- [Tutorials](tutorials/) - guided workflows
- [Blog](https://h5i.dev/blog/index.html) - design notes, audits, and case studies
- [Website](https://h5i.dev/) - project overview

---

## Contributing

High-impact contributions:

- try h5i on a real AI-assisted repo and file issues with confusing moments
- improve PR-body presentation and GitHub reviewer workflows
- add adapters for more test runners and agent tools
- harden prompt-injection and compliance rules
- improve dashboard workflows for reviewers

If the idea matters to you, starring the repo is the fastest way to help more AI-heavy teams find it.

---

## License

Apache-2.0. See [LICENSE](LICENSE).
