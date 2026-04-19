# Did Claude Actually Know What It Was Doing?

## How to catch AI blind spots before they ship to production

*Part 2 of 4 — The h5i Notes & Context Series*

---

Claude refactored your payment processor. The tests pass. The diff looks clean. You merge.

Three days later a charge fails in production — a subtle edge case in the token refresh path. You check the session log. Claude edited `src/billing/token.rs` in turn 14 of the session. You scroll back through the session log looking for when Claude read that file first.

It didn't. Claude wrote 47 lines into a file it had never read in that session.

This is a **blind edit**: the AI modified code from memory rather than from direct inspection of the current state. It happens more often than you'd expect, and standard code review doesn't catch it because the diff looks fine — you see what changed, not what the AI *didn't* check before changing it.

`h5i notes` surfaces three signals that answer the question "should a human double-check this?" before the merge happens.

---

## The three review signals

**1. Uncertainty heatmap** — moments where Claude expressed doubt in its thinking blocks ("I'm not sure if...", "this might break...", "I should verify..."), scored and ranked by file

**2. Blind-edit detection** — files Claude modified without having read first in that session

**3. Review surface** — a composite score that combines both signals with edit count and file complexity to prioritize which commits need human eyes

All three are derived automatically from the session log. No annotations required.

---

## Setup

If you haven't set up h5i yet, two minutes gets you there. In your project root:

```bash
h5i init
```

Add the PostToolUse hook to `.claude/settings.json`:

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

After any Claude Code session, run:

```bash
h5i notes analyze
```

That's everything needed to populate all three signals.

---

## Signal 1: The uncertainty heatmap

After a session, run:

```bash
h5i notes uncertainty
```

```
── Uncertainty Heatmap ────────────────────────────────────────
  Session a3f8c12  ·  89 messages  ·  11 uncertainty moments

  By File:
    ⚠ src/billing/token.rs       3 moments  ████████
    ⚠ src/auth/session.rs        2 moments  █████
    ⚠ src/middleware/rate.rs     1 moment   ███

  Top Uncertainty Moments:
    t:14  src/billing/token.rs
          "I'm not sure whether refresh_token takes priority over
           access_token expiry here — the docs aren't clear and I
           could be reading the wrong code path."

    t:22  src/auth/session.rs
          "This might break existing sessions if the TTL check
           fires before the renewal window. Should verify with
           the session store implementation."

    t:31  src/middleware/rate.rs
          "I'm working from memory on the rate limit semantics —
           the actual bucket implementation may differ."
```

The file-level bucketing is the most useful part. When you see `src/billing/token.rs` with three uncertainty moments, that's not a random signal — Claude was actively uncertain while editing that file. That's the file to review.

### Filtering by file

If you want to focus on a specific file:

```bash
h5i notes uncertainty --file src/billing/token.rs
```

```
── Uncertainty — src/billing/token.rs ─────────────────────────
  3 moments in this file

  t:14  "I'm not sure whether refresh_token takes priority over..."
  t:19  "The expiry comparison might be off by one — Unix epoch
         vs milliseconds — I should double-check."
  t:26  "Assuming this follows the same pattern as access tokens
         but that assumption may be wrong."
```

You can paste this directly into a PR review comment. It tells reviewers exactly where to focus.

---

## Signal 2: Blind-edit detection

```bash
h5i notes coverage
```

```
── Attention Coverage ─────────────────────────────────────────
  Session a3f8c12  ·  4 files edited

  File                          Reads  Edits  Blind?  Coverage
  ─────────────────────────────────────────────────────────────
  src/auth.rs                     4      3      —       100%
  src/session.rs                  2      1      —       100%
  src/billing/token.rs            0      2     BLIND     0%
  src/middleware/mod.rs           1      1      —       100%

  ⚠ 1 blind edit detected: src/billing/token.rs
    Claude modified this file without reading it in this session.
```

The `BLIND` flag is the one that should give you pause. `src/billing/token.rs` had zero reads and two edits. Claude was writing to that file without inspecting its current state in this session.

This doesn't always mean a bug. If the file is simple and Claude has strong prior context, a blind edit might be fine. But in payment code? That's a mandatory human review.

### Filtering to the riskiest files

```bash
h5i notes coverage --max-ratio 0.5
```

This shows only files below 50% read-to-edit coverage — your highest-risk edits from the session.

---

## Signal 3: The composite review surface

Individual signals are useful. The composite score is better for prioritization across multiple sessions.

```bash
h5i notes review --limit 10
```

```
Suggested Review Points — 3 commits flagged (scanned 10, min_score=0.40)
──────────────────────────────────────────────────────────────────────────
  #1  a3f8c12  score 0.81  ████████░░
     Alice · 2026-04-20 14:02 UTC
     refactor billing token refresh
     ⚠ high uncertainty (3 moments) · 1 blind edit · 6 files touched

  #2  b7e29a1  score 0.53  █████░░░░░
     Alice · 2026-04-19 09:44 UTC
     fix session expiry edge case
     moderate uncertainty · 4 edits

  #3  c4d18f3  score 0.41  ████░░░░░░
     Bob · 2026-04-18 16:12 UTC
     update rate limiting
     low uncertainty · 2 blind edits
```

Scores above 0.7 are high-priority review candidates. The `score 0.81` commit — the billing refactor — combines high uncertainty *and* a blind edit. That combination is meaningful: Claude wasn't sure what it was doing, and it was doing it without reading the file first.

The `--limit` flag scans the last N analyzed commits. Useful in CI or in a pre-merge checklist:

```bash
h5i notes review --limit 50
```

---

## The proactive review surface in context status

If you use h5i's context workspace (Part 3 covers this in detail), `h5i context status` includes review signals automatically:

```bash
h5i context status
```

```
── h5i-ctx · branch: main ─────────────────────────────────────
  Goal: refactor billing token refresh

  Commits: 4  |  Traces: 23  |  Branch: main

── Flagged Commits ─────────────────────────────────────────────
  ⚠ a3f8c12  score 0.81  refactor billing token refresh
     high uncertainty · blind edit in src/billing/token.rs
  ⚠ b7e29a1  score 0.53  fix session expiry edge case
```

This surfaces at the *start* of your next session. Before you continue working, you already know which commits from the previous session need review. You don't have to remember to run `h5i notes review` — it's part of your morning standup.

---

## A practical pre-merge workflow

Here's the two-minute checklist before merging any AI-assisted PR:

```bash
# 1. Check for blind edits
h5i notes coverage

# 2. Check for uncertainty in touched files
h5i notes uncertainty

# 3. Get the composite score
h5i notes review --limit 20
```

If the composite score is above 0.7, the PR gets a mandatory human review of the flagged files before merge. Below 0.4, it passes through.

You can encode this in your PR template or CI pipeline. The score is machine-readable:

```bash
h5i notes review --limit 20 --json | jq '.[] | select(.score > 0.7)'
```

---

## What this looks like in a real review

Here's what your PR description now looks like with h5i signals attached:

```
## AI Review Surface

Session: a3f8c12 · model: claude-sonnet-4-6

Files needing human review:
- src/billing/token.rs  ← BLIND EDIT + 3 uncertainty moments
  Claude expressed doubt about token expiry ordering (t:14, t:19, t:26)
  and modified this file without reading it first.

- src/auth/session.rs  ← 2 uncertainty moments
  Claude was uncertain about TTL/renewal window interaction (t:22)

All other files: full read coverage, no uncertainty signals.
```

That's a specific, actionable review request — not "Claude wrote it, please review." It tells the reviewer exactly which files to scrutinize and exactly what Claude was uncertain about. Reviews that would have taken an hour get done in ten minutes.

---

## The underlying insight

A clean diff doesn't mean a careful edit. Git shows you what changed; `h5i notes` shows you *how the AI was thinking* when it made that change. Uncertainty and blind edits are the two leading indicators of latent bugs in AI-generated code — not because Claude is bad at coding, but because AI agents have variable confidence and variable context coverage, and both should be visible before code ships.

---

## Coming up next

In Part 3 we'll cover the other half of h5i's answer to AI amnesia: the **context workspace** — a versioned reasoning trail that survives session resets.

You'll see how `h5i context init`, `h5i context trace`, and the PostToolUse hook turn every Claude session into a structured reasoning log that carries forward into the next session automatically. The result: a Claude that always knows what it was thinking last time, even after you close the terminal.

---

*h5i is open source. Source: [github.com/Koukyosyumei/h5i](https://github.com/Koukyosyumei/h5i)*

*Tags: AI, Developer Tools, Git, Code Review, Software Engineering*
