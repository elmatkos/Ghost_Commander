//! Core logic for Ghost Commander: directory listing, no UI dependencies.

pub mod filetype;
pub mod jobs;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

/// Lists a directory: folders first, then files, each group sorted case-insensitively.
/// Entries that cannot be read are skipped.
pub fn list_dir(dir: &Path) -> io::Result<Vec<Entry>> {
    let mut entries: Vec<Entry> = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let mut meta = e.metadata().ok()?;
            if meta.file_type().is_symlink() {
                // Show the target's type and size; a dangling link keeps its own metadata.
                if let Ok(target) = fs::metadata(e.path()) {
                    meta = target;
                }
            }
            Some(Entry {
                name: e.file_name().to_string_lossy().into_owned(),
                path: e.path(),
                is_dir: meta.is_dir(),
                size: if meta.is_dir() { 0 } else { meta.len() },
                modified: meta.modified().ok(),
            })
        })
        .collect();
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

/// Root paths of the available drives (`C:\`, `D:\`, ...); `/` on non-Windows systems.
#[cfg(windows)]
pub fn drives() -> Vec<PathBuf> {
    // SAFETY: GetLogicalDrives takes no arguments and only returns a bitmask.
    let mask = unsafe { windows_sys::Win32::Storage::FileSystem::GetLogicalDrives() };
    (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| PathBuf::from(format!("{}:\\", (b'A' + i) as char)))
        .collect()
}

#[cfg(not(windows))]
pub fn drives() -> Vec<PathBuf> {
    vec![PathBuf::from("/")]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_come_first_and_sorting_ignores_case() {
        let dir = std::env::temp_dir().join(format!("gc_core_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("zeta")).unwrap();
        fs::write(dir.join("Beta.txt"), b"12345").unwrap();
        fs::write(dir.join("alpha.txt"), b"").unwrap();

        let names: Vec<_> = list_dir(&dir).unwrap().into_iter().map(|e| e.name).collect();
        fs::remove_dir_all(&dir).unwrap();

        assert_eq!(names, ["zeta", "alpha.txt", "Beta.txt"]);
    }

    #[test]
    fn symlinks_show_the_size_of_their_target() {
        let dir = std::env::temp_dir().join(format!("gc_core_symlink_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("real.txt"), b"12345").unwrap();
        // Creating symlinks needs a privilege on Windows; skip quietly without it.
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_file(dir.join("real.txt"), dir.join("link.txt")).is_ok();
        #[cfg(not(windows))]
        let linked = std::os::unix::fs::symlink(dir.join("real.txt"), dir.join("link.txt")).is_ok();

        let size = list_dir(&dir).unwrap().into_iter().find(|e| e.name == "link.txt").map(|e| e.size);
        fs::remove_dir_all(&dir).unwrap();
        if linked {
            assert_eq!(size, Some(5));
        }
    }
}
