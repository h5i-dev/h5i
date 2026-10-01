import Lemmas
import Schema
/-! `apply` computes `Spec.applyAll`. `schema!` generated `apply` and the
table operations with their lemmas (`Schema.lean`); this is the dispatch. -/
open Aeneas Aeneas.Std Result atuin_kernel atuin_kernel.Spec atuin_kernel.Schema H5iAppLib

namespace atuin_kernel.ApplyLemmas

/-- Total rows in a snapshot. -/
def total (s : Snapshot) : Nat := s.users.length + s.sessions.length + s.records.length

theorem write_clone (w : Write) : Write.Insts.CoreCloneClone.clone w = ok w := by
  cases w <;> simp [Write.Insts.CoreCloneClone.clone, User.clone_eq, Session.clone_eq, Record.clone_eq,
    Counter.clone_eq, lift]

theorem apply_write_spec (s : Snapshot) (w : Write) (h : Snapshot.size s < Usize.max) :
    apply_write s w ⦃ s' => Snapshot.toSt s' = applyWrite (Snapshot.toSt s) w ∧
      Snapshot.size s' ≤ Snapshot.size s + 1 ⦄ := by
  have e : rkey = fun r : Record => (r.user, r.host, r.tag, r.idx) := rfl
  unfold Snapshot.size at *
  unfold apply_write
  cases w <;> step* <;> simp_all [Snapshot.toSt, applyWrite, alloc.vec.Vec.length, RECORD_USER, Record.row] <;>
    grind [upsert_length, List.length_filter_le]

/-- No vector can overflow: each write adds at most one row. -/
def Room (s : Snapshot) (n : Nat) : Prop := total s + n < Usize.max

theorem apply_spec (s : Snapshot) (ws : alloc.vec.Vec Write) (h : Room s ws.length) :
    apply s ws ⦃ s' => Snapshot.toSt s' = applyAll (Snapshot.toSt s) ws.val ⦄ :=
  apply_spec' Snapshot.toSt applyWrite apply_write_spec write_clone s ws h

end atuin_kernel.ApplyLemmas
