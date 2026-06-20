# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
cargo build --verbose       # Build the project
cargo build --release       # Release build
cargo test --verbose        # Run all tests
cargo test <test_name>      # Run a single test
cargo run -- <subcommand>   # Run the h5i CLI

# Real-container tests (isolation=container) are opt-in — they pull an image and
# make a live network call, so CI never runs them implicitly:
H5I_TEST_CONTAINER=1 cargo test --test env_integration container_
```

CI runs clippy (`-D warnings`), `cargo build --verbose` then `cargo test --verbose` with Git user config pre-set (needed because tests perform Git operations). The kernel-confinement (`process` tier) and container tests are capability-gated: they skip cleanly where the host can't run them — no podman or special CI setup is required.

## Architecture

**h5i** ("high-five") is a Git sidecar that extends version control with semantic dimensions: temporal (Git history), intentional (AI provenance), empirical (test metrics), and associative (cross-agent messaging via `refs/h5i/msg`). It stores its data in `.git/.h5i/` with subdirectories `metadata/` and `msg/` (per-agent identity, read cursors, and reply views).

### Module Overview

- **`repository.rs`** (67KB) — Core hub. `H5iRepository` wraps a `git2::Repository` and orchestrates the dimensions. Key operations: `init`, `commit`, `log`, `blame`, `resolve`. Commit flow optionally captures AI metadata, test metrics, and runs integrity audits.
- **`session.rs`** — `LocalSession` manages per-file Yrs (Y-CRDT) documents for collaborative editing. Writes append-only binary updates to `delta_store`. Enables concurrent agent edits with strong eventual consistency.
- **`delta_store.rs`** — Append-only binary log for CRDT updates. Files are keyed by `sha256(file_path)`. Format: `[length: u32][update bytes]`. Supports snapshots and archival on commit.
- **`metadata.rs`** — Data types: `H5iCommitRecord`, `AiMetadata` (model, agent ID, prompt, token count), `TestMetrics`, `IntegrityReport` (severity: Valid/Warning/Violation). Serialized as JSON in Git Notes.
- **`blame.rs`** — Line-based blame associating authorship with AI metadata and test results per commit.
- **`msg.rs`** — Cross-agent messaging (the i5h protocol, `docs/i5h-protocol.md`). Stores an append-only `messages.jsonl` + `agents.json` roster in `refs/h5i/msg`; sends via compare-and-swap, pulls union-merge by message id. `Message` carries i5h fields (version, kind, reply_to, thread_id, priority, focus, risk, links). Identity resolves `--from`/`--as` > `$H5I_AGENT` > stored. Read-state is per-agent local files (`cursors/<agent>.json`, `views/<agent>.json`). Includes `sanitize_display` (terminal-injection defense for untrusted pulled fields) and `merge_settings_json` (powers `h5i msg setup`).
- **`env.rs`** — h5i environments (`h5i env`, `docs/environments-design.md`): the triple fusion of a code branch (`refs/heads/h5i/env/<agent>/<slug>`), a reasoning branch (`refs/h5i/context/env/<agent>/<slug>`), and a policy manifest. Workspace backend is a native git worktree under `.git/.h5i/env/<agent>/<slug>/work` (locked for the env's life). Lifecycle: `create → run → propose → apply | abort → gc`; `rm` permanently removes an env (prunes the worktree, deletes the code + reasoning branches, erases the on-disk manifest, and strips its manifest/policy lines from `refs/h5i/env/meta` so a re-materialize can't resurrect it — only the append-only `removed` event survives; `--force` for a still-live env); `inspect` renders one capture; `compare` ranks N envs (the "arena"); `rebase` re-pins the base onto an advanced parent (3-way, refuses conflicts); `status` is a human view (policy + evidence + base drift, `--json` for the manifest). **Shareable:** everything env-related lives under one `refs/h5i/env/` namespace. `refs/h5i/env/meta` holds `events.jsonl` + `manifests.jsonl` + `policies.jsonl` (one CAS commit per change; union-merge — events append-only, manifests newest-`updated_at` wins, policies immutable), so `h5i share push`/`pull` carry the whole env to another clone. The env **code branch** travels at `refs/h5i/env/code/*`: locally the code is a real branch `refs/heads/h5i/env/<agent>/<slug>` (a worktree needs one), but on the wire it is a **transport remap** to the hidden `refs/h5i/env/code/*` ns (push `+refs/heads/h5i/env/*:refs/h5i/env/code/*`, fetch FF-only `refs/h5i/env/code/*:refs/heads/h5i/env/*`) so it never clutters a remote's branch UI (GitHub lists only `refs/heads/*`). The state ref is `…/meta` (not the bare leaf `refs/h5i/env`) so the code refs can nest beside it without a git file/dir collision; `h5i share push` also deletes any stray `refs/heads/h5i/env/*` left on the remote. On pull, `materialize_from_ref` writes manifests/policies to disk; a pulled env has no `work/`, so `diff` falls back to `base..branch-tip` (the proposed state) and `apply` works from the branch — the cross-agent review loop (claude proposes → codex reviews+applies on another clone). exec events carry secret-redacted command + wall/cpu/rss accounting. Every `env run` is a tagged, secret-redacted `objects` capture (`env_id`, `policy_digest`, `redactions`). Interactive **container** sessions leave **observation evidence** too: the tee shim spools per-command records, and `env shell` ingests them at session end into env-tagged, secret-redacted captures (`ingest_shell_spool`: box-written spool is untrusted — regular files only, 200-entry/4MiB caps, no silent truncation). (Supervised-tier exec observation via seccomp-notify is **deferred**: notifying `execve` makes the bootstrap exec block on the supervisor, which deadlocks with the egress `pre_exec` bring-up handshake.) Concurrent runs of one env are serialized by a `flock` on `run.lock`. Mediated commit enforces a canonicalized `$WORK` path allowlist (rejects nested `.git`, symlink-dir escapes, `..`); a gitlink is refused unless it round-trips a registered base submodule unchanged (same path + same OID as the env-branch tip) — agent-introduced/re-pointed gitlinks still fail closed. **In-box git:** `run`/`shell` apply the structural plumbing surface (`box_git_plumbing`) so the worktree is a *functional* checkout inside the box — rw on the env's own `worktrees/<wt>` admin dir, `objects`, its agent's `refs/heads/h5i/env/<agent>` (+ reflog dir) and `refs/h5i/context`; ro on `HEAD`/`config`/`packed-refs`/`refs`/`info`. At process/supervised these become Landlock grants (+ ro `~/.gitconfig`/`~/.config/git` — git dies on an unreadable global config); at container they become bind mounts at *identical host paths* (`policy.box_git`, never serialized/digested) so the worktree's gitdir/commondir pointer files resolve, with `$WORK` dual-mounted at its host path. `refs/h5i/env` meta, hooks, and the manifest/policy dir stay sealed (a box that could rewrite its manifest could widen its own policy). Grants derive only from the identity-validated manifest, never from box-writable state like the `$WORK/.git` pointer.
- **`mcp.rs`** — Native MCP tools mirroring the CLI (`tool_definitions()` schemas + `call_tool` name→handler dispatch). Includes the `h5i_env_*` family (`create/run/list/status/diff/inspect/compare/propose/apply/rebase/abort`) so agents drive the sandbox directly instead of shell-quoting `h5i env …`; handlers reuse `env.rs` and return JSON (status/list/compare), patch text (diff), or result+status. (The large `tool_definitions()` `json!` literal is why `lib.rs` raises `recursion_limit`.)
- **`container.rs`** — The `isolation=container` backend (design phase 4): runs an env's command in a **rootless Podman** container only — `--rm`, `--pull=never`, `--cap-drop=ALL`, `--security-opt=no-new-privileges`, read-only rootfs + private `/tmp` tmpfs, `--mount type=bind,source=$WORK,target=/work,rw` with `--userns=keep-id`, mem/pid limits, env allowlist, no docker.sock, `--name` for wall-clock cleanup (`podman rm -f`). Uniquely unlocks the `net.egress` **domain allowlist** (which the static process tier can't): non-empty `net.egress` spawns a host-side DNS-pinned **HTTP/HTTPS CONNECT allowlist proxy** (`AllowList`: exact / `.wildcard` / `:port`; fail-closed `403`), the container reaching it via slirp4netns at `10.0.2.2` with `HTTP(S)_PROXY` set. Honest L7 scoping (blocks proxy-respecting tooling; airtight L3/L4 is the hardened/microvm tier). `build_run_argv` is pure + unit-tested; `probe()` detects rootless Podman. Interactive `env shell` sessions add an **observation shim**: the container's own image is self-mounted ro at `/.h5i/orig` (so the real shell stays reachable for *any* image), a generated POSIX tee shim (`shim_script`, pure + live-tested) shadows `/bin/sh`+`/bin/bash`, and top-level command invocations spool `cmd-*.{cmd,out,err,exit}` records to `/.h5i/spool` while passing stdout/stderr/exit/stdin through untouched. The command flag is found by **scanning argv for a short-option cluster ending in `c`** (`-c`/`-lc`/`-ic`/…) — runtime-agnostic: Claude Code runs `bash -c`, Codex runs `bash -lc`. Best-effort: every shim guard fails open to the real shell. A command whose first word is `h5i` passes through **unrecorded** (the wrap-bash hook already rewrote the agent command to `h5i capture run -- …`, which self-captures) — so when both the hook and the shim are active the shim doesn't double-capture; it records only what the hook didn't wrap (the floor). The shim needs **no `h5i` in the box** (it writes plain spool files, ingested host-side), which is why it — not the hook — is the image-agnostic observation mechanism for arbitrary container images. Interactive Claude sessions also get an **unkillable observation hook**: `prepare_managed_settings` writes a minimal managed-settings.json carrying only `h5i hook wrap-bash` and bind-mounts it **read-only** at `/etc/claude-code/managed-settings.json` (Podman auto-creates the nested target on the overlay; the mount is ns-local so the host is untouched). Claude's managed scope can't be removed by higher-precedence merge and survives a non-managed `disableAllHooks`, and the in-box agent can't write the root-owned path — so it can't silence observation from its own config. Skipped for a known-Codex profile (the file is Claude-specific; Codex hardening is separate). Prereq: `h5i` must be reachable in-box for the hook to execute. This complements (not replaces) the tee-shim, which observes at the shell level regardless of agent cooperation.
- **`sandbox.rs`** — Policy model + process-tier confinement. Profiles from checked-in `.h5i/env.toml` (isolation claim, `fs.read/write/deny` lint, `net.mode deny|host`, resources `mem/procs/wall/fsize/cpu`, `env.pass` allowlist, `tools` allowlist), all fail-closed. Two built-ins need no file: `default` (deny-home build/test confinement) and `agent` (agent-in-box, supervised/container-only). The `agent` profile is **runtime-scoped** (`AgentRuntime`): it grants only the *creating* runtime's HOME state + API egress — `agent`/`agent-claude` → `~/.claude*` + Anthropic hosts, `agent-codex` → `~/.codex` + OpenAI hosts — so a Claude box can't read Codex's credentials (or vice versa) and reach the other's API. The `~/.local` read is narrowed to `~/.local/bin`/`~/.local/lib` + the runtime's own `~/.local/share/<runtime>` (no blanket `~/.local/share`, which held Jupyter/app secrets). **Per-env credential isolation** (`env::prepare_home_state` → `policy.home_binds`): rather than granting the box the *real* `~/.claude`/`~/.claude.json`/`~/.codex` rw (so concurrent agent boxes race + corrupt the shared session/token files), each env gets a per-env **copy** under `<env>/home/`, seeded once from the real HOME (copy-in) and persisted across that env's runs; the copy is granted rw and bind-mounted over the real absolute path in the box's mount ns (same `pre_exec` MS_BIND as config-lock/private-paths, before Landlock). The real HOME is only ever **read** to seed — never written — so an env can't clobber it (reconciliation = copy-in only). Kernel-tier only (container's ro rootfs never mounts host HOME); a state path absent on the host keeps today's direct grant (never created in real HOME just to bind over). Interactive sessions (`env shell`) skip `setsid` (keep the controlling tty → job control/TUIs) and have no wall-clock kill (non-empty `net.egress`/`secrets` under `process` refuse; non-empty `tools` refuses any unlisted program). Interactive sessions also get **config lockdown** (`config_lock_paths`): the project config dirs `$WORK/.claude`/`$WORK/.codex` are bind-mounted **read-only** (blocks editing *and* creating files — e.g. a `settings.local.json` with `disableAllHooks`), and the user settings files `~/.claude/settings.json`/`~/.codex/config.toml` are pinned read-only as single files (the rest of `~/.claude` stays writable for session state). The bind runs in `pre_exec` before Landlock/seccomp (which can't subtract a writable child from a granted parent), forcing `CLONE_NEWNS` (supervised is pidns=false → no mount ns otherwise); contained by the userns so the host is untouched, and unremovable in-box (`mount`/`umount2` are seccomp-denied). Fail-closed. This makes the in-box observation hook tamper-resistant on the kernel tiers, which have no managed-settings tier to reach; residual: an *absent* project config dir could still be created (the tee-shim is the floor for that). Capability probing (Landlock ABI, userns, seccomp) — refuses, never silently downgrades — plus a **functional** `verify_exec` self-test (bits present ≠ confinement can exec; `env create` fails closed with a clear message when it can't, e.g. AppArmor-restricted userns on CI). Linux enforcement: Landlock allowlist (`HardRequirement`), a broad seccomp-bpf deny-list (mount/ptrace/keyctl/bpf/module/kexec/`*_handle_at`/namespace/admin/**io_uring** syscalls), always-on `unshare(NEWUSER|NEWIPC|NEWUTS)` + `NEWNET` when net-deny, `no_new_privs`, rlimits (AS/NPROC/CORE, opt-in FSIZE/CPU), and a `setsid` + process-group SIGKILL wall-clock kill that reaps the whole descendant tree. Reaps via `wait4` to record `rusage` (peak RSS, CPU). A timed-out run exits 124.
- **`watcher.rs`** — Uses `notify` crate. Detects file changes and syncs to CRDT session.
- **`error.rs`** — Error categories mirror the dimensions (Git/temporal, metadata/intentional, quality/empirical, CRDT/associative).
- **`main.rs`** — CLI via `clap`. Subcommands: `init`, `session`, `commit`, `log`, `blame`, `resolve`.

### Key CLI Subcommands

```
h5i init
h5i session --file <path>
h5i capture commit --message <msg> [--intent <text>] [--model <name>] [--agent <id>] [--tests] [--audit] [--force]
                                   # --intent is a fallback (Codex/CI/manual); in Claude Code the
                                   # human prompt is auto-captured by the UserPromptSubmit hook.
                                   # (--prompt is a back-compat alias for --intent.)
h5i recall log [--limit N]
h5i recall blame <file>
h5i resolve <ours> <theirs> <file>

# Cross-agent messaging (i5h). Identity via $H5I_AGENT (per agent), no --as needed.
h5i msg setup [<name>] [--scope project|user] [--no-block]   # one-time Claude Code wiring
h5i msg                                  # inbox dashboard
h5i msg send <agent> <text>              # also: ask|review|risk|handoff <agent> <text>
h5i msg reply|ack|done|decline <n> [text]
h5i msg inbox | history | team | watch [--all]
h5i msg hook [--block]                   # Stop-hook turn delivery

# Isolated agent environments (worktree + sandbox + provenance)
h5i env create <name> [--from REV] [--profile P] [--isolation workspace|process|...]
                                         # --profile unset auto-picks the creating runtime's agent-in-box
                                         # profile (`agent-claude`/`agent-codex`: only that runtime's HOME
                                         # state + API egress) where the host can enforce it, else
                                         # `default` (fail-closed build/test)
h5i env run <name> -- <cmd>              # policy-enforced, capture-wrapped
h5i env shell <name> [-- <cmd>]          # interactive confined session (agent-in-box)
h5i env probe                            # host isolation capabilities (incl. rootless Podman)
h5i env list | status <name> [--json] | log <name> | diff <name> [--stat]
h5i env context <name> [--trace]        # show the env's reasoning/context branch
h5i env rebase <name>                   # re-pin base onto the advanced parent
h5i env inspect <name> --capture <id>   # render one evidence capture
h5i recall objects --env <name>         # list this env's evidence captures (also: recall search --env)
h5i env compare <names...> [--json]     # the arena: rank envs side by side
h5i env propose <name>                   # mediated commit + review brief
h5i env apply <name> [--patch]           # reviewer-selected; never automatic
h5i env abort <name> | gc
h5i env rm <name> [--force]              # permanently remove: worktree + branches + manifest
                                         # (strips refs/h5i/env; --force for a still-live env)
```

### Key Dependencies

- **git2** — Git operations
- **yrs** — Y-CRDT implementation for collaborative sessions
- **tokio** — Async runtime
- **tiktoken-rs** — Token counting for AI metadata
- **notify** — File system watching
- **clap** — CLI parsing
- **landlock / seccompiler / libc** (Linux) — `h5i env` process-tier sandbox (filesystem allowlist, syscall deny-list, namespaces)

@.claude/h5i.md
