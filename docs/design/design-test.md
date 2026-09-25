# Design: portable security regression tests

Status: MVP, 2026-09-13.

`h5i test` replays repository-owned attack flows. It does not invent an
assertion language. The verdict comes from one of two places: a repository-owned
external oracle that reads the result bundle (source 3), or data-only `expect`
clauses on the steps themselves (source 2). See `design-flow-and-verdict.md` for
why the two are different things and when to use each.

```text
portable attack flow -> structured evidence -> oracle or expect -> CI result
```

A test carries an oracle, or at least one step `expect`, or it is refused: with
neither, nothing decides pass or fail. An oracle is the repository's own code and
does not travel; a flow whose verdict is `expect` is data and can be shared or
imported (the Nuclei case).

## File format

Tests are strict YAML or JSON with `version: h5i.test/v1`. Request templates
contain a method, target-relative path, headers and either a text `body` or a
typed `json` body. `${name}` uses a value extracted by an earlier step and
`${env.NAME}` reads a CI environment variable. Unknown and missing fields are
errors rather than ignored configuration.

The default test directory is `.h5i-tests/tests`. It is repository-owned and
intended to be committed together with `.h5i-tests/oracles`; local h5i state
and machine-specific configuration remain under the gitignored `.h5i/`.

Actors name isolated browser sessions and cookie jars. A flow step sends a
template as an actor, may apply the existing websec edit language, saves its
response under a stable name, and may extract a regex, JSON field, header or
status for later steps. A step may also carry an `expect`: a bounded matcher over
its answer (`status`, `body`, `header`, and the combinators `all`, `any`, `not`),
the same grammar `websec sequence` uses. Cleanup steps run after the verdict even
when it fails, and never carry a verdict of their own.

## Payload sweeps

A flow step may carry a `sweep`: named payload lists and an attack type
(`batteringram`, `pitchfork`, `clusterbomb`, the last being every combination).
The step then sends once per combination, binding each payload value as `${name}`
in the request template, and its verdict is "the `expect` held for at least one
combination", with the deciding payload named in the step result. A swept step
therefore needs an `expect`, never appears in cleanup, and is bounded to 1024
sends so one template cannot become an unbounded run. This is the portable form
of the Intruder workflow: active testing from a committed file.

## Verdict from expect

When a test has no oracle, it passes only if every step `expect` held and the
flow completed; a step whose `expect` did not hold makes the run fail; a flow
error or a test with no verdict at all is an error, not a pass. The step results
carry `matched` and a one-line `verdict` either way, so a report reads the same
whether an oracle or an `expect` decided.

## Oracle contract

The oracle command runs with its working directory set to the test file's
directory. h5i provides:

- `H5I_TEST_RESULT`: the structured JSON evidence bundle.
- `H5I_TEST_ARTIFACTS`: its owner-only artifact directory.
- `H5I_TEST_INPUTS`: the comma-separated saved response names declared by the
  test.

Exit 0 means the property held, 1 means it did not, and any other exit or a
timeout means the test could not decide. h5i deliberately has no assertion
language. `jq`, `grep`, `diff`, an application test client, or any other
program may be the oracle.

## Coverage

A successful send may declare the OpenAPI operation and mutation class it
exercises. It counts only when the flow completed and the oracle returned a
conclusive 0 or 1. Setup and cleanup sends without `covers` do not count.

With `--openapi`, h5i reports oracle-checked operation coverage. It is informational
unless `--min-coverage` is explicitly supplied. Without an OpenAPI denominator
h5i reports no percentage and refuses a minimum.

## Outputs

Every run writes `result.json`, `junit.xml`, and per-response artifacts. The
directory is owner-only because response bodies are evidence and may contain
secrets. Credential request headers are redacted from exported request JSON;
response bodies remain exact for the oracle and are not uploaded automatically.
