import ConduitKernel
import H5iAppLib
/-!
# What Conduit should do

This is the file to review. It describes the state as lists, what the
replies should contain, who may make which write, what a write does, and
which facts hold in every reachable state.
-/
open Aeneas Aeneas.Std conduit_kernel

namespace conduit_kernel.Spec

abbrev Text := alloc.vec.Vec U8

/-- The state as lists; the counters are the last ids handed out. -/
structure St where
  lastUser : Nat
  lastArticle : Nat
  lastComment : Nat
  users : List User
  follows : List Follow
  articles : List Article
  tags : List Tag
  favs : List Favorite
  comments : List Comment

def Snapshot.toSt (s : Snapshot) : St :=
  ⟨s.counter.last_user.val, s.counter.last_article.val, s.counter.last_comment.val,
    s.users.val, s.follows.val, s.articles.val, s.tags.val, s.favorites.val, s.comments.val⟩

/-! ## Lookups -/

def userById (s : St) (id : U64) : Option User := s.users.find? (·.id = id)
def userByName (s : St) (n : Text) : Option User := s.users.find? (·.username = n)
def userByEmail (s : St) (e : Text) : Option User := s.users.find? (·.email = e)
def articleBySlug (s : St) (slug : Text) : Option Article := s.articles.find? (·.slug = slug)
def commentById (s : St) (id : U64) : Option Comment := s.comments.find? (·.id = id)

def isUser (s : St) (u : U64) : Prop := ∃ x ∈ s.users, x.id = u
def isArticle (s : St) (id : U64) : Prop := ∃ a ∈ s.articles, a.id = id
def wrote (s : St) (u id : U64) : Prop := ∃ a ∈ s.articles, a.id = id ∧ a.author = u

/-! ## What replies contain -/

/-- `u` follows `v`. -/
def follows (s : St) (u v : U64) : Bool := s.follows.any (fun f => f.follower = u ∧ f.followed = v)

/-- `u` favorited article `a`. -/
def favorited (s : St) (u a : U64) : Bool := s.favs.any (fun f => f.article = a ∧ f.user = u)

/-- What upstream reports as `favorited` (issue #16): `u` favorited some article. -/
def favoritedUpstream (s : St) (u : U64) : Bool := s.favs.any (fun f => f.user = u)

def favCount (s : St) (a : U64) : Nat := (s.favs.filter (·.article = a)).length

def tagsOf (s : St) (a : U64) : List Text := (s.tags.filter (·.article = a)).map (·.tag)

def noProfile : Profile := ⟨alloc.vec.Vec.new U8, alloc.vec.Vec.new U8, alloc.vec.Vec.new U8, false⟩

/-- A user as `viewer` sees them. -/
def profileOf (s : St) (viewer : U64) (x : User) : Profile :=
  ⟨x.username, x.bio, x.image, follows s viewer x.id⟩

def authorOf (s : St) (viewer id : U64) : Profile :=
  match userById s id with
  | some x => profileOf s viewer x
  | none => noProfile

/-- An article as the reply shows it, with counts as numbers. -/
structure View where
  article : Article
  tags : List Text
  favorited : Bool
  favorites : Nat
  author : Profile

def view (s : St) (viewer : U64) (a : Article) : View :=
  ⟨a, tagsOf s a.id, favorited s viewer a.id, favCount s a.id, authorOf s viewer a.author⟩

def _root_.conduit_kernel.ArticleView.toView (v : ArticleView) : View :=
  ⟨v.article, v.tags.val, v.favorited, v.favorites.val, v.author⟩

/-- Newest first (the list is in creation order), skip `offset`, keep `limit`. -/
def pageOf (l : List Article) (offset limit : Nat) : List Article :=
  (l.reverse.drop offset).take limit

/-- The filters of `GET /api/articles`, with user names resolved to ids. -/
def listed (s : St) (tag : Option Text) (author fav : Option U64) (a : Article) : Bool :=
  tag.all (fun t => s.tags.any (fun x => x.article = a.id ∧ x.tag = t)) &&
    author.all (a.author = ·) && fav.all (fun u => favorited s u a.id)

/-- A name filter: `some none` if absent, `none` if nobody has that name. -/
def resolveName (s : St) : Option Text → Option (Option U64)
  | none => some none
  | some n => (userByName s n).map (fun x => some x.id)

def listing (s : St) (viewer : U64) (tag author fav : Option Text) (limit offset : Nat) : List View :=
  match resolveName s author, resolveName s fav with
  | some a, some f => (pageOf (s.articles.filter (listed s tag a f)) offset limit).map (view s viewer)
  | _, _ => []

/-- The feed: articles by authors `u` follows. -/
def feedOf (s : St) (u : U64) (limit offset : Nat) : List View :=
  (pageOf (s.articles.filter (fun a => follows s u a.author)) offset limit).map (view s u)

def commentsOf (s : St) (viewer a : U64) : List CommentView :=
  (s.comments.filter (·.article = a)).map (fun c => ⟨c, authorOf s viewer c.author⟩)

/-- Each element once, first occurrence first. -/
def distinct {α} [DecidableEq α] (l : List α) : List α :=
  l.foldl (fun acc x => if acc.any (· = x) then acc else acc ++ [x]) []

def allTags (s : St) : List Text := distinct (s.tags.map (·.tag))

/-! ## The policy -/

/-- May user `u` make write `w` in state `s`? -/
def allowed (s : St) (u : U64) : Write → Prop
  -- Sign-up with the next id, or an edit of one's own account.
  | .PutUser x => x.id.val = s.lastUser + 1 ∨ (x.id = u ∧ isUser s u)
  -- Follows and favorites: only the caller's own rows.
  | .PutFollow f => f.follower = u ∧ isUser s u ∧ f.followed ≠ u ∧ isUser s f.followed
  | .DelFollow f => f.follower = u ∧ isUser s u
  | .PutFavorite f => f.user = u ∧ isUser s u ∧ isArticle s f.article
  | .DelFavorite f => f.user = u ∧ isUser s u
  -- A new article by the caller, or an edit by its author.
  | .PutArticle a => a.author = u ∧ isUser s u ∧ (a.id.val = s.lastArticle + 1 ∨ wrote s u a.id)
  | .PutTag t => t.article.val = s.lastArticle + 1 ∧ isUser s u
  -- Deleting an article and its rows: only its author.
  | .DelArticle id => wrote s u id
  | .DelTagsOf id => wrote s u id
  | .DelFavoritesOf id => wrote s u id
  | .DelCommentsOf id => wrote s u id
  -- A new comment by the caller on an existing article.
  | .PutComment c => c.author = u ∧ isUser s u ∧ c.id.val = s.lastComment + 1 ∧ isArticle s c.article
  -- Deleting a comment: only its author.
  | .DelComment id => ∃ c ∈ s.comments, c.id = id ∧ c.author = u
  -- Counters never go back.
  | .SetCounter c =>
    s.lastUser ≤ c.last_user.val ∧ s.lastArticle ≤ c.last_article.val ∧ s.lastComment ≤ c.last_comment.val

/-! ## What writes do

`H5iAppLib.upsert` replaces the row with the same key, or appends it. -/

def applyWrite (s : St) : Write → St
  | .PutUser x => { s with users := H5iAppLib.upsert (·.id) x s.users }
  | .PutFollow f => { s with follows := H5iAppLib.upsert (fun f => (f.follower, f.followed)) f s.follows }
  | .DelFollow f => { s with follows := s.follows.filter (fun g => ¬(g.follower = f.follower ∧ g.followed = f.followed)) }
  | .PutArticle a => { s with articles := H5iAppLib.upsert (·.id) a s.articles }
  | .DelArticle id => { s with articles := s.articles.filter (·.id ≠ id) }
  | .PutTag t => { s with tags := H5iAppLib.upsert (fun t => (t.article, t.tag)) t s.tags }
  | .DelTagsOf id => { s with tags := s.tags.filter (·.article ≠ id) }
  | .PutFavorite f => { s with favs := H5iAppLib.upsert (fun f => (f.article, f.user)) f s.favs }
  | .DelFavorite f => { s with favs := s.favs.filter (fun g => ¬(g.article = f.article ∧ g.user = f.user)) }
  | .DelFavoritesOf id => { s with favs := s.favs.filter (·.article ≠ id) }
  | .PutComment c => { s with comments := H5iAppLib.upsert (·.id) c s.comments }
  | .DelComment id => { s with comments := s.comments.filter (·.id ≠ id) }
  | .DelCommentsOf id => { s with comments := s.comments.filter (·.article ≠ id) }
  | .SetCounter c => { s with lastUser := c.last_user.val, lastArticle := c.last_article.val,
                              lastComment := c.last_comment.val }

def applyAll (s : St) (ws : List Write) : St := ws.foldl applyWrite s

/-! ## Invariants -/

/-- Facts that hold in every reachable state. -/
structure Inv (s : St) : Prop where
  user_ids : (s.users.map (·.id)).Nodup
  user_fresh : ∀ x ∈ s.users, 0 < x.id.val ∧ x.id.val ≤ s.lastUser
  usernames : (s.users.map (·.username)).Nodup
  emails : (s.users.map (·.email)).Nodup
  follow_keys : (s.follows.map (fun f => (f.follower, f.followed))).Nodup
  follow_self : ∀ f ∈ s.follows, f.follower ≠ f.followed
  follow_users : ∀ f ∈ s.follows, isUser s f.follower ∧ isUser s f.followed
  article_ids : (s.articles.map (·.id)).Nodup
  article_fresh : ∀ a ∈ s.articles, a.id.val ≤ s.lastArticle
  slugs : (s.articles.map (·.slug)).Nodup
  article_authors : ∀ a ∈ s.articles, isUser s a.author
  tag_keys : (s.tags.map (fun t => (t.article, t.tag))).Nodup
  tag_articles : ∀ t ∈ s.tags, isArticle s t.article
  fav_keys : (s.favs.map (fun f => (f.article, f.user))).Nodup
  fav_articles : ∀ f ∈ s.favs, isArticle s f.article
  fav_users : ∀ f ∈ s.favs, isUser s f.user
  comment_ids : (s.comments.map (·.id)).Nodup
  comment_fresh : ∀ c ∈ s.comments, c.id.val ≤ s.lastComment
  comment_articles : ∀ c ∈ s.comments, isArticle s c.article
  comment_authors : ∀ c ∈ s.comments, isUser s c.author

def init : St := ⟨0, 0, 0, [], [], [], [], [], []⟩

/-- The states reachable from an empty site by successful commands. -/
inductive Reachable : St → Prop
  | init : Reachable init
  | step {a s c ws r} : Reachable (Snapshot.toSt s) → transition a s c = .ok (.Ok (ws, r)) →
      Reachable (applyAll (Snapshot.toSt s) ws.val)

end conduit_kernel.Spec
