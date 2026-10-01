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
proofs are checked by `cargo app-verify` and CI in the same way as the rest of
the repository. `examples/app/` is its own Cargo workspace, so run the `cargo`
commands from there.
