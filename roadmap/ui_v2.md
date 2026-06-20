````markdown
# UI Ideas for h5i as “Auditable Workspaces for AI Agents”

The short version is: **the best UI for h5i should not be a generic dashboard. It should feel like a “Flight Recorder” or “Black Box Replay” for AI agent runs.**

If the core concept is **“Auditable workspaces for AI agents,”** then the UI should make one thing instantly clear:

> What did the AI agent see, do, run, change, get blocked from accessing, and why did that result in this diff?

The strongest product experience would be an interface that lets users **replay the agent run behind the code change**.

---

## 1. Core UI: Agent Run Replay

The most compelling UI concept is:

```text
Prompt
  ↓
Files read
  ↓
Commands run
  ↓
Blocked accesses
  ↓
Tests failed / passed
  ↓
Agent messages
  ↓
Diff produced
  ↓
PR evidence
````

This should be presented as a **replayable timeline**.

Possible names:

```text
h5i Replay
Agent Flight Recorder
Workspace Black Box
Run Behind the Diff
```

Strong product copy:

> Replay the workspace behind the diff.

Or:

> See what your AI agent actually did before you merge.

Instead of positioning this as a “dashboard,” h5i should position it as **Replay**. That is much easier to understand and much more exciting.

---

## 2. Use a Three-Pane Layout

The ideal UI is probably a three-pane layout.

### Left Pane: Timeline

A chronological list of events:

```text
10:31  PROMPT     "Fix token refresh safely"
10:32  READ       src/auth/token.rs
10:33  READ       src/session.rs
10:34  RUN        cargo test
10:34  FAIL       test_refresh_expiry
10:36  THINK      expiry window too wide
10:37  EDIT       src/auth/token.rs
10:38  BLOCKED    curl https://evil.example
10:39  MSG        Codex flagged cache scope risk
10:41  PASS       142 tests
10:42  DIFF       2 files changed
```

The key is **not to show raw logs by default**.

h5i’s strength is that it can preserve raw logs while giving agents and humans compact summaries. The UI should follow the same principle:

* Show summaries first.
* Let users expand into raw logs only when needed.
* Make the “evidence” available without overwhelming the reviewer.

---

### Center Pane: Workspace Map

This is where the UI can become visually memorable.

Show the file tree as a heatmap:

```text
src/
  auth/
    token.rs       READ  EDIT  TESTED
    session.rs     READ  RISK
  billing/
    secret.rs      BLOCKED READ
tests/
  auth_test.rs     EDIT  PASS
```

Simple visual language:

```text
Blue   = read
Yellow = edited
Green  = tested / verified
Red    = blocked / risky / unverified
Gray   = untouched
```

The user should be able to understand at a glance:

* Which files the agent read
* Which files the agent edited
* Which files were edited without enough context
* Which accesses were blocked
* Which files need human review first

This is much more powerful than a plain log viewer.

---

### Right Pane: Evidence Drawer

When the user clicks an event, show the evidence behind it:

```text
Prompt
Model
Agent
Command
Exit code
Policy digest
Network allowlist
Files read/written
Raw log hash
Replay object
```

The key word here should be **Evidence**.

h5i should not ask users to “trust the AI.”
It should say:

> Here is the evidence.

---

## 3. The Best Demo: Blocked Access Replay

The most viral UI demo is not a successful run.
It is a run where the agent tries something risky and h5i blocks it.

Example demo:

```text
Claude is fixing auth in a sandboxed workspace.

✓ read src/auth.rs
✓ ran cargo test
✗ tried to access ~/.ssh/id_rsa — blocked
✗ tried to curl evil.example — blocked
✓ produced patch
✓ 142 tests passed

Apply to branch?  [Review diff] [Apply]
```

This immediately communicates the value of h5i.

It is not just logging the agent.
It is proving what the agent could and could not access.

Strong product copy:

> It didn’t just log the run. It proved what the agent could not reach.

This is the kind of screenshot or GIF that could work very well on X, Hacker News, Reddit, GitHub README, or a launch page.

---

## 4. PR UI: Reviewer Cockpit

For GitHub PRs, h5i should avoid generating a long Markdown dump.

Instead, it should create a compact **Reviewer Cockpit** card.

Example:

```text
h5i Review Cockpit

Merge confidence: 82 / 100
Prompt maturity: 81 / 100
Agent provenance: Claude → Codex review
Sandbox: supervised
Network: 1 blocked, 2 allowed
Tests: 142 passed, 0 failed
Risk: medium

Review first:
1. src/auth/token.rs — edited after failed expiry test
2. src/session.rs — Codex flagged cache-scope risk
3. .github/workflows/test.yml — CI config touched

Evidence:
[Replay run] [Open workspace map] [Raw logs] [Policy digest]
```

The order matters.

Bad order:

```text
Prompt
Model
Agent
Trace
Logs
Policy
...
```

Good order:

```text
Should I trust this PR?
Where should I review first?
What did the agent actually do?
What was blocked?
What evidence exists?
```

The UI should be designed around the reviewer’s real workflow, not around the internal data model.

---

## 5. “Why Did This Line Change?” UI

This would be a very h5i-native feature.

Inside the diff, each hunk could have a button:

```text
Why did this change?
```

Clicking it opens a small explanation:

```text
Prompt: "harden refresh window"
Agent: claude-code
Reasoning: expiry boundary can accept stale token
Command: cargo test test_refresh_expiry
Before: failed
After: passed
Codex review: cache scope risk resolved
```

GitHub shows **what changed**.

h5i should show:

> Why this changed, what evidence supports it, and what the agent did before changing it.

This is a strong way to connect prompts, tool use, tests, agent review, and final diffs.

---

## 6. Prompt Maturity Should Be a Coach, Not Just a Score

Prompt Maturity Score is useful, but the UI should not stop at a number.

Instead of only showing:

```text
Prompt Maturity: 58 / 100
```

Show actionable feedback:

```text
Prompt Maturity: 58 / 100

Weak spots:
- No acceptance criteria
- No files or modules specified
- No verification command
- No rollback instruction

Suggested upgrade:
"Modify src/auth/token.rs to fix token refresh expiry.
Do not change session storage.
Run cargo test auth::token_refresh.
If tests fail, summarize the failing case before editing again."
```

This is much more useful.

It also creates a great screenshot because the value is obvious even to people who do not know h5i.

However, h5i should be careful with positioning.
The score should not feel like a personal rating of the developer.

A useful disclaimer:

```text
Scores the task delegation, not the developer.
```

This makes the feature feel constructive rather than judgmental.

---

## 7. Agent Radio Should Feel Like Code Review, Not Slack

h5i’s agent messaging feature should not look like a normal chat app.

If it looks like Slack, people will think:

> Another chat UI.

Instead, Agent Radio should look more like a **code review thread** or **risk resolution graph**.

Example:

```text
Claude proposed change
  ↓
Codex flagged risk: session cache crosses requests
  ↓
Claude acknowledged
  ↓
Claude patched
  ↓
Codex re-reviewed
  ↓
Resolved
```

The key is to show agent communication as structured collaboration around risk, review, and resolution.

Not:

> agents chatting

But:

> agents producing auditable review evidence

This is much more aligned with the core concept of h5i.

---

## 8. Landing Page Hero: Show the Replay, Not Just the Terminal

A terminal demo is useful, but the landing page needs a visual that instantly communicates the product.

The ideal hero image:

```text
Left: GitHub PR diff
Right: h5i Replay
```

With copy like:

> Git shows what changed.
> h5i shows how the agent got there.

Or:

```text
Before h5i:
  reviewer sees only diff

After h5i:
  prompt → reads → commands → blocked access → tests → agent review → diff
```

The strongest homepage message might be:

```text
Git shows the diff.
h5i replays the agent run behind it.
```

Or even shorter:

```text
Review the run, not just the diff.
```

This directly explains why h5i exists.

---

## 9. MVP Priority

h5i does not need to build everything at once.

The MVP should focus on the smallest UI that demonstrates the core value.

### v0: PR Card

Improve the output of `h5i share pr post`.

The PR card should include:

```text
Trust summary
Review-first files
Prompt maturity
Sandbox proof
Tests
Agent handoffs
Replay link
```

This is probably the fastest path to a useful product experience.

---

### v1: Local Replay Page

Add a local replay view:

```bash
h5i replay latest
h5i replay <commit>
h5i serve --replay <run-id>
```

This page should show:

```text
Prompt
Timeline
Commands
File reads/writes
Blocked accesses
Tests
Diff
Raw logs
```

---

### v2: Workspace Heatmap

Add a file-tree view showing:

```text
Read
Edited
Tested
Blocked
Risky
Unverified
```

This gives h5i a visual identity.

---

### v3: Diff Hunk Provenance

Add “Why did this change?” explanations to each diff hunk.

This connects the final code change to:

```text
Prompt
Agent decision
Command output
Test result
Review message
```

---

### v4: Shareable GIF / Static Report

This is the growth feature.

Add export commands:

```bash
h5i replay export --format html
h5i replay export --format gif
h5i share replay
```

This would make h5i much easier to share on social media, GitHub issues, PRs, launch pages, and blog posts.

---

## 10. Best Product Framing

The best product framing is probably:

```text
h5i Replay
The flight recorder for AI coding agents.
```

Supporting copy:

```text
See the prompt, commands, blocked access, tests, agent handoffs, and evidence behind every AI-generated diff.
```

Another strong version:

```text
Review the run, not just the diff.
```

And a more Git-native version:

```text
Git tracks the diff.
h5i replays the agent run behind it.
```

---

## Final Recommendation

If h5i makes **“Auditable workspaces for AI agents”** its core concept, the UI should focus on three things:

```text
Agent Replay
PR Reviewer Cockpit
Sandbox Proof
```

The most important first product should be **h5i Replay**:

> A visual, replayable timeline of what the AI agent did inside the workspace before producing a diff.

The most viral demo would be:

```text
An AI agent tries to access a secret or network endpoint.
h5i blocks it.
The UI shows the blocked access, the final safe diff, and the full evidence trail.
```

That makes h5i feel much bigger than an AI logging tool.

It becomes:

> The auditable workspace layer for AI-generated code.

```
```

