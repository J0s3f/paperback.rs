//! Runs batches of CPU-bound tasks of unequal length with several ways of using threads.
//!
//! The decoder's shape of work: for a block that does not read, a dozen ways of reading it are
//! tried, each a millisecond or more and some many times longer; the first that works counts.
//! Every batch here is such a set of independent tasks, one batch after the other.

use std::hint::black_box;
use std::time::Instant;

/// About 40 microseconds of arithmetic per unit, which the optimiser cannot remove.
fn work(units: u32) -> u64 {
    let mut state = 0x9E37_79B9_7F4A_7C15u64 ^ u64::from(units);
    let mut sum = 0.0f64;
    for _ in 0..units * 20_000 {
        state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        sum += f64::from((state >> 40) as u32) * 1e-6;
    }
    black_box(state ^ sum.to_bits())
}

type Backend = (&'static str, Box<dyn Fn(&[u32]) -> u64>);

fn backends(threads: usize) -> Vec<Backend> {
    let runtime = std::sync::Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(threads)
            .build()
            .expect("tokio runtime"),
    );
    // smol's blocking pool grows on demand; its executor threads are not used for blocking work.
    vec![
        (
            "sequential",
            Box::new(|tasks| tasks.iter().map(|&u| work(u)).fold(0, |a, b| a ^ b)),
        ),
        (
            "rayon",
            Box::new(|tasks| {
                use rayon::prelude::*;
                tasks.par_iter().map(|&u| work(u)).reduce(|| 0, |a, b| a ^ b)
            }),
        ),
        (
            "rayon max_len(1)",
            Box::new(|tasks| {
                use rayon::prelude::*;
                tasks
                    .par_iter()
                    .with_max_len(1)
                    .map(|&u| work(u))
                    .reduce(|| 0, |a, b| a ^ b)
            }),
        ),
        (
            "rayon::scope spawn",
            Box::new(|tasks| {
                let results = std::sync::Mutex::new(0u64);
                rayon::scope(|scope| {
                    for &u in tasks {
                        let results = &results;
                        scope.spawn(move |_| {
                            let value = work(u);
                            *results.lock().expect("lock") ^= value;
                        });
                    }
                });
                results.into_inner().expect("lock")
            }),
        ),
        (
            "std::thread::scope",
            Box::new(|tasks| {
                std::thread::scope(|scope| {
                    let handles: Vec<_> = tasks
                        .iter()
                        .map(|&u| scope.spawn(move || work(u)))
                        .collect();
                    handles
                        .into_iter()
                        .map(|h| h.join().expect("task"))
                        .fold(0, |a, b| a ^ b)
                })
            }),
        ),
        (
            "tokio spawn_blocking",
            Box::new(move |tasks| {
                let runtime = runtime.clone();
                runtime.block_on(async {
                    let handles: Vec<_> = tasks
                        .iter()
                        .map(|&u| tokio::task::spawn_blocking(move || work(u)))
                        .collect();
                    let mut sum = 0;
                    for handle in handles {
                        sum ^= handle.await.expect("task");
                    }
                    sum
                })
            }),
        ),
        (
            "smol unblock",
            Box::new(|tasks| {
                smol::block_on(async {
                    let handles: Vec<_> = tasks
                        .iter()
                        .map(|&u| smol::unblock(move || work(u)))
                        .collect();
                    let mut sum = 0;
                    for handle in handles {
                        sum ^= handle.await;
                    }
                    sum
                })
            }),
        ),
    ]
}

fn measure(name: &str, run: &dyn Fn(&[u32]) -> u64, tasks: &[u32], batches: usize) -> f64 {
    // One warm-up batch, so that thread pools exist.
    let expected = run(tasks);
    let start = Instant::now();
    let mut check = 0;
    for _ in 0..batches {
        check ^= run(tasks);
    }
    let seconds = start.elapsed().as_secs_f64();
    let _ = (name, expected, check);
    seconds
}

fn main() {
    let threads = std::thread::available_parallelism().map_or(4, usize::from);
    println!("{threads} threads available\n");
    let scenarios: [(&str, Vec<u32>, usize); 3] = [
        (
            "16 tasks, 1 to 12 units (uneven, like stubborn blocks)",
            vec![1, 1, 1, 2, 2, 2, 3, 3, 4, 4, 6, 8, 1, 1, 2, 12],
            300,
        ),
        ("16 tasks of 4 units (even)", vec![4; 16], 300),
        (
            "4 tasks of 1 unit (too small to be worth threads)",
            vec![1; 4],
            3000,
        ),
    ];
    let all = backends(threads);
    for (title, tasks, batches) in &scenarios {
        println!("{title}, {batches} batches");
        let mut base = 0.0;
        for (name, run) in &all {
            let seconds = measure(name, run.as_ref(), tasks, *batches);
            if *name == "sequential" {
                base = seconds;
            }
            println!(
                "  {name:<22} {seconds:>7.3} s  {:>5.1}x  {:>7.1} us per batch",
                base / seconds,
                seconds * 1e6 / *batches as f64
            );
        }
        println!();
    }
}
