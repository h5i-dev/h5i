# i5h Design Notes & Rationale

Companion to [`i5h-protocol.md`](i5h-protocol.md). The protocol doc is the
normative spec — the smallest thing an implementer needs. This file holds the
*why*: the substrate justification, the minimalism discipline, the prior art, and
the positioning. None of it is normative; it explains the choices.

## Why Git? (the load-bearing design choice)

i5h does not invent a transport. It stores messages as ordinary Git objects in a
dedicated ref. This is not a quirk — it is the design, and it is a well-trodden
path. Four mature open-source systems independently converged on "append-only
collaboration data in a Git ref, synced by push/pull":

| System | What it stores in Git | Mechanism i5h reuses |
|---|---|---|
| **public-inbox** | entire email mailing lists, one blob per message in an append-only history | *messages-as-an-append-only-log*; pull-based subscription; "no database" |
| **git-appraise** (Google) | code reviews & comments as one-JSON-object-per-line under `refs/notes/devtools/*` | *one-JSON-per-line + `cat_sort_uniq` merge* — concatenate, sort, drop dups: a grow-only-set CRDT. This is **exactly** i5h's union-merge. |
| **git-bug** | issues as an operation-based CRDT in Git blobs/trees/commits | *logical clocks over the commit DAG instead of wall-clock*; signed authorship |
| **Radicle** | issues, patches, identities as CRDTs ("Collaborative Objects") in `refs/cobs/*` | *non-destructive idempotent union of commit graphs*; **signed refs** so a peer verifies content without trusting the node |

**git-appraise is the closest analogue:** like it, i5h today keeps one JSON
object per line and merges by set-union. (public-inbox is a looser cousin — it
stores *one blob per message*, whereas i5h currently appends to *one growing
`messages.jsonl` blob*; see the honest limitations on what that costs.)
Architecturally, **Radicle COBs and git-bug are the most ambitious cousins** —
both store each change as its own Git commit and replay a causal DAG, which is the
direction i5h would grow if the single-blob layout ever became a bottleneck. We
are standing on proven ground.

What Git gives i5h **for free**, that a socket or database would make us build:

1. **Serverless and offline-first.** No broker to run, no endpoint to expose.
   Agents exchange messages by pushing/pulling whatever Git remote they already
   share. git-appraise's stated rationale: this "removes the need for any sort of
   server-side setup… works with any git hosting provider."
2. **Conflict-free merge by construction.** Messages are immutable; the log is a
   grow-only set; union-merge-by-`id` is commutative, associative, and
   idempotent. Two agents that edited offline converge with no conflict
   resolution — the same property git-appraise gets from `cat_sort_uniq` and
   Radicle from "unioning commit graphs in a non-destructive, idempotent way."
3. **Integrity.** Git is a content-addressed Merkle DAG, so a stored message
   can't be silently altered without changing its object hash and breaking the
   chain. (Integrity, note — *not* authorship.)
4. **Efficient object transfer.** `git fetch` ships only the objects a peer is
   missing (Merkle "have/want"). This makes *transfer* cheap — though, honestly,
   the current single-blob layout claws some of that back at merge time.
5. **Co-location with the code.** A message lives in the same repository as the
   commits, branches, and PRs it references — `branch`, `links.commits`,
   `links.pr` point at objects in the very same store.
6. **Replay and audit.** The log *is* the history. Every request, ACK, and DONE
   can be replayed deterministically, which is what powers `h5i msg replay`.

### Honest limitations

(The same systems hit these; i5h inherits the answers.)

- **The single growing blob does not scale forever.** Today a send rewrites the
  whole `messages.jsonl` blob, and a divergent pull parses and unions the entire
  log. Git transfers *objects* efficiently, but it does not let a peer fetch
  "just the new lines," and the rewrite/union cost grows with the log. This is
  fine for the volumes a coding-agent channel sees; if real measurements ever
  show it hurting, the answer is the Radicle/git-bug layout — **one commit per
  event** (or segmented logs + a snapshot index) — reserved as a future scale
  profile. The wire semantics are defined independently of that physical layout
  precisely so it can change without a protocol break.
- **Don't use one ref per message.** Git's `packed-refs` scans linearly and loose
  refs burn inodes. i5h keeps *all* messages inside the single `refs/h5i/msg`
  ref as append-only lines — the git-appraise approach — never a ref per message.
- **Git is not a query engine.** Search/filter needs an index. Every system above
  builds one (public-inbox→Xapian, git-bug→in-memory excerpt cache). i5h's inbox
  state and any search are a local index over the log, not a Git query.
- **You must supply the semantics.** Git gives storage and merge; ordering rules,
  kinds, and read-state are i5h's responsibility (as they are for git-bug and
  Radicle). That is what the protocol doc specifies.

## Design discipline

i5h is deliberately small, because the historical record is unambiguous about
what kills messaging and agent protocols: **completeness, formal correctness, and
negotiation machinery added in the name of generality.** SOAP/WS-\* drowned in
negotiation layers; CORBA's optional-but-typed features meant "compliant" peers
still couldn't talk; **FIPA-ACL — the closest ancestor of i5h's typed kinds —
saw adoption wane under 20+ performatives and a mandatory shared ontology.** Our
guardrails, drawn from that record:

1. **Few options → ubiquity** (RFC 6709). Every field a re-implementer must
   handle is a tax on adoption. The required core is seven fields.
2. **Unknown fields MUST be ignored** (RFC 6709 must-ignore). This is how i5h
   evolves without a flag day: new ideas go into the optional set or the `meta`
   bag, never as new required fields. There is exactly one `version`.
3. **Strict where structured, free where not.** A structured field must have
   testable semantics. If a receiver would have to "do its best" to interpret it,
   it belongs in free-text `body`, not in a typed field (the Postel-was-wrong
   lesson: liberally accepting loose variants causes protocol decay).
4. **Free text first.** `h5i msg send <agent> "<text>"` is the whole UX for the
   common case. Structure is *earned* by an important handoff, never required.
5. **Tiny verb set.** The `kind` taxonomy is the single feature most at risk of
   the FIPA-ACL failure. It stays small, every kind has a one-line meaning, and
   an unknown kind degrades gracefully to a plain message.
6. **Don't pre-extend** (RFC 6709). Features go in only when a concrete present
   use case demands them. Speculative generality is parked in
   [Considered & Deliberately Deferred](#considered--deliberately-deferred),
   not shipped.

**A field earns a place in the required core only if it is** (a) needed on *most*
messages, (b) strictly testable, (c) implementable from the spec alone in an
afternoon, (d) free of any shared registry/ontology/handshake, (e) such that two
agents still interoperate when it is absent, and (f) human-readable in the raw
log. Everything else is optional or deferred.

## Considered & Deliberately Deferred

These were evaluated against real prior art and **left out of the core on
purpose** — each adds fields or shared machinery the current coding-agent use
case does not justify. They are recorded here (not deleted) so the reasoning is
visible and the door stays open. Adding any of them must clear the
[earns-a-place](#design-discipline) bar with a concrete use case.

| Deferred feature | Prior art | Why not now |
|---|---|---|
| **Hybrid logical clocks** (`ts`+logical counter) for perfectly causal sort | Lamport/HLC; git-bug's clocks-over-DAG | `reply_to` already gives causal *display* order; a low-volume agent channel doesn't need globally-correct linearization. Extra field, no present payoff. |
| **Per-agent feed chains** (`seq`/`prev`, gap detection) | Secure Scuttlebutt | Useful at scale or under adversarial drops; today, Git's own history + seen-ID tracking suffice. |
| **Full ancestor chain** (`references[]`) for gap-tolerant threading | email `References` (RFC 5322) | Single `reply_to` reconstructs threads for our volumes; the redundant chain is weight without a felt problem. |
| **Capability *negotiation*** (handshake, must-understand bits) | A2A Agent Card, MCP handshake | This is the WS-\*/CORBA failure axis. The lightweight half — a static agent roster — is kept; the negotiation machinery is speculative generality. |
| **Contract-Net bidding** (`CFP`/`PROPOSE`/`ACCEPT`/`REJECT`) | FIPA Contract-Net | No current task-auction use case. A broadcast `ASK` (plus the optional advisory claim) covers "who can take this?" until one genuinely exists. |
| **Per-message signatures** (`sig`/`key`, `did:key`, `alg`) | Radicle signed refs, SSB | Authenticity is a real gap but needs a trust-anchor + signer→identity policy and a canonicalization (RFC 8785). Deferred to a security profile, not shipped half-done. |
| **Large performative taxonomy / ontologies** | FIPA-ACL, KQML | The documented adoption-killer. i5h keeps a tiny kind set and `NOT_UNDERSTOOD` instead. |
| **Per-session delivery for one identity** — two live sessions both `H5I_AGENT=claude` in one clone (TODO) | maildir per-reader cursors | They share `cursors/claude.json`, so delivery *splits* between them (each sees a slice; no loss/corruption after the deliver-then-ack + atomic-union fix). The model is one agent per identity, so this is an anti-pattern to avoid, not a supported mode. If genuinely needed, track read-state per `(identity, session)`. |

The throughline: i5h prefers what **Git already provides** (history, merge,
content-addressing) and what **`reply_to` already provides** (causal order) over
new protocol fields. We add machinery only when a concrete need outweighs the
adoption tax it imposes.

## Local delivery UX (outside the wire format)

How messages reach an agent is a CLI concern, deliberately *not* part of the wire
protocol (the sibling tool `agmsg` keeps the same separation). Useful, portable
behaviors worth implementing without touching the message format:

- **Delivery modes** — `watch` (live side-terminal), `turn` (Stop-hook delivery
  between turns), `both`, or `off`.
- **Role-scoped inboxes** — one identity active per session, so two agents
  sharing a clone don't consume each other's mail.
- **Clear turn-vs-watch semantics** — `watch` is a human dashboard showing a
  recent window; `inbox`/`history` are the authoritative per-agent views.

(The normative read-state rule lives in the protocol doc under *Read-state*.)

## Positioning

i5h borrows ideas from three traditions but keeps only the parts that survive the
[design discipline](#design-discipline):

- **From agent communication languages** (FIPA-ACL, KQML): typed communicative
  acts and `reply_to` threading — but a *tiny* kind set, no ontology, free text
  first. We treat FIPA's 20+ performatives as the anti-pattern.
- **From modern agent interop** (MCP, A2A, ACP): a familiar request lifecycle and
  the negotiate-before-use *instinct* — but realized over a durable Git ref
  instead of a live HTTP session, and without the discovery/negotiation layers
  that concentrate adoption friction.
- **From distributed systems** (CRDTs, gossip, git-bug, Radicle, git-appraise,
  public-inbox): grow-only-set union-merge and Git-as-substrate — the parts that
  are *free* because Git already implements them.

### How other agent systems pass messages

The framework landscape moves fast and any feature matrix rots quickly, so the
durable contrast is one of *what each design optimizes for*, not a cell-by-cell
table. **Mainstream multi-agent frameworks optimize live orchestration**: AutoGen
0.4+ passes async events between in-process actors; CrewAI hands off between
role-based tasks; OpenAI's Swarm/Agents SDK does handoffs as function returns;
LangGraph routes through shared graph state; Letta/MemGPT coordinate through a
server's shared memory; MCP and A2A are live HTTP/SSE transports. Several offer
*durable* state — notably LangGraph checkpointers, Letta's store, and LangSmith
traces — and these are genuinely replayable. But that durability is **centralized
and online**: it is state for one orchestrated app behind a shared backend, not a
portable artifact two independently-running agents can each append to *offline*
and reconcile later. (Where persistence is "implementation-defined," as in
MCP/A2A, treat it as not guaranteed by the protocol.)

**i5h optimizes the opposite axis: repo-resident, offline-convergent receipts.**
Its wedge is a *conjunction* none of the above targets together — **durable +
offline-first + decentralized + repo-resident + replayable + CRDT-merged**. MCP
and A2A connect *live* agents and degrade gracefully when the connection drops;
i5h connects *asynchronous* agents where there is no connection to drop.

### Prior art in the Git-native niche (and how i5h differs)

i5h is **not** the first to coordinate agents through Git — and saying so keeps it
honest:

- **GNAP** (Git-Native Agent Protocol) is the closest: JSON files in a repo, a
  `messages/` directory, "git history *is* the audit log." But it writes to the
  **working tree**, is **not strictly append-only** (run/task state mutates), and
  reconciles with **last-write-wins + pull-rebase-retry**. i5h instead uses a
  dedicated **side ref** (`refs/h5i/msg`, out of the working tree), a **strictly
  append-only** log, and **CRDT union-merge by message id** — so concurrent
  offline writers converge with no rebase race and no lost message.
- **EvoGit** ([arXiv:2506.02049](https://arxiv.org/abs/2506.02049)) coordinates
  agents *implicitly* through a Git phylogenetic/commit graph — asynchronous, with
  **no explicit messaging or shared memory**; i5h provides explicit, typed
  receipts (`kind`, `reply_to`, `thread_id`).
- **Open GAP / GitAgentProtocol**
  ([open-gitagent/gitagent](https://github.com/open-gitagent/gitagent)) is
  Git-native but versions **agent definitions/skills/memory**, not append-only
  *inter-agent receipts* — adjacent landscape, different layer.
- **CodeCRDT** ([arXiv:2510.18893](https://arxiv.org/abs/2510.18893)) is the
  academic validation of the premise: observation-driven CRDT coordination with
  deterministic convergence for multi-agent work. (It establishes the CRDT/
  offline-convergence idea; i5h's contribution is persisting that log as a Git
  side ref rather than as runtime state.)

So the honest claim is narrow and defensible: not "first Git-native agent
protocol," but **the append-only, side-ref, CRDT-by-id coordination log** — the
combination that makes concurrent offline coordination converge without losing or
rewriting a message.

## README Pitch

> i5h is agent radio for Git — **coordination receipts** for coding agents.
> Claude, Codex, and reviewers exchange typed handoffs through `refs/h5i/msg`, so
> every request, risk, ACK, and DONE is a durable Git object that can be pushed,
> pulled, merged, audited, and replayed — no server, no socket, no schema
> registry.

Longer:

> Where MCP and A2A connect *live* agents, i5h connects *asynchronous* ones. A
> message is one line of JSON in a Git ref — the same proven pattern git-appraise
> uses for distributed code review (and Radicle/git-bug take further with a commit
> per change). Seven required fields, a tiny set of typed kinds, and append-only
> set-union that converges across clones with no conflict resolution. No broker to
> run; agents reconcile whenever they next sync.

Screenshot caption:

> Two agents coordinate a review across clones. The messages are not local chat;
> they are Git objects under `refs/h5i/msg`.

## References

Git as an application data store (the "why Git" precedents)

- public-inbox — mailing lists as Git repositories — <https://public-inbox.org/README.html>
- git-appraise — distributed code review in `refs/notes` (`cat_sort_uniq` merge) — <https://github.com/google/git-appraise>
- git-bug — issues as an operation-based CRDT in Git, logical clocks over the DAG — <https://github.com/git-bug/git-bug/blob/master/doc/design/data-model.md>
- Radicle — Collaborative Objects (CRDTs in Git) and signed refs — <https://docs.radicle.xyz/guides/protocol>
- Git reftable (ref-scaling background) — <https://git-scm.com/docs/reftable>

Protocol minimalism & extensibility

- R. Gabriel, "Worse is Better" — <https://www.jwz.org/doc/worse-is-better.html>
- RFC 6709, *Design Considerations for Protocol Extensions* — <https://www.rfc-editor.org/rfc/rfc6709>
- M. Thomson, *The Harmful Consequences of the Robustness Principle* (IAB) — <https://www.ietf.org/archive/id/draft-iab-protocol-maintenance-05.html>
- RFC 5322 §3.6.4 — `Message-ID`/`In-Reply-To`/`References` threading — <https://www.rfc-editor.org/rfc/rfc5322.html>

Agent communication languages & interop (borrowed selectively)

- FIPA-ACL (the cautionary tale) — <http://www.fipa.org/specs/fipa00061/SC00061G.html>
- Model Context Protocol — <https://modelcontextprotocol.io/specification>
- Agent2Agent (A2A) — <https://a2a-protocol.org/latest/specification/>
- Survey of agent interoperability protocols (MCP/ACP/A2A/ANP) — <https://arxiv.org/abs/2505.02279>

How agent frameworks pass messages (the ephemeral/centralized contrast)

- AutoGen 0.4 actor runtime — <https://www.microsoft.com/en-us/research/articles/autogen-v0-4-reimagining-the-foundation-of-agentic-ai-for-scale-extensibility-and-robustness/>
- LangGraph persistence & checkpointers (the strongest *centralized* durable/replay story) — <https://docs.langchain.com/oss/javascript/langgraph/persistence>
- Letta / MemGPT shared memory blocks — <https://docs.letta.com/tutorials/shared-memory-blocks/>
- OpenAI Swarm (stateless handoffs) — <https://github.com/openai/swarm>
- On the unmet need for immutable inter-agent audit trails — <https://truescreen.io/articles/agent-to-agent-audit-trail/>

Git-native agent coordination (the niche i5h shares — see Positioning for how it differs)

- GNAP — Git-Native Agent Protocol (working-tree JSON, tasks/runs/messages, LWW + pull-rebase-retry) — <https://github.com/farol-team/gnap>
- EvoGit — decentralized agents coordinating via a Git phylogenetic graph (no explicit channel) — <https://arxiv.org/abs/2506.02049>
- Open GAP / GitAgentProtocol — Git-native agent *definitions*/skills/memory (different layer) — <https://github.com/open-gitagent/gitagent>
- CodeCRDT — observation-driven CRDT multi-agent coordination, deterministic convergence — <https://arxiv.org/abs/2510.18893>

Distributed systems foundations

- Kafka delivery semantics — at-least-once + idempotency, *not* exactly-once effect — <https://docs.confluent.io/kafka/design/delivery-semantics.html>
- Dynamo (gossip + Merkle anti-entropy) — <https://www.allthingsdistributed.com/2007/10/amazons_dynamo.html>

Envelope, IDs, and message hygiene

- BCP 14 — RFC 2119 / RFC 8174 (normative MUST/SHOULD) — <https://www.rfc-editor.org/info/bcp14>
- RFC 7493 — The I-JSON Message Format — <https://www.rfc-editor.org/rfc/rfc7493>
- RFC 9562 — UUID (UUIDv7, time-ordered ids) — <https://www.rfc-editor.org/rfc/rfc9562>
- CloudEvents — event `id`/`source` discipline (id identifies one occurrence) — <https://cloudevents.io/>
- RFC 8785 — JSON Canonicalization Scheme (for future message signing) — <https://www.rfc-editor.org/rfc/rfc8785>
