//! The Axum-based application framework for h5i.
//!
//! Application logic is a pure Rust function, the *kernel*, which is
//! extracted to Lean 4 with Aeneas so its properties can be proven. The
//! shell around it — HTTP, authentication, PostgreSQL — is ordinary Rust
//! and is trusted. `docs/app/TRUST.md` in the h5i repository lists exactly
//! what is proven and what is assumed.
//!
//! This crate re-exports the `h5i-app-*` crates behind features, so a
//! server needs one dependency:
//!
//! ```toml
//! [dependencies]
//! h5i-app = { version = "0.1", features = ["http", "postgres"] }
//! ```
//!
//! The core contract ([`Kernel`], [`TenantId`], …) is always available at
//! the crate root. A kernel crate should depend on `h5i-app-sql` and
//! `h5i-app-schema` directly rather than on this crate, so that extraction
//! sees only the code it has to translate.
//!
//! `schema!` expands to paths through `::h5i_app_pg` and `::h5i_app_core`,
//! so a server crate that uses its server-side mapping also names those two
//! crates in its own `Cargo.toml`.

pub use h5i_app_core::*;

/// The axum shell: [`http::Actor`], [`http::H5iApp`], [`http::rpc_router`].
#[cfg(feature = "http")]
pub use h5i_app_http as http;

/// The PostgreSQL engine.
#[cfg(feature = "postgres")]
pub use h5i_app_pg as pg;

/// `schema!`: kernel row types and their table mappings.
#[cfg(feature = "schema")]
pub use h5i_app_schema as schema;

/// The verified planner from write sets to keyed SQL statements.
#[cfg(feature = "sql")]
pub use h5i_app_sql as sql;

/// The verified compiler from planned statements to PostgreSQL text.
#[cfg(feature = "pgsql")]
pub use h5i_app_pgsql as pgsql;

/// The verified JSON writer for replies.
#[cfg(feature = "json")]
pub use h5i_app_json as json;

/// Verified bearer-token encoding and parsing.
#[cfg(feature = "token")]
pub use h5i_app_token as token;
