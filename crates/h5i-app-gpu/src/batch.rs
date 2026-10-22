//! Application-owned input, operation, and result types across execution backends.

/// A backend owns execution resources. Implementing this trait does not compile
/// Rust for CUDA: the application provides an explicit device entry point/ABI.
/// Inputs must describe independent computations over immutable snapshots if
/// they are batched. Conflict resolution and state commit remain with the app.
pub trait Backend<Input: ?Sized, Operation, Response> {
    type Error;
    fn run(&mut self, operation: Operation, input: &Input) -> Result<Response, Self::Error>;
}

/// Run the application's ordinary Rust function for each independent input.
/// A row error discards the batch; callers receive no partial output.
/// Functions used here should have no externally visible side effects.
pub struct Cpu<F>(pub F);
impl<I, O, E, F> Backend<[I], (), Vec<O>> for Cpu<F>
where
    F: FnMut(&I) -> Result<O, E>,
{
    type Error = E;
    fn run(&mut self, (): (), input: &[I]) -> Result<Vec<O>, E> {
        input.iter().map(&mut self.0).collect()
    }
}
