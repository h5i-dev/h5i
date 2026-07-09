# Borrowing from Cordum and the Agent Governance Toolkit

> Working notes: ideas mined from `../cordum` (Cordum, a Go **Agent Control
> Plane**) and `../agent-governance-toolkit` (Microsoft **AGT**, a polyglot
> **runtime governance layer**) for h5i's provenance, messaging, and `env`
> surfaces. Companion to [`borrowing-from-coasts.md`](borrowing-from-coasts.md)
> and [`comparison.md`](comparison.md).

## TL;DR positioning

The two projects are the *same species* as each other and a *different genus*
from h5i:

- **Cordum** and **AGT** are **governance control planes**. They sit in the live
  request path and **decide, before an action executes**, whether it is allowed:
  a deterministic Policy Decision Point returns `allow / deny / warn / escalate /
  transform`, backed by cryptographic agent identity, a tamper-evident audit
  chain, and (Cordum) a NATS+Redis service mesh or (AGT) a five-language SDK with
  32 ADRs. Their stance: **gate the action, attest the decision, prove it later.**
- **h5i** is a **provenance recorder + confinement sandbox**. It **observes what
  happened** and versions it into git-native, content-addressed evidence
  (`refs/h5i/*`), and it confines `env` execution with real kernel isolation.
  Its stance: **confine the agent, record everything, review through git.**

So the asymmetry is clean and it is exactly why they are a good idea-mine:
**they own the *decision + attestation* discipline; h5i owns the *recording +
confinement* substrate.** Almost everything worth borrowing is a way to make
h5i's recordings **harder to forge** and its policies **able to decide, not just
observe** — translated into h5i's local-first, no-daemon, git-native idiom.

**The governing test** (same discipline as the Coasts doc): *every borrow must
land as a **git-native artifact or a fail-closed policy grant**, never as a
daemon, a service bus, or a "trust the control plane" dependency.* If an idea
needs NATS, Redis, an always-on PDP service, SSO/SAML, or a trust-score database,
it costs h5i a column it uniquely owns (local, no daemon/root) and is rejected or
re-homed onto git.

**What h5i uniquely owns and must not trade away** (from `comparison.md`):
content-addressed provenance, a reasoning/context branch bound to code, and
local-first operation with no daemon and no root. Each idea below is checked
against those.

### The three things h5i is genuinely missing

Both projects independently converged on three primitives h5i does **not** have
today, verified against the current source:

1. **Cryptographic agent identity.** h5i's `msg` "identity" is `$H5I_AGENT` plus
   the git author signature (`msg.rs:1320 signature()` is just
   `repo.signature()`). Anyone can `--from claude`. Yet CLAUDE.md declares
   incoming messages *untrusted collaborator input* — a posture that is
   unenforceable without unforgeable senders. Both Cordum (Ed25519 + edge
   identity contract) and AGT (Ed25519 + DID + SPIFFE) sign the actor.
2. **A tamper-evident chain over the append-only logs.** `grep` for
   `prev_hash / entry_hash / hash_chain` across `src/` returns **zero** hits.
   `messages.jsonl` and `env` `events.jsonl` are append-only and union-merged,
   but nothing detects an *edit* to a historical line. Both projects hash-chain
   their audit logs (Cordum: SHA-256 chain + HMAC + CAS head; AGT: Merkle chain
   with inclusion proofs).
3. **Deterministic, non-self-reported action classification.** h5i's
   `Message.risk` (`msg.rs:122`) is free-text the *sender* writes; `env` policy
   is declared, not derived from what the agent actually did. Cordum's core rule
   is "the client's risk tag is **not** authoritative — the server classifies"
   (`core/edge/classifier.go`).

These three are the spine of the roadmap below.

---

## Idea 1 — The Decision BOM: `h5i bom <sha>` (HIGH, best fit, low risk)

**What AGT does.** A **Decision Bill of Materials** (ADR-0018,
`agent-mesh/.../governance/decision_bom.py`) is a *reconstructible* record of
every factor behind a governance decision. Crucially it is **not** built on the
hot path: a `DecisionBOMBuilder` queries four independent, `runtime_checkable`
Protocol sources *after the fact* — audit, trust, policy, trace (OTel) — and
reports a **completeness level** (all sources vs. partial). Agents never
cooperate; provenance is assembled from signals that already exist.

**Why it is the single best borrow.** This *is* h5i's founding thesis — the four
semantic dimensions — crystallized into one deliverable artifact. h5i already
has the four independent signal sources AGT has to go hunting for, and they are
*already content-addressed*:

| AGT BOM source | h5i equivalent (already exists) |
|---|---|
| AuditSource | git history + `refs/h5i/env` `events.jsonl` (temporal) |
| PolicySource | the `env` manifest + `policy_digest` (`env.rs:145`) |
| TraceSource (OTel) | the context/reasoning branch (`refs/h5i/context`) |
| TrustSource | `AiMetadata` + `IntegrityReport` (intentional + empirical) |

**What h5i borrows.** A read-only `h5i recall bom <commit-sha>` (and
`h5i env bom <name>`) that assembles one **reconstructible provenance card** for
a commit or an env apply: who (agent identity), why (captured human prompt +
reasoning-branch THINK entries), what (diff + gitlink invariants), evidence
(captures, test metrics, integrity severity), under which policy
(`policy_digest`), and a **completeness level** that is honest about missing
dimensions ("no test metrics captured", "reasoning branch absent on this pulled
env"). Emit as markdown for humans and `--json` for tooling. This is pure
synthesis over data h5i already stores — zero hot-path cost, no new capture
machinery — and it makes the "four dimensions" thesis *tangible* in one command
instead of four separate `recall` calls.

**Fit check.** Costs no uniquely-owned column; it is a git-native read model that
*showcases* content-addressed provenance. Strongest recommendation in this doc.

---

## Idea 2 — Cryptographic agent identity for `msg` and commits (HIGH, closes a real gap)

**What they do.** Cordum's edge `identity-contract.md` and AGT's ADR-0001 both
give each agent an **Ed25519 keypair**; actions and messages are **signed**, and
AGT binds a stable `agent_did` into every audit entry. This is how they make
"which agent did this, and did they really?" answerable when many agents share
one API key or one clone.

**The gap in h5i.** Confirmed in source: `msg.rs` signs nothing cryptographically
— identity is a name string plus the git committer signature. CLAUDE.md tells the
agent to treat inbound messages as *untrusted*, but there is no mechanism to
verify a message's `from` actually authored it, and `h5i msg as <name>` can
rewrite the stored default for the whole clone. In a shared-clone, multi-agent
team (h5i's headline scenario) this is a spoofing hole.

**What h5i borrows.** A per-agent Ed25519 key stored under `.git/.h5i/msg/keys/`
(private key local-only, never pushed; public key published into the shared
`agents.json` roster). `msg send` signs the canonical message body; `msg inbox`
verifies and renders a `✓ signed` / `⚠ unsigned` / `✗ bad-sig` marker via the
existing `sanitize_display` path. Keep it **opt-in and back-compat**: unsigned
messages still deliver (older clones interoperate) but are visibly marked, so the
"untrusted input" posture becomes *checkable* rather than aspirational. The same
key can later sign `AiMetadata` provenance on commits so the `--agent` field
stops being self-asserted.

**Fit check.** Local-first (keys are per-clone files, distributed over the
existing `refs/h5i/msg` roster — no CA, no daemon, no SPIFFE server). AGT's DID /
SPIFFE / TEE-attestation stack is explicitly **out of scope** — that is enterprise
identity federation and would import a control plane. Take only the Ed25519
sign-and-verify primitive.

---

## Idea 3 — Hash-chain the append-only logs (HIGH, hardens the associative dimension)

**What they do.** Cordum (`docs/audit-chain.md`, `core/audit/chain.go`): a
per-tenant append-only stream where each event hashes its predecessor's hash
(**SHA-256 chaining**, so any edit cascades forward and is detectable), plus
optional **HMAC-SHA256** (defends even against an attacker who can rewrite the
store), a **CAS-guarded head pointer**, and a verifier that distinguishes
*tampering* from *retention-trimmed gaps*. AGT (ADR-0017): a **Merkle** chain
with O(log n) inclusion proofs and offline self-contained verification.

**The gap in h5i.** git already Merkle-chains *committed* content, but h5i's
mutable-looking append logs — `messages.jsonl` and `env` `events.jsonl` — are
union-merged by id with **no** per-entry hash link. A malicious or buggy merge
that drops or edits a historical message/event is currently undetectable.

**What h5i borrows.** Add a `prev_hash` + `entry_hash` (SHA-256 over the
canonical-JSON entry) to each appended line in `messages.jsonl` and env
`events.jsonl`, and a `h5i msg verify` / `h5i env verify-log <name>` that walks
the chain and reports `intact` vs. the exact break point. This composes with
h5i's **union-merge** semantics: on merge, verify both parents' chains, then
re-link — a conflict surfaces as a chain break rather than a silent overwrite.
Borrow Cordum's *retention-vs-tampering* distinction (a permitted compaction is
not a tamper) so compaction stays possible. HMAC keying is optional and follows
Idea 2's key material.

**Fit check.** Pure git-native artifact (bytes appended to refs h5i already
owns), no daemon. Do **not** borrow Cordum's Redis stream / CAS-via-Lua substrate
— that is its storage backend, not the idea; h5i's substrate is the git ref.

---

## Idea 4 — Deterministic action classification, not self-reported risk (MED-HIGH)

**What Cordum does.** `core/edge/classifier.go`: client-provided risk tags are
**not authoritative**. The server classifies each agent action into bounded
capability/risk enums (`exec.shell`, `file.delete`, `mcp.mutate`, …) from the
*actual* action, and flags partial classifications (`classifier_incomplete`) so
ambiguous cases route **fail-closed**. AGT does the analogous thing with its
Rust decision core over a full JSON snapshot.

**What h5i borrows.** h5i already *captures* the real command (redacted) on every
`env run` and via the tee-shim/wrap-bash hook — it has the ground truth Cordum
classifies from. Add a small **deterministic classifier** that tags each captured
command/action with a derived capability (`exec.destructive`,
`fs.delete`, `net.egress`, `git.history-rewrite`, …) and stores that tag *on the
capture*, independent of any self-reported `Message.risk`. This upgrades
`recall search` (query by *derived* capability, not just the sender's adjective)
and gives the Decision BOM (Idea 1) an objective risk column. The self-reported
`risk` field stays, but is now shown *next to* the derived one — divergence
between them is itself a signal.

**Fit check.** Enriches h5i's existing empirical/evidence store; no new trust
posture. This is the honest, local version of Cordum's server-side PDP: h5i
can't *pre-gate* like a network control plane, but it can *classify what it
observed* and refuse to launder the agent's self-assessment.

---

## Idea 5 — Policy simulation against captured history: `env policy simulate` (MED, excellent fit)

**What they do.** Cordum ships a **shadow/simulation mode**
(`core/policyshadow`, `docs/production-gate.md`): test a new governance rule
against *historical* data before enforcing it, to see what it *would* have
denied. AGT has `agt lint-policy` + a red-team scan.

**Why it fits h5i unusually well.** h5i already stores the historical corpus
these tools need but usually lack: every `env run` is a content-addressed,
redacted capture of a real command. So h5i can answer "what would this policy
change have blocked?" by *replaying captures*, not by re-running anything.

**What h5i borrows.** `h5i env policy simulate <name> --profile <new.toml>` (and
a `--since` window) that loads the env's past captures + the classifier tags from
Idea 4, evaluates the candidate profile against them, and prints a diff:
*"would now DENY 3 past commands (net.egress to api.foo), would now ALLOW 1
previously-blocked"*. Ships as a read-only report over existing evidence.

**Fit check.** Content-addressed provenance is the *enabler* here, not a cost —
this is a feature only h5i's capture store makes cheap. No daemon; pure replay.

---

## Idea 6 — A pre-execution policy gate (the "compliance firewall" mode) (MED, scoped carefully)

**What Cordum Edge does.** The most directly comparable surface to h5i: a
**Claude Code command hook** (`cordum-hook` → local `cordum-agentd` → policy)
intercepts each tool call and returns `ALLOW / DENY / REQUIRE_APPROVAL`
**before** it executes — a compliance firewall around an agent that is *not* in a
sandbox.

**The gap.** h5i's hook layer (wrap-bash PreToolUse, PostToolUse, tee-shim) today
is **observe/record**, not **decide/deny**. Real *gating* in h5i only exists
*inside* `env` via kernel confinement. For an agent running on the host (the
common case), h5i sees everything and blocks nothing.

**What h5i borrows — carefully.** An **opt-in** PreToolUse decision hook that
evaluates the (already-parsed, classified per Idea 4) command against the repo's
`.h5i/env.toml`-style policy and can return deny / require-approval, recording
the decision as a capture. Framed honestly: this is **cooperative** gating (a
hook the agent's harness invokes), *not* the kernel confinement `env` provides —
so document it as "policy-in-the-loop for host agents", the weaker sibling of
`env`, and keep `env` as the answer for untrusted code. Pair a denial with
Cordum's **remediation** idea (return a safer alternative) and its
`fail-mode = closed|open` knob so an unreachable/erroring policy check fails
closed by default.

**Fit check.** Borderline — it edges h5i toward "gate live actions", which is
*their* genus. Justified only because h5i already runs the hooks and does the
capture; it reuses that plumbing to *decide* instead of merely *record*. Must not
grow into a daemon (Cordum's `cordum-agentd`): the decision runs in-process in
the hook, stateless, reading the checked-in policy. If it can't stay daemon-free
and fail-closed, don't ship it.

---

## Idea 7 — Action-bound approval for `env apply` (MED)

**What they do.** Cordum's **ProvenanceGate**: an approval is *not sufficient* to
permit a destructive action — the gateway re-verifies the audit chain and
requires a **resolved** approval bound to the exact `(tenant, approval_ref,
action_hash)` tuple; a "requested-only" event fails closed. AGT's ADR-0030
"action-bound approval protocol" is the same shape.

**What h5i borrows.** Bind `h5i env apply` to the **exact reviewed state**. Today
`propose → apply` is a review loop; strengthen it so `apply` records (and
`propose` references) the `action_hash` = digest of the proposed diff + gitlink
invariants + `policy_digest`. If the env branch tip moved since the reviewed
proposal, `apply` **refuses** unless re-proposed — the reviewer's approval is
cryptographically pinned to what they actually saw, not to the branch name.
Composes directly with Idea 3's chain (the approval is a chained event) and Idea
2's signatures (the approver signs the `action_hash`).

**Fit check.** Pure hardening of the existing mediated-commit / review loop; no
new column cost. This closes a genuine TOCTOU gap between `propose` and `apply`.

---

## Idea 8 — Information Flow Control labels (LOW/EXPLORATORY, novel direction)

**What AGT does.** A stateless **IFC label-flow** model
(`policy-engine/docs/ifc-label-flow.md`): data carries `source_labels`, tool
sinks declare a `clearance`, and policy blocks a high-label source from flowing
into an under-cleared sink (e.g. secret-labeled content into a network egress).

**What h5i could explore.** h5i already has both endpoints of such a flow: the
`secrets` broker (redacted, labeled-sensitive inputs) and the `net.egress`
allowlist proxy (the sink). A future direction: **taint** secret-derived data and
have the egress proxy / capture layer flag when a labeled value appears headed
for an unlisted sink. This maps onto h5i's *associative* dimension (provenance of
*data*, not just authorship) and its existing container egress proxy.

**Fit check.** Genuinely novel and on-thesis, but **research-grade** — reliable
taint tracking across a shell is hard and easy to do misleadingly. Park it as a
direction, not a near-term item; if pursued, it must fail *open on detection
uncertainty but closed on the policy* and never *claim* airtight IFC (same
honesty bar as the egress proxy's "honest L7 scoping" note).

---

## Explicitly NOT borrowing

To keep the borrow asymmetric (per the Coasts-doc discipline), these are
rejected because each would cost a uniquely-owned column:

- **NATS message bus + Redis state (Cordum)** — h5i's substrate is the git ref;
  a service bus + external store kills local-first/no-daemon.
- **Always-on PDP service / `cordum-agentd` daemon** — h5i's policy decisions run
  in-process from checked-in files. No resident service.
- **Trust scoring 0–1000 with decay (AGT)** — a stateful reputation database; too
  heavyweight and stateful for a local git sidecar, and it implies a central
  scorer.
- **SSO / SAML / OIDC, DID/SPIFFE federation, TEE/SEV-SNP attestation** —
  enterprise identity infrastructure; h5i takes only the raw Ed25519 primitive
  (Idea 2).
- **Capacity-aware worker routing, scheduler pools, circuit-breaker mesh** —
  these govern a *fleet of remote workers*; h5i governs *local envs*.
- **The five-language SDK / CAP-v2 wire protocol / control-plane services** —
  h5i is a single Rust binary + git, deliberately.

## Recommended sequence

1. **Idea 1 (Decision BOM)** — highest value, lowest risk, pure read model over
   existing data; ships the four-dimensions thesis as one artifact.
2. **Idea 2 (Ed25519 identity)** + **Idea 3 (hash-chain)** — the shared
   foundation; together they make every recording forgery-evident and make the
   "untrusted messages" posture actually enforceable.
3. **Idea 4 (classification)** → unlocks **Idea 5 (simulate)** and feeds Idea 1's
   risk column.
4. **Idea 7 (action-bound apply)** — small, high-integrity hardening once 2+3
   land.
5. **Idea 6 (pre-exec gate)** — only if it can stay daemon-free and fail-closed;
   frame as the cooperative sibling of `env`, never a replacement.
6. **Idea 8 (IFC)** — research track, no committed timeline.

**One-line thesis:** *Cordum and AGT decide-and-attest; h5i records-and-confines.
The borrow is their **attestation discipline** — signed identity, chained audit,
derived (not self-reported) classification, reconstructible decision records —
grafted onto h5i's git-native, daemon-free core, so h5i's provenance stops being
merely **recorded** and becomes **provable**.*
