import H1Proofs
/-!
# What an accepted head satisfies

Properties of `Spec.parseHead`, carried to the extracted `parse_head` by
`parse_head_spec`.
-/
open Aeneas Aeneas.Std Result h5i_app_h1.Spec

namespace H1Props

/-! ## The read bound -/

theorem window_take (L : List Nat) (n : Nat) : window L n = window (L.take n) n := by
  funext k
  unfold window
  split
  · rename_i h
    simp [List.getD_eq_getElem?_getD, List.getElem?_take, h]
  · rfl

/-- Once the buffer holds `maxHead` bytes, the result depends only on those
bytes: a peer that sends more cannot change it, so the server never needs to
buffer more than `max_head` bytes of head. -/
theorem parseHead_reads (L L' : List Nat) (mh mb : Nat) (h : mh ≤ L.length) (h' : mh ≤ L'.length)
    (he : L.take mh = L'.take mh) : parseHead L mh mb = parseHead L' mh mb := by
  have hn : readLimit L.length mh = mh := by simp [readLimit, h]
  have hn' : readLimit L'.length mh = mh := by simp [readLimit, h']
  have hw : window L mh = window L' mh := by rw [window_take, he, ← window_take]
  unfold parseHead
  simp only [hn, hn', hw, decide_eq_true h, decide_eq_true h']

/-- The same for the extracted parser: two buffers that agree on their first
`max_head` bytes (and hold at least that many) parse to the same result. -/
theorem parse_head_reads (buf buf' : Slice U8) (lim : h5i_app_h1.Limits)
    (h : lim.max_head.val ≤ buf.length) (h' : lim.max_head.val ≤ buf'.length)
    (he : buf.val.take lim.max_head.val = buf'.val.take lim.max_head.val) :
    ∃ r r', h5i_app_h1.parse_head buf lim = ok r ∧ h5i_app_h1.parse_head buf' lim = ok r' ∧
      h5i_app_h1.Proofs.resultView r = h5i_app_h1.Proofs.resultView r' := by
  have s := h5i_app_h1.Proofs.parse_head_spec buf lim
  have s' := h5i_app_h1.Proofs.parse_head_spec buf' lim
  obtain ⟨r, hr, hv, -⟩ := (WP.spec_equiv_exists _ _).1 s
  obtain ⟨r', hr', hv', -⟩ := (WP.spec_equiv_exists _ _).1 s'
  refine ⟨r, r', hr, hr', ?_⟩
  rw [hv, hv']
  apply parseHead_reads
  · simpa using h
  · simpa using h'
  · simp only [h5i_app_h1.Proofs.B, ← List.map_take, he]

/-! ## Accepted heads are well formed -/

theorem scan_all (p : Nat → Bool) (g : Nat → Nat) (i e k : Nat) (h1 : i ≤ k) (h2 : k < scan p g i e) :
    p (g k) = true := by
  rw [scan] at h2
  split at h2
  · split at h2
    · rename_i _ hp
      by_cases hk : k = i
      · subst hk; exact hp
      · exact scan_all p g (i + 1) e k (by omega) h2
    · omega
  · omega
termination_by e - i

theorem allIn_all (p : Nat → Bool) (g : Nat → Nat) (s e k : Nat) (h : allIn p g s e = true)
    (h1 : s ≤ k) (h2 : k < e) : p (g k) = true := by
  rw [allIn] at h
  split at h
  · split at h
    · rename_i _ hp
      by_cases hk : k = s
      · subst hk; exact hp
      · exact allIn_all p g (s + 1) e k h (by omega) h2
    · simp at h
  · omega
termination_by e - s

/-- A header's name is a non-empty token. -/
def NameOk (g : Nat → Nat) (x : Field) : Prop :=
  x.ns < x.ne ∧ ∀ k, x.ns ≤ k → k < x.ne → isTchar (g k) = true

/-- A header's value holds only VCHAR, obs-text, SP, HTAB: no CR, LF or NUL. -/
def ValueOk (g : Nat → Nat) (x : Field) : Prop :=
  x.vs ≤ x.ve ∧ ∀ k, x.vs ≤ k → k < x.ve → isFieldByte (g k) = true

theorem fieldByte_not_ctl {b : Nat} (h : isFieldByte b = true) : b ≠ 13 ∧ b ≠ 10 ∧ b ≠ 0 := by
  unfold isFieldByte at h; simp at h; omega

/-- How many recorded headers have the name of kind `kind` (`classify`). -/
def kindCount (g : Nat → Nat) (hs : List Field) (kind : Nat) : Nat :=
  (hs.filter (fun x => classify g x.ns x.ne == kind)).length

theorem kindCount_append (g : Nat → Nat) (hs : List Field) (x : Field) (kind : Nat) :
    kindCount g (hs ++ [x]) kind = kindCount g hs kind + (if classify g x.ns x.ne = kind then 1 else 0) := by
  unfold kindCount; simp [List.filter_append]; split <;> simp_all

/-- What the parser keeps true of the fields it has read, within `[0, bound)`. -/
structure FInv (g : Nat → Nat) (bound mb minor : Nat) (f : Fields) : Prop where
  len : f.headers.length ≤ 16
  each : ∀ x ∈ f.headers, NameOk g x ∧ ValueOk g x ∧ x.ve ≤ bound
  host : kindCount g f.headers 1 = if f.host then 1 else 0
  cl : kindCount g f.headers 2 = if f.hasCl then 1 else 0
  te : kindCount g f.headers 3 = if f.te then 1 else 0
  teVal : ∀ x ∈ f.headers, classify g x.ns x.ne = 3 → eqCi g x.vs x.ve litChunked = true
  clVal : ∀ x ∈ f.headers, classify g x.ns x.ne = 2 → contentLength g x.vs x.ve mb = .ok f.cl
  excl : ¬ (f.hasCl = true ∧ f.te = true)
  te11 : f.te = true → minor ≠ 0

theorem kindCount_zero {g : Nat → Nat} {hs : List Field} {kind : Nat} (h : kindCount g hs kind = 0) :
    ∀ x ∈ hs, classify g x.ns x.ne ≠ kind := by
  intro x hx hc
  unfold kindCount at h
  rw [List.length_eq_zero_iff, List.filter_eq_nil_iff] at h
  exact h x hx (by simp [hc])

theorem FInv.empty (g : Nat → Nat) (bound mb minor : Nat) : FInv g bound mb minor Fields.empty := by
  constructor <;> simp [Fields.empty, kindCount]

theorem addHeader_inv {g : Nat → Nat} {bound mb minor : Nat} {f f' : Fields} {ns ne vs ve le : Nat}
    (hI : FInv g bound mb minor f) (hx : NameOk g ⟨ns, ne, vs, ve⟩ ∧ ValueOk g ⟨ns, ne, vs, ve⟩ ∧ ve ≤ bound)
    (h : addHeader g f ns ne vs ve le minor mb = .ok f') : FInv g bound mb minor f' := by
  obtain ⟨hl, he, hh, hc, ht, htv, hcv, hex, ht11⟩ := hI
  unfold addHeader at h
  have each' : ∀ y ∈ f.headers ++ [⟨ns, ne, vs, ve⟩], NameOk g y ∧ ValueOk g y ∧ y.ve ≤ bound := by
    intro y hy; rw [List.mem_append] at hy
    rcases hy with hy | hy
    · exact he y hy
    · simp at hy; subst hy; exact hx
  dsimp only at h
  split_ifs at h with h1 h2 h3 h4 h5 h6 h7 h8 h9 h10
  all_goals (try (simp at h; done))
  all_goals (try split at h)
  all_goals (try (simp at h; done))
  all_goals (simp only [Added.ok.injEq] at h; subst h)
  all_goals (refine ⟨by simp [maxHeaders] at *; omega, each', ?_, ?_, ?_, ?_, ?_, ?_, ?_⟩)
  all_goals (try (rw [kindCount_append]; simp_all; done))
  all_goals (try (simp_all; done))
  all_goals (intro y hy hy3; rw [List.mem_append] at hy; rcases hy with hy | hy)
  all_goals (try (simp at hy; subst hy; simp_all; done))
  all_goals (try (first | exact htv y hy hy3 | exact hcv y hy hy3))
  all_goals (try (exfalso; refine kindCount_zero (g := g) (hs := f.headers) ?_ y hy hy3; simp_all))

theorem headerLine_ok {g : Nat → Nat} {i j k vs ve : Nat} (h : headerLine g i j = .ok k vs ve) :
    NameOk g ⟨i, k, vs, ve⟩ ∧ ValueOk g ⟨i, k, vs, ve⟩ ∧ ve ≤ j ∧ k < j := by
  unfold headerLine at h
  dsimp only at h
  split_ifs at h with h1 h2 h3
  simp only [HdrLine.ok.injEq] at h
  obtain ⟨rfl, rfl, rfl⟩ := h
  have hj : i ≤ j := by
    by_contra hc
    have : scan isTchar g i j = i := by rw [scan, if_neg (by omega)]
    exact h1 (Or.inl this)
  have hk1 := scan_ge isTchar g i j
  have hk2 := scan_le isTchar g i j hj
  have hv1 := scan_ge isOws g (scan isTchar g i j + 1) j
  have hv2 := scan_le isOws g (scan isTchar g i j + 1) j (by omega)
  have he1 := trimEnd_ge g (scan isOws g (scan isTchar g i j + 1) j) j hv2
  have he2 := trimEnd_le g (scan isOws g (scan isTchar g i j + 1) j) j
  have hall : allIn isFieldByte g (scan isTchar g i j + 1) j = true := by simpa using h3
  unfold NameOk ValueOk; dsimp only
  refine ⟨⟨by omega, fun m hm1 hm2 => scan_all isTchar g i j m hm1 hm2⟩,
    ⟨he1, fun m hm1 hm2 => allIn_all isFieldByte g _ j m hall (by omega) (by omega)⟩, he2, by omega⟩

theorem lineTerm_zero' {g : Nat → Nat} {j n : Nat} (h1 : lineTerm g j n ≠ 1) (h2 : lineTerm g j n ≠ 2) :
    j + 1 < n := by
  unfold lineTerm at h1 h2; split_ifs at h1 h2 <;> omega

theorem addHeader_headers {g : Nat → Nat} {f f' : Fields} {ns ne vs ve le minor mb : Nat}
    (h : addHeader g f ns ne vs ve le minor mb = .ok f') : f'.headers = f.headers ++ [⟨ns, ne, vs, ve⟩] := by
  unfold addHeader at h
  dsimp only at h
  split_ifs at h
  all_goals (try (simp at h; done))
  all_goals (try split at h)
  all_goals (try (simp at h; done))
  all_goals (simp only [Added.ok.injEq] at h; subst h; rfl)

theorem fields_inv (g : Nat → Nat) (n minor mb i : Nat) (f f' : Fields) (len : Nat)
    (hI : FInv g n mb minor f) (hb : ∀ x ∈ f.headers, x.ve ≤ i) (h : fields g n minor mb i f = .done f' len) :
    FInv g n mb minor f' ∧ len ≤ n ∧ ∀ x ∈ f'.headers, x.ve ≤ len := by
  rw [fields] at h
  split at h
  · dsimp only at h
    split_ifs at h with h1 h2 h3
    · simp only [Fielded.done.injEq] at h
      obtain ⟨rfl, rfl⟩ := h
      have := lineTerm_zero' h1 h2
      refine ⟨hI, by omega, fun x hx => ?_⟩
      have := hb x hx; rw [h3] at *; omega
    · split at h
      · simp at h
      · rename_i k vs ve hl
        split at h
        · simp at h
        · rename_i f2 ha
          obtain ⟨hn, hv, hve, hk⟩ := headerLine_ok hl
          have hj := lineTerm_zero' h1 h2
          have hI2 := addHeader_inv hI ⟨hn, hv, by omega⟩ ha
          have hj' : i ≤ lineEnd g i n := scan_ge notEol g i n
          refine fields_inv g n minor mb _ f2 f' len hI2 ?_ h
          rw [addHeader_headers ha]
          intro x hx; rw [List.mem_append] at hx
          rcases hx with hx | hx
          · have := hb x hx; omega
          · simp at hx; subst hx; simp only; omega
  · simp at h
termination_by n - i
decreasing_by
  have := lineTerm_zero' h1 h2
  have := scan_ge notEol g i n
  unfold lineEnd at *; omega

theorem kindCount_pos {g : Nat → Nat} {hs : List Field} {kind : Nat} {x : Field} (hx : x ∈ hs)
    (hk : classify g x.ns x.ne = kind) : 1 ≤ kindCount g hs kind := by
  unfold kindCount
  apply List.length_pos_of_mem (a := x)
  simp [hx, hk]

theorem kindCount_some {g : Nat → Nat} {hs : List Field} {kind : Nat} (h : 1 ≤ kindCount g hs kind) :
    ∃ x ∈ hs, classify g x.ns x.ne = kind := by
  unfold kindCount at h
  obtain ⟨x, hx⟩ := List.length_pos_iff_exists_mem.1 (show 0 < _ from h)
  rw [List.mem_filter] at hx
  exact ⟨x, hx.1, by simpa using hx.2⟩

theorem clLoop_le {g : Nat → Nat} {e max acc p v : Nat} (ha : acc ≤ max) (h : clLoop g e max acc p = .ok v) :
    v ≤ max := by
  rw [clLoop] at h
  split at h
  · split_ifs at h with h1 h2
    exact clLoop_le (by omega) h
  · simp at h; omega
termination_by e - p

theorem contentLength_le {g : Nat → Nat} {s e max v : Nat} (h : contentLength g s e max = .ok v) : v ≤ max := by
  unfold contentLength at h
  split_ifs at h
  exact clLoop_le (Nat.zero_le _) h

theorem requestLine_minor {g : Nat → Nat} {i j ms me ts te minor : Nat}
    (h : requestLine g i j = .ok ms me ts te minor) : minor = 0 ∨ minor = 1 := by
  unfold requestLine at h
  dsimp only at h
  split_ifs at h with h1 h2 h3 h4 h5 h6
  simp only [ReqLine.ok.injEq] at h
  obtain ⟨-, -, -, -, rfl⟩ := h
  have : ∀ a d b, versionDigits a d b ≠ 2 → versionDigits a d b ≠ 3 → versionDigits a d b = 0 ∨
      versionDigits a d b = 1 := by
    intro a d b; unfold versionDigits; split_ifs <;> simp
  unfold version at h5 h6 ⊢
  split_ifs at h5 h6 ⊢ <;> first | omega | exact this _ _ _ h6 h5

/-- What every accepted head satisfies. `g` is what the parser read: the
input below `readLimit`, which `window_below` relates to the input itself. -/
theorem accept_wellformed (L : List Nat) (mh mb : Nat) (h : Head)
    (hd : parseHead L mh mb = .done h) :
    let n := readLimit L.length mh
    let g := window L n
    h.len ≤ n ∧ n ≤ mh ∧ h.headers.length ≤ 16 ∧
    (∀ x ∈ h.headers, NameOk g x ∧ ValueOk g x ∧ x.ve ≤ h.len) ∧
    (h.minor = 0 ∨ h.minor = 1) ∧
    (h.minor = 1 → kindCount g h.headers 1 = 1) ∧
    kindCount g h.headers 1 ≤ 1 ∧ kindCount g h.headers 2 ≤ 1 ∧ kindCount g h.headers 3 ≤ 1 ∧
    ¬ (kindCount g h.headers 2 = 1 ∧ kindCount g h.headers 3 = 1) ∧
    (h.framing = .chunked ↔ kindCount g h.headers 3 = 1) ∧
    (h.framing = .empty ↔ kindCount g h.headers 2 = 0 ∧ kindCount g h.headers 3 = 0) ∧
    (∀ x ∈ h.headers, classify g x.ns x.ne = 3 → eqCi g x.vs x.ve litChunked = true ∧ h.minor = 1) ∧
    (∀ v, h.framing = .length v → v ≤ mb ∧
      ∀ x ∈ h.headers, classify g x.ns x.ne = 2 → contentLength g x.vs x.ve mb = .ok v) := by
  intro n g
  unfold parseHead at hd
  dsimp only at hd
  split at hd
  · split at hd <;> simp at hd
  split at hd
  · simp at hd
  split at hd
  · simp at hd
  · rename_i ms me ts te minor hrl
    split at hd
    · split_ifs at hd <;> simp at hd
    · simp at hd
    · rename_i f len hf
      by_cases h3 : minor = 1 ∧ (!f.host) = true
      · simp [h3] at hd
      rw [if_neg h3] at hd
      simp only [HeadR.done.injEq] at hd
      subst hd
      obtain ⟨hI, hlen, hve⟩ := fields_inv g n minor mb _ _ f len (FInv.empty g n mb minor)
        (by simp [Fields.empty]) hf
      obtain ⟨hl, he, hh, hc, ht, htv, hcv, hex, ht11⟩ := hI
      have hm := requestLine_minor hrl
      have hn : n ≤ mh := by simp only [n, readLimit]; split <;> omega
      have hb : ∀ (b : Bool), (if b = true then 1 else 0) ≤ 1 := by intro b; split <;> omega
      dsimp only
      refine ⟨hlen, hn, hl, fun x hx => ⟨(he x hx).1, (he x hx).2.1, hve x hx⟩, hm, ?_, by rw [hh]; exact hb _,
        by rw [hc]; exact hb _, by rw [ht]; exact hb _, ?_, ?_, ?_, ?_, ?_⟩
      · intro h1'; rw [hh]; simp_all
      · rw [hc, ht]; intro ⟨a, b⟩; apply hex; constructor <;> (split at a <;> split at b <;> simp_all)
      · rw [ht]; split <;> simp_all <;> split <;> simp
      · rw [hc, ht]; split <;> simp_all <;> split <;> simp_all
      · intro x hx hk
        have h1 := kindCount_pos hx hk
        rw [ht] at h1
        have hte : f.te = true := by by_contra hc'; simp [hc'] at h1
        refine ⟨htv x hx hk, ?_⟩
        have := ht11 hte; omega
      · intro v hv
        split at hv
        · simp at hv
        · split at hv
          · rename_i hcl
            simp only [Framing.length.injEq] at hv
            subst hv
            obtain ⟨x, hx, hk⟩ := kindCount_some (g := g) (hs := f.headers) (kind := 2) (by rw [hc]; simp [hcl])
            exact ⟨contentLength_le (hcv x hx hk), fun y hy hky => hcv y hy hky⟩
          · simp at hv

theorem window_below (L : List Nat) (n k : Nat) (h : k < n) : window L n k = L.getD k 0 := by
  simp [window, h]

/-- `accept_wellformed` for the extracted parser: whenever `parse_head`
returns `Done h`, the spec's view of `h` is well formed. -/
theorem parse_head_wellformed (buf : Slice U8) (lim : h5i_app_h1.Limits) (h : h5i_app_h1.Head)
    (hr : h5i_app_h1.parse_head buf lim = ok (.Done h)) :
    let L := h5i_app_h1.Proofs.B buf.val
    let hv := h5i_app_h1.Proofs.headView h
    let n := readLimit L.length lim.max_head.val
    let g := window L n
    h.count.val ≤ 16 ∧ hv.len ≤ n ∧ n ≤ lim.max_head.val ∧
    (∀ x ∈ hv.headers, NameOk g x ∧ ValueOk g x ∧ x.ve ≤ hv.len) ∧
    (hv.minor = 0 ∨ hv.minor = 1) ∧
    (hv.minor = 1 → kindCount g hv.headers 1 = 1) ∧
    kindCount g hv.headers 1 ≤ 1 ∧ kindCount g hv.headers 2 ≤ 1 ∧ kindCount g hv.headers 3 ≤ 1 ∧
    ¬ (kindCount g hv.headers 2 = 1 ∧ kindCount g hv.headers 3 = 1) ∧
    (hv.framing = .chunked ↔ kindCount g hv.headers 3 = 1) ∧
    (hv.framing = .empty ↔ kindCount g hv.headers 2 = 0 ∧ kindCount g hv.headers 3 = 0) ∧
    (∀ x ∈ hv.headers, classify g x.ns x.ne = 3 → eqCi g x.vs x.ve litChunked = true ∧ hv.minor = 1) ∧
    (∀ v, hv.framing = .length v → v ≤ lim.max_body.val ∧
      ∀ x ∈ hv.headers, classify g x.ns x.ne = 2 → contentLength g x.vs x.ve lim.max_body.val = .ok v) := by
  intro L hv n g
  have s := h5i_app_h1.Proofs.parse_head_spec buf lim
  rw [hr, WP.spec_ok] at s
  obtain ⟨hs, hc⟩ := s
  obtain ⟨a1, a2, -, rest⟩ := accept_wellformed L lim.max_head.val lim.max_body.val hv hs.symm
  exact ⟨hc h rfl, a1, a2, rest⟩

end H1Props
