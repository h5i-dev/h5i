# h5i

<p align="left">
  <a href="https://github.com/h5i-dev/h5i/actions/workflows/test.yaml"><img alt="tests" src="https://github.com/h5i-dev/h5i/actions/workflows/test.yaml/badge.svg"></a>
  <a href="https://github.com/h5i-dev/h5i/stargazers"><img alt="GitHub stars" src="https://img.shields.io/github/stars/h5i-dev/h5i?style=social"></a>
  <a href="https://github.com/h5i-dev/h5i/releases"><img alt="release" src="https://img.shields.io/github/v/release/h5i-dev/h5i?label=release"></a>
  <a href="https://crates.io/crates/h5i-app"><img alt="release" src="https://img.shields.io/crates/v/h5i-app"></a>
</p>

**h5i** (pronounced *high-five*) is an Axum-based Rust web framework designed for formal verification with Lean 4 and optional GPU acceleration. It also provides integrated development infrastructure for browser automation, red-teaming, security regression testing, and sandboxed AI agents.

<p align="center">
  <img src="./docs/_static/calculator-rust-lean-proof.svg" alt="Watching a sandboxed browser session from the host" width="99%" />
</p>


<a href="https://trendshift.io/repositories/46160?utm_source=trendshift-badge&amp;utm_medium=badge&amp;utm_campaign=badge-trendshift-46160" target="_blank" rel="noopener noreferrer"><img src="https://trendshift.io/api/badge/trendshift/repositories/46160/daily?language=Rust" alt="h5i on Trendshift" width="250" height="55"/></a> 

> h5i's CLI-based red-teaming tools are framework-independent and agent-friendly. They can test any website or web application, including existing projects, without adopting `h5i-app`.

---

## 1. Install

```bash
# Add the Rust web framework
cargo add h5i-app

# Install the h5i CLI and development tools
curl -fsSL https://h5i.dev/install.sh | sh -s -- --websec --recon --test

# AI agent integration (optional)
npx skills add h5i-dev/h5i
```

---

## 2. Verifiable Web Framework

[`h5i-app`](crates/h5i-app/README.md) is a composable, Axum-based Rust web framework built around a verifiable application kernel.

```toml
[dependencies]
h5i-app = { version = "0.1.2", features = ["http", "postgres"] }
```

- **Kernel-based architecture**: Separate application logic from infrastructure to make core behavior easier to verify and optimize.
- **Formal verification**: Translate Rust application logic into Lean 4 using [Aeneas](https://github.com/AeneasVerif/aeneas) and prove properties such as authorization, data isolation, and business logic correctness.
- **Optional GPU acceleration**: Execute supported application kernels on GPUs while retaining CPU execution.

## 3. Development Infrastructure

h5i provides a unified CLI for developing, debugging, and security-testing web applications with AI agents.

For example, you can run professional red-teaming by just opening your favorite agent harness and saying *"Audit `https://<your-target>.com` with h5i."*

<p align="center">
  <img src="./docs/_static/agent-audit-target.svg" alt="A coding agent auditing target.com with h5i commands while the browser captures its traffic" width="99%" />
</p>


> Most of `h5i` CLI commands work with any web services regardless of their framework, and no migration to `h5i-app` is required.

**Project template**

```bash
# Build and verify a Rust web application
h5i app new counter && cd counter                       # Create an h5i-app project
h5i app extract                                         # Translate Rust logic into Lean 4
h5i app prove                                           # Build and check formal proofs
h5i app mutate                                          # Test proof sensitivity with mutations
```

**Browser automation:** Drive Chromium through h5i's capture proxy: navigate, fill forms, and test authenticated workflows, with every request policy-checked and recorded.

```bash
h5i browser open https://example.com                    # Open a website in Chromium, captured by h5i
h5i browser click @e3                                   # Interact with page elements
```

**HTTP security testing:** Capture, modify, replay, and compare HTTP requests to investigate vulnerabilities.

```bash
h5i websec requests                                     # Inspect captured HTTP traffic
h5i websec replay req_42 --set query.id=456             # Modify and replay a request
```

**Recon:** Discover endpoints, parameters, and hidden attack surfaces through crawling and JavaScript analysis.

```bash
h5i recon crawl --max-requests 200                      # Crawl the application for attack surfaces
```

**CI/CD:** Save confirmed attack flows and replay them in CI/CD to catch security regressions.

```bash
h5i test .h5i-tests/tests/auth.yaml --target https://staging.example.com                # Replay a security test
h5i test --target https://staging.example.com --openapi openapi.yaml --min-coverage 80  # Fail CI when API coverage drops
```

**Sandboxed workflow:** Run agents in isolated environments with configurable filesystem and network restrictions.

```bash
h5i box --name lab --profile browser && h5i box shell lab -- claude   # Confine an agent's files, network and secrets
h5i box export lab                                                    # Review its changes before they reach your repository
```

**Intuitive dashboard:** While agents access these capabilities through a unified CLI, you can also monitor their activity through the dashboard.

```bash
h5i ui  # Open the local dashboard   
```

<p align="center">
  <img src="./docs/_static/sandbox-ui-demo.png" alt="h5i local dashboard showing browser and session activity" width="99%" />
</p>

---

## 5. License

h5i is licensed under the Apache License 2.0. See [LICENSE](LICENSE).
