import Transition
import Apply
open Aeneas Aeneas.Std Result docs_kernel docs_kernel.Spec docs_kernel.TransitionLemmas H5iAppLib

namespace docs_kernel.Theorems

/-! ## Keys -/

def pkey (q : Project) : Nat × Nat := (q.id.val, 0)
def mkey (n : Member) : Nat × Nat := (n.project.val, n.user.val)
def dkey (e : Document) : Nat × Nat := (e.id.val, 0)

theorem pkey_iff (a b : Project) : pkey a = pkey b ↔ a.id = b.id := by
  simp [pkey, ← u64_val_eq]
theorem mkey_iff (a b : Member) : mkey a = mkey b ↔ (a.project, a.user) = (b.project, b.user) := by
  simp [mkey, ← u64_val_eq]
theorem dkey_iff (a b : Document) : dkey a = dkey b ↔ a.id = b.id := by
  simp [dkey, ← u64_val_eq]

def hkey (w : Webhook) : Nat × Nat := (w.project.val, 0)

theorem hkey_iff (a b : Webhook) : hkey a = hkey b ↔ a.project = b.project := by
  simp [hkey, ← u64_val_eq]

/-! ## Owners and roles -/

theorem owners_pos_iff (ms : List Member) (p : Nat) :
    0 < owners ms p ↔ ∃ n ∈ ms, n.project.val = p ∧ n.role = .Owner := by
  unfold owners
  rw [List.length_pos_iff_exists_mem]
  simp [List.mem_filter]

/-- With unique keys, two owners of `p` means one of them is not `t`. -/
theorem other_owner {ms : List Member} {p t : Nat} (hk : (ms.map mkey).Nodup)
    (h : 1 < owners ms p) : ∃ n ∈ ms, n.project.val = p ∧ n.role = .Owner ∧ n.user.val ≠ t := by
  unfold owners at h
  have hn := nodup_map_filter mkey (fun m => decide (m.project.val = p ∧ m.role = .Owner)) ms hk
  generalize hF : ms.filter (fun m => decide (m.project.val = p ∧ m.role = .Owner)) = F at h hn
  match F, hF, h, hn with
  | a :: b :: _, hF, _, hn =>
    have ha : a ∈ ms.filter _ := hF ▸ List.mem_cons_self
    have hb : b ∈ ms.filter _ := hF ▸ List.mem_cons_of_mem _ List.mem_cons_self
    simp only [List.mem_filter, decide_eq_true_eq] at ha hb
    simp only [List.map_cons, List.nodup_cons, List.mem_cons, List.mem_map, not_or,
      not_exists, not_and] at hn
    have hab : mkey a ≠ mkey b := hn.1.1
    by_cases hat : a.user.val = t
    · refine ⟨b, hb.1, hb.2.1, hb.2.2, ?_⟩
      intro hbt; apply hab; simp [mkey, ha.2.1, hb.2.1, hat, hbt]
    · exact ⟨a, ha.1, ha.2.1, ha.2.2, hat⟩

theorem roleOf_some {ms : List Member} {p u : Nat} {r : Role} (h : roleOf ms p u = some r) :
    ∃ n ∈ ms, n.project.val = p ∧ n.user.val = u ∧ n.role = r := by
  unfold roleOf at h
  obtain ⟨n, hn, rfl⟩ := Option.map_eq_some_iff.1 h
  have := List.find?_some hn
  simp only [decide_eq_true_eq] at this
  exact ⟨n, List.mem_of_find?_eq_some hn, this.1, this.2, rfl⟩

/-- With unique keys, the row `roleOf` finds is the only row with that key. -/
theorem roleOf_unique {ms : List Member} {p u : Nat} {r : Role} (hk : (ms.map mkey).Nodup)
    (h : roleOf ms p u = some r) {n : Member} (hn : n ∈ ms) (hp : n.project.val = p)
    (hu : n.user.val = u) : n.role = r := by
  obtain ⟨m, hm, hmp, hmu, rfl⟩ := roleOf_some h
  have : n = m := List.inj_on_of_nodup_map hk hn hm (by simp [mkey, hp, hu, hmp, hmu])
  rw [this]

theorem allowed_member {s : St} {u p : Nat} {a : Action} (h : allowed s u p a) :
    ∃ n ∈ s.members, n.project.val = p ∧ n.user.val = u := by
  unfold allowed at h
  split at h
  · obtain ⟨n, hn, h1, h2, _⟩ := roleOf_some ‹_›
    exact ⟨n, hn, h1, h2⟩
  · simp at h

theorem allowed_proj {s : St} (hi : Inv s) {u p : Nat} {a : Action} (h : allowed s u p a) :
    ∃ q ∈ s.projects, q.id.val = p := by
  obtain ⟨n, hn, hp, _⟩ := allowed_member h
  obtain ⟨q, hq, hqid⟩ := hi.member_proj n hn
  exact ⟨q, hq, by rw [hqid, hp]⟩

theorem roleOf_of_mem {ms : List Member} (hk : (ms.map mkey).Nodup) {n : Member} (hn : n ∈ ms) :
    roleOf ms n.project.val n.user.val = some n.role := by
  unfold roleOf
  obtain ⟨m, hm⟩ : ∃ m, ms.find? (fun m => decide (m.project.val = n.project.val ∧ m.user.val = n.user.val)) = some m := by
    apply Option.isSome_iff_exists.1
    rw [List.find?_isSome]
    exact ⟨n, hn, by simp⟩
  have hp := List.find?_some hm
  simp only [decide_eq_true_eq] at hp
  have : m = n := List.inj_on_of_nodup_map hk (List.mem_of_find?_eq_some hm) hn
    (by simp [mkey, hp.1, hp.2])
  rw [hm, this]; rfl

/-! ## Preservation, one lemma per kind of write set -/

theorem inv_create_project {s : St} (hi : Inv s) (c : Counter) (p : Project) (u : U64)
    (hc : c.next_id.val = s.next + 1) (hp : p.id.val = s.next) :
    Inv (applyWrite (applyWrite (applyWrite s (.SetCounter c)) (.PutProject p))
      (.PutMember ⟨p.id, u, .Owner⟩)) := by
  simp only [applyWrite]
  have pf : ∀ y ∈ s.projects, (fun q : Project => (q.id.val, 0)) y ≠ (fun q : Project => (q.id.val, 0)) p := by
    intro y hy h; simp at h; have := hi.proj_fresh y hy; omega
  have mf : ∀ y ∈ s.members, (fun n : Member => (n.project.val, n.user.val)) y ≠
      (fun n : Member => (n.project.val, n.user.val)) ⟨p.id, u, .Owner⟩ := by
    intro y hy h; simp at h
    obtain ⟨q, hq, hqid⟩ := hi.member_proj y hy
    have := hi.proj_fresh q hq; rw [hqid] at this; ((try dsimp only at *); omega)
  have hP := fun z => mem_upsert_fresh (z := z) pf
  have hM := fun z => mem_upsert_fresh (z := z) mf
  constructor
  · intro q hq
    rw [owners_pos_iff]
    rcases (hP q).1 hq with rfl | hq
    · exact ⟨_, (hM _).2 (Or.inl rfl), rfl, rfl⟩
    · obtain ⟨n, hn, h1, h2⟩ := (owners_pos_iff _ _).1 (hi.owned q hq)
      exact ⟨n, (hM n).2 (Or.inr hn), h1, h2⟩
  · intro m hm
    rcases (hM m).1 hm with rfl | hm
    · exact ⟨p, (hP p).2 (Or.inl rfl), rfl⟩
    · obtain ⟨q, hq, h⟩ := hi.member_proj m hm
      exact ⟨q, (hP q).2 (Or.inr hq), h⟩
  · intro d hd
    obtain ⟨q, hq, h⟩ := hi.doc_proj d hd
    exact ⟨q, (hP q).2 (Or.inr hq), h⟩
  · exact nodup_map_upsert _ _ pkey_iff _ _ hi.proj_keys
  · exact nodup_map_upsert _ _ mkey_iff _ _ hi.member_keys
  · exact hi.doc_keys
  · intro q hq
    rcases (hP q).1 hq with rfl | hq
    · ((try dsimp only at *); omega)
    · have := hi.proj_fresh q hq; ((try dsimp only at *); omega)
  · intro d hd; have := hi.doc_fresh d hd; ((try dsimp only at *); omega)
  · exact hi.four_eyes
  · exact hi.approver_iff
  · intro w hw
    obtain ⟨q, hq, h⟩ := hi.hook_proj w hw
    exact ⟨q, (hP q).2 (Or.inr hq), h⟩
  · exact hi.hook_keys

theorem inv_put_member {s : St} (hi : Inv s) (m : Member)
    (hproj : ∃ q ∈ s.projects, q.id = m.project)
    (hguard : m.role = .Owner ∨ roleOf s.members m.project.val m.user.val ≠ some .Owner ∨
      1 < owners s.members m.project.val) :
    Inv (applyWrite s (.PutMember m)) := by
  simp only [applyWrite]
  have hk := nodup_map_k_of_g _ _ mkey_iff _ hi.member_keys
  have hM := fun z => mem_upsert_iff (z := z) (x := m) hk
  refine { hi with owned := ?_, member_proj := ?_, member_keys := ?_ }
  · intro q hq
    obtain ⟨n, hn, h1, h2⟩ := (owners_pos_iff _ _).1 (hi.owned q hq)
    rw [owners_pos_iff]
    by_cases hnm : (n.project.val, n.user.val) = (m.project.val, m.user.val)
    · simp only [Prod.mk.injEq] at hnm
      rcases hguard with hr | hr | hr
      · exact ⟨m, (hM m).2 (Or.inl rfl), by rw [← hnm.1, h1], hr⟩
      · exfalso; apply hr
        have := roleOf_of_mem (nodup_map_k_of_g _ _ mkey_iff _ hi.member_keys) hn
        rw [← hnm.1, ← hnm.2, this, h2]
      · obtain ⟨n2, hn2, h3, h4, h5⟩ := other_owner (t := m.user.val)
          (nodup_map_k_of_g _ _ mkey_iff _ hi.member_keys) hr
        refine ⟨n2, (hM n2).2 (Or.inr ⟨hn2, ?_⟩), by rw [h3, ← hnm.1, h1], h4⟩
        intro h; exact h5 (Prod.mk.inj h).2
    · exact ⟨n, (hM n).2 (Or.inr ⟨hn, hnm⟩), h1, h2⟩
  · intro z hz
    rcases (hM z).1 hz with rfl | ⟨hz, _⟩
    · exact hproj
    · exact hi.member_proj z hz
  · exact nodup_map_upsert _ _ mkey_iff _ _ hi.member_keys

theorem inv_del_member {s : St} (hi : Inv s) (p u : U64) (r : Role)
    (hr : roleOf s.members p.val u.val = some r) (hguard : r = .Owner → 1 < owners s.members p.val) :
    Inv (applyWrite s (.DelMember p u)) := by
  simp only [applyWrite]
  have hk := nodup_map_k_of_g _ _ mkey_iff _ hi.member_keys
  refine { hi with owned := ?_, member_proj := ?_, member_keys := ?_ }
  · intro q hq
    obtain ⟨n, hn, h1, h2⟩ := (owners_pos_iff _ _).1 (hi.owned q hq)
    rw [owners_pos_iff]
    by_cases hnm : n.project = p ∧ n.user = u
    · have hrole := roleOf_unique hk hr hn (by rw [hnm.1]) (by rw [hnm.2])
      obtain ⟨n2, hn2, h3, h4, h5⟩ := other_owner (t := u.val) hk (hguard (hrole ▸ h2))
      refine ⟨n2, ?_, by rw [h3, ← h1, hnm.1], h4⟩
      simp only [List.mem_filter, decide_eq_true_eq]
      refine ⟨hn2, ?_⟩
      rintro ⟨_, hu⟩; exact h5 (by rw [hu])
    · exact ⟨n, by simp only [List.mem_filter, decide_eq_true_eq]; exact ⟨hn, hnm⟩, h1, h2⟩
  · intro z hz
    simp only [List.mem_filter] at hz
    exact hi.member_proj z hz.1
  · exact nodup_map_filter _ _ _ hi.member_keys

theorem inv_create_doc {s : St} (hi : Inv s) (c : Counter) (d : Document)
    (hc : c.next_id.val = s.next + 1) (hd : d.id.val = s.next)
    (hproj : ∃ q ∈ s.projects, q.id = d.project) (hst : d.status = .Draft)
    (hap : d.approver = none) :
    Inv (applyWrite (applyWrite s (.SetCounter c)) (.PutDocument d)) := by
  simp only [applyWrite]
  have df : ∀ y ∈ s.docs, (fun e : Document => (e.id.val, 0)) y ≠ (fun e : Document => (e.id.val, 0)) d := by
    intro y hy h; simp at h; have := hi.doc_fresh y hy; omega
  have hD := fun z => mem_upsert_fresh (z := z) df
  constructor
  · exact hi.owned
  · exact hi.member_proj
  · intro z hz
    rcases (hD z).1 hz with rfl | hz
    · exact hproj
    · exact hi.doc_proj z hz
  · exact hi.proj_keys
  · exact hi.member_keys
  · exact nodup_map_upsert _ _ dkey_iff _ _ hi.doc_keys
  · intro q hq; have := hi.proj_fresh q hq; ((try dsimp only at *); omega)
  · intro z hz
    rcases (hD z).1 hz with rfl | hz
    · ((try dsimp only at *); omega)
    · have := hi.doc_fresh z hz; ((try dsimp only at *); omega)
  · intro z hz hs
    rcases (hD z).1 hz with rfl | hz
    · rw [hst] at hs; simp at hs
    · exact hi.four_eyes z hz hs
  · intro z hz
    rcases (hD z).1 hz with rfl | hz
    · simp [hst, hap]
    · exact hi.approver_iff z hz
  · exact hi.hook_proj
  · exact hi.hook_keys

theorem inv_put_doc {s : St} (hi : Inv s) (d old : Document)
    (hold : old ∈ s.docs) (hid : old.id = d.id) (hp : old.project = d.project)
    (h4 : d.status = .Approved ∨ d.status = .Published → ∃ a, d.approver = some a ∧ a ≠ d.author)
    (hiff : d.approver.isSome ↔ (d.status = .Approved ∨ d.status = .Published)) :
    Inv (applyWrite s (.PutDocument d)) := by
  simp only [applyWrite]
  have hk := nodup_map_k_of_g _ _ dkey_iff _ hi.doc_keys
  have hD := fun z => mem_upsert_iff (z := z) (x := d) hk
  refine { hi with doc_proj := ?_, doc_keys := ?_, doc_fresh := ?_, four_eyes := ?_, approver_iff := ?_ }
  · intro z hz
    rcases (hD z).1 hz with rfl | ⟨hz, _⟩
    · obtain ⟨q, hq, h⟩ := hi.doc_proj old hold; exact ⟨q, hq, h.trans hp⟩
    · exact hi.doc_proj z hz
  · exact nodup_map_upsert _ _ dkey_iff _ _ hi.doc_keys
  · intro z hz
    rcases (hD z).1 hz with rfl | ⟨hz, _⟩
    · rw [← hid]; exact hi.doc_fresh old hold
    · exact hi.doc_fresh z hz
  · intro z hz hs
    rcases (hD z).1 hz with rfl | ⟨hz, _⟩
    · exact h4 hs
    · exact hi.four_eyes z hz hs
  · intro z hz
    rcases (hD z).1 hz with rfl | ⟨hz, _⟩
    · exact hiff
    · exact hi.approver_iff z hz

theorem inv_del_doc {s : St} (hi : Inv s) (i : U64) : Inv (applyWrite s (.DelDocument i)) := by
  simp only [applyWrite]
  refine { hi with doc_proj := ?_, doc_keys := ?_, doc_fresh := ?_, four_eyes := ?_, approver_iff := ?_ }
  · intro z hz; simp only [List.mem_filter] at hz; exact hi.doc_proj z hz.1
  · exact nodup_map_filter _ _ _ hi.doc_keys
  · intro z hz; simp only [List.mem_filter] at hz; exact hi.doc_fresh z hz.1
  · intro z hz; simp only [List.mem_filter] at hz; exact hi.four_eyes z hz.1
  · intro z hz; simp only [List.mem_filter] at hz; exact hi.approver_iff z hz.1

theorem inv_put_webhook {s : St} (hi : Inv s) (w : Webhook)
    (hproj : ∃ q ∈ s.projects, q.id = w.project) : Inv (applyWrite s (.PutWebhook w)) := by
  simp only [applyWrite]
  have hk := nodup_map_k_of_g _ _ hkey_iff _ hi.hook_keys
  refine { hi with hook_proj := ?_, hook_keys := ?_ }
  · intro z hz
    rcases (mem_upsert_iff (x := w) hk).1 hz with rfl | ⟨hz, _⟩
    · exact hproj
    · exact hi.hook_proj z hz
  · exact nodup_map_upsert _ _ hkey_iff _ _ hi.hook_keys

theorem inv_del_webhook {s : St} (hi : Inv s) (p : U64) : Inv (applyWrite s (.DelWebhook p)) := by
  simp only [applyWrite]
  refine { hi with hook_proj := ?_, hook_keys := ?_ }
  · intro z hz; simp only [List.mem_filter] at hz; exact hi.hook_proj z hz.1
  · exact nodup_map_filter _ _ _ hi.hook_keys

/-- Effects leave through the outbox; the state is unchanged. -/
@[simp] theorem applyWrite_emit (s : St) (e : Effect) : applyWrite s (.Emit e) = s := rfl

/-- Successful transitions preserve the invariants. -/
theorem inv_preserved (a : Principal) (s : Snapshot) (c : Command) ws r
    (hinv : Inv (Snapshot.toSt s)) (h : transition a s c = .ok (.Ok (ws, r))) :
    Inv (applyAll (Snapshot.toSt s) ws.val) := by
  refine of_spec (P := fun ws _ => Inv (applyAll (Snapshot.toSt s) ws.val)) ?_ h
  walk transition
  all_goals (simp only [OnOk]; try trivial)
  all_goals (simp_all only [applyAll, List.foldl, List.nil_append, List.cons_append,
    vec_new_val, List.foldl_cons, applyWrite_emit])
  all_goals simp only [Snapshot.toSt] at *
  -- SetMember
  all_goals try (
    refine inv_put_member hinv _ ?_ ?_
    · obtain ⟨q, hq, hqid⟩ := allowed_proj hinv b_post.symm
      exact ⟨q, hq, (u64_val_eq _ _).1 hqid⟩
    · first
      | (right; left; rw [← o_post]; simp; done)
      | (left; simp_all; done)
      | (right; right; rw [← i_post]; scalar_tac))
  -- RemoveMember
  all_goals try (
    refine inv_del_member hinv _ _ _ o_post.symm ?_
    intro hr
    first
      | (rw [← i_post]; simp_all)
      | simp_all)
  -- DeleteDocument
  all_goals try exact inv_del_doc hinv _
  -- SetWebhook
  all_goals try exact inv_del_webhook hinv _
  all_goals try (
    obtain ⟨q, hq, hqid⟩ := allowed_proj hinv b_post.symm
    exact inv_put_webhook hinv _ ⟨q, hq, (u64_val_eq _ _).1 hqid⟩)
  -- CreateProject
  all_goals try exact inv_create_project hinv _ _ a.user r_post.2 rfl
  -- CreateDocument
  all_goals try (
    obtain ⟨q, hq, hqid⟩ := allowed_proj hinv b_post.symm
    exact inv_create_doc hinv _ _ r_post.2 rfl ⟨q, hq, (u64_val_eq _ _).1 hqid⟩ rfl rfl)
  -- EditDocument, Submit, Approve, Publish
  all_goals try (
    obtain ⟨hf, hread, hact⟩ := authDoc_ok r_post.symm
    have hmem := (findDoc_some hf).1
    rcases r1_post with ⟨_, hbad⟩ | ⟨ver, hver, hv1⟩
    · cases hbad
    cases hv1
    refine inv_put_doc hinv _ v hmem rfl rfl ?_ ?_
    · intro hst
      first
        | (simp at hst; done)
        | (refine ⟨_, rfl, ?_⟩; simp_all)
        | (have hA : v.status = .Approved := by simp_all
           exact hinv.four_eyes v hmem (Or.inl hA))
    · first
        | (simp; done)
        | (have hA : v.status = .Approved := by simp_all
           have := (hinv.approver_iff v hmem).2 (Or.inl hA)
           simp [this]))

theorem init_inv : Inv init := by
  constructor <;> simp [init]

/-- Every state the database can reach satisfies the invariants. -/
theorem reachable_inv {s : St} (h : Reachable s) : Inv s := by
  induction h with
  | init => exact init_inv
  | step _ ht ih => exact inv_preserved _ _ _ _ _ ih ht

end docs_kernel.Theorems
