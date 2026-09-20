//! Making untrusted strings safe to show and safe to store.

pub use h5i_error::redact::{is_bidi_control, sanitize_block, sanitize_display};
