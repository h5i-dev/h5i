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

## Commit 69e4c71b — 2026-04-19 12:14 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary
src/redact.rs: 12-rule regex + entropy scanner, wired into save_session/create_checkpoint/save_analysis. src/trail.rs: branch-level Trail struct with init/show/update/link-pr/list; h5i checkpoint auto-links into trail. All 304 tests pass.

### This Commit's Contribution
DAG trace nodes (dag.json per branch, parent links, merge nodes), ephemeral traces (ephemeral.md, cleared on commit, not in DAG/snapshots), 3-pass lossless pack (subsumption + consolidation + preservation), stable-prefix counts on GccContext, scope sub-contexts (scope/ prefix, shown separately in status, metadata tag). 57/57 tests pass.

---

## Commit 69e4c9d0 — 2026-04-19 12:25 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary
DAG trace nodes (dag.json per branch, parent links, merge nodes), ephemeral traces (ephemeral.md, cleared on commit, not in DAG/snapshots), 3-pass lossless pack (subsumption + consolidation + preservation), stable-prefix counts on GccContext, scope sub-contexts (scope/ prefix, shown separately in status, metadata tag). 57/57 tests pass.

### This Commit's Contribution
DAG nodes with sha256 IDs in dag.json; ephemeral.md cleared on milestone; stable-prefix boundary at last-40 lines; scope/<name> branches; three-pass lossless pack. README, MANUAL, man page updated. 57 tests pass.

---

