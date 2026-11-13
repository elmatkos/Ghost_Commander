//! Ctrl+K: compares the folders of the two panels (optionally with all sub-folders) and lists what is
//! only on the left, only on the right, different, or the same. Enter on a different file pair opens
//! the side by side diff; "Mark in panels" marks the differing top-level items in both panels.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::SystemTime;

use eframe::egui::{self, Align2, FontId, Key, Sense};

use crate::panel::fmt_size;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    OnlyLeft,
    OnlyRight,
    Different,
    Same,
}

#[derive(Clone, Debug)]
pub struct Side {
    path: PathBuf,
    size: u64,
    modified: Option<SystemTime>,
}

#[derive(Clone, Debug)]
pub struct Row {
    /// Path relative to the compared folders, shown in the list.
    rel: String,
    is_dir: bool,
    left: Option<Side>,
    right: Option<Side>,
    status: Status,
}

enum Msg {
    Row(Row),
    Progress(usize),
    Done,
}

pub enum Action {
    /// Open the diff of two files.
    Diff(PathBuf, PathBuf),
    /// Mark these (lower-case) top-level names in the left / right panel.
    Mark(HashSet<String>, HashSet<String>),
}

#[derive(Clone, Copy)]
struct Opts {
    subfolders: bool,
    content: bool,
}

/// What one folder holds, by lower-case name.
fn read_side(dir: Option<&Path>) -> BTreeMap<String, (String, bool, Side)> {
    let mut m = BTreeMap::new();
    let Some(dir) = dir else { return m };
    let Ok(rd) = fs::read_dir(dir) else { return m };
    for e in rd.filter_map(|e| e.ok()) {
        let Ok(meta) = fs::metadata(e.path()).or_else(|_| e.metadata()) else { continue };
        let name = e.file_name().to_string_lossy().into_owned();
        m.insert(name.to_lowercase(), (name, meta.is_dir(), Side { path: e.path(), size: if meta.is_dir() { 0 } else { meta.len() }, modified: meta.modified().ok() }));
    }
    m
}

/// Same bytes? (Reads both files in blocks, stops at the first difference.)
fn same_bytes(a: &Path, b: &Path, stop: &AtomicBool) -> bool {
    let (Ok(mut fa), Ok(mut fb)) = (fs::File::open(a), fs::File::open(b)) else { return false };
    let (mut ba, mut bb) = (vec![0u8; 256 * 1024], vec![0u8; 256 * 1024]);
    loop {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        let Ok(na) = read_full(&mut fa, &mut ba) else { return false };
        let Ok(nb) = read_full(&mut fb, &mut bb) else { return false };
        if na != nb || ba[..na] != bb[..nb] {
            return false;
        }
        if na == 0 {
            return true;
        }
    }
}

fn read_full(f: &mut fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        let k = f.read(&mut buf[n..])?;
        if k == 0 {
            break;
        }
        n += k;
    }
    Ok(n)
}

/// Do two files with the same name differ? Sizes first, then (optionally) the content, otherwise the
/// modification time (2 s apart or more: FAT stores even seconds only).
fn files_differ(l: &Side, r: &Side, content: bool, stop: &AtomicBool) -> bool {
    if l.size != r.size {
        return true;
    }
    if content {
        return !same_bytes(&l.path, &r.path, stop);
    }
    match (l.modified, r.modified) {
        (Some(a), Some(b)) => a.duration_since(b).or_else(|e| Ok::<_, ()>(e.duration())).map(|d| d.as_secs() >= 2).unwrap_or(false),
        _ => false,
    }
}

fn walk(l: Option<&Path>, r: Option<&Path>, rel: &str, o: Opts, tx: &std::sync::mpsc::Sender<Msg>, stop: &AtomicBool, seen: &mut usize) {
    let (a, b) = (read_side(l), read_side(r));
    let mut names: Vec<&String> = a.keys().chain(b.keys()).collect();
    names.sort();
    names.dedup();
    for key in names {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let (la, rb) = (a.get(key), b.get(key));
        let name = la.map(|x| x.0.clone()).or_else(|| rb.map(|x| x.0.clone())).unwrap_or_default();
        let shown = if rel.is_empty() { name.clone() } else { format!("{rel}\\{name}") };
        *seen += 1;
        if *seen % 50 == 0 {
            let _ = tx.send(Msg::Progress(*seen));
        }
        let send = |is_dir: bool, left: Option<Side>, right: Option<Side>, status: Status| {
            let _ = tx.send(Msg::Row(Row { rel: shown.clone(), is_dir, left, right, status }));
        };
        match (la, rb) {
            (Some((_, ld, ls)), Some((_, rd, rs))) => {
                if *ld && *rd {
                    if o.subfolders {
                        walk(Some(&ls.path), Some(&rs.path), &shown, o, tx, stop, seen);
                    }
                } else if ld != rd {
                    send(*ld, Some(ls.clone()), Some(rs.clone()), Status::Different);
                } else {
                    let st = if files_differ(ls, rs, o.content, stop) { Status::Different } else { Status::Same };
                    send(false, Some(ls.clone()), Some(rs.clone()), st);
                }
            }
            (Some((_, d, s)), None) => send(*d, Some(s.clone()), None, Status::OnlyLeft),
            (None, Some((_, d, s))) => send(*d, None, Some(s.clone()), Status::OnlyRight),
            (None, None) => {}
        }
    }
}

pub struct DirCompare {
    pub open: bool,
    left: String,
    right: String,
    subfolders: bool,
    content: bool,
    show: [bool; 4],
    rows: Vec<Row>,
    rx: Option<Receiver<Msg>>,
    stop: Arc<AtomicBool>,
    seen: usize,
    done: bool,
    sel: usize,
    reveal: bool,
    start_now: bool,
    action: Option<Action>,
}

impl Default for DirCompare {
    fn default() -> Self {
        Self {
            open: false,
            left: String::new(),
            right: String::new(),
            subfolders: true,
            content: false,
            show: [true, true, true, false],
            rows: Vec::new(),
            rx: None,
            stop: Arc::new(AtomicBool::new(false)),
            seen: 0,
            done: false,
            sel: 0,
            reveal: false,
            start_now: false,
            action: None,
        }
    }
}

impl DirCompare {
    /// Opens the window for these two folders and starts comparing.
    pub fn open_for(&mut self, left: &Path, right: &Path) {
        self.stop_scan();
        self.left = left.display().to_string();
        self.right = right.display().to_string();
        self.open = true;
        self.start_now = true;
    }

    pub fn take_action(&mut self) -> Option<Action> {
        self.action.take()
    }

    fn stop_scan(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.rx = None;
    }

    fn start(&mut self, ctx: &egui::Context) {
        self.stop_scan();
        self.rows.clear();
        self.seen = 0;
        self.done = false;
        self.sel = 0;
        let stop = Arc::new(AtomicBool::new(false));
        self.stop = stop.clone();
        let (tx, rx) = channel();
        self.rx = Some(rx);
        let (l, r) = (PathBuf::from(self.left.trim()), PathBuf::from(self.right.trim()));
        let o = Opts { subfolders: self.subfolders, content: self.content };
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let mut seen = 0;
            walk(Some(&l), Some(&r), "", o, &tx, &stop, &mut seen);
            let _ = tx.send(Msg::Done);
            ctx.request_repaint();
        });
    }

    fn poll(&mut self) {
        let Some(rx) = &self.rx else { return };
        let mut finished = false;
        loop {
            match rx.try_recv() {
                Ok(Msg::Row(r)) => self.rows.push(r),
                Ok(Msg::Progress(n)) => self.seen = n,
                Ok(Msg::Done) => {
                    finished = true;
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    finished = true;
                    break;
                }
            }
        }
        if finished {
            self.rx = None;
            self.done = true;
        }
    }

    fn visible(&self) -> Vec<usize> {
        let idx = |s: Status| match s {
            Status::OnlyLeft => 0,
            Status::OnlyRight => 1,
            Status::Different => 2,
            Status::Same => 3,
        };
        (0..self.rows.len()).filter(|&i| self.show[idx(self.rows[i].status)]).collect()
    }

    /// Top-level names (first path part) of everything that is not the same, per side.
    fn marks(&self) -> (HashSet<String>, HashSet<String>) {
        let (mut l, mut r) = (HashSet::new(), HashSet::new());
        for row in self.rows.iter().filter(|r| r.status != Status::Same) {
            let top = row.rel.split('\\').next().unwrap_or("").to_lowercase();
            if row.left.is_some() {
                l.insert(top.clone());
            }
            if row.right.is_some() {
                r.insert(top);
            }
        }
        (l, r)
    }

    fn activate(&mut self, row: &Row) {
        if let (Some(l), Some(r), false) = (&row.left, &row.right, row.is_dir) {
            self.action = Some(Action::Diff(l.path.clone(), r.path.clone()));
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }
        if std::mem::take(&mut self.start_now) {
            self.start(ctx);
        }
        self.poll();
        let typing = ctx.wants_keyboard_input();
        let mut open = true;
        let mut start = false;
        egui::Window::new("Compare folders").id(egui::Id::new("gc_dircmp")).open(&mut open).default_size([900.0, 560.0]).collapsible(false).show(ctx, |ui| {
            egui::Grid::new("dircmp_fields").num_columns(2).show(ui, |ui| {
                ui.label("Left");
                start |= ui.add(egui::TextEdit::singleline(&mut self.left).desired_width(f32::INFINITY)).lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                ui.end_row();
                ui.label("Right");
                start |= ui.add(egui::TextEdit::singleline(&mut self.right).desired_width(f32::INFINITY)).lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                ui.end_row();
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.subfolders, "Subfolders");
                ui.checkbox(&mut self.content, "Compare content").on_hover_text("Off: files with the same name differ when their size or time (2 s or more) differ.\nOn: same size files are also read and compared byte by byte (slower).");
                if self.rx.is_some() {
                    if ui.button("Stop").clicked() {
                        self.stop_scan();
                        self.done = true;
                    }
                } else if ui.button("Compare  (Enter)").clicked() {
                    start = true;
                }
            });
            ui.horizontal(|ui| {
                let labels = ["Only left", "Only right", "Different", "Same"];
                let counts = [Status::OnlyLeft, Status::OnlyRight, Status::Different, Status::Same].map(|s| self.rows.iter().filter(|r| r.status == s).count());
                for i in 0..4 {
                    ui.checkbox(&mut self.show[i], format!("{} ({})", labels[i], counts[i]));
                }
            });
            let state = if self.rx.is_some() { format!("Comparing ... {} items looked at", self.seen) } else if self.done { format!("Done: {} rows", self.rows.len()) } else { String::new() };
            ui.weak(state);
            ui.separator();

            let list = self.visible();
            let (mut up, mut down, mut enter) = (false, false, false);
            if !typing {
                ui.input(|i| {
                    up = i.key_pressed(Key::ArrowUp);
                    down = i.key_pressed(Key::ArrowDown);
                    enter = i.key_pressed(Key::Enter);
                });
            }
            let last = list.len().saturating_sub(1);
            if up || down {
                self.sel = if up { self.sel.saturating_sub(1) } else { (self.sel + 1).min(last) };
                self.reveal = true;
            }
            self.sel = self.sel.min(last);
            let row_h = 20.0;
            let mut area = egui::ScrollArea::vertical().id_salt("dircmp_rows").auto_shrink(false).max_height(ui.available_height() - 34.0);
            if std::mem::take(&mut self.reveal) {
                let step = row_h + ui.spacing().item_spacing.y;
                area = area.vertical_scroll_offset((self.sel as f32 * step - 80.0).max(0.0));
            }
            let mut clicked = None;
            let mut double = None;
            area.show_rows(ui, row_h, list.len(), |ui, range| {
                for k in range {
                    let row = &self.rows[list[k]];
                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), Sense::click());
                    if resp.clicked() {
                        clicked = Some(k);
                    }
                    if resp.double_clicked() {
                        double = Some(k);
                    }
                    let p = ui.painter_at(rect);
                    if k == self.sel {
                        p.rect_filled(rect, 2.0, ui.visuals().selection.bg_fill);
                    }
                    let (mark, color) = match row.status {
                        Status::OnlyLeft => ("<", egui::Color32::from_rgb(220, 90, 90)),
                        Status::OnlyRight => (">", egui::Color32::from_rgb(90, 190, 110)),
                        Status::Different => ("\u{2260}", egui::Color32::from_rgb(230, 170, 40)),
                        Status::Same => ("=", ui.visuals().weak_text_color()),
                    };
                    let font = FontId::monospace(13.0);
                    let weak = ui.visuals().weak_text_color();
                    let w = rect.width();
                    let c1 = rect.left() + w * 0.52;
                    let c2 = rect.left() + w * 0.52 + (w * 0.48 - 16.0) / 2.0;
                    p.text(egui::pos2(rect.left() + 6.0, rect.center().y), Align2::LEFT_CENTER, mark, font.clone(), color);
                    let name = if row.is_dir { format!("[{}]", row.rel) } else { row.rel.clone() };
                    let clip = p.with_clip_rect(egui::Rect::from_min_max(rect.min, egui::pos2(c1 - 4.0, rect.bottom())));
                    clip.text(egui::pos2(rect.left() + 22.0, rect.center().y), Align2::LEFT_CENTER, name, font.clone(), ui.visuals().text_color());
                    for (x, side) in [(c1, &row.left), (c2 + 8.0, &row.right)] {
                        if let Some(s) = side {
                            let size = if row.is_dir { "<DIR>".to_owned() } else { fmt_size(s.size) };
                            let date = s.modified.map(|t| chrono::DateTime::<chrono::Local>::from(t).format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default();
                            p.text(egui::pos2(x + 70.0, rect.center().y), Align2::RIGHT_CENTER, size, font.clone(), weak);
                            p.text(egui::pos2(x + 78.0, rect.center().y), Align2::LEFT_CENTER, date, font.clone(), weak);
                        }
                    }
                }
            });
            if let Some(k) = clicked {
                self.sel = k;
            }
            if let Some(k) = double {
                self.sel = k;
                enter = true;
            }
            ui.horizontal(|ui| {
                let cur = list.get(self.sel).map(|&i| self.rows[i].clone());
                let diffable = cur.as_ref().is_some_and(|r| r.left.is_some() && r.right.is_some() && !r.is_dir);
                if ui.add_enabled(diffable, egui::Button::new("Compare files  (Enter)")).clicked() {
                    enter = true;
                }
                if ui.add_enabled(!self.rows.is_empty(), egui::Button::new("Mark differences in panels")).on_hover_text("Marks the items of the panels' current folders that differ or are missing on the other side.").clicked() {
                    let (l, r) = self.marks();
                    self.action = Some(Action::Mark(l, r));
                }
                if enter {
                    if let Some(row) = &cur {
                        self.activate(row);
                    }
                }
            });
        });
        if start {
            self.start(ctx);
        }
        if !open || ctx.input(|i| i.key_pressed(Key::Escape)) && !typing {
            self.stop_scan();
            self.open = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gc_dircmp_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn compare(l: &Path, r: &Path, o: Opts) -> Vec<Row> {
        let (tx, rx) = channel();
        let stop = AtomicBool::new(false);
        let mut seen = 0;
        walk(Some(l), Some(r), "", o, &tx, &stop, &mut seen);
        drop(tx);
        rx.iter().filter_map(|m| if let Msg::Row(r) = m { Some(r) } else { None }).collect()
    }

    #[test]
    fn finds_only_left_only_right_different_and_same() {
        let (l, r) = (scratch("l"), scratch("r"));
        fs::write(l.join("same.txt"), "abc").unwrap();
        fs::write(r.join("same.txt"), "abc").unwrap();
        fs::write(l.join("diff.txt"), "abc").unwrap();
        fs::write(r.join("diff.txt"), "abcd").unwrap();
        fs::write(l.join("left.txt"), "x").unwrap();
        fs::write(r.join("right.txt"), "y").unwrap();
        fs::create_dir(l.join("sub")).unwrap();
        fs::create_dir(r.join("sub")).unwrap();
        fs::write(l.join("sub").join("inner.txt"), "1").unwrap();
        fs::write(r.join("sub").join("inner.txt"), "2").unwrap();
        let rows = compare(&l, &r, Opts { subfolders: true, content: false });
        let st = |n: &str| rows.iter().find(|x| x.rel == n).map(|x| x.status);
        assert_eq!(st("same.txt"), Some(Status::Same));
        assert_eq!(st("diff.txt"), Some(Status::Different));
        assert_eq!(st("left.txt"), Some(Status::OnlyLeft));
        assert_eq!(st("right.txt"), Some(Status::OnlyRight));
        // same size and (nearly) same time: only the content shows the difference in sub\inner.txt
        let rows = compare(&l, &r, Opts { subfolders: true, content: true });
        assert_eq!(rows.iter().find(|x| x.rel == "sub\\inner.txt").map(|x| x.status), Some(Status::Different));
        let flat = compare(&l, &r, Opts { subfolders: false, content: true });
        assert!(flat.iter().all(|x| !x.rel.contains('\\')));
        let _ = fs::remove_dir_all(&l);
        let _ = fs::remove_dir_all(&r);
    }
}
