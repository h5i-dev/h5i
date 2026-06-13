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

## Commit 6a2b450c — 2026-06-11 23:30 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution
Three observe-only surfaces: (1) optional 'h5i hook observe-bash' PostToolUse handler stores Bash command+output as redacted captures (>=2KB or stderr; h5i's own commands skipped), registered in this repo's .claude/settings.json; (2) supervised tier argv-logs every execve via seccomp-notify (interactive sessions) to <env>/spool/exec.jsonl; (3) container tier env shell shadows /bin/sh+/bin/bash with a POSIX tee shim (image self-mount keeps real shell at /.h5i/orig), spooling per-command records. env shell ingests spool into env-tagged captures (untrusted: caps+redaction). Left: supervised-tier output tee (needs mount-ns), exit codes absent in hook payload.

---

## Commit 6a2b47a9 — 2026-06-11 23:41 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary
Three observe-only surfaces: (1) optional 'h5i hook observe-bash' PostToolUse handler stores Bash command+output as redacted captures (>=2KB or stderr; h5i's own commands skipped), registered in this repo's .claude/settings.json; (2) supervised tier argv-logs every execve via seccomp-notify (interactive sessions) to <env>/spool/exec.jsonl; (3) container tier env shell shadows /bin/sh+/bin/bash with a POSIX tee shim (image self-mount keeps real shell at /.h5i/orig), spooling per-command records. env shell ingests spool into env-tagged captures (untrusted: caps+redaction). Left: supervised-tier output tee (needs mount-ns), exit codes absent in hook payload.

### This Commit's Contribution


---

## Commit 6a2b4803 — 2026-06-11 23:42 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2b4dcc — 2026-06-12 00:07 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2b53b5 — 2026-06-12 00:32 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution
Root-caused the '19-min test' hang: adding execve to the supervised seccomp-notify filter deadlocks the bootstrap exec against the egress pre_exec handshake (mcp::tests::env_lifecycle_over_mcp). Reverted supervisor.rs + seccomp_notify.rs to baseline. SHIPPED: (1) container tee-shim (image self-mount /.h5i/orig keeps real shell for any image; shadows /bin/sh+bash; spools cmd-* records; passes stdout/stderr/exit/stdin through) + ingest_shell_spool (untrusted spool: caps+redaction); (2) optional h5i hook observe-bash PostToolUse handler (stores Bash cmd+output redacted, >=2KB or stderr, skips h5i's own); (3) Cargo.toml debug=line-tables-only (fixes -j4 OOM). All green: 782 lib + 52 env_integration + container/seccomp/supervisor suites.

---

## Commit 6a2b53d9 — 2026-06-12 00:33 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary
Root-caused the '19-min test' hang: adding execve to the supervised seccomp-notify filter deadlocks the bootstrap exec against the egress pre_exec handshake (mcp::tests::env_lifecycle_over_mcp). Reverted supervisor.rs + seccomp_notify.rs to baseline. SHIPPED: (1) container tee-shim (image self-mount /.h5i/orig keeps real shell for any image; shadows /bin/sh+bash; spools cmd-* records; passes stdout/stderr/exit/stdin through) + ingest_shell_spool (untrusted spool: caps+redaction); (2) optional h5i hook observe-bash PostToolUse handler (stores Bash cmd+output redacted, >=2KB or stderr, skips h5i's own); (3) Cargo.toml debug=line-tables-only (fixes -j4 OOM). All green: 782 lib + 52 env_integration + container/seccomp/supervisor suites.

### This Commit's Contribution


---

## Commit 6a2b5642 — 2026-06-12 00:43 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2b571a — 2026-06-12 00:47 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2b5856 — 2026-06-12 00:52 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2b5a1e — 2026-06-12 01:00 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2b5c5a — 2026-06-12 01:09 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2b5e6b — 2026-06-12 01:18 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2bf066 — 2026-06-12 11:41 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2bf2b6 — 2026-06-12 11:51 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution
New src/hooks.rs: pure merge_hook_settings_json (7 unit tests) — idempotently merges SessionStart/PostToolUse(Edit|Write|Read)/Stop wiring into .claude/settings.json; observe-bash is opt-in via --observe-bash (requires --write), default never adds it but won't strip an existing entry. CLI: hook setup gained --write/--scope/--observe-bash; print mode + init quick-start + README point at --write. Left manual: UserPromptSubmit jq prompt-capture script and MCP registration. Commit 1e514a5c.

---

## Commit 6a2bf2d3 — 2026-06-12 11:51 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary
New src/hooks.rs: pure merge_hook_settings_json (7 unit tests) — idempotently merges SessionStart/PostToolUse(Edit|Write|Read)/Stop wiring into .claude/settings.json; observe-bash is opt-in via --observe-bash (requires --write), default never adds it but won't strip an existing entry. CLI: hook setup gained --write/--scope/--observe-bash; print mode + init quick-start + README point at --write. Left manual: UserPromptSubmit jq prompt-capture script and MCP registration. Commit 1e514a5c.

### This Commit's Contribution


---

## Commit 6a2bf2f2 — 2026-06-12 11:52 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2bf6b9 — 2026-06-12 12:08 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution
Deleted hook observe-bash (~100 LOC PostToolUse observer). New: h5i hook wrap-bash (PreToolUse, updatedInput) rewrites Bash commands into h5i capture run — automatic token reduction. Pure wrap_bash_command in hooks.rs (simple→argv for adapters, complex→bash -c single-quoted; skips h5i cmds, top-level cd for cwd tracking, outside-repo; fail-open). merge_hook_settings_json: wrap_bash opt-in param, always strips legacy observe-bash entries. Repo settings.json migrated (observe-bash removed, wrap-bash NOT added per opt-in default). Caveat documented: permission allowlists match rewritten command. 11 unit tests + live e2e. Commit c375e3c0.

---

## Commit 6a2bf6ff — 2026-06-12 12:09 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary
Deleted hook observe-bash (~100 LOC PostToolUse observer). New: h5i hook wrap-bash (PreToolUse, updatedInput) rewrites Bash commands into h5i capture run — automatic token reduction. Pure wrap_bash_command in hooks.rs (simple→argv for adapters, complex→bash -c single-quoted; skips h5i cmds, top-level cd for cwd tracking, outside-repo; fail-open). merge_hook_settings_json: wrap_bash opt-in param, always strips legacy observe-bash entries. Repo settings.json migrated (observe-bash removed, wrap-bash NOT added per opt-in default). Caveat documented: permission allowlists match rewritten command. 11 unit tests + live e2e. Commit c375e3c0.

### This Commit's Contribution


---

## Commit 6a2bfab2 — 2026-06-12 12:25 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c1381 — 2026-06-12 14:11 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c13d6 — 2026-06-12 14:12 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c1563 — 2026-06-12 14:19 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c158f — 2026-06-12 14:19 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c16dd — 2026-06-12 14:25 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c176f — 2026-06-12 14:27 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c178e — 2026-06-12 14:28 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c182e — 2026-06-12 14:31 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c1913 — 2026-06-12 14:34 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c1b63 — 2026-06-12 14:44 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c1fcd — 2026-06-12 15:03 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c20fb — 2026-06-12 15:08 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c22e4 — 2026-06-12 15:16 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c235b — 2026-06-12 15:18 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c3b31 — 2026-06-12 17:00 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution
Fix options sketched: granular grants (.git/objects rw, .git/worktrees/<wt> rw, env branch ref rw, HEAD/packed-refs ro — avoid .git/config token leak + hooks) vs declaring in-box git unsupported with honest error. ctx::is_initialized also swallows EACCES.

---

## Commit 6a2c3b47 — 2026-06-12 17:00 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary
Fix options sketched: granular grants (.git/objects rw, .git/worktrees/<wt> rw, env branch ref rw, HEAD/packed-refs ro — avoid .git/config token leak + hooks) vs declaring in-box git unsupported with honest error. ctx::is_initialized also swallows EACCES.

### This Commit's Contribution


---

## Commit 6a2c4336 — 2026-06-12 17:34 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution
box_git_grants in env.rs wired into run+shell; ctx print_status honesty fix; design doc + CLAUDE.md updated; inverted stale GIT-BLOCKED assertion in process_tier_confines_fs_and_network; 3 new integration tests (positive git flow, fail-closed jail, h5i context flow) + 2 unit tests. Full suite green (798 lib + 223 integration). Verified supervised env shell end-to-end manually. Remaining gap: container tier in-box git (gitdir pointer names host path).

---

## Commit 6a2c434b — 2026-06-12 17:35 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary
box_git_grants in env.rs wired into run+shell; ctx print_status honesty fix; design doc + CLAUDE.md updated; inverted stale GIT-BLOCKED assertion in process_tier_confines_fs_and_network; 3 new integration tests (positive git flow, fail-closed jail, h5i context flow) + 2 unit tests. Full suite green (798 lib + 223 integration). Verified supervised env shell end-to-end manually. Remaining gap: container tier in-box git (gitdir pointer names host path).

### This Commit's Contribution


---

## Commit 6a2c49a1 — 2026-06-12 18:02 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution
ResolvedPolicy.box_git serde-skipped field; build_run_argv emits ordered --mount flags; grant_box_git dispatches per claim. Unit tests (mount emission, per-backend grant application) + gated e2e (busybox mount surface) + live-verified real git commit in alpine+git container (commit lands on env branch, main move fails EROFS, hooks blocked). Full suite green incl. H5I_TEST_CONTAINER=1.

---

## Commit 6a2c49ae — 2026-06-12 18:02 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary
ResolvedPolicy.box_git serde-skipped field; build_run_argv emits ordered --mount flags; grant_box_git dispatches per claim. Unit tests (mount emission, per-backend grant application) + gated e2e (busybox mount surface) + live-verified real git commit in alpine+git container (commit lands on env branch, main move fails EROFS, hooks blocked). Full suite green incl. H5I_TEST_CONTAINER=1.

### This Commit's Contribution


---

## Commit 6a2c4a90 — 2026-06-12 18:06 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c4bbd — 2026-06-12 18:11 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c4ff7 — 2026-06-12 18:29 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c50ed — 2026-06-12 18:33 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c54e4 — 2026-06-12 18:50 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c5659 — 2026-06-12 18:56 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c58b0 — 2026-06-12 19:06 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution
Verified (claude-code-guide + live unshare experiment): ro self-bind-mount blocks overwrite/unlink/rename of an existing config file even under a rw Landlock parent grant, BUT agent can create settings.local.json with disableAllHooks:true to kill all non-managed hooks. Robust fix = inject our own /etc/claude-code/managed-settings.json (Claude managed scope survives non-managed disableAllHooks, agent can't write root-owned /etc) read-only into the box's private mount ns. Container: podman auto-creates mount target on overlay, host untouched. Chosen scope: container only; process/supervised stay on revert-seal + tee-shim floor. Prereq: h5i must be reachable in-box for 'h5i hook wrap-bash' to run. Codex managed-config story still unknown.

---

## Commit 6a2c5b64 — 2026-06-12 19:17 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary
Verified (claude-code-guide + live unshare experiment): ro self-bind-mount blocks overwrite/unlink/rename of an existing config file even under a rw Landlock parent grant, BUT agent can create settings.local.json with disableAllHooks:true to kill all non-managed hooks. Robust fix = inject our own /etc/claude-code/managed-settings.json (Claude managed scope survives non-managed disableAllHooks, agent can't write root-owned /etc) read-only into the box's private mount ns. Container: podman auto-creates mount target on overlay, host untouched. Chosen scope: container only; process/supervised stay on revert-seal + tee-shim floor. Prereq: h5i must be reachable in-box for 'h5i hook wrap-bash' to run. Codex managed-config story still unknown.

### This Commit's Contribution
Unkillable wrap-bash hook via ro bind-mount at /etc/claude-code/managed-settings.json in the container ns. Live-verified read-only + present. Codex-gated, interactive-only, complements tee-shim. Full suite green (806 lib + 59 container e2e). Open follow-ups: (1) Codex equivalent — needs Codex managed-config research before its hook can be made equally unkillable; (2) process/supervised managed-settings would need one-time sudo /etc/claude-code setup, currently still on revert-seal + tee-shim floor; (3) h5i must be in-box image for the hook command to run.

---

## Commit 6a2c5b71 — 2026-06-12 19:18 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary
Unkillable wrap-bash hook via ro bind-mount at /etc/claude-code/managed-settings.json in the container ns. Live-verified read-only + present. Codex-gated, interactive-only, complements tee-shim. Full suite green (806 lib + 59 container e2e). Open follow-ups: (1) Codex equivalent — needs Codex managed-config research before its hook can be made equally unkillable; (2) process/supervised managed-settings would need one-time sudo /etc/claude-code setup, currently still on revert-seal + tee-shim floor; (3) h5i must be in-box image for the hook command to run.

### This Commit's Contribution


---

## Commit 6a2c5cb9 — 2026-06-12 19:23 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c5dcc — 2026-06-12 19:28 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c6077 — 2026-06-12 19:39 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c62f0 — 2026-06-12 19:50 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c6442 — 2026-06-12 19:55 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c6854 — 2026-06-12 20:13 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution
ro-bind project .claude/.codex DIRS (blocks edit+create, closing the settings.local.json disableAllHooks bypass) + pin user ~/.claude/settings.json & ~/.codex/config.toml FILES, in pre_exec before Landlock/seccomp, forcing CLONE_NEWNS for supervised (pidns=false). Contained by userns, unremovable (mount/umount2 seccomp-denied), fail-closed, interactive-only. Live-verified process + supervised: edit/create blocked, reads+other writes ok, host untouched. Full suite green (807 lib + 60 env_integration). Residual: absent project config dir could be created (tee-shim floor covers). Codex still needs trust-gate handling for its hook to actually RUN even with config locked.

---

## Commit 6a2c6860 — 2026-06-12 20:13 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary
ro-bind project .claude/.codex DIRS (blocks edit+create, closing the settings.local.json disableAllHooks bypass) + pin user ~/.claude/settings.json & ~/.codex/config.toml FILES, in pre_exec before Landlock/seccomp, forcing CLONE_NEWNS for supervised (pidns=false). Contained by userns, unremovable (mount/umount2 seccomp-denied), fail-closed, interactive-only. Live-verified process + supervised: edit/create blocked, reads+other writes ok, host untouched. Full suite green (807 lib + 60 env_integration). Residual: absent project config dir could be created (tee-shim floor covers). Codex still needs trust-gate handling for its hook to actually RUN even with config locked.

### This Commit's Contribution


---

## Commit 6a2c69db — 2026-06-12 20:19 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c6b05 — 2026-06-12 20:24 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c6b85 — 2026-06-12 20:26 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution
Added an operator-facing subsection to docs/environments-design.md under the agent-in-box profile: in-box observation layers (hook+config-lockdown / managed-settings / tee-shim) and the required 'codex --dangerously-bypass-hook-trust' launch for Codex env shell. Left to the human by design (config lockdown means agent can't disable/re-point the hook). Observation/hook-hardening thread now complete across tiers: Claude=managed-settings(container)+config-lockdown(kernel tiers); Codex=tee-shim(container)+config-lockdown+operator bypass-trust(kernel tiers). Supervised tee-shim port remains optional future work; not needed given the documented Codex incantation.

---

## Commit 6a2c6b91 — 2026-06-12 20:26 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary
Added an operator-facing subsection to docs/environments-design.md under the agent-in-box profile: in-box observation layers (hook+config-lockdown / managed-settings / tee-shim) and the required 'codex --dangerously-bypass-hook-trust' launch for Codex env shell. Left to the human by design (config lockdown means agent can't disable/re-point the hook). Observation/hook-hardening thread now complete across tiers: Claude=managed-settings(container)+config-lockdown(kernel tiers); Codex=tee-shim(container)+config-lockdown+operator bypass-trust(kernel tiers). Supervised tee-shim port remains optional future work; not needed given the documented Codex incantation.

### This Commit's Contribution


---

## Commit 6a2c6c81 — 2026-06-12 20:30 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c6d2b — 2026-06-12 20:33 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c6dad — 2026-06-12 20:35 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c6e30 — 2026-06-12 20:38 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c6ef3 — 2026-06-12 20:41 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c6fbc — 2026-06-12 20:44 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c7045 — 2026-06-12 20:47 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c70fd — 2026-06-12 20:50 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c72f5 — 2026-06-12 20:58 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution
shim_script now passes through h5i-prefixed commands unrecorded (case h5i|h5i\ *), mirroring the wrap-bash hook's own skip. Eliminates the hook+shim double-capture/overhead while keeping the shim as the non-duplicating floor: records only what the hook didn't wrap. H5I_SHIM set before the skip so h5i's sub-shells stay unrecorded. Live-tested (h5i capture run / bare h5i pass through; grep h5i still observed). Full suite green incl. container e2e. Open/related: managed-settings injection assumes h5i-in-image — on an image without h5i the injected hook would break commands (h5i: not found); consider gating injection on h5i-presence. Tee-shim confirmed as the image-agnostic primary for container (no h5i-in-box needed).

---

## Commit 6a2c730b — 2026-06-12 20:58 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary
shim_script now passes through h5i-prefixed commands unrecorded (case h5i|h5i\ *), mirroring the wrap-bash hook's own skip. Eliminates the hook+shim double-capture/overhead while keeping the shim as the non-duplicating floor: records only what the hook didn't wrap. H5I_SHIM set before the skip so h5i's sub-shells stay unrecorded. Live-tested (h5i capture run / bare h5i pass through; grep h5i still observed). Full suite green incl. container e2e. Open/related: managed-settings injection assumes h5i-in-image — on an image without h5i the injected hook would break commands (h5i: not found); consider gating injection on h5i-presence. Tee-shim confirmed as the image-agnostic primary for container (no h5i-in-box needed).

### This Commit's Contribution


---

## Commit 6a2c7478 — 2026-06-12 21:04 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c756e — 2026-06-12 21:09 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c761f — 2026-06-12 21:11 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c76e5 — 2026-06-12 21:15 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c77be — 2026-06-12 21:18 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c7f72 — 2026-06-12 21:51 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c89e8 — 2026-06-12 22:36 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c960e — 2026-06-12 23:28 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c96d5 — 2026-06-12 23:31 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2c9c47 — 2026-06-12 23:54 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution
apply now stamps the applied commit (FF + merge paths) with an h5i note carrying EnvProvenance (env_id, agent, isolation, policy_digest, base, capped capture ids + total, evidence count by trust lane). Lanes preserved (host-env-run/inbox-capture/tee-shim) so host-verified vs box-claimed stays distinguishable post-merge. From identity-validated manifest only; idempotent (ST_PROPOSED guard + force note); best-effort. h5i log renders From env:/Evidence:. Note ref refs/h5i/notes is read via git2 (git notes CLI won't resolve it outside refs/notes/) — tests read via git show refs/h5i/notes:<oid>. Tests: 1 unit (cap/unknown-lane) + 3 e2e (FF, merge, log). Full suite green (809 lib + 63 env). Still open: in-box h5i commit graceful-degrade + spool-note (separate from this apply carry-forward).

---

## Commit 6a2c9c55 — 2026-06-12 23:55 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary
apply now stamps the applied commit (FF + merge paths) with an h5i note carrying EnvProvenance (env_id, agent, isolation, policy_digest, base, capped capture ids + total, evidence count by trust lane). Lanes preserved (host-env-run/inbox-capture/tee-shim) so host-verified vs box-claimed stays distinguishable post-merge. From identity-validated manifest only; idempotent (ST_PROPOSED guard + force note); best-effort. h5i log renders From env:/Evidence:. Note ref refs/h5i/notes is read via git2 (git notes CLI won't resolve it outside refs/notes/) — tests read via git show refs/h5i/notes:<oid>. Tests: 1 unit (cap/unknown-lane) + 3 e2e (FF, merge, log). Full suite green (809 lib + 63 env). Still open: in-box h5i commit graceful-degrade + spool-note (separate from this apply carry-forward).

### This Commit's Contribution


---

## Commit 6a2cafb7 — 2026-06-13 01:17 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution
repository.rs::commit gains note_spool param: when host env-capture vars present, stage the H5iCommitRecord to the spool (skip AST/sealed-store writes) instead of EACCES-ing the notes ref. Git commit still lands on env branch; exits 0 with 'staged for host ingest'. Host ingest_shell_spool drains note-*.json, applies via git2 note, SCOPED to base..env_tip (rejects inherited/arbitrary commits like main, logged). Live-verified process tier end-to-end. Tests: write_note_spool sanitize unit + 2 e2e (apply happy path, off-range rejection). Full suite green (810 lib + 65 env + cli/objects/metrics). Completes the in-box provenance path: capture-run spool (28d509b8) + apply carry-forward (261a445e) + commit spool-note (this). Note: refs/h5i/notes read via git2/git show <ref>:<oid>, not git notes CLI.

---

## Commit 6a2cafc6 — 2026-06-13 01:17 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary
repository.rs::commit gains note_spool param: when host env-capture vars present, stage the H5iCommitRecord to the spool (skip AST/sealed-store writes) instead of EACCES-ing the notes ref. Git commit still lands on env branch; exits 0 with 'staged for host ingest'. Host ingest_shell_spool drains note-*.json, applies via git2 note, SCOPED to base..env_tip (rejects inherited/arbitrary commits like main, logged). Live-verified process tier end-to-end. Tests: write_note_spool sanitize unit + 2 e2e (apply happy path, off-range rejection). Full suite green (810 lib + 65 env + cli/objects/metrics). Completes the in-box provenance path: capture-run spool (28d509b8) + apply carry-forward (261a445e) + commit spool-note (this). Note: refs/h5i/notes read via git2/git show <ref>:<oid>, not git notes CLI.

### This Commit's Contribution


---

## Commit 6a2cb2ff — 2026-06-13 01:31 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

## Commit 6a2cb64a — 2026-06-13 01:45 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution
3 new e2e: (1) inbox_commit_on_supervised — in-box commit→spool→host-apply on supervised (the original report's tier), passes; (2) container_env_capture_spool_is_mounted_and_ingested — proves /.h5i/spool rw mount + host ingest via busybox sh writing a synthetic inbox-capture (sidesteps glibc/h5i-in-image), passes with H5I_TEST_CONTAINER=1; (3) apply_provenance_preserves_inbox_and_host_lanes — integrity: inbox-capture + host-env-run lanes survive apply as distinct lanes in the provenance note (no laundering). All gated, skip cleanly. Full suite green: 810 lib + 68 env_integration (supervised+container opted in). Remaining open (lower priority, flagged earlier): root-owned .h5i/objects is a host-state issue not addressed by code (needs chown / ownership-aware creation); the exact original .h5i/objects writer was a capture path (not the commit itself), now covered by the capture-spool + these tier tests.

---

## Commit 6a2cb656 — 2026-06-13 01:45 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary
3 new e2e: (1) inbox_commit_on_supervised — in-box commit→spool→host-apply on supervised (the original report's tier), passes; (2) container_env_capture_spool_is_mounted_and_ingested — proves /.h5i/spool rw mount + host ingest via busybox sh writing a synthetic inbox-capture (sidesteps glibc/h5i-in-image), passes with H5I_TEST_CONTAINER=1; (3) apply_provenance_preserves_inbox_and_host_lanes — integrity: inbox-capture + host-env-run lanes survive apply as distinct lanes in the provenance note (no laundering). All gated, skip cleanly. Full suite green: 810 lib + 68 env_integration (supervised+container opted in). Remaining open (lower priority, flagged earlier): root-owned .h5i/objects is a host-state issue not addressed by code (needs chown / ownership-aware creation); the exact original .h5i/objects writer was a capture path (not the commit itself), now covered by the capture-spool + these tier tests.

### This Commit's Contribution


---

## Commit 6a2cbb16 — 2026-06-13 02:06 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution
store_io_error() converts a PermissionDenied on .h5i/objects into a clear diagnostic: names path, reports owner-uid mismatch, explains likely cause (earlier sudo/root run), gives exact chown repair, notes the env-sandbox-sealed case. Wired into LocalStore.put + ensure_layout. Non-permission errors unchanged. Live-verified: 'h5i capture run' on a chmod-000 store now prints the repair command instead of raw EACCES. Tests: unit (permission vs other message shape) + functional (chmod 000 → actionable put error, root-skipped). Full suite green (812 lib + objects_e2e 25). This addresses the original report's actual blocker (root-owned .git/.h5i/objects) at the diagnostic level — the spool redirect handles the in-box write path; this handles the host-side ownership case with a clear, self-repairing message.

---

## Commit 6a2cbb24 — 2026-06-13 02:06 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary
store_io_error() converts a PermissionDenied on .h5i/objects into a clear diagnostic: names path, reports owner-uid mismatch, explains likely cause (earlier sudo/root run), gives exact chown repair, notes the env-sandbox-sealed case. Wired into LocalStore.put + ensure_layout. Non-permission errors unchanged. Live-verified: 'h5i capture run' on a chmod-000 store now prints the repair command instead of raw EACCES. Tests: unit (permission vs other message shape) + functional (chmod 000 → actionable put error, root-skipped). Full suite green (812 lib + objects_e2e 25). This addresses the original report's actual blocker (root-owned .git/.h5i/objects) at the diagnostic level — the spool redirect handles the in-box write path; this handles the host-side ownership case with a clear, self-repairing message.

### This Commit's Contribution


---

## Commit 6a2cbdb4 — 2026-06-13 02:17 UTC

### Branch Purpose
improve default UX of h5i env shell so AI agents (claude/codex) can actually run inside the sandbox

### Previous Progress Summary


### This Commit's Contribution


---

