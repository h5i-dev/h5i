import H1Read
import H1Prefix
/-!
# Receiving a request

`recv_step` is the receive loop without its I/O: the server calls it on the
bytes received so far, reads more when it answers `NeedRead`, and acts on
anything else. `recvS` says what it decides, on the list of bytes received
for the request: `parseHead`, then the body by its framing (`Spec.dechunk`
for a chunked one), with 431/413 when the buffer is full first.

* `recv_step_spec`: one call, from a state satisfying `RInv`, gives `recvS`
  of the bytes received, keeps the head bytes and every byte from `used` on,
  and after `NeedRead` leaves a state satisfying `RInv`; `RInv.extend`:
  appending a read at `filled` keeps `RInv`.
* `Drive` is the server's loop over the extracted `recv_step` for a
  sequence of reads. `drive_view`: its action is `recvS` of every byte read;
  `drive_split`: two sequences of reads with the same bytes give the same
  action, payload and `filled`; `drive_at_once`: one call on all the bytes
  at once (and any that follow) gives the same; `drive_room`: it reads only
  with room left in the buffer.
* `recvS_prefix`: once `recvS` decides, more bytes and the peer closing do
  not change it.
* `drive_next` (with `carry_spec`): after `Ready`, `carry` leaves exactly the
  bytes received after `used` as the start of the next request.
* `fit_limits_spec`, `must_close_spec`.
-/
open Aeneas Aeneas.Std Result h5i_app_h1 h5i_app_h1.Spec h5i_app_h1.Proofs

namespace h5i_app_h1.Recv

/-! ## What is decided -/

/-- A decision on the bytes received for a request. -/
inductive RA where
  | needRead
  /-- The head, the payload at `[start, fin)` (`payload`) and the message
  at `[0, used)`. -/
  | ready (h : Spec.Head) (start fin used : Nat) (payload : List Nat)
  | reject (status : Nat)
  | eof
  deriving DecidableEq, Repr

/-- Nothing decided on `len` bytes in a buffer of `cap`. -/
def undecidedS (len cap : Nat) (eof between : Bool) (full : Nat) : RA :=
  if cap ≤ len then .reject full
  else if eof then (if between then (if len = 0 then .eof else .reject 400) else .reject 400)
  else .needRead

/-- The body of `h`, from the wire bytes `W` after the head; `none` while
undecided. -/
def bodyS (h : Spec.Head) (W : List Nat) (cap mb : Nat) : Option RA :=
  match h.framing with
  | .empty => some (.ready h h.len h.len h.len [])
  | .length n =>
    if cap < h.len + n then some (.reject 413)
    else if n ≤ W.length then some (.ready h h.len (h.len + n) (h.len + n) (W.take n))
    else none
  | .chunked =>
    match Spec.dechunk W mb with
    | .done p c => some (.ready h h.len (h.len + p.length) (h.len + c) p)
    | .reject x => some (.reject x)
    | .more => none

/-- What the receive loop decides on the bytes `L` received for a request,
in a buffer of `cap` bytes; `eof`: no more will come. -/
def recvS (L : List Nat) (cap mh mb : Nat) (eof : Bool) : RA :=
  match parseHead L mh mb with
  | .reject s => .reject s
  | .incomplete => undecidedS L.length cap eof true 431
  | .done h =>
    match bodyS h (L.drop h.len) cap mb with
    | some a => a
    | none => undecidedS L.length cap eof false 413

/-- The extracted `Action` (with the buffer it leaves) is the decision `r`. -/
def Matches (a : Action) (buf : Slice U8) : RA → Prop
  | .needRead => a = .NeedRead
  | .eof => a = .Eof
  | .reject s => ∃ x : U16, a = .Reject x ∧ x.val = s
  | .ready hd s e u p => ∃ (h : Head) (s' e' u' : Usize), a = .Ready h s' e' u' ∧ headView h = hd ∧
      s'.val = s ∧ e'.val = e ∧ u'.val = u ∧ payload buf s e = p

/-! ## The state between calls -/

/-- The body state: for a chunked body, `ChunkInv` over the wire bytes after
the head; otherwise the buffer still holds the bytes received. -/
def BodyInv (mb : Nat) (h : Head) (bs : BodyState) (buf : Slice U8) (L : List Nat) : Prop :=
  match h.framing with
  | .Chunked => ChunkInv mb h.len.val L.length (L.drop h.len.val) (B buf.val) (csView bs.chunk) bs.r.val bs.w.val
  | _ => (B buf.val).take L.length = L

/-- `buf[..filled]` holds the bytes `L` received for this request, as far as
`st` says: all of them before the head is parsed; after, the head bytes and
the body as `BodyInv` says. -/
def RInv (lim : Limits) (st : RecvState) (buf : Slice U8) (filled : Nat) (L : List Nat) : Prop :=
  filled = L.length ∧ filled ≤ buf.length ∧
  match st with
  | .Head => (B buf.val).take filled = L
  | .Body h bs => parseHead L lim.max_head.val lim.max_body.val = .done (headView h) ∧
      (B buf.val).take h.len.val = L.take h.len.val ∧ BodyInv lim.max_body.val h bs buf L

/-- What a call leaves: the buffer keeps its length, the action is `recvS`
of `L`, a `NeedRead` state satisfies `RInv`, and a `Ready` request leaves
its head bytes and every byte from `used` to `filled` as received. -/
def StepPost (lim : Limits) (cap filled : Nat) (L : List Nat) (eof : Bool)
    (st' : RecvState) (a : Action) (buf' : Slice U8) : Prop :=
  buf'.length = cap ∧ Matches a buf' (recvS L cap lim.max_head.val lim.max_body.val eof) ∧
  (a = .NeedRead → RInv lim st' buf' filled L) ∧
  (∀ h s e u, a = .Ready h s e u →
    s.val ≤ e.val ∧ e.val ≤ u.val ∧ u.val ≤ filled ∧ (B buf'.val).take s.val = L.take s.val ∧
    ∀ i, u.val ≤ i → i < filled → (B buf'.val).getD i 0 = L.getD i 0)

/-! ## One call -/

theorem undecided_spec (buf : Slice U8) (filled cap : Usize) (eof between : Bool) (full : U16) :
    undecided filled cap eof between full ⦃ a => Matches a buf (undecidedS filled.val cap.val eof between full.val) ⦄ := by
  unfold undecided undecidedS
  by_cases h1 : cap.val ≤ filled.val
  · rw [if_pos (show filled >= cap by scalar_tac), if_pos h1]; exact ⟨full, rfl, rfl⟩
  · rw [if_neg (show ¬ filled >= cap by scalar_tac), if_neg h1]
    cases eof <;> cases between <;> simp only [if_true, if_false, Bool.false_eq_true]
    · simp [Matches]
    · simp [Matches]
    · exact ⟨_, rfl, by simp⟩
    · by_cases h0 : filled.val = 0
      · rw [if_pos (show filled = 0#usize by scalar_tac), if_pos h0]; simp [Matches]
      · rw [if_neg (show ¬ filled = 0#usize by scalar_tac), if_neg h0]; exact ⟨_, rfl, by simp⟩

theorem take_of_getD (A A' : List Nat) (n : Nat) (h1 : n ≤ A.length) (h2 : n ≤ A'.length)
    (h : ∀ i, i < n → A.getD i 0 = A'.getD i 0) : A.take n = A'.take n := by
  apply List.ext_getElem (by simp; omega)
  intro i hi hi'
  simp at hi
  have := h i (by omega)
  simp only [List.getD_eq_getElem?_getD, List.getElem?_eq_getElem (show i < A.length by omega),
    List.getElem?_eq_getElem (show i < A'.length by omega), Option.getD_some] at this
  simp [this]

theorem getD_of_take {A L : List Nat} {m i : Nat} (h : A.take m = L) (hi : i < m) : A.getD i 0 = L.getD i 0 := by
  rw [← h]; simp [List.getD_eq_getElem?_getD, hi]

theorem take_drop_take (A : List Nat) (s n m : Nat) (h : s + n ≤ m) :
    ((A.take m).drop s).take n = (A.drop s).take n := by
  apply List.ext_getElem?; intro i
  simp only [List.getElem?_take, List.getElem?_drop]
  by_cases hi : i < n
  · rw [if_pos hi, if_pos hi, if_pos (by omega)]
  · rw [if_neg hi, if_neg hi]

theorem getD_drop (L : List Nat) (s i : Nat) (h : s ≤ i) : (L.drop s).getD (i - s) 0 = L.getD i 0 := by
  simp only [List.getD_eq_getElem?_getD, List.getElem?_drop]
  rw [show s + (i - s) = i by omega]

theorem B_length (l : List U8) : (B l).length = l.length := by simp [B]

theorem undecidedS_not_ready {a : Action} {buf : Slice U8} {len cap : Nat} {eof between : Bool} {full : Nat}
    (hm : Matches a buf (undecidedS len cap eof between full)) : ∀ h s e u, a ≠ .Ready h s e u := by
  intro h s e u he
  subst he
  unfold undecidedS at hm
  split at hm
  · obtain ⟨_, h, _⟩ := hm; cases h
  · split at hm
    · split at hm
      · split at hm
        · cases hm
        · obtain ⟨_, h, _⟩ := hm; cases h
      · obtain ⟨_, h, _⟩ := hm; cases h
    · cases hm

/-- `recv_body`'s decision. -/
def bodyR (h : Head) (L : List Nat) (cap mb : Nat) (eof : Bool) : RA :=
  match bodyS (headView h) (L.drop h.len.val) cap mb with
  | some a => a
  | none => undecidedS L.length cap eof false 413

theorem recv_body_spec (h : Head) (bs : BodyState) (buf : Slice U8) (filled : Usize) (eof : Bool) (mb : U32)
    (L : List Nat) (hf : filled.val = L.length) (hcap : filled.val ≤ buf.length) (hlen : h.len.val ≤ filled.val)
    (hhead : (B buf.val).take h.len.val = L.take h.len.val) (hI : BodyInv mb.val h bs buf L) :
    recv_body h bs buf filled eof mb ⦃ r buf' =>
      buf'.length = buf.length ∧ Matches r.2 buf' (bodyR h L buf.length mb.val eof) ∧
      (r.2 = .NeedRead → ∃ bs', r.1 = .Body h bs' ∧ (B buf'.val).take h.len.val = L.take h.len.val ∧
        BodyInv mb.val h bs' buf' L) ∧
      (∀ h' s e u, r.2 = .Ready h' s e u →
        s.val ≤ e.val ∧ e.val ≤ u.val ∧ u.val ≤ filled.val ∧ (B buf'.val).take s.val = L.take s.val ∧
        ∀ i, u.val ≤ i → i < filled.val → (B buf'.val).getD i 0 = L.getD i 0) ⦄ := by
  unfold recv_body
  have hbl := B_length buf.val
  rcases hfr : h.framing with _ | n | _
  · simp only [BodyInv, hfr] at hI
    step with read_body_empty buf filled h.len bs mb
    subst rx1 bs2_post out_post
    simp only [WP.spec_ok, WP.uncurry'_pair]
    have hbr : bodyR h L buf.length mb.val eof = .ready (headView h) h.len.val h.len.val h.len.val [] := by
      simp [bodyR, bodyS, headView, framingView, hfr]
    rw [hbr]
    refine ⟨by trivial, ⟨h, h.len, h.len, h.len, rfl, rfl, rfl, rfl, rfl, by simp [payload]⟩, by simp, ?_⟩
    rintro h' s e u he
    simp only [Action.Ready.injEq] at he
    obtain ⟨rfl, rfl, rfl, rfl⟩ := he
    exact ⟨le_refl _, le_refl _, hlen, hhead, fun i _ hi' => getD_of_take hI (by omega)⟩
  · simp only [BodyInv, hfr] at hI
    step with read_body_length buf filled h.len n bs mb
    subst rx1 bs2_post
    have h2 := bs2_post1
    cases out with
    | Done e u =>
      obtain ⟨he, hue, hef⟩ := h2
      rw [hue]
      simp only [WP.spec_ok, WP.uncurry'_pair]
      have hbr : bodyR h L buf.length mb.val eof = .ready (headView h) h.len.val (h.len.val + n.val)
          (h.len.val + n.val) ((L.drop h.len.val).take n.val) := by
        simp only [bodyR, bodyS, headView, framingView, hfr]
        rw [if_neg (by omega), if_pos (by simp; omega)]
      rw [hbr]
      refine ⟨by trivial, ⟨h, h.len, e, e, rfl, rfl, rfl, he, he, ?_⟩, by simp, ?_⟩
      · simp only [payload]
        rw [show h.len.val + n.val - h.len.val = n.val by omega, ← take_drop_take _ _ _ L.length (by omega), hI]
      · rintro h' s e' u he'
        simp only [Action.Ready.injEq] at he'
        obtain ⟨rfl, rfl, rfl, rfl⟩ := he'
        exact ⟨by omega, by omega, by omega, hhead, fun i _ hi' => getD_of_take hI (by omega)⟩
    | More =>
      simp only at h2
      step with undecided_spec buf filled buf.len eof false PAYLOAD_TOO_LARGE as ⟨a, hm⟩
      try simp only [WP.spec_ok, WP.uncurry'_pair]
      have hbr : bodyR h L buf.length mb.val eof = undecidedS L.length buf.length eof false 413 := by
        simp only [bodyR, bodyS, headView, framingView, hfr]
        rw [if_neg (by omega), if_neg (by simp; omega)]
      rw [hbr]
      rw [hf, Slice.len_val, payload_too_large_val] at hm
      refine ⟨hm, fun _ => ⟨bs2, rfl, hhead, by simp only [BodyInv, hfr]; exact hI⟩, ?_⟩
      intro h' s e u he
      exact absurd he (undecidedS_not_ready hm h' s e u)
    | Reject x =>
      simp only [WP.spec_ok, WP.uncurry'_pair]
      have hbr : bodyR h L buf.length mb.val eof = .reject 413 := by
        simp only [bodyR, bodyS, headView, framingView, hfr]
        rw [if_pos (by omega)]
      rw [hbr]
      refine ⟨by trivial, ⟨x, rfl, h2.1⟩, by simp, by simp⟩
  · simp only [BodyInv, hfr] at hI
    rw [← hf] at hI
    step with read_body_chunked buf filled h.len bs mb (L.drop h.len.val) hI
    have hpost := bs2_post
    have hd := bs2_post1
    obtain ⟨hl', hw1, hw2, hr1, hrr, hun, hm⟩ := hpost
    try simp only at hl' hw1 hw2 hr1 hrr hun hm hd
    have hun0 := hI.unread
    have hle := hI.le
    have hbl' := B_length rx1.val
    have hhead' : (B rx1.val).take h.len.val = L.take h.len.val := by
      rw [← hhead]
      apply take_of_getD _ _ _ (by simp [Slice.length] at hl' ⊢; omega) (by omega)
      intro i hi; exact hun i (Or.inl (by omega))
    cases out with
    | Done e u =>
      obtain ⟨rfl, rfl, -⟩ := hm
      simp only [WP.spec_ok, WP.uncurry'_pair]
      have hbr : bodyR h L buf.length mb.val eof = .ready (headView h) h.len.val
          (h.len.val + (payload rx1 h.len.val bs2.w.val).length) (h.len.val + (bs2.r.val - h.len.val))
          (payload rx1 h.len.val bs2.w.val) := by
        simp only [bodyR, bodyS, headView, framingView, hfr, hd]
      rw [hbr]
      have hpl : (payload rx1 h.len.val bs2.w.val).length = bs2.w.val - h.len.val := by
        simp [payload, Slice.length] at hl' ⊢; omega
      refine ⟨hl', ⟨h, h.len, bs2.w, bs2.r, rfl, rfl, rfl, by omega, by omega, ?_⟩, by simp, ?_⟩
      · rw [hpl, show h.len.val + (bs2.w.val - h.len.val) = bs2.w.val by omega]
      · rintro h' s e u he
        simp only [Action.Ready.injEq] at he
        obtain ⟨rfl, rfl, rfl, rfl⟩ := he
        refine ⟨by omega, by omega, by omega, hhead', fun i hi hi' => ?_⟩
        rw [hun i (Or.inr hi), hun0 i (by omega) hi', getD_drop _ _ _ (by omega)]
    | More =>
      obtain ⟨hrf, hI'⟩ := hm
      step with undecided_spec rx1 filled rx1.len eof false PAYLOAD_TOO_LARGE as ⟨a, hm⟩
      try simp only [WP.spec_ok, WP.uncurry'_pair]
      have hbr : bodyR h L buf.length mb.val eof = undecidedS L.length buf.length eof false 413 := by
        simp only [bodyR, bodyS, headView, framingView, hfr, hd]
      rw [hbr]
      rw [hf, Slice.len_val, payload_too_large_val, hl'] at hm
      refine ⟨hl', hm, fun _ => ⟨bs2, rfl, hhead', ?_⟩, ?_⟩
      · simp only [BodyInv, hfr]; rw [← hf]; exact hI'
      · intro h' s e u he
        exact absurd he (undecidedS_not_ready hm h' s e u)
    | Reject x =>
      simp only [WP.spec_ok, WP.uncurry'_pair]
      have hbr : bodyR h L buf.length mb.val eof = .reject x.val := by
        simp only [bodyR, bodyS, headView, framingView, hfr, hd]
      rw [hbr]
      exact ⟨hl', ⟨x, rfl, rfl⟩, by simp, by simp⟩

theorem readLimit_le (len mh : Nat) : readLimit len mh ≤ len := by
  unfold readLimit; split <;> omega

theorem head_len_le {L : List Nat} {mh mb : Nat} {h : Head} (hp : parseHead L mh mb = .done (headView h)) :
    h.len.val ≤ L.length := by
  have := (H1Props.accept_wellformed L mh mb (headView h) hp).1
  have := readLimit_le L.length mh
  simp only [headView] at *
  omega

/-- The body part of a call. -/
theorem recv_body_step (lim : Limits) (h : Head) (bs : BodyState) (buf : Slice U8) (filled : Usize) (eof : Bool)
    (L : List Nat) (hf : filled.val = L.length) (hcap : filled.val ≤ buf.length)
    (hp : parseHead L lim.max_head.val lim.max_body.val = .done (headView h))
    (hhead : (B buf.val).take h.len.val = L.take h.len.val) (hI : BodyInv lim.max_body.val h bs buf L) :
    recv_body h bs buf filled eof lim.max_body ⦃ r buf' => StepPost lim buf.length filled.val L eof r.1 r.2 buf' ⦄ := by
  have hlen : h.len.val ≤ filled.val := by rw [hf]; exact head_len_le hp
  apply WP.spec_mono (recv_body_spec h bs buf filled eof lim.max_body L hf hcap hlen hhead hI)
  rintro ⟨⟨st', a⟩, buf'⟩ ⟨h1, h2, h3, h4⟩
  have hr : recvS L buf.length lim.max_head.val lim.max_body.val eof = bodyR h L buf.length lim.max_body.val eof := by
    simp only [recvS, hp]; rfl
  refine ⟨h1, by rw [hr]; exact h2, fun hn => ?_, h4⟩
  obtain ⟨bs', hbs, hh, hb⟩ := h3 hn
  simp only at hbs
  subst hbs
  exact ⟨hf, by omega, hp, hh, hb⟩

theorem B_slice (l : List U8) (n : Nat) : B (l.slice 0 n) = (B l).take n := by
  simp [B, List.map_take]

/-- One call of `recv_step`. -/
theorem recv_step_spec (lim : Limits) (st : RecvState) (buf : Slice U8) (filled : Usize) (eof : Bool) (L : List Nat)
    (hI : RInv lim st buf filled.val L) :
    recv_step st buf filled eof lim ⦃ r buf' => StepPost lim buf.length filled.val L eof r.1 r.2 buf' ⦄ := by
  obtain ⟨hf, hcap, hst⟩ := hI
  unfold recv_step
  rw [if_neg (by scalar_tac)]
  cases st with
  | Head =>
    simp only at hst
    step as ⟨sl, hs1, hs2⟩
    have hsl : B sl.val = L := by rw [hs1, B_slice, hst]
    step with Proofs.parse_head_spec sl lim as ⟨hr, hv, _⟩
    rw [hsl] at hv
    cases hr with
    | Incomplete =>
      step with undecided_spec buf filled buf.len eof true HEADERS_TOO_LARGE as ⟨a, hm⟩
      rw [hf, Slice.len_val, headers_too_large_val] at hm
      refine ⟨rfl, ?_, fun _ => ⟨hf, hcap, hst⟩, fun h s e u he => absurd he (undecidedS_not_ready hm h s e u)⟩
      simp only [recvS, ← hv]
      exact hm
    | Reject x =>
      simp only [WP.spec_ok, WP.uncurry'_pair]
      refine ⟨rfl, ?_, by simp, by simp⟩
      simp only [recvS, ← hv]
      exact ⟨x, rfl, rfl⟩
    | Done h =>
      step with body_start_spec h.len as ⟨bs, hb1, hb2, hb3⟩
      have hp : parseHead L lim.max_head.val lim.max_body.val = .done (headView h) := hv.symm
      have hlen : h.len.val ≤ L.length := head_len_le hp
      have hhead : (B buf.val).take h.len.val = L.take h.len.val := by
        rw [← hst, List.take_take, Nat.min_eq_left (by omega)]
      apply recv_body_step lim h bs buf filled eof L hf hcap hp hhead
      unfold BodyInv
      split
      · rename_i hfr
        have := inv_start lim.max_body.val h.len.val filled.val (B buf.val) (by omega) (by rw [B_length]; exact hcap)
        rw [hb1, hb2, hb3, ← hf]
        rw [← hst, List.drop_take]
        exact this
      · rw [← hf]; exact hst
  | Body h bs =>
    obtain ⟨hp, hhead, hb⟩ := hst
    exact recv_body_step lim h bs buf filled eof L hf hcap hp hhead hb

/-! ## Reading more -/

/-- A read appends `x` at `filled` and changes nothing below it: `RInv`
holds for the bytes received plus `x`. -/
theorem RInv.extend {lim : Limits} {st : RecvState} {buf buf2 : Slice U8} {filled filled2 : Nat} {L x : List Nat}
    (hI : RInv lim st buf filled L) (hl : buf2.length = buf.length) (hf2 : filled2 = filled + x.length)
    (hc : filled2 ≤ buf2.length) (hx : (B buf2.val).take filled2 = (B buf.val).take filled ++ x) :
    RInv lim st buf2 filled2 (L ++ x) := by
  obtain ⟨hf, hcap, hst⟩ := hI
  have hbl := B_length buf.val
  have hbl2 := B_length buf2.val
  have e1 : buf.length = buf.val.length := rfl
  have e2 : buf2.length = buf2.val.length := rfl
  have htf : ((B buf.val).take filled).length = filled := by simp; omega
  have hbelow : ∀ i, i < filled → (B buf2.val).getD i 0 = (B buf.val).getD i 0 := by
    intro i hi
    have h1 : (B buf2.val).getD i 0 = ((B buf2.val).take filled2).getD i 0 := by
      simp [List.getD_eq_getElem?_getD, List.getElem?_take, show i < filled2 by omega]
    rw [h1, hx, List.getD_eq_getElem?_getD, List.getElem?_append_left (by omega)]
    simp [List.getD_eq_getElem?_getD, List.getElem?_take, hi]
  have hxv : ((B buf2.val).drop filled).take (filled2 - filled) = x := by
    rw [← List.drop_take, hx, List.drop_append_of_le_length (by omega), List.drop_eq_nil_of_le (by omega)]
    rfl
  have htake : (B buf2.val).take filled2 = (B buf.val).take filled ++ x → (B buf.val).take filled = L →
      (B buf2.val).take filled2 = L ++ x := fun h1 h2 => by rw [h1, h2]
  refine ⟨by simp; omega, hc, ?_⟩
  cases st with
  | Head => exact htake hx hst
  | Body h bs =>
    obtain ⟨hp, hhead, hb⟩ := hst
    have hlen : h.len.val ≤ L.length := head_len_le hp
    refine ⟨?_, ?_, ?_⟩
    · rw [H1Props.parseHead_prefix L x _ _ (by rw [hp]; simp)]; exact hp
    · rw [List.take_append_of_le_length hlen, ← hhead]
      exact take_of_getD _ _ _ (by omega) (by omega) (fun i hi => hbelow i (by omega))
    · unfold BodyInv at hb ⊢
      split
      · rename_i hfr
        simp only [hfr] at hb
        have := inv_extend hb (show L.length ≤ filled2 by omega) (by rw [hbl2]; exact hc)
          (fun i hi => hbelow i (by omega))
        rw [List.length_append, List.drop_append_of_le_length hlen]
        rw [← hf, hxv, show filled2 = L.length + x.length by omega] at this
        exact this
      · rename_i hfr
        have hb' : (B buf.val).take L.length = L := by
          revert hb; cases hh : h.framing <;> simp_all
        rw [List.length_append, ← show filled2 = L.length + x.length by omega]
        exact htake hx (by rw [hf]; exact hb')

/-! ## Driving the loop -/

/-- The action with the payload it points to. -/
def actView : Action → Slice U8 → RA
  | .NeedRead, _ => .needRead
  | .Eof, _ => .eof
  | .Reject x, _ => .reject x.val
  | .Ready h s e u, buf => .ready (headView h) s.val e.val u.val (payload buf s.val e.val)

theorem Matches.view {a : Action} {buf : Slice U8} {r : RA} (h : Matches a buf r) : actView a buf = r := by
  cases r with
  | needRead => simp only [Matches] at h; subst h; rfl
  | eof => simp only [Matches] at h; subst h; rfl
  | reject s => obtain ⟨x, rfl, rfl⟩ := h; rfl
  | ready hd s e u p => obtain ⟨h, s', e', u', rfl, rfl, rfl, rfl, rfl, rfl⟩ := h; rfl

/-- The server's loop over the extracted `recv_step`, with the reads `xs`
it made: call; on `NeedRead` read `x` into `buf[filled..]` (changing
nothing below `filled`) and call again; a read of nothing (`[]`, the peer
closed) is followed by one call with `eof`. The last call's action and
buffer are `a` and `buf'`. -/
inductive Drive (lim : Limits) : RecvState → Slice U8 → Usize → List (List Nat) → Action → Slice U8 → Usize → Prop
  | last {st buf filled st' a buf'} :
      recv_step st buf filled false lim = ok ((st', a), buf') →
      Drive lim st buf filled [] a buf' filled
  | eof {st buf filled st' buf' st'' a buf''} :
      recv_step st buf filled false lim = ok ((st', .NeedRead), buf') →
      recv_step st' buf' filled true lim = ok ((st'', a), buf'') →
      Drive lim st buf filled [[]] a buf'' filled
  | read {st buf filled st' buf' buf2 filled2 x xs a buf3 filled3} :
      recv_step st buf filled false lim = ok ((st', .NeedRead), buf') →
      x ≠ [] → buf2.length = buf'.length → filled2.val = filled.val + x.length → filled2.val ≤ buf2.length →
      (B buf2.val).take filled2.val = (B buf'.val).take filled.val ++ x →
      Drive lim st' buf2 filled2 xs a buf3 filled3 →
      Drive lim st buf filled (x :: xs) a buf3 filled3

/-- Whether the reads end with the peer closing. -/
def closes (xs : List (List Nat)) : Bool := decide ([] ∈ xs)

theorem step_post {lim : Limits} {st st' : RecvState} {buf buf' : Slice U8} {filled : Usize} {eof : Bool}
    {L : List Nat} {a : Action} (hI : RInv lim st buf filled.val L)
    (h : recv_step st buf filled eof lim = ok ((st', a), buf')) :
    StepPost lim buf.length filled.val L eof st' a buf' := by
  obtain ⟨y, hy, hp⟩ := WP.spec_imp_exists (recv_step_spec lim st buf filled eof L hI)
  rw [h] at hy
  have := Result.ok_injective hy
  subst this
  exact hp

/-- Driving `recv_step` over the reads `xs` from bytes `L` already in the
buffer: the last action is `recvS` of `L` and every byte read, and it
leaves the head bytes and the bytes from `used` on as received. -/
theorem drive_recvS {lim : Limits} {st : RecvState} {buf : Slice U8} {filled : Usize} {xs : List (List Nat)}
    {a : Action} {buf' : Slice U8} {filled' : Usize} {L : List Nat}
    (hd : Drive lim st buf filled xs a buf' filled') (hI : RInv lim st buf filled.val L) :
    filled'.val = (L ++ xs.flatten).length ∧ filled'.val ≤ buf.length ∧
    ∃ st', StepPost lim buf.length filled'.val (L ++ xs.flatten) (closes xs) st' a buf' := by
  induction hd generalizing L with
  | @last st buf filled st' a buf' h =>
    refine ⟨by simp; exact hI.1, hI.2.1, st', ?_⟩
    simp only [List.flatten_nil, List.append_nil, closes, List.not_mem_nil, decide_false]
    exact step_post hI h
  | @eof st buf filled st' buf' st'' a buf'' h1 h2 =>
    have p1 := step_post hI h1
    have p2 := step_post (p1.2.2.1 rfl) h2
    refine ⟨by simp; exact hI.1, hI.2.1, st'', ?_⟩
    simp only [List.flatten_cons, List.flatten_nil, List.append_nil, closes, List.mem_cons, true_or,
      decide_true]
    rw [p1.1] at p2
    exact p2
  | @read st buf filled st' buf' buf2 filled2 x xs a buf3 filled3 h1 hx hl hf2 hc htake _ ih =>
    have p1 := step_post hI h1
    have hI2 := RInv.extend (p1.2.2.1 rfl) hl hf2 hc htake
    obtain ⟨hf, hfc, st'', hp⟩ := ih hI2
    have hcl : closes (x :: xs) = closes xs := by
      simp [closes, List.mem_cons, Ne.symm hx]
    refine ⟨by rw [hf]; simp, by rw [hl, p1.1] at hfc; exact hfc, st'', ?_⟩
    rw [hl, p1.1] at hp
    rw [hcl, List.flatten_cons, ← List.append_assoc]
    exact hp

/-! ## More bytes after a decision -/

theorem undecidedS_eof {len cap : Nat} {between : Bool} {full : Nat} (e : Bool)
    (h : undecidedS len cap false between full ≠ .needRead) :
    undecidedS len cap e between full = undecidedS len cap false between full ∧ cap ≤ len := by
  unfold undecidedS at *
  by_cases hc : cap ≤ len
  · simp [hc]
  · simp [hc] at h

/-- Undecided only with room to read, and only before the peer closed. -/
theorem recvS_needRead {L : List Nat} {cap mh mb : Nat} {e : Bool} (h : recvS L cap mh mb e = .needRead) :
    L.length < cap ∧ e = false := by
  have hu : ∀ {len b f}, undecidedS len cap e b f = .needRead → len < cap ∧ e = false := by
    intro len b f hu
    unfold undecidedS at hu
    by_cases hc : cap ≤ len
    · simp [hc] at hu
    · refine ⟨by omega, ?_⟩
      cases e
      · rfl
      · exfalso
        by_cases h0 : len = 0 <;> cases b <;> simp [hc, h0] at hu <;> split at hu <;> cases hu
  unfold recvS at h
  split at h
  · cases h
  · exact hu h
  · split at h
    · rename_i a hb
      subst h
      unfold bodyS at hb
      split at hb
      · cases hb
      · split at hb
        · cases hb
        · split at hb
          · cases hb
          · cases hb
      · split at hb <;> cases hb
    · exact hu h

theorem recvS_eof (L : List Nat) (cap mh mb : Nat) (e : Bool) (h : recvS L cap mh mb false ≠ .needRead) :
    recvS L cap mh mb e = recvS L cap mh mb false := by
  unfold recvS at *
  rcases hp : parseHead L mh mb with _ | s | hh <;> simp only [hp] at h ⊢
  · exact (undecidedS_eof e h).1
  · rcases hb : bodyS hh (L.drop hh.len) cap mb with _ | a <;> simp only [hb] at h ⊢
    exact (undecidedS_eof e h).1

theorem dechunk_append (W R : List Nat) (mb : Nat) (h : Spec.dechunk W mb ≠ .more) :
    Spec.dechunk (W ++ R) mb = Spec.dechunk W mb := by
  rw [← feed_dechunk, ← feed_dechunk, feed_append]
  rw [← feed_dechunk] at h
  rcases hf : feed cStart W mb [] 0 with ⟨s, a⟩ | x | ⟨p, k⟩
  · rw [hf] at h; exact absurd rfl h
  · rfl
  · rfl

theorem bodyS_append (h : Spec.Head) (W R : List Nat) (cap mb : Nat) (a : RA) (hb : bodyS h W cap mb = some a) :
    bodyS h (W ++ R) cap mb = some a := by
  unfold bodyS at *
  rcases hf : h.framing with _ | n | _ <;> simp only [hf] at hb ⊢
  · exact hb
  · by_cases h1 : cap < h.len + n
    · simp only [h1, if_true] at hb ⊢; exact hb
    · simp only [h1, if_false] at hb ⊢
      by_cases h2 : n ≤ W.length
      · simp only [h2, if_true] at hb
        rw [if_pos (by simp; omega), List.take_append_of_le_length h2]; exact hb
      · simp [h2] at hb
  · rcases hd : Spec.dechunk W mb with _ | x | ⟨p, c⟩ <;> simp only [hd] at hb
    · cases hb
    · rw [dechunk_append W R mb (by rw [hd]; simp), hd]; exact hb
    · rw [dechunk_append W R mb (by rw [hd]; simp), hd]; exact hb

/-- Once `recvS` decides on `L`, bytes after `L` (that fit) and the peer
closing do not change the decision. -/
theorem recvS_prefix (L R : List Nat) (cap mh mb : Nat) (e : Bool) (hd : recvS L cap mh mb false ≠ .needRead)
    (hfit : (L ++ R).length ≤ cap) : recvS (L ++ R) cap mh mb e = recvS L cap mh mb false := by
  by_cases hR : R = []
  · subst hR; rw [List.append_nil]; exact recvS_eof L cap mh mb e hd
  have hRl : 0 < R.length := List.length_pos_iff.mpr hR
  simp only [List.length_append] at hfit
  unfold recvS at hd ⊢
  rcases hp : parseHead L mh mb with _ | s | hh
  · simp only [hp] at hd; have := (undecidedS_eof false hd).2; omega
  · rw [H1Props.parseHead_prefix L R mh mb (by rw [hp]; simp), hp]
  · rw [H1Props.parseHead_prefix L R mh mb (by rw [hp]; simp), hp]
    simp only [hp] at hd ⊢
    have hlen : hh.len ≤ L.length := by
      have := (H1Props.accept_wellformed L mh mb hh hp).1
      have := readLimit_le L.length mh
      omega
    rw [List.drop_append_of_le_length hlen]
    rcases hb : bodyS hh (L.drop hh.len) cap mb with _ | a
    · simp only [hb] at hd; have := (undecidedS_eof false hd).2; omega
    · rw [bodyS_append _ _ _ _ _ _ hb]

/-! ## Any split of the reads -/

theorem actView_needRead {a : Action} {buf : Slice U8} : actView a buf = .needRead ↔ a = .NeedRead := by
  cases a <;> simp [actView]

/-- What the loop decides after the reads `xs`: `recvS` of every byte read. -/
theorem drive_view {lim : Limits} {buf : Slice U8} {filled : Usize} {xs : List (List Nat)}
    {a : Action} {buf' : Slice U8} {filled' : Usize} {P : List Nat}
    (hd : Drive lim .Head buf filled xs a buf' filled') (hP : (B buf.val).take filled.val = P)
    (hc : filled.val ≤ buf.length) :
    actView a buf' = recvS (P ++ xs.flatten) buf.length lim.max_head.val lim.max_body.val (closes xs) := by
  have hI : RInv lim .Head buf filled.val P := ⟨by rw [← hP]; simp; omega, hc, hP⟩
  obtain ⟨-, -, _, hp⟩ := drive_recvS hd hI
  exact hp.2.1.view

/-- Segmentation invariance: two sequences of reads with the same bytes
(and both closing or neither) give the same action, payload and `filled`. -/
theorem drive_split {lim : Limits} {buf : Slice U8} {filled : Usize} {xs ys : List (List Nat)}
    {a b : Action} {buf1 buf2 : Slice U8} {f1 f2 : Usize} {P : List Nat}
    (h1 : Drive lim .Head buf filled xs a buf1 f1) (h2 : Drive lim .Head buf filled ys b buf2 f2)
    (hP : (B buf.val).take filled.val = P) (hc : filled.val ≤ buf.length)
    (hxy : xs.flatten = ys.flatten) (hcl : closes xs = closes ys) :
    actView a buf1 = actView b buf2 ∧ f1.val = f2.val := by
  have hI : RInv lim .Head buf filled.val P := ⟨by rw [← hP]; simp; omega, hc, hP⟩
  refine ⟨by rw [drive_view h1 hP hc, drive_view h2 hP hc, hxy, hcl], ?_⟩
  rw [(drive_recvS h1 hI).1, (drive_recvS h2 hI).1, hxy]

/-- The same as all the bytes at once: if the loop decided after the reads
`xs`, one call on a buffer holding those bytes and any more `R` after them
(none if the peer closed) gives the same action and payload. -/
theorem drive_at_once {lim : Limits} {buf : Slice U8} {filled : Usize} {xs : List (List Nat)}
    {a : Action} {buf' : Slice U8} {filled' : Usize} {P : List Nat}
    (hd : Drive lim .Head buf filled xs a buf' filled') (hP : (B buf.val).take filled.val = P)
    (hc : filled.val ≤ buf.length) (hdec : a ≠ .NeedRead)
    (bufA : Slice U8) (fA : Usize) (R : List Nat) (e : Bool) (hlen : bufA.length = buf.length)
    (hA : (B bufA.val).take fA.val = P ++ xs.flatten ++ R) (hfA : fA.val ≤ bufA.length)
    (hcl : closes xs = true → R = [] ∧ e = true) :
    recv_step .Head bufA fA e lim ⦃ r bufA' => actView r.2 bufA' = actView a buf' ⦄ := by
  have hv := drive_view hd hP hc
  have hIA : RInv lim .Head bufA fA.val (P ++ xs.flatten ++ R) := ⟨by rw [← hA]; simp; omega, hfA, hA⟩
  apply WP.spec_mono (recv_step_spec lim .Head bufA fA e _ hIA)
  rintro ⟨⟨st', a'⟩, bufA'⟩ hp
  simp only [WP.uncurry'_pair] at hp ⊢
  rw [hp.2.1.view, hv, hlen]
  cases hx : closes xs
  · rw [hx] at hv
    apply recvS_prefix
    · rw [← hv]; intro h; exact hdec (actView_needRead.1 h)
    · rw [← hlen, ← hA]; simp; try omega
  · obtain ⟨rfl, rfl⟩ := hcl hx
    rw [List.append_nil]

/-- The loop reads only with room left in the buffer: on `NeedRead`,
`filled < buf.length`. -/
theorem drive_room {lim : Limits} {buf : Slice U8} {filled : Usize} {xs : List (List Nat)}
    {buf' : Slice U8} {filled' : Usize} {P : List Nat}
    (hd : Drive lim .Head buf filled xs .NeedRead buf' filled') (hP : (B buf.val).take filled.val = P)
    (hc : filled.val ≤ buf.length) : filled'.val < buf.length := by
  have hI : RInv lim .Head buf filled.val P := ⟨by rw [← hP]; simp; omega, hc, hP⟩
  have hv := drive_view hd hP hc
  have := (recvS_needRead hv.symm).1
  rw [(drive_recvS hd hI).1]; exact this

/-! ## The next request -/

theorem carry_loop_spec (rx : Slice U8) (used n i : Usize) (src : List Nat)
    (hn : used.val + n.val ≤ rx.length) (hi : i.val ≤ n.val)
    (hlo : ∀ k, k < i.val → (B rx.val).getD k 0 = src.getD (used.val + k) 0)
    (hhi : ∀ k, i.val ≤ k → (B rx.val).getD k 0 = src.getD k 0) :
    carry_loop rx used n i ⦃ rx' => rx'.length = rx.length ∧
      ∀ k, k < n.val → (B rx'.val).getD k 0 = src.getD (used.val + k) 0 ⦄ := by
  unfold carry_loop
  apply loop.spec_decr_nat
    (measure := fun (x : Slice U8 × Usize) => n.val - x.2.val)
    (inv := fun x => x.1.length = rx.length ∧ x.2.val ≤ n.val ∧
      (∀ k, k < x.2.val → (B x.1.val).getD k 0 = src.getD (used.val + k) 0) ∧
      (∀ k, x.2.val ≤ k → (B x.1.val).getD k 0 = src.getD k 0))
  · rintro ⟨r1, i1⟩ ⟨hl, hi1, hlo1, hhi1⟩
    simp only at hl hi1 hlo1 hhi1 ⊢
    unfold carry_loop.body
    split
    · rename_i hlt
      step as ⟨j, hj⟩
      step as ⟨x, hx⟩
      step as ⟨r2, hr2⟩
      step as ⟨i2, hi2⟩
      have hxv : (B r1.val).getD j.val 0 = x.val := B_getD r1 j.val x (by scalar_tac) hx.symm
      have hset : B r2.val = (B r1.val).set i1.val x.val := by rw [hr2]; simp [B, List.map_set]
      have hl2 : r2.length = rx.length := by rw [hr2]; simp [Slice.length]; exact hl
      try simp only [WP.spec_ok]
      refine ⟨hl2, by scalar_tac, fun k hk => ?_, fun k hk => ?_, by scalar_tac⟩
      · rw [hset]
        by_cases hk' : k = i1.val
        · subst hk'
          have : i1.val < (B r1.val).length := by rw [B_length]; simp [Slice.length] at hl; scalar_tac
          rw [List.getD_eq_getElem?_getD, List.getElem?_set_self this, Option.getD_some, ← hxv,
            hhi1 j.val (by scalar_tac), hj]
        · rw [getD_set_ne _ _ _ _ hk']; exact hlo1 k (by scalar_tac)
      · rw [hset, getD_set_ne _ _ _ _ (by scalar_tac)]; exact hhi1 k (by scalar_tac)
    · simp only [WP.spec_ok]
      have : i1.val = n.val := by scalar_tac
      exact ⟨hl, fun k hk => hlo1 k (by omega)⟩
  · exact ⟨rfl, hi, hlo, hhi⟩

/-- `carry`: the bytes `rx[used..filled]` move to the start. -/
theorem carry_spec (rx : Slice U8) (used filled : Usize) (hu : used.val ≤ filled.val) (hf : filled.val ≤ rx.length) :
    carry rx used filled ⦃ n rx' => n.val = filled.val - used.val ∧ rx'.length = rx.length ∧
      (B rx'.val).take n.val = ((B rx.val).drop used.val).take n.val ⦄ := by
  unfold carry
  rw [if_neg (by scalar_tac), if_neg (by scalar_tac)]
  step as ⟨n, hn⟩
  step with carry_loop_spec rx used n 0#usize (B rx.val) (by scalar_tac) (by simp)
    (fun k hk => by simp at hk) (fun k _ => rfl) as ⟨rx', hl, hk2⟩
  refine ⟨hn, hl, ?_⟩
  apply take_of_getD _ _ _ (by rw [B_length]; simp [Slice.length] at hl; scalar_tac) (by simp; scalar_tac)
  intro k hk
  rw [hk2 k hk]
  simp [List.getD_eq_getElem?_getD, List.getElem?_drop]

/-- After a request that used `rx[..u]`, `carry` leaves the bytes received
after it, `L[u..]`, as the start of the next request. -/
theorem next_request (lim : Limits) (buf : Slice U8) (filled u : Usize) (L : List Nat)
    (hl : filled.val = L.length) (hu : u.val ≤ filled.val) (hc : filled.val ≤ buf.length)
    (hkeep : ∀ i, u.val ≤ i → i < filled.val → (B buf.val).getD i 0 = L.getD i 0) :
    carry buf u filled ⦃ n buf' => RInv lim .Head buf' n.val (L.drop u.val) ⦄ := by
  apply WP.spec_mono (carry_spec buf u filled hu hc)
  rintro ⟨n, buf'⟩ ⟨hn, hbl, ht⟩
  refine ⟨by simp; omega, by omega, ?_⟩
  rw [ht]
  have : (L.drop u.val).take n.val = L.drop u.val := List.take_of_length_le (by simp; omega)
  rw [← this]
  apply take_of_getD _ _ _ (by simp [B_length]; simp [Slice.length] at hc; omega) (by simp; omega)
  intro k hk
  simp only [List.getD_eq_getElem?_getD, List.getElem?_drop]
  have := hkeep (u.val + k) (by omega) (by omega)
  simp only [List.getD_eq_getElem?_getD] at this
  exact this

/-- The whole hand-over: after the loop returns `Ready` with `used`, the
next request starts with exactly the bytes received after `used`. -/
theorem drive_next {lim : Limits} {buf : Slice U8} {filled : Usize} {xs : List (List Nat)}
    {buf' : Slice U8} {filled' : Usize} {P : List Nat} {h : Head} {s e u : Usize}
    (hd : Drive lim .Head buf filled xs (.Ready h s e u) buf' filled') (hP : (B buf.val).take filled.val = P)
    (hc : filled.val ≤ buf.length) :
    carry buf' u filled' ⦃ n buf'' => RInv lim .Head buf'' n.val ((P ++ xs.flatten).drop u.val) ⦄ := by
  have hI : RInv lim .Head buf filled.val P := ⟨by rw [← hP]; simp; omega, hc, hP⟩
  obtain ⟨hf, hfc, st', hp⟩ := drive_recvS hd hI
  obtain ⟨-, -, hu, -, hkeep⟩ := hp.2.2.2 h s e u rfl
  exact next_request lim buf' filled' u _ hf hu (by rw [hp.1]; exact hfc) hkeep

/-! ## Limits -/

/-- `fit_limits`: a whole request fits in a buffer of `cap` bytes. -/
theorem fit_limits_spec (lim : Limits) (cap : Usize) :
    fit_limits lim cap ⦃ l => l.max_head.val ≤ cap.val ∧ l.max_head.val + l.max_body.val ≤ cap.val ∧
      l.max_head.val ≤ lim.max_head.val ∧ l.max_body.val ≤ lim.max_body.val ⦄ := by
  unfold fit_limits
  have key : ∀ mh : Usize, mh.val ≤ cap.val → mh.val ≤ lim.max_head.val →
      (do
        let rest ← cap - mh
        let rest32 ← if rest < 4294967295#usize then ok (UScalar.cast UScalarTy.U32 rest) else ok 4294967295#u32
        if lim.max_body < rest32 then ok ({ max_head := mh, max_body := lim.max_body } : Limits)
          else ok ({ max_head := mh, max_body := rest32 } : Limits)) ⦃ l =>
      l.max_head.val ≤ cap.val ∧ l.max_head.val + l.max_body.val ≤ cap.val ∧
      l.max_head.val ≤ lim.max_head.val ∧ l.max_body.val ≤ lim.max_body.val ⦄ := by
    intro mh h1 h2
    step as ⟨rest, hrest⟩
    by_cases h3 : rest < 4294967295#usize
    · rw [if_pos h3]
      have hc : (UScalar.cast UScalarTy.U32 rest).val = rest.val :=
        UScalar.cast_val_mod_pow_of_inBounds_eq _ _ (by simp; scalar_tac)
      simp only [Std.bind_tc_ok, Std.bind_ok]
      split
      · simp only [WP.spec_ok]; scalar_tac
      · simp only [WP.spec_ok]; scalar_tac
    · rw [if_neg h3]
      simp only [Std.bind_tc_ok, Std.bind_ok]
      split
      · simp only [WP.spec_ok]; scalar_tac
      · simp only [WP.spec_ok]; scalar_tac
  by_cases h0 : lim.max_head < cap
  · rw [if_pos h0]; simp only [Std.bind_tc_ok, Std.bind_ok]; exact key _ (by scalar_tac) (le_refl _)
  · rw [if_neg h0]; simp only [Std.bind_tc_ok, Std.bind_ok]; exact key _ (le_refl _) (by scalar_tac)

/-! ## Closing -/

/-- `must_close` closes after an HTTP/1.0 request and after the
`max_requests`-th. (Whether a `Connection` field lists `close` is tested,
not proven.) -/
theorem must_close_spec (buf : Slice U8) (head : Head) (served max : U32)
    (h : head.minor.val = 0 ∨ max.val ≤ served.val) : must_close buf head served max = ok true := by
  unfold must_close
  rcases h with h | h
  · rw [if_pos (by scalar_tac)]
  · by_cases h0 : head.minor = 0#u8
    · rw [if_pos h0]
    · rw [if_neg h0, if_pos (by scalar_tac)]

end h5i_app_h1.Recv
