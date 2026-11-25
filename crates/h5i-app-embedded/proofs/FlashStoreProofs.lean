import H5iAppEmbedded
import H5iAppLib.Basic
import NorFlash

/-!
# Implementation-Level Formal Proofs for Production `FlashStore` (`store_core`)

Proves functional correctness, memory/storage safety, wraparound durability,
bounded recovery termination, and compositional crash consistency directly over
the **Aeneas-extracted production Rust implementation**
(`crates/h5i-app-embedded/src/store_core.rs` → `generated/H5iAppEmbedded.lean`),
composed with the reusable `NorFlash` hardware and power-failure specification
(`NorFlash.lean`).

## Scope & Explicit Assumptions
- **Mechanically proved here (`kernel_implementation_level` / `compositional`)**:
  1. Sequence-number wraparound safety of extracted `next_seq` & `seq_newer`
     (including `u32::MAX → 0` and strict ordering across any `< 2^31` window).
  2. Exact little-endian header encoding/decoding (`put_u16_le`/`get_u16_le`,
     `put_u32_le`/`get_u32_le`), panic freedom of CRC-32 and slice loops, and
     end-to-end `encode_record` → `parse_header` + `validate_record_crc`
     round-trip with payload preservation.
  3. Storage & memory safety of extracted `plan_save`, `scan_init`, `scan_step`:
     all flash addresses/lengths satisfy `NorFlash.{ReadValid, WriteValid, EraseValid}`,
     buffer bounds hold, standby erase NEVER touches the active region, and
     append writes NEVER overlap committed records.
  4. Bounded termination of extracted `scan_step` via a strictly decreasing
     natural-number measure.
  5. Compositional durability & crash consistency when executing extracted
     `encode_record` / `plan_save` / `pos_after_write` against `NorFlash.NorFlashStep`
     (including `eraseFailPartial`, `writeFailPartial`, ambiguous write failures,
     and repeated reboots).
- **Explicit trusted assumptions**:
  - **H1–H3 (`NorFlash.lean`)**: NOR flash bit-transition physics, range
    isolation on partial write/erase failures, reboot persistence, and
    non-interference of reads.
  - **C1 (CRC-32 collision freedom on torn records)**: a partially written or
    partially erased record does not spuriously pass `validate_record_crc` with
    `MAGIC` and a newer sequence number.
  - **C2 (`SnapshotCodec` round-trip)**: `C::decode(C::encode(s)) = some s`.
  - **D1 (Async I/O driver fidelity)**: `store.rs::FlashStore` executes the
    exact `ScanAction` and `SavePlan` operations returned by `store_core` in
    order (validated by unit, proptest, and wraparound/power-cut tests).
-/

open Aeneas Aeneas.Std Result h5i_app_embedded h5i_app_embedded.store_core

namespace h5i_app_embedded.FlashStoreProofs

/-! ## 1. Constants & Geometry Helpers -/

theorem magic_val : MAGIC.val = 0x53354948 := by unfold MAGIC; rfl
theorem hdr_val : HDR.val = 16 := by unfold HDR; rfl
theorem hdr_u32_val : HDR_U32.val = 16 := by unfold HDR_U32; rfl
theorem max_payload_val : MAX_PAYLOAD_LEN.val = 65535 := by unfold MAX_PAYLOAD_LEN; rfl
theorem max_region_val : MAX_REGION_BYTES.val = 0x3FFFFFFF := by unfold MAX_REGION_BYTES; rfl

/-- Project `store_core.FlashGeometry` to the reusable `NorFlash.FlashConfig`. -/
def toFlashConfig (g : FlashGeometry) : NorFlash.FlashConfig where
  capacity := g.capacity.val
  readSize := g.read_size.val
  writeSize := g.write_size.val
  eraseSize := g.erase_size.val

/-- Natural-number starting flash address of region `r` (`0` or `1`). -/
def regionStartNat (g : FlashGeometry) (r : Nat) : Nat :=
  if r = 0 then g.base.val else g.base.val + g.region.val

private theorem u32_val_lt (x : U32) : x.val < 4294967296 := by scalar_tac
private theorem u16_val_lt (x : U16) : x.val < 65536 := by scalar_tac
private theorem u8_val_lt (x : U8) : x.val < 256 := by scalar_tac

private theorem u32_add_ok {a b : U32} (h : a.val + b.val ≤ U32.max) :
    ∃ c : U32, a + b = ok c ∧ c.val = a.val + b.val := by
  have hs : a + b ⦃ c => c.val = a.val + b.val ⦄ := by step*
  exact H5iAppLib.spec_exists_ok hs

private theorem u32_sub_ok {a b : U32} (h : b.val ≤ a.val) :
    ∃ c : U32, a - b = ok c ∧ c.val = a.val - b.val := by
  have hs : a - b ⦃ c => c.val = a.val - b.val ⦄ := by step*
  exact H5iAppLib.spec_exists_ok hs

private theorem u32_rem_ok {a b : U32} (h : b.val ≠ 0) :
    ∃ c : U32, a % b = ok c ∧ c.val = a.val % b.val := by
  have hs : a % b ⦃ c => c.val = a.val % b.val ⦄ := by step*
  exact H5iAppLib.spec_exists_ok hs

private theorem usize_add_ok {a b : Usize} (h : a.val + b.val ≤ Usize.max) :
    ∃ c : Usize, a + b = ok c ∧ c.val = a.val + b.val := by
  have hs : a + b ⦃ c => c.val = a.val + b.val ⦄ := by step*
  exact H5iAppLib.spec_exists_ok hs

private theorem slice_val_length (buf : Slice U8) : buf.val.length = buf.length := by
  simp [Slice.length]

/-! ## 2. Sequence Number Management & Wraparound Correctness -/

@[step]
theorem next_seq_spec (s : U32) :
    next_seq s ⦃ s' => s' = core.num.U32.wrapping_add s 1#u32 ∧
      s'.val = (s.val + 1) % 4294967296 ⦄ := by
  unfold next_seq
  step*
  let one : U32 := 1#u32
  have hbv : (core.num.U32.wrapping_add s one).bv = s.bv + one.bv := rfl
  change (core.num.U32.wrapping_add s one).bv.toNat = (s.bv.toNat + 1) % 4294967296
  rw [hbv, BitVec.toNat_add]
  rfl

@[step]
theorem seq_newer_spec (cand cur : U32) :
    seq_newer cand cur ⦃ b =>
      let d := (cand.val + 4294967296 - cur.val) % 4294967296
      b = decide (d ≠ 0 ∧ d < 2147483648) ⦄ := by
  unfold seq_newer
  have hsub : (core.num.U32.wrapping_sub cand cur).val =
      (cand.val + 4294967296 - cur.val) % 4294967296 := by
    have hbv : (core.num.U32.wrapping_sub cand cur).bv = cand.bv - cur.bv := rfl
    change (core.num.U32.wrapping_sub cand cur).bv.toNat =
      (cand.val + 4294967296 - cur.val) % 4294967296
    rw [hbv, BitVec.toNat_sub]
    have hc := u32_val_lt cand
    have hk := u32_val_lt cur
    change (4294967296 - cur.val + cand.val) % 4294967296 =
      (cand.val + 4294967296 - cur.val) % 4294967296
    congr 1; omega
  step*
  rename_i hne0
  have hne : d.val ≠ 0 := by
    intro h0
    have : d = 0#u32 := UScalar.eq_of_val_eq (by simpa using h0)
    simp [this] at hne0
  rw [d_post, hsub] at hne
  rw [d_post]
  have hlt_iff : (core.num.U32.wrapping_sub cand cur < 2147483648#u32) ↔
      (cand.val + 4294967296 - cur.val) % 4294967296 < 2147483648 := by
    simp [UScalar.lt_equiv, hsub]
  by_cases hlt : (cand.val + 4294967296 - cur.val) % 4294967296 < 2147483648
  · rw [decide_eq_true (hlt_iff.mpr hlt), decide_eq_true ⟨hne, hlt⟩]
  · rw [decide_eq_false (fun h => hlt (hlt_iff.mp h)), decide_eq_false (fun h => hlt h.2)]

/-- **Wraparound-safe successor**: for EVERY `s : U32` (including `u32::MAX`),
`next_seq s` is strictly newer than `s` according to extracted `seq_newer`. -/
theorem seq_newer_next_seq (s : U32) :
    ∃ s', next_seq s = ok s' ∧ seq_newer s' s = ok true := by
  obtain ⟨s', hns⟩ := H5iAppLib.ok_of (next_seq_spec s)
  have hns_p := H5iAppLib.post_of_ok (next_seq_spec s) hns
  obtain ⟨b, hsn⟩ := H5iAppLib.ok_of (seq_newer_spec s' s)
  have hsn_p := H5iAppLib.post_of_ok (seq_newer_spec s' s) hsn
  have hs := u32_val_lt s
  have hd : (s'.val + 4294967296 - s.val) % 4294967296 = 1 := by
    rw [hns_p.2]
    omega
  have hb : b = true := by
    rw [hsn_p, hd]
    rfl
  subst hb
  exact ⟨s', hns, hsn⟩

/-- Concrete regression witness at the `u32::MAX → 0` boundary. -/
theorem seq_newer_wraparound_boundary :
    seq_newer 0#u32 4294967295#u32 = ok true ∧
    seq_newer 4294967295#u32 0#u32 = ok false := by
  constructor
  · obtain ⟨b, h⟩ := H5iAppLib.ok_of (seq_newer_spec 0#u32 4294967295#u32)
    have hp := H5iAppLib.post_of_ok (seq_newer_spec 0#u32 4294967295#u32) h
    have hb : b = true := by simpa using hp
    rwa [hb] at h
  · obtain ⟨b, h⟩ := H5iAppLib.ok_of (seq_newer_spec 4294967295#u32 0#u32)
    have hp := H5iAppLib.post_of_ok (seq_newer_spec 4294967295#u32 0#u32) h
    have hb : b = false := by simpa using hp
    rwa [hb] at h

/-- **Modular half-space total ordering**: for ANY base sequence `base` and any
two records written at logical offsets `a, b < 2^31` from `base`, extracted
`seq_newer` agrees strictly with `b < a` — even across `u32::MAX → 0`. -/
theorem seq_newer_modular_window (base a b : Nat) (c₁ c₂ : U32)
    (ha : a < 2147483648) (hb : b < 2147483648)
    (hc₁ : c₁.val = (base + a) % 4294967296) (hc₂ : c₂.val = (base + b) % 4294967296) :
    seq_newer c₁ c₂ = ok (decide (b < a)) := by
  obtain ⟨r, hr⟩ := H5iAppLib.ok_of (seq_newer_spec c₁ c₂)
  have hp := H5iAppLib.post_of_ok (seq_newer_spec c₁ c₂) hr
  have hmod : (c₁.val + 4294967296 - c₂.val) % 4294967296 = (a + 4294967296 - b) % 4294967296 := by
    rw [hc₁, hc₂]
    have hbase : base = 4294967296 * (base / 4294967296) + base % 4294967296 :=
      (Nat.div_add_mod base 4294967296).symm
    set r0 := base % 4294967296
    have hr0 : r0 < 4294967296 := Nat.mod_lt _ (by decide)
    have e1 : (base + a) % 4294967296 = (r0 + a) % 4294967296 := by
      conv_lhs => rw [hbase]
      rw [Nat.add_assoc, Nat.mul_add_mod]
    have e2 : (base + b) % 4294967296 = (r0 + b) % 4294967296 := by
      conv_lhs => rw [hbase]
      rw [Nat.add_assoc, Nat.mul_add_mod]
    rw [e1, e2]
    omega
  have hr_eq : r = decide (b < a) := by
    rw [hp, hmod]
    by_cases hba : b < a
    · have : (a + 4294967296 - b) % 4294967296 = a - b := by omega
      rw [this, decide_eq_true hba, decide_eq_true (by omega)]
    · by_cases hab : a = b
      · subst hab
        have : (a + 4294967296 - a) % 4294967296 = 0 := by omega
        rw [this, decide_eq_false hba]
        rfl
      · have : (a + 4294967296 - b) % 4294967296 = 4294967296 - (b - a) := by omega
        rw [this, decide_eq_false hba, decide_eq_false (by omega)]
  rwa [hr_eq] at hr

theorem seq_newer_irrefl (s : U32) : seq_newer s s = ok false := by
  obtain ⟨r, hr⟩ := H5iAppLib.ok_of (seq_newer_spec s s)
  have hp := H5iAppLib.post_of_ok (seq_newer_spec s s) hr
  have hs := u32_val_lt s
  have h0 : (s.val + 4294967296 - s.val) % 4294967296 = 0 := by omega
  have hr_eq : r = false := by rw [hp, h0]; rfl
  rwa [hr_eq] at hr

theorem seq_newer_asymm (a b : U32) (h : seq_newer a b = ok true) :
    seq_newer b a = ok false := by
  have hp := H5iAppLib.post_of_ok (seq_newer_spec a b) h
  obtain ⟨r, hr⟩ := H5iAppLib.ok_of (seq_newer_spec b a)
  have hp2 := H5iAppLib.post_of_ok (seq_newer_spec b a) hr
  have ha := u32_val_lt a
  have hb := u32_val_lt b
  have hd1 : (a.val + 4294967296 - b.val) % 4294967296 ≠ 0 ∧
      (a.val + 4294967296 - b.val) % 4294967296 < 2147483648 := by
    exact of_decide_eq_true hp.symm
  have hge : ¬ ((b.val + 4294967296 - a.val) % 4294967296 < 2147483648) := by omega
  have hr_eq : r = false := by
    rw [hp2, decide_eq_false (fun h' => hge h'.2)]
  rwa [hr_eq] at hr

/-! ## 3. Geometry, Alignment & Bounds -/

@[step]
theorem align_eq_max (w r : U32) :
    align w r ⦃ a => a.val = max (max w.val r.val) 4 ⦄ := by
  unfold align
  by_cases h1 : w > r
  · simp only [h1, ↓reduceIte, bind_ok]
    step* <;> (simp only [Nat.max_def]; split <;> (try split) <;> scalar_tac)
  · simp only [h1, ↓reduceIte, bind_ok]
    step* <;> (simp only [Nat.max_def]; split <;> (try split) <;> scalar_tac)

theorem align_spec (w r : U32) :
    align w r ⦃ a => 4 ≤ a.val ∧ w.val ≤ a.val ∧ r.val ≤ a.val ∧
      (a.val = w.val ∨ a.val = r.val ∨ a.val = 4) ⦄ := by
  refine WP.spec_mono (align_eq_max w r) (fun a ha => ?_)
  rw [ha]
  simp only [Nat.max_def]
  split <;> split <;> omega

@[step]
theorem record_len_spec (len a : U32) :
    record_len len a ⦃ o =>
      (∀ rl, o = some rl →
        len.val ≤ 65535 ∧ 0 < a.val ∧ a.val ≤ 16 ∧
        16 + len.val ≤ rl.val ∧ rl.val % a.val = 0 ∧ rl.val ≤ 65566) ∧
      (len.val ≤ 65535 → 0 < a.val → a.val ≤ 16 → ∃ rl, o = some rl) ⦄ := by
  unfold record_len
  have := max_payload_val
  have := hdr_u32_val
  step*
  · refine ⟨fun rl h => by cases h; refine ⟨by scalar_tac, by scalar_tac, by scalar_tac, by scalar_tac, by scalar_tac, by scalar_tac⟩,
            fun _ _ _ => ⟨_, rfl⟩⟩
  · refine ⟨fun rl h => by
      cases h
      have hrem : total.val % a.val < a.val := Nat.mod_lt _ (by scalar_tac)
      have hmod : i1.val % a.val = 0 := by
        have h1 : i1.val = total.val + (a.val - total.val % a.val) := by scalar_tac
        rw [h1]
        have hdiv := Nat.div_add_mod total.val a.val
        have h2 : total.val + (a.val - total.val % a.val) = a.val * (total.val / a.val + 1) := by
          have : a.val * (total.val / a.val + 1) = a.val * (total.val / a.val) + a.val := Nat.mul_succ _ _
          omega
        rw [h2, Nat.mul_mod_right]
      exact ⟨by scalar_tac, by scalar_tac, by scalar_tac, by scalar_tac, hmod, by scalar_tac⟩,
      fun _ _ _ => ⟨_, rfl⟩⟩

/-- Key structural consequences of extracted `layout_ok g = ok true`. -/
structure LayoutProps (g : FlashGeometry) : Prop where
  read_pos : 0 < g.read_size.val
  write_pos : 0 < g.write_size.val
  erase_pos : 0 < g.erase_size.val
  align_ge4 : 4 ≤ max (max g.write_size.val g.read_size.val) 4
  align_le16 : max (max g.write_size.val g.read_size.val) 4 ≤ 16
  hdr_aligned : 16 % max (max g.write_size.val g.read_size.val) 4 = 0
  region_ge16 : 16 ≤ g.region.val
  region_le_max : g.region.val ≤ 0x3FFFFFFF
  region_erase_aligned : g.region.val % g.erase_size.val = 0
  region_aligned : g.region.val % max (max g.write_size.val g.read_size.val) 4 = 0
  base_erase_aligned : g.base.val % g.erase_size.val = 0
  base_aligned : g.base.val % max (max g.write_size.val g.read_size.val) 4 = 0
  in_capacity : g.base.val + 2 * g.region.val ≤ g.capacity.val
  buf_ge16 : 16 ≤ g.buf_cap.val
  buf_aligned : g.buf_cap.val % max (max g.write_size.val g.read_size.val) 4 = 0

theorem layout_ok_spec (g : FlashGeometry) :
    layout_ok g ⦃ b => b = true → LayoutProps g ⦄ := by
  unfold layout_ok
  have := hdr_u32_val
  have := max_region_val
  step*
  all_goals (try (intro hf; cases hf; done))
  intro _
  have ha : a.val = max (max g.write_size.val g.read_size.val) 4 := a_post
  have hi7 : i7.val = 0 := by
    scalar_tac
  refine
    { read_pos := by scalar_tac
      write_pos := by scalar_tac
      erase_pos := by scalar_tac
      align_ge4 := by omega
      align_le16 := by scalar_tac
      hdr_aligned := by rw [← ha]; scalar_tac
      region_ge16 := by scalar_tac
      region_le_max := by scalar_tac
      region_erase_aligned := by scalar_tac
      region_aligned := by rw [← ha]; scalar_tac
      base_erase_aligned := by scalar_tac
      base_aligned := by rw [← ha]; scalar_tac
      in_capacity := by scalar_tac
      buf_ge16 := by scalar_tac
      buf_aligned := by rw [← ha]; scalar_tac }

/-- `layout_ok` guarantees the two regions `[base, base + region)` and
`[base + region, base + 2 * region)` are disjoint, fit inside `capacity`, and
their combined record count fits strictly inside the `< 2^31` modular window. -/
theorem layout_ok_regions_disjoint_and_window (g : FlashGeometry)
    (hok : layout_ok g = ok true) :
    NorFlash.disjoint g.base.val g.region.val (g.base.val + g.region.val) g.region.val ∧
    g.base.val + 2 * g.region.val ≤ g.capacity.val ∧
    2 * (g.region.val / 16) < 2147483648 := by
  have hp := H5iAppLib.post_of_ok (layout_ok_spec g) hok rfl
  refine ⟨Or.inl (Nat.le_refl _), hp.in_capacity, ?_⟩
  have := hp.region_le_max
  omega

/-- Power-of-two NOR flash alignment divisibility: `write_size` and `read_size`
both divide `a = max(max(write_size, read_size), 4)`. -/
def AlignDivides (g : FlashGeometry) : Prop :=
  let a := max (max g.write_size.val g.read_size.val) 4
  a % g.write_size.val = 0 ∧ a % g.read_size.val = 0

private theorem mod_zero_of_dvd_mod_zero {x a d : Nat}
    (hxa : x % a = 0) (had : a % d = 0) : x % d = 0 := by
  have h1 : a ∣ x := Nat.dvd_of_mod_eq_zero hxa
  have h2 : d ∣ a := Nat.dvd_of_mod_eq_zero had
  exact Nat.mod_eq_zero_of_dvd (Nat.dvd_trans h2 h1)

@[step]
theorem region_start_spec (g : FlashGeometry) (r : U32)
    (hcap : g.base.val + g.region.val ≤ U32.max) :
    region_start g r ⦃ addr => addr.val = regionStartNat g r.val ⦄ := by
  unfold region_start regionStartNat
  step*
  all_goals (try (simp [show r.val = 0 by scalar_tac]; done))
  simp [show r.val ≠ 0 by scalar_tac]; scalar_tac

@[step]
theorem other_region_spec (r : U32) :
    other_region r ⦃ r' => r'.val ≤ 1 ∧ (r.val ≤ 1 → r'.val ≠ r.val ∧ r'.val = 1 - r.val) ⦄ := by
  unfold other_region
  step*
  all_goals refine ⟨by scalar_tac, fun _ => ⟨by scalar_tac, by scalar_tac⟩⟩

/-! ## 4. Little-Endian Encoding/Decoding, CRC-32 & Record Framing -/

private theorem two_mul_add_or (a b r : Nat) (hr : r ≤ 1) :
    ((2 * a + r) ||| (2 * b)) = 2 * (a ||| b) + r := by
  apply Nat.eq_of_testBit_eq
  intro i
  cases i with
  | zero =>
    rw [Nat.testBit_or, Nat.testBit_zero, Nat.testBit_zero, Nat.testBit_zero]
    have h1 : (2 * a + r) % 2 = r := by omega
    have h2 : (2 * b) % 2 = 0 := by omega
    have h3 : (2 * (a ||| b) + r) % 2 = r := by omega
    rw [h1, h2, h3]
    simp
  | succ j =>
    rw [Nat.testBit_or, Nat.testBit_succ, Nat.testBit_succ, Nat.testBit_succ]
    have h1 : (2 * a + r) / 2 = a := by omega
    have h2 : (2 * b) / 2 = b := by omega
    have h3 : (2 * (a ||| b) + r) / 2 = a ||| b := by omega
    rw [h1, h2, h3, Nat.testBit_or]

private theorem nat_or_shift_add {x y k : Nat} (hx : x < 2^k) :
    (x ||| (y <<< k)) = x + y * 2^k := by
  induction k generalizing x with
  | zero =>
    have hx0 : x = 0 := by omega
    subst hx0; simp
  | succ k ih =>
    have hx_split : x = 2 * (x / 2) + x % 2 := by omega
    have hsh : y <<< (k + 1) = 2 * (y <<< k) := by
      rw [Nat.shiftLeft_eq, Nat.shiftLeft_eq, Nat.pow_succ]
      rw [Nat.mul_comm (2^k) 2, ← Nat.mul_assoc, Nat.mul_comm y 2, Nat.mul_assoc]
    conv_lhs => rw [hx_split, hsh]
    rw [two_mul_add_or (x / 2) (y <<< k) (x % 2) (by omega)]
    have hx_div : x / 2 < 2^k := by
      have : 2^(k + 1) = 2 * 2^k := by rw [Nat.pow_succ]; omega
      omega
    rw [ih hx_div, Nat.pow_succ]
    have hmul : y * (2^k * 2) = 2 * (y * 2^k) := by
      rw [Nat.mul_comm (2^k) 2, ← Nat.mul_assoc, Nat.mul_comm y 2, Nat.mul_assoc]
    omega

@[step]
theorem put_u16_le_spec (buf : Slice U8) (off : Usize) (v : U16)
    (hbound : off.val + 2 ≤ buf.length) :
    put_u16_le buf off v ⦃ res =>
      res.1 = true ∧
      res.2.length = buf.length ∧
      (∀ i, i < off.val ∨ off.val + 2 ≤ i → res.2.val[i]? = buf.val[i]?) ∧
      (∀ b, res.2.val[off.val]? = some b → b.val = v.val % 256) ∧
      (∀ b, res.2.val[off.val + 1]? = some b → b.val = (v.val / 256) % 256) ⦄ := by
  unfold put_u16_le
  step*
  all_goals try scalar_tac
  refine ⟨by simp [*], ?_, ?_, ?_⟩
  · intro i hout
    have hi1 : off.val ≠ i := by omega
    have hi2 : off.val + 1 ≠ i := by omega
    simp [buf2_post, buf1_post, List.getElem?_set_ne, hi1, hi2, i6_post]
  · intro b hb
    have hne : off.val + 1 ≠ off.val := by omega
    have hlen : off.val < buf.val.length := by rw [slice_val_length]; omega
    simp [buf2_post, buf1_post, i6_post, List.getElem?_set_ne hne,
      List.getElem?_set_self hlen] at hb
    subst hb
    have h1 : i2.val = v.val &&& 255 := by
      have := congrArg BitVec.toNat i2_post1
      simpa [BitVec.toNat_and] using this
    have h2 : i3.val = i2.val % 256 := by simp [i3_post]
    rw [h2, h1, Nat.and_two_pow_sub_one_eq_mod v.val 8, Nat.mod_mod]
  · intro b hb
    have hlen1 : buf1.length = buf.length := by simp [buf1_post]
    have hlen : off.val + 1 < buf1.val.length := by rw [slice_val_length]; omega
    simp [buf2_post, i6_post, List.getElem?_set_self hlen] at hb
    subst hb
    have h4 : i4.val = v.val / 256 := by simp [i4_post, Nat.shiftRight_eq_div_pow]
    have h5 : i5.val = i4.val &&& 255 := by
      have := congrArg BitVec.toNat i5_post1
      simpa [BitVec.toNat_and] using this
    have h7 : i7.val = i5.val % 256 := by simp [i7_post]
    rw [h7, h5, h4, Nat.and_two_pow_sub_one_eq_mod (v.val / 256) 8, Nat.mod_mod]

@[step]
theorem get_u16_le_spec (buf : Slice U8) (off : Usize) (hbound : off.val + 2 ≤ buf.length) :
    get_u16_le buf off ⦃ o =>
      ∃ v b0 b1, o = some v ∧
        buf.val[off.val]? = some b0 ∧
        buf.val[off.val + 1]? = some b1 ∧
        v.val = b0.val + 256 * b1.val ⦄ := by
  unfold get_u16_le
  step*
  all_goals try scalar_tac
  have hlen0 : off.val < buf.val.length := by rw [slice_val_length]; omega
  have hlen1 : off.val + 1 < buf.val.length := by rw [slice_val_length]; omega
  refine ⟨i6, buf.val[off.val]'hlen0, buf.val[off.val + 1]'hlen1, rfl,
    List.getElem?_eq_getElem hlen0, List.getElem?_eq_getElem hlen1, ?_⟩
  have hb0 : b0.val = (buf.val[off.val]'hlen0).val := by simp [b0_post, i2_post]
  have hb1 : b1.val = (buf.val[off.val + 1]'hlen1).val := by simp [b1_post, i4_post, i3_post]
  have l0 : b0.val < 256 := by rw [hb0]; exact u8_val_lt _
  have l1 : b1.val < 256 := by rw [hb1]; exact u8_val_lt _
  have hsz : U16.size = 65536 := by scalar_tac
  have hi5 : i5.val = b1.val <<< 8 := by
    have hsh : i5.val = (b1.val * 256) % 65536 := by simp [i5_post, Nat.shiftLeft_eq, hsz]
    rw [hsh, Nat.mod_eq_of_lt (by omega), Nat.shiftLeft_eq]
  have hi6 : i6.val = b0.val ||| i5.val := by
    have := congrArg BitVec.toNat i6_post1
    simpa [BitVec.toNat_or] using this
  rw [hi6, hi5, nat_or_shift_add (k := 8) (by omega), hb0, hb1]
  omega

/-- Exact little-endian round-trip for `u16` (`put_u16_le` followed by `get_u16_le`). -/
theorem put_get_u16_le_roundtrip (buf : Slice U8) (off : Usize) (v : U16)
    (hbound : off.val + 2 ≤ buf.length) :
    ∃ buf', put_u16_le buf off v = ok (true, buf') ∧ get_u16_le buf' off = ok (some v) := by
  obtain ⟨res, hput⟩ := H5iAppLib.ok_of (put_u16_le_spec buf off v hbound)
  have hp := H5iAppLib.post_of_ok (put_u16_le_spec buf off v hbound) hput
  obtain ⟨ok_b, buf'⟩ := res
  have htrue : ok_b = true := hp.1
  subst htrue
  have hlen : buf'.length = buf.length := hp.2.1
  obtain ⟨o, hget⟩ := H5iAppLib.ok_of (get_u16_le_spec buf' off (by omega))
  obtain ⟨v', b0, b1, ho, hb0, hb1, hv'⟩ := H5iAppLib.post_of_ok (get_u16_le_spec buf' off (by omega)) hget
  subst ho
  have hv_eq : v' = v := by
    apply UScalar.eq_of_val_eq
    rw [hv', hp.2.2.2.1 b0 hb0, hp.2.2.2.2 b1 hb1]
    have hv := u16_val_lt v
    omega
  subst hv_eq
  exact ⟨buf', hput, hget⟩

@[step]
theorem put_u32_le_spec (buf : Slice U8) (off : Usize) (v : U32)
    (hbound : off.val + 4 ≤ buf.length) :
    put_u32_le buf off v ⦃ res =>
      res.1 = true ∧
      res.2.length = buf.length ∧
      (∀ i, i < off.val ∨ off.val + 4 ≤ i → res.2.val[i]? = buf.val[i]?) ∧
      (∀ b, res.2.val[off.val]? = some b → b.val = v.val % 256) ∧
      (∀ b, res.2.val[off.val + 1]? = some b → b.val = (v.val / 256) % 256) ∧
      (∀ b, res.2.val[off.val + 2]? = some b → b.val = (v.val / 65536) % 256) ∧
      (∀ b, res.2.val[off.val + 3]? = some b → b.val = (v.val / 16777216) % 256) ⦄ := by
  unfold put_u32_le
  step*
  all_goals try scalar_tac
  refine ⟨by simp [*], ?_, ?_, ?_, ?_, ?_⟩
  · intro i hout
    have h0 : off.val ≠ i := by omega
    have h1 : off.val + 1 ≠ i := by omega
    have h2 : off.val + 2 ≠ i := by omega
    have h3 : off.val + 3 ≠ i := by omega
    simp [buf4_post, buf3_post, buf2_post, buf1_post, List.getElem?_set_ne, h0, h1, h2, h3, i6_post, i10_post, i14_post]
  · intro b hb
    have h1 : off.val + 1 ≠ off.val := by omega
    have h2 : off.val + 2 ≠ off.val := by omega
    have h3 : off.val + 3 ≠ off.val := by omega
    have hlen : off.val < buf.val.length := by rw [slice_val_length]; omega
    simp [buf4_post, buf3_post, buf2_post, buf1_post, i6_post, i10_post, i14_post,
      List.getElem?_set_ne h3, List.getElem?_set_ne h2, List.getElem?_set_ne h1,
      List.getElem?_set_self hlen] at hb
    subst hb
    have ha : i2.val = v.val &&& 255 := by
      have := congrArg BitVec.toNat i2_post1
      simpa [BitVec.toNat_and] using this
    have hc : i3.val = i2.val % 256 := by simp [i3_post]
    rw [hc, ha, Nat.and_two_pow_sub_one_eq_mod v.val 8, Nat.mod_mod]
  · intro b hb
    have h2 : off.val + 2 ≠ off.val + 1 := by omega
    have h3 : off.val + 3 ≠ off.val + 1 := by omega
    have hlen1 : buf1.length = buf.length := by simp [buf1_post]
    have hlen : off.val + 1 < buf1.val.length := by rw [slice_val_length]; omega
    simp [buf4_post, buf3_post, buf2_post, i6_post, i10_post, i14_post,
      List.getElem?_set_ne h3, List.getElem?_set_ne h2,
      List.getElem?_set_self hlen] at hb
    subst hb
    have ha : i4.val = v.val / 256 := by simp [i4_post, Nat.shiftRight_eq_div_pow]
    have hb' : i5.val = i4.val &&& 255 := by
      have := congrArg BitVec.toNat i5_post1
      simpa [BitVec.toNat_and] using this
    have hc : i7.val = i5.val % 256 := by simp [i7_post]
    rw [hc, hb', ha, Nat.and_two_pow_sub_one_eq_mod (v.val / 256) 8, Nat.mod_mod]
  · intro b hb
    have h3 : off.val + 3 ≠ off.val + 2 := by omega
    have hlen2 : buf2.length = buf.length := by simp [buf2_post, buf1_post]
    have hlen : off.val + 2 < buf2.val.length := by rw [slice_val_length]; omega
    simp [buf4_post, buf3_post, i10_post, i14_post,
      List.getElem?_set_ne h3, List.getElem?_set_self hlen] at hb
    subst hb
    have ha : i8.val = v.val / 65536 := by simp [i8_post, Nat.shiftRight_eq_div_pow]
    have hb' : i9.val = i8.val &&& 255 := by
      have := congrArg BitVec.toNat i9_post1
      simpa [BitVec.toNat_and] using this
    have hc : i11.val = i9.val % 256 := by simp [i11_post]
    rw [hc, hb', ha, Nat.and_two_pow_sub_one_eq_mod (v.val / 65536) 8, Nat.mod_mod]
  · intro b hb
    have hlen3 : buf3.length = buf.length := by simp [buf3_post, buf2_post, buf1_post]
    have hlen : off.val + 3 < buf3.val.length := by rw [slice_val_length]; omega
    simp [buf4_post, i14_post, List.getElem?_set_self hlen] at hb
    subst hb
    have ha : i12.val = v.val / 16777216 := by simp [i12_post, Nat.shiftRight_eq_div_pow]
    have hb' : i13.val = i12.val &&& 255 := by
      have := congrArg BitVec.toNat i13_post1
      simpa [BitVec.toNat_and] using this
    have hc : i15.val = i13.val % 256 := by simp [i15_post]
    rw [hc, hb', ha, Nat.and_two_pow_sub_one_eq_mod (v.val / 16777216) 8, Nat.mod_mod]

@[step]
theorem get_u32_le_spec (buf : Slice U8) (off : Usize) (hbound : off.val + 4 ≤ buf.length) :
    get_u32_le buf off ⦃ o =>
      ∃ v b0 b1 b2 b3, o = some v ∧
        buf.val[off.val]? = some b0 ∧
        buf.val[off.val + 1]? = some b1 ∧
        buf.val[off.val + 2]? = some b2 ∧
        buf.val[off.val + 3]? = some b3 ∧
        v.val = b0.val + 256 * b1.val + 65536 * b2.val + 16777216 * b3.val ⦄ := by
  unfold get_u32_le
  step*
  all_goals try scalar_tac
  have hlen0 : off.val < buf.val.length := by rw [slice_val_length]; omega
  have hlen1 : off.val + 1 < buf.val.length := by rw [slice_val_length]; omega
  have hlen2 : off.val + 2 < buf.val.length := by rw [slice_val_length]; omega
  have hlen3 : off.val + 3 < buf.val.length := by rw [slice_val_length]; omega
  refine ⟨i14, buf.val[off.val]'hlen0, buf.val[off.val + 1]'hlen1,
    buf.val[off.val + 2]'hlen2, buf.val[off.val + 3]'hlen3, rfl,
    List.getElem?_eq_getElem hlen0, List.getElem?_eq_getElem hlen1,
    List.getElem?_eq_getElem hlen2, List.getElem?_eq_getElem hlen3, ?_⟩
  have hb0 : b0.val = (buf.val[off.val]'hlen0).val := by simp [b0_post, i2_post]
  have hb1 : b1.val = (buf.val[off.val + 1]'hlen1).val := by simp [b1_post, i4_post, i3_post]
  have hb2 : b2.val = (buf.val[off.val + 2]'hlen2).val := by simp [b2_post, i6_post, i5_post]
  have hb3 : b3.val = (buf.val[off.val + 3]'hlen3).val := by simp [b3_post, i8_post, i7_post]
  have l0 : b0.val < 256 := by rw [hb0]; exact u8_val_lt _
  have l1 : b1.val < 256 := by rw [hb1]; exact u8_val_lt _
  have l2 : b2.val < 256 := by rw [hb2]; exact u8_val_lt _
  have l3 : b3.val < 256 := by rw [hb3]; exact u8_val_lt _
  have hsz : U32.size = 4294967296 := by scalar_tac
  have hi9 : i9.val = b1.val <<< 8 := by
    have h : i9.val = (b1.val * 256) % 4294967296 := by simp [i9_post, Nat.shiftLeft_eq, hsz]
    rw [h, Nat.mod_eq_of_lt (by omega), Nat.shiftLeft_eq]
  have hi10 : i10.val = b0.val + b1.val * 256 := by
    have h : i10.val = b0.val ||| i9.val := by
      have := congrArg BitVec.toNat i10_post1
      simpa [BitVec.toNat_or] using this
    rw [h, hi9, nat_or_shift_add (k := 8) (by omega)]
    rfl
  have hi11 : i11.val = b2.val <<< 16 := by
    have h : i11.val = (b2.val * 65536) % 4294967296 := by simp [i11_post, Nat.shiftLeft_eq, hsz]
    rw [h, Nat.mod_eq_of_lt (by omega), Nat.shiftLeft_eq]
  have hi12 : i12.val = i10.val + b2.val * 65536 := by
    have h : i12.val = i10.val ||| i11.val := by
      have := congrArg BitVec.toNat i12_post1
      simpa [BitVec.toNat_or] using this
    rw [h, hi11, nat_or_shift_add (k := 16) (by omega)]
    rfl
  have hi13 : i13.val = b3.val <<< 24 := by
    have h : i13.val = (b3.val * 16777216) % 4294967296 := by simp [i13_post, Nat.shiftLeft_eq, hsz]
    rw [h, Nat.mod_eq_of_lt (by omega), Nat.shiftLeft_eq]
  have hi14 : i14.val = i12.val + b3.val * 16777216 := by
    have h : i14.val = i12.val ||| i13.val := by
      have := congrArg BitVec.toNat i14_post1
      simpa [BitVec.toNat_or] using this
    rw [h, hi13, nat_or_shift_add (k := 24) (by omega)]
    rfl
  rw [hi14, hi12, hi10, hb0, hb1, hb2, hb3]
  omega

/-- Exact little-endian round-trip for `u32` (`put_u32_le` followed by `get_u32_le`). -/
theorem put_get_u32_le_roundtrip (buf : Slice U8) (off : Usize) (v : U32)
    (hbound : off.val + 4 ≤ buf.length) :
    ∃ buf', put_u32_le buf off v = ok (true, buf') ∧ get_u32_le buf' off = ok (some v) := by
  obtain ⟨res, hput⟩ := H5iAppLib.ok_of (put_u32_le_spec buf off v hbound)
  have hp := H5iAppLib.post_of_ok (put_u32_le_spec buf off v hbound) hput
  obtain ⟨ok_b, buf'⟩ := res
  have htrue : ok_b = true := hp.1
  subst htrue
  have hlen : buf'.length = buf.length := hp.2.1
  obtain ⟨o, hget⟩ := H5iAppLib.ok_of (get_u32_le_spec buf' off (by omega))
  obtain ⟨v', b0, b1, b2, b3, ho, hb0, hb1, hb2, hb3, hv'⟩ :=
    H5iAppLib.post_of_ok (get_u32_le_spec buf' off (by omega)) hget
  subst ho
  have hv_eq : v' = v := by
    apply UScalar.eq_of_val_eq
    rw [hv', hp.2.2.2.1 b0 hb0, hp.2.2.2.2.1 b1 hb1, hp.2.2.2.2.2.1 b2 hb2, hp.2.2.2.2.2.2 b3 hb3]
    have hv := u32_val_lt v
    omega
  subst hv_eq
  exact ⟨buf', hput, hget⟩

/-! ### Panic Freedom & Congruence of CRC-32 and Byte Loops -/

@[step]
theorem crc32_byte_spec (crc : U32) (b : U8) :
    crc32_byte crc b ⦃ _ => True ⦄ := by
  unfold crc32_byte crc32_byte_loop
  step*
  apply loop.spec_decr_nat (measure := fun p : U32 × U32 => 8 - p.2.val) (inv := fun p => p.2.val ≤ 8)
  · intro ⟨c1, bit1⟩ hinv
    unfold crc32_byte_loop.body
    step*
    by_cases h0 : (i != 0#u32) = true
    · simp only [h0, ↓reduceIte]
      step*
    · simp only [h0, Bool.false_eq_true, ↓reduceIte]
      step*
  · scalar_tac

@[step]
theorem crc32_range_spec (crc : U32) (buf : Slice U8) (start «end» : Usize) :
    crc32_range crc buf start «end» ⦃ _ => True ⦄ := by
  unfold crc32_range crc32_range_loop
  by_cases h : «end» ≤ buf.len
  · simp only [h, ↓reduceIte, bind_ok]
    apply loop.spec_decr_nat (measure := fun p : U32 × Usize => «end».val - p.2.val) (inv := fun _ => True)
    · intro ⟨c1, i1⟩ _
      unfold crc32_range_loop.body
      step*
    · trivial
  · simp only [h, ↓reduceIte, bind_ok]
    apply loop.spec_decr_nat (measure := fun p : U32 × Usize => buf.length - p.2.val) (inv := fun _ => True)
    · intro ⟨c1, i1⟩ _
      unfold crc32_range_loop.body
      step*
    · trivial

/-- Congruence: `crc32_range_loop` depends ONLY on bytes in `[i, limit)`. -/
theorem crc32_range_loop_congr (buf₁ buf₂ : Slice U8) (limit i : Usize) (crc : U32)
    (hl₁ : limit.val ≤ buf₁.length) (hl₂ : limit.val ≤ buf₂.length)
    (heq : ∀ j, i.val ≤ j → j < limit.val → buf₁.val[j]? = buf₂.val[j]?) :
    crc32_range_loop crc buf₁ limit i = crc32_range_loop crc buf₂ limit i := by
  have hmain : ∀ d (crc : U32) (i : Usize), limit.val - i.val = d →
      (∀ j, i.val ≤ j → j < limit.val → buf₁.val[j]? = buf₂.val[j]?) →
      crc32_range_loop crc buf₁ limit i = crc32_range_loop crc buf₂ limit i := by
    intro d
    induction d with
    | zero =>
      intro crc i hd _
      unfold crc32_range_loop
      conv_lhs => rw [loop]
      conv_rhs => rw [loop]
      unfold crc32_range_loop.body
      have hge : ¬ (i < limit) := by simp [UScalar.lt_equiv]; omega
      simp [hge]
    | succ k ih =>
      intro crc i hd heq'
      unfold crc32_range_loop
      conv_lhs => rw [loop]
      conv_rhs => rw [loop]
      unfold crc32_range_loop.body
      have hlt : i < limit := by simp [UScalar.lt_equiv]; omega
      have h1 : i.val < buf₁.val.length := by rw [slice_val_length]; omega
      have h2 : i.val < buf₂.val.length := by rw [slice_val_length]; omega
      have hbi : buf₁.val[i.val]'h1 = buf₂.val[i.val]'h2 := by
        have := heq' i.val (Nat.le_refl _) (by omega)
        rwa [List.getElem?_eq_getElem h1, List.getElem?_eq_getElem h2, Option.some.injEq] at this
      have hidx1 : Slice.index_usize buf₁ i = ok (buf₁.val[i.val]'h1) := by
        have hs : Slice.index_usize buf₁ i ⦃ x => x = buf₁.val[i.val]'h1 ⦄ := by step*
        exact H5iAppLib.eq_ok_of_spec hs
      have hidx2 : Slice.index_usize buf₂ i = ok (buf₂.val[i.val]'h2) := by
        have hs : Slice.index_usize buf₂ i ⦃ x => x = buf₂.val[i.val]'h2 ⦄ := by step*
        exact H5iAppLib.eq_ok_of_spec hs
      simp only [hlt, ↓reduceIte, hidx1, hidx2, hbi, bind_tc_ok, bind_ok]
      obtain ⟨c', hc'⟩ := H5iAppLib.ok_of (crc32_byte_spec crc (buf₂.val[i.val]'h2))
      simp only [hc', bind_ok]
      obtain ⟨i', hi', hi'_val⟩ := usize_add_ok (a := i) (b := 1#usize) (by scalar_tac)
      simp only [hi', bind_ok]
      exact ih c' i' (by scalar_tac) (fun j hj1 hj2 => heq' j (by scalar_tac) hj2)
  exact hmain (limit.val - i.val) crc i rfl heq

/-- Congruence: `record_crc` depends ONLY on `buf[4..12]` and `buf[16..16+payload_len]`,
so writing the CRC into `buf[12..16]` leaves `record_crc` completely unchanged. -/
theorem record_crc_congr (buf₁ buf₂ : Slice U8) (plen : Usize)
    (hplen : plen.val ≤ 65535)
    (hl₁ : 16 + plen.val ≤ buf₁.length) (hl₂ : 16 + plen.val ≤ buf₂.length)
    (hhdr : ∀ j, 4 ≤ j → j < 12 → buf₁.val[j]? = buf₂.val[j]?)
    (hpay : ∀ j, 16 ≤ j → j < 16 + plen.val → buf₁.val[j]? = buf₂.val[j]?) :
    record_crc buf₁ plen = record_crc buf₂ plen := by
  unfold record_crc
  have hm := max_payload_val
  have hh := hdr_val
  have hcast : (UScalar.cast UScalarTy.Usize MAX_PAYLOAD_LEN).val = 65535 := by
    have := H5iAppLib.usize_max_ge; scalar_tac
  have hnotgt : ¬ (plen > UScalar.cast UScalarTy.Usize MAX_PAYLOAD_LEN) := by
    simp [UScalar.lt_equiv, gt_iff_lt, hcast]; omega
  simp only [lift, bind_ok, hnotgt, ↓reduceIte]
  obtain ⟨endU, hendU, hendU_val⟩ := usize_add_ok (a := HDR) (b := plen) (by
    have := H5iAppLib.usize_max_ge; scalar_tac)
  simp only [hendU, bind_ok]
  have hvl1 := slice_val_length buf₁
  have hvl2 := slice_val_length buf₂
  have hng1 : ¬ (endU > Slice.len buf₁) := by simp [UScalar.lt_equiv, gt_iff_lt, Slice.len]; omega
  have hng2 : ¬ (endU > Slice.len buf₂) := by simp [UScalar.lt_equiv, gt_iff_lt, Slice.len]; omega
  simp only [hng1, hng2, ↓reduceIte]
  have hc1 : crc32_range 4294967295#u32 buf₁ 4#usize 12#usize =
      crc32_range 4294967295#u32 buf₂ 4#usize 12#usize := by
    unfold crc32_range
    have le1 : 12#usize ≤ Slice.len buf₁ := by simp [UScalar.le_equiv, Slice.len]; scalar_tac
    have le2 : 12#usize ≤ Slice.len buf₂ := by simp [UScalar.le_equiv, Slice.len]; scalar_tac
    simp only [le1, le2, ↓reduceIte, bind_ok]
    exact crc32_range_loop_congr buf₁ buf₂ 12#usize 4#usize 4294967295#u32 (by scalar_tac) (by scalar_tac)
      (fun j hj1 hj2 => hhdr j (by scalar_tac) (by scalar_tac))
  rw [hc1]
  obtain ⟨c1, hc1_ok⟩ := H5iAppLib.ok_of (crc32_range_spec 4294967295#u32 buf₂ 4#usize 12#usize)
  simp only [hc1_ok, bind_ok]
  have hc2 : crc32_range c1 buf₁ HDR endU = crc32_range c1 buf₂ HDR endU := by
    unfold crc32_range
    have le1 : endU ≤ Slice.len buf₁ := by simp [UScalar.le_equiv, Slice.len]; omega
    have le2 : endU ≤ Slice.len buf₂ := by simp [UScalar.le_equiv, Slice.len]; omega
    simp only [le1, le2, ↓reduceIte, bind_ok]
    exact crc32_range_loop_congr buf₁ buf₂ endU HDR c1 (by omega) (by omega)
      (fun j hj1 hj2 => hpay j (by omega) (by omega))
  rw [hc2]

@[step]
theorem record_crc_spec (buf : Slice U8) (plen : Usize) :
    record_crc buf plen ⦃ o =>
      (plen.val ≤ 65535 → 16 + plen.val ≤ buf.length → ∃ crc, o = some crc) ⦄ := by
  unfold record_crc
  have := max_payload_val
  have := hdr_val
  have := H5iAppLib.usize_max_ge
  step*
  all_goals first
    | (intro _ _; exact ⟨_, rfl⟩)
    | (intro h1 h2; scalar_tac)

@[step]
theorem all_erased_spec (buf : Slice U8) (start «end» : Usize) :
    all_erased buf start «end» ⦃ b =>
      b = true ↔ «end».val ≤ buf.length ∧
        ∀ i b', start.val ≤ i → i < «end».val → buf.val[i]? = some b' → b'.val = 255 ⦄ := by
  unfold all_erased all_erased_loop
  step*
  apply loop.spec_decr_nat
    (measure := fun j : Usize => «end».val - j.val)
    (inv := fun j => start.val ≤ j.val ∧
      ∀ k b', start.val ≤ k → k < j.val → buf.val[k]? = some b' → b'.val = 255)
  · intro j ⟨hsj, hprev⟩
    unfold all_erased_loop.body
    step*
    · constructor
      · intro hf; cases hf
      · intro ⟨_, hall⟩
        have hjlen : j.val < buf.val.length := by rw [slice_val_length]; scalar_tac
        have h255 := hall j.val (buf.val[j.val]'hjlen) hsj (by scalar_tac) (List.getElem?_eq_getElem hjlen)
        have hne : i1.val ≠ 255 := by
          intro h
          have heq : i1 = 255#u8 := UScalar.eq_of_val_eq (by simpa using h)
          simp [heq] at *
        have hi1_eq : i1.val = (buf.val[j.val]'hjlen).val := by simp [i1_post]
        omega
    · refine ⟨by scalar_tac, ?_, by scalar_tac⟩
      intro k b' hsk hkj hget
      by_cases hkj' : k < j.val
      · exact hprev k b' hsk hkj' hget
      · have hkeq : k = j.val := by scalar_tac
        subst hkeq
        have hjlen : j.val < buf.val.length := by rw [slice_val_length]; scalar_tac
        rw [List.getElem?_eq_getElem hjlen, Option.some.injEq] at hget
        subst hget
        have hi1_255 : i1.val = 255 := by scalar_tac
        simpa [i1_post] using hi1_255
  · exact ⟨Nat.le_refl _, fun _ _ h1 h2 _ => by omega⟩

@[step]
theorem payload_matches_total (last : Slice U8) (last_len : Option Usize) (buf : Slice U8) (plen : Usize)
    (hplen : plen.val ≤ 65535) :
    payload_matches last last_len buf plen ⦃ _ => True ⦄ := by
  unfold payload_matches payload_matches_loop
  have := hdr_val
  have := H5iAppLib.usize_max_ge
  cases last_len with
  | none => step*
  | some ll =>
    step*
    all_goals first
      | scalar_tac
      | (apply loop.spec_decr_nat (measure := fun j : Usize => plen.val - j.val) (inv := fun _ => True)
         · intro j _
           unfold payload_matches_loop.body
           step*
         · trivial)

@[step]
theorem encode_record_loop_spec (buf : Slice U8) (rl pad : Usize) (hrl : rl.val ≤ buf.length) :
    encode_record_loop buf rl pad ⦃ buf' =>
      buf'.length = buf.length ∧
      (∀ i, i < pad.val ∨ rl.val ≤ i → buf'.val[i]? = buf.val[i]?) ∧
      (∀ i b, pad.val ≤ i → i < rl.val → buf'.val[i]? = some b → b.val = 255) ⦄ := by
  unfold encode_record_loop
  apply loop.spec_decr_nat
    (measure := fun p : Slice U8 × Usize => rl.val - p.2.val)
    (inv := fun p =>
      pad.val ≤ p.2.val ∧
      p.1.length = buf.length ∧
      (∀ i, i < pad.val ∨ rl.val ≤ i → p.1.val[i]? = buf.val[i]?) ∧
      (∀ i b, pad.val ≤ i → i < p.2.val → p.1.val[i]? = some b → b.val = 255))
  · intro ⟨b1, p1⟩ ⟨hp, hlen, hpres, hpad⟩
    unfold encode_record_loop.body
    step*
    · refine ⟨by scalar_tac, by simp [s_post, hlen], ?_, ?_, by scalar_tac⟩
      · intro i hout
        have hne : p1.val ≠ i := by scalar_tac
        simpa [s_post, List.getElem?_set_ne hne] using hpres i hout
      · intro i b h1 h2 hget
        by_cases hlt : i < p1.val
        · have hne : p1.val ≠ i := by omega
          simp [s_post, List.getElem?_set_ne hne] at hget
          exact hpad i b h1 hlt hget
        · have heq : i = p1.val := by scalar_tac
          subst heq
          have hp_lt : p1.val < b1.val.length := by rw [slice_val_length, hlen]; scalar_tac
          simp [s_post, List.getElem?_set_self hp_lt] at hget
          subst hget
          rfl
  · exact ⟨Nat.le_refl _, rfl, fun _ _ => rfl, fun _ _ h1 h2 _ => by dsimp only at h2; omega⟩

private theorem encode_record_fields_ok
    (g : FlashGeometry) (buf : Slice U8) (seq : U32) (version : U16)
    (payload_len a rl : U32)
    (hrl : record_len payload_len a = ok (some rl))
    (hbuf : buf.length = g.buf_cap.val)
    (hrl_buf : rl.val ≤ g.buf_cap.val) :
    encode_record buf seq version payload_len a ⦃ res =>
      res.1 = some rl ∧
      res.2.length = buf.length ∧
      (∀ i, 16 ≤ i → i < 16 + payload_len.val → res.2.val[i]? = buf.val[i]?) ∧
      get_u32_le res.2 0#usize = ok (some MAGIC) ∧
      get_u32_le res.2 4#usize = ok (some seq) ∧
      get_u16_le res.2 8#usize = ok (some version) ∧
      get_u16_le res.2 10#usize = ok (some (UScalar.cast .U16 payload_len)) ∧
      validate_record_crc res.2 payload_len = ok true ⦄ := by
  have hrl_p := (H5iAppLib.post_of_ok (record_len_spec payload_len a) hrl).1 rl rfl
  have hh := hdr_val
  have husize := H5iAppLib.usize_max_ge
  have hrl_cast : (UScalar.cast UScalarTy.Usize rl).val = rl.val := by scalar_tac
  have hplen_cast : (UScalar.cast UScalarTy.Usize payload_len).val = payload_len.val := by scalar_tac
  obtain ⟨padU, hpad_ok, hpad_val⟩ := usize_add_ok (a := HDR) (b := UScalar.cast .Usize payload_len) (by scalar_tac)
  obtain ⟨buf1, hloop⟩ := H5iAppLib.ok_of (encode_record_loop_spec buf (UScalar.cast .Usize rl) padU (by omega))
  have hloop_p := H5iAppLib.post_of_ok (encode_record_loop_spec buf (UScalar.cast .Usize rl) padU (by omega)) hloop
  obtain ⟨buf2, hput0, hget0_2⟩ := put_get_u32_le_roundtrip buf1 0#usize MAGIC (by scalar_tac)
  have hput0_p := H5iAppLib.post_of_ok (put_u32_le_spec buf1 0#usize MAGIC (by scalar_tac)) hput0
  dsimp only at hput0_p
  obtain ⟨buf3, hput4, hget4_3⟩ := put_get_u32_le_roundtrip buf2 4#usize seq (by scalar_tac)
  have hput4_p := H5iAppLib.post_of_ok (put_u32_le_spec buf2 4#usize seq (by scalar_tac)) hput4
  dsimp only at hput4_p
  obtain ⟨buf4, hput8, hget8_4⟩ := put_get_u16_le_roundtrip buf3 8#usize version (by scalar_tac)
  have hput8_p := H5iAppLib.post_of_ok (put_u16_le_spec buf3 8#usize version (by scalar_tac)) hput8
  dsimp only at hput8_p
  obtain ⟨buf5, hput10, hget10_5⟩ := put_get_u16_le_roundtrip buf4 10#usize (UScalar.cast .U16 payload_len) (by scalar_tac)
  have hput10_p := H5iAppLib.post_of_ok (put_u16_le_spec buf4 10#usize (UScalar.cast .U16 payload_len) (by scalar_tac)) hput10
  dsimp only at hput10_p
  obtain ⟨ocrc, hcrc⟩ := H5iAppLib.ok_of (record_crc_spec buf5 (UScalar.cast .Usize payload_len))
  obtain ⟨crc, hocrc⟩ := H5iAppLib.post_of_ok (record_crc_spec buf5 (UScalar.cast .Usize payload_len)) hcrc (by omega) (by omega)
  subst hocrc
  obtain ⟨buf6, hput12, hget12_6⟩ := put_get_u32_le_roundtrip buf5 12#usize crc (by scalar_tac)
  have hput12_p := H5iAppLib.post_of_ok (put_u32_le_spec buf5 12#usize crc (by scalar_tac)) hput12
  dsimp only at hput12_p
  have henc_val : encode_record buf seq version payload_len a = ok (some rl, buf6) := by
    unfold encode_record
    have hvl := slice_val_length buf
    have hnotgt : ¬ (UScalar.cast UScalarTy.Usize rl > Slice.len buf) := by
      simp [UScalar.lt_equiv, gt_iff_lt, Slice.len, hrl_cast]; omega
    apply H5iAppLib.bind_tc_eq_ok.mpr; refine ⟨some rl, hrl, ?_⟩
    apply H5iAppLib.bind_tc_eq_ok.mpr; refine ⟨_, rfl, ?_⟩
    apply H5iAppLib.bind_tc_eq_ok.mpr; refine ⟨_, rfl, ?_⟩
    rw [if_neg hnotgt]
    apply H5iAppLib.bind_tc_eq_ok.mpr; refine ⟨padU, hpad_ok, ?_⟩
    apply H5iAppLib.bind_tc_eq_ok.mpr; refine ⟨buf1, hloop, ?_⟩
    apply H5iAppLib.bind_tc_eq_ok.mpr; refine ⟨(true, buf2), hput0, ?_⟩
    apply H5iAppLib.bind_tc_eq_ok.mpr; refine ⟨(true, buf3), hput4, ?_⟩
    apply H5iAppLib.bind_tc_eq_ok.mpr; refine ⟨(true, buf4), hput8, ?_⟩
    apply H5iAppLib.bind_tc_eq_ok.mpr; refine ⟨_, rfl, ?_⟩
    apply H5iAppLib.bind_tc_eq_ok.mpr; refine ⟨(true, buf5), hput10, ?_⟩
    apply H5iAppLib.bind_tc_eq_ok.mpr; refine ⟨some crc, hcrc, ?_⟩
    apply H5iAppLib.bind_tc_eq_ok.mpr; refine ⟨(true, buf6), hput12, ?_⟩
    rfl
  rw [henc_val]
  have hlen6 : buf6.length = buf.length := by omega
  have hbyte_eq_get32 : ∀ (bA bB : Slice U8) (o : Usize) (v : U32),
      o.val + 4 ≤ bA.length → bB.length = bA.length →
      (∀ j, o.val ≤ j → j < o.val + 4 → bB.val[j]? = bA.val[j]?) →
      get_u32_le bA o = ok (some v) → get_u32_le bB o = ok (some v) := by
    intro bA bB o v hbo hbl hbe hga
    obtain ⟨rA, hA, vA, a0, a1, a2, a3, hrA, ha0, ha1, ha2, ha3, hvA⟩ :=
      H5iAppLib.spec_exists_ok (get_u32_le_spec bA o hbo)
    rw [hga, Result.ok.injEq] at hA; subst hA; cases hrA
    obtain ⟨rB, hB, vB, b0, b1, b2, b3, hrB, hb0, hb1, hb2, hb3, hvB⟩ :=
      H5iAppLib.spec_exists_ok (get_u32_le_spec bB o (by omega))
    subst hrB
    have e0 : b0 = a0 := by have := hbe o.val (by omega) (by omega); rw [hb0, ha0] at this; exact Option.some.inj this
    have e1 : b1 = a1 := by have := hbe (o.val + 1) (by omega) (by omega); rw [hb1, ha1] at this; exact Option.some.inj this
    have e2 : b2 = a2 := by have := hbe (o.val + 2) (by omega) (by omega); rw [hb2, ha2] at this; exact Option.some.inj this
    have e3 : b3 = a3 := by have := hbe (o.val + 3) (by omega) (by omega); rw [hb3, ha3] at this; exact Option.some.inj this
    have heq : vB = v := UScalar.eq_of_val_eq (by rw [hvB, hvA, e0, e1, e2, e3])
    rwa [heq] at hB
  have hbyte_eq_get16 : ∀ (bA bB : Slice U8) (o : Usize) (v : U16),
      o.val + 2 ≤ bA.length → bB.length = bA.length →
      (∀ j, o.val ≤ j → j < o.val + 2 → bB.val[j]? = bA.val[j]?) →
      get_u16_le bA o = ok (some v) → get_u16_le bB o = ok (some v) := by
    intro bA bB o v hbo hbl hbe hga
    obtain ⟨rA, hA, vA, a0, a1, hrA, ha0, ha1, hvA⟩ :=
      H5iAppLib.spec_exists_ok (get_u16_le_spec bA o hbo)
    rw [hga, Result.ok.injEq] at hA; subst hA; cases hrA
    obtain ⟨rB, hB, vB, b0, b1, hrB, hb0, hb1, hvB⟩ :=
      H5iAppLib.spec_exists_ok (get_u16_le_spec bB o (by omega))
    subst hrB
    have e0 : b0 = a0 := by have := hbe o.val (by omega) (by omega); rw [hb0, ha0] at this; exact Option.some.inj this
    have e1 : b1 = a1 := by have := hbe (o.val + 1) (by omega) (by omega); rw [hb1, ha1] at this; exact Option.some.inj this
    have heq : vB = v := UScalar.eq_of_val_eq (by rw [hvB, hvA, e0, e1])
    rwa [heq] at hB
  have hget0_6 : get_u32_le buf6 0#usize = ok (some MAGIC) := by
    apply hbyte_eq_get32 buf2 buf6 0#usize MAGIC (by scalar_tac) (by omega) _ hget0_2
    intro j h1 h2
    rw [hput12_p.2.2.1 j (Or.inl (by scalar_tac)), hput10_p.2.2.1 j (Or.inl (by scalar_tac)),
        hput8_p.2.2.1 j (Or.inl (by scalar_tac)), hput4_p.2.2.1 j (Or.inl (by scalar_tac))]
  have hget4_6 : get_u32_le buf6 4#usize = ok (some seq) := by
    apply hbyte_eq_get32 buf3 buf6 4#usize seq (by scalar_tac) (by omega) _ hget4_3
    intro j h1 h2
    rw [hput12_p.2.2.1 j (Or.inl (by scalar_tac)), hput10_p.2.2.1 j (Or.inl (by scalar_tac)),
        hput8_p.2.2.1 j (Or.inl (by scalar_tac))]
  have hget8_6 : get_u16_le buf6 8#usize = ok (some version) := by
    apply hbyte_eq_get16 buf4 buf6 8#usize version (by scalar_tac) (by omega) _ hget8_4
    intro j h1 h2
    rw [hput12_p.2.2.1 j (Or.inl (by scalar_tac)), hput10_p.2.2.1 j (Or.inl (by scalar_tac))]
  have hget10_6 : get_u16_le buf6 10#usize = ok (some (UScalar.cast .U16 payload_len)) := by
    apply hbyte_eq_get16 buf5 buf6 10#usize _ (by scalar_tac) (by omega) _ hget10_5
    intro j h1 h2
    exact hput12_p.2.2.1 j (Or.inl (by scalar_tac))
  have hcrc6 : record_crc buf6 (UScalar.cast .Usize payload_len) = ok (some crc) := by
    rw [← hcrc]
    exact record_crc_congr buf6 buf5 (UScalar.cast .Usize payload_len)
      (by omega) (by omega) (by omega)
      (fun j hj1 hj2 => hput12_p.2.2.1 j (Or.inl (by scalar_tac)))
      (fun j hj1 hj2 => hput12_p.2.2.1 j (Or.inr (by scalar_tac)))
  have hval_crc : validate_record_crc buf6 payload_len = ok true := by
    unfold validate_record_crc
    rw [hget12_6]
    simp only [bind_ok, lift]
    rw [hcrc6]
    simp only [bind_ok]
    rfl
  have hpay_pres : ∀ i, 16 ≤ i → i < 16 + payload_len.val → buf6.val[i]? = buf.val[i]? := by
    intro i h1 h2
    have h12 : (12#usize).val + 4 ≤ i := by simp; omega
    have h10 : (10#usize).val + 2 ≤ i := by simp; omega
    have h8 : (8#usize).val + 2 ≤ i := by simp; omega
    have h4 : (4#usize).val + 4 ≤ i := by simp; omega
    have h0 : (0#usize).val + 4 ≤ i := by simp; omega
    rw [hput12_p.2.2.1 i (Or.inr h12),
        hput10_p.2.2.1 i (Or.inr h10),
        hput8_p.2.2.1 i (Or.inr h8),
        hput4_p.2.2.1 i (Or.inr h4),
        hput0_p.2.2.1 i (Or.inr h0),
        hloop_p.2.1 i (Or.inl (by omega))]
  rw [WP.spec_ok]
  exact ⟨rfl, hlen6, hpay_pres, hget0_6, hget4_6, hget8_6, hget10_6, hval_crc⟩

/-- **Functional correctness of `encode_record` → `parse_header` + `validate_record_crc`**:
Framing any payload in `buf[16..16 + payload_len]` via extracted `encode_record`
produces an aligned record of length `rl` that:
1. preserves the payload bytes in `buf'[16..16 + payload_len]`,
2. parses back cleanly via extracted `parse_header` with exact `(seq, version, payload_len, rl)`,
3. passes extracted `validate_record_crc` (`ok true`). -/
theorem encode_record_roundtrip
    (g : FlashGeometry) (buf : Slice U8) (seq : U32) (version : U16)
    (payload_len off a rl : U32)
    (hlay : layout_ok g = ok true)
    (ha : align g.write_size g.read_size = ok a)
    (hrl : record_len payload_len a = ok (some rl))
    (hbuf : buf.length = g.buf_cap.val)
    (hrl_buf : rl.val ≤ g.buf_cap.val)
    (hoff : off.val + rl.val ≤ g.region.val) :
    ∃ buf',
      encode_record buf seq version payload_len a = ok (some rl, buf') ∧
      buf'.length = buf.length ∧
      (∀ i, 16 ≤ i → i < 16 + payload_len.val → buf'.val[i]? = buf.val[i]?) ∧
      parse_header buf' off g = ok (.Ok { seq, version, payload_len, record_len := rl }) ∧
      validate_record_crc buf' payload_len = ok true := by
  have hrl_p := (H5iAppLib.post_of_ok (record_len_spec payload_len a) hrl).1 rl rfl
  have hm := magic_val
  have hh := hdr_val
  obtain ⟨⟨ores, buf'⟩, henc, hore, hlen', hpay', hget0, hget4, hget8, hget10, hvcrc⟩ :=
    H5iAppLib.spec_exists_ok (encode_record_fields_ok g buf seq version payload_len a rl hrl hbuf hrl_buf)
  dsimp only at hore hlen' hpay' hget0 hget4 hget8 hget10 hvcrc
  subst hore
  have hnot_erased : all_erased buf' 0#usize HDR = ok false := by
    obtain ⟨b, hb, hbp⟩ := H5iAppLib.spec_exists_ok (all_erased_spec buf' 0#usize HDR)
    cases b with
    | false => exact hb
    | true =>
      have ⟨_, hall⟩ := hbp.mp rfl
      have hz : (0#usize).val = 0 := by simp
      have h0b : (0#usize).val + 4 ≤ buf'.length := by rw [hz]; omega
      obtain ⟨r, hr, v, b0, b1, b2, b3, hr_eq, hb0, hb1, hb2, hb3, hv⟩ :=
        H5iAppLib.spec_exists_ok (get_u32_le_spec buf' 0#usize h0b)
      rw [hget0, Result.ok.injEq] at hr; subst hr; cases hr_eq
      rw [hz] at hb0 hb1 hb2 hb3 hall
      have h0 := hall 0 b0 (Nat.zero_le _) (by rw [hh]; omega) hb0
      have h1 := hall 1 b1 (Nat.zero_le _) (by rw [hh]; omega) hb1
      have h2 := hall 2 b2 (Nat.zero_le _) (by rw [hh]; omega) hb2
      have h3 := hall 3 b3 (Nat.zero_le _) (by rw [hh]; omega) hb3
      rw [hm, h0, h1, h2, h3] at hv
      omega
  have hparse : parse_header buf' off g = ok (.Ok { seq, version, payload_len, record_len := rl }) := by
    unfold parse_header
    have hvl' := slice_val_length buf'
    have hnotlt : ¬ (Slice.len buf' < HDR) := by simp [UScalar.lt_equiv, Slice.len, hh]; omega
    rw [if_neg hnotlt]
    apply H5iAppLib.bind_tc_eq_ok.mpr; refine ⟨false, hnot_erased, ?_⟩
    rw [if_neg Bool.false_ne_true]
    apply H5iAppLib.bind_tc_eq_ok.mpr; refine ⟨some MAGIC, hget0, ?_⟩
    apply H5iAppLib.bind_tc_eq_ok.mpr; refine ⟨some seq, hget4, ?_⟩
    apply H5iAppLib.bind_tc_eq_ok.mpr; refine ⟨some version, hget8, ?_⟩
    apply H5iAppLib.bind_tc_eq_ok.mpr; refine ⟨some (UScalar.cast .U16 payload_len), hget10, ?_⟩
    apply H5iAppLib.bind_tc_eq_ok.mpr; refine ⟨payload_len, ?_, ?_⟩
    · show ok (UScalar.cast UScalarTy.U32 (UScalar.cast UScalarTy.U16 payload_len)) = ok payload_len
      congr 1; apply UScalar.eq_of_val_eq; scalar_tac
    apply H5iAppLib.bind_tc_eq_ok.mpr; refine ⟨a, ha, ?_⟩
    apply H5iAppLib.bind_tc_eq_ok.mpr; refine ⟨some rl, hrl, ?_⟩
    have hneq_m : (MAGIC != MAGIC) = false := bne_self_eq_false MAGIC
    have hng1 : ¬ (rl > g.buf_cap) := by simp [UScalar.lt_equiv, gt_iff_lt]; omega
    have hng2 : ¬ (off > g.region) := by simp [UScalar.lt_equiv, gt_iff_lt]; omega
    obtain ⟨rem, hsub, hsub_val⟩ := u32_sub_ok (a := g.region) (b := off) (by omega)
    have hng3 : ¬ (rl > rem) := by simp [UScalar.lt_equiv, gt_iff_lt]; omega
    rw [hneq_m]
    dsimp only
    rw [if_neg Bool.false_ne_true, if_neg hng1, if_neg hng2]
    apply H5iAppLib.bind_tc_eq_ok.mpr; refine ⟨rem, hsub, ?_⟩
    rw [if_neg hng3]
  exact ⟨buf', henc, hlen', hpay', hparse, hvcrc⟩

/-! ## 5. Memory & Storage Safety of Save and Recovery State Machines -/

/-- **Storage & memory safety of extracted `plan_save`**:
Every I/O action prescribed by `plan_save` satisfies `NorFlash.{WriteValid, EraseValid}`,
fits within `buf_cap` and `region`, advances `seq` via `next_seq` (strictly newer by
`seq_newer_next_seq`), NEVER erases the active region `pos.region`, and NEVER
overwrites committed records before `target_off`. -/
theorem plan_save_safe (g : FlashGeometry) (pos : Pos) (payload_len : U32)
    (hlay : layout_ok g = ok true) (hdiv : AlignDivides g) (hreg : pos.region.val ≤ 1) :
    plan_save g pos payload_len ⦃ plan =>
      match plan with
      | .TooLarge => True
      | .WriteOnly tr toff waddr wlen nseq =>
        let a_nat := max (max g.write_size.val g.read_size.val) 4
        tr = pos.region ∧
        pos.append = some toff ∧
        waddr.val = regionStartNat g tr.val + toff.val ∧
        NorFlash.WriteValid (toFlashConfig g) waddr.val wlen.val ∧
        wlen.val ≤ g.buf_cap.val ∧
        toff.val + wlen.val ≤ g.region.val ∧
        NorFlash.disjoint waddr.val wlen.val (regionStartNat g pos.region.val) toff.val ∧
        nseq = core.num.U32.wrapping_add pos.seq 1#u32 ∧
        16 ≤ wlen.val ∧
        toff.val % a_nat = 0 ∧
        wlen.val % a_nat = 0 ∧
        (∀ a, align g.write_size g.read_size = ok a → record_len payload_len a = ok (some wlen))
      | .EraseThenWrite efrom eto tr waddr wlen nseq =>
        let a_nat := max (max g.write_size.val g.read_size.val) 4
        tr.val ≤ 1 ∧ tr ≠ pos.region ∧
        efrom.val = regionStartNat g tr.val ∧
        eto.val = efrom.val + g.region.val ∧
        NorFlash.EraseValid (toFlashConfig g) efrom.val eto.val ∧
        NorFlash.disjoint efrom.val (eto.val - efrom.val) (regionStartNat g pos.region.val) g.region.val ∧
        waddr = efrom ∧
        NorFlash.WriteValid (toFlashConfig g) waddr.val wlen.val ∧
        NorFlash.disjoint waddr.val wlen.val (regionStartNat g pos.region.val) g.region.val ∧
        wlen.val ≤ g.buf_cap.val ∧ wlen.val ≤ g.region.val ∧
        nseq = core.num.U32.wrapping_add pos.seq 1#u32 ∧
        16 ≤ wlen.val ∧
        wlen.val % a_nat = 0 ∧
        (∀ a, align g.write_size g.read_size = ok a → record_len payload_len a = ok (some wlen)) ⦄ := by
  have hp := H5iAppLib.post_of_ok (layout_ok_spec g) hlay rfl
  have hcap_u32 : g.base.val + g.region.val ≤ U32.max := by
    have := hp.in_capacity; scalar_tac
  unfold plan_save
  simp only [hlay, bind_ok, ↓reduceIte]
  have hnotgt : ¬ (pos.region > 1#u32) := by simp [UScalar.lt_equiv, gt_iff_lt]; omega
  simp only [hnotgt, ↓reduceIte]
  obtain ⟨a, ha, ha_val⟩ := H5iAppLib.spec_exists_ok (align_eq_max g.write_size g.read_size)
  simp only [ha, bind_ok]
  obtain ⟨orl, horl, horl_p, _⟩ := H5iAppLib.spec_exists_ok (record_len_spec payload_len a)
  simp only [horl, bind_ok]
  cases orl with
  | none => rw [WP.spec_ok]; trivial
  | some rl =>
    have hrl_p := horl_p rl rfl
    have hrl_for_a : ∀ a', align g.write_size g.read_size = ok a' → record_len payload_len a' = ok (some rl) := by
      intro a' ha'
      rw [ha, Result.ok.injEq] at ha'
      subst ha'; exact horl
    by_cases h1 : rl > g.buf_cap
    · simp [h1]
    · by_cases h2 : rl > g.region
      · simp [h1, h2]
      · simp only [h1, h2, ↓reduceIte]
        obtain ⟨nseq, hnseq, hnseq_p⟩ := H5iAppLib.spec_exists_ok (next_seq_spec pos.seq)
        simp only [hnseq, bind_ok]
        have hwrite_align_rl : rl.val % g.write_size.val = 0 :=
          mod_zero_of_dvd_mod_zero (by rw [← ha_val]; exact hrl_p.2.2.2.2.1) hdiv.1
        have hbase_walign : g.base.val % g.write_size.val = 0 :=
          mod_zero_of_dvd_mod_zero hp.base_aligned hdiv.1
        have hreg_walign : g.region.val % g.write_size.val = 0 :=
          mod_zero_of_dvd_mod_zero hp.region_aligned hdiv.1
        have herase_ok :
            (do
              let other ← other_region pos.region
              let start ← region_start g other
              let i ← start + g.region
              ok (SavePlan.EraseThenWrite start i other start rl nseq)) ⦃ plan =>
              match plan with
              | .EraseThenWrite efrom eto tr waddr wlen ns =>
                let a_nat := max (max g.write_size.val g.read_size.val) 4
                tr.val ≤ 1 ∧ tr ≠ pos.region ∧
                efrom.val = regionStartNat g tr.val ∧
                eto.val = efrom.val + g.region.val ∧
                NorFlash.EraseValid (toFlashConfig g) efrom.val eto.val ∧
                NorFlash.disjoint efrom.val (eto.val - efrom.val) (regionStartNat g pos.region.val) g.region.val ∧
                waddr = efrom ∧
                NorFlash.WriteValid (toFlashConfig g) waddr.val wlen.val ∧
                NorFlash.disjoint waddr.val wlen.val (regionStartNat g pos.region.val) g.region.val ∧
                wlen.val ≤ g.buf_cap.val ∧ wlen.val ≤ g.region.val ∧
                ns = core.num.U32.wrapping_add pos.seq 1#u32 ∧
                16 ≤ wlen.val ∧
                wlen.val % a_nat = 0 ∧
                (∀ a', ok a = ok a' → record_len payload_len a' = ok (some wlen))
              | _ => False ⦄ := by
          obtain ⟨other, hoth, hoth_le, hoth_ne⟩ := H5iAppLib.spec_exists_ok (other_region_spec pos.region)
          have ⟨hne_val, hoth_eq⟩ := hoth_ne hreg
          obtain ⟨start, hst, hst_val⟩ := H5iAppLib.spec_exists_ok (region_start_spec g other hcap_u32)
          have hst_le : start.val + g.region.val ≤ g.capacity.val := by
            rw [hst_val]; unfold regionStartNat
            split <;> have := hp.in_capacity <;> omega
          obtain ⟨eto, hadd_ok, heto_val⟩ := u32_add_ok (a := start) (b := g.region) (by scalar_tac)
          simp only [hoth, hst, hadd_ok, bind_ok]
          rw [WP.spec_ok]
          have hdisj_reg : NorFlash.disjoint start.val g.region.val (regionStartNat g pos.region.val) g.region.val := by
            rw [hst_val]; unfold regionStartNat
            by_cases hr0 : pos.region.val = 0
            · have : other.val = 1 := by omega
              simp only [hr0, this, ↓reduceIte, Nat.one_ne_zero]
              exact Or.inr (Nat.le_refl _)
            · have : other.val = 0 := by omega
              simp only [hr0, this, ↓reduceIte]
              exact Or.inl (Nat.le_refl _)
          have hst_emod : start.val % g.erase_size.val = 0 := by
            rw [hst_val]; unfold regionStartNat
            split
            · exact hp.base_erase_aligned
            · rw [Nat.add_mod, hp.base_erase_aligned, hp.region_erase_aligned]; simp
          have hst_wmod : start.val % g.write_size.val = 0 := by
            rw [hst_val]; unfold regionStartNat
            split
            · exact hbase_walign
            · rw [Nat.add_mod, hbase_walign, hreg_walign]; simp
          have heto_emod : eto.val % g.erase_size.val = 0 := by
            rw [heto_val, Nat.add_mod, hst_emod, hp.region_erase_aligned]; simp
          have hrl_le_reg : rl.val ≤ g.region.val := by simp [UScalar.lt_equiv, gt_iff_lt] at h2; omega
          have hrl_le_buf : rl.val ≤ g.buf_cap.val := by simp [UScalar.lt_equiv, gt_iff_lt] at h1; omega
          dsimp only [NorFlash.EraseValid, NorFlash.WriteValid, toFlashConfig]
          refine ⟨hoth_le, fun heq => hne_val (congrArg UScalar.val heq), hst_val, heto_val,
            ⟨by omega, by omega, hst_emod, heto_emod⟩,
            by simpa [heto_val] using hdisj_reg, rfl,
            ⟨by omega, hst_wmod, hwrite_align_rl⟩,
            ?_, hrl_le_buf, hrl_le_reg, hnseq_p.1, by omega,
            by rw [← ha_val]; exact hrl_p.2.2.2.2.1,
            fun a' ha' => by rw [Result.ok.injEq] at ha'; subst ha'; exact horl⟩
          rcases hdisj_reg with hd | hd
          · exact Or.inl (by omega)
          · exact Or.inr (by omega)
        cases hpos_app : pos.append with
        | none =>
          exact WP.spec_mono herase_ok (fun p hp' => by cases p with | TooLarge => trivial | WriteOnly => exact False.elim hp' | EraseThenWrite => exact hp')
        | some off =>
          by_cases hoff_le : off ≤ g.region
          · simp only [hoff_le, ↓reduceIte]
            have hoff_le_nat : off.val ≤ g.region.val := by simpa [UScalar.le_equiv] using hoff_le
            obtain ⟨rem, hsub_ok, hrem_val⟩ := u32_sub_ok (a := g.region) (b := off) hoff_le_nat
            simp only [hsub_ok, bind_ok]
            by_cases hrl_fit : rl ≤ rem
            · simp only [hrl_fit, ↓reduceIte]
              obtain ⟨i1, hrem_ok, hi1_val⟩ := u32_rem_ok (a := off) (b := a) (by omega)
              simp only [hrem_ok, bind_ok]
              by_cases hrem0 : i1 = 0#u32
              · simp only [hrem0, ↓reduceIte]
                obtain ⟨rs, hrs, hrs_val⟩ := H5iAppLib.spec_exists_ok (region_start_spec g pos.region hcap_u32)
                have hfit_nat : off.val + rl.val ≤ g.region.val := by
                  simp [UScalar.le_equiv] at hrl_fit; omega
                have hwa_le : rs.val + off.val + rl.val ≤ g.capacity.val := by
                  rw [hrs_val]; unfold regionStartNat; split <;> have := hp.in_capacity <;> omega
                obtain ⟨wa, hwa_ok, hwa_val⟩ := u32_add_ok (a := rs) (b := off) (by scalar_tac)
                simp only [hrs, hwa_ok, bind_ok]
                rw [WP.spec_ok]
                have hoff_amod : off.val % a.val = 0 := by
                  have : i1.val = 0 := by simpa using congrArg UScalar.val hrem0
                  omega
                have hoff_wmod : off.val % g.write_size.val = 0 :=
                  mod_zero_of_dvd_mod_zero (by rw [← ha_val]; exact hoff_amod) hdiv.1
                have hrs_wmod : rs.val % g.write_size.val = 0 := by
                  rw [hrs_val]; unfold regionStartNat
                  split
                  · exact hbase_walign
                  · rw [Nat.add_mod, hbase_walign, hreg_walign]; simp
                have hwa_wmod : wa.val % g.write_size.val = 0 := by
                  rw [hwa_val, Nat.add_mod, hrs_wmod, hoff_wmod]; simp
                dsimp only [NorFlash.WriteValid, toFlashConfig]
                refine ⟨rfl, rfl, by rw [hwa_val, hrs_val],
                  ⟨by omega, hwa_wmod, hwrite_align_rl⟩,
                  by simp [UScalar.lt_equiv, gt_iff_lt] at h1; omega,
                  hfit_nat,
                  Or.inr (by rw [hwa_val, hrs_val]),
                  hnseq_p.1, by omega,
                  by rw [← ha_val]; exact hoff_amod,
                  by rw [← ha_val]; exact hrl_p.2.2.2.2.1,
                  fun a' ha' => by rw [Result.ok.injEq] at ha'; subst ha'; exact horl⟩
              · simp only [hrem0, ↓reduceIte]
                exact WP.spec_mono herase_ok (fun p hp' => by cases p with | TooLarge => trivial | WriteOnly => exact False.elim hp' | EraseThenWrite => exact hp')
            · simp only [hrl_fit, ↓reduceIte]
              exact WP.spec_mono herase_ok (fun p hp' => by cases p with | TooLarge => trivial | WriteOnly => exact False.elim hp' | EraseThenWrite => exact hp')
          · simp only [hoff_le, ↓reduceIte]
            exact WP.spec_mono herase_ok (fun p hp' => by cases p with | TooLarge => trivial | WriteOnly => exact False.elim hp' | EraseThenWrite => exact hp')

/-! ### Recovery Scan State Machine: Safety & Bounded Termination -/

/-- Well-formedness invariant maintained across every step of `scan_init` / `scan_step`. -/
def ScanInv (g : FlashGeometry) (st : ScanState) : Prop :=
  let a := max (max g.write_size.val g.read_size.val) 4
  st.region.val ≤ 1 ∧
  st.off.val ≤ g.region.val ∧
  st.off.val % a = 0 ∧
  (∀ b, st.best = some b →
    b.region.val ≤ 1 ∧
    b.off.val % a = 0 ∧
    b.record_len.val % a = 0 ∧
    16 ≤ b.record_len.val ∧
    b.record_len.val ≤ g.buf_cap.val ∧
    b.off.val + b.record_len.val ≤ g.region.val) ∧
  (∀ hdr, st.phase = .ReadBody hdr →
    hdr.record_len.val % a = 0 ∧
    16 < hdr.record_len.val ∧
    hdr.record_len.val ≤ g.buf_cap.val ∧
    st.off.val + hdr.record_len.val ≤ g.region.val)

/-- Strictly decreasing natural-number termination measure for `scan_step`. -/
def scanMeasure (g : FlashGeometry) (st : ScanState) : Nat :=
  match st.phase with
  | .Done => 0
  | .ReloadBest => 1
  | .ReadBody _ => 2 + 2 * (1 - st.region.val) * (g.region.val + 1) + 2 * (g.region.val - st.off.val)
  | .ReadHeader => 2 + 2 * (1 - st.region.val) * (g.region.val + 1) + 2 * (g.region.val - st.off.val) + 1

/-- Every `ScanAction.Read` issued by `scan_init` / `scan_step` is in-bounds and read-aligned. -/
def ScanActionSafe (g : FlashGeometry) : ScanAction → Prop
  | .InvalidLayout => True
  | .Done _ _ _ => True
  | .Read addr buf_off len =>
    NorFlash.ReadValid (toFlashConfig g) addr.val len.val ∧
    buf_off.val + len.val ≤ g.buf_cap.val

theorem scan_init_safe (g : FlashGeometry) (hlay : layout_ok g = ok true) (hdiv : AlignDivides g) :
    scan_init g ⦃ res => ScanInv g res.1 ∧ ScanActionSafe g res.2 ⦄ := by
  have hp := H5iAppLib.post_of_ok (layout_ok_spec g) hlay rfl
  have hcap_u32 : g.base.val + g.region.val ≤ U32.max := by
    have := hp.in_capacity; scalar_tac
  unfold scan_init
  simp only [hlay, bind_ok, ↓reduceIte]
  obtain ⟨i, hi, hi_val⟩ := H5iAppLib.spec_exists_ok (region_start_spec g 0#u32 hcap_u32)
  simp only [hi, bind_ok]
  rw [WP.spec_ok]
  have hh := hdr_u32_val
  refine ⟨⟨by simp, by simp, by simp, (fun _ h => by cases h), (fun _ h => by cases h)⟩, ?_⟩
  simp only [ScanActionSafe, NorFlash.ReadValid, toFlashConfig]
  have hbase_rmod : g.base.val % g.read_size.val = 0 :=
    mod_zero_of_dvd_mod_zero hp.base_aligned hdiv.2
  have hhdr_rmod : 16 % g.read_size.val = 0 :=
    mod_zero_of_dvd_mod_zero hp.hdr_aligned hdiv.2
  refine ⟨⟨?_, ?_, ?_⟩, ?_⟩
  · rw [hi_val]; simp [regionStartNat, hh]; have := hp.region_ge16; have := hp.in_capacity; omega
  · rw [hi_val]; simp [regionStartNat, hbase_rmod]
  · rw [hh]; exact hhdr_rmod
  · simp [hh]; exact hp.buf_ge16

private theorem finish_region_safe_decr (g : FlashGeometry) (st : ScanState) (torn : Bool)
    (hlay : layout_ok g = ok true) (hdiv : AlignDivides g)
    (hinv : ScanInv g st) (hphase : st.phase = .ReadHeader ∨ ∃ hdr, st.phase = .ReadBody hdr) :
    finish_region g st torn ⦃ res =>
      ScanInv g res.1 ∧ ScanActionSafe g res.2 ∧ scanMeasure g res.1 < scanMeasure g st ⦄ := by
  have hp := H5iAppLib.post_of_ok (layout_ok_spec g) hlay rfl
  have hcap_u32 : g.base.val + g.region.val ≤ U32.max := by
    have := hp.in_capacity; scalar_tac
  have hbase_rmod : g.base.val % g.read_size.val = 0 := mod_zero_of_dvd_mod_zero hp.base_aligned hdiv.2
  have hreg_rmod : g.region.val % g.read_size.val = 0 := mod_zero_of_dvd_mod_zero hp.region_aligned hdiv.2
  have hhdr_rmod : 16 % g.read_size.val = 0 := mod_zero_of_dvd_mod_zero hp.hdr_aligned hdiv.2
  have hh := hdr_u32_val
  unfold finish_region
  by_cases hr0 : st.region = 0#u32
  · simp only [hr0, ↓reduceIte]
    obtain ⟨i, hi, hi_val⟩ := H5iAppLib.spec_exists_ok (region_start_spec g 1#u32 hcap_u32)
    simp only [hi, bind_ok]
    rw [WP.spec_ok]
    have hr0_val : st.region.val = 0 := by simpa using congrArg UScalar.val hr0
    refine ⟨⟨by simp, by simp, by simp, hinv.2.2.2.1, fun _ h => by cases h⟩, ?_, ?_⟩
    · simp only [ScanActionSafe, NorFlash.ReadValid, toFlashConfig]
      refine ⟨⟨?_, ?_, ?_⟩, by simp [hh]; exact hp.buf_ge16⟩
      · rw [hi_val]; simp [regionStartNat, hh]; have := hp.region_ge16; have := hp.in_capacity; omega
      · rw [hi_val]; simp [regionStartNat]; rw [Nat.add_mod, hbase_rmod, hreg_rmod]; simp
      · rw [hh]; exact hhdr_rmod
    · unfold scanMeasure
      rcases hphase with hph | ⟨hdr, hph⟩ <;> simp [hph, hr0_val] <;> omega
  · simp only [hr0, ↓reduceIte]
    have hr1_val : st.region.val = 1 := by
      have := hinv.1
      have hne : st.region.val ≠ 0 := fun h => hr0 (UScalar.eq_of_val_eq (by simpa using h))
      omega
    cases hbest : st.best with
    | none =>
      rw [WP.spec_ok]
      refine ⟨⟨by scalar_tac, hinv.2.1, hinv.2.2.1, (fun _ h => by cases h), (fun _ h => by cases h)⟩, trivial, ?_⟩
      unfold scanMeasure
      rcases hphase with hph | ⟨hdr, hph⟩ <;> simp [hph]
    | some b =>
      have hb := hinv.2.2.2.1 b hbest
      obtain ⟨i, hi, hi_val⟩ := H5iAppLib.spec_exists_ok (region_start_spec g b.region hcap_u32)
      have haddr_le : i.val + b.off.val + b.record_len.val ≤ g.capacity.val := by
        rw [hi_val]; unfold regionStartNat; split <;> have := hp.in_capacity <;> omega
      obtain ⟨raddr, hadd_ok, hraddr_val⟩ := u32_add_ok (a := i) (b := b.off) (by scalar_tac)
      simp only [hi, hadd_ok, bind_ok]
      rw [WP.spec_ok]
      refine ⟨⟨by scalar_tac, hinv.2.1, hinv.2.2.1, (fun b' hb' => hinv.2.2.2.1 b' (by rw [hbest]; exact hb')), (fun _ h => by cases h)⟩, ?_, ?_⟩
      · simp only [ScanActionSafe, NorFlash.ReadValid, toFlashConfig]
        have hi_rmod : i.val % g.read_size.val = 0 := by
          rw [hi_val]; unfold regionStartNat
          split
          · exact hbase_rmod
          · rw [Nat.add_mod, hbase_rmod, hreg_rmod]; simp
        have hboff_rmod : b.off.val % g.read_size.val = 0 := mod_zero_of_dvd_mod_zero hb.2.1 hdiv.2
        have hblen_rmod : b.record_len.val % g.read_size.val = 0 := mod_zero_of_dvd_mod_zero hb.2.2.1 hdiv.2
        refine ⟨⟨by omega, ?_, hblen_rmod⟩, by simp; exact hb.2.2.2.2.1⟩
        rw [hraddr_val, Nat.add_mod, hi_rmod, hboff_rmod]; simp
      · unfold scanMeasure
        rcases hphase with hph | ⟨hdr, hph⟩ <;> simp [hph] <;> omega

private theorem parse_header_spec (buf : Slice U8) (off : U32) (g : FlashGeometry) :
    parse_header buf off g ⦃ hp =>
      match hp with
      | .Ok hdr =>
        let a := max (max g.write_size.val g.read_size.val) 4
        hdr.record_len.val % a = 0 ∧
        16 ≤ hdr.record_len.val ∧
        hdr.record_len.val ≤ g.buf_cap.val ∧
        off.val + hdr.record_len.val ≤ g.region.val ∧
        hdr.payload_len.val ≤ 65535
      | _ => True ⦄ := by
  have hh := hdr_val
  obtain ⟨a, ha, ha_val⟩ := H5iAppLib.spec_exists_ok (align_eq_max g.write_size g.read_size)
  unfold parse_header
  by_cases hlt : Slice.len buf < HDR
  · simp [hlt]
  · simp only [hlt, ↓reduceIte]
    have hblen : 16 ≤ buf.length := by simp [UScalar.lt_equiv, Slice.len, hh] at hlt; omega
    obtain ⟨b, hb⟩ := H5iAppLib.ok_of (all_erased_spec buf 0#usize HDR)
    simp only [hb, bind_ok]
    cases b with
    | true => simp
    | false =>
      simp only [Bool.false_eq_true, ↓reduceIte]
      obtain ⟨_, h0, magic, _, _, _, _, h0_eq, _⟩ := H5iAppLib.spec_exists_ok (get_u32_le_spec buf 0#usize (by scalar_tac))
      subst h0_eq
      obtain ⟨_, h4, _, _, _, _, _, h4_eq, _⟩ := H5iAppLib.spec_exists_ok (get_u32_le_spec buf 4#usize (by scalar_tac))
      subst h4_eq
      obtain ⟨_, h8, _, _, _, h8_eq, _⟩ := H5iAppLib.spec_exists_ok (get_u16_le_spec buf 8#usize (by scalar_tac))
      subst h8_eq
      obtain ⟨_, h10, len16, _, _, h10_eq, _⟩ := H5iAppLib.spec_exists_ok (get_u16_le_spec buf 10#usize (by scalar_tac))
      subst h10_eq
      simp only [h0, h4, h8, h10, lift, ha, bind_ok]
      obtain ⟨orl, horl, horl_p, _⟩ := H5iAppLib.spec_exists_ok (record_len_spec (UScalar.cast .U32 len16) a)
      simp only [horl, bind_ok]
      cases orl with
      | none => rw [WP.spec_ok]; trivial
      | some rl =>
        have hrl_p := horl_p rl rfl
        by_cases hm : (magic != MAGIC) = true
        · simp [hm]
        · by_cases h1 : rl > g.buf_cap
          · simp [hm, h1]
          · by_cases h2 : off > g.region
            · simp [hm, h1, h2]
            · simp only [hm, h1, h2, ↓reduceIte]
              have hoff_le : off.val ≤ g.region.val := by simp [UScalar.lt_equiv, gt_iff_lt] at h2; omega
              obtain ⟨rem, hsub, hrem_val⟩ := u32_sub_ok (a := g.region) (b := off) hoff_le
              simp only [hsub, bind_ok]
              by_cases h3 : rl > rem
              · simp [h3]
              · simp only [h3, Bool.false_eq_true, ↓reduceIte]
                rw [WP.spec_ok]
                dsimp only
                refine ⟨by rw [← ha_val]; exact hrl_p.2.2.2.2.1, by omega,
                  by simp [UScalar.lt_equiv, gt_iff_lt] at h1; omega,
                  by simp [UScalar.lt_equiv, gt_iff_lt] at h3; omega,
                  by omega⟩

private theorem validate_record_crc_total (buf : Slice U8) (plen : U32) :
    validate_record_crc buf plen ⦃ _ => True ⦄ := by
  unfold validate_record_crc
  by_cases h16 : 16 ≤ buf.length
  · obtain ⟨o, hget⟩ := H5iAppLib.ok_of (get_u32_le_spec buf 12#usize (by scalar_tac))
    simp only [hget, bind_ok]
    cases o with
    | none => rw [WP.spec_ok]; trivial
    | some _ =>
      simp only [lift, bind_ok]
      obtain ⟨oc, hcrc⟩ := H5iAppLib.ok_of (record_crc_spec buf (UScalar.cast .Usize plen))
      simp only [hcrc, bind_ok]
      cases oc <;> (rw [WP.spec_ok]; trivial)
  · unfold get_u32_le
    have husize := H5iAppLib.usize_max_ge
    obtain ⟨s16, hs16, hs16_val⟩ := usize_add_ok (a := 12#usize) (b := 4#usize) (by scalar_tac)
    have hlt_len : buf.val.length < s16.val := by rw [slice_val_length]; scalar_tac
    simp [hs16, hlt_len, bind_ok]

private theorem accept_valid_record_safe_decr (g : FlashGeometry) (st : ScanState) (hdr : HeaderInfo)
    (hlay : layout_ok g = ok true) (hdiv : AlignDivides g) (hinv : ScanInv g st)
    (hphase : st.phase = .ReadHeader ∨ st.phase = .ReadBody hdr)
    (hhdr : let a := max (max g.write_size.val g.read_size.val) 4
            hdr.record_len.val % a = 0 ∧ 16 ≤ hdr.record_len.val ∧
            hdr.record_len.val ≤ g.buf_cap.val ∧ st.off.val + hdr.record_len.val ≤ g.region.val) :
    accept_valid_record g st hdr ⦃ res =>
      ScanInv g res.1 ∧ ScanActionSafe g res.2 ∧ scanMeasure g res.1 < scanMeasure g st ⦄ := by
  have hp := H5iAppLib.post_of_ok (layout_ok_spec g) hlay rfl
  have hcap_u32 : g.base.val + g.region.val ≤ U32.max := by
    have := hp.in_capacity; scalar_tac
  have hbase_rmod : g.base.val % g.read_size.val = 0 := mod_zero_of_dvd_mod_zero hp.base_aligned hdiv.2
  have hreg_rmod : g.region.val % g.read_size.val = 0 := mod_zero_of_dvd_mod_zero hp.region_aligned hdiv.2
  have hhdr_rmod : 16 % g.read_size.val = 0 := mod_zero_of_dvd_mod_zero hp.hdr_aligned hdiv.2
  have hh := hdr_u32_val
  have hbest_next : ∀ o1,
      (∀ b, o1 = some b → b.region.val ≤ 1 ∧ b.off.val % max (max g.write_size.val g.read_size.val) 4 = 0 ∧
        b.record_len.val % max (max g.write_size.val g.read_size.val) 4 = 0 ∧
        16 ≤ b.record_len.val ∧ b.record_len.val ≤ g.buf_cap.val ∧ b.off.val + b.record_len.val ≤ g.region.val) →
      (do
        let i ← st.off + hdr.record_len
        if i ≤ g.region then
          let i1 ← g.region - i
          if HDR_U32 ≤ i1 then
            let i2 ← region_start g st.region
            let addr ← i2 + i
            ok ({ st with off := i, best := o1, phase := .ReadHeader }, ScanAction.Read addr 0#u32 HDR_U32)
          else finish_region g { st with off := i, best := o1 } false
        else finish_region g { st with off := i, best := o1 } false) ⦃ res =>
        ScanInv g res.1 ∧ ScanActionSafe g res.2 ∧ scanMeasure g res.1 < scanMeasure g st ⦄ := by
    intro o1 ho1
    obtain ⟨nxt, hadd_i, hnxt_val⟩ := u32_add_ok (a := st.off) (b := hdr.record_len) (by scalar_tac)
    simp only [hadd_i, bind_tc_ok, bind_ok]
    have hle_reg : nxt ≤ g.region := by simp [UScalar.le_equiv]; omega
    simp only [hle_reg, ↓reduceIte]
    obtain ⟨rem, hsub_i1, hrem_val⟩ := u32_sub_ok (a := g.region) (b := nxt) (by omega)
    simp only [hsub_i1, bind_tc_ok, bind_ok]
    have hoff_amod : nxt.val % max (max g.write_size.val g.read_size.val) 4 = 0 := by
      rw [hnxt_val, Nat.add_mod, hinv.2.2.1, hhdr.1]; simp
    by_cases hhdr_le : HDR_U32 ≤ rem
    · simp only [hhdr_le, ↓reduceIte]
      obtain ⟨i2, hi2, hi2_val⟩ := H5iAppLib.spec_exists_ok (region_start_spec g st.region hcap_u32)
      have hfit16 : nxt.val + 16 ≤ g.region.val := by
        simp [UScalar.le_equiv, hh] at hhdr_le; omega
      have haddr_cap : i2.val + nxt.val + 16 ≤ g.capacity.val := by
        rw [hi2_val]; unfold regionStartNat; split <;> have := hp.in_capacity <;> omega
      obtain ⟨addr, hadd_addr, haddr_val⟩ := u32_add_ok (a := i2) (b := nxt) (by scalar_tac)
      simp only [hi2, hadd_addr, bind_ok]
      rw [WP.spec_ok]
      refine ⟨⟨hinv.1, by omega, hoff_amod, ho1, fun _ h => by cases h⟩, ?_, ?_⟩
      · simp only [ScanActionSafe, NorFlash.ReadValid, toFlashConfig]
        have hi2_rmod : i2.val % g.read_size.val = 0 := by
          rw [hi2_val]; unfold regionStartNat
          split
          · exact hbase_rmod
          · rw [Nat.add_mod, hbase_rmod, hreg_rmod]; simp
        have hoff_rmod : nxt.val % g.read_size.val = 0 :=
          mod_zero_of_dvd_mod_zero hoff_amod hdiv.2
        refine ⟨⟨by omega, ?_, by rw [hh]; exact hhdr_rmod⟩, by simp [hh]; exact hp.buf_ge16⟩
        rw [haddr_val, Nat.add_mod, hi2_rmod, hoff_rmod]; simp
      · unfold scanMeasure
        rcases hphase with hph | hph <;> simp [hph] <;> omega
    · simp only [hhdr_le, ↓reduceIte]
      have hf := finish_region_safe_decr g { st with off := nxt, best := o1, phase := .ReadHeader } false hlay hdiv
        ⟨hinv.1, by omega, hoff_amod, ho1, fun _ h => by cases h⟩ (Or.inl rfl)
      have heq_fr : finish_region g { st with off := nxt, best := o1 } false =
          finish_region g { st with off := nxt, best := o1, phase := .ReadHeader } false := by
        unfold finish_region
        by_cases h0 : st.region = 0#u32
        · simp [h0]
        · simp [h0]
      rw [heq_fr]
      refine WP.spec_mono hf (fun res ⟨hi', hs', hm'⟩ => ⟨hi', hs', ?_⟩)
      have hmeas_le : scanMeasure g { st with off := nxt, best := o1, phase := .ReadHeader } ≤ scanMeasure g st := by
        unfold scanMeasure
        rcases hphase with hph | hph <;> simp [hph] <;> omega
      omega
  let newBest : BestRecord :=
    { seq := hdr.seq, region := st.region, off := st.off, version := hdr.version,
      payload_len := hdr.payload_len, record_len := hdr.record_len }
  have hnew_ok : ∀ b, some newBest = some b →
      b.region.val ≤ 1 ∧ b.off.val % max (max g.write_size.val g.read_size.val) 4 = 0 ∧
      b.record_len.val % max (max g.write_size.val g.read_size.val) 4 = 0 ∧
      16 ≤ b.record_len.val ∧ b.record_len.val ≤ g.buf_cap.val ∧ b.off.val + b.record_len.val ≤ g.region.val := by
    intro b hb; cases hb; exact ⟨hinv.1, hinv.2.2.1, hhdr.1, hhdr.2.1, hhdr.2.2.1, hhdr.2.2.2⟩
  cases hst_best : st.best with
  | none =>
    obtain ⟨res, hres, hP⟩ := H5iAppLib.spec_exists_ok (hbest_next (some newBest) hnew_ok)
    have hacc_eq : accept_valid_record g st hdr = ok res := by
      unfold accept_valid_record
      apply H5iAppLib.bind_tc_eq_ok.mpr
      refine ⟨(none, hdr, true), by rw [hst_best], ?_⟩
      apply H5iAppLib.bind_tc_eq_ok.mpr
      refine ⟨(some newBest, hdr), rfl, hres⟩
    rw [hacc_eq, WP.spec_ok]
    exact hP
  | some prev =>
    obtain ⟨rep, hrep⟩ := H5iAppLib.ok_of (seq_newer_spec hdr.seq prev.seq)
    cases rep with
    | true =>
      obtain ⟨res, hres, hP⟩ := H5iAppLib.spec_exists_ok (hbest_next (some newBest) hnew_ok)
      have hacc_eq : accept_valid_record g st hdr = ok res := by
        unfold accept_valid_record
        apply H5iAppLib.bind_tc_eq_ok.mpr
        refine ⟨(some prev, hdr, true), ?_, ?_⟩
        · rw [hst_best]
          apply H5iAppLib.bind_tc_eq_ok.mpr
          exact ⟨true, hrep, rfl⟩
        · apply H5iAppLib.bind_tc_eq_ok.mpr
          refine ⟨(some newBest, hdr), rfl, hres⟩
      rw [hacc_eq, WP.spec_ok]
      exact hP
    | false =>
      obtain ⟨res, hres, hP⟩ := H5iAppLib.spec_exists_ok
        (hbest_next (some prev) (fun b hb => hinv.2.2.2.1 b (by rw [hst_best]; exact hb)))
      have hacc_eq : accept_valid_record g st hdr = ok res := by
        unfold accept_valid_record
        apply H5iAppLib.bind_tc_eq_ok.mpr
        refine ⟨(some prev, hdr, false), ?_, ?_⟩
        · rw [hst_best]
          apply H5iAppLib.bind_tc_eq_ok.mpr
          exact ⟨false, hrep, rfl⟩
        · apply H5iAppLib.bind_tc_eq_ok.mpr
          refine ⟨(some prev, hdr), rfl, hres⟩
      rw [hacc_eq, WP.spec_ok]
      exact hP

/-- **Safety & Bounded Termination of extracted `scan_step`**:
From any well-formed `ScanState` (`ScanInv g st`) with `st.phase ≠ .Done`,
extracted `scan_step`:
1. never panics,
2. preserves `ScanInv g`,
3. emits only valid, aligned, in-bounds `ScanAction.Read` requests (`ScanActionSafe`), and
4. strictly decreases `scanMeasure g st` (proving recovery terminates in at most
   `2 * (g.region + 2) + 2` I/O steps). -/
theorem scan_step_safe_and_terminates (g : FlashGeometry) (st : ScanState) (buf : Slice U8)
    (hlay : layout_ok g = ok true) (hdiv : AlignDivides g)
    (hinv : ScanInv g st) (hnd : st.phase ≠ .Done) :
    scan_step g st buf ⦃ res =>
      ScanInv g res.1 ∧ ScanActionSafe g res.2 ∧ scanMeasure g res.1 < scanMeasure g st ⦄ := by
  have hp := H5iAppLib.post_of_ok (layout_ok_spec g) hlay rfl
  have hcap_u32 : g.base.val + g.region.val ≤ U32.max := by
    have := hp.in_capacity; scalar_tac
  have hbase_rmod : g.base.val % g.read_size.val = 0 := mod_zero_of_dvd_mod_zero hp.base_aligned hdiv.2
  have hreg_rmod : g.region.val % g.read_size.val = 0 := mod_zero_of_dvd_mod_zero hp.region_aligned hdiv.2
  have hhdr_rmod : 16 % g.read_size.val = 0 := mod_zero_of_dvd_mod_zero hp.hdr_aligned hdiv.2
  have hh := hdr_u32_val
  unfold scan_step
  simp only [hlay, bind_tc_ok, bind_ok, ↓reduceIte]
  cases hph : st.phase with
  | Done => exact False.elim (hnd hph)
  | ReadHeader =>
    simp only [hph]
    obtain ⟨hres, hparse⟩ := H5iAppLib.ok_of (parse_header_spec buf st.off g)
    have hparse_p := H5iAppLib.post_of_ok (parse_header_spec buf st.off g) hparse
    simp only [hparse, bind_tc_ok, bind_ok]
    cases hres with
    | Erased => exact finish_region_safe_decr g st false hlay hdiv hinv (Or.inl hph)
    | Torn => exact finish_region_safe_decr g st true hlay hdiv hinv (Or.inl hph)
    | Ok hdr =>
      by_cases hgt : hdr.record_len > HDR_U32
      · simp only [hgt, ↓reduceIte]
        obtain ⟨i, hi, hi_val⟩ := H5iAppLib.spec_exists_ok (region_start_spec g st.region hcap_u32)
        have hgt_nat : 16 < hdr.record_len.val := by simpa [UScalar.lt_equiv, gt_iff_lt, hh] using hgt
        have haddr_cap : i.val + st.off.val + hdr.record_len.val ≤ g.capacity.val := by
          rw [hi_val]; unfold regionStartNat; split <;> have := hp.in_capacity <;> omega
        obtain ⟨i1, hi1_ok, hi1_val⟩ := u32_add_ok (a := i) (b := st.off) (by scalar_tac)
        obtain ⟨addr, haddr_ok, haddr_val⟩ := u32_add_ok (a := i1) (b := HDR_U32) (by scalar_tac)
        obtain ⟨blen, hblen_ok, hblen_val⟩ := u32_sub_ok (a := hdr.record_len) (b := HDR_U32) (by omega)
        simp only [hi, hi1_ok, haddr_ok, hblen_ok, bind_ok]
        rw [WP.spec_ok]
        refine ⟨⟨hinv.1, hinv.2.1, hinv.2.2.1, hinv.2.2.2.1, fun hdr' heq => by
          cases heq; exact ⟨hparse_p.1, hgt_nat, hparse_p.2.2.1, hparse_p.2.2.2.1⟩⟩, ?_, ?_⟩
        · simp only [ScanActionSafe, NorFlash.ReadValid, toFlashConfig]
          have hi_rmod : i.val % g.read_size.val = 0 := by
            rw [hi_val]; unfold regionStartNat
            split
            · exact hbase_rmod
            · rw [Nat.add_mod, hbase_rmod, hreg_rmod]; simp
          have hoff_rmod : st.off.val % g.read_size.val = 0 := mod_zero_of_dvd_mod_zero hinv.2.2.1 hdiv.2
          have hrl_rmod : hdr.record_len.val % g.read_size.val = 0 := mod_zero_of_dvd_mod_zero hparse_p.1 hdiv.2
          have hsub_rmod : blen.val % g.read_size.val = 0 := by
            rw [hblen_val, hh]
            have d1 : g.read_size.val ∣ hdr.record_len.val := Nat.dvd_of_mod_eq_zero hrl_rmod
            have d2 : g.read_size.val ∣ 16 := Nat.dvd_of_mod_eq_zero hhdr_rmod
            exact Nat.mod_eq_zero_of_dvd (Nat.dvd_sub d1 d2)
          refine ⟨⟨by omega, ?_, hsub_rmod⟩, by omega⟩
          rw [haddr_val, hi1_val, hh, Nat.add_mod (i.val + st.off.val) 16,
              Nat.add_mod i.val st.off.val, hi_rmod, hoff_rmod, hhdr_rmod]; simp
        · unfold scanMeasure; simp [hph]
      · simp only [hgt, ↓reduceIte]
        obtain ⟨b1, hb1⟩ := H5iAppLib.ok_of (validate_record_crc_total buf hdr.payload_len)
        simp only [hb1, bind_ok]
        cases b1 with
        | true =>
          simp only [↓reduceIte]
          exact accept_valid_record_safe_decr g st hdr hlay hdiv hinv (Or.inl hph)
            ⟨hparse_p.1, hparse_p.2.1, hparse_p.2.2.1, hparse_p.2.2.2.1⟩
        | false =>
          simp only [Bool.false_eq_true, ↓reduceIte]
          exact finish_region_safe_decr g st true hlay hdiv hinv (Or.inl hph)
  | ReadBody hdr =>
    have hhdr := hinv.2.2.2.2 hdr hph
    obtain ⟨b1, hb1⟩ := H5iAppLib.ok_of (validate_record_crc_total buf hdr.payload_len)
    simp only [hb1, bind_ok]
    cases b1 with
    | true =>
      simp only [↓reduceIte]
      exact accept_valid_record_safe_decr g st hdr hlay hdiv hinv (Or.inr hph)
        ⟨hhdr.1, by omega, hhdr.2.2.1, hhdr.2.2.2⟩
    | false =>
      simp only [Bool.false_eq_true, ↓reduceIte]
      exact finish_region_safe_decr g st true hlay hdiv hinv (Or.inr ⟨hdr, hph⟩)
  | ReloadBest =>
    cases hbest : st.best with
    | none =>
      rw [WP.spec_ok]
      refine ⟨⟨hinv.1, hinv.2.1, hinv.2.2.1, (fun _ h => by cases h), (fun _ h => by cases h)⟩,
        trivial, by unfold scanMeasure; simp [hph]⟩
    | some b1 =>
      have hb1 := hinv.2.2.2.1 b1 hbest
      have hpos_ok : scan_result_pos { st with best := some b1, phase := .Done } b1 ⦃ _ => True ⦄ := by
        unfold scan_result_pos
        by_cases hb0 : b1.region = 0#u32
        · simp only [hb0, ↓reduceIte, bind_ok]
          step*
        · simp only [hb0, ↓reduceIte, bind_ok]
          step*
      obtain ⟨pos', hpos'⟩ := H5iAppLib.ok_of hpos_ok
      obtain ⟨b2, hb2⟩ := H5iAppLib.ok_of (validate_record_crc_total buf b1.payload_len)
      simp only [hb2, bind_ok]
      cases b2 <;> (
        simp only [↓reduceIte, Bool.false_eq_true, hpos', bind_ok]
        rw [WP.spec_ok]
        exact ⟨⟨hinv.1, hinv.2.1, hinv.2.2.1, (fun b hb => hinv.2.2.2.1 b (by rw [hbest]; exact hb)), (fun _ h => by cases h)⟩,
          trivial, by unfold scanMeasure; simp [hph]⟩)

/-! ## 6. Compositional Crash Consistency & Durability over `NorFlash` -/

/-- Convert an Aeneas `Slice U8` prefix `buf[0..n]` to `List UInt8` for `NorFlash.writeBytes`. -/
def slicePrefixToBytes (buf : Slice U8) (n : Nat) : List UInt8 :=
  (buf.val.take n).map (fun u => UInt8.ofNat u.val)

theorem slicePrefixToBytes_length (buf : Slice U8) (n : Nat) (hn : n ≤ buf.length) :
    (slicePrefixToBytes buf n).length = n := by
  have := slice_val_length buf
  simp [slicePrefixToBytes]
  omega

/-- A committed record stored on physical NOR flash `s`. -/
structure CommittedRecord where
  region : U32
  off : U32
  seq : U32
  version : U16
  payloadLen : U32
  recordLen : U32
  bytes : List UInt8

/-- Predicate: `rec` is intact on physical NOR flash `s` at `regionStartNat g rec.region + rec.off`. -/
def RecordIntact (g : FlashGeometry) (s : NorFlash.FlashState) (rec : CommittedRecord) : Prop :=
  rec.region.val ≤ 1 ∧
  rec.off.val + rec.recordLen.val ≤ g.region.val ∧
  rec.bytes.length = rec.recordLen.val ∧
  NorFlash.readSlice s (regionStartNat g rec.region.val + rec.off.val) rec.recordLen.val = rec.bytes

/-- **Standby-erase isolation**: during an extracted `EraseThenWrite`, ANY
`NorFlashStep` (`eraseOk` OR `eraseFailPartial`) preserves every byte of the
active region `pos.region`, hence keeps any committed record in `pos.region`
100% intact across power loss during erase. -/
theorem save_erase_preserves_active_region
    (g : FlashGeometry) (pos : Pos) (payload_len : U32)
    (efrom eto tr waddr wlen nseq : U32)
    (hlay : layout_ok g = ok true) (hdiv : AlignDivides g) (hreg : pos.region.val ≤ 1)
    (hplan : plan_save g pos payload_len = ok (.EraseThenWrite efrom eto tr waddr wlen nseq))
    {s s' : NorFlash.FlashState}
    (herase : NorFlash.NorFlashStep (toFlashConfig g) (.erase efrom.val eto.val) s s') :
    NorFlash.agreesOn s' s (regionStartNat g pos.region.val) g.region.val := by
  have hp := H5iAppLib.post_of_ok (plan_save_safe g pos payload_len hlay hdiv hreg) hplan
  exact NorFlash.step_erase_disjoint_preserved herase hp.2.2.2.2.2.1

/-- **Committed-record write isolation**: during either `WriteOnly` (append after
`committed.off + committed.recordLen ≤ toff`) or `EraseThenWrite` (write in the
opposite region), ANY `NorFlashStep` (`writeOk` OR `writeFailPartial`) leaves
every byte of the previously committed record untouched! -/
theorem save_write_preserves_committed_record
    (g : FlashGeometry) (pos : Pos) (payload_len : U32) (committed : CommittedRecord)
    (hlay : layout_ok g = ok true) (hdiv : AlignDivides g) (hreg : pos.region.val ≤ 1)
    (hcomm_reg : committed.region = pos.region)
    (hcomm_intact : RecordIntact g s committed)
    (hcomm_before_append : ∀ off, pos.append = some off → committed.off.val + committed.recordLen.val ≤ off.val)
    {waddr wlen : U32} {bytes : List UInt8} {s' : NorFlash.FlashState}
    (hlen_eq : bytes.length = wlen.val)
    (hplan : (∃ tr toff nseq, plan_save g pos payload_len = ok (.WriteOnly tr toff waddr wlen nseq)) ∨
             (∃ efrom eto tr nseq, plan_save g pos payload_len = ok (.EraseThenWrite efrom eto tr waddr wlen nseq)))
    (hwrite : NorFlash.NorFlashStep (toFlashConfig g) (.write waddr.val bytes) s s') :
    RecordIntact g s' committed := by
  have hdisj : NorFlash.disjoint waddr.val bytes.length
      (regionStartNat g committed.region.val + committed.off.val) committed.recordLen.val := by
    rw [hlen_eq, hcomm_reg]
    rcases hplan with ⟨tr, toff, nseq, hp_eq⟩ | ⟨efrom, eto, tr, nseq, hp_eq⟩
    · have hp := H5iAppLib.post_of_ok (plan_save_safe g pos payload_len hlay hdiv hreg) hp_eq
      have hbef := hcomm_before_append toff hp.2.1
      have hwa_eq : waddr.val = regionStartNat g pos.region.val + toff.val := by
        rw [hp.2.2.1, hp.1]
      exact Or.inr (by omega)
    · have hp := H5iAppLib.post_of_ok (plan_save_safe g pos payload_len hlay hdiv hreg) hp_eq
      have hd := hp.2.2.2.2.2.2.2.2.1
      have hcomm_in := hcomm_intact.2.1
      rcases hd with h | h
      · exact Or.inl (by omega)
      · exact Or.inr (by omega)
  have hag := NorFlash.step_write_disjoint_preserved hwrite hdisj
  refine ⟨hcomm_intact.1, hcomm_intact.2.1, hcomm_intact.2.2.1, ?_⟩
  rw [NorFlash.readSlice_eq_of_agreesOn hag]
  exact hcomm_intact.2.2.2

/-- **End-to-end durability of a completed save**: when `encode_record` frames
a record and `NorFlash.writeBytes` writes it into an erased slot, reading the
slot back from physical NOR flash reproduces the exact framed bytes, passes
extracted `parse_header` and `validate_record_crc`, and has `seq = next_seq pos.seq`
which is strictly newer than `pos.seq` via `seq_newer_next_seq` (including
across `u32::MAX → 0` wraparound!). -/
theorem save_write_ok_durable_and_newer
    (g : FlashGeometry) (s : NorFlash.FlashState) (buf : Slice U8)
    (pos : Pos) (version : U16) (payload_len target_region target_off a rl : U32)
    (hlay : layout_ok g = ok true)
    (ha : align g.write_size g.read_size = ok a)
    (hrl : record_len payload_len a = ok (some rl))
    (hbuf : buf.length = g.buf_cap.val)
    (hrl_buf : rl.val ≤ g.buf_cap.val)
    (htr : target_region.val ≤ 1)
    (htoff : target_off.val + rl.val ≤ g.region.val)
    (herased : NorFlash.isErasedRange s (regionStartNat g target_region.val + target_off.val) rl.val) :
    ∃ nseq buf' rec,
      next_seq pos.seq = ok nseq ∧
      seq_newer nseq pos.seq = ok true ∧
      encode_record buf nseq version payload_len a = ok (some rl, buf') ∧
      rec = {
        region := target_region, off := target_off, seq := nseq,
        version := version, payloadLen := payload_len, recordLen := rl,
        bytes := slicePrefixToBytes buf' rl.val : CommittedRecord
      } ∧
      RecordIntact g (NorFlash.writeBytes s (regionStartNat g target_region.val + target_off.val) rec.bytes) rec ∧
      parse_header buf' target_off g = ok (.Ok { seq := nseq, version, payload_len, record_len := rl }) ∧
      validate_record_crc buf' payload_len = ok true := by
  obtain ⟨nseq, hns, hnewer⟩ := seq_newer_next_seq pos.seq
  obtain ⟨buf', henc, hlen', _, hparse, hvcrc⟩ :=
    encode_record_roundtrip g buf nseq version payload_len target_off a rl hlay ha hrl hbuf hrl_buf htoff
  let bytes := slicePrefixToBytes buf' rl.val
  have hblen : bytes.length = rl.val := slicePrefixToBytes_length buf' rl.val (by omega)
  let crec : CommittedRecord := {
    region := target_region, off := target_off, seq := nseq,
    version := version, payloadLen := payload_len, recordLen := rl,
    bytes := bytes
  }
  have hintact : RecordIntact g (NorFlash.writeBytes s (regionStartNat g target_region.val + target_off.val) bytes) crec := by
    refine ⟨htr, htoff, hblen, ?_⟩
    have hwr := NorFlash.writeBytes_in_erased (s := s) (addr := regionStartNat g target_region.val + target_off.val)
      (bytes := bytes) (by rw [hblen]; exact herased)
    simpa [crec, hblen] using hwr
  exact ⟨nseq, buf', crec, hns, hnewer, henc, rfl, hintact, hparse, hvcrc⟩

/-! ## 7. Physical Recovery Correctness (`scan_init` / `scan_step` over `NorFlash`) -/

/-- Total `UInt8 → U8` conversion without requiring a proof argument at call sites. -/
def u8OfUInt8 (b : UInt8) : U8 :=
  UScalar.ofNatCore b.toNat (by change b.toNat < 256; exact b.toNat_lt)

/-- Total `Nat → U32` conversion (`n % 2^32`) without requiring a proof argument at call sites. -/
def u32OfNat (n : Nat) : U32 :=
  UScalar.ofNatCore (n % 4294967296) (Nat.mod_lt n (by decide))

private theorem u8OfUInt8_val (b : UInt8) : (u8OfUInt8 b).val = b.toNat := by
  exact UScalar.ofNatCore_val_eq (ty := .U8) (x := b.toNat) (by change b.toNat < 256; exact b.toNat_lt)

private theorem u32OfNat_val {n : Nat} (hn : n < U32.size) : (u32OfNat n).val = n := by
  have hn' : n < 4294967296 := by scalar_tac
  have h := UScalar.ofNatCore_val_eq (ty := .U32) (x := n % 4294967296) (Nat.mod_lt n (by decide))
  unfold u32OfNat
  simpa [Nat.mod_eq_of_lt hn'] using h

private theorem u8_uint8_roundtrip (u : U8) :
    u8OfUInt8 (UInt8.ofNat u.val) = u := by
  apply UScalar.eq_of_val_eq
  have hu : u.val < 256 := by scalar_tac
  rw [u8OfUInt8_val]
  simp [UInt8.toNat_ofNat, Nat.mod_eq_of_lt hu]

private theorem u8_ofNat_255 : u8OfUInt8 NorFlash.erasedByte = 255#u8 := by
  apply UScalar.eq_of_val_eq
  rw [u8OfUInt8_val]
  rfl

private theorem slicePrefixToBytes_getD (buf : Slice U8) (n i : Nat)
    (hn : n ≤ buf.length) (hi : i < n) :
    some (u8OfUInt8 ((slicePrefixToBytes buf n).getD i 0)) = buf.val[i]? := by
  have hvl := slice_val_length buf
  have hi_lt : i < buf.val.length := by omega
  have hlen : i < (slicePrefixToBytes buf n).length := by
    rw [slicePrefixToBytes_length buf n hn]; exact hi
  have hget : (slicePrefixToBytes buf n).getD i 0 = UInt8.ofNat (buf.val[i]'hi_lt).val := by
    rw [List.getD_eq_getElem?_getD, List.getElem?_eq_getElem hlen, Option.getD_some]
    simp [slicePrefixToBytes]
  rw [hget, u8_uint8_roundtrip, List.getElem?_eq_getElem hi_lt]

private theorem recordIntact_mem_eq (g : FlashGeometry) (s : NorFlash.FlashState)
    (rec : CommittedRecord) (hintact : RecordIntact g s rec) {i : Nat} (hi : i < rec.recordLen.val) :
    s.mem (regionStartNat g rec.region.val + rec.off.val + i) = rec.bytes.getD i 0 := by
  have hslice := hintact.2.2.2
  have hblen := hintact.2.2.1
  have hlt1 : i < (NorFlash.readSlice s (regionStartNat g rec.region.val + rec.off.val) rec.recordLen.val).length := by
    rw [NorFlash.readSlice_length]; exact hi
  have hlt2 : i < rec.bytes.length := by omega
  have h1 : (NorFlash.readSlice s (regionStartNat g rec.region.val + rec.off.val) rec.recordLen.val)[i]'hlt1 =
      rec.bytes[i]'hlt2 := by
    simp [hslice]
  have h2 : (NorFlash.readSlice s (regionStartNat g rec.region.val + rec.off.val) rec.recordLen.val)[i]'hlt1 =
      s.mem (regionStartNat g rec.region.val + rec.off.val + i) := by
    simp [NorFlash.readSlice]
  rw [← h2, h1, List.getD_eq_getElem?_getD, List.getElem?_eq_getElem hlt2]
  rfl

private theorem get_u32_le_congr (bA bB : Slice U8) (o : Usize)
    (hbo : o.val + 4 ≤ bA.length) (hbl : bB.length = bA.length)
    (hbe : ∀ j, o.val ≤ j → j < o.val + 4 → bB.val[j]? = bA.val[j]?) :
    get_u32_le bB o = get_u32_le bA o := by
  obtain ⟨rA, hA, vA, a0, a1, a2, a3, hrA, ha0, ha1, ha2, ha3, hvA⟩ :=
    H5iAppLib.spec_exists_ok (get_u32_le_spec bA o hbo)
  subst hrA
  obtain ⟨rB, hB, vB, b0, b1, b2, b3, hrB, hb0, hb1, hb2, hb3, hvB⟩ :=
    H5iAppLib.spec_exists_ok (get_u32_le_spec bB o (by omega))
  subst hrB
  have e0 : b0 = a0 := by have := hbe o.val (by omega) (by omega); rw [hb0, ha0] at this; exact Option.some.inj this
  have e1 : b1 = a1 := by have := hbe (o.val + 1) (by omega) (by omega); rw [hb1, ha1] at this; exact Option.some.inj this
  have e2 : b2 = a2 := by have := hbe (o.val + 2) (by omega) (by omega); rw [hb2, ha2] at this; exact Option.some.inj this
  have e3 : b3 = a3 := by have := hbe (o.val + 3) (by omega) (by omega); rw [hb3, ha3] at this; exact Option.some.inj this
  have heq : vB = vA := UScalar.eq_of_val_eq (by rw [hvB, hvA, e0, e1, e2, e3])
  rw [hB, hA, heq]

private theorem get_u16_le_congr (bA bB : Slice U8) (o : Usize)
    (hbo : o.val + 2 ≤ bA.length) (hbl : bB.length = bA.length)
    (hbe : ∀ j, o.val ≤ j → j < o.val + 2 → bB.val[j]? = bA.val[j]?) :
    get_u16_le bB o = get_u16_le bA o := by
  obtain ⟨rA, hA, vA, a0, a1, hrA, ha0, ha1, hvA⟩ :=
    H5iAppLib.spec_exists_ok (get_u16_le_spec bA o hbo)
  subst hrA
  obtain ⟨rB, hB, vB, b0, b1, hrB, hb0, hb1, hvB⟩ :=
    H5iAppLib.spec_exists_ok (get_u16_le_spec bB o (by omega))
  subst hrB
  have e0 : b0 = a0 := by have := hbe o.val (by omega) (by omega); rw [hb0, ha0] at this; exact Option.some.inj this
  have e1 : b1 = a1 := by have := hbe (o.val + 1) (by omega) (by omega); rw [hb1, ha1] at this; exact Option.some.inj this
  have heq : vB = vA := UScalar.eq_of_val_eq (by rw [hvB, hvA, e0, e1])
  rw [hB, hA, heq]

private theorem all_erased_hdr_congr (bA bB : Slice U8)
    (h16 : 16 ≤ bA.length) (hbl : bB.length = bA.length)
    (hbe : ∀ j, j < 16 → bB.val[j]? = bA.val[j]?) :
    all_erased bB 0#usize HDR = all_erased bA 0#usize HDR := by
  have hh := hdr_val
  obtain ⟨rA, hA, hpA⟩ := H5iAppLib.spec_exists_ok (all_erased_spec bA 0#usize HDR)
  obtain ⟨rB, hB, hpB⟩ := H5iAppLib.spec_exists_ok (all_erased_spec bB 0#usize HDR)
  rw [hB, hA]
  congr 1
  have hiff : rB = true ↔ rA = true := by
    rw [hpB, hpA]
    constructor
    · rintro ⟨h1, hall⟩
      refine ⟨by omega, fun i b' hi1 hi2 hg => hall i b' hi1 hi2 ?_⟩
      rw [hbe i (by rw [hh] at hi2; exact hi2)]; exact hg
    · rintro ⟨h1, hall⟩
      refine ⟨by omega, fun i b' hi1 hi2 hg => hall i b' hi1 hi2 ?_⟩
      rw [← hbe i (by rw [hh] at hi2; exact hi2)]; exact hg
  cases rA <;> cases rB <;> simp at hiff ⊢

private theorem parse_header_congr (bA bB : Slice U8) (off : U32) (g : FlashGeometry)
    (h16 : 16 ≤ bA.length) (hbl : bB.length = bA.length)
    (hbe : ∀ j, j < 16 → bB.val[j]? = bA.val[j]?) :
    parse_header bB off g = parse_header bA off g := by
  have hh := hdr_val
  have hvlA := slice_val_length bA
  have hvlB := slice_val_length bB
  have hnltA : ¬ (Slice.len bA < HDR) := by simp [UScalar.lt_equiv, Slice.len, hh]; omega
  have hnltB : ¬ (Slice.len bB < HDR) := by simp [UScalar.lt_equiv, Slice.len, hh]; omega
  have hae := all_erased_hdr_congr bA bB h16 hbl hbe
  have hg0 := get_u32_le_congr bA bB 0#usize (by scalar_tac) hbl (fun j _ h2 => hbe j (by scalar_tac))
  have hg4 := get_u32_le_congr bA bB 4#usize (by scalar_tac) hbl (fun j _ h2 => hbe j (by scalar_tac))
  have hg8 := get_u16_le_congr bA bB 8#usize (by scalar_tac) hbl (fun j _ h2 => hbe j (by scalar_tac))
  have hg10 := get_u16_le_congr bA bB 10#usize (by scalar_tac) hbl (fun j _ h2 => hbe j (by scalar_tac))
  unfold parse_header
  simp only [hnltA, hnltB, ↓reduceIte, hae, hg0, hg4, hg8, hg10]

private theorem parse_header_of_all_erased (buf : Slice U8) (off : U32) (g : FlashGeometry)
    (h16 : 16 ≤ buf.length)
    (hall : ∀ i, i < 16 → buf.val[i]? = some 255#u8) :
    parse_header buf off g = ok .Erased := by
  have hh := hdr_val
  have hvl := slice_val_length buf
  unfold parse_header
  have hnlt : ¬ (Slice.len buf < HDR) := by simp [UScalar.lt_equiv, Slice.len, hh]; omega
  simp only [hnlt, ↓reduceIte]
  obtain ⟨eb, heb, hiff⟩ := H5iAppLib.spec_exists_ok (all_erased_spec buf 0#usize HDR)
  have htrue : eb = true := hiff.mpr ⟨by omega, fun i b' _ hi2 hg => by
    have h255 := hall i (by rw [hh] at hi2; exact hi2)
    rw [h255, Option.some.injEq] at hg
    subst hg; rfl⟩
  subst htrue
  simp only [heb, bind_ok, ↓reduceIte]

private theorem validate_record_crc_congr (bA bB : Slice U8) (plen : U32)
    (hplen : plen.val ≤ 65535) (hfit : 16 + plen.val ≤ bA.length) (hbl : bB.length = bA.length)
    (hbe : ∀ j, j < 16 + plen.val → bB.val[j]? = bA.val[j]?) :
    validate_record_crc bB plen = validate_record_crc bA plen := by
  unfold validate_record_crc
  rw [get_u32_le_congr bA bB 12#usize (by scalar_tac) hbl (fun j _ h2 => hbe j (by scalar_tac))]
  obtain ⟨o, ho⟩ := H5iAppLib.ok_of (get_u32_le_spec bA 12#usize (by scalar_tac))
  simp only [ho, bind_ok]
  cases o with
  | none => rfl
  | some stored =>
    simp only [lift, bind_ok]
    have hpc : (UScalar.cast UScalarTy.Usize plen).val = plen.val := by scalar_tac
    rw [record_crc_congr bB bA (UScalar.cast .Usize plen) (by omega) (by omega) (by omega)
      (fun j hj1 hj2 => hbe j (by omega))
      (fun j hj1 hj2 => hbe j (by omega))]

/-- `ValidRecordBytes g rec`: `rec` carries the exact framed bytes of an
extracted `encode_record` output, so loading its bytes from physical NOR flash
into any `g.buf_cap`-sized scratch buffer passes extracted `parse_header` and
`validate_record_crc`. -/
def ValidRecordBytes (g : FlashGeometry) (rec : CommittedRecord) : Prop :=
  let a := max (max g.write_size.val g.read_size.val) 4
  rec.region.val ≤ 1 ∧
  rec.off.val % a = 0 ∧
  rec.recordLen.val % a = 0 ∧
  16 ≤ rec.recordLen.val ∧
  rec.recordLen.val ≤ g.buf_cap.val ∧
  rec.off.val + rec.recordLen.val ≤ g.region.val ∧
  16 + rec.payloadLen.val ≤ rec.recordLen.val ∧
  rec.payloadLen.val ≤ 65535 ∧
  rec.bytes.length = rec.recordLen.val ∧
  (∀ (buf : Slice U8), buf.length = g.buf_cap.val →
    (∀ i, i < 16 → buf.val[i]? = some (u8OfUInt8 (rec.bytes.getD i 0))) →
    parse_header buf rec.off g =
      ok (.Ok { seq := rec.seq, version := rec.version,
                payload_len := rec.payloadLen, record_len := rec.recordLen })) ∧
  (∀ (buf : Slice U8), buf.length = g.buf_cap.val →
    (∀ i, i < rec.recordLen.val → buf.val[i]? = some (u8OfUInt8 (rec.bytes.getD i 0))) →
    validate_record_crc buf rec.payloadLen = ok true)

theorem validRecordBytes_of_encode_record
    (g : FlashGeometry) (buf buf' : Slice U8) (reg off seq : U32) (version : U16)
    (payload_len a rl : U32)
    (hlay : layout_ok g = ok true)
    (ha : align g.write_size g.read_size = ok a)
    (hrl : record_len payload_len a = ok (some rl))
    (hbuf : buf.length = g.buf_cap.val)
    (hrl_buf : rl.val ≤ g.buf_cap.val)
    (hreg : reg.val ≤ 1)
    (hoff_al : off.val % max (max g.write_size.val g.read_size.val) 4 = 0)
    (hoff_fit : off.val + rl.val ≤ g.region.val)
    (henc : encode_record buf seq version payload_len a = ok (some rl, buf')) :
    ValidRecordBytes g {
      region := reg, off := off, seq := seq, version := version,
      payloadLen := payload_len, recordLen := rl,
      bytes := slicePrefixToBytes buf' rl.val
    } := by
  have ha_val := H5iAppLib.post_of_ok (align_eq_max g.write_size g.read_size) ha
  have hrl_p := (H5iAppLib.post_of_ok (record_len_spec payload_len a) hrl).1 rl rfl
  obtain ⟨buf'', henc'', hlen'', _, hparse'', hvcrc''⟩ :=
    encode_record_roundtrip g buf seq version payload_len off a rl hlay ha hrl hbuf hrl_buf hoff_fit
  rw [henc, Result.ok.injEq, Prod.mk.injEq] at henc''
  have hbuf_eq : buf'' = buf' := henc''.2.symm
  rw [hbuf_eq] at hlen'' hparse'' hvcrc''
  have hlen' : buf'.length = g.buf_cap.val := by rw [hlen'', hbuf]
  have hblen : (slicePrefixToBytes buf' rl.val).length = rl.val :=
    slicePrefixToBytes_length buf' rl.val (by omega)
  have hrl_ge_pay : 16 + payload_len.val ≤ rl.val := hrl_p.2.2.2.1
  have hrl_ge16 : 16 ≤ rl.val := by omega
  refine ⟨hreg, hoff_al, by rw [← ha_val]; exact hrl_p.2.2.2.2.1,
    hrl_ge16, hrl_buf, hoff_fit, hrl_ge_pay, hrl_p.1, hblen, ?_, ?_⟩
  · intro b1 hb1 hbytes
    have hcongr : parse_header b1 off g = parse_header buf' off g :=
      parse_header_congr buf' b1 off g (by omega) (by omega) (fun j hj => by
        rw [hbytes j hj, slicePrefixToBytes_getD buf' rl.val j (by omega) (by omega)])
    rw [hcongr]; exact hparse''
  · intro b1 hb1 hbytes
    have hcongr : validate_record_crc b1 payload_len = validate_record_crc buf' payload_len :=
      validate_record_crc_congr buf' b1 payload_len hrl_p.1 (by omega) (by omega) (fun j hj => by
        rw [hbytes j (Nat.lt_of_lt_of_le hj hrl_ge_pay), slicePrefixToBytes_getD buf' rl.val j (by omega) (Nat.lt_of_lt_of_le hj hrl_ge_pay)])
    rw [hcongr]; exact hvcrc''

/-- Exact mathematical model of `self.flash.read(addr, &mut self.buf[bufOff..bufOff+len])`:
preserves buffer length, loads bytes from physical `NorFlash.FlashState` into
`[bufOff, bufOff + len)`, and leaves bytes outside that range unchanged. -/
def BufAfterRead (s : NorFlash.FlashState) (addr bufOff len : Nat) (buf buf' : Slice U8) : Prop :=
  buf'.length = buf.length ∧
  (∀ i, bufOff ≤ i → i < bufOff + len →
    buf'.val[i]? = some (u8OfUInt8 (s.mem (addr + (i - bufOff))))) ∧
  (∀ i, (i < bufOff ∨ bufOff + len ≤ i) → buf'.val[i]? = buf.val[i]?)

private theorem bufAfterRead_exists (s : NorFlash.FlashState) (addr bufOff len : Nat)
    (buf : Slice U8) (hfit : bufOff + len ≤ buf.length) :
    ∃ buf' : Slice U8, BufAfterRead s addr bufOff len buf buf' := by
  induction len with
  | zero => exact ⟨buf, rfl, fun _ h1 h2 => by omega, fun _ _ => rfl⟩
  | succ k ih =>
    obtain ⟨buf1, hlen1, hin1, hout1⟩ := ih (by omega)
    have hvl1 := slice_val_length buf1
    have hidx_lt : bufOff + k < buf1.length := by omega
    have hidx_max : bufOff + k ≤ Usize.max := by
      have := buf1.property; omega
    let idx : Usize := UScalar.ofNatCore (bufOff + k) (by scalar_tac)
    have hidx_val : idx.val = bufOff + k := UScalar.ofNatCore_val_eq (by scalar_tac)
    have hidx_in : idx.val < buf1.length := by rw [hidx_val]; exact hidx_lt
    let val : U8 := u8OfUInt8 (s.mem (addr + k))
    let buf2 : Slice U8 := buf1.set idx val
    have hbuf2_val : buf2.val = buf1.val.set idx.val val := by simp [buf2, Slice.set, Slice.setAtNat]
    have hlen2 : buf2.length = buf.length := by
      have hvl2 := slice_val_length buf2
      rw [← hvl2, hbuf2_val, List.length_set, hvl1, hlen1]
    refine ⟨buf2, hlen2, ?_, ?_⟩
    · intro i hi1 hi2
      by_cases heq : i = bufOff + k
      · subst heq
        rw [hbuf2_val, hidx_val, List.getElem?_set_self (by omega)]
        simp [val]
      · have hne : idx.val ≠ i := by omega
        rw [hbuf2_val, List.getElem?_set_ne hne]
        exact hin1 i hi1 (by omega)
    · intro i hout
      have hne : idx.val ≠ i := by rcases hout with h | h <;> omega
      rw [hbuf2_val, List.getElem?_set_ne hne]
      exact hout1 i (by rcases hout with h | h; exact Or.inl h; exact Or.inr (by omega))

/-- Multi-step execution of the **actual Aeneas-extracted** `store_core.scan_step`
state machine reading directly from physical NOR flash `s` (mirroring
`FlashStore::scan` in `store.rs`). -/
inductive ScanExec (g : FlashGeometry) (s : NorFlash.FlashState) :
    ScanState → ScanAction → Slice U8 → ScanState → ScanAction → Slice U8 → Prop where
  | done {st : ScanState} {pos : Pos} {ver : Option U16} {plen : U32} {buf : Slice U8} :
      ScanExec g s st (.Done pos ver plen) buf st (.Done pos ver plen) buf
  | stepRead {st st1 st2 : ScanState} {act1 act2 : ScanAction}
      {addr bufOff len : U32} {buf buf1 buf2 : Slice U8}
      (hvalid : NorFlash.ReadValid (toFlashConfig g) addr.val len.val)
      (hbuf_in : bufOff.val + len.val ≤ buf.length)
      (hread : BufAfterRead s addr.val bufOff.val len.val buf buf1)
      (hstep : scan_step g st buf1 = ok (st1, act1))
      (hrest : ScanExec g s st1 act1 buf1 st2 act2 buf2) :
      ScanExec g s st (.Read addr bufOff len) buf st2 act2 buf2

/-- Full physical recovery run: executes extracted `scan_init g` followed by
`ScanExec` over physical NOR flash `s` until `ScanAction.Done`. -/
def RecoverFromFlash (g : FlashGeometry) (s : NorFlash.FlashState) (buf0 : Slice U8)
    (pos : Pos) (bestVer : Option U16) (bestLen : U32) (finalBuf : Slice U8) : Prop :=
  ∃ st0 act0 stF,
    scan_init g = ok (st0, act0) ∧
    ScanExec g s st0 act0 buf0 stF (.Done pos bestVer bestLen) finalBuf

/-- State of the remaining bytes after the last valid record in a region. -/
inductive RegionTail where
  | erased
  | torn
deriving DecidableEq

/-- A valid record on flash tagged with its monotone logical sequence counter `idx`. -/
structure LogEntry where
  idx : Nat
  record : CommittedRecord

/-- Contiguous sequence of valid, intact records in region `r` on physical flash `s`,
starting at `startOff` and ending at `endOff`. -/
inductive ContiguousLog (g : FlashGeometry) (s : NorFlash.FlashState) (r : Nat) :
    Nat → List LogEntry → Nat → Prop where
  | nil (off : Nat) (hle : off ≤ g.region.val)
      (hal : off % max (max g.write_size.val g.read_size.val) 4 = 0) :
      ContiguousLog g s r off [] off
  | cons {off nextOff endOff : Nat} {e : LogEntry} {rest : List LogEntry}
      (hreg : e.record.region.val = r)
      (hoff : e.record.off.val = off)
      (hidx : e.record.seq.val = e.idx % 4294967296)
      (hvalid : ValidRecordBytes g e.record)
      (hintact : RecordIntact g s e.record)
      (hnext : nextOff = off + e.record.recordLen.val)
      (hrest : ContiguousLog g s r nextOff rest endOff) :
      ContiguousLog g s r off (e :: rest) endOff

private theorem contiguousLog_bounds {g : FlashGeometry} {s : NorFlash.FlashState} {r startOff endOff : Nat}
    {entries : List LogEntry} (hlog : ContiguousLog g s r startOff entries endOff) :
    startOff ≤ endOff ∧ endOff ≤ g.region.val ∧
    endOff % max (max g.write_size.val g.read_size.val) 4 = 0 ∧
    entries.length * 16 ≤ endOff - startOff := by
  induction hlog with
  | nil off hle hal => exact ⟨Nat.le_refl _, hle, hal, by simp⟩
  | cons _ _ _ hvalid _ hnext _ ih =>
    have hrl16 := hvalid.2.2.2.1
    refine ⟨by omega, ih.2.1, ih.2.2.1, ?_⟩
    simp only [List.length_cons]
    omega

private theorem contiguousLog_agreesOn {g : FlashGeometry} {s s' : NorFlash.FlashState}
    {r startOff endOff : Nat} {entries : List LogEntry}
    (hlog : ContiguousLog g s r startOff entries endOff)
    (hag : NorFlash.agreesOn s' s (regionStartNat g r + startOff) (endOff - startOff)) :
    ContiguousLog g s' r startOff entries endOff := by
  induction hlog with
  | nil off hle hal => exact .nil off hle hal
  | @cons off nextOff endOff e rest hreg hoff hidx hvalid hintact hnext hrest ih =>
    have hb := contiguousLog_bounds hrest
    have hintact' : RecordIntact g s' e.record := by
      refine ⟨hintact.1, hintact.2.1, hintact.2.2.1, ?_⟩
      have hag_e : NorFlash.agreesOn s' s (regionStartNat g e.record.region.val + e.record.off.val) e.record.recordLen.val := by
        intro i hi1 hi2
        rw [hreg, hoff] at hi1 hi2
        exact hag i (by omega) (by omega)
      rw [NorFlash.readSlice_eq_of_agreesOn hag_e]
      exact hintact.2.2.2
    refine .cons hreg hoff hidx hvalid hintact' hnext (ih ?_)
    intro i hi1 hi2
    exact hag i (by omega) (by omega)

private theorem contiguousLog_append_one {g : FlashGeometry} {s : NorFlash.FlashState}
    {r startOff endOff : Nat} {entries : List LogEntry} {e : LogEntry}
    (hlog : ContiguousLog g s r startOff entries endOff)
    (hreg : e.record.region.val = r)
    (hoff : e.record.off.val = endOff)
    (hidx : e.record.seq.val = e.idx % 4294967296)
    (hvalid : ValidRecordBytes g e.record)
    (hintact : RecordIntact g s e.record) :
    ContiguousLog g s r startOff (entries ++ [e]) (endOff + e.record.recordLen.val) := by
  induction hlog with
  | nil off hle hal =>
    have hfit := hvalid.2.2.2.2.2.1
    have hral := hvalid.2.2.1
    have hal' : (off + e.record.recordLen.val) % max (max g.write_size.val g.read_size.val) 4 = 0 := by
      rw [Nat.add_mod, hal, hral]; simp
    exact .cons hreg hoff hidx hvalid hintact rfl (.nil _ (by omega) hal')
  | cons hr ho hi hv ha hn hrest ih =>
    exact .cons hr ho hi hv ha hn (ih hoff)

private theorem contiguousLog_last_end {g : FlashGeometry} {s : NorFlash.FlashState}
    {r startOff endOff : Nat} {entries : List LogEntry} {e : LogEntry}
    (hlog : ContiguousLog g s r startOff entries endOff)
    (hlast : entries.getLast? = some e) :
    e.record.off.val + e.record.recordLen.val = endOff := by
  induction hlog with
  | nil => simp at hlast
  | @cons off nextOff endOff x rest _ hoff _ _ _ hnext hrest ih =>
    cases rest with
    | nil =>
      simp at hlast; subst hlast
      cases hrest; omega
    | cons y ys =>
      have hl : (y :: ys).getLast? = some e := by simpa using hlast
      exact ih hl

/-- Physical tail condition after `endOff` in region `r`:
- `.erased`: every byte in `[regionStart g r + endOff, regionStart g r + g.region)` is `0xFF`.
- `.torn`: `endOff + 16 ≤ g.region`, and under Assumption C1 (non-forgery on torn flash)
  reading the header at `endOff` either returns `HeaderParse.Torn` or parses a header
  whose body fails `validate_record_crc` (never `Erased` and never a forged valid record). -/
def TailValid (g : FlashGeometry) (s : NorFlash.FlashState) (r endOff : Nat) : RegionTail → Prop
  | .erased => NorFlash.isErasedRange s (regionStartNat g r + endOff) (g.region.val - endOff)
  | .torn =>
    endOff + 16 ≤ g.region.val ∧
    ∀ (buf : Slice U8), buf.length = g.buf_cap.val →
      (∀ i, i < 16 →
        buf.val[i]? = some (u8OfUInt8 (s.mem (regionStartNat g r + endOff + i)))) →
      parse_header buf (u32OfNat endOff) g = ok .Torn ∨
      ∃ hdr,
        parse_header buf (u32OfNat endOff) g = ok (.Ok hdr) ∧
        ∀ (buf2 : Slice U8), buf2.length = g.buf_cap.val →
          (∀ i, i < hdr.record_len.val →
            buf2.val[i]? = some (u8OfUInt8 (s.mem (regionStartNat g r + endOff + i)))) →
          validate_record_crc buf2 hdr.payload_len = ok false

private theorem tailValid_agreesOn {g : FlashGeometry} {s s' : NorFlash.FlashState}
    {r endOff : Nat} {tail : RegionTail}
    (ht : TailValid g s r endOff tail)
    (hag : NorFlash.agreesOn s' s (regionStartNat g r + endOff) (g.region.val - endOff)) :
    TailValid g s' r endOff tail := by
  cases tail with
  | erased =>
    intro i hi1 hi2
    rw [hag i hi1 (by omega)]
    exact ht i hi1 hi2
  | torn =>
    obtain ⟨h16, hfor⟩ := ht
    refine ⟨h16, fun buf hblen hbytes => ?_⟩
    have hbytes_s : ∀ i, i < 16 →
        buf.val[i]? = some (u8OfUInt8 (s.mem (regionStartNat g r + endOff + i))) := by
      intro i hi
      rw [hbytes i hi, hag (regionStartNat g r + endOff + i) (by omega) (by omega)]
    rcases hfor buf hblen hbytes_s with htorn | ⟨hdr, hok, hbadcrc⟩
    · exact Or.inl htorn
    · refine Or.inr ⟨hdr, hok, fun buf2 hb2 hbytes2 => ?_⟩
      have hparse_p := H5iAppLib.post_of_ok (parse_header_spec buf (u32OfNat endOff) g) hok
      have hend_lt : endOff < U32.size := by scalar_tac
      have hoff_val : (u32OfNat endOff).val = endOff := u32OfNat_val hend_lt
      have hfit : endOff + hdr.record_len.val ≤ g.region.val := by
        have := hparse_p.2.2.2.1; rw [hoff_val] at this; exact this
      apply hbadcrc buf2 hb2
      intro i hi
      rw [hbytes2 i hi, hag (regionStartNat g r + endOff + i) (by omega) (by omega)]

/-- Convert a `CommittedRecord` to the `store_core.BestRecord` tracked by `scan_step`. -/
def toBestRecord (rec : CommittedRecord) : BestRecord where
  seq := rec.seq
  region := rec.region
  off := rec.off
  version := rec.version
  payload_len := rec.payloadLen
  record_len := rec.recordLen

/-- Pure boolean equivalent of extracted `store_core.seq_newer`. -/
def seqNewerBool (cand cur : U32) : Bool :=
  let d := (cand.val + 4294967296 - cur.val) % 4294967296
  decide (d ≠ 0 ∧ d < 2147483648)

private theorem seq_newer_eq_bool (cand cur : U32) :
    seq_newer cand cur = ok (seqNewerBool cand cur) := by
  obtain ⟨b, hb, hp⟩ := H5iAppLib.spec_exists_ok (seq_newer_spec cand cur)
  rw [hb]; congr 1

/-- Pure specification of how `accept_valid_record` folds `best` over valid records
using extracted `seq_newer`. -/
def bestFoldStep (cur : Option BestRecord) (e : LogEntry) : Option BestRecord :=
  match cur with
  | none => some (toBestRecord e.record)
  | some prev =>
    if seqNewerBool e.record.seq prev.seq then some (toBestRecord e.record) else some prev

def bestFold (cur : Option BestRecord) : List LogEntry → Option BestRecord
  | [] => cur
  | e :: rest => bestFold (bestFoldStep cur e) rest

private theorem bestFold_append (cur : Option BestRecord) (xs ys : List LogEntry) :
    bestFold cur (xs ++ ys) = bestFold (bestFold cur xs) ys := by
  induction xs generalizing cur with
  | nil => rfl
  | cons x xs ih => exact ih (bestFoldStep cur x)

private theorem seq_newer_of_idx_gt {a b : U32} {ia ib : Nat}
    (ha : a.val = ia % 4294967296) (hb : b.val = ib % 4294967296)
    (hlt : ib < ia) (hwin : ia - ib < 2147483648) :
    seq_newer a b = ok true := by
  have h := seq_newer_modular_window ib (ia - ib) 0 a b hwin (by omega)
    (by rw [ha]; congr 1; omega) (by simpa using hb)
  have h0 : 0 < ia - ib := by omega
  simpa [h0] using h

private theorem seq_newer_of_idx_lt {a b : U32} {ia ib : Nat}
    (ha : a.val = ia % 4294967296) (hb : b.val = ib % 4294967296)
    (hlt : ia < ib) (hwin : ib - ia < 2147483648) :
    seq_newer a b = ok false := by
  have h := seq_newer_modular_window ia 0 (ib - ia) a b (by omega) hwin
    (by simpa using ha) (by rw [hb]; congr 1; omega)
  have h0 : ¬ (ib - ia < 0) := by omega
  simpa [h0] using h

private theorem seqNewerBool_eq_of_ok {a b : U32} {r : Bool}
    (h : seq_newer a b = ok r) : seqNewerBool a b = r := by
  have h1 := seq_newer_eq_bool a b
  rw [h, Result.ok.injEq] at h1
  exact h1.symm

/-- If `top ∈ entries` has the unique maximum logical index `top.idx` and every
other entry in `entries` lies within the `< 2^31` half-space window below
`top.idx`, then `bestFold none entries = some (toBestRecord top.record)` (even
across `u32::MAX → 0` wraparound!). -/
theorem bestFold_selects_unique_max (entries : List LogEntry) (top : LogEntry)
    (hmem : top ∈ entries)
    (hseq : ∀ e ∈ entries, e.record.seq.val = e.idx % 4294967296)
    (hmax : ∀ e ∈ entries, e = top ∨ (e.idx < top.idx ∧ top.idx - e.idx < 2147483648)) :
    bestFold none entries = some (toBestRecord top.record) := by
  have haux : ∀ (xs : List LogEntry) (cur : Option BestRecord),
      (∀ e ∈ xs, e.record.seq.val = e.idx % 4294967296) →
      (∀ e ∈ xs, e = top ∨ (e.idx < top.idx ∧ top.idx - e.idx < 2147483648)) →
      (cur = none ∨ cur = some (toBestRecord top.record) ∨
        ∃ cIdx, ∀ b, cur = some b → b.seq.val = cIdx % 4294967296 ∧ cIdx < top.idx ∧ top.idx - cIdx < 2147483648) →
      (cur = some (toBestRecord top.record) ∨ top ∈ xs) →
      bestFold cur xs = some (toBestRecord top.record) := by
    intro xs
    induction xs with
    | nil =>
      intro cur _ _ _ htop
      rcases htop with h | h
      · exact h
      · cases h
    | cons x xs ih =>
      intro cur hseqx hmaxx hcur htop
      have hx_seq := hseqx x (List.mem_cons_self ..)
      have hxs_seq : ∀ e ∈ xs, e.record.seq.val = e.idx % 4294967296 :=
        fun e he => hseqx e (List.mem_cons_of_mem _ he)
      have hxs_max : ∀ e ∈ xs, e = top ∨ (e.idx < top.idx ∧ top.idx - e.idx < 2147483648) :=
        fun e he => hmaxx e (List.mem_cons_of_mem _ he)
      rcases hmaxx x (List.mem_cons_self ..) with rfl | ⟨hxlt, hxwin⟩
      · -- `x = top`: `bestFoldStep cur top` unconditionally produces `some (toBestRecord top.record)`.
        have hstep_top : bestFoldStep cur x = some (toBestRecord x.record) := by
          rcases hcur with rfl | rfl | ⟨cIdx, hc⟩
          · rfl
          · unfold bestFoldStep
            have hirr := seqNewerBool_eq_of_ok (seq_newer_irrefl x.record.seq)
            simp [toBestRecord, hirr]
          · cases cur with
            | none => rfl
            | some b =>
              obtain ⟨hbseq, hblt, hbwin⟩ := hc b rfl
              have hgt := seqNewerBool_eq_of_ok (seq_newer_of_idx_gt hx_seq hbseq hblt hbwin)
              simp [bestFoldStep, hgt]
        simp only [bestFold, hstep_top]
        exact ih (some (toBestRecord x.record)) hxs_seq hxs_max (Or.inr (Or.inl rfl)) (Or.inl rfl)
      · -- `x` is strictly older than `top`.
        have htop_xs : cur = some (toBestRecord top.record) ∨ top ∈ xs := by
          rcases htop with h | h
          · exact Or.inl h
          · rcases List.mem_cons.mp h with rfl | hmem_xs
            · omega
            · exact Or.inr hmem_xs
        rcases hcur with rfl | rfl | ⟨cIdx, hc⟩
        · simp only [bestFold, bestFoldStep]
          exact ih (some (toBestRecord x.record)) hxs_seq hxs_max
            (Or.inr (Or.inr ⟨x.idx, fun b hb => by cases hb; exact ⟨hx_seq, hxlt, hxwin⟩⟩))
            (by rcases htop_xs with h | h; cases h; exact Or.inr h)
        · have htop_seq : top.record.seq.val = top.idx % 4294967296 := hseq top hmem
          have hlt_sn := seqNewerBool_eq_of_ok (seq_newer_of_idx_lt hx_seq htop_seq hxlt hxwin)
          have hkeep : bestFoldStep (some (toBestRecord top.record)) x = some (toBestRecord top.record) := by
            simp [bestFoldStep, toBestRecord, hlt_sn]
          simp only [bestFold, hkeep]
          exact ih (some (toBestRecord top.record)) hxs_seq hxs_max (Or.inr (Or.inl rfl)) (Or.inl rfl)
        · cases cur with
          | none =>
            simp only [bestFold, bestFoldStep]
            exact ih (some (toBestRecord x.record)) hxs_seq hxs_max
              (Or.inr (Or.inr ⟨x.idx, fun b hb => by cases hb; exact ⟨hx_seq, hxlt, hxwin⟩⟩))
              (by rcases htop_xs with h | h; cases h; exact Or.inr h)
          | some b =>
            obtain ⟨hbseq, hblt, hbwin⟩ := hc b rfl
            obtain ⟨sn, hsn⟩ := H5iAppLib.ok_of (seq_newer_spec x.record.seq b.seq)
            cases sn with
            | true =>
              have hsn_b := seqNewerBool_eq_of_ok hsn
              have hst : bestFoldStep (some b) x = some (toBestRecord x.record) := by
                simp [bestFoldStep, hsn_b]
              simp only [bestFold, hst]
              have htop_in_xs : top ∈ xs := by
                rcases htop_xs with h | h
                · cases h
                  have htop_seq := hseq top hmem
                  have := seq_newer_of_idx_lt hx_seq htop_seq hxlt hxwin
                  rw [toBestRecord] at hsn; simp [this] at hsn
                · exact h
              exact ih (some (toBestRecord x.record)) hxs_seq hxs_max
                (Or.inr (Or.inr ⟨x.idx, fun b' hb' => by cases hb'; exact ⟨hx_seq, hxlt, hxwin⟩⟩))
                (Or.inr htop_in_xs)
            | false =>
              have hsn_b := seqNewerBool_eq_of_ok hsn
              have hst : bestFoldStep (some b) x = some b := by
                simp [bestFoldStep, hsn_b]
              simp only [bestFold, hst]
              exact ih (some b) hxs_seq hxs_max
                (Or.inr (Or.inr ⟨cIdx, hc⟩)) htop_xs
  exact haux entries none hseq hmax (Or.inl rfl) (Or.inr hmem)

/-- Helper: exact result of `accept_valid_record` on a valid record `e`. -/
private theorem accept_valid_record_eq (g : FlashGeometry) (st : ScanState) (e : LogEntry)
    (hlay : layout_ok g = ok true) (hinv : ScanInv g st)
    (hreg : e.record.region = st.region) (hoff : e.record.off = st.off)
    (hvalid : ValidRecordBytes g e.record) :
    let hdr : HeaderInfo := { seq := e.record.seq, version := e.record.version,
                              payload_len := e.record.payloadLen, record_len := e.record.recordLen }
    let nextBest := bestFoldStep st.best e
    let nextOff : U32 := u32OfNat (st.off.val + e.record.recordLen.val)
    ∃ res, accept_valid_record g st hdr = ok res ∧
      if nextOff.val + 16 ≤ g.region.val then
        res = ({ st with off := nextOff, best := nextBest, phase := .ReadHeader },
               ScanAction.Read (u32OfNat (regionStartNat g st.region.val + nextOff.val)) 0#u32 HDR_U32)
      else
        finish_region g { st with off := nextOff, best := nextBest } false = ok res := by
  have hp := H5iAppLib.post_of_ok (layout_ok_spec g) hlay rfl
  have hcap_u32 : g.base.val + g.region.val ≤ U32.max := by
    have := hp.in_capacity; scalar_tac
  have hh := hdr_u32_val
  have hfit : st.off.val + e.record.recordLen.val ≤ g.region.val := by
    rw [← hoff]; exact hvalid.2.2.2.2.2.1
  let hdr : HeaderInfo := { seq := e.record.seq, version := e.record.version,
                            payload_len := e.record.payloadLen, record_len := e.record.recordLen }
  have hbest_eq : ∀ (st_b : Option BestRecord),
      st.best = st_b →
      (do
        let b ← match st_b with
          | none => ok true
          | some prev => seq_newer hdr.seq prev.seq
        ok (if b then some { seq := hdr.seq, region := st.region, off := st.off,
                             version := hdr.version, payload_len := hdr.payload_len,
                             record_len := hdr.record_len : BestRecord } else st_b)) =
      ok (bestFoldStep st_b e) := by
    intro st_b _
    cases st_b with
    | none =>
      simp [bestFoldStep, toBestRecord, hdr, hreg, hoff]
    | some prev =>
      obtain ⟨sn, hsn⟩ := H5iAppLib.ok_of (seq_newer_spec hdr.seq prev.seq)
      simp only [hsn, bind_ok]
      have hsn_b := seqNewerBool_eq_of_ok hsn
      cases sn <;> simp [bestFoldStep, toBestRecord, hdr, hreg, hoff, hsn_b]
  obtain ⟨nxt, hadd, hnxt_val⟩ := u32_add_ok (a := st.off) (b := e.record.recordLen) (by scalar_tac)
  have hnxt_eq : nxt = u32OfNat (st.off.val + e.record.recordLen.val) := by
    apply UScalar.eq_of_val_eq
    have hlt : st.off.val + e.record.recordLen.val < U32.size := by scalar_tac
    rw [u32OfNat_val hlt, hnxt_val]
  have hle_reg : nxt ≤ g.region := by simp [UScalar.le_equiv]; omega
  obtain ⟨rem, hsub, hrem_val⟩ := u32_sub_ok (a := g.region) (b := nxt) (by omega)
  have hstep_body : ∀ (o1 : Option BestRecord),
      (do
        let i ← st.off + hdr.record_len
        if i ≤ g.region then
          let i1 ← g.region - i
          if HDR_U32 ≤ i1 then
            let i2 ← region_start g st.region
            let addr ← i2 + i
            ok ({ st with off := i, best := o1, phase := .ReadHeader }, ScanAction.Read addr 0#u32 HDR_U32)
          else finish_region g { st with off := i, best := o1 } false
        else finish_region g { st with off := i, best := o1 } false) =
      (if nxt.val + 16 ≤ g.region.val then
        ok ({ st with off := nxt, best := o1, phase := .ReadHeader },
            ScanAction.Read (u32OfNat (regionStartNat g st.region.val + nxt.val)) 0#u32 HDR_U32)
       else finish_region g { st with off := nxt, best := o1 } false) := by
    intro o1
    simp only [hdr, hadd, bind_ok, hle_reg, ↓reduceIte, hsub]
    by_cases h16 : HDR_U32 ≤ rem
    · have h16n : nxt.val + 16 ≤ g.region.val := by simp [UScalar.le_equiv, hh] at h16; omega
      simp only [h16, h16n, ↓reduceIte]
      obtain ⟨rs, hrs, hrs_val⟩ := H5iAppLib.spec_exists_ok (region_start_spec g st.region hcap_u32)
      have hrs_cap : rs.val + nxt.val ≤ U32.max := by
        rw [hrs_val]; unfold regionStartNat; split <;> have := hp.in_capacity <;> scalar_tac
      obtain ⟨addr, haddr, haddr_val⟩ := u32_add_ok (a := rs) (b := nxt) hrs_cap
      simp only [hrs, haddr, bind_ok]
      congr 2; congr 1
      apply UScalar.eq_of_val_eq
      have hlt : regionStartNat g st.region.val + nxt.val < U32.size := by rw [← hrs_val]; scalar_tac
      rw [u32OfNat_val hlt, haddr_val, hrs_val]
    · have h16n : ¬ (nxt.val + 16 ≤ g.region.val) := by simp [UScalar.le_equiv, hh] at h16; omega
      simp only [h16, h16n, ↓reduceIte]
  have hacc : accept_valid_record g st hdr =
      (if nxt.val + 16 ≤ g.region.val then
        ok ({ st with off := nxt, best := bestFoldStep st.best e, phase := .ReadHeader },
            ScanAction.Read (u32OfNat (regionStartNat g st.region.val + nxt.val)) 0#u32 HDR_U32)
       else finish_region g { st with off := nxt, best := bestFoldStep st.best e } false) := by
    unfold accept_valid_record
    cases hsb : st.best with
    | none =>
      have h1 := hbest_eq none hsb
      simp only [bind_ok]
      have : ({ seq := hdr.seq, region := st.region, off := st.off,
                version := hdr.version, payload_len := hdr.payload_len,
                record_len := hdr.record_len : BestRecord }) = toBestRecord e.record := by
        simp [toBestRecord, hdr, hreg, hoff]
      simpa [bestFoldStep, this] using hstep_body (some (toBestRecord e.record))
    | some prev =>
      obtain ⟨sn, hsn⟩ := H5iAppLib.ok_of (seq_newer_spec hdr.seq prev.seq)
      simp only [hsn, bind_ok]
      have hsn_b := seqNewerBool_eq_of_ok hsn
      cases sn with
      | true =>
        have : ({ seq := hdr.seq, region := st.region, off := st.off,
                  version := hdr.version, payload_len := hdr.payload_len,
                  record_len := hdr.record_len : BestRecord }) = toBestRecord e.record := by
          simp [toBestRecord, hdr, hreg, hoff]
        simpa [bestFoldStep, hdr, this, hsn_b] using hstep_body (some (toBestRecord e.record))
      | false =>
        simpa [bestFoldStep, hdr, hsn_b] using hstep_body (some prev)
  rw [← hnxt_eq]
  by_cases h16 : nxt.val + 16 ≤ g.region.val
  · simp only [h16, ↓reduceIte] at hacc ⊢
    exact ⟨_, hacc, rfl⟩
  · simp only [h16, ↓reduceIte] at hacc ⊢
    -- `finish_region` always succeeds under `layout_ok g = ok true` and `ScanInv`
    have hoff_al : nxt.val % max (max g.write_size.val g.read_size.val) 4 = 0 := by
      rw [hnxt_val, Nat.add_mod, hinv.2.2.1, hvalid.2.2.1]; simp
    have hbest_inv : ∀ b, bestFoldStep st.best e = some b →
        b.region.val ≤ 1 ∧ b.off.val % max (max g.write_size.val g.read_size.val) 4 = 0 ∧
        b.record_len.val % max (max g.write_size.val g.read_size.val) 4 = 0 ∧
        16 ≤ b.record_len.val ∧ b.record_len.val ≤ g.buf_cap.val ∧ b.off.val + b.record_len.val ≤ g.region.val := by
      intro b hb
      cases hsb : st.best with
      | none =>
        rw [hsb] at hb; simp [bestFoldStep, toBestRecord] at hb; subst hb
        exact ⟨hvalid.1, hvalid.2.1, hvalid.2.2.1, hvalid.2.2.2.1, hvalid.2.2.2.2.1, hvalid.2.2.2.2.2.1⟩
      | some prev =>
        rw [hsb] at hb
        obtain ⟨sn, hsn⟩ := H5iAppLib.ok_of (seq_newer_spec e.record.seq prev.seq)
        have hsn_b := seqNewerBool_eq_of_ok hsn
        cases sn <;> simp [bestFoldStep, hsn_b, toBestRecord] at hb
        · subst hb; exact hinv.2.2.2.1 prev hsb
        · subst hb; exact ⟨hvalid.1, hvalid.2.1, hvalid.2.2.1, hvalid.2.2.2.1, hvalid.2.2.2.2.1, hvalid.2.2.2.2.2.1⟩
    have hfr_inv : ScanInv g { st with off := nxt, best := bestFoldStep st.best e, phase := .ReadHeader } :=
      ⟨hinv.1, by omega, hoff_al, hbest_inv, fun _ h => by cases h⟩
    have hfr_eq : finish_region g { st with off := nxt, best := bestFoldStep st.best e } false =
        finish_region g { st with off := nxt, best := bestFoldStep st.best e, phase := .ReadHeader } false := by
      unfold finish_region; by_cases h0 : st.region = 0#u32 <;> simp [h0]
    have hfr_ok : ∃ res, finish_region g { st with off := nxt, best := bestFoldStep st.best e } false = ok res := by
      rw [hfr_eq]
      have hspec : finish_region g { st with off := nxt, best := bestFoldStep st.best e, phase := .ReadHeader } false ⦃ _ => True ⦄ := by
        unfold finish_region
        by_cases h0 : st.region = 0#u32
        · simp only [h0, ↓reduceIte]
          obtain ⟨rs, hrs, _⟩ := H5iAppLib.spec_exists_ok (region_start_spec g 1#u32 hcap_u32)
          simp only [hrs, bind_ok]; rw [WP.spec_ok]; trivial
        · simp only [h0, ↓reduceIte]
          cases hbf : bestFoldStep st.best e with
          | none => rw [WP.spec_ok]; trivial
          | some b =>
            have hb := hbest_inv b hbf
            obtain ⟨rs, hrs, hrs_val⟩ := H5iAppLib.spec_exists_ok (region_start_spec g b.region hcap_u32)
            have hcap : rs.val + b.off.val ≤ U32.max := by
              rw [hrs_val]; unfold regionStartNat; split <;> have := hp.in_capacity <;> scalar_tac
            obtain ⟨ra, hra, _⟩ := u32_add_ok (a := rs) (b := b.off) hcap
            simp only [hrs, hra, bind_ok]; rw [WP.spec_ok]; trivial
      exact H5iAppLib.ok_of hspec
    obtain ⟨res, hres⟩ := hfr_ok
    rw [hres] at hacc
    exact ⟨res, hacc, hres⟩

/-- Helper: `ReadValid` for reading a 16-byte header at aligned offset `off` in region `r`. -/
private theorem readValid_hdr (g : FlashGeometry) (r off : Nat)
    (hlay : layout_ok g = ok true) (hdiv : AlignDivides g)
    (hr : r ≤ 1) (hoff_al : off % max (max g.write_size.val g.read_size.val) 4 = 0)
    (hfit : off + 16 ≤ g.region.val) :
    NorFlash.ReadValid (toFlashConfig g) (regionStartNat g r + off) 16 := by
  have hp := H5iAppLib.post_of_ok (layout_ok_spec g) hlay rfl
  have hbase_rmod := mod_zero_of_dvd_mod_zero hp.base_aligned hdiv.2
  have hreg_rmod := mod_zero_of_dvd_mod_zero hp.region_aligned hdiv.2
  have hhdr_rmod := mod_zero_of_dvd_mod_zero hp.hdr_aligned hdiv.2
  have hoff_rmod := mod_zero_of_dvd_mod_zero hoff_al hdiv.2
  have hrs_rmod : regionStartNat g r % g.read_size.val = 0 := by
    unfold regionStartNat; split
    · exact hbase_rmod
    · rw [Nat.add_mod, hbase_rmod, hreg_rmod]; simp
  dsimp only [NorFlash.ReadValid, toFlashConfig]
  refine ⟨?_, ?_, hhdr_rmod⟩
  · unfold regionStartNat; split <;> have := hp.in_capacity <;> omega
  · rw [Nat.add_mod, hrs_rmod, hoff_rmod]; simp

/-- Helper: `ReadValid` for reading a record's body `[16..rl)` or full record `[0..rl)`. -/
private theorem readValid_record_slice (g : FlashGeometry) (r off start len : Nat)
    (hlay : layout_ok g = ok true) (hdiv : AlignDivides g)
    (hr : r ≤ 1)
    (hoff_al : off % max (max g.write_size.val g.read_size.val) 4 = 0)
    (hstart_al : start % max (max g.write_size.val g.read_size.val) 4 = 0)
    (hlen_al : len % max (max g.write_size.val g.read_size.val) 4 = 0)
    (hfit : off + start + len ≤ g.region.val) :
    NorFlash.ReadValid (toFlashConfig g) (regionStartNat g r + off + start) len := by
  have hp := H5iAppLib.post_of_ok (layout_ok_spec g) hlay rfl
  have hbase_rmod := mod_zero_of_dvd_mod_zero hp.base_aligned hdiv.2
  have hreg_rmod := mod_zero_of_dvd_mod_zero hp.region_aligned hdiv.2
  have hrs_rmod : regionStartNat g r % g.read_size.val = 0 := by
    unfold regionStartNat; split
    · exact hbase_rmod
    · rw [Nat.add_mod, hbase_rmod, hreg_rmod]; simp
  have hoff_rmod := mod_zero_of_dvd_mod_zero hoff_al hdiv.2
  have hst_rmod := mod_zero_of_dvd_mod_zero hstart_al hdiv.2
  have hlen_rmod := mod_zero_of_dvd_mod_zero hlen_al hdiv.2
  dsimp only [NorFlash.ReadValid, toFlashConfig]
  refine ⟨?_, ?_, hlen_rmod⟩
  · unfold regionStartNat; split <;> have := hp.in_capacity <;> omega
  · rw [Nat.add_mod (regionStartNat g r + off) start, Nat.add_mod (regionStartNat g r) off,
        hrs_rmod, hoff_rmod, hst_rmod]; simp

/-- **Region scan functional correctness & determinism**:
Starting from `ScanState` at `ReadHeader` in region `r` (`off = startOff`) with
a `ContiguousLog g s r startOff entries endOff` and `TailValid g s r endOff tail`,
executing the **actual extracted `store_core.scan_step`** (`ScanExec`) across
the region:
1. (Existence) reaches the exact continuation `finish_region g { st with off := endOff, best := bestFold st.best entries } (tail == .torn)` whenever a continuation `ScanExec` exists from there;
2. (Determinism) ANY `ScanExec` run from `(st, Read (regionStart+startOff) 0 16, buf)` MUST pass through that exact `finish_region` state! -/
private theorem scan_region_spec
    (g : FlashGeometry) (s : NorFlash.FlashState)
    (hlay : layout_ok g = ok true) (hdiv : AlignDivides g)
    {r startOff endOff : Nat} {entries : List LogEntry} {tail : RegionTail}
    (hr_le : r ≤ 1)
    (hlog : ContiguousLog g s r startOff entries endOff)
    (htail : TailValid g s r endOff tail) :
    ∀ (st : ScanState) (buf : Slice U8),
      ScanInv g st →
      st.phase = .ReadHeader →
      st.region.val = r →
      st.off.val = startOff →
      startOff + 16 ≤ g.region.val →
      buf.length = g.buf_cap.val →
      let stEnd := { st with off := u32OfNat endOff, best := bestFold st.best entries }
      let tornBool := decide (tail = .torn)
      (∀ (P : Slice U8 → Prop) stF actF stNext actNext,
        finish_region g stEnd tornBool = ok (stNext, actNext) →
        (∀ buf1, buf1.length = g.buf_cap.val → ∃ bufF, ScanExec g s stNext actNext buf1 stF actF bufF ∧ P bufF) →
        ∃ bufF, ScanExec g s st (.Read (u32OfNat (regionStartNat g r + startOff)) 0#u32 HDR_U32) buf stF actF bufF ∧ P bufF) ∧
      (∀ stF actF bufF,
        ScanExec g s st (.Read (u32OfNat (regionStartNat g r + startOff)) 0#u32 HDR_U32) buf stF actF bufF →
        ∃ stNext actNext buf1,
          finish_region g stEnd tornBool = ok (stNext, actNext) ∧
          buf1.length = g.buf_cap.val ∧
          ScanExec g s stNext actNext buf1 stF actF bufF) := by
  have hp := H5iAppLib.post_of_ok (layout_ok_spec g) hlay rfl
  have hh := hdr_u32_val
  induction hlog with
  | nil off hle hal =>
    intro st buf hinv hph hreg hoff hfit16 hbuf
    have haddr_lt : regionStartNat g r + off < U32.size := by
      unfold regionStartNat; split <;> have := hp.in_capacity <;> scalar_tac
    have haddr_val : (u32OfNat (regionStartNat g r + off)).val = regionStartNat g r + off := u32OfNat_val haddr_lt
    have hoff_eq : u32OfNat off = st.off := by
      apply UScalar.eq_of_val_eq
      have hlt : off < U32.size := by scalar_tac
      rw [u32OfNat_val hlt, hoff]
    have hstEnd_eq : { st with off := u32OfNat off, best := bestFold st.best [] } = st := by
      simp [bestFold, hoff_eq]
    have hrv := readValid_hdr g r off hlay hdiv hr_le hal hfit16
    have hread_valid : NorFlash.ReadValid (toFlashConfig g)
        (u32OfNat (regionStartNat g r + off)).val HDR_U32.val := by
      rw [haddr_val, hh]; exact hrv
    have hbuf_in : (0#u32).val + HDR_U32.val ≤ buf.length := by
      have := hp.buf_ge16; rw [hbuf, hh]; simp; omega
    -- Helper: one `scan_step` from `(st, buf1)` at the tail reaches `finish_region g st (decide (tail = .torn))`.
    cases tail with
    | erased =>
      have hstep_erased : ∀ buf1,
          BufAfterRead s (regionStartNat g r + off) 0 16 buf buf1 →
          scan_step g st buf1 = finish_region g st false := by
        intro buf1 hbr
        have hall255 : ∀ i, i < 16 → buf1.val[i]? = some 255#u8 := by
          intro i hi
          rw [hbr.2.1 i (Nat.zero_le _) (by omega)]
          have hmem := htail (regionStartNat g r + off + i) (by omega) (by omega)
          simpa [Nat.sub_zero, hmem] using congrArg Option.some u8_ofNat_255
        have hph_er := parse_header_of_all_erased buf1 st.off g (by rw [hbr.1, hbuf]; exact hp.buf_ge16) hall255
        unfold scan_step
        simp only [hlay, bind_ok, ↓reduceIte, hph, hph_er]
      constructor
      · intro P stF actF stNext actNext hfin hcont
        rw [hstEnd_eq] at hfin
        obtain ⟨buf1, hbr⟩ := bufAfterRead_exists s (regionStartNat g r + off) 0 16 buf (by rw [hbuf]; have := hp.buf_ge16; omega)
        have hbr' : BufAfterRead s (u32OfNat (regionStartNat g r + off)).val (0#u32).val HDR_U32.val buf buf1 := by
          simpa [haddr_val, hh] using hbr
        have hst1 : scan_step g st buf1 = ok (stNext, actNext) := by
          rw [hstep_erased buf1 hbr]; exact hfin
        obtain ⟨bufF, hexF, hPF⟩ := hcont buf1 (by rw [hbr.1, hbuf])
        exact ⟨bufF, .stepRead hread_valid hbuf_in hbr' hst1 hexF, hPF⟩
      · intro stF actF bufF hex
        cases hex with
        | stepRead _ _ hbr' hst1 hrest =>
          have h0v : (0#u32).val = 0 := by simp
          rw [haddr_val, h0v, hh] at hbr'
          rw [hstep_erased _ hbr'] at hst1
          exact ⟨_, _, _, by simpa [hstEnd_eq] using hst1, by rw [hbr'.1, hbuf], hrest⟩
    | torn =>
      obtain ⟨_, htorn_spec⟩ := htail
      have hbuf_step_torn : ∀ buf1,
          BufAfterRead s (regionStartNat g r + off) 0 16 buf buf1 →
          (scan_step g st buf1 = finish_region g st true) ∨
          (∃ hdr,
            parse_header buf1 st.off g = ok (.Ok hdr) ∧
            16 < hdr.record_len.val ∧
            scan_step g st buf1 = ok ({ st with phase := .ReadBody hdr },
              .Read (u32OfNat (regionStartNat g r + off + 16)) HDR_U32 (u32OfNat (hdr.record_len.val - 16))) ∧
            (∀ buf2, BufAfterRead s (regionStartNat g r + off + 16) 16 (hdr.record_len.val - 16) buf1 buf2 →
              scan_step g { st with phase := .ReadBody hdr } buf2 = finish_region g { st with phase := .ReadBody hdr } true)) := by
        intro buf1 hbr
        have hbytes1 : ∀ i, i < 16 →
            buf1.val[i]? = some (u8OfUInt8 (s.mem (regionStartNat g r + off + i))) := by
          intro i hi; simpa [Nat.sub_zero] using hbr.2.1 i (Nat.zero_le _) (by omega)
        have hspec1 := htorn_spec buf1 (by rw [hbr.1, hbuf]) hbytes1
        rw [hoff_eq] at hspec1
        rcases hspec1 with hptorn | ⟨hdr, hpok, hbadcrc⟩
        · left
          unfold scan_step
          simp only [hlay, bind_ok, ↓reduceIte, hph, hptorn]
        · have hparse_p := H5iAppLib.post_of_ok (parse_header_spec buf1 st.off g) hpok
          by_cases hgt : hdr.record_len > HDR_U32
          · right
            have hgt_n : 16 < hdr.record_len.val := by simpa [UScalar.lt_equiv, gt_iff_lt, hh] using hgt
            have hcap_u32 : g.base.val + g.region.val ≤ U32.max := by have := hp.in_capacity; scalar_tac
            obtain ⟨rs, hrs, hrs_val⟩ := H5iAppLib.spec_exists_ok (region_start_spec g st.region hcap_u32)
            have hrs_eq : rs.val = regionStartNat g r := by rw [hrs_val, hreg]
            have hcap1 : rs.val + st.off.val ≤ U32.max := by
              rw [hrs_eq]; unfold regionStartNat; split <;> have := hp.in_capacity <;> scalar_tac
            obtain ⟨i1, hi1, hi1_val⟩ := u32_add_ok (a := rs) (b := st.off) hcap1
            have hcap2 : i1.val + HDR_U32.val ≤ U32.max := by
              rw [hi1_val, hrs_eq, hoff, hh]; unfold regionStartNat; split <;> have := hp.in_capacity <;> scalar_tac
            obtain ⟨addr, haddr, haddr_val2⟩ := u32_add_ok (a := i1) (b := HDR_U32) hcap2
            obtain ⟨blen, hblen, hblen_val⟩ := u32_sub_ok (a := hdr.record_len) (b := HDR_U32) (by omega)
            have haddr_eq2 : addr = u32OfNat (regionStartNat g r + off + 16) := by
              apply UScalar.eq_of_val_eq
              have hlt : regionStartNat g r + off + 16 < U32.size := by
                rw [← hrs_eq, ← hoff, ← hh]; scalar_tac
              rw [u32OfNat_val hlt, haddr_val2, hi1_val, hrs_eq, hoff, hh]
            have hblen_eq : blen = u32OfNat (hdr.record_len.val - 16) := by
              apply UScalar.eq_of_val_eq
              have hlt : hdr.record_len.val - 16 < U32.size := by scalar_tac
              rw [u32OfNat_val hlt, hblen_val, hh]
            refine ⟨hdr, hpok, hgt_n, ?_, ?_⟩
            · unfold scan_step
              simp only [hlay, bind_ok, ↓reduceIte, hph, hpok, hgt, hrs, hi1, haddr, hblen, haddr_eq2, hblen_eq]
            · intro buf2 hbr2
              have hbytes2 : ∀ i, i < hdr.record_len.val →
                  buf2.val[i]? = some (u8OfUInt8 (s.mem (regionStartNat g r + off + i))) := by
                intro i hi
                by_cases hi16 : i < 16
                · rw [hbr2.2.2 i (Or.inl hi16)]; exact hbytes1 i hi16
                · have hsub_eq : regionStartNat g r + off + 16 + (i - 16) = regionStartNat g r + off + i := by omega
                  rw [hbr2.2.1 i (by omega) (by omega), hsub_eq]
              have hcfalse := hbadcrc buf2 (by rw [hbr2.1, hbr.1, hbuf]) hbytes2
              unfold scan_step
              simp only [hlay, bind_ok, ↓reduceIte, hcfalse, Bool.false_eq_true]
          · left
            have hle16 : hdr.record_len.val ≤ 16 := by simp [UScalar.lt_equiv, gt_iff_lt, hh] at hgt; omega
            have hcfalse := hbadcrc buf1 (by rw [hbr.1, hbuf]) (fun i hi => hbytes1 i (by omega))
            unfold scan_step
            simp only [hlay, bind_ok, ↓reduceIte, hph, hpok, hgt, hcfalse, Bool.false_eq_true]
      have hfin_phase_irrel : ∀ hdr,
          finish_region g { st with phase := .ReadBody hdr } true = finish_region g st true := by
        intro hdr; unfold finish_region; by_cases h0 : st.region = 0#u32 <;> simp [h0]
      constructor
      · intro P stF actF stNext actNext hfin hcont
        rw [hstEnd_eq] at hfin
        obtain ⟨buf1, hbr⟩ := bufAfterRead_exists s (regionStartNat g r + off) 0 16 buf (by rw [hbuf]; have := hp.buf_ge16; omega)
        have hbr' : BufAfterRead s (u32OfNat (regionStartNat g r + off)).val (0#u32).val HDR_U32.val buf buf1 := by
          simpa [haddr_val, hh] using hbr
        rcases hbuf_step_torn buf1 hbr with hdirect | ⟨hdr, hpok, hgt_n, hstep1, hstep2⟩
        · have hst1 : scan_step g st buf1 = ok (stNext, actNext) := by rw [hdirect]; exact hfin
          obtain ⟨bufF, hexF, hPF⟩ := hcont buf1 (by rw [hbr.1, hbuf])
          exact ⟨bufF, .stepRead hread_valid hbuf_in hbr' hst1 hexF, hPF⟩
        · have hparse_p := H5iAppLib.post_of_ok (parse_header_spec buf1 st.off g) hpok
          have hblen_al : (hdr.record_len.val - 16) % max (max g.write_size.val g.read_size.val) 4 = 0 := by
            have d1 := Nat.dvd_of_mod_eq_zero hparse_p.1
            have d2 := Nat.dvd_of_mod_eq_zero hp.hdr_aligned
            exact Nat.mod_eq_zero_of_dvd (Nat.dvd_sub d1 d2)
          have hrv2 := readValid_record_slice g r off 16 (hdr.record_len.val - 16) hlay hdiv hr_le
            hal hp.hdr_aligned hblen_al (by have := hparse_p.2.2.2.1; omega)
          have haddr2_val : (u32OfNat (regionStartNat g r + off + 16)).val = regionStartNat g r + off + 16 := by
            have hlt : regionStartNat g r + off + 16 < U32.size := by
              have hcap := hp.in_capacity; have hcap_lt := u32_val_lt g.capacity
              have hsz : U32.size = 4294967296 := by scalar_tac
              unfold regionStartNat; split <;> omega
            exact u32OfNat_val hlt
          have hlen2_val : (u32OfNat (hdr.record_len.val - 16)).val = hdr.record_len.val - 16 := by
            have hlt : hdr.record_len.val - 16 < U32.size := by scalar_tac
            exact u32OfNat_val hlt
          have hfit_buf2 : 16 + (hdr.record_len.val - 16) ≤ buf1.length := by
            rw [hbr.1, hbuf]; have := hparse_p.2.2.1; omega
          obtain ⟨buf2, hbr2⟩ := bufAfterRead_exists s (regionStartNat g r + off + 16) 16 (hdr.record_len.val - 16) buf1 hfit_buf2
          have hbr2' : BufAfterRead s (u32OfNat (regionStartNat g r + off + 16)).val HDR_U32.val
              (u32OfNat (hdr.record_len.val - 16)).val buf1 buf2 := by
            simpa [haddr2_val, hlen2_val, hh] using hbr2
          have hst2 : scan_step g { st with phase := .ReadBody hdr } buf2 = ok (stNext, actNext) := by
            rw [hstep2 buf2 hbr2, hfin_phase_irrel]; exact hfin
          obtain ⟨bufF, hexF, hPF⟩ := hcont buf2 (by rw [hbr2.1, hbr.1, hbuf])
          exact ⟨bufF, .stepRead hread_valid hbuf_in hbr' hstep1
            (.stepRead (by rw [haddr2_val, hlen2_val]; exact hrv2) (by rw [hh, hlen2_val]; exact hfit_buf2)
              hbr2' hst2 hexF), hPF⟩
      · intro stF actF bufF hex
        cases hex with
        | stepRead _ _ hbr' hst1 hrest =>
          have h0v : (0#u32).val = 0 := by simp
          rw [haddr_val, h0v, hh] at hbr'
          rcases hbuf_step_torn _ hbr' with hdirect | ⟨hdr, hpok, hgt_n, hstep1, hstep2⟩
          · rw [hdirect] at hst1
            exact ⟨_, _, _, by simpa [hstEnd_eq] using hst1, by rw [hbr'.1, hbuf], hrest⟩
          · rw [hstep1, Result.ok.injEq, Prod.mk.injEq] at hst1
            obtain ⟨hst_eq, hact_eq⟩ := hst1
            subst hst_eq; subst hact_eq
            cases hrest with
            | stepRead _ _ hbr2' hst2 hrest2 =>
              have haddr2_val : (u32OfNat (regionStartNat g r + off + 16)).val = regionStartNat g r + off + 16 := by
                have hlt : regionStartNat g r + off + 16 < U32.size := by
                  have hcap := hp.in_capacity; have hcap_lt := u32_val_lt g.capacity
                  have hsz : U32.size = 4294967296 := by scalar_tac
                  unfold regionStartNat; split <;> omega
                exact u32OfNat_val hlt
              have hlen2_val : (u32OfNat (hdr.record_len.val - 16)).val = hdr.record_len.val - 16 := by
                have hlt : hdr.record_len.val - 16 < U32.size := by scalar_tac
                exact u32OfNat_val hlt
              rw [haddr2_val, hlen2_val, hh] at hbr2'
              rw [hstep2 _ hbr2', hfin_phase_irrel] at hst2
              exact ⟨_, _, _, by simpa [hstEnd_eq] using hst2, by rw [hbr2'.1, hbr'.1, hbuf], hrest2⟩
  | @cons off nextOff endOff e rest hreg_e hoff_e hidx_e hvalid_e hintact_e hnext_e hrest_log ih =>
    intro st buf hinv hph hreg_st hoff_st hfit16 hbuf
    have hreg_eq : e.record.region = st.region := UScalar.eq_of_val_eq (by rw [hreg_e, hreg_st])
    have hoff_eq : e.record.off = st.off := UScalar.eq_of_val_eq (by rw [hoff_e, hoff_st])
    have haddr_lt : regionStartNat g r + off < U32.size := by
      unfold regionStartNat; split <;> have := hp.in_capacity <;> scalar_tac
    have haddr_val : (u32OfNat (regionStartNat g r + off)).val = regionStartNat g r + off := u32OfNat_val haddr_lt
    have hrv := readValid_hdr g r off hlay hdiv hr_le (by rw [← hoff_e]; exact hvalid_e.2.1) hfit16
    have hread_valid : NorFlash.ReadValid (toFlashConfig g)
        (u32OfNat (regionStartNat g r + off)).val HDR_U32.val := by
      rw [haddr_val, hh]; exact hrv
    have hbuf_in : (0#u32).val + HDR_U32.val ≤ buf.length := by
      have := hp.buf_ge16; rw [hbuf, hh]; simp; omega
    let hdr : HeaderInfo := { seq := e.record.seq, version := e.record.version,
                              payload_len := e.record.payloadLen, record_len := e.record.recordLen }
    obtain ⟨accRes, hacc_eq, hacc_spec⟩ :=
      accept_valid_record_eq g st e hlay hinv hreg_eq hoff_eq hvalid_e
    have hnxt_val : (u32OfNat (st.off.val + e.record.recordLen.val)).val = nextOff := by
      have hlt : st.off.val + e.record.recordLen.val < U32.size := by
        have := hvalid_e.2.2.2.2.2.1; scalar_tac
      rw [u32OfNat_val hlt, hoff_st, hnext_e]
    -- Helper: parsing `e` from `buf1` always yields `Ok hdr`, and after optional `ReadBody`,
    -- `validate_record_crc` returns `true` and transitions to `accRes`!
    have hread_e_hdr : ∀ buf1, BufAfterRead s (regionStartNat g r + off) 0 16 buf buf1 →
        parse_header buf1 st.off g = ok (.Ok hdr) := by
      intro buf1 hbr
      rw [← hoff_eq]
      apply hvalid_e.2.2.2.2.2.2.2.2.2.1 buf1 (by rw [hbr.1, hbuf])
      intro i hi
      rw [hbr.2.1 i (Nat.zero_le _) (by omega)]
      have hmem := recordIntact_mem_eq g s e.record hintact_e (i := i) (by have := hvalid_e.2.2.2.1; omega)
      rw [hreg_e, hoff_e] at hmem
      simp [Nat.sub_zero, hmem]
    have hstep_to_acc : ∀ buf1, BufAfterRead s (regionStartNat g r + off) 0 16 buf buf1 →
        (16 = e.record.recordLen.val ∧ scan_step g st buf1 = ok accRes) ∨
        (16 < e.record.recordLen.val ∧
          scan_step g st buf1 = ok ({ st with phase := .ReadBody hdr },
            .Read (u32OfNat (regionStartNat g r + off + 16)) HDR_U32 (u32OfNat (e.record.recordLen.val - 16))) ∧
          ∀ buf2, BufAfterRead s (regionStartNat g r + off + 16) 16 (e.record.recordLen.val - 16) buf1 buf2 →
            scan_step g { st with phase := .ReadBody hdr } buf2 = ok accRes) := by
      intro buf1 hbr
      have hpok := hread_e_hdr buf1 hbr
      have hrl16 := hvalid_e.2.2.2.1
      by_cases hgt : hdr.record_len > HDR_U32
      · right
        have hgt_n : 16 < e.record.recordLen.val := by simpa [hdr, UScalar.lt_equiv, gt_iff_lt, hh] using hgt
        have hcap_u32 : g.base.val + g.region.val ≤ U32.max := by have := hp.in_capacity; scalar_tac
        obtain ⟨rs, hrs, hrs_val⟩ := H5iAppLib.spec_exists_ok (region_start_spec g st.region hcap_u32)
        have hrs_eq : rs.val = regionStartNat g r := by rw [hrs_val, hreg_st]
        have hcap1 : rs.val + st.off.val ≤ U32.max := by
          rw [hrs_eq]; unfold regionStartNat; split <;> have := hp.in_capacity <;> scalar_tac
        obtain ⟨i1, hi1, hi1_val⟩ := u32_add_ok (a := rs) (b := st.off) hcap1
        have hcap2 : i1.val + HDR_U32.val ≤ U32.max := by
          rw [hi1_val, hrs_eq, hoff_st, hh]; unfold regionStartNat; split <;> have := hp.in_capacity <;> scalar_tac
        obtain ⟨addr, haddr, haddr_val2⟩ := u32_add_ok (a := i1) (b := HDR_U32) hcap2
        obtain ⟨blen, hblen, hblen_val⟩ := u32_sub_ok (a := hdr.record_len) (b := HDR_U32) (by simp [hdr]; omega)
        have haddr_eq2 : addr = u32OfNat (regionStartNat g r + off + 16) := by
          apply UScalar.eq_of_val_eq
          have hlt : regionStartNat g r + off + 16 < U32.size := by
            rw [← hrs_eq, ← hoff_st, ← hh]; scalar_tac
          rw [u32OfNat_val hlt, haddr_val2, hi1_val, hrs_eq, hoff_st, hh]
        have hblen_eq : blen = u32OfNat (e.record.recordLen.val - 16) := by
          apply UScalar.eq_of_val_eq
          have hlt : e.record.recordLen.val - 16 < U32.size := by have := u32_val_lt e.record.recordLen; have hsz : U32.size = 4294967296 := (by scalar_tac); omega
          rw [u32OfNat_val hlt, hblen_val, hh]
        refine ⟨hgt_n, ?_, ?_⟩
        · unfold scan_step
          simp only [hlay, bind_ok, ↓reduceIte, hph, hpok, hgt, hrs, hi1, haddr, hblen, haddr_eq2, hblen_eq]
        · intro buf2 hbr2
          have hcrc : validate_record_crc buf2 e.record.payloadLen = ok true := by
            apply hvalid_e.2.2.2.2.2.2.2.2.2.2 buf2 (by rw [hbr2.1, hbr.1, hbuf])
            intro i hi
            by_cases hi16 : i < 16
            · rw [hbr2.2.2 i (Or.inl hi16), hbr.2.1 i (Nat.zero_le _) (by omega)]
              have hmem := recordIntact_mem_eq g s e.record hintact_e hi
              rw [hreg_e, hoff_e] at hmem; simp [Nat.sub_zero, hmem]
            · have hsub_eq : regionStartNat g r + off + 16 + (i - 16) = regionStartNat g r + off + i := by omega
              rw [hbr2.2.1 i (by omega) (by omega), hsub_eq]
              have hmem := recordIntact_mem_eq g s e.record hintact_e hi
              rw [hreg_e, hoff_e] at hmem; simp [hmem]
          have hacc_ph : accept_valid_record g { st with phase := .ReadBody hdr } hdr = accept_valid_record g st hdr := by
            have hst_rb_inv : ScanInv g { st with phase := .ReadBody hdr } :=
              ⟨hinv.1, hinv.2.1, hinv.2.2.1, hinv.2.2.2.1, fun hdr' heq => by
                cases heq; exact ⟨hvalid_e.2.2.1, hgt_n, hvalid_e.2.2.2.2.1, by rw [← hoff_eq]; exact hvalid_e.2.2.2.2.2.1⟩⟩
            obtain ⟨res2, hres2, hspec2⟩ := accept_valid_record_eq g { st with phase := .ReadBody hdr } e hlay hst_rb_inv hreg_eq hoff_eq hvalid_e
            rw [hres2, hacc_eq]
            by_cases hc : (u32OfNat (st.off.val + e.record.recordLen.val)).val + 16 ≤ g.region.val
            · simp only [hc, ↓reduceIte] at hspec2 hacc_spec
              rw [hspec2, hacc_spec]
            · simp only [hc, ↓reduceIte] at hspec2 hacc_spec
              have hfr_same : finish_region g { st with off := u32OfNat (st.off.val + e.record.recordLen.val), best := bestFoldStep st.best e, phase := .ReadBody hdr } false =
                  finish_region g { st with off := u32OfNat (st.off.val + e.record.recordLen.val), best := bestFoldStep st.best e } false := by
                unfold finish_region; by_cases h0 : st.region = 0#u32 <;> simp [h0]
              rw [hfr_same, hacc_spec, Result.ok.injEq] at hspec2
              rw [hspec2]
          unfold scan_step
          simp only [hlay, bind_ok, ↓reduceIte, hdr, hcrc, hacc_ph, hacc_eq]
      · left
        have heq16 : 16 = e.record.recordLen.val := by simp [hdr, UScalar.lt_equiv, gt_iff_lt, hh] at hgt; omega
        have hcrc : validate_record_crc buf1 e.record.payloadLen = ok true := by
          apply hvalid_e.2.2.2.2.2.2.2.2.2.2 buf1 (by rw [hbr.1, hbuf])
          intro i hi
          rw [hbr.2.1 i (Nat.zero_le _) (by omega)]
          have hmem := recordIntact_mem_eq g s e.record hintact_e hi
          rw [hreg_e, hoff_e] at hmem; simp [Nat.sub_zero, hmem]
        refine ⟨heq16, ?_⟩
        unfold scan_step
        simp only [hlay, bind_ok, ↓reduceIte, hph, hpok, hgt, hdr, hcrc, hacc_eq]
    -- Now split on whether `nextOff + 16 ≤ g.region.val` (continuing to `rest`) or not (finishing region).
    by_cases hcont16 : (u32OfNat (st.off.val + e.record.recordLen.val)).val + 16 ≤ g.region.val
    · -- Continue scanning `rest` at `nextOff`!
      simp only [hcont16, ↓reduceIte] at hacc_spec
      have hst1_inv : ScanInv g { st with off := u32OfNat (st.off.val + e.record.recordLen.val), best := bestFoldStep st.best e, phase := .ReadHeader } := by
        have hinv_step : ScanInv g accRes.1 := by
          -- Derive directly from `scan_step_safe_and_terminates` or fields!
          have hnxt_al : (u32OfNat (st.off.val + e.record.recordLen.val)).val %
              max (max g.write_size.val g.read_size.val) 4 = 0 := by
            rw [hnxt_val, hnext_e, Nat.add_mod, ← hoff_e, hvalid_e.2.1, hvalid_e.2.2.1]; simp
          have hbest_inv : ∀ b, bestFoldStep st.best e = some b →
              b.region.val ≤ 1 ∧ b.off.val % max (max g.write_size.val g.read_size.val) 4 = 0 ∧
              b.record_len.val % max (max g.write_size.val g.read_size.val) 4 = 0 ∧
              16 ≤ b.record_len.val ∧ b.record_len.val ≤ g.buf_cap.val ∧ b.off.val + b.record_len.val ≤ g.region.val := by
            intro b hb
            cases hsb : st.best with
            | none =>
              rw [hsb] at hb; simp [bestFoldStep, toBestRecord] at hb; subst hb
              exact ⟨hvalid_e.1, hvalid_e.2.1, hvalid_e.2.2.1, hvalid_e.2.2.2.1, hvalid_e.2.2.2.2.1, hvalid_e.2.2.2.2.2.1⟩
            | some prev =>
              rw [hsb] at hb
              obtain ⟨sn, hsn⟩ := H5iAppLib.ok_of (seq_newer_spec e.record.seq prev.seq)
              have hsn_b := seqNewerBool_eq_of_ok hsn
              cases sn <;> simp [bestFoldStep, hsn_b, toBestRecord] at hb
              · subst hb; exact hinv.2.2.2.1 prev hsb
              · subst hb; exact ⟨hvalid_e.1, hvalid_e.2.1, hvalid_e.2.2.1, hvalid_e.2.2.2.1, hvalid_e.2.2.2.2.1, hvalid_e.2.2.2.2.2.1⟩
          rw [hacc_spec]
          exact ⟨hinv.1, by rw [hnxt_val]; have := hvalid_e.2.2.2.2.2.1; omega, hnxt_al, hbest_inv, (fun _ h => by cases h)⟩
        rw [hacc_spec] at hinv_step; exact hinv_step
      have hact_eq_addr : u32OfNat (regionStartNat g st.region.val +
          (u32OfNat (st.off.val + e.record.recordLen.val)).val) =
          (u32OfNat (regionStartNat g r + nextOff)) := by
        rw [hreg_st, hnxt_val]
      have hst_end_eq :
          { { st with off := u32OfNat (st.off.val + e.record.recordLen.val),
                      best := bestFoldStep st.best e, phase := .ReadHeader } with
            off := u32OfNat endOff, best := bestFold (bestFoldStep st.best e) rest } =
          { st with off := u32OfNat endOff, best := bestFold st.best (e :: rest) } := by
        cases st; simp [bestFold] at hph ⊢; exact hph.symm
      constructor
      · intro P stF actF stNext actNext hfin hcont
        obtain ⟨buf1, hbr⟩ := bufAfterRead_exists s (regionStartNat g r + off) 0 16 buf (by rw [hbuf]; have := hp.buf_ge16; omega)
        have hbr' : BufAfterRead s (u32OfNat (regionStartNat g r + off)).val (0#u32).val HDR_U32.val buf buf1 := by
          simpa [haddr_val, hh] using hbr
        have hih_ex : ∀ bnext, bnext.length = g.buf_cap.val →
            ∃ bufF, ScanExec g s accRes.1 accRes.2 bnext stF actF bufF ∧ P bufF := by
          intro bnext hbn
          rw [hacc_spec, hact_eq_addr]
          have ⟨hih1, _⟩ := ih htail _ bnext hst1_inv rfl hreg_st hnxt_val (by omega) hbn
          exact hih1 P stF actF stNext actNext (by rw [hst_end_eq]; exact hfin) hcont
        rcases hstep_to_acc buf1 hbr with ⟨_, hst1⟩ | ⟨hgt_n, hst1, hst2⟩
        · obtain ⟨bufF, hexF, hPF⟩ := hih_ex buf1 (by rw [hbr.1, hbuf])
          exact ⟨bufF, .stepRead hread_valid hbuf_in hbr' hst1 hexF, hPF⟩
        · have hblen_al : (e.record.recordLen.val - 16) % max (max g.write_size.val g.read_size.val) 4 = 0 := by
            have d1 := Nat.dvd_of_mod_eq_zero hvalid_e.2.2.1
            have d2 := Nat.dvd_of_mod_eq_zero hp.hdr_aligned
            exact Nat.mod_eq_zero_of_dvd (Nat.dvd_sub d1 d2)
          have hrv2 := readValid_record_slice g r off 16 (e.record.recordLen.val - 16) hlay hdiv hr_le
            (by rw [← hoff_e]; exact hvalid_e.2.1) hp.hdr_aligned hblen_al (by have := hvalid_e.2.2.2.2.2.1; omega)
          have haddr2_val : (u32OfNat (regionStartNat g r + off + 16)).val = regionStartNat g r + off + 16 := by
            have hlt : regionStartNat g r + off + 16 < U32.size := by
              have hcap := hp.in_capacity; have hcap_lt := u32_val_lt g.capacity
              have hsz : U32.size = 4294967296 := by scalar_tac
              unfold regionStartNat; split <;> omega
            exact u32OfNat_val hlt
          have hlen2_val : (u32OfNat (e.record.recordLen.val - 16)).val = e.record.recordLen.val - 16 := by
            have hlt : e.record.recordLen.val - 16 < U32.size := by have := u32_val_lt e.record.recordLen; have hsz : U32.size = 4294967296 := (by scalar_tac); omega
            exact u32OfNat_val hlt
          have hfit_buf2 : 16 + (e.record.recordLen.val - 16) ≤ buf1.length := by
            rw [hbr.1, hbuf]; have := hvalid_e.2.2.2.2.1; omega
          obtain ⟨buf2, hbr2⟩ := bufAfterRead_exists s (regionStartNat g r + off + 16) 16 (e.record.recordLen.val - 16) buf1 hfit_buf2
          have hbr2' : BufAfterRead s (u32OfNat (regionStartNat g r + off + 16)).val HDR_U32.val
              (u32OfNat (e.record.recordLen.val - 16)).val buf1 buf2 := by
            simpa [haddr2_val, hlen2_val, hh] using hbr2
          obtain ⟨bufF, hexF, hPF⟩ := hih_ex buf2 (by rw [hbr2.1, hbr.1, hbuf])
          exact ⟨bufF, .stepRead hread_valid hbuf_in hbr' hst1
            (.stepRead (by rw [haddr2_val, hlen2_val]; exact hrv2) (by rw [hh, hlen2_val]; exact hfit_buf2)
              hbr2' (hst2 buf2 hbr2) hexF), hPF⟩
      · intro stF actF bufF hex
        cases hex with
        | stepRead _ _ hbr' hst1 hrest =>
          have h0v : (0#u32).val = 0 := by simp
          rw [haddr_val, h0v, hh] at hbr'
          rcases hstep_to_acc _ hbr' with ⟨_, hst_eq1⟩ | ⟨hgt_n, hst_eq1, hst2⟩
          · rw [hst_eq1, Result.ok.injEq, Prod.mk.injEq] at hst1
            obtain ⟨h1, h2⟩ := hst1; subst h1; subst h2
            rw [hacc_spec] at hrest
            rw [hact_eq_addr] at hrest
            simpa [hst_end_eq] using
              (ih htail _ _ hst1_inv rfl hreg_st hnxt_val (by omega) (by rw [hbr'.1, hbuf])).2 stF actF bufF hrest
          · rw [hst_eq1, Result.ok.injEq, Prod.mk.injEq] at hst1
            obtain ⟨h1, h2⟩ := hst1; subst h1; subst h2
            cases hrest with
            | stepRead _ _ hbr2' hst_2 hrest2 =>
              have haddr2_val : (u32OfNat (regionStartNat g r + off + 16)).val = regionStartNat g r + off + 16 := by
                have hlt : regionStartNat g r + off + 16 < U32.size := by
                  have hcap := hp.in_capacity; have hcap_lt := u32_val_lt g.capacity
                  have hsz : U32.size = 4294967296 := by scalar_tac
                  unfold regionStartNat; split <;> omega
                exact u32OfNat_val hlt
              have hlen2_val : (u32OfNat (e.record.recordLen.val - 16)).val = e.record.recordLen.val - 16 := by
                have hlt : e.record.recordLen.val - 16 < U32.size := by have := u32_val_lt e.record.recordLen; have hsz : U32.size = 4294967296 := (by scalar_tac); omega
                exact u32OfNat_val hlt
              rw [haddr2_val, hlen2_val, hh] at hbr2'
              rw [hst2 _ hbr2', Result.ok.injEq, Prod.mk.injEq] at hst_2
              obtain ⟨h1', h2'⟩ := hst_2; subst h1'; subst h2'
              rw [hacc_spec] at hrest2
              rw [hact_eq_addr] at hrest2
              simpa [hst_end_eq] using
                (ih htail _ _ hst1_inv rfl hreg_st hnxt_val (by omega) (by rw [hbr2'.1, hbr'.1, hbuf])).2 stF actF bufF hrest2
    · -- `nextOff + 16 > g.region.val`: so `rest` MUST be `[]`, `endOff = nextOff`, and `tail` MUST be `.erased`!
      simp only [hcont16, ↓reduceIte] at hacc_spec
      have hrest_nil : rest = [] := by
        cases hrest_log with
        | nil => rfl
        | cons _ _ _ hv_next _ _ _ =>
          have := hv_next.2.2.2.1; have := hv_next.2.2.2.2.2.1; omega
      subst hrest_nil
      have hend_eq : endOff = nextOff := by
        cases hrest_log; rfl
      subst hend_eq
      have htail_er : tail = .erased := by
        cases tail with
        | erased => rfl
        | torn => have := htail.1; omega
      subst htail_er
      have hstEnd_eq : { st with off := u32OfNat (st.off.val + e.record.recordLen.val), best := bestFoldStep st.best e } =
          { st with off := u32OfNat endOff, best := bestFold st.best [e] } := by
        simp only [bestFold, hoff_st, hnext_e]
      rw [hstEnd_eq] at hacc_spec
      constructor
      · intro P stF actF stNext actNext hfin hcont
        have hres_eq : accRes = (stNext, actNext) := by
          simp only [reduceCtorEq, decide_false] at hfin
          rw [hacc_spec, Result.ok.injEq] at hfin; exact hfin
        obtain ⟨buf1, hbr⟩ := bufAfterRead_exists s (regionStartNat g r + off) 0 16 buf (by rw [hbuf]; have := hp.buf_ge16; omega)
        have hbr' : BufAfterRead s (u32OfNat (regionStartNat g r + off)).val (0#u32).val HDR_U32.val buf buf1 := by
          simpa [haddr_val, hh] using hbr
        rcases hstep_to_acc buf1 hbr with ⟨_, hst1⟩ | ⟨hgt_n, hst1, hst2⟩
        · rw [hres_eq] at hst1
          obtain ⟨bufF, hexF, hPF⟩ := hcont buf1 (by rw [hbr.1, hbuf])
          exact ⟨bufF, .stepRead hread_valid hbuf_in hbr' hst1 hexF, hPF⟩
        · have hblen_al : (e.record.recordLen.val - 16) % max (max g.write_size.val g.read_size.val) 4 = 0 := by
            have d1 := Nat.dvd_of_mod_eq_zero hvalid_e.2.2.1
            have d2 := Nat.dvd_of_mod_eq_zero hp.hdr_aligned
            exact Nat.mod_eq_zero_of_dvd (Nat.dvd_sub d1 d2)
          have hrv2 := readValid_record_slice g r off 16 (e.record.recordLen.val - 16) hlay hdiv hr_le
            (by rw [← hoff_e]; exact hvalid_e.2.1) hp.hdr_aligned hblen_al (by have := hvalid_e.2.2.2.2.2.1; omega)
          have haddr2_val : (u32OfNat (regionStartNat g r + off + 16)).val = regionStartNat g r + off + 16 := by
            have hlt : regionStartNat g r + off + 16 < U32.size := by
              have hcap := hp.in_capacity; have hcap_lt := u32_val_lt g.capacity
              have hsz : U32.size = 4294967296 := by scalar_tac
              unfold regionStartNat; split <;> omega
            exact u32OfNat_val hlt
          have hlen2_val : (u32OfNat (e.record.recordLen.val - 16)).val = e.record.recordLen.val - 16 := by
            have hlt : e.record.recordLen.val - 16 < U32.size := by have := u32_val_lt e.record.recordLen; have hsz : U32.size = 4294967296 := (by scalar_tac); omega
            exact u32OfNat_val hlt
          have hfit_buf2 : 16 + (e.record.recordLen.val - 16) ≤ buf1.length := by
            rw [hbr.1, hbuf]; have := hvalid_e.2.2.2.2.1; omega
          obtain ⟨buf2, hbr2⟩ := bufAfterRead_exists s (regionStartNat g r + off + 16) 16 (e.record.recordLen.val - 16) buf1 hfit_buf2
          have hbr2' : BufAfterRead s (u32OfNat (regionStartNat g r + off + 16)).val HDR_U32.val
              (u32OfNat (e.record.recordLen.val - 16)).val buf1 buf2 := by
            simpa [haddr2_val, hlen2_val, hh] using hbr2
          have hst2' := hst2 buf2 hbr2; rw [hres_eq] at hst2'
          obtain ⟨bufF, hexF, hPF⟩ := hcont buf2 (by rw [hbr2.1, hbr.1, hbuf])
          exact ⟨bufF, .stepRead hread_valid hbuf_in hbr' hst1
            (.stepRead (by rw [haddr2_val, hlen2_val]; exact hrv2) (by rw [hh, hlen2_val]; exact hfit_buf2)
              hbr2' hst2' hexF), hPF⟩
      · intro stF actF bufF hex
        cases hex with
        | stepRead _ _ hbr' hst1 hrest =>
          have h0v : (0#u32).val = 0 := by simp
          rw [haddr_val, h0v, hh] at hbr'
          rcases hstep_to_acc _ hbr' with ⟨_, hst_eq1⟩ | ⟨hgt_n, hst_eq1, hst2⟩
          · rw [hst_eq1, Result.ok.injEq] at hst1; subst hst1
            exact ⟨_, _, _, by simpa using hacc_spec, by rw [hbr'.1, hbuf], hrest⟩
          · rw [hst_eq1, Result.ok.injEq, Prod.mk.injEq] at hst1
            obtain ⟨h1, h2⟩ := hst1; subst h1; subst h2
            cases hrest with
            | stepRead _ _ hbr2' hst_2 hrest2 =>
              have haddr2_val : (u32OfNat (regionStartNat g r + off + 16)).val = regionStartNat g r + off + 16 := by
                have hlt : regionStartNat g r + off + 16 < U32.size := by
                  have hcap := hp.in_capacity; have hcap_lt := u32_val_lt g.capacity
                  have hsz : U32.size = 4294967296 := by scalar_tac
                  unfold regionStartNat; split <;> omega
                exact u32OfNat_val hlt
              have hlen2_val : (u32OfNat (e.record.recordLen.val - 16)).val = e.record.recordLen.val - 16 := by
                have hlt : e.record.recordLen.val - 16 < U32.size := by have := u32_val_lt e.record.recordLen; have hsz : U32.size = 4294967296 := (by scalar_tac); omega
                exact u32OfNat_val hlt
              rw [haddr2_val, hlen2_val, hh] at hbr2'
              rw [hst2 _ hbr2', Result.ok.injEq] at hst_2; subst hst_2
              exact ⟨_, _, _, by simpa using hacc_spec, by rw [hbr2'.1, hbr'.1, hbuf], hrest2⟩

/-- Full two-region physical flash layout invariant connecting physical `NorFlash.FlashState`
to the logical sequence-number window across both regions. -/
structure FlashLayout (g : FlashGeometry) (s : NorFlash.FlashState) where
  log0 : List LogEntry
  end0 : Nat
  tail0 : RegionTail
  log1 : List LogEntry
  end1 : Nat
  tail1 : RegionTail
  hlog0 : ContiguousLog g s 0 0 log0 end0
  htail0 : TailValid g s 0 end0 tail0
  hlog1 : ContiguousLog g s 1 0 log1 end1
  htail1 : TailValid g s 1 end1 tail1

/-- **Two-region sequence-number window invariant**:
Either both regions are empty (`topOpt = none`), or `top` is the last entry of
its active region `top.record.region`, every entry in the active region has
`loAct ≤ e.idx ≤ top.idx` with `top.idx - loAct < actLog.length` (and strictly
`< top.idx` for all entries before `top`), and every entry in the standby region
has `e.idx < loAct` with `loAct - e.idx ≤ g.region.val / 16`.
Since `g.region.val ≤ 0x3FFF_FFFF`, every older entry on flash lies within
`< 2 * (g.region.val / 16) < 2^27 < 2^31` of `top.idx`! -/
def LayoutSeqWindow (g : FlashGeometry) (s : NorFlash.FlashState) (L : FlashLayout g s)
    (topOpt : Option LogEntry) : Prop :=
  match topOpt with
  | none => L.log0 = [] ∧ L.log1 = []
  | some top =>
    let actLog := if top.record.region.val = 0 then L.log0 else L.log1
    let stdLog := if top.record.region.val = 0 then L.log1 else L.log0
    ∃ loAct : Nat,
      top.record.region.val ≤ 1 ∧
      top.record.seq.val = top.idx % 4294967296 ∧
      actLog.getLast? = some top ∧
      (∀ e ∈ actLog, loAct ≤ e.idx ∧ e.idx ≤ top.idx ∧ (e = top ∨ e.idx < top.idx)) ∧
      top.idx - loAct < actLog.length ∧
      (∀ e ∈ stdLog, e.idx < loAct ∧ loAct - e.idx ≤ g.region.val / 16)

private theorem layoutSeqWindow_top_unique_max
    {g : FlashGeometry} {s : NorFlash.FlashState} {L : FlashLayout g s} {top : LogEntry}
    (hlay : layout_ok g = ok true)
    (hwin : LayoutSeqWindow g s L (some top)) :
    top ∈ L.log0 ++ L.log1 ∧
    (∀ e ∈ L.log0 ++ L.log1, e.record.seq.val = e.idx % 4294967296) ∧
    (∀ e ∈ L.log0 ++ L.log1, e = top ∨ (e.idx < top.idx ∧ top.idx - e.idx < 2147483648)) := by
  have hp := H5iAppLib.post_of_ok (layout_ok_spec g) hlay rfl
  have hb0 := contiguousLog_bounds L.hlog0
  have hb1 := contiguousLog_bounds L.hlog1
  have hlen0 : L.log0.length ≤ g.region.val / 16 := by omega
  have hlen1 : L.log1.length ≤ g.region.val / 16 := by omega
  have hreg_max := hp.region_le_max
  have hdiv_bound : g.region.val / 16 ≤ 67108863 := by omega
  have hseq_log : ∀ {r startOff endOff} {es : List LogEntry},
      ContiguousLog g s r startOff es endOff → ∀ e ∈ es, e.record.seq.val = e.idx % 4294967296 := by
    intro r startOff endOff es hl
    induction hl with
    | nil => intro _ h; cases h
    | cons _ _ hidx _ _ _ _ ih =>
      intro e he
      rcases List.mem_cons.mp he with rfl | hmem
      · exact hidx
      · exact ih e hmem
  have hseq_all : ∀ e ∈ L.log0 ++ L.log1, e.record.seq.val = e.idx % 4294967296 := by
    intro e he
    rcases List.mem_append.mp he with h0 | h1
    · exact hseq_log L.hlog0 e h0
    · exact hseq_log L.hlog1 e h1
  obtain ⟨loAct, htr, htop_seq, hlast, hact_all, hspan, hstd_all⟩ := hwin
  have hmem_of_last : ∀ {α : Type} {l : List α} {x : α}, l.getLast? = some x → x ∈ l := by
    intro α l x hl
    cases l with
    | nil => simp at hl
    | cons a xs =>
      rw [List.getLast?_eq_some_getLast (List.cons_ne_nil a xs), Option.some.injEq] at hl
      subst hl; exact List.getLast_mem _
  by_cases hr0 : top.record.region.val = 0
  · simp only [hr0, ↓reduceIte] at hlast hact_all hspan hstd_all
    have hlo_le_top : loAct ≤ top.idx := (hact_all top (hmem_of_last hlast)).1
    refine ⟨List.mem_append_left _ (hmem_of_last hlast), hseq_all, ?_⟩
    intro e he
    rcases List.mem_append.mp he with h0 | h1
    · obtain ⟨hlo, hhi, hdisj⟩ := hact_all e h0
      rcases hdisj with rfl | hlt
      · exact Or.inl rfl
      · exact Or.inr ⟨hlt, by omega⟩
    · obtain ⟨hlt, hdist⟩ := hstd_all e h1
      exact Or.inr ⟨by omega, by omega⟩
  · simp only [hr0, ↓reduceIte] at hlast hact_all hspan hstd_all
    have hlo_le_top : loAct ≤ top.idx := (hact_all top (hmem_of_last hlast)).1
    refine ⟨List.mem_append_right _ (hmem_of_last hlast), hseq_all, ?_⟩
    intro e he
    rcases List.mem_append.mp he with h0 | h1
    · obtain ⟨hlt, hdist⟩ := hstd_all e h0
      exact Or.inr ⟨by omega, by omega⟩
    · obtain ⟨hlo, hhi, hdisj⟩ := hact_all e h1
      rcases hdisj with rfl | hlt
      · exact Or.inl rfl
      · exact Or.inr ⟨hlt, by omega⟩

/-- Expected `Pos` produced by `scan_result_pos` for a winning record `top` on layout `L`. -/
def expectedRecoveredPos (g : FlashGeometry) (s : NorFlash.FlashState)
    (L : FlashLayout g s) (topOpt : Option LogEntry) : Pos :=
  match topOpt with
  | none => { region := 1#u32, append := none, seq := 0#u32 }
  | some top =>
    let endOff := if top.record.region.val = 0 then L.end0 else L.end1
    let tail := if top.record.region.val = 0 then L.tail0 else L.tail1
    { region := top.record.region,
      append := if tail = .torn then none else some (u32OfNat endOff),
      seq := top.record.seq }

/-- **Full physical recovery functional correctness & determinism** over the
**actual Aeneas-extracted `store_core.scan_init` and `store_core.scan_step`**:
On ANY physical NOR flash `s` satisfying `FlashLayout g s` and `LayoutSeqWindow g s L topOpt`,
for ANY initial buffer `buf0` of length `g.buf_cap.val`:
1. (Existence) `RecoverFromFlash g s buf0` ALWAYS terminates cleanly at the exact
   `expectedRecoveredPos`, with `bestVer`/`bestLen` and exact record bytes in `finalBuf`.
2. (Determinism) EVERY `RecoverFromFlash g s buf0` run MUST return that exact
   `expectedRecoveredPos`, `bestVer`, `bestLen`, and exact record bytes in `finalBuf`! -/
theorem recover_from_flash_correct
    (g : FlashGeometry) (s : NorFlash.FlashState) (L : FlashLayout g s)
    (topOpt : Option LogEntry) (buf0 : Slice U8)
    (hlay : layout_ok g = ok true) (hdiv : AlignDivides g)
    (hwin : LayoutSeqWindow g s L topOpt) (hbuf0 : buf0.length = g.buf_cap.val) :
    let expPos := expectedRecoveredPos g s L topOpt
    let expVer := topOpt.map (fun e => e.record.version)
    let expLen := match topOpt with | none => 0#u32 | some e => e.record.payloadLen
    (∃ finalBuf,
      RecoverFromFlash g s buf0 expPos expVer expLen finalBuf ∧
      finalBuf.length = g.buf_cap.val ∧
      (∀ top, topOpt = some top → ∀ i, i < top.record.recordLen.val →
        finalBuf.val[i]? = some (u8OfUInt8 (top.record.bytes.getD i 0)))) ∧
    (∀ pos' ver' len' finalBuf,
      RecoverFromFlash g s buf0 pos' ver' len' finalBuf →
      pos' = expPos ∧ ver' = expVer ∧ len' = expLen ∧
      finalBuf.length = g.buf_cap.val ∧
      (∀ top, topOpt = some top → ∀ i, i < top.record.recordLen.val →
        finalBuf.val[i]? = some (u8OfUInt8 (top.record.bytes.getD i 0)))) := by
  have hp := H5iAppLib.post_of_ok (layout_ok_spec g) hlay rfl
  have hcap_u32 : g.base.val + g.region.val ≤ U32.max := by have := hp.in_capacity; scalar_tac
  have hh := hdr_u32_val
  -- 1. Evaluate `scan_init g`
  obtain ⟨rs0, hrs0, hrs0_val⟩ := H5iAppLib.spec_exists_ok (region_start_spec g 0#u32 hcap_u32)
  have h0v : (0#u32).val = 0 := by simp
  rw [h0v] at hrs0_val
  have hrs0_eq : rs0 = u32OfNat (regionStartNat g 0 + 0) := by
    apply UScalar.eq_of_val_eq
    have hlt : regionStartNat g 0 + 0 < U32.size := by have := hp.in_capacity; unfold regionStartNat; simp; scalar_tac
    rw [u32OfNat_val hlt, hrs0_val]; simp
  let st0 : ScanState := { region := 0#u32, off := 0#u32, best := none, end0 := 0#u32,
                           torn0 := false, end1 := 0#u32, torn1 := false, phase := .ReadHeader }
  have hinit_eq : scan_init g = ok (st0, .Read (u32OfNat (regionStartNat g 0 + 0)) 0#u32 HDR_U32) := by
    unfold scan_init
    simp only [hlay, bind_ok, ↓reduceIte, hrs0, hrs0_eq, st0]
  have hst0_inv : ScanInv g st0 := ⟨by simp [st0], by simp [st0], by simp [st0],
    (fun _ h => by cases h), (fun _ h => by cases h)⟩
  -- 2. Evaluate `finish_region` at end of Region 0 -> Region 1 start
  obtain ⟨rs1, hrs1, hrs1_val⟩ := H5iAppLib.spec_exists_ok (region_start_spec g 1#u32 hcap_u32)
  have h1v : (1#u32).val = 1 := by simp
  rw [h1v] at hrs1_val
  have hrs1_eq : rs1 = u32OfNat (regionStartNat g 1 + 0) := by
    apply UScalar.eq_of_val_eq
    have hlt : regionStartNat g 1 + 0 < U32.size := by have := hp.in_capacity; unfold regionStartNat; simp; scalar_tac
    rw [u32OfNat_val hlt, hrs1_val]; simp
  let best0 := bestFold none L.log0
  let st0End : ScanState := { st0 with off := u32OfNat L.end0, best := best0 }
  let torn0B := decide (L.tail0 = .torn)
  let st1 : ScanState := { st0End with end0 := u32OfNat L.end0, torn0 := torn0B, region := 1#u32, off := 0#u32, phase := .ReadHeader }
  have hfin0_eq : finish_region g st0End torn0B =
      ok (st1, .Read (u32OfNat (regionStartNat g 1 + 0)) 0#u32 HDR_U32) := by
    unfold finish_region
    simp only [st0End, st0, ↓reduceIte, hrs1, hrs1_eq, st1, bind_ok]
  have hbest_inv_log : ∀ {r startOff endOff} {es : List LogEntry} {b0 : Option BestRecord},
      ContiguousLog g s r startOff es endOff →
      (∀ b, b0 = some b → b.region.val ≤ 1 ∧ b.off.val % max (max g.write_size.val g.read_size.val) 4 = 0 ∧
        b.record_len.val % max (max g.write_size.val g.read_size.val) 4 = 0 ∧
        16 ≤ b.record_len.val ∧ b.record_len.val ≤ g.buf_cap.val ∧ b.off.val + b.record_len.val ≤ g.region.val) →
      ∀ b, bestFold b0 es = some b →
        b.region.val ≤ 1 ∧ b.off.val % max (max g.write_size.val g.read_size.val) 4 = 0 ∧
        b.record_len.val % max (max g.write_size.val g.read_size.val) 4 = 0 ∧
        16 ≤ b.record_len.val ∧ b.record_len.val ≤ g.buf_cap.val ∧ b.off.val + b.record_len.val ≤ g.region.val := by
    intro r startOff endOff es b0 hlog
    induction hlog generalizing b0 with
    | nil => exact fun hb => hb
    | @cons _ _ _ e _ _ _ _ hv _ _ _ ih =>
      intro hb0
      apply ih
      intro b hb
      cases h0 : b0 with
      | none =>
        rw [h0] at hb
        simp [bestFoldStep, toBestRecord] at hb; subst hb
        exact ⟨hv.1, hv.2.1, hv.2.2.1, hv.2.2.2.1, hv.2.2.2.2.1, hv.2.2.2.2.2.1⟩
      | some prev =>
        rw [h0] at hb
        cases hsn_b : seqNewerBool e.record.seq prev.seq <;> simp [bestFoldStep, hsn_b, toBestRecord] at hb
        · subst hb; exact hb0 prev h0
        · subst hb; exact ⟨hv.1, hv.2.1, hv.2.2.1, hv.2.2.2.1, hv.2.2.2.2.1, hv.2.2.2.2.2.1⟩
  have hst1_inv : ScanInv g st1 := ⟨by simp [st1], by simp [st1], by simp [st1],
    hbest_inv_log L.hlog0 (fun _ h => by cases h), fun _ h => by cases h⟩
  -- 3. Combine both regions' `bestFold`
  let bestAll := bestFold best0 L.log1
  have hbestAll_eq : bestAll = bestFold none (L.log0 ++ L.log1) := by
    simp [bestAll, best0, bestFold_append]
  let st1End : ScanState := { st1 with off := u32OfNat L.end1, best := bestAll }
  let torn1B := decide (L.tail1 = .torn)
  -- Helper: from `st1End`, `finish_region` + optional `ReloadBest` reaches the expected `Done`!
  have hreg0_spec := scan_region_spec g s hlay hdiv (by omega) L.hlog0 L.htail0 st0 buf0
    hst0_inv rfl (by simp [st0]) (by simp [st0]) (by have := hp.region_ge16; omega) hbuf0
  have hreg1_spec : ∀ buf1, buf1.length = g.buf_cap.val →
      (∀ (P : Slice U8 → Prop) stF actF stNext actNext,
        finish_region g st1End torn1B = ok (stNext, actNext) →
        (∀ b2, b2.length = g.buf_cap.val → ∃ bufF, ScanExec g s stNext actNext b2 stF actF bufF ∧ P bufF) →
        ∃ bufF, ScanExec g s st1 (.Read (u32OfNat (regionStartNat g 1 + 0)) 0#u32 HDR_U32) buf1 stF actF bufF ∧ P bufF) ∧
      (∀ stF actF bufF,
        ScanExec g s st1 (.Read (u32OfNat (regionStartNat g 1 + 0)) 0#u32 HDR_U32) buf1 stF actF bufF →
        ∃ stNext actNext b2,
          finish_region g st1End torn1B = ok (stNext, actNext) ∧
          b2.length = g.buf_cap.val ∧
          ScanExec g s stNext actNext b2 stF actF bufF) :=
    fun buf1 hb1 => scan_region_spec g s hlay hdiv (by omega) L.hlog1 L.htail1 st1 buf1
      hst1_inv rfl (by simp [st1]) (by simp [st1]) (by have := hp.region_ge16; omega) hb1
  cases topOpt with
  | none =>
    obtain ⟨hl0, hl1⟩ := hwin
    have hba_none : bestAll = none := by rw [hbestAll_eq, hl0, hl1]; rfl
    let stDone : ScanState := { st1End with end1 := u32OfNat L.end1, torn1 := torn1B, phase := .Done }
    have hfin1_eq : finish_region g st1End torn1B =
        ok (stDone, .Done { region := 1#u32, append := none, seq := 0#u32 } none 0#u32) := by
      unfold finish_region
      have hne0 : st1End.region ≠ 0#u32 := by
        intro h; have := congrArg UScalar.val h; simp [st1End, st1] at this
      simp only [hne0, ↓reduceIte, st1End, hba_none, stDone]
    constructor
    · obtain ⟨finalBuf, hex, hlenF⟩ :=
        hreg0_spec.1 (fun b => b.length = g.buf_cap.val) stDone
          (.Done { region := 1#u32, append := none, seq := 0#u32 } none 0#u32) st1 _ hfin0_eq
          (fun b1 hb1 => (hreg1_spec b1 hb1).1 (fun b => b.length = g.buf_cap.val) stDone _ stDone _ hfin1_eq
            (fun b2 hb2 => ⟨b2, .done, hb2⟩))
      exact ⟨finalBuf, ⟨st0, _, stDone, hinit_eq, hex⟩, hlenF, fun _ h => by cases h⟩
    · intro pos' ver' len' finalBuf ⟨st0', act0', stF', hinit', hex'⟩
      rw [hinit_eq, Result.ok.injEq, Prod.mk.injEq] at hinit'
      obtain ⟨h1, h2⟩ := hinit'; subst h1; subst h2
      obtain ⟨stN1, actN1, b1, hfin0', hb1, hex1⟩ := hreg0_spec.2 stF' _ finalBuf hex'
      rw [hfin0_eq, Result.ok.injEq, Prod.mk.injEq] at hfin0'
      obtain ⟨h3, h4⟩ := hfin0'; subst h3; subst h4
      obtain ⟨stN2, actN2, b2, hfin1', hb2, hex2⟩ := (hreg1_spec b1 hb1).2 stF' _ finalBuf hex1
      rw [hfin1_eq, Result.ok.injEq, Prod.mk.injEq] at hfin1'
      obtain ⟨h5, h6⟩ := hfin1'; subst h5; subst h6
      cases hex2
      exact ⟨rfl, rfl, rfl, hb2, fun _ h => by cases h⟩
  | some top =>
    have ⟨hmem_top, hseq_all, hmax_all⟩ := layoutSeqWindow_top_unique_max hlay hwin
    have hba_some : bestAll = some (toBestRecord top.record) := by
      rw [hbestAll_eq]
      exact bestFold_selects_unique_max (L.log0 ++ L.log1) top hmem_top hseq_all hmax_all
    -- Extract `ValidRecordBytes` and `RecordIntact` for `top.record` from whichever region holds `top`.
    have hmem_props : ∀ {r startOff endOff} {es : List LogEntry},
        ContiguousLog g s r startOff es endOff →
        ∀ e ∈ es, ValidRecordBytes g e.record ∧ RecordIntact g s e.record ∧ e.record.region.val = r := by
      intro r startOff endOff es hl
      induction hl with
      | nil => intro _ h; cases h
      | cons hrg _ _ hv ha _ _ ih =>
        intro e he
        rcases List.mem_cons.mp he with rfl | hrest
        · exact ⟨hv, ha, hrg⟩
        · exact ih e hrest
    have ⟨hvalid_top, hintact_top⟩ : ValidRecordBytes g top.record ∧ RecordIntact g s top.record := by
      rcases List.mem_append.mp hmem_top with h0 | h1
      · have h := hmem_props L.hlog0 top h0; exact ⟨h.1, h.2.1⟩
      · have h := hmem_props L.hlog1 top h1; exact ⟨h.1, h.2.1⟩
    -- Compute `finish_region` at end of Region 1 -> `ReloadBest`
    obtain ⟨rsT, hrsT, hrsT_val⟩ := H5iAppLib.spec_exists_ok (region_start_spec g top.record.region hcap_u32)
    have hrsT_cap : rsT.val + top.record.off.val ≤ U32.max := by
      rw [hrsT_val]; unfold regionStartNat; split <;> have := hp.in_capacity <;> have := hvalid_top.2.2.2.2.2.1 <;> scalar_tac
    obtain ⟨raddr, hraddr, hraddr_val⟩ := u32_add_ok (a := rsT) (b := top.record.off) hrsT_cap
    let stReload : ScanState := { st1End with end1 := u32OfNat L.end1, torn1 := torn1B, phase := .ReloadBest }
    have hfin1_eq : finish_region g st1End torn1B =
        ok (stReload, .Read raddr 0#u32 top.record.recordLen) := by
      unfold finish_region
      have hne0 : st1End.region ≠ 0#u32 := by
        intro h; have := congrArg UScalar.val h; simp [st1End, st1] at this
      simp only [hne0, ↓reduceIte, st1End, hba_some, toBestRecord, hrsT, hraddr, bind_ok, stReload]
    -- Compute `scan_result_pos` for `top`!
    have hlast_end : top.record.off.val + top.record.recordLen.val =
        (if top.record.region.val = 0 then L.end0 else L.end1) := by
      obtain ⟨_, _, _, hlast, _, _, _⟩ := hwin
      by_cases hr0 : top.record.region.val = 0
      · simp only [hr0, ↓reduceIte] at hlast ⊢
        exact contiguousLog_last_end L.hlog0 hlast
      · simp only [hr0, ↓reduceIte] at hlast ⊢
        exact contiguousLog_last_end L.hlog1 hlast
    have hpos_eq : scan_result_pos { stReload with phase := .Done } (toBestRecord top.record) =
        ok (expectedRecoveredPos g s L (some top)) := by
      have hb0 := contiguousLog_bounds L.hlog0
      have hb1 := contiguousLog_bounds L.hlog1
      obtain ⟨sum, hadd, hsum_val⟩ := u32_add_ok (a := top.record.off) (b := top.record.recordLen) (by
        have := hvalid_top.2.2.2.2.2.1; scalar_tac)
      unfold scan_result_pos expectedRecoveredPos
      by_cases hr0 : top.record.region = 0#u32
      · have hr0n : top.record.region.val = 0 := by simpa using congrArg UScalar.val hr0
        simp only [toBestRecord, hr0, hr0n, ↓reduceIte, stReload, st1End, st1, hadd, bind_ok]
        have hsum_end0 : sum = u32OfNat L.end0 := by
          apply UScalar.eq_of_val_eq
          have hlt : L.end0 < U32.size := by scalar_tac
          rw [u32OfNat_val hlt, hsum_val]; simpa [hr0n] using hlast_end
        simp only [hsum_end0, beq_self_eq_true, Bool.not_true, Bool.or_false, torn0B]
        cases L.tail0 <;> simp
      · have hr0n : top.record.region.val ≠ 0 := fun h => hr0 (UScalar.eq_of_val_eq (by simpa using h))
        simp only [toBestRecord, hr0, hr0n, ↓reduceIte, stReload, st1End, st1, hadd, bind_ok]
        have hsum_end1 : sum = u32OfNat L.end1 := by
          apply UScalar.eq_of_val_eq
          have hlt : L.end1 < U32.size := by scalar_tac
          rw [u32OfNat_val hlt, hsum_val]; simpa [hr0n] using hlast_end
        simp only [hsum_end1, beq_self_eq_true, Bool.not_true, Bool.or_false, torn1B]
        cases L.tail1 <;> simp
    -- Reload step on `stReload`!
    have hrv_top : NorFlash.ReadValid (toFlashConfig g) raddr.val top.record.recordLen.val := by
      have hrv0 := readValid_record_slice g top.record.region.val top.record.off.val 0 top.record.recordLen.val
        hlay hdiv hvalid_top.1 hvalid_top.2.1 (by simp) hvalid_top.2.2.1 (by have := hvalid_top.2.2.2.2.2.1; omega)
      simpa [hraddr_val, hrsT_val] using hrv0
    have hreload_step : ∀ b2 b3, b2.length = g.buf_cap.val →
        BufAfterRead s raddr.val (0#u32).val top.record.recordLen.val b2 b3 →
        (∀ i, i < top.record.recordLen.val → b3.val[i]? = some (u8OfUInt8 (top.record.bytes.getD i 0))) ∧
        scan_step g stReload b3 =
          ok ({ stReload with phase := .Done },
              .Done (expectedRecoveredPos g s L (some top)) (some top.record.version) top.record.payloadLen) := by
      intro b2 b3 hb2 hbr3
      have hbytes3 : ∀ i, i < top.record.recordLen.val →
          b3.val[i]? = some (u8OfUInt8 (top.record.bytes.getD i 0)) := by
        intro i hi
        have h0 : (0#u32).val = 0 := by simp
        rw [hbr3.2.1 i (by omega) (by omega)]
        have hmem := recordIntact_mem_eq g s top.record hintact_top hi
        have heq_addr : raddr.val + (i - (0#u32).val) = regionStartNat g top.record.region.val + top.record.off.val + i := by
          rw [hraddr_val, hrsT_val, h0]; omega
        rw [heq_addr, hmem]
      have hcrc3 := hvalid_top.2.2.2.2.2.2.2.2.2.2 b3 (by rw [hbr3.1, hb2]) hbytes3
      refine ⟨hbytes3, ?_⟩
      have hsr_ph : stReload.phase = .ReloadBest := rfl
      have hsr_best : stReload.best = some (toBestRecord top.record) := hba_some
      have hpos_eq' : scan_result_pos { stReload with best := some (toBestRecord top.record), phase := .Done }
          (toBestRecord top.record) = ok (expectedRecoveredPos g s L (some top)) := by
        simpa [hsr_best] using hpos_eq
      unfold scan_step
      simp only [hlay, bind_ok, ↓reduceIte, hsr_ph, hsr_best]
      rw [hpos_eq']
      simp only [bind_ok, toBestRecord, hcrc3, ↓reduceIte]
    let Pgood : Slice U8 → Prop := fun b =>
      b.length = g.buf_cap.val ∧
      ∀ i, i < top.record.recordLen.val → b.val[i]? = some (u8OfUInt8 (top.record.bytes.getD i 0))
    constructor
    · obtain ⟨finalBuf, hex, hlenF, hbytesF⟩ :=
        hreg0_spec.1 Pgood { stReload with phase := .Done }
          (.Done (expectedRecoveredPos g s L (some top)) (some top.record.version) top.record.payloadLen)
          st1 _ hfin0_eq
          (fun b1 hb1 => (hreg1_spec b1 hb1).1 Pgood { stReload with phase := .Done } _ stReload _ hfin1_eq
            (fun b2 hb2 => by
              have hfit : (0#u32).val + top.record.recordLen.val ≤ b2.length := by
                have h0 : (0#u32).val = 0 := by simp
                rw [h0, Nat.zero_add, hb2]; exact hvalid_top.2.2.2.2.1
              obtain ⟨b3, hbr3⟩ := bufAfterRead_exists s raddr.val (0#u32).val top.record.recordLen.val b2 hfit
              obtain ⟨hbytes3, hst3⟩ := hreload_step b2 b3 hb2 hbr3
              exact ⟨b3, .stepRead hrv_top hfit hbr3 hst3 .done, by rw [hbr3.1, hb2], hbytes3⟩))
      exact ⟨finalBuf, ⟨st0, _, _, hinit_eq, hex⟩, hlenF, fun t ht => by cases ht; exact hbytesF⟩
    · intro pos' ver' len' finalBuf ⟨st0', act0', stF', hinit', hex'⟩
      rw [hinit_eq, Result.ok.injEq, Prod.mk.injEq] at hinit'
      obtain ⟨h1, h2⟩ := hinit'; subst h1; subst h2
      obtain ⟨stN1, actN1, b1, hfin0', hb1, hex1⟩ := hreg0_spec.2 stF' _ finalBuf hex'
      rw [hfin0_eq, Result.ok.injEq, Prod.mk.injEq] at hfin0'
      obtain ⟨h3, h4⟩ := hfin0'; subst h3; subst h4
      obtain ⟨stN2, actN2, b2, hfin1', hb2, hex2⟩ := (hreg1_spec b1 hb1).2 stF' _ finalBuf hex1
      rw [hfin1_eq, Result.ok.injEq, Prod.mk.injEq] at hfin1'
      obtain ⟨h5, h6⟩ := hfin1'; subst h5; subst h6
      cases hex2 with
      | stepRead _ _ hbr3 hst3 hrest3 =>
        obtain ⟨hbytes3, hst3_eq⟩ := hreload_step b2 _ hb2 hbr3
        rw [hst3_eq, Result.ok.injEq, Prod.mk.injEq] at hst3
        obtain ⟨h7, h8⟩ := hst3; subst h7; subst h8
        cases hrest3
        exact ⟨rfl, rfl, rfl, by rw [hbr3.1, hb2], fun t ht => by cases ht; exact hbytes3⟩

/-! ## 8. Compositional System Model (`ComposedStep`), Discharged Preconditions, & Recovery Connection -/

/-- **Assumption C1 (non-forgery under partial erase)**:
A partially erased standby region `r` leaves some subset/prefix of its previously
valid records intact and leaves the tail cleanly erased or torn (never forging a
brand-new CRC-valid record). -/
def PartialEraseNonForging (g : FlashGeometry) (s s' : NorFlash.FlashState) (r : Nat) : Prop :=
  ∀ log endOff tail,
    ContiguousLog g s r 0 log endOff → TailValid g s r endOff tail →
    ∃ log' end' tail',
      ContiguousLog g s' r 0 log' end' ∧ TailValid g s' r end' tail' ∧
      (∀ e ∈ log', e ∈ log)

/-- **Assumption C1 (non-forgery under partial write)**:
A partially written slot at `(tr, toff)` that does not complete the intended
record verbatim leaves the tail at `(tr, toff)` either untouched/erased or torn
(never forging a CRC-valid record). If all bytes of the intended record DID land
cleanly despite returning an error or being cancelled, that is modeled by
`ComposedStep.writeFailAmbiguousLanded`. -/
def PartialWriteNonForging (g : FlashGeometry) (s' : NorFlash.FlashState) (tr toff : Nat) : Prop :=
  TailValid g s' tr toff .erased ∨ TailValid g s' tr toff .torn

/-- Phase of the composed `FlashStore` + `NorFlash` system. -/
inductive SavePhase where
  | idle
  | erasing (efrom eto targetRegion waddr wlen nseq : U32) (inFlight : CommittedRecord)
  | writing (targetRegion targetOff waddr wlen nseq : U32) (inFlight : CommittedRecord)
  | writeLanded (targetRegion targetOff wlen nseq : U32) (inFlight : CommittedRecord)

/-- Full compositional state mirroring `FlashStore` in `store.rs`:
- `flash`: physical `NorFlash.FlashState`
- `pos`: `Option Pos` (invalidated to `none` before ANY flash mutation, forcing
  full `scan_init`/`scan_step` recovery before the next save after any error,
  cancellation, or reboot)
- `phase`: protocol phase (`idle`, `erasing`, `writing`, `writeLanded`)
- `acked`: last acknowledged (or recovered) committed record
- `landed`: unacknowledged record that physically landed on flash (either in
  `writeLanded` or after an ambiguous write failure / async cancellation). -/
structure ComposedState where
  flash : NorFlash.FlashState
  pos : Option Pos
  phase : SavePhase
  acked : Option CommittedRecord
  landed : Option CommittedRecord

/-- Initial erased NOR flash state with `pos = none` (matching `FlashStore::new`). -/
def composedInit : ComposedState where
  flash := { mem := fun _ => NorFlash.erasedByte }
  pos := none
  phase := .idle
  acked := none
  landed := none

/-- Compositional transition relation executing the **actual Aeneas-extracted**
`plan_save`, `encode_record`, `pos_after_write`, `scan_init`, and `scan_step`
against `NorFlash`.

Crucially:
- **Zero unproven safety preconditions** on `eraseOk` and `writeOk`: `EraseValid`,
  `WriteValid`, `isErasedRange`, `disjoint`, and `seq_newer` are NOT assumed here;
  they are PROVED as consequences of reachability (`composed_erase_preconditions_discharged`,
  `composed_write_preconditions_discharged`).
- **Real extracted recovery**: `recover` executes `RecoverFromFlash` (actual
  `scan_init` + `scan_step` on physical `cs.flash`), NOT ghost reconstruction.
- **Ambiguous write / async cancellation**: `writeFailAmbiguousLanded` models a
  write that physically programs the full record then returns `Err` or is dropped,
  leaving `pos := none` so the next save MUST run `recover` first. -/
inductive ComposedStep (g : FlashGeometry) : ComposedState → ComposedState → Prop where
  | beginSaveAppend {cs : ComposedState} {p : Pos} {buf buf' : Slice U8}
      {version : U16} {payload_len a tr toff waddr wlen nseq : U32}
      (hidle : cs.phase = .idle)
      (hpos : cs.pos = some p)
      (ha : align g.write_size g.read_size = ok a)
      (hplan : plan_save g p payload_len = ok (.WriteOnly tr toff waddr wlen nseq))
      (hbuf : buf.length = g.buf_cap.val)
      (henc : encode_record buf nseq version payload_len a = ok (some wlen, buf')) :
      ComposedStep g cs {
        cs with
        pos := none
        phase := .writing tr toff waddr wlen nseq {
          region := tr, off := toff, seq := nseq, version := version,
          payloadLen := payload_len, recordLen := wlen,
          bytes := slicePrefixToBytes buf' wlen.val
        }
      }
  | beginSaveSwitch {cs : ComposedState} {p : Pos} {buf buf' : Slice U8}
      {version : U16} {payload_len a efrom eto tr waddr wlen nseq : U32}
      (hidle : cs.phase = .idle)
      (hpos : cs.pos = some p)
      (ha : align g.write_size g.read_size = ok a)
      (hplan : plan_save g p payload_len = ok (.EraseThenWrite efrom eto tr waddr wlen nseq))
      (hbuf : buf.length = g.buf_cap.val)
      (henc : encode_record buf nseq version payload_len a = ok (some wlen, buf')) :
      ComposedStep g cs {
        cs with
        pos := none
        phase := .erasing efrom eto tr waddr wlen nseq {
          region := tr, off := 0#u32, seq := nseq, version := version,
          payloadLen := payload_len, recordLen := wlen,
          bytes := slicePrefixToBytes buf' wlen.val
        }
      }
  | eraseOk {cs : ComposedState} {efrom eto tr waddr wlen nseq : U32} {rec : CommittedRecord}
      (hph : cs.phase = .erasing efrom eto tr waddr wlen nseq rec) :
      ComposedStep g cs {
        cs with
        flash := NorFlash.eraseRange cs.flash efrom.val eto.val
        phase := .writing tr 0#u32 waddr wlen nseq rec
      }
  | eraseFailPowerCut {cs : ComposedState} {efrom eto tr waddr wlen nseq : U32} {rec : CommittedRecord}
      {flash' : NorFlash.FlashState}
      (hph : cs.phase = .erasing efrom eto tr waddr wlen nseq rec)
      (hIso : ∀ i, (i < efrom.val ∨ eto.val ≤ i) → flash'.mem i = cs.flash.mem i)
      (hC1 : PartialEraseNonForging g cs.flash flash' tr.val) :
      ComposedStep g cs { cs with flash := flash', pos := none, phase := .idle }
  | writeOk {cs : ComposedState} {tr toff waddr wlen nseq : U32} {rec : CommittedRecord}
      (hph : cs.phase = .writing tr toff waddr wlen nseq rec) :
      ComposedStep g cs {
        cs with
        flash := NorFlash.writeBytes cs.flash waddr.val rec.bytes
        phase := .writeLanded tr toff wlen nseq rec
        landed := some rec
      }
  | writeFailTorn {cs : ComposedState} {tr toff waddr wlen nseq : U32} {rec : CommittedRecord}
      {flash' : NorFlash.FlashState}
      (hph : cs.phase = .writing tr toff waddr wlen nseq rec)
      (hIso : ∀ i, (i < waddr.val ∨ waddr.val + wlen.val ≤ i) → flash'.mem i = cs.flash.mem i)
      (hNor : ∀ i, waddr.val ≤ i → i < waddr.val + wlen.val → (flash'.mem i &&& cs.flash.mem i) = flash'.mem i)
      (hC1 : PartialWriteNonForging g flash' tr.val toff.val) :
      ComposedStep g cs { cs with flash := flash', pos := none, phase := .idle }
  | writeFailAmbiguousLanded {cs : ComposedState} {tr toff waddr wlen nseq : U32} {rec : CommittedRecord}
      (hph : cs.phase = .writing tr toff waddr wlen nseq rec) :
      ComposedStep g cs {
        cs with
        flash := NorFlash.writeBytes cs.flash waddr.val rec.bytes
        pos := none
        phase := .idle
        landed := some rec
      }
  | ackSave {cs : ComposedState} {tr toff wlen nseq : U32} {rec : CommittedRecord} {pos' : Pos}
      (hph : cs.phase = .writeLanded tr toff wlen nseq rec)
      (hpos' : pos_after_write tr toff wlen nseq = ok pos') :
      ComposedStep g cs {
        cs with
        pos := some pos'
        phase := .idle
        acked := some rec
        landed := none
      }
  | recover {cs : ComposedState} {buf0 finalBuf : Slice U8}
      {pos' : Pos} {bestVer : Option U16} {bestLen : U32}
      (hbuf0 : buf0.length = g.buf_cap.val)
      (hrecov : RecoverFromFlash g cs.flash buf0 pos' bestVer bestLen finalBuf) :
      ComposedStep g cs {
        cs with
        pos := some pos'
        phase := .idle
        acked := match cs.landed with | some r => some r | none => cs.acked
        landed := none
      }

/-- Reachability closure of `ComposedStep`. -/
inductive ComposedReachable (g : FlashGeometry) : ComposedState → Prop where
  | init : ComposedReachable g composedInit
  | step {s s'} : ComposedReachable g s → ComposedStep g s s' → ComposedReachable g s'

/-- The latest valid committed record physically present on flash in state `cs`:
`cs.landed` if a new write has landed before acknowledgment/recovery, else `cs.acked`. -/
def latestOnFlash (cs : ComposedState) : Option CommittedRecord :=
  match cs.landed with
  | some r => some r
  | none => cs.acked

/-- **Full compositional & physical flash invariant (`CrashConsistentInv`)**:
Establishes simultaneously:
1. Physical two-region `FlashLayout g cs.flash` + `LayoutSeqWindow g cs.flash L topOpt`
   where `topOpt.map LogEntry.record = latestOnFlash cs` (hence `recover_from_flash_correct`
   applies at EVERY reachable state!),
2. Cached `cs.pos` consistency: whenever `cs.pos = some p`, `cs.landed = none` and
   `p` matches the physical layout (`p = expectedRecoveredPos g cs.flash L topOpt`),
   so any `p.append = some off` guarantees `[off, g.region)` in `p.region` is erased,
3. Protocol phase soundness:
   - In `.erasing efrom eto tr waddr wlen nseq rec`: `EraseValid`, `WriteValid`,
     `tr` is the standby region disjoint from `acked`, `rec` satisfies `ValidRecordBytes`,
     and `nseq` advances `topIdx + 1`,
   - In `.writing tr toff waddr wlen nseq rec`: `WriteValid`, `isErasedRange` at
     `[waddr, waddr + wlen)`, `tr/toff` matches `endOff` of region `tr` (with erased tail),
     `waddr` is disjoint from every record in `acked`, `rec` satisfies `ValidRecordBytes`,
     and `nseq` advances `topIdx + 1`,
   - In `.writeLanded tr toff wlen nseq rec`: `cs.landed = some rec`, `RecordIntact`,
     `rec.region = tr`, `rec.off = toff`, `rec.recordLen = wlen`, `rec.seq = nseq`,
     and whenever `cs.acked = some a`, `RecordIntact g cs.flash a` and `seq_newer rec.seq a.seq = ok true`. -/
def CrashConsistentInv (g : FlashGeometry) (cs : ComposedState) : Prop :=
  ∃ (L : FlashLayout g cs.flash) (topOpt : Option LogEntry),
    LayoutSeqWindow g cs.flash L topOpt ∧
    topOpt.map LogEntry.record = latestOnFlash cs ∧
    (∀ a, cs.acked = some a → RecordIntact g cs.flash a) ∧
    (∀ p, cs.pos = some p →
      cs.phase = .idle ∧
      cs.landed = none ∧
      p = expectedRecoveredPos g cs.flash L topOpt) ∧
    (∀ efrom eto tr waddr wlen nseq rec,
      cs.phase = .erasing efrom eto tr waddr wlen nseq rec →
        cs.pos = none ∧ cs.landed = none ∧
        tr.val ≤ 1 ∧
        efrom.val = regionStartNat g tr.val ∧
        eto.val = efrom.val + g.region.val ∧
        waddr = efrom ∧
        NorFlash.EraseValid (toFlashConfig g) efrom.val eto.val ∧
        NorFlash.WriteValid (toFlashConfig g) waddr.val wlen.val ∧
        ValidRecordBytes g rec ∧
        rec.region = tr ∧ rec.off = 0#u32 ∧ rec.recordLen = wlen ∧ rec.seq = nseq ∧
        (match topOpt with
         | none => nseq.val = 1 % 4294967296
         | some top =>
           tr ≠ top.record.region ∧
           nseq.val = (top.idx + 1) % 4294967296 ∧
           seq_newer nseq top.record.seq = ok true)) ∧
    (∀ tr toff waddr wlen nseq rec,
      cs.phase = .writing tr toff waddr wlen nseq rec →
        cs.pos = none ∧ cs.landed = none ∧
        tr.val ≤ 1 ∧
        waddr.val = regionStartNat g tr.val + toff.val ∧
        NorFlash.WriteValid (toFlashConfig g) waddr.val wlen.val ∧
        NorFlash.isErasedRange cs.flash waddr.val wlen.val ∧
        ValidRecordBytes g rec ∧
        rec.region = tr ∧ rec.off = toff ∧ rec.recordLen = wlen ∧ rec.seq = nseq ∧
        (if tr.val = 0 then L.end0 = toff.val ∧ L.tail0 = .erased
         else L.end1 = toff.val ∧ L.tail1 = .erased) ∧
        (match topOpt with
         | none => nseq.val = 1 % 4294967296 ∧ toff.val = 0
         | some top =>
           nseq.val = (top.idx + 1) % 4294967296 ∧
           seq_newer nseq top.record.seq = ok true ∧
           (tr = top.record.region ∨ (toff.val = 0 ∧ (if tr.val = 0 then L.log0 = [] else L.log1 = []))))) ∧
    (∀ tr toff wlen nseq rec,
      cs.phase = .writeLanded tr toff wlen nseq rec →
        cs.pos = none ∧
        cs.landed = some rec ∧
        RecordIntact g cs.flash rec ∧
        rec.region = tr ∧ rec.off = toff ∧ rec.recordLen = wlen ∧ rec.seq = nseq ∧
        (if tr.val = 0 then L.tail0 = .erased else L.tail1 = .erased) ∧
        (∀ a, cs.acked = some a → seq_newer rec.seq a.seq = ok true))

/-- Helper: `isErasedRange` on `[start + off, start + g.region)` implies `isErasedRange` on `[start + off, start + off + wlen)`. -/
private theorem isErasedRange_sub {s : NorFlash.FlashState} {a l1 l2 : Nat}
    (h : NorFlash.isErasedRange s a l1) (hle : l2 ≤ l1) :
    NorFlash.isErasedRange s a l2 :=
  fun i h1 h2 => h i h1 (by omega)

/-- Helper: disjointness of distinct regions `0` and `1`. -/
private theorem regions_disjoint (g : FlashGeometry) {r1 r2 : Nat}
    (h1 : r1 ≤ 1) (h2 : r2 ≤ 1) (hne : r1 ≠ r2) :
    NorFlash.disjoint (regionStartNat g r1) g.region.val (regionStartNat g r2) g.region.val := by
  unfold regionStartNat
  by_cases hr1 : r1 = 0
  · have hr2 : r2 ≠ 0 := by omega
    simp only [hr1, hr2, ↓reduceIte]; exact Or.inl (Nat.le_refl _)
  · have hr2 : r2 = 0 := by omega
    simp only [hr1, hr2, ↓reduceIte]; exact Or.inr (Nat.le_refl _)

/-- Helper: `List.getLast?` of `xs ++ [x]` is `some x`. -/
private theorem getLast?_append_singleton {α : Type} (xs : List α) (x : α) :
    (xs ++ [x]).getLast? = some x := by
  induction xs with
  | nil => rfl
  | cons y ys ih =>
    cases ys with
    | nil => rfl
    | cons z zs => simpa using ih

/-- Helper: `wrapping_add s 1` matches `(idx + 1) % 2^32`. -/
private theorem wrapping_add_one_mod {s : U32} {idx : Nat}
    (hs : s.val = idx % 4294967296) :
    (core.num.U32.wrapping_add s 1#u32).val = (idx + 1) % 4294967296 := by
  have hns := H5iAppLib.post_of_ok (next_seq_spec s) (by unfold next_seq; rfl)
  rw [← hns.1, hns.2, hs]
  rw [Nat.add_mod, Nat.mod_mod, ← Nat.add_mod]

/-- **Main Compositional Invariant Theorem (`extracted_flashstore_crash_consistent`)**:
`CrashConsistentInv g cs` holds in EVERY reachable state `cs` across arbitrary saves,
erases, partial erases, torn writes, ambiguous write completions/cancellations,
extracted recoveries, and `u32::MAX → 0` sequence-number wraparound. -/
theorem extracted_flashstore_crash_consistent (g : FlashGeometry)
    (hlay : layout_ok g = ok true) (hdiv : AlignDivides g)
    {cs : ComposedState} (hr : ComposedReachable g cs) :
    CrashConsistentInv g cs := by
  have hlp := H5iAppLib.post_of_ok (layout_ok_spec g) hlay rfl
  induction hr with
  | init =>
    let L0 : FlashLayout g composedInit.flash := {
      log0 := [], end0 := 0, tail0 := .erased,
      log1 := [], end1 := 0, tail1 := .erased,
      hlog0 := .nil 0 (by omega) (by simp),
      htail0 := fun _ _ _ => rfl,
      hlog1 := .nil 0 (by omega) (by simp),
      htail1 := fun _ _ _ => rfl
    }
    refine ⟨L0, none, ⟨rfl, rfl⟩, rfl, (fun _ h => by cases h), (fun _ h => by cases h),
      (fun _ _ _ _ _ _ _ h => by cases h), (fun _ _ _ _ _ _ h => by cases h),
      (fun _ _ _ _ _ h => by cases h)⟩
  | @step cs0 _ _ hstep ih =>
    obtain ⟨L, topOpt, hwin, htop_eq, hacked_intact, hpos_inv, herasing_inv, hwriting_inv, hlanded_inv⟩ := ih
    cases hstep with
    | @beginSaveAppend p buf buf' ver plen a tr toff waddr wlen nseq hidle hpos ha hplan hbuf henc =>
      obtain ⟨_, hland_none, hp_eq⟩ := hpos_inv p hpos
      have hreg_p : p.region.val ≤ 1 := by
        rw [hp_eq]; cases topOpt with
        | none => simp [expectedRecoveredPos]
        | some top => simp [expectedRecoveredPos]; exact hwin.choose_spec.1
      have hps := H5iAppLib.post_of_ok (plan_save_safe g p plen hlay hdiv hreg_p) hplan
      obtain ⟨htr_eq, happ_eq, hwaddr_eq, hwrite_v, hwlen_buf, hfit_reg, _, hnseq_eq, h16, htoff_al, hwlen_al, hrl_fn⟩ := hps
      have hrl_ok := hrl_fn a ha
      -- Notice `p.append = some toff` is impossible when `topOpt = none`!
      cases topOpt with
      | none =>
        rw [hp_eq] at happ_eq; simp [expectedRecoveredPos] at happ_eq
      | some top =>
        have hp_reg : p.region = top.record.region := by rw [hp_eq]; rfl
        have hp_seq : p.seq = top.record.seq := by rw [hp_eq]; rfl
        have htr_top : tr = top.record.region := by rw [htr_eq, hp_reg]
        have htr_le : tr.val ≤ 1 := by rw [htr_eq]; exact hreg_p
        have hrec_valid := validRecordBytes_of_encode_record g buf buf' tr toff nseq ver plen a wlen
          hlay ha hrl_ok hbuf hwlen_buf htr_le htoff_al hfit_reg henc
        -- From `happ_eq`, active tail MUST be `.erased` and `toff.val = endOff`!
        have htail_and_end : (if tr.val = 0 then L.end0 = toff.val ∧ L.tail0 = .erased
            else L.end1 = toff.val ∧ L.tail1 = .erased) := by
          rw [hp_eq] at happ_eq; simp only [expectedRecoveredPos] at happ_eq
          by_cases hr0 : tr.val = 0
          · have ht0 : top.record.region.val = 0 := by rw [← htr_top]; exact hr0
            simp only [hr0, ht0, ↓reduceIte] at happ_eq ⊢
            cases htl : L.tail0 with
            | torn => simp [htl] at happ_eq
            | erased =>
              simp only [htl, reduceCtorEq, ↓reduceIte, Option.some.injEq] at happ_eq
              have hb0 := contiguousLog_bounds L.hlog0
              have hlt : L.end0 < U32.size := by scalar_tac
              have heq_v : toff.val = L.end0 := by
                rw [← happ_eq, u32OfNat_val hlt]
              exact ⟨heq_v.symm, rfl⟩
          · have ht0 : top.record.region.val ≠ 0 := by rw [← htr_top]; exact hr0
            simp only [hr0, ht0, ↓reduceIte] at happ_eq ⊢
            cases htl : L.tail1 with
            | torn => simp [htl] at happ_eq
            | erased =>
              simp only [htl, reduceCtorEq, ↓reduceIte, Option.some.injEq] at happ_eq
              have hb1 := contiguousLog_bounds L.hlog1
              have hlt : L.end1 < U32.size := by scalar_tac
              have heq_v : toff.val = L.end1 := by
                rw [← happ_eq, u32OfNat_val hlt]
              exact ⟨heq_v.symm, rfl⟩
        have herased_slot : NorFlash.isErasedRange cs0.flash waddr.val wlen.val := by
          rw [hwaddr_eq]
          by_cases hr0 : tr.val = 0
          · simp only [hr0, ↓reduceIte] at htail_and_end
            have ht0 : NorFlash.isErasedRange cs0.flash (regionStartNat g 0 + L.end0) (g.region.val - L.end0) := by
              have := L.htail0; rw [htail_and_end.2] at this; exact this
            rw [hr0, ← htail_and_end.1]
            exact isErasedRange_sub ht0 (by omega)
          · have hr1 : tr.val = 1 := by omega
            simp only [hr0, ↓reduceIte] at htail_and_end
            have ht1 : NorFlash.isErasedRange cs0.flash (regionStartNat g 1 + L.end1) (g.region.val - L.end1) := by
              have := L.htail1; rw [htail_and_end.2] at this; exact this
            rw [hr1, ← htail_and_end.1]
            exact isErasedRange_sub ht1 (by omega)
        have htop_seq := hwin.choose_spec.2.1
        have hnseq_mod : nseq.val = (top.idx + 1) % 4294967296 := by
          rw [hnseq_eq, hp_seq]; exact wrapping_add_one_mod htop_seq
        have hnseq_newer : seq_newer nseq top.record.seq = ok true := by
          obtain ⟨ns', hns', hnew⟩ := seq_newer_next_seq top.record.seq
          have hns_eq : ns' = nseq := by
            have hp_ns := H5iAppLib.post_of_ok (next_seq_spec top.record.seq) hns'
            rw [hnseq_eq, hp_seq, hp_ns.1]
          rwa [hns_eq] at hnew
        refine ⟨L, some top, hwin, htop_eq, hacked_intact, (fun _ h => by cases h),
          (fun _ _ _ _ _ _ _ h => by cases h), ?_, (fun _ _ _ _ _ h => by cases h)⟩
        intro tr' toff' waddr' wlen' nseq' rec' hph'
        cases hph'
        exact ⟨rfl, hland_none, htr_le, hwaddr_eq, hwrite_v, herased_slot, hrec_valid,
          rfl, rfl, rfl, rfl, htail_and_end, hnseq_mod, hnseq_newer, Or.inl htr_top⟩
    | @beginSaveSwitch p buf buf' ver plen a efrom eto tr waddr wlen nseq hidle hpos ha hplan hbuf henc =>
      obtain ⟨_, hland_none, hp_eq⟩ := hpos_inv p hpos
      have hreg_p : p.region.val ≤ 1 := by
        rw [hp_eq]; cases topOpt with
        | none => simp [expectedRecoveredPos]
        | some top => simp [expectedRecoveredPos]; exact hwin.choose_spec.1
      have hps := H5iAppLib.post_of_ok (plan_save_safe g p plen hlay hdiv hreg_p) hplan
      obtain ⟨htr_le, htr_ne, hefrom_eq, heto_eq, herase_v, _, hwaddr_eq, hwrite_v, _, hwlen_buf, hwlen_reg, hnseq_eq, h16, hwlen_al, hrl_fn⟩ := hps
      have hrl_ok := hrl_fn a ha
      have hrec_valid := validRecordBytes_of_encode_record g buf buf' tr 0#u32 nseq ver plen a wlen
        hlay ha hrl_ok hbuf hwlen_buf htr_le (by simp) (by simp; exact hwlen_reg) henc
      refine ⟨L, topOpt, hwin, htop_eq, hacked_intact, (fun _ h => by cases h), ?_,
        (fun _ _ _ _ _ _ h => by cases h), (fun _ _ _ _ _ h => by cases h)⟩
      intro efrom' eto' tr' waddr' wlen' nseq' rec' hph'
      cases hph'
      refine ⟨rfl, hland_none, htr_le, hefrom_eq, heto_eq, hwaddr_eq, herase_v, hwrite_v,
        hrec_valid, rfl, rfl, rfl, rfl, ?_⟩
      cases topOpt with
      | none =>
        have hp_seq : p.seq = 0#u32 := by rw [hp_eq]; rfl
        rw [hnseq_eq, hp_seq]
        have := wrapping_add_one_mod (s := 0#u32) (idx := 0) (by simp)
        exact this
      | some top =>
        have hp_reg : p.region = top.record.region := by rw [hp_eq]; rfl
        have hp_seq : p.seq = top.record.seq := by rw [hp_eq]; rfl
        have htop_seq := hwin.choose_spec.2.1
        have hnseq_mod : nseq.val = (top.idx + 1) % 4294967296 := by
          rw [hnseq_eq, hp_seq]; exact wrapping_add_one_mod htop_seq
        have hnseq_newer : seq_newer nseq top.record.seq = ok true := by
          obtain ⟨ns', hns', hnew⟩ := seq_newer_next_seq top.record.seq
          have hns_eq : ns' = nseq := by
            have hp_ns := H5iAppLib.post_of_ok (next_seq_spec top.record.seq) hns'
            rw [hnseq_eq, hp_seq, hp_ns.1]
          rwa [hns_eq] at hnew
        exact ⟨by rw [← hp_reg]; exact htr_ne, hnseq_mod, hnseq_newer⟩
    | @eraseOk efrom eto tr waddr wlen nseq rec hph =>
      obtain ⟨hpos_none, hland_none, htr_le, hefrom_eq, heto_eq, hwaddr_eq, herase_v, hwrite_v,
        hrec_valid, hrr, hro, hrl, hrs, htop_cond⟩ := herasing_inv efrom eto tr waddr wlen nseq rec hph
      let flash' := NorFlash.eraseRange cs0.flash efrom.val eto.val
      have herased_tr : NorFlash.isErasedRange flash' (regionStartNat g tr.val) g.region.val := by
        have h1 := NorFlash.eraseRange_erased cs0.flash efrom.val eto.val herase_v.1
        simpa [flash', hefrom_eq, heto_eq] using h1
      have hag_other : ∀ r, r ≤ 1 → r ≠ tr.val →
          NorFlash.agreesOn flash' cs0.flash (regionStartNat g r) g.region.val := by
        intro r hr hne
        have hdisj := regions_disjoint g htr_le hr (Ne.symm hne)
        have hdisj' : NorFlash.disjoint efrom.val (eto.val - efrom.val) (regionStartNat g r) g.region.val := by
          simpa [hefrom_eq, heto_eq] using hdisj
        exact NorFlash.eraseRange_disjoint_preserved hdisj' herase_v.1
      -- Construct updated `FlashLayout` with region `tr` cleanly erased (`[]`, `0`, `.erased`)
      -- and the other region completely preserved!
      have hL'_and_win : ∃ L' : FlashLayout g flash',
          LayoutSeqWindow g flash' L' topOpt ∧
          (if tr.val = 0 then L'.log0 = [] ∧ L'.end0 = 0 ∧ L'.tail0 = .erased
           else L'.log1 = [] ∧ L'.end1 = 0 ∧ L'.tail1 = .erased) := by
        by_cases htr0 : tr.val = 0
        · have hag1 := hag_other 1 (by omega) (by omega)
          have hb1 := contiguousLog_bounds L.hlog1
          let L' : FlashLayout g flash' := {
            log0 := [], end0 := 0, tail0 := .erased,
            log1 := L.log1, end1 := L.end1, tail1 := L.tail1,
            hlog0 := .nil 0 (by omega) (by simp),
            htail0 := by simpa [TailValid, htr0] using herased_tr,
            hlog1 := contiguousLog_agreesOn L.hlog1 (fun i hi1 hi2 => hag1 i (by omega) (by omega)),
            htail1 := tailValid_agreesOn L.htail1 (fun i hi1 hi2 => hag1 i (by omega) (by omega))
          }
          refine ⟨L', ?_, by simp [htr0, L']⟩
          cases topOpt with
          | none => exact ⟨rfl, hwin.2⟩
          | some top =>
            obtain ⟨htr_ne, _, _⟩ := htop_cond
            have htop_ne0 : top.record.region.val ≠ 0 := fun h => htr_ne (UScalar.eq_of_val_eq (by omega))
            obtain ⟨loAct, h1, h2, h3, h4, h5, _⟩ := hwin
            simp only [htop_ne0, ↓reduceIte] at h3 h4 h5
            exact ⟨loAct, h1, h2, by simp [htop_ne0, L', h3], by simp [htop_ne0, L']; exact h4,
              by simp [htop_ne0, L']; exact h5, by simp [htop_ne0, L']⟩
        · have htr1 : tr.val = 1 := by omega
          have hag0 := hag_other 0 (by omega) (by omega)
          have hb0 := contiguousLog_bounds L.hlog0
          let L' : FlashLayout g flash' := {
            log0 := L.log0, end0 := L.end0, tail0 := L.tail0,
            log1 := [], end1 := 0, tail1 := .erased,
            hlog0 := contiguousLog_agreesOn L.hlog0 (fun i hi1 hi2 => hag0 i (by omega) (by omega)),
            htail0 := tailValid_agreesOn L.htail0 (fun i hi1 hi2 => hag0 i (by omega) (by omega)),
            hlog1 := .nil 0 (by omega) (by simp),
            htail1 := by simpa [TailValid, htr1] using herased_tr
          }
          refine ⟨L', ?_, by simp [htr0, L']⟩
          cases topOpt with
          | none => exact ⟨hwin.1, rfl⟩
          | some top =>
            obtain ⟨htr_ne, _, _⟩ := htop_cond
            have htop_eq0 : top.record.region.val = 0 := by
              have htle := hwin.choose_spec.1
              have hne : tr.val ≠ top.record.region.val := fun h => htr_ne (UScalar.eq_of_val_eq h)
              omega
            obtain ⟨loAct, h1, h2, h3, h4, h5, _⟩ := hwin
            simp only [htop_eq0, ↓reduceIte] at h3 h4 h5
            exact ⟨loAct, h1, h2, by simp [htop_eq0, L', h3], by simp [htop_eq0, L']; exact h4,
              by simp [htop_eq0, L']; exact h5, by simp [htop_eq0, L']⟩
      obtain ⟨L', hwin', htr_clean⟩ := hL'_and_win
      have hacked_intact' : ∀ a, cs0.acked = some a → RecordIntact g flash' a := by
        intro a ha
        have hint := hacked_intact a ha
        have ha_eq : topOpt.map LogEntry.record = some a := by
          rw [htop_eq]; simp [latestOnFlash, hland_none, ha]
        cases topOpt with
        | none => simp at ha_eq
        | some top =>
          simp at ha_eq; subst ha_eq
          obtain ⟨htr_ne, _, _⟩ := htop_cond
          have hne_val : top.record.region.val ≠ tr.val := fun h => htr_ne (UScalar.eq_of_val_eq h.symm)
          have hag := hag_other top.record.region.val hint.1 hne_val
          refine ⟨hint.1, hint.2.1, hint.2.2.1, ?_⟩
          rw [NorFlash.readSlice_eq_of_agreesOn (fun i hi1 hi2 => hag i (by omega) (by have := hint.2.1; omega))]
          exact hint.2.2.2
      refine ⟨L', topOpt, hwin', htop_eq, hacked_intact', (fun _ h => by rw [hpos_none] at h; cases h),
        (fun _ _ _ _ _ _ _ h => by cases h), ?_, (fun _ _ _ _ _ h => by cases h)⟩
      intro tr' toff' waddr' wlen' nseq' rec' hph'
      cases hph'
      have hwaddr_val' : waddr.val = regionStartNat g tr.val + (0#u32).val := by
        simp [hwaddr_eq, hefrom_eq]
      have herased_slot : NorFlash.isErasedRange flash' waddr.val wlen.val := by
        rw [hwaddr_eq, hefrom_eq]
        exact isErasedRange_sub herased_tr (by have := hrec_valid.2.2.2.2.2.1; rw [hro, hrl] at this; simp at this; exact this)
      refine ⟨hpos_none, hland_none, htr_le, hwaddr_val', hwrite_v, herased_slot, hrec_valid,
        hrr, hro, hrl, hrs, ?_, ?_⟩
      · by_cases h0 : tr.val = 0 <;> simp [h0] at htr_clean ⊢ <;> exact htr_clean.2
      · cases topOpt with
        | none => exact ⟨htop_cond, by simp⟩
        | some top =>
          refine ⟨htop_cond.2.1, htop_cond.2.2, Or.inr ⟨by simp, ?_⟩⟩
          by_cases h0 : tr.val = 0 <;> simp [h0] at htr_clean ⊢ <;> exact htr_clean.1
    | @eraseFailPowerCut efrom eto tr waddr wlen nseq rec flash' hph hIso hC1 =>
      obtain ⟨_, hland_none, htr_le, hefrom_eq, heto_eq, _, _, _, _, _, _, _, _, htop_cond⟩ :=
        herasing_inv efrom eto tr waddr wlen nseq rec hph
      have hag_other : ∀ r, r ≤ 1 → r ≠ tr.val →
          NorFlash.agreesOn flash' cs0.flash (regionStartNat g r) g.region.val := by
        intro r hr hne
        have hdisj := regions_disjoint g htr_le hr (Ne.symm hne)
        intro i hi1 hi2
        apply hIso i
        rw [hefrom_eq, heto_eq]
        rcases hdisj with hd | hd <;> omega
      have hL'_and_win : ∃ L' : FlashLayout g flash', LayoutSeqWindow g flash' L' topOpt := by
        by_cases htr0 : tr.val = 0
        · obtain ⟨log0', end0', tail0', hlog0', htail0', hsub0⟩ :=
            hC1 L.log0 L.end0 L.tail0 (by rw [htr0]; exact L.hlog0) (by rw [htr0]; exact L.htail0)
          have hag1 := hag_other 1 (by omega) (by omega)
          have hb1 := contiguousLog_bounds L.hlog1
          let L' : FlashLayout g flash' := {
            log0 := log0', end0 := end0', tail0 := tail0',
            log1 := L.log1, end1 := L.end1, tail1 := L.tail1,
            hlog0 := by rwa [htr0] at hlog0',
            htail0 := by rwa [htr0] at htail0',
            hlog1 := contiguousLog_agreesOn L.hlog1 (fun i hi1 hi2 => hag1 i (by omega) (by omega)),
            htail1 := tailValid_agreesOn L.htail1 (fun i hi1 hi2 => hag1 i (by omega) (by omega))
          }
          refine ⟨L', ?_⟩
          cases topOpt with
          | none =>
            have h0nil : log0' = [] := by
              cases log0' with
              | nil => rfl
              | cons x _ => have := hsub0 x (List.mem_cons_self ..); rw [hwin.1] at this; cases this
            exact ⟨h0nil, hwin.2⟩
          | some top =>
            obtain ⟨htr_ne, _, _⟩ := htop_cond
            have htop_ne0 : top.record.region.val ≠ 0 := fun h => htr_ne (UScalar.eq_of_val_eq (by omega))
            obtain ⟨loAct, h1, h2, h3, h4, h5, h6⟩ := hwin
            simp only [htop_ne0, ↓reduceIte] at h3 h4 h5 h6
            dsimp only [LayoutSeqWindow]
            refine ⟨loAct, h1, h2, by simp [htop_ne0, L', h3], by simp [htop_ne0, L']; exact h4,
              by simp [htop_ne0, L']; exact h5, ?_⟩
            simp only [htop_ne0, ↓reduceIte, L']
            intro e he; exact h6 e (hsub0 e he)
        · have htr1 : tr.val = 1 := by omega
          obtain ⟨log1', end1', tail1', hlog1', htail1', hsub1⟩ :=
            hC1 L.log1 L.end1 L.tail1 (by rw [htr1]; exact L.hlog1) (by rw [htr1]; exact L.htail1)
          have hag0 := hag_other 0 (by omega) (by omega)
          have hb0 := contiguousLog_bounds L.hlog0
          let L' : FlashLayout g flash' := {
            log0 := L.log0, end0 := L.end0, tail0 := L.tail0,
            log1 := log1', end1 := end1', tail1 := tail1',
            hlog0 := contiguousLog_agreesOn L.hlog0 (fun i hi1 hi2 => hag0 i (by omega) (by omega)),
            htail0 := tailValid_agreesOn L.htail0 (fun i hi1 hi2 => hag0 i (by omega) (by omega)),
            hlog1 := by rwa [htr1] at hlog1',
            htail1 := by rwa [htr1] at htail1'
          }
          refine ⟨L', ?_⟩
          cases topOpt with
          | none =>
            have h1nil : log1' = [] := by
              cases log1' with
              | nil => rfl
              | cons x _ => have := hsub1 x (List.mem_cons_self ..); rw [hwin.2] at this; cases this
            exact ⟨hwin.1, h1nil⟩
          | some top =>
            obtain ⟨htr_ne, _, _⟩ := htop_cond
            have htop_eq0 : top.record.region.val = 0 := by
              have htle := hwin.choose_spec.1
              have hne : tr.val ≠ top.record.region.val := fun h => htr_ne (UScalar.eq_of_val_eq h)
              omega
            obtain ⟨loAct, h1, h2, h3, h4, h5, h6⟩ := hwin
            simp only [htop_eq0, ↓reduceIte] at h3 h4 h5 h6
            dsimp only [LayoutSeqWindow]
            refine ⟨loAct, h1, h2, by simp [htop_eq0, L', h3], by simp [htop_eq0, L']; exact h4,
              by simp [htop_eq0, L']; exact h5, ?_⟩
            simp only [htop_eq0, ↓reduceIte, L']
            intro e he; exact h6 e (hsub1 e he)
      obtain ⟨L', hwin'⟩ := hL'_and_win
      have hacked_intact' : ∀ a, cs0.acked = some a → RecordIntact g flash' a := by
        intro a ha
        have hint := hacked_intact a ha
        have ha_eq : topOpt.map LogEntry.record = some a := by
          rw [htop_eq]; simp [latestOnFlash, hland_none, ha]
        cases topOpt with
        | none => simp at ha_eq
        | some top =>
          simp at ha_eq; subst ha_eq
          obtain ⟨htr_ne, _, _⟩ := htop_cond
          have hne_val : top.record.region.val ≠ tr.val := fun h => htr_ne (UScalar.eq_of_val_eq h.symm)
          have hag := hag_other top.record.region.val hint.1 hne_val
          refine ⟨hint.1, hint.2.1, hint.2.2.1, ?_⟩
          rw [NorFlash.readSlice_eq_of_agreesOn (fun i hi1 hi2 => hag i (by omega) (by have := hint.2.1; omega))]
          exact hint.2.2.2
      refine ⟨L', topOpt, hwin', htop_eq, hacked_intact', (fun _ h => by cases h),
        (fun _ _ _ _ _ _ _ h => by cases h), (fun _ _ _ _ _ _ h => by cases h), (fun _ _ _ _ _ h => by cases h)⟩
    | @writeOk tr toff waddr wlen nseq rec hph =>
      obtain ⟨hpos_none, hland_none, htr_le, hwaddr_eq, hwrite_v, herased_slot, hrec_valid,
        hrr, hro, hrl, hrs, htail_tr, htop_cond⟩ := hwriting_inv tr toff waddr wlen nseq rec hph
      let flash' := NorFlash.writeBytes cs0.flash waddr.val rec.bytes
      have hblen : rec.bytes.length = wlen.val := by rw [← hrl]; exact hrec_valid.2.2.2.2.2.2.2.2.1
      have hintact_rec : RecordIntact g flash' rec := by
        refine ⟨hrec_valid.1, hrec_valid.2.2.2.2.2.1, hrec_valid.2.2.2.2.2.2.2.2.1, ?_⟩
        rw [hrr, hro, hrl, ← hwaddr_eq]
        have hwr := NorFlash.writeBytes_in_erased (s := cs0.flash) (addr := waddr.val) (bytes := rec.bytes)
          (by rw [hblen]; exact herased_slot)
        simpa [flash', hblen] using hwr
      let newIdx : Nat := match topOpt with | none => 1 | some top => top.idx + 1
      let newEntry : LogEntry := { idx := newIdx, record := rec }
      have hnew_idx_mod : rec.seq.val = newIdx % 4294967296 := by
        rw [hrs]; cases topOpt with
        | none => exact htop_cond.1
        | some top => exact htop_cond.1
      have hdisj_other : ∀ r, r ≤ 1 → r ≠ tr.val →
          NorFlash.agreesOn flash' cs0.flash (regionStartNat g r) g.region.val := by
        intro r hr hne
        have hd := regions_disjoint g htr_le hr (Ne.symm hne)
        have hd' : NorFlash.disjoint waddr.val rec.bytes.length (regionStartNat g r) g.region.val := by
          rw [hblen, hwaddr_eq]
          have hfit := hrec_valid.2.2.2.2.2.1; rw [hro, hrl] at hfit
          rcases hd with h | h
          · exact Or.inl (by omega)
          · exact Or.inr (by omega)
        exact NorFlash.writeBytes_disjoint_preserved hd'
      have hag_prefix_tr : NorFlash.agreesOn flash' cs0.flash (regionStartNat g tr.val) toff.val :=
        NorFlash.writeBytes_disjoint_preserved (by rw [hblen, hwaddr_eq]; exact Or.inr (Nat.le_refl _))
      have herased_after_tr : NorFlash.isErasedRange flash'
          (regionStartNat g tr.val + (toff.val + wlen.val)) (g.region.val - (toff.val + wlen.val)) := by
        intro i hi1 hi2
        have hag : flash'.mem i = cs0.flash.mem i :=
          NorFlash.writeBytes_disjoint_preserved
            (waddr := waddr.val) (a := regionStartNat g tr.val + (toff.val + wlen.val))
            (l := g.region.val - (toff.val + wlen.val))
            (by rw [hblen, hwaddr_eq]; exact Or.inl (by omega)) i hi1 hi2
        rw [hag]
        by_cases h0 : tr.val = 0
        · simp only [h0, ↓reduceIte] at htail_tr
          have ht := L.htail0; rw [htail_tr.2, htail_tr.1] at ht
          rw [h0] at hi1 hi2; exact ht i (by omega) (by omega)
        · have h1 : tr.val = 1 := by omega
          simp only [h0, ↓reduceIte] at htail_tr
          have ht := L.htail1; rw [htail_tr.2, htail_tr.1] at ht
          rw [h1] at hi1 hi2; exact ht i (by omega) (by omega)
      -- Build updated `FlashLayout` + `LayoutSeqWindow` with `newEntry` appended to region `tr`!
      have hL'_and_win : ∃ L' : FlashLayout g flash',
          LayoutSeqWindow g flash' L' (some newEntry) ∧
          (if tr.val = 0 then L'.tail0 = .erased else L'.tail1 = .erased) := by
        by_cases htr0 : tr.val = 0
        · simp only [htr0, ↓reduceIte] at htail_tr
          have hlog0_pres := contiguousLog_agreesOn (s' := flash') L.hlog0
            (fun i hi1 hi2 => by rw [htr0] at hag_prefix_tr; exact hag_prefix_tr i (by omega) (by omega))
          have hlog0' := contiguousLog_append_one hlog0_pres (e := newEntry)
            (by rw [hrr, htr0]) (by rw [hro, htail_tr.1]) hnew_idx_mod hrec_valid hintact_rec
          have hag1 := hdisj_other 1 (by omega) (by omega)
          have hb1 := contiguousLog_bounds L.hlog1
          let L' : FlashLayout g flash' := {
            log0 := L.log0 ++ [newEntry], end0 := L.end0 + wlen.val, tail0 := .erased,
            log1 := L.log1, end1 := L.end1, tail1 := L.tail1,
            hlog0 := by simpa [newEntry, hrl] using hlog0',
            htail0 := by simpa [TailValid, htr0, htail_tr.1] using herased_after_tr,
            hlog1 := contiguousLog_agreesOn L.hlog1 (fun i hi1 hi2 => hag1 i (by omega) (by omega)),
            htail1 := tailValid_agreesOn L.htail1 (fun i hi1 hi2 => hag1 i (by omega) (by omega))
          }
          refine ⟨L', ?_, by simp [htr0, L']⟩
          have hnr0 : newEntry.record.region.val = 0 := by simp [newEntry, hrr, htr0]
          cases topOpt with
          | none =>
            dsimp only [LayoutSeqWindow]
            refine ⟨1, by omega, hnew_idx_mod, by simp [hnr0, L', hwin.1], ?_, by simp [hnr0, L', hwin.1, newEntry, newIdx],
              by simp [hnr0, L', hwin.2]⟩
            intro e he; simp [hnr0, L', hwin.1] at he; subst he; exact ⟨Nat.le_refl _, Nat.le_refl _, Or.inl rfl⟩
          | some top =>
            obtain ⟨loAct, htop_r, htop_s, htop_last, htop_act, htop_span, htop_std⟩ := hwin
            rcases htop_cond.2.2 with hsame | ⟨htoff0, hlog0_nil⟩
            · -- Appended in same active region 0!
              have htop0 : top.record.region.val = 0 := by rw [← hsame]; exact htr0
              simp only [htop0, ↓reduceIte] at htop_last htop_act htop_span htop_std
              dsimp only [LayoutSeqWindow]
              refine ⟨loAct, by omega, hnew_idx_mod, by simp [hnr0, L', getLast?_append_singleton], ?_, ?_,
                by simp only [hnr0, ↓reduceIte, L']; exact htop_std⟩
              · simp only [hnr0, ↓reduceIte, L']
                intro e he
                rcases List.mem_append.mp he with hprev | hnew
                · obtain ⟨h1, h2, _⟩ := htop_act e hprev
                  exact ⟨h1, by simp [newEntry, newIdx]; omega, Or.inr (by simp [newEntry, newIdx]; omega)⟩
                · simp at hnew; subst hnew
                  have hlo_le : loAct ≤ top.idx := by
                    have hmem_t : top ∈ L.log0 := by
                      cases h : L.log0 with
                      | nil => rw [h] at htop_last; simp at htop_last
                      | cons a xs =>
                        rw [h, List.getLast?_eq_some_getLast (List.cons_ne_nil a xs), Option.some.injEq] at htop_last
                        subst htop_last; exact List.getLast_mem _
                    exact (htop_act top hmem_t).1
                  exact ⟨by simp [newEntry, newIdx]; omega, Nat.le_refl _, Or.inl rfl⟩
              · simp only [hnr0, ↓reduceIte, L', List.length_append, List.length_singleton, newEntry, newIdx]
                omega
            · -- Switched into freshly erased region 0!
              simp only [htr0, ↓reduceIte] at hlog0_nil
              have htop_ne0 : top.record.region.val ≠ 0 := by
                intro h0; simp only [h0, ↓reduceIte] at htop_last; rw [hlog0_nil] at htop_last; simp at htop_last
              simp only [htop_ne0, ↓reduceIte] at htop_last htop_act htop_span htop_std
              have hb1' := contiguousLog_bounds L.hlog1
              refine ⟨top.idx + 1, by omega, hnew_idx_mod, by simp [hnr0, L', hlog0_nil], ?_,
                by simp [hnr0, L', hlog0_nil, newEntry, newIdx], ?_⟩
              · intro e he; simp [hnr0, L', hlog0_nil] at he; subst he
                exact ⟨Nat.le_refl _, Nat.le_refl _, Or.inl rfl⟩
              · simp only [hnr0, ↓reduceIte, L']
                intro e he
                obtain ⟨hlo, hhi, _⟩ := htop_act e he
                exact ⟨by omega, by omega⟩
        · have htr1 : tr.val = 1 := by omega
          simp only [htr0, ↓reduceIte] at htail_tr
          have hlog1_pres := contiguousLog_agreesOn (s' := flash') L.hlog1
            (fun i hi1 hi2 => by rw [htr1] at hag_prefix_tr; exact hag_prefix_tr i (by omega) (by omega))
          have hlog1' := contiguousLog_append_one hlog1_pres (e := newEntry)
            (by rw [hrr, htr1]) (by rw [hro, htail_tr.1]) hnew_idx_mod hrec_valid hintact_rec
          have hag0 := hdisj_other 0 (by omega) (by omega)
          have hb0 := contiguousLog_bounds L.hlog0
          let L' : FlashLayout g flash' := {
            log0 := L.log0, end0 := L.end0, tail0 := L.tail0,
            log1 := L.log1 ++ [newEntry], end1 := L.end1 + wlen.val, tail1 := .erased,
            hlog0 := contiguousLog_agreesOn L.hlog0 (fun i hi1 hi2 => hag0 i (by omega) (by omega)),
            htail0 := tailValid_agreesOn L.htail0 (fun i hi1 hi2 => hag0 i (by omega) (by omega)),
            hlog1 := by simpa [newEntry, hrl] using hlog1',
            htail1 := by simpa [TailValid, htr1, htail_tr.1] using herased_after_tr
          }
          refine ⟨L', ?_, by simp [htr0, L']⟩
          have hnr1 : newEntry.record.region.val = 1 := by simp [newEntry, hrr, htr1]
          have hnr0 : newEntry.record.region.val ≠ 0 := by omega
          cases topOpt with
          | none =>
            dsimp only [LayoutSeqWindow]
            refine ⟨1, by omega, hnew_idx_mod, by simp [hnr0, L', hwin.2], ?_, by simp [hnr0, L', hwin.2, newEntry, newIdx],
              by simp [hnr0, L', hwin.1]⟩
            intro e he; simp [hnr0, L', hwin.2] at he; subst he; exact ⟨Nat.le_refl _, Nat.le_refl _, Or.inl rfl⟩
          | some top =>
            obtain ⟨loAct, htop_r, htop_s, htop_last, htop_act, htop_span, htop_std⟩ := hwin
            rcases htop_cond.2.2 with hsame | ⟨htoff0, hlog1_nil⟩
            · have htop1 : top.record.region.val ≠ 0 := by rw [← hsame]; exact htr0
              simp only [htop1, ↓reduceIte] at htop_last htop_act htop_span htop_std
              dsimp only [LayoutSeqWindow]
              refine ⟨loAct, by omega, hnew_idx_mod, by simp [hnr0, L', getLast?_append_singleton], ?_, ?_,
                by simp only [hnr0, ↓reduceIte, L']; exact htop_std⟩
              · simp only [hnr0, ↓reduceIte, L']
                intro e he
                rcases List.mem_append.mp he with hprev | hnew
                · obtain ⟨h1, h2, _⟩ := htop_act e hprev
                  exact ⟨h1, by simp [newEntry, newIdx]; omega, Or.inr (by simp [newEntry, newIdx]; omega)⟩
                · simp at hnew; subst hnew
                  have hlo_le : loAct ≤ top.idx := by
                    have hmem_t : top ∈ L.log1 := by
                      cases h : L.log1 with
                      | nil => rw [h] at htop_last; simp at htop_last
                      | cons a xs =>
                        rw [h, List.getLast?_eq_some_getLast (List.cons_ne_nil a xs), Option.some.injEq] at htop_last
                        subst htop_last; exact List.getLast_mem _
                    exact (htop_act top hmem_t).1
                  exact ⟨by simp [newEntry, newIdx]; omega, Nat.le_refl _, Or.inl rfl⟩
              · simp only [hnr0, ↓reduceIte, L', List.length_append, List.length_singleton, newEntry, newIdx]
                omega
            · simp only [htr0, ↓reduceIte] at hlog1_nil
              have htop0 : top.record.region.val = 0 := by
                by_contra hne0; simp only [hne0, ↓reduceIte] at htop_last; rw [hlog1_nil] at htop_last; simp at htop_last
              simp only [htop0, ↓reduceIte] at htop_last htop_act htop_span htop_std
              have hb0' := contiguousLog_bounds L.hlog0
              refine ⟨top.idx + 1, by omega, hnew_idx_mod, by simp [hnr0, L', hlog1_nil], ?_,
                by simp [hnr0, L', hlog1_nil, newEntry, newIdx], ?_⟩
              · intro e he; simp [hnr0, L', hlog1_nil] at he; subst he
                exact ⟨Nat.le_refl _, Nat.le_refl _, Or.inl rfl⟩
              · simp only [hnr0, ↓reduceIte, L']
                intro e he
                obtain ⟨hlo, hhi, _⟩ := htop_act e he
                exact ⟨by omega, by omega⟩
      obtain ⟨L', hwin', htail'_er⟩ := hL'_and_win
      have hacked_intact' : ∀ a, cs0.acked = some a → RecordIntact g flash' a := by
        intro a ha
        have hint := hacked_intact a ha
        have ha_eq : topOpt.map LogEntry.record = some a := by
          rw [htop_eq]; simp [latestOnFlash, hland_none, ha]
        cases topOpt with
        | none => simp at ha_eq
        | some top =>
          simp at ha_eq; subst ha_eq
          refine ⟨hint.1, hint.2.1, hint.2.2.1, ?_⟩
          by_cases hsame : top.record.region.val = tr.val
          · have hend_top : top.record.off.val + top.record.recordLen.val = toff.val := by
              obtain ⟨_, _, _, hlast, _, _, _⟩ := hwin
              by_cases h0 : tr.val = 0
              · have ht0 : top.record.region.val = 0 := by omega
                simp only [h0, ↓reduceIte] at htail_tr
                simp only [ht0, ↓reduceIte] at hlast
                rw [contiguousLog_last_end L.hlog0 hlast, htail_tr.1]
              · have ht0 : top.record.region.val ≠ 0 := by omega
                simp only [h0, ↓reduceIte] at htail_tr
                simp only [ht0, ↓reduceIte] at hlast
                rw [contiguousLog_last_end L.hlog1 hlast, htail_tr.1]
            have hag : NorFlash.agreesOn flash' cs0.flash
                (regionStartNat g top.record.region.val + top.record.off.val) top.record.recordLen.val := by
              intro i hi1 hi2
              rw [hsame] at hi1 hi2
              exact hag_prefix_tr i (by omega) (by omega)
            rw [NorFlash.readSlice_eq_of_agreesOn hag]; exact hint.2.2.2
          · have hag := hdisj_other top.record.region.val hint.1 hsame
            rw [NorFlash.readSlice_eq_of_agreesOn (fun i hi1 hi2 => hag i (by omega) (by have := hint.2.1; omega))]
            exact hint.2.2.2
      refine ⟨L', some newEntry, hwin', rfl, hacked_intact', (fun _ h => by rw [hpos_none] at h; cases h),
        (fun _ _ _ _ _ _ _ h => by cases h), (fun _ _ _ _ _ _ h => by cases h), ?_⟩
      intro tr' toff' wlen' nseq' rec' hph'
      cases hph'
      refine ⟨hpos_none, rfl, hintact_rec, hrr, hro, hrl, hrs, htail'_er, ?_⟩
      intro a ha
      dsimp only at ha
      have ha_eq : topOpt.map LogEntry.record = some a := by
        rw [htop_eq]; simp [latestOnFlash, hland_none, ha]
      cases topOpt with
      | none => simp at ha_eq
      | some top =>
        simp at ha_eq; subst ha_eq
        rw [hrs]; exact htop_cond.2.1
    | @writeFailTorn tr toff waddr wlen nseq rec flash' hph hIso _ hC1 =>
      obtain ⟨_, hland_none, htr_le, hwaddr_eq, _, _, hrec_valid, hrr, hro, hrl, _, htail_tr, _⟩ :=
        hwriting_inv tr toff waddr wlen nseq rec hph
      have hdisj_other : ∀ r, r ≤ 1 → r ≠ tr.val →
          NorFlash.agreesOn flash' cs0.flash (regionStartNat g r) g.region.val := by
        intro r hr hne
        have hd := regions_disjoint g htr_le hr (Ne.symm hne)
        have hfit := hrec_valid.2.2.2.2.2.1; rw [hro, hrl] at hfit
        intro i hi1 hi2
        apply hIso i; rw [hwaddr_eq]
        rcases hd with h | h <;> omega
      have hag_prefix_tr : NorFlash.agreesOn flash' cs0.flash (regionStartNat g tr.val) toff.val := by
        intro i hi1 hi2; apply hIso i; rw [hwaddr_eq]; exact Or.inl (by omega)
      -- Choose `newTail` from `hC1` (`.erased` or `.torn`).
      have htail_ex : ∃ newTail : RegionTail, TailValid g flash' tr.val toff.val newTail := by
        rcases hC1 with h | h
        · exact ⟨.erased, h⟩
        · exact ⟨.torn, h⟩
      obtain ⟨newTail, hnewTail⟩ := htail_ex
      have hL'_and_win : ∃ L' : FlashLayout g flash', LayoutSeqWindow g flash' L' topOpt := by
        by_cases htr0 : tr.val = 0
        · simp only [htr0, ↓reduceIte] at htail_tr
          have hag1 := hdisj_other 1 (by omega) (by omega)
          have hb1 := contiguousLog_bounds L.hlog1
          let L' : FlashLayout g flash' := {
            log0 := L.log0, end0 := L.end0, tail0 := newTail,
            log1 := L.log1, end1 := L.end1, tail1 := L.tail1,
            hlog0 := contiguousLog_agreesOn L.hlog0
              (fun i hi1 hi2 => by rw [htr0] at hag_prefix_tr; exact hag_prefix_tr i (by omega) (by omega)),
            htail0 := by simpa [htr0, htail_tr.1] using hnewTail,
            hlog1 := contiguousLog_agreesOn L.hlog1 (fun i hi1 hi2 => hag1 i (by omega) (by omega)),
            htail1 := tailValid_agreesOn L.htail1 (fun i hi1 hi2 => hag1 i (by omega) (by omega))
          }
          exact ⟨L', hwin⟩
        · have htr1 : tr.val = 1 := by omega
          simp only [htr0, ↓reduceIte] at htail_tr
          have hag0 := hdisj_other 0 (by omega) (by omega)
          have hb0 := contiguousLog_bounds L.hlog0
          let L' : FlashLayout g flash' := {
            log0 := L.log0, end0 := L.end0, tail0 := L.tail0,
            log1 := L.log1, end1 := L.end1, tail1 := newTail,
            hlog0 := contiguousLog_agreesOn L.hlog0 (fun i hi1 hi2 => hag0 i (by omega) (by omega)),
            htail0 := tailValid_agreesOn L.htail0 (fun i hi1 hi2 => hag0 i (by omega) (by omega)),
            hlog1 := contiguousLog_agreesOn L.hlog1
              (fun i hi1 hi2 => by rw [htr1] at hag_prefix_tr; exact hag_prefix_tr i (by omega) (by omega)),
            htail1 := by simpa [htr1, htail_tr.1] using hnewTail
          }
          exact ⟨L', hwin⟩
      obtain ⟨L', hwin'⟩ := hL'_and_win
      have hacked_intact' : ∀ a, cs0.acked = some a → RecordIntact g flash' a := by
        intro a ha
        have hint := hacked_intact a ha
        have ha_eq : topOpt.map LogEntry.record = some a := by
          rw [htop_eq]; simp [latestOnFlash, hland_none, ha]
        cases topOpt with
        | none => simp at ha_eq
        | some top =>
          simp at ha_eq; subst ha_eq
          refine ⟨hint.1, hint.2.1, hint.2.2.1, ?_⟩
          by_cases hsame : top.record.region.val = tr.val
          · have hend_top : top.record.off.val + top.record.recordLen.val = toff.val := by
              obtain ⟨_, _, _, hlast, _, _, _⟩ := hwin
              by_cases h0 : tr.val = 0
              · have ht0 : top.record.region.val = 0 := by omega
                simp only [h0, ↓reduceIte] at htail_tr; simp only [ht0, ↓reduceIte] at hlast
                rw [contiguousLog_last_end L.hlog0 hlast, htail_tr.1]
              · have ht0 : top.record.region.val ≠ 0 := by omega
                simp only [h0, ↓reduceIte] at htail_tr; simp only [ht0, ↓reduceIte] at hlast
                rw [contiguousLog_last_end L.hlog1 hlast, htail_tr.1]
            have hag : NorFlash.agreesOn flash' cs0.flash
                (regionStartNat g top.record.region.val + top.record.off.val) top.record.recordLen.val := by
              intro i hi1 hi2; rw [hsame] at hi1 hi2; exact hag_prefix_tr i (by omega) (by omega)
            rw [NorFlash.readSlice_eq_of_agreesOn hag]; exact hint.2.2.2
          · have hag := hdisj_other top.record.region.val hint.1 hsame
            rw [NorFlash.readSlice_eq_of_agreesOn (fun i hi1 hi2 => hag i (by omega) (by have := hint.2.1; omega))]
            exact hint.2.2.2
      refine ⟨L', topOpt, hwin', htop_eq, hacked_intact', (fun _ h => by cases h),
        (fun _ _ _ _ _ _ _ h => by cases h), (fun _ _ _ _ _ _ h => by cases h), (fun _ _ _ _ _ h => by cases h)⟩
    | @writeFailAmbiguousLanded tr toff waddr wlen nseq rec hph =>
      -- Physically identical flash update to `writeOk`, except `phase := .idle`, `pos := none`, `landed := some rec`!
      obtain ⟨_, hland_none, htr_le, hwaddr_eq, _, herased_slot, hrec_valid,
        hrr, hro, hrl, hrs, htail_tr, htop_cond⟩ := hwriting_inv tr toff waddr wlen nseq rec hph
      let flash' := NorFlash.writeBytes cs0.flash waddr.val rec.bytes
      have hblen : rec.bytes.length = wlen.val := by rw [← hrl]; exact hrec_valid.2.2.2.2.2.2.2.2.1
      have hintact_rec : RecordIntact g flash' rec := by
        refine ⟨hrec_valid.1, hrec_valid.2.2.2.2.2.1, hrec_valid.2.2.2.2.2.2.2.2.1, ?_⟩
        rw [hrr, hro, hrl, ← hwaddr_eq]
        have hwr := NorFlash.writeBytes_in_erased (s := cs0.flash) (addr := waddr.val) (bytes := rec.bytes)
          (by rw [hblen]; exact herased_slot)
        simpa [flash', hblen] using hwr
      let newIdx : Nat := match topOpt with | none => 1 | some top => top.idx + 1
      let newEntry : LogEntry := { idx := newIdx, record := rec }
      have hnew_idx_mod : rec.seq.val = newIdx % 4294967296 := by
        rw [hrs]; cases topOpt with
        | none => exact htop_cond.1
        | some top => exact htop_cond.1
      have hdisj_other : ∀ r, r ≤ 1 → r ≠ tr.val →
          NorFlash.agreesOn flash' cs0.flash (regionStartNat g r) g.region.val := by
        intro r hr hne
        have hd := regions_disjoint g htr_le hr (Ne.symm hne)
        have hd' : NorFlash.disjoint waddr.val rec.bytes.length (regionStartNat g r) g.region.val := by
          rw [hblen, hwaddr_eq]
          have hfit := hrec_valid.2.2.2.2.2.1; rw [hro, hrl] at hfit
          rcases hd with h | h
          · exact Or.inl (by omega)
          · exact Or.inr (by omega)
        exact NorFlash.writeBytes_disjoint_preserved hd'
      have hag_prefix_tr : NorFlash.agreesOn flash' cs0.flash (regionStartNat g tr.val) toff.val :=
        NorFlash.writeBytes_disjoint_preserved (by rw [hblen, hwaddr_eq]; exact Or.inr (Nat.le_refl _))
      have herased_after_tr : NorFlash.isErasedRange flash'
          (regionStartNat g tr.val + (toff.val + wlen.val)) (g.region.val - (toff.val + wlen.val)) := by
        intro i hi1 hi2
        have hag : flash'.mem i = cs0.flash.mem i :=
          NorFlash.writeBytes_disjoint_preserved
            (waddr := waddr.val) (a := regionStartNat g tr.val + (toff.val + wlen.val))
            (l := g.region.val - (toff.val + wlen.val))
            (by rw [hblen, hwaddr_eq]; exact Or.inl (by omega)) i hi1 hi2
        rw [hag]
        by_cases h0 : tr.val = 0
        · simp only [h0, ↓reduceIte] at htail_tr
          have ht := L.htail0; rw [htail_tr.2, htail_tr.1] at ht
          rw [h0] at hi1 hi2; exact ht i (by omega) (by omega)
        · have h1 : tr.val = 1 := by omega
          simp only [h0, ↓reduceIte] at htail_tr
          have ht := L.htail1; rw [htail_tr.2, htail_tr.1] at ht
          rw [h1] at hi1 hi2; exact ht i (by omega) (by omega)
      have hL'_and_win : ∃ L' : FlashLayout g flash', LayoutSeqWindow g flash' L' (some newEntry) := by
        by_cases htr0 : tr.val = 0
        · simp only [htr0, ↓reduceIte] at htail_tr
          have hlog0_pres := contiguousLog_agreesOn (s' := flash') L.hlog0
            (fun i hi1 hi2 => by rw [htr0] at hag_prefix_tr; exact hag_prefix_tr i (by omega) (by omega))
          have hlog0' := contiguousLog_append_one hlog0_pres (e := newEntry)
            (by rw [hrr, htr0]) (by rw [hro, htail_tr.1]) hnew_idx_mod hrec_valid hintact_rec
          have hag1 := hdisj_other 1 (by omega) (by omega)
          have hb1 := contiguousLog_bounds L.hlog1
          let L' : FlashLayout g flash' := {
            log0 := L.log0 ++ [newEntry], end0 := L.end0 + wlen.val, tail0 := .erased,
            log1 := L.log1, end1 := L.end1, tail1 := L.tail1,
            hlog0 := by simpa [newEntry, hrl] using hlog0',
            htail0 := by simpa [TailValid, htr0, htail_tr.1] using herased_after_tr,
            hlog1 := contiguousLog_agreesOn L.hlog1 (fun i hi1 hi2 => hag1 i (by omega) (by omega)),
            htail1 := tailValid_agreesOn L.htail1 (fun i hi1 hi2 => hag1 i (by omega) (by omega))
          }
          refine ⟨L', ?_⟩
          have hnr0 : newEntry.record.region.val = 0 := by simp [newEntry, hrr, htr0]
          cases topOpt with
          | none =>
            dsimp only [LayoutSeqWindow]
            refine ⟨1, by omega, hnew_idx_mod, by simp [hnr0, L', hwin.1], ?_, by simp [hnr0, L', hwin.1, newEntry, newIdx],
              by simp [hnr0, L', hwin.2]⟩
            intro e he; simp [hnr0, L', hwin.1] at he; subst he; exact ⟨Nat.le_refl _, Nat.le_refl _, Or.inl rfl⟩
          | some top =>
            obtain ⟨loAct, htop_r, htop_s, htop_last, htop_act, htop_span, htop_std⟩ := hwin
            rcases htop_cond.2.2 with hsame | ⟨htoff0, hlog0_nil⟩
            · have htop0 : top.record.region.val = 0 := by rw [← hsame]; exact htr0
              simp only [htop0, ↓reduceIte] at htop_last htop_act htop_span htop_std
              dsimp only [LayoutSeqWindow]
              refine ⟨loAct, by omega, hnew_idx_mod, by simp [hnr0, L', getLast?_append_singleton], ?_, ?_,
                by simp only [hnr0, ↓reduceIte, L']; exact htop_std⟩
              · simp only [hnr0, ↓reduceIte, L']
                intro e he
                rcases List.mem_append.mp he with hprev | hnew
                · obtain ⟨h1, h2, _⟩ := htop_act e hprev
                  exact ⟨h1, by simp [newEntry, newIdx]; omega, Or.inr (by simp [newEntry, newIdx]; omega)⟩
                · simp at hnew; subst hnew
                  have hlo_le : loAct ≤ top.idx := by
                    have hmem_t : top ∈ L.log0 := by
                      cases h : L.log0 with
                      | nil => rw [h] at htop_last; simp at htop_last
                      | cons a xs =>
                        rw [h, List.getLast?_eq_some_getLast (List.cons_ne_nil a xs), Option.some.injEq] at htop_last
                        subst htop_last; exact List.getLast_mem _
                    exact (htop_act top hmem_t).1
                  exact ⟨by simp [newEntry, newIdx]; omega, Nat.le_refl _, Or.inl rfl⟩
              · simp only [hnr0, ↓reduceIte, L', List.length_append, List.length_singleton, newEntry, newIdx]
                omega
            · simp only [htr0, ↓reduceIte] at hlog0_nil
              have htop_ne0 : top.record.region.val ≠ 0 := by
                intro h0; simp only [h0, ↓reduceIte] at htop_last; rw [hlog0_nil] at htop_last; simp at htop_last
              simp only [htop_ne0, ↓reduceIte] at htop_last htop_act htop_span htop_std
              have hb1' := contiguousLog_bounds L.hlog1
              refine ⟨top.idx + 1, by omega, hnew_idx_mod, by simp [hnr0, L', hlog0_nil], ?_,
                by simp [hnr0, L', hlog0_nil, newEntry, newIdx], ?_⟩
              · intro e he; simp [hnr0, L', hlog0_nil] at he; subst he
                exact ⟨Nat.le_refl _, Nat.le_refl _, Or.inl rfl⟩
              · simp only [hnr0, ↓reduceIte, L']
                intro e he; obtain ⟨hlo, hhi, _⟩ := htop_act e he; exact ⟨by omega, by omega⟩
        · have htr1 : tr.val = 1 := by omega
          simp only [htr0, ↓reduceIte] at htail_tr
          have hlog1_pres := contiguousLog_agreesOn (s' := flash') L.hlog1
            (fun i hi1 hi2 => by rw [htr1] at hag_prefix_tr; exact hag_prefix_tr i (by omega) (by omega))
          have hlog1' := contiguousLog_append_one hlog1_pres (e := newEntry)
            (by rw [hrr, htr1]) (by rw [hro, htail_tr.1]) hnew_idx_mod hrec_valid hintact_rec
          have hag0 := hdisj_other 0 (by omega) (by omega)
          have hb0 := contiguousLog_bounds L.hlog0
          let L' : FlashLayout g flash' := {
            log0 := L.log0, end0 := L.end0, tail0 := L.tail0,
            log1 := L.log1 ++ [newEntry], end1 := L.end1 + wlen.val, tail1 := .erased,
            hlog0 := contiguousLog_agreesOn L.hlog0 (fun i hi1 hi2 => hag0 i (by omega) (by omega)),
            htail0 := tailValid_agreesOn L.htail0 (fun i hi1 hi2 => hag0 i (by omega) (by omega)),
            hlog1 := by simpa [newEntry, hrl] using hlog1',
            htail1 := by simpa [TailValid, htr1, htail_tr.1] using herased_after_tr
          }
          refine ⟨L', ?_⟩
          have hnr1 : newEntry.record.region.val = 1 := by simp [newEntry, hrr, htr1]
          have hnr0 : newEntry.record.region.val ≠ 0 := by omega
          cases topOpt with
          | none =>
            dsimp only [LayoutSeqWindow]
            refine ⟨1, by omega, hnew_idx_mod, by simp [hnr0, L', hwin.2], ?_, by simp [hnr0, L', hwin.2, newEntry, newIdx],
              by simp [hnr0, L', hwin.1]⟩
            intro e he; simp [hnr0, L', hwin.2] at he; subst he; exact ⟨Nat.le_refl _, Nat.le_refl _, Or.inl rfl⟩
          | some top =>
            obtain ⟨loAct, htop_r, htop_s, htop_last, htop_act, htop_span, htop_std⟩ := hwin
            rcases htop_cond.2.2 with hsame | ⟨htoff0, hlog1_nil⟩
            · have htop1 : top.record.region.val ≠ 0 := by rw [← hsame]; exact htr0
              simp only [htop1, ↓reduceIte] at htop_last htop_act htop_span htop_std
              dsimp only [LayoutSeqWindow]
              refine ⟨loAct, by omega, hnew_idx_mod, by simp [hnr0, L', getLast?_append_singleton], ?_, ?_,
                by simp only [hnr0, ↓reduceIte, L']; exact htop_std⟩
              · simp only [hnr0, ↓reduceIte, L']
                intro e he
                rcases List.mem_append.mp he with hprev | hnew
                · obtain ⟨h1, h2, _⟩ := htop_act e hprev
                  exact ⟨h1, by simp [newEntry, newIdx]; omega, Or.inr (by simp [newEntry, newIdx]; omega)⟩
                · simp at hnew; subst hnew
                  have hlo_le : loAct ≤ top.idx := by
                    have hmem_t : top ∈ L.log1 := by
                      cases h : L.log1 with
                      | nil => rw [h] at htop_last; simp at htop_last
                      | cons a xs =>
                        rw [h, List.getLast?_eq_some_getLast (List.cons_ne_nil a xs), Option.some.injEq] at htop_last
                        subst htop_last; exact List.getLast_mem _
                    exact (htop_act top hmem_t).1
                  exact ⟨by simp [newEntry, newIdx]; omega, Nat.le_refl _, Or.inl rfl⟩
              · simp only [hnr0, ↓reduceIte, L', List.length_append, List.length_singleton, newEntry, newIdx]
                omega
            · simp only [htr0, ↓reduceIte] at hlog1_nil
              have htop0 : top.record.region.val = 0 := by
                by_contra hne0; simp only [hne0, ↓reduceIte] at htop_last; rw [hlog1_nil] at htop_last; simp at htop_last
              simp only [htop0, ↓reduceIte] at htop_last htop_act htop_span htop_std
              have hb0' := contiguousLog_bounds L.hlog0
              refine ⟨top.idx + 1, by omega, hnew_idx_mod, by simp [hnr0, L', hlog1_nil], ?_,
                by simp [hnr0, L', hlog1_nil, newEntry, newIdx], ?_⟩
              · intro e he; simp [hnr0, L', hlog1_nil] at he; subst he
                exact ⟨Nat.le_refl _, Nat.le_refl _, Or.inl rfl⟩
              · simp only [hnr0, ↓reduceIte, L']
                intro e he; obtain ⟨hlo, hhi, _⟩ := htop_act e he; exact ⟨by omega, by omega⟩
      obtain ⟨L', hwin'⟩ := hL'_and_win
      have hacked_intact' : ∀ a, cs0.acked = some a → RecordIntact g flash' a := by
        intro a ha
        have hint := hacked_intact a ha
        have ha_eq : topOpt.map LogEntry.record = some a := by
          rw [htop_eq]; simp [latestOnFlash, hland_none, ha]
        cases topOpt with
        | none => simp at ha_eq
        | some top =>
          simp at ha_eq; subst ha_eq
          refine ⟨hint.1, hint.2.1, hint.2.2.1, ?_⟩
          by_cases hsame : top.record.region.val = tr.val
          · have hend_top : top.record.off.val + top.record.recordLen.val = toff.val := by
              obtain ⟨_, _, _, hlast, _, _, _⟩ := hwin
              by_cases h0 : tr.val = 0
              · have ht0 : top.record.region.val = 0 := by omega
                simp only [h0, ↓reduceIte] at htail_tr; simp only [ht0, ↓reduceIte] at hlast
                rw [contiguousLog_last_end L.hlog0 hlast, htail_tr.1]
              · have ht0 : top.record.region.val ≠ 0 := by omega
                simp only [h0, ↓reduceIte] at htail_tr; simp only [ht0, ↓reduceIte] at hlast
                rw [contiguousLog_last_end L.hlog1 hlast, htail_tr.1]
            have hag : NorFlash.agreesOn flash' cs0.flash
                (regionStartNat g top.record.region.val + top.record.off.val) top.record.recordLen.val := by
              intro i hi1 hi2; rw [hsame] at hi1 hi2; exact hag_prefix_tr i (by omega) (by omega)
            rw [NorFlash.readSlice_eq_of_agreesOn hag]; exact hint.2.2.2
          · have hag := hdisj_other top.record.region.val hint.1 hsame
            rw [NorFlash.readSlice_eq_of_agreesOn (fun i hi1 hi2 => hag i (by omega) (by have := hint.2.1; omega))]
            exact hint.2.2.2
      refine ⟨L', some newEntry, hwin', rfl, hacked_intact', (fun _ h => by cases h),
        (fun _ _ _ _ _ _ _ h => by cases h), (fun _ _ _ _ _ _ h => by cases h), (fun _ _ _ _ _ h => by cases h)⟩
    | @ackSave tr toff wlen nseq rec pos' hph hpos' =>
      obtain ⟨_, hland_some, hintact, hrr, hro, hrl, hrs, htail_er, _⟩ := hlanded_inv tr toff wlen nseq rec hph
      have htop_rec : topOpt.map LogEntry.record = some rec := by
        rw [htop_eq]; simp [latestOnFlash, hland_some]
      cases topOpt with
      | none => simp at htop_rec
      | some top =>
        simp at htop_rec; subst htop_rec
        unfold pos_after_write at hpos'
        obtain ⟨sum, hadd, hsum_val⟩ := u32_add_ok (a := toff) (b := wlen) (by
          rw [← hro, ← hrl]; have := hintact.2.1; scalar_tac)
        simp only [hadd, bind_ok, Result.ok.injEq] at hpos'
        subst hpos'
        have hpos_match : { region := tr, append := some sum, seq := nseq : Pos } =
            expectedRecoveredPos g cs0.flash L (some top) := by
          unfold expectedRecoveredPos
          obtain ⟨_, _, _, hlast, _, _, _⟩ := hwin
          have htr_val : top.record.region.val = tr.val := congrArg UScalar.val hrr
          by_cases hr0 : tr.val = 0
          · have ht0 : top.record.region.val = 0 := by omega
            simp only [hr0, ↓reduceIte] at htail_er
            simp only [ht0, ↓reduceIte] at hlast ⊢
            have hend0 := contiguousLog_last_end L.hlog0 hlast
            have hb0 := contiguousLog_bounds L.hlog0
            have hsum_eq : sum = u32OfNat L.end0 := by
              apply UScalar.eq_of_val_eq
              have hlt : L.end0 < U32.size := by scalar_tac
              rw [u32OfNat_val hlt, hsum_val, ← hro, ← hrl, hend0]
            simp [htail_er, hrr, hrs, hsum_eq]
          · have ht0 : top.record.region.val ≠ 0 := by omega
            simp only [hr0, ↓reduceIte] at htail_er
            simp only [ht0, ↓reduceIte] at hlast ⊢
            have hend1 := contiguousLog_last_end L.hlog1 hlast
            have hb1 := contiguousLog_bounds L.hlog1
            have hsum_eq : sum = u32OfNat L.end1 := by
              apply UScalar.eq_of_val_eq
              have hlt : L.end1 < U32.size := by scalar_tac
              rw [u32OfNat_val hlt, hsum_val, ← hro, ← hrl, hend1]
            simp [htail_er, hrr, hrs, hsum_eq]
        refine ⟨L, some top, hwin, rfl, (fun a ha => by cases ha; exact hintact),
          (fun p hp => by cases hp; exact ⟨rfl, rfl, hpos_match⟩),
          (fun _ _ _ _ _ _ _ h => by cases h), (fun _ _ _ _ _ _ h => by cases h),
          (fun _ _ _ _ _ h => by cases h)⟩
    | @recover buf0 finalBuf pos' bestVer bestLen hbuf0 hrecov =>
      obtain ⟨_, hdet⟩ := recover_from_flash_correct g cs0.flash L topOpt buf0 hlay hdiv hwin hbuf0
      obtain ⟨hpos_det, _, _, _, _⟩ := hdet pos' bestVer bestLen finalBuf hrecov
      have hlatest_eq : latestOnFlash { cs0 with pos := some pos', phase := .idle, acked := match cs0.landed with | some r => some r | none => cs0.acked, landed := none } =
          latestOnFlash cs0 := by
        simp [latestOnFlash]
      have hacked_intact' : ∀ a, (match cs0.landed with | some r => some r | none => cs0.acked) = some a →
          RecordIntact g cs0.flash a := by
        intro a ha
        have ha_top : topOpt.map LogEntry.record = some a := by
          rw [htop_eq]; simp [latestOnFlash]; exact ha
        cases topOpt with
        | none => simp at ha_top
        | some top =>
          simp at ha_top; subst ha_top
          have ⟨hmem, _, _⟩ := layoutSeqWindow_top_unique_max hlay hwin
          have hmem_intact : ∀ {r startOff endOff} {es : List LogEntry},
              ContiguousLog g cs0.flash r startOff es endOff → ∀ e ∈ es, RecordIntact g cs0.flash e.record := by
            intro r startOff endOff es hl
            induction hl with
            | nil => intro _ h; cases h
            | cons _ _ _ _ hi _ _ ih =>
              intro e he; rcases List.mem_cons.mp he with rfl | hrest
              · exact hi
              · exact ih e hrest
          rcases List.mem_append.mp hmem with h0 | h1
          · exact hmem_intact L.hlog0 top h0
          · exact hmem_intact L.hlog1 top h1
      refine ⟨L, topOpt, hwin, by rw [hlatest_eq]; exact htop_eq, hacked_intact', ?_,
        (fun _ _ _ _ _ _ _ h => by cases h), (fun _ _ _ _ _ _ h => by cases h), (fun _ _ _ _ _ h => by cases h)⟩
      intro p hp
      cases hp
      exact ⟨rfl, rfl, hpos_det⟩

/-- **Precondition Discharge Theorem 1 (`composed_erase_preconditions_discharged`)**:
In EVERY reachable state where `FlashStore` is about to erase `[efrom, eto)` of
region `tr`, `NorFlash.EraseValid` AND disjointness from every acknowledged record
hold as **proved consequences** of reachability and extracted `plan_save` (never
assumed as transition preconditions). -/
theorem composed_erase_preconditions_discharged (g : FlashGeometry)
    (hlay : layout_ok g = ok true) (hdiv : AlignDivides g)
    {cs : ComposedState} (hr : ComposedReachable g cs)
    {efrom eto tr waddr wlen nseq : U32} {rec : CommittedRecord}
    (hph : cs.phase = .erasing efrom eto tr waddr wlen nseq rec) :
    NorFlash.EraseValid (toFlashConfig g) efrom.val eto.val ∧
    (∀ a, cs.acked = some a →
      NorFlash.disjoint efrom.val (eto.val - efrom.val)
        (regionStartNat g a.region.val + a.off.val) a.recordLen.val) := by
  obtain ⟨_, topOpt, _, htop_eq, hacked_intact, _, herasing_inv, _, _⟩ :=
    extracted_flashstore_crash_consistent g hlay hdiv hr
  obtain ⟨_, hland_none, htr_le, hefrom_eq, heto_eq, _, herase_v, _, _, _, _, _, _, htop_cond⟩ :=
    herasing_inv efrom eto tr waddr wlen nseq rec hph
  refine ⟨herase_v, fun a ha => ?_⟩
  have hint := hacked_intact a ha
  have ha_eq : topOpt.map LogEntry.record = some a := by
    rw [htop_eq]; simp [latestOnFlash, hland_none, ha]
  cases topOpt with
  | none => simp at ha_eq
  | some top =>
    simp at ha_eq; subst ha_eq
    have hne : tr.val ≠ top.record.region.val := fun h => htop_cond.1 (UScalar.eq_of_val_eq h)
    have hdisj := regions_disjoint g htr_le hint.1 hne
    rw [heto_eq, Nat.add_sub_cancel_left, hefrom_eq]
    rcases hdisj with hd | hd
    · exact Or.inl (by omega)
    · exact Or.inr (by have := hint.2.1; omega)

/-- **Precondition Discharge Theorem 2 (`composed_write_preconditions_discharged`)**:
In EVERY reachable state where `FlashStore` is about to write `rec` at `(waddr, wlen)`:
1. `NorFlash.WriteValid (toFlashConfig g) waddr.val wlen.val` holds,
2. `NorFlash.isErasedRange cs.flash waddr.val wlen.val` holds (never writes to unerased NOR flash),
3. `[waddr, waddr + wlen)` is disjoint from any previously acknowledged record `a`,
4. `seq_newer nseq a.seq = ok true` (strictly newer across `u32::MAX → 0` wraparound). -/
theorem composed_write_preconditions_discharged (g : FlashGeometry)
    (hlay : layout_ok g = ok true) (hdiv : AlignDivides g)
    {cs : ComposedState} (hr : ComposedReachable g cs)
    {tr toff waddr wlen nseq : U32} {rec : CommittedRecord}
    (hph : cs.phase = .writing tr toff waddr wlen nseq rec) :
    NorFlash.WriteValid (toFlashConfig g) waddr.val wlen.val ∧
    NorFlash.isErasedRange cs.flash waddr.val wlen.val ∧
    (∀ a, cs.acked = some a →
      NorFlash.disjoint waddr.val wlen.val
        (regionStartNat g a.region.val + a.off.val) a.recordLen.val ∧
      seq_newer nseq a.seq = ok true) := by
  obtain ⟨L, topOpt, hwin, htop_eq, hacked_intact, _, _, hwriting_inv, _⟩ :=
    extracted_flashstore_crash_consistent g hlay hdiv hr
  obtain ⟨_, hland_none, htr_le, hwaddr_eq, hwrite_v, herased, hrec_valid,
    _, hro, hrl, _, htail_tr, htop_cond⟩ := hwriting_inv tr toff waddr wlen nseq rec hph
  refine ⟨hwrite_v, herased, fun a ha => ?_⟩
  have hint := hacked_intact a ha
  have ha_eq : topOpt.map LogEntry.record = some a := by
    rw [htop_eq]; simp [latestOnFlash, hland_none, ha]
  cases topOpt with
  | none => simp at ha_eq
  | some top =>
    simp at ha_eq; subst ha_eq
    refine ⟨?_, htop_cond.2.1⟩
    by_cases hsame : tr.val = top.record.region.val
    · obtain ⟨_, _, _, hlast, _, _, _⟩ := hwin
      have hend_top : top.record.off.val + top.record.recordLen.val = toff.val := by
        by_cases h0 : tr.val = 0
        · have ht0 : top.record.region.val = 0 := by omega
          simp only [h0, ↓reduceIte] at htail_tr; simp only [ht0, ↓reduceIte] at hlast
          rw [contiguousLog_last_end L.hlog0 hlast, htail_tr.1]
        · have ht0 : top.record.region.val ≠ 0 := by omega
          simp only [h0, ↓reduceIte] at htail_tr; simp only [ht0, ↓reduceIte] at hlast
          rw [contiguousLog_last_end L.hlog1 hlast, htail_tr.1]
      rw [hwaddr_eq, hsame]
      exact Or.inr (by omega)
    · have hdisj := regions_disjoint g htr_le hint.1 hsame
      have hfit := hrec_valid.2.2.2.2.2.1; rw [hro, hrl] at hfit
      rw [hwaddr_eq]
      rcases hdisj with hd | hd
      · exact Or.inl (by omega)
      · exact Or.inr (by have := hint.2.1; omega)

/-- **End-to-End Crash-Consistent Recovery Theorem (`extracted_recovery_functional_correctness`)**:
In EVERY reachable state `cs` of the composed `FlashStore` + `NorFlash` system
(including mid-erase, mid-write, torn tail, post-write/pre-ack, post-ambiguous-write-failure,
and across `u32::MAX → 0` wraparound), running the **actual Aeneas-extracted
`store_core.scan_init` and `store_core.scan_step`** directly on physical `cs.flash`
(`RecoverFromFlash`):
1. ALWAYS succeeds (existence), and
2. DETERMINISTICALLY selects `latestOnFlash cs` (either the newly landed record
   `cs.landed` if a write completed physically, or the previously acknowledged
   record `cs.acked` otherwise), loading its exact bytes into `finalBuf` and
   returning a `Pos` with `pos'.seq = latest.seq` and `pos'.region = latest.region`! -/
theorem extracted_recovery_functional_correctness (g : FlashGeometry)
    (hlay : layout_ok g = ok true) (hdiv : AlignDivides g)
    {cs : ComposedState} (hr : ComposedReachable g cs)
    (buf0 : Slice U8) (hbuf0 : buf0.length = g.buf_cap.val) :
    (∃ pos' ver' len' finalBuf,
      RecoverFromFlash g cs.flash buf0 pos' ver' len' finalBuf) ∧
    (∀ pos' ver' len' finalBuf,
      RecoverFromFlash g cs.flash buf0 pos' ver' len' finalBuf →
      ver' = (latestOnFlash cs).map CommittedRecord.version ∧
      len' = (match latestOnFlash cs with | none => 0#u32 | some r => r.payloadLen) ∧
      (∀ r, latestOnFlash cs = some r →
        pos'.region = r.region ∧
        pos'.seq = r.seq ∧
        ∀ i, i < r.recordLen.val →
          finalBuf.val[i]? = some (u8OfUInt8 (r.bytes.getD i 0)))) := by
  obtain ⟨L, topOpt, hwin, htop_eq, _, _, _, _, _⟩ :=
    extracted_flashstore_crash_consistent g hlay hdiv hr
  obtain ⟨⟨fb, hrec, _, _⟩, hdet⟩ := recover_from_flash_correct g cs.flash L topOpt buf0 hlay hdiv hwin hbuf0
  refine ⟨⟨_, _, _, fb, hrec⟩, fun pos' ver' len' finalBuf hr' => ?_⟩
  obtain ⟨hp', hv', hl', _, hbytes'⟩ := hdet pos' ver' len' finalBuf hr'
  rw [← htop_eq]
  cases topOpt with
  | none =>
    exact ⟨hv', hl', fun _ h => by cases h⟩
  | some top =>
    refine ⟨by simpa [Option.map] using hv', by simpa using hl', fun r hr_eq => ?_⟩
    simp at hr_eq; subst hr_eq
    exact ⟨by rw [hp']; rfl, by rw [hp']; rfl, hbytes' top rfl⟩

/-- **Crash-Consistent Recovery Set Inclusion (`Recover_Rust(F_after_crash) ∈ {S_old, S_new}`)**:
Corollary stated directly in terms of `cs.acked` (`S_old`) and `cs.landed` (`S_new`):
every execution of extracted `scan_init` + `scan_step` on physical `cs.flash`
recovers either `cs.acked` or `cs.landed` (and strictly `cs.acked` when no
unacknowledged write has landed). -/
theorem extracted_recovery_in_old_or_new (g : FlashGeometry)
    (hlay : layout_ok g = ok true) (hdiv : AlignDivides g)
    {cs : ComposedState} (hr : ComposedReachable g cs)
    {buf0 finalBuf : Slice U8} {pos' : Pos} {ver' : Option U16} {len' : U32}
    (hbuf0 : buf0.length = g.buf_cap.val)
    (hrecov : RecoverFromFlash g cs.flash buf0 pos' ver' len' finalBuf) :
    (ver' = cs.acked.map CommittedRecord.version ∧
     ∀ r, cs.acked = some r → cs.landed = none →
       pos'.seq = r.seq ∧ ∀ i, i < r.recordLen.val →
         finalBuf.val[i]? = some (u8OfUInt8 (r.bytes.getD i 0))) ∨
    (∃ rNew, cs.landed = some rNew ∧
     ver' = some rNew.version ∧
     len' = rNew.payloadLen ∧
     pos'.seq = rNew.seq ∧
     ∀ i, i < rNew.recordLen.val →
       finalBuf.val[i]? = some (u8OfUInt8 (rNew.bytes.getD i 0))) := by
  have ⟨_, hdet⟩ := extracted_recovery_functional_correctness g hlay hdiv hr buf0 hbuf0
  obtain ⟨hv', hl', hbytes'⟩ := hdet pos' ver' len' finalBuf hrecov
  cases hl : cs.landed with
  | none =>
    left
    simp only [latestOnFlash, hl] at hv' hbytes'
    exact ⟨hv', fun r hr _ => ⟨(hbytes' r hr).2.1, (hbytes' r hr).2.2⟩⟩
  | some rNew =>
    right
    simp only [latestOnFlash, hl] at hv' hl' hbytes'
    obtain ⟨_, hseq, hby⟩ := hbytes' rNew rfl
    exact ⟨rNew, rfl, hv', hl', hseq, hby⟩

/-- **No Duplicate Sequence Numbers After Ambiguous Write Failure / Async Cancellation**:
If `writeFailAmbiguousLanded` lands `rec` on flash while returning an error / being
cancelled, then:
1. `cs'.pos = none` (no save can proceed using stale cached `pos`),
2. the required recovery step (`ComposedStep.recover` via extracted `scan_init`/`scan_step`)
   restores `pos'.seq = rec.seq`, and
3. the next `plan_save` assigns a `next_seq` strictly newer than `rec.seq`
   (`seq_newer next_seq rec.seq = ok true`, NEVER duplicating `rec.seq`). -/
theorem ambiguous_write_no_duplicate_seq (g : FlashGeometry)
    (hlay : layout_ok g = ok true) (hdiv : AlignDivides g)
    {cs0 cs1 cs2 : ComposedState} {tr toff waddr wlen nseq : U32} {rec : CommittedRecord}
    (hr0 : ComposedReachable g cs0)
    (hph : cs0.phase = .writing tr toff waddr wlen nseq rec)
    (hambig : cs1 = { cs0 with flash := NorFlash.writeBytes cs0.flash waddr.val rec.bytes,
                               pos := none, phase := .idle, landed := some rec })
    {buf0 finalBuf : Slice U8} {pos' : Pos} {ver' : Option U16} {len' : U32}
    (hbuf0 : buf0.length = g.buf_cap.val)
    (hrecov : RecoverFromFlash g cs1.flash buf0 pos' ver' len' finalBuf)
    (hcs2 : cs2 = { cs1 with pos := some pos', phase := .idle, acked := some rec, landed := none })
    {payload_len : U32} {plan : SavePlan}
    (hplan : plan_save g pos' payload_len = ok plan) :
    cs1.pos = none ∧
    pos'.seq = rec.seq ∧
    match plan with
    | .TooLarge => True
    | .WriteOnly _ _ _ _ nextSeq => seq_newer nextSeq rec.seq = ok true
    | .EraseThenWrite _ _ _ _ _ nextSeq => seq_newer nextSeq rec.seq = ok true := by
  have hr1 : ComposedReachable g cs1 := by
    rw [hambig]; exact .step hr0 (.writeFailAmbiguousLanded hph)
  have ⟨_, hdet⟩ := extracted_recovery_functional_correctness g hlay hdiv hr1 buf0 hbuf0
  obtain ⟨_, _, hrec_sel⟩ := hdet pos' ver' len' finalBuf hrecov
  have hlat1 : latestOnFlash cs1 = some rec := by rw [hambig]; rfl
  obtain ⟨hpos_reg, hpos_seq, _⟩ := hrec_sel rec hlat1
  have hpos_rle : pos'.region.val ≤ 1 := by
    obtain ⟨_, _, _, _, _, _, _, hwr, _⟩ := extracted_flashstore_crash_consistent g hlay hdiv hr0
    have ⟨_, _, htr_le, _, _, _, _, hrr, _, _, _, _, _⟩ := hwr tr toff waddr wlen nseq rec hph
    rw [hpos_reg, hrr]; exact htr_le
  have hps := H5iAppLib.post_of_ok (plan_save_safe g pos' payload_len hlay hdiv hpos_rle) hplan
  refine ⟨by rw [hambig], hpos_seq, ?_⟩
  cases plan with
  | TooLarge => trivial
  | WriteOnly _ _ _ _ nextSeq =>
    dsimp only at hps ⊢
    obtain ⟨ns', hns', hnew⟩ := seq_newer_next_seq rec.seq
    have hp_ns := H5iAppLib.post_of_ok (next_seq_spec rec.seq) hns'
    have heq : nextSeq = ns' := by rw [hps.2.2.2.2.2.2.2.1, hpos_seq, hp_ns.1]
    rwa [heq]
  | EraseThenWrite _ _ _ _ _ nextSeq =>
    dsimp only at hps ⊢
    obtain ⟨ns', hns', hnew⟩ := seq_newer_next_seq rec.seq
    have hp_ns := H5iAppLib.post_of_ok (next_seq_spec rec.seq) hns'
    have heq : nextSeq = ns' := by rw [hps.2.2.2.2.2.2.2.2.2.2.2.1, hpos_seq, hp_ns.1]
    rwa [heq]

end h5i_app_embedded.FlashStoreProofs
