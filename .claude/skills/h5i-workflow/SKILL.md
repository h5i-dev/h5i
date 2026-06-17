# h5i Workflow Skill

Activate this skill to automatically structure your reasoning and record AI
provenance as you work. Invoke with `/h5i-workflow` or include in your system prompt.

The h5i CLI is **verb-based**: `capture` (record), `recall` (read), `audit`
(assess risk), `share` (publish). Legacy top-level forms (`h5i commit`,
`h5i context`, `h5i notes`, …) still run but print a deprecation hint — prefer
the verb forms below.

---

## When to activate

Activate at the start of any non-trivial task (implementation, debugging, refactoring,
or exploration that spans more than one file or tool call).

---

## Session start

Before doing any work:

```bash
# Check whether a context workspace exists
h5i recall context status

# If not initialized yet:
h5i recall context init --goal "<one-line summary of what you are about to do>"
```

> If h5i's Claude Code hooks are installed (`h5i hook setup`), a **SessionStart**
> hook already injects prior reasoning into your context — you'll see a
> `[h5i] Context workspace active` banner. You still set the goal for a new task.

---

## While working — emit one trace entry per logical step

```bash
# After reading / grepping files to understand the codebase:
h5i recall context trace --kind OBSERVE "<what you found>"

# After deciding on an approach or making a design choice:
h5i recall context trace --kind THINK "<the decision and why>"

# After editing or writing a file:
h5i recall context trace --kind ACT "<what you changed and where>"

# For open questions, blockers, or reminders:
h5i recall context trace --kind NOTE "TODO: <what to revisit>"
```

> If the **PostToolUse** hook (`h5i claude sync`) is installed, ACT/OBSERVE traces
> are emitted automatically on every Edit/Write/Read. You should still add
> **THINK** and **NOTE** entries by hand — those capture intent the hook can't infer.
> Use `--ephemeral` for scratch notes that should be cleared on the next commit.

**Rules:**
- One entry per distinct step — do not batch multiple actions into one trace.
- OBSERVE: facts learned from reading code, logs, or docs.
- THINK: a decision, trade-off, or hypothesis you are committing to.
- ACT: a concrete change (file path + what changed).
- NOTE: anything that doesn't fit above — todos, warnings, open questions.

---

## After completing a logical milestone

```bash
h5i recall context commit "<milestone summary>" \
  --detail "<what was done and what is left>"
```

A milestone is: analysis complete, feature implemented, bug isolated, tests passing, etc.
Do not wait until the end of the session — checkpoint after each meaningful unit of work.

---

## Checking prior reasoning

Before reading a file, check if h5i has prior reasoning about it:

```bash
h5i recall context relevant <file-path>     # milestones + trace mentioning the file
h5i recall context search "<query>"         # BM25 + git co-change search across traces
```

Before starting a sub-task, check recent decisions:

```bash
h5i recall context show              # depth=2 timeline (default)
h5i recall context show --depth 1    # compact index (~800 tokens), faster
h5i recall context show --depth 3    # full OTA log  (alias: --trace)
h5i recall context todo              # open TODO / FIXME / BLOCKED items from the trace
h5i recall context knowledge         # every THINK decision ever recorded, distilled
```

---

## Exploring alternatives

When you want to try an approach without losing your current thread:

```bash
h5i recall context branch experiment/<name> --purpose "<hypothesis>"
# ... explore ...
h5i recall context checkout main
h5i recall context merge experiment/<name>   # if it worked
```

To delegate to a subagent in an isolated sub-context:

```bash
h5i recall context scope <name>      # creates a scope/<name> branch
# ... subagent works ...
h5i recall context merge scope/<name>
```

---

## Committing code

Always use `h5i capture commit` instead of `git commit`, and record AI provenance:

```bash
h5i capture commit -m "<message>" \
  --model claude-opus-4-8 \
  --agent claude-code
# Do not pass --intent in Claude Code: the human prompt is auto-captured by the
# UserPromptSubmit hook and takes precedence. In Codex, `h5i codex finish`
# records the raw prompt from session JSONL when installed as the Stop hook.
# --intent is a fallback for CI/scripts/manual commits.
```

Add `--tests` when tests were added or modified, `--ast` to snapshot structure,
`--audit` for security-sensitive changes, and `--decisions <FILE>` to record
non-obvious design tradeoffs (JSON array of
`{ "location", "choice", "alternatives"?, "reason" }`).

After every commit, link the just-completed session to HEAD:

```bash
h5i recall notes analyze
```

---

## Verifying AI provenance & risk (optional but encouraged)

```bash
h5i recall log --limit 10                 # recent commits with AI metadata
h5i recall blame <file> --show-prompt     # blame annotated with the prompt per boundary
h5i recall notes show                     # files consulted vs edited, causal chain
h5i recall notes coverage                 # blind edits (edited without reading first)
h5i audit review --limit 50               # rank commits that most need human review
h5i audit scan                            # scan reasoning traces for prompt-injection
h5i audit vibe                            # repo-wide AI-footprint audit
```

---

## Pinning facts as claims

When you establish a fact that should auto-invalidate if its backing files change:

```bash
h5i capture claim "<the fact>" --path <file>   # content-addressed, evidence-backed
h5i recall claims                              # list live & stale claims
```

---

## End of session

The `Stop` hook auto-checkpoints the context workspace if installed. If not,
checkpoint manually:

```bash
h5i recall context commit "session end" --detail "<summary of what was done>"
h5i capture memory                  # snapshot agent memory → refs/h5i/memory
```

To share h5i refs (not included in a plain `git push`):

```bash
h5i share push     # push refs/h5i/* (notes, context, memory, ast)
h5i share pr post  # upsert a sticky GitHub PR comment with AI provenance (needs gh)
```

---

## Quick reference

| Command | When |
|---------|------|
| `h5i recall context init --goal` | Start of a new task |
| `h5i recall context trace --kind OBSERVE` | After reading a file |
| `h5i recall context trace --kind THINK` | Before making a design choice |
| `h5i recall context trace --kind ACT` | After editing a file |
| `h5i recall context commit` | After each logical milestone |
| `h5i recall context show --depth 1` | Quick orientation |
| `h5i recall context relevant <file>` | Before reading a complex file |
| `h5i capture commit --model … --agent …` | Instead of git commit |
| `h5i recall notes analyze` | After every commit |
| `h5i audit review` | Triage risky commits before merge |
| `h5i capture memory` | End of session |
| `h5i share push` / `h5i share pr post` | Publish h5i refs / surface on a PR |

> Every command has an MCP equivalent (`h5i_context_trace`, `h5i_commit`,
> `h5i_log`, …) when running against the `h5i mcp` server.
