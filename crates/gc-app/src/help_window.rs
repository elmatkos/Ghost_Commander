//! F1: every keyboard shortcut and function key, grouped by modifier.

use eframe::egui::{self, Key, RichText};

use crate::theme::Theme;

const GROUPS: [(&str, &[(&str, &str)]); 8] = [
    (
        "Function keys",
        &[
            ("F1", "This help"),
            ("F2", "Rename the item under the cursor (name and extension; also right click > Rename)"),
            ("F3", "View (text viewer, image viewer for pictures)"),
            ("F4", "Edit in Notepad"),
            ("F5", "Copy to the other panel (from an archive: extract); the dialog lets you edit the target path and, for one item, the new name"),
            ("F6", "Move to the other panel; same editable dialog (move to the same folder with a new name = rename)"),
            ("F7", "Make folder"),
            ("F8 / Del", "Move to the recycle bin"),
            ("F9", "System Info"),
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
