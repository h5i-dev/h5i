import Commands
/-!
# The invariants hold in every reachable state

One lemma per effect shows that it keeps `Inv`; `reachable_inv` follows by
induction over `Reachable`.
-/
open Aeneas Aeneas.Std Result cratesio_kernel cratesio_kernel.Spec cratesio_kernel.Lemmas
  cratesio_kernel.Commands H5iAppLib

namespace cratesio_kernel.Invariants

/-! ## Lists -/

theorem hasCrate_iff (s : St) (k : U64) : hasCrate s k = true ↔ ∃ c ∈ s.crates, c.id = k := by
  simp [hasCrate]

/-- A crate that exists keeps existing when `crates` keeps every id. -/
theorem hasCrate_mono {s s' : St} (hc : ∀ c ∈ s.crates, ∃ c' ∈ s'.crates, c'.id = c.id) {k : U64}
    (h : hasCrate s k = true) : hasCrate s' k = true := by
  obtain ⟨c, hc1, rfl⟩ := (hasCrate_iff s k).1 h
  obtain ⟨c', hc', he⟩ := hc c hc1
  exact (hasCrate_iff s' _).2 ⟨c', hc', he⟩

/-! ## Dependency rows -/

def depKey (d : Dep) : U64 × U64 × U64 := (d.krate, d.num, d.on)

/-- The dependency rows publishing adds. -/
def addDeps (k n : U64) (ds : List U64) (l : List Dep) : List Dep :=
  ds.foldl (fun acc d => upsert depKey ⟨k, n, d⟩ acc) l

theorem applyAll_depWrites (s : St) (k n : U64) (ds : List U64) :
    applyAll s (depWrites k n ds) = { s with deps := addDeps k n ds s.deps } := by
  induction ds generalizing s with
  | nil => rfl
  | cons d ds ih => exact ih _

theorem mem_addDeps {k n : U64} {ds : List U64} {l : List Dep} {z : Dep} (h : z ∈ addDeps k n ds l) :
    z ∈ l ∨ (z.krate = k ∧ z.num = n ∧ z.on ∈ ds) := by
  induction ds generalizing l with
  | nil => exact .inl h
  | cons d ds ih =>
    rcases ih h with h | h
    · rcases mem_upsert_of h with rfl | h
      · exact .inr ⟨rfl, rfl, by simp⟩
      · exact .inl h
    · exact .inr ⟨h.1, h.2.1, by simp [h.2.2]⟩

theorem nodup_addDeps (k n : U64) (ds : List U64) {l : List Dep} (h : (l.map depKey).Nodup) :
    ((addDeps k n ds l).map depKey).Nodup := by
  induction ds generalizing l with
  | nil => exact h
  | cons d ds ih => exact ih (nodup_upsert depKey _ h)

theorem applyAll_append (s : St) (a b : List Write) : applyAll s (a ++ b) = applyAll (applyAll s a) b :=
  List.foldl_append ..

theorem applyAll_cons (s : St) (w : Write) (l : List Write) : applyAll s (w :: l) = applyAll (applyWrite s w) l := rfl

theorem applyAll_nil (s : St) : applyAll s [] = s := rfl

theorem userOf_mem {s : St} {u : U64} {x : User} (h : userOf s u = some x) : x ∈ s.users := (find?_mem h).1

/-! ## Each effect keeps the invariants -/

section
variable {s : St} (hi : Inv s)
include hi

/-- Signing up adds a user and a session. -/
theorem inv_signUp (p : Principal) (c : Counter)
    (h1 : c.next_session.val = s.counter.next_session.val + 1) (h2 : c.next_token = s.counter.next_token) :
    Inv (applyAll s [.PutUser ⟨p.user, false, false, 0#u64, false⟩, .PutSession ⟨s.counter.next_session, p.user⟩,
      .SetCounter c]) := by
  obtain ⟨k1, k2, k3, k4, k5, k6, k7, k8, f1, f2, r1, r2, r3, r4, r5, r6, r7⟩ := hi
  simp only [applyAll_cons, applyAll_nil, applyWrite]
  refine ⟨nodup_upsert _ _ k1, nodup_upsert _ _ k2, k3, k4, k5, k6, k7, k8, ?_, by rw [h2]; exact f2, r1, ?_,
    r3, r4, r5, r6, r7⟩
  · intro x hx
    dsimp only at hx ⊢
    rcases mem_upsert_of hx with rfl | hx
    · simp only [h1]; omega
    · have := f1 x hx; omega
  · intro o ho ht
    obtain ⟨u, hu', he⟩ := r2 o ho ht
    obtain ⟨z, hz, hze⟩ := key_kept (·.id) (⟨p.user, false, false, 0#u64, false⟩ : User) hu'
    exact ⟨z, hz, hze.trans he⟩

theorem inv_signIn (p : Principal) (c : Counter)
    (h1 : c.next_session.val = s.counter.next_session.val + 1) (h2 : c.next_token = s.counter.next_token) :
    Inv (applyAll s [.PutSession ⟨s.counter.next_session, p.user⟩, .SetCounter c]) := by
  obtain ⟨k1, k2, k3, k4, k5, k6, k7, k8, f1, f2, r1, r2, r3, r4, r5, r6, r7⟩ := hi
  simp only [applyAll_cons, applyAll_nil, applyWrite]
  refine ⟨k1, nodup_upsert _ _ k2, k3, k4, k5, k6, k7, k8, ?_, by rw [h2]; exact f2, r1, r2, r3, r4, r5, r6, r7⟩
  intro x hx
  dsimp only at hx ⊢
  rcases mem_upsert_of hx with rfl | hx
  · simp only [h1]; omega
  · have := f1 x hx; omega

/-- Writing a user row; ids are never removed. -/
theorem inv_putUser (x : User) : Inv (applyAll s [.PutUser x]) := by
  obtain ⟨k1, k2, k3, k4, k5, k6, k7, k8, f1, f2, r1, r2, r3, r4, r5, r6, r7⟩ := hi
  simp only [applyAll_cons, applyAll_nil, applyWrite]
  refine ⟨nodup_upsert _ _ k1, k2, k3, k4, k5, k6, k7, k8, f1, f2, r1, ?_, r3, r4, r5, r6, r7⟩
  intro o ho ht
  obtain ⟨u, hu', he⟩ := r2 o ho ht
  obtain ⟨z, hz, hze⟩ := key_kept (·.id) x hu'
  exact ⟨z, hz, hze.trans he⟩

theorem inv_newToken (t : Token) (c : Counter) (ht : t.id = s.counter.next_token)
    (h1 : c.next_token.val = s.counter.next_token.val + 1) (h2 : c.next_session = s.counter.next_session) :
    Inv (applyAll s [.PutToken t, .SetCounter c]) := by
  obtain ⟨k1, k2, k3, k4, k5, k6, k7, k8, f1, f2, r1, r2, r3, r4, r5, r6, r7⟩ := hi
  simp only [applyAll_cons, applyAll_nil, applyWrite]
  refine ⟨k1, k2, nodup_upsert _ _ k3, k4, k5, k6, k7, k8, by rw [h2]; exact f1, ?_, r1, r2, r3, r4, r5, r6, r7⟩
  intro x hx
  dsimp only at hx ⊢
  rcases mem_upsert_of hx with rfl | hx
  · simp only [h1, ht]; omega
  · have := f2 x hx; omega

/-- Replacing a token row with the same id. -/
theorem inv_revoke (t : Token) (ht : t ∈ s.tokens) : Inv (applyAll s [.PutToken { t with revoked := true }]) := by
  obtain ⟨k1, k2, k3, k4, k5, k6, k7, k8, f1, f2, r1, r2, r3, r4, r5, r6, r7⟩ := hi
  simp only [applyAll_cons, applyAll_nil, applyWrite]
  refine ⟨k1, k2, nodup_upsert _ _ k3, k4, k5, k6, k7, k8, f1, ?_, r1, r2, r3, r4, r5, r6, r7⟩
  intro x hx
  dsimp only at hx ⊢
  rcases mem_upsert_of hx with rfl | hx
  · exact f2 t ht
  · exact f2 x hx

/-- Publishing a new crate: the crate, its first user owner, the version and
its dependencies. -/
theorem inv_publishNew (u : User) (k n now : U64) (ds : List U64) (hu : u ∈ s.users)
    (hd : ds.all (fun d => hasCrate s d) = true) :
    Inv (applyAll s ([.PutCrate ⟨k, now⟩, .PutOwner ⟨k, u.id, false⟩, .PutVersion ⟨k, n, false, u.id⟩] ++
      depWrites k n ds)) := by
  obtain ⟨k1, k2, k3, k4, k5, k6, k7, k8, f1, f2, r1, r2, r3, r4, r5, r6, r7⟩ := hi
  simp only [applyAll_append, applyAll_cons, applyAll_nil, applyWrite, applyAll_depWrites]
  -- Every crate that existed still exists, and so does `k`.
  have mono : ∀ x, hasCrate s x = true → hasCrate { s with crates := upsert (·.id) ⟨k, now⟩ s.crates } x = true :=
    fun x => hasCrate_mono (fun c hc => key_kept (·.id) _ hc)
  have hk : hasCrate { s with crates := upsert (·.id) ⟨k, now⟩ s.crates } k = true :=
    (hasCrate_iff _ _).2 ⟨_, mem_upsert_self _ _ _, rfl⟩
  refine ⟨k1, k2, k3, nodup_upsert _ _ k4, nodup_upsert _ _ k5, nodup_upsert _ _ k6, k7, nodup_addDeps _ _ _ k8,
    f1, f2, ?_, ?_, ?_, ?_, fun i hi => mono _ (r5 i hi), ?_, ?_⟩
  · intro c hc
    rcases mem_upsert_of hc with rfl | hc
    · exact ⟨_, mem_upsert_self _ _ _, rfl, rfl⟩
    · obtain ⟨o, ho, h1, h2⟩ := r1 c hc
      obtain ⟨z, hz, hze⟩ := key_kept (fun o => (o.krate, o.owner, o.team)) (⟨k, u.id, false⟩ : Owner) ho
      simp only [Prod.mk.injEq] at hze
      exact ⟨z, hz, hze.1.trans h1, hze.2.2.trans h2⟩
  · intro o ho ht
    rcases mem_upsert_of ho with rfl | ho
    · exact ⟨u, hu, rfl⟩
    · exact r2 o ho ht
  · intro v hv
    rcases mem_upsert_of hv with rfl | hv
    · exact hk
    · exact mono _ (r3 v hv)
  · intro o ho
    rcases mem_upsert_of ho with rfl | ho
    · exact hk
    · exact mono _ (r4 o ho)
  · intro d hd'
    rcases mem_addDeps hd' with hd' | ⟨h1, -, -⟩
    · exact mono _ (r6 d hd')
    · rw [h1]; exact hk
  · intro d hd'
    rcases mem_addDeps hd' with hd' | ⟨-, -, h3⟩
    · exact mono _ (r7 d hd')
    · exact mono _ (by simpa using List.all_eq_true.1 hd _ h3)

/-- Publishing a new version of an existing crate. -/
theorem inv_publishUpdate (u : User) (k n : U64) (ds : List U64) (hk : hasCrate s k = true)
    (hd : ds.all (fun d => hasCrate s d) = true) :
    Inv (applyAll s (.PutVersion ⟨k, n, false, u.id⟩ :: depWrites k n ds)) := by
  obtain ⟨k1, k2, k3, k4, k5, k6, k7, k8, f1, f2, r1, r2, r3, r4, r5, r6, r7⟩ := hi
  simp only [applyAll_cons, applyWrite, applyAll_depWrites]
  refine ⟨k1, k2, k3, k4, nodup_upsert _ _ k5, k6, k7, nodup_addDeps _ _ _ k8,
    f1, f2, r1, r2, ?_, r4, r5, ?_, ?_⟩
  · intro v hv
    rcases mem_upsert_of hv with rfl | hv
    · exact hk
    · exact r3 v hv
  · intro d hd'
    rcases mem_addDeps hd' with hd' | ⟨h1, -, -⟩
    · exact r6 d hd'
    · rw [h1]; exact hk
  · intro d hd'
    rcases mem_addDeps hd' with hd' | ⟨-, -, h3⟩
    · exact r7 d hd'
    · exact (by simpa using List.all_eq_true.1 hd _ h3 : hasCrate s d.on = true)

/-- A yank or unyank rewrites an existing version. -/
theorem inv_yank (v' : Version) (hk : hasCrate s v'.krate = true) : Inv (applyAll s [.PutVersion v']) := by
  obtain ⟨k1, k2, k3, k4, k5, k6, k7, k8, f1, f2, r1, r2, r3, r4, r5, r6, r7⟩ := hi
  simp only [applyAll_cons, applyAll_nil, applyWrite]
  refine ⟨k1, k2, k3, k4, nodup_upsert _ _ k5, k6, k7, k8, f1, f2, r1, r2, ?_, r4, r5, r6, r7⟩
  intro v hv
  rcases mem_upsert_of hv with rfl | hv
  · exact hk
  · exact r3 v hv

theorem inv_invite (i : Invite) (hk : hasCrate s i.krate = true) : Inv (applyAll s [.PutInvite i]) := by
  obtain ⟨k1, k2, k3, k4, k5, k6, k7, k8, f1, f2, r1, r2, r3, r4, r5, r6, r7⟩ := hi
  simp only [applyAll_cons, applyAll_nil, applyWrite]
  refine ⟨k1, k2, k3, k4, k5, k6, nodup_upsert _ _ k7, k8, f1, f2, r1, r2, r3, r4, ?_, r6, r7⟩
  intro x hx
  rcases mem_upsert_of hx with rfl | hx
  · exact hk
  · exact r5 x hx

/-- Adding an owner row: a team, or a user who is registered. -/
theorem owners_put (o : Owner) (hk : hasCrate s o.krate = true)
    (hu : o.team = false → ∃ u ∈ s.users, u.id = o.owner) :
    Inv { s with owners := upsert (fun o => (o.krate, o.owner, o.team)) o s.owners } := by
  obtain ⟨k1, k2, k3, k4, k5, k6, k7, k8, f1, f2, r1, r2, r3, r4, r5, r6, r7⟩ := hi
  refine ⟨k1, k2, k3, k4, k5, nodup_upsert _ _ k6, k7, k8, f1, f2, ?_, ?_, r3, ?_, r5, r6, r7⟩
  · intro c hc
    obtain ⟨o', ho, h1, h2⟩ := r1 c hc
    obtain ⟨z, hz, hze⟩ := key_kept (fun o => (o.krate, o.owner, o.team)) o ho
    simp only [Prod.mk.injEq] at hze
    exact ⟨z, hz, hze.1.trans h1, hze.2.2.trans h2⟩
  · intro x hx ht
    rcases mem_upsert_of hx with rfl | hx
    · exact hu ht
    · exact r2 x hx ht
  · intro x hx
    rcases mem_upsert_of hx with rfl | hx
    · exact hk
    · exact r4 x hx

theorem inv_addTeam (k t : U64) (hk : hasCrate s k = true) : Inv (applyAll s [.PutOwner ⟨k, t, true⟩]) := by
  simp only [applyAll_cons, applyAll_nil, applyWrite]
  exact owners_put hi _ hk (by simp)

/-- Removing an owner, while another user owner of the crate remains. -/
theorem inv_removeOwner (k o : U64) (t : Bool) (ho : otherUserOwner s.owners k o t = true) :
    Inv (applyAll s [.DelOwner ⟨k, o, t⟩]) := by
  obtain ⟨k1, k2, k3, k4, k5, k6, k7, k8, f1, f2, r1, r2, r3, r4, r5, r6, r7⟩ := hi
  simp only [applyAll_cons, applyAll_nil, applyWrite]
  refine ⟨k1, k2, k3, k4, k5, nodup_map_filter _ _ _ k6, k7, k8, f1, f2, ?_,
    fun x hx => r2 x (List.mem_filter.1 hx).1, r3, fun x hx => r4 x (List.mem_filter.1 hx).1, r5, r6, r7⟩
  intro c hc
  by_cases hck : c.id = k
  · -- The other user owner stays.
    obtain ⟨x, hx, h1, h2, h3⟩ := by simpa [otherUserOwner] using ho
    refine ⟨x, List.mem_filter.2 ⟨hx, ?_⟩, h1.trans hck.symm, h2⟩
    simp only [ne_eq, decide_eq_true_eq, Prod.mk.injEq, not_and]
    intro _ ho' ht'
    cases t <;> simp_all
  · obtain ⟨x, hx, h1, h2⟩ := r1 c hc
    refine ⟨x, List.mem_filter.2 ⟨hx, ?_⟩, h1, h2⟩
    simp only [ne_eq, decide_eq_true_eq, Prod.mk.injEq, not_and]
    intro h; exact absurd (h1.symm.trans h) hck

theorem inv_decline (k u : U64) : Inv (applyAll s [.DelInvite k u]) := by
  obtain ⟨k1, k2, k3, k4, k5, k6, k7, k8, f1, f2, r1, r2, r3, r4, r5, r6, r7⟩ := hi
  simp only [applyAll_cons, applyAll_nil, applyWrite]
  exact ⟨k1, k2, k3, k4, k5, k6, nodup_map_filter _ _ _ k7, k8, f1, f2, r1, r2, r3, r4,
    fun x hx => r5 x (List.mem_filter.1 hx).1, r6, r7⟩

/-- Accepting an invitation to a crate that exists. -/
theorem inv_accept (k : U64) (u : User) (hu : u ∈ s.users) (hk : hasCrate s k = true) :
    Inv (applyAll s [.PutOwner ⟨k, u.id, false⟩, .DelInvite k u.id]) := by
  simp only [applyAll_cons, applyAll_nil, applyWrite]
  have h := owners_put hi ⟨k, u.id, false⟩ hk (fun _ => ⟨u, hu, rfl⟩)
  exact inv_decline h k u.id

/-- Deleting a crate with everything that belongs to it. No other crate
depends on it, so no dependency is left dangling. -/
theorem inv_delete (k : U64) (hr : hasReverseDep s k = false) : Inv (applyAll s [.DelCrate k]) := by
  obtain ⟨k1, k2, k3, k4, k5, k6, k7, k8, f1, f2, r1, r2, r3, r4, r5, r6, r7⟩ := hi
  simp only [applyAll_cons, applyAll_nil, applyWrite]
  have hdel : ∀ x, hasCrate s x = true → x ≠ k →
      hasCrate { s with crates := s.crates.filter (·.id ≠ k) } x = true := by
    intro x hx hne
    obtain ⟨c, hc, rfl⟩ := (hasCrate_iff _ _).1 hx
    exact (hasCrate_iff _ _).2 ⟨c, List.mem_filter.2 ⟨hc, by simpa using hne⟩, rfl⟩
  have hne : ∀ {α} {l : List α} {f : α → U64} {x : α}, x ∈ l.filter (fun y => f y ≠ k) → x ∈ l ∧ f x ≠ k :=
    fun h => by simpa using List.mem_filter.1 h
  refine ⟨k1, k2, k3, nodup_map_filter _ _ _ k4, nodup_map_filter _ _ _ k5, nodup_map_filter _ _ _ k6,
    nodup_map_filter _ _ _ k7, nodup_map_filter _ _ _ k8, f1, f2, ?_, ?_, ?_, ?_, ?_, ?_, ?_⟩
  · intro c hc
    obtain ⟨hc, hck⟩ := hne hc
    obtain ⟨o, ho, h1, h2⟩ := r1 c hc
    exact ⟨o, List.mem_filter.2 ⟨ho, by simpa [h1] using hck⟩, h1, h2⟩
  · intro o ho ht; exact r2 o (hne ho).1 ht
  · intro v hv; obtain ⟨hv, hk⟩ := hne hv; exact hdel _ (r3 v hv) hk
  · intro o ho; obtain ⟨ho, hk⟩ := hne ho; exact hdel _ (r4 o ho) hk
  · intro i hi; obtain ⟨hi, hk⟩ := hne hi; exact hdel _ (r5 i hi) hk
  · intro d hd; obtain ⟨hd, hk⟩ := hne hd; exact hdel _ (r6 d hd) hk
  · intro d hd
    obtain ⟨hd, hk⟩ := hne hd
    refine hdel _ (r7 d hd) ?_
    intro hon
    have := List.any_eq_false.1 hr d hd
    simp_all

end

/-- Successful commands keep the invariants. -/
theorem inv_step {s : St} {p : Principal} {fixed : Bool} {c : Command} {l : List Write} (hi : Inv s)
    (he : Effect s p fixed c l) : Inv (applyAll s l) := by
  cases he with
  | nothing => exact hi
  | signUp cn _ _ h1 h2 => exact inv_signUp hi p cn h1 h2
  | signIn cn _ _ _ _ h1 h2 => exact inv_signIn hi p cn h1 h2
  | verify => exact inv_putUser hi _
  | newToken _ _ cn _ h1 h2 => exact inv_newToken hi _ cn rfl h1 h2
  | revoke _ _ _ t _ _ ht _ => exact inv_revoke hi t (find?_mem ht).1
  | publishNew u _ k n ds hs _ hd => exact inv_publishNew hi u k n p.now ds.val (userOf_mem hs.1) hd
  | publishUpdate u _ k n ds _ _ hd hk => exact inv_publishUpdate hi u k n ds.val hk hd
  | yank _ _ v k n y _ _ hv =>
    obtain ⟨hm, hp⟩ := find?_mem hv
    simp only [decide_eq_true_eq] at hp
    exact inv_yank hi ⟨k, n, y, v.publisher⟩ (hp.1 ▸ hi.version_crates v hm)
  | invite _ _ _ _ hg => obtain ⟨_, _, _, hk, _⟩ := hg; exact inv_invite hi _ hk
  | addTeam _ k t hg => obtain ⟨_, _, _, hk, _⟩ := hg; exact inv_addTeam hi k t hk
  | removeOwner _ k o t _ ho => exact inv_removeOwner hi k o t ho
  | decline u _ k _ _ => exact inv_decline hi k u.id
  | accept u _ k _ i hs _ hin =>
    obtain ⟨hm, hp⟩ := find?_mem hin
    simp only [decide_eq_true_eq] at hp
    exact inv_accept hi k u (userOf_mem hs.1) (hp.1 ▸ hi.invite_crates i hm)
  | delete _ _ _ k _ _ _ _ _ _ hr => exact inv_delete hi k hr
  | operator _ x => exact inv_putUser hi x

theorem init_inv : Inv init := by
  constructor <;> simp [init]

/-- The invariants hold in every reachable state. -/
theorem reachable_inv {s : St} (h : Reachable s) : Inv s := by
  induction h with
  | init => exact init_inv
  | step _ ht ih => exact inv_step ih (writes_of _ _ _ true _ _ ht)

end cratesio_kernel.Invariants
