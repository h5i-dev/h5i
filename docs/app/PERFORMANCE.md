# Performance on Conduit

`examples/app/conduit` is the RealWorld (Conduit) API on an h5i-app kernel.
All users share one tenant, the worst case for an engine that serializes per
tenant. This file records how the engine changes behave on it.

## Method

`examples/app/conduit/server/examples/load.rs` seeds a site with plain SQL
(50 users, 10,000 articles, 20,000 tag rows over 100 tag names, 5,000
favorites, 500 follows), serves the API in-process on a pool of 32
connections, and drives it over HTTP/1.1 keep-alive connections for 10 s per
row:

| Scenario | Request (random user token) |
|---|---|
| get-article | `GET /api/articles/article-<random>` |
| list | `GET /api/articles?limit=20&offset=0` |
| list-tag | `GET /api/articles?tag=tag<random>&limit=20` |
| feed | `GET /api/articles/feed?limit=20&offset=0` |
| favorite | `POST /api/articles/article-<random>/favorite` (favorites reset first) |

`attempts`, `retries` and `commits` are the deltas of the engine's
`EngineStats` during the row (`commits` counts accepted commands, reads
included). PostgreSQL 16 runs in Docker on the same host; server, client and
database share CPUs 64-127 of a 128-core machine that other jobs also used,
so treat differences under ~20% as noise.

```
DATABASE_URL=postgres://... cargo run --release -p conduit-server --example load -- 10
```

## Step 0: baseline

Every request takes the tenant's advisory lock and a read-write
SERIALIZABLE transaction and loads the whole site.

| scenario | conc | req/s | p50 ms | p99 ms | non-2xx | attempts | retries | commits |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| get-article | 1 | 12.4 | 79.79 | 114.54 | 0 | 124 | 0 | 124 |
| get-article | 16 | 12.3 | 1263.20 | 1450.46 | 0 | 140 | 0 | 140 |
| get-article | 64 | 13.0 | 4820.17 | 5146.81 | 0 | 192 | 0 | 192 |
| list | 1 | 12.8 | 76.37 | 101.23 | 0 | 129 | 0 | 129 |
| list | 16 | 12.4 | 1247.58 | 1436.04 | 0 | 140 | 0 | 140 |
| list | 64 | 12.4 | 4965.38 | 5458.60 | 0 | 189 | 0 | 189 |
| list-tag | 1 | 5.1 | 190.61 | 242.81 | 0 | 52 | 0 | 52 |
| list-tag | 16 | 5.2 | 3005.42 | 3289.01 | 0 | 68 | 0 | 68 |
| list-tag | 64 | 5.1 | 11189.44 | 13142.21 | 0 | 117 | 0 | 117 |
| feed | 1 | 12.5 | 77.50 | 166.58 | 0 | 126 | 0 | 126 |
| feed | 16 | 13.0 | 1221.91 | 1404.70 | 0 | 144 | 0 | 144 |
| feed | 64 | 13.0 | 4731.86 | 5191.02 | 0 | 192 | 0 | 192 |
| favorite | 1 | 12.4 | 77.78 | 113.12 | 0 | 125 | 0 | 125 |
| favorite | 16 | 11.1 | 1494.06 | 1751.01 | 0 | 126 | 0 | 126 |
| favorite | 64 | 12.5 | 5118.11 | 5310.79 | 0 | 188 | 0 | 188 |

Throughput is flat at about 12 requests/s whatever the concurrency (5/s for
the tag listing, which scans more): one request at a time, ~80 ms each to
load and decode the whole site.

## Step 1: read-only commands

The kernel declares its eight read commands read-only (`Kernel::read_only`,
proven by `ReadOnly.read_only_sound`). The engine runs them in `SERIALIZABLE
READ ONLY DEFERRABLE` transactions without the tenant lock; writes are
unchanged.

| scenario | conc | req/s | p50 ms | p99 ms | non-2xx | attempts | retries | commits |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| get-article | 1 | 13.1 | 75.32 | 110.46 | 0 | 131 | 0 | 131 |
| get-article | 16 | 87.6 | 177.84 | 342.66 | 0 | 886 | 0 | 886 |
| get-article | 64 | 122.2 | 514.22 | 634.76 | 0 | 1278 | 0 | 1278 |
| list | 1 | 12.3 | 79.88 | 111.66 | 0 | 123 | 0 | 123 |
| list | 16 | 89.4 | 176.16 | 240.98 | 0 | 909 | 0 | 909 |
| list | 64 | 124.2 | 506.76 | 615.20 | 0 | 1299 | 0 | 1299 |
| list-tag | 1 | 5.6 | 179.95 | 207.33 | 0 | 56 | 0 | 56 |
| list-tag | 16 | 50.7 | 293.89 | 527.53 | 0 | 536 | 0 | 536 |
| list-tag | 64 | 65.9 | 936.90 | 1198.09 | 0 | 715 | 0 | 715 |
| feed | 1 | 10.6 | 85.55 | 172.81 | 0 | 107 | 0 | 107 |
| feed | 16 | 91.2 | 172.28 | 255.57 | 0 | 924 | 0 | 924 |
| feed | 64 | 120.2 | 523.02 | 666.64 | 0 | 1261 | 0 | 1261 |
| favorite | 1 | 12.4 | 79.06 | 116.66 | 0 | 125 | 0 | 125 |
| favorite | 16 | 12.7 | 1209.49 | 1451.40 | 0 | 144 | 0 | 144 |
| favorite | 64 | 11.3 | 5505.95 | 5859.44 | 0 | 175 | 0 | 175 |

Reads now run in parallel: about 120 requests/s at concurrency 64 (10x),
bounded by the CPU cost of loading the whole site for each request. A
single request is as slow as before, and favorites, still serialized on the
tenant lock, stay at 12/s.

## Step 2: per-key advisory locks

`Store::lock_for` names the advisory locks of a write: the whole tenant (the
default), the tenant shared plus exclusive per-key locks, or none. Conduit
locks an article's slug for favorite, unfavorite, edit and delete, a
(follower, name) pair for follow, the user for account edits, and the id
counter for sign-up, new articles and comments. Loads are still of the whole
site.

| scenario | conc | req/s | p50 ms | p99 ms | non-2xx | attempts | retries | commits |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| get-article | 1 | 8.9 | 110.28 | 155.39 | 0 | 90 | 0 | 90 |
| get-article | 16 | 91.7 | 168.44 | 266.97 | 0 | 929 | 0 | 929 |
| get-article | 64 | 113.1 | 553.45 | 702.25 | 0 | 1198 | 0 | 1198 |
| list | 1 | 8.1 | 121.69 | 165.35 | 0 | 81 | 0 | 81 |
| list | 16 | 84.7 | 184.67 | 272.80 | 0 | 861 | 0 | 861 |
| list | 64 | 108.7 | 579.29 | 752.00 | 0 | 1151 | 0 | 1151 |
| list-tag | 1 | 4.3 | 235.17 | 282.88 | 0 | 44 | 0 | 44 |
| list-tag | 16 | 56.6 | 278.01 | 355.92 | 0 | 580 | 0 | 580 |
| list-tag | 64 | 72.0 | 868.53 | 1067.78 | 0 | 779 | 0 | 779 |
| feed | 1 | 8.8 | 112.68 | 147.31 | 0 | 88 | 0 | 88 |
| feed | 16 | 88.2 | 175.93 | 265.30 | 0 | 900 | 0 | 900 |
| feed | 64 | 108.6 | 582.38 | 724.01 | 0 | 1146 | 0 | 1146 |
| favorite | 1 | 7.3 | 138.46 | 178.95 | 0 | 73 | 0 | 73 |
| favorite | 16 | 7.5 | 1196.21 | 5610.68 | 13 | 752 | 675 | 77 |
| favorite | 64 | 7.6 | 6341.49 | 12394.94 | 33 | 1448 | 1366 | 82 |

Reads are unchanged (the single-request rows are lower than in step 1 only
because other builds shared the CPUs). Favorites get worse: two favorites
of different articles no longer queue, but each still reads every table, so
under SERIALIZABLE each commit conflicts with the others' reads; 675 of 752
attempts at concurrency 16 were retries, and 13 requests ran out of
retries. Finer locks only help once a command reads what it needs and no
more, which is step 3.

## Steps 3 and 5: declarative reads and Conduit's read plans

The engine gains declarative reads (`h5i_app_sql::Read`: filters, `IN`
sub-reads, key-ordered windows, counts) and Conduit a read plan per command
(`reads`, see its README), proven by `Frame.frame` to load enough. Reads by
key go through indexes `ConduitStore::ddl` adds (slug, author, tag name,
favoriting user, username, email, comment's article). The listing and the
feed load the newest `offset + limit` matching articles and what their
views need, nothing else. Writes keep the per-key locks of step 2. This run
shared the CPUs with a Lean build, so it understates the throughput.

| scenario | conc | req/s | p50 ms | p99 ms | non-2xx | attempts | retries | commits |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| get-article | 1 | 78.1 | 8.53 | 37.01 | 0 | 783 | 0 | 783 |
| get-article | 16 | 1263.8 | 9.52 | 39.49 | 0 | 12652 | 0 | 12652 |
| get-article | 64 | 1991.4 | 28.63 | 64.35 | 0 | 20002 | 0 | 20002 |
| list | 1 | 62.9 | 12.69 | 39.32 | 0 | 630 | 0 | 630 |
| list | 16 | 838.5 | 18.36 | 49.71 | 0 | 8406 | 0 | 8406 |
| list | 64 | 1557.1 | 39.21 | 76.32 | 0 | 15675 | 0 | 15675 |
| list-tag | 1 | 55.2 | 15.21 | 43.14 | 0 | 552 | 0 | 552 |
| list-tag | 16 | 744.0 | 20.89 | 54.93 | 0 | 7479 | 0 | 7479 |
| list-tag | 64 | 1519.1 | 40.14 | 75.69 | 0 | 15313 | 0 | 15313 |
| feed | 1 | 48.1 | 20.96 | 57.90 | 0 | 482 | 0 | 482 |
| feed | 16 | 747.6 | 20.95 | 56.97 | 0 | 7501 | 0 | 7501 |
| feed | 64 | 1573.4 | 38.85 | 74.22 | 0 | 15836 | 0 | 15836 |
| favorite | 1 | 34.9 | 25.90 | 73.03 | 0 | 349 | 0 | 349 |
| favorite | 16 | 529.6 | 26.58 | 79.15 | 0 | 5449 | 134 | 5315 |
| favorite | 64 | 991.2 | 60.29 | 143.37 | 0 | 10327 | 320 | 10007 |

| scenario, concurrency 64 | baseline | step 1 | step 2 | steps 3+5 | final | step 6 | step 7 |
|---|---:|---:|---:|---:|---:|---:|---:|
| get-article (req/s) | 13.0 | 122.2 | 113.1 | 1991.4 | 3831.4 | 15246.7 | 14657.0 |
| list | 12.4 | 124.2 | 108.7 | 1557.1 | 3862.9 | 8027.6 | 8203.0 |
| list-tag | 5.1 | 65.9 | 72.0 | 1519.1 | 3894.2 | 7519.7 | 8071.1 |
| feed | 13.0 | 120.2 | 108.6 | 1573.4 | 3063.6 | 5474.5 | 5941.7 |
| favorite | 12.5 | 11.3 | 7.6 | 991.2 | 2440.1 | 4371.1 | 4705.9 |
| favorite retries / attempts | 0 / 188 | 0 / 175 | 1366 / 1448 | 320 / 10327 | 533 / 25000 | 624 / 44404 | 641 / 47764 |

A single request takes 8-26 ms instead of ~80 ms, and throughput now grows
with concurrency. Favorites of different articles still retry about 3% of
the time: PostgreSQL takes SERIALIZABLE predicate locks per index page, so
two favorites whose reads and writes touch the same page of the favorites
index can conflict although their rows differ. Read-only commands take no
predicate locks (a deferrable read-only transaction runs on a safe
snapshot) and never retry.

## Final run

The same code as steps 3+5 (plus the generated proof support of item 4,
which changes no Rust), rerun when the host was quieter (load average ~20 of
128 CPUs):

| scenario | conc | req/s | p50 ms | p99 ms | non-2xx | attempts | retries | commits |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| get-article | 1 | 178.7 | 5.60 | 6.47 | 0 | 1787 | 0 | 1787 |
| get-article | 16 | 2082.0 | 7.64 | 9.33 | 0 | 20841 | 0 | 20841 |
| get-article | 64 | 3831.4 | 16.59 | 20.02 | 0 | 38399 | 0 | 38399 |
| list | 1 | 145.9 | 6.83 | 9.24 | 0 | 1460 | 0 | 1460 |
| list | 16 | 1842.0 | 8.68 | 10.99 | 0 | 18440 | 0 | 18440 |
| list | 64 | 3862.9 | 16.49 | 19.53 | 0 | 38716 | 0 | 38716 |
| list-tag | 1 | 119.5 | 8.64 | 11.23 | 0 | 1195 | 0 | 1195 |
| list-tag | 16 | 1855.1 | 8.30 | 12.13 | 0 | 18568 | 0 | 18568 |
| list-tag | 64 | 3894.2 | 16.26 | 19.82 | 0 | 39027 | 0 | 39027 |
| feed | 1 | 145.9 | 7.07 | 9.42 | 0 | 1460 | 0 | 1460 |
| feed | 16 | 1469.4 | 10.90 | 13.39 | 0 | 14709 | 0 | 14709 |
| feed | 64 | 3063.6 | 20.85 | 24.48 | 0 | 30701 | 0 | 30701 |
| favorite | 1 | 141.5 | 7.21 | 10.32 | 0 | 1416 | 0 | 1416 |
| favorite | 16 | 1320.6 | 11.85 | 24.22 | 0 | 13486 | 265 | 13221 |
| favorite | 64 | 2440.1 | 25.51 | 52.18 | 0 | 25000 | 533 | 24467 |

Against the baseline, at concurrency 64: get-article 13 → 3,831 requests/s,
listing 12 → 3,863, listing by tag 5 → 3,894, feed 13 → 3,064, favorite 12
→ 2,440 (2% retries); single requests take 6-9 ms instead of ~80 ms.

## Step 6: prepared statements and pipelined plans

Every statement of a `Store` (reads, plan reads, writes, outbox rows) is
now prepared once per connection (deadpool's statement cache) and reused,
instead of being sent as text to be parsed and planned again. The reads of a
plan, and the per-table `SELECT`s of a whole-tenant load, are all sent on
the transaction's connection before any result is awaited (tokio-postgres
pipelines them), so an article view costs about one round trip instead of
five. The SQL text, its parameters and the transaction are the same, so
each read returns the same rows from the same snapshot; nothing in the
proofs changed. The differential test also runs each case's reads
pipelined.

Before (HEAD of step 5, rerun right before the change, host load ~7):

| scenario | conc | req/s | p50 ms | p99 ms | non-2xx | attempts | retries | commits |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| get-article | 1 | 193.9 | 5.50 | 6.24 | 0 | 1939 | 0 | 1939 |
| get-article | 16 | 2275.3 | 6.98 | 9.04 | 0 | 22773 | 0 | 22773 |
| get-article | 64 | 4102.6 | 15.40 | 19.12 | 0 | 41112 | 0 | 41112 |
| list | 1 | 162.5 | 5.71 | 9.27 | 0 | 1625 | 0 | 1625 |
| list | 16 | 1954.7 | 8.14 | 10.94 | 0 | 19565 | 0 | 19565 |
| list | 64 | 3780.7 | 16.84 | 20.03 | 0 | 37888 | 0 | 37888 |
| list-tag | 1 | 113.7 | 8.95 | 11.05 | 0 | 1138 | 0 | 1138 |
| list-tag | 16 | 1922.7 | 8.03 | 11.72 | 0 | 19244 | 0 | 19244 |
| list-tag | 64 | 3902.4 | 16.27 | 19.67 | 0 | 39097 | 0 | 39097 |
| feed | 1 | 155.7 | 6.08 | 9.35 | 0 | 1557 | 0 | 1557 |
| feed | 16 | 1613.9 | 10.01 | 12.61 | 0 | 16158 | 0 | 16158 |
| feed | 64 | 3176.3 | 20.07 | 24.12 | 0 | 31837 | 0 | 31837 |
| favorite | 1 | 111.5 | 9.00 | 12.01 | 0 | 1115 | 0 | 1115 |
| favorite | 16 | 1282.4 | 12.16 | 24.83 | 0 | 13104 | 265 | 12839 |
| favorite | 64 | 2375.5 | 26.26 | 53.39 | 0 | 24356 | 529 | 23827 |

After (load ~13):

| scenario | conc | req/s | p50 ms | p99 ms | non-2xx | attempts | retries | commits |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| get-article | 1 | 696.5 | 1.43 | 1.69 | 0 | 6966 | 0 | 6966 |
| get-article | 16 | 7266.6 | 2.16 | 3.10 | 0 | 72693 | 0 | 72693 |
| get-article | 64 | 15246.7 | 4.14 | 5.20 | 0 | 152642 | 0 | 152642 |
| list | 1 | 223.3 | 4.60 | 5.96 | 0 | 2233 | 0 | 2233 |
| list | 16 | 3202.2 | 5.12 | 7.39 | 0 | 32046 | 0 | 32046 |
| list | 64 | 8027.6 | 7.84 | 11.16 | 0 | 80380 | 0 | 80380 |
| list-tag | 1 | 428.9 | 2.09 | 5.63 | 0 | 4290 | 0 | 4290 |
| list-tag | 16 | 4070.4 | 3.79 | 7.07 | 0 | 40721 | 0 | 40721 |
| list-tag | 64 | 7519.7 | 8.28 | 13.26 | 0 | 75262 | 0 | 75262 |
| feed | 1 | 262.7 | 3.52 | 6.41 | 0 | 2628 | 0 | 2628 |
| feed | 16 | 3092.6 | 5.01 | 8.88 | 0 | 30942 | 0 | 30942 |
| feed | 64 | 5474.5 | 11.41 | 17.53 | 0 | 54806 | 0 | 54806 |
| favorite | 1 | 258.4 | 3.82 | 6.74 | 0 | 2585 | 0 | 2585 |
| favorite | 16 | 2896.6 | 5.32 | 11.23 | 0 | 29423 | 437 | 28986 |
| favorite | 64 | 4371.1 | 14.03 | 28.24 | 0 | 44404 | 624 | 43780 |

At concurrency 64: get-article 4,103 → 15,247 requests/s, listing 3,781 →
8,028, listing by tag 3,902 → 7,520, feed 3,176 → 5,475, favorite 2,376 →
4,371. A single get-article takes 1.4 ms instead of 5.5 ms. Retries per
favorite stay about the same (1.4% of attempts, was 2.2%).

## Step 7: a bounded statement cache

Each connection now keeps at most `EngineConfig::statement_cache` prepared
statements (default 256). Beyond that the least recently used one is
removed from the connection's cache, and dropping it closes it on the
server, so `pg_prepared_statements` stays bounded too. A test runs about
300 random read shapes through one connection with room for 8 and checks
the cache, the server's count and the rows after every read.

The SQL text of a read was already independent of its values: every value,
limit and offset is a parameter. It varies only with the read's shape and
with whether its window has a limit and a non-zero offset (at most four
texts per shape). Rendering `OFFSET $n` always, to make that one text, was
tried and dropped: PostgreSQL then estimates the generic plan as dearer
than the custom plans for offset 0, never switches to it, and re-plans the
statement on every execution (`pg_prepared_statements` showed 0 generic and
8 custom plans after 8 executions); the tag listing lost a third of its
throughput. Writes and filtered loads have one text per table and column.

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

Within noise of step 6 (a 256-statement cache holds all of Conduit's few
dozen shapes, so nothing is evicted here).

## Done since the first report

- The statement cache is bounded (step 7); it was unbounded in step 6.

## What remains slow or open

- Sign-up, login, new, edited and deleted articles and the tag list have no
  read plan and still load the whole site (login and the tag list without
  locks, as read-only commands).
- Favorites retry about 2% of the time under SERIALIZABLE page-level
  predicate locks.
