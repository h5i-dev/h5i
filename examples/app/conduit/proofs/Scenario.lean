import Counterexample
/-!
# Scenarios

Concrete runs on a small site: guarded actions succeed for the right user
and fail for others, so the permission theorems are not vacuous.
-/
open Aeneas Aeneas.Std Result conduit_kernel conduit_kernel.Spec conduit_kernel.Helpers
  conduit_kernel.Commands conduit_kernel.Counterexample H5iAppLib

namespace conduit_kernel.Scenario

def hi : Text := vecOf [104#u8, 105#u8]

/-- `s0`, plus Bob's comment on "a". -/
def s1 : Snapshot := { s0 with
  counter := ⟨2#u64, 2#u64, 1#u64⟩
  comments := vecOf [⟨1#u64, 1#u64, 2#u64, hi, 0#u64⟩] }

def aliceP : Principal := ⟨1#u64, 1#u64⟩
def anon : Principal := ⟨1#u64, 0#u64⟩

theorem author_edits :
    transition aliceP s1 (.UpdateArticle slugA none none none (some hi) 5#u64) ⦃ o =>
      ∃ ws r, o = .Ok (ws, r) ∧ ws.val = [.PutArticle ⟨1#u64, 1#u64, slugA, slugA, empty, hi, 0#u64, 5#u64⟩] ⦄ := by
  have h1 : List.find? (fun x => decide (x.id = aliceP.user)) s1.users.val = some alice := rfl
  have h2 : List.find? (fun x => decide (x.slug = slugA)) s1.articles.val = some artA := rfl
  simp only [transition, step, update_article, find_user_eq, find_article_eq, bind_ok, h1, h2]
  step*
  all_goals simp_all [artA, artB, alice, bobUser]

def c1 : Comment := ⟨1#u64, 1#u64, 2#u64, hi, 0#u64⟩
def bobP : Principal := bob

theorem u1 : List.find? (fun x => decide (x.id = aliceP.user)) s1.users.val = some alice := rfl
theorem u2 : List.find? (fun x => decide (x.id = bobP.user)) s1.users.val = some bobUser := rfl
theorem u0 : List.find? (fun x => decide (x.id = anon.user)) s1.users.val = none := rfl
theorem aA : List.find? (fun x => decide (x.slug = slugA)) s1.articles.val = some artA := rfl
theorem aB : List.find? (fun x => decide (x.slug = slugB)) s1.articles.val = some artB := rfl
theorem cm1 : List.find? (fun x => decide (x.id = 1#u64)) s1.comments.val = some c1 := rfl
theorem nAl : List.find? (fun x => decide (x.username = alice.username)) s1.users.val = some alice := rfl
theorem eNew : List.find? (fun x => decide (x.email = hi)) s1.users.val = none := rfl
theorem nNew : List.find? (fun x => decide (x.username = hi)) s1.users.val = none := rfl

theorem s1_counter : s1.counter = ⟨2#u64, 2#u64, 1#u64⟩ := rfl
theorem s1_favs : s1.favorites.val = [⟨1#u64, 2#u64⟩] := rfl
theorem s1_tags : s1.tags.val = [] := rfl

/-- Run a command on `s1`: resolve the lookups, then step. -/
macro "run" f:ident : tactic => `(tactic| (
  simp only [transition, step, $f:ident, find_user_eq, find_article_eq, find_comment_eq, find_user_by_name_eq,
    find_user_by_email_eq, bind_ok, u0, u1, u2, aA, aB, cm1, nAl, eNew, nNew]
  step*
  all_goals simp_all [artA, artB, alice, bobUser, c1, s1_counter, s1_favs, s1_tags, core.num.U64.MAX, U64.rMax]
  all_goals try scalar_tac))

theorem other_cannot_edit :
    transition bobP s1 (.UpdateArticle slugA none none none (some hi) 5#u64) ⦃ o => o = .Err .Forbidden ⦄ := by
  run update_article

theorem author_deletes :
    transition aliceP s1 (.DeleteArticle slugA) ⦃ o => ∃ ws r, o = .Ok (ws, r) ∧
      ws.val = [.DelTagsOf 1#u64, .DelFavoritesOf 1#u64, .DelCommentsOf 1#u64, .DelArticle 1#u64] ⦄ := by
  run delete_article

theorem other_cannot_delete :
    transition bobP s1 (.DeleteArticle slugA) ⦃ o => o = .Err .Forbidden ⦄ := by
  run delete_article

theorem comment_author_deletes :
    transition bobP s1 (.DeleteComment slugA 1#u64) ⦃ o => ∃ ws r, o = .Ok (ws, r) ∧ ws.val = [.DelComment 1#u64] ⦄ := by
  run delete_comment

/-- Upstream only lets the comment's author delete it, not the article's. -/
theorem article_author_cannot_delete_comment :
    transition aliceP s1 (.DeleteComment slugA 1#u64) ⦃ o => o = .Err .Forbidden ⦄ := by
  run delete_comment

theorem anonymous_cannot_favorite :
    transition anon s1 (.Favorite slugA) ⦃ o => o = .Err .Unauthorized ⦄ := by
  run favorite

theorem cannot_follow_self :
    transition aliceP s1 (.Follow alice.username) ⦃ o => o = .Err .Forbidden ⦄ := by
  run follow

theorem bob_follows_alice :
    transition bobP s1 (.Follow alice.username) ⦃ o => ∃ ws r, o = .Ok (ws, r) ∧
      ws.val = [.PutFollow ⟨2#u64, 1#u64⟩] ⦄ := by
  run follow

theorem slug_taken :
    transition aliceP s1 (.CreateArticle slugB slugB empty empty (vecOf []) 5#u64) ⦃ o => o = .Err .SlugTaken ⦄ := by
  run create_article

theorem bob_favorites_b :
    transition bobP s1 (.Favorite slugB) ⦃ o => ∃ ws v, o = .Ok (ws, .Article v) ∧
      ws.val = [.PutFavorite ⟨2#u64, 2#u64⟩] ∧ v.favorited = true ∧ v.favorites.val = 1 ⦄ := by
  run favorite

theorem anyone_signs_up :
    transition anon s1 (.Register hi hi hi) ⦃ o => ∃ ws r, o = .Ok (ws, r) ∧
      ws.val = [.SetCounter ⟨3#u64, 2#u64, 1#u64⟩, .PutUser ⟨3#u64, hi, hi, hi, alloc.vec.Vec.new U8, alloc.vec.Vec.new U8⟩] ⦄ := by
  run register

/-! ## `s1` is reachable

Six commands lead from the empty site to `s1`, so the theorems stated over
`Reachable` states apply to it. -/

def t0 : Snapshot := ⟨⟨0#u64, 0#u64, 0#u64⟩, vecOf [], vecOf [], vecOf [], vecOf [], vecOf [], vecOf []⟩
def t1 : Snapshot := { t0 with counter := ⟨1#u64, 0#u64, 0#u64⟩, users := vecOf [alice] }
def t2 : Snapshot := { t0 with counter := ⟨2#u64, 0#u64, 0#u64⟩, users := vecOf [alice, bobUser] }
def t3 : Snapshot := { t2 with counter := ⟨2#u64, 1#u64, 0#u64⟩, articles := vecOf [artA] }
def t4 : Snapshot := { t2 with counter := ⟨2#u64, 2#u64, 0#u64⟩, articles := vecOf [artA, artB] }

theorem reach_step {s t : Snapshot} {a : Principal} {c : Command} {L : List Write}
    (hs : Reachable (Snapshot.toSt s)) (hrun : transition a s c ⦃ o => ∃ ws r, o = .Ok (ws, r) ∧ ws.val = L ⦄)
    (ht : applyAll (Snapshot.toSt s) L = Snapshot.toSt t) : Reachable (Snapshot.toSt t) := by
  obtain ⟨o, ho, ws, r, rfl, hws⟩ := (WP.spec_equiv_exists _ _).1 hrun
  have := Reachable.step hs ho
  rwa [hws, ht] at this

theorem step1 : transition anon t0 (.Register alice.username alice.email empty) ⦃ o => ∃ ws r, o = .Ok (ws, r) ∧
    ws.val = [.SetCounter ⟨1#u64, 0#u64, 0#u64⟩, .PutUser alice] ⦄ := by
  have h1 : List.find? (fun x => decide (x.username = alice.username)) t0.users.val = none := rfl
  have h2 : List.find? (fun x => decide (x.email = alice.email)) t0.users.val = none := rfl
  have hc : t0.counter = ⟨0#u64, 0#u64, 0#u64⟩ := rfl
  simp only [transition, step, register, find_user_by_name_eq, find_user_by_email_eq, bind_ok, h1, h2]
  step*
  all_goals simp_all [alice, empty, core.num.U64.MAX, U64.rMax]
  all_goals first | scalar_tac | rfl

theorem step2 : transition anon t1 (.Register bobName bobUser.email empty) ⦃ o => ∃ ws r, o = .Ok (ws, r) ∧
    ws.val = [.SetCounter ⟨2#u64, 0#u64, 0#u64⟩, .PutUser bobUser] ⦄ := by
  have h1 : List.find? (fun x => decide (x.username = bobName)) t1.users.val = none := rfl
  have h2 : List.find? (fun x => decide (x.email = bobUser.email)) t1.users.val = none := rfl
  have hc : t1.counter = ⟨1#u64, 0#u64, 0#u64⟩ := rfl
  simp only [transition, step, register, find_user_by_name_eq, find_user_by_email_eq, bind_ok, h1, h2]
  step*
  all_goals simp_all [bobUser, empty, core.num.U64.MAX, U64.rMax]
  all_goals first | scalar_tac | rfl

theorem step3 : transition aliceP t2 (.CreateArticle slugA slugA empty empty (vecOf []) 0#u64) ⦃ o =>
    ∃ ws r, o = .Ok (ws, r) ∧ ws.val = [.SetCounter ⟨2#u64, 1#u64, 0#u64⟩, .PutArticle artA] ⦄ := by
  have h1 : List.find? (fun x => decide (x.id = aliceP.user)) t2.users.val = some alice := rfl
  have h2 : List.find? (fun x => decide (x.slug = slugA)) t2.articles.val = none := rfl
  have hc : t2.counter = ⟨2#u64, 0#u64, 0#u64⟩ := rfl
  simp only [transition, step, create_article, find_user_eq, find_article_eq, bind_ok, h1, h2]
  step*
  all_goals simp_all [alice, artA, core.num.U64.MAX, U64.rMax, distinct, vecOf]
  all_goals first | scalar_tac | rfl

theorem step4 : transition aliceP t3 (.CreateArticle slugB slugB empty empty (vecOf []) 0#u64) ⦃ o =>
    ∃ ws r, o = .Ok (ws, r) ∧ ws.val = [.SetCounter ⟨2#u64, 2#u64, 0#u64⟩, .PutArticle artB] ⦄ := by
  have h1 : List.find? (fun x => decide (x.id = aliceP.user)) t3.users.val = some alice := rfl
  have h2 : List.find? (fun x => decide (x.slug = slugB)) t3.articles.val = none := rfl
  have hc : t3.counter = ⟨2#u64, 1#u64, 0#u64⟩ := rfl
  simp only [transition, step, create_article, find_user_eq, find_article_eq, bind_ok, h1, h2]
  step*
  all_goals simp_all [alice, artB, core.num.U64.MAX, U64.rMax, distinct, vecOf]
  all_goals first | scalar_tac | rfl

theorem step5 : transition bobP t4 (.Favorite slugA) ⦃ o =>
    ∃ ws r, o = .Ok (ws, r) ∧ ws.val = [.PutFavorite ⟨1#u64, 2#u64⟩] ⦄ := by
  have h1 : List.find? (fun x => decide (x.id = bobP.user)) t4.users.val = some bobUser := rfl
  have h2 : List.find? (fun x => decide (x.slug = slugA)) t4.articles.val = some artA := rfl
  have hf : t4.favorites.val = [] := rfl
  simp only [transition, step, favorite, find_user_eq, find_article_eq, bind_ok, h1, h2]
  step*
  all_goals simp_all [bobUser, artA, core.num.U64.MAX, U64.rMax]
  all_goals scalar_tac

theorem step6 : transition bobP s0 (.AddComment slugA hi 0#u64) ⦃ o =>
    ∃ ws r, o = .Ok (ws, r) ∧ ws.val = [.SetCounter ⟨2#u64, 2#u64, 1#u64⟩, .PutComment c1] ⦄ := by
  have h1 : List.find? (fun x => decide (x.id = bobP.user)) s0.users.val = some bobUser := rfl
  have h2 : List.find? (fun x => decide (x.slug = slugA)) s0.articles.val = some artA := rfl
  have hc : s0.counter = ⟨2#u64, 2#u64, 0#u64⟩ := rfl
  simp only [transition, step, add_comment, find_user_eq, find_article_eq, bind_ok, h1, h2]
  step*
  all_goals simp_all [bobUser, artA, c1, core.num.U64.MAX, U64.rMax]
  all_goals first | scalar_tac | rfl

theorem s1_reachable : Reachable (Snapshot.toSt s1) := by
  have r0 : Reachable (Snapshot.toSt t0) := Reachable.init
  have r1 := reach_step (t := t1) r0 step1 rfl
  have r2 := reach_step (t := t2) r1 step2 rfl
  have r3 := reach_step (t := t3) r2 step3 rfl
  have r4 := reach_step (t := t4) r3 step4 rfl
  have r5 := reach_step (t := s0) r4 step5 rfl
  exact reach_step (t := s1) r5 step6 rfl

end conduit_kernel.Scenario
