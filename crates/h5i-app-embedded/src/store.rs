//! [`FlashStore`]: committed snapshots in NOR flash, so a reset resumes from
//! the last commit. Trusted, tested (power cuts at every byte, `tests`).
//!
//! The flash range is split into two regions, each a whole number of erase
//! blocks. Each save appends one record to the current region:
//!
//! ```text
//! magic u32 | seq u32 | version u16 | len u16 | crc32 u32 | data[len] | 0xFF padding
//! ```
//!
//! (little-endian; the CRC covers `seq`, `version`, `len` and the data). When
//! the current region is full, or its tail is torn, the save erases the
//! other region and writes there. Loading takes the valid record with the
//! highest `seq` in either region. The region holding the newest record is
//! never erased, so a power cut at any point leaves either the new record or
//! the one before it; and every stored snapshot is one the engine committed,
//! so it is a reachable state of the kernel.
//!
//! A save whose bytes equal the newest record's is skipped, so a command that
//! changes nothing (a sensor reporting the same value) costs no flash wear.
//! A record whose `version` differs from [`SnapshotCodec::VERSION`] (a
//! firmware update changed the layout) is not loaded: the device starts from
//! its initial snapshot. Each region is erased once per
//! `region size / record size` saves; size the regions for the flash's
//! endurance and the expected write rate.

use core::marker::PhantomData;

use embedded_storage_async::nor_flash::NorFlash;
use h5i_app_core::Kernel;

use crate::kernel::Store;

/// How a snapshot is laid out in flash (trusted).
pub trait SnapshotCodec<K: Kernel> {
    /// Changes whenever the layout does.
    const VERSION: u16;
    /// The bytes of `s` in `out`, or `None` if they do not fit.
    fn encode(s: &K::Snapshot, out: &mut [u8]) -> Option<usize>;
    /// `None` for bytes `encode` cannot have written.
    fn decode(b: &[u8]) -> Option<K::Snapshot>;
}

const MAGIC: u32 = 0x5335_4948; // "HI5S"
const HDR: usize = 16;

/// Why a [`FlashStore`] could not load or save.
#[derive(Debug, PartialEq, Eq)]
pub enum StoreError<E> {
    Flash(E),
    /// The encoded snapshot does not fit in the record buffer or a region.
    TooLarge,
    /// The range is not two regions of whole, aligned erase blocks.
    Layout,
}

/// The current region and where the next record goes.
#[derive(Clone, Copy)]
struct Pos {
    region: u32,
    /// `None`: the region is full or torn; the next save switches.
    append: Option<u32>,
    seq: u32,
}

/// Snapshots of `K`, encoded by `C`, in `flash[base..base + 2 * region]`,
/// staged in a `BUF`-byte buffer (one whole record: 16 bytes of header, the
/// encoded snapshot, padding to the write size).
pub struct FlashStore<F, K, C, const BUF: usize> {
    flash: F,
    base: u32,
    region: u32,
    pos: Option<Pos>,
    buf: [u8; BUF],
    /// The newest record's data, if it has this codec's version.
    last: [u8; BUF],
    last_len: Option<usize>,
    _k: PhantomData<fn() -> (K, C)>,
}

impl<F: NorFlash, K: Kernel, C: SnapshotCodec<K>, const BUF: usize> FlashStore<F, K, C, BUF> {
    /// `region` bytes from `base` for one region, the next `region` for the
    /// other. Both must be multiples of `F::ERASE_SIZE`.
    pub const fn new(flash: F, base: u32, region: u32) -> Self {
        FlashStore { flash, base, region, pos: None, buf: [0xFF; BUF], last: [0; BUF], last_len: None, _k: PhantomData }
    }

    pub fn into_inner(self) -> F {
        self.flash
    }

    fn align() -> usize {
        let a = if F::WRITE_SIZE > F::READ_SIZE { F::WRITE_SIZE } else { F::READ_SIZE };
        if a > 4 { a } else { 4 }
    }

    fn record_len(len: usize) -> usize {
        let a = Self::align();
        (HDR + len).div_ceil(a) * a
    }

    fn layout_ok(&self) -> bool {
        let e = F::ERASE_SIZE as u32;
        let a = Self::align() as u32;
        e > 0
            && self.region > 0
            && self.region.is_multiple_of(e)
            && self.base.is_multiple_of(e)
            && (HDR as u32).is_multiple_of(a)
            && (self.base as u64) + 2 * (self.region as u64) <= self.flash.capacity() as u64
            && BUF >= HDR
            && BUF.is_multiple_of(Self::align())
    }

    fn region_start(&self, r: u32) -> u32 {
        self.base + r * self.region
    }

    /// The record at `off` in region `r`, if valid: (seq, version, len, record length).
    async fn record_at(&mut self, r: u32, off: u32) -> Result<Rec, F::Error> {
        if off as usize + HDR > self.region as usize {
            return Ok(Rec::End);
        }
        let at = self.region_start(r) + off;
        self.flash.read(at, &mut self.buf[..HDR]).await?;
        let h = &self.buf[..HDR];
        if h.iter().all(|&b| b == 0xFF) {
            return Ok(Rec::End);
        }
        let word = |i: usize| u32::from_le_bytes([h[i], h[i + 1], h[i + 2], h[i + 3]]);
        let magic = word(0);
        let seq = word(4);
        let version = u16::from_le_bytes([h[8], h[9]]);
        let len = u16::from_le_bytes([h[10], h[11]]) as usize;
        let crc = word(12);
        let rl = Self::record_len(len);
        if magic != MAGIC || rl > BUF || off as usize + rl > self.region as usize {
            return Ok(Rec::Torn);
        }
        if rl > HDR {
            self.flash.read(at + HDR as u32, &mut self.buf[HDR..rl]).await?;
        }
        let mut c = Crc::new();
        c.update(&self.buf[4..12]);
        c.update(&self.buf[HDR..HDR + len]);
        if c.finish() != crc {
            return Ok(Rec::Torn);
        }
        Ok(Rec::Valid { seq, version, len, rl })
    }

    /// Scan both regions; leave the newest valid record's data in `buf`.
    async fn scan(&mut self) -> Result<Option<(u16, usize)>, StoreError<F::Error>> {
        if !self.layout_ok() {
            return Err(StoreError::Layout);
        }
        let mut best: Option<(u32, u32, u32)> = None; // (seq, region, offset)
        let mut ends = [(0u32, false); 2]; // (end offset, torn)
        for r in 0..2u32 {
            let mut off = 0u32;
            loop {
                match self.record_at(r, off).await.map_err(StoreError::Flash)? {
                    Rec::End => break,
                    Rec::Torn => {
                        ends[r as usize].1 = true;
                        break;
                    }
                    Rec::Valid { seq, rl, .. } => {
                        if best.is_none_or(|(s, _, _)| seq > s) {
                            best = Some((seq, r, off));
                        }
                        off += rl as u32;
                    }
                }
            }
            ends[r as usize].0 = off;
        }
        self.last_len = None;
        let Some((seq, r, off)) = best else {
            self.pos = Some(Pos { region: 1, append: None, seq: 0 });
            return Ok(None);
        };
        let (end, torn) = ends[r as usize];
        // Appending after the newest record is safe only if it is the region's last.
        let Rec::Valid { version, len, rl, .. } = self.record_at(r, off).await.map_err(StoreError::Flash)? else {
            return Ok(None);
        };
        let last = off + rl as u32 == end;
        self.pos = Some(Pos { region: r, append: if torn || !last { None } else { Some(end) }, seq });
        if version == C::VERSION {
            self.last[..len].copy_from_slice(&self.buf[HDR..HDR + len]);
            self.last_len = Some(len);
        }
        Ok(Some((version, len)))
    }
}

enum Rec {
    End,
    Torn,
    Valid { seq: u32, version: u16, len: usize, rl: usize },
}

impl<F, K, C, const BUF: usize> Store<K> for FlashStore<F, K, C, BUF>
where
    F: NorFlash,
    K: Kernel,
    C: SnapshotCodec<K>,
{
    type Error = StoreError<F::Error>;

    async fn load(&mut self) -> Result<Option<K::Snapshot>, Self::Error> {
        match self.scan().await? {
            Some((v, len)) if v == C::VERSION => Ok(C::decode(&self.buf[HDR..HDR + len])),
            _ => Ok(None),
        }
    }

    async fn save(&mut self, snap: &K::Snapshot) -> Result<(), Self::Error> {
        let pos = match self.pos {
            Some(p) => p,
            None => {
                self.scan().await?;
                self.pos.ok_or(StoreError::Layout)?
            }
        };
        let len = C::encode(snap, &mut self.buf[HDR..]).ok_or(StoreError::TooLarge)?;
        if self.last_len == Some(len) && self.last[..len] == self.buf[HDR..HDR + len] {
            return Ok(());
        }
        let rl = Self::record_len(len);
        if len > u16::MAX as usize || rl > BUF || rl > self.region as usize {
            return Err(StoreError::TooLarge);
        }
        let seq = pos.seq.wrapping_add(1);
        self.buf[HDR + len..rl].fill(0xFF);
        self.buf[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        self.buf[4..8].copy_from_slice(&seq.to_le_bytes());
        self.buf[8..10].copy_from_slice(&C::VERSION.to_le_bytes());
        self.buf[10..12].copy_from_slice(&(len as u16).to_le_bytes());
        let mut c = Crc::new();
        c.update(&self.buf[4..12]);
        c.update(&self.buf[HDR..HDR + len]);
        self.buf[12..16].copy_from_slice(&c.finish().to_le_bytes());

        let fits = pos.append.filter(|&o| o as usize + rl <= self.region as usize);
        let (region, off) = match fits {
            Some(o) => (pos.region, o),
            None => {
                // Erase the region that does not hold the newest record.
                let other = 1 - pos.region;
                let s = self.region_start(other);
                // Whatever happens next, the current region keeps the newest record.
                self.pos = Some(Pos { append: None, ..pos });
                self.flash.erase(s, s + self.region).await.map_err(StoreError::Flash)?;
                (other, 0)
            }
        };
        // A failed or cut write leaves a torn tail: the next save switches.
        self.pos = Some(Pos { region: pos.region, append: None, seq: pos.seq });
        let at = self.region_start(region) + off;
        self.last_len = None;
        self.flash.write(at, &self.buf[..rl]).await.map_err(StoreError::Flash)?;
        self.pos = Some(Pos { region, append: Some(off + rl as u32), seq });
        self.last[..len].copy_from_slice(&self.buf[HDR..HDR + len]);
        self.last_len = Some(len);
        Ok(())
    }
}

/// CRC-32 (IEEE), bitwise: small, and records are short.
struct Crc(u32);

impl Crc {
    fn new() -> Self {
        Crc(0xFFFF_FFFF)
    }

    fn update(&mut self, data: &[u8]) {
        for &b in data {
            self.0 ^= b as u32;
            for _ in 0..8 {
                self.0 = if self.0 & 1 != 0 { (self.0 >> 1) ^ 0xEDB8_8320 } else { self.0 >> 1 };
            }
        }
    }

    fn finish(&self) -> u32 {
        !self.0
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use std::vec;
    use std::vec::Vec;

    use embedded_storage_async::nor_flash::{ErrorType, NorFlashError, NorFlashErrorKind, ReadNorFlash};
    use h5i_app_core::TenantId;
    use proptest::prelude::*;

    use super::*;

    struct K;
    impl Kernel for K {
        type Principal = ();
        type Snapshot = u64;
        type Command = ();
        type WriteSet = ();
        type Reply = ();
        type Error = ();
        fn tenant(_: &()) -> TenantId {
            TenantId(0)
        }
        fn transition(_: &(), _: &u64, _: &()) -> Result<((), ()), ()> {
            Ok(((), ()))
        }
        fn apply(s: &u64, _: &()) -> u64 {
            *s
        }
    }

    /// `v` as `n` bytes (`n >= 8`), so tests vary the record size.
    struct Wide<const N: usize>;
    impl<const N: usize> SnapshotCodec<K> for Wide<N> {
        const VERSION: u16 = 1;
        fn encode(s: &u64, out: &mut [u8]) -> Option<usize> {
            let o = out.get_mut(..N)?;
            o.fill(0x5A);
            o[..8].copy_from_slice(&s.to_le_bytes());
            Some(N)
        }
        fn decode(b: &[u8]) -> Option<u64> {
            if b.len() != N || b[8..].iter().any(|&x| x != 0x5A) {
                return None;
            }
            Some(u64::from_le_bytes(b[..8].try_into().ok()?))
        }
    }

    #[derive(Debug, PartialEq)]
    struct Cut;
    impl NorFlashError for Cut {
        fn kind(&self) -> NorFlashErrorKind {
            NorFlashErrorKind::Other
        }
    }

    /// NOR flash in memory: writes only clear bits, erases set them, and the
    /// power goes off after `budget` byte operations (an erase counts each
    /// byte), in the middle of whatever is under way.
    struct Mock {
        mem: Vec<u8>,
        budget: usize,
        erases: usize,
    }

    impl Mock {
        fn new(size: usize, budget: usize) -> Self {
            Mock { mem: vec![0xFF; size], budget, erases: 0 }
        }
        fn spend(&mut self) -> Result<(), Cut> {
            if self.budget == 0 {
                return Err(Cut);
            }
            self.budget -= 1;
            Ok(())
        }
    }

    impl ErrorType for Mock {
        type Error = Cut;
    }

    impl ReadNorFlash for Mock {
        const READ_SIZE: usize = 1;
        async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Cut> {
            if self.budget == 0 {
                return Err(Cut);
            }
            bytes.copy_from_slice(&self.mem[offset as usize..offset as usize + bytes.len()]);
            Ok(())
        }
        fn capacity(&self) -> usize {
            self.mem.len()
        }
    }

    impl NorFlash for Mock {
        const WRITE_SIZE: usize = 4;
        const ERASE_SIZE: usize = 256;
        async fn erase(&mut self, from: u32, to: u32) -> Result<(), Cut> {
            assert!(from.is_multiple_of(256) && to.is_multiple_of(256));
            self.erases += 1;
            for i in from..to {
                self.spend()?;
                self.mem[i as usize] = 0xFF;
            }
            Ok(())
        }
        async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Cut> {
            assert!(offset.is_multiple_of(4) && bytes.len().is_multiple_of(4), "unaligned write");
            for (i, &b) in bytes.iter().enumerate() {
                self.spend()?;
                let m = &mut self.mem[offset as usize + i];
                assert_eq!(*m, 0xFF, "write to unerased flash");
                *m &= b;
            }
            Ok(())
        }
    }

    type S<const N: usize> = FlashStore<Mock, K, Wide<N>, 256>;

    fn block_on<T>(f: impl core::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(f)
    }

    #[test]
    fn saves_and_loads() {
        block_on(async {
            let mut s: S<8> = FlashStore::new(Mock::new(1024, usize::MAX), 0, 512);
            assert_eq!(s.load().await, Ok(None));
            for v in 1..=200u64 {
                s.save(&v).await.unwrap();
            }
            let mock = s.into_inner();
            // 24-byte records, 21 per 512-byte region: one erase per 21 saves.
            assert!(mock.erases <= 200 / 21 + 1, "{}", mock.erases);
            let mut s: S<8> = FlashStore::new(mock, 0, 512);
            assert_eq!(s.load().await, Ok(Some(200)));
            s.save(&201).await.unwrap();
            let mut s: S<8> = FlashStore::new(s.into_inner(), 0, 512);
            assert_eq!(s.load().await, Ok(Some(201)));
        });
    }

    #[test]
    fn unchanged_saves_write_nothing() {
        block_on(async {
            let mut s: S<8> = FlashStore::new(Mock::new(1024, usize::MAX), 0, 512);
            s.save(&5).await.unwrap();
            let before = s.flash.mem.clone();
            for _ in 0..100 {
                s.save(&5).await.unwrap();
            }
            assert_eq!(s.flash.mem, before);
            // After a reboot too: the newest record is known from the scan.
            let mut s: S<8> = FlashStore::new(s.into_inner(), 0, 512);
            assert_eq!(s.load().await, Ok(Some(5)));
            s.save(&5).await.unwrap();
            assert_eq!(s.flash.mem, before);
            s.save(&6).await.unwrap();
            assert_ne!(s.flash.mem, before);
        });
    }

    #[test]
    fn layout_and_size() {
        block_on(async {
            let mut s: S<8> = FlashStore::new(Mock::new(1024, usize::MAX), 0, 300);
            assert_eq!(s.load().await, Err(StoreError::Layout));
            let mut s: S<8> = FlashStore::new(Mock::new(1024, usize::MAX), 512, 512);
            assert_eq!(s.load().await, Err(StoreError::Layout));
            let mut s: S<300> = FlashStore::new(Mock::new(1024, usize::MAX), 0, 512);
            assert_eq!(s.save(&1).await, Err(StoreError::TooLarge));
        });
    }

    #[test]
    fn other_version_is_not_loaded() {
        struct V2;
        impl SnapshotCodec<K> for V2 {
            const VERSION: u16 = 2;
            fn encode(s: &u64, out: &mut [u8]) -> Option<usize> {
                Wide::<8>::encode(s, out)
            }
            fn decode(b: &[u8]) -> Option<u64> {
                Wide::<8>::decode(b)
            }
        }
        block_on(async {
            let mut s: S<8> = FlashStore::new(Mock::new(1024, usize::MAX), 0, 512);
            s.save(&7).await.unwrap();
            let mut s2: FlashStore<Mock, K, V2, 256> = FlashStore::new(s.into_inner(), 0, 512);
            assert_eq!(s2.load().await, Ok(None));
            s2.save(&8).await.unwrap();
            assert_eq!(s2.load().await, Ok(Some(8)));
        });
    }

    /// Save 1, 2, ... with the power cut after `cut` byte operations; reboot,
    /// load, go on; repeat. After each reboot the store holds the last
    /// acknowledged value or the one in flight, never anything else.
    fn power_cuts<const N: usize>(cuts: &[usize]) -> Result<(), TestCaseError> {
        block_on(async {
            let mut mock = Mock::new(1024, usize::MAX);
            let mut acked: Option<u64> = None;
            let mut next = 1u64;
            for &cut in cuts {
                mock.budget = cut;
                let mut s: S<N> = FlashStore::new(mock, 0, 512);
                let loaded = s.load().await;
                let mut in_flight = None;
                if let Ok(v) = loaded {
                    prop_assert_eq!(v, acked);
                    // Save until the power goes; the value being saved then is in flight.
                    while s.save(&next).await.is_ok() {
                        acked = Some(next);
                        next += 1;
                    }
                    in_flight = Some(next);
                }
                mock = s.into_inner();
                mock.budget = usize::MAX;
                let mut check: S<N> =
                    FlashStore::new(Mock { mem: mock.mem.clone(), budget: usize::MAX, erases: 0 }, 0, 512);
                let v = check.load().await.unwrap();
                prop_assert!(
                    v == acked || (v.is_some() && v == in_flight),
                    "after cut {cut}: {v:?}, acked {acked:?}, in flight {in_flight:?}"
                );
                if in_flight.is_some() && v == in_flight {
                    acked = v;
                    next += 1;
                }
            }
            Ok(())
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(400))]
        #[test]
        fn power_cut_anywhere_small(cuts in proptest::collection::vec(0usize..4000, 1..12)) {
            power_cuts::<8>(&cuts)?;
        }
        #[test]
        fn power_cut_anywhere_large(cuts in proptest::collection::vec(0usize..4000, 1..12)) {
            power_cuts::<100>(&cuts)?;
        }
    }
}
