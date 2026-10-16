#![cfg(feature = "gpu")]
#![allow(improper_ctypes_definitions)]
#![no_std]
#[path = "../../../../../examples/app/parallel/kernel/src/lib.rs"]
mod shared;
use cuda_std::{kernel, thread};
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
        let (value, error) = if i >= 1_000_000 || i >= tags.len() || i >= rows.len() / 512 {
            (0, 1)
        } else {
            (
                shared::bookmark_search(
                    &rows[i * 512..i * 512 + 512],
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
        let (value, error) = if i >= 1_000_000
            || i >= events.len() / 512
            || i >= patterns.len() / 64
            || i >= displays.len() / 64
            || i >= members.len()
            || i >= powers.len()
        {
            (0, 1)
        } else {
            (
                shared::push_rule(
                    &events[i * 512..i * 512 + 512],
                    &patterns[i * 64..i * 64 + 64],
                    &displays[i * 64..i * 64 + 64],
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
