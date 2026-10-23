# Security regression tests

Read this when turning a confirmed security property into a repeatable
`h5i test` flow or using OpenAPI coverage in CI. The test plugin works for
applications built with any framework. Obtain authorization for the target
origin, actor identities, requests, and rate before replaying a flow. Use a
resettable staging environment for tests that change state.

`h5i plugin install test` adds the verb to a plain CLI install. Check
`h5i test --help` for flags. A test file uses `version: h5i.test/v1`, named
actors and request templates, and a `flow` whose sends carry explicit
`expect` clauses. Use a repository-owned `oracle` when response checks cannot
express the verdict. A test with neither is refused.

```yaml
version: h5i.test/v1
id: post-ownership
name: Alice cannot read Bob's post
actors:
  alice: {}
requests:
  own_post:
    path: /api/posts/41
    headers:
      authorization: Bearer ${env.ALICE_TOKEN}
  other_post:
    path: /api/posts/42
    headers:
      authorization: Bearer ${env.ALICE_TOKEN}
flow:
  - send: own_post
    as: alice
    expect:
      status: 200
  - send: other_post
    as: alice
    expect:
      status: 403
```

Choose expectations that express the property, not an incidental response
from one run. Confirm setup establishes the actor and state before the verdict.
Keep credentials in environment or CI secrets, never in a checked-in file.
Use separate actor cookie jars for multi-user flows and extract dynamic IDs
from prior responses. Keep cleanup separate and preserve its errors.

```bash
h5i test .h5i-tests/tests/post-ownership.yaml --target https://staging.example.com
h5i test --target https://staging.example.com --openapi openapi.yaml --min-coverage 80
```

Review `h5i-test-results/` and the command exit status. Pass means the stated
expectation held; fail means it was contradicted; error means the run could not
decide and is not a pass. OpenAPI coverage counts checked operations, not
security completeness. Review the denominator and operation mapping before
setting `--min-coverage`. See `docs/demo/.h5i-tests/tests/board-auth.yaml`
and the `h5i test` manual for a full example and schema.
