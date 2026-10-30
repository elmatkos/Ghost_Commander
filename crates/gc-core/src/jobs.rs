//! Background file operations (copy / move / delete) with progress, cancellation
//! and interactive conflict resolution.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

const BUF_SIZE: usize = 1024 * 1024;

#[derive(Debug, Clone)]
pub enum Op {
    Copy { srcs: Vec<PathBuf>, dest_dir: PathBuf },
    Move { srcs: Vec<PathBuf>, dest_dir: PathBuf },
    /// `permanent == false` sends the items to the recycle bin.
    Delete { srcs: Vec<PathBuf>, permanent: bool },
}

impl Op {
    pub fn label(&self) -> String {
        let (verb, n) = match self {
            Op::Copy { srcs, .. } => ("Copy", srcs.len()),
            Op::Move { srcs, .. } => ("Move", srcs.len()),
            Op::Delete { srcs, permanent: true } => ("Delete", srcs.len()),
            Op::Delete { srcs, permanent: false } => ("Recycle", srcs.len()),
        };
        format!("{verb} {n} item(s)")
    }
}

/// What to do when the target file already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    Overwrite,
    Skip,
    Rename,
    Cancel,
}

/// Shared state between the worker thread and the UI.
#[derive(Default)]
pub struct Progress {
    pub total: AtomicU64,
    pub done: AtomicU64,
    pub finished: AtomicBool,
    pub cancel: AtomicBool,
    errors: Mutex<Vec<String>>,
    /// Sticky answer after "apply to all".
    sticky: Mutex<Option<Resolution>>,
    /// Target the worker is waiting on, and the UI's answer to it.
    prompt: Mutex<Option<PathBuf>>,
    answer: Mutex<Option<(Resolution, bool)>>,
    answered: Condvar,
    /// Held while asking, so parallel workers queue up behind one prompt at a time.
    ask_turn: Mutex<()>,
}

impl Progress {
    pub fn errors(&self) -> Vec<String> {
        self.errors.lock().unwrap().clone()
    }

    /// Existing target the worker is blocked on, if any.
    pub fn pending_conflict(&self) -> Option<PathBuf> {
        self.prompt.lock().unwrap().clone()
    }

    /// Answers the pending conflict; `apply_to_all` reuses the answer for the rest of the job.
    pub fn resolve(&self, resolution: Resolution, apply_to_all: bool) {
        *self.answer.lock().unwrap() = Some((resolution, apply_to_all));
        self.answered.notify_all();
    }

    pub fn fail(&self, msg: String) {
        self.errors.lock().unwrap().push(msg);
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Called by the worker: blocks until the UI answers (or the job is cancelled).
    fn ask_conflict(&self, target: &Path) -> Resolution {
        let _turn = self.ask_turn.lock().unwrap();
        if let Some(r) = *self.sticky.lock().unwrap() {
            return r;
        }
        if self.cancelled() {
            return Resolution::Cancel;
        }
        *self.prompt.lock().unwrap() = Some(target.to_path_buf());
        let mut answer = self.answer.lock().unwrap();
        let (resolution, all) = loop {
            if let Some(a) = answer.take() {
                break a;
            }
            if self.cancelled() {
                break (Resolution::Cancel, false);
            }
            answer = self.answered.wait_timeout(answer, Duration::from_millis(100)).unwrap().0;
        };
        drop(answer);
        *self.prompt.lock().unwrap() = None;
        if resolution == Resolution::Cancel {
            self.cancel.store(true, Ordering::Relaxed);
        } else if all {
            *self.sticky.lock().unwrap() = Some(resolution);
        }
        resolution
    }
}

pub struct Job {
    pub label: String,
    pub progress: Arc<Progress>,
}

/// Starts the operation on a worker thread.
pub fn spawn(op: Op) -> Job {
    let label = op.label();
    spawn_with(label, move |p| run(&op, p))
}

/// Starts any work on a worker thread with the same progress / cancel / conflict plumbing.
pub fn spawn_with(label: String, work: impl FnOnce(&Progress) + Send + 'static) -> Job {
    let progress = Arc::new(Progress::default());
    let job = Job { label, progress: progress.clone() };
    thread::spawn(move || {
        work(&progress);
        progress.finished.store(true, Ordering::Release);
    });
    job
}

/// Runs the operation on the current thread. Errors are collected in `progress`.
pub fn run(op: &Op, p: &Progress) {
    let srcs = match op {
        Op::Copy { srcs, .. } | Op::Move { srcs, .. } | Op::Delete { srcs, .. } => srcs,
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
            Op::Delete { permanent, .. } => delete_tree(src, *permanent, p),
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

fn copy_file(src: &Path, dst: &Path, p: &Progress) -> io::Result<()> {
    let mut input = File::open(src)?;
    let size = input.metadata()?.len();
    if let Some(written) = copy_stream(&mut input, size, dst, p)? {
        if let Ok(meta) = fs::metadata(src) {
            let _ = fs::set_permissions(&written, meta.permissions());
        }
    }
    Ok(())
}

/// Writes `input` (`size` bytes) to `dst`; an existing target goes through the conflict dialog.
/// Returns the path actually written, or `None` when the file was skipped or the job cancelled.
pub fn copy_stream(input: &mut dyn Read, size: u64, dst: &Path, p: &Progress) -> io::Result<Option<PathBuf>> {
    let mut target = dst.to_path_buf();
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    let mut output = match options.open(&target) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && !target.is_dir() => {
            match p.ask_conflict(&target) {
                Resolution::Skip => {
                    p.done.fetch_add(size, Ordering::Relaxed);
                    return Ok(None);
                }
                Resolution::Cancel => return Ok(None),
                Resolution::Overwrite => OpenOptions::new().write(true).create(true).truncate(true).open(&target)?,
                Resolution::Rename => {
                    target = unique_name(&target);
                    options.open(&target)?
                }
            }
        }
        Err(e) => return Err(e),
    };
    let mut buf = vec![0u8; BUF_SIZE];
    loop {
        if p.cancelled() {
            drop(output);
            let _ = fs::remove_file(&target);
            return Ok(None);
        }
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        output.write_all(&buf[..n])?;
        p.done.fetch_add(n as u64, Ordering::Relaxed);
    }
    output.flush()?;
    Ok(Some(target))
}

/// `name.ext` -> first free `name (2).ext`, `name (3).ext`, ...
fn unique_name(path: &Path) -> PathBuf {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (2..)
        .map(|n| path.with_file_name(format!("{stem} ({n}){ext}")))
        .find(|c| fs::symlink_metadata(c).is_err())
        .unwrap()
}

fn move_tree(src: &Path, dst: &Path, p: &Progress) -> io::Result<()> {
    let src_is_dir = fs::symlink_metadata(src)?.is_dir();
    let mut dst = dst.to_path_buf();
    if let Ok(existing) = fs::symlink_metadata(&dst) {
        match (src_is_dir, existing.is_dir()) {
            // Folder onto folder: merge, then drop the emptied source.
            (true, true) => {
                for entry in fs::read_dir(src)? {
                    if p.cancelled() {
                        return Ok(());
                    }
                    let entry = entry?;
                    move_tree(&entry.path(), &dst.join(entry.file_name()), p)?;
                }
                return if p.cancelled() { Ok(()) } else { fs::remove_dir(src) };
            }
            (false, false) => match p.ask_conflict(&dst) {
                Resolution::Skip => {
                    p.done.fetch_add(existing.len(), Ordering::Relaxed);
                    return Ok(());
                }
                Resolution::Cancel => return Ok(()),
                Resolution::Overwrite => fs::remove_file(&dst)?,
                Resolution::Rename => dst = unique_name(&dst),
            },
            _ => return Err(io::Error::new(io::ErrorKind::AlreadyExists, "a file and a folder share the name")),
        }
    }
    let size = tree_size(src);
    if fs::rename(src, &dst).is_ok() {
        p.done.fetch_add(size, Ordering::Relaxed);
        return Ok(());
    }
    // Different volume: copy, then delete the source only if the copy finished.
    copy_tree(src, &dst, p)?;
    if p.cancelled() {
        return Ok(());
    }
    if src_is_dir {
        fs::remove_dir_all(src)
    } else {
        fs::remove_file(src)
    }
}

fn delete_tree(path: &Path, permanent: bool, p: &Progress) -> io::Result<()> {
    let size = tree_size(path);
    if !permanent {
        trash::delete(path).map_err(|e| io::Error::other(e.to_string()))?;
    } else if fs::symlink_metadata(path)?.is_dir() {
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

    /// Runs `op` on a worker thread and answers every conflict prompt with `answer`.
    fn run_answering(op: Op, answer: Resolution, all: bool) -> (Arc<Progress>, usize) {
        let p = Arc::new(Progress::default());
        let worker = {
            let p = p.clone();
            thread::spawn(move || {
                run(&op, &p);
                p.finished.store(true, Ordering::Release);
            })
        };
        let mut prompts = 0;
        while !p.finished.load(Ordering::Acquire) {
            if p.pending_conflict().is_some() {
                prompts += 1;
                p.resolve(answer, all);
                // Wait until the worker consumed this prompt.
                while p.pending_conflict().is_some() && !p.finished.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(5));
                }
            }
            thread::sleep(Duration::from_millis(5));
        }
        worker.join().unwrap();
        (p, prompts)
    }

    fn conflict_setup(tag: &str) -> (PathBuf, Op, PathBuf) {
        let root = scratch(tag);
        let src = sample_tree(&root);
        let dest = root.join("dest");
        fs::create_dir_all(dest.join("src/sub")).unwrap();
        fs::write(dest.join("src/a.txt"), b"keep").unwrap();
        let op = Op::Copy { srcs: vec![src], dest_dir: dest.clone() };
        (root, op, dest)
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
    fn conflict_skip_keeps_existing_file() {
        let (root, op, dest) = conflict_setup("skip");
        let (p, prompts) = run_answering(op, Resolution::Skip, false);
        assert!(p.errors().is_empty());
        assert_eq!(prompts, 1);
        assert_eq!(fs::read(dest.join("src/a.txt")).unwrap(), b"keep");
        assert!(dest.join("src/sub/b.bin").exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn conflict_overwrite_replaces_file() {
        let (root, op, dest) = conflict_setup("over");
        let (p, _) = run_answering(op, Resolution::Overwrite, false);
        assert!(p.errors().is_empty());
        assert_eq!(fs::read(dest.join("src/a.txt")).unwrap(), b"hello");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn conflict_rename_keeps_both() {
        let (root, op, dest) = conflict_setup("rename");
        let (p, _) = run_answering(op, Resolution::Rename, false);
        assert!(p.errors().is_empty());
        assert_eq!(fs::read(dest.join("src/a.txt")).unwrap(), b"keep");
        assert_eq!(fs::read(dest.join("src/a (2).txt")).unwrap(), b"hello");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn conflict_cancel_stops_the_job() {
        let (root, op, dest) = conflict_setup("cancelc");
        let (p, _) = run_answering(op, Resolution::Cancel, false);
        assert!(p.cancel.load(Ordering::Relaxed));
        assert_eq!(fs::read(dest.join("src/a.txt")).unwrap(), b"keep");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn apply_to_all_asks_only_once() {
        let root = scratch("all");
        let src = root.join("src");
        let dest = root.join("dest");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(dest.join("src")).unwrap();
        for n in ["1.txt", "2.txt", "3.txt"] {
            fs::write(src.join(n), b"new").unwrap();
            fs::write(dest.join("src").join(n), b"old").unwrap();
        }
        let op = Op::Copy { srcs: vec![src], dest_dir: dest.clone() };
        let (_, prompts) = run_answering(op, Resolution::Overwrite, true);
        assert_eq!(prompts, 1);
        for n in ["1.txt", "2.txt", "3.txt"] {
            assert_eq!(fs::read(dest.join("src").join(n)).unwrap(), b"new");
        }
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
    fn move_and_permanent_delete() {
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
        run(&Op::Delete { srcs: vec![dest.join("src")], permanent: true }, &p);
        assert!(p.errors().is_empty());
        assert!(!dest.join("src").exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn recycle_removes_item_from_original_location() {
        let root = scratch("recycle");
        let file = root.join("to_recycle.txt");
        fs::write(&file, b"bye").unwrap();

        let p = Progress::default();
        run(&Op::Delete { srcs: vec![file.clone()], permanent: false }, &p);

        assert!(p.errors().is_empty(), "{:?}", p.errors());
        assert!(!file.exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn move_folder_onto_existing_folder_merges() {
        let root = scratch("merge");
        let src = sample_tree(&root);
        let dest = root.join("dest");
        fs::create_dir_all(dest.join("src")).unwrap();
        fs::write(dest.join("src/other.txt"), b"x").unwrap();

        let p = Progress::default();
        run(&Op::Move { srcs: vec![src.clone()], dest_dir: dest.clone() }, &p);
        assert!(p.errors().is_empty());
        assert!(!src.exists());
        assert!(dest.join("src/other.txt").exists());
        assert!(dest.join("src/a.txt").exists());
        assert!(dest.join("src/sub/b.bin").exists());
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
