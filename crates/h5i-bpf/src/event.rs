//! The event the kernel hands up, and the decoding of it.
//!
//! Mirrors `bpf/h5i_event.h` field for field. The two are held together by
//! three things, in increasing order of how much they would embarrass us if
//! they were the only one:
//!
//! 1. A compile-time assertion on [`RawEvent`]'s size and alignment here.
//! 2. A magic word and a version in every record, checked on decode, so a probe
//!    object and a loader that disagree are caught at the first event rather than
//!    turned into plausible-looking nonsense.
//! 3. `tests/wire_contract.rs`, which parses the C header and checks the
//!    constants against the Rust ones. The only one of the three that notices a
//!    *field* moving rather than the struct changing size.
//!
//! Nothing here does I/O or touches a kernel, so it compiles and is tested on
//! every target h5i releases for, including the ones where eBPF is not a concept.

use std::fmt;

/// `"h5iE"`. Must equal `H5I_EVENT_MAGIC` in `bpf/h5i_event.h`.
pub const EVENT_MAGIC: u32 = 0x6835_6945;
/// Must equal `H5I_EVENT_VERSION`. Bumped when a field's meaning changes;
/// appending a kind does not need a bump, because an unknown kind decodes to
/// [`EventKind::Unknown`] rather than to garbage.
pub const EVENT_VERSION: u16 = 1;

pub const COMM_LEN: usize = 16;
pub const PATH_LEN: usize = 256;
pub const AUX_LEN: usize = 192;
/// `aux` carries two independent strings at fixed offsets; see the header.
pub const AUX_HALF: usize = 96;

/// Ring-buffer geometry the loader programs. Must match the header.
pub const MAX_PREFIX: usize = 16;
pub const PREFIX_LEN: usize = 64;

/// Statistics slots in the per-CPU counter array.
pub const STAT_EMITTED: u32 = 0;
pub const STAT_LOST: u32 = 1;
pub const STAT_FILTERED: u32 = 2;
pub const STAT_MAX: u32 = 3;

/// The record exactly as it crosses the ring buffer.
///
/// `repr(C)` and never reordered: this is a wire type, not a domain type. The
/// domain type is [`Event`], which is what everything above the loader sees.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RawEvent {
    pub magic: u32,
    pub version: u16,
    pub kind: u16,
    pub ts_ns: u64,
    pub tgid: u32,
    pub tid: u32,
    pub ppid: u32,
    pub uid: u32,
    pub a0: i64,
    pub a1: i64,
    pub a2: i64,
    pub comm: [u8; COMM_LEN],
    pub path: [u8; PATH_LEN],
    pub aux: [u8; AUX_LEN],
}

// The C struct is 520 bytes with 8-byte alignment. If either side moves, this
// is where the build stops.
const _: () = assert!(std::mem::size_of::<RawEvent>() == 520);
const _: () = assert!(std::mem::align_of::<RawEvent>() == 8);

/// What the probe observed. Wire values; append, never renumber.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EventKind {
    Exec,
    Open,
    Connect,
    Socket,
    Ptrace,
    Bpf,
    Nsop,
    Module,
    Memfd,
    Mount,
    Fork,
    Exit,
    /// A kind this build does not know. Carried rather than dropped so a
    /// newer probe against an older loader degrades into "something happened
    /// that I cannot name", which is a true statement, instead of into
    /// silence, which is not.
    Unknown(u16),
}

impl EventKind {
    pub fn from_wire(v: u16) -> Self {
        match v {
            1 => Self::Exec,
            2 => Self::Open,
            3 => Self::Connect,
            4 => Self::Socket,
            5 => Self::Ptrace,
            6 => Self::Bpf,
            7 => Self::Nsop,
            8 => Self::Module,
            9 => Self::Memfd,
            10 => Self::Mount,
            11 => Self::Fork,
            12 => Self::Exit,
            other => Self::Unknown(other),
        }
    }

    pub fn to_wire(self) -> u16 {
        match self {
            Self::Exec => 1,
            Self::Open => 2,
            Self::Connect => 3,
            Self::Socket => 4,
            Self::Ptrace => 5,
            Self::Bpf => 6,
            Self::Nsop => 7,
            Self::Module => 8,
            Self::Memfd => 9,
            Self::Mount => 10,
            Self::Fork => 11,
            Self::Exit => 12,
            Self::Unknown(v) => v,
        }
    }

    /// Short, stable name. Used in the receipt and by `detect rules`, so it is
    /// part of the interface rather than a debug convenience.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exec => "exec",
            Self::Open => "open",
            Self::Connect => "connect",
            Self::Socket => "socket",
            Self::Ptrace => "ptrace",
            Self::Bpf => "bpf",
            Self::Nsop => "nsop",
            Self::Module => "module",
            Self::Memfd => "memfd",
            Self::Mount => "mount",
            Self::Fork => "fork",
            Self::Exit => "exit",
            Self::Unknown(_) => "unknown",
        }
    }

    /// Every kind this build collects, in wire order. The kind mask the loader
    /// pushes into the config map is built from this.
    pub const ALL: [EventKind; 12] = [
        Self::Exec,
        Self::Open,
        Self::Connect,
        Self::Socket,
        Self::Ptrace,
        Self::Bpf,
        Self::Nsop,
        Self::Module,
        Self::Memfd,
        Self::Mount,
        Self::Fork,
        Self::Exit,
    ];
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown(v) => write!(f, "unknown({v})"),
            other => f.write_str(other.as_str()),
        }
    }
}

/// A socket address family, only as far as the rules care.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Unix,
    Inet,
    Inet6,
    Other(i64),
}

impl Family {
    pub fn from_wire(v: i64) -> Self {
        match v {
            1 => Self::Unix,
            2 => Self::Inet,
            10 => Self::Inet6,
            other => Self::Other(other),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unix => "unix",
            Self::Inet => "inet",
            Self::Inet6 => "inet6",
            Self::Other(_) => "other",
        }
    }
}

/// The decoded event, which is what the rules and the session see.
///
/// Strings are lossy-decoded and truncated at the first NUL. They are the
/// bytes a *process* passed to a syscall, not anything the kernel resolved, so
/// every consumer treats them as a hint (design-detect.md D13.3), and they can
/// contain anything, including terminal control sequences, which is why the
/// rendering path runs them through `h5i_error::redact`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub kind: EventKind,
    pub ts_ns: u64,
    pub tgid: u32,
    pub tid: u32,
    /// Filled in by the session from the Fork events it has already seen; the
    /// probe never reports it (design-detect.md D5).
    pub ppid: u32,
    pub uid: u32,
    pub a0: i64,
    pub a1: i64,
    pub a2: i64,
    pub comm: String,
    pub path: String,
    /// `aux[..AUX_HALF]`, per kind: `argv[1]` for `Exec`, the source for
    /// `Mount`, the raw address bytes for `Connect`.
    pub aux: String,
    /// `aux[AUX_HALF..]`: `argv[2]` for `Exec`, `put_old` for `pivot_root`.
    pub aux2: String,
    /// `Connect` only: the peer address, rendered. `None` for a family this
    /// build does not render, which is not the same as "no address".
    pub peer: Option<String>,
}

/// Why a ring-buffer record was not turned into an [`Event`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// Fewer bytes than a record. A torn read, or a probe with a smaller
    /// event.
    Short { got: usize, want: usize },
    /// The magic word is not ours.
    Magic(u32),
    /// Our magic, a version we do not speak.
    Version(u16),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Short { got, want } => {
                write!(f, "ring-buffer record too short: {got} bytes, want {want}")
            }
            Self::Magic(m) => write!(
                f,
                "ring-buffer record has magic {m:#010x}, want {EVENT_MAGIC:#010x} \
                 (a probe object from a different build)"
            ),
            Self::Version(v) => write!(
                f,
                "probe speaks event version {v}, this build speaks {EVENT_VERSION}"
            ),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Take a NUL-terminated string out of a fixed byte field.
///
/// Lossy, deliberately: a path is whatever bytes a process passed, and
/// refusing to decode invalid UTF-8 would mean the one event most worth
/// looking at, a deliberately mangled path, is the one that disappears.
fn cstr(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Render an `AF_UNIX` `sun_path`, including the abstract-namespace form.
///
/// An abstract socket's name starts with a NUL, so `cstr` would render every
/// one of them as the empty string, and abstract sockets are exactly the ones
/// worth seeing, since that is how a browser's daemon, a D-Bus and an X server
/// are all reached.
fn unix_path(bytes: &[u8]) -> String {
    if bytes.first() == Some(&0) {
        let rest = &bytes[1..];
        let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        format!("@{}", String::from_utf8_lossy(&rest[..end]))
    } else {
        cstr(bytes)
    }
}

fn render_ipv4(b: &[u8]) -> Option<String> {
    (b.len() >= 4).then(|| format!("{}.{}.{}.{}", b[0], b[1], b[2], b[3]))
}

fn render_ipv6(b: &[u8]) -> Option<String> {
    if b.len() < 16 {
        return None;
    }
    let groups: Vec<String> = (0..8)
        .map(|i| format!("{:x}", u16::from_be_bytes([b[i * 2], b[i * 2 + 1]])))
        .collect();
    Some(groups.join(":"))
}

impl Event {
    /// Decode one ring-buffer record.
    ///
    /// Reads the header fields out of the byte slice explicitly rather than
    /// casting to `RawEvent`: a ring-buffer record is only 8-byte aligned by
    /// convention, and `RawEvent` needs 8-byte alignment, so a cast would be
    /// unsound the day the convention changes. The field-by-field read costs
    /// nothing measurable at these volumes and cannot be wrong.
    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        const SZ: usize = std::mem::size_of::<RawEvent>();
        if buf.len() < SZ {
            return Err(DecodeError::Short {
                got: buf.len(),
                want: SZ,
            });
        }
        let u32_at = |o: usize| u32::from_ne_bytes([buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]);
        let u16_at = |o: usize| u16::from_ne_bytes([buf[o], buf[o + 1]]);
        let i64_at = |o: usize| {
            i64::from_ne_bytes([
                buf[o],
                buf[o + 1],
                buf[o + 2],
                buf[o + 3],
                buf[o + 4],
                buf[o + 5],
                buf[o + 6],
                buf[o + 7],
            ])
        };

        let magic = u32_at(0);
        if magic != EVENT_MAGIC {
            return Err(DecodeError::Magic(magic));
        }
        let version = u16_at(4);
        if version != EVENT_VERSION {
            return Err(DecodeError::Version(version));
        }
        let kind = EventKind::from_wire(u16_at(6));
        let ts_ns = i64_at(8) as u64;
        let tgid = u32_at(16);
        let tid = u32_at(20);
        let ppid = u32_at(24);
        let uid = u32_at(28);
        let a0 = i64_at(32);
        let a1 = i64_at(40);
        let a2 = i64_at(48);

        const COMM_OFF: usize = 56;
        const PATH_OFF: usize = COMM_OFF + COMM_LEN;
        const AUX_OFF: usize = PATH_OFF + PATH_LEN;

        let comm = cstr(&buf[COMM_OFF..COMM_OFF + COMM_LEN]);
        let path_bytes = &buf[PATH_OFF..PATH_OFF + PATH_LEN];
        let aux_bytes = &buf[AUX_OFF..AUX_OFF + AUX_LEN];

        // `Connect` reuses the string fields as binary: the address bytes go in
        // `aux`, and an `AF_UNIX` peer goes in `path` where a leading NUL is
        // meaningful. Decoding those as C strings would silently blank both.
        let (path, aux, aux2, peer) = match kind {
            EventKind::Connect => match Family::from_wire(a0) {
                Family::Inet => (String::new(), String::new(), String::new(), render_ipv4(aux_bytes)),
                Family::Inet6 => (String::new(), String::new(), String::new(), render_ipv6(aux_bytes)),
                Family::Unix => {
                    let p = unix_path(path_bytes);
                    let peer = (!p.is_empty()).then(|| p.clone());
                    (p, String::new(), String::new(), peer)
                }
                Family::Other(_) => (String::new(), String::new(), String::new(), None),
            },
            _ => (
                cstr(path_bytes),
                cstr(&aux_bytes[..AUX_HALF]),
                cstr(&aux_bytes[AUX_HALF..]),
                None,
            ),
        };

        Ok(Event {
            kind,
            ts_ns,
            tgid,
            tid,
            ppid,
            uid,
            a0,
            a1,
            a2,
            comm,
            path,
            aux,
            aux2,
            peer,
        })
    }

    /// `Open`: did the caller ask for write access?
    pub fn write_intent(&self) -> bool {
        self.kind == EventKind::Open && self.a1 != 0
    }

    /// `Connect`: the peer port in host byte order.
    pub fn port(&self) -> Option<u16> {
        (self.kind == EventKind::Connect).then(|| u16::try_from(self.a1).unwrap_or(0))
    }

    /// `Connect`: the address family.
    pub fn family(&self) -> Option<Family> {
        matches!(self.kind, EventKind::Connect | EventKind::Socket)
            .then(|| Family::from_wire(self.a0))
    }

    /// The command line, as far as the probe captured it: the executable plus
    /// `argv[1]` and `argv[2]`. Only meaningful for `Exec`.
    pub fn cmdline(&self) -> String {
        let mut s = self.path.clone();
        for part in [&self.aux, &self.aux2] {
            if !part.is_empty() {
                s.push(' ');
                s.push_str(part);
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a wire record the way the probe would, so the decoder is tested
    /// against bytes rather than against itself.
    pub(crate) fn wire(kind: u16, a0: i64, a1: i64, a2: i64, path: &[u8], aux: &[u8]) -> Vec<u8> {
        let mut b = vec![0u8; std::mem::size_of::<RawEvent>()];
        b[0..4].copy_from_slice(&EVENT_MAGIC.to_ne_bytes());
        b[4..6].copy_from_slice(&EVENT_VERSION.to_ne_bytes());
        b[6..8].copy_from_slice(&kind.to_ne_bytes());
        b[8..16].copy_from_slice(&7u64.to_ne_bytes());
        b[16..20].copy_from_slice(&100u32.to_ne_bytes());
        b[20..24].copy_from_slice(&100u32.to_ne_bytes());
        b[24..28].copy_from_slice(&0u32.to_ne_bytes());
        b[28..32].copy_from_slice(&1000u32.to_ne_bytes());
        b[32..40].copy_from_slice(&a0.to_ne_bytes());
        b[40..48].copy_from_slice(&a1.to_ne_bytes());
        b[48..56].copy_from_slice(&a2.to_ne_bytes());
        b[56..60].copy_from_slice(b"sh\0\0");
        let po = 72;
        b[po..po + path.len()].copy_from_slice(path);
        let ao = po + PATH_LEN;
        b[ao..ao + aux.len()].copy_from_slice(aux);
        b
    }

    #[test]
    fn decodes_an_exec() {
        let raw = wire(1, 3, 0, 0, b"/bin/sh\0", b"-c\0");
        let e = Event::decode(&raw).unwrap();
        assert_eq!(e.kind, EventKind::Exec);
        assert_eq!(e.path, "/bin/sh");
        assert_eq!(e.aux, "-c");
        assert_eq!(e.a0, 3);
        assert_eq!(e.comm, "sh");
    }

    #[test]
    fn exec_carries_both_argv_slots() {
        let mut aux = vec![0u8; AUX_LEN];
        aux[..2].copy_from_slice(b"-c");
        aux[AUX_HALF..AUX_HALF + 4].copy_from_slice(b"true");
        let raw = wire(1, 3, 0, 0, b"/bin/sh\0", &aux);
        let e = Event::decode(&raw).unwrap();
        assert_eq!(e.aux, "-c");
        assert_eq!(e.aux2, "true");
        assert_eq!(e.cmdline(), "/bin/sh -c true");
    }

    #[test]
    fn decodes_an_ipv4_connect() {
        let raw = wire(3, 2, 443, 16, b"", &[93, 184, 216, 34]);
        let e = Event::decode(&raw).unwrap();
        assert_eq!(e.family(), Some(Family::Inet));
        assert_eq!(e.port(), Some(443));
        assert_eq!(e.peer.as_deref(), Some("93.184.216.34"));
    }

    #[test]
    fn decodes_an_ipv6_connect() {
        let mut addr = [0u8; 16];
        addr[0] = 0x20;
        addr[1] = 0x01;
        addr[15] = 0x01;
        let raw = wire(3, 10, 80, 28, b"", &addr);
        let e = Event::decode(&raw).unwrap();
        assert_eq!(e.peer.as_deref(), Some("2001:0:0:0:0:0:0:1"));
    }

    /// An abstract socket's name begins with a NUL. Rendering it as an empty
    /// string would hide exactly the sockets worth seeing.
    #[test]
    fn abstract_unix_sockets_are_not_blanked() {
        let mut path = vec![0u8; 32];
        path[1..8].copy_from_slice(b"h5i.sok");
        let raw = wire(3, 1, 0, 0, &path, b"");
        let e = Event::decode(&raw).unwrap();
        assert_eq!(e.peer.as_deref(), Some("@h5i.sok"));
    }

    #[test]
    fn filesystem_unix_sockets_decode_as_paths() {
        let raw = wire(3, 1, 0, 0, b"/run/user/1000/bus\0", b"");
        let e = Event::decode(&raw).unwrap();
        assert_eq!(e.peer.as_deref(), Some("/run/user/1000/bus"));
    }

    #[test]
    fn a_foreign_magic_is_refused_not_guessed() {
        let mut raw = wire(1, 0, 0, 0, b"/bin/true\0", b"");
        raw[0] = 0xff;
        assert!(matches!(Event::decode(&raw), Err(DecodeError::Magic(_))));
    }

    #[test]
    fn a_future_version_is_refused() {
        let mut raw = wire(1, 0, 0, 0, b"/bin/true\0", b"");
        raw[4..6].copy_from_slice(&(EVENT_VERSION + 1).to_ne_bytes());
        assert!(matches!(Event::decode(&raw), Err(DecodeError::Version(_))));
    }

    #[test]
    fn a_torn_record_is_short_not_panicking() {
        let raw = wire(1, 0, 0, 0, b"/bin/true\0", b"");
        assert!(matches!(
            Event::decode(&raw[..64]),
            Err(DecodeError::Short { .. })
        ));
    }

    /// An unknown kind must survive as "something I cannot name", because the
    /// alternative, dropping it, makes a newer probe look like a quiet one.
    #[test]
    fn an_unknown_kind_survives() {
        let raw = wire(999, 0, 0, 0, b"/x\0", b"");
        let e = Event::decode(&raw).unwrap();
        assert_eq!(e.kind, EventKind::Unknown(999));
        assert_eq!(e.kind.to_string(), "unknown(999)");
    }

    #[test]
    fn invalid_utf8_in_a_path_does_not_lose_the_event() {
        let raw = wire(2, 0, 1, 0, &[0x2f, 0xff, 0xfe, 0x00], b"");
        let e = Event::decode(&raw).unwrap();
        assert!(e.path.starts_with('/'));
        assert!(e.write_intent());
    }

    #[test]
    fn kind_wire_values_round_trip() {
        for k in EventKind::ALL {
            assert_eq!(EventKind::from_wire(k.to_wire()), k);
        }
    }
}
