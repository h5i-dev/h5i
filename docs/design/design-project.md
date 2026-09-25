# Design: projects that outlive sessions, and reports built on them

Status: proposed, 2026-09-25. Branch `implement-project-report`.

```text
session (disposable)  ->  project (durable)          ->  report (issued, frozen)
  store, jar, log          notes, findings,               markdown the agent writes,
  session findings         evidence copies, checklists    resolved against the project
```

## P1. The problem

A finding lives in `<session>/findings/findings.jsonl`, beside the message store
it cites. `h5i browser rm` removes the whole session directory, so removing a
session also removes what was concluded in it. `--project` today is a label and a
scope file; nothing durable hangs off it.

"Professional red-teaming for everyone" needs the conclusions to be the lasting
asset, and needs a way to hand them to someone who does not read HTTP.

## P2. What h5i guarantees, and what it leaves to the agent

| h5i guarantees | the agent decides |
|---|---|
| notes, findings and evidence survive session removal | what is worth a finding |
| every cited evidence id exists in the project | how evidence is interpreted |
| dates, sources and versions are recorded | the report's structure and wording |
| an issued report is frozen and re-renderable | how uncertainty is explained |
| required checklist items cannot silently disappear | the plan and its order |

Templates are starting points, not forms. The agent may add, drop or merge
sections. Mechanical problems (a dangling reference, a required item with no
recorded outcome) are errors; everything about whether the prose is right is
advice.

## P3. Storage

`$H5I_PROJECT_HOME`, else `$XDG_DATA_HOME/h5i/projects`, else
`~/.local/share/h5i/projects`. Data, not config: losing it loses work. The
directory is owner-only (0700, files 0600), like the session store.

```text
<projects>/<name>/
  project.json          title, created, description
  notes.jsonl           append-only, folded by id
  findings.jsonl        append-only, folded by id (same fold as session findings)
  evidence/<E-n>.json   a copy of one message, credentials removed
  checklists/<slug>.md  the imported original, byte for byte
  checklists/<slug>.json items parsed from it, with mode and digest
  checklists/<slug>.jsonl outcomes recorded against items
  reports/draft.md      the working document the agent edits freely
  reports/<vN>/         an issued report: report.md, snapshot.json, report.html
  glossary.toml         optional project terms, over the built-in glossary
```

Project names follow the scope file rule (`scope.rs::validated`).

## P4. Findings

A project finding keeps the session finding's shape (title, free-text state,
notes, evidence, repro) and adds a few optional fields a report needs:
`severity`, `severity_reason`, `impact`, `remediation`, `status` (the fix
status, distinct from the agent's `state`), and `sources` (which session finding
it was promoted from). Severity, confidence (`state`) and fix status stay three
separate fields: "High but unconfirmed" and "Low but confirmed" are different
facts.

`h5i project finding promote <session-finding> --session S` copies a session
finding in, copies every message it cites into project evidence, and records the
source. Promoting the same session finding twice updates rather than duplicates.

## P5. Evidence

`h5i project evidence add req_42 --session S` reads the stored message and
writes `evidence/E-n.json` holding:

- the request and response heads, with `Cookie`, `Set-Cookie`, `Authorization`,
  `Proxy-Authorization` and any header named like a token replaced by
  `[removed]`;
- the body, capped at 64 KiB for display;
- the sha256 of the original message files, so the copy can be tied back to the
  store while it exists;
- session id, sequence, capture time, URL, and a caption.

The raw message is never copied by default. The display copy is what the console
and the report show.

## P6. Checklists

A checklist is imported Markdown: an internal standard, a client's list, last
time's notes. Every list item (`- [ ]`, `-`, `*`, `1.`) becomes an item with a
stable id (`C1`, `C2`…), and the nearest heading becomes its group. The original
text and its digest are kept.

Two modes:

- `reference`: context the agent may adopt, change or ignore.
- `required`: every item must end with a recorded outcome (`recorded`,
  `blocked`, `not-applicable`) and a note. Nothing constrains how.

Item states: `open`, `in-progress`, `recorded`, `blocked`, `not-applicable`.
"Nothing found" is recorded as a result under stated conditions, never as a claim
that the feature is safe. The console reports coverage as counts with the
denominator ("18 of 24 have an outcome; 4 open; 2 blocked"), never a score.

## P7. Reports

`reports/draft.md` is free Markdown. `h5i project report new` seeds it from a
template (the built-in "Security Assessment Report" or `--template FILE`). The
agent edits it with ordinary file tools. The report may embed project data with
directives h5i resolves at render time:

| directive | renders |
|---|---|
| `{{finding F-3}}` | the finding's full block: severity, status, impact, remediation, evidence |
| `{{findings}}` | a table of every finding |
| `{{evidence E-2}}` | the redacted message pair |
| `{{checklist SLUG}}` | the checklist's items and outcomes |
| `{{coverage}}` | coverage counts for every checklist |
| `{{term IDOR}}` | the term, linked to its glossary entry |
| `{{glossary}}` | every term used in the report, defined |

Counts and ids come from h5i, so the prose cannot drift from the data.

`h5i project report check` lints deterministically: unknown directives, dangling
ids, required checklist items without an outcome, findings with no evidence,
severity without a reason, no remediation. Errors exit 1; advice is printed and
exits 0.

`h5i project report issue` freezes the draft: the markdown, a snapshot of every
finding, evidence copy and checklist it references, and a rendered HTML page.
Later changes to a finding never change an issued report; a new issue is a new
version.

## P8. Rendering and PDF

Rust renders the Markdown (pulldown-cmark, raw HTML escaped, only `http`,
`https`, `mailto` and in-page links kept) into one HTML page with print CSS:
cover, table of contents, finding numbers, page breaks before each finding,
wrapped code and URLs. The console shows the same HTML, so the screen and the
PDF are drawn from one snapshot.

`h5i project report export --format html|pdf`. PDF goes through a local headless
Chromium (`--print-to-pdf`) when one is found; otherwise h5i writes the HTML and
says to print it from a browser. h5i's own engine does not print.

## P9. Glossary

A built-in glossary ships in the binary (`glossary.toml`, versioned), with a
plain-language definition for each term a non-specialist meets in a web security
report. A project `glossary.toml` adds or overrides terms. In the console a term
is a button that opens a definition panel (click or keyboard, not hover). In the
PDF, the first use of a term gets a footnote and the appendix lists every term
used.

## P10. Session removal

`h5i browser rm` on a session whose findings were not promoted into its project
lists them and refuses without `--force`. Removing a session never touches the
project.

## P11. Boxes

The project store is on the host and is not granted to boxes. Inside a box,
`h5i project` refuses with a message naming the host command. An agent in a box
keeps writing session findings; the host promotes them.

## P12. Phases

1. Store, notes, findings, evidence, promote, rm guard. Done when every finding
   and its evidence are readable after `h5i browser rm --all --force`.
2. Reports: template, directives, check, issue, HTML/PDF export, glossary,
   console Report tab.
3. Checklists: import, mark, coverage, required-item check.
4. Later: assessments (dated rounds within a project), diffs between issued
   reports, fix verification, issue tracker export.
