import Theorems
import Schema
/-!
# Committing a write set

The kernel's `apply` computes `Spec.applyAll`, which `Theorems.lean` uses.
`schema!` generated `apply` and the table operations with their lemmas
(`Schema.lean`), so only the dispatch over `Write` is left.
-/
open Aeneas Aeneas.Std Result board_kernel board_kernel.Spec board_kernel.Schema H5iAppLib

namespace board_kernel.ApplyProofs

theorem write_clone (w : Write) : Write.Insts.CoreCloneClone.clone w = ok w := by
  cases w <;> simp [Write.Insts.CoreCloneClone.clone, Post.clone_eq, Moderator.clone_eq, Counter.clone_eq, lift]

/-- One write does what `applyWrite` says, and adds at most one row. -/
theorem apply_write_spec (s : Snapshot) (w : Write) (h : Snapshot.size s < Usize.max) :
    apply_write s w ⦃ s' => Snapshot.toSt s' = applyWrite (Snapshot.toSt s) w ∧
      Snapshot.size s' ≤ Snapshot.size s + 1 ⦄ := by
  unfold Snapshot.size at *
  unfold apply_write
  cases w <;> step* <;> simp_all [Snapshot.toSt, applyWrite, Snapshot.size, alloc.vec.Vec.length] <;>
    grind [upsert_length, List.length_filter_le]

/-- The kernel's `apply` computes `Spec.applyAll`, as long as no vector can
overflow (each write adds at most one row). -/
theorem apply_spec (s : Snapshot) (ws : alloc.vec.Vec Write) (h : Snapshot.size s + ws.length < Usize.max) :
    apply s ws ⦃ s' => Snapshot.toSt s' = applyAll (Snapshot.toSt s) ws.val ⦄ :=
  apply_spec' Snapshot.toSt applyWrite apply_write_spec write_clone s ws h

end board_kernel.ApplyProofs
