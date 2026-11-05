import H5iAppH1
import H1Spec
import H5iAppLib.Basic
/-! The extracted head parser computes `H1Spec`. -/
open Aeneas Aeneas.Std Result h5i_app_h1

namespace h5i_app_h1.Proofs

/-- `g` agrees with `buf` below `n`, and `n` is within `buf`. -/
def Reads (buf : Slice U8) (g : Nat → Nat) (n : Nat) : Prop :=
  n ≤ buf.length ∧ ∀ k (hk : k < buf.val.length), k < n → g k = (buf.val[k]'hk).val

theorem Reads.get {buf : Slice U8} {g : Nat → Nat} {n : Nat} (h : Reads buf g n) {k : Nat}
    (hk : k < n) : g k = (buf.val[k]'(by have := h.1; simp [Slice.length] at this; omega)).val :=
  h.2 k _ hk

theorem cr_val : CR.val = 13 := by unfold CR; rfl
theorem lf_val : LF.val = 10 := by unfold LF; rfl
theorem sp_val : SP.val = 32 := by unfold SP; rfl
theorem htab_val : HTAB.val = 9 := by unfold HTAB; rfl
theorem colon_val : COLON.val = 58 := by unfold COLON; rfl

@[simp] theorem bad_request_val : BAD_REQUEST.val = 400 := by unfold BAD_REQUEST; rfl
@[simp] theorem payload_too_large_val : PAYLOAD_TOO_LARGE.val = 413 := by unfold PAYLOAD_TOO_LARGE; rfl
@[simp] theorem uri_too_long_val : URI_TOO_LONG.val = 414 := by unfold URI_TOO_LONG; rfl
@[simp] theorem headers_too_large_val : HEADERS_TOO_LARGE.val = 431 := by unfold HEADERS_TOO_LARGE; rfl
@[simp] theorem not_implemented_val : NOT_IMPLEMENTED.val = 501 := by unfold NOT_IMPLEMENTED; rfl
@[simp] theorem version_not_supported_val : VERSION_NOT_SUPPORTED.val = 505 := by
  unfold VERSION_NOT_SUPPORTED; rfl
@[simp] theorem max_headers_val : MAX_HEADERS.val = 16 := by unfold MAX_HEADERS; rfl

theorem usize_bne_true (x y : Usize) : ((x != y) = true) ↔ x.val ≠ y.val := by
  simp [bne_iff_ne]

theorem u16_ne_iff (a b : U16) : a ≠ b ↔ a.val ≠ b.val := by
  constructor
  · intro h h'; exact h (by scalar_tac)
  · rintro h rfl; exact h rfl

theorem u8_eq_iff' (a b : U8) : a = b ↔ a.val = b.val := by
  constructor
  · rintro rfl; rfl
  · intro h; scalar_tac

/-! ## Byte classes -/

/-- Close a goal of Booleans and byte comparisons by arithmetic. -/
macro "bool_tac" : tactic => `(tactic| (rw [Bool.eq_iff_iff]; simp only [bne_iff_ne, beq_iff_eq,
  Bool.or_eq_true, Bool.and_eq_true, ne_eq, u8_eq_iff', UScalar.ofNatCore_val_eq, decide_eq_true_eq]; omega))

theorem is_tchar_eq (b : U8) : is_tchar b = ok (Spec.isTchar b.val) := by
  simp only [is_tchar, Spec.isTchar, UScalar.lt_equiv, bne_iff_ne, ne_eq, u8_eq_iff',
    UScalar.ofNatCore_val_eq]
  split_ifs <;> simp_all <;> bool_tac

@[step]
theorem is_tchar_spec (b : U8) : is_tchar b ⦃ r => r = Spec.isTchar b.val ⦄ := by
  rw [is_tchar_eq]; simp

theorem is_field_byte_eq (b : U8) : is_field_byte b = ok (Spec.isFieldByte b.val) := by
  simp only [is_field_byte, Spec.isFieldByte, UScalar.le_equiv, u8_eq_iff', htab_val, sp_val]
  split_ifs <;> simp_all <;> bool_tac

@[step]
theorem is_field_byte_spec (b : U8) : is_field_byte b ⦃ r => r = Spec.isFieldByte b.val ⦄ := by
  rw [is_field_byte_eq]; simp

@[step]
theorem is_target_byte_spec (b : U8) : is_target_byte b ⦃ r => r = Spec.isTargetByte b.val ⦄ := by
  simp only [is_target_byte, Spec.isTargetByte, UScalar.le_equiv, UScalar.ofNatCore_val_eq]
  split_ifs <;> simp_all <;> bool_tac

@[step]
theorem is_ows_spec (b : U8) : is_ows b ⦃ r => r = Spec.isOws b.val ⦄ := by
  simp only [is_ows, Spec.isOws, u8_eq_iff', sp_val, htab_val]
  split_ifs <;> simp_all <;> bool_tac

@[step]
theorem lower_spec (b : U8) : lower b ⦃ r => r.val = Spec.lower b.val ⦄ := by
  unfold lower Spec.lower
  step*
  all_goals (split_ifs <;> scalar_tac)

/-! ## Scanning loops -/


/-- The byte the Rust read at `k` is `g k`. -/
theorem byte_eq {buf : Slice U8} {g : Nat → Nat} {n : Nat} (hR : Reads buf g n) {k : Usize} {x : U8}
    (hk : k.val < n) (hx : x = buf.val[k.val]'(by have := hR.1; simp [Slice.length] at this; omega)) :
    g k.val = x.val := by
  rw [hR.get hk, hx]

theorem token_end_loop_eq (buf : Slice U8) (e k : Usize) (g : Nat → Nat) (n : Nat)
    (hR : Reads buf g n) (he : e.val ≤ n) :
    token_end_loop buf e k ⦃ r => r.val = Spec.scan Spec.isTchar g k.val e.val ⦄ := by
  unfold token_end_loop
  have hl := hR.1
  apply loop.spec_decr_nat (measure := fun (j : Usize) => e.val - j.val)
    (inv := fun j => Spec.scan Spec.isTchar g k.val e.val = Spec.scan Spec.isTchar g j.val e.val)
  · intro j hj
    unfold token_end_loop.body
    step*
    · have hb := byte_eq hR (k := j) (by scalar_tac) i_post
      refine ⟨?_, by scalar_tac⟩
      rw [hj, Spec.scan_cont (by scalar_tac) (by rw [hb, ← b_post]; simp_all), k1_post]
    · have hb := byte_eq hR (k := j) (by scalar_tac) i_post
      rw [hj, Spec.scan_stop (by scalar_tac) (by rw [hb, ← b_post]; simp_all)]
    · rw [hj, Spec.scan_end (by scalar_tac)]
  · rfl

@[step]
theorem token_end_loop_spec (buf : Slice U8) (e k : Usize) (g : Nat → Nat) (n : Nat)
    (hR : Reads buf g n) (he : e.val ≤ n) :
    token_end_loop buf e k ⦃ r => r.val = Spec.scan Spec.isTchar g k.val e.val ∧ k.val ≤ r.val ∧
      (k.val ≤ e.val → r.val ≤ e.val) ⦄ :=
  WP.spec_mono (token_end_loop_eq buf e k g n hR he)
    (fun r hr => by rw [hr]; exact ⟨rfl, Spec.scan_ge _ _ _ _, Spec.scan_le _ _ _ _⟩)

theorem target_end_loop_eq (buf : Slice U8) (e k : Usize) (g : Nat → Nat) (n : Nat)
    (hR : Reads buf g n) (he : e.val ≤ n) :
    target_end_loop buf e k ⦃ r => r.val = Spec.scan Spec.isTargetByte g k.val e.val ⦄ := by
  unfold target_end_loop
  have hl := hR.1
  apply loop.spec_decr_nat (measure := fun (j : Usize) => e.val - j.val)
    (inv := fun j => Spec.scan Spec.isTargetByte g k.val e.val = Spec.scan Spec.isTargetByte g j.val e.val)
  · intro j hj
    unfold target_end_loop.body
    step*
    · have hb := byte_eq hR (k := j) (by scalar_tac) i_post
      refine ⟨?_, by scalar_tac⟩
      rw [hj, Spec.scan_cont (by scalar_tac) (by rw [hb, ← b_post]; simp_all), k1_post]
    · have hb := byte_eq hR (k := j) (by scalar_tac) i_post
      rw [hj, Spec.scan_stop (by scalar_tac) (by rw [hb, ← b_post]; simp_all)]
    · rw [hj, Spec.scan_end (by scalar_tac)]
  · rfl

@[step]
theorem target_end_loop_spec (buf : Slice U8) (e k : Usize) (g : Nat → Nat) (n : Nat)
    (hR : Reads buf g n) (he : e.val ≤ n) :
    target_end_loop buf e k ⦃ r => r.val = Spec.scan Spec.isTargetByte g k.val e.val ∧ k.val ≤ r.val ∧
      (k.val ≤ e.val → r.val ≤ e.val) ⦄ :=
  WP.spec_mono (target_end_loop_eq buf e k g n hR he)
    (fun r hr => by rw [hr]; exact ⟨rfl, Spec.scan_ge _ _ _ _, Spec.scan_le _ _ _ _⟩)

theorem skip_ows_loop_eq (buf : Slice U8) (e k : Usize) (g : Nat → Nat) (n : Nat)
    (hR : Reads buf g n) (he : e.val ≤ n) :
    skip_ows_loop buf e k ⦃ r => r.val = Spec.scan Spec.isOws g k.val e.val ⦄ := by
  unfold skip_ows_loop
  have hl := hR.1
  apply loop.spec_decr_nat (measure := fun (j : Usize) => e.val - j.val)
    (inv := fun j => Spec.scan Spec.isOws g k.val e.val = Spec.scan Spec.isOws g j.val e.val)
  · intro j hj
    unfold skip_ows_loop.body
    step*
    · have hb := byte_eq hR (k := j) (by scalar_tac) i_post
      refine ⟨?_, by scalar_tac⟩
      rw [hj, Spec.scan_cont (by scalar_tac) (by rw [hb, ← b_post]; simp_all), p1_post]
    · have hb := byte_eq hR (k := j) (by scalar_tac) i_post
      rw [hj, Spec.scan_stop (by scalar_tac) (by rw [hb, ← b_post]; simp_all)]
    · rw [hj, Spec.scan_end (by scalar_tac)]
  · rfl

@[step]
theorem skip_ows_loop_spec (buf : Slice U8) (e k : Usize) (g : Nat → Nat) (n : Nat)
    (hR : Reads buf g n) (he : e.val ≤ n) :
    skip_ows_loop buf e k ⦃ r => r.val = Spec.scan Spec.isOws g k.val e.val ∧ k.val ≤ r.val ∧
      (k.val ≤ e.val → r.val ≤ e.val) ⦄ :=
  WP.spec_mono (skip_ows_loop_eq buf e k g n hR he)
    (fun r hr => by rw [hr]; exact ⟨rfl, Spec.scan_ge _ _ _ _, Spec.scan_le _ _ _ _⟩)

theorem line_end_loop_eq (buf : Slice U8) (e k : Usize) (g : Nat → Nat) (n : Nat)
    (hR : Reads buf g n) (he : e.val ≤ n) :
    line_end_loop buf e k ⦃ r => r.val = Spec.scan Spec.notEol g k.val e.val ⦄ := by
  unfold line_end_loop
  have hl := hR.1
  apply loop.spec_decr_nat (measure := fun (j : Usize) => e.val - j.val)
    (inv := fun j => Spec.scan Spec.notEol g k.val e.val = Spec.scan Spec.notEol g j.val e.val)
  · intro j hj
    unfold line_end_loop.body
    step*
    · have hb := byte_eq hR (k := j) (by scalar_tac) b_post
      rw [hj, Spec.scan_stop (by scalar_tac)]
      simp only [Spec.notEol, hb]
      simp_all [u8_eq_iff', cr_val, lf_val]
    · have hb := byte_eq hR (k := j) (by scalar_tac) b_post
      rw [hj, Spec.scan_stop (by scalar_tac)]
      simp only [Spec.notEol, hb]
      simp_all [u8_eq_iff', cr_val, lf_val]
    · have hb := byte_eq hR (k := j) (by scalar_tac) b_post
      refine ⟨?_, by scalar_tac⟩
      rw [hj, Spec.scan_cont (by scalar_tac), j1_post]
      simp only [Spec.notEol, hb]
      rename_i h1 h2
      simp only [u8_eq_iff', cr_val, lf_val] at h1 h2
      simp [h1, h2]
    · rw [hj, Spec.scan_end (by scalar_tac)]
  · rfl

@[step]
theorem line_end_loop_spec (buf : Slice U8) (e k : Usize) (g : Nat → Nat) (n : Nat)
    (hR : Reads buf g n) (he : e.val ≤ n) :
    line_end_loop buf e k ⦃ r => r.val = Spec.scan Spec.notEol g k.val e.val ∧ k.val ≤ r.val ∧
      (k.val ≤ e.val → r.val ≤ e.val) ⦄ :=
  WP.spec_mono (line_end_loop_eq buf e k g n hR he)
    (fun r hr => by rw [hr]; exact ⟨rfl, Spec.scan_ge _ _ _ _, Spec.scan_le _ _ _ _⟩)

@[step]
theorem field_bytes_loop_spec (buf : Slice U8) (e k : Usize) (g : Nat → Nat) (n : Nat)
    (hR : Reads buf g n) (he : e.val ≤ n) :
    field_bytes_loop buf e k ⦃ r => r = Spec.allIn Spec.isFieldByte g k.val e.val ⦄ := by
  unfold field_bytes_loop
  have hl := hR.1
  apply loop.spec_decr_nat (measure := fun (j : Usize) => e.val - j.val)
    (inv := fun j => Spec.allIn Spec.isFieldByte g k.val e.val = Spec.allIn Spec.isFieldByte g j.val e.val)
  · intro j hj
    unfold field_bytes_loop.body
    step*
    · have hb := byte_eq hR (k := j) (by scalar_tac) i_post
      refine ⟨?_, by scalar_tac⟩
      rw [hj, Spec.allIn_cont (by scalar_tac) (by rw [hb, ← b_post]; simp_all), p1_post]
    · have hb := byte_eq hR (k := j) (by scalar_tac) i_post
      rw [hj, Spec.allIn_stop (by scalar_tac) (by rw [hb, ← b_post]; simp_all)]
    · rw [hj, Spec.allIn_end (by scalar_tac)]
  · rfl

theorem trimEnd_cont {g : Nat → Nat} {s p : Nat} (h : s < p) (hp : Spec.isOws (g (p - 1)) = true) :
    Spec.trimEnd g s p = Spec.trimEnd g s (p - 1) := by
  rw [Spec.trimEnd, if_pos h, if_pos hp]

theorem trimEnd_stop {g : Nat → Nat} {s p : Nat} (h : s < p) (hp : Spec.isOws (g (p - 1)) = false) :
    Spec.trimEnd g s p = p := by
  rw [Spec.trimEnd, if_pos h, if_neg (by simp [hp])]

theorem trimEnd_end {g : Nat → Nat} {s p : Nat} (h : p ≤ s) : Spec.trimEnd g s p = p := by
  rw [Spec.trimEnd, if_neg (by omega)]

@[step]
theorem trim_ows_end_loop_spec (buf : Slice U8) (s e : Usize) (g : Nat → Nat) (n : Nat)
    (hR : Reads buf g n) (he : e.val ≤ n) :
    trim_ows_end_loop buf s e ⦃ r => r.val = Spec.trimEnd g s.val e.val ⦄ := by
  unfold trim_ows_end_loop
  have hl := hR.1
  apply loop.spec_decr_nat (measure := fun (j : Usize) => j.val)
    (inv := fun j => j.val ≤ e.val ∧ Spec.trimEnd g s.val e.val = Spec.trimEnd g s.val j.val)
  · rintro j ⟨hj1, hj⟩
    unfold trim_ows_end_loop.body
    step*
    · have hb := byte_eq hR (k := i) (by scalar_tac) i1_post
      refine ⟨by scalar_tac, ?_, by scalar_tac⟩
      rw [hj, trimEnd_cont (by scalar_tac) (by rw [show j.val - 1 = i.val by scalar_tac, hb, ← b_post]; simp_all)]
      congr 1; scalar_tac
    · have hb := byte_eq hR (k := i) (by scalar_tac) i1_post
      rw [hj, trimEnd_stop (by scalar_tac) (by rw [show j.val - 1 = i.val by scalar_tac, hb, ← b_post]; simp_all)]
    · rw [hj, trimEnd_end (by scalar_tac)]
  · exact ⟨le_refl _, rfl⟩

/-- Byte values of a slice. -/
abbrev B (l : List U8) : List Nat := l.map (·.val)

theorem eqFrom_cont {g : Nat → Nat} {s : Nat} {lit : List Nat} {q : Nat} (h : q < lit.length)
    (hq : Spec.lower (g (s + q)) = lit.getD q 0) :
    Spec.eqFrom g s lit q = Spec.eqFrom g s lit (q + 1) := by
  rw [Spec.eqFrom, if_pos h, if_neg (by simp [hq])]

theorem eqFrom_stop {g : Nat → Nat} {s : Nat} {lit : List Nat} {q : Nat} (h : q < lit.length)
    (hq : Spec.lower (g (s + q)) ≠ lit.getD q 0) :
    Spec.eqFrom g s lit q = false := by
  rw [Spec.eqFrom, if_pos h, if_pos hq]

theorem eqFrom_end {g : Nat → Nat} {s : Nat} {lit : List Nat} {q : Nat} (h : lit.length ≤ q) :
    Spec.eqFrom g s lit q = true := by
  rw [Spec.eqFrom, if_neg (by omega)]

@[step]
theorem eq_ci_loop_spec (buf : Slice U8) (s : Usize) (lit : Slice U8) (q : Usize) (g : Nat → Nat) (n : Nat)
    (hR : Reads buf g n) (he : s.val + lit.length ≤ n) :
    eq_ci_loop buf s lit q ⦃ r => r = Spec.eqFrom g s.val (B lit.val) q.val ⦄ := by
  unfold eq_ci_loop
  have hl := hR.1
  apply loop.spec_decr_nat (measure := fun (j : Usize) => lit.length - j.val)
    (inv := fun j => Spec.eqFrom g s.val (B lit.val) q.val = Spec.eqFrom g s.val (B lit.val) j.val)
  · intro j hj
    unfold eq_ci_loop.body
    step*
    · have hb := byte_eq hR (k := i1) (by scalar_tac) i2_post
      rw [hj, eqFrom_stop (by simp; scalar_tac)]
      rw [i1_post] at hb
      rw [hb, ← i3_post]
      simp only [B, List.getD_eq_getElem?_getD, List.getElem?_map]
      rw [List.getElem?_eq_getElem (by scalar_tac)]
      simp only [Option.map_some, Option.getD_some]
      simp_all [u8_eq_iff']
    · have hb := byte_eq hR (k := i1) (by scalar_tac) i2_post
      refine ⟨?_, by scalar_tac⟩
      rw [hj, eqFrom_cont (by simp; scalar_tac), q1_post]
      rw [i1_post] at hb
      rw [hb, ← i3_post]
      simp only [B, List.getD_eq_getElem?_getD, List.getElem?_map]
      rw [List.getElem?_eq_getElem (by scalar_tac)]
      simp only [Option.map_some, Option.getD_some]
      simp_all
    · rw [hj, eqFrom_end (by simp; scalar_tac)]
  · rfl

@[step]
theorem eq_ci_spec (buf : Slice U8) (s e : Usize) (lit : Slice U8) (g : Nat → Nat) (n : Nat)
    (hR : Reads buf g n) (he : e.val ≤ n) :
    eq_ci buf s e lit ⦃ r => r = Spec.eqCi g s.val e.val (B lit.val) ⦄ := by
  unfold eq_ci Spec.eqCi
  step*
  all_goals first
    | (rw [if_pos (by left; scalar_tac)])
    | (rw [if_pos (by right; simp; scalar_tac)])
    | (rw [if_neg (by simp; scalar_tac)])

theorem clLoop_bad {g : Nat → Nat} {e max acc p : Nat} (h : p < e) (hc : g p < 48 ∨ 57 < g p) :
    Spec.clLoop g e max acc p = .bad 400 := by
  rw [Spec.clLoop, if_pos h, if_pos hc]

theorem clLoop_big {g : Nat → Nat} {e max acc p : Nat} (h : p < e) (hc : ¬(g p < 48 ∨ 57 < g p))
    (hm : max < acc * 10 + (g p - 48)) : Spec.clLoop g e max acc p = .bad 413 := by
  rw [Spec.clLoop, if_pos h, if_neg hc, if_pos hm]

theorem clLoop_cont {g : Nat → Nat} {e max acc p : Nat} (h : p < e) (hc : ¬(g p < 48 ∨ 57 < g p))
    (hm : ¬ max < acc * 10 + (g p - 48)) :
    Spec.clLoop g e max acc p = Spec.clLoop g e max (acc * 10 + (g p - 48)) (p + 1) := by
  rw [Spec.clLoop, if_pos h, if_neg hc, if_neg hm]

theorem clLoop_end {g : Nat → Nat} {e max acc p : Nat} (h : e ≤ p) : Spec.clLoop g e max acc p = .ok acc := by
  rw [Spec.clLoop, if_neg (by omega)]

def numView : Num → Spec.Num
  | .Bad s => .bad s.val
  | .Ok v => .ok v.val

@[step]
theorem content_length_loop_spec (buf : Slice U8) (e : Usize) (max acc : U64) (p : Usize) (g : Nat → Nat)
    (n : Nat) (hR : Reads buf g n) (he : e.val ≤ n) (hmax : max.val ≤ 4294967295) (hacc : acc.val ≤ max.val) :
    content_length_loop buf e max acc p ⦃ r => numView r = Spec.clLoop g e.val max.val acc.val p.val ⦄ := by
  unfold content_length_loop
  have hl := hR.1
  apply loop.spec_decr_nat (measure := fun (x : U64 × Usize) => e.val - x.2.val)
    (inv := fun x => x.1.val ≤ max.val ∧
      Spec.clLoop g e.val max.val acc.val p.val = Spec.clLoop g e.val max.val x.1.val x.2.val)
  · rintro ⟨a, j⟩ ⟨h1, h2⟩
    simp only at h1 h2 ⊢
    unfold content_length_loop.body
    step*
    all_goals (try have hb := byte_eq hR (k := j) (by scalar_tac) c_post)
    · rw [h2, clLoop_bad (by scalar_tac) (by left; scalar_tac)]; simp [numView]
    · rw [h2, clLoop_bad (by scalar_tac) (by right; scalar_tac)]; simp [numView]
    · rw [h2, clLoop_big (by scalar_tac) (by scalar_tac)]
      · simp [numView]
      · have : i2.val = c.val - 48 := by rw [i2_post]; simp; scalar_tac
        scalar_tac
    · refine ⟨by scalar_tac, ?_, by scalar_tac⟩
      have : i2.val = c.val - 48 := by rw [i2_post]; simp; scalar_tac
      rw [h2, clLoop_cont (by scalar_tac) (by scalar_tac) (by scalar_tac), p1_post]
      congr 1; scalar_tac
    · rw [h2, clLoop_end (by scalar_tac)]; rfl
  · exact ⟨hacc, rfl⟩

@[step]
theorem content_length_spec (buf : Slice U8) (s e : Usize) (max : U64) (g : Nat → Nat)
    (n : Nat) (hR : Reads buf g n) (he : e.val ≤ n) (hmax : max.val ≤ 4294967295) :
    content_length buf s e max ⦃ r => numView r = Spec.contentLength g s.val e.val max.val ⦄ := by
  unfold content_length Spec.contentLength
  step*
  rw [if_pos (by scalar_tac)]; simp [numView]

@[step]
theorem line_term_spec (buf : Slice U8) (j n : Usize) (g : Nat → Nat) (hR : Reads buf g n.val) :
    line_term buf j n ⦃ r => r.val = Spec.lineTerm g j.val n.val ⦄ := by
  unfold line_term Spec.lineTerm
  have hl := hR.1
  step*
  all_goals (try have h0 := byte_eq hR (k := j) (by scalar_tac) i_post)
  all_goals (try have h1 := byte_eq hR (k := i1) (by scalar_tac) i2_post)
  all_goals (try rw [i1_post] at h1)
  all_goals (simp only [ge_iff_le, gt_iff_lt, UScalar.le_equiv, UScalar.lt_equiv, u8_eq_iff', lf_val, bne_iff_ne, ne_eq] at *)
  all_goals (split_ifs <;> simp_all <;> scalar_tac)

theorem eqAt_cont {g : Nat → Nat} {s : Nat} {lit : List Nat} {q : Nat} (h : q < lit.length)
    (hq : g (s + q) = lit.getD q 0) :
    Spec.eqAt g s lit q = Spec.eqAt g s lit (q + 1) := by
  rw [Spec.eqAt, if_pos h, if_neg (by simp [hq])]

theorem eqAt_stop {g : Nat → Nat} {s : Nat} {lit : List Nat} {q : Nat} (h : q < lit.length)
    (hq : g (s + q) ≠ lit.getD q 0) :
    Spec.eqAt g s lit q = false := by
  rw [Spec.eqAt, if_pos h, if_pos hq]

theorem eqAt_end {g : Nat → Nat} {s : Nat} {lit : List Nat} {q : Nat} (h : lit.length ≤ q) :
    Spec.eqAt g s lit q = true := by
  rw [Spec.eqAt, if_neg (by omega)]

@[step]
theorem eq_at_loop_spec (buf : Slice U8) (s : Usize) (lit : Slice U8) (q : Usize) (g : Nat → Nat) (n : Nat)
    (hR : Reads buf g n) (he : s.val + lit.length ≤ n) :
    eq_at_loop buf s lit q ⦃ r => r = Spec.eqAt g s.val (B lit.val) q.val ⦄ := by
  unfold eq_at_loop
  have hl := hR.1
  apply loop.spec_decr_nat (measure := fun (j : Usize) => lit.length - j.val)
    (inv := fun j => Spec.eqAt g s.val (B lit.val) q.val = Spec.eqAt g s.val (B lit.val) j.val)
  · intro j hj
    unfold eq_at_loop.body
    step*
    · have hb := byte_eq hR (k := i1) (by scalar_tac) i2_post
      rw [hj, eqAt_stop (by simp; scalar_tac)]
      rw [i1_post] at hb
      rw [hb]
      simp only [B, List.getD_eq_getElem?_getD, List.getElem?_map]
      rw [List.getElem?_eq_getElem (by scalar_tac)]
      simp only [Option.map_some, Option.getD_some]
      simp_all [u8_eq_iff']
    · have hb := byte_eq hR (k := i1) (by scalar_tac) i2_post
      refine ⟨?_, by scalar_tac⟩
      rw [hj, eqAt_cont (by simp; scalar_tac), q1_post]
      rw [i1_post] at hb
      rw [hb]
      simp only [B, List.getD_eq_getElem?_getD, List.getElem?_map]
      rw [List.getElem?_eq_getElem (by scalar_tac)]
      simp only [Option.map_some, Option.getD_some]
      simp_all
    · rw [hj, eqAt_end (by simp; scalar_tac)]
  · rfl

theorem lit_http : B (Array.to_slice (Array.make 5#usize [72#u8, 84#u8, 84#u8, 80#u8, 47#u8])).val =
    Spec.litHttp := by rfl

theorem not_bne_val {x y : Usize} (h : ¬ (x != y) = true) : x.val = y.val := by
  simp at h; rw [h]

@[step]
theorem version_digits_spec (a d b : U8) :
    version_digits a d b ⦃ r => r.val = Spec.versionDigits a.val d.val b.val ⦄ := by
  simp only [version_digits, Spec.versionDigits, UScalar.lt_equiv, gt_iff_lt, bne_iff_ne, ne_eq, u8_eq_iff',
    UScalar.ofNatCore_val_eq]
  split_ifs <;> simp_all <;> omega

@[step]
theorem version_spec (buf : Slice U8) (s e : Usize) (g : Nat → Nat) (n : Nat)
    (hR : Reads buf g n) (he : e.val ≤ n) :
    version buf s e ⦃ r => r.val = Spec.version g s.val e.val ⦄ := by
  unfold version Spec.version
  have hl := hR.1
  step*
  all_goals (try subst s1_post)
  all_goals (try rw [lit_http] at b_post)
  · have h8 : i.val = 8 := by rw [‹i = 8#usize›]; rfl
    rw [if_neg (by omega), if_neg (by simp_all)]
    have ha := byte_eq hR (k := i1) (by omega) i2_post
    have hd := byte_eq hR (k := i3) (by omega) i4_post
    have hb := byte_eq hR (k := i5) (by omega) i6_post
    rw [i1_post] at ha; rw [i3_post] at hd; rw [i5_post] at hb
    rw [ha, hd, hb, r_post]
  · have h8 : i.val = 8 := by rw [‹i = 8#usize›]; rfl
    rw [if_neg (by omega), if_pos (by simp_all)]; rfl
  all_goals (rw [if_pos (by scalar_tac)])

def rlView : ReqLine → Spec.ReqLine
  | .Bad s => .bad s.val
  | .Ok m t v => .ok m.start.val m.end.val t.start.val t.end.val v.val

theorem byte_eq' {buf : Slice U8} {g : Nat → Nat} {n : Nat} (hR : Reads buf g n) {k : Nat} {x : U8}
    {pf : k < buf.val.length} (hx : x = buf.val[k]'pf) (hk : k < n) : g k = x.val := by
  rw [hR.get hk, hx]

theorem lt_of_not_ge' {x y : Usize} (h : ¬ x ≥ y) : x.val < y.val := by
  simp only [ge_iff_le, UScalar.le_equiv, not_le] at h; exact h

theorem usize_lt_val {x y : Usize} (h : x < y) : x.val < y.val := by
  simpa only [UScalar.lt_equiv] using h

theorem usize_not_lt_val {x y : Usize} (h : ¬ x < y) : y.val ≤ x.val := by
  simp only [UScalar.lt_equiv, not_lt] at h; exact h

theorem usize_ne_val {x y : Usize} (h : ¬ x = y) : x.val ≠ y.val := fun h' => h (by scalar_tac)

@[step]
theorem request_line_spec (buf : Slice U8) (i j : Usize) (g : Nat → Nat) (n : Nat)
    (hR : Reads buf g n) (hj : j.val ≤ n) :
    request_line buf i j ⦃ r => rlView r = Spec.requestLine g i.val j.val ⦄ := by
  unfold request_line
  have hl := hR.1
  step*
  all_goals (try have hmj : m.val < j.val := (lt_of_not_ge' ‹¬m ≥ j›))
  all_goals (try have htj : te.val < j.val := (lt_of_not_ge' ‹¬te ≥ j›))
  all_goals (try have hts : ts.val ≤ te.val := (by have := Spec.scan_ge Spec.isTargetByte g ts.val j.val; omega))
  all_goals (try have h1 := byte_eq hR (k := m) (by omega) i1_post)
  all_goals (try have h2 := byte_eq hR (k := ts) (by omega) i2_post)
  all_goals (try have h3 := byte_eq hR (k := te) (by omega) i3_post)
  all_goals (try rw [ts_post] at h2)
  all_goals (try rw [ts_post] at te_post)
  all_goals (unfold Spec.requestLine; simp only)
  all_goals (rw [← m_post])
  all_goals (try rw [← te_post])
  all_goals (try rw [h1])
  all_goals (try rw [h2])
  all_goals (try rw [h3])
  all_goals (try rw [← i4_post])
  all_goals (try rw [← v_post])
  all_goals (try simp only [bne_iff_ne, ne_eq, not_not, u8_eq_iff', UScalar.eq_equiv, sp_val,
    UScalar.ofNatCore_val_eq, ge_iff_le, UScalar.le_equiv, not_le] at *)
  all_goals (simp only [rlView, bad_request_val, version_not_supported_val])
  all_goals (split_ifs <;> first | rfl | omega | (exfalso; omega) | (rw [ts_post]))

def hlView : HdrLine → Spec.HdrLine
  | .Bad s => .bad s.val
  | .Ok k vs ve => .ok k.val vs.val ve.val

@[step]
theorem header_line_spec (buf : Slice U8) (i j : Usize) (g : Nat → Nat) (n : Nat)
    (hR : Reads buf g n) (hj : j.val ≤ n) :
    header_line buf i j ⦃ r => hlView r = Spec.headerLine g i.val j.val ∧
      (∀ k vs ve, r = .Ok k vs ve → i.val ≤ k.val ∧ k.val < vs.val + 1 ∧ vs.val ≤ ve.val ∧ ve.val ≤ j.val) ⦄ := by
  unfold header_line
  have hl := hR.1
  step*
  all_goals (try have hkj : k.val < j.val := (lt_of_not_ge' ‹¬k ≥ j›))
  all_goals (try have h1 := byte_eq hR (k := k) (by omega) i1_post)
  all_goals (unfold Spec.headerLine; simp only)
  all_goals (rw [← k_post])
  all_goals (try rw [h1])
  all_goals (try rw [← i2_post])
  all_goals (try rw [← b_post])
  all_goals (try rw [← vs_post])
  all_goals (try rw [← ve_post])
  all_goals (try simp only [bne_iff_ne, ne_eq, not_not, u8_eq_iff', UScalar.eq_equiv, colon_val,
    UScalar.ofNatCore_val_eq] at *)
  all_goals (simp only [hlView, bad_request_val])
  all_goals (try have hve1 := Spec.trimEnd_ge g vs.val j.val (by omega))
  all_goals (try have hve2 := Spec.trimEnd_le g vs.val j.val)
  all_goals (split_ifs <;> (first
    | (refine ⟨rfl, ?_⟩; intro _ _ _ h; cases h <;> omega)
    | (exfalso; omega)
    | (exfalso; simp_all; done)))

theorem lit_host : B (Array.to_slice (Array.make 4#usize [104#u8, 111#u8, 115#u8, 116#u8])).val =
    Spec.litHost := by rfl

theorem lit_cl : B (Array.to_slice (Array.make 14#usize [99#u8, 111#u8, 110#u8, 116#u8, 101#u8, 110#u8, 116#u8,
    45#u8, 108#u8, 101#u8, 110#u8, 103#u8, 116#u8, 104#u8])).val = Spec.litContentLength := by rfl

theorem lit_te : B (Array.to_slice (Array.make 17#usize [116#u8, 114#u8, 97#u8, 110#u8, 115#u8, 102#u8, 101#u8,
    114#u8, 45#u8, 101#u8, 110#u8, 99#u8, 111#u8, 100#u8, 105#u8, 110#u8, 103#u8])).val =
    Spec.litTransferEncoding := by rfl

theorem lit_chunked : B (Array.to_slice (Array.make 7#usize [99#u8, 104#u8, 117#u8, 110#u8, 107#u8, 101#u8,
    100#u8])).val = Spec.litChunked := by rfl

@[simp] theorem kind_host_val : KIND_HOST.val = 1 := by unfold KIND_HOST; rfl
@[simp] theorem kind_cl_val : KIND_CONTENT_LENGTH.val = 2 := by unfold KIND_CONTENT_LENGTH; rfl
@[simp] theorem kind_te_val : KIND_TRANSFER_ENCODING.val = 3 := by unfold KIND_TRANSFER_ENCODING; rfl
@[simp] theorem kind_other_val : KIND_OTHER.val = 0 := by unfold KIND_OTHER; rfl

@[step]
theorem classify_spec (buf : Slice U8) (s e : Usize) (g : Nat → Nat) (n : Nat)
    (hR : Reads buf g n) (he : e.val ≤ n) :
    classify buf s e ⦃ r => r.val = Spec.classify g s.val e.val ⦄ := by
  unfold classify Spec.classify
  step*
  all_goals (try subst s1_post)
  all_goals (try subst s2_post)
  all_goals (try subst s3_post)
  all_goals (try rw [lit_host] at b_post)
  all_goals (try rw [lit_cl] at b1_post)
  all_goals (try rw [lit_te] at b2_post)
  all_goals (simp_all)

def hdrView (h : Header) : Spec.Field :=
  ⟨h.name.start.val, h.name.end.val, h.value.start.val, h.value.end.val⟩

/-- The first `count` headers, as the spec sees them. -/
def hdrsView (a : Std.Array Header 16#usize) (c : Usize) : List Spec.Field :=
  (a.val.take c.val).map hdrView

def fieldsView (f : Fields) : Spec.Fields :=
  ⟨hdrsView f.headers f.count, f.host, f.has_cl, f.cl.val, f.te⟩

def addedView : Added → Spec.Added
  | .Bad s => .bad s.val
  | .Ok f => .ok (fieldsView f)

theorem take_set_succ {α} (l : List α) (c : Nat) (x : α) (h : c < l.length) :
    (l.set c x).take (c + 1) = l.take c ++ [x] := by
  induction l generalizing c with
  | nil => simp at h
  | cons y l ih =>
    cases c with
    | zero => simp
    | succ c => simp [List.take_succ_cons, ih c (by simp at h; omega)]

theorem hdrsView_push (a : Std.Array Header 16#usize) (c c' : Usize) (x : Header) (hc : c.val < 16)
    (hc' : c'.val = c.val + 1) :
    hdrsView (a.set c x) c' = hdrsView a c ++ [hdrView x] := by
  unfold hdrsView
  rw [hc', Std.Array.set_val_eq, take_set_succ _ _ _ (by simp; omega)]
  simp

theorem hdrsView_length (a : Std.Array Header 16#usize) (c : Usize) (hc : c.val ≤ 16) :
    (hdrsView a c).length = c.val := by
  unfold hdrsView; simp; omega

@[step]
theorem add_header_spec (buf : Slice U8) (f : Fields) (ns ne vs ve le : Usize) (minor : U8) (mb : U64)
    (g : Nat → Nat) (n : Nat) (hR : Reads buf g n) (hne : ne.val ≤ n) (hve : ve.val ≤ n)
    (hmb : mb.val ≤ 4294967295) (hc : f.count.val ≤ 16) :
    add_header buf f ns ne vs ve le minor mb ⦃ r =>
      addedView r = Spec.addHeader g (fieldsView f) ns.val ne.val vs.val ve.val le.val minor.val mb.val ∧
      (∀ f', r = .Ok f' → f'.count.val ≤ 16) ⦄ := by
  unfold add_header
  have hlen := hdrsView_length f.headers f.count hc
  rw [show MAX_HEADERS = 16#usize by unfold MAX_HEADERS; rfl,
    show KIND_HOST = 1#u8 by unfold KIND_HOST; rfl,
    show KIND_CONTENT_LENGTH = 2#u8 by unfold KIND_CONTENT_LENGTH; rfl,
    show KIND_TRANSFER_ENCODING = 3#u8 by unfold KIND_TRANSFER_ENCODING; rfl]
  step*
  all_goals (try subst s_post)
  all_goals (try rw [lit_chunked] at b_post)
  all_goals (unfold Spec.addHeader; simp only [fieldsView, hlen, Spec.maxHeaders])
  all_goals (try simp only [← kind_post])
  all_goals (try rw [a_post, hdrsView_push _ _ _ _ (by scalar_tac) i_post])
  all_goals (try simp only [← n_post])
  all_goals (try simp only [← b_post])
  all_goals (simp only [UScalar.eq_equiv, UScalar.ofNatCore_val_eq, ge_iff_le, UScalar.le_equiv, not_le] at *)
  all_goals (split_ifs <;> simp_all [addedView, fieldsView, numView, hdrView] <;> (try omega) <;>
    (try rw [hdrsView_push _ _ _ _ (by omega) i_post]) <;> (try rw [← n_post]) <;> (try simp [hdrView]) <;>
    (try omega))

def fieldedView : Fielded → Spec.Fielded
  | .More => .more
  | .Bad s => .bad s.val
  | .Done f len => .done (fieldsView f) len.val

theorem lineTerm_zero {g : Nat → Nat} {j n : Nat} (h : Spec.lineTerm g j n = 0) : j + 1 < n := by
  unfold Spec.lineTerm at h; split_ifs at h <;> omega

theorem fields_unfold (g : Nat → Nat) (n minor mb i : Nat) (f : Spec.Fields) (h : i < n) :
    Spec.fields g n minor mb i f =
      (let j := Spec.lineEnd g i n
       let t := Spec.lineTerm g j n
       if t = 1 then .more
       else if t = 2 then .bad 400
       else if j = i then .done f (j + 2)
       else
         match Spec.headerLine g i j with
         | .bad s => .bad s
         | .ok k vs ve =>
           match Spec.addHeader g f i k vs ve j minor mb with
           | .bad s => .bad s
           | .ok f' => Spec.fields g n minor mb (j + 2) f') := by
  rw [Spec.fields, if_pos h]; rfl

theorem lineTerm_cases (g : Nat → Nat) (j n : Nat) :
    Spec.lineTerm g j n = 0 ∨ Spec.lineTerm g j n = 1 ∨ Spec.lineTerm g j n = 2 := by
  unfold Spec.lineTerm; split_ifs <;> simp

theorem fields_end (g : Nat → Nat) (n minor mb i : Nat) (f : Spec.Fields) (h : n ≤ i) :
    Spec.fields g n minor mb i f = .more := by
  rw [Spec.fields, if_neg (by omega)]

@[step]
theorem fields_loop_spec (buf : Slice U8) (n : Usize) (minor : U8) (mb : U64) (i : Usize) (f : Fields)
    (gg : Nat → Nat) (hR : Reads buf gg n.val) (hmb : mb.val ≤ 4294967295) (hc : f.count.val ≤ 16) :
    fields_loop buf n minor mb i f ⦃ r =>
      fieldedView r = Spec.fields gg n.val minor.val mb.val i.val (fieldsView f) ∧
      (∀ f' len, r = .Done f' len → f'.count.val ≤ 16) ⦄ := by
  unfold fields_loop
  have hl := hR.1
  apply loop.spec_decr_nat (measure := fun (x : Usize × Fields) => n.val - x.1.val)
    (inv := fun x => x.2.count.val ≤ 16 ∧
      Spec.fields gg n.val minor.val mb.val i.val (fieldsView f) =
        Spec.fields gg n.val minor.val mb.val x.1.val (fieldsView x.2))
  · rintro ⟨i', f'⟩ ⟨h1, h2⟩
    simp only at h1 h2 ⊢
    unfold fields_loop.body
    have hlt : ∀ (j : Usize) (t : U8), t.val = Spec.lineTerm gg j.val n.val → t.val ≠ 1 → t.val ≠ 2 →
        j.val + 1 < n.val := by
      intro j t ht h1 h2
      apply lineTerm_zero (g := gg)
      rcases lineTerm_cases gg j.val n.val with h | h | h <;> omega
    have hp := Slice.length_ineq buf
    have hl' : n.val ≤ buf.val.length := hl
    step*
    all_goals (try simp only [UScalar.eq_equiv, UScalar.ofNatCore_val_eq] at *)
    all_goals (try have hj1 := hlt j t t_post (by assumption) (by assumption))
    all_goals (first
      | (rw [h2, fields_unfold gg n.val minor.val mb.val i'.val (fieldsView f') (usize_lt_val ‹i' < n›)])
      | (rw [h2, fields_end gg n.val minor.val mb.val i'.val (fieldsView f') (usize_not_lt_val ‹¬ i' < n›)]))
    all_goals (try simp only [Spec.lineEnd])
    all_goals (try rw [← j_post])
    all_goals (try rw [← t_post])
    all_goals (try split_ifs)
    all_goals (try (simp [fieldedView, bad_request_val]; done))
    all_goals (try (simp [fieldedView]; omega))
    all_goals (try (rw [← hl_post]; subst_vars; simp only [hlView]))
    all_goals (try (rw [← a_post]; simp only [addedView]))
    all_goals (try (simp [fieldedView]; done))
    all_goals (refine ⟨a_post1 _ rfl, by rw [i1_post], by rw [i1_post]; omega⟩)
  · exact ⟨hc, rfl⟩

def framingView : Framing → Spec.Framing
  | .Empty => .empty
  | .Length n => .length n.val
  | .Chunked => .chunked

def headView (h : Head) : Spec.Head :=
  ⟨(h.method.start.val, h.method.end.val), (h.target.start.val, h.target.end.val), h.minor.val,
    hdrsView h.headers h.count, h.len.val, framingView h.framing⟩

def resultView : HeadResult → Spec.HeadR
  | .Incomplete => .incomplete
  | .Reject s => .reject s.val
  | .Done h => .done (headView h)

theorem reads_window (buf : Slice U8) (n : Nat) (hn : n ≤ buf.length) :
    Reads buf (Spec.window (B buf.val) n) n := by
  refine ⟨hn, fun k hk hkn => ?_⟩
  simp [Spec.window, hkn, B, List.getD_eq_getElem?_getD, List.getElem?_eq_getElem hk]

theorem fieldsView_empty :
    fieldsView ⟨Std.Array.repeat 16#usize NO_HEADER, 0#usize, false, false, 0#u64, false⟩ = Spec.Fields.empty := by
  simp [fieldsView, hdrsView, Spec.Fields.empty]

/-- `parse_head` computes `Spec.parseHead` of the input's bytes, for every input
and every limit: in particular it never panics. -/
theorem parse_head_spec (buf : Slice U8) (lim : Limits) :
    parse_head buf lim ⦃ r => resultView r = Spec.parseHead (B buf.val) lim.max_head.val lim.max_body.val ∧
      (∀ h, r = .Done h → h.count.val ≤ 16) ⦄ := by
  unfold parse_head
  have hlen : (B buf.val).length = buf.length := by simp
  have hp := Slice.length_ineq buf
  obtain ⟨full, n, hfn, hn, hfull⟩ : ∃ full n,
      (if buf.len ≥ lim.max_head then ok (true, lim.max_head) else ok (false, buf.len) :
        Result (Bool × Usize)) = ok (full, n) ∧
      n.val = Spec.readLimit buf.length lim.max_head.val ∧ full = decide (lim.max_head.val ≤ buf.length) := by
    by_cases hf : lim.max_head.val ≤ buf.length
    · refine ⟨true, lim.max_head, by rw [if_pos (by scalar_tac)], by simp [Spec.readLimit, hf], by simp [hf]⟩
    · refine ⟨false, buf.len, by rw [if_neg (by scalar_tac)], ?_, by simp [hf]⟩
      unfold Spec.readLimit; rw [if_neg hf]; simp
  dsimp only
  rw [hfn]
  have hnl : n.val ≤ buf.length := by rw [hn]; unfold Spec.readLimit; split <;> omega
  have hR := reads_window buf n.val hnl
  unfold Spec.parseHead
  simp only [hlen, ← hn, ← hfull]
  have hlt : ∀ (j : Usize) (t : U8), t.val = Spec.lineTerm (Spec.window (B buf.val) n.val) j.val n.val →
      ¬ t = 1#u8 → ¬ t = 2#u8 → j.val + 1 < n.val := by
    intro j t ht h1 h2
    apply lineTerm_zero (g := Spec.window (B buf.val) n.val)
    have h1' : t.val ≠ 1 := fun h => h1 (by scalar_tac)
    have h2' : t.val ≠ 2 := fun h => h2 (by scalar_tac)
    rcases lineTerm_cases (Spec.window (B buf.val) n.val) j.val n.val with h | h | h <;> omega
  step*
  all_goals (try have hj1 := hlt j t t_post (by assumption) (by assumption))
  all_goals (try (simp only [UScalar.ofNatCore_val_eq]; scalar_tac))
  all_goals (try clear hlt)
  all_goals (try simp only [Spec.lineEnd])
  all_goals (try rw [← j_post])
  all_goals (try rw [← t_post])
  all_goals (try simp only [UScalar.eq_equiv, UScalar.ofNatCore_val_eq] at *)
  all_goals (try subst hfull)
  all_goals (try rw [← rl_post])
  all_goals (try (rw [fieldsView_empty] at f_post))
  all_goals (try (rw [show i1.val = j.val + 2 by scalar_tac] at f_post))
  all_goals (try (rw [show i2.val = lim.max_body.val by rw [i2_post]; simp] at f_post))
  all_goals (try rw [← f_post])
  all_goals (try subst_vars)
  all_goals (try simp only [rlView, fieldedView])
  all_goals (try rw [← f_post])
  all_goals (try simp only [fieldedView])
  all_goals (try split_ifs)
  all_goals (try (simp [resultView, rlView, fieldedView, uri_too_long_val, bad_request_val, headers_too_large_val] at *; done))
  all_goals (try (exfalso; simp_all [fieldsView]; done))
  all_goals (try (simp_all [resultView, headView, framingView, fieldsView]; done))

/-! ## Chunked decoding -/

def csView (st : ChunkState) : Spec.CState := ⟨st.phase.val, st.size.val, st.digits.val, st.total.val⟩

def coView : ChunkOut → Spec.COut
  | .Nothing => .nothing
  | .Data => .data
  | .Done => .done
  | .Reject s => .reject s.val

@[step]
theorem hex_value_spec (c : U8) : hex_value c ⦃ v => v.val = Spec.hexv c.val ⦄ := by
  unfold hex_value Spec.hexv
  step*
  all_goals (simp only [UScalar.le_equiv, ge_iff_le] at *)
  all_goals (split_ifs <;> scalar_tac)

theorem ch_vals : CH_SIZE.val = 0 ∧ CH_SIZE_LF.val = 1 ∧ CH_DATA.val = 2 ∧ CH_DATA_CR.val = 3 ∧
    CH_DATA_LF.val = 4 ∧ CH_LAST_CR.val = 5 ∧ CH_LAST_LF.val = 6 ∧ CH_END.val = 7 ∧
    MAX_CHUNK_DIGITS.val = 8 := by
  unfold CH_SIZE CH_SIZE_LF CH_DATA CH_DATA_CR CH_DATA_LF CH_LAST_CR CH_LAST_LF CH_END MAX_CHUNK_DIGITS
  simp

@[step]
theorem chunk_size_spec (st : ChunkState) (b : U8) (mb : U32) :
    chunk_size st b mb ⦃ r => (csView r.1, coView r.2) = Spec.sizeStep (csView st) b.val mb.val ⦄ := by
  obtain ⟨h0, h1, h2, h3, h4, h5, h6, h7, h8⟩ := ch_vals
  unfold chunk_size Spec.sizeStep
  step*
  all_goals (simp only [csView, UScalar.eq_equiv, UScalar.lt_equiv, UScalar.le_equiv, ge_iff_le, gt_iff_lt,
    cr_val, h1, h8, UScalar.ofNatCore_val_eq] at *)
  all_goals (try (rw [show (UScalar.cast UScalarTy.U64 mb).val = mb.val by simp] at *))
  all_goals (split_ifs <;> first
    | (simp [coView, bad_request_val, payload_too_large_val]; done)
    | (simp [coView]; scalar_tac)
    | (exfalso; scalar_tac))

@[step]
theorem expect_spec (st : ChunkState) (b want next : U8) (o : ChunkOut) :
    expect st b want next o ⦃ r => (csView r.1, coView r.2) =
      Spec.expectStep (csView st) b.val want.val next.val (coView o) ⦄ := by
  unfold expect Spec.expectStep
  step*
  all_goals (simp only [bne_iff_ne, ne_eq, u8_eq_iff'] at *)
  all_goals (split_ifs <;> first | (simp [coView, csView]; done) | (exfalso; scalar_tac))

/-- `chunk_step` computes `Spec.chunkStep` for every state, byte and limit:
it never panics, even in states no byte sequence reaches. -/
@[step]
theorem chunk_step_spec (st : ChunkState) (b : U8) (mb : U32) :
    chunk_step st b mb ⦃ r => (csView r.1, coView r.2) = Spec.chunkStep (csView st) b.val mb.val ⦄ := by
  obtain ⟨h0, h1, h2, h3, h4, h5, h6, h7, h8⟩ := ch_vals
  unfold chunk_step chunk_size_lf chunk_data Spec.chunkStep
  step*
  all_goals (simp only [csView, UScalar.eq_equiv, UScalar.le_equiv, bne_iff_ne, ne_eq, u8_eq_iff', lf_val, cr_val,
    h0, h1, h2, h3, h4, h5, h6, h7, UScalar.ofNatCore_val_eq] at *)
  all_goals (try rw [← r_post])
  all_goals (split_ifs <;> first
    | (simp [coView, csView, bad_request_val, h0, h1, h2, h3, h4, h5, h6, h7]; done)
    | (simp [coView, csView, Spec.expectStep, cr_val, lf_val, h0, h1, h2, h3, h4, h5, h6, h7]; done)
    | (simp [coView, csView]; scalar_tac)
    | (exfalso; scalar_tac)
    | (simp_all [coView]; done))

/-! ## Response heads -/

theorem splice_step {α} (o : List α) (src : List α) (pos q : Nat) (hq : q < src.length)
    (hlen : pos + q < o.length) :
    (o.take pos ++ src.take q ++ o.drop (pos + q)).set (pos + q) (src[q]'hq) =
      o.take pos ++ src.take (q + 1) ++ o.drop (pos + (q + 1)) := by
  have hl1 : (o.take pos ++ src.take q).length = pos + q := by
    simp [List.length_take]; omega
  rw [List.set_append_right _ _ (by omega), hl1, Nat.sub_self]
  have hd : o.drop (pos + q) = o[pos + q]'(by omega) :: o.drop (pos + (q + 1)) := by
    rw [List.drop_eq_getElem_cons (by omega)]; rfl
  rw [hd, List.set_cons_zero, List.take_add_one, List.getElem?_eq_getElem hq]
  simp only [Option.toList_some, List.append_assoc, List.singleton_append]

theorem put_n_loop_spec (out : Slice U8) (pos : Usize) (src : Slice U8) (n q : Usize) (L : List U8)
    (hn : n.val ≤ src.length) (hlen : pos.val + n.val ≤ out.length) (hq : q.val ≤ n.val)
    (hL : out.val = L.take pos.val ++ src.val.take q.val ++ L.drop (pos.val + q.val))
    (hLl : L.length = out.length) :
    put_n_loop out pos src n q ⦃ o => o.length = out.length ∧
      o.val = L.take pos.val ++ src.val.take n.val ++ L.drop (pos.val + n.val) ⦄ := by
  unfold put_n_loop
  apply loop.spec_decr_nat (measure := fun (x : Slice U8 × Usize) => n.val - x.2.val)
    (inv := fun x => x.1.length = out.length ∧ x.2.val ≤ n.val ∧
      x.1.val = L.take pos.val ++ src.val.take x.2.val ++ L.drop (pos.val + x.2.val))
  · rintro ⟨o, j⟩ ⟨h1, h3, h4⟩
    simp only at h1 h3 h4 ⊢
    unfold put_n_loop.body
    step*
    · have hjs : j.val < src.val.length := by clear h4 hL; scalar_tac
      have hpl : pos.val + j.val < L.length := by
        rw [hLl]; simp only [Slice.length] at hlen ⊢; clear h4 hL; scalar_tac
      refine ⟨by rw [s_post]; simp only [Slice.length, Slice.set_val_eq, List.length_set] at h1 ⊢; exact h1,
        by clear h4 hL; scalar_tac, ?_, by clear h4 hL; scalar_tac⟩
      rw [s_post, Slice.set_val_eq, h4, i_post]
      have : i1.val = pos.val + j.val := by clear h4 hL; scalar_tac
      rw [this, q1_post]
      exact splice_step L src.val pos.val j.val hjs hpl
    all_goals (try (refine ⟨h1, ?_⟩; rw [h4, show j.val = n.val by clear h4 hL; scalar_tac]))
  · exact ⟨rfl, hq, hL⟩

theorem put_n_spec (out : Slice U8) (pos : Usize) (src : Slice U8) (n : Usize) :
    put_n out pos src n ⦃ r o => o.length = out.length ∧ match r with
      | none => src.length < n.val ∨ out.length < pos.val + n.val
      | some p => p.val = pos.val + n.val ∧ n.val ≤ src.length ∧ pos.val + n.val ≤ out.length ∧
          o.val = out.val.take pos.val ++ src.val.take n.val ++ out.val.drop (pos.val + n.val) ⦄ := by
  unfold put_n
  have hs := Slice.length_ineq src
  have ho := Slice.length_ineq out
  dsimp only
  split
  · simp only [WP.spec_ok, Slice.len_val, gt_iff_lt, UScalar.lt_equiv] at *
    refine ⟨rfl, by left; omega⟩
  split
  · simp only [WP.spec_ok, Slice.len_val, gt_iff_lt, UScalar.lt_equiv] at *
    refine ⟨rfl, by right; simp only [Slice.length] at *; omega⟩
  step
  split
  · simp only [WP.spec_ok, Slice.len_val, gt_iff_lt, UScalar.lt_equiv] at *
    refine ⟨rfl, by right; simp only [Slice.length] at *; omega⟩
  rename_i h1 h2 h3
  simp only [Slice.len_val, gt_iff_lt, UScalar.lt_equiv, not_lt] at *
  step with put_n_loop_spec out pos src n 0#usize out.val (by simp only [Slice.length] at *; omega)
    (by simp only [Slice.length] at *; omega) (by simp) (by simp) rfl
  step
  exact ⟨out1_post, by omega, by simp only [Slice.length] at *; omega, by simp only [Slice.length] at *; omega,
    out1_post1⟩

/-- `o` holds `bs` over `out`: the same length, `bs` first, then `out`'s bytes. -/
def W (o out : Slice U8) (bs : List Nat) : Prop :=
  o.length = out.length ∧ bs.length ≤ out.length ∧ B o.val = bs ++ (B out.val).drop bs.length

theorem W.refl (out : Slice U8) : W out out [] := ⟨rfl, by simp, by simp⟩

theorem W.step {o o' out : Slice U8} {bs : List Nat} {src : List U8} {n : Nat}
    (hW : W o out bs) (hn : n ≤ src.length) (hfit : bs.length + n ≤ out.length)
    (ho : o'.val = o.val.take bs.length ++ src.take n ++ o.val.drop (bs.length + n)) (hl : o'.length = o.length) :
    W o' out (bs ++ B (src.take n)) := by
  obtain ⟨h1, h2, h3⟩ := hW
  refine ⟨by rw [hl, h1], by simp [Nat.min_eq_left hn]; omega, ?_⟩
  simp only [B] at h3
  have hb : B (o.val.take bs.length) = bs := by
    simp only [B]; rw [List.map_take, h3, List.take_left' rfl]
  have hd : B (o.val.drop (bs.length + n)) = (B out.val).drop (bs.length + n) := by
    simp only [B]; rw [List.map_drop, h3, ← List.drop_drop, List.drop_left' rfl, List.drop_drop]
  rw [ho]
  simp only [B, List.map_append] at hb hd ⊢
  rw [hb, hd]
  simp only [List.length_append, List.length_map, List.length_take, List.append_assoc]
  congr 2
  rw [Nat.min_eq_left hn]

theorem put_n_W (o out : Slice U8) (bs : List Nat) (pos : Usize) (src : Slice U8) (n : Usize)
    (hW : W o out bs) (hp : pos.val = bs.length) :
    put_n o pos src n ⦃ r o' => match r with
      | none => src.length < n.val ∨ out.length < bs.length + n.val
      | some p => p.val = bs.length + n.val ∧ W o' out (bs ++ B (src.val.take n.val)) ⦄ := by
  apply WP.spec_mono (put_n_spec o pos src n)
  rintro ⟨r, o'⟩ ⟨hl, hr⟩
  have hol := hW.1
  cases r with
  | none => rw [← hW.1, ← hp]; exact hr
  | some p =>
    obtain ⟨hpv, hn, hfit, ho⟩ := hr
    exact ⟨by omega, W.step hW hn (by rw [← hp, ← hol]; exact hfit) (by rw [ho, hp]) hl⟩

theorem put_W (o out : Slice U8) (bs : List Nat) (pos : Usize) (src : Slice U8)
    (hW : W o out bs) (hp : pos.val = bs.length) :
    put o pos src ⦃ r o' => match r with
      | none => out.length < bs.length + src.length
      | some p => p.val = bs.length + src.length ∧ W o' out (bs ++ B src.val) ⦄ := by
  unfold put
  step with put_n_W o out bs pos src src.len hW hp
  split at r_post
  · simp_all
  · simp only [Slice.len_val] at r_post
    rw [List.take_of_length_le (by simp)] at r_post
    exact r_post

theorem put1_W (o out : Slice U8) (bs : List Nat) (pos : Usize) (b : U8)
    (hW : W o out bs) (hp : pos.val = bs.length) :
    put1 o pos b ⦃ r o' => match r with
      | none => out.length ≤ bs.length
      | some p => p.val = bs.length + 1 ∧ W o' out (bs ++ [b.val]) ⦄ := by
  unfold put1
  obtain ⟨h1, h2, h3⟩ := hW
  step*
  all_goals (try (simp only [Slice.len_val, Slice.length, ge_iff_le, UScalar.le_equiv] at *; omega))
  · have hlt : bs.length < o.val.length := by rw [← hp]; scalar_tac
    refine ⟨by scalar_tac, ?_⟩
    have hW' := W.step (o := o) (o' := out1) (src := [b]) (n := 1) ⟨h1, h2, h3⟩ (by simp) (by
      simp only [Slice.length] at *; omega) ?_ (by rw [out1_post]; simp [Slice.length])
    · simpa using hW'
    · rw [out1_post, Slice.set_val_eq, hp, List.set_eq_take_append_cons_drop, if_pos hlt]
      simp

theorem pow10_val (c : Usize) (hc : c.val < 10) : (POW10.val[c.val]'(by simp; omega)).val = 10 ^ (9 - c.val) := by
  unfold POW10
  have : c.val = 0 ∨ c.val = 1 ∨ c.val = 2 ∨ c.val = 3 ∨ c.val = 4 ∨ c.val = 5 ∨ c.val = 6 ∨ c.val = 7 ∨
    c.val = 8 ∨ c.val = 9 := by omega
  rcases this with h | h | h | h | h | h | h | h | h | h <;> simp [h]

@[step]
theorem dec_digits_loop_spec (n : U32) (buf : Std.Array U8 10#usize) (c k : Usize)
    (hc : c.val ≤ 10) (hk : k.val ≤ c.val) (hk0 : k.val = 0 ∨ 0 < k.val) :
    dec_digits_loop n buf c k ⦃ r =>
      r.1.val ≤ 10 ∧ B (r.2.val.take r.1.val) = B (buf.val.take k.val) ++ Spec.decFrom n.val c.val (0 < k.val) ⦄ := by
  unfold dec_digits_loop
  apply loop.spec_decr_nat (measure := fun (x : Std.Array U8 10#usize × Usize × Usize) => 10 - x.2.1.val)
    (inv := fun x => x.2.1.val ≤ 10 ∧ x.2.2.val ≤ x.2.1.val ∧
      B (buf.val.take k.val) ++ Spec.decFrom n.val c.val (0 < k.val) =
        B (x.1.val.take x.2.2.val) ++ Spec.decFrom n.val x.2.1.val (0 < x.2.2.val))
  · rintro ⟨b, j, m⟩ ⟨h1, h2, h3⟩
    simp only at h1 h2 h3 ⊢
    unfold dec_digits_loop.body
    have hp := pow10_val
    by_cases hj : j.val < 10
    · have hpj := hp j hj
      have hm10 : m.val < 10 := by omega
      have hbl : b.val.length = 10 := by simp
      rw [if_pos (by scalar_tac)]
      step
      step
      · rw [i_post, hpj]; simp
      step
      have hd10 : d.val < 10 := by rw [d_post]; omega
      have hdv : d.val = n.val / 10 ^ (9 - j.val) % 10 := by rw [d_post, i1_post, i_post, hpj]
      have hunf : Spec.decFrom n.val j.val (decide (0 < m.val)) =
          if d.val ≠ 0 ∨ decide (0 < m.val) = true ∨ j.val = 9 then (48 + d.val) :: Spec.decFrom n.val (j.val + 1) true
          else Spec.decFrom n.val (j.val + 1) false := by
        rw [Spec.decFrom, if_pos hj]; simp only [← hdv]
      have hw : ∀ (a : Std.Array U8 10#usize) (i3 : U8), i3.val = 48 + d.val → a = b.set m i3 →
          B (List.take (m.val + 1) a.val) = B (List.take m.val b.val) ++ [48 + d.val] := by
        intro a i3 hi3 ha
        rw [ha, Std.Array.set_val_eq, take_set_succ _ _ _ (by omega)]
        simp [hi3]
      have hcast : (UScalar.cast UScalarTy.U8 d).val = d.val := by simp; omega
      split_ifs with hA hB hC
      all_goals (simp only [bne_iff_ne, ne_eq, gt_iff_lt, UScalar.lt_equiv, UScalar.ofNatCore_val_eq] at hA)
      all_goals (try simp only [bne_iff_ne, ne_eq, gt_iff_lt, UScalar.lt_equiv, UScalar.ofNatCore_val_eq] at hB)
      all_goals (try simp only [UScalar.eq_equiv, UScalar.ofNatCore_val_eq] at hC)
      rotate_left 3
      · -- no digit yet: nothing written
        step as ⟨c1, hc1⟩
        refine ⟨by scalar_tac, by scalar_tac, ?_, by scalar_tac⟩
        have hd0 : d.val = 0 := by
          by_contra hne; exact hA (fun h => hne (by rw [h]; rfl))
        have hm0 : m.val = 0 := by omega
        rw [h3, hunf, if_neg (by simp only [not_or]; refine ⟨by omega, by simp [hm0], by omega⟩), hc1]
        simp [hm0]
      all_goals
        step as ⟨i2, hi2⟩
        step as ⟨i3, hi3⟩
        step as ⟨a, ha⟩
        step as ⟨k2, hk2⟩
        step as ⟨c1, hc1⟩
        have hi3' : i3.val = 48 + d.val := by rw [hi3, hi2, hcast]
        refine ⟨by scalar_tac, by scalar_tac, ?_, by scalar_tac⟩
        rw [h3, hunf, if_pos ?_, hc1, hk2, hw a i3 hi3' ha]
        · simp
        · first
            | (left; intro h; exact hA (by scalar_tac))
            | (right; left; simp; omega)
            | (right; right; scalar_tac)
    · rw [if_neg (by scalar_tac)]
      simp only [WP.spec_ok]
      have hj' : j.val = 10 := by omega
      refine ⟨by omega, ?_⟩
      rw [h3, hj', Spec.decFrom, if_neg (by omega)]
      simp
  · exact ⟨hc, hk, rfl⟩

theorem dec_digits_spec (n : U32) (buf : Std.Array U8 10#usize) :
    dec_digits n buf ⦃ r => r.1.val ≤ 10 ∧ B (r.2.val.take r.1.val) = Spec.dec n.val ⦄ := by
  unfold dec_digits
  apply WP.spec_mono (dec_digits_loop_spec n buf 0#usize 0#usize (by simp) (by simp) (by simp))
  rintro ⟨k, b⟩ ⟨h1, h2⟩
  refine ⟨h1, ?_⟩
  rw [h2]; simp [Spec.dec]

theorem put_dec_W (o out : Slice U8) (bs : List Nat) (pos : Usize) (n : U32)
    (hW : W o out bs) (hp : pos.val = bs.length) :
    put_dec o pos n ⦃ r o' => match r with
      | none => out.length < bs.length + (Spec.dec n.val).length
      | some p => p.val = bs.length + (Spec.dec n.val).length ∧ W o' out (bs ++ Spec.dec n.val) ⦄ := by
  unfold put_dec
  step with dec_digits_spec
  step
  have hsl : s.val = o'.val := by rw [s_post]; simp
  have hlen : (Spec.dec n.val).length = r.val := by
    rw [← r_post1]; simp; omega
  apply WP.spec_mono (put_n_W o out bs pos s r hW hp)
  rintro ⟨res, o2⟩ hres
  cases res with
  | none =>
    rcases hres with h | h
    · simp [Slice.length, hsl] at h; omega
    · show out.length < _; omega
  | some p =>
    obtain ⟨hp1, hp2⟩ := hres
    rw [hsl, r_post1] at hp2
    exact ⟨by omega, hp2⟩

theorem post_transfer {r : Option Usize} {o' out : Slice U8} {bs : List Nat} {s : Slice U8} {L : List Nat}
    (hL : B s.val = L) :
    (match r with
      | none => out.length < bs.length + s.length
      | some p => p.val = bs.length + s.length ∧ W o' out (bs ++ B s.val)) →
    (match r with
      | none => out.length < bs.length + L.length
      | some p => p.val = bs.length + L.length ∧ W o' out (bs ++ L)) := by
  intro h; subst hL; cases r <;> simpa [Slice.length] using h

theorem put_reason_W (o out : Slice U8) (bs : List Nat) (pos : Usize) (status : U16)
    (hW : W o out bs) (hp : pos.val = bs.length) :
    put_reason o pos status ⦃ r o' => match r with
      | none => out.length < bs.length + (Spec.reasonOf status.val).length
      | some p => p.val = bs.length + (Spec.reasonOf status.val).length ∧
          W o' out (bs ++ Spec.reasonOf status.val) ⦄ := by
  unfold put_reason
  split
  all_goals (try (
    step
    step with put_W o out bs pos _ hW hp
    refine post_transfer ?_ r_post
    subst s_post; decide))
  rename_i h200 h201 h204 h400 h401 h403 h404 h405 h408 h409 h413 h414 h422 h431 h500 h501 h503 h505
  have n200 : status.val ≠ 200 := fun e => h200 (UScalar.eq_of_val_eq (by rw [e]; rfl))
  have n201 : status.val ≠ 201 := fun e => h201 (UScalar.eq_of_val_eq (by rw [e]; rfl))
  have n204 : status.val ≠ 204 := fun e => h204 (UScalar.eq_of_val_eq (by rw [e]; rfl))
  have n400 : status.val ≠ 400 := fun e => h400 (UScalar.eq_of_val_eq (by rw [e]; rfl))
  have n401 : status.val ≠ 401 := fun e => h401 (UScalar.eq_of_val_eq (by rw [e]; rfl))
  have n403 : status.val ≠ 403 := fun e => h403 (UScalar.eq_of_val_eq (by rw [e]; rfl))
  have n404 : status.val ≠ 404 := fun e => h404 (UScalar.eq_of_val_eq (by rw [e]; rfl))
  have n405 : status.val ≠ 405 := fun e => h405 (UScalar.eq_of_val_eq (by rw [e]; rfl))
  have n408 : status.val ≠ 408 := fun e => h408 (UScalar.eq_of_val_eq (by rw [e]; rfl))
  have n409 : status.val ≠ 409 := fun e => h409 (UScalar.eq_of_val_eq (by rw [e]; rfl))
  have n413 : status.val ≠ 413 := fun e => h413 (UScalar.eq_of_val_eq (by rw [e]; rfl))
  have n414 : status.val ≠ 414 := fun e => h414 (UScalar.eq_of_val_eq (by rw [e]; rfl))
  have n422 : status.val ≠ 422 := fun e => h422 (UScalar.eq_of_val_eq (by rw [e]; rfl))
  have n431 : status.val ≠ 431 := fun e => h431 (UScalar.eq_of_val_eq (by rw [e]; rfl))
  have n500 : status.val ≠ 500 := fun e => h500 (UScalar.eq_of_val_eq (by rw [e]; rfl))
  have n501 : status.val ≠ 501 := fun e => h501 (UScalar.eq_of_val_eq (by rw [e]; rfl))
  have n503 : status.val ≠ 503 := fun e => h503 (UScalar.eq_of_val_eq (by rw [e]; rfl))
  have n505 : status.val ≠ 505 := fun e => h505 (UScalar.eq_of_val_eq (by rw [e]; rfl))
  have hr : Spec.reasonOf status.val = [] := by
    simp only [Spec.reasonOf, n200, n201, n204, n400, n401, n403, n404, n405, n408, n409, n413, n414, n422, n431, n500, n501, n503, n505, if_false]
  simp only [WP.spec_ok, hr, List.append_nil, List.length_nil, Nat.add_zero]
  exact ⟨hp, hW⟩

@[step] theorem put_n_total (o : Slice U8) (pos : Usize) (src : Slice U8) (n : Usize) :
    put_n o pos src n ⦃ _ _ => True ⦄ :=
  WP.spec_mono (put_n_spec o pos src n) (fun _ _ => trivial)

@[step] theorem put_total (o : Slice U8) (pos : Usize) (src : Slice U8) : put o pos src ⦃ _ _ => True ⦄ := by
  unfold put; step*

@[step] theorem put1_total (o : Slice U8) (pos : Usize) (b : U8) : put1 o pos b ⦃ _ _ => True ⦄ := by
  unfold put1; step*

@[step] theorem put_dec_total (o : Slice U8) (pos : Usize) (n : U32) : put_dec o pos n ⦃ _ _ => True ⦄ := by
  unfold put_dec
  step with dec_digits_spec
  step*

@[step] theorem put_reason_total (o : Slice U8) (pos : Usize) (status : U16) :
    put_reason o pos status ⦃ _ _ => True ⦄ := by
  unfold put_reason; split <;> step*

@[step] theorem has_body_spec (status : U16) :
    has_body status ⦃ b => b = Spec.hasBody status.val ⦄ := by
  unfold has_body Spec.hasBody
  split_ifs <;> simp only [WP.spec_ok, UScalar.lt_equiv, UScalar.ofNatCore_val_eq] at * <;> (try omega)
  all_goals (rw [Bool.eq_iff_iff]; simp only [bne_iff_ne, ne_eq, Bool.and_eq_true, UScalar.eq_equiv,
    UScalar.ofNatCore_val_eq] at *; simp_all)

set_option maxHeartbeats 2000000 in
/-- `write_head` never panics, for every buffer (of any length), status,
content type and body length. -/
theorem write_head_total (out : Slice U8) (status : U16) (ct : Slice U8) (bl : U32) (close : Bool) :
    write_head out status ct bl close ⦃ _ _ => True ⦄ := by
  unfold write_head
  have hR : Reads ct (fun k => (B ct.val).getD k 0) ct.length := by
    refine ⟨le_refl _, fun k hk _ => ?_⟩
    simp [List.getD_eq_getElem?_getD, List.getElem?_eq_getElem hk]
  step*
  all_goals (try (split <;> step*))
  all_goals (try (split <;> step*))
  all_goals (try (split <;> step*))
  all_goals (try (split <;> step*))
  all_goals (try (split <;> step*))
  all_goals (try (split <;> step*))

end h5i_app_h1.Proofs
