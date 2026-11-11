//! Tools > DISM manager: mount a WIM into a folder (created when missing), see what is mounted
//! (also what was mounted by other tools, or in an earlier session), and commit / unmount it.
//! A badge after the Help menu tells at a glance that an image is mounted.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eframe::egui::{self, Key, RichText};
use egui_phosphor::regular as icon;
use gc_dism::{ImageEntry, Mount, Op, Run};

use crate::theme::Theme;

type Query = (Vec<Mount>, Option<String>);

pub struct DismWindow {
    pub open: bool,
    mounts: Vec<Mount>,
    /// Why Windows could not be asked (the list then comes from our own records).
    note: Option<String>,
    elevated: bool,
    query: Option<Receiver<Query>>,
    last_query: Instant,
    // the "mount an image" form
    wim: String,
    loaded_wim: String,
    images: Result<Vec<ImageEntry>, String>,
    index: u32,
    dir: String,
    read_only: bool,
    // running / last job
    run: Option<Run>,
    last: Option<(String, String, Result<(), String>)>,
    confirm_discard: Option<PathBuf>,
    /// A job waits one frame so the panels can leave the folder first.
    pending: Option<Op>,
    leave: Option<PathBuf>,
    show_panel: Option<PathBuf>,
}

impl Default for DismWindow {
    fn default() -> Self {
        Self {
            open: false,
            mounts: gc_dism::remembered_mounts(),
            note: None,
            elevated: gc_dism::is_elevated(),
            query: None,
            last_query: Instant::now(),
            wim: String::new(),
            loaded_wim: String::new(),
            images: Ok(Vec::new()),
            index: 1,
            dir: String::new(),
            read_only: false,
            run: None,
            last: None,
            confirm_discard: None,
            pending: None,
            leave: None,
            show_panel: None,
        }
    }
}

fn ago(since: u64) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(since);
    let s = now.saturating_sub(since);
    match s {
        0..=89 => "just now".into(),
        90..=5399 => format!("{} min ago", s / 60),
        5400..=172_799 => format!("{} h ago", s / 3600),
        _ => format!("{} days ago", s / 86_400),
    }
}

fn is_image_file(p: &Path) -> bool {
    p.extension().is_some_and(|e| ["wim", "esd", "swm"].iter().any(|x| e.eq_ignore_ascii_case(x)))
}

impl DismWindow {
    /// Asks Windows what is mounted (in the background; takes a second or two).
    pub fn refresh(&mut self, ctx: &egui::Context) {
        if self.query.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(gc_dism::query_mounts());
            ctx.request_repaint();
        });
        self.query = Some(rx);
        self.last_query = Instant::now();
    }

    /// Opens the window; a WIM / ESD under the panel cursor is offered for mounting.
    pub fn open_with(&mut self, ctx: &egui::Context, selected: Option<PathBuf>) {
        self.open = true;
        self.refresh(ctx);
        if let Some(f) = selected.filter(|f| is_image_file(f)) {
            if self.wim.trim().is_empty() {
                self.set_wim(f);
            }
        }
    }

    fn set_wim(&mut self, f: PathBuf) {
        self.wim = f.display().to_string();
        self.dir.clear();
    }

    /// A folder the panels must leave before it can be unmounted.
    pub fn take_leave(&mut self) -> Option<PathBuf> {
        self.leave.take()
    }

    /// A mounted folder the user asked to see in the active panel.
    pub fn take_show(&mut self) -> Option<PathBuf> {
        self.show_panel.take()
    }

    fn start(&mut self, ctx: &egui::Context, op: Op) {
        let ctx = ctx.clone();
        self.last = None;
        self.run = Some(gc_dism::spawn(op, move || ctx.request_repaint()));
    }

    /// Every frame: collects answers and finished jobs, refreshes now and then, starts waiting jobs.
    pub fn update(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.query {
            match rx.try_recv() {
                Ok((mounts, note)) => {
                    self.mounts = mounts;
                    self.note = note;
                    self.query = None;
                }
                Err(TryRecvError::Disconnected) => self.query = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(run) = &self.run {
            let (output, _, done) = run.snapshot();
            if let Some(result) = done {
                self.last = Some((run.op.title(), output, result));
                self.run = None;
                self.refresh(ctx);
            }
        }
        if self.leave.is_none() {
            if let Some(op) = self.pending.take() {
                self.start(ctx, op);
            }
        }
        if self.run.is_none() && self.query.is_none() && self.last_query.elapsed() > Duration::from_secs(300) {
            self.refresh(ctx);
        }
        if self.run.is_some() || self.query.is_some() {
            ctx.request_repaint_after(Duration::from_millis(300));
        }
    }

    /// The indicator after the Help menu. Returns true when it was clicked.
    pub fn badge(&self, ui: &mut egui::Ui, theme: &Theme) -> bool {
        let busy = self.run.as_ref().map(|r| (r.op.title(), r.snapshot().1));
        let text = if let Some((_, pct)) = &busy {
            format!("{} DISM working{}", icon::HARD_DRIVES, pct.map(|p| format!(" {p:.0}%")).unwrap_or_default())
        } else if !self.mounts.is_empty() {
            format!("{} {} WIM mounted", icon::HARD_DRIVES, self.mounts.len())
        } else {
            return false;
        };
        let any_bad = self.mounts.iter().any(|m| !m.is_ok());
        let color = if any_bad { theme.error.0 } else { theme.marked.0 };
        let r = ui.add(egui::Button::new(RichText::new(text).strong().color(color)));
        let r = r.on_hover_ui(|ui| {
            if let Some((title, _)) = &busy {
                ui.label(title);
            }
            for m in &self.mounts {
                ui.label(format!("{}  <-  {} (index {}){}", m.dir.display(), m.wim.display(), m.index, if m.read_only { "  read-only" } else { "" }));
            }
            ui.weak("Click to open the DISM manager (commit / unmount).");
        });
        r.clicked()
    }

    pub fn show(&mut self, ctx: &egui::Context, theme: &Theme, selected: Option<PathBuf>) {
        if !self.open {
            return;
        }
        let mut open = self.open;
        egui::Window::new("DISM manager").open(&mut open).default_size([760.0, 640.0]).show(ctx, |ui| {
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| self.contents(ui, ctx, theme, selected));
        });
        self.open = open && !ctx.input(|i| i.key_pressed(Key::Escape));
    }

    fn contents(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, theme: &Theme, selected: Option<PathBuf>) {
        let busy = self.run.is_some() || self.pending.is_some();
        if !self.elevated {
            ui.colored_label(theme.marked.0, "Ghost Commander is not running as administrator: every DISM job asks for approval (UAC). A mounted Windows image may also be unreadable here; run as administrator to browse it.");
        }
        if let Some(n) = &self.note {
            ui.colored_label(theme.error.0, format!("Could not ask Windows what is mounted ({}). Showing what Ghost Commander remembers.", n.lines().next().unwrap_or("")));
        }

        // ---- mounted images ----
        ui.horizontal(|ui| {
            ui.heading("Mounted images");
            if self.query.is_some() {
                ui.spinner();
            } else if ui.small_button(format!("{} Refresh", icon::ARROWS_CLOCKWISE)).clicked() {
                self.refresh(ctx);
            }
        });
        if self.mounts.is_empty() {
            ui.weak("Nothing is mounted.");
        }
        let mut job: Option<Op> = None;
        let mut leave_first = false;
        for m in self.mounts.clone() {
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(m.dir.display().to_string()).strong().monospace());
                    let status = if m.is_ok() { RichText::new("OK").color(theme.dir.0) } else { RichText::new(m.status.clone()).color(theme.error.0) };
                    ui.label(status);
                });
                let mut line = format!("{}  (index {})  {}", m.wim.display(), m.index, if m.read_only { "read-only" } else { "read/write" });
                if let Some(s) = m.since {
                    line += &format!("  -  mounted {}", ago(s));
                }
                ui.label(line);
                if m.needs_remount() {
                    ui.colored_label(theme.error.0, "The mount was interrupted (reboot?). Remount it to use it again.");
                }
                ui.horizontal_wrapped(|ui| {
                    if ui.button(format!("{} Show in panel", icon::FOLDER_OPEN)).clicked() {
                        self.show_panel = Some(m.dir.clone());
                    }
                    if m.needs_remount() && ui.add_enabled(!busy, egui::Button::new(format!("{} Remount", icon::ARROWS_CLOCKWISE))).clicked() {
                        job = Some(Op::Remount { dir: m.dir.clone() });
                    }
                    let can_save = !busy && !m.read_only && m.is_ok();
                    if ui.add_enabled(can_save, egui::Button::new(format!("{} Commit (keep mounted)", icon::FLOPPY_DISK))).on_hover_text("Save the changes made in the folder into the WIM and stay mounted").clicked() {
                        job = Some(Op::Commit { dir: m.dir.clone() });
                    }
                    if ui.add_enabled(can_save, egui::Button::new(format!("{} Commit and unmount", icon::EJECT))).on_hover_text("Save the changes, then unmount").clicked() {
                        job = Some(Op::UnmountCommit { dir: m.dir.clone() });
                        leave_first = true;
                    }
                    if self.confirm_discard.as_deref() == Some(m.dir.as_path()) {
                        ui.colored_label(theme.error.0, "Throw the changes away?");
                        if ui.add_enabled(!busy, egui::Button::new("Yes, discard")).clicked() {
                            job = Some(Op::UnmountDiscard { dir: m.dir.clone() });
                            leave_first = true;
                            self.confirm_discard = None;
                        }
                        if ui.button("No").clicked() {
                            self.confirm_discard = None;
                        }
                    } else if ui.add_enabled(!busy, egui::Button::new(format!("{} Discard and unmount", icon::TRASH))).on_hover_text("Unmount and drop every change made in the folder").clicked() {
                        self.confirm_discard = Some(m.dir.clone());
                    }
                });
            });
        }
        if let Some(op) = job {
            if leave_first {
                self.leave = op.dir().map(Path::to_path_buf);
            }
            self.pending = Some(op);
        }

        // ---- mount an image ----
        ui.add_space(8.0);
        ui.heading("Mount an image");
        ui.horizontal(|ui| {
            ui.label("Image file:");
            ui.add(egui::TextEdit::singleline(&mut self.wim).desired_width(ui.available_width() - 190.0).hint_text("C:\\images\\boot.wim"));
            let under_cursor = selected.filter(|f| is_image_file(f));
            if ui.add_enabled(under_cursor.is_some(), egui::Button::new("Use file under cursor")).clicked() {
                if let Some(f) = under_cursor {
                    self.set_wim(f);
                }
            }
        });
        let wim_path = PathBuf::from(self.wim.trim().trim_matches('"'));
        if self.wim != self.loaded_wim {
            self.loaded_wim = self.wim.clone();
            self.images = if wim_path.as_os_str().is_empty() {
                Ok(Vec::new())
            } else if wim_path.is_file() {
                gc_dism::wim_images(&wim_path)
            } else {
                Err("That file does not exist.".into())
            };
            if let Ok(list) = &self.images {
                self.index = list.first().map_or(1, |i| i.index);
                if self.dir.trim().is_empty() {
                    if let Some(parent) = wim_path.parent().filter(|p| !p.as_os_str().is_empty()) {
                        self.dir = parent.join("mount").display().to_string();
                    }
                }
            }
        }
        match &self.images {
            Err(e) => {
                ui.colored_label(theme.error.0, e);
            }
            Ok(list) if !list.is_empty() => {
                let shown = list.iter().find(|i| i.index == self.index).map(|i| format!("{} - {}", i.index, i.name)).unwrap_or_else(|| format!("{}", self.index));
                ui.horizontal(|ui| {
                    ui.label("Image:");
                    egui::ComboBox::from_id_salt("dism_index").selected_text(shown).width(420.0).show_ui(ui, |ui| {
                        for i in list {
                            let size = if i.bytes > 0 { format!("  ({})", crate::panel::fmt_size(i.bytes)) } else { String::new() };
                            ui.selectable_value(&mut self.index, i.index, format!("{} - {}{}", i.index, i.name, size));
                        }
                    });
                });
                if let Some(d) = list.iter().find(|i| i.index == self.index).map(|i| i.description.as_str()).filter(|d| !d.is_empty()) {
                    ui.weak(d);
                }
            }
            Ok(_) => {}
        }
        ui.horizontal(|ui| {
            ui.label("Mount folder:");
            ui.add(egui::TextEdit::singleline(&mut self.dir).desired_width(ui.available_width() - 10.0).hint_text("created when it does not exist"));
        });
        ui.checkbox(&mut self.read_only, "Read-only (nothing can be committed)");
        let dir = PathBuf::from(self.dir.trim().trim_matches('"'));
        let problem = if !matches!(&self.images, Ok(l) if !l.is_empty()) {
            Some("Choose an image file first.".to_string())
        } else {
            gc_dism::mount_dir_problem(&dir, &self.mounts)
        };
        ui.horizontal(|ui| {
            let go = ui.add_enabled(problem.is_none() && !busy, egui::Button::new(format!("{} Mount", icon::HARD_DRIVES)));
            if let Some(p) = &problem {
                if !self.wim.trim().is_empty() {
                    ui.colored_label(theme.error.0, p);
                }
            } else if !dir.is_dir() {
                ui.weak("The folder will be created.");
            }
            if go.clicked() {
                self.pending = Some(Op::Mount { wim: wim_path.clone(), index: self.index, dir: dir.clone(), read_only: self.read_only });
            }
        });

        // ---- progress / log ----
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.heading("Last job");
            if ui.add_enabled(!busy, egui::Button::new("Clean up orphaned mounts")).on_hover_text("dism /Cleanup-Wim: forgets mounts whose folder or image is gone").clicked() {
                self.pending = Some(Op::Cleanup);
            }
        });
        if let Some(run) = &self.run {
            let (output, pct, _) = run.snapshot();
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(run.op.title());
            });
            if !self.elevated && output.is_empty() {
                ui.weak("Waiting for the administrator approval...");
            }
            ui.add(egui::ProgressBar::new(pct.unwrap_or(0.0) / 100.0).show_percentage().animate(true));
            log_box(ui, &output);
        } else if let Some((title, output, result)) = &self.last {
            match result {
                Ok(()) => ui.colored_label(theme.dir.0, format!("Done: {title}")),
                Err(e) => ui.colored_label(theme.error.0, format!("Failed: {title}\n{e}")),
            };
            log_box(ui, output);
        } else {
            ui.weak("Nothing run yet in this session.");
        }
    }
}

fn log_box(ui: &mut egui::Ui, text: &str) {
    egui::ScrollArea::vertical().max_height(180.0).stick_to_bottom(true).id_salt("dism_log").show(ui, |ui| {
        ui.add(egui::Label::new(RichText::new(text).monospace().size(12.0)).selectable(true));
    });
}
