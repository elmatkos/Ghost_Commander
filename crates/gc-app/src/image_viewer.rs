//! F3 on an image: opens it in its own window. Small images show at native size; images larger
//! than the monitor are scaled down to 75% of the monitor height (friendly to wide monitors).
//! Decoding and resizing run on a worker thread.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use eframe::egui::{self, Key};

/// Formats the `image` crate is built with here.
const EXTENSIONS: [&str; 7] = ["png", "jpg", "jpeg", "gif", "bmp", "webp", "ico"];
/// Share of the monitor height an oversized image is scaled to.
const HEIGHT_FRACTION: f32 = 0.75;

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
    image: egui::ColorImage,
    original: [u32; 2],
}

fn decode(path: &Path, monitor: (u32, u32)) -> Result<Decoded, String> {
    let img = image::open(path).map_err(|e| e.to_string())?;
    let (w, h) = (img.width(), img.height());
    let (tw, th) = target_size(w, h, monitor);
    // Lanczos on the CPU looks much better than the GPU's linear minification.
    let img = if (tw, th) == (w, h) { img } else { img.resize_exact(tw, th, image::imageops::FilterType::Lanczos3) };
    let rgba = img.to_rgba8();
    Ok(Decoded { image: egui::ColorImage::from_rgba_unmultiplied([tw as usize, th as usize], rgba.as_raw()), original: [w, h] })
}

enum State {
    Loading { rx: Receiver<Result<Decoded, String>>, path: PathBuf },
    Showing { title: String, texture: egui::TextureHandle },
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
                    let shown = d.image.size[0] as u32;
                    let title = if shown == w { format!("{name} - {w}x{h}") } else { format!("{name} - {w}x{h} (shown at {}%)", shown * 100 / w) };
                    let texture = ctx.load_texture("image-viewer", d.image, egui::TextureOptions::LINEAR);
                    self.state = Some(State::Showing { title, texture });
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
        let Some(State::Showing { title, texture }) = &self.state else { return None };
        let size = texture.size_vec2() / ctx.pixels_per_point();
        let mut close = false;
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("image-viewer"),
            egui::ViewportBuilder::default().with_title(title.as_str()).with_inner_size(size),
            |ctx, _| {
                egui::CentralPanel::default().frame(egui::Frame::new().fill(egui::Color32::BLACK)).show(ctx, |ui| {
                    // Follows the window if the user resizes it, keeping the aspect ratio.
                    let avail = ui.available_size();
                    let draw = size * (avail.x / size.x).min(avail.y / size.y);
                    ui.centered_and_justified(|ui| ui.add(egui::Image::new((texture.id(), draw))));
                });
                close = ctx.input(|i| i.viewport().close_requested() || i.key_pressed(Key::Escape) || i.key_pressed(Key::Q) || i.key_pressed(Key::F3));
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
