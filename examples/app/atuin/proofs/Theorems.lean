import Apply
/-! # Theorems about the extracted Atuin kernel

Each covers every principal and command. Most are stated for `step` with
either value of the re-auth flag, so they cover both variants. -/
open Aeneas Aeneas.Std Result atuin_kernel atuin_kernel.Spec atuin_kernel.Lemmas H5iAppLib

namespace atuin_kernel.Theorems

@[step]
theorem one_spec (w : Write) : one w ⦃ v => v.val = [w] ⦄ := by
  unfold one; step*

/-- No input makes the kernel fail. -/
theorem step_total (a : Principal) (s : Snapshot) (c : Command) (b : Bool) :
    ∃ r, step a s c b = .ok r := by
  have h : step a s c b ⦃ _ => True ⦄ := by
    walk step
    all_goals (simp_all [U64.rMax, U64.max_eq]; try scalar_tac)
  obtain ⟨r, hr, _⟩ := (WP.spec_equiv_exists _ _).1 h
  exact ⟨r, hr⟩

/-- Isolation: every write touches only the caller's account or the account
being created. -/
theorem isolation (a : Principal) (s : Snapshot) (c : Command) (b : Bool) ws r
    (h : step a s c b = .ok (.Ok (ws, r))) :
    ∀ w ∈ ws.val, isolated (Snapshot.toSt s) a w := by
  refine of_spec (P := fun ws _ => ∀ w ∈ ws.val, isolated (Snapshot.toSt s) a w) ?_ h
  walk step
  all_goals (try (simp only [OnOk]; try trivial))
  all_goals (try (simp_all [U64.rMax, U64.max_eq]; scalar_tac))
  all_goals (
    intro w hw
    simp [*, toWritesL] at hw
    all_goals first
      | (rcases hw with rfl | rfl | rfl <;> simp_all [isolated, owner, Snapshot.toSt])
      | (obtain ⟨x, _, rfl⟩ := hw; simp_all [isolated, owner, Snapshot.toSt]))
  -- ChangePassword: the user found by the caller's id has that id.
  all_goals (
    left
    rw [← o_post]
    have hid := List.find?_some o1_post.symm
    simp only [decide_eq_true_eq] at hid
    rw [hid])

/-! ## Replies -/

theorem nextL_mem {rs : List Record} {u h t st c : U64} {r : Record} (hr : r ∈ nextL rs u h t st c) :
    r ∈ rs ∧ r.user = u := by
  unfold nextL at hr
  suffices ∀ acc : List Record, (∀ x ∈ acc, x ∈ rs ∧ x.user = u) → ∀ l : List Record, (∀ x ∈ l, x ∈ rs) →
      ∀ x ∈ l.foldl (fun acc r => if decide (r.user = u ∧ r.host = h ∧ r.tag = t ∧ st.val ≤ r.idx.val) &&
        decide (acc.length < c.val) then acc ++ [r] else acc) acc, x ∈ rs ∧ x.user = u by
    exact this [] (by simp) rs (fun _ h => h) r hr
  intro acc hacc l
  induction l generalizing acc with
  | nil => intro _ x hx; exact hacc x hx
  | cons y ys ih =>
    intro hl x hx
    simp only [List.foldl_cons] at hx
    refine ih _ ?_ (fun z hz => hl z (List.mem_cons_of_mem _ hz)) x hx
    split
    · rename_i hc
      intro z hz
      rcases List.mem_append.1 hz with hz | hz
      · exact hacc z hz
      · simp only [List.mem_singleton] at hz; subst hz
        simp only [Bool.and_eq_true, decide_eq_true_eq] at hc
        exact ⟨hl z List.mem_cons_self, hc.1.1⟩
    · exact hacc

/-- Replies contain only the caller's own records. -/
theorem reply_confined (a : Principal) (s : Snapshot) (c : Command) (b : Bool) ws r
    (h : step a s c b = .ok (.Ok (ws, r))) :
    replyAllowed (Snapshot.toSt s) a r := by
  refine of_spec (P := fun _ r => replyAllowed (Snapshot.toSt s) a r) ?_ h
  walk step
  all_goals (try (simp only [OnOk]; try trivial))
  all_goals (try (simp_all [U64.rMax, U64.max_eq]; scalar_tac))
  all_goals simp only [replyAllowed]
  · -- NextRecords
    intro x hx
    rw [v_post] at hx
    obtain ⟨hm, hu⟩ := nextL_mem hx
    exact ⟨hm, by subst_vars; rw [← o_post]; simp⟩
  · -- Status
    intro k hk
    rw [v_post] at hk
    simp only [statusL, List.mem_map, List.mem_filter, decide_eq_true_eq] at hk
    obtain ⟨x, ⟨hm, hu⟩, rfl⟩ := hk
    exact ⟨x, hm, by subst_vars; rw [← o_post]; simp, rfl⟩

/-! ## Registration and re-authentication -/

/-- Closed registration refuses every sign-up. -/
theorem registration_closed (a : Principal) (s : Snapshot) (b : Bool) (n p t : alloc.vec.Vec U8)
    (h : s.settings.open_registration = false) :
    step a s (.Register n p t) b = .ok (.Err .RegistrationClosed) := by
  simp [step, h]

/-- With the fix for issue #3297, deleting an account needs the password. -/
theorem delete_needs_password (a : Principal) (s : Snapshot) (p : Bool) ws r
    (h : transition a s (.DeleteAccount p) = .ok (.Ok (ws, r))) : p = true := by
  cases p
  · exfalso
    have hs : transition a s (.DeleteAccount false) ⦃ o => ∀ ws r, o ≠ .Ok (ws, r) ⦄ := by
      unfold transition step
      step*
    obtain ⟨o, ho, hp⟩ := (WP.spec_equiv_exists _ _).1 hs
    rw [h, Result.ok.injEq] at ho
    exact hp ws r ho.symm
  · rfl

/-- Changing the password needs the current one, in both variants. -/
theorem change_needs_password (a : Principal) (s : Snapshot) (b : Bool) (np : alloc.vec.Vec U8) ws r
    (h : step a s (.ChangePassword false np) b = .ok (.Ok (ws, r))) : False := by
  have hs : step a s (.ChangePassword false np) b ⦃ o => ∀ ws r, o ≠ .Ok (ws, r) ⦄ := by
    unfold step
    step*
  obtain ⟨o, ho, hp⟩ := (WP.spec_equiv_exists _ _).1 hs
  rw [h, Result.ok.injEq] at ho
  exact hp ws r ho.symm

/-- Atuin today (issue #3297): any signed-in user can delete their account
without the password. -/
theorem current_deletes_without_password (s : Snapshot) (id : U64) (hu : ∃ u ∈ s.users.val, u.id = id) :
    ∃ ws, transition_current (.User id) s (.DeleteAccount false) = .ok (.Ok (ws, .Done)) := by
  have hs : transition_current (.User id) s (.DeleteAccount false) ⦃ o => ∃ ws, o = .Ok (ws, .Done) ⦄ := by
    obtain ⟨u, hu1, hu2⟩ := hu
    unfold transition_current step
    step*
    all_goals (try simp_all)
    -- The account exists, so the caller is signed in.
    simp only [signedIn, Snapshot.toSt] at o_post
    rw [if_pos (List.any_eq_true.2 ⟨u, hu1, by simp [hu2]⟩)] at o_post
    simp at o_post
  obtain ⟨o, ho, ws, rfl⟩ := (WP.spec_equiv_exists _ _).1 hs
  exact ⟨ws, ho⟩

/-! ## Scenarios

The guarded commands still succeed when they should, so the theorems above do
not hold by refusing everything. -/

/-- With the fix, a signed-in user who gives the password deletes the account. -/
theorem deletes_with_password (s : Snapshot) (id : U64) (hu : ∃ u ∈ s.users.val, u.id = id) :
    ∃ ws, transition (.User id) s (.DeleteAccount true) = .ok (.Ok (ws, .Done)) := by
  have hs : transition (.User id) s (.DeleteAccount true) ⦃ o => ∃ ws, o = .Ok (ws, .Done) ⦄ := by
    obtain ⟨u, hu1, hu2⟩ := hu
    unfold transition step
    step*
    all_goals (try simp_all)
    simp only [signedIn, Snapshot.toSt] at o_post
    rw [if_pos (List.any_eq_true.2 ⟨u, hu1, by simp [hu2]⟩)] at o_post
    simp at o_post
  obtain ⟨o, ho, ws, rfl⟩ := (WP.spec_equiv_exists _ _).1 hs
  exact ⟨ws, ho⟩

def v8 (l : List U8) (h : l.length ≤ Usize.max := by simp only [List.length_cons, List.length_nil]; scalar_tac) :
    alloc.vec.Vec U8 := alloc.vec.Vec.from l h

/-- A fresh server with open registration and no size cap. -/
def fresh : Snapshot :=
  ⟨⟨0#u64⟩, ⟨true, 0#u64⟩, alloc.vec.Vec.new _, alloc.vec.Vec.new _, alloc.vec.Vec.new _⟩

/-- Signing up as "a" on an open server creates user 0, in both variants. -/
theorem register_opens (b : Bool) :
    step .Anonymous fresh (.Register (v8 [97#u8]) (v8 [1#u8]) (v8 [2#u8])) b ⦃ o =>
      ∃ ws, o = .Ok (ws, .Registered 0#u64) ∧ ws.val.length = 3 ⦄ := by
  unfold step
  step*
  all_goals (simp_all [fresh, v8, byteOK, U64.rMax, U64.max_eq]; try scalar_tac)

end atuin_kernel.Theorems
