# Design: portable security regression tests

`h5i test` replays repository-owned attack flows. The verdict comes from a
repository-owned external oracle that reads the result bundle (source 3), or
from data-only `expect` clauses on the steps (source 2). See
[design-flow-and-verdict.md](design-flow-and-verdict.md) for the difference.

```text
portable attack flow -> structured evidence -> oracle or expect -> CI result
```

A test with neither an oracle nor any step `expect` is refused. An oracle is the
repository's own code and does not travel; a flow whose verdict is `expect` is
data and can be shared or imported (the Nuclei case).

## File format

Strict YAML or JSON with `version: h5i.test/v1`. Unknown and missing fields are
errors. Request templates have a method, target-relative path, headers, and
either a text `body` or a typed `json` body. `${name}` uses a value extracted by
an earlier step; `${env.NAME}` reads a CI environment variable.

Tests live in `.h5i-tests/tests`, committed with `.h5i-tests/oracles`. Local
state stays in the gitignored `.h5i/`.

Actors name isolated browser sessions and cookie jars. A step sends a template as
an actor, may apply the websec edit language, saves its response under a stable
name, and may extract a regex, JSON field, header or status. It may carry an
`expect` (`status`, `body`, `header`, `all`, `any`, `not`), the same grammar
`websec sequence` uses. Cleanup steps run after the verdict even when it fails
and never carry a verdict.

## Verdict from expect

With no oracle, a test passes only if the flow completed and every step `expect`
held. A failed `expect` fails the run. A flow error, or a test with no verdict,
is an error, not a pass. Each step reports `matched` and a one-line `verdict`.

- `variants`: further requests tried in place of `send`, stopping at the first
  whose `expect` holds.
- `sweep`: named payload lists and an attack type (`batteringram`, `pitchfork`,
  `clusterbomb`), each value bound as `${name}`. The verdict is "held for at
  least one", with the deciding payload named. Capped at 1024 sends; not allowed
  in cleanup.
- Response history: a later step's `dsl` reads `body_1`, `status_code_2`. A
  reference to a request the flow did not send is an evaluation error, not a
  false match.

## Oracle contract

The oracle runs in the test file's directory with:

- `H5I_TEST_RESULT`: the JSON evidence bundle.
- `H5I_TEST_ARTIFACTS`: its owner-only artifact directory.
- `H5I_TEST_INPUTS`: comma-separated saved response names declared by the test.

Exit 0: the property held. Exit 1: it did not. Any other exit, or a timeout: the
test could not decide. Any program (`jq`, `grep`, `diff`, an app test client) can
be the oracle.

## Coverage

A send may declare the OpenAPI operation and mutation class it exercises
(`covers`). It counts only when the flow completed and the oracle returned a conclusive 0 or 1. Setup and cleanup sends without `covers` do not count.

With `--openapi`, h5i reports oracle-checked operation coverage, informational
unless `--min-coverage` is given. Without an OpenAPI denominator there is no
percentage and a minimum is refused.

## Outputs

Every run writes `result.json`, `junit.xml` and per-response artifacts into an
owner-only directory, since bodies are evidence and may hold secrets. Credential
request headers are redacted from exported request JSON. Response bodies stay
exact for the oracle and are not uploaded.
