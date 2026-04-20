# h5i Workflow Skill

Activate this skill to automatically structure your reasoning as you work.
Invoke with `/h5i-workflow` or include in your system prompt.

---

## When to activate

Activate at the start of any non-trivial task (implementation, debugging, refactoring,
or exploration that spans more than one file or tool call).

---

## Session start

Before doing any work:

```bash
# Check whether a context workspace exists
h5i context status

# If not initialized yet:
h5i context init --goal "<one-line summary of what you are about to do>"
```

---

## While working — emit one trace entry per logical step

Use the MCP tool `h5i_context_trace` (preferred) or the CLI:

```bash
# After reading / grepping files to understand the codebase:
h5i context trace --kind OBSERVE "<what you found>"

# After deciding on an approach or making a design choice:
h5i context trace --kind THINK "<the decision and why>"

# After editing or writing a file:
h5i context trace --kind ACT "<what you changed and where>"

# For open questions, blockers, or reminders:
h5i context trace --kind NOTE "TODO: <what to revisit>"
```

**Rules:**
- One entry per distinct step — do not batch multiple actions into one trace.
- OBSERVE: facts learned from reading code, logs, or docs.
- THINK: a decision, trade-off, or hypothesis you are committing to.
- ACT: a concrete change (file path + what changed).
- NOTE: anything that doesn't fit above — todos, warnings, open questions.

---

## After completing a logical milestone

```bash
h5i context commit "<milestone summary>" \
  --detail "<what was done and what is left>"
```

A milestone is: analysis complete, feature implemented, bug isolated, tests passing, etc.
Do not wait until the end of the session — checkpoint after each meaningful unit of work.

---

## Checking prior reasoning

Before reading a file, check if h5i has prior reasoning about it:

```bash
h5i context relevant <file-path>
```

Before starting a sub-task, check recent decisions:

```bash
h5i context show          # depth=2 timeline (default)
h5i context show --depth 1   # compact index, faster
h5i context show --depth 3   # full OTA log
```

---

## Exploring alternatives

When you want to try an approach without losing your current thread:

```bash
h5i context branch experiment/<name> --purpose "<hypothesis>"
# ... explore ...
h5i context checkout main
h5i context merge experiment/<name>   # if it worked
```

---

## Committing code

Always use `h5i commit` instead of `git commit`, and record AI provenance:

```bash
h5i commit -m "<message>" \
  --model claude-sonnet-4-6 \
  --agent claude-code \
  --prompt "<the user's original request>"
```

Add `--tests` when tests were modified, `--audit` for security-sensitive changes.

After every commit:

```bash
h5i notes analyze
```

---

## End of session

The `Stop` hook auto-checkpoints if installed. If not, checkpoint manually:

```bash
h5i context commit "session end" --detail "<summary of what was done>"
h5i memory snapshot
```

---

## Quick reference

| Command | When |
|---------|------|
| `h5i context init` | Start of a new task |
| `h5i context trace --kind OBSERVE` | After reading a file |
| `h5i context trace --kind THINK` | Before making a design choice |
| `h5i context trace --kind ACT` | After editing a file |
| `h5i context commit` | After each logical milestone |
| `h5i context show --depth 1` | Quick orientation |
| `h5i context relevant <file>` | Before reading a complex file |
| `h5i commit --model ... --agent ...` | Instead of git commit |
| `h5i notes analyze` | After every h5i commit |
| `h5i memory snapshot` | End of session |
