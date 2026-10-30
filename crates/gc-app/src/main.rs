mod panel;

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Duration;

use eframe::egui::{self, Key};
use gc_core::jobs::{self, Job, Op};
use panel::Panel;

enum Dialog {
    Confirm(Op),
    MkDir(String),
}

struct RunningJob {
    job: Job,
    /// Panels were refreshed after this job finished.
    refreshed: bool,
}

struct App {
    panels: [Panel; 2],
    active: usize,
    dialog: Option<Dialog>,
    jobs: Vec<RunningJob>,
    status: Option<String>,
}

impl App {
    fn new() -> Self {
        let start = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self {
            panels: [Panel::new(start.clone()), Panel::new(start)],
            active: 0,
            dialog: None,
            jobs: Vec::new(),
            status: None,
        }
    }

    fn reload_all(&mut self) {
        for p in &mut self.panels {
            p.reload();
        }
    }

    fn start_job(&mut self, op: Op) {
        self.jobs.push(RunningJob { job: jobs::spawn(op), refreshed: false });
    }

    fn open_dialog_for(&mut self, key: Key) {
        let srcs = self.panels[self.active].targets();
        let dest_dir = self.panels[1 - self.active].cwd.clone();
        self.dialog = match key {
            Key::F5 if !srcs.is_empty() => Some(Dialog::Confirm(Op::Copy { srcs, dest_dir })),
            Key::F6 if !srcs.is_empty() => Some(Dialog::Confirm(Op::Move { srcs, dest_dir })),
            Key::F8 if !srcs.is_empty() => Some(Dialog::Confirm(Op::Delete { srcs })),
            Key::F7 => Some(Dialog::MkDir(String::new())),
            _ => None,
        };
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        let pressed = |ctx: &egui::Context, k: Key| ctx.input(|i| i.key_pressed(k));
        let panel = &mut self.panels[self.active];
        if pressed(ctx, Key::ArrowDown) {
            panel.move_cursor(1);
        }
        if pressed(ctx, Key::ArrowUp) {
            panel.move_cursor(-1);
        }
        if pressed(ctx, Key::PageDown) {
            panel.move_cursor(panel.page_size());
        }
        if pressed(ctx, Key::PageUp) {
            panel.move_cursor(-panel.page_size());
        }
        if pressed(ctx, Key::Home) {
            panel.cursor_to_start();
        }
        if pressed(ctx, Key::End) {
            panel.cursor_to_end();
        }
        if pressed(ctx, Key::Enter) {
            panel.enter();
        }
        if pressed(ctx, Key::Backspace) {
            panel.go_up();
        }
        if pressed(ctx, Key::Space) || pressed(ctx, Key::Insert) {
            panel.toggle_mark_and_advance();
        }
        if pressed(ctx, Key::Tab) {
            self.active = 1 - self.active;
        }
        for key in [Key::F5, Key::F6, Key::F7, Key::F8] {
            if pressed(ctx, key) {
                self.open_dialog_for(key);
            }
        }
    }

    fn show_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.dialog.take() else { return };
        let enter = ctx.input(|i| i.key_pressed(Key::Enter));
        let escape = ctx.input(|i| i.key_pressed(Key::Escape));
        let mut keep: Option<Dialog> = None;
        let mut run: Option<Op> = None;
        let mut mkdir: Option<String> = None;

        match dialog {
            Dialog::Confirm(op) => {
                let text = match &op {
                    Op::Copy { srcs, dest_dir } => format!("Copy {} item(s) to\n{}", srcs.len(), dest_dir.display()),
                    Op::Move { srcs, dest_dir } => format!("Move {} item(s) to\n{}", srcs.len(), dest_dir.display()),
                    Op::Delete { srcs } => format!("Permanently delete {} item(s)?", srcs.len()),
                };
                let mut ok = enter;
                let mut cancel = escape;
                egui::Window::new(op.label()).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
                    ui.label(text);
                    ui.horizontal(|ui| {
                        ok |= ui.button("OK (Enter)").clicked();
                        cancel |= ui.button("Cancel (Esc)").clicked();
                    });
                });
                if ok {
                    run = Some(op);
                } else if !cancel {
                    keep = Some(Dialog::Confirm(op));
                }
            }
            Dialog::MkDir(mut name) => {
                let mut ok = enter;
                let mut cancel = escape;
                egui::Window::new("New folder").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
                    ui.add(egui::TextEdit::singleline(&mut name)).request_focus();
                    ui.horizontal(|ui| {
                        ok |= ui.button("Create (Enter)").clicked();
                        cancel |= ui.button("Cancel (Esc)").clicked();
                    });
                });
                if ok {
                    mkdir = Some(name);
                } else if !cancel {
                    keep = Some(Dialog::MkDir(name));
                }
            }
        }

        self.dialog = keep;
        if let Some(op) = run {
            self.start_job(op);
        }
        if let Some(name) = mkdir {
            match jobs::make_dir(&self.panels[self.active].cwd, &name) {
                Ok(_) => {
                    self.status = None;
                    self.reload_all();
                }
                Err(e) => self.status = Some(format!("Create folder failed: {e}")),
            }
        }
    }

    fn poll_jobs(&mut self) {
        let mut finished_now = false;
        for j in &mut self.jobs {
            if !j.refreshed && j.job.progress.finished.load(Ordering::Acquire) {
                j.refreshed = true;
                finished_now = true;
            }
        }
        if finished_now {
            self.reload_all();
        }
        // Successful jobs disappear; failed ones stay until dismissed.
        self.jobs.retain(|j| !(j.refreshed && j.job.progress.errors().is_empty()));
    }

    fn show_jobs(&mut self, ui: &mut egui::Ui) {
        let mut dismiss: Option<usize> = None;
        for (i, j) in self.jobs.iter().enumerate() {
            let p = &j.job.progress;
            ui.horizontal(|ui| {
                ui.label(&j.job.label);
                if p.finished.load(Ordering::Acquire) {
                    if ui.small_button("x").clicked() {
                        dismiss = Some(i);
                    }
                    ui.colored_label(egui::Color32::LIGHT_RED, format!("{} error(s)", p.errors().len()));
                } else {
                    let total = p.total.load(Ordering::Relaxed).max(1);
                    let frac = (p.done.load(Ordering::Relaxed) as f32 / total as f32).clamp(0.0, 1.0);
                    ui.add(egui::ProgressBar::new(frac).desired_width(200.0).show_percentage());
                    if ui.small_button("Cancel").clicked() {
                        p.cancel.store(true, Ordering::Relaxed);
                    }
                }
            });
            if p.finished.load(Ordering::Acquire) {
                for err in p.errors() {
                    ui.colored_label(egui::Color32::LIGHT_RED, err);
                }
            }
        }
        if let Some(i) = dismiss {
            self.jobs.remove(i);
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let dialog_open = self.dialog.is_some();
        if !dialog_open {
            self.handle_keys(ctx);
        }
        self.poll_jobs();

        egui::TopBottomPanel::bottom("bottom").show(ctx, |ui| {
            if let Some(s) = &self.status {
                ui.colored_label(egui::Color32::LIGHT_RED, s);
            }
            self.show_jobs(ui);
            ui.horizontal(|ui| {
                for (key, label) in [("F5", "Copy"), ("F6", "Move"), ("F7", "MkDir"), ("F8", "Delete")] {
                    ui.monospace(format!("{key} {label}"));
                    ui.add_space(12.0);
                }
                ui.monospace("Tab switch | Space mark | Bksp up");
            });
        });

        let mut clicked: Option<usize> = None;
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.columns(2, |cols| {
                for (i, col) in cols.iter_mut().enumerate() {
                    if self.panels[i].ui(col, &format!("panel{i}"), self.active == i) {
                        clicked = Some(i);
                    }
                }
            });
        });
        if let Some(i) = clicked {
            self.active = i;
        }

        self.show_dialog(ctx);

        if self.jobs.iter().any(|j| !j.job.progress.finished.load(Ordering::Acquire)) {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Ghost Commander")
            .with_inner_size([1200.0, 700.0]),
        ..Default::default()
    };
    eframe::run_native("Ghost Commander", options, Box::new(|_cc| Ok(Box::new(App::new()))))
}
