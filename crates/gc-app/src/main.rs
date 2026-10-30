use std::path::PathBuf;

use eframe::egui;
use gc_core::{list_dir, Entry};

struct App {
    cwd: PathBuf,
    entries: Vec<Entry>,
    error: Option<String>,
}

impl App {
    fn new() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let mut app = Self { cwd, entries: Vec::new(), error: None };
        app.reload();
        app
    }

    fn reload(&mut self) {
        match list_dir(&self.cwd) {
            Ok(entries) => {
                self.entries = entries;
                self.error = None;
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    fn go_to(&mut self, path: PathBuf) {
        let previous = std::mem::replace(&mut self.cwd, path);
        self.reload();
        if self.error.is_some() {
            self.cwd = previous;
            let err = self.error.take();
            self.reload();
            self.error = err;
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut target: Option<PathBuf> = None;

        egui::TopBottomPanel::top("path").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("..").clicked() {
                    target = self.cwd.parent().map(|p| p.to_path_buf());
                }
                ui.monospace(self.cwd.display().to_string());
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(err) = &self.error {
                ui.colored_label(egui::Color32::LIGHT_RED, err);
            }
            let row_h = ui.text_style_height(&egui::TextStyle::Monospace) + 4.0;
            egui::ScrollArea::vertical().auto_shrink(false).show_rows(
                ui,
                row_h,
                self.entries.len(),
                |ui, range| {
                    for e in &self.entries[range] {
                        ui.horizontal(|ui| {
                            let label = if e.is_dir { format!("[{}]", e.name) } else { e.name.clone() };
                            if ui.selectable_label(false, label).double_clicked() && e.is_dir {
                                target = Some(e.path.clone());
                            }
                            if !e.is_dir {
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.monospace(e.size.to_string());
                                });
                            }
                        });
                    }
                },
            );
        });

        if let Some(path) = target {
            self.go_to(path);
        }
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Ghost Commander")
            .with_inner_size([1000.0, 640.0]),
        ..Default::default()
    };
    eframe::run_native("Ghost Commander", options, Box::new(|_cc| Ok(Box::new(App::new()))))
}
