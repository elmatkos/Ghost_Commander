//! Checksums: CRC-32, MD5, SHA-1 and SHA-256 of files, all computed in one read pass.
//! Several files are hashed in parallel on a few worker threads. The hash crates pick
//! hardware instructions (SHA-NI, PCLMULQDQ) at run time and fall back to scalar code.

use std::fs::File;
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use md5::Md5;
use sha1::Sha1;
use sha2::{Digest, Sha256};

const BUF: usize = 1 << 20;

/// The four checksums of one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Digests {
    pub crc32: String,
    pub md5: String,
    pub sha1: String,
    pub sha256: String,
}

impl Digests {
    /// True if `text` (any case, spaces ignored) equals one of the checksums.
    pub fn matches(&self, text: &str) -> bool {
        let t: String = text.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_lowercase();
        !t.is_empty() && [&self.crc32, &self.md5, &self.sha1, &self.sha256].iter().any(|d| **d == t)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Reads everything from `r`, adding the byte count to `done`. `None` when cancelled.
pub fn hash_reader<R: Read>(mut r: R, done: &AtomicU64, cancel: &AtomicBool) -> io::Result<Option<Digests>> {
    let mut crc = crc32fast::Hasher::new();
    let (mut md5, mut sha1, mut sha256) = (Md5::new(), Sha1::new(), Sha256::new());
    let mut buf = vec![0u8; BUF];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let chunk = &buf[..n];
        crc.update(chunk);
        md5.update(chunk);
        sha1.update(chunk);
        sha256.update(chunk);
        done.fetch_add(n as u64, Ordering::Relaxed);
    }
    Ok(Some(Digests {
        crc32: format!("{:08x}", crc.finalize()),
        md5: hex(&md5.finalize()),
        sha1: hex(&sha1.finalize()),
        sha256: hex(&sha256.finalize()),
    }))
}

/// One file of a [`HashJob`].
pub struct FileHash {
    pub path: PathBuf,
    pub size: u64,
    /// Filled in by a worker: the checksums, or the error text.
    pub result: Mutex<Option<Result<Digests, String>>>,
}

/// Checksums of a list of files, running in the background. Poll `done`/`finished`.
pub struct HashJob {
    pub files: Vec<FileHash>,
    pub total: u64,
    pub done: AtomicU64,
    pub finished: AtomicUsize,
    pub cancel: AtomicBool,
}

impl HashJob {
    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire) >= self.files.len() || self.cancel.load(Ordering::Relaxed)
    }

    pub fn percent(&self) -> f32 {
        if self.total == 0 {
            return 1.0;
        }
        (self.done.load(Ordering::Relaxed) as f32 / self.total as f32).min(1.0)
    }
}

/// Starts hashing `paths` (plain files; others report an error) and returns at once.
/// `on_progress` is called from the workers now and then (e.g. to request a repaint).
pub fn spawn(paths: Vec<PathBuf>, on_progress: impl Fn() + Send + Sync + 'static) -> Arc<HashJob> {
    let files: Vec<FileHash> = paths
        .into_iter()
        .map(|path| {
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            FileHash { path, size, result: Mutex::new(None) }
        })
        .collect();
    let total = files.iter().map(|f| f.size).sum();
    let job = Arc::new(HashJob { files, total, done: AtomicU64::new(0), finished: AtomicUsize::new(0), cancel: AtomicBool::new(false) });
    let workers = std::thread::available_parallelism().map_or(2, |n| n.get()).min(job.files.len()).min(8);
    let next = Arc::new(AtomicUsize::new(0));
    let on_progress = Arc::new(on_progress);
    for _ in 0..workers {
        let (job, next, on_progress) = (job.clone(), next.clone(), on_progress.clone());
        std::thread::spawn(move || loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            let Some(file) = job.files.get(i) else { break };
            let outcome = match File::open(&file.path).and_then(|f| hash_reader(f, &job.done, &job.cancel)) {
                Ok(Some(d)) => Some(Ok(d)),
                Ok(None) => None,
                Err(e) => Some(Err(e.to_string())),
            };
            if let Some(r) = outcome {
                *file.result.lock().unwrap() = Some(r);
            }
            job.finished.fetch_add(1, Ordering::Release);
            on_progress();
        });
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(data: &[u8]) -> Digests {
        hash_reader(data, &AtomicU64::new(0), &AtomicBool::new(false)).unwrap().unwrap()
    }

    #[test]
    fn known_vectors() {
        let d = digest(b"abc");
        assert_eq!(d.crc32, "352441c2");
        assert_eq!(d.md5, "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(d.sha1, "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(d.sha256, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn empty_input_and_match() {
        let d = digest(b"");
        assert_eq!(d.crc32, "00000000");
        assert!(d.matches("  E3B0C442 98FC1C14 9AFBF4C8 996FB924 27AE41E4 649B934C A495991B 7852B855 "));
        assert!(!d.matches(""));
        assert!(!d.matches("deadbeef"));
    }

    #[test]
    fn cancelled_gives_none() {
        let r = hash_reader(&b"data"[..], &AtomicU64::new(0), &AtomicBool::new(true)).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn job_hashes_files_in_parallel() {
        let dir = std::env::temp_dir().join(format!("gc-hash-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let paths: Vec<PathBuf> = (0..5)
            .map(|i| {
                let p = dir.join(format!("f{i}.txt"));
                std::fs::write(&p, b"abc").unwrap();
                p
            })
            .collect();
        let job = spawn(paths, || {});
        while !job.is_finished() {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        for f in &job.files {
            assert_eq!(f.result.lock().unwrap().as_ref().unwrap().as_ref().unwrap().crc32, "352441c2");
        }
        assert_eq!(job.total, 15);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
