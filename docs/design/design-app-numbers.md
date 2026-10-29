# h5i-app numbers

Proof effort, check results, check times, bugs found, the cost of the
framework authorization guard, and Conduit load-test throughput.

## Proof effort per app

Measured 2026-09-27. Lines exclude blanks and comments. Hand-written Lean
includes the spec and excludes generated files (extracted kernels,
`Schema.lean`). Rust kernel sizes exclude tests. Counts include scenarios and
upstream-bug counterexamples. The docs, filters and keys apps and the ports
have since moved to the h5i-web-app repository; their rows are as measured
here.

| App | Kernel (Rust) | Spec (Lean) | Hand-written Lean | Lean per kernel line |
|---|---|---|---|---|
| docs | 936 | 108 | 2,538 | 2.7 |
| kellnr | 374 | 71 | 820 | 2.2 |
| atuin | 459 | 51 | 527 | 1.1 |
| wastebin | 275 | 51 | 720 | 2.6 |
| conduit | 1,063 | 127 | 1,682 | 1.6 |
| cratesio | 1,030 | 142 | 1,497 | 1.5 |
| tutorial 1, calculator | 114 | 11 | 218 | 1.9 |
| tutorial 2, board | 221 | 45 | 441 | 2.0 |
| tutorial 3, ledger | 180 | 29 | 461 | 2.6 |
| tutorial 4, inbox | 239 | 41 | 580 | 2.4 |
| tutorial 5, booking | 268 | 61 | 644 | 2.4 |
| filters | 251 | 55 | 425 | 1.7 |
| keys | 204 | 41 | 298 | 1.5 |

Upstream Conduit has 1,077 handler lines with inline SQL, so the port costs
about 1.6 Lean lines per handler line. Docs also proves noninterference,
partial snapshots, migration checks, row codecs and scoped loads; its spec,
authorization, replies and invariants alone take 865 Lean lines, about 1.3 per
line of the matching kernel code. Its generated `Schema.lean` is 474 lines;
the app writes 31 lines of `Columns.lean` for two enums.

Shared code:

| Piece | Rust | Lean |
|---|---|---|
| `H5iAppLib` (loops, tables, lists, scalars, SQL and store semantics, tactics) | none | 1,310 |
| `h5i-app-sql` (planner) | 141 | 118 |
| `h5i-app-token` (token parser and encoder) | 197 | 963 |
| `h5i-app-json` (reply writer) | 207 | 427 |
| `h5i-app-std` (bytes, sets, maps, reachability, expiry), list models in `H5iAppLib` | 335 | 1,102 |

## Framework-proven authorization

Measured 2026-10-26 ([design-app-authz.md](design-app-authz.md)). The guard
and its theorem are written once; an app adds a policy and no Lean.

| Piece | Rust | Lean |
|---|---|---|
| `h5i_app_std::authz` (guard, checks, traits) | 113 | none |
| `AuthzSpecs.lean` (theorems, `h5i_authz`) | none | 412 |
| forum: handlers | 77 | 0 |
| forum: `Policy`, `Disclose`, `View`, wiring | 107 | 0 |
| forum: `generated/Authz.lean` | none | 79, written by `h5i app extract` (4 for authorization, the rest the storage proof) |

`h5i app check` on the forum gates 63 theorems, among them
`guarded_transition.authorized`, `.kernel_apply_eq`, `.read_only_sound` and the
storage proof's `Committed.sql_fits`, `apply_enc` and `pg_stored`. For
comparison, board (tutorial 2) states its authorization and invariants in 319
hand-written Lean lines over a 177-line kernel.

Handler bugs behind the forum's policy (`handler_bugs_are_denied`) are each
denied: taking over a post on edit, disclosing others' drafts on publish,
self-promotion on registration, and replying with a hidden draft relabelled
as published. `tests/postgres.rs` runs 600 random commands from four users on
PostgreSQL and `MemoryEngine`; replies (denials included) and final state
agree, and the run contains both denials and commits.

### Guard overhead

`guard_overhead` in `examples/app/forum/kernel/src/lib.rs` (ignored test,
release build, one core) times a command with the guard against `transition`
alone. The guard runs `kernel_apply` per write prefix and once for the state
after, and builds the view for reads, which copies the snapshot (linear in the
tenant). `stored` looks up each disclosed row, so on `Vec` snapshots a listing
is quadratic in its length. The engine also loads and decodes the whole tenant
per command; that cost is not in this table.

| Tenant | Edit | Create (2 writes) | List (read-only, view) |
|---|---|---|---|
| 10 users, 100 posts | 0.4 µs → 22 µs | 0.07 µs → 25 µs | 8 µs → 25 µs |
| 100 users, 1,000 posts | 0.8 µs → 168 µs | 0.07 µs → 245 µs | 79 µs → 348 µs |
| 1,000 users, 10,000 posts | 9 µs → 1.8 ms | 0.08 µs → 2.6 ms | 0.83 ms → 18.6 ms |

## Checks

| Check | Result |
|---|---|
| `h5i app mutate` on docs | 22 of 22 kernel bugs break a proof |
| `h5i app mutate` on the other twelve kernels | 12 of 12 compiling kernel bugs break a proof |
| docs' Rust-vs-Lean differential test | 5,000 random cases per run agree (55,000 in one longer run); every outcome kind hit |
| Axioms (`h5i app check`) | only `propext`, `Classical.choice`, `Quot.sound`, for all 3,974 theorems in hand-written modules |
| Extraction drift (CI) | every kernel and extracted crate re-extracted and compared |

## Time

On a 128-core machine:

| Step | Time |
|---|---|
| Clean `lake build` of docs proofs (Mathlib cached) | 87 s |
| `cargo app-verify`, incremental | about 5 min |
| Docs mutation suite, 3 jobs | 13 min |

## Bugs found

| Where | What | How |
|---|---|---|
| h5i-app engine | idempotency keys per tenant only; one user could get another's reply | PostgreSQL test; keys now per app-defined scope |
| h5i-app engine | stale snapshot under the tenant lock | fault and concurrency tests; lock now taken before `BEGIN` |
| h5i-app engine | apps sharing a database shared tables, keys and outbox; one dispatcher killed another's effects | example tests; one PostgreSQL schema per app |
| Kellnr (before PR #1243) | read-only session could add crate owners | read-only theorem fails; Lean gives the counterexample |
| Atuin (issue #3297, open) | a session alone can delete the account | proven for current rules; fix proven to need the password |
| Wastebin (before 632ddf2, issue #190) | link preview GET burned a burn-after-reading paste | `preview_broken` counterexample; `preview_fixed` for current code |
| Wastebin (current) | `/raw`, `/dl`, `/md` still burn a paste on GET | `raw_link_burns` |
| Conduit (issue #16, open) | `favorited` means "favorited any article" | `upstream_violates_reply_spec` |
| Conduit (unreported) | `?favorited=` lists every article once the user favorited one | `favorited_filter` |
| crates.io (before PR #14760) | a locked account could sign in | `pre14760_violates_lock` |
| crates.io (current) | the emailed invitation link skips the lock check; a locked user can become owner | found while porting; see the crates.io port's README |

## Conduit performance

Conduit is the RealWorld API on an h5i-app kernel. Its kernel, server, proofs
and load test live in
[h5i-app-bench](https://github.com/h5i-dev/h5i-app-bench/tree/realworld-bench/realworld/rust-h5i-app),
in `realworld/rust-h5i-app`. All users share one tenant, the worst case for an
engine that serializes writes per tenant.

### Method

`server/examples/load.rs` seeds a site with plain SQL (50 users, 10,000
articles, 20,000 tag rows over 100 tag names, 5,000 favorites, 500 follows),
serves the API in-process on a pool of 32 connections, and drives it over
HTTP/1.1 keep-alive for 10 s per row. Each request uses a random user's token.

| Scenario | Request |
|---|---|
| get-article | `GET /api/articles/article-<random>` |
| list | `GET /api/articles?limit=20&offset=0` |
| list-tag | `GET /api/articles?tag=tag<random>&limit=20` |
| feed | `GET /api/articles/feed?limit=20&offset=0` |
| favorite | `POST /api/articles/article-<random>/favorite` (favorites reset first) |

`attempts`, `retries` and `commits` are deltas of the engine's `EngineStats`
(`commits` counts accepted commands, reads included). PostgreSQL 16 runs in
Docker on the same host; server, client and database share CPUs 64-127 of a
128-core machine that other jobs also used, so differences under about 20%
are noise.

```
git clone -b realworld-bench https://github.com/h5i-dev/h5i-app-bench && cd h5i-app-bench/realworld/rust-h5i-app
DATABASE_URL=postgres://... cargo run --release -p conduit-server --example load -- 10
```

### Current results

| scenario | conc | req/s | p50 ms | p99 ms | non-2xx | attempts | retries | commits |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| get-article | 1 | 709.5 | 1.41 | 1.72 | 0 | 7097 | 0 | 7097 |
| get-article | 16 | 7007.7 | 2.24 | 3.20 | 0 | 70112 | 0 | 70112 |
| get-article | 64 | 14657.0 | 4.31 | 5.50 | 0 | 146725 | 0 | 146725 |
| list | 1 | 221.2 | 4.50 | 5.85 | 0 | 2213 | 0 | 2213 |
| list | 16 | 3755.2 | 4.23 | 5.85 | 0 | 37573 | 0 | 37573 |
| list | 64 | 8203.0 | 7.73 | 10.18 | 0 | 82097 | 0 | 82097 |
| list-tag | 1 | 294.2 | 3.26 | 5.90 | 0 | 2943 | 0 | 2943 |
| list-tag | 16 | 3718.8 | 4.26 | 5.99 | 0 | 37203 | 0 | 37203 |
| list-tag | 64 | 8071.1 | 7.86 | 10.31 | 0 | 80802 | 0 | 80802 |
| feed | 1 | 201.6 | 5.00 | 6.68 | 0 | 2017 | 0 | 2017 |
| feed | 16 | 2704.6 | 5.90 | 8.27 | 0 | 27061 | 0 | 27061 |
| feed | 64 | 5941.7 | 10.61 | 14.79 | 0 | 59486 | 0 | 59486 |
| favorite | 1 | 232.0 | 4.22 | 7.58 | 0 | 2321 | 0 | 2321 |
| favorite | 16 | 2522.2 | 6.16 | 12.98 | 0 | 25604 | 367 | 25237 |
| favorite | 64 | 4705.9 | 13.27 | 26.90 | 0 | 47764 | 641 | 47123 |

Single runs at concurrency 1 spread by 10-20% for the listings. Six
interleaved runs per build showed the bounded statement cache within one
standard deviation of the unbounded one in every scenario.

### What each change bought

Requests per second at concurrency 64.

| Change | get-article | list | list-tag | feed | favorite | favorite retries / attempts |
|---|---:|---:|---:|---:|---:|---:|
| Baseline: tenant lock, whole-site load per request | 13.0 | 12.4 | 5.1 | 13.0 | 12.5 | 0 / 188 |
| Read-only commands without the tenant lock (`Kernel::read_only`) | 122.2 | 124.2 | 65.9 | 120.2 | 11.3 | 0 / 175 |
| Per-key advisory locks (`Store::lock_for`) | 113.1 | 108.7 | 72.0 | 108.6 | 7.6 | 1366 / 1448 |
| Declarative reads and per-command read plans (`h5i_app_sql::Read`, `Frame.frame`), quiet-host rerun | 3831.4 | 3862.9 | 3894.2 | 3063.6 | 2440.1 | 533 / 25000 |
| Prepared statements, pipelined plan reads | 15246.7 | 8027.6 | 7519.7 | 5474.5 | 4371.1 | 624 / 44404 |
| Bounded statement cache (`EngineConfig::statement_cache`, default 256) | 14657.0 | 8203.0 | 8071.1 | 5941.7 | 4705.9 | 641 / 47764 |

Per-key locks alone made favorites worse: each favorite still read every
table, so SERIALIZABLE commits conflicted with the others' reads. They pay off
only once a command reads what it needs. Read plans are proven by
`Frame.frame` to load enough; prepared statements and pipelining send the same
SQL with the same parameters in the same transaction, so no proof changed.
A read's SQL text depends only on its shape and on whether its window has a
limit and a non-zero offset (at most four texts per shape). Rendering
`OFFSET $n` always was rejected: PostgreSQL then never switches to the
generic plan and re-plans every execution, and the tag listing lost a third
of its throughput.

### Open

- Sign-up, login, new, edited and deleted articles and the tag list have no
  read plan and still load the whole site (login and the tag list without
  locks, as read-only commands).
- Favorites retry on about 1.3% of attempts at concurrency 64: PostgreSQL takes SERIALIZABLE
  predicate locks per index page, so two favorites touching the same page of
  the favorites index can conflict although their rows differ. Read-only
  commands run in deferrable read-only transactions on a safe snapshot and
  never retry.

The RAM and CUDA executors are measured separately in
[design-app-executors.md](design-app-executors.md); neither result is a
complete application request speedup.
