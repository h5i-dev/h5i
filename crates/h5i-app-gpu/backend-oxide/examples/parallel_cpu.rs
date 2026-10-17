//! CPU reference timings only. No GPU timing is inferred from these results.
use std::{hint::black_box,time::Instant};
use parallel_kernel::{bookmark_search,push_rule};
fn main() {
    let mut terms=[0u8;128];
    for t in 0..8 {terms[t*16..t*16+12].copy_from_slice(b"nonmatchword");}
    let text=(0..512).map(|i| b'a'+(i%23) as u8).collect::<Vec<_>>();
    let event=b"hello Alice, a notification message with enough content to exercise wildcard matching";
    let pattern=b"h*notification*matching";
    println!("workload,rows,threads,seconds,rows_per_second,checksum");
    for n in [10_000usize,100_000,1_000_000] {
        for threads in [1,16] {
            for search in [true,false] {
                let start=Instant::now();
                let checksum:usize=std::thread::scope(|scope| {
                    let mut tasks=Vec::new();
                    for worker in 0..threads {
                        let text=&text; let terms=&terms;
                        tasks.push(scope.spawn(move || {
                            let mut sum=0;
                            for _ in (worker..n).step_by(threads) {
                                let result=if search {bookmark_search(black_box(text),black_box(terms),8,false,0,3,1) as u32}
                                else {push_rule(black_box(event),black_box(pattern),black_box(b"Alice"),5,1,10,50,20,true,true)};
                                sum+=black_box(result) as usize;
                            }
                            sum
                        }));
                    }
                    tasks.into_iter().map(|t| t.join().unwrap()).sum()
                });
                let s=start.elapsed().as_secs_f64();
                println!("{},{n},{threads},{s:.6},{:.1},{checksum}",if search {"search"} else {"push"},n as f64/s);
            }
        }
    }
}
