# Security policy

h5i is a browser an agent drives and a human can audit. The engine is the HTTP
client, so every request is policy-checked and written to the session log before
the bytes move, and a fetch that cannot be recorded is refused. A box is the
second boundary, taken on request: the code, the toolchain and the session
itself run inside it, and work leaves through an export a human reviewed.

That makes most of h5i security-relevant, because the product is a boundary plus
an honest account of where it stops. The Limits section of `docs/MANUAL.md` is
the user-facing companion to this file and is specific per tier and per
platform. Read both.

## Supported versions

Fixes land on `main` first, and ship as a patch release when the backport is
clean. Older releases get nothing automatically. Track `main` or the newest
release if you package h5i downstream.

## Reporting a vulnerability

Report privately first. Use GitHub's private vulnerability reporting for this
repository. If that is unavailable, open a minimal public issue asking for a
private contact path, with no exploit details, secrets, logs or reproduction
archives in it.

Include what you have:

- h5i version or commit, and whether the build used default features.
- OS, architecture and kernel. For sandbox issues this outweighs everything
  else, so paste `h5i box probe`.
- The isolation tier and profile, the exact command line, and the
  `.h5i/env.toml` in play.
- The boundary you expected, and how it was crossed.
- Reproduction against a throwaway repository and fake credentials.
- What the attack needs: a hostile page in the session, a hostile repository, a
  prompt-injected agent, local shell access on the host, or a compromised
  dependency.

Do not send real credentials, private prompts, proprietary source or full agent
logs unless a maintainer asks for a redacted sample.

## What counts as security-sensitive

- The fetch path: policy resolution per request, the record-before-wire
  invariant, cookie and origin scoping, per-navigation budgets, and the redirect
  and CORS rules.
- Everything a session relays. Page text is attacker-composed, so escape
  sequences, control characters and unbounded structures are stripped or capped
  before any of it reaches a terminal, a model or the console.
- Evidence labeling. `engine-claimed`, `host-observed`, `box-claimed` and
  `kernel-observed` are separate lanes and must never be merged or averaged into
  a score.
- Isolation enforcement and tier resolution: Landlock, seccomp, namespaces, the
  seccomp-notify supervisor, cgroups, Seatbelt, the Podman and microVM backends,
  and the code that decides what this host can actually run.
- Downgrade behavior. Anything that can turn a requested claim into a weaker one
  still reported as satisfied.
- Egress: the nftables allowlist and pinned DNS at `supervised`, the CONNECT
  proxy at `container`, the netstack rules at `microvm`, and the host allowlist
  under `~/.config/h5i/`, which merges only into a profile that already sets
  `net.egress` and never widens a deny-all one.
- Credentials: the auth proxy (`crates/h5i-sandbox/src/auth_proxy.rs`), the
  secrets broker (`secrets_broker.rs`), the websec capture store, and the
  scanner and redaction that run before anything is written down.
- The output gate (`h5i box export` / `apply`): the canonicalized `$WORK`
  allowlist, nested `.git` rejection, symlink escape rejection, gitlink round
  trip.
- Browser control mediation (`crates/h5i-core/src/browser_proxy.rs`), the
  control lock, the viewer socket, and `h5i box share` tickets.
- Plugin resolution and installation, console request handling and its
  per-session token, shell quoting and generated in-box configuration, and
  release packaging and install scripts.
- Parsing of anything a box or a page produced: receipts, manifests, command
  output, page content, file paths.

Treat every byte out of a box or off a page as untrusted input, including a
box's own manifest and resolved policy read back from disk.

## The claims

- h5i never claims isolation it did not enforce. Everything else is subordinate
  to this. Tier guarantees are reported per host and per platform.
- Explicit claims fail closed. An `--isolation` or profile tier the host cannot
  satisfy is a refusal, never a quiet downgrade. `auto` picks the strongest tier
  available and names it.
- A session with no `--in` is not contained, and every status line says so.
- What was enforced is recorded. The resolved policy is serialized and digested
  at box creation, and every receipt names the digest in force.
- The provider token stays in the host proxy's memory. The box sees a base URL
  and a dummy, and a Claude box never gets Codex's credentials or egress.
- Runtime detection observes and never denies. The eBPF collector carries no
  `bpf_send_signal`, no `bpf_override_return` and no LSM program, by
  construction. A `runtime` block in a receipt is not evidence that anything was
  stopped, and an empty detection list means the catalogue modeled nothing that
  happened, not that nothing happened.

## Where it stops

The Limits section of `docs/MANUAL.md` enumerates the residual risk per tier and
per platform, including terminal sharing, Chrome's own sandbox being off inside
a box, macOS loopback, and the resource caps macOS cannot hold. Four points
belong here rather than there:

- Containment does not stop an agent from sending your source to an allowed
  model API. That is a different control: a self-hosted model, or no model
  egress.
- The kernel and container tiers share the host kernel. They hold against a
  runaway agent and careless dependency code, not against a targeted kernel
  exploit. `isolation=microvm` is where the boundary is a hypervisor.
- Mediation is not containment against an evasive agent. The browser daemon runs
  inside the box, and a box has no internal privilege boundary, so a socket the
  daemon can bind the agent can reach directly. The same limit applies to
  `session login`, which withholds the reads that would put a typed credential
  into a snapshot while the live view keeps streaming.
- A user-writable install directory is a user-writable h5i. Homebrew on macOS is
  the common case. An `isolation=workspace` box shares your uid, so it can
  rewrite the binary that confines every other box, and a later `sudo h5i` runs
  that binary as root. `H5I_INSTALL_DIR=/opt/h5i/bin` closes it.

Secret detection and redaction are guards, not guarantees, and h5i does not
claim a hostile repository cannot exploit your editor, build tools or OS on the
host side of the boundary.

## Authorized use

`h5i websec` and `h5i recon` send traffic a target did not ask for. Use them
only against systems you own or have written permission to test. The tooling
keeps that boundary visible rather than policing it: capture is opt-in because
the store holds bodies and credentials in full, an experiment caps at 1000 sends
with a `--rate` ceiling on what the target sees, recon ships no wordlist and
generates no payloads, and a refusal is recorded as a fact about the scope
instead of retried. Do not add a verb that widens scope by default, and do not
make a policy refusal recoverable from inside a box.

## Isolation boundaries

The tiers are `workspace`, `process`, `supervised`, `container` and `microvm`.
They are not one ladder: `container` buys portability and an L7 egress proxy
while `supervised` enforces at L3/L4, so neither dominates. What review looks
for:

- The check against what the host can enforce must be functional. Landlock, user
  namespaces and seccomp being present does not mean a confined exec succeeds,
  since AppArmor or a hardened container can still refuse it. Capability bits are
  not evidence; the exec self-test is.
- A domain-scoped egress rule requires a tier that can inspect or mediate the
  traffic. One that cannot must fail closed rather than accept the rule and
  ignore it.
- Caps a platform cannot hold are reported as unenforced, not listed as applied.
- Linux and macOS are two mechanisms, not one abstraction. A guarantee proven on
  one says nothing about the other, and Seatbelt denials surface only in
  `log show`.

Changes to sandbox behavior need tests for the allowed path and the refusal
path. A bypass that turns a denied policy into a permitted operation is a
security bug.

## Credentials and secrets

Four mechanisms, with different properties.

The auth proxy lets a box authenticate upstream without holding the token. It
terminates the box's request on host loopback and re-originates it with the real
credential. Each guarantee holds something up: the upstream origin is pinned at
spawn and re-checked after assembly, since a request target not beginning with
`/` would extend the authority rather than the path; DNS is pinned once against
rebinding; the listener is loopback-bound behind a shared secret. Changes here
need adversarial tests.

The secrets broker resolves declared grants at run time, never at policy load,
and injects them scoped and audited. A profile is part of the repository, so two
sources need something the repository cannot write: a `command:` extractor also
needs `H5I_ALLOW_COMMAND_EXTRACTORS=1` on the host, and a `file:` source may not
point inside the profile's own `fs.deny`. An `[[auth]]` destination must be a
bare hostname and is announced every run. The record keeps the grant id, source,
injection method, TTL and a value fingerprint, never a value, and never in the
policy, the manifest or a git ref. File-injected secrets are written `0600`
outside `$WORK` and unlinked at the end. An unresolvable grant aborts the run.

The capture store holds whole requests and responses, credentials included. It
exists only under `--capture`, and enters an export only when named.

The scanner covers common credential formats and high-entropy assignments near
credential-like keywords, and feeds redaction of captured output. Do not weaken
its rules to quiet local noise without replacement coverage or a precise
allowlist.

Contributors: fake tokens everywhere (`sk-example-not-real`), no real
environment values in captured output, redact before sharing a receipt outside
the boundary it was made in, and rotate anything that lands in a commit, issue,
pull request, log archive or screenshot.

## Plugins, console, supply chain

A plugin is a separate executable, installed deliberately, under a name h5i
already knows, into h5i's own state directory rather than `$PATH`. It holds no
privilege of its own: it reaches a session through the verbs a person types, so
its requests are the engine's, checked by the engine's policy and written into
the engine's receipts. Keep it that way. A plugin that talks to the network or
the session store directly is a hole.

`h5i ui` binds loopback, serves GET only, and keeps lifecycle verbs in the CLI,
so the console can watch boxes but never drive them. Access needs a per-session
token held in memory; `--open` hands the browser a separate single-use token,
because a URL on a command line is readable by every uid on the machine. Badges
are arithmetic over receipts and never a score. Binding elsewhere, adding a
mutating route, serving files from a worktree, or rendering box output are all
security-sensitive.

Dependency updates reach TLS, HTTP, Git, parsing, sandboxing, release artifacts
and the embedded console. Keep them focused, run the checks with `--locked`, and
review transitive changes in those areas. `web/package-lock.json` is a
supply-chain artifact: CI verifies it covers every release platform and the
build uses `npm ci`. `install.sh` is published at `h5i.dev/install.sh` and from
`raw.githubusercontent.com`. CI fails if the bytes diverge, but the trust chains
differ, and the repository URL is the one that depends only on GitHub.

## Before merging

- The change fails closed on unsupported or ambiguous states.
- A guarantee that holds at some tiers or on one platform says so, in code and
  in docs.
- Enforcement is verified functionally, not inferred from capability bits.
- Box- and page-controlled text is sanitized before display.
- Paths are canonicalized before filesystem access; refs, object ids, branch and
  profile names are validated before use.
- Commands are spawned with structured argv, not assembled strings.
- Tests cover malformed and malicious input, and receipts leak no avoidable
  secrets.
- The Limits section of `docs/MANUAL.md` still describes the boundary.

Run what CI runs:

```bash
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo build  --locked --workspace --all-targets
cargo test   --locked --workspace
```

Console or release-path changes also need the Node build path. Detection-lane
changes need the probe compiled, which the default build leaves out, so run the
`bpf` feature with `H5I_BPF_REQUIRE=1` and, on a host with the capability, the
live attach under `H5I_BPF_LIVE=1`. That attach is the one path CI cannot
exercise.

## Disclosure

Maintainers acknowledge private reports as soon as practical, triage affected
versions, prepare the fix on a private or minimal branch where that matters, and
publish once users have an upgrade path. Fixes ship with regression tests, unless
the test would publish a working exploit before users can update. In that case
the test follows the release.
