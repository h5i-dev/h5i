//! The thermostat's HTTP API and device logic, shared by the host runner and
//! the firmware.
//!
//! ```text
//! GET  /state                 the snapshot as JSON (no key needed)
//! PUT  /setpoint   "215"      tenths of a degree, 50..=300
//! PUT  /mode       "auto"     off | heat | auto
//! PUT  /relay/{i}  "on"       on | off; i >= 4 is refused by the kernel
//! ```
//!
//! Writes are signed (`h5i_app_embedded::hmac`). The temperature comes from
//! the device's own sensor ([`sensor_command`]), not from HTTP: the
//! interlock the kernel proves is about the reported temperature, so no
//! client may report one. The host runner, which has no sensor, adds
//! `POST /temp` ([`sim_router`]) for tests.
//!
//! Each route binds its own decoder (A2, trusted); encoding replies is
//! trusted (A3); the kernel's theorems hold for every command.

#![no_std]

use h5i_app_core::{Kernel, TenantId};
use h5i_app_embedded::hmac::HmacAuth;
use h5i_app_embedded::json::Writer;
use h5i_app_embedded::kernel::{Anyone, Calls, Codec, Device as HasEngine, DeviceEngine, NoStore, Store};
use h5i_app_embedded::store::SnapshotCodec;
use h5i_app_embedded::{Request, Router, Routes, form};
use thermostat_kernel::{Command, Device, Error, MAX_TEMP_DC, Mode, RELAYS, Reply, Snapshot};

/// The kernel, as the shell sees it.
pub struct Thermostat;

impl Kernel for Thermostat {
    type Principal = Device;
    type Snapshot = Snapshot;
    type Command = Command;
    type WriteSet = Option<Snapshot>;
    type Reply = Reply;
    type Error = Error;

    fn tenant(_: &Device) -> TenantId {
        TenantId(0)
    }

    fn transition(actor: &Device, snap: &Snapshot, cmd: &Command) -> Result<(Option<Snapshot>, Reply), Error> {
        thermostat_kernel::transition(actor, snap, cmd)
    }

    fn apply(snap: &Snapshot, w: &Option<Snapshot>) -> Snapshot {
        thermostat_kernel::apply(snap, w)
    }

    fn read_only(cmd: &Command) -> bool {
        thermostat_kernel::read_only(cmd)
    }
}

/// Clients without a key: may read.
pub const GUEST: Device = Device { admin: false };
/// The device's own sensor task. Not an admin: it can report a temperature
/// and nothing else the kernel guards.
pub const SENSOR: Device = Device { admin: false };

/// The device's state, its store `St`, and the key that signs writes.
pub struct App<St = NoStore> {
    pub engine: DeviceEngine<Thermostat, St>,
    pub admin: HmacAuth<Device>,
    pub guest: Anyone<Device>,
}

impl App<NoStore> {
    /// Starts from `thermostat_kernel::INITIAL`, the state the proofs start
    /// from. `boot` must differ on every boot (see `hmac`).
    pub const fn new(key: &'static [u8], boot: u32) -> App {
        App::with_store(key, boot, NoStore)
    }
}

impl<St: Store<Thermostat>> App<St> {
    /// Call `engine.restore()` before serving to resume from `store`.
    pub const fn with_store(key: &'static [u8], boot: u32, store: St) -> App<St> {
        App {
            engine: DeviceEngine::with_store(thermostat_kernel::INITIAL, store),
            admin: HmacAuth::new(key, Device { admin: true }, boot),
            guest: Anyone(GUEST),
        }
    }
}

impl<St: Store<Thermostat>> HasEngine<Thermostat> for App<St> {
    type Store = St;
    fn engine(&self) -> &DeviceEngine<Thermostat, St> {
        &self.engine
    }
}

/// JSON replies.
pub struct Wire;

impl Codec<Thermostat> for Wire {
    fn encode_reply(reply: &Reply, out: &mut [u8]) -> Option<usize> {
        let Reply::State(s) = reply;
        let mut w = Writer::new(out);
        w.open_obj();
        w.key(b"setpoint_dc");
        w.int(s.setpoint_dc as i64);
        w.key(b"temp_dc");
        w.int(s.temp_dc as i64);
        w.key(b"heater");
        w.bool(s.heater);
        w.key(b"relays");
        w.open_arr();
        for r in s.relays {
            w.bool(r);
        }
        w.close_arr();
        w.key(b"mode");
        w.str(mode_name(s.mode));
        w.close_obj();
        w.finish()
    }

    fn encode_error(err: &Error, out: &mut [u8]) -> (u16, usize) {
        let (status, code): (u16, &[u8]) = match err {
            Error::SetpointOutOfRange => (422, b"setpoint_out_of_range"),
            Error::NoSuchRelay => (404, b"no_such_relay"),
            Error::Forbidden => (403, b"forbidden"),
        };
        let mut w = Writer::new(out);
        w.open_obj();
        w.key(b"error");
        w.str(code);
        w.close_obj();
        (status, w.finish().unwrap_or(0))
    }
}

fn mode_name(m: Mode) -> &'static [u8] {
    match m {
        Mode::Off => b"off",
        Mode::Heat => b"heat",
        Mode::Auto => b"auto",
    }
}

/// `-?[0-9]{1,5}` within `i16`.
pub fn parse_i16(b: &[u8]) -> Option<i16> {
    form::parse_i32(b).filter(|_| b.len() <= 6).and_then(|v| i16::try_from(v).ok())
}

fn body<'r>(req: &Request<'r>) -> &'r [u8] {
    req.body.trim_ascii()
}

fn decode_setpoint(req: &Request<'_>) -> Result<Command, u16> {
    parse_i16(body(req)).map(|dc| Command::SetSetpoint { dc }).ok_or(400)
}

fn decode_mode(req: &Request<'_>) -> Result<Command, u16> {
    let mode = match body(req) {
        b"off" => Mode::Off,
        b"heat" => Mode::Heat,
        b"auto" => Mode::Auto,
        _ => return Err(400),
    };
    Ok(Command::SetMode { mode })
}

fn decode_relay(req: &Request<'_>) -> Result<Command, u16> {
    // Any u8 reaches the kernel, which refuses an index of 4 or more.
    let idx = req.param_u32(0).and_then(|i| u8::try_from(i).ok()).ok_or(404u16)?;
    let on = match body(req) {
        b"on" => true,
        b"off" => false,
        _ => return Err(400),
    };
    Ok(Command::SetRelay { idx, on })
}

fn decode_temp(req: &Request<'_>) -> Result<Command, u16> {
    parse_i16(body(req)).map(|dc| Command::ReportTemp { dc }).ok_or(400)
}

fn guest<St>(a: &App<St>) -> &Anyone<Device> {
    &a.guest
}

fn admin<St>(a: &App<St>) -> &HmacAuth<Device> {
    &a.admin
}

/// The device's routes.
pub fn router<St: Store<Thermostat>>() -> Router<impl Routes<App<St>>> {
    let k = Calls::<Thermostat, Wire, App<St>>::new();
    Router::new()
        .get(b"/state", k.call(guest, |_| Ok(Command::Read)))
        .put(b"/setpoint", k.call(admin, decode_setpoint))
        .put(b"/mode", k.call(admin, decode_mode))
        .put(b"/relay/{i}", k.call(admin, decode_relay))
}

/// [`router`] and `POST /temp` (signed), standing in for the sensor on a
/// host. Not for a device that has one.
pub fn sim_router<St: Store<Thermostat>>() -> Router<impl Routes<App<St>>> {
    let k = Calls::<Thermostat, Wire, App<St>>::new();
    router::<St>().post(b"/temp", k.call(admin, decode_temp))
}

/// The temperature in tenths of a degree from a TMP108's temperature
/// register (12 bits, two's complement, 1/16 °C per step, MSB first), or
/// `None` for an unreadable sensor. The sensor task reports `None` as
/// [`MAX_TEMP_DC`]: the interlock then turns the heater off, so a dead sensor
/// fails safe.
pub fn sensor_command(reading: Option<[u8; 2]>) -> Command {
    let dc = match reading {
        Some([msb, lsb]) => {
            let raw = (i16::from_be_bytes([msb, lsb]) >> 4) as i32;
            // raw / 16 °C = raw * 10 / 16 tenths, rounded toward zero.
            (raw * 10 / 16) as i16
        }
        None => MAX_TEMP_DC,
    };
    Command::ReportTemp { dc }
}

/// What the outputs must show for a committed state: the heater, then the
/// relays. The actuator task sets its pins from this.
pub fn outputs(s: &Snapshot) -> [bool; 1 + RELAYS] {
    [s.heater, s.relays[0], s.relays[1], s.relays[2], s.relays[3]]
}

/// The snapshot in flash: 8 bytes.
pub struct Flash;

impl SnapshotCodec<Thermostat> for Flash {
    const VERSION: u16 = 1;

    fn encode(s: &Snapshot, out: &mut [u8]) -> Option<usize> {
        let o = out.get_mut(..8)?;
        o[0..2].copy_from_slice(&s.setpoint_dc.to_le_bytes());
        o[2..4].copy_from_slice(&s.temp_dc.to_le_bytes());
        o[4] = s.heater as u8;
        o[5] = s.relays.iter().enumerate().fold(0u8, |m, (i, &r)| m | ((r as u8) << i));
        o[6] = match s.mode {
            Mode::Off => 0,
            Mode::Heat => 1,
            Mode::Auto => 2,
        };
        o[7] = 0;
        Some(8)
    }

    fn decode(b: &[u8]) -> Option<Snapshot> {
        if b.len() != 8 || b[4] > 1 || b[5] > 15 || b[7] != 0 {
            return None;
        }
        let mode = match b[6] {
            0 => Mode::Off,
            1 => Mode::Heat,
            2 => Mode::Auto,
            _ => return None,
        };
        let mut relays = [false; RELAYS];
        for (i, r) in relays.iter_mut().enumerate() {
            *r = b[5] & (1 << i) != 0;
        }
        Some(Snapshot {
            setpoint_dc: i16::from_le_bytes([b[0], b[1]]),
            temp_dc: i16::from_le_bytes([b[2], b[3]]),
            heater: b[4] == 1,
            relays,
            mode,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmp108_readings() {
        let dc = |r| match sensor_command(r) {
            Command::ReportTemp { dc } => dc,
            _ => unreachable!(),
        };
        // Datasheet examples: 0x190 = 25 °C, 0xE70 = -25 °C, 0x7FF = 127.9375 °C.
        assert_eq!(dc(Some([0x19, 0x00])), 250);
        assert_eq!(dc(Some([0xE7, 0x00])), -250);
        assert_eq!(dc(Some([0x7F, 0xF0])), 1279);
        assert_eq!(dc(Some([0x00, 0x10])), 0);
        assert_eq!(dc(Some([0x23, 0x00])), 350);
        assert_eq!(dc(None), MAX_TEMP_DC);
    }

    #[test]
    fn flash_codec_round_trips() {
        let s = Snapshot { setpoint_dc: -7, temp_dc: 351, heater: true, relays: [true, false, false, true], mode: Mode::Auto };
        let mut b = [0u8; 8];
        assert_eq!(Flash::encode(&s, &mut b), Some(8));
        assert_eq!(Flash::decode(&b), Some(s));
        assert_eq!(Flash::decode(&b[..7]), None);
        b[6] = 3;
        assert_eq!(Flash::decode(&b), None);
    }
}
