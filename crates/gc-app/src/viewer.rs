//! F3 text viewer: read-only, scrolls by rows so large files stay smooth. Source files of the
//! languages known to `syntax` are coloured (only the rows on screen are coloured each frame).

use std::io::Read;
use std::path::Path;

use eframe::egui::{self, text::LayoutJob, Color32, FontId, Key, TextFormat};

use crate::syntax::{self, Lang, State};

/// Files are cut after this many bytes (the title says so).
const MAX_BYTES: u64 = 8 * 1024 * 1024;
const FONT_SIZE: f32 = 13.0;

#[derive(Default)]
pub struct Viewer {
    open: Option<Doc>,
    /// Opened during the current frame.
    fresh: bool,
}

struct Doc {
    title: String,
    lines: Vec<String>,
    lang: Option<Lang>,
    /// What each line starts inside of (block comments, here-strings, ...); empty without a language.
    starts: Vec<State>,
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
        let mut title = if total > MAX_BYTES { format!("{name} (first {} MB)", MAX_BYTES >> 20) } else { name };
        let lines: Vec<String> = String::from_utf8_lossy(&bytes).lines().map(|l| l.replace('\t', "    ")).collect();
        let lang = syntax::detect(path);
        let starts = lang.map(|l| syntax::line_states(l, lines.iter().map(String::as_str))).unwrap_or_default();
        if let Some(l) = lang {
            title = format!("{title}  [{}]", l.name());
        }
        self.open = Some(Doc { title, lines, lang, starts });
        self.fresh = true;
        Ok(())
    }

    pub fn show(&mut self, ctx: &egui::Context, dark: bool) {
        let Some(doc) = &self.open else { return };
        // The F3 that opened the viewer is still "pressed" in this very frame: ignore it once.
        let close = !std::mem::take(&mut self.fresh) && ctx.input(|i| i.key_pressed(Key::Escape) || i.key_pressed(Key::F3) || i.key_pressed(Key::Q));
        let mut open = !close;
        let row_h = ctx.fonts(|f| f.row_height(&egui::FontId::monospace(FONT_SIZE)));
        egui::Window::new(format!("View: {}", doc.title))
            .id(egui::Id::new("viewer"))
            .open(&mut open)
            .default_size([900.0, 560.0])
            .show(ctx, |ui| {
                let plain = ui.visuals().text_color();
                egui::ScrollArea::both().auto_shrink(false).show_rows(ui, row_h, doc.lines.len(), |ui, range| {
                    for n in range {
                        let line = &doc.lines[n];
                        match (doc.lang, doc.starts.get(n)) {
                            (Some(lang), Some(&st)) => {
                                ui.add(egui::Label::new(colored(line, lang, st, dark, plain)).extend());
                            }
                            _ => {
                                ui.add(egui::Label::new(egui::RichText::new(line).monospace().size(FONT_SIZE)).extend());
                            }
                        }
                    }
                });
            });
        if !open {
            self.open = None;
        }
    }
}

/// One line as a layout job with a colour per syntax span.
fn colored(line: &str, lang: Lang, st: State, dark: bool, plain: Color32) -> LayoutJob {
    let font = FontId::monospace(FONT_SIZE);
    let fmt = |color: Color32| TextFormat { font_id: font.clone(), color, ..Default::default() };
    let mut job = LayoutJob::default();
    let mut pos = 0;
    for (range, kind) in syntax::scan(lang, line, st).0 {
        if range.start > pos {
            job.append(&line[pos..range.start], 0.0, fmt(plain));
        }
        job.append(&line[range.clone()], 0.0, fmt(syntax::color(kind, dark)));
        pos = range.end;
    }
    if pos < line.len() {
        job.append(&line[pos..], 0.0, fmt(plain));
    }
    if line.is_empty() {
        job.append(" ", 0.0, fmt(plain));
    }
    job
}
