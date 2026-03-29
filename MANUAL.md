# h5i Manual

Complete reference for all h5i commands and configuration.

---

## Table of Contents

1. [Installation](#1-installation)
2. [Claude Code Setup (hooks)](#2-claude-code-setup-hooks)
3. [Committing with AI Provenance](#3-committing-with-ai-provenance)
4. [Session Analysis (h5i notes)](#4-session-analysis-h5i-notes)
5. [Context Workspace (h5i context)](#5-context-workspace-h5i-context)
6. [Session Handoff (h5i resume)](#6-session-handoff-h5i-resume)
7. [Enriched Log and Blame](#7-enriched-log-and-blame)
8. [Integrity Engine](#8-integrity-engine)
9. [Memory Management](#9-memory-management)
10. [Sharing with Your Team](#10-sharing-with-your-team)
11. [Web Dashboard](#11-web-dashboard)
12. [Storage Layout](#12-storage-layout)

---

## 1. Installation

Requires Rust 1.70+:

```bash
cargo install --git https://github.com/Koukyosyumei/h5i h5i-core
```

From a local clone:

```bash
git clone https://github.com/Koukyosyumei/h5i
cd h5i && cargo install --path .
```

Initialize h5i in any Git repository:

```bash
cd your-project
h5i init
# → h5i sidecar initialized at .git/.h5i
```

---

## 2. Claude Code Setup (hooks)

The fastest way to use h5i with Claude Code is to install the prompt-capture hook. After setup, every `h5i commit` automatically records the prompt from your Claude Code conversation — no `--prompt` flag required.

```bash
h5i hooks
```

This prints:
1. A shell script to save at `~/.claude/hooks/h5i-capture-prompt.sh`
2. The exact `~/.claude/settings.json` snippet to register it

Follow the printed instructions. Once installed, the flow is: conversation → `.git/.h5i/pending_context.json` → consumed and cleared by the next `h5i commit`.

**Environment variable fallback** (no hooks, or non-Claude agents):

```bash
export H5I_PROMPT="implement rate limiting on the auth endpoint"
export H5I_MODEL="claude-sonnet-4-6"
export H5I_AGENT_ID="claude-code"
h5i commit -m "add rate limiting"
```

---

## 3. Committing with AI Provenance

With hooks installed, a basic commit is just:

```bash
h5i commit -m "implement rate limiting"
```

h5i picks up the prompt, model, and agent from the hook-written context file automatically.

### All commit flags

| Flag | Env var | Description |
|------|---------|-------------|
| `--prompt` | `H5I_PROMPT` | The user prompt (auto-captured with hooks) |
| `--model` | `H5I_MODEL` | Model name, e.g. `claude-sonnet-4-6` |
| `--agent` | `H5I_AGENT_ID` | Agent identifier, e.g. `claude-code` |
| `--decisions <FILE>` | — | Path to a JSON file of design decisions (see below) |
| `--caused-by <OID>` | — | Commit that causally triggered this one (repeatable) |
| `--test-results <FILE>` | `H5I_TEST_RESULTS` | Path to a JSON test results file |
| `--test-cmd <CMD>` | — | Shell command whose stdout produces test results JSON |
| `--tests` | — | Scan staged files for inline `h5_i_test_start`/`h5_i_test_end` markers |
| `--ast` | — | Capture AST snapshot for semantic blame |
| `--audit` | — | Run integrity rules before committing (see §8) |
| `--force` | — | Commit despite warnings (violations still block) |

Flag resolution order: CLI flag → environment variable → pending context file (hook).

### Design decisions (`--decisions`)

Record the "why" alongside a commit — which alternatives were considered and why the chosen approach was preferred:

```bash
cat > decisions.json << 'EOF'
[
  {
    "location": "src/session.rs:44",
    "choice": "Redis over in-process HashMap",
    "alternatives": ["in-process HashMap", "Memcached"],
    "reason": "40 MB overhead is acceptable; survives restarts; required for horizontal scaling"
  }
]
EOF

h5i commit -m "switch session store to Redis" --decisions decisions.json
```

Decisions appear in `h5i log`:

```
Decisions:
  ◆ src/session.rs:44  Redis over in-process HashMap
    alternatives: in-process HashMap, Memcached
    40 MB overhead is acceptable; survives restarts; required for horizontal scaling
```

Decision schema: array of `{ "location", "choice", "alternatives"?, "reason" }`. `location` and `choice` are required.

### Causal chains (`--caused-by`)

Declare which earlier commit triggered the current one:

```bash
h5i commit -m "fix off-by-one in validate_token" --caused-by a3f9c2b
```

When rolling back a commit, h5i warns if later commits declared it as a cause:

```
⚠ Warning: 2 later commits causally depend on this one:
  → b2f3a1c "fix bug introduced by rate limiter"
Continue anyway? [y/N]
```

### Test adapters

**Option A — pre-computed file:**
```bash
python script/h5i-pytest-adapter.py > /tmp/results.json
h5i commit -m "add login tests" --test-results /tmp/results.json
```

**Option B — inline command:**
```bash
h5i commit -m "add login tests" --test-cmd "python script/h5i-pytest-adapter.py"
```

Bundled adapters: `script/h5i-pytest-adapter.py` (pytest), `script/h5i-cargo-test-adapter.sh` (cargo test).

Custom adapter schema — produce a JSON file and pass it via `--test-results`:

```json
{
  "tool": "jest", "passed": 42, "failed": 1, "skipped": 3,
  "total": 46, "duration_secs": 4.7, "exit_code": 1
}
```

All fields are optional. `exit_code` takes precedence over counts; `total` is computed from counts if omitted.

---

## 4. Session Analysis (h5i notes)

Claude Code stores a detailed JSONL log of every conversation in `~/.claude/projects/<repo>/`. `h5i notes` parses these logs and stores structured metadata linked to commits.

```bash
h5i notes analyze        # index the latest session (run after each session)
```

### Subcommands

| Command | What it shows |
|---------|--------------|
| `h5i notes footprint` | Files the AI read vs. edited; implicit dependencies |
| `h5i notes uncertainty [--file <path>]` | Every moment Claude hedged, with confidence score and context |
| `h5i notes omissions [--file <path>]` | Deferrals, placeholders, and unfulfilled promises |
| `h5i notes coverage [--max-ratio F]` | Per-file blind-edit count (edits with no prior Read) |
| `h5i notes churn [--limit N]` | Edit-to-read ratio per file — proxy for rework |
| `h5i notes graph [--limit N]` | Directed intent graph across commits |
| `h5i notes review [--limit N] [--min-score F]` | Ranked list of commits needing human review |
| `h5i notes show` | Raw stored analysis for HEAD |

All subcommands accept `--commit <oid>` to target a specific commit.

### Exploration footprint

```
── Exploration Footprint ──────────────────────────────────────
  Session 90130372  ·  503 messages  ·  181 tool calls

  Files Consulted:
    📖 src/main.rs ×13  [Read]
    📖 src/server.rs ×17  [Read,Grep]

  Files Edited:
    ✏ src/main.rs  ×18 edit(s)
    ✏ src/server.rs  ×17 edit(s)

  Implicit Dependencies (read but not edited):
    → src/metadata.rs
    → Cargo.toml
```

*Implicit dependencies* — read but not edited — are the most actionable output: files the AI had to understand to make the change, which Git's diff never captures.

### Uncertainty heatmap

Phrases like "not sure", "let me check", "this might break" are recorded with the surrounding context and the file being edited at that moment.

```
── Uncertainty Heatmap ─────────────────────────────────────────────────
  7 signals  ·  3 files

  src/auth.rs    ████████████░░░░  ●●●  4 signals  avg 28%
  src/main.rs    ██████░░░░░░░░░░  ●●   2 signals  avg 40%
  src/server.rs  ██░░░░░░░░░░░░░░  ●    1 signal   avg 52%

  ██ t:32   not sure    src/auth.rs  [25%]
       "…token validation might break if the token contains special chars…"
```

Confidence: **red** (<35%) = very uncertain, **yellow** (35–55%) = moderate, **green** (>55%) = incidental mention.

### Omission report

Detects three categories of incomplete work extracted from Claude's thinking:

| Kind | Badge | What it catches |
|------|-------|-----------------|
| **Deferral** | `⏭` | Acknowledged and explicitly skipped: `"for now"`, `"out of scope"`, `"separate PR"` |
| **Placeholder** | `⬜` | Admittedly incomplete: `"stub"`, `"hardcoded for now"`, `"simplified version"` |
| **Unfulfilled promise** | `💬` | Claude said `"I'll also update X"` but that file never appeared in the edit sequence |

```
── Omission Report ─────────────────────────────────────────────
  5 signals  ·  2 deferrals  ·  2 placeholders  ·  1 unfulfilled promise

  ⏭ DEFERRAL    src/auth.rs · "for now"
       "…I'll hardcode the token TTL for now — a proper config value can be added later…"

  ⬜ PLACEHOLDER  src/session.rs · "hardcoded for now"
       "…session timeout is hardcoded for now at 3600s, should come from config…"

  💬 UNFULFILLED  src/auth.rs · "i'll also update"
     → promised file: src/auth/tests.rs  (never edited)
```

Run `h5i notes omissions` right after `h5i notes uncertainty` — uncertainty tells you where Claude was unsure; omissions tell you what it left incomplete. Together they form a targeted review checklist.

### Attention coverage

An edit with no preceding Read in the same session is a **blind edit** — a change made without direct evidence that Claude understood the current state of the file.

```
── Attention Coverage — a3f9c2b

  File                    Edits   Coverage   Blind edits
  src/auth.rs                 4       75%             1
  src/session.rs              2        0%             2   ← review these
  src/main.rs                 1      100%             0
```

`h5i notes review` adds a `BLIND_EDIT` signal (weight 0.10 per file, max 0.30) to the commit's review score when coverage data is available.

---

## 5. Context Workspace (h5i context)

The context workspace (based on [arXiv:2508.00031](https://arxiv.org/abs/2508.00031)) gives Claude a version-controlled notepad at `.h5i-ctx/` that survives session resets. Its command structure mirrors Git.

### Recommended workflow

```bash
# Session start — restore state
h5i context show --trace

# During session — log reasoning
h5i context trace --kind OBSERVE "Redis p99 latency is 2 ms"
h5i context trace --kind THINK   "40 MB overhead is acceptable"
h5i context trace --kind ACT     "Switching session store to Redis"

# Before exploring a risky alternative
h5i context branch experiment/sync-session --purpose "try synchronous session store"

# After confirming or discarding the experiment
h5i context checkout main
h5i context merge experiment/sync-session   # or just stay on main

# After a meaningful milestone
h5i context commit "Implemented token refresh flow" \
  --detail "Handles 401s transparently; refresh stored in HttpOnly cookie."

# End of session
h5i context status
```

### Subcommands

| Command | Description |
|---------|-------------|
| `h5i context init --goal <text>` | Create workspace with initial goal |
| `h5i context show [--trace] [--window N]` | Show goal, milestones, recent commits, trace |
| `h5i context trace --kind <KIND> <content>` | Append an OTA trace entry (`OBSERVE`, `THINK`, `ACT`, `NOTE`) |
| `h5i context commit <summary> [--detail <text>]` | Save a milestone checkpoint |
| `h5i context branch <name> [--purpose <text>]` | Create and switch to a new branch |
| `h5i context checkout <name>` | Switch to an existing branch |
| `h5i context merge <branch>` | Merge a branch's log into current |
| `h5i context status` | Quick overview: branch, commit count, trace lines |
| `h5i context prompt` | Print a system prompt that tells Claude how to use these commands |

### `h5i context show` flags

| Flag | Description |
|------|-------------|
| `--branch <name>` | Show context for a branch without switching |
| `--commit <hash>` | Pull a specific milestone entry by hash prefix |
| `--trace` | Include recent OTA trace lines |
| `--window <N>` | Number of recent milestone commits to include (default: 3) |
| `--trace-offset <N>` | Scroll back N lines from the end of the trace |
| `--metadata <segment>` | Pull a named section from `metadata.yaml` |

### Workspace layout

```
.h5i-ctx/
├── main.md               ← global roadmap: goal, milestones, progress notes
└── branches/
    └── <branch>/
        ├── commit.md     ← milestone summaries (append-only)
        ├── trace.md      ← OTA execution trace
        └── metadata.yaml ← file structure, dependencies, env config
```

---

## 6. Session Handoff (h5i resume)

`h5i resume` assembles a structured briefing from locally stored h5i data so Claude can pick up exactly where the last session left off. No API call required.

```bash
h5i resume              # briefing for the current branch
h5i resume feat/oauth   # briefing for a specific branch
```

```
── Session Handoff ─────────────────────────────────────────────────
  Branch: feat/oauth  ·  Last active: 2026-03-27 14:22 UTC
  Agent: claude-code  ·  Model: claude-sonnet-4-6
  HEAD: a3f9c2b  implement token refresh flow

  Goal: Build an OAuth2 login system
  Progress:
    ✔ Initial setup
    ✔ GitHub provider integration
    ○ Token refresh flow  ← resume here
    ○ Logout + session cleanup

  Last Session: 503 messages  ·  181 tool calls  ·  4 files edited

  ⚠  High-Risk Files  (review before continuing)
    ██████████  src/auth.rs       4 signals  churn 80%  "not sure"
    ██████░░░░  src/session.rs    2 signals  churn 60%  "let me check"

  Memory Changes Since Last Snapshot
    + 2 files added  ~  1 file modified

  Suggested Opening Prompt
  ─────────────────────────────────────────────────────────────────
  Continue building "Build an OAuth2 login system". Completed: Initial
  setup, GitHub provider. Next: Token refresh flow. Review src/auth.rs
  before editing — 4 uncertainty signals recorded in the last session.
  ─────────────────────────────────────────────────────────────────
```

The briefing is richer when the full h5i workflow is in place:

| Feature | How to enable |
|---------|---------------|
| Goal + milestones | `h5i context init --goal "..."` |
| Session stats + risk files | `h5i notes analyze` after each session |
| Memory changes | `h5i memory snapshot` after each session |
| Agent + model in header | hooks or `H5I_MODEL` / `H5I_AGENT_ID` env vars |

### Recommended end-of-session checklist

```bash
h5i notes analyze                        # index the session
h5i memory snapshot -m "end of session"  # checkpoint memory
h5i resume                               # preview the next session's briefing
```

---

## 7. Enriched Log and Blame

```bash
h5i log [--limit N]
h5i log --ancestry src/auth.rs:42        # full prompt history for a specific line

h5i blame src/auth.rs
h5i blame src/auth.rs --show-prompt      # annotate commit boundaries with their prompt
h5i blame src/auth.rs --mode ast         # AST-level semantic blame
```

`h5i blame` shows two status columns before each line:
- Test status: `✅` passing, `✖` failing, blank = no data
- AI indicator: `✨` AI-authored

**`--show-prompt`** annotates each commit boundary with the prompt that triggered it:

```
── commit a3f9c2b ── prompt: "add per-IP rate limiting to the auth endpoint" ──
✅✨  a3f9c2b  claude-code  | pub fn check_rate_limit(ip: IpAddr) -> bool {
── commit 9e21b04 ── (no prompt recorded) ──
     9e21b04  Alice        | pub fn authenticate(token: &str) -> Result<User> {
```

**`h5i log --ancestry src/auth.rs:42`** traces every commit that touched a line, annotated with its prompt:

```
── Prompt ancestry for src/auth.rs:42

  [1 of 3]  a3f9c2b  Alice · 2026-03-27 14:02 UTC
       line:    check_rate_limit(&ip, &config.rate_limit)
       prompt:  "add per-IP rate limiting to the auth endpoint"

  [2 of 3]  9e21b04  Bob · 2026-03-26 11:45 UTC
       line:    check_rate_limit(&ip)
       prompt:  (none recorded)
```

**Intent-based rollback** — revert by describing the change, not by hash:

```bash
h5i rollback "the OAuth login changes"
h5i rollback "rate limiting" --dry-run   # preview
h5i rollback "the broken migration" --yes  # skip confirmation in CI
```

---

## 8. Integrity Engine

The `--audit` flag (and the dashboard's per-commit audit button) runs twelve deterministic rules against the diff. No AI, no network — pure string and stat checks.

| Rule | Severity | Trigger |
|------|----------|---------|
| `CREDENTIAL_LEAK` | **Violation** | Credential keyword + assignment + quoted value, or PEM header |
| `CODE_EXECUTION` | **Violation** | `eval()`, `exec()`, `os.system()`, `subprocess.*` in non-comment lines |
| `CI_CD_MODIFIED` | **Violation** | `.github/workflows/`, `Jenkinsfile`, etc. modified without CI/CD intent in prompt |
| `SENSITIVE_FILE_MODIFIED` | Warning | `.env`, `.pem`, `.key`, `id_rsa`, `credentials` in diff |
| `LOCKFILE_MODIFIED` | Warning | `Cargo.lock`, `package-lock.json`, `go.sum` changed without dependency intent |
| `UNDECLARED_DELETION` | Warning | >60% deletions with no deletion/refactor intent stated |
| `SCOPE_EXPANSION` | Warning | Prompt names a specific file but other source files were also modified |
| `LARGE_DIFF` | Warning | >500 total lines changed |
| `REFACTOR_ANOMALY` | Warning | "refactor" intent but insertions ≥ 3× deletions |
| `PERMISSION_CHANGE` | Warning | `chmod 777`, `sudo`, `setuid`, `chown root` in added lines |
| `BINARY_FILE_CHANGED` | Info | Binary file in diff |
| `CONFIG_FILE_MODIFIED` | Info | `.yaml`, `.toml`, `.json`, `.ini` modified |

Use `--force` to commit despite warnings; violations always block.

To add a rule: add a `pub const` to `rule_id` in `src/rules.rs`, write one pure `fn check_*(ctx: &DiffContext) -> Vec<RuleFinding>`, and register it in `run_all_rules`.

---

## 9. Memory Management

h5i versions Claude Code's persistent memory files alongside your code. Claude stores per-project memory in `~/.claude/projects/<repo-path>/memory/`. These files are local-only by default; h5i snapshots and versions them.

```bash
h5i memory snapshot                     # snapshot current memory state
h5i memory snapshot --commit a3f9c2b    # tie to a specific commit
h5i memory log                          # view snapshot history
h5i memory diff                         # last snapshot → live memory
h5i memory diff a3f9c2b b2f3a1c         # between two snapshots
h5i memory restore a3f9c2b             # restore memory to a snapshot
h5i memory push / h5i memory pull       # share with team
```

Example diff output:

```
memory diff a3f9c2b..b2f3a1c
────────────────────────────────────────────────────────────
  added     project_auth.md
    +  The auth middleware rewrite is driven by legal compliance
    +  requirements around session token storage.
  modified  feedback_tests.md
    +How to apply: always use a real DB in integration tests.
────────────────────────────────────────────────────────────
  1 added, 0 removed, 1 modified
```

Memory snapshots are backed by real git objects under `refs/h5i/memory`.

---

## 10. Sharing with Your Team

h5i stores its data in two git refs that are **not** pushed by `git push`:

| Ref | Contains |
|-----|----------|
| `refs/h5i/notes` | AI provenance, test metrics, causal links, integrity reports, design decisions |
| `refs/h5i/memory` | Claude memory snapshots |

```bash
h5i push                   # push both refs to origin
h5i push --remote upstream

# Pull manually
git fetch origin refs/h5i/notes:refs/h5i/notes
git fetch origin refs/h5i/memory:refs/h5i/memory
```

Add fetch refspecs to `.git/config` so `git pull` picks them up automatically:

```ini
[remote "origin"]
    url = git@github.com:you/repo.git
    fetch = +refs/heads/*:refs/remotes/origin/*
    fetch = +refs/h5i/notes:refs/h5i/notes
    fetch = +refs/h5i/memory:refs/h5i/memory
```

CI push step:

```yaml
- name: Push h5i metadata
  run: |
    git push origin refs/h5i/notes
    git push origin refs/h5i/memory
```

Team workflow:

```bash
# Alice
h5i commit -m "add rate limiting"
h5i memory snapshot && h5i push && git push origin main

# Bob
git pull                              # fetches code + notes + memory
h5i log                               # sees Alice's AI provenance
h5i memory restore <alice-commit>     # apply Alice's Claude memory locally
```

---

## 11. Web Dashboard

```bash
h5i serve            # http://localhost:7150
h5i serve --port 8080
```

<img src="./assets/screenshot_h5i_server.png" alt="h5i web dashboard">

| Tab | What it shows |
|-----|---------------|
| **Timeline** | Full commit history: model, agent, prompt, test badge, one-click Re-audit |
| **Summary** | Aggregate stats, agent leaderboard, filter pills (AI only / with tests / failing) |
| **Integrity** | Manually audit any commit message + prompt without committing |
| **Intent Graph** | Directed graph of causal commit chains |
| **Memory** | Browse and diff Claude memory snapshots per commit |
| **Sessions** | Per-commit: footprint, uncertainty heatmap, omissions, churn |

---

## 12. Storage Layout

```
.git/
└── .h5i/
    ├── ast/                        # SHA-256-keyed S-expression AST snapshots
    ├── crdt/                       # Yjs CRDT document state
    ├── delta/                      # Append-only CRDT update logs (per file)
    ├── memory/                     # Claude memory snapshots
    │   └── <commit-oid>/
    │       ├── MEMORY.md
    │       └── _meta.json
    ├── session_log/                # Session log analyses
    │   └── <commit-oid>/
    │       └── analysis.json
    └── pending_context.json        # Transient: consumed at next commit

.h5i-ctx/                           # Context workspace
├── main.md
└── branches/<branch>/
    ├── commit.md
    ├── trace.md
    └── metadata.yaml
```

**`refs/h5i/notes`** stores extended commit metadata as JSON blobs attached to each commit. Inspect any entry with:

```bash
git notes --ref refs/h5i/notes show <commit-oid>
```

**`refs/h5i/memory`** stores Claude memory snapshots as a linear commit history of git tree objects.

Neither ref is pushed or fetched by a plain `git push` / `git pull` — share them explicitly with `h5i push` (see §10).

---

## Demo

`examples/dnn-from-scratch` is a neural network built entirely with Claude Code and versioned with h5i:

```bash
bash examples/dnn-from-scratch/demo.sh --inspect   # inspect h5i log, blame, run XOR demo
bash examples/dnn-from-scratch/demo.sh             # replay the full build from scratch
```
