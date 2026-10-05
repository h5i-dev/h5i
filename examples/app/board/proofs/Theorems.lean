import Commands
/-!
# Theorems about the bulletin board

`writes_of` sums up `Commands.lean`: a successful command does one of six
things. The other theorems split on those six and never look at the code.
-/
open Aeneas Aeneas.Std Result board_kernel board_kernel.Spec board_kernel.Commands H5iAppLib

namespace board_kernel.Theorems

/-- What a successful command writes, in six cases. -/
theorem writes_of (a : Principal) (s : Snapshot) (c : Command) ws r
    (h : transition a s c = .ok (.Ok (ws, r))) :
    let st := Snapshot.toSt s
    -- List: nothing.
    ws.val = [] ∨
    -- Publish: a post with the next id, and the counter moved past it.
    (∃ t cnt, textOk t.val ∧ cnt.next_id.val = st.next + 1 ∧
      ws.val = [.PutPost ⟨s.counter.next_id, a.user, t⟩, .SetCounter cnt]) ∨
    -- Edit: new text for one of the caller's posts.
    (∃ id t q, textOk t.val ∧ findPost st id.val = some q ∧ q.author = a.user ∧
      ws.val = [.PutPost ⟨id, a.user, t⟩]) ∨
    -- Delete: an existing post, by its author or a moderator.
    (∃ id q, findPost st id.val = some q ∧ (q.author = a.user ∨ isMod st a.user.val) ∧
      ws.val = [.DelPost id]) ∨
    -- Promote: by a moderator, or the caller alone when there is none.
    (∃ tg, (isMod st a.user.val ∨ (st.mods = [] ∧ tg = a.user)) ∧ ws.val = [.PutModerator ⟨tg⟩]) ∨
    -- Demote: by a moderator, of a moderator, when there are at least two.
    (∃ tg, isMod st a.user.val ∧ isMod st tg.val ∧ 2 ≤ st.mods.length ∧ ws.val = [.DelModerator tg]) := by
  intro st
  cases c with
  | Publish t =>
    obtain ⟨ht, cnt, hc, hws⟩ := post_of_ok (publish_spec a.user s t) h ws r rfl
    exact .inr (.inl ⟨t, cnt, ht, hc, hws⟩)
  | Edit id t =>
    obtain ⟨ht, q, hq, ha, hws⟩ := post_of_ok (edit_spec a.user s id t) h ws r rfl
    exact .inr (.inr (.inl ⟨id, t, q, ht, hq, ha, hws⟩))
  | Delete id =>
    obtain ⟨⟨q, hq, ha⟩, hws⟩ := post_of_ok (delete_spec a.user s id) h ws r rfl
    exact .inr (.inr (.inr (.inl ⟨id, q, hq, ha, hws⟩)))
  | List =>
    simp [transition, vec_clone_eq Post.Insts.CoreCloneClone s.posts post_clone] at h
    left
    rw [← h.1]
    rfl
  | Promote tg =>
    obtain ⟨ha, hws⟩ := post_of_ok (promote_spec a.user s tg) h ws r rfl
    exact .inr (.inr (.inr (.inr (.inl ⟨tg, ha, hws⟩))))
  | Demote tg =>
    obtain ⟨hu, ht, hl, hws⟩ := post_of_ok (demote_spec a.user s tg) h ws r rfl
    exact .inr (.inr (.inr (.inr (.inr ⟨tg, hu, ht, hl, hws⟩))))

/-- No command makes the kernel fail: no panic, overflow or bad index. -/
theorem transition_total (a : Principal) (s : Snapshot) (c : Command) : ∃ r, transition a s c = ok r := by
  cases c <;> simp only [transition]
  · exact ok_of (publish_spec _ _ _)
  · exact ok_of (edit_spec _ _ _ _)
  · exact ok_of (delete_spec _ _ _)
  · exact ⟨.Ok (alloc.vec.Vec.new Write, .Posts s.posts), by simp [vec_clone_eq Post.Insts.CoreCloneClone s.posts post_clone]⟩
  · exact ok_of (promote_spec _ _ _)
  · exact ok_of (demote_spec _ _ _)

/-! ## Permissions -/

/-- With distinct users, two or more moderators include one other than `t`. -/
theorem other_mod (l : List Moderator) (hn : (l.map (·.user)).Nodup) (hl : 2 ≤ l.length) (t : U64) :
    ∃ m ∈ l, m.user ≠ t := by
  match l, hn, hl with
  | a :: b :: rest, hn, _ =>
    by_cases ha : a.user = t
    · refine ⟨b, by simp, fun hb => ?_⟩
      simp only [List.map_cons, List.nodup_cons, List.mem_cons, List.mem_map] at hn
      exact hn.1 (.inl (ha.trans hb.symm))
    · exact ⟨a, by simp, ha⟩

/-- The policy allows every write of a successful command, judged against the
state before it. -/
theorem authorized (a : Principal) (s : Snapshot) (c : Command) ws r
    (hinv : Inv (Snapshot.toSt s)) (h : transition a s c = .ok (.Ok (ws, r))) :
    ∀ w ∈ ws.val, allowed (Snapshot.toSt s) a.user.val w := by
  rcases writes_of a s c ws r h with
    hws | ⟨t, cnt, ht, hc, hws⟩ | ⟨pid, t, q, ht, hq, ha, hws⟩ | ⟨pid, q, hq, ha, hws⟩ | ⟨tg, ha, hws⟩ |
    ⟨tg, hu, htg, hl, hws⟩
  · -- List writes nothing.
    simp [hws]
  · -- Publish: a new post by the caller, and the next counter value.
    rw [hws]; intro w hw
    simp only [List.mem_cons, List.not_mem_nil, or_false] at hw
    rcases hw with rfl | rfl
    · exact ⟨rfl, ht, .inl rfl⟩
    · exact hc
  · -- Edit: the caller's own post.
    rw [hws]; intro w hw
    simp only [List.mem_singleton] at hw
    subst hw
    exact ⟨rfl, ht, .inr ⟨q, hq, by rw [ha]⟩⟩
  · -- Delete: by the author or a moderator.
    rw [hws]; intro w hw
    simp only [List.mem_singleton] at hw
    subst hw
    exact ⟨q, hq, ha.imp (fun e => by rw [e]) id⟩
  · -- Promote.
    rw [hws]; intro w hw
    simp only [List.mem_singleton] at hw
    subst hw
    exact ha.imp id (fun ⟨h1, h2⟩ => ⟨h1, by rw [h2]⟩)
  · -- Demote: another moderator remains.
    rw [hws]; intro w hw
    simp only [List.mem_singleton] at hw
    subst hw
    exact ⟨hu, other_mod _ hinv.mod_keys hl tg⟩

/-! ## Invariants

One lemma per kind of state change, then `inv_preserved` is the same six-way
case analysis. -/

theorem findPost_mem {s : St} {id : Nat} {q : Post} (h : findPost s id = some q) :
    q ∈ s.posts ∧ q.id.val = id :=
  ⟨List.mem_of_find?_eq_some h, by simpa using List.find?_some h⟩

/-- Writing a post whose id is below the new counter keeps the invariants. -/
theorem inv_put_post {s : St} (hi : Inv s) (p : Post) (n : Nat) (hn : s.next ≤ n)
    (hid : p.id.val < n) (ht : textOk p.text.val) :
    Inv ⟨n, upsert (·.id) p s.posts, s.mods⟩ where
  post_keys := nodup_map_upsert (·.id) (·.id) (fun _ _ => Iff.rfl) p s.posts hi.post_keys
  fresh z hz := by
    rcases mem_upsert_of hz with rfl | hz
    · exact hid
    · exact Nat.lt_of_lt_of_le (hi.fresh z hz) hn
  texts z hz := by
    rcases mem_upsert_of hz with rfl | hz
    · exact ht
    · exact hi.texts z hz
  mod_keys := hi.mod_keys

theorem inv_del_post {s : St} (hi : Inv s) (id : U64) :
    Inv { s with posts := s.posts.filter (fun p => p.id ≠ id) } where
  post_keys := nodup_map_filter _ _ _ hi.post_keys
  fresh z hz := hi.fresh z (List.mem_filter.1 hz).1
  texts z hz := hi.texts z (List.mem_filter.1 hz).1
  mod_keys := hi.mod_keys

theorem inv_put_mod {s : St} (hi : Inv s) (m : Moderator) :
    Inv { s with mods := upsert (·.user) m s.mods } where
  post_keys := hi.post_keys
  fresh := hi.fresh
  texts := hi.texts
  mod_keys := nodup_map_upsert (·.user) (·.user) (fun _ _ => Iff.rfl) m s.mods hi.mod_keys

theorem inv_del_mod {s : St} (hi : Inv s) (u : U64) :
    Inv { s with mods := s.mods.filter (fun m => m.user ≠ u) } where
  post_keys := hi.post_keys
  fresh := hi.fresh
  texts := hi.texts
  mod_keys := nodup_map_filter _ _ _ hi.mod_keys

/-- Successful commands keep the invariants. -/
theorem inv_preserved (a : Principal) (s : Snapshot) (c : Command) ws r
    (hinv : Inv (Snapshot.toSt s)) (h : transition a s c = .ok (.Ok (ws, r))) :
    Inv (applyAll (Snapshot.toSt s) ws.val) := by
  rcases writes_of a s c ws r h with
    hws | ⟨t, cnt, ht, hc, hws⟩ | ⟨pid, t, q, ht, hq, ha, hws⟩ | ⟨pid, q, hq, ha, hws⟩ | ⟨tg, ha, hws⟩ |
    ⟨tg, hu, htg, hl, hws⟩ <;> rw [hws] <;> simp only [applyAll, List.foldl_cons, List.foldl_nil, applyWrite]
  · exact hinv
  · -- Publish: the new id is the old counter, below the new one.
    exact inv_put_post hinv _ cnt.next_id.val (by omega) (by simp [hc, Snapshot.toSt]) ht
  · -- Edit: the id belongs to an existing post, so it is below the counter.
    obtain ⟨hm, hqid⟩ := findPost_mem hq
    exact inv_put_post hinv _ _ (Nat.le_refl _) (hqid ▸ hinv.fresh q hm) ht
  · exact inv_del_post hinv pid
  · exact inv_put_mod hinv _
  · exact inv_del_mod hinv tg

theorem init_inv : Inv init := by
  constructor <;> simp [init]

/-- The invariants hold in every state the board can reach. -/
theorem reachable_inv {s : St} (h : Reachable s) : Inv s := by
  induction h with
  | init => exact init_inv
  | step _ ht ih => exact inv_preserved _ _ _ _ _ ih ht

/-- `authorized` on reachable states, where `Inv` need not be assumed. -/
theorem authorized_reachable (a : Principal) (s : Snapshot) (c : Command) ws r
    (hr : Reachable (Snapshot.toSt s)) (h : transition a s c = .ok (.Ok (ws, r))) :
    ∀ w ∈ ws.val, allowed (Snapshot.toSt s) a.user.val w :=
  authorized a s c ws r (reachable_inv hr) h

/-! ## Two facts across a step -/

theorem same_post {s : St} (hi : Inv s) {p q : Post} (hp : p ∈ s.posts) (hq : q ∈ s.posts) (h : p.id = q.id) :
    p = q :=
  List.inj_on_of_nodup_map hi.post_keys hp hq h

/-- No command changes the author of a post. -/
theorem author_kept (a : Principal) (s : Snapshot) (c : Command) ws r
    (hinv : Inv (Snapshot.toSt s)) (h : transition a s c = .ok (.Ok (ws, r))) :
    ∀ q ∈ (Snapshot.toSt s).posts, ∀ p ∈ (applyAll (Snapshot.toSt s) ws.val).posts,
      p.id = q.id → p.author = q.author := by
  intro q hq p hp hid
  rcases writes_of a s c ws r h with
    hws | ⟨t, cnt, ht, hc, hws⟩ | ⟨pid, t, q', ht, hq', ha, hws⟩ | ⟨pid, q', hq', ha, hws⟩ | ⟨tg, ha, hws⟩ |
    ⟨tg, hu, htg, hl, hws⟩ <;> rw [hws] at hp <;>
    simp only [applyAll, List.foldl_cons, List.foldl_nil, applyWrite] at hp
  · rw [same_post hinv hp hq hid]
  · -- Publish: the new post has a fresh id, so it is not `q`.
    rcases mem_upsert_of hp with rfl | hp
    · have := hinv.fresh q hq
      simp only [Snapshot.toSt] at this
      rw [← hid] at this
      exact absurd this (Nat.lt_irrefl _)
    · rw [same_post hinv hp hq hid]
  · -- Edit: the caller is the author of the post being edited.
    rcases mem_upsert_of hp with rfl | hp
    · obtain ⟨hm, hqid⟩ := findPost_mem hq'
      rw [same_post hinv hm hq ((u64_val_eq _ _).1 hqid ▸ hid)] at ha
      exact ha.symm
    · rw [same_post hinv hp hq hid]
  · rw [same_post hinv (List.mem_filter.1 hp).1 hq hid]
  · rw [same_post hinv hp hq hid]
  · rw [same_post hinv hp hq hid]

/-- Once the board has a moderator, it always has one. -/
theorem moderator_kept (a : Principal) (s : Snapshot) (c : Command) ws r
    (hinv : Inv (Snapshot.toSt s)) (h : transition a s c = .ok (.Ok (ws, r)))
    (hm : (Snapshot.toSt s).mods ≠ []) :
    (applyAll (Snapshot.toSt s) ws.val).mods ≠ [] := by
  rcases writes_of a s c ws r h with
    hws | ⟨t, cnt, ht, hc, hws⟩ | ⟨pid, t, q', ht, hq', ha, hws⟩ | ⟨pid, q', hq', ha, hws⟩ | ⟨tg, ha, hws⟩ |
    ⟨tg, hu, htg, hl, hws⟩ <;> rw [hws] <;> simp only [applyAll, List.foldl_cons, List.foldl_nil, applyWrite]
  all_goals try exact hm
  · exact List.ne_nil_of_mem (mem_upsert_self _ _ _)
  · -- Demote leaves the other moderator in place.
    obtain ⟨m, hm', hne⟩ := other_mod _ hinv.mod_keys hl tg
    exact List.ne_nil_of_mem (List.mem_filter.2 ⟨hm', by simpa using hne⟩)

end board_kernel.Theorems
