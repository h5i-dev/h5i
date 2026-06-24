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
  <img src="./docs/_static/screenshot_sandbox_h5i_3.png" alt="One task fans out to several agents working in sealed sandboxes; the agents peer-review each other; a neutral verifier replays each candidate; one verified result is applied." width="95%">
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

## Run an ensemble (60 seconds)

Initialize h5i and wire the Claude Code / Codex hooks (the `--team` hook keeps each agent alive through the peer-review round):

```bash
h5i init
h5i hook setup --write --wrap-bash --team
```

Create sandboxed environments:

```bash
h5i env create claude-env --profile agent-claude
h5i env create codex-env  --profile agent-codex
```

Create a team, then add each env to the roster (each agent gets an auto-generated id — `h5i team status` shows them):

```bash
h5i team create  team-name --base HEAD
h5i team add-env team-name env/human/claude-env --runtime claude
h5i team add-env team-name env/human/codex-env  --runtime codex
h5i team status  team-name                      # note the generated agent ids
```

Send the task to each agent:

```bash
echo "Implement Quick Sort from scratch in Python." > TASK.md
h5i team dispatch team-name --prompt-file TASK.md
```

Launch every agent in its own sandboxed environment (a terminal per env). Each agent automatically starts working on the given task:

```bash
h5i env shell env/human/claude-1 -- claude --dangerously-skip-permissions "$(h5i team bootstrap)"
```

```bash
h5i env shell env/human/codex-1 -- codex --sandbox danger-full-access "$(h5i team bootstrap)"
```

Each agent peer-reviews, and revises inside its own implementation:

```bash
h5i team auto-peer-review team-name              # sync → freeze → mutual grant → instruct
```

Replay each candidate, run the tests, merge the winner:

```bash
# h5i team sync     team-name                              # ingest agents' staged work (no relaunch)
# h5i team freeze   team-name                              # seal the independent attempts
h5i team verify   team-name --agent <agent-id> -- pytest   # id from `team status`
h5i team finalize team-name                                # explainable verdict (gates + smallest diff)
h5i team apply    team-name                                # merge the winner, gated on the verdict
```

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
