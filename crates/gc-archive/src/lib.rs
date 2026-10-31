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
    Tar(Compression),
}

/// Outer compression of a tarball.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Compression {
    None,
    Gzip,
    Xz,
}

fn kind_of(path: &Path) -> Option<Kind> {
    let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    let ends = |suffixes: &[&str]| suffixes.iter().any(|s| name.ends_with(s));
    if ends(&[".zip"]) {
        Some(Kind::Zip)
    } else if ends(&[".7z"]) {
        Some(Kind::SevenZip)
    } else if ends(&[".tar.gz", ".tgz"]) {
        Some(Kind::Tar(Compression::Gzip))
    } else if ends(&[".tar.xz", ".txz"]) {
        Some(Kind::Tar(Compression::Xz))
    } else if ends(&[".tar"]) {
        Some(Kind::Tar(Compression::None))
    } else {
        None
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
        Kind::Tar(c) => read_tar_index(file, c)?,
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

/// A tarball is a stream: the decompressor is always read from the start.
fn tar_archive(file: &Path, compression: Compression) -> io::Result<tar::Archive<Box<dyn io::Read>>> {
    let input = BufReader::new(File::open(file)?);
    let reader: Box<dyn io::Read> = match compression {
        Compression::None => Box::new(input),
        Compression::Gzip => Box::new(flate2::read::MultiGzDecoder::new(input)),
        Compression::Xz => Box::new(lzma_rust2::XzReader::new(input, true)),
    };
    Ok(tar::Archive::new(reader))
}

/// Only regular files and folders are listed (links and devices are skipped).
fn read_tar_index(file: &Path, compression: Compression) -> io::Result<Vec<ArchiveEntry>> {
    let mut archive = tar_archive(file, compression)?;
    let mut entries = Vec::new();
    for entry in archive.entries()? {
        let entry = entry?;
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir()) {
            continue;
        }
        let Some(path) = entry.path().ok().and_then(|p| normalize(&p.to_string_lossy())) else { continue };
        let modified = entry.header().mtime().ok().filter(|&s| s > 0).map(|s| UNIX_EPOCH + Duration::from_secs(s));
        entries.push(ArchiveEntry { path, is_dir: kind.is_dir(), size: entry.size(), modified, zip_index: 0 });
    }
    Ok(entries)
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
            Kind::Tar(c) => self.extract_tar(c, &items, dest_dir, p),
        }
    }

    /// One pass over the stream, writing only the wanted entries.
    fn extract_tar(&self, compression: Compression, items: &[Item], dest_dir: &Path, p: &Progress) {
        let mut wanted: HashMap<&str, &PathBuf> = HashMap::new();
        for entry in items {
            if entry.is_dir {
                if let Err(e) = fs::create_dir_all(dest_dir.join(&entry.rel)) {
                    p.fail(format!("{}: {e}", entry.path));
                }
            } else {
                wanted.insert(entry.path.as_str(), &entry.rel);
            }
        }
        let result = tar_archive(&self.file, compression).and_then(|mut archive| {
            for entry in archive.entries()? {
                if p.cancelled() || wanted.is_empty() {
                    break;
                }
                let mut entry = entry?;
                let Some(rel) = entry.path().ok().and_then(|n| normalize(&n.to_string_lossy())).and_then(|n| wanted.remove(n.as_str())) else { continue };
                let (size, target) = (entry.size(), dest_dir.join(rel));
                if let Err(e) = prepare_parent(&target).and_then(|_| jobs::copy_stream(&mut entry, size, &target, p).map(|_| ())) {
                    p.fail(format!("{}: {e}", target.display()));
                }
            }
            Ok(())
        });
        if let Err(e) = result {
            p.fail(format!("{}: {e}", self.file.display()));
        }
    }

    /// Deflate is CPU-bound and entries are independent, so files are spread over all cores;
    /// every chunk opens its own handle because `ZipArchive` is not shareable.
    fn extract_zip(&self, items: &[Item], dest_dir: &Path, p: &Progress) {
        use rayon::prelude::*;

        for entry in items.iter().filter(|i| i.is_dir) {
            if let Err(e) = fs::create_dir_all(dest_dir.join(&entry.rel)) {
                p.fail(format!("{}: {e}", entry.path));
            }
        }
        let files: Vec<&Item> = items.iter().filter(|i| !i.is_dir).collect();
        let chunk_len = files.len().div_ceil(rayon::current_num_threads() * 4).max(1);
        files.par_chunks(chunk_len).for_each(|chunk| {
            let mut zip = match File::open(&self.file).map_err(io::Error::other).and_then(|f| zip::ZipArchive::new(BufReader::new(f)).map_err(io::Error::other)) {
                Ok(z) => z,
                Err(e) => return p.fail(format!("{}: {e}", self.file.display())),
            };
            for entry in chunk {
                if p.cancelled() {
                    return;
                }
                let target = dest_dir.join(&entry.rel);
                let result = prepare_parent(&target).and_then(|_| {
                    let mut f = zip.by_index(entry.zip_index).map_err(io::Error::other)?;
                    jobs::copy_stream(&mut f, entry.size, &target, p).map(|_| ())
                });
                if let Err(e) = result {
                    p.fail(format!("{}: {e}", entry.path));
                }
            }
        });
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

/// Per-process scratch folder for archives that live inside other archives.
fn nested_root() -> PathBuf {
    std::env::temp_dir().join("Ghost Commander").join(format!("nested-{}", std::process::id()))
}

/// Extracts one file of `parent` (inner path `inner`) into a fresh scratch folder and returns
/// its path, so it can be viewed, edited or opened like any file on disk. Slow for big files,
/// so call it off the UI thread. Scratch folders are removed by [`cleanup_nested`].
pub fn extract_to_scratch(parent: &ArchiveIndex, inner: &str) -> io::Result<PathBuf> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = nested_root().join(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed).to_string());
    fs::create_dir_all(&dir)?;
    let p = Progress::default();
    parent.extract(&[inner.to_owned()], &dir, &p);
    if let Some(e) = p.errors().into_iter().next() {
        return Err(io::Error::other(e));
    }
    Ok(dir.join(inner.rsplit('/').next().unwrap_or(inner)))
}

/// Opens an archive stored inside `parent` (at inner path `inner`) after extracting it to the
/// scratch folder. Call off the UI thread.
pub fn open_nested(parent: &ArchiveIndex, inner: &str) -> io::Result<ArchiveIndex> {
    open(&extract_to_scratch(parent, inner)?)
}

/// Deletes the scratch folder of [`open_nested`]; call on exit.
pub fn cleanup_nested() {
    let _ = fs::remove_dir_all(nested_root());
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackFormat {
    Zip,
    SevenZip,
    TarGz,
    TarXz,
}

impl PackFormat {
    pub const ALL: [PackFormat; 4] = [PackFormat::Zip, PackFormat::SevenZip, PackFormat::TarGz, PackFormat::TarXz];

    pub fn extension(self) -> &'static str {
        match self {
            PackFormat::Zip => "zip",
            PackFormat::SevenZip => "7z",
            PackFormat::TarGz => "tar.gz",
            PackFormat::TarXz => "tar.xz",
        }
    }
}

/// One file or folder to pack; `name` is its `/` path inside the archive.
struct PackItem {
    path: PathBuf,
    name: String,
    is_dir: bool,
    size: u64,
}

/// Lists `path` and everything below it. Symlinks are skipped; unreadable folders are reported.
fn walk(path: &Path, name: String, out: &mut Vec<PackItem>, p: &Progress) {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) => return p.fail(format!("{}: {e}", path.display())),
    };
    if meta.file_type().is_symlink() {
        return;
    }
    if !meta.is_dir() {
        out.push(PackItem { path: path.to_path_buf(), name, is_dir: false, size: meta.len() });
        return;
    }
    out.push(PackItem { path: path.to_path_buf(), name: name.clone(), is_dir: true, size: 0 });
    match fs::read_dir(path) {
        Ok(children) => {
            for child in children.flatten() {
                let child_name = format!("{name}/{}", child.file_name().to_string_lossy());
                walk(&child.path(), child_name, out, p);
            }
        }
        Err(e) => p.fail(format!("{}: {e}", path.display())),
    }
}

/// Counts bytes into the progress and aborts the read when the job is cancelled.
/// Share of the input counted as done while a compressor may still lag behind the reads;
/// the rest is added when the archive is finished, so the bar does not sit at 100% early.
const COMPRESSED_PERCENT: u64 = 90;

/// Size of one independently compressed block (and of the dictionary) for 7z and xz. Every core
/// works on one block, so reads can run ahead of the compressor by about cores x this much:
/// small blocks keep the progress bar close to the real work.
const BLOCK_BYTES: u64 = 2 << 20;

struct CountingReader<'a, R> {
    inner: R,
    p: &'a Progress,
    /// Percent of each read added to the progress (100 = exact).
    percent: u64,
}

impl<R: io::Read> io::Read for CountingReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.p.cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        let n = self.inner.read(buf)?;
        self.p.done.fetch_add(n as u64 * self.percent / 100, std::sync::atomic::Ordering::Relaxed);
        Ok(n)
    }
}

fn zip_datetime(t: SystemTime) -> Option<zip::DateTime> {
    use chrono::{DateTime, Datelike, Local, Timelike};
    let t = DateTime::<Local>::from(t);
    zip::DateTime::from_date_and_time(u16::try_from(t.year()).ok()?, t.month() as u8, t.day() as u8, t.hour() as u8, t.minute() as u8, t.second() as u8).ok()
}

/// Packs `srcs` (files or folders, kept by name) into the new archive `dest`.
/// The partial file is removed when the job fails or is cancelled.
pub fn pack(format: PackFormat, srcs: &[PathBuf], dest: &Path, p: &Progress) {
    let mut items = Vec::new();
    for src in srcs {
        let name = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        walk(src, name, &mut items, p);
    }
    p.total.store(items.iter().map(|i| i.size).sum(), std::sync::atomic::Ordering::Relaxed);
    let result = match format {
        PackFormat::Zip => pack_zip(&items, dest, p),
        PackFormat::SevenZip => pack_7z(&items, dest, p),
        PackFormat::TarGz => pack_tar(&items, dest, Compression::Gzip, p),
        PackFormat::TarXz => pack_tar(&items, dest, Compression::Xz, p),
    };
    match result {
        Ok(()) if !p.cancelled() => {
            // Compressors lag behind the reads, so the bar only reaches 100% here.
            p.done.store(p.total.load(std::sync::atomic::Ordering::Relaxed), std::sync::atomic::Ordering::Relaxed);
        }
        Ok(()) => {
            let _ = fs::remove_file(dest);
        }
        // Somebody else's file: report it, never delete it.
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => p.fail(format!("{}: {e}", dest.display())),
        Err(e) => {
            if e.kind() != io::ErrorKind::Interrupted {
                p.fail(format!("{}: {e}", dest.display()));
            }
            let _ = fs::remove_file(dest);
        }
    }
}

fn pack_zip(items: &[PackItem], dest: &Path, p: &Progress) -> io::Result<()> {
    let mut w = zip::ZipWriter::new(io::BufWriter::new(fs::OpenOptions::new().write(true).create_new(true).open(dest)?));
    for item in items {
        if p.cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        let mut options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).large_file(item.size >= u32::MAX as u64);
        if let Some(t) = fs::metadata(&item.path).and_then(|m| m.modified()).ok().and_then(zip_datetime) {
            options = options.last_modified_time(t);
        }
        if item.is_dir {
            w.add_directory(&item.name, options).map_err(io::Error::other)?;
            continue;
        }
        let file = match File::open(&item.path) {
            Ok(f) => f,
            Err(e) => {
                p.fail(format!("{}: {e}", item.path.display()));
                continue;
            }
        };
        w.start_file(&item.name, options).map_err(io::Error::other)?;
        io::copy(&mut CountingReader { inner: file, p, percent: 100 }, &mut w)?;
    }
    w.finish().map_err(io::Error::other)?;
    Ok(())
}

/// Opens its file on the first read and closes it at the end, so a solid 7z block over
/// thousands of files never holds thousands of handles.
struct LazyFile<'a> {
    path: &'a Path,
    file: Option<File>,
    p: &'a Progress,
}

impl io::Read for LazyFile<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.file.is_none() {
            self.file = Some(File::open(self.path).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", self.path.display())))?);
        }
        let mut counted = CountingReader { inner: self.file.as_mut().expect("opened above"), p: self.p, percent: COMPRESSED_PERCENT };
        let n = io::Read::read(&mut counted, buf)?;
        if n == 0 {
            self.file = None;
        }
        Ok(n)
    }
}

/// One solid block compressed by all cores: much faster than a block per file (no encoder
/// start-up per file) and it compresses small files together.
fn pack_7z(items: &[PackItem], dest: &Path, p: &Progress) -> io::Result<()> {
    // `create` would silently replace an existing file.
    drop(fs::OpenOptions::new().write(true).create_new(true).open(dest)?);
    let other = |e: sevenz_rust2::Error| io::Error::other(e.to_string());
    let mut w = sevenz_rust2::ArchiveWriter::create(dest).map_err(other)?;
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get() as u32);
    let mut lzma2 = sevenz_rust2::encoder_options::Lzma2Options::from_level_mt(5, threads, BLOCK_BYTES);
    lzma2.set_dictionary_size(BLOCK_BYTES as u32);
    w.set_content_methods(vec![lzma2.into()]);

    let mut entries = Vec::new();
    let mut readers = Vec::new();
    for item in items {
        let entry = sevenz_rust2::ArchiveEntry::from_path(&item.path, item.name.clone());
        if item.is_dir {
            w.push_archive_entry(entry, None::<io::Empty>).map_err(other)?;
        } else {
            entries.push(entry);
            readers.push(sevenz_rust2::SourceReader::new(LazyFile { path: &item.path, file: None, p }));
        }
    }
    if !entries.is_empty() {
        w.push_archive_entries(entries, readers).map_err(other)?;
    }
    w.finish()?;
    Ok(())
}

fn pack_tar(items: &[PackItem], dest: &Path, compression: Compression, p: &Progress) -> io::Result<()> {
    use std::io::Write;
    let out = io::BufWriter::new(fs::OpenOptions::new().write(true).create_new(true).open(dest)?);
    match compression {
        Compression::None => tar_items(items, out, 100, p)?.flush(),
        Compression::Gzip => tar_items(items, flate2::write::GzEncoder::new(out, flate2::Compression::default()), 100, p)?.finish()?.flush(),
        Compression::Xz => {
            // Independent blocks let every core compress a slice at the same time.
            let mut options = lzma_rust2::XzOptions::with_preset(5);
            options.lzma_options.dict_size = BLOCK_BYTES as u32;
            options.set_block_size(std::num::NonZeroU64::new(BLOCK_BYTES));
            let workers = std::thread::available_parallelism().map_or(2, |n| n.get() as u32);
            let writer = lzma_rust2::XzWriterMt::new(out, options, workers).map_err(io::Error::other)?;
            tar_items(items, writer, COMPRESSED_PERCENT, p)?.finish().map_err(io::Error::other)?.flush()
        }
    }
}

fn tar_items<W: io::Write>(items: &[PackItem], out: W, percent: u64, p: &Progress) -> io::Result<W> {
    let mut builder = tar::Builder::new(out);
    for item in items {
        if p.cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        let meta = match fs::metadata(&item.path) {
            Ok(m) => m,
            Err(e) => {
                p.fail(format!("{}: {e}", item.path.display()));
                continue;
            }
        };
        let mut header = tar::Header::new_gnu();
        header.set_metadata(&meta);
        if item.is_dir {
            builder.append_data(&mut header, &item.name, io::empty())?;
            continue;
        }
        match File::open(&item.path) {
            Ok(f) => builder.append_data(&mut header, &item.name, CountingReader { inner: f, p, percent })?,
            Err(e) => p.fail(format!("{}: {e}", item.path.display())),
        }
    }
    builder.into_inner()
}

/// Starts packing on a worker thread.
pub fn spawn_pack(format: PackFormat, srcs: Vec<PathBuf>, dest: PathBuf) -> Job {
    let label = format!("Pack {} item(s) to {}", srcs.len(), dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    jobs::spawn_with(label, move |p| pack(format, &srcs, &dest, p))
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

    fn pack_round_trip(format: PackFormat) {
        let dir = scratch(&format!("pack_{}", format.extension()));
        let src = dir.join("proj");
        fs::create_dir_all(src.join("sub/empty")).unwrap();
        fs::write(src.join("a.txt"), "A").unwrap();
        fs::write(src.join("sub/b.txt"), "B").unwrap();
        fs::write(dir.join("loose.txt"), "L").unwrap();

        let archive = dir.join(format!("out.{}", format.extension()));
        let p = Progress::default();
        pack(format, &[src, dir.join("loose.txt")], &archive, &p);
        assert!(p.errors().is_empty(), "{:?}", p.errors());
        assert_eq!(p.done.load(Ordering::Relaxed), 3);

        let idx = open(&archive).unwrap();
        assert_eq!(names(&idx.list("")), [("proj", true), ("loose.txt", false)]);
        assert_eq!(names(&idx.list("proj/sub")), [("empty", true), ("b.txt", false)]);
        let out = dir.join("x");
        fs::create_dir(&out).unwrap();
        let p = Progress::default();
        idx.extract(&["proj".to_owned()], &out, &p);
        assert!(p.errors().is_empty(), "{:?}", p.errors());
        assert_eq!(fs::read_to_string(out.join("proj/sub/b.txt")).unwrap(), "B");

        // An existing archive is never overwritten.
        let p = Progress::default();
        pack(format, &[dir.join("loose.txt")], &archive, &p);
        assert!(!p.errors().is_empty());
        assert_eq!(open(&archive).unwrap().list("").len(), 2);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pack_zip_round_trips() {
        pack_round_trip(PackFormat::Zip);
    }

    #[test]
    fn pack_7z_round_trips() {
        pack_round_trip(PackFormat::SevenZip);
    }

    #[test]
    fn pack_tar_gz_round_trips() {
        pack_round_trip(PackFormat::TarGz);
    }

    #[test]
    fn pack_tar_xz_round_trips() {
        pack_round_trip(PackFormat::TarXz);
    }

    #[test]
    fn tar_names_are_recognized() {
        for name in ["a.tar", "a.TAR.GZ", "a.tgz", "a.tar.xz", "a.txz"] {
            assert!(is_archive(Path::new(name)), "{name}");
        }
        assert!(!is_archive(Path::new("a.gz")));
        assert!(!is_archive(Path::new("tar")));
    }

    #[test]
    fn archive_inside_archive_opens_via_scratch_copy() {
        let dir = scratch("nested");
        let mut inner = Vec::new();
        {
            let mut w = zip::ZipWriter::new(io::Cursor::new(&mut inner));
            w.start_file("deep/x.txt", zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(b"X").unwrap();
            w.finish().unwrap();
        }
        let mut w = zip::ZipWriter::new(File::create(dir.join("outer.zip")).unwrap());
        w.start_file("pkg/inner.zip", zip::write::SimpleFileOptions::default()).unwrap();
        w.write_all(&inner).unwrap();
        w.finish().unwrap();

        let outer = open(&dir.join("outer.zip")).unwrap();
        let nested = open_nested(&outer, "pkg/inner.zip").unwrap();
        assert_eq!(names(&nested.list("")), [("deep", true)]);
        assert_eq!(names(&nested.list("deep")), [("x.txt", false)]);
        cleanup_nested();
        assert!(!nested_root().exists());
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
