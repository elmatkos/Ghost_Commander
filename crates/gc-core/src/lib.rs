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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveKind {
    Fixed,
    Removable,
    Network,
    Optical,
    Other,
}

/// A drive with its type and capacity (`total` is 0 when there is no medium or it cannot be read).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveInfo {
    pub root: PathBuf,
    pub kind: DriveKind,
    pub total: u64,
    pub free: u64,
}

/// Drives with type and free space. Asks the drives themselves, so a stale network drive can
/// block: call it from a worker thread, never from the UI thread.
#[cfg(windows)]
pub fn drive_infos() -> Vec<DriveInfo> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW};

    drives()
        .into_iter()
        .map(|root| {
            let wide: Vec<u16> = root.as_os_str().encode_wide().chain(Some(0)).collect();
            // SAFETY: `wide` is NUL-terminated; the out pointers are valid for the call.
            let (kind, total, free) = unsafe {
                let kind = match GetDriveTypeW(wide.as_ptr()) {
                    2 => DriveKind::Removable,
                    3 => DriveKind::Fixed,
                    4 => DriveKind::Network,
                    5 => DriveKind::Optical,
                    _ => DriveKind::Other,
                };
                let (mut available, mut total, mut free) = (0u64, 0u64, 0u64);
                if GetDiskFreeSpaceExW(wide.as_ptr(), &mut available, &mut total, &mut free) == 0 {
                    (kind, 0, 0)
                } else {
                    (kind, total, available)
                }
            };
            DriveInfo { root, kind, total, free }
        })
        .collect()
}

#[cfg(not(windows))]
pub fn drive_infos() -> Vec<DriveInfo> {
    drives().into_iter().map(|root| DriveInfo { root, kind: DriveKind::Fixed, total: 0, free: 0 }).collect()
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
    fn drive_infos_describe_the_drive_of_the_current_folder() {
        let here = std::env::current_dir().unwrap();
        let infos = drive_infos();
        let mine = infos.iter().find(|d| here.starts_with(&d.root)).expect("current folder is on a listed drive");
        assert!(mine.total > 0 && mine.free <= mine.total);
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
