use eframe::egui::{self, Color32, Key};

const LOGO_PNG: &[u8] = include_bytes!("../../../assets/logo_512.png");
/// Logo is shown at this size in points.
const LOGO_SIZE: f32 = 260.0;

#[derive(Default)]
pub struct About {
    pub open: bool,
    logo: Option<egui::TextureHandle>,
}

impl About {
    pub fn show(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }
        if ctx.input(|i| i.key_pressed(Key::Escape) || i.key_pressed(Key::F1) || i.key_pressed(Key::Enter)) {
            self.open = false;
            return;
        }
        let logo = self.logo.get_or_insert_with(|| load_logo(ctx));
        let mut open = self.open;
        egui::Window::new("About Ghost Commander")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    // The artwork has a black ground, so it always sits on a black card, whatever the theme.
                    egui::Frame::new().fill(Color32::BLACK).corner_radius(8.0).show(ui, |ui| {
                        ui.add(egui::Image::new((logo.id(), egui::vec2(LOGO_SIZE, LOGO_SIZE))));
                    });
                    ui.add_space(6.0);
                    ui.heading(format!("Ghost Commander {}", env!("CARGO_PKG_VERSION")));
                    ui.label("Fast, modern dual-pane file manager written in Rust.");
                });
            });
        self.open = open;
    }
}

fn load_logo(ctx: &egui::Context) -> egui::TextureHandle {
    let img = eframe::icon_data::from_png_bytes(LOGO_PNG).expect("embedded logo is a valid PNG");
    let color = egui::ColorImage::from_rgba_unmultiplied([img.width as usize, img.height as usize], &img.rgba);
    ctx.load_texture("logo", color, egui::TextureOptions::LINEAR)
}
