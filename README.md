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

h5i is an open source Git sidecar that turns AI coding sessions into reviewable pull-request provenance.

It records what the agent was asked to do, which files it read and edited, what it decided, what it skipped, and which risks reviewers should inspect first.


<table>
<tr>

<td width="38%" valign="top">

**A reviewer's brief on every AI pull request:**

```bash
h5i share pr post
```

One sticky comment that reads like a triage note — verdict up top, full trail below:

- **Merge status** — ready, review-needed, or block-merge, from the branch's credential and duplicate-code scans
- **Review focus** — the files to open first, ranked by where the agent actually worked
- **Reviewer checklist** — concrete next steps for *this* diff, not boilerplate
- **Reasoning + provenance** — every OBSERVE / THINK / ACT step, plus per-commit prompt, model, agent, and tests — one expand away

**Why it matters**

Reviewers know where to spend their attention without leaving GitHub — and without trusting the diff to explain itself.

<sub>For AI-heavy repos, long-running agent work, security-sensitive changes, and agent-to-agent handoffs.</sub>

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

The context DAG shows how the work unfolded:

```bash
h5i recall context show
```

<p align="center">
  <img src="./assets/screenshot_h5i_dag.png" alt="h5i context DAG view" width="95%">
</p>

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/Koukyosyumei/h5i/main/install.sh | sh
```

Or build from source:

```bash
cargo install --git https://github.com/Koukyosyumei/h5i h5i-core
```

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

Sync h5i sidecar refs with teammates:

```bash
h5i share push
h5i share pull
```

### Our Choices

- **The pull request is the product** - reviewers should see AI risk, intent, and evidence where they already work.
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

## What It Looks Like Locally

The dashboard makes AI provenance browsable:

```bash
h5i serve        # http://localhost:7150
```

<p align="center">
  <img src="./assets/screenshot_h5i_server.png" alt="h5i web dashboard showing AI commit timeline and context details" width="95%">
</p>

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

## Storage Model

h5i is a pure Git sidecar. It uses dedicated refs, so it does not pollute your working tree or normal branch graph.

| Ref | What lives there |
|---|---|
| `refs/h5i/notes` | Per-commit metadata: model, agent, prompt, tokens, tests, decisions, risk signals. |
| `refs/h5i/context` | The reasoning workspace as a DAG: goal, milestones, traces, branches, restores. |
| `refs/h5i/ast` | AST snapshots for structural blame and semantic diffs. |
| `refs/h5i/checkpoints/<agent>` | Per-agent memory snapshots. |

Because these are Git objects, they are content-addressed, deduplicated, pushable, fetchable, and survive `git gc`.

## When To Use h5i

Use h5i when:

- AI agents write production code in your repo
- reviewers need to know what the agent saw before editing
- long tasks span multiple sessions or multiple agents
- security-sensitive work needs provenance
- future agents should reuse verified facts instead of rediscovering them
- you inherit a repo and need a fast AI-risk map

You probably do not need h5i for tiny throwaway scripts.

## Documentation

- [Manual](MANUAL.md) - full command reference
- [Tutorials](tutorials/) - guided workflows
- [Blog](https://h5i.dev/blog/index.html) - design notes, audits, and case studies
- [Website](https://h5i.dev/) - project overview

## Contributing

High-impact contributions:

- try h5i on a real AI-assisted repo and file issues with confusing moments
- improve PR-body presentation and GitHub reviewer workflows
- add adapters for more test runners and agent tools
- harden prompt-injection and compliance rules
- improve dashboard workflows for reviewers

If the idea matters to you, starring the repo is the fastest way to help more AI-heavy teams find it.

## License

Apache-2.0. See [LICENSE](LICENSE).
