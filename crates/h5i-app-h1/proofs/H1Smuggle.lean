import H1Props
/-!
# No request smuggling, relative to a family of lenient parsers

Request smuggling needs two parsers on the path of one byte stream to
disagree on where a message ends. `Interp` is a family of lenient head
parsers, one per choice in `Opts`, each a behaviour real servers and proxies
have shipped:

* `lines`: end lines at CR LF only, or at LF (dropping a CR before it);
* `dupLast`: of several `Content-Length` fields, the last wins (else the first);
* `teWins`: with both `Content-Length` and `Transfer-Encoding`, the coding
  wins (else the length);
* `stripColonWs`: whitespace before the colon is dropped from the name (so
  `Transfer-Encoding :` is the coding), or kept (so it is another field);
* `unfold`: an obs-fold line continues the previous field (so a folded
  `Content-Length` is no number, a folded coding not `chunked`), or is ignored;
* `trimValue`: trailing whitespace is trimmed from values, or kept.

Every member compares names ASCII case-insensitively, skips the whitespace
after the colon, skips the request line, ends the head at the first empty
line, and takes the body as chunked when the last coding is `chunked`.

`no_smuggling`: whenever `parseHead` accepts, every member, having received
any prefix of the input that holds the head, accepts too and computes the
same head length and the same framing. So no member of the family sees a
message boundary anywhere else.

The theorem is relative to the family. Outside it, for example: a parser
that compares names case-sensitively (RFC 9110 §5.1 says they are
case-insensitive; one recognizing only `Content-Length` would read a body
framed by `content-length` as the next request, and no parser can agree with
both it and a conforming one without refusing every other spelling); one
that reads a `Content-Length` list (`3, 3`) or skips leading zeros
differently when it rejects them (it then rejects, which is no smuggling);
and anything about chunked bodies, which `chunk_step_spec` covers separately.
-/
open h5i_app_h1.Spec

namespace H1Smuggle

inductive LineMode where
  | crlf
  | lf
  deriving DecidableEq, Repr

structure Opts where
  lines : LineMode
  dupLast : Bool
  teWins : Bool
  stripColonWs : Bool
  unfold : Bool
  trimValue : Bool
  deriving DecidableEq, Repr

/-- The first CR LF at or after `i`, within `[0, n)`. -/
def crlfAt (g : Nat → Nat) (i n : Nat) : Option Nat :=
  if i + 1 < n then (if g i = 13 ∧ g (i + 1) = 10 then some i else crlfAt g (i + 1) n) else none
termination_by n - i

/-- The first LF at or after `i`, within `[0, n)`. -/
def lfAt (g : Nat → Nat) (i n : Nat) : Option Nat :=
  if i < n then (if g i = 10 then some i else lfAt g (i + 1) n) else none
termination_by n - i

/-- The line starting at `i`: where its content ends, and where the next starts. -/
def nextLine (m : LineMode) (g : Nat → Nat) (i n : Nat) : Option (Nat × Nat) :=
  match m with
  | .crlf => (crlfAt g i n).map fun k => (k, k + 2)
  | .lf => (lfAt g i n).map fun k => (if i < k ∧ g (k - 1) = 13 then k - 1 else k, k + 1)

/-- Decimal digits from `p` to `e` on top of `acc`, without a limit. -/
def decFrom (g : Nat → Nat) (e acc p : Nat) : Option Nat :=
  if p < e then (if g p < 48 ∨ 57 < g p then none else decFrom g e (acc * 10 + (g p - 48)) (p + 1))
  else some acc
termination_by e - p

def decAll (g : Nat → Nat) (s e : Nat) : Option Nat := if e ≤ s then none else decFrom g e 0 s

/-- What a lenient parser keeps of the fields: each `Content-Length` value,
decoded, and for each `Transfer-Encoding` whether it is `chunked`. `last`:
the kind of the last field, which an obs-fold line continues. -/
structure LSt where
  cls : List (Option Nat)
  tes : List Bool
  last : Nat

/-- One non-empty header line `[i, le)`. -/
def lfield (o : Opts) (g : Nat → Nat) (st : LSt) (i le : Nat) : LSt :=
  if isOws (g i) then
    if o.unfold then
      if st.last = 2 then { st with cls := st.cls.dropLast ++ [none] }
      else if st.last = 3 then { st with tes := st.tes.dropLast ++ [false] }
      else st
    else st
  else
    let c := scan (fun b => b != 58) g i le
    if le ≤ c then { st with last := 0 }
    else
      let ne := if o.stripColonWs then trimEnd g i c else c
      let vs := scan isOws g (c + 1) le
      let ve := if o.trimValue then trimEnd g vs le else le
      let kind := classify g i ne
      if kind = 2 then { st with cls := st.cls ++ [decAll g vs ve], last := 2 }
      else if kind = 3 then { st with tes := st.tes ++ [eqCi g vs ve litChunked], last := 3 }
      else { st with last := 0 }

/-- The header lines from `i` to the empty line: the head's length and the fields. -/
def lfields (o : Opts) (g : Nat → Nat) (n i : Nat) (st : LSt) : Option (Nat × LSt) :=
  if i < n then
    match nextLine o.lines g i n with
    | none => none
    | some (le, nx) =>
      if le = i then some (nx, st)
      else if nx ≤ i then none
      else lfields o g n nx (lfield o g st i le)
  else none
termination_by n - i
decreasing_by omega

def pickCl (o : Opts) (cls : List (Option Nat)) : Option Nat :=
  (if o.dupLast then cls.getLast? else cls.head?).bind id

/-- The framing from the fields. `none`: the parser rejects. -/
def frame (o : Opts) (st : LSt) : Option Framing :=
  match st.tes, st.cls with
  | [], [] => some .empty
  | [], cls => (pickCl o cls).map .length
  | tes, [] => if tes.getLast? = some true then some .chunked else none
  | tes, cls =>
    if o.teWins then (if tes.getLast? = some true then some .chunked else none)
    else (pickCl o cls).map .length

/-- Member `o` of the family, having received the first `n` bytes of `L`:
the head's length and the body's framing, or `none` if it rejects. -/
def interp (o : Opts) (L : List Nat) (n : Nat) : Option (Nat × Framing) :=
  let g := window L n
  match nextLine o.lines g 0 n with
  | none => none
  | some (_, nx) =>
    match lfields o g n nx ⟨[], [], 0⟩ with
    | none => none
    | some (len, st) => (frame o st).map (len, ·)

/-! ## Lemmas -/

theorem crlfAt_eq (g : Nat → Nat) (i j n : Nat) (hij : i ≤ j) (hjn : j + 1 < n)
    (hno : ∀ k, i ≤ k → k < j → g k ≠ 13) (h13 : g j = 13) (h10 : g (j + 1) = 10) :
    crlfAt g i n = some j := by
  rw [crlfAt, if_pos (by omega)]
  by_cases hk : i = j
  · subst hk; rw [if_pos ⟨h13, h10⟩]
  · rw [if_neg (fun h => hno i (le_refl _) (by omega) h.1)]
    exact crlfAt_eq g (i + 1) j n (by omega) hjn (fun k h1 h2 => hno k (by omega) h2) h13 h10
termination_by j - i

theorem lfAt_eq (g : Nat → Nat) (i j n : Nat) (hij : i ≤ j) (hjn : j < n)
    (hno : ∀ k, i ≤ k → k < j → g k ≠ 10) (h10 : g j = 10) : lfAt g i n = some j := by
  rw [lfAt, if_pos (by omega)]
  by_cases hk : i = j
  · subst hk; rw [if_pos h10]
  · rw [if_neg (hno i (le_refl _) (by omega))]
    exact lfAt_eq g (i + 1) j n (by omega) hjn (fun k h1 h2 => hno k (by omega) h2) h10
termination_by j - i

/-- A line `[i, j)` with no CR or LF, ended by CR LF: both modes find it. -/
theorem nextLine_eq (m : LineMode) (g : Nat → Nat) (i j n : Nat) (hij : i ≤ j) (hjn : j + 1 < n)
    (hno : ∀ k, i ≤ k → k < j → g k ≠ 13 ∧ g k ≠ 10) (h13 : g j = 13) (h10 : g (j + 1) = 10) :
    nextLine m g i n = some (j, j + 2) := by
  cases m
  · simp [nextLine, crlfAt_eq g i j n hij hjn (fun k a b => (hno k a b).1) h13 h10]
  · have := lfAt_eq g i (j + 1) n (by omega) hjn
      (fun k a b => by
        by_cases hk : k = j
        · subst hk; rw [h13]; omega
        · exact (hno k a (by omega)).2) h10
    simp only [nextLine, this, Option.map_some]
    rw [if_pos ⟨by omega, by simpa using h13⟩]
    simp

theorem scan_congr (p : Nat → Bool) (g g' : Nat → Nat) (i e : Nat) (h : ∀ k, k < e → g k = g' k) :
    scan p g i e = scan p g' i e := by
  conv_lhs => rw [scan]
  conv_rhs => rw [scan]
  split
  · rw [h i (by omega)]
    split
    · exact scan_congr p g g' (i + 1) e h
    · rfl
  · rfl
termination_by e - i

theorem trimEnd_congr (g g' : Nat → Nat) (s p : Nat) (h : ∀ k, k < p → g k = g' k) :
    trimEnd g s p = trimEnd g' s p := by
  conv_lhs => rw [trimEnd]
  conv_rhs => rw [trimEnd]
  split
  · rw [h (p - 1) (by omega)]
    split
    · exact trimEnd_congr g g' s (p - 1) (fun k hk => h k (by omega))
    · rfl
  · rfl
termination_by p

theorem eqFrom_congr (g g' : Nat → Nat) (s : Nat) (lit : List Nat) (q : Nat)
    (h : ∀ k, k < s + lit.length → g k = g' k) : eqFrom g s lit q = eqFrom g' s lit q := by
  conv_lhs => rw [eqFrom]
  conv_rhs => rw [eqFrom]
  split
  · rw [h (s + q) (by omega)]
    split
    · rfl
    · exact eqFrom_congr g g' s lit (q + 1) h
  · rfl
termination_by lit.length - q

theorem eqCi_congr (g g' : Nat → Nat) (s e : Nat) (lit : List Nat) (h : ∀ k, k < e → g k = g' k) :
    eqCi g s e lit = eqCi g' s e lit := by
  unfold eqCi
  split
  · rfl
  · exact eqFrom_congr g g' s lit 0 (fun k hk => h k (by omega))

theorem classify_congr (g g' : Nat → Nat) (s e : Nat) (h : ∀ k, k < e → g k = g' k) :
    classify g s e = classify g' s e := by
  unfold classify
  rw [eqCi_congr g g' s e _ h, eqCi_congr g g' s e _ h, eqCi_congr g g' s e _ h]

theorem decFrom_of_clLoop (g g' : Nat → Nat) (e max acc p v : Nat) (h : ∀ k, k < e → g k = g' k)
    (hc : clLoop g e max acc p = .ok v) : decFrom g' e acc p = some v := by
  rw [clLoop] at hc
  rw [decFrom]
  split at hc
  · rename_i hp
    rw [if_pos hp, ← h p hp]
    split_ifs at hc with h1 h2
    rw [if_neg h1]
    exact decFrom_of_clLoop g g' e max _ (p + 1) v h hc
  · rename_i hp
    simp at hc
    rw [if_neg hp, hc]
termination_by e - p

theorem decAll_of_contentLength (g g' : Nat → Nat) (s e max v : Nat) (h : ∀ k, k < e → g k = g' k)
    (hc : contentLength g s e max = .ok v) : decAll g' s e = some v := by
  unfold contentLength at hc
  unfold decAll
  split_ifs at hc with h1
  rw [if_neg h1]
  exact decFrom_of_clLoop g g' e max 0 s v h hc

theorem tchar_ge (b : Nat) (h : isTchar b = true) : 33 ≤ b ∧ b ≠ 58 := by
  unfold isTchar at h
  split_ifs at h <;> simp at h <;> omega

theorem tchar_not_ows (b : Nat) (h : isTchar b = true) : isOws b = false := by
  have := tchar_ge b h
  unfold isOws; simp; omega

theorem trimEnd_self (g : Nat → Nat) (s p : Nat) (h : s < p) (hp : isOws (g (p - 1)) = false) :
    trimEnd g s p = p := by
  rw [trimEnd, if_pos h, if_neg (by simp [hp])]

theorem scan_stops (p : Nat → Bool) (g : Nat → Nat) (i e : Nat) (h : scan p g i e < e) :
    p (g (scan p g i e)) = false := by
  rw [scan] at h ⊢
  split at h
  · rename_i hi
    rw [if_pos hi]
    split at h
    · rename_i hp; rw [if_pos hp]; exact scan_stops p g (i + 1) e h
    · rename_i hp; rw [if_neg hp]; simpa using hp
  · rename_i hi; rw [if_neg hi]; omega
termination_by e - i

/-- A line's terminator, from `lineEnd` and `lineTerm`. -/
theorem line_facts (g : Nat → Nat) (i n j : Nat) (hj : j = lineEnd g i n) (h1 : lineTerm g j n ≠ 1)
    (h2 : lineTerm g j n ≠ 2) :
    i ≤ j ∧ j + 1 < n ∧ g j = 13 ∧ g (j + 1) = 10 ∧ ∀ k, i ≤ k → k < j → g k ≠ 13 ∧ g k ≠ 10 := by
  have hj1 := H1Props.lineTerm_zero' h1 h2
  have hge : i ≤ j := by rw [hj]; exact scan_ge notEol g i n
  have hstop : notEol (g j) = false := by
    rw [hj]; exact scan_stops notEol g i n (by unfold lineEnd at hj; omega)
  refine ⟨hge, hj1, ?_, ?_, ?_⟩
  · unfold lineTerm at h1 h2
    simp only [notEol] at hstop
    split_ifs at h1 h2 <;> simp_all <;> omega
  · unfold lineTerm at h1 h2
    split_ifs at h1 h2 <;> simp_all
  · intro k hk1 hk2
    have := H1Props.scan_all notEol g i n k hk1 (by unfold lineEnd at hj; omega)
    simp [notEol] at this; omega

theorem scan_eq (p : Nat → Bool) (g : Nat → Nat) (i k e : Nat) (hik : i ≤ k) (hke : k < e)
    (hall : ∀ m, i ≤ m → m < k → p (g m) = true) (hstop : p (g k) = false) : scan p g i e = k := by
  rw [scan, if_pos (by omega)]
  by_cases h : i = k
  · subst h; rw [if_neg (by simp [hstop])]
  · rw [if_pos (hall i (le_refl _) (by omega))]
    exact scan_eq p g (i + 1) k e (by omega) hke (fun m h1 h2 => hall m (by omega) h2) hstop
termination_by k - i

theorem headerLine_eqs {g : Nat → Nat} {i j k vs ve : Nat} (h : headerLine g i j = .ok k vs ve) :
    k = scan isTchar g i j ∧ vs = scan isOws g (k + 1) j ∧ ve = trimEnd g vs j ∧ g k = 58 := by
  unfold headerLine at h
  dsimp only at h
  split_ifs at h with h1 h2 h3
  simp only [HdrLine.ok.injEq] at h
  obtain ⟨rfl, rfl, rfl⟩ := h
  exact ⟨rfl, rfl, rfl, by simpa using h2⟩

/-- What a successful `addHeader` did to the framing fields. -/
theorem addHeader_cases {g : Nat → Nat} {f f' : Fields} {ns ne vs ve le minor mb : Nat}
    (h : addHeader g f ns ne vs ve le minor mb = .ok f') :
    (classify g ns ne ≠ 2 ∧ classify g ns ne ≠ 3 ∧ f'.hasCl = f.hasCl ∧ f'.cl = f.cl ∧ f'.te = f.te) ∨
    (classify g ns ne = 2 ∧ f.hasCl = false ∧ f.te = false ∧ ve = le ∧
      contentLength g vs ve mb = .ok f'.cl ∧ f'.hasCl = true ∧ f'.te = false) ∨
    (classify g ns ne = 3 ∧ f.hasCl = false ∧ f.te = false ∧ ve = le ∧
      eqCi g vs ve litChunked = true ∧ f'.hasCl = false ∧ f'.te = true) := by
  unfold addHeader at h
  dsimp only at h
  split_ifs at h
  all_goals (try (simp at h; done))
  all_goals (try split at h)
  all_goals (try (simp at h; done))
  all_goals (simp only [Added.ok.injEq] at h; subst h)
  all_goals (first
    | (left; refine ⟨by omega, by omega, rfl, rfl, rfl⟩; done)
    | (left; simp_all; done)
    | (right; left; simp_all; done)
    | (right; right; simp_all; done))

/-- How the lenient state follows ours. -/
def R (f : Fields) (st : LSt) : Prop :=
  st.cls = (if f.hasCl then [some f.cl] else []) ∧ st.tes = (if f.te then [true] else [])

theorem fields_len_ge (g : Nat → Nat) (n minor mb i : Nat) (f f' : Fields) (len : Nat)
    (h : fields g n minor mb i f = .done f' len) : i + 2 ≤ len := by
  rw [fields] at h
  split at h
  · dsimp only at h
    split_ifs at h with h1 h2 h3
    · simp only [Fielded.done.injEq] at h; obtain ⟨-, rfl⟩ := h; omega
    · split at h
      · simp at h
      · split at h
        · simp at h
        · have := fields_len_ge g n minor mb _ _ f' len h
          have := scan_ge notEol g i n
          unfold lineEnd at *; omega
  · simp at h
termination_by n - i
decreasing_by
  have := H1Props.lineTerm_zero' h1 h2
  have := scan_ge notEol g i n
  unfold lineEnd at *; omega

theorem fields_sim (o : Opts) (g g' : Nat → Nat) (n n' minor mb i : Nat) (f f' : Fields) (len : Nat)
    (st : LSt) (hf : fields g n minor mb i f = .done f' len) (hR : R f st)
    (hag : ∀ k, k < len → g k = g' k) (hlen : len ≤ n') :
    ∃ st', lfields o g' n' i st = some (len, st') ∧ R f' st' := by
  have hge := fields_len_ge g n minor mb i f f' len hf
  rw [fields] at hf
  split at hf
  · dsimp only at hf
    split_ifs at hf with h1 h2 h3
    · -- the empty line
      simp only [Fielded.done.injEq] at hf
      obtain ⟨rfl, rfl⟩ := hf
      obtain ⟨hij, hjn, h13, h10, hno⟩ := line_facts g i n _ rfl h1 h2
      have hj : lineEnd g i n = i := h3
      rw [hj] at hjn h13 h10
      have hnl : nextLine o.lines g' i n' = some (i, i + 2) :=
        nextLine_eq o.lines g' i i n' (le_refl _) (by omega)
          (fun k a b => by omega) (by rw [← hag i (by omega)]; exact h13)
          (by rw [← hag (i + 1) (by omega)]; exact h10)
      refine ⟨st, ?_, hR⟩
      rw [lfields, if_pos (by omega), hnl]
      simp [hj]
    · split at hf
      · simp at hf
      · rename_i k vs ve hl
        split at hf
        · simp at hf
        · rename_i f2 ha
          obtain ⟨hij, hjn, h13, h10, hno⟩ := line_facts g i n _ rfl h1 h2
          generalize hjdef : lineEnd g i n = j at *
          have hlen2 := fields_len_ge g n minor mb _ _ f' len hf
          have hagj : ∀ k, k < j + 2 → g k = g' k := fun k hk => hag k (by omega)
          have hnl : nextLine o.lines g' i n' = some (j, j + 2) :=
            nextLine_eq o.lines g' i j n' hij (by omega)
              (fun k a b => by rw [← hagj k (by omega)]; exact hno k a b)
              (by rw [← hagj j (by omega)]; exact h13) (by rw [← hagj (j + 1) (by omega)]; exact h10)
          obtain ⟨hk, hvs, hve, h58⟩ := headerLine_eqs hl
          obtain ⟨⟨hik, hnm⟩, ⟨hvsve, -⟩, hvej, hkj⟩ := H1Props.headerLine_ok hl
          dsimp only at hik hnm hvsve hvej hkj
          -- the lenient parser reads this line as we do
          have hgi : isOws (g' i) = false := by
            rw [← hagj i (by omega)]; exact tchar_not_ows _ (hnm i (le_refl _) hik)
          have hc : scan (fun b => b != 58) g' i j = k := by
            apply scan_eq _ _ _ _ _ (by omega) hkj
            · intro m h1 h2
              rw [← hagj m (by omega)]
              have := tchar_ge _ (hnm m h1 h2); simp; omega
            · rw [← hagj k (by omega), h58]; rfl
          have hne : (if o.stripColonWs then trimEnd g' i k else k) = k := by
            split
            · apply trimEnd_self _ _ _ hik
              rw [← hagj (k - 1) (by omega)]
              exact tchar_not_ows _ (hnm (k - 1) (by omega) (by omega))
            · rfl
          have hvs' : scan isOws g' (k + 1) j = vs := by
            rw [hvs]; exact (scan_congr _ _ _ _ _ (fun m hm => hagj m (by omega))).symm
          have hcl : classify g' i k = classify g i k :=
            (classify_congr _ _ _ _ (fun m hm => hagj m (by omega))).symm
          have hlf : lfields o g' n' i st = lfields o g' n' (j + 2) (lfield o g' st i j) := by
            rw [lfields, if_pos (by omega), hnl]
            simp only
            rw [if_neg (by omega), if_neg (by omega)]
          -- the lenient state after this line follows ours
          have hRl : R f2 (lfield o g' st i j) := by
            obtain ⟨hcls, htes⟩ := hR
            unfold lfield
            rw [if_neg (by simp [hgi]), hc]
            simp only
            rw [if_neg (by omega), hne, hvs', hcl]
            rcases addHeader_cases ha with ⟨k2, k3, e1, e2, e3⟩ | ⟨k2, c1, c2, c3, c4, c5, c6⟩ |
              ⟨k3, c1, c2, c3, c4, c5, c6⟩
            · rw [if_neg k2, if_neg k3]
              exact ⟨by simp [hcls, e1, e2], by simp [htes, e3]⟩
            · rw [if_pos k2]
              have hve' : (if o.trimValue then trimEnd g' vs j else j) = j := by
                split
                · rw [← trimEnd_congr _ _ _ _ (fun m hm => hagj m (by omega)), ← hve, c3]
                · rfl
              rw [hve']
              have hd := decAll_of_contentLength g g' vs j mb f2.cl (fun m hm => hagj m (by omega))
                (by rw [← c3]; exact c4)
              refine ⟨?_, ?_⟩
              · simp [hcls, c1, c5, hd]
              · simp [htes, c2, c6]
            · rw [if_neg (by omega), if_pos k3]
              have hve' : (if o.trimValue then trimEnd g' vs j else j) = j := by
                split
                · rw [← trimEnd_congr _ _ _ _ (fun m hm => hagj m (by omega)), ← hve, c3]
                · rfl
              rw [hve', ← eqCi_congr g g' vs j _ (fun m hm => hagj m (by omega)), ← c3, c4]
              refine ⟨?_, ?_⟩
              · simp [hcls, c1, c5]
              · simp [htes, c2, c6]
          obtain ⟨st2, hst2, hR2⟩ := fields_sim o g g' n n' minor mb (j + 2) f2 f' len (lfield o g' st i j) hf hRl hag hlen
          exact ⟨st2, by rw [hlf]; exact hst2, hR2⟩
  · simp at hf
termination_by n - i
decreasing_by
  have := H1Props.lineTerm_zero' h1 h2
  have := scan_ge notEol g i n
  unfold lineEnd at *; omega

theorem frame_sim (o : Opts) (f : Fields) (st : LSt) (hR : R f st) (hex : ¬ (f.hasCl = true ∧ f.te = true)) :
    frame o st = some (if f.te then .chunked else if f.hasCl then .length f.cl else .empty) := by
  obtain ⟨hc, ht⟩ := hR
  unfold frame
  cases h1 : f.hasCl <;> cases h2 : f.te <;> simp_all [pickCl]

/-- **No smuggling.** Whenever `parseHead` accepts `L`, every member of
`Interp`, having received any prefix of `L` that holds the head (`L` itself
included), accepts it with the same head length and the same framing. -/
theorem no_smuggling (L : List Nat) (mh mb : Nat) (h : Head) (hd : parseHead L mh mb = .done h)
    (o : Opts) (n' : Nat) (h1 : h.len ≤ n') (h2 : n' ≤ L.length) :
    interp o L n' = some (h.len, h.framing) := by
  have hw := H1Props.accept_wellformed L mh mb h hd
  dsimp only at hw
  obtain ⟨hlenn, -⟩ := hw
  unfold parseHead at hd
  dsimp only at hd
  split at hd
  · split at hd <;> simp at hd
  split at hd
  · simp at hd
  split at hd
  · simp at hd
  rename_i ht1 ht2 _ ms me ts te minor hrl
  split at hd
  · split_ifs at hd <;> simp at hd
  · simp at hd
  rename_i f len hf
  by_cases h3 : minor = 1 ∧ (!f.host) = true
  · simp [h3] at hd
  rw [if_neg h3] at hd
  simp only [HeadR.done.injEq] at hd
  subst hd
  dsimp only at h1 hlenn ⊢
  generalize hn : readLimit L.length mh = n at *
  generalize hg : window L n = g at *
  have hag : ∀ k, k < len → g k = window L n' k := by
    intro k hk; rw [← hg]; simp [window, show k < n by omega, show k < n' by omega]
  have hlen4 := fields_len_ge g n minor mb _ _ f len hf
  obtain ⟨hij, hjn, h13, h10, hno⟩ := line_facts g 0 n _ rfl ht1 ht2
  generalize hj0 : lineEnd g 0 n = j0 at *
  have hnl : nextLine o.lines (window L n') 0 n' = some (j0, j0 + 2) :=
    nextLine_eq o.lines _ 0 j0 n' (by omega) (by omega)
      (fun k a b => by rw [← hag k (by omega)]; exact hno k a b)
      (by rw [← hag j0 (by omega)]; exact h13) (by rw [← hag (j0 + 1) (by omega)]; exact h10)
  obtain ⟨st, hst, hR⟩ := fields_sim o g (window L n') n n' minor mb (j0 + 2) Fields.empty f len ⟨[], [], 0⟩ hf
    (by simp [R, Fields.empty]) hag (by omega)
  obtain ⟨hI, -, -⟩ := H1Props.fields_inv g n minor mb _ _ f len (H1Props.FInv.empty g n mb minor)
    (by simp [Fields.empty]) hf
  unfold interp
  simp only [hnl, hst, frame_sim o f st hR hI.excl, Option.map_some]

end H1Smuggle
