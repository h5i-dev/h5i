//! Counterexample trace replay directly against the real [`FlashStore`] and
//! [`DeviceEngine`] implementations (100% `no_std`, zero async runtime needed).
//!
//! Model checking and temporal verification are performed by the optional Veil
//! backend (`crates/h5i-app-veil`). This module replays action sequences from
//! Veil counterexample traces directly against the unmodified Rust runtime code
//! to verify whether an abstract model counterexample reproduces on the real
//! implementation.

use core::future::Future;
use core::pin::pin;
use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

use embedded_storage_async::nor_flash::{
    ErrorType, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
};
use h5i_app_core::{Kernel, TenantId};

use crate::kernel::{DeviceEngine, Refused, Store};
use crate::store::{FlashStore, SnapshotCodec};

/* -------------------------------------------------------------------------- */
/* Minimal no_std poll helper (for synchronous in-memory futures)             */
/* -------------------------------------------------------------------------- */

fn noop_waker() -> Waker {
    const VTABLE: RawWakerVTable = RawWakerVTable::new(
        |p| RawWaker::new(p, &VTABLE),
        |_| {},
        |_| {},
        |_| {},
    );
    // SAFETY: vtable functions are pure no-ops and never dereference the pointer.
    unsafe { Waker::from_raw(RawWaker::new(core::ptr::null(), &VTABLE)) }
}

/// Poll a future expected to complete immediately (e.g. in-memory NOR flash or
/// unlocked `DeviceEngine` operations). Returns `None` if it yields `Pending`.
fn poll_now<F: Future>(fut: F) -> Option<F::Output> {
    let waker = noop_waker();
    let mut cx = Context::from_waker(&waker);
    let fut = pin!(fut);
    match fut.poll(&mut cx) {
        Poll::Ready(v) => Some(v),
        Poll::Pending => None,
    }
}

/* -------------------------------------------------------------------------- */
/* Real FlashStore Counterexample Replay                                      */
/* -------------------------------------------------------------------------- */

/// Kernel whose snapshot is a `u64` value, for replaying `FlashStore` traces.
pub struct ValueKernel;

impl Kernel for ValueKernel {
    type Principal = ();
    type Snapshot = u64;
    type Command = ();
    type WriteSet = u64;
    type Reply = ();
    type Error = ();

    fn tenant(_: &()) -> TenantId {
        TenantId(0)
    }

    fn transition(_: &(), _: &u64, _: &()) -> Result<(u64, ()), ()> {
        Ok((0, ()))
    }

    fn apply(_: &u64, ws: &u64) -> u64 {
        *ws
    }
}

/// 8-byte little-endian snapshot codec for [`ValueKernel`].
pub struct U64Codec;

impl SnapshotCodec<ValueKernel> for U64Codec {
    const VERSION: u16 = 1;

    fn encode(s: &u64, out: &mut [u8]) -> Option<usize> {
        let dst = out.get_mut(..8)?;
        dst.copy_from_slice(&s.to_le_bytes());
        Some(8)
    }

    fn decode(b: &[u8]) -> Option<u64> {
        if b.len() != 8 {
            return None;
        }
        Some(u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }
}

/// Power-cut error injected into [`ReplayFlash`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PowerCut;

impl NorFlashError for PowerCut {
    fn kind(&self) -> NorFlashErrorKind {
        NorFlashErrorKind::Other
    }
}

/// Fixed-size `no_std` NOR flash backing the real [`FlashStore`] during replay.
///
/// Two 64-byte regions (`REGION = 64`, `ERASE_SIZE = 64`, `WRITE_SIZE = 4`):
/// each 8-byte snapshot takes a 24-byte record (`16` header + `8` data), so
/// each region holds 2 records before switching and erasing the standby region.
pub struct ReplayFlash {
    pub mem: [u8; 128],
    pub budget: usize,
    pub erases: usize,
}

impl Default for ReplayFlash {
    fn default() -> Self {
        Self::new()
    }
}

impl ReplayFlash {
    pub const REGION_SIZE: u32 = 64;

    pub const fn new() -> Self {
        ReplayFlash { mem: [0xFF; 128], budget: usize::MAX, erases: 0 }
    }

    /// Seed a valid CRC-32 record directly in region `region` (0 or 1) at
    /// `offset` with an arbitrary `seq` (for example `u32::MAX` to reach the
    /// wraparound boundary without running 2^32 saves in replay).
    pub fn seed_valid_record(&mut self, region: u32, offset: usize, seq: u32, val: u64) {
        const MAGIC: u32 = 0x5335_4948;
        let base = (region * Self::REGION_SIZE) as usize + offset;
        let mut rec = [0xFFu8; 24];
        rec[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        rec[4..8].copy_from_slice(&seq.to_le_bytes());
        rec[8..10].copy_from_slice(&U64Codec::VERSION.to_le_bytes());
        rec[10..12].copy_from_slice(&8u16.to_le_bytes());
        rec[16..24].copy_from_slice(&val.to_le_bytes());
        let mut c = 0xFFFF_FFFFu32;
        for &b in rec[4..12].iter().chain(rec[16..24].iter()) {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
            }
        }
        rec[12..16].copy_from_slice(&(!c).to_le_bytes());
        self.mem[base..base + 24].copy_from_slice(&rec);
    }

    fn spend(&mut self) -> Result<(), PowerCut> {
        if self.budget == 0 {
            return Err(PowerCut);
        }
        self.budget -= 1;
        Ok(())
    }
}

impl ErrorType for ReplayFlash {
    type Error = PowerCut;
}

impl ReadNorFlash for ReplayFlash {
    const READ_SIZE: usize = 1;

    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), PowerCut> {
        if self.budget == 0 {
            return Err(PowerCut);
        }
        let s = offset as usize;
        bytes.copy_from_slice(&self.mem[s..s + bytes.len()]);
        Ok(())
    }

    fn capacity(&self) -> usize {
        self.mem.len()
    }
}

impl NorFlash for ReplayFlash {
    const WRITE_SIZE: usize = 4;
    const ERASE_SIZE: usize = 64;

    async fn erase(&mut self, from: u32, to: u32) -> Result<(), PowerCut> {
        self.erases += 1;
        for i in from..to {
            self.spend()?;
            self.mem[i as usize] = 0xFF;
        }
        Ok(())
    }

    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), PowerCut> {
        for (i, &b) in bytes.iter().enumerate() {
            self.spend()?;
            self.mem[offset as usize + i] &= b;
        }
        Ok(())
    }
}

/// One replayable action targeting the real [`FlashStore`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlashReplayAction {
    /// Save `val` cleanly (unlimited power budget).
    Save(u64),
    /// Save `val` with a power cut after `budget` byte operations (e.g. `12` cuts mid-write).
    SaveWithCut { val: u64, budget: usize },
    /// Reboot by dropping the current [`FlashStore`], constructing a fresh one
    /// on the same NOR flash bytes, and calling [`Store::load`].
    RebootLoad,
}

/// Outcome of replaying a trace against the real [`FlashStore`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlashReplayOutcome {
    pub steps_replayed: usize,
    pub last_acked: Option<u64>,
    pub last_loaded: Option<u64>,
    pub durability_violated: bool,
    pub crash_consistency_violated: bool,
    pub erases: usize,
}

/// Replay `actions` against the **actual** [`FlashStore`].
///
/// If `seed_pre_wraparound` is `Some((seq, val))`, region 0 is seeded with a
/// valid record `(seq, val)` and loaded via `FlashStore::load()` before
/// replaying `actions` (so `seq = u32::MAX` lets replay reach the 32-bit
/// wraparound boundary immediately).
pub fn replay_real_flash_store(
    seed: Option<(u32, u64)>,
    actions: &[FlashReplayAction],
) -> FlashReplayOutcome {
    let mut flash = ReplayFlash::new();
    let mut last_acked: Option<u64> = None;
    let mut in_flight: Option<u64> = None;
    if let Some((seq, val)) = seed {
        flash.seed_valid_record(0, 0, seq, val);
        last_acked = Some(val);
    }

    let mut store: FlashStore<ReplayFlash, ValueKernel, U64Codec, 64> =
        FlashStore::new(flash, 0, ReplayFlash::REGION_SIZE);
    let mut last_loaded = poll_now(store.load()).expect("sync load").expect("initial load ok");
    let mut durability_violated = false;
    let mut crash_consistency_violated = false;

    for &act in actions {
        match act {
            FlashReplayAction::Save(v) => {
                in_flight = Some(v);
                if poll_now(store.save(&v)).expect("sync save").is_ok() {
                    last_acked = Some(v);
                    in_flight = None;
                }
            }
            FlashReplayAction::SaveWithCut { val, budget } => {
                // Transfer flash bytes with limited byte budget, preserving current state via reboot-less cut.
                let mut f = store.into_inner();
                f.budget = usize::MAX;
                let mut s = FlashStore::<ReplayFlash, ValueKernel, U64Codec, 64>::new(
                    f,
                    0,
                    ReplayFlash::REGION_SIZE,
                );
                let _ = poll_now(s.load());
                let mut f2 = s.into_inner();
                f2.budget = budget;
                store = FlashStore::new(f2, 0, ReplayFlash::REGION_SIZE);
                in_flight = Some(val);
                if poll_now(store.save(&val)).expect("sync save").is_ok() {
                    last_acked = Some(val);
                    in_flight = None;
                }
            }
            FlashReplayAction::RebootLoad => {
                let mut f = store.into_inner();
                f.budget = usize::MAX;
                store = FlashStore::new(f, 0, ReplayFlash::REGION_SIZE);
                let loaded = poll_now(store.load()).expect("sync load").expect("reboot load ok");
                last_loaded = loaded;
                if in_flight.is_none() {
                    // Clean reboot after an acknowledged save: durability requires `loaded == last_acked`.
                    if loaded != last_acked {
                        durability_violated = true;
                    }
                } else {
                    // Crash during an in-flight save: crash consistency requires `loaded ∈ {last_acked, in_flight}`.
                    if loaded != last_acked && loaded != in_flight {
                        crash_consistency_violated = true;
                    }
                }
                last_acked = loaded;
                in_flight = None;
            }
        }
    }

    let erases = store.into_inner().erases;
    FlashReplayOutcome {
        steps_replayed: actions.len(),
        last_acked,
        last_loaded,
        durability_violated,
        crash_consistency_violated,
        erases,
    }
}

/* -------------------------------------------------------------------------- */
/* Real DeviceEngine Counterexample Replay                                    */
/* -------------------------------------------------------------------------- */

/// Motor controller snapshot for replaying `DeviceEngine` traces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MotorSnap {
    pub emergency: bool,
    pub motor_on: bool,
    /// Set by the actuator once it observes `motor_on == false` (used by latched mode).
    pub stop_observed: bool,
}

impl Default for MotorSnap {
    fn default() -> Self {
        MotorSnap { emergency: false, motor_on: true, stop_observed: false }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MotorCmd {
    RequestEmergency,
    ClearEmergency,
    AckStop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MotorErr {
    StopNotObserved,
}

/// Motor kernel parameterized by `LATCHED` (`false` = unlatched, `true` = latched).
pub struct MotorKernel<const LATCHED: bool>;

impl<const LATCHED: bool> Kernel for MotorKernel<LATCHED> {
    type Principal = ();
    type Snapshot = MotorSnap;
    type Command = MotorCmd;
    type WriteSet = MotorSnap;
    type Reply = ();
    type Error = MotorErr;

    fn tenant(_: &()) -> TenantId {
        TenantId(0)
    }

    fn transition(_: &(), snap: &MotorSnap, cmd: &MotorCmd) -> Result<(MotorSnap, ()), MotorErr> {
        match cmd {
            MotorCmd::RequestEmergency => Ok((
                MotorSnap { emergency: true, motor_on: false, stop_observed: false },
                (),
            )),
            MotorCmd::ClearEmergency => {
                if LATCHED && snap.emergency && !snap.stop_observed {
                    return Err(MotorErr::StopNotObserved);
                }
                Ok((MotorSnap { emergency: false, motor_on: true, stop_observed: false }, ()))
            }
            MotorCmd::AckStop => Ok((
                MotorSnap { emergency: snap.emergency, motor_on: snap.motor_on, stop_observed: true },
                (),
            )),
        }
    }

    fn apply(_: &MotorSnap, ws: &MotorSnap) -> MotorSnap {
        *ws
    }
}

/// Fault-injectable in-memory [`Store`] for replaying `DeviceEngine` traces,
/// including `Store::save`'s documented ambiguous-failure contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreFault {
    None,
    /// Return `Err(())` without modifying stored flash.
    FailClean,
    /// Persist the snapshot to flash AND return `Err(())` (allowed by `Store::save`!).
    FailAmbiguous,
}

pub struct FaultStore {
    pub stored: Option<MotorSnap>,
    pub next_fault: StoreFault,
}

impl FaultStore {
    pub const fn new(initial: Option<MotorSnap>) -> Self {
        FaultStore { stored: initial, next_fault: StoreFault::None }
    }
}

impl<const LATCHED: bool> Store<MotorKernel<LATCHED>> for FaultStore {
    type Error = ();

    async fn load(&mut self) -> Result<Option<MotorSnap>, ()> {
        Ok(self.stored)
    }

    async fn save(&mut self, snap: &MotorSnap) -> Result<(), ()> {
        let fault = core::mem::replace(&mut self.next_fault, StoreFault::None);
        match fault {
            StoreFault::None => {
                self.stored = Some(*snap);
                Ok(())
            }
            StoreFault::FailClean => Err(()),
            StoreFault::FailAmbiguous => {
                self.stored = Some(*snap);
                Err(())
            }
        }
    }
}

/// One replayable action targeting the real [`DeviceEngine`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceEngineReplayAction {
    Execute(MotorCmd),
    /// Execute a command whose `Store::save` fails cleanly or ambiguously.
    ExecuteWithStoreFault(MotorCmd, StoreFault),
    /// Poll the real [`DeviceEngine::changed`] once with the actuator's `seen` cursor.
    ActuatorPoll,
    /// Reboot/restore by calling [`DeviceEngine::restore`]. Note: `restore()` updates
    /// the in-memory snapshot and bumps `version`, WITHOUT atomically updating the
    /// actuator GPIO output (a subsequent `ActuatorPoll` drives the actuator).
    Restore,
    /// Backwards-compatible alias for [`Self::Restore`].
    CrashAndRestore,
}

/// Projected observable runtime state used to check cycle closure during
/// lasso replay.
///
/// **Projection boundary**: compares only `(snapshot, notify_pending,
/// actuator_motor_on)`. It omits concrete `version` / `seen` counter values,
/// registered async wakers, and `Store` internals, so equality on this
/// projection does not by itself prove full concrete state equality or
/// infinite repeatability under version wraparound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceEngineProjectedState {
    pub snapshot: MotorSnap,
    pub notify_pending: bool,
    pub actuator_motor_on: bool,
}

/// Outcome of replaying a lasso (`prefix · cycle^ω`) against the real [`DeviceEngine`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceEngineLassoOutcome {
    pub prefix_steps: usize,
    pub cycle_steps: usize,
    pub emergency_committed: bool,
    /// Whether `cycle` closed an identical [`DeviceEngineProjectedState`] across
    /// two consecutive iterations (checked on the 3-field projection only, not
    /// full concrete `version`/`seen`/waker/`Store` state).
    pub projected_cycle_closed: bool,
    /// Whether `ActuatorPoll` actually ran during both cycle iterations (witnessing `𝒲ℱ actuatorPoll`).
    pub actuator_poll_in_cycle: bool,
    /// Whether the actuator observed `motor_on == false` during prefix or cycle.
    pub actuator_observed_stop: bool,
    /// `true` iff the cycle reproduced on the real `DeviceEngine` under the
    /// 3-field observable projection `(snapshot, notify_pending, actuator_motor_on)`
    /// (emergency committed, non-empty projected cycle closed across two runs,
    /// actuator polled in both runs, and stop never observed).
    pub projected_cycle_reproduced: bool,
    pub clear_refused_count: usize,
    pub state_after_cycle_1: DeviceEngineProjectedState,
    pub state_after_cycle_2: DeviceEngineProjectedState,
    pub final_version: u32,
}

/// Outcome of replaying a finite trace against the real [`DeviceEngine`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceEngineReplayOutcome {
    pub steps_replayed: usize,
    pub emergency_committed: bool,
    pub actuator_observed_stop: bool,
    pub clear_refused_count: usize,
    pub final_snapshot: MotorSnap,
    pub final_actuator_motor_on: bool,
    pub final_version: u32,
}

#[allow(clippy::too_many_arguments)]
fn step_device_engine<const LATCHED: bool>(
    engine: &mut DeviceEngine<MotorKernel<LATCHED>, FaultStore>,
    seen: &mut u32,
    actuator_motor_on: &mut bool,
    emergency_committed: &mut bool,
    actuator_observed_stop: &mut bool,
    clear_refused_count: &mut usize,
    actuator_polled: &mut bool,
    act: DeviceEngineReplayAction,
) {
    match act {
        DeviceEngineReplayAction::Execute(cmd) => {
            match poll_now(engine.execute(&(), &cmd, None)).expect("sync execute") {
                Ok(()) => {
                    if matches!(cmd, MotorCmd::RequestEmergency) {
                        *emergency_committed = true;
                    }
                }
                Err(Refused::Kernel(MotorErr::StopNotObserved)) => {
                    *clear_refused_count += 1;
                }
                Err(Refused::Store(())) => {}
            }
        }
        DeviceEngineReplayAction::ExecuteWithStoreFault(cmd, fault) => {
            let cur_snap = engine.snapshot();
            let mut fs = FaultStore::new(Some(cur_snap));
            fs.next_fault = fault;
            *engine = DeviceEngine::with_store(cur_snap, fs);
            *seen = engine.version();
            let res = poll_now(engine.execute(&(), &cmd, None)).expect("sync execute");
            if res.is_ok() && matches!(cmd, MotorCmd::RequestEmergency) {
                *emergency_committed = true;
            }
        }
        DeviceEngineReplayAction::ActuatorPoll => {
            *actuator_polled = true;
            if let Some(snap) = poll_now(engine.changed(seen)) {
                *actuator_motor_on = snap.motor_on;
                if !*actuator_motor_on {
                    *actuator_observed_stop = true;
                    if LATCHED {
                        let _ = poll_now(engine.execute(&(), &MotorCmd::AckStop, None));
                    }
                }
            }
        }
        DeviceEngineReplayAction::Restore | DeviceEngineReplayAction::CrashAndRestore => {
            // Mirror real `DeviceEngine::restore()`: loads persisted snapshot and
            // calls `notify()` (`version` increments), WITHOUT atomically mutating
            // `actuator_motor_on` (the actuator task polls `changed()` separately).
            *seen = engine.version();
            let _ = poll_now(engine.restore()).expect("sync restore");
        }
    }
}

/// Replay a finite sequence of `actions` directly against the **actual**
/// [`DeviceEngine<MotorKernel<LATCHED>, FaultStore>`].
pub fn replay_real_device_engine<const LATCHED: bool>(
    actions: &[DeviceEngineReplayAction],
) -> DeviceEngineReplayOutcome {
    let init = MotorSnap::default();
    let mut engine: DeviceEngine<MotorKernel<LATCHED>, FaultStore> =
        DeviceEngine::with_store(init, FaultStore::new(Some(init)));
    let mut seen = engine.version();
    let mut actuator_motor_on = engine.snapshot().motor_on;
    let mut emergency_committed = false;
    let mut actuator_observed_stop = !actuator_motor_on;
    let mut clear_refused_count = 0usize;
    let mut actuator_polled = false;

    for &act in actions {
        step_device_engine(
            &mut engine,
            &mut seen,
            &mut actuator_motor_on,
            &mut emergency_committed,
            &mut actuator_observed_stop,
            &mut clear_refused_count,
            &mut actuator_polled,
            act,
        );
    }

    DeviceEngineReplayOutcome {
        steps_replayed: actions.len(),
        emergency_committed,
        actuator_observed_stop,
        clear_refused_count,
        final_snapshot: engine.snapshot(),
        final_actuator_motor_on: actuator_motor_on,
        final_version: engine.version(),
    }
}

/// Replay a lasso trace `prefix · cycle^ω` directly against the **actual**
/// [`DeviceEngine<MotorKernel<LATCHED>, FaultStore>`], checking projected state
/// closure `(snapshot, notify_pending, actuator_motor_on)` across two
/// iterations of `cycle` and verifying that `ActuatorPoll` executed without
/// observing stop.
pub fn replay_real_device_engine_lasso<const LATCHED: bool>(
    prefix: &[DeviceEngineReplayAction],
    cycle: &[DeviceEngineReplayAction],
) -> DeviceEngineLassoOutcome {
    let init = MotorSnap::default();
    let mut engine: DeviceEngine<MotorKernel<LATCHED>, FaultStore> =
        DeviceEngine::with_store(init, FaultStore::new(Some(init)));
    let mut seen = engine.version();
    let mut actuator_motor_on = engine.snapshot().motor_on;
    let mut emergency_committed = false;
    let mut actuator_observed_stop = !actuator_motor_on;
    let mut clear_refused_count = 0usize;
    let mut unused_polled = false;

    for &act in prefix {
        step_device_engine(
            &mut engine,
            &mut seen,
            &mut actuator_motor_on,
            &mut emergency_committed,
            &mut actuator_observed_stop,
            &mut clear_refused_count,
            &mut unused_polled,
            act,
        );
    }

    let snapshot_state = |eng: &DeviceEngine<MotorKernel<LATCHED>, FaultStore>,
                          seen: u32,
                          motor_on: bool| {
        DeviceEngineProjectedState {
            snapshot: eng.snapshot(),
            notify_pending: eng.version() != seen,
            actuator_motor_on: motor_on,
        }
    };

    let mut actuator_poll_in_cycle = false;
    for &act in cycle {
        step_device_engine(
            &mut engine,
            &mut seen,
            &mut actuator_motor_on,
            &mut emergency_committed,
            &mut actuator_observed_stop,
            &mut clear_refused_count,
            &mut actuator_poll_in_cycle,
            act,
        );
    }
    let state_after_cycle_1 = snapshot_state(&engine, seen, actuator_motor_on);

    let mut actuator_poll_in_cycle_2 = false;
    for &act in cycle {
        step_device_engine(
            &mut engine,
            &mut seen,
            &mut actuator_motor_on,
            &mut emergency_committed,
            &mut actuator_observed_stop,
            &mut clear_refused_count,
            &mut actuator_poll_in_cycle_2,
            act,
        );
    }
    let state_after_cycle_2 = snapshot_state(&engine, seen, actuator_motor_on);

    let projected_cycle_closed = !cycle.is_empty() && state_after_cycle_1 == state_after_cycle_2;
    let poll_in_cycle = actuator_poll_in_cycle && actuator_poll_in_cycle_2;
    let projected_cycle_reproduced =
        emergency_committed && projected_cycle_closed && poll_in_cycle && !actuator_observed_stop;

    DeviceEngineLassoOutcome {
        prefix_steps: prefix.len(),
        cycle_steps: cycle.len(),
        emergency_committed,
        projected_cycle_closed,
        actuator_poll_in_cycle: poll_in_cycle,
        actuator_observed_stop,
        projected_cycle_reproduced,
        clear_refused_count,
        state_after_cycle_1,
        state_after_cycle_2,
        final_version: engine.version(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Replays the Veil-discovered sequence-wraparound trace (`u32::MAX -> 0`)
    /// directly against the **production** `h5i_app_embedded::store::FlashStore`.
    /// With modular sequence ordering (`store_core::seq_newer`), durability is
    /// now preserved across wraparound: `Save(2)` at `seq = 0` is recovered.
    #[test]
    fn real_flash_store_preserves_durability_across_u32_wraparound() {
        let outcome = replay_real_flash_store(
            Some((u32::MAX, 1)),
            &[FlashReplayAction::Save(2), FlashReplayAction::RebootLoad],
        );
        assert!(
            !outcome.durability_violated,
            "fixed production FlashStore MUST preserve durability across u32::MAX -> 0 wraparound: {outcome:?}"
        );
        assert_eq!(outcome.last_loaded, Some(2));
    }

    /// Replays power cuts mid-write directly against the real `FlashStore`
    /// (below wraparound), verifying crash consistency holds.
    #[test]
    fn real_flash_store_preserves_crash_consistency_below_wraparound() {
        let outcome = replay_real_flash_store(
            None,
            &[
                FlashReplayAction::Save(1),
                FlashReplayAction::Save(2),
                FlashReplayAction::SaveWithCut { val: 3, budget: 10 },
                FlashReplayAction::RebootLoad,
                FlashReplayAction::Save(4),
                FlashReplayAction::RebootLoad,
            ],
        );
        assert!(!outcome.crash_consistency_violated, "{outcome:?}");
        assert!(!outcome.durability_violated, "{outcome:?}");
        assert_eq!(outcome.last_loaded, Some(4));
    }

    /// Replays the Veil-discovered `changed()` coalescing counterexample directly
    /// against the **real, unmodified** `h5i_app_embedded::kernel::DeviceEngine`.
    #[test]
    fn real_device_engine_reproduces_unlatched_coalescing_missed_emergency() {
        let outcome = replay_real_device_engine::<false>(&[
            DeviceEngineReplayAction::Execute(MotorCmd::RequestEmergency),
            DeviceEngineReplayAction::Execute(MotorCmd::ClearEmergency),
            DeviceEngineReplayAction::ActuatorPoll,
        ]);
        assert!(outcome.emergency_committed);
        assert!(
            !outcome.actuator_observed_stop,
            "real DeviceEngine MUST coalesce Normal->Emergency->Normal and miss stop: {outcome:?}"
        );
        assert!(outcome.final_actuator_motor_on);
        assert_eq!(outcome.final_version, 2);
    }

    /// Replays the same sequence on the latched kernel with the real `DeviceEngine`:
    /// `ClearEmergency` is refused until the actuator polls `changed()` and observes stop.
    #[test]
    fn real_device_engine_latched_protocol_prevents_missed_emergency() {
        let outcome = replay_real_device_engine::<true>(&[
            DeviceEngineReplayAction::Execute(MotorCmd::RequestEmergency),
            DeviceEngineReplayAction::Execute(MotorCmd::ClearEmergency),
            DeviceEngineReplayAction::ActuatorPoll,
            DeviceEngineReplayAction::Execute(MotorCmd::ClearEmergency),
        ]);
        assert!(outcome.emergency_committed);
        assert_eq!(outcome.clear_refused_count, 1);
        assert!(outcome.actuator_observed_stop);
        assert!(!outcome.final_snapshot.emergency);
    }

    /// Replays ambiguous `Store::save` failure + `restore()` + `actuatorPoll` on
    /// the real `DeviceEngine`: `restore()` loads the persisted emergency snapshot
    /// and bumps `version` WITHOUT touching the actuator; a subsequent `ActuatorPoll`
    /// observes the stop.
    #[test]
    fn real_device_engine_replays_ambiguous_store_failure_and_restore() {
        let after_restore_only = replay_real_device_engine::<false>(&[
            DeviceEngineReplayAction::ExecuteWithStoreFault(
                MotorCmd::RequestEmergency,
                StoreFault::FailAmbiguous,
            ),
            DeviceEngineReplayAction::Restore,
        ]);
        assert!(after_restore_only.final_snapshot.emergency);
        assert!(!after_restore_only.final_snapshot.motor_on);
        assert!(
            !after_restore_only.actuator_observed_stop,
            "restore() alone must not atomically update the actuator before ActuatorPoll"
        );

        let after_poll = replay_real_device_engine::<false>(&[
            DeviceEngineReplayAction::ExecuteWithStoreFault(
                MotorCmd::RequestEmergency,
                StoreFault::FailAmbiguous,
            ),
            DeviceEngineReplayAction::Restore,
            DeviceEngineReplayAction::ActuatorPoll,
        ]);
        assert!(after_poll.final_snapshot.emergency);
        assert!(!after_poll.final_snapshot.motor_on);
        assert!(after_poll.actuator_observed_stop);
    }

    /// Verifies projected cycle reproduction (`prefix · cycle^ω`) on the real
    /// unlatched `DeviceEngine`: repeating `[RequestEmergency, ClearEmergency, ActuatorPoll]`
    /// closes an identical projected state `(snapshot, notify_pending, actuator_motor_on)`
    /// while `ActuatorPoll` runs in each iteration and never observes stop.
    #[test]
    fn real_device_engine_lasso_reproduces_unlatched_projected_cycle() {
        let out = replay_real_device_engine_lasso::<false>(
            &[],
            &[
                DeviceEngineReplayAction::Execute(MotorCmd::RequestEmergency),
                DeviceEngineReplayAction::Execute(MotorCmd::ClearEmergency),
                DeviceEngineReplayAction::ActuatorPoll,
            ],
        );
        assert!(out.emergency_committed);
        assert!(out.projected_cycle_closed, "cycle must close identical projected state: {out:?}");
        assert!(out.actuator_poll_in_cycle, "fairness witness ActuatorPoll must execute in cycle");
        assert!(!out.actuator_observed_stop);
        assert!(out.projected_cycle_reproduced);
    }

    /// A finite trace ending after `RequestEmergency` without a cycle does NOT
    /// reproduce a projected cycle.
    #[test]
    fn real_device_engine_finite_trace_without_cycle_is_not_lasso() {
        let out = replay_real_device_engine_lasso::<false>(
            &[DeviceEngineReplayAction::Execute(MotorCmd::RequestEmergency)],
            &[],
        );
        assert!(out.emergency_committed);
        assert!(!out.actuator_observed_stop);
        assert!(!out.projected_cycle_closed);
        assert!(!out.projected_cycle_reproduced);
    }

    /// On the latched kernel, the same cycle does NOT reproduce the projected cycle:
    /// `ClearEmergency` is refused and `ActuatorPoll` observes the stop.
    #[test]
    fn real_device_engine_latched_blocks_coalescing_lasso() {
        let out = replay_real_device_engine_lasso::<true>(
            &[],
            &[
                DeviceEngineReplayAction::Execute(MotorCmd::RequestEmergency),
                DeviceEngineReplayAction::Execute(MotorCmd::ClearEmergency),
                DeviceEngineReplayAction::ActuatorPoll,
            ],
        );
        assert!(out.emergency_committed);
        assert!(out.actuator_observed_stop);
        assert!(!out.projected_cycle_reproduced);
        assert!(out.clear_refused_count >= 1);
    }

    /// Verifies the real `FlashStore` append-per-region and unchanged-save no-op protocol:
    /// 2 records fit in region 0 (1 erase on first switch from empty region 1, 0 erases on
    /// append, 0 erases on unchanged save), and the 3rd distinct record triggers region switch.
    #[test]
    fn real_flash_store_append_and_unchanged_save_protocol() {
        let outcome = replay_real_flash_store(
            None,
            &[
                FlashReplayAction::Save(10), // erases region 0, writes slot 0
                FlashReplayAction::Save(10), // unchanged -> no-op
                FlashReplayAction::Save(20), // appends to region 0 slot 1 (no erase!)
                FlashReplayAction::RebootLoad,
                FlashReplayAction::Save(30), // region 0 full -> erases region 1, writes slot 0
                FlashReplayAction::RebootLoad,
            ],
        );
        assert!(!outcome.durability_violated);
        assert!(!outcome.crash_consistency_violated);
        assert_eq!(outcome.last_loaded, Some(30));
        assert_eq!(outcome.erases, 2, "expected 1 erase for region 0 + 1 erase for region 1");
    }
}
