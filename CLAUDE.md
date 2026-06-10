# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
cargo build --verbose       # Build the project
cargo build --release       # Release build
cargo test --verbose        # Run all tests
cargo test <test_name>      # Run a single test
cargo run -- <subcommand>   # Run the h5i CLI
```

CI runs `cargo build --verbose` then `cargo test --verbose` with Git user config pre-set (needed because tests perform Git operations).

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
- **`env.rs`** — h5i environments (`h5i env`, `docs/environments-design.md`): the triple fusion of a code branch (`refs/heads/h5i/env/<agent>/<slug>`), a reasoning branch (`refs/h5i/context/env/<agent>/<slug>`), and a policy manifest. Workspace backend is a native git worktree under `.git/.h5i/env/<agent>/<slug>/work` (locked for the env's life). Lifecycle: `create → run → propose → apply | abort → gc`; `inspect` renders one capture; `compare` ranks N envs (the "arena"). Event log in `refs/h5i/env` (CAS append + union-merge, same pattern as msg/objects); exec events carry secret-redacted command + wall/cpu/rss accounting. Every `env run` is a tagged, secret-redacted `objects` capture (`env_id`, `policy_digest`, `redactions`). Concurrent runs of one env are serialized by a `flock` on `run.lock`. Mediated commit enforces a canonicalized `$WORK` path allowlist (rejects nested `.git`, symlink-dir escapes, `..`).
- **`sandbox.rs`** — Policy model + process-tier confinement. Profiles from checked-in `.h5i/env.toml` (isolation claim, `fs.read/write/deny` lint, `net.mode deny|host`, resources, `env.pass` allowlist, `tools` allowlist), all fail-closed (non-empty `net.egress`/`secrets` under `process` refuse; non-empty `tools` refuses any unlisted program). Capability probing (Landlock ABI, userns, seccomp) — refuses, never silently downgrades. Linux enforcement: Landlock allowlist (`HardRequirement`), a broad seccomp-bpf deny-list (mount/ptrace/keyctl/bpf/module/kexec/`*_handle_at`/namespace/admin syscalls), always-on `unshare(NEWUSER|NEWIPC|NEWUTS)` + `NEWNET` when net-deny, `no_new_privs`, rlimits, and a `setsid` + process-group SIGKILL wall-clock kill that reaps the whole descendant tree. Reaps via `wait4` to record `rusage` (peak RSS, CPU). A timed-out run exits 124.
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
h5i env run <name> -- <cmd>              # policy-enforced, capture-wrapped
h5i env probe                            # host isolation capabilities
h5i env list | status <name> | log <name> | diff <name> [--stat]
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
