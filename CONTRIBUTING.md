# Contributing to h5i

h5i is an Axum-based Rust web framework for building and verifying web apps
with Lean 4, featuring optional GPU acceleration and integrated development
infrastructure for red-teaming and CI/CD. Contributions to the framework, its
proofs, GPU adapters, CLI, and testing tools are welcome.

Check the [roadmap](docs/ROADMAP.md) before proposing a new feature. The root
Cargo workspace contains the CLI and libraries in `crates/`; application
examples live in the separate `examples/app` workspace. See the
[framework guide](crates/h5i-app/README.md), [GPU guide](docs/design/design-app-executors.md), and
[manual](docs/MANUAL.md) for details.

## Build and test

Install stable Rust and Clippy. The default CLI build also needs Node for the
web console. Lean 4, Charon, Aeneas, and PostgreSQL are needed for the relevant
application proofs and integration tests; `h5i app doctor` checks the proof
toolchain. GPU backend work has separate requirements in the GPU guide.

Run the checks relevant to your change:

```bash
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo build --locked --workspace --all-targets
cargo test --locked --workspace
```

Use `--workspace` so the checks include the crates under `crates/`. For
framework or proof changes, also run the relevant `cargo app` checks; CI checks
the Lean proofs and extracted code. For console changes, build the `web/`
package and verify the embedded UI. If your host cannot run a relevant check,
say so in the pull request.

## Make a focused change

- Keep the Rust implementation, Lean proofs, tests, and docs in sync when an
  application guarantee changes. State clearly which properties are proved,
  tested, or assumed; use [the trust model](docs/design/design-app-trust.md) as the reference.
- Keep GPU execution optional. Compare its results with the CPU path and keep
  application state changes on the established kernel path.
- For security-sensitive code, follow [SECURITY.md](SECURITY.md). Refuse
  policies the host cannot enforce, test rejection paths, and treat browser,
  box, and network data as untrusted.
- Update the manual when CLI behavior changes. Regenerate its published forms
  with `./scripts/gen_man.sh` and `python3 scripts/gen_manual.py`.
- Use fake credentials and redacted artifacts in tests, docs, and pull requests.

## Pull requests

Keep the change narrow. Describe what changed, the checks you ran, and any
platform or toolchain you could not test. Call out changes to proof assumptions,
security boundaries, or user-visible behavior. Add screenshots for visible UI
changes.
