# Design: portable security regression tests

Status: MVP, 2026-09-13.

`h5i test` replays repository-owned attack flows. It does not decide what a
secure response means. A repository-owned external oracle reads the result
bundle and returns pass, fail, or error.

```text
portable attack flow -> structured evidence -> external oracle -> CI result
```

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
status for later steps. Cleanup steps run after the oracle even when it fails.

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
