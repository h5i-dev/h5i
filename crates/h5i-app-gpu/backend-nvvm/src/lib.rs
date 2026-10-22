//! Rust-CUDA host adapter. Applications own PTX, device-compatible layouts and
//! entry points. No application kernel or tutorial is linked into this crate.
use cust::prelude::*;
use std::error::Error;
pub type Failure = Box<dyn Error>;

pub struct Nvvm {
    module: Module,
    stream: Stream,
    _context: Context,
}
impl Nvvm {
    pub fn new(ptx: &str) -> Result<Self, Failure> {
        let context = cust::quick_init()?;
        Ok(Self {
            module: Module::from_ptx(ptx, &[])?,
            stream: Stream::new(StreamFlags::NON_BLOCKING, None)?,
            _context: context,
        })
    }

    /// Execute independent rows using the ABI `(input: *const I, output:
    /// *mut O, len: usize)`. Outputs are initialized with `O::default()` and
    /// copied back only after the stream completes. Empty batches do not launch.
    ///
    /// # Safety
    /// The named PTX function must have exactly this ABI and matching layouts.
    /// Each thread must bounds-check its index against `len`, access only the
    /// supplied allocations, and fully write valid `O` values to its disjoint
    /// output row. It must not write the input, race with other threads, or
    /// retain pointers after completion. It must implement the
    /// application's pure computation with no external effects. `DeviceCopy`
    /// alone does not establish that a PTX function obeys this contract.
    pub unsafe fn map<I, O>(&mut self, entry: &str, input: &[I]) -> Result<Vec<O>, Failure>
    where
        I: cust::memory::DeviceCopy,
        O: cust::memory::DeviceCopy + Default + Clone,
    {
        let mut output = vec![O::default(); input.len()];
        if input.is_empty() {
            return Ok(output);
        }
        let grid = u32::try_from(input.len().div_ceil(128))
            .map_err(|_| "batch exceeds CUDA grid capacity")?;
        let rows = DeviceBuffer::from_slice(input)?;
        let results = DeviceBuffer::from_slice(&output)?;
        let function = self.module.get_function(entry)?;
        let stream = &self.stream;
        // Drain the stream even if launch reports an error, before buffers drop.
        let launch = unsafe {
            cust::launch!(function<<<grid, 128, 0, stream>>>(
                rows.as_device_ptr(), results.as_device_ptr(), input.len()
            ))
        };
        let completion = stream.synchronize();
        launch?;
        completion?;
        results.copy_to(&mut output)?;
        Ok(output)
    }

    /// Escape hatch for application-specific multi-buffer launch adapters.
    /// The caller is responsible for layout, aliasing, capacity and lifetimes.
    pub fn module(&self) -> &Module {
        &self.module
    }
    pub fn stream(&self) -> &Stream {
        &self.stream
    }
}
