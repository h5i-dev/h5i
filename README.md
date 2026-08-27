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
      <sub><a href="#benchmarks">In our benchmarks</a></sub>
    </td>
    <td align="center">
      <strong>Auditable networking</strong><br>
      <sub>Allowed and denied requests</sub>
    </td>
    <td align="center">
      <strong>Configurable sandboxing</strong><br>
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
```

<a href="https://trendshift.io/repositories/46160?utm_source=trendshift-badge&amp;utm_medium=badge&amp;utm_campaign=badge-trendshift-46160" target="_blank" rel="noopener noreferrer"><img src="https://trendshift.io/api/badge/trendshift/repositories/46160/daily?language=Rust" alt="h5i on Trendshift" width="250" height="55"/></a>

<p align="center">
  <img src="./docs/_static/browser-demo.gif" alt="An agent reading and acting on a page through h5i" width="99%" />
</p>

---

## 1. Install

```bash
curl -fsSL https://h5i.dev/install.sh | sh
# if you would rather not add a domain to the chain:
# curl -fsSL https://raw.githubusercontent.com/h5i-dev/h5i/main/install.sh | sh
```

Or build from source:

```bash
cargo install --path .
```

That installs two binaries: `h5i`, the front door, and `h5i-browser-light`, the
engine it launches to render a page. `install.sh --browser-only` installs the
engine alone, for a CI image that renders a page and nothing else.

Two optional runtimes add stronger sandbox tiers: rootless
[Podman](https://podman.io/) provides `container`, while
[microsandbox](https://microsandbox.dev) (`msb`) provides `microvm` on a host
with hardware virtualization (`/dev/kvm` on Linux or Apple Silicon on macOS).

---

## 2. Use it

### 2.1. A browser session

A **session** is the whole agent-facing surface: one page state, one cookie jar,
one request log, one policy. `open` makes one and every verb that follows acts
on it; `close` ends it. Nothing else is a concept an agent has to learn.

```bash
h5i browser open https://docs.rs/ --allow docs.rs
h5i browser snapshot                        # outline, with @ref handles
h5i browser click    @e3
h5i browser type     @e5 "serde"
h5i browser extract  '{"titles": ["h2"]}'   # structured, by selector
h5i browser markdown                        # the page a reader would read
h5i browser close
```

No session id anywhere. `open` makes one and points the default at it, and
every verb that follows lands there. The opaque id (`br_7k2xqa`) exists, and it
is what `--json` and the receipts carry, because a durable reference has to
survive a rename. It is simply not what you type.

Running several at once is what names are for:

```bash
h5i browser open https://example.com/login --session auth --new
h5i browser open https://example.com/      --session public --new
h5i browser snapshot --session auth
```

Two verbs the shape of an agent loop makes worth having:

```bash
h5i browser snapshot --delta   # only what changed since the last read
h5i browser login              # hand the page to the human at the viewer
```

`--delta` matters because re-reading three hundred lines after every click is
the wrong shape for a loop. `login` closes the page to the agent while a person
types a credential into the live view. The session it establishes stays in the
jar afterwards, and the agent can see *that* it is logged in without ever
reading the cookie that says so.

#### Read the record

```bash
h5i browser requests           # every request, including the refusals
h5i browser audit              # the whole session: verbs, fetches, handovers, ending
h5i browser status             # placement, policy digest, who saw the network
h5i browser list               # every session on this machine, and which is default
```

`requests` is the network layer, and the one to poll in a loop. `audit` is the
one to read afterwards. It merges the verbs the agent asked for, the decision
the engine made about every fetch, the moments a human took the controls, and
how the session ended, into one ordered timeline. Every row says which lane it
came from, and the summary says which logs could not be read at all: an empty
timeline over a log h5i cannot see looks exactly like a session that did
nothing, and those are different findings.

### 2.2. Put the session in a sandbox

Optional, and it changes nothing you type.

```bash
h5i box --profile browser --engine h5i-light --name web
h5i browser open https://example.com --in web
h5i browser snapshot                            # identical verb, identical answer
```

What changes is **who saw the network**. The box enforces its egress allowlist
at its own boundary, outside the browser being described, so the session's
request lane is upgraded from the engine's own account to an outside
observation:

```
requests : engine-claimed (fail-closed, and the engine's own account of what it fetched)
requests : host-observed  (also seen at the box's boundary, outside the engine)
```

Being inside a box is not by itself enough to earn the second line. A box that
lets the browser reach the whole network corroborates nothing, and h5i keeps
calling that session `engine-claimed`.

A boxed session also makes the human takeover real. Every verb is carried in
from the host, so pausing the agent is a boundary rather than a request.

```bash
h5i browser take       # a human takes control; the agent pauses
h5i browser release    # hands it back; the agent must re-snapshot first
h5i box view web       # watch the page, in a loopback-only forward
```

<p align="center">
  <img src="./docs/_static/sandboxed-browser-ui.png" alt="Watching a sandboxed browser session from the host" width="99%" />
</p>

### 2.3. Give an agent a whole sandbox

A box holds more than a browser. It can hold the code, the toolchain, the dev
server and the agent itself, which is what you want when the agent is building
the app it is about to browse.

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

### 2.4. Several agents, one conversation

Agents in separate boxes coordinate through a host-owned forum. They share
information, never permissions: messages carry no capability, and the host
stamps identity and policy context rather than the payload.

```bash
h5i forum create "fix the auth refresh race" --ceiling agent-claude
h5i forum attach alpha --as alpha-worker  --role worker
h5i forum attach beta  --as beta-reviewer --role reviewer
```

```bash
# what an agent gets inside its box: verbs, and no forum or git credential
h5i forum list                                # what is open
h5i forum read <thread>                       # read it, with the posts numbered
h5i forum post <thread> --kind FINDING "..."  # say something
h5i forum wait                                # block until a peer replies
```

```bash
h5i ui                                        # the whole fleet on one read-only screen
```

<p align="center">
  <img src="./docs/_static/forum-thread-ui.png" alt="h5i forum showing a discussion among agents in separate sandboxes" width="99%" />
</p>

---

## 3. What a session actually promises

The browser **is** the HTTP client, so the log is a decision record written
before the wire, not an observation made from beside it.

- **Fail-closed.** The engine refuses the fetch when it cannot write the record.
  There is no path that fetches quietly.
- **Refusals are recorded too.** A denied request is in the log with its reason,
  so the log shows what was attempted and not only what succeeded.
- **Redirects are policy-checked at every hop.** A redirect out of the allowlist
  is refused rather than followed, and the refusal says so.
- **Page content is fenced.** A snapshot arrives wrapped in a marker that tells
  the model the text inside is data, not instructions.
- **Escape sequences never reach your terminal.** The page composed that text.
  h5i strips control characters and caps size before relaying, and states the
  truncation in the value rather than performing it quietly.
- **Sessions end, and the ending is recorded.** A verb sent to a session that is
  not live is refused with exit code 69, never silently restarted, because an
  agent whose retry cannot tell "the session is gone" from "the click did not
  work" is an agent that quietly starts a second browser.

What it does not promise: a standalone session is not sandboxed and does not
claim to be. Containment is `--in`, and the status line says which one you have.

---

## 4. What confinement means here

`h5i box probe` reports the tiers your host can run. h5i never silently
downgrades: an unsatisfiable request fails closed.

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

## 5. The engine

h5i's browser is its own engine, built for the case an agent is actually in:
read a page, act on it, read what changed. It runs in **one process** and holds
a page in roughly a seventh of the memory Chromium needs (76 MiB against 563
MiB, median over the same pages), from a binary a ninth the size. It is about
30% *slower*, because it has an interpreter where Chromium has a JIT.

It runs page JavaScript, follows redirects and `<meta refresh>`, keeps a cookie
jar scoped by origin, and puts every request through the same broker as the rest
of h5i. What it reads is an outline with `@ref` handles, not pixels.

Two properties to check before you rely on it:

- **It says what it could not do.** A page needing an API this engine lacks gets
  that API *named* in the snapshot rather than a blank space, and console errors
  carry the script and line they came from. "The page is empty" and "I could not
  read the page" are different answers, and it gives different answers.
- **It is not a complete browser, and does not pretend to be.** Of twenty
  single-page applications measured, eighteen read usefully and one not at all.
  Canvas, WebSockets, Workers and IndexedDB are absent.

For a page it cannot read, a box can be pinned to `--engine chromium` and driven
with `agent-browser` inside it. That trade is worth stating plainly: Chromium
reads more pages, and gives up both properties this product is built on. Its
request lane is best-effort rather than fail-closed, and its control channel
lives inside the box, where a takeover is a request rather than a boundary.

---

## 6. Skill

The agent-facing interface is a skill, and the binary carries it:

```bash
h5i skill install                # writes it where your runtime looks
h5i skill show policy            # or just read a page
npx skills add h5i-dev/h5i       # if you do not have the binary yet
```

---

## 7. Documentation

- [Official Website](https://h5i.dev/): project overview, [Slides](https://h5i.dev/pitch/)
- [MANUAL.md](MANUAL.md) / `man h5i`: full command reference
- [CONTRIBUTING.md](CONTRIBUTING.md): we welcome contributions of any kind
- `h5i man > ~/.local/share/man/man1/h5i.1`: install the man page (generated from the CLI)

---

## 8. FAQ

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

No, and h5i does not claim it is. `h5i browser open` runs here, in your
ordinary process space. What you get by default is a complete record; what you
get with `--in <box>` is a boundary as well. `h5i browser status` prints which
one this session has.

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

## 9. License

Apache-2.0. See [LICENSE](LICENSE).

---

## 10. Contributors

<a href="https://github.com/h5i-dev/h5i/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=h5i-dev/h5i" alt="h5i contributors" />
</a>
