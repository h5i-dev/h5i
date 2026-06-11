# Branch: improve-shell

**Purpose:** improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

_Commits will be appended below._

## Commit 6a2ad0a9 — 2026-06-11 15:13 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution
Diagnosed why h5i env shell couldn't run claude/codex (no HOME grants, socketpair(AF_UNIX) denied by supervised gate, setsid broke tty/job control, TERM not passed, wall-clock killed interactive sessions, net deny). Implemented: decide_socketpair (anonymous AF_UNIX pair always allowed), interactive sessions skip setsid + no wall kill, TERM/COLORTERM in default env_pass, /dev/null+/dev/zero write-granted sinks, built-in 'agent' profile (HOME grants, API egress, /dev/tty, supervised/container-only), CLI hints + docs. Verified: claude -p API round-trip inside supervised box, codex auth read, egress allowlist enforced, ~/.ssh denied, 989 tests + clippy clean. Left: PTY-proxy for airtight tty isolation.

---

