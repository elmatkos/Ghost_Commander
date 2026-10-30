//! Background file operations (copy / move / delete) with progress and cancellation.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

const BUF_SIZE: usize = 1024 * 1024;

#[derive(Debug, Clone)]
pub enum Op {
    Copy { srcs: Vec<PathBuf>, dest_dir: PathBuf },
    Move { srcs: Vec<PathBuf>, dest_dir: PathBuf },
    Delete { srcs: Vec<PathBuf> },
}

impl Op {
    pub fn label(&self) -> String {
        let (verb, n) = match self {
            Op::Copy { srcs, .. } => ("Copy", srcs.len()),
            Op::Move { srcs, .. } => ("Move", srcs.len()),
            Op::Delete { srcs } => ("Delete", srcs.len()),
        };
        format!("{verb} {n} item(s)")
    }
}

/// Shared state between the worker thread and the UI.
#[derive(Default)]
pub struct Progress {
    pub total: AtomicU64,
    pub done: AtomicU64,
    pub finished: AtomicBool,
    pub cancel: AtomicBool,
    errors: Mutex<Vec<String>>,
}

impl Progress {
    pub fn errors(&self) -> Vec<String> {
        self.errors.lock().unwrap().clone()
    }

    fn fail(&self, msg: String) {
        self.errors.lock().unwrap().push(msg);
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

pub struct Job {
    pub label: String,
    pub progress: Arc<Progress>,
}

/// Starts the operation on a worker thread.
pub fn spawn(op: Op) -> Job {
    let progress = Arc::new(Progress::default());
    let job = Job { label: op.label(), progress: progress.clone() };
    thread::spawn(move || {
        run(&op, &progress);
        progress.finished.store(true, Ordering::Release);
    });
    job
}

/// Runs the operation on the current thread. Errors are collected in `progress`.
pub fn run(op: &Op, p: &Progress) {
    let srcs = match op {
        Op::Copy { srcs, .. } | Op::Move { srcs, .. } | Op::Delete { srcs } => srcs,
    };
    let total: u64 = srcs.iter().map(|s| tree_size(s)).sum();
    p.total.store(total, Ordering::Relaxed);

    for src in srcs {
        if p.cancelled() {
            return;
        }
        let result = match op {
            Op::Copy { dest_dir, .. } => target_of(src, dest_dir).and_then(|t| copy_tree(src, &t, p)),
            Op::Move { dest_dir, .. } => target_of(src, dest_dir).and_then(|t| move_tree(src, &t, p)),
            Op::Delete { .. } => delete_tree(src, p),
        };
        if let Err(e) = result {
            p.fail(format!("{}: {e}", src.display()));
        }
    }
}

fn target_of(src: &Path, dest_dir: &Path) -> io::Result<PathBuf> {
    let name = src
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
    let target = dest_dir.join(name);
    if src.is_dir() && target.starts_with(src) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "cannot copy a folder into itself"));
    }
    Ok(target)
}

fn tree_size(path: &Path) -> u64 {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => fs::read_dir(path)
            .map(|rd| rd.filter_map(|e| e.ok()).map(|e| tree_size(&e.path())).sum())
            .unwrap_or(0),
        Ok(m) => m.len(),
        Err(_) => 0,
    }
}

fn copy_tree(src: &Path, dst: &Path, p: &Progress) -> io::Result<()> {
    if fs::symlink_metadata(src)?.is_dir() {
        match fs::create_dir(dst) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && dst.is_dir() => {}
            Err(e) => return Err(e),
        }
        for entry in fs::read_dir(src)? {
            if p.cancelled() {
                return Ok(());
            }
            let entry = entry?;
            copy_tree(&entry.path(), &dst.join(entry.file_name()), p)?;
        }
        Ok(())
    } else {
        copy_file(src, dst, p)
    }
}

/// Never overwrites: an existing target is an error.
fn copy_file(src: &Path, dst: &Path, p: &Progress) -> io::Result<()> {
    let mut input = File::open(src)?;
    let mut output = OpenOptions::new().write(true).create_new(true).open(dst)?;
    let mut buf = vec![0u8; BUF_SIZE];
    loop {
        if p.cancelled() {
            drop(output);
            let _ = fs::remove_file(dst);
            return Ok(());
        }
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        output.write_all(&buf[..n])?;
        p.done.fetch_add(n as u64, Ordering::Relaxed);
    }
    output.flush()?;
    if let Ok(meta) = fs::metadata(src) {
        let _ = output.set_permissions(meta.permissions());
    }
    Ok(())
}

fn move_tree(src: &Path, dst: &Path, p: &Progress) -> io::Result<()> {
    if dst.exists() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, "target already exists"));
    }
    let size = tree_size(src);
    if fs::rename(src, dst).is_ok() {
        p.done.fetch_add(size, Ordering::Relaxed);
        return Ok(());
    }
    // Different volume: copy, then delete the source only if the copy finished.
    copy_tree(src, dst, p)?;
    if p.cancelled() {
        return Ok(());
    }
    if fs::symlink_metadata(src)?.is_dir() {
        fs::remove_dir_all(src)
    } else {
        fs::remove_file(src)
    }
}

fn delete_tree(path: &Path, p: &Progress) -> io::Result<()> {
    let size = tree_size(path);
    if fs::symlink_metadata(path)?.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    p.done.fetch_add(size, Ordering::Relaxed);
    Ok(())
}

/// Creates a new folder inside `parent`.
pub fn make_dir(parent: &Path, name: &str) -> io::Result<PathBuf> {
    if name.is_empty() || name.contains(['/', '\\']) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid folder name"));
    }
    let path = parent.join(name);
    fs::create_dir(&path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gc_jobs_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample_tree(root: &Path) -> PathBuf {
        let src = root.join("src");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("a.txt"), b"hello").unwrap();
        fs::write(src.join("sub").join("b.bin"), vec![7u8; 3 * BUF_SIZE / 2]).unwrap();
        src
    }

    #[test]
    fn copy_recursive_keeps_source_and_counts_bytes() {
        let root = scratch("copy");
        let src = sample_tree(&root);
        let dest = root.join("dest");
        fs::create_dir(&dest).unwrap();

        let p = Progress::default();
        run(&Op::Copy { srcs: vec![src.clone()], dest_dir: dest.clone() }, &p);

        assert!(p.errors().is_empty());
        assert_eq!(fs::read(dest.join("src/a.txt")).unwrap(), b"hello");
        assert_eq!(fs::metadata(dest.join("src/sub/b.bin")).unwrap().len(), 3 * BUF_SIZE as u64 / 2);
        assert!(src.join("a.txt").exists());
        assert_eq!(p.done.load(Ordering::Relaxed), p.total.load(Ordering::Relaxed));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn copy_does_not_overwrite() {
        let root = scratch("noover");
        let src = sample_tree(&root);
        let dest = root.join("dest");
        fs::create_dir_all(dest.join("src")).unwrap();
        fs::write(dest.join("src/a.txt"), b"keep").unwrap();

        let p = Progress::default();
        run(&Op::Copy { srcs: vec![src], dest_dir: dest.clone() }, &p);

        assert_eq!(p.errors().len(), 1);
        assert_eq!(fs::read(dest.join("src/a.txt")).unwrap(), b"keep");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn copy_folder_into_itself_is_rejected() {
        let root = scratch("self");
        let src = sample_tree(&root);
        let p = Progress::default();
        run(&Op::Copy { srcs: vec![src.clone()], dest_dir: src.join("sub") }, &p);
        assert_eq!(p.errors().len(), 1);
        assert!(!src.join("sub/src").exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn move_and_delete() {
        let root = scratch("movedel");
        let src = sample_tree(&root);
        let dest = root.join("dest");
        fs::create_dir(&dest).unwrap();

        let p = Progress::default();
        run(&Op::Move { srcs: vec![src.clone()], dest_dir: dest.clone() }, &p);
        assert!(p.errors().is_empty());
        assert!(!src.exists());
        assert!(dest.join("src/sub/b.bin").exists());

        let p = Progress::default();
        run(&Op::Delete { srcs: vec![dest.join("src")] }, &p);
        assert!(p.errors().is_empty());
        assert!(!dest.join("src").exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn cancel_before_start_does_nothing() {
        let root = scratch("cancel");
        let src = sample_tree(&root);
        let dest = root.join("dest");
        fs::create_dir(&dest).unwrap();

        let p = Progress::default();
        p.cancel.store(true, Ordering::Relaxed);
        run(&Op::Copy { srcs: vec![src], dest_dir: dest.clone() }, &p);
        assert_eq!(fs::read_dir(&dest).unwrap().count(), 0);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn make_dir_rejects_bad_names() {
        let root = scratch("mkdir");
        assert!(make_dir(&root, "").is_err());
        assert!(make_dir(&root, "a/b").is_err());
        assert!(make_dir(&root, "ok").unwrap().is_dir());
        assert!(make_dir(&root, "ok").is_err());
        fs::remove_dir_all(&root).unwrap();
    }
}
