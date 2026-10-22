#![cfg(feature = "gpu")]
#![no_std]
#[path = "../../src/kernel.rs"]
mod shared;
use cuda_std::{kernel, thread};
#[kernel]
pub unsafe fn quote_batch(rows: *const [u64; 3], out: *mut [u64; 2], len: usize) {
    let i = thread::index_1d() as usize;
    if i < len {
        unsafe {
            *out.add(i) = shared::quote(*rows.add(i));
        }
    }
}
