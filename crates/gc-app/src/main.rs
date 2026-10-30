// GUI subsystem: no console window next to the app (also in debug builds).
#![windows_subsystem = "windows"]

mod about;
mod panel;
mod shell_menu;
mod theme;
mod theme_editor;
mod viewer;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use eframe::egui::{self, Key};
use gc_core::jobs::{self, Job, Op, Resolution};
use about::About;
use gc_archive::ArchiveIndex;
use panel::{Panel, PanelAction};
use theme_editor::ThemeManager;
use viewer::Viewer;

enum Dialog {
    Confirm(Op),
    MkDir(String),
    /// Drive picker for panel `panel`.
    Drives { panel: usize, list: Vec<PathBuf>, sel: usize },
    /// Extract inner paths of an archive into a folder.
    Extract { index: Arc<ArchiveIndex>, names: Vec<String>, dest: PathBuf },
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
    /// "Apply to all" checkbox state of the conflict dialog.
    apply_all: bool,
    about: About,
    themes: ThemeManager,
    viewer: Viewer,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_icon_font(&cc.egui_ctx);
        let start = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self {
            panels: [Panel::new(start.clone(), cc.egui_ctx.clone()), Panel::new(start, cc.egui_ctx.clone())],
            active: 0,
            dialog: None,
            jobs: Vec::new(),
            status: None,
            apply_all: false,
            about: About::default(),
            themes: ThemeManager::load(&cc.egui_ctx),
            viewer: Viewer::default(),
        }
    }

    /// F3: built-in viewer. F4: external editor.
    fn view_or_edit(&mut self, edit: bool) {
        let Some(file) = self.panels[self.active].current_file() else {
            self.status = Some("Select a file on disk first (archives and folders are not supported yet).".into());
            return;
        };
        self.status = None;
        let result = if edit {
            #[cfg(windows)]
            let editor = "notepad";
            #[cfg(not(windows))]
            let editor = "xdg-open";
            std::process::Command::new(editor).arg(&file).spawn().map(|_| ())
        } else {
            self.viewer.open(&file)
        };
        if let Err(e) = result {
            self.status = Some(format!("{}: {e}", file.display()));
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

    fn open_dialog_for(&mut self, key: Key, shift: bool) {
        let other = 1 - self.active;
        let dest_dir = self.panels[other].disk_folder().map(Path::to_path_buf);

        // Archive panels are read-only for now; F5 extracts.
        if let Some((index, names)) = self.panels[self.active].archive_selection() {
            self.dialog = None;
            match (key, dest_dir) {
                (Key::F5, Some(dest)) if !names.is_empty() => self.dialog = Some(Dialog::Extract { index, names, dest }),
                (Key::F5, None) => self.status = Some("Extract into a normal folder: the other panel shows an archive.".into()),
                (Key::F6 | Key::F7 | Key::F8, _) => self.status = Some("Archives are read-only for now. F5 extracts.".into()),
                _ => {}
            }
            return;
        }

        let srcs = self.panels[self.active].targets();
        self.dialog = match (key, dest_dir) {
            (Key::F5, Some(dest_dir)) if !srcs.is_empty() => Some(Dialog::Confirm(Op::Copy { srcs, dest_dir })),
            (Key::F6, Some(dest_dir)) if !srcs.is_empty() => Some(Dialog::Confirm(Op::Move { srcs, dest_dir })),
            (Key::F5 | Key::F6, None) => {
                self.status = Some("Packing into archives is not supported yet.".into());
                None
            }
            (Key::F8, _) if !srcs.is_empty() => Some(Dialog::Confirm(Op::Delete { srcs, permanent: shift })),
            (Key::F7, _) => Some(Dialog::MkDir(String::new())),
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
        if pressed(ctx, Key::F1) {
            self.about.open = true;
        }
        if pressed(ctx, Key::F9) {
            self.themes.open = true;
        }
        let shift = ctx.input(|i| i.modifiers.shift);
        for key in [Key::F5, Key::F6, Key::F7, Key::F8] {
            if pressed(ctx, key) {
                self.open_dialog_for(key, shift);
            }
        }
        // Del / Shift+Del behave like F8 / Shift+F8. egui turns Shift+Del into a Cut event
        // on Windows and never reports the key press, so look for the event too.
        // (Ctrl+X is a Cut event as well, hence the modifier check.)
        let cut = ctx.input(|i| i.modifiers.shift && !i.modifiers.command && i.events.iter().any(|e| matches!(e, egui::Event::Cut)));
        if pressed(ctx, Key::Delete) || cut {
            self.open_dialog_for(Key::F8, shift || cut);
        }
        if pressed(ctx, Key::F3) {
            self.view_or_edit(false);
        }
        if pressed(ctx, Key::F4) {
            self.view_or_edit(true);
        }
        let ctrl = ctx.input(|i| i.modifiers.ctrl);
        for (key, panel) in [(Key::Num1, 0), (Key::Num2, 1)] {
            if ctrl && pressed(ctx, key) {
                let list = gc_core::drives();
                let sel = list.iter().position(|d| self.panels[panel].cwd.starts_with(d)).unwrap_or(0);
                self.dialog = Some(Dialog::Drives { panel, list, sel });
            }
        }
    }

    /// Right-click requests raised by the panels while drawing.
    fn handle_panel_actions(&mut self, frame: &eframe::Frame) {
        for i in 0..2 {
            match self.panels[i].action.take() {
                Some(PanelAction::ShellMenu(paths)) => {
                    self.active = i;
                    self.status = match window_handle(frame) {
                        Some(hwnd) => shell_menu::show(hwnd, &paths).err().map(|e| format!("Context menu failed: {e}")),
                        None => Some("Context menu failed: no window handle.".into()),
                    };
                    // The menu may have deleted, renamed or created items.
                    self.reload_all();
                }
                Some(PanelAction::Extract) => {
                    self.active = i;
                    self.open_dialog_for(Key::F5, false);
                }
                None => {}
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
        let mut extract: Option<Job> = None;

        match dialog {
            Dialog::Confirm(op) => {
                let text = match &op {
                    Op::Copy { srcs, dest_dir } => format!("Copy {} item(s) to\n{}", srcs.len(), dest_dir.display()),
                    Op::Move { srcs, dest_dir } => format!("Move {} item(s) to\n{}", srcs.len(), dest_dir.display()),
                    Op::Delete { srcs, permanent: true } => format!("Permanently delete {} item(s)?\nThis cannot be undone.", srcs.len()),
                    Op::Delete { srcs, permanent: false } => format!("Move {} item(s) to the recycle bin?", srcs.len()),
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
            Dialog::Extract { index, names, dest } => {
                let mut ok = enter;
                let mut cancel = escape;
                egui::Window::new("Extract").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
                    ui.label(format!("Extract {} item(s) to\n{}", names.len(), dest.display()));
                    ui.horizontal(|ui| {
                        ok |= ui.button("OK (Enter)").clicked();
                        cancel |= ui.button("Cancel (Esc)").clicked();
                    });
                });
                if ok {
                    extract = Some(gc_archive::spawn_extract(index, names, dest));
                } else if !cancel {
                    keep = Some(Dialog::Extract { index, names, dest });
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
            Dialog::Drives { panel, list, mut sel } => {
                let (down, up) = ctx.input(|i| (i.key_pressed(Key::ArrowDown), i.key_pressed(Key::ArrowUp)));
                if down {
                    sel = (sel + 1).min(list.len().saturating_sub(1));
                }
                if up {
                    sel = sel.saturating_sub(1);
                }
                let mut chosen = enter.then_some(sel);
                egui::Window::new(format!("Drive for {} panel", if panel == 0 { "left" } else { "right" }))
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        for (i, d) in list.iter().enumerate() {
                            if ui.selectable_label(i == sel, d.display().to_string()).clicked() {
                                chosen = Some(i);
                            }
                        }
                    });
                if let Some(i) = chosen {
                    self.panels[panel].open_root(list[i].clone());
                    self.active = panel;
                } else if !escape {
                    keep = Some(Dialog::Drives { panel, list, sel });
                }
            }
        }

        self.dialog = keep;
        if let Some(op) = run {
            self.start_job(op);
        }
        if let Some(job) = extract {
            self.jobs.push(RunningJob { job, refreshed: false });
        }
        if let Some(name) = mkdir {
            let made = match self.panels[self.active].disk_folder() {
                Some(dir) => jobs::make_dir(dir, &name),
                None => Err(std::io::Error::other("archives are read-only")),
            };
            match made {
                Ok(_) => {
                    self.status = None;
                    self.reload_all();
                }
                Err(e) => self.status = Some(format!("Create folder failed: {e}")),
            }
        }
    }

    /// Shows the name-conflict dialog for the first job waiting on an answer.
    /// Returns true while a conflict is open (keyboard goes to the dialog).
    fn show_conflict(&mut self, ctx: &egui::Context) -> bool {
        let Some((progress, target)) = self
            .jobs
            .iter()
            .find_map(|j| j.job.progress.pending_conflict().map(|t| (j.job.progress.clone(), t)))
        else {
            return false;
        };
        let mut answer: Option<Resolution> = None;
        egui::Window::new("File exists").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
            ui.label(format!("{}\nalready exists.", target.display()));
            ui.checkbox(&mut self.apply_all, "Apply to all remaining conflicts");
            ui.horizontal(|ui| {
                if ui.button("Overwrite (O)").clicked() {
                    answer = Some(Resolution::Overwrite);
                }
                if ui.button("Skip (S)").clicked() {
                    answer = Some(Resolution::Skip);
                }
                if ui.button("Rename (R)").clicked() {
                    answer = Some(Resolution::Rename);
                }
                if ui.button("Cancel (Esc)").clicked() {
                    answer = Some(Resolution::Cancel);
                }
            });
        });
        for (key, res) in [(Key::O, Resolution::Overwrite), (Key::S, Resolution::Skip), (Key::R, Resolution::Rename), (Key::Escape, Resolution::Cancel)] {
            if ctx.input(|i| i.key_pressed(key)) {
                answer = Some(res);
            }
        }
        if let Some(res) = answer {
            progress.resolve(res, self.apply_all);
            self.apply_all = false;
        }
        true
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
        let error = self.themes.active.error.0;
        for (i, j) in self.jobs.iter().enumerate() {
            let p = &j.job.progress;
            ui.horizontal(|ui| {
                ui.label(&j.job.label);
                if p.finished.load(Ordering::Acquire) {
                    if ui.small_button("x").clicked() {
                        dismiss = Some(i);
                    }
                    ui.colored_label(error, format!("{} error(s)", p.errors().len()));
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
                    ui.colored_label(error, err);
                }
            }
        }
        if let Some(i) = dismiss {
            self.jobs.remove(i);
        }
    }
}

impl eframe::App for App {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        gc_archive::cleanup_nested();
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let dialog_open = self.dialog.is_some() || self.about.open || self.themes.open || self.viewer.is_open() || self.jobs.iter().any(|j| j.job.progress.pending_conflict().is_some());
        if !dialog_open {
            self.handle_keys(ctx);
        }
        self.poll_jobs();

        egui::TopBottomPanel::bottom("bottom").show(ctx, |ui| {
            if let Some(s) = &self.status {
                ui.colored_label(self.themes.active.error.0, s);
            }
            self.show_jobs(ui);
            ui.horizontal(|ui| {
                for (key, label) in [("F3", "View"), ("F4", "Edit"), ("F5", "Copy"),("F6", "Move"), ("F7", "MkDir"), ("F8/Del", "Recycle"), ("Shift+F8/Del", "Delete")] {
                    ui.monospace(format!("{key} {label}"));
                    ui.add_space(12.0);
                }
                ui.monospace("Tab switch | Space mark | Bksp up | Ctrl+1/2 drive");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("About (F1)").clicked() {
                        self.about.open = true;
                    }
                    if ui.small_button("Themes (F9)").clicked() {
                        self.themes.open = true;
                    }
                });
            });
        });

        let mut clicked: Option<usize> = None;
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.columns(2, |cols| {
                for (i, col) in cols.iter_mut().enumerate() {
                    if self.panels[i].ui(col, &format!("panel{i}"), self.active == i, &self.themes.active) {
                        clicked = Some(i);
                    }
                }
            });
        });
        if let Some(i) = clicked {
            self.active = i;
        }
        self.handle_panel_actions(frame);

        self.show_dialog(ctx);
        self.show_conflict(ctx);
        self.about.show(ctx);
        self.themes.show(ctx);
        self.viewer.show(ctx);

        if self.jobs.iter().any(|j| !j.job.progress.finished.load(Ordering::Acquire)) {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(concat!("Ghost Commander v", env!("CARGO_PKG_VERSION")))
            .with_icon(eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/icon_256.png")).expect("embedded icon is a valid PNG"))
            .with_inner_size([1200.0, 700.0]),
        ..Default::default()
    };
    eframe::run_native("Ghost Commander", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

/// Win32 handle of the main window, needed to own the shell context menu.
fn window_handle(frame: &eframe::Frame) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match frame.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

/// Registers the Phosphor icon font so file-type icons render as text glyphs.
fn install_icon_font(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);
}
