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
/// Execution failures are distinct from successful negative decisions.
#[derive(Debug, PartialEq, Eq)]
pub enum OutputError {
    Length {
        expected: usize,
        values: usize,
        errors: usize,
    },
    Row {
        index: usize,
        code: u32,
    },
}
impl core::fmt::Display for OutputError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Length {
                expected,
                values,
                errors,
            } => write!(
                f,
                "expected {expected} output rows, got {values} values and {errors} errors"
            ),
            Self::Row { index, code } => write!(f, "kernel row {index} failed with code {code}"),
        }
    }
}
impl std::error::Error for OutputError {}
impl Output {
    /// Check the complete result before using any decisions. Values retain
    /// their kernel-specific meaning; every nonzero error invalidates the batch.
    pub fn validate(&self, expected: usize) -> Result<(), OutputError> {
        if self.values.len() != expected || self.errors.len() != expected {
            return Err(OutputError::Length {
                expected,
                values: self.values.len(),
                errors: self.errors.len(),
            });
        }
        if let Some(index) = self.errors.iter().position(|&code| code != 0) {
            return Err(OutputError::Row {
                index,
                code: self.errors[index],
            });
        }
        Ok(())
    }
}
pub use h5i_app_gpu::batch::Backend;
pub struct Cpu;
impl Backend<Batch, Decision, Output> for Cpu {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negative_decisions_are_successful_but_row_errors_reject_the_batch() {
        let mut out = Output {
            values: vec![0, 1],
            errors: vec![0, 0],
        };
        assert_eq!(out.validate(2), Ok(()));
        out.errors[1] = 7;
        assert_eq!(out.validate(2), Err(OutputError::Row { index: 1, code: 7 }));
    }

    #[test]
    fn output_columns_must_cover_exactly_the_requested_rows() {
        let mut out = Output {
            values: vec![1],
            errors: vec![],
        };
        assert!(matches!(out.validate(1), Err(OutputError::Length { .. })));
        out.errors.push(0);
        assert!(matches!(out.validate(2), Err(OutputError::Length { .. })));
        assert_eq!(
            Output {
                values: vec![],
                errors: vec![]
            }
            .validate(0),
            Ok(())
        );
    }
}
