mod kernel;
use h5i_app_gpu::{batch::Cpu, differential::check_batch};
use std::{convert::Infallible, num::NonZeroUsize};

#[cfg(feature = "gpu")]
struct Quotes(h5i_app_gpu_backend_nvvm::Nvvm);
#[cfg(feature = "gpu")]
impl h5i_app_gpu::batch::Backend<[[u64; 3]], (), Vec<[u64; 2]>> for Quotes {
    type Error = h5i_app_gpu_backend_nvvm::Failure;
    fn run(&mut self, (): (), input: &[[u64; 3]]) -> Result<Vec<[u64; 2]>, Self::Error> {
        // This application's PTX entry uses the matching pointer/pointer/usize
        // ABI, bounds-checks every row, and writes two initialized u64 fields.
        unsafe { self.0.map("quote_batch", input) }
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut cpu = Cpu(|row: &[u64; 3]| Ok::<_, Infallible>(kernel::quote(*row)));
    let cases = [
        [0, 1, 0],
        [199, 3, 15],
        [u64::MAX, 1, 99],
        [u64::MAX, 2, 0],
        [100, 1, 101],
    ];
    for n in [0, 1, 127, 128, 129, 257, 1025] {
        let rows: Vec<_> = (0..n).map(|i| cases[i % cases.len()]).collect();
        check_batch(&mut cpu, &rows, NonZeroUsize::new(128).unwrap()).unwrap();
        #[cfg(feature = "gpu")]
        {
            use h5i_app_gpu::{
                differential::compare,
                runtime::{ExecutionPolicy, execute_batch},
            };
            // Missing PTX or unavailable CUDA fails this explicit GPU check.
            let path = std::env::args()
                .nth(1)
                .ok_or("pass discount device PTX path")?;
            let ptx = std::fs::read_to_string(path)?;
            let mut gpu = Quotes(h5i_app_gpu_backend_nvvm::Nvvm::new(&ptx)?);
            compare(&mut cpu, &mut gpu, (), &rows[..], PartialEq::eq).unwrap();
            check_batch(&mut gpu, &rows, NonZeroUsize::new(128).unwrap()).unwrap();
            let out = execute_batch(ExecutionPolicy::Gpu, &mut cpu, Some(&mut gpu), &rows).unwrap();
            assert_eq!(
                out,
                rows.iter().map(|r| kernel::quote(*r)).collect::<Vec<_>>()
            );
            // A missing symbol must propagate as an error, not fabricated output.
            if n > 0 {
                assert!(unsafe { gpu.0.map::<_, [u64; 2]>("missing_entry", &rows) }.is_err());
            }
        }
    }
    #[cfg(feature = "gpu")]
    println!("discount: CPU/CUDA equivalence and batch conformance passed");
    #[cfg(not(feature = "gpu"))]
    println!("discount: CPU batch conformance passed");
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::kernel::quote;
    #[test]
    fn integer_semantics_and_errors() {
        assert_eq!(quote([199, 3, 15]), [508, 0]);
        assert_eq!(quote([u64::MAX, 1, 100]), [0, 0]);
        assert_eq!(quote([u64::MAX, 1, 0]), [u64::MAX, 0]);
        assert_eq!(quote([u64::MAX, 2, 0]), [0, 2]);
        assert_eq!(quote([1, 1, 101]), [0, 1]);
        for price in [0u64, 1, 99, 100, 199, u64::MAX] {
            for pct in 0..=100u64 {
                let expected = price as u128 - (price as u128 * pct as u128 / 100);
                assert_eq!(quote([price, 1, pct]), [expected as u64, 0]);
            }
        }
    }
}
