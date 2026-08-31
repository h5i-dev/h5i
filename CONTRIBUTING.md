# Contributing to h5i

Thanks for helping improve h5i. It is a Rust CLI that gives a coding agent a
disposable, confined development box: the agent, the workspace, the shell, the
toolchain, the dev server and the browser all run inside one security boundary,
and the work leaves as a reviewable patch plus a receipt of what ran.

That framing is the whole scope. h5i used to be a much broader provenance tool,
and `ROADMAP.md` is the authority on what was cut. Read it before proposing a
feature: an idea that records what an agent did, rather than containing what it
can do, is probably out of scope on purpose.

## Project shape

A Cargo workspace. The `h5i` binary is at the repository root (so
`cargo install --path .` does the obvious thing) and the libraries are under
`crates/`:

- `src/main.rs`, `src/cli/`: the clap command tree and its handlers. `box` is
  the noun the product uses; `env` and `dev` survive as hidden aliases.
- `crates/h5i-error`: the shared error type. Depends on nothing else here.
- `crates/h5i-sandbox`: policy and enforcement. `sandbox_policy.rs` parses
  `.h5i/env.toml` and resolves a profile; `sandbox.rs` applies it;
  `seccomp_notify.rs`, `supervisor.rs`, `container.rs`, `microvm.rs`,
  `seatbelt.rs`, `cgroup.rs` are the per-tier backends; `secrets.rs`,
  `secrets_broker.rs` and `auth_proxy.rs` are the credential paths.
- `crates/h5i-core`: everything built on a box. `env.rs` is the lifecycle,
  `receipt.rs` and `redact.rs` the evidence, `export.rs` the output gate,
  `browser*.rs` the mediated browser, `server.rs` and `ui.rs` the console.
- `crates/h5i-browser-light`: h5i's own browser engine. It exists because the
  engine being the HTTP client is what makes a browser receipt the network
  rather than an observation of it.
- `web/`: the React sources for the `h5i ui` console.
- `tests/`: integration coverage that drives real repositories and real boxes.
- `docs/`, `MANUAL.md`, `README.md`, `ROADMAP.md`: user-facing documentation.

Dependencies run one way: `h5i-error <- h5i-sandbox <- h5i-core <- the binary`.
When in doubt, prefer the existing module boundary over a new abstraction.

## Development requirements

Install a stable Rust toolchain with `clippy`, plus Git. Node.js 20 and npm are
needed only if you touch `web/` or the release asset build path.

Optional, and only to exercise the tiers that use them: rootless
[Podman](https://podman.io/) for `isolation=container`, and
[microsandbox](https://microsandbox.dev) (`msb`) for `isolation=microvm`.

Some tests do real Git operations, so libgit2 needs an author and a committer:

```bash
git config --global user.name "Your Name"
git config --global user.email "you@example.com"
```

Local config in your test repository works too if you would rather leave global
Git config alone.

## First build

From the repository root:

```bash
cargo build --workspace --all-targets
cargo test --workspace
```

The default feature set includes the `h5i ui` console, so
`crates/h5i-core/build.rs` builds the web bundle and needs Node. Two ways
around that:

```bash
cargo build --no-default-features            # no console, no Node
H5I_SKIP_WEB_BUILD=1 cargo build             # console code, stubbed bundle
```

CI runs these, all with `--locked` because `Cargo.lock` is committed and the
release build uses it:

```bash
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo build  --locked --workspace --all-targets
cargo test   --locked --workspace
H5I_SKIP_WEB_BUILD=1 cargo clippy --locked --workspace --all-targets --no-default-features -- -D warnings
```

Pass `--workspace` to clippy. Without it only the root `h5i` package is linted
and every crate under `crates/` is skipped, which is where most of the code
lives.

CI does not enforce rustfmt, because the repository is not fmt-clean. Do not
submit broad formatting-only churn unless the change is about formatting.

On a memory-constrained machine, the test binaries are what blow up, not the
library. Build them single-job with debug info off rather than letting cargo
pick a parallelism your RAM cannot hold.

## Using h5i while developing h5i

This repository dogfoods h5i. Do agent-assisted work in a box, not in your
checkout:

```bash
h5i box create <name> --profile agent-claude
h5i box shell <name>            # or: h5i box run <name> -- <cmd>
h5i box diff <name>             # what the agent changed
h5i box export <name>           # a patch plus the receipt
```

`h5i box log` and `h5i box inspect` show what ran and what pressed on a
boundary. h5i's own suite runs inside a box with caveats: the cargo caches are
mounted read-only, there is a memory cap, and the tests that create nested
boxes need to be skipped.

## Coding guidelines

Keep changes focused. Policy resolution, the tier backends and the evidence
path are shared by every workflow, so small patches are easier to review and
safer to release.

- Fail closed. For policy, isolation, credentials and the export gate,
  ambiguity is a refusal. A tier that cannot enforce what was asked must say so
  and stop, never downgrade quietly.
- Never claim a boundary you did not enforce. This is the product's central
  promise and a documentation rule too: if a guarantee holds only at some tiers
  or on one platform, the code and the docs both have to say which.
- Distinguish host-observed evidence from box-claimed evidence, and keep that
  distinction visible wherever either is displayed.
- Preserve existing CLI behavior unless the change intentionally migrates it,
  and keep user-facing output stable where scripts might depend on it.
- Prefer structured parsing and typed data over ad hoc string manipulation.
- Use `Path` and `PathBuf` APIs for filesystem work.
- Use structured command arguments rather than shell strings when spawning
  processes.
- Treat everything that comes out of a box as untrusted: command output,
  receipts, file paths, branch names, browser page content, and any manifest or
  policy read back from a worktree.
- Comment only where it clarifies a non-obvious invariant or a security
  boundary. Existing comments explain *why*, in the places where the reason is
  not recoverable from the code. Match that.

For CLI changes, update the command definition, the implementation, the tests
and the documentation together.

## Security-sensitive changes

Read `SECURITY.md` before changing:

- Sandbox enforcement, tier resolution, or downgrade behavior.
- `h5i box` execution paths: `run`, `shell`, services, and the supervisor.
- The egress proxy, the credential-injecting auth proxy, or the secrets broker.
- Browser control mediation and the browser's own egress path.
- Secret scanning, redaction, or receipt contents.
- The export and apply gate, which decides that box output may touch your
  repository.
- Console binding, routing, the session token, or anything that would let a
  route do more than GET.
- Install scripts, release workflows, or dependency and TLS behavior.

These changes need tests for the refusal and malformed-input paths, not only
for successful operation. A bypass that turns a denied policy into a permitted
operation is a security bug. If a platform cannot enforce a requested guarantee,
the implementation must say so and refuse.

## Tests

Run the narrowest relevant test while iterating, then the broader suite before
submitting.

```bash
cargo test --workspace --lib             # unit tests, fast
cargo test --test env_integration        # box lifecycle, on real repos
cargo test --test console_api            # spawns the binary, speaks HTTP to it
cargo test --test browser_mediation      # the control-socket mediator
cargo clippy --locked --workspace --all-targets -- -D warnings
```

`tests/env_integration.rs` asserts Linux kernel behavior directly: that
`unshare` is refused by the seccomp deny-list, that a host process's
`/proc/<pid>/environ` is unreachable from the box's PID namespace, that
Landlock is what denied a write. It is Linux-only by design. CI on macOS runs
`--lib` only, and unit tests in the `seatbelt` module cover that backend.

Kernel-tier results depend on the host. Landlock, user namespaces and seccomp
being *present* does not mean a confined exec *works*: a hardened container or
an AppArmor policy can still refuse it. Verify functionally (`h5i box probe`,
and the exec self-test behind it) rather than by reading capability bits.

Tests should avoid real network dependencies and real credentials. Use
temporary directories, fake remotes, fake tokens and deterministic fixtures.

## Documentation

Update documentation in the same change as the behavior.

- `README.md`: overview, install, and the shortest path to a working box.
- `MANUAL.md`: the complete command, policy, receipt and limits reference. Its
  Limits section is a security document in prose. If your change moves a
  boundary, it changes there too.
- `ROADMAP.md`: scope. What is in, what was cut, and why. Short on purpose; it
  is meant to be read in one sitting.
- `docs/design-*.md`: the design behind each part, one file per part
  (`design-browser-engine.md`, `design-policy.md`, `design-runner.md`,
  `design-detect.md`). Live code cites their section numbers, so a section that
  moves needs its citations moved with it.
- `docs/`: website content, guides, features and static assets.
  `docs/content-style-guide.md` governs voice and structure there.
- `SECURITY.md`: security model, reporting, and sensitive areas.
- `CONTRIBUTING.md`: this file.

The manuals are generated and CI diffs them. `docs/man/man1/h5i.1` comes from
the clap tree, rendered by `examples/gen_man.rs`, and `docs/manual/index.html`
comes from `MANUAL.md`. A CLI flag change or a `MANUAL.md` edit that lands
without regenerating both fails the `docs` job. Regenerate on Linux with the
pinned generator and commit the result:

```bash
./scripts/gen_man.sh
python3 -m pip install -r scripts/requirements.txt && python3 scripts/gen_manual.py
git add docs/man/man1/h5i.1 docs/manual/index.html
```

The man page lives under `docs/` and nowhere else, because `docs/` is published
verbatim: the site serves that exact file at `https://h5i.dev/man/man1/h5i.1`,
which is how a reader installs it now that there is no `h5i man` subcommand.
Read it locally with `MANPATH=$PWD/docs/man man h5i`. (`install.sh` is the one
file that keeps two copies, because it has to answer at exactly `/install.sh`.)

Keep real tokens, private logs, private prompts and private repository names
out of docs, screenshots, fixtures and examples.

## The console (`web/`)

`web/` is the box console served by `h5i ui`. It is optional at the Rust
feature level (`--no-default-features` drops it, and with it the build script's
need for Node) but included in default builds, where
`crates/h5i-core/build.rs` builds the bundle and embeds it. If you touch `web/`
or release packaging:

- Use Node.js 20.
- Keep generated build output out of source diffs unless the repository already
  tracks a specific artifact.
- Verify the Rust build path that embeds or serves web assets.
- Check responsive behavior for user-facing UI changes.

The console reports rather than scores. Its badges are arithmetic over
receipts, and the difference between host-observed and box-claimed evidence is
shown, not averaged away. Keep it that way: a number that looks like a verdict
but is not one is worse than no number.

Regenerating `package-lock.json` needs npm 10 or newer. Rollup ships its native
code as per-platform optional packages, and npm 9 records only the ones
matching the machine it ran on. A lockfile regenerated from scratch on an arm64
laptop installs fine there, then fails on an x64 CI runner with a
`Cannot find module @rollup/rollup-linux-x64-gnu` stack trace that names
nothing useful. To rebuild it:

```bash
cd web && rm -rf node_modules package-lock.json && npx npm@10 install
```

`scripts/check-lockfile-platforms.mjs` runs in CI and fails with that
instruction if the lockfile is missing a platform. Installing from a correct
lockfile is safe with any npm version, and the build script uses `npm ci`, so
an ordinary `cargo build` never rewrites the committed file.

Do not turn the console into a remotely exposed service without an explicit
security design and review. Today it binds loopback only, every route is a GET,
and access needs a per-session token that is never written to disk. See the
module docs in `crates/h5i-core/src/server.rs`.

## Commits and pull requests

Good commits are narrow and explain the behavior change. Keep unrelated cleanup
out of feature and bug-fix commits.

Before opening a pull request:

- Rebase or merge `main` as appropriate for your workflow.
- Run the relevant tests and include the commands in the PR description.
- Update docs for user-visible changes, and regenerate the manuals if the CLI
  or `MANUAL.md` changed.
- Note platform coverage, especially for Linux-only sandbox behavior, macOS
  Seatbelt behavior, or cross-target build changes.
- Call out security-sensitive areas and any residual risk.
- Include screenshots or short recordings for visible console changes.

Say what changed, why, how it was tested, and what it breaks: CLI output, the
box manifest and receipt formats, or release artifacts.

## Review standards

Review prioritizes correctness, safety and maintainability over patch size.
Expect reviewers to ask about:

- Failure behavior when the host cannot enforce what the policy asked for.
- Behavior on unsupported platforms and at the tiers your change did not test.
- Backward compatibility of box manifests, resolved policies and receipts.
- Secret leakage through receipts, logs, the console, docs, or tests.
- Whether box-provided data is sanitized before display or execution.
- Whether the claim in the docs matches the enforcement in the code.
- Whether tests prove the refusal path, not just the happy path.

If a change intentionally leaves a limitation, document it where a future
maintainer will see it, and in `MANUAL.md`'s Limits section if a user would.

## Release notes

Maintainers preparing releases should call out:

- New commands or flags.
- Manifest, policy or receipt format changes, and any migration steps.
- Security fixes.
- Sandbox behavior changes, including anything that moves a tier's guarantees.
- Platform support changes.
- Known limitations.

GitHub Actions builds release artifacts for `x86_64-unknown-linux-musl`,
`aarch64-unknown-linux-musl`, `aarch64-apple-darwin` and
`x86_64-pc-windows-msvc`. The `cross-check` job in `test.yaml` compile-checks
that same matrix on every PR, so cross-target breakage surfaces before a tag is
cut. Keep the two matrices aligned when changing supported targets.
