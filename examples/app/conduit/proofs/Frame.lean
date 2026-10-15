import Plan
import Commands
/-!
# Frame theorem: the read plans load enough

`frame`: for every command with a read plan (`reads a c = some plan`),
`transition` on `Snapshot.load plan s` (what the server loads, `Plan.decode_load`)
gives exactly the result it gives on the whole site `s`. The only fact about
`s` it needs is that usernames are unique (`Inv.usernames`), for the
listing's author and `favorited` filters, which the plan resolves with
`IN (SELECT id FROM users WHERE username = ...)`.

Each command's plan keeps, of every table it reads, all rows a lookup of the
kernel can return (`H5iAppLib.Query.find?_slice`, `any_slice`,
`filter_slice`); the listing and the feed load only the newest
`offset + limit` matching articles, which hold the page
(`H5iAppLib.Query.page_of_newest`).
-/
open Classical Aeneas Aeneas.Std Result conduit_kernel conduit_kernel.Spec conduit_kernel.Helpers
  conduit_kernel.Commands conduit_kernel.Schema conduit_kernel.Plan H5iAppLib H5iAppLib.Query

namespace conduit_kernel.Frame

/-! ## Results that agree on what they compute -/

/-- Two computations that both satisfy a postcondition with at most one
solution are equal. -/
theorem spec_congr {α : Type} {m m' : Result α} {P : α → Prop} (h : m ⦃ P ⦄) (h' : m' ⦃ P ⦄)
    (u : ∀ x y, P x → P y → x = y) : m = m' := by
  obtain ⟨x, hx, px⟩ := (WP.spec_equiv_exists _ _).1 h
  obtain ⟨y, hy, py⟩ := (WP.spec_equiv_exists _ _).1 h'
  rw [hx, hy, u x y px py]

theorem toView_inj : Function.Injective ArticleView.toView := by
  intro x y h
  obtain ⟨a, t, f, n, p⟩ := x
  obtain ⟨a', t', f', n', p'⟩ := y
  simp only [ArticleView.toView, View.mk.injEq] at h
  obtain ⟨rfl, ht, rfl, hn, rfl⟩ := h
  have : t = t' := alloc.vec.Vec.ext _ _ ht
  have : n = n' := UScalar.eq_of_val_eq hn
  subst_vars; rfl

theorem toViews_inj {x y : alloc.vec.Vec ArticleView} (h : x.val.map ArticleView.toView = y.val.map ArticleView.toView) :
    x = y :=
  alloc.vec.Vec.ext _ _ (List.map_injective_iff.2 toView_inj h)

/-! ## What a reply reads, on a slice -/

section Slice
variable (s s' : St)

/-- `s'` keeps, of each table of `s`, a sublist that is a filter of it
(every table but the articles, which may also be windowed). -/
structure Keeps (ku : User → Bool) (kf : Follow → Bool) (kt : Tag → Bool) (kv : Favorite → Bool)
    (kc : Comment → Bool) : Prop where
  users : s'.users = s.users.filter ku
  follows : s'.follows = s.follows.filter kf
  tags : s'.tags = s.tags.filter kt
  favs : s'.favs = s.favs.filter kv
  comments : s'.comments = s.comments.filter kc

variable {s s'} {ku : User → Bool} {kf : Follow → Bool} {kt : Tag → Bool} {kv : Favorite → Bool}
  {kc : Comment → Bool}

theorem follows_keep (h : Keeps s s' ku kf kt kv kc) (u v : U64) (hk : ∀ f ∈ s.follows, f.follower = u → kf f) :
    follows s' u v = follows s u v := by
  simp only [follows, h.follows]
  exact any_slice _ _ _ (fun f hf hp => hk f hf (by simp at hp; exact hp.1))

theorem userById_keep (h : Keeps s s' ku kf kt kv kc) (id : U64) (hk : ∀ x ∈ s.users, x.id = id → ku x) :
    userById s' id = userById s id := by
  simp only [userById, h.users]
  exact find?_slice _ _ _ (fun x hx hp => hk x hx (by simpa using hp))

theorem userByName_keep (h : Keeps s s' ku kf kt kv kc) (n : Text) (hk : ∀ x ∈ s.users, x.username = n → ku x) :
    userByName s' n = userByName s n := by
  simp only [userByName, h.users]
  exact find?_slice _ _ _ (fun x hx hp => hk x hx (by simpa using hp))

theorem userByEmail_keep (h : Keeps s s' ku kf kt kv kc) (e : Text) (hk : ∀ x ∈ s.users, x.email = e → ku x) :
    userByEmail s' e = userByEmail s e := by
  simp only [userByEmail, h.users]
  exact find?_slice _ _ _ (fun x hx hp => hk x hx (by simpa using hp))

theorem profileOf_keep (h : Keeps s s' ku kf kt kv kc) (u : U64) (x : User)
    (hf : ∀ f ∈ s.follows, f.follower = u → kf f) : profileOf s' u x = profileOf s u x := by
  simp only [profileOf, follows_keep h u x.id hf]

theorem authorOf_keep (h : Keeps s s' ku kf kt kv kc) (u id : U64) (hk : ∀ x ∈ s.users, x.id = id → ku x)
    (hf : ∀ f ∈ s.follows, f.follower = u → kf f) : authorOf s' u id = authorOf s u id := by
  simp only [authorOf, userById_keep h id hk]
  split <;> simp only [profileOf_keep h u _ hf]

theorem tagsOf_keep (h : Keeps s s' ku kf kt kv kc) (a : U64) (hk : ∀ t ∈ s.tags, t.article = a → kt t) :
    tagsOf s' a = tagsOf s a := by
  simp only [tagsOf, h.tags]
  rw [filter_slice _ _ _ (fun t ht hp => hk t ht (by simpa using hp))]

theorem favorited_keep (h : Keeps s s' ku kf kt kv kc) (u a : U64) (hk : ∀ f ∈ s.favs, f.article = a → kv f) :
    favorited s' u a = favorited s u a := by
  simp only [favorited, h.favs]
  exact any_slice _ _ _ (fun f hf hp => hk f hf (by simp at hp; exact hp.1))

theorem favCount_keep (h : Keeps s s' ku kf kt kv kc) (a : U64) (hk : ∀ f ∈ s.favs, f.article = a → kv f) :
    favCount s' a = favCount s a := by
  simp only [favCount, h.favs]
  rw [filter_slice _ _ _ (fun f hf hp => hk f hf (by simpa using hp))]

/-- An article's view reads its tags and favorites, its author, and the
viewer's follows. -/
theorem view_keep (h : Keeps s s' ku kf kt kv kc) (u : U64) (a : Article)
    (ht : ∀ t ∈ s.tags, t.article = a.id → kt t) (hv : ∀ f ∈ s.favs, f.article = a.id → kv f)
    (hu : ∀ x ∈ s.users, x.id = a.author → ku x) (hf : ∀ f ∈ s.follows, f.follower = u → kf f) :
    view s' u a = view s u a := by
  simp only [view, tagsOf_keep h a.id ht, favorited_keep h u a.id hv, favCount_keep h a.id hv,
    authorOf_keep h u a.author hu hf]

theorem commentsOf_keep (h : Keeps s s' ku kf kt kv kc) (u a : U64)
    (hc : ∀ c ∈ s.comments, c.article = a → kc c)
    (hu : ∀ c ∈ s.comments, c.article = a → ∀ x ∈ s.users, x.id = c.author → ku x)
    (hf : ∀ f ∈ s.follows, f.follower = u → kf f) :
    commentsOf s' u a = commentsOf s u a := by
  simp only [commentsOf, h.comments]
  rw [filter_slice _ _ _ (fun c hc' hp => hc c hc' (by simpa using hp))]
  apply List.map_congr_left
  intro c hcm
  have := List.mem_filter.1 hcm
  rw [authorOf_keep h u c.author (hu c this.1 (by simpa using this.2)) hf]

/-- The page of the newest `offset + limit` rows is the page of all of them. -/
theorem pageOf_newest (l : List Article) (off lim : U64) :
    pageOf ((newestW off lim).apply l) off.val lim.val = pageOf l off.val lim.val := by
  unfold newestW pageOf
  split
  · simp [Window.apply, cut]
  · exact page_of_newest l off.val lim.val

theorem mem_of_mem_pageOf {l : List Article} {o n : Nat} {y : Article} (h : y ∈ pageOf l o n) : y ∈ l := by
  unfold pageOf at h
  exact List.mem_reverse.1 (List.mem_of_mem_drop (List.mem_of_mem_take h))

/-- A page of views over the newest matching articles reads only those
articles' tags, favorites and authors, and the viewer's follows. -/
theorem page_views_keep (h : Keeps s s' ku kf kt kv kc) (u : U64) (P P' : Article → Bool) (off lim : U64)
    (hA : s'.articles = (newestW off lim).apply (s.articles.filter P))
    (hP : ∀ y ∈ (newestW off lim).apply (s.articles.filter P), P' y = true)
    (ht : ∀ y ∈ (newestW off lim).apply (s.articles.filter P), ∀ t ∈ s.tags, t.article = y.id → kt t)
    (hv : ∀ y ∈ (newestW off lim).apply (s.articles.filter P), ∀ f ∈ s.favs, f.article = y.id → kv f)
    (hu : ∀ y ∈ (newestW off lim).apply (s.articles.filter P), ∀ x ∈ s.users, x.id = y.author → ku x)
    (hf : ∀ f ∈ s.follows, f.follower = u → kf f) :
    (pageOf (s'.articles.filter P') off.val lim.val).map (view s' u) =
      (pageOf (s.articles.filter P) off.val lim.val).map (view s u) := by
  rw [hA, List.filter_eq_self.2 hP, pageOf_newest]
  apply List.map_congr_left
  intro y hy
  have hy' : y ∈ (newestW off lim).apply (s.articles.filter P) := by
    rw [← pageOf_newest] at hy; exact mem_of_mem_pageOf hy
  exact view_keep h u y (ht y hy') (hv y hy') (hu y hy') hf

end Slice

/-! ## What a plan keeps -/

/-- The rows of table `t` the plan keeps, for a read without a window. -/
noncomputable def keepOf {α : Type} (plan : List (Read Val)) (tabs : H5iAppLib.Sql.Tables Val) (t : Nat) (row : α → List Val)
    (x : α) : Bool :=
  match plan.find? (fun q => decide (q.table = t)) with
  | some q => q.pred.holds tabs (row x)
  | none => false

/-- Every read of the plan but the articles' has no window. -/
def Unwindowed (plan : List (Read Val)) : Prop := ∀ q ∈ plan, q.table ≠ 2 → q.window = {}

theorem pick_eq {α : Type} {plan : List (Read Val)} (hw : Unwindowed plan) (tabs : H5iAppLib.Sql.Tables Val)
    (t : Nat) (ht : t ≠ 2) (row : α → List Val) (l : List α) :
    pickRows plan tabs t row l = l.filter (keepOf plan tabs t row) := by
  unfold pickRows keepOf
  cases hq : plan.find? (fun q => decide (q.table = t)) with
  | none => simp
  | some q =>
    have hm := List.mem_of_find?_eq_some hq
    have hqt : q.table = t := by simpa using List.find?_some hq
    simp only [hw q hm (hqt ▸ ht), Window.apply_default]

theorem keeps {plan : List (Read Val)} (hw : Unwindowed plan) (s : Snapshot) :
    Keeps (Snapshot.toSt s) (Snapshot.toSt (Snapshot.load plan s)) (keepOf plan (Snapshot.enc s) 0 User.row)
      (keepOf plan (Snapshot.enc s) 1 Follow.row) (keepOf plan (Snapshot.enc s) 3 Tag.row)
      (keepOf plan (Snapshot.enc s) 4 Favorite.row) (keepOf plan (Snapshot.enc s) 5 Comment.row) where
  users := by simp [Snapshot.toSt, pick_eq hw]
  follows := by simp [Snapshot.toSt, pick_eq hw]
  tags := by simp [Snapshot.toSt, pick_eq hw]
  favs := by simp [Snapshot.toSt, pick_eq hw]
  comments := by simp [Snapshot.toSt, pick_eq hw]

/-- The articles a plan loads. -/
noncomputable def artsOf (plan : List (Read Val)) (s : Snapshot) : List Article :=
  pickRows plan (Snapshot.enc s) 2 Article.row s.articles.val

@[simp] theorem toSt_articles (plan : List (Read Val)) (s : Snapshot) :
    (Snapshot.toSt (Snapshot.load plan s)).articles = artsOf plan s := by
  simp [Snapshot.toSt, artsOf]

/-! ## Predicates on encoded rows -/

@[simp] theorem holds_eq (tabs : H5iAppLib.Sql.Tables Val) (c : Nat) (v : Val) (r : List Val) :
    (Pred.eq c v).holds tabs r = decide (r[c]? = some v) := rfl

@[simp] theorem holds_all (tabs : H5iAppLib.Sql.Tables Val) (r : List Val) : (Pred.all : Pred Val).holds tabs r = true := rfl

@[simp] theorem holds_or (tabs : H5iAppLib.Sql.Tables Val) (p q : Pred Val) (r : List Val) :
    (Pred.or p q).holds tabs r = (p.holds tabs r || q.holds tabs r) := rfl

@[simp] theorem holds_and (tabs : H5iAppLib.Sql.Tables Val) (p q : Pred Val) (r : List Val) :
    (Pred.and p q).holds tabs r = (p.holds tabs r && q.holds tabs r) := rfl

/-- Unfolds the predicates of a plan on encoded rows. -/
macro "holds_simp" : tactic => `(tactic| simp [keepOf, Unwindowed, articlePlan, eqU, eqB, orOpt, andOpt, bySlug, Plan.named,
  selPred, listNames, holds_in_2, holds_in_0, holds_in_1, holds_in_3, holds_in_4,
  holds_in_5, User.row, Follow.row, Article.row, Tag.row, Favorite.row, Comment.row, int_u64])

/-! ## Helpers on a slice -/

section Helpers
variable {s s' : Snapshot} {ku : User → Bool} {kf : Follow → Bool} {kt : Tag → Bool} {kv : Favorite → Bool}
  {kc : Comment → Bool} (K : Keeps (Snapshot.toSt s) (Snapshot.toSt s') ku kf kt kv kc)
include K

theorem is_favorited_keep (a u : U64) (hk : ∀ f ∈ s.favorites.val, f.article = a → kv f) :
    is_favorited s'.favorites a u = is_favorited s.favorites a u := by
  rw [eq_ok_of_spec (is_favorited_spec _ a u), eq_ok_of_spec (is_favorited_spec _ a u)]
  have := favorited_keep K u a hk
  simp only [favorited, Snapshot.toSt] at this
  rw [this]

theorem fav_count_keep (a : U64) (hk : ∀ f ∈ s.favorites.val, f.article = a → kv f) :
    fav_count s'.favorites a = fav_count s.favorites a := by
  have := favCount_keep K a hk
  simp only [favCount, Snapshot.toSt] at this
  have h := fav_count_spec s'.favorites a
  rw [this] at h
  exact spec_congr h (fav_count_spec s.favorites a) (fun x y hx hy => UScalar.eq_of_val_eq (hx.trans hy.symm))

theorem tags_of_keep (a : U64) (hk : ∀ t ∈ s.tags.val, t.article = a → kt t) :
    tags_of s'.tags a = tags_of s.tags a := by
  have := tagsOf_keep K a hk
  simp only [tagsOf, Snapshot.toSt] at this
  have h := tags_of_spec s'.tags a
  rw [this] at h
  exact spec_congr h (tags_of_spec s.tags a) (fun x y hx hy => alloc.vec.Vec.ext _ _ (hx.trans hy.symm))

theorem author_profile_keep (u id : U64) (hk : ∀ x ∈ s.users.val, x.id = id → ku x)
    (hf : ∀ f ∈ s.follows.val, f.follower = u → kf f) :
    author_profile s' u id = author_profile s u id := by
  rw [eq_ok_of_spec (author_profile_spec _ u id), eq_ok_of_spec (author_profile_spec _ u id),
    authorOf_keep K u id hk hf]

theorem profile_keep (u : U64) (x : User) (hf : ∀ f ∈ s.follows.val, f.follower = u → kf f) :
    profile s' u x = profile s u x := by
  rw [eq_ok_of_spec (profile_spec _ u x), eq_ok_of_spec (profile_spec _ u x), profileOf_keep K u x hf]

theorem name_taken_keep (n : Option (alloc.vec.Vec U8)) (me : U64)
    (hk : ∀ x ∈ s.users.val, some x.username = n → ku x) :
    name_taken s'.users n me = name_taken s.users n me := by
  rw [eq_ok_of_spec (name_taken_spec _ n me), eq_ok_of_spec (name_taken_spec _ n me)]
  cases n with
  | none => rfl
  | some n =>
    have := userByName_keep K n (fun x hx h => hk x hx (by rw [h]))
    simp only [userByName, Snapshot.toSt] at this
    simp only [Option.bind_some, this]

theorem email_taken_keep (e : Option (alloc.vec.Vec U8)) (me : U64)
    (hk : ∀ x ∈ s.users.val, some x.email = e → ku x) :
    email_taken s'.users e me = email_taken s.users e me := by
  rw [eq_ok_of_spec (email_taken_spec _ e me), eq_ok_of_spec (email_taken_spec _ e me)]
  cases e with
  | none => rfl
  | some e =>
    have := userByEmail_keep K e (fun x hx h => hk x hx (by rw [h]))
    simp only [userByEmail, Snapshot.toSt] at this
    simp only [Option.bind_some, this]

theorem comment_views_keep (u a : U64) (hc : ∀ c ∈ s.comments.val, c.article = a → kc c)
    (hu : ∀ c ∈ s.comments.val, c.article = a → ∀ x ∈ s.users.val, x.id = c.author → ku x)
    (hf : ∀ f ∈ s.follows.val, f.follower = u → kf f) :
    comment_views s' u a = comment_views s u a := by
  have h := comment_views_spec s' u a
  rw [commentsOf_keep K u a hc hu hf] at h
  exact spec_congr h (comment_views_spec s u a) (fun x y hx hy => alloc.vec.Vec.ext _ _ (hx.trans hy.symm))

end Helpers

theorem unfavorited_flag_false (fs : alloc.vec.Vec Favorite) (a u : U64) : unfavorited_flag fs a u false = ok false := by
  rw [eq_ok_of_spec (unfavorited_flag_spec fs a u false)]; simp

/-! ## Commands -/

section Commands
variable (a : Principal) (s : Snapshot)

theorem eq_of_specs {α : Type} {m m' : Result α} {R : α} (h : m ⦃ fun r => r = R ⦄) (h' : m' ⦃ fun r => r = R ⦄) :
    m = m' :=
  spec_congr h h' (fun x y hx hy => hx.trans hy.symm)

theorem current_user_frame (u : U64) (plan : List (Read Val)) (hp : plan = [⟨0, eqU 0 u, {}⟩]) :
    current_user (Snapshot.load plan s) u = current_user s u := by
  have hw : Unwindowed plan := by subst hp; holds_simp
  have K := keeps hw s
  have h := current_user_spec (Snapshot.load plan s) u
  rw [userById_keep K u (fun x _ hx => by subst hp; holds_simp; exact hx)] at h
  exact eq_of_specs h (current_user_spec s u)

theorem get_profile_frame (u : U64) (n : alloc.vec.Vec U8) (plan : List (Read Val))
    (hp : plan = [⟨0, eqB 1 n, {}⟩, ⟨1, eqU 0 u, {}⟩]) :
    get_profile (Snapshot.load plan s) u n = get_profile s u n := by
  have hw : Unwindowed plan := by subst hp; holds_simp
  have K := keeps hw s
  have hf : ∀ f ∈ (Snapshot.toSt s).follows, f.follower = u → keepOf plan (Snapshot.enc s) 1 Follow.row f :=
    fun f _ hf => by subst hp; holds_simp; exact hf
  have h := get_profile_spec (Snapshot.load plan s) u n
  rw [userByName_keep K n (fun x _ hx => by subst hp; holds_simp; exact hx)] at h
  simp only [profileOf_keep K u _ hf] at h
  exact eq_of_specs h (get_profile_spec s u n)

theorem follow_frame (u : U64) (n : alloc.vec.Vec U8) (plan : List (Read Val))
    (hp : plan = [⟨0, .or (eqU 0 u) (eqB 1 n), {}⟩]) :
    follow (Snapshot.load plan s) u n = follow s u n ∧ unfollow (Snapshot.load plan s) u n = unfollow s u n := by
  have hw : Unwindowed plan := by subst hp; holds_simp
  have K := keeps hw s
  have hU : (Snapshot.load plan s).users.val.find? (fun x => decide (x.id = u)) =
      s.users.val.find? (fun x => decide (x.id = u)) :=
    userById_keep K u (fun x _ hx => by subst hp; holds_simp; simp [hx])
  have hN : (Snapshot.load plan s).users.val.find? (fun x => decide (x.username = n)) =
      s.users.val.find? (fun x => decide (x.username = n)) :=
    userByName_keep K n (fun x _ hx => by subst hp; holds_simp; simp [hx])
  refine ⟨?_, ?_⟩
  · unfold follow; simp only [find_user_eq, find_user_by_name_eq, hU, hN]
  · unfold unfollow; simp only [find_user_eq, find_user_by_name_eq, hU, hN]

/-- The plan reads articles `artsOf` keeps by `p`, with no window. -/
theorem articleBySlug_load (plan : List (Read Val)) (slug : alloc.vec.Vec U8)
    (hA : artsOf plan s = s.articles.val.filter (fun y => (bySlug slug).holds (Snapshot.enc s) (Article.row y))) :
    articleBySlug (Snapshot.toSt (Snapshot.load plan s)) slug = articleBySlug (Snapshot.toSt s) slug := by
  simp only [articleBySlug, toSt_articles, hA]
  exact find?_slice _ _ _ (fun y _ hy => by holds_simp; simpa using hy)

theorem get_article_frame (u : U64) (slug : alloc.vec.Vec U8) (plan : List (Read Val))
    (hp : plan = articlePlan u (.Slug slug) {} none) :
    get_article (Snapshot.load plan s) u slug false = get_article s u slug false := by
  have hw : Unwindowed plan := by subst hp; holds_simp
  have K := keeps hw s
  have hA : artsOf plan s = s.articles.val.filter (fun y => (bySlug slug).holds (Snapshot.enc s) (Article.row y)) := by
    subst hp; simp [artsOf, pickRows, articlePlan, selPred]
  have h := get_article_spec (Snapshot.load plan s) u slug false
  rw [articleBySlug_load s plan slug hA] at h
  have h' := get_article_spec s u slug false
  cases ha : articleBySlug (Snapshot.toSt s) slug with
  | none => simp only [ha] at h h'; exact eq_of_specs h h'
  | some a =>
    simp only [ha] at h h'
    obtain ⟨hmem, hslug⟩ := Theorems.articleBySlug_some ha
    rw [viewWith_false, view_keep K u a
      (fun t _ ht => by subst hp; holds_simp; exact ⟨a, hmem, hslug, ht.symm⟩)
      (fun f _ hf => by subst hp; holds_simp; exact ⟨a, hmem, hslug, hf.symm⟩)
      (fun x _ hx => by subst hp; holds_simp; exact ⟨a, hmem, hslug, hx.symm⟩)
      (fun f _ hf => by subst hp; holds_simp; exact hf)] at h
    exact spec_congr h h' (fun x y ⟨v, hx, hv⟩ ⟨v', hy, hv'⟩ => by
      subst hx hy; rw [toView_inj (hv.trans hv'.symm)])

theorem favorite_frame (u : U64) (slug : alloc.vec.Vec U8) (plan : List (Read Val))
    (hp : plan = articlePlan u (.Slug slug) {} (some (eqU 0 u))) :
    favorite (Snapshot.load plan s) u slug = favorite s u slug ∧
      unfavorite (Snapshot.load plan s) u slug false = unfavorite s u slug false := by
  have hw : Unwindowed plan := by subst hp; holds_simp
  have K := keeps hw s
  have hA : artsOf plan s = s.articles.val.filter (fun y => (bySlug slug).holds (Snapshot.enc s) (Article.row y)) := by
    subst hp; simp [artsOf, pickRows, articlePlan, selPred]
  have hU : (Snapshot.load plan s).users.val.find? (fun x => decide (x.id = u)) =
      s.users.val.find? (fun x => decide (x.id = u)) :=
    userById_keep K u (fun x _ hx => by subst hp; holds_simp; simp [hx])
  have hS : (Snapshot.load plan s).articles.val.find? (fun x => decide (x.slug = slug)) =
      s.articles.val.find? (fun x => decide (x.slug = slug)) :=
    articleBySlug_load s plan slug hA
  have hrest : ∀ me a, s.users.val.find? (fun x => decide (x.id = u)) = some me →
      s.articles.val.find? (fun x => decide (x.slug = slug)) = some a →
      is_favorited (Snapshot.load plan s).favorites a.id me.id = is_favorited s.favorites a.id me.id ∧
      fav_count (Snapshot.load plan s).favorites a.id = fav_count s.favorites a.id ∧
      tags_of (Snapshot.load plan s).tags a.id = tags_of s.tags a.id ∧
      author_profile (Snapshot.load plan s) me.id a.author = author_profile s me.id a.author := by
    intro me a hu ha
    obtain ⟨hmem, hslug⟩ := Theorems.articleBySlug_some (s := Snapshot.toSt s) ha
    obtain ⟨-, hme⟩ := Theorems.userById_some (s := Snapshot.toSt s) hu
    exact ⟨is_favorited_keep K _ _ (fun f _ hf => by subst hp; holds_simp; exact ⟨a, hmem, hslug, hf.symm⟩),
      fav_count_keep K _ (fun f _ hf => by subst hp; holds_simp; exact ⟨a, hmem, hslug, hf.symm⟩),
      tags_of_keep K _ (fun t _ ht => by subst hp; holds_simp; exact ⟨a, hmem, hslug, ht.symm⟩),
      author_profile_keep K _ _ (fun x _ hx => by subst hp; holds_simp; exact Or.inl ⟨a, hmem, hslug, hx.symm⟩)
        (fun f _ hf => by subst hp; holds_simp; rw [hf, hme])⟩
  refine ⟨?_, ?_⟩
  · unfold favorite
    simp only [find_user_eq, find_article_eq, hU, hS, bind_ok]
    split
    next hu => simp only [hu]
    next me hu =>
      split
      next ha => simp only [ha]
      next a ha =>
        obtain ⟨h1, h2, h3, h4⟩ := hrest me a hu ha
        simp only [ha, h1, h2, h3, h4]
  · unfold unfavorite
    simp only [find_user_eq, find_article_eq, hU, hS, unfavorited_flag_false, bind_ok]
    split
    next hu => simp only [hu]
    next me hu =>
      split
      next ha => simp only [ha]
      next a ha =>
        obtain ⟨h1, h2, h3, h4⟩ := hrest me a hu ha
        simp only [ha, h1, h2, h3, h4]

theorem update_user_frame (u : U64) (e n p b i : Option (alloc.vec.Vec U8)) (plan : List (Read Val))
    (hp : plan = [⟨0, orOpt (orOpt (eqU 0 u) (n.map (eqB 1))) (e.map (eqB 2)), {}⟩]) :
    update_user (Snapshot.load plan s) u e n p b i = update_user s u e n p b i := by
  have hw : Unwindowed plan := by subst hp; holds_simp
  have K := keeps hw s
  have hU : (Snapshot.load plan s).users.val.find? (fun x => decide (x.id = u)) =
      s.users.val.find? (fun x => decide (x.id = u)) :=
    userById_keep K u (fun x _ hx => by subst hp; cases e <;> cases n <;> holds_simp <;> simp [hx])
  have hN := fun me => name_taken_keep K n me
    (fun x _ hx => by subst hp; subst hx; cases e <;> holds_simp)
  have hE := fun me => email_taken_keep K e me
    (fun x _ hx => by subst hp; subst hx; cases n <;> holds_simp)
  unfold update_user
  simp only [find_user_eq, hU, bind_ok, hN, hE]

theorem get_comments_frame (u : U64) (slug : alloc.vec.Vec U8) (plan : List (Read Val))
    (hp : plan = [⟨0, .isIn 0 5 (.isIn 1 2 (bySlug slug) 0 {}) 2 {}, {}⟩, ⟨1, eqU 0 u, {}⟩, ⟨2, bySlug slug, {}⟩,
      ⟨5, .isIn 1 2 (bySlug slug) 0 {}, {}⟩]) :
    get_comments (Snapshot.load plan s) u slug = get_comments s u slug := by
  have hw : Unwindowed plan := by subst hp; holds_simp
  have K := keeps hw s
  have hA : artsOf plan s = s.articles.val.filter (fun y => (bySlug slug).holds (Snapshot.enc s) (Article.row y)) := by
    subst hp; simp [artsOf, pickRows]
  have hS : (Snapshot.load plan s).articles.val.find? (fun x => decide (x.slug = slug)) =
      s.articles.val.find? (fun x => decide (x.slug = slug)) :=
    articleBySlug_load s plan slug hA
  unfold get_comments
  simp only [find_article_eq, hS, bind_ok]
  split
  next ha => simp only [ha]
  next a ha =>
    simp only [ha]
    obtain ⟨hmem, hslug⟩ := Theorems.articleBySlug_some (s := Snapshot.toSt s) ha
    rw [comment_views_keep K u a.id
      (fun c _ hc => by subst hp; holds_simp; exact ⟨a, hmem, hslug, hc.symm⟩)
      (fun c hcm hc x _ hx => by subst hp; holds_simp; exact ⟨c, hcm, ⟨a, hmem, hslug, hc.symm⟩, hx.symm⟩)
      (fun f _ hf => by subst hp; holds_simp; exact hf)]

theorem add_comment_frame (u : U64) (slug body : alloc.vec.Vec U8) (now : U64) (plan : List (Read Val))
    (hp : plan = [⟨0, eqU 0 u, {}⟩, ⟨1, eqU 0 u, {}⟩, ⟨2, bySlug slug, {}⟩, ⟨6, .all, {}⟩]) :
    add_comment (Snapshot.load plan s) u slug body now = add_comment s u slug body now := by
  have hw : Unwindowed plan := by subst hp; holds_simp
  have K := keeps hw s
  have hA : artsOf plan s = s.articles.val.filter (fun y => (bySlug slug).holds (Snapshot.enc s) (Article.row y)) := by
    subst hp; simp [artsOf, pickRows]
  have hU : (Snapshot.load plan s).users.val.find? (fun x => decide (x.id = u)) =
      s.users.val.find? (fun x => decide (x.id = u)) :=
    userById_keep K u (fun x _ hx => by subst hp; holds_simp; simp [hx])
  have hS : (Snapshot.load plan s).articles.val.find? (fun x => decide (x.slug = slug)) =
      s.articles.val.find? (fun x => decide (x.slug = slug)) :=
    articleBySlug_load s plan slug hA
  have hC : (Snapshot.load plan s).counter = s.counter := by subst hp; simp [Snapshot.load, pickRows]
  unfold add_comment
  simp only [find_user_eq, find_article_eq, hU, hS, hC, bind_ok]
  split
  next hu => simp only [hu]
  next me hu =>
    obtain ⟨-, hme⟩ := Theorems.userById_some (s := Snapshot.toSt s) hu
    simp only [profile_keep K me.id me (fun f _ hf => by subst hp; holds_simp; rw [hf, hme])]

theorem delete_comment_frame (u : U64) (slug : alloc.vec.Vec U8) (id : U64) (plan : List (Read Val))
    (hp : plan = [⟨0, eqU 0 u, {}⟩, ⟨2, bySlug slug, {}⟩, ⟨5, eqU 0 id, {}⟩]) :
    delete_comment (Snapshot.load plan s) u slug id = delete_comment s u slug id := by
  have hw : Unwindowed plan := by subst hp; holds_simp
  have K := keeps hw s
  have hA : artsOf plan s = s.articles.val.filter (fun y => (bySlug slug).holds (Snapshot.enc s) (Article.row y)) := by
    subst hp; simp [artsOf, pickRows]
  have hU : (Snapshot.load plan s).users.val.find? (fun x => decide (x.id = u)) =
      s.users.val.find? (fun x => decide (x.id = u)) :=
    userById_keep K u (fun x _ hx => by subst hp; holds_simp; simp [hx])
  have hS : (Snapshot.load plan s).articles.val.find? (fun x => decide (x.slug = slug)) =
      s.articles.val.find? (fun x => decide (x.slug = slug)) :=
    articleBySlug_load s plan slug hA
  have hC : (Snapshot.load plan s).comments.val.find? (fun x => decide (x.id = id)) =
      s.comments.val.find? (fun x => decide (x.id = id)) := by
    have := K.comments
    simp only [Snapshot.toSt] at this
    rw [this]
    exact find?_slice _ _ _ (fun c _ hc => by subst hp; holds_simp; simpa using hc)
  unfold delete_comment
  simp only [find_user_eq, find_article_eq, find_comment_eq, hU, hS, hC, bind_ok]

theorem selPred_feed (u : U64) (y : Article) :
    (selPred (.Feed u)).holds (Snapshot.enc s) (Article.row y) = follows (Snapshot.toSt s) u y.author := by
  holds_simp; try simp [follows, Snapshot.toSt]

theorem feed_frame (u lim off : U64) (plan : List (Read Val))
    (hp : plan = articlePlan u (.Feed u) (newestW off lim) (some (eqU 0 u))) :
    feed (Snapshot.load plan s) u lim off false = feed s u lim off false := by
  have hw : Unwindowed plan := by subst hp; holds_simp
  have K := keeps hw s
  have hF0 : s.articles.val.filter (fun y => (selPred (.Feed u)).holds (Snapshot.enc s) (Article.row y)) =
      (Snapshot.toSt s).articles.filter (fun a => follows (Snapshot.toSt s) u a.author) := by
    apply List.filter_congr; intro y _; rw [selPred_feed]
  have hA : (Snapshot.toSt (Snapshot.load plan s)).articles =
      (newestW off lim).apply ((Snapshot.toSt s).articles.filter (fun a => follows (Snapshot.toSt s) u a.author)) := by
    subst hp; simp only [toSt_articles, artsOf, pickRows, articlePlan, List.find?]; simp [hF0]
  have hkt : ∀ y ∈ (newestW off lim).apply ((Snapshot.toSt s).articles.filter (fun a => follows (Snapshot.toSt s) u a.author)),
      ∀ t ∈ (Snapshot.toSt s).tags, t.article = y.id → keepOf plan (Snapshot.enc s) 3 Tag.row t := by
    intro y hy t _ ht; subst hp
    simp [keepOf, articlePlan, holds_in_2, hF0, Tag.row]
    exact ⟨y, hy, by simp [Article.row, ht]⟩
  have hkv : ∀ y ∈ (newestW off lim).apply ((Snapshot.toSt s).articles.filter (fun a => follows (Snapshot.toSt s) u a.author)),
      ∀ f ∈ (Snapshot.toSt s).favs, f.article = y.id → keepOf plan (Snapshot.enc s) 4 Favorite.row f := by
    intro y hy f _ hf; subst hp
    simp [keepOf, articlePlan, holds_in_2, hF0, Favorite.row]
    exact ⟨y, hy, by simp [Article.row, hf]⟩
  have hku : ∀ y ∈ (newestW off lim).apply ((Snapshot.toSt s).articles.filter (fun a => follows (Snapshot.toSt s) u a.author)),
      ∀ x ∈ (Snapshot.toSt s).users, x.id = y.author → keepOf plan (Snapshot.enc s) 0 User.row x := by
    intro y hy x _ hx; subst hp
    simp [keepOf, articlePlan, orOpt, holds_in_2, hF0, User.row]
    exact Or.inl ⟨y, hy, by simp [Article.row, hx]⟩
  have hkf : ∀ f ∈ (Snapshot.toSt s).follows, f.follower = u → keepOf plan (Snapshot.enc s) 1 Follow.row f := by
    intro f _ hf; subst hp; holds_simp; exact hf
  have hU : userById (Snapshot.toSt (Snapshot.load plan s)) u = userById (Snapshot.toSt s) u :=
    userById_keep K u (fun x _ hx => by subst hp; simp [keepOf, articlePlan, orOpt, eqU, User.row, hx])
  have h := feed_spec (Snapshot.load plan s) u lim off false
  rw [hU] at h
  have h' := feed_spec s u lim off false
  cases hme : userById (Snapshot.toSt s) u with
  | none => simp only [hme] at h h'; exact eq_of_specs h h'
  | some me =>
    simp only [hme] at h h'
    obtain ⟨-, hid⟩ := Theorems.userById_some hme
    have hF : feedWith false (Snapshot.toSt (Snapshot.load plan s)) me.id lim.val off.val =
        feedWith false (Snapshot.toSt s) me.id lim.val off.val := by
      simp only [feedWith_false, feedOf, hid]
      exact page_views_keep K u _ _ off lim hA
        (fun y hy => by
          rw [follows_keep K u y.author hkf]
          exact (List.mem_filter.1 (Window.mem_apply hy)).2)
        hkt hkv hku hkf
    rw [hF] at h
    exact spec_congr h h' (fun x y ⟨vs, hx, hv⟩ ⟨vs', hy, hv'⟩ => by
      subst hx hy; rw [toViews_inj (hv.trans hv'.symm)])

/-- With unique usernames, the users named `n` are the one `userByName` finds. -/
theorem named_unique {S : St} (hn : (S.users.map (·.username)).Nodup) {n : alloc.vec.Vec U8} {x : User}
    (hx : userByName S n = some x) (z : User) (hz : z ∈ S.users) (hzn : z.username = n) : z = x := by
  obtain ⟨hxm, hxn⟩ := Theorems.userByName_some hx
  exact List.inj_on_of_nodup_map hn hz hxm (hzn.trans hxn.symm)

theorem any_named {S : St} (hn : (S.users.map (·.username)).Nodup) {n : alloc.vec.Vec U8} {x : User}
    (hx : userByName S n = some x) (v : U64) :
    (S.users.any fun z => decide (z.username = n) && decide (z.id = v)) = decide (v = x.id) := by
  obtain ⟨hxm, hxn⟩ := Theorems.userByName_some hx
  apply Bool.eq_iff_iff.2
  simp only [List.any_eq_true, Bool.and_eq_true, decide_eq_true_eq]
  constructor
  · rintro ⟨z, hz, hzn, rfl⟩; rw [named_unique hn hx z hz hzn]
  · rintro rfl; exact ⟨x, hxm, hxn, rfl⟩

theorem resolve_some {S : St} {n : alloc.vec.Vec U8} {a : Option U64} (h : resolveName S (some n) = some a) :
    ∃ x, userByName S n = some x ∧ a = some x.id := by
  simp only [resolveName, Option.map_eq_some_iff] at h
  obtain ⟨x, hx, rfl⟩ := h; exact ⟨x, hx, rfl⟩

theorem selPred_list (hn : ((Snapshot.toSt s).users.map (·.username)).Nodup) (tag au fav : Option (alloc.vec.Vec U8))
    (a f : Option U64) (ha : resolveName (Snapshot.toSt s) au = some a) (hf : resolveName (Snapshot.toSt s) fav = some f)
    (y : Article) :
    (selPred (.List tag au fav)).holds (Snapshot.enc s) (Article.row y) = listed (Snapshot.toSt s) tag a f y := by
  have hA : ∀ n x, userByName (Snapshot.toSt s) n = some x → ∀ v : U64,
      ((↑s.users : List User).any fun z => decide (z.id = v) && decide (z.username = n)) = decide (v = x.id) := by
    intro n x hx v
    have := any_named hn hx v
    simp only [Snapshot.toSt] at this
    rw [← this]; congr 1; funext z; rw [Bool.and_comm]
  cases au with
  | none =>
    simp only [resolveName, Option.some.injEq] at ha; subst ha
    cases fav with
    | none =>
      simp only [resolveName, Option.some.injEq] at hf; subst hf
      cases tag <;> holds_simp <;> simp [listed, Snapshot.toSt, Bool.and_comm]
    | some fn =>
      obtain ⟨fx, hfx, rfl⟩ := resolve_some hf
      cases tag <;> holds_simp <;> simp [listed, favorited, Snapshot.toSt, hA fn fx hfx, Bool.and_comm, Bool.and_left_comm, Bool.and_assoc]
  | some an =>
    obtain ⟨ax, hax, rfl⟩ := resolve_some ha
    cases fav with
    | none =>
      simp only [resolveName, Option.some.injEq] at hf; subst hf
      cases tag <;> holds_simp <;> simp [listed, Snapshot.toSt, hA an ax hax, Bool.and_comm, Bool.and_left_comm, Bool.and_assoc]
    | some fn =>
      obtain ⟨fx, hfx, rfl⟩ := resolve_some hf
      cases tag <;> holds_simp <;>
        simp [listed, favorited, Snapshot.toSt, hA an ax hax, hA fn fx hfx, Bool.and_comm, Bool.and_left_comm, Bool.and_assoc]

theorem list_articles_frame (hn : ((Snapshot.toSt s).users.map (·.username)).Nodup) (u : U64)
    (tag au fav : Option (alloc.vec.Vec U8)) (lim off : U64) (plan : List (Read Val))
    (hp : plan = articlePlan u (.List tag au fav) (newestW off lim) (listNames au fav)) :
    list_articles (Snapshot.load plan s) u tag au fav lim off false = list_articles s u tag au fav lim off false := by
  have hw : Unwindowed plan := by subst hp; holds_simp
  have K := keeps hw s
  have hRa : resolveName (Snapshot.toSt (Snapshot.load plan s)) au = resolveName (Snapshot.toSt s) au := by
    cases au with
    | none => rfl
    | some n =>
      simp only [resolveName]
      rw [userByName_keep K n (fun x _ hx => by
        subst hp; cases fav <;> simp_all [keepOf, articlePlan, listNames, orOpt, eqB, User.row])]
  have hRf : resolveName (Snapshot.toSt (Snapshot.load plan s)) fav = resolveName (Snapshot.toSt s) fav := by
    cases fav with
    | none => rfl
    | some n =>
      simp only [resolveName]
      rw [userByName_keep K n (fun x _ hx => by
        subst hp; cases au <;> simp_all [keepOf, articlePlan, listNames, orOpt, eqB, User.row])]
  have h := list_articles_spec (Snapshot.load plan s) u tag au fav lim off false
  have h' := list_articles_spec s u tag au fav lim off false
  have hL : listing (Snapshot.toSt (Snapshot.load plan s)) u tag au fav lim.val off.val =
      listing (Snapshot.toSt s) u tag au fav lim.val off.val := by
    unfold listing
    rw [hRa, hRf]
    cases ha : resolveName (Snapshot.toSt s) au with
    | none => rfl
    | some a =>
      cases hf : resolveName (Snapshot.toSt s) fav with
      | none => rfl
      | some f =>
        simp only []
        have hF0 : s.articles.val.filter (fun y => (selPred (.List tag au fav)).holds (Snapshot.enc s) (Article.row y)) =
            (Snapshot.toSt s).articles.filter (listed (Snapshot.toSt s) tag a f) := by
          apply List.filter_congr; intro y _; rw [selPred_list s hn tag au fav a f ha hf]
        have hA : (Snapshot.toSt (Snapshot.load plan s)).articles =
            (newestW off lim).apply ((Snapshot.toSt s).articles.filter (listed (Snapshot.toSt s) tag a f)) := by
          subst hp; simp only [toSt_articles, artsOf, pickRows, articlePlan, List.find?]; simp [hF0]
        have hkt : ∀ y ∈ (newestW off lim).apply ((Snapshot.toSt s).articles.filter (listed (Snapshot.toSt s) tag a f)),
            ∀ t ∈ (Snapshot.toSt s).tags, t.article = y.id → keepOf plan (Snapshot.enc s) 3 Tag.row t := by
          intro y hy t _ ht; subst hp
          simp [keepOf, articlePlan, holds_in_2, hF0, Tag.row]
          exact ⟨y, hy, by simp [Article.row, ht]⟩
        have hkv : ∀ y ∈ (newestW off lim).apply ((Snapshot.toSt s).articles.filter (listed (Snapshot.toSt s) tag a f)),
            ∀ g ∈ (Snapshot.toSt s).favs, g.article = y.id → keepOf plan (Snapshot.enc s) 4 Favorite.row g := by
          intro y hy g _ hg; subst hp
          simp [keepOf, articlePlan, holds_in_2, hF0, Favorite.row]
          exact ⟨y, hy, by simp [Article.row, hg]⟩
        have hku : ∀ y ∈ (newestW off lim).apply ((Snapshot.toSt s).articles.filter (listed (Snapshot.toSt s) tag a f)),
            ∀ x ∈ (Snapshot.toSt s).users, x.id = y.author → keepOf plan (Snapshot.enc s) 0 User.row x := by
          intro y hy x _ hx; subst hp
          have : (Pred.isIn 0 2 (selPred (.List tag au fav)) 1 (newestW off lim)).holds (Snapshot.enc s) (User.row x) := by
            simp [holds_in_2, hF0, User.row]
            exact ⟨y, hy, by simp [Article.row, hx]⟩
          cases hn : listNames au fav <;> simp [keepOf, articlePlan, orOpt, hn, this]
        have hkf : ∀ g ∈ (Snapshot.toSt s).follows, g.follower = u → keepOf plan (Snapshot.enc s) 1 Follow.row g := by
          intro g _ hg; subst hp; holds_simp; exact hg
        exact page_views_keep K u _ _ off lim hA
          (fun y hy => by
            have hy' := (List.mem_filter.1 (Window.mem_apply hy)).2
            rw [← hy']
            simp only [listed]
            congr 1
            · congr 1
              cases tag with
              | none => rfl
              | some t =>
                simp only [Option.all]
                rw [K.tags]
                exact any_slice _ _ _ (fun x hx hp' => hkt y hy x hx (by simp at hp'; exact hp'.1))
            · cases f with
              | none => rfl
              | some v => simp only [Option.all]; exact favorited_keep K v y.id (hkv y hy))
          hkt hkv hku hkf
  rw [listingWith_false, hL] at h
  rw [listingWith_false] at h'
  exact spec_congr h h' (fun x y ⟨vs, hx, hv⟩ ⟨vs', hy, hv'⟩ => by
    subst hx hy; rw [toViews_inj (hv.trans hv'.symm)])

end Commands

/-! ## The frame theorem -/

/-- On what its plan loads, a command gives the result it gives on the
whole site. Usernames must be unique (`Inv.usernames`). -/
theorem frame (a : Principal) (s : Snapshot) (c : Command) (plan : List (Read Val)) (hp : readsA a c = some plan)
    (hn : ((Snapshot.toSt s).users.map (·.username)).Nodup) :
    transition a (Snapshot.load plan s) c = transition a s c := by
  cases c <;> simp only [readsA, Option.some.injEq, reduceCtorEq] at hp <;> simp only [transition, step]
  case CurrentUser => exact current_user_frame s _ plan hp.symm
  case UpdateUser e n p b i => exact update_user_frame s _ e n p b i plan hp.symm
  case GetProfile n => exact get_profile_frame s _ n plan hp.symm
  case Follow n => exact (follow_frame s _ n plan hp.symm).1
  case Unfollow n => exact (follow_frame s _ n plan hp.symm).2
  case ListArticles tag au fav lim off => exact list_articles_frame s hn _ tag au fav lim off plan hp.symm
  case Feed lim off => exact feed_frame s _ lim off plan hp.symm
  case GetArticle slug => exact get_article_frame s _ slug plan hp.symm
  case Favorite slug => exact (favorite_frame s _ slug plan hp.symm).1
  case Unfavorite slug => exact (favorite_frame s _ slug plan hp.symm).2
  case GetComments slug => exact get_comments_frame s _ slug plan hp.symm
  case AddComment slug body now => exact add_comment_frame s _ slug body now plan hp.symm
  case DeleteComment slug id => exact delete_comment_frame s _ slug id plan hp.symm

/-- End to end: when `reads a c` names a plan, rows that hold what each of its
reads returns from the tables of `s` decode to a snapshot on which
`transition` gives the same result as on `s`. This is what `load_for` does,
given that PostgreSQL returns `Read.rows` for each compiled read (tested). -/
theorem load_for_sound (a : Principal) (s : Snapshot) (c : Command) (v : alloc.vec.Vec h5i_app_sql.Read)
    (hr : reads a c = ok (some v)) (hn : ((Snapshot.toSt s).users.map (·.username)).Nodup) (r : Rows)
    (hrows : ∀ t, t < 7 → Rows.tabs r t = load (v.val.map Read.abs) (Snapshot.enc s) t) :
    ∃ s', decode r = ok (some s') ∧ transition a s' c = transition a s c := by
  obtain ⟨o, ho, hpl⟩ := reads_abs a c
  rw [hr] at ho
  have ho' := (Result.ok.inj ho).symm
  subst ho'
  exact ⟨_, Schema.decode_load _ s r hrows, frame a s c _ (by rw [← hpl]; rfl) hn⟩

/-- The same for the states the proofs are about: in a state satisfying the
invariants (every reachable one, `Invariants.reachable_inv`). -/
theorem load_for_sound_inv (a : Principal) (s : Snapshot) (c : Command) (v : alloc.vec.Vec h5i_app_sql.Read)
    (hr : reads a c = ok (some v)) (hi : Inv (Snapshot.toSt s)) (r : Rows)
    (hrows : ∀ t, t < 7 → Rows.tabs r t = load (v.val.map Read.abs) (Snapshot.enc s) t) :
    ∃ s', decode r = ok (some s') ∧ transition a s' c = transition a s c :=
  load_for_sound a s c v hr hi.usernames r hrows

end conduit_kernel.Frame
