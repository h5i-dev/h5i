# h5i

<p align="left">
  <a href="https://github.com/h5i-dev/h5i/actions/workflows/test.yaml"><img alt="tests" src="https://github.com/h5i-dev/h5i/actions/workflows/test.yaml/badge.svg"></a>
  <a href="https://github.com/h5i-dev/h5i/stargazers"><img alt="GitHub stars" src="https://img.shields.io/github/stars/h5i-dev/h5i?style=social"></a>
  <a href="https://github.com/h5i-dev/h5i/releases"><img alt="release" src="https://img.shields.io/github/v/release/h5i-dev/h5i?label=release"></a>
  <a href="https://crates.io/crates/h5i-app"><img alt="release" src="https://img.shields.io/crates/v/h5i-app"></a>
</p>

**h5i** (pronounced *high-five*) unifies web development, security testing, and formal verification around a composable axum-based Rust framework. Its kernel-based architecture enables Lean 4 verification and optional GPU acceleration, while its built-in red-teaming and CI/CD toolkit enables automated vulnerability discovery.

> h5i's CLI-based red-teaming tools are framework-independent and agent-friendly. They can test any website or web application, including existing projects, without adopting `h5i-app`.

```bash
# Build and verify a Rust web application
h5i app new counter && cd counter                       # Create an h5i-app project
h5i app extract                                         # Translate Rust logic into Lean 4
h5i app prove                                           # Build and check formal proofs
h5i app mutate                                          # Test proof sensitivity with mutations

# Red-team any web application
h5i browser open https://example.com                    # Open a website in the browser
h5i browser click @e3                                   # Interact with page elements
h5i websec requests                                     # Inspect captured HTTP traffic
h5i websec replay req_42 --set query.id=456             # Modify and replay a request
h5i recon crawl --max-requests 200                      # Crawl the application for attack surfaces

# Run security regression tests in CI/CD
h5i test .h5i-tests/tests/auth.yaml --target https://staging.example.com  # Replay a security test
```

<a href="https://trendshift.io/repositories/46160?utm_source=trendshift-badge&amp;utm_medium=badge&amp;utm_campaign=badge-trendshift-46160" target="_blank" rel="noopener noreferrer"><img src="https://trendshift.io/api/badge/trendshift/repositories/46160/daily?language=Rust" alt="h5i on Trendshift" width="250" height="55"/></a> 

---

## 1. Install

```bash
curl -fsSL https://h5i.dev/install.sh | sh -s -- --websec --recon --test         # `websec`, `recon`, and `test` are optional plugins
# curl -fsSL https://raw.githubusercontent.com/h5i-dev/h5i/main/install.sh | sh  # if you would rather not add a domain to the chain:
# cargo install --path .                                                         # build from source
# h5i plugin list                                                                # says what is installed
```

The agent-facing interface is a skill, and the binary carries it:

```bash
npx skills add h5i-dev/h5i         # if you do not have the binary yet
# h5i skill install                # writes it where your runtime looks
```

---

## 2. Build, Verify & Test

### Verifiable Web Framework

[`h5i-app`](crates/h5i-app/README.md) is an Axum-based Rust web framework that makes application logic amenable to Lean 4.

```toml
[dependencies]
h5i-app = { version = "0.1.1", features = ["http", "postgres"] }
```

We can write web applications in Rust, translate them into Lean 4 via [Aeneas](https://github.com/AeneasVerif/aeneas), and prove various properties like authorization, 
isolation, business logics, and more.

<p align="center">
  <img src="./docs/_static/calculator-rust-lean-proof.svg" alt="Watching a sandboxed browser session from the host" width="99%" />
</p>

### Security Testing: Red-Teaming & CI/CD

Let AI agents explore, attack, and test web applications using h5i's integrated security toolkit. 

> `h5i` CLI works with any web services regardless of their framework, and no migration to `h5i-app` is required.

- **Browser automation:** Navigate applications, interact with forms, and test authenticated workflows.
- **HTTP security testing:** Capture, modify, replay, and compare HTTP requests to investigate vulnerabilities.
- **Recon:** Discover endpoints, parameters, and hidden attack surfaces through crawling and JavaScript analysis.
- **CI/CD:** Save confirmed attack flows and replay them in CI/CD to catch security regressions.
- **Sandboxed workflow**: Run agents in isolated environments with configurable filesystem and network restrictions.

 Just open your favorite agent harness and say *"Audit `https://<your-target>.com` with h5i."*

<p align="center">
  <img src="./docs/_static/agent-audit-target.svg" alt="A coding agent auditing target.com with h5i commands while the browser captures its traffic" width="99%" />
</p>

---

## 3. Dashboard

While agents access these capabilities through a unified CLI, you can also monitor their activity through the dashboard:

```bash
h5i ui    # Open the local dashboard
```

<p align="center">
  <img src="./docs/_static/sandbox-ui-demo.png" alt="h5i local dashboard showing browser and session activity" width="99%" />
</p>

---

## 4. Documentation

- [Official Website](https://h5i.dev/): project overview, [Slides](https://h5i.dev/pitch/)
- [MANUAL.md](docs/MANUAL.md) / `man h5i`: full command reference
- [CONTRIBUTING.md](CONTRIBUTING.md): we welcome contributions of any kind
- `curl -fsSL https://h5i.dev/man/man1/h5i.1 -o ~/.local/share/man/man1/h5i.1`: install the man page

---

## 5. License

h5i is licensed under the Apache License 2.0. See [LICENSE](LICENSE).

---

## 6. Contributors

<a href="https://github.com/h5i-dev/h5i/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=h5i-dev/h5i" alt="h5i contributors" />
</a>
