//! Alt+F7: search for files by name (wildcards) and by the text they contain, in a folder tree and
//! optionally inside archives. Runs on a worker thread; hits appear while it works and can be
//! shown in the panel (Enter), viewed (F3) or edited (F4).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

use eframe::egui::{self, Align2, FontId, Key, Sense, Vec2};

use crate::panel::fmt_size;
use crate::textenc;

/// A found file; `inner` is set for a file inside the archive `path`.
#[derive(Clone)]
pub struct Hit {
    pub path: PathBuf,
    pub inner: Option<String>,
    pub size: u64,
    /// First matching line (number, text) when the search looked at the content.
    pub line: Option<(usize, String)>,
}

impl Hit {
    /// The path as shown in the list, relative to the folder that was searched.
    fn shown(&self, root: &Path) -> String {
        let base = self.path.strip_prefix(root).unwrap_or(&self.path).display().to_string();
        match &self.inner {
            Some(i) => format!("{base}\\{}", i.replace('/', "\\")),
            None => base,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Action {
    Goto,
    View,
    Edit,
}

/// File name patterns: `*` and `?` wildcards, several separated by `;` or `,`; a pattern without
/// wildcards matches names that contain it. Case-insensitive. Empty matches everything.
pub struct Mask(Vec<String>);

fn wild(p: &[char], t: &[char]) -> bool {
    let (mut pi, mut ti, mut star, mut mark) = (0, 0, None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

impl Mask {
    pub fn new(text: &str) -> Self {
        Mask(text.split([';', ',']).map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect())
    }

    pub fn matches(&self, name: &str) -> bool {
        if self.0.is_empty() {
            return true;
        }
        let n = name.to_lowercase();
        let nc: Vec<char> = n.chars().collect();
        self.0.iter().any(|p| if p.contains('*') || p.contains('?') { wild(&p.chars().collect::<Vec<_>>(), &nc) } else { n.contains(p.as_str()) })
    }
}

/// First line of the file that contains `text` (files over `max` bytes and binary files are skipped).
fn scan_file(path: &Path, text: &str, case: bool, max: u64) -> Option<(usize, String)> {
    if std::fs::metadata(path).ok()?.len() > max {
        return None;
    }
    let (s, _) = textenc::decode(&std::fs::read(path).ok()?)?;
    let needle = if case { text.to_string() } else { text.to_lowercase() };
    if !case && !s.to_lowercase().contains(&needle) {
        return None;
    }
    for (i, line) in s.lines().enumerate() {
        let found = if case { line.contains(&needle) } else { line.to_lowercase().contains(&needle) };
        if found {
            return Some((i + 1, line.trim().chars().take(160).collect()));
        }
    }
    None
}

struct Opts {
    root: PathBuf,
    mask: Mask,
    text: String,
    case: bool,
    subfolders: bool,
    archives: bool,
}

enum Msg {
    Hit(Hit),
    Progress(usize, String),
    Done,
}

const MAX_HITS: usize = 20_000;
const MAX_FILE: u64 = 64 * 1024 * 1024;
const MAX_ARCHIVE_ENTRY: u64 = 16 * 1024 * 1024;

#[cfg(windows)]
fn is_reparse(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_reparse(meta: &std::fs::Metadata) -> bool {
    meta.file_type().is_symlink()
}

fn scan_archive(path: &Path, o: &Opts, tx: &std::sync::mpsc::Sender<Msg>, cancel: &AtomicBool, hits: &mut usize) {
    let Ok(index) = gc_archive::open(path) else { return };
    for (inner, is_dir, size) in index.entry_list() {
        if cancel.load(Ordering::Relaxed) || *hits >= MAX_HITS {
            return;
        }
        if is_dir || !o.mask.matches(inner.rsplit('/').next().unwrap_or(&inner)) {
            continue;
        }
        let line = if o.text.is_empty() {
            None
        } else {
            if size > MAX_ARCHIVE_ENTRY {
                continue;
            }
            let Ok(p) = gc_archive::extract_to_scratch(&index, &inner) else { continue };
            let found = scan_file(&p, &o.text, o.case, MAX_ARCHIVE_ENTRY);
            if let Some(dir) = p.parent() {
                let _ = std::fs::remove_dir_all(dir);
            }
            match found {
                Some(l) => Some(l),
                None => continue,
            }
        };
        *hits += 1;
        let _ = tx.send(Msg::Hit(Hit { path: path.to_path_buf(), inner: Some(inner), size, line }));
    }
}

fn run(o: Opts, tx: std::sync::mpsc::Sender<Msg>, cancel: Arc<AtomicBool>, ctx: egui::Context) {
    let (mut scanned, mut hits) = (0usize, 0usize);
    let mut stack = vec![(o.root.clone(), 0usize)];
    let mut last_ping = std::time::Instant::now();
    while let Some((dir, depth)) = stack.pop() {
        if cancel.load(Ordering::Relaxed) || hits >= MAX_HITS {
            break;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let Ok(meta) = e.metadata() else { continue };
            let path = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if meta.is_dir() {
                if o.subfolders && depth < 64 && !is_reparse(&meta) {
                    stack.push((path.clone(), depth + 1));
                }
                if o.text.is_empty() && o.mask.matches(&name) {
                    hits += 1;
                    let _ = tx.send(Msg::Hit(Hit { path, inner: None, size: 0, line: None }));
                }
                continue;
            }
            scanned += 1;
            if last_ping.elapsed().as_millis() > 150 {
                last_ping = std::time::Instant::now();
                let _ = tx.send(Msg::Progress(scanned, dir.display().to_string()));
                ctx.request_repaint();
            }
            if o.mask.matches(&name) {
                let line = if o.text.is_empty() { None } else { scan_file(&path, &o.text, o.case, MAX_FILE) };
                if o.text.is_empty() || line.is_some() {
                    hits += 1;
                    let _ = tx.send(Msg::Hit(Hit { path: path.clone(), inner: None, size: meta.len(), line }));
                }
            }
            if o.archives && gc_archive::is_archive(&path) {
                scan_archive(&path, &o, &tx, &cancel, &mut hits);
            }
        }
    }
    let _ = tx.send(Msg::Progress(scanned, String::new()));
    let _ = tx.send(Msg::Done);
    ctx.request_repaint();
}

pub struct SearchWindow {
    pub open: bool,
    dir: String,
    mask: String,
    text: String,
    case: bool,
    subfolders: bool,
    archives: bool,
    rx: Option<Receiver<Msg>>,
    cancel: Arc<AtomicBool>,
    hits: Vec<Hit>,
    selected: usize,
    scanned: usize,
    current: String,
    done: bool,
    focus_mask: bool,
    action: Option<(Action, Hit)>,
    reveal: bool,
    /// The folder of the running / last search.
    root: PathBuf,
}

impl Default for SearchWindow {
    fn default() -> Self {
        Self {
            open: false,
            dir: String::new(),
            mask: String::new(),
            text: String::new(),
            case: false,
            subfolders: true,
            archives: false,
            rx: None,
            cancel: Arc::new(AtomicBool::new(false)),
            hits: Vec::new(),
            selected: 0,
            scanned: 0,
            current: String::new(),
            done: false,
            focus_mask: false,
            action: None,
            reveal: false,
            root: PathBuf::new(),
        }
    }
}

impl SearchWindow {
    /// Opens the window; `dir` is the folder of the active panel (kept when a search is running).
    pub fn open_in(&mut self, dir: &Path) {
        if self.rx.is_none() {
            self.dir = dir.display().to_string();
        }
        self.open = true;
        self.focus_mask = true;
    }

    /// A hit the user asked to go to / view / edit.
    pub fn take_action(&mut self) -> Option<(Action, Hit)> {
        self.action.take()
    }

    fn start(&mut self, ctx: &egui::Context) {
        self.stop();
        self.hits.clear();
        self.selected = 0;
        self.scanned = 0;
        self.done = false;
        self.current.clear();
        let (tx, rx) = channel();
        self.cancel = Arc::new(AtomicBool::new(false));
        self.root = PathBuf::from(self.dir.trim());
        let opts = Opts { root: self.root.clone(), mask: Mask::new(&self.mask), text: self.text.clone(), case: self.case, subfolders: self.subfolders, archives: self.archives };
        let (cancel, c) = (self.cancel.clone(), ctx.clone());
        std::thread::spawn(move || run(opts, tx, cancel, c));
        self.rx = Some(rx);
    }

    fn stop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.rx = None;
    }

    fn poll(&mut self) {
        let mut finished = false;
        if let Some(rx) = &self.rx {
            while let Ok(m) = rx.try_recv() {
                match m {
                    Msg::Hit(h) => self.hits.push(h),
                    Msg::Progress(n, d) => (self.scanned, self.current) = (n, d),
                    Msg::Done => finished = true,
                }
            }
        }
        if finished {
            self.rx = None;
            self.done = true;
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }
        self.poll();
        let typing = ctx.wants_keyboard_input();
        let mut open = true;
        let mut start = false;
        egui::Window::new("Search").id(egui::Id::new("gc_search")).open(&mut open).default_size([820.0, 560.0]).collapsible(false).show(ctx, |ui| {
            egui::Grid::new("search_fields").num_columns(2).show(ui, |ui| {
                ui.label("Search in");
                ui.add(egui::TextEdit::singleline(&mut self.dir).desired_width(f32::INFINITY));
                ui.end_row();
                ui.label("File name");
                let m = ui.add(egui::TextEdit::singleline(&mut self.mask).hint_text("*.txt; *.log   (empty = all files)").desired_width(f32::INFINITY));
                if std::mem::take(&mut self.focus_mask) {
                    m.request_focus();
                }
                start |= m.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                ui.end_row();
                ui.label("Containing text");
                let t = ui.add(egui::TextEdit::singleline(&mut self.text).hint_text("optional").desired_width(f32::INFINITY));
                start |= t.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                ui.end_row();
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.case, "Match case");
                ui.checkbox(&mut self.subfolders, "Subfolders");
                ui.checkbox(&mut self.archives, "Inside archives");
                if self.rx.is_some() {
                    if ui.button("Stop").clicked() {
                        self.stop();
                        self.done = true;
                    }
                } else if ui.button("Search  (Enter)").clicked() {
                    start = true;
                }
            });
            ui.horizontal(|ui| {
                let state = if self.rx.is_some() { format!("Searching ... {} files looked at   {}", self.scanned, self.current) } else if self.done { format!("Done: {} found, {} files looked at", self.hits.len(), self.scanned) } else { String::new() };
                ui.weak(state);
            });
            ui.separator();

            let row_h = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
            let step = row_h + ui.spacing().item_spacing.y;
            let mut area = egui::ScrollArea::vertical().auto_shrink(false).max_height((ui.available_height() - 40.0).max(80.0));
            if std::mem::take(&mut self.reveal) {
                area = area.vertical_scroll_offset((self.selected as f32 * step - 3.0 * step).max(0.0));
            }
            let mut goto = None;
            area.show_rows(ui, row_h, self.hits.len(), |ui, range| {
                for i in range {
                    let h = &self.hits[i];
                    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), row_h), Sense::click());
                    if i == self.selected {
                        ui.painter().rect_filled(rect, 0.0, ui.visuals().selection.bg_fill);
                    } else if resp.hovered() {
                        ui.painter().rect_filled(rect, 0.0, ui.visuals().widgets.hovered.bg_fill.linear_multiply(0.4));
                    }
                    if resp.clicked() {
                        self.selected = i;
                    }
                    if resp.double_clicked() {
                        goto = Some(i);
                    }
                    let p = ui.painter().with_clip_rect(egui::Rect::from_min_max(rect.min, egui::pos2(rect.right() - 90.0, rect.bottom())));
                    let font = FontId::proportional(13.0);
                    let name = h.shown(&self.root);
                    let line = h.line.as_ref().map(|(n, t)| format!("   [{n}]  {t}")).unwrap_or_default();
                    let size = if h.inner.is_none() && h.size == 0 && h.line.is_none() { String::new() } else { fmt_size(h.size) };
                    p.text(egui::pos2(rect.left() + 4.0, rect.center().y), Align2::LEFT_CENTER, format!("{name}{line}"), font.clone(), ui.visuals().text_color());
                    ui.painter().text(egui::pos2(rect.right() - 4.0, rect.center().y), Align2::RIGHT_CENTER, size, font, ui.visuals().weak_text_color());
                }
            });
            if let Some(i) = goto {
                self.action = Some((Action::Goto, self.hits[i].clone()));
                self.open = false;
            }
            ui.horizontal(|ui| {
                let have = self.selected < self.hits.len();
                for (label, act) in [("Go to file  (Enter)", Action::Goto), ("View  (F3)", Action::View), ("Edit  (F4)", Action::Edit)] {
                    if ui.add_enabled(have, egui::Button::new(label)).clicked() {
                        self.action = Some((act, self.hits[self.selected].clone()));
                        if act == Action::Goto {
                            self.open = false;
                        }
                    }
                }
            });
        });
        if start && self.rx.is_none() {
            self.start(ctx);
        }
        // keys of the result list (not while a text field has the focus)
        if !typing && !self.hits.is_empty() {
            let (up, down, pgup, pgdn, home, end, enter, f3, f4) = ctx.input(|i| {
                let k = |key| i.key_pressed(key);
                (k(Key::ArrowUp), k(Key::ArrowDown), k(Key::PageUp), k(Key::PageDown), k(Key::Home), k(Key::End), k(Key::Enter), k(Key::F3), k(Key::F4))
            });
            let last = self.hits.len() - 1;
            let before = self.selected;
            if up {
                self.selected = self.selected.saturating_sub(1);
            }
            if down {
                self.selected = (self.selected + 1).min(last);
            }
            if pgup {
                self.selected = self.selected.saturating_sub(15);
            }
            if pgdn {
                self.selected = (self.selected + 15).min(last);
            }
            if home {
                self.selected = 0;
            }
            if end {
                self.selected = last;
            }
            self.reveal |= self.selected != before;
            let hit = self.hits[self.selected.min(last)].clone();
            if enter {
                self.action = Some((Action::Goto, hit));
                self.open = false;
            } else if f3 {
                self.action = Some((Action::View, hit));
            } else if f4 {
                self.action = Some((Action::Edit, hit));
            }
        }
        if !open || (!typing && ctx.input(|i| i.key_pressed(Key::Escape))) {
            self.open = false;
        }
        if !self.open {
            self.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks() {
        let m = Mask::new("*.TXT; report?.doc");
        assert!(m.matches("notes.txt") && m.matches("a.b.TXT") && m.matches("report1.doc"));
        assert!(!m.matches("notes.txt.bak") && !m.matches("report12.doc"));
        assert!(Mask::new("").matches("anything"));
        assert!(Mask::new("log").matches("system.LOG.old"), "no wildcard: contains");
        assert!(Mask::new("a*b*c").matches("aXXbYYc") && !Mask::new("a*b*c").matches("aXXbYY"));
    }

    #[test]
    fn searches_inside_archives() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("gc_search_zip_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut z = zip::ZipWriter::new(std::fs::File::create(dir.join("pack.zip")).unwrap());
        z.start_file("docs/readme.txt", zip::write::SimpleFileOptions::default()).unwrap();
        z.write_all(b"the NEEDLE is here
").unwrap();
        z.start_file("docs/other.txt", zip::write::SimpleFileOptions::default()).unwrap();
        z.write_all(b"nothing
").unwrap();
        z.finish().unwrap();
        let (tx, rx) = channel();
        let o = Opts { root: dir.clone(), mask: Mask::new("*.txt"), text: "needle".into(), case: false, subfolders: true, archives: true };
        run(o, tx, Arc::new(AtomicBool::new(false)), egui::Context::default());
        let hits: Vec<Hit> = rx.try_iter().filter_map(|m| if let Msg::Hit(h) = m { Some(h) } else { None }).collect();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].inner.as_deref(), Some("docs/readme.txt"));
        assert_eq!(hits[0].line.as_ref().map(|l| l.0), Some(1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finds_names_and_content() {
        let dir = std::env::temp_dir().join(format!("gc_search_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), "first line\nHello World here\n").unwrap();
        std::fs::write(dir.join("sub").join("b.log"), "nothing\n").unwrap();
        std::fs::write(dir.join("sub").join("c.txt"), "hello again\n").unwrap();
        std::fs::write(dir.join("bin.dat"), [0u8, 1, 2, b'h', b'e', b'l', b'l', b'o']).unwrap();
        assert_eq!(scan_file(&dir.join("a.txt"), "hello world", false, 1000), Some((2, "Hello World here".to_string())));
        assert_eq!(scan_file(&dir.join("a.txt"), "hello world", true, 1000), None);
        assert_eq!(scan_file(&dir.join("bin.dat"), "hello", false, 1000), None, "binary files are skipped");
        let run_search = |mask: &str, text: &str| {
            let (tx, rx) = channel();
            let o = Opts { root: dir.clone(), mask: Mask::new(mask), text: text.into(), case: false, subfolders: true, archives: false };
            run(o, tx, Arc::new(AtomicBool::new(false)), egui::Context::default());
            let mut names: Vec<String> = rx.try_iter().filter_map(|m| if let Msg::Hit(h) = m { Some(h.path.file_name().unwrap().to_string_lossy().into_owned()) } else { None }).collect();
            names.sort();
            names
        };
        assert_eq!(run_search("*.txt", ""), vec!["a.txt", "c.txt"]);
        assert_eq!(run_search("*.txt", "hello"), vec!["a.txt", "c.txt"]);
        assert_eq!(run_search("", "nothing"), vec!["b.log"]);
        assert_eq!(run_search("sub", ""), vec!["sub"], "folders match by name when no text is asked");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
