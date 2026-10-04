import H5iAppLib.Loops
import H5iAppLib.Bytes
/-!
# Vectors as sets and maps

`h5i-app-std`'s `set` and `map` functions are generic over `T: PartialEq`,
so their extracted Lean takes the `PartialEq` instance as an argument.
`EqLaw inst` says that instance decides equality; with it, each spec in
`StdSpecs.lean` states the result over lists: membership, `⊆`, and
`List.lookup`. Instances below cover the scalars and `Vec<u8>`;
`h5i_derive_eq` adds one for each kernel type it runs on.
-/
open Aeneas Aeneas.Std Result

namespace H5iAppLib

/-- The extracted `PartialEq` instance `inst` decides equality on `T`. -/
class EqLaw {T : Type} [DecidableEq T] (inst : core.cmp.PartialEq T T) : Prop where
  eq_ok : ∀ a b, inst.eq a b = ok (decide (a = b))
  ne_ok : ∀ a b, inst.ne a b = ok (!decide (a = b))

theorem EqLaw.of_eq {T : Type} [DecidableEq T] (inst : core.cmp.PartialEq T T)
    (h : ∀ a b, inst.eq a b = ok (decide (a = b)))
    (hne : inst.ne = core.cmp.PartialEq.ne.default inst.eq) : EqLaw inst where
  eq_ok := h
  ne_ok := fun a b => by rw [hne]; simp [core.cmp.PartialEq.ne.default, h]

@[step] theorem EqLaw.eq_spec {T : Type} [DecidableEq T] (inst : core.cmp.PartialEq T T) [EqLaw inst]
    (a b : T) : inst.eq a b ⦃ r => r = decide (a = b) ⦄ := by
  rw [EqLaw.eq_ok]; simp

@[step] theorem EqLaw.ne_spec {T : Type} [DecidableEq T] (inst : core.cmp.PartialEq T T) [EqLaw inst]
    (a b : T) : inst.ne a b ⦃ r => r = !decide (a = b) ⦄ := by
  rw [EqLaw.ne_ok]; simp

instance : EqLaw core.cmp.PartialEqU8 where
  eq_ok a b := by by_cases h : a = b <;> simp [h]
  ne_ok a b := by by_cases h : a = b <;> simp [h]

instance : EqLaw core.cmp.PartialEqU32 where
  eq_ok a b := by by_cases h : a = b <;> simp [h]
  ne_ok a b := by by_cases h : a = b <;> simp [h]

instance : EqLaw core.cmp.PartialEqU64 where
  eq_ok a b := by by_cases h : a = b <;> simp [h]
  ne_ok a b := by by_cases h : a = b <;> simp [h]

instance : EqLaw core.cmp.PartialEqUsize where
  eq_ok a b := by by_cases h : a = b <;> simp [h]
  ne_ok a b := by by_cases h : a = b <;> simp [h]

instance : EqLaw core.cmp.PartialEqI64 where
  eq_ok a b := by by_cases h : a = b <;> simp [h]
  ne_ok a b := by by_cases h : a = b <;> simp [h]

instance : EqLaw (core.cmp.PartialEqVec core.cmp.PartialEqU8) where
  eq_ok a b := eq_ok_of_spec (vec_u8_eq_spec a b)
  ne_ok a b := eq_ok_of_spec (vec_u8_ne_spec a b)

/-- The extracted `Clone` instance `inst` returns its argument. -/
class CloneLaw {T : Type} (inst : core.clone.Clone T) : Prop where
  clone_ok : ∀ x, inst.clone x = ok x

@[step] theorem CloneLaw.clone_spec {T : Type} (inst : core.clone.Clone T) [CloneLaw inst] (x : T) :
    inst.clone x ⦃ y => y = x ⦄ := by
  rw [CloneLaw.clone_ok]; simp

@[step] theorem CloneLaw.vec_clone_spec {T : Type} (inst : core.clone.Clone T) [CloneLaw inst]
    (v : alloc.vec.Vec T) : alloc.vec.CloneVec.clone inst v ⦃ w => w = v ⦄ := by
  rw [vec_clone_ok inst v CloneLaw.clone_ok]; simp

instance : CloneLaw core.clone.CloneU8 := ⟨fun _ => rfl⟩
instance : CloneLaw core.clone.CloneU32 := ⟨fun _ => rfl⟩
instance : CloneLaw core.clone.CloneU64 := ⟨fun _ => rfl⟩
instance : CloneLaw core.clone.CloneUsize := ⟨fun _ => rfl⟩
instance : CloneLaw core.clone.CloneI64 := ⟨fun _ => rfl⟩
instance : CloneLaw core.clone.CloneBool := ⟨fun _ => rfl⟩
instance (T : Type) : CloneLaw (BuiltinClone T) := ⟨fun _ => rfl⟩

instance {T : Type} (inst : core.clone.Clone T) [CloneLaw inst] : CloneLaw (core.clone.CloneallocvecVec inst) :=
  ⟨fun v => vec_clone_ok inst v CloneLaw.clone_ok⟩

/-! ## Sets

A list stands for the set of its elements: `x ∈ l`, `a ⊆ b`. -/

section Sets
variable {α : Type}

theorem any_eq_iff [DecidableEq α] (l : List α) (x : α) :
    l.any (fun y => decide (y = x)) = decide (x ∈ l) := by
  rw [Bool.eq_iff_iff]; simp

theorem subset_iff_all (a b : List α) : a ⊆ b ↔ ∀ x ∈ a, x ∈ b := Iff.rfl

/-- Equal as sets. -/
def SetEq (a b : List α) : Prop := a ⊆ b ∧ b ⊆ a

instance [DecidableEq α] (a b : List α) : Decidable (SetEq a b) := by unfold SetEq; infer_instance

theorem SetEq.mem_iff {a b : List α} (h : SetEq a b) (x : α) : x ∈ a ↔ x ∈ b := ⟨(h.1 ·), (h.2 ·)⟩

theorem SetEq.refl (a : List α) : SetEq a a := ⟨List.Subset.refl a, List.Subset.refl a⟩

theorem SetEq.symm {a b : List α} (h : SetEq a b) : SetEq b a := ⟨h.2, h.1⟩

theorem SetEq.trans {a b c : List α} (h₁ : SetEq a b) (h₂ : SetEq b c) : SetEq a c :=
  ⟨h₁.1.trans h₂.1, h₂.2.trans h₁.2⟩

theorem mem_insert_iff [DecidableEq α] (l : List α) (x y : α) :
    y ∈ (if x ∈ l then l else l ++ [x]) ↔ y = x ∨ y ∈ l := by
  split
  · constructor
    · exact Or.inr
    · rintro (rfl | h) <;> assumption
  · simp [or_comm]

theorem mem_remove_iff [DecidableEq α] (l : List α) (x y : α) :
    y ∈ l.filter (fun z => decide (z ≠ x)) ↔ y ∈ l ∧ y ≠ x := by
  simp

end Sets

/-! ## Maps

A list of pairs stands for a map; the first pair with a key wins.
`mapGet` reads it with `DecidableEq`, not `List.lookup`'s `BEq`: Aeneas
scalars have their own `BEq` instance, and a model that picked one up would
not match a statement that picked up the other. -/

section Maps
variable {κ ν : Type} [DecidableEq κ]

/-- `m.get(k)`: the value of the first entry with key `k`. -/
def mapGet (k : κ) : List (κ × ν) → Option ν
  | [] => none
  | (a, b) :: es => if a = k then some b else mapGet k es

/-- `m.insert(k, v)`: replace the first entry with key `k`, or append one. -/
def mapInsert (k : κ) (v : ν) (m : List (κ × ν)) : List (κ × ν) :=
  upsertBy (fun e => decide (e.1 = k)) (k, v) m

/-- `m.remove(k)`. -/
def mapRemove (k : κ) (m : List (κ × ν)) : List (κ × ν) := m.filter (fun e => decide (e.1 ≠ k))

@[simp] theorem mapGet_nil (k : κ) : mapGet k ([] : List (κ × ν)) = none := rfl

theorem mapGet_cons (k a : κ) (b : ν) (es : List (κ × ν)) :
    mapGet k ((a, b) :: es) = if a = k then some b else mapGet k es := rfl

theorem mapGet_eq_find (m : List (κ × ν)) (k : κ) :
    mapGet k m = (m.find? (fun e => decide (e.1 = k))).map (·.2) := by
  induction m with
  | nil => rfl
  | cons e es ih =>
    obtain ⟨a, b⟩ := e
    rw [mapGet_cons, List.find?_cons]
    by_cases h : a = k <;> simp [h, ih]

@[simp] theorem mapGet_mapInsert (k k' : κ) (v : ν) (m : List (κ × ν)) :
    mapGet k' (mapInsert k v m) = if k' = k then some v else mapGet k' m := by
  unfold mapInsert
  induction m with
  | nil =>
    by_cases h : k' = k
    · subst h; simp [upsertBy, mapGet_cons]
    · simp [upsertBy, mapGet_cons, h, Ne.symm h]
  | cons e es ih =>
    obtain ⟨a, b⟩ := e
    by_cases ha : a = k
    · subst ha
      by_cases h : k' = a
      · subst h; simp [upsertBy, mapGet_cons]
      · simp [upsertBy, mapGet_cons, h, Ne.symm h]
    · simp only [upsertBy, ha, decide_false, Bool.false_eq_true, if_false, mapGet_cons, ih]
      by_cases h : k' = a
      · subst h; simp [ha]
      · simp [Ne.symm h]

@[simp] theorem mapGet_mapRemove (k k' : κ) (m : List (κ × ν)) :
    mapGet k' (mapRemove k m) = if k' = k then none else mapGet k' m := by
  unfold mapRemove
  induction m with
  | nil => simp
  | cons e es ih =>
    obtain ⟨a, b⟩ := e
    rw [List.filter_cons]
    by_cases ha : a = k
    · subst ha
      simp only [ne_eq, not_true_eq_false, decide_false, Bool.false_eq_true, if_false, ih]
      by_cases h : k' = a
      · simp [h]
      · simp [h, mapGet_cons, Ne.symm h]
    · simp only [ne_eq, ha, not_false_eq_true, decide_true, if_true, mapGet_cons, ih]
      by_cases h : k' = a
      · subst h; simp [ha]
      · simp [Ne.symm h]

theorem mapGet_isSome_iff (m : List (κ × ν)) (k : κ) : (mapGet k m).isSome ↔ ∃ v, (k, v) ∈ m := by
  induction m with
  | nil => simp
  | cons e es ih =>
    obtain ⟨a, b⟩ := e
    by_cases h : a = k
    · subst h; simp [mapGet_cons]
    · simp [mapGet_cons, h, ih, Ne.symm h]

theorem mem_of_mapGet {m : List (κ × ν)} {k : κ} {v : ν} (h : mapGet k m = some v) : (k, v) ∈ m := by
  induction m with
  | nil => simp at h
  | cons e es ih =>
    obtain ⟨a, b⟩ := e
    by_cases ha : a = k
    · subst ha; simp [mapGet_cons] at h; simp [h]
    · simp [mapGet_cons, ha] at h; exact List.mem_cons_of_mem _ (ih h)

end Maps

end H5iAppLib
