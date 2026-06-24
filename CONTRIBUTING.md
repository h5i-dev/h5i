# Contributing to h5i

Thanks for helping improve h5i. This project is a Rust CLI and local workflow
tool for AI-agent collaboration, Git sidecar state, command capture, sandboxed
environments, peer review, and auditable provenance.

This guide explains how to set up a development checkout, make changes that fit
the project, test them, and submit them for review.

## Project Shape

The main crate is `h5i-core`, with the `h5i` binary at `src/main.rs`.

Important areas:

- `src/main.rs`: CLI definitions, command routing, and user-facing command
  behavior.
- `src/storage.rs`, `src/objects.rs`, `src/ctx.rs`, `src/memory.rs`: h5i refs,
  sidecar storage, context, memory, and captured objects.
- `src/env.rs`, `src/sandbox.rs`, `src/sandbox_policy.rs`,
  `src/seccomp_notify.rs`, `src/container.rs`, `src/cgroup.rs`: environments
  and isolation policy.
- `src/msg.rs`, `src/team.rs`, `src/review.rs`: cross-agent messaging, teams,
  and review workflows.
- `src/hooks.rs`, `src/claude.rs`, `src/codex.rs`, `src/session_log.rs`: agent
  hook integration and transcript/provenance capture.
- `src/secrets.rs`, `src/filter_rules.rs`, `src/token_filter.rs`: secret and
  output filtering.
- `web/`: dashboard/workbench sources.
- `docs/`, `MANUAL.md`, `README.md`: user-facing documentation.
- `tests/`: integration and end-to-end coverage.

When in doubt, prefer the existing module boundary over adding a new abstraction.

## Development Requirements

Install:

- A stable Rust toolchain.
- `cargo` and `clippy`.
- Git.
- Node.js 20 and npm if you touch the web workbench or release asset build path.

Some tests perform real Git operations. Configure a local Git identity before
running the full suite:

```bash
git config --global user.name "Your Name"
git config --global user.email "you@example.com"
```

If you do not want to change global Git config, set equivalent local config in
the test repository environment you use.

## First Build

From the repository root:

```bash
cargo build --all-targets
cargo test
```

The default feature set includes the web dashboard. For a lean CLI-only build:

```bash
cargo build --no-default-features
```

CI checks all targets with warnings denied:

```bash
cargo clippy --all-targets --all-features -- -D warnings
cargo build --all-targets
cargo test
```

Rustfmt is not currently enforced by CI because the repository is not yet
fmt-clean. Do not submit broad formatting-only churn unless the change is
explicitly about formatting cleanup.

## Using h5i While Developing h5i

This repository dogfoods h5i. For non-trivial work, set a clear context goal at
the start:

```bash
h5i recall context goal
h5i recall context init --goal "short description of your task"
```

If your local h5i CLI reports that context is pinned to another branch, unpin it
using the command supported by your installed version before recording new work.

After meaningful read/edit bursts, sync the session:

```bash
h5i hook codex sync
```

After a logical milestone:

```bash
h5i hook codex finish --summary "what changed and what was verified"
```

For commits made by AI-assisted workflows, prefer provenance capture:

```bash
git add <exact paths>
h5i capture commit -m "concise commit message" --agent codex
```

Add `--tests` when tests were added or modified. Add `--audit` for
security-sensitive or high-risk changes.

## Coding Guidelines

Keep changes focused. h5i has many workflows that share storage and provenance
machinery, so small, well-scoped patches are easier to review and safer to
release.

General expectations:

- Preserve existing CLI behavior unless the change intentionally migrates it.
- Keep user-facing output stable where scripts might depend on it.
- Prefer structured parsing and typed data over ad hoc string manipulation.
- Use `Path` and `PathBuf` APIs for filesystem work.
- Use structured command arguments rather than shell strings when spawning
  processes.
- Treat branch names, ref names, agent names, messages, captured output, object
  metadata, and remote data as untrusted.
- Fail closed for policy, sandbox, sharing, and provenance ambiguity.
- Add comments only where they clarify a non-obvious invariant or security
  boundary.

For CLI changes, update the command definition, implementation, tests, and
documentation together. `MANUAL.md` is the command reference; `README.md` should
stay concise and oriented around common workflows.

## Security-Sensitive Changes

Read `SECURITY.md` before changing:

- Sandbox or isolation code.
- `h5i env`, `h5i team`, or hook execution paths.
- Secret scanning, filtering, or redaction.
- Git ref sharing, sidecar object storage, or object push/pull.
- Web dashboard binding, routing, rendering, or mutation endpoints.
- Install scripts, release workflows, or dependency/TLS behavior.

Security-sensitive changes need tests for refusal and malformed-input paths, not
only successful operation. If a platform cannot enforce a requested guarantee,
the implementation should say so explicitly and refuse the unsafe operation.

## Tests

Run the narrowest relevant test while iterating, then the broader suite before
submitting.

Useful commands:

```bash
cargo test
cargo test --test cli_integration
cargo test --test env_integration
cargo test --test msg_integration
cargo test --test objects_e2e
cargo clippy --all-targets --all-features -- -D warnings
```

Use integration tests for user-visible CLI behavior, Git ref behavior, sidecar
object behavior, and workflows involving real repositories. Unit tests are fine
for pure parsing, policy resolution, filtering, scoring, and formatting helpers.

Tests should avoid real network dependencies and real credentials. Use temporary
directories, fake remotes, fake tokens, and deterministic fixtures.

## Documentation

Update documentation in the same change when behavior changes.

Documentation expectations:

- `README.md`: project overview, installation, quick workflows, and high-signal
  examples.
- `MANUAL.md`: complete command reference and flags.
- `docs/`: website content, guides, feature pages, and static assets.
- `AGENTS.md`: instructions for AI agents working in this repository.
- `SECURITY.md`: security model, reporting, and sensitive development areas.
- `CONTRIBUTING.md`: contributor workflow and review expectations.

Do not include real tokens, private logs, private prompts, or private repository
names in docs, screenshots, fixtures, or examples.

## Web Workbench

The web dashboard is optional at the Rust feature level but included in default
builds. If you touch `web/` or release packaging:

- Use Node.js 20.
- Keep generated build output out of source diffs unless the repository already
  tracks a specific artifact.
- Verify the Rust build path that embeds or serves web assets.
- Check responsive behavior for user-facing UI changes.

Avoid turning the dashboard into a remotely exposed service without an explicit
security design and review.

## Commit and Pull Request Guidance

Good commits are narrow and explain the behavior change. Keep unrelated cleanup
out of feature and bug-fix commits.

Before opening a pull request:

- Rebase or merge `main` as appropriate for your workflow.
- Run relevant tests and include the commands in the PR description.
- Update docs for user-visible changes.
- Note platform coverage, especially for Linux-only sandbox behavior or
  cross-target build changes.
- Call out security-sensitive areas and any residual risk.
- Include screenshots or short recordings for visible web UI changes.

Pull requests should explain:

- What changed.
- Why it changed.
- How it was tested.
- Any compatibility impact on CLI output, stored h5i refs, sidecar formats,
  hooks, or release artifacts.

## Review Standards

Review prioritizes correctness, safety, and maintainability over patch size.
Expect reviewers to ask about:

- Backward compatibility for existing h5i refs and sidecar data.
- Failure behavior when Git state is unusual.
- Behavior on unsupported platforms.
- Secret leakage through logs, prompts, captures, docs, or tests.
- Whether agent-provided data is sanitized before display or execution.
- Whether tests prove the behavior users rely on.

If a change intentionally leaves a limitation, document it in the code or docs
where a future maintainer will see it.

## Release Notes

Maintainers preparing releases should call out:

- New commands or flags.
- Storage or ref format changes.
- Migration steps.
- Security fixes.
- Sandbox behavior changes.
- Platform support changes.
- Known limitations.

Release artifacts are built by GitHub Actions for Linux musl targets, macOS
Apple Silicon, and Windows MSVC. Keep the release matrix and CI cross-check
matrix aligned when changing supported targets.
