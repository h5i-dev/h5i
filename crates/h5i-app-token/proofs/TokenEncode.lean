import TokenProofs
/-! The extracted encoder computes `enc` and `joinS`, so `parse` reads back
what `encode_payload` and `join` write. -/
open Aeneas Aeneas.Std Result h5i_app_token h5i_app_token.Spec h5i_app_token.Proofs H5iAppLib

namespace h5i_app_token.Encode

@[step]
theorem hex_digit_spec (n : U8) (h : n.val < 16) : hex_digit n ⦃ r => r.val = hexd n.val ⦄ := by
  unfold hex_digit hexd
  step*

@[step]
theorem join_loop0_spec (p : Slice U8) (out : alloc.vec.Vec U8) (i : Usize)
    (hi : i.val ≤ p.length) (hc : out.length + (p.length - i.val) < Usize.max) :
    join_loop0 p out i ⦃ r => B r.val = B out.val ++ (B p.val).drop i.val ⦄ := by
  unfold join_loop0
  apply loop.spec_decr_nat (measure := fun (x : alloc.vec.Vec U8 × Usize) => p.length - x.2.val)
    (inv := fun x => x.2.val ≤ p.length ∧ x.1.length + (p.length - x.2.val) < Usize.max ∧
      B x.1.val ++ (B p.val).drop x.2.val = B out.val ++ (B p.val).drop i.val)
  · rintro ⟨o, j⟩ ⟨hj, hcap, heq⟩
    unfold join_loop0.body
    simp only at hj hcap heq
    step*
    · have hjl : j.val < p.length := by scalar_tac
      refine ⟨by scalar_tac, by simp [out1_post]; scalar_tac, ?_, by scalar_tac⟩
      rw [← heq, i3_post, List.drop_eq_getElem_cons (i := j.val) (l := B p.val) (by simpa using hjl)]
      simp [out1_post, i2_post]
    · rw [← heq, List.drop_eq_nil_of_le (by simp; scalar_tac)]; simp
  · simp [hi, hc]

@[step]
theorem join_loop1_spec (sg : Slice U8) (out : alloc.vec.Vec U8) (j : Usize)
    (hj : j.val ≤ sg.length) (hc : out.length + 2 * (sg.length - j.val) < Usize.max) :
    join_loop1 sg out j ⦃ r => B r.val = B out.val ++ ((B sg.val).drop j.val).flatMap hexOf ⦄ := by
  unfold join_loop1
  apply loop.spec_decr_nat (measure := fun (x : alloc.vec.Vec U8 × Usize) => sg.length - x.2.val)
    (inv := fun x => x.2.val ≤ sg.length ∧ x.1.length + 2 * (sg.length - x.2.val) < Usize.max ∧
      B x.1.val ++ ((B sg.val).drop x.2.val).flatMap hexOf = B out.val ++ ((B sg.val).drop j.val).flatMap hexOf)
  · rintro ⟨o, k⟩ ⟨hk, hcap, heq⟩
    unfold join_loop1.body
    simp only at hk hcap heq
    step*
    all_goals try (simp only [out1_post, List.length_append, List.length_singleton]; scalar_tac)
    · have hkl : k.val < sg.length := by scalar_tac
      refine ⟨by scalar_tac, by simp [out2_post, out1_post]; scalar_tac, ?_, by scalar_tac⟩
      rw [← heq, j1_post, List.drop_eq_getElem_cons (i := k.val) (l := B sg.val) (by simpa using hkl)]
      simp [out2_post, out1_post, i2_post, i4_post, i1_post, i3_post, b_post, hexOf]
    · rw [← heq, List.drop_eq_nil_of_le (by simp; scalar_tac)]; simp
  · simp [hj, hc]

@[step]
theorem join_spec (p sg : Slice U8) (h : p.length + 2 * sg.length + 1 < Usize.max) :
    join p sg ⦃ v => B v.val = joinS (B p.val) (B sg.val) ⦄ := by
  unfold join
  step as ⟨out, hout⟩
  have hl : out.length = p.length := by
    have := congrArg List.length hout; simpa using this
  step as ⟨out1, hout1⟩
  step as ⟨v, hv⟩
  simp [hv, hout1, hout, joinS, dot_val]

@[step]
theorem push_dec_loop0_spec (rev : alloc.vec.Vec U8) (m : U64)
    (hc : rev.length + (Nat.digits 10 m.val).length ≤ 21) :
    push_dec_loop0 rev m ⦃ r => B r.val = B rev.val ++ (Nat.digits 10 m.val).map (48 + ·) ∧
      r.length ≤ 21 ⦄ := by
  unfold push_dec_loop0
  apply loop.spec_decr_nat (measure := fun (x : alloc.vec.Vec U8 × U64) => x.2.val)
    (inv := fun x => x.1.length + (Nat.digits 10 x.2.val).length ≤ 21 ∧
      B x.1.val ++ (Nat.digits 10 x.2.val).map (48 + ·) = B rev.val ++ (Nat.digits 10 m.val).map (48 + ·))
  · rintro ⟨r, k⟩ ⟨hcap, heq⟩
    unfold push_dec_loop0.body
    simp only at hcap heq
    step*
    · have hk0 : 0 < k.val := by scalar_tac
      have hd : d.val = k.val % 10 := by
        rw [d_post, UScalar.cast_val_eq, i_post]; simp [UScalarTy.numBits]; omega
      have hdig := Nat.digits_def' (by norm_num : 1 < 10) hk0
      rw [hdig] at hcap heq
      refine ⟨?_, ?_, by rw [m1_post]; omega⟩
      · simp only [List.length_cons] at hcap; simp [rev1_post, m1_post]; scalar_tac
      · rw [← heq, m1_post]; simp [rev1_post, i1_post, hd]
    · have hk : k.val = 0 := by scalar_tac
      rw [hk] at heq hcap
      simp only [Nat.digits_zero, List.map_nil, List.append_nil, List.length_nil] at heq hcap
      exact ⟨heq, by omega⟩
  · simp [hc]

@[step]
theorem push_dec_loop1_spec (out rev : alloc.vec.Vec U8) (i : Usize)
    (hi : i.val ≤ rev.length) (hc : out.length + i.val < Usize.max) :
    push_dec_loop1 out rev i ⦃ r => B r.val = B out.val ++ ((B rev.val).take i.val).reverse ⦄ := by
  unfold push_dec_loop1
  apply loop.spec_decr_nat (measure := fun (x : alloc.vec.Vec U8 × Usize) => x.2.val)
    (inv := fun x => x.2.val ≤ rev.length ∧ x.1.length + x.2.val < Usize.max ∧
      B x.1.val ++ ((B rev.val).take x.2.val).reverse = B out.val ++ ((B rev.val).take i.val).reverse)
  · rintro ⟨o, k⟩ ⟨hk, hcap, heq⟩
    unfold push_dec_loop1.body
    simp only at hk hcap heq
    step*
    · have hlt : i1.val < rev.val.length := by scalar_tac
      refine ⟨by scalar_tac, by simp [out1_post]; scalar_tac, ?_, by omega⟩
      rw [← heq, show k.val = i1.val + 1 by omega, List.take_add_one]
      simp [out1_post, i2_post, List.getElem?_eq_getElem hlt]
    · have hk0 : k.val = 0 := by scalar_tac
      rw [hk0] at heq; simpa using heq
  · simp [hi, hc]

theorem digits_u64_div10 (n : U64) : (Nat.digits 10 (n.val / 10)).length ≤ 19 := by
  rw [Nat.digits_length_le_iff (by norm_num)]
  have : n.val ≤ U64.max := by scalar_tac
  rw [U64.max_eq] at this
  omega

@[step]
theorem push_dec_spec (out : alloc.vec.Vec U8) (n : U64) (hc : out.length + 21 < Usize.max) :
    push_dec out n ⦃ r => B r.val = B out.val ++ dec n.val ∧ r.length ≤ out.length + 20 ⦄ := by
  unfold push_dec
  have hdl := digits_u64_div10 n
  step*
  have hd : d.val = n.val % 10 := by
    rw [d_post, UScalar.cast_val_eq, i_post]; simp [UScalarTy.numBits]; omega
  have hr1 : B rev1.val = (48 + n.val % 10) :: (Nat.digits 10 (n.val / 10)).map (48 + ·) := by
    rw [rev1_post, rev_post, m_post]; simp [i1_post, hd]
  have ht : List.take rev1.len.val (B rev1.val) = B rev1.val := List.take_of_length_le (by simp)
  refine ⟨?_, ?_⟩
  · rw [r_post, ht, hr1]; simp [dec, digitsLE]
  · have hl := congrArg List.length r_post
    rw [ht, hr1] at hl
    simp at hl
    scalar_tac

@[step]
theorem encode_payload_spec (t u e : U64) :
    encode_payload t u e ⦃ v => B v.val = enc t.val u.val e.val ∧ v.length ≤ 66 ⦄ := by
  unfold encode_payload
  step*
  all_goals try (simp_all; scalar_tac)
  refine ⟨?_, ?_⟩
  · simp [v_post, out6_post, out5_post, out4_post, out3_post, out2_post, out1_post, out_post,
      enc, dot_val]
  · have h2 : out2.length = 3 := by simp [out2_post, out1_post, out_post]
    have h4 : out4.length = out3.length + 1 := by simp [out4_post]
    have h6 : out6.length = out5.length + 1 := by simp [out6_post]
    omega

theorem u64_eq_of_val {x y : U64} (h : x.val = y.val) : x = y := by scalar_tac

/-- `parse` reads back what `encode_payload` and `join` write. -/
theorem round_trip (t u e : U64) (sig : Slice U8) (h : 2 * sig.length + 68 < Usize.max) :
    (do
      let pl ← encode_payload t u e
      let tok ← join pl.slice sig
      parse tok.slice) ⦃ r => ∃ p, r = some p ∧
        p.tenant = t ∧ p.user = u ∧ p.exp = e ∧ p.sig.val = sig.val ⦄ := by
  step as ⟨pl, hpl, hlen⟩
  step as ⟨tok, htok⟩
  · have : pl.slice.length = pl.length := rfl
    scalar_tac
  apply WP.spec_mono (parse_idx tok.slice)
  intro r hr
  rw [parseIdx_eq] at hr
  have hb : ∀ x : U64, x.val ≤ u64max := fun x => by unfold u64max; scalar_tac
  have hs : ∀ b ∈ B sig.val, b < 256 := by
    intro b hb; simp at hb; obtain ⟨c, -, rfl⟩ := hb; scalar_tac
  have hj : B tok.slice.val = joinS (enc t.val u.val e.val) (B sig.val) := by
    rw [← hpl]; exact htok
  rw [hj, parse_join _ _ _ _ (hb t) (hb u) (hb e) hs] at hr
  cases r with
  | none => simp at hr
  | some p =>
    simp only [Option.map_some, Option.some.injEq, view, Tok.mk.injEq] at hr
    obtain ⟨h1, h2, h3, -, h5⟩ := hr
    refine ⟨p, rfl, u64_eq_of_val h1, u64_eq_of_val h2, u64_eq_of_val h3, ?_⟩
    exact List.map_injective_iff.2 (fun a b hab => (u8_eq_iff a b).2 hab) h5

end h5i_app_token.Encode
