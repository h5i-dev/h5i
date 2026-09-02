# Design: the remote runner

> The box's boundary becomes a machine you own and can afford to lose. The
> product does not move: the repo, the policy, the credentials, and the patch
> gate stay here.

Sections R1 to R13. R13.1 is built; R13.2 to R13.4 are not.

## In one screen

- Placement is a second axis beside the isolation tier: `local` or
  `runner:<name>`.
- Transport is SSH with a forced command, one session per RPC. No listener
  anywhere, of any kind, ever.
- The worker *is* h5i, not a thin shim, because argv is path-laden and the
  egress proxy has to run where podman runs.
- Export quarantines the runner's objects and authors the commit on this side.
- Evidence gets its own lane, `runner-observed`, which collapses to box-claimed
  exactly when the runner host is compromised.

Part of the h5i design set. The roadmap, and what is next, is
[`ROADMAP.md`](../../ROADMAP.md). Superseded positioning and the build logs are in
[`roadmap-history.md`](../roadmap-history.md).

---

These sections are the authority on design; the order and what landed against
each step are in [`roadmap-history.md`](../roadmap-history.md), along with the full
reading of the two codebases the design was drawn against: the E2B spec repo and
bhatti, a Go single-node microVM sandbox service.


## R1. Placement, not a tier

A second axis on every box, orthogonal to the tier it already declares:

```
placement:  local | runner:<name>
isolation:  workspace | process | supervised | container | microvm
```

A runner requires Linux and the h5i protocol, nothing else. Everything past that
(isolation tiers, container runtime, KVM, memory, storage, persistence, its own
internet route) is an advertised capability, and a capability the runner lacks
is a refusal, never a silent weakening. There is no fallback ladder across
machines. No device class is named anywhere: the capability report, not the
hardware, is the vocabulary.

The MVP builds one cell, `runner × container`. The kernel tiers are deferred for
real work rather than principle: they assume the worktree backend even locally,
so they wait on a copy-in workspace path that does not exist anywhere yet.
`runner × microvm` waits until the container cell has earned it. On a
sacrificial runner the tier protects the runner's *other* boxes and its own
state machinery; the machine boundary is what protects you, so a weak tier on a
strong boundary is legitimate for weak hardware.

The security claim: the agent's execution moves to hardware whose compromise you
have priced in, while the working tree, the credentials, the receipts store and
the apply step stay on the machine that never runs agent code. This does not
make the box harder to escape, it changes what an escape reaches. Against Coder
or a self-hosted E2B the differentiator was never the remoting, it is that the
far end returns a reviewable patch and evidence rather than a live filesystem
you trust by default.

## R2. Related work: take the wire shapes, refuse the planes

bhatti moved its internal API off loopback TCP onto a unix socket after a
sandbox reached the daemon's loopback listener. The forced command over SSH
stdio is the end of that trajectory: no listener anywhere, of any kind, ever.

What both references were refused is the same thing twice: the plane.
Control-plane REST, an in-guest HTTP daemon, tokens minted at create, a
bearer-token listener and a WebSocket relay all exist because their clients and
sandboxes meet across the public internet. Ours meet across an SSH session we
already authenticated.

## R3. The cut: the worker is h5i

A thin `h5i-worker` driving podman while the real logic stays here is wrong
three times over. Argv is path-laden: `container::build_run_argv` is pure but
full of local paths, so built here it reasons about another machine's filesystem
and built there it needs the policy-to-argv logic, which *is* `h5i-sandbox`. The
egress proxy must run where podman runs, since the container tier wires
`HTTPS_PROXY` to the slirp4netns address meaning "the machine podman runs on".
And the binary is already the distribution, since boxes exec
`/usr/local/bin/h5i` today; that last is an MVP decision, not a permanent
constraint, since a slim worker build is a cargo feature set away and the
protocol never learns the difference.

```
this machine (control plane)          runner (worker)
  repo, worktrees, env branches         the isolation backend it advertises
  manifests, policy resolution          the box volume (the only copy
  receipts store, the console             of the source over there)
  credentials, secrets broker           the egress CONNECT proxy
  export gate, apply                    a state dir with lease files
  h5i runner pair/probe/gc              h5i runner serve-stdio
```

The worker is stateless across invocations: box state lives in podman and the
state dir, not in a daemon. On this side, placement is consulted at the three
dispatch sites in `sandbox.rs` *before* the tier match. No backend trait is
invented for it.

## R4. Transport: SSH, a forced command, one session per RPC

Mostly a list of things not built.

- No custom listener, no TLS, no tokens. The runner's `authorized_keys` gets one
  line, `restrict,command="h5i runner serve-stdio" ssh-ed25519 …`, against a
  dedicated keypair generated at pair time. `restrict` kills shell, port
  forwarding, agent forwarding, X11 and pty allocation in one word.
- The client shells out to `ssh` rather than linking a library, inheriting
  `~/.ssh/config`, the agent and ProxyJump. The invocation is pinned hard: the
  pair key with `IdentitiesOnly=yes`, a per-runner `UserKnownHostsFile` recorded
  at pair time, `StrictHostKeyChecking=yes` forever after. That is the mutual
  authentication the share ticket model was never designed to provide.
- One SSH session is one RPC. Concurrency is OpenSSH's ControlMaster, about ten
  milliseconds per session against a warm master, which deletes request ids,
  channel numbers and interleaving bugs from the MVP protocol entirely.
- The pty rides in frames, not in SSH. `restrict` disables pty allocation and
  nothing re-enables it; the worker allocates the pty around `podman exec`.

WAN comes later and is not this transport: R12.

## R5. The frame protocol

bhatti's frame, kept because two hundred lines that survived production beat
anything designed fresh: `[u32 BE length][u8 type][payload]`, length excluding
the prefix, a hard 1 MiB cap, every frame written with one write. JSON payloads
for control types, raw bytes for stdio. The codec is transport-free, like
`h5i-share`'s `wire.rs`, so it is testable over an in-memory pipe.

```
0x01 HELLO        0x02 HELLO_ACK      0x0E ERROR       0x0F KEEPALIVE
0x10 PROBE        0x11 CAPABILITIES
0x20 CREATE_BOX   0x21 DATA           0x22 DATA_DONE   0x23 CREATE_RESULT
0x30 EXEC         0x31 EXEC_STARTED   0x32 STDOUT      0x33 STDERR
0x34 PTY_OUT      0x35 STDIN          0x36 PTY_IN      0x37 RESIZE
0x38 SIGNAL       0x39 CLOSE_STDIN    0x3A EXIT
0x40 EXPORT_BOX   0x41 EXPORT_RESULT
0x50 DESTROY_BOX  0x51 LIST_BOXES     0x52 GC
```

- `EXEC_STARTED` is the mandatory first frame of an exec stream. "It spawned"
  and "here is output" are different facts, so the first gets a short handshake
  timeout, the stream then lives under the long timeout, and reads run under an
  idle clock. Three clocks, never one.
- `EXIT` carries what the receipt needs: exit code, wall and cpu time, max RSS,
  and the `EgressSummary` from the worker-side `ProxyHandle`, in the same struct
  the local path produces so the receipt writer does not fork.
- `ERROR` on create carries the tail of the worker-side log.
- `HELLO` is static, `PROBE` is dynamic, and neither does the other's job.
  `HELLO` exchanges what never changes within an install; there is no
  negotiation, the lower protocol version governs, and both sides gate features
  by named constants so a worker too old fails at probe time rather than
  mid-create. Everything that drifts belongs to `CAPABILITIES`.
- Identity never rides in a frame. `runner_id` is computed on this side from the
  host key SSH verified against pinned known_hosts. The worker may echo it as a
  sanity check, and the echo is never identity-bearing: a value the peer asserts
  about itself is what pinning exists to make irrelevant.
- Transfer reuses `DATA`/`DATA_DONE` behind a JSON header frame, with
  `DATA_DONE` carrying the SHA-256 the receiver verifies before acting.
- Limits are per RPC, not just per frame. The frame cap bounds one message and
  nothing stops a peer streaming forever, so every RPC class carries a
  receiver-enforced total; the sender's declared size is a claim, and the
  receiver aborts the moment it is exceeded.
- Commands are argv arrays end to end. A shell is asked for by name, never
  implied by the protocol.

## R6. Pairing, probing, and where runner config lives

```
h5i runner pair pi5 user@192.168.1.50
h5i runner probe pi5
h5i runner list | gc <name> | unpair <name>
```

`pair` generates the dedicated Ed25519 keypair at mode 0600, installs the
forced-command line (over existing SSH access, or by printing the line to
paste), records the host key into the per-runner known_hosts (trust on first use
at pair, strict forever after), and runs `HELLO` plus a first `PROBE`. The only
hard failure is no `h5i` on the far side; everything else lands in the report:

```json
{
  "arch": "aarch64",
  "memory_mb": 512,
  "workspace_mb": 4096,
  "isolation": ["process", "supervised"],
  "container": false,
  "kvm": false,
  "persistent_boxes": true,
  "own_egress": true
}
```

Pair records the report and does not judge it; `box create` enforces it.

Identity is the key, not the name. A label can be re-paired to a different
machine tomorrow, so `runner_id = SHA-256(host public key)` is what the manifest
and every receipt record. A reinstalled machine with a fresh host key is a fresh
identity, which is correct: it *is* a different trust anchor.

The account is part of the boundary. `restrict` binds *our key*, not the
machine; every other key, account and sshd setting is whatever the admin left.
So the docs specify a dedicated OS user and `pair` offers to create it: no
password login, no sudo, no supplementary groups, a clean environment, the
forced command by absolute path. `probe` warns on what it can see. The docs must
not conflate "the pair key is constrained" with "the account is".

Runner config is host-scoped, never in the repo: which machines *this* developer
can reach is a fact about this machine, like the user egress allowlist. A
profile may later carry a human-facing label, which resolves to `runner_id`
before the manifest is authored; only `runner_id` is digested.

`probe` is `box probe` one machine over, and for every tier the runner
advertises it must end by running `verify_exec` functionally. Present bits are
not a working confined exec, and a runner whose advertisement its own kernel
cannot back gets it corrected, loudly.

## R7. Create: copy in, one machine over

Remote create *dissolves* the hardest local problem instead of carrying it: the
identical-path git-plumbing binds exist only because a local box shares the host
repo's worktree inodes, and a remote box shares nothing.

1. Create checks the request against the capability report, refusing with the
   capability named. The stored report is a cache of the last `PROBE` and the
   client-side check exists for good errors; the worker refusing at create time
   is the enforcement. Then the front half of `env::create` runs unchanged: pin
   `base_commit` and `base_tree`, create the env branch, write the manifest. No
   worktree. The manifest carries `runner_id` in `validate_imported_manifest`'s
   object-id loop beside `base_commit`, `base_tree` and `policy_digest`, as a
   64-character hex check, fail-closed. The display name sits beside it for
   humans: the box is bound to the machine, not the label.
2. This side builds a git bundle: `base_commit`, shallow allowed, plus one
   synthetic commit when the box starts from dirty state. A bundle rather than a
   tar because the bundle *is* the base identity, verifiable on receipt and
   incremental when a later phase re-syncs.
3. `CREATE_BOX` carries the box id, image, limits, serialized resolved policy
   and bundle digest, with the bundle following as `DATA` frames. The worker
   verifies the digest and materialises into a box-owned directory, never a bind
   mount of anything on the runner. A remote create makes the box (source,
   policy, lease); the container is made when there is something to run in it
   (R13.3), since the container tier is `podman run --rm` per command and has no
   warm form, and a warm container idling on a small runner costs memory for
   nothing. When it lands, copy `microvm::guest_name`'s rule: the container's
   name is a digest of its own create argv, so a config change forces a fresh
   one by construction.
4. `CREATE_RESULT` echoes the digest of the policy the worker actually enforced,
   and this side refuses to mark the box live unless it matches. Cheap, and it
   converts "the worker silently ran an older policy" from a possibility into a
   detected fault.

Create is crash-safe by state, not by hope. The worker builds under
`creating/<operation_id>` and an atomic rename to `live/<box_id>` is the one
moment a box exists, so there is no state in between for a crash to invent. A
re-sent `CREATE_BOX` whose request digest matches returns the existing result,
so a lost response costs a retry rather than a duplicate; a matching id with a
different digest is refused. Orphaned `creating/` entries carry a short TTL and
fall to the normal sweep.

Secrets keep the microvm tier's argv discipline: nothing secret in remote argv
or environment visible in the runner's process table.

## R8. Exec and shell

`env::run` and `env::shell` become an `EXEC` RPC carrying argv, cwd, the
already-filtered env, an optional pty size, and a timeout the worker clamps to
its own default and maximum. Output streams back as `STDOUT`/`STDERR`, or
`PTY_OUT` when a pty was asked for. Pty against pipes is one flag on the same
RPC, discriminated by frame type; in pty mode there is no `CLOSE_STDIN`, there
is Ctrl-D, because that is what a terminal is.

Disconnect semantics: the *container* survives a dropped session, the *exec*
dies with it, which is what happens locally when h5i is killed mid-run.
Reattachable execs are a later capability the frame layout leaves room for.

Concurrency: worker invocations are separate processes, so the lock is a file
lock in the box's state dir. `CREATE_BOX`, `DESTROY_BOX` and `EXPORT_BOX` take
it *exclusive*, `EXEC` takes it *shared*. An export attempted while execs hold
it is refused with the live execs named, because an export racing a build reads
a torn tree, and a torn tree that passes validation is worse than a refused RPC.
Nothing waits silently.

## R9. Export: quarantine the objects, author the commit here

Export is the trust boundary, so this is the careful one. `env::diff` already
has a no-worktree branch that diffs `base_tree` against the env branch tip
through the object store.

1. `EXPORT_BOX`: the worker commits the box's tree in the runner-side clone and
   returns a bundle of `base_commit..tip`, an archive of exportable untracked
   artifacts, and its receipt spool.
2. This side unpacks into a throwaway bare repository with its own object
   database, never the host repo. A ref namespace is not a quarantine: fetching
   writes untrusted objects into the shared store, and a ref only quarantines
   reachability. The throwaway gets `git bundle verify`, `transfer.fsckObjects`,
   and the structural checks that only make sense before anything is trusted:
   bundle size and object count against the R5 limits, a blob ceiling, path
   length, symlink and hardlink entries flagged, and no tree entry that
   traverses.
3. The host takes the tip tree, not the commits. The mediated-commit scans run
   against the `base_tree`-to-fetched-tree diff inside the throwaway, violations
   are filtered, and only the surviving tree's objects are materialized into the
   host repo as one host-authored mediated commit. The runner's history and
   authorship are discarded by construction. This needs `mediated_commit`
   refactored to accept a tree source instead of a worktree, the single invasive
   change to existing code in this part.
4. Downstream is untouched. A remote box that cannot complete the fetch degrades
   to the detached-box posture that already exists: export-only, no apply.

## R10. Evidence: the runner-observed lane

A remote execution observed by the worker is host-observed *from the runner's
point of view*, and it arrives here over a wire. Folding it into
`HOST_OBSERVED_LANES` would overclaim, since a compromised runner kernel can
forge it; calling it box-claimed would underclaim, since the box cannot edit it
and the channel is mutually authenticated with pinned keys.

So it is a third thing with an honest name, *`runner-observed`*: observed from
outside the box, by an h5i we authenticated, on hardware we do not control. The
security claim of this part is one sentence: *runner-observed collapses to
box-claimed exactly when the runner host is compromised, and the runner host is
the machine you chose to be able to lose.* The `Grade` axis is unchanged.

Receipts are written on this side from the `EXIT` and `EXPORT_RESULT` payloads.
No signing is added, because none exists locally either and a signature from a
machine the threat model already sacrifices is not evidence.

## R11. Lifecycle without a daemon

No resident process means nothing watches a clock, so the reaper is
opportunistic. Every box carries a lease, a file in the state dir and a label on
the container, default TTL two hours and hard TTL twelve, refreshed by any RPC
that touches the box. Every worker invocation reaps expired boxes before doing
its own work, the same sweep-on-entry pattern
`sweep_invalid_worktree_registrations` uses, plus an explicit `h5i runner gc`.
Reaping stops the container, snapshots a partial export bundle and the receipt
spool, and deletes after a grace window; when the snapshot fails it keeps the
box and says so. There is no heartbeat, because there is no daemon to keep
alive.

Persistence is a capability, not a requirement. A `persistent_boxes: false`
runner (read-only OS, tmpfs workspace, one microSD) loses every box at reboot,
and the protocol treats that as a lease that expired early: the next contact
reaps the record and anything not yet exported is honestly gone. Separate
filesystems for OS and box storage is the recommended shape on persistent
runners, so a box that fills its disk takes the state partition rather than the
machine; a pairing-time warning, not something h5i can enforce.

## R12. What the MVP refuses, and what comes later

Refused, fail-closed, with the reason in the error:

- Profiles that need the secrets broker or the auth proxy. Both exist to keep
  secret values on this machine. The later design is a credential channel: a
  dedicated long-lived session carrying muxed connections from the runner-side
  proxy back to the auth proxy here. That channel is the one place a mux enters
  the protocol, which is why it is not in the MVP. Until it exists, no agent
  that needs model credentials runs on a runner.
- Any request past the runner's advertised capabilities, per R1.

Assumed, and stated so it is priced: the MVP runner has its own outbound
internet. Image pulls and package installs leave through the runner's own
CONNECT proxy under the box's allowlist. A runner with no default route is not a
supported MVP topology; it becomes one when brokered egress lands.

Deferred, with their shape already known. Brokered no-network egress: the
container gets no network at all and its only egress is a proxy whose upstream
is the credential channel, so raw sockets fail closed instead of bypassing the
CONNECT proxy; when it lands it lands for local boxes too. WAN transport over
iroh: a runner ALPN beside the share ALPN, with the pair keys authenticating
above it, reusing the existing QUIC stack without touching the ticket model, and
the runner dials out so no router configuration. The kernel tiers on a runner,
blocked on the copy-in workspace path they lack even locally. And reattachable
execs, runner pools, and re-sync of a live box's source.

## R12b. What the adversarial review changed

Eighteen rounds against the branch, 2026-08-17, under this part's threat model:
the runner may be compromised, so the interesting direction is runner to host.
Thirty-seven findings, all fixed; the round-by-round record is in
[`roadmap-history.md`](../roadmap-history.md). Four rules came out of it and now
govern the code:

- Never invoke the git CLI in a tree whose configuration is hostile. A box owns
  its own repository config, and git executes `core.fsmonitor` and
  `filter.<name>.clean` out of it, so staging an export with `git add` let any
  box with a shell run a command as the runner user. `core.hooksPath=/dev/null`
  covers neither mechanism. libgit2 is only half the fix: it runs no commands
  but still honours `core.worktree` and a `gitdir:` pointer, so the export's
  staging must also refuse hostile *redirection*, not only hostile execution.
- A refspec is not a limit on what a fetch writes. git follows tags by default,
  so a crafted bundle placed an attacker-named tag object in the host repository
  past every quarantine check. R9's "only commits the host authored" was false
  for tags until `--no-tags` and `--no-write-fetch-head`.
- Gate on the tier the policy carries, not the one the request declares.
  `run_with_env` dispatches on the former, so validating the latter let a box be
  recorded as `container` and run every command unconfined.
- Pin both host-key files and pass `-F`. ssh consults `GlobalKnownHostsFile`
  too, and a hostile `~/.ssh/config` redirected every RPC to another machine
  with the pin apparently intact. That breaks the attestation, not merely the
  transport, because `runner_id` is what a manifest records.

R12's refusal of credential-bearing profiles was also written down and never
implemented: values never crossed, but the runner resolves grant descriptors
against its own environment, so a box could be handed the runner's credential.

One process lesson. Several fixes were themselves wrong, three of them surviving
until a round was spent reviewing the *fixes* rather than the code, so reviewing
a patch is not the same activity as reviewing a system.

## R13. The order

The step-by-step order, and what landed against each step, is in
[`roadmap-history.md`](../roadmap-history.md).
