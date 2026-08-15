import H5iSpec.Landlock

/-!
The phase machine and its theorems (ROADMAP.md §V3, L3): a process under a
Landlock domain, its open fds as capabilities, and the phase transition as
`restrict_self` — pushing a layer onto the domain stack.

The two theorems and the two machine-checked counterexamples:

- `phase_confidentiality`: a secret the INSTALL phase already denies stays
  unreadable through every phase, forever. The proof is an invariant: denial
  persists through restriction (`Domain.deny_persists`), and every fd ever
  opened was justified by a domain that denied the secret.
- `run_deny_insufficient`: the theorem's side condition is real. A run-phase
  ruleset that denies the secret does NOT make the run phase safe: an fd
  opened during install crosses the transition with its rights intact
  (`fd_smuggle`). So "run denies credentials" is enforced by *install*
  denying them, or by the transition closing fds — the design output §V3
  promised.
- `shared_tmp_survives`: narrowing is only ever ⊆. A grant present in every
  layer survives the intersection, so the agent profile's host-shared `/tmp`
  is still reachable after the transition — no fd smuggling required. The
  known footgun, as a `decide`-closed fact instead of prose.
-/

namespace H5iSpec

/-- An open file descriptor: a capability whose rights were fixed at `open`.
No later domain change revisits them — that is Landlock's contract, and it is
why `Step.useFd` below never consults the domain. -/
structure Fd where
  path : FsPath
  access : List Access
deriving Repr, DecidableEq

/-- A confined process: its domain stack and its open fds. -/
structure ProcState where
  domain : Domain
  fds : List Fd
deriving Repr, DecidableEq

/-- What a process can do, at the altitude the phase theorems need:
open a file, use an fd it holds, restrict itself (the phase transition). -/
inductive PhaseAction where
  | openFile (p : FsPath) (a : Access)
  | useFd (fd : Fd) (a : Access)
  | restrict (rs : Ruleset)
deriving Repr

/-- The step relation. `openFile` consults the domain and mints a capability;
`useFd` consults ONLY the capability; `restrict` pushes a layer and touches
nothing else — in particular it does not revoke fds. -/
inductive Step : ProcState → PhaseAction → ProcState → Prop where
  | openFile {s : ProcState} {p : FsPath} {a : Access}
      (h : s.domain.allows p a = true) :
      Step s (.openFile p a) ⟨s.domain, s.fds ++ [⟨p, [a]⟩]⟩
  | useFd {s : ProcState} {fd : Fd} {a : Access}
      (hmem : fd ∈ s.fds) (ha : fd.access.contains a = true) :
      Step s (.useFd fd a) s
  | restrict {s : ProcState} {rs : Ruleset} :
      Step s (.restrict rs) ⟨rs :: s.domain, s.fds⟩

/-- Reachability: zero or more steps. -/
inductive Reachable : ProcState → ProcState → Prop where
  | refl (s : ProcState) : Reachable s s
  | step {s t u : ProcState} {a : PhaseAction}
      (h : Reachable s t) (hs : Step t a u) : Reachable s u

/-- The observation the confidentiality theorems bound: the process can read
`p` right now, either through its domain or through a capability it holds. -/
def CanRead (s : ProcState) (p : FsPath) : Prop :=
  s.domain.allows p .read = true
    ∨ ∃ fd ∈ s.fds, fd.path = p ∧ fd.access.contains .read = true

/-- The invariant: the domain denies reading the secret, and no held fd
carries read on it. (Write-only fds on the secret are permitted — `CanRead`
is about reads, and an fd's rights are exactly what `open` asked for.) -/
def SecretClean (s : ProcState) (secret : FsPath) : Prop :=
  s.domain.allows secret .read = false
    ∧ ∀ fd ∈ s.fds, fd.path = secret → fd.access.contains .read = false

theorem step_preserves_clean {s t : ProcState} {act : PhaseAction}
    {secret : FsPath} (h : Step s act t) (hc : SecretClean s secret) :
    SecretClean t secret := by
  cases h with
  | openFile hall =>
    refine ⟨hc.1, ?_⟩
    intro fd hfd hpath
    rcases List.mem_append.mp hfd with hold | hnew
    · exact hc.2 fd hold hpath
    · rcases List.mem_singleton.mp hnew with rfl
      -- The freshly minted fd: its sole right is what `open` asked for. If
      -- that were `read` on the secret, the domain allowed it — against hc.1.
      rename_i p a
      cases a with
      | read =>
        rw [show p = secret from hpath, hc.1] at hall
        cases hall
      | write =>
        show ([Access.write] : List Access).contains .read = false
        decide
  | useFd _ _ => exact hc
  | restrict =>
    exact ⟨Domain.deny_persists _ _ _ _ hc.1, hc.2⟩

theorem reachable_preserves_clean {s t : ProcState} {secret : FsPath}
    (h : Reachable s t) (hc : SecretClean s secret) :
    SecretClean t secret := by
  induction h with
  | refl => exact hc
  | step _ hs ih => exact step_preserves_clean hs ih

/-- A clean state cannot read the secret. -/
theorem clean_cannot_read {s : ProcState} {secret : FsPath}
    (hc : SecretClean s secret) : ¬ CanRead s secret := by
  rintro (hdom | ⟨fd, hmem, hpath, hread⟩)
  · rw [hc.1] at hdom; cases hdom
  · rw [hc.2 fd hmem hpath] at hread; cases hread

/-- **The conditional phase theorem, positive half**: a secret the install
phase already denies — with no inherited fd on it — stays unreadable in every
reachable state, through any number of phase transitions. The hypothesis is
on the *install* domain: that is the theorem's whole point. -/
theorem phase_confidentiality (install : Ruleset) (secret : FsPath)
    (hinstall : install.allows secret .read = false)
    {s : ProcState} (h : Reachable ⟨[install], []⟩ s) :
    ¬ CanRead s secret := by
  have hclean : SecretClean ⟨[install], []⟩ secret := by
    refine ⟨?_, by intro fd hfd; cases hfd⟩
    simp [Domain.allows, hinstall]
  exact clean_cannot_read (reachable_preserves_clean h hclean)

section Insufficiency

/-! The negative half, on concrete witnesses in the shape of senv's two
phases: install may read HOME (a registry token lives there), run is meant to
be workspace-only. -/

/-- The secret: `~/.aws/credentials`. -/
def secretPath : FsPath := ["home", "user", ".aws", "credentials"]

/-- Install phase: HOME readable (that is why it can fetch the token). -/
def installRules : Ruleset := [⟨["home", "user"], [.read, .write]⟩]

/-- Run phase: workspace only. On paper, the secret is gone. -/
def runRules : Ruleset := [⟨["work"], [.read, .write]⟩]

/-- The smuggling trace: open the secret during install, transition. -/
def smuggledState : ProcState :=
  ⟨[runRules, installRules], [⟨secretPath, [.read]⟩]⟩

theorem smuggle_trace :
    Reachable ⟨[installRules], []⟩ smuggledState :=
  .step (.step (.refl _) (.openFile (by decide))) .restrict

/-- After the transition the DOMAIN denies the secret… -/
theorem run_domain_denies :
    smuggledState.domain.allows secretPath .read = false := by decide

/-- …and the state reads it anyway, through the smuggled fd. -/
theorem smuggled_can_read : CanRead smuggledState secretPath :=
  .inr ⟨⟨secretPath, [.read]⟩, by simp [smuggledState], rfl, by decide⟩

/-- **The conditional phase theorem, negative half**: run-phase denial alone
is not confinement. There is a reachable state whose domain denies the secret
and which reads it regardless — so `phase_confidentiality`'s hypothesis on
the install phase is load-bearing, not proof convenience. -/
theorem run_deny_insufficient :
    ∃ s : ProcState,
      Reachable ⟨[installRules], []⟩ s
        ∧ s.domain.allows secretPath .read = false
        ∧ CanRead s secretPath :=
  ⟨smuggledState, smuggle_trace, run_domain_denies, smuggled_can_read⟩

/-- The agent profile's shape: both phases grant host-shared `/tmp`
(`agent-profile-grants-shared-tmp` is policy, not accident). -/
def installAgent : Ruleset :=
  [⟨["work"], [.read, .write]⟩, ⟨["tmp"], [.read, .write]⟩]

def runAgent : Ruleset :=
  [⟨["work"], [.read, .write]⟩, ⟨["tmp"], [.read, .write]⟩]

/-- **The shared-`/tmp` counterexample**: intersection keeps what every layer
grants, so a host file under `/tmp` is still openable AFTER the transition —
no fd smuggling needed. Narrowing bounds the boundary from above; it never
promises the boundary moved. -/
theorem shared_tmp_survives :
    Domain.allows [runAgent, installAgent]
      ["tmp", "other-agent", "token"] .read = true := by decide

end Insufficiency

end H5iSpec
