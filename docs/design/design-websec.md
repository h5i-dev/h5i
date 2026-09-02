# Design: the HTTP workbench, sections W1 to W20

Status: proposed, 2026-09-02. Nothing in this file is built. It is the design
for turning what h5i already records into the loop a person runs today in Burp
Suite next to a browser, expressed as verbs an agent can call.

## In one screen

> Burp Suite is an HTTP workbench for humans. h5i is an HTTP workbench for
> agents.

- The engine *is* the HTTP client, so there is no MITM proxy, no CA
  certificate, no browser proxy setting and no manual handoff from history to
  Repeater. The three-tool dance collapses into one process that already writes
  every request down before the bytes move.
- The split is deliberate. h5i owns the deterministic half: exact capture,
  stable ids, session and cookie state, structured edit, resend, structural
  diff, timing, scope, rate limit, audit. The agent owns the judgement half:
  which request matters, which parameter to bend, what a difference means, what
  to try next.
- No scanner. No payload generator. No wordlists. No vulnerability verdicts.
  Those live in the agent's own throwaway script, and W20 says why keeping them
  out is what makes the rest reusable.
- Bash first. The CLI is designed from day one to be wrapped, so a thin Python
  client can arrive later without h5i learning any Python (W10).
- **Not in the default binary.** `h5i websec` arrives with
  `h5i plugin install websec` and is a separate executable that holds no
  privilege of its own. W21 is the packaging design.
- The one hard new decision is the message store (W5): receipts hold counts,
  never values, and a workbench needs bodies. Those are two different artifacts
  with two different retention stances, and merging them would put credentials
  into every bug report.

Part of the h5i design set. The roadmap, and what is next, is
[`ROADMAP.md`](../../ROADMAP.md); the engine it builds on is
[`design-browser.md`](design-browser.md).

---

## W1. What is being claimed

> For a session run with capture on, every HTTP message the engine sent or
> received has a stable id, and `h5i websec replay <id>` re-sends that exact
> message, with the edits named on the command line and nothing else changed,
> through the same policy, the same cookie jar and the same identity that
> produced it.

Not claimed: that h5i finds anything. It sends what it is told to send and
reports what came back, in a shape a program can branch on. Not claimed: that a
replayed request is byte-identical to the original at the TLS layer, since
connection reuse, header ordering imposed by the client and the jar's current
contents all move. What is claimed is that every difference is named in the
reply, so a replay that does not reproduce is a fact the agent can read rather
than a mystery.

## W2. Why this belongs in h5i and not in a proxy

Burp's architecture is a consequence of not owning the browser. It intercepts
from beside the network, which costs a CA certificate, a proxy setting, TLS
interception that modern clients increasingly refuse, and a permanent inability
to say *why* a request happened.

h5i sits on the other side of that line. `crates/h5i-browser/src/broker.rs`
already funnels every byte through one `Fetch`, and
`crates/h5i-browser/src/receipt.rs` already records the decision before the wire
and the outcome after it. Three things follow that a proxy cannot have:

1. **Provenance.** The receipt carries an `Initiator` (`navigation`,
   `subresource`, `frame`, `redirect`), and the action log beside it carries the
   verb the agent asked for. A proxy sees a GET; h5i knows it was the third
   redirect hop of a click on "Export".
2. **No interception gap.** There is nothing to bypass. A request that is not in
   the log did not happen, which is the guarantee the whole product already
   leads with.
3. **State without re-plumbing.** The jar, the identity, the policy and the
   budget are the session's, so "resend this as user B" is a lookup rather than
   a cookie string pasted by hand.

## W3. What already exists

The workbench is mostly a matter of exposing machinery that is shipped.

| piece | where | what it gives |
|---|---|---|
| named sessions, cookie jars, `--restore` | `src/cli/browser.rs` | multiple logged-in identities on one machine (feature 9) |
| request log | `crates/h5i-browser/src/receipt.rs` | seq, time, method, url, initiator, allowed, status, decoded and wire bytes, duration, cookie counts (part of features 1, 2) |
| action log and `h5i browser audit` | `receipt.rs`, `src/cli/browser.rs` | one ordered timeline of verbs and fetches, lane-labelled (feature 3) |
| single fetch entry point | `broker.rs` | one place to add capture and one place to police a replay |
| policy, budget, CORS, identity | `policy.rs`, `budget.rs`, `cors.rs`, `identity.rs` | scope and rate limiting already exist and already refuse (W16) |
| `extract`, `snapshot`, `markdown`, `find` | `extract.rs`, `snapshot.rs`, `markdown.rs` | response reading primitives to reuse rather than reinvent (feature 14) |
| `session script --save` and `browser replay` | `replay.rs` | the *page-level* replay layer, which W11 keeps distinct from the HTTP-level one |

## W4. What is missing, precisely

1. ~~**Bodies and headers are not stored.**~~ Built 2026-09-02; see W5. The
   engine now records both directions of every hop when a session is opened
   with `--capture`, including the hops a redirect chain passes through and the
   header set as the client built it rather than as the caller asked for it.
2. **No stable message id.** `seq` is per session and per process. Feature 2
   needs an id that survives a session restart and names a request in a
   filename, a diff and a finding.
3. **No way to send an arbitrary request.** `Fetch` has url, method, body and
   content type, and no header map at all. Every replay verb waits on that
   field, and on the policy question it opens (W16).
4. **No response comparison, match, timing detail, parallelism, site map,
   sequence engine, DOM taint or OAST.** Features 11 to 20.

## W5. The message store, and the credential problem

The receipt log says, in a comment that is load-bearing: a credential in a
receipt is a credential in a bug report. It stores cookie *counts*. A workbench
needs the opposite: the exact bytes, headers included, `Authorization` included.

These do not merge. The design keeps two artifacts.

- **Receipts** stay exactly as they are. Append-only JSONL, safe to paste,
  shipped in exports, fail-closed (no writable log, no fetch). Nothing in this
  design adds a value to them.
- **The message store** is new, opt-in, and separate. `h5i browser open
  --capture` (off by default) writes each request and response into
  `<session-dir>/messages/`, mode 0600, as a header sidecar plus a
  content-addressed body blob so that fifty identical 2 MB responses cost one.
  It is never included in an export, a share or a bug report unless the caller
  names it, and `h5i browser close` can drop it with `--capture-drop`.

Bounds are part of the format, not a policy layered on top: a per-message body
cap (default 8 MiB, truncation recorded as a field rather than silently), a
per-session store cap (default 512 MiB), and a MIME skip list for fonts and
media that a workbench never reads. A truncated or refused body is an explicit
state, never an empty string.

Built, 2026-09-02: `crates/h5i-browser/src/capture.rs`, reached by
`h5i browser open --capture`. Messages land in `<session>/messages/` as one JSON
file per phase per receipt sequence number, with bodies content-addressed under
`bodies/<sha256>` so a loop that gets the same answer a hundred times stores it
once. Two deviations from the paragraph above, both deliberate. The store
*refuses* rather than evicts when it fills, recording `store-full` against the
body, because eviction wants the pin that `websec pin` has not built yet and
dropping the oldest evidence silently is worse than refusing the newest loudly.
And writing is best-effort where the receipt is fail-closed: a fetch is never
refused because the store could not be written, since the receipt still records
that the request happened. Failures are counted and reported.

The store is the taint boundary for everything downstream. Anything an agent
reads out of it is untrusted content authored by the target, which is the same
stance the snapshot already takes and the reason `snapshot::collapse` exists.

## W6. Ids

`req_<n>` and `res_<n>`, where `<n>` is the receipt `seq`, scoped to a session
and stable for its life. Fully qualified as `<session>/req_42` when a command
spans sessions, which cross-session replay and diff both do.

A request and its response share a number because they are two phases of one
receipt row, which is already how `RequestRecord` models them. `res_42` is the
response to `req_42` and no other numbering is introduced.

## W7. The command surface

One noun, `websec`, under the existing `h5i` binary, with the engine-side work
reached over the control channel a resident session already listens on. Every
verb takes `--session`, defaults to the default session, and takes `--json`.

```
h5i websec requests   [--filter EXPR] [--since SEQ] [--limit N]
h5i websec show       <req_42> [--raw | --json] [--part request|response|both]
h5i websec replay     <req_42> [--set K=V]... [--edits-file PATH|-] [--as SESSION]
h5i websec diff       <res_42> <res_43> [--body-mode auto|json|dom|text|none]
h5i websec match      <res_43> [--regex RE] [--jsonpath P] [--css SEL] [--header H]
h5i websec sitemap    [--origin ORIGIN]
h5i websec sequence   run <file.json> [--var K=V]...
h5i websec finding    create --from req_42,res_43 [--note TEXT]
h5i websec rpc        --stdio
```

`replay` always creates a new receipt and a new pair of ids, so a replay is
itself replayable and a chain of five attempts is five rows in the audit
timeline. There is no hidden buffer that the next command implicitly acts on:
every verb names its inputs, because an agent's turns are not a session in the
shell sense and "the last response" is exactly the state that breaks when two
agents share a box.

## W8. The edit language

Structured edits, not string surgery, because the point of owning the client is
that nobody has to hand-maintain `Content-Length`.

```
--set path=/api/v2/users/456
--set query.user_id=456
--set header.X-Forwarded-For=127.0.0.1
--set cookie.session=<value>
--set json.$.user.role=admin
--set form.username=admin
--set body.raw=@payload.bin
--set multipart.avatar.filename=../../etc/passwd
--set multipart.avatar.content_type=image/png
--unset header.Origin
```

Rules that have to be settled once:

- **Order is applied, then recomputed.** Edits apply in the order given, then
  `Content-Length` and multipart boundaries are recomputed. `Host` follows the
  URL unless explicitly set, because overriding it is a real test.
- **Nothing is silently corrected.** A JSON edit against a body that is not JSON
  is an error naming the actual content type, not a coerced string. An edit
  whose path does not exist is an error unless `--set-create` is given, since a
  typo that silently no-ops is a wrong answer that looks right.
- **Raw is available and honest.** `--edits-file` accepts a `raw` key holding a
  whole request, in which case h5i sends the bytes given and reports which of
  its own invariants it had to break to do so.
- **Binary is a path or base64, never a shell argument.** `@file` for a path,
  `base64:` for inline.
- **`--edits-file -` reads stdin.** This exists from day one, because shell
  quoting is where a Bash-driven agent loop actually breaks:

```bash
jq -n --arg p "$PAYLOAD" '{set: {"query.username": $p}}' |
  h5i websec replay req_42 --edits-file - --json
```

The reply echoes the edits as applied, so the log of an exploit is the log of
what was sent rather than of what was requested.

## W9. The JSON contract

This section is what makes W10 cheap later, so it is a phase A requirement, not
a polish item.

- `--json` on every verb, including errors. An error is
  `{"error": {"code": "...", "message": "...", "hint": "..."}}` on stdout with a
  nonzero exit, never a bare string on stderr.
- Exit codes are stable and few: 0 success, 1 usage, 2 refused by policy or
  budget, 3 network failure, 4 not found, 5 target of the assertion did not
  match (so `match` is usable in `if` without `jq`).
- stdout carries machine-readable results only. Progress, timing chatter and
  warnings go to stderr.
- Every payload carries `"schema": "websec/1"`. Fields are added, never
  repurposed; a removal is a new major.
- Time is RFC3339 with microseconds, matching `receipt.rs`.

## W10. Bash now, RPC later, Python last

The order is deliberate and comes from the discussion this file records: finish
the CLI and JSON, solve real problems with Bash, and only add a client where a
loop actually hurts.

Bash covers more than expected:

```bash
for id in $(seq 100 200); do
  h5i websec replay req_42 --set "query.user_id=$id" --json |
    jq -c '{id: .request.edits, status: .response.status, len: .response.bytes}'
done
```

It stops covering it at adaptive payloads, blind extraction character by
character, timing statistics, race windows, multipart and crypto, and anything
past a few hundred replays where process startup dominates. The answer to the
last one is not a Python library, it is a long-lived process:

```
h5i websec rpc --stdio
{"id":1,"method":"replay","request":"req_42","set":{"query.id":123}}
{"id":2,"method":"replay","request":"req_42","set":{"query.id":124}}
```

One JSONL request per line, one reply per line, ids for correlation, and the
same schema as `--json`. A Python client is then roughly 150 lines wrapping that
pipe, with `requests()`, `inspect()`, `replay()`, `compare()` and nothing else.
No `scan_sqli()`, no `find_idor()`, no `exploit_ssrf()`: the moment security
logic enters the client, h5i owns a payload database and the product changes
shape. Keeping the RPC language-neutral also means Node, Rust or Bash get the
same surface for free.

## W11. Two replay layers, and why both

h5i will have `h5i browser replay` (a recorded *page* script: click, type,
submit) and `h5i websec replay` (a recorded *HTTP message*). They are not
redundant and neither can be built from the other.

The page layer reproduces a human path through an application, including
JavaScript that computes a nonce. The HTTP layer reproduces one message with a
field bent, which is what a test needs and what the page layer cannot express.
The workflow uses both: drive the browser to reach state, then work at the HTTP
layer from the requests that state produced.

`h5i websec sequence` is the join. A sequence is an ordered list of steps, each
either a page verb or an HTTP replay, with `extract` bindings between them
(feature 11, feature 12):

```json
{
  "steps": [
    {"replay": "req_login", "extract": {"csrf": "jsonpath:$.csrf"}},
    {"replay": "req_update",
     "set": {"header.X-CSRF-Token": "${csrf}", "json.$.role": "admin"}}
  ]
}
```

Bindings are named, typed by their extractor prefix (`jsonpath:`, `regex:`,
`css:`, `header:`, `cookie:`), and a binding that fails stops the sequence by
default, because a step acting on a token the previous step failed to produce is
acting somewhere the sequence never described. That is the same rule
`browser replay` already applies to its steps.

## W12. Structural diff

`h5i websec diff res_42 res_43` is Comparer for programs. The reply has three
layers so an agent can branch cheaply and read expensively only when it must:

1. **Verdict fields**: `same`, `status_changed`, `length_delta`,
   `time_delta_ms`, `similarity` (0.0 to 1.0).
2. **Header diff**: added, removed, changed, with a stable order and cookie
   values redacted by default (`--reveal-cookies` to override, since the whole
   point of some tests is the cookie).
3. **Body diff**, typed by content type: JSON gets a structural diff keyed by
   JSON pointer; HTML is diffed as a DOM shape through the machinery
   `snapshot.rs` and the read IR already have, so a re-ordered ad slot does not
   read as a difference; anything else is line-based. `--body-mode` forces it.

The `similarity` number matters more than it looks: boolean-based blind
injection is a loop over "is this response the true page or the false page", and
a single float an agent can threshold is the difference between a five-line loop
and a model reading two HTML pages per candidate character.

## W13. Match and extract on responses

Intruder's grep-match and grep-extract, without Intruder. `h5i websec match`
takes any number of conditions (`--regex`, `--jsonpath`, `--css`, `--header`,
`--status`, `--length-gt`, `--time-gt`), returns each one's boolean and its
captures, and sets exit code 5 when the combined expression is false. That makes
the common CTF loop a shell conditional with no `jq` in it, while the same call
with `--json` gives a script the captures.

This is an executor for someone else's conditions. h5i ships no conditions of
its own.

## W14. Timing

`duration_ms` on the receipt is a page-load number. Blind injection needs a
better one, so replay reports `connect_ms`, `tls_ms`, `ttfb_ms` and `total_ms`
measured at the socket, plus `--repeat N` returning median and median absolute
deviation rather than a mean that one scheduling hiccup ruins.

The honest caveat belongs in the reply, not in a footnote: a session inside a
box pays a proxy hop and a namespace, so absolute latency is not the host's.
Comparisons within one session are sound, comparisons across placements are not,
and the reply names the placement so the agent can tell.

## W15. Parallel replay

`--parallel N` with bounded concurrency, and `--sync` which holds N prepared
requests and releases them together for race windows (feature 16). Two things
have to be right:

- It composes with `budget.rs`. A parallel replay spends from the same
  `Limits`, so a runaway loop hits `max_requests` exactly as a runaway page
  does. Rate limiting is a first-class flag (`--rate R`) because a CTF target
  and an authorised engagement both have someone who will notice.
- It is not a fuzzer. The payloads come from the caller, one per line on stdin
  or as a list in an edits file. h5i sends them efficiently, records each, and
  returns a table. It never generates one.

## W16. Scope, policy and authorisation

Replay does not get a new path to the network. It goes through `Broker::fetch`
like everything else, which means the session's policy decides, the receipt is
written first, and an off-scope replay is refused with a reason rather than
sent. A replay cannot widen its session's allowlist; changing scope means a new
session with a new policy, which is a visible act.

Adding a header map to `Fetch` (W4) is the one place this could go wrong. The
rule: headers are carried, recorded and sent, and the policy checks the request
by URL as it does now, with the header set included in the recorded decision.
Hop-by-hop headers the client owns (`Content-Length`, `Transfer-Encoding`,
`Connection`) are recomputed, and an attempt to set them is reported as
overridden rather than accepted silently, since request smuggling is a real test
and pretending to support it is worse than declining.

The audit posture is the product's, not an afterthought: every payload an agent
sent is in the timeline with the verb that sent it, so a session is reproducible
by someone who was not there. That is what makes this usable in an engagement
rather than only in a CTF.

## W17. Site map

Fold the request log into a tree by origin, then path, carrying methods seen,
status codes seen, parameter names observed, content types, and whether each
node was reached by navigation or by script (feature 18). Add endpoint
candidates the pages themselves disclose: form actions, `fetch` targets found in
loaded JavaScript, `link` and `script` sources, WebSocket URLs from
`wsclient.rs`, and Structured metadata from `structured.rs`.

Candidates are labelled as candidates. A URL scraped from a bundle was not
visited, and the map must not blur the two, because "what did this session
reach" is the question the receipts exist to answer.

## W18. DOM instrumentation

The DOM Invader analogue, and the first feature that is a plugin rather than
core. With `--script` on, instrument the sources (`location`, `name`,
`document.referrer`, `postMessage` data, storage) and the sinks (`innerHTML`,
`eval`, `document.write`, `setAttribute` on URL attributes, `Function`), and
report reachability from source to sink with the path taken. Add a
`postMessage` log and a prototype pollution probe.

It sits last but one because it is engine-invasive, it only pays off on
client-side problems, and everything before it pays off on every problem.

## W19. OAST

Out-of-band detection needs a callback receiver, which means a service, which
means an operating cost and a privacy story. The design here stops short of
running one:

- A neutral interface: `h5i websec oast token` mints a correlation id and
  returns a hostname and URL to embed; `h5i websec oast poll <token>` returns
  the interactions seen, with type, source address and time.
- Backends are pluggable and none ships hosted. Bring your own domain, your own
  webhook endpoint, or a self-hosted receiver; a lab backend that binds a local
  listener covers CTF targets that can reach the host.
- Nothing is sent to an h5i-operated service, because a payload URL in a target
  application is data about someone else's system.

## W20. Order, and what each phase buys

The twenty features, ranked, with the phase that carries them.

| # | feature | Burp analogue | phase |
|---|---|---|---|
| 1 | full request and response capture | Proxy history, Logger | A |
| 2 | stable message id | Proxy history | A |
| 3 | action to request provenance | Logger, site map | A |
| 4 | history search and filter | Proxy history | A |
| 5 | raw and typed views | Message editor, Inspector | A |
| 6 | same-session replay | Repeater | A |
| 7 | structured request edit | Repeater, Inspector | A |
| 9 | multiple sessions and jars | browser sessions | A (mostly shipped) |
| 13 | machine-readable response diff | Comparer | A |
| 14 | match and extract primitives | Intruder grep | A |
| 8 | multipart and upload editing | Repeater | B |
| 10 | replay as another session | Repeater plus session rules | B |
| 11 | extract and bind | macros, session rules | B |
| 12 | multi-request sequences | Repeater sequences | B |
| 15 | precise timing | Repeater, Intruder | B |
| 16 | controlled parallel replay | Intruder, race testing | B |
| 17 | redirect chain observation and control | Proxy, Repeater | B |
| 18 | site map and inventory | Target site map | B |
| 19 | DOM instrumentation | DOM Invader | C |
| 20 | OAST callbacks | Collaborator | C |

**Phase A, the workbench.** Features 1 to 7, 9, 13, 14, plus W9's JSON contract
and the plugin packaging of W21. The engine work is the message store, the
header field on `Fetch`, the replay path and the diff. This alone covers IDOR, authentication bypass, basic
injection, reflected XSS, SSRF against a named target, path traversal, header
and cookie manipulation, and a useful slice of business logic. The acceptance
test is a person driving an agent through easy and medium CTF web problems with
Bash and no Python.

**Phase B, the harder half.** Features 8, 10, 11, 12, 15 to 18. This is what
CSRF-protected multi-step flows, blind and time-based injection, race
conditions, upload chains, OAuth and session problems, and SSRF pivots need. The
RPC of W10 lands here, driven by whichever loop first proves too slow, and the
Python client lands after that, extracted from scripts that already exist rather
than designed in advance.

**Phase C, the long tail.** Features 19 and 20. Client-side and blind classes.

**The benchmark.** Each phase's claim is a measured one: a corpus of web CTF
problems, run by an agent with only these verbs, scored on solved and on how
many turns and requests it took. A feature that does not move that number is not
finished, whatever its tests say. The corpus belongs in `docs/benchmarks/`
beside the environment ones, with the same rule that the harness is committed.

## W21. Packaging: a plugin, not a part of the binary

`h5i websec` is not in the default build and is not a cargo feature someone
turns on at compile time. It is installed, after the fact, into an h5i that is
already on the machine:

```
h5i plugin install websec
h5i plugin list
h5i plugin remove websec
```

Nothing like this exists today. `h5i` has cargo features (`web`, `browser`,
`share`, `runner`, `ytdlp`, `identity`) and no runtime install path at all, so
W21 is the section that has to be built before phase A can ship the way this
file describes.

### What a plugin is

A separate executable, discovered by name the way `git` finds its subcommands.
`h5i plugin install websec` fetches `h5i-websec` for this platform into a
per-user plugin directory, and `h5i websec <verb>` execs it with the arguments
forwarded and the environment that names the session. A build without the
plugin still knows the name: `h5i websec` prints what it is and how to install
it, rather than an unknown-subcommand error, so the feature is discoverable
without being present.

Not a dynamically loaded library, and the reason is the product's whole claim.
The h5i process is the one that resolves policy, writes the receipt before the
bytes move and refuses the fetch when the record cannot be written. Code
`dlopen`ed into that process sits *inside* the boundary it is supposed to be
subject to, and every guarantee in `ROADMAP.md` would then rest on the good
behaviour of whatever was installed last.

A plugin as a separate process is subject to the boundary instead. The websec
plugin reaches a session over the same control channel `h5i browser snapshot`
uses, and it has no other route to the network. Its replays are the engine's
fetches, checked by the engine's policy, spent from the engine's budget and
written into the engine's receipts. An audit of a session driven by a plugin
reads exactly like an audit of a session driven by hand, which is the property
worth protecting.

### What still has to live in the binary

A plugin cannot add a field to `Fetch` or write the message store, so phase A's
engine work (W5, and the header map of W4) ships in the engine regardless.
Correctly, it is *capability without behaviour*: capture is off unless a session
is opened with `--capture`, and a build that has never had the plugin installed
behaves exactly as it does today.

The two halves negotiate through the verb that already exists for this.
`h5i browser capabilities` reports what an engine can do, as JSON, so h5i can
route by capability rather than by version number. The plugin reads it, and an
engine too old to capture gets a named refusal naming the build, not a confusing
empty result.

### Why opt in

Dependency weight is the small reason. The real one is posture. A browser for
agents that ships an HTTP attack workbench in every install is a different
product to defend, to a security review and to a user who wanted a page reader.
Making the workbench a deliberate act by the operator keeps the default install
honest about what it is, and costs an authorised user one command.

It is worth being precise about what this does not do: it is not a security
control. Anyone who can install a plugin can install any tool. What it buys is
that the capability is *named* at install time and visible in `h5i plugin list`,
which is the same stance the rest of h5i takes toward capability, and none of
the enforcement in W16 depends on it.

## What is deliberately not built

Each of these is a decision to be defended in review, not a gap to be filled.

- **A scanner.** No crawl-and-flag mode, no severity ratings.
- **Payload generation.** No SQL injection strings, no XSS vectors, no
  wordlists, no encoders beyond the ones an edit needs to be correct.
- **Vulnerability verdicts.** h5i reports differences. Calling one a
  vulnerability is the agent's claim, made in a finding it writes and signs.
- **An exploit database or plugin marketplace.**
- **An agent planner.** The loop belongs to whatever is driving h5i.

Parallel replay and match are not exceptions to this. Intruder is not a scanner
either: both are executors for conditions and payloads that arrive from outside.

## Open questions

1. **Capture default.** Off is the safe default and also the one that makes a
   fresh session useless for this workflow. A middle option is capture on for
   sessions created by `h5i websec` verbs and off for `h5i browser open`.
2. **Store lifetime across restarts.** Ids are stable per session; a session
   that is closed and reopened from a restored jar is arguably the same
   engagement and arguably not.
3. **Whether `websec` is a noun under `h5i` or a mode of `h5i browser`.** The
   verbs act on a browser session either way. `h5i websec` reads better in a
   script and keeps the browser's own verb list from doubling.
4. **How much of the diff belongs in the read IR.** The DOM-shape body mode
   wants the IR, and the IR was built for a different consumer.
5. **Whether plugins are signed, and how they are pinned.** A plugin speaks the
   `websec/1` schema against an engine that has to be new enough for it, so
   install needs a compatibility check, and a downloaded executable needs a
   provenance story that `install.sh` does not currently have.
