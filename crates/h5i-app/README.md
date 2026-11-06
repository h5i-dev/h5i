# h5i-app

A Rust application framework for [h5i](https://github.com/h5i-dev/h5i).
Write application logic in Rust and prove its properties in Lean 4.

[![Crates.io](https://img.shields.io/crates/v/h5i-app)](https://crates.io/crates/h5i-app)

```toml
[dependencies]
h5i-app = { version = "0.5", features = ["http", "postgres"] }
```

| Crate | Role |
|---|---|
| `h5i-app` | feature-gated entry point for web and database integrations |
| `h5i-app-core` | the `Kernel` contract and an in-memory reference engine |
| `h5i-app-http` | Axum integration: `Actor`, `H5iApp::respond`, `rpc_router` |
| `h5i-app-pg` | the PostgreSQL engine |
| `h5i-app-schema` | `schema!`: row types, table mappings, generated Lean |
| `h5i-app-sql` | write sets to keyed SQL statements (extracted, proven) |
| `h5i-app-pgsql` | statements to PostgreSQL text (extracted, proven) |
| `h5i-app-json` | the reply JSON writer (extracted, proven) |
| `h5i-app-token` | bearer-token encoding and parsing (extracted, proven) |
| `h5i-app-std` | extractable kernel utilities and reusable verified authorization guards |
| `h5i-app-h1` | `no_std`, heap-free HTTP/1.1 parsing and framing (extracted, proven) |
| `h5i-app-embedded` | async, heap-free HTTP server for embedded kernels |
| `h5i-app-gpu` | optional batch execution adapters with CPU fallback and differential checks |

Kernel crates depend directly on the extractable components they use, so extraction does not pull in Axum or PostgreSQL drivers. Embedded and GPU support are separate crates, not `h5i-app` feature flags.

## High level features

- Write application logic as pure Rust functions and prove custom properties in Lean 4 via [Aeneas](https://github.com/AeneasVerif/aeneas).
- Reuse verified components and generic guarantees, including authorization guards and storage compilation, without reproving them for every application.
- Serve kernels with [Axum](https://github.com/tokio-rs/axum) or an async `no_std` embedded HTTP server.
- Run web requests in SERIALIZABLE PostgreSQL transactions, with retries and idempotency keys.
- Declare tables once with `schema!` to generate Rust mappings and Lean proof artifacts.
- Optionally accelerate eligible batch workloads on GPUs, with CPU fallback and differential testing.

The Axum/PostgreSQL path separates extracted logic from trusted infrastructure:

```mermaid
flowchart LR
    H["HTTP transport: axum, hyper (trusted)"] --> K["kernel (extracted)"]
    K --> S["storage: plan, SQL compiler (extracted)"]
    S --> DB["engine, PostgreSQL (trusted)"]

    K -. Aeneas .-> A["application-specific proofs"]
    S -. Aeneas .-> B["framework-provided proofs"]
    DB -. "SQL model" .-> B
    SP["Spec (reviewed)"] --> A
    A --> D["db_inv"]
    B --> D
```

## Usage example

The kernel is one function that decides what a command does. This one, from
[the calculator tutorial](https://github.com/h5i-dev/h5i/blob/main/examples/app/calculator/TUTORIAL.md), keeps one number per user:

```rust
pub fn transition(actor: &Principal, snap: &Snapshot, cmd: &Command) -> Result<(Option<Memory>, Reply), Error> {
    match cmd {
        Command::Set { value } => Ok((Some(Memory { user: actor.user, value: *value }), Reply::Value(*value))),
        Command::Apply { op, arg } => {
            let m = memory_of(&snap.memories, actor.user);
            match compute(*op, m, *arg) {
                Ok(v) => Ok((Some(Memory { user: actor.user, value: v }), Reply::Value(v))),
                Err(e) => Err(e),
            }
        }
        Command::Get => Ok((None, Reply::Value(memory_of(&snap.memories, actor.user)))),
    }
}
```

The server around it is an ordinary Axum application:

```rust
let engine = Arc::new(Engine::<Calc, CalcStore>::new(pool(&url, 8)?, EngineConfig::default()));
engine.install_schema().await?;
let app = H5iApp::new(engine, HmacAuth::<Calc>::new(secret, principal));
let router = Router::new().route("/healthz", get(|| async { "ok" })).merge(rpc_router(app));
axum::serve(TcpListener::bind("127.0.0.1:8080").await?, router).await?;
```

`pool` connects over plain TCP. For certificate-verified TLS to PostgreSQL, enable the `tls` feature and use `pool_tls` with `Roots::WebPki` or `Roots::PemFile(ca)`. For the full trust boundary and assumptions, see [what h5i-app proves and assumes](https://github.com/h5i-dev/h5i/blob/main/docs/design/design-app-trust.md).

After the kernel is translated to Lean, you can prove properties of it, for
example that after any successful command a `get` by the same user returns its
result:

```lean
theorem get_after (a : Principal) (s s' : Snapshot) (c : Command) (w : Option Memory) (v : U64)
    (hroom : s.memories.length < Usize.max)
    (ht : transition a s c = ok (.Ok (w, .Value v))) (hs : apply s w = ok s') :
    transition a s' .Get = ok (.Ok (none, .Value v))
```

## License

This project is licensed under the [Apache-2.0 license](https://github.com/h5i-dev/h5i/blob/main/LICENSE).
