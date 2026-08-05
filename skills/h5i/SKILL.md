---
name: h5i
description: Use when work should run inside a disposable, confined development box instead of on the host — reviewing a pull request or any untrusted or AI-generated code, letting an agent build and test with full autonomy, running a dev server and driving a real browser against it, and exporting the result as a reviewed patch with an execution receipt. Covers creating a box, running commands and interactive sessions in it, reading what a policy actually enforced, and the output gate.
---

# Driving h5i

A **box** is a disposable development environment: a git worktree on its own
branch, confined by a pinned, fail-closed policy. Code, toolchain, dev server
and agent run inside it. Nothing reaches the host except what you export.

`h5i dev <command> --help` is the authoritative flag reference and cannot go
stale. Reach for it before guessing at a flag.

## Are you inside a box?

`$H5I_ENV_ID` is set inside one. It changes what you should do:

- **Outside**: you create boxes, hand work to them, and read exports.
- **Inside**: you already have the whole worktree. Work normally. Some things
  are denied on purpose (see "When something is denied").

## From outside: make a box and use it

```bash
h5i dev                      # a box from this repository's HEAD
h5i dev 1234                 # a box from pull request #1234 (number, #n, or URL)
h5i dev --name fix-auth      # name it yourself; otherwise the branch name is used
```

Then work with it:

```bash
h5i dev ls                       # every box on this clone
h5i dev status <name>            # policy actually enforced, evidence, base drift
h5i dev run <name> -- cargo test # one command, policy-enforced, exit code passes through
h5i dev shell <name>             # interactive confined session (this is where an agent runs)
h5i dev diff <name>              # what changed against the pinned base
```

Every `run` is recorded as a **receipt**: the command, its exit code, wall/cpu/
rss, the egress verdicts, and the policy digest that was in force. Secrets are
redacted before anything is written.

```bash
h5i dev log <name>                          # the box's event log
h5i dev inspect <name> --capture <id>       # one receipt, rendered
```

## The output gate

A box cannot write to the host. Getting work out is one command, and it is
deliberately a human step:

```bash
h5i dev export <name>        # → h5i-export/<name>/{patch.diff,report.md,receipt.json}
```

Read `report.md` before applying anything: it lists every command that ran, any
**denied egress attempts**, and which secret rules fired. Then apply the patch
where you want it (`git apply --3way patch.diff`).

`h5i dev apply <name>` still lands a proposed box onto its parent branch in this
repository, for the local case where that is what you want.

## Know what is actually enforced

Never assume a tier. Ask:

```bash
h5i dev probe                    # what this host can enforce at all
h5i dev capabilities <name> --json   # what this box got: tier, egress, limits
```

Tiers: `workspace` (no confinement, just a separate worktree), `process`
(Landlock + seccomp + namespaces), `supervised` (adds a private netns with an
nftables egress allowlist pinned to resolved IPs, and a socket gate),
`container` (rootless Podman: a portable image, with a proxy-based egress
allowlist). Strongest network scoping is `supervised`; `container` buys
portability. h5i never silently downgrades — an unsatisfiable request fails
closed.

## When something is denied

A denial is the policy working, not a bug to route around. Read the message: it
names the path or host and the profile that refused it.

- Filesystem denial → the path is outside `$WORK` and the profile's grants.
- Network denial → the host is not in `net.egress`. Add it deliberately with
  `h5i dev allow <host>` (host-side only; it refuses inside a box).
- A missing tool → the profile's `tools` allowlist does not include it.

Do not disable hooks, edit the policy from inside the box, or reach for a way
around the boundary. Report what was denied and why you needed it.

## References

- [references/boxes.md](references/boxes.md) — lifecycle, sources, naming, gc
- [references/policy.md](references/policy.md) — profiles, tiers, egress, secrets
- [references/export.md](references/export.md) — the gate and reading a receipt
- [references/troubleshooting.md](references/troubleshooting.md) — probe output, common denials
