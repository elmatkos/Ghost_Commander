//! F3 viewer, a window of its own that opens over the main window: read-only text with syntax colours (only the rows on screen are
//! coloured), a hex dump for binary files (H toggles), find (F7 / Ctrl+F, F3 next, Shift+F3 previous)
//! and go to line / offset (Ctrl+G).

use std::io::Read;
use std::ops::Range;
use std::path::Path;

use eframe::egui::{self, text::LayoutJob, Color32, FontId, Key, TextFormat, Vec2};

use crate::fullscreen::Link;
use crate::syntax::{self, Kind, Lang, State};
use crate::textenc::{self, Enc};

/// Files are cut after this many bytes (the title says so).
const MAX_BYTES: u64 = 8 * 1024 * 1024;
const FONT_SIZE: f32 = 13.0;
/// Bytes per row of the hex dump.
const HEX_W: usize = 16;

#[derive(Default)]
pub struct Viewer {
    open: Option<Doc>,
    /// Opened during the current frame.
    fresh: bool,
    link: Link,
}

pub struct Doc {
    pub title: String,
    bytes: Vec<u8>,
    /// Text lines (empty for a binary file).
    lines: Vec<String>,
    lang: Option<Lang>,
    /// What each line starts inside of (block comments, here-strings, ...); empty without a language.
    starts: Vec<State>,
    /// The file decoded as text; false for a binary file (hex only).
    text_ok: bool,
    hex: bool,
    scroll: Vec2,
    page: f32,
    find_open: bool,
    focus_find: bool,
    find_q: String,
    case: bool,
    /// Last match: (row, start, end) - byte range inside the line, or file offsets in the hex dump.
    hit: Option<(usize, usize, usize)>,
    msg: String,
    goto_open: bool,
    focus_goto: bool,
    goto_q: String,
}

impl Doc {
    fn rows(&self) -> usize {
        if self.hex { self.bytes.len().div_ceil(HEX_W) } else { self.lines.len() }
    }

    fn scroll_to_row(&mut self, row: usize, row_h: f32) {
        let max = (self.rows() as f32 * row_h - self.page).max(0.0);
        self.scroll.y = (row as f32 * row_h - self.page / 3.0).clamp(0.0, max);
    }

    /// Finds the next (or previous) match of the search text and scrolls to it.
    fn find(&mut self, forward: bool, row_h: f32, char_w: f32) {
        if self.find_q.is_empty() {
            return;
        }
        self.msg.clear();
        let found = if self.hex { self.find_hex(forward, row_h) } else { self.find_text(forward, row_h) };
        match found {
            Some((row, s, e)) => {
                self.hit = Some((row, s, e));
                self.scroll_to_row(row, row_h);
                if self.hex {
                    self.msg = format!("offset 0x{s:X}");
                    self.scroll.x = 0.0;
                } else {
                    let col = self.lines[row][..s].chars().count() as f32;
                    self.scroll.x = if col * char_w > 400.0 { col * char_w - 200.0 } else { 0.0 };
                }
            }
            None => self.msg = "Not found.".into(),
        }
    }

    fn find_text(&self, forward: bool, row_h: f32) -> Option<(usize, usize, usize)> {
        let n = self.lines.len();
        if n == 0 {
            return None;
        }
        let q = if self.case { self.find_q.clone() } else { self.find_q.to_ascii_lowercase() };
        let top = ((self.scroll.y / row_h) as usize).min(n - 1);
        let (line, col) = match self.hit {
            Some((l, s, e)) => (l, if forward { e } else { s }),
            None => (top, 0),
        };
        for step in 0..=n {
            let li = if forward { (line + step) % n } else { (line + n - step % n) % n };
            let hay = if self.case { std::borrow::Cow::Borrowed(self.lines[li].as_str()) } else { std::borrow::Cow::Owned(self.lines[li].to_ascii_lowercase()) };
            if forward {
                let from = if step == 0 { col.min(hay.len()) } else { 0 };
                if let Some(p) = hay.get(from..).and_then(|h| h.find(&q)) {
                    return Some((li, from + p, from + p + q.len()));
                }
            } else {
                let to = if step == 0 { col.min(hay.len()) } else { hay.len() };
                if let Some(p) = hay.get(..to).and_then(|h| h.rfind(&q)) {
                    return Some((li, p, p + q.len()));
                }
            }
        }
        None
    }

    fn find_hex(&self, forward: bool, row_h: f32) -> Option<(usize, usize, usize)> {
        let q = self.find_q.as_bytes();
        let b = &self.bytes;
        let eq = |w: &[u8]| if self.case { w == q } else { w.eq_ignore_ascii_case(q) };
        let pos_in = |range: Range<usize>| b.get(range.clone()).and_then(|s| s.windows(q.len()).position(|w| eq(w))).map(|p| p + range.start);
        let rpos_in = |range: Range<usize>| b.get(range.clone()).and_then(|s| s.windows(q.len()).rposition(|w| eq(w))).map(|p| p + range.start);
        let from = match self.hit {
            Some((_, s, e)) => if forward { e } else { s },
            None => (self.scroll.y / row_h) as usize * HEX_W,
        };
        let p = if forward { pos_in(from..b.len()).or_else(|| pos_in(0..b.len())) } else { rpos_in(0..from.min(b.len())).or_else(|| rpos_in(0..b.len())) }?;
        Some((p / HEX_W, p, p + q.len()))
    }

    fn go_to(&mut self, row_h: f32) {
        let q = self.goto_q.trim();
        if self.hex {
            match usize::from_str_radix(q.trim_start_matches("0x").trim_start_matches("0X"), 16) {
                Ok(off) => {
                    self.scroll_to_row(off / HEX_W, row_h);
                    self.msg.clear();
                }
                Err(_) => self.msg = "Type an offset in hex.".into(),
            }
        } else {
            match q.parse::<usize>() {
                Ok(l) if l >= 1 => {
                    self.scroll_to_row(l.min(self.lines.len().max(1)) - 1, row_h);
                    self.scroll.y = (self.scroll.y + self.page / 3.0 - row_h * 2.0).max(0.0);
                    self.msg.clear();
                }
                _ => self.msg = "Type a line number.".into(),
            }
        }
    }
}

impl Viewer {
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Opens `path` in the viewer window.
    pub fn open(&mut self, path: &Path) -> std::io::Result<()> {
        self.open = Some(load(path)?);
        self.fresh = true;
        Ok(())
    }

    pub fn show(&mut self, ctx: &egui::Context, dark: bool) {
        let Some(doc) = &mut self.open else { return };
        // The F3 that opened the viewer is still "pressed" in this very frame: ignore it once.
        let fresh = std::mem::take(&mut self.fresh);
        if fresh {
            self.link = Link::default();
        }
        let title = format!("View: {}", doc.title);
        let builder = self.link.builder(ctx, &title);
        let id = egui::ViewportId::from_hash_of("gc_viewer");
        let (mut close, mut minimized) = (false, false);
        ctx.show_viewport_immediate(id, builder, |ctx, class| {
            minimized = ctx.input(|i| i.viewport().minimized).unwrap_or(false);
            close = draw(doc, fresh, ctx, class, dark, &title);
        });
        self.link.sync(ctx, id, minimized);
        if close {
            self.open = None;
            ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Focus);
        }
    }
}

/// Draws the viewer window; returns true when it should close.
fn draw(doc: &mut Doc, fresh: bool, ctx: &egui::Context, class: egui::ViewportClass, dark: bool, title: &str) -> bool {
    let mut close = ctx.input(|i| i.viewport().close_requested());
    if crate::fullscreen::show(ctx, class, title, |ui| {
        if doc.ui(ui, fresh, Keys::All, dark) {
            close = true;
        }
    }) {
        close = true;
    }
    close
}

/// Which keys a viewer reacts to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Keys {
    /// None (it sits beside something that uses the keyboard).
    None,
    /// Scrolling only, while no text field has the focus.
    Scroll,
    /// All of them: scrolling, find, go to, hex, close.
    All,
}

impl Doc {
    /// Draws the toolbar and the text (or hex dump) into `ui`; returns true when a key asked to close it.
    pub fn ui(&mut self, ui: &mut egui::Ui, fresh: bool, keys: Keys, dark: bool) -> bool {
        let ctx = ui.ctx().clone();
        let ctx = &ctx;
        let typing = ctx.wants_keyboard_input();
        let font = FontId::monospace(FONT_SIZE);
        let row_h = ctx.fonts(|f| f.row_height(&font));
        let char_w = ctx.fonts(|f| f.glyph_width(&font, 'M'));
        let mut close = false;

        let k = |ctx: &egui::Context, key: Key| ctx.input(|i| i.key_pressed(key));
        let (ctrl, shift) = ctx.input(|i| (i.modifiers.command, i.modifiers.shift));
        if keys == Keys::All && !fresh {
            if k(ctx, Key::F7) || (ctrl && k(ctx, Key::F)) {
                self.find_open = true;
                self.focus_find = true;
            } else if ctrl && k(ctx, Key::G) {
                self.goto_open = true;
                self.focus_goto = true;
            } else if k(ctx, Key::Escape) {
                if self.find_open || self.goto_open {
                    self.find_open = false;
                    self.goto_open = false;
                } else {
                    close = true;
                }
            } else if k(ctx, Key::F3) {
                if self.find_q.is_empty() {
                    close = true;
                } else {
                    self.find(!shift, row_h, char_w);
                }
            } else if !typing && k(ctx, Key::Q) {
                close = true;
            } else if !typing && !ctrl && self.text_ok && k(ctx, Key::H) {
                self.hex = !self.hex;
                self.hit = None;
                self.scroll = Vec2::ZERO;
            }
        }
        // Keyboard scrolling: arrows by a row / a few characters, PgUp/PgDn by a page, Home/End to the ends.
        if keys != Keys::None && !close && !typing {
            let total = self.rows() as f32 * row_h;
            let (dy, dx, home, end) = ctx.input(|i| {
                let k = |key| i.key_pressed(key);
                let mut dy = 0.0;
                let mut dx = 0.0;
                if k(Key::ArrowDown) { dy += row_h; }
                if k(Key::ArrowUp) { dy -= row_h; }
                if k(Key::PageDown) || k(Key::Space) { dy += self.page - row_h; }
                if k(Key::PageUp) { dy -= self.page - row_h; }
                if k(Key::ArrowRight) { dx += 40.0; }
                if k(Key::ArrowLeft) { dx -= 40.0; }
                (dy, dx, k(Key::Home), k(Key::End))
            });
            self.scroll.y = if home { 0.0 } else if end { total } else { self.scroll.y + dy };
            self.scroll.x = if home { 0.0 } else { (self.scroll.x + dx).max(0.0) };
            self.scroll.y = self.scroll.y.clamp(0.0, (total - self.page).max(0.0));
        }

        let scroll = self.scroll;
        let page: f32;
        let mut offset = scroll;
        {
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled(self.text_ok, egui::Button::new(if self.hex { "Text  (H)" } else { "Hex  (H)" })).clicked() {
                    self.hex = !self.hex;
                    self.hit = None;
                    self.scroll = Vec2::ZERO;
                    offset = Vec2::ZERO;
                }
                if ui.button("Find  (F7)").clicked() {
                    self.find_open = !self.find_open;
                    self.focus_find = self.find_open;
                }
                if ui.button(if self.hex { "Go to offset  (Ctrl+G)" } else { "Go to line  (Ctrl+G)" }).clicked() {
                    self.goto_open = !self.goto_open;
                    self.focus_goto = self.goto_open;
                }
                ui.weak(if self.hex { format!("{} bytes", self.bytes.len()) } else { format!("{} lines", self.lines.len()) });
                if !self.msg.is_empty() {
                    ui.label(&self.msg);
                }
            });
            if self.find_open {
                ui.horizontal(|ui| {
                    ui.label("Find");
                    let f = ui.add(egui::TextEdit::singleline(&mut self.find_q).id(egui::Id::new("viewer_find")).desired_width(240.0));
                    if std::mem::take(&mut self.focus_find) {
                        f.request_focus();
                    }
                    if f.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        self.find(!ui.input(|i| i.modifiers.shift), row_h, char_w);
                        f.request_focus();
                    }
                    ui.checkbox(&mut self.case, "Match case");
                    if ui.button("Previous  (Shift+F3)").clicked() {
                        self.find(false, row_h, char_w);
                    }
                    if ui.button("Next  (F3)").clicked() {
                        self.find(true, row_h, char_w);
                    }
                });
            }
            if self.goto_open {
                ui.horizontal(|ui| {
                    ui.label(if self.hex { "Offset (hex)" } else { "Line" });
                    let g = ui.add(egui::TextEdit::singleline(&mut self.goto_q).id(egui::Id::new("viewer_goto")).desired_width(120.0));
                    if std::mem::take(&mut self.focus_goto) {
                        g.request_focus();
                    }
                    let enter = g.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    if ui.button("Go").clicked() || enter {
                        self.go_to(row_h);
                        self.goto_open = false;
                    }
                });
            }
            if keys == Keys::All {
                ui.weak("Up/Down, PgUp/PgDn, Space, Home/End, Left/Right scroll   F7 find   F3 / Shift+F3 next / previous   Ctrl+G go to   H hex / text   Esc or Q close");
            }
            ui.separator();
            // find / go to may have moved the scroll position
            offset = if self.scroll != scroll { self.scroll } else { offset };
            let plain = ui.visuals().text_color();
            let weak = ui.visuals().weak_text_color();
            let d = &*self;
            let out = egui::ScrollArea::both().auto_shrink(false).scroll_offset(offset).show_rows(ui, row_h, d.rows(), |ui, range| {
                for n in range {
                    let job = if d.hex {
                        hex_row(&d.bytes, n, d.hit, dark, plain, weak)
                    } else {
                        let hit = d.hit.filter(|h| h.0 == n).map(|h| h.1..h.2);
                        colored(&d.lines[n], d.lang, d.starts.get(n).copied().unwrap_or_default(), dark, plain, hit)
                    };
                    ui.add(egui::Label::new(job).extend());
                }
            });
            offset = out.state.offset;
            page = out.inner_rect.height();
        }
        if !close {
            self.scroll = offset;
            self.page = page;
        }
        close
    }

}

/// Loads `path` for viewing. Text is decoded (UTF-8, UTF-16 with BOM, else Windows-1250);
    /// a file with NUL bytes opens as a hex dump.
pub fn load(path: &Path) -> std::io::Result<Doc> {
        let mut bytes = Vec::new();
        let file = std::fs::File::open(path)?;
        let total = file.metadata()?.len();
        file.take(MAX_BYTES).read_to_end(&mut bytes)?;
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut title = if total > MAX_BYTES { format!("{name} (first {} MB)", MAX_BYTES >> 20) } else { name };
        let decoded = textenc::decode(&bytes);
        let (lines, lang, starts, text_ok) = match &decoded {
            Some((text, enc)) => {
                let lines: Vec<String> = text.lines().map(|l| l.replace('\t', "    ")).collect();
                let lang = syntax::detect(path);
                let starts = lang.map(|l| syntax::line_states(l, lines.iter().map(String::as_str))).unwrap_or_default();
                if let Some(l) = lang {
                    title = format!("{title}  [{}]", l.name());
                }
                if *enc != Enc::Utf8 {
                    title = format!("{title}  ({})", enc.name());
                }
                (lines, lang, starts, true)
            }
            None => {
                title = format!("{title}  (binary, {} bytes)", bytes.len());
                (Vec::new(), None, Vec::new(), false)
            }
        };
    Ok(Doc {
            title,
            bytes,
            lines,
            lang,
            starts,
            text_ok,
            hex: !text_ok,
            scroll: Vec2::ZERO,
            page: 400.0,
            find_open: false,
            focus_find: false,
            find_q: String::new(),
            case: false,
            hit: None,
            msg: String::new(),
            goto_open: false,
            focus_goto: false,
            goto_q: String::new(),
    })
}

fn hit_color(dark: bool) -> Color32 {
    if dark { Color32::from_rgb(110, 90, 20) } else { Color32::from_rgb(255, 230, 120) }
}

/// One text line as a layout job: a colour per syntax span, the search hit on a background.
fn colored(line: &str, lang: Option<Lang>, st: State, dark: bool, plain: Color32, hit: Option<Range<usize>>) -> LayoutJob {
    let font = FontId::monospace(FONT_SIZE);
    let mut segments: Vec<(Range<usize>, Color32)> = Vec::new();
    let mut pos = 0;
    if let Some(lang) = lang {
        for (range, kind) in syntax::scan(lang, line, st).0 {
            if range.start > pos {
                segments.push((pos..range.start, plain));
            }
            pos = range.end;
            segments.push((range.clone(), syntax::color(kind, dark)));
        }
    }
    if pos < line.len() {
        segments.push((pos..line.len(), plain));
    }
    let mut job = LayoutJob::default();
    for (range, color) in segments {
        // split at the hit borders
        let mut cuts = vec![range.start];
        if let Some(h) = &hit {
            for c in [h.start, h.end] {
                if c > range.start && c < range.end && line.is_char_boundary(c) {
                    cuts.push(c);
                }
            }
        }
        cuts.push(range.end);
        cuts.sort_unstable();
        for w in cuts.windows(2) {
            let inside = hit.as_ref().is_some_and(|h| w[0] >= h.start && w[1] <= h.end);
            let background = if inside { hit_color(dark) } else { Color32::TRANSPARENT };
            job.append(&line[w[0]..w[1]], 0.0, TextFormat { font_id: font.clone(), color, background, ..Default::default() });
        }
    }
    if line.is_empty() {
        job.append(" ", 0.0, TextFormat { font_id: font, color: plain, ..Default::default() });
    }
    job
}

/// One row of the hex dump: offset, 16 bytes, ASCII.
fn hex_row(bytes: &[u8], row: usize, hit: Option<(usize, usize, usize)>, dark: bool, plain: Color32, weak: Color32) -> LayoutJob {
    let font = FontId::monospace(FONT_SIZE);
    let fmt = |color: Color32, background: Color32| TextFormat { font_id: font.clone(), color, background, ..Default::default() };
    let off = row * HEX_W;
    let chunk = &bytes[off..(off + HEX_W).min(bytes.len())];
    let mut job = LayoutJob::default();
    job.append(&format!("{off:08X}  "), 0.0, fmt(weak, Color32::TRANSPARENT));
    let in_hit = |i: usize| hit.is_some_and(|h| i >= h.1 && i < h.2);
    for i in 0..HEX_W {
        let bg = |at: usize| if in_hit(at) { hit_color(dark) } else { Color32::TRANSPARENT };
        match chunk.get(i) {
            Some(b) => job.append(&format!("{b:02X}"), 0.0, fmt(plain, bg(off + i))),
            None => job.append("  ", 0.0, fmt(plain, Color32::TRANSPARENT)),
        }
        job.append(if i == 7 { "  " } else { " " }, 0.0, fmt(plain, Color32::TRANSPARENT));
    }
    job.append(" ", 0.0, fmt(plain, Color32::TRANSPARENT));
    let ascii_color = syntax::color(Kind::Number, dark);
    for (i, &b) in chunk.iter().enumerate() {
        let c = if (0x20..0x7F).contains(&b) { b as char } else { '.' };
        let bg = if in_hit(off + i) { hit_color(dark) } else { Color32::TRANSPARENT };
        job.append(&c.to_string(), 0.0, fmt(if c == '.' { weak } else { ascii_color }, bg));
    }
    job
}
