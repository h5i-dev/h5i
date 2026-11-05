//! The thermostat's HTTP API, shared by the host runner and the firmware.
//!
//! ```text
//! GET  /state                 the snapshot as JSON (no key needed)
//! PUT  /setpoint   "215"      tenths of a degree, 50..=300
//! POST /temp       "-12"      a sensor reading
//! PUT  /mode       "auto"     off | heat | auto
//! PUT  /relay/{i}  "on"       on | off; i >= 4 is refused by the kernel
//! ```
//!
//! Writes need `Authorization: Bearer <key>`. Decoding requests and encoding
//! replies is trusted (A2, A3); the kernel's theorems hold for every command.

#![no_std]

use core::fmt::Write as _;

use h5i_app_core::{Kernel, TenantId};
use h5i_app_embedded::kernel::{Api, Authenticator, DeviceEngine, PskAuth, respond};
use h5i_app_embedded::{Request, Response, Router, Routes};
use thermostat_kernel::{Command, Device, Error, Mode, Reply, Snapshot};

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

/// The device's state and key.
pub struct App {
    pub engine: DeviceEngine<Thermostat>,
    pub auth: PskAuth<Device>,
}

impl App {
    /// Starts from `thermostat_kernel::INITIAL`, the state the proofs start from.
    pub const fn new(key: &'static [u8]) -> App {
        App {
            engine: DeviceEngine::new(thermostat_kernel::INITIAL),
            auth: PskAuth { key, principal: Device { admin: true } },
        }
    }
}

/// Anyone may read.
struct Open;

impl Authenticator<Thermostat> for Open {
    fn authenticate(&self, _: &Request<'_>) -> Result<Device, u16> {
        Ok(Device { admin: false })
    }
}

/// Text bodies: a decimal, or a word.
pub struct Wire;

fn trimmed(b: &[u8]) -> &[u8] {
    b.trim_ascii()
}

/// `-?[0-9]{1,5}` within `i16`.
pub fn parse_i16(b: &[u8]) -> Option<i16> {
    let (neg, digits) = match b.split_first() {
        Some((b'-', rest)) => (true, rest),
        _ => (false, b),
    };
    if digits.is_empty() || digits.len() > 5 || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let mut v: i32 = 0;
    for &c in digits {
        v = v * 10 + (c - b'0') as i32;
    }
    i16::try_from(if neg { -v } else { v }).ok()
}

impl Api<Thermostat> for Wire {
    fn decode_command(req: &Request<'_>) -> Result<Command, u16> {
        let body = trimmed(req.body);
        match req.path {
            b"/state" => Ok(Command::Read),
            b"/setpoint" => parse_i16(body).map(|dc| Command::SetSetpoint { dc }).ok_or(400),
            b"/temp" => parse_i16(body).map(|dc| Command::ReportTemp { dc }).ok_or(400),
            b"/mode" => match body {
                b"off" => Ok(Command::SetMode { mode: Mode::Off }),
                b"heat" => Ok(Command::SetMode { mode: Mode::Heat }),
                b"auto" => Ok(Command::SetMode { mode: Mode::Auto }),
                _ => Err(400),
            },
            _ => {
                // /relay/{i}: the router matched it; the index is any u8.
                let idx = req.param_u32(0).ok_or(404u16)?;
                let idx = u8::try_from(idx).map_err(|_| 404u16)?;
                let on = match body {
                    b"on" => true,
                    b"off" => false,
                    _ => return Err(400),
                };
                Ok(Command::SetRelay { idx, on })
            }
        }
    }

    fn encode_reply(reply: &Reply, out: &mut [u8]) -> Option<usize> {
        let Reply::State(s) = reply;
        let mut w = Cursor { buf: out, at: 0 };
        let mode = match s.mode {
            Mode::Off => "off",
            Mode::Heat => "heat",
            Mode::Auto => "auto",
        };
        write!(
            w,
            "{{\"setpoint_dc\":{},\"temp_dc\":{},\"heater\":{},\"relays\":[{},{},{},{}],\"mode\":\"{}\"}}",
            s.setpoint_dc, s.temp_dc, s.heater, s.relays[0], s.relays[1], s.relays[2], s.relays[3], mode
        )
        .ok()?;
        Some(w.at)
    }

    fn encode_error(err: &Error, out: &mut [u8]) -> (u16, usize) {
        let (status, msg): (u16, &[u8]) = match err {
            Error::SetpointOutOfRange => (422, b"{\"error\":\"setpoint_out_of_range\"}"),
            Error::NoSuchRelay => (404, b"{\"error\":\"no_such_relay\"}"),
            Error::Forbidden => (403, b"{\"error\":\"forbidden\"}"),
        };
        let n = msg.len().min(out.len());
        out[..n].copy_from_slice(&msg[..n]);
        (status, n)
    }
}

struct Cursor<'a> {
    buf: &'a mut [u8],
    at: usize,
}

impl core::fmt::Write for Cursor<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let b = s.as_bytes();
        if b.len() > self.buf.len() - self.at {
            return Err(core::fmt::Error);
        }
        self.buf[self.at..self.at + b.len()].copy_from_slice(b);
        self.at += b.len();
        Ok(())
    }
}

async fn read(req: &Request<'_>, app: &App, out: &mut [u8]) -> Response {
    respond::<Thermostat, Wire>(&app.engine, &Open, req, out)
}

async fn write(req: &Request<'_>, app: &App, out: &mut [u8]) -> Response {
    respond::<Thermostat, Wire>(&app.engine, &app.auth, req, out)
}

/// The routes.
pub fn router() -> Router<impl Routes<App>> {
    Router::new()
        .get(b"/state", read)
        .put(b"/setpoint", write)
        .post(b"/temp", write)
        .put(b"/mode", write)
        .put(b"/relay/{i}", write)
}
