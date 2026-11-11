use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;

use eframe::egui::{self, Align2, FontId, Sense, Stroke, StrokeKind};
use gc_archive::ArchiveIndex;
use gc_core::{list_dir, Entry};
use gc_ftp::{RemoteItem, SharedConn, Target};

use crate::theme::Theme;

/// An archive opened as a folder: the panel shows `inner` (a `/` path) of `index`.
struct ArchiveView {
    index: Arc<ArchiveIndex>,
    inner: String,
    /// Where the archive file lives, for the header (nested archives: `outer.zip\dir\inner.zip`).
    label: String,
}

/// An archive being opened on a worker thread.
struct Loading {
    rx: Receiver<io::Result<ArchiveIndex>>,
    label: String,
    /// Opened from inside the current archive (keeps the current one on the stack).
    nested: bool,
}

/// An FTP server shown as a folder tree. The panel's `cwd` is the remote folder (a `/` path).
struct FtpView {
    conn: SharedConn,
    /// `ftp://user@host` for the header.
    label: String,
    /// Local folder to go back to when leaving the server.
    back: PathBuf,
}

/// What an FTP worker thread brings back.
enum FtpReply {
    Connected { conn: SharedConn, label: String, path: String, entries: Vec<Entry> },
    Listed { path: String, entries: Vec<Entry> },
}

/// A connect or folder read running on a worker thread.
struct FtpWork {
    rx: Receiver<io::Result<FtpReply>>,
    label: String,
    /// Name to put the cursor on when the listing arrives.
    select: Option<String>,
}

/// What to do with a file once it is on disk.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FileUse {
    Open,
    View,
    Edit,
}

/// Something the user asked for that only the app can do (it owns the other panel and the jobs).
pub enum PanelAction {
    /// Right click on disk items: show the Windows shell menu for them.
    ShellMenu(Vec<PathBuf>),
    /// Enter on a plain file: open it with the Windows default program.
    Open(PathBuf),
    /// A file inside an archive: extract it to the scratch folder, then use it.
    ArchiveFile { index: Arc<ArchiveIndex>, inner: String, mode: FileUse },
    /// Context menu of a server item: rename it.
    Rename,
    /// Open the FTP connection log (button, or after a failed connect).
    FtpLog,
    /// A file on an FTP server: download it to the scratch folder, then use it.
    FtpFile { conn: SharedConn, remote: String, mode: FileUse },
    /// Context menu of an archive item: extract the selection to the other panel.
    Extract,
}

pub struct Panel {
    /// Folder on disk; while an archive is open, the archive file itself.
    pub cwd: PathBuf,
    archive: Option<ArchiveView>,
    /// Archives we descended out of to reach the current nested archive, outermost first.
    stack: Vec<ArchiveView>,
    loading: Option<Loading>,
    ftp: Option<FtpView>,
    ftp_work: Option<FtpWork>,
    /// The last connect attempt failed (keeps the Log button visible).
    ftp_failed: bool,
    /// Set by the app from Config > Options: leave a vanished folder / drive for an existing one.
    pub switch_on_missing: bool,
    select_next: Option<String>,
    ctx: egui::Context,
    pub action: Option<PanelAction>,
    /// The folder as read and sorted; `entries` is what is shown (filtered, with `..`).
    listing: Vec<Entry>,
    entries: Vec<Entry>,
    sort: SortKey,
    sort_desc: bool,
    filter: String,
    filter_open: bool,
    filter_focus: bool,
    cursor: usize,
    marked: HashSet<PathBuf>,
    error: Option<String>,
    scroll_to_cursor: bool,
    last_offset: f32,
    last_height: f32,
    /// Modified time of the folder when it was last read (to notice files created elsewhere).
    stamp: Option<std::time::SystemTime>,
}

impl Panel {
    pub fn new(cwd: PathBuf, ctx: egui::Context) -> Self {
        let mut p = Self {
            cwd,
            archive: None,
            stack: Vec::new(),
            loading: None,
            ftp: None,
            ftp_work: None,
            ftp_failed: false,
            switch_on_missing: false,
            select_next: None,
            ctx,
            action: None,
            listing: Vec::new(),
            entries: Vec::new(),
            sort: SortKey::Name,
            sort_desc: false,
            filter: String::new(),
            filter_open: false,
            filter_focus: false,
            cursor: 0,
            marked: HashSet::new(),
            error: None,
            scroll_to_cursor: false,
            last_offset: 0.0,
            last_height: 0.0,
            stamp: None,
        };
        p.reload();
        p
    }

    fn folder_stamp(&self) -> Option<std::time::SystemTime> {
        std::fs::metadata(&self.cwd).and_then(|m| m.modified()).ok()
    }

    /// Re-reads the folder if something was added, removed or renamed in it behind our back.
    pub fn refresh_if_changed(&mut self) {
        let on_disk = self.archive.is_none() && self.ftp.is_none() && self.loading.is_none();
        if on_disk && self.switch_on_missing && !self.cwd.exists() {
            if let Some(near) = nearest_existing(&self.cwd) {
                self.go_to(near, None);
                return;
            }
        }
        if on_disk && self.folder_stamp() != self.stamp {
            self.reload();
        }
    }

    /// Re-reads the directory and keeps the cursor on the same name when possible.
    pub fn reload(&mut self) {
        let keep = self.entries.get(self.cursor).map(|e| e.name.clone());
        if self.ftp.is_some() {
            // A server read can take long: do it on a worker, the old listing stays until it arrives.
            let path = gc_ftp::remote_str(&self.cwd);
            self.ftp_list(path, keep);
            return;
        }
        self.stamp = if self.archive.is_none() { self.folder_stamp() } else { None };
        let listing = match &self.archive {
            Some(a) => Ok(a.index.list(&a.inner)),
            None => list_dir(&self.cwd),
        };
        match listing {
            Ok(entries) => {
                self.listing = entries;
                self.error = None;
            }
            Err(e) => {
                self.listing.clear();
                self.error = Some(e.to_string());
            }
        }
        sort_entries(&mut self.listing, self.sort, self.sort_desc);
        self.apply_view(keep);
        self.marked.retain(|m| self.listing.iter().any(|e| &e.path == m));
        self.scroll_to_cursor = true;
    }

    /// Builds the shown list from the raw listing: quick filter applied, `..` on top.
    fn apply_view(&mut self, keep: Option<String>) {
        let needle = self.filter.to_lowercase();
        let mut entries: Vec<Entry> = if self.error.is_some() {
            Vec::new()
        } else {
            self.listing.iter().filter(|e| needle.is_empty() || e.name.to_lowercase().contains(&needle)).cloned().collect()
        };
        if self.ftp.is_some() {
            // A way up inside the server; the root has none (Disconnect leaves the server, so one never
            // ends up on a local drive by accident).
            if let Some(parent) = gc_ftp::parent(&gc_ftp::remote_str(&self.cwd)) {
                entries.insert(0, Entry { name: PARENT.into(), path: PathBuf::from(parent), is_dir: true, size: 0, modified: None });
            }
        } else if let (Some(parent), None) = (self.cwd.parent(), &self.error) {
            // Inside an archive there is always a way up (to the folder holding it).
            entries.insert(0, Entry { name: PARENT.into(), path: parent.to_path_buf(), is_dir: true, size: 0, modified: None });
        }
        self.entries = entries;
        self.cursor = keep
            .and_then(|n| self.entries.iter().position(|e| e.name == n))
            .unwrap_or_else(|| self.cursor.min(self.entries.len().saturating_sub(1)));
        self.scroll_to_cursor = true;
    }

    /// Sorts by `key`; choosing the same key again reverses the order.
    pub fn set_sort(&mut self, key: SortKey) {
        if self.sort == key {
            self.sort_desc = !self.sort_desc;
        } else {
            self.sort = key;
            // Biggest and newest first is what one usually wants from these two.
            self.sort_desc = matches!(key, SortKey::Size | SortKey::Date);
        }
        let keep = self.entries.get(self.cursor).map(|e| e.name.clone());
        sort_entries(&mut self.listing, self.sort, self.sort_desc);
        self.apply_view(keep);
    }

    pub fn filter_open(&self) -> bool {
        self.filter_open
    }

    /// Shows the quick filter box (Ctrl+F) and puts the keyboard into it.
    pub fn open_filter(&mut self) {
        self.filter_open = true;
        self.filter_focus = true;
    }

    /// Esc: drops the filter text and shows everything again.
    pub fn close_filter(&mut self) {
        self.filter_open = false;
        if !self.filter.is_empty() {
            self.filter.clear();
            let keep = self.entries.get(self.cursor).map(|e| e.name.clone());
            self.apply_view(keep);
        }
    }

    /// Enter in the filter box: stop typing but keep the filtered list.
    pub fn finish_filter(&mut self) {
        self.filter_open = false;
        self.filter_focus = false;
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let last = self.entries.len().saturating_sub(1) as isize;
        self.cursor = (self.cursor as isize + delta).clamp(0, last) as usize;
        self.scroll_to_cursor = true;
    }

    pub fn cursor_to_start(&mut self) {
        self.cursor = 0;
        self.scroll_to_cursor = true;
    }

    pub fn cursor_to_end(&mut self) {
        self.cursor = self.entries.len().saturating_sub(1);
        self.scroll_to_cursor = true;
    }

    /// Visible rows, used for PageUp / PageDown.
    pub fn page_size(&self) -> isize {
        ((self.last_height / ROW_H) as isize - 1).max(1)
    }

    pub fn toggle_mark_and_advance(&mut self) {
        if let Some(e) = self.entries.get(self.cursor).filter(|e| e.name != PARENT) {
            if !self.marked.remove(&e.path) {
                self.marked.insert(e.path.clone());
            }
        }
        self.move_cursor(1);
    }

    /// Opens the folder or archive under the cursor (or goes up on the `..` row).
    pub fn enter(&mut self) {
        if self.loading.is_some() || self.ftp_work.is_some() {
            return;
        }
        let Some(e) = self.entries.get(self.cursor).cloned() else { return };
        if e.name == PARENT {
            self.go_up();
        } else if let Some(v) = &self.ftp {
            if e.is_dir {
                self.ftp_list(gc_ftp::remote_str(&e.path), None);
            } else {
                self.action = Some(PanelAction::FtpFile { conn: v.conn.clone(), remote: gc_ftp::remote_str(&e.path), mode: FileUse::Open });
            }
        } else if let Some(a) = &mut self.archive {
            if e.is_dir {
                a.inner = join_inner(&a.inner, &e.name);
                self.enter_listing();
            } else if gc_archive::is_archive(&e.path) {
                let index = a.index.clone();
                let inner = join_inner(&a.inner, &e.name);
                let label = format!("{}\\{}", self.location(), e.name);
                self.start_loading(label, true, move || gc_archive::open_nested(&index, &inner));
            } else {
                let index = a.index.clone();
                let inner = join_inner(&a.inner, &e.name);
                self.action = Some(PanelAction::ArchiveFile { index, inner, mode: FileUse::Open });
            }
        } else if e.is_dir {
            self.go_to(e.path, None);
        } else if gc_archive::is_archive(&e.path) {
            let path = e.path;
            self.start_loading(path.display().to_string(), false, move || gc_archive::open(&path));
        } else {
            self.action = Some(PanelAction::Open(e.path));
        }
    }

    /// Opens an archive on a worker thread so big files never freeze the window.
    fn start_loading(&mut self, label: String, nested: bool, work: impl FnOnce() -> io::Result<ArchiveIndex> + Send + 'static) {
        let (tx, rx) = mpsc::channel();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(work());
            ctx.request_repaint();
        });
        self.loading = Some(Loading { rx, label, nested });
    }

    /// Picks up a finished archive open; called every frame.
    pub fn poll(&mut self) {
        self.poll_ftp();
        let Some(l) = &self.loading else { return };
        let result = match l.rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err(io::Error::other("worker stopped")),
        };
        let Some(l) = self.loading.take() else { return };
        match result {
            Ok(index) => {
                let outer = self.archive.take();
                if l.nested {
                    self.stack.extend(outer);
                }
                self.cwd = index.file.clone();
                self.archive = Some(ArchiveView { index: Arc::new(index), inner: String::new(), label: l.label });
                self.enter_listing();
            }
            Err(e) => self.error = Some(format!("{}: {e}", l.label)),
        }
    }

    /// Fresh listing at the top of a new location.
    fn enter_listing(&mut self) {
        self.entries.clear();
        self.cursor = 0;
        self.marked.clear();
        self.filter.clear();
        self.filter_open = false;
        self.reload();
    }

    /// Switches to another root (e.g. a drive).
    pub fn open_root(&mut self, path: PathBuf) {
        self.go_to(path, None);
    }

    pub fn go_up(&mut self) {
        self.loading = None; // Backspace also cancels an archive that is still opening
        if self.ftp.is_some() {
            self.ftp_work = None;
            let here = gc_ftp::remote_str(&self.cwd);
            match gc_ftp::parent(&here) {
                Some(parent) => {
                    let came_from = here.trim_end_matches('/').rsplit('/').next().map(str::to_owned);
                    self.ftp_list(parent, came_from);
                }
                None => {} // at the root of the server: stay (Disconnect leaves)
            }
            return;
        }
        if let Some(a) = &mut self.archive {
            if let Some((parent, name)) = a.inner.rsplit_once('/') {
                let (parent, name) = (parent.to_owned(), name.to_owned());
                a.inner = parent;
                self.enter_listing();
                self.select_name(&name);
                return;
            }
            if !a.inner.is_empty() {
                let name = std::mem::take(&mut a.inner);
                self.enter_listing();
                self.select_name(&name);
                return;
            }
            // At the root of a nested archive: back into the archive that holds it.
            if let Some(outer) = self.stack.pop() {
                let came_from = self.cwd.file_name().map(|n| n.to_string_lossy().into_owned());
                self.cwd = outer.index.file.clone();
                self.archive = Some(outer);
                self.enter_listing();
                if let Some(name) = came_from {
                    self.select_name(&name);
                }
                return;
            }
            // At the archive root: leave the archive, landing on the archive file.
            self.archive = None;
            if let Some(parent) = self.cwd.parent().map(|p| p.to_path_buf()) {
                let came_from = self.cwd.file_name().map(|n| n.to_string_lossy().into_owned());
                self.go_to(parent, came_from);
            }
            return;
        }
        if let Some(parent) = self.cwd.parent().map(|p| p.to_path_buf()) {
            let came_from = self.cwd.file_name().map(|n| n.to_string_lossy().into_owned());
            self.go_to(parent, came_from);
        }
    }

    fn select_name(&mut self, name: &str) {
        if let Some(i) = self.entries.iter().position(|e| e.name == name) {
            self.cursor = i;
        }
    }

    fn go_to(&mut self, path: PathBuf, select: Option<String>) {
        self.loading = None;
        self.ftp_work = None;
        let previous_ftp = self.ftp.take();
        let previous_archive = self.archive.take();
        let previous_stack = std::mem::take(&mut self.stack);
        let previous = std::mem::replace(&mut self.cwd, path);
        self.enter_listing();
        if self.error.is_some() {
            // Keep the old listing visible, show the error on top of it.
            let err = self.error.take();
            self.cwd = previous;
            self.archive = previous_archive;
            self.stack = previous_stack;
            self.ftp = previous_ftp;
            self.reload();
            self.error = err;
        } else {
            if let Some(v) = previous_ftp {
                std::thread::spawn(move || gc_ftp::disconnect(&v.conn));
            }
            if let Some(name) = select {
                self.select_name(&name);
            }
        }
    }

    /// The folder on disk this panel shows, `None` while an archive is open.
    pub fn disk_folder(&self) -> Option<&Path> {
        (self.archive.is_none() && self.ftp.is_none()).then_some(self.cwd.as_path())
    }

    /// Location text for the header, e.g. `C:\data\x.zip\docs`.
    fn location(&self) -> String {
        if let Some(v) = &self.ftp {
            return format!("{}{}", v.label, gc_ftp::remote_str(&self.cwd));
        }
        match &self.archive {
            Some(a) if !a.inner.is_empty() => format!("{}\\{}", a.label, a.inner.replace('/', "\\")),
            Some(a) => a.label.clone(),
            None => self.cwd.display().to_string(),
        }
    }

    /// While an archive is open: the archive and the selected inner paths (marked, or under the cursor).
    pub fn archive_selection(&self) -> Option<(Arc<ArchiveIndex>, Vec<String>)> {
        let a = self.archive.as_ref()?;
        let inner = self
            .targets()
            .iter()
            .filter_map(|p| p.strip_prefix(&self.cwd).ok())
            .map(|rel| rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/"))
            .collect();
        Some((a.index.clone(), inner))
    }

    /// The plain file under the cursor, for F3 / F4 (not a folder, not inside an archive).
    pub fn current_file(&self) -> Option<PathBuf> {
        let e = self.entries.get(self.cursor)?;
        (self.archive.is_none() && self.ftp.is_none() && !e.is_dir && e.name != PARENT).then(|| e.path.clone())
    }

    /// The plain file under the cursor while an archive is open: archive and inner path.
    pub fn archive_file(&self) -> Option<(Arc<ArchiveIndex>, String)> {
        let a = self.archive.as_ref()?;
        let e = self.entries.get(self.cursor).filter(|e| !e.is_dir && e.name != PARENT)?;
        Some((a.index.clone(), join_inner(&a.inner, &e.name)))
    }

    /// Images of the shown folder in listing order, for browsing in the image viewer.
    pub fn image_files(&self) -> Vec<PathBuf> {
        let on_disk = self.archive.is_none() && self.ftp.is_none();
        self.entries.iter().filter(|e| on_disk && !e.is_dir && crate::image_viewer::is_image(&e.path)).map(|e| e.path.clone()).collect()
    }

    /// Connects to an FTP server on a worker thread; the panel switches to it when logged in.
    pub fn connect_ftp(&mut self, target: Target) {
        let label = target.label();
        let (tx, rx) = mpsc::channel();
        let ctx = self.ctx.clone();
        let l = label.clone();
        std::thread::spawn(move || {
            let reply = gc_ftp::connect_and_list(target).map(|(conn, path, entries)| FtpReply::Connected { conn, label: l, path, entries });
            let _ = tx.send(reply);
            ctx.request_repaint();
        });
        self.loading = None;
        self.ftp_work = Some(FtpWork { rx, label, select: None });
        self.error = None;
        self.ftp_failed = false;
    }

    /// Reads remote folder `path` on a worker thread and shows it when it arrives.
    fn ftp_list(&mut self, path: String, select: Option<String>) {
        let Some(v) = &self.ftp else { return };
        let conn = v.conn.clone();
        let (tx, rx) = mpsc::channel();
        let ctx = self.ctx.clone();
        let p = path.clone();
        std::thread::spawn(move || {
            let reply = gc_ftp::list(&conn, &p).map(|entries| FtpReply::Listed { path: p, entries });
            let _ = tx.send(reply);
            ctx.request_repaint();
        });
        self.ftp_work = Some(FtpWork { rx, label: path, select });
    }

    /// Puts the cursor on `name` once the next listing of a server folder arrives (e.g. a folder just created).
    pub fn select_after_reload(&mut self, name: &str) {
        self.select_next = Some(name.to_owned());
    }

    /// Short name for the tab strip.
    pub fn tab_title(&self) -> String {
        let name = match self.cwd.file_name() {
            Some(n) => n.to_string_lossy().into_owned(),
            None => self.cwd.display().to_string(),
        };
        let name = if self.ftp.is_some() { format!("ftp: {name}") } else { name };
        if name.chars().count() > 18 {
            format!("{}...", name.chars().take(16).collect::<String>())
        } else {
            name
        }
    }

    /// The folder to open again at the next start: the server panel's previous folder, an archive's folder.
    pub fn restore_dir(&self) -> PathBuf {
        if let Some(v) = &self.ftp {
            return v.back.clone();
        }
        if self.archive.is_some() {
            let file = self.stack.first().map(|a| a.index.file.clone()).unwrap_or_else(|| self.cwd.clone());
            return file.parent().map(Path::to_path_buf).unwrap_or(file);
        }
        self.cwd.clone()
    }

    fn poll_ftp(&mut self) {
        let Some(w) = &self.ftp_work else { return };
        let result = match w.rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err(io::Error::other("worker stopped")),
        };
        let Some(w) = self.ftp_work.take() else { return };
        match result {
            Ok(FtpReply::Connected { conn, label, path, entries }) => {
                let back = match self.ftp.take() {
                    Some(old) => {
                        let c = old.conn.clone();
                        std::thread::spawn(move || gc_ftp::disconnect(&c));
                        old.back
                    }
                    None => self.cwd.clone(),
                };
                self.archive = None;
                self.stack.clear();
                self.ftp = Some(FtpView { conn, label, back });
                self.show_remote(path, entries, w.select);
            }
            Ok(FtpReply::Listed { path, entries }) => self.show_remote(path, entries, w.select),
            Err(e) => {
                self.error = Some(format!("{}: {e}", w.label));
                if self.ftp.is_none() {
                    // A connect that failed: show what happened.
                    self.ftp_failed = true;
                    self.action = Some(PanelAction::FtpLog);
                }
            }
        }
    }

    fn show_remote(&mut self, path: String, entries: Vec<Entry>, select: Option<String>) {
        let select = self.select_next.take().or(select);
        let changed = gc_ftp::remote_str(&self.cwd) != path;
        self.cwd = PathBuf::from(path);
        self.listing = entries;
        self.error = None;
        if changed {
            self.marked.clear();
            self.filter.clear();
            self.filter_open = false;
            self.cursor = 0;
        }
        sort_entries(&mut self.listing, self.sort, self.sort_desc);
        self.apply_view(select);
        self.marked.retain(|m| self.listing.iter().any(|e| &e.path == m));
        self.scroll_to_cursor = true;
    }

    /// Closes the server connection and goes back to the folder the panel showed before.
    pub fn leave_ftp(&mut self) {
        let Some(v) = self.ftp.take() else { return };
        let c = v.conn.clone();
        std::thread::spawn(move || gc_ftp::disconnect(&c));
        self.go_to(v.back, None);
    }

    /// While a server is shown: connection and the remote paths of the marked items (or the one under the cursor).
    pub fn ftp_selection(&self) -> Option<(SharedConn, Vec<RemoteItem>)> {
        let v = self.ftp.as_ref()?;
        let items = self
            .targets()
            .into_iter()
            .map(|p| RemoteItem { is_dir: self.entries.iter().any(|e| e.path == p && e.is_dir), path: gc_ftp::remote_str(&p) })
            .collect();
        Some((v.conn.clone(), items))
    }

    /// While a server is shown: connection and the remote folder.
    pub fn ftp_folder(&self) -> Option<(SharedConn, String)> {
        self.ftp.as_ref().map(|v| (v.conn.clone(), gc_ftp::remote_str(&self.cwd)))
    }

    /// While a server is shown: the file under the cursor (for F3 / F4).
    pub fn ftp_file(&self) -> Option<(SharedConn, String)> {
        let v = self.ftp.as_ref()?;
        let e = self.entries.get(self.cursor).filter(|e| !e.is_dir && e.name != PARENT)?;
        Some((v.conn.clone(), gc_ftp::remote_str(&e.path)))
    }

    /// Full path and name of the entry under the cursor (not the `..` row).
    pub fn cursor_entry(&self) -> Option<(PathBuf, String)> {
        self.entries.get(self.cursor).filter(|e| e.name != PARENT).map(|e| (e.path.clone(), e.name.clone()))
    }

    pub fn is_ftp(&self) -> bool {
        self.ftp.is_some()
    }

    /// Puts the cursor on `path` if it is listed.
    pub fn select_path(&mut self, path: &Path) {
        if let Some(i) = self.entries.iter().position(|e| e.path == path) {
            self.cursor = i;
            self.scroll_to_cursor = true;
        }
    }

    /// Name for a new archive made from the targets: the single item's name, else the folder's.
    pub fn pack_name(&self) -> String {
        let targets = self.targets();
        let source = match targets.as_slice() {
            [one] => one.file_stem().or_else(|| one.file_name()),
            _ => self.cwd.file_name(),
        };
        source.map_or_else(|| "archive".to_owned(), |n| n.to_string_lossy().into_owned())
    }

    /// Marked entries, or the one under the cursor when nothing is marked.
    pub fn targets(&self) -> Vec<PathBuf> {
        if self.marked.is_empty() {
            self.entries.get(self.cursor).filter(|e| e.name != PARENT).map(|e| vec![e.path.clone()]).unwrap_or_default()
        } else {
            self.entries.iter().filter(|e| self.marked.contains(&e.path)).map(|e| e.path.clone()).collect()
        }
    }

    /// Counts for the status line: (marked, total) as (files, dirs, bytes). The `..` row is not counted.
    fn stats(&self) -> ((usize, usize, u64), (usize, usize, u64)) {
        let (mut marked, mut total) = ((0, 0, 0u64), (0, 0, 0u64));
        for e in self.entries.iter().filter(|e| e.name != PARENT) {
            let counters = if self.marked.contains(&e.path) { vec![&mut total, &mut marked] } else { vec![&mut total] };
            for c in counters {
                if e.is_dir {
                    c.1 += 1;
                } else {
                    c.0 += 1;
                    c.2 += e.size;
                }
            }
        }
        (marked, total)
    }

    /// Column headings; a click sorts by that column (again: reverse).
    fn show_header(&mut self, ui: &mut egui::Ui, panel_id: &str, theme: &Theme) {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), HEADER_H), Sense::hover());
        let right = rect.right() - 6.0;
        let size_right = right - DATE_COL_W;
        let zones = [
            (SortKey::Name, "Name", Align2::LEFT_CENTER, rect.left() + 6.0 + ICON_COL_W, rect.left(), size_right - 90.0),
            (SortKey::Size, "Size", Align2::RIGHT_CENTER, size_right, size_right - 90.0, size_right + 4.0),
            (SortKey::Date, "Date", Align2::RIGHT_CENTER, right, size_right + 4.0, rect.right()),
        ];
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 2.0, ui.visuals().faint_bg_color);
        for (key, label, align, x, from, to) in zones {
            let zone = egui::Rect::from_min_max(egui::pos2(from, rect.top()), egui::pos2(to, rect.bottom()));
            let resp = ui.interact(zone, ui.id().with((panel_id, "sort", key as u8)), Sense::click()).on_hover_text("Click to sort (click again to reverse). Ctrl+F3 name, F4 extension, F5 date, F6 size");
            if resp.hovered() {
                painter.rect_filled(zone, 2.0, ui.visuals().widgets.hovered.weak_bg_fill);
            }
            let active = self.sort == key;
            let caret = if self.sort_desc { egui_phosphor::regular::CARET_DOWN } else { egui_phosphor::regular::CARET_UP };
            let text = match (active, align == Align2::LEFT_CENTER) {
                (false, _) => label.to_owned(),
                (true, true) => format!("{label} {caret}"),
                (true, false) => format!("{caret} {label}"),
            };
            let color = if active { theme.accent.0 } else { ui.visuals().weak_text_color() };
            painter.text(egui::pos2(x, rect.center().y), align, text, FontId::proportional(12.0), color);
            if resp.clicked() {
                self.set_sort(key);
            }
        }
    }

    /// The quick filter box under the list (Ctrl+F); typing narrows the list by name.
    fn show_filter_bar(&mut self, ui: &mut egui::Ui) {
        if !self.filter_open {
            return;
        }
        ui.horizontal(|ui| {
            ui.label(egui_phosphor::regular::FUNNEL);
            let edit = ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("filter by name (Enter keeps it, Esc clears)").desired_width(f32::INFINITY));
            if std::mem::take(&mut self.filter_focus) {
                edit.request_focus();
            }
            if edit.changed() {
                let keep = self.entries.get(self.cursor).map(|e| e.name.clone());
                self.apply_view(keep);
            }
        });
    }

    /// Total Commander style summary under the list: `marked / all in files, dirs`.
    fn show_status(&self, ui: &mut egui::Ui, theme: &Theme) {
        let ((mf, md, ms), (tf, td, ts)) = self.stats();
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), STATUS_H), Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 3.0, ui.visuals().faint_bg_color);
        let any = mf + md > 0;
        let filter = if self.filter.is_empty() { String::new() } else { format!("filter \"{}\"  ", self.filter) };
        let text = format!("{filter}{} / {} in {mf} / {tf} file(s), {md} / {td} dir(s)", fmt_size(ms), fmt_size(ts));
        let color = if any { theme.marked.0 } else { theme.text.0.gamma_multiply(0.75) };
        painter.text(rect.left_center() + egui::vec2(6.0, 0.0), Align2::LEFT_CENTER, text, FontId::monospace(12.0), color);
    }

    /// Draws the panel. Returns true when the user clicked into it.
    pub fn ui(&mut self, ui: &mut egui::Ui, id: &str, active: bool, theme: &Theme) -> bool {
        self.poll();
        let mut clicked = false;
        let accent = theme.accent.0;

        ui.horizontal(|ui| {
            if ui.small_button("..").clicked() {
                self.go_up();
                clicked = true;
            }
            if self.ftp.is_some() && ui.small_button("Disconnect").on_hover_text("Close the connection and show the local folder again").clicked() {
                self.leave_ftp();
                clicked = true;
            }
            if (self.ftp.is_some() || self.ftp_work.is_some() || self.ftp_failed) && ui.small_button("Log").on_hover_text("FTP connection log").clicked() {
                self.action = Some(PanelAction::FtpLog);
                clicked = true;
            }
            if let Some(l) = &self.loading {
                ui.spinner();
                ui.label(format!("Opening {}...", l.label));
            } else if let Some(w) = &self.ftp_work {
                ui.spinner();
                ui.label(format!("{} {}...", if self.ftp.is_some() { "Reading" } else { "Connecting to" }, w.label));
            } else {
                ui.add(egui::Label::new(egui::RichText::new(self.location()).monospace()).truncate());
            }
        });
        if let Some(err) = &self.error {
            ui.colored_label(theme.error.0, err);
        }

        ui.spacing_mut().item_spacing.y = 0.0;
        self.show_header(ui, id, theme);
        let mut area = egui::ScrollArea::vertical().id_salt(id).auto_shrink(false).max_height((ui.available_height() - STATUS_H - if self.filter_open { FILTER_H } else { 0.0 }).max(ROW_H));
        if self.scroll_to_cursor {
            let top = self.cursor as f32 * ROW_H;
            let mut off = self.last_offset;
            if top < off {
                off = top;
            } else if top + ROW_H > off + self.last_height {
                off = top + ROW_H - self.last_height;
            }
            area = area.vertical_scroll_offset(off.max(0.0));
            self.scroll_to_cursor = false;
        }

        let out = area.show_rows(ui, ROW_H, self.entries.len(), |ui, range| {
            for i in range {
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), Sense::click());
                if resp.clicked() || resp.double_clicked() {
                    self.cursor = i;
                    clicked = true;
                }
                if resp.double_clicked() {
                    self.enter();
                    return;
                }
                if self.entries[i].name != PARENT {
                    if resp.secondary_clicked() {
                        self.cursor = i;
                        clicked = true;
                        // Like Explorer: a right click on an unmarked row acts on that row alone.
                        let path = self.entries[i].path.clone();
                        if self.archive.is_none() && self.ftp.is_none() {
                            let paths = if self.marked.contains(&path) { self.targets() } else { vec![path] };
                            self.action = Some(PanelAction::ShellMenu(paths));
                        } else if !self.marked.contains(&path) {
                            self.marked.clear();
                        }
                    }
                    if self.archive.is_some() || self.ftp.is_some() {
                        let mut open = false;
                        resp.context_menu(|ui| {
                            if ui.button("Open (Enter)").clicked() {
                                open = true;
                                ui.close_menu();
                            }
                            if self.ftp.is_some() && ui.button("Rename (F2)").clicked() {
                                self.action = Some(PanelAction::Rename);
                                ui.close_menu();
                            }
                            if ui.button(if self.ftp.is_some() { "Copy to other panel (F5)" } else { "Extract to other panel (F5)" }).clicked() {
                                self.action = Some(PanelAction::Extract);
                                ui.close_menu();
                            }
                        });
                        if open {
                            self.enter();
                            return;
                        }
                    }
                }
                let e = &self.entries[i];
                let painter = ui.painter_at(rect);
                if i == self.cursor {
                    if active {
                        painter.rect_filled(rect, 0.0, accent);
                    } else {
                        painter.rect_stroke(rect.shrink(0.5), 0.0, Stroke::new(1.0_f32, accent), StrokeKind::Inside);
                    }
                }
                let marked = self.marked.contains(&e.path);
                let color = if marked {
                    theme.marked.0
                } else if e.is_dir {
                    theme.dir.0
                } else {
                    theme.text.0
                };
                let font = FontId::monospace(13.0);
                let (glyph, glyph_color) = theme.style(theme.category_of(e));
                let left = rect.left_center() + egui::vec2(6.0, 0.0);
                painter.text(left, Align2::LEFT_CENTER, glyph, FontId::proportional(15.0), glyph_color);
                let name = if e.is_dir { format!("[{}]", e.name) } else { e.name.clone() };
                painter.text(left + egui::vec2(ICON_COL_W, 0.0), Align2::LEFT_CENTER, name, font.clone(), color);
                let right = rect.right_center() - egui::vec2(6.0, 0.0);
                if let Some(t) = e.modified {
                    let date = chrono::DateTime::<chrono::Local>::from(t).format("%Y-%m-%d %H:%M").to_string();
                    painter.text(right, Align2::RIGHT_CENTER, date, font.clone(), color);
                }
                let size = if e.is_dir { "<DIR>".to_owned() } else { fmt_size(e.size) };
                painter.text(right - egui::vec2(DATE_COL_W, 0.0), Align2::RIGHT_CENTER, size, font, color);
            }
        });
        self.last_offset = out.state.offset.y;
        self.last_height = out.inner_rect.height();
        self.show_filter_bar(ui);
        self.show_status(ui, theme);
        clicked
    }
}

fn join_inner(inner: &str, name: &str) -> String {
    if inner.is_empty() {
        name.to_owned()
    } else {
        format!("{inner}/{name}")
    }
}

/// What the list is ordered by.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortKey {
    Name,
    Ext,
    Size,
    Date,
}

fn name_order(a: &Entry, b: &Entry) -> std::cmp::Ordering {
    a.name.to_lowercase().cmp(&b.name.to_lowercase())
}

/// Folders always stay on top. Folders have no size or extension, so they go by name then.
fn sort_entries(list: &mut [Entry], key: SortKey, desc: bool) {
    list.sort_by(|a, b| {
        b.is_dir.cmp(&a.is_dir).then_with(|| {
            let ord = match key {
                SortKey::Name => name_order(a, b),
                SortKey::Ext => {
                    let ext = |e: &Entry| if e.is_dir { String::new() } else { gc_core::filetype::extension_of(&e.path).unwrap_or_default() };
                    ext(a).cmp(&ext(b)).then_with(|| name_order(a, b))
                }
                SortKey::Size if a.is_dir => name_order(a, b),
                SortKey::Size => a.size.cmp(&b.size).then_with(|| name_order(a, b)),
                SortKey::Date => a.modified.cmp(&b.modified).then_with(|| name_order(a, b)),
            };
            if desc { ord.reverse() } else { ord }
        })
    });
}

/// Name of the synthetic first row that leads to the parent folder.
/// No real entry can have this name, so it is safe to compare against.
const PARENT: &str = "..";
const ROW_H: f32 = 20.0;
/// Height of the status line under the list.
const STATUS_H: f32 = 22.0;
/// Height of the column headings above the list.
const HEADER_H: f32 = 20.0;
/// Height reserved for the quick filter box.
const FILTER_H: f32 = 28.0;
/// Space before the name for the file-type icon.
const ICON_COL_W: f32 = 22.0;
/// Width reserved for the "YYYY-MM-DD HH:MM" column plus a gap.
const DATE_COL_W: f32 = 140.0;

pub fn fmt_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

#[cfg(test)]
mod tests {
    use super::{fmt_size, Panel, PARENT};

    fn new_panel(root: &std::path::Path) -> Panel {
        Panel::new(root.to_path_buf(), eframe::egui::Context::default())
    }

    #[test]
    fn refresh_notices_new_files() {
        let dir = std::env::temp_dir().join(format!("gc_refresh_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut p = new_panel(&dir);
        std::thread::sleep(std::time::Duration::from_millis(30));
        std::fs::write(dir.join("new.crc"), b"x").unwrap();
        p.refresh_if_changed();
        assert!(p.entries.iter().any(|e| e.name == "new.crc"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Waits for the archive worker thread, like the UI does over several frames.
    fn settle(p: &mut Panel) {
        while p.loading.is_some() {
            p.poll();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn archive_inside_archive_can_be_entered_and_left() {
        use std::io::Write;
        let root = std::env::temp_dir().join(format!("gc_panel_nested_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let mut inner = Vec::new();
        {
            let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut inner));
            w.start_file("deep/x.txt", zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(b"X").unwrap();
            w.finish().unwrap();
        }
        let mut w = zip::ZipWriter::new(std::fs::File::create(root.join("outer.zip")).unwrap());
        w.start_file("pkg/inner.zip", zip::write::SimpleFileOptions::default()).unwrap();
        w.write_all(&inner).unwrap();
        w.finish().unwrap();

        let mut p = new_panel(&root);
        p.move_cursor(1);
        p.enter(); // outer.zip
        settle(&mut p);
        p.move_cursor(1);
        p.enter(); // pkg
        p.move_cursor(1);
        p.enter(); // inner.zip, opened from inside outer.zip
        settle(&mut p);
        let names: Vec<_> = p.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["..", "deep"]);
        assert_eq!(p.location(), format!("{}\\pkg\\inner.zip", root.join("outer.zip").display()));

        p.go_up(); // out of inner.zip, back into outer.zip\pkg
        assert_eq!(p.location(), format!("{}\\pkg", root.join("outer.zip").display()));
        assert_eq!(p.entries[p.cursor].name, "inner.zip");
        p.go_up();
        p.go_up(); // leave outer.zip
        assert_eq!(p.disk_folder(), Some(root.as_path()));
        gc_archive::cleanup_nested();
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn archive_opens_like_a_folder_and_leaves_back_to_its_file() {
        use std::io::Write;
        let root = std::env::temp_dir().join(format!("gc_panel_zip_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let mut w = zip::ZipWriter::new(std::fs::File::create(root.join("t.zip")).unwrap());
        for name in ["a.txt", "docs/b.txt"] {
            w.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(b"x").unwrap();
        }
        w.finish().unwrap();

        let mut p = new_panel(&root);
        assert_eq!(p.entries[p.cursor + 1].name, "t.zip");
        p.move_cursor(1);
        p.enter();
        settle(&mut p);
        assert!(p.disk_folder().is_none());
        let names: Vec<_> = p.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["..", "docs", "a.txt"]);

        p.move_cursor(1); // docs
        p.enter();
        assert_eq!(p.location(), format!("{}\\docs", root.join("t.zip").display()));
        p.move_cursor(1); // b.txt
        let (_, inner) = p.archive_selection().unwrap();
        assert_eq!(inner, ["docs/b.txt"]);

        p.go_up();
        assert_eq!(p.entries[p.cursor].name, "docs");
        p.go_up(); // archive root -> leave the archive
        assert_eq!(p.disk_folder(), Some(root.as_path()));
        assert_eq!(p.entries[p.cursor].name, "t.zip");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn parent_row_leads_up_and_is_never_an_operation_target() {
        let root = std::env::temp_dir().join(format!("gc_panel_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("child")).unwrap();
        std::fs::write(root.join("file.txt"), b"x").unwrap();

        let mut p = new_panel(&root);
        assert_eq!(p.entries[0].name, PARENT);
        assert!(p.targets().is_empty(), "cursor on '..' must not select anything");

        p.toggle_mark_and_advance();
        assert!(p.marked.is_empty(), "'..' cannot be marked");

        assert_eq!(p.entries[p.cursor].name, "child"); // marking '..' still advanced the cursor
        p.enter();
        assert_eq!(p.cwd, root.join("child"));
        assert_eq!(p.entries[0].name, PARENT);

        p.enter(); // cursor starts on '..'
        assert_eq!(p.cwd, root);
        assert_eq!(p.entries[p.cursor].name, "child", "cursor returns to the folder we left");
        std::fs::remove_dir_all(&root).unwrap();
    }

    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("gc_panel_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("zdir")).unwrap();
        std::fs::write(root.join("b.txt"), vec![0u8; 10]).unwrap();
        std::fs::write(root.join("a.rs"), vec![0u8; 300]).unwrap();
        std::fs::write(root.join("C.md"), vec![0u8; 20]).unwrap();
        root
    }

    fn shown(p: &Panel) -> Vec<&str> {
        p.entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn sorting_keeps_folders_on_top_and_reverses_on_second_click() {
        use super::SortKey;
        let root = scratch_dir("sort");
        let mut p = new_panel(&root);
        assert_eq!(shown(&p), ["..", "zdir", "a.rs", "b.txt", "C.md"]);

        p.set_sort(SortKey::Size); // biggest first
        assert_eq!(shown(&p), ["..", "zdir", "a.rs", "C.md", "b.txt"]);
        p.set_sort(SortKey::Size);
        assert_eq!(shown(&p), ["..", "zdir", "b.txt", "C.md", "a.rs"]);
        p.set_sort(SortKey::Ext);
        assert_eq!(shown(&p), ["..", "zdir", "C.md", "a.rs", "b.txt"]);
        p.set_sort(SortKey::Name);
        assert_eq!(shown(&p), ["..", "zdir", "a.rs", "b.txt", "C.md"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn quick_filter_narrows_the_list_and_survives_a_reload_but_not_navigation() {
        let root = scratch_dir("filter");
        let mut p = new_panel(&root);
        p.open_filter();
        p.filter = "B.T".into();
        p.apply_view(None);
        assert_eq!(shown(&p), ["..", "b.txt"], "case-insensitive substring, '..' stays");

        p.reload();
        assert_eq!(shown(&p), ["..", "b.txt"]);
        p.close_filter();
        assert_eq!(shown(&p).len(), 5);

        p.filter = "zd".into();
        p.apply_view(None);
        p.move_cursor(1);
        p.enter(); // into zdir
        assert!(p.filter.is_empty(), "a new folder starts unfiltered");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn sizes_are_human_readable() {
        assert_eq!(fmt_size(0), "0 B");
        assert_eq!(fmt_size(1023), "1023 B");
        assert_eq!(fmt_size(1536), "1.5 KB");
        assert_eq!(fmt_size(5 * 1024 * 1024 * 1024), "5.0 GB");
    }
}

/// The closest folder that still exists: the nearest existing parent of `path`, or, when its whole
/// drive is gone, the root of another drive (C: first).
fn nearest_existing(path: &Path) -> Option<PathBuf> {
    if let Some(p) = path.ancestors().skip(1).find(|a| a.exists()) {
        return Some(p.to_path_buf());
    }
    let drives = std::iter::once('C').chain('A'..='Z').map(|c| PathBuf::from(format!("{c}:\\")));
    drives.into_iter().find(|d| d.exists())
}

#[cfg(test)]
mod missing_tests {
    use super::*;

    #[test]
    fn nearest_existing_parent_or_a_drive() {
        let tmp = std::env::temp_dir();
        assert_eq!(nearest_existing(&tmp.join("gc_gone_a").join("b")), Some(tmp.clone()));
        let drive = nearest_existing(Path::new(r"Q:\no\such\dir"));
        assert!(drive.is_some_and(|d| d.exists()));
    }
}
