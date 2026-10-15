import Storage
/-!
# What a read plan loads

`reads a c` (extracted) names one declarative read per table; the server
loads each table by its read (`schema_load_plan`) and `decode`s. Here the
extracted reads are read as `H5iAppLib.Query` reads (`Read.abs`), and
`loadSnap plan s` is the snapshot that loading them from the tables of `s`
decodes to (`decode_load`). `Frame.lean` shows `transition` gives the same
result on `loadSnap (planOf a c) s` as on `s`.

Trusted: PostgreSQL returns, for each read, `Read.rows` of the tenant's
tables listed in key order (the order `load` returns them in), which the
differential tests of `h5i_app_pgsql::compile_read` check.
-/
open Aeneas Aeneas.Std Result conduit_kernel conduit_kernel.Schema H5iAppLib

namespace conduit_kernel.Plan

deriving instance DecidableEq for h5i_app_sql.Val

open H5iAppLib.Query

def Window.abs (w : h5i_app_sql.Window) : Window := ⟨w.desc, w.offset.val, w.limit.map (·.val)⟩

def Pred.abs : h5i_app_sql.Pred → Pred Val
  | .All => .all
  | .Eq c v => .eq c.val v
  | .In c t p o w => .isIn c.val t.val (Pred.abs p) o.val (Window.abs w)
  | .And p q => .and (Pred.abs p) (Pred.abs q)
  | .Or p q => .or (Pred.abs p) (Pred.abs q)

def Read.abs (r : h5i_app_sql.Read) : Read Val := ⟨r.table.val, Pred.abs r.pred, Window.abs r.window⟩

/-- The rows of table `t` (the list `l`, encoded by `row`) that `plan` loads
from the tables `tabs`. -/
def pick {α : Type} (plan : List (Read Val)) (tabs : H5iAppLib.Sql.Tables Val) (t : Nat) (row : α → List Val) (l : List α) :
    List α :=
  match plan.find? (fun q => decide (q.table = t)) with
  | some q => q.window.apply (l.filter (fun x => q.pred.holds tabs (row x)))
  | none => []

theorem pick_sublist {α : Type} (plan : List (Read Val)) (tabs : H5iAppLib.Sql.Tables Val) (t : Nat) (row : α → List Val)
    (l : List α) : List.Sublist (pick plan tabs t row l) l := by
  unfold pick; split
  · exact (Window.apply_sublist _ _).trans List.filter_sublist
  · exact List.nil_sublist _

theorem pick_length {α : Type} (plan : List (Read Val)) (tabs : H5iAppLib.Sql.Tables Val) (t : Nat) (row : α → List Val)
    (l : List α) : (pick plan tabs t row l).length ≤ l.length :=
  (pick_sublist plan tabs t row l).length_le

/-- A vector's rows that the plan loads. -/
def pickV {α : Type} (plan : List (Read Val)) (tabs : H5iAppLib.Sql.Tables Val) (t : Nat) (row : α → List Val)
    (v : alloc.vec.Vec α) : alloc.vec.Vec α :=
  .from (pick plan tabs t row v.val) ((pick_length plan tabs t row v.val).trans v.len_ineq)

@[simp] theorem pickV_val {α : Type} (plan : List (Read Val)) (tabs : H5iAppLib.Sql.Tables Val) (t : Nat) (row : α → List Val)
    (v : alloc.vec.Vec α) : (pickV plan tabs t row v).val = pick plan tabs t row v.val :=
  alloc.vec.Vec.from_val _ _

/-- What loading `plan` from the tables of `s` decodes to. -/
def loadSnap (plan : List (Read Val)) (s : Snapshot) : Snapshot where
  counter := (pick plan (Snapshot.enc s) 6 Counter.row [s.counter]).headD Counter.zero
  users := pickV plan (Snapshot.enc s) 0 User.row s.users
  follows := pickV plan (Snapshot.enc s) 1 Follow.row s.follows
  articles := pickV plan (Snapshot.enc s) 2 Article.row s.articles
  tags := pickV plan (Snapshot.enc s) 3 Tag.row s.tags
  favorites := pickV plan (Snapshot.enc s) 4 Favorite.row s.favorites
  comments := pickV plan (Snapshot.enc s) 5 Comment.row s.comments

/-- `Query.load` of a table that encodes `l`: the encoded rows `pick` keeps. -/
theorem load_enc {α : Type} (plan : List (Read Val)) (tabs : H5iAppLib.Sql.Tables Val) (t : Nat) (row : α → List Val)
    (l : List α) (h : tabs t = l.map row) : load plan tabs t = (pick plan tabs t row l).map row := by
  unfold load pick
  cases hq : plan.find? (fun q => decide (q.table = t)) with
  | none => rfl
  | some q =>
    have ht : q.table = t := by simpa using (List.find?_some hq)
    simp only []
    exact rows_enc tabs q row l (by rw [ht]; exact h)

/-- Rows that hold, for each table, what `plan` loads from `s`'s tables
decode to `loadSnap plan s`. -/
theorem decode_load (plan : List (Read Val)) (s : Snapshot) (r : Rows)
    (h : ∀ t, t < 7 → Rows.tabs r t = load plan (Snapshot.enc s) t) :
    decode r = ok (some (loadSnap plan s)) := by
  have e0 := h 0 (by omega); have e1 := h 1 (by omega); have e2 := h 2 (by omega)
  have e3 := h 3 (by omega); have e4 := h 4 (by omega); have e5 := h 5 (by omega); have e6 := h 6 (by omega)
  rw [load_enc plan _ 0 User.row s.users.val rfl] at e0
  rw [load_enc plan _ 1 Follow.row s.follows.val rfl] at e1
  rw [load_enc plan _ 2 Article.row s.articles.val rfl] at e2
  rw [load_enc plan _ 3 Tag.row s.tags.val rfl] at e3
  rw [load_enc plan _ 4 Favorite.row s.favorites.val rfl] at e4
  rw [load_enc plan _ 5 Comment.row s.comments.val rfl] at e5
  rw [load_enc plan _ 6 Counter.row [s.counter] rfl] at e6
  simp only [Rows.tabs] at e0 e1 e2 e3 e4 e5 e6
  have hl6 : (pick plan (Snapshot.enc s) 6 Counter.row [s.counter]).length ≤ 1 := pick_length _ _ _ _ _
  apply eq_ok_of_spec
  unfold decode
  step with Counter.from_one_spec r.counter _ hl6 e6 as ⟨ o6, h6 ⟩
  subst h6; dsimp only
  step with User.from_rows_spec r.users _ e0 as ⟨ o0, v0, ho0, hv0 ⟩
  subst ho0; dsimp only
  step with Follow.from_rows_spec r.follows _ e1 as ⟨ o1, v1, ho1, hv1 ⟩
  subst ho1; dsimp only
  step with Article.from_rows_spec r.articles _ e2 as ⟨ o2, v2, ho2, hv2 ⟩
  subst ho2; dsimp only
  step with Tag.from_rows_spec r.tags _ e3 as ⟨ o3, v3, ho3, hv3 ⟩
  subst ho3; dsimp only
  step with Favorite.from_rows_spec r.favorites _ e4 as ⟨ o4, v4, ho4, hv4 ⟩
  subst ho4; dsimp only
  step with Comment.from_rows_spec r.comments _ e5 as ⟨ o5, v5, ho5, hv5 ⟩
  subst ho5; dsimp only
  simp only [WP.spec_ok, Option.some.injEq, loadSnap, Snapshot.mk.injEq, true_and]
  refine ⟨?_, ?_, ?_, ?_, ?_, ?_⟩ <;> apply alloc.vec.Vec.ext <;> simp [*]

/-- The plan of `reads a c`, if any, as `H5iAppLib.Query` reads. -/
def planOf (o : Option (alloc.vec.Vec h5i_app_sql.Read)) : Option (List (Read Val)) :=
  o.map (fun v => v.val.map Read.abs)

/-! ## The plans, written out

`readsA` is `reads` read as `H5iAppLib.Query` reads (`reads_abs`); the
frame theorem is stated over it. Tables and columns are numbered as in
`schema!`: users 0 (id, username, email, ...), follows 1 (follower,
followed), articles 2 (id, author, slug, ...), tags 3 (article, tag),
favorites 4 (article, user), comments 5 (id, article, author, ...), the
counter 6. -/

def eqU (c : Nat) (u : U64) : Pred Val := .eq c (int u.val)
def eqB (c : Nat) (t : alloc.vec.Vec U8) : Pred Val := .eq c (.Bytes t)

def orOpt (p : Pred Val) : Option (Pred Val) → Pred Val
  | none => p
  | some q => .or p q

def andOpt (p : Pred Val) : Option (Pred Val) → Pred Val
  | none => p
  | some q => .and p q

def bySlug (slug : alloc.vec.Vec U8) : Pred Val := eqB 2 slug

/-- Column `c` is the id of a user named `n`. -/
def named (c : Nat) (n : alloc.vec.Vec U8) : Pred Val := .isIn c 0 (eqB 1 n) 0 {}

def selPred : Sel → Pred Val
  | .Slug slug => bySlug slug
  | .List tag author fav =>
    andOpt (andOpt (andOpt .all (tag.map (fun t => .isIn 0 3 (eqB 1 t) 0 {}))) (author.map (named 1)))
      (fav.map (fun f => .isIn 0 4 (named 1 f) 0 {}))
  | .Feed u => .isIn 1 1 (eqU 0 u) 1 {}

def newestW (off lim : U64) : Window :=
  ⟨true, 0, if lim.val > U64.rMax - off.val then none else some (off.val + lim.val)⟩

def articlePlan (viewer : U64) (sel : Sel) (w : Window) (extra : Option (Pred Val)) : List (Read Val) :=
  [⟨0, orOpt (.isIn 0 2 (selPred sel) 1 w) extra, {}⟩, ⟨1, eqU 0 viewer, {}⟩, ⟨2, selPred sel, w⟩,
    ⟨3, .isIn 0 2 (selPred sel) 0 w, {}⟩, ⟨4, .isIn 0 2 (selPred sel) 0 w, {}⟩]

def listNames (author fav : Option (alloc.vec.Vec U8)) : Option (Pred Val) :=
  match fav with
  | none => author.map (eqB 1)
  | some f => some (orOpt (eqB 1 f) (author.map (eqB 1)))

def readsA (a : Principal) : Command → Option (List (Read Val))
  | .CurrentUser => some [⟨0, eqU 0 a.user, {}⟩]
  | .UpdateUser e n _ _ _ => some [⟨0, orOpt (orOpt (eqU 0 a.user) (n.map (eqB 1))) (e.map (eqB 2)), {}⟩]
  | .GetProfile n => some [⟨0, eqB 1 n, {}⟩, ⟨1, eqU 0 a.user, {}⟩]
  | .Follow n => some [⟨0, .or (eqU 0 a.user) (eqB 1 n), {}⟩]
  | .Unfollow n => some [⟨0, .or (eqU 0 a.user) (eqB 1 n), {}⟩]
  | .GetArticle slug => some (articlePlan a.user (.Slug slug) {} none)
  | .Favorite slug => some (articlePlan a.user (.Slug slug) {} (some (eqU 0 a.user)))
  | .Unfavorite slug => some (articlePlan a.user (.Slug slug) {} (some (eqU 0 a.user)))
  | .ListArticles tag au fav lim off =>
    some (articlePlan a.user (.List tag au fav) (newestW off lim) (listNames au fav))
  | .Feed lim off => some (articlePlan a.user (.Feed a.user) (newestW off lim) (some (eqU 0 a.user)))
  | .GetComments slug =>
    some [⟨0, .isIn 0 5 (.isIn 1 2 (bySlug slug) 0 {}) 2 {}, {}⟩, ⟨1, eqU 0 a.user, {}⟩, ⟨2, bySlug slug, {}⟩,
      ⟨5, .isIn 1 2 (bySlug slug) 0 {}, {}⟩]
  | .AddComment slug _ _ => some [⟨0, eqU 0 a.user, {}⟩, ⟨1, eqU 0 a.user, {}⟩, ⟨2, bySlug slug, {}⟩, ⟨6, .all, {}⟩]
  | .DeleteComment slug id => some [⟨0, eqU 0 a.user, {}⟩, ⟨2, bySlug slug, {}⟩, ⟨5, eqU 0 id, {}⟩]
  | _ => none

theorem bytes_eq (v : alloc.vec.Vec U8) : alloc.vec.VecU8.Insts.H5i_app_sqlColumn.to_val v = ok (.Bytes v) :=
  eq_ok_of_spec (bytes_to_val v)

theorem opt_text_eq (o : Option (alloc.vec.Vec U8)) : opt_text o = ok o := by
  cases o <;> simp [opt_text, Schema.u8vec_clone]

theorem newest_spec (off lim : U64) : newest off lim ⦃ w => Window.abs w = newestW off lim ⦄ := by
  unfold newest newestW
  step*
  · have h1 : (core.num.U64.MAX).val = U64.max := by simp [core.num.U64.MAX]; rw [U64.max_eq]; rfl
    rw [h1]; scalar_tac
  · have : lim.val > U64.rMax - off.val := by scalar_tac
    simp [Window.abs, this]
  · simp only [U64.max_eq]; simp only [U64.rMax] at *; scalar_tac
  · have : ¬ lim.val > U64.rMax - off.val := by scalar_tac
    simp [Window.abs, *]

/-- The extracted `reads` computes `readsA`. -/
theorem reads_abs (a : Principal) (c : Command) : ∃ o, reads a c = ok o ∧ planOf o = readsA a c := by
  cases c
  case UpdateUser e n p b i =>
    cases e <;> cases n <;> simp [reads, rd_where, p_eq, p_or, or_opt, v_int, v_bytes, bytes_eq, u64_to_val_eq,
      planOf, readsA, Read.abs, Pred.abs, Window.abs, eqU, eqB, orOpt, h5i_app_sql.whole, alloc.vec.Vec.push,
      alloc.vec.Vec.from_val, U32.max, U32.numBits, T_USERS, USER_ID, USER_NAME, USER_EMAIL]
  case ListArticles t au f lim off =>
    obtain ⟨w, hw, hwa⟩ := (WP.spec_equiv_exists _ _).1 (newest_spec off lim)
    cases t <;> cases au <;> cases f <;> simp [reads, article_plan, sel_pred, v_int, v_bytes, p_eq, p_in, p_or,
      p_and, rd_where, of_articles, or_opt, and_opt, conduit_kernel.named, hw, u64_to_val_eq, Schema.u8vec_clone,
      h5i_app_sql.whole, alloc.vec.Vec.push, bytes_eq, alloc.vec.Vec.from_val, U32.max, U32.numBits, opt_text_eq,
      planOf, readsA, Read.abs, Pred.abs, articlePlan, selPred, eqU, eqB, orOpt, andOpt, Plan.named, listNames, ← hwa,
      T_USERS, T_FOLLOWS, T_ARTICLES, T_TAGS, T_FAVORITES, USER_ID, USER_NAME, FOLLOW_FOLLOWER, ARTICLE_ID,
      ARTICLE_AUTHOR, TAG_TAG, FAVORITE_USER] <;> simp [Window.abs]
  case Feed lim off =>
    obtain ⟨w, hw, hwa⟩ := (WP.spec_equiv_exists _ _).1 (newest_spec off lim)
    simp [reads, article_plan, sel_pred, v_int, p_eq, p_in, p_or, rd_where, of_articles, or_opt, hw, u64_to_val_eq,
      h5i_app_sql.whole, alloc.vec.Vec.push, alloc.vec.Vec.from_val, U32.max, U32.numBits,
      planOf, readsA, Read.abs, Pred.abs, articlePlan, selPred, eqU, orOpt, ← hwa,
      T_USERS, T_FOLLOWS, T_ARTICLES, T_TAGS, T_FAVORITES, USER_ID, FOLLOW_FOLLOWER, FOLLOW_FOLLOWED, ARTICLE_ID,
      ARTICLE_AUTHOR] <;> simp [Window.abs]
  all_goals simp [reads, article_plan, sel_pred, by_slug, v_int, v_bytes, p_eq, p_in, p_or, rd_where, of_articles,
      or_opt, u64_to_val_eq, Schema.u8vec_clone, h5i_app_sql.whole, alloc.vec.Vec.push, bytes_eq,
      alloc.vec.Vec.from_val, U32.max, U32.numBits, planOf, readsA, Read.abs, Pred.abs, Window.abs, articlePlan,
      selPred, eqU, eqB, orOpt, bySlug, T_USERS, T_FOLLOWS, T_ARTICLES, T_TAGS, T_FAVORITES, T_COMMENTS, T_COUNTER,
      USER_ID, USER_NAME, FOLLOW_FOLLOWER, ARTICLE_ID, ARTICLE_AUTHOR, ARTICLE_SLUG, COMMENT_ID, COMMENT_AUTHOR]

end conduit_kernel.Plan
