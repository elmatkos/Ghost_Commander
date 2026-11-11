//! DISM image mounting without a UI: reads what is mounted, lists the images inside a WIM, and runs
//! `dism.exe` operations (mount, commit, unmount, remount, cleanup) in the background.
//!
//! DISM needs administrator rights. When this process is not elevated, every operation is started
//! through a UAC prompt (`Start-Process -Verb RunAs`); output goes to a temp file that is followed
//! while the operation runs, so a progress percentage and the full log are available either way.
//!
//! What is mounted is read from Windows (`Get-WindowsImage -Mounted`), so mounts made by other tools
//! show up too. Our own small record file adds what Windows does not tell: when we mounted it.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Debug, PartialEq)]
pub struct Mount {
    pub dir: PathBuf,
    pub wim: PathBuf,
    pub index: u32,
    pub read_only: bool,
    /// "Ok", "NeedsRemount" or "Invalid" as Windows reports it (empty when only our record knows it).
    pub status: String,
    /// Unix seconds when this app mounted it, if it did.
    pub since: Option<u64>,
}

impl Mount {
    pub fn needs_remount(&self) -> bool {
        self.status.eq_ignore_ascii_case("NeedsRemount")
    }
    pub fn is_ok(&self) -> bool {
        self.status.is_empty() || self.status.eq_ignore_ascii_case("Ok")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImageEntry {
    pub index: u32,
    pub name: String,
    pub description: String,
    pub bytes: u64,
}

/// One DISM job.
#[derive(Clone, Debug)]
pub enum Op {
    Mount { wim: PathBuf, index: u32, dir: PathBuf, read_only: bool },
    /// Save the changes, keep the image mounted.
    Commit { dir: PathBuf },
    UnmountCommit { dir: PathBuf },
    UnmountDiscard { dir: PathBuf },
    Remount { dir: PathBuf },
    /// `/Cleanup-Wim`: forget mounts whose folder or image is gone.
    Cleanup,
}

impl Op {
    pub fn title(&self) -> String {
        match self {
            Op::Mount { wim, index, dir, .. } => format!("Mount {} (index {index}) to {}", wim.display(), dir.display()),
            Op::Commit { dir } => format!("Commit changes of {}", dir.display()),
            Op::UnmountCommit { dir } => format!("Commit and unmount {}", dir.display()),
            Op::UnmountDiscard { dir } => format!("Discard changes and unmount {}", dir.display()),
            Op::Remount { dir } => format!("Remount {}", dir.display()),
            Op::Cleanup => "Clean up orphaned mounts".into(),
        }
    }

    /// The folder this job works on, if any.
    pub fn dir(&self) -> Option<&Path> {
        match self {
            Op::Mount { dir, .. } | Op::Commit { dir } | Op::UnmountCommit { dir } | Op::UnmountDiscard { dir } | Op::Remount { dir } => Some(dir),
            Op::Cleanup => None,
        }
    }

    /// Does the job end with the folder no longer being a mount point?
    pub fn unmounts(&self) -> bool {
        matches!(self, Op::UnmountCommit { .. } | Op::UnmountDiscard { .. })
    }

    /// The `dism.exe` command line (without the program name).
    pub fn dism_args(&self) -> String {
        match self {
            Op::Mount { wim, index, dir, read_only } => {
                format!("/Mount-Image /ImageFile:{} /Index:{index} /MountDir:{}{}", q(wim), q(dir), if *read_only { " /ReadOnly" } else { "" })
            }
            Op::Commit { dir } => format!("/Commit-Image /MountDir:{}", q(dir)),
            Op::UnmountCommit { dir } => format!("/Unmount-Image /MountDir:{} /Commit", q(dir)),
            Op::UnmountDiscard { dir } => format!("/Unmount-Image /MountDir:{} /Discard", q(dir)),
            Op::Remount { dir } => format!("/Remount-Image /MountDir:{}", q(dir)),
            Op::Cleanup => "/Cleanup-Wim".into(),
        }
    }
}

/// A path as a quoted batch-file argument (no trailing backslash, which would escape the quote).
fn q(p: &Path) -> String {
    let s = p.display().to_string();
    let s = s.trim_end_matches('\\');
    format!("\"{}\"", s.replace('%', "%%"))
}

// ---- state we remember --------------------------------------------------------

#[derive(Serialize, Deserialize, Default)]
struct Records {
    #[serde(default)]
    mount: Vec<Record>,
}

#[derive(Serialize, Deserialize, Clone)]
struct Record {
    dir: String,
    wim: String,
    index: u32,
    read_only: bool,
    since: u64,
}

fn records_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Ghost Commander").join("dism_mounts.toml"))
}

fn load_records() -> Records {
    records_path().and_then(|p| fs::read_to_string(p).ok()).and_then(|t| toml::from_str(&t).ok()).unwrap_or_default()
}

fn save_records(r: &Records) {
    if let Some(p) = records_path() {
        if let Some(d) = p.parent() {
            let _ = fs::create_dir_all(d);
        }
        if let Ok(t) = toml::to_string(r) {
            let _ = fs::write(p, t);
        }
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    a.display().to_string().trim_end_matches('\\').eq_ignore_ascii_case(b.display().to_string().trim_end_matches('\\'))
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// What we know without asking Windows: the remembered mounts whose folder still exists.
pub fn remembered_mounts() -> Vec<Mount> {
    load_records()
        .mount
        .into_iter()
        .filter(|r| Path::new(&r.dir).is_dir())
        .map(|r| Mount { dir: r.dir.into(), wim: r.wim.into(), index: r.index, read_only: r.read_only, status: String::new(), since: Some(r.since) })
        .collect()
}

// ---- asking Windows -------------------------------------------------------------

/// True when this process runs with administrator rights.
pub fn is_elevated() -> bool {
    #[cfg(windows)]
    // SAFETY: plain token query on our own process; the handle is closed again.
    unsafe {
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
        use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut e = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut len = 0u32;
        let ok = GetTokenInformation(token, TokenElevation, &mut e as *mut _ as *mut _, std::mem::size_of::<TOKEN_ELEVATION>() as u32, &mut len);
        CloseHandle(token);
        ok != 0 && e.TokenIsElevated != 0
    }
    #[cfg(not(windows))]
    false
}

fn powershell(script: &str) -> Result<String, String> {
    let mut cmd = Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", script]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd.output().map_err(|e| format!("cannot start PowerShell: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() {
        Ok(text)
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(format!("{}{}", err.trim(), if text.trim().is_empty() { String::new() } else { format!(" {}", text.trim()) }))
    }
}

/// The images Windows currently has mounted, with our records merged in. When Windows cannot be
/// asked (no admin rights), the remembered mounts are returned together with the reason.
pub fn query_mounts() -> (Vec<Mount>, Option<String>) {
    let script = "Get-WindowsImage -Mounted | ForEach-Object { \"$($_.Path)`t$($_.ImagePath)`t$($_.ImageIndex)`t$($_.MountMode)`t$($_.MountStatus)\" }";
    let records = load_records();
    match powershell(script) {
        Ok(text) => {
            let mut found = parse_mounts(&text);
            for m in &mut found {
                m.since = records.mount.iter().find(|r| same_path(Path::new(&r.dir), &m.dir)).map(|r| r.since);
            }
            // Forget records of mounts that are gone.
            let kept: Vec<Record> = records.mount.iter().filter(|r| found.iter().any(|m| same_path(&m.dir, Path::new(&r.dir)))).cloned().collect();
            if kept.len() != records.mount.len() {
                save_records(&Records { mount: kept });
            }
            (found, None)
        }
        Err(e) => (remembered_mounts(), Some(e)),
    }
}

fn parse_mounts(text: &str) -> Vec<Mount> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.trim_end().split('\t').collect();
            if f.len() < 5 || f[0].is_empty() {
                return None;
            }
            Some(Mount {
                dir: f[0].into(),
                wim: f[1].into(),
                index: f[2].trim().parse().unwrap_or(1),
                read_only: f[3].to_ascii_lowercase().contains("only"),
                status: f[4].trim().to_owned(),
                since: None,
            })
        })
        .collect()
}

// ---- reading a WIM -------------------------------------------------------------------

/// The images inside a `.wim` / `.esd` / `.swm`, read from its XML block (no admin rights needed).
pub fn wim_images(path: &Path) -> Result<Vec<ImageEntry>, String> {
    let mut f = fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut head = [0u8; 208];
    f.read_exact(&mut head).map_err(|_| "not a WIM file (too short)".to_string())?;
    if &head[0..5] != b"MSWIM" {
        return Err("not a WIM file (no MSWIM signature)".into());
    }
    // XML resource header at 0x48: 7 bytes size, 1 byte flags, 8 bytes offset, 8 bytes original size.
    let size = u64::from_le_bytes([head[0x48], head[0x49], head[0x4A], head[0x4B], head[0x4C], head[0x4D], head[0x4E], 0]);
    let flags = head[0x4F];
    let offset = u64::from_le_bytes(head[0x50..0x58].try_into().unwrap());
    if flags & 0x04 != 0 {
        return Err("the image list is compressed; use DISM /Get-WimInfo".into());
    }
    if size == 0 || size > 64 << 20 {
        return Err("the WIM has no readable image list".into());
    }
    use std::io::{Seek, SeekFrom};
    f.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
    let mut raw = vec![0u8; size as usize];
    f.read_exact(&mut raw).map_err(|e| format!("cannot read the image list: {e}"))?;
    let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let xml = String::from_utf16_lossy(&units);
    Ok(parse_wim_xml(&xml))
}

fn tag<'a>(block: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let start = block.find(&open)? + open.len();
    let end = block[start..].find(&format!("</{name}>"))? + start;
    Some(&block[start..end])
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

fn parse_wim_xml(xml: &str) -> Vec<ImageEntry> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find("<IMAGE INDEX=\"") {
        let after = &rest[i + 14..];
        let Some(q) = after.find('"') else { break };
        let index: u32 = after[..q].parse().unwrap_or(0);
        let end = after.find("</IMAGE>").unwrap_or(after.len());
        let block = &after[..end];
        let name = tag(block, "NAME").or_else(|| tag(block, "DISPLAYNAME")).map(unescape).unwrap_or_default();
        let description = tag(block, "DESCRIPTION").or_else(|| tag(block, "DISPLAYDESCRIPTION")).map(unescape).unwrap_or_default();
        let bytes = tag(block, "TOTALBYTES").and_then(|t| t.trim().parse().ok()).unwrap_or(0);
        if index > 0 {
            out.push(ImageEntry { index, name, description, bytes });
        }
        rest = &after[end..];
    }
    out
}

// ---- running a job ----------------------------------------------------------------------

#[derive(Default)]
pub struct RunState {
    /// Everything `dism.exe` printed so far.
    pub output: String,
    /// Progress of the current step, 0..=100.
    pub percent: Option<f32>,
    /// `None` while running, then the outcome.
    pub finished: Option<Result<(), String>>,
}

/// A job running in the background.
#[derive(Clone)]
pub struct Run {
    pub op: Op,
    pub state: Arc<Mutex<RunState>>,
}

impl Run {
    pub fn snapshot(&self) -> (String, Option<f32>, Option<Result<(), String>>) {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        (s.output.clone(), s.percent, s.finished.clone())
    }
}

/// Starts the job. `on_change` is called whenever the output changed or the job ended (to repaint).
pub fn spawn(op: Op, on_change: impl Fn() + Send + 'static) -> Run {
    let state = Arc::new(Mutex::new(RunState::default()));
    let run = Run { op: op.clone(), state: state.clone() };
    std::thread::spawn(move || {
        let result = execute(&op, &state, &on_change);
        if result.is_ok() {
            update_records(&op);
        }
        if let Ok(mut s) = state.lock() {
            if result.is_ok() {
                s.percent = Some(100.0);
            }
            s.finished = Some(result);
        }
        on_change();
    });
    run
}

fn update_records(op: &Op) {
    let mut r = load_records();
    match op {
        Op::Mount { wim, index, dir, read_only } => {
            r.mount.retain(|m| !same_path(Path::new(&m.dir), dir));
            r.mount.push(Record { dir: dir.display().to_string(), wim: wim.display().to_string(), index: *index, read_only: *read_only, since: now() });
        }
        Op::UnmountCommit { dir } | Op::UnmountDiscard { dir } => r.mount.retain(|m| !same_path(Path::new(&m.dir), dir)),
        _ => return,
    }
    save_records(&r);
}

fn set_output(state: &Arc<Mutex<RunState>>, text: &str) {
    if let Ok(mut s) = state.lock() {
        s.percent = last_percent(text).or(s.percent);
        s.output = clean_output(text);
    }
}

/// The last `12.3%` in the text (DISM redraws its progress bar with carriage returns).
fn last_percent(text: &str) -> Option<f32> {
    let i = text.rfind('%')?;
    let digits: String = text[..i].chars().rev().take_while(|c| c.is_ascii_digit() || *c == '.').collect::<Vec<_>>().into_iter().rev().collect();
    digits.parse().ok()
}

/// Progress bar redraws collapse to their last state, so the log reads as lines.
fn clean_output(text: &str) -> String {
    let lines: Vec<String> = text
        .replace("\r\n", "\n")
        .split('\n')
        .map(|line| line.rsplit('\r').find(|p| !p.trim().is_empty()).unwrap_or("").trim_end().to_owned())
        .collect();
    let bar = |l: &str| l.starts_with('[') && l.contains('%');
    let mut out: Vec<&str> = Vec::new();
    for l in &lines {
        if bar(l) && out.last().is_some_and(|p| bar(p)) {
            out.pop();
        }
        out.push(l);
    }
    out.join("\n")
}

fn execute(op: &Op, state: &Arc<Mutex<RunState>>, on_change: &dyn Fn()) -> Result<(), String> {
    let tmp = std::env::temp_dir().join(format!("gc_dism_{}_{}", std::process::id(), now()));
    fs::create_dir_all(&tmp).map_err(|e| format!("cannot create {}: {e}", tmp.display()))?;
    let (script, out, code) = (tmp.join("run.cmd"), tmp.join("out.txt"), tmp.join("code.txt"));
    let mut body = String::from("@echo off\r\n");
    if let Op::Mount { dir, .. } = op {
        // The mount folder is created when it does not exist.
        body += &format!("if not exist {} mkdir {}\r\n", q(dir), q(dir));
    }
    body += &format!("dism.exe {} > {} 2>&1\r\necho %errorlevel% > {}\r\n", op.dism_args(), q(&out), q(&code));
    fs::write(&script, body).map_err(|e| e.to_string())?;

    let elevated = is_elevated();
    let mut cmd = if elevated {
        let mut c = Command::new("cmd.exe");
        c.arg("/c").arg(&script);
        c
    } else {
        let mut c = Command::new("powershell.exe");
        let s = script.display().to_string().replace('\'', "''");
        c.args(["-NoProfile", "-NonInteractive", "-Command", &format!("Start-Process -FilePath cmd.exe -ArgumentList '/c','\"{s}\"' -Verb RunAs -WindowStyle Hidden -Wait")]);
        c
    };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.stderr(std::process::Stdio::piped()).stdout(std::process::Stdio::null()).spawn().map_err(|e| format!("cannot start the job: {e}"))?;
    let mut last = String::new();
    let status = loop {
        if let Ok(bytes) = fs::read(&out) {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            if text != last {
                set_output(state, &text);
                on_change();
                last = text;
            }
        }
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => std::thread::sleep(Duration::from_millis(250)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let mut launcher_err = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut launcher_err);
    }
    if let Ok(bytes) = fs::read(&out) {
        set_output(state, &String::from_utf8_lossy(&bytes));
    }
    let exit = fs::read_to_string(&code).ok().and_then(|t| t.trim().parse::<i64>().ok());
    let _ = fs::remove_dir_all(&tmp);
    match exit {
        Some(0) => Ok(()),
        Some(n) => Err(format!("DISM ended with error code {n} (see the log)")),
        None if !elevated => Err(if launcher_err.contains("canceled") || launcher_err.contains("cancelled") { "Administrator approval was cancelled.".into() } else { format!("The job did not run (UAC refused?): {}", launcher_err.trim()) }),
        None => Err(format!("The job did not report a result (exit {status})")),
    }
}

// ---- checks before mounting -------------------------------------------------------------------

/// Why `dir` cannot be a mount folder, if it cannot (it is created when missing).
pub fn mount_dir_problem(dir: &Path, mounts: &[Mount]) -> Option<String> {
    if dir.as_os_str().is_empty() {
        return Some("Enter a mount folder.".into());
    }
    if mounts.iter().any(|m| same_path(&m.dir, dir)) {
        return Some("An image is already mounted there.".into());
    }
    if dir.is_file() {
        return Some("That is a file, not a folder.".into());
    }
    if dir.is_dir() && fs::read_dir(dir).map(|mut d| d.next().is_some()).unwrap_or(false) {
        return Some("The folder is not empty (DISM needs an empty folder).".into());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_lines() {
        let op = Op::Mount { wim: r"C:\a b\x.wim".into(), index: 2, dir: r"C:\mnt\".into(), read_only: true };
        assert_eq!(op.dism_args(), r#"/Mount-Image /ImageFile:"C:\a b\x.wim" /Index:2 /MountDir:"C:\mnt" /ReadOnly"#);
        assert_eq!(Op::UnmountDiscard { dir: r"D:\m".into() }.dism_args(), r#"/Unmount-Image /MountDir:"D:\m" /Discard"#);
        assert_eq!(Op::Commit { dir: r"D:\m".into() }.dism_args(), r#"/Commit-Image /MountDir:"D:\m""#);
    }

    #[test]
    fn mounted_list_is_parsed() {
        let m = parse_mounts("C:\\mount\tD:\\x.wim\t1\tReadWrite\tOk\r\nE:\\m2\tE:\\y.wim\t3\tReadOnly\tNeedsRemount\r\n\r\n");
        assert_eq!(m.len(), 2);
        assert!(!m[0].read_only && m[0].is_ok());
        assert!(m[1].read_only && m[1].needs_remount() && m[1].index == 3);
    }

    #[test]
    fn progress_and_output() {
        assert_eq!(last_percent("[==   ] 12.5%\r[===  ] 40.0%"), Some(40.0));
        assert_eq!(clean_output("a\r\nb\r[=] 10%\r[==] 20%\r\n"), "a\n[==] 20%\n");
    }

    #[test]
    fn wim_xml_is_parsed() {
        let xml = "<WIM><IMAGE INDEX=\"1\"><NAME>Win &amp; Co</NAME><DESCRIPTION>d1</DESCRIPTION><TOTALBYTES>1000</TOTALBYTES></IMAGE><IMAGE INDEX=\"2\"><NAME>B</NAME></IMAGE></WIM>";
        let v = parse_wim_xml(xml);
        assert_eq!(v.len(), 2);
        assert_eq!((v[0].name.as_str(), v[0].bytes), ("Win & Co", 1000));
        assert_eq!(v[1].index, 2);
    }

    #[test]
    fn mount_folder_checks() {
        let d = std::env::temp_dir().join(format!("gc_dism_t_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        assert!(mount_dir_problem(&d, &[]).is_none()); // missing: will be created
        fs::create_dir_all(&d).unwrap();
        assert!(mount_dir_problem(&d, &[]).is_none()); // empty
        fs::write(d.join("f"), b"x").unwrap();
        assert!(mount_dir_problem(&d, &[]).is_some()); // not empty
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn reads_a_real_wim_when_present() {
        let p = Path::new(r"C:\Windows\System32\DrtmAuthTxt.wim");
        if p.is_file() {
            let imgs = wim_images(p).unwrap();
            assert!(!imgs.is_empty(), "{imgs:?}");
        }
    }
}
