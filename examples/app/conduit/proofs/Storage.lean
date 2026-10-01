import Apply
import Invariants
/-!
# What the store holds

The server writes with the kernel's `sql_writes` and loads with `decode`.
`H5iAppLib.Store` proves for any schema that the database then holds the
encoding of `applyAll`; this file supplies Conduit's encoding and table
writes. Cascades run as a `SELECT` plus keyed deletes.
-/
open Aeneas Aeneas.Std Result conduit_kernel conduit_kernel.Spec conduit_kernel.Schema H5iAppLib H5iAppLib.Sql H5iAppLib.Store

namespace conduit_kernel.Storage

/-- A state's rows, table by table. -/
def enc (s : St) : Tables Val
  | 0 => s.users.map User.row
  | 1 => s.follows.map Follow.row
  | 2 => s.articles.map Article.row
  | 3 => s.tags.map Tag.row
  | 4 => s.favs.map Favorite.row
  | 5 => s.comments.map Comment.row
  | 6 => [[int s.lastUser, int s.lastArticle, int s.lastComment]]
  | _ => []

/-- The table writes of one write. -/
def sqlA : Write → List (AWrite Val)
  | .PutUser x => [User.putA x]
  | .PutFollow f => [Follow.putA f]
  | .DelFollow f => [Follow.delA f.follower f.followed]
  | .PutArticle a => [Article.putA a]
  | .DelArticle id => [Article.delA id]
  | .PutTag t => [Tag.putA t]
  | .DelTagsOf id => [Tag.delWhereA 0 (int id.val)]
  | .PutFavorite f => [Favorite.putA f]
  | .DelFavorite f => [Favorite.delA f.article f.user]
  | .DelFavoritesOf id => [Favorite.delWhereA 0 (int id.val)]
  | .PutComment c => [Comment.putA c]
  | .DelComment id => [Comment.delA id]
  | .DelCommentsOf id => [Comment.delWhereA 1 (int id.val)]
  | .SetCounter c => [Counter.putA c]

set_option maxHeartbeats 1000000 in
theorem enc_step : ∀ s w, applyAllW kl (enc s) (sqlA w) = enc (applyWrite s w) := by
  schema_step [enc, sqlA, applyWrite]

def app : App St Write Val where
  kl := kl
  enc := enc
  sql := sqlA
  step := applyWrite
  init := init
  IsRow := IsRow
  enc_step := enc_step
  sql_ok := by schema_ok [sqlA]
  init_ok := by schema_init [enc, init]
  init_rows := by schema_rows [enc, init]

theorem fits : Fits app Snapshot.toSt where
  kl := rfl
  rows := rfl
  enc s := by funext t; cases_table t <;> rfl
  init := by funext t; cases_table t <;> rfl
  nil s t h := by cases_table t <;> first | omega | rfl

theorem sql_fits : SqlFits app := by
  intro w out h
  unfold sql_write; cases w <;> simp only [app, sqlA, List.length_singleton] at h ⊢ <;> step* <;>
    simp_all [TAG_ARTICLE, FAVORITE_ARTICLE, COMMENT_ARTICLE]

/-- The store holds what `apply` computes: every database the server produces
from an empty site reads back exactly the rows of the state `applyAll` gives
for its commits, and loading it decodes to that state, up to row order. -/
theorem stored {db : Db Val} {s : St} (h : Served app Snapshot.toSt db s) :
    app.Holds db (enc s) ∧
      ∀ r, Lists kl db (Rows.tabs r) → decode r ⦃ o => ∃ snap, o = some snap ∧ app.Equiv (Snapshot.toSt snap) s ⦄ :=
  Schema.stored fits sql_fits h

/-! ## The invariants on PostgreSQL

`Accepted` write sets keep `Inv` (`inv_preserved`); `Schema.pg_loaded_inv`
carries that through the compiled statements to what a later load decodes. -/

/-- Write sets the kernel returns for a command it accepts. -/
def Accepted (snap : Snapshot) (ws : alloc.vec.Vec Write) : Prop :=
  ∃ a c r, transition a snap c = .ok (.Ok (ws, r))

/-- The counters fit their `BIGINT`s. -/
def Bounded (s : St) : Prop := s.lastUser < 2 ^ 64 ∧ s.lastArticle < 2 ^ 64 ∧ s.lastComment < 2 ^ 64

/-- The invariants, and the counters fit their `BIGINT`s. -/
def DbInv (s : St) : Prop := Inv s ∧ Bounded s

theorem bounded_applyAll (s : St) (ws : List Write) (h : Bounded s) : Bounded (applyAll s ws) := by
  induction ws generalizing s with
  | nil => exact h
  | cons w ws ih =>
    apply ih
    cases w <;> simp only [applyWrite, Bounded] at h ⊢ <;> first | exact h | scalar_tac

/-- `Inv` depends on each table only up to row order. -/
theorem inv_perm {s s' : St} (hu : s'.lastUser = s.lastUser) (ha : s'.lastArticle = s.lastArticle)
    (hc : s'.lastComment = s.lastComment) (h0 : s'.users.Perm s.users) (h1 : s'.follows.Perm s.follows)
    (h2 : s'.articles.Perm s.articles) (h3 : s'.tags.Perm s.tags) (h4 : s'.favs.Perm s.favs)
    (h5 : s'.comments.Perm s.comments) (hi : Inv s) : Inv s' := by
  have iu : ∀ u, isUser s u → isUser s' u := fun _ ⟨x, hx, e⟩ => ⟨x, h0.symm.subset hx, e⟩
  have ia : ∀ a, isArticle s a → isArticle s' a := fun _ ⟨x, hx, e⟩ => ⟨x, h2.symm.subset hx, e⟩
  exact {
    user_ids := (h0.map _).nodup_iff.2 hi.user_ids
    user_fresh := fun x hx => hu ▸ hi.user_fresh x (h0.subset hx)
    usernames := (h0.map _).nodup_iff.2 hi.usernames
    emails := (h0.map _).nodup_iff.2 hi.emails
    follow_keys := (h1.map _).nodup_iff.2 hi.follow_keys
    follow_self := fun f hf => hi.follow_self f (h1.subset hf)
    follow_users := fun f hf =>
      ⟨iu _ (hi.follow_users f (h1.subset hf)).1, iu _ (hi.follow_users f (h1.subset hf)).2⟩
    article_ids := (h2.map _).nodup_iff.2 hi.article_ids
    article_fresh := fun a hx => ha ▸ hi.article_fresh a (h2.subset hx)
    slugs := (h2.map _).nodup_iff.2 hi.slugs
    article_authors := fun a hx => iu _ (hi.article_authors a (h2.subset hx))
    tag_keys := (h3.map _).nodup_iff.2 hi.tag_keys
    tag_articles := fun t ht => ia _ (hi.tag_articles t (h3.subset ht))
    fav_keys := (h4.map _).nodup_iff.2 hi.fav_keys
    fav_articles := fun f hf => ia _ (hi.fav_articles f (h4.subset hf))
    fav_users := fun f hf => iu _ (hi.fav_users f (h4.subset hf))
    comment_ids := (h5.map _).nodup_iff.2 hi.comment_ids
    comment_fresh := fun x hx => hc ▸ hi.comment_fresh x (h5.subset hx)
    comment_articles := fun x hx => ia _ (hi.comment_articles x (h5.subset hx))
    comment_authors := fun x hx => iu _ (hi.comment_authors x (h5.subset hx)) }

theorem dbInv_equiv (snap : Snapshot) (s : St) (h : app.Equiv (Snapshot.toSt snap) s) (hi : DbInv s) :
    DbInv (Snapshot.toSt snap) := by
  have h0 := h 0
  have h1 := h 1
  have h2 := h 2
  have h3 := h 3
  have h4 := h 4
  have h5 := h 5
  have h6 := h 6
  simp only [app, enc, Snapshot.toSt] at h0 h1 h2 h3 h4 h5 h6
  rw [List.map_perm_map_iff User.row_inj] at h0
  rw [List.map_perm_map_iff Follow.row_inj] at h1
  rw [List.map_perm_map_iff Article.row_inj] at h2
  rw [List.map_perm_map_iff Tag.row_inj] at h3
  rw [List.map_perm_map_iff Favorite.row_inj] at h4
  rw [List.map_perm_map_iff Comment.row_inj] at h5
  simp only [List.perm_singleton, List.cons.injEq, and_true] at h6
  obtain ⟨hi, b1, b2, b3⟩ := hi
  have hu : snap.counter.last_user.val = s.lastUser := int_inj (by scalar_tac) b1 h6.1
  have ha : snap.counter.last_article.val = s.lastArticle := int_inj (by scalar_tac) b2 h6.2.1
  have hc : snap.counter.last_comment.val = s.lastComment := int_inj (by scalar_tac) b3 h6.2.2
  exact ⟨inv_perm hu ha hc h0 h1 h2 h3 h4 h5 hi, by simp only [Bounded, Snapshot.toSt]; scalar_tac⟩

/-- The invariants hold on the database. For the schema the server passes to
`h5i_app_pgsql`, every database the server produces, one accepted command at a
time among other commits, holds a state satisfying `Inv`, and every snapshot
a later load decodes satisfies `Inv`. -/
theorem db_inv {ts : List Pg.Tab} {tv : Val} (hv : Pg.Valid ts) (htv : valKind tv = some .int)
    (hkl : Pg.klOf ts = kl) (hlen : ts.length = 7) {db : Pg.PgDb Val} {s : St}
    (h : PgServed app Snapshot.toSt ts tv Accepted db s) :
    Inv s ∧ ∀ r, Loads ts tv db r → decode r ⦃ o => ∃ snap, o = some snap ∧ Inv (Snapshot.toSt snap) ⦄ := by
  obtain ⟨hi, hload⟩ := pg_loaded_inv sql_fits fits hv htv hkl hlen DbInv
    ⟨Invariants.init_inv, by simp [app, init, Bounded]⟩ dbInv_equiv
    (fun snap ws ⟨a, c, r, ht⟩ hi => ⟨Invariants.inv_preserved a snap c ws r hi.1 ht,
      bounded_applyAll _ _ (by simp only [Bounded, Snapshot.toSt]; scalar_tac)⟩) h
  refine ⟨hi.1, fun r hr => WP.spec_mono (hload r hr) ?_⟩
  rintro o ⟨snap, rfl, hs⟩
  exact ⟨snap, rfl, hs.1⟩

end conduit_kernel.Storage
