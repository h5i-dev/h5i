use h5i_app_gpu_backend_nvvm::Failure;
use parallel_gpu::ParallelGpu;
use parallel_gpu::{
    batch::{Backend, Cpu, Decision},
    differential::{check, fixture},
};
fn main() -> Result<(), Failure> {
    let ptx = std::fs::read_to_string(std::env::args().nth(1).ok_or("pass PTX path")?)?;
    let mut gpu = ParallelGpu::new(&ptx)?;
    check(&mut gpu)?;
    eprintln!("random differential, error slots, batch independence, SoA boundaries: passed");
    if std::env::args().any(|arg| arg == "--check-only") {
        return Ok(());
    }
    println!("workload,rows,repeat,kernel_ms,total_ms,cpu_ms,checksum");
    for n in [10_000, 100_000, 1_000_000] {
        for decision in [Decision::Search, Decision::Push] {
            let mut b = fixture(n, false);
            if matches!(decision, Decision::Push) {
                let event=b"hello Alice, a notification message with enough content to exercise wildcard matching";
                for row in b.text.chunks_exact_mut(512) {
                    row.fill(0);
                    row[..event.len()].copy_from_slice(event);
                }
            }
            let start = std::time::Instant::now();
            let cpu = Cpu.run(decision, &b).unwrap();
            let cpu_ms = start.elapsed().as_secs_f64() * 1000.;
            gpu.run(decision, &b)?; // warm up JIT/device caches; excluded
            for repeat in 0..3 {
                let out = gpu.run(decision, &b)?;
                assert_eq!(out, cpu);
                let checksum: u64 = out.values.iter().map(|v| *v as u64).sum();
                println!(
                    "{decision:?},{n},{repeat},{:.6},{:.6},{cpu_ms:.6},{checksum}",
                    gpu.kernel_ms, gpu.total_ms
                );
            }
        }
    }
    Ok(())
}
