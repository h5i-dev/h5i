/-!
# The HTTP/1.1 subset `h5i-app-h1` accepts

Pure Lean over byte values (`Nat`). The extracted Rust computes these
functions (`H1Proofs.lean`); what they accept is characterized in
`H1Props.lean` (well-formedness, the read bound) and `H1Smuggle.lean` (no
lenient parser in the `Interp` family sees another message boundary).

The head parser reads the request through a byte accessor `g : Nat → Nat`.
`parseHead` builds `g` from the input with every byte at or beyond the read
limit masked to 0, so the result depends only on the bytes below the limit.
-/

namespace h5i_app_h1.Spec

/-! ## Byte classes -/

/-- `tchar` (RFC 9110 §5.6.2), by range as in the Rust. -/
def isTchar (b : Nat) : Bool :=
  if b < 33 then false
  else if b < 48 then b != 34 && b != 40 && b != 41 && b != 44 && b != 47
  else if b < 58 then true
  else if b < 65 then false
  else if b < 91 then true
  else if b < 94 then false
  else if b < 123 then true
  else b == 124 || b == 126

/-- VCHAR, obs-text, SP or HTAB: a byte that may appear in a field value. -/
def isFieldByte (b : Nat) : Bool := b == 9 || (32 ≤ b && b != 127)

/-- Visible ASCII: a byte of an origin-form target. -/
def isTargetByte (b : Nat) : Bool := 33 ≤ b && b ≤ 126

def isOws (b : Nat) : Bool := b == 32 || b == 9

/-- Not CR, not LF. -/
def notEol (b : Nat) : Bool := !(b == 13 || b == 10)

/-- ASCII lowercase. -/
def lower (b : Nat) : Nat := if 65 ≤ b ∧ b ≤ 90 then b + 32 else b

/-! ## Scanning -/

/-- The first `k` in `[i, e)` with `¬ p (g k)`; `e` if there is none
(`i` if `e ≤ i`). -/
def scan (p : Nat → Bool) (g : Nat → Nat) (i e : Nat) : Nat :=
  if i < e then (if p (g i) then scan p g (i + 1) e else i) else i
termination_by e - i

theorem scan_ge (p : Nat → Bool) (g : Nat → Nat) (i e : Nat) : i ≤ scan p g i e := by
  rw [scan]
  split
  · split
    · have := scan_ge p g (i + 1) e; omega
    · omega
  · omega
termination_by e - i

theorem scan_le (p : Nat → Bool) (g : Nat → Nat) (i e : Nat) (h : i ≤ e) : scan p g i e ≤ e := by
  rw [scan]
  split
  · split
    · exact scan_le p g (i + 1) e (by omega)
    · omega
  · omega
termination_by e - i

/-- Every byte of `[s, e)` satisfies `p`. -/
def allIn (p : Nat → Bool) (g : Nat → Nat) (s e : Nat) : Bool :=
  if s < e then (if p (g s) then allIn p g (s + 1) e else false) else true
termination_by e - s

theorem scan_cont {p : Nat → Bool} {g : Nat → Nat} {i e : Nat} (h : i < e) (hp : p (g i) = true) :
    scan p g i e = scan p g (i + 1) e := by
  rw [scan, if_pos h, if_pos hp]

theorem scan_stop {p : Nat → Bool} {g : Nat → Nat} {i e : Nat} (h : i < e) (hp : p (g i) = false) :
    scan p g i e = i := by
  rw [scan, if_pos h, if_neg (by simp [hp])]

theorem scan_end {p : Nat → Bool} {g : Nat → Nat} {i e : Nat} (h : e ≤ i) : scan p g i e = i := by
  rw [scan, if_neg (by omega)]

theorem allIn_cont {p : Nat → Bool} {g : Nat → Nat} {i e : Nat} (h : i < e) (hp : p (g i) = true) :
    allIn p g i e = allIn p g (i + 1) e := by
  rw [allIn, if_pos h, if_pos hp]

theorem allIn_stop {p : Nat → Bool} {g : Nat → Nat} {i e : Nat} (h : i < e) (hp : p (g i) = false) :
    allIn p g i e = false := by
  rw [allIn, if_pos h, if_neg (by simp [hp])]

theorem allIn_end {p : Nat → Bool} {g : Nat → Nat} {i e : Nat} (h : e ≤ i) : allIn p g i e = true := by
  rw [allIn, if_neg (by omega)]

/-- `p` less the OWS bytes before it, down to `s`. -/
def trimEnd (g : Nat → Nat) (s p : Nat) : Nat :=
  if s < p then (if isOws (g (p - 1)) then trimEnd g s (p - 1) else p) else p
termination_by p

theorem trimEnd_ge (g : Nat → Nat) (s p : Nat) (h : s ≤ p) : s ≤ trimEnd g s p := by
  rw [trimEnd]
  split
  · split
    · exact trimEnd_ge g s (p - 1) (by omega)
    · omega
  · omega
termination_by p

theorem trimEnd_le (g : Nat → Nat) (s p : Nat) : trimEnd g s p ≤ p := by
  rw [trimEnd]
  split
  · split
    · have := trimEnd_le g s (p - 1); omega
    · omega
  · omega
termination_by p

/-- `g (s + q) .. ` matches `lit` from position `q` on, ignoring case. -/
def eqFrom (g : Nat → Nat) (s : Nat) (lit : List Nat) (q : Nat) : Bool :=
  if q < lit.length then (if lower (g (s + q)) ≠ lit.getD q 0 then false else eqFrom g s lit (q + 1))
  else true
termination_by lit.length - q

/-- `[s, e)` spells the lowercase `lit`, ignoring ASCII case. -/
def eqCi (g : Nat → Nat) (s e : Nat) (lit : List Nat) : Bool :=
  if e < s ∨ e - s ≠ lit.length then false else eqFrom g s lit 0

/-! ## Lines -/

/-- The first CR or LF in `[i, n)`, or `n`. -/
def lineEnd (g : Nat → Nat) (i n : Nat) : Nat := scan notEol g i n

/-- How the line ending at `j` is terminated within `[0, n)`: 0 by CRLF, 1 not
yet, 2 by a bare CR or LF. -/
def lineTerm (g : Nat → Nat) (j n : Nat) : Nat :=
  if n ≤ j then 1
  else if g j = 10 then 2
  else if n ≤ j + 1 then 1
  else if g (j + 1) ≠ 10 then 2
  else 0

/-! ## Request line -/

/-- `g (s + q) ..` is exactly `lit` from position `q` on. -/
def eqAt (g : Nat → Nat) (s : Nat) (lit : List Nat) (q : Nat) : Bool :=
  if q < lit.length then (if g (s + q) ≠ lit.getD q 0 then false else eqAt g s lit (q + 1))
  else true
termination_by lit.length - q

/-- `HTTP/` -/
def litHttp : List Nat := [72, 84, 84, 80, 47]

/-- The version from the three bytes after `HTTP/`. -/
def versionDigits (a dot b : Nat) : Nat :=
  if dot ≠ 46 ∨ a < 48 ∨ 57 < a ∨ b < 48 ∨ 57 < b then 3
  else if a ≠ 49 then 2
  else if b = 48 then 0
  else if b = 49 then 1
  else 2

/-- `HTTP/1.0` (0), `HTTP/1.1` (1), another `HTTP/d.d` (2), anything else (3). -/
def version (g : Nat → Nat) (s e : Nat) : Nat :=
  if e < s ∨ e - s ≠ 8 then 3
  else if !eqAt g s litHttp 0 then 3
  else versionDigits (g (s + 5)) (g (s + 6)) (g (s + 7))

inductive ReqLine where
  | bad (status : Nat)
  /-- Method `[ms, me)`, target `[ts, te)`, minor version. -/
  | ok (ms me ts te minor : Nat)
  deriving DecidableEq, Repr

/-- `method SP origin-form SP HTTP-version` in `[i, j)`. -/
def requestLine (g : Nat → Nat) (i j : Nat) : ReqLine :=
  let m := scan isTchar g i j
  if m = i ∨ j ≤ m then .bad 400
  else if g m ≠ 32 then .bad 400
  else
    let te := scan isTargetByte g (m + 1) j
    if te = m + 1 ∨ j ≤ te then .bad 400
    else if g (m + 1) ≠ 47 ∨ g te ≠ 32 then .bad 400
    else
      let v := version g (te + 1) j
      if v = 3 then .bad 400
      else if v = 2 then .bad 505
      else .ok i m (m + 1) te v

/-! ## Header fields -/

inductive HdrLine where
  | bad (status : Nat)
  /-- The colon at `k`; the value without OWS is `[vs, ve)`. -/
  | ok (k vs ve : Nat)
  deriving DecidableEq, Repr

/-- `field-name ":" OWS field-value OWS` in `[i, j)`. -/
def headerLine (g : Nat → Nat) (i j : Nat) : HdrLine :=
  let k := scan isTchar g i j
  if k = i ∨ j ≤ k then .bad 400
  else if g k ≠ 58 then .bad 400
  else if !allIn isFieldByte g (k + 1) j then .bad 400
  else
    let vs := scan isOws g (k + 1) j
    .ok k vs (trimEnd g vs j)

def litHost : List Nat := [104, 111, 115, 116]
def litContentLength : List Nat := [99, 111, 110, 116, 101, 110, 116, 45, 108, 101, 110, 103, 116, 104]
def litTransferEncoding : List Nat :=
  [116, 114, 97, 110, 115, 102, 101, 114, 45, 101, 110, 99, 111, 100, 105, 110, 103]
def litChunked : List Nat := [99, 104, 117, 110, 107, 101, 100]

/-- 1 `Host`, 2 `Content-Length`, 3 `Transfer-Encoding`, 0 any other name. -/
def classify (g : Nat → Nat) (s e : Nat) : Nat :=
  if eqCi g s e litHost then 1
  else if eqCi g s e litContentLength then 2
  else if eqCi g s e litTransferEncoding then 3
  else 0

inductive Num where
  | bad (status : Nat)
  | ok (v : Nat)
  deriving DecidableEq, Repr

/-- Decimal digits `[p, e)` on top of `acc`; 413 as soon as the value passes `max`. -/
def clLoop (g : Nat → Nat) (e max acc p : Nat) : Num :=
  if p < e then
    if g p < 48 ∨ 57 < g p then .bad 400
    else if max < acc * 10 + (g p - 48) then .bad 413
    else clLoop g e max (acc * 10 + (g p - 48)) (p + 1)
  else .ok acc
termination_by e - p

/-- `Content-Length`: `1*DIGIT`, at most `max`. -/
def contentLength (g : Nat → Nat) (s e max : Nat) : Num :=
  if e ≤ s then .bad 400 else clLoop g e max 0 s

/-- A recorded field: name `[ns, ne)`, value `[vs, ve)`. -/
structure Field where
  ns : Nat
  ne : Nat
  vs : Nat
  ve : Nat
  deriving DecidableEq, Repr

structure Fields where
  headers : List Field
  host : Bool
  hasCl : Bool
  cl : Nat
  te : Bool
  deriving DecidableEq, Repr

def Fields.empty : Fields := ⟨[], false, false, 0, false⟩

def maxHeaders : Nat := 16

inductive Added where
  | bad (status : Nat)
  | ok (f : Fields)
  deriving DecidableEq, Repr

/-- Record field `[ns, ne)` with value `[vs, ve)`, from a line ending at `le`.
`Content-Length` and `Transfer-Encoding` values may not end in OWS
(`ve = le`). -/
def addHeader (g : Nat → Nat) (f : Fields) (ns ne vs ve le minor maxBody : Nat) : Added :=
  if maxHeaders ≤ f.headers.length then .bad 431 else
  let h := f.headers ++ [⟨ns, ne, vs, ve⟩]
  let kind := classify g ns ne
  if kind = 1 then
    if f.host then .bad 400 else .ok { f with headers := h, host := true }
  else if kind = 2 then
    if f.hasCl ∨ f.te ∨ ve ≠ le then .bad 400 else
    match contentLength g vs ve maxBody with
    | .bad s => .bad s
    | .ok v => .ok { f with headers := h, hasCl := true, cl := v }
  else if kind = 3 then
    if minor = 0 ∨ f.te ∨ f.hasCl ∨ ve ≠ le then .bad 400
    else if !eqCi g vs ve litChunked then .bad 501
    else .ok { f with headers := h, te := true }
  else .ok { f with headers := h }

inductive Fielded where
  | more
  | bad (status : Nat)
  | done (f : Fields) (len : Nat)
  deriving DecidableEq, Repr

/-- The header lines from `i` up to the empty line, within `[0, n)`. -/
def fields (g : Nat → Nat) (n minor maxBody i : Nat) (f : Fields) : Fielded :=
  if i < n then
    let j := lineEnd g i n
    let t := lineTerm g j n
    if t = 1 then .more
    else if t = 2 then .bad 400
    else if j = i then .done f (j + 2)
    else
      match headerLine g i j with
      | .bad s => .bad s
      | .ok k vs ve =>
        match addHeader g f i k vs ve j minor maxBody with
        | .bad s => .bad s
        | .ok f' => fields g n minor maxBody (j + 2) f'
  else .more
termination_by n - i
decreasing_by have := scan_ge notEol g i n; unfold lineEnd at *; omega

/-! ## The head -/

inductive Framing where
  | empty
  | length (n : Nat)
  | chunked
  deriving DecidableEq, Repr

structure Head where
  method : Nat × Nat
  target : Nat × Nat
  minor : Nat
  headers : List Field
  len : Nat
  framing : Framing
  deriving DecidableEq, Repr

inductive HeadR where
  | incomplete
  | reject (status : Nat)
  | done (h : Head)
  deriving DecidableEq, Repr

/-- The bytes the parser may read: `L` below `n`, 0 from there on. -/
def window (L : List Nat) (n : Nat) : Nat → Nat := fun k => if k < n then L.getD k 0 else 0

/-- How many bytes the parser looks at. -/
def readLimit (len maxHead : Nat) : Nat := if maxHead ≤ len then maxHead else len

/-- Parse the request head at the start of `L`. -/
def parseHead (L : List Nat) (maxHead maxBody : Nat) : HeadR :=
  let full := decide (maxHead ≤ L.length)
  let n := readLimit L.length maxHead
  let g := window L n
  let j := lineEnd g 0 n
  let t := lineTerm g j n
  if t = 1 then (if full then .reject 414 else .incomplete)
  else if t = 2 then .reject 400
  else
    match requestLine g 0 j with
    | .bad s => .reject s
    | .ok ms me ts te minor =>
      match fields g n minor maxBody (j + 2) Fields.empty with
      | .more => if full then .reject 431 else .incomplete
      | .bad s => .reject s
      | .done f len =>
        if minor = 1 ∧ !f.host then .reject 400
        else .done ⟨(ms, me), (ts, te), minor, f.headers, len,
          if f.te then .chunked else if f.hasCl then .length f.cl else .empty⟩

/-! ## Chunked bodies -/

/-- The decoder's state: phase, chunk size (or bytes left), size digits, total. -/
structure CState where
  phase : Nat
  size : Nat
  digits : Nat
  total : Nat
  deriving DecidableEq, Repr

def cStart : CState := ⟨0, 0, 0, 0⟩

inductive COut where
  | nothing
  | data
  | done
  | reject (status : Nat)
  deriving DecidableEq, Repr

/-- A hex digit's value, either case, or 16. -/
def hexv (c : Nat) : Nat :=
  if 48 ≤ c ∧ c ≤ 57 then c - 48
  else if 97 ≤ c ∧ c ≤ 102 then c - 87
  else if 65 ≤ c ∧ c ≤ 70 then c - 55
  else 16

/-- Phase 0: a size digit, or the CR after at least one. -/
def sizeStep (st : CState) (b maxBody : Nat) : CState × COut :=
  if hexv b < 16 then
    if 8 ≤ st.digits ∨ 268435455 < st.size then (st, .reject 400)
    else ({ st with size := st.size * 16 + hexv b, digits := st.digits + 1 }, .nothing)
  else if b = 13 ∧ 0 < st.digits then
    if maxBody < st.total ∨ maxBody - st.total < st.size then (st, .reject 413)
    else ({ st with phase := 1, total := st.total + st.size }, .nothing)
  else (st, .reject 400)

/-- Exactly byte `want`, then phase `next` with output `o`. -/
def expectStep (st : CState) (b want next : Nat) (o : COut) : CState × COut :=
  if b ≠ want then (st, .reject 400) else ({ st with phase := next }, o)

/-- One byte of `chunked`: `1*8HEXDIG CRLF`, data, CRLF, ..., `0 CRLF CRLF`.
Phases: 0 size, 1 size LF, 2 data, 3 data CR, 4 data LF, 5 last CR, 6 last LF, 7 end. -/
def chunkStep (st : CState) (b maxBody : Nat) : CState × COut :=
  if st.phase = 0 then sizeStep st b maxBody
  else if st.phase = 1 then
    if b ≠ 10 then (st, .reject 400)
    else ({ st with phase := if st.size = 0 then 5 else 2, digits := 0 }, .nothing)
  else if st.phase = 2 then
    if st.size ≤ 1 then ({ st with phase := 3, size := 0 }, .data)
    else ({ st with size := st.size - 1 }, .data)
  else if st.phase = 3 then expectStep st b 13 4 .nothing
  else if st.phase = 4 then expectStep { st with size := 0 } b 10 0 .nothing
  else if st.phase = 5 then expectStep st b 13 6 .nothing
  else if st.phase = 6 then expectStep st b 10 7 .done
  else (st, .reject 400)

/-! ## Response heads -/

/-- Decimal digits of `n`, most significant first, from position `c` of
`10^9, ..., 10^0`; `started` once a digit was written. -/
def decFrom (n c : Nat) (started : Bool) : List Nat :=
  if c < 10 then
    let d := (n / 10 ^ (9 - c)) % 10
    if d ≠ 0 ∨ started ∨ c = 9 then (48 + d) :: decFrom n (c + 1) true else decFrom n (c + 1) false
  else []
termination_by 10 - c

/-- `n < 2^32` in decimal, without leading zeros. -/
def dec (n : Nat) : List Nat := decFrom n 0 false

example : dec 0 = [48] := by simp [dec, decFrom]
example : dec 1024 = [49, 48, 50, 52] := by simp [dec, decFrom]
example : dec 4294967295 = [52, 50, 57, 52, 57, 54, 55, 50, 57, 53] := by simp [dec, decFrom]

/-- The reason phrase sent with a status, as bytes (ASCII). -/
def reasonOf (status : Nat) : List Nat :=
  if status = 200 then [79, 75] -- OK
  else if status = 201 then [67, 114, 101, 97, 116, 101, 100] -- Created
  else if status = 204 then [78, 111, 32, 67, 111, 110, 116, 101, 110, 116] -- No Content
  else if status = 400 then [66, 97, 100, 32, 82, 101, 113, 117, 101, 115, 116] -- Bad Request
  else if status = 401 then [85, 110, 97, 117, 116, 104, 111, 114, 105, 122, 101, 100] -- Unauthorized
  else if status = 403 then [70, 111, 114, 98, 105, 100, 100, 101, 110] -- Forbidden
  else if status = 404 then [78, 111, 116, 32, 70, 111, 117, 110, 100] -- Not Found
  else if status = 405 then [77, 101, 116, 104, 111, 100, 32, 78, 111, 116, 32, 65, 108, 108, 111, 119, 101, 100] -- Method Not Allowed
  else if status = 408 then [82, 101, 113, 117, 101, 115, 116, 32, 84, 105, 109, 101, 111, 117, 116] -- Request Timeout
  else if status = 409 then [67, 111, 110, 102, 108, 105, 99, 116] -- Conflict
  else if status = 413 then [67, 111, 110, 116, 101, 110, 116, 32, 84, 111, 111, 32, 76, 97, 114, 103, 101] -- Content Too Large
  else if status = 414 then [85, 82, 73, 32, 84, 111, 111, 32, 76, 111, 110, 103] -- URI Too Long
  else if status = 422 then [85, 110, 112, 114, 111, 99, 101, 115, 115, 97, 98, 108, 101, 32, 67, 111, 110, 116, 101, 110, 116] -- Unprocessable Content
  else if status = 431 then [82, 101, 113, 117, 101, 115, 116, 32, 72, 101, 97, 100, 101, 114, 32, 70, 105, 101, 108, 100, 115, 32, 84, 111, 111, 32, 76, 97, 114, 103, 101] -- Request Header Fields Too Large
  else if status = 500 then [73, 110, 116, 101, 114, 110, 97, 108, 32, 83, 101, 114, 118, 101, 114, 32, 69, 114, 114, 111, 114] -- Internal Server Error
  else if status = 501 then [78, 111, 116, 32, 73, 109, 112, 108, 101, 109, 101, 110, 116, 101, 100] -- Not Implemented
  else if status = 503 then [83, 101, 114, 118, 105, 99, 101, 32, 85, 110, 97, 118, 97, 105, 108, 97, 98, 108, 101] -- Service Unavailable
  else if status = 505 then [72, 84, 84, 80, 32, 86, 101, 114, 115, 105, 111, 110, 32, 78, 111, 116, 32, 83, 117, 112, 112, 111, 114, 116, 101, 100] -- HTTP Version Not Supported
  else []

/-- 1xx, 204 and 304 responses have no body, and no `Content-Length`. -/
def hasBody (status : Nat) : Bool := if status < 200 then false else status != 204 && status != 304

/-- The response head `write_head` writes (ASCII):
`HTTP/1.1 <status> <reason>` CRLF, `Content-Type: <ct>` CRLF when `ct` is not
empty, `Content-Length: <bodyLen>` CRLF when `hasBody status`, `Connection:
close` CRLF when `close`, and CRLF. -/
def renderHead (status : Nat) (ct : List Nat) (bodyLen : Nat) (close : Bool) : List Nat :=
  [72, 84, 84, 80, 47, 49, 46, 49, 32] ++ [48 + status / 100, 48 + status / 10 % 10, 48 + status % 10, 32] ++
    reasonOf status ++ [13, 10] ++
    (if ct = [] then [] else [67, 111, 110, 116, 101, 110, 116, 45, 84, 121, 112, 101, 58, 32]) ++ ct ++ (if ct = [] then [] else [13, 10]) ++
    (if hasBody status then [67, 111, 110, 116, 101, 110, 116, 45, 76, 101, 110, 103, 116, 104, 58, 32] ++
      dec bodyLen ++ [13, 10] else []) ++
    (if close then [67, 111, 110, 110, 101, 99, 116, 105, 111, 110, 58, 32, 99, 108, 111, 115, 101] ++ [13, 10] else []) ++ [13, 10]

end h5i_app_h1.Spec
