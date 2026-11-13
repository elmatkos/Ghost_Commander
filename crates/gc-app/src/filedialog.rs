//! The system "Open" dialog, run on a worker thread so the UI keeps drawing while it is open.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};

/// Asks for a file, starting in `start`. The receiver gets the chosen file, or `None` when cancelled.
pub fn pick_file(ctx: &eframe::egui::Context, title: &str, start: &Path) -> Receiver<Option<PathBuf>> {
    let (tx, rx) = channel();
    let ctx = ctx.clone();
    let title = title.to_string();
    let start = start.to_path_buf();
    #[cfg(windows)]
    let owner = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() }.0 as isize;
    std::thread::spawn(move || {
        #[cfg(windows)]
        let chosen = open_dialog(owner, &title, &start);
        #[cfg(not(windows))]
        let chosen = {
            let _ = (&title, &start);
            None
        };
        let _ = tx.send(chosen);
        ctx.request_repaint();
    });
    rx
}

#[cfg(windows)]
fn open_dialog(owner: isize, title: &str, start: &Path) -> Option<PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Controls::Dialogs::{GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW};

    let wide = |s: &std::ffi::OsStr| -> Vec<u16> { s.encode_wide().chain(std::iter::once(0)).collect() };
    let filter: Vec<u16> = "All files\0*.*\0\0".encode_utf16().collect();
    let dir = wide(start.as_os_str());
    let title = wide(std::ffi::OsStr::new(title));
    let mut buf = vec![0u16; 32768];
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: HWND(owner as *mut std::ffi::c_void),
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(buf.as_mut_ptr()),
        nMaxFile: buf.len() as u32,
        lpstrInitialDir: PCWSTR(dir.as_ptr()),
        lpstrTitle: PCWSTR(title.as_ptr()),
        Flags: OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_EXPLORER | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    if unsafe { GetOpenFileNameW(&mut ofn) }.as_bool() {
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(PathBuf::from(OsString::from_wide(&buf[..len])))
    } else {
        None
    }
}
