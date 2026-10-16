//! Trusted NVVM launch/ABI shell. Input slices become pointer/length pairs.
use cust::{
    event::{Event, EventFlags},
    prelude::*,
};
use h5i_app_gpu::batch::{Backend, Batch, Decision, Output};
use std::{error::Error, time::Instant};
pub type Failure = Box<dyn Error>;
pub struct Nvvm {
    module: Module,
    stream: Stream,
    _context: Context,
    pub kernel_ms: f32,
    pub total_ms: f64,
}
impl Nvvm {
    pub fn new(ptx: &str) -> Result<Self, Failure> {
        let context = cust::quick_init()?;
        Ok(Self {
            module: Module::from_ptx(ptx, &[])?,
            stream: Stream::new(StreamFlags::NON_BLOCKING, None)?,
            _context: context,
            kernel_ms: 0.,
            total_ms: 0.,
        })
    }
}
impl Backend for Nvvm {
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
        let stream = &self.stream;
        let grid = (b.n as u32).div_ceil(128);
        match decision {
            Decision::Search => {
                let tags = DeviceBuffer::from_slice(&b.tags)?;
                let terms = DeviceBuffer::from_slice(&b.terms)?;
                let f = self.module.get_function("search_batch")?;
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
                let f = self.module.get_function("push_batch")?;
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
