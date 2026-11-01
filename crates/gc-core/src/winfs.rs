//! Windows file copy through the operating system (`CopyFileExW`) and a seek-penalty probe.
//!
//! The OS copies on the kernel side: SMB shares can copy server to server without the data
//! passing through this PC, ReFS clones blocks instead of copying them, and timestamps,
//! attributes and alternate data streams come along.

use std::cell::Cell;
use std::ffi::c_void;
use std::io;
use std::iter::once;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::{null, null_mut};
use std::sync::atomic::Ordering;

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, BOOL, ERROR_ALREADY_EXISTS, ERROR_FILE_EXISTS, ERROR_REQUEST_ABORTED, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CopyFileExW, CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Ioctl::{
    PropertyStandardQuery, StorageDeviceSeekPenaltyProperty, DEVICE_SEEK_PENALTY_DESCRIPTOR, IOCTL_STORAGE_QUERY_PROPERTY, STORAGE_PROPERTY_QUERY,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::WindowsProgramming::{COPY_FILE_FAIL_IF_EXISTS, COPY_FILE_NO_BUFFERING, PROGRESS_CANCEL};

const PROGRESS_CONTINUE: u32 = 0;

use crate::jobs::Progress;

/// Result of one `CopyFileExW` call.
pub enum Outcome {
    Done,
    /// The target exists and `overwrite` was false.
    Exists,
    Cancelled,
}

struct Ctx<'a> {
    p: &'a Progress,
    last: Cell<u64>,
}

unsafe extern "system" fn on_progress(_total: i64, transferred: i64, _stream_size: i64, _stream_done: i64, stream: u32, _reason: u32, _src: HANDLE, _dst: HANDLE, data: *const c_void) -> u32 {
    // SAFETY: `data` is the `Ctx` that `copy_file` keeps alive for the whole call.
    let ctx = unsafe { &*(data as *const Ctx) };
    // Only the main data stream counts towards the byte total (alternate streams are small).
    if stream == 1 {
        let now = transferred.max(0) as u64;
        ctx.p.done.fetch_add(now.saturating_sub(ctx.last.replace(now)), Ordering::Relaxed);
    }
    if ctx.p.cancelled() {
        PROGRESS_CANCEL
    } else {
        PROGRESS_CONTINUE
    }
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(once(0)).collect()
}

/// Copies `src` to `dst`, adding the bytes written to `p.done` and honouring `p.cancel`
/// (a cancelled copy removes the partial target). `unbuffered` bypasses the system cache,
/// which keeps RAM free of a huge one-off file.
pub fn copy_file(src: &Path, dst: &Path, overwrite: bool, unbuffered: bool, p: &Progress) -> io::Result<Outcome> {
    let ctx = Ctx { p, last: Cell::new(0) };
    let mut cancel: BOOL = 0;
    let flags = if overwrite { 0 } else { COPY_FILE_FAIL_IF_EXISTS } | if unbuffered { COPY_FILE_NO_BUFFERING } else { 0 };
    // SAFETY: both paths are NUL-terminated and outlive the call; `ctx` outlives it too.
    let ok = unsafe { CopyFileExW(wide(src).as_ptr(), wide(dst).as_ptr(), Some(on_progress), &ctx as *const Ctx as *const c_void, &mut cancel, flags) };
    if ok != 0 {
        return Ok(Outcome::Done);
    }
    match unsafe { GetLastError() } {
        ERROR_REQUEST_ABORTED => Ok(Outcome::Cancelled),
        ERROR_FILE_EXISTS | ERROR_ALREADY_EXISTS => Ok(Outcome::Exists),
        code => Err(io::Error::from_raw_os_error(code as i32)),
    }
}

/// True if the drive holding `path` reports a seek penalty (a spinning disk). Several files
/// copied at once make such a disk seek back and forth, so the caller then copies one by one.
/// Network paths, unknown devices and any failure count as "not rotational".
pub fn is_rotational(path: &Path) -> bool {
    let text = path.to_string_lossy();
    let b = text.as_bytes();
    if b.len() < 2 || b[1] != b':' || !b[0].is_ascii_alphabetic() {
        return false;
    }
    let device: Vec<u16> = format!("\\\\.\\{}:", b[0] as char).encode_utf16().chain(once(0)).collect();
    // SAFETY: plain Win32 calls with properly sized, initialised buffers; the handle is closed.
    unsafe {
        let handle = CreateFileW(device.as_ptr(), 0, FILE_SHARE_READ | FILE_SHARE_WRITE, null(), OPEN_EXISTING, 0, null_mut());
        if handle == INVALID_HANDLE_VALUE {
            return false;
        }
        let query = STORAGE_PROPERTY_QUERY { PropertyId: StorageDeviceSeekPenaltyProperty, QueryType: PropertyStandardQuery, AdditionalParameters: [0] };
        let mut out: DEVICE_SEEK_PENALTY_DESCRIPTOR = std::mem::zeroed();
        let mut returned = 0u32;
        let ok = DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            &query as *const _ as *const c_void,
            std::mem::size_of_val(&query) as u32,
            &mut out as *mut _ as *mut c_void,
            std::mem::size_of_val(&out) as u32,
            &mut returned,
            null_mut(),
        );
        CloseHandle(handle);
        ok != 0 && out.IncursSeekPenalty != 0
    }
}
