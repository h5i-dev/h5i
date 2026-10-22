# Security policy

h5i is an Axum-based Rust web framework for building and verifying web apps
with Lean 4, featuring optional GPU acceleration and integrated development
infrastructure for red-teaming and CI/CD. Security reports may concern the
framework, its proof claims, or the tools used to test and run applications.

## Supported versions

Security fixes land on `main` first and may ship in a patch release. Older
releases are not maintained automatically. Use `main` or the latest release.

## Report a vulnerability

Use [GitHub's private vulnerability reporting](https://github.com/h5i-dev/h5i/security/advisories/new).
If that is unavailable, open a public issue requesting a private contact path
without including exploit details.

Please include the affected version or commit, build features, platform, steps
to reproduce with fake credentials, the expected security boundary, and the
observed behavior. For sandbox issues, include the isolation tier, profile,
command, and `h5i box probe` output. Do not attach real secrets, private source,
or full session logs.

Maintainers will triage the report, prepare a fix, and disclose it once users
have an upgrade path. A regression test may follow the release if publishing it
earlier would expose a working exploit.

## Security boundaries

- **Verified applications:** A Lean proof applies to extracted Rust code
  and its stated assumptions. Changes to extraction, proof checks, generated
  code, database access, or tenant scoping can invalidate a security claim. See
  [the trust model](docs/app/TRUST.md) for what is proven, enforced, tested, and
  assumed.
- **GPU execution:** The optional GPU adapters are checked through shared
  semantics and differential tests; GPU execution is not formally proved.
  Keep validation and state changes on the verified application path. See
  [GPU design](docs/app/GPU.md).
- **Red-teaming and CI/CD:** Browser capture, request replay, reconnaissance,
  and security regression tests can handle hostile content and credentials.
  Requests must obey their configured scope and recording policy. Treat page
  content, captured traffic, and test artifacts as untrusted input.
- **Sandboxing and credentials:** Isolation guarantees depend on the selected
  tier and host capabilities. An unenforceable explicit policy must fail rather
  than silently weaken. Keep secret values out of receipts, routine logs, and
  examples; inspect captures and exports before sharing them. See the
  [manual's Limits section](docs/MANUAL.md).

Changes to these boundaries should test both allowed behavior and rejection of
malformed or unauthorized input. Document any change to a proof assumption or
user-visible guarantee in the same pull request.

## Authorized testing

Use `h5i websec` and `h5i recon` only against systems you own or have written
permission to test. Capture may store complete requests and responses, including
credentials; enable and share it with care. Use fake tokens in reports, tests,
and examples.
