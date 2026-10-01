import Theorems
/-!
# Invariants

One lemma per kind of write says when it keeps `Inv`; `inv_preserved` walks
the eleven effects of `Theorems.effect_of`, and `reachable_inv` follows by
induction.
-/
open Aeneas Aeneas.Std Result conduit_kernel conduit_kernel.Spec conduit_kernel.Helpers
  conduit_kernel.Commands conduit_kernel.Theorems H5iAppLib

namespace conduit_kernel.Invariants

/-! ## Keyed tables -/

theorem isUser_upsert {s : St} {x : User} {v : U64} (h : isUser s v) :
    ∃ y ∈ upsert (·.id) x s.users, y.id = v := by
  obtain ⟨z, hz, rfl⟩ := h
  exact key_kept (·.id) x hz

theorem isArticle_upsert {s : St} {a : Article} {v : U64} (h : isArticle s v) :
    ∃ y ∈ upsert (·.id) a s.articles, y.id = v := by
  obtain ⟨z, hz, rfl⟩ := h
  exact key_kept (·.id) a hz

/-! ## One lemma per kind of write -/

theorem inv_set_counter {s : St} (hi : Inv s) (c : Counter) (h1 : s.lastUser ≤ c.last_user.val)
    (h2 : s.lastArticle ≤ c.last_article.val) (h3 : s.lastComment ≤ c.last_comment.val) :
    Inv (applyWrite s (.SetCounter c)) where
  user_ids := hi.user_ids
  user_fresh x hx := ⟨(hi.user_fresh x hx).1, (hi.user_fresh x hx).2.trans h1⟩
  usernames := hi.usernames
  emails := hi.emails
  follow_keys := hi.follow_keys
  follow_self := hi.follow_self
  follow_users := hi.follow_users
  article_ids := hi.article_ids
  article_fresh a ha := (hi.article_fresh a ha).trans h2
  slugs := hi.slugs
  article_authors := hi.article_authors
  tag_keys := hi.tag_keys
  tag_articles := hi.tag_articles
  fav_keys := hi.fav_keys
  fav_articles := hi.fav_articles
  fav_users := hi.fav_users
  comment_ids := hi.comment_ids
  comment_fresh c' hc := (hi.comment_fresh c' hc).trans h3
  comment_articles := hi.comment_articles
  comment_authors := hi.comment_authors

/-- A user row with a valid id whose name and email no other user has; covers
sign-up and account edits. -/
theorem inv_put_user {s : St} (hi : Inv s) (x : User) (hpos : 0 < x.id.val) (hle : x.id.val ≤ s.lastUser)
    (hn : ∀ y ∈ s.users, y.username = x.username → y.id = x.id)
    (he : ∀ y ∈ s.users, y.email = x.email → y.id = x.id) :
    Inv (applyWrite s (.PutUser x)) where
  user_ids := nodup_map_upsert (·.id) (·.id) (fun _ _ => Iff.rfl) x s.users hi.user_ids
  user_fresh z hz := by
    rcases mem_upsert_of hz with rfl | hz
    · exact ⟨hpos, hle⟩
    · exact hi.user_fresh z hz
  usernames := nodup_map_upsert_attr (·.id) (·.username) x s.users hi.user_ids hi.usernames hn
  emails := nodup_map_upsert_attr (·.id) (·.email) x s.users hi.user_ids hi.emails he
  follow_keys := hi.follow_keys
  follow_self := hi.follow_self
  follow_users f hf := ⟨isUser_upsert (hi.follow_users f hf).1, isUser_upsert (hi.follow_users f hf).2⟩
  article_ids := hi.article_ids
  article_fresh := hi.article_fresh
  slugs := hi.slugs
  article_authors a ha := isUser_upsert (hi.article_authors a ha)
  tag_keys := hi.tag_keys
  tag_articles := hi.tag_articles
  fav_keys := hi.fav_keys
  fav_articles := hi.fav_articles
  fav_users f hf := isUser_upsert (hi.fav_users f hf)
  comment_ids := hi.comment_ids
  comment_fresh := hi.comment_fresh
  comment_articles := hi.comment_articles
  comment_authors c hc := isUser_upsert (hi.comment_authors c hc)

theorem inv_put_follow {s : St} (hi : Inv s) (f : Follow) (h1 : isUser s f.follower) (h2 : isUser s f.followed)
    (hne : f.follower ≠ f.followed) : Inv (applyWrite s (.PutFollow f)) :=
  { hi with
    follow_keys := nodup_map_upsert _ _ (fun _ _ => Iff.rfl) f s.follows hi.follow_keys
    follow_self := fun g hg => by
      rcases mem_upsert_of hg with rfl | hg
      · exact hne
      · exact hi.follow_self g hg
    follow_users := fun g hg => by
      rcases mem_upsert_of hg with rfl | hg
      · exact ⟨h1, h2⟩
      · exact hi.follow_users g hg }

theorem inv_del_follow {s : St} (hi : Inv s) (f : Follow) : Inv (applyWrite s (.DelFollow f)) :=
  { hi with
    follow_keys := nodup_map_filter _ _ _ hi.follow_keys
    follow_self := fun g hg => hi.follow_self g (List.mem_filter.1 hg).1
    follow_users := fun g hg => hi.follow_users g (List.mem_filter.1 hg).1 }

/-- An article row with a valid id and author whose slug no other article
has; covers creation and edits. -/
theorem inv_put_article {s : St} (hi : Inv s) (a : Article) (hle : a.id.val ≤ s.lastArticle)
    (hau : isUser s a.author) (hslug : ∀ b ∈ s.articles, b.slug = a.slug → b.id = a.id) :
    Inv (applyWrite s (.PutArticle a)) :=
  { hi with
    article_ids := nodup_map_upsert (·.id) (·.id) (fun _ _ => Iff.rfl) a s.articles hi.article_ids
    article_fresh := fun b hb => by
      rcases mem_upsert_of hb with rfl | hb
      · exact hle
      · exact hi.article_fresh b hb
    slugs := nodup_map_upsert_attr (·.id) (·.slug) a s.articles hi.article_ids hi.slugs hslug
    article_authors := fun b hb => by
      rcases mem_upsert_of hb with rfl | hb
      · exact hau
      · exact hi.article_authors b hb
    tag_articles := fun t ht => isArticle_upsert (hi.tag_articles t ht)
    fav_articles := fun f hf => isArticle_upsert (hi.fav_articles f hf)
    comment_articles := fun c hc => isArticle_upsert (hi.comment_articles c hc) }

theorem inv_put_tag {s : St} (hi : Inv s) (t : Tag) (ha : isArticle s t.article) :
    Inv (applyWrite s (.PutTag t)) :=
  { hi with
    tag_keys := nodup_map_upsert _ _ (fun _ _ => Iff.rfl) t s.tags hi.tag_keys
    tag_articles := fun t' ht => by
      rcases mem_upsert_of ht with rfl | ht
      · exact ha
      · exact hi.tag_articles t' ht }

theorem inv_del_tags_of {s : St} (hi : Inv s) (id : U64) : Inv (applyWrite s (.DelTagsOf id)) :=
  { hi with
    tag_keys := nodup_map_filter _ _ _ hi.tag_keys
    tag_articles := fun t ht => hi.tag_articles t (List.mem_filter.1 ht).1 }

theorem inv_put_favorite {s : St} (hi : Inv s) (f : Favorite) (ha : isArticle s f.article) (hu : isUser s f.user) :
    Inv (applyWrite s (.PutFavorite f)) :=
  { hi with
    fav_keys := nodup_map_upsert _ _ (fun _ _ => Iff.rfl) f s.favs hi.fav_keys
    fav_articles := fun g hg => by
      rcases mem_upsert_of hg with rfl | hg
      · exact ha
      · exact hi.fav_articles g hg
    fav_users := fun g hg => by
      rcases mem_upsert_of hg with rfl | hg
      · exact hu
      · exact hi.fav_users g hg }

theorem inv_del_favorite {s : St} (hi : Inv s) (f : Favorite) : Inv (applyWrite s (.DelFavorite f)) :=
  { hi with
    fav_keys := nodup_map_filter _ _ _ hi.fav_keys
    fav_articles := fun g hg => hi.fav_articles g (List.mem_filter.1 hg).1
    fav_users := fun g hg => hi.fav_users g (List.mem_filter.1 hg).1 }

theorem inv_del_favorites_of {s : St} (hi : Inv s) (id : U64) : Inv (applyWrite s (.DelFavoritesOf id)) :=
  { hi with
    fav_keys := nodup_map_filter _ _ _ hi.fav_keys
    fav_articles := fun g hg => hi.fav_articles g (List.mem_filter.1 hg).1
    fav_users := fun g hg => hi.fav_users g (List.mem_filter.1 hg).1 }

theorem inv_put_comment {s : St} (hi : Inv s) (c : Comment) (hle : c.id.val ≤ s.lastComment)
    (ha : isArticle s c.article) (hu : isUser s c.author) : Inv (applyWrite s (.PutComment c)) :=
  { hi with
    comment_ids := nodup_map_upsert (·.id) (·.id) (fun _ _ => Iff.rfl) c s.comments hi.comment_ids
    comment_fresh := fun d hd => by
      rcases mem_upsert_of hd with rfl | hd
      · exact hle
      · exact hi.comment_fresh d hd
    comment_articles := fun d hd => by
      rcases mem_upsert_of hd with rfl | hd
      · exact ha
      · exact hi.comment_articles d hd
    comment_authors := fun d hd => by
      rcases mem_upsert_of hd with rfl | hd
      · exact hu
      · exact hi.comment_authors d hd }

theorem inv_del_comment {s : St} (hi : Inv s) (id : U64) : Inv (applyWrite s (.DelComment id)) :=
  { hi with
    comment_ids := nodup_map_filter _ _ _ hi.comment_ids
    comment_fresh := fun d hd => hi.comment_fresh d (List.mem_filter.1 hd).1
    comment_articles := fun d hd => hi.comment_articles d (List.mem_filter.1 hd).1
    comment_authors := fun d hd => hi.comment_authors d (List.mem_filter.1 hd).1 }

theorem inv_del_comments_of {s : St} (hi : Inv s) (id : U64) : Inv (applyWrite s (.DelCommentsOf id)) :=
  { hi with
    comment_ids := nodup_map_filter _ _ _ hi.comment_ids
    comment_fresh := fun d hd => hi.comment_fresh d (List.mem_filter.1 hd).1
    comment_articles := fun d hd => hi.comment_articles d (List.mem_filter.1 hd).1
    comment_authors := fun d hd => hi.comment_authors d (List.mem_filter.1 hd).1 }

/-- Deleting an article nothing refers to any more. -/
theorem inv_del_article {s : St} (hi : Inv s) (id : U64) (ht : ∀ t ∈ s.tags, t.article ≠ id)
    (hf : ∀ f ∈ s.favs, f.article ≠ id) (hc : ∀ c ∈ s.comments, c.article ≠ id) :
    Inv (applyWrite s (.DelArticle id)) :=
  have keep : ∀ v, isArticle s v → v ≠ id → isArticle (applyWrite s (.DelArticle id)) v :=
    fun v ⟨a, ha, hv⟩ hne => ⟨a, List.mem_filter.2 ⟨ha, by simp [hv, hne]⟩, hv⟩
  { hi with
    article_ids := nodup_map_filter _ _ _ hi.article_ids
    article_fresh := fun a ha => hi.article_fresh a (List.mem_filter.1 ha).1
    slugs := nodup_map_filter _ _ _ hi.slugs
    article_authors := fun a ha => hi.article_authors a (List.mem_filter.1 ha).1
    tag_articles := fun t h => keep _ (hi.tag_articles t h) (ht t h)
    fav_articles := fun f h => keep _ (hi.fav_articles f h) (hf f h)
    comment_articles := fun c h => keep _ (hi.comment_articles c h) (hc c h) }

/-! ## Whole commands -/

theorem inv_put_tags {s : St} (hi : Inv s) (id : U64) (ha : isArticle s id) (tags : List Text) :
    Inv (applyAll s (tags.map (fun t => .PutTag ⟨id, t⟩))) := by
  induction tags generalizing s with
  | nil => exact hi
  | cons t ts ih =>
    simp only [List.map_cons, applyAll, List.foldl_cons]
    exact ih (inv_put_tag hi ⟨id, t⟩ ha) ha

theorem not_taken {α β} [DecidableEq β] (l : List α) (g : α → β) (v : β)
    (h : l.find? (fun y => decide (g y = v)) = none) : ∀ y ∈ l, g y ≠ v := by
  intro y hy hgy
  rw [List.find?_eq_none] at h
  exact h y hy (by simp [hgy])

/-- If the given name is taken, it is taken by `me`; so a row keeping or
changing to that name leaves the names unique. -/
theorem owner_of_any {α β} [DecidableEq β] (l : List α) (g : α → β) (key : α → U64) (hg : (l.map g).Nodup)
    (me : α) (hme : me ∈ l) (n : Option β)
    (hn : (n.bind (fun v => l.find? (fun y => decide (g y = v)))).any (fun z => key z ≠ key me) = false) :
    ∀ y ∈ l, g y = n.getD (g me) → key y = key me := by
  intro y hy hgy
  cases n with
  | none => simp at hgy; rw [List.inj_on_of_nodup_map hg hy hme hgy]
  | some v =>
    simp at hgy
    cases hz : l.find? (fun y => decide (g y = v)) with
    | none => exact absurd hgy (not_taken l g v hz y hy)
    | some z =>
      simp [hz] at hn
      obtain ⟨hzl, hzv⟩ := find?_mem hz
      simp at hzv
      rw [List.inj_on_of_nodup_map hg hy hzl (hgy.trans hzv.symm), hn]

/-- Successful commands keep the invariants. -/
theorem inv_preserved (a : Principal) (s : Snapshot) (c : Command) ws r
    (hinv : Inv (Snapshot.toSt s)) (h : transition a s c = .ok (.Ok (ws, r))) :
    Inv (applyAll (Snapshot.toSt s) ws.val) := by
  have he := effect_of a s c false ws r h
  revert he; generalize ws.val = l; intro he
  cases he with
  | none => exact hinv
  | register x hn he hid =>
    simp only [applyAll, List.foldl_cons, List.foldl_nil]
    have h1 := inv_set_counter hinv { s.counter with last_user := x.id }
      (by simp [Snapshot.toSt, hid]) (by simp [Snapshot.toSt]) (by simp [Snapshot.toSt])
    refine inv_put_user h1 x (by omega) (by simp [applyWrite, hid]) ?_ ?_
    · intro y hy hyn; exact absurd hyn (not_taken _ _ _ hn y hy)
    · intro y hy hye; exact absurd hye (not_taken _ _ _ he y hy)
  | updateUser me x n e hme hn he hid hxn hxe =>
    obtain ⟨hm, -⟩ := userById_some hme
    simp only [applyAll, List.foldl_cons, List.foldl_nil]
    refine inv_put_user hinv x (hid ▸ (hinv.user_fresh me hm).1) (hid ▸ (hinv.user_fresh me hm).2) ?_ ?_
    · intro y hy hyn
      rw [hid]; exact owner_of_any _ (·.username) (·.id) hinv.usernames me hm n hn y hy (hyn.trans hxn)
    · intro y hy hye
      rw [hid]; exact owner_of_any _ (·.email) (·.id) hinv.emails me hm e he y hy (hye.trans hxe)
  | follow me t hme ht hne =>
    obtain ⟨hm, -⟩ := userById_some hme
    simp only [applyAll, List.foldl_cons, List.foldl_nil]
    exact inv_put_follow hinv _ ⟨me, hm, rfl⟩ ⟨t, ht, rfl⟩ (Ne.symm hne)
  | unfollow me t hme =>
    simp only [applyAll, List.foldl_cons, List.foldl_nil]
    exact inv_del_follow hinv _
  | create me x tags hme hslug hid hau hnd =>
    obtain ⟨hm, -⟩ := userById_some hme
    rw [applyAll, List.foldl_append, ← applyAll]
    simp only [List.foldl_cons, List.foldl_nil]
    have h1 := inv_set_counter hinv { s.counter with last_article := x.id }
      (by simp [Snapshot.toSt]) (by simp [Snapshot.toSt, hid]) (by simp [Snapshot.toSt])
    have h2 := inv_put_article h1 x (by simp [applyWrite]) ⟨me, hm, hau.symm⟩
      (fun b hb hbs => absurd hbs (not_taken _ _ _ hslug b hb))
    exact inv_put_tags h2 x.id ⟨x, mem_upsert_self _ _ _, rfl⟩ tags
  | update me x x2 ns hme hx hau hns hid hau2 hs2 =>
    obtain ⟨hm, -⟩ := userById_some hme
    simp only [applyAll, List.foldl_cons, List.foldl_nil]
    refine inv_put_article hinv x2 (hid ▸ hinv.article_fresh x hx) ⟨me, hm, (hau2.trans hau).symm⟩ ?_
    intro b hb hbs
    rw [hid]; exact owner_of_any _ (·.slug) (·.id) hinv.slugs x hx ns hns b hb (hbs.trans hs2)
  | delete me x hme hx hau =>
    simp only [applyAll, List.foldl_cons, List.foldl_nil]
    have h1 := inv_del_comments_of (inv_del_favorites_of (inv_del_tags_of hinv x.id) x.id) x.id
    refine inv_del_article h1 x.id ?_ ?_ ?_
    · intro t ht; exact of_decide_eq_true (List.mem_filter.1 ht).2
    · intro f hf; exact of_decide_eq_true (List.mem_filter.1 hf).2
    · intro c hc; exact of_decide_eq_true (List.mem_filter.1 hc).2
  | favorite me x hme hx =>
    obtain ⟨hm, -⟩ := userById_some hme
    simp only [applyAll, List.foldl_cons, List.foldl_nil]
    exact inv_put_favorite hinv _ ⟨x, hx, rfl⟩ ⟨me, hm, rfl⟩
  | unfavorite me x hme =>
    simp only [applyAll, List.foldl_cons, List.foldl_nil]
    exact inv_del_favorite hinv _
  | comment me x cm hme hx hid har hau =>
    obtain ⟨hm, -⟩ := userById_some hme
    simp only [applyAll, List.foldl_cons, List.foldl_nil]
    have h1 := inv_set_counter hinv { s.counter with last_comment := cm.id }
      (by simp [Snapshot.toSt]) (by simp [Snapshot.toSt]) (by simp [Snapshot.toSt, hid])
    exact inv_put_comment h1 cm (by simp [applyWrite]) ⟨x, hx, har.symm⟩ ⟨me, hm, hau.symm⟩
  | uncomment me cm id hme hcm hcid hau =>
    simp only [applyAll, List.foldl_cons, List.foldl_nil]
    exact inv_del_comment hinv id

theorem init_inv : Inv init := by
  constructor <;> simp [init]

/-- The invariants hold in every state the site can reach. -/
theorem reachable_inv {s : St} (h : Reachable s) : Inv s := by
  induction h with
  | init => exact init_inv
  | step _ ht ih => exact inv_preserved _ _ _ _ _ ih ht

/-! ## Permissions, stated directly -/

theorem same_by_id {α} (l : List α) (key : α → U64) (h : (l.map key).Nodup) {x y : α} (hx : x ∈ l) (hy : y ∈ l)
    (he : key x = key y) : x = y :=
  List.inj_on_of_nodup_map h hx hy he

/-- Only an article's author changes it, and the author stays the same. -/
theorem only_author_edits (a : Principal) (s : Snapshot) (c : Command) ws r
    (hr : Reachable (Snapshot.toSt s)) (h : transition a s c = .ok (.Ok (ws, r)))
    (x : Article) (hx : .PutArticle x ∈ ws.val) (b : Article) (hb : b ∈ (Snapshot.toSt s).articles) (hid : b.id = x.id) :
    b.author = a.user ∧ x.author = a.user := by
  have hi := reachable_inv hr
  obtain ⟨hau, -, hnew | ⟨b', hb', hid', hau'⟩⟩ := authorized a s c ws r h _ hx
  · have := hi.article_fresh b hb; rw [hid] at this; omega
  · exact ⟨same_by_id _ _ hi.article_ids hb hb' (hid.trans hid'.symm) ▸ hau', hau⟩

/-- Only an article's author deletes it or the rows that point at it. -/
theorem only_author_deletes (a : Principal) (s : Snapshot) (c : Command) ws r
    (hr : Reachable (Snapshot.toSt s)) (h : transition a s c = .ok (.Ok (ws, r))) (id : U64)
    (hx : .DelArticle id ∈ ws.val ∨ .DelTagsOf id ∈ ws.val ∨ .DelFavoritesOf id ∈ ws.val ∨ .DelCommentsOf id ∈ ws.val)
    (b : Article) (hb : b ∈ (Snapshot.toSt s).articles) (hid : b.id = id) : b.author = a.user := by
  have hi := reachable_inv hr
  have hw : wrote (Snapshot.toSt s) a.user id := by
    rcases hx with hx | hx | hx | hx <;> exact authorized a s c ws r h _ hx
  obtain ⟨b', hb', hid', hau'⟩ := hw
  exact same_by_id _ _ hi.article_ids hb hb' (hid.trans hid'.symm) ▸ hau'

/-- Only a comment's author deletes it; the article's author cannot. -/
theorem only_comment_author_deletes (a : Principal) (s : Snapshot) (c : Command) ws r
    (hr : Reachable (Snapshot.toSt s)) (h : transition a s c = .ok (.Ok (ws, r))) (id : U64)
    (hx : .DelComment id ∈ ws.val) (cm : Comment) (hc : cm ∈ (Snapshot.toSt s).comments) (hid : cm.id = id) :
    cm.author = a.user := by
  have hi := reachable_inv hr
  obtain ⟨cm', hc', hid', hau'⟩ := authorized a s c ws r h _ hx
  exact same_by_id _ _ hi.comment_ids hc hc' (hid.trans hid'.symm) ▸ hau'

/-- Follow and favorite rows written by a command belong to the caller. -/
theorem own_rows (a : Principal) (s : Snapshot) (c : Command) ws r (h : transition a s c = .ok (.Ok (ws, r))) :
    (∀ f, .PutFollow f ∈ ws.val ∨ .DelFollow f ∈ ws.val → f.follower = a.user) ∧
    (∀ f, .PutFavorite f ∈ ws.val ∨ .DelFavorite f ∈ ws.val → f.user = a.user) := by
  refine ⟨fun f hf => ?_, fun f hf => ?_⟩
  · rcases hf with hf | hf
    · exact (authorized a s c ws r h _ hf).1
    · exact (authorized a s c ws r h _ hf).1
  · rcases hf with hf | hf
    · exact (authorized a s c ws r h _ hf).1
    · exact (authorized a s c ws r h _ hf).1

/-- A caller without an account can only sign up. -/
theorem anonymous_only_signs_up (a : Principal) (s : Snapshot) (c : Command) ws r
    (hr : Reachable (Snapshot.toSt s)) (h : transition a s c = .ok (.Ok (ws, r)))
    (hanon : ¬ isUser (Snapshot.toSt s) a.user) :
    ∀ w ∈ ws.val, (∃ k, w = .SetCounter k) ∨ (∃ x, w = .PutUser x ∧ x.id.val = (Snapshot.toSt s).lastUser + 1) := by
  have hi := reachable_inv hr
  have nowrote : ∀ id, ¬ wrote (Snapshot.toSt s) a.user id := fun id ⟨b, hb, _, hau⟩ =>
    hanon (hau ▸ hi.article_authors b hb)
  intro w hw
  have := authorized a s c ws r h w hw
  cases w with
  | SetCounter k => exact .inl ⟨k, rfl⟩
  | PutUser x => rcases this with h1 | ⟨-, h2⟩; exact .inr ⟨x, rfl, h1⟩; exact absurd h2 hanon
  | PutFollow f => exact absurd this.2.1 hanon
  | DelFollow f => exact absurd this.2 hanon
  | PutFavorite f => exact absurd this.2.1 hanon
  | DelFavorite f => exact absurd this.2 hanon
  | PutArticle x => exact absurd this.2.1 hanon
  | PutTag t => exact absurd this.2 hanon
  | DelArticle id => exact absurd this (nowrote id)
  | DelTagsOf id => exact absurd this (nowrote id)
  | DelFavoritesOf id => exact absurd this (nowrote id)
  | DelCommentsOf id => exact absurd this (nowrote id)
  | PutComment x => exact absurd this.2.1 hanon
  | DelComment id =>
    obtain ⟨cm, hc, -, hau⟩ := this
    exact absurd (hau ▸ hi.comment_authors cm hc) hanon

end conduit_kernel.Invariants
