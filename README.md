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

**Who it's for:** platform, security, and DevEx leads rolling out Claude Code and Codex who want to run *teams* of agents and keep review and audit defensible as agents write more of the diff.

<table align="center">
  <tr>
    <td align="center"><strong>Isolated per agent</strong><br><sub>no file, branch, or port clashes</sub></td>
    <td align="center"><strong>Auto peer-review</strong><br><sub>cross-agent discussion</sub></td>
    <td align="center"><strong>Rich dashboard</strong><br><sub>diffs, reviews, results</sub></td>
    <td align="center"><strong>Lives in your Git</strong><br><sub>refs/h5i/* · no SaaS</sub></td>
  </tr>
</table>

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
h5i hook setup --write --wrap-bash # --team
git add .
git commit -m "update hooks"
```

### 2.2. Track Prompts and Contexts

Once the hooks are registered, h5i versions your human prompts and every agent context step (reads, writes, thinking) as Git objects, trimming noisy tool output along the way (for `pytest`, just the failures) to cut up to 95% of the tokens while keeping the raw output recoverable. 

- `h5i recall log`: replay the captured prompts - [example output](#recall-log)
- `h5i recall context show`: replay the agent context steps - [example output](#recall-context)
- `h5i audit review`: suggested Review Points - [example output](#audit-review)
- `h5i audit maturity`: measure the quality of prompts - [example output](#audit-maturity)
- `h5i share push` / `h5i share pull`: share the prompts, contexts. and all logs with other team members
- `h5i share pr post`: post an AI-usage summary (prompt quality, AI/human commit ratio, secret leaks, prompt injection, and more) to the pull request (needs the `gh` CLI)

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

### 2.4. Web UI

Monitor the status:

```bash
h5i serve
```

<p align="center">
  <img src="./docs/_static/h5i-serve-film.gif" alt="The h5i serve workbench in motion: an attention rail shows what needs you, the sandbox blocks a forbidden egress, a Why drawer opens with evidence badged by authority (enforced, verified, observed), a neutral verdict lands on the Board, and the Decide tab ranks candidates by merge confidence, prompt maturity, and risk." width="99%">
</p>

### 2.5. Programmable Multi-Agent Orchestration

You can further **program** flexible multi-agent workflows using ordinary control flow such as parallel execution, loops, and conditionals in Rust or [Python SDK](https://github.com/h5i-dev/h5i-python). For example, you can have Claude and Codex independently implement the same task, review and improve each other’s work, and then select the better result.

```bash
pip install h5i-orchestra
```

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

---

## 4. Documentation

- [Official Website](https://h5i.dev/): project overview, [Slides](https://h5i.dev/pitch/)
- [Tutorials](https://h5i.dev/guides/): guided workflows · [Blog](https://h5i.dev/blog/): design notes, audits, case studies
- [MANUAL.md](MANUAL.md) / `man h5i`: full command reference
- [CONTRIBUTING.md](CONTRIBUTING.md): we welcome contributions of any kind.
- `h5i man > ~/.local/share/man/man1/h5i.1`: install the man page (generated from the CLI), then read it with `man h5i`.

---

## 5. Gallary

<details>
<summary><a id="recall-log">Example output of <code>h5i recall log</code></a></summary>

```yaml
commit 9c76075822d743125587574e63bc1756866df496
Author:    Koukyosyumei <koukyosyumei@hotmail.com>
Agent:     claude-code (claude-fable-5)
Prompt:    "I guess you can remove the arXiv column, and just use hyperlink in Paper column to arviv website"
Message:   README: fold arXiv column into hyperlinked paper names
```
</details>

<details>
<summary><a id="recall-context">Example output of <code>h5i recall context show</code></a></summary>

```yaml
── Context (depth=2) ────────────────────────────────────
  Goal: add herdr support to h5i-python: launcher='herdr' (seats in herdr panes) + herdr  (branch: herdr-launcher)
  Milestones: (showing 20 most recent of 88; --limit 0 for all)
    ✔ [x] edited env.rs; edited env.rs; edited team.rs
    ✔ [x] Surveyed papers for h5i-python reference implementations
  Recent Trace:
    [00:00:37] ACT: edited blog/reimplementing-40-multi-agent-papers.md
    [00:01:04] NOTE: PLACEHOLDER (~/Dev/h5i-python/examples/README.md): iting 40 of these: the only paper mechanics that needed any workaround were self-review (forbidden by the engine, solved with a same-model second seat…
    [01:02:16] OBSERVE: read README.md
```
</details>

<details>
<summary><a id="audit-review">Example output of <code>h5i audit review</code></a></summary>

```yaml
  #1 3e744a3f  score 1.00  ██████████
     Floze · 2026-07-19 14:20 UTC
     docs: benchmark env isolation overhead (#355)
       ⬦ LARGE_DIFF          182 lines changed (>50)
       ⬦ UNTESTED_CHANGE     182 lines changed with no test metrics recorded
       ⬦ CODE_EXECUTION      Dangerous execution pattern 'subprocess.run()' added (line 13). Verify this is intentional and use --force to override.
```
</details>

<details>
<summary><a id="audit-maturity">Example output of <code>h5i audit maturity</code></a></summary>

```yaml
🧠 Prompt maturity: 42.7/100  🪴 developing
   coverage: 4/6 AI commits scored (67% coverage) · low confidence
   common flags: too short, weak context, no acceptance criteria
   Objective (core)   █████░░░░░ 0.54
   Grounding (core)   ██████░░░░ 0.60
   Direction (core)   ██████░░░░ 0.64
   Context            ██████░░░░ 0.60
   Examples           ░░░░░░░░░░ 0.00
   Structure          ██░░░░░░░░ 0.21
   Diversity          ████████░░ 0.84
   Clarity            █████████░ 0.94
   Adequacy           █████████░ 0.87
   Evidence (bonus)   ████████░░ 0.80 (+bonus)
```
</details>

## 6. Acknowledgements

h5i's token-reduction filters build on prior art, both Apache-2.0:

- **[rtk](https://github.com/rtk-ai/rtk)**: the declarative output-filter rule files and the engine that runs them are derived from rtk.
- **[headroom](https://github.com/chopratejas/headroom)**: the log line-folding technique (collapse near-identical lines into one with a count) is reimplemented from headroom.

See [`NOTICE`](NOTICE) and [`assets/filters/NOTICE`](assets/filters/NOTICE) for full attribution.

## 7. License

Apache-2.0. See [LICENSE](LICENSE).
