/-!
# Reusable NOR Flash Formal Specification (`NorFlash`)

Minimal, storage-algorithm-independent Lean 4 specification of the
`embedded_storage_async::nor_flash::NorFlash` hardware interface, designed to
be reused across `FlashStore`, OTA updaters, and embedded filesystems.

## Separation of Guarantees vs Hardware/Failure Assumptions

1. **Trait Interface Preconditions (`ReadValid`, `WriteValid`, `EraseValid`)**:
   These mirror the caller obligations of `embedded_storage_async::nor_flash::{ReadNorFlash, NorFlash}`:
   - `read(addr, buf)`: `addr + buf.len ≤ capacity`, `addr % readSize = 0`, `buf.len % readSize = 0`.
   - `write(addr, buf)`: `addr + buf.len ≤ capacity`, `addr % writeSize = 0`, `buf.len % writeSize = 0`.
   - `erase(from, to)`: `from ≤ to ≤ capacity`, `from % eraseSize = 0`, `to % eraseSize = 0`.

2. **Hardware & Failure Model (`NorFlashStep`)**:
   These represent explicit assumptions about the physical NOR flash controller:
   - **Bit-transition physics**: programming clears bits (`mem' i = mem i &&& b`);
     writing `b` into an erased byte (`0xFF`) stores `b` verbatim.
   - **Erase physics**: a clean erase sets every byte in `[from, to)` to `0xFF`.
   - **Range isolation under faults (Assumption H1)**: a failed or power-cut
     `write(addr, bytes)` may leave arbitrary bytes inside `[addr, addr + bytes.length)`
     partially programmed (each byte `m'` satisfies `m' &&& mem i = m'`, i.e.,
     only `1 → 0` bit transitions from `mem i` toward `mem i &&& b`), and a
     failed or power-cut `erase(from, to)` may leave arbitrary bytes inside
     `[from, to)` in an indeterminate state, **but neither modifies any byte
     outside its target range**.
   - **Reboot persistence (Assumption H2)**: across a power loss and reboot,
     the NOR flash byte array `mem` persists unchanged.
   - **Non-interference of reads (Assumption H3)**: `read` never mutates `mem`.
-/

namespace NorFlash

/-- Hardware geometry corresponding to `ReadNorFlash` / `NorFlash` associated constants. -/
structure FlashConfig where
  capacity : Nat
  readSize : Nat
  writeSize : Nat
  eraseSize : Nat
deriving DecidableEq, Repr

/-- Well-formed NOR flash hardware parameters. -/
def ValidConfig (cfg : FlashConfig) : Prop :=
  0 < cfg.readSize ∧
  0 < cfg.writeSize ∧
  0 < cfg.eraseSize ∧
  cfg.capacity % cfg.eraseSize = 0

/-- Physical NOR flash byte memory modeled as a total function `Nat → UInt8`. -/
structure FlashState where
  mem : Nat → UInt8

/-- Erased NOR flash byte (`0xFF`, all 1-bits). -/
def erasedByte : UInt8 := 0xFF

/-- Every byte in `[addr, addr + len)` is erased (`0xFF`). -/
def isErasedRange (s : FlashState) (addr len : Nat) : Prop :=
  ∀ i, addr ≤ i → i < addr + len → s.mem i = erasedByte

/-- Two flash states agree on every byte in `[addr, addr + len)`. -/
def agreesOn (s₁ s₂ : FlashState) (addr len : Nat) : Prop :=
  ∀ i, addr ≤ i → i < addr + len → s₁.mem i = s₂.mem i

/-- Two half-open address ranges `[a₁, a₁ + l₁)` and `[a₂, a₂ + l₂)` are disjoint. -/
def disjoint (a₁ l₁ a₂ l₂ : Nat) : Prop :=
  a₁ + l₁ ≤ a₂ ∨ a₂ + l₂ ≤ a₁

/-- Caller precondition for `ReadNorFlash::read(addr, len)`. -/
def ReadValid (cfg : FlashConfig) (addr len : Nat) : Prop :=
  addr + len ≤ cfg.capacity ∧
  addr % cfg.readSize = 0 ∧
  len % cfg.readSize = 0

/-- Caller precondition for `NorFlash::write(addr, len)`. -/
def WriteValid (cfg : FlashConfig) (addr len : Nat) : Prop :=
  addr + len ≤ cfg.capacity ∧
  addr % cfg.writeSize = 0 ∧
  len % cfg.writeSize = 0

/-- Caller precondition for `NorFlash::erase(from_, to_)`. -/
def EraseValid (cfg : FlashConfig) (from_ to_ : Nat) : Prop :=
  from_ ≤ to_ ∧
  to_ ≤ cfg.capacity ∧
  from_ % cfg.eraseSize = 0 ∧
  to_ % cfg.eraseSize = 0

/-- Read `len` bytes starting at `addr` as a `List UInt8`. -/
def readSlice (s : FlashState) (addr len : Nat) : List UInt8 :=
  (List.range len).map (fun i => s.mem (addr + i))

/-- Clean NOR flash write: clears bits (`mem i &&& bytes[i - addr]`) in `[addr, addr + bytes.length)`. -/
def writeBytes (s : FlashState) (addr : Nat) (bytes : List UInt8) : FlashState where
  mem i :=
    if h : addr ≤ i ∧ i < addr + bytes.length then
      s.mem i &&& bytes[i - addr]'(by omega)
    else
      s.mem i

/-- Clean NOR flash erase: sets every byte in `[from_, to_)` to `0xFF`. -/
def eraseRange (s : FlashState) (from_ to_ : Nat) : FlashState where
  mem i :=
    if from_ ≤ i ∧ i < to_ then erasedByte else s.mem i

/-- Hardware operations on NOR flash, including partial failures and reboots. -/
inductive NorFlashOp where
  | read (addr len : Nat)
  | write (addr : Nat) (bytes : List UInt8)
  | erase (from_ to_ : Nat)
  | reboot

/-- Operational semantics of `NorFlash` with explicit failure / power-loss steps. -/
inductive NorFlashStep (cfg : FlashConfig) : NorFlashOp → FlashState → FlashState → Prop where
  | readOk {addr len : Nat} {s : FlashState}
      (hv : ReadValid cfg addr len) :
      NorFlashStep cfg (.read addr len) s s
  | readErr {addr len : Nat} {s : FlashState}
      (hv : ReadValid cfg addr len) :
      NorFlashStep cfg (.read addr len) s s
  | writeOk {addr : Nat} {bytes : List UInt8} {s : FlashState}
      (hv : WriteValid cfg addr bytes.length) :
      NorFlashStep cfg (.write addr bytes) s (writeBytes s addr bytes)
  | writeFailPartial {addr : Nat} {bytes : List UInt8} {s s' : FlashState}
      (hv : WriteValid cfg addr bytes.length)
      (hIso : ∀ i, (i < addr ∨ addr + bytes.length ≤ i) → s'.mem i = s.mem i)
      (hNor : ∀ i, addr ≤ i → i < addr + bytes.length → (s'.mem i &&& s.mem i) = s'.mem i) :
      NorFlashStep cfg (.write addr bytes) s s'
  | eraseOk {from_ to_ : Nat} {s : FlashState}
      (hv : EraseValid cfg from_ to_) :
      NorFlashStep cfg (.erase from_ to_) s (eraseRange s from_ to_)
  | eraseFailPartial {from_ to_ : Nat} {s s' : FlashState}
      (hv : EraseValid cfg from_ to_)
      (hIso : ∀ i, (i < from_ ∨ to_ ≤ i) → s'.mem i = s.mem i) :
      NorFlashStep cfg (.erase from_ to_) s s'
  | reboot {s : FlashState} :
      NorFlashStep cfg .reboot s s

/-! ## Reusable NOR Flash Specification Lemmas -/

theorem erasedByte_and (b : UInt8) : erasedByte &&& b = b := by
  cases b with | ofBitVec bv =>
    exact congrArg UInt8.ofBitVec (BitVec.allOnes_and (x := bv))

theorem readSlice_length (s : FlashState) (addr len : Nat) :
    (readSlice s addr len).length = len := by
  simp [readSlice]

theorem readSlice_eq_of_agreesOn {s₁ s₂ : FlashState} {addr len : Nat}
    (h : agreesOn s₁ s₂ addr len) :
    readSlice s₁ addr len = readSlice s₂ addr len := by
  unfold readSlice
  apply List.map_congr_left
  intro i hi
  rw [List.mem_range] at hi
  exact h (addr + i) (by omega) (by omega)

theorem writeBytes_in_erased {s : FlashState} {addr : Nat} {bytes : List UInt8}
    (he : isErasedRange s addr bytes.length) :
    readSlice (writeBytes s addr bytes) addr bytes.length = bytes := by
  apply List.ext_getElem
  · simp [readSlice]
  · intro i h1 h2
    simp only [readSlice, List.getElem_map, List.getElem_range]
    have hlt : i < bytes.length := by simpa [readSlice] using h1
    have hmem : (writeBytes s addr bytes).mem (addr + i) = s.mem (addr + i) &&& bytes[i] := by
      unfold writeBytes
      have hcond : addr ≤ addr + i ∧ addr + i < addr + bytes.length := ⟨by omega, by omega⟩
      simp [hcond]
    rw [hmem, he (addr + i) (by omega) (by omega), erasedByte_and]

theorem eraseRange_erased (s : FlashState) (from_ to_ : Nat) (hle : from_ ≤ to_) :
    isErasedRange (eraseRange s from_ to_) from_ (to_ - from_) := by
  intro i h1 h2
  unfold eraseRange
  have hc : from_ ≤ i ∧ i < to_ := ⟨h1, by omega⟩
  simp [hc]

theorem writeBytes_disjoint_preserved {s : FlashState} {waddr a l : Nat} {bytes : List UInt8}
    (hdisj : disjoint waddr bytes.length a l) :
    agreesOn (writeBytes s waddr bytes) s a l := by
  intro i h1 h2
  unfold writeBytes
  have hnot : ¬ (waddr ≤ i ∧ i < waddr + bytes.length) := by
    rcases hdisj with h | h <;> omega
  simp [hnot]

theorem eraseRange_disjoint_preserved {s : FlashState} {from_ to_ a l : Nat}
    (hdisj : disjoint from_ (to_ - from_) a l) (hle : from_ ≤ to_) :
    agreesOn (eraseRange s from_ to_) s a l := by
  intro i h1 h2
  unfold eraseRange
  have hnot : ¬ (from_ ≤ i ∧ i < to_) := by
    rcases hdisj with h | h <;> omega
  simp [hnot]

theorem step_write_disjoint_preserved {cfg : FlashConfig} {waddr a l : Nat}
    {bytes : List UInt8} {s s' : FlashState}
    (hstep : NorFlashStep cfg (.write waddr bytes) s s')
    (hdisj : disjoint waddr bytes.length a l) :
    agreesOn s' s a l := by
  cases hstep with
  | writeOk _ => exact writeBytes_disjoint_preserved hdisj
  | writeFailPartial _ hIso _ =>
    intro i h1 h2
    exact hIso i (by rcases hdisj with h | h <;> omega)

theorem step_erase_disjoint_preserved {cfg : FlashConfig} {from_ to_ a l : Nat}
    {s s' : FlashState}
    (hstep : NorFlashStep cfg (.erase from_ to_) s s')
    (hdisj : disjoint from_ (to_ - from_) a l) :
    agreesOn s' s a l := by
  cases hstep with
  | eraseOk hv => exact eraseRange_disjoint_preserved hdisj hv.1
  | eraseFailPartial hv hIso =>
    intro i h1 h2
    exact hIso i (by have := hv.1; rcases hdisj with h | h <;> omega)

end NorFlash
