import Theorems
/-! # The invariants hold in every reachable state

In particular deleting an account leaves no session or record behind. -/
open Aeneas Aeneas.Std Result atuin_kernel atuin_kernel.Spec atuin_kernel.Lemmas H5iAppLib

namespace atuin_kernel.Invariants

/-! ## Lists -/

/-- Replacing a row by one with the same `g` leaves the `g` column unchanged. -/
theorem map_upsert_same {α κ β} [DecidableEq κ] (k : α → κ) (g : α → β) (x y : α) (l : List α)
    (hn : (l.map k).Nodup) (hy : y ∈ l) (hk : k y = k x) (hg : g y = g x) :
    (upsert k x l).map g = l.map g := by
  induction l with
  | nil => simp at hy
  | cons z zs ih =>
    simp only [List.map_cons, List.nodup_cons, List.mem_map] at hn
    by_cases hz : k z = k x
    · simp only [upsert, if_pos hz, List.map_cons]
      rcases List.mem_cons.1 hy with rfl | hy'
      · rw [hg]
      · exact absurd ⟨y, hy', hk.trans hz.symm⟩ hn.1
    · simp only [upsert, if_neg hz, List.map_cons]
      rcases List.mem_cons.1 hy with rfl | hy'
      · exact absurd hk hz
      · rw [ih hn.2 hy']

/-! ## One lemma per kind of write set -/

theorem inv_del_records {s : St} (hi : Inv s) (u : U64) : Inv (applyWrite s (.DelRecordsOf u)) := by
  simp only [applyWrite]
  refine { hi with records_owned := ?_, record_keys := ?_, sized := ?_ }
  · intro r hr; simp only [List.mem_filter] at hr; exact hi.records_owned r hr.1
  · exact nodup_map_filter _ _ _ hi.record_keys
  · intro hm r hr; simp only [List.mem_filter] at hr; exact hi.sized hm r hr.1

/-- Deleting sessions, then records, then the user leaves nothing orphaned. -/
theorem inv_delete_account {s : St} (hi : Inv s) (u : U64) :
    Inv (applyWrite (applyWrite (applyWrite s (.DelSession u)) (.DelRecordsOf u)) (.DelUser u)) := by
  simp only [applyWrite]
  refine ⟨?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_⟩
  · intro x hx
    simp only [List.mem_filter, decide_eq_true_eq] at hx
    obtain ⟨v, hv, hid⟩ := hi.sessions_owned x hx.1
    exact ⟨v, by simp only [List.mem_filter, decide_eq_true_eq]; exact ⟨hv, by rw [hid]; exact hx.2⟩, hid⟩
  · intro r hr
    simp only [List.mem_filter, decide_eq_true_eq] at hr
    obtain ⟨v, hv, hid⟩ := hi.records_owned r hr.1
    exact ⟨v, by simp only [List.mem_filter, decide_eq_true_eq]; exact ⟨hv, by rw [hid]; exact hr.2⟩, hid⟩
  · exact nodup_map_filter _ _ _ hi.user_keys
  · exact nodup_map_filter _ _ _ hi.usernames
  · exact nodup_map_filter _ _ _ hi.session_keys
  · exact nodup_map_filter _ _ _ hi.record_keys
  · intro v hv; simp only [List.mem_filter] at hv; exact hi.fresh v hv.1
  · intro hm r hr; simp only [List.mem_filter] at hr; exact hi.sized hm r hr.1

/-- Registration adds a user with a fresh id and an unused name, and its session. -/
theorem inv_register {s : St} (hi : Inv s) (c : Counter) (u : User) (x : Session)
    (hc : c.next_id.val = s.next + 1) (hu : u.id.val = s.next) (hx : x.user = u.id)
    (hname : ∀ v ∈ s.users, v.username.val ≠ u.username.val) :
    Inv (applyWrite (applyWrite (applyWrite s (.SetCounter c)) (.PutUser u)) (.PutSession x)) := by
  simp only [applyWrite]
  have hfresh : ∀ v ∈ s.users, v.id ≠ u.id := by
    intro v hv h; have := hi.fresh v hv; rw [h] at this; omega
  have hsfresh : ∀ y ∈ s.sessions, y.user ≠ x.user := by
    intro y hy h
    obtain ⟨v, hv, hid⟩ := hi.sessions_owned y hy
    exact hfresh v hv (hid.trans (h.trans hx))
  rw [upsert_fresh _ _ _ hfresh, upsert_fresh _ _ _ hsfresh]
  refine ⟨?_, ?_, ?_, ?_, ?_, hi.record_keys, ?_, hi.sized⟩
  · intro y hy
    rcases List.mem_append.1 hy with hy | hy
    · obtain ⟨v, hv, h⟩ := hi.sessions_owned y hy; exact ⟨v, List.mem_append_left _ hv, h⟩
    · simp only [List.mem_singleton] at hy; subst hy
      exact ⟨u, List.mem_append_right _ (List.mem_singleton_self _), hx.symm⟩
  · intro r hr
    obtain ⟨v, hv, h⟩ := hi.records_owned r hr; exact ⟨v, List.mem_append_left _ hv, h⟩
  · rw [List.map_append, List.nodup_append]
    refine ⟨hi.user_keys, by simp, ?_⟩
    intro a ha b hb; simp only [List.map_cons, List.map_nil, List.mem_singleton] at hb
    obtain ⟨v, hv, rfl⟩ := List.mem_map.1 ha; rw [hb]; exact hfresh v hv
  · rw [List.map_append, List.nodup_append]
    refine ⟨hi.usernames, by simp, ?_⟩
    intro a ha b hb; simp only [List.map_cons, List.map_nil, List.mem_singleton] at hb
    obtain ⟨v, hv, rfl⟩ := List.mem_map.1 ha; rw [hb]; exact hname v hv
  · rw [List.map_append, List.nodup_append]
    refine ⟨hi.session_keys, by simp, ?_⟩
    intro a ha b hb; simp only [List.map_cons, List.map_nil, List.mem_singleton] at hb
    obtain ⟨y, hy, rfl⟩ := List.mem_map.1 ha; rw [hb]; exact hsfresh y hy
  · intro v hv
    dsimp only
    rcases List.mem_append.1 hv with hv | hv
    · have := hi.fresh v hv; omega
    · simp only [List.mem_singleton] at hv; subst hv; omega

/-- Changing a password replaces the user row, keeping its id and name. -/
theorem inv_change_password {s : St} (hi : Inv s) (old u : User) (hold : old ∈ s.users)
    (hid : u.id = old.id) (hname : u.username = old.username) : Inv (applyWrite s (.PutUser u)) := by
  simp only [applyWrite]
  have hk := hi.user_keys
  have hmem := fun z => mem_upsert_iff (k := (·.id)) (x := u) (z := z) hk
  refine { hi with sessions_owned := ?_, records_owned := ?_, user_keys := ?_, usernames := ?_, fresh := ?_ }
  · intro y hy
    obtain ⟨v, hv, h⟩ := hi.sessions_owned y hy
    by_cases hvu : v.id = u.id
    · exact ⟨u, mem_upsert_self _ _ _, hvu ▸ h⟩
    · exact ⟨v, mem_upsert_of_ne hv hvu, h⟩
  · intro r hr
    obtain ⟨v, hv, h⟩ := hi.records_owned r hr
    by_cases hvu : v.id = u.id
    · exact ⟨u, mem_upsert_self _ _ _, hvu ▸ h⟩
    · exact ⟨v, mem_upsert_of_ne hv hvu, h⟩
  · rw [map_upsert_same (·.id) (·.id) u old _ hk hold hid.symm hid.symm]; exact hk
  · rw [map_upsert_same (·.id) (·.username.val) u old _ hk hold hid.symm (by rw [hname])]
    exact hi.usernames
  · intro v hv
    rcases (hmem v).1 hv with rfl | ⟨hv, _⟩
    · rw [hid]; exact hi.fresh old hold
    · exact hi.fresh v hv

/-- Storing a record of an existing user, within the size cap. -/
theorem inv_put_record {s : St} (hi : Inv s) (r : Record) (hown : ∃ v ∈ s.users, v.id = r.user)
    (hsize : s.maxSize ≠ 0 → r.data.length ≤ s.maxSize) : Inv (applyWrite s (.PutRecord r)) := by
  simp only [applyWrite]
  refine { hi with records_owned := ?_, record_keys := ?_, sized := ?_ }
  · intro z hz
    rcases mem_upsert_of hz with rfl | hz
    · exact hown
    · exact hi.records_owned z hz
  · exact nodup_map_upsert rkey (fun r => (r.user, r.host, r.tag, r.idx)) (fun _ _ => Iff.rfl) _ _ hi.record_keys
  · intro hm z hz
    rcases mem_upsert_of hz with rfl | hz
    · exact hsize hm
    · exact hi.sized hm z hz

/-- `AddRecords`: a list of puts by an existing user, each within the cap. -/
theorem inv_put_records (uid : U64) (rs : List NewRecord) :
    ∀ s : St, Inv s → (∃ v ∈ s.users, v.id = uid) →
      (∀ r ∈ rs, s.maxSize ≠ 0 → r.data.length ≤ s.maxSize) →
      Inv (applyAll s (toWritesL uid rs)) := by
  induction rs with
  | nil => intro s hi _ _; simpa [applyAll, toWritesL] using hi
  | cons r rs ih =>
    intro s hi hown hsize
    simp only [applyAll, toWritesL, List.map_cons, List.foldl_cons] at *
    apply ih
    · exact inv_put_record hi _ hown (hsize r List.mem_cons_self)
    · simpa [applyWrite] using hown
    · intro x hx; simpa [applyWrite] using hsize x (List.mem_cons_of_mem _ hx)

theorem signedIn_exists {s : St} {a : Principal} {n : Nat} (h : signedIn s a = some n) :
    ∃ v ∈ s.users, v.id.val = n := by
  cases a with
  | Anonymous => simp [signedIn] at h
  | User id =>
    simp only [signedIn] at h
    split at h
    · rename_i hany
      obtain ⟨v, hv, hid⟩ := List.any_eq_true.1 hany
      simp only [decide_eq_true_eq] at hid
      simp only [Option.some.injEq] at h
      exact ⟨v, hv, by rw [hid, h]⟩
    · simp at h

/-- Successful transitions preserve the invariants, in both variants. -/
theorem inv_preserved (a : Principal) (s : Snapshot) (c : Command) (b : Bool) ws r
    (hinv : Inv (Snapshot.toSt s)) (h : step a s c b = .ok (.Ok (ws, r))) :
    Inv (applyAll (Snapshot.toSt s) ws.val) := by
  refine of_spec (P := fun ws _ => Inv (applyAll (Snapshot.toSt s) ws.val)) ?_ h
  walk step
  all_goals (try (simp only [OnOk]; try trivial))
  all_goals (try (simp_all [U64.rMax, U64.max_eq]; scalar_tac))
  -- Register
  all_goals try (
    rw [ws2_post, ws1_post, ws_post, vec_new_val]
    simp only [List.nil_append, List.cons_append, List.singleton_append, applyAll, List.foldl_cons,
      List.foldl_nil]
    apply inv_register hinv _ _ _ (by simp [Snapshot.toSt, i_post]) rfl rfl
    intro w hw heq
    have hb1 : b1 = true := by
      rw [b1_post]; exact List.any_eq_true.2 ⟨w, hw, by simp [heq, v_post]⟩
    contradiction)
  -- ChangePassword
  all_goals try (
    rw [v1_post]
    simp only [applyAll, List.foldl_cons, List.foldl_nil]
    exact inv_change_password hinv u1 _ (List.mem_of_find?_eq_some (o1_post.symm.trans ‹o1 = some u1›)) rfl rfl)
  -- DeleteAccount
  all_goals try (
    rw [ws2_post, ws1_post, ws_post, vec_new_val]
    simp only [List.nil_append, List.cons_append, List.singleton_append, applyAll, List.foldl_cons,
      List.foldl_nil]
    exact inv_delete_account hinv u)
  -- DeleteStore
  all_goals try (
    rw [v_post]
    simp only [applyAll, List.foldl_cons, List.foldl_nil]
    exact inv_del_records hinv u)
  -- AddRecords
  all_goals (
    rw [v_post]
    subst_vars
    apply inv_put_records u _ _ hinv
    · obtain ⟨w, hw, hid⟩ := signedIn_exists o_post.symm
      exact ⟨w, hw, (u64_val_eq _ _).1 hid⟩
    · intro x hx hm
      have hall := ‹(!List.any _ _) = true›
      simp only [Bool.not_eq_true', List.any_eq_false, Bool.not_eq_false] at hall
      have := hall x hx
      simp only [fitsB, decide_eq_true_eq] at this
      simp only [Snapshot.toSt] at hm ⊢
      omega)

/-- A fresh server: any settings, no data. -/
def init (set : Settings) : St :=
  ⟨0, set.open_registration, set.max_record_size.val, [], [], []⟩

theorem init_inv (set : Settings) : Inv (init set) := by
  constructor <;> simp [init]

/-- States the database can reach from a fresh server; `b` selects the variant. -/
inductive Reachable (b : Bool) : St → Prop
  | init (set : Settings) : Reachable b (init set)
  | step {s : Snapshot} {a c ws r} :
      Reachable b (Snapshot.toSt s) → step a s c b = .ok (.Ok (ws, r)) →
      Reachable b (applyAll (Snapshot.toSt s) ws.val)

/-- Every reachable state satisfies the invariants. -/
theorem reachable_inv {b : Bool} {s : St} (h : Reachable b s) : Inv s := by
  induction h with
  | init set => exact init_inv set
  | step _ ht ih => exact inv_preserved _ _ _ _ _ _ ih ht

end atuin_kernel.Invariants
