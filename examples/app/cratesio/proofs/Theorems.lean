import Invariants
/-!
# Theorems about the crates.io kernel

Each theorem is a case analysis over `Effect` (`Commands.lean`), so none of
them looks at the code again.
-/
open Aeneas Aeneas.Std Result cratesio_kernel cratesio_kernel.Spec cratesio_kernel.Lemmas
  cratesio_kernel.Commands cratesio_kernel.Invariants H5iAppLib

namespace cratesio_kernel.Theorems

/-! ## Locked accounts -/

theorem SignedIn.user_id {s : St} {p : Principal} {u : User} {tok : Option Token} (h : SignedIn s p u tok) :
    u.id = p.user := userOf_id h.1

theorem SignedIn.not_locked {s : St} {p : Principal} {u : User} {tok : Option Token} (h : SignedIn s p u tok)
    (hl : lockedNow s p) : False := by
  obtain ⟨u', hu', hl'⟩ := hl
  obtain ⟨hu, hn, _⟩ := h
  rw [hu] at hu'; cases hu'
  simp_all

/-- After PR #14760, a locked user commits no write at all, whatever they
send and however they sign in. Only the operator acts on a locked account. -/
theorem locked_commits_nothing (p : Principal) (s : Snapshot) (c : Command) ws r
    (h : transition p s c = .ok (.Ok (ws, r))) (hop : p.via ≠ .Operator) (hl : lockedNow (Snapshot.toSt s) p) :
    ws.val = [] := by
  have he := writes_of p s c true ws r h
  generalize ws.val = l at he ⊢
  cases he with
  | nothing => rfl
  | signUp _ _ hu => obtain ⟨u, hu', _⟩ := hl; simp_all
  | signIn _ u _ hu hlk => obtain ⟨u', hu', hl'⟩ := hl; rw [hu] at hu'; cases hu'; simp_all
  | verify _ hs | newToken _ _ _ hs | revoke _ _ _ _ hs | publishNew _ _ _ _ _ hs | publishUpdate _ _ _ _ _ hs
  | yank _ _ _ _ _ _ hs | decline _ _ _ _ _ hs | accept _ _ _ _ _ hs | delete _ _ _ _ _ hs =>
    exact (SignedIn.not_locked hs hl).elim
  | invite _ _ _ _ hg | addTeam _ _ _ hg | removeOwner _ _ _ _ hg =>
    obtain ⟨_, hs, _⟩ := hg; exact (SignedIn.not_locked hs hl).elim
  | operator _ _ hv => exact absurd hv hop

/-! ## The policy -/

/-- A crate that does not exist has no versions. -/
theorem versionOf_none {s : St} (hi : Inv s) {k : U64} (hk : hasCrate s k = false) (n : U64) :
    versionOf s k n = none := by
  unfold versionOf
  rw [List.find?_eq_none]
  intro v hv hp
  simp only [decide_eq_true_eq] at hp
  have := hi.version_crates v hv
  rw [hp.1, hk] at this
  cases this

theorem tokenOf_id {s : St} {i : U64} {t : Token} (h : tokenOf s i = some t) : t.id = i := by
  unfold tokenOf at h; simpa using List.find?_some h

/-- Every write of a successful command is allowed by the policy, judged
against the state before the command. -/
theorem authorized (p : Principal) (s : Snapshot) (c : Command) ws r
    (hr : Reachable (Snapshot.toSt s)) (h : transition p s c = .ok (.Ok (ws, r))) :
    ∀ w ∈ ws.val, allowed (Snapshot.toSt s) p w := by
  have hi := reachable_inv hr
  have he := writes_of p s c true ws r h
  generalize ws.val = l at he ⊢
  generalize Snapshot.toSt s = st at hi he ⊢
  cases he with
  | nothing => simp
  | signUp cn hv hu h1 h2 =>
    intro w hw; simp only [List.mem_cons, List.not_mem_nil, or_false] at hw
    rcases hw with rfl | rfl | rfl
    · exact .inl ⟨hv, hu, rfl⟩
    · exact ⟨hv, rfl, rfl⟩
    · exact .inl ⟨h1, h2⟩
  | signIn cn u hv hu _ h1 h2 =>
    intro w hw; simp only [List.mem_cons, List.not_mem_nil, or_false] at hw
    rcases hw with rfl | rfl
    · exact ⟨hv, rfl, rfl⟩
    · exact .inl ⟨h1, h2⟩
  | verify u hs =>
    intro w hw; simp only [List.mem_cons, List.not_mem_nil, or_false] at hw; subst hw
    exact .inr (.inl ⟨u, hs.1, rfl⟩)
  | newToken u nt cn hs h1 h2 =>
    intro w hw; simp only [List.mem_cons, List.not_mem_nil, or_false] at hw
    rcases hw with rfl | rfl
    · exact ⟨SignedIn.user_id hs, .inl ⟨rfl, rfl⟩⟩
    · exact .inr ⟨h1, h2⟩
  | revoke u tok id t hs _ ht htu =>
    intro w hw; simp only [List.mem_cons, List.not_mem_nil, or_false] at hw; subst hw
    have hid := tokenOf_id ht
    exact ⟨htu.trans (SignedIn.user_id hs), .inr ⟨t, hid ▸ ht, htu.trans (SignedIn.user_id hs), rfl⟩⟩
  | publishNew u tok k n ds hs _ hd hk _ =>
    have huid := SignedIn.user_id hs
    intro w hw
    simp only [List.cons_append, List.mem_cons, List.nil_append] at hw
    rcases hw with rfl | rfl | rfl | hw
    · exact ⟨hk, rfl⟩
    · simp only [allowed, Bool.false_eq_true, ite_false]; exact ⟨huid, .inl hk⟩
    · unfold allowed; dsimp only; rw [versionOf_none hi hk n]; exact ⟨huid, rfl, .inr hk⟩
    · obtain ⟨d, hdm, rfl⟩ := List.mem_map.1 hw
      exact ⟨.inr hk, versionOf_none hi hk n, by simpa using List.all_eq_true.1 hd d hdm⟩
  | publishUpdate u tok k n ds hs _ hd hk _ hrt hn =>
    have huid := SignedIn.user_id hs
    intro w hw
    simp only [List.mem_cons] at hw
    rcases hw with rfl | hw
    · unfold allowed; dsimp only; rw [hn]; exact ⟨huid, rfl, .inl (huid ▸ hrt)⟩
    · obtain ⟨d, hdm, rfl⟩ := List.mem_map.1 hw
      exact ⟨.inl (huid ▸ hrt), hn, by simpa using List.all_eq_true.1 hd d hdm⟩
  | yank u tok v k n y hs _ hv hrt =>
    have huid := SignedIn.user_id hs
    intro w hw; simp only [List.mem_cons, List.not_mem_nil, or_false] at hw; subst hw
    unfold allowed; dsimp only; rw [hv]
    refine ⟨rfl, ?_⟩
    rcases hrt with hrt | hrt
    · exact .inl (huid ▸ hrt)
    · exact .inr ⟨u, hs.1, hrt⟩
  | invite u k t e hg =>
    obtain ⟨tok, hs, _, _, hrt⟩ := hg
    have huid := SignedIn.user_id hs
    intro w hw; simp only [List.mem_cons, List.not_mem_nil, or_false] at hw; subst hw
    exact ⟨huid ▸ hrt, huid⟩
  | addTeam u k t hg ht =>
    obtain ⟨tok, hs, _, _, hrt⟩ := hg
    have huid := SignedIn.user_id hs
    intro w hw; simp only [List.mem_cons, List.not_mem_nil, or_false] at hw; subst hw
    simp only [allowed, ite_true]; exact ⟨huid ▸ hrt, ht⟩
  | removeOwner u k o t hg _ =>
    obtain ⟨tok, hs, _, _, hrt⟩ := hg
    have huid := SignedIn.user_id hs
    intro w hw; simp only [List.mem_cons, List.not_mem_nil, or_false] at hw; subst hw
    exact (huid ▸ hrt : Spec.rights st k p.user p.teams.val = .Full)
  | decline u tok k _ i hs _ _ =>
    intro w hw; simp only [List.mem_cons, List.not_mem_nil, or_false] at hw; subst hw
    exact SignedIn.user_id hs
  | accept u tok k _ i hs _ hin he _ =>
    have huid := SignedIn.user_id hs
    intro w hw; simp only [List.mem_cons, List.not_mem_nil, or_false] at hw
    rcases hw with rfl | rfl
    · simp only [allowed, Bool.false_eq_true, ite_false]; exact ⟨huid, .inr ⟨i, huid ▸ hin, he⟩⟩
    · exact huid
  | delete u tok kr k d hs _ _ hrt =>
    have huid := SignedIn.user_id hs
    intro w hw; simp only [List.mem_cons, List.not_mem_nil, or_false] at hw; subst hw
    exact (huid ▸ hrt : Spec.rights st k p.user p.teams.val = .Full)
  | operator u x hv hu hver =>
    intro w hw; simp only [List.mem_cons, List.not_mem_nil, or_false] at hw; subst hw
    exact .inr (.inr ⟨hv, u, hu, hver⟩)

/-- Only a user owner (Full rights) invites owners, adds teams or removes
owners. Members of a team owner have Publish rights and cannot. -/
theorem only_full_changes_owners (p : Principal) (s : Snapshot) (c : Command) ws r
    (hr : Reachable (Snapshot.toSt s)) (h : transition p s c = .ok (.Ok (ws, r))) (k : U64)
    (hf : Spec.rights (Snapshot.toSt s) k p.user p.teams.val ≠ .Full) :
    ∀ w ∈ ws.val, (∀ i : Invite, w = .PutInvite i → i.krate ≠ k) ∧ (∀ o : Owner, w = .DelOwner o → o.krate ≠ k) ∧
      (∀ o : Owner, w = .PutOwner o → o.team = true → o.krate ≠ k) := by
  intro w hw
  have ha := authorized p s c ws r hr h w hw
  refine ⟨?_, ?_, ?_⟩
  · rintro i rfl rfl; exact hf ha.1
  · rintro o rfl rfl; exact hf ha
  · rintro o rfl ht rfl; simp only [allowed, ht, ite_true] at ha; exact hf ha.1

/-- A crate always keeps a user owner, who has Full rights. -/
theorem crate_has_user_owner {st : St} (hr : Reachable st) :
    ∀ k ∈ st.crates, ∃ o ∈ st.owners, o.krate = k.id ∧ o.team = false ∧
      ∀ teams, Spec.rights st k.id o.owner teams = .Full := by
  intro k hk
  obtain ⟨o, ho, h1, h2⟩ := (reachable_inv hr).owned k hk
  refine ⟨o, ho, h1, h2, fun teams => ?_⟩
  have : st.owners.any (fun x => decide (x.krate = k.id ∧ x.owner = o.owner ∧ x.team = false)) = true :=
    List.any_eq_true.2 ⟨o, ho, by simp [h1, h2]⟩
  simp only [Spec.rights, this, ite_true]

/-! ## Token scopes -/

theorem token_of_signed {s : St} {p : Principal} {u : User} {tok : Option Token} {tid : U64}
    (hs : SignedIn s p u tok) (hv : p.via = .Token tid) :
    ∃ t, tok = some t ∧ tokenOf s tid = some t ∧ live t p.now = true := by
  obtain ⟨-, -, h⟩ := hs
  rw [hv] at h
  obtain ⟨t, h1, h2, -, h4⟩ := h
  exact ⟨t, h1, h2, h4⟩

/-- A request made with an API token writes only within the token's endpoint
and crate scopes, and only while the token is live. Crates cannot be deleted
with a token at all (`needs` is `none` for `DelCrate`). -/
theorem token_scoped (p : Principal) (s : Snapshot) (c : Command) ws r (tid : U64) (hv : p.via = .Token tid)
    (hr : Reachable (Snapshot.toSt s)) (h : transition p s c = .ok (.Ok (ws, r))) :
    ∀ w ∈ ws.val, ∃ t e k, tokenOf (Snapshot.toSt s) tid = some t ∧ live t p.now = true ∧
      needs (Snapshot.toSt s) w = some (e, k) ∧ endpointOk t e = true ∧ crateOk t k = true := by
  have hi := reachable_inv hr
  have he := writes_of p s c true ws r h
  generalize ws.val = l at he ⊢
  generalize Snapshot.toSt s = st at hi he ⊢
  cases he with
  | nothing => simp
  | signUp _ hg | signIn _ _ hg | operator _ _ hg => rw [hv] at hg; cases hg
  | verify u hs | newToken u _ _ hs =>
    obtain ⟨t, ht, -⟩ := token_of_signed hs hv; cases ht
  | revoke u tok _ _ hs hsc =>
    obtain ⟨t, rfl, htk, hl⟩ := token_of_signed hs hv
    intro w hw; simp only [List.mem_cons, List.not_mem_nil, or_false] at hw; subst hw
    exact ⟨t, _, _, htk, hl, rfl, hsc.2.1, hsc.2.2⟩
  | publishNew u tok k n ds hs _ _ hk hsc =>
    obtain ⟨t, rfl, htk, hl⟩ := token_of_signed hs hv
    intro w hw
    simp only [List.cons_append, List.mem_cons, List.nil_append] at hw
    refine ⟨t, _, _, htk, hl, ?_, hsc.2.1, hsc.2.2⟩
    rcases hw with rfl | rfl | rfl | hw
    · rfl
    · simp [needs, hk]
    · simp [needs, hk, versionOf_none hi hk]
    · obtain ⟨d, -, rfl⟩ := List.mem_map.1 hw; simp [needs, hk]
  | publishUpdate u tok k n ds hs _ _ hk hsc _ hn =>
    obtain ⟨t, rfl, htk, hl⟩ := token_of_signed hs hv
    intro w hw
    simp only [List.mem_cons] at hw
    refine ⟨t, _, _, htk, hl, ?_, hsc.2.1, hsc.2.2⟩
    rcases hw with rfl | hw
    · simp [needs, hk, hn]
    · obtain ⟨d, -, rfl⟩ := List.mem_map.1 hw; simp [needs, hk]
  | yank u tok v k n y hs hsc hver =>
    obtain ⟨t, rfl, htk, hl⟩ := token_of_signed hs hv
    intro w hw; simp only [List.mem_cons, List.not_mem_nil, or_false] at hw; subst hw
    exact ⟨t, _, _, htk, hl, by simp [needs, hver], hsc.2.1, hsc.2.2⟩
  | invite u k _ _ hg | addTeam u k _ hg | removeOwner u k _ _ hg =>
    obtain ⟨tok, hs, hsc, -⟩ := hg
    obtain ⟨t, rfl, htk, hl⟩ := token_of_signed hs hv
    intro w hw; simp only [List.mem_cons, List.not_mem_nil, or_false] at hw; subst hw
    exact ⟨t, _, _, htk, hl, by simp [needs], hsc.2.1, hsc.2.2⟩
  | decline u tok k _ _ hs hsc =>
    obtain ⟨t, rfl, htk, hl⟩ := token_of_signed hs hv
    intro w hw; simp only [List.mem_cons, List.not_mem_nil, or_false] at hw; subst hw
    exact ⟨t, _, _, htk, hl, rfl, hsc.2.1, hsc.2.2⟩
  | accept u tok k _ i hs hsc hin =>
    obtain ⟨t, rfl, htk, hl⟩ := token_of_signed hs hv
    obtain ⟨hm, hp⟩ := find?_mem hin
    simp only [decide_eq_true_eq] at hp
    have hk : hasCrate st k = true := hp.1 ▸ hi.invite_crates i hm
    intro w hw; simp only [List.mem_cons, List.not_mem_nil, or_false] at hw
    refine ⟨t, _, _, htk, hl, ?_, hsc.2.1, hsc.2.2⟩
    rcases hw with rfl | rfl
    · simp [needs, hk]
    · rfl
  | delete u tok _ _ _ hs hsc =>
    obtain ⟨t, rfl, -⟩ := token_of_signed hs hv
    exact absurd hsc.1 (by decide)

/-! ## Versions and deletion -/

/-- Versions are never unpublished: every version survives a command, with
its publisher, unless the command deletes the whole crate. -/
theorem versions_kept (p : Principal) (s : Snapshot) (c : Command) ws r
    (hr : Reachable (Snapshot.toSt s)) (h : transition p s c = .ok (.Ok (ws, r))) :
    ∀ v ∈ (Snapshot.toSt s).versions,
      (∃ v' ∈ (applyAll (Snapshot.toSt s) ws.val).versions, v'.krate = v.krate ∧ v'.num = v.num ∧
        v'.publisher = v.publisher) ∨ .DelCrate v.krate ∈ ws.val := by
  have hi := reachable_inv hr
  have he := writes_of p s c true ws r h
  generalize ws.val = l at he ⊢
  generalize Snapshot.toSt s = st at hi he ⊢
  intro v hv
  -- The row for `(k, n)` is new, so `v` survives the upsert.
  have kept : ∀ (x : Version) (k n : U64), versionOf st k n = none → x.krate = k → x.num = n →
      v ∈ upsert (fun v => (v.krate, v.num)) x st.versions := by
    intro x k n hn hk hn'
    refine mem_upsert_of_ne hv ?_
    intro heq
    simp only [Prod.mk.injEq] at heq
    rw [versionOf, List.find?_eq_none] at hn
    exact hn v hv (by simp [heq.1, heq.2, hk, hn'])
  cases he with
  | publishNew u _ k n ds _ _ _ hk =>
    left
    simp only [applyAll_append, applyAll_cons, applyAll_nil, applyWrite, applyAll_depWrites]
    exact ⟨v, kept _ k n (versionOf_none hi hk n) rfl rfl, rfl, rfl, rfl⟩
  | publishUpdate u _ k n ds _ _ _ _ _ _ hn =>
    left
    simp only [applyAll_cons, applyWrite, applyAll_depWrites]
    exact ⟨v, kept _ k n hn rfl rfl, rfl, rfl, rfl⟩
  | yank u _ v0 k n y _ _ hv0 =>
    left
    simp only [applyAll_cons, applyAll_nil, applyWrite]
    obtain ⟨hm, hp⟩ := find?_mem hv0
    simp only [decide_eq_true_eq] at hp
    by_cases hkey : (v.krate, v.num) = (k, n)
    · -- `v` is the yanked version; its publisher is kept.
      simp only [Prod.mk.injEq] at hkey
      have : v = v0 := List.inj_on_of_nodup_map hi.version_keys hv hm (by simp [hkey, hp])
      subst this
      exact ⟨_, mem_upsert_self _ _ _, hkey.1.symm, hkey.2.symm, rfl⟩
    · exact ⟨v, mem_upsert_of_ne hv hkey, rfl, rfl, rfl⟩
  | delete _ _ _ k _ =>
    by_cases hk : v.krate = k
    · right; simp [hk]
    · left
      simp only [applyAll_cons, applyAll_nil, applyWrite]
      exact ⟨v, List.mem_filter.2 ⟨hv, by simpa using hk⟩, rfl, rfl, rfl⟩
  | nothing => exact .inl ⟨v, hv, rfl, rfl, rfl⟩
  | _ =>
    left
    simp only [applyAll_cons, applyAll_nil, applyWrite]
    exact ⟨v, hv, rfl, rfl, rfl⟩

/-- A crate is deleted only by the `DeleteCrate` command, from a session
cookie, by a user owner, within 72 hours or (later) with a single owner and
at most 1000 downloads per started month, and when no other crate depends on it. -/
theorem delete_rules (p : Principal) (s : Snapshot) (c : Command) ws r (k : U64)
    (h : transition p s c = .ok (.Ok (ws, r))) (hk : .DelCrate k ∈ ws.val) :
    ∃ d kr sid, c = .DeleteCrate k d ∧ p.via = .Cookie sid ∧ crateOf (Snapshot.toSt s) k = some kr ∧
      Spec.rights (Snapshot.toSt s) k p.user p.teams.val = .Full ∧ mayDelete (Snapshot.toSt s) kr p.now d ∧
      hasReverseDep (Snapshot.toSt s) k = false := by
  have he := writes_of p s c true ws r h
  generalize ws.val = l at he hk
  generalize Snapshot.toSt s = st at he ⊢
  cases he with
  | delete u tok kr k' d hs hsc hc hrt hm hd =>
    simp only [List.mem_cons, Write.DelCrate.injEq, List.not_mem_nil, or_false] at hk
    subst hk
    have hnone : tok = none := by
      cases tok with
      | none => rfl
      | some t => exact absurd hsc.1 (by decide)
    obtain ⟨hu, -, hvia⟩ := hs
    subst hnone
    cases hvv : p.via with
    | Cookie sid => exact ⟨d, kr, sid, rfl, rfl, hc, userOf_id hu ▸ hrt, hm, hd⟩
    | Token tid => rw [hvv] at hvia; obtain ⟨t, ht, -⟩ := hvia; cases ht
    | GitHub => rw [hvv] at hvia; exact hvia.elim
    | Operator => rw [hvv] at hvia; exact hvia.elim
  | publishNew | publishUpdate =>
    simp [depWrites] at hk
  | _ => simp at hk

end cratesio_kernel.Theorems
