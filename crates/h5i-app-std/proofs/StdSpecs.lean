import H5iAppStd
import H5iAppLib
/-!
# Specs of h5i-app-std

A `@[step]` spec for every function of `h5i-app-std`, stating its result over
the lists the vectors hold, with the models of `H5iAppLib.Text` and
`H5iAppLib.Sets`. `step*` then goes through a call to any of them.

This file is checked against the crate's own extraction. A kernel that
includes the crate gets a copy importing the kernel's extraction instead
(`scripts/app/std-specs.sh`), which opens the kernel's namespace: the
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
