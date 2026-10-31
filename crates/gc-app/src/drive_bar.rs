//! Drive buttons above a panel, Total Commander style: one pill per drive with a type icon,
//! the letter and a thin used-space bar, then `\` (root of the drive) and `..` (parent).

use std::path::{Path, PathBuf};

use eframe::egui::{self, Align2, Color32, FontId, Rect, Sense, Stroke, StrokeKind};
use egui_phosphor::regular as icon;
use gc_core::{DriveInfo, DriveKind};

use crate::panel::fmt_size;
use crate::theme::Theme;

pub enum Click {
    Drive(PathBuf),
    Root,
    Up,
}

const BUTTON_H: f32 = 30.0;
const DRIVE_W: f32 = 54.0;
const NAV_W: f32 = 30.0;

fn glyph(kind: DriveKind) -> &'static str {
    match kind {
        DriveKind::Fixed => icon::HARD_DRIVE,
        DriveKind::Removable => icon::USB,
        DriveKind::Network => icon::NETWORK,
        DriveKind::Optical => icon::DISC,
        DriveKind::Other => icon::HARD_DRIVES,
    }
}

/// Draws one pill; returns its response. `fill` is the resting background.
fn pill(ui: &mut egui::Ui, width: f32, theme: &Theme, selected: bool) -> (egui::Response, Rect) {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, BUTTON_H), Sense::click());
    let accent = theme.accent.0;
    let fill = if selected {
        accent.gamma_multiply(0.35)
    } else if resp.hovered() {
        ui.visuals().widgets.hovered.bg_fill
    } else {
        ui.visuals().widgets.inactive.weak_bg_fill
    };
    let stroke = if selected { Stroke::new(1.0_f32, accent) } else { Stroke::new(1.0_f32, ui.visuals().widgets.noninteractive.bg_stroke.color) };
    ui.painter().rect(rect, 6.0, fill, stroke, StrokeKind::Inside);
    (resp, rect)
}

pub fn show(ui: &mut egui::Ui, drives: &[DriveInfo], current: &Path, theme: &Theme, free_bar: bool) -> Option<Click> {
    let mut click = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
        for d in drives {
            let selected = current.starts_with(&d.root);
            let (resp, rect) = pill(ui, DRIVE_W, theme, selected);
            let letter = d.root.to_string_lossy().chars().next().unwrap_or('?').to_ascii_uppercase();
            let p = ui.painter();
            let text_color = if selected { theme.text.0 } else { ui.visuals().text_color() };
            p.text(rect.left_center() + egui::vec2(8.0, if free_bar { -2.0 } else { 0.0 }), Align2::LEFT_CENTER, glyph(d.kind), FontId::proportional(15.0), theme.style(gc_core::filetype::Category::Folder).1);
            p.text(rect.right_center() - egui::vec2(9.0, if free_bar { 2.0 } else { 0.0 }), Align2::RIGHT_CENTER, letter, FontId::monospace(14.0), text_color);
            if free_bar && d.total > 0 {
                let used = 1.0 - d.free as f32 / d.total as f32;
                let track = Rect::from_min_size(rect.left_bottom() + egui::vec2(6.0, -6.0), egui::vec2(rect.width() - 12.0, 3.0));
                p.rect_filled(track, 1.5, Color32::from_black_alpha(90));
                let color = if used > 0.9 { theme.error.0 } else { theme.accent.0 };
                p.rect_filled(Rect::from_min_size(track.min, egui::vec2(track.width() * used.clamp(0.0, 1.0), 3.0)), 1.5, color);
            }
            let tip = if d.total > 0 { format!("{}\n{} free of {}", d.root.display(), fmt_size(d.free), fmt_size(d.total)) } else { format!("{}\nno medium", d.root.display()) };
            if resp.on_hover_text(tip).clicked() {
                click = Some(Click::Drive(d.root.clone()));
            }
        }
        ui.add_space(6.0);
        for (label, tip, which) in [("\\", "Root of the drive", Click::Root), (icon::ARROW_ELBOW_LEFT_UP, "Parent folder", Click::Up)] {
            let (resp, rect) = pill(ui, NAV_W, theme, false);
            let size = if label == "\\" { FontId::monospace(15.0) } else { FontId::proportional(15.0) };
            ui.painter().text(rect.center(), Align2::CENTER_CENTER, label, size, ui.visuals().text_color());
            if resp.on_hover_text(tip).clicked() {
                click = Some(which);
            }
        }
    });
    click
}
