import H5iAppLib.Basic
/-!
# Reachability

`Reach edges start` is the set of nodes reachable from `start` along
`edges`: role inheritance, nested groups. `h5i-app-std`'s
`graph::reachable` computes it (`StdSpecs.graph.reachable_spec`), so an
inheritance check is stated over `Reach` and never over the search loop.

The lemmas below are what the loop's proof needs, and what a kernel proof
uses to reason about the result: a set that holds `start` and is closed
under the edges holds everything reachable (`Reach.sub_of_closed`).
-/

namespace H5iAppLib

variable {α : Type}

/-- Nodes reachable from `start` along `edges`, `start` included. -/
inductive Reach (edges : List (α × α)) (start : List α) : α → Prop
  | start {x} : x ∈ start → Reach edges start x
  | step {x y} : Reach edges start x → (x, y) ∈ edges → Reach edges start y

namespace Reach

variable {edges : List (α × α)} {start : List α}

/-- A set that holds `start` and is closed under `edges` holds every
reachable node. -/
theorem sub_of_closed (S : α → Prop) (h0 : ∀ x ∈ start, S x)
    (hc : ∀ x y, S x → (x, y) ∈ edges → S y) {x : α} (h : Reach edges start x) : S x := by
  induction h with
  | start hx => exact h0 _ hx
  | step _ he ih => exact hc _ _ ih he

/-- More edges or more starting nodes reach more. -/
theorem mono {edges' : List (α × α)} {start' : List α} (he : edges ⊆ edges') (hs : start ⊆ start')
    {x : α} (h : Reach edges start x) : Reach edges' start' x := by
  induction h with
  | start hx => exact .start (hs hx)
  | step _ hxy ih => exact .step ih (he hxy)

/-- Reaching through a reachable node. -/
theorem trans {mid : List α} (hm : ∀ y ∈ mid, Reach edges start y) {x : α} (h : Reach edges mid x) :
    Reach edges start x := by
  induction h with
  | start hx => exact hm _ hx
  | step _ hxy ih => exact .step ih hxy

/-- Every reachable node is a start node or the target of an edge. -/
theorem mem_nodes {x : α} (h : Reach edges start x) : x ∈ start ∨ ∃ e ∈ edges, e.2 = x := by
  cases h with
  | start hx => exact .inl hx
  | step _ he => exact .inr ⟨_, he, rfl⟩

end Reach

/-! ## The search loop

`graph::reachable` keeps `out`, which is both the visited set and the queue.
Visiting `x` scans the edges and appends each new target of an edge from
`x`: `visitStep`. -/

section Visit
variable [DecidableEq α]

/-- One step of the scan from `x`: append the target of an edge from `x`
unless it is already there. -/
def visitStep (x : α) (o : List α) (e : α × α) : List α :=
  if e.1 = x ∧ e.2 ∉ o then o ++ [e.2] else o

/-- One step of the dedup of `start`. -/
def insertStep (o : List α) (y : α) : List α := if y ∈ o then o else o ++ [y]

theorem prefix_foldl_visitStep (x : α) (es : List (α × α)) (o : List α) : o <+: es.foldl (visitStep x) o := by
  induction es generalizing o with
  | nil => exact List.prefix_refl _
  | cons e es ih =>
    rw [List.foldl_cons]
    refine List.IsPrefix.trans ?_ (ih _)
    unfold visitStep; split
    · exact List.prefix_append _ _
    · exact List.prefix_refl _

theorem mem_foldl_visitStep (x : α) (es : List (α × α)) (o : List α) (y : α) :
    y ∈ es.foldl (visitStep x) o ↔ y ∈ o ∨ ∃ e ∈ es, e.1 = x ∧ e.2 = y := by
  induction es generalizing o with
  | nil => simp
  | cons e es ih =>
    rw [List.foldl_cons, ih]
    unfold visitStep
    split
    · rename_i h
      simp only [List.mem_append, List.mem_cons, List.not_mem_nil, or_false, exists_eq_or_imp]
      constructor
      · rintro ((hy | rfl) | ⟨e', he', h1, h2⟩)
        · exact .inl hy
        · exact .inr (.inl ⟨h.1, rfl⟩)
        · exact .inr (.inr ⟨e', he', h1, h2⟩)
      · rintro (hy | (⟨h1, h2⟩ | ⟨e', he', h1, h2⟩))
        · exact .inl (.inl hy)
        · exact .inl (.inr h2.symm)
        · exact .inr ⟨e', he', h1, h2⟩
    · rename_i h
      simp only [List.mem_cons, exists_eq_or_imp]
      constructor
      · rintro (hy | ⟨e', he', h1, h2⟩)
        · exact .inl hy
        · exact .inr (.inr ⟨e', he', h1, h2⟩)
      · rintro (hy | (⟨h1, h2⟩ | ⟨e', he', h1, h2⟩))
        · exact .inl hy
        · exact .inl (by subst h2; by_contra hn; exact h ⟨h1, hn⟩)
        · exact .inr ⟨e', he', h1, h2⟩

theorem nodup_foldl_visitStep (x : α) (es : List (α × α)) (o : List α) (h : o.Nodup) :
    (es.foldl (visitStep x) o).Nodup := by
  induction es generalizing o with
  | nil => exact h
  | cons e es ih =>
    rw [List.foldl_cons]; apply ih
    unfold visitStep; split
    · rename_i hc; exact List.Nodup.append h (List.nodup_singleton _) (by simpa using hc.2)
    · exact h

theorem mem_foldl_insertStep (l o : List α) (y : α) : y ∈ l.foldl insertStep o ↔ y ∈ o ∨ y ∈ l := by
  induction l generalizing o with
  | nil => simp
  | cons z zs ih =>
    rw [List.foldl_cons, ih]; unfold insertStep
    split
    · rename_i h; simp only [List.mem_cons]
      constructor
      · rintro (hy | hy); exact .inl hy; exact .inr (.inr hy)
      · rintro (hy | rfl | hy); exact .inl hy; exact .inl h; exact .inr hy
    · simp only [List.mem_append, List.mem_cons, List.not_mem_nil, or_false]
      constructor
      · rintro ((hy | rfl) | hy); exact .inl hy; exact .inr (.inl rfl); exact .inr (.inr hy)
      · rintro (hy | rfl | hy); exact .inl (.inl hy); exact .inl (.inr rfl); exact .inr hy

theorem nodup_foldl_insertStep (l o : List α) (h : o.Nodup) : (l.foldl insertStep o).Nodup := by
  induction l generalizing o with
  | nil => exact h
  | cons z zs ih =>
    rw [List.foldl_cons]; apply ih
    unfold insertStep; split
    · exact h
    · rename_i hc; exact List.Nodup.append h (List.nodup_singleton _) (by simpa using hc)

end Visit

end H5iAppLib
