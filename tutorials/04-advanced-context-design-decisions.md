# When Claude Goes Off the Rails, Here's How to Recover

## Advanced context branching, knowledge distillation, and non-destructive rewind

*Part 4 of 4 — The h5i Notes & Context Series*

---

You're three sessions deep into a complex refactor. Claude has a rich reasoning trail. Then it happens: Claude goes down a dead-end approach, edits six files in the wrong direction, and you realize the whole thing needs to roll back.

With plain git, you'd `git checkout HEAD -- .` and lose everything. With h5i, you have two better options: reasoning branches to explore alternatives without losing your main thread, and `h5i rewind` to non-destructively restore any past working-tree state with an automatic safety backup.

This is Part 4 — the advanced patterns that make h5i's context workspace feel like a proper tool for AI-assisted engineering rather than just a log viewer.

---

## Reasoning branches: explore without commitment

The context workspace has branches, just like git. The default branch is `main`. You can create a branch to explore an alternative approach, then either merge the findings back or discard them.

```bash
h5i context branch experiment/sync-approach \
  --purpose "try synchronous retry as simpler fallback before committing to async"
```

```
✔  Created branch experiment/sync-approach
   Purpose: try synchronous retry as simpler fallback before committing to async
   Switched to experiment/sync-approach
```

Work on the experimental approach. Add traces, make edits, accumulate findings.

```bash
h5i context trace --kind OBSERVE "std::thread::sleep works but blocks the async executor"
h5i context trace --kind THINK   "sync retry is incompatible with the tokio runtime — this approach won't work"
h5i context commit "sync retry exploration — rejected" \
  --detail "blocking sleep incompatible with tokio; sticking with tokio::time::sleep"
```

Then return to your main reasoning thread:

```bash
h5i context checkout main
```

```
✔  Switched to main
   Traces: 14  |  Commits: 2
```

The exploration happened. The findings are recorded on `experiment/sync-approach`. Your main branch is exactly where you left it — no contamination from the dead end.

If the exploration yielded something worth keeping, merge it back:

```bash
h5i context merge experiment/sync-approach
```

The `merge` command pulls the trace entries and milestone notes from the branch into `main`, without the git code changes (those were discarded when you checked back out to main). You get the reasoning without the failed implementation.

---

## Knowledge distillation: extract what Claude actually learned

After several sessions, your context workspace accumulates hundreds of trace entries across multiple branches. Most of it is mechanical — file reads, routine edits. The signal you want is the THINK entries: the decisions, the tradeoffs, the things Claude figured out.

```bash
h5i context knowledge
```

```
── Distilled Knowledge ────────────────────────────────────────
  Extracted from 3 branches · 67 THINK entries · 24 unique insights

  [main]
    "Arc<RwLock<>> preferred over clone in SessionMiddleware — avoids
     allocation in hot path; confirmed safe under concurrent reads"

    "Token expiry check must use server time, not client-supplied
     timestamp — client clocks drift; validated against auth spec §4.2"

    "Middleware ordering: rate limiter must run before auth to avoid
     wasting token budget on requests that will be rejected anyway"

  [experiment/sync-approach]
    "std::thread::sleep blocks the tokio executor — synchronous retry
     is incompatible with async runtime; use tokio::time::sleep"

  [experiment/session-cache]
    "In-memory session cache reduces DB hits by ~40% in benchmarks
     but adds state synchronization complexity across replicas"

── 24 insights · run 'h5i context knowledge' again after more sessions
```

These are the distilled learnings from the AI's reasoning across all of your sessions. Not the implementation — the *understanding*. This is the output you'd want to paste into a design doc, a PR description, or the opening context of a new session on a related feature.

The deduplication is intentional: if Claude reached the same conclusion three times across different sessions, it appears once. Repetition is filtered; insight is preserved.

---

## Targeted context injection: relevant before editing

Before touching a complex file you haven't looked at in a while:

```bash
h5i context relevant src/auth/middleware.rs
```

```
── Relevant Context — src/auth/middleware.rs ──────────────────

  Milestone contributions (2):
    [c1a2b3] analyzed auth middleware
      "Injection point at line 88; SessionMiddleware::new takes
       owned SessionStore, needs to change to Arc<RwLock<>>"

    [d4e5f6] implemented session store injection
      "Updated new() signature, updated store field type; tests
       not yet updated"

  Trace mentions (5):
    [OBSERVE] t:03  "middleware.rs reads token from cookie at line 44"
    [THINK]   t:07  "Arc<RwLock<>> avoids clone in hot path"
    [ACT]     t:12  "SessionMiddleware::new() signature updated"
    [OBSERVE] t:21  "error handling in middleware doesn't propagate to client"
    [NOTE]    t:34  "TODO: verify cookie-based clients still work after injection"

  Cross-branch mentions (1):
    [experiment/sync-approach] "middleware is async — sync retry won't compose here"
```

This is targeted context retrieval: everything h5i knows about that specific file, assembled from across all branches and sessions. Run this before editing a file you last touched three commits ago. It takes two seconds and replaces ten minutes of re-reading session logs.

---

## h5i rewind: non-destructive working-tree recovery

When Claude makes a series of changes that need to be rolled back — not just the last commit, but the entire working tree to a known-good state — `h5i rewind` handles it without destroying anything.

### Preview first

```bash
h5i rewind a3f8c12 --dry-run
```

```
── Rewind Preview ──────────────────────────────────────────────
  Target:  a3f8c12  implement session store injection
  Current: d4e5f6   (HEAD)

  Files that would change:
    ~  src/auth/middleware.rs   (modified)
    ~  src/auth/session.rs      (modified)
    ✗  src/billing/token.rs     (would be deleted — added after target)

  Dry run — no changes made.
  Run without --dry-run to apply.
```

`--dry-run` shows you exactly what would change before touching anything. Check this against your intentions before proceeding.

### Apply the rewind

```bash
h5i rewind a3f8c12
```

```
── Rewind ──────────────────────────────────────────────────────
  Target: a3f8c12  implement session store injection
  ✔  WIP backed up → refs/h5i/shadow/20260420-142233

  Applied:
    ~  src/auth/middleware.rs   restored
    ~  src/auth/session.rs      restored
    ✗  src/billing/token.rs     deleted

  Working tree is now at a3f8c12 state.
  HEAD is unchanged — run 'git diff HEAD' to see what changed.
  To commit this state: h5i commit -m "rewind to a3f8c12"
```

Two things to notice:

**HEAD doesn't move.** The working tree is restored, but HEAD still points to the current commit. `git diff HEAD` shows the full picture of what changed. You can inspect before committing.

**The WIP is saved automatically.** Before touching anything, h5i commits your current working tree to a shadow ref (`refs/h5i/shadow/20260420-142233`). Even if you had uncommitted changes, they're not lost.

### Recovering the shadow ref

If you rewind and then change your mind:

```bash
git checkout refs/h5i/shadow/20260420-142233 -- .
```

Your working tree is back to exactly where it was before the rewind. Nothing is destroyed.

### Force flag for clean trees

If your working tree is already clean (no uncommitted changes), the shadow commit step is skipped automatically. If you want to skip it even with dirty state — you understand the risk and are certain you don't need the backup:

```bash
h5i rewind a3f8c12 --force
```

Use sparingly. The default behavior is safe; `--force` trades the safety net for speed.

---

## Putting it all together: a real advanced workflow

Here's what a full session looks like with all four advanced patterns:

```bash
# Morning: pick up where you left off
h5i resume

# Before touching the complex file:
h5i context relevant src/auth/middleware.rs

# Claude explores a new approach — branch the reasoning
h5i context branch experiment/jwt-migration \
  --purpose "evaluate JWT as replacement for session cookies"

# ... Claude works, traces accumulate automatically via hook ...

# The JWT approach won't work (too many client changes required)
h5i context trace --kind THINK "JWT migration requires client-side changes we can't coordinate — rejecting"
h5i context commit "JWT migration — rejected" \
  --detail "client coordination cost too high; sticking with session cookies"

# Back to main thread
h5i context checkout main

# Distill what we learned across all branches
h5i context knowledge

# If the experimental code changes need undoing:
h5i rewind HEAD~3 --dry-run    # preview
h5i rewind HEAD~3              # apply with automatic WIP backup

# Commit the clean state
h5i commit -m "revert jwt experiment, restore session cookie approach" \
  --model claude-sonnet-4-6 \
  --agent claude-code \
  --prompt "roll back jwt migration experiment"

h5i notes analyze              # link session log to this commit

# End of day
h5i memory snapshot
h5i push                       # push h5i refs to share with the team
```

Every step is auditable. Every decision is traced. Every rollback is non-destructive. The next person who opens this repo — or the next session of Claude — gets full context without any manual reconstruction.

---

## What you've built

Across all four parts of this series, here's what `h5i notes` and `h5i context` give you together:

| Problem | h5i solution |
|---------|-------------|
| AI sessions are amnesiac | `h5i context` persists reasoning across sessions |
| "Why did Claude touch that file?" | `h5i notes show` — footprint, causal chain, edit sequence |
| "Was Claude confident about that?" | `h5i notes uncertainty` — heatmap by file |
| "Did Claude read the file before editing?" | `h5i notes coverage` — blind edit detection |
| "Which commits need human review?" | `h5i notes review` — composite risk score |
| "What did Claude figure out across all sessions?" | `h5i context knowledge` — distilled THINK entries |
| "What did Claude know about this file?" | `h5i context relevant <file>` — targeted retrieval |
| "Claude went off the rails, need to recover" | `h5i rewind` — non-destructive working-tree restore |

The code survives. The reasoning survives too.

---

*h5i is open source. Source: [github.com/Koukyosyumei/h5i](https://github.com/Koukyosyumei/h5i)*

*Tags: AI, Developer Tools, Git, Software Engineering, Claude, Refactoring*
