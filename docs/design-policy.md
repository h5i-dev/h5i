# Design: policy resolution and the authority validator

How a declared policy becomes an enforced one, and what re-checks that the
translation was faithful. Sections P1 to P4, all shipped.

## In one screen

- `policy.effective.json` dumps the *enforced* state, which is larger than the
  digested intent in `policy.resolved.toml`.
- A per-run translation validator re-derives the subset claims independently of
  the resolver that produced them. Opt-in behind `H5I_FS_AUTHORITY_ENFORCE`.
- The supervisor reads back the child's realized mounts before `exec` and fails
  closed on a mismatch.
- Mount construction is race-free by `openat2` and mount-by-handle; the
  read-back is the net under that.

Part of the h5i design set. The roadmap, and what is next, is
[`ROADMAP.md`](../ROADMAP.md). Superseded positioning and the build logs are in
[`roadmap-history.md`](roadmap-history.md).

---

This was once the tail of a formal verification effort whose Lean 4 model was
removed on 2026-08-28: it cost more to keep in step with the Rust than it
caught. What outlived it is all Rust and all exercised by the normal test suite,
so the claims below are what the code checks rather than what a prover proved.

## P1. The effective configuration, dumped at the apply seam

`policy.resolved.toml` is the digested *intent*. The enforced state is larger:
`ResolvedPolicy` carries serde-skipped fields that never enter the digest and
are still applied as mounts and grants (`ro_binds`, `home_binds`,
`private_binds`, `cache_write`, `work_readonly`, `user_egress_allow`, the
loopback ports, `box_git`). So a reader of the toml alone sees less than a box
gets, and `policy.effective.json` is written at box creation to close that.

The dump serializes the exact values handed to the mechanism appliers in
`build_confined_command`, never a parallel pretty-printer that would be a
brochure every check then checked. It takes the same structs at the seam where
Landlock rules, mount calls and the seccomp filter are constructed, after
`$WORK` expansion and after `prepare_private_paths` and `prepare_home_state`
have run.

Version 1 of a versioned schema, canonically ordered so the digest is stable:
the tier selected and the claim it resolved from; Landlock grants as absolute
paths with read and write rights separately; every bind with source, target and
writability; net mode, egress allowlist with host-side extras, loopback ports
and the AF_UNIX flag; the seccomp template identifier and parameters, the filter
being a fixed artifact per template; rlimits, `env_pass` and the tools
allowlist.

`fs_deny` appears under resolution metadata rather than enforcement, because
Landlock is allowlist-only and `fs_deny` is a preflight refusal on the *policy*.
What can be said is "resolution refuses", never "the kernel denies".

The dump's digest goes in the capture manifest beside the policy digest, so it
is tamper-evident for the cost of one hash. Linux kernel tiers only, matching
the mechanisms it describes (`crates/h5i-sandbox/src/effective.rs`).

## P2. The per-run translation validator

The dump feeds a check on the resolver itself: re-derive the subset claims from
the *shipped* effective config and the declared policy, independently of the
`compute_effective` code that produced them. Translation validation, the shape
of checking a compiler's output for one program rather than proving the
compiler, and it catches resolution silently widening a grant.

`fs_authority::validate_grants` records one boolean per claim in the box
manifest as an `AuthorityVerdict`, rendered by `box status`:

- `fs_subset`: every effective grant was authorized by the declared policy.
- `writes_confined`: every read-write grant was declared writable.
- `cache_readonly`: the config-lock pin and warm cache stay read-only. Private,
  home-state and the one cache-rw refresh bind are writable by design.
- `symlink_clean`: no grant, bind source or mountpoint beneath the worktree
  resolves out through a planted symlink. `None` when the host was not measured.
  Evidence, reported separately, not part of the gate.

`AuthorityVerdict::confined()` gates on the three statically-decidable claims,
where a false is a real bug and safe to fail a launch on.

Fully opt-in. With `H5I_FS_AUTHORITY_ENFORCE` unset nothing executes: no
computation, no host measurement, no manifest field, no gate. Setting it to `1`
computes the verdict at create and run and fails closed. Flipping the default
should be a decision with a receipt trail rather than a drift.

Two bounds. `no_shared_writable`, whether a box shares a writable path with
another live box, is not a single-run property: deciding it needs a lock or an
atomic registry snapshot over all live boxes, or two boxes race into a shared
`/tmp` between their checks. It is a cross-box obligation on the registry
(`effective::interferes`). And backend representability is not a subset question
but whether a backend can express a constraint at all, since enforcement points
differ per tier (kernel: nft plus the proxy; microvm: msb's coarser on/off;
macOS: SBPL carries no network proof). An unrepresentable constraint is marked
unenforced, never rendered as enforced and never silently downgraded.

## P3. Mount realization audit: plan-check plus a read-back

A check on the plan says the plan is safe, not that the kernel realized it, and
for mechanisms whose output is a syscall stream there is no argv to re-parse.
`mount_audit.rs` narrows the gap: after setup and before `exec` the supervisor
reads back the child's realized state and diffs it against the plan, aborting
the launch and landing in the receipt on a mismatch. It reads mount ID and
parent, major/minor, mount root, ro/rw and nosuid/nodev/noexec, propagation
flags, per-target object identity via `statx`/`fdinfo`, the inherited-fd
inventory, `NoNewPrivs` and the seccomp mode.

Two bounds, because "complete mediation" would overstate it. It is a
mount-topology and identity audit: `/proc/<pid>/mountinfo` does not expose the
installed Landlock ruleset or seccomp filter, so the fs-grant enforcement itself
is not read back. And it detects a large slice of the TOCTOU class rather than
all of it, turning mount-swap and masked-path realizations (the shape of runc's
2025 CVEs) from "prevent perfectly" into "detect and fail closed", while a
symlink race leaving topology unchanged is prevented by P4 instead.

The audit needs an explicit exec barrier. `Command::pre_exec` runs setup and
execs in the same breath, with no point for a second party to look. So the child
completes setup and *stops*, on a `SIGSTOP` or a blocking wait on a pipe, the
supervisor audits, and only on success sends *go*.

## P4. Race-free mount construction

The audit is a net; prevention is the floor under it. Two disciplines in the
setup code:

- Resolution. Every path the privileged setup opens on the adversarial worktree
  goes through `openat2` with `RESOLVE_NO_SYMLINKS` and `RESOLVE_BENEATH`, then
  fd-relative operations only, so a path already checked is never looked up
  twice.
- Mount by handle. `openat2` alone does not remove races in path-based
  `mount(2)`, whose source and destination are re-resolved by string. Where the
  kernel allows, setup uses `open_tree`, `mount_setattr` and `move_mount`, so
  the object mounted is the object checked by descriptor identity.

An attacker acting *between* two steps is the case these exist for, and P3's
read-back catches the residue.
