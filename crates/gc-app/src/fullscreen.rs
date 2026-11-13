//! The F3 viewer and the F4 editor are separate native windows that open exactly over the Ghost
//! Commander window. They resize / maximize on their own (GC underneath is not touched); minimizing
//! one of them minimizes GC too, and restoring either restores both (`Link::sync`).

use eframe::egui::{self, Context, Frame, Id, Margin, Order, Pos2, Sense, Vec2, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId};

/// Where the window was opened and the minimize state of both windows.
#[derive(Default)]
pub struct Link {
    geom: Option<(Pos2, Vec2)>,
    child_min: bool,
    root_min: bool,
}

impl Link {
    /// The viewport description: the GC window's place and size, taken once when the window opens (it
    /// must not change later, or it would move the window back).
    pub fn builder(&mut self, ctx: &Context, title: &str) -> ViewportBuilder {
        let (pos, size) = *self.geom.get_or_insert_with(|| {
            ctx.input(|i| {
                let v = i.viewport();
                (v.outer_rect.map_or(Pos2::new(100.0, 100.0), |r| r.min), v.inner_rect.map_or(Vec2::new(1100.0, 700.0), |r| r.size()))
            })
        });
        ViewportBuilder::default()
            .with_title(title)
            .with_position(pos)
            .with_inner_size(size)
            .with_min_inner_size([480.0, 300.0])
            .with_icon(eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/icon_256.png")).expect("embedded icon is a valid PNG"))
    }

    /// Call from the main window after showing the viewport. `child_min`: the viewport is minimized.
    pub fn sync(&mut self, ctx: &Context, id: ViewportId, child_min: bool) {
        let root_min = ctx.input(|i| i.viewport().minimized).unwrap_or(false);
        if child_min != self.child_min {
            ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Minimized(child_min));
        }
        if root_min != self.root_min && root_min != child_min {
            ctx.send_viewport_cmd_to(id, ViewportCommand::Minimized(root_min));
        }
        self.child_min = child_min;
        self.root_min = root_min;
    }
}

/// The page of the window: a title row with a Close button, then whatever `body` draws. Returns true
/// when Close was pressed. (Without separate-window support it is drawn over the main window.)
pub fn show(ctx: &Context, class: ViewportClass, title: &str, body: impl FnOnce(&mut egui::Ui)) -> bool {
    let mut close = false;
    let content = |ui: &mut egui::Ui| {
        ui.horizontal(|ui| {
            if ui.button("Close  (Esc)").clicked() {
                close = true;
            }
            if class == ViewportClass::Embedded {
                ui.strong(title);
            }
        });
        ui.separator();
        body(ui);
    };
    if class == ViewportClass::Embedded {
        let rect = ctx.screen_rect();
        egui::Area::new(Id::new("gc_fullscreen_page")).order(Order::Foreground).fixed_pos(rect.min).constrain(false).show(ctx, |ui| {
            ui.set_clip_rect(rect);
            ui.interact(rect, Id::new("gc_fullscreen_block"), Sense::click_and_drag());
            Frame::new().fill(ctx.style().visuals.panel_fill).inner_margin(Margin::same(6)).show(ui, |ui| {
                ui.set_width(rect.width() - 12.0);
                ui.set_height(rect.height() - 12.0);
                content(ui);
            });
        });
    } else {
        egui::CentralPanel::default().show(ctx, content);
    }
    close
}
