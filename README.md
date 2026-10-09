 <p align="center">
  <a href="https://h5i.dev/" target="_blank">
    <img src="./docs/_static/logo.png" alt="h5i logo" height="126">
  </a>
</p>

<p align="center">
  <a href="https://github.com/h5i-dev/h5i/actions/workflows/test.yaml"><img alt="tests" src="https://github.com/h5i-dev/h5i/actions/workflows/test.yaml/badge.svg"></a>
  <a href="https://github.com/h5i-dev/h5i/blob/main/LICENSE"><img alt="Apache-2.0" src="https://img.shields.io/github/license/h5i-dev/h5i?color=blue"></a>
  <a href="https://github.com/h5i-dev/h5i/stargazers"><img alt="GitHub stars" src="https://img.shields.io/github/stars/h5i-dev/h5i?style=social"></a>
  <a href="https://github.com/h5i-dev/h5i/releases"><img alt="release" src="https://img.shields.io/github/v/release/h5i-dev/h5i?label=release"></a>
</p>

<h1 align="center">The Agent-Native Web Security Workspace</h1>

**h5i** (pronounced *high-five*) is a workspace for securing web applications through two complementary approaches: **finding vulnerabilities** with AI agents and **proving correctness** with formal verification.

- **Find Bugs (Red-Teaming):** Give AI agents a unified toolkit for browser automation, HTTP traffic manipulation, and reconnaissance. Test existing web applications for vulnerabilities, reproduce attacks, and run security regression checks in CI/CD.
- **Prove Correctness (Formal Verification):** Build applications with `h5i-app`, an Axum-based Rust framework designed for verification with Lean 4. Prove authorization, data isolation, and business logic properties directly against your Rust implementation.

**Build with agents. Red-team for bugs. Formally verify properties.**

```bash
# Find bugs in web applications
h5i browser open https://example.com                   # Open a website in the browser
h5i browser click @e3                                   # Interact with page elements
h5i websec requests                                     # Inspect captured HTTP traffic
h5i websec replay req_42 --set query.id=456             # Modify and replay a request
h5i recon crawl --max-requests 200                      # Crawl the application for attack surfaces

# Prove correctness of Rust application logic
h5i app new counter && cd counter                       # Create an h5i-app project
h5i app extract                                         # Translate Rust logic into Lean 4
h5i app prove                                           # Build and check formal proofs
h5i app mutate                                          # Test proof sensitivity with mutations
```

---

## 1. Install

Install h5i and its security testing plugins:

```bash
curl -fsSL https://h5i.dev/install.sh | sh -s -- --websec --recon --test   # Install h5i with optional security plugins
h5i plugin list                                                           # Show installed plugins
```

Alternatively, install from [GitHub](https://github.com/h5i-dev/h5i) or build from source:

```bash
curl -fsSL https://raw.githubusercontent.com/h5i-dev/h5i/main/install.sh | sh   # Install directly from GitHub
cargo install --path .                                                         # Build from a local source checkout
```

**Using h5i with AI agents?** Install the agent skill so your agent can discover and use h5i's commands directly:

```bash
npx skills add h5i-dev/h5i                 # Add the h5i skill to your AI agent
# h5i skill install                        # Alternatively, install the skill using the h5i CLI
```

---

## 2. Find Bugs: Red-Team with Agents

Let AI agents explore, attack, and test web applications using h5i's integrated security toolkit. **h5i works with existing applications regardless of their framework**—no migration to `h5i-app` is required.

- **Browser automation:** Navigate applications, interact with forms, and test authenticated workflows using a lightweight Rust browser or Chromium.
- **HTTP security testing:** Capture, modify, replay, and compare HTTP requests to investigate broken access control, injection flaws, and other vulnerabilities.
- **Reconnaissance:** Discover endpoints, parameters, and hidden attack surfaces through crawling and JavaScript analysis.
- **Reproducible testing:** Save confirmed attack flows and replay them in CI/CD to catch security regressions.

Agents access these capabilities through a unified CLI. You can also monitor their activity and inspect sessions through the local dashboard:

```bash
h5i ui    # Open the local dashboard
```

<p align="center">
  <img src="./docs/_static/sandbox-ui-demo.png" alt="h5i local dashboard showing browser and session activity" width="99%" />
</p>

For additional isolation, h5i also supports **optional sandboxed execution** with configurable filesystem and network restrictions.

See the [CLI manual](docs/MANUAL.md) for complete command documentation and the [CI regression example](examples/security-regression-ci) for automated security testing.

---

## 3. Prove Correctness: Build on h5i-app

Red-teaming discovers vulnerabilities through testing. Formal verification takes a complementary approach: **proving that specified properties hold for all possible inputs and behaviors covered by the model and assumptions.**

`h5i-app` is an Axum-based Rust web framework that makes application logic amenable to formal verification in Lean 4.

- **Write in Rust:** Define application logic as pure Rust functions, separating state transitions from HTTP handlers and database operations.
- **Prove in Lean 4:** Translate Rust implementations using [Aeneas](https://github.com/AeneasVerif/aeneas) and verify authorization, tenant isolation, and business invariants.
- **Reason across requests:** Prove properties of state transitions and sequences of committed requests, not just individual functions.
- **Test your proofs:** Use mutation testing to check whether intentionally introduced implementation changes invalidate the relevant proofs.

Add `h5i-app` to your Rust project:

```toml
[dependencies]
h5i-app = { version = "0.1", features = ["http", "postgres"] }
```

For example, the following Rust function defines how a calculator application's state changes in response to a command:

```rust
// Simplified example from examples/app/calculator
pub fn transition(
    actor: &Principal,
    snap: &Snapshot,
    cmd: &Command,
) -> Result<(Option<Memory>, Reply), Error> {
    match cmd {
        Command::Set { value } => Ok((
            Some(Memory { user: actor.user, value: *value }),
            Reply::Value(*value),
        )),
        Command::Apply { op, arg } => {
            let m = memory_of(&snap.memories, actor.user);
            match compute(*op, m, *arg) {
                Ok(v) => Ok((
                    Some(Memory { user: actor.user, value: v }),
                    Reply::Value(v),
                )),
                Err(e) => Err(e),
            }
        }
        Command::Get => Ok((
            None,
            Reply::Value(memory_of(&snap.memories, actor.user)),
        )),
    }
}
```

Because the logic is expressed as a pure function, it can be translated into Lean 4 and reasoned about mathematically.

For example, we can prove that **after a successful state-changing command, reading the user's state returns the value produced by that command**:

```lean
theorem get_after
    (a : Principal) (s s' : Snapshot)
    (c : Command) (w : Option Memory) (v : U64)
    (hroom : s.memories.length < Usize.max)
    (ht : transition a s c = ok (.Ok (w, .Value v)))
    (hs : apply s w = ok s') :
    transition a s' .Get = ok (.Ok (none, .Value v))
```

Unlike ordinary testing, this theorem establishes the property for every state and command satisfying its assumptions—not just selected test cases.

The same approach can be used to prove security-critical properties such as:

- **Authorization:** Unauthorized operations cannot modify protected state.
- **Tenant isolation:** Operations by one tenant cannot access or modify another tenant's data.
- **Business invariants:** Application-specific constraints are preserved across state transitions.

Proofs establish the stated properties under their assumptions; they do not automatically guarantee the security of every component of the deployed application.

To get started:

```bash
h5i app new counter && cd counter      # Create a new verifiable Rust application
h5i app extract                        # Generate Lean 4 representations of Rust logic
h5i app prove                          # Check the proofs against the implementation
h5i app mutate                         # Introduce mutations and check proof sensitivity
```

See the [h5i-app documentation](crates/h5i-app/README.md) for a complete example, the Axum integration, and the formal verification workflow.

---

## 4. Documentation

- [Official website](https://h5i.dev/) — Project overview and [presentation slides](https://h5i.dev/pitch/)
- [CLI manual](docs/MANUAL.md) / `man h5i` — Complete command reference
- [h5i-app](crates/h5i-app/README.md) — Rust framework and Lean 4 verification
- [Security regression CI example](examples/security-regression-ci) — Reproducible security tests
- [Contributing](CONTRIBUTING.md) — Development guide and contribution instructions

---

## 5. License

h5i is licensed under the Apache License 2.0. See [LICENSE](LICENSE).

---

## 6. Contributors

Contributions of all kinds are welcome!

<a href="https://github.com/h5i-dev/h5i/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=h5i-dev/h5i" alt="h5i contributors" />
</a>

<a href="https://trendshift.io/repositories/46160?utm_source=trendshift-badge&amp;utm_medium=badge&amp;utm_campaign=badge-trendshift-46160" target="_blank" rel="noopener noreferrer"><img src="https://trendshift.io/api/badge/trendshift/repositories/46160/daily?language=Rust" alt="h5i on Trendshift" width="250" height="55"/></a> 
