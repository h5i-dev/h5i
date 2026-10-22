#![cfg(feature = "gpu")]
#![allow(improper_ctypes_definitions)]
#![no_std]
#[path = "../../../kernel/src/lib.rs"]
mod shared;
// The SoA widths are the single source of truth shared with the host crate.
#[path = "../../src/layout.rs"]
mod layout;
use cuda_std::{kernel, thread};
use layout::{MAX_ROWS, ROW_BYTES, RULE_BYTES};
#[kernel]
pub unsafe fn search_batch(
    rows: &[u8],
    tags: &[u64],
    terms: &[u8],
    count: usize,
    any: bool,
    negated: u32,
    required: u64,
    out: *mut u32,
    errors: *mut u32,
    n: usize,
) {
    let i = thread::index_1d() as usize;
    if i < n {
        let (value, error) = if i >= MAX_ROWS || i >= tags.len() || i >= rows.len() / ROW_BYTES {
            (0, 1)
        } else {
            (
                shared::bookmark_search(
                    &rows[i * ROW_BYTES..i * ROW_BYTES + ROW_BYTES],
                    terms,
                    count,
                    any,
                    negated,
                    tags[i],
                    required,
                ) as u32,
                0,
            )
        };
        unsafe {
            *out.add(i) = value;
            *errors.add(i) = error;
        }
    }
}
#[kernel]
pub unsafe fn push_batch(
    events: &[u8],
    patterns: &[u8],
    displays: &[u8],
    members: &[u64],
    powers: &[u64],
    minimum: u64,
    maximum: u64,
    required: u64,
    enabled: bool,
    require_display: bool,
    out: *mut u32,
    errors: *mut u32,
    n: usize,
) {
    let i = thread::index_1d() as usize;
    if i < n {
        let (value, error) = if i >= MAX_ROWS
            || i >= events.len() / ROW_BYTES
            || i >= patterns.len() / RULE_BYTES
            || i >= displays.len() / RULE_BYTES
            || i >= members.len()
            || i >= powers.len()
        {
            (0, 1)
        } else {
            (
                shared::push_rule(
                    &events[i * ROW_BYTES..i * ROW_BYTES + ROW_BYTES],
                    &patterns[i * RULE_BYTES..i * RULE_BYTES + RULE_BYTES],
                    &displays[i * RULE_BYTES..i * RULE_BYTES + RULE_BYTES],
                    members[i],
                    minimum,
                    maximum,
                    powers[i],
                    required,
                    enabled,
                    require_display,
                ),
                0,
            )
        };
        unsafe {
            *out.add(i) = value;
            *errors.add(i) = error;
        }
    }
}
