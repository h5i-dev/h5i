# Projects and reports

A browser session is disposable, and a session finding goes with it. A project is the durable side of an engagement: notes, findings, the evidence they rest on, the checklists you were asked to cover, and the reports issued from them. It lives on the host, owner-only, and nothing under `h5i browser rm` removes it. Use it when the work should become something a person keeps.

```bash
h5i project init acme --title "ACME web" --target https://acme.test
h5i browser open https://acme.test --project acme --capture
# ... test, write session findings, then keep them:
h5i project finding promote --all -p acme --session <name>
```

`promote` copies each session finding and the messages it cites into the project as evidence, with `Authorization`, `Cookie`, secret-looking query parameters and JSON or form fields replaced by `[removed]`. The raw capture is never copied. Promoting the same finding again updates it rather than duplicating. `h5i browser rm` refuses to remove a session whose findings are not saved to a project yet, and names the promote command, so a cleanup does not lose work.

Tie the work to the project as you go, or the link is never recorded and the console shows the project with no sessions. Two ways create the link: open the session with `--project <name>` (above), and build the project's findings from the session — `h5i project finding promote --session S` or `h5i project evidence add req_N --session S` — rather than hand-writing them with `finding create` / `evidence add-text`. A hand-written finding has no session behind it. When you did capture the work in a session, promote it; keep `finding create` for a conclusion that has no single session (a design note, a cross-cutting observation).

## Findings

Before a finding enters a report, rule out a false positive. Assume the first result is one: re-run it, try the same request unauthenticated, and check it is the target's doing, not a cache, a redirect, an error page, or state you carried in. Report a vulnerability only with a complete proof of concept: the requests that reproduce it and the response that shows impact. No complete PoC, no confirmed vulnerability. Rate it `info` at most and say what is missing. An honest `info` beats a high that is disproved in a minute.

A project finding keeps severity, your confidence, and the fix status as three separate fields, because they answer different questions. Severity and status are small fixed sets; `--state` (confidence) is free text you own.

```bash
h5i project finding create -p acme --title "Users list readable by any account" \
    --severity high --severity-reason "any authenticated user reads all users" \
    --summary "A normal login can read the full user list." \
    --remediation "Add a role check on /admin/users." --evidence E-1
h5i project finding update -p acme F-1 --status fixed-verified --note "retested, 403 now"
```

`--severity` is one of critical, high, medium, low, info. `--status` is one of open, in-progress, fix-claimed, fixed-verified, risk-accepted. h5i refuses evidence the project does not hold, so an `--evidence E-2` that names nothing is an error, not a dangling reference.

Evidence can also be a screenshot or a log excerpt, not only a captured message:

```bash
h5i project evidence add req_42 -p acme --session <name> --caption "the leak"
h5i project evidence add-file -p acme shot.png --caption "admin panel, as a normal user"
h5i project evidence add-text -p acme "..." --caption "server error with a stack trace"
```

## Checklists

A checklist is any Markdown list you import: an internal standard, a client's list, last round's notes. Its items become tracked. A `--required` list has to end each item with an outcome (`recorded`, `blocked`, `not-applicable`) and a reason; an item left open stays visible rather than passing as done. Coverage is reported as counts, never a score.

```bash
h5i project checklist import -p acme checks.md --required
h5i project checklist mark -p acme C1 --status recorded --note "role check present" --link F-1
h5i project checklist coverage -p acme
```

"Nothing found" is recorded as a result under stated conditions, never as a claim that a feature is safe. A checklist tracks what you were asked to look at; it does not turn the report into a pass or fail.

## The report

The report is a free Markdown document you write. h5i fills the counts, ids and evidence through directives it resolves against the project, which keeps the numbers in your prose matched to the data. You keep control of the structure and the words.

| directive | renders |
|---|---|
| `{{finding F-3}}` | the finding in full: severity, status, impact, remediation, evidence |
| `{{findings}}` | a table of every finding |
| `{{evidence E-2}}` | the redacted request and response |
| `{{coverage}}` | coverage counts for every checklist |
| `{{checklist SLUG}}` | one checklist's items and outcomes |
| `{{term IDOR}}` | mark a term for the glossary and a definition panel |
| `{{glossary}}` | define every term the report used |

```bash
h5i project report new -p acme            # a starting template, not a form
h5i project report set -p acme report.md  # or pipe the draft on stdin
h5i project report check -p acme          # dangling ids and missing outcomes are errors
h5i project report issue -p acme          # freeze it as a versioned snapshot
h5i project report export -p acme --version 1 --format pdf
```

`check` is deterministic. An unknown directive, an id that names nothing, and a required checklist item with no outcome are errors that block `issue`. A finding with no evidence, no severity reason, or no remediation is advice you weigh. Fix the errors; judge the advice.

`issue` freezes the draft with a snapshot of everything it referenced, so a later change to a finding never rewrites a report already handed over. A new issue is a new version. PDF export uses a local Chromium when one is found (`$H5I_CHROME` names it); otherwise export `--format html` and print from a browser.

The same report renders in `h5i ui` under the project's Report tab, where a marked term is a button that opens its plain-language definition with a link to read more. `h5i project glossary <term>` prints a definition on the command line, and a project's own `glossary.toml` adds or overrides terms.

Use `h5i project <command> --help` rather than guessing flags. The vulnerability judgment and the report's claims are yours; h5i keeps the record and checks that the ids and counts are consistent.
