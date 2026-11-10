/-!
# What a valid request is, independently of the parser

A declarative description of the requests `h5i-app-h1` may accept: the
RFC 9112 / RFC 9110 / RFC 3986 grammar for the parts it supports, plus the
h5i policy (one `Host` on HTTP/1.1, at most one `Content-Length` and one
`Transfer-Encoding`, never both, and so on), the framing RFC 9112 §6.3
gives a request, and chunked bodies (RFC 9112 §7.1, with h5i's limits).

This file imports nothing: its byte classes, its ASCII lowercasing
(`Char.toLower`), its literals (from string literals) and its decimal and
hex reading are its own, so a misreading the parser shares with `H1Spec`
does not carry over. `H1Sound.lean` proves that every head `parse_head`
accepts is a `ValidHead`, with the framing `framing` computes;
`H1Chunked.lean` that every chunked body the decoder accepts is a
`ValidChunked`, with its payload.
-/

namespace H1Valid

/-- The bytes of an ASCII string. -/
def ascii (s : String) : List Nat := s.toList.map Char.toNat

/-! ## Byte classes (RFC 5234 B.1, RFC 9110 §5.5 and §5.6.2, RFC 3986 §2) -/

def isAlpha (b : Nat) : Bool := (65 ≤ b && b ≤ 90) || (97 ≤ b && b ≤ 122)
def isDigit (b : Nat) : Bool := 48 ≤ b && b ≤ 57
def isHexDig (b : Nat) : Bool := isDigit b || (ascii "ABCDEFabcdef").contains b
def isVchar (b : Nat) : Bool := 33 ≤ b && b ≤ 126
def isObsText (b : Nat) : Bool := 128 ≤ b && b ≤ 255
def isWs (b : Nat) : Bool := b == 32 || b == 9
/-- `tchar`. -/
def tchar (b : Nat) : Bool := isAlpha b || isDigit b || (ascii "!#$%&'*+-.^_`|~").contains b
/-- `field-vchar`. -/
def fieldVchar (b : Nat) : Bool := isVchar b || isObsText b
def unreserved (b : Nat) : Bool := isAlpha b || isDigit b || (ascii "-._~").contains b
def subDelims (b : Nat) : Bool := (ascii "!$&'()*+,;=").contains b
/-- `pchar` without `pct-encoded`. -/
def pcharByte (b : Nat) : Bool := unreserved b || subDelims b || b == 58 || b == 64

/-- ASCII lowercase. -/
def lc (b : Nat) : Nat := (Char.ofNat b).toLower.toNat

/-- `s` spells `lit`, ignoring ASCII case. -/
def named (s : List Nat) (lit : String) : Bool := s.map lc == ascii lit

/-! ## Targets and `Host` values (RFC 3986 §3.2.2, §3.3, §3.4; RFC 9110 §7.2) -/

/-- A sequence of `ok` bytes and `pct-encoded` (`%` HEXDIG HEXDIG). -/
def pctRun (ok : Nat → Bool) : List Nat → Bool
  | [] => true
  | c :: r =>
    if c = 37 then
      2 ≤ r.length && isHexDig (r.headD 0) && isHexDig ((r.drop 1).headD 0) && pctRun ok (r.drop 2)
    else ok c && pctRun ok r
termination_by l => l.length

@[simp] theorem pctRun_nil (ok : Nat → Bool) : pctRun ok [] = true := by rw [pctRun]

/-- `absolute-path = 1*( "/" segment )`, `segment = *pchar`. -/
def absolutePath (p : List Nat) : Prop :=
  ∃ rest, p = 47 :: rest ∧ pctRun (fun b => pcharByte b || b == 47) rest = true

/-- `query = *( pchar / "/" / "?" )`. -/
def query (q : List Nat) : Prop := pctRun (fun b => pcharByte b || b == 47 || b == 63) q = true

/-- `origin-form = absolute-path [ "?" query ]` (RFC 9112 §3.2.1). -/
def originForm (t : List Nat) : Prop :=
  ∃ p q, t = p ++ q ∧ absolutePath p ∧ (q = [] ∨ ∃ q', q = 63 :: q' ∧ query q')

/-- `reg-name = *( unreserved / pct-encoded / sub-delims )`; it includes
`IPv4address`. -/
def regName (h : List Nat) : Prop := pctRun (fun b => unreserved b || subDelims b) h = true

/-- `[` IP-literal `]`, with the IP-literal's bytes from `IPv6address` and
`IPvFuture`: `unreserved`, `sub-delims`, `:`. -/
def ipLiteral (h : List Nat) : Prop :=
  ∃ x, h = 91 :: (x ++ [93]) ∧ x ≠ [] ∧ x.all (fun b => unreserved b || subDelims b || b == 58) = true

/-- `Host = uri-host [ ":" port ]`, `port = *DIGIT`; the empty value is an
empty `reg-name`. -/
def hostValue (v : List Nat) : Prop :=
  ∃ h p, v = h ++ p ∧ (regName h ∨ ipLiteral h) ∧ (p = [] ∨ ∃ ds, p = 58 :: ds ∧ ds.all isDigit = true)

/-! ## The head (RFC 9112 §2.1, §3, §5) -/

def crlf : List Nat := [13, 10]

/-- A request line: `method SP request-target SP HTTP/1.minor`. -/
structure RequestLine where
  method : List Nat
  target : List Nat
  minor : Nat

def RequestLine.bytes (r : RequestLine) : List Nat :=
  r.method ++ [32] ++ r.target ++ [32] ++ ascii "HTTP/1." ++ [48 + r.minor]

/-- A field line: `field-name ":" OWS field-value OWS`. -/
structure FieldLine where
  name : List Nat
  ows1 : List Nat
  value : List Nat
  ows2 : List Nat

def FieldLine.bytes (f : FieldLine) : List Nat := f.name ++ [58] ++ f.ows1 ++ f.value ++ f.ows2

/-- The bytes of a head: the request line, the field lines, each ended by
CRLF, and an empty line. -/
def headBytes (r : RequestLine) (fs : List FieldLine) : List Nat :=
  r.bytes ++ crlf ++ (fs.map (fun f => f.bytes ++ crlf)).flatten ++ crlf

/-- `field-value = *field-content`, `field-content = field-vchar
[ 1*( SP / HTAB / field-vchar ) field-vchar ]`. -/
def fieldValue (v : List Nat) : Prop :=
  v.all (fun b => fieldVchar b || isWs b) = true ∧
  (v = [] ∨ (∃ a, v.head? = some a ∧ fieldVchar a = true) ∧ (∃ z, v.getLast? = some z ∧ fieldVchar z = true))

def FieldLine.ok (f : FieldLine) : Prop :=
  f.name ≠ [] ∧ f.name.all tchar = true ∧ f.ows1.all isWs = true ∧ fieldValue f.value ∧ f.ows2.all isWs = true

/-! ## Framing (RFC 9112 §6.3) -/

def isHost (f : FieldLine) : Bool := named f.name "host"
def isContentLength (f : FieldLine) : Bool := named f.name "content-length"
def isTransferEncoding (f : FieldLine) : Bool := named f.name "transfer-encoding"

/-- A decimal number's value. -/
def decimal (ds : List Nat) : Nat := ds.foldl (fun a d => 10 * a + (d - 48)) 0

inductive Framing where
  | empty
  | length (n : Nat)
  | chunked
  deriving DecidableEq, Repr

/-- How a request's body is framed: `Transfer-Encoding` overrides
`Content-Length`; a `Content-Length` gives the length; otherwise there is no
body. -/
def framing (fs : List FieldLine) : Framing :=
  if fs.any isTransferEncoding then .chunked
  else match fs.find? isContentLength with
    | some f => .length (decimal f.value)
    | none => .empty

/-! ## The heads h5i accepts -/

/-- `H` is the head `r`, `fs`, and it is one h5i accepts with limits `mh`
(head bytes) and `mb` (body bytes). -/
def ValidHead (mh mb : Nat) (H : List Nat) (r : RequestLine) (fs : List FieldLine) : Prop :=
  H = headBytes r fs ∧ H.length ≤ mh ∧
  -- request line
  r.method ≠ [] ∧ r.method.all tchar = true ∧ originForm r.target ∧ (r.minor = 0 ∨ r.minor = 1) ∧
  -- field lines
  (∀ f ∈ fs, f.ok) ∧ fs.length ≤ 16 ∧
  -- Host: one on HTTP/1.1, at most one, a valid value
  (fs.filter isHost).length ≤ 1 ∧ (r.minor = 1 → (fs.filter isHost).length = 1) ∧
  (∀ f ∈ fs, isHost f = true → hostValue f.value) ∧
  -- Content-Length: at most one, digits, at most `mb`, no trailing whitespace
  (fs.filter isContentLength).length ≤ 1 ∧
  (∀ f ∈ fs, isContentLength f = true →
    f.value ≠ [] ∧ f.value.all isDigit = true ∧ decimal f.value ≤ mb ∧ f.ows2 = []) ∧
  -- Transfer-Encoding: at most one, exactly `chunked`, HTTP/1.1 only, no trailing whitespace
  (fs.filter isTransferEncoding).length ≤ 1 ∧
  (∀ f ∈ fs, isTransferEncoding f = true → named f.value "chunked" = true ∧ r.minor = 1 ∧ f.ows2 = []) ∧
  -- never both
  ¬ (fs.any isContentLength = true ∧ fs.any isTransferEncoding = true)

/-! ## Chunked bodies (RFC 9112 §7.1) -/

/-- A hex digit's value: its place in `0123456789abcdef`, case ignored. -/
def hexDigitValue (b : Nat) : Nat := (ascii "0123456789abcdef").idxOf (lc b)

/-- A hex numeral's value. -/
def hexNumber (ds : List Nat) : Nat := ds.foldl (fun a d => 16 * a + hexDigitValue d) 0

/-- `chunk-size = 1*HEXDIG`, at most 8 digits (h5i). -/
def chunkSize (ds : List Nat) : Prop := ds ≠ [] ∧ ds.length ≤ 8 ∧ ds.all isHexDig = true

structure Chunk where
  size : List Nat
  data : List Nat

/-- `chunk = chunk-size CRLF chunk-data CRLF`, without `chunk-ext` (h5i). -/
def Chunk.bytes (c : Chunk) : List Nat := c.size ++ crlf ++ c.data ++ crlf

/-- `chunk-data = 1*OCTET`, as many as the size says. -/
def Chunk.ok (c : Chunk) : Prop := chunkSize c.size ∧ c.data ≠ [] ∧ c.data.length = hexNumber c.size

/-- `chunked-body = *chunk last-chunk trailer-section CRLF`, with
`last-chunk = 1*("0") CRLF` (`z` the zeros) and no trailer fields (h5i). -/
def chunkedBytes (cs : List Chunk) (z : List Nat) : List Nat :=
  (cs.map Chunk.bytes).flatten ++ z ++ crlf ++ crlf

/-- `W` is a chunked body h5i accepts with at most `mb` payload bytes, and
`P` is its payload: the chunk data in order. Its wire length is `W.length`. -/
def ValidChunked (mb : Nat) (W P : List Nat) : Prop :=
  ∃ cs z, W = chunkedBytes cs z ∧ (∀ c ∈ cs, c.ok) ∧ chunkSize z ∧ z.all (· == 48) = true ∧
    P = (cs.map Chunk.data).flatten ∧ P.length ≤ mb

/-! ## `origin-form` as one run -/

theorem pctRun_cons_ne {ok : Nat → Bool} {c : Nat} {r : List Nat} (h : c ≠ 37) :
    pctRun ok (c :: r) = (ok c && pctRun ok r) := by
  rw [pctRun, if_neg h]

theorem pctRun_pct {ok : Nat → Bool} {a b : Nat} {r : List Nat} :
    pctRun ok (37 :: a :: b :: r) = (isHexDig a && isHexDig b && pctRun ok r) := by
  rw [pctRun, if_pos rfl]; simp

theorem pctRun_mono {ok ok' : Nat → Bool} (h : ∀ b, ok b = true → ok' b = true) :
    ∀ l, pctRun ok l = true → pctRun ok' l = true
  | [] => fun _ => pctRun_nil _
  | c :: r => by
    intro hr
    by_cases hc : c = 37
    · subst hc
      match r, hr with
      | a :: b :: r', hr =>
        rw [pctRun_pct] at hr ⊢
        simp only [Bool.and_eq_true] at hr ⊢
        exact ⟨hr.1, pctRun_mono h r' hr.2⟩
      | [], hr => rw [pctRun, if_pos rfl] at hr; simp at hr
      | [_], hr => rw [pctRun, if_pos rfl] at hr; simp at hr
    · rw [pctRun_cons_ne hc] at hr ⊢
      simp only [Bool.and_eq_true] at hr ⊢
      exact ⟨h c hr.1, pctRun_mono h r hr.2⟩

theorem hexDig_ne_63 {a : Nat} (h : isHexDig a = true) : a ≠ 63 := by
  intro e; subst e; revert h; decide

/-- A run of `pchar`, `/`, `?` splits at its first `?` into a path part
without `?` and the rest. -/
theorem run_split :
    ∀ l, pctRun (fun b => pcharByte b || b == 47 || b == 63) l = true →
      ∃ p q, l = p ++ q ∧ pctRun (fun b => pcharByte b || b == 47) p = true ∧
        (q = [] ∨ ∃ q', q = 63 :: q' ∧ query q')
  | [] => fun _ => ⟨[], [], rfl, pctRun_nil _, Or.inl rfl⟩
  | c :: r => by
    intro hr
    by_cases hc : c = 37
    · subst hc
      match r, hr with
      | a :: b :: r', hr =>
        rw [pctRun_pct] at hr
        simp only [Bool.and_eq_true] at hr
        obtain ⟨p, q, rfl, hp, hq⟩ := run_split r' hr.2
        refine ⟨37 :: a :: b :: p, q, rfl, ?_, hq⟩
        rw [pctRun_pct]; simp only [Bool.and_eq_true]; exact ⟨hr.1, hp⟩
      | [], hr => rw [pctRun, if_pos rfl] at hr; simp at hr
      | [_], hr => rw [pctRun, if_pos rfl] at hr; simp at hr
    · rw [pctRun_cons_ne hc] at hr
      simp only [Bool.and_eq_true] at hr
      by_cases hq : c = 63
      · subst hq
        exact ⟨[], 63 :: r, rfl, pctRun_nil _, Or.inr ⟨r, rfl, hr.2⟩⟩
      · obtain ⟨p, q, rfl, hp, hq'⟩ := run_split r hr.2
        refine ⟨c :: p, q, rfl, ?_, hq'⟩
        rw [pctRun_cons_ne hc]
        have : (pcharByte c || c == 47) = true := by
          have h1 := hr.1; simp only [Bool.or_eq_true, beq_iff_eq] at h1 ⊢
          rcases h1 with (h | h) | h
          · exact Or.inl h
          · exact Or.inr h
          · exact absurd h hq
        simp [this, hp]

/-- So `/` followed by such a run is an `origin-form`. -/
theorem originForm_of_run {rest : List Nat}
    (h : pctRun (fun b => pcharByte b || b == 47 || b == 63) rest = true) : originForm (47 :: rest) := by
  obtain ⟨p, q, rfl, hp, hq⟩ := run_split rest h
  exact ⟨47 :: p, q, rfl, ⟨p, rfl, hp⟩, hq⟩

end H1Valid
