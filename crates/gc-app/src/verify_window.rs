//! Enter on a `.crc / .sfv / .md5 / .sha1 / .sha256` file: checks every file it lists
//! (looked up next to the checksum file) and shows OK / FAILED / MISSING per file.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use eframe::egui::{self, Key, RichText};
use egui_phosphor::regular as icon;
use gc_core::hash::{self, HashJob};

use crate::theme::Theme;

struct Row {
    name: String,
    expected: String,
    /// Index into the hash job, or `None` when the file does not exist.
    job_index: Option<usize>,
}

enum State {
    Pending,
    Ok,
    Failed(String),
    Missing,
    Error(String),
}

#[derive(Default)]
pub struct VerifyWindow {
    pub open: bool,
    title: String,
    rows: Vec<Row>,
    job: Option<Arc<HashJob>>,
    error: Option<String>,
}

impl VerifyWindow {
    pub fn start(&mut self, ctx: &egui::Context, checksum_file: &Path) {
        self.stop();
        self.rows.clear();
        self.error = None;
        self.open = true;
        self.title = checksum_file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let text = match std::fs::read(checksum_file) {
            Ok(b) => String::from_utf8_lossy(&b).into_owned(),
            Err(e) => {
                self.error = Some(e.to_string());
                return;
            }
        };
        // `x.iso.sha256` holding a bare hash refers to `x.iso`.
        let default_name = checksum_file.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let dir = checksum_file.parent().map(Path::to_path_buf).unwrap_or_default();
        let mut paths: Vec<PathBuf> = Vec::new();
        for e in hash::parse_checksum_file(&text, &default_name) {
            let path = dir.join(e.name.replace('/', "\\"));
            let job_index = path.is_file().then(|| {
                paths.push(path);
                paths.len() - 1
            });
            self.rows.push(Row { name: e.name, expected: e.value, job_index });
        }
        if self.rows.is_empty() {
            self.error = Some("No checksums found in this file.".into());
        } else if !paths.is_empty() {
            let ctx = ctx.clone();
            self.job = Some(hash::spawn(paths, move || ctx.request_repaint()));
        }
    }

    fn stop(&mut self) {
        if let Some(job) = self.job.take() {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }

    fn state(&self, row: &Row) -> State {
        let (Some(j), Some(job)) = (row.job_index, &self.job) else { return State::Missing };
        match job.files[j].result.lock().unwrap().as_ref() {
            None => State::Pending,
            Some(Err(e)) => State::Error(e.clone()),
            Some(Ok(d)) => match d.by_len(row.expected.len()) {
                Some(actual) if actual == row.expected => State::Ok,
                Some(actual) => State::Failed(actual.to_string()),
                None => State::Error("unsupported checksum length".into()),
            },
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, theme: &Theme) {
        if !self.open {
            return;
        }
        let mut open = self.open;
        egui::Window::new(format!("Verify - {}", self.title)).id(egui::Id::new("verify_window")).open(&mut open).default_size([700.0, 420.0]).show(ctx, |ui| {
            if let Some(e) = &self.error {
                ui.colored_label(theme.error.0, e);
                return;
            }
            let states: Vec<State> = self.rows.iter().map(|r| self.state(r)).collect();
            let count = |f: fn(&State) -> bool| states.iter().filter(|s| f(s)).count();
            let (ok, bad, missing, pending) = (
                count(|s| matches!(s, State::Ok)),
                count(|s| matches!(s, State::Failed(_) | State::Error(_))),
                count(|s| matches!(s, State::Missing)),
                count(|s| matches!(s, State::Pending)),
            );
            if let Some(job) = &self.job {
                if !job.is_finished() {
                    ui.add(egui::ProgressBar::new(job.percent()).show_percentage().desired_width(ui.available_width()));
                }
            }
            let summary = format!("{ok} OK, {bad} failed, {missing} missing{}", if pending > 0 { format!(", {pending} pending") } else { String::new() });
            let color = if pending > 0 { ui.visuals().text_color() } else if bad + missing == 0 { theme.accent.0 } else { theme.error.0 };
            ui.label(RichText::new(summary).strong().color(color));
            ui.separator();
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                egui::Grid::new("verify_rows").num_columns(3).spacing([12.0, 4.0]).show(ui, |ui| {
                    for (row, state) in self.rows.iter().zip(&states) {
                        let (glyph, text, color) = match state {
                            State::Pending => (icon::HOURGLASS, "...".to_string(), ui.visuals().weak_text_color()),
                            State::Ok => (icon::CHECK_CIRCLE, "OK".to_string(), theme.accent.0),
                            State::Failed(actual) => (icon::X_CIRCLE, format!("FAILED  expected {}  got {actual}", row.expected), theme.error.0),
                            State::Missing => (icon::WARNING, "MISSING".to_string(), theme.error.0),
                            State::Error(e) => (icon::X_CIRCLE, e.clone(), theme.error.0),
                        };
                        ui.label(RichText::new(glyph).color(color));
                        ui.label(&row.name);
                        ui.label(RichText::new(text).monospace().color(color));
                        ui.end_row();
                    }
                });
            });
        });
        self.open = open && !ctx.input(|i| i.key_pressed(Key::Escape));
        if !self.open {
            self.stop();
        }
    }
}
