//! Counterexample trace replay directly against [`MemoryEngine`].
//!
//! Model checking and temporal verification belong to the optional Veil backend
//! (`crates/h5i-app-veil`); this module only provides the h5i-specific bridge
//! for replaying a counterexample sequence of `(Principal, Command)` steps
//! against the real [`MemoryEngine`] implementation.

use crate::{Kernel, MemoryEngine, TenantId, Timestamp};

/// One step in a kernel trace to replay against [`MemoryEngine`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayStep<P, C, S> {
    pub actor: P,
    pub command: C,
    /// Optional expected snapshot after this step (from Veil's counterexample trace).
    pub expected_snapshot: Option<S>,
}

/// Result of replaying a counterexample trace against [`MemoryEngine`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KernelReplayReport<S> {
    pub initial: S,
    pub final_state: S,
    pub steps_replayed: usize,
    pub accepted_steps: usize,
    pub refused_steps: usize,
    pub all_matched: bool,
    pub divergence_step: Option<usize>,
}

/// Replay `steps` from `initial` directly on [`MemoryEngine::execute`], checking
/// every expected snapshot if provided.
pub fn replay_kernel<K>(
    tenant: TenantId,
    initial: K::Snapshot,
    steps: &[ReplayStep<K::Principal, K::Command, K::Snapshot>],
) -> KernelReplayReport<K::Snapshot>
where
    K: Kernel,
    K::Snapshot: PartialEq,
{
    let engine = MemoryEngine::<K>::default().with_snapshot(tenant, initial.clone());
    let mut accepted = 0usize;
    let mut refused = 0usize;
    let mut divergence_step = None;

    for (idx, step) in steps.iter().enumerate() {
        match engine.execute(&step.actor, &step.command) {
            Ok(_) => accepted += 1,
            Err(_) => refused += 1,
        }
        if let Some(expected) = &step.expected_snapshot {
            let actual = engine.snapshot(tenant);
            if &actual != expected && divergence_step.is_none() {
                divergence_step = Some(idx + 1);
            }
        }
    }

    KernelReplayReport {
        initial,
        final_state: engine.snapshot(tenant),
        steps_replayed: steps.len(),
        accepted_steps: accepted,
        refused_steps: refused,
        all_matched: divergence_step.is_none(),
        divergence_step,
    }
}

/// Replay a timed sequence `(actor, timestamp, command)` directly on
/// [`MemoryEngine::execute_at`], verifying monotonic timestamp behavior.
pub fn replay_kernel_timed<K>(
    tenant: TenantId,
    initial: K::Snapshot,
    steps: &[(K::Principal, Timestamp, K::Command)],
) -> (K::Snapshot, Timestamp, bool)
where
    K: Kernel,
{
    let engine = MemoryEngine::<K>::default().with_snapshot(tenant, initial);
    let mut prev_time = Timestamp::EPOCH;
    let mut monotonic = true;
    for (actor, ts, cmd) in steps {
        let _ = engine.execute_at(actor, *ts, cmd);
        let cur_time = engine.last_time(tenant);
        if cur_time < prev_time {
            monotonic = false;
        }
        prev_time = cur_time;
    }
    (engine.snapshot(tenant), engine.last_time(tenant), monotonic)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Counter;

    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    struct Snap {
        count: u8,
        limit: u8,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Cmd {
        Incr,
        Reset,
    }

    impl Kernel for Counter {
        type Principal = u8;
        type Snapshot = Snap;
        type Command = Cmd;
        type WriteSet = Snap;
        type Reply = u8;
        type Error = ();

        fn tenant(_: &u8) -> TenantId {
            TenantId(1)
        }

        fn transition(_: &u8, snap: &Snap, cmd: &Cmd) -> Result<(Snap, u8), ()> {
            match cmd {
                Cmd::Incr => {
                    if snap.count >= snap.limit {
                        Err(())
                    } else {
                        let next = Snap { count: snap.count + 1, limit: snap.limit };
                        Ok((next.clone(), next.count))
                    }
                }
                Cmd::Reset => Ok((Snap { count: 0, limit: snap.limit }, 0)),
            }
        }

        fn apply(_: &Snap, ws: &Snap) -> Snap {
            ws.clone()
        }
    }

    #[test]
    fn replays_trace_against_real_memory_engine() {
        let init = Snap { count: 0, limit: 2 };
        let steps = vec![
            ReplayStep { actor: 1, command: Cmd::Incr, expected_snapshot: Some(Snap { count: 1, limit: 2 }) },
            ReplayStep { actor: 1, command: Cmd::Incr, expected_snapshot: Some(Snap { count: 2, limit: 2 }) },
            ReplayStep { actor: 1, command: Cmd::Incr, expected_snapshot: Some(Snap { count: 2, limit: 2 }) },
            ReplayStep { actor: 1, command: Cmd::Reset, expected_snapshot: Some(Snap { count: 0, limit: 2 }) },
        ];
        let rep = replay_kernel::<Counter>(TenantId(1), init, &steps);
        assert!(rep.all_matched);
        assert_eq!(rep.accepted_steps, 3);
        assert_eq!(rep.refused_steps, 1);
        assert_eq!(rep.final_state.count, 0);
    }
}
