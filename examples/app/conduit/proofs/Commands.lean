import Helpers
/-!
# What each command does

One lemma per command, in terms of `Spec.lean`: what a successful run writes
and replies, and which facts about the state made it succeed. Read-only
commands get an exact description of their result.
-/
open Aeneas Aeneas.Std Result conduit_kernel conduit_kernel.Spec conduit_kernel.Helpers H5iAppLib

namespace conduit_kernel.Commands

/-- The listing and the feed in both variants. -/
def listingWith (up : Bool) (s : St) (viewer : U64) (tag author fav : Option Text) (limit offset : Nat) : List View :=
  match resolveName s author, resolveName s fav with
  | some a, some f => (pageOf (s.articles.filter (listedWith up s tag a f)) offset limit).map (viewWith up s viewer)
  | _, _ => []

def feedWith (up : Bool) (s : St) (u : U64) (limit offset : Nat) : List View :=
  (pageOf (s.articles.filter (fun a => follows s u a.author)) offset limit).map (viewWith up s u)

@[simp] theorem listingWith_false : listingWith false = listing := rfl
@[simp] theorem feedWith_false : feedWith false = feedOf := rfl

def acct (x : User) : Account := ⟨x.id, x.email, x.username, x.bio, x.image⟩

/-- Run a command: rewrite its lookups to `find?`, split on the results,
then step through the rest. -/
macro "walk_cmd" : tactic => `(tactic| (
  try simp only [find_user_eq, find_user_by_name_eq, find_user_by_email_eq, find_article_eq, find_comment_eq, bind_ok]
  repeat' split
  all_goals (try step*)))

/-- Close what is left with the facts collected on the way. -/
macro "close_cmd" : tactic => `(tactic| (
  all_goals try simp_all [-List.find?_eq_none, -List.find?_isSome, userById, userByName, userByEmail, articleBySlug, commentById,
    Snapshot.toSt, acct, U64.rMax, feedWith, ArticleView.toView, tagsOf, favCount, favorited]
  all_goals try scalar_tac))

/-! ## Users and profiles -/

theorem register_spec (s : Snapshot) (n e p : Text) :
    register s n e p ⦃ r => ∀ ws rep, r = .Ok (ws, rep) →
      userByName (Snapshot.toSt s) n = none ∧ userByEmail (Snapshot.toSt s) e = none ∧
      ∃ x : User, x.id.val = s.counter.last_user.val + 1 ∧ x.username = n ∧ x.email = e ∧
        ws.val = [.SetCounter { s.counter with last_user := x.id }, .PutUser x] ∧ rep = .Account (acct x) ⦄ := by
  unfold register
  walk_cmd
  close_cmd

theorem login_spec (s : Snapshot) (e p : Text) :
    login s e p ⦃ r => r = match userByEmail (Snapshot.toSt s) e with
      | none => .Err .UnknownEmail
      | some x => if x.password = p then .Ok (alloc.vec.Vec.new Write, .Account (acct x)) else .Err .WrongPassword ⦄ := by
  unfold login
  walk_cmd
  close_cmd

theorem current_user_spec (s : Snapshot) (u : U64) :
    current_user s u ⦃ r => r = match userById (Snapshot.toSt s) u with
      | none => .Err .Unauthorized
      | some x => .Ok (alloc.vec.Vec.new Write, .Account (acct x)) ⦄ := by
  unfold current_user
  walk_cmd
  close_cmd

theorem update_user_spec (s : Snapshot) (u : U64) (e n p b i : Option Text) :
    update_user s u e n p b i ⦃ r => ∀ ws rep, r = .Ok (ws, rep) →
      ∃ me, userById (Snapshot.toSt s) u = some me ∧
        (n.bind (userByName (Snapshot.toSt s))).any (·.id ≠ me.id) = false ∧
        (e.bind (userByEmail (Snapshot.toSt s))).any (·.id ≠ me.id) = false ∧
        ∃ x : User, x.id = me.id ∧ x.username = n.getD me.username ∧ x.email = e.getD me.email ∧
          ws.val = [.PutUser x] ∧ rep = .Account (acct x) ⦄ := by
  unfold update_user
  walk_cmd
  close_cmd

theorem get_profile_spec (s : Snapshot) (u : U64) (n : Text) :
    get_profile s u n ⦃ r => r = match userByName (Snapshot.toSt s) n with
      | none => .Err .NotFound
      | some x => .Ok (alloc.vec.Vec.new Write, .Profile (profileOf (Snapshot.toSt s) u x)) ⦄ := by
  unfold get_profile
  walk_cmd
  close_cmd

theorem follow_spec (s : Snapshot) (u : U64) (n : Text) :
    follow s u n ⦃ r => ∀ ws rep, r = .Ok (ws, rep) →
      ∃ me t, userById (Snapshot.toSt s) u = some me ∧ userByName (Snapshot.toSt s) n = some t ∧
        t.id ≠ me.id ∧ ws.val = [.PutFollow ⟨me.id, t.id⟩] ∧
        rep = .Profile ⟨t.username, t.bio, t.image, true⟩ ⦄ := by
  unfold follow
  walk_cmd
  close_cmd

theorem unfollow_spec (s : Snapshot) (u : U64) (n : Text) :
    unfollow s u n ⦃ r => ∀ ws rep, r = .Ok (ws, rep) →
      ∃ me t, userById (Snapshot.toSt s) u = some me ∧ userByName (Snapshot.toSt s) n = some t ∧
        ws.val = [.DelFollow ⟨me.id, t.id⟩] ∧ rep = .Profile ⟨t.username, t.bio, t.image, false⟩ ⦄ := by
  unfold unfollow
  walk_cmd
  close_cmd

/-! ## Reading articles -/

theorem resolve_eq (s : Snapshot) (n : Option Text) : resolve s.users n = ok (resolveName (Snapshot.toSt s) n) := by
  rw [eq_ok_of_spec (resolve_spec s.users n)]; cases n <;> rfl

theorem list_articles_spec (s : Snapshot) (u : U64) (tag author fav : Option Text) (lim off : U64) (up : Bool) :
    list_articles s u tag author fav lim off up ⦃ r => ∃ vs, r = .Ok (alloc.vec.Vec.new Write, .Articles vs) ∧
      vs.val.map ArticleView.toView = listingWith up (Snapshot.toSt s) u tag author fav lim.val off.val ⦄ := by
  unfold list_articles listingWith
  simp only [resolve_eq, bind_ok]
  cases resolveName (Snapshot.toSt s) author <;> cases resolveName (Snapshot.toSt s) fav <;> simp only [] <;> step*
  all_goals simp_all [Snapshot.toSt]

theorem feed_spec (s : Snapshot) (u : U64) (lim off : U64) (up : Bool) :
    feed s u lim off up ⦃ r => match userById (Snapshot.toSt s) u with
      | none => r = .Err .Unauthorized
      | some me => ∃ vs, r = .Ok (alloc.vec.Vec.new Write, .Articles vs) ∧
          vs.val.map ArticleView.toView = feedWith up (Snapshot.toSt s) me.id lim.val off.val ⦄ := by
  unfold feed
  walk_cmd
  close_cmd

theorem get_article_spec (s : Snapshot) (u : U64) (slug : Text) (up : Bool) :
    get_article s u slug up ⦃ r => match articleBySlug (Snapshot.toSt s) slug with
      | none => r = .Err .NotFound
      | some a => ∃ v, r = .Ok (alloc.vec.Vec.new Write, .Article v) ∧ v.toView = viewWith up (Snapshot.toSt s) u a ⦄ := by
  unfold get_article
  walk_cmd
  close_cmd

theorem get_comments_spec (s : Snapshot) (u : U64) (slug : Text) :
    get_comments s u slug ⦃ r => match articleBySlug (Snapshot.toSt s) slug with
      | none => r = .Err .NotFound
      | some a => ∃ cs, r = .Ok (alloc.vec.Vec.new Write, .Comments cs) ∧ cs.val = commentsOf (Snapshot.toSt s) u a.id ⦄ := by
  unfold get_comments
  walk_cmd
  close_cmd

/-! ## Writing articles -/

theorem create_article_spec (s : Snapshot) (u : U64) (slug title desc body : Text) (tags : alloc.vec.Vec Text) (now : U64) :
    create_article s u slug title desc body tags now ⦃ r => ∀ ws rep, r = .Ok (ws, rep) →
      ∃ me, userById (Snapshot.toSt s) u = some me ∧ articleBySlug (Snapshot.toSt s) slug = none ∧
        ∃ a : Article, a.id.val = s.counter.last_article.val + 1 ∧ a.author = me.id ∧ a.slug = slug ∧
          ws.val = [.SetCounter { s.counter with last_article := a.id }, .PutArticle a] ++
            (distinct tags.val).map (fun t => .PutTag ⟨a.id, t⟩) ∧
          ∃ v, rep = .Article v ∧ v.toView = ⟨a, distinct tags.val, false, 0, profileOf (Snapshot.toSt s) me.id me⟩ ⦄ := by
  unfold create_article
  walk_cmd
  close_cmd

theorem update_article_spec (s : Snapshot) (u : U64) (slug : Text) (ns t d b : Option Text) (now : U64) (up : Bool) :
    update_article s u slug ns t d b now up ⦃ r => ∀ ws rep, r = .Ok (ws, rep) →
      ∃ me a, userById (Snapshot.toSt s) u = some me ∧ articleBySlug (Snapshot.toSt s) slug = some a ∧
        a.author = me.id ∧ (ns.bind (articleBySlug (Snapshot.toSt s))).any (·.id ≠ a.id) = false ∧
        ∃ a2 : Article, a2.id = a.id ∧ a2.author = a.author ∧ a2.slug = ns.getD a.slug ∧
          ws.val = [.PutArticle a2] ∧ ∃ v, rep = .Article v ∧ v.toView = viewWith up (Snapshot.toSt s) me.id a2 ⦄ := by
  unfold update_article
  walk_cmd
  close_cmd

theorem delete_article_spec (s : Snapshot) (u : U64) (slug : Text) :
    delete_article s u slug ⦃ r => ∀ ws rep, r = .Ok (ws, rep) →
      ∃ me a, userById (Snapshot.toSt s) u = some me ∧ articleBySlug (Snapshot.toSt s) slug = some a ∧
        a.author = me.id ∧ ws.val = [.DelTagsOf a.id, .DelFavoritesOf a.id, .DelCommentsOf a.id, .DelArticle a.id] ⦄ := by
  unfold delete_article
  walk_cmd
  close_cmd

theorem favorite_spec (s : Snapshot) (u : U64) (slug : Text) :
    favorite s u slug ⦃ r => ∀ ws rep, r = .Ok (ws, rep) →
      ∃ me a, userById (Snapshot.toSt s) u = some me ∧ articleBySlug (Snapshot.toSt s) slug = some a ∧
        ws.val = [.PutFavorite ⟨a.id, me.id⟩] ∧
        ∃ v, rep = .Article v ∧ v.toView = ⟨a, tagsOf (Snapshot.toSt s) a.id, true,
          favCount (Snapshot.toSt s) a.id + (if favorited (Snapshot.toSt s) me.id a.id then 0 else 1),
          authorOf (Snapshot.toSt s) me.id a.author⟩ ⦄ := by
  unfold favorite
  walk_cmd
  close_cmd

theorem unfavorite_spec (s : Snapshot) (u : U64) (slug : Text) (up : Bool) :
    unfavorite s u slug up ⦃ r => ∀ ws rep, r = .Ok (ws, rep) →
      ∃ me a, userById (Snapshot.toSt s) u = some me ∧ articleBySlug (Snapshot.toSt s) slug = some a ∧
        ws.val = [.DelFavorite ⟨a.id, me.id⟩] ∧
        ∃ v, rep = .Article v ∧ v.toView = ⟨a, tagsOf (Snapshot.toSt s) a.id,
          up && (Snapshot.toSt s).favs.any (fun f => f.user = me.id ∧ f.article ≠ a.id),
          favCount (Snapshot.toSt s) a.id - (if favorited (Snapshot.toSt s) me.id a.id then 1 else 0),
          authorOf (Snapshot.toSt s) me.id a.author⟩ ⦄ := by
  unfold unfavorite
  walk_cmd
  close_cmd
  all_goals (split <;> simp)

/-! ## Comments -/

theorem add_comment_spec (s : Snapshot) (u : U64) (slug body : Text) (now : U64) :
    add_comment s u slug body now ⦃ r => ∀ ws rep, r = .Ok (ws, rep) →
      ∃ me a, userById (Snapshot.toSt s) u = some me ∧ articleBySlug (Snapshot.toSt s) slug = some a ∧
        ∃ c : Comment, c.id.val = s.counter.last_comment.val + 1 ∧ c.article = a.id ∧ c.author = me.id ∧
          ws.val = [.SetCounter { s.counter with last_comment := c.id }, .PutComment c] ∧
          rep = .Comment ⟨c, profileOf (Snapshot.toSt s) me.id me⟩ ⦄ := by
  unfold add_comment
  walk_cmd
  close_cmd

theorem delete_comment_spec (s : Snapshot) (u : U64) (slug : Text) (id : U64) :
    delete_comment s u slug id ⦃ r => ∀ ws rep, r = .Ok (ws, rep) →
      ∃ me a c, userById (Snapshot.toSt s) u = some me ∧ articleBySlug (Snapshot.toSt s) slug = some a ∧
        commentById (Snapshot.toSt s) id = some c ∧ c.article = a.id ∧ c.author = me.id ∧
        ws.val = [.DelComment id] ⦄ := by
  unfold delete_comment
  walk_cmd
  close_cmd

end conduit_kernel.Commands
