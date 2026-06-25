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
  <a href="#everything-rides-with-every-run"><img alt="Sandboxed Worktree" src="https://img.shields.io/badge/Sandboxed%20Worktree-isolated%20%26%20confined-D21C1C"></a>
  <a href="#everything-rides-with-every-run"><img alt="95% less token waste" src="https://img.shields.io/badge/Compressed%20Logs-95%25%20less%20token%20waste-2ea44f"></a>
</p>

<h1 align="center">Run many coding agents. Merge one auditable result.</h1>

Agent ensembles work because **independent attempts beat isolated guesses**. h5i runs several coding agents on the *same* task, each in its own sandbox, **sealed** so they can't copy one another. It lets them peer-review, then a **neutral verifier** replays every candidate, runs the tests itself, and merges the one that actually passes. The whole run (prompts, models, commands, logs, policies, messages, and the verdict) is versioned in your repo under `refs/h5i/*`.

> ***Two heads are better than one.***

<p align="center">
  <img src="./docs/_static/hero-team.svg" alt="One task fans out to claude and codex in sealed sandboxes; they peer-review each other in a continuous loop; a neutral verifier replays and tests every candidate; the one verified result is merged back into your repo." width="99%">
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

## 60-Second Flow

### Setup

Initialize h5i and wire the Claude Code / Codex hooks:

```bash
h5i init
h5i hook setup --write --wrap-bash --team
git add .
git commit -m "update hooks"
```

### Track Prompts and Contexts

Once the hooks are registered, h5i automatically version controls the human prompts and every agent context step (reads, writes, thinking, and so on) of your Claude and Codex agents as Git objects. Noisy tool output is trimmed at the same time. For a `pytest` run, for example, only the failing tests are kept, cutting up to 95% of the token footprint while the full raw output stays tracked and recoverable whenever you need it. Share this metadata with the rest of your team via `h5i share push`, and post a summary of your AI usage (prompt quality score, AI/human commit ratio, secret leak detection, prompt injection score, and more) straight to the pull request with `h5i share pr post` (this one needs the `gh` CLI installed).

```bash
h5i recall context show
h5i share push
h5i share pr post
```

### Sandboxed Environment

h5i gives each agent a secure, sandboxed worktree. Let it run with permissions
off inside the box, then review its diff before anything lands on your branch:

```
h5i env create claude-env --profile agent-claude
h5i env shell claude-env
box$ claude --dangerously-skip-permissions
box$ exit

h5i env diff claude-env
h5i env propose claude-env
h5i env apply claude-env
```

### Run an ensemble

Create two sandboxed agent environments:

```bash
h5i env create claude-env --profile agent-claude
h5i env create codex-env  --profile agent-codex
```

Create a team and register both agents:

```bash
h5i team create  qsort-demo --base HEAD
h5i team add-env qsort-demo env/human/claude-env --runtime claude
h5i team add-env qsort-demo env/human/codex-env  --runtime codex
h5i team status  qsort-demo                                 # note the generated agent ids
```

Dispatch one task to every agent:

```bash
echo "Implement Quick Sort from scratch in Python." | h5i team dispatch qsort-demo
```

Launch every agent in its own sandboxed environment. Each agent automatically starts working on the dispatched task:

```bash
# Terminal 1: Claude, running inside its own h5i sandboxed env.
h5i env shell env/human/claude-env -- claude --dangerously-skip-permissions "$(h5i team bootstrap)"
```

```bash
# Terminal 2: Codex, running inside its own h5i sandboxed env.
h5i env shell env/human/codex-env  -- codex  --sandbox danger-full-access "$(h5i team bootstrap)"
```

Each agent peer-reviews, and revises inside its own implementation:

```bash
h5i team auto-peer-review qsort-demo                       # sync → freeze → mutual grant → instruct
```

Replay each candidate, run the tests, merge the winner:

```bash
h5i team verify   qsort-demo --agent <agent-id> -- pytest  # id from `team status`
h5i team finalize qsort-demo                               # explainable verdict (gates + smallest diff)
h5i team apply    qsort-demo                               # merge the winner, gated on the verdict
```

Monitor the status:

```bash
h5i serve
```

<p align="center">
  <img src="./docs/_static/screenshot-team-serve.png" alt="One task fans out to claude and codex in sealed sandboxes; they peer-review each other in a continuous loop; a neutral verifier replays and tests every candidate; the one verified result is merged back into your repo." width="95%">
</p>

---


## What h5i is, and is not

- h5i **is not** a Git replacement, a hosted SaaS / dev-environment, or *just* a sandbox.
- h5i **is** a Git sidecar for **auditable agent ensembles**: run many agents, merge one provable result.

**Why not a hosted sandbox?**: The whole point is that the workspace and its evidence live *in your repo* (`refs/h5i/*`): pushable, fetchable, offline, and yours. Codespaces, Coder, and E2B give you an environment; h5i gives you an *auditable* one, versioned in Git with no service to depend on.

**Why naive agent teams break**: In ML, ensembles beat the best single model: diverse estimators cut variance and won a decade of competitions. The same shift is coming to coding agents. But spawn several agents on one repo with **no coordination layer** and you don't get an ensemble, you get a pileup:

| Failure mode | What happens | h5i's answer |
|---|---|---|
| **Environment conflict** | agents overwrite each other's files and may run destructive commands | a confined worktree + policy per agent (`h5i env`) |
| **Token explosion** | every agent re-reads the repo and drags raw logs into context | compressed tool logs (`h5i capture run`, ~95% less) |
| **Review overload** | humans can't inspect every prompt or command | reviewer-ready PR (`h5i share pr`) |

---

## Documentation

- [Official Website](https://h5i.dev/): project overview, [Pitch Deck](https://h5i.dev/pitch/)
- [Tutorials](https://h5i.dev/guides/): guided workflows · [Blog](https://h5i.dev/blog/): design notes, audits, case studies
- [MANUAL.md](MANUAL.md) / `man h5i`: full command reference
- [CONTRIBUTING.md](CONTRIBUTING.md): we welcomes contributions of any kind.

---

## Acknowledgements

h5i's token-reduction filters build on prior art, both Apache-2.0:

- **[rtk](https://github.com/rtk-ai/rtk)**: the declarative output-filter rule files and the engine that runs them are derived from rtk.
- **[headroom](https://github.com/chopratejas/headroom)**: the log line-folding technique (collapse near-identical lines into one with a count) is reimplemented from headroom.

See [`NOTICE`](NOTICE) and [`assets/filters/NOTICE`](assets/filters/NOTICE) for full attribution.

## License

Apache-2.0. See [LICENSE](LICENSE).
