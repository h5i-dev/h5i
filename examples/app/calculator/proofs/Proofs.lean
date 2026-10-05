import Spec
import Schema
/-!
# Proofs about the extracted calculator
-/
open Aeneas Aeneas.Std Result calculator_kernel calculator_kernel.Spec H5iAppLib

namespace calculator_kernel.Proofs

attribute [simp] u64_val_eq

/-- The Rust overflow check for `a * b`, in plain arithmetic. -/
theorem mul_check (a b : Nat) (hb : 0 < b) : (2 ^ 64 - 1) / b < a ↔ 2 ^ 64 ≤ a * b := by
  rw [Nat.div_lt_iff_lt_mul hb]; omega

@[step]
theorem compute_spec (op : Op) (a b : U64) :
    compute op a b ⦃ r => match r with
      | .Ok v => eval op a.val b.val = .ok v.val
      | .Err e => eval op a.val b.val = .error e ⦄ := by
  unfold compute
  cases op <;> simp only [eval]
  case Add =>
    step*
    · simp [U64.rMax]; scalar_tac                -- `MAX - b` does not underflow
    · simp only [U64.rMax] at *                   -- `a > MAX - b` means a + b ≥ 2^64
      rw [if_neg (by scalar_tac)]
    · simp only [U64.rMax] at *; scalar_tac       -- otherwise a + b fits
  case Sub => step*
  case Mul =>
    step*
    · rename_i hb ha                              -- `a > MAX / b` means a * b ≥ 2^64
      have hb : 0 < b.val := by simp at hb; scalar_tac
      have : (2 ^ 64 - 1) / b.val < a.val := by simp only [U64.rMax] at *; scalar_tac
      rw [if_neg (Nat.not_lt.2 ((mul_check _ _ hb).1 this))]
    · rename_i hb ha                              -- otherwise a * b fits
      have hb : 0 < b.val := by simp at hb; scalar_tac
      have : a.val ≤ (2 ^ 64 - 1) / b.val := by simp only [U64.rMax] at *; scalar_tac
      have := (Nat.le_div_iff_mul_le hb).1 this
      scalar_tac
  case Div =>
    step*
    rw [if_neg (by scalar_tac), i_post]

@[step]
theorem memory_of_spec (ms : alloc.vec.Vec Memory) (u : U64) :
    memory_of ms u ⦃ v => v.val = memOf ms.val u.val ⦄ := by
  unfold memory_of memory_of_loop
  apply WP.spec_mono (loop_search ms.val (fun m => decide (m.user = u)) (fun v : U64 => v.val)
    (fun _ m => m.value.val) 0 _ ?_ 0#usize (by simp))
  · intro r hr
    rw [hr, searchFrom_findD]
    simp [memOf]
  · intro j hj; unfold memory_of_loop.body; h5i_step

/-! ## What a command does -/

/-- The kernel never fails: no panic, overflow or bad index, for any input. -/
theorem transition_total (a : Principal) (s : Snapshot) (c : Command) :
    ∃ r, transition a s c = ok r := by
  have h : transition a s c ⦃ _ => True ⦄ := by
    unfold transition; cases c <;> step*
  obtain ⟨r, hr, -⟩ := (WP.spec_equiv_exists _ _).1 h
  exact ⟨r, hr⟩

/-- `apply` computes exactly `eval` on my memory: the new value when there is
one, the same refusal when there is not. -/
theorem apply_correct (a : Principal) (s : Snapshot) (op : Op) (arg : U64) :
    transition a s (.Apply op arg) ⦃ r => match r with
      | .Ok (w, .Value v) =>
        eval op (memOf s.memories.val a.user.val) arg.val = .ok v.val ∧ w = some ⟨a.user, v⟩
      | .Err e => eval op (memOf s.memories.val a.user.val) arg.val = .error e ⦄ := by
  unfold transition
  step with memory_of_spec as ⟨ m, hm ⟩
  step with compute_spec as ⟨ r, hr ⟩
  cases r <;> simp_all

/-- A command writes only the caller's own memory. -/
theorem writes_own_memory (a : Principal) (s : Snapshot) (c : Command) :
    transition a s c ⦃ r => ∀ w reply, r = .Ok (w, reply) → ∀ m, w = some m → m.user = a.user ⦄ := by
  unfold transition
  cases c <;> step* <;> (try split) <;> simp_all

/-! ## What committing does -/

/-- Committing a write replaces (or adds) that user's row. -/
theorem apply_spec (s : Snapshot) (w : Option Memory) (hroom : s.memories.length < Usize.max) :
    apply s w ⦃ s' => s'.memories.val = match w with
      | none => s.memories.val
      | some m => upsert (·.user) m s.memories.val ⦄ := by
  unfold apply
  rw [Schema.Snapshot.clone_eq]
  cases w with
  | none => simp
  | some m => step with Schema.Memory.put_spec _ m hroom; simp_all

theorem memOf_upsert_self (l : List Memory) (m : Memory) : memOf (upsert (·.user) m l) m.user.val = m.value.val := by
  induction l with
  | nil => simp [upsert, memOf]
  | cons x xs ih =>
    by_cases h : x.user = m.user
    · simp [upsert, h, memOf]
    · simp only [upsert, h, if_false]; simpa [memOf, h] using ih

/-- Any successful command either reads my memory and writes nothing, or
writes its result to my memory. -/
theorem shape (a : Principal) (s : Snapshot) (c : Command) :
    transition a s c ⦃ r => ∀ w v, r = .Ok (w, .Value v) →
      (w = none ∧ v.val = memOf s.memories.val a.user.val) ∨ w = some ⟨a.user, v⟩ ⦄ := by
  unfold transition
  cases c <;> step* <;> (try split) <;> simp_all

theorem get_spec (a : Principal) (s : Snapshot) :
    transition a s .Get ⦃ r => ∃ x, r = .Ok (none, .Value x) ∧ x.val = memOf s.memories.val a.user.val ⦄ := by
  unfold transition; step*

/-- The whole round trip: after any successful command, a `get` by the same
user returns that command's result. -/
theorem get_after (a : Principal) (s s' : Snapshot) (c : Command) (w : Option Memory) (v : U64)
    (hroom : s.memories.length < Usize.max)
    (ht : transition a s c = ok (.Ok (w, .Value v))) (hs : apply s w = ok s') :
    transition a s' .Get = ok (.Ok (none, .Value v)) := by
  have hw := post_of_ok (shape a s c) ht w v rfl
  have hs' := post_of_ok (apply_spec s w hroom) hs
  obtain ⟨r, hr⟩ := transition_total a s' .Get
  obtain ⟨x, rfl, hxv⟩ := post_of_ok (get_spec a s') hr
  rw [hr]
  have : x.val = v.val := by
    rcases hw with ⟨rfl, hv⟩ | rfl
    · simp only at hs'; rw [hxv, hs', hv]
    · simp only at hs'; rw [hxv, hs']; exact memOf_upsert_self _ ⟨a.user, v⟩
  simp [(u64_val_eq x v).1 this]

theorem memOf_upsert_other (l : List Memory) (m : Memory) (u : Nat) (hu : u ≠ m.user.val) :
    memOf (upsert (·.user) m l) u = memOf l u := by
  induction l with
  | nil => simp [upsert, memOf, Ne.symm hu]
  | cons x xs ih =>
    by_cases h : x.user = m.user
    · simp [upsert, h, memOf, Ne.symm hu]
    · simp only [upsert, h, if_false]
      by_cases hx : x.user.val = u
      · simp [memOf, hx]
      · simpa [memOf, hx] using ih

/-- Isolation: after any successful command by `a`, committing its write
leaves every other user's memory unchanged. -/
theorem others_unchanged (a : Principal) (s s' : Snapshot) (c : Command) (w : Option Memory) (reply : Reply)
    (hroom : s.memories.length < Usize.max)
    (ht : transition a s c = ok (.Ok (w, reply))) (hs : apply s w = ok s')
    (u : Nat) (hu : u ≠ a.user.val) :
    memOf s'.memories.val u = memOf s.memories.val u := by
  have hown := post_of_ok (writes_own_memory a s c) ht w reply rfl
  have hs' := post_of_ok (apply_spec s w hroom) hs
  cases w with
  | none => simp only at hs'; rw [hs']
  | some m =>
    simp only at hs'
    rw [hs']
    exact memOf_upsert_other _ m u (by rw [hown m rfl]; exact hu)

/-! ## A concrete run

`get_after` assumes the command succeeded and was committed. Here both
happen: Alice sets 5 and reads 5 back. Subtracting 1 from her empty memory is
refused. -/

def alice : Principal := ⟨0#u64, 1#u64⟩
def empty : Snapshot := ⟨alloc.vec.Vec.new Memory⟩

theorem set_then_get : ∃ s', apply empty (some ⟨alice.user, 5#u64⟩) = ok s' ∧
    transition alice s' .Get = ok (.Ok (none, .Value 5#u64)) := by
  have hroom : empty.memories.length < Usize.max := by simp [empty]; scalar_tac
  have ht : transition alice empty (.Set 5#u64) = ok (.Ok (some ⟨alice.user, 5#u64⟩, .Value 5#u64)) := by
    simp [transition]
  obtain ⟨s', hs, -⟩ := (WP.spec_equiv_exists _ _).1 (apply_spec empty (some ⟨alice.user, 5#u64⟩) hroom)
  exact ⟨s', hs, get_after alice empty s' _ _ _ hroom ht hs⟩

theorem sub_refused : transition alice empty (.Apply .Sub 1#u64) = ok (.Err .Underflow) := by
  obtain ⟨r, hr⟩ := transition_total alice empty (.Apply .Sub 1#u64)
  have h := post_of_ok (apply_correct alice empty .Sub 1#u64) hr
  have hm : memOf empty.memories.val alice.user.val = 0 := by simp [empty, memOf]
  rw [hr]
  rcases r with ⟨w, ⟨v⟩⟩ | e <;> simp only [hm, eval] at h
  · simp at h
  · cases e <;> simp at h ⊢

end calculator_kernel.Proofs