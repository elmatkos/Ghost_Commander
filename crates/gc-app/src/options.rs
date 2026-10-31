//! Config > Options window. Settings are saved to `settings.toml` as soon as they change.

use eframe::egui::{self, Key};

use crate::theme::{load_settings, save_settings};

pub struct Options {
    pub open: bool,
    pub drive_buttons: bool,
    pub drive_free_bar: bool,
    error: Option<String>,
}

impl Options {
    pub fn load() -> Self {
        let s = load_settings();
        Self { open: false, drive_buttons: s.drive_buttons, drive_free_bar: s.drive_free_bar, error: None }
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }
        let mut open = self.open;
        let mut changed = false;
        egui::Window::new("Options").open(&mut open).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
            ui.heading("Drive buttons");
            ui.add_space(4.0);
            changed |= ui.checkbox(&mut self.drive_buttons, "Show drive buttons above each panel").changed();
            ui.add_enabled_ui(self.drive_buttons, |ui| {
                changed |= ui.checkbox(&mut self.drive_free_bar, "Show a free-space bar under every drive").changed();
            });
            ui.add_space(2.0);
            ui.weak("Also adds \\ (root of the drive) and .. (parent folder) buttons.");
            if let Some(e) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
        });
        if changed {
            let mut s = load_settings();
            s.drive_buttons = self.drive_buttons;
            s.drive_free_bar = self.drive_free_bar;
            self.error = save_settings(&s).err().map(|e| format!("Cannot save settings: {e}"));
        }
        self.open = open && !ctx.input(|i| i.key_pressed(Key::Escape));
    }
}
