//! Pure, deterministic storage protocol core for [`crate::store::FlashStore`].
//!
//! `#![no_std]`, heap-free, async-free, and written in the Rust subset that
//! Charon and Aeneas extract directly to Lean 4 (`crates/h5i-app-embedded/proofs/`).
//! The production [`crate::store::FlashStore`] calls these exact functions and
//! executes the I/O actions they prescribe against `embedded_storage_async::nor_flash::NorFlash`.

/// Record header magic (`"HI5S"` little-endian).
pub const MAGIC: u32 = 0x5335_4948;

/// Fixed record header size in bytes:
/// `magic(4) | seq(4) | version(2) | len(2) | crc32(4)`.
pub const HDR: usize = 16;
pub const HDR_U32: u32 = 16;

/// Maximum payload length representable in the `u16` header field.
pub const MAX_PAYLOAD_LEN: u32 = 65_535;

/// Upper bound on each region's byte size (`< 2^30`).
/// Guarantees `2 * region` fits in `u32` and that the maximum number of valid
/// records across both regions (`<= 2 * (region / 16) < 2^27`) stays strictly
/// below the half-space window `2^31` required by [`seq_newer`].
pub const MAX_REGION_BYTES: u32 = 0x3FFF_FFFF;

/// Static geometry of a two-region NOR flash store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlashGeometry {
    pub base: u32,
    pub region: u32,
    pub read_size: u32,
    pub write_size: u32,
    pub erase_size: u32,
    pub capacity: u32,
    pub buf_cap: u32,
}

/// Active region and append cursor tracked between saves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pos {
    /// Active region (`0` or `1`) holding the newest committed record.
    pub region: u32,
    /// Byte offset inside `region` for the next append, or `None` if `region`
    /// is full, torn, or empty (so the next save switches to `1 - region`).
    pub append: Option<u32>,
    /// Sequence number of the newest committed record (or `0` if empty).
    pub seq: u32,
}

/// Effective record alignment: `max(write_size, read_size, 4)`.
pub fn align(write_size: u32, read_size: u32) -> u32 {
    let a = if write_size > read_size { write_size } else { read_size };
    if a > 4 { a } else { 4 }
}

/// Total aligned record size (`HDR + len`, rounded up to a multiple of `a`).
/// Returns `None` if `len > MAX_PAYLOAD_LEN` or `a == 0` or `a > HDR_U32`.
pub fn record_len(len: u32, a: u32) -> Option<u32> {
    if len > MAX_PAYLOAD_LEN || a == 0 || a > HDR_U32 {
        return None;
    }
    let total = HDR_U32 + len;
    let rem = total % a;
    if rem == 0 {
        Some(total)
    } else {
        Some(total + (a - rem))
    }
}

/// Validate that the flash layout and staging buffer satisfy all alignment,
/// bounds, and wraparound-window preconditions.
pub fn layout_ok(g: FlashGeometry) -> bool {
    let a = align(g.write_size, g.read_size);
    g.read_size > 0
        && g.write_size > 0
        && g.erase_size > 0
        && a <= HDR_U32
        && HDR_U32 % a == 0
        && g.region >= HDR_U32
        && g.region <= MAX_REGION_BYTES
        && g.region % g.erase_size == 0
        && g.region % a == 0
        && g.base % g.erase_size == 0
        && g.base % a == 0
        && g.base <= g.capacity
        && g.region * 2 <= g.capacity - g.base
        && g.buf_cap >= HDR_U32
        && g.buf_cap % a == 0
}

/// Starting flash address of region `r` (`0` or `1`).
pub fn region_start(g: FlashGeometry, r: u32) -> u32 {
    if r == 0 { g.base } else { g.base + g.region }
}

/// The Standby region opposite `r` (`0 <-> 1`).
pub fn other_region(r: u32) -> u32 {
    if r == 0 { 1 } else { 0 }
}

/// Next sequence number (wrapping modulo `2^32`).
pub fn next_seq(seq: u32) -> u32 {
    seq.wrapping_add(1)
}

/// Modular half-space sequence comparison (RFC 1982 serial-number arithmetic):
/// returns `true` iff `candidate` is strictly newer than `current` within a
/// `< 2^31` window (`0 < candidate.wrapping_sub(current) < 2^31`).
///
/// Correctly orders wraparound transitions such as `u32::MAX -> 0`.
pub fn seq_newer(candidate: u32, current: u32) -> bool {
    let d = candidate.wrapping_sub(current);
    d != 0 && d < 0x8000_0000
}

/* -------------------------------------------------------------------------- */
/* Little-endian encoding/decoding and CRC-32 (IEEE)                          */
/* -------------------------------------------------------------------------- */

pub fn get_u16_le(buf: &[u8], off: usize) -> Option<u16> {
    if off + 2 > buf.len() {
        return None;
    }
    let b0 = buf[off] as u16;
    let b1 = buf[off + 1] as u16;
    Some(b0 | (b1 << 8))
}

pub fn get_u32_le(buf: &[u8], off: usize) -> Option<u32> {
    if off + 4 > buf.len() {
        return None;
    }
    let b0 = buf[off] as u32;
    let b1 = buf[off + 1] as u32;
    let b2 = buf[off + 2] as u32;
    let b3 = buf[off + 3] as u32;
    Some(b0 | (b1 << 8) | (b2 << 16) | (b3 << 24))
}

pub fn put_u16_le(buf: &mut [u8], off: usize, v: u16) -> bool {
    if off + 2 > buf.len() {
        return false;
    }
    buf[off] = (v & 0xFF) as u8;
    buf[off + 1] = ((v >> 8) & 0xFF) as u8;
    true
}

pub fn put_u32_le(buf: &mut [u8], off: usize, v: u32) -> bool {
    if off + 4 > buf.len() {
        return false;
    }
    buf[off] = (v & 0xFF) as u8;
    buf[off + 1] = ((v >> 8) & 0xFF) as u8;
    buf[off + 2] = ((v >> 16) & 0xFF) as u8;
    buf[off + 3] = ((v >> 24) & 0xFF) as u8;
    true
}

/// Update an IEEE CRC-32 accumulator with a single byte.
pub fn crc32_byte(crc: u32, b: u8) -> u32 {
    let mut c = crc ^ (b as u32);
    let mut bit: u32 = 0;
    while bit < 8 {
        if (c & 1) != 0 {
            c = (c >> 1) ^ 0xEDB8_8320;
        } else {
            c >>= 1;
        }
        bit += 1;
    }
    c
}

/// Update an IEEE CRC-32 accumulator over `buf[start..end]`.
pub fn crc32_range(mut crc: u32, buf: &[u8], start: usize, end: usize) -> u32 {
    let limit = if end <= buf.len() { end } else { buf.len() };
    let mut i = start;
    while i < limit {
        crc = crc32_byte(crc, buf[i]);
        i += 1;
    }
    crc
}

/// Compute the finalized CRC-32 over header bytes `buf[4..12]` (`seq`,
/// `version`, `len`) followed by payload bytes `buf[16..16 + payload_len]`.
pub fn record_crc(buf: &[u8], payload_len: usize) -> Option<u32> {
    if payload_len > MAX_PAYLOAD_LEN as usize || HDR + payload_len > buf.len() {
        return None;
    }
    let c1 = crc32_range(0xFFFF_FFFF, buf, 4, 12);
    let c2 = crc32_range(c1, buf, HDR, HDR + payload_len);
    Some(!c2)
}

/// Returns `true` iff every byte in `buf[start..end]` equals `0xFF` (erased NOR flash).
pub fn all_erased(buf: &[u8], start: usize, end: usize) -> bool {
    if end > buf.len() {
        return false;
    }
    let mut i = start;
    while i < end {
        if buf[i] != 0xFF {
            return false;
        }
        i += 1;
    }
    true
}

/// Returns `true` iff `last_len == Some(payload_len)` and `last[..payload_len]`
/// equals `buf[HDR..HDR + payload_len]`.
pub fn payload_matches(
    last: &[u8],
    last_len: Option<usize>,
    buf: &[u8],
    payload_len: usize,
) -> bool {
    let Some(ll) = last_len else {
        return false;
    };
    if ll != payload_len || payload_len > last.len() || HDR + payload_len > buf.len() {
        return false;
    }
    let mut i = 0;
    while i < payload_len {
        if last[i] != buf[HDR + i] {
            return false;
        }
        i += 1;
    }
    true
}

/* -------------------------------------------------------------------------- */
/* Record Encoding & Validation                                               */
/* -------------------------------------------------------------------------- */

/// Frame a record in `buf` whose payload is already stored at `buf[HDR..HDR + payload_len]`.
/// Pads `buf[HDR + payload_len..rl]` with `0xFF`, writes the 16-byte header and
/// IEEE CRC-32, and returns `Some(rl)`.
pub fn encode_record(
    buf: &mut [u8],
    seq: u32,
    version: u16,
    payload_len: u32,
    a: u32,
) -> Option<u32> {
    let Some(rl) = record_len(payload_len, a) else {
        return None;
    };
    let rl_usize = rl as usize;
    let plen_usize = payload_len as usize;
    if rl_usize > buf.len() {
        return None;
    }
    let mut pad = HDR + plen_usize;
    while pad < rl_usize {
        buf[pad] = 0xFF;
        pad += 1;
    }
    if !put_u32_le(buf, 0, MAGIC) {
        return None;
    }
    if !put_u32_le(buf, 4, seq) {
        return None;
    }
    if !put_u16_le(buf, 8, version) {
        return None;
    }
    if !put_u16_le(buf, 10, payload_len as u16) {
        return None;
    }
    let Some(crc) = record_crc(buf, plen_usize) else {
        return None;
    };
    if !put_u32_le(buf, 12, crc) {
        return None;
    }
    Some(rl)
}

/// Decoded header metadata for one candidate record at offset `off`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeaderInfo {
    pub seq: u32,
    pub version: u16,
    pub payload_len: u32,
    pub record_len: u32,
}

/// Result of parsing a 16-byte header at `off` in a region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderParse {
    /// All 16 header bytes are `0xFF` (end of valid records in this region).
    Erased,
    /// Invalid magic, oversized length, or record overflows region/buffer.
    Torn,
    /// Syntactically valid header whose body (if `record_len > HDR`) must be read and CRC-checked.
    Ok(HeaderInfo),
}

/// Parse the 16-byte header in `buf[0..16]` for a record at region offset `off`.
pub fn parse_header(buf: &[u8], off: u32, g: FlashGeometry) -> HeaderParse {
    if buf.len() < HDR {
        return HeaderParse::Torn;
    }
    if all_erased(buf, 0, HDR) {
        return HeaderParse::Erased;
    }
    let Some(magic) = get_u32_le(buf, 0) else {
        return HeaderParse::Torn;
    };
    let Some(seq) = get_u32_le(buf, 4) else {
        return HeaderParse::Torn;
    };
    let Some(version) = get_u16_le(buf, 8) else {
        return HeaderParse::Torn;
    };
    let Some(len_u16) = get_u16_le(buf, 10) else {
        return HeaderParse::Torn;
    };
    let len = len_u16 as u32;
    let a = align(g.write_size, g.read_size);
    let Some(rl) = record_len(len, a) else {
        return HeaderParse::Torn;
    };
    if magic != MAGIC || rl > g.buf_cap || off > g.region || rl > g.region - off {
        return HeaderParse::Torn;
    }
    HeaderParse::Ok(HeaderInfo { seq, version, payload_len: len, record_len: rl })
}

/// Verify the stored CRC-32 in `buf[12..16]` against the header + payload in `buf`.
pub fn validate_record_crc(buf: &[u8], payload_len: u32) -> bool {
    let Some(stored) = get_u32_le(buf, 12) else {
        return false;
    };
    let Some(computed) = record_crc(buf, payload_len as usize) else {
        return false;
    };
    stored == computed
}

/* -------------------------------------------------------------------------- */
/* Save Protocol State Machine                                                */
/* -------------------------------------------------------------------------- */

/// Concrete I/O plan prescribed for saving a record of `payload_len` bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SavePlan {
    /// Snapshot does not fit in the buffer or region, or layout is invalid.
    TooLarge,
    /// Append directly into the active region without erasing.
    WriteOnly {
        target_region: u32,
        target_off: u32,
        write_addr: u32,
        write_len: u32,
        next_seq: u32,
        pos_in_flight: Pos,
    },
    /// Erase the standby region (`1 - pos.region`), then write at offset 0 there.
    EraseThenWrite {
        erase_from: u32,
        erase_to: u32,
        target_region: u32,
        write_addr: u32,
        write_len: u32,
        next_seq: u32,
        pos_in_flight: Pos,
    },
}

/// Deterministically decide append vs region-switch-and-erase for a save.
pub fn plan_save(g: FlashGeometry, pos: Pos, payload_len: u32) -> SavePlan {
    if !layout_ok(g) || pos.region > 1 {
        return SavePlan::TooLarge;
    }
    let a = align(g.write_size, g.read_size);
    let Some(rl) = record_len(payload_len, a) else {
        return SavePlan::TooLarge;
    };
    if rl > g.buf_cap || rl > g.region {
        return SavePlan::TooLarge;
    }
    let seq = next_seq(pos.seq);
    let pos_in_flight = Pos { region: pos.region, append: None, seq: pos.seq };
    match pos.append {
        Some(off) if off <= g.region && rl <= g.region - off && off % a == 0 => {
            let write_addr = region_start(g, pos.region) + off;
            SavePlan::WriteOnly {
                target_region: pos.region,
                target_off: off,
                write_addr,
                write_len: rl,
                next_seq: seq,
                pos_in_flight,
            }
        }
        _ => {
            let other = other_region(pos.region);
            let start = region_start(g, other);
            SavePlan::EraseThenWrite {
                erase_from: start,
                erase_to: start + g.region,
                target_region: other,
                write_addr: start,
                write_len: rl,
                next_seq: seq,
                pos_in_flight,
            }
        }
    }
}

/// Update `Pos` after a planned write completes cleanly.
pub fn pos_after_write(target_region: u32, target_off: u32, write_len: u32, seq: u32) -> Pos {
    Pos { region: target_region, append: Some(target_off + write_len), seq }
}

/* -------------------------------------------------------------------------- */
/* Recovery / Scan Protocol State Machine                                     */
/* -------------------------------------------------------------------------- */

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanPhase {
    ReadHeader,
    ReadBody(HeaderInfo),
    ReloadBest,
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BestRecord {
    pub seq: u32,
    pub region: u32,
    pub off: u32,
    pub version: u16,
    pub payload_len: u32,
    pub record_len: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanState {
    pub region: u32,
    pub off: u32,
    pub best: Option<BestRecord>,
    pub end0: u32,
    pub torn0: bool,
    pub end1: u32,
    pub torn1: bool,
    pub phase: ScanPhase,
}

/// Next I/O operation or terminal outcome emitted by the scan state machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanAction {
    /// Layout check failed; do not issue any flash I/O.
    InvalidLayout,
    /// Read `len` bytes from flash `addr` into `buf[buf_offset..buf_offset + len]`.
    Read { addr: u32, buf_offset: u32, len: u32 },
    /// Recovery finished; if `best_version` is `Some(v)`, the newest valid record's
    /// payload resides in `buf[HDR..HDR + best_payload_len]`.
    Done { pos: Pos, best_version: Option<u16>, best_payload_len: u32 },
}

/// Initialize the two-region recovery scan.
pub fn scan_init(g: FlashGeometry) -> (ScanState, ScanAction) {
    let st = ScanState {
        region: 0,
        off: 0,
        best: None,
        end0: 0,
        torn0: false,
        end1: 0,
        torn1: false,
        phase: ScanPhase::ReadHeader,
    };
    if !layout_ok(g) {
        let done_st = ScanState { phase: ScanPhase::Done, ..st };
        return (done_st, ScanAction::InvalidLayout);
    }
    let act = ScanAction::Read { addr: region_start(g, 0), buf_offset: 0, len: HDR_U32 };
    (st, act)
}

/// Helper: record the end/torn status of the current region and either start
/// scanning region 1 or transition to reloading the winning record / finishing.
fn finish_region(g: FlashGeometry, mut st: ScanState, torn: bool) -> (ScanState, ScanAction) {
    if st.region == 0 {
        st.end0 = st.off;
        st.torn0 = torn;
        st.region = 1;
        st.off = 0;
        st.phase = ScanPhase::ReadHeader;
        let act = ScanAction::Read { addr: region_start(g, 1), buf_offset: 0, len: HDR_U32 };
        (st, act)
    } else {
        st.end1 = st.off;
        st.torn1 = torn;
        match st.best {
            None => {
                st.phase = ScanPhase::Done;
                let pos = Pos { region: 1, append: None, seq: 0 };
                (st, ScanAction::Done { pos, best_version: None, best_payload_len: 0 })
            }
            Some(b) => {
                st.phase = ScanPhase::ReloadBest;
                let addr = region_start(g, b.region) + b.off;
                (st, ScanAction::Read { addr, buf_offset: 0, len: b.record_len })
            }
        }
    }
}

/// Helper: commit a validated record in the current region and advance `off`.
fn accept_valid_record(
    g: FlashGeometry,
    mut st: ScanState,
    hdr: HeaderInfo,
) -> (ScanState, ScanAction) {
    let replace = match st.best {
        None => true,
        Some(prev) => seq_newer(hdr.seq, prev.seq),
    };
    if replace {
        st.best = Some(BestRecord {
            seq: hdr.seq,
            region: st.region,
            off: st.off,
            version: hdr.version,
            payload_len: hdr.payload_len,
            record_len: hdr.record_len,
        });
    }
    st.off += hdr.record_len;
    if st.off <= g.region && HDR_U32 <= g.region - st.off {
        st.phase = ScanPhase::ReadHeader;
        let addr = region_start(g, st.region) + st.off;
        (st, ScanAction::Read { addr, buf_offset: 0, len: HDR_U32 })
    } else {
        finish_region(g, st, false)
    }
}

/// Compute the post-scan `Pos` for the winning record `b`.
pub fn scan_result_pos(st: ScanState, b: BestRecord) -> Pos {
    let (end, torn) = if b.region == 0 { (st.end0, st.torn0) } else { (st.end1, st.torn1) };
    let is_last = b.off + b.record_len == end;
    let append = if torn || !is_last { None } else { Some(end) };
    Pos { region: b.region, append, seq: b.seq }
}

/// Advance the recovery state machine after the previously requested `ScanAction::Read`
/// has filled `buf`.
pub fn scan_step(g: FlashGeometry, mut st: ScanState, buf: &[u8]) -> (ScanState, ScanAction) {
    if !layout_ok(g) {
        st.phase = ScanPhase::Done;
        return (st, ScanAction::InvalidLayout);
    }
    match st.phase {
        ScanPhase::ReadHeader => match parse_header(buf, st.off, g) {
            HeaderParse::Erased => finish_region(g, st, false),
            HeaderParse::Torn => finish_region(g, st, true),
            HeaderParse::Ok(hdr) => {
                if hdr.record_len > HDR_U32 {
                    st.phase = ScanPhase::ReadBody(hdr);
                    let addr = region_start(g, st.region) + st.off + HDR_U32;
                    let body_len = hdr.record_len - HDR_U32;
                    (st, ScanAction::Read { addr, buf_offset: HDR_U32, len: body_len })
                } else if validate_record_crc(buf, hdr.payload_len) {
                    accept_valid_record(g, st, hdr)
                } else {
                    finish_region(g, st, true)
                }
            }
        },
        ScanPhase::ReadBody(hdr) => {
            if validate_record_crc(buf, hdr.payload_len) {
                accept_valid_record(g, st, hdr)
            } else {
                finish_region(g, st, true)
            }
        }
        ScanPhase::ReloadBest => {
            st.phase = ScanPhase::Done;
            match st.best {
                None => {
                    let pos = Pos { region: 1, append: None, seq: 0 };
                    (st, ScanAction::Done { pos, best_version: None, best_payload_len: 0 })
                }
                Some(b) => {
                    let pos = scan_result_pos(st, b);
                    if validate_record_crc(buf, b.payload_len) {
                        (
                            st,
                            ScanAction::Done {
                                pos,
                                best_version: Some(b.version),
                                best_payload_len: b.payload_len,
                            },
                        )
                    } else {
                        (st, ScanAction::Done { pos, best_version: None, best_payload_len: 0 })
                    }
                }
            }
        }
        ScanPhase::Done => {
            let pos = match st.best {
                None => Pos { region: 1, append: None, seq: 0 },
                Some(b) => scan_result_pos(st, b),
            };
            (st, ScanAction::Done { pos, best_version: None, best_payload_len: 0 })
        }
    }
}
