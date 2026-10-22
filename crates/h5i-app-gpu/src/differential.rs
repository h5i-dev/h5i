//! Compare application-provided reference and candidate executions. These are
//! empirical checks, not a proof of compiler or CPU/device equivalence.
use crate::batch::Backend;

#[derive(Debug, PartialEq, Eq)]
pub enum DifferentialError<C, G, O> {
    Reference(C),
    Candidate(G),
    Mismatch { expected: O, actual: O },
}

/// Compare the entire response, including application error/status columns.
/// Supply a comparator for explicit float tolerances or state invariants.
/// Use an independent reference implementation where practical.
pub fn compare<I: ?Sized, Op: Clone, O, C, G>(
    reference: &mut C,
    candidate: &mut G,
    operation: Op,
    input: &I,
    equivalent: impl FnOnce(&O, &O) -> bool,
) -> Result<(), DifferentialError<C::Error, G::Error, O>>
where
    C: Backend<I, Op, O>,
    G: Backend<I, Op, O>,
{
    let expected = reference
        .run(operation.clone(), input)
        .map_err(DifferentialError::Reference)?;
    let actual = candidate
        .run(operation, input)
        .map_err(DifferentialError::Candidate)?;
    if equivalent(&expected, &actual) {
        Ok(())
    } else {
        Err(DifferentialError::Mismatch { expected, actual })
    }
}

/// Check partition independence, output cardinality and repeat determinism for
/// a row-wise computation. Generators supply boundary/malformed cases as input.
/// Stateful or order-dependent transitions should not use this contract.
pub fn check_batch<I, O: PartialEq, B>(
    backend: &mut B,
    input: &[I],
    chunk_size: core::num::NonZeroUsize,
) -> Result<(), BatchCheckError<B::Error, O>>
where
    B: Backend<[I], (), Vec<O>>,
{
    let whole = backend.run((), input).map_err(BatchCheckError::Execution)?;
    if whole.len() != input.len() {
        return Err(BatchCheckError::Length {
            expected: input.len(),
            actual: whole.len(),
        });
    }
    let repeat = backend.run((), input).map_err(BatchCheckError::Execution)?;
    if whole != repeat {
        return Err(BatchCheckError::Nondeterministic {
            first: whole,
            repeated: repeat,
        });
    }
    let mut joined = Vec::with_capacity(input.len());
    for part in input.chunks(chunk_size.get()) {
        let out = backend.run((), part).map_err(BatchCheckError::Execution)?;
        if out.len() != part.len() {
            return Err(BatchCheckError::Length {
                expected: part.len(),
                actual: out.len(),
            });
        }
        joined.extend(out);
    }
    if whole != joined {
        return Err(BatchCheckError::Partition { whole, joined });
    }
    Ok(())
}
#[derive(Debug, PartialEq, Eq)]
pub enum BatchCheckError<E, O> {
    Execution(E),
    Length { expected: usize, actual: usize },
    Nondeterministic { first: Vec<O>, repeated: Vec<O> },
    Partition { whole: Vec<O>, joined: Vec<O> },
}
