# h5i

> **Version control for the age of AI-generated code — including the reasoning behind it.**

<p align="center">
  <a href="https://github.com/Koukyosyumei/h5i" target="_blank">
      <img src="./assets/logo.svg" alt="h5i Logo" height="126">
  </a>
</p>

![example workflow](https://github.com/h5i-dev/h5i/actions/workflows/test.yaml/badge.svg)
![Apache-2.0](https://img.shields.io/github/license/h5i-dev/h5i?color=blue)

`h5i` (pronounced *high-five*) is a Git sidecar that answers the questions Git can't: *Who prompted this change? What did the AI skip or defer? What was it thinking, and can we safely resume where it left off?*

It integrates with **Claude Code** and **Codex** out of the box — every prompt, decision, and file touch is captured as a first-class artifact alongside your commits.

```bash
curl -fsSL https://raw.githubusercontent.com/Koukyosyumei/h5i/main/install.sh | sh
```

> **Or build from source:** `cargo install --git https://github.com/Koukyosyumei/h5i h5i-core`

<p align="center">
      <img src="./assets/screenshot_h5i_dag.png" alt="Context DAG" width="90%">
</p>

---

## Why h5i

Five commands do most of the work:

- **`h5i context`** — records the goal, milestones, and every OBSERVE / THINK / ACT step of an agent session as first-class git objects. Pick up where the last session stopped, hand a task off from **Claude Code** to **Codex** without losing the thread, and `git diff` your own reasoning.
- **`h5i claims`** — attach short, content-addressed facts (e.g. *"HTTP helpers live only in `src/api/client.py`"*) to the files that back them. Live claims are injected into future agent sessions, cutting cache-read tokens by **~69%** at N=10.
- **`h5i summary`** — pin a per-file orientation (exports, role, structure) keyed by the file's blob OID, auto-invalidated on edit. Eagerly inlined into the prompt so the agent skips re-reading. **−46% cache-read tokens** at N=10 on the same benchmark.
- **`h5i notes`** — attaches each session's exploration footprint, uncertainty moments, and blind edits to the commit, then ranks the commits that most need human review.
- **`h5i vibe`** — a 5-second audit of any repo: AI footprint, directories that are fully AI-written, leaked API tokens, and prompt-injection hits. Useful on any codebase you inherit.

---

## Quick start

The whole workflow is five steps. Once hooks are installed, h5i runs silently — your normal `claude` or `codex` session is automatically captured.

### 1. Initialize

```bash
cd your-project
h5i init
```

This creates `.git/.h5i/` (AST snapshots, metadata, CRDT deltas) and drops the h5i usage rules into the agent instruction files your tools already look for:

- `.claude/h5i.md` + a `@.claude/h5i.md` import appended to `CLAUDE.md` (so Claude Code picks it up on load)
- an `h5i` section appended to `AGENTS.md` (so Codex picks it up on load)

Both are append-only — any existing content in `CLAUDE.md` / `AGENTS.md` is preserved. Nothing about your normal Git workflow changes.

### 2. Wire up your agent

**Claude Code** — one command prints everything you need (hooks + MCP server):

```bash
h5i hook setup
```

Paste the printed `hooks` and `mcpServers` blocks into `~/.claude/settings.json`. You get four integrations:

| Hook | What it does |
|---|---|
| `SessionStart` | Injects prior goal, milestones, and last decisions into every new session |
| `UserPromptSubmit` | Captures the user prompt so commits record it without `--prompt` |
| `PostToolUse` | Emits an OBSERVE/ACT trace entry for every Read/Edit/Write |
| `Stop` | Auto-checkpoints the context workspace when the session ends |

The MCP server gives Claude native access to `h5i_log`, `h5i_blame`, `h5i_context_trace`, and friends — no shell gymnastics.

**Codex** — `h5i init` already wrote the relevant `AGENTS.md` section. If you want to drive it manually:

```bash
h5i codex prelude                    # print shared context at session start
h5i codex sync                       # backfill OBSERVE/ACT traces during the session
h5i codex finish --summary "…"       # sync + checkpoint at the end
```

### 3. Code normally

Use Claude Code or Codex as you would anyway. h5i records reasoning in the background:

```
[h5i] Context workspace active — prior reasoning follows.

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

When you commit, use `h5i commit` in place of `git commit` so provenance gets attached:

```bash
h5i commit -m "switch session store to Redis" \
  --model claude-sonnet-4-6 --agent claude-code \
  --prompt "sessions need to survive process restarts"
```

With hooks installed, `--prompt` is inferred automatically.

### 4. Pin reusable facts with `h5i claims`

When an agent figures out something a future session will need — where a helper lives, which module owns a concern, a non-obvious invariant — pin it as a claim so the next session doesn't re-derive it from scratch:

```bash
h5i claims add "HTTP helpers live only in src/api/client.py" \
  --path src/api/client.py --path src/middleware.rs
```

Claims are content-addressed over `(path, blob_oid)` pairs. If any referenced file changes, the claim auto-invalidates — stale guidance never leaks into the next session. Live claims are injected into `h5i context prompt`. See [Cutting token cost](#cutting-token-cost) for measured impact (−69% cache-read tokens at N=10).

### 5. Share with your team

h5i metadata lives in dedicated Git refs (see [Under the hood](#under-the-hood)) and is **not** part of a plain `git push`. Sync it in one shot:

```bash
h5i push         # pushes notes + context + memory refs to origin
h5i pull         # the reverse
```

---

## Reviewing AI-assisted work

Three commands cover the audit loop — from a 5-second footprint check to a full compliance report.

### `h5i vibe` — instant AI footprint

```bash
h5i vibe
```

How much of the repo is AI-generated, which directories are fully AI-written, and which files are the riskiest. Use this on any repo you inherit.

### `h5i context scan` — prompt-injection signals

```bash
h5i context scan
```

```
── h5i context scan ────────────────────────────── main
  risk score  1.00  ██████████  (48 lines scanned, 2 hit(s))

  HIGH line 31  [override_instructions]  ignore all previous instructions
  HIGH line 31  [exfiltration_attempt]   reveal the system prompt
```

Eight regex rules cover role hijacking, instruction overrides, credential exfiltration, delimiter escapes, and more — each with a 0.0–1.0 risk score.

### `h5i compliance` — audit-grade report

```bash
h5i compliance --since 2026-01-01 --until 2026-03-31 --format html --output audit.html
```

Covers AI provenance coverage, missing prompts, policy violations, and flagged prompt-injection hits across any date range. Text / JSON / HTML output.

### `h5i notes review` — commits most in need of human eyes

```bash
h5i notes review --limit 50
```

Ranks commits by uncertainty signals, blind edits (files modified without being read), churn, and scope — so review effort goes where it matters.

---

## Cutting token cost

Two pre-loadable artifacts attack the same loss from different angles. Agents pay tokens to re-derive things every session. Both `h5i claims` and `h5i summary` pin known content into the cached prompt prefix so the next session skips the re-grounding.

### `h5i claims` — content-addressed facts that auto-invalidate

`h5i claims` records cross-file conclusions with their evidence pinned as a Merkle hash over `(path, blob_oid)` pairs at HEAD. The claim stays `live` until any evidence blob changes, then auto-invalidates.

```bash
h5i claims add "HTTP helpers live only in src/api/client.py" \
  --path src/api/client.py

h5i claims list       # live / stale badges
h5i claims prune      # drop claims whose evidence changed
```

Write claims **caveman-style** (≈30 tokens, drop articles + copulas, keep paths/identifiers exact). The text gets re-read on every cached-prefix turn forever, so brevity at write time pays back forever.

### `h5i summary` — per-file orientations keyed by blob OID

Where a claim is a fact pinned to multiple files, a summary is a precis of *one* file's content keyed by that file's git blob OID. Because git blobs are immutable, a summary written for blob X is correct for blob X forever — and it auto-invalidates the moment the file is edited (HEAD points at a new blob with no summary). Summaries are eagerly inlined into the prompt prelude when the count fits a budget, so the agent uses them without an extra fetch round-trip.

```bash
h5i summary set src/api/client.py --text \
  "HTTP client. requests to api.example.com. Exports: fetch_user(id)→dict, create_post(...), delete_post(id)→bool. Logger \`log\` at top."

h5i summary list      # which HEAD files have summaries; which don't
h5i summary show <path>
h5i summary prune     # drop summaries whose blob OID is no longer reachable from HEAD
```

### Measured impact

Controlled A/B/C at N=10 trials per arm (`./scripts/experiment_claims.sh`), single model `claude-opus-4-7`, MCP server mounted, fidelity **10/10 in every arm**:

| Metric              | No claims/summaries (mean ± sd) | With claims (mean ± sd) |  Δ_claims | With file summaries (mean ± sd) |  Δ_summaries |
|---------------------|--------------------------------:|------------------------:|----------:|--------------------------------:|-------------:|
| Cache-read tokens   |              528,136 ± 101,765  |     165,722 ± 105,423   | **−68.6%** |              283,174 ± 113,206  |   **−46.4%** |
| Read tool calls     |                       5.2 ± 1.1 |               1.0 ± 0   |     −80.8% |                          1.0 ± 0 |       −80.8% |
| Assistant turns     |                      16.5 ± 2.8 |               6.1 ± 3.2 |     −63.0% |                        9.9 ± 3.5 |       −40.0% |
| Wall time           |                       46 ± 15 s |              20 ± 7 s   |     −55.6% |                         35 ± 21 s |       −23.1% |
| Fidelity (success)  |                         10/10 ✓ |                10/10 ✓  |           |                          10/10 ✓ |              |

Both treated arms read **exactly one file** in every trial — the file the agent edits — vs ~5 in the no-artifact baseline. The cache-read deltas exceed `2·max(stdev)` for both arms, so neither is noise.

The full methodology, all four arms (including a `freq=high` arm that quantifies the cost of letting the agent self-record), raw per-trial data, lessons-learned (caveman compression, eager rendering), and honest caveats live in [`scripts/experiment_claims_results.md`](scripts/experiment_claims_results.md).

---

## Under the hood

h5i is a pure Git sidecar: it stores everything in the same repo, using dedicated refs so it never pollutes your working tree or branch graph.

| Ref | What lives there |
|---|---|
| `refs/h5i/notes` | Per-commit metadata — model, agent, prompt, tokens, test metrics, decisions |
| `refs/h5i/context` | The reasoning workspace (goal, milestones, OBSERVE / THINK / ACT trace) as a DAG |
| `refs/h5i/ast` | AST snapshots used for structural blame and semantic merges |
| `refs/h5i/checkpoints/<agent>` | Per-agent memory snapshots (`~/.claude/` state, Codex state) |

Because these are first-class Git objects, everything you'd expect works: they're content-addressed, dedup'd, pushable, and survive `git gc`. `h5i push` / `h5i pull` is a thin wrapper over `git push`/`fetch` targeting those refspecs.

```bash
git for-each-ref refs/h5i/     # peek at what h5i has stored
```

---

## Other things h5i does

- **`h5i log`** — enriched commit history with prompts, models, tokens, and decisions inline.
- **`h5i blame <file>`** — line or AST-level blame, annotated with AI provenance per commit.
- **`h5i policy`** — policy-as-code (`.h5i/policy.toml`): require provenance, cap AI ratio per directory, enforce audit on sensitive paths.
- **`h5i claims`** — record content-addressed facts about the codebase that auto-invalidate when their evidence blobs change; injects live ones into `h5i context prompt`.
- **`h5i summary`** — pin a per-file orientation keyed by the file's git blob OID; eagerly inlined into the prompt so agents skip re-reading. Auto-invalidates on edit.
- **`h5i memory`** — snapshot / diff / restore Claude or Codex memory state alongside the code.
- **`h5i resume`** — one-screen session-handoff briefing (last branch, high-risk files, suggested opening prompt).
- **`h5i context restore <sha>`** — time-travel the reasoning workspace to any past commit.
- **`h5i rollback` / `h5i rewind`** — revert the AI commit whose *intent* best matches a description, or restore the tree to a past commit.

See [MANUAL.md](MANUAL.md) for the full command reference — every flag, integrity rule, MCP tool, and dashboard feature.

---

## Web dashboard

```bash
h5i serve        # opens http://localhost:7150
```

<img src="./assets/screenshot_h5i_server.png" alt="h5i web dashboard — Timeline tab">

**Timeline** shows every commit with its full AI context inline: model, agent, prompt, test badge, and a one-click **Re-audit** button. **Sessions** visualizes footprint, uncertainty heatmap, and churn per commit.

---

## License

Apache 2.0 — see [LICENSE](LICENSE).
