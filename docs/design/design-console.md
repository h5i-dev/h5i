# Design: the console (C1 to C12)

`h5i ui` is a read-only screen over what h5i is doing on this machine: boxes,
projects, and the browser sessions the workbench and recon act on. Every route
is a GET, behind the same gate as the rest of `server.rs`: a loopback `Host`, a
session token, and a matching `Origin` and `Sec-Fetch-Site`.

## C1. A session row

A row shows only what the session's own files say.

| shown | from |
|---|---|
| requests, refusals, origins reached | `requests.jsonl` |
| capture on, message count | `messages/` |
| ledger counts by state | `recon/ledger.jsonl`, folded through `h5i-wire` |
| runs that spent requests | `recon/jobs/*.json` |
| who holds the wheel | `control.json` |

The row also carries the record's `engine`, `confinement`, `end_reason`,
`permissive_cors`, `policy_digest`, `restored_from`, `enclosing_box`, and counts
of `findings` and `verbs`. The folds live in `crates/h5i-core/src/session_view.rs`.
The findings fold is the one `h5i websec finding` uses, so the two never
disagree.

## C2. Messages are shown, credentials masked

The inspector reads one fetch at a time from `/api/session/:id/message/:seq`,
never in the fleet list or overview.

- `Authorization`, `Proxy-Authorization`, `Cookie`, `Set-Cookie` and `X-Api-Key`
  come back as `*** (N bytes)`. The length stays: an empty header and a 900-byte
  JWT are different problems.
- Revealing is a second request with `?reveal=1`. Masking happens in the server
  (`message_view.rs`), so an unrevealed page never held the secret.
- Selecting another fetch starts masked again.

The store stays 0600 and is left out of an export unless named. Every row also
prints the CLI command that reads it, for use in a report.

## C3. Attention: five states

| state | shown as | fires when |
|---|---|---|
| `blocked` | waiting on you | a human holds the control lock, or a run stopped for a reason only a person can answer |
| `done` | finished, unread | the session or its last run ended and this client has not looked |
| `working` | working | live, with activity in the last minute |
| `idle` | idle | live and quiet |
| `unknown` | unclassified | the record says live and the engine's control file is gone |

Each state carries the clause that produced it in a `why` field, and the UI shows
it. There is no score. `blocked` is strict: a run that spent its allowance is
`done`; a login that expired mid-crawl is `blocked`. A state that fires on a
guess teaches the reader to ignore it.

## C4. Seen is per client

`done` is the only state that depends on the viewer. The server reports what is
true; each browser remembers what it has opened in `localStorage`. Nothing is
written back. `h5i box watch` follows the same rule for message read-state.

## C5. Boxes keep their own states

Box pressure is not an attention state. The attention bar counts sessions in the
five states and boxes as `refused egress` and `with failures`, each labelled.

## C6. Two scopes

Boxes belong to the repository the console was started in. Sessions belong to the
machine (the registry is in the user's state directory). The tab says which.

## C7. Poll cost

- The newest 120 sessions are read, live first (`MAX_SESSIONS_READ`). The rest
  are counted: "the newest 120 of 1727 recorded sessions".
- A row is cached against the size and mtime of the files it was folded from. An
  ended session is folded once; a live one refolds when one of its files changes.

Measured on 1,730 sessions: 90 ms for the first poll, 18 ms after.

## C8. Limits

No actions: the console cannot open, drive, crawl or replay, and buttons copy
commands instead of running them. No cross-machine fleet. No notifications.

## C9. Layout

A rail of five sections over a two-pane body:

| section | left pane | right pane |
|---|---|---|
| Overview | who wants a person, what is live, findings, boxes under pressure | |
| Projects | engagements, live first | one project: scope, origins, sessions |
| Sessions | the registry, loudest first, searchable | one session's workspace |
| Boxes | the fleet, most pressing first | one box's flight recorder |
| Host | what this machine can enforce | |

A session workspace has six tabs, each a fold of a file the session keeps:

| tab | from | adds |
|---|---|---|
| History | `requests.jsonl` joined to `actions.jsonl` | C10 filter, sorting, verb column, inspector |
| Sitemap | `requests.jsonl`, folded server-side | tree with counts folded upward, refusals apart |
| Actions | `actions.jsonl` | `h5i browser audit` as a timeline |
| Findings | `findings/findings.jsonl` | evidence ids beside each claim |
| Recon | `recon/ledger.jsonl`, `recon/jobs/` | state chips; calibration probes hidden by default |
| About | `session.json`, `control.json` | the record |

The join gives History a `verb` column: the inspector opens with "the agent asked
for `click @e3`". It works because the engine logs the decision before the
request goes out (design W2).

## C10. Filter language

Space-separated terms are ANDed, `a|b` inside a value is OR, a leading `-`
negates, and a bare word matches anywhere in the URL.

| term | matches |
|---|---|
| `method:GET\|POST` | whole word |
| `host:api` `path:/v1/` `query:id=` `url:…` | fragment, case-insensitive |
| `path:~"\.php$"` | regex, on any text field |
| `status:4xx` `status:>=500` `status:200\|404` | class, comparison, list |
| `ms:>1000` `ttfb:>200` `bytes:<100` `cookies:>0` `seq:>=100` | numbers |
| `initiator:navigation` `verb:click` `action:12` | provenance |
| `refused` `errors` `navigation` `replay` `is:pending` `is:cookies` | `is:` shorthands |

An unknown field is refused at parse time with a message under the input. Origin
chips are folded from the sitemap. Parser, matcher and tests are in
`web/src/filter.ts`; the query is kept per tab in `sessionStorage`.

## C11. The inspector

Selecting a fetch shows what the log knows: seq, timestamps, initiator, the verb
that spent it, the allow/refuse decision and reason, status, size, first-byte and
total time, cookies sent and stored. Below that it shows the masked message (C2)
and the commands `h5i websec show req_N`, `res_N`, `replay req_N`. A refused
fetch says nothing was sent; a session without `--capture` says the decision
record is all there is; a reclaimed store says when it went.

## C12. The registry past the fold

`/api/sessions` returns an `index` of every record (id, name, url, state,
timestamps), so search covers the whole registry while only 120 are folded. A hit
past the fold is folded when opened.
