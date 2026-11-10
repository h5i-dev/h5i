import H1Props
/-!
# Prefix stability of the head parser

Once `parseHead` has decided (`done` or `reject`), more bytes after the
input do not change the decision. With `parseHead_reads` this makes the
result independent of how the bytes are split across reads.
-/
open Aeneas Aeneas.Std Result h5i_app_h1.Spec

namespace H1Props

/-- `g` and `g'` agree below `n`. -/
def Agree (g g' : Nat → Nat) (n : Nat) : Prop := ∀ k, k < n → g k = g' k

theorem Agree.mono {g g' : Nat → Nat} {n m : Nat} (h : Agree g g' n) (hm : m ≤ n) : Agree g g' m :=
  fun k hk => h k (by omega)

/-! ## Readers that stop at a bound read only below it -/

theorem scan_congr {g g' : Nat → Nat} {e : Nat} (h : Agree g g' e) (p : Nat → Bool) (i : Nat) :
    scan p g i e = scan p g' i e := by
  conv => lhs; rw [scan]
  conv => rhs; rw [scan]
  split
  · rename_i hi
    rw [h i hi]
    split
    · exact scan_congr h p (i + 1)
    · rfl
  · rfl
termination_by e - i

theorem allIn_congr {g g' : Nat → Nat} {e : Nat} (h : Agree g g' e) (p : Nat → Bool) (s : Nat) :
    allIn p g s e = allIn p g' s e := by
  conv => lhs; rw [allIn]
  conv => rhs; rw [allIn]
  split
  · rename_i hi
    rw [h s hi]
    split
    · exact allIn_congr h p (s + 1)
    · rfl
  · rfl
termination_by e - s

theorem trimEnd_congr {g g' : Nat → Nat} {e : Nat} (h : Agree g g' e) (s p : Nat) (hp : p ≤ e) :
    trimEnd g s p = trimEnd g' s p := by
  conv => lhs; rw [trimEnd]
  conv => rhs; rw [trimEnd]
  split
  · rename_i hi
    rw [h (p - 1) (by omega)]
    split
    · exact trimEnd_congr h s (p - 1) (by omega)
    · rfl
  · rfl
termination_by p

theorem eqFrom_congr {g g' : Nat → Nat} {s : Nat} {lit : List Nat} (h : Agree g g' (s + lit.length)) (q : Nat) :
    eqFrom g s lit q = eqFrom g' s lit q := by
  conv => lhs; rw [eqFrom]
  conv => rhs; rw [eqFrom]
  split
  · rename_i hq
    rw [h (s + q) (by omega)]
    split
    · rfl
    · exact eqFrom_congr h (q + 1)
  · rfl
termination_by lit.length - q

theorem eqCi_congr {g g' : Nat → Nat} {e : Nat} (h : Agree g g' e) (s : Nat) (lit : List Nat) :
    eqCi g s e lit = eqCi g' s e lit := by
  unfold eqCi
  split
  · rfl
  · rename_i hs
    exact eqFrom_congr (h.mono (by omega)) 0

theorem eqAt_congr {g g' : Nat → Nat} {s : Nat} {lit : List Nat} (h : Agree g g' (s + lit.length)) (q : Nat) :
    eqAt g s lit q = eqAt g' s lit q := by
  conv => lhs; rw [eqAt]
  conv => rhs; rw [eqAt]
  split
  · rename_i hq
    rw [h (s + q) (by omega)]
    split
    · rfl
    · exact eqAt_congr h (q + 1)
  · rfl
termination_by lit.length - q

theorem version_congr {g g' : Nat → Nat} {e : Nat} (h : Agree g g' e) (s : Nat) :
    version g s e = version g' s e := by
  unfold version
  split
  · rfl
  · rename_i hs
    rw [eqAt_congr (h.mono (by simp [litHttp]; omega)) 0, h (s + 5) (by omega), h (s + 6) (by omega),
      h (s + 7) (by omega)]

theorem clLoop_congr {g g' : Nat → Nat} {e : Nat} (h : Agree g g' e) (max acc p : Nat) :
    clLoop g e max acc p = clLoop g' e max acc p := by
  conv => lhs; rw [clLoop]
  conv => rhs; rw [clLoop]
  split
  · rename_i hp
    rw [h p hp]
    split
    · rfl
    · split
      · rfl
      · exact clLoop_congr h max _ (p + 1)
  · rfl
termination_by e - p

theorem contentLength_congr {g g' : Nat → Nat} {e : Nat} (h : Agree g g' e) (s max : Nat) :
    contentLength g s e max = contentLength g' s e max := by
  unfold contentLength; rw [clLoop_congr h]

theorem spanEnd_congr {g g' : Nat → Nat} {e : Nat} (h : Agree g g' e) (c i : Nat) :
    spanEnd c g i e = spanEnd c g' i e := by
  conv => lhs; rw [spanEnd]
  conv => rhs; rw [spanEnd]
  split
  · rename_i hi
    rw [h i hi]
    split
    · by_cases h3 : i + 2 < e
      · rw [h (i + 1) (by omega), h (i + 2) (by omega)]
        by_cases hc : i + 2 < e ∧ hexv (g' (i + 1)) < 16 ∧ hexv (g' (i + 2)) < 16
        · rw [if_pos hc, if_pos hc]; exact spanEnd_congr h c (i + 3)
        · rw [if_neg hc, if_neg hc]
      · rw [if_neg (by omega), if_neg (by omega)]
    · split
      · exact spanEnd_congr h c (i + 1)
      · rfl
  · rfl
termination_by e - i

theorem portOk_congr {g g' : Nat → Nat} {e : Nat} (h : Agree g g' e) (p : Nat) :
    portOk g p e = portOk g' p e := by
  unfold portOk
  split
  · rfl
  · rw [h p (by omega), spanEnd_congr h]

theorem hostOk_congr {g g' : Nat → Nat} {e : Nat} (h : Agree g g' e) (s : Nat) :
    hostOk g s e = hostOk g' s e := by
  unfold hostOk
  simp only [spanEnd_congr h, portOk_congr h]
  by_cases hs : s < e
  · rw [h s hs]
    by_cases hc : s < e ∧ g' s = 91
    · rw [if_pos hc, if_pos hc]
      by_cases hk : spanEnd 2 g' (s + 1) e = s + 1 ∨ e ≤ spanEnd 2 g' (s + 1) e
      · rcases hk with hk | hk <;> simp [hk]
      · rw [h _ (by omega)]
    · rw [if_neg hc, if_neg hc]
  · simp [hs]

theorem classify_congr {g g' : Nat → Nat} {e : Nat} (h : Agree g g' e) (s : Nat) :
    classify g s e = classify g' s e := by
  unfold classify; simp only [eqCi_congr h]

theorem requestLine_congr {g g' : Nat → Nat} {j : Nat} (h : Agree g g' j) (i : Nat) :
    requestLine g i j = requestLine g' i j := by
  unfold requestLine
  simp only [scan_congr h]
  generalize hm : scan isTchar g' i j = m
  by_cases c1 : m = i ∨ j ≤ m
  · simp [c1]
  · rw [if_neg c1, if_neg c1, h m (by omega)]
    rw [spanEnd_congr h]
    have hte1 := spanEnd_ge 1 g' (m + 1) j
    generalize hte : spanEnd 1 g' (m + 1) j = te at hte1
    by_cases c2 : te = m + 1 ∨ j ≤ te
    · simp [c2]
    · rw [h (m + 1) (by omega), h te (by omega), version_congr h]

theorem headerLine_congr {g g' : Nat → Nat} {j : Nat} (h : Agree g g' j) (i : Nat) :
    headerLine g i j = headerLine g' i j := by
  unfold headerLine
  simp only [scan_congr h, allIn_congr h]
  generalize hk : scan isTchar g' i j = k
  by_cases c1 : k = i ∨ j ≤ k
  · simp [c1]
  · rw [if_neg c1, if_neg c1, h k (by omega)]
    have hv := scan_le isOws g' (k + 1) j (by omega)
    rw [trimEnd_congr h _ j (le_refl _)]

theorem addHeader_congr {g g' : Nat → Nat} {b : Nat} (h : Agree g g' b) (f : Fields) (ns ne vs ve le minor mb : Nat)
    (hne : ne ≤ b) (hve : ve ≤ b) :
    addHeader g f ns ne vs ve le minor mb = addHeader g' f ns ne vs ve le minor mb := by
  unfold addHeader
  simp only [classify_congr (h.mono hne), contentLength_congr (h.mono hve), eqCi_congr (h.mono hve),
    hostOk_congr (h.mono hve)]

/-! ## Decided lines and fields stay decided -/

/-- A line whose end was decided below `n` (CRLF, or a bare CR or LF) ends
at the same place, the same way, given more bytes. -/
theorem line_prefix {g g' : Nat → Nat} {n n' i : Nat} (h : Agree g g' n) (hn : n ≤ n')
    (ht : lineTerm g (lineEnd g i n) n ≠ 1) :
    lineEnd g' i n' = lineEnd g i n ∧ lineTerm g' (lineEnd g i n) n' = lineTerm g (lineEnd g i n) n := by
  have hj : lineEnd g i n < n := by
    unfold lineTerm at ht; split_ifs at ht <;> omega
  have key : ∀ k, k < n → scan notEol g k n < n → scan notEol g' k n' = scan notEol g k n := by
    intro k hk hs
    induction hm : n - k using Nat.strongRecOn generalizing k with
    | _ m ih =>
    have e1 : scan notEol g k n = if notEol (g k) then scan notEol g (k + 1) n else k := by
      rw [scan, if_pos hk]
    have e2 : scan notEol g' k n' = if notEol (g' k) then scan notEol g' (k + 1) n' else k := by
      rw [scan, if_pos (by omega)]
    rw [e1] at hs ⊢
    rw [e2, ← h k hk]
    split
    · rename_i hp
      rw [if_pos hp] at hs
      have hk1 : k + 1 < n := by
        by_contra hc
        rw [scan, if_neg (by omega)] at hs; omega
      exact ih (n - (k + 1)) (by omega) (k + 1) hk1 hs rfl
    · rfl
  have hi : i < n := by
    by_contra hc
    unfold lineEnd at hj; rw [scan, if_neg (by omega)] at hj; omega
  have he : lineEnd g' i n' = lineEnd g i n := key i hi hj
  refine ⟨he, ?_⟩
  generalize hJ : lineEnd g i n = j at ht hj ⊢
  unfold lineTerm at ht ⊢
  simp only [show ¬ n ≤ j by omega, show ¬ n' ≤ j by omega, if_false] at ht ⊢
  rw [← h j hj]
  by_cases h1 : g j = 10
  · simp [h1]
  · simp only [h1, if_false] at ht ⊢
    by_cases h2 : n ≤ j + 1
    · simp [h2] at ht
    · simp only [h2, show ¬ n' ≤ j + 1 by omega, if_false]
      rw [h (j + 1) (by omega)]

theorem fields_prefix (g g' : Nat → Nat) (n n' minor mb i : Nat) (f : Fields) (h : Agree g g' n) (hn : n ≤ n')
    (hr : fields g n minor mb i f ≠ .more) : fields g' n' minor mb i f = fields g n minor mb i f := by
  rw [fields] at hr
  conv => rhs; rw [fields]
  conv => lhs; rw [fields]
  by_cases hi : i < n
  · rw [if_pos hi] at hr ⊢
    rw [if_pos (by omega)]
    dsimp only at hr ⊢
    by_cases ht1 : lineTerm g (lineEnd g i n) n = 1
    · simp [ht1] at hr
    obtain ⟨he, ht⟩ := line_prefix h hn ht1
    rw [he, ht]
    have hj : lineEnd g i n < n := by
      unfold lineTerm at ht1; split_ifs at ht1 <;> omega
    rw [if_neg ht1, if_neg ht1] at *
    by_cases h2 : lineTerm g (lineEnd g i n) n = 2
    · rw [if_pos h2, if_pos h2]
    rw [if_neg h2, if_neg h2] at *
    by_cases h3 : lineEnd g i n = i
    · rw [if_pos h3, if_pos h3]
    rw [if_neg h3, if_neg h3] at *
    rw [headerLine_congr (h.mono (le_of_lt hj))]
    split
    · rfl
    rename_i k vs ve hl
    rw [← headerLine_congr (h.mono (le_of_lt hj))] at hl
    rw [hl] at hr
    obtain ⟨-, -, hve, hk⟩ := headerLine_ok hl
    rw [addHeader_congr (h.mono (le_of_lt hj)) f i k vs ve _ minor mb (by omega) hve]
    split
    · rfl
    rename_i f2 ha
    rw [← addHeader_congr (h.mono (le_of_lt hj)) f i k vs ve _ minor mb (by omega) hve] at ha
    simp only [ha] at hr
    exact fields_prefix g g' n n' minor mb _ f2 h hn hr
  · rw [if_neg hi] at hr
    simp at hr
termination_by n - i
decreasing_by
  have := lineTerm_zero' ht1 h2
  have := scan_ge notEol g i n
  unfold lineEnd at *; omega

/-! ## The head parser -/

/-- Once `parseHead` returns `done` or `reject` on `L`, it returns the same
on `L ++ R` for every `R`: a decision depends only on bytes already there. -/
theorem parseHead_prefix (L R : List Nat) (mh mb : Nat) (h : parseHead L mh mb ≠ .incomplete) :
    parseHead (L ++ R) mh mb = parseHead L mh mb := by
  by_cases hfull : mh ≤ L.length
  · -- `max_head` bytes were already there: `parseHead_reads`.
    exact parseHead_reads (L ++ R) L mh mb (by simp; omega) hfull (by rw [List.take_append_of_le_length hfull])
  have hn : readLimit L.length mh = L.length := by simp [readLimit]; omega
  have hn' : L.length ≤ readLimit (L ++ R).length mh := by simp [readLimit]; split <;> omega
  have hA : Agree (window L L.length) (window (L ++ R) (readLimit (L ++ R).length mh)) L.length := by
    intro k hk
    simp only [window, if_pos hk, if_pos (show k < readLimit (L ++ R).length mh by omega),
      List.getD_eq_getElem?_getD, List.getElem?_append_left hk]
  unfold parseHead at h ⊢
  simp only [hn, decide_eq_false (show ¬ mh ≤ L.length from hfull)] at h ⊢
  generalize window L L.length = g at h hA ⊢
  generalize window (L ++ R) (readLimit (L ++ R).length mh) = g' at hA ⊢
  generalize readLimit (L ++ R).length mh = n' at hn' ⊢
  by_cases ht1 : lineTerm g (lineEnd g 0 L.length) L.length = 1
  · simp [ht1] at h
  obtain ⟨he, ht⟩ := line_prefix hA hn' ht1
  have hj : lineEnd g 0 L.length < L.length := by
    unfold lineTerm at ht1; split_ifs at ht1 <;> omega
  rw [he, ht, requestLine_congr (hA.mono (le_of_lt hj)) 0 |>.symm]
  try dsimp only at h ⊢
  rw [if_neg ht1] at h ⊢
  rw [if_neg ht1]
  by_cases h2 : lineTerm g (lineEnd g 0 L.length) L.length = 2
  · rw [if_pos h2, if_pos h2]
  rw [if_neg h2] at h ⊢
  rw [if_neg h2]
  rcases hrl : requestLine g 0 (lineEnd g 0 L.length) with s | ⟨ms, me, ts, te, minor⟩
  · rfl
  rw [hrl] at h
  have hfr : fields g L.length minor mb (lineEnd g 0 L.length + 2) Fields.empty ≠ .more := by
    intro hm; simp [hm] at h
  simp only
  rw [fields_prefix g g' L.length n' minor mb _ _ hA hn' hfr]
  rcases hf : fields g L.length minor mb (lineEnd g 0 L.length + 2) Fields.empty with _ | s | ⟨f, len⟩
  · exact absurd hf hfr
  · rfl
  · rfl

/-- Segmentation invariance: however a stream `S` is split into reads, the
first buffered prefix on which `parseHead` decides gives the same result. -/
theorem parseHead_split_invariant (S P P' : List Nat) (mh mb : Nat) (hP : P <+: S) (hP' : P' <+: S)
    (h : parseHead P mh mb ≠ .incomplete) (h' : parseHead P' mh mb ≠ .incomplete) :
    parseHead P mh mb = parseHead P' mh mb := by
  rcases Nat.le_total P.length P'.length with hl | hl
  · obtain ⟨R, rfl⟩ := List.prefix_of_prefix_length_le hP hP' hl
    exact (parseHead_prefix P R mh mb h).symm
  · obtain ⟨R, rfl⟩ := List.prefix_of_prefix_length_le hP' hP hl
    exact parseHead_prefix P' R mh mb h'

/-- The same for the extracted parser: once `parse_head` returns `Done` or
`Reject` on `buf`, it returns the same on every longer buffer that starts
with `buf`. -/
theorem parse_head_prefix (buf buf' : Slice U8) (lim : h5i_app_h1.Limits) (hp : buf.val <+: buf'.val) :
    ∃ r r', h5i_app_h1.parse_head buf lim = ok r ∧ h5i_app_h1.parse_head buf' lim = ok r' ∧
      (h5i_app_h1.Proofs.resultView r ≠ .incomplete →
        h5i_app_h1.Proofs.resultView r' = h5i_app_h1.Proofs.resultView r) := by
  have s := h5i_app_h1.Proofs.parse_head_spec buf lim
  have s' := h5i_app_h1.Proofs.parse_head_spec buf' lim
  obtain ⟨r, hr, hv, -⟩ := (WP.spec_equiv_exists _ _).1 s
  obtain ⟨r', hr', hv', -⟩ := (WP.spec_equiv_exists _ _).1 s'
  refine ⟨r, r', hr, hr', fun hne => ?_⟩
  obtain ⟨R, hR⟩ := hp
  rw [hv, hv']
  rw [hv] at hne
  rw [← hR, h5i_app_h1.Proofs.B, List.map_append]
  exact parseHead_prefix _ _ _ _ hne

end H1Props
