# Executor comparison

`executor_load` compares PostgreSQL and write-through RAM executors using the
existing calculator kernel and the proved `Get` read-only adapter in
`executor_demo`. It drives get and set-plus-snapshot at concurrency 1/16/64,
with 64 users per tenant and a warm cache. The two executors own separate
tenants. It uses the Executor interface
directly; no HTTP or authentication cost is included.

Set `DATABASE_URL` to a disposable PostgreSQL database and run the release
example from the examples/app workspace. It installs its own schema. The
`ram` integration test additionally checks accepted writes, refusals,
idempotent replay and cancellation, using `H5I_APP_TEST_DATABASE_URL`.

Recorded results and trust obligations: [MEMORY.md](../../../../../docs/design/design-app-executors.md).
