# Design: policy resolution and the authority validator

How a declared policy becomes an enforced one, and what checks the translation.

| section | status |
|---|---|
| P1. effective configuration dump | shipped, always on (Linux kernel tiers) |
| P2. per-run translation validator | shipped, opt-in (`H5I_FS_AUTHORITY_ENFORCE=1`) |
| P3. mount realization audit | designed, not built |
| P4. race-free mount construction | designed, not built |

These checks are Rust code exercised by the normal test suite. Nothing here is
machine-proved. Roadmap: [`ROADMAP.md`](../ROADMAP.md).

## P1. The effective configuration

`policy.resolved.toml` is the digested intent. The enforced state is larger:
`ResolvedPolicy` has serde-skipped fields that are applied but not digested
(`ro_binds`, `home_binds`, `private_binds`, `cache_write`, `work_readonly`,
`user_egress_allow`, loopback ports, `box_git`). `policy.effective.json`, written
at box creation, records the full enforced state.

The dump serializes the exact values passed to the mechanism appliers in
`build_confined_command`, after `$WORK` expansion and after
`prepare_private_paths` and `prepare_home_state`. It is not a separate
pretty-printer.

Schema version 1, canonically ordered so the digest is stable:

- tier selected and the claim it resolved from;
- Landlock grants as absolute paths, read and write rights separate;
- every bind: source, target, writability;
- net mode, egress allowlist with host-side extras, loopback ports, AF_UNIX flag;
- seccomp template id and parameters (the filter is fixed per template);
- rlimits, `env_pass`, tools allowlist.

`fs_deny` is listed under resolution metadata, not enforcement. Landlock is
allowlist-only, so `fs_deny` is a preflight refusal of the policy: "resolution
refuses", never "the kernel denies".

The dump's digest goes in the capture manifest beside the policy digest. Code:
`crates/h5i-sandbox/src/effective.rs`.

## P2. The translation validator

`fs_authority::validate_grants` re-derives the subset claims from the shipped
effective config and the declared policy, independently of `compute_effective`.
It catches resolution silently widening a grant. Results go in the box manifest
as an `AuthorityVerdict`, shown by `box status`:

| claim | meaning |
|---|---|
| `fs_subset` | every effective grant is authorized by the declared policy |
| `writes_confined` | every read-write grant was declared writable |
| `cache_readonly` | the config-lock pin and warm cache stay read-only (private, home-state and the one cache-rw refresh bind are writable by design) |
| `symlink_clean` | no grant, bind source or mountpoint under the worktree resolves out through a symlink; `None` when not measured |

`AuthorityVerdict::confined()` gates on the first three, which are statically
decidable. `symlink_clean` is reported but does not gate; a `false` prints a
warning and the run continues.

With `H5I_FS_AUTHORITY_ENFORCE` unset, nothing runs: no computation, no host
measurement, no manifest field. With `1`, the verdict is computed at create and
run and a failure refuses the launch.

Not covered:

- `no_shared_writable` (two live boxes sharing a writable path) is a cross-box
  property. It needs a lock or atomic registry snapshot and belongs to the
  registry (`effective::interferes`), not a single run.
- Backend representability. Enforcement differs per tier (kernel: nft plus the
  proxy; microvm: msb's on/off; macOS: SBPL carries no network proof). A
  constraint a backend cannot express is marked unenforced, never shown as
  enforced.

## P3. Mount realization audit

Not built. `crates/h5i-sandbox/src/mount_audit.rs` parses mount point and
per-mount `ro` and has no callers. There is no exec barrier.

Specification: after setup and before `exec`, the supervisor reads back the
child's realized state and diffs it against the plan, failing the launch and
recording the mismatch in the receipt. It would read mount ID and parent,
major/minor, mount root, ro/rw, nosuid/nodev/noexec, propagation, per-target
identity via `statx`/`fdinfo`, the inherited-fd inventory, `NoNewPrivs` and the
seccomp mode. This needs a barrier: the child finishes setup and stops (on
`SIGSTOP` or a blocking pipe read) until the supervisor sends go.

Limits even once built: `/proc/<pid>/mountinfo` does not expose the Landlock
ruleset or seccomp filter, so fs-grant enforcement is not read back. It detects
mount-swap and masked-path realizations, not a symlink race that leaves topology
unchanged (that is P4's job).

## P4. Race-free mount construction

Not built. No `openat2`, `open_tree`, `mount_setattr` or `move_mount` call exists
in the workspace; setup resolves paths by string (the residual
`env::staged_path_violation` names in its comment). The filesystem boundary
today is the Landlock ruleset, and nothing re-checks how it was constructed.

Specification:

- Every path privileged setup opens on the worktree goes through `openat2` with
  `RESOLVE_NO_SYMLINKS` and `RESOLVE_BENEATH`, then fd-relative operations only.
- Where the kernel allows, mounts use `open_tree`, `mount_setattr` and
  `move_mount`, so the mounted object is the checked descriptor.
