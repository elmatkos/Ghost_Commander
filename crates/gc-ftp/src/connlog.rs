//! Connection log shared by every FTP connection of the app: connect steps, TLS, login, folder
//! reads, transfers and all errors with their full text. Passwords are never written to it.

use std::sync::Mutex;

const MAX_LINES: usize = 3000;

static LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Appends a line with the local time.
pub fn log(msg: impl AsRef<str>) {
    let stamp = chrono::Local::now().format("%H:%M:%S%.3f");
    let mut lines = LINES.lock().unwrap_or_else(|e| e.into_inner());
    for part in msg.as_ref().lines() {
        lines.push(format!("{stamp}  {part}"));
    }
    if lines.len() > MAX_LINES {
        let extra = lines.len() - MAX_LINES;
        lines.drain(..extra);
    }
}

/// A copy of the log, oldest line first.
pub fn lines() -> Vec<String> {
    LINES.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Number of lines (cheap check for "did anything new arrive").
pub fn len() -> usize {
    LINES.lock().unwrap_or_else(|e| e.into_inner()).len()
}

pub fn clear() {
    LINES.lock().unwrap_or_else(|e| e.into_inner()).clear();
}
