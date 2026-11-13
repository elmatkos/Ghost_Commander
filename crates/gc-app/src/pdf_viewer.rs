//! F3 on a `.pdf` file: page viewer that uses the PDF renderer built into Windows 10 / 11
//! (`Windows.Data.Pdf`, no extra files). A worker thread renders pages to PNG, the window shows the
//! current page (fit to the width, zoomable); PgUp / PgDn scroll and turn pages, Left / Right and
//! Home / End jump between pages. Not available in WinPE (the renderer is missing there).

use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};

use eframe::egui::{self, load::SizedTexture, ColorImage, Key, TextureHandle, TextureOptions, Vec2};

use crate::fullscreen::Link;

enum Req {
    Render { page: u32, width: u32 },
}

enum Msg {
    Info(Result<u32, String>),
    Page { page: u32, width: u32, image: Result<ColorImage, String> },
}

struct Doc {
    name: String,
    pages: Option<u32>,
    error: Option<String>,
    tx: Sender<Req>,
    rx: Receiver<Msg>,
    cur: u32,
    zoom: f32,
    /// Rendered pages: (page, width in pixels) and the texture.
    tex: Vec<((u32, u32), TextureHandle)>,
    pending: Vec<(u32, u32)>,
    scroll: Vec2,
    view_h: f32,
    content_h: f32,
    goto_q: String,
}

#[derive(Default)]
pub struct PdfViewer {
    open: Option<Doc>,
    link: Link,
    fresh: bool,
}

#[cfg(windows)]
fn es<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

#[cfg(windows)]
fn render(doc: &windows::Data::Pdf::PdfDocument, page: u32, width: u32) -> Result<ColorImage, String> {
    use windows::Data::Pdf::PdfPageRenderOptions;
    use windows::Storage::Streams::{DataReader, InMemoryRandomAccessStream};
    let p = doc.GetPage(page).map_err(es)?;
    let opts = PdfPageRenderOptions::new().map_err(es)?;
    opts.SetDestinationWidth(width).map_err(es)?;
    let stream = InMemoryRandomAccessStream::new().map_err(es)?;
    p.RenderWithOptionsToStreamAsync(&stream, &opts).map_err(es)?.get().map_err(es)?;
    let size = stream.Size().map_err(es)? as u32;
    let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0).map_err(es)?).map_err(es)?;
    reader.LoadAsync(size).map_err(es)?.get().map_err(es)?;
    let mut buf = vec![0u8; size as usize];
    reader.ReadBytes(&mut buf).map_err(es)?;
    let img = image::load_from_memory(&buf).map_err(es)?.to_rgba8();
    Ok(ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw()))
}

#[cfg(windows)]
fn worker(path: std::path::PathBuf, ctx: egui::Context, rx: Receiver<Req>, tx: Sender<Msg>) {
    use windows::core::HSTRING;
    use windows::Data::Pdf::PdfDocument;
    use windows::Storage::StorageFile;
    use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

    let run = || -> Result<(), String> {
        unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.map_err(es)?;
        let abs = std::fs::canonicalize(&path).map_err(es)?;
        let abs = abs.to_string_lossy().trim_start_matches(r"\\?\").to_string();
        let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(abs)).map_err(es)?.get().map_err(es)?;
        let doc = PdfDocument::LoadFromFileAsync(&file).map_err(es)?.get().map_err(|e| format!("{e} (password protected or damaged?)"))?;
        let _ = tx.send(Msg::Info(Ok(doc.PageCount().map_err(es)?)));
        ctx.request_repaint();
        while let Ok(Req::Render { page, width }) = rx.recv() {
            let image = render(&doc, page, width);
            if tx.send(Msg::Page { page, width, image }).is_err() {
                break;
            }
            ctx.request_repaint();
        }
        Ok(())
    };
    if let Err(e) = run() {
        let _ = tx.send(Msg::Info(Err(e)));
        ctx.request_repaint();
    }
}

#[cfg(not(windows))]
fn worker(_: std::path::PathBuf, ctx: egui::Context, _: Receiver<Req>, tx: Sender<Msg>) {
    let _ = tx.send(Msg::Info(Err("PDF pages can only be shown on Windows.".into())));
    ctx.request_repaint();
}

impl Doc {
    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Info(Ok(n)) => self.pages = Some(n),
                Msg::Info(Err(e)) => self.error = Some(format!("{e}. The PDF viewer needs the renderer that is part of Windows 10 / 11 (it is not in WinPE).")),
                Msg::Page { page, width, image } => {
                    self.pending.retain(|&p| p != (page, width));
                    match image {
                        Ok(img) => {
                            let t = ctx.load_texture(format!("pdf-{page}-{width}"), img, TextureOptions::LINEAR);
                            self.tex.push(((page, width), t));
                            if self.tex.len() > 6 {
                                let _ = self.tex.remove(0);
                            }
                        }
                        Err(e) => self.error = Some(format!("Page {}: {e}", page + 1)),
                    }
                }
            }
        }
    }

    fn texture(&self, page: u32, width: u32) -> Option<&TextureHandle> {
        self.tex.iter().find(|(k, _)| *k == (page, width)).or_else(|| self.tex.iter().rev().find(|(k, _)| k.0 == page)).map(|(_, t)| t)
    }

    fn want(&mut self, page: u32, width: u32) {
        if !self.tex.iter().any(|(k, _)| *k == (page, width)) && !self.pending.contains(&(page, width)) {
            self.pending.push((page, width));
            let _ = self.tx.send(Req::Render { page, width });
        }
    }

    fn set_page(&mut self, p: u32) {
        let last = self.pages.unwrap_or(1).saturating_sub(1);
        let p = p.min(last);
        if p != self.cur {
            self.cur = p;
            self.scroll = Vec2::ZERO;
        }
    }
}

impl PdfViewer {
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    pub fn open(&mut self, ctx: &egui::Context, path: &Path) {
        let (req_tx, req_rx) = channel();
        let (msg_tx, msg_rx) = channel();
        let (c, p) = (ctx.clone(), path.to_path_buf());
        std::thread::spawn(move || worker(p, c, req_rx, msg_tx));
        self.open = Some(Doc {
            name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            pages: None,
            error: None,
            tx: req_tx,
            rx: msg_rx,
            cur: 0,
            zoom: 1.0,
            tex: Vec::new(),
            pending: Vec::new(),
            scroll: Vec2::ZERO,
            view_h: 600.0,
            content_h: 0.0,
            goto_q: String::new(),
        });
        self.fresh = true;
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        let Some(doc) = &mut self.open else { return };
        let fresh = std::mem::take(&mut self.fresh);
        if fresh {
            self.link = Link::default();
        }
        doc.poll(ctx);
        let title = match doc.pages {
            Some(n) => format!("PDF: {}  (page {} of {n})", doc.name, doc.cur + 1),
            None => format!("PDF: {}", doc.name),
        };
        let builder = self.link.builder(ctx, &title);
        let id = egui::ViewportId::from_hash_of("gc_pdf");
        let (mut close, mut minimized) = (false, false);
        ctx.show_viewport_immediate(id, builder, |ctx, class| {
            minimized = ctx.input(|i| i.viewport().minimized).unwrap_or(false);
            close = ctx.input(|i| i.viewport().close_requested());
            if crate::fullscreen::show(ctx, class, &title, |ui| {
                if draw(doc, ui, fresh) {
                    close = true;
                }
            }) {
                close = true;
            }
        });
        self.link.sync(ctx, id, minimized);
        if close {
            self.open = None;
            ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Focus);
        }
    }
}

/// Draws the window content; returns true when a key asked to close it.
fn draw(doc: &mut Doc, ui: &mut egui::Ui, fresh: bool) -> bool {
    let ctx = ui.ctx().clone();
    let typing = ctx.wants_keyboard_input();
    let ppp = ctx.pixels_per_point();
    let mut close = false;
    let pages = doc.pages.unwrap_or(0);

    if !fresh && !typing && ctx.input(|i| i.key_pressed(Key::Escape) || i.key_pressed(Key::Q) || i.key_pressed(Key::F3)) {
        close = true;
    }
    if pages > 0 && !typing {
        let (up, down, pgup, pgdn, left, right, home, end, plus, minus, zero) = ctx.input(|i| {
            let k = |key| i.key_pressed(key);
            (k(Key::ArrowUp), k(Key::ArrowDown), k(Key::PageUp), k(Key::PageDown), k(Key::ArrowLeft), k(Key::ArrowRight), k(Key::Home), k(Key::End), k(Key::Plus) || k(Key::Equals), k(Key::Minus), k(Key::Num0))
        });
        let max_y = (doc.content_h - doc.view_h).max(0.0);
        let chain = crate::prefs::pdf_turn_pages();
        if up {
            if chain && doc.scroll.y <= 2.0 && doc.cur > 0 {
                doc.set_page(doc.cur - 1);
                doc.scroll.y = f32::MAX;
            } else {
                doc.scroll.y = (doc.scroll.y - 60.0).max(0.0);
            }
        }
        if down {
            if chain && doc.scroll.y >= max_y - 2.0 && doc.cur + 1 < pages {
                doc.set_page(doc.cur + 1);
            } else {
                doc.scroll.y = (doc.scroll.y + 60.0).min(max_y);
            }
        }
        if pgdn {
            if doc.scroll.y >= max_y - 2.0 && doc.cur + 1 < pages {
                doc.set_page(doc.cur + 1);
            } else {
                doc.scroll.y = (doc.scroll.y + doc.view_h - 40.0).min(max_y);
            }
        }
        if pgup {
            if doc.scroll.y <= 2.0 && doc.cur > 0 {
                doc.set_page(doc.cur - 1);
                doc.scroll.y = f32::MAX;
            } else {
                doc.scroll.y = (doc.scroll.y - doc.view_h + 40.0).max(0.0);
            }
        }
        if right {
            doc.set_page(doc.cur + 1);
        }
        if left && doc.cur > 0 {
            doc.set_page(doc.cur - 1);
        }
        if home {
            doc.set_page(0);
        }
        if end {
            doc.set_page(pages - 1);
        }
        if plus {
            doc.zoom = (doc.zoom * 1.25).min(5.0);
        }
        if minus {
            doc.zoom = (doc.zoom / 1.25).max(0.3);
        }
        if zero {
            doc.zoom = 1.0;
        }
    }

    ui.horizontal_wrapped(|ui| {
        if ui.add_enabled(doc.cur > 0, egui::Button::new("< Previous")).clicked() {
            doc.set_page(doc.cur - 1);
        }
        if ui.add_enabled(doc.cur + 1 < pages, egui::Button::new("Next >")).clicked() {
            doc.set_page(doc.cur + 1);
        }
        ui.label("Page");
        let g = ui.add(egui::TextEdit::singleline(&mut doc.goto_q).hint_text(format!("{}", doc.cur + 1)).desired_width(50.0));
        if g.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            if let Ok(n) = doc.goto_q.trim().parse::<u32>() {
                doc.set_page(n.max(1) - 1);
            }
            doc.goto_q.clear();
        }
        ui.label(format!("of {pages}"));
        if ui.button("-").on_hover_text("Zoom out (-)").clicked() {
            doc.zoom = (doc.zoom / 1.25).max(0.3);
        }
        ui.label(format!("{:.0}%", doc.zoom * 100.0));
        if ui.button("+").on_hover_text("Zoom in (+)").clicked() {
            doc.zoom = (doc.zoom * 1.25).min(5.0);
        }
        if ui.button("Fit width  (0)").clicked() {
            doc.zoom = 1.0;
        }
        ui.weak("PgUp / PgDn scroll and turn pages   Left / Right, Home / End pages   Esc or Q close");
    });
    ui.separator();

    if let Some(e) = &doc.error {
        ui.colored_label(egui::Color32::from_rgb(230, 90, 90), e);
        return close;
    }
    if pages == 0 {
        ui.label("Opening the document ...");
        return close;
    }

    let avail = ui.available_width() - 24.0;
    let width_px = ((avail * doc.zoom * ppp) as u32).clamp(300, 4000);
    doc.want(doc.cur, width_px);
    // read ahead: the next page
    if doc.pending.is_empty() && doc.cur + 1 < pages {
        doc.want(doc.cur + 1, width_px);
    }
    let tex = doc.texture(doc.cur, width_px).map(|t| (t.id(), t.size_vec2()));
    let out = egui::ScrollArea::both().auto_shrink(false).scroll_offset(doc.scroll).show(ui, |ui| match tex {
        Some((id, size)) => {
            // shown at the width that was asked for, whatever resolution is ready
            let shown = Vec2::new(avail * doc.zoom, avail * doc.zoom * size.y / size.x);
            let x = ((ui.available_width() - shown.x) / 2.0).max(0.0);
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(x);
                ui.add(egui::Image::new(SizedTexture::new(id, shown)));
            });
            shown.y + 8.0
        }
        None => {
            ui.label("Rendering the page ...");
            0.0
        }
    });
    doc.content_h = out.content_size.y;
    doc.view_h = out.inner_rect.height();
    doc.scroll = out.state.offset;
    close
}
