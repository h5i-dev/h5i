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

**h5i** ("high-five") is a Git sidecar that extends version control with five semantic dimensions: temporal (Git history), structural (AST), intentional (AI provenance), empirical (test metrics), and associative (cross-agent messaging via `refs/h5i/msg`). It stores its data in `.git/.h5i/` with subdirectories `ast/`, `metadata/`, and `msg/` (per-agent identity, read cursors, and reply views).

### Module Overview

- **`repository.rs`** (67KB) — Core hub. `H5iRepository` wraps a `git2::Repository` and orchestrates all five dimensions. Key operations: `init`, `commit`, `log`, `blame`, `resolve`. Commit flow optionally captures AI metadata, AST snapshots, test metrics, and runs integrity audits.
- **`session.rs`** — `LocalSession` manages per-file Yrs (Y-CRDT) documents for collaborative editing. Writes append-only binary updates to `delta_store`. Enables concurrent agent edits with strong eventual consistency.
- **`delta_store.rs`** — Append-only binary log for CRDT updates. Files are keyed by `sha256(file_path)`. Format: `[length: u32][update bytes]`. Supports snapshots and archival on commit.
- **`metadata.rs`** — Data types: `H5iCommitRecord`, `AiMetadata` (model, agent ID, prompt, token count), `TestMetrics`, `IntegrityReport` (severity: Valid/Warning/Violation). Serialized as JSON in Git Notes.
- **`ast.rs`** — `SemanticAst` (S-expression based), `AstDiff` (additions/deletions/moves/unchanged), similarity scoring (0.0–1.0), SHA-256 structure hashing. Python files are parsed via `script/h5i-py-parser.py`.
- **`blame.rs`** — Two modes: `Line` (traditional) and `Ast` (semantic). Associates authorship with AI metadata and test results per commit.
- **`msg.rs`** — Cross-agent messaging (the i5h protocol, `docs/i5h-protocol.md`). Stores an append-only `messages.jsonl` + `agents.json` roster in `refs/h5i/msg`; sends via compare-and-swap, pulls union-merge by message id. `Message` carries i5h fields (version, kind, reply_to, thread_id, priority, focus, risk, links). Identity resolves `--from`/`--as` > `$H5I_AGENT` > stored. Read-state is per-agent local files (`cursors/<agent>.json`, `views/<agent>.json`). Includes `sanitize_display` (terminal-injection defense for untrusted pulled fields) and `merge_settings_json` (powers `h5i msg setup`).
- **`env.rs`** — h5i environments (`h5i env`, `docs/environments-design.md`): the triple fusion of a code branch (`refs/heads/h5i/env/<agent>/<slug>`), a reasoning branch (`refs/h5i/context/env/<agent>/<slug>`), and a policy manifest. Workspace backend is a native git worktree under `.git/.h5i/env/<agent>/<slug>/work` (locked for the env's life). Lifecycle: `create → run → propose → apply | abort → gc`; `inspect` renders one capture; `compare` ranks N envs (the "arena"); `rebase` re-pins the base onto an advanced parent (3-way, refuses conflicts); `status` is a human view (policy + evidence + base drift, `--json` for the manifest). **Shareable:** `refs/h5i/env` holds `events.jsonl` + `manifests.jsonl` + `policies.jsonl` (one CAS commit per change; union-merge — events append-only, manifests newest-`updated_at` wins, policies immutable), so `h5i push`/`pull` carry the whole env (with its code branch under `refs/heads/h5i/env/*`) to another clone. On pull, `materialize_from_ref` writes manifests/policies to disk; a pulled env has no `work/`, so `diff` falls back to `base..branch-tip` (the proposed state) and `apply` works from the branch — the cross-agent review loop (claude proposes → codex reviews+applies on another clone). exec events carry secret-redacted command + wall/cpu/rss accounting. Every `env run` is a tagged, secret-redacted `objects` capture (`env_id`, `policy_digest`, `redactions`). Concurrent runs of one env are serialized by a `flock` on `run.lock`. Mediated commit enforces a canonicalized `$WORK` path allowlist (rejects nested `.git`, symlink-dir escapes, `..`).
- **`mcp.rs`** — Native MCP tools mirroring the CLI (`tool_definitions()` schemas + `call_tool` name→handler dispatch). Includes the `h5i_env_*` family (`create/run/list/status/diff/inspect/compare/propose/apply/rebase/abort`) so agents drive the sandbox directly instead of shell-quoting `h5i env …`; handlers reuse `env.rs` and return JSON (status/list/compare), patch text (diff), or result+status. (The large `tool_definitions()` `json!` literal is why `lib.rs` raises `recursion_limit`.)
- **`container.rs`** — The `isolation=container` backend (design phase 4): runs an env's command in a **rootless Podman** container only — `--rm`, `--pull=never`, `--cap-drop=ALL`, `--security-opt=no-new-privileges`, read-only rootfs + private `/tmp` tmpfs, `--mount type=bind,source=$WORK,target=/work,rw` with `--userns=keep-id`, mem/pid limits, env allowlist, no docker.sock, `--name` for wall-clock cleanup (`podman rm -f`). Uniquely unlocks the `net.egress` **domain allowlist** (which the static process tier can't): non-empty `net.egress` spawns a host-side DNS-pinned **HTTP/HTTPS CONNECT allowlist proxy** (`AllowList`: exact / `.wildcard` / `:port`; fail-closed `403`), the container reaching it via slirp4netns at `10.0.2.2` with `HTTP(S)_PROXY` set. Honest L7 scoping (blocks proxy-respecting tooling; airtight L3/L4 is the hardened/microvm tier). `build_run_argv` is pure + unit-tested; `probe()` detects rootless Podman.
- **`sandbox.rs`** — Policy model + process-tier confinement. Profiles from checked-in `.h5i/env.toml` (isolation claim, `fs.read/write/deny` lint, `net.mode deny|host`, resources `mem/procs/wall/fsize/cpu`, `env.pass` allowlist, `tools` allowlist), all fail-closed. Two built-ins need no file: `default` (deny-home build/test confinement) and `agent` (agent-in-box, supervised/container-only). The `agent` profile is **runtime-scoped** (`AgentRuntime`): it grants only the *creating* runtime's HOME state + API egress — `agent`/`agent-claude` → `~/.claude*` + Anthropic hosts, `agent-codex` → `~/.codex` + OpenAI hosts — so a Claude box can't read Codex's credentials (or vice versa) and reach the other's API. The `~/.local` read is narrowed to `~/.local/bin`/`~/.local/lib` + the runtime's own `~/.local/share/<runtime>` (no blanket `~/.local/share`, which held Jupyter/app secrets). Interactive sessions (`env shell`) skip `setsid` (keep the controlling tty → job control/TUIs) and have no wall-clock kill (non-empty `net.egress`/`secrets` under `process` refuse; non-empty `tools` refuses any unlisted program). Capability probing (Landlock ABI, userns, seccomp) — refuses, never silently downgrades — plus a **functional** `verify_exec` self-test (bits present ≠ confinement can exec; `env create` fails closed with a clear message when it can't, e.g. AppArmor-restricted userns on CI). Linux enforcement: Landlock allowlist (`HardRequirement`), a broad seccomp-bpf deny-list (mount/ptrace/keyctl/bpf/module/kexec/`*_handle_at`/namespace/admin/**io_uring** syscalls), always-on `unshare(NEWUSER|NEWIPC|NEWUTS)` + `NEWNET` when net-deny, `no_new_privs`, rlimits (AS/NPROC/CORE, opt-in FSIZE/CPU), and a `setsid` + process-group SIGKILL wall-clock kill that reaps the whole descendant tree. Reaps via `wait4` to record `rusage` (peak RSS, CPU). A timed-out run exits 124.
- **`watcher.rs`** — Uses `notify` crate. Detects file changes and syncs to CRDT session.
- **`error.rs`** — Error categories mirror the five dimensions (Git/temporal, AST/structural, metadata/intentional, quality/empirical, CRDT/associative).
- **`main.rs`** — CLI via `clap`. Subcommands: `init`, `session`, `commit`, `log`, `blame`, `resolve`.

### Key CLI Subcommands

```
h5i init
h5i session --file <path>
h5i commit --message <msg> [--prompt <text>] [--model <name>] [--agent <id>] [--tests] [--ast] [--audit] [--force]
h5i log [--limit N]
h5i blame <file> [--mode line|ast]
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
h5i env rebase <name>                   # re-pin base onto the advanced parent
h5i env inspect <name> --capture <id>   # render one evidence capture
h5i env compare <names...> [--json]     # the arena: rank envs side by side
h5i env propose <name>                   # mediated commit + review brief
h5i env apply <name> [--patch]           # reviewer-selected; never automatic
h5i env abort <name> | gc
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
