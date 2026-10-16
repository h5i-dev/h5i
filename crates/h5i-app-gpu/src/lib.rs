//! Experimental GPU compilation probes; not a production executor.
//! The wrapper calls the existing calculator source rather than a twin kernel.
use calculator_kernel::{Error, Op, compute};

/// Explicit per-thread error tags, zero on success. No panic on arithmetic errors.
pub fn calculate(op: u32, a: u64, b: u64) -> (u64, u32) {
    let op = match op {
        0 => Op::Add,
        1 => Op::Sub,
        2 => Op::Mul,
        3 => Op::Div,
        _ => return (0, 4),
    };
    match compute(op, a, b) {
        Ok(value) => (value, 0),
        Err(Error::Overflow) => (0, 1),
        Err(Error::Underflow) => (0, 2),
        Err(Error::DivByZero) => (0, 3),
    }
}

#[cfg(feature = "calculator-probe")]
use cuda_device::{DisjointSlice, kernel, thread};

/// One command per thread; separate value/error columns. Caller supplies equal lengths.
#[cfg(feature = "calculator-probe")]
#[kernel]
pub fn calculator_batch(
    ops: &[u32],
    a: &[u64],
    b: &[u64],
    mut values: DisjointSlice<u64>,
    mut errors: DisjointSlice<u32>,
) {
    let i = thread::index_1d();
    if let Some(out) = values.get_mut(i) {
        if let Some(err) = errors.get_mut(i) {
            if i.get() >= ops.len() || i.get() >= a.len() || i.get() >= b.len() {
                *out = 0;
                *err = 5;
            } else {
                let (v, e) = calculate(ops[i.get()], a[i.get()], b[i.get()]);
                *out = v;
                *err = e;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arithmetic_errors_are_explicit() {
        assert_eq!(calculate(0, u64::MAX, 1), (0, 1));
        assert_eq!(calculate(1, 0, 1), (0, 2));
        assert_eq!(calculate(2, u64::MAX, 2), (0, 1));
        assert_eq!(calculate(3, 1, 0), (0, 3));
        assert_eq!(calculate(4, 0, 0), (0, 4));
        assert_eq!(calculate(0, 1, 2), (3, 0));
    }
}

// Compile-only marker shim: upstream #[kernel] generates this trait impl even
// when no host launcher is linked. Runtime builds use the real cuda-host trait.
#[cfg(feature = "calculator-probe")]
#[allow(dead_code)]
mod cuda_host {
    pub trait CudaKernel {
        const PTX_NAME: &'static str;
    }
}
