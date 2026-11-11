//! FTP / FTPS client for Ghost Commander: connection targets (also from `ftp://user:pass@host/dir`
//! links), directory listing, recursive download / upload / delete as background-job work, and
//! saved sessions (see [`sessions`]).

pub mod sessions;

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::ToSocketAddrs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use gc_core::jobs::{Progress, Resolution};
use gc_core::Entry;
use suppaftp::list::File as ListFile;
use suppaftp::native_tls::TlsConnector;
use suppaftp::types::FileType;
use suppaftp::{FtpError, NativeTlsConnector, NativeTlsFtpStream};

const BUF_SIZE: usize = 256 * 1024;
/// An idle control connection is probed with NOOP before use after this long.
const IDLE_PROBE: Duration = Duration::from_secs(15);

/// Where and how to connect.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Target {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    /// Explicit FTPS (`AUTH TLS`).
    pub secure: bool,
    /// Accept self-signed / mismatching certificates (FTPS only).
    pub insecure_tls: bool,
    /// Folder to start in.
    pub path: String,
}

impl Target {
    /// `ftp://user@host:port` (never contains the password); used for headers and messages.
    pub fn label(&self) -> String {
        let scheme = if self.secure { "ftps" } else { "ftp" };
        let user = if self.user.is_empty() || self.user == "anonymous" { String::new() } else { format!("{}@", self.user) };
        let port = if self.port == 21 { String::new() } else { format!(":{}", self.port) };
        format!("{scheme}://{user}{}{port}", self.host)
    }

    /// Parses `ftp://[user[:password]@]host[:port][/path]` (also `ftps://`). User, password and path
    /// may be percent-encoded (`%40` for `@`, `%3A` for `:`, `%2F` for `/`).
    pub fn from_url(url: &str) -> Result<Target, String> {
        let url = url.trim();
        let (secure, rest) = match url.split_once("://") {
            Some((scheme, rest)) if scheme.eq_ignore_ascii_case("ftp") => (false, rest),
            Some((scheme, rest)) if scheme.eq_ignore_ascii_case("ftps") => (true, rest),
            Some((scheme, _)) => return Err(format!("Unsupported scheme \"{scheme}\" (use ftp:// or ftps://).")),
            None => (false, url),
        };
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        // The last '@' separates the login from the host, so an unencoded '@' in a password still works.
        let (userinfo, hostport) = match authority.rfind('@') {
            Some(i) => (Some(&authority[..i]), &authority[i + 1..]),
            None => (None, authority),
        };
        let (user, password) = match userinfo {
            Some(u) => match u.split_once(':') {
                Some((user, pass)) => (percent_decode(user), percent_decode(pass)),
                None => (percent_decode(u), String::new()),
            },
            None => ("anonymous".to_owned(), "anonymous@".to_owned()),
        };
        let (host, port) = split_host_port(hostport)?;
        if host.is_empty() {
            return Err("The link has no host name.".into());
        }
        Ok(Target { host, port, user, password, secure, insecure_tls: false, path: percent_decode(path) })
    }
}

fn split_host_port(s: &str) -> Result<(String, u16), String> {
    let bad_port = |p: &str| format!("\"{p}\" is not a valid port.");
    if let Some(rest) = s.strip_prefix('[') {
        // [IPv6]:port
        let (host, tail) = rest.split_once(']').ok_or("Missing ] in the IPv6 address.")?;
        let port = match tail.strip_prefix(':') {
            Some(p) => p.parse().map_err(|_| bad_port(p))?,
            None => 21,
        };
        return Ok((host.to_owned(), port));
    }
    match s.rsplit_once(':') {
        Some((host, p)) => Ok((host.to_owned(), p.parse().map_err(|_| bad_port(p))?)),
        None => Ok((s.to_owned(), 21)),
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn err(e: FtpError) -> io::Error {
    io::Error::other(e.to_string())
}

/// Joins remote paths (always `/`).
pub fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// Parent of a remote path, `None` at the root.
pub fn parent(path: &str) -> Option<String> {
    let t = path.trim_end_matches('/');
    let i = t.rfind('/')?;
    Some(if i == 0 { "/".to_owned() } else { t[..i].to_owned() })
}

/// Remote path of a panel entry (panels keep remote paths in a `PathBuf`; Windows may have turned `/` into `\`).
pub fn remote_str(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// One control connection, shared between the panel and the background jobs.
pub struct Conn {
    ftp: Option<NativeTlsFtpStream>,
    pub target: Target,
    last_used: Instant,
}

pub type SharedConn = Arc<Mutex<Conn>>;

impl Conn {
    /// Connects and logs in. Blocks: call off the UI thread.
    pub fn connect(target: Target) -> io::Result<Conn> {
        let ftp = open(&target)?;
        Ok(Conn { ftp: Some(ftp), target, last_used: Instant::now() })
    }

    pub fn shared(self) -> SharedConn {
        Arc::new(Mutex::new(self))
    }

    /// The live stream: probes a long-idle connection and reconnects when the server dropped it.
    fn ftp(&mut self) -> io::Result<&mut NativeTlsFtpStream> {
        let stale = self.last_used.elapsed() > IDLE_PROBE;
        let healthy = match self.ftp.as_mut() {
            Some(f) if stale => f.noop().is_ok(),
            Some(_) => true,
            None => false,
        };
        if !healthy {
            self.ftp = None;
            self.ftp = Some(open(&self.target)?);
        }
        self.last_used = Instant::now();
        Ok(self.ftp.as_mut().expect("connection was just opened"))
    }

    /// Drops a connection that is in an unknown state (after an aborted transfer).
    fn poison(&mut self) {
        self.ftp = None;
    }

    pub fn quit(&mut self) {
        if let Some(mut f) = self.ftp.take() {
            let _ = f.quit();
        }
    }

    /// Lists a folder (MLSD when the server has it, else LIST). `.` and `..` are left out.
    pub fn list(&mut self, dir: &str) -> io::Result<Vec<Entry>> {
        let ftp = self.ftp()?;
        let mut files: Vec<ListFile> = Vec::new();
        match ftp.mlsd(Some(dir)) {
            Ok(lines) => files.extend(lines.iter().filter_map(|l| ListFile::from_mlsx_line(l).ok())),
            Err(_) => {
                let lines = ftp.list(Some(dir)).map_err(err)?;
                files.extend(lines.iter().filter_map(|l| ListFile::try_from(l.as_str()).ok()));
            }
        }
        Ok(files
            .into_iter()
            .filter(|f| f.name() != "." && f.name() != "..")
            .map(|f| Entry {
                path: PathBuf::from(join(dir, f.name())),
                name: f.name().to_owned(),
                is_dir: f.is_directory(),
                size: if f.is_directory() { 0 } else { f.size() as u64 },
                modified: Some(f.modified()).filter(|t| *t > SystemTime::UNIX_EPOCH),
            })
            .collect())
    }

    pub fn mkdir(&mut self, path: &str) -> io::Result<()> {
        self.ftp()?.mkdir(path).map_err(err)
    }

    /// Deletes a file, or a folder with everything in it.
    pub fn remove(&mut self, path: &str, is_dir: bool) -> io::Result<()> {
        if !is_dir {
            return self.ftp()?.rm(path).map_err(err);
        }
        for e in self.list(path)? {
            self.remove(&remote_str(&e.path), e.is_dir)?;
        }
        self.ftp()?.rmdir(path).map_err(err)
    }
}

fn open(t: &Target) -> io::Result<NativeTlsFtpStream> {
    let addr = (t.host.as_str(), t.port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::other(format!("Cannot resolve {}", t.host)))?;
    let mut ftp = NativeTlsFtpStream::connect_timeout(addr, Duration::from_secs(15)).map_err(err)?;
    if t.secure {
        let mut builder = TlsConnector::builder();
        if t.insecure_tls {
            builder.danger_accept_invalid_certs(true).danger_accept_invalid_hostnames(true);
        }
        let connector = builder.build().map_err(|e| io::Error::other(e.to_string()))?;
        ftp = ftp.into_secure(NativeTlsConnector::from(connector), &t.host).map_err(err)?;
    }
    ftp.login(&t.user, &t.password).map_err(err)?;
    ftp.transfer_type(FileType::Binary).map_err(err)?;
    Ok(ftp)
}

/// Connects and reads the start folder. Blocks: call off the UI thread.
pub fn connect_and_list(target: Target) -> io::Result<(SharedConn, String, Vec<Entry>)> {
    let mut conn = Conn::connect(target.clone())?;
    let mut path = if target.path.is_empty() { "/".to_owned() } else { target.path.clone() };
    let entries = match conn.list(&path) {
        Ok(e) => e,
        // A start folder that is not there: fall back to the root rather than failing the login.
        Err(_) if path != "/" => {
            path = "/".to_owned();
            conn.list("/")?
        }
        Err(e) => return Err(e),
    };
    Ok((conn.shared(), path, entries))
}

// ---------------------------------------------------------------- transfers

/// An item to transfer: remote path and whether it is a folder.
#[derive(Clone, Debug)]
pub struct RemoteItem {
    pub path: String,
    pub is_dir: bool,
}

enum Step {
    Dir(PathBuf),
    File { remote: String, local: PathBuf, size: u64 },
}

fn lock(conn: &SharedConn) -> std::sync::MutexGuard<'_, Conn> {
    conn.lock().unwrap_or_else(|e| e.into_inner())
}

fn base_name(path: &str) -> &str {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or(path)
}

/// Downloads `items` into the local folder `dest_dir`. With `delete_source` the remote items are
/// removed afterwards, but only when everything was copied. Errors go to `p`.
pub fn download(conn: &SharedConn, items: &[RemoteItem], dest_dir: &Path, delete_source: bool, p: &Progress) {
    let mut steps = Vec::new();
    for item in items {
        let local = dest_dir.join(base_name(&item.path));
        if let Err(e) = plan_download(conn, item, &local, &mut steps) {
            p.fail(format!("{}: {e}", item.path));
            return;
        }
    }
    let total: u64 = steps.iter().map(|s| if let Step::File { size, .. } = s { *size } else { 0 }).sum();
    p.total.store(total, Ordering::Relaxed);
    let mut all_ok = true;
    for step in steps {
        if p.cancelled() {
            return;
        }
        match step {
            Step::Dir(local) => {
                if let Err(e) = fs::create_dir_all(&local) {
                    p.fail(format!("{}: {e}", local.display()));
                    all_ok = false;
                }
            }
            Step::File { remote, local, size } => match get_file(conn, &remote, &local, size, p) {
                Ok(true) => {}
                Ok(false) => all_ok = false,
                Err(e) => {
                    p.fail(format!("{remote}: {e}"));
                    all_ok = false;
                }
            },
        }
    }
    if delete_source && all_ok && !p.cancelled() {
        for item in items {
            if let Err(e) = lock(conn).remove(&item.path, item.is_dir) {
                p.fail(format!("Delete {}: {e}", item.path));
            }
        }
    }
}

fn plan_download(conn: &SharedConn, item: &RemoteItem, local: &Path, steps: &mut Vec<Step>) -> io::Result<()> {
    if !item.is_dir {
        // The size is only needed for the progress bar; take it from the parent's listing when cheap.
        let size = parent(&item.path).and_then(|d| lock(conn).list(&d).ok()).and_then(|l| l.into_iter().find(|e| remote_str(&e.path) == item.path)).map_or(0, |e| e.size);
        steps.push(Step::File { remote: item.path.clone(), local: local.to_path_buf(), size });
        return Ok(());
    }
    steps.push(Step::Dir(local.to_path_buf()));
    let children = lock(conn).list(&item.path)?;
    for c in children {
        let child = RemoteItem { path: remote_str(&c.path), is_dir: c.is_dir };
        if c.is_dir {
            plan_download(conn, &child, &local.join(&c.name), steps)?;
        } else {
            steps.push(Step::File { remote: child.path, local: local.join(&c.name), size: c.size });
        }
    }
    Ok(())
}

/// Downloads one file. `Ok(false)` = skipped or cancelled.
fn get_file(conn: &SharedConn, remote: &str, local: &Path, size: u64, p: &Progress) -> io::Result<bool> {
    let Some(target) = gc_core::jobs::claim_target(local, p) else {
        p.done.fetch_add(size, Ordering::Relaxed);
        return Ok(false);
    };
    let mut c = lock(conn);
    let ftp = c.ftp()?;
    let mut stream = ftp.retr_as_stream(remote).map_err(err)?;
    let mut out = File::create(&target)?;
    let mut buf = vec![0u8; BUF_SIZE];
    loop {
        if p.cancelled() {
            drop(out);
            let _ = fs::remove_file(&target);
            // An open data connection cannot be reused safely: ask the server to stop, then reconnect on next use.
            let _ = ftp.abort(stream);
            c.poison();
            return Ok(false);
        }
        let n = stream.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        p.done.fetch_add(n as u64, Ordering::Relaxed);
    }
    out.flush()?;
    ftp.finalize_retr_stream(stream).map_err(err)?;
    Ok(true)
}

/// Downloads one file into a fresh scratch folder (for F3 / F4 / Enter) and returns its local path.
pub fn download_to_scratch(conn: &SharedConn, remote: &str) -> io::Result<PathBuf> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = scratch_dir().join(NEXT.fetch_add(1, Ordering::Relaxed).to_string());
    fs::create_dir_all(&dir)?;
    let local = dir.join(base_name(remote));
    let p = Progress::default();
    get_file(conn, remote, &local, 0, &p)?;
    Ok(local)
}

pub fn scratch_dir() -> PathBuf {
    std::env::temp_dir().join(format!("ghost-commander-ftp-{}", std::process::id()))
}

/// Deletes the scratch folder of [`download_to_scratch`]; call on exit.
pub fn cleanup_scratch() {
    let _ = fs::remove_dir_all(scratch_dir());
}

/// Uploads local `srcs` into the remote folder `remote_dir`. With `delete_source` the local items are
/// removed afterwards, but only when everything was copied. Errors go to `p`.
pub fn upload(conn: &SharedConn, srcs: &[PathBuf], remote_dir: &str, delete_source: bool, p: &Progress) {
    let mut dirs: Vec<String> = Vec::new();
    let mut files: Vec<(PathBuf, String, u64)> = Vec::new();
    for src in srcs {
        let Some(name) = src.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
        plan_upload(src, &join(remote_dir, &name), &mut dirs, &mut files);
    }
    p.total.store(files.iter().map(|f| f.2).sum(), Ordering::Relaxed);
    for d in &dirs {
        // Existing folders make MKD fail; that is fine, a real problem shows up when a file is stored.
        let _ = lock(conn).mkdir(d);
    }
    let mut known: HashMap<String, HashSet<String>> = HashMap::new();
    let mut all_ok = true;
    for (local, remote, size) in files {
        if p.cancelled() {
            return;
        }
        match put_file(conn, &local, &remote, size, &mut known, p) {
            Ok(true) => {}
            Ok(false) => all_ok = false,
            Err(e) => {
                p.fail(format!("{}: {e}", local.display()));
                all_ok = false;
            }
        }
    }
    if delete_source && all_ok && !p.cancelled() {
        for src in srcs {
            let r = if src.is_dir() { fs::remove_dir_all(src) } else { fs::remove_file(src) };
            if let Err(e) = r {
                p.fail(format!("Delete {}: {e}", src.display()));
            }
        }
    }
}

fn plan_upload(local: &Path, remote: &str, dirs: &mut Vec<String>, files: &mut Vec<(PathBuf, String, u64)>) {
    if local.is_dir() {
        dirs.push(remote.to_owned());
        if let Ok(rd) = fs::read_dir(local) {
            for e in rd.flatten() {
                plan_upload(&e.path(), &join(remote, &e.file_name().to_string_lossy()), dirs, files);
            }
        }
    } else {
        files.push((local.to_path_buf(), remote.to_owned(), local.metadata().map_or(0, |m| m.len())));
    }
}

/// Uploads one file. `Ok(false)` = skipped or cancelled.
fn put_file(conn: &SharedConn, local: &Path, remote: &str, size: u64, known: &mut HashMap<String, HashSet<String>>, p: &Progress) -> io::Result<bool> {
    let mut remote = remote.to_owned();
    let dir = parent(&remote).unwrap_or_else(|| "/".into());
    if !known.contains_key(&dir) {
        let names = lock(conn).list(&dir).map(|l| l.into_iter().map(|e| e.name).collect()).unwrap_or_default();
        known.insert(dir.clone(), names);
    }
    if known[&dir].contains(base_name(&remote)) {
        match p.ask_conflict(Path::new(&remote)) {
            Resolution::Overwrite => {}
            Resolution::Skip | Resolution::Cancel => {
                p.done.fetch_add(size, Ordering::Relaxed);
                return Ok(false);
            }
            Resolution::Rename => {
                let taken = &known[&dir];
                let name = base_name(&remote).to_owned();
                let (stem, ext) = match name.rfind('.') {
                    Some(i) if i > 0 => (&name[..i], &name[i..]),
                    _ => (name.as_str(), ""),
                };
                let fresh = (2..).map(|n| format!("{stem} ({n}){ext}")).find(|c| !taken.contains(c)).expect("endless range");
                remote = join(&dir, &fresh);
            }
        }
    }
    known.entry(dir).or_default().insert(base_name(&remote).to_owned());
    let mut input = File::open(local)?;
    let mut c = lock(conn);
    let ftp = c.ftp()?;
    let mut stream = ftp.put_with_stream(&remote).map_err(err)?;
    let mut buf = vec![0u8; BUF_SIZE];
    loop {
        if p.cancelled() {
            drop(input);
            let _ = ftp.abort(stream);
            c.poison();
            // The partial file stays on the server unless it can be removed on a fresh connection.
            let _ = c.rm_quiet(&remote);
            return Ok(false);
        }
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        stream.write_all(&buf[..n])?;
        p.done.fetch_add(n as u64, Ordering::Relaxed);
    }
    ftp.finalize_put_stream(stream).map_err(err)?;
    Ok(true)
}

impl Conn {
    fn rm_quiet(&mut self, path: &str) -> io::Result<()> {
        self.ftp()?.rm(path).map_err(err)
    }
}

/// Deletes remote items as a job: progress counts items.
pub fn delete(conn: &SharedConn, items: &[RemoteItem], p: &Progress) {
    p.total.store(items.len() as u64, Ordering::Relaxed);
    for item in items {
        if p.cancelled() {
            return;
        }
        if let Err(e) = lock(conn).remove(&item.path, item.is_dir) {
            p.fail(format!("{}: {e}", item.path));
        }
        p.done.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn mkdir(conn: &SharedConn, path: &str) -> io::Result<()> {
    lock(conn).mkdir(path)
}

pub fn list(conn: &SharedConn, dir: &str) -> io::Result<Vec<Entry>> {
    lock(conn).list(dir)
}

pub fn disconnect(conn: &SharedConn) {
    lock(conn).quit();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_with_login() {
        let t = Target::from_url("ftp://mato:mojpass@lenovo-ts.com").unwrap();
        assert_eq!((t.host.as_str(), t.port, t.user.as_str(), t.password.as_str(), t.path.as_str()), ("lenovo-ts.com", 21, "mato", "mojpass", "/"));
        assert!(!t.secure);
        assert_eq!(t.label(), "ftp://mato@lenovo-ts.com");
    }

    #[test]
    fn url_encoded_and_port_and_path() {
        let t = Target::from_url("ftps://me:p%40ss%3Aw%2Frd@host.example:2121/pub/dir%20x").unwrap();
        assert_eq!((t.user.as_str(), t.password.as_str(), t.port, t.path.as_str()), ("me", "p@ss:w/rd", 2121, "/pub/dir x"));
        assert!(t.secure);
    }

    #[test]
    fn url_anonymous_and_bare_host() {
        let t = Target::from_url("ftp.example.org/files").unwrap();
        assert_eq!((t.user.as_str(), t.host.as_str(), t.path.as_str()), ("anonymous", "ftp.example.org", "/files"));
    }

    #[test]
    fn url_rejects_other_schemes() {
        assert!(Target::from_url("http://x").is_err());
        assert!(Target::from_url("ftp://").is_err());
        assert!(Target::from_url("ftp://h:abc").is_err());
    }

    #[test]
    fn remote_paths() {
        assert_eq!(join("/", "a"), "/a");
        assert_eq!(join("/a", "b"), "/a/b");
        assert_eq!(parent("/a/b"), Some("/a".into()));
        assert_eq!(parent("/a"), Some("/".into()));
        assert_eq!(parent("/"), None);
        assert_eq!(remote_str(Path::new("\\a\\b")), "/a/b");
    }
}
