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

## Commit 69fa743c — 2026-05-05 22:50 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary


### This Commit's Contribution
Removed max-width: 1200px / margin: 0 auto from .ctx-view so the dashboard fills the viewport like Explore mode. Inner caps preserve reading line-length: hero text 60ch (~470px), hero purpose 80ch, milestones list 90ch, activity body 90ch. Wider padding kicks in at >=1600px (28px -> 36px). Tables and DAG fill the available width naturally.

---

## Commit 69fa744e — 2026-05-05 22:50 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary
Removed max-width: 1200px / margin: 0 auto from .ctx-view so the dashboard fills the viewport like Explore mode. Inner caps preserve reading line-length: hero text 60ch (~470px), hero purpose 80ch, milestones list 90ch, activity body 90ch. Wider padding kicks in at >=1600px (28px -> 36px). Tables and DAG fill the available width naturally.

### This Commit's Contribution


---

## Commit 69fa74df — 2026-05-05 22:53 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 69fa7577 — 2026-05-05 22:55 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary


### This Commit's Contribution
Reorganised Context dashboard rows: was full/full/full/2-col(milestones+activity)/full(DAG)/full(branches)/full(snapshots), now full/full/2-col(OTA+Milestones)/full(DAG)/full(activity)/full(branches)/full(snapshots). OTA balance and Recent milestones share a row sized 0.8fr/1.2fr (OTA narrower, milestones wider) with align-items: start so OTA doesn't stretch awkwardly when milestones list is tall. Recent activity now stands alone full-width below DAG so long OBSERVE/THINK content has room to breathe.

---

## Commit 69fa7582 — 2026-05-05 22:56 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary
Reorganised Context dashboard rows: was full/full/full/2-col(milestones+activity)/full(DAG)/full(branches)/full(snapshots), now full/full/2-col(OTA+Milestones)/full(DAG)/full(activity)/full(branches)/full(snapshots). OTA balance and Recent milestones share a row sized 0.8fr/1.2fr (OTA narrower, milestones wider) with align-items: start so OTA doesn't stretch awkwardly when milestones list is tall. Recent activity now stands alone full-width below DAG so long OBSERVE/THINK content has room to breathe.

### This Commit's Contribution


---

## Commit 69fa768b — 2026-05-05 23:00 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary


### This Commit's Contribution
Restructured to match Explore's 3-pane rhythm: Recent milestones | Reasoning DAG | Recent activity. OTA balance restored to full width above the row. Each column max-height 720px (DAG 660 since it has its own canvas). Milestone and activity items now render as single horizontally-scrollable lines with thin scrollbars and a 24px right-edge fade mask so clipped content is visible. ctx-row-three uses 1fr/1.4fr/1fr at >=1280px (DAG gets the wider middle), 1fr/1.6fr/1fr at narrower widths, collapses to 1 col at <=1000px. Sticky section headers within columns.

---

## Commit 69fa769d — 2026-05-05 23:00 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary
Restructured to match Explore's 3-pane rhythm: Recent milestones | Reasoning DAG | Recent activity. OTA balance restored to full width above the row. Each column max-height 720px (DAG 660 since it has its own canvas). Milestone and activity items now render as single horizontally-scrollable lines with thin scrollbars and a 24px right-edge fade mask so clipped content is visible. ctx-row-three uses 1fr/1.4fr/1fr at >=1280px (DAG gets the wider middle), 1fr/1.6fr/1fr at narrower widths, collapses to 1 col at <=1000px. Sticky section headers within columns.

### This Commit's Contribution


---

## Commit 69fa7805 — 2026-05-05 23:06 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary


### This Commit's Contribution
Built HSplit.tsx: 3-pane horizontal splitter w/ draggable 1px dividers (7px hit area via ::before, blue on hover/active), localStorage persistence under h5i.ctx.three-{left,right}. ctx-three-bleed: edge-to-edge row via negative margins escaping ctx-view padding, top+bottom borders, 720px tall. CtxPane: workbench-style pane (sticky uppercase header + scrollable body), reuses Explore's surface/border tokens. DAG pane drops body padding so canvas spans pane width. Mobile (<=1000px): bleed disengages, panes stack vertically, dividers hidden.

---

## Commit 69fa7820 — 2026-05-05 23:07 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary
Built HSplit.tsx: 3-pane horizontal splitter w/ draggable 1px dividers (7px hit area via ::before, blue on hover/active), localStorage persistence under h5i.ctx.three-{left,right}. ctx-three-bleed: edge-to-edge row via negative margins escaping ctx-view padding, top+bottom borders, 720px tall. CtxPane: workbench-style pane (sticky uppercase header + scrollable body), reuses Explore's surface/border tokens. DAG pane drops body padding so canvas spans pane width. Mobile (<=1000px): bleed disengages, panes stack vertically, dividers hidden.

### This Commit's Contribution


---

## Commit 69faa5e0 — 2026-05-06 02:22 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary


### This Commit's Contribution
PromotionFlow and OtaBalance are diagnostics, not primary content. Moved them below Branches and Snapshots so users land on the working surface (3-pane Milestones/DAG/Activity) immediately after the goal hero. New flow: Hero (what's the project about?) -> 3-pane workbench (drill down) -> open TODOs -> Branches -> Snapshots -> Promotion pipeline + OTA balance (diagnostics).

---

## Commit 69faa5e6 — 2026-05-06 02:22 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary
PromotionFlow and OtaBalance are diagnostics, not primary content. Moved them below Branches and Snapshots so users land on the working surface (3-pane Milestones/DAG/Activity) immediately after the goal hero. New flow: Hero (what's the project about?) -> 3-pane workbench (drill down) -> open TODOs -> Branches -> Snapshots -> Promotion pipeline + OTA balance (diagnostics).

### This Commit's Contribution


---

## Commit 69faa68e — 2026-05-06 02:25 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 69faa6fb — 2026-05-06 02:27 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 69faa85a — 2026-05-06 02:32 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary


### This Commit's Contribution
1) Signature accent: redefined :root tokens — bg #0a0c10 (was #1c2127), surface #12161c, accent mint #42e2d0 / hi #7cefe0. Mapped --bp-blue/blue-hi/blue-bg to accent so existing rules cascade. 2) Borders pruned on .ctx-section and .ctx-hero — surface contrast does separation now. 3) KPI tertiary line via .ctx-kpi-sub (mono tabular-nums, 10px dim) — Milestones shows '<branches> branches', Trace shows '<stable> stable · <live> live', Snapshots shows 'last <relative>', TODOs shows 'across <branches> branches', Branches shows '<stale> stale'. 4) OTA legend gains percent column. Hardcoded blue rgbas in 6 places replaced with var(--h5-accent-bg) / accent rgbas. Violet bumped from #8f5fbf to #b48bd9 to read against darker bg. Greens / oranges / reds brightened slightly to match.

---

## Commit 69faa874 — 2026-05-06 02:33 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary
1) Signature accent: redefined :root tokens — bg #0a0c10 (was #1c2127), surface #12161c, accent mint #42e2d0 / hi #7cefe0. Mapped --bp-blue/blue-hi/blue-bg to accent so existing rules cascade. 2) Borders pruned on .ctx-section and .ctx-hero — surface contrast does separation now. 3) KPI tertiary line via .ctx-kpi-sub (mono tabular-nums, 10px dim) — Milestones shows '<branches> branches', Trace shows '<stable> stable · <live> live', Snapshots shows 'last <relative>', TODOs shows 'across <branches> branches', Branches shows '<stale> stale'. 4) OTA legend gains percent column. Hardcoded blue rgbas in 6 places replaced with var(--h5-accent-bg) / accent rgbas. Violet bumped from #8f5fbf to #b48bd9 to read against darker bg. Greens / oranges / reds brightened slightly to match.

### This Commit's Contribution


---

## Commit 69faab4b — 2026-05-06 02:45 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 69faacd8 — 2026-05-06 02:52 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary


### This Commit's Contribution
Extended BranchInfo with: ahead/behind (via git2 graph_ahead_behind, when upstream exists), last_commit (oid+short+msg+author+ISO ts), ai_commit_count + walked_commit_count (walks last 100 commits via revwalk + load_h5i_record, checks ai_metadata), context (ContextBranchLink: purpose, last_milestone, last_activity, milestone_count, trace_lines, snapshot_count, todo_count — populated when a same-named context branch exists), has_context_branch flag. Helpers walk_branch_tip and build_context_branch_link factor reusable logic. Heavy work skipped for remote branches. Tests: 449/449 pass.

---

## Commit 69faacee — 2026-05-06 02:52 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary
Extended BranchInfo with: ahead/behind (via git2 graph_ahead_behind, when upstream exists), last_commit (oid+short+msg+author+ISO ts), ai_commit_count + walked_commit_count (walks last 100 commits via revwalk + load_h5i_record, checks ai_metadata), context (ContextBranchLink: purpose, last_milestone, last_activity, milestone_count, trace_lines, snapshot_count, todo_count — populated when a same-named context branch exists), has_context_branch flag. Helpers walk_branch_tip and build_context_branch_link factor reusable logic. Heavy work skipped for remote branches. Tests: 449/449 pass.

### This Commit's Contribution


---

## Commit 69faaeb1 — 2026-05-06 03:00 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary


### This Commit's Contribution
api.ts: extended BranchInfo with ahead/behind/last_commit/ai_commit_count/walked_commit_count/context (ContextBranchLink)/has_context_branch. ContextView: AllCtx now loads /api/branches in parallel; activeBranch derived from is_head; passed into Hero. Hero: shows active branch's context.purpose as primary text (eyebrow 'Branch intent · X') with project goal as secondary; falls back to project goal when no linked ctx, with inline CTA showing the h5i context branch CLI command. BranchesTable: rewritten to consume BranchInfo[] (local only); 9 columns (freshness/branch+HEAD+no-ctx tag/purpose/last activity/ahead↑↓behind/AI sparkbar over walked count/milestones/trace/todos). 'no ctx' tag with tooltip when has_context_branch=false. AheadBehindCell w/ green↑/orange↓ arrows or 'even' or '—'. BranchPicker: shows mint dot next to branches with linked context, ahead/behind in label instead of just upstream when divergent. New CSS: .wb-branch-ctx-dot (mint, glow), .ctx-hero-cta (left-accent panel).

---

## Commit 69faaec9 — 2026-05-06 03:00 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary
api.ts: extended BranchInfo with ahead/behind/last_commit/ai_commit_count/walked_commit_count/context (ContextBranchLink)/has_context_branch. ContextView: AllCtx now loads /api/branches in parallel; activeBranch derived from is_head; passed into Hero. Hero: shows active branch's context.purpose as primary text (eyebrow 'Branch intent · X') with project goal as secondary; falls back to project goal when no linked ctx, with inline CTA showing the h5i context branch CLI command. BranchesTable: rewritten to consume BranchInfo[] (local only); 9 columns (freshness/branch+HEAD+no-ctx tag/purpose/last activity/ahead↑↓behind/AI sparkbar over walked count/milestones/trace/todos). 'no ctx' tag with tooltip when has_context_branch=false. AheadBehindCell w/ green↑/orange↓ arrows or 'even' or '—'. BranchPicker: shows mint dot next to branches with linked context, ahead/behind in label instead of just upstream when divergent. New CSS: .wb-branch-ctx-dot (mint, glow), .ctx-hero-cta (left-accent panel).

### This Commit's Contribution


---

## Commit 69faaf55 — 2026-05-06 03:02 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 69fab0d3 — 2026-05-06 03:09 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary


### This Commit's Contribution
Wired /api/context/diff into SnapshotsTable: row click toggles a diff drawer (newer→older pair) with sections for goal change (red strikethrough from-goal vs green to-goal), milestones added/removed, trace delta (capped 8 each side). React.Fragment used to inject second tr after expandable rows. Empty state when no changes. Cross-branch warning tag when from/to branches differ. RecentActivity (in clipped 3-pane mode): each entry is now click-to-expand. Collapsed: single-line scroll w/ fade mask (existing). Expanded: pre-wrap multi-line, max-height 280px with vertical scroll, mint left-border accent, chevron flips ▸/▾. Tracked via expandedIds Set in component state.

---

## Commit 69fab0eb — 2026-05-06 03:09 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary
Wired /api/context/diff into SnapshotsTable: row click toggles a diff drawer (newer→older pair) with sections for goal change (red strikethrough from-goal vs green to-goal), milestones added/removed, trace delta (capped 8 each side). React.Fragment used to inject second tr after expandable rows. Empty state when no changes. Cross-branch warning tag when from/to branches differ. RecentActivity (in clipped 3-pane mode): each entry is now click-to-expand. Collapsed: single-line scroll w/ fade mask (existing). Expanded: pre-wrap multi-line, max-height 280px with vertical scroll, mint left-border accent, chevron flips ▸/▾. Tracked via expandedIds Set in component state.

### This Commit's Contribution


---

## Commit 69fab2a1 — 2026-05-06 03:16 UTC

### Branch Purpose
Primary development branch

### Previous Progress Summary


### This Commit's Contribution
Added parse_commit_milestones() in server.rs that extracts {sha_short, timestamp, contribution} from each '## Commit <sha> — <ts>' header in commit.md (existing parse_commit_contributions now thin wrapper that drops sha/ts). New endpoint /api/context/milestones?branch=X returns Vec<ContextMilestoneEntry>. Frontend: api.ts adds ContextMilestoneEntry + contextMilestones(branch?). ContextView loads it in parallel with the other context calls; falls back to show.milestones strings when context not initialised. Each milestone row in the panel now shows a mint-tinted mono SHA chip (7-char) followed by the contribution text; tooltip on the chip reveals 'Context commit <sha> · <timestamp>'. 449/449 tests pass.

---

