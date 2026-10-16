//! Compile-only minimal reproduction, no CUDA host runtime required.
use cuda_device::{kernel, thread, DisjointSlice};
#[allow(dead_code)]
mod cuda_host { pub trait CudaKernel { const PTX_NAME: &'static str; } }
#[derive(Clone, Copy)]
enum Failure { Overflow(u64) }
#[inline(never)]
fn checked(a: u64) -> Result<u64, Failure> {
    if a == u64::MAX { Err(Failure::Overflow(a)) } else { Ok(a + 1) }
}
#[kernel]
fn result_probe(a: &[u64], mut out: DisjointSlice<u64>) {
    let i = thread::index_1d();
    if let Some(dst) = out.get_mut(i) {
        if i.get() < a.len() {
            *dst = match checked(a[i.get()]) { Ok(v) => v, Err(Failure::Overflow(v)) => v };
        }
    }
}
fn main() {}
