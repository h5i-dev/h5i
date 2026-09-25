# Design: flows and verdicts, one replay core with two owners

Status: proposed, 2026-09-24. This does not add a third engine. It names the line
between the two multi-step engines h5i ships (`websec sequence` and `h5i test`)
and adds one seam: a declarative verdict that is data, sits below the bash oracle,
and is the only form a shared or imported recipe may carry.

```text
one flow language  ->  three verdict sources  ->  two owners
  (send + edit +        none  | matcher |          agent (live)
   extract, shared)      (data)  oracle (code)      repository (CI)
```

## F1. What is claimed

h5i has two ways to run an ordered, stateful HTTP flow:

- `websec sequence` (design-websec.md W11): live session, binds a value from one
  response into the next, stops on a failed binding. No built-in verdict; the
  agent reads the result and writes a `finding`.
- `h5i test` (design-test.md): replays a repository-owned flow and hands the
  evidence to an external oracle, which returns pass/fail/error by exit code. It
  has no assertion language on purpose.

The recurring questions were: should `sequence` get a stateful assertion DSL, and
should a shared recipe carry bash to decide its own verdict. The answer is to
separate the flow language (one thing, shared) from the verdict (three sources
with different trust models). Which source a flow may use is the whole decision.

## F2. Who owns the verdict

The dividing line is not "interactive vs CI"; it is who may say pass or fail, and
what that makes the artifact.

| | `websec sequence` | `h5i test` |
|---|---|---|
| verdict owner | the agent, after the fact | the repository, via oracle |
| verdict form | a `finding` | oracle exit code |
| artifact is | data (the flow) | code (the oracle) plus data |
| trust model | this session only | runs the repo's own program |
| shareable | yes | no, it carries executable code |
| home | live exploration | committed CI regression |

A `sequence` file is inert data (sends, edits, extractors), gated by the
receiver's own session, policy and receipts. An `h5i test` is that data plus an
oracle command, which the runner executes: safe to run because you wrote and
committed it, not safe to download. The verdict, not the flow, is where the trust
boundary lives.

## F3. What they share

Both describe a step the same way: send a named request as an actor, apply the
websec edit language (`set`/`unset` over `query.`, `json.`, `header.`), save the
response, and extract bindings. The extractors are already identical in code
(`regex:`, `json:`, `header:`, `status` in both `src/cli/websec.rs` `extract_one`
and `crates/h5i-test/src/main.rs` `extract`), though `design-websec.md` W11's
`jsonpath:`/`css:`/`cookie:` list is aspirational. The rule: the step language is
one grammar; a new extractor is added to both engines or neither.

## F4. Three verdict sources

Turning evidence into pass/fail happens in one of three places, a ladder of
expressiveness bought with trust:

1. **No verdict (agent decides).** The agent reads the responses and writes a
   `finding`. This is `sequence`. Right for logic and authorization holes that
   resist being written down.
2. **Declarative matcher (data decides).** A small closed language over one
   response yields a boolean. This is the Nuclei shape and it is data, so a flow
   carrying only this stays shareable and sandbox-safe. F6 adds it, bounded.
3. **External oracle (code decides).** An arbitrary program reads the evidence
   and returns an exit code. This is `h5i test`: unbounded, and therefore
   repository-local, never shared.

Source 2 is the new rung. Sources 1 and 3 both need someone in the loop (an agent,
or an author you trust), so neither can be the verdict of a recipe a stranger
publishes. Source 2 can.

## F5. Why not bash everywhere

Tempting: `h5i test` already avoids an assertion language by shelling to bash, so
let every flow carry a bash oracle. Rejected. h5i's guarantee is containment with
evidence; a shared recipe carrying bash is executable code crossing the trust
boundary at download time, turning the recipe library into a channel for the exact
thing the box exists to contain. So bash-as-oracle stays for a repo testing its
own code, and is not extended to the shared or imported layer. "Recipes are data;
only your own repo's oracles are code; the box enforces the line" is the dual-use
answer the pitch needs.

## F6. The declarative matcher, bounded

Source 2 is one new language; the failure mode is growing it unbounded. A matcher
attaches to a step and evaluates against its response:

```json
{"send": "req_probe",
 "expect": {"all": [
   {"status": 200},
   {"body": "regex:SQL syntax error near '(\\w+)'"},
   {"header": "content-type", "contains": "text/html"}]}}
```

The grammar is leaf matchers (`status` equals; `body`, `headers` block, or whole
`response` word/`regex`; named `header` with `contains`/`regex`; and `dsl`) plus
`all`/`any`/`not`. A flow passes when every step's `expect` held; a step with none
falls back to source 1 or 3.

The `dsl` leaf looks like code and is not. Nuclei's matcher DSL
(`!contains(tolower(body), '<html') && status_code == 200`) is a *pure*
expression language: it reads only the response and calls only pure functions
(string tests, `regex`, `len`, `compare_versions`, `base64`, the hashes, `mmh3`),
with no I/O, exec or network. `h5i-wire`'s `dsl` module evaluates it in-engine, so
it stays shareable. What keeps it there is the evaluator's refusal: an expression
naming a variable or function it does not implement is rejected at parse, so an
accepted expression is pure and faithfully evaluated. It reads the current
response through `body`/`status_code` and earlier ones through `body_1`/
`status_code_2`. Timing (`duration`) and request-side values (`host`) are not
bound: a shared verdict must be a deterministic function of the responses in hand.

The rule to hold in review: anything the matcher cannot say goes to an oracle (if
repository-owned) or the agent (if live), never into `expect`. The moment `expect`
can compute or compare arbitrary responses it has become an assertion language and
left its rung. Branching is the same trap from the flow side: a step may be gated
on a prior step's `expect` (still data), but a general conditional is exploration,
which is the agent's job as a prompt.

## F7. The Nuclei importer

Nuclei templates are the largest free corpus of source-2 verdicts: a request, a
set of matchers, a boolean. The importer's target is an `h5i test` file, not
`sequence`: a template describes a request from scratch (method + `{{BaseURL}}`
path), which a test request template expresses and a sequence step (which only
resends captured messages) cannot. `h5i websec import-nuclei <template>` prints an
`h5i.test/v1` whose verdict is `expect`:

- Each `http`/`requests` entry becomes a request template and a flow step.
  `{{BaseURL}}`/`{{RootURL}}` strip to a relative path; any other variable in a
  path, header or body is refused.
- A single `raw:` request is parsed into the same template (skip `@directive`
  lines; drop `Host` and `Content-Length`, which the engine sets; carry the body).
  An `unsafe: true` request is refused.
- `matchers` map onto `expect`: `word`→substring, `status`→`status`,
  `regex`→`regex:`. `matchers-condition` and a matcher's own `condition` map to
  `all`/`any` (Nuclei default `or`), `negative`→`not`. `part` picks the leaf:
  `body`→`body`, `header`→`headers` (case-insensitive, since the engine lowercases
  header names), `all`/`response`→`response`, a named header (`content_type`→
  `Content-Type`)→`header`. `interactsh_*` (out-of-band) is refused.
- `regex` `extractors` map onto the shared `extract`.
- A multi-request `raw` block becomes either a **variant list** (matchers do not
  read across responses: one step trying each request, verdict ORed, like a sweep)
  or a **chain** (`req-condition`, or an indexed part/dsl variable: one step per
  request, a single verdict on the last step reading `body_1`, `status_code_2`
  through the flow's response history, its matchers lowered to one `dsl` clause).
- A `payloads` template becomes a step with a `sweep`: the lists, the attack type
  (`batteringram`/`pitchfork`/`clusterbomb`), and `{{name}}`/`§name§` markers
  rewritten to `${name}`. It runs once per combination and passes if `expect` held
  for any — the Intruder workflow, portable. A file-backed list is refused.

Every `expect` built is verified against the shared grammar before it is written.
The importer never emits an oracle: what it cannot express as `expect` (a `dsl`
function it lacks, a `binary` matcher, an unmodelled variable) is refused, because
a skipped template is a gap and a smuggled oracle is a shared executable.

Measured against `nuclei-templates` (2026-09, 11,640 http templates), the importer
produces a runnable test for ~58%. The `dsl` evaluator did most of the lift: the
"unsupported matcher" refusal fell from ~2,000 to under a dozen. The remaining
~42% is the boundary working: ~1,200 have an unresolvable path variable; ~1,100
are `flow`/`javascript`/`headless`/`code` (code, F9); ~1,000 are cross-request raw
that reads another request's response outside the single-response model; ~1,000
use a non-deterministic dsl variable (`duration`, `host`); a few hundred use a
file-backed payload list. Each has a home (a sweep, an oracle, a multi-step flow)
that is not the single-response shareable verdict.

## F8. The regression seam

The engines touch at regression. `finding create --repro` already accepts a
sequence or experiment file, so an agent's live conclusion points at the flow that
reproduces it. The seam lets that flow graduate into `h5i test` without a rewrite:
a `sequence` flow is a valid test `flow` (same grammar), and a test only adds a
verdict field. With source-2 `expect` it is a shareable oracle-less test; with a
property that needs source 3 the repo writes an oracle and it stays local. The
lifecycle: explore with `sequence`, file a `finding` with a `--repro` flow, and
graduate it into `.h5i-tests/`. Run in CI, that is "attacked before it ships."

## F9. Sharing and the audit boundary

The registry the pitch wants is exactly the source-2 layer: a published recipe is
a flow with `expect` verdicts and no oracle. Enforceable, not merely encouraged:

- The import path is data-only. `import-nuclei` never emits an oracle and refuses
  a template carrying an executable protocol (`code`, `javascript`, `headless`,
  `flow`) rather than importing only its http part. Built.
- A pulled recipe runs like any flow, under the session's policy and receipts. It
  describes requests; it cannot smuggle execution because it contains none.
- `audit` records what ran; a data recipe leaves a receipt of its requests and
  nothing else.

The share half (a registry refusing a flow with an oracle) is designed, not built,
because there is no publishing surface yet. The rule it will enforce: an oracle is
repository-local by definition; there is no shared oracle.

## F10. What this changes

Unchanged: `h5i test`'s oracle contract, `websec sequence`'s live behavior,
`finding`. Changes, in order:

1. Keep the extractor prefixes identical across engines (F3).
2. Add the source-2 `expect` matcher to the shared grammar (F6), bounded to the
   Nuclei surface plus `all`/`any`/`not` and gated-step branching.
3. Add `h5i websec import-nuclei` (F7), refusing what it cannot express.
4. Let an `expect`-carrying flow be an oracle-less `h5i test` (F8).
5. Keep the crossing that exists data-only (F9); the registry half waits.

Deliberately not built: a general assertion language (that is the oracle); a
shared oracle (a contradiction); general conditionals and loops in a flow (that is
exploration, the agent's job). Each will be asked for, and each has an owner on
F4's ladder.
