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
  have hbv : (core.num.U32.wrapping_add s 1#u32).bv = s.bv + (1#u32).bv := rfl
  change (core.num.U32.wrapping_add s 1#u32).bv.toNat = (s.bv.toNat + 1) % 4294967296
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
      | .WriteOnly tr toff waddr wlen nseq pinf =>
        tr = pos.region ∧
        pos.append = some toff ∧
        waddr.val = regionStartNat g tr.val + toff.val ∧
        NorFlash.WriteValid (toFlashConfig g) waddr.val wlen.val ∧
        wlen.val ≤ g.buf_cap.val ∧
        toff.val + wlen.val ≤ g.region.val ∧
        NorFlash.disjoint waddr.val wlen.val (regionStartNat g pos.region.val) toff.val ∧
        nseq = core.num.U32.wrapping_add pos.seq 1#u32 ∧
        pinf = { pos with append := none }
      | .EraseThenWrite efrom eto tr waddr wlen nseq pinf =>
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
        pinf.region = pos.region ∧ pinf.append = none ∧ pinf.seq = pos.seq ⦄ := by
  have hp := H5iAppLib.post_of_ok (layout_ok_spec g) hlay rfl
  have hcap_u32 : g.base.val + g.region.val ≤ U32.max := by
    have := hp.in_capacity; scalar_tac
  unfold plan_save
  simp only [hlay, bind_tc_ok, bind_ok, ↓reduceIte]
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
        have herase_ok : ∀ (pinf : Pos), pinf.region = pos.region → pinf.append = none → pinf.seq = pos.seq →
            (do
              let other ← other_region pos.region
              let start ← region_start g other
              let i ← start + g.region
              ok (SavePlan.EraseThenWrite start i other start rl nseq pinf)) ⦃ plan =>
              match plan with
              | .EraseThenWrite efrom eto tr waddr wlen ns pi =>
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
                pi.region = pos.region ∧ pi.append = none ∧ pi.seq = pos.seq
              | _ => False ⦄ := by
          intro pinf hpr hpa hps
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
            ?_, hrl_le_buf, hrl_le_reg, hnseq_p.1, hpr, hpa, hps⟩
          rcases hdisj_reg with hd | hd
          · exact Or.inl (by omega)
          · exact Or.inr (by omega)
        cases hpos_app : pos.append with
        | none =>
          exact WP.spec_mono (herase_ok pos rfl hpos_app rfl) (fun p hp' => by cases p <;> simpa using hp')
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
                  hnseq_p.1, rfl⟩
              · simp only [hrem0, ↓reduceIte]
                exact WP.spec_mono (herase_ok { pos with append := none } rfl rfl rfl) (fun p hp' => by cases p <;> simpa using hp')
            · simp only [hrl_fit, ↓reduceIte]
              exact WP.spec_mono (herase_ok { pos with append := none } rfl rfl rfl) (fun p hp' => by cases p <;> simpa using hp')
          · simp only [hoff_le, ↓reduceIte]
            exact WP.spec_mono (herase_ok { pos with append := none } rfl rfl rfl) (fun p hp' => by cases p <;> simpa using hp')

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
    (efrom eto tr waddr wlen nseq : U32) (pinf : Pos)
    (hlay : layout_ok g = ok true) (hdiv : AlignDivides g) (hreg : pos.region.val ≤ 1)
    (hplan : plan_save g pos payload_len = ok (.EraseThenWrite efrom eto tr waddr wlen nseq pinf))
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
    (hplan : (∃ tr toff nseq pinf, plan_save g pos payload_len = ok (.WriteOnly tr toff waddr wlen nseq pinf)) ∨
             (∃ efrom eto tr nseq pinf, plan_save g pos payload_len = ok (.EraseThenWrite efrom eto tr waddr wlen nseq pinf)))
    (hwrite : NorFlash.NorFlashStep (toFlashConfig g) (.write waddr.val bytes) s s') :
    RecordIntact g s' committed := by
  have hdisj : NorFlash.disjoint waddr.val bytes.length
      (regionStartNat g committed.region.val + committed.off.val) committed.recordLen.val := by
    rw [hlen_eq, hcomm_reg]
    rcases hplan with ⟨tr, toff, nseq, pinf, hp_eq⟩ | ⟨efrom, eto, tr, nseq, pinf, hp_eq⟩
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

/-! ## 7. Compositional System-Level Transition Model (`ComposedStep`) -/

/-- Phase of the composed `FlashStore` + `NorFlash` system. -/
inductive SavePhase where
  | idle
  | erasing (efrom eto targetRegion waddr wlen nseq : U32) (inFlight : CommittedRecord)
  | writing (targetRegion targetOff waddr wlen nseq : U32) (inFlight : CommittedRecord)
  | writeLanded (targetRegion targetOff wlen nseq : U32) (inFlight : CommittedRecord)

/-- Full compositional state: physical `NorFlash.FlashState` + `store_core.Pos` +
protocol phase + ghost history of the last acknowledged record. -/
structure ComposedState where
  flash : NorFlash.FlashState
  pos : Pos
  phase : SavePhase
  acked : Option CommittedRecord

/-- Crash-consistency & durability invariant on `ComposedState`:
1. `pos.region ≤ 1`,
2. Whenever an acknowledged record `rec` exists, its bytes remain 100% intact on
   physical NOR flash (`RecordIntact`), and if `pos.append = some off`, `rec`
   lies strictly before `off` in `pos.region` (`rec.region = pos.region`),
3. Whenever a write has landed before acknowledgment (`writeLanded`), BOTH the
   new `inFlight` record AND (if present) the previous `acked` record are intact,
   and `seq_newer inFlight.seq pos.seq = ok true` (strictly newer across wraparound!). -/
def CrashConsistentInv (g : FlashGeometry) (cs : ComposedState) : Prop :=
  cs.pos.region.val ≤ 1 ∧
  (∀ rec, cs.acked = some rec →
    RecordIntact g cs.flash rec ∧
    rec.region = cs.pos.region ∧
    (∀ off, cs.pos.append = some off → rec.off.val + rec.recordLen.val ≤ off.val)) ∧
  (∀ tr toff wlen nseq rec, cs.phase = .writeLanded tr toff wlen nseq rec →
    RecordIntact g cs.flash rec ∧
    seq_newer rec.seq cs.pos.seq = ok true ∧
    rec.region = tr ∧ rec.off = toff ∧ rec.recordLen = wlen ∧ rec.seq = nseq)

/-- Initial erased NOR flash state with no acknowledged record. -/
def composedInit : ComposedState where
  flash := { mem := fun _ => NorFlash.erasedByte }
  pos := { region := 1#u32, append := none, seq := 0#u32 }
  phase := .idle
  acked := none

/-- Compositional transition steps executing the **actual Aeneas-extracted**
`plan_save`, `encode_record`, and `pos_after_write` against `NorFlash.NorFlashStep`. -/
inductive ComposedStep (g : FlashGeometry) : ComposedState → ComposedState → Prop where
  | beginSaveAppend {cs : ComposedState} {buf buf' : Slice U8}
      {version : U16} {payload_len a tr toff waddr wlen nseq : U32} {pinf : Pos}
      (hidle : cs.phase = .idle)
      (ha : align g.write_size g.read_size = ok a)
      (hplan : plan_save g cs.pos payload_len = ok (.WriteOnly tr toff waddr wlen nseq pinf))
      (henc : encode_record buf nseq version payload_len a = ok (some wlen, buf'))
      (hblen : buf'.length = g.buf_cap.val) :
      ComposedStep g cs {
        cs with
        pos := pinf
        phase := .writing tr toff waddr wlen nseq {
          region := tr, off := toff, seq := nseq, version := version,
          payloadLen := payload_len, recordLen := wlen,
          bytes := slicePrefixToBytes buf' wlen.val
        }
      }
  | beginSaveSwitch {cs : ComposedState} {buf buf' : Slice U8}
      {version : U16} {payload_len a efrom eto tr waddr wlen nseq : U32} {pinf : Pos}
      (hidle : cs.phase = .idle)
      (ha : align g.write_size g.read_size = ok a)
      (hplan : plan_save g cs.pos payload_len = ok (.EraseThenWrite efrom eto tr waddr wlen nseq pinf))
      (henc : encode_record buf nseq version payload_len a = ok (some wlen, buf'))
      (hblen : buf'.length = g.buf_cap.val) :
      ComposedStep g cs {
        cs with
        pos := pinf
        phase := .erasing efrom eto tr waddr wlen nseq {
          region := tr, off := 0#u32, seq := nseq, version := version,
          payloadLen := payload_len, recordLen := wlen,
          bytes := slicePrefixToBytes buf' wlen.val
        }
      }
  | eraseOk {cs : ComposedState} {efrom eto tr waddr wlen nseq : U32} {rec : CommittedRecord}
      {flash' : NorFlash.FlashState}
      (hph : cs.phase = .erasing efrom eto tr waddr wlen nseq rec)
      (hdisj : NorFlash.disjoint efrom.val (eto.val - efrom.val) (regionStartNat g cs.pos.region.val) g.region.val)
      (hstep : NorFlash.NorFlashStep (toFlashConfig g) (.erase efrom.val eto.val) cs.flash flash') :
      ComposedStep g cs { cs with flash := flash', phase := .writing tr 0#u32 waddr wlen nseq rec }
  | eraseFailPowerCut {cs : ComposedState} {efrom eto tr waddr wlen nseq : U32} {rec : CommittedRecord}
      {flash' : NorFlash.FlashState}
      (hph : cs.phase = .erasing efrom eto tr waddr wlen nseq rec)
      (hdisj : NorFlash.disjoint efrom.val (eto.val - efrom.val) (regionStartNat g cs.pos.region.val) g.region.val)
      (hstep : NorFlash.NorFlashStep (toFlashConfig g) (.erase efrom.val eto.val) cs.flash flash') :
      ComposedStep g cs { cs with flash := flash', phase := .idle }
  | writeOk {cs : ComposedState} {tr toff waddr wlen nseq : U32} {rec : CommittedRecord}
      (hph : cs.phase = .writing tr toff waddr wlen nseq rec)
      (hrec : rec.region = tr ∧ rec.off = toff ∧ rec.recordLen = wlen ∧ rec.seq = nseq ∧
              rec.bytes.length = wlen.val ∧ tr.val ≤ 1 ∧ toff.val + wlen.val ≤ g.region.val)
      (hwaddr : waddr.val = regionStartNat g tr.val + toff.val)
      (hnewer : seq_newer nseq cs.pos.seq = ok true)
      (herased : NorFlash.isErasedRange cs.flash waddr.val wlen.val)
      (hdisj : ∀ a, cs.acked = some a →
        NorFlash.disjoint waddr.val wlen.val (regionStartNat g a.region.val + a.off.val) a.recordLen.val)
      (hv : NorFlash.WriteValid (toFlashConfig g) waddr.val wlen.val) :
      ComposedStep g cs {
        cs with
        flash := NorFlash.writeBytes cs.flash waddr.val rec.bytes
        phase := .writeLanded tr toff wlen nseq rec
      }
  | writeFailPartial {cs : ComposedState} {tr toff waddr wlen nseq : U32} {rec : CommittedRecord}
      {flash' : NorFlash.FlashState}
      (hph : cs.phase = .writing tr toff waddr wlen nseq rec)
      (hblen : rec.bytes.length = wlen.val)
      (hdisj : ∀ a, cs.acked = some a →
        NorFlash.disjoint waddr.val wlen.val (regionStartNat g a.region.val + a.off.val) a.recordLen.val)
      (hstep : NorFlash.NorFlashStep (toFlashConfig g) (.write waddr.val rec.bytes) cs.flash flash') :
      ComposedStep g cs { cs with flash := flash', phase := .idle }
  | ackSave {cs : ComposedState} {tr toff wlen nseq : U32} {rec : CommittedRecord} {pos' : Pos}
      (hph : cs.phase = .writeLanded tr toff wlen nseq rec)
      (hpos' : pos_after_write tr toff wlen nseq = ok pos') :
      ComposedStep g cs { cs with pos := pos', phase := .idle, acked := some rec }
  | rebootKeepAcked {cs : ComposedState} :
      ComposedStep g cs { cs with pos := { cs.pos with append := none }, phase := .idle }
  | rebootRecoverLanded {cs : ComposedState} {tr toff wlen nseq : U32} {rec : CommittedRecord}
      (hph : cs.phase = .writeLanded tr toff wlen nseq rec) :
      ComposedStep g cs {
        cs with
        pos := { region := tr, append := none, seq := nseq }
        phase := .idle
        acked := some rec
      }

/-- Reachability closure of `ComposedStep`. -/
inductive ComposedReachable (g : FlashGeometry) : ComposedState → Prop where
  | init : ComposedReachable g composedInit
  | step {s s'} : ComposedReachable g s → ComposedStep g s s' → ComposedReachable g s'

/-- **Crash-consistency & durability theorem** for the composed extracted
`store_core` + `NorFlash` system: `CrashConsistentInv` holds in EVERY reachable
state across arbitrary saves, partial erases, partial/ambiguous writes, and
repeated reboots. -/
theorem extracted_flashstore_crash_consistent (g : FlashGeometry)
    (hlay : layout_ok g = ok true) (hdiv : AlignDivides g)
    {cs : ComposedState} (hr : ComposedReachable g cs) :
    CrashConsistentInv g cs := by
  induction hr with
  | init =>
    exact ⟨by simp [composedInit], (fun _ h => by cases h), (fun _ _ _ _ _ h => by cases h)⟩
  | @step cs0 _ _ hstep ih =>
    obtain ⟨hreg, hacked, hland⟩ := ih
    cases hstep with
    | beginSaveAppend _ _ hplan _ _ =>
      have hp := H5iAppLib.post_of_ok (plan_save_safe g _ _ hlay hdiv hreg) hplan
      refine ⟨by rw [hp.2.2.2.2.2.2.2.2]; exact hreg, (fun rec hrec => ?_), (fun _ _ _ _ _ h => by cases h)⟩
      have ⟨hi, hr, _⟩ := hacked rec hrec
      rw [hp.2.2.2.2.2.2.2.2]
      exact ⟨hi, hr, fun _ h => by cases h⟩
    | beginSaveSwitch _ _ hplan _ _ =>
      have hp := H5iAppLib.post_of_ok (plan_save_safe g _ _ hlay hdiv hreg) hplan
      refine ⟨by rw [hp.2.2.2.2.2.2.2.2.2.2.2.2.1]; exact hreg, (fun rec hrec => ?_), (fun _ _ _ _ _ h => by cases h)⟩
      have ⟨hi, hr, _⟩ := hacked rec hrec
      refine ⟨hi, by rw [hp.2.2.2.2.2.2.2.2.2.2.2.2.1]; exact hr,
        fun _ h => by rw [hp.2.2.2.2.2.2.2.2.2.2.2.2.2.1] at h; cases h⟩
    | eraseOk _ hdisj hnst =>
      have hag := NorFlash.step_erase_disjoint_preserved hnst hdisj
      refine ⟨hreg, (fun rec hrec => ?_), (fun _ _ _ _ _ h => by cases h)⟩
      have ⟨hi, hr, hbef⟩ := hacked rec hrec
      refine ⟨⟨hi.1, hi.2.1, hi.2.2.1, ?_⟩, hr, hbef⟩
      dsimp only
      have hread_eq := NorFlash.readSlice_eq_of_agreesOn
        (addr := regionStartNat g rec.region.val + rec.off.val) (len := rec.recordLen.val)
        (fun i h1 h2 => hag i (by rw [hr] at h1; omega) (by rw [hr] at h2; have := hi.2.1; omega))
      rw [hread_eq]
      exact hi.2.2.2
    | eraseFailPowerCut _ hdisj hnst =>
      have hag := NorFlash.step_erase_disjoint_preserved hnst hdisj
      refine ⟨hreg, (fun rec hrec => ?_), (fun _ _ _ _ _ h => by cases h)⟩
      have ⟨hi, hr, hbef⟩ := hacked rec hrec
      refine ⟨⟨hi.1, hi.2.1, hi.2.2.1, ?_⟩, hr, hbef⟩
      dsimp only
      have hread_eq := NorFlash.readSlice_eq_of_agreesOn
        (addr := regionStartNat g rec.region.val + rec.off.val) (len := rec.recordLen.val)
        (fun i h1 h2 => hag i (by rw [hr] at h1; omega) (by rw [hr] at h2; have := hi.2.1; omega))
      rw [hread_eq]
      exact hi.2.2.2
    | @writeOk tr toff waddr wlen nseq rec _ hrec hwaddr hnewer herased hdisj _ =>
      obtain ⟨hrr, hro, hrl, hrs, hbl, htr, htoff⟩ := hrec
      refine ⟨hreg, (fun a ha => ?_), (fun tr' toff' wlen' nseq' rec' hph' => ?_)⟩
      · have ⟨hi, hr, hbef⟩ := hacked a ha
        have hd := hdisj a ha
        rw [← hbl] at hd
        have hag := NorFlash.writeBytes_disjoint_preserved (s := cs0.flash) (bytes := rec.bytes) hd
        refine ⟨⟨hi.1, hi.2.1, hi.2.2.1, ?_⟩, hr, hbef⟩
        rw [NorFlash.readSlice_eq_of_agreesOn hag]; exact hi.2.2.2
      · cases hph'
        refine ⟨⟨by rw [hrr]; exact htr, by rw [hro, hrl]; exact htoff, by rw [hrl]; exact hbl, ?_⟩,
          by rw [hrs]; exact hnewer, hrr, hro, hrl, hrs⟩
        rw [hrr, hro, hrl, ← hwaddr]
        have hwr := NorFlash.writeBytes_in_erased (addr := waddr.val) (bytes := rec.bytes)
          (by rw [hbl]; exact herased)
        simpa [hbl] using hwr
    | writeFailPartial _ hbl hdisj hnst =>
      refine ⟨hreg, (fun a ha => ?_), (fun _ _ _ _ _ h => by cases h)⟩
      have ⟨hi, hr, hbef⟩ := hacked a ha
      have hd := hdisj a ha
      rw [← hbl] at hd
      have hag := NorFlash.step_write_disjoint_preserved hnst hd
      refine ⟨⟨hi.1, hi.2.1, hi.2.2.1, ?_⟩, hr, hbef⟩
      rw [NorFlash.readSlice_eq_of_agreesOn hag]; exact hi.2.2.2
    | @ackSave tr toff wlen nseq rec pos' hph hpos' =>
      have ⟨hintact, _, hrr, hro, hrl, _⟩ := hland _ _ _ _ _ hph
      unfold pos_after_write at hpos'
      obtain ⟨sum, hadd, hsum_val⟩ := u32_add_ok (a := toff) (b := wlen) (by
        rw [← hro, ← hrl]; have := hintact.2.1; scalar_tac)
      simp only [hadd, bind_ok] at hpos'
      rw [Result.ok.injEq] at hpos'
      subst hpos'
      refine ⟨by rw [← hrr]; exact hintact.1, (fun r hr' => ?_), (fun _ _ _ _ _ h => by cases h)⟩
      cases hr'
      exact ⟨hintact, hrr, fun off hoff => by cases hoff; rw [hsum_val, ← hro, ← hrl]⟩
    | rebootKeepAcked =>
      refine ⟨hreg, (fun a ha => ?_), (fun _ _ _ _ _ h => by cases h)⟩
      have ⟨hi, hr, _⟩ := hacked a ha
      exact ⟨hi, hr, fun _ h => by cases h⟩
    | rebootRecoverLanded hph =>
      have ⟨hintact, _, hrr, _, _, _⟩ := hland _ _ _ _ _ hph
      refine ⟨by rw [← hrr]; exact hintact.1, (fun r hr' => ?_), (fun _ _ _ _ _ h => by cases h)⟩
      cases hr'
      exact ⟨hintact, hrr, fun _ h => by cases h⟩

end h5i_app_embedded.FlashStoreProofs
