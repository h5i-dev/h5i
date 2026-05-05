# Branch: main

**Purpose:** Primary development branch

_Commits will be appended below._

## Commit 69fa4e7d — 2026-05-05 20:09 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary


### This Commit's Contribution
Added Commands::Pull clap variant + handler in src/main.rs that fans 'git fetch +refspec' across refs/h5i/{notes,memory,context,ast}, distinguishes 'not present on remote' from real failures, and prints a Tip footer. Two new round-trip integration tests in tests/cli_integration.rs cover (1) push from sender → pull on receiver via a bare remote and (2) graceful skip when the remote has no h5i refs. All 435 tests pass.

---

## Commit 69fa52fc — 2026-05-05 20:28 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary
Added Commands::Pull clap variant + handler in src/main.rs that fans 'git fetch +refspec' across refs/h5i/{notes,memory,context,ast}, distinguishes 'not present on remote' from real failures, and prints a Tip footer. Two new round-trip integration tests in tests/cli_integration.rs cover (1) push from sender → pull on receiver via a bare remote and (2) graceful skip when the remote has no h5i refs. All 435 tests pass.

### This Commit's Contribution
1. Added --force flag to Pull. 2. Pull now fetches into temp ref refs/h5i/_incoming/<base> and classifies the relationship: missing-on-remote / new / up-to-date / fast-forward / local-ahead / diverged. 3. On notes divergence we union-merge via a new helper (union_merge_trees + union_merge_notes_commits, git2-based), since 'git notes merge' refuses refs outside refs/notes/. 4. On non-notes divergence we keep local unless --force. 5. Seven new e2e tests cover every branch (idempotent, fast-forward, local-ahead, notes union merge preserves both sides, context kept without force, context overwritten with force, notes still merged under --force). All 442 tests pass.

---

## Commit 69fa5f38 — 2026-05-05 21:20 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary
1. Added --force flag to Pull. 2. Pull now fetches into temp ref refs/h5i/_incoming/<base> and classifies the relationship: missing-on-remote / new / up-to-date / fast-forward / local-ahead / diverged. 3. On notes divergence we union-merge via a new helper (union_merge_trees + union_merge_notes_commits, git2-based), since 'git notes merge' refuses refs outside refs/notes/. 4. On non-notes divergence we keep local unless --force. 5. Seven new e2e tests cover every branch (idempotent, fast-forward, local-ahead, notes union merge preserves both sides, context kept without force, context overwritten with force, notes still merged under --force). All 442 tests pass.

### This Commit's Contribution
Additive override block at end of <style> in src/server.rs. Tokens: --bp-bg, --bp-surface, --bp-elev, --bp-border, --bp-text*, --bp-blue/green/orange/red/violet, --bp-radius (2px). Verified: cargo build ok, server serves index 143KB with 44 occurrences of bp-* tokens, /api endpoints respond. Reversible by deleting one CSS block. Not yet committed to git.

---

## Commit 69fa5f42 — 2026-05-05 21:21 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary
Additive override block at end of <style> in src/server.rs. Tokens: --bp-bg, --bp-surface, --bp-elev, --bp-border, --bp-text*, --bp-blue/green/orange/red/violet, --bp-radius (2px). Verified: cargo build ok, server serves index 143KB with 44 occurrences of bp-* tokens, /api endpoints respond. Reversible by deleting one CSS block. Not yet committed to git.

### This Commit's Contribution


---

## Commit 69fa60b9 — 2026-05-05 21:27 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary


### This Commit's Contribution
Visual depth pass. HTML edits remove emoji from primary nav. CSS: smaller base size, tabular numerics, themed scrollbars, focus rings, sticky table headers, flat sidebar sections, uppercase tabs. Reversible by deleting iteration-2 CSS block plus reverting ~12 small HTML edits.

---

## Commit 69fa60ca — 2026-05-05 21:27 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary
Visual depth pass. HTML edits remove emoji from primary nav. CSS: smaller base size, tabular numerics, themed scrollbars, focus rings, sticky table headers, flat sidebar sections, uppercase tabs. Reversible by deleting iteration-2 CSS block plus reverting ~12 small HTML edits.

### This Commit's Contribution


---

## Commit 69fa6144 — 2026-05-05 21:29 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 69fa6367 — 2026-05-05 21:38 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary


### This Commit's Contribution
3-pane workbench: virtualized commit table (left), CommitDetail w/ identity/AI/tests/structure sections (center), CrossRef stat panel (right). Linked selection across panes. Legacy / kept untouched. Next: more API endpoints in xref pane (sessions, intent links), Memory/Sessions/Context tab parity, then flip / to bundle.

---

## Commit 69fa637a — 2026-05-05 21:39 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary
3-pane workbench: virtualized commit table (left), CommitDetail w/ identity/AI/tests/structure sections (center), CrossRef stat panel (right). Linked selection across panes. Legacy / kept untouched. Next: more API endpoints in xref pane (sessions, intent links), Memory/Sessions/Context tab parity, then flip / to bundle.

### This Commit's Contribution


---

## Commit 69fa63b7 — 2026-05-05 21:40 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 69fa67e8 — 2026-05-05 21:58 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary


### This Commit's Contribution
Right pane: Refs (intent-graph parents/children/causal links, click to navigate), Sessions (per-commit session-log), Integrity (auto-runs audit on selection), Context (workspace status). Modes: Review (sortable review-points table w/ score bars), Memory (snapshot list). Build hook: build.rs detects stale dist via mtime, invokes npm install/build, opt-out via H5I_SKIP_WEB_BUILD env. Asset serving fixed: /assets/*path handler prepends 'assets/' before rust-embed lookup. Legacy UI preserved at /legacy for unmigrated Intent Graph viz. Not yet visually verified in browser.

---

## Commit 69fa6800 — 2026-05-05 21:58 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary
Right pane: Refs (intent-graph parents/children/causal links, click to navigate), Sessions (per-commit session-log), Integrity (auto-runs audit on selection), Context (workspace status). Modes: Review (sortable review-points table w/ score bars), Memory (snapshot list). Build hook: build.rs detects stale dist via mtime, invokes npm install/build, opt-out via H5I_SKIP_WEB_BUILD env. Asset serving fixed: /assets/*path handler prepends 'assets/' before rust-embed lookup. Legacy UI preserved at /legacy for unmigrated Intent Graph viz. Not yet visually verified in browser.

### This Commit's Contribution


---

## Commit 69fa6aa5 — 2026-05-05 22:09 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary


### This Commit's Contribution
Replaced Blueprint Navbar absolute-centred Alignment groups with flex layout. Right pane is now Refs/Sessions/Integrity (3 tabs). Context promoted to top-level mode. rust-embed debug-embed removed for read-from-disk in debug.

---

## Commit 69fa6b02 — 2026-05-05 22:11 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary
Replaced Blueprint Navbar absolute-centred Alignment groups with flex layout. Right pane is now Refs/Sessions/Integrity (3 tabs). Context promoted to top-level mode. rust-embed debug-embed removed for read-from-disk in debug.

### This Commit's Contribution


---

## Commit 69fa6d73 — 2026-05-05 22:21 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary


### This Commit's Contribution
GitHub linking, branch picker, persistent context strip, per-commit Context tab. Backend: get_log_at_branch, /api/branches, /api/commit-files.

---

## Commit 69fa6d8b — 2026-05-05 22:22 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary
GitHub linking, branch picker, persistent context strip, per-commit Context tab. Backend: get_log_at_branch, /api/branches, /api/commit-files.

### This Commit's Contribution


---

## Commit 69fa6efe — 2026-05-05 22:28 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary


### This Commit's Contribution
Replaced thin ContextTab (goal+stats only) with ContextView calling 5 endpoints in parallel. Sections: hero with KPI tiles, promotion pipeline flow, OTA balance bar, recent milestones, recent trace with timestamps, open TODOs, branches table, snapshot history.

---

## Commit 69fa6f16 — 2026-05-05 22:28 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary
Replaced thin ContextTab (goal+stats only) with ContextView calling 5 endpoints in parallel. Sections: hero with KPI tiles, promotion pipeline flow, OTA balance bar, recent milestones, recent trace with timestamps, open TODOs, branches table, snapshot history.

### This Commit's Contribution


---

## Commit 69fa7045 — 2026-05-05 22:33 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary


### This Commit's Contribution
Earlier iterations cranked density too far. Pass 1 (CSS): html base 14px, line-height 1.5, all major surfaces +1-2px. Body 14, table 13, eyebrow 11 (was 10), monospace 12 (was 11), commit message 17 (was 15), KPI label 10 (was 9). Pass 2 (TSX inline): bumped 27 hardcoded fontSize values across 11 files. Tags default to 12px instead of forcing 10. Bundle CSS 352KB (was 344). Not regressed: density still tight enough that tables stay information-dense.

---

## Commit 69fa705b — 2026-05-05 22:34 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary
Earlier iterations cranked density too far. Pass 1 (CSS): html base 14px, line-height 1.5, all major surfaces +1-2px. Body 14, table 13, eyebrow 11 (was 10), monospace 12 (was 11), commit message 17 (was 15), KPI label 10 (was 9). Pass 2 (TSX inline): bumped 27 hardcoded fontSize values across 11 files. Tags default to 12px instead of forcing 10. Bundle CSS 352KB (was 344). Not regressed: density still tight enough that tables stay information-dense.

### This Commit's Contribution


---

## Commit 69fa7234 — 2026-05-05 22:41 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary


### This Commit's Contribution
DagViz.tsx: 5-lane SVG layout (OBSERVE/THINK/ACT/NOTE/MERGE) with HTML nodes positioned absolutely and an SVG layer behind drawing cubic-bezier parent->child edges. Default 40 last nodes, 'Show all' toggle. Hover highlights a node and tints all incident edges with the lane colour. RecentActivity: replaces the old mini_trace summary list with full DAG node content (4-line clamp), kind tag, timestamp, and node id — finally surfaces what OBSERVE / THINK / ACT entries actually said. Bundle CSS 354KB.

---

## Commit 69fa724a — 2026-05-05 22:42 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary
DagViz.tsx: 5-lane SVG layout (OBSERVE/THINK/ACT/NOTE/MERGE) with HTML nodes positioned absolutely and an SVG layer behind drawing cubic-bezier parent->child edges. Default 40 last nodes, 'Show all' toggle. Hover highlights a node and tints all incident edges with the lane colour. RecentActivity: replaces the old mini_trace summary list with full DAG node content (4-line clamp), kind tag, timestamp, and node id — finally surfaces what OBSERVE / THINK / ACT entries actually said. Bundle CSS 354KB.

### This Commit's Contribution


---

## Commit 69fa7371 — 2026-05-05 22:47 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary


### This Commit's Contribution
Default mode is now Context (Context|Explore|Review|Memory order). Branches table: sortable headers w/ ▴/▾ indicators, active branch pinned to top with blue left-border accent and HEAD tag, freshness dot column (green=<1h, orange=<1d, gray=stale), inline sparkbars for milestones (violet) and trace (blue), exclusive cell w/ subtle units. Snapshots table: pinned newest at top, Δ column showing time delta to previous snapshot, goal-evolution Icon when goal changed between snapshots. CSS: .ctx-table sort indicators, .ctx-fresh-dot, .ctx-spark-track/fill, active row inset shadow.

---

## Commit 69fa738a — 2026-05-05 22:47 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary
Default mode is now Context (Context|Explore|Review|Memory order). Branches table: sortable headers w/ ▴/▾ indicators, active branch pinned to top with blue left-border accent and HEAD tag, freshness dot column (green=<1h, orange=<1d, gray=stale), inline sparkbars for milestones (violet) and trace (blue), exclusive cell w/ subtle units. Snapshots table: pinned newest at top, Δ column showing time delta to previous snapshot, goal-evolution Icon when goal changed between snapshots. CSS: .ctx-table sort indicators, .ctx-fresh-dot, .ctx-spark-track/fill, active row inset shadow.

### This Commit's Contribution


---

## Commit 69fa73d7 — 2026-05-05 22:48 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary


### This Commit's Contribution


---

