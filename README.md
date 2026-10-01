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


**h5i** (pronounced *high-five*) is a unified workspace for building secure web applications through two complementary approaches: finding bugs and proving correctness.

* **Find Bugs (Red-Teaming):** Equip your AI agents with headless browser automation and deep HTTP traffic control to explore apps, intercept requests, and uncover vulnerabilities inside a configurable sandbox. Perfect for bug bounties, penetration testing, and CI regressions.
* **Prove Correctness (Formal Verification):** Build your backend on `h5i-app` (our Axum-based Rust framework) and use Lean 4 to formally verify your application logic, proving everything from tenant isolation and authorization to core business state invariants.

**Build with agents. Red-team for bugs. Formally verify properties.**

<table align="center">
  <tr>
    <td align="center">
      <strong>Red-teaming</strong><br>
      <sub>Browser + direct HTTP control</sub>
    </td>
    <td align="center">
      <strong>Formal verification</strong><br>
      <sub><a href="#3-prove-correctness-build-on-h5i-app">Rust + Lean 4</a></sub>
    </td>
    <td align="center">
      <strong>CI/CD</strong><br>
      <sub>Continuous security checks</sub>
    </td>
    <td align="center">
      <strong>Sandboxed workflows</strong><br>
      <sub>Configurable limits + auditable sessions</sub>
    </td>
  </tr>
</table>



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
# h5i skill show policy            # or just read a page
```

<a href="https://trendshift.io/repositories/46160?utm_source=trendshift-badge&amp;utm_medium=badge&amp;utm_campaign=badge-trendshift-46160" target="_blank" rel="noopener noreferrer"><img src="https://trendshift.io/api/badge/trendshift/repositories/46160/daily?language=Rust" alt="h5i on Trendshift" width="250" height="55"/></a>

---

## 2. Find bugs: red-team with agents

### 2.1. Drive the browser

A **session** combines one page state, cookie jar, network policy, and request
record. Agents read pages, interact with elements, and extract structured data
through one CLI:

```bash
h5i browser open https://docs.rs/ --allow docs.rs
h5i browser snapshot                        # page outline with @ref handles
h5i browser click @e3
h5i browser extract '{"titles": ["h2"]}'    # structured extraction
h5i browser read https://docs.rs/           # one page, no persistent session
```

### 2.2. Capture, replay, and compare traffic

The native browser owns its network layer, so agents capture, inspect, edit,
replay, and compare HTTP traffic directly. Sites that need
full Chromium go through the same workbench via `h5i browser proxy`.

```bash
h5i browser open https://target.example --capture --allow target.example
h5i websec requests                                  # list messages and IDs
h5i websec replay req_42 --set query.id=456          # edit and resend one
h5i websec diff res_42 res_43                        # compare responses
h5i websec sequence flow.json                        # run a multi-step test
h5i recon endpoints --state confirmed                # discovery, each row names its evidence
```

### 2.3. Replay confirmed flows in CI

We can replay confirmed attack flows in CI. See
[`examples/security-regression-ci`](examples/security-regression-ci) for the
GitHub Actions template:

```yaml
- uses: h5i-dev/h5i@v1
  with:
    target: http://localhost:3000
    tests: .h5i-tests/tests
```

### 2.4. Sandbox and audit the agent

AI agnet might run out of control and perform dangerous operations. To reduce such risks,
h5i offers an auditable sandbox, where `h5i browser requests` and
`h5i browser audit` show what the agent did. For stronger isolation, a profile
in `.h5i/env.toml` picks a tier (`workspace`, `process`, `supervised`,
`container`, or `microvm`) and limits network egress and filesystem access.

```bash
h5i box create alpha --profile agent-claude   # sandboxed git worktree
h5i box shell alpha                           # interactive confined session
h5i browser open https://docs.rs/ --in alpha  # browser inside the box
h5i box propose alpha                         # reviewable snapshot
h5i box apply alpha                           # merge approved changes
h5i box rm alpha                              # discard it
```

Watch it all from the host with `h5i ui`:

<p align="center">
  <img src="./docs/_static/sandbox-ui-demo.png" alt="Watching a sandboxed browser session from the host" width="99%" />
</p>

---

## 3. Prove correctness: build on h5i-app

Red-teaming finds the bugs you did not anticipate. `h5i-app` is a Rust web
framework for proving, in Lean 4, the properties you can state.

```toml
[dependencies]
h5i-app = { version = "0.1", features = ["http", "postgres"] }
```

- Write the logic as pure Rust functions and prove it in Lean 4 via [Aeneas](https://github.com/AeneasVerif/aeneas).
- Serve it with [axum](https://github.com/tokio-rs/axum); handlers never touch the database.
- Prove that invariants hold for the rows loaded back from the database.
- Prove properties across requests, for every order in which clients' requests commit.

The kernel is one function that decides what a command does. This one, from the
[calculator tutorial](examples/app/tutorials/calculator/TUTORIAL.md), keeps one
number per user:

```rust
pub fn transition(actor: &Principal, snap: &Snapshot, cmd: &Command) -> Result<(Option<Memory>, Reply), Error> {
    match cmd {
        Command::Set { value } => Ok((Some(Memory { user: actor.user, value: *value }), Reply::Value(*value))),
        Command::Apply { op, arg } => {
            let m = memory_of(&snap.memories, actor.user);
            match compute(*op, m, *arg) {
                Ok(v) => Ok((Some(Memory { user: actor.user, value: v }), Reply::Value(v))),
                Err(e) => Err(e),
            }
        }
        Command::Get => Ok((None, Reply::Value(memory_of(&snap.memories, actor.user)))),
    }
}
```

Aeneas translates it to Lean, where theorems about it are ordinary Lean:

```lean
theorem get_after (a : Principal) (s s' : Snapshot) (c : Command) (w : Option Memory) (v : U64)
    (hroom : s.memories.length < Usize.max)
    (ht : transition a s c = ok (.Ok (w, .Value v))) (hs : apply s w = ok s') :
    transition a s' .Get = ok (.Ok (none, .Value v))
```

See [crates/h5i-app](crates/h5i-app/README.md) for the full kernel, the axum
server around it, and the proof workflow, and [TRUST.md](docs/app/TRUST.md)
for exactly what is proven and what is assumed.

---

## 4. Tutorial

- [Web Security Tutorial with h5i — Part 1: HTTP Request Tampering](https://medium.com/@Koukyosyumei/web-security-tutorial-with-h5i-part-1-http-request-tampering-39c4a0857b85)

---

## 5. Documentation

- [Official Website](https://h5i.dev/): project overview, [Slides](https://h5i.dev/pitch/)
- [MANUAL.md](docs/MANUAL.md) / `man h5i`: full command reference
- [CONTRIBUTING.md](CONTRIBUTING.md): we welcome contributions of any kind
- `curl -fsSL https://h5i.dev/man/man1/h5i.1 -o ~/.local/share/man/man1/h5i.1`: install the man page

---

## 6. FAQ

<details>
<summary>What is h5i?</summary>

h5i is an open-source workspace for building secure web applications. The
`h5i` CLI is a red-teaming tool for AI agents. It drives a target through its
own lightweight Rust browser, or through a capture proxy in front of Chromium,
and lets the agent capture, inspect, replay, and compare the HTTP traffic from
policy-controlled, auditable, sandboxed sessions. `h5i-app` is a Rust web
framework whose application logic is proven in Lean 4.

</details>

<details>
<summary>Do I need h5i-app to red-team, or the h5i CLI to use h5i-app?</summary>

No. The CLI tests any running web application, whatever it is built on.
`h5i-app` is a crate you add to a Rust project and needs no h5i binary. They
meet when an agent builds an application on `h5i-app` inside an h5i sandbox and
red-teams it from the same box.

</details>

<details>
<summary>What does h5i-app actually prove?</summary>

Charon and Aeneas translate the kernel, the SQL planner and compiler, the JSON
writer, and the token codec to Lean, and every theorem is about that extracted
code. Proven: properties of `transition`, that compiled statements touch only
the tenant's rows, and that an invariant kept by accepted writes holds in every
database state and every snapshot loaded back, for every order in which
requests commit. Trusted, not proven: axum, PostgreSQL's statement semantics,
the HMAC key, the clock, and the translation tools. [TRUST.md](docs/app/TRUST.md)
has the full list.

</details>

<details>
<summary>Why use h5i instead of Playwright or Puppeteer?</summary>

Use Playwright or Puppeteer when maximum compatibility with complex websites is
the priority. Use h5i when you want lower resource use, direct network controls,
a complete session record, built-in HTTP testing tools, or a sandbox for the
browser and agent.

</details>

<details>
<summary>Is h5i a replacement for Burp Suite?</summary>

Not for every use case. h5i is useful when an AI agent needs to browse an
application and capture, edit, replay, and compare its HTTP traffic through one
interface. Its native browser needs no proxy; the optional agent-browser lane
creates and configures a per-session proxy and CA. Burp Suite remains better
suited to mature manual workflows, automated scanning, extensions, and
low-level protocol testing.

</details>

<details>
<summary>Does h5i work on every website?</summary>

No. h5i works best for content-heavy websites and common browser interactions.
For sites that need Chromium, run `h5i browser proxy <url>` and open the
printed command with agent-browser, or run Chromium inside an h5i sandbox.
Linux trusts the session CA. macOS passes `--ignore-https-errors`.

</details>

<details>
<summary>Is h5i sandboxed by default?</summary>

The browser uses lightweight process isolation when available. For stronger
isolation, place the browser or the agent's entire workflow inside a supervised
network sandbox, container, or microVM.

</details>

<details>
<summary>Can h5i prevent prompt injection?</summary>

No browser can reliably detect or prevent every prompt injection. h5i reduces
the potential impact by treating page content as untrusted, restricting network
and filesystem access, isolating credentials, and recording the resulting
actions for review.

</details>

<details>
<summary>Can the agent see my passwords or cookies?</summary>

The agent can reference a named credential without reading its value, or a
human can take control to log in. The authenticated session continues without
returning the password or cookie to the model.

</details>

<details>
<summary>Does h5i keep my data local?</summary>

h5i has no hosted service and stores its sessions locally. Browser traffic still
goes to websites you allow, and model traffic goes to your configured model
provider.

</details>

---

## 7. License

Apache-2.0. See [LICENSE](LICENSE).

---

## 8. Contributors

<a href="https://github.com/h5i-dev/h5i/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=h5i-dev/h5i" alt="h5i contributors" />
</a>
