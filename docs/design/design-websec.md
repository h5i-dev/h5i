# Design: the HTTP workbench, sections W1 to W23

`h5i websec` lets an agent read what a session sent, change part of a request,
send it again, and compare the answers. h5i owns the deterministic half: exact
capture, stable ids, cookie state, structured edits, resend, diff, timing,
scope, rate and audit. The agent owns the judgement: which request matters,
what to bend, what a difference means. Scanning, payload generation, wordlists
and vulnerability verdicts stay with the agent.

The workbench is a plugin (`h5i plugin install websec`, W21). The engine it
builds on is [`design-browser.md`](design-browser.md); discovery is
[`design-recon.md`](design-recon.md); the roadmap is
[`ROADMAP.md`](../ROADMAP.md).

## W1. The claim

> Every HTTP message a session's capture proxy forwarded or the session sent
> itself has a stable id, and `h5i websec replay <id>` re-sends that message,
> with the edits named on the command line and nothing else changed, through
> the same policy, cookie jar and receipts as the session.

h5i does not find vulnerabilities and does not promise TLS-level byte
identity. Connection reuse, client header order and current cookies may differ,
and replay reports what it changed so a caller can explain a divergent result.

## W2. Where the messages come from

A session is h5i's capture proxy (`h5i __capture-proxy`) plus a Chromium run
by agent-browser and pointed at it. The proxy writes a per-session CA (`0600`)
to decrypt HTTPS; agent-browser is launched with `--ignore-https-errors`. Every
request is checked against policy and recorded before it is forwarded, and
refused when the record cannot be written. Workbench sends (`resend`,
`sequence`, `socket`, `grpc`) go through the same broker, so browser traffic
and replays share one sequence and one timeline.

The receipt's `Initiator` separates the application from the tester:
proxy-observed browser traffic is `subresource`, workbench sends are `replay`.
The receipt lane is `proxy-observed`; agent-browser and Chromium are ordinary
host processes. Use a box when the browser itself must be confined.

## W3. Where the code lives

| piece | where |
|---|---|
| capture proxy | `src/cli/capture_proxy.rs` |
| broker, policy, budget, jar, receipts, message store | `crates/h5i-browser/src/` (`broker.rs`, `net.rs`, `policy.rs`, `budget.rs`, `cookies.rs`, `receipt.rs`, `capture.rs`) |
| edit language, multipart | `edits.rs`, `multipart.rs` |
| raw sends, WebSocket | `rawsock.rs`, `wsclient.rs` |
| receipt and message types, readers, triage | `crates/h5i-wire` |
| session verbs that send | `src/cli/browser.rs` (`requests`, `resend`, `sequence`, `socket`, `grpc`, `rpc`) |
| the plugin | `crates/h5i-websec` (`read.rs`, `view.rs`, `experiment.rs`, `matrix.rs`, `finding.rs`, `dom.rs`, `grpc.rs`, `nuclei.rs`) |
| plugin install and dispatch | `src/cli/plugin.rs` |

## W4. The send path

`Fetch` carries the caller's headers. The three framing headers
(`Content-Length`, `Transfer-Encoding`, `Connection`) are recomputed, and an
attempt to set them is refused and named in the receipt. Credentials stop at
an origin boundary. Edits (`edits.rs`) are applied in the broker process, and
`show` and `diff` read the store from disk on h5i's side rather than through a
session verb, so no stored `Authorization` header is relayed through the page
side.

## W5. The message store

Receipts and the message store are separate artifacts.

- Receipts are append-only JSONL with cookie counts, not values. They are safe
  to paste, ship in exports, and are fail-closed: no writable log, no fetch.
- The message store holds both directions of every message, headers and bodies,
  in `<session>/messages/`, mode `0600`: one JSON file per phase per receipt
  sequence number, bodies content-addressed under `bodies/<sha256>`. It is
  never in an export, share or bug report unless named, and
  `h5i browser close --capture-drop` deletes it.

Limits: 8 MiB per body, 512 MiB per session counting header sidecars as well as
bodies, and fonts, audio and video skipped. Truncated and skipped bodies carry
an explicit state. A full store refuses the new body (`store-full`) rather
than evicting old evidence. Writes are best-effort: a fetch is never refused
because the store could not be written, since the receipt still records it, and
failures are counted and reported.

Non-UTF-8 bodies keep their authoritative length and digest plus a lossy text
preview, so `match` and `diff` can read mixed content. `show --body-to PATH`
writes the exact bytes (`-` is stdout). All store content is untrusted target
input.

## W6. Ids

`req_<n>` and `res_<n>`, where `<n>` is the receipt `seq`, scoped to a session.
A request and its response share a number because they are two phases of one
receipt row. Verbs also accept a bare `42`.

The cross-session form `<session>/req_42` is written (recon's `merge` records
evidence that way) but not yet parsed by any verb; `show` takes a bare id plus
`--session`.

## W7. The command surface

Every verb takes `--session` and `--json`. Sending verbs live in the default
binary as `h5i browser`; reading and composing verbs live in the plugin.

```
h5i websec requests   [--method M] [--url-contains TEXT] [--status N] [--initiator K]
                      [--denied-only] [--limit N]
h5i websec show       <id> [--raw] [--body-to PATH|-]
h5i websec replay     <id> [--set T=V]... [--set-file T=PATH] [--unset T] [--create]
                           [--as SESSION [--keep-credentials]] [--repeat N [--race]]
                           [--no-follow] [--reset-budget] [--set-each T=PATH]
                           [--raw-target T | --raw-headers | --raw-request PATH|-] [--body | --raw]
h5i websec diff       <id> <id>
h5i websec match      <id>|--all [--regex RE] [--contains TEXT] [--json-path P[=V]]
                           [--header H[=V]] [--status N] [--longer-than B] [--shorter-than B]
h5i websec socket     <ws-url> [--send TEXT]... [--wait-ms MS]
h5i websec sitemap
h5i websec experiment <plan.json>
h5i websec matrix     <id> --as a,b,c
h5i websec sequence   <file.json> [--var K=V]... [--keep-going]
h5i websec finding    create|list|show|update
h5i websec grpc       describe|call
h5i websec dom        scan|node
h5i websec import-nuclei <template.yaml>
h5i browser rpc --stdio
```

`replay` always creates a new receipt and new ids, so a replay is itself
replayable. There is no implicit "last response"; every verb names its inputs.

`import-nuclei` sends nothing; it prints an `h5i test` file whose verdict is
data ([`design-flow-and-verdict.md`](design-flow-and-verdict.md)). `grpc`
compiles descriptors in the plugin and sends through `h5i browser grpc`, so
each call is policed, recorded and citable by id.

## W8. The edit language

```
--set path=/api/v2/users/456        --set query.user_id=456
--set header.X-Forwarded-For=1.2.3.4 --set cookie.session=<value>
--set json.user.role=admin          --set form.username=admin
--set body.raw=<bytes>              --set method=POST
--set-file multipart.avatar=./a.jpg --unset header.Origin
```

Rules:

- Edits apply in order, then `Content-Length` and multipart boundaries are
  recomputed. `Host` follows the URL unless set explicitly.
- Nothing is silently corrected. A JSON edit against a non-JSON body is an
  error naming the content type. An edit to an absent target is an error unless
  `--create` is given or that one target is prefixed with `+`
  (`--set +query.debug=1`); headers always upsert.
- An edit changes what it names and nothing else. `query.`, `form.` and
  `cookie.` edits rewrite one piece and leave the rest byte for byte (`?debug`
  stays `?debug`, `x[]=1` stays unencoded). `json.` reserialises the document:
  field order survives (`preserve_order`), whitespace does not, so a signed body
  needs `body.raw`.
- `json.` values are typed the way they read (`json.id=99` is a number); quote
  to insist on a string. When the encoded value differs from the text, the reply
  names it in `encoded`. An integer past `2^53` is refused, naming the quoted
  form and `body.raw`.
- Binary goes through `--set-file TARGET=PATH`, applied after every `--set`,
  bytes unaltered. A separate flag rather than an `@` prefix, because payloads
  often start with `@`.
- Multipart is parsed, edited and rebuilt, never patched as text.
  `multipart.<field>`, `.filename` and `.content_type` are separate targets.
  With `--create` an upload is built from nothing. The boundary is always the
  engine's own, so a part's data cannot split the message.

The reply echoes the edits as applied, so the log of an exploit is what was
sent rather than what was requested.

## W9. The JSON contract

- Text is the default output: the HTTP message under one line of ids, status,
  size and time, body cut at 4 KiB, control characters made inert.
  `--repeat`, `--race` and `--set-each` print one row per send. `--json` gives
  the envelope on every verb; `--human` is accepted and does nothing.
- An error under `--json` is `{"error": {"code", "message", "hint"}}` on stdout
  with a nonzero exit.
- Exit codes are few and borrowed. `match` uses grep's: 0 matched, 1 did not
  match, 2 could not look. A verb that cannot reach its session exits 69
  (`browser_session::EXIT_SESSION_GONE`). "No" and "could not ask" never share a
  code. Exit codes pass through the plugin dispatch unchanged.
- stdout carries results; progress and warnings go to stderr.
- Every payload carries `"schema": "websec/1"`. Fields are added, never
  repurposed; a removal is a new major.
- Time is RFC3339 with microseconds, matching `receipt.rs`.

## W10. Bash first, then RPC

Bash loops over `replay --json | jq` cover most work. They stop at adaptive
payloads, character-by-character blind extraction, timing statistics and
anything past a few hundred sends, where process startup dominates. On
XBEN-037-24, a 64-character flag through a one-bit oracle was 256 probes and 23
seconds, almost all of it startup; each request was 45 ms.

`h5i browser rpc --stdio` is the long-lived form: one JSON object per line in,
one per line out, ids matched, each request through the same `ask_session` a
typed verb uses. On 200 path probes against a local target it took 6.1 s
against 10.1 s spawning `h5i` per request. Each call still resolves the session
and opens the control channel; holding that connection open is not built.

A client library, if written, wraps that pipe and holds no security logic.

## W11. Sequences, cross-session replay, redirects

`websec sequence` (and `h5i browser sequence`) runs ordered `resend` steps with
named bindings between them:

```json
{"steps": [
  {"resend": 3, "extract": {"csrf": "regex:value=\"([^\"]+)\""}},
  {"resend": 5, "set": ["header.X-CSRF-Token=${csrf}", "json.role=admin"],
   "expect": {"all": [{"status": 200}, {"body": "regex:role.*admin"}]}}
]}
```

Bindings are typed by extractor prefix. A failed binding or step stops the run
unless `--keep-going` is given, because a step acting on a token the previous
step failed to produce is acting somewhere the file never described. A step's
`expect` that does not hold exits 1, distinct from 2 (could not look). Reaching
page state (logging in, clicking through) is done with the page verbs, which
drive Chromium; there is no recorded page-script replay.

`resend --as <session>` means the other session makes the request: its jar,
policy and receipts. The source's `Cookie`, `Authorization` and
`Proxy-Authorization` are dropped and reported in `credentials_dropped`.
`--keep-credentials` carries them, for the test where that is the question. A
replay of a stored request never takes cookies from the jar, so
`--unset header.cookie` stays unauthenticated.

`--no-follow` sets the request's hop limit to zero and reports the redirect
with its headers. Stopping where asked is not an error. The per-request limit
can lower the session's policy limit, never raise it.

## W12. Structural diff

`websec diff` returns layers an agent can branch on before reading:

1. Verdict fields: `same`, `status_changed`, `length_delta`, `similarity`
   (0.0 to 1.0, meaningful only when `bodies_compared` is true; an unstored body
   would otherwise compare as identical).
2. Header names added, removed and changed.
3. Body changes: keyed by dotted JSON path when both bodies are JSON, otherwise
   line-based. Lists are capped and the full count is reported beside them.

`similarity` is what a boolean-blind loop thresholds on instead of a model
reading two pages per candidate character. A DOM-shape body mode and a
`--body-mode` override are designed, not built.

## W13. Match

`websec match` takes `--regex`, `--contains`, `--json-path`, `--header`,
`--status`, `--longer-than` and `--shorter-than`, ANDs them, and returns each
condition's result and captures. Exit 0, 1 or 2 as W9 says. `--all` asks every
stored response and lists those that hold; exit 0 means at least one did. h5i
executes the caller's conditions and ships none of its own.

## W14. Timing

Each send reports `ttfb_ms` (to the response headers: the server's decision)
and `total_ms` (to the body in hand). Both come from the receipt the fetch
wrote, off the same start time. `connect_ms` and `tls_ms` are not reported:
they would need a custom connector, and a wall-clock guess is not a
measurement.

`--repeat N` runs inside the broker, since starting a process costs more than
the thing measured, and returns every sample with a median and median absolute
deviation. Each send is its own receipt. The count is clamped. A session in a
box pays a proxy hop, so comparisons within one session are sound and absolute
numbers across placements are not.

## W15. Parallel replay and rate

`--repeat N --race` starts N threads that meet at a barrier and then send.
It is a burst, not a single-packet attack: requests leave within the cost of
waking a thread. `scripts/websec/smoke.sh` redeems a one-use coupon twelve
times out of twenty against a server that sleeps between check and act. A
panicked sender fails the whole call, since a burst that sent fewer requests
than asked would make a race look absent.

Parallel sends spend from the same `budget.rs` limits as everything else.
`--rate R` on `resend` and `experiment` is measured from the last send and is
refused together with `--race`. Payloads come from the caller; h5i never
generates one.

## W16. Scope, policy and audit

Replay has no new path to the network. The session's policy decides, the
receipt is written first, and an off-scope replay is refused with a reason. A
replay cannot widen the allowlist; changing scope means a new session. Every
payload is in the timeline with the verb that sent it, so a session is
reproducible by someone who was not there.

## W17. Site map

`websec sitemap` folds the receipts into origins and endpoints: methods,
statuses, parameter names, hit counts, a navigation mark, and refused URLs
listed apart. It reports only what the session reached. Disclosed but
unvisited endpoints are recon's (`design-recon.md`), labelled as candidates.

## W18. DOM instrumentation

`websec dom` is the DOM Invader analogue. `h5i browser proxy <url>
--dom-instrument` makes the proxy inject an instrument into HTML pages, which
beacons source-to-sink reports back to the proxy (`dom-report.jsonl`; the
beacon is answered before policy and never stored). `dom scan` builds probe
URLs from a prototype-pollution corpus, opens them in the proxied Chromium and
folds what reached a sink into findings. `dom node` runs a Node target inside a
box once per candidate `Object.prototype` property and reports which reach a
dangerous sink; it refuses to run outside a box. The corpus is the one built-in
payload set in the workbench.

## W19. OAST

Not built. The design: `websec oast token` mints a correlation id and a
hostname to embed, `oast poll` returns interactions. Backends are the
operator's (own domain, webhook or self-hosted receiver; a local listener for
lab targets). Nothing goes to an h5i-operated service, because a payload URL in
a target is data about someone else's system.

## W20. Feature status and benchmarks

| # | feature | Burp analogue | status |
|---|---|---|---|
| 1 | full request and response capture | Proxy history, Logger | built |
| 2 | stable message id | Proxy history | built per session; qualified form not parsed |
| 3 | action to request provenance | Logger | partly: `replay` vs proxy-observed |
| 4 | history search and filter | Proxy history | built |
| 5 | raw and typed views | Message editor | built |
| 6, 7 | replay with structured edits | Repeater | built |
| 8 | multipart and upload editing | Repeater | built |
| 9 | multiple sessions and jars | session handling | built |
| 10 | replay as another session | session rules | built |
| 11, 12 | extract, bind, sequences | macros | built |
| 13 | response diff | Comparer | built, no DOM mode |
| 14 | match and extract | Intruder grep | built |
| 15 | timing | Repeater | built |
| 16 | parallel replay | race testing | built |
| 17 | redirect control | Repeater | built |
| 18 | site map | Target | observed half built |
| 19 | DOM instrumentation | DOM Invader | built (`dom`) |
| 20 | OAST | Collaborator | not built |
| 21 | multi-position experiments | Intruder | built |
| 22 | findings | none | built |

Benchmarks, run by an agent with these verbs; runners and notes are in the
separate `h5i-benchmark` repository:

| corpus | result |
|---|---|
| XBOW validation set | 100 of 104 |
| Argus | 54 of the first 60 (71 total) |
| Cybench web tasks | 8 of 8 |

These runs predate the move from the in-house engine to the capture proxy and
have not been repeated since. `scripts/websec/smoke.sh` drives every verb end
to end against `scripts/websec/server.py`; it covers the seams between CLI,
control channel, proxy and store that unit tests miss.

## W21. Packaging: a plugin

```
h5i plugin install websec --from <path>
h5i plugin list
h5i plugin remove websec
```

`h5i-websec` is a separate executable in its own release archive, copied into
a per-user plugin directory. `h5i websec <verb>` execs it with the arguments
and session environment forwarded. Installing by name or download is not
built: a downloaded executable needs a provenance story this does not have.
Only names on `plugin::KNOWN` install, so a plugin directory cannot turn
`h5i <anything>` into an execution. A build without the plugin still answers
`h5i websec` with what it is and how to install it.

The split: what sends stays in the binary (`requests`, `resend`, `sequence`,
`socket`, `grpc`, `rpc`), because it needs the session's control channel and a
plugin must have no other route to the network. What reads is in the plugin
(`show`, `diff`, `match`, `sitemap` and the rest), reading the store through
`crates/h5i-wire`, which holds the record and message types without linking
the engine. The old `h5i browser message|diff|match|sitemap` names are kept
hidden only to say where they went.

A plugin is a separate process, not a `dlopen`ed library. The h5i process
resolves policy and writes the receipt before bytes move; code loaded into it
would sit inside the boundary it is meant to be subject to. As a separate
process the plugin's sends are the engine's fetches, policed, budgeted and
recorded, and an audit reads the same as one of a hand-driven session.

Opt-in is posture, not a security control: anyone who can install a plugin can
install any tool. None of W16's enforcement depends on it.

## W22. Experiments

`websec experiment <plan.json>` is the multi-position form of
`replay --set-each`:

```json
{"request": "req_42",
 "positions": [
   {"name": "user", "target": "query.user", "values_file": "users.txt"},
   {"name": "role", "target": "json.role", "values": ["user", "admin"]}],
 "strategy": "product",
 "baseline": "res_42",
 "extract": {"error": "regex:SQL error: (\\w+)"},
 "rate": 4}
```

- `product` sends every combination; `zip` takes the nth value of each
  position. Relative paths are read beside the plan.
- The plugin expands the combinations and hands them to
  `h5i browser resend --walk -`; the engine sends. `--set-each` is the
  one-target case of the same path.
- The answer is clusters. The fold is `h5i-wire::triage` with
  `By::ShapeAndWords`: status, type, redirect target, size and what the body
  says, so "same length, different answer" stays apart. Every row names every
  message it folded.
- No transforms. Encodings beyond what an edit needs to be correct (W8) are the
  caller's, in the values.
- `"as": "other-session"` sends under another identity, and results are read
  from that session's store.
- An incomplete walk is not a negative result. The report counts `planned`,
  `answered` and `read`, carries the engine's error, and `ok` is true only when
  they agree. The usual cause is the 500-request allowance;
  `"reset_budget": true` raises it, off by default.
- The ceiling is 1000 sends per experiment. There is no concurrency option,
  which keeps receipts in request order; `--race` and `--rate` cover the rest.

`websec matrix req_42 --as anon,userB,admin` sends one request under each named
session, folds with `By::ShapeAndWords`, and reports a row per identity with
its class, and the identities in each class. Two identities in one class saw
the same answer; whether that is a missing boundary is the agent's call.
Identities are sessions the operator holds, each read from its own store, and a
refused identity is a row with its reason.

## W23. Findings

`websec finding create|list|show|update` keeps an append-only log beside the
message store: title, state, notes, cited message ids, and a repro file. Title,
state and repro replace; notes and evidence accumulate. Owner-only, never in an
export, dropped with the capture. To keep one, promote it into a project, which
copies the finding and the messages it cites with credentials removed.

- `--state` is free text, because nothing reads it.
- h5i asserts one thing: every cited message id is one this session holds. A
  finding with no real evidence is refused at write time. Everything else is
  the agent's claim.
- There is no `check` verb. Re-running a repro is `sequence` or `experiment`,
  and asking whether it still holds is `match`; the receipts already make a
  re-verification checkable.

## Raw requests

Parsed URLs normalise dot segments and percent-encoding, so
`--set path=/cgi-bin/.%2e/.%2e/etc/passwd` would send `/etc/passwd`; `apply`
rejects such edits. The raw path (`LocalBroker::send_raw`, sharing
`rawsock.rs` with the WebSocket client) sends HTTP/1.1 without `reqwest`:

- `--raw-target T` writes the request-target byte for byte and computes `Host`
  and `Content-Length`.
- `--raw-headers` sends the edited request with header names in the case given,
  keeping `--set` and cookies.
- `--raw-request PATH|-` sends a whole request, framing included, recomputed by
  nothing. The receipt records which framing invariants were broken. Bytes that
  follow the first response on the connection are kept (`trailing` in the
  stored message, `trailing_bytes` in the receipt), so a smuggled second
  response is visible. A dumped request uses CRLF, so it can be fed back.

All forms check policy, pin the address, claim the budget and write a receipt
before dialling. Proxied sessions allow raw requests only to loopback, as the
WebSocket client does.

## Not built, by decision

- A scanner: no crawl-and-flag mode, no severity ratings. Crawling lives in
  recon, which discovers and never flags.
- Payload generation: no injection strings or encoders beyond what an edit
  needs. Wordlists are input, never shipped. `dom scan`'s prototype-pollution
  corpus is the one exception (W18).
- Vulnerability verdicts. h5i reports differences; the agent signs a finding.
- An exploit database, a plugin marketplace, or an agent planner.


## Open questions

1. Store lifetime: whether a session reopened later is the same engagement.
2. Plugin signing and pinning: install needs a compatibility check against the
   engine and a provenance story for downloaded executables.
3. Whether the DOM-shape diff mode is worth building on the proxy design.
