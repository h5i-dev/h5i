# Security regression test in CI

This example keeps a manually authored attack flow in the repository and
replays it on every pull request. The fictional application has two seeded
users, Alice and Bob. The test logs in as Alice and verifies that she cannot
read Bob's document.

The pieces are deliberately separate:

- [`.h5i-tests/tests/document-isolation.yaml`](.h5i-tests/tests/document-isolation.yaml)
  describes the HTTP attack flow.
- [`.h5i-tests/oracles/document-isolation.sh`](.h5i-tests/oracles/document-isolation.sh)
  decides whether the resulting evidence satisfies the application's security
  property.
- [`openapi.yaml`](openapi.yaml) supplies an optional coverage denominator.
- [`.github/workflows/security.yml`](.github/workflows/security.yml) runs the
  test in GitHub Actions and uploads the evidence.

Copy these files into the corresponding paths in an application repository,
then change the login request, document request, seeded fixture values, and
oracle for that application. The workflow assumes `docker compose up -d
--wait` starts the application at `http://localhost:3000`.

Run the same test locally after installing the test plugin:

```sh
h5i plugin install test
H5I_ALICE_PASSWORD=alice-ci-password \
H5I_OTHER_DOCUMENT_ID=2 \
H5I_FORBIDDEN_MARKER=bob@example.test \
  h5i test .h5i-tests/tests \
    --target http://localhost:3000 \
    --openapi openapi.yaml
```

Coverage is informational in this example. Add `--min-coverage PERCENT`
locally, or the action's `min-coverage` input, only when the repository wants
coverage to be a required gate.
