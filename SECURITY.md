# Security Policy

h5i is a local-first developer tool that gives a coding agent a disposable,
confined development box. The agent, the workspace, the shell, the toolchain,
the dev server and the browser run inside one boundary; your host files,
credentials and network stay outside it. Work leaves the box as a reviewable
patch plus a receipt of what ran.

That makes almost all of h5i security-relevant: it exists to enforce a boundary
and to describe that boundary honestly. This document covers how to report
vulnerabilities, what the project treats as security-sensitive, and where the
current protections stop.

`MANUAL.md`'s Limits section is the user-facing companion to this file, and it
is deliberately specific about what each tier and each platform does not do.
Read it alongside this one.

## Supported versions

Security fixes target the current `main` branch first. If a vulnerability
affects a published release, maintainers may publish a patch release when the
fix backports cleanly.

Older releases are not guaranteed to receive fixes. If you maintain downstream
packages or long-lived internal builds, track `main` or the newest release and
watch release notes.

## Reporting a vulnerability

Report suspected vulnerabilities privately first.

- Use GitHub's private vulnerability reporting for this repository when
  available.
- Otherwise, open a minimal public issue saying you have a security report and
  need a private contact path. Keep exploit details, secrets, private logs and
  reproduction archives out of that issue.

Include what you can of:

- Affected h5i version or commit.
- Operating system, architecture, and kernel version. For sandbox issues this
  matters more than anything else, so include the output of `h5i box probe`.
- The isolation tier and profile involved, and whether the build used default
  features.
- Exact command line, `.h5i/env.toml` profile, or workflow.
- The boundary you expected and how it was crossed.
- Minimal reproduction steps using throwaway repositories and fake credentials.
- Whether the issue needs malicious repository contents, malicious agent
  behavior, a malicious page loaded in the box's browser, local shell access on
  the host, or a compromised dependency.

Do not send real credentials, private prompts, proprietary source, or full
agent logs unless a maintainer asks for a redacted sample.

## What counts as security-sensitive

- Isolation enforcement and tier resolution: Landlock, seccomp, namespaces, the
  seccomp-notify supervisor, cgroups, Seatbelt, the Podman and microVM
  backends, and the code that decides which of them a host can actually run.
- Downgrade behavior: anything that could turn a requested claim into a weaker
  one that is still reported as satisfied.
- Policy resolution: parsing `.h5i/env.toml`, merging profiles, resolving
  filesystem grants, network mode and egress rules, and digesting the resolved
  policy.
- Egress paths: the nftables allowlist and pinned DNS at `supervised`, the
  CONNECT proxy at `container`, the netstack rules at `microvm`, and the
  host-side allowlist under `~/.config/h5i/`, which merges into a profile that
  already sets `net.egress` and never widens a deny-all one.
- The credential-injecting auth proxy
  (`crates/h5i-sandbox/src/auth_proxy.rs`), which terminates the box's API
  request on host loopback and re-originates it upstream with the real token.
  Drop any one of its origin pinning, request-target validation, DNS pinning or
  loopback gate and the token is reachable.
- The secrets broker (`secrets_broker.rs`), which resolves grants at run time
  and injects them into the box.
- Secret scanning and redaction, before anything reaches a receipt, a log, or
  the console.
- The export and apply gate, which decides that box output may touch your
  repository: the `$WORK` allowlist, nested `.git` rejection, symlink escape
  rejection and gitlink round-trip live there.
- Browser control mediation and the browser's own egress path.
- Console request handling, the per-session token, and the embedded assets.
- Shell quoting, command wrapping and generated configuration written into a
  box.
- Parsing of anything produced inside a box: receipts, manifests, command
  output, page content, file paths.
- Release packaging and install scripts.

Treat every byte that comes out of a box as untrusted input, including the
box's own manifest and resolved policy read back from disk.

## Security model

h5i is not a hosted sandbox service. It runs on your machine, and it confines
processes you started, on a boundary the host kernel is willing to enforce.

The claims:

- h5i never claims isolation it did not enforce. This is the central promise;
  everything else is subordinate to it. A tier's guarantees are reported per
  host and per platform, not asserted globally.
- Explicit claims fail closed. An explicit `--isolation` or profile tier the
  host cannot satisfy is a refusal, never a silent downgrade. `auto` picks the
  strongest tier the host can run and says which.
- What was enforced is recorded. The resolved policy is serialized and digested
  at box creation, and every receipt names the digest in force, so "what the
  policy was" is not a matter of trust.
- Evidence is labeled by who observed it. A denial recorded by the egress proxy
  is host-observed; a command the box reported is box-claimed; a run an eBPF
  collector watched from the kernel is kernel-observed. h5i keeps those apart
  wherever they are shown, and never averages them into a score.
- Credentials stay out of the box where the design allows it. The provider
  token lives in the host proxy's memory and the box sees a base URL and a
  dummy.
- Runtime scoping is a boundary, not cosmetics. A Claude box does not get
  Codex's credentials or egress to OpenAI, because a prompt-injected agent
  could otherwise use the other runtime's token against an allowlisted host.

The non-goals, which matter just as much:

- h5i does not stop the agent from sending your source to the model API.
  Containment stops the agent from touching the host. Putting private code in a
  prompt is a different control (a self-hosted model, or no model egress), and
  h5i will not imply otherwise.
- The kernel tiers and the container tier share the host kernel. They are good
  against a runaway agent and careless dependency code. They are not a claim
  against a targeted kernel exploit. `isolation=microvm` is the tier where the
  boundary is a hypervisor instead.
- The container tier's egress scoping is L7. Its allowlist is a proxy, so it
  binds proxy-respecting tooling only. `supervised` and `microvm` enforce at
  L3/L4.
- An interactive `box shell` at a kernel tier shares your terminal, which is a
  two-way device. `MANUAL.md`'s Limits enumerates the residual, including TTY
  input injection, whose availability is a property of your kernel on Linux and
  of the Seatbelt profile on macOS. h5i measures it with `h5i box probe` rather
  than claiming it.
- Browser control mediation is not containment against an evasive agent. The
  daemon runs inside the box, and inside a box there is no privilege boundary,
  so a socket the daemon can bind the agent can also reach directly if it goes
  looking. The mediator enforces against an agent following the documented
  path, which is the threat the control lock was written for. See the module
  docs in `crates/h5i-core/src/browser_proxy.rs`.
- A user-writable install directory is a user-writable h5i. The installer puts
  the binary in `/usr/local/bin` by default and uses `sudo install -o root`
  when it has to. Where that directory is already writable by you (Homebrew on
  macOS is the common case), the file's owner and mode do not matter: anything
  running as your uid can replace or unlink it. An agent in an
  `isolation=workspace` box shares that uid by design, so it can rewrite the
  binary that enforces every *other* box's confinement, and a later `sudo h5i`
  would run it as root. The installer says so at install time; putting h5i
  somewhere root-owned (`H5I_INSTALL_DIR=/opt/h5i/bin`) is what closes it.
- `session login` withholds reads, not frames. The mode refuses every control
  verb that reads the page while a human types a credential, which is what
  stops the credential landing in a snapshot the agent asked for. The live view
  keeps streaming, because the person typing has to see the page, and the
  viewer socket is inside the box. So this is the same structural limit as the
  bullet above: an agent that attaches to the viewer socket watches the same
  pixels. ROADMAP §5.10 specifies withholding both; only the read half exists.
- Chrome inside a box runs with its own sandbox off, because the seccomp
  deny-list blocks the namespace syscalls it needs. The box is the boundary;
  Chrome's own is one layer you do not have.
- Runtime detection observes; it never denies. The eBPF collector
  (`[profile.X.detect]`, ROADMAP.md D1–D14) reports what a box's processes did.
  It contains nothing, and it is built so that it cannot: no
  `bpf_send_signal`, no `bpf_override_return`, no LSM program anywhere in it.
  Confinement stays with Landlock, seccomp, the network namespace and the
  egress proxy. A `runtime` block in a receipt is never evidence that something
  was stopped.
- A detection list is a lower bound, not a verdict. A signature only fires on
  what it models, and `h5i box detect rules` is a finite list. A run with no
  detections means "nothing the catalogue models happened", never "nothing
  happened". Where the record can be more precise it is: the block carries the
  event count, the number of events dropped, how much of the tier the scope
  covered, and, when the probe could not attach at all, the reason. An
  unwatched run can never be read as a quiet one.
- The collector's evidence is caller-supplied strings, seen going in. Paths and
  command lines come from syscall arguments captured at `sys_enter`: not the
  kernel's resolution of those strings, and the probe sees the attempt rather
  than the outcome. A `connect` a network namespace refused looks exactly like
  one that succeeded.
- Running the collector needs capabilities h5i does not otherwise want.
  `CAP_BPF` and `CAP_PERFMON` are what loading a program and attaching it to a
  tracepoint cost, and `setcap`-ing the h5i binary grants them to *all* of h5i,
  not just to the collector. h5i never grants them to itself, never invokes
  `sudo`, and prints the command rather than running it, so the decision stays
  yours. A privilege-separated collector, a small setcap'd helper that owns the
  probe and streams events over a socket, is the right long-term shape and is
  not built (ROADMAP.md D13.1).
- h5i does not guarantee that all secrets are detected, nor complete redaction
  of prompts, transcripts, or command output.
- h5i does not guarantee a malicious repository cannot exploit your editor,
  shell, build tools, compiler, dependency scripts, or operating system on the
  host side of the boundary.

## Isolation boundaries

The tiers are `workspace`, `process`, `supervised`, `container` and `microvm`.
They are not a single ladder: `container` buys portability and an L7 egress
proxy, while `supervised` enforces egress at L3/L4, so neither strictly
dominates the other. `MANUAL.md` documents what each one grants. The
security-relevant expectations:

- A requested claim must be checked against what the host can actually enforce,
  and the check must be functional. Landlock, user namespaces and seccomp being
  present in the kernel does not mean a confined exec succeeds: a hardened
  container or an AppArmor policy can still refuse it, so capability bits are
  not evidence and the exec self-test is.
- Missing kernel support, disabled user namespaces, unusable Seatbelt or an
  unavailable runtime must produce an explicit refusal, not a quiet downgrade.
- A domain-scoped egress policy requires enforcement that can inspect or
  mediate the traffic. A tier that cannot must fail closed rather than accept
  the rule and ignore it.
- Resource caps a platform cannot hold must be reported as unenforced rather
  than listed as applied. macOS has no cgroups, does not enforce `RLIMIT_AS`
  against a modern runtime's mmap'd heap, and scopes `RLIMIT_NPROC` to the
  whole user, so `mem` and `procs` are marked rather than claimed at the
  `process` and `supervised` tiers there.
- Linux and macOS are two different mechanisms, not one abstraction with a
  porting layer. A guarantee proven on one says nothing about the other, and
  Seatbelt denials surface only in `log show`, so a macOS change needs to be
  verified there specifically.

When changing sandbox behavior, include tests for both the allowed path and the
refusal path. A bypass that turns a denied policy into a permitted operation is
a security bug.

## Credentials and secrets

Three separate mechanisms, with different properties.

The auth proxy lets an agent box authenticate to its provider without the
long-lived token entering the box. It terminates the box's request in cleartext
on host loopback and re-originates it upstream over TLS with the real
credential injected. Its guarantees fail closed, and each one is holding
something up: the upstream origin is pinned at spawn and re-checked after
assembly (a request target that does not begin with `/` would otherwise extend
the authority rather than the path), DNS is pinned once to resist rebinding,
and the listener is loopback-bound behind a shared secret. Changes here need
adversarial tests, not functional ones.

The secrets broker resolves a profile's declared grants from host-side sources
at run time, never at policy load, and injects them capability-scoped and
audited. It records only the grant id, source, injection method, TTL and a
value fingerprint. It never writes a value into the policy, the manifest, or a
git ref. File-injected secrets are written `0600` outside `$WORK` and unlinked
when the run ends. A declared grant that cannot be resolved aborts the run
rather than running with the credential silently absent.

The secret scanner covers common credential formats and high-entropy
assignments near credential-like keywords, and feeds redaction of captured
output. It is a guard, not a guarantee.

Contributors should:

- Use fake tokens in tests, examples, documentation, screenshots and fixtures,
  with obvious placeholders such as `H5I_EXAMPLE_TOKEN` or
  `sk-example-not-real`.
- Avoid recording real environment values in captured output.
- Redact receipts and logs before sharing them outside the trust boundary where
  they were created.
- Rotate any credential that appears in a commit, a receipt, an issue, a pull
  request, a log archive, or a screenshot.

Do not weaken scanner rules to reduce local noise without adding replacement
coverage or a precise allowlist.

## Box state and output

h5i keeps durable state under the Git common directory (`.git/.h5i/`) and a box
event log in `refs/h5i/env`. This is local state, not a sharing mechanism: h5i
no longer pushes, pulls, or merges any of it between clones.

The path that matters for security is the one out of the box. `h5i box export`
and `h5i box apply` are the output gate: a canonicalized `$WORK` allowlist,
rejection of nested `.git` directories and symlink escapes, and a gitlink round
trip. Everything crossing that gate is agent-produced and must be reviewed as
such. A receipt is evidence about a box, not an endorsement of what it did.

## The console

`h5i ui` serves a read-only screen over the fleet. Its current properties are
the security design, not incidental:

- It binds loopback only.
- Every route is a GET. Lifecycle verbs (`shell`, `run`, `export`, `apply`)
  stay in the CLI, so the console can watch boxes but never drive them.
- Access needs a per-session token, held in memory and never written to disk.
  The page drops it from the address bar as soon as the cookie is set.
- Its badges are arithmetic over receipts. Nothing on the screen is a score.

Security-sensitive console changes include binding to a non-loopback interface,
adding a route that mutates anything, serving files from a worktree or the
sidecar directory, adding session or token handling, and rendering untrusted
box output. Prefer loopback-only defaults, explicit opt-in for broader
exposure, safe content types, and escaping untrusted display text.

## Dependencies and supply chain

Dependency updates can affect the CLI, Git operations, TLS and HTTP client
behavior, sandboxing, parsing, release artifacts and the embedded console.

For dependency changes:

- Keep them focused and explain why the update is needed.
- Run the normal Rust checks with `--locked`.
- For release-target or platform-sensitive changes, run or explain the relevant
  cross-target checks.
- Review transitive changes that touch TLS, Git, archive handling, process
  spawning, web serving, or sandboxing.

`web/package-lock.json` is a supply-chain artifact as well as a build one. CI
verifies it covers every release platform, and the build script uses `npm ci`
so an ordinary build cannot rewrite it.

Install scripts and release workflows are security-sensitive. Changes there
should be small, reviewed carefully, and tested from a clean checkout where
possible.

`install.sh` is published twice, at `h5i.dev/install.sh` and from the
repository at `raw.githubusercontent.com`. They are the same bytes and CI fails
if they diverge, but they do not carry the same trust chain: the first also
depends on the `h5i.dev` domain and its Pages deployment, the second only on
GitHub. Anyone who would rather not add the domain should use the repository
URL, and both are documented for that reason.

## Secure development checklist

Before merging security-sensitive code, verify that:

- The change fails closed on unsupported or ambiguous states.
- A guarantee that holds only at some tiers or on one platform says so, in the
  code and in the docs.
- Enforcement is verified functionally, not inferred from capability bits.
- Box-controlled text is sanitized before terminal or console display.
- Paths are canonicalized or constrained before filesystem access.
- Git refs, object ids, branch names and profile names are validated before
  use.
- Commands are spawned with structured argv rather than assembled strings.
- Tests cover malicious and malformed input, not only the happy path.
- Receipts and logs do not leak avoidable secrets.
- `MANUAL.md`'s Limits still describes the boundary after your change.

Run at least:

```bash
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo build  --locked --workspace --all-targets
cargo test   --locked --workspace
```

If your change touches the console or the release build path, also verify the
Node build path used by CI and release packaging.

If it touches the runtime-detection lane, run it with the probe actually
compiled, since the default build leaves it out:

```bash
H5I_BPF_REQUIRE=1 cargo clippy --locked --workspace --all-targets --features bpf -- -D warnings
H5I_BPF_REQUIRE=1 cargo test   --locked --features bpf --test detect_integration
```

And, on a host where you have the capability, the live attach, which is the one
path CI cannot exercise:

```bash
sudo -E env "PATH=$PATH" H5I_BPF_LIVE=1 \
    cargo test -p h5i-bpf --test live_attach -- --nocapture
```

## Disclosure process

Maintainers should acknowledge private reports as soon as practical, triage the
affected versions and impact, prepare a fix on a private or minimal public
branch when appropriate, and publish a release or advisory once users have a
clear upgrade path.

Security fixes should include regression tests, unless that would publish a
weaponized exploit before users can update. In that case, add a focused test
after the fix ships.
