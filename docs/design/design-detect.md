# Design: runtime detection, a kernel-observed lane

The confinement layer answers what the box was *allowed* to do. This answers
what it actually *did*, from a place the box cannot reach. Sections D1 to D14,
built 2026-08-19.

## In one screen

- eBPF over twelve syscall tracepoints, loaded by aya, with the probe in C.
- No CO-RE, no BTF, no `vmlinux.h`: it reads only stable tracepoint arguments,
  so one object loads on every kernel from 5.8 up.
- It never denies anything, and it never renders an absence as a clean result.
- Off by default at three layers: build feature, host capability, policy.
- Needs `CAP_BPF`; the released binaries do not carry the probe (D4).

Part of the h5i design set. The roadmap, and what is next, is
[`ROADMAP.md`](../../ROADMAP.md). Superseded positioning and the build logs are in
[`roadmap-history.md`](../roadmap-history.md).

---

These sections are the authority on design; the order is in
[`roadmap-history.md`](../roadmap-history.md).

The confinement layer answers "what was the box *allowed* to do". This answers
"what did it actually *do*", from a place the box cannot reach. Everything is
additive: no policy decision changes, no syscall is ever blocked by this code,
and a host that cannot run it loses nothing.

## D1. What is being claimed

> For a run whose receipt carries a `runtime` block with `coverage = "full"`,
> the listed detections are the ones that fired on events the *kernel*
> reported for that box's processes, and `events_lost = 0` means no event was
> dropped between the kernel and the record.

Not claimed: that the list is complete for the *behaviour*, since a signature
only fires on what it models, and a box doing something nobody wrote a rule for
produces a clean list and a nonzero event count, which is why the record carries
both numbers rather than a verdict. Not that it is enforcement; denial is
Landlock, seccomp, the netns and the egress proxy, and it stays there (D12). Not
that it survives a kernel-level adversary, since a box with `CAP_SYS_ADMIN` can
unload the programs. And not that absence of the block means a clean run:
absence means the detector did not run, so the block is written even when it
could not attach, carrying the reason.

## D2. The lane problem this fixes

h5i sorts its evidence into lanes because they are not equally trustworthy:

| lane | who observed it | what it covers | what defeats it |
|---|---|---|---|
| `host-env-run` | h5i, as parent | argv, exit code, rusage, wall clock | nothing, but it sees only the *outermost* command |
| `tee-shim` | a shim in the box | interactive shell commands | `exec` without the shim, a script, any child that does its own work |
| `shell-egress` | the CONNECT proxy | HTTP(S) the box routed through the proxy | anything that dials a socket directly |
| `browser` | the browser in the box | console, page errors, failed requests | closing the browser |
| `runner-observed` | the paired worker | the same as the above, one machine over | the same as the above |

Every one either sits at the boundary of the box or lives inside it, so between
them sits what an agent's build actually does: the four hundred processes `npm
ci` forks, the `postinstall` that reads `~/.aws/credentials` because the profile
granted the directory, the test that dials a hardcoded IP because the proxy only
ever sees names.

The kernel sees every `execve` whether or not a shim wrapped it, every `connect`
whether or not it spoke HTTP, and every `openat` whether or not the opener
wanted to be seen. It is the first lane that is neither at the boundary nor
inside the box, and the only one a box cannot defeat by declining to cooperate.

Three things that buys. A grant is `fs_read` on `$HOME` and a fact is
`openat("$HOME/.aws/credentials")`, so a profile can be tightened against what
the box *used*. `net.mode = "proxy"` delivers an allowlist only *for clients
that use the proxy*, and on the workspace tier a direct `connect(2)` goes
nowhere near it, which SECURITY.md states and nothing observed. And `tee-shim`
is box-claimed by construction, so there is now a second opinion from a lane
that is not.

## D3. Related work: Tracee and Tetragon, and what not to take

Both solve this at a scale h5i does not have; the full reading is in
[`roadmap-history.md`](../roadmap-history.md). Three decisions came out of it. The
collector/signature split is Tracee's: rules never touch a ring buffer and the
collector never learns what a credential file is (D7, D9). Lineage lives in the
kernel rather than being reconstructed by racing `/proc`, which is Tetragon's
idea and D6's scope mechanism, because by the time userspace reads `/proc/<pid>`
a short-lived `postinstall` is gone. And a dropped event is reported rather than
smoothed over, which is why `events_lost` sits beside `events_seen`.

Refused: Tracee's event catalogue, since hundreds of instrumented events need
CO-RE plus a full BTF toolchain and a detector that costs a second toolchain is
a detector nobody builds; the daemon, since the unit of observation here is a
run, not a host; and Tetragon's enforcement, since a detector that sometimes
blocks is a policy layer with unclear semantics and h5i already has one with
clear semantics (D12).

## D4. Why aya, and why the probe is C

The loader is `aya`, pure Rust: no libbpf, no libelf, no bindgen, no C toolchain
at link time, and no new cross-compilation story for the musl and Darwin targets
the release matrix already builds. `libbpf-rs` would drag in libbpf, libelf and
zlib as native link-time dependencies; hand-rolling `bpf(2)` is two thousand
lines of ELF parsing aya has already had reviewed.

The probe is C, compiled by `clang -target bpf` in the build script, though aya
has a Rust eBPF frontend. Three reasons: `aya-ebpf` requires a *nightly*
toolchain and the `bpf-linker` binary; the probe is ~350 lines of straight-line
code with no allocation, generics or error handling, where C's disadvantages are
smallest; and every reference implementation writes its probes in C, so the code
is reviewable against them line for line.

The build script is honest about the toolchain rather than demanding it. No
BPF-capable `clang` means the object is not built, the crate still compiles, and
the loader reports `unavailable` with the reason. `H5I_BPF_REQUIRE=1` turns that
into a build failure, which this lane's CI job sets, so a lane that exists to
prove the probe loads never passes by silently skipping it.

The released binaries do *not* carry the probe: the matrix cross-builds musl
targets in containers with no LLVM, and putting a BPF-capable clang into four
images, for a feature that also needs `CAP_BPF` on the user's machine, should
follow somebody wanting it. `h5i box detect probe` reports that in one line and
prints the `cargo install` that fixes it.

## D5. No CO-RE: the stable-ABI cut

CO-RE exists because `task_struct` changes shape between kernels, so libbpf
rewrites field offsets at load time using the running kernel's BTF. It costs a
`vmlinux.h` generated by `bpftool`, BTF at runtime, and a relocating loader.

> The probe reads no kernel structure. It reads only syscall tracepoint
> arguments, which are a stable kernel ABI, and calls only helpers whose
> signatures are stable.

Everything it touches: the `syscalls/sys_enter_*` context, whose layout is fixed
and documented; the `sched_process_fork` and `sched_process_exit` contexts, read
through published field offsets that the loader verifies at attach time by
parsing `/sys/kernel/tracing/events/.../format`, so a kernel that moved a field
is refused rather than misread; and the stable helpers
(`bpf_get_current_pid_tgid`, `_uid_gid`, `_comm`, `bpf_ktime_get_ns`,
`bpf_get_current_cgroup_id`, `bpf_get_ns_current_pid_tgid`,
`bpf_probe_read_user[_str]`, `bpf_ringbuf_reserve/submit/discard`, the map
accessors), all stable since 5.8, which is the floor the loader checks for.

The cut costs real things: no `task_struct` walking, so no parent `comm` without
keeping it ourselves, no cgroup *path*, no mount-namespace inode, no file inode
on `openat`. It buys a probe that loads on any kernel from 5.8 onward with no
build-time headers and no runtime BTF, the difference between a feature that
works on a user's WSL2 kernel and one that works on the maintainer's laptop.

## D6. Scope: which events belong to which box

The hard problem is not collecting events, it is knowing which of the host's
events are the box's. Too permissive reports the user's own editor; too
restrictive misses the interesting child. One constraint decides it:

> The scope has to be decided before the payload exists. A scope programmed
> after the child is spawned has already missed the `execve` that named it,
> which is the most valuable single event of the run.

That rules out cgroup id, exact and cheap but created *inside* the spawn path
and on most hosts unavailable without a systemd user manager that grants
delegation; and pid namespace, whose inode comes from `/proc/<pid>` of a process
not yet forked. The process tree is the one thing knowable in advance, because
h5i is already running.

So the scope is `pidtree`, seeded with every task of the h5i process, not just
the main thread: `Command::spawn` can be called from any thread, and a tree
seeded with one would miss a payload spawned from a worker. The kernel grows the
set on fork and prunes it on exit. Seeding from h5i's own tree leaves two holes,
and the probe's state machine closes both:

1. h5i's own threads are not the box. A new task is `PENDING` until its first
   event, which settles it: a task whose tid equals its tgid leads its own
   thread group and is a *process*, while anything else is one of h5i's threads
   and is marked `SELF`. Exact, one comparison, no kernel structure.
2. h5i's own bootstrap is not the box either. Between fork and exec the child
   still runs `pre_exec`: applying Landlock, opening ruleset paths, setting
   rlimits. So a task is `PRE` until its `execve`, and in that state only the
   exec itself is emitted. A child *inherits* its parent's post-exec state, so a
   fork-only worker is not silently muted for never having execed.

The config map's `mode` field is where a cgroup or namespace filter would go,
and the probe is written so adding one is additive. Nothing in v1 uses it, so
nothing in v1 ships it.

| tier | coverage | why |
|---|---|---|
| workspace | `full` | the payload is a direct descendant of h5i |
| process | `full` | same, plus everything it spawns |
| supervised | `full` | same, and the supervisor is in the tree too |
| container | `partial` | Podman's `conmon` double-forks and reparents, so the workload leaves h5i's tree; what stays visible is the runtime's own activity on the host |
| microvm | `none` | the workload runs against a *guest* kernel, which a host probe cannot see at all |
| anything else | `none` | an unknown tier is uncovered, never assumed covered |

`partial` and `none` go into the receipt as facts with their reasons attached,
the difference between "we looked and found nothing" and "we could not look".
One consequence of seeding from h5i's tree: any process h5i spawns during the
run window is in scope. On the kernel tiers that is the payload and nothing
else; on the container tier it is also the runtime, which is why that tier is
`partial` rather than wrong.

## D7. The event model and the wire format

Twelve tracepoints, one fixed-size event struct, one ring buffer. The struct is
`#[repr(C)]` on the Rust side and a plain `struct` in the probe, with a
compile-time size assertion on each side plus a runtime magic-and-version word
in every event, so a mismatched pair is caught at the first record.

| kind | source tracepoints | captured |
|---|---|---|
| `Exec` | `sys_enter_execve`, `sys_enter_execveat` | path, first argument, argc |
| `Open` | `sys_enter_openat`, `sys_enter_openat2` | path, flags, write-intent bit |
| `Connect` | `sys_enter_connect` | family, IPv4/IPv6 address, port |
| `Socket` | `sys_enter_socket` | family, type, protocol |
| `Ptrace` | `sys_enter_ptrace` | request, target pid |
| `Bpf` | `sys_enter_bpf` | command |
| `Nsop` | `sys_enter_unshare`, `sys_enter_setns` | flags |
| `Module` | `sys_enter_init_module`, `sys_enter_finit_module` | nothing |
| `Memfd` | `sys_enter_memfd_create` | name |
| `Mount` | `sys_enter_mount`, `sys_enter_pivot_root` | target path |
| `Fork` | `sched_process_fork` | child pid |
| `Exit` | `sched_process_exit` | nothing |

Every event carries `ts_ns`, `pid`, `tgid`, `ppid` where known, `uid`,
`comm[16]`, and one 256-byte payload area interpreted per kind. Fixed size
throughout, since a variable-size record would need a second length field to
convince the verifier about, for a saving that does not matter at these volumes.

Volume control lives in the kernel, not userspace. `openat` is the loudest
syscall a build makes, and shipping every one to userspace to throw away 99% is
how a detector becomes a performance problem people turn off. So the probe
filters `Open` in-kernel to write intent, or a path matching one of a small set
of prefixes loaded from userspace: the credential-path list the signatures care
about, pushed down so the rule's own vocabulary decides what the kernel sends.

## D8. The ring buffer, loss, and back pressure

`BPF_MAP_TYPE_RINGBUF`, 256 KiB by default, read by a dedicated thread that
`poll(2)`s the map fd. The size is a policy knob because a `cargo build` and a
`sleep 1` do not need the same buffer.

Loss is counted, never hidden: a failed `bpf_ringbuf_reserve` increments a
per-CPU counter that the session reads at stop time into `events_lost`. A run
with a nonzero count is neither failed nor clean, it is a run whose detection
list is a lower bound, and the console renders it that way.

The reader thread is bounded by the run, starting before the child spawns and
joined with a timeout so a wedged reader cannot outlive the command. Its channel
is bounded too, so a slow consumer degrades the same way a full kernel buffer
does: into a number in the record.

## D9. The signatures

A signature is a pure function from an event stream to zero or more detections:
no I/O, no clock, no per-event allocation beyond what it stores, and therefore
unit-testable against synthetic streams, which is how all of them are tested
since attaching a probe needs privileges CI does not have. Each has a stable id,
a severity, a one-line description, and a bounded exemplar list so a flood
becomes a count rather than a megabyte. Rules are data: `h5i box detect rules`
prints the table, so what the detector looks for is inspectable without reading
Rust.

Network, the family the egress proxy structurally cannot see:

- `net.direct-egress` (`alert`): `connect(2)` to a routable address on a box
  whose network policy is an allowlist or a denial. On the workspace tier this
  is the *only* thing that would notice. It reports the *attempt*, since the
  probe sees the syscall going in, so a connect a netns refused looks like one
  that succeeded; on a `net.mode = deny` box that is the useful reading.
- `net.raw-socket` (`alert`): `AF_PACKET`, or `SOCK_RAW` on any family.
- `net.unix-socket` (`notice`): `AF_UNIX` connect on a profile that did not set
  `unix_sockets`. The supervised tier denies these; nothing else did.
- `net.dns-direct` (`notice`): connect to port 53 or 853.

Credentials, the family a filesystem grant cannot express, because a grant is a
directory and a credential is a file inside it:

- `secret.read` (`alert`): an open of `.ssh/`, `.aws/credentials`,
  `.config/gh/`, `.git-credentials`, `.netrc`, `.kube/config`,
  `.docker/config.json`, `.npmrc`, `.pypirc`, `.gnupg/`.
- `secret.dotenv` (`notice`): a `.env`-family file outside the workspace.
- `secret.proc-environ` (`alert`): `/proc/<pid>/environ` for a pid outside the
  box, the classic read of the host's secrets.
- `secret.h5i-state` (`alert`): a write-intent open under the box's own `.h5i/`,
  which is the box editing its own evidence.

Execution, the shape of a supply-chain payload:

- `exec.from-tmp` (`notice`): exec of a path under `/tmp`, `/var/tmp` or
  `/dev/shm`.
- `exec.memfd` (`alert`): `memfd_create` then an exec of `/proc/*/fd/<n>` in the
  same process: fileless execution, and why `Memfd` is collected at all.
- `exec.interpreter-pipe` (`notice`): a shell exec whose first argument is `-c`
  and whose command line has a download-and-pipe shape.
- `exec.package-manager` (`info`): npm/pip/cargo/gem/go invoked. Not suspicious,
  and present because "what installed things" is the first question asked of any
  supply-chain incident.

Privilege and kernel, uninteresting until they are not: `priv.ptrace` (`alert`),
any attach to a process the box did not spawn; `priv.namespace` (`notice`),
`unshare`/`setns`, which the supervised tier denies outright and the other tiers
did not; `kernel.bpf` (`alert`); `kernel.module` (`alert`); and `mount.change`
(`notice`).

## D10. Where it lands

The receipt grows an optional `runtime` block on `ExecRecord`, appended last and
`skip_serializing_if` empty, so every existing record's shape and pinned digest
is unchanged. It carries the lane string (`kernel-bpf`), the scope kind, the
coverage, `events_seen`, `events_lost`, the detections, and `unavailable` with a
reason. `source` does *not* change: the run is still `host-env-run` and the
kernel lane is a block inside it, because the record is about the command and
the block is a second observer of it.

The console gains a runtime row per record, badged by the highest severity and
grey when the detector did not run, obeying the honesty model: counting over
receipts, not scoring, and grey means "no evidence", never "clean". *The export*
renders detections for every record it carries, and says so when coverage is
`none` rather than showing an empty list. The CLI gets `h5i box detect probe`,
`rules` and `show <name>`.

## D11. Policy surface

```toml
[profile.agent.detect]
enabled = true      # attach the probe for runs under this profile
require  = false    # refuse to run when the probe cannot attach
buffer_kb = 256     # ring buffer size
rules = ["*"]       # rule ids or families to enable; "*" is all
```

All optional, all appended last on `Profile` so no existing canonical
serialization or pinned digest moves. `enabled` defaults to false: turning on a
kernel facility that needs privileges most users have not granted would produce
a fleet of `unavailable` blocks and teach everyone to ignore them. `require =
true` is the fail-closed switch, the setting for "I am running somebody else's
dependency tree", off by default because the failure mode of a mandatory
detector on a laptop kernel is a tool that does not start.

### D11.1. Opt-in at three layers, and the one that is easy to get wrong

| Layer | Switch | Default | What it decides |
|---|---|---|---|
| build | `h5i/bpf` → `h5i-core/bpf` → `h5i-bpf/load` | *off* | whether the binary carries aya and a compiled probe at all |
| host | `CAP_BPF` + `CAP_PERFMON` | not granted | whether it can attach |
| policy | `[profile.X.detect] enabled` | *false* | whether a given box is watched |

What is *not* optional is the evidence types: `h5i-core` depends on `h5i-bpf`
unconditionally with `default-features = false`, so a build with no collector
can still read a receipt written by one that had it. A feature flag that changed
a serialized record's shape would make yesterday's evidence unreadable after an
upgrade.

The subtle layer is the crate's own default. `h5i-bpf` was written with `default
= ["load"]` so the main clippy job would lint the loader, and cargo unifies
features across a workspace build, so `cargo build --workspace` pulled aya and
ran clang for every contributor while `cargo install --path .` did not.
"Optional" had two answers depending on how you built. The default is now `[]`,
and the dedicated CI job passes `--features bpf` explicitly.

## D12. What it refuses to do

- No enforcement, in any form. No `bpf_send_signal`, no `bpf_override_return`,
  no LSM programs. Not "not yet": a detector that can block has to answer for
  the gap between observing an argument and the kernel using it, the TOCTOU that
  makes syscall-argument enforcement unsound in general, and h5i has a policy
  layer without that gap. The way to keep the two unconfused is that this one
  has no verb.
- No BPF LSM. `CONFIG_BPF_LSM=y` is common but `lsm=…,bpf` on the kernel command
  line is not, so an LSM collector would be unavailable on most hosts.
- No CO-RE, no `vmlinux.h`, no BTF requirement (D5).
- No daemon. The probe is loaded for a run and unloaded when it ends.
- No privilege escalation of its own. No `sudo`, no helper install, no setuid.
  It uses the capabilities the process has and names the missing ones.

## D13. Limits, stated up front

1. It needs `CAP_BPF` and `CAP_PERFMON` (or root), so on a stock install the
   answer is `unavailable: missing CAP_BPF` plus the `setcap` command. A
   privilege-separated collector, a small setcap'd helper owning the probe and
   streaming over a socket, is the right long-term shape and is not built. The
   seam is real: `Watch` is one type for "watching" and "could not watch", every
   caller goes through it, and the config map already carries the `mode` field
   such a collector would need (D6). Only `session.rs` would change.
2. Linux 5.8 or newer, for `BPF_MAP_TYPE_RINGBUF`. Older kernels get
   `unavailable`, never a silent fallback to perf buffers.
3. `sys_enter` arguments are the caller's, not the kernel's resolution. A path
   is the string the process passed, so symlinks, relative paths against an
   `openat` dirfd, and races between the read and the kernel's use are all
   unresolved. Every path rule is a *heuristic over caller-supplied strings* and
   the record labels the field as such. That is the price of being CO-RE-free.
4. The container tier is `partial` and the microVM tier is `none` (D6).
5. Pid reuse can in principle admit a foreign process into a `pidtree` scope,
   between an exit h5i has not seen and a fork the kernel reuses the pid for.
   The window is a scheduler quantum and a per-pid generation counter costs more
   than the exposure is worth for an observation-only lane. Stated, not fixed.
6. A box with `CAP_SYS_ADMIN` on the host kernel defeats it. No h5i box has it;
   the sentence exists so nobody has to work that out.
7. `sys_enter` sees attempts, not outcomes. A `connect` the netns refused, an
   `openat` that returned `EACCES` and a denied `ptrace` all look like the ones
   that succeeded. Attaching `sys_exit` would fix it and double the event volume
   for a distinction that on a *confined* box is usually the less interesting
   half: "the box tried to reach 8.8.8.8" is the finding, and what stopped it is
   already answered by the policy.
8. The read-only `openat` feed is filtered in the kernel (D7), so a read of a
   credential path nobody listed is not collected and cannot fire a rule.
   `[detect] open_all = true` removes the filter and is honest about the cost: a
   `cargo build` produces six figures of `openat`.

## D14. The order

The step-by-step order, and what landed against each step, is in
[`roadmap-history.md`](../roadmap-history.md).
