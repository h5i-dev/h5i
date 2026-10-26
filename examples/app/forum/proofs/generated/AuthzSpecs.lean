-- Copied from h5i-app-std's proofs/AuthzSpecs.lean by `h5i app extract`. Do not edit.
import ForumKernel
import H5iAppLib
/-!
# The authorization guard's theorem

`h5i_app_std::authz::guard` runs a kernel's `transition` and refuses the
command unless every write is permitted, each in the state the writes before
it leave (the kernel's own `apply` of the prefix), and every row the reply
discloses is visible before or after the command.

`guard_authorized` is proven here once, over arbitrary `Transition`, `Policy`
and `Disclose` instances: it never looks inside `transition`, `apply`,
`permits`, `visible` or `disclosed`. So an app whose server runs
`guarded_transition` gets `Authorized` by instantiating it, with no proof of
its own (`h5i_authz`, at the end).

Partial correctness throughout: the statements are about a run that returned
`ok`, so none of the app's functions needs a total spec.

This file is checked against the crate's own extraction. A kernel that calls
the guard gets a copy importing its extraction instead (`h5i app extract`,
with `std-specs = true`), like `StdSpecs`.
-/
open Aeneas Aeneas.Std Result H5iAppLib forum_kernel

set_option linter.unusedTactic false
set_option linter.unreachableTactic false

namespace h5i_app_std.Specs.authz

variable {A S C W R E D T P : Type}

/-- The first `n` writes. -/
def take (v : alloc.vec.Vec W) (n : Nat) : alloc.vec.Vec W :=
  .from (v.val.take n) (by have := v.property; simp only [List.length_take]; omega)

@[simp] theorem take_val (v : alloc.vec.Vec W) (n : Nat) : (take v n).val = v.val.take n := by
  simp [take]

theorem take_length (v : alloc.vec.Vec W) : take v v.val.length = v := by
  apply alloc.vec.Vec.ext; simp

/-! ## The guarantee -/

/-- Every write is permitted in the state the writes before it leave, by the
kernel's own `apply`. -/
def WritesPermitted (tinst : h5i_app_std.authz.Transition T A S C W R E)
    (pinst : h5i_app_std.authz.Policy P A S W D) (a : A) (s : S) (ws : alloc.vec.Vec W) : Prop :=
  ∀ i (hi : i < ws.val.length), ∃ si, tinst.kernel_apply s (take ws i) = ok si ∧ pinst.permits a si ws.val[i] = ok true

/-- Every row the reply discloses is visible before the command or after it. -/
def ReadsVisible (tinst : h5i_app_std.authz.Transition T A S C W R E)
    (pinst : h5i_app_std.authz.Policy P A S W D) (dinst : h5i_app_std.authz.Disclose R D)
    (a : A) (s : S) (ws : alloc.vec.Vec W) (r : R) : Prop :=
  ∃ after ds, tinst.kernel_apply s ws = ok after ∧ dinst.disclosed r = ok ds ∧
    ∀ d ∈ ds.val, pinst.visible a s d = ok true ∨ pinst.visible a after d = ok true

/-- The framework theorem for a guarded kernel `g`: for every actor, state
and command, a committed command's writes are permitted and its disclosed
rows visible. -/
def Authorized (g : A → S → C → Result (core.result.Result (alloc.vec.Vec W × R) (h5i_app_std.authz.Guarded E)))
    (tinst : h5i_app_std.authz.Transition T A S C W R E)
    (pinst : h5i_app_std.authz.Policy P A S W D) (dinst : h5i_app_std.authz.Disclose R D) : Prop :=
  ∀ a s c ws r, g a s c = ok (.Ok (ws, r)) →
    WritesPermitted tinst pinst a s ws ∧ ReadsVisible tinst pinst dinst a s ws r

/-- `g` never refuses a command the kernel accepted: the policy adds no
runtime denial, so `Authorized` holds of the kernel's own `transition`. -/
def NeverDenied (g : A → S → C → Result (core.result.Result (alloc.vec.Vec W × R) (h5i_app_std.authz.Guarded E))) : Prop :=
  ∀ a s c, g a s c ≠ ok (.Err .Denied)

/-! ## The loops -/

theorem push_ok {v v' : alloc.vec.Vec W} {x : W} (h : v.push x = ok v') : v'.val = v.val ++ [x] := by
  unfold alloc.vec.Vec.push at h
  dsimp only at h
  split at h
  · simp only [ok.injEq] at h; subst h; simp
  · simp at h

theorem index_ok {v : alloc.vec.Vec W} {i : Usize} {x : W}
    (h : alloc.vec.Vec.index (core.slice.index.SliceIndexUsizeSlice W) v i = ok x) :
    ∃ hi : i.val < v.val.length, v.val[i.val] = x := by
  have := vec_index_slice_ok_get? h
  rw [List.getElem?_eq_some_iff] at this
  obtain ⟨hi, hx⟩ := this
  exact ⟨hi, hx⟩

h5i_when h5i_app_std.authz.prefix =>
theorem prefix_ok (cinst : core.clone.Clone W) [CloneLaw cinst] (ws : alloc.vec.Vec W) (n : Usize)
    {v : alloc.vec.Vec W} (h : h5i_app_std.authz.prefix cinst ws n = ok v) : v = take ws n.val := by
  unfold h5i_app_std.authz.prefix h5i_app_std.authz.prefix_loop at h
  refine loop_idx_ok _ (fun x => x.2) ws.val.length
    (fun x => x.1.val = ws.val.take x.2.val ∧ x.2.val ≤ n.val) (fun v => v = take ws n.val) ?_
    _ v ⟨by simp, by simp⟩ (by simp) h
  rintro ⟨out, i⟩ r ⟨hout, hin⟩ hle hr
  simp only at hout hin hle
  unfold h5i_app_std.authz.prefix_loop.body at hr
  dsimp only at hr
  split at hr
  · split at hr
    · obtain ⟨t, ht, hr⟩ := bind_eq_ok.1 hr
      obtain ⟨t1, ht1, hr⟩ := bind_eq_ok.1 hr
      obtain ⟨out1, hout1, hr⟩ := bind_eq_ok.1 hr
      obtain ⟨i2, hi2, hr⟩ := bind_eq_ok.1 hr
      simp only [ok.injEq] at hr; subst hr
      obtain ⟨hlt, rfl⟩ := index_ok ht
      rw [CloneLaw.clone_ok] at ht1; simp only [ok.injEq] at ht1; subst ht1
      have hv := push_ok hout1
      have hi2v := add_ok_val hi2
      simp only
      refine ⟨⟨?_, ?_⟩, ?_, ?_⟩
      · rw [hv, hout, hi2v, show ((1#usize : Usize) : Nat) = 1 from rfl, List.take_add_one,
          List.getElem?_eq_getElem hlt]
        rfl
      · scalar_tac
      · scalar_tac
      · scalar_tac
    · simp only [ok.injEq] at hr; subst hr
      apply alloc.vec.Vec.ext
      simp only [take_val, hout]
      rw [List.take_of_length_le (by scalar_tac), List.take_of_length_le (by scalar_tac)]
  · simp only [ok.injEq] at hr; subst hr
    apply alloc.vec.Vec.ext
    simp only [take_val, hout]
    congr 1; scalar_tac

h5i_when h5i_app_std.authz.check_writes =>
theorem check_writes_ok (cinst : core.clone.Clone W) [CloneLaw cinst]
    (tinst : h5i_app_std.authz.Transition T A S C W R E) (pinst : h5i_app_std.authz.Policy P A S W D)
    (a : A) (s : S) (ws : alloc.vec.Vec W)
    (h : h5i_app_std.authz.check_writes cinst tinst pinst a s ws = ok true) :
    WritesPermitted tinst pinst a s ws := by
  unfold h5i_app_std.authz.check_writes h5i_app_std.authz.check_writes_loop at h
  refine loop_true_witness _
    (fun i : Usize => i.val ≤ ws.val.length ∧ ∀ j (hj : j < i.val) (hj' : j < ws.val.length),
      ∃ si, tinst.kernel_apply s (take ws j) = ok si ∧ pinst.permits a si ws.val[j] = ok true)
    (fun i => ws.val.length - i.val) _ ?_ 0#usize ⟨by simp, by simp⟩ h
  rintro i r ⟨hle, hall⟩ hr
  unfold h5i_app_std.authz.check_writes_loop.body at hr
  dsimp only at hr
  split at hr
  · rename_i hlt
    obtain ⟨v, hv, hr⟩ := bind_eq_ok.1 hr
    obtain ⟨cur, hcur, hr⟩ := bind_eq_ok.1 hr
    obtain ⟨t, ht, hr⟩ := bind_eq_ok.1 hr
    obtain ⟨hlt', rfl⟩ := index_ok ht
    obtain ⟨b, hb, hr⟩ := bind_eq_ok.1 hr
    rw [prefix_ok cinst ws i hv] at hcur
    split at hr
    · obtain ⟨i2, hi2, hr⟩ := bind_eq_ok.1 hr
      simp only [ok.injEq] at hr; subst hr
      have hi2v := add_ok_val hi2
      refine ⟨⟨by scalar_tac, fun j hj hj' => ?_⟩, by scalar_tac⟩
      rcases Nat.lt_or_ge j i.val with hji | hji
      · exact hall j hji hj'
      · have : j = i.val := by scalar_tac
        subst this
        exact ⟨cur, hcur, by subst_vars; exact hb⟩
    · simp only [ok.injEq] at hr; subst hr; simp
  · rename_i hge
    simp only [ok.injEq] at hr; subst hr
    intro _ j hj
    exact hall j (by scalar_tac) hj

h5i_when h5i_app_std.authz.check_reads =>
theorem check_reads_ok (pinst : h5i_app_std.authz.Policy P A S W D) (a : A) (before after : S)
    (ds : alloc.vec.Vec D) (h : h5i_app_std.authz.check_reads pinst a before after ds = ok true) :
    ∀ d ∈ ds.val, pinst.visible a before d = ok true ∨ pinst.visible a after d = ok true := by
  unfold h5i_app_std.authz.check_reads h5i_app_std.authz.check_reads_loop at h
  refine loop_true_witness _
    (fun i : Usize => i.val ≤ ds.val.length ∧ ∀ j (hj : j < i.val) (hj' : j < ds.val.length),
      pinst.visible a before ds.val[j] = ok true ∨ pinst.visible a after ds.val[j] = ok true)
    (fun i => ds.val.length - i.val) _ ?_ 0#usize ⟨by simp, by simp⟩ h
  · rintro i r ⟨hle, hall⟩ hr
    unfold h5i_app_std.authz.check_reads_loop.body at hr
    dsimp only at hr
    split at hr
    · obtain ⟨t, ht, hr⟩ := bind_eq_ok.1 hr
      obtain ⟨hlt, rfl⟩ := index_ok ht
      obtain ⟨b, hb, hr⟩ := bind_eq_ok.1 hr
      have step (hv : pinst.visible a before ds.val[i.val] = ok true ∨
          pinst.visible a after ds.val[i.val] = ok true) {i2 : Usize} (hi2 : i + 1#usize = ok i2) :
          (i2.val ≤ ds.val.length ∧ ∀ j (hj : j < i2.val) (hj' : j < ds.val.length),
            pinst.visible a before ds.val[j] = ok true ∨ pinst.visible a after ds.val[j] = ok true) ∧
          ds.val.length - i2.val < ds.val.length - i.val := by
        have hi2v := add_ok_val hi2
        refine ⟨⟨by scalar_tac, fun j hj hj' => ?_⟩, by scalar_tac⟩
        rcases Nat.lt_or_ge j i.val with hji | hji
        · exact hall j hji hj'
        · have : j = i.val := by scalar_tac
          subst this; exact hv
      split at hr
      · obtain ⟨i2, hi2, hr⟩ := bind_eq_ok.1 hr
        simp only [ok.injEq] at hr; subst hr
        exact step (.inl (by subst_vars; exact hb)) hi2
      · obtain ⟨b1, hb1, hr⟩ := bind_eq_ok.1 hr
        split at hr
        · obtain ⟨i2, hi2, hr⟩ := bind_eq_ok.1 hr
          simp only [ok.injEq] at hr; subst hr
          exact step (.inr (by subst_vars; exact hb1)) hi2
        · simp only [ok.injEq] at hr; subst hr; simp
    · simp only [ok.injEq] at hr; subst hr
      intro _ d hd
      obtain ⟨j, hj, rfl⟩ := List.getElem_of_mem hd
      exact hall j (by scalar_tac) hj

/-! ## The guard -/

h5i_when h5i_app_std.authz.guard =>
/-- The guard's theorem, for every kernel and policy: a committed command is
the kernel's own result, its writes are permitted and its disclosed rows
visible. -/
theorem guard_authorized (cinst : core.clone.Clone W) [CloneLaw cinst]
    (dinst : h5i_app_std.authz.Disclose R D) (tinst : h5i_app_std.authz.Transition T A S C W R E)
    (pinst : h5i_app_std.authz.Policy P A S W D) (a : A) (s : S) (c : C) (ws : alloc.vec.Vec W) (r : R)
    (h : h5i_app_std.authz.guard cinst dinst tinst pinst a s c = ok (.Ok (ws, r))) :
    tinst.kernel_transition a s c = ok (.Ok (ws, r)) ∧ WritesPermitted tinst pinst a s ws ∧
      ReadsVisible tinst pinst dinst a s ws r := by
  unfold h5i_app_std.authz.guard at h
  obtain ⟨o, ho, h⟩ := bind_eq_ok.1 h
  cases o with
  | Err e => simp at h
  | Ok p =>
    obtain ⟨ws', r'⟩ := p
    simp only at h
    obtain ⟨b, hb, h⟩ := bind_eq_ok.1 h
    split at h
    · obtain ⟨after, hafter, h⟩ := bind_eq_ok.1 h
      obtain ⟨ds, hds, h⟩ := bind_eq_ok.1 h
      obtain ⟨b1, hb1, h⟩ := bind_eq_ok.1 h
      split at h
      · simp only [ok.injEq, core.result.Result.Ok.injEq, Prod.mk.injEq] at h
        obtain ⟨rfl, rfl⟩ := h
        subst_vars
        exact ⟨ho, check_writes_ok cinst tinst pinst a s ws' hb,
          after, ds, hafter, hds, check_reads_ok pinst a s after ds hb1⟩
      · simp at h
    · simp at h

h5i_when h5i_app_std.authz.guard =>
/-- The guard passes the kernel's refusals through. -/
theorem guard_app_error (cinst : core.clone.Clone W)
    (dinst : h5i_app_std.authz.Disclose R D) (tinst : h5i_app_std.authz.Transition T A S C W R E)
    (pinst : h5i_app_std.authz.Policy P A S W D) (a : A) (s : S) (c : C) (e : E)
    (h : tinst.kernel_transition a s c = ok (.Err e)) :
    h5i_app_std.authz.guard cinst dinst tinst pinst a s c = ok (.Err (.App e)) := by
  unfold h5i_app_std.authz.guard; simp [h]

h5i_when h5i_app_std.authz.guard =>
/-- The guard changes nothing but denials: if it returns anything other than
`Denied` for a command the kernel accepted, it returns the kernel's result. -/
theorem guard_only_denies (cinst : core.clone.Clone W)
    (dinst : h5i_app_std.authz.Disclose R D) (tinst : h5i_app_std.authz.Transition T A S C W R E)
    (pinst : h5i_app_std.authz.Policy P A S W D) (a : A) (s : S) (c : C) (ws : alloc.vec.Vec W) (r : R)
    (o : core.result.Result (alloc.vec.Vec W × R) (h5i_app_std.authz.Guarded E))
    (ht : tinst.kernel_transition a s c = ok (.Ok (ws, r)))
    (h : h5i_app_std.authz.guard cinst dinst tinst pinst a s c = ok o) (hne : o ≠ .Err .Denied) :
    o = .Ok (ws, r) := by
  unfold h5i_app_std.authz.guard at h
  rw [ht] at h
  obtain ⟨o', ho', h⟩ := bind_eq_ok.1 h
  simp only [ok.injEq] at ho'; subst ho'
  simp only at h
  obtain ⟨b, hb, h⟩ := bind_eq_ok.1 h
  split at h
  · obtain ⟨after, hafter, h⟩ := bind_eq_ok.1 h
    obtain ⟨ds, hds, h⟩ := bind_eq_ok.1 h
    obtain ⟨b1, hb1, h⟩ := bind_eq_ok.1 h
    split at h
    · simp only [ok.injEq] at h; exact h.symm
    · simp only [ok.injEq] at h; exact absurd h.symm hne
  · simp only [ok.injEq] at h; exact absurd h.symm hne

h5i_when h5i_app_std.authz.guard =>
/-- What an app's generated theorem applies: a kernel defined as the guard
is `Authorized`. -/
theorem authorized_of_eq
    (g : A → S → C → Result (core.result.Result (alloc.vec.Vec W × R) (h5i_app_std.authz.Guarded E)))
    (cinst : core.clone.Clone W) [CloneLaw cinst]
    (dinst : h5i_app_std.authz.Disclose R D) (tinst : h5i_app_std.authz.Transition T A S C W R E)
    (pinst : h5i_app_std.authz.Policy P A S W D)
    (hg : ∀ a s c, g a s c = h5i_app_std.authz.guard cinst dinst tinst pinst a s c) :
    Authorized g tinst pinst dinst := by
  intro a s c ws r h
  rw [hg] at h
  exact (guard_authorized cinst dinst tinst pinst a s c ws r h).2

h5i_when h5i_app_std.authz.guard =>
/-- With `NeverDenied`, every command the kernel accepts commits, so the
policy holds of the kernel's own `transition`: the static guarantee. -/
theorem transition_authorized_of_never_denied
    (g : A → S → C → Result (core.result.Result (alloc.vec.Vec W × R) (h5i_app_std.authz.Guarded E)))
    (cinst : core.clone.Clone W) [CloneLaw cinst]
    (dinst : h5i_app_std.authz.Disclose R D) (tinst : h5i_app_std.authz.Transition T A S C W R E)
    (pinst : h5i_app_std.authz.Policy P A S W D)
    (hg : ∀ a s c, g a s c = h5i_app_std.authz.guard cinst dinst tinst pinst a s c)
    (hnd : NeverDenied g) (a : A) (s : S) (c : C) (ws : alloc.vec.Vec W) (r : R)
    (ht : tinst.kernel_transition a s c = ok (.Ok (ws, r)))
    (htot : ∃ o, g a s c = ok o) :
    WritesPermitted tinst pinst a s ws ∧ ReadsVisible tinst pinst dinst a s ws r := by
  obtain ⟨o, ho⟩ := htot
  have hne : o ≠ .Err .Denied := fun e => hnd a s c (e ▸ ho)
  rw [hg] at ho
  have := guard_only_denies cinst dinst tinst pinst a s c ws r o ht ho hne
  subst this
  exact (guard_authorized cinst dinst tinst pinst a s c ws r ho).2

/-! ## Read-only commands on the actor's view -/

h5i_when h5i_app_std.authz.guard_viewed =>
/-- Every row the reply discloses is visible in `v`. -/
def DisclosedVisible (pinst : h5i_app_std.authz.Policy P A S W D) (dinst : h5i_app_std.authz.Disclose R D)
    (a : A) (v : S) (r : R) : Prop :=
  ∃ ds, dinst.disclosed r = ok ds ∧ ∀ d ∈ ds.val, pinst.visible a v d = ok true

h5i_when h5i_app_std.authz.guard_viewed =>
/-- The framework theorem for a kernel guarded with `guard_viewed`: a
committed command that writes is `Authorized`'s case; a committed read-only
command made no writes and disclosed only rows visible in the actor's view. -/
def AuthorizedViewed
    (g : A → S → C → Result (core.result.Result (alloc.vec.Vec W × R) (h5i_app_std.authz.Guarded E)))
    (tinst : h5i_app_std.authz.Transition T A S C W R E)
    (pinst : h5i_app_std.authz.Policy P A S W D) (dinst : h5i_app_std.authz.Disclose R D)
    {V : Type} (vinst : h5i_app_std.authz.View V A S C) : Prop :=
  ∀ a s c ws r, g a s c = ok (.Ok (ws, r)) →
    (vinst.read_only c = ok false ∧ WritesPermitted tinst pinst a s ws ∧ ReadsVisible tinst pinst dinst a s ws r) ∨
    (vinst.read_only c = ok true ∧ ws.val = [] ∧ ∃ v, vinst.view a s = ok v ∧ DisclosedVisible pinst dinst a v r)

h5i_when h5i_app_std.authz.guard_viewed =>
/-- Noninterference of read-only commands: two states with the same view give
the same result, refusals and denials included. Nothing of `s` outside the
view reaches the reply, derived values (counts, flags) included. -/
def ViewNoninterference
    (g : A → S → C → Result (core.result.Result (alloc.vec.Vec W × R) (h5i_app_std.authz.Guarded E)))
    {V : Type} (vinst : h5i_app_std.authz.View V A S C) : Prop :=
  ∀ a s₁ s₂ c v, vinst.read_only c = ok true → vinst.view a s₁ = ok v → vinst.view a s₂ = ok v →
    g a s₁ c = g a s₂ c

section Viewed
variable {V : Type}

h5i_when h5i_app_std.authz.guard_viewed =>
theorem guard_viewed_noninterference (cinst : core.clone.Clone W)
    (dinst : h5i_app_std.authz.Disclose R D) (tinst : h5i_app_std.authz.Transition T A S C W R E)
    (pinst : h5i_app_std.authz.Policy P A S W D) (vinst : h5i_app_std.authz.View V A S C) :
    ViewNoninterference (h5i_app_std.authz.guard_viewed cinst dinst tinst pinst vinst) vinst := by
  intro a s₁ s₂ c v hro h₁ h₂
  unfold h5i_app_std.authz.guard_viewed
  simp [hro, h₁, h₂]

h5i_when h5i_app_std.authz.guard_viewed =>
theorem guard_viewed_authorized (cinst : core.clone.Clone W) [CloneLaw cinst]
    (dinst : h5i_app_std.authz.Disclose R D) (tinst : h5i_app_std.authz.Transition T A S C W R E)
    (pinst : h5i_app_std.authz.Policy P A S W D) (vinst : h5i_app_std.authz.View V A S C) :
    AuthorizedViewed (h5i_app_std.authz.guard_viewed cinst dinst tinst pinst vinst) tinst pinst dinst vinst := by
  intro a s c ws r h
  unfold h5i_app_std.authz.guard_viewed at h
  obtain ⟨b, hb, h⟩ := bind_eq_ok.1 h
  split at h
  · rename_i hro
    subst hro
    right
    obtain ⟨v, hv, h⟩ := bind_eq_ok.1 h
    obtain ⟨o, ho, h⟩ := bind_eq_ok.1 h
    cases o with
    | Err e => simp at h
    | Ok p =>
      obtain ⟨ws', r'⟩ := p
      simp at h
      split at h
      · rename_i hnil
        obtain ⟨ds, hds, h⟩ := bind_eq_ok.1 h
        obtain ⟨b1, hb1, h⟩ := bind_eq_ok.1 h
        split at h
        · simp only [ok.injEq, core.result.Result.Ok.injEq, Prod.mk.injEq] at h
          obtain ⟨rfl, rfl⟩ := h
          subst_vars
          exact ⟨hb, hnil, v, hv, ds, hds, fun d hd => (check_reads_ok pinst a v v ds hb1 d hd).elim id id⟩
        · simp at h
      · simp at h
  · rename_i hro
    have hb' : b = false := by simpa using hro
    subst hb'
    left
    exact ⟨hb, (guard_authorized cinst dinst tinst pinst a s c ws r h).2⟩

h5i_when h5i_app_std.authz.guard_viewed =>
/-- What an app's generated theorems apply for a kernel defined as `guard_viewed`. -/
theorem authorized_viewed_of_eq
    (g : A → S → C → Result (core.result.Result (alloc.vec.Vec W × R) (h5i_app_std.authz.Guarded E)))
    (cinst : core.clone.Clone W) [CloneLaw cinst]
    (dinst : h5i_app_std.authz.Disclose R D) (tinst : h5i_app_std.authz.Transition T A S C W R E)
    (pinst : h5i_app_std.authz.Policy P A S W D) (vinst : h5i_app_std.authz.View V A S C)
    (hg : ∀ a s c, g a s c = h5i_app_std.authz.guard_viewed cinst dinst tinst pinst vinst a s c) :
    AuthorizedViewed g tinst pinst dinst vinst ∧ ViewNoninterference g vinst := by
  have : g = h5i_app_std.authz.guard_viewed cinst dinst tinst pinst vinst := by
    funext a s c; exact hg a s c
  subst this
  exact ⟨guard_viewed_authorized cinst dinst tinst pinst vinst,
    guard_viewed_noninterference cinst dinst tinst pinst vinst⟩

h5i_when h5i_app_std.authz.guard_viewed =>
/-- A kernel guarded with `guard_viewed` meets `Kernel::read_only`'s
obligation for the view's `read_only`: the engine may run those commands in
a read-only transaction, without locks or retries. -/
theorem read_only_sound_of_viewed
    (g : A → S → C → Result (core.result.Result (alloc.vec.Vec W × R) (h5i_app_std.authz.Guarded E)))
    (tinst : h5i_app_std.authz.Transition T A S C W R E)
    (pinst : h5i_app_std.authz.Policy P A S W D) (dinst : h5i_app_std.authz.Disclose R D)
    (vinst : h5i_app_std.authz.View V A S C) (h : AuthorizedViewed g tinst pinst dinst vinst) :
    H5iAppLib.ReadOnly.Sound vinst.read_only g (fun ws : alloc.vec.Vec W => ws.val) := by
  intro a s c ws r hro hg
  rcases h a s c ws r hg with ⟨hf, _⟩ | ⟨_, hnil, _⟩
  · rw [hro] at hf; simp at hf
  · exact hnil

end Viewed

/-! ## An app's theorem, in one line

`h5i_authz g` for a kernel function `g` whose extracted body is the guard
applied to the app's instances (`guarded_transition` in the kernel crate):
it reads the instances off `g`'s definition and adds `g.authorized`, proven by
`authorized_of_eq` or `authorized_viewed_of_eq`. The statement it adds is
printed, so the app's report shows what was proven. -/

open Lean Elab Command Meta in
/-- `(guard function, its number of implicit type arguments)` if `e` is a
call of the guard, in any namespace the extraction puts it under. -/
private def guardCall? (e : Expr) : Option (Name × Nat × Array Expr) :=
  match e.getAppFn.constName? with
  | some n =>
    if n.toString.endsWith "h5i_app_std.authz.guard" then some (n, 9, e.getAppArgs)
    else if n.toString.endsWith "h5i_app_std.authz.guard_viewed" then some (n, 10, e.getAppArgs)
    else none
  | none => none

syntax "h5i_authz " ident : command

open Lean Elab Command Meta in
elab_rules : command
  | `(h5i_authz $g) => do
    let gName ← liftCoreM <| realizeGlobalConstNoOverloadWithInfo g
    let some ci := (← getEnv).find? gName | throwError "h5i_authz: {gName} not found"
    let some val := ci.value? | throwError "h5i_authz: {gName} has no definition to read"
    liftTermElabM do
      let some (_, nImpl, args) ← lambdaTelescope val fun _ body => pure (guardCall? body)
        | throwError "h5i_authz: {gName} is not defined as `h5i_app_std::authz::guard` or `guard_viewed` of its instances"
      let gE := mkConst gName (ci.levelParams.map mkLevelParam)
      let viewed := nImpl == 10
      let cinst := args[nImpl]!
      let dinst := args[nImpl + 1]!
      let tinst := args[nImpl + 2]!
      let pinst := args[nImpl + 3]!
      let lem := if viewed then ``authorized_viewed_of_eq else ``authorized_of_eq
      let insts := if viewed then #[some dinst, some tinst, some pinst, some args[nImpl + 4]!]
        else #[some dinst, some tinst, some pinst]
      -- The lemma's implicit types (as many as the guard's), `g`, the clone
      -- instance, its law (synthesized), then the policy's instances.
      let part ← mkAppOptM lem ((Array.replicate nImpl none) ++ #[some gE, some cinst, none] ++ insts)
      let .forallE _ hgTy _ _ ← whnf (← inferType part)
        | throwError "h5i_authz: unexpected type of {lem}"
      let hg ← forallTelescope hgTy fun xs eq => do
        let some (_, lhs, _) := eq.eq? | throwError "h5i_authz: unexpected hypothesis of {lem}"
        mkLambdaFVars xs (← mkEqRefl lhs)
      let pf ← instantiateMVars (mkApp part hg)
      let ty ← instantiateMVars (← inferType pf)
      let thm := gName ++ `authorized
      let tv := TheoremVal.mk (ConstantVal.mk thm ci.levelParams ty) pf [thm]
      addDecl (Declaration.thmDecl tv)
      logInfo m!"{thm} : {ty}"
      if viewed then
        let ro ← mkAppM ``read_only_sound_of_viewed #[gE, tinst, pinst, dinst, args[nImpl + 4]!,
          ← mkAppM ``And.left #[mkConst thm (ci.levelParams.map mkLevelParam)]]
        let roTy ← instantiateMVars (← inferType ro)
        let roThm := gName ++ `read_only_sound
        addDecl (Declaration.thmDecl (TheoremVal.mk (ConstantVal.mk roThm ci.levelParams roTy) ro [roThm]))
        logInfo m!"{roThm} : {roTy}"

end h5i_app_std.Specs.authz
