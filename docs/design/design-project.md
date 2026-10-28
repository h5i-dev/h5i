# Design: projects and reports

A project is the durable part of an engagement. Sessions are disposable; their
conclusions are promoted into a project, and reports are built from the project.

```text
session (disposable)  ->  project (durable)          ->  report (issued, frozen)
  store, jar, log          notes, findings,               markdown the agent writes,
  session findings         evidence copies, checklists    resolved against the project
```

Code: `crates/h5i-core/src/project/`, `src/cli/project.rs`, console Projects tab
(`web/src/Projects.tsx`).

## P1. Scope

A session finding lives beside the message store it cites, and `h5i browser rm`
removes both. A project keeps findings, evidence and notes after session removal
and turns them into a report a non-specialist can read.

## P2. What h5i guarantees, and what it leaves to the agent

| h5i guarantees | the agent decides |
|---|---|
| notes, findings and evidence survive session removal | what is worth a finding |
| every cited evidence id exists in the project | how evidence is interpreted |
| dates, sources and versions are recorded | the report's structure and wording |
| an issued report is frozen and re-renderable | how uncertainty is explained |
| required checklist items cannot silently disappear | the plan and its order |

Templates are starting points. Mechanical problems (a dangling reference, a
required item with no outcome) are errors; judgements about the prose are advice.

## P3. Storage

`$H5I_PROJECT_HOME`, else `$XDG_DATA_HOME/h5i/projects`, else
`~/.local/share/h5i/projects`. Owner-only (0700, files 0600). Names follow the
scope file rule (`scope.rs::validated`).

```text
<projects>/<name>/
  project.json            title, created, description
  notes.jsonl             append-only, folded by id
  findings.jsonl          append-only, folded by id (same fold as session findings)
  evidence/<E-n>.json     a copy of one message, credentials removed
  checklists/<slug>.md    the imported original, byte for byte
  checklists/<slug>.json  parsed items, with mode and digest
  checklists/<slug>.jsonl outcomes recorded against items
  reports/draft.md        the working draft
  reports/<vN>/           an issued report: report.md, snapshot.json, report.html
  glossary.toml           optional project terms
```

## P4. Findings

A project finding has the session finding's shape (title, free-text `state`,
notes, evidence, repro) plus optional `severity`, `severity_reason`, `impact`,
`remediation`, `status` (fix status) and `sources` (the session finding it came
from). Severity, confidence (`state`) and fix status are separate fields.

`h5i project finding promote <session-finding> --session S` copies the finding
and every message it cites into the project and records the source. Promoting
twice updates rather than duplicates.

## P5. Evidence

`h5i project evidence add req_42 --session S` writes `evidence/E-n.json`:

- request and response heads, with `Cookie`, `Set-Cookie`, `Authorization`,
  `Proxy-Authorization` and token-like headers replaced by `[removed]`;
- the body, capped at 64 KiB;
- sha256 of the original message files;
- session id, sequence, capture time, URL, caption.

The raw message is not copied by default.

## P6. Checklists

`h5i project checklist import` reads Markdown. Each list item (`- [ ]`, `-`, `*`,
`1.`) gets a stable id (`C1`, `C2`, …) and its nearest heading as group. The
original text and digest are kept.

- `reference`: context the agent may adopt, change or ignore.
- `required`: every item must end `recorded`, `blocked` or `not-applicable`, with
  a note.

Item states: `open`, `in-progress`, `recorded`, `blocked`, `not-applicable`.
"Nothing found" is a result under stated conditions, not a claim of safety.
Coverage is shown as counts with the denominator ("18 of 24 have an outcome; 4
open; 2 blocked"), never a score.

## P7. Reports

`h5i project report new` seeds `reports/draft.md` from the built-in "Security
Assessment Report" template or `--template FILE`. The agent edits it with
ordinary tools. Directives resolved at render time:

| directive | renders |
|---|---|
| `{{finding F-3}}` | severity, status, impact, remediation, evidence |
| `{{findings}}` | a table of every finding |
| `{{evidence E-2}}` | the redacted message pair |
| `{{checklist SLUG}}` | items and outcomes |
| `{{coverage}}` | coverage for every checklist |
| `{{term IDOR}}` | the term, linked to its glossary entry |
| `{{glossary}}` | every term used, defined |

`h5i project report check` lints: unknown directives, dangling ids, required
items without an outcome, findings without evidence, severity without a reason,
no remediation. Errors exit 1; advice exits 0.

`h5i project report issue` freezes the markdown, a snapshot of every referenced
finding, evidence copy and checklist, and the rendered HTML. Later edits never
change an issued report; reissuing makes a new version.

## P8. Rendering and PDF

Rust renders the Markdown with pulldown-cmark (raw HTML escaped; only `http`,
`https`, `mailto` and in-page links kept) into one HTML page with print CSS. The
console shows the same HTML.

`h5i project report export --format html|pdf`. PDF uses a local headless Chromium
(`--print-to-pdf`) when found; otherwise h5i writes HTML and says to print it.

## P9. Glossary

A built-in `glossary.toml` ships in the binary; a project `glossary.toml` adds or
overrides terms. In the console a term is a button that opens a definition panel.
In the PDF the first use gets a footnote and an appendix lists every term used.

## P10. Session removal

`h5i browser rm` on a session with unpromoted findings lists them and refuses
without `--force`. Removing a session never touches the project.

## P11. Boxes

The project store is host-only and not granted to boxes. Inside a box,
`h5i project` refuses and names the host command. An agent in a box writes
session findings; the host promotes them.

## P12. Not built

Assessments (dated rounds within a project), diffs between issued reports, fix
verification, issue tracker export.
