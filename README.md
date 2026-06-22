<p align="center">
  <a href="https://h5i.dev/" target="_blank">
    <img src="./docs/_static/logo.png" alt="h5i logo" height="126">
  </a>
</p>

<p align="center">
  <a href="https://github.com/h5i-dev/h5i/actions/workflows/test.yaml"><img alt="tests" src="https://github.com/h5i-dev/h5i/actions/workflows/test.yaml/badge.svg"></a>
  <a href="https://github.com/h5i-dev/h5i/blob/main/LICENSE"><img alt="Apache-2.0" src="https://img.shields.io/github/license/h5i-dev/h5i?color=blue"></a>
  <a href="https://github.com/h5i-dev/h5i/stargazers"><img alt="GitHub stars" src="https://img.shields.io/github/stars/h5i-dev/h5i?style=social"></a>
  <a href="https://github.com/h5i-dev/h5i/releases"><img alt="release" src="https://img.shields.io/github/v/release/h5i-dev/h5i?label=release"></a>
  <br>
  <a href="#run-an-ensemble-60-seconds"><img alt="Agent Ensembles" src="https://img.shields.io/badge/Agent%20Ensembles-Claude%20%2B%20Codex-6f42c1"></a>
  <a href="#why-the-verdict-is-trustworthy"><img alt="Neutral Verifier" src="https://img.shields.io/badge/Neutral%20Verifier-sandboxed%20%26%20gated-D21C1C"></a>
  <a href="#everything-rides-with-every-run"><img alt="95% less token waste" src="https://img.shields.io/badge/Compressed%20Logs-95%25%20less%20token%20waste-2ea44f"></a>
  <a href="#what-makes-it-auditable-refsh5i"><img alt="Lives in your Git" src="https://img.shields.io/badge/Lives%20in%20your%20Git-refs%2Fh5i%2F*%20%C2%B7%20no%20SaaS-0969da"></a>
</p>

<h1 align="center">Run many coding agents. Merge one auditable result.</h1>

Agent ensembles work because **independent attempts beat isolated guesses**. h5i runs several coding agents on the *same* task, each in its own sandbox, **sealed** so they can't copy one another. It lets them peer-review, then a **neutral verifier** replays every candidate, runs the tests itself, and merges the one that actually passes. The whole run (prompts, models, commands, logs, policies, messages, and the verdict) is versioned in your repo under `refs/h5i/*`.

> ***Two heads are better than one.***

<p align="center">
  <img src="./assets/screenshot_sandbox_h5i_3.png" alt="One task fans out to several agents working in sealed sandboxes; the agents peer-review each other; a neutral verifier replays each candidate; one verified result is applied." width="95%">
</p>

<table align="center">
  <tr>
    <td align="center"><strong>Isolated per agent</strong><br><sub>no file, branch, or port clashes</sub></td>
    <td align="center"><strong>Sealed attempts</strong><br><sub>independence-first</sub></td>
    <td align="center"><strong>Neutral verifier</strong><br><sub>a fair, sandboxed winner</sub></td>
    <td align="center"><strong>Lives in your Git</strong><br><sub>refs/h5i/* · no SaaS</sub></td>
  </tr>
</table>

**Who it's for:** platform, security, and DevEx leads rolling out Claude Code and Codex who want to run *teams* of agents and keep review and audit defensible as agents write more of the diff.

### Why naive agent teams break

In ML, ensembles beat the best single model: diverse estimators cut variance and won a decade of competitions. The same shift is coming to coding agents, with an architect, an implementer, a reviewer, a security skeptic. But spawn several agents on one repo with **no coordination layer** and you don't get an ensemble, you get a pileup. Six failure modes Git was never built to handle:

| Failure mode | What happens | h5i's answer |
|---|---|---|
| **Environment conflict** | agents overwrite each other's files, ports, caches, credentials, branches | a confined worktree + policy per agent (`h5i env`) |
| **Context contamination** | agents see a peer's output before an independent attempt, so diversity collapses | sealed submissions; no peer access until `freeze` |
| **Token explosion** | every agent re-reads the repo and drags raw logs into context | compressed tool logs (`h5i capture run`, ~95% less) |
| **Review overload** | humans can't inspect every prompt, command, retry, and failure across N agents | one reviewer-ready PR brief (`h5i share pr`) |
| **Unsafe autonomy** | agents run destructive commands without containment | Landlock + seccomp + namespaces, fail-closed |
| **No fair winner** | many patches, no neutral evidence for which to merge | a sandboxed verifier + explainable verdict |

### Recent News

- **New: Agent teams (`h5i team`).** Run several agents on one task in sealed sandboxes, peer-review, verify neutrally, and merge one auditable winner. [Run an ensemble below](#run-an-ensemble-60-seconds)
- **New: Sandboxed agent workspaces (`h5i env`).** A disposable, confined worktree per agent: prove what it couldn't reach, apply only after review.
- **New: Prompt Maturity Score.** A deterministic, fully offline **0 to 100** quality signal for every AI commit's prompt, rolled into the PR brief.
- **Agent Radio reached 100+ on Hacker News.** Cross-agent messaging over Git ([discussion](https://news.ycombinator.com/item?id=48345837)).

---

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/h5i-dev/h5i/main/install.sh | sh
```

Or build from source:

```bash
cargo install --git https://github.com/h5i-dev/h5i h5i-core
```

---

## Run an ensemble (60 seconds)

Initialize h5i and wire the Claude Code / Codex hooks (the `--team` hook keeps each agent alive through the peer-review round):

```bash
h5i init
h5i hook setup --write --wrap-bash --team
```

Run the same task across several agents and merge the verified winner. A roster member is a **persona, not a backend**: `runtime` · `model` · `persona` are attributes, so a team can be three Claudes with different skills, a Claude + Codex mix, or one model under two personas:

```bash
# 1. create a run, then add one confined env per agent persona
#    (--persona injects a standing working style; each agent gets an
#     auto-generated id — `h5i team status` shows them)
h5i team create fix-auth --base HEAD
h5i team add-env fix-auth env/claude-arch/fix-auth --runtime claude --persona examples/personas/architect.md
h5i team add-env fix-auth env/codex/fix-auth       --runtime codex  --persona examples/personas/implementer.md
h5i team status fix-auth                          # note the generated agent ids

# 2. launch every agent in its own sealed box (a terminal per env)
scripts/team-launch.sh fix-auth --task task.md

# 3. Each agent peer-reviews, and revises inside its box
scripts/team-review.sh fix-auth

# 3. the neutral verdict: replay each candidate, run the tests, merge the winner
# h5i team sync     fix-auth                       # ingest agents' staged work (no relaunch)
# h5i team freeze   fix-auth                       # seal the independent attempts
h5i team verify   fix-auth --agent <agent-id> -- cargo test   # id from `team status`
h5i team finalize fix-auth                       # explainable verdict (gates + smallest diff)
h5i team apply    fix-auth                       # merge the winner, gated on the verdict
```

Or drive the whole cycle hands-off:

```bash
scripts/team-run.sh fix-auth --task task.md --verify-cmd "cargo test" --apply
```

Full command reference: [`MANUAL.md`](MANUAL.md) · `man h5i`.

### How the protocol works

State lives in one append-only event log per run (`refs/h5i/team/<run-id>`): phase, roster, and verdict are *folded* from events, so the whole run travels with `h5i share push` / `pull` for a cross-clone review loop. It is a thin coordination layer, **not** a group chat and **not** a daemon.

| # | Phase | What happens |
|---|---|---|
| 1 | **create** | a roster of personas, each an isolated `h5i env` (runtime · model · role) |
| 2 | **dispatch** | one task to every agent, the same prompt to all |
| 3 | **independent** | each agent works **sealed**, so no agent sees a peer's attempt |
| 4 | **freeze** | submissions become **immutable**, frozen candidates |
| 5 | **review** | scoped, permissioned peer review + opt-in discussion (logged) |
| 6 | **verify** | a neutral, sandboxed judge replays each candidate at the shared base |
| 7 | **verdict** | one explainable, gated winner is applied |

**Independence-first:** diversity is protected until `freeze`. Discussion is opt-in and only allowed *after* it, every message is logged, and any candidate revised afterward is stamped `independent=false` with its influence edges recorded.

### Why the verdict is trustworthy

Finalization never trusts an agent's *own* captures: an agent can run weak tests, omit failures, or report the wrong result. `h5i team verify` is the authority. For each frozen candidate it replays the change at the **shared base** in a fresh, **sandboxed** worktree and runs the declared command itself. The hard gates (tests pass, applies cleanly) come **only** from that run; smallest diff breaks ties **only among gate-passers**, so a candidate can't win by deleting tests or stubbing features. If nothing clears the gates, `finalize` records `no_verdict` and applies nothing, the one place a human is pinged, by choice. `apply` only auto-applies a gate-passing verdict (`--force` is an explicit, logged override).

---

## Everything rides with every run

The ensemble is built on h5i's auditable-workspace primitives; the same evidence accumulates whether you run one agent or a team. Each is a property of the workspace, written to its own `refs/h5i/*` ref:

- **Confined sandbox (`h5i env`).** A disposable worktree plus a policy that limits what the code inside can read, write, and reach. Reading `/etc/shadow`, raw sockets, off-allowlist hosts, and `mount`/`ptrace` are blocked by Landlock + seccomp + namespaces (fail-closed); `h5i env create` picks the strongest tier the host can enforce (`workspace`, then `process`, `supervised`, `container`) or refuses. *Proves the agent physically **could not** exfiltrate, not "we logged it."*

  <p align="center"><img src="./assets/agent-sandbox.svg" alt="An agent runs a build inside a policy-confined sandbox; reads of /etc/shadow, raw sockets, off-allowlist hosts, and mount/ptrace are blocked, while the legitimate build is allowed and captured as reviewer evidence." width="92%"></p>

- **Provenance & audit (`h5i capture commit` · `h5i audit`).** Every commit is **prompt-aware**: prompt, model, agent, tests, and decisions ride along in `refs/h5i/notes`, plus a deterministic, **offline 0 to 100 Prompt Maturity Score**. Deterministic risk triage flags credential leaks, blind edits, and copy-paste, with no model in the loop.

- **95% less token waste (`h5i capture run`).** Wrap any command and the agent sees a compact, normalized summary while the full raw output is stored out of band in `refs/h5i/objects` (recoverable via `h5i recall object`). A 4 MB test log no longer burns the context window.

  <p align="center"><img src="./assets/token-reduction-unified.svg" alt="Every tool's output collapses into one unified schema; the agent sees a compact summary while the raw bytes stay recoverable." width="92%"></p>

- **Cross-agent messaging (`h5i msg`).** A Git-backed channel (*Agent Radio*) for typed handoffs (`ASK` · `REVIEW_REQUEST` · `RISK` · `DONE` · `ACK`) in `refs/h5i/msg`; divergent sends union-merge with no messages lost.

- **Reviewer-ready PRs (`h5i share pr`).** One sticky comment carries review focus, goal, reasoning, tests, prompt maturity, risks, and agent handoffs, so the reviewer reads the provable record of the run, not just the diff.

  <p align="center"><img src="./assets/pr-demo.svg" alt="h5i PR review brief: review focus, goal, prompt maturity, reviewer checklist, reasoning, security flags, and AI provenance in one comment." width="72%"></p>

- **Reasoning DAG & dashboard (`h5i serve`).** Replay the goal, milestones, and OBSERVE / THINK / ACT trace behind every change; the web dashboard renders the commit timeline, provenance, sandbox actions, context, and the **Team** board, compare, and verdict in one place.

---

## What makes it auditable: `refs/h5i/*`

h5i is a pure Git sidecar. The workspace and all of its evidence live in dedicated refs, so they don't pollute your working tree or branch graph, and they travel with the repo.

| Ref | What lives there |
|---|---|
| `refs/h5i/team` | **Agent-team runs**: roster, phases, submissions, and verdict (one append-only event log per run). |
| `refs/h5i/env` | **The sandboxed workspaces**: sandbox events, manifests, and digest-pinned policies. |
| `refs/h5i/notes` | Per-commit provenance: model, agent, prompt, tests, decisions, risk signals. |
| `refs/h5i/context` | The reasoning DAG: goal, milestones, traces, branches. |
| `refs/h5i/msg` | Cross-agent handoff log (append-only, union-merged on pull). |
| `refs/h5i/objects` | Compressed-log capture manifests (full raw kept locally / on a Git LFS backend). |
| `refs/h5i/checkpoints/<agent>` | Per-agent memory snapshots. |

Because these are Git objects, they are content-addressed, deduplicated, pushable, fetchable, and survive `git gc`. **Lives in your Git, travels with the repo, no SaaS, no lock-in, works offline.**

<p align="center">
  <img src="./assets/h5i-concept.svg" alt="h5i stores every dimension of an agent run in refs/h5i/*" width="95%">
</p>

### What h5i is, and is not

- h5i **is not** a Git replacement, a hosted SaaS / dev-environment, or *just* a sandbox.
- h5i **is** a Git sidecar for **auditable agent ensembles**: run many agents, merge one provable result.

**Why not a hosted sandbox?** The whole point is that the workspace and its evidence live *in your repo* (`refs/h5i/*`): pushable, fetchable, offline, and yours. Codespaces, Coder, and E2B give you an environment; h5i gives you an *auditable* one, versioned in Git with no service to depend on.

---

## Documentation

- [Official Website](https://h5i.dev/): project overview, [Pitch Deck](https://h5i.dev/pitch/)
- [`MANUAL.md`](MANUAL.md) / `man h5i`: full command reference
- [Tutorials](https://h5i.dev/guides/): guided workflows · [Blog](https://h5i.dev/blog/): design notes, audits, case studies

---

## Contributing

High-impact contributions:

- try h5i on a real AI-assisted repo and file issues with confusing moments
- run a real agent team and report where the cycle snags
- improve PR-body presentation and GitHub reviewer workflows
- add adapters for more test runners and agent tools
- harden prompt-injection and compliance rules

If the idea matters to you, starring the repo is the fastest way to help more AI-heavy teams find it.

---

## Acknowledgements

h5i's token-reduction filters build on prior art, both Apache-2.0:

- **[rtk](https://github.com/rtk-ai/rtk)**: the declarative output-filter rule files and the engine that runs them are derived from rtk.
- **[headroom](https://github.com/chopratejas/headroom)**: the log line-folding technique (collapse near-identical lines into one with a count) is reimplemented from headroom.

See [`NOTICE`](NOTICE) and [`assets/filters/NOTICE`](assets/filters/NOTICE) for full attribution.

## License

Apache-2.0. See [LICENSE](LICENSE).
