//! F1: every keyboard shortcut and function key, grouped by modifier.

use eframe::egui::{self, Key, RichText};

use crate::theme::Theme;

const GROUPS: [(&str, &[(&str, &str)]); 10] = [
    (
        "Function keys",
        &[
            ("F1", "This help"),
            ("F2", "Rename the item under the cursor (name and extension; also right click > Rename)"),
            ("F3", "View (text viewer in its own window over GC with syntax colors for C, C++, C#, PowerShell, Rust, Python, UEFI Shell, logs, JSON, XML/HTML, INI/TOML, Markdown and batch; hex dump for binary files; image viewer for pictures; DOCX, ODT and RTF documents as text; EVTX event logs in a filterable table; PDF pages through the Windows renderer)"),
            ("Viewer keys", "Up/Down, PgUp/PgDn or Space, Home/End, Left/Right scroll; F7 or Ctrl+F find (F3 next, Shift+F3 previous); Ctrl+G go to line (offset in hex); H hex / text; Wrap and Auto-reload checkboxes; Esc closes"),
            ("F4", "Edit (editor in its own window over GC with syntax colors, line numbers, Ctrl+S save, Ctrl+F find / replace, Ctrl+G go to line, Ctrl+F3 viewer beside the editor, Ctrl+Z / Ctrl+Y undo / redo, encoding and line-end choice; big or binary files open in Notepad)"),
            ("F5", "Copy to the other panel (from an archive: extract); the dialog lets you edit the target path and, for one item, the new name"),
            ("F6", "Move to the other panel; same editable dialog (move to the same folder with a new name = rename)"),
            ("F7", "Make folder"),
            ("F8 / Del", "Move to the recycle bin"),
            ("F9", "System Info (Up / Down / PgUp / PgDn / Home / End scroll it)"),
        ],
    ),
    ("Shift", &[("Shift+F4", "Create a new empty file and edit it"), ("Shift+F6", "Rename"), ("Shift+F8 / Shift+Del", "Delete permanently")]),
    (
        "Alt",
        &[
            ("Alt+F1 / Alt+F2", "Pick a drive for the left / right panel"),
            ("Alt+F5", "Pack the selection into a new archive (7z, zip, tar.gz, tar.xz)"),
        ],
    ),
    (
        "Ctrl",
        &[
            ("Ctrl+R", "Refresh both panels"),
            ("Ctrl+F", "Quick filter (Enter keeps it, Esc clears it)"),
            ("Ctrl+H", "Checksums: CRC-32, MD5, SHA-1, SHA-256 (and Save .crc)"),
            ("Ctrl+1 / Ctrl+2", "Pick a drive for the left / right panel"),
            ("Ctrl+T / Ctrl+W", "New tab (same folder) / close the tab"),
            ("Ctrl+L", "Lock / unlock the tab: a locked tab keeps its folder, going elsewhere opens a new tab"),
            ("Ctrl+Tab / Ctrl+Shift+Tab", "Next / previous tab of the active side"),
            ("Ctrl+D", "DISM manager: mount a WIM, commit / unmount"),
            ("Ctrl+N", "FTP connect: paste a link (ftp://user:password@host/dir) or pick a saved session"),
            ("Ctrl+F3 / F4 / F5 / F6", "Sort by name / extension / date / size (again = reverse)"),
        ],
    ),
    (
        "Command line (bottom)",
        &[
            ("Type any letter", "Starts the command line; Enter runs it in a new console in the folder shown by the active panel"),
            ("cd <folder>", "Changes the active panel to that folder instead"),
            ("Up / Down", "Earlier / later commands"),
            ("Esc / Tab", "Clear it / give the keyboard back to the panel"),
        ],
    ),
    (
        "FTP / FTPS (Net menu, Ctrl+N)",
        &[
            ("Enter / Backspace", "Open a folder / go up (stays at the root of the server; Disconnect button or Net menu leaves it)"),
            ("Log button, Net menu", "Connection log: every step, answer and error (also shown after a failed connect)"),
            ("F5 / F6", "Download to the other panel, or upload from here when the other panel is a server (F6 deletes the source afterwards)"),
            ("F3 / Enter on a file", "Download to a scratch folder and view / open it"),
            ("F4", "Edit a local copy (not uploaded back)"),
            ("F7 / F8", "Create a folder / delete on the server"),
            ("Drive buttons, Alt+F1/F2", "Leave the server and show a local drive"),
        ],
    ),
    (
        "DISM manager (Tools menu, Ctrl+D)",
        &[
            ("Mount", "Pick a .wim / .esd (or Use file under cursor), an image and a folder; the folder is created when missing"),
            ("Badge after Help", "Shown while an image is mounted (also one mounted by other tools or before a restart); click it to open the manager"),
            ("Commit (keep mounted)", "Save the changes made in the folder into the WIM and stay mounted"),
            ("Commit and unmount / Discard", "Save and unmount, or drop all changes; panels showing the folder move out of it first"),
            ("Administrator", "DISM needs it: without it each job asks for approval (UAC)"),
        ],
    ),
    (
        "Tabs",
        &[
            ("+ / click / middle click", "Open a tab / switch / close (the strip above each panel)"),
            ("Right click a tab", "Lock (orange frame and lock icon; leaving its folder opens a new tab) / close"),
            ("Remembered", "Both sides reopen their tabs, locks and the shown tab at the next start"),
        ],
    ),
    (
        "Moving around",
        &[
            ("Up / Down", "Move the cursor"),
            ("PgUp / PgDn", "Move one page"),
            ("Home / End", "First / last entry"),
            ("Enter", "Open folder or archive, start the file with its Windows default; a .crc/.sfv/.md5/.sha1/.sha256 file is verified"),
            ("Backspace", "Go up (out of an archive too)"),
            ("Tab", "Switch panel"),
            ("Space / Insert", "Mark or unmark and move down"),
            ("Esc", "Close the window or dialog on top"),
        ],
    ),
    (
        "Mouse",
        &[("Right click", "Windows context menu of the item"), ("Click a heading", "Sort by Name, Size or Date; click again to reverse"), ("Drive buttons", "Switch drive; \\ goes to the root, .. one folder up")],
    ),
];

#[derive(Default)]
pub struct HelpWindow {
    pub open: bool,
}

impl HelpWindow {
    pub fn show(&mut self, ctx: &egui::Context, theme: &Theme) {
        if !self.open {
            return;
        }
        let mut open = self.open;
        egui::Window::new("Help - keyboard shortcuts").open(&mut open).default_size([640.0, 560.0]).show(ctx, |ui| {
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                for (n, (title, rows)) in GROUPS.iter().enumerate() {
                    ui.add_space(if n == 0 { 0.0 } else { 10.0 });
                    ui.label(RichText::new(*title).strong().size(15.0).color(theme.accent.0));
                    egui::Grid::new(("help", n)).num_columns(2).spacing([16.0, 3.0]).show(ui, |ui| {
                        for (key, what) in *rows {
                            ui.label(RichText::new(*key).monospace().strong());
                            ui.label(*what);
                            ui.end_row();
                        }
                    });
                }
            });
        });
        self.open = open && !ctx.input(|i| i.key_pressed(Key::Escape));
    }
}
