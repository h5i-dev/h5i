import H5iSpec.Refinement

/-!
The Seatbelt refinement (ROADMAP.md §V3's "each backend gets its own"): a
model of the **file-access fragment** of the SBPL profile
`seatbelt::build_profile` generates, in the generator's exact rule order —
because in SBPL, order is the semantics: the profile is `(deny default)`
plus rules where the **last** matching rule wins. That is the opposite
regime from Landlock (denies exist here, and the generator banks on it by
emitting every deny after every allow), which is what makes this refinement
worth its own file rather than a parameter on the Landlock one.

Binding to the Rust: the generator is pure and compiles on Linux, so
`tests/seatbelt_drt.rs` runs it, parses the file rules out of the generated
SBPL text, and diffs them against this model's emission
(`seatbeltFileRules`, via the binary's `--seatbelt` mode) — structural
equality, rule for rule, path for path. The platform constant lists are
mirrored here on purpose: when the Rust lists change, the DRT fails until
this file follows.

Scope, named: file rules only. The network, mach, sysctl, signal and ioctl
sections are structurally excluded (the harness filters to rules whose
operations are exactly `file-read*`/`file-write*` sets); the interactive
tty rules are mirrored in the emission but their `regex` filters are
outside the denotation (`sbplAllows` never matches them); and
`macos_developer_reads` and `config_lock_paths` are host-measured inputs,
supplied by the harness the same way the DRT world is.
-/

namespace H5iSpec

/-- Mirror of Rust `MACOS_SYSTEM_READ`. -/
def macosSystemRead : List String := [
  "/System", "/usr/lib", "/usr/share", "/usr/bin", "/usr/sbin", "/bin",
  "/sbin", "/Library/Apple", "/Library/Frameworks",
  "/Library/Preferences/.GlobalPreferences.plist",
  "/Library/Preferences/com.apple.dt.Xcode.plist",
  "/private/var/db/dyld", "/private/var/db/timezone", "/private/var/select",
  "/private/etc/localtime", "/private/etc/protocols",
  "/private/etc/services", "/private/etc/ssl", "/private/etc/hosts", "/private/etc/passwd",
  "/private/etc/group", "/private/etc/resolv.conf"]

/-- Mirror of Rust `MACOS_DEV_NODES`. -/
def macosDevNodes : List String := [
  "/dev/null", "/dev/zero", "/dev/random", "/dev/urandom",
  "/dev/dtracehelper", "/dev/tty", "/dev/fd", "/dev/stdin", "/dev/stdout",
  "/dev/stderr", "/dev/ptmx"]

/-- Mirror of Rust `MACOS_DEV_WRITE`. -/
def macosDevWrite : List String := [
  "/dev/null", "/dev/zero", "/dev/tty", "/dev/stdout", "/dev/stderr",
  "/dev/fd", "/dev/ptmx", "/dev/dtracehelper"]

/-- Mirror of Rust `path_aliases`: Seatbelt matches **resolved** paths, and
the macOS root firmlinks `/tmp`, `/var`, `/etc` under `/private`, so every
rule names both spellings. -/
def pathAliases (path : String) : List String :=
  let prefixed :=
    ["/tmp", "/var", "/etc"].filterMap fun pre =>
      if path == pre || path.startsWith (pre ++ "/") then
        some ("/private" ++ path)
      else none
  let unprefixed :=
    match path.dropPrefix? "/private" with
    | some rest =>
      let rest := rest.toString
      if ["/tmp", "/var", "/etc"].any fun pre =>
          rest == pre || rest.startsWith (pre ++ "/") then
        [rest]
      else []
    | none => []
  path :: prefixed ++ unprefixed

/-- Mirror of Rust `expand_home`: `~` needs a home, `$`-prefixed entries are
repo-scoped lints and never become SBPL rules. -/
def expandHomeSbpl (home : Option String) (path : String) : Option String :=
  if path.startsWith "~/" then
    home.map fun h => h ++ path.drop 1
  else if path == "~" then
    home
  else if path.startsWith "$" then
    none
  else
    some path

/-- Adjacent dedup, for a sorted list. -/
def dedupSorted : List String → List String
  | [] => []
  | a :: t =>
    match dedupSorted t with
    | [] => [a]
    | b :: t' => if a == b then b :: t' else a :: b :: t'

/-- Sorted, deduplicated — Rust's `BTreeSet` iteration order, which is what
makes the emission canonical and the DRT comparison order-stable. -/
def sortedDedup (l : List String) : List String :=
  dedupSorted (l.mergeSort (· ≤ ·))

theorem mem_dedupSorted_of_mem {a : String} :
    ∀ {l : List String}, a ∈ l → a ∈ dedupSorted l := by
  intro l
  induction l with
  | nil => intro h; cases h
  | cons b t ih =>
    intro h
    unfold dedupSorted
    rcases List.mem_cons.mp h with rfl | ht
    · split
      · exact List.mem_singleton.mpr rfl
      · rename_i c t' _
        split
        · rename_i heq
          exact (beq_iff_eq.mp heq) ▸ List.mem_cons_self ..
        · exact List.mem_cons_self ..
    · have := ih ht
      split
      · rename_i hnil; rw [hnil] at this; cases this
      · rename_i c t' heq
        rw [heq] at this
        split
        · exact this
        · exact List.mem_cons_of_mem _ this

theorem mem_sortedDedup {a : String} {l : List String} (h : a ∈ l) :
    a ∈ sortedDedup l :=
  mem_dedupSorted_of_mem
    ((List.mergeSort_perm l _).symm.mem_iff.mp h)

inductive SbplDecision where
  | allow
  | deny
deriving Repr, DecidableEq

inductive SbplFilterKind where
  | subpath
  | literal
  | regex
deriving Repr, DecidableEq

/-- One modeled SBPL rule: a decision over operation names, with one filter
kind and its (alias-expanded, sorted) paths — the exact shape Rust's
`rule()` emits. -/
structure SbplRule where
  decision : SbplDecision
  ops : List String
  kind : SbplFilterKind
  paths : List String
deriving Repr, DecidableEq

/-- Rust `rule()`: alias-expand, dedup, sort; emit nothing when empty. -/
def mkRule (decision : SbplDecision) (ops : List String)
    (kind : SbplFilterKind) (paths : List String) : List SbplRule :=
  if (sortedDedup (paths.flatMap pathAliases)).isEmpty then []
  else
    [{ decision := decision, ops := ops, kind := kind,
       paths := sortedDedup (paths.flatMap pathAliases) }]

/-- Whatever `mkRule` emits carries its decision. -/
theorem mem_mkRule_decision {r : SbplRule} {d : SbplDecision}
    {ops : List String} {kind : SbplFilterKind} {paths : List String}
    (h : r ∈ mkRule d ops kind paths) : r.decision = d := by
  unfold mkRule at h
  split at h
  · cases h
  · rcases List.mem_singleton.mp h with rfl; rfl

/-- The file-access inputs of `build_profile`, with the host-measured parts
(developer reads, config-lock paths) supplied by the harness. -/
structure SeatbeltInput where
  fs_read : List String
  fs_write : List String
  fs_deny : List String
  work : String
  work_readonly : Bool
  home : Option String
  interactive : Bool
  ro_backings : List String
  cache_write_backing : Option String
  capture_spool : Option String
  /-- `home_binds` targets other than `/tmp` — the real HOME paths the
  per-env copies shadow, which the profile must make unreachable. -/
  shadowed_home : List String
  /-- Host-measured: `macos_developer_reads()`. -/
  developer_reads : List String
  /-- Host-measured: `config_lock_paths(work, home)`. -/
  config_locks : List String
deriving Repr, Lean.FromJson

/-- The tty rules an interactive session gets — mirrored for emission,
outside the denotation (regex filters). -/
def ttyRules : List SbplRule :=
  [{ decision := .allow, ops := ["file-write*", "file-read*"],
     kind := .regex, paths := ["^/dev/tty[a-z0-9]*$"] },
   { decision := .allow, ops := ["file-write*", "file-read*"],
     kind := .regex, paths := ["^/dev/pty[a-z0-9]*$"] }]

/-- The allow section of the file fragment, in `build_profile`'s order. -/
def seatbeltAllowRules (i : SeatbeltInput) : List SbplRule :=
  let reads :=
    macosSystemRead ++ i.developer_reads
      ++ i.fs_read.filterMap (expandHomeSbpl i.home) ++ [i.work]
  let writes :=
    (if i.work_readonly then [] else [i.work])
      ++ (i.fs_write.filter (· != "$WORK")).filterMap (expandHomeSbpl i.home)
      ++ (match i.cache_write_backing with | some b => [b] | none => [])
      ++ (match i.capture_spool with | some s => [s] | none => [])
  mkRule .allow ["file-read*"] .subpath reads
    ++ [{ decision := .allow, ops := ["file-read*"], kind := .literal,
          paths := ["/"] }]
    ++ mkRule .allow ["file-read*"] .literal macosDevNodes
    ++ mkRule .allow ["file-read*"] .subpath i.ro_backings
    ++ mkRule .allow ["file-write*", "file-read*"] .subpath writes
    ++ mkRule .allow ["file-write*", "file-read*"] .literal macosDevWrite
    ++ (if i.interactive then ttyRules else [])

/-- The deny tail — last, because last match wins and these must win. -/
def seatbeltDenyRules (i : SeatbeltInput) : List SbplRule :=
  mkRule .deny ["file-read*", "file-write*"] .subpath
      (i.fs_deny.filterMap (expandHomeSbpl i.home))
    ++ (if i.interactive then
          mkRule .deny ["file-write*"] .subpath i.config_locks
        else [])
    ++ mkRule .deny ["file-read*", "file-write*"] .subpath i.shadowed_home

/-- The whole file fragment: allows, then denies. -/
def seatbeltFileRules (i : SeatbeltInput) : List SbplRule :=
  seatbeltAllowRules i ++ seatbeltDenyRules i

/-- The operation name an [`Access`] queries. -/
def opName : Access → String
  | .read => "file-read*"
  | .write => "file-write*"

/-- Does one rule match a path and access? `regex` filters never match in
the denotation — named exclusion, they exist for emission fidelity only. -/
def SbplRule.matches (r : SbplRule) (p : FsPath) (a : Access) : Bool :=
  r.ops.contains (opName a)
    && match r.kind with
       | .subpath => r.paths.any fun g => p.beneath (parsePath g)
       | .literal => r.paths.any fun g => p == parsePath g
       | .regex => false

/-- SBPL: `(deny default)`, last match wins. -/
def sbplAllows (rules : List SbplRule) (p : FsPath) (a : Access) : Bool :=
  match rules.foldl
      (fun acc r => if r.matches p a then some r.decision else acc) none with
  | some SbplDecision.allow => true
  | some SbplDecision.deny => false
  | none => false

/-- If some rule in the tail matches and every tail rule is a deny, the
verdict is deny — the generic form of "the deny section wins". -/
theorem deny_tail_wins (pre post : List SbplRule) (p : FsPath) (a : Access)
    (hmatch : ∃ r ∈ post, r.matches p a = true)
    (hdeny : ∀ r ∈ post, r.decision = .deny) :
    sbplAllows (pre ++ post) p a = false := by
  unfold sbplAllows
  rw [List.foldl_append]
  -- Whatever the prefix folded to, the matching deny in the tail overwrites
  -- it, and later tail rules can only overwrite with more denies.
  have tail :
      ∀ (rules : List SbplRule) (acc : Option SbplDecision),
        (∀ r ∈ rules, r.decision = .deny) →
        (acc = some .deny ∨ ∃ r ∈ rules, r.matches p a = true) →
        List.foldl
          (fun acc r => if r.matches p a then some r.decision else acc)
          acc rules = some .deny := by
    intro rules
    induction rules with
    | nil =>
      rintro acc _ (hacc | ⟨r, hr, _⟩)
      · exact hacc
      · cases hr
    | cons r rest ih =>
      rintro acc hd hstate
      simp only [List.foldl_cons]
      by_cases hm : r.matches p a = true
      · rw [if_pos hm, hd r (List.mem_cons_self ..)]
        exact ih _ (fun x hx => hd x (List.mem_cons_of_mem _ hx))
          (Or.inl rfl)
      · rw [if_neg hm]
        refine ih _ (fun x hx => hd x (List.mem_cons_of_mem _ hx)) ?_
        rcases hstate with hacc | ⟨s, hs, hsm⟩
        · exact Or.inl hacc
        · rcases List.mem_cons.mp hs with rfl | hs'
          · exact absurd hsm hm
          · exact Or.inr ⟨s, hs', hsm⟩
  rw [tail post _ hdeny (Or.inr hmatch)]

/-- Every rule the deny section emits is a deny. -/
theorem deny_rules_are_denies (i : SeatbeltInput) :
    ∀ r ∈ seatbeltDenyRules i, r.decision = .deny := by
  intro r hr
  simp only [seatbeltDenyRules, List.mem_append] at hr
  rcases hr with (h1 | h2) | h3
  · exact mem_mkRule_decision h1
  · split at h2
    · exact mem_mkRule_decision h2
    · cases h2
  · exact mem_mkRule_decision h3

/-- **`fs.deny` is genuinely enforced on Seatbelt** (on Linux it is only a
resolution lint): a path beneath an expanded deny entry is denied — for
reads AND writes, whatever the grant sections said. The Landlock chapter's
`fs_deny` honesty note, with the polarity flipped and proved. -/
theorem fs_deny_wins (i : SeatbeltInput) (p : FsPath) (a : Access)
    (d : String) (hd : d ∈ i.fs_deny.filterMap (expandHomeSbpl i.home))
    (hbeneath : p.beneath (parsePath d) = true) :
    sbplAllows (seatbeltFileRules i) p a = false := by
  have hin : d ∈ sortedDedup
      ((i.fs_deny.filterMap (expandHomeSbpl i.home)).flatMap pathAliases) :=
    mem_sortedDedup
      (List.mem_flatMap.mpr ⟨d, hd, by simp [pathAliases]⟩)
  have hne : (sortedDedup
      ((i.fs_deny.filterMap (expandHomeSbpl i.home)).flatMap
        pathAliases)).isEmpty = false := by
    cases hexp : sortedDedup
        ((i.fs_deny.filterMap (expandHomeSbpl i.home)).flatMap pathAliases) with
    | nil => rw [hexp] at hin; cases hin
    | cons x xs => rfl
  apply deny_tail_wins
  · refine ⟨{ decision := .deny, ops := ["file-read*", "file-write*"],
              kind := .subpath,
              paths := sortedDedup
                ((i.fs_deny.filterMap (expandHomeSbpl i.home)).flatMap
                  pathAliases) }, ?_, ?_⟩
    · simp only [seatbeltDenyRules, List.mem_append]
      left; left
      simp [mkRule, hne]
    · unfold SbplRule.matches
      refine (Bool.and_eq_true ..).mpr ⟨?_, ?_⟩
      · cases a <;> simp [opName] <;> decide
      · exact List.any_eq_true.mpr ⟨d, hin, hbeneath⟩
  · exact deny_rules_are_denies i

end H5iSpec
