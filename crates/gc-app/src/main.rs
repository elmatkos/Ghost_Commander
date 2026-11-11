// GUI subsystem: no console window next to the app (also in debug builds).
#![windows_subsystem = "windows"]

mod about;
mod checksums_window;
mod drive_bar;
mod ftp_window;
mod help_window;
mod options;
mod image_viewer;
mod log;
mod panel;
mod shell_menu;
mod sysinfo_window;
mod theme;
mod theme_editor;
mod verify_window;
mod viewer;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use eframe::egui::{self, Key};
use gc_core::jobs::{self, Job, Op, Resolution};
use about::About;
use ftp_window::FtpWindow;
use help_window::HelpWindow;
use gc_archive::{ArchiveIndex, PackFormat};
use panel::{FileUse, Panel, PanelAction, SortKey};
use options::Options;
use checksums_window::ChecksumsWindow;
use verify_window::VerifyWindow;
use sysinfo_window::SysInfoWindow;
use theme_editor::ThemeManager;
use image_viewer::ImageViewer;
use viewer::Viewer;

enum Dialog {
    Confirm(Op),
    MkDir(String),
    /// Shift+F4: name of a new empty file to create and edit.
    NewFile(String),
    /// Drive picker for panel `panel`.
    Drives { panel: usize, list: Vec<PathBuf>, sel: usize },
    /// Extract inner paths of an archive into a folder.
    Extract { index: Arc<ArchiveIndex>, names: Vec<String>, dest: PathBuf },
    /// F5 / F6 on a server panel: download `items` into the local folder `dest`.
    FtpDownload { conn: gc_ftp::SharedConn, items: Vec<gc_ftp::RemoteItem>, dest: PathBuf, delete_source: bool },
    /// F5 / F6 into a server panel: upload local `srcs` into the remote folder.
    FtpUpload { conn: gc_ftp::SharedConn, srcs: Vec<PathBuf>, folder: String, delete_source: bool },
    /// F8 on a server panel.
    FtpDelete { conn: gc_ftp::SharedConn, items: Vec<gc_ftp::RemoteItem> },
    /// Pack items into a new archive `name.ext` in `dest_dir`.
    Pack { srcs: Vec<PathBuf>, dest_dir: String, name: String, format: PackFormat },
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
    help: HelpWindow,
    themes: ThemeManager,
    options: Options,
    sysinfo: SysInfoWindow,
    checksums: ChecksumsWindow,
    verify: VerifyWindow,
    ftp_win: FtpWindow,
    /// Folders last written to settings (to notice a change and save the new ones).
    saved_dirs: [PathBuf; 2],
    /// Drive list with free space, refreshed by a background thread (a dead network drive can block).
    drives: Arc<std::sync::Mutex<Vec<gc_core::DriveInfo>>>,
    viewer: Viewer,
    images: ImageViewer,
    /// Archive files being extracted for F3 / F4 / Enter, with what to do when ready.
    extractions: Vec<(std::sync::mpsc::Receiver<std::io::Result<PathBuf>>, FileUse)>,
}

static FIRST_FRAME_DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_icon_font(&cc.egui_ctx);
        let start = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        // Open the folders the panels showed last time (when they still exist).
        let saved = theme::load_settings();
        let remembered = |d: &Option<String>| d.as_ref().map(PathBuf::from).filter(|p| p.is_dir()).unwrap_or_else(|| start.clone());
        let (left, right) = (remembered(&saved.left_dir), remembered(&saved.right_dir));
        let mut panels = [Panel::new(left.clone(), cc.egui_ctx.clone()), Panel::new(right.clone(), cc.egui_ctx.clone())];
        // `ghost-commander ftp://user:pass@host/dir` connects the left panel right away.
        if let Some(link) = std::env::args().nth(1).filter(|a| a.to_ascii_lowercase().starts_with("ftp")) {
            if let Ok(target) = gc_ftp::Target::from_url(&link) {
                panels[0].connect_ftp(target);
            }
        }
        Self {
            panels,
            saved_dirs: [left, right],
            ftp_win: FtpWindow::default(),
            active: 0,
            dialog: None,
            jobs: Vec::new(),
            status: None,
            apply_all: false,
            about: About::default(),
            help: HelpWindow::default(),
            themes: ThemeManager::load(&cc.egui_ctx),
            options: Options::load(),
            sysinfo: SysInfoWindow::default(),
            checksums: ChecksumsWindow::default(),
            verify: VerifyWindow::default(),
            drives: watch_drives(&cc.egui_ctx),
            viewer: Viewer::default(),
            images: ImageViewer::default(),
            extractions: Vec::new(),
        }
    }

    /// F3: built-in viewer. F4: external editor.
    fn view_or_edit(&mut self, ctx: &egui::Context, edit: bool) {
        let mode = if edit { FileUse::Edit } else { FileUse::View };
        if let Some((index, inner)) = self.panels[self.active].archive_file() {
            return self.extract_then_use(ctx, index, inner, mode);
        }
        if let Some((conn, remote)) = self.panels[self.active].ftp_file() {
            return self.ftp_then_use(ctx, conn, remote, mode);
        }
        let Some(file) = self.panels[self.active].current_file() else {
            self.status = Some("Select a file first (folders cannot be viewed).".into());
            return;
        };
        let siblings = self.panels[self.active].image_files();
        self.use_file(ctx, file, mode, siblings);
    }

    /// Views, edits or opens a file that is on disk.
    fn use_file(&mut self, ctx: &egui::Context, file: PathBuf, mode: FileUse, siblings: Vec<PathBuf>) {
        self.status = None;
        let result = match mode {
            FileUse::Edit => {
                #[cfg(windows)]
                let editor = "notepad";
                #[cfg(not(windows))]
                let editor = "xdg-open";
                std::process::Command::new(editor).arg(&file).spawn().map(|_| ())
            }
            FileUse::View if image_viewer::is_image(&file) => {
                self.images.open(ctx, &file, siblings);
                Ok(())
            }
            FileUse::View => self.viewer.open(&file),
            FileUse::Open => shell_menu::open(&file).map_err(std::io::Error::other),
        };
        if let Err(e) = result {
            self.status = Some(format!("{}: {e}", file.display()));
        }
    }

    /// File inside an archive: extract it to the scratch folder on a worker thread, then
    /// `use_file` it (see `poll_extractions`).
    fn extract_then_use(&mut self, ctx: &egui::Context, index: Arc<ArchiveIndex>, inner: String, mode: FileUse) {
        self.status = Some(format!("Extracting {inner} ..."));
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(gc_archive::extract_to_scratch(&index, &inner));
            ctx.request_repaint();
        });
        self.extractions.push((rx, mode));
    }

    /// File on an FTP server: download it to the scratch folder on a worker thread, then `use_file` it.
    fn ftp_then_use(&mut self, ctx: &egui::Context, conn: gc_ftp::SharedConn, remote: String, mode: FileUse) {
        self.status = Some(format!("Downloading {remote} ..."));
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(gc_ftp::download_to_scratch(&conn, &remote));
            ctx.request_repaint();
        });
        self.extractions.push((rx, mode));
    }

    fn poll_extractions(&mut self, ctx: &egui::Context) {
        let mut finished = Vec::new();
        self.extractions.retain(|(rx, mode)| match rx.try_recv() {
            Err(std::sync::mpsc::TryRecvError::Empty) => true,
            Ok(result) => {
                finished.push((result, *mode));
                false
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => false,
        });
        for (result, mode) in finished {
            match result {
                Ok(file) => {
                    let from_server = file.starts_with(gc_ftp::scratch_dir());
                    self.use_file(ctx, file.clone(), mode, vec![file]);
                    if from_server && mode == FileUse::Edit {
                        self.status = Some("Editing a local copy of the server file: changes are not uploaded (save it, then F5 into the server panel).".into());
                    }
                }
                Err(e) => self.status = Some(format!("Extract failed: {e}")),
            }
        }
    }

    /// Remembers the folders of both panels for the next start.
    fn save_dirs_if_changed(&mut self) {
        let dirs = [self.panels[0].restore_dir(), self.panels[1].restore_dir()];
        if dirs != self.saved_dirs {
            let mut s = theme::load_settings();
            s.left_dir = Some(dirs[0].display().to_string());
            s.right_dir = Some(dirs[1].display().to_string());
            let _ = theme::save_settings(&s);
            self.saved_dirs = dirs;
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

        // A server panel: F5 / F6 download, F7 makes a folder, F8 deletes on the server.
        if let Some((conn, items)) = self.panels[self.active].ftp_selection() {
            self.dialog = None;
            match key {
                Key::F5 | Key::F6 => match dest_dir {
                    Some(dest) if !items.is_empty() => self.dialog = Some(Dialog::FtpDownload { conn, items, dest, delete_source: key == Key::F6 }),
                    Some(_) => {}
                    None => self.status = Some("Download into a normal folder: the other panel does not show one.".into()),
                },
                Key::F7 => self.dialog = Some(Dialog::MkDir(String::new())),
                Key::F8 if !items.is_empty() => self.dialog = Some(Dialog::FtpDelete { conn, items }),
                _ => {}
            }
            return;
        }

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

        // A server in the other panel: F5 / F6 upload.
        if matches!(key, Key::F5 | Key::F6) {
            if let Some((conn, folder)) = self.panels[other].ftp_folder() {
                let srcs = self.panels[self.active].targets();
                if !srcs.is_empty() {
                    self.dialog = Some(Dialog::FtpUpload { conn, srcs, folder, delete_source: key == Key::F6 });
                }
                return;
            }
        }

        let srcs = self.panels[self.active].targets();
        self.dialog = match (key, dest_dir) {
            (Key::F5, Some(dest_dir)) if !srcs.is_empty() => Some(Dialog::Confirm(Op::Copy { srcs, dest_dir })),
            (Key::F6, Some(dest_dir)) if !srcs.is_empty() => Some(Dialog::Confirm(Op::Move { srcs, dest_dir })),
            (Key::F5 | Key::F6, None) => {
                self.status = Some("Archives are read-only here; Alt+F5 packs into a new archive.".into());
                None
            }
            (Key::F8, _) if !srcs.is_empty() => Some(Dialog::Confirm(Op::Delete { srcs, permanent: shift })),
            (Key::F7, _) => Some(Dialog::MkDir(String::new())),
            _ => None,
        };
    }

    /// Alt+F5: pack the selection into a new archive in the other panel's folder (or this one's).
    fn open_pack_dialog(&mut self) {
        let panel = &self.panels[self.active];
        let srcs = panel.targets();
        let dest_dir = self.panels[1 - self.active].disk_folder().or_else(|| panel.disk_folder()).map(Path::to_path_buf);
        match dest_dir {
            Some(dest_dir) if panel.disk_folder().is_some() && !srcs.is_empty() => {
                self.dialog = Some(Dialog::Pack { srcs, dest_dir: dest_dir.display().to_string(), name: panel.pack_name(), format: PackFormat::SevenZip });
            }
            _ => self.status = Some("Select files or folders on disk to pack.".into()),
        }
    }

    /// Ctrl+H: checksums of the marked files (or the one under the cursor). Folders are skipped.
    fn open_checksums(&mut self, ctx: &egui::Context) {
        let panel = &self.panels[self.active];
        let files: Vec<PathBuf> = if panel.disk_folder().is_some() { panel.targets().into_iter().filter(|p| p.is_file()).collect() } else { Vec::new() };
        if files.is_empty() {
            self.status = Some("Select files on disk to checksum.".into());
        } else {
            self.checksums.start(ctx, files);
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        let pressed = |ctx: &egui::Context, k: Key| ctx.input(|i| i.key_pressed(k));
        let ctrl = ctx.input(|i| i.modifiers.ctrl);
        // While the quick filter box is open, typing belongs to it: only these keys are ours.
        if self.panels[self.active].filter_open() {
            let panel = &mut self.panels[self.active];
            if pressed(ctx, Key::Escape) {
                panel.close_filter();
            } else if pressed(ctx, Key::Enter) {
                panel.finish_filter();
            }
            if pressed(ctx, Key::ArrowDown) {
                panel.move_cursor(1);
            }
            if pressed(ctx, Key::ArrowUp) {
                panel.move_cursor(-1);
            }
            return;
        }
        if ctrl {
            let sort = [(Key::F3, SortKey::Name), (Key::F4, SortKey::Ext), (Key::F5, SortKey::Date), (Key::F6, SortKey::Size)];
            for (key, by) in sort {
                if pressed(ctx, key) {
                    self.panels[self.active].set_sort(by);
                }
            }
            if pressed(ctx, Key::F) {
                self.panels[self.active].open_filter();
            }
            if pressed(ctx, Key::R) {
                self.reload_all();
            }
            if pressed(ctx, Key::H) {
                self.open_checksums(ctx);
            }
            if pressed(ctx, Key::N) {
                self.ftp_win.open();
            }
        }
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
        // Consume the key: the window it opens also closes on the same key and must not see this press.
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::F1)) {
            self.help.open = true;
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::F9)) {
            self.sysinfo.start(ctx);
        }
        let shift = ctx.input(|i| i.modifiers.shift);
        let alt = ctx.input(|i| i.modifiers.alt);
        for key in [Key::F5, Key::F6, Key::F7, Key::F8] {
            if pressed(ctx, key) && !ctrl {
                if alt && key == Key::F5 {
                    self.open_pack_dialog();
                } else {
                    self.open_dialog_for(key, shift);
                }
            }
        }
        // Del / Shift+Del behave like F8 / Shift+F8. egui turns Shift+Del into a Cut event
        // on Windows and never reports the key press, so look for the event too.
        // (Ctrl+X is a Cut event as well, hence the modifier check.)
        let cut = ctx.input(|i| i.modifiers.shift && !i.modifiers.command && i.events.iter().any(|e| matches!(e, egui::Event::Cut)));
        if pressed(ctx, Key::Delete) || cut {
            self.open_dialog_for(Key::F8, shift || cut);
        }
        if pressed(ctx, Key::F3) && !ctrl {
            self.view_or_edit(ctx, false);
        }
        if pressed(ctx, Key::F4) && !ctrl {
            if shift {
                if self.panels[self.active].disk_folder().is_some() {
                    self.dialog = Some(Dialog::NewFile("New file.txt".into()));
                } else {
                    self.status = Some("Archives are read-only.".into());
                }
            } else {
                self.view_or_edit(ctx, true);
            }
        }
        // Ctrl+1/2 and (as in Total Commander) Alt+F1/F2: drive picker for the left / right panel.
        for (key, alt_key, panel) in [(Key::Num1, Key::F1, 0), (Key::Num2, Key::F2, 1)] {
            if (ctrl && pressed(ctx, key)) || (alt && !ctrl && pressed(ctx, alt_key)) {
                let list = gc_core::drives();
                let sel = list.iter().position(|d| self.panels[panel].cwd.starts_with(d)).unwrap_or(0);
                self.dialog = Some(Dialog::Drives { panel, list, sel });
            }
        }
    }

    /// Right-click requests raised by the panels while drawing.
    fn handle_panel_actions(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
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
                Some(PanelAction::Open(path)) => {
                    self.active = i;
                    if gc_core::hash::is_checksum_file(&path) {
                        self.verify.start(ctx, &path);
                    } else {
                        self.status = shell_menu::open(&path).err().map(|e| format!("{}: {e}", path.display()));
                    }
                }
                Some(PanelAction::ArchiveFile { index, inner, mode }) => {
                    self.active = i;
                    self.extract_then_use(ctx, index, inner, mode);
                }
                Some(PanelAction::FtpFile { conn, remote, mode }) => {
                    self.active = i;
                    self.ftp_then_use(ctx, conn, remote, mode);
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
        let mut newfile: Option<String> = None;
        let mut spawned: Option<Job> = None;

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
                    spawned = Some(gc_archive::spawn_extract(index, names, dest));
                } else if !cancel {
                    keep = Some(Dialog::Extract { index, names, dest });
                }
            }
            Dialog::FtpDownload { conn, items, dest, delete_source } => {
                let verb = if delete_source { "Move" } else { "Copy" };
                let mut ok = enter;
                let mut cancel = escape;
                egui::Window::new(format!("{verb} from server")).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
                    ui.label(format!("{verb} {} item(s) from the server to\n{}", items.len(), dest.display()));
                    if delete_source {
                        ui.weak("The server copies are deleted once everything arrived.");
                    }
                    ui.horizontal(|ui| {
                        ok |= ui.button("OK (Enter)").clicked();
                        cancel |= ui.button("Cancel (Esc)").clicked();
                    });
                });
                if ok {
                    let label = format!("{verb} {} item(s) from server", items.len());
                    spawned = Some(jobs::spawn_with(label, move |p| gc_ftp::download(&conn, &items, &dest, delete_source, p)));
                } else if !cancel {
                    keep = Some(Dialog::FtpDownload { conn, items, dest, delete_source });
                }
            }
            Dialog::FtpUpload { conn, srcs, folder, delete_source } => {
                let verb = if delete_source { "Move" } else { "Copy" };
                let mut ok = enter;
                let mut cancel = escape;
                egui::Window::new(format!("{verb} to server")).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
                    ui.label(format!("{verb} {} item(s) to the server folder\n{folder}", srcs.len()));
                    if delete_source {
                        ui.weak("The local copies are deleted once everything is uploaded.");
                    }
                    ui.horizontal(|ui| {
                        ok |= ui.button("OK (Enter)").clicked();
                        cancel |= ui.button("Cancel (Esc)").clicked();
                    });
                });
                if ok {
                    let label = format!("{verb} {} item(s) to server", srcs.len());
                    spawned = Some(jobs::spawn_with(label, move |p| gc_ftp::upload(&conn, &srcs, &folder, delete_source, p)));
                } else if !cancel {
                    keep = Some(Dialog::FtpUpload { conn, srcs, folder, delete_source });
                }
            }
            Dialog::FtpDelete { conn, items } => {
                let mut ok = enter;
                let mut cancel = escape;
                egui::Window::new("Delete on server").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
                    ui.label(format!("Delete {} item(s) on the server?\nFolders are deleted with everything in them. This cannot be undone.", items.len()));
                    ui.horizontal(|ui| {
                        ok |= ui.button("OK (Enter)").clicked();
                        cancel |= ui.button("Cancel (Esc)").clicked();
                    });
                });
                if ok {
                    let label = format!("Delete {} item(s) on server", items.len());
                    spawned = Some(jobs::spawn_with(label, move |p| gc_ftp::delete(&conn, &items, p)));
                } else if !cancel {
                    keep = Some(Dialog::FtpDelete { conn, items });
                }
            }
            Dialog::Pack { srcs, mut dest_dir, mut name, mut format } => {
                let folder = PathBuf::from(dest_dir.trim());
                let target = folder.join(format!("{}.{}", name.trim(), format.extension()));
                let problem = if name.trim().is_empty() || name.contains(['\\', '/', ':']) {
                    Some("Enter a plain file name.")
                } else if !folder.is_dir() {
                    Some("That folder does not exist.")
                } else if target.exists() {
                    Some("That archive already exists.")
                } else {
                    None
                };
                let mut ok = enter && problem.is_none();
                let mut cancel = escape;
                egui::Window::new("Pack").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
                    ui.label(format!("Pack {} item(s) into folder", srcs.len()));
                    ui.add(egui::TextEdit::singleline(&mut dest_dir).desired_width(480.0));
                    ui.label("as archive");
                    let name_edit = ui.add(egui::TextEdit::singleline(&mut name).desired_width(480.0));
                    // The name field starts focused; clicking the folder field moves the focus away for good.
                    if ctx.memory(|m| m.focused().is_none()) {
                        name_edit.request_focus();
                    }
                    ui.horizontal(|ui| {
                        for f in PackFormat::ALL {
                            ui.radio_value(&mut format, f, f.extension());
                        }
                    });
                    ui.label(target.display().to_string());
                    if let Some(p) = problem {
                        ui.colored_label(self.themes.active.error.0, p);
                    }
                    ui.horizontal(|ui| {
                        ok |= ui.add_enabled(problem.is_none(), egui::Button::new("Pack (Enter)")).clicked();
                        cancel |= ui.button("Cancel (Esc)").clicked();
                    });
                });
                if ok {
                    spawned = Some(gc_archive::spawn_pack(format, srcs, target));
                } else if !cancel {
                    keep = Some(Dialog::Pack { srcs, dest_dir, name, format });
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
            Dialog::NewFile(mut name) => {
                let mut ok = enter;
                let mut cancel = escape;
                egui::Window::new("New file").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
                    ui.add(egui::TextEdit::singleline(&mut name)).request_focus();
                    ui.horizontal(|ui| {
                        ok |= ui.button("Create and edit (Enter)").clicked();
                        cancel |= ui.button("Cancel (Esc)").clicked();
                    });
                });
                if ok {
                    newfile = Some(name);
                } else if !cancel {
                    keep = Some(Dialog::NewFile(name));
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
        if let Some(job) = spawned {
            self.jobs.push(RunningJob { job, refreshed: false });
        }
        if let Some(name) = newfile {
            let made = match self.panels[self.active].disk_folder() {
                Some(dir) => jobs::make_file(dir, &name),
                None => Err(std::io::Error::other("archives are read-only")),
            };
            match made {
                Ok(path) => {
                    self.status = None;
                    self.reload_all();
                    self.panels[self.active].select_path(&path);
                    self.use_file(ctx, path, FileUse::Edit, Vec::new());
                }
                Err(e) => self.status = Some(format!("Create file failed: {e}")),
            }
        }
        let ftp_mkdir = if self.panels[self.active].is_ftp() { mkdir.take() } else { None };
        if let Some(name) = ftp_mkdir {
            let name = name.trim().to_owned();
            if let Some((conn, folder)) = self.panels[self.active].ftp_folder().filter(|_| !name.is_empty() && !name.contains(['/', '\\'])) {
                self.status = None;
                self.panels[self.active].select_after_reload(&name);
                let remote = gc_ftp::join(&folder, &name);
                self.jobs.push(RunningJob {
                    job: jobs::spawn_with("Create folder on server".into(), move |p| {
                        if let Err(e) = gc_ftp::mkdir(&conn, &remote) {
                            p.fail(format!("{remote}: {e}"));
                        }
                    }),
                    refreshed: false,
                });
            } else {
                self.status = Some("Enter a plain folder name (one level at a time on a server).".into());
            }
        }
        if let Some(name) = mkdir {
            let made = match self.panels[self.active].disk_folder() {
                Some(dir) => jobs::make_dir(dir, &name),
                None => Err(std::io::Error::other("archives are read-only")),
            };
            match made {
                Ok(path) => {
                    self.status = None;
                    self.reload_all();
                    // "a\b" creates nested folders: the cursor goes to the top one that shows in this panel.
                    let panel = &mut self.panels[self.active];
                    panel.select_path(&path);
                    if let Some(top) = panel.disk_folder().and_then(|cwd| path.strip_prefix(cwd).ok()).and_then(|rel| rel.components().next()) {
                        let top = panel.disk_folder().map(|cwd| cwd.join(top));
                        if let Some(top) = top {
                            panel.select_path(&top);
                        }
                    }
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
        log::log("on_exit called");
        gc_archive::cleanup_nested();
        gc_ftp::cleanup_scratch();
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        log::frame_edge(true);
        self.update_inner(ctx, frame);
        log::frame_edge(false);
    }
}

impl App {
    fn update_inner(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        static FIRST_FRAME: std::sync::Once = std::sync::Once::new();
        FIRST_FRAME.call_once(|| log::log("first frame"));
        let first = !FIRST_FRAME_DONE.swap(true, Ordering::Relaxed);
        let step = |what: &str| if first { log::log(&format!("frame 1: {what}")) };
        let dialog_open = self.dialog.is_some()
            || self.about.open
            || self.help.open
            || self.themes.open
            || self.options.open
            || self.sysinfo.open
            || self.checksums.open
            || self.verify.open
            || self.ftp_win.open
            || self.viewer.is_open()
            || self.jobs.iter().any(|j| j.job.progress.pending_conflict().is_some());
        if !dialog_open {
            self.handle_keys(ctx);
            // Tab switches panels here. egui also uses it to move the keyboard focus to a button,
            // where Enter or Space would then press it: keep nothing focused.
            if !self.panels.iter().any(|p| p.filter_open()) {
                ctx.memory_mut(|m| {
                    if let Some(id) = m.focused() {
                        m.surrender_focus(id);
                    }
                });
            }
        }
        step("keys handled");
        self.poll_jobs();
        step("jobs polled");
        // Notice files created or removed outside the app (or by a tool such as Save .crc).
        for p in &mut self.panels {
            p.refresh_if_changed();
        }
        step("panels refreshed");
        ctx.request_repaint_after(Duration::from_secs(1));

        egui::TopBottomPanel::top("menu").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("Config", |ui| {
                    if ui.button("Options...").clicked() {
                        self.options.open = true;
                        ui.close_menu();
                    }
                    if ui.button("Themes...").clicked() {
                        self.themes.open = true;
                        ui.close_menu();
                    }
                });
                ui.menu_button("Tools", |ui| {
                    if ui.button("Checksums...  (Ctrl+H)").clicked() {
                        self.open_checksums(ctx);
                        ui.close_menu();
                    }
                });
                ui.menu_button("Net", |ui| {
                    if ui.button("FTP connect...  (Ctrl+N)").clicked() {
                        self.ftp_win.open();
                        ui.close_menu();
                    }
                    if ui.add_enabled(self.panels[self.active].is_ftp(), egui::Button::new("Disconnect active panel")).clicked() {
                        self.panels[self.active].leave_ftp();
                        ui.close_menu();
                    }
                });
                ui.menu_button("Info", |ui| {
                    if ui.button("System Info...  (F9)").clicked() {
                        self.sysinfo.start(ctx);
                        ui.close_menu();
                    }
                });
                ui.menu_button("Help", |ui| {
                    if ui.button("Keyboard shortcuts  (F1)").clicked() {
                        self.help.open = true;
                        ui.close_menu();
                    }
                    if ui.button("About").clicked() {
                        self.about.open = true;
                        ui.close_menu();
                    }
                });
            });
        });

        egui::TopBottomPanel::bottom("bottom").show(ctx, |ui| {
            if let Some(s) = &self.status {
                ui.colored_label(self.themes.active.error.0, s);
            }
            self.show_jobs(ui);
            show_hints(ui);
        });

        step("menu and bottom bar drawn");
        let mut clicked: Option<usize> = None;
        let mut bar_click: Option<(usize, drive_bar::Click)> = None;
        let drives = self.drives.lock().map(|d| d.clone()).unwrap_or_default();
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.columns(2, |cols| {
                for (i, col) in cols.iter_mut().enumerate() {
                    if self.options.drive_buttons {
                        if let Some(c) = drive_bar::show(col, &drives, &self.panels[i].cwd, &self.themes.active, self.options.drive_free_bar) {
                            bar_click = Some((i, c));
                        }
                        col.add_space(2.0);
                    }
                    if self.panels[i].ui(col, &format!("panel{i}"), self.active == i, &self.themes.active) {
                        clicked = Some(i);
                    }
                }
            });
        });
        step("panels drawn");
        if let Some(i) = clicked {
            self.active = i;
        }
        if let Some((i, click)) = bar_click {
            self.active = i;
            match click {
                drive_bar::Click::Drive(root) => self.panels[i].open_root(root),
                drive_bar::Click::Root => {
                    if let Some(root) = self.panels[i].cwd.ancestors().last().map(Path::to_path_buf) {
                        self.panels[i].open_root(root);
                    }
                }
                drive_bar::Click::Up => self.panels[i].go_up(),
            }
        }
        self.handle_panel_actions(ctx, frame);
        self.poll_extractions(ctx);

        step("panel actions handled");
        self.show_dialog(ctx);
        self.show_conflict(ctx);
        self.about.show(ctx);
        self.help.show(ctx, &self.themes.active);
        self.themes.show(ctx);
        self.options.show(ctx);
        self.sysinfo.show(ctx, &self.themes.active);
        self.checksums.show(ctx, &self.themes.active);
        self.verify.show(ctx, &self.themes.active);
        if let Some(target) = self.ftp_win.show(ctx) {
            self.status = None;
            self.panels[self.active].connect_ftp(target);
        }
        self.save_dirs_if_changed();
        self.viewer.show(ctx);
        if let Some(err) = self.images.show(ctx) {
            self.status = Some(err);
        }
        if let Some(path) = self.images.take_moved() {
            self.panels[self.active].select_path(&path);
        }

        if self.jobs.iter().any(|j| !j.job.progress.finished.load(Ordering::Acquire)) {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        step("update finished");
    }
}

/// Shortcut hints for the bottom line, grouped by the modifier keys that must be held.
/// (ctrl, shift, alt, [(key, label)]). The group matching the keys held right now is shown.
type Hint = (&'static str, &'static str);
const HINTS: [(bool, bool, bool, &[Hint]); 4] = [
    (
        false,
        false,
        false,
        &[
            ("F1", "Help"),
            ("F3", "View"),
            ("F4", "Edit"),
            ("F5", "Copy"),
            ("F6", "Move"),
            ("F7", "MkDir"),
            ("F8/Del", "Recycle"),
            ("F9", "System Info"),
            ("Tab", "Switch"),
            ("Space", "Mark"),
            ("Bksp", "Up"),
        ],
    ),
    (false, true, false, &[("Shift+F4", "New file"), ("Shift+F8/Del", "Delete")]),
    (
        true,
        false,
        false,
        &[
            ("Ctrl+R", "Refresh"),
            ("Ctrl+F", "Filter"),
            ("Ctrl+H", "Checksums"),
            ("Ctrl+1/2", "Drive"),
            ("Ctrl+N", "FTP"),
            ("Ctrl+F3", "Sort name"),
            ("Ctrl+F4", "Sort ext"),
            ("Ctrl+F5", "Sort date"),
            ("Ctrl+F6", "Sort size"),
        ],
    ),
    (false, false, true, &[("Alt+F1", "Left drive"), ("Alt+F2", "Right drive"), ("Alt+F5", "Pack")]),
];

/// Draws the hints for the held modifiers, as many as fit into the width of `ui`.
fn show_hints(ui: &mut egui::Ui) {
    let m = ui.input(|i| i.modifiers);
    let Some((_, _, _, items)) = HINTS.iter().find(|(c, s, a, _)| (*c, *s, *a) == (m.ctrl, m.shift, m.alt)) else { return };
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let color = ui.visuals().text_color();
    let gap = 12.0;
    let mut left = ui.available_width();
    ui.horizontal(|ui| {
        for (key, label) in *items {
            let text = format!("{key} {label}");
            let w = ui.fonts(|f| f.layout_no_wrap(text.clone(), font.clone(), color).size().x);
            if w > left {
                break;
            }
            ui.monospace(text);
            ui.add_space(gap);
            left -= w + gap + ui.spacing().item_spacing.x;
        }
    });
}

fn main() -> eframe::Result<()> {
    log::init();
    log::start_watchdog();
    // WinPE package: a Mesa opengl32.dll (software OpenGL, there is no GPU driver) sits beside the exe.
    // Keep it on its pure software rasteriser (its d3d12 back end needs DLLs WinPE lacks) and limit
    // the threads (the default, one per core, hung or powered off an 8-core AMD laptop).
    // Normal installs have no such DLL, so a real OpenGL driver is never touched.
    let mesa_beside_exe = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("libgallium_wgl.dll").is_file())).unwrap_or(false);
    if mesa_beside_exe {
        log::log("bundled Mesa found: software OpenGL (llvmpipe)");
        for (key, value) in [("GALLIUM_DRIVER", "llvmpipe"), ("MESA_D3D12_DEFAULT_ADAPTER_NAME", "none"), ("LP_NUM_THREADS", "4")] {
            if std::env::var_os(key).is_none() {
                std::env::set_var(key, value);
            }
        }
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(concat!("Ghost Commander v", env!("CARGO_PKG_VERSION")))
            .with_icon(eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/icon_256.png")).expect("embedded icon is a valid PNG"))
            .with_inner_size([1200.0, 700.0]),
        ..Default::default()
    };
    log::log("starting eframe (OpenGL window)");
    let result = eframe::run_native(
        "Ghost Commander",
        options,
        Box::new(|cc| {
            use eframe::glow::HasContext;
            if let Some(gl) = cc.gl.as_ref() {
                // SAFETY: plain string queries on the current context.
                let (vendor, renderer, version) = unsafe { (gl.get_parameter_string(eframe::glow::VENDOR), gl.get_parameter_string(eframe::glow::RENDERER), gl.get_parameter_string(eframe::glow::VERSION)) };
                log::log(&format!("OpenGL: {vendor} / {renderer} / {version}"));
            }
            log::log("window and GL context created, building the app");
            let app = App::new(cc);
            log::log("app built, entering the UI loop");
            Ok(Box::new(app))
        }),
    );
    match &result {
        Ok(()) => log::log("eframe exited normally"),
        Err(e) => log::log(&format!("eframe FAILED: {e:?}")),
    }
    result
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

/// Keeps the drive list (letters, types, free space) fresh from a worker thread.
fn watch_drives(ctx: &egui::Context) -> Arc<std::sync::Mutex<Vec<gc_core::DriveInfo>>> {
    let shared = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (out, ctx) = (shared.clone(), ctx.clone());
    std::thread::spawn(move || loop {
        let fresh = gc_core::drive_infos();
        if let Ok(mut current) = out.lock() {
            if *current != fresh {
                *current = fresh;
                ctx.request_repaint();
            }
        }
        std::thread::sleep(Duration::from_secs(3));
    });
    shared
}
