//! The fixed-width SoA layout shared by the host arenas and the device kernels.
//!
//! These are the single source of truth for every packed buffer. The device
//! crate includes this file directly (`#[path]`) so host packing and device
//! indexing can never disagree. Keep it free of `std` so it compiles on the
//! `nvptx64` target too.

/// Upper bound on rows in one batch (the output arena capacity).
pub const MAX_ROWS: usize = 1_000_000;
/// Packed searchable text per row.
pub const ROW_BYTES: usize = 512;
/// Push-rule pattern or display buffer per row.
pub const RULE_BYTES: usize = 64;
/// Fixed byte slot for one search term.
pub const TERM_SLOT: usize = 16;
/// Search terms per query.
pub const MAX_TERMS: usize = 8;
/// The query's terms buffer: `MAX_TERMS` slots of `TERM_SLOT` bytes.
pub const TERMS_BYTES: usize = TERM_SLOT * MAX_TERMS;
