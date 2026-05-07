# h5i

> Git provenance, memory, and audit trails for AI-generated code.

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

<p align="center">
  <strong>Claude Code and Codex can write code. h5i remembers why they wrote it.</strong>
</p>

`h5i` (pronounced "high-five") is a Git sidecar for AI-era development. It captures the prompt, model, file reads, edits, decisions, TODOs, and session memory behind each change, then stores that context in dedicated Git refs next to your code.

If you use coding agents, star this repo to track the tool that makes their work reviewable, resumable, and auditable.

```bash
curl -fsSL https://raw.githubusercontent.com/Koukyosyumei/h5i/main/install.sh | sh
```

```bash
# Or build from source
cargo install --git https://github.com/Koukyosyumei/h5i h5i-core
```

<p align="center">
  <img src="./assets/screenshot_h5i_dag.png" alt="h5i context DAG view" width="95%">
</p>

## Why Developers Star h5i

- **See the prompt behind a commit.** `h5i commit` attaches agent, model, prompt, token usage, test result, and reasoning notes to Git history.
- **Resume long AI tasks without amnesia.** `h5i context` restores goals, milestones, OBSERVE / THINK / ACT traces, branch decisions, and open TODOs at the start of the next session.
- **Stop re-reading the whole repo.** `h5i claims` records content-addressed facts that auto-invalidate when their evidence files change. In a controlled N=10 experiment, claims cut cache-read tokens by **68.6%**.
- **Review AI code where risk is highest.** `h5i notes review` ranks commits by uncertainty, blind edits, churn, and scope.
- **Audit inherited repos in seconds.** `h5i vibe` reports AI footprint, fully AI-written directories, leaked-token signals, and prompt-injection hits.
- **Stay in Git.** h5i metadata is stored as first-class Git objects under `refs/h5i/*`, not in a SaaS silo.

## The Problem

Git tells you what changed. It does not tell you:

- who prompted the AI to make the change
- what files the agent read before editing
- what assumptions it made
- what it skipped, deferred, or felt uncertain about
- whether a future agent can safely continue the work
- which AI-generated commits deserve the most human review

h5i adds that missing layer without replacing Git.

## 60-Second Start

Run this inside an existing Git repository:

```bash
h5i init
```

That creates `.git/.h5i/` and appends h5i usage instructions to the agent files your tools already read:

- `CLAUDE.md` plus `.claude/h5i.md` for Claude Code
- `AGENTS.md` for Codex

For Claude Code hooks and MCP tools:

```bash
h5i hook setup
```

For Codex:

```bash
h5i codex prelude                  # restore shared context at session start
h5i codex sync                     # backfill file reads and edits while working
h5i codex finish --summary "..."   # checkpoint the session
```

Commit with provenance:

```bash
h5i commit -m "switch session store to Redis" \
  --model claude-sonnet-4-6 \
  --agent claude-code \
  --prompt "sessions need to survive process restarts"
```

Sync h5i sidecar refs with teammates:

```bash
h5i push
h5i pull
```

## What It Looks Like

When a new session starts, h5i gives the agent a compact handoff instead of a blank slate:

```text
[h5i] Context workspace active - prior reasoning follows.

  branch=main  goal=Build an OAuth2 login system
  milestones=3  commits=7  trace_lines=142+12

  m0: [x] Initial setup
  m1: [x] GitHub provider integration
  m2: [ ] Token refresh flow

[h5i] Last decisions & actions:
  [14:02] THINK: 40 MB overhead acceptable; Redis survives process restarts
  [14:03] ACT:   switched session store to Redis in src/session.rs
  [14:05] NOTE:  TODO: integration test for failover path
```

The dashboard makes that provenance browsable:

```bash
h5i serve        # http://localhost:7150
```

<p align="center">
  <img src="./assets/screenshot_h5i_server.png" alt="h5i web dashboard showing AI commit timeline and context details" width="95%">
</p>

## Core Commands

| Command | Use it for |
|---|---|
| `h5i context` | Versioned agent reasoning: goal, milestones, OBSERVE / THINK / ACT traces, branch context, TODOs, restore points. |
| `h5i commit` | Git commit plus AI provenance: prompt, model, agent, token usage, tests, decisions, and policy metadata. |
| `h5i claims` | Content-addressed reusable facts that future sessions can trust until evidence files change. |
| `h5i notes` | Per-commit review signals: exploration footprint, uncertainty, blind edits, churn, omissions. |
| `h5i vibe` | Fast audit of an inherited repo's AI footprint and risk signals. |
| `h5i compliance` | Text, JSON, or HTML audit report across a date range. |
| `h5i blame` | Line or AST-level blame annotated with AI provenance. |
| `h5i memory` | Snapshot, diff, restore, push, and pull Claude or Codex memory state. |
| `h5i serve` | Local web dashboard for commits, sessions, context, integrity, refs, and memory. |

## Token Savings With Claims

Agents burn tokens rediscovering facts they already proved in earlier sessions. `h5i claims` records those facts with the exact evidence files that support them:

```bash
h5i claims add "HTTP only src/api/client.py: fetch_user, create_post, delete_post." \
  --path src/api/client.py

h5i claims list --group-by-path
```

Each claim is hashed over `(path, blob_oid)` evidence. If an evidence file changes, the claim becomes stale and stops being injected into future context.

<p align="center">
  <img src="./assets/claims-merkle.svg" alt="A claim is backed by evidence paths and git blob OIDs, so edits auto-invalidate stale facts" width="95%">
</p>

Controlled experiment at N=10 trials per arm (`./scripts/experiment_claims.sh`), single model `claude-opus-4-7`, MCP server mounted, fidelity 10/10 in every arm:

| Metric | No claims, mean +/- sd | With claims, mean +/- sd | Delta |
|---|---:|---:|---:|
| Cache-read tokens | 528,136 +/- 101,765 | 165,722 +/- 105,423 | **-68.6%** |
| Read tool calls | 5.2 +/- 1.1 | 1.0 +/- 0 | -80.8% |
| Assistant turns | 16.5 +/- 2.8 | 6.1 +/- 3.2 | -63.0% |
| Wall time | 46 +/- 15 s | 20 +/- 7 s | -55.6% |
| Fidelity | 10/10 | 10/10 | unchanged |

Full methodology and raw results: [scripts/experiment_claims_results.md](scripts/experiment_claims_results.md).

## AI Review And Audit

Find commits that need human attention:

```bash
h5i notes review --limit 50
```

Scan reasoning traces for prompt-injection patterns:

```bash
h5i context scan
```

```text
-- h5i context scan -------------------------------- main
  risk score  1.00  (48 lines scanned, 2 hit(s))

  HIGH line 31  [override_instructions]  ignore all previous instructions
  HIGH line 31  [exfiltration_attempt]   reveal the system prompt
```

Generate an audit report:

```bash
h5i compliance --since 2026-01-01 --until 2026-03-31 \
  --format html --output audit.html
```

Audit any repo you inherit:

```bash
h5i vibe
```

## How h5i Stores Data

h5i is a pure Git sidecar. It uses dedicated refs, so it does not pollute your working tree or normal branch graph.

| Ref | What lives there |
|---|---|
| `refs/h5i/notes` | Per-commit metadata: model, agent, prompt, tokens, tests, decisions, risk signals. |
| `refs/h5i/context` | The reasoning workspace as a DAG: goal, milestones, trace, branches, restores. |
| `refs/h5i/ast` | AST snapshots for structural blame and semantic diffs. |
| `refs/h5i/checkpoints/<agent>` | Per-agent memory snapshots. |

Because these are Git objects, they are content-addressed, deduplicated, pushable, fetchable, and survive `git gc`.

```bash
git for-each-ref refs/h5i/
```

## Claude Code Integration

`h5i hook setup` prints the configuration for:

| Hook | What it captures |
|---|---|
| `SessionStart` | Prior goal, milestones, decisions, TODOs, and live claims. |
| `UserPromptSubmit` | The user prompt, so `h5i commit` can infer it. |
| `PostToolUse` | Read/Edit/Write activity as OBSERVE and ACT trace entries. |
| `Stop` | Final sync and context checkpoint. |

The MCP server exposes native tools such as `h5i_log`, `h5i_blame`, `h5i_context_trace`, `h5i_context_commit`, and `h5i_claims_add`.

## Codex Integration

`h5i init` appends the repo-local `AGENTS.md` instructions Codex needs. The explicit workflow is:

```bash
h5i codex prelude
h5i context init --goal "<one-line task summary>"
h5i context relevant <file>
h5i codex sync
h5i context trace --kind THINK "<decision and reason>"
h5i codex finish --summary "<milestone summary>"
```

`h5i codex sync` reads the active Codex JSONL session and backfills file reads, searches, listings, and patch edits into the context DAG.

## When To Use h5i

Use h5i when:

- AI agents write production code in your repo
- code review needs to know what the agent saw before editing
- long tasks span multiple sessions or multiple agents
- regulated or security-sensitive work needs provenance
- you want future agents to reuse verified facts instead of rediscovering them
- you inherit a repo and need a fast AI-risk map

You probably do not need h5i for tiny throwaway scripts.

## Documentation

- [Manual](MANUAL.md) - full command reference
- [Tutorials](tutorials/) - guided workflows
- [Blog](docs/blog/) - design notes, audits, and case studies
- [Website](https://h5i.dev/) - project overview

## Contributing

High-impact contributions:

- try h5i on a real AI-assisted repo and file issues with confusing moments
- improve Claude Code and Codex integrations
- add adapters for more test runners and agent tools
- harden prompt-injection and compliance rules
- improve dashboard workflows for reviewers

If the idea matters to you, starring the repo is the fastest way to help more AI-heavy teams find it.

## License

Apache-2.0. See [LICENSE](LICENSE).
