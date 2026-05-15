# Branch: doc-cve

**Purpose:** write CVE blog posts on doc-cve git branch

_Commits will be appended below._

## Commit 69fb594f — 2026-05-06 15:07 UTC

### Branch Purpose
write CVE blog posts on doc-cve git branch

### Previous Progress Summary


### This Commit's Contribution
Wrote docs/blog/cve-2025-59536-startup-trust-dialog.html (CWE-94 startup trust dialog code injection, fixed in 1.0.111) and docs/blog/cve-2026-33068-bypass-permissions-settings.html (CWE-807 bypassPermissions via committed .claude/settings.json, fixed in 2.1.53). Both posts are technical write-ups with NVD/GHSA citations, severity tables, mitigation steps, and an h5i-as-incident-response framing. Wired both into docs/blog/index.html (cards + JSON-LD) and docs/sitemap.xml.

---

## Commit 69fb595b — 2026-05-06 15:08 UTC

### Branch Purpose
write CVE blog posts on doc-cve git branch

### Previous Progress Summary
Wrote docs/blog/cve-2025-59536-startup-trust-dialog.html (CWE-94 startup trust dialog code injection, fixed in 1.0.111) and docs/blog/cve-2026-33068-bypass-permissions-settings.html (CWE-807 bypassPermissions via committed .claude/settings.json, fixed in 2.1.53). Both posts are technical write-ups with NVD/GHSA citations, severity tables, mitigation steps, and an h5i-as-incident-response framing. Wired both into docs/blog/index.html (cards + JSON-LD) and docs/sitemap.xml.

### This Commit's Contribution


---

## Commit 69fce8ed — 2026-05-07 19:33 UTC

### Branch Purpose
write CVE blog posts on doc-cve git branch

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a072d83 — 2026-05-15 14:28 UTC

### Branch Purpose
write CVE blog posts on doc-cve git branch

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a07324f — 2026-05-15 14:48 UTC

### Branch Purpose
write CVE blog posts on doc-cve git branch

### Previous Progress Summary


### This Commit's Contribution
src/{session,delta_store,watcher}.rs deleted. yrs and notify removed from Cargo.toml. H5iCommitRecord.crdt_states removed. server.rs has_crdt badge removed. cargo test --lib targeted tests pass.

---

## Commit 6a0735cf — 2026-05-15 15:03 UTC

### Branch Purpose
write CVE blog posts on doc-cve git branch

### Previous Progress Summary
src/{session,delta_store,watcher}.rs deleted. yrs and notify removed from Cargo.toml. H5iCommitRecord.crdt_states removed. server.rs has_crdt badge removed. cargo test --lib targeted tests pass.

### This Commit's Contribution
capture/recall/audit/share are first-class subcommands. Legacy verbs hidden + deprecation-hinted. src/pr.rs renders Markdown body with prompt/model/decisions/test/review-flag per AI commit. Upsert is sticky via HTML MARKER and gh api PATCH. Build + test green.

---

## Commit 6a0735e2 — 2026-05-15 15:04 UTC

### Branch Purpose
write CVE blog posts on doc-cve git branch

### Previous Progress Summary
capture/recall/audit/share are first-class subcommands. Legacy verbs hidden + deprecation-hinted. src/pr.rs renders Markdown body with prompt/model/decisions/test/review-flag per AI commit. Upsert is sticky via HTML MARKER and gh api PATCH. Build + test green.

### This Commit's Contribution


---

## Commit 6a07397f — 2026-05-15 15:19 UTC

### Branch Purpose
write CVE blog posts on doc-cve git branch

### Previous Progress Summary


### This Commit's Contribution
README/MANUAL/man reflect new noun-group CLI. Helper text shows aligned tables + examples + did-you-mean. Stop-hook auto-trace + drop-CRDT also documented.

---

## Commit 6a07398e — 2026-05-15 15:19 UTC

### Branch Purpose
write CVE blog posts on doc-cve git branch

### Previous Progress Summary
README/MANUAL/man reflect new noun-group CLI. Helper text shows aligned tables + examples + did-you-mean. Stop-hook auto-trace + drop-CRDT also documented.

### This Commit's Contribution


---

## Commit 6a073af9 — 2026-05-15 15:25 UTC

### Branch Purpose
write CVE blog posts on doc-cve git branch

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a073e37 — 2026-05-15 15:39 UTC

### Branch Purpose
write CVE blog posts on doc-cve git branch

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a073f0f — 2026-05-15 15:43 UTC

### Branch Purpose
write CVE blog posts on doc-cve git branch

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a073f85 — 2026-05-15 15:45 UTC

### Branch Purpose
write CVE blog posts on doc-cve git branch

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a07406d — 2026-05-15 15:49 UTC

### Branch Purpose
write CVE blog posts on doc-cve git branch

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a074718 — 2026-05-15 16:17 UTC

### Branch Purpose
write CVE blog posts on doc-cve git branch

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a074aeb — 2026-05-15 16:33 UTC

### Branch Purpose
write CVE blog posts on doc-cve git branch

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a077c60 — 2026-05-15 20:04 UTC

### Branch Purpose
write CVE blog posts on doc-cve git branch

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a078164 — 2026-05-15 20:26 UTC

### Branch Purpose
write CVE blog posts on doc-cve git branch

### Previous Progress Summary


### This Commit's Contribution
Eliminated 19 production unwrap/expect + reframed 1 panic; canonicalize input paths + validate H5I_PARSER_DIR + 30s timeout (configurable) + 64MiB output cap on python AST parser; added tracing+tracing-subscriber wired in main.rs behind H5I_LOG/RUST_LOG (off by default, stderr writer). Discovered library-module println! calls are CLI display, not debug — kept them; only the new subprocess helper uses tracing::warn for genuine diagnostics. All 448 tests pass.

---

## Commit 6a078174 — 2026-05-15 20:26 UTC

### Branch Purpose
write CVE blog posts on doc-cve git branch

### Previous Progress Summary
Eliminated 19 production unwrap/expect + reframed 1 panic; canonicalize input paths + validate H5I_PARSER_DIR + 30s timeout (configurable) + 64MiB output cap on python AST parser; added tracing+tracing-subscriber wired in main.rs behind H5I_LOG/RUST_LOG (off by default, stderr writer). Discovered library-module println! calls are CLI display, not debug — kept them; only the new subprocess helper uses tracing::warn for genuine diagnostics. All 448 tests pass.

### This Commit's Contribution


---

## Commit 6a07852a — 2026-05-15 20:42 UTC

### Branch Purpose
write CVE blog posts on doc-cve git branch

### Previous Progress Summary


### This Commit's Contribution
Refactored find_parser_script + parser_timeout + run_parser_subprocess to take params instead of reading env globals, enabling 17 unit tests covering: parser_timeout default/zero/garbage/valid, find_parser_script override-wins/non-dir-rejected/missing/file-named-like-script-rejected/workdir-fallback/exe-fallback, run_parser stdout/non-zero-exit/empty-output/timeout-kill/missing-script. All 465 tests pass. AGENT.md/CLAUDE.md auto-generated templates require no update (agent workflow unchanged). MANUAL.md gained Environment Variables appendix covering 10 vars.

---

## Commit 6a07853a — 2026-05-15 20:42 UTC

### Branch Purpose
write CVE blog posts on doc-cve git branch

### Previous Progress Summary
Refactored find_parser_script + parser_timeout + run_parser_subprocess to take params instead of reading env globals, enabling 17 unit tests covering: parser_timeout default/zero/garbage/valid, find_parser_script override-wins/non-dir-rejected/missing/file-named-like-script-rejected/workdir-fallback/exe-fallback, run_parser stdout/non-zero-exit/empty-output/timeout-kill/missing-script. All 465 tests pass. AGENT.md/CLAUDE.md auto-generated templates require no update (agent workflow unchanged). MANUAL.md gained Environment Variables appendix covering 10 vars.

### This Commit's Contribution


---

## Commit 6a0788cb — 2026-05-15 20:57 UTC

### Branch Purpose
write CVE blog posts on doc-cve git branch

### Previous Progress Summary


### This Commit's Contribution
Restructured src/pr.rs::render_body: GitHub-native > [!CAUTION] alert + table for credential leaks (parses CREDENTIAL_LEAK trigger detail), > [!WARNING] alert + grouped-by-file table for duplicated code (parses DUPLICATED_CODE trigger detail), collapsible Mermaid graph TD of the reasoning DAG with OBSERVE/THINK/ACT/NOTE/MERGE color-coded classDefs (sanitizes labels, escapes Mermaid-unsafe chars, tail-truncates to 40 nodes with elision marker, suppresses dangling edges). Per-commit provenance now collapsible when >5 commits. Empty test_metrics no longer render as ❌0/0. Zero-token badge suppressed. Added 16 unit tests for parsers, section renderers, mermaid sanitization, aggregators, and badge formatting. Exposed new ctx::dag_for_branch public API. All 481 tests pass.

---

## Commit 6a0788de — 2026-05-15 20:58 UTC

### Branch Purpose
write CVE blog posts on doc-cve git branch

### Previous Progress Summary
Restructured src/pr.rs::render_body: GitHub-native > [!CAUTION] alert + table for credential leaks (parses CREDENTIAL_LEAK trigger detail), > [!WARNING] alert + grouped-by-file table for duplicated code (parses DUPLICATED_CODE trigger detail), collapsible Mermaid graph TD of the reasoning DAG with OBSERVE/THINK/ACT/NOTE/MERGE color-coded classDefs (sanitizes labels, escapes Mermaid-unsafe chars, tail-truncates to 40 nodes with elision marker, suppresses dangling edges). Per-commit provenance now collapsible when >5 commits. Empty test_metrics no longer render as ❌0/0. Zero-token badge suppressed. Added 16 unit tests for parsers, section renderers, mermaid sanitization, aggregators, and badge formatting. Exposed new ctx::dag_for_branch public API. All 481 tests pass.

### This Commit's Contribution


---

