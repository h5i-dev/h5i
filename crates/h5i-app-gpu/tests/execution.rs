use h5i_app_gpu::{
    batch::{Backend, Cpu},
    differential::{BatchCheckError, DifferentialError, check_batch, compare},
    repr::DeviceRepr,
    runtime::{ExecutionError, ExecutionPolicy, execute_batch},
};
use std::num::NonZeroUsize;

#[derive(Default)]
struct MockGpu {
    calls: usize,
    fail: bool,
}
impl Backend<[u64], (), Vec<u64>> for MockGpu {
    type Error = &'static str;
    fn run(&mut self, (): (), rows: &[u64]) -> Result<Vec<u64>, Self::Error> {
        self.calls += 1;
        if self.fail {
            Err("launch failed")
        } else {
            Ok(rows.iter().map(|x| x / 2).collect())
        }
    }
}
#[test]
fn dispatch_selects_cpu_or_gpu_before_execution() {
    let mut cpu_calls = 0;
    let mut cpu = Cpu(|x: &u64| {
        cpu_calls += 1;
        Ok::<_, &'static str>(x / 2)
    });
    let mut gpu = MockGpu::default();
    let policy = ExecutionPolicy::Auto { min_gpu_rows: 2 };
    assert_eq!(
        execute_batch(policy, &mut cpu, Some(&mut gpu), &[6]),
        Ok(vec![3])
    );
    assert_eq!(gpu.calls, 0);
    assert_eq!(
        execute_batch(policy, &mut cpu, Some(&mut gpu), &[6, 8]),
        Ok(vec![3, 4])
    );
    assert_eq!(gpu.calls, 1);
    assert_eq!(
        execute_batch(policy, &mut cpu, None::<&mut MockGpu>, &[6, 8]),
        Ok(vec![3, 4])
    );
    assert_eq!(
        execute_batch(ExecutionPolicy::Cpu, &mut cpu, Some(&mut gpu), &[8]),
        Ok(vec![4])
    );
    assert_eq!(gpu.calls, 1);
    assert_eq!(cpu_calls, 4);
}
#[test]
fn gpu_failure_is_never_replayed_on_cpu() {
    let mut cpu = Cpu(|_: &u64| -> Result<u64, &'static str> { panic!("must not replay") });
    let mut gpu = MockGpu {
        fail: true,
        ..Default::default()
    };
    for policy in [
        ExecutionPolicy::Gpu,
        ExecutionPolicy::Auto { min_gpu_rows: 0 },
    ] {
        assert_eq!(
            execute_batch(policy, &mut cpu, Some(&mut gpu), &[8]),
            Err(ExecutionError::Gpu("launch failed"))
        );
    }
    assert_eq!(
        execute_batch(ExecutionPolicy::Gpu, &mut cpu, None::<&mut MockGpu>, &[8]),
        Err(ExecutionError::GpuUnavailable)
    );
}
#[test]
fn cpu_errors_discard_partial_results() {
    let mut cpu = Cpu(|x: &u64| x.checked_add(1).ok_or("overflow"));
    assert_eq!(cpu.run((), &[2, u64::MAX, 3]), Err("overflow"));
}
#[test]
fn differential_detects_wrong_results_and_execution_errors() {
    let mut cpu = Cpu(|x: &u64| Ok::<_, &'static str>(x / 2));
    let mut gpu = MockGpu::default();
    compare(&mut cpu, &mut gpu, (), &[0, 1, u64::MAX][..], PartialEq::eq).unwrap();
    let mut wrong = Cpu(|x: &u64| Ok::<_, &'static str>(*x));
    assert!(matches!(
        compare(&mut cpu, &mut wrong, (), &[8][..], PartialEq::eq),
        Err(DifferentialError::Mismatch { .. })
    ));
    gpu.fail = true;
    assert!(matches!(
        compare(&mut cpu, &mut gpu, (), &[8][..], PartialEq::eq),
        Err(DifferentialError::Candidate("launch failed"))
    ));
    let mut failed = Cpu(|_: &u64| Err::<u64, _>("reference failed"));
    assert!(matches!(
        compare(&mut failed, &mut cpu, (), &[8][..], PartialEq::eq),
        Err(DifferentialError::Reference("reference failed"))
    ));
}
#[test]
fn independent_batches_cover_empty_boundaries_and_partitions() {
    let mut gpu = MockGpu::default();
    for n in [0, 1, 127, 128, 129, 257] {
        check_batch(
            &mut gpu,
            &vec![u64::MAX; n],
            NonZeroUsize::new(128).unwrap(),
        )
        .unwrap();
    }
}
#[test]
fn conformance_catches_truncation_state_and_partition_dependence() {
    struct Broken(u8);
    impl Backend<[u64], (), Vec<u64>> for Broken {
        type Error = ();
        fn run(&mut self, (): (), rows: &[u64]) -> Result<Vec<u64>, ()> {
            Ok(match self.0 {
                0 => vec![],
                1 | 2 => {
                    self.0 += 1;
                    vec![self.0 as u64; rows.len()]
                }
                _ => vec![rows.len() as u64; rows.len()],
            })
        }
    }
    let chunk = NonZeroUsize::new(1).unwrap();
    assert!(matches!(
        check_batch(&mut Broken(0), &[1, 2], chunk),
        Err(BatchCheckError::Length { .. })
    ));
    assert!(matches!(
        check_batch(&mut Broken(1), &[1, 2], chunk),
        Err(BatchCheckError::Nondeterministic { .. })
    ));
    assert!(matches!(
        check_batch(&mut Broken(3), &[1, 2], chunk),
        Err(BatchCheckError::Partition { .. })
    ));
}
#[test]
fn bounded_representations_reject_overflow_instead_of_truncating() {
    #[derive(Debug, PartialEq)]
    struct Host(u64);
    impl DeviceRepr for Host {
        type Device = u32;
        type Error = std::num::TryFromIntError;
        fn to_device(&self) -> Result<u32, Self::Error> {
            self.0.try_into()
        }
        fn from_device(value: u32) -> Result<Self, Self::Error> {
            Ok(Self(value.into()))
        }
    }
    assert_eq!(
        Host::from_device(Host(u32::MAX as u64).to_device().unwrap()).unwrap(),
        Host(u32::MAX as u64)
    );
    assert!(Host(u32::MAX as u64 + 1).to_device().is_err());
}

#[test]
fn dispatch_rejects_incomplete_backend_output() {
    struct Truncated;
    impl Backend<[u64], (), Vec<u64>> for Truncated {
        type Error = &'static str;
        fn run(&mut self, (): (), _: &[u64]) -> Result<Vec<u64>, Self::Error> {
            Ok(vec![])
        }
    }
    let mut cpu = Cpu(|x: &u64| Ok::<_, &'static str>(*x));
    assert_eq!(
        execute_batch(ExecutionPolicy::Gpu, &mut cpu, Some(&mut Truncated), &[8]),
        Err(ExecutionError::OutputLength {
            expected: 1,
            actual: 0
        })
    );
}
