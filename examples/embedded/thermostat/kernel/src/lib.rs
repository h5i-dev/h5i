//! A thermostat with four relays, as an h5i-app kernel for a microcontroller.
//!
//! `no_std`, no allocation, in the Rust subset Aeneas translates to Lean.
//! `proofs/` proves, about this code: `transition` never panics for any
//! command (a relay index of 4 or more is refused, never used to index); the
//! interlock `heater → temp_dc < MAX_TEMP_DC` and `setpoint_dc ∈ [MIN, MAX]`
//! hold in every reachable state; `Read` writes nothing.
//!
//! Temperatures are in tenths of a degree Celsius.

#![no_std]

pub const MIN_SETPOINT_DC: i16 = 50;
pub const MAX_SETPOINT_DC: i16 = 300;
/// The heater is never on at or above this temperature.
pub const MAX_TEMP_DC: i16 = 350;
pub const RELAYS: usize = 4;

/// The device itself: one tenant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Device {
    /// May change the configuration (setpoint, mode, relays).
    pub admin: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Off,
    /// Heat whenever below the interlock.
    Heat,
    /// Heat while below the setpoint.
    Auto,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub setpoint_dc: i16,
    pub temp_dc: i16,
    pub heater: bool,
    pub relays: [bool; RELAYS],
    pub mode: Mode,
}

pub const INITIAL: Snapshot =
    Snapshot { setpoint_dc: 200, temp_dc: 200, heater: false, relays: [false; RELAYS], mode: Mode::Off };

impl Default for Snapshot {
    fn default() -> Self {
        INITIAL
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    SetSetpoint { dc: i16 },
    /// A sensor reading.
    ReportTemp { dc: i16 },
    SetRelay { idx: u8, on: bool },
    SetMode { mode: Mode },
    Read,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reply {
    State(Snapshot),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    SetpointOutOfRange,
    NoSuchRelay,
    Forbidden,
}

/// Whether the heater runs, from the mode and the temperatures.
pub fn heater_on(mode: Mode, temp_dc: i16, setpoint_dc: i16) -> bool {
    if temp_dc >= MAX_TEMP_DC {
        return false;
    }
    match mode {
        Mode::Off => false,
        Mode::Heat => true,
        Mode::Auto => temp_dc < setpoint_dc,
    }
}

/// `s` with the heater recomputed.
fn settle(s: Snapshot) -> Snapshot {
    Snapshot {
        setpoint_dc: s.setpoint_dc,
        temp_dc: s.temp_dc,
        heater: heater_on(s.mode, s.temp_dc, s.setpoint_dc),
        relays: s.relays,
        mode: s.mode,
    }
}

/// The new state (`Some`) or nothing to write, and the reply.
pub fn transition(actor: &Device, s: &Snapshot, cmd: &Command) -> Result<(Option<Snapshot>, Reply), Error> {
    match cmd {
        Command::Read => Ok((None, Reply::State(*s))),
        Command::ReportTemp { dc } => {
            let mut n = *s;
            n.temp_dc = *dc;
            let n = settle(n);
            Ok((Some(n), Reply::State(n)))
        }
        Command::SetSetpoint { dc } => {
            if !actor.admin {
                return Err(Error::Forbidden);
            }
            if *dc < MIN_SETPOINT_DC || *dc > MAX_SETPOINT_DC {
                return Err(Error::SetpointOutOfRange);
            }
            let mut n = *s;
            n.setpoint_dc = *dc;
            let n = settle(n);
            Ok((Some(n), Reply::State(n)))
        }
        Command::SetRelay { idx, on } => {
            if !actor.admin {
                return Err(Error::Forbidden);
            }
            let i = *idx as usize;
            if i >= RELAYS {
                return Err(Error::NoSuchRelay);
            }
            let mut n = *s;
            n.relays[i] = *on;
            Ok((Some(n), Reply::State(n)))
        }
        Command::SetMode { mode } => {
            if !actor.admin {
                return Err(Error::Forbidden);
            }
            let mut n = *s;
            n.mode = *mode;
            let n = settle(n);
            Ok((Some(n), Reply::State(n)))
        }
    }
}

pub fn apply(s: &Snapshot, w: &Option<Snapshot>) -> Snapshot {
    match w {
        Some(n) => *n,
        None => *s,
    }
}

/// `Read` writes nothing (`ReadOnly.lean`).
pub fn read_only(cmd: &Command) -> bool {
    match cmd {
        Command::Read => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADMIN: Device = Device { admin: true };

    fn run(s: Snapshot, c: Command) -> Result<Snapshot, Error> {
        transition(&ADMIN, &s, &c).map(|(w, _)| apply(&s, &w))
    }

    #[test]
    fn interlock() {
        let s = run(INITIAL, Command::SetMode { mode: Mode::Heat }).unwrap();
        assert!(s.heater);
        let s = run(s, Command::ReportTemp { dc: 350 }).unwrap();
        assert!(!s.heater);
        let s = run(s, Command::SetMode { mode: Mode::Auto }).unwrap();
        let s = run(s, Command::ReportTemp { dc: 150 }).unwrap();
        assert!(s.heater);
        let s = run(s, Command::ReportTemp { dc: 250 }).unwrap();
        assert!(!s.heater);
    }

    #[test]
    fn refusals() {
        assert_eq!(run(INITIAL, Command::SetSetpoint { dc: 301 }), Err(Error::SetpointOutOfRange));
        assert_eq!(run(INITIAL, Command::SetSetpoint { dc: 49 }), Err(Error::SetpointOutOfRange));
        assert_eq!(run(INITIAL, Command::SetRelay { idx: 4, on: true }), Err(Error::NoSuchRelay));
        assert_eq!(run(INITIAL, Command::SetRelay { idx: 255, on: true }), Err(Error::NoSuchRelay));
        assert!(run(INITIAL, Command::SetRelay { idx: 3, on: true }).unwrap().relays[3]);
        let guest = Device { admin: false };
        assert_eq!(transition(&guest, &INITIAL, &Command::SetMode { mode: Mode::Heat }), Err(Error::Forbidden));
        assert!(transition(&guest, &INITIAL, &Command::ReportTemp { dc: 1 }).is_ok());
    }
}
