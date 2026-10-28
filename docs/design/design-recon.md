# Design: reconnaissance and the endpoint ledger, sections N1 to N21

`h5i recon` finds what an application exposes, records where each candidate
came from, and hands a confirmed request to the workbench
([`design-websec.md`](design-websec.md)). It is a plugin
(`h5i plugin install recon`), built in `crates/h5i-recon`. Section ids are
cited from code.

The rules the rest of the file follows:

- Discovery and confirmation are different states. A URL scraped from a bundle
  was not visited; a URL that answered was observed; a URL that differs from
  the target's not-found baseline is confirmed.
- Recon collects, websec verifies. Payloads, authorisation comparison and
  callbacks are websec's.
- No HTTP client of its own. Every request goes through the session's engine,
  so policy decides, the budget pays and the receipt is written first.
- Recon writes its own parsers and runs no third-party tool (N19).
- Nothing is flagged. Calling something a vulnerability is the agent's claim.

## N1. The claim

> Every endpoint recon reports carries the source that disclosed it, the
> identity that observed it, and the message id of the request that observed
> it, or a state saying no request was sent. A row with no request id is a
> candidate, and the ledger prints it as nothing else.

`Observation::is_keepable` in `crates/h5i-wire/src/ledger.rs` enforces the second
half: a state that means a request happened must name its message. Recon does
not claim completeness. A crawl bounded by depth, budget and rate misses
things, and URLs generated at runtime cannot be enumerated from bundles.

## N2. The join is a message id

Recon's output is an inventory: it grows, is read by cursor, and is worth
keeping. Websec's output is an experiment. They join at one place: a ledger row
names a `req_<n>` from the message store, and `h5i websec replay req_<n>` picks
it up with the jar and policy that produced it. Recon does not send payloads
and websec does not crawl.

## N3. What it reuses

| piece | where |
|---|---|
| message store and receipts | `crates/h5i-browser/src/capture.rs`, `receipt.rs` (websec W5) |
| scope and limits | `policy.rs`, `budget.rs` |
| sending | `h5i browser resend` and `h5i browser rpc --stdio` |
| receipt fold into origins and endpoints | `websec sitemap` (W17) |
| clustering | `h5i-wire::triage`, shared with websec experiments (W22) |

## N4. Crates

`crates/h5i-recon` is the binary and its library. It links `crates/h5i-wire`,
which holds the receipt row, the stored message, the ledger row and the triage
fold, and does not link the engine, so the plugin reads a ledger and a message
store in a 2.4 MB binary. It opens no socket. Anything it sends runs an
`h5i browser` verb in a subprocess: one `h5i browser rpc --stdio` for the whole
run when the engine speaks it, otherwise one process per request. Ledger file
IO stays in recon.

## N5. The endpoint ledger

One append-only JSONL file per session at `<session>/recon/ledger.jsonl`, mode
`0600`, with a derived index. Append-only because resume, provenance and "what
is new since the last turn" are reads of the same log, and a crashed run keeps
what it found. Never in an export, share or bug report unless named, and
dropped with the capture on close, because candidate URLs are the target's
data and can carry a session token. Every value in it came from the target and
is untrusted.

| field | meaning |
|---|---|
| `id` | `ep_<hash>` over the key, stable across runs |
| `origin`, `path`, `method` | the endpoint |
| `params` | parameter names, with where each lived: query, form, json, header, cookie |
| `identity` | the identity that observed it, or `anonymous` |
| `state` | `candidate`, `observed`, `confirmed`, `refused`, `gone` |
| `source` | how it was learned |
| `evidence` | `req_<n>` ids that observed it, most recent last |
| `cluster` | the triage cluster, when triage ran |
| `first_seen`, `last_seen` | RFC3339 with microseconds |
| `notes` | agent-written, never tool-written |

The key is `(origin, path, method, identity)`. Identity is in the key so the
ledger can say whether an endpoint answers differently when logged in.

States:

- `candidate`: disclosed, never sent (a bundle string, an unsubmitted form, an
  untried wordlist entry, an imported URL).
- `observed`: a request answered; carries a `req_<n>`.
- `confirmed`: observed and distinguished from the directory's not-found
  baseline by N11. The only state that means "this exists".
- `refused`: policy declined it. Kept, because it is a fact about the scope.
- `gone`: confirmed once, now answering like a missing path. Only a confirmed
  row can become `gone`.

Sources are strings: `page:<req_n>`, `script:<req_n>`, `json:<req_n>`,
`header:<req_n>`, `known-file:<req_n>`, `wordlist:<name>`,
`openapi:<req_n|path>`, `import:<tool>`, `calibration` (N11's deliberate
not-found probes), `manual`.

Receipts are numbered from zero, so the fold cursor is an `Option<u64>`; a zero
meaning "nothing yet" once dropped every session's first request.

## N6. Ids and the handoff

Websec ids stay as W6 defines them. Recon adds `ep_<hash>` and no other
numbering.

```bash
h5i recon endpoints --state confirmed --json |
  jq -r '.endpoints[] | select(.params | index("id")) | .evidence[-1]' |
  xargs -I{} h5i websec replay {} --set query.id=1 --json
```

A ledger row is a name for a place plus evidence of the last visit. Anything
that sends bytes at it hands that evidence to websec.

## N7. The command surface

Every verb takes `--session` and `--json` and names its inputs.

```
h5i recon endpoints [--state S] [--origin O] [--identity I] [--since CURSOR]
h5i recon show      <ep_hash>
h5i recon extract   [--from req_42] [--kind html,js,json,headers,secrets]
h5i recon known     [--origin O]
h5i recon crawl     [--seed URL]... [--depth N] [--max-requests N] [--per-shape N]
                    [--rate R] [--check-login-every N] [--reset-budget]
h5i recon paths     [--wordlist PATH] [--reuse-words] [--under DIR]... [--extensions LIST]
                    [--backups] [--max-requests N] [--rate R] [--origin O]
                    [--reset-budget] [--stream]
h5i recon triage    [--calibrate] [--probes N]
h5i recon import    --format urls|katana|subfinder|httpx|openapi PATH [--origin O]
h5i recon export    [--state S]
h5i recon merge     --from SESSION
h5i recon jobs      list | show [id] | resume [id]
```

`extract` sends nothing, so it can run freely and keeps working when the
budget is spent. Sending verbs use `h5i browser resend --raw-target <path>`.

## N8. Candidate sources

Readers write `candidate` rows and send nothing; `known` fetches a handful of
well-known paths.

- HTML (`extract.rs`): links, form actions with method and input names, `link`,
  `script`, `img`, `iframe`, `srcset`. Relative URLs resolve against the
  response's final URL.
- Headers: `Location`, `Link`, `Refresh`, CSP `report-uri`.
- JSON bodies: URL-shaped strings.
- JavaScript (`js.rs`): a token scanner, not a regex sweep and not a full parse.
  It knows comments, strings and template literals. A concatenated path such as
  `"/api/" + id` is reported as a `Partial` and never written to the ledger.
- Known files (`known.rs`): `robots.txt`, `sitemap.xml` and its index chain,
  `security.txt`, `.well-known/openid-configuration`. `robots.txt` gives
  candidates, not permission; scope comes from policy.
- Secrets (`secrets.rs`, `--kind secrets`, off by default): credentials, keys,
  connection strings and sourcemaps, by regex over every body. Reported in a
  `secrets` array beside the ledger, never into it. Values are redacted.
  Prefix rules (AWS, Google, Slack, GitHub, Stripe, JWT, PEM) trust the shape;
  the label rule is gated on entropy. JWTs fold by header with a count, and
  publishable tokens (Mapbox `pk.`, Stripe `pk_`) are listed under
  `publishable`.

Every row records the `req_<n>` it was read from.

## N9. Authenticated crawl

`crawl.rs` walks ledger candidates under the session's identity, so there is
no cookie to copy.

- Seeds: the ledger's candidates plus any `--seed`.
- Bounds: depth, total requests, rate, all on the command line.
- Cycle detection on the normalised URL, with a per-path-shape cap
  (`--per-shape`) so a calendar of per-day URLs does not consume the run.
- Login loss: every `--check-login-every` requests it re-probes the first page
  the walk visited, and stops with a named state when the answer changes.
  Re-login is not automated; the operator or agent logs in again.
- GET only. A disclosed `POST` endpoint stays a candidate; submitting a form is
  a page action.

Two identities crawling one target produce two sets of rows (N5's key).

## N10. Path discovery

`paths.rs` asks for paths the application never disclosed.

- h5i ships no wordlist. `--wordlist` takes the operator's;
  `--reuse-words` adds words the session has already seen.
- Generation is mechanical: `--extensions`, `--backups` (`.bak`, `~`, `.old`,
  ...), each directory given with `--under`, and both `/admin` and `/admin/`.
- Recursion into confirmed directories is designed, not built.

A 200 from a target that answers 200 for everything is `observed`. Nothing is
`confirmed` until N11 has run.

## N11. Triage: calibration and clustering

Calibrate (`triage --calibrate`): per directory, send `--probes` random paths
that cannot exist and record the baseline. A response matching it is
not-found-like whatever its status. Baselines are persisted beside the
ledger's cursor. A baseline whose probes disagree with each other is unstable
and confirms nothing.

Cluster: the fold is `h5i-wire::triage` with `By::Shape`, which groups a
template's renderings together. Each cluster has a representative, a count
and its outliers. A cluster row points at `req_<n>` ids and every message
stays in the store, so the summary is a view over the evidence, not a
replacement. A five thousand request run comes back as tens of rows.

Checked against a server that answers 200 with its own template for every path:
three invented paths folded into one not-found cluster, two real pages
confirmed.

## N12. Job control

- Budget: `--max-requests` bounds a run. `--reset-budget` restarts the page's
  500-request allowance before each send; it is a flag because raising the
  ceiling should be said out loud.
- Rate: `--rate`, default 4 per second.
- Observations are written in batches of 25, so a killed run keeps what it
  found. There is no `jobs stop`; a run is a foreground process.
- `jobs list`, `jobs show` and `jobs resume`. The job file at
  `<session>/recon/jobs/<id>.json` holds the parameters, so a resume runs the
  same job and skips what the ledger already answered.
- Progress goes to stderr, results to stdout (W9).

## N13. Scope and policy

Recon inherits websec W16. Every send goes through the engine, so an off-scope
candidate is refused with a reason and recorded as `refused`. Recon cannot
widen an allowlist; that takes a new session. Discovery is the verb most
likely to wander, so a large refused count is reported rather than left as a
silent gap; the operator decides whether the scope was too narrow.

## N14. What recon returns

1. A cursor diff: `endpoints --since <cursor>` returns what is new and a new
   cursor.
2. A cluster summary (N11).
3. A row: `show <ep_hash>` with sources, evidence and per-identity
   observations.

`"schema": "recon/1"`, errors as `{"error": {...}}` on stdout with a nonzero
exit, exit 69 when the session is gone. All per W9.

## N15. Benchmark

`scripts/recon/bench/run.sh`, results in
[`docs/benchmarks/recon.md`](../benchmarks/recon.md): four targets that declare
what they have, including one that answers 200 for every path and one where
everything real is behind the wordlist. Score: 20 of 20 endpoints confirmed,
no false confirmations, scored also on requests per confirmed endpoint. A
corpus of real applications is not built.

`scripts/recon/smoke.sh` drives every verb against `scripts/websec/server.py`,
whose `/site/` tree includes a directory that answers 200 with its own "not
found" page.

The acceptance path is one run: log in, crawl under that identity, discover
paths, triage to clusters, pick a confirmed endpoint, hand its `req_<n>` to
`websec replay`, with every request in the session's receipts.

## N16. Phase 2, not built

| feature | design |
|---|---|
| hidden parameter discovery | candidate names in batches, differencing against a calibrated baseline, each hit re-verified alone |
| API definition gaps | what an imported OpenAPI definition promises that the crawl never saw |
| service and technology probe | status, title, redirects, TLS, headers, guesses; every guess carries the evidence that produced it |
| virtual host discovery | `Host` candidates against a pinned address; connect address, SNI and `Host` kept as separate fields |

## N17. The perimeter, by import

`recon import` reads a file the operator produced (`urls` for gau or
waybackurls, `katana`, `subfinder`, `httpx`, `openapi`), writes rows with
`source: import:<tool>`, and confirms nothing. It never records what another
tool saw: an httpx row carries its URL, not its status. OpenAPI is JSON only;
converting YAML is the operator's job (N19). Native passive collection, DNS
resolution and port probing are not built and may never be.

## N18. What belongs to websec

| capability | why |
|---|---|
| fuzzing at named positions | verification of a known request: `replay` and `experiment` |
| authorisation comparison | a payload-free experiment on a known request (`matrix`) |
| passive response rules | read stored messages, belong beside `match` |
| out-of-band callbacks | W19 |
| template-driven checks | `websec import-nuclei` |

## N19. Own the code that touches the target

Extraction, path generation and clustering are written here in Rust against
crates in the workspace. Recon does not run katana, ffuf, feroxbuster, jsluice,
arjun or httpx, and calls no service that does.

1. The code sits where hostile bytes arrive. A bug in a borrowed parser is a
   bug in a process holding the session's cookie jar.
2. A borrowed tool brings its own HTTP client, which misses policy, budget and
   receipts.
3. A binary on `PATH` is an unversioned dependency outside `Cargo.lock`.

Import is a file, not an execution: h5i does not spawn the tool or fetch its
output, reads the file with a size cap as untrusted input, and lands every row
as a `candidate`. The same reasoning keeps parsers small: YAML and a full XML
parser are not carried.

## N20. Boundary with websec's refusals

Websec refuses a crawl-and-flag scanner and shipped wordlists. Recon crawls
and takes a wordlist, within those lines: it flags nothing, and wordlists are
input, never shipped.

## N21. Not built, by decision

- Verdicts or severities. `confirmed` means distinguishable from not-found, not
  interesting.
- Bundled wordlists, payload corpora, or fingerprint databases without
  provenance.
- A hosted service. Nothing about a target leaves the machine.
- Execution of third-party security tools (N19).
- Crawling outside the session policy. There is no `--all-origins`.
- An agent planner beyond the frontier rules.
- A second HTTP client, in the plugin or anywhere else.
