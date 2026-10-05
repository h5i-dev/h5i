import Spec
/-!
# What each command does

Two lemmas per command: what a successful run writes and why it succeeded,
and when it succeeds (the business conditions plus a bound on the numbers).
-/
open Aeneas Aeneas.Std Result ledger_kernel ledger_kernel.Spec H5iAppLib

namespace ledger_kernel.Commands

attribute [simp] u64_val_eq

/-! ## Helpers -/

@[step] theorem two_spec (a b : Write) : two a b ⦃ v => v.val = [a, b] ⦄ := by
  unfold two; step*

@[step] theorem find_account_spec (v : alloc.vec.Vec Account) (id : U64) :
    find_account v id ⦃ o => o = v.val.find? (fun a => a.id.val = id.val) ⦄ := by
  unfold find_account find_account_loop
  apply WP.spec_mono (loop_search v.val (fun a => decide (a.id = id)) (fun o : Option Account => o)
    (fun _ a => some a) none _ ?_ 0#usize (by simp))
  · intro r hr; rw [search_find _ _ _ hr]; simp
  · intro j hj; unfold find_account_loop.body; h5i_step

theorem find_eq (s : Snapshot) (id : U64) :
    s.accounts.val.find? (fun a => a.id.val = id.val) = findAcc (Snapshot.toSt s) id.val := rfl

/-! ## What a successful command writes -/

/-- A new empty account with the next id, owned by the caller. -/
theorem open_spec (u : U64) (s : Snapshot) :
    «open» u s ⦃ r => ∀ ws rep, r = .Ok (ws, rep) →
      ∃ n : U64, n.val = s.ledger.next_id.val + 1 ∧
        ws.val = [.PutAccount ⟨s.ledger.next_id, u, 0#u64⟩, .SetLedger { s.ledger with next_id := n }] ⦄ := by
  unfold «open»
  step*
  simp only [core.num.U64.MAX, U64.rMax] at *; scalar_tac

/-- The caller's account gains `amt`, and so do total deposits. -/
theorem deposit_spec (u : U64) (s : Snapshot) (id amt : U64) :
    deposit u s id amt ⦃ r => ∀ ws rep, r = .Ok (ws, rep) →
      ∃ q, findAcc (Snapshot.toSt s) id.val = some q ∧ q.owner = u ∧
        ∃ b d : U64, b.val = q.balance.val + amt.val ∧ d.val = s.ledger.deposited.val + amt.val ∧
          ws.val = [.PutAccount { q with id, balance := b }, .SetLedger { s.ledger with deposited := d }] ⦄ := by
  unfold deposit
  step*
  all_goals try (simp only [core.num.U64.MAX, U64.rMax] at *; scalar_tac)
  intro ws rep h; obtain ⟨rfl, rfl⟩ := ok_inj h
  have hf : findAcc (Snapshot.toSt s) id.val = some a := by rw [← find_eq, ← o_post]; exact ‹o = some a›
  exact ⟨a, hf, by simpa using ‹¬(a.owner != u) = true›, i1, i2, i1_post, i2_post, v_post⟩

/-- The caller's account loses `amt`, which it holds, and total withdrawals
grow by `amt`. -/
theorem withdraw_spec (u : U64) (s : Snapshot) (id amt : U64) :
    withdraw u s id amt ⦃ r => ∀ ws rep, r = .Ok (ws, rep) →
      ∃ q, findAcc (Snapshot.toSt s) id.val = some q ∧ q.owner = u ∧ amt.val ≤ q.balance.val ∧
        ∃ b w : U64, b.val = q.balance.val - amt.val ∧ w.val = s.ledger.withdrawn.val + amt.val ∧
          ws.val = [.PutAccount { q with id, balance := b }, .SetLedger { s.ledger with withdrawn := w }] ⦄ := by
  unfold withdraw
  step*
  all_goals try (simp only [core.num.U64.MAX, U64.rMax] at *; scalar_tac)
  intro ws rep h; obtain ⟨rfl, rfl⟩ := ok_inj h
  have hf : findAcc (Snapshot.toSt s) id.val = some a := by rw [← find_eq, ← o_post]; exact ‹o = some a›
  exact ⟨a, hf, by simpa using ‹¬(a.owner != u) = true›, by scalar_tac, i1, i2, i1_post, i2_post, v_post⟩

/-- Two different accounts: the caller's loses `amt`, which it holds, and the
other gains `amt`. -/
theorem transfer_spec (u : U64) (s : Snapshot) (src dst amt : U64) :
    transfer u s src dst amt ⦃ r => ∀ ws rep, r = .Ok (ws, rep) → src ≠ dst ∧
      ∃ qa qb, findAcc (Snapshot.toSt s) src.val = some qa ∧ qa.owner = u ∧ amt.val ≤ qa.balance.val ∧
        findAcc (Snapshot.toSt s) dst.val = some qb ∧
        ∃ x y : U64, x.val = qa.balance.val - amt.val ∧ y.val = qb.balance.val + amt.val ∧
          ws.val = [.PutAccount { qa with id := src, balance := x }, .PutAccount { qb with id := dst, balance := y }] ⦄ := by
  unfold transfer
  step*
  all_goals try (simp only [core.num.U64.MAX, U64.rMax] at *; scalar_tac)
  intro ws rep h; obtain ⟨rfl, rfl⟩ := ok_inj h
  have ha : findAcc (Snapshot.toSt s) src.val = some a := by rw [← find_eq, ← o_post]; exact ‹o = some a›
  have hb : findAcc (Snapshot.toSt s) dst.val = some b := by rw [← find_eq, ← o1_post]; exact ‹o1 = some b›
  exact ⟨‹¬src = dst›, a, b, ha, by simpa using ‹¬(a.owner != u) = true›, by scalar_tac, hb,
    i1, i2, i1_post, i2_post, v_post⟩

/-! ## When a command succeeds

The kernel checks every addition. These checks pass for small enough numbers;
`Theorems.lean` shows the invariant keeps them small. -/

theorem deposit_ok (u : U64) (s : Snapshot) (id amt : U64) (q : Account)
    (hq : findAcc (Snapshot.toSt s) id.val = some q) (ho : q.owner = u)
    (hd : s.ledger.deposited.val + amt.val < 2 ^ 64) (hb : q.balance.val ≤ s.ledger.deposited.val) :
    deposit u s id amt ⦃ r => ∃ ws rep, r = .Ok (ws, rep) ⦄ := by
  unfold deposit
  step*
  all_goals try (simp only [core.num.U64.MAX, U64.rMax] at *; scalar_tac)
  all_goals (rw [find_eq, hq] at o_post; subst o_post; simp_all; try scalar_tac)

theorem withdraw_ok (u : U64) (s : Snapshot) (id amt : U64) (q : Account)
    (hq : findAcc (Snapshot.toSt s) id.val = some q) (ho : q.owner = u) (hamt : amt.val ≤ q.balance.val)
    (hw : s.ledger.withdrawn.val + q.balance.val < 2 ^ 64) :
    withdraw u s id amt ⦃ r => ∃ ws rep, r = .Ok (ws, rep) ⦄ := by
  unfold withdraw
  step*
  all_goals try (simp only [core.num.U64.MAX, U64.rMax] at *; scalar_tac)
  all_goals (rw [find_eq, hq] at o_post; subst o_post; simp_all; try scalar_tac)

theorem transfer_ok (u : U64) (s : Snapshot) (src dst amt : U64) (qa qb : Account) (hne : src ≠ dst)
    (ha : findAcc (Snapshot.toSt s) src.val = some qa) (ho : qa.owner = u) (hamt : amt.val ≤ qa.balance.val)
    (hb : findAcc (Snapshot.toSt s) dst.val = some qb) (hfit : qa.balance.val + qb.balance.val < 2 ^ 64) :
    transfer u s src dst amt ⦃ r => ∃ ws rep, r = .Ok (ws, rep) ⦄ := by
  unfold transfer
  step*
  all_goals try (simp only [core.num.U64.MAX, U64.rMax] at *; scalar_tac)
  all_goals try (rw [find_eq, ha] at o_post; subst o_post)
  all_goals try (rw [find_eq, hb] at o1_post; subst o1_post)
  all_goals (simp_all [U64.rMax]; try omega)

end ledger_kernel.Commands
