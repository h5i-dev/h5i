import Commands
/-!
# Theorems about Wastebin

`writes_of`: a successful command creates a paste or makes policy-allowed
deletions. The other theorems case on those two shapes and hold for both
kernel variants unless named otherwise.
-/
open Aeneas Aeneas.Std Result wastebin_kernel wastebin_kernel.Spec wastebin_kernel.Commands H5iAppLib

namespace wastebin_kernel.Theorems

/-- `transition` or `transition_pre190`. -/
def Variant (T : Kernel) : Prop := T = transition ∨ T = transition_pre190

theorem pre190_eq (a : Principal) (s : Snapshot) (c : Command) :
    transition_pre190 a s c = match c with
      | .View k _ key => fetch a s k key
      | _ => transition a s c := by
  cases c <;> rfl

theorem findSlug_mem {st : St} {k : Nat} {p : Paste} (h : findSlug st k = some p) :
    p ∈ st.pastes ∧ p.slug.val = k :=
  ⟨List.mem_of_find?_eq_some h, by simpa using List.find?_some h⟩

/-! ## What a command writes -/

/-- A new paste under the next id and an unused slug, and the counter moved on. -/
def Creates (s : Snapshot) (a : Principal) (ws : List Write) : Prop :=
  ∃ p cnt, p.id = s.counter.next_id ∧ p.slug = a.fresh ∧ findSlug (Snapshot.toSt s) a.fresh.val = none ∧
    ((a.uids.val.head? = some p.owner ∧ cnt.last_uid = s.counter.last_uid) ∨
      (a.uids.val = [] ∧ p.owner.val = s.counter.last_uid.val + 1 ∧ cnt.last_uid = p.owner)) ∧
    cnt.next_id.val = s.counter.next_id.val + 1 ∧ ws = [.PutPaste p, .SetCounter cnt]

/-- Deletions the policy allows. -/
def Deletes (st : St) (a : Principal) (ws : List Write) : Prop :=
  ∃ ids : List U64, ws = ids.map .DelPaste ∧ ∀ id ∈ ids, allowed st a (.DelPaste id)

theorem readPost_deletes {st : St} {a : Principal} {p : Paste} {key ws rep}
    (hp : p ∈ st.pastes) (h : ReadPost a p key ws rep) : Deletes st a ws := by
  rcases h with ⟨he, rfl, -⟩ | ⟨-, -, v, -, -, rfl⟩
  · exact ⟨[p.id], rfl, by simp only [List.mem_singleton]; rintro _ rfl; exact ⟨p, hp, rfl, .inr (.inl he)⟩⟩
  · by_cases hb : p.burn = true
    · exact ⟨[p.id], by simp [hb], by simp only [List.mem_singleton]; rintro _ rfl; exact ⟨p, hp, rfl, .inr (.inr hb)⟩⟩
    · exact ⟨[], by simp [hb], by simp⟩

theorem fetch_deletes {a : Principal} {s : Snapshot} {k key ws r}
    (h : fetch a s k key = .ok (.Ok (ws, r))) : Deletes (Snapshot.toSt s) a ws.val := by
  obtain ⟨p, hp, hr⟩ := post_of_ok (fetch_spec a s k key) h ws r rfl
  exact readPost_deletes (findSlug_mem hp).1 hr

/-- What a successful command writes, in two cases. -/
theorem writes_of {T : Kernel} (hT : Variant T) (a : Principal) (s : Snapshot) (c : Command) ws r
    (h : T a s c = .ok (.Ok (ws, r))) :
    Creates s a ws.val ∨ Deletes (Snapshot.toSt s) a ws.val := by
  have main : ∀ c ws r, transition a s c = .ok (.Ok (ws, r)) →
      Creates s a ws.val ∨ Deletes (Snapshot.toSt s) a ws.val := by
    intro c ws r h
    cases c with
    | Create t e b l =>
      obtain ⟨p, cnt, hid, hsl, -, -, -, -, hf, ho, hn, hws, -⟩ := post_of_ok (create_spec a s t e b l) h ws r rfl
      exact .inl ⟨p, cnt, hid, hsl, hf, ho, hn, hws⟩
    | View k cf key =>
      obtain ⟨p, hp, hv⟩ := post_of_ok (view_spec a s k cf key) h ws r rfl
      rcases hv with ⟨-, -, hws, -⟩ | ⟨-, hr⟩
      · exact .inr ⟨[], by simp [hws], by simp⟩
      · exact .inr (readPost_deletes (findSlug_mem hp).1 hr)
    | Fetch k key => exact .inr (fetch_deletes h)
    | Delete k =>
      obtain ⟨p, hp, ho, hws, -⟩ := post_of_ok (delete_spec a s k) h ws r rfl
      exact .inr ⟨[p.id], hws, by
        simp only [List.mem_singleton]; rintro _ rfl; exact ⟨p, (findSlug_mem hp).1, rfl, .inl ho⟩⟩
    | Purge =>
      obtain ⟨ws', hr', hws⟩ := post_of_ok (purge_spec a.now s) h
      simp only [core.result.Result.Ok.injEq, Prod.mk.injEq] at hr'
      obtain ⟨rfl, -⟩ := hr'
      refine .inr ⟨(s.pastes.val.filter (isExpired a.now.val)).map (·.id), by simp [hws], ?_⟩
      intro id hid
      obtain ⟨p, hp, rfl⟩ := List.mem_map.1 hid
      obtain ⟨hm, he⟩ := List.mem_filter.1 hp
      exact ⟨p, hm, rfl, .inr (.inl he)⟩
  rcases hT with rfl | rfl
  · exact main c ws r h
  · rw [pre190_eq] at h
    cases c with
    | View k _ key => exact .inr (fetch_deletes h)
    | _ => exact main _ ws r h

/-- No command makes the kernel fail: no panic, overflow or bad index. -/
theorem transition_total (a : Principal) (s : Snapshot) (c : Command) : ∃ r, transition a s c = ok r := by
  cases c <;> simp only [transition]
  · exact ok_of (create_spec _ _ _ _ _ _)
  · exact ok_of (view_spec _ _ _ _ _)
  · exact ok_of (fetch_spec _ _ _ _)
  · exact ok_of (delete_spec _ _ _)
  · exact ok_of (purge_spec _ _)

theorem pre190_total (a : Principal) (s : Snapshot) (c : Command) : ∃ r, transition_pre190 a s c = ok r := by
  rw [pre190_eq]
  cases c
  case View => exact ok_of (fetch_spec _ _ _ _)
  all_goals exact transition_total _ _ _

/-! ## Permissions -/

/-- Every write of a successful command is allowed by the policy, judged
against the state before the command. -/
theorem authorized {T : Kernel} (hT : Variant T) (a : Principal) (s : Snapshot) (c : Command) ws r
    (h : T a s c = .ok (.Ok (ws, r))) :
    ∀ w ∈ ws.val, allowed (Snapshot.toSt s) a w := by
  rcases writes_of hT a s c ws r h with ⟨p, cnt, hid, hsl, hf, ho, hn, hws⟩ | ⟨ids, hws, hall⟩
  · rw [hws]; intro w hw
    simp only [List.mem_cons, List.not_mem_nil, or_false] at hw
    rcases hw with rfl | rfl
    · refine ⟨by simp [hid, Snapshot.toSt], hsl, hf, ?_⟩
      rcases ho with ⟨h1, -⟩ | ⟨h1, h2, -⟩
      · exact .inl h1
      · exact .inr ⟨h1, h2⟩
    · refine ⟨hn, ?_⟩
      rcases ho with ⟨-, h2⟩ | ⟨-, h2, h3⟩
      · exact .inl (by rw [h2]; rfl)
      · exact .inr (by rw [h3, h2]; rfl)
  · rw [hws]; intro w hw
    obtain ⟨id, hid, rfl⟩ := List.mem_map.1 hw
    exact hall id hid

/-- Deleting through `DELETE /{id}` needs a uid that owns the paste. -/
theorem delete_needs_owner {T : Kernel} (hT : Variant T) (a : Principal) (s : Snapshot) (k : U64) ws r
    (h : T a s (.Delete k) = .ok (.Ok (ws, r))) :
    ∃ p, findSlug (Snapshot.toSt s) k.val = some p ∧ p.owner ∈ a.uids.val ∧ ws.val = [.DelPaste p.id] := by
  rcases hT with rfl | rfl <;> simp only [transition, transition_pre190] at h <;>
  · obtain ⟨p, hp, ho, hws, -⟩ := post_of_ok (delete_spec a s k) h ws r rfl
    exact ⟨p, hp, ho, hws⟩

/-! ## Invariants -/

theorem applyAll_dels (st : St) (ids : List U64) :
    applyAll st (ids.map .DelPaste) = { st with pastes := st.pastes.filter (fun p => p.id ∉ ids) } := by
  induction ids generalizing st with
  | nil => simp [applyAll]
  | cons id ids ih =>
    simp only [List.map_cons, applyAll, List.foldl_cons] at ih ⊢
    rw [ih]
    simp only [applyWrite, List.filter_filter, St.mk.injEq, true_and]
    congr 1
    funext p
    simp [and_comm]

theorem applyAll_create (st : St) (p : Paste) (cnt : Counter) (h : ∀ q ∈ st.pastes, q.id ≠ p.id) :
    applyAll st [.PutPaste p, .SetCounter cnt] = ⟨cnt.next_id.val, cnt.last_uid.val, st.pastes ++ [p]⟩ := by
  simp [applyAll, applyWrite, upsert_fresh (·.id) p st.pastes h]

theorem findSlug_none {st : St} {k : U64} (h : findSlug st k.val = none) : ∀ q ∈ st.pastes, q.slug ≠ k := by
  intro q hq he
  simp only [findSlug, List.find?_eq_none] at h
  exact h q hq (by simp [he])

/-- After a create: the new paste is appended and the counter is past it. -/
theorem after_create {s : Snapshot} {a : Principal} {ws : List Write} (hi : Inv (Snapshot.toSt s))
    (h : Creates s a ws) :
    ∃ (p : Paste) (cnt : Counter), p.id.val = (Snapshot.toSt s).next ∧ cnt.next_id.val = (Snapshot.toSt s).next + 1 ∧
      (∀ q ∈ (Snapshot.toSt s).pastes, q.slug ≠ p.slug) ∧
      applyAll (Snapshot.toSt s) ws = ⟨cnt.next_id.val, cnt.last_uid.val, (Snapshot.toSt s).pastes ++ [p]⟩ := by
  obtain ⟨p, cnt, hid, hsl, hf, -, hn, rfl⟩ := h
  have hid' : p.id.val = (Snapshot.toSt s).next := by simp [hid, Snapshot.toSt]
  refine ⟨p, cnt, hid', hn, hsl ▸ findSlug_none hf, applyAll_create _ p cnt ?_⟩
  intro q hq he
  have := hi.fresh q hq
  rw [he, hid'] at this
  exact Nat.lt_irrefl _ this

theorem inv_filter {st : St} (hi : Inv st) (P : Paste → Bool) : Inv { st with pastes := st.pastes.filter P } where
  ids := nodup_map_filter _ _ _ hi.ids
  slugs := nodup_map_filter _ _ _ hi.slugs
  fresh q hq := hi.fresh q (List.mem_filter.1 hq).1

/-- Successful commands keep the invariants. -/
theorem inv_preserved {T : Kernel} (hT : Variant T) (a : Principal) (s : Snapshot) (c : Command) ws r
    (hi : Inv (Snapshot.toSt s)) (h : T a s c = .ok (.Ok (ws, r))) :
    Inv (applyAll (Snapshot.toSt s) ws.val) := by
  rcases writes_of hT a s c ws r h with hc | ⟨ids, hws, -⟩
  · obtain ⟨p, cnt, hid, hn, hsl, heq⟩ := after_create hi hc
    rw [heq]
    refine ⟨?_, ?_, ?_⟩
    · simp only [List.map_append, List.map_cons, List.map_nil]
      refine List.nodup_append.2 ⟨hi.ids, by simp, ?_⟩
      intro x hx y hy
      simp only [List.mem_singleton] at hy
      obtain ⟨q, hq, rfl⟩ := List.mem_map.1 hx
      subst hy
      intro he
      have := hi.fresh q hq
      rw [he, hid] at this
      exact Nat.lt_irrefl _ this
    · simp only [List.map_append, List.map_cons, List.map_nil]
      refine List.nodup_append.2 ⟨hi.slugs, by simp, ?_⟩
      intro x hx y hy
      simp only [List.mem_singleton] at hy
      obtain ⟨q, hq, rfl⟩ := List.mem_map.1 hx
      subst hy
      exact hsl q hq
    · intro q hq
      rcases List.mem_append.1 hq with hq | hq
      · have := hi.fresh q hq; simp only at this ⊢; omega
      · simp only [List.mem_singleton] at hq; subst hq; simp only; omega
  · rw [hws, applyAll_dels]
    exact inv_filter hi _

theorem init_inv : Inv init := by
  constructor <;> simp [init]

/-- Steps from a reachable state stay reachable. -/
theorem steps_reachable {T : Kernel} {x y : St} (hs : Steps T x y) (hx : Reachable T x) : Reachable T y := by
  induction hs with
  | refl => exact hx
  | step _ ht ih => exact .step ih ht

/-- The invariants hold in every state either variant can reach. -/
theorem reachable_inv {T : Kernel} (hT : Variant T) {s : St} (h : Reachable T s) : Inv s := by
  induction h with
  | refl => exact init_inv
  | step _ ht ih => exact inv_preserved hT _ _ _ _ _ ih ht

/-! ## Reads -/

theorem readPost_shown {a : Principal} {p : Paste} {key ws v} (h : ReadPost a p key ws (.Shown v)) :
    Shows a p v ∧ isExpired a.now.val p = false ∧ Unlocked p key ∧
      ws = if p.burn then [.DelPaste p.id] else [] := by
  rcases h with ⟨-, -, h⟩ | ⟨he, hu, v', hv, hs, hws⟩
  · cases h
  · cases hv; exact ⟨hs, he, hu, hws⟩

/-- A read shows only the paste the request names, never one that has
expired, never one whose password check failed, and a burn-after-reading
paste is deleted by the read that shows it. -/
theorem shown_of {T : Kernel} (hT : Variant T) (a : Principal) (s : Snapshot) (c : Command) ws v
    (h : T a s c = .ok (.Ok (ws, .Shown v))) :
    ∃ k key p, (c = .Fetch k key ∨ ∃ cf, c = .View k cf key) ∧
      findSlug (Snapshot.toSt s) k.val = some p ∧ Shows a p v ∧ isExpired a.now.val p = false ∧
      Unlocked p key ∧ ws.val = if p.burn then [.DelPaste p.id] else [] := by
  have hfetch : ∀ k key, fetch a s k key = .ok (.Ok (ws, .Shown v)) →
      ∃ p, findSlug (Snapshot.toSt s) k.val = some p ∧ Shows a p v ∧ isExpired a.now.val p = false ∧
        Unlocked p key ∧ ws.val = if p.burn then [.DelPaste p.id] else [] := by
    intro k key h
    obtain ⟨p, hp, hr⟩ := post_of_ok (fetch_spec a s k key) h ws _ rfl
    exact ⟨p, hp, readPost_shown hr⟩
  have main : transition a s c = .ok (.Ok (ws, .Shown v)) → ∃ k key p, (c = .Fetch k key ∨ ∃ cf, c = .View k cf key) ∧
      findSlug (Snapshot.toSt s) k.val = some p ∧ Shows a p v ∧ isExpired a.now.val p = false ∧
      Unlocked p key ∧ ws.val = if p.burn then [.DelPaste p.id] else [] := by
    intro h
    cases c with
    | Create t e b l =>
      obtain ⟨_, _, -, -, -, -, -, -, -, -, -, -, hr⟩ := post_of_ok (create_spec a s t e b l) h ws _ rfl
      cases hr
    | View k cf key =>
      obtain ⟨p, hp, hv⟩ := post_of_ok (view_spec a s k cf key) h ws _ rfl
      rcases hv with ⟨-, -, -, hr⟩ | ⟨-, hr⟩
      · cases hr
      · exact ⟨k, key, p, .inr ⟨cf, rfl⟩, hp, readPost_shown hr⟩
    | Fetch k key =>
      obtain ⟨p, hp, hr⟩ := hfetch k key h
      exact ⟨k, key, p, .inl rfl, hp, hr⟩
    | Delete k =>
      obtain ⟨_, -, -, -, hr⟩ := post_of_ok (delete_spec a s k) h ws _ rfl
      cases hr
    | Purge =>
      obtain ⟨_, hr, -⟩ := post_of_ok (purge_spec a.now s) h
      simp at hr
  rcases hT with rfl | rfl
  · exact main h
  · rw [pre190_eq] at h
    cases c with
    | View k cf key =>
      obtain ⟨p, hp, hr⟩ := hfetch k key h
      exact ⟨k, key, p, .inr ⟨cf, rfl⟩, hp, hr⟩
    | _ => exact main h

/-- An expired paste is never shown. -/
theorem never_expired {T : Kernel} (hT : Variant T) (a : Principal) (s : Snapshot) (c : Command) ws v
    (h : T a s c = .ok (.Ok (ws, .Shown v))) :
    ∃ p ∈ (Snapshot.toSt s).pastes, Shows a p v ∧ isExpired a.now.val p = false := by
  obtain ⟨_, _, p, -, hp, hs, he, -⟩ := shown_of hT a s c ws v h
  exact ⟨p, (findSlug_mem hp).1, hs, he⟩

/-! ## Who can remove a paste -/

/-- A paste disappears only if the caller owns it, it has expired, or it is
a burn-after-reading paste being read. So a live, ordinary paste can only
be deleted by its owner. -/
theorem removed_only_if {T : Kernel} (hT : Variant T) (a : Principal) (s : Snapshot) (c : Command) ws r
    (hi : Inv (Snapshot.toSt s)) (h : T a s c = .ok (.Ok (ws, r)))
    {p : Paste} (hp : p ∈ (Snapshot.toSt s).pastes) (hgone : p ∉ (applyAll (Snapshot.toSt s) ws.val).pastes) :
    p.owner ∈ a.uids.val ∨ isExpired a.now.val p = true ∨ p.burn = true := by
  rcases writes_of hT a s c ws r h with hc | ⟨ids, hws, hall⟩
  · obtain ⟨_, _, -, -, -, heq⟩ := after_create hi hc
    rw [heq] at hgone
    exact absurd (List.mem_append_left _ hp) hgone
  · rw [hws, applyAll_dels] at hgone
    have hin : p.id ∈ ids := by
      by_contra hn
      exact hgone (List.mem_filter.2 ⟨hp, by simp [hn]⟩)
    obtain ⟨q, hq, hid, hcond⟩ := hall p.id hin
    rw [List.inj_on_of_nodup_map hi.ids hq hp hid] at hcond
    exact hcond

/-! ## Burn after reading -/

/-- No paste with id `id` exists, and none can be created (ids are fresh). -/
def Gone (id : U64) (st : St) : Prop := (∀ q ∈ st.pastes, q.id ≠ id) ∧ id.val < st.next

theorem gone_step {T : Kernel} (hT : Variant T) (a : Principal) (s : Snapshot) (c : Command) ws r
    (hi : Inv (Snapshot.toSt s)) (h : T a s c = .ok (.Ok (ws, r))) {id : U64} (hg : Gone id (Snapshot.toSt s)) :
    Gone id (applyAll (Snapshot.toSt s) ws.val) := by
  rcases writes_of hT a s c ws r h with hc | ⟨ids, hws, -⟩
  · obtain ⟨p, cnt, hid, hn, -, heq⟩ := after_create hi hc
    rw [heq]
    refine ⟨fun q hq => ?_, by have := hg.2; simp only; omega⟩
    rcases List.mem_append.1 hq with hq | hq
    · exact hg.1 q hq
    · simp only [List.mem_singleton] at hq
      subst hq
      intro he
      have := hg.2
      rw [← he, hid] at this
      exact Nat.lt_irrefl _ this
  · rw [hws, applyAll_dels]
    exact ⟨fun q hq => hg.1 q (List.mem_filter.1 hq).1, hg.2⟩

theorem gone_steps {T : Kernel} (hT : Variant T) {x y : St} (hs : Steps T x y) (hx : Reachable T x)
    {id : U64} (hg : Gone id x) : Gone id y := by
  induction hs with
  | refl => exact hg
  | step hs' ht ih => exact gone_step hT _ _ _ _ _ (reachable_inv hT (steps_reachable hs' hx)) ht ih

/-- A burn-after-reading paste is shown at most once: the read that shows it
deletes it, and no state reachable afterwards holds a paste with its id. -/
theorem burn_once {T : Kernel} (hT : Variant T) (a : Principal) (s : Snapshot) (c : Command) ws v
    (hr : Reachable T (Snapshot.toSt s)) (h : T a s c = .ok (.Ok (ws, .Shown v))) (hb : v.burned = true) :
    ∃ p ∈ (Snapshot.toSt s).pastes, Shows a p v ∧
      ∀ t, Steps T (applyAll (Snapshot.toSt s) ws.val) t → ∀ q ∈ t.pastes, q.id ≠ p.id := by
  obtain ⟨_, _, p, -, hp, hs, -, -, hws⟩ := shown_of hT a s c ws v h
  have hpb : p.burn = true := by rw [← hs.2.2.1]; exact hb
  have hm := (findSlug_mem hp).1
  refine ⟨p, hm, hs, fun t ht => (gone_steps hT ht (.step hr h) ?_).1⟩
  rw [hws, if_pos hpb, show [Write.DelPaste p.id] = [p.id].map .DelPaste from rfl, applyAll_dels]
  exact ⟨fun q hq => by simpa using (List.mem_filter.1 hq).2, (reachable_inv hT hr).fresh p hm⟩

/-- So every later read shows some other paste. -/
theorem burn_never_again {T : Kernel} (hT : Variant T) (a : Principal) (s : Snapshot) (c : Command) ws v
    (hr : Reachable T (Snapshot.toSt s)) (h : T a s c = .ok (.Ok (ws, .Shown v))) (hb : v.burned = true) :
    ∃ p ∈ (Snapshot.toSt s).pastes, Shows a p v ∧
      ∀ (a' : Principal) (s' : Snapshot) c' ws' v', Steps T (applyAll (Snapshot.toSt s) ws.val) (Snapshot.toSt s') →
        T a' s' c' = .ok (.Ok (ws', .Shown v')) →
        ∃ q ∈ (Snapshot.toSt s').pastes, Shows a' q v' ∧ q.id ≠ p.id := by
  obtain ⟨p, hm, hs, hgone⟩ := burn_once hT a s c ws v hr h hb
  refine ⟨p, hm, hs, fun a' s' c' ws' v' hst h' => ?_⟩
  obtain ⟨_, _, q, -, hq, hqs, -⟩ := shown_of hT a' s' c' ws' v' h'
  have hqm := (findSlug_mem hq).1
  exact ⟨q, hqm, hqs, hgone _ hst q hqm⟩

end wastebin_kernel.Theorems
