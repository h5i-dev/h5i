//! Explicit batch dispatch. No operation is replayed after a backend error.
use crate::batch::Backend;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionPolicy {
    Cpu,
    /// Require a GPU; absence is an error.
    Gpu,
    /// Use the CPU below this size or when no GPU backend was initialized.
    Auto {
        min_gpu_rows: usize,
    },
}
#[derive(Debug, PartialEq, Eq)]
pub enum ExecutionError<C, G> {
    GpuUnavailable,
    OutputLength { expected: usize, actual: usize },
    Cpu(C),
    Gpu(G),
}
impl<C: core::fmt::Display, G: core::fmt::Display> core::fmt::Display for ExecutionError<C, G> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::GpuUnavailable => {
                f.write_str("GPU execution requested without an initialized backend")
            }
            Self::OutputLength { expected, actual } => {
                write!(f, "expected {expected} output rows, got {actual}")
            }
            Self::Cpu(e) => write!(f, "CPU execution failed: {e}"),
            Self::Gpu(e) => write!(f, "GPU execution failed: {e}"),
        }
    }
}
impl<C: std::error::Error + 'static, G: std::error::Error + 'static> std::error::Error
    for ExecutionError<C, G>
{
}

/// Select a backend before executing. `Auto` falls back only on absence, never
/// on launch, conversion, validation, or readback failure. The app decides how
/// to report initialization errors before supplying `None` here.
pub fn execute_batch<I, O, C, G>(
    policy: ExecutionPolicy,
    cpu: &mut C,
    gpu: Option<&mut G>,
    input: &[I],
) -> Result<Vec<O>, ExecutionError<C::Error, G::Error>>
where
    C: Backend<[I], (), Vec<O>>,
    G: Backend<[I], (), Vec<O>>,
{
    let output = match policy {
        ExecutionPolicy::Cpu => cpu.run((), input).map_err(ExecutionError::Cpu),
        ExecutionPolicy::Auto { min_gpu_rows } if input.len() < min_gpu_rows || gpu.is_none() => {
            cpu.run((), input).map_err(ExecutionError::Cpu)
        }
        _ => gpu
            .ok_or(ExecutionError::GpuUnavailable)?
            .run((), input)
            .map_err(ExecutionError::Gpu),
    }?;
    if output.len() != input.len() {
        return Err(ExecutionError::OutputLength {
            expected: input.len(),
            actual: output.len(),
        });
    }
    Ok(output)
}
