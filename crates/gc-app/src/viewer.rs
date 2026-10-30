//! F3 text viewer: read-only, scrolls by rows so large files stay smooth.

use std::io::Read;
use std::path::Path;

use eframe::egui::{self, Key};

/// Files are cut after this many bytes (the title says so).
const MAX_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Default)]
pub struct Viewer {
    open: Option<Doc>,
}

struct Doc {
    title: String,
    lines: Vec<String>,
}

impl Viewer {
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Loads `path` into the viewer; non-UTF-8 bytes are shown as replacement characters.
    pub fn open(&mut self, path: &Path) -> std::io::Result<()> {
        let mut bytes = Vec::new();
        let file = std::fs::File::open(path)?;
        let total = file.metadata()?.len();
        file.take(MAX_BYTES).read_to_end(&mut bytes)?;
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let title = if total > MAX_BYTES { format!("{name} (first {} MB)", MAX_BYTES >> 20) } else { name };
        let lines = String::from_utf8_lossy(&bytes).lines().map(|l| l.replace('\t', "    ")).collect();
        self.open = Some(Doc { title, lines });
        Ok(())
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        let Some(doc) = &self.open else { return };
        let close = ctx.input(|i| i.key_pressed(Key::Escape) || i.key_pressed(Key::F3) || i.key_pressed(Key::Q));
        let mut open = !close;
        let row_h = ctx.fonts(|f| f.row_height(&egui::FontId::monospace(13.0)));
        egui::Window::new(format!("View: {}", doc.title))
            .id(egui::Id::new("viewer"))
            .open(&mut open)
            .default_size([900.0, 560.0])
            .show(ctx, |ui| {
                egui::ScrollArea::both().auto_shrink(false).show_rows(ui, row_h, doc.lines.len(), |ui, range| {
                    for line in &doc.lines[range] {
                        ui.add(egui::Label::new(egui::RichText::new(line).monospace().size(13.0)).extend());
                    }
                });
            });
        if !open {
            self.open = None;
        }
    }
}
