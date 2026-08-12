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

<h1 align="center">Integrated Sandbox for AI Coding Agents</h1>

**h5i** (pronounced *high-five*) gives coding agents a complete, disposable
development environment inside a single security boundary. The agent,
workspace, shell, dependencies, dev server, and browser all run together
inside the sandbox, while your host files and credentials stay outside. 
You can securely share web apps running inside the sandbox with others over
an end-to-end encrypted P2P connection or a browser-ready demo link.
When the work is done, h5i exports a reviewable patch and execution logs.

h5i gives you:

- **A self-contained sandbox with multiple isolation tiers** for the agent, toolchain, dependencies, and browser
  - **Lightweight OS-level isolation** that starts in under 200 ms, with filesystem, syscall, and network controls
  - **Rootless containers** for portable, image-based environments
  - **MicroVM isolation** with a separate kernel when stronger boundaries matter
- **Isolated browsers** that agents can securely control from inside the sandbox
  - **Chromium** for broad compatibility with modern web applications
  - **h5i-browser-light**, a pure-Rust, single-process engine using 7.4× less peak memory than Chromium
- **Securely share dev servers** running inside local sandboxes over the internet
  - **End-to-end encrypted P2P sharing** when both sides use h5i
  - **Browser-ready demo links** for everyone else, with expiring grants, revocation, and ingress receipts
- **Reviewable patches and execution logs** showing what changed, what ran, and what was denied

**Local-first. No hosted sandbox. No SaaS account required.**

<a href="https://trendshift.io/repositories/46160?utm_source=trendshift-badge&amp;utm_medium=badge&amp;utm_campaign=badge-trendshift-46160" target="_blank" rel="noopener noreferrer"><img src="https://trendshift.io/api/badge/trendshift/repositories/46160/daily?language=Rust" alt="h5i-dev%2Fh5i | Trendshift" width="250" height="55"/></a>

---

## Install

```bash
curl -fsSL https://h5i.dev/install.sh | sh
# if you would rather not add a domain to the chain:
# curl -fsSL https://raw.githubusercontent.com/h5i-dev/h5i/main/install.sh | sh
```

Or build from source:

```bash
cargo install --path .
```

Two optional runtimes add tiers on top of either: rootless [Podman](https://podman.io/) gives 
you `container`, and [microsandbox](https://microsandbox.dev) (`msb`) 
gives you `microvm` on a host with hardware virtualization (`/dev/kvm` 
on Linux, Apple Silicon on macOS).

---

## Use it

- **Create a sandboxed**

```bash
h5i box create <name> --profile agent-claude            # a sandboxed Git worktree from this repository
h5i box create <name> --profile agent-claude --pr 1234  # a sandbox from pull request #1234
```

- **Run a single command**

```bash
h5i box run <name> -- cargo test # one command; the exit code passes through
```

- **Work in it interactively**

```bash
h5i box shell <name>             # an interactive confined session
                                 # every command is policy-enforced and recorded:
```

- **Watch the browser it drives**

```bash
h5i box view <name>              # the box's page, on a forward only your host can reach
h5i box view <name> --term       # draw it in this terminal instead (needs kitty)
```

<p align="center">
  <img src="./docs/_static/browser-demo.gif" width="99%" />
</p>

- **Review the work, then take it**

```bash
h5i box propose <name>           # freeze the worktree into a reviewable snapshot
h5i box apply   <name>           # merge that snapshot onto the parent branch
```

- **Share the web app running inside the box**

```bash
h5i box share <name> --port 3000 # end-to-end encrypted P2P sharing
h5i box share <name> --port 3000 --tunnel # browser-ready demo link

# For P2P sharing, the recipient connects with the generated ticket:
h5i join <ticket>
```

- **Keep the record of what happened**

```bash
h5i box export <name>            # freeze the box and write a bundle you can read
# → h5i-export/<name>/patch.diff    the change, path-validated
#   h5i-export/<name>/report.md     what ran, what was denied, what was redacted
#   h5i-export/<name>/receipt.json  the records, with the enforced policy digest
```

- **See where your boxes stand**

```bash
h5i box ls                       # every box on this clone, and how far each has drifted
h5i box status <name>            # the policy that was actually enforced
h5i box diff <name>              # what changed against the pinned base
```

- **Throw a box away**

```bash
h5i box rm <name>                # prune the worktree, delete its branches, erase its manifest
```

- **Watch the whole fleet in a browser**

```bash
h5i ui                           # the whole fleet on one screen, read-only
```

<p align="center">
  <img src="./docs/_static/sandbox-ui-demo.png" width="99%" />
</p>

<p align="center">
  <img src="./docs/_static/sandboxed-browser-ui.png" width="99%" />
</p>


---

## What confinement means here

`h5i box probe` reports the tiers your host can run. h5i never silently
downgrades: an unsatisfiable request fails closed.

| Tier | What enforces it |
| --- | --- |
| `workspace` | a separate git worktree, no confinement |
| `process` | Landlock filesystem allowlist, seccomp deny-list, namespaces, rlimits |
| `supervised` | all of the above, plus a private network namespace with an **nftables egress allowlist pinned to resolved IPs**, DNS pinned by hosts file, and a seccomp-notify socket gate |
| `container` | rootless Podman, read-only rootfs, dropped capabilities, a portable image, and an HTTP/HTTPS proxy allowlist |
| `microvm` | a hardware-isolated guest with **its own kernel**, booted by [microsandbox](https://microsandbox.dev) (`msb`) from the same OCI images, with the egress allowlist evaluated **by the VM's network stack** |

microvm is the strongest tier and the only one that does not share the host kernel. It requires msb, hardware virtualization (`/dev/kvm` or Apple Silicon), and an image; otherwise, it is refused, never downgraded.

No credentials enter a box. A runtime-scoped host proxy injects model API keys outside the boundary, preventing cross-runtime access. Each box receives a one-time copy of HOME state that is never written back.

---

## Skill

The agent-facing interface is a skill, and the binary carries it:

```bash
h5i skill install                # writes it where your runtime looks
h5i skill show policy            # or just read a page
npx skills add h5i-dev/h5i       # if you do not have the binary yet
```

---

## Documentation

- [ROADMAP.md](ROADMAP.md): where this is going, and what was cut to get there
- [Official Website](https://h5i.dev/): project overview, [Slides](https://h5i.dev/pitch/)
- [MANUAL.md](MANUAL.md) / `man h5i`: full command reference
- [CONTRIBUTING.md](CONTRIBUTING.md): we welcome contributions of any kind
- `h5i man > ~/.local/share/man/man1/h5i.1`: install the man page (generated from the CLI)

---

## What h5i does not claim

- **It cannot stop an agent from putting your code in a prompt.** Containment
  keeps the agent off your machine. Model egress is a separate control.
- **The kernel is shared, below `microvm`.** Podman and the kernel tiers are
  good against a runaway agent and careless dependency code, not against a
  targeted kernel exploit. `isolation=microvm` is the answer to that, and it
  needs a host with virtualization and an image — so it is opt-in, not the
  default you get by typing `h5i box`.
- **The container tier's egress scoping is L7.** Its allowlist is a proxy, so
  it binds proxy-respecting tooling only. `supervised` and `microvm` enforce at
  L3/L4 and do not have that hole.

---

## License

Apache-2.0. See [LICENSE](LICENSE).

---

## Contributors

<a href="https://github.com/h5i-dev/h5i/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=h5i-dev/h5i" />
</a>

