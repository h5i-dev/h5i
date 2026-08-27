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

<h1 align="center">A Secure, Auditable Browser for AI Agents</h1>

**h5i** (pronounced *high-five*) is a lightweight browser for policy-controlled, auditable agent access to the web. Every session records allowed and denied network requests in a reviewable receipt. Run it directly, sandbox only the browser, or contain the agent’s entire workflow in one disposable environment.


<table align="center">
  <tr>
    <td align="center">
      <strong>Pure Rust</strong><br>
      <sub>No Chromium or V8</sub>
    </td>
    <td align="center">
      <strong>~80% less peak memory</strong><br>
      <sub><a href="./crates/h5i-browser-light/DESIGN.md">In our benchmarks</a></sub>
    </td>
    <td align="center">
      <strong>~5× faster reads</strong><br>
      <sub><a href="./crates/h5i-browser-light/DESIGN.md">In our benchmarks</a></sub>
    </td>
    <td align="center">
      <strong>Sandboxed & auditable</strong><br>
      <sub>Browser-only or full workflow</sub>
    </td>
  </tr>
</table>

**Pure Rust. No Chromium. No V8.**

```bash
h5i browser open https://example.com
h5i browser snapshot                    # the page as a model should read it
h5i browser click @e3
h5i browser requests                    # what it asked for, and what was refused
h5i browser audit                       # the whole session: verbs, fetches, handovers, ending
h5i browser close

h5i browser read https://example.com          # or: one page, no session
```

<a href="https://trendshift.io/repositories/46160?utm_source=trendshift-badge&amp;utm_medium=badge&amp;utm_campaign=badge-trendshift-46160" target="_blank" rel="noopener noreferrer"><img src="https://trendshift.io/api/badge/trendshift/repositories/46160/daily?language=Rust" alt="h5i on Trendshift" width="250" height="55"/></a>

<p align="center">
  <img src="./docs/_static/browser-demo.gif" alt="An agent reading and acting on a page through h5i" width="99%" />
</p>

---

## 1. Install

```bash
curl -fsSL https://h5i.dev/install.sh | sh
# curl -fsSL https://raw.githubusercontent.com/h5i-dev/h5i/main/install.sh | sh  # if you would rather not add a domain to the chain:
# cargo install --path .                                                         # build from source
```

The agent-facing interface is a skill, and the binary carries it:

```bash
npx skills add h5i-dev/h5i         # if you do not have the binary yet
# h5i skill install                # writes it where your runtime looks
# h5i skill show policy            # or just read a page
```

Two optional runtimes add stronger sandbox tiers: rootless
[Podman](https://podman.io/) provides `container`, while
[microsandbox](https://microsandbox.dev) (`msb`) provides `microvm` on a host
with hardware virtualization (`/dev/kvm` on Linux or Apple Silicon on macOS).

---

## 2. Use it

### 2.1. A browser session

A **session** is the whole agent-facing surface: one page state, one cookie jar,
one request log, one policy:

```bash
h5i browser open https://docs.rs/ --allow docs.rs
h5i browser snapshot                        # outline, with @ref handles
h5i browser snapshot --delta                # only what changed since the last read
h5i browser click    @e3
h5i browser type     @e5 "serde"
h5i browser extract  '{"titles": ["h2"]}'   # structured, by selector
h5i browser markdown                        # the page a reader would read
h5i browser login                           # hand the page to the human at the viewer
h5i browser close
```

Running several at once is what names are for:

```bash
h5i browser open https://example.com/login --session auth --new
h5i browser open https://example.com/      --session public --new
h5i browser snapshot --session auth
```

Read the record:

```bash
h5i browser requests           # every request, including the refusals
h5i browser audit              # the whole session: verbs, fetches, handovers, ending
h5i browser status             # placement, policy digest, who saw the network
h5i browser list               # every session on this machine, and which is default
```

### 2.2. The configurable sandbox

While h5i runs in a light-weight sandbox by default, we can further specify
fine-grained setting in `.h5i/env.toml`.

```toml
[profile.reading]
isolation = "supervised"          # workspace | process | supervised | container | microvm

[profile.reading.net]
mode   = "host"
egress = ["docs.rs", "static.crates.io"]   # everything else is refused

[profile.reading.fs]
read  = ["/usr", "/etc"]          # replaces the defaults, so grant what it needs
write = []

[profile.reading.resources]
mem   = "512M"
procs = 64

secrets = ["ACME_PASS"]           # the only $H5I_SECRET_* it may substitute
```

Give it to a browser session through a box:

```bash
h5i box --profile reading --name docs
h5i browser open https://docs.rs/ --in docs
```

### 2.3. A sandbox holds more than a browser

The top rung of that ladder is a whole environment. It can hold the code, the
toolchain, the dev server and the agent itself, which is what you want when the
agent is building the app it is about to browse.

```bash
h5i box create alpha --profile agent-claude   # a sandboxed git worktree
h5i box shell alpha                           # an interactive confined session
h5i box run   alpha -- cargo test             # one command; the exit code passes through
h5i box propose alpha                         # freeze the work into a reviewable snapshot
h5i box apply   alpha                         # merge it onto the parent branch
h5i box export  alpha                         # patch, report and receipts you can read
h5i box rm      alpha                         # throw it away
```

```bash
h5i box share alpha --port 3000            # end-to-end encrypted P2P sharing
h5i box share alpha --port 3000 --tunnel   # or a browser-ready demo link
h5i join <ticket>                          # what the recipient runs
```

```bash
h5i ui # watch the whole fleet in a browser
```

<p align="center">
  <img src="./docs/_static/sandbox-ui-demo.png" alt="Watching a sandboxed browser session from the host" width="99%" />
</p>

---

## 3. What confinement means here

`h5i box probe` reports the tiers your host can run. h5i never silently
downgrades: an unsatisfiable request fails closed.

A browser session uses `process` by default, without a box. The rows below it
are what `--in <box>` reaches for, and the difference that matters to a session
is the one thing `process` cannot do: enforce which addresses may be reached, at
a boundary outside the engine.

| Tier | What enforces it |
| --- | --- |
| `workspace` | a separate git worktree, no confinement |
| `process` | Landlock filesystem allowlist, seccomp deny-list, namespaces, rlimits |
| `supervised` | all of the above, plus a private network namespace with an **nftables egress allowlist pinned to resolved IPs**, DNS pinned by hosts file, and a seccomp-notify socket gate |
| `container` | rootless Podman, dropped capabilities, a portable image, and an HTTP/HTTPS proxy allowlist |
| `microvm` | a hardware-isolated guest with **its own kernel**, booted by [microsandbox](https://microsandbox.dev) (`msb`) from the same OCI images, with the egress allowlist evaluated **by the VM's network stack** |

microvm is the strongest tier and the only one that does not share the host
kernel. It requires msb, hardware virtualization (`/dev/kvm` or Apple Silicon),
and an image; otherwise it is refused, never downgraded.

Host credentials do not enter a box. A runtime-scoped proxy authenticates model
API requests outside the boundary, preventing cross-runtime access. Each box
receives a private, one-time copy of approved HOME state.

---

## 4. Documentation

- [Official Website](https://h5i.dev/): project overview, [Slides](https://h5i.dev/pitch/)
- [MANUAL.md](MANUAL.md) / `man h5i`: full command reference
- [CONTRIBUTING.md](CONTRIBUTING.md): we welcome contributions of any kind
- `h5i man > ~/.local/share/man/man1/h5i.1`: install the man page (generated from the CLI)

---

## 5. FAQ

<details>
<summary>Why not Playwright or Puppeteer?</summary>

They drive a browser. They do not tell you what it reached. h5i's engine is the
HTTP client, so the request log is a decision record it wrote before the bytes
moved, not a trace assembled beside the network. If a request is not in the log,
it did not happen.

</details>

<details>
<summary>Why do I not have to pass a session id?</summary>

Because you share a filesystem with the browser. An opaque id on every verb is
the shape of a remote-browser HTTP API, where the id exists because the client
and the browser have nothing else in common. Here `open` makes a session and
points the default at it, and the verbs that follow land there. The id still
exists in `--json` and in the receipts, where a durable reference belongs. Use
`--session <name>` when you want several at once.

</details>

<details>
<summary>Is a default session sandboxed?</summary>

Yes. `h5i browser open` runs the engine in a process-tier sandbox: Landlock
filesystem scoping, a seccomp filter and rlimits, with no box and no repository
involved. It contains what a compromised engine could *do* — its files, its
environment, its allocations.

It does not contain the network, because a browser needs one, and it does not
upgrade the request lane: a process-tier sandbox corroborates no part of the
log. `--in <box>` is the rung that does both. `--no-sandbox` turns it off, and
a host that cannot confine runs the session unconfined and says so.
`h5i browser status` prints which you have.

</details>

<details>
<summary>Why does one session show up as two processes?</summary>

Because the half that parses a stranger's bytes should not be the half that
holds the decisions. The process h5i starts is the broker: the allowlist, the
HTTP client, the receipt sink, the cookie jar and the credentials. It starts the
renderer, which parses the HTML, runs the cascade and the script and draws the
frame. The renderer holds none of those. Its environment has no `H5I_SECRET_*`
in it, and its only route to the network is to ask the broker, which records
first.

Neither half is a command. `h5i browser open` is unchanged, and
`H5I_BROWSER_NO_SPLIT=1` runs the old single process if you want to compare.

It does not yet make the log evidence against a compromised renderer: the
renderer is still in the broker's network namespace, so it could open a socket
of its own. That closes when the renderer's own profile denies the network.

</details>

<details>
<summary>What do `engine-claimed` and `host-observed` mean?</summary>

`engine-claimed` is the browser's own account of what it fetched: fail-closed,
complete, and still the browser describing itself. `host-observed` means h5i
also saw it at a box's boundary, outside the browser. h5i never merges the two.

</details>

<details>
<summary>Does a box automatically make the lane host-observed?</summary>

No. A box whose policy lets the browser reach the whole network corroborates
nothing. The lane is upgraded only when something outside the engine decides
what may leave: an egress allowlist, or a net mode that denies everything.

</details>

<details>
<summary>Can h5i stop a page from injecting instructions into my agent?</summary>

Not by classifying the text, and it does not try. Page content arrives fenced as
data, escape sequences are stripped, and script is off unless you ask for it,
which removes the delivery channel entirely. What limits a persuaded agent is
the session's policy and the box, not a filter.

</details>

<details>
<summary>Can a human take the browser away from the agent mid-task?</summary>

Yes. `h5i browser take` pauses the agent's mutating verbs while read-only ones
keep working, and handing control back forces a re-snapshot because the page
moved. In a box that pause is enforced, because every verb is carried in from
the host. On this machine it is advisory, and `take` says so.

</details>

<details>
<summary>What happens if the browser crashes mid-task?</summary>

The session is recorded as `died`, with a time, and the next verb is refused
with exit code 69. Nothing restarts automatically. `--restore` carries the old
session's storage into a **new** session with a new id and the inheritance
written down; an id is never reused.

</details>

<details>
<summary>Can a box forge its identity on the forum?</summary>

Not on a confined tier. Forum storage stays outside the sandbox's grants, and
the host, not the payload, supplies the sender, role, box ID, and policy digest.

</details>

<details>
<summary>Does h5i guarantee that posts contain no secrets?</summary>

No. h5i scrubs supported patterns before writing Git objects, but this is
defense in depth, not a guarantee.

</details>

<details>
<summary>Which isolation tiers provide a security boundary?</summary>

`workspace` has no confinement and is refused unless explicitly allowed. Other
tiers enforce a boundary; only `microvm` has its own kernel.

</details>

<details>
<summary>Can h5i stop an agent from sending code to its model provider?</summary>

No. Model egress is a separate policy decision.

</details>

---

## 6. License

Apache-2.0. See [LICENSE](LICENSE).

---

## 7. Contributors

<a href="https://github.com/h5i-dev/h5i/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=h5i-dev/h5i" alt="h5i contributors" />
</a>
