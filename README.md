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

<h1 align="center">Auditable Workspaces for AI Coding Agents</h1>

**h5i** (pronounced *high-five*) gives every AI coding agent a sandboxed Git worktree, and records the prompts, commands, logs, policies, and reviews behind every change. Run one agent safely, scale to many via a conflict-free multi-agent orchestra, then merge one auditable result. It all lives in your repo, carried by Git, with no SaaS.

- Prompt versioning and quality assessment
- Persistent context/memory
- Supervised sandboxed environment
- Token reduction up to 95%
- Programmable and conflict-free multi-agent orchestration
- Fully-automated audit of AI-generated code

<a href="https://trendshift.io/repositories/46160?utm_source=trendshift-badge&amp;utm_medium=badge&amp;utm_campaign=badge-trendshift-46160" target="_blank" rel="noopener noreferrer"><img src="https://trendshift.io/api/badge/trendshift/repositories/46160/daily?language=Rust" alt="h5i-dev%2Fh5i | Trendshift" width="250" height="55"/></a>

> ***Two heads are better than one.***

<p align="center">
  <img src="./docs/_static/hero-team.svg" alt="One task fans out to claude and codex in sealed sandboxes; they peer-review each other in a continuous loop; a neutral verifier replays and tests every candidate; the one verified result is merged back into your repo." width="99%">
</p>

<table align="center">
  <tr>
    <td align="center"><strong>Isolated per agent</strong><br><sub>no file, branch, or port clashes</sub></td>
    <td align="center"><strong>Auto peer-review</strong><br><sub>cross-agent discussion</sub></td>
    <td align="center"><strong>Rich dashboard</strong><br><sub>diffs, reviews, results</sub></td>
    <td align="center"><strong>Lives in your Git</strong><br><sub>refs/h5i/* · no SaaS</sub></td>
  </tr>
</table>

**Who it's for:** platform, security, and DevEx leads rolling out Claude Code and Codex who want to run *teams* of agents and keep review and audit defensible as agents write more of the diff.

---

## 1. Install

```bash
curl -fsSL https://raw.githubusercontent.com/h5i-dev/h5i/main/install.sh | sh
```

Or build from source:

```bash
cargo install --git https://github.com/h5i-dev/h5i h5i-core
```

---

## 2. 60-Second Flow

### 2.1. Setup

Initialize h5i and wire the Claude Code / Codex hooks:

```bash
h5i init
h5i hook setup --write --wrap-bash --team
git add .
git commit -m "update hooks"
```

### 2.2. Track Prompts and Contexts

Once the hooks are registered, h5i versions your human prompts and every agent context step (reads, writes, thinking) as Git objects, trimming noisy tool output along the way (for `pytest`, just the failures) to cut up to 95% of the tokens while keeping the raw output recoverable. 

```bash
h5i recall context show   # replay the captured prompts and agent context steps
```


Share it with `h5i share push`, or post an AI-usage summary (prompt quality, AI/human commit ratio, secret leaks, prompt injection, and more) to the pull request with `h5i share pr post` (needs the `gh` CLI).

```bash
h5i share push      # push the h5i metadata (refs/h5i/*) to your teammates
h5i share pr post   # post the AI-usage summary to the pull request (needs `gh`)
```

### 2.3. Sandboxed Environment

h5i gives each agent a secure, sandboxed worktree. Let it run with permissions
off inside the box, then review its diff before anything lands on your branch:

```bash
h5i env create claude-env --profile agent-claude
h5i env shell claude-env
box$ claude --dangerously-skip-permissions
box$ exit

h5i env diff claude-env      # review what the agent changed in the box
h5i env propose claude-env   # turn the box's work into a reviewable proposal
h5i env apply claude-env     # merge the reviewed changes onto your branch
```

### 2.4. Programmable Multi-Agent Orchestration

You can further **program** flexible multi-agent workflows using ordinary control flow such as parallel execution, loops, and conditionals in Rust or Python SDK. For example, you can have Claude and Codex independently implement the same task, review and improve each other’s work, and then select the better result.

```python
from h5i.orchestra import Conductor

async def main():
    task = "implement quicksort in python with unit test"

    async with Conductor(".", "fix-auth") as c:
        claude = await c.hire("claude-agent", runtime="claude")
        codex  = await c.hire("codex-agent",  runtime="codex")

        # Have both agents implement the task independently and in parallel
        claude_work, codex_work = await asyncio.gather(claude.work(task), codex.work(task))

        await c.freeze() # Seal the round, ensuring that neither agent influenced the other beforehand

        # Have each agent review the other's work
        await asyncio.gather(codex.review(claude_work), claude.review(codex_work))

        # Verify each submission in a fresh, neutral sandbox
        await c.verify(claude_work, ["pytest", "--quiet"])
        await c.verify(codex_work, ["pytest", "--quiet"])

        verdict = await c.judge() # Select the smallest diff among the submissions that pass all tests
        print("winner:", verdict.selected_submission)

asyncio.run(main())
```

### 2.5. Web UI

Monitor the status:

```bash
h5i serve
```

<p align="center">
  <img src="./docs/_static/screenshot-team-serve.png" alt="One task fans out to claude and codex in sealed sandboxes; they peer-review each other in a continuous loop; a neutral verifier replays and tests every candidate; the one verified result is merged back into your repo." width="95%">
</p>

---

## 4. Documentation

- [Official Website](https://h5i.dev/): project overview, [Pitch Deck](https://h5i.dev/pitch/)
- [Tutorials](https://h5i.dev/guides/): guided workflows · [Blog](https://h5i.dev/blog/): design notes, audits, case studies
- [MANUAL.md](MANUAL.md) / `man h5i`: full command reference
- [CONTRIBUTING.md](CONTRIBUTING.md): we welcome contributions of any kind.
- `h5i man > ~/.local/share/man/man1/h5i.1`: install the man page (generated from the CLI), then read it with `man h5i`.

---

## 5. Acknowledgements

h5i's token-reduction filters build on prior art, both Apache-2.0:

- **[rtk](https://github.com/rtk-ai/rtk)**: the declarative output-filter rule files and the engine that runs them are derived from rtk.
- **[headroom](https://github.com/chopratejas/headroom)**: the log line-folding technique (collapse near-identical lines into one with a count) is reimplemented from headroom.

See [`NOTICE`](NOTICE) and [`assets/filters/NOTICE`](assets/filters/NOTICE) for full attribution.

## 6. License

Apache-2.0. See [LICENSE](LICENSE).
