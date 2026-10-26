# Tutorials

Each tutorial builds a complete web application with h5i-app and then proves it
correct in Lean 4, one step at a time. The tutorials build on each other, so
start with the first one.

| Tutorial | Application | Topics |
|---|---|---|
| [1. Calculator](calculator/TUTORIAL.md) | a calculator with one memory per user | the kernel and the shell, extraction, specifications, proofs about loops |
| [2. Bulletin board](board/TUTORIAL.md) | posts, edits and moderation | permissions, invariants and reachable states |
| [3. Ledger](ledger/TUTORIAL.md) | accounts, deposits, withdrawals and transfers | sums over tables, conservation, invariants that rule out overflow |
| [4. Inbox](inbox/TUTORIAL.md) | private messages with blocking | confidentiality, views and noninterference |
| [5. Meeting rooms](booking/TUTORIAL.md) | rooms booked for intervals of time, with notifications | time as an input, monotonic time, interval invariants, effects through the outbox |

Every tutorial runs with one PostgreSQL container and `cargo run`, and its
proofs are checked by `h5i app prove` and CI in the same way as the rest of
the repository. `examples/app/` is its own Cargo workspace, so run the `cargo`
commands from there.

## Further examples

[Forum](forum/README.md) ports Nyxis's bulletin board (drafts, admins,
self-registration) with no hand-written Lean: its handlers run behind a policy
written in Rust, and the framework proves the authorization theorem
([docs/app/AUTHZ.md](../../docs/app/AUTHZ.md)). Read it after tutorial 4.

A larger application lives in
[h5i-app-bench](https://github.com/h5i-dev/h5i-app-bench), with its own Cargo
workspace and proof project: [Conduit](https://github.com/h5i-dev/h5i-app-bench/tree/realworld-bench/realworld/rust-h5i-app) (`realworld/rust-h5i-app`) is a port of the
RealWorld API (realworld-axum-sqlx) with proofs about its replies and
invariants. All its users share one tenant; it is the benchmark of
[docs/app/PERFORMANCE.md](../../docs/app/PERFORMANCE.md).
