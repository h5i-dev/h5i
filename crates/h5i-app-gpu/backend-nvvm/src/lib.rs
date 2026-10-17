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

use h5i_app_gpu::linkding::{Query, Rows};
pub struct ResidentLinkding {
    fields: DeviceBuffer<u8>,
    lengths: DeviceBuffer<u32>,
    tags: DeviceBuffer<u64>,
    meta: DeviceBuffer<u32>,
    values: DeviceBuffer<u32>,
    errors: DeviceBuffer<u32>,
    n: usize,
}
impl Nvvm {
    pub fn upload_linkding(&self, r: &Rows) -> Result<ResidentLinkding, Failure> {
        let n = r.len();
        Ok(ResidentLinkding {
            fields: DeviceBuffer::from_slice(&r.fields)?,
            lengths: DeviceBuffer::from_slice(&r.lengths)?,
            tags: DeviceBuffer::from_slice(&r.tags)?,
            meta: DeviceBuffer::from_slice(&r.meta)?,
            values: DeviceBuffer::from_slice(&vec![0; n])?,
            errors: DeviceBuffer::from_slice(&vec![0; n])?,
            n,
        })
    }
    pub fn linkding(&mut self, r: &ResidentLinkding, q: &Query) -> Result<Output, Failure> {
        let start = Instant::now();
        if r.n == 0 {
            return Ok(Output {
                values: vec![],
                errors: vec![],
            });
        }
        let nodes = DeviceBuffer::from_slice(if q.nodes.is_empty() { &[0] } else { &q.nodes })?;
        let text = DeviceBuffer::from_slice(if q.text.is_empty() { &[0] } else { &q.text })?;
        let begin = Event::new(EventFlags::DEFAULT)?;
        let end = Event::new(EventFlags::DEFAULT)?;
        let f = self.module.get_function("linkding_batch")?;
        let stream = &self.stream;
        begin.record(stream)?;
        unsafe {
            cust::launch!(f<<<(r.n as u32).div_ceil(128),128,0,stream>>>(r.fields.as_device_ptr(),r.fields.len(),r.lengths.as_device_ptr(),r.lengths.len(),r.tags.as_device_ptr(),r.tags.len(),r.meta.as_device_ptr(),r.meta.len(),nodes.as_device_ptr(),q.nodes.len(),text.as_device_ptr(),q.text.len(),q.root,q.flags,r.values.as_device_ptr(),r.errors.as_device_ptr(),r.n))?;
        }
        end.record(stream)?;
        end.synchronize()?;
        self.kernel_ms = end.elapsed_time_f32(&begin)?;
        let mut out = Output {
            values: vec![0; r.n],
            errors: vec![0; r.n],
        };
        r.values.copy_to(&mut out.values)?;
        r.errors.copy_to(&mut out.errors)?;
        self.total_ms = start.elapsed().as_secs_f64() * 1000.;
        Ok(out)
    }
}
