use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;

use eframe::egui::{self, Align2, FontId, Sense, Stroke, StrokeKind};
use gc_archive::ArchiveIndex;
use gc_core::{list_dir, Entry};

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

/// Something the user asked for that only the app can do (it owns the other panel and the jobs).
pub enum PanelAction {
    /// Right click on disk items: show the Windows shell menu for them.
    ShellMenu(Vec<PathBuf>),
    /// Enter on a plain file: open it with the Windows default program.
    Open(PathBuf),
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
    ctx: egui::Context,
    pub action: Option<PanelAction>,
    entries: Vec<Entry>,
    cursor: usize,
    marked: HashSet<PathBuf>,
    error: Option<String>,
    scroll_to_cursor: bool,
    last_offset: f32,
    last_height: f32,
}

impl Panel {
    pub fn new(cwd: PathBuf, ctx: egui::Context) -> Self {
        let mut p = Self {
            cwd,
            archive: None,
            stack: Vec::new(),
            loading: None,
            ctx,
            action: None,
            entries: Vec::new(),
            cursor: 0,
            marked: HashSet::new(),
            error: None,
            scroll_to_cursor: false,
            last_offset: 0.0,
            last_height: 0.0,
        };
        p.reload();
        p
    }

    /// Re-reads the directory and keeps the cursor on the same name when possible.
    pub fn reload(&mut self) {
        let keep = self.entries.get(self.cursor).map(|e| e.name.clone());
        let listing = match &self.archive {
            Some(a) => Ok(a.index.list(&a.inner)),
            None => list_dir(&self.cwd),
        };
        match listing {
            Ok(mut entries) => {
                // Inside an archive there is always a way up (to the folder holding it).
                if let Some(parent) = self.cwd.parent() {
                    entries.insert(
                        0,
                        Entry { name: PARENT.into(), path: parent.to_path_buf(), is_dir: true, size: 0, modified: None },
                    );
                }
                self.entries = entries;
                self.error = None;
            }
            Err(e) => {
                self.entries.clear();
                self.error = Some(e.to_string());
            }
        }
        self.cursor = keep
            .and_then(|n| self.entries.iter().position(|e| e.name == n))
            .unwrap_or_else(|| self.cursor.min(self.entries.len().saturating_sub(1)));
        self.marked.retain(|m| self.entries.iter().any(|e| &e.path == m));
        self.scroll_to_cursor = true;
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
        if self.loading.is_some() {
            return;
        }
        let Some(e) = self.entries.get(self.cursor).cloned() else { return };
        if e.name == PARENT {
            self.go_up();
        } else if let Some(a) = &mut self.archive {
            if e.is_dir {
                a.inner = join_inner(&a.inner, &e.name);
                self.enter_listing();
            } else if gc_archive::is_archive(&e.path) {
                let index = a.index.clone();
                let inner = join_inner(&a.inner, &e.name);
                let label = format!("{}\\{}", self.location(), e.name);
                self.start_loading(label, true, move || gc_archive::open_nested(&index, &inner));
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
        self.reload();
    }

    /// Switches to another root (e.g. a drive).
    pub fn open_root(&mut self, path: PathBuf) {
        self.go_to(path, None);
    }

    pub fn go_up(&mut self) {
        self.loading = None; // Backspace also cancels an archive that is still opening
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
            self.reload();
            self.error = err;
        } else if let Some(name) = select {
            self.select_name(&name);
        }
    }

    /// The folder on disk this panel shows, `None` while an archive is open.
    pub fn disk_folder(&self) -> Option<&Path> {
        self.archive.is_none().then_some(self.cwd.as_path())
    }

    /// Location text for the header, e.g. `C:\data\x.zip\docs`.
    fn location(&self) -> String {
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
        (self.archive.is_none() && !e.is_dir && e.name != PARENT).then(|| e.path.clone())
    }

    /// Images of the shown folder in listing order, for browsing in the image viewer.
    pub fn image_files(&self) -> Vec<PathBuf> {
        let on_disk = self.archive.is_none();
        self.entries.iter().filter(|e| on_disk && !e.is_dir && crate::image_viewer::is_image(&e.path)).map(|e| e.path.clone()).collect()
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
            if let Some(l) = &self.loading {
                ui.spinner();
                ui.label(format!("Opening {}...", l.label));
            } else {
                ui.add(egui::Label::new(egui::RichText::new(self.location()).monospace()).truncate());
            }
        });
        if let Some(err) = &self.error {
            ui.colored_label(theme.error.0, err);
        }

        ui.spacing_mut().item_spacing.y = 0.0;
        let mut area = egui::ScrollArea::vertical().id_salt(id).auto_shrink(false);
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
                        if self.archive.is_none() {
                            let paths = if self.marked.contains(&path) { self.targets() } else { vec![path] };
                            self.action = Some(PanelAction::ShellMenu(paths));
                        } else if !self.marked.contains(&path) {
                            self.marked.clear();
                        }
                    }
                    if self.archive.is_some() {
                        let mut open = false;
                        resp.context_menu(|ui| {
                            if ui.button("Open (Enter)").clicked() {
                                open = true;
                                ui.close_menu();
                            }
                            if ui.button("Extract to other panel (F5)").clicked() {
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

/// Name of the synthetic first row that leads to the parent folder.
/// No real entry can have this name, so it is safe to compare against.
const PARENT: &str = "..";
const ROW_H: f32 = 20.0;
/// Space before the name for the file-type icon.
const ICON_COL_W: f32 = 22.0;
/// Width reserved for the "YYYY-MM-DD HH:MM" column plus a gap.
const DATE_COL_W: f32 = 140.0;

fn fmt_size(bytes: u64) -> String {
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

    #[test]
    fn sizes_are_human_readable() {
        assert_eq!(fmt_size(0), "0 B");
        assert_eq!(fmt_size(1023), "1023 B");
        assert_eq!(fmt_size(1536), "1.5 KB");
        assert_eq!(fmt_size(5 * 1024 * 1024 * 1024), "5.0 GB");
    }
}
