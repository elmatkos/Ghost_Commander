use std::collections::HashSet;
use std::path::PathBuf;

use eframe::egui::{self, Align2, Color32, FontId, Sense, Stroke, StrokeKind};
use gc_core::{list_dir, Entry};

pub struct Panel {
    pub cwd: PathBuf,
    entries: Vec<Entry>,
    cursor: usize,
    marked: HashSet<PathBuf>,
    error: Option<String>,
    scroll_to_cursor: bool,
    last_offset: f32,
    last_height: f32,
}

impl Panel {
    pub fn new(cwd: PathBuf) -> Self {
        let mut p = Self {
            cwd,
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
        match list_dir(&self.cwd) {
            Ok(mut entries) => {
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

    /// Enters the directory under the cursor.
    pub fn enter(&mut self) {
        if let Some(e) = self.entries.get(self.cursor).filter(|e| e.is_dir) {
            if e.name == PARENT {
                self.go_up();
            } else {
                let path = e.path.clone();
                self.go_to(path, None);
            }
        }
    }

    /// Switches to another root (e.g. a drive).
    pub fn open_root(&mut self, path: PathBuf) {
        self.go_to(path, None);
    }

    pub fn go_up(&mut self) {
        if let Some(parent) = self.cwd.parent().map(|p| p.to_path_buf()) {
            let came_from = self.cwd.file_name().map(|n| n.to_string_lossy().into_owned());
            self.go_to(parent, came_from);
        }
    }

    fn go_to(&mut self, path: PathBuf, select: Option<String>) {
        let previous = std::mem::replace(&mut self.cwd, path);
        self.entries.clear();
        self.cursor = 0;
        self.marked.clear();
        self.reload();
        if self.error.is_some() {
            // Keep the old listing visible, show the error on top of it.
            let err = self.error.take();
            self.cwd = previous;
            self.reload();
            self.error = err;
        } else if let Some(name) = select {
            if let Some(i) = self.entries.iter().position(|e| e.name == name) {
                self.cursor = i;
            }
        }
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
    pub fn ui(&mut self, ui: &mut egui::Ui, id: &str, active: bool) -> bool {
        let mut clicked = false;
        let accent = ui.visuals().selection.bg_fill;

        ui.horizontal(|ui| {
            if ui.small_button("..").clicked() {
                self.go_up();
                clicked = true;
            }
            ui.add(egui::Label::new(egui::RichText::new(self.cwd.display().to_string()).monospace()).truncate());
        });
        if let Some(err) = &self.error {
            ui.colored_label(Color32::LIGHT_RED, err);
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
                let e = &self.entries[i];
                if resp.clicked() || resp.double_clicked() {
                    self.cursor = i;
                    clicked = true;
                }
                if resp.double_clicked() && e.is_dir {
                    if e.name == PARENT {
                        self.go_up();
                    } else {
                        let path = e.path.clone();
                        self.go_to(path, None);
                    }
                    return;
                }
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
                    Color32::from_rgb(255, 210, 90)
                } else if e.is_dir {
                    Color32::from_rgb(140, 200, 255)
                } else {
                    ui.visuals().text_color()
                };
                let font = FontId::monospace(13.0);
                let name = if e.is_dir { format!("[{}]", e.name) } else { e.name.clone() };
                painter.text(rect.left_center() + egui::vec2(6.0, 0.0), Align2::LEFT_CENTER, name, font.clone(), color);
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

/// Name of the synthetic first row that leads to the parent folder.
/// No real entry can have this name, so it is safe to compare against.
const PARENT: &str = "..";
const ROW_H: f32 = 20.0;
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

    #[test]
    fn parent_row_leads_up_and_is_never_an_operation_target() {
        let root = std::env::temp_dir().join(format!("gc_panel_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("child")).unwrap();
        std::fs::write(root.join("file.txt"), b"x").unwrap();

        let mut p = Panel::new(root.clone());
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
