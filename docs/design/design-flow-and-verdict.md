# Design: flows and verdicts

h5i has two multi-step HTTP engines, `websec sequence` and `h5i test`. They share
one flow language. They differ in who decides pass or fail.

```text
one flow language  ->  three verdict sources  ->  two owners
  (send + edit +        none  | matcher |          agent (live)
   extract, shared)      (data)  oracle (code)      repository (CI)
```

## F1. The two engines

- `websec sequence` (design-websec.md W11) runs in a live session, binds values
  from one response into the next, and stops on a failed binding. The agent reads
  the result and writes a `finding`.
- `h5i test` ([design-test.md](design-test.md)) replays a repository-owned flow.
  An external oracle or step `expect` clauses decide the verdict.

## F2. Who owns the verdict

| | `websec sequence` | `h5i test` |
|---|---|---|
| verdict owner | the agent, after the fact | the repository |
| verdict form | a `finding` | oracle exit code, or `expect` |
| artifact | data | data, plus oracle code if it has one |
| shareable | yes | only when the verdict is `expect` |
| use | live exploration | committed CI regression |

The trust boundary sits at the verdict, not the flow.

## F3. The shared step language

A step sends a named request as an actor, applies the websec edit language
(`set`/`unset` over `query.`, `json.`, `header.`), saves the response, and
extracts bindings. Extractors are `regex:`, `json:`, `header:` and `status` in both
`src/cli/websec.rs` (`extract_one`) and `crates/h5i-test/src/main.rs` (`extract`).
The `jsonpath:`/`css:`/`cookie:` list in W11 is not implemented. A new extractor
goes into both engines or neither.

## F4. Three verdict sources

1. No verdict: the agent decides and writes a `finding`. This is `sequence`.
2. Declarative matcher: a closed language over responses yields a boolean
   (`expect`, F6). It is data, so a flow carrying only this is shareable.
3. External oracle: a program reads the evidence and returns an exit code. This
   is `h5i test`'s oracle. It is code, so it stays in its repository.

Sources 1 and 3 need someone trusted in the loop. Only source 2 can be the
verdict of a recipe written by a stranger.

## F5. No shared oracle

A bash oracle is fine for a repository testing its own code. A shared or imported
recipe never carries one, because that would ship executable code across the
boundary the box exists to hold.

## F6. The `expect` matcher

`expect` attaches to a step and evaluates against its response
(`crates/h5i-wire/src/expect.rs`):

```json
{"send": "req_probe",
 "expect": {"all": [
   {"status": 200},
   {"body": "regex:SQL syntax error near '(\\w+)'"},
   {"header": "content-type", "contains": "text/html"}]}}
```

Leaves: `status` equals; `body`, `headers` or whole `response` with word or
`regex`; named `header` with `contains`/`regex`; and `dsl`. Combinators: `all`,
`any`, `not`. In `h5i test` with no oracle, a flow passes when every step's
`expect` held. In `sequence`, a failed `expect` stops the chain with
`EXIT_NO_MATCH`.

`dsl` is Nuclei's matcher expression language, evaluated in-engine
(`crates/h5i-wire/src/dsl.rs`). It reads only responses and calls only pure
functions (string tests, `regex`, `len`, `compare_versions`, `base64`, hashes,
`mmh3`). An unknown variable or function is rejected at parse, so an accepted
expression is pure. It reads the current response as `body`/`status_code` and
earlier ones as `body_1`, `status_code_2`. `duration` and request-side values
such as `host` are not bound, so a verdict is a deterministic function of the
responses.

Review rule: what the matcher cannot say goes to an oracle (repository) or the
agent (live), never into `expect`. General conditionals and loops in a flow are
not supported.

## F7. The Nuclei importer

`h5i websec import-nuclei <template>` (`crates/h5i-websec/src/nuclei.rs`) prints
an `h5i.test/v1` file whose verdict is `expect`. It targets `h5i test` because a
template builds requests from scratch, which a `sequence` step cannot.

| Nuclei | becomes |
|---|---|
| `http`/`requests` entry | request template and flow step; `{{BaseURL}}`/`{{RootURL}}` stripped to a relative path |
| single `raw:` request | the same template (`@directive` lines skipped, `Host`/`Content-Length` dropped) |
| `word`, `status`, `regex` matchers | substring, `status`, `regex:` |
| `matchers-condition`, `condition`, `negative` | `all`/`any` (default `or`), `not` |
| `part` | `body`, `headers` (case-insensitive), `response`, or a named `header` |
| `regex` extractors | the shared `extract` |
| multi-request `raw`, independent | one step with `variants`, verdict ORed |
| multi-request `raw`, chained (`req-condition` or indexed variables) | one step per request, one `dsl` verdict on the last step over the response history |
| `payloads` | a `sweep` (`batteringram`/`pitchfork`/`clusterbomb`), passing if `expect` held for any combination |

Refused: other variables in a path, header or body; `unsafe: true`;
`interactsh_*`; file-backed payload lists; `binary` matchers; unsupported `dsl`
functions; executable protocols (`code`, `javascript`, `headless`, `flow`). Each
`expect` is checked against the grammar before it is written. The importer never
emits an oracle.

Against `nuclei-templates` (2026-09, 11,640 http templates) it produces a
runnable test for about 58%. Of the rest: about 1,200 use an unresolvable path
variable, 1,100 an executable protocol, 1,000 cross-request raw outside the
single-response model, 1,000 a non-deterministic `dsl` variable, and a few
hundred file-backed payloads.

## F8. From finding to regression

`finding create --repro` accepts a sequence or experiment file. A `sequence` flow
is a valid `h5i test` flow; a test adds a verdict. With `expect` it needs no
oracle; with an oracle it stays in the repository. The path is: explore with
`sequence`, file a `finding` with `--repro`, move the flow into `.h5i-tests/`, run
it in CI.

## F9. Sharing

- Import is data-only: `import-nuclei` never emits an oracle and refuses a
  template with an executable protocol rather than importing its http part.
- A pulled recipe runs like any flow, under the session's policy and receipts.
- `audit` records the requests a data recipe sent and nothing else.

Not built: a registry that refuses a flow carrying an oracle. There is no
publishing surface yet.

## F10. Limits

No general assertion language (that is the oracle), no shared oracle, and no
conditionals or loops in a flow (that is the agent's job).
