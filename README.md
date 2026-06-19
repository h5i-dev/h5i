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
  <a href="#confine-the-workspace-h5i-env"><img alt="Sandboxed Workspace" src="https://img.shields.io/badge/Sandboxed%20Workspace-confined%20%26%20provable-D21C1C"></a>
  <a href="#review-the-evidence-h5i-share-pr"><img alt="Review-ready PR" src="https://img.shields.io/badge/PR%20Evidence-reviewer%20ready-0969da"></a>
  <a href="#coordinate-agents-h5i-msg"><img alt="Agent Handoffs" src="https://img.shields.io/badge/Agent%20Handoffs-Claude%20%E2%86%94%20Codex-6f42c1"></a>
  <a href="#compress-the-logs-h5i-capture-run"><img alt="Compressed Logs" src="https://img.shields.io/badge/Compressed%20Logs-95%25%20less%20token%20waste-2ea44f"></a>
</p>

**Git tracks the diff. h5i tracks the run.**

AI coding agents do more than edit files. They follow prompts, talk to other agents, run tests, inspect logs, retry commands, skip failures, touch the filesystem, and make decisions that Git never records.

h5i gives each agent a **Git-backed workspace**: a sandboxed worktree, prompt-aware commits, compressed command logs, agent-to-agent handoffs, and PR-ready evidence, all stored in `refs/h5i/*`, traveling with the repo.


> **What is an auditable workspace?** It's the place an AI agent does its work, a Git-backed worktree where every prompt, context, command, log, policy, and multi-AI conversation is recorded in your repo.
>
> **Auditable Workspace = worktree + prompt + model + commands + logs + policy + messages + PR evidence.**

```text
Agent Workspace
├─ Sandboxed worktree        h5i env
├─ Prompt-aware commits      h5i capture commit
├─ Compressed tool logs      h5i capture run
├─ Agent handoffs            h5i msg
├─ Risk/audit signals        h5i audit
└─ PR evidence brief         h5i share pr
```

<table align="center">
  <tr>
    <td align="center">
      <strong>Sandboxed worktrees</strong><br>
      <sub>Run agents off your main tree</sub>
    </td>
    <td align="center">
      <strong>95% lower token waste</strong><br>
      <sub>Compact summaries, raw logs kept</sub>
    </td>
    <td align="center">
      <strong>Prompt-aware commits</strong><br>
      <sub>Prompt · model · tests · decisions</sub>
    </td>
    <td align="center">
      <strong>Review-ready PRs</strong><br>
      <sub>Provenance, risks, handoffs in one brief</sub>
    </td>
  </tr>
</table>

### Why this matters

| Without h5i | With h5i |
|---|---|
| prompts live in chat | every agent run leaves Git-backed evidence |
| terminal logs scroll past and disappear | raw logs are recoverable without filling the context window |
| test evidence is hard to recover | prompt, model, tests, and decisions ride with each commit |
| risky agent runs touch your main tree | risky work happens in a disposable, confined sandboxed worktree |
| the reviewer only sees the diff | PRs carry provenance, prompt quality, tests, risks, and agent handoffs |

**Who it's for:** platform, security, and DevEx leads at orgs rolling out Claude Code and Codex who need PR review and audit to stay defensible as agents write more of the diff.

### Recent News

- **New: Sandboxed agent workspaces (`h5i env`).** Hand an agent a disposable, confined worktree, prove what it couldn't reach, and apply the result only after review. [Jump to the sandboxed workspace ↓](#confine-the-workspace-h5i-env)
- **New: Prompt Maturity Score.** Every AI commit's prompt gets a deterministic, fully-offline **0–100** quality signal, rolled up across the branch and rendered in the PR evidence. [Jump to prompt-aware commits ↓](#record-the-work-h5i-capture-commit)
- **New: Compressed tool logs.** Agents see a compact summary while the full output stays out of context, recoverable via Git LFS. [Jump to compressed logs ↓](#compress-the-logs-h5i-capture-run)
- **Agent Radio reached 100+ points on Hacker News.** Read the discussion [here](https://news.ycombinator.com/item?id=48345837).
- **New: Agent Radio.** Since your agents' context already lives in Git, they can now talk to each other through it. h5i msg adds a cross-agent message channel stored in refs/h5i/msg. [Jump to Agent Radio ↓](#multi-ai-conversation-h5i-msg)

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

Initialize h5i in an existing Git repo:

```bash
h5i init
```

For Claude Code or Codex hooks:

```bash
h5i hook setup --write --wrap-bash    # writes .claude/settings.json and .codex/config.toml
```

Post the PR review brief:

```bash
h5i share pr post         # upsert sticky PR comment
```

`h5i share pr post` requires the GitHub CLI (`gh`) to be installed and authenticated
(`gh auth status` clean). Use `h5i share pr body` when CI should render markdown
without posting through `gh`.

Sync h5i sidecar refs with teammates:

```bash
h5i share push
h5i share pull
```

---

## 3. What an auditable workspace gives you

Five capabilities, each a property of the workspace and each writing to its own `refs/h5i/*` ref. The sandboxed worktree is where the work happens; everything else is the evidence it accumulates, converging on one reviewer-ready artifact, the PR.

| | Capability | Command |
|---|---|---|
| 1 | **Confine the workspace** | `h5i env` |
| 2 | **Review the evidence** | `h5i share pr` |
| 3 | **Record the work** | `h5i capture commit` |
| 4 | **Reduce token consumption** | `h5i capture run` |
| 5 | **Coordinate multi-AI conversation** | `h5i msg` |

### Confine the workspace: `h5i env`

`h5i env` gives you a disposable, confined **workspace**, a git worktree plus a policy that limits what the code inside can read, write, and reach over the network, so you can run a refactor, a dependency upgrade, or an untrusted build (yourself or via an agent) without it touching your main tree. This is the canonical auditable workspace, and the command humans actually run. Your loop is four commands:

```bash
h5i env create env-name --profile agent-claude  # make a confined box (picks the strongest isolation the host supports)
# h5i env create env-name --profile agent-codex 
h5i env shell  env-name                         # work inside it — or hand the box to an agent
h5i env propose env-name                        # mediated commit + review brief
h5i env apply  fix-auth                         # merge it into your branch (only when you choose to)
```

Inside the box, **every command is confined**: reading `/etc/shadow`, opening a raw socket, reaching a host that isn't on the allowlist, or calling `mount` / `unshare` / `ptrace` is blocked by Landlock + seccomp + namespaces, while normal work runs and is recorded. (`h5i env run <name> -- <cmd>` runs a single confined command the same way.) *What this proves: the agent physically could not exfiltrate, not "we logged it."*

**Everything is auditable after the fact.** `h5i env log` lists every command run, secret used, and access blocked; `h5i env inspect --capture <id>` shows one run's record, its output, exit code, the exact policy that was enforced, and any redacted secrets; `h5i env compare` lines up parallel attempts; and the Sandbox tab of the [web dashboard](#see-the-reasoning-context-dag--dashboard) shows every allowed and blocked action. `h5i env propose` writes the change up for review first, nothing reaches your branch until you `apply`. The whole record lives in `refs/h5i/env` and moves between clones with `h5i share push` / `pull`.

`h5i env create` picks the strongest isolation level the host can actually enforce (`h5i env probe` shows what that is). If you ask for a level the host can't provide, h5i refuses rather than quietly running with less:

| Tier | Confinement |
|------|-------------|
| `workspace` | git worktree only, trusted code |
| `process` | Landlock + seccomp deny-list + user/net namespaces + cgroup v2 (rootless) |
| `supervised` | process tier + a live seccomp-notify socket gate (default-deny sockets) **plus a real L3/L4 `net.egress` allowlist**, slirp4netns uplink + nftables default-drop + `/etc/hosts` DNS pinning |
| `container` | rootless Podman + a DNS-pinned **`net.egress` domain allowlist** (L7) |

<p align="center">
  <img src="./assets/agent-sandbox.svg" alt="An agent runs cargo build via h5i env run inside a policy-confined sandbox; reads of /etc/shadow are blocked by Landlock, raw sockets and off-allowlist hosts by the seccomp gate and egress proxy, and mount/unshare/ptrace by the seccomp deny-list, while the legitimate build is allowed and captured as evidence that a reviewer applies via propose → apply." width="95%">
</p>

### Review the evidence: `h5i share pr`

When a branch is ready for review, h5i surfaces all of it where reviewers already work, on the pull request. One command posts a sticky, idempotent brief: review focus, goal, reasoning, tests, prompt maturity, and risk flags, all in one comment. *What this proves: the reviewer reads the provable record of the run, not just the diff.*

<table>
<tr>

<td width="38%" valign="top">

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
**📊 Prompt Maturity**

The offline 0–100 score for the prompts behind this branch, with the per-signal breakdown.

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

The prompt, model names, and commit lineage.

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

### Record the work: `h5i capture commit`

`h5i capture commit` makes every commit **prompt-aware**: it records the prompt, model, agent, tests, and decisions behind the change as Git-backed provenance in `refs/h5i/notes`, so the workspace remembers *who/why/what the agent knew*. *What this proves: the provenance behind the diff, recoverable months later.*

On top of that provenance, the **Prompt Maturity Score** turns the recorded prompts into a single, explainable **0–100** quality signal of *how well the work was delegated*, computed **fully offline**: no LLM, no network, deterministic enough to run in a Git hook, in CI, or in a PR render with no API key.

```text
🌳 Prompt maturity: 81/100 · advanced · 7 prompts scored (100% of AI commits)
🔧 Recurring weak spots: weak context.
   Heuristic signal of prompt craft — not a developer rating.

  Specificity                 ████████░░  0.82
  Control / acceptance        ████████░░  0.79
  Context grounding           ████░░░░░░  0.41
  Structure                   ███████░░░  0.68
  Lexical diversity           ███████░░░  0.71
  Clarity (readability band)  ████████░░  0.80
  Length adequacy             ██████████  1.00
```

Prompt quality becomes **inspectable, trendable, and reviewable** across commits, a simple way for a team to see and improve how it delegates work to agents, without sending a single prompt to another model. (Design write-up: [How to Measure Prompt Quality Offline](https://h5i.dev/blog/prompt-maturity-score/).)

### Multi-AI Conversation: `h5i msg`

Because the workspace already lives in Git, your agents can also **hand off to each other through it**: `h5i msg` (a.k.a. *Agent Radio*) is a Git-backed cross-agent message channel stored in `refs/h5i/msg`, built for typed operational handoffs (`ASK` · `REVIEW_REQUEST` · `RISK` · `DONE` · `ACK`). Claude can ask, Codex can review, risks can be flagged and resolved, and the whole log survives clones, machines, and branches. It travels with `h5i share push` / `pull`, and divergent sends from two machines **union-merge with no messages lost**. *What this proves: the workspace is the shared, auditable substrate for Claude ↔ Codex.*

To efficiently use `h5i msg`, first register some hookups for agents: 

```bash
h5i msg setup
```

Then, we’re ready to let Claude and Codex communicate with each other in real time. Open two separate terminals, launch Claude Code and Codex, and give instructions to them.

**Example Instructions**

- Claude: `Can you play Chess with Codex via h5i`
- Codex: `Can you play Chess with Claude via h5i`

We can also monitor the conversation in real time with `h5i msg watch`. 

<p align="center">
  <img src="./assets/claude-codex-chess.gif" alt="h5i msg watch, a live claude ↔ codex code review streaming over refs/h5i/msg" width="95%">
</p>

For more details, see this blog post: [Claude Code and Codex Can Have Real-Time Conversation via Git](https://medium.com/@Koukyosyumei/claude-code-and-codex-can-have-real-time-conversation-via-git-f95b696c1c05)

### Compress the logs: `h5i capture run`

Wrap any command with `h5i capture run -- <cmd>` and the agent sees only a compact, normalized summary of errors, failures, and counts, while the full raw output is stored out of band in `refs/h5i/objects`. Every tool's output collapses into **one unified form**, so a 4 MB test log no longer burns your context window, and the raw bytes are always one `h5i recall object <id>` away when you need them. *What this proves: the workspace keeps raw logs recoverable without filling the context window, up to 95% less token waste.*

```bash
# One Schema for Every Tool
tool: pytest
kind: test            # test | lint | typecheck | build | vcs | generic
status: failed        # passed | ok | failed | error | unknown
exit_code: 1
counts: { failed: 1, passed: 120 }
parser_confidence: parsed     # parsed | heuristic | generic
raw_oid: sha256:934f…         # the full output, always recoverable
findings:
  - kind: test_failure        # test_failure | diagnostic | build_error | panic | generic
    severity: failure
    id: tests/test_auth.py::test_refresh
    message: assert 0 == 100
    location: tests/test_auth.py:42
    fingerprint: 0bb827e4e61a  # stable across line shifts → dedupe / track
```

To share captures across a team, h5i borrows the split that Git LFS uses: the manifest in `refs/h5i/objects` is a lightweight pointer (it carries the raw output's `sha256`), while the bytes themselves ride on a native Git LFS backend, so huge tool output never bloats the Git object database. For remotes that are not HTTP, it transparently falls back to a git ref store.

<p align="center">
  <img src="./assets/token-reduction-unified.svg" alt="h5i recall object" width="95%">
</p>

### See the reasoning: context DAG & dashboard

The context DAG shows how the work unfolded: the goal, every milestone, and the OBSERVE / THINK / ACT trace behind each change, captured automatically as the agent works. Because it is snapshotted on every commit, you can replay exactly what an agent knew and why it acted at any point in history.

```bash
h5i recall context show
```

<p align="center">
  <img src="./assets/screenshot_h5i_dag.png" alt="h5i context DAG view" width="95%">
</p>

The web dashboard renders the whole workspace, commit timeline, provenance, sandbox actions, and context, in one place:

```bash
h5i serve        # http://localhost:7150
```

<p align="center">
  <img src="./assets/screenshot_h5i_server.png" alt="h5i web dashboard showing AI commit timeline and context details" width="95%">
</p>

---

## 4. What makes a workspace auditable: `refs/h5i/*`

h5i is a pure Git sidecar. The workspace and all of its evidence live in dedicated refs, so they don’t pollute your working tree or your normal branch graph, and they travel with the repo. `refs/h5i/env` is the workspace itself; the others are the evidence it accumulates.

| Ref | What lives there |
|---|---|
| `.git/refs/h5i/env` | **The workspace itself**, sandbox events, manifests, and digest-pinned policies for isolated, confined agent runs. |
| `.git/refs/h5i/notes` | Per-commit provenance: model, agent, prompt, tests, decisions, risk signals. |
| `.git/refs/h5i/context` | The reasoning DAG: goal, milestones, traces, branches. |
| `.git/refs/h5i/msg` | Cross-agent handoff log (append-only, union-merged on pull). |
| `.git/refs/h5i/objects` | Compressed-log capture manifests: command, exit code, and filtered summary of large outputs (full raw kept locally). |
| `.git/refs/h5i/checkpoints/<agent>` | Per-agent memory snapshots. |

Because these are Git objects, they are content-addressed, deduplicated, pushable, fetchable, and survive `git gc`. **Lives in your Git, travels with the repo, no SaaS, no lock-in, works offline.**

<p align="center">
  <img src="./assets/h5i-concept.svg" alt="h5i context DAG view" width="95%">
</p>

### What h5i is: and is not

- h5i **is not** a Git replacement.
- h5i **is not** a hosted SaaS or dev-environment.
- h5i **is not** just a sandbox.
- h5i **is** a Git sidecar for auditable agent workspaces.

**Why not just a hosted sandbox?** Because the whole point is that the workspace and its evidence live *in your repo* (`refs/h5i/*`), pushable, fetchable, offline, and yours. Codespaces, Coder, and E2B give you an environment; h5i gives you an *auditable* one, versioned in Git with no service to depend on.

---

## 5. Documentation

- [Official Website](https://h5i.dev/) - project overview
- [Tutorials](https://h5i.dev/guides/) - guided workflows
- [Blog](https://h5i.dev/blog/) - design notes, audits, and case studies

---

## 6. Contributing

High-impact contributions:

- try h5i on a real AI-assisted repo and file issues with confusing moments
- improve PR-body presentation and GitHub reviewer workflows
- add adapters for more test runners and agent tools
- harden prompt-injection and compliance rules
- improve dashboard workflows for reviewers

If the idea matters to you, starring the repo is the fastest way to help more AI-heavy teams find it.

---

## 7. Acknowledgements

h5i's token-reduction filters build on prior art, both Apache-2.0:

- **[rtk](https://github.com/rtk-ai/rtk)**, the declarative
  output-filter rule files and the engine that runs them are derived from rtk.
- **[headroom](https://github.com/chopratejas/headroom)**, the log line-folding
  technique (collapse near-identical lines into one with a count) is reimplemented
  from headroom.

See [`NOTICE`](NOTICE) and [`assets/filters/NOTICE`](assets/filters/NOTICE) for full attribution.

## 8. License

Apache-2.0. See [LICENSE](LICENSE).
