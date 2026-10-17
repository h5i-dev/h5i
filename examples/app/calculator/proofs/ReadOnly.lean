import Proofs
import H5iAppLib.ReadOnly
open Aeneas Aeneas.Std Result calculator_kernel
namespace calculator_kernel.ReadOnly

def writesOf : Option Memory → List Memory
  | none => []
  | some m => [m]

/-- Every accepted Get writes nothing, for all actors and snapshots. -/
theorem sound : H5iAppLib.ReadOnly.Sound read_only transition writesOf := by
  apply H5iAppLib.ReadOnly.sound_of_spec
  intro a s c hro
  cases c <;> simp [read_only] at hro
  unfold transition
  step with Proofs.memory_of_spec
  simp [H5iAppLib.OnOk, writesOf]

end calculator_kernel.ReadOnly
