//! Info > System Info: the `gc-sysinfo` summary as a scrollable window with a section per topic.

use std::sync::mpsc::{self, Receiver, TryRecvError};

use eframe::egui::{self, Key, RichText};
use egui_phosphor::regular as icon;
use gc_sysinfo::SysInfo;

use crate::theme::Theme;

#[derive(Default)]
pub struct SysInfoWindow {
    pub open: bool,
    info: Option<SysInfo>,
    loading: Option<Receiver<SysInfo>>,
    /// Scroll position and visible height, so the arrow keys can move the view.
    scroll: f32,
    page: f32,
}

fn section_icon(title: &str) -> &'static str {
    match title {
        "System" => icon::DESKTOP_TOWER,
        "Firmware and security" => icon::SHIELD_CHECK,
        "Processor" => icon::CPU,
        "Memory" => icon::MEMORY,
        "Graphics and displays" => icon::MONITOR,
        "Battery" => icon::BATTERY_FULL,
        "Storage" => icon::HARD_DRIVES,
        _ => icon::WINDOWS_LOGO,
    }
}

impl SysInfoWindow {
    /// Opens the window and starts gathering (it can take a moment, so not on the UI thread).
    pub fn start(&mut self, ctx: &egui::Context) {
        self.open = true;
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(gc_sysinfo::collect());
            ctx.request_repaint();
        });
        self.loading = Some(rx);
    }

    pub fn show(&mut self, ctx: &egui::Context, theme: &Theme) {
        if !self.open {
            return;
        }
        if let Some(rx) = &self.loading {
            match rx.try_recv() {
                Ok(info) => {
                    self.info = Some(info);
                    self.loading = None;
                }
                Err(TryRecvError::Disconnected) => self.loading = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        let mut open = self.open;
        let mut refresh = false;
        egui::Window::new("System Info").open(&mut open).default_size([680.0, 580.0]).show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(info) = &self.info {
                    if ui.button(format!("{} Copy", icon::COPY)).on_hover_text("Copy everything as text").clicked() {
                        ctx.copy_text(info.to_text());
                    }
                }
                refresh = ui.add_enabled(self.loading.is_none(), egui::Button::new(format!("{} Refresh", icon::ARROWS_CLOCKWISE))).clicked();
                if self.loading.is_some() {
                    ui.spinner();
                    ui.label("Collecting...");
                }
            });
            ui.separator();
            let (up, down, pgup, pgdn, home, end) = ctx.input(|i| (i.key_pressed(Key::ArrowUp), i.key_pressed(Key::ArrowDown), i.key_pressed(Key::PageUp), i.key_pressed(Key::PageDown), i.key_pressed(Key::Home), i.key_pressed(Key::End)));
            let page = self.page.max(100.0);
            if up {
                self.scroll -= 40.0;
            }
            if down {
                self.scroll += 40.0;
            }
            if pgup {
                self.scroll -= page * 0.9;
            }
            if pgdn {
                self.scroll += page * 0.9;
            }
            if home {
                self.scroll = 0.0;
            }
            if end {
                self.scroll = 1.0e7;
            }
            self.scroll = self.scroll.max(0.0);
            let out = egui::ScrollArea::vertical().auto_shrink(false).vertical_scroll_offset(self.scroll).show(ui, |ui| {
                let Some(info) = &self.info else { return };
                for (n, section) in info.sections.iter().enumerate() {
                    ui.add_space(if n == 0 { 0.0 } else { 10.0 });
                    ui.label(RichText::new(format!("{}  {}", section_icon(section.title), section.title)).strong().size(15.0).color(theme.accent.0));
                    ui.add_space(2.0);
                    egui::Grid::new(("sysinfo", n)).num_columns(2).min_col_width(140.0).spacing([16.0, 3.0]).show(ui, |ui| {
                        for (label, value) in &section.rows {
                            ui.label(RichText::new(label).monospace().color(ui.visuals().weak_text_color()));
                            ui.add(egui::Label::new(RichText::new(value).monospace()).selectable(true));
                            ui.end_row();
                        }
                    });
                }
            });
            // Keep what egui really used (it clamps, and the mouse wheel and scroll bar move it too).
            self.scroll = out.state.offset.y;
            self.page = out.inner_rect.height();
        });
        self.open = open && !ctx.input(|i| i.key_pressed(Key::Escape));
        if refresh {
            self.start(ctx);
        }
    }
}
