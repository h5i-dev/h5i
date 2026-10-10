import H5iAppStd
import H5iAppLib
/-!
# Specs of h5i-app-std

A `@[step]` spec for every function of `h5i-app-std`, stating its result over
the lists the vectors hold, with the models of `H5iAppLib.Text` and
`H5iAppLib.Sets`. `step*` then goes through a call to any of them.

This file is checked against the crate's own extraction. A kernel that
includes the crate gets a copy importing the kernel's extraction instead
(`h5i app extract`, with `std-specs = true`), which opens the kernel's namespace: the
extracted functions have the same names and bodies there, so the same proofs
go through. Aeneas extracts only the functions the kernel calls, so each
spec is guarded by its function (`h5i_when`).
-/
open Aeneas Aeneas.Std Result H5iAppLib

-- In a kernel's copy, the specs of functions the kernel does not call are
-- skipped, and their tactics would be reported as never run.
set_option linter.unusedTactic false
set_option linter.unreachableTactic false

namespace h5i_app_std.Specs

/-! ## Byte strings -/

h5i_when h5i_app_std.bytes.eq =>
@[step] theorem bytes.eq_spec (a b : alloc.vec.Vec U8) :
    h5i_app_std.bytes.eq a b ⦃ r => r = decide (a.val = b.val) ⦄ := by
  unfold h5i_app_std.bytes.eq
  apply WP.spec_mono (vec_u8_eq_spec a b)
  intro r hr; rw [hr]; exact decide_eq_decide.2 (alloc.vec.Vec.eq_iff a b)

h5i_when h5i_app_std.bytes.range_eq =>
theorem bytes.range_eq_spec (a : alloc.vec.Vec U8) (ao : Usize) (b : alloc.vec.Vec U8) (bo : Usize) (n : Usize)
    (ha : ao.val + n.val ≤ a.length) (hb : bo.val + n.val ≤ b.length) :
    h5i_app_std.bytes.range_eq a ao b bo n ⦃ r => r = decide ((a.val.drop ao.val).take n.val = (b.val.drop bo.val).take n.val) ⦄ := by
  unfold h5i_app_std.bytes.range_eq h5i_app_std.bytes.range_eq_loop
  apply WP.spec_mono (loop_search (((a.val.drop ao.val).take n.val).zip ((b.val.drop bo.val).take n.val))
    (fun q => decide (q.1 ≠ q.2)) (fun r : Bool => r) (fun _ _ => false) true _ ?_ 0#usize (by simp))
  · intro r hr
    have ha' : ao.val + n.val ≤ a.val.length := ha
    have hb' : bo.val + n.val ≤ b.val.length := hb
    rw [search_bool _ _ _ _ _ hr, ← zip_all_eq _ _ (by simp; omega), List.all_eq_not_any_not]
    simp only [decide_not]
    cases List.any _ _ <;> rfl
  · intro j hj
    unfold h5i_app_std.bytes.range_eq_loop.body
    simp at hj
    h5i_step

h5i_when h5i_app_std.bytes.range_eq =>
/-- `range_eq` under the bounds the callers below establish. -/
@[step] theorem bytes.range_eq_spec' (a : alloc.vec.Vec U8) (ao : Usize) (b : alloc.vec.Vec U8) (bo : Usize) (n : Usize)
    (ha : ao.val + n.val ≤ a.val.length) (hb : bo.val + n.val ≤ b.val.length) :
    h5i_app_std.bytes.range_eq a ao b bo n ⦃ r => r = decide ((a.val.drop ao.val).take n.val = (b.val.drop bo.val).take n.val) ⦄ :=
  bytes.range_eq_spec a ao b bo n ha hb

theorem not_prefix_of_long {p s : List U8} (h : s.length < p.length) : decide (p <+: s) = false := by
  simp only [decide_eq_false_iff_not]; intro hp; have := hp.length_le; omega

theorem not_suffix_of_long {p s : List U8} (h : s.length < p.length) : decide (p <:+ s) = false := by
  simp only [decide_eq_false_iff_not]; intro hp; have := hp.length_le; omega

h5i_when h5i_app_std.bytes.starts_with =>
@[step] theorem bytes.starts_with_spec (s p : alloc.vec.Vec U8) :
    h5i_app_std.bytes.starts_with s p ⦃ r => r = decide (p.val <+: s.val) ⦄ := by
  unfold h5i_app_std.bytes.starts_with
  simp only [alloc.vec.Vec.len]
  split
  · rename_i hl; simp only [WP.spec_ok]; rw [not_prefix_of_long (by scalar_tac)]
  · rename_i hl
    have hl' : p.val.length ≤ s.val.length := by scalar_tac
    apply WP.spec_mono (bytes.range_eq_spec s 0#usize p 0#usize _ (by simpa using hl') (by simp))
    intro r hr; rw [hr]; simp
    rw [prefix_iff_take]; simp [hl']

h5i_when h5i_app_std.bytes.ends_with =>
@[step] theorem bytes.ends_with_spec (s p : alloc.vec.Vec U8) :
    h5i_app_std.bytes.ends_with s p ⦃ r => r = decide (p.val <:+ s.val) ⦄ := by
  unfold h5i_app_std.bytes.ends_with
  simp only [alloc.vec.Vec.len]
  split
  · rename_i hl; simp only [WP.spec_ok]; rw [not_suffix_of_long (by scalar_tac)]
  · rename_i hl
    have hl' : p.val.length ≤ s.val.length := by scalar_tac
    step*
    have h4 : i4.val = s.val.length - p.val.length := by scalar_tac
    rw [r_post]
    simp only [h4, List.drop_zero]
    rw [List.take_of_length_le (by simp; omega)]
    simp only [suffix_iff_drop, hl', true_and]
    simp [eq_comm]

h5i_when h5i_app_std.bytes.slice =>
@[step] theorem bytes.slice_spec (s : alloc.vec.Vec U8) (lo hi : Usize) (h : lo.val ≤ hi.val) (hs : hi.val ≤ s.val.length) :
    h5i_app_std.bytes.slice s lo hi ⦃ v => v.val = (s.val.drop lo.val).take (hi.val - lo.val) ⦄ := by
  unfold h5i_app_std.bytes.slice h5i_app_std.bytes.slice_loop
  apply WP.spec_mono (loop_fold (s.val.take hi.val) (fun o : alloc.vec.Vec U8 => o.val)
    (fun o x => o ++ [x]) (fun o j => o.length ≤ j)
    (fun x => h5i_app_std.bytes.slice_loop.body s hi x.1 x.2) ?_ (alloc.vec.Vec.new U8) lo (by simp; omega) (by simp))
  · intro v hv; rw [hv, foldl_snoc, ← List.drop_take]; simp
  · intro o i hi' hinv
    unfold h5i_app_std.bytes.slice_loop.body
    simp only [List.length_take] at hi'
    h5i_step

h5i_when h5i_app_std.bytes.strip_prefix =>
@[step] theorem bytes.strip_prefix_spec (s p : alloc.vec.Vec U8) :
    h5i_app_std.bytes.strip_prefix s p ⦃ r => (if p.val <+: s.val then some (s.val.drop p.val.length) else none) = r.map (·.val) ⦄ := by
  unfold h5i_app_std.bytes.strip_prefix
  apply WP.spec_bind (bytes.starts_with_spec s p)
  intro b hb; subst hb
  split
  · rename_i h
    have hp : p.val <+: s.val := by simpa using h
    have hl := hp.length_le
    step*
    simp [v_post]
  · rename_i h
    have hp : ¬ p.val <+: s.val := by simpa using h
    simp [hp]

h5i_when h5i_app_std.bytes.strip_suffix =>
@[step] theorem bytes.strip_suffix_spec (s p : alloc.vec.Vec U8) :
    h5i_app_std.bytes.strip_suffix s p ⦃ r =>
      (if p.val <:+ s.val then some (s.val.take (s.val.length - p.val.length)) else none) = r.map (·.val) ⦄ := by
  unfold h5i_app_std.bytes.strip_suffix
  apply WP.spec_bind (bytes.ends_with_spec s p)
  intro b hb; subst hb
  split
  · rename_i h
    have hp : p.val <:+ s.val := by simpa using h
    have hl := hp.length_le
    step*
    simp [v_post, i2_post]

  · rename_i h
    have hp : ¬ p.val <:+ s.val := by simpa using h
    simp [hp]

h5i_when h5i_app_std.bytes.contains =>
@[step] theorem bytes.contains_spec (s needle : alloc.vec.Vec U8) :
    h5i_app_std.bytes.contains s needle ⦃ r => r = decide (needle.val <:+: s.val) ⦄ := by
  unfold h5i_app_std.bytes.contains
  simp only [alloc.vec.Vec.len]
  split
  · rename_i h0; simp only [WP.spec_ok]
    have : needle.val = [] := by have := (usize_ofNatCore_eq_zero _ _).1 h0; simpa using this
    simp [this]
  split
  · rename_i h0 hl; simp only [WP.spec_ok]
    symm; simp only [decide_eq_false_iff_not]; intro h; have := h.length_le; scalar_tac
  · rename_i h0 hl
    have hl' : needle.val.length ≤ s.val.length := by scalar_tac
    have h0' : needle.val.length ≠ 0 := fun h => h0 ((usize_ofNatCore_eq_zero _ _).2 h)
    step*
    have hlast : last.val = s.val.length - needle.val.length := by scalar_tac
    unfold h5i_app_std.bytes.contains_loop
    apply WP.spec_mono (loop_search (List.range (last.val + 1))
      (fun i => decide ((s.val.drop i).take needle.val.length = needle.val)) (fun r : Bool => r)
      (fun _ _ => true) false _ ?_ 0#usize (by simp))
    · intro r hr
      rw [search_any _ _ _ hr, Bool.eq_iff_iff]
      simp only [List.any_eq_true, List.mem_range, decide_eq_true_eq, infix_iff_window]
      constructor
      · rintro ⟨i, hi, h⟩; exact ⟨i, by omega, h⟩
      · rintro ⟨i, hi, h⟩; exact ⟨i, by omega, h⟩
    · intro j hj
      unfold h5i_app_std.bytes.contains_loop.body
      simp only [List.length_range] at hj
      h5i_step

h5i_when h5i_app_std.bytes.find_byte =>
@[step] theorem bytes.find_byte_spec (s : alloc.vec.Vec U8) (c : U8) :
    h5i_app_std.bytes.find_byte s c ⦃ r => s.val.findIdx? (fun x => decide (x = c)) = r.map (·.val) ⦄ := by
  unfold h5i_app_std.bytes.find_byte h5i_app_std.bytes.find_byte_loop
  apply WP.spec_mono (loop_search s.val (fun x => decide (x = c)) (fun r : Option Usize => r.map (·.val))
    (fun i _ => some i) none _ ?_ 0#usize (by simp))
  · intro r hr; exact (search_findIdx _ _ _ hr).symm
  · intro j hj; unfold h5i_app_std.bytes.find_byte_loop.body; h5i_step

h5i_when h5i_app_std.bytes.contains_byte =>
@[step] theorem bytes.contains_byte_spec (s : alloc.vec.Vec U8) (c : U8) :
    h5i_app_std.bytes.contains_byte s c ⦃ r => r = decide (c ∈ s.val) ⦄ := by
  unfold h5i_app_std.bytes.contains_byte h5i_app_std.bytes.contains_byte_loop
  apply WP.spec_mono (loop_search s.val (fun x => decide (x = c)) (fun r : Bool => r)
    (fun _ _ => true) false _ ?_ 0#usize (by simp))
  · intro r hr; exact (search_any _ _ _ hr).trans ((any_eq_iff _ _).trans (decide_eq_decide.2 Iff.rfl))
  · intro j hj; unfold h5i_app_std.bytes.contains_byte_loop.body; h5i_step

h5i_when h5i_app_std.bytes.split_once =>
@[step] theorem bytes.split_once_spec (s : alloc.vec.Vec U8) (c : U8) :
    h5i_app_std.bytes.split_once s c ⦃ r => splitOnce c s.val = r.map (fun p => (p.1.val, p.2.val)) ⦄ := by
  unfold h5i_app_std.bytes.split_once
  apply WP.spec_bind (bytes.find_byte_spec s c)
  intro o ho
  unfold splitOnce; rw [ho]
  cases o with
  | none => simp
  | some i =>
    simp only [Option.map_some] at ho
    have hi := (List.findIdx?_eq_some_iff_getElem.1 ho).1
    step*
    simp [v_post, v1_post, i1_post]

h5i_when h5i_app_std.bytes.is_whitespace =>
@[step] theorem bytes.is_whitespace_spec (c : U8) : h5i_app_std.bytes.is_whitespace c ⦃ r => r = isWs c ⦄ := by
  unfold h5i_app_std.bytes.is_whitespace isWs
  split
  · simp_all
  · split <;> simp_all

h5i_when h5i_app_std.bytes.lower =>
@[step] theorem bytes.lower_spec (c : U8) : h5i_app_std.bytes.lower c ⦃ r => r = lowerB c ⦄ := by
  unfold h5i_app_std.bytes.lower
  split
  · split
    · step*; apply UScalar.eq_of_val_eq; rw [lowerB_val]; split <;> scalar_tac
    · simp only [WP.spec_ok]; apply UScalar.eq_of_val_eq; rw [lowerB_val]; split <;> scalar_tac
  · simp only [WP.spec_ok]; apply UScalar.eq_of_val_eq; rw [lowerB_val]; split <;> scalar_tac

h5i_when h5i_app_std.bytes.to_lowercase =>
@[step] theorem bytes.to_lowercase_spec (s : alloc.vec.Vec U8) :
    h5i_app_std.bytes.to_lowercase s ⦃ v => v.val = s.val.map lowerB ⦄ := by
  unfold h5i_app_std.bytes.to_lowercase h5i_app_std.bytes.to_lowercase_loop
  apply WP.spec_mono (loop_fold s.val (fun o : alloc.vec.Vec U8 => o.val)
    (fun o x => o ++ [lowerB x]) (fun o j => o.length ≤ j)
    (fun x => h5i_app_std.bytes.to_lowercase_loop.body s x.1 x.2) ?_ (alloc.vec.Vec.new U8) 0#usize (by simp) (by simp))
  · intro v hv; rw [hv, foldl_map]; simp
  · intro o i hi hinv
    unfold h5i_app_std.bytes.to_lowercase_loop.body
    h5i_step

theorem map_eq_iff_zip {α β} [DecidableEq β] (f : α → β) (a b : List α) (h : a.length = b.length) :
    decide (a.map f = b.map f) = !(a.zip b).any (fun q => decide (f q.1 ≠ f q.2)) := by
  induction a generalizing b with
  | nil => cases b <;> simp_all
  | cons x xs ih =>
    cases b with
    | nil => simp at h
    | cons y ys =>
      simp only [List.length_cons, Nat.add_right_cancel_iff] at h
      have := ih ys h
      by_cases hxy : f x = f y <;> simp_all

h5i_when h5i_app_std.bytes.eq_ignore_case =>
@[step] theorem bytes.eq_ignore_case_spec (a b : alloc.vec.Vec U8) :
    h5i_app_std.bytes.eq_ignore_case a b ⦃ r => r = decide (a.val.map lowerB = b.val.map lowerB) ⦄ := by
  unfold h5i_app_std.bytes.eq_ignore_case
  simp only [alloc.vec.Vec.len]
  split
  · rename_i hl
    simp only [WP.spec_ok]; symm; simp only [decide_eq_false_iff_not]
    intro h; have := congrArg List.length h; simp only [List.length_map] at this
    simp [this] at hl
  · rename_i hl
    have hl' : a.val.length = b.val.length := by simpa using hl
    unfold h5i_app_std.bytes.eq_ignore_case_loop
    apply WP.spec_mono (loop_search (a.val.zip b.val) (fun q => decide (lowerB q.1 ≠ lowerB q.2))
      (fun r : Bool => r) (fun _ _ => false) true _ ?_ 0#usize (by simp))
    · intro r hr; rw [search_bool _ _ _ _ _ hr, map_eq_iff_zip _ _ _ hl']
      cases List.any _ _ <;> rfl
    · intro j hj
      unfold h5i_app_std.bytes.eq_ignore_case_loop.body
      simp only [List.length_zip, hl', Nat.min_self] at hj
      h5i_step

h5i_when h5i_app_std.bytes.concat =>
@[step] theorem bytes.concat_spec (a b : alloc.vec.Vec U8) (h : a.val.length + b.val.length ≤ Usize.max) :
    h5i_app_std.bytes.concat a b ⦃ v => v.val = a.val ++ b.val ⦄ := by
  unfold h5i_app_std.bytes.concat h5i_app_std.bytes.concat_loop
  step*
  apply WP.spec_mono (loop_fold b.val (fun o : alloc.vec.Vec U8 => o.val)
    (fun o x => o ++ [x]) (fun o j => o.length = a.val.length + j)
    (fun x => h5i_app_std.bytes.concat_loop.body b x.1 x.2) ?_ out 0#usize (by simp) (by simp [out_post]))
  · intro v hv; rw [hv, foldl_snoc]; simp [out_post]
  · intro o i hi hinv
    unfold h5i_app_std.bytes.concat_loop.body
    h5i_step

h5i_when h5i_app_std.bytes.star_match =>
@[step] theorem bytes.star_match_spec (p s : alloc.vec.Vec U8) (star : U8) :
    h5i_app_std.bytes.star_match p s star ⦃ r => r = decide (starMatch star p.val s.val) ⦄ := by
  unfold h5i_app_std.bytes.star_match
  apply WP.spec_bind (vec_u8_eq_spec p s)
  intro b hb; subst hb
  dsimp only
  split
  · rename_i h
    have : p.val = s.val := by simpa [alloc.vec.Vec.eq_iff] using h
    simp [starMatch, this]
  · rename_i h
    have hne : p.val ≠ s.val := by simpa [alloc.vec.Vec.eq_iff] using h
    simp only [alloc.vec.Vec.len]
    split
    · rename_i h0
      have : p.val = [] := by have := (usize_ofNatCore_eq_zero _ _).1 h0; simpa using this
      simp [starMatch, this]; intro hs; exact hne (by simp [this, hs])
    · rename_i h0
      have hp0 : p.val.length ≠ 0 := fun h => h0 ((usize_ofNatCore_eq_zero _ _).2 h)
      have hi := starMatch_iff_last (star := star) (s := s.val) hp0 hne
      step*
      all_goals simp only [hi]
      all_goals simp_all
      all_goals first | (intros; omega) | scalar_tac

h5i_when h5i_app_std.bytes.trim =>
theorem bytes.trim_loop0_spec (s : alloc.vec.Vec U8) :
    h5i_app_std.bytes.trim_loop0 s 0#usize ⦃ lo => lo.val = (s.val.takeWhile isWs).length ⦄ := by
  unfold h5i_app_std.bytes.trim_loop0
  apply WP.spec_mono (loop_search s.val (fun x => !isWs x) (fun r : Usize => r.val)
    (fun i _ => i) s.val.length _ ?_ 0#usize (by simp))
  · intro r hr; rw [hr, searchFrom_index]; simp
  · intro j hj; unfold h5i_app_std.bytes.trim_loop0.body; h5i_step

theorem take_drop_succ {α} (s : List α) (lo k : Nat) (h1 : lo ≤ k) (h2 : k < s.length) :
    (s.take (k + 1)).drop lo = (s.take k).drop lo ++ [s[k]] := by
  rw [List.take_succ_eq_append_getElem h2, List.drop_append_of_le_length (by simp; omega)]

h5i_when h5i_app_std.bytes.trim =>
theorem bytes.trim_loop1_spec (s : alloc.vec.Vec U8) (lo hi : Usize) (h1 : lo.val ≤ hi.val) (h2 : hi.val ≤ s.val.length) :
    h5i_app_std.bytes.trim_loop1 s lo hi ⦃ y => lo.val ≤ y.val ∧ y.val ≤ s.val.length ∧
      (s.val.take y.val).drop lo.val = ((s.val.take hi.val).drop lo.val).rdropWhile isWs ⦄ := by
  unfold h5i_app_std.bytes.trim_loop1
  apply loop.spec_decr_nat (measure := fun (j : Usize) => j.val)
    (inv := fun j => lo.val ≤ j.val ∧ j.val ≤ hi.val ∧
      ((s.val.take j.val).drop lo.val).rdropWhile isWs = ((s.val.take hi.val).drop lo.val).rdropWhile isWs)
  · rintro j ⟨hj1, hj2, heq⟩
    unfold h5i_app_std.bytes.trim_loop1.body
    split
    · rename_i hgt
      have hlt : j.val - 1 < s.val.length := by scalar_tac
      have hsplit := take_drop_succ s.val lo.val (j.val - 1) (by scalar_tac) hlt
      rw [show j.val - 1 + 1 = j.val by scalar_tac] at hsplit
      step*
      · refine ⟨by scalar_tac, by scalar_tac, ?_, by scalar_tac⟩
        rw [i_post, ← heq, hsplit]
        symm; apply List.rdropWhile_concat_pos; simp_all
      · refine ⟨by scalar_tac, by scalar_tac, ?_⟩
        rw [← heq, hsplit]
        symm; apply List.rdropWhile_concat_neg; simp_all
    · rename_i hle
      simp only [WP.spec_ok]
      have : j.val = lo.val := by scalar_tac
      refine ⟨hj1, by omega, ?_⟩
      rw [← heq, this, List.drop_eq_nil_of_le (by simp)]; rfl
  · exact ⟨h1, le_refl _, rfl⟩

h5i_when h5i_app_std.bytes.trim =>
@[step] theorem bytes.trim_spec (s : alloc.vec.Vec U8) : h5i_app_std.bytes.trim s ⦃ v => v.val = trimB s.val ⦄ := by
  unfold h5i_app_std.bytes.trim
  apply WP.spec_bind (bytes.trim_loop0_spec s)
  intro lo hlo
  have hle : lo.val ≤ s.val.length := by rw [hlo]; exact (List.takeWhile_prefix _).length_le
  simp only [alloc.vec.Vec.len]
  apply WP.spec_bind (bytes.trim_loop1_spec s lo _ (by simpa using hle) (by simp))
  rintro hi ⟨h1, h2, heq⟩
  apply WP.spec_mono (bytes.slice_spec s lo hi h1 h2)
  intro v hv
  rw [hv, ← List.drop_take, heq]
  simp only [Usize.ofNatCore_val_eq, List.take_length, trimB, hlo, drop_takeWhile_length]

/-- The pieces a `split` loop state stands for. -/
abbrev pieces (o : alloc.vec.Vec (alloc.vec.Vec U8)) (cu : alloc.vec.Vec U8) : List (List U8) × List U8 :=
  (o.val.map (·.val), cu.val)

h5i_when h5i_app_std.bytes.split =>
@[step] theorem bytes.split_spec (s : alloc.vec.Vec U8) (c : U8) (h : s.val.length < Usize.max) :
    h5i_app_std.bytes.split s c ⦃ v => v.val.map (·.val) = splitB c s.val ⦄ := by
  unfold h5i_app_std.bytes.split h5i_app_std.bytes.split_loop
  apply WP.spec_bind (loop_fold2 s.val pieces (splitStep c) (fun o cu j => o.val.length + cu.val.length ≤ j)
    _ ?_ (alloc.vec.Vec.new _) (alloc.vec.Vec.new U8) 0#usize (by simp) (by simp))
  · rintro ⟨o, cu⟩ ⟨habs, hlen⟩
    simp only at habs hlen ⊢
    step*
    have := foldl_splitStep c s.val [] []
    simp only [UScalar.ofNatCore_val_eq, List.drop_zero, pieces, vec_new_val, List.map_nil, List.reverse_nil] at habs this
    rw [← habs] at this
    simp only [List.nil_append, List.splitOnPPrepend_nil_right] at this
    simp [splitB, v_post, ← this]
  · intro o cu i hi hinv
    unfold h5i_app_std.bytes.split_loop.body
    h5i_step [splitStep]

h5i_when h5i_app_std.bytes.split_whitespace =>
@[step] theorem bytes.split_whitespace_spec (s : alloc.vec.Vec U8) :
    h5i_app_std.bytes.split_whitespace s ⦃ v => v.val.map (·.val) = splitWs s.val ⦄ := by
  unfold h5i_app_std.bytes.split_whitespace h5i_app_std.bytes.split_whitespace_loop
  apply WP.spec_bind (loop_fold2 s.val pieces splitWsStep (fun o cu j => o.val.length + cu.val.length ≤ j)
    _ ?_ (alloc.vec.Vec.new _) (alloc.vec.Vec.new U8) 0#usize (by simp) (by simp))
  · rintro ⟨o, cu⟩ ⟨habs, hlen⟩
    simp only at habs hlen ⊢
    have := foldl_splitWsStep s.val [] []
    simp only [UScalar.ofNatCore_val_eq, List.drop_zero, pieces, vec_new_val, List.map_nil, List.reverse_nil] at habs this
    rw [← habs] at this
    simp only [List.nil_append, List.splitOnPPrepend_nil_right] at this
    unfold splitWs; rw [← this]
    step*
    · simp [v_post]; intro h; simp [h] at *
    · simp_all
  · intro o cu i hi hinv
    unfold h5i_app_std.bytes.split_whitespace_loop.body
    h5i_step [splitWsStep, pieces, List.length_pos_iff]

/-! ## Sets -/

section Sets
variable {T : Type} [DecidableEq T] (inst : core.cmp.PartialEq T T) [EqLaw inst]

h5i_when h5i_app_std.set.contains =>
@[step] theorem set.contains_spec (s : alloc.vec.Vec T) (x : T) :
    h5i_app_std.set.contains inst s x ⦃ r => r = decide (x ∈ s.val) ⦄ := by
  unfold h5i_app_std.set.contains h5i_app_std.set.contains_loop
  apply WP.spec_mono (loop_search s.val (fun y => decide (y = x)) (fun r : Bool => r)
    (fun _ _ => true) false _ ?_ 0#usize (by simp))
  · intro r hr; exact (search_any _ _ _ hr).trans ((any_eq_iff _ _).trans (decide_eq_decide.2 Iff.rfl))
  · intro j hj; unfold h5i_app_std.set.contains_loop.body; h5i_step

h5i_when h5i_app_std.set.subset =>
@[step] theorem set.subset_spec (a b : alloc.vec.Vec T) :
    h5i_app_std.set.subset inst a b ⦃ r => r = decide (a.val ⊆ b.val) ⦄ := by
  unfold h5i_app_std.set.subset h5i_app_std.set.subset_loop
  apply WP.spec_mono (loop_search a.val (fun y => decide (y ∉ b.val)) (fun r : Bool => r)
    (fun _ _ => false) true _ ?_ 0#usize (by simp))
  · intro r hr; rw [search_all _ _ _ hr]
    rw [Bool.eq_iff_iff]; simp [List.subset_def]
  · intro j hj; unfold h5i_app_std.set.subset_loop.body; h5i_step

h5i_when h5i_app_std.set.set_eq =>
@[step] theorem set.set_eq_spec (a b : alloc.vec.Vec T) :
    h5i_app_std.set.set_eq inst a b ⦃ r => r = decide (SetEq a.val b.val) ⦄ := by
  unfold h5i_app_std.set.set_eq SetEq
  step*
  all_goals simp_all

h5i_when h5i_app_std.set.intersects =>
@[step] theorem set.intersects_spec (a b : alloc.vec.Vec T) :
    h5i_app_std.set.intersects inst a b ⦃ r => r = decide (∃ x ∈ a.val, x ∈ b.val) ⦄ := by
  unfold h5i_app_std.set.intersects h5i_app_std.set.intersects_loop
  apply WP.spec_mono (loop_search a.val (fun y => decide (y ∈ b.val)) (fun r : Bool => r)
    (fun _ _ => true) false _ ?_ 0#usize (by simp))
  · intro r hr; rw [search_any _ _ _ hr, Bool.eq_iff_iff]; simp
  · intro j hj; unfold h5i_app_std.set.intersects_loop.body; h5i_step

h5i_when h5i_app_std.set.position =>
@[step] theorem set.position_spec (s : alloc.vec.Vec T) (x : T) :
    h5i_app_std.set.position inst s x ⦃ r => s.val.findIdx? (fun y => decide (y = x)) = r.map (·.val) ⦄ := by
  unfold h5i_app_std.set.position h5i_app_std.set.position_loop
  apply WP.spec_mono (loop_search s.val (fun y => decide (y = x)) (fun r : Option Usize => r.map (·.val))
    (fun i _ => some i) none _ ?_ 0#usize (by simp))
  · intro r hr; exact (search_findIdx _ _ _ hr).symm
  · intro j hj; unfold h5i_app_std.set.position_loop.body; h5i_step

variable (cinst : core.clone.Clone T) [CloneLaw cinst]

h5i_when h5i_app_std.set.insert =>
@[step] theorem set.insert_spec (s : alloc.vec.Vec T) (x : T) (h : s.val.length < Usize.max) :
    h5i_app_std.set.insert inst cinst s x ⦃ v => v.val = if x ∈ s.val then s.val else s.val ++ [x] ⦄ := by
  unfold h5i_app_std.set.insert
  step*

h5i_when h5i_app_std.set.remove =>
@[step] theorem set.remove_spec (s : alloc.vec.Vec T) (x : T) :
    h5i_app_std.set.remove inst cinst s x ⦃ v => v.val = s.val.filter (fun y => decide (y ≠ x)) ⦄ := by
  unfold h5i_app_std.set.remove h5i_app_std.set.remove_loop
  apply WP.spec_mono (loop_fold s.val (fun o : alloc.vec.Vec T => o.val)
    (fun o y => if decide (y ≠ x) then o ++ [y] else o) (fun o j => o.length ≤ j)
    (fun st => h5i_app_std.set.remove_loop.body inst cinst s x st.1 st.2) ?_ (alloc.vec.Vec.new T) 0#usize (by simp) (by simp))
  · intro v hv; rw [hv, foldl_filter]; simp
  · intro o i hi hinv
    unfold h5i_app_std.set.remove_loop.body
    h5i_step

end Sets

/-! ## Maps -/

section Maps
variable {K V : Type} [DecidableEq K] (inst : core.cmp.PartialEq K K) [EqLaw inst]

h5i_when h5i_app_std.map.contains_key =>
@[step] theorem map.contains_key_spec (m : alloc.vec.Vec (K × V)) (k : K) :
    h5i_app_std.map.contains_key inst m k ⦃ r => r = (mapGet k m.val).isSome ⦄ := by
  unfold h5i_app_std.map.contains_key h5i_app_std.map.contains_key_loop
  apply WP.spec_mono (loop_search m.val (fun e => decide (e.1 = k)) (fun r : Bool => r)
    (fun _ _ => true) false _ ?_ 0#usize (by simp))
  · intro r hr; rw [search_any _ _ _ hr, mapGet_eq_find, Bool.eq_iff_iff]; simp
  · intro j hj; unfold h5i_app_std.map.contains_key_loop.body; h5i_step [Prod.ext_iff]

variable (vinst : core.clone.Clone V) [CloneLaw vinst]

h5i_when h5i_app_std.map.get =>
@[step] theorem map.get_spec (m : alloc.vec.Vec (K × V)) (k : K) :
    h5i_app_std.map.get inst vinst m k ⦃ r => mapGet k m.val = r ⦄ := by
  unfold h5i_app_std.map.get h5i_app_std.map.get_loop
  apply WP.spec_mono (loop_search m.val (fun e => decide (e.1 = k)) id
    (fun _ e => some e.2) none _ ?_ 0#usize (by simp))
  · intro r hr; rw [id_eq] at hr
    have := searchFrom_find m.val (fun e => decide (e.1 = k)) (fun e : K × V => e.2) 0
    rw [hr, UScalar.ofNatCore_val_eq, this, mapGet_eq_find]; simp
  · intro j hj; unfold h5i_app_std.map.get_loop.body; h5i_step [Prod.ext_iff]

variable (kinst : core.clone.Clone K) [CloneLaw kinst]

h5i_when h5i_app_std.map.remove =>
@[step] theorem map.remove_spec (m : alloc.vec.Vec (K × V)) (k : K) :
    h5i_app_std.map.remove inst kinst vinst m k ⦃ v => v.val = mapRemove k m.val ⦄ := by
  unfold h5i_app_std.map.remove h5i_app_std.map.remove_loop
  apply WP.spec_mono (loop_fold m.val (fun o : alloc.vec.Vec (K × V) => o.val)
    (fun o e => if decide (e.1 ≠ k) then o ++ [e] else o) (fun o j => o.length ≤ j)
    (fun st => h5i_app_std.map.remove_loop.body inst kinst vinst m k st.1 st.2) ?_ (alloc.vec.Vec.new _) 0#usize (by simp) (by simp))
  · intro v hv; rw [hv, foldl_filter]; simp [mapRemove]
  · intro o i hi hinv
    unfold h5i_app_std.map.remove_loop.body
    h5i_step [Prod.ext_iff]

h5i_when h5i_app_std.map.insert =>
@[step] theorem map.insert_spec (m : alloc.vec.Vec (K × V)) (k : K) (v : V) (h : m.val.length < Usize.max) :
    h5i_app_std.map.insert inst kinst vinst m k v ⦃ w => w.val = mapInsert k v m.val ⦄ := by
  unfold h5i_app_std.map.insert h5i_app_std.map.insert_loop
  step*
  rw [out_post]
  apply WP.spec_mono (loop_search m.val (fun e => decide (e.1 = k)) (fun w : alloc.vec.Vec (K × V) => w.val)
    (fun j _ => m.val.set j (k, v)) (m.val ++ [(k, v)]) _ ?_ 0#usize (by simp))
  · intro w hw
    rw [hw, searchFrom_upsert _ _ _ _ (by simp)]; simp [mapInsert]
  · intro j hj; unfold h5i_app_std.map.insert_loop.body; h5i_step [Prod.ext_iff]

end Maps


/-! ## Hash maps

A `HashMap` stands for the map `hashmap.lookup`, read off its entries
(`hashmap.entries`) with the list-map model above. `hashmap.Inv f m` says
each key sits once, in bucket `key_hash k % n`; every operation keeps it.
`HashModel f` says the extracted `key_hash` never fails, and is all a spec
asks of the hash: nothing about collisions, which only make buckets longer. -/

h5i_when h5i_app_std.hashmap.KeyHash =>
/-- The extracted hash `f` never fails, and computes `hash`. -/
class HashModel {K : Type} (f : h5i_app_std.hashmap.KeyHash K) where
  hash : K → U64
  ok : ∀ k, f.key_hash k = ok (hash k)

h5i_when h5i_app_std.hashmap.KeyHash =>
/-- The hash of `k` as a number. -/
abbrev hashOf {K : Type} (f : h5i_app_std.hashmap.KeyHash K) [HashModel f] (k : K) : Nat :=
  (HashModel.hash f k).val

h5i_when h5i_app_std.hashmap.KeyHash =>
@[step] theorem HashModel.spec {K : Type} (f : h5i_app_std.hashmap.KeyHash K) [HashModel f] (k : K) :
    f.key_hash k ⦃ h => h = HashModel.hash f k ⦄ := by
  rw [HashModel.ok]; simp

h5i_when h5i_app_std.hashmap.KeyHash =>
/-- A `HashModel` for any `key_hash` that never fails. For a kernel's own
`impl KeyHash`: `instance : HashModel T.Insts.H5i_app_stdHashmapKeyHash :=
HashModel.ofTotal _ fun k => by unfold ...; step*`. -/
@[reducible] noncomputable def HashModel.ofTotal {K : Type} (f : h5i_app_std.hashmap.KeyHash K)
    (h : ∀ k, f.key_hash k ⦃ _ => True ⦄) : HashModel f where
  hash k := Classical.choose ((WP.spec_equiv_exists _ _).1 (h k))
  ok k := (Classical.choose_spec ((WP.spec_equiv_exists _ _).1 (h k))).1

h5i_when U8.Insts.H5i_app_stdHashmapKeyHash =>
instance : HashModel U8.Insts.H5i_app_stdHashmapKeyHash := ⟨fun x => UScalar.cast .U64 x, fun _ => rfl⟩
h5i_when U8.Insts.H5i_app_stdHashmapKeyHash =>
@[step] theorem u8_key_hash_spec (x : U8) :
    U8.Insts.H5i_app_stdHashmapKeyHash.key_hash x ⦃ h => h = UScalar.cast .U64 x ⦄ := by
  simp [U8.Insts.H5i_app_stdHashmapKeyHash.key_hash]
h5i_when U32.Insts.H5i_app_stdHashmapKeyHash =>
instance : HashModel U32.Insts.H5i_app_stdHashmapKeyHash := ⟨fun x => UScalar.cast .U64 x, fun _ => rfl⟩
h5i_when U32.Insts.H5i_app_stdHashmapKeyHash =>
@[step] theorem u32_key_hash_spec (x : U32) :
    U32.Insts.H5i_app_stdHashmapKeyHash.key_hash x ⦃ h => h = UScalar.cast .U64 x ⦄ := by
  simp [U32.Insts.H5i_app_stdHashmapKeyHash.key_hash]
h5i_when U64.Insts.H5i_app_stdHashmapKeyHash =>
instance : HashModel U64.Insts.H5i_app_stdHashmapKeyHash := ⟨fun x => x, fun _ => rfl⟩
h5i_when U64.Insts.H5i_app_stdHashmapKeyHash =>
@[step] theorem u64_key_hash_spec (x : U64) :
    U64.Insts.H5i_app_stdHashmapKeyHash.key_hash x ⦃ h => h = x ⦄ := by
  simp [U64.Insts.H5i_app_stdHashmapKeyHash.key_hash]
h5i_when Usize.Insts.H5i_app_stdHashmapKeyHash =>
instance : HashModel Usize.Insts.H5i_app_stdHashmapKeyHash := ⟨fun x => UScalar.cast .U64 x, fun _ => rfl⟩
h5i_when Usize.Insts.H5i_app_stdHashmapKeyHash =>
@[step] theorem usize_key_hash_spec (x : Usize) :
    Usize.Insts.H5i_app_stdHashmapKeyHash.key_hash x ⦃ h => h = UScalar.cast .U64 x ⦄ := by
  simp [Usize.Insts.H5i_app_stdHashmapKeyHash.key_hash]
h5i_when Bool.Insts.H5i_app_stdHashmapKeyHash =>
instance : HashModel Bool.Insts.H5i_app_stdHashmapKeyHash :=
  ⟨fun b => if b then 1#u64 else 0#u64, fun b => by cases b <;> rfl⟩
h5i_when Bool.Insts.H5i_app_stdHashmapKeyHash =>
@[step] theorem bool_key_hash_spec (b : Bool) :
    Bool.Insts.H5i_app_stdHashmapKeyHash.key_hash b ⦃ h => h = if b then 1#u64 else 0#u64 ⦄ := by
  cases b <;> simp [Bool.Insts.H5i_app_stdHashmapKeyHash.key_hash]

/-- FNV-1a over `l`, from `h`: what `Vec<u8>`'s `key_hash` computes. -/
def fnv1a (h : U64) (l : List U8) : U64 :=
  l.foldl (fun h x => core.num.U64.wrapping_mul (h ^^^ UScalar.cast .U64 x) 1099511628211#u64) h

h5i_when alloc.vec.VecU8.Insts.H5i_app_stdHashmapKeyHash =>
@[step] theorem vec_u8_key_hash_spec (v : alloc.vec.Vec U8) :
    alloc.vec.VecU8.Insts.H5i_app_stdHashmapKeyHash.key_hash v ⦃ h => h = fnv1a 14695981039346656037#u64 v.val ⦄ := by
  unfold alloc.vec.VecU8.Insts.H5i_app_stdHashmapKeyHash.key_hash
    alloc.vec.VecU8.Insts.H5i_app_stdHashmapKeyHash.key_hash_loop
  apply WP.spec_mono (loop_fold v.val id
    (fun h x => core.num.U64.wrapping_mul (h ^^^ UScalar.cast .U64 x) 1099511628211#u64) (fun _ _ => True)
    (fun st => alloc.vec.VecU8.Insts.H5i_app_stdHashmapKeyHash.key_hash_loop.body v st.1 st.2) ?_
    14695981039346656037#u64 0#usize (by simp) trivial)
  · intro h hh; simpa [fnv1a] using hh
  · intro h i hi _
    unfold alloc.vec.VecU8.Insts.H5i_app_stdHashmapKeyHash.key_hash_loop.body
    h5i_step
    refine ⟨by scalar_tac, ?_⟩
    congr 1
    apply UScalar.eq_of_val_eq
    rw [i4_post]
    simp [UScalar.cast_val_eq]
    apply (Nat.mod_eq_of_lt _).symm
    have := (v.val[i.val]'(by scalar_tac)).hBounds; simp at this; omega

h5i_when alloc.vec.VecU8.Insts.H5i_app_stdHashmapKeyHash =>
instance : HashModel alloc.vec.VecU8.Insts.H5i_app_stdHashmapKeyHash :=
  ⟨fun v => fnv1a 14695981039346656037#u64 v.val, fun v => eq_ok_of_spec (vec_u8_key_hash_spec v)⟩

section HashMap
variable {K V : Type} [DecidableEq K] (inst : core.cmp.PartialEq K K) [EqLaw inst]

h5i_when h5i_app_std.hashmap.bucket_of =>
omit [DecidableEq K] in
@[step] theorem hashmap.bucket_of_spec (f : h5i_app_std.hashmap.KeyHash K) [HashModel f] (n : Usize) (k : K) (hn : 0 < n.val) :
    h5i_app_std.hashmap.bucket_of f n k ⦃ r => r.val = hashOf f k % n.val ⦄ := by
  unfold h5i_app_std.hashmap.bucket_of
  have hn' : (UScalar.cast .U64 n).val = n.val := usize_cast_u64 n
  step*
  subst i_post i1_post
  rw [hn'] at i2_post
  have hlt : i2.val < n.val := by rw [i2_post]; exact Nat.mod_lt _ hn
  rw [u64_cast_usize i2 (by scalar_tac), i2_post]

h5i_when h5i_app_std.hashmap.find =>
@[step] theorem hashmap.find_spec (b : alloc.vec.Vec (K × V)) (k : K) :
    h5i_app_std.hashmap.find inst b k ⦃ r => r.map (·.val) = b.val.findIdx? (fun e => decide (e.1 = k)) ⦄ := by
  unfold h5i_app_std.hashmap.find h5i_app_std.hashmap.find_loop
  apply WP.spec_mono (loop_search b.val (fun e => decide (e.1 = k)) (fun r : Option Usize => r.map (·.val))
    (fun i _ => some i) none _ ?_ 0#usize (by simp))
  · intro r hr; exact hr.trans (search_findIdx _ _ _ rfl)
  · intro j hj; unfold h5i_app_std.hashmap.find_loop.body; h5i_step [Prod.ext_iff]

h5i_when h5i_app_std.hashmap.KeyHash =>
h5i_when h5i_app_std.hashmap.HashMap =>
/-- The entries of `m`, bucket after bucket. -/
def hashmap.entries (m : h5i_app_std.hashmap.HashMap K V) : List (K × V) :=
  (m.buckets.val.map (·.val)).flatten

h5i_when h5i_app_std.hashmap.KeyHash =>
h5i_when h5i_app_std.hashmap.HashMap =>
/-- `m.get(q)`: the map `m` stands for. -/
abbrev hashmap.lookup (m : h5i_app_std.hashmap.HashMap K V) (q : K) : Option V :=
  mapGet q (hashmap.entries m)

h5i_when h5i_app_std.hashmap.KeyHash =>
h5i_when h5i_app_std.hashmap.HashMap =>
/-- The bucket each key of `m` belongs in, by hash `f`. -/
abbrev hashmap.slot (f : h5i_app_std.hashmap.KeyHash K) [HashModel f] (m : h5i_app_std.hashmap.HashMap K V) (k : K) : Nat :=
  hashOf f k % m.buckets.val.length

h5i_when h5i_app_std.hashmap.KeyHash =>
h5i_when h5i_app_std.hashmap.HashMap =>
/-- `m` is a map built with hash `f`: each key sits once, in its bucket, and
`len` counts the entries. -/
structure hashmap.Inv (f : h5i_app_std.hashmap.KeyHash K) [HashModel f] (m : h5i_app_std.hashmap.HashMap K V) : Prop where
  pos : 0 < m.buckets.val.length
  buckets : BucketsInv (hashmap.slot f m) (m.buckets.val.map (·.val))
  len : m.len.val = (hashmap.entries m).length

h5i_when h5i_app_std.hashmap.KeyHash =>
h5i_when h5i_app_std.hashmap.HashMap =>
theorem hashmap.Inv.lookup_eq (f : h5i_app_std.hashmap.KeyHash K) [HashModel f] {m : h5i_app_std.hashmap.HashMap K V} (hm : hashmap.Inv f m) (q : K) :
    hashmap.lookup m q = mapGet q (m.buckets.val[hashmap.slot f m q]'(Nat.mod_lt _ hm.pos)).val := by
  have := hm.buckets.mapGet_flatten q (by simpa using Nat.mod_lt (hashOf f q) hm.pos)
  simpa [hashmap.lookup, hashmap.entries] using this

h5i_when h5i_app_std.hashmap.KeyHash =>
h5i_when h5i_app_std.hashmap.HashMap =>
/-- Bucket `i` is the slot of `k`: looking `k` up reads only that bucket. -/
theorem hashmap.Inv.lookup_at (f : h5i_app_std.hashmap.KeyHash K) [HashModel f] {m : h5i_app_std.hashmap.HashMap K V} (hm : hashmap.Inv f m) {k : K} {i : Usize}
    (hi : i.val = hashOf f k % m.buckets.val.length) :
    ∃ h : i.val < m.buckets.val.length, hashmap.lookup m k = mapGet k (m.buckets.val[i.val]).val := by
  have h : i.val < m.buckets.val.length := by rw [hi]; exact Nat.mod_lt _ hm.pos
  refine ⟨h, ?_⟩
  rw [hm.lookup_eq]; simp only [hashmap.slot, ← hi]

h5i_when h5i_app_std.hashmap.KeyHash =>
h5i_when h5i_app_std.hashmap.HashMap =>
h5i_when h5i_app_std.hashmap.HashMap.get =>
@[step] theorem hashmap.get_spec (f : h5i_app_std.hashmap.KeyHash K) [HashModel f] (m : h5i_app_std.hashmap.HashMap K V) (k : K) (hm : hashmap.Inv f m) :
    h5i_app_std.hashmap.HashMap.get inst f m k ⦃ r => r = hashmap.lookup m k ⦄ := by
  unfold h5i_app_std.hashmap.HashMap.get
  have hpos := hm.pos
  step*
  all_goals
    subst_vars
    obtain ⟨_, hl⟩ := hm.lookup_at f (k := k) (i := i1) (by simpa using i1_post)
  · obtain ⟨hg, -⟩ := mapGet_of_findIdx_none (by simpa using o_post.symm)
    rw [hl, hg]
  · obtain ⟨hi, -⟩ := mapGet_of_findIdx (by simpa using o_post.symm); exact hi
  · obtain ⟨hi, -, hg, -⟩ := mapGet_of_findIdx (by simpa using o_post.symm)
    rw [hl, hg]; simp [← __post]
h5i_when h5i_app_std.hashmap.KeyHash =>
h5i_when h5i_app_std.hashmap.HashMap =>
omit [DecidableEq K] in
/-- A bucket holds at most `len` entries. -/
theorem hashmap.Inv.bucket_le (f : h5i_app_std.hashmap.KeyHash K) [HashModel f] {m : h5i_app_std.hashmap.HashMap K V} (hm : hashmap.Inv f m) (i : Nat)
    (hi : i < m.buckets.val.length) : (m.buckets.val[i]).val.length ≤ m.len.val := by
  have h := congrArg List.length (flatten_split (m.buckets.val.map (·.val)) i (by simpa using hi))
  have h2 := hm.len
  simp only [hashmap.entries] at h2
  simp only [List.length_append, List.getElem_map] at h
  omega

h5i_when h5i_app_std.hashmap.KeyHash =>
h5i_when h5i_app_std.hashmap.HashMap =>
/-- Replacing the bucket of `k` by `x`, which keeps the bucket's slot and its
entries at other keys, keeps the invariant and changes the map only at `k`. -/
theorem hashmap.Inv.update (f : h5i_app_std.hashmap.KeyHash K) [HashModel f] {m : h5i_app_std.hashmap.HashMap K V} (hm : hashmap.Inv f m) {k : K} {b : Usize}
    (hb : b.val = hashOf f k % m.buckets.val.length) (hbl : b.val < m.buckets.val.length)
    (x : alloc.vec.Vec (K × V)) (hx : ∀ e ∈ x.val, hashOf f e.1 % m.buckets.val.length = b.val)
    (hnd : (x.val.map Prod.fst).Nodup)
    (hother : ∀ q, q ≠ k → mapGet q x.val = mapGet q (m.buckets.val[b.val]).val)
    (len' : Usize) (hlen' : len'.val + (m.buckets.val[b.val]).val.length = m.len.val + x.val.length) :
    hashmap.Inv f ⟨m.buckets.set b x, len'⟩ ∧
      ∀ q, hashmap.lookup ⟨m.buckets.set b x, len'⟩ q = if q = k then mapGet k x.val else hashmap.lookup m q := by
  have hset : (m.buckets.set b x).val.map (·.val) = (m.buckets.val.map (·.val)).set b.val x.val := by
    simp [List.map_set]
  have hn : (m.buckets.set b x).val.length = m.buckets.val.length := by simp
  have hbB : b.val < (m.buckets.val.map (·.val)).length := by simpa using hbl
  have hent : hashmap.entries ⟨m.buckets.set b x, len'⟩ = ((m.buckets.val.map (·.val)).set b.val x.val).flatten := by
    simp only [hashmap.entries, hset]
  refine ⟨⟨by rw [hn]; exact hm.pos, ?_, ?_⟩, ?_⟩
  · have hs : hashmap.slot f (⟨m.buckets.set b x, len'⟩ : h5i_app_std.hashmap.HashMap K V) = hashmap.slot f m := by
      funext q; simp only [hashmap.slot, hn]
    rw [hs, hset]; exact hm.buckets.set x.val hx hnd
  · rw [hent]
    have h1 := length_flatten_set _ _ hbB x.val
    have h2 := hm.len
    simp only [hashmap.entries] at h2
    have h3 : ((m.buckets.val.map (fun y : alloc.vec.Vec (K × V) => y.val))[b.val]'hbB).length =
        (m.buckets.val[b.val]).val.length := by simp
    show len'.val = _
    omega
  · intro q
    simp only [hashmap.lookup] at *
    rw [hent, hm.buckets.mapGet_flatten_set hbB x.val hx q]
    by_cases hq : hashmap.slot f m q = b.val
    · rw [if_pos hq]
      by_cases hqk : q = k
      · subst hqk; simp
      · rw [if_neg hqk, hother q hqk]
        have := hm.lookup_eq f q
        simp only [hashmap.lookup, hashmap.entries] at this ⊢
        rw [this]; simp only [hq]
    · rw [if_neg hq]
      have hqk : q ≠ k := by rintro rfl; exact hq hb.symm
      rw [if_neg hqk]; rfl

h5i_when h5i_app_std.hashmap.KeyHash =>
h5i_when h5i_app_std.hashmap.HashMap =>
/-- `update` with `x` the bucket after `mapInsert k v`. -/
theorem hashmap.Inv.insert_at (f : h5i_app_std.hashmap.KeyHash K) [HashModel f] {m : h5i_app_std.hashmap.HashMap K V} (hm : hashmap.Inv f m) {k : K} {b : Usize}
    (hb : b.val = hashOf f k % m.buckets.val.length) (v : V) (x : alloc.vec.Vec (K × V))
    (hx : x.val = mapInsert k v (m.buckets.val[b.val]'(by rw [hb]; exact Nat.mod_lt _ hm.pos)).val)
    (len' : Usize) (hlen' : len'.val = m.len.val + if (hashmap.lookup m k).isSome then 0 else 1) :
    hashmap.Inv f ⟨m.buckets.set b x, len'⟩ ∧
      ∀ q, hashmap.lookup ⟨m.buckets.set b x, len'⟩ q = if q = k then some v else hashmap.lookup m q := by
  obtain ⟨hbl, hl⟩ := hm.lookup_at f hb
  have hI := hm.buckets
  have hslot : ∀ e ∈ (m.buckets.val[b.val]).val, hashOf f e.1 % m.buckets.val.length = b.val := by
    intro e he; have := hI.slot b.val (by simpa using hbl) e (by simpa using he); simpa using this
  have hnd : ((m.buckets.val[b.val]).val.map Prod.fst).Nodup := by
    have := hI.nodup b.val (by simpa using hbl); simpa using this
  have := hm.update f hb hbl x ?_ ?_ ?_ len' ?_
  · refine ⟨this.1, fun q => ?_⟩; rw [this.2 q, hx]; simp
  · intro e he; rw [hx] at he
    rcases mem_mapInsert he with rfl | he
    · exact hb.symm
    · exact hslot e he
  · rw [hx]; exact nodup_keys_mapInsert hnd
  · intro q hq; rw [hx]; simp [hq]
  · rw [hlen', hx, hl]
    have hk := congrArg List.length (keys_mapInsert k v (m.buckets.val[b.val]).val)
    simp only [List.length_map] at hk
    rw [hk]
    have := mapGet_isSome_iff (m.buckets.val[b.val]).val k
    by_cases hin : k ∈ (m.buckets.val[b.val]).val.map Prod.fst
    · have : (mapGet k (m.buckets.val[b.val]).val).isSome := this.2 (by simpa using hin)
      simp [hin, this]
    · have : (mapGet k (m.buckets.val[b.val]).val).isSome = false := by
        rw [Bool.eq_false_iff]; intro h; exact hin (by simpa using this.1 h)
      simp [hin, this]; omega

h5i_when h5i_app_std.hashmap.KeyHash =>
h5i_when h5i_app_std.hashmap.HashMap =>
h5i_when h5i_app_std.hashmap.HashMap.insert =>
@[step] theorem hashmap.insert_spec (f : h5i_app_std.hashmap.KeyHash K) [HashModel f] (m : h5i_app_std.hashmap.HashMap K V) (k : K) (v : V)
    (hm : hashmap.Inv f m) (hlen : m.len.val < Usize.max) :
    h5i_app_std.hashmap.HashMap.insert inst f m k v ⦃ r =>
      r.1 = hashmap.lookup m k ∧ hashmap.Inv f r.2 ∧
      (∀ q, hashmap.lookup r.2 q = if q = k then some v else hashmap.lookup m q) ∧
      r.2.len.val = m.len.val + (if (hashmap.lookup m k).isSome then 0 else 1) ⦄ := by
  unfold h5i_app_std.hashmap.HashMap.insert
  have hpos := hm.pos
  have hbl := fun i hi => hm.bucket_le f i hi
  step*
  all_goals subst_vars
  all_goals have hb' : b.val = hashOf f k % m.buckets.val.length := by simpa using b_post
  all_goals obtain ⟨hbl', hl⟩ := hm.lookup_at f hb'
  · obtain ⟨hg, hu⟩ := mapGet_of_findIdx_none (by simpa using o_post.symm)
    have := hm.insert_at f hb' v v3 (by rw [v3_post, hu]) i1 (by rw [i1_post, hl, hg]; simp)
    exact ⟨by rw [hl, hg], this.1, this.2, by rw [i1_post, hl, hg]; simp⟩
  · obtain ⟨hi, -⟩ := mapGet_of_findIdx (ν := V) (by simpa using o_post.symm); exact hi
  · obtain ⟨hi, hk, hg, hu⟩ := mapGet_of_findIdx (by simpa using o_post.symm)
    have ht : t = ((m.buckets.val[b.val]).val[i1.val]).2 := by rw [← __post]
    have := hm.insert_at f hb' v ((m.buckets.val[b.val]).set i1 (k, v)) (by simp [hu]) m.len (by rw [hl, hg]; simp)
    exact ⟨by rw [hl, hg, ht], this.1, this.2, by rw [hl, hg]; simp⟩
h5i_when h5i_app_std.hashmap.KeyHash =>
h5i_when h5i_app_std.hashmap.HashMap =>
h5i_when h5i_app_std.hashmap.HashMap.contains_key =>
@[step] theorem hashmap.contains_key_spec (f : h5i_app_std.hashmap.KeyHash K) [HashModel f] (m : h5i_app_std.hashmap.HashMap K V) (k : K) (hm : hashmap.Inv f m) :
    h5i_app_std.hashmap.HashMap.contains_key inst f m k ⦃ r => r = (hashmap.lookup m k).isSome ⦄ := by
  unfold h5i_app_std.hashmap.HashMap.contains_key
  have hpos := hm.pos
  step*
  subst_vars
  obtain ⟨_, hl⟩ := hm.lookup_at f (k := k) (i := i1) (by simpa using i1_post)
  rw [hl]
  rcases o with _ | j
  · obtain ⟨hg, -⟩ := mapGet_of_findIdx_none (by simpa using o_post.symm)
    rw [hg]; rfl
  · obtain ⟨hi, -, hg, -⟩ := mapGet_of_findIdx (by simpa using o_post.symm)
    rw [hg]; rfl

h5i_when h5i_app_std.hashmap.KeyHash =>
h5i_when h5i_app_std.hashmap.HashMap =>
/-- `update` with `x` the bucket after `mapRemove k`, when `k` is there. -/
theorem hashmap.Inv.remove_at (f : h5i_app_std.hashmap.KeyHash K) [HashModel f] {m : h5i_app_std.hashmap.HashMap K V} (hm : hashmap.Inv f m) {k : K} {b : Usize}
    (hb : b.val = hashOf f k % m.buckets.val.length) (hk : (hashmap.lookup m k).isSome) (x : alloc.vec.Vec (K × V))
    (hx : x.val = mapRemove k (m.buckets.val[b.val]'(by rw [hb]; exact Nat.mod_lt _ hm.pos)).val)
    (len' : Usize) (hlen' : len'.val + 1 = m.len.val) :
    hashmap.Inv f ⟨m.buckets.set b x, len'⟩ ∧
      ∀ q, hashmap.lookup ⟨m.buckets.set b x, len'⟩ q = if q = k then none else hashmap.lookup m q := by
  obtain ⟨hbl, hl⟩ := hm.lookup_at f hb
  have hI := hm.buckets
  have hslot : ∀ e ∈ (m.buckets.val[b.val]).val, hashOf f e.1 % m.buckets.val.length = b.val := by
    intro e he; have := hI.slot b.val (by simpa using hbl) e (by simpa using he); simpa using this
  have hnd : ((m.buckets.val[b.val]).val.map Prod.fst).Nodup := by
    have := hI.nodup b.val (by simpa using hbl); simpa using this
  have := hm.update f hb hbl x ?_ ?_ ?_ len' ?_
  · refine ⟨this.1, fun q => ?_⟩; rw [this.2 q, hx]; simp
  · intro e he; rw [hx] at he; exact hslot e (List.mem_of_mem_filter he)
  · rw [hx]; exact nodup_keys_mapRemove hnd
  · intro q hq; rw [hx]; simp [hq]
  · rw [hl] at hk
    have := length_mapRemove_of_nodup hnd hk
    rw [hx]; omega

variable (kinst : core.clone.Clone K) [CloneLaw kinst] (vinst : core.clone.Clone V) [CloneLaw vinst]

h5i_when h5i_app_std.hashmap.KeyHash =>
h5i_when h5i_app_std.hashmap.HashMap =>
h5i_when h5i_app_std.hashmap.HashMap.remove =>
@[step] theorem hashmap.remove_spec (f : h5i_app_std.hashmap.KeyHash K) [HashModel f] (m : h5i_app_std.hashmap.HashMap K V) (k : K) (hm : hashmap.Inv f m) :
    h5i_app_std.hashmap.HashMap.remove inst f kinst vinst m k ⦃ r =>
      r.1 = hashmap.lookup m k ∧ hashmap.Inv f r.2 ∧
      (∀ q, hashmap.lookup r.2 q = if q = k then none else hashmap.lookup m q) ∧
      r.2.len.val + (if (hashmap.lookup m k).isSome then 1 else 0) = m.len.val ⦄ := by
  unfold h5i_app_std.hashmap.HashMap.remove
  have hpos := hm.pos
  have hbl := fun i hi => hm.bucket_le f i hi
  step*
  all_goals
    try simp only [Prod.ext_iff] at *
    try casesm* _ ∧ _
    subst_vars
    have hb' : b.val = hashOf f k % m.buckets.val.length := by simpa using b_post
    obtain ⟨hbl', hl⟩ := hm.lookup_at f hb'
  · obtain ⟨hg, -⟩ := mapGet_of_findIdx_none (by simpa using o_post.symm)
    refine ⟨by rw [hl, hg], hm, fun q => ?_, by simp [hl, hg]⟩
    split <;> simp_all
  · obtain ⟨hi, -⟩ := mapGet_of_findIdx (ν := V) (by simpa using o_post.symm); exact hi
  · obtain ⟨hi, -⟩ := mapGet_of_findIdx (ν := V) (by simpa using o_post.symm)
    have h1 := hbl b.val hbl'
    have : (1#usize).val = 1 := rfl
    omega
  · obtain ⟨hi, -, hg, -⟩ := mapGet_of_findIdx (by simpa using o_post.symm)
    have hlk := hl.trans hg
    have hk : (hashmap.lookup m k).isSome := by rw [hlk]; rfl
    have := hm.remove_at f hb' hk out (by rw [out_post]) i2 (by omega)
    exact ⟨hlk.symm, this.1, this.2, by simp [hk]; omega⟩

h5i_when h5i_app_std.hashmap.KeyHash =>
h5i_when h5i_app_std.hashmap.HashMap =>
omit [DecidableEq K] in
/-- Empty buckets, at least one: an empty map for every hash. -/
theorem hashmap.empty_inv (m : h5i_app_std.hashmap.HashMap K V) (n : Nat) (hn : 0 < n)
    (hb : m.buckets.val = List.replicate n (alloc.vec.Vec.new (K × V))) (hl : m.len.val = 0) :
    (∀ (g : h5i_app_std.hashmap.KeyHash K) [HashModel g], hashmap.Inv g m) ∧ hashmap.entries m = [] ∧
      m.len.val = 0 := by
  have he : hashmap.entries m = [] := by simp [hashmap.entries, hb]
  refine ⟨fun g _ => ⟨by simp [hb, hn], ⟨fun i hi e he => ?_, fun i hi => ?_⟩, by rw [he, hl]; rfl⟩, he, hl⟩
  · simp [hb] at he
  · simp [hb]

h5i_when h5i_app_std.hashmap.KeyHash =>
h5i_when h5i_app_std.hashmap.HashMap =>
h5i_when h5i_app_std.hashmap.HashMap.with_capacity =>
omit [DecidableEq K] in
@[step] theorem hashmap.with_capacity_loop_spec (nb : Usize) :
    h5i_app_std.hashmap.HashMap.with_capacity_loop (K := K) (V := V) nb (alloc.vec.Vec.new _) 0#usize ⦃ bs =>
      bs.val = List.replicate nb.val (alloc.vec.Vec.new (K × V)) ⦄ := by
  unfold h5i_app_std.hashmap.HashMap.with_capacity_loop
  apply WP.spec_mono (loop_fold (List.replicate nb.val ()) (fun o : alloc.vec.Vec (alloc.vec.Vec (K × V)) => o.val)
    (fun acc _ => acc ++ [alloc.vec.Vec.new (K × V)]) (fun o j => o.val.length = j)
    (fun st => h5i_app_std.hashmap.HashMap.with_capacity_loop.body nb st.1 st.2) ?_ (alloc.vec.Vec.new _) 0#usize
    (by simp) (by simp))
  · intro bs h; rw [h, foldl_map (fun _ => alloc.vec.Vec.new (K × V))]; simp
  · intro o i hi hinv
    unfold h5i_app_std.hashmap.HashMap.with_capacity_loop.body
    h5i_step

h5i_when h5i_app_std.hashmap.KeyHash =>
h5i_when h5i_app_std.hashmap.HashMap =>
h5i_when h5i_app_std.hashmap.HashMap.with_capacity =>
omit [DecidableEq K] in
@[step] theorem hashmap.with_capacity_spec (n : Usize) :
    h5i_app_std.hashmap.HashMap.with_capacity K V n ⦃ m =>
      (∀ (g : h5i_app_std.hashmap.KeyHash K) [HashModel g], hashmap.Inv g m) ∧ hashmap.entries m = [] ∧
        m.len.val = 0 ⦄ := by
  unfold h5i_app_std.hashmap.HashMap.with_capacity
  h5i_steps
  all_goals exact hashmap.empty_inv _ _ (by scalar_tac) buckets_post rfl

h5i_when h5i_app_std.hashmap.KeyHash =>
h5i_when h5i_app_std.hashmap.HashMap =>
h5i_when h5i_app_std.hashmap.HashMap.new =>
omit [DecidableEq K] in
@[step] theorem hashmap.new_spec :
    h5i_app_std.hashmap.HashMap.new K V ⦃ m =>
      (∀ (g : h5i_app_std.hashmap.KeyHash K) [HashModel g], hashmap.Inv g m) ∧ hashmap.entries m = [] ∧
        m.len.val = 0 ⦄ := by
  unfold h5i_app_std.hashmap.HashMap.new; step*

h5i_when h5i_app_std.hashmap.KeyHash =>
h5i_when h5i_app_std.hashmap.HashMap =>
h5i_when h5i_app_std.hashmap.HashMap.Insts.CoreDefaultDefault.default =>
omit [DecidableEq K] in
@[step] theorem hashmap.default_spec :
    h5i_app_std.hashmap.HashMap.Insts.CoreDefaultDefault.default K V ⦃ m =>
      (∀ (g : h5i_app_std.hashmap.KeyHash K) [HashModel g], hashmap.Inv g m) ∧ hashmap.entries m = [] ∧
        m.len.val = 0 ⦄ := by
  unfold h5i_app_std.hashmap.HashMap.Insts.CoreDefaultDefault.default; step*

h5i_when h5i_app_std.hashmap.KeyHash =>
h5i_when h5i_app_std.hashmap.HashMap =>
h5i_when h5i_app_std.hashmap.HashMap.impl.len =>
omit [DecidableEq K] in
@[step] theorem hashmap.len_spec (m : h5i_app_std.hashmap.HashMap K V) :
    h5i_app_std.hashmap.HashMap.impl.len m ⦃ r => r = m.len ⦄ := by
  unfold h5i_app_std.hashmap.HashMap.impl.len; simp

h5i_when h5i_app_std.hashmap.KeyHash =>
h5i_when h5i_app_std.hashmap.HashMap =>
omit [DecidableEq K] in
/-- No key appears twice among the entries, so `len` counts the keys. -/
theorem hashmap.Inv.nodup (f : h5i_app_std.hashmap.KeyHash K) [HashModel f] {m : h5i_app_std.hashmap.HashMap K V} (hm : hashmap.Inv f m) :
    ((hashmap.entries m).map Prod.fst).Nodup :=
  hm.buckets.nodup_keys

h5i_when h5i_app_std.hashmap.KeyHash =>
h5i_when h5i_app_std.hashmap.HashMap =>
h5i_when h5i_app_std.hashmap.HashMap.is_empty =>
omit [DecidableEq K] in
@[step] theorem hashmap.is_empty_spec (f : h5i_app_std.hashmap.KeyHash K) [HashModel f] (m : h5i_app_std.hashmap.HashMap K V) (hm : hashmap.Inv f m) :
    h5i_app_std.hashmap.HashMap.is_empty m ⦃ r => r = decide (hashmap.entries m = []) ⦄ := by
  unfold h5i_app_std.hashmap.HashMap.is_empty
  have := hm.len
  simp only [WP.spec_ok, decide_eq_decide, ← List.length_eq_zero_iff, ← this]
  constructor
  · intro h; rw [h]; rfl
  · intro h; scalar_tac

h5i_when h5i_app_std.hashmap.KeyHash =>
h5i_when h5i_app_std.hashmap.HashMap =>
h5i_when h5i_app_std.hashmap.HashMap.from_vec =>
@[step] theorem hashmap.from_vec_spec (f : h5i_app_std.hashmap.KeyHash K) [HashModel f] (entries : alloc.vec.Vec (K × V)) :
    h5i_app_std.hashmap.HashMap.from_vec inst f kinst vinst entries ⦃ m =>
      hashmap.Inv f m ∧ ∀ q, hashmap.lookup m q = mapGet q entries.val.reverse ⦄ := by
  unfold h5i_app_std.hashmap.HashMap.from_vec h5i_app_std.hashmap.HashMap.from_vec_loop
  step*
  rename_i hm0
  apply loop_idx_spec _ (fun x => x.2) entries.val.length
    (fun x => hashmap.Inv f x.1 ∧ x.1.len.val ≤ x.2.val ∧ ∀ q, hashmap.lookup x.1 q = mapGet q (entries.val.take x.2.val).reverse)
    _ ?_ _ ⟨m_post f, by simp [m_post2],
      fun q => by simp [hashmap.lookup, m_post1]⟩ (by simp)
  rintro ⟨mm, i⟩ ⟨hI, hle, hlk⟩ hi
  unfold h5i_app_std.hashmap.HashMap.from_vec_loop.body
  step*
  · simp only at hI hle hlk hi
    have hil : i.val < entries.val.length := by scalar_tac
    subst t2_post t3_post
    refine ⟨__post1, ?_, fun q => ?_, by omega, by scalar_tac⟩
    · rw [__post3, i2_post]; split <;> omega
    · rw [__post2, i2_post, List.take_add_one, List.getElem?_eq_getElem hil, ← t_post]
      by_cases hq : q = t2
      · subst hq; simp [mapGet_cons]
      · simp [hq, mapGet_cons, Ne.symm hq, hlk]
  · simp only at hI hle hlk hi
    have : i.val = entries.val.length := by scalar_tac
    refine ⟨hI, fun q => ?_⟩
    rw [hlk, this, List.take_length]

end HashMap

/-! ## Graphs -/

section Graph
variable {T : Type} [DecidableEq T] (inst : core.cmp.PartialEq T T) [EqLaw inst]
  (cinst : core.clone.Clone T) [CloneLaw cinst]

/-- The nodes a search can add: start nodes and edge targets. -/
abbrev graphNodes (edges : List (T × T)) (start : List T) : List T := start ++ edges.map (·.2)

omit [DecidableEq T] in
theorem length_le_of_nodup_sub {l n : List T} (h : l.Nodup) (hs : l ⊆ n) : l.length ≤ n.length :=
  (h.subperm hs).length_le

omit [DecidableEq T] in
theorem mem_graphNodes_of_edge {edges : List (T × T)} {start : List T} {e : T × T} (he : e ∈ edges) :
    e.2 ∈ graphNodes edges start := by
  simp only [graphNodes, List.mem_append, List.mem_map]; exact .inr ⟨e, he, rfl⟩

omit [DecidableEq T] in
/-- Room for one more node: `o` misses a node of `n`. -/
theorem length_lt_of_nodup_sub {l n : List T} {y : T} (h : l.Nodup) (hs : l ⊆ n) (hy : y ∈ n) (hny : y ∉ l) :
    l.length < n.length := by
  have := length_le_of_nodup_sub (List.nodup_cons.2 ⟨hny, h⟩) (List.cons_subset.2 ⟨hy, hs⟩)
  simpa using this

h5i_when h5i_app_std.graph.reachable =>
theorem graph.reachable_loop0_spec (start : alloc.vec.Vec T) :
    h5i_app_std.graph.reachable_loop0 inst cinst start (alloc.vec.Vec.new T) 0#usize
      ⦃ o => o.val = start.val.foldl insertStep [] ⦄ := by
  unfold h5i_app_std.graph.reachable_loop0
  apply WP.spec_mono (loop_fold start.val (fun o : alloc.vec.Vec T => o.val) insertStep (fun o j => o.length ≤ j)
    (fun st => h5i_app_std.graph.reachable_loop0.body inst cinst start st.1 st.2) ?_ (alloc.vec.Vec.new T) 0#usize
    (by simp) (by simp))
  · intro o ho; rw [ho]; simp
  · intro o i hi hinv
    unfold h5i_app_std.graph.reachable_loop0.body
    h5i_step [insertStep]

h5i_when h5i_app_std.graph.reachable =>
theorem graph.reachable_visit_spec (edges : alloc.vec.Vec (T × T)) (start : List T) (o : alloc.vec.Vec T) (x : T)
    (hb : start.length + edges.val.length < Usize.max)
    (hn : o.val.Nodup) (hs : o.val ⊆ graphNodes edges.val start) :
    h5i_app_std.graph.reachable_loop1_loop0 inst cinst edges o x 0#usize
      ⦃ o' => o'.val = edges.val.foldl (visitStep x) o.val ⦄ := by
  unfold h5i_app_std.graph.reachable_loop1_loop0
  apply WP.spec_mono (loop_fold edges.val (fun o : alloc.vec.Vec T => o.val) (visitStep x)
    (fun o _ => o.val.Nodup ∧ o.val ⊆ graphNodes edges.val start)
    (fun st => h5i_app_std.graph.reachable_loop1_loop0.body inst cinst edges x st.1 st.2) ?_ o 0#usize
    (by simp) ⟨hn, hs⟩)
  · intro o' ho; rw [ho]; simp
  · rintro o i hi ⟨hn, hs⟩
    unfold h5i_app_std.graph.reachable_loop1_loop0.body
    h5i_steps
    all_goals try simp only [FoldStep, visitStep]
    all_goals try (have he : (t, t1) ∈ edges.val := by rw [t_post]; exact List.getElem_mem _)
    · -- an edge from `x` to a node already in `o`
      refine ⟨by scalar_tac, ?_, by scalar_tac, hn, hs⟩
      rw [← t_post]; simp_all
    · -- room to push the new node
      have hl := length_lt_of_nodup_sub hn hs (mem_graphNodes_of_edge (start := start) he) (by simp_all)
      simp only [graphNodes, List.length_append, List.length_map] at hl; omega
    · -- the new node pushed
      have hy : t1 ∉ o.val := by simp_all
      refine ⟨by scalar_tac, ?_, by scalar_tac, ?_, ?_⟩
      · rw [← t_post]; simp_all
      · rw [out1_post, x_post]; exact List.Nodup.append hn (List.nodup_singleton _) (by simpa using hy)
      · rw [out1_post, x_post]
        exact List.append_subset.2 ⟨hs, by simpa using mem_graphNodes_of_edge (start := start) he⟩
    · -- an edge from another node
      refine ⟨by scalar_tac, ?_, by scalar_tac, hn, hs⟩
      rw [← t_post]; simp_all
    · exact ⟨by scalar_tac, trivial⟩

/-- What the outer loop keeps: `out` is duplicate-free, made of nodes, all
reachable, holds `start`, and the edges of its first `k` nodes are scanned. -/
def VisitInv (edges : List (T × T)) (start out : List T) (k : Nat) : Prop :=
  k ≤ out.length ∧ out.Nodup ∧ out ⊆ graphNodes edges start ∧ (∀ y ∈ out, Reach edges start y) ∧
    (∀ y ∈ start, y ∈ out) ∧ ∀ m (hm : m < out.length), m < k → ∀ e ∈ edges, e.1 = out[m] → e.2 ∈ out

h5i_when h5i_app_std.graph.reachable =>
theorem graph.reachable_loop1_spec (edges : alloc.vec.Vec (T × T)) (start : List T) (out : alloc.vec.Vec T)
    (hb : start.length + edges.val.length < Usize.max) (hinv : VisitInv edges.val start out.val 0) :
    h5i_app_std.graph.reachable_loop1 inst cinst edges out 0#usize
      ⦃ o => (∀ y, y ∈ o.val ↔ Reach edges.val start y) ∧ o.val.Nodup ⦄ := by
  unfold h5i_app_std.graph.reachable_loop1
  apply loop.spec_decr_nat (measure := fun (st : alloc.vec.Vec T × Usize) => (graphNodes edges.val start).length + 1 - st.2.val)
    (inv := fun st => VisitInv edges.val start st.1.val st.2.val)
  · rintro ⟨o, k⟩ ⟨hk, hn, hs, hr, h0, hp⟩
    simp only at hk hn hs hr h0 hp
    unfold h5i_app_std.graph.reachable_loop1.body
    step*
    · apply WP.spec_bind (graph.reachable_visit_spec inst cinst edges start o x hb hn hs)
      intro o' ho'
      step*
      have hkl : k.val < o.val.length := by scalar_tac
      have hpre := prefix_foldl_visitStep x edges.val o.val
      rw [← ho'] at hpre
      have hx : x = o.val[k.val] := by rw [x_post, t_post]
      have hN := length_le_of_nodup_sub hn hs
      refine ⟨⟨?_, ?_, ?_, ?_, ?_, ?_⟩, ?_⟩
      · have := hpre.length_le; scalar_tac
      · rw [ho']; exact nodup_foldl_visitStep _ _ _ hn
      · intro y hy
        rw [ho', mem_foldl_visitStep] at hy
        rcases hy with hy | ⟨e, he, -, rfl⟩
        · exact hs hy
        · exact mem_graphNodes_of_edge he
      · intro y hy
        rw [ho', mem_foldl_visitStep] at hy
        rcases hy with hy | ⟨e, he, h1, rfl⟩
        · exact hr y hy
        · have hrx : Reach edges.val start x := hr x (by rw [hx]; exact List.getElem_mem _)
          exact .step hrx (by rw [← h1]; exact he)
      · intro y hy; exact hpre.subset (h0 y hy)
      · intro m hm hmk e he he1
        have hm' : m < o.val.length := by scalar_tac
        have hget : o'.val[m] = o.val[m] := (hpre.getElem hm').symm
        rw [hget] at he1
        rw [ho', mem_foldl_visitStep]
        by_cases hmk' : m < k.val
        · exact .inl (hp m hm' hmk' e he he1)
        · have : m = k.val := by scalar_tac
          subst this
          exact .inr ⟨e, he, by rw [he1, hx], rfl⟩
      · scalar_tac
    · have hko : k.val = o.val.length := by scalar_tac
      refine ⟨fun y => ⟨hr y, fun h => ?_⟩, hn⟩
      apply Reach.sub_of_closed (· ∈ o.val) h0 _ h
      intro a b ha hab
      obtain ⟨m, hm, rfl⟩ := List.getElem_of_mem ha
      exact hp m hm (by omega) _ hab rfl
  · simpa using hinv

h5i_when h5i_app_std.graph.reachable =>
/-- The result holds exactly the nodes reachable from `start`, each once. -/
@[step] theorem graph.reachable_spec (edges : alloc.vec.Vec (T × T)) (start : alloc.vec.Vec T)
    (hb : start.val.length + edges.val.length < Usize.max) :
    h5i_app_std.graph.reachable inst cinst edges start
      ⦃ o => (∀ y, y ∈ o.val ↔ Reach edges.val start.val y) ∧ o.val.Nodup ⦄ := by
  unfold h5i_app_std.graph.reachable
  apply WP.spec_bind (graph.reachable_loop0_spec inst cinst start)
  intro o ho
  apply graph.reachable_loop1_spec inst cinst edges start.val o hb
  refine ⟨Nat.zero_le _, ?_, ?_, ?_, ?_, fun m _ hm => absurd hm (Nat.not_lt_zero _)⟩
  · rw [ho]; exact nodup_foldl_insertStep _ _ List.nodup_nil
  · intro y hy; rw [ho, mem_foldl_insertStep] at hy; simp at hy; simp [graphNodes, hy]
  · intro y hy; rw [ho, mem_foldl_insertStep] at hy; simp at hy; exact .start hy
  · intro y hy; rw [ho, mem_foldl_insertStep]; exact .inr hy

end Graph

/-! ## Time, in whole seconds -/

h5i_when h5i_app_std.time.expired =>
@[step] theorem time.expired_spec (exp now leeway : U64) :
    h5i_app_std.time.expired exp now leeway ⦃ r => r = decide (exp.val + leeway.val < now.val) ⦄ := by
  unfold h5i_app_std.time.expired
  step*

h5i_when h5i_app_std.time.not_yet_valid =>
@[step] theorem time.not_yet_valid_spec (nbf now leeway : U64) :
    h5i_app_std.time.not_yet_valid nbf now leeway ⦃ r => r = decide (now.val + leeway.val < nbf.val) ⦄ := by
  unfold h5i_app_std.time.not_yet_valid
  step*

h5i_when h5i_app_std.time.within =>
@[step] theorem time.within_spec (start now ttl : U64) :
    h5i_app_std.time.within start now ttl ⦃ r => r = decide (now.val < start.val + ttl.val) ⦄ := by
  unfold h5i_app_std.time.within
  step*

h5i_when h5i_app_std.time.secs_ceil =>
@[step] theorem time.secs_ceil_spec (micros : U64) :
    h5i_app_std.time.secs_ceil micros ⦃ r => r.val = (micros.val + 999999) / 1000000 ⦄ := by
  unfold h5i_app_std.time.secs_ceil
  step*

end h5i_app_std.Specs
