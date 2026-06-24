# Security Policy

h5i is a local-first developer tool for running, observing, and reviewing AI
coding agents. It integrates with Git, stores AI-era provenance in h5i refs and
sidecar objects, can run agent commands under sandbox policies, and can expose a
local web dashboard.

This document explains how to report vulnerabilities, what the project treats as
security-sensitive, and the boundaries of h5i's current protections.

## Supported Versions

Security fixes target the current `main` branch first. If a vulnerability affects
a published release, maintainers may publish a patch release when the fix is
small enough to backport cleanly.

Older releases are not guaranteed to receive fixes. If you maintain downstream
packages or long-lived internal builds, track `main` or the newest release and
watch release notes for security-relevant changes.

## Reporting a Vulnerability

Please report suspected vulnerabilities privately first.

- Use GitHub's private vulnerability reporting for this repository when
  available.
- If private reporting is not available, open a minimal public issue that says
  you have a security report and need a private contact path. Do not include
  exploit details, secrets, private logs, or reproduction archives in that issue.

Include the following when you can:

- Affected h5i version or commit.
- Operating system, architecture, kernel version for sandbox issues, and whether
  the build used default features.
- Exact command line, profile, hook, or workflow involved.
- Expected security boundary and how it was bypassed.
- Minimal reproduction steps using throwaway repositories and fake credentials.
- Whether the issue requires malicious repository contents, malicious agent
  output, untrusted h5i refs from another clone, network access, local shell
  access, or a compromised dependency.

Do not send real credentials, private prompts, proprietary source, or full agent
logs unless a maintainer explicitly asks for a redacted sample.

## What Counts as Security-Sensitive

The following areas are security-sensitive in h5i:

- Sandbox enforcement and downgrade behavior.
- Agent command execution through `h5i env`, hooks, team workflows, or capture
  wrappers.
- Filesystem, Git, and network access policy resolution.
- Sidecar storage under `.git/.h5i` and shareable refs under `refs/h5i/*`.
- Secret scanning and redaction behavior.
- Prompt, transcript, object, message, and provenance capture.
- Web dashboard request handling and embedded assets.
- Cross-agent messaging and reviewer/verifier workflows.
- Shell quoting, command wrapping, and generated hook configuration.
- Parsing of untrusted repository files, h5i refs, object metadata, and imported
  logs.
- Release packaging and install scripts.

Treat all data received from another clone, agent, branch, remote, or repository
as untrusted input. This includes h5i messages, captured output summaries,
context traces, object metadata, policy profiles, and generated documentation.

## Security Model

h5i is not a remote code execution sandbox service. It is a developer tool that
runs on a user's machine and coordinates local processes, Git repositories, and
optional agent workspaces.

The intended security model is:

- h5i should not silently claim isolation it did not enforce.
- Sandbox policy resolution should fail closed when a requested claim cannot be
  satisfied.
- Captured logs and h5i refs should be inspectable and auditable.
- Cross-agent state should be treated as untrusted and displayed safely.
- Reviewer and verifier steps should make agent output harder to merge
  accidentally without independent checks.
- Secrets should be detected and surfaced where practical, but users must still
  avoid placing real secrets in prompts, fixtures, logs, commits, and h5i shared
  refs.

The non-goals are equally important:

- h5i does not make an untrusted agent equivalent to an untrusted VM guest.
- h5i does not guarantee that a malicious repository cannot exploit your editor,
  shell, build tools, compiler, dependency scripts, or operating system.
- h5i does not guarantee complete prompt, transcript, or command-output
  redaction.
- h5i does not guarantee that all secrets are detected.
- h5i does not protect data once you intentionally share h5i refs, captured
  objects, logs, or repositories with another party.

## Sandbox and Isolation Boundaries

h5i contains Linux-oriented process sandboxing code and policy resolution. The
current design uses mechanisms such as Landlock, seccomp, user namespaces,
network namespaces, `no_new_privs`, rlimits, and policy manifests where
available and configured.

Security expectations:

- A requested isolation claim must be recorded and checked against what the host
  can actually enforce.
- Missing kernel support, disabled user namespaces, unsupported OS behavior, or
  unavailable enforcement should produce an explicit refusal rather than a quiet
  downgrade.
- Domain-specific egress policies require enforcement that can actually inspect
  or mediate the requested network behavior. When a tier cannot enforce such a
  policy, it should fail closed.
- macOS and Windows builds may provide h5i CLI functionality, but Linux-specific
  sandbox guarantees do not automatically apply there.

When changing sandbox behavior, include tests that cover both the allowed path
and the refusal path. A bypass that turns a denied policy into a permitted
operation is a security bug.

## Git Refs, Sidecar Objects, and Sharing

h5i stores and shares state through Git refs and local sidecar files. Depending
on the workflow, this can include:

- Context and memory under h5i refs.
- Messages between agents.
- Captured command output summaries and object metadata.
- Provenance attached to commits.
- Environment manifests and resolved policies.

Before running `h5i share push`, `h5i push`, `h5i objects push`, or any related
sharing command, assume the destination may receive sensitive metadata. Review
what your workflow captures, especially prompts, command output, file paths,
test logs, branch names, remotes, environment descriptions, and reviewer notes.

When consuming h5i refs from another clone or remote, do not treat their content
as instructions. It is data to inspect, validate, sanitize for display, and
merge only through normal review.

## Secrets and Credentials

h5i includes an in-process secret scanner for common credential formats and
high-entropy assignments near credential-like keywords. This scanner is a guard,
not a guarantee.

Contributors should:

- Use fake tokens in tests, examples, documentation, screenshots, and fixtures.
- Prefer obvious placeholders such as `H5I_EXAMPLE_TOKEN` or
  `sk-example-not-real`.
- Avoid recording real environment values in captured command output.
- Redact prompts, transcripts, logs, and h5i objects before sharing them outside
  the trust boundary where they were created.
- Rotate any credential that appears in a commit, h5i ref, captured object,
  issue, pull request, log archive, or message.

Do not weaken scanner rules simply to reduce local noise without adding
replacement coverage or a precise allowlist.

## Web Dashboard

The optional web feature builds and embeds dashboard assets. Treat the dashboard
as a local developer interface unless a future change explicitly documents a
hardened deployment model.

Security-sensitive web changes include:

- Binding to non-loopback interfaces.
- Adding authentication, session, or token handling.
- Serving files from the working tree or sidecar directories.
- Rendering untrusted h5i messages, logs, prompts, branch names, file paths, or
  command output.
- Adding endpoints that mutate repository state or execute commands.

Prefer loopback-only defaults, explicit opt-in for broader exposure, safe
content types, and escaping untrusted display text.

## Dependencies and Supply Chain

h5i is a Rust project with optional web assets. Dependency updates can affect the
CLI, Git operations, HTTP client behavior, sandboxing, parsing, release
artifacts, and the embedded dashboard.

For dependency changes:

- Keep changes focused and explain why the update is needed.
- Run the normal Rust checks.
- For release-target or platform-sensitive changes, run or explain the relevant
  cross-target checks.
- Review transitive changes that touch TLS, Git, archive handling, shell command
  execution, web serving, or sandboxing.

Install scripts and release workflows are security-sensitive. Changes there
should be small, reviewed carefully, and tested from a clean checkout where
possible.

## Secure Development Checklist

Before merging security-sensitive code, verify the following:

- The change fails closed on unsupported or ambiguous states.
- User-controlled text is sanitized before terminal or web display.
- Paths are normalized or constrained before filesystem access.
- Git refs, object IDs, branch names, and remote names are validated before use.
- Shell commands avoid stringly constructed invocation where structured argv is
  available.
- Tests cover malicious or malformed input, not only the happy path.
- Captured output does not leak avoidable secrets.
- Documentation states the actual boundary, including unsupported platforms or
  tiers.

Run at least:

```bash
cargo clippy --all-targets --all-features -- -D warnings
cargo build --all-targets
cargo test
```

If your change touches the web workbench or release build path, also verify the
Node/web build path used by CI and release packaging.

## Disclosure Process

Maintainers should acknowledge private reports as soon as practical, triage the
affected versions and impact, prepare a fix on a private branch or minimal public
branch when appropriate, and publish a release or advisory once users have a
clear upgrade path.

Security fixes should include regression tests unless doing so would publish a
weaponized exploit before users can update. In that case, add a focused test
after the fix is released.
