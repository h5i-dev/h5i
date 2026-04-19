# Stop Letting Your AI Agent Forget Everything It Did

## How h5i Notes turns ephemeral Claude sessions into a permanent audit trail

*Part 1 of 4 — The h5i Notes & Context Series*

---

You finish a three-hour session with Claude Code. The agent navigated twelve files, rewrote a tricky auth flow, and caught a bug you hadn't noticed. You commit the code and close the terminal.

Twenty minutes later your colleague asks: "Why did Claude touch `src/session.rs`? That wasn't in the ticket."

You have no idea. The session log is a wall of JSON. The commit message says "refactor auth middleware." That's it.

This is the dirty secret of AI-assisted development: **the code survives, but the reasoning doesn't.** Every Claude session is amnesiac by design. The next one starts from zero.

`h5i notes` is the fix.

---

## What h5i Notes actually does

`h5i notes` is a thin layer that reads Claude Code's session log — the JSONL file Claude writes to `~/.claude/projects/<your-repo>/` — and turns it into structured, queryable metadata attached to your git commits.

After one command you get:

- **Exploration footprint** — every file Claude read vs. edited, with tool names and counts
- **Causal chain** — the opening prompt, key decisions extracted from thinking blocks, and the ordered edit sequence
- **Uncertainty heatmap** — moments where Claude expressed doubt, with confidence scores
- **Omission report** — deferrals, placeholders, and broken promises Claude left in its thinking
- **Blind-edit coverage** — files Claude modified without having read first (a proxy for "edited from memory")

All of this is stored in `.git/.h5i/session_log/<commit-oid>/analysis.json` and linked to HEAD. It follows your code through `git push`, `h5i push`, and survives forever.

---

## Setup in two minutes

First, initialize h5i in your repository:

```bash
h5i init
```

Then add the PostToolUse hook to `.claude/settings.json` in your project root:

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

And register the MCP server so Claude can call h5i tools natively:

```json
{
  "mcpServers": {
    "h5i": { "command": "h5i", "args": ["mcp"] }
  }
}
```

That's it. From this point every Claude session is automatically traced, and the MCP tools are available inside Claude Code without any shell commands.

---

## Your first session analysis

Do some work with Claude Code — any task is fine. When the session ends, run:

```bash
h5i notes analyze
```

```
➜  Auto-detected session: ~/.claude/projects/-home-you-myrepo/abc123.jsonl
➜  Analyzing session log → commit a3f8c12
✔  89 messages · 34 tool calls · 4 edited · 6 consulted
  ℹ Run h5i notes show a3f8c12 to inspect results.
```

Then look at what was captured:

```bash
h5i notes show
```

```
── Exploration Footprint ──────────────────────────────────
  Session a3f8c12  ·  89 messages  ·  34 tool calls

  Files Consulted:
    📖 src/auth.rs              ×4  (Read, Grep)
    📖 src/session.rs           ×3  (Read)
    📖 src/middleware/mod.rs    ×2  (Read)

  Files Edited:
    ✏ src/auth.rs               ×3 edit(s)
    ✏ src/session.rs            ×1 edit(s)

  Implicit Dependencies (read but not edited):
    → src/middleware/mod.rs
    → Cargo.toml
    → src/error.rs

── Causal Chain ─────────────────────────────────────────────
  Trigger:
    "refactor the auth middleware to use the new session store"

  Key Decisions:
    1. Moved token validation into a dedicated method to isolate the session boundary
    2. Used Arc<RwLock<SessionStore>> rather than cloning to avoid unnecessary allocations
    3. Left the legacy check_token path in place behind a feature flag for backward compat

  Edit Sequence:
     1.  src/auth.rs          modify  t:12
     2.  src/session.rs       modify  t:19
     3.  src/auth.rs          modify  t:23
     4.  src/auth.rs          modify  t:31
```

Now your colleague's question has an answer: Claude touched `src/session.rs` because the trigger prompt was about the new session store, and the edit sequence shows it was step 2 in a logical progression.

---

## The implicit dependencies insight

Notice the **Implicit Dependencies** section in the footprint output. These are files Claude *read* but did not edit — `src/middleware/mod.rs`, `Cargo.toml`, `src/error.rs`.

Standard git diff and blame never capture this. A future developer reading the commit has no idea that the auth refactor was informed by how the middleware module worked, or that a Cargo.toml dependency was checked before making an API choice.

`h5i notes` makes these invisible influences visible and permanently attached to the commit.

---

## Integrating it into your workflow

The recommended end-of-session checklist is two commands:

```bash
h5i notes analyze          # index the session
h5i memory snapshot        # version Claude's memory files too
```

Run these before closing the terminal. The analysis takes about two seconds.

At the start of the *next* session:

```bash
h5i resume                 # get a full handoff briefing from local data
```

`h5i resume` assembles branch state, goal, milestone progress, last session statistics, high-risk files, and a suggested opening prompt — entirely from local data, no API call needed.

---

## What the commit history looks like now

```bash
h5i log --limit 3
```

```
● a3f8c12  refactor auth middleware
  2026-04-20 14:02  Alice <alice@example.com>
  model: claude-sonnet-4-6 · agent: claude-code · 312 tokens
  prompt: "refactor the auth middleware to use the new session store"
  tests:  ✔ 47 passed, 0 failed, 1.8s

● 9e21b04  fix off-by-one in token parser
  2026-04-19 11:45  Bob <bob@example.com>
  (no AI metadata)

● 4c8d2a1  initial session store implementation
  2026-04-18 09:10  Alice <alice@example.com>
  model: claude-sonnet-4-6 · agent: claude-code · 201 tokens
  prompt: "implement an in-memory session store with TTL eviction"
```

Every AI-assisted commit now carries its full provenance. The prompt is searchable, the model is recorded, token usage is tracked. And behind each commit, the full session analysis — footprint, decisions, uncertainty — is one `h5i notes show` away.

---

## Coming up next

In Part 2 we'll go deeper: **the uncertainty heatmap, blind-edit detection, and the review surface** — the tools that answer "should a human double-check what Claude did here?"

If you've ever merged an AI-generated PR and later found a subtle bug in a file the AI edited without reading first, Part 2 is for you.

---

*h5i is open source. Source: [github.com/Koukyosyumei/h5i](https://github.com/Koukyosyumei/h5i)*

*Tags: AI, Developer Tools, Git, Software Engineering, Claude*
