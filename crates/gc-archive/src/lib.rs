//! Browse and extract archives as if they were folders. Formats: zip, 7z (read-only for now).
//!
//! An archive is read once into an [`ArchiveIndex`] (the "central directory"); folder listings
//! are then derived from that index without touching the file again.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gc_core::jobs::{self, Job, Progress};
use gc_core::Entry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Zip,
    SevenZip,
}

fn kind_of(path: &Path) -> Option<Kind> {
    match path.extension()?.to_string_lossy().to_ascii_lowercase().as_str() {
        "zip" => Some(Kind::Zip),
        "7z" => Some(Kind::SevenZip),
        _ => None,
    }
}

/// True when the file looks like an archive Ghost Commander can open (by extension).
pub fn is_archive(path: &Path) -> bool {
    kind_of(path).is_some()
}

#[derive(Debug, Clone)]
struct ArchiveEntry {
    /// Normalized path inside the archive: `/` separators, no leading or trailing slash.
    path: String,
    is_dir: bool,
    size: u64,
    modified: Option<SystemTime>,
    /// Position in the zip central directory (unused for 7z).
    zip_index: usize,
}

#[derive(Debug)]
pub struct ArchiveIndex {
    pub file: PathBuf,
    kind: Kind,
    entries: Vec<ArchiveEntry>,
}

/// Normalizes an entry name; `None` for names that could escape the destination folder.
fn normalize(name: &str) -> Option<String> {
    let name = name.replace('\\', "/");
    let mut parts = Vec::new();
    for part in name.split('/') {
        match part {
            "" | "." => {}
            ".." => return None,
            p if p.contains(':') => return None,
            p => parts.push(p),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

pub fn open(file: &Path) -> io::Result<ArchiveIndex> {
    let kind = kind_of(file).ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "unsupported archive type"))?;
    let entries = match kind {
        Kind::Zip => read_zip_index(file)?,
        Kind::SevenZip => read_7z_index(file)?,
    };
    Ok(ArchiveIndex { file: file.to_path_buf(), kind, entries })
}

fn read_zip_index(file: &Path) -> io::Result<Vec<ArchiveEntry>> {
    let mut zip = zip::ZipArchive::new(BufReader::new(File::open(file)?)).map_err(io::Error::other)?;
    let mut entries = Vec::with_capacity(zip.len());
    for i in 0..zip.len() {
        let f = zip.by_index_raw(i).map_err(io::Error::other)?;
        let Some(path) = f.name().ok().and_then(|n| normalize(&n)) else { continue };
        entries.push(ArchiveEntry { path, is_dir: f.is_dir(), size: f.size(), modified: f.last_modified().and_then(zip_time), zip_index: i });
    }
    Ok(entries)
}

fn zip_time(t: zip::DateTime) -> Option<SystemTime> {
    use chrono::{Local, TimeZone};
    Local
        .with_ymd_and_hms(t.year().into(), t.month().into(), t.day().into(), t.hour().into(), t.minute().into(), t.second().into())
        .single()
        .map(SystemTime::from)
}

fn read_7z_index(file: &Path) -> io::Result<Vec<ArchiveEntry>> {
    let reader = sevenz_rust2::ArchiveReader::new(BufReader::new(File::open(file)?), sevenz_rust2::Password::empty())
        .map_err(|e| io::Error::other(e.to_string()))?;
    Ok(reader
        .archive()
        .files
        .iter()
        .filter(|f| !f.is_anti_item)
        .filter_map(|f| {
            let modified = f.has_last_modified_date.then(|| nt_time(u64::from(f.last_modified_date))).flatten();
            Some(ArchiveEntry { path: normalize(&f.name)?, is_dir: f.is_directory, size: f.size, modified, zip_index: 0 })
        })
        .collect())
}

/// Windows FILETIME (100 ns since 1601) to `SystemTime`; `None` outside the representable range.
fn nt_time(ticks: u64) -> Option<SystemTime> {
    const UNIX_EPOCH_TICKS: u64 = 116_444_736_000_000_000;
    let since_unix = ticks.checked_sub(UNIX_EPOCH_TICKS)?;
    UNIX_EPOCH.checked_add(Duration::from_nanos(since_unix.checked_mul(100)?))
}

impl ArchiveIndex {
    /// Direct children of `dir` (`""` is the archive root). Folders that only exist
    /// implicitly (as a prefix of a file path) are listed too.
    pub fn list(&self, dir: &str) -> Vec<Entry> {
        let prefix = if dir.is_empty() { String::new() } else { format!("{dir}/") };
        let mut children: BTreeMap<&str, Entry> = BTreeMap::new();
        for e in &self.entries {
            let Some(rest) = e.path.strip_prefix(&prefix) else { continue };
            let (name, nested) = match rest.split_once('/') {
                Some((name, _)) => (name, true),
                None => (rest, false),
            };
            let is_dir = nested || e.is_dir;
            let child = Entry {
                name: name.to_owned(),
                path: self.file.join(format!("{prefix}{name}")),
                is_dir,
                size: if is_dir { 0 } else { e.size },
                modified: if nested { None } else { e.modified },
            };
            // An explicit entry wins over an implicit one (it carries the date).
            children.entry(name).and_modify(|c| if !nested { *c = child.clone() }).or_insert(child);
        }
        let mut list: Vec<Entry> = children.into_values().collect();
        list.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        list
    }

    /// Selected paths plus everything below them, each with its destination path relative
    /// to the extraction folder (the selected item's own name is kept).
    fn expand(&self, selected: &[String]) -> Vec<Item> {
        let mut out = Vec::new();
        for sel in selected {
            let parent_len = sel.rfind('/').map_or(0, |i| i + 1);
            let below = format!("{sel}/");
            let rel_of = |path: &str| path[parent_len..].split('/').collect::<PathBuf>();
            for e in &self.entries {
                if e.path == *sel || e.path.starts_with(&below) {
                    out.push(Item { path: e.path.clone(), is_dir: e.is_dir, size: e.size, zip_index: e.zip_index, rel: rel_of(&e.path) });
                }
            }
            // A folder that exists only implicitly (no entry of its own) still has to be created.
            if !self.entries.iter().any(|e| e.path == *sel) {
                out.push(Item { path: sel.clone(), is_dir: true, size: 0, zip_index: 0, rel: rel_of(sel) });
            }
        }
        out
    }

    /// Extracts `selected` (inner paths) into `dest_dir`. Errors per item go to `p`.
    pub fn extract(&self, selected: &[String], dest_dir: &Path, p: &Progress) {
        let items = self.expand(selected);
        p.total.store(items.iter().filter(|i| !i.is_dir).map(|i| i.size).sum(), std::sync::atomic::Ordering::Relaxed);
        match self.kind {
            Kind::Zip => self.extract_zip(&items, dest_dir, p),
            Kind::SevenZip => self.extract_7z(&items, dest_dir, p),
        }
    }

    fn extract_zip(&self, items: &[Item], dest_dir: &Path, p: &Progress) {
        let mut zip = match File::open(&self.file).map_err(io::Error::other).and_then(|f| zip::ZipArchive::new(BufReader::new(f)).map_err(io::Error::other)) {
            Ok(z) => z,
            Err(e) => return p.fail(format!("{}: {e}", self.file.display())),
        };
        for entry in items {
            let rel = &entry.rel;
            if p.cancelled() {
                return;
            }
            let target = dest_dir.join(rel);
            let result = if entry.is_dir {
                fs::create_dir_all(&target)
            } else {
                prepare_parent(&target).and_then(|_| {
                    let mut f = zip.by_index(entry.zip_index).map_err(io::Error::other)?;
                    jobs::copy_stream(&mut f, entry.size, &target, p).map(|_| ())
                })
            };
            if let Err(e) = result {
                p.fail(format!("{}: {e}", entry.path));
            }
        }
    }

    fn extract_7z(&self, items: &[Item], dest_dir: &Path, p: &Progress) {
        let mut wanted: HashMap<&str, &PathBuf> = HashMap::new();
        for entry in items {
            let rel = &entry.rel;
            if entry.is_dir {
                if let Err(e) = fs::create_dir_all(dest_dir.join(rel)) {
                    p.fail(format!("{}: {e}", entry.path));
                }
            } else {
                wanted.insert(entry.path.as_str(), rel);
            }
        }
        let open = File::open(&self.file)
            .map_err(|e| e.to_string())
            .and_then(|f| sevenz_rust2::ArchiveReader::new(BufReader::new(f), sevenz_rust2::Password::empty()).map_err(|e| e.to_string()));
        let mut reader = match open {
            Ok(r) => r,
            Err(e) => return p.fail(format!("{}: {e}", self.file.display())),
        };
        let result = reader.for_each_entries(|entry, data| {
            if p.cancelled() {
                return Ok(false);
            }
            let Some(rel) = normalize(&entry.name).and_then(|n| wanted.get(n.as_str()).copied()) else { return Ok(true) };
            let target = dest_dir.join(rel);
            if let Err(e) = prepare_parent(&target).and_then(|_| jobs::copy_stream(data, entry.size, &target, p).map(|_| ())) {
                p.fail(format!("{}: {e}", entry.name));
            }
            Ok(true)
        });
        if let Err(e) = result {
            p.fail(format!("{}: {e}", self.file.display()));
        }
    }
}

fn prepare_parent(target: &Path) -> io::Result<()> {
    target.parent().map_or(Ok(()), fs::create_dir_all)
}

/// Starts extraction on a worker thread (progress, cancel and conflict dialog included).
pub fn spawn_extract(index: std::sync::Arc<ArchiveIndex>, selected: Vec<String>, dest_dir: PathBuf) -> Job {
    let label = format!("Extract {} item(s)", selected.len());
    jobs::spawn_with(label, move |p| index.extract(&selected, &dest_dir, p))
}

/// One file or folder to extract, with its destination relative to the target folder.
struct Item {
    path: String,
    is_dir: bool,
    size: u64,
    zip_index: usize,
    rel: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::Ordering;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gc_archive_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// zip with files only (folders are implicit) plus one path-traversal entry.
    fn make_zip(path: &Path) {
        let mut w = zip::ZipWriter::new(File::create(path).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        for (name, data) in [("a.txt", "A"), ("docs/b.txt", "B"), ("docs/deep/c.txt", "C"), ("../evil.txt", "X")] {
            w.start_file(name, opts).unwrap();
            w.write_all(data.as_bytes()).unwrap();
        }
        w.finish().unwrap();
    }

    fn names(list: &[Entry]) -> Vec<(&str, bool)> {
        list.iter().map(|e| (e.name.as_str(), e.is_dir)).collect()
    }

    fn find(dir: &Path, name: &str) -> Option<PathBuf> {
        for e in fs::read_dir(dir).ok()?.flatten() {
            if e.file_name() == name {
                return Some(e.path());
            }
            if e.path().is_dir() {
                if let Some(p) = find(&e.path(), name) {
                    return Some(p);
                }
            }
        }
        None
    }

    #[test]
    fn zip_listing_derives_folders_and_hides_unsafe_names() {
        let dir = scratch("list");
        make_zip(&dir.join("t.zip"));
        let idx = open(&dir.join("t.zip")).unwrap();

        assert_eq!(names(&idx.list("")), [("docs", true), ("a.txt", false)]);
        assert_eq!(names(&idx.list("docs")), [("deep", true), ("b.txt", false)]);
        assert_eq!(names(&idx.list("docs/deep")), [("c.txt", false)]);
        assert!(idx.list("").iter().all(|e| e.name != "evil.txt"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn zip_extract_folder_keeps_its_name_and_structure() {
        let dir = scratch("zipx");
        make_zip(&dir.join("t.zip"));
        let idx = open(&dir.join("t.zip")).unwrap();
        let dest = dir.join("out");
        fs::create_dir(&dest).unwrap();

        let p = Progress::default();
        idx.extract(&["docs".to_owned()], &dest, &p);

        assert!(p.errors().is_empty(), "{:?}", p.errors());
        assert_eq!(fs::read_to_string(dest.join("docs/b.txt")).unwrap(), "B");
        assert_eq!(fs::read_to_string(dest.join("docs/deep/c.txt")).unwrap(), "C");
        assert!(!dest.join("a.txt").exists());
        assert!(!dir.join("evil.txt").exists());
        assert_eq!(p.done.load(Ordering::Relaxed), p.total.load(Ordering::Relaxed));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn zip_extract_single_nested_file_lands_in_destination_root() {
        let dir = scratch("zip1");
        make_zip(&dir.join("t.zip"));
        let idx = open(&dir.join("t.zip")).unwrap();
        let dest = dir.join("out");
        fs::create_dir(&dest).unwrap();

        let p = Progress::default();
        idx.extract(&["docs/deep/c.txt".to_owned()], &dest, &p);

        assert!(p.errors().is_empty());
        assert_eq!(fs::read_to_string(dest.join("c.txt")).unwrap(), "C");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sevenz_round_trip() {
        let dir = scratch("7z");
        let src = dir.join("src");
        fs::create_dir_all(src.join("docs/deep")).unwrap();
        fs::write(src.join("a.txt"), "A").unwrap();
        fs::write(src.join("docs/deep/c.txt"), "C").unwrap();
        sevenz_rust2::compress_to_path(&src, dir.join("t.7z")).unwrap();

        let idx = open(&dir.join("t.7z")).unwrap();
        let top: Vec<String> = idx.list("").into_iter().map(|e| e.name).collect();
        let dest = dir.join("out");
        fs::create_dir(&dest).unwrap();
        let p = Progress::default();
        idx.extract(&top, &dest, &p);

        assert!(p.errors().is_empty(), "{:?}", p.errors());
        assert_eq!(fs::read_to_string(find(&dest, "c.txt").unwrap()).unwrap(), "C");
        assert_eq!(fs::read_to_string(find(&dest, "a.txt").unwrap()).unwrap(), "A");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn normalize_rejects_traversal_and_drive_tricks() {
        assert_eq!(normalize("a\\b/./c/").as_deref(), Some("a/b/c"));
        assert_eq!(normalize("../x"), None);
        assert_eq!(normalize("a/../x"), None);
        assert_eq!(normalize("C:/x"), None);
        assert_eq!(normalize("/"), None);
    }

    #[test]
    fn only_known_extensions_are_archives() {
        assert!(is_archive(Path::new("x.ZIP")));
        assert!(is_archive(Path::new("x.7z")));
        assert!(!is_archive(Path::new("x.txt")));
    }
}
