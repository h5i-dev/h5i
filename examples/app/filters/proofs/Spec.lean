import FiltersKernel
import H5iAppLib
/-!
# The saved-filters specification

Text is a list of bytes. `escM` and `substM` model `escape_into` and
`substitute`, `parseM` the parser's state machine, one `pstep` per byte. The
round trip (`parse_substitute`) is proven here on lists; `Lemmas.lean` proves
the extracted functions compute these models.
-/
open Aeneas Aeneas.Std Result H5iAppLib filters_kernel

namespace Filters

theorem QUOTE_val : QUOTE = 39#u8 := by unfold QUOTE; rfl
theorem BSLASH_val : BSLASH = 92#u8 := by unfold BSLASH; rfl
theorem BAR_val : BAR = 124#u8 := by unfold BAR; rfl
theorem EQ_val : EQ = 61#u8 := by unfold EQ; rfl
theorem HOLE_val : HOLE = 36#u8 := by unfold HOLE; rfl

/-- The five special bytes as literals, so `decide` can compare them. -/
theorem bytes_val : QUOTE = 39#u8 ∧ BSLASH = 92#u8 ∧ BAR = 124#u8 ∧ EQ = 61#u8 ∧ HOLE = 36#u8 :=
  ⟨QUOTE_val, BSLASH_val, BAR_val, EQ_val, HOLE_val⟩

/-! ## Escaping and substitution -/

/-- One byte of a value, escaped. -/
def escB (b : U8) : List U8 := if b = BSLASH then [BSLASH, b] else if b = QUOTE then [BSLASH, b] else [b]

def escM (v : List U8) : List U8 := v.flatMap escB

/-- Before the fix: only `\` is escaped. -/
def escPreB (b : U8) : List U8 := if b = BSLASH then [BSLASH, b] else [b]

def escPreM (v : List U8) : List U8 := v.flatMap escPreB

/-- The template with each `$` replaced by `esc nm`. -/
def substM (esc : List U8 → List U8) (nm : List U8) (tpl : List U8) : List U8 :=
  tpl.flatMap (fun b => if b = HOLE then esc nm else [b])

/-! ## The parser -/

/-- A clause as bytes: key and value. -/
abbrev CL := List U8 × List U8

/-- An extracted clause as bytes. -/
def toCL (c : Clause) : CL := (c.key.val, c.val.val)

/-- Parser state: clauses so far, key, value, and where in a clause it is. -/
abbrev PS := List CL × List U8 × List U8 × St

/-- One byte of `parse`: `.inl` continues, `.inr` returns. -/
def pstep : PS → U8 → PS ⊕ Option (List CL)
  | (out, key, val, st), b =>
    match st with
    | .Key =>
      if b = EQ then .inl (out, key, val, .Open)
      else if b = QUOTE then .inr none
      else if b = BAR then .inr none
      else if b = BSLASH then .inr none
      else .inl (out, key ++ [b], val, .Key)
    | .Open => if b = QUOTE then .inl (out, key, val, .Val) else .inr none
    | .Val =>
      if b = BSLASH then .inl (out, key, val, .Esc)
      else if b = QUOTE then .inl (out ++ [(key, val)], [], [], .Closed)
      else .inl (out, key, val ++ [b], .Val)
    | .Esc => .inl (out, key, val ++ [b], .Val)
    | .Closed => if b = BAR then .inl (out, key, val, .Key) else .inr none

/-- What `parse` returns at the end of the input. -/
def pfin : PS → Option (List CL)
  | (out, key, _, .Key) => if out = [] then (if key = [] then some out else none) else none
  | (out, _, _, .Closed) => some out
  | _ => none

def parseM (s : List U8) : Option (List CL) := iterRun pstep pfin s ([], [], [], .Key)

/-! ## Filters as text -/

/-- A key the parser reads back: none of `=`, `'`, `|`, `\`. -/
def PlainKey (k : List U8) : Prop := ∀ b ∈ k, b ≠ EQ ∧ b ≠ QUOTE ∧ b ≠ BAR ∧ b ≠ BSLASH

def clauseText (c : CL) : List U8 := c.1 ++ [EQ, QUOTE] ++ escM c.2 ++ [QUOTE]

/-- `k1='v1'|k2='v2'|...`. -/
def renderM : List CL → List U8
  | [] => []
  | c :: cs => clauseText c ++ cs.flatMap (fun c => BAR :: clauseText c)

/-! ### The round trip -/

theorem go_key (out : List CL) (key val k : List U8) (hk : PlainKey k) :
    iterGo pstep k (out, key, val, .Key) = .inl (out, key ++ k, val, .Key) := by
  induction k generalizing key with
  | nil => simp [iterGo]
  | cons b bs ih =>
    obtain ⟨h1, h2, h3, h4⟩ := hk b (by simp)
    simp only [iterGo, pstep, h1, h2, h3, h4, if_false]
    rw [ih (key ++ [b]) (fun x hx => hk x (by simp [hx]))]; simp

theorem go_escB (out : List CL) (key val : List U8) (b : U8) :
    iterGo pstep (escB b) (out, key, val, .Val) = .inl (out, key, val ++ [b], .Val) := by
  obtain ⟨hq, hb, -, -, -⟩ := bytes_val
  have hqb : QUOTE ≠ BSLASH := by rw [hq, hb]; decide
  unfold escB
  by_cases h1 : b = BSLASH
  · subst h1; simp [iterGo, pstep]
  · by_cases h2 : b = QUOTE
    · subst h2; simp [iterGo, pstep, hqb]
    · simp [iterGo, pstep, h1, h2]

theorem go_val (out : List CL) (key val v : List U8) :
    iterGo pstep (escM v) (out, key, val, .Val) = .inl (out, key, val ++ v, .Val) := by
  induction v generalizing val with
  | nil => simp [escM, iterGo]
  | cons b bs ih =>
    have ih' := ih (val ++ [b])
    simp only [escM] at ih' ⊢
    rw [List.flatMap_cons, iterGo_append, go_escB, Sum.elim_inl, ih']; simp

theorem go_open (out : List CL) (key val : List U8) :
    iterGo pstep [EQ, QUOTE] (out, key, val, .Key) = .inl (out, key, val, .Val) := by
  obtain ⟨hq, -, -, he, -⟩ := bytes_val
  have hqe : QUOTE ≠ EQ := by rw [hq, he]; decide
  simp [iterGo, pstep]

theorem go_close (out : List CL) (key val : List U8) :
    iterGo pstep [QUOTE] (out, key, val, .Val) = .inl (out ++ [(key, val)], [], [], .Closed) := by
  obtain ⟨hq, hb, -, -, -⟩ := bytes_val
  have hqb : QUOTE ≠ BSLASH := by rw [hq, hb]; decide
  simp [iterGo, pstep, hqb]

theorem go_clause (out : List CL) (c : CL) (hk : PlainKey c.1) :
    iterGo pstep (clauseText c) (out, [], [], .Key) = .inl (out ++ [c], [], [], .Closed) := by
  simp only [clauseText, List.append_assoc]
  rw [iterGo_append, go_key _ _ _ _ hk, Sum.elim_inl, iterGo_append, List.nil_append, go_open,
    Sum.elim_inl, iterGo_append, go_val, Sum.elim_inl, go_close]
  simp

theorem go_rest (out : List CL) (cs : List CL) (hk : ∀ c ∈ cs, PlainKey c.1) :
    iterGo pstep (cs.flatMap (fun c => BAR :: clauseText c)) (out, [], [], .Closed) =
      .inl (out ++ cs, [], [], .Closed) := by
  induction cs generalizing out with
  | nil => simp [iterGo]
  | cons c cs ih =>
    simp only [List.flatMap_cons, List.cons_append, iterGo, pstep, if_true]
    rw [iterGo_append, go_clause _ _ (hk c (by simp))]
    simp only [Sum.elim_inl]
    rw [ih _ (fun c' h => hk c' (by simp [h]))]; simp

/-- Parsing a rendered filter gives back its clauses. -/
theorem parse_render (cs : List CL) (hk : ∀ c ∈ cs, PlainKey c.1) : parseM (renderM cs) = some cs := by
  unfold parseM
  rw [iterRun_eq]
  cases cs with
  | nil => rfl
  | cons c cs =>
    simp only [renderM]
    rw [iterGo_append, go_clause _ _ (hk c (by simp))]
    simp only [Sum.elim_inl, List.nil_append]
    rw [go_rest _ _ (fun c' h => hk c' (by simp [h]))]
    rfl

/-! ## Matching records -/

/-- `"owner"` and `"tag"`. -/
def ownerKey : List U8 := [111#u8, 119#u8, 110#u8, 101#u8, 114#u8]
def tagKey : List U8 := [116#u8, 97#u8, 103#u8]

def fieldM (r : Record) (k : List U8) : Option (List U8) :=
  if k = ownerKey then some r.owner.val else if k = tagKey then some r.tag.val else none

/-- A record matches a filter if a clause names one of its fields with its value. -/
def matchesM (cs : List CL) (r : Record) : Bool := cs.any (fun c => decide (fieldM r c.1 = some c.2))

/-! ## Templates -/

/-- A byte of a template value: a literal or the caller's name. -/
inductive Piece where
  | lit (b : U8)
  | hole

/-- A template clause: a key and a value made of pieces. -/
abbrev TClause := List U8 × List Piece

def pieceText : Piece → List U8
  | .lit b => escB b
  | .hole => [HOLE]

def tclauseText (t : TClause) : List U8 := t.1 ++ [EQ, QUOTE] ++ t.2.flatMap pieceText ++ [QUOTE]

/-- A template as an admin writes it, e.g. `owner='$'`. -/
def renderT : List TClause → List U8
  | [] => []
  | t :: ts => tclauseText t ++ ts.flatMap (fun t => BAR :: tclauseText t)

/-- The clause a template clause becomes for the name `nm`. -/
def inst (nm : List U8) (t : TClause) : CL :=
  (t.1, t.2.flatMap (fun p => match p with | .lit b => [b] | .hole => nm))

/-- Keys are plain and have no `$`; literals are not `$` (a `$` is always a hole). -/
def WellFormed (ts : List TClause) : Prop :=
  ∀ t ∈ ts, PlainKey t.1 ∧ (∀ b ∈ t.1, b ≠ HOLE) ∧ ∀ p ∈ t.2, p ≠ .lit HOLE

theorem escM_length (v : List U8) : (escM v).length ≤ 2 * v.length := by
  induction v with
  | nil => simp [escM]
  | cons b bs ih =>
    have : (escB b).length ≤ 2 := by unfold escB; split_ifs <;> simp
    simp only [escM, List.flatMap_cons, List.length_append, List.length_cons] at ih ⊢
    omega

theorem escPreM_length (v : List U8) : (escPreM v).length ≤ 2 * v.length := by
  induction v with
  | nil => simp [escPreM]
  | cons b bs ih =>
    have : (escPreB b).length ≤ 2 := by unfold escPreB; split_ifs <;> simp
    simp only [escPreM, List.flatMap_cons, List.length_append, List.length_cons] at ih ⊢
    omega

theorem substM_append (esc : List U8 → List U8) (nm a b : List U8) :
    substM esc nm (a ++ b) = substM esc nm a ++ substM esc nm b := by
  simp [substM]

theorem substM_plain (esc : List U8 → List U8) (nm l : List U8) (h : ∀ b ∈ l, b ≠ HOLE) :
    substM esc nm l = l := by
  induction l with
  | nil => rfl
  | cons b bs ih =>
    simp only [substM, List.flatMap_cons] at ih ⊢
    rw [if_neg (h b (by simp)), ih (fun x hx => h x (by simp [hx]))]; rfl

theorem special_ne_hole : EQ ≠ HOLE ∧ QUOTE ≠ HOLE ∧ BAR ≠ HOLE ∧ BSLASH ≠ HOLE := by
  obtain ⟨hq, hb, hbar, he, hh⟩ := bytes_val
  rw [hq, hb, hbar, he, hh]; decide

theorem substM_escB (esc : List U8 → List U8) (nm : List U8) (b : U8) (hb : b ≠ HOLE) :
    substM esc nm (escB b) = escB b := by
  apply substM_plain
  obtain ⟨-, h2, -, h4⟩ := special_ne_hole
  unfold escB; split_ifs <;> simp_all

theorem substM_pieces (nm : List U8) (ps : List Piece) (h : ∀ p ∈ ps, p ≠ .lit HOLE) :
    substM escM nm (ps.flatMap pieceText) =
      escM (ps.flatMap (fun p => match p with | .lit b => [b] | .hole => nm)) := by
  induction ps with
  | nil => rfl
  | cons p ps ih =>
    rw [List.flatMap_cons, substM_append, ih (fun q hq => h q (by simp [hq])), List.flatMap_cons]
    simp only [escM, List.flatMap_append]
    congr 1
    cases p with
    | lit b =>
      have hb : b ≠ HOLE := fun e => h (.lit b) (by simp) (by rw [e])
      simp [pieceText, substM_escB _ _ _ hb]
    | hole => simp [pieceText, substM, escM]

theorem substM_tclause (nm : List U8) (t : TClause) (hk : ∀ b ∈ t.1, b ≠ HOLE)
    (hp : ∀ p ∈ t.2, p ≠ .lit HOLE) : substM escM nm (tclauseText t) = clauseText (inst nm t) := by
  obtain ⟨h1, h2, -, -⟩ := special_ne_hole
  simp only [tclauseText, clauseText, inst, substM_append, substM_plain _ _ _ hk, substM_pieces _ _ hp]
  rw [substM_plain _ _ [EQ, QUOTE] (by
      intro b hb; simp only [List.mem_cons, List.not_mem_nil, or_false] at hb
      rcases hb with rfl | rfl <;> assumption),
    substM_plain _ _ [QUOTE] (fun b hb => by rw [List.mem_singleton] at hb; subst hb; exact h2)]

/-- Substituting a name into a rendered template renders the instantiated filter. -/
theorem substM_renderT (nm : List U8) (ts : List TClause) (hw : WellFormed ts) :
    substM escM nm (renderT ts) = renderM (ts.map (inst nm)) := by
  obtain ⟨-, -, h3, -⟩ := special_ne_hole
  cases ts with
  | nil => rfl
  | cons t ts =>
    simp only [renderT, renderM, List.map_cons, substM_append]
    obtain ⟨-, hk, hp⟩ := hw t (by simp)
    rw [substM_tclause _ _ hk hp, List.flatMap_map]
    congr 1
    have : ∀ t' ∈ ts, substM escM nm (BAR :: tclauseText t') = BAR :: clauseText (inst nm t') := by
      intro t' ht'
      obtain ⟨-, hk', hp'⟩ := hw t' (by simp [ht'])
      rw [show BAR :: tclauseText t' = [BAR] ++ tclauseText t' from rfl, substM_append,
        substM_tclause _ _ hk' hp',
        substM_plain _ _ [BAR] (fun b hb => by rw [List.mem_singleton] at hb; subst hb; exact h3)]; rfl
    simp only [substM, List.flatMap_assoc] at this ⊢
    exact List.flatMap_congr this

/-- The round trip: the filter `parse` reads from a substituted template is the
template with the name in its holes, for every name. -/
theorem parse_substitute (nm : List U8) (ts : List TClause) (hw : WellFormed ts) :
    parseM (substM escM nm (renderT ts)) = some (ts.map (inst nm)) := by
  rw [substM_renderT _ _ hw]
  apply parse_render
  intro c hc
  obtain ⟨t, ht, rfl⟩ := List.mem_map.1 hc
  exact (hw t ht).1

end Filters
