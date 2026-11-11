//! Live check against the real DISM: `cargo run -p gc-dism --example probe -- <wim> <new mount folder>`
//! Mounts read/write (creating the folder), lists mounts, writes a file, commits, unmounts (discard).

use std::path::PathBuf;

fn run(op: gc_dism::Op) -> bool {
    println!("== {}", op.title());
    let r = gc_dism::spawn(op, || {});
    loop {
        let (out, pct, done) = r.snapshot();
        if let Some(d) = done {
            println!("{out}\n-> {d:?} (last percent {pct:?})");
            return d.is_ok();
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (wim, dir) = (PathBuf::from(&a[1]), PathBuf::from(&a[2]));
    println!("elevated: {}", gc_dism::is_elevated());
    println!("images: {:?}", gc_dism::wim_images(&wim));
    if !run(gc_dism::Op::Mount { wim: wim.clone(), index: 1, dir: dir.clone(), read_only: false }) {
        return;
    }
    println!("mounts: {:?}", gc_dism::query_mounts());
    println!("problem for the same dir now: {:?}", gc_dism::mount_dir_problem(&dir, &gc_dism::query_mounts().0));
    let _ = std::fs::write(dir.join("gc_probe.txt"), b"hello");
    run(gc_dism::Op::Commit { dir: dir.clone() });
    run(gc_dism::Op::UnmountDiscard { dir: dir.clone() });
    println!("mounts after: {:?}", gc_dism::query_mounts());
}
