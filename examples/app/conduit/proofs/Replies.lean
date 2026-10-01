import Invariants
/-!
# What replies contain

Reads reply with the spec function (`view`, `listing`, `feedOf`,
`commentsOf`, `profileOf`, `allTags`). Writes reply with what that read shows
after the write, so `favorited`, `favoritesCount` and `following` are right
for the caller.
-/
open Aeneas Aeneas.Std Result conduit_kernel conduit_kernel.Spec conduit_kernel.Helpers
  conduit_kernel.Commands conduit_kernel.Theorems conduit_kernel.Invariants H5iAppLib

namespace conduit_kernel.Replies

/-! ## Reads -/

theorem get_article_reply (a : Principal) (s : Snapshot) (slug : Text) :
    transition a s (.GetArticle slug) ⦃ r => match articleBySlug (Snapshot.toSt s) slug with
      | none => r = .Err .NotFound
      | some art => ∃ v, r = .Ok (alloc.vec.Vec.new Write, .Article v) ∧ v.toView = view (Snapshot.toSt s) a.user art ⦄ := by
  simp only [transition, step]; exact get_article_spec s a.user slug false

theorem list_reply (a : Principal) (s : Snapshot) (tag author fav : Option Text) (lim off : U64) :
    transition a s (.ListArticles tag author fav lim off) ⦃ r => ∃ vs, r = .Ok (alloc.vec.Vec.new Write, .Articles vs) ∧
      vs.val.map ArticleView.toView = listing (Snapshot.toSt s) a.user tag author fav lim.val off.val ⦄ := by
  simp only [transition, step]; exact list_articles_spec s a.user tag author fav lim off false

theorem feed_reply (a : Principal) (s : Snapshot) (lim off : U64) :
    transition a s (.Feed lim off) ⦃ r => match userById (Snapshot.toSt s) a.user with
      | none => r = .Err .Unauthorized
      | some _ => ∃ vs, r = .Ok (alloc.vec.Vec.new Write, .Articles vs) ∧
          vs.val.map ArticleView.toView = feedOf (Snapshot.toSt s) a.user lim.val off.val ⦄ := by
  have h := feed_spec s a.user lim off false
  simp only [transition, step]
  apply WP.spec_mono h
  intro r hr
  split at hr
  · rename_i heq; rw [heq]; exact hr
  · rename_i me heq; rw [heq]; simpa [(userById_some heq).2] using hr

theorem get_comments_reply (a : Principal) (s : Snapshot) (slug : Text) :
    transition a s (.GetComments slug) ⦃ r => match articleBySlug (Snapshot.toSt s) slug with
      | none => r = .Err .NotFound
      | some art => ∃ cs, r = .Ok (alloc.vec.Vec.new Write, .Comments cs) ∧ cs.val = commentsOf (Snapshot.toSt s) a.user art.id ⦄ := by
  simp only [transition, step]; exact get_comments_spec s a.user slug

theorem get_profile_reply (a : Principal) (s : Snapshot) (n : Text) :
    transition a s (.GetProfile n) ⦃ r => r = match userByName (Snapshot.toSt s) n with
      | none => .Err .NotFound
      | some x => .Ok (alloc.vec.Vec.new Write, .Profile (profileOf (Snapshot.toSt s) a.user x)) ⦄ := by
  simp only [transition, step]; exact get_profile_spec s a.user n

/-- Logging in needs the stored password hash. -/
theorem login_reply (a : Principal) (s : Snapshot) (e p : Text) :
    transition a s (.Login e p) ⦃ r => r = match userByEmail (Snapshot.toSt s) e with
      | none => .Err .UnknownEmail
      | some x => if x.password = p then .Ok (alloc.vec.Vec.new Write, .Account (acct x)) else .Err .WrongPassword ⦄ := by
  simp only [transition, step]; exact login_spec s e p

/-- The feed shows only articles, by authors the caller follows. -/
theorem feed_only_followed (a : Principal) (s : Snapshot) (lim off : U64) ws vs
    (h : transition a s (.Feed lim off) = .ok (.Ok (ws, .Articles vs))) :
    ∀ v ∈ vs.val, v.article ∈ (Snapshot.toSt s).articles ∧ follows (Snapshot.toSt s) a.user v.article.author := by
  have hs := post_of_ok (feed_reply a s lim off) h
  split at hs
  · simp at hs
  · obtain ⟨vs', hr, hv⟩ := hs
    simp only [core.result.Result.Ok.injEq, Prod.mk.injEq, Reply.Articles.injEq] at hr
    obtain ⟨-, rfl⟩ := hr
    intro v hmem
    have : v.toView ∈ feedOf (Snapshot.toSt s) a.user lim.val off.val := hv ▸ List.mem_map_of_mem hmem
    simp only [feedOf, view, List.mem_map, pageOf] at this
    obtain ⟨art, hart, heq⟩ := this
    have hart := List.mem_filter.1 (List.mem_reverse.1 (List.mem_of_mem_drop (List.mem_of_mem_take hart)))
    have : v.article = art := by have := congrArg View.article heq; simpa [ArticleView.toView] using this.symm
    rw [this]; exact hart

/-! ## Writes: the reply shows the state after the write -/

theorem profile_same {s t : St} (hu : s.users = t.users) (hf : s.follows = t.follows) (v id : U64) :
    authorOf s v id = authorOf t v id := by
  simp [authorOf, userById, profileOf, follows, hu, hf]

/-- Favoriting replies with the article as it reads afterwards: `favorited`
is true and the count includes the caller once. -/
theorem favorite_reply (a : Principal) (s : Snapshot) (slug : Text) ws v
    (h : transition a s (.Favorite slug) = .ok (.Ok (ws, .Article v))) :
    articleBySlug (Snapshot.toSt s) slug = some v.article ∧
      v.toView = view (applyAll (Snapshot.toSt s) ws.val) a.user v.article := by
  simp only [transition, step] at h
  obtain ⟨me, art, hme, hart, hws, v', hv', hview⟩ := post_of_ok (favorite_spec s a.user slug) h ws _ rfl
  simp only [Reply.Article.injEq] at hv'; subst hv'
  have hmu := (userById_some hme).2
  have hva : v.article = art := by have := congrArg View.article hview; simpa [ArticleView.toView] using this
  refine ⟨hva ▸ hart, ?_⟩
  rw [hview, hws, hva, ← hmu]
  simp only [applyAll, List.foldl_cons, List.foldl_nil, applyWrite, view, View.mk.injEq]
  refine ⟨trivial, rfl, ?_, ?_, rfl⟩
  · symm; simp only [favorited, List.any_eq_true, decide_eq_true_eq]
    exact ⟨_, mem_upsert_self _ _ _, rfl, rfl⟩
  · symm; simp only [favCount]
    rw [length_filter_upsert _ _ _ _ (fun y hy => by simp only [Prod.mk.injEq] at hy; simp [hy.1])]
    simp [favorited]

/-- Unfavoriting replies with the article as it reads afterwards: not
favorited, and the count without the caller. -/
theorem unfavorite_reply (a : Principal) (s : Snapshot) (slug : Text) ws v
    (hr : Reachable (Snapshot.toSt s))
    (h : transition a s (.Unfavorite slug) = .ok (.Ok (ws, .Article v))) :
    articleBySlug (Snapshot.toSt s) slug = some v.article ∧
      v.toView = view (applyAll (Snapshot.toSt s) ws.val) a.user v.article := by
  have hinv := reachable_inv hr
  simp only [transition, step] at h
  obtain ⟨me, art, hme, hart, hws, v', hv', hview⟩ := post_of_ok (unfavorite_spec s a.user slug false) h ws _ rfl
  simp only [Reply.Article.injEq] at hv'; subst hv'
  have hmu := (userById_some hme).2
  have hva : v.article = art := by have := congrArg View.article hview; simpa [ArticleView.toView] using this
  refine ⟨hva ▸ hart, ?_⟩
  rw [hview, hws, hva, ← hmu]
  simp only [applyAll, List.foldl_cons, List.foldl_nil, applyWrite, view, View.mk.injEq]
  refine ⟨trivial, rfl, ?_, ?_, rfl⟩
  · symm; simp [favorited]; intro x _ h1 h2; rcases h1 with h1 | h1 <;> simp_all
  · symm; simp only [favCount]
    have := length_filter_remove (fun g : Favorite => (g.article, g.user)) (fun g => decide (g.article = art.id))
      (art.id, me.id) (Snapshot.toSt s).favs hinv.fav_keys (fun y hy => by simp only [Prod.mk.injEq] at hy; simp [hy.1])
    simp only [Prod.mk.injEq] at this
    rw [this]; simp [favorited]

theorem userById_same {s t : St} (hu : s.users = t.users) : userById s = userById t := by
  funext id; simp [userById, hu]

/-- Writing the tags of a new article appends them. -/
theorem applyAll_put_tags (s : St) (id : U64) (l : List Text) (hl : l.Nodup)
    (hfresh : ∀ x ∈ s.tags, x.article = id → x.tag ∉ l) :
    applyAll s (l.map (fun t => .PutTag ⟨id, t⟩)) = { s with tags := s.tags ++ l.map (fun t => ⟨id, t⟩) } := by
  induction l generalizing s with
  | nil => simp [applyAll]
  | cons t ts ih =>
    simp only [List.nodup_cons] at hl
    simp only [List.map_cons, applyAll, List.foldl_cons, applyWrite]
    rw [upsert_fresh _ _ _ (fun y hy hk => by
      simp only [Prod.mk.injEq] at hk; exact hfresh y hy hk.1 (hk.2 ▸ List.mem_cons_self))]
    rw [← applyAll, ih _ hl.2]
    · simp
    · intro x hx hxa
      rcases List.mem_append.1 hx with hx | hx
      · exact fun hm => hfresh x hx hxa (List.mem_cons_of_mem _ hm)
      · simp at hx; rw [hx]; exact hl.1

/-- Creating an article replies with the article as it reads afterwards. -/
theorem create_reply (a : Principal) (s : Snapshot) (slug t d b : Text) (tags : alloc.vec.Vec Text) (now : U64) ws v
    (hr : Reachable (Snapshot.toSt s))
    (h : transition a s (.CreateArticle slug t d b tags now) = .ok (.Ok (ws, .Article v))) :
    v.article.slug = slug ∧ v.article.author = a.user ∧
      v.toView = view (applyAll (Snapshot.toSt s) ws.val) a.user v.article := by
  have hinv := reachable_inv hr
  simp only [transition, step] at h
  obtain ⟨me, hme, hslug, art, hid, hau, hs, hws, v', hv', hview⟩ :=
    post_of_ok (create_article_spec s a.user slug t d b tags now) h ws _ rfl
  simp only [Reply.Article.injEq] at hv'; subst hv'
  obtain ⟨hm, hmu⟩ := userById_some hme
  have hva : v.article = art := by have := congrArg View.article hview; simpa [ArticleView.toView] using this
  refine ⟨hva ▸ hs, hva ▸ hau.trans hmu, ?_⟩
  -- Nothing refers to the new id yet.
  have hnew : ∀ x : U64, isArticle (Snapshot.toSt s) x → x ≠ art.id := by
    rintro x ⟨b, hb, rfl⟩ heq
    have := hinv.article_fresh b hb; rw [heq] at this; simp [Snapshot.toSt] at this hid; omega
  rw [hview, hws, hva, ← hmu, applyAll, List.foldl_append, ← applyAll,
    applyAll_put_tags _ art.id (distinct tags.val) (distinct_nodup _) ?fresh]
  case fresh => exact fun x hx hxa => absurd hxa (hnew _ (hinv.tag_articles x hx))
  simp only [applyAll, List.foldl_cons, List.foldl_nil, applyWrite, view, View.mk.injEq]
  refine ⟨trivial, ?_, ?_, ?_, ?_⟩
  · simp only [tagsOf, List.filter_append, List.map_append]
    rw [List.filter_eq_nil_iff.2 (fun x hx => by simpa using hnew _ (hinv.tag_articles x hx))]
    simp [List.filter_map, Function.comp_def]
  · symm; simp only [favorited, Bool.eq_false_iff, ne_eq, List.any_eq_true, decide_eq_true_eq, not_exists, not_and]
    intro f hf hfa; exact absurd hfa (hnew _ (hinv.fav_articles f hf))
  · symm; simp only [favCount, List.length_eq_zero_iff, List.filter_eq_nil_iff, decide_eq_true_eq]
    intro f hf hfa; exact hnew _ (hinv.fav_articles f hf) hfa
  · have hu : userById (Snapshot.toSt s) me.id = some me := hmu ▸ hme
    simp only [authorOf, hau, userById] at hu ⊢
    rw [hu]; rfl

/-- Editing an article replies with the article as it reads afterwards. -/
theorem update_reply (a : Principal) (s : Snapshot) (slug : Text) (ns t d b : Option Text) (now : U64) ws v
    (h : transition a s (.UpdateArticle slug ns t d b now) = .ok (.Ok (ws, .Article v))) :
    v.toView = view (applyAll (Snapshot.toSt s) ws.val) a.user v.article := by
  simp only [transition, step] at h
  obtain ⟨me, art, hme, hart, hau, hns, a2, hid, hau2, hs2, hws, v', hv', hview⟩ :=
    post_of_ok (update_article_spec s a.user slug ns t d b now false) h ws _ rfl
  simp only [Reply.Article.injEq] at hv'; subst hv'
  have hmu := (userById_some hme).2
  have hva : v.article = a2 := by have := congrArg View.article hview; simpa [ArticleView.toView, view] using this
  rw [hview, hws, hva, ← hmu]; rfl

/-- Following replies with the profile as it reads afterwards. -/
theorem follow_reply (a : Principal) (s : Snapshot) (n : Text) ws p
    (h : transition a s (.Follow n) = .ok (.Ok (ws, .Profile p))) :
    ∃ x, userByName (Snapshot.toSt s) n = some x ∧ p = profileOf (applyAll (Snapshot.toSt s) ws.val) a.user x ∧
      p.following = true := by
  simp only [transition, step] at h
  obtain ⟨me, x, hme, hx, -, hws, hp⟩ := post_of_ok (follow_spec s a.user n) h ws _ rfl
  simp only [Reply.Profile.injEq] at hp; subst hp
  have hmu := (userById_some hme).2
  refine ⟨x, hx, ?_, rfl⟩
  rw [hws, ← hmu]
  simp only [applyAll, List.foldl_cons, List.foldl_nil, applyWrite, profileOf, Profile.mk.injEq, true_and]
  symm; simp only [follows, List.any_eq_true, decide_eq_true_eq]
  exact ⟨_, mem_upsert_self _ _ _, rfl, rfl⟩

theorem unfollow_reply (a : Principal) (s : Snapshot) (n : Text) ws p
    (h : transition a s (.Unfollow n) = .ok (.Ok (ws, .Profile p))) :
    ∃ x, userByName (Snapshot.toSt s) n = some x ∧ p = profileOf (applyAll (Snapshot.toSt s) ws.val) a.user x ∧
      p.following = false := by
  simp only [transition, step] at h
  obtain ⟨me, x, hme, hx, hws, hp⟩ := post_of_ok (unfollow_spec s a.user n) h ws _ rfl
  simp only [Reply.Profile.injEq] at hp; subst hp
  have hmu := (userById_some hme).2
  refine ⟨x, hx, ?_, rfl⟩
  rw [hws, ← hmu]
  simp only [applyAll, List.foldl_cons, List.foldl_nil, applyWrite, profileOf, Profile.mk.injEq, true_and]
  symm; simp [follows]; intro y _ h1 h2; rcases h1 with h1 | h1 <;> simp_all

/-- A new comment replies with the comment as the listing shows it afterwards. -/
theorem add_comment_reply (a : Principal) (s : Snapshot) (slug body : Text) (now : U64) ws cv
    (h : transition a s (.AddComment slug body now) = .ok (.Ok (ws, .Comment cv))) :
    ∃ art, articleBySlug (Snapshot.toSt s) slug = some art ∧ cv.comment.author = a.user ∧
      cv ∈ commentsOf (applyAll (Snapshot.toSt s) ws.val) a.user art.id := by
  simp only [transition, step] at h
  obtain ⟨me, art, hme, hart, c, hid, har, hau, hws, hr⟩ := post_of_ok (add_comment_spec s a.user slug body now) h ws _ rfl
  simp only [Reply.Comment.injEq] at hr; subst hr
  have hmu := (userById_some hme).2
  refine ⟨art, hart, hau.trans hmu, ?_⟩
  rw [hws, ← hmu]
  simp only [applyAll, List.foldl_cons, List.foldl_nil, applyWrite, commentsOf, List.mem_map, List.mem_filter]
  refine ⟨c, ⟨mem_upsert_self _ _ _, by simp [har]⟩, ?_⟩
  have hu : userById (Snapshot.toSt s) me.id = some me := hmu ▸ hme
  simp only [authorOf, hau, userById] at hu ⊢
  rw [hu]; rfl

/-- Deleting an article leaves no tag, favorite or comment pointing at it. -/
theorem delete_cascades (a : Principal) (s : Snapshot) (slug : Text) ws r
    (h : transition a s (.DeleteArticle slug) = .ok (.Ok (ws, r))) :
    ∃ art, articleBySlug (Snapshot.toSt s) slug = some art ∧ art.author = a.user ∧
      let s' := applyAll (Snapshot.toSt s) ws.val
      (∀ b ∈ s'.articles, b.id ≠ art.id) ∧ (∀ t ∈ s'.tags, t.article ≠ art.id) ∧
      (∀ f ∈ s'.favs, f.article ≠ art.id) ∧ (∀ c ∈ s'.comments, c.article ≠ art.id) := by
  simp only [transition, step] at h
  obtain ⟨me, art, hme, hart, hau, hws⟩ := post_of_ok (delete_article_spec s a.user slug) h ws r rfl
  refine ⟨art, hart, hau.trans (userById_some hme).2, ?_⟩
  rw [hws]
  simp only [applyAll, List.foldl_cons, List.foldl_nil, applyWrite]
  refine ⟨?_, ?_, ?_, ?_⟩ <;> intro x hx <;> exact of_decide_eq_true (List.mem_filter.1 hx).2

end conduit_kernel.Replies
