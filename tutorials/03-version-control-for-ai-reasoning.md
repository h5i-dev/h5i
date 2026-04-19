# Version Control for AI Reasoning

## How to make Claude's thinking persist across sessions

*Part 3 of 4 — The h5i Notes & Context Series*

---

Here's a scenario that happens every week in AI-assisted engineering teams.

Monday: Claude spends two hours analyzing your codebase, mapping dependencies, understanding the auth flow, and forming a clear plan for the refactor. At the end of the session you have a rough mental model of what Claude "knows."

Tuesday: You open a new session. You type "continue the auth refactor." Claude has no idea what you're talking about. The analysis is gone. The plan is gone. The two hours of context-building is gone.

You could paste in context manually. You could maintain a notes document. You could write a long system prompt at the start of every session.

Or you could let h5i's context workspace do it automatically.

---

## What the context workspace is

`h5i context` is a versioned reasoning trail — a structured log of OBSERVE/THINK/ACT steps attached to your repository. It lives in `refs/h5i/context`, follows your code when you `h5i push`, and snapshots automatically on every `h5i commit`.

Unlike session notes (which capture what happened after the fact), the context workspace captures reasoning *as it happens* — and can inject it back into Claude's context at the start of the next session.

The three primitives are:

- **`h5i context init`** — start a new context workspace with a goal
- **`h5i context trace`** — add a single reasoning step (OBSERVE, THINK, ACT, or NOTE)
- **`h5i context commit`** — checkpoint the current state as a named milestone

Everything else (`branch`, `merge`, `status`, `relevant`, `knowledge`) builds on top of these three.

---

## Setup

If you haven't initialized h5i yet:

```bash
h5i init
```

Add the PostToolUse hook to `.claude/settings.json` in your project root:

```json
{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Edit|Write|Read",
        "hooks": [{ "type": "command", "command": "h5i hook run" }]
      }
    ]
  }
}
```

This hook is what makes tracing automatic. Every Read and Edit Claude does will emit a context trace entry without any manual intervention.

Register the MCP server so Claude can also call h5i directly:

```json
{
  "mcpServers": {
    "h5i": { "command": "h5i", "args": ["mcp"] }
  }
}
```

---

## Starting a context workspace

At the beginning of a new task, initialize the workspace:

```bash
h5i context init --goal "refactor auth middleware to use new session store"
```

```
✔  Initialized context workspace
   Goal: refactor auth middleware to use new session store
   Branch: main
```

This creates `.h5i-ctx/` in your working directory (git-tracked but h5i-managed) and sets the goal. The goal persists. Every future `h5i context status` call will show it, every session handoff will include it.

You only need to run `init` once per major task. For ongoing development in a long-lived repo, one init is often enough for weeks.

---

## The trace loop: OBSERVE, THINK, ACT, NOTE

While working, emit trace entries at natural checkpoints. These are the four kinds:

| Kind | When to use |
|------|------------|
| OBSERVE | After reading or exploring code — "I found X" |
| THINK | After making a decision — "I'll use Y because Z" |
| ACT | After editing a file — "Changed X in file Y" |
| NOTE | Anything else: reminders, risks, deferreds |

```bash
h5i context trace --kind OBSERVE "auth middleware reads token from cookie, not header"
h5i context trace --kind THINK   "session store should be injected via Arc<RwLock<>> to avoid clone overhead"
h5i context trace --kind ACT     "updated SessionMiddleware::new() in src/auth/middleware.rs to accept store"
h5i context trace --kind NOTE    "TODO: verify that existing cookie-based clients aren't broken by this"
```

These trace entries stack up chronologically. They're the raw material for session handoffs, diffs, and the knowledge distillation we'll see in Part 4.

### Automatic tracing via the hook

You don't have to type these manually. The PostToolUse hook emits them automatically:

- When Claude reads a file → `OBSERVE` with the file path
- When Claude edits a file → `ACT` with the file and change summary
- When Claude enters a thinking block → `THINK` with the reasoning excerpt

Manual traces are still useful for high-level decisions that the hook doesn't capture — architectural choices, rejected approaches, things worth calling out explicitly. But the mechanical tracking happens automatically.

---

## Checkpointing milestones

After completing a logical unit of work, commit the context:

```bash
h5i context commit "analyzed auth middleware" \
  --detail "read middleware.rs and session.rs; identified injection point at line 88; plan is to replace the cloned store with Arc<RwLock<>>"
```

```
✔  Context commit [c1a2b3]  analyzed auth middleware
```

Context commits are not git commits. They're checkpoints in the reasoning trail — named moments you can return to, diff against, or inject as handoff context. They accumulate alongside your regular git commits.

The `--detail` field is the key part. It should answer: "if I came back to this task cold tomorrow, what would I need to know?" That's exactly what `h5i resume` will show at the start of your next session.

---

## Checking the current state

```bash
h5i context show --trace --window 5
```

```
── h5i-ctx · branch: main ──────────────────────────────────────
  Goal: refactor auth middleware to use new session store

  Recent commits (2):
    [c1a2b3] analyzed auth middleware
    [d4e5f6] implemented Arc<RwLock<>> injection

── Trace (last 5 lines) ─────────────────────────────────────────
  [OBSERVE] src/auth/middleware.rs ×3 reads
  [THINK]   Arc<RwLock<>> preferred over clone — avoids allocation in hot path
  [ACT]     src/auth/middleware.rs modified (SessionMiddleware::new signature)
  [OBSERVE] src/auth/session.rs ×1 read
  [ACT]     src/auth/session.rs modified (store field type updated)
```

This is your current state. The goal is at the top, recent milestones below, and the last 5 trace entries showing exactly what happened.

```bash
h5i context status
```

```
── h5i-ctx · branch: main ─────────────────────────────────────
  Goal: refactor auth middleware to use new session store
  Commits: 2  |  Traces: 14  |  Branch: main

── Flagged Commits ─────────────────────────────────────────────
  ⚠ d4e5f6  score 0.62  implement session store injection
     moderate uncertainty · 3 files touched
```

The proactive review surface (Part 2) appears here automatically — any commits from your recent sessions with high review scores show up before you start working. You don't have to remember to check.

---

## Starting the next session: h5i resume

This is where the context workspace pays off most clearly. Close your terminal. Open a new one. Run:

```bash
h5i resume
```

```
── Session Handoff ──────────────────────────────────────────────
  Branch: update-auth-middleware
  Goal:   refactor auth middleware to use new session store

  Last session: 2026-04-19 14:02  (a3f8c12)
  Messages: 89  ·  Tool calls: 34  ·  Edited: 4  ·  Consulted: 6

  Context milestones:
    [c1a2b3] analyzed auth middleware
    [d4e5f6] implemented Arc<RwLock<>> injection

  Recent trace (last 5):
    [OBSERVE] src/auth/middleware.rs ×3 reads
    [THINK]   Arc<RwLock<>> preferred over clone
    [ACT]     src/auth/middleware.rs — SessionMiddleware::new updated
    [OBSERVE] src/auth/session.rs ×1 read
    [ACT]     src/auth/session.rs — store field type updated

  High-risk files (from last session):
    ⚠ src/billing/token.rs  (blind edit · 3 uncertainty moments)

  Suggested opening prompt:
    "Continue the auth middleware refactor. The session store injection
     is complete in middleware.rs and session.rs. Next: update the
     integration tests to use the injected store, and verify the
     cookie-based client path isn't broken."

── Run 'h5i context relevant <file>' before editing a complex file ─
```

`h5i resume` assembles all of this from local data — no API call, no external service. It reads git state, the context workspace, and the last session analysis and composes a structured handoff.

The suggested opening prompt is the most useful part: instead of starting a new session with a vague "continue the refactor," you start with a specific, grounded prompt that includes exactly where you left off.

---

## Context is versioned alongside code

Every `h5i commit` automatically snapshots the context workspace:

```bash
h5i commit -m "implement session store injection" \
  --model claude-sonnet-4-6 \
  --agent claude-code \
  --prompt "replace cloned session store with Arc<RwLock<>>"
```

```
✔  Committed d4e5f6  implement session store injection
   model: claude-sonnet-4-6 · agent: claude-code · 247 tokens
   ✔  Context snapshot linked to d4e5f6
```

If you need to return to the reasoning state at that commit:

```bash
h5i context restore d4e5f6
```

The context workspace reverts to exactly where it was when that commit was made — goal, milestones, trace entries, all of it. Useful when you need to understand *why* a decision was made three weeks ago, or when you're debugging a regression and want to know what Claude was thinking when it introduced the change.

---

## The end-of-session checklist

```bash
h5i notes analyze          # link the session log to HEAD
h5i context commit "implemented session store injection" \
  --detail "Arc<RwLock<>> injection complete. Tests not yet updated. See TODO note in trace."
h5i memory snapshot        # version Claude's memory files
```

Three commands. About ten seconds. The next session starts with full context.

---

## What you gain

Before h5i context:
- Each Claude session starts from zero
- Handoff context lives in your head or in a manually maintained notes doc
- "Why did Claude do that?" is unanswerable without reading the full session log

After h5i context:
- Every session builds on the last
- `h5i resume` assembles the handoff automatically
- Every decision is traced, timestamped, and linked to the git commit that resulted from it
- Context diffs show how reasoning evolved alongside code

The goal was never "make Claude smarter." The goal is to make the *session* a first-class artifact — something that persists, accumulates, and can be inspected — the same way the code does.

---

## Coming up next

In Part 4 we'll cover advanced context features: reasoning branches for exploring alternatives without losing your main thread, `h5i context knowledge` for distilling key insights across all branches, `h5i context relevant` for targeted context injection before editing a complex file, and `h5i rewind` for non-destructive recovery when things go sideways.

---

*h5i is open source. Source: [github.com/Koukyosyumei/h5i](https://github.com/Koukyosyumei/h5i)*

*Tags: AI, Developer Tools, Git, Software Engineering, Claude*
