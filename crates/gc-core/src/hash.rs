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

/// Writes `<file>.crc` next to `file` (e.g. `name.ext` -> `name.ext.crc`) in SFV format:
/// `name.ext 1A2B3C4D`. Returns the path written.
pub fn write_crc_file(file: &std::path::Path, d: &Digests) -> io::Result<PathBuf> {
    let name = file.file_name().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
    let mut target = file.as_os_str().to_owned();
    target.push(".crc");
    let target = PathBuf::from(target);
    std::fs::write(&target, format!("{} {}\r\n", name.to_string_lossy(), d.crc32.to_uppercase()))?;
    Ok(target)
}

impl Digests {
    /// The checksum whose hex length is `len` (8 CRC-32, 32 MD5, 40 SHA-1, 64 SHA-256).
    pub fn by_len(&self, len: usize) -> Option<&str> {
        match len {
            8 => Some(&self.crc32),
            32 => Some(&self.md5),
            40 => Some(&self.sha1),
            64 => Some(&self.sha256),
            _ => None,
        }
    }
}

/// One line of a checksum file: the file it names and the expected checksum (lower case hex).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expected {
    pub name: String,
    pub value: String,
}

/// True for the checksum file types we can verify (`.crc .sfv .md5 .sha1 .sha256`).
pub fn is_checksum_file(path: &std::path::Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| ["crc", "sfv", "md5", "sha1", "sha256"].iter().any(|x| e.eq_ignore_ascii_case(x)))
}

fn is_digest(s: &str) -> bool {
    matches!(s.len(), 8 | 32 | 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Parses SFV (`name CRC`), `md5sum`/`sha256sum` (`hash  name`, `hash *name`) and bare-hash files.
/// `default_name` names the file when a line holds only a hash.
pub fn parse_checksum_file(text: &str, default_name: &str) -> Vec<Expected> {
    let mut out = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with(';') && !l.starts_with('#')) {
        let (first, rest) = line.split_once(char::is_whitespace).map_or((line, ""), |(a, b)| (a, b.trim_start()));
        let item = if is_digest(first) {
            let name = rest.strip_prefix('*').unwrap_or(rest);
            Some((if name.is_empty() { default_name } else { name }, first))
        } else {
            line.rsplit_once(char::is_whitespace).filter(|(_, crc)| is_digest(crc)).map(|(name, crc)| (name.trim(), crc))
        };
        if let Some((name, value)) = item {
            out.push(Expected { name: name.to_string(), value: value.to_lowercase() });
        }
    }
    out
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
    fn parses_checksum_files() {
        let sfv = "; made by x\r\nfine.name.ext 352441C2\r\nother file.bin  DEADBEEF\r\n";
        let v = parse_checksum_file(sfv, "d");
        assert_eq!(v, vec![Expected { name: "fine.name.ext".into(), value: "352441c2".into() }, Expected { name: "other file.bin".into(), value: "deadbeef".into() }]);
        let sum = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad *a.txt\n900150983cd24fb0d6963f7d28e17f72  b c.txt\n";
        let v = parse_checksum_file(sum, "d");
        assert_eq!((v[0].name.as_str(), v[0].value.len(), v[1].name.as_str()), ("a.txt", 64, "b c.txt"));
        let bare = parse_checksum_file("A9993E364706816ABA3E25717850C26C9CD0D89D\n", "x.iso");
        assert_eq!(bare[0].name, "x.iso");
        assert!(parse_checksum_file("garbage line here", "d").is_empty());
        assert!(is_checksum_file(std::path::Path::new("a.SFV")) && !is_checksum_file(std::path::Path::new("a.txt")));
    }
    #[test]
    fn crc_file_is_named_after_source() {
        let dir = std::env::temp_dir().join(format!("gc-crc-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("fine.name.ext");
        let out = write_crc_file(&src, &digest(b"abc")).unwrap();
        assert_eq!(out, dir.join("fine.name.ext.crc"));
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "fine.name.ext 352441C2\r\n");
        std::fs::remove_dir_all(&dir).unwrap();
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
