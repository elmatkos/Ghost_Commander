//! Tools > Checksums (Ctrl+H): CRC-32, MD5, SHA-1 and SHA-256 of the marked files, plus a
//! "compare with" box that flags the file whose checksum equals the pasted one.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use eframe::egui::{self, Key, RichText};
use egui_phosphor::regular as icon;
use gc_core::hash::{self, HashJob};

use crate::panel::fmt_size;
use crate::theme::Theme;

#[derive(Default)]
pub struct ChecksumsWindow {
    pub open: bool,
    job: Option<Arc<HashJob>>,
    compare: String,
    /// Result of the last "Save .crc" click.
    saved: Option<String>,
}

/// Writes `<file>.crc` for every file that was hashed; returns a one-line report.
fn save_crc_files(job: &HashJob) -> String {
    let (mut ok, mut last, mut errors) = (0, String::new(), Vec::new());
    for f in &job.files {
        if let Some(Ok(d)) = f.result.lock().unwrap().as_ref() {
            match hash::write_crc_file(&f.path, d) {
                Ok(p) => {
                    ok += 1;
                    last = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                }
                Err(e) => errors.push(format!("{}: {e}", f.path.display())),
            }
        }
    }
    let mut msg = if ok == 1 { format!("Saved {last}") } else { format!("Saved {ok} .crc files") };
    if !errors.is_empty() {
        msg.push_str(&format!(" - errors: {}", errors.join("; ")));
    }
    msg
}

impl ChecksumsWindow {
    pub fn start(&mut self, ctx: &egui::Context, paths: Vec<PathBuf>) {
        self.stop();
        self.saved = None;
        let ctx = ctx.clone();
        self.job = Some(hash::spawn(paths, move || ctx.request_repaint()));
        self.open = true;
    }

    fn stop(&mut self) {
        if let Some(job) = self.job.take() {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Everything computed so far as text, `sha256sum` style plus the other algorithms.
    fn to_text(job: &HashJob) -> String {
        let mut out = String::new();
        for f in &job.files {
            let name = f.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            match f.result.lock().unwrap().as_ref() {
                Some(Ok(d)) => out.push_str(&format!("{name}\nCRC32   {}\nMD5     {}\nSHA-1   {}\nSHA-256 {}\n\n", d.crc32, d.md5, d.sha1, d.sha256)),
                Some(Err(e)) => out.push_str(&format!("{name}\nerror: {e}\n\n")),
                None => {}
            }
        }
        out
    }

    pub fn show(&mut self, ctx: &egui::Context, theme: &Theme) {
        if !self.open {
            return;
        }
        let mut open = self.open;
        let mut restart = false;
        egui::Window::new("Checksums").open(&mut open).default_size([760.0, 420.0]).show(ctx, |ui| {
            let Some(job) = self.job.clone() else { return };
            ui.horizontal(|ui| {
                if ui.button(format!("{} Copy", icon::COPY)).on_hover_text("Copy all results as text").clicked() {
                    ctx.copy_text(Self::to_text(&job));
                }
                restart = ui.button(format!("{} Recalculate", icon::ARROWS_CLOCKWISE)).clicked();
                let done = job.is_finished();
                if ui.add_enabled(done, egui::Button::new(format!("{} Save .crc", icon::FLOPPY_DISK))).on_hover_text("Write <file>.crc (name + CRC-32) next to each file").clicked() {
                    self.saved = Some(save_crc_files(&job));
                }
                ui.label("Compare with:");
                ui.add(egui::TextEdit::singleline(&mut self.compare).hint_text("paste a checksum").desired_width(380.0));
            });
            if let Some(msg) = &self.saved {
                ui.label(msg);
            }
            if job.is_finished() {
                ui.label(format!("{} file(s), {}", job.files.len(), fmt_size(job.total)));
            } else {
                ui.add(egui::ProgressBar::new(job.percent()).show_percentage().desired_width(ui.available_width()));
            }
            ui.separator();
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                for (n, f) in job.files.iter().enumerate() {
                    let name = f.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    ui.label(RichText::new(format!("{}  {name}", icon::FILE)).strong().color(theme.accent.0));
                    match f.result.lock().unwrap().as_ref() {
                        None => {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(format!("{} - waiting or calculating...", fmt_size(f.size)));
                            });
                        }
                        Some(Err(e)) => {
                            ui.colored_label(theme.error.0, e);
                        }
                        Some(Ok(d)) => {
                            let hit = d.matches(&self.compare);
                            egui::Grid::new(("checksums", n)).num_columns(2).spacing([16.0, 3.0]).show(ui, |ui| {
                                for (label, value) in [("CRC32", &d.crc32), ("MD5", &d.md5), ("SHA-1", &d.sha1), ("SHA-256", &d.sha256)] {
                                    ui.label(RichText::new(label).monospace().color(ui.visuals().weak_text_color()));
                                    let same = hit && value.as_str() == self.compare.split_whitespace().collect::<String>().to_lowercase();
                                    let mut text = RichText::new(value).monospace();
                                    if same {
                                        text = text.color(theme.accent.0).strong();
                                    }
                                    ui.horizontal(|ui| {
                                        ui.add(egui::Label::new(text).selectable(true));
                                        if same {
                                            ui.label(RichText::new(format!("{} match", icon::CHECK_CIRCLE)).color(theme.accent.0));
                                        }
                                    });
                                    ui.end_row();
                                }
                            });
                        }
                    }
                    ui.add_space(8.0);
                }
            });
        });
        self.open = open && !ctx.input(|i| i.key_pressed(Key::Escape));
        if !self.open {
            self.stop();
        } else if restart {
            if let Some(job) = &self.job {
                let paths = job.files.iter().map(|f| f.path.clone()).collect();
                self.start(ctx, paths);
            }
        }
    }
}
