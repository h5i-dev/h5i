# OTA Log — Branch: improve-shell

[14:49:30] OBSERVE: read src/env.rs
[14:49:40] OBSERVE: read src/sandbox.rs
[14:49:54] OBSERVE: read src/sandbox.rs
[14:50:04] OBSERVE: read src/sandbox.rs
[14:50:09] OBSERVE: read src/supervisor.rs
[14:50:31] OBSERVE: read src/sandbox.rs
[14:50:37] OBSERVE: read src/main.rs
[14:52:53] OBSERVE: read src/sandbox.rs
[14:52:54] OBSERVE: read src/seccomp_notify.rs
[14:54:36] OBSERVE: read src/sandbox.rs
[14:54:50] OBSERVE: read src/sandbox.rs
[14:56:56] ACT: edited src/supervisor.rs
[14:57:09] ACT: edited src/seccomp_notify.rs
[14:57:32] ACT: edited src/supervisor.rs
[14:57:48] ACT: edited src/sandbox.rs
[14:58:00] ACT: edited src/sandbox.rs
[14:58:12] ACT: edited src/sandbox.rs
[14:58:13] ACT: edited src/sandbox.rs
[14:58:31] ACT: edited src/sandbox.rs
[14:58:38] ACT: edited src/sandbox.rs
[14:58:44] OBSERVE: read src/sandbox.rs
[14:58:45] OBSERVE: read src/sandbox.rs
[14:58:56] OBSERVE: read src/sandbox.rs
[14:58:56] OBSERVE: read src/sandbox.rs
[14:59:07] ACT: edited src/sandbox.rs
[14:59:08] OBSERVE: read src/sandbox.rs
[14:59:16] ACT: edited src/sandbox.rs
[14:59:24] ACT: edited src/supervisor.rs
[14:59:29] ACT: edited src/supervisor.rs
[14:59:58] ACT: edited src/sandbox.rs
[15:00:32] ACT: edited src/sandbox.rs
[15:01:30] ACT: edited src/sandbox.rs
[15:01:48] OBSERVE: read src/main.rs
[15:01:56] ACT: edited src/main.rs
[15:02:04] ACT: edited src/main.rs
[15:02:39] ACT: edited docs/environments-design.md
[15:02:46] ACT: edited CLAUDE.md
[15:02:56] ACT: edited CLAUDE.md
[15:08:10] ACT: edited src/sandbox.rs
[15:08:13] ACT: edited src/sandbox.rs
[15:08:27] OBSERVE: read src/sandbox.rs
[15:08:39] ACT: edited src/sandbox.rs
[15:13:09] ACT: wrote supervised-tier-green-on-this-host.md
[15:13:33] OBSERVE: read MEMORY.md
[15:13:42] ACT: edited MEMORY.md


---
_[Checkpoint: 6a2ad0a9 — env shell agent-in-box: agent profile + interactive fixes, verified end-to-end]_
---



---
_[Checkpoint: 6a2ad0da — edited src/sandbox.rs; wrote supervised-tier-green-on-this-host.md; edited MEMORY.md]_
---

[15:20:36] OBSERVE: read src/env.rs
[15:20:36] OBSERVE: read src/main.rs
[15:22:10] ACT: edited src/main.rs
[15:23:54] ACT: edited src/sandbox.rs
[15:37:58] OBSERVE: read src/env.rs
[15:37:59] OBSERVE: read src/mcp.rs
[15:38:45] ACT: edited src/env.rs
[15:39:00] ACT: edited src/env.rs
[15:39:10] ACT: edited src/mcp.rs
[15:39:12] ACT: edited src/mcp.rs
[15:39:29] ACT: edited src/main.rs
[15:39:41] ACT: edited src/main.rs
[15:39:50] OBSERVE: read src/main.rs
[15:40:12] ACT: edited src/main.rs
[15:40:13] OBSERVE: read src/main.rs
[15:40:23] ACT: edited src/main.rs
[15:40:34] ACT: edited CLAUDE.md
[15:40:38] ACT: edited docs/environments-design.md
[15:41:13] ACT: edited src/env.rs


---
_[Checkpoint: 6a2ad7f2 — agent profile is now the auto-picked default for env create]_
---

[15:45:00] OBSERVE: read wsl2-cargo-test-oom.md
[15:45:10] ACT: edited wsl2-cargo-test-oom.md
[15:45:24] THINK: claude.json` via a temp-file-rename in `$HOME` root, that write would still be denied (only the file itself is granted) — tell me if you see config-save warnings and I'll widen that grant.
