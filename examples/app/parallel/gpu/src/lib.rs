//! Application-specific packing and CUDA launch adapters for the parallel example.
pub mod arena;
pub mod batch;
pub mod differential;
pub mod layout;
use batch::{Backend, Batch, Decision, Output};
use cust::{
    event::{Event, EventFlags},
    prelude::*,
};
use h5i_app_gpu_backend_nvvm::{Failure, Nvvm};
use std::time::Instant;
pub struct ParallelGpu {
    runtime: Nvvm,
    pub kernel_ms: f32,
    pub total_ms: f64,
}
impl ParallelGpu {
    pub fn new(ptx: &str) -> Result<Self, Failure> {
        Ok(Self {
            runtime: Nvvm::new(ptx)?,
            kernel_ms: 0.,
            total_ms: 0.,
        })
    }
}
impl Backend<Batch, Decision, Output> for ParallelGpu {
    type Error = Failure;
    fn run(&mut self, decision: Decision, b: &Batch) -> Result<Output, Failure> {
        if b.n > 1_000_001 {
            return Err("output arena exceeds probe capacity".into());
        }
        if b.n == 0 {
            return Ok(Output {
                values: vec![],
                errors: vec![],
            });
        }
        let start = Instant::now();
        let text = DeviceBuffer::from_slice(&b.text)?;
        let mut out = Output {
            values: vec![u32::MAX; b.n],
            errors: vec![u32::MAX; b.n],
        };
        let values = DeviceBuffer::from_slice(&out.values)?;
        let errors = DeviceBuffer::from_slice(&out.errors)?;
        let begin = Event::new(EventFlags::DEFAULT)?;
        let end = Event::new(EventFlags::DEFAULT)?;
        let stream = self.runtime.stream();
        let grid = (b.n as u32).div_ceil(128);
        match decision {
            Decision::Search => {
                let tags = DeviceBuffer::from_slice(&b.tags)?;
                let terms = DeviceBuffer::from_slice(&b.terms)?;
                let f = self.runtime.module().get_function("search_batch")?;
                begin.record(stream)?;
                // All output pointers have n disjoint slots. The device checks each
                // input capacity before indexing; lengths are in elements, not bytes.
                unsafe {
                    cust::launch!(f<<<grid,128,0,stream>>>(text.as_device_ptr(),text.len(),tags.as_device_ptr(),tags.len(),terms.as_device_ptr(),terms.len(),b.count,b.any,b.negated,b.required,values.as_device_ptr(),errors.as_device_ptr(),b.n))?;
                }
                end.record(stream)?;
                end.synchronize()?;
            }
            Decision::Push => {
                let patterns = DeviceBuffer::from_slice(&b.patterns)?;
                let displays = DeviceBuffer::from_slice(&b.displays)?;
                let members = DeviceBuffer::from_slice(&b.members)?;
                let powers = DeviceBuffer::from_slice(&b.powers)?;
                let f = self.runtime.module().get_function("push_batch")?;
                begin.record(stream)?;
                unsafe {
                    cust::launch!(f<<<grid,128,0,stream>>>(text.as_device_ptr(),text.len(),patterns.as_device_ptr(),patterns.len(),displays.as_device_ptr(),displays.len(),members.as_device_ptr(),members.len(),powers.as_device_ptr(),powers.len(),b.minimum,b.maximum,b.required,b.enabled,b.require_display,values.as_device_ptr(),errors.as_device_ptr(),b.n))?;
                }
                end.record(stream)?;
                end.synchronize()?;
            }
        }
        self.kernel_ms = end.elapsed_time_f32(&begin)?;
        values.copy_to(&mut out.values)?;
        errors.copy_to(&mut out.errors)?;
        self.total_ms = start.elapsed().as_secs_f64() * 1000.;
        Ok(out)
    }
}
