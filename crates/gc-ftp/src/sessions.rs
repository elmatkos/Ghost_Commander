//! Saved FTP sessions in `ftp_sessions.toml`. Passwords are encrypted with the Windows user key
//! (DPAPI), so the file is useless on another account or machine; where DPAPI is not available
//! (e.g. WinPE) the password is simply not saved.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Target;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Session {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    /// Plain text in memory only; stored encrypted.
    #[serde(skip)]
    pub password: String,
    pub secure: bool,
    pub insecure_tls: bool,
    pub path: String,
    /// `dpapi:<hex>` or empty.
    password_enc: String,
}

impl Session {
    pub fn new() -> Self {
        Session { name: "New session".into(), port: 21, user: "anonymous".into(), password: "anonymous@".into(), path: "/".into(), ..Default::default() }
    }

    pub fn target(&self) -> Target {
        Target { host: self.host.trim().to_owned(), port: self.port, user: self.user.clone(), password: self.password.clone(), secure: self.secure, insecure_tls: self.insecure_tls, path: self.path.clone() }
    }

    /// A session from a typed link (name = host).
    pub fn from_target(t: &Target) -> Self {
        Session { name: t.host.clone(), host: t.host.clone(), port: t.port, user: t.user.clone(), password: t.password.clone(), secure: t.secure, insecure_tls: t.insecure_tls, path: t.path.clone(), password_enc: String::new() }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    session: Vec<Session>,
}

fn file_in(dir: &Path) -> PathBuf {
    dir.join("ftp_sessions.toml")
}

/// Reads the saved sessions; a missing or broken file gives an empty list.
pub fn load(dir: &Path) -> Vec<Session> {
    let Ok(text) = fs::read_to_string(file_in(dir)) else { return Vec::new() };
    let mut file: File = toml::from_str(&text).unwrap_or_default();
    for s in &mut file.session {
        s.password = s.password_enc.strip_prefix("dpapi:").and_then(from_hex).and_then(|b| unprotect(&b)).and_then(|b| String::from_utf8(b).ok()).unwrap_or_default();
        s.password_enc.clear(); // regenerated on save
    }
    file.session
}

/// Writes the sessions. Returns a warning when a password could not be encrypted (and was left out).
pub fn save(dir: &Path, sessions: &[Session]) -> io::Result<Option<String>> {
    let mut warning = None;
    let mut file = File::default();
    for s in sessions {
        let mut s = s.clone();
        s.password_enc = String::new();
        if !s.password.is_empty() {
            match protect(s.password.as_bytes()) {
                Some(b) => s.password_enc = format!("dpapi:{}", to_hex(&b)),
                None => warning = Some("This system cannot encrypt passwords; they were not saved.".to_owned()),
            }
        }
        file.session.push(s);
    }
    fs::create_dir_all(dir)?;
    let text = toml::to_string_pretty(&file).map_err(io::Error::other)?;
    fs::write(file_in(dir), text)?;
    Ok(warning)
}

fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

#[cfg(windows)]
fn protect(data: &[u8]) -> Option<Vec<u8>> {
    use windows_sys::Win32::Security::Cryptography::{CryptProtectData, CRYPT_INTEGER_BLOB};
    let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
    let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
    // SAFETY: `input` points at `data` for the call; on success `out` is a LocalAlloc buffer we copy and free.
    unsafe {
        if CryptProtectData(&input, std::ptr::null(), std::ptr::null(), std::ptr::null(), std::ptr::null(), 0, &mut out) == 0 {
            return None;
        }
        Some(take_blob(out))
    }
}

#[cfg(windows)]
fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
    use windows_sys::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};
    let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
    let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
    // SAFETY: as in `protect`.
    unsafe {
        if CryptUnprotectData(&input, std::ptr::null_mut(), std::ptr::null(), std::ptr::null(), std::ptr::null(), 0, &mut out) == 0 {
            return None;
        }
        Some(take_blob(out))
    }
}

/// Copies a DPAPI output blob and frees it.
#[cfg(windows)]
unsafe fn take_blob(blob: windows_sys::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB) -> Vec<u8> {
    let bytes = std::slice::from_raw_parts(blob.pbData, blob.cbData as usize).to_vec();
    windows_sys::Win32::Foundation::LocalFree(blob.pbData as _);
    bytes
}

#[cfg(not(windows))]
fn protect(_: &[u8]) -> Option<Vec<u8>> {
    None
}

#[cfg(not(windows))]
fn unprotect(_: &[u8]) -> Option<Vec<u8>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip() {
        assert_eq!(from_hex(&to_hex(&[0, 1, 254, 255])), Some(vec![0, 1, 254, 255]));
        assert_eq!(from_hex("abc"), None);
    }

    #[cfg(windows)]
    #[test]
    fn password_survives_save_and_load() {
        let dir = std::env::temp_dir().join(format!("gc-ftp-sessions-{}", std::process::id()));
        let mut s = Session::new();
        s.host = "h.example".into();
        s.user = "mato".into();
        s.password = "pä ss:wörd@".into();
        save(&dir, &[s.clone()]).unwrap();
        let text = fs::read_to_string(file_in(&dir)).unwrap();
        assert!(!text.contains("wörd"), "password must not be stored in clear text");
        assert_eq!(load(&dir), vec![s]);
        let _ = fs::remove_dir_all(dir);
    }
}
