//! F3 on an image: opens it in its own window. Small images show at native size; images larger
//! than the monitor are scaled down to 75% of the monitor height (friendly to wide monitors).
//! `+` / `-` zoom (`0` resets, Ctrl+wheel works too), drag pans. Decoding and resizing run on a
//! worker thread.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use eframe::egui::{self, Key};

/// Formats the `image` crate is built with here.
const EXTENSIONS: [&str; 7] = ["png", "jpg", "jpeg", "gif", "bmp", "webp", "ico"];
/// Share of the monitor height an oversized image is scaled to.
const HEIGHT_FRACTION: f32 = 0.75;
/// Zoom change per key press.
const ZOOM_STEP: f32 = 1.25;
const ZOOM_RANGE: (f32, f32) = (0.1, 16.0);
/// Longest side of the full-resolution copy kept for zooming (GPU texture limit).
const MAX_FULL_SIDE: u32 = 8192;

pub fn is_image(path: &Path) -> bool {
    gc_core::filetype::extension_of(path).is_some_and(|e| EXTENSIONS.contains(&e.as_str()))
}

/// Pixel size to show an `w` x `h` image at on a monitor of `monitor` pixels.
fn target_size(w: u32, h: u32, (mw, mh): (u32, u32)) -> (u32, u32) {
    if w <= mw && h <= mh {
        return (w, h);
    }
    // Never wider than the screen either (panoramas).
    let scale = (mh as f32 * HEIGHT_FRACTION / h as f32).min(mw as f32 * 0.95 / w as f32);
    (((w as f32 * scale).round() as u32).max(1), ((h as f32 * scale).round() as u32).max(1))
}

struct Decoded {
    /// What the window shows at first (zoom 1).
    fitted: egui::ColorImage,
    /// Sharper copy for zooming in; only kept when `fitted` is a reduction.
    full: Option<egui::ColorImage>,
    original: [u32; 2],
}

fn to_color(img: &image::DynamicImage) -> egui::ColorImage {
    egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.to_rgba8().as_raw())
}

fn decode(path: &Path, monitor: (u32, u32)) -> Result<Decoded, String> {
    let img = image::open(path).map_err(|e| e.to_string())?;
    let (w, h) = (img.width(), img.height());
    let (tw, th) = target_size(w, h, monitor);
    if (tw, th) == (w, h) {
        return Ok(Decoded { fitted: to_color(&img), full: None, original: [w, h] });
    }
    // Lanczos on the CPU looks much better than the GPU's linear minification.
    let fitted = to_color(&img.resize_exact(tw, th, image::imageops::FilterType::Lanczos3));
    let full = if w.max(h) > MAX_FULL_SIDE { to_color(&img.resize(MAX_FULL_SIDE, MAX_FULL_SIDE, image::imageops::FilterType::Lanczos3)) } else { to_color(&img) };
    Ok(Decoded { fitted, full: Some(full), original: [w, h] })
}

struct Shown {
    title: String,
    fitted: egui::TextureHandle,
    /// Uploaded on the first zoom-in that needs it.
    full_image: Option<egui::ColorImage>,
    full: Option<egui::TextureHandle>,
    /// 1.0 = fit the window.
    zoom: f32,
}

enum State {
    Loading { rx: Receiver<Result<Decoded, String>>, path: PathBuf },
    Showing(Shown),
}

#[derive(Default)]
pub struct ImageViewer {
    state: Option<State>,
}

impl ImageViewer {
    pub fn open(&mut self, ctx: &egui::Context, path: &Path) {
        let monitor = ctx.input(|i| i.viewport().monitor_size).map_or((1920, 1080), |s| {
            let ppp = ctx.pixels_per_point();
            ((s.x * ppp) as u32, (s.y * ppp) as u32)
        });
        let (tx, rx) = mpsc::channel();
        let (ctx, file) = (ctx.clone(), path.to_path_buf());
        std::thread::spawn(move || {
            let _ = tx.send(decode(&file, monitor));
            ctx.request_repaint();
        });
        self.state = Some(State::Loading { rx, path: path.to_path_buf() });
    }

    /// Draws the viewer window; returns an error message when the image could not be decoded.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<String> {
        if let Some(State::Loading { rx, path }) = &self.state {
            match rx.try_recv() {
                Err(TryRecvError::Empty) => return None,
                Ok(Ok(d)) => {
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let [w, h] = d.original;
                    let shown = d.fitted.size[0] as u32;
                    let title = if shown == w { format!("{name} - {w}x{h}") } else { format!("{name} - {w}x{h} (shown at {}%)", shown * 100 / w) };
                    let fitted = ctx.load_texture("image-viewer", d.fitted, egui::TextureOptions::LINEAR);
                    self.state = Some(State::Showing(Shown { title, fitted, full_image: d.full, full: None, zoom: 1.0 }));
                }
                Ok(Err(e)) => {
                    let msg = format!("{}: {e}", path.display());
                    self.state = None;
                    return Some(msg);
                }
                Err(TryRecvError::Disconnected) => {
                    self.state = None;
                    return Some("image decoder stopped".into());
                }
            }
        }
        let Some(State::Showing(s)) = &mut self.state else { return None };
        let ppp = ctx.pixels_per_point();
        let size = s.fitted.size_vec2() / ppp;
        let title = if s.zoom == 1.0 { s.title.clone() } else { format!("{} - zoom {}%", s.title, (s.zoom * 100.0).round()) };
        let mut close = false;
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("image-viewer"),
            egui::ViewportBuilder::default().with_title(title).with_inner_size(size),
            |ctx, _| {
                let (zoom_in, zoom_out, reset, wheel, quit) = ctx.input(|i| {
                    (
                        i.key_pressed(Key::Plus) || i.key_pressed(Key::Equals),
                        i.key_pressed(Key::Minus),
                        i.key_pressed(Key::Num0),
                        i.zoom_delta(),
                        i.viewport().close_requested() || i.key_pressed(Key::Escape) || i.key_pressed(Key::Q) || i.key_pressed(Key::F3),
                    )
                });
                if zoom_in {
                    s.zoom *= ZOOM_STEP;
                }
                if zoom_out {
                    s.zoom /= ZOOM_STEP;
                }
                s.zoom = if reset { 1.0 } else { (s.zoom * wheel).clamp(ZOOM_RANGE.0, ZOOM_RANGE.1) };
                close = quit;

                egui::CentralPanel::default().frame(egui::Frame::new().fill(egui::Color32::BLACK)).show(ctx, |ui| {
                    let avail = ui.available_size();
                    let draw = size * (avail.x / size.x).min(avail.y / size.y) * s.zoom;
                    // Past the fitted texture's resolution, switch to the sharper full copy.
                    if draw.x * ppp > s.fitted.size()[0] as f32 && s.full.is_none() {
                        if let Some(img) = s.full_image.take() {
                            s.full = Some(ctx.load_texture("image-viewer-full", img, egui::TextureOptions::LINEAR));
                        }
                    }
                    let texture = match &s.full {
                        Some(full) if draw.x * ppp > s.fitted.size()[0] as f32 => full,
                        _ => &s.fitted,
                    };
                    egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                        // Centered while it fits, scrollable (and draggable) once it is larger.
                        let (area, _) = ui.allocate_exact_size(draw.max(avail), egui::Sense::hover());
                        let rect = egui::Rect::from_center_size(area.center(), draw);
                        ui.painter().image(texture.id(), rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                    });
                });
            },
        );
        if close {
            self.state = None;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::target_size;

    #[test]
    fn fitting_images_keep_native_size() {
        assert_eq!(target_size(1920, 1080, (5120, 1440)), (1920, 1080));
        assert_eq!(target_size(5120, 1440, (5120, 1440)), (5120, 1440));
    }

    #[test]
    fn tall_images_shrink_to_three_quarters_of_monitor_height() {
        assert_eq!(target_size(4000, 3000, (5120, 1440)), (1440, 1080));
    }

    #[test]
    fn panoramas_are_limited_by_width() {
        let (w, h) = target_size(10000, 1000, (5120, 1440));
        assert_eq!(w, 4864);
        assert!(h < 1080);
    }
}
