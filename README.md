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

**h5i** (pronounced *high-five*) gives a coding agent full autonomy inside a
throwaway box, and gives your machine nothing to lose. The code, the toolchain,
the tests, the dev server and the agent itself run inside one boundary. Your
host directories are not mounted into it, your credentials never enter it, and
the only thing that comes out is a patch you reviewed, next to a receipt of
everything that ran.

> **The pivot in progress.** h5i began as a provenance system for AI coding
> work. It is being rebuilt around the boundary, which was always the part
> worth having. [ROADMAP.md](ROADMAP.md) is the plan of record: what stays,
> what was cut, and what is still coming.

<a href="https://trendshift.io/repositories/46160?utm_source=trendshift-badge&amp;utm_medium=badge&amp;utm_campaign=badge-trendshift-46160" target="_blank" rel="noopener noreferrer"><img src="https://trendshift.io/api/badge/trendshift/repositories/46160/daily?language=Rust" alt="h5i-dev%2Fh5i | Trendshift" width="250" height="55"/></a>

---

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/h5i-dev/h5i/main/install.sh | sh
```

Or build from source:

```bash
cargo install --path .
```

Linux and macOS. The two confine by different means: Linux uses Landlock,
seccomp and namespaces, macOS uses Seatbelt. What that buys you is close but not
identical, so run `h5i box probe`. It reports the mechanism your host actually
has and what it can enforce, rather than a tier name that means different things
in different places. Two optional runtimes add tiers on top of either: rootless
[Podman](https://podman.io/) gives you `container`, and
[microsandbox](https://microsandbox.dev) (`msb`) gives you `microvm` on a host
with hardware virtualization — `/dev/kvm` on Linux, Apple Silicon on macOS.

The gaps worth knowing before you pick a host: macOS has no per box memory or
process-count cap (Darwin has no cgroups, does not enforce `RLIMIT_AS` against
the mmap'd heap every modern runtime uses, and scopes `RLIMIT_NPROC` to the
whole user rather than to one box), and no syscall filter. `h5i box status`
marks a declared-but-unenforced limit with `*` rather than reporting it as
enforced. Use the `container` or `microvm` tier if you need any of those: both
cap memory and process count in the runtime itself.

## Use it

```bash
h5i box                          # a box from this repository
h5i box --pr 1234                # a box from pull request #1234
```

Work in it. Every command is policy-enforced and recorded:

```bash
h5i box run <name> -- cargo test # one command; the exit code passes through
h5i box shell <name>             # an interactive confined session
h5i box status <name>            # the policy that was actually enforced
h5i box diff <name>              # what changed against the pinned base
h5i ui                           # the whole fleet on one screen, read-only
```

<p align="center">
  <img src="./docs/_static/sandbox-ui-demo.png" width="99%" />
</p>

Get the work out through the gate, which is deliberately a human step:

```bash
h5i box export <name>
# → h5i-export/<name>/patch.diff    the change, path-validated
#   h5i-export/<name>/report.md     what ran, what was denied, what was redacted
#   h5i-export/<name>/receipt.json  the records, with the enforced policy digest
```

Nothing writes into your repository until you apply that patch.

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

No SSH keys, cloud credentials, or Docker socket enter a box. A runtime-scoped host proxy injects model API keys outside the boundary, preventing cross-runtime access. Each box receives a one-time copy of HOME state that is never written back.


## Skill

The agent-facing interface is a skill, and the binary carries it:

```bash
h5i skill install                # writes it where your runtime looks
h5i skill show policy            # or just read a page
npx skills add h5i-dev/h5i       # if you do not have the binary yet
```

## Documentation

- [ROADMAP.md](ROADMAP.md): where this is going, and what was cut to get there
- [Official Website](https://h5i.dev/): project overview, [Slides](https://h5i.dev/pitch/)
- [MANUAL.md](MANUAL.md) / `man h5i`: full command reference
- [CONTRIBUTING.md](CONTRIBUTING.md): we welcome contributions of any kind
- `h5i man > ~/.local/share/man/man1/h5i.1`: install the man page (generated from the CLI)

Parts of the documentation still describe the previous product. The roadmap
says which, and the rewrite is tracked there.

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

## License

Apache-2.0. See [LICENSE](LICENSE).
