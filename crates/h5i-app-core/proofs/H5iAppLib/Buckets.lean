import H5iAppLib.Sets
/-!
# Buckets as maps

`h5i-app-std`'s `HashMap` keeps its entries in buckets, a `List (List (κ × ν))`
here, and puts the entry for key `k` in bucket `slot k`. With
`BucketsInv slot B`, every lookup reads the flattened buckets as a map (the
model of `H5iAppLib.Sets`), and replacing one bucket changes that map only
at the keys of its slot. `slot` is any function: the specs never need it to
spread keys out, only to be the function the code computes.
-/
open Aeneas Aeneas.Std Result

namespace H5iAppLib

section Lists
variable {α : Type}

theorem flatten_split (B : List (List α)) (s : Nat) (hs : s < B.length) :
    B.flatten = (B.take s).flatten ++ B[s] ++ (B.drop (s + 1)).flatten := by
  have := congrArg List.flatten (List.take_append_drop s B)
  rw [List.drop_eq_getElem_cons hs] at this
  conv_lhs => rw [← this]
  simp only [List.flatten_append, List.flatten_cons, List.append_assoc]

theorem flatten_set (B : List (List α)) (s : Nat) (hs : s < B.length) (x : List α) :
    (B.set s x).flatten = (B.take s).flatten ++ x ++ (B.drop (s + 1)).flatten := by
  rw [List.set_eq_take_append_cons_drop, if_pos hs]; simp

theorem mem_flatten_take {B : List (List α)} {s : Nat} {e : α} (h : e ∈ (B.take s).flatten) :
    ∃ j, ∃ hj : j < B.length, j < s ∧ e ∈ B[j] := by
  obtain ⟨l, hl, he⟩ := List.mem_flatten.1 h
  obtain ⟨j, hj, rfl⟩ := List.getElem_of_mem hl
  simp only [List.length_take] at hj
  exact ⟨j, by omega, by omega, by simpa using he⟩

theorem mem_flatten_drop {B : List (List α)} {s : Nat} {e : α} (h : e ∈ (B.drop (s + 1)).flatten) :
    ∃ j, ∃ hj : j < B.length, s < j ∧ e ∈ B[j] := by
  obtain ⟨l, hl, he⟩ := List.mem_flatten.1 h
  obtain ⟨j, hj, rfl⟩ := List.getElem_of_mem hl
  simp only [List.length_drop] at hj
  exact ⟨s + 1 + j, by omega, by omega, by simpa using he⟩

theorem length_flatten_set (B : List (List α)) (s : Nat) (hs : s < B.length) (x : List α) :
    (B.set s x).flatten.length + B[s].length = B.flatten.length + x.length := by
  rw [flatten_set B s hs, flatten_split B s hs]; simp; omega

end Lists

section Maps
variable {κ ν : Type} [DecidableEq κ]

theorem mapGet_append (q : κ) (a b : List (κ × ν)) :
    mapGet q (a ++ b) = (mapGet q a).or (mapGet q b) := by
  induction a with
  | nil => simp
  | cons e es ih => obtain ⟨x, y⟩ := e; by_cases h : x = q <;> simp [mapGet_cons, h, ih]

theorem mapGet_eq_none {q : κ} {a : List (κ × ν)} (h : ∀ e ∈ a, e.1 ≠ q) : mapGet q a = none := by
  induction a with
  | nil => rfl
  | cons e es ih =>
    obtain ⟨x, y⟩ := e
    rw [mapGet_cons, if_neg (h _ List.mem_cons_self), ih (fun e he => h e (List.mem_cons_of_mem _ he))]

/-- The first entry with key `k` is at `i`: `mapGet` reads it, `mapInsert` replaces it. -/
theorem mapGet_of_findIdx {l : List (κ × ν)} {k : κ} {i : Nat}
    (h : l.findIdx? (fun e => decide (e.1 = k)) = some i) :
    ∃ hi : i < l.length, l[i].1 = k ∧ mapGet k l = some l[i].2 ∧
      ∀ v, mapInsert k v l = l.set i (k, v) := by
  induction l generalizing i with
  | nil => simp at h
  | cons e es ih =>
    obtain ⟨a, b⟩ := e
    rw [List.findIdx?_cons] at h
    by_cases ha : a = k
    · subst ha; simp at h; subst h
      exact ⟨by simp, rfl, by simp [mapGet_cons], fun v => by simp [mapInsert, upsertBy]⟩
    · simp [ha] at h
      obtain ⟨j, hj, rfl⟩ := h
      obtain ⟨hl, hk, hg, hu⟩ := ih hj
      refine ⟨by simp; omega, by simpa using hk, by simpa [mapGet_cons, ha] using hg, fun v => ?_⟩
      have := hu v; simp only [mapInsert] at this ⊢
      simp [upsertBy, ha, this]

/-- No entry has key `k`: `mapGet` misses, `mapInsert` appends. -/
theorem mapGet_of_findIdx_none {l : List (κ × ν)} {k : κ}
    (h : l.findIdx? (fun e => decide (e.1 = k)) = none) :
    mapGet k l = none ∧ ∀ v, mapInsert k v l = l ++ [(k, v)] := by
  induction l with
  | nil => simp [mapInsert, upsertBy]
  | cons e es ih =>
    obtain ⟨a, b⟩ := e
    rw [List.findIdx?_cons] at h
    by_cases ha : a = k
    · simp [ha] at h
    · obtain ⟨hg, hu⟩ := ih (by simpa [ha] using h)
      refine ⟨by simp [mapGet_cons, ha, hg], fun v => ?_⟩
      have := hu v; simp only [mapInsert] at this ⊢
      simp [upsertBy, ha, this]

theorem mem_mapInsert {k : κ} {v : ν} {l : List (κ × ν)} {e : κ × ν} (h : e ∈ mapInsert k v l) :
    e = (k, v) ∨ e ∈ l := by
  unfold mapInsert at h
  induction l with
  | nil => simp [upsertBy] at h; exact .inl h
  | cons y ys ih =>
    simp only [upsertBy] at h
    split at h
    · simp at h; rcases h with h | h
      · exact .inl h
      · exact .inr (List.mem_cons_of_mem _ h)
    · simp at h; rcases h with h | h
      · exact .inr (h ▸ List.mem_cons_self)
      · rcases ih h with h | h
        · exact .inl h
        · exact .inr (List.mem_cons_of_mem _ h)

theorem keys_mapInsert (k : κ) (v : ν) (l : List (κ × ν)) :
    (mapInsert k v l).map Prod.fst = if k ∈ l.map Prod.fst then l.map Prod.fst else l.map Prod.fst ++ [k] := by
  unfold mapInsert
  induction l with
  | nil => simp [upsertBy]
  | cons y ys ih =>
    obtain ⟨a, b⟩ := y
    by_cases ha : a = k
    · subst ha; simp [upsertBy]
    · simp only [upsertBy, ha, decide_false, Bool.false_eq_true, if_false, List.map_cons, ih]
      by_cases hk : k ∈ ys.map Prod.fst <;> simp [hk, Ne.symm ha]

theorem nodup_keys_mapInsert {k : κ} {v : ν} {l : List (κ × ν)} (h : (l.map Prod.fst).Nodup) :
    ((mapInsert k v l).map Prod.fst).Nodup := by
  rw [keys_mapInsert]; split
  · exact h
  · rename_i hk
    exact List.nodup_append.2 ⟨h, List.nodup_singleton _, fun a ha b hb => by
      simp at hb; subst hb; rintro rfl; exact hk ha⟩

theorem nodup_keys_mapRemove {k : κ} {l : List (κ × ν)} (h : (l.map Prod.fst).Nodup) :
    ((mapRemove k l).map Prod.fst).Nodup :=
  h.sublist ((List.filter_sublist).map _)

/-- Removing a key that occurs once drops one entry. -/
theorem length_mapRemove_of_nodup {k : κ} {l : List (κ × ν)} (h : (l.map Prod.fst).Nodup)
    (hk : (mapGet k l).isSome) : (mapRemove k l).length + 1 = l.length := by
  induction l with
  | nil => simp at hk
  | cons e es ih =>
    obtain ⟨a, b⟩ := e
    simp only [List.map_cons, List.nodup_cons] at h
    unfold mapRemove at ih ⊢
    by_cases ha : a = k
    · subst ha
      have : es.filter (fun e => decide (e.1 ≠ a)) = es :=
        List.filter_eq_self.2 fun e he => by
          simp only [ne_eq, decide_eq_true_eq]; rintro rfl; exact h.1 (List.mem_map_of_mem he)
      rw [List.filter_cons, if_neg (by simp), this]; simp
    · rw [mapGet_cons, if_neg ha] at hk
      rw [List.filter_cons, if_pos (by simpa using ha), List.length_cons, ih h.2 hk]; simp

/-- Buckets `B` hold each key in its slot, at most once. -/
structure BucketsInv (slot : κ → Nat) (B : List (List (κ × ν))) : Prop where
  slot : ∀ i (h : i < B.length), ∀ e ∈ B[i], slot e.1 = i
  nodup : ∀ i (h : i < B.length), (B[i].map Prod.fst).Nodup

variable {slot : κ → Nat} {B : List (List (κ × ν))}

/-- A key is found in its own bucket. -/
theorem BucketsInv.mapGet_flatten (hI : BucketsInv slot B) (q : κ) (hq : slot q < B.length) :
    mapGet q B.flatten = mapGet q B[slot q] := by
  rw [flatten_split B (slot q) hq, mapGet_append, mapGet_append,
    mapGet_eq_none (a := (B.take _).flatten), mapGet_eq_none (a := (B.drop _).flatten)]
  · simp
  · intro e he heq
    obtain ⟨j, hj, hlt, hm⟩ := mem_flatten_drop he
    have := hI.slot j hj e hm; rw [heq] at this; omega
  · intro e he heq
    obtain ⟨j, hj, hlt, hm⟩ := mem_flatten_take he
    have := hI.slot j hj e hm; rw [heq] at this; omega

omit [DecidableEq κ] in
theorem BucketsInv.set (hI : BucketsInv slot B) {s : Nat} (x : List (κ × ν))
    (hx : ∀ e ∈ x, slot e.1 = s) (hnd : (x.map Prod.fst).Nodup) : BucketsInv slot (B.set s x) where
  slot i hi e he := by
    rw [List.getElem_set] at he; split at he
    · subst_vars; exact hx e he
    · exact hI.slot i (by simpa using hi) e he
  nodup i hi := by
    rw [List.getElem_set]; split
    · exact hnd
    · exact hI.nodup i (by simpa using hi)

/-- Replacing the bucket of slot `s` changes the map only at the keys of `s`. -/
theorem BucketsInv.mapGet_flatten_set (hI : BucketsInv slot B) {s : Nat} (hs : s < B.length)
    (x : List (κ × ν)) (hx : ∀ e ∈ x, slot e.1 = s) (q : κ) :
    mapGet q (B.set s x).flatten = if slot q = s then mapGet q x else mapGet q B.flatten := by
  by_cases hq : slot q = s
  · rw [if_pos hq, flatten_set B s hs, mapGet_append, mapGet_append,
      mapGet_eq_none (a := (B.take _).flatten), mapGet_eq_none (a := (B.drop _).flatten)]
    · simp
    · intro e he heq
      obtain ⟨j, hj, hlt, hm⟩ := mem_flatten_drop he
      have := hI.slot j hj e hm; rw [heq] at this; omega
    · intro e he heq
      obtain ⟨j, hj, hlt, hm⟩ := mem_flatten_take he
      have := hI.slot j hj e hm; rw [heq] at this; omega
  · rw [if_neg hq, flatten_set B s hs, flatten_split B s hs]
    simp only [mapGet_append]
    rw [mapGet_eq_none (a := x) (fun e he heq => hq (by rw [← heq]; exact hx e he)),
      mapGet_eq_none (a := B[s]) (fun e he heq => hq (by rw [← heq]; exact hI.slot s hs e he))]

omit [DecidableEq κ] in
/-- Keys in different buckets differ, so no key appears twice. -/
theorem BucketsInv.nodup_keys (hI : BucketsInv slot B) : (B.flatten.map Prod.fst).Nodup := by
  rw [List.map_flatten, List.nodup_flatten]
  refine ⟨fun l hl => ?_, ?_⟩
  · obtain ⟨b, hb, rfl⟩ := List.mem_map.1 hl
    obtain ⟨i, hi, rfl⟩ := List.getElem_of_mem hb
    exact hI.nodup i hi
  · rw [List.pairwise_iff_getElem]
    intro i j hi hj hij a ha ha'
    simp only [List.getElem_map, List.mem_map] at ha ha'
    obtain ⟨e, he, rfl⟩ := ha
    obtain ⟨e', he', heq⟩ := ha'
    have h1 := hI.slot i (by simpa using hi) e he
    have h2 := hI.slot j (by simpa using hj) e' he'
    rw [heq] at h2; omega

end Maps

end H5iAppLib
