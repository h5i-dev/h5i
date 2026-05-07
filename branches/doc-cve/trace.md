# OTA Log — Branch: doc-cve

[15:03:15] OBSERVE: Existing blog posts in docs/blog/ follow a consistent template (header, deck, OG meta, JSON-LD). Index lives at docs/blog/index.html and lists post cards + JSON-LD blogPost entries. Sitemap lives at docs/sitemap.xml.
[15:03:15] OBSERVE: CVE-2025-59536: <1.0.111, CWE-94, fix in 1.0.111 (Oct 3 2025). Trust dialog bypass — code in untrusted dir executed before dialog accepted. Reporter: avivdon (HackerOne).
[15:03:15] OBSERVE: CVE-2026-33068: <2.1.53, CWE-807, fix in 2.1.53 (Mar 18 2026). .claude/settings.json with permissions.defaultMode=bypassPermissions silently skipped trust dialog. Reporter: Cantina.
[15:03:22] THINK: Plan: write two technical posts. (1) cve-2025-59536-startup-trust-dialog.html — explains the trust-dialog code-injection bug, why agentic CLIs need trust gates, what changed in 1.0.111. (2) cve-2026-33068-bypass-permissions-settings.html — explains how repo-controlled settings.json fed into the trust gate (CWE-807), recommended posture (don't commit bypassPermissions, review .claude/ before first open), pin to 2.1.53+. Both posts: factual, link to NVD + GHSA, credit reporters, tie back to h5i provenance/audit where natural without overclaiming. Then update docs/blog/index.html (cards + JSON-LD), docs/sitemap.xml, and the JSON-LD in docs/index.html if it lists posts.
[15:05:42] ACT: wrote docs/blog/cve-2025-59536-startup-trust-dialog.html
[15:07:13] ACT: wrote docs/blog/cve-2026-33068-bypass-permissions-settings.html
[15:07:19] ACT: edited docs/blog/index.html
[15:07:27] ACT: edited docs/blog/index.html
[15:07:32] ACT: edited docs/sitemap.xml
[15:07:54] ACT: Wrote docs/blog/cve-2025-59536-startup-trust-dialog.html and docs/blog/cve-2026-33068-bypass-permissions-settings.html. Updated docs/blog/index.html (cards + JSON-LD) and docs/sitemap.xml. docs/index.html doesn't list individual posts, no change needed there.


---
_[Checkpoint: 69fb594f — Added two CVE write-up blog posts]_
---



---
_[Checkpoint: 69fb595b — edited docs/blog/index.html; edited docs/sitemap.xml; Wrote docs/blog/cve-2025-59536-startup-trust-dialog.html and docs/…]_
---

[19:31:31] OBSERVE: listed files under .
[19:31:31] OBSERVE: listed files under .
[19:31:31] OBSERVE: read README.md
[19:31:31] OBSERVE: read Cargo.toml
[19:31:31] OBSERVE: read docs/index.html
[19:31:31] OBSERVE: read README.md
[19:31:31] OBSERVE: read Cargo.toml
[19:31:31] OBSERVE: read docs/index.html
[19:31:31] OBSERVE: read README.md
[19:31:31] OBSERVE: read MANUAL.md
[19:31:31] OBSERVE: listed files under assets
