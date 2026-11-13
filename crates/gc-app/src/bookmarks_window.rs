//! Ctrl+B: bookmarked folders. Enter / double click shows a bookmark in the active panel; the
//! current folder is added with Insert or the button. Saved in `settings.toml` (no history).

use std::path::{Path, PathBuf};

use eframe::egui::{self, Key};

use crate::theme::{load_settings, save_settings};

pub struct Bookmarks {
    pub open: bool,
    list: Vec<String>,
    sel: usize,
    go: Option<PathBuf>,
    message: Option<String>,
}

impl Bookmarks {
    pub fn load() -> Self {
        Self { open: false, list: load_settings().bookmarks, sel: 0, go: None, message: None }
    }

    /// The bookmark the user chose to go to, once.
    pub fn take_go(&mut self) -> Option<PathBuf> {
        self.go.take()
    }

    fn save(&mut self) {
        let mut s = load_settings();
        s.bookmarks = self.list.clone();
        self.message = save_settings(&s).err().map(|e| format!("Cannot save settings: {e}"));
    }

    fn add(&mut self, folder: &Path) {
        let text = folder.display().to_string();
        if self.list.iter().any(|b| b.eq_ignore_ascii_case(&text)) {
            self.message = Some("That folder is already bookmarked.".into());
            return;
        }
        self.list.push(text);
        self.sel = self.list.len() - 1;
        self.message = None;
        self.save();
    }

    /// `current`: the folder of the active panel (`None` inside an archive or on a server).
    pub fn show(&mut self, ctx: &egui::Context, current: Option<&Path>) {
        if !self.open {
            return;
        }
        let typing = ctx.wants_keyboard_input();
        let mut open = true;
        let (mut go, mut remove, mut add) = (None, false, false);
        egui::Window::new("Bookmarks").id(egui::Id::new("gc_bookmarks")).open(&mut open).default_size([520.0, 360.0]).collapsible(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
            if !typing {
                ui.input(|i| {
                    if i.key_pressed(Key::ArrowDown) {
                        self.sel = (self.sel + 1).min(self.list.len().saturating_sub(1));
                    }
                    if i.key_pressed(Key::ArrowUp) {
                        self.sel = self.sel.saturating_sub(1);
                    }
                    if i.key_pressed(Key::Enter) && self.sel < self.list.len() {
                        go = Some(self.sel);
                    }
                    remove |= i.key_pressed(Key::Delete);
                    add |= i.key_pressed(Key::Insert);
                });
            }
            if self.list.is_empty() {
                ui.weak("No bookmarks yet. Press Insert (or the button) to bookmark the folder of the active panel.");
            }
            egui::ScrollArea::vertical().max_height(ui.available_height() - 40.0).auto_shrink([false, true]).show(ui, |ui| {
                for (i, b) in self.list.iter().enumerate() {
                    let r = ui.selectable_label(i == self.sel, egui::RichText::new(b).monospace());
                    if r.clicked() {
                        self.sel = i;
                    }
                    if r.double_clicked() {
                        go = Some(i);
                    }
                    if i == self.sel && ui.input(|i| i.key_pressed(Key::ArrowDown) || i.key_pressed(Key::ArrowUp)) {
                        r.scroll_to_me(None);
                    }
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                if ui.add_enabled(current.is_some(), egui::Button::new("Add current folder  (Ins)")).clicked() {
                    add = true;
                }
                if ui.add_enabled(self.sel < self.list.len(), egui::Button::new("Remove  (Del)")).clicked() {
                    remove = true;
                }
                if ui.add_enabled(self.sel > 0 && self.sel < self.list.len(), egui::Button::new(egui_phosphor::regular::CARET_UP)).on_hover_text("Move up").clicked() {
                    self.list.swap(self.sel, self.sel - 1);
                    self.sel -= 1;
                    self.save();
                }
                if ui.add_enabled(self.sel + 1 < self.list.len(), egui::Button::new(egui_phosphor::regular::CARET_DOWN)).on_hover_text("Move down").clicked() {
                    self.list.swap(self.sel, self.sel + 1);
                    self.sel += 1;
                    self.save();
                }
                if ui.add_enabled(self.sel < self.list.len(), egui::Button::new("Go  (Enter)")).clicked() {
                    go = Some(self.sel);
                }
            });
            if let Some(m) = &self.message {
                ui.colored_label(ui.visuals().warn_fg_color, m);
            }
        });
        if add {
            if let Some(c) = current {
                self.add(c);
            }
        }
        if remove && self.sel < self.list.len() {
            self.list.remove(self.sel);
            self.sel = self.sel.min(self.list.len().saturating_sub(1));
            self.save();
        }
        if let Some(i) = go {
            let p = PathBuf::from(&self.list[i]);
            if p.is_dir() {
                self.go = Some(p);
                self.open = false;
            } else {
                self.message = Some(format!("{} does not exist (any more).", self.list[i]));
            }
        }
        if !open || (ctx.input(|i| i.key_pressed(Key::Escape)) && !typing) {
            self.open = false;
        }
    }
}
