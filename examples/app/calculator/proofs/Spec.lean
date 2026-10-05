import CalculatorKernel
/-!
# What the calculator should do

The file to review: the arithmetic we mean, on natural numbers, with no
loops, `U64` overflow checks or vectors.
-/
open calculator_kernel

namespace calculator_kernel.Spec

/-- `a op b`, or why there is no answer below 2^64. -/
def eval : Op → Nat → Nat → Except Error Nat
  | .Add, a, b => if a + b < 2 ^ 64 then .ok (a + b) else .error .Overflow
  | .Sub, a, b => if b ≤ a then .ok (a - b) else .error .Underflow
  | .Mul, a, b => if a * b < 2 ^ 64 then .ok (a * b) else .error .Overflow
  | .Div, a, b => if b = 0 then .error .DivByZero else .ok (a / b)

/-- User `u`'s memory: the first row for `u`, or 0. -/
def memOf (ms : List Memory) (u : Nat) : Nat :=
  ((ms.find? (fun m => m.user.val = u)).map (fun m => m.value.val)).getD 0

end calculator_kernel.Spec
