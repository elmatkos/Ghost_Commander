//! Times a copy through the same code path the app uses.
//! `cargo run --release -p gc-core --example bench_copy -- <source file or folder> <empty destination folder>`
//! `GC_COPY_THREADS=1` forces one file at a time.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use gc_core::jobs::{self, Op};

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(src), Some(dest)) = (args.next(), args.next()) else {
        eprintln!("usage: bench_copy <source> <destination folder>");
        return;
    };
    let start = Instant::now();
    let job = jobs::spawn(Op::Copy { srcs: vec![PathBuf::from(src)], dest_dir: PathBuf::from(dest) });
    while !job.progress.finished.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(10));
    }
    let secs = start.elapsed().as_secs_f64();
    let bytes = job.progress.done.load(Ordering::Relaxed);
    println!("{:.2} s, {:.1} MiB, {:.0} MiB/s, errors: {:?}", secs, bytes as f64 / 1048576.0, bytes as f64 / 1048576.0 / secs, job.progress.errors());
}
