# Design: the console, sections C1 to C12

Status: sessions and attention built 2026-09-07; the session workspace (C9 to
C12) built 2026-09-11. `h5i ui` is the read-only screen over everything h5i is
doing on this machine: boxes, and the browser sessions the workbench and recon
act on. `C` is the section prefix; `B`, `V`, `P`, `R`, `D`, `W` and `N` are
taken.

## In one screen

> A person running one agent reads its terminal. A person running twelve needs
> to be told which one wants them.

- **Attention is a state with evidence, never a score.** Five states, borrowed
  from herdr because the problem is herdr's, and each one carries the clause
  that produced it.
- **The console shows the account, not the evidence.** Receipts, ledger states,
  job records. Never a stored body, header or cookie: those are owner-only on
  disk, and reading them stays a command someone types.
- **It never writes.** Every route is a GET. Even "I have looked at this" is
  remembered in the browser, not sent back, because a passive view that writes
  is not a passive view.
- **It is bounded.** A registry grows without limit; a poll does not.

## C1. What it shows about a session

The row is what a session's own files say, and nothing inferred:

| shown | from |
|---|---|
| requests, refusals | `requests.jsonl`, both phases folded into one fetch |
| origins reached | the same log, parsed |
| capture on, and how many messages | the presence and contents of `messages/`, counted |
| ledger counts by state | `recon/ledger.jsonl`, folded through `h5i-wire` |
| runs that spent requests | `recon/jobs/*.json` |
| who holds the wheel | `control.json` |

Selecting one opens the workspace of C9: the history, the sitemap, the verbs,
the findings, the recon inventory with its job records, and the record itself.

## C2. What it refuses to show

The capture store holds bodies, cookies and `Authorization` in full. It is the
one artifact h5i keeps that is *not* safe to paste, which is why it is 0600, why
it is never in an export unless named, and why the console does not render it.

Every request row prints the command that reads it instead:
`h5i websec show req_42 --session <name>`. Clicking copies it. The same for an
endpoint's evidence and for resuming a job. The console teaches the next command
rather than performing it, which keeps a browser page from becoming a second
way to reach a credential.

## C3. Attention: five states

herdr's vocabulary, because the problem it solves is the one h5i now has: many
sessions, one person, and no useful way to know which wants them.

| state | shown as | fires when |
|---|---|---|
| `blocked` | waiting on you | a human holds the control lock, or a run stopped for a reason only a person can answer |
| `done` | finished, unread | the session ended, or its last run finished, and this client has not looked |
| `working` | working | live, and something happened in the last minute |
| `idle` | idle | live and quiet |
| `unknown` | unclassified | the record says live and the engine's control file is gone |

Two rules keep it honest. **Every state carries its evidence**, in the `why`
field, and the UI shows it: a badge that cannot say why it is amber is a score,
and this console does not score. **`blocked` is strict**: a run that spent the
allowance the operator gave it is a bounded run ending as asked, not a question,
so it is `done`; a login that went away mid-crawl is a question, so it is
`blocked`. herdr's reason applies exactly: a state that fires on a guess teaches
the reader to ignore it.

## C4. Seen is the client's, not the server's

`done` is the only state that depends on who is looking, so the server reports
what is true and each client remembers what it has read, in `localStorage`.
Opening a session clears its `done`; opening it in one browser does not clear it
in another. Nothing is written back.

This is not only tidiness. The console's whole posture is that it cannot change
what it watches, and a "seen" flag on disk would be the first write. It is also
the rule `h5i box watch` already follows for message read-state.

## C5. Boxes keep their own words

A box's pressure is not one of the five states: an egress refusal is the
boundary working, not somebody waiting. The attention bar counts sessions in the
five states and boxes in their own two (`refused egress`, `with failures`), in
one row, each labelled. Folding them into one vocabulary would be tidier and
would be a lie.

## C6. Two scopes, said out loud

Boxes belong to the repository the console was started in. Sessions belong to
the machine: `h5i browser open` needs no repository, and the registry lives in
the user's state directory. The tab says which, because a reader who assumes one
scope for both will misread an empty column.

## C7. What a poll costs

A registry of a thousand sessions is ordinary after a week of benchmarks, and
reading every one of their logs every eight seconds would make the console the
most expensive process on the machine. Two bounds:

- **The newest 120 are read**, live first. The rest are counted, and the column
  says so: "the newest 120 of 1727 recorded sessions".
- **A row is cached against a stamp** of the files it was folded from: the
  record, the request log, the ledger, the jobs directory, the message store and
  the control lock, each by size and mtime. An ended session is folded once and
  never again; a live one refolds exactly when something it is made of moved.

Measured on a registry of 1,730 sessions, 120 of them read: 90 ms for the first
poll and 18 ms for every one after it. Without the cache every poll is the first
one.

## C8. What is deliberately not built

- **No actions.** The console cannot open, drive, crawl or replay. Every route
  is a GET, and the buttons copy commands rather than run them.
- **No message rendering**, per C2.
- **No cross-machine fleet.** It shows this machine and this repository. A
  runner's boxes appear through the runner's own records, not by the console
  reaching out.
- **No notifications.** Attention is a state on a screen someone is looking at.
  A console that pushed would need a channel, a policy and a reason to be
  trusted with one.

## C9. The workspace: what a proxy reader expects, minus the bytes

Built 2026-09-11. A person auditing a session reaches for the shape every
HTTP workbench has taught them: a table of fetches, a tree of what was reached,
a place for what was concluded. Caido's pages were the checklist (HTTP History,
Sitemap, Scope, Findings, Logs); C2 was the constraint. What survives the
constraint is everything that is *about* a fetch, and the one column no proxy
can show.

The screen is a rail of four sections and a two-pane body:

| section | left pane | right pane |
|---|---|---|
| Overview | the board: who wants a person, what is live, findings, boxes under pressure | |
| Sessions | the registry, loudest first, searchable | one session's workspace |
| Boxes | the fleet, most pressing first | one box's flight recorder |
| Host | what this machine can enforce | |

A session's workspace has six tabs, and each one is a fold of a file the
session already keeps:

| tab | from | what it adds to the CLI's view |
|---|---|---|
| History | `requests.jsonl`, joined to `actions.jsonl` | the filter language of C10, sorting, the verb column, the inspector |
| Sitemap | `requests.jsonl`, folded server-side over the whole log | a tree with counts folded upward and refusals apart |
| Actions | `actions.jsonl` | `h5i browser audit` as a timeline, each verb with the receipts it spent |
| Findings | `findings/findings.jsonl` | the workbench's fold, with the evidence ids beside each claim |
| Recon | `recon/ledger.jsonl`, `recon/jobs/` | state chips, calibration probes hidden until asked for |
| About | `session.json`, `control.json` | the record, in the CLI's words |

The join is the point. A proxy sees a GET; the action log says which verb the
agent asked for while that GET was made, so the History table has a `verb`
column and the inspector opens with "the agent asked for `click @e3`". The
engine wrote the decision before the bytes moved, which is why the trace can be
drawn at all (design W2).

## C10. The filter language

One input above the history, shaped after HTTPQL and Burp's bar but flattened
to what fits on a line: space-separated terms are ANDed, `a|b` inside a value
is OR, a leading `-` negates, a bare word matches anywhere in the URL.

| term | matches |
|---|---|
| `method:GET\|POST` | whole word |
| `host:api` `path:/v1/` `query:id=` `url:…` | fragments, case-insensitive |
| `path:~"\.php$"` | a regular expression, on any text field |
| `status:4xx` `status:>=500` `status:200\|404` | class, comparison, or list |
| `ms:>1000` `ttfb:>200` `bytes:<100` `cookies:>0` `seq:>=100` | numbers, with `> >= < <=` |
| `initiator:navigation` `verb:click` `action:12` | the fetch's provenance |
| `refused` `errors` `navigation` `replay` `is:pending` `is:cookies` | shorthands for `is:` |

A field the language does not know is refused at parse time and said so under
the input, rather than matching nothing. Presets are chips that toggle a term;
the origin chips are the scope dropdown Caido puts above every table, folded
from the sitemap. The parser and matcher live in `web/src/filter.ts` with their
tests, and the query is remembered per session in `sessionStorage` (per tab,
never sent).

## C11. The inspector shows the account, not the evidence

C2 stands. Selecting a fetch opens a pane that carries everything the log knows
(seq, timestamps, initiator, the verb that spent it, the allowed/refused
decision with its reason, status, size, first-byte and total time, cookies
sent and stored) drawn as the chain of decisions that produced the fetch, and
then the commands that read the bytes: `h5i websec show req_N`, `res_N`,
`replay req_N`. Never a header, cookie or body. A refused fetch says what
refused it and that nothing was sent; a session opened without `--capture`
says the decision record is all there is; a reclaimed store says when it went.

## C12. The registry past the fold

C7's bound stands: the newest 120 sessions are folded, the rest counted. What
changed is that the rest can now be found. `/api/sessions` returns an `index`
of every record (id, name, url, state, timestamps: the fields `bs::list` has
already read to count them), so the column's search covers the whole registry.
A hit past the fold is shown as a stub and folded on demand when opened. A
machine that has run two thousand benchmark sessions can still open the one
named last week without the poll reading two thousand logs.

## What the CLI gained with it

`/api/session/:id` grew `requests_total`, `actions`, `findings_list` and
`sitemap`; the row grew the record's `engine`, `confinement`, `end_reason`,
`permissive_cors`, `policy_digest`, `restored_from`, `enclosing_box` and counts
of `findings` and `verbs`. The folds live in `session_view.rs` beside the ones
that were there, and the findings fold is the same one `h5i websec finding`
makes so the two never disagree about what a finding currently says.

