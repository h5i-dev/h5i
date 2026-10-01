import Mathlib.Tactic
/-!
# JSON output, specified over byte values

`print` is what the writer must produce. `lexStr` is a standard JSON string
lexer; `lex_esc` says an escaped string always lexes back to itself and ends
at its own closing quote, so its bytes cannot inject JSON.
-/

namespace h5i_app_json.Spec

inductive T where
  | null
  | bool (b : Bool)
  | num (n : Nat)
  | str (s : List Nat)
  | key (s : List Nat)
  | arrOpen
  | arrClose
  | objOpen
  | objClose
deriving DecidableEq

def hexd (d : Nat) : Nat := if d < 10 then 48 + d else 87 + d

def escByte (c : Nat) : List Nat :=
  if c = 34 then [92, 34]
  else if c = 92 then [92, 92]
  else if c < 32 then [92, 117, 48, 48, hexd (c / 16), hexd (c % 16)]
  else [c]

def esc (s : List Nat) : List Nat := s.flatMap escByte

/-- Little-endian decimal digits; at least one. -/
def revDigits (n : Nat) : List Nat :=
  if n < 10 then [n] else n % 10 :: revDigits (n / 10)

def dec (n : Nat) : List Nat := ((revDigits n).map (48 + ·)).reverse

def tokBytes : T → List Nat
  | .null => [110, 117, 108, 108]
  | .bool true => [116, 114, 117, 101]
  | .bool false => [102, 97, 108, 115, 101]
  | .num n => dec n
  | .str s => 34 :: esc s ++ [34]
  | .key s => 34 :: esc s ++ [34, 58]
  | .arrOpen => [91]
  | .arrClose => [93]
  | .objOpen => [123]
  | .objClose => [125]

def endsValue : T → Bool
  | .null | .bool _ | .num _ | .str _ | .arrClose | .objClose => true
  | _ => false

def isClose : T → Bool
  | .arrClose | .objClose => true
  | _ => false

/-- One token: comma if it follows a value and does not close. -/
def step (st : List Nat × Bool) (t : T) : List Nat × Bool :=
  (st.1 ++ (if st.2 && !isClose t then [44] else []) ++ tokBytes t, endsValue t)

def print (ts : List T) : List Nat := (ts.foldl step ([], false)).1

/-! ## Reading strings back -/

def unhex (c : Nat) : Option Nat :=
  if 48 ≤ c ∧ c ≤ 57 then some (c - 48)
  else if 97 ≤ c ∧ c ≤ 102 then some (c - 87)
  else if 65 ≤ c ∧ c ≤ 70 then some (c - 55)
  else none

/-- JSON string lexer from after the opening quote. `\uXXXX` limited to ASCII (we only emit `\u00XX`). -/
def lexStr : List Nat → Option (List Nat × List Nat)
  | [] => none
  | c :: r =>
    if c = 34 then some ([], r)
    else if c = 92 then
      match r with
      | e :: r' =>
        let short : Option Nat :=
          if e = 34 then some 34 else if e = 92 then some 92 else if e = 47 then some 47
          else if e = 98 then some 8 else if e = 102 then some 12 else if e = 110 then some 10
          else if e = 114 then some 13 else if e = 116 then some 9 else none
        match short with
        | some v => (lexStr r').map fun p => (v :: p.1, p.2)
        | none =>
          if e = 117 then
            match r' with
            | a :: b :: x :: y :: r'' =>
              match unhex a, unhex b, unhex x, unhex y with
              | some a, some b, some x, some y =>
                let v := ((a * 16 + b) * 16 + x) * 16 + y
                if v < 128 then (lexStr r'').map fun p => (v :: p.1, p.2) else none
              | _, _, _, _ => none
            | _ => none
          else none
      | [] => none
    else if c < 32 then none
    else (lexStr r).map fun p => (c :: p.1, p.2)
termination_by l => l.length

theorem unhex_hexd (d : Nat) (h : d < 16) : unhex (hexd d) = some d := by
  unfold unhex hexd
  split_ifs <;> first | omega | (simp; omega) | simp

theorem lex_esc (s rest : List Nat) (hs : ∀ c ∈ s, c < 256) :
    lexStr (esc s ++ 34 :: rest) = some (s, rest) := by
  induction s with
  | nil => rw [esc, List.flatMap_nil, List.nil_append, lexStr.eq_def]; simp
  | cons c s ih =>
    have hc : c < 256 := hs c (by simp)
    have ih := ih (fun x hx => hs x (by simp [hx]))
    simp only [esc, List.flatMap_cons, List.append_assoc] at ih ⊢
    by_cases h34 : c = 34
    · subst h34
      rw [show escByte 34 = [92, 34] by simp [escByte]]
      rw [List.cons_append, lexStr.eq_def]; simp [ih]
    by_cases h92 : c = 92
    · subst h92
      rw [show escByte 92 = [92, 92] by simp [escByte]]
      rw [List.cons_append, lexStr.eq_def]; simp [ih]
    by_cases h32 : c < 32
    · have h1 : c / 16 < 16 := by omega
      have h2 : c % 16 < 16 := by omega
      rw [show escByte c = [92, 117, 48, 48, hexd (c / 16), hexd (c % 16)] by simp [escByte, h34, h92, h32]]
      rw [List.cons_append, lexStr.eq_def]
      have u0 : unhex 48 = some 0 := rfl
      simp [u0, unhex_hexd _ h1, unhex_hexd _ h2, ih]
      omega
    · rw [show escByte c = [c] by simp [escByte, h34, h92, h32]]
      rw [List.cons_append, lexStr.eq_def]
      simp [h34, h92, h32, ih]

/-- Escaped text has no raw control bytes. -/
theorem esc_no_control (s : List Nat) : ∀ c ∈ esc s, 32 ≤ c := by
  intro c hc
  simp only [esc, List.mem_flatMap] at hc
  obtain ⟨x, -, hx⟩ := hc
  unfold escByte hexd at hx
  split_ifs at hx <;> simp at hx <;> omega

end h5i_app_json.Spec
