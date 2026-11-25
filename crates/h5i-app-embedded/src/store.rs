//! [`FlashStore`]: committed snapshots in NOR flash, so a reset resumes from
//! the last commit.
//!
//! All deterministic storage logic (record framing, IEEE CRC-32, modular
//! sequence-number ordering across `u32::MAX -> 0` wraparound, layout checks,
//! recovery scan state machine, and append/region-switch planning) lives in
//! [`crate::store_core`], which is extracted via Charon + Aeneas and formally
//! verified in Lean 4 (`crates/h5i-app-embedded/proofs/`). This module is the
//! thin asynchronous `no_std` driver executing those I/O actions against
//! `embedded_storage_async::nor_flash::NorFlash`.
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
//! other region and writes there. Loading takes the newest valid record in
//! either region using modular half-space sequence ordering
//! (`store_core::seq_newer`). The region holding the newest record is never
//! erased, so a power cut at any point leaves either the new record or the one
//! before it; and every stored snapshot is one the engine committed, so it is
//! a reachable state of the kernel.
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
use crate::store_core::{
    self, FlashGeometry, HDR, Pos, SavePlan, ScanAction,
};

/// How a snapshot is laid out in flash (trusted).
pub trait SnapshotCodec<K: Kernel> {
    /// Changes whenever the layout does.
    const VERSION: u16;
    /// The bytes of `s` in `out`, or `None` if they do not fit.
    fn encode(s: &K::Snapshot, out: &mut [u8]) -> Option<usize>;
    /// `None` for bytes `encode` cannot have written.
    fn decode(b: &[u8]) -> Option<K::Snapshot>;
}

/// Why a [`FlashStore`] could not load or save.
#[derive(Debug, PartialEq, Eq)]
pub enum StoreError<E> {
    Flash(E),
    /// The encoded snapshot does not fit in the record buffer or a region.
    TooLarge,
    /// The range is not two regions of whole, aligned erase blocks.
    Layout,
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
        FlashStore {
            flash,
            base,
            region,
            pos: None,
            buf: [0xFF; BUF],
            last: [0; BUF],
            last_len: None,
            _k: PhantomData,
        }
    }

    pub fn into_inner(self) -> F {
        self.flash
    }

    fn geometry(&self) -> FlashGeometry {
        let cap = self.flash.capacity();
        let capacity = if cap > u32::MAX as usize { u32::MAX } else { cap as u32 };
        let buf_cap = if BUF > u32::MAX as usize { u32::MAX } else { BUF as u32 };
        FlashGeometry {
            base: self.base,
            region: self.region,
            read_size: F::READ_SIZE as u32,
            write_size: F::WRITE_SIZE as u32,
            erase_size: F::ERASE_SIZE as u32,
            capacity,
            buf_cap,
        }
    }

    /// Drive the verified [`store_core::scan_init`] / [`store_core::scan_step`]
    /// state machine across both regions; leaves the newest valid record's
    /// payload in `buf[HDR..HDR + len]`.
    async fn scan(&mut self) -> Result<Option<(u16, usize)>, StoreError<F::Error>> {
        let g = self.geometry();
        let (mut st, mut action) = store_core::scan_init(g);
        self.last_len = None;
        loop {
            match action {
                ScanAction::InvalidLayout => return Err(StoreError::Layout),
                ScanAction::Read { addr, buf_offset, len } => {
                    let start = buf_offset as usize;
                    let end = start + len as usize;
                    self.flash
                        .read(addr, &mut self.buf[start..end])
                        .await
                        .map_err(StoreError::Flash)?;
                    let (next_st, next_act) = store_core::scan_step(g, st, &self.buf);
                    st = next_st;
                    action = next_act;
                }
                ScanAction::Done { pos, best_version, best_payload_len } => {
                    self.pos = Some(pos);
                    let Some(version) = best_version else {
                        return Ok(None);
                    };
                    let len = best_payload_len as usize;
                    if version == C::VERSION {
                        self.last[..len].copy_from_slice(&self.buf[HDR..HDR + len]);
                        self.last_len = Some(len);
                    }
                    return Ok(Some((version, len)));
                }
            }
        }
    }
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
        if store_core::payload_matches(&self.last, self.last_len, &self.buf, len) {
            return Ok(());
        }
        if len > u16::MAX as usize {
            return Err(StoreError::TooLarge);
        }
        let g = self.geometry();
        let a = store_core::align(g.write_size, g.read_size);
        let plan = store_core::plan_save(g, pos, len as u32);
        let (target_region, target_off, write_addr, write_len, next_seq) = match plan {
            SavePlan::TooLarge => return Err(StoreError::TooLarge),
            SavePlan::WriteOnly {
                target_region,
                target_off,
                write_addr,
                write_len,
                next_seq,
            } => {
                store_core::encode_record(&mut self.buf, next_seq, C::VERSION, len as u32, a)
                    .ok_or(StoreError::TooLarge)?;
                // Invalidate cached `pos` and `last_len` before mutating flash so that
                // any ambiguous write failure or async cancellation forces a full rescan
                // on the next save, avoiding duplicate sequence numbers across regions.
                self.pos = None;
                self.last_len = None;
                (target_region, target_off, write_addr, write_len, next_seq)
            }
            SavePlan::EraseThenWrite {
                erase_from,
                erase_to,
                target_region,
                write_addr,
                write_len,
                next_seq,
            } => {
                store_core::encode_record(&mut self.buf, next_seq, C::VERSION, len as u32, a)
                    .ok_or(StoreError::TooLarge)?;
                // Invalidate cached `pos` and `last_len` before erasing the standby region.
                // The active region keeps the newest record; if erase or write fails or is
                // cancelled, the next save rescans both regions to recover the true state.
                self.pos = None;
                self.last_len = None;
                self.flash.erase(erase_from, erase_to).await.map_err(StoreError::Flash)?;
                (target_region, 0, write_addr, write_len, next_seq)
            }
        };
        self.flash
            .write(write_addr, &self.buf[..write_len as usize])
            .await
            .map_err(StoreError::Flash)?;
        self.pos = Some(store_core::pos_after_write(target_region, target_off, write_len, next_seq));
        self.last[..len].copy_from_slice(&self.buf[HDR..HDR + len]);
        self.last_len = Some(len);
        Ok(())
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

    #[test]
    fn wraparound_preserves_durability_across_u32_max_to_zero() {
        block_on(async {
            let mut mock = Mock::new(1024, usize::MAX);
            // Seed a valid record at region 0 offset 0 with `seq = u32::MAX` and payload `1`.
            let mut rec = [0xFFu8; 24];
            Wide::<8>::encode(&1u64, &mut rec[HDR..]).unwrap();
            let rl = store_core::encode_record(&mut rec, u32::MAX, 1, 8, 4).unwrap() as usize;
            mock.mem[..rl].copy_from_slice(&rec[..rl]);

            let mut s: S<8> = FlashStore::new(mock, 0, 512);
            assert_eq!(s.load().await, Ok(Some(1)));
            // Next save wraps `u32::MAX -> 0` and appends into region 0.
            s.save(&2).await.unwrap();
            let mut s: S<8> = FlashStore::new(s.into_inner(), 0, 512);
            assert_eq!(s.load().await, Ok(Some(2)), "wraparound u32::MAX -> 0 must preserve durability");

            // Continue saving across region switch (`seq = 1..30`) and reboot after every save.
            for v in 3..=30u64 {
                s.save(&v).await.unwrap();
                let mut check: S<8> = FlashStore::new(
                    Mock { mem: s.flash.mem.clone(), budget: usize::MAX, erases: 0 },
                    0,
                    512,
                );
                assert_eq!(check.load().await, Ok(Some(v)));
            }
        });
    }

    #[test]
    fn wraparound_power_cuts_preserve_crash_consistency() {
        block_on(async {
            for cut in 0..=80usize {
                let mut mock = Mock::new(1024, usize::MAX);
                // Fill region 0 nearly to capacity ending at `seq = u32::MAX`, so the
                // next save wraps to `0` AND triggers an erase + region switch to region 1.
                let mut off = 0usize;
                let mut seq = u32::MAX - 20;
                let mut last_val = 0u64;
                while off + 24 <= 512 {
                    last_val += 1;
                    let mut rec = [0xFFu8; 24];
                    Wide::<8>::encode(&last_val, &mut rec[HDR..]).unwrap();
                    store_core::encode_record(&mut rec, seq, 1, 8, 4).unwrap();
                    mock.mem[off..off + 24].copy_from_slice(&rec);
                    off += 24;
                    seq = seq.wrapping_add(1);
                }
                // `last_val` was written at `seq = u32::MAX` (21 records: u32::MAX-20..=u32::MAX).
                let acked = last_val;
                let in_flight = last_val + 1;

                mock.budget = usize::MAX;
                let mut s: S<8> = FlashStore::new(mock, 0, 512);
                assert_eq!(s.load().await, Ok(Some(acked)));
                s.flash.budget = cut;
                let save_res = s.save(&in_flight).await;
                let mut reboot: S<8> = FlashStore::new(
                    Mock { mem: s.into_inner().mem, budget: usize::MAX, erases: 0 },
                    0,
                    512,
                );
                let recovered = reboot.load().await.unwrap();
                if save_res.is_ok() {
                    assert_eq!(recovered, Some(in_flight), "acked wraparound save lost at cut {cut}");
                } else {
                    assert!(
                        recovered == Some(acked) || recovered == Some(in_flight),
                        "crash inconsistency across wraparound at cut {cut}: recovered={recovered:?}, acked={acked}, in_flight={in_flight}"
                    );
                }
            }
        });
    }

    #[test]
    fn ambiguous_write_completion_append_does_not_duplicate_seq_or_lose_data() {
        struct AmbiguousFlash {
            inner: Mock,
            fail_after_next_write: bool,
        }
        impl ErrorType for AmbiguousFlash {
            type Error = Cut;
        }
        impl ReadNorFlash for AmbiguousFlash {
            const READ_SIZE: usize = Mock::READ_SIZE;
            async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Cut> {
                self.inner.read(offset, bytes).await
            }
            fn capacity(&self) -> usize {
                self.inner.capacity()
            }
        }
        impl NorFlash for AmbiguousFlash {
            const WRITE_SIZE: usize = Mock::WRITE_SIZE;
            const ERASE_SIZE: usize = Mock::ERASE_SIZE;
            async fn erase(&mut self, from: u32, to: u32) -> Result<(), Cut> {
                self.inner.erase(from, to).await
            }
            async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Cut> {
                // Physically program ALL bytes to flash first.
                self.inner.write(offset, bytes).await?;
                if self.fail_after_next_write {
                    self.fail_after_next_write = false;
                    return Err(Cut);
                }
                Ok(())
            }
        }

        block_on(async {
            let flash = AmbiguousFlash {
                inner: Mock::new(1024, usize::MAX),
                fail_after_next_write: false,
            };
            let mut s: FlashStore<AmbiguousFlash, K, Wide<8>, 256> = FlashStore::new(flash, 0, 512);
            // Step 1: Save X=10 cleanly into Region 0 (seq=1).
            s.save(&10).await.unwrap();
            // Step 2: Save Y=20 physically lands in Region 0 (seq=2), then returns Err.
            s.flash.fail_after_next_write = true;
            assert_eq!(s.save(&20).await, Err(StoreError::Flash(Cut)));
            // Step 3: Without reboot, save Z=30 succeeds.
            s.save(&30).await.unwrap();
            // Step 4: Reboot and recover: MUST recover acknowledged Z=30, NEVER Y=20.
            let mem = s.into_inner().inner.mem;
            let mut reboot: S<8> =
                FlashStore::new(Mock { mem, budget: usize::MAX, erases: 0 }, 0, 512);
            assert_eq!(
                reboot.load().await,
                Ok(Some(30)),
                "ambiguous write completion must not cause duplicate seq or lose subsequent acknowledged save"
            );
        });
    }

    #[test]
    fn ambiguous_write_completion_across_wraparound_and_switch_preserves_durability() {
        struct AmbiguousFlash {
            inner: Mock,
            fail_after_next_write: bool,
        }
        impl ErrorType for AmbiguousFlash {
            type Error = Cut;
        }
        impl ReadNorFlash for AmbiguousFlash {
            const READ_SIZE: usize = Mock::READ_SIZE;
            async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Cut> {
                self.inner.read(offset, bytes).await
            }
            fn capacity(&self) -> usize {
                self.inner.capacity()
            }
        }
        impl NorFlash for AmbiguousFlash {
            const WRITE_SIZE: usize = Mock::WRITE_SIZE;
            const ERASE_SIZE: usize = Mock::ERASE_SIZE;
            async fn erase(&mut self, from: u32, to: u32) -> Result<(), Cut> {
                self.inner.erase(from, to).await
            }
            async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Cut> {
                self.inner.write(offset, bytes).await?;
                if self.fail_after_next_write {
                    self.fail_after_next_write = false;
                    return Err(Cut);
                }
                Ok(())
            }
        }

        block_on(async {
            let mut mock = Mock::new(1024, usize::MAX);
            // Seed Region 0 with `seq = u32::MAX - 1`, payload `100`.
            let mut rec = [0xFFu8; 24];
            Wide::<8>::encode(&100u64, &mut rec[HDR..]).unwrap();
            let rl = store_core::encode_record(&mut rec, u32::MAX - 1, 1, 8, 4).unwrap() as usize;
            mock.mem[..rl].copy_from_slice(&rec[..rl]);

            let flash = AmbiguousFlash { inner: mock, fail_after_next_write: false };
            let mut s: FlashStore<AmbiguousFlash, K, Wide<8>, 256> = FlashStore::new(flash, 0, 512);
            assert_eq!(s.load().await, Ok(Some(100)));
            // Ambiguous write of `seq = u32::MAX` (200) lands in Region 0 then fails.
            s.flash.fail_after_next_write = true;
            assert_eq!(s.save(&200).await, Err(StoreError::Flash(Cut)));
            // Next save without reboot must advance to `seq = 0` (wraparound) and persist 300.
            s.save(&300).await.unwrap();
            let mut reboot: S<8> = FlashStore::new(
                Mock { mem: s.into_inner().inner.mem, budget: usize::MAX, erases: 0 },
                0,
                512,
            );
            assert_eq!(reboot.load().await, Ok(Some(300)));
        });
    }

    #[test]
    fn async_cancellation_after_write_lands_does_not_duplicate_seq_or_lose_data() {
        use core::future::Future;
        use core::pin::Pin;
        use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

        struct SuspendAfterWriteFlash {
            inner: Mock,
            suspend_next_write: bool,
        }
        impl ErrorType for SuspendAfterWriteFlash {
            type Error = Cut;
        }
        impl ReadNorFlash for SuspendAfterWriteFlash {
            const READ_SIZE: usize = Mock::READ_SIZE;
            async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Cut> {
                self.inner.read(offset, bytes).await
            }
            fn capacity(&self) -> usize {
                self.inner.capacity()
            }
        }
        impl NorFlash for SuspendAfterWriteFlash {
            const WRITE_SIZE: usize = Mock::WRITE_SIZE;
            const ERASE_SIZE: usize = Mock::ERASE_SIZE;
            async fn erase(&mut self, from: u32, to: u32) -> Result<(), Cut> {
                self.inner.erase(from, to).await
            }
            async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Cut> {
                // Physically write all bytes to flash immediately.
                self.inner.write(offset, bytes).await?;
                if self.suspend_next_write {
                    self.suspend_next_write = false;
                    // Suspend forever so the caller can drop/cancel the future after
                    // the record has already physically landed on flash.
                    core::future::pending::<()>().await;
                }
                Ok(())
            }
        }

        fn noop_waker() -> Waker {
            fn clone(_: *const ()) -> RawWaker {
                RawWaker::new(core::ptr::null(), &VTABLE)
            }
            fn noop(_: *const ()) {}
            static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
            // SAFETY: no-op vtable with null data pointer.
            unsafe { Waker::from_raw(RawWaker::new(core::ptr::null(), &VTABLE)) }
        }

        block_on(async {
            let flash = SuspendAfterWriteFlash {
                inner: Mock::new(1024, usize::MAX),
                suspend_next_write: false,
            };
            let mut s: FlashStore<SuspendAfterWriteFlash, K, Wide<8>, 256> =
                FlashStore::new(flash, 0, 512);
            // Step 1: Save X=10 cleanly into Region 0 (seq=1).
            s.save(&10).await.unwrap();
            // Step 2: Start save Y=20; poll once so (seq=2, Y=20) physically lands in Region 0,
            // then drop/cancel the future before `save` returns!
            s.flash.suspend_next_write = true;
            {
                let mut fut = core::pin::pin!(s.save(&20));
                let waker = noop_waker();
                let mut cx = Context::from_waker(&waker);
                assert!(matches!(Pin::as_mut(&mut fut).poll(&mut cx), Poll::Pending));
            }
            // Step 3: Without reboot, save Z=30 succeeds.
            s.save(&30).await.unwrap();
            // Step 4: Reboot and verify Z=30 is recovered (not Y=20).
            let mem = s.into_inner().inner.mem;
            let mut reboot: S<8> =
                FlashStore::new(Mock { mem, budget: usize::MAX, erases: 0 }, 0, 512);
            assert_eq!(
                reboot.load().await,
                Ok(Some(30)),
                "async cancellation after physical write completion must not lose subsequent acknowledged save"
            );
        });
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
