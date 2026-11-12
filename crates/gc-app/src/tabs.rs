//! Tab strip below the drive buttons of each panel: one framed tab per open folder, a "+" to open another.

use eframe::egui::{self, Color32, CornerRadius, Frame, Margin, RichText, Sense, Stroke, StrokeKind, Vec2};

pub enum TabClick {
    Select(usize),
    Close(usize),
    Lock(usize),
    New,
}

/// Border of a locked tab.
const LOCK_COLOR: Color32 = Color32::from_rgb(230, 150, 30);
const RED: Color32 = Color32::from_rgb(200, 50, 50);
const RED_HOVER: Color32 = Color32::from_rgb(235, 70, 70);

/// The close button of the active tab: a red square with a white x.
fn close_button(ui: &mut egui::Ui) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(15.0), Sense::click());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(2), if resp.hovered() { RED_HOVER } else { RED });
    let c = rect.center();
    let d = 3.5;
    let stroke = Stroke::new(1.5_f32, Color32::WHITE);
    painter.line_segment([c + Vec2::new(-d, -d), c + Vec2::new(d, d)], stroke);
    painter.line_segment([c + Vec2::new(-d, d), c + Vec2::new(d, -d)], stroke);
    resp.on_hover_text("Close this tab (Ctrl+W)")
}

/// Draws the strip; returns what was clicked. A middle click on a tab closes it.
pub fn show(ui: &mut egui::Ui, titles: &[(String, bool)], cur: usize, side_active: bool) -> Option<TabClick> {
    let mut out = None;
    let visuals = ui.visuals().clone();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(4.0, 2.0);
        for (k, (title, locked)) in titles.iter().enumerate() {
            let active = k == cur;
            let (mut stroke, fill) = if active {
                (Stroke::new(1.0_f32, if side_active { visuals.selection.stroke.color } else { visuals.widgets.inactive.fg_stroke.color }), visuals.selection.bg_fill)
            } else {
                (Stroke::new(1.0_f32, visuals.widgets.noninteractive.bg_stroke.color), Color32::TRANSPARENT)
            };
            if *locked {
                stroke = Stroke::new(1.5_f32, LOCK_COLOR);
            }
            Frame::NONE.stroke(stroke).fill(fill).corner_radius(CornerRadius::same(3)).inner_margin(Margin::symmetric(6, 2)).show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let mut text = RichText::new(if *locked { format!("{} {title}", egui_phosphor::regular::LOCK_SIMPLE) } else { title.clone() });
                if active {
                    text = text.strong();
                }
                let r = ui.add(egui::Label::new(text).selectable(false).sense(Sense::click())).on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Click: switch  |  right click: lock / close  |  middle click: close  |  Ctrl+T new, Ctrl+W close, Ctrl+L lock, Ctrl+Tab next");
                if r.clicked() {
                    out = Some(TabClick::Select(k));
                }
                if r.middle_clicked() && titles.len() > 1 {
                    out = Some(TabClick::Close(k));
                }
                r.context_menu(|ui| {
                    if ui.button(if *locked { "Unlock tab  (Ctrl+L)" } else { "Lock tab  (Ctrl+L)" }).clicked() {
                        out = Some(TabClick::Lock(k));
                        ui.close_menu();
                    }
                    if titles.len() > 1 && ui.button("Close tab  (Ctrl+W)").clicked() {
                        out = Some(TabClick::Close(k));
                        ui.close_menu();
                    }
                });
                if active && titles.len() > 1 && close_button(ui).clicked() {
                    out = Some(TabClick::Close(k));
                }
            });
        }
        let (rect, plus) = ui.allocate_exact_size(Vec2::new(22.0, 20.0), Sense::click());
        let border = if plus.hovered() { visuals.widgets.hovered.bg_stroke.color } else { visuals.widgets.noninteractive.bg_stroke.color };
        ui.painter().rect_stroke(rect, CornerRadius::same(3), Stroke::new(1.0_f32, border), StrokeKind::Inside);
        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "+", egui::FontId::proportional(14.0), visuals.text_color());
        if plus.on_hover_text("New tab in this folder (Ctrl+T)").clicked() {
            out = Some(TabClick::New);
        }
    });
    out
}
