import Replies
/-!
# Issue #16, machine-checked

Upstream's `favorited` is `exists(select 1 from article_favorite where
user_id = $1)`: "the caller favorited any article". Bob favorited only "a",
yet upstream shows "b" as favorited. `upstream_violates_reply_spec` shows
`Replies.get_article_reply` fails for upstream. The `?favorited=` filter has
the same bug.
-/
open Aeneas Aeneas.Std Result conduit_kernel conduit_kernel.Spec conduit_kernel.Helpers
  conduit_kernel.Commands conduit_kernel.Replies H5iAppLib

namespace conduit_kernel.Counterexample

def empty : Text := alloc.vec.Vec.new U8
def slugA : Text := vecOf [97#u8]
def slugB : Text := vecOf [98#u8]
def bobName : Text := vecOf [98#u8, 111#u8, 98#u8]

def alice : User := ⟨1#u64, vecOf [97#u8, 108#u8], vecOf [97#u8], empty, empty, empty⟩
def bobUser : User := ⟨2#u64, bobName, vecOf [98#u8], empty, empty, empty⟩
def artA : Article := ⟨1#u64, 1#u64, slugA, slugA, empty, empty, 0#u64, 0#u64⟩
def artB : Article := ⟨2#u64, 1#u64, slugB, slugB, empty, empty, 0#u64, 0#u64⟩

def bob : Principal := ⟨1#u64, 2#u64⟩

/-- Alice wrote "a" and "b"; Bob favorited "a". -/
def s0 : Snapshot where
  counter := ⟨2#u64, 2#u64, 0#u64⟩
  users := vecOf [alice, bobUser]
  follows := vecOf []
  articles := vecOf [artA, artB]
  tags := vecOf []
  favorites := vecOf [⟨1#u64, 2#u64⟩]
  comments := vecOf []


theorem s0_facts :
    articleBySlug (Snapshot.toSt s0) slugB = some artB ∧
    favorited (Snapshot.toSt s0) bob.user artB.id = false ∧ favoritedUpstream (Snapshot.toSt s0) bob.user = true :=
  ⟨rfl, rfl, rfl⟩

/-- The fixed kernel: "b" is not favorited by Bob. -/
theorem fixed_get_b :
    transition bob s0 (.GetArticle slugB) ⦃ r => ∃ v, r = .Ok (alloc.vec.Vec.new Write, .Article v) ∧
      v.article = artB ∧ v.favorited = false ⦄ := by
  apply WP.spec_mono (get_article_reply bob s0 slugB)
  intro r hr
  rw [s0_facts.1] at hr
  obtain ⟨v, rfl, hv⟩ := hr
  have h1 := congrArg View.article hv
  have h2 := congrArg View.favorited hv
  simp only [ArticleView.toView, view] at h1 h2
  exact ⟨v, rfl, h1, h2.trans s0_facts.2.1⟩

/-- Upstream: "b" shows as favorited, because Bob favorited "a". -/
theorem upstream_get_b :
    transition_upstream bob s0 (.GetArticle slugB) ⦃ r => ∃ v, r = .Ok (alloc.vec.Vec.new Write, .Article v) ∧
      v.article = artB ∧ v.favorited = true ⦄ := by
  simp only [transition_upstream, step]
  apply WP.spec_mono (get_article_spec s0 bob.user slugB true)
  intro r hr
  rw [s0_facts.1] at hr
  obtain ⟨v, rfl, hv⟩ := hr
  have h1 := congrArg View.article hv
  have h2 := congrArg View.favorited hv
  simp only [ArticleView.toView, viewWith, favFlag, if_true] at h1 h2
  exact ⟨v, rfl, h1, h2.trans s0_facts.2.2⟩

/-- The correctness statement that `get_article_reply` proves for the fixed
kernel is false for upstream. -/
theorem upstream_violates_reply_spec :
    ¬ ∀ (a : Principal) (s : Snapshot) (slug : Text) ws v,
        transition_upstream a s (.GetArticle slug) = .ok (.Ok (ws, .Article v)) →
        v.favorited = favorited (Snapshot.toSt s) a.user v.article.id := by
  intro hall
  obtain ⟨r, hr, v, rfl, hart, hfav⟩ := (WP.spec_equiv_exists _ _).1 upstream_get_b
  have := hall bob s0 slugB _ v hr
  rw [hfav, hart, s0_facts.2.1] at this
  exact Bool.noConfusion this

/-- `?favorited=bob`: the fixed kernel lists only "a"; upstream lists both. -/
theorem favorited_filter :
    (listing (Snapshot.toSt s0) 0#u64 none none (some bobName) 20 0).map (·.article) = [artA] ∧
    (listingWith true (Snapshot.toSt s0) 0#u64 none none (some bobName) 20 0).map (·.article) = [artB, artA] :=
  ⟨rfl, rfl⟩

end conduit_kernel.Counterexample
