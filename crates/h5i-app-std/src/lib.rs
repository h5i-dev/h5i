//! What a kernel misses from `std` in the Aeneas subset: text, sets and maps.
//!
//! Aeneas has no model of `String`, `HashSet` or `HashMap`, so kernels keep
//! text as `Vec<u8>`, a set as a `Vec<T>` and a map as a `Vec<(K, V)>`, or as
//! the [`hashmap::HashMap`] here when it is looked up often. This
//! crate holds the operations on those once, so a kernel calls
//! `bytes::starts_with` instead of writing and proving its own loop. Every
//! function has a `@[step]` spec in `proofs/StdSpecs.lean`, stated over the
//! list the vector holds; `H5iAppLib.Text`, `Sets`, `Buckets` and `Graph` prove
//! the facts about those lists (prefix order, subset, lookups, reachability).
//!
//! A kernel extracts the functions it calls with its own code (`charon
//! --include h5i_app_std`), and `h5i app extract` copies the specs next to the
//! kernel's Lean when its `h5i-app.toml` says `std-specs = true`.
//!
//! Written in the subset Charon + Aeneas translate: `while` loops, no
//! closures, no `?`, `&Vec` rather than slices.

pub mod bytes;
pub mod graph;
pub mod hashmap;
pub mod map;
pub mod set;
pub mod time;
