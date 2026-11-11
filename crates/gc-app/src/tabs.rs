//! Tab strip above each panel: one tab per open folder, a "+" to open another.

use eframe::egui::{self, RichText};

pub enum TabClick {
    Select(usize),
    Close(usize),
    New,
}

/// Draws the strip; returns what was clicked. A middle click on a tab closes it.
pub fn show(ui: &mut egui::Ui, titles: &[String], cur: usize, side_active: bool) -> Option<TabClick> {
    let mut out = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for (k, title) in titles.iter().enumerate() {
            let mut text = RichText::new(title);
            if k == cur && side_active {
                text = text.strong();
            }
            let r = ui.selectable_label(k == cur, text).on_hover_text("Click: switch  |  middle click: close  |  Ctrl+T new, Ctrl+W close, Ctrl+Tab next");
            if r.clicked() {
                out = Some(TabClick::Select(k));
            }
            if r.middle_clicked() && titles.len() > 1 {
                out = Some(TabClick::Close(k));
            }
            if k == cur && titles.len() > 1 && ui.small_button("x").on_hover_text("Close this tab (Ctrl+W)").clicked() {
                out = Some(TabClick::Close(k));
            }
        }
        if ui.small_button("+").on_hover_text("New tab in this folder (Ctrl+T)").clicked() {
            out = Some(TabClick::New);
        }
    });
    out
}
