import Lemmas
/-!
# What each command writes

One lemma per command: what a successful run writes, and which facts about
the state made it succeed. Refusals write nothing. `writes_of` then sums them
up as `Effect`: a successful command writes nothing, or makes one of sixteen
kinds of change.
-/
open Aeneas Aeneas.Std Result cratesio_kernel cratesio_kernel.Spec cratesio_kernel.Lemmas H5iAppLib

namespace cratesio_kernel.Commands

/-- Close an arithmetic side condition left by `step*`. -/
macro "side" : tactic => `(tactic| ((try simp only [core.num.U64.MAX, U64.rMax, MAX_DEPS] at *); scalar_tac))

theorem authorize_spec (p : Principal) (s : Snapshot) (fixed : Bool) :
    authorize p s fixed ⦃ r => ∀ ws rep, r = .Ok (ws, rep) → p.via = .GitHub ∧
      ∃ c : Counter, c.next_session.val = s.counter.next_session.val + 1 ∧ c.next_token = s.counter.next_token ∧
        ((userOf (Snapshot.toSt s) p.user = none ∧
            ws.val = [.PutUser ⟨p.user, false, false, 0#u64, false⟩, .PutSession ⟨s.counter.next_session, p.user⟩, .SetCounter c]) ∨
          (∃ u, userOf (Snapshot.toSt s) p.user = some u ∧ (fixed = true → lockedAt u p.now = false) ∧
            ws.val = [.PutSession ⟨s.counter.next_session, p.user⟩, .SetCounter c])) ⦄ := by
  unfold authorize
  split <;> step*
  all_goals first
    | (simp only [core.num.U64.MAX, U64.rMax] at *; scalar_tac)
    | (intro ws rep h; obtain ⟨rfl, rfl⟩ := ok_inj h
       refine ⟨‹_›, ⟨i, s.counter.next_token⟩, i_post, rfl, ?_⟩
       (try subst o_post); simp_all [userOf, Snapshot.toSt])

theorem verify_email_spec (p : Principal) (s : Snapshot) :
    verify_email p s ⦃ r => ∀ ws rep, r = .Ok (ws, rep) → ∃ u, SignedIn (Snapshot.toSt s) p u none ∧
      ws.val = [.PutUser { u with verified := true }] ⦄ := by
  unfold verify_email
  step*
  intro ws rep h; obtain ⟨rfl, rfl⟩ := ok_inj h
  have hsi := r_post l ‹_›
  have hsc := r1_post (by simp_all)
  cases ht : l.token <;> simp only [scopeOk, ht] at hsc
  · exact ⟨l.user, ht ▸ hsi, v_post⟩
  · simp at hsc

theorem create_token_spec (p : Principal) (s : Snapshot) (nt : NewToken) :
    create_token p s nt ⦃ r => ∀ ws rep, r = .Ok (ws, rep) → ∃ u, SignedIn (Snapshot.toSt s) p u none ∧
      ∃ c : Counter, c.next_token.val = s.counter.next_token.val + 1 ∧ c.next_session = s.counter.next_session ∧
        ws.val = [.PutToken ⟨s.counter.next_token, u.id, nt.legacy, nt.publish_new, nt.publish_update, nt.yank,
          nt.change_owners, nt.krate, nt.expires, false⟩, .SetCounter c] ⦄ := by
  unfold create_token
  step*
  · simp only [core.num.U64.MAX, U64.rMax] at *; scalar_tac

theorem revoke_token_spec (p : Principal) (s : Snapshot) (id : U64) :
    revoke_token p s id ⦃ r => ∀ ws rep, r = .Ok (ws, rep) → ∃ u tok, SignedIn (Snapshot.toSt s) p u tok ∧
      scopeOk tok true .Unscoped none ∧
      (ws.val = [] ∨ ∃ t, tokenOf (Snapshot.toSt s) id = some t ∧ t.user = u.id ∧
        ws.val = [.PutToken { t with revoked := true }]) ⦄ := by
  unfold revoke_token
  step*
  all_goals (intro ws rep h; obtain ⟨rfl, rfl⟩ := ok_inj h)
  all_goals refine ⟨l.user, l.token, r_post l ‹_›, r1_post (by simp_all), ?_⟩
  all_goals (try subst o_post); simp_all [tokenOf, Snapshot.toSt]

theorem hasCrate_eq (s : Snapshot) (k : U64) :
    hasCrate (Snapshot.toSt s) k = (s.crates.val.find? (·.id = k)).isSome := by
  rw [Bool.eq_iff_iff]; simp [hasCrate, Snapshot.toSt, List.find?_isSome]

@[step] theorem publish_checks_spec (s : Snapshot) (l : Login) (e : Endpoint) (k : U64) (ds : alloc.vec.Vec U64) :
    publish_checks s l e k ds ⦃ r => r = .Ok () → scopeOk l.token true e (some k) ∧ l.user.verified = true ∧
      ds.val.all (fun d => hasCrate (Snapshot.toSt s) d) = true ⦄ := by
  unfold publish_checks
  step*
  simp_all [hasCrate, Snapshot.toSt]

@[step] theorem publish_new_spec (p : Principal) (s : Snapshot) (l : Login) (k n : U64) (ds : alloc.vec.Vec U64)
    (hd : ds.length ≤ 500) :
    publish_new p s l k n ds ⦃ r => ∀ ws rep, r = .Ok (ws, rep) → scopeOk l.token true .PublishNew (some k) ∧
      l.user.verified = true ∧ ds.val.all (fun d => hasCrate (Snapshot.toSt s) d) = true ∧
      ws.val = [.PutCrate ⟨k, p.now⟩, .PutOwner ⟨k, l.user.id, false⟩, .PutVersion ⟨k, n, false, l.user.id⟩] ++
        depWrites k n ds.val ⦄ := by
  unfold publish_new
  have := publish_checks_spec s l .PublishNew k ds
  step*
  simp_all [alloc.vec.Vec.length]; scalar_tac

@[step] theorem publish_update_spec (p : Principal) (s : Snapshot) (l : Login) (k n : U64) (ds : alloc.vec.Vec U64)
    (hd : ds.length ≤ 500) :
    publish_update p s l k n ds ⦃ r => ∀ ws rep, r = .Ok (ws, rep) → scopeOk l.token true .PublishUpdate (some k) ∧
      l.user.verified = true ∧ ds.val.all (fun d => hasCrate (Snapshot.toSt s) d) = true ∧
      Spec.rights (Snapshot.toSt s) k l.user.id p.teams.val ≠ .None ∧ versionOf (Snapshot.toSt s) k n = none ∧
      ws.val = .PutVersion ⟨k, n, false, l.user.id⟩ :: depWrites k n ds.val ⦄ := by
  unfold publish_update
  have := publish_checks_spec s l .PublishUpdate k ds
  step*
  all_goals first
    | (simp_all [alloc.vec.Vec.length]; scalar_tac)
    | (intro ws rep h; obtain ⟨rfl, rfl⟩ := ok_inj h
       obtain ⟨h1, h2, h3⟩ := r_post (by simp_all)
       refine ⟨h1, h2, h3, by rw [← r1_post]; intro h; subst h; contradiction, ?_, by simp_all⟩
       simp only [versionOf, Snapshot.toSt]; rw [← o_post]; simp_all)

theorem publish_spec (p : Principal) (s : Snapshot) (k n : U64) (ds : alloc.vec.Vec U64) :
    publish p s k n ds ⦃ r => ∀ ws rep, r = .Ok (ws, rep) → ∃ u tok, SignedIn (Snapshot.toSt s) p u tok ∧
      u.verified = true ∧ ds.val.all (fun d => hasCrate (Snapshot.toSt s) d) = true ∧
      ((hasCrate (Snapshot.toSt s) k = false ∧ scopeOk tok true .PublishNew (some k) ∧
          ws.val = [.PutCrate ⟨k, p.now⟩, .PutOwner ⟨k, u.id, false⟩, .PutVersion ⟨k, n, false, u.id⟩] ++
            depWrites k n ds.val) ∨
        (hasCrate (Snapshot.toSt s) k = true ∧ scopeOk tok true .PublishUpdate (some k) ∧
          Spec.rights (Snapshot.toSt s) k u.id p.teams.val ≠ .None ∧ versionOf (Snapshot.toSt s) k n = none ∧
          ws.val = .PutVersion ⟨k, n, false, u.id⟩ :: depWrites k n ds.val)) ⦄ := by
  unfold publish
  step*
  all_goals first
    | (simp only [MAX_DEPS] at *; scalar_tac)
    | skip
  all_goals
    obtain ⟨h1, h2, h3, h4⟩ := r_post _ _ r_post1
    have hsi := ‹∀ (l : Login), _ = core.result.Result.Ok l → SignedIn _ _ _ _› l ‹_›
    refine ⟨l.user, l.token, hsi, h2, h3, ?_⟩
  · exact .inl ⟨by rw [hasCrate_eq, ← o_post, ‹o = none›]; rfl, h1, h4⟩
  · obtain ⟨h5, h6, h7⟩ := h4
    exact .inr ⟨by rw [hasCrate_eq, ← o_post, ‹o = some _›]; rfl, h1, h5, h6, h7⟩

theorem yank_spec (p : Principal) (s : Snapshot) (k n : U64) (y : Bool) :
    yank p s k n y ⦃ r => ∀ ws rep, r = .Ok (ws, rep) → ∃ u tok v, SignedIn (Snapshot.toSt s) p u tok ∧
      scopeOk tok true .Yank (some k) ∧ versionOf (Snapshot.toSt s) k n = some v ∧
      (Spec.rights (Snapshot.toSt s) k u.id p.teams.val ≠ .None ∨ u.admin = true) ∧
      (ws.val = [] ∨ ws.val = [.PutVersion ⟨k, n, y, v.publisher⟩]) ⦄ := by
  unfold yank
  step*
  all_goals (intro ws rep h; obtain ⟨rfl, rfl⟩ := ok_inj h)
  all_goals refine ⟨l.user, l.token, v, r_post l ‹_›, r1_post (by simp_all),
    by simp only [versionOf, Snapshot.toSt]; rw [← o_post]; simp_all, ?_, by simp_all⟩
  all_goals first
    | exact .inr ‹_›
    | (left; rw [← r2_post]; intro h; subst h; contradiction)

theorem owner_guard_spec (p : Principal) (s : Snapshot) (k : U64) :
    owner_guard p s k ⦃ r => ∀ l, r = .Ok l → SignedIn (Snapshot.toSt s) p l.user l.token ∧
      scopeOk l.token true .ChangeOwners (some k) ∧ hasCrate (Snapshot.toSt s) k = true ∧
      Spec.rights (Snapshot.toSt s) k l.user.id p.teams.val = .Full ⦄ := by
  unfold owner_guard
  step*
  intro l' h; simp only [core.result.Result.Ok.injEq] at h; subst h
  refine ⟨r_post l ‹_›, r1_post (by simp_all), ?_, by rw [← r2_post]; exact r3_post (by simp_all)⟩
  simp only [hasCrate, Snapshot.toSt]; rw [← b_post]; assumption

/-- The facts every owner endpoint establishes. -/
def Guard (s : St) (p : Principal) (k : U64) (u : User) : Prop :=
  ∃ tok, SignedIn s p u tok ∧ scopeOk tok true .ChangeOwners (some k) ∧ hasCrate s k = true ∧
    Spec.rights s k u.id p.teams.val = .Full

theorem invite_owner_spec (p : Principal) (s : Snapshot) (k t : U64) :
    invite_owner p s k t ⦃ r => ∀ ws rep, r = .Ok (ws, rep) → ∃ u, Guard (Snapshot.toSt s) p k u ∧
      (ws.val = [] ∨ ∃ e, ws.val = [.PutInvite ⟨k, t, u.id, e⟩]) ⦄ := by
  unfold invite_owner
  have := owner_guard_spec p s k
  step*
  all_goals first
    | side
    | (intro ws rep h; obtain ⟨rfl, rfl⟩ := ok_inj h
       obtain ⟨h1, h2, h3, h4⟩ := r_post l ‹_›
       exact ⟨l.user, ⟨l.token, h1, h2, h3, h4⟩, by simp_all⟩)

theorem add_team_spec (p : Principal) (s : Snapshot) (k t : U64) :
    add_team p s k t ⦃ r => ∀ ws rep, r = .Ok (ws, rep) → ∃ u, Guard (Snapshot.toSt s) p k u ∧
      t ∈ p.teams.val ∧ ws.val = [.PutOwner ⟨k, t, true⟩] ⦄ := by
  unfold add_team
  have := owner_guard_spec p s k
  step*
  intro ws rep h; obtain ⟨rfl, rfl⟩ := ok_inj h
  obtain ⟨h1, h2, h3, h4⟩ := r_post _ ‹_›
  exact ⟨_, ⟨_, h1, h2, h3, h4⟩, by simp_all⟩

theorem remove_owner_spec (p : Principal) (s : Snapshot) (k o : U64) (t : Bool) :
    remove_owner p s k o t ⦃ r => ∀ ws rep, r = .Ok (ws, rep) → ∃ u, Guard (Snapshot.toSt s) p k u ∧
      otherUserOwner (Snapshot.toSt s).owners k o t = true ∧ ws.val = [.DelOwner ⟨k, o, t⟩] ⦄ := by
  unfold remove_owner
  have := owner_guard_spec p s k
  step*
  intro ws rep h; obtain ⟨rfl, rfl⟩ := ok_inj h
  obtain ⟨h1, h2, h3, h4⟩ := r_post _ ‹_›
  exact ⟨_, ⟨_, h1, h2, h3, h4⟩, by simp_all [Snapshot.toSt]⟩

theorem handle_invite_spec (p : Principal) (s : Snapshot) (k : U64) (acc : Bool) :
    handle_invite p s k acc ⦃ r => ∀ ws rep, r = .Ok (ws, rep) → ∃ u tok i, SignedIn (Snapshot.toSt s) p u tok ∧
      scopeOk tok true .Unscoped none ∧ inviteOf (Snapshot.toSt s) k u.id = some i ∧
      (ws.val = [.DelInvite k u.id] ∨
        (p.now < i.expires ∧ u.verified = true ∧ ws.val = [.PutOwner ⟨k, u.id, false⟩, .DelInvite k u.id])) ⦄ := by
  unfold handle_invite
  step*
  all_goals (intro ws rep h; obtain ⟨rfl, rfl⟩ := ok_inj h)
  all_goals refine ⟨l.user, l.token, i, r_post l ‹_›, r1_post (by simp_all),
    by simp only [inviteOf, Snapshot.toSt]; rw [← o_post]; simp_all, ?_⟩
  all_goals simp_all

theorem delete_crate_spec (p : Principal) (s : Snapshot) (krate d : U64) :
    delete_crate p s krate d ⦃ r => ∀ ws rep, r = .Ok (ws, rep) → ∃ u tok kr, SignedIn (Snapshot.toSt s) p u tok ∧
      scopeOk tok false .Unscoped (some krate) ∧ crateOf (Snapshot.toSt s) krate = some kr ∧
      Spec.rights (Snapshot.toSt s) krate u.id p.teams.val = .Full ∧ mayDelete (Snapshot.toSt s) kr p.now d ∧
      hasReverseDep (Snapshot.toSt s) krate = false ∧ ws.val = [.DelCrate krate] ⦄ := by
  unfold delete_crate
  step*
  all_goals (intro ws rep h; obtain ⟨rfl, rfl⟩ := ok_inj h)
  all_goals refine ⟨l.user, l.token, k, r_post l ‹_›, r1_post (by simp_all),
    by simp only [crateOf, Snapshot.toSt]; rw [← o_post]; simp_all, by rw [← r2_post]; exact r3_post (by simp_all),
    ?_, by simp only [hasReverseDep, Snapshot.toSt]; rw [← b_post]; simp_all, v_post⟩
  · -- Older than 72 hours: one owner and few downloads.
    right
    have hk : k.id = krate := by
      have := ‹o = some k›; rw [o_post] at this; simpa using List.find?_some this
    refine ⟨by simp only [Snapshot.toSt, hk]; rw [← i1_post]; scalar_tac, ?_⟩
    rw [← age_post, ← i2_post]; scalar_tac
  · left; rw [← age_post]; scalar_tac

theorem operator_spec (p : Principal) (s : Snapshot) (user : U64) (admin locked : Bool) (lu : U64) (sa : Bool) :
    operator p s user admin locked lu sa ⦃ r => ∀ ws rep, r = .Ok (ws, rep) → p.via = .Operator ∧
      ∃ u x, userOf (Snapshot.toSt s) user = some u ∧ x.id = u.id ∧ x.verified = u.verified ∧ ws.val = [.PutUser x] ⦄ := by
  unfold operator
  split <;> step*
  all_goals (intro ws rep h; obtain ⟨rfl, rfl⟩ := ok_inj h)
  all_goals refine ⟨‹_›, u, _, ?_, ?_, ?_, v_post⟩
  all_goals first | rfl | (simp only [userOf, Snapshot.toSt]; rw [← o_post]; assumption)

/-! ## All commands -/

/-- What a successful command `c` writes, and why it was allowed to. -/
inductive Effect (s : St) (p : Principal) (fixed : Bool) : Command → List Write → Prop
  | nothing {c} : Effect s p fixed c []
  | signUp (c : Counter) : p.via = .GitHub → userOf s p.user = none →
      c.next_session.val = s.counter.next_session.val + 1 → c.next_token = s.counter.next_token →
      Effect s p fixed .Authorize
        [.PutUser ⟨p.user, false, false, 0#u64, false⟩, .PutSession ⟨s.counter.next_session, p.user⟩, .SetCounter c]
  | signIn (c : Counter) (u : User) : p.via = .GitHub → userOf s p.user = some u →
      (fixed = true → lockedAt u p.now = false) →
      c.next_session.val = s.counter.next_session.val + 1 → c.next_token = s.counter.next_token →
      Effect s p fixed .Authorize [.PutSession ⟨s.counter.next_session, p.user⟩, .SetCounter c]
  | verify (u : User) : SignedIn s p u none → Effect s p fixed .VerifyEmail [.PutUser { u with verified := true }]
  | newToken (u : User) (nt : NewToken) (c : Counter) : SignedIn s p u none →
      c.next_token.val = s.counter.next_token.val + 1 → c.next_session = s.counter.next_session →
      Effect s p fixed (.CreateToken nt) [.PutToken ⟨s.counter.next_token, u.id, nt.legacy, nt.publish_new,
        nt.publish_update, nt.yank, nt.change_owners, nt.krate, nt.expires, false⟩, .SetCounter c]
  | revoke (u : User) (tok : Option Token) (id : U64) (t : Token) : SignedIn s p u tok →
      scopeOk tok true .Unscoped none → tokenOf s id = some t → t.user = u.id →
      Effect s p fixed (.RevokeToken id) [.PutToken { t with revoked := true }]
  | publishNew (u : User) (tok : Option Token) (k n : U64) (ds : alloc.vec.Vec U64) : SignedIn s p u tok →
      u.verified = true → ds.val.all (fun d => hasCrate s d) = true → hasCrate s k = false →
      scopeOk tok true .PublishNew (some k) →
      Effect s p fixed (.Publish k n ds)
        ([.PutCrate ⟨k, p.now⟩, .PutOwner ⟨k, u.id, false⟩, .PutVersion ⟨k, n, false, u.id⟩] ++ depWrites k n ds.val)
  | publishUpdate (u : User) (tok : Option Token) (k n : U64) (ds : alloc.vec.Vec U64) : SignedIn s p u tok →
      u.verified = true → ds.val.all (fun d => hasCrate s d) = true → hasCrate s k = true →
      scopeOk tok true .PublishUpdate (some k) → Spec.rights s k u.id p.teams.val ≠ .None → versionOf s k n = none →
      Effect s p fixed (.Publish k n ds) (.PutVersion ⟨k, n, false, u.id⟩ :: depWrites k n ds.val)
  | yank (u : User) (tok : Option Token) (v : Version) (k n : U64) (y : Bool) : SignedIn s p u tok →
      scopeOk tok true .Yank (some k) → versionOf s k n = some v → (Spec.rights s k u.id p.teams.val ≠ .None ∨ u.admin = true) →
      Effect s p fixed (.Yank k n y) [.PutVersion ⟨k, n, y, v.publisher⟩]
  | invite (u : User) (k t e : U64) : Guard s p k u → Effect s p fixed (.InviteOwner k t) [.PutInvite ⟨k, t, u.id, e⟩]
  | addTeam (u : User) (k t : U64) : Guard s p k u → t ∈ p.teams.val → Effect s p fixed (.AddTeam k t) [.PutOwner ⟨k, t, true⟩]
  | removeOwner (u : User) (k o : U64) (t : Bool) : Guard s p k u → otherUserOwner s.owners k o t = true →
      Effect s p fixed (.RemoveOwner k o t) [.DelOwner ⟨k, o, t⟩]
  | decline (u : User) (tok : Option Token) (k : U64) (acc : Bool) (i : Invite) : SignedIn s p u tok →
      scopeOk tok true .Unscoped none → inviteOf s k u.id = some i →
      Effect s p fixed (.HandleInvite k acc) [.DelInvite k u.id]
  | accept (u : User) (tok : Option Token) (k : U64) (acc : Bool) (i : Invite) : SignedIn s p u tok →
      scopeOk tok true .Unscoped none → inviteOf s k u.id = some i → p.now < i.expires → u.verified = true →
      Effect s p fixed (.HandleInvite k acc) [.PutOwner ⟨k, u.id, false⟩, .DelInvite k u.id]
  | delete (u : User) (tok : Option Token) (kr : Krate) (k d : U64) : SignedIn s p u tok →
      scopeOk tok false .Unscoped (some k) → crateOf s k = some kr → Spec.rights s k u.id p.teams.val = .Full →
      mayDelete s kr p.now d → hasReverseDep s k = false → Effect s p fixed (.DeleteCrate k d) [.DelCrate k]
  | operator {c} (u : User) (x : User) : p.via = .Operator → userOf s x.id = some u → x.verified = u.verified →
      Effect s p fixed c [.PutUser x]

theorem userOf_id {s : St} {n : U64} {u : User} (h : userOf s n = some u) : u.id = n := by
  unfold userOf at h; simpa using List.find?_some h

/-- Every successful command has one of the effects above. -/
theorem writes_of (p : Principal) (s : Snapshot) (c : Command) (fixed : Bool) ws r
    (h : run p s c fixed = .ok (.Ok (ws, r))) : Effect (Snapshot.toSt s) p fixed c ws.val := by
  cases c <;> simp only [run] at h
  case Authorize =>
    obtain ⟨hv, cn, h1, h2, ⟨hu, hws⟩ | ⟨u, hu, hl, hws⟩⟩ := post_of_ok (authorize_spec p s fixed) h ws r rfl <;> rw [hws]
    · exact .signUp cn hv hu h1 h2
    · exact .signIn cn u hv hu hl h1 h2
  case VerifyEmail =>
    obtain ⟨u, hs, hws⟩ := post_of_ok (verify_email_spec p s) h ws r rfl
    rw [hws]; exact .verify u hs
  case CreateToken nt =>
    obtain ⟨u, hs, cn, h1, h2, hws⟩ := post_of_ok (create_token_spec p s nt) h ws r rfl
    rw [hws]; exact .newToken u nt cn hs h1 h2
  case RevokeToken id =>
    obtain ⟨u, tok, hs, hsc, hws | ⟨t, ht, htu, hws⟩⟩ := post_of_ok (revoke_token_spec p s id) h ws r rfl <;> rw [hws]
    · exact .nothing
    · exact .revoke u tok id t hs hsc ht htu
  case Publish k n ds =>
    obtain ⟨u, tok, hs, hv, hd, ⟨hk, hsc, hws⟩ | ⟨hk, hsc, hr, hn, hws⟩⟩ :=
      post_of_ok (publish_spec p s k n ds) h ws r rfl <;> rw [hws]
    · exact .publishNew u tok k n ds hs hv hd hk hsc
    · exact .publishUpdate u tok k n ds hs hv hd hk hsc hr hn
  case Yank k n y =>
    obtain ⟨u, tok, v, hs, hsc, hv, hr, hws | hws⟩ := post_of_ok (yank_spec p s k n y) h ws r rfl <;> rw [hws]
    · exact .nothing
    · exact .yank u tok v k n y hs hsc hv hr
  case InviteOwner k t =>
    obtain ⟨u, hg, hws | ⟨e, hws⟩⟩ := post_of_ok (invite_owner_spec p s k t) h ws r rfl <;> rw [hws]
    · exact .nothing
    · exact .invite u k t e hg
  case AddTeam k t =>
    obtain ⟨u, hg, ht, hws⟩ := post_of_ok (add_team_spec p s k t) h ws r rfl
    rw [hws]; exact .addTeam u k t hg ht
  case RemoveOwner k o t =>
    obtain ⟨u, hg, ho, hws⟩ := post_of_ok (remove_owner_spec p s k o t) h ws r rfl
    rw [hws]; exact .removeOwner u k o t hg ho
  case HandleInvite k acc =>
    obtain ⟨u, tok, i, hs, hsc, hi, hws | ⟨he, hv, hws⟩⟩ := post_of_ok (handle_invite_spec p s k acc) h ws r rfl <;>
      rw [hws]
    · exact .decline u tok k acc i hs hsc hi
    · exact .accept u tok k acc i hs hsc hi he hv
  case DeleteCrate k d =>
    obtain ⟨u, tok, kr, hs, hsc, hk, hr, hm, hd, hws⟩ := post_of_ok (delete_crate_spec p s k d) h ws r rfl
    rw [hws]; exact .delete u tok kr k d hs hsc hk hr hm hd
  all_goals
    obtain ⟨hv, u, x, hu, hid, hver, hws⟩ := post_of_ok (operator_spec _ _ _ _ _ _ _) h ws r rfl
    rw [hws]; exact .operator u x hv (by rw [hid, userOf_id hu]; exact hu) hver

/-- No input makes the kernel fail: no panic, overflow or bad index. -/
theorem run_total (p : Principal) (s : Snapshot) (c : Command) (fixed : Bool) : ∃ r, run p s c fixed = ok r := by
  cases c <;> simp only [run]
  · exact ok_of (authorize_spec _ _ _)
  · exact ok_of (verify_email_spec _ _)
  · exact ok_of (create_token_spec _ _ _)
  · exact ok_of (revoke_token_spec _ _ _)
  · exact ok_of (publish_spec _ _ _ _ _)
  · exact ok_of (yank_spec _ _ _ _ _)
  · exact ok_of (invite_owner_spec _ _ _ _)
  · exact ok_of (add_team_spec _ _ _ _)
  · exact ok_of (remove_owner_spec _ _ _ _ _)
  · exact ok_of (handle_invite_spec _ _ _ _)
  · exact ok_of (delete_crate_spec _ _ _ _)
  all_goals exact ok_of (operator_spec _ _ _ _ _ _ _)

theorem transition_total (p : Principal) (s : Snapshot) (c : Command) : ∃ r, transition p s c = ok r := by
  simpa [transition] using run_total p s c true

end cratesio_kernel.Commands
