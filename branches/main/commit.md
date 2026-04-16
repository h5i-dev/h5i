# Branch: main

**Purpose:** Primary development branch

_Commits will be appended below._

## Commit 69dffb71 — 2026-04-15 20:56 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary


### This Commit's Contribution
Added snapshot_for_commit, restore, context_diff, relevant, pack to ctx.rs. Wired four new CLI subcommands (restore/diff/relevant/pack) in main.rs. Auto-snapshot on h5i commit. 15 new tests, all passing.

---

## Commit 69e00157 — 2026-04-15 21:21 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary
Added snapshot_for_commit, restore, context_diff, relevant, pack to ctx.rs. Wired four new CLI subcommands (restore/diff/relevant/pack) in main.rs. Auto-snapshot on h5i commit. 15 new tests, all passing.

### This Commit's Contribution
All JS functions for Context tab added: loadContextTab, loadCtxTrace, renderCtxSnapshots, selectCtxSnap, clearCtxSelection, updateCtxControls, showCtxViewer, runCtxDiff, runCtxRelevant. Fixed API field name mismatches, added missing CSS classes, updated route test. 273/273 tests pass.

---

## Commit 69e02934 — 2026-04-16 00:11 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary
All JS functions for Context tab added: loadContextTab, loadCtxTrace, renderCtxSnapshots, selectCtxSnap, clearCtxSelection, updateCtxControls, showCtxViewer, runCtxDiff, runCtxRelevant. Fixed API field name mismatches, added missing CSS classes, updated route test. 273/273 tests pass.

### This Commit's Contribution
src/redact.rs: 12-rule regex + entropy scanner, wired into save_session/create_checkpoint/save_analysis. src/trail.rs: branch-level Trail struct with init/show/update/link-pr/list; h5i checkpoint auto-links into trail. All 304 tests pass.

---

