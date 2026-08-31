//! Making untrusted strings safe to show and safe to store.
//!
//! Two different defenses live here, both applied at the boundary where
//! box-written or externally-fetched data reaches a host-side surface:
//!
//! * [`sanitize_display`] neutralises terminal control sequences before a
//!   string is printed.
//! * Secret scrubbing is [`crate::secrets::redact_text`], applied by
//!   [`crate::receipt::append`] before anything is written.
//!
//! The display sanitisers themselves now live in `h5i-error`, one crate
//! further down, and are re-exported here so every call site keeps its path.
//! They moved because `h5i-sandbox` sits *below* this crate and could not
//! reach them: `sandbox::validate_image` worked around that with `{:?}`, and
//! `microvm::tail_service_log` did not work around it at all. It printed a
//! box-written service log to the operator's terminal raw. A guard that half
//! the code cannot reach is a guard half the code will skip.

pub use h5i_error::redact::{sanitize_block, sanitize_display};
