# Contributing to h5i

h5i gives an agent a browser it can drive and a human can audit. The engine is
the HTTP client, so every request is policy-checked and written to the session
log before the bytes move, and a fetch that cannot be recorded is refused. A box
is a second boundary, taken on request with `--in <box>` rather than built into
the browser.

`docs/ROADMAP.md` is the scope authority. Read it before proposing a feature.

## Layout

A Cargo workspace. The `h5i` binary is at the root so `cargo install --path .`
does the obvious thing, and the libraries are under `crates/`:

| crate | what it holds |
| --- | --- |
| `h5i-error` | the shared error type. Depends on nothing else here |
| `h5i-wire` | receipt rows and stored HTTP messages, without the engine that produces them |
| `h5i-sandbox` | policy and enforcement. `sandbox_policy.rs` resolves `.h5i/env.toml`; `seccomp_notify.rs`, `supervisor.rs`, `container.rs`, `microvm.rs`, `seatbelt.rs` are the per-tier backends; `secrets*.rs` and `auth_proxy.rs` the credential paths |
| `h5i-bpf` | the `kernel-observed` lane on eBPF tracepoints, off by default |
| `h5i-core` | sessions, boxes and evidence: `browser_session.rs`, `env.rs`, `receipt.rs`, `redact.rs`, `export.rs`, `server.rs`, `ui.rs` |
| `h5i-browser` | the engine: fetch path, policy, cookies, CORS, snapshot, verbs |
| `h5i-runner`, `h5i-share` | a box on a second machine over SSH; a bridge to one box's web app |
| `h5i-websec`, `h5i-recon`, `h5i-test` | plugin binaries, installed with `h5i plugin install <name>` |

`src/cli/` is the clap tree, `web/` the console's React sources, `tests/` the
integration suites.

Dependencies run one way: `h5i-error` under `h5i-wire`, `h5i-sandbox` and
`h5i-bpf`, those under `h5i-core`, and `h5i-core` under the engine, the plugins
and the binary. Prefer an existing module boundary over a new abstraction.

## Build and test

Stable Rust with clippy, plus Git. Node 20 only if you touch `web/` or release
packaging. Rootless [Podman](https://podman.io/) and
[microsandbox](https://microsandbox.dev) (`msb`) are optional, and only to
exercise `isolation=container` and `isolation=microvm`.

Some tests do real Git work, and libgit2 needs an author and a committer:

```bash
git config --global user.name "Your Name"
git config --global user.email "you@example.com"
```

What CI runs, all `--locked` because `Cargo.lock` is committed and the release
build uses it:

```bash
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo build  --locked --workspace --all-targets
cargo test   --locked --workspace
H5I_SKIP_WEB_BUILD=1 cargo clippy --locked --workspace --all-targets --no-default-features -- -D warnings
```

`--workspace` is not optional. Without it clippy lints the root package only and
skips every crate under `crates/`, which is where most of the code lives.

Default features build the console, so `crates/h5i-core/build.rs` wants Node.
`H5I_SKIP_WEB_BUILD=1` stubs the bundle; `--no-default-features` drops the
console and the Node dependency with it.

Narrower suites while iterating:

```bash
cargo test --workspace --lib            # fast
cargo test --test browser_session_cli   # the session verbs, through the binary
cargo test --test env_integration       # box lifecycle on real repos, Linux only
cargo test --test console_api           # spawns the binary, speaks HTTP to it
./scripts/websec/smoke.sh target/release/h5i target/release/h5i-websec
```

Five more CI jobs sit behind that first one. `smoke` drives the plugin binaries
against a real server, `bpf` compiles the probe and proves the run seam behaves
on a host with no CAP_BPF, `macos` runs `--lib` and checks Seatbelt is present,
`docs` diffs the generated manuals, and `cross-check` compile-checks the four
release targets.

There is no fmt gate, because the repository is not fmt-clean. Do not send
formatting-only churn.

A debug build of this workspace carries a browser engine and a JavaScript engine
and runs to tens of gigabytes, so build release if your disk is small
(`scripts/no-debug-guard.sh` makes that stick). On a small-memory machine the
test binaries are what blow up, not the library: build them single-job with
debug info off.

This repository dogfoods h5i. Do agent-assisted work in a box rather than in
your checkout: `h5i box create <name> --profile agent-claude`, then `shell`,
`diff` and `export`. The suite runs in there with caveats, since the cargo
caches are mounted read-only, memory is capped, and the tests that create nested
boxes have to be skipped.

## Coding rules

- Fail closed. For policy, isolation, credentials and the export gate,
  ambiguity is a refusal. A tier that cannot enforce what was asked says so and
  stops. It never downgrades quietly.
- Never claim a boundary you did not enforce. When a guarantee holds only at
  some tiers or on one platform, the code and the docs both name which.
- Keep the evidence lanes apart. `engine-claimed`, `host-observed`,
  `box-claimed` and `kernel-observed` are never merged or averaged into a score.
- Treat what comes out of a box or off a page as attacker-composed: command
  output, receipts, paths, branch names, page text, and any policy read back
  from a worktree.
- Keep user-facing output stable where scripts depend on it. Migrate CLI
  behavior on purpose or not at all.
- Prefer typed data over string manipulation, `Path` over string paths, and
  argument vectors over shell strings.
- Comment where the reason is not recoverable from the code, in one to three
  lines.

For a CLI change, move the command definition, the implementation, the tests and
the docs together.

## Security-sensitive changes

Read `SECURITY.md` first if you touch the fetch path, policy or tier resolution,
downgrade behavior, egress and the credential proxies, secret scanning or
receipt contents, the export gate, box execution and the supervisor, console
binding and routing, or the install and release scripts.

These need tests for the refusal and malformed-input paths, not only the happy
one. A bypass that turns a denied policy into a permitted operation is a
security bug.

Kernel-tier results depend on the host. Landlock, user namespaces and seccomp
being present does not mean a confined exec works, because a hardened container
or an AppArmor policy can still refuse it. Check with `h5i box probe` rather
than by reading capability bits.

## Docs

Update the docs in the change that moves the behavior.

- `README.md`: the overview and the shortest path to a first session.
- `docs/MANUAL.md`: the command, policy and receipt reference. Its Limits
  section is a security document in prose.
- `docs/ROADMAP.md`: scope. What is built, what was cut, and why.
- `docs/design/*.md`: one file per part. Live code cites their section numbers,
  so a section that moves takes its citations with it.
- `docs/content-style-guide.md`: voice and structure for the website.

`docs/man/man1/h5i.1` and `docs/manual/index.html` are generated, and the `docs`
job diffs them. A CLI flag or a `docs/MANUAL.md` edit that lands without both
regenerated fails it. Regenerate on Linux and commit the result:

```bash
./scripts/gen_man.sh
python3 -m pip install -r scripts/requirements.txt && python3 scripts/gen_manual.py
python3 docs/build-content.py
```

Each of those re-stamps the pages it writes, so the order does not matter. For a
change to `docs/_static` alone, run `python3 scripts/stamp_assets.py` instead, or
browsers keep serving the cached stylesheet. `docs/` is published verbatim,
which is why the man page lives there and nowhere else (read it locally with
`MANPATH=$PWD/docs/man man h5i`) and why `install.sh` is copied to
`docs/install.sh`, with CI failing when the two differ.

Keep real tokens, private logs, private prompts and private repository names out
of docs, fixtures, screenshots and examples.

## The console (`web/`)

`web/` is the box console that `h5i ui` serves. Use Node 20, verify the Rust
path that embeds the bundle, and check narrow widths for visible changes.

The console counts over receipts. Its badges are arithmetic rather than a score,
and the gap between host-observed and box-claimed is shown, not averaged away. A
number that looks like a verdict but is not one is worse than no number.

It binds loopback, every route is a GET, and access needs a per-session token
that is never written to disk. See the module docs in
`crates/h5i-core/src/server.rs` before changing any of that.

Regenerating `package-lock.json` needs npm 10 or newer. Rollup ships its native
code as per-platform optional packages and npm 9 records only the one matching
the machine it ran on, so a lockfile rebuilt on an arm64 laptop fails on an x64
runner with a `Cannot find module @rollup/rollup-linux-x64-gnu` trace that names
nothing useful. `scripts/check-lockfile-platforms.mjs` catches that in CI. To
rebuild:

```bash
cd web && rm -rf node_modules package-lock.json && npx npm@10 install
```

## Pull requests

Keep commits narrow, with a one-line subject that says what changed, and keep
unrelated cleanup out of them.

Before opening a PR, run the relevant tests and name them in the description,
regenerate the manuals if the CLI or `docs/MANUAL.md` moved, say which platforms
and tiers you covered and which you did not, and call out anything security
sensitive along with the residual risk. Add screenshots for visible console
changes.

Reviewers will ask what happens when the host cannot enforce what the policy
asked for, whether manifests and receipts stay compatible, whether box-provided
data is sanitized before display or execution, whether the docs match the
enforcement, and whether a test proves the refusal path. A limitation you leave
in on purpose belongs in `docs/MANUAL.md`'s Limits section if a user would hit
it.

Release artifacts are built for `x86_64-unknown-linux-musl`,
`aarch64-unknown-linux-musl`, `aarch64-apple-darwin` and
`x86_64-pc-windows-msvc`. `cross-check` compile-checks that same matrix on every
PR, so keep the two aligned when the supported targets change.
