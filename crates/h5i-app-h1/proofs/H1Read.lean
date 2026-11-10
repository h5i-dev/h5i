import H1Body
import H1Proofs
/-!
# Reading a body from the receive buffer

`read_body` finds where a request's body and message end in the server's
buffer, decoding a chunked body in place. `dechunk_spec` relates it to
`Spec.feed` over the body bytes received so far (the wire bytes): the
payload is at `buf[start..end]`, `used - start` bytes of the wire were read,
nothing at or past `used` was written, and writes never reach bytes not yet
read. With `feed_dechunk` the result is `Spec.dechunk` of the wire bytes.
-/
open Aeneas Aeneas.Std Result h5i_app_h1 h5i_app_h1.Spec

namespace h5i_app_h1.Proofs

/-- The buffer `buf` holds a head of `start` bytes and then, up to `filled`,
the wire bytes `wire` of a chunked body, decoded in place up to `r`: the
fold over `wire[..r - start]` is in state `st` with the payload at
`buf[start..w]`, and `buf[r..filled]` still holds the unread wire bytes. -/
structure ChunkInv (mb start filled : Nat) (wire buf : List Nat) (st : CState) (r w : Nat) : Prop where
  le : start ≤ w ∧ w ≤ r ∧ r ≤ filled ∧ filled ≤ buf.length
  len : wire.length = filled - start
  fed : feed cStart (wire.take (r - start)) mb [] 0 = .more st ((buf.drop start).take (w - start))
  unread : ∀ i, r ≤ i → i < filled → buf.getD i 0 = wire.getD (i - start) 0

/-! ## One step, on lists -/

theorem feed_take_succ (wire : List Nat) (j mb : Nat) (st : CState) (acc : List Nat) (hj : j < wire.length)
    (h : feed cStart (wire.take j) mb [] 0 = .more st acc) :
    feed cStart (wire.take (j + 1)) mb [] 0 = match chunkStep st (wire.getD j 0) mb with
      | (s, .nothing) => .more s acc
      | (s, .data) => .more s (acc ++ [wire.getD j 0])
      | (_, .done) => .done acc (j + 1)
      | (_, .reject x) => .reject x := by
  have ht : wire.take (j + 1) = wire.take j ++ [wire.getD j 0] := by
    rw [List.take_add_one, List.getD_eq_getElem?_getD, List.getElem?_eq_getElem hj]; simp
  have hl : (wire.take j).length = j := by simp; omega
  rw [ht, feed_append, h]
  dsimp only
  rw [feed_cons, hl, Nat.zero_add]
  rcases chunkStep st (wire.getD j 0) mb with ⟨s, o⟩
  cases o <;> rfl

theorem feed_of_take (wire : List Nat) (j mb : Nat) (r : FR) (hr : ∀ s a, r ≠ .more s a)
    (h : feed cStart (wire.take j) mb [] 0 = r) : feed cStart wire mb [] 0 = r := by
  rw [← List.take_append_drop j wire, feed_append, h]
  cases r with
  | more s a => exact absurd rfl (hr s a)
  | _ => rfl

theorem payload_set (L : List Nat) (start w x : Nat) (h1 : start ≤ w) (h2 : w < L.length) :
    ((L.set w x).drop start).take (w + 1 - start) = (L.drop start).take (w - start) ++ [x] := by
  rw [List.drop_set, if_neg (by omega), show w + 1 - start = (w - start) + 1 by omega]
  exact take_set_succ _ _ _ (by simp; omega)

theorem getD_set_ne (L : List Nat) (i w x : Nat) (h : i ≠ w) : (L.set w x).getD i 0 = L.getD i 0 := by
  simp [List.getD_eq_getElem?_getD, Ne.symm h]

/-- A byte that is framing: the fold moves on, nothing is written. -/
theorem inv_nothing {mb start filled : Nat} {wire buf : List Nat} {st s : CState} {r w : Nat}
    (hI : ChunkInv mb start filled wire buf st r w) (hr : r < filled)
    (hs : chunkStep st (buf.getD r 0) mb = (s, .nothing)) :
    ChunkInv mb start filled wire buf s (r + 1) w := by
  obtain ⟨⟨h1, h2, h3, h4⟩, hlen, hfed, hun⟩ := hI
  have hb := hun r (le_refl _) hr
  refine ⟨⟨h1, by omega, by omega, h4⟩, hlen, ?_, fun i hi hi' => hun i (by omega) hi'⟩
  rw [show r + 1 - start = (r - start) + 1 by omega,
    feed_take_succ wire (r - start) mb st _ (by omega) hfed, ← hb, hs]

/-- A payload byte: it moves down to `w`, which is at most `r`. -/
theorem inv_data {mb start filled : Nat} {wire buf : List Nat} {st s : CState} {r w : Nat}
    (hI : ChunkInv mb start filled wire buf st r w) (hr : r < filled)
    (hs : chunkStep st (buf.getD r 0) mb = (s, .data)) :
    ChunkInv mb start filled wire (buf.set w (buf.getD r 0)) s (r + 1) (w + 1) := by
  obtain ⟨⟨h1, h2, h3, h4⟩, hlen, hfed, hun⟩ := hI
  have hb := hun r (le_refl _) hr
  refine ⟨⟨by omega, by omega, by omega, by simp; omega⟩, hlen, ?_, fun i hi hi' => ?_⟩
  · rw [show r + 1 - start = (r - start) + 1 by omega,
      feed_take_succ wire (r - start) mb st _ (by omega) hfed, ← hb, hs,
      payload_set _ _ _ _ h1 (by omega)]
  · rw [getD_set_ne _ _ _ _ (by omega)]; exact hun i (by omega) hi'

theorem inv_done {mb start filled : Nat} {wire buf : List Nat} {st s : CState} {r w : Nat}
    (hI : ChunkInv mb start filled wire buf st r w) (hr : r < filled)
    (hs : chunkStep st (buf.getD r 0) mb = (s, .done)) :
    feed cStart wire mb [] 0 = .done ((buf.drop start).take (w - start)) (r + 1 - start) := by
  obtain ⟨⟨h1, h2, h3, h4⟩, hlen, hfed, hun⟩ := hI
  have hb := hun r (le_refl _) hr
  apply feed_of_take wire (r - start + 1) mb _ (fun _ _ h => by cases h)
  rw [feed_take_succ wire (r - start) mb st _ (by omega) hfed, ← hb, hs,
    show r - start + 1 = r + 1 - start by omega]

theorem inv_reject {mb start filled : Nat} {wire buf : List Nat} {st s : CState} {r w x : Nat}
    (hI : ChunkInv mb start filled wire buf st r w) (hr : r < filled)
    (hs : chunkStep st (buf.getD r 0) mb = (s, .reject x)) :
    feed cStart wire mb [] 0 = .reject x := by
  obtain ⟨⟨h1, h2, h3, h4⟩, hlen, hfed, hun⟩ := hI
  have hb := hun r (le_refl _) hr
  apply feed_of_take wire (r - start + 1) mb _ (fun _ _ h => by cases h)
  rw [feed_take_succ wire (r - start) mb st _ (by omega) hfed, ← hb, hs]

/-- At `r = filled` the whole wire has been fed. -/
theorem inv_more {mb start filled : Nat} {wire buf : List Nat} {st : CState} {w : Nat}
    (hI : ChunkInv mb start filled wire buf st filled w) :
    feed cStart wire mb [] 0 = .more st ((buf.drop start).take (w - start)) := by
  have := hI.fed
  rwa [← hI.len, List.take_of_length_le (le_refl _)] at this

/-- The first call: nothing read yet. -/
theorem inv_start (mb start filled : Nat) (buf : List Nat) (h1 : start ≤ filled) (h2 : filled ≤ buf.length) :
    ChunkInv mb start filled ((buf.drop start).take (filled - start)) buf cStart start start := by
  refine ⟨⟨le_refl _, le_refl _, h1, h2⟩, by simp; omega, by simp [feed_nil], fun i hi hi' => ?_⟩
  simp only [List.getD_eq_getElem?_getD, List.getElem?_take, List.getElem?_drop]
  rw [if_pos (by omega), show start + (i - start) = i by omega]

/-- Between calls the server appends bytes at `filled` and changes nothing
below it: the invariant holds for the longer wire. -/
theorem inv_extend {mb start filled filled' : Nat} {wire buf buf' : List Nat} {st : CState} {r w : Nat}
    (hI : ChunkInv mb start filled wire buf st r w) (hf : filled ≤ filled') (hl : filled' ≤ buf'.length)
    (hbelow : ∀ i, i < filled → buf'.getD i 0 = buf.getD i 0) :
    ChunkInv mb start filled' (wire ++ (buf'.drop filled).take (filled' - filled)) buf' st r w := by
  obtain ⟨⟨h1, h2, h3, h4⟩, hlen, hfed, hun⟩ := hI
  have hpay : (buf'.drop start).take (w - start) = (buf.drop start).take (w - start) := by
    apply List.ext_getElem?
    intro i
    simp only [List.getElem?_take, List.getElem?_drop]
    split
    · have := hbelow (start + i) (by omega)
      simp only [List.getD_eq_getElem?_getD] at this
      by_cases ha : start + i < buf'.length
      · by_cases hb : start + i < buf.length
        · rw [List.getElem?_eq_getElem ha, List.getElem?_eq_getElem hb] at *
          simp at this; rw [this]
        · omega
      · omega
    · rfl
  refine ⟨⟨h1, h2, by omega, hl⟩, by simp; omega, ?_, fun i hi hi' => ?_⟩
  · rw [List.take_append_of_le_length (by omega), hfed, hpay]
  · by_cases hi2 : i < filled
    · rw [hbelow i hi2, hun i hi hi2]
      simp only [List.getD_eq_getElem?_getD, List.getElem?_append_left (show i - start < wire.length by omega)]
    · simp only [List.getD_eq_getElem?_getD]
      rw [List.getElem?_append_right (by omega), List.getElem?_take, List.getElem?_drop, hlen,
        if_pos (by omega), show filled + (i - start - (filled - start)) = i by omega]

/-! ## The extracted loop -/

theorem B_set (s : Slice U8) (i : Usize) (x : U8) : B (s.set i x).val = (B s.val).set i.val x.val := by
  simp [B, Slice.set_val_eq, List.map_set]

theorem B_getD (s : Slice U8) (i : Nat) (x : U8) (h : i < s.val.length) (hx : s.val[i]'h = x) :
    (B s.val).getD i 0 = x.val := by
  simp [B, List.getD_eq_getElem?_getD, List.getElem?_eq_getElem h, hx]

def boView : BodyOut → Spec.DR
  | .More => .more
  | .Done _ u => .done [] u.val
  | .Reject s => .reject s.val

/-- What the loop of `dechunk` computes, from a state satisfying `ChunkInv`:
`buf` keeps its length; bytes below the first write position `w` and at or
past the final read position `r'` are unchanged; and the result is the fold
over the whole wire: `More` (with the invariant at `r' = filled`), `Done`
with the payload at `buf'[start..w']` and `r' - start` wire bytes used, or
`Reject`. -/
theorem dechunk_loop_spec (buf : Slice U8) (filled : Usize) (mb : U32) (cs : ChunkState) (r w : Usize)
    (start : Nat) (wire : List Nat) (hI : ChunkInv mb.val start filled.val wire (B buf.val) (csView cs) r.val w.val) :
    dechunk_loop buf filled mb cs r w ⦃ cs' r' w' out buf' =>
      buf'.length = buf.length ∧ start ≤ w'.val ∧ w'.val ≤ r'.val ∧ r'.val ≤ filled.val ∧ r.val ≤ r'.val ∧
      (∀ i, i < w.val ∨ r'.val ≤ i → (B buf'.val).getD i 0 = (B buf.val).getD i 0) ∧
      match out with
      | .More => r'.val = filled.val ∧
          ChunkInv mb.val start filled.val wire (B buf'.val) (csView cs') r'.val w'.val
      | .Done e u => e = w' ∧ u = r' ∧
          feed cStart wire mb.val [] 0 = .done (((B buf'.val).drop start).take (w'.val - start)) (u.val - start)
      | .Reject x => feed cStart wire mb.val [] 0 = .reject x.val ⦄ := by
  unfold dechunk_loop
  apply loop.spec_decr_nat
    (measure := fun (x : Slice U8 × ChunkState × Usize × Usize) => filled.val - x.2.2.1.val)
    (inv := fun x => x.1.length = buf.length ∧ w.val ≤ x.2.2.2.val ∧ r.val ≤ x.2.2.1.val ∧
      (∀ i, i < w.val ∨ x.2.2.1.val ≤ i → (B x.1.val).getD i 0 = (B buf.val).getD i 0) ∧
      ChunkInv mb.val start filled.val wire (B x.1.val) (csView x.2.1) x.2.2.1.val x.2.2.2.val)
  · rintro ⟨b1, c1, r1, w1⟩ ⟨hl, hw, hrr, hun, hI1⟩
    simp only at hl hw hrr hun hI1 ⊢
    have hle := hI1.le
    have hblen : (B b1.val).length = b1.val.length := by simp [B]
    unfold dechunk_loop.body
    split
    · rename_i hr
      have hr' : r1.val < filled.val := by scalar_tac
      step as ⟨x, hx⟩
      step as ⟨s, out, hso⟩
      step as ⟨r2, hr2⟩
      have hb : (B b1.val).getD r1.val 0 = x.val := B_getD b1 r1.val x (by scalar_tac) hx.symm
      have hstep : chunkStep (csView c1) ((B b1.val).getD r1.val 0) mb.val = (csView s, coView out) := by
        rw [hb, hso]
      have hr2' : r2.val = r1.val + 1 := by scalar_tac
      cases out with
      | Nothing =>
        try simp only [WP.spec_ok]
        have hI2 := inv_nothing hI1 hr' hstep
        rw [← hr2'] at hI2
        and_intros
        all_goals first | exact hl | exact hw | exact hI2 | (intro i hi; exact hun i (by omega)) | scalar_tac
      | Data =>
        step as ⟨b2, hb2⟩
        step as ⟨w2, hw2⟩
        try simp only [WP.spec_ok]
        have hwr : w1.val ≤ r1.val := hle.2.1
        have hset : B b2.val = (B b1.val).set w1.val ((B b1.val).getD r1.val 0) := by
          rw [hb2, B_set, hb]
        have hI2 := inv_data hI1 hr' hstep
        rw [← hr2', ← hset, show w1.val + 1 = w2.val by scalar_tac] at hI2
        have hl2 : b2.length = buf.length := by rw [hb2]; simp [Slice.length]; exact hl
        have hun2 : ∀ i, i < w.val ∨ r2.val ≤ i → (B b2.val).getD i 0 = (B buf.val).getD i 0 := by
          intro i hi; rw [hset, getD_set_ne _ _ _ _ (by omega)]; exact hun i (by omega)
        and_intros
        all_goals first | exact hl2 | exact hI2 | exact hun2 | scalar_tac
      | Done =>
        try simp only [WP.spec_ok]
        have hD := inv_done hI1 hr' hstep
        rw [← hr2'] at hD
        and_intros
        all_goals first | exact hl | exact hD | trivial | (intro i hi; exact hun i (by omega)) | scalar_tac
      | Reject x' =>
        try simp only [WP.spec_ok]
        have hR := inv_reject hI1 hr' hstep
        and_intros
        all_goals first | exact hl | exact hR | trivial | (intro i hi; exact hun i (by omega)) | scalar_tac
    · rename_i hr
      simp only [WP.spec_ok]
      have hrf : r1.val = filled.val := by have := hle.2.2.1; scalar_tac
      exact ⟨hl, hle.1, hle.2.1, hle.2.2.1, hrr, hun, hrf, hI1⟩
  · exact ⟨rfl, le_refl _, le_refl _, fun _ _ => rfl, hI⟩

/-! ## `dechunk` and `read_body` -/

/-- The payload `buf[start..e]`. -/
def payload (buf : Slice U8) (start e : Nat) : List Nat := ((B buf.val).drop start).take (e - start)

@[simp] theorem internal_error_val : INTERNAL_ERROR.val = 500 := by unfold INTERNAL_ERROR; rfl

/-- What a call of `dechunk` (and `read_body` on a chunked body) returns,
for wire bytes `wire` and a state `st` satisfying `ChunkInv`. -/
def ChunkPost (mb start filled : Nat) (wire : List Nat) (buf : Slice U8) (st : BodyState)
    (st' : BodyState) (out : BodyOut) (buf' : Slice U8) : Prop :=
  buf'.length = buf.length ∧ start ≤ st'.w.val ∧ st'.w.val ≤ st'.r.val ∧ st'.r.val ≤ filled ∧
  st.r.val ≤ st'.r.val ∧ (∀ i, i < st.w.val ∨ st'.r.val ≤ i → (B buf'.val).getD i 0 = (B buf.val).getD i 0) ∧
  match out with
  | .More => st'.r.val = filled ∧ ChunkInv mb start filled wire (B buf'.val) (csView st'.chunk) st'.r.val st'.w.val
  | .Done e u => e = st'.w ∧ u = st'.r ∧ feed cStart wire mb [] 0 = .done (payload buf' start e.val) (u.val - start)
  | .Reject x => feed cStart wire mb [] 0 = .reject x.val

theorem dechunk_spec (buf : Slice U8) (filled : Usize) (st : BodyState) (mb : U32) (start : Nat) (wire : List Nat)
    (hI : ChunkInv mb.val start filled.val wire (B buf.val) (csView st.chunk) st.r.val st.w.val) :
    dechunk buf filled st mb ⦃ r buf' => ChunkPost mb.val start filled.val wire buf st r.1 r.2 buf' ⦄ := by
  have hle := hI.le
  have hbl : (B buf.val).length = buf.length := by simp [B, Slice.length]
  unfold dechunk
  rw [if_neg (by scalar_tac)]
  rw [if_neg (by scalar_tac)]
  step with dechunk_loop_spec buf filled mb st.chunk st.r st.w start wire hI
  refine ⟨cs_post, start_post, cs_post1, cs_post2, cs_post3, cs_post4, ?_⟩
  cases bo with
  | Done e u => obtain ⟨rfl, rfl, h⟩ := cs_post5; exact ⟨rfl, rfl, h⟩
  | More => exact cs_post5
  | Reject x => exact cs_post5

/-- `read_body` on a chunked body: the payload is `buf'[start..end]`, the
message used `used - start` wire bytes, and the result is `Spec.dechunk` of
the wire bytes. -/
theorem read_body_chunked (buf : Slice U8) (filled start : Usize) (st : BodyState) (mb : U32) (wire : List Nat)
    (hI : ChunkInv mb.val start.val filled.val wire (B buf.val) (csView st.chunk) st.r.val st.w.val) :
    read_body buf filled start .Chunked st mb ⦃ r buf' =>
      ChunkPost mb.val start.val filled.val wire buf st r.1 r.2 buf' ∧
      match r.2 with
      | .More => Spec.dechunk wire mb.val = .more
      | .Done e u => Spec.dechunk wire mb.val = .done (payload buf' start.val e.val) (u.val - start.val)
      | .Reject x => Spec.dechunk wire mb.val = .reject x.val ⦄ := by
  unfold read_body
  apply WP.spec_mono (dechunk_spec buf filled st mb start.val wire hI)
  rintro ⟨⟨st', out⟩, buf'⟩ h
  refine ⟨h, ?_⟩
  obtain ⟨-, -, -, -, -, -, h5⟩ := h
  rw [← feed_dechunk]
  cases out with
  | More =>
    obtain ⟨hr, hI'⟩ := h5
    simp only at hr hI' ⊢
    rw [hr] at hI'
    rw [inv_more hI']; rfl
  | Done e u => obtain ⟨-, -, h⟩ := h5; simp only; rw [h]; rfl
  | Reject x => simp only; rw [h5]; rfl

/-- `read_body` with `Content-Length: n`: done once `filled` reaches
`start + n`, 413 if that is past the end of `buf`; `buf` is not changed. -/
theorem read_body_length (buf : Slice U8) (filled start : Usize) (n : U64) (st : BodyState) (mb : U32) :
    read_body buf filled start (.Length n) st mb ⦃ r buf' => buf' = buf ∧ r.1 = st ∧
      match r.2 with
      | .Done e u => e.val = start.val + n.val ∧ u = e ∧ e.val ≤ filled.val
      | .More => filled.val < start.val + n.val ∧ start.val + n.val ≤ buf.length
      | .Reject x => x.val = 413 ∧ buf.length < start.val + n.val ⦄ := by
  unfold read_body
  simp only
  split
  · simp only [WP.spec_ok, WP.uncurry'_pair]
    and_intros <;> first | rfl | trivial | scalar_tac | simp
  step as ⟨i2, hi2⟩
  step as ⟨i3, hi3⟩
  have hi3' : i3.val = i2.val := by rw [hi3]; simp
  split
  · simp only [WP.spec_ok, WP.uncurry'_pair]
    and_intros <;> first | rfl | trivial | scalar_tac | simp
  step as ⟨i4, hi4⟩
  have hi4' : i4.val = n.val := by
    have hn : n.val ≤ i2.val := by scalar_tac
    have hb : i2.val < 2 ^ System.Platform.numBits := i2.bv.isLt
    rw [hi4]; simp; exact Nat.mod_eq_of_lt (by omega)
  step as ⟨e, he⟩
  split
  · simp only [WP.spec_ok, WP.uncurry'_pair]
    and_intros <;> first | rfl | trivial | scalar_tac
  · simp only [WP.spec_ok, WP.uncurry'_pair]
    and_intros <;> first | rfl | trivial | scalar_tac

/-- `read_body` with no body: done at `start`. -/
theorem read_body_empty (buf : Slice U8) (filled start : Usize) (st : BodyState) (mb : U32) :
    read_body buf filled start .Empty st mb ⦃ r buf' => buf' = buf ∧ r.1 = st ∧ r.2 = .Done start start ⦄ := by
  unfold read_body; simp

/-- `body_start` is the state `inv_start` describes. -/
theorem body_start_spec (start : Usize) :
    body_start start ⦃ st => csView st.chunk = cStart ∧ st.r = start ∧ st.w = start ⦄ := by
  unfold body_start; simp [csView, cStart, CHUNK_START, ch_vals]

end h5i_app_h1.Proofs
