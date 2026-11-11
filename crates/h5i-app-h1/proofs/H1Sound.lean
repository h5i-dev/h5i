import H1Props
import H1Valid
/-!
# Every accepted head is valid, by the independent definition

`parse_head_sound`: whenever the extracted `parse_head` returns `Done h`, the
head's bytes are an `H1Valid.ValidHead` (`H1Valid.lean`, which shares no
definition with `H1Spec`), `h.framing` is `H1Valid.framing` of its field
lines, and the method, target and field spans hold exactly the request
line's and the field lines' parts.

The proof goes through `Spec.parseHead` (`parse_head_spec`) and then relates
each `H1Spec` helper to the independent definitions byte by byte: those
bridges are where a misreading shared by the Rust and `H1Spec` (a wrong
literal, case-sensitive names, a wrong byte class) would fail.
-/
open h5i_app_h1.Spec

namespace H1Sound

/-! ## Bytes as lists -/

/-- The bytes `[a, b)` of `g`. -/
def seg (g : Nat → Nat) (a b : Nat) : List Nat := (List.range' a (b - a)).map g

theorem seg_nil {g : Nat → Nat} {a b : Nat} (h : b ≤ a) : seg g a b = [] := by
  simp [seg, show b - a = 0 by omega]

theorem seg_cons {g : Nat → Nat} {a b : Nat} (h : a < b) : seg g a b = g a :: seg g (a + 1) b := by
  unfold seg
  rw [show b - a = (b - (a + 1)) + 1 by omega, List.range'_succ]
  simp

theorem seg_append (g : Nat → Nat) {a b c : Nat} (h1 : a ≤ b) (h2 : b ≤ c) :
    seg g a c = seg g a b ++ seg g b c := by
  induction hk : b - a generalizing a with
  | zero => rw [show b = a by omega, seg_nil (le_refl a)]; rfl
  | succ k ih =>
    rw [seg_cons (show a < c by omega), seg_cons (show a < b by omega), ih (by omega) (by omega)]
    rfl

theorem mem_seg {g : Nat → Nat} {a b x : Nat} : x ∈ seg g a b ↔ ∃ k, a ≤ k ∧ k < b ∧ g k = x := by
  unfold seg
  simp only [List.mem_map, List.mem_range'_1]
  constructor
  · rintro ⟨k, ⟨h1, h2⟩, h3⟩; exact ⟨k, h1, by omega, h3⟩
  · rintro ⟨k, h1, h2, h3⟩; exact ⟨k, ⟨h1, by omega⟩, h3⟩

theorem seg_length (g : Nat → Nat) (a b : Nat) : (seg g a b).length = b - a := by simp [seg]

theorem seg_single (g : Nat → Nat) (a : Nat) : seg g a (a + 1) = [g a] := by
  rw [seg_cons (by omega), seg_nil (le_refl _)]

theorem seg_window (L : List Nat) {n a b : Nat} (hb : b ≤ n) (hn : n ≤ L.length) :
    seg (window L n) a b = (L.drop a).take (b - a) := by
  induction hk : b - a generalizing a with
  | zero => rw [seg_nil (by omega)]; simp
  | succ k ih =>
    have ha : a < L.length := by omega
    rw [seg_cons (by omega), ih (by omega), H1Props.window_below L n a (by omega),
      List.drop_eq_getElem_cons ha, List.take_succ_cons]
    simp [List.getD_eq_getElem?_getD, ha]

theorem all_seg {g : Nat → Nat} {a b : Nat} {p : Nat → Bool} (h : ∀ k, a ≤ k → k < b → p (g k) = true) :
    (seg g a b).all p = true := by
  rw [List.all_eq_true]
  intro x hx
  obtain ⟨k, h1, h2, rfl⟩ := mem_seg.1 hx
  exact h k h1 h2

theorem all_seg_iff {g : Nat → Nat} {a b : Nat} {p : Nat → Bool} :
    (seg g a b).all p = true ↔ ∀ k, a ≤ k → k < b → p (g k) = true := by
  constructor
  · intro h k h1 h2
    exact List.all_eq_true.1 h (g k) (mem_seg.2 ⟨k, h1, h2, rfl⟩)
  · exact all_seg

theorem all_congr' {l : List Nat} {p q : Nat → Bool} (hb : ∀ x ∈ l, x < 256) (h : ∀ b, b < 256 → p b = q b) :
    l.all p = l.all q := by
  induction l with
  | nil => rfl
  | cons x l ih =>
    simp only [List.all_cons]
    rw [h x (hb x (by simp)), ih (fun y hy => hb y (by simp [hy]))]

theorem map_congr' {l : List Nat} {f f' : Nat → Nat} (hb : ∀ x ∈ l, x < 256) (h : ∀ b, b < 256 → f b = f' b) :
    l.map f = l.map f' := List.map_congr_left (fun x hx => h x (hb x hx))

/-! ## Byte classes, case and literals: the parser's and the independent ones -/

theorem lower_lc : ∀ b, b < 256 → lower b = H1Valid.lc b := by decide +kernel
theorem tchar_eq : ∀ b, b < 256 → isTchar b = H1Valid.tchar b := by decide +kernel
theorem ows_eq : ∀ b, b < 256 → isOws b = H1Valid.isWs b := by decide +kernel
theorem hexv_eq : ∀ b, b < 256 → decide (hexv b < 16) = H1Valid.isHexDig b := by decide +kernel
theorem reg_eq : ∀ b, b < 256 → byteIn b 0 = (H1Valid.unreserved b || H1Valid.subDelims b) := by
  decide +kernel
theorem target_eq : ∀ b, b < 256 → byteIn b 1 = (H1Valid.pcharByte b || b == 47 || b == 63) := by
  decide +kernel
theorem literal_eq : ∀ b, b < 256 → byteIn b 2 = (H1Valid.unreserved b || H1Valid.subDelims b || b == 58) := by
  decide +kernel
theorem digit_eq : ∀ b, b < 256 → byteIn b 3 = H1Valid.isDigit b := by decide +kernel
theorem field_vchar : ∀ b, b < 256 → isFieldByte b = true → isOws b = false → H1Valid.fieldVchar b = true := by
  decide +kernel
theorem field_byte : ∀ b, b < 256 → isFieldByte b = true → (H1Valid.fieldVchar b || H1Valid.isWs b) = true := by
  decide +kernel
theorem cl_digit : ∀ b, b < 256 → ¬ (b < 48 ∨ 57 < b) → H1Valid.isDigit b = true := by decide +kernel
theorem byteIn_37 (c : Nat) : byteIn 37 c = false := by
  unfold byteIn; split_ifs <;> simp_all [isRegByte]

theorem lit_host : litHost = H1Valid.ascii "host" := by decide
theorem lit_cl : litContentLength = H1Valid.ascii "content-length" := by decide
theorem lit_te : litTransferEncoding = H1Valid.ascii "transfer-encoding" := by decide
theorem lit_chunked : litChunked = H1Valid.ascii "chunked" := by decide
theorem lit_http : litHttp ++ [49, 46] = H1Valid.ascii "HTTP/1." := by decide

/-! ## Names -/

section
variable {g : Nat → Nat} (hg : ∀ k, g k < 256)
include hg

theorem seg_lt (a b : Nat) : ∀ x ∈ seg g a b, x < 256 := by
  intro x hx; obtain ⟨k, -, -, rfl⟩ := mem_seg.1 hx; exact hg k

omit hg in
theorem eqFrom_iff (s : Nat) (lit : List Nat) :
    ∀ q, q ≤ lit.length → (eqFrom g s lit q = true ↔ (seg g (s + q) (s + lit.length)).map lower = lit.drop q) := by
  intro q hq
  induction hk : lit.length - q generalizing q with
  | zero =>
    rw [eqFrom, if_neg (by omega), seg_nil (by omega), List.drop_of_length_le (by omega)]; simp
  | succ k ih =>
    rw [eqFrom, if_pos (by omega), seg_cons (by omega), List.drop_eq_getElem_cons (by omega)]
    simp only [List.map_cons, List.cons.injEq]
    rw [show s + q + 1 = s + (q + 1) by omega, ← ih (q + 1) (by omega) (by omega)]
    rw [List.getD_eq_getElem?_getD, List.getElem?_eq_getElem (by omega), Option.getD_some]
    by_cases h : lower (g (s + q)) = lit[q]
    · rw [if_neg (by simpa using h)]; simp [h]
    · rw [if_pos (by simpa using h)]; simp [h]

/-- `eqCi` against a lowercase literal is `named`. -/
theorem eqCi_named {s e : Nat} {lit : List Nat} {str : String} (hl : lit = H1Valid.ascii str)
    (hne : lit ≠ []) : eqCi g s e lit = H1Valid.named (seg g s e) str := by
  unfold eqCi H1Valid.named
  rw [← hl]
  split
  · rename_i h
    symm
    rw [beq_eq_false_iff_ne]
    intro he
    have := congrArg List.length he
    simp only [List.length_map, seg_length] at this
    rcases h with h | h
    · have : lit.length = 0 := by omega
      exact hne (List.eq_nil_of_length_eq_zero this)
    · omega
  · rename_i h
    have he : e = s + lit.length := by omega
    have := eqFrom_iff (g := g) s lit 0 (Nat.zero_le _)
    simp only [Nat.add_zero, List.drop_zero] at this
    rw [← he, map_congr' (seg_lt hg s e) lower_lc] at this
    by_cases hb : eqFrom g s lit 0 = true
    · rw [hb]; symm; exact beq_iff_eq.2 (this.1 hb)
    · rw [Bool.not_eq_true] at hb
      rw [hb]; symm; rw [beq_eq_false_iff_ne]; intro h'; rw [this.2 h'] at hb; simp at hb

omit hg in
theorem named_length {l : List Nat} {str : String} (h : H1Valid.named l str = true) :
    l.length = (H1Valid.ascii str).length := by
  unfold H1Valid.named at h
  have := congrArg List.length (beq_iff_eq.1 h)
  simpa using this

theorem classify_iff (s e : Nat) :
    (classify g s e = 1 ↔ H1Valid.named (seg g s e) "host" = true) ∧
    (classify g s e = 2 ↔ H1Valid.named (seg g s e) "content-length" = true) ∧
    (classify g s e = 3 ↔ H1Valid.named (seg g s e) "transfer-encoding" = true) := by
  have e1 := eqCi_named hg (s := s) (e := e) lit_host (by decide)
  have e2 := eqCi_named hg (s := s) (e := e) lit_cl (by decide)
  have e3 := eqCi_named hg (s := s) (e := e) lit_te (by decide)
  have x12 : ¬ (H1Valid.named (seg g s e) "host" = true ∧ H1Valid.named (seg g s e) "content-length" = true) :=
    fun ⟨a, b⟩ => by have := named_length a; have := named_length b; simp_all [H1Valid.ascii]
  have x13 : ¬ (H1Valid.named (seg g s e) "host" = true ∧ H1Valid.named (seg g s e) "transfer-encoding" = true) :=
    fun ⟨a, b⟩ => by have := named_length a; have := named_length b; simp_all [H1Valid.ascii]
  have x23 : ¬ (H1Valid.named (seg g s e) "content-length" = true ∧
      H1Valid.named (seg g s e) "transfer-encoding" = true) :=
    fun ⟨a, b⟩ => by have := named_length a; have := named_length b; simp_all [H1Valid.ascii]
  unfold classify
  rw [e1, e2, e3]
  refine ⟨?_, ?_, ?_⟩ <;> split_ifs <;> simp_all

/-! ## Scans -/

omit hg in
theorem scan_seg (p : Nat → Bool) (i e : Nat) : ∀ k, i ≤ k → k < scan p g i e → p (g k) = true :=
  fun k h1 h2 => H1Props.scan_all p g i e k h1 h2

omit hg in
theorem scan_stop' (p : Nat → Bool) (i e : Nat) (h : scan p g i e < e) : p (g (scan p g i e)) = false := by
  rw [scan] at h ⊢
  split at h
  · split at h
    · rename_i h1 h2
      rw [if_pos h1, if_pos h2]
      exact scan_stop' p (i + 1) e h
    · rename_i h1 h2
      rw [if_pos h1, if_neg h2]; simpa using h2
  · omega
termination_by e - i

omit hg in
theorem trimEnd_ows (s p : Nat) : ∀ k, trimEnd g s p ≤ k → k < p → isOws (g k) = true := by
  intro k h1 h2
  rw [trimEnd] at h1
  split at h1
  · split at h1
    · rename_i _ ho
      by_cases hk : k = p - 1
      · subst hk; exact ho
      · exact trimEnd_ows s (p - 1) k h1 (by omega)
    · omega
  · omega
termination_by p

omit hg in
theorem trimEnd_last (s p : Nat) (h : s < trimEnd g s p) : isOws (g (trimEnd g s p - 1)) = false := by
  rw [trimEnd] at h ⊢
  split at h
  · split at h
    · rename_i h1 h2
      rw [if_pos h1, if_pos h2]
      exact trimEnd_last s (p - 1) h
    · rename_i h1 h2
      rw [if_pos h1, if_neg h2]; simpa using h2
  · omega
termination_by p

/-! ## `%`-encoded runs -/

theorem pctRun_spanEnd (c : Nat) (hc : c ≤ 1) (e : Nat) :
    ∀ i, H1Valid.pctRun (fun b => byteIn b c) (seg g i (spanEnd c g i e)) = true := by
  intro i
  induction hk : e - i using Nat.strong_induction_on generalizing i with
  | _ m ih =>
    rw [spanEnd]
    split
    · rename_i hi
      split
      · rename_i h37
        split
        · rename_i hh
          have hge := spanEnd_ge c g (i + 3) e
          rw [seg_cons (by omega), seg_cons (by omega), seg_cons (by omega), h37.1, H1Valid.pctRun_pct]
          have a1 := hexv_eq (g (i + 1)) (hg _)
          have a2 := hexv_eq (g (i + 2)) (hg _)
          rw [decide_eq_true hh.2.1] at a1
          rw [decide_eq_true hh.2.2] at a2
          rw [← a1, ← a2]
          simpa using ih (e - (i + 3)) (by omega) (i + 3) rfl
        · rw [seg_nil (le_refl _)]; simp
      · split
        · rename_i hn hb
          have hge := spanEnd_ge c g (i + 1) e
          have h37 : g i ≠ 37 := by intro h; rw [h, byteIn_37] at hb; simp at hb
          rw [seg_cons (by omega), H1Valid.pctRun_cons_ne h37, hb]
          simpa using ih (e - (i + 1)) (by omega) (i + 1) rfl
        · rw [seg_nil (le_refl _)]; simp
    · rw [seg_nil (le_refl _)]; simp

theorem all_spanEnd (c : Nat) (hc : 2 ≤ c) (e : Nat) :
    ∀ i k, i ≤ k → k < spanEnd c g i e → byteIn (g k) c = true := by
  intro i
  induction hk : e - i using Nat.strong_induction_on generalizing i with
  | _ m ih =>
    intro k h1 h2
    rw [spanEnd] at h2
    split at h2
    · split at h2
      · rename_i h37; omega
      · split at h2
        · rename_i hb
          by_cases hki : k = i
          · subst hki; exact hb
          · exact ih (e - (i + 1)) (by omega) (i + 1) rfl k (by omega) h2
        · omega
    · omega

omit hg in
theorem pctRun_congr {p q : Nat → Bool} (h : ∀ b, b < 256 → p b = q b) :
    ∀ l, (∀ x ∈ l, x < 256) → H1Valid.pctRun p l = H1Valid.pctRun q l
  | [] => fun _ => by simp
  | c :: r => by
    intro hb
    rw [H1Valid.pctRun, H1Valid.pctRun]
    split
    · rw [pctRun_congr h (r.drop 2) (fun x hx => hb x (by simp [List.mem_of_mem_drop hx]))]
    · rw [h c (hb c (by simp)), pctRun_congr h r (fun x hx => hb x (by simp [hx]))]
termination_by l => l.length

/-! ## `Content-Length` -/

theorem clLoop_dec (e max acc p v : Nat) (h : clLoop g e max acc p = .ok v) :
    (seg g p e).all H1Valid.isDigit = true ∧ (seg g p e).foldl (fun a d => 10 * a + (d - 48)) acc = v := by
  rw [clLoop] at h
  split at h
  · rename_i hp
    split_ifs at h with h1 h2
    obtain ⟨ha, hf⟩ := clLoop_dec e max _ (p + 1) v h
    rw [seg_cons hp]
    simp only [List.all_cons, List.foldl_cons, Bool.and_eq_true]
    refine ⟨⟨cl_digit _ (hg p) h1, ha⟩, ?_⟩
    rw [Nat.mul_comm]; exact hf
  · simp only [Num.ok.injEq] at h
    rw [seg_nil (by omega)]; simp [h]
termination_by e - p

theorem contentLength_dec {s e max v : Nat} (h : contentLength g s e max = .ok v) :
    seg g s e ≠ [] ∧ (seg g s e).all H1Valid.isDigit = true ∧ H1Valid.decimal (seg g s e) = v ∧ v ≤ max := by
  have hle := H1Props.contentLength_le h
  unfold contentLength at h
  split_ifs at h with h1
  obtain ⟨ha, hf⟩ := clLoop_dec hg e max 0 s v h
  refine ⟨?_, ha, hf, hle⟩
  rw [seg_cons (by omega)]; simp

/-! ## `Host` values -/

theorem portOk_port {p e : Nat} (h : portOk g p e = true) :
    seg g p e = [] ∨ ∃ ds, seg g p e = 58 :: ds ∧ ds.all H1Valid.isDigit = true := by
  unfold portOk at h
  split at h
  · left; exact seg_nil (by omega)
  · split at h
    · simp at h
    · rename_i h1 h2
      have h3 : spanEnd 3 g (p + 1) e = e := by simpa using h
      right
      refine ⟨seg g (p + 1) e, by rw [seg_cons (by omega)]; simp at h2; rw [h2], ?_⟩
      apply all_seg
      intro k hk1 hk2
      rw [← digit_eq _ (hg k)]
      exact all_spanEnd hg 3 (by omega) e (p + 1) k hk1 (by omega)

theorem hostOk_value {s e : Nat} (hse : s ≤ e) (h : hostOk g s e = true) : H1Valid.hostValue (seg g s e) := by
  unfold hostOk at h
  split at h
  · rename_i hb
    dsimp only at h
    split at h
    · simp at h
    · rename_i hk
      have hk1 := spanEnd_ge 2 g (s + 1) e
      have hk2 := spanEnd_le 2 g (s + 1) e (by omega)
      generalize hK : spanEnd 2 g (s + 1) e = k at *
      have hk93 : g k = 93 := by simp at hk; omega
      refine ⟨seg g s (k + 1), seg g (k + 1) e, by rw [← seg_append g (by omega) (by omega)], Or.inr ?_,
        portOk_port hg h⟩
      refine ⟨seg g (s + 1) k, ?_, ?_, ?_⟩
      · rw [seg_cons (by omega), hb.2, seg_append g (show s + 1 ≤ k by omega) (show k ≤ k + 1 by omega),
          seg_single, hk93]
      · intro hn; have := congrArg List.length hn; simp [seg_length] at this; omega
      · apply all_seg
        intro m hm1 hm2
        rw [← literal_eq _ (hg m)]
        exact all_spanEnd hg 2 (le_refl _) e (s + 1) m hm1 (by rw [hK]; omega)
  · have hk1 := spanEnd_ge 0 g s e
    have hk2 := spanEnd_le 0 g s e hse
    refine ⟨seg g s (spanEnd 0 g s e), seg g (spanEnd 0 g s e) e, by rw [← seg_append g hk1 hk2],
      Or.inl ?_, portOk_port hg h⟩
    unfold H1Valid.regName
    rw [← pctRun_congr reg_eq _ (seg_lt hg _ _)]
    exact pctRun_spanEnd hg 0 (by omega) e s

/-! ## The request line -/

omit hg in
theorem eqAt_get {s : Nat} {lit : List Nat} :
    ∀ q, eqAt g s lit q = true → ∀ r, q ≤ r → (hr : r < lit.length) → g (s + r) = lit[r] := by
  intro q h r hq hr
  induction hk : lit.length - q generalizing q with
  | zero => omega
  | succ k ih =>
    rw [eqAt, if_pos (by omega)] at h
    split at h
    · simp at h
    · rename_i hne
      by_cases hqr : q = r
      · subst hqr
        simp only [ne_eq, Decidable.not_not] at hne
        rw [hne, List.getD_eq_getElem?_getD, List.getElem?_eq_getElem hr, Option.getD_some]
      · exact ih (q + 1) h (by omega) (by omega)

omit hg in
theorem getElem_seg {a b i : Nat} (hi : i < (seg g a b).length) : (seg g a b)[i] = g (a + i) := by
  simp [seg]

theorem requestLine_valid {j ms me ts te minor : Nat} (h : requestLine g 0 j = .ok ms me ts te minor) :
    ms = 0 ∧ ts = me + 1 ∧ ts ≤ te ∧ te + 9 = j ∧
    seg g 0 j = H1Valid.RequestLine.bytes ⟨seg g 0 me, seg g ts te, minor⟩ ∧
    seg g 0 me ≠ [] ∧ (seg g 0 me).all H1Valid.tchar = true ∧ H1Valid.originForm (seg g ts te) ∧
    (minor = 0 ∨ minor = 1) := by
  have hmin := H1Props.requestLine_minor h
  unfold requestLine at h
  dsimp only at h
  split_ifs at h with h1 h2 h3 h4 h5 h6
  simp only [ReqLine.ok.injEq] at h
  obtain ⟨rfl, rfl, rfl, rfl, rfl⟩ := h
  generalize hm : scan isTchar g 0 j = m at *
  generalize hT : spanEnd 1 g (m + 1) j = te at *
  generalize hv : version g (te + 1) j = v at *
  have hte1 : m + 1 ≤ te := by rw [← hT]; exact spanEnd_ge 1 g (m + 1) j
  simp only [not_or, ne_eq, Decidable.not_not] at h1 h2 h3 h4
  -- the version
  unfold version at hv
  split_ifs at hv with hv1 hv2
  · omega
  · omega
  have hj : j = te + 1 + 8 := by omega
  have hhttp : eqAt g (te + 1) litHttp 0 = true := by simpa using hv2
  have hg0 := eqAt_get (g := g) 0 hhttp 0 (by omega) (by decide)
  have hg1 := eqAt_get (g := g) 0 hhttp 1 (by omega) (by decide)
  have hg2 := eqAt_get (g := g) 0 hhttp 2 (by omega) (by decide)
  have hg3 := eqAt_get (g := g) 0 hhttp 3 (by omega) (by decide)
  have hg4 := eqAt_get (g := g) 0 hhttp 4 (by omega) (by decide)
  have hd : g (te + 1 + 6) = 46 ∧ g (te + 1 + 5) = 49 ∧ g (te + 1 + 7) = 48 + v := by
    unfold versionDigits at hv; split_ifs at hv <;> omega
  refine ⟨rfl, rfl, by omega, by omega, ?_, ?_, ?_, ?_, hmin⟩
  · rw [seg_append g (show 0 ≤ m by omega) (show m ≤ j by omega), seg_cons (show m < j by omega),
      seg_append g (show m + 1 ≤ te by omega) (show te ≤ j by omega), seg_cons (show te < j by omega), h2, h4.2]
    have hver : seg g (te + 1) j = H1Valid.ascii "HTTP/1." ++ [48 + v] := by
      apply List.ext_getElem
      · simp [seg_length, H1Valid.ascii]; omega
      · intro i h1 h2
        rw [getElem_seg]
        simp only [seg_length] at h1
        have : i < 8 := by omega
        rcases i with _ | _ | _ | _ | _ | _ | _ | _ | i
        all_goals (try omega)
        all_goals simp_all [litHttp, H1Valid.ascii]
    rw [hver]
    simp [H1Valid.RequestLine.bytes]
  · rw [seg_cons (by omega)]; simp
  · apply all_seg; intro k hk1 hk2
    rw [← tchar_eq _ (hg k)]; rw [← hm] at hk2; exact scan_seg isTchar 0 j k hk1 hk2
  · rw [seg_cons (show m + 1 < te by omega), h4.1]
    apply H1Valid.originForm_of_run
    rw [← pctRun_congr target_eq _ (seg_lt hg _ _), ← hT]
    have := pctRun_spanEnd hg 1 (le_refl _) j (m + 1)
    rw [seg_cons (show m + 1 < spanEnd 1 g (m + 1) j by omega), h4.1, H1Valid.pctRun_cons_ne (by decide)] at this
    simp only [Bool.and_eq_true] at this
    exact this.2

/-! ## Field lines -/

theorem headerLine_valid {i j k vs ve : Nat} (h : headerLine g i j = .ok k vs ve) :
    (⟨seg g i k, seg g (k + 1) vs, seg g vs ve, seg g ve j⟩ : H1Valid.FieldLine).ok ∧
    seg g i j = (⟨seg g i k, seg g (k + 1) vs, seg g vs ve, seg g ve j⟩ : H1Valid.FieldLine).bytes ∧
    i < k ∧ k < j ∧ k + 1 ≤ vs ∧ vs ≤ ve ∧ ve ≤ j := by
  unfold headerLine at h
  dsimp only at h
  split_ifs at h with h1 h2 h3
  simp only [HdrLine.ok.injEq] at h
  obtain ⟨rfl, rfl, rfl⟩ := h
  simp only [not_or, not_le, ne_eq, Decidable.not_not, Bool.not_eq_false'] at h1 h2 h3
  generalize hk : scan isTchar g i j = k at *
  have hik : i < k := by have := scan_ge isTchar g i j; omega
  generalize hV : scan isOws g (k + 1) j = vs at *
  have hv1 : k + 1 ≤ vs := by rw [← hV]; exact scan_ge _ _ _ _
  have hv2 : vs ≤ j := by rw [← hV]; exact scan_le _ _ _ _ (by omega)
  generalize hE : trimEnd g vs j = ve at *
  have he1 : vs ≤ ve := by rw [← hE]; exact trimEnd_ge _ _ _ hv2
  have he2 : ve ≤ j := by rw [← hE]; exact trimEnd_le _ _ _
  have hfb : ∀ m, k + 1 ≤ m → m < j → isFieldByte (g m) = true :=
    fun m a b => H1Props.allIn_all isFieldByte g (k + 1) j m (by simpa using h3) a b
  refine ⟨⟨?_, ?_, ?_, ⟨?_, ?_⟩, ?_⟩, ?_, hik, h1.2, hv1, he1, he2⟩
  · rw [seg_cons hik]; simp
  · apply all_seg; intro m a b; rw [← tchar_eq _ (hg m)]; rw [← hk] at b; exact scan_seg isTchar i j m a b
  · apply all_seg; intro m a b; rw [← ows_eq _ (hg m)]; rw [← hV] at b; exact scan_seg isOws (k + 1) j m a b
  · apply all_seg; intro m a b; exact field_byte _ (hg m) (hfb m (by omega) (by omega))
  · by_cases hn : vs < ve
    · right
      refine ⟨⟨g vs, by rw [seg_cons hn]; rfl, ?_⟩, ⟨g (ve - 1), ?_, ?_⟩⟩
      · apply field_vchar _ (hg vs) (hfb vs (by omega) (by omega))
        have := scan_stop' (g := g) isOws (k + 1) j (by rw [hV]; omega)
        rwa [hV] at this
      · have h1' := seg_single g (ve - 1)
        rw [show ve - 1 + 1 = ve by omega] at h1'
        show (seg g vs ve).getLast? = _
        rw [seg_append g (show vs ≤ ve - 1 by omega) (show ve - 1 ≤ ve by omega), h1']; simp
      · apply field_vchar _ (hg _) (hfb (ve - 1) (by omega) (by omega))
        have := trimEnd_last (g := g) vs j (by rw [hE]; omega)
        rwa [hE] at this
    · left; exact seg_nil (by omega)
  · apply all_seg; intro m a b; rw [← ows_eq _ (hg m)]; rw [← hE] at a; exact trimEnd_ows vs j m a b
  · rw [seg_append g (show i ≤ k by omega) (show k ≤ j by omega), seg_cons (show k < j by omega), h2,
      seg_append g (show k + 1 ≤ vs by omega) (show vs ≤ j by omega),
      seg_append g (show vs ≤ ve by omega) (show ve ≤ j by omega)]
    simp [H1Valid.FieldLine.bytes]

/-- What one recorded field says, by the independent definitions. -/
def LineOk (mb minor : Nat) (l : H1Valid.FieldLine) : Prop :=
  l.ok ∧
  (H1Valid.isHost l = true → H1Valid.hostValue l.value) ∧
  (H1Valid.isContentLength l = true →
    l.value ≠ [] ∧ l.value.all H1Valid.isDigit = true ∧ H1Valid.decimal l.value ≤ mb ∧ l.ows2 = []) ∧
  (H1Valid.isTransferEncoding l = true → H1Valid.named l.value "chunked" = true ∧ minor ≠ 0 ∧ l.ows2 = [])

/-- A recorded field and its line. -/
def Rel (mb minor : Nat) (x : Field) (l : H1Valid.FieldLine) : Prop :=
  l.name = seg g x.ns x.ne ∧ l.value = seg g x.vs x.ve ∧ x.ns ≤ x.ne ∧ x.ne ≤ x.vs ∧ x.vs ≤ x.ve ∧
  LineOk mb minor l

omit hg in
theorem addHeader_host {f f' : Fields} {ns ne vs ve le minor mb : Nat}
    (h : addHeader g f ns ne vs ve le minor mb = .ok f') (hc : classify g ns ne = 1) : hostOk g vs ve = true := by
  by_contra hc'
  simp only [Bool.not_eq_true] at hc'
  unfold addHeader at h
  simp only [hc, hc', Bool.not_false, Bool.or_true, if_true] at h
  split_ifs at h <;> simp at h

omit hg in
theorem addHeader_cl {f f' : Fields} {ns ne vs ve le minor mb : Nat}
    (h : addHeader g f ns ne vs ve le minor mb = .ok f') (hc : classify g ns ne = 2) :
    ve = le ∧ ∃ v, contentLength g vs ve mb = .ok v := by
  unfold addHeader at h
  simp only [hc] at h
  split_ifs at h
  all_goals (try (simp at h; done))
  all_goals (try omega)
  have hle : ve = le := by clear h; simp_all
  refine ⟨hle, ?_⟩
  revert h
  cases contentLength g vs ve mb with
  | bad s => simp
  | ok v => intro _; exact ⟨v, rfl⟩

omit hg in
theorem addHeader_te {f f' : Fields} {ns ne vs ve le minor mb : Nat}
    (h : addHeader g f ns ne vs ve le minor mb = .ok f') (hc : classify g ns ne = 3) :
    minor ≠ 0 ∧ ve = le ∧ eqCi g vs ve litChunked = true := by
  unfold addHeader at h
  simp only [hc] at h
  split_ifs at h
  all_goals (try (simp at h; done))
  all_goals (try omega)
  clear h
  simp_all

theorem addHeader_line {f f' : Fields} {i j k vs ve minor mb : Nat} (hl : headerLine g i j = .ok k vs ve)
    (h : addHeader g f i k vs ve j minor mb = .ok f') :
    Rel (g := g) mb minor ⟨i, k, vs, ve⟩ ⟨seg g i k, seg g (k + 1) vs, seg g vs ve, seg g ve j⟩ := by
  obtain ⟨hok, -, hik, hkj, hkv, hve, hej⟩ := headerLine_valid hg hl
  obtain ⟨c1, c2, c3⟩ := classify_iff hg i k
  refine ⟨rfl, rfl, by dsimp only; omega, by dsimp only; omega, hve, hok, ?_, ?_, ?_⟩ <;> intro hn <;>
    dsimp only [H1Valid.isHost, H1Valid.isContentLength, H1Valid.isTransferEncoding] at hn ⊢
  · exact hostOk_value hg hve (addHeader_host h (c1.2 hn))
  · obtain ⟨hle, v, hv⟩ := addHeader_cl h (c2.2 hn)
    obtain ⟨a, b, d, e⟩ := contentLength_dec hg hv
    exact ⟨a, b, by rw [d]; exact e, seg_nil (by omega)⟩
  · obtain ⟨hm, hle, hch⟩ := addHeader_te h (c3.2 hn)
    have := eqCi_named hg (s := vs) (e := ve) lit_chunked (by decide)
    exact ⟨by rw [← this]; exact hch, hm, seg_nil (by omega)⟩

omit hg in
theorem seg_crlf {j : Nat} (h13 : g j = 13) (h10 : g (j + 1) = 10) : seg g j (j + 2) = H1Valid.crlf := by
  rw [seg_cons (a := j) (b := j + 2) (by omega), seg_cons (a := j + 1) (b := j + 2) (by omega),
    seg_nil (a := j + 1 + 1) (b := j + 2) (by omega), h13, h10]; rfl

inductive All2 {α β : Type} (R : α → β → Prop) : List α → List β → Prop
  | nil : All2 R [] []
  | cons {a b xs ys} : R a b → All2 R xs ys → All2 R (a :: xs) (b :: ys)

omit hg in
theorem lineTerm_crlf {i n : Nat} (h1 : lineTerm g (lineEnd g i n) n ≠ 1) (h2 : lineTerm g (lineEnd g i n) n ≠ 2) :
    lineEnd g i n + 1 < n ∧ g (lineEnd g i n) = 13 ∧ g (lineEnd g i n + 1) = 10 := by
  have hlt := H1Props.lineTerm_zero' h1 h2
  have hs := scan_stop' (g := g) notEol i n (by unfold lineEnd at hlt; omega)
  unfold lineTerm at h1 h2
  unfold lineEnd at *
  simp only [notEol, Bool.not_eq_false', Bool.or_eq_true, beq_iff_eq] at hs
  split_ifs at h1 h2 <;> omega

theorem fields_lines (n minor mb : Nat) (i : Nat) (f f' : Fields) (len : Nat)
    (h : fields g n minor mb i f = .done f' len) :
    ∃ hs ls, f'.headers = f.headers ++ hs ∧ All2 (Rel (g := g) mb minor) hs ls ∧ i + 2 ≤ len ∧
      seg g i len = (ls.map (fun l => l.bytes ++ H1Valid.crlf)).flatten ++ H1Valid.crlf := by
  rw [fields] at h
  split at h
  · dsimp only at h
    split_ifs at h with h1 h2 h3
    · simp only [Fielded.done.injEq] at h
      obtain ⟨rfl, rfl⟩ := h
      obtain ⟨hlt, c13, c10⟩ := lineTerm_crlf h1 h2
      rw [h3] at hlt c13 c10 ⊢
      refine ⟨[], [], by simp, .nil, by omega, ?_⟩
      rw [seg_crlf c13 c10]; simp
    · split at h
      · simp at h
      · rename_i k vs ve hl
        split at h
        · simp at h
        · rename_i f2 ha
          obtain ⟨hlt, c13, c10⟩ := lineTerm_crlf h1 h2
          obtain ⟨hs, ls, e1, e2, e3, e4⟩ := fields_lines n minor mb _ f2 f' len h
          obtain ⟨-, hb, hik, hkj, -, -, -⟩ := headerLine_valid hg hl
          have hij := scan_ge notEol g i n
          refine ⟨⟨i, k, vs, ve⟩ :: hs, _ :: ls, ?_, .cons (addHeader_line hg hl ha) e2, by omega, ?_⟩
          · rw [e1, H1Props.addHeader_headers ha]; simp
          · generalize hJ : lineEnd g i n = j at *
            rw [seg_append g (show i ≤ j by omega) (show j ≤ len by omega),
              seg_append g (show j ≤ j + 2 by omega) (show j + 2 ≤ len by omega), hb, e4, seg_crlf c13 c10]
            simp
  · simp at h
termination_by n - i
decreasing_by
  have := H1Props.lineTerm_zero' h1 h2
  have := scan_ge notEol g i n
  unfold lineEnd at *; omega

end

/-! ## Counting -/

theorem filter_length_all2 {α β : Type} {R : α → β → Prop} {P : β → Bool} {Q : α → Bool}
    (hPQ : ∀ x l, R x l → P l = Q x) :
    ∀ {xs ls}, All2 R xs ls → (ls.filter P).length = (xs.filter Q).length
  | _, _, .nil => rfl
  | _, _, .cons (a := x) (b := l) hr hrest => by
    simp only [List.filter_cons, hPQ x l hr]
    split <;> simp [filter_length_all2 hPQ hrest]

theorem mem_all2 {α β : Type} {R : α → β → Prop} :
    ∀ {xs ls}, All2 R xs ls → ∀ l ∈ ls, ∃ x ∈ xs, R x l
  | _, _, .nil => by simp
  | _, _, .cons (a := x) (b := l) hr hrest => by
    intro l' hl'
    simp only [List.mem_cons] at hl'
    rcases hl' with rfl | hl'
    · exact ⟨x, by simp, hr⟩
    · obtain ⟨y, hy, hr'⟩ := mem_all2 hrest l' hl'
      exact ⟨y, by simp [hy], hr'⟩

theorem all2_length {α β : Type} {R : α → β → Prop} : ∀ {xs ls}, All2 R xs ls → xs.length = ls.length
  | _, _, .nil => rfl
  | _, _, .cons _ hrest => by simp [all2_length hrest]

theorem all2_mono {α β : Type} {R R' : α → β → Prop} (h : ∀ x l, R x l → R' x l) :
    ∀ {xs ls}, All2 R xs ls → All2 R' xs ls
  | _, _, .nil => .nil
  | _, _, .cons hr hrest => .cons (h _ _ hr) (all2_mono h hrest)

theorem all2_mono_mem {α β : Type} {R R' : α → β → Prop} :
    ∀ {xs ls}, (∀ x ∈ xs, ∀ l, R x l → R' x l) → All2 R xs ls → All2 R' xs ls
  | _, _, _, .nil => .nil
  | _, _, h, .cons hr hrest =>
    .cons (h _ (by simp) _ hr) (all2_mono_mem (fun x hx l => h x (by simp [hx]) l) hrest)

theorem any_iff_length {α : Type} (l : List α) (p : α → Bool) : l.any p = true ↔ 1 ≤ (l.filter p).length := by
  rw [List.any_eq_true]
  constructor
  · rintro ⟨x, hx, hp⟩; exact List.length_pos_of_mem (List.mem_filter.2 ⟨hx, hp⟩)
  · intro h
    obtain ⟨x, hx⟩ := List.length_pos_iff_exists_mem.1 (show 0 < _ from h)
    exact ⟨x, (List.mem_filter.1 hx).1, (List.mem_filter.1 hx).2⟩

/-! ## The head -/

def fview : Framing → H1Valid.Framing
  | .empty => .empty
  | .length n => .length n
  | .chunked => .chunked

theorem window_lt (L : List Nat) (n : Nat) (hb : ∀ x ∈ L, x < 256) (k : Nat) : window L n k < 256 := by
  unfold window
  split
  · rw [List.getD_eq_getElem?_getD]
    cases hk : L[k]? with
    | none => simp
    | some x => simpa using hb x (List.mem_of_getElem? hk)
  · omega

/-- `Spec.parseHead` accepts only valid heads, with the independent framing. -/
theorem parseHead_valid (L : List Nat) (mh mb : Nat) (h : Head) (hb : ∀ x ∈ L, x < 256)
    (hd : parseHead L mh mb = .done h) :
    ∃ r fs, H1Valid.ValidHead mh mb (L.take h.len) r fs ∧ h.len ≤ L.length ∧
      fview h.framing = H1Valid.framing fs ∧
      h.method = (0, r.method.length) ∧ h.target = (r.method.length + 1, r.method.length + 1 + r.target.length) ∧
      h.minor = r.minor ∧
      All2 (fun x l => (L.drop x.ns).take (x.ne - x.ns) = l.name ∧ (L.drop x.vs).take (x.ve - x.vs) = l.value)
        h.headers fs := by
  have hw := H1Props.accept_wellformed L mh mb h hd
  dsimp only at hw
  obtain ⟨hlen, hn, h16, heach, hmin, hhost1, hh1, hc1, ht1, hboth, hchunk, hempty, hte, hcl⟩ := hw
  generalize hN : readLimit L.length mh = n at *
  have hnL : n ≤ L.length := by rw [← hN]; unfold readLimit; split <;> omega
  have hg := window_lt L n hb
  unfold parseHead at hd
  dsimp only at hd
  rw [hN] at hd
  split at hd
  · split at hd <;> simp at hd
  split at hd
  · simp at hd
  rename_i ht1' ht2'
  split at hd
  · simp at hd
  rename_i ms me ts te minor hrl
  split at hd
  · split_ifs at hd <;> simp at hd
  · simp at hd
  rename_i f len hf
  by_cases h3 : minor = 1 ∧ (!f.host) = true
  · simp [h3] at hd
  rw [if_neg h3] at hd
  simp only [HeadR.done.injEq] at hd
  subst hd
  dsimp only at *
  obtain ⟨crl, c13, c10⟩ := lineTerm_crlf (g := window L n) (i := 0) ht1' ht2'
  generalize hJ : lineEnd (window L n) 0 n = j at *
  obtain ⟨rfl, rfl, hts, hte9, hrlb, hmne, hmt, horig, hmin'⟩ := requestLine_valid hg hrl
  obtain ⟨hs, ls, e1, e2, e3, e4⟩ := fields_lines hg n minor mb (j + 2) Fields.empty f len hf
  simp only [Fields.empty, List.nil_append] at e1
  subst e1
  have hlenn : len ≤ n := hlen
  have hcount : ∀ kind str, (kind = 1 ∧ str = "host" ∨ kind = 2 ∧ str = "content-length" ∨
      kind = 3 ∧ str = "transfer-encoding") →
      (ls.filter (fun l => H1Valid.named l.name str)).length = H1Props.kindCount (window L n) f.headers kind := by
    intro kind str hk
    unfold H1Props.kindCount
    apply filter_length_all2 _ e2
    intro x l hr
    rw [hr.1]
    obtain ⟨c1, c2, c3⟩ := classify_iff hg x.ns x.ne
    rcases hk with ⟨rfl, rfl⟩ | ⟨rfl, rfl⟩ | ⟨rfl, rfl⟩
    · by_cases hc : classify (window L n) x.ns x.ne = 1 <;> simp_all
    · by_cases hc : classify (window L n) x.ns x.ne = 2 <;> simp_all
    · by_cases hc : classify (window L n) x.ns x.ne = 3 <;> simp_all
  have kH := hcount 1 "host" (Or.inl ⟨rfl, rfl⟩)
  have kC := hcount 2 "content-length" (Or.inr (Or.inl ⟨rfl, rfl⟩))
  have kT := hcount 3 "transfer-encoding" (Or.inr (Or.inr ⟨rfl, rfl⟩))
  have eH : ls.filter H1Valid.isHost = ls.filter (fun l => H1Valid.named l.name "host") := rfl
  have eC : ls.filter H1Valid.isContentLength = ls.filter (fun l => H1Valid.named l.name "content-length") := rfl
  have eT : ls.filter H1Valid.isTransferEncoding =
    ls.filter (fun l => H1Valid.named l.name "transfer-encoding") := rfl
  have lineok : ∀ l ∈ ls, LineOk mb minor l := by
    intro l hl; obtain ⟨x, -, hr⟩ := mem_all2 e2 l hl; exact hr.2.2.2.2.2
  refine ⟨⟨seg (window L n) 0 me, seg (window L n) (me + 1) te, minor⟩, ls, ⟨?_, ?_, hmne, hmt, horig, hmin',
    fun l hl => (lineok l hl).1, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_⟩, by omega, ?_, ?_, ?_, rfl, ?_⟩
  · -- the bytes
    rw [show L.take len = (L.drop 0).take (len - 0) by simp, ← seg_window L (show len ≤ n by omega) hnL,
      seg_append _ (show 0 ≤ j by omega) (show j ≤ len by omega),
      seg_append _ (show j ≤ j + 2 by omega) (show j + 2 ≤ len by omega), hrlb, e4,
      seg_cons (show j < j + 2 by omega), seg_single, c13, c10]
    simp [H1Valid.headBytes, H1Valid.crlf]
  · simp; omega
  · have := all2_length e2; omega
  · rw [eH, kH]; exact hh1
  · intro hm1; rw [eH, kH]; exact hhost1 hm1
  · intro l hl hh; exact (lineok l hl).2.1 hh
  · rw [eC, kC]; exact hc1
  · intro l hl hc; exact (lineok l hl).2.2.1 hc
  · rw [eT, kT]; exact ht1
  · intro l hl ht
    obtain ⟨a, b, c⟩ := (lineok l hl).2.2.2 ht
    exact ⟨a, by show minor = 1; omega, c⟩
  · rw [any_iff_length, any_iff_length, eC, eT, kC, kT]; omega
  · -- framing
    unfold H1Valid.framing
    have aT := any_iff_length ls H1Valid.isTransferEncoding
    have aC := any_iff_length ls H1Valid.isContentLength
    rw [eT, kT] at aT
    rw [eC, kC] at aC
    cases hfr : (if f.te = true then Framing.chunked else if f.hasCl = true then Framing.length f.cl else Framing.empty)
      with
    | chunked =>
      have := hchunk.1 hfr
      rw [if_pos (aT.2 (by omega))]; rfl
    | empty =>
      obtain ⟨z2, z3⟩ := hempty.1 hfr
      rw [if_neg (by rw [aT]; omega)]
      have : ls.find? H1Valid.isContentLength = none := by
        rw [List.find?_eq_none]; intro l hl hc
        have := aC.1 (List.any_eq_true.2 ⟨l, hl, hc⟩); omega
      rw [this]; rfl
    | length v =>
      have z3 : H1Props.kindCount (window L n) f.headers 3 ≠ 1 := fun h' => by
        have := hchunk.2 h'; rw [hfr] at this; simp at this
      rw [if_neg (by rw [aT]; omega)]
      have z2 : 1 ≤ H1Props.kindCount (window L n) f.headers 2 := by
        by_contra hz
        have : (if f.te = true then Framing.chunked else if f.hasCl = true then Framing.length f.cl
          else Framing.empty) = Framing.empty := hempty.2 ⟨by omega, by omega⟩
        rw [hfr] at this; simp at this
      obtain ⟨l, hl⟩ : ∃ l, ls.find? H1Valid.isContentLength = some l := by
        cases hfd : ls.find? H1Valid.isContentLength with
        | some l => exact ⟨l, rfl⟩
        | none =>
          rw [List.find?_eq_none] at hfd
          obtain ⟨l, hl, hc⟩ := List.any_eq_true.1 (aC.2 z2)
          exact absurd hc (hfd l hl)
      rw [hl]
      have hlm := List.mem_of_find?_eq_some hl
      have hlc := List.find?_some hl
      obtain ⟨x, hx, hr⟩ := mem_all2 e2 l hlm
      have hcx : classify (window L n) x.ns x.ne = 2 :=
        (classify_iff hg x.ns x.ne).2.1.2 (by rw [← hr.1]; exact hlc)
      have hv := (hcl v hfr).2 x hx hcx
      obtain ⟨-, -, hd, -⟩ := contentLength_dec hg hv
      simp only [fview, H1Valid.Framing.length.injEq]
      rw [hr.2.1, hd]
  · simp [seg_length]
  · simp [seg_length]; omega
  · -- spans
    apply all2_mono_mem _ e2
    intro x hx l hr
    have hxl : x.ve ≤ len := (heach x hx).2.2
    obtain ⟨h1, h2, a1, a2, a3, -⟩ := hr
    rw [h1, h2, seg_window L (by omega) hnL, seg_window L (by omega) hnL]
    exact ⟨rfl, rfl⟩

end H1Sound

namespace H1Sound
open Aeneas Aeneas.Std Result

/-- Every head the extracted `parse_head` accepts is valid by the independent
definition: its bytes are `ValidHead r fs` for the request line `r` and field
lines `fs` that its spans hold, and its framing is `H1Valid.framing fs`. -/
theorem parse_head_sound (buf : Slice U8) (lim : h5i_app_h1.Limits) (h : h5i_app_h1.Head)
    (hr : h5i_app_h1.parse_head buf lim = ok (.Done h)) :
    let L := h5i_app_h1.Proofs.B buf.val
    ∃ r fs, H1Valid.ValidHead lim.max_head.val lim.max_body.val (L.take h.len.val) r fs ∧
      h.len.val ≤ buf.length ∧
      fview (h5i_app_h1.Proofs.framingView h.framing) = H1Valid.framing fs ∧
      h.method.start.val = 0 ∧ h.method.end.val = r.method.length ∧
      h.target.start.val = r.method.length + 1 ∧ h.target.end.val = r.method.length + 1 + r.target.length ∧
      h.minor.val = r.minor ∧ h.count.val = fs.length ∧
      All2 (fun x l => (L.drop x.ns).take (x.ne - x.ns) = l.name ∧ (L.drop x.vs).take (x.ve - x.vs) = l.value)
        (h5i_app_h1.Proofs.hdrsView h.headers h.count) fs := by
  intro L
  have s := h5i_app_h1.Proofs.parse_head_spec buf lim
  rw [hr, WP.spec_ok] at s
  obtain ⟨hs, hc⟩ := s
  have hb : ∀ x ∈ L, x < 256 := by
    intro x hx
    obtain ⟨y, -, rfl⟩ := List.mem_map.1 hx
    scalar_tac
  obtain ⟨r, fs, hv, hlen, hf, hm, ht, hmin, hall⟩ :=
    parseHead_valid L _ _ (h5i_app_h1.Proofs.headView h) hb hs.symm
  have hcount := hc h rfl
  simp only [h5i_app_h1.Proofs.headView, Prod.mk.injEq] at hlen hf hm ht hmin hall
  refine ⟨r, fs, hv, by simpa [L] using hlen, hf, hm.1, hm.2, ht.1, ht.2, hmin, ?_, hall⟩
  rw [← all2_length hall, h5i_app_h1.Proofs.hdrsView_length _ _ hcount]

end H1Sound
