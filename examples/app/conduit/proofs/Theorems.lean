import Commands
/-!
# Who may write what

`effect_of` sums up `Commands.lean`: a successful command writes nothing, or
it does one of eleven things. The policy theorem and the invariants are case
analyses over those eleven, so they never look at the code again.
-/
open Aeneas Aeneas.Std Result conduit_kernel conduit_kernel.Spec conduit_kernel.Helpers
  conduit_kernel.Commands H5iAppLib

namespace conduit_kernel.Theorems

/-- What a successful command writes, as a function of the state before. -/
inductive Effect (s : Snapshot) (u : U64) : List Write → Prop
  | none : Effect s u []
  | register (x : User) :
      userByName (Snapshot.toSt s) x.username = none → userByEmail (Snapshot.toSt s) x.email = none →
      x.id.val = s.counter.last_user.val + 1 →
      Effect s u [.SetCounter { s.counter with last_user := x.id }, .PutUser x]
  | updateUser (me x : User) (n e : Option Text) :
      userById (Snapshot.toSt s) u = some me →
      (n.bind (userByName (Snapshot.toSt s))).any (·.id ≠ me.id) = false →
      (e.bind (userByEmail (Snapshot.toSt s))).any (·.id ≠ me.id) = false →
      x.id = me.id → x.username = n.getD me.username → x.email = e.getD me.email →
      Effect s u [.PutUser x]
  | follow (me t : User) :
      userById (Snapshot.toSt s) u = some me → t ∈ s.users.val → t.id ≠ me.id →
      Effect s u [.PutFollow ⟨me.id, t.id⟩]
  | unfollow (me t : User) :
      userById (Snapshot.toSt s) u = some me → Effect s u [.DelFollow ⟨me.id, t.id⟩]
  | create (me : User) (a : Article) (tags : List Text) :
      userById (Snapshot.toSt s) u = some me → articleBySlug (Snapshot.toSt s) a.slug = none →
      a.id.val = s.counter.last_article.val + 1 → a.author = me.id → tags.Nodup →
      Effect s u ([.SetCounter { s.counter with last_article := a.id }, .PutArticle a] ++
        tags.map (fun t => .PutTag ⟨a.id, t⟩))
  | update (me : User) (a a2 : Article) (ns : Option Text) :
      userById (Snapshot.toSt s) u = some me → a ∈ s.articles.val → a.author = me.id →
      (ns.bind (articleBySlug (Snapshot.toSt s))).any (·.id ≠ a.id) = false →
      a2.id = a.id → a2.author = a.author → a2.slug = ns.getD a.slug →
      Effect s u [.PutArticle a2]
  | delete (me : User) (a : Article) :
      userById (Snapshot.toSt s) u = some me → a ∈ s.articles.val → a.author = me.id →
      Effect s u [.DelTagsOf a.id, .DelFavoritesOf a.id, .DelCommentsOf a.id, .DelArticle a.id]
  | favorite (me : User) (a : Article) :
      userById (Snapshot.toSt s) u = some me → a ∈ s.articles.val → Effect s u [.PutFavorite ⟨a.id, me.id⟩]
  | unfavorite (me : User) (a : Article) :
      userById (Snapshot.toSt s) u = some me → Effect s u [.DelFavorite ⟨a.id, me.id⟩]
  | comment (me : User) (a : Article) (c : Comment) :
      userById (Snapshot.toSt s) u = some me → a ∈ s.articles.val →
      c.id.val = s.counter.last_comment.val + 1 → c.article = a.id → c.author = me.id →
      Effect s u [.SetCounter { s.counter with last_comment := c.id }, .PutComment c]
  | uncomment (me : User) (c : Comment) (id : U64) :
      userById (Snapshot.toSt s) u = some me → c ∈ s.comments.val → c.id = id → c.author = me.id →
      Effect s u [.DelComment id]

/-! ## Lookups -/

theorem userById_some {s : St} {u : U64} {me : User} (h : userById s u = some me) : me ∈ s.users ∧ me.id = u := by
  have := find?_mem h; simpa using this

theorem userByName_some {s : St} {n : Text} {x : User} (h : userByName s n = some x) :
    x ∈ s.users ∧ x.username = n := by
  have := find?_mem h; simpa using this

theorem articleBySlug_some {s : St} {n : Text} {a : Article} (h : articleBySlug s n = some a) :
    a ∈ s.articles ∧ a.slug = n := by
  have := find?_mem h; simpa using this

theorem commentById_some {s : St} {id : U64} {c : Comment} (h : commentById s id = some c) :
    c ∈ s.comments ∧ c.id = id := by
  have := find?_mem h; simpa using this

theorem distinct_nodup_aux {α} [DecidableEq α] (l acc : List α) (h : acc.Nodup) :
    (l.foldl (fun acc x => if acc.any (· = x) then acc else acc ++ [x]) acc).Nodup := by
  induction l generalizing acc with
  | nil => simpa
  | cons y ys ih =>
    rw [List.foldl_cons]; apply ih
    split
    · exact h
    · rename_i hy
      simp only [List.any_eq_true, decide_eq_true_eq, not_exists, not_and] at hy
      exact List.nodup_append.2 ⟨h, List.nodup_singleton y, fun a ha b hb => by
        simp at hb; subst hb; exact hy a ha⟩

theorem distinct_nodup {α} [DecidableEq α] (l : List α) : (distinct l).Nodup :=
  distinct_nodup_aux l [] List.nodup_nil

/-! ## What a command writes -/

/-- The writes of a read-only command. -/
theorem ws_nil {ws : alloc.vec.Vec Write} (h : ws = alloc.vec.Vec.new Write) : ws.val = [] := by
  subst h; rfl

theorem effect_of (a : Principal) (s : Snapshot) (c : Command) (up : Bool) ws r
    (h : step a s c up = .ok (.Ok (ws, r))) : Effect s a.user ws.val := by
  cases c <;> simp only [step] at h
  case Register n e p =>
    obtain ⟨hn, he, x, hid, hxn, hxe, hws, -⟩ := post_of_ok (register_spec s n e p) h ws r rfl
    rw [hws]; subst hxn hxe; exact .register x hn he hid
  case Login e p =>
    have := post_of_ok (login_spec s e p) h
    revert this; split <;> (try split) <;> intro h' <;> simp_all [ws_nil]; exact .none
  case CurrentUser =>
    have := post_of_ok (current_user_spec s a.user) h
    revert this; split <;> intro h' <;> simp_all [ws_nil]; exact .none
  case UpdateUser e n p b i =>
    obtain ⟨me, hme, hn, he, x, hid, hxn, hxe, hws, -⟩ := post_of_ok (update_user_spec s a.user e n p b i) h ws r rfl
    rw [hws]; exact .updateUser me x n e hme hn he hid hxn hxe
  case GetProfile n =>
    have := post_of_ok (get_profile_spec s a.user n) h
    revert this; split <;> intro h' <;> simp_all [ws_nil]; exact .none
  case Follow n =>
    obtain ⟨me, t, hme, ht, hne, hws, -⟩ := post_of_ok (follow_spec s a.user n) h ws r rfl
    rw [hws]; exact .follow me t hme (userByName_some ht).1 hne
  case Unfollow n =>
    obtain ⟨me, t, hme, ht, hws, -⟩ := post_of_ok (unfollow_spec s a.user n) h ws r rfl
    rw [hws]; exact .unfollow me t hme
  case ListArticles tag au fav lim off =>
    obtain ⟨vs, hr, -⟩ := post_of_ok (list_articles_spec s a.user tag au fav lim off up) h
    simp only [core.result.Result.Ok.injEq, Prod.mk.injEq] at hr
    rw [ws_nil hr.1]; exact .none
  case Feed lim off =>
    have := post_of_ok (feed_spec s a.user lim off up) h
    split at this
    · simp at this
    · obtain ⟨vs, hr, -⟩ := this
      simp only [core.result.Result.Ok.injEq, Prod.mk.injEq] at hr
      rw [ws_nil hr.1]; exact .none
  case GetArticle slug =>
    have := post_of_ok (get_article_spec s a.user slug up) h
    split at this
    · simp at this
    · obtain ⟨vs, hr, -⟩ := this
      simp only [core.result.Result.Ok.injEq, Prod.mk.injEq] at hr
      rw [ws_nil hr.1]; exact .none
  case CreateArticle slug t d b tags now =>
    obtain ⟨me, hme, hslug, x, hid, hau, hxs, hws, -⟩ := post_of_ok (create_article_spec s a.user slug t d b tags now) h ws r rfl
    rw [hws]; subst hxs; exact .create me x _ hme hslug hid hau (distinct_nodup _)
  case UpdateArticle slug ns t d b now =>
    obtain ⟨me, x, hme, hx, hau, hns, x2, hid, hau2, hs2, hws, -⟩ :=
      post_of_ok (update_article_spec s a.user slug ns t d b now up) h ws r rfl
    rw [hws]; exact .update me x x2 ns hme (articleBySlug_some hx).1 hau hns hid hau2 hs2
  case DeleteArticle slug =>
    obtain ⟨me, x, hme, hx, hau, hws⟩ := post_of_ok (delete_article_spec s a.user slug) h ws r rfl
    rw [hws]; exact .delete me x hme (articleBySlug_some hx).1 hau
  case Favorite slug =>
    obtain ⟨me, x, hme, hx, hws, -⟩ := post_of_ok (favorite_spec s a.user slug) h ws r rfl
    rw [hws]; exact .favorite me x hme (articleBySlug_some hx).1
  case Unfavorite slug =>
    obtain ⟨me, x, hme, hx, hws, -⟩ := post_of_ok (unfavorite_spec s a.user slug up) h ws r rfl
    rw [hws]; exact .unfavorite me x hme
  case GetComments slug =>
    have := post_of_ok (get_comments_spec s a.user slug) h
    split at this
    · simp at this
    · obtain ⟨vs, hr, -⟩ := this
      simp only [core.result.Result.Ok.injEq, Prod.mk.injEq] at hr
      rw [ws_nil hr.1]; exact .none
  case AddComment slug body now =>
    obtain ⟨me, x, hme, hx, cm, hid, har, hau, hws, -⟩ := post_of_ok (add_comment_spec s a.user slug body now) h ws r rfl
    rw [hws]; exact .comment me x cm hme (articleBySlug_some hx).1 hid har hau
  case DeleteComment slug id =>
    obtain ⟨me, x, cm, hme, -, hc, -, hau, hws⟩ := post_of_ok (delete_comment_spec s a.user slug id) h ws r rfl
    obtain ⟨hcm, hcid⟩ := commentById_some hc
    rw [hws]; exact .uncomment me cm id hme hcm hcid hau
  case GetTags =>
    obtain ⟨v, hv, -⟩ := (WP.spec_equiv_exists _ _).1 (all_tags_spec s.tags)
    rw [hv] at h; simp only [bind_ok, Result.ok.injEq, core.result.Result.Ok.injEq, Prod.mk.injEq] at h
    rw [ws_nil h.1.symm]; exact .none

/-- The kernel never fails: no panic, overflow or bad index, in either variant. -/
theorem step_total (a : Principal) (s : Snapshot) (c : Command) (up : Bool) : ∃ r, step a s c up = ok r := by
  cases c <;> simp only [step]
  · exact ok_of (register_spec _ _ _ _)
  · exact ok_of (login_spec _ _ _)
  · exact ok_of (current_user_spec _ _)
  · exact ok_of (update_user_spec _ _ _ _ _ _ _)
  · exact ok_of (get_profile_spec _ _ _)
  · exact ok_of (follow_spec _ _ _)
  · exact ok_of (unfollow_spec _ _ _)
  · exact ok_of (list_articles_spec _ _ _ _ _ _ _ _)
  · exact ok_of (feed_spec _ _ _ _ _)
  · exact ok_of (get_article_spec _ _ _ _)
  · exact ok_of (create_article_spec _ _ _ _ _ _ _ _)
  · exact ok_of (update_article_spec _ _ _ _ _ _ _ _ _)
  · exact ok_of (delete_article_spec _ _ _)
  · exact ok_of (favorite_spec _ _ _)
  · exact ok_of (unfavorite_spec _ _ _ _)
  · exact ok_of (get_comments_spec _ _ _)
  · exact ok_of (add_comment_spec _ _ _ _ _)
  · exact ok_of (delete_comment_spec _ _ _ _)
  · obtain ⟨v, hv, -⟩ := (WP.spec_equiv_exists _ _).1 (all_tags_spec s.tags); rw [hv]; simp

theorem transition_total (a : Principal) (s : Snapshot) (c : Command) : ∃ r, transition a s c = ok r :=
  step_total a s c false

theorem transition_upstream_total (a : Principal) (s : Snapshot) (c : Command) :
    ∃ r, transition_upstream a s c = ok r :=
  step_total a s c true

/-! ## The policy -/

/-- Every write of a successful command is allowed by the policy, judged
against the state before the command. -/
theorem authorized (a : Principal) (s : Snapshot) (c : Command) ws r
    (h : transition a s c = .ok (.Ok (ws, r))) :
    ∀ w ∈ ws.val, allowed (Snapshot.toSt s) a.user w := by
  have he := effect_of a s c false ws r h
  revert he; generalize ws.val = l; intro he
  cases he with
  | none => simp
  | register x hn he hid =>
    intro w hw; simp at hw
    rcases hw with rfl | rfl
    · simp [allowed, Snapshot.toSt, hid]
    · exact .inl (by simp [Snapshot.toSt, hid])
  | updateUser me x n e hme hn he hid =>
    obtain ⟨hm, hmu⟩ := userById_some hme
    intro w hw; simp at hw; subst hw
    exact .inr ⟨hid.trans hmu, me, hm, hmu⟩
  | follow me t hme ht hne =>
    obtain ⟨hm, hmu⟩ := userById_some hme
    intro w hw; simp at hw; subst hw
    exact ⟨hmu, ⟨me, hm, hmu⟩, hmu ▸ hne, t, ht, rfl⟩
  | unfollow me t hme =>
    obtain ⟨hm, hmu⟩ := userById_some hme
    intro w hw; simp at hw; subst hw
    exact ⟨hmu, me, hm, hmu⟩
  | create me x tags hme hslug hid hau hnd =>
    obtain ⟨hm, hmu⟩ := userById_some hme
    intro w hw; simp at hw
    rcases hw with rfl | rfl | ⟨t, -, rfl⟩
    · simp [allowed, Snapshot.toSt, hid]
    · exact ⟨hau.trans hmu, ⟨me, hm, hmu⟩, .inl (by simp [Snapshot.toSt, hid])⟩
    · exact ⟨by simp [Snapshot.toSt, hid], me, hm, hmu⟩
  | update me x x2 ns hme hx hau hns hid hau2 hs2 =>
    obtain ⟨hm, hmu⟩ := userById_some hme
    intro w hw; simp at hw; subst hw
    exact ⟨hau2.trans (hau.trans hmu), ⟨me, hm, hmu⟩, .inr ⟨x, hx, hid.symm, hau.trans hmu⟩⟩
  | delete me x hme hx hau =>
    obtain ⟨hm, hmu⟩ := userById_some hme
    have hw : wrote (Snapshot.toSt s) a.user x.id := ⟨x, hx, rfl, hau.trans hmu⟩
    intro w hw'; simp at hw'
    rcases hw' with rfl | rfl | rfl | rfl <;> exact hw
  | favorite me x hme hx =>
    obtain ⟨hm, hmu⟩ := userById_some hme
    intro w hw; simp at hw; subst hw
    exact ⟨hmu, ⟨me, hm, hmu⟩, x, hx, rfl⟩
  | unfavorite me x hme =>
    obtain ⟨hm, hmu⟩ := userById_some hme
    intro w hw; simp at hw; subst hw
    exact ⟨hmu, me, hm, hmu⟩
  | comment me x cm hme hx hid har hau =>
    obtain ⟨hm, hmu⟩ := userById_some hme
    intro w hw; simp at hw
    rcases hw with rfl | rfl
    · simp [allowed, Snapshot.toSt, hid]
    · exact ⟨hau.trans hmu, ⟨me, hm, hmu⟩, by simp [Snapshot.toSt, hid], x, hx, har.symm⟩
  | uncomment me cm id hme hcm hcid hau =>
    obtain ⟨hm, hmu⟩ := userById_some hme
    intro w hw; simp at hw; subst hw
    exact ⟨cm, hcm, hcid, hau.trans hmu⟩

end conduit_kernel.Theorems
