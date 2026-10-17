//! Backend-independent bounded SoA inputs and CPU oracle. Padding is semantic:
//! both CPU and GPU evaluate the same fixed-width byte slices.
use crate::arena::{MAX_ROWS, ROW_BYTES, RULE_BYTES};
#[derive(Clone)]
pub struct Batch {
    pub text: Vec<u8>,
    pub patterns: Vec<u8>,
    pub displays: Vec<u8>,
    pub tags: Vec<u64>,
    pub members: Vec<u64>,
    pub powers: Vec<u64>,
    pub terms: Vec<u8>,
    pub count: usize,
    pub any: bool,
    pub negated: u32,
    pub required: u64,
    pub minimum: u64,
    pub maximum: u64,
    pub enabled: bool,
    pub require_display: bool,
    pub n: usize,
}
#[derive(Clone, Copy, Debug)]
pub enum Decision {
    Search,
    Push,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Output {
    pub values: Vec<u32>,
    pub errors: Vec<u32>,
}
pub trait Backend {
    type Error;
    fn run(&mut self, decision: Decision, batch: &Batch) -> Result<Output, Self::Error>;
}
pub struct Cpu;
impl Backend for Cpu {
    type Error = core::convert::Infallible;
    fn run(&mut self, decision: Decision, b: &Batch) -> Result<Output, Self::Error> {
        let mut out = Output {
            values: vec![0; b.n],
            errors: vec![0; b.n],
        };
        for i in 0..b.n {
            let valid = i < MAX_ROWS
                && i < b.text.len() / ROW_BYTES
                && match decision {
                    Decision::Search => i < b.tags.len(),
                    Decision::Push => {
                        i < b.patterns.len() / RULE_BYTES
                            && i < b.displays.len() / RULE_BYTES
                            && i < b.members.len()
                            && i < b.powers.len()
                    }
                };
            if !valid {
                out.errors[i] = 1;
                continue;
            }
            let row = &b.text[i * ROW_BYTES..i * ROW_BYTES + ROW_BYTES];
            out.values[i] = match decision {
                Decision::Search => parallel_kernel::bookmark_search(
                    row, &b.terms, b.count, b.any, b.negated, b.tags[i], b.required,
                ) as u32,
                Decision::Push => parallel_kernel::push_rule(
                    row,
                    &b.patterns[i * RULE_BYTES..i * RULE_BYTES + RULE_BYTES],
                    &b.displays[i * RULE_BYTES..i * RULE_BYTES + RULE_BYTES],
                    b.members[i],
                    b.minimum,
                    b.maximum,
                    b.powers[i],
                    b.required,
                    b.enabled,
                    b.require_display,
                ),
            };
        }
        Ok(out)
    }
}
