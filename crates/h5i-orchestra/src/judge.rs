//! Pluggable verdicts — the `VerdictPolicy` trait and its built-ins.
//!
//! `h5i team finalize` hardcodes one rule. The eDSL generalizes it: a policy
//! sees the folded run (submissions, verifications, reviews are all in the
//! projected `TeamRun`) and returns a `TeamVerdict` with reasons. Whatever the
//! policy decides is recorded through `team::record_verdict`, so a programmatic
//! verdict is exactly as auditable as the CLI's.

use h5i_core::error::H5iError;
use h5i_core::team::{self, TeamRun, TeamVerdict};

pub trait VerdictPolicy: Send + Sync {
    /// Short policy name, recorded in the verdict's `method` audit field by
    /// convention (built-ins set `method` themselves).
    fn name(&self) -> &str;
    fn decide(&self, run: &TeamRun) -> Result<TeamVerdict, H5iError>;
}

impl VerdictPolicy for Box<dyn VerdictPolicy> {
    fn name(&self) -> &str {
        (**self).name()
    }
    fn decide(&self, run: &TeamRun) -> Result<TeamVerdict, H5iError> {
        (**self).decide(run)
    }
}

/// Built-in policies.
pub mod policy {
    use super::*;

    /// Today's `h5i team finalize` rule, verbatim (shared implementation):
    /// keep candidates whose latest verification applies cleanly and passes
    /// tests, refuse divergent verifier commands, pick the smallest diff.
    pub fn tests_then_smallest_diff() -> impl VerdictPolicy {
        FnPolicy {
            name: "tests_then_smallest_diff",
            f: Box::new(|run| Ok(team::default_verdict(run))),
        }
    }

    /// Wrap a closure as a policy — the escape hatch for user rules
    /// (including LLM judges, which are just policies that `ask` inside).
    pub fn from_fn<F>(name: &'static str, f: F) -> impl VerdictPolicy
    where
        F: Fn(&TeamRun) -> Result<TeamVerdict, H5iError> + Send + Sync + 'static,
    {
        FnPolicy {
            name,
            f: Box::new(f),
        }
    }

    struct FnPolicy {
        name: &'static str,
        #[allow(clippy::type_complexity)]
        f: Box<dyn Fn(&TeamRun) -> Result<TeamVerdict, H5iError> + Send + Sync>,
    }

    impl VerdictPolicy for FnPolicy {
        fn name(&self) -> &str {
            self.name
        }
        fn decide(&self, run: &TeamRun) -> Result<TeamVerdict, H5iError> {
            (self.f)(run)
        }
    }
}
