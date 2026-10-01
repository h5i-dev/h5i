import Commands
import Schema
/-!
# Committing a write set

The kernel's `apply` computes `Spec.applyAll`. The table operations and
their lemmas come from `schema!` (`Schema.lean`), so only the dispatch over
`Write` is proved here. `Comment.row` gives the column a cascade deletes by.
-/
open Aeneas Aeneas.Std Result conduit_kernel conduit_kernel.Spec conduit_kernel.Schema H5iAppLib

namespace conduit_kernel.ApplyProofs

/-- Rows in a snapshot; each write adds at most one. -/
def total (s : Snapshot) : Nat :=
  s.users.length + s.follows.length + s.articles.length + s.tags.length + s.favorites.length + s.comments.length

theorem write_clone (w : Write) : Write.Insts.CoreCloneClone.clone w = ok w := by
  cases w <;> simp [Write.Insts.CoreCloneClone.clone, User.clone_eq, Follow.clone_eq, Article.clone_eq,
    Tag.clone_eq, Favorite.clone_eq, Comment.clone_eq, Counter.clone_eq, lift]

theorem apply_write_spec (s : Snapshot) (w : Write) (h : Snapshot.size s < Usize.max) :
    apply_write s w ⦃ s' => Snapshot.toSt s' = applyWrite (Snapshot.toSt s) w ∧
      Snapshot.size s' ≤ Snapshot.size s + 1 ⦄ := by
  unfold Snapshot.size at *
  unfold apply_write
  cases w <;> step* <;> simp_all [Snapshot.toSt, applyWrite, alloc.vec.Vec.length, TAG_ARTICLE,
    FAVORITE_ARTICLE, COMMENT_ARTICLE, Tag.row, Favorite.row, Comment.row] <;>
    grind [upsert_length, List.length_filter_le]

/-- The kernel's `apply` computes `Spec.applyAll`, as long as no vector can
overflow (each write adds at most one row). -/
theorem apply_spec (s : Snapshot) (ws : alloc.vec.Vec Write) (h : total s + ws.length < Usize.max) :
    apply s ws ⦃ s' => Snapshot.toSt s' = applyAll (Snapshot.toSt s) ws.val ⦄ :=
  apply_spec' Snapshot.toSt applyWrite apply_write_spec write_clone s ws h

end conduit_kernel.ApplyProofs
