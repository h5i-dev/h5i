# h5i Supervisor Tier — `isolation=supervised`

Status: foundation implemented (fail-closed claim + probe + supervisor scaffold);
enforcement mechanics phased. **This tier never claims untrusted-code
containment unless every component probes green — it refuses instead.**

## Why

The `container` tier's `net.egress` is **L7-only** (an HTTP CONNECT proxy): it
blocks proxy-respecting tooling but not a raw socket to an arbitrary IP. The
`process` tier can't do egress allowlists at all. And kernel denials
(Landlock/seccomp) are **silent** — no record of what was blocked. The supervisor
tier closes both gaps with one architecture, because **enforcement and
observability become the same primitive**: a syscall the supervisor decides on
is also a syscall it can emit as structured evidence.

## The keystone safety property (implemented first)

`isolation=supervised` is **opt-in** and **fails closed**. `sandbox::resolve`
refuses the claim unless *every* required component probes green:

- user namespace, mount namespace, **network namespace**,
- **nftables** usable inside that netns (the airtight L3/L4 egress guard),
- **seccomp user-notification** (`SECCOMP_FILTER_FLAG_NEW_LISTENER`),
- Landlock (filesystem allowlist),
- cgroup v2 delegation (resource limits),
- `no_new_privs` + full capability drop.

If any is missing, the claim is **refused** — never silently downgraded to a
weaker tier. The dashboard renders an unsatisfiable `supervised` claim as
**"supervisor unavailable" (impossible claim)**, not a degraded pass. The
supervisor process dying, blocking, desyncing, or seeing an unsupported syscall
shape is **fail-closed**: the run is killed, not allowed to continue unmediated.

## Architecture

```
 h5i (host)                          supervised run (child)
 ┌──────────────┐  seccomp notify fd  ┌───────────────────────────┐
 │ supervisor   │◀───────────────────│ unshare(USER|MNT|NET|IPC|  │
 │  loop        │   (recv/send/addfd) │   UTS|PID) + no_new_privs  │
 │  - decide    │                     │ + cap-drop + Landlock      │
 │  - emit deny │                     │ + seccomp(NOTIFY on socket │
 │    events    │                     │   /connect/openat-sensitive│
 └──────┬───────┘                     │   , DENY on dangerous)     │
        │ nftables (in child netns)   │ + cgroup v2 limits         │
        │ default-drop + allowlist    └───────────────────────────┘
        ▼
   L3/L4 egress guard (the airtight layer)
```

**Two layers, distinct jobs (Codex's rule):**

1. **netns + nftables = the airtight egress guard (L3/L4).** The child runs in
   its own network namespace with a `default drop` nftables ruleset; only the
   policy's pinned `IP:port`s are accepted (plus a controlled resolver). This is
   the layer that actually *stops* exfiltration — independent of whether the
   process respects any proxy.
2. **seccomp user-notif = a default-deny socket gate + observability.** The
   supervisor intercepts `socket`/`socketpair` and applies a **default-deny**
   gate (Codex review): a call may `CONTINUE` only if it is a "boring" inet
   socket — `AF_INET`/`AF_INET6`, `SOCK_STREAM`/`SOCK_DGRAM`, not `IPPROTO_RAW`
   — or an explicitly granted `AF_UNIX`. **Everything else is denied with
   `EPERM`**: `AF_PACKET`, any `SOCK_RAW`, `IPPROTO_RAW`, `AF_NETLINK`,
   `AF_VSOCK`/`AF_BLUETOOTH`/`AF_CAN`, ungranted `AF_UNIX` (SCM_RIGHTS authority
   passing), and — critically — **any unknown family/type/protocol** (deny, never
   observe-and-allow). Once the gate proves the fd is a boring inet socket,
   **nftables** is the sole enforcement for *where* its packets may go. seccomp
   is never the sole decision for an allow, and we do **not** intercept
   `connect()` for UDP (`sendto`/`sendmsg` bypass it) — nftables covers that.
   Sensitive `openat` (`docker.sock`, `/proc/*/mem`) is denied with `EPERM` (no
   pointer deref); ordinary file access is left to Landlock.

## seccomp-notify, done without the TOCTOU footguns (Codex's list)

1. **Path TOCTOU — never validate-then-continue.** For a path syscall we either
   (a) **deny** (respond `EPERM` — no pointer deref, no race), or (b) leave it
   unmediated to Landlock (the real FS guard). We do **not** read the tracee's
   path pointer and then `CONTINUE`. If a future version needs to *allow* a
   specific path, it must `openat2(RESOLVE_IN_ROOT|BENEATH|NO_MAGICLINKS)` on the
   tracee's behalf and inject the fd via `SECCOMP_IOCTL_NOTIF_ADDFD` — never
   continue the original.
2. **connect() sockaddr TOCTOU.** Same rule: nftables enforces the address; the
   supervisor reads the sockaddr only for *telemetry*, and a telemetry misread
   can never weaken enforcement (nftables already dropped or passed it).
3. **Inherited fds / dirfds.** Close all unexpected fds before exec; reject
   inherited dirfds/sockets; block `open_by_handle_at`/`name_to_handle_at`/
   `mount`/`pivot_root`; tightly control `/proc` magic-links.
4. **fd-passing (SCM_RIGHTS).** Deny arbitrary `AF_UNIX`; host sockets are never
   mounted; `docker.sock`/`ssh-agent`/`gpg-agent` are explicit grants, never
   ambient.
5. **io_uring.** `io_uring_setup`/`register`/`enter` are denied (they bypass
   seccomp syscall mediation).
6. **DNS/egress story.** netns default-drop + a controlled resolver +
   registry/allowlist rules; raw-IP, UDP, IPv6, ICMP, DoH all covered by the
   netns ruleset rather than hoped-away.
7. **Supervisor liveness = fail-closed.** If the supervisor thread dies, blocks
   past a deadline, loses notify-id sync, or sees an unsupported syscall shape,
   the run is **killed**. A run is never left executing with an unmediated
   notify fd.
8. **Performance.** The mediated syscall set is deliberately narrow (sockets +
   a sensitive-path denylist), and allow decisions are cached by a canonical
   policy key so a path-heavy build doesn't drown the supervisor. The supervisor
   never depends on resources inside the sandbox it supervises.
9. **Identity.** Filters are installed before the untrusted `exec`; tracee
   identity uses notify ids / pidfd, assuming hostile multithreaded mutation.
10. **Not a sandbox alone.** seccomp-notify is one mechanism; it sits *with*
    userns/mountns/netns/cgroups/Landlock/nftables/no-new-privs/cap-drop — all
    required-green by the probe.

## Probe (`supervised` readiness)

`supervisor::probe()` reports each component and an overall `usable`. It is the
single source of truth for whether the claim can be satisfied; `resolve` consults
it and refuses otherwise. On WSL2/CI (no cgroup delegation, often no rootless
nftables) it reports `usable=false` with the specific missing piece — and the
tier is correctly unavailable rather than silently weak.

## Phasing (honesty about what enforces today)

- **Phase A (this change): the safety skeleton.** The claim, the fail-closed
  probe + `resolve` gating, the supervisor module with the seccomp-notify
  protocol types and the decision/event model, and the dashboard surfacing.
  Because the full stack does not probe green on current hosts, the tier
  **refuses** everywhere today — which is the correct, safe state.
- **Phase B: live enforcement.** Wire the netns+nftables guard and the
  seccomp-notify loop into `run`, emit structured deny events into the existing
  `EgressSummary`/`risk` pipeline, behind the green probe. Validated on a host
  with rootless nftables + cgroup delegation.
- **Phase C: path allow via openat2+ADDFD**, secret-broker `file`/fd injection
  at this tier, and the `microvm`/`hardened-container` escalation.

Only when Phase B probes green on a host may a profile there truthfully claim
untrusted-code containment.
