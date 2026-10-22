# h5i-app-gpu

Optional batch adapters for pure application kernels. Kernels and Lean 4
verification remain the source of application semantics. This crate has no
application dependencies, CUDA dependencies, or required device representations.

```rust
use h5i_app_gpu::{batch::{Backend, Cpu}, differential::{compare, check_batch}};
use std::{convert::Infallible, num::NonZeroUsize};

let mut cpu = Cpu(|row: &u64| Ok::<_, Infallible>(row / 2));
assert_eq!(cpu.run((), &[8, 10]).unwrap(), vec![4, 5]);
// Supply a CUDA adapter as candidate in an application with GPU support.
let mut candidate = Cpu(|row: &u64| Ok::<_, Infallible>(row / 2));
compare(&mut cpu, &mut candidate, (), &[8, 10][..], PartialEq::eq).unwrap();
check_batch(&mut candidate, &[8, 10], NonZeroUsize::new(1).unwrap()).unwrap();
```

- `Backend<Input, Operation, Response>`: application-owned types and resources.
- `Cpu`: ordinary Rust function over independent rows.
- `execute_batch`: CPU/GPU/Auto selection, exact output count, no error replay.
- `DeviceRepr`: opt-in, fallible application representation conversions.
- `compare` / `check_batch`: output equivalence and execution conformance.

`backend-nvvm` is the separately built Rust-CUDA/cust adapter. Applications own
PTX, entry point ABI and semantics; only their launch shell uses unsafe CUDA.
The search/push adapters live in `examples/app/parallel/gpu`; the integer pricing
example in `examples/app/discount-gpu` exercises a different ABI with shared
host/device logic. See `docs/app/GPU.md` for toolchain and reproduction commands.

GPU computations must not commit application state. Validate errors and domain
invariants before the existing application transition/commit. Shared source and
differential tests are evidence for tested executions, not formal GPU proofs.
