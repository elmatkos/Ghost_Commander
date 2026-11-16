//! Diagnostic log `gc.log` next to the exe (startup steps, panics, crashes, UI stalls).
//! A GUI exe has no console, so this is the only way to see why it died (e.g. in WinPE).
//!
//! Off by default. Switched on by `--log`, by Config > Options, or `logging = true` in settings.toml;
//! `--no-log` forces it off. It can be toggled while the program runs.

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, Once};
use std::time::{Duration, Instant};

static FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);
static HOOKS: Once = Once::new();

/// True while lines are being written.
pub fn is_enabled() -> bool {
    FILE.lock().map_or(false, |f| f.is_some())
}

/// Appends one timestamped line; does nothing while logging is off.
pub fn log(msg: &str) {
    if let Ok(mut guard) = FILE.lock() {
        if let Some(f) = guard.as_mut() {
            let _ = writeln!(f, "[{}] {msg}", chrono::Local::now().format("%H:%M:%S%.3f"));
            let _ = f.flush();
        }
    }
}

pub fn set_enabled(on: bool) {
    if on {
        enable();
    } else if is_enabled() {
        log("logging switched off");
        if let Ok(mut guard) = FILE.lock() {
            *guard = None;
        }
    }
}

/// Creates a fresh `gc.log` (next to the exe, else in the current folder). The panic hook, the
/// crash filter and the stall watchdog are installed the first time and stay (silent while off).
pub fn enable() {
    if is_enabled() {
        return;
    }
    let beside_exe = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("gc.log")));
    let file = beside_exe.and_then(|p| std::fs::File::create(p).ok()).or_else(|| std::fs::File::create("gc.log").ok());
    if let Ok(mut guard) = FILE.lock() {
        *guard = file;
    }
    log(&format!("Ghost Commander v{} logging on", env!("CARGO_PKG_VERSION")));
    log(&format!("args: {:?}, cwd: {:?}", std::env::args().collect::<Vec<_>>(), std::env::current_dir()));
    HOOKS.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if is_enabled() {
                log(&format!("PANIC: {info}\n{}", std::backtrace::Backtrace::force_capture()));
            } else {
                previous(info);
            }
        }));
        #[cfg(windows)]
        install_crash_filter();
        // Tells apart "someone called exit()" (this runs) from ExitProcess/TerminateProcess (it does not).
        extern "C" fn at_exit() {
            log("atexit handler ran (C runtime exit path)");
        }
        extern "C" {
            fn atexit(cb: extern "C" fn()) -> i32;
        }
        // SAFETY: `at_exit` has the C ABI and stays valid for the whole process.
        unsafe { atexit(at_exit) };
        start_watchdog();
    });
}

#[cfg(windows)]
fn install_crash_filter() {
    use windows::Win32::System::Diagnostics::Debug::{AddVectoredExceptionHandler, SetUnhandledExceptionFilter, EXCEPTION_POINTERS};

    /// First-chance view of every exception, also the ones the program handles itself.
    unsafe extern "system" fn first_chance(info: *mut EXCEPTION_POINTERS) -> i32 {
        // SAFETY: Windows passes a valid pointer to the exception record.
        let code = unsafe { info.as_ref().and_then(|i| i.ExceptionRecord.as_ref()).map(|r| r.ExceptionCode.0) };
        log(&format!("first-chance exception: {code:#x?}"));
        0 // EXCEPTION_CONTINUE_SEARCH
    }

    unsafe extern "system" fn filter(info: *const EXCEPTION_POINTERS) -> i32 {
        // SAFETY: Windows passes a valid pointer to the exception record.
        let code = unsafe { info.as_ref().and_then(|i| i.ExceptionRecord.as_ref()).map(|r| r.ExceptionCode.0) };
        log(&format!("UNHANDLED EXCEPTION: {code:#x?}"));
        0 // EXCEPTION_CONTINUE_SEARCH: let Windows terminate as usual
    }
    // SAFETY: both functions have the required signature and live for the whole process.
    unsafe {
        SetUnhandledExceptionFilter(Some(filter));
        AddVectoredExceptionHandler(1, Some(first_chance));
    }
}

/// Milliseconds since `START` at which the UI thread last entered or left `update`.
static LAST_PROGRESS_MS: AtomicU64 = AtomicU64::new(0);
static IN_UPDATE: AtomicBool = AtomicBool::new(false);
static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

fn now_ms() -> u64 {
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// Called by the UI thread when `update` starts (`true`) and ends (`false`).
pub fn frame_edge(entering: bool) {
    IN_UPDATE.store(entering, Ordering::Relaxed);
    LAST_PROGRESS_MS.store(now_ms(), Ordering::Relaxed);
}

/// Logs when the UI thread makes no progress for 5 s: stuck in our `update` code, or outside it
/// (rendering, buffer swap, the OS event loop). Silent while everything runs normally or logging is off.
fn start_watchdog() {
    START.get_or_init(Instant::now);
    std::thread::spawn(|| {
        let mut reported = false;
        loop {
            std::thread::sleep(Duration::from_secs(1));
            let stalled_ms = now_ms().saturating_sub(LAST_PROGRESS_MS.load(Ordering::Relaxed));
            if stalled_ms >= 5000 && !reported {
                reported = true;
                let place = if IN_UPDATE.load(Ordering::Relaxed) { "inside update (app code)" } else { "outside update (render, swap or event loop)" };
                log(&format!("UI STALLED for {} s, {place}; {}", stalled_ms / 1000, memory_summary()));
            } else if stalled_ms < 5000 && reported {
                reported = false;
                log("UI recovered");
            }
        }
    });
}

#[cfg(windows)]
fn memory_summary() -> String {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut m = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    // SAFETY: `m` is initialised with its size as the API requires.
    match unsafe { GlobalMemoryStatusEx(&mut m) } {
        Ok(()) => format!("RAM free {} of {} MB", m.ullAvailPhys >> 20, m.ullTotalPhys >> 20),
        Err(e) => format!("memory unknown ({e})"),
    }
}

#[cfg(not(windows))]
fn memory_summary() -> String {
    String::new()
}
