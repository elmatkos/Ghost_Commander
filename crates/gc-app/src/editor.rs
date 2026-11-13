//! F4 text editor: a window around egui's multi-line `TextEdit` with syntax colours (see `syntax`),
//! line numbers, Ctrl+S, find / replace and a keep-or-discard question when closing a changed file.
//! Keeps the file's encoding (UTF-8, UTF-8 with BOM, UTF-16 with BOM) and line endings. Big or
//! binary files are not opened here (`open` returns `Ok(false)`: the caller falls back to Notepad).

use std::borrow::Cow;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui::{
    self,
    text::{CCursor, CCursorRange, LayoutJob},
    Color32, FontId, Id, Key, Modifiers, RichText, TextEdit, TextFormat,
};

use crate::syntax::{self, Lang, State};

/// Bigger files go to Notepad: the whole text is coloured and hashed on every change.
const MAX_BYTES: u64 = 2 * 1024 * 1024;
const FONT_SIZE: f32 = 13.0;
/// Inserted by the Tab key.
static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
const TAB: &str = "    ";

#[derive(Clone, Copy, PartialEq)]
enum Enc {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
}

impl Enc {
    fn name(self) -> &'static str {
        match self {
            Enc::Utf8 => "UTF-8",
            Enc::Utf8Bom => "UTF-8 BOM",
            Enc::Utf16Le => "UTF-16 LE",
            Enc::Utf16Be => "UTF-16 BE",
        }
    }
}

#[derive(Default)]
pub struct Editor {
    doc: Option<Doc>,
}

struct Doc {
    path: PathBuf,
    name: String,
    /// Always with `\n` line ends; `crlf` says what to write back.
    text: String,
    lang: Option<Lang>,
    enc: Enc,
    crlf: bool,
    /// Hash of the text as it is on disk.
    saved: u64,
    /// Coloured layout of the text with this hash.
    cache: (u64, Option<Arc<LayoutJob>>),
    /// Line numbers for this many lines.
    gutter: (usize, String),
    find_open: bool,
    focus_find: bool,
    find_q: String,
    repl: String,
    case: bool,
    msg: String,
    confirm: bool,
    fresh: bool,
    pending_scroll: Option<f32>,
    /// Distinguishes openings, so cursor and scroll position of an earlier file are not reused.
    serial: u64,
    scroll_y: f32,
    page: f32,
}

fn hash(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

fn decode(b: &[u8]) -> Option<(String, Enc)> {
    if let Some(r) = b.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8(r.to_vec()).ok().map(|s| (s, Enc::Utf8Bom));
    }
    if b.starts_with(&[0xFF, 0xFE]) || b.starts_with(&[0xFE, 0xFF]) {
        let le = b[0] == 0xFF;
        let units: Vec<u16> = b[2..].chunks_exact(2).map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) }).collect();
        return String::from_utf16(&units).ok().map(|s| (s, if le { Enc::Utf16Le } else { Enc::Utf16Be }));
    }
    if b.contains(&0) {
        return None;
    }
    String::from_utf8(b.to_vec()).ok().map(|s| (s, Enc::Utf8))
}

fn encode(text: &str, crlf: bool, enc: Enc) -> Vec<u8> {
    let text: Cow<str> = if crlf { Cow::Owned(text.replace('\n', "\r\n")) } else { Cow::Borrowed(text) };
    match enc {
        Enc::Utf8 => text.as_bytes().to_vec(),
        Enc::Utf8Bom => [&[0xEF, 0xBB, 0xBF][..], text.as_bytes()].concat(),
        Enc::Utf16Le => [0xFF, 0xFE].into_iter().chain(text.encode_utf16().flat_map(u16::to_le_bytes)).collect(),
        Enc::Utf16Be => [0xFE, 0xFF].into_iter().chain(text.encode_utf16().flat_map(u16::to_be_bytes)).collect(),
    }
}

fn byte_of(text: &str, chars: usize) -> usize {
    text.char_indices().nth(chars).map(|(b, _)| b).unwrap_or(text.len())
}

fn char_of(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].chars().count()
}

/// Selection of the editor as byte offsets (start, end); (0, 0) when nothing is known yet.
fn selection(ctx: &egui::Context, id: Id, text: &str) -> (usize, usize) {
    match TextEdit::load_state(ctx, id).and_then(|s| s.cursor.char_range()) {
        Some(r) => {
            let (a, b) = (r.primary.index.min(r.secondary.index), r.primary.index.max(r.secondary.index));
            (byte_of(text, a), byte_of(text, b))
        }
        None => (0, 0),
    }
}

fn set_selection(ctx: &egui::Context, id: Id, text: &str, a: usize, b: usize) {
    let mut st = TextEdit::load_state(ctx, id).unwrap_or_default();
    st.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(char_of(text, a)), CCursor::new(char_of(text, b)))));
    st.store(ctx, id);
    ctx.memory_mut(|m| m.request_focus(id));
}

/// First match of `q` at or after byte `from`, wrapping around to the start.
fn find_from(text: &str, q: &str, case: bool, from: usize) -> Option<(usize, usize)> {
    if q.is_empty() {
        return None;
    }
    let (h, n): (Cow<str>, Cow<str>) = if case { (Cow::Borrowed(text), Cow::Borrowed(q)) } else { (Cow::Owned(text.to_ascii_lowercase()), Cow::Owned(q.to_ascii_lowercase())) };
    let at = |s: usize| h.get(s..).and_then(|t| t.find(&*n)).map(|i| (s + i, s + i + n.len()));
    at(from).or_else(|| at(0))
}

fn replace_all(text: &str, q: &str, r: &str, case: bool) -> (String, usize) {
    if q.is_empty() {
        return (text.to_string(), 0);
    }
    let (h, n): (Cow<str>, Cow<str>) = if case { (Cow::Borrowed(text), Cow::Borrowed(q)) } else { (Cow::Owned(text.to_ascii_lowercase()), Cow::Owned(q.to_ascii_lowercase())) };
    let mut out = String::with_capacity(text.len());
    let (mut pos, mut count) = (0, 0);
    while let Some(i) = h[pos..].find(&*n) {
        out.push_str(&text[pos..pos + i]);
        out.push_str(r);
        pos += i + n.len();
        count += 1;
    }
    out.push_str(&text[pos..]);
    (out, count)
}

/// The whole text as one layout job, coloured per syntax span (plain when the language is unknown).
fn job_for(text: &str, lang: Option<Lang>, dark: bool, plain: Color32) -> LayoutJob {
    let font = FontId::monospace(FONT_SIZE);
    let fmt = |color: Color32| TextFormat { font_id: font.clone(), color, ..Default::default() };
    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    let Some(lang) = lang else {
        job.append(text, 0.0, fmt(plain));
        return job;
    };
    let lines: Vec<&str> = text.split('\n').collect();
    let starts = syntax::line_states(lang, lines.iter().copied());
    for (n, line) in lines.iter().enumerate() {
        let mut pos = 0;
        for (range, kind) in syntax::scan(lang, line, starts.get(n).copied().unwrap_or(State::Normal)).0 {
            if range.start > pos {
                job.append(&line[pos..range.start], 0.0, fmt(plain));
            }
            job.append(&line[range.clone()], 0.0, fmt(syntax::color(kind, dark)));
            pos = range.end;
        }
        if pos < line.len() {
            job.append(&line[pos..], 0.0, fmt(plain));
        }
        if n + 1 < lines.len() {
            job.append("\n", 0.0, fmt(plain));
        }
    }
    job
}

impl Doc {
    fn modified(&self) -> bool {
        hash(&self.text) != self.saved
    }

    fn save(&mut self) -> bool {
        match std::fs::write(&self.path, encode(&self.text, self.crlf, self.enc)) {
            Ok(()) => {
                self.saved = hash(&self.text);
                self.msg = "Saved.".into();
                true
            }
            Err(e) => {
                self.msg = format!("Save failed: {e}");
                false
            }
        }
    }

    fn scroll_to(&mut self, byte: usize, row_h: f32, page: f32) {
        let line = self.text[..byte.min(self.text.len())].matches('\n').count();
        self.pending_scroll = Some((line as f32 * row_h - page / 3.0).max(0.0));
    }

    fn find_next(&mut self, ctx: &egui::Context, id: Id, row_h: f32, page: f32) {
        let (_, end) = selection(ctx, id, &self.text);
        match find_from(&self.text, &self.find_q, self.case, end) {
            Some((a, b)) => {
                set_selection(ctx, id, &self.text, a, b);
                self.scroll_to(a, row_h, page);
                self.msg.clear();
            }
            None => self.msg = "Not found.".into(),
        }
    }

    fn replace_one(&mut self, ctx: &egui::Context, id: Id, row_h: f32, page: f32) {
        let (a, b) = selection(ctx, id, &self.text);
        let selected = self.text.get(a..b).unwrap_or("");
        let same = if self.case { selected == self.find_q } else { selected.eq_ignore_ascii_case(&self.find_q) };
        if a < b && same {
            self.text.replace_range(a..b, &self.repl);
            let end = a + self.repl.len();
            set_selection(ctx, id, &self.text, end, end);
        }
        self.find_next(ctx, id, row_h, page);
    }

    fn replace_everything(&mut self) {
        let (t, n) = replace_all(&self.text, &self.find_q, &self.repl, self.case);
        self.msg = format!("{n} replaced.");
        self.text = t;
    }
}

impl Editor {
    pub fn is_open(&self) -> bool {
        self.doc.is_some()
    }

    /// Opens `path` in the editor. `Ok(false)`: not suitable (too big, binary or not UTF-8/UTF-16) - use another editor.
    pub fn open(&mut self, path: &Path) -> std::io::Result<bool> {
        if std::fs::metadata(path)?.len() > MAX_BYTES {
            return Ok(false);
        }
        let Some((text, enc)) = decode(&std::fs::read(path)?) else { return Ok(false) };
        let crlf = text.contains("\r\n");
        let text = if crlf { text.replace("\r\n", "\n") } else { text };
        let saved = hash(&text);
        self.doc = Some(Doc {
            path: path.to_path_buf(),
            name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            lang: syntax::detect(path),
            text,
            enc,
            crlf,
            saved,
            cache: (0, None),
            gutter: (0, String::new()),
            find_open: false,
            focus_find: false,
            find_q: String::new(),
            repl: String::new(),
            case: false,
            msg: String::new(),
            confirm: false,
            fresh: true,
            pending_scroll: None,
            serial: SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            scroll_y: 0.0,
            page: 400.0,
        });
        Ok(true)
    }

    pub fn show(&mut self, ctx: &egui::Context, dark: bool) {
        let Some(d) = &mut self.doc else { return };
        let id = Id::new(("editor_text", d.serial));
        let row_h = ctx.fonts(|f| f.row_height(&FontId::monospace(FONT_SIZE)));
        let modified = d.modified();

        let (ctrl_s, ctrl_f, esc, f3) = ctx.input(|i| (i.modifiers.command && i.key_pressed(Key::S), i.modifiers.command && i.key_pressed(Key::F), i.key_pressed(Key::Escape), i.key_pressed(Key::F3)));
        let mut want_close = false;
        if ctrl_s {
            d.save();
        }
        if ctrl_f {
            d.find_open = true;
            d.focus_find = true;
        }
        if esc && !d.confirm {
            if d.find_open {
                d.find_open = false;
            } else {
                want_close = true;
            }
        }
        // Tab inserts spaces (lock_focus keeps egui from moving the focus away).
        if ctx.memory(|m| m.has_focus(id)) && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Tab)) {
            let (a, b) = selection(ctx, id, &d.text);
            d.text.replace_range(a..b, TAB);
            set_selection(ctx, id, &d.text, a + TAB.len(), a + TAB.len());
        }

        // PageUp / PageDown: TextEdit ignores them; move the cursor and the view by a page.
        if ctx.memory(|m| m.has_focus(id)) {
            let shift = ctx.input(|i| i.modifiers.shift);
            let (down, up) = ctx.input_mut(|i| {
                (i.consume_key(Modifiers::NONE, Key::PageDown) | i.consume_key(Modifiers::SHIFT, Key::PageDown), i.consume_key(Modifiers::NONE, Key::PageUp) | i.consume_key(Modifiers::SHIFT, Key::PageUp))
            });
            if down || up {
                let rows = (((d.page / row_h) as isize) - 1).max(1);
                let delta = if down { rows } else { -rows };
                let mut st = TextEdit::load_state(ctx, id).unwrap_or_default();
                let (cur, anchor) = st.cursor.char_range().map(|r| (r.primary.index, r.secondary.index)).unwrap_or((0, 0));
                let b = byte_of(&d.text, cur);
                let line = d.text[..b].matches('\n').count();
                let col = d.text[..b].rsplit('\n').next().map(|l| l.chars().count()).unwrap_or(0);
                let last = d.text.matches('\n').count();
                let tl = (line as isize + delta).clamp(0, last as isize) as usize;
                let start = if tl == 0 { 0 } else { d.text.match_indices('\n').nth(tl - 1).map(|(i, _)| i + 1).unwrap_or(0) };
                let len = d.text[start..].split('\n').next().map(|l| l.chars().count()).unwrap_or(0);
                let new = char_of(&d.text, start) + col.min(len);
                let sec = if shift { anchor } else { new };
                st.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(sec), CCursor::new(new))));
                st.store(ctx, id);
                d.pending_scroll = Some((d.scroll_y + delta as f32 * row_h).max(0.0));
            }
        }

        let (line, col) = match TextEdit::load_state(ctx, id).and_then(|s| s.cursor.char_range()) {
            Some(r) => {
                let b = byte_of(&d.text, r.primary.index);
                let before = &d.text[..b];
                (before.matches('\n').count() + 1, before.rsplit('\n').next().map(|l| l.chars().count()).unwrap_or(0) + 1)
            }
            None => (1, 1),
        };
        let mut page = 400.0;
        let mut scroll_y = 0.0;
        let title = format!("Edit: {}{}{}", d.name, d.lang.map(|l| format!("  [{}]", l.name())).unwrap_or_default(), if modified { "  *" } else { "" });
        let closed = crate::fullscreen::show(ctx, "editor", &title, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Save  (Ctrl+S)").clicked() {
                    d.save();
                }
                if ui.button("Find / Replace  (Ctrl+F)").clicked() {
                    d.find_open = !d.find_open;
                    d.focus_find = d.find_open;
                }
                ui.weak(format!("Ln {line}, Col {col}   {}   {}", d.enc.name(), if d.crlf { "CRLF" } else { "LF" }));
                if !d.msg.is_empty() {
                    ui.label(&d.msg);
                }
            });
            if d.confirm {
                ui.horizontal(|ui| {
                    ui.colored_label(Color32::from_rgb(230, 150, 30), "The file has unsaved changes.");
                    if ui.button("Save and close").clicked() && d.save() {
                        d.confirm = false;
                        want_close = true;
                    }
                    if ui.button("Discard changes").clicked() {
                        d.saved = hash(&d.text);
                        d.confirm = false;
                        want_close = true;
                    }
                    if ui.button("Cancel").clicked() {
                        d.confirm = false;
                    }
                });
            }
            page = ui.available_height();
            if d.find_open {
                ui.horizontal(|ui| {
                    ui.label("Find");
                    let f = ui.add(TextEdit::singleline(&mut d.find_q).id(Id::new("editor_find")).desired_width(180.0));
                    if std::mem::take(&mut d.focus_find) {
                        f.request_focus();
                    }
                    let enter = f.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    ui.label("Replace");
                    ui.add(TextEdit::singleline(&mut d.repl).desired_width(180.0));
                    ui.checkbox(&mut d.case, "Match case");
                    if ui.button("Next  (F3)").clicked() || enter || f3 {
                        d.find_next(ctx, id, row_h, page);
                    }
                    if ui.button("Replace").clicked() {
                        d.replace_one(ctx, id, row_h, page);
                    }
                    if ui.button("Replace all").clicked() {
                        d.replace_everything();
                    }
                });
            }
            ui.separator();

            let mut scroll = egui::ScrollArea::both().auto_shrink(false).id_salt(("editor_scroll", d.serial));
            if let Some(y) = d.pending_scroll.take() {
                scroll = scroll.vertical_scroll_offset(y);
            }
            let rows = ((ui.available_height() / row_h) as usize).max(3);
            let view_w = ui.available_width();
            let plain = ui.visuals().text_color();
            let weak = ui.visuals().weak_text_color();
            let Doc { text, cache, lang, gutter, fresh, .. } = d;
            let out = scroll.show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let n = text.matches('\n').count() + 1;
                    if gutter.0 != n {
                        let w = n.to_string().len();
                        *gutter = (n, (1..=n).map(|i| format!("{i:>w$}")).collect::<Vec<_>>().join("\n"));
                    }
                    ui.add(egui::Label::new(RichText::new(gutter.1.as_str()).monospace().size(FONT_SIZE).color(weak)).selectable(false));
                    let lang = *lang;
                    let mut layouter = |ui: &egui::Ui, s: &str, _wrap: f32| {
                        let key = hash(s);
                        if cache.0 != key || cache.1.is_none() {
                            *cache = (key, Some(Arc::new(job_for(s, lang, dark, plain))));
                        }
                        let job = cache.1.as_ref().map(|j| (**j).clone()).unwrap_or_default();
                        ui.fonts(|f| f.layout_job(job))
                    };
                    let resp = ui.add(
                        TextEdit::multiline(text)
                            .id(id)
                            .frame(false)
                            .margin(egui::Margin::ZERO)
                            .lock_focus(true)
                            .desired_rows(rows)
                            .desired_width((view_w - 70.0).max(200.0))
                            .layouter(&mut layouter),
                    );
                    if std::mem::take(fresh) {
                        resp.request_focus();
                        set_selection(ui.ctx(), id, text, 0, 0);
                    }
                });
            });
            scroll_y = out.state.offset.y;
        });
        d.page = page;
        d.scroll_y = scroll_y;
        if closed {
            want_close = true;
        }
        if want_close {
            if d.modified() {
                d.confirm = true;
            } else {
                self.doc = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodings_round_trip() {
        for enc in [Enc::Utf8, Enc::Utf8Bom, Enc::Utf16Le, Enc::Utf16Be] {
            let bytes = encode("a\nž€\n", true, enc);
            let (text, got) = decode(&bytes).unwrap();
            assert!(got == enc);
            assert_eq!(text, "a\r\nž€\r\n");
        }
        assert!(decode(b"MZ\0\0").is_none());
        assert!(decode(&[0xFF, 0x41]).is_none());
    }

    #[test]
    fn find_and_replace() {
        assert_eq!(find_from("Hello hello", "hello", false, 1), Some((6, 11)));
        assert_eq!(find_from("Hello hello", "hello", true, 0), Some((6, 11)));
        assert_eq!(find_from("abc", "b", true, 2), Some((1, 2)), "wraps around");
        assert_eq!(replace_all("aXbxc", "x", "--", false), ("a--b--c".to_string(), 2));
        assert_eq!(replace_all("aXbxc", "x", "-", true), ("aXb-c".to_string(), 1));
    }
}
