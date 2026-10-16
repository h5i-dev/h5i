//! Minimal device failure: a bounded slice view, returning only a scalar.
use cuda_device::{kernel,thread,DisjointSlice};
mod cuda_host { pub trait CudaKernel { const PTX_NAME: &'static str; } }
fn decision(bytes: &[u8]) -> bool { bytes[0] == b'a' }
#[kernel]
pub fn slice_probe(bytes: &[u8], mut out: DisjointSlice<u32>) {
    let i=thread::index_1d();
    if let Some(value)=out.get_mut(i) {
        if bytes.len() >= 16 { *value=decision(&bytes[0..16]) as u32; }
        else { *value=0; }
    }
}
fn main() {}
