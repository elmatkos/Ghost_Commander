//! Config > Options window. Settings are saved to `settings.toml` as soon as they change.

use eframe::egui::{self, Key};

use crate::theme::{load_settings, save_settings};

pub struct Options {
    pub open: bool,
    pub drive_buttons: bool,
    pub drive_free_bar: bool,
    pub switch_on_missing_drive: bool,
    pub pdf_turn_pages: bool,
    error: Option<String>,
}

impl Options {
    pub fn load() -> Self {
        let s = load_settings();
        Self { open: false, drive_buttons: s.drive_buttons, drive_free_bar: s.drive_free_bar, switch_on_missing_drive: s.switch_on_missing_drive, pdf_turn_pages: s.pdf_turn_pages, error: None }
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
            ui.add_space(10.0);
            ui.heading("When a drive disappears");
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                changed |= ui.radio_value(&mut self.switch_on_missing_drive, false, "Keep showing it, reload when it is back").changed();
                info_icon(ui).on_hover_text(
                    "A panel shows a drive that is removed (USB stick, network share, ...).\n\nKeep: the panel stays on that path and shows an error. Plug the drive in again and the folder loads by itself.",
                );
            });
            ui.horizontal(|ui| {
                changed |= ui.radio_value(&mut self.switch_on_missing_drive, true, "Switch to the nearest existing folder or drive").changed();
                info_icon(ui).on_hover_text(
                    "Switch: the panel jumps to the closest parent folder that still exists, or to another drive (C: first) when the whole drive is gone.\n\nThe removed path is not remembered, so it is not reopened when the drive returns.",
                );
            });
            ui.add_space(10.0);
            ui.heading("PDF viewer");
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                changed |= ui.checkbox(&mut self.pdf_turn_pages, "Arrow keys continue on the next / previous page").changed();
                info_icon(ui).on_hover_text(
                    "On: Down at the end of a page shows the start of the next page, Up at the top shows the end of the previous page, like scrolling through one long document.\n\nOff: the arrow keys stop at the page edges; use PgUp / PgDn or Left / Right to change the page.",
                );
            });
            if let Some(e) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
        });
        if changed {
            let mut s = load_settings();
            s.drive_buttons = self.drive_buttons;
            s.drive_free_bar = self.drive_free_bar;
            s.switch_on_missing_drive = self.switch_on_missing_drive;
            s.pdf_turn_pages = self.pdf_turn_pages;
            crate::prefs::set_pdf_turn_pages_cached(self.pdf_turn_pages);
            self.error = save_settings(&s).err().map(|e| format!("Cannot save settings: {e}"));
        }
        self.open = open && !ctx.input(|i| i.key_pressed(Key::Escape));
    }
}

/// A small circled "i" drawn by hand (the font has no such glyph); hover it for the help text.
fn info_icon(ui: &mut egui::Ui) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
    let color = if resp.hovered() { ui.visuals().strong_text_color() } else { ui.visuals().weak_text_color() };
    let painter = ui.painter();
    painter.circle_stroke(rect.center(), 7.0, egui::Stroke::new(1.3_f32, color));
    painter.text(rect.center(), egui::Align2::CENTER_CENTER, "i", egui::FontId::proportional(11.0), color);
    resp
}
