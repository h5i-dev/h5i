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

## Commit 6a2ad0da — 2026-06-11 15:14 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary
Diagnosed why h5i env shell couldn't run claude/codex (no HOME grants, socketpair(AF_UNIX) denied by supervised gate, setsid broke tty/job control, TERM not passed, wall-clock killed interactive sessions, net deny). Implemented: decide_socketpair (anonymous AF_UNIX pair always allowed), interactive sessions skip setsid + no wall kill, TERM/COLORTERM in default env_pass, /dev/null+/dev/zero write-granted sinks, built-in 'agent' profile (HOME grants, API egress, /dev/tty, supervised/container-only), CLI hints + docs. Verified: claude -p API round-trip inside supervised box, codex auth read, egress allowlist enforced, ~/.ssh denied, 989 tests + clippy clean. Left: PTY-proxy for airtight tty isolation.

### This Commit's Contribution


---

## Commit 6a2ad7f2 — 2026-06-11 15:44 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution
Per user: replaced the interactive sibling-env prompt with a real default — CreateOpts.profile is Option<String>; None auto-picks builtin 'agent' when enforceable (checks effective_auto/pinned claim + load_profile + resolve + verify_exec), else 'default' with a printed note. Pinned weak --isolation correctly falls back. agent profile also gained ro ~/.cargo/env + ~/.cargo/bin. Verified e2e: bare create→agent+supervised, claude runs in box, process-pin falls back. -j2 builds are safe on this box (user-confirmed).

---

## Commit 6a2ad814 — 2026-06-11 15:45 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary
Per user: replaced the interactive sibling-env prompt with a real default — CreateOpts.profile is Option<String>; None auto-picks builtin 'agent' when enforceable (checks effective_auto/pinned claim + load_profile + resolve + verify_exec), else 'default' with a printed note. Pinned weak --isolation correctly falls back. agent profile also gained ro ~/.cargo/env + ~/.cargo/bin. Verified e2e: bare create→agent+supervised, claude runs in box, process-pin falls back. -j2 builds are safe on this box (user-confirmed).

### This Commit's Contribution


---

## Commit 6a2ad9ea — 2026-06-11 15:53 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2ae979 — 2026-06-11 16:59 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2aecc9 — 2026-06-11 17:13 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2af0fd — 2026-06-11 17:31 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution
Split builtin agent profile into runtime-scoped variants (agent-claude/agent-codex) + bare agent auto-detecting from $H5I_AGENT. Narrowed ~/.local read to bin/lib + runtime's own share dir. Verified e2e: claude launches in box; cross-runtime creds + jupyter secret denied. 777 tests pass, clippy clean.

---

## Commit 6a2af10f — 2026-06-11 17:31 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary
Split builtin agent profile into runtime-scoped variants (agent-claude/agent-codex) + bare agent auto-detecting from $H5I_AGENT. Narrowed ~/.local read to bin/lib + runtime's own share dir. Verified e2e: claude launches in box; cross-runtime creds + jupyter secret denied. 777 tests pass, clippy clean.

### This Commit's Contribution


---

## Commit 6a2af158 — 2026-06-11 17:33 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2af1b0 — 2026-06-11 17:34 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2af245 — 2026-06-11 17:37 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2b145c — 2026-06-11 20:02 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2b180f — 2026-06-11 20:18 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2b190e — 2026-06-11 20:22 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2b1e15 — 2026-06-11 20:44 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2b1eb1 — 2026-06-11 20:46 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2b20ad — 2026-06-11 20:55 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2b2717 — 2026-06-11 21:22 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2b29f3 — 2026-06-11 21:34 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2b2a33 — 2026-06-11 21:35 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2b2cc2 — 2026-06-11 21:46 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2b2d4c — 2026-06-11 21:49 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2b3a2b — 2026-06-11 22:43 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2b3b20 — 2026-06-11 22:48 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2b3bcd — 2026-06-11 22:50 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2b3c6f — 2026-06-11 22:53 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2b3dba — 2026-06-11 22:59 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2b3ee9 — 2026-06-11 23:04 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

