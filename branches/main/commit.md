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

## Commit 69e4e7cb — 2026-04-19 14:33 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary
DAG nodes with sha256 IDs in dag.json; ephemeral.md cleared on milestone; stable-prefix boundary at last-40 lines; scope/<name> branches; three-pass lossless pack. README, MANUAL, man page updated. 57 tests pass.

### This Commit's Contribution
Key Decisions now requires technical content; rel_path strips CWD from notes output; ACT char limit 120; system prompt includes relevant+commit+notes-analyze; all 290 tests pass

---

## Commit 69e4f796 — 2026-04-19 15:41 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary
Key Decisions now requires technical content; rel_path strips CWD from notes output; ACT char limit 120; system prompt includes relevant+commit+notes-analyze; all 290 tests pass

### This Commit's Contribution
1) auto-inject relevant context on Read via hook; 2) ThoughtEntry struct in session_log captures thinking with file context; 3) h5i commit --add <paths> stages files before committing; 4) h5i context knowledge distills THINK entries across branches; 5) h5i context status shows proactive review surface from suggest_review_points

---

## Commit 69e6bc30 — 2026-04-20 23:52 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary
1) auto-inject relevant context on Read via hook; 2) ThoughtEntry struct in session_log captures thinking with file context; 3) h5i commit --add <paths> stages files before committing; 4) h5i context knowledge distills THINK entries across branches; 5) h5i context status shows proactive review surface from suggest_review_points

### This Commit's Contribution
34 CLI subprocess tests in tests/cli_integration.rs (all passing); 5-target release workflow in .github/workflows/release.yaml triggered on v* tags

---

## Commit 69e83f50 — 2026-04-22 03:24 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary
34 CLI subprocess tests in tests/cli_integration.rs (all passing); 5-target release workflow in .github/workflows/release.yaml triggered on v* tags

### This Commit's Contribution


---

## Commit 69ea0faf — 2026-04-23 12:25 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary


### This Commit's Contribution
Restructured README from 301 → 204 lines. New shape: Quick start (init → hook setup → code normally → push) is the backbone; auditing grouped as vibe/context scan/compliance/notes review; a short 'Under the hood' table documents refs/h5i/{notes,context,ast,checkpoints/<agent>} per user's ask for a technical taste; context-subcommand depth collapsed into 'Other things h5i does'.

---

