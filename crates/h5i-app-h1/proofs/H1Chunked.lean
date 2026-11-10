import H1Recv
import H1Valid
/-!
# Chunked bodies against the independent grammar

`H1Valid.ValidChunked` says what a chunked body h5i accepts is, with its own
hex digits and values. Here: every body `Spec.dechunk` accepts is one, with
the payload it returns (`dechunk_valid`), and so is every chunked body
`read_body` and `recv_step` accept, read from the bytes received
(`read_body_chunked_valid`, `recv_step_chunked_valid`). The hex digits and
their values meet `Spec.hexv` only in the byte bridges below, checked over all
256 bytes.
-/

open Aeneas Aeneas.Std Result h5i_app_h1 h5i_app_h1.Spec

namespace h5i_app_h1.Chunked

/-! ## Byte bridges -/

theorem hex_byte : ∀ b, b < 256 → isHex b = H1Valid.isHexDig b := by decide +kernel
theorem hexv_byte : ∀ b, b < 256 → H1Valid.isHexDig b = true → hexv b = H1Valid.hexDigitValue b := by
  decide +kernel
theorem zero_byte : ∀ b, b < 256 → H1Valid.isHexDig b = true → H1Valid.hexDigitValue b = 0 → b = 48 := by
  decide +kernel

/-- Above 255 neither side has a hex digit, so the bridges hold for every
number. -/
theorem big_not_hex {b : Nat} (h : 256 ≤ b) : isHex b = false ∧ H1Valid.isHexDig b = false := by
  have ha : H1Valid.ascii "ABCDEFabcdef" = [65, 66, 67, 68, 69, 70, 97, 98, 99, 100, 101, 102] := by decide
  refine ⟨?_, ?_⟩
  · simp only [isHex, hexv, decide_eq_false_iff_not]; split_ifs <;> omega
  · simp only [H1Valid.isHexDig, H1Valid.isDigit, ha, Bool.or_eq_false_iff, Bool.and_eq_false_iff,
      decide_eq_false_iff_not, List.contains_eq_any_beq, List.any_cons, List.any_nil, Bool.or_false,
      beq_eq_false_iff_ne]
    omega

theorem hex_eq (b : Nat) : isHex b = H1Valid.isHexDig b := by
  by_cases h : b < 256
  · exact hex_byte b h
  · obtain ⟨h1, h2⟩ := big_not_hex (by omega : 256 ≤ b); rw [h1, h2]

theorem hexv_val (b : Nat) (h : H1Valid.isHexDig b = true) : hexv b = H1Valid.hexDigitValue b := by
  by_cases hb : b < 256
  · exact hexv_byte b hb h
  · rw [(big_not_hex (by omega : 256 ≤ b)).2] at h; cases h

theorem zero_digit (b : Nat) (h : H1Valid.isHexDig b = true) (h0 : H1Valid.hexDigitValue b = 0) : b = 48 := by
  by_cases hb : b < 256
  · exact zero_byte b hb h h0
  · rw [(big_not_hex (by omega : 256 ≤ b)).2] at h; cases h

/-! ## Numerals -/

theorem hexVal_number {ds : List Nat} (h : ∀ d ∈ ds, isHex d = true) :
    ∀ s, hexVal s ds = ds.foldl (fun a d => 16 * a + H1Valid.hexDigitValue d) s := by
  induction ds with
  | nil => intro s; rfl
  | cons d ds ih =>
    intro s
    have hd := h d (by simp)
    rw [hexVal_cons, List.foldl_cons, hexv_val d (by rw [← hex_eq d]; exact hd), Nat.mul_comm]
    exact ih (fun x hx => h x (by simp [hx])) _

theorem number_zero {ds : List Nat} :
    ∀ s, ds.foldl (fun a d => 16 * a + H1Valid.hexDigitValue d) s = 0 →
      s = 0 ∧ ∀ d ∈ ds, H1Valid.hexDigitValue d = 0 := by
  induction ds with
  | nil => intro s h; simpa using h
  | cons d ds ih =>
    intro s h
    rw [List.foldl_cons] at h
    obtain ⟨h1, h2⟩ := ih _ h
    refine ⟨by omega, fun x hx => ?_⟩
    simp only [List.mem_cons] at hx
    rcases hx with rfl | hx
    · omega
    · exact h2 x hx

/-! ## `Spec.dechunk` -/

theorem takeWhile_all' (p : Nat → Bool) (L : List Nat) : ∀ c ∈ L.takeWhile p, c ∈ L ∧ p c = true := by
  intro c hc
  exact ⟨(List.takeWhile_sublist _).subset hc, takeWhile_all p L c hc⟩

theorem size_ok {L : List Nat} (hne : L.takeWhile isHex ≠ [])
    (h8 : ¬ 8 < (L.takeWhile isHex).length) :
    H1Valid.chunkSize (L.takeWhile isHex) ∧ hexVal 0 (L.takeWhile isHex) = H1Valid.hexNumber (L.takeWhile isHex) := by
  have hall := takeWhile_all' isHex L
  refine ⟨⟨hne, by omega, ?_⟩, hexVal_number (fun d hd => (hall d hd).2) 0⟩
  rw [List.all_eq_true]
  intro d hd
  rw [← hex_eq d]; exact (hall d hd).2

/-- What `dechunkFrom` accepts, from a total of `t`, is a valid chunked body
with at most `mb - t` payload bytes. -/
theorem dechunkFrom_valid (L : List Nat) (mb t : Nat) (p : List Nat) (k : Nat)
    (h : dechunkFrom L mb t = .done p k) :
    k ≤ L.length ∧ t ≤ mb ∧ H1Valid.ValidChunked (mb - t) (L.take k) p := by
  induction hL : L.length using Nat.strongRecOn generalizing L t p k with
  | _ len ih =>
  subst hL
  have hsplit := (List.takeWhile_append_dropWhile (p := isHex) (l := L)).symm
  rw [dechunkFrom] at h
  try simp only at h
  split at h
  · cases h
  rename_i h8
  split at h
  · cases h
  rename_i c L2 hd
  split at h
  · cases h
  rename_i hok
  simp only [not_or, Decidable.not_not] at hok
  obtain ⟨hcr, hne⟩ := hok
  subst hcr
  split at h
  · cases h
  rename_i hfit
  split at h
  · cases h
  rename_i c2 L3 _
  split at h
  · cases h
  rename_i hlf
  simp only [Decidable.not_not] at hlf
  subst hlf
  obtain ⟨hsz, hnum⟩ := size_ok hne h8
  rw [hd] at hsplit
  split at h
  · -- the last chunk
    rename_i h0
    split at h
    · cases h
    rename_i c3 L4 _
    split at h
    · cases h
    rename_i h3
    simp only [Decidable.not_not] at h3
    subst h3
    split at h
    · cases h
    rename_i c4 L5 _
    split at h
    · cases h
    rename_i h4
    simp only [Decidable.not_not] at h4
    subst h4
    simp only [DR.done.injEq] at h
    obtain ⟨rfl, rfl⟩ := h
    set ds := L.takeWhile isHex
    have hz : ds.all (· == 48) = true := by
      rw [hnum] at h0
      obtain ⟨-, hz⟩ := number_zero _ h0
      have hall := takeWhile_all' isHex L
      rw [List.all_eq_true]
      intro d hd'
      have := zero_digit d (by rw [← hex_eq d]; exact (hall d hd').2) (hz d hd')
      simp [this]
    refine ⟨?_, by omega, [], ds, ?_, by simp, hsz, hz, rfl, by simp⟩
    · rw [hsplit]; simp
    · rw [hsplit]
      simp [H1Valid.chunkedBytes, H1Valid.crlf, List.take_append]
  · rename_i hnz
    split at h
    · cases h
    rename_i hlong
    split at h
    · cases h
    rename_i c3 L4 hdr
    split at h
    · cases h
    rename_i h3
    simp only [Decidable.not_not] at h3
    subst h3
    split at h
    · cases h
    rename_i c4 L5 _
    split at h
    · cases h
    rename_i h4
    simp only [Decidable.not_not] at h4
    subst h4
    set ds := L.takeWhile isHex
    set n := hexVal 0 ds
    rcases hr : dechunkFrom L5 mb (t + n) with _ | x | ⟨p', k'⟩ <;> rw [hr] at h <;> simp only [DR.shift] at h
    · cases h
    · cases h
    simp only [DR.done.injEq] at h
    obtain ⟨rfl, rfl⟩ := h
    have hL3 : L3 = L3.take n ++ 13 :: 10 :: L5 := by rw [← hdr, List.take_append_drop]
    have hlen5 : L5.length < L.length := by
      have a := congrArg List.length hsplit
      have b := congrArg List.length hL3
      simp only [List.length_append, List.length_cons, List.length_take] at a b
      omega
    obtain ⟨hk', ht', cs, z, hW, hcs, hz, hz0, hP, hPl⟩ :=
      ih L5.length hlen5 L5 (t + n) p' k' hr rfl
    have hDl : (L3.take n).length = n := by simp; omega
    refine ⟨?_, by omega, ⟨ds, L3.take n⟩ :: cs, z, ?_, ?_, hz, hz0, ?_, ?_⟩
    · rw [hsplit, hL3]; simp; omega
    · have hD : (L3.take n ++ 13 :: 10 :: L5).take n = L3.take n := by
        rw [List.take_append_of_le_length (by omega), List.take_of_length_le (by omega)]
      rw [hsplit, hL3, hD]
      have hpre : ds ++ 13 :: 10 :: (L3.take n ++ 13 :: 10 :: L5) =
          (ds ++ [13, 10] ++ L3.take n ++ [13, 10]) ++ L5 := by simp
      have hlen : ds.length + 4 + n + k' = (ds ++ [13, 10] ++ L3.take n ++ [13, 10]).length + k' := by
        simp [hDl]; omega
      rw [hpre, hlen, List.take_length_add_append, hW]
      simp [H1Valid.chunkedBytes, H1Valid.Chunk.bytes, H1Valid.crlf]
    · intro c hc
      simp only [List.mem_cons] at hc
      rcases hc with rfl | hc
      · refine ⟨hsz, ?_, ?_⟩
        · show L3.take n ≠ []
          intro he; rw [he] at hDl; simp at hDl; omega
        · show (L3.take n).length = H1Valid.hexNumber ds
          rw [hDl, hnum]
      · exact hcs c hc
    · simp [hP]
    · simp; omega

/-- What `Spec.dechunk` accepts is a valid chunked body: the bytes it used
form one, and the payload is that body's. So `Spec.dechunk`, and through
`feed_dechunk` the decoder `chunk_step`, reads sizes by `H1Valid`'s hex
values and checks the total against `mb`. -/
theorem dechunk_valid {L : List Nat} {mb : Nat} {p : List Nat} {k : Nat} (h : Spec.dechunk L mb = .done p k) :
    k ≤ L.length ∧ H1Valid.ValidChunked mb (L.take k) p := by
  obtain ⟨h1, -, h3⟩ := dechunkFrom_valid L mb 0 p k h
  exact ⟨h1, by simpa using h3⟩

/-! ## The extracted code -/

open Proofs Recv

/-- `read_body` on a chunked body: when it is done, the wire bytes it used,
`wire[..used - start]`, are a valid chunked body whose payload is
`buf'[start..end]`. -/
theorem read_body_chunked_valid (buf : Slice U8) (filled start : Usize) (st : BodyState) (mb : U32) (wire : List Nat)
    (hI : ChunkInv mb.val start.val filled.val wire (B buf.val) (csView st.chunk) st.r.val st.w.val) :
    read_body buf filled start .Chunked st mb ⦃ r buf' => ∀ e u, r.2 = .Done e u →
      u.val - start.val ≤ wire.length ∧
      H1Valid.ValidChunked mb.val (wire.take (u.val - start.val)) (payload buf' start.val e.val) ⦄ := by
  apply WP.spec_mono (read_body_chunked buf filled start st mb wire hI)
  rintro ⟨⟨st', out⟩, buf'⟩ ⟨-, h⟩ e u he
  simp only at he
  subst he
  exact dechunk_valid h

theorem undecidedS_ne_ready {len cap : Nat} {eof between : Bool} {full : Nat} {hd : Spec.Head}
    {s e u : Nat} {p : List Nat} : undecidedS len cap eof between full ≠ .ready hd s e u p := by
  unfold undecidedS; split_ifs <;> simp

/-- A `ready` decision on a chunked request: the payload and the bytes used
are `Spec.dechunk`'s on the bytes after the head. -/
theorem recvS_chunked {L : List Nat} {cap mh mb : Nat} {eof : Bool} {hd : Spec.Head} {s e u : Nat}
    {p : List Nat} (h : recvS L cap mh mb eof = .ready hd s e u p) (hc : hd.framing = .chunked) :
    s = hd.len ∧ s ≤ u ∧ Spec.dechunk (L.drop s) mb = .done p (u - s) := by
  unfold recvS at h
  split at h
  · cases h
  · exact absurd h undecidedS_ne_ready
  rename_i h0 _
  split at h
  · rename_i a hb
    subst h
    unfold bodyS at hb
    split at hb
    · simp at hb; obtain rfl := hb.1; simp_all
    · split_ifs at hb <;> simp at hb; obtain rfl := hb.1; simp_all
    · rename_i hf
      split at hb
      · rename_i p' c hdc
        simp only [Option.some.injEq, RA.ready.injEq] at hb
        obtain ⟨rfl, rfl, -, rfl, rfl⟩ := hb
        refine ⟨rfl, by omega, ?_⟩
        rw [hdc]; simp
      · simp at hb
      · simp at hb
  · exact absurd h undecidedS_ne_ready

/-- `recv_step`: a chunked request it returns as `Ready(head, start, end,
used)` has, among the bytes received, `[start, used)` a valid chunked body of
`used - start` bytes, and its payload is `rx'[start..end]`. -/
theorem recv_step_chunked_valid (lim : Limits) (st : RecvState) (buf : Slice U8) (filled : Usize) (eof : Bool)
    (L : List Nat) (hI : RInv lim st buf filled.val L) :
    recv_step st buf filled eof lim ⦃ r buf' => ∀ h s e u, r.2 = .Ready h s e u → h.framing = .Chunked →
      s.val ≤ u.val ∧ u.val ≤ filled.val ∧ ((L.drop s.val).take (u.val - s.val)).length = u.val - s.val ∧
      H1Valid.ValidChunked lim.max_body.val ((L.drop s.val).take (u.val - s.val)) (payload buf' s.val e.val) ⦄ := by
  have hfl := hI.1
  apply WP.spec_mono (recv_step_spec lim st buf filled eof L hI)
  rintro ⟨⟨st', a⟩, buf'⟩ ⟨-, hm, -, hr⟩ h s e u ha hc
  simp only at ha hm hr
  obtain ⟨-, -, hu, -⟩ := hr h s e u ha
  subst ha
  generalize hR : recvS L buf.length lim.max_head.val lim.max_body.val eof = R at hm
  cases R with
  | ready hd s0 e0 u0 p =>
    obtain ⟨h', s', e', u', he, hv, rfl, rfl, rfl, rfl⟩ := hm
    simp only [Action.Ready.injEq] at he
    obtain ⟨rfl, rfl, rfl, rfl⟩ := he
    have hcf : hd.framing = .chunked := by rw [← hv]; simp [headView, framingView, hc]
    obtain ⟨-, hsu, hd'⟩ := recvS_chunked hR hcf
    obtain ⟨hk, hv⟩ := dechunk_valid hd'
    refine ⟨hsu, hu, ?_, hv⟩
    simp; omega
  | needRead => simp [Matches] at hm
  | reject x => obtain ⟨_, hm, -⟩ := hm; cases hm
  | eof => simp [Matches] at hm

end h5i_app_h1.Chunked
