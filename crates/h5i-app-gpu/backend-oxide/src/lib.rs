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

#[cfg(any(feature = "calculator-probe", feature = "search-probe", feature = "push-probe"))]
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
// when no host launcher is linked. A launcher would need the real cuda-host trait.
#[cfg(any(feature = "calculator-probe", feature = "search-probe", feature = "push-probe"))]
#[allow(dead_code)]
mod cuda_host {
    pub trait CudaKernel {
        const PTX_NAME: &'static str;
    }
}

/// Fixed-width flat SoA: 512 bytes per bookmark, one tag mask per row.
/// Caller supplies equal output/error lengths, one slot per requested row.
#[cfg(feature = "search-probe")]
#[kernel]
pub fn search_batch(rows: &[u8], tags: &[u64], terms: &[u8], count: usize,
    any: bool, negated: u32, required: u64, mut out: DisjointSlice<u32>, mut errors: DisjointSlice<u32>) {
    let i = thread::index_1d();
    if let Some(value) = out.get_mut(i) {
        if let Some(error) = errors.get_mut(i) {
            if i.get() >= tags.len() || i.get() >= rows.len() / 512 || i.get() >= 1_000_000 {
                *value = 0; *error = 1;
            } else {
                let start = i.get() * 512;
                *value = parallel_kernel::bookmark_search(&rows[start..start+512], terms, count, any, negated, tags[i.get()], required) as u32;
                *error = 0;
            }
        }
    }
}

/// One flattened (event,rule) pair per thread. Patterns/display names are
/// bounded to 64 bytes and event content to 512, with zero padding.
/// Caller supplies equal output/error lengths, one slot per requested pair.
#[cfg(feature = "push-probe")]
#[kernel]
pub fn push_batch(events: &[u8], patterns: &[u8], displays: &[u8], members: &[u64], powers: &[u64],
    minimum: u64, maximum: u64, required: u64, enabled: bool, require_display: bool,
    mut out: DisjointSlice<u32>, mut errors: DisjointSlice<u32>) {
    let i = thread::index_1d();
    if let Some(value) = out.get_mut(i) {
        if let Some(error) = errors.get_mut(i) {
            let n = i.get();
            if n >= events.len()/512 || n >= patterns.len()/64 || n >= displays.len()/64 || n >= members.len() || n >= powers.len() || n >= 1_000_000 {
                *value = 0; *error = 1;
            } else {
                *value = parallel_kernel::push_rule(&events[n*512..n*512+512], &patterns[n*64..n*64+64], &displays[n*64..n*64+64], members[n], minimum, maximum, powers[n], required, enabled, require_display);
                *error = 0;
            }
        }
    }
}


