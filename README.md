# h5i

> Git-native provenance for AI-generated code, surfaced where reviewers already work: the pull request.

<p align="center">
  <a href="https://github.com/Koukyosyumei/h5i" target="_blank">
    <img src="./assets/logo.svg" alt="h5i logo" height="126">
  </a>
</p>

<p align="center">
  <a href="https://github.com/Koukyosyumei/h5i/actions/workflows/test.yaml"><img alt="tests" src="https://github.com/Koukyosyumei/h5i/actions/workflows/test.yaml/badge.svg"></a>
  <a href="https://github.com/Koukyosyumei/h5i/LICENSE"><img alt="Apache-2.0" src="https://img.shields.io/github/license/Koukyosyumei/h5i?color=blue"></a>
  <a href="https://github.com/Koukyosyumei/h5i/stargazers"><img alt="GitHub stars" src="https://img.shields.io/github/stars/Koukyosyumei/h5i?style=social"></a>
</p>

<p align="center">
  <strong>Claude Code and Codex can write the patch. h5i makes the patch reviewable.</strong>
</p>

AI agents are becoming normal contributors. Git still only tells reviewers what changed, not what the agent saw, why it made the change, what it skipped, or which commits need extra human attention.

`h5i` is a Git sidecar that records that missing context and turns it into a sticky PR review brief.

```bash
h5i share pr post --style review
```

```md
## h5i review brief

**Merge status:** review needed · security clean · 4 duplicate-code findings in 4 files
**Review focus:** `src/pr.rs`, `src/main.rs`, `src/mcp.rs`
**Evidence:** 6 AI commits · 3 files touched · 306 trace nodes

### Reviewer checklist
- Inspect duplicate-code findings in `src/main.rs`, `src/mcp.rs`, `src/pr.rs`, `web/src/theme.css`.
- Start review with `src/pr.rs`, `src/main.rs`, `src/mcp.rs`.

### Reasoning highlights
| Signal | Trace |
|---|---|
| `THINK` | Keep review style Mermaid DAG collapsed below audit sections because triage should lead while provenance remains available on click. |

<details>
<summary><b>Reasoning by file</b> - click to expand the Mermaid DAG</summary>
...
</details>
```

If you want AI-generated code to be easier to trust, review, hand off, and audit, star h5i.

## Why Star This Repo

- **Best first feature: PR review bodies for AI code.** `h5i share pr post` upserts a sticky GitHub PR comment with merge status, review-focus files, reasoning highlights, credential/duplicate findings, and per-commit AI provenance.
- **Prompt-to-commit provenance.** `h5i capture commit` attaches prompt, model, agent, token usage, tests, decisions, and risk signals to the Git commit.
- **Reasoning memory across sessions.** `h5i recall context` restores branch goals, milestones, OBSERVE / THINK / ACT traces, TODOs, and decisions so future agents do not start cold.
- **Lower token waste with live claims.** `h5i capture claim` records content-addressed facts that auto-invalidate when evidence files change. In a controlled N=10 experiment, claims cut cache-read tokens by **68.6%**.
- **Risk triage without alert fatigue.** `h5i audit review` separates real Quality signals from noisy Shape signals so PR flags fire for meaningful review risks.
- **Git-native, not SaaS-native.** Metadata lives in `refs/h5i/*` beside your code and can be pushed, fetched, reviewed, and garbage-collected like Git data.

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

## The Pull Request Is The Product

Most AI coding tools stop at generation. h5i focuses on the moment that matters after generation: review.

The PR body can show:

- merge status: security clean, review needed, or block merge
- review focus: the files a human should inspect first
- evidence: AI commits, touched files, trace nodes, decisions, test evidence
- reasoning highlights: curated THINK / NOTE entries
- credential leaks and duplicate-code findings
- a collapsed Mermaid DAG of what the agent read, thought, and edited
- per-commit prompt, model, agent, decisions, tests, and review flags

Styles:

| Style | Best for |
|---|---|
| `review` | Default reviewer-friendly brief: triage first, reasoning highlights last, DAG collapsed. |
| `receipt` | Screenshot-friendly provenance card with punchline stats. |
| `detective` | Narrative: goal, numbers, considered alternatives, key insight, shipped work. |
| `replay` | Mermaid reasoning DAG promoted above the fold. |
| `minimal` | Quiet internal provenance with little presentation chrome. |

## What h5i Captures

| Signal | Why it matters |
|---|---|
| Prompt | Reviewers see what the agent was asked to do. |
| Model + agent | You know which tool produced the change. |
| File reads and edits | Reviewers can tell whether the agent looked before editing. |
| THINK / NOTE traces | Intent, risks, TODOs, and deferrals survive the session. |
| Decisions | Alternatives and tradeoffs are attached to the commit. |
| Tests | Test evidence travels with the commit. |
| Claims | Verified facts are reused until their evidence files change. |
| Review signals | Credential leaks, duplicate code, blind edits, sensitive files, and other risks become PR-visible. |

## What It Looks Like Locally

The dashboard makes AI provenance browsable:

```bash
h5i serve        # http://localhost:7150
```

<p align="center">
  <img src="./assets/screenshot_h5i_server.png" alt="h5i web dashboard showing AI commit timeline and context details" width="95%">
</p>

The context DAG shows how the work unfolded:

<p align="center">
  <img src="./assets/screenshot_h5i_dag.png" alt="h5i context DAG view" width="95%">
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
