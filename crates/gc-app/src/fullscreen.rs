//! A page that covers the whole Ghost Commander window (used by the F3 viewer and the F4 editor):
//! a title row with a Close button, then whatever `body` draws. Returns true when Close was pressed.

use eframe::egui::{self, Frame, Id, Margin, Order, Sense};

pub fn show(ctx: &egui::Context, id: &str, title: &str, body: impl FnOnce(&mut egui::Ui)) -> bool {
    let rect = ctx.screen_rect();
    let mut close = false;
    egui::Area::new(Id::new(id)).order(Order::Foreground).fixed_pos(rect.min).constrain(false).show(ctx, |ui| {
        ui.set_clip_rect(rect);
        // Swallow the pointer so nothing underneath reacts.
        ui.interact(rect, Id::new((id, "block")), Sense::click_and_drag());
        Frame::new().fill(ctx.style().visuals.panel_fill).inner_margin(Margin::same(6)).show(ui, |ui| {
            ui.set_width(rect.width() - 12.0);
            ui.set_height(rect.height() - 12.0);
            ui.horizontal(|ui| {
                if ui.button("Close  (Esc)").clicked() {
                    close = true;
                }
                ui.strong(title);
            });
            ui.separator();
            body(ui);
        });
    });
    close
}
