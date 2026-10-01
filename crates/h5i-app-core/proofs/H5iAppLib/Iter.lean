import H5iAppLib.Loops
/-!
# Specs for `for x in s.iter()` loops

Aeneas turns `for x in s.iter() { ... }` (also on a `Vec`, through `deref`)
into `loop body it` over a slice iterator `⟨s, i⟩`. The Rust code needs no
index and no bound. Each spec turns a per-element fact about `body` into a
statement about the whole list:

- `iter_loop`: an accumulator and an early exit, modeled by `iterRun`;
- `iter_fold`: runs to the end, modeled by `List.foldl`;
- `iter_search`: stops at the first match, modeled by `searchFrom`;
- `iter_filter_map`, `iter_any`, `iter_find`: the usual loops, stated on lists.

`h5i_iter` closes the per-element goal of each.
-/
open Aeneas Aeneas.Std Result

namespace H5iAppLib

abbrev SIter := core.slice.iter.Iter

@[step] theorem slice_iter_spec {α} (s : Slice α) :
    core.slice.Slice.iter s ⦃ it => it = ⟨s, 0⟩ ⦄ := by
  simp [core.slice.Slice.iter]

theorem vec_deref_val {α} (v : alloc.vec.Vec α) : (alloc.vec.Vec.deref v).val = v.val := by
  simp [alloc.vec.Vec.deref]

/-- `next` on `⟨s, i⟩`, bounded by `s.val.length`. Aeneas bounds it by
`s.len`, which `simp` cannot rewrite under `s[i]`. -/
theorem slice_iter_next {α} (s : Slice α) (i : Nat) :
    core.slice.iter.IteratorSliceIter.next ⟨s, i⟩ =
      if h : i < s.val.length then ok (some s.val[i], ⟨s, i + 1⟩) else ok (none, ⟨s, i⟩) := rfl

/-! ## Loops with an accumulator and an early exit -/

/-- Run `f` over `l` from state `b`: `.inl` continues with a new state, `.inr`
stops with a result, and `fin` reads the state at the end. -/
def iterRun {α β δ} (f : β → α → β ⊕ δ) (fin : β → δ) : List α → β → δ
  | [], b => fin b
  | x :: xs, b => match f b x with
    | .inl b' => iterRun f fin xs b'
    | .inr d => d

/-- `iterRun` without `fin`: `.inl` if the run reaches the end, `.inr` if it stops. -/
def iterGo {α β δ} (f : β → α → β ⊕ δ) : List α → β → β ⊕ δ
  | [], b => .inl b
  | x :: xs, b => match f b x with
    | .inl b' => iterGo f xs b'
    | .inr d => .inr d

theorem iterRun_eq {α β δ} (f : β → α → β ⊕ δ) (fin : β → δ) (l : List α) (b : β) :
    iterRun f fin l b = (iterGo f l b).elim fin id := by
  induction l generalizing b with
  | nil => rfl
  | cons x xs ih => simp only [iterRun, iterGo]; split <;> simp_all

/-- Runs compose: a run over `l₁ ++ l₂` is a run over `l₁`, then over `l₂`. -/
theorem iterGo_append {α β δ} (f : β → α → β ⊕ δ) (l₁ l₂ : List α) (b : β) :
    iterGo f (l₁ ++ l₂) b = (iterGo f l₁ b).elim (iterGo f l₂) .inr := by
  induction l₁ generalizing b with
  | nil => rfl
  | cons x xs ih => simp only [List.cons_append, iterGo]; split <;> simp_all

/-- Per-element shape of `body` at element `i` with accumulator `acc`: `absS`
reads the accumulator and `absY` the result as model values, `Inv` carries
bounds. -/
def IterStep {α σ γ β δ} (s : Slice α) (absS : σ → β) (absY : γ → δ) (f : β → α → β ⊕ δ)
    (fin : β → δ) (Inv : σ → Nat → Prop) (i : Nat) (acc : σ) : ControlFlow (SIter α × σ) γ → Prop
  | .done y => (∃ h : i < s.val.length, f (absS acc) s[i] = .inr (absY y)) ∨
      (s.val.length ≤ i ∧ absY y = fin (absS acc))
  | .cont (it, acc') => ∃ h : i < s.val.length,
      it = ⟨s, i + 1⟩ ∧ f (absS acc) s[i] = .inl (absS acc') ∧ Inv acc' (i + 1)

theorem iter_loop {α σ γ β δ : Type} (s : Slice α) (absS : σ → β) (absY : γ → δ)
    (f : β → α → β ⊕ δ) (fin : β → δ) (Inv : σ → Nat → Prop)
    (body : SIter α × σ → Result (ControlFlow (SIter α × σ) γ))
    (hstep : ∀ i (acc : σ), i ≤ s.val.length → Inv acc i →
      body (⟨s, i⟩, acc) ⦃ IterStep s absS absY f fin Inv i acc ⦄)
    (i : Nat) (acc : σ) (hi : i ≤ s.val.length) (hinv : Inv acc i) :
    loop body (⟨s, i⟩, acc) ⦃ y => absY y = iterRun f fin (s.val.drop i) (absS acc) ⦄ := by
  apply loop.spec_decr_nat (measure := fun (x : SIter α × σ) => s.val.length - x.1.i)
    (inv := fun x => x.1.slice = s ∧ x.1.i ≤ s.val.length ∧ Inv x.2 x.1.i ∧
      iterRun f fin (s.val.drop i) (absS acc) = iterRun f fin (s.val.drop x.1.i) (absS x.2))
  · rintro ⟨⟨s', j⟩, t⟩ ⟨hs, hj, hI, heq⟩
    simp only at hs hj hI heq; subst hs
    apply WP.spec_mono (hstep j t hj hI)
    intro r h
    cases r with
    | done y =>
      simp only
      rcases h with ⟨hlt, hf⟩ | ⟨hle, hy⟩
      · rw [heq, List.drop_eq_getElem_cons hlt, iterRun]
        rw [← Slice.getElem_Nat_eq _ _ hlt, hf]
      · rw [heq, List.drop_eq_nil_of_le hle, iterRun, hy]
    | cont x =>
      obtain ⟨it, t'⟩ := x
      obtain ⟨hlt, rfl, hf, hI'⟩ := h
      simp only
      refine ⟨⟨by simp, by omega, hI', ?_⟩, by omega⟩
      rw [heq, List.drop_eq_getElem_cons hlt, iterRun]
      rw [← Slice.getElem_Nat_eq _ _ hlt, hf]
  · exact ⟨rfl, hi, hinv, rfl⟩

/-! ## Loops that run to the end -/

/-- Per-element shape of a loop that runs to the end: `g` is the model step. -/
def IterFoldStep {α σ β} (s : Slice α) (abs : σ → β) (g : β → α → β) (Inv : σ → Nat → Prop)
    (i : Nat) (acc : σ) : ControlFlow (SIter α × σ) σ → Prop
  | .done acc' => s.val.length ≤ i ∧ abs acc' = abs acc
  | .cont (it, acc') => ∃ h : i < s.val.length,
      it = ⟨s, i + 1⟩ ∧ abs acc' = g (abs acc) s[i] ∧ Inv acc' (i + 1)

theorem iterRun_foldl {α β} (g : β → α → β) (l : List α) (b : β) :
    iterRun (δ := β) (fun b x => .inl (g b x)) id l b = l.foldl g b := by
  induction l generalizing b with
  | nil => rfl
  | cons x xs ih => exact ih _

theorem iter_fold {α σ β : Type} (s : Slice α) (abs : σ → β) (g : β → α → β)
    (Inv : σ → Nat → Prop)
    (body : SIter α × σ → Result (ControlFlow (SIter α × σ) σ))
    (hstep : ∀ i (acc : σ), i ≤ s.val.length → Inv acc i →
      body (⟨s, i⟩, acc) ⦃ IterFoldStep s abs g Inv i acc ⦄)
    (i : Nat) (acc : σ) (hi : i ≤ s.val.length) (hinv : Inv acc i) :
    loop body (⟨s, i⟩, acc) ⦃ acc' => abs acc' = (s.val.drop i).foldl g (abs acc) ⦄ := by
  have h := iter_loop s abs abs (fun b x => .inl (g b x)) id Inv body (fun j t hj hI => by
    apply WP.spec_mono (hstep j t hj hI)
    intro r hr
    cases r with
    | done y => exact .inr hr
    | cont x =>
      obtain ⟨hlt, hit, habs, hI'⟩ := hr
      exact ⟨hlt, hit, by rw [habs], hI'⟩) i acc hi hinv
  apply WP.spec_mono h
  intro y hy; rw [hy, iterRun_foldl]

/-! ## Loops that stop at the first match -/

/-- Per-element shape of a loop over `s` that may stop early, at element `i`. -/
def IterSearchStep {α γ δ} (s : Slice α) (P : α → Bool) (abs : γ → δ) (found : Nat → α → δ)
    (missing : δ) (i : Nat) : ControlFlow (SIter α) γ → Prop
  | .done y => (∃ h : i < s.val.length, P s[i] ∧ abs y = found i s[i]) ∨
      (s.val.length ≤ i ∧ abs y = missing)
  | .cont it => ∃ h : i < s.val.length, ¬ P s[i] ∧ it = ⟨s, i + 1⟩

theorem iter_search {α γ δ : Type} (s : Slice α) (P : α → Bool) (abs : γ → δ)
    (found : Nat → α → δ) (missing : δ)
    (body : SIter α → Result (ControlFlow (SIter α) γ))
    (hstep : ∀ i, i ≤ s.val.length → body ⟨s, i⟩ ⦃ IterSearchStep s P abs found missing i ⦄)
    (i : Nat) (hi : i ≤ s.val.length) :
    loop body ⟨s, i⟩ ⦃ y => abs y = searchFrom s.val P found missing i ⦄ := by
  apply loop.spec_decr_nat (measure := fun (j : SIter α) => s.val.length - j.i)
    (inv := fun j => j.slice = s ∧ j.i ≤ s.val.length ∧
      searchFrom s.val P found missing i = searchFrom s.val P found missing j.i)
  · rintro ⟨s', j⟩ ⟨hs, hj, heq⟩
    simp only at hs hj heq; subst hs
    apply WP.spec_mono (hstep j hj)
    intro r h
    cases r with
    | done y =>
      rcases h with ⟨hlt, hp, hy⟩ | ⟨hle, hy⟩
      · simp only; rw [hy, heq, searchFrom_found hlt hp]; rfl
      · simp only; rw [hy, heq, searchFrom_end hle]
    | cont k =>
      obtain ⟨hlt, hp, rfl⟩ := h
      refine ⟨⟨by simp, by simp only; omega, ?_⟩, by simp only; omega⟩
      rw [heq, searchFrom_skip hlt hp]
  · exact ⟨rfl, hi, rfl⟩

/-! ## The usual loops, stated on lists -/

/-- `for x in s.iter() { if P(x) { out.push(f(x)) } }`, from an empty `out`. -/
theorem iter_filter_map {α β : Type} (s : Slice α) (P : α → Bool) (f : α → β)
    (body : SIter α × alloc.vec.Vec β → Result (ControlFlow (SIter α × alloc.vec.Vec β) (alloc.vec.Vec β)))
    (hstep : ∀ i (acc : alloc.vec.Vec β), i ≤ s.val.length → acc.val.length ≤ i →
      body (⟨s, i⟩, acc) ⦃ IterFoldStep s (fun o => o.val) (fun o x => if P x then o ++ [f x] else o)
        (fun o i => o.val.length ≤ i) i acc ⦄) :
    loop body (⟨s, 0⟩, alloc.vec.Vec.new β) ⦃ out => out.val = (s.val.filter P).map f ⦄ := by
  apply WP.spec_mono (iter_fold s (fun o => o.val) (fun o x => if P x then o ++ [f x] else o)
    (fun o i => o.val.length ≤ i) body hstep 0 _ (by simp) (by simp))
  intro out h
  rw [h, foldl_filter_map]; simp

/-- `for x in s.iter() { if P(x) { return b } } c`. -/
theorem iter_any {α γ : Type} (s : Slice α) (P : α → Bool) (b c : γ)
    (body : SIter α → Result (ControlFlow (SIter α) γ))
    (hstep : ∀ i, i ≤ s.val.length → body ⟨s, i⟩ ⦃ IterSearchStep s P id (fun _ _ => b) c i ⦄) :
    loop body ⟨s, 0⟩ ⦃ y => y = if s.val.any P then b else c ⦄ := by
  apply WP.spec_mono (iter_search s P id (fun _ _ => b) c body hstep 0 (by simp))
  intro y h
  rw [id_eq] at h; rw [h, searchFrom_const, List.drop_zero]

/-- `for x in s.iter() { if P(x) { return Some(x) } } None`. -/
theorem iter_find {α : Type} (s : Slice α) (P : α → Bool)
    (body : SIter α → Result (ControlFlow (SIter α) (Option α)))
    (hstep : ∀ i, i ≤ s.val.length → body ⟨s, i⟩ ⦃ IterSearchStep s P id (fun _ x => some x) none i ⦄) :
    loop body ⟨s, 0⟩ ⦃ y => y = s.val.find? P ⦄ := by
  apply WP.spec_mono (iter_search s P id (fun _ x => some x) none body hstep 0 (by simp))
  intro y h
  rw [id_eq] at h
  rw [h, show (fun (_ : Nat) (x : α) => some x) = (fun _ x => some (_root_.id x)) from rfl,
    searchFrom_find, List.drop_zero]; simp

/-- Close the per-element goal of the specs above. -/
macro "h5i_iter" : tactic => `(tactic| (
  rw [H5iAppLib.slice_iter_next]
  split <;> step* <;> (repeat' (first | step | split)) <;>
    (try simp only [H5iAppLib.IterStep, H5iAppLib.IterFoldStep, H5iAppLib.IterSearchStep]) <;>
    (try simp_all) <;> try (first | scalar_tac | omega)))

/-- `h5i_iter` with extra simp lemmas, such as the definition of the model step. -/
macro "h5i_iter" " [" ls:Lean.Parser.Tactic.simpLemma,* "]" : tactic => `(tactic| (
  rw [H5iAppLib.slice_iter_next]
  split <;> step* <;> (repeat' (first | step | split)) <;>
    (try simp only [H5iAppLib.IterStep, H5iAppLib.IterFoldStep, H5iAppLib.IterSearchStep]) <;>
    (try simp_all [$ls,*]) <;> try (first | scalar_tac | omega)))

theorem ite_true_false (b : Bool) : (if b = true then true else false) = b := by cases b <;> rfl

/-- `h5i_for f using (spec) [lemmas] [closing]`: prove a spec for `f`, whose
body is one `for` loop `f_loop`, from `spec`, an `iter_*` lemma with `?_` for
its per-element premise, e.g. `(iter_find _ P _ ?_)`. Unfolds `f`, closes the
per-element goal with `h5i_iter [lemmas]` and the spec's conclusion with
`simp_all [closing]` (default: `lemmas`). Per-element goals it cannot close are
left to the caller. -/
macro "h5i_for " f:ident " using " e:term:max " [" ls:Lean.Parser.Tactic.simpLemma,* "]"
    " [" cs:Lean.Parser.Tactic.simpLemma,* "]" : tactic => do
  let loopId := Lean.mkIdent (f.getId.appendAfter "_loop")
  let bodyId := Lean.mkIdent (loopId.getId ++ `body)
  `(tactic| (
    unfold $f:ident $loopId:ident
    step*
    try subst_vars
    apply WP.spec_mono $e
    · intro y hy
      first
        | exact hy | exact hy.symm
        | (subst hy; simp only [H5iAppLib.ite_true_false, $cs,*]; done)
        | (subst hy; simp only [H5iAppLib.ite_true_false, $cs,*]; rfl)
        | (subst hy; simp_all [$cs,*]; try rfl)
        | (simp_all [$cs,*]; done)
        | (simp_all [$cs,*]; try rfl)
    try simp only [Prod.forall]
    intros
    unfold $bodyId:ident
    h5i_iter [$ls,*]))

macro "h5i_for " f:ident " using " e:term:max " [" ls:Lean.Parser.Tactic.simpLemma,* "]" : tactic =>
  `(tactic| h5i_for $f using $e [$ls,*] [$ls,*])

macro "h5i_for " f:ident " using " e:term:max : tactic => `(tactic| h5i_for $f using $e [] [])

end H5iAppLib
