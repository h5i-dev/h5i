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
