//! Diagnostic log `gc.log` next to the exe (startup steps, panics, crashes, exit result).
//! A GUI exe has no console, so this is the only way to see why it died (e.g. in WinPE).

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

static FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);

/// Appends one timestamped line; silently does nothing when the log could not be created.
pub fn log(msg: &str) {
    if let Ok(mut guard) = FILE.lock() {
        if let Some(f) = guard.as_mut() {
            let _ = writeln!(f, "[{}] {msg}", chrono::Local::now().format("%H:%M:%S%.3f"));
            let _ = f.flush();
        }
    }
}

/// Creates a fresh `gc.log` (next to the exe, else in the current folder) and installs the panic
/// hook and, on Windows, an unhandled-exception filter that record what killed the process.
pub fn init() {
    let beside_exe = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("gc.log")));
    let file = beside_exe.and_then(|p| std::fs::File::create(p).ok()).or_else(|| std::fs::File::create("gc.log").ok());
    if let Ok(mut guard) = FILE.lock() {
        *guard = file;
    }
    log(&format!("Ghost Commander v{} starting", env!("CARGO_PKG_VERSION")));
    log(&format!("args: {:?}, cwd: {:?}", std::env::args().collect::<Vec<_>>(), std::env::current_dir()));
    std::panic::set_hook(Box::new(|info| {
        log(&format!("PANIC: {info}\n{}", std::backtrace::Backtrace::force_capture()));
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
    // SAFETY: `filter` has the required signature and lives for the whole process.
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
/// (rendering, buffer swap, the OS event loop). Silent while everything runs normally.
pub fn start_watchdog() {
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
