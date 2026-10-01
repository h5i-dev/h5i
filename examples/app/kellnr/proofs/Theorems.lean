import Lemmas
import H5iAppLib.Basic
import H5iAppLib.Authz
/-!
# Theorems about the extracted Kellnr kernel (after PR #1243)

Each theorem covers every principal, login path, state and command.
-/
open Aeneas Aeneas.Std Result kellnr_kernel kellnr_kernel.Spec kellnr_kernel.Lemmas H5iAppLib

namespace kellnr_kernel.Theorems

/-! ## The helpers, as list functions -/

/-- `MaybeUser` as the kernel builds it. -/
def mu (s : St) (p : Principal) (fixed : Bool) : Option MaybeUser :=
  (userOf s p.user.val).map fun u =>
    ⟨u.id, u.is_admin, match p.login with
      | .Token => u.is_read_only
      | .Session => fixed && u.is_read_only⟩

def tu (s : St) (p : Principal) : Option MaybeUser :=
  match p.login with
  | .Session => none
  | .Token => mu s p true

def own (s : St) (k : Nat) (u : MaybeUser) : Bool := u.is_admin || isOwner s k u.name.val

def canMod (u : MaybeUser) : Bool := u.is_admin || !u.is_read_only

theorem userOf_id {s : St} {n : Nat} {u : User} (h : userOf s n = some u) : u.id.val = n := by
  unfold userOf at h; simpa using List.find?_some h

@[step]
theorem maybe_user_spec (s : Snapshot) (p : Principal) (fixed : Bool) :
    maybe_user s p fixed ⦃ r => r = mu (Snapshot.toSt s) p fixed ⦄ := by
  unfold maybe_user
  step as ⟨o, ho⟩
  simp only [mu, userOf, Snapshot.toSt, ← ho]
  cases o <;> [simp; (cases p.login <;> [cases fixed; skip] <;> simp)]

@[step]
theorem token_user_spec (s : Snapshot) (p : Principal) :
    token_user s p ⦃ r => r = tu (Snapshot.toSt s) p ⦄ := by
  unfold token_user tu
  cases h : p.login <;> simp only [h] <;> step*

@[step]
theorem check_ownership_spec (s : Snapshot) (k : U64) (u : MaybeUser) :
    check_ownership s k u ⦃ b => b = own (Snapshot.toSt s) k.val u ⦄ := by
  unfold check_ownership own isOwner
  split <;> step* <;> simp_all [Snapshot.toSt]

@[step]
theorem check_can_modify_spec (u : MaybeUser) : check_can_modify u ⦃ b => b = canMod u ⦄ := by
  unfold check_can_modify canMod
  split <;> simp_all

def gd (s : St) (p : Principal) (fixed m : Bool) (k : Nat) : core.result.Result MaybeUser Error :=
  match mu s p fixed with
  | none => .Err .Unauthorized
  | some u =>
    if m && !canMod u then .Err .ReadOnlyModify
    else if own s k u then .Ok u else .Err .NotOwner

def gt (s : St) (p : Principal) (k : Nat) : core.result.Result MaybeUser Error :=
  match tu s p with
  | none => .Err .Unauthorized
  | some u =>
    if !canMod u then .Err .ReadOnlyModify
    else if own s k u then .Ok u else .Err .NotOwner

def dl (s : St) (p : Principal) (k : Nat) : core.result.Result Unit Error :=
  if restricted s k then
    match tu s p with
    | none => .Err .DownloadUnauthorized
    | some t =>
      if t.is_admin || isCrateUser s k t.name.val || inGrantedGroup s k t.name.val ||
          isOwner s k t.name.val then .Ok () else .Err .NotCrateUser
  else .Ok ()

@[step]
theorem guarded_spec (s : Snapshot) (p : Principal) (fixed m : Bool) (k : U64) :
    guarded s p fixed m k ⦃ r => r = gd (Snapshot.toSt s) p fixed m k.val ⦄ := by
  unfold guarded gd
  step as ⟨o, ho⟩
  rw [← ho]
  cases o <;> step* <;> simp_all

@[step]
theorem guarded_token_spec (s : Snapshot) (p : Principal) (k : U64) :
    guarded_token s p k ⦃ r => r = gt (Snapshot.toSt s) p k.val ⦄ := by
  unfold guarded_token gt
  step as ⟨o, ho⟩
  rw [← ho]
  cases o <;> step* <;> simp_all

theorem check_download_auth_eq (s : Snapshot) (p : Principal) (k : U64) :
    check_download_auth s k p = ok (dl (Snapshot.toSt s) p k.val) := by
  simp only [check_download_auth, H5iAppLib.eq_ok_of_spec (find_crate_spec _ _),
    H5iAppLib.eq_ok_of_spec (token_user_spec _ _), H5iAppLib.eq_ok_of_spec (has_pair_spec _ _ _),
    H5iAppLib.eq_ok_of_spec (is_crate_group_user_spec _ _ _), bind_tc_ok, bind_ok]
  unfold dl restricted isCrateUser isOwner
  have hc : (Snapshot.toSt s).crates = s.crates.val := rfl
  rw [hc]
  cases tu (Snapshot.toSt s) p <;> cases List.find? (fun x => decide (x.id.val = k.val)) s.crates.val <;>
    simp only [bind_tc_ok, bind_ok, Option.any_some, Option.any_none] <;> split_ifs <;> simp_all [Snapshot.toSt]

@[step]
theorem check_download_auth_spec (s : Snapshot) (p : Principal) (k : U64) :
    check_download_auth s k p ⦃ r => r = dl (Snapshot.toSt s) p k.val ⦄ := by
  rw [check_download_auth_eq]; simp [WP.spec_ok]

/-! ## Main theorems -/

/-- Lift a property of successful results to a postcondition. -/
def OnOk (P : alloc.vec.Vec Write → Reply → Prop) :
    core.result.Result (alloc.vec.Vec Write × Reply) Error → Prop
  | .Ok (ws, r) => P ws r
  | .Err _ => True

theorem of_spec {p s c P} (hs : transition p s c ⦃ OnOk P ⦄) {ws r}
    (h : transition p s c = .ok (.Ok (ws, r))) : P ws r := by
  obtain ⟨o, ho, hp⟩ := (WP.spec_equiv_exists _ _).1 hs
  rw [h, Result.ok.injEq] at ho
  subst ho
  exact hp

@[step]
theorem one_spec (w : Write) : one w ⦃ v => v.val = [w] ⦄ := by
  unfold one; step*

/-- Symbolically execute `transition`, leaving one goal per path. -/
macro "walk" : tactic => `(tactic| (
  unfold transition run
  split <;> step* <;> (repeat' (first | step | split | simp only [WP.spec_ok, bind_tc_ok, bind_ok]))))

theorem transition_total (p : Principal) (s : Snapshot) (c : Command) :
    ∃ r, transition p s c = .ok r := by
  have h : transition p s c ⦃ _ => True ⦄ := by walk
  obtain ⟨r, hr, _⟩ := (WP.spec_equiv_exists _ _).1 h
  exact ⟨r, hr⟩

theorem mu_some {s : St} {p : Principal} {fixed : Bool} {u : MaybeUser} (h : mu s p fixed = some u) :
    u.name.val = p.user.val ∧ u.is_admin = isAdmin s p.user.val ∧
      (fixed = true ∨ p.login = .Token → u.is_read_only = isReadOnly s p.user.val) := by
  unfold mu at h
  cases hu : userOf s p.user.val with
  | none => simp [hu] at h
  | some v =>
    simp only [hu, Option.map_some, Option.some.injEq] at h
    subst h
    refine ⟨userOf_id hu, by simp [isAdmin, hu], ?_⟩
    intro hf
    cases hl : p.login <;> rcases hf with hf | hf <;> simp_all [isReadOnly]

theorem tu_some {s : St} {p : Principal} {u : MaybeUser} (h : tu s p = some u) :
    p.login = .Token ∧ mu s p true = some u := by
  unfold tu at h; cases hl : p.login <;> simp_all

theorem gd_ok {s : St} {p : Principal} {fixed m : Bool} {k : Nat} {u : MaybeUser}
    (h : gd s p fixed m k = .Ok u) :
    mu s p fixed = some u ∧ (m = true → canMod u = true) ∧ own s k u = true := by
  unfold gd at h
  split at h
  · cases h
  · rename_i v hv
    split_ifs at h with h1 h2 <;> first | (cases h; refine ⟨hv, ?_, h2⟩; cases m <;> cases hc : canMod u <;> simp_all) | cases h

theorem gt_ok {s : St} {p : Principal} {k : Nat} {u : MaybeUser} (h : gt s p k = .Ok u) :
    tu s p = some u ∧ canMod u = true ∧ own s k u = true := by
  unfold gt at h
  split at h
  · cases h
  · rename_i v hv
    split_ifs at h with h1 h2 <;> first | (cases h; refine ⟨hv, ?_, h2⟩; cases hc : canMod u <;> simp_all) | cases h

theorem ro_canMod {s : St} {p : Principal} {u : MaybeUser} (h : mu s p true = some u)
    (hro : isReadOnly s p.user.val = true) (hna : isAdmin s p.user.val = false) : canMod u = false := by
  obtain ⟨-, ha, hr⟩ := mu_some h
  simp [canMod, ha, hr (.inl rfl), hro, hna]

theorem ro_gd {s : St} {p : Principal} {k : Nat} {x : MaybeUser}
    (hro : isReadOnly s p.user.val = true) (hna : isAdmin s p.user.val = false) :
    (gd s p true true k = .Ok x) ↔ False := by
  refine ⟨fun h => ?_, False.elim⟩
  obtain ⟨hmu, hc, -⟩ := gd_ok h
  simp [ro_canMod hmu hro hna] at hc

theorem ro_gt {s : St} {p : Principal} {k : Nat} {x : MaybeUser}
    (hro : isReadOnly s p.user.val = true) (hna : isAdmin s p.user.val = false) :
    (gt s p k = .Ok x) ↔ False := by
  refine ⟨fun h => ?_, False.elim⟩
  obtain ⟨htu, hc, -⟩ := gt_ok h
  simp [ro_canMod (tu_some htu).2 hro hna] at hc

theorem ro_tu {s : St} {p : Principal} {u : MaybeUser}
    (hro : isReadOnly s p.user.val = true) (hna : isAdmin s p.user.val = false)
    (h : some u = tu s p) : canMod u = false :=
  ro_canMod (tu_some h.symm).2 hro hna

/-- (a) A read-only user who is not an admin commits nothing, whichever way
they logged in. Before PR #1243 this failed for session logins. -/
theorem read_only_commits_nothing (p : Principal) (s : Snapshot) (c : Command) ws r
    (h : transition p s c = .ok (.Ok (ws, r)))
    (hro : isReadOnly (Snapshot.toSt s) p.user.val = true)
    (hna : isAdmin (Snapshot.toSt s) p.user.val = false) : ws.val = [] := by
  refine of_spec (P := fun ws _ => isReadOnly (Snapshot.toSt s) p.user.val = true →
    isAdmin (Snapshot.toSt s) p.user.val = false → ws.val = []) ?_ h hro hna
  walk
  all_goals (simp only [OnOk]; try trivial)
  all_goals (intro hro' hna')
  all_goals (try simp_all [ro_gd hro' hna', ro_gt hro' hna'])
  all_goals (first
    | exact (ro_gd hro' hna').1 r_post.symm
    | exact (ro_gt hro' hna').1 r_post.symm
    | (have := ro_tu hro' hna' o_post; simp_all))

theorem dl_ok {s : St} {p : Principal} {k : Nat} (h : dl s p k = .Ok ()) : downloadAllowed s p k := by
  intro hr
  unfold dl at h
  rw [if_pos hr] at h
  cases ht : tu s p with
  | none => simp [ht] at h
  | some t =>
    rw [ht] at h
    obtain ⟨hl, hmu⟩ := tu_some ht
    obtain ⟨hn, ha, -⟩ := mu_some hmu
    refine ⟨hl, ?_⟩
    simp only at h
    split_ifs at h with hc
    rw [hn] at hc; rw [← ha]; simp only [Bool.or_eq_true] at hc; tauto

/-- (c) A restricted crate is served only to a token of an admin, owner, crate
user or member of a granted group. -/
theorem download_authorized (p : Principal) (s : Snapshot) (k : U64) ws r
    (h : transition p s (.Download k) = .ok (.Ok (ws, r))) :
    downloadAllowed (Snapshot.toSt s) p k.val := by
  unfold transition run at h
  simp only at h
  rw [check_download_auth_eq] at h
  simp only [bind_tc_ok, bind_ok] at h
  split at h
  · rename_i hd; exact dl_ok hd
  · simp at h

theorem gd_ok_iff {s : St} {p : Principal} {fixed m : Bool} {k : Nat} {u : MaybeUser} :
    gd s p fixed m k = .Ok u ↔ mu s p fixed = some u ∧ (m = true → canMod u = true) ∧ own s k u = true := by
  refine ⟨gd_ok, fun ⟨h1, h2, h3⟩ => ?_⟩
  unfold gd; rw [h1]; simp only
  cases m <;> simp_all

theorem gt_ok_iff {s : St} {p : Principal} {k : Nat} {u : MaybeUser} :
    gt s p k = .Ok u ↔ tu s p = some u ∧ canMod u = true ∧ own s k u = true := by
  refine ⟨gt_ok, fun ⟨h1, h2, h3⟩ => ?_⟩
  unfold gt; rw [h1]; simp_all

theorem tu_eq_some {s : St} {p : Principal} {u : MaybeUser} :
    some u = tu s p ↔ p.login = .Token ∧ mu s p true = some u := by
  refine ⟨fun h => tu_some h.symm, fun ⟨h1, h2⟩ => ?_⟩
  unfold tu; rw [h1]; exact h2.symm

theorem own_eq {s : St} {p : Principal} {fixed : Bool} {k : Nat} {u : MaybeUser}
    (hmu : mu s p fixed = some u) : own s k u = (isAdmin s p.user.val || isOwner s k p.user.val) := by
  obtain ⟨hn, ha, -⟩ := mu_some hmu
  unfold own; rw [hn, ha]

theorem mu_name {s : St} {p : Principal} {fixed : Bool} {u : MaybeUser}
    (hmu : mu s p fixed = some u) : u.name.val = p.user.val := (mu_some hmu).1

theorem own_of_mu {s : St} {p : Principal} {fixed : Bool} {k : Nat} {u : MaybeUser}
    (hmu : mu s p fixed = some u) (h : own s k u = true) :
    isAdmin s p.user.val = true ∨ isOwner s k p.user.val = true := by
  obtain ⟨hn, ha, -⟩ := mu_some hmu
  unfold own at h; rw [hn, ha] at h; simpa using h

theorem not_exists_of_find {s : Snapshot} {k : Nat}
    (h : none = s.crates.val.find? (fun x => decide (x.id.val = k))) :
    crateExists (Snapshot.toSt s) k = false := by
  simp only [crateExists, Snapshot.toSt, List.any_eq_false]
  intro x hx; have := List.find?_eq_none.1 h.symm x hx; simpa using this

/-- (d) Every ACL or yank write for a crate comes from an admin or an owner of
that crate; publishing a new crate makes the publisher its first owner. -/
theorem writes_authorized (p : Principal) (s : Snapshot) (c : Command) ws r
    (h : transition p s c = .ok (.Ok (ws, r))) :
    ∀ w ∈ ws.val, writeAllowed (Snapshot.toSt s) p.user.val w := by
  refine of_spec (P := fun ws _ => ∀ w ∈ ws.val, writeAllowed (Snapshot.toSt s) p.user.val w) ?_ h
  walk
  all_goals (simp only [OnOk]; try trivial)
  all_goals (try subst r_post)
  all_goals (try simp only [gd_ok_iff, gt_ok_iff, tu_eq_some] at *)
  all_goals (intro w hw)
  all_goals (try casesm* _ ∧ _)
  all_goals (try have hmu' := (tu_some ‹tu _ _ = some _›).2)
  all_goals (try have hmu' := (tu_some o_post.symm).2)
  all_goals (try (have hk := List.find?_some o_post.symm; simp only [Bool.and_eq_true, decide_eq_true_eq] at hk))
  all_goals (try have hne := not_exists_of_find o1_post)
  all_goals (try have hname := mu_name ‹mu _ _ _ = some _›)
  all_goals (try have hown := own_of_mu ‹mu _ _ _ = some _› ‹own _ _ _ = true›)
  all_goals (try have hown := own_of_mu ‹mu _ _ _ = some _› (b1_post ▸ ‹b1 = true›))
  all_goals (try subst w)
  all_goals (try (rcases hw with h1 | h1 | h1 <;> subst h1))
  all_goals (try (simp only [writeAllowed]; rw [hk.1]; exact hown))
  all_goals (try simp_all [writeAllowed, own_eq])
  all_goals (try (have hk := List.find?_some o_post.symm; simp only [Bool.and_eq_true, decide_eq_true_eq] at hk; rw [hk.1]; exact hown))
  all_goals (try (rcases hown with h | h <;> simp [h]; done))
  all_goals (try (obtain ⟨-, hm⟩ := tu_some o_post.symm; obtain ⟨hn, -, -⟩ := mu_some hm; have hne := not_exists_of_find o1_post; rcases hw with rfl | rfl | rfl <;> simp_all [writeAllowed]; done))
  all_goals (try (obtain ⟨-, hm⟩ := tu_some o_post.symm; have ho := own_of_mu hm b1_post; rcases ho with h | h <;> simp [h]; done))

/-- (b) Removing an owner needs at least two owners, unless ownerless crates
are allowed. -/
theorem owner_removal_needs_two (p : Principal) (s : Snapshot) (c : Command) ws r
    (h : transition p s c = .ok (.Ok (ws, r)))
    (hal : (Snapshot.toSt s).allowOwnerless = false) :
    ∀ x, Write.DelOwner x ∈ ws.val → 2 ≤ ownerCount (Snapshot.toSt s) x.a.val := by
  refine of_spec (P := fun ws _ => (Snapshot.toSt s).allowOwnerless = false →
    ∀ x, Write.DelOwner x ∈ ws.val → 2 ≤ ownerCount (Snapshot.toSt s) x.a.val) ?_ h hal
  walk
  all_goals (simp only [OnOk]; try trivial)
  all_goals (intro hal' x hx)
  all_goals (simp_all [Snapshot.toSt, ownerCount])
  all_goals scalar_tac

theorem at_most_one {l : List Pair} {k u : Nat}
    (hnd : (l.map fun o => (o.a.val, o.b.val)).Nodup) :
    (l.filter fun o => o.a.val = k ∧ o.b.val = u).length ≤ 1 := by
  have hsub : ((l.filter fun o => o.a.val = k ∧ o.b.val = u).map fun o => (o.a.val, o.b.val)).Sublist
      (l.map fun o => (o.a.val, o.b.val)) := List.filter_sublist.map _
  have hn := hnd.sublist hsub
  have hall : ∀ x ∈ ((l.filter fun o => o.a.val = k ∧ o.b.val = u).map fun o => (o.a.val, o.b.val)),
      x = (k, u) := by
    intro x hx; simp only [List.mem_map, List.mem_filter, decide_eq_true_eq] at hx
    obtain ⟨o, ⟨-, ha, hb⟩, rfl⟩ := hx; simp [ha, hb]
  rw [List.eq_replicate_iff.2 ⟨rfl, hall⟩] at hn
  rw [List.nodup_replicate] at hn
  simpa using hn

/-- With unique owner rows, deleting one owner of a crate that has two leaves
one. `Invariants.owner_remains` discharges the uniqueness for reachable states. -/
theorem owner_remains_of_unique (s : St) (k u : Nat)
    (hnd : (s.owners.map fun o => (o.a.val, o.b.val)).Nodup) (h2 : 2 ≤ ownerCount s k) :
    1 ≤ ((s.owners.filter fun o => ¬(o.a.val = k ∧ o.b.val = u)).filter (·.a.val = k)).length := by
  have h1 := at_most_one (k := k) (u := u) hnd
  have hsplit := List.length_eq_length_filter_add (l := s.owners.filter (·.a.val = k))
    (f := fun o => o.b.val = u)
  unfold ownerCount at h2
  simp only [List.filter_filter] at hsplit h1 ⊢
  have e1 : (s.owners.filter fun o => decide (o.b.val = u) && decide (o.a.val = k)).length =
      (s.owners.filter fun o => decide (o.a.val = k ∧ o.b.val = u)).length := by
    congr 1; apply List.filter_congr; intro x _; simp [Bool.and_comm]
  have e2 : (s.owners.filter fun o => !decide (o.b.val = u) && decide (o.a.val = k)).length =
      (s.owners.filter fun o => decide (o.a.val = k) && decide ¬(o.a.val = k ∧ o.b.val = u)).length := by
    congr 1; apply List.filter_congr; intro x _; by_cases ha : x.a.val = k <;> simp [ha]
  omega

/-- The universal authorization property in the reusable `H5iAppLib` shape, so
`cargo app-verify` counts kellnr as covered. It is exactly `writes_authorized`. -/
theorem authorized_schema :
    H5iAppLib.WritesAuthorized transition Snapshot.toSt (·.val)
      (fun st (p : Principal) w => writeAllowed st p.user.val w) :=
  fun p s c ws r h => writes_authorized p s c ws r h

end kellnr_kernel.Theorems
