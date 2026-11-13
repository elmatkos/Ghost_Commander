//! Ctrl+Shift+K: two text files side by side with the differences coloured (a line diff). Opens as
//! a separate window over GC like the viewer. Binary files are only compared byte by byte.

use std::path::{Path, PathBuf};

use eframe::egui::{self, Align2, Color32, FontId, Key, Sense};
use similar::{capture_diff_slices, Algorithm, DiffOp};

use crate::fullscreen::Link;

/// Files bigger than this are not diffed (the diff and the row list live in memory).
const MAX_BYTES: u64 = 16 * 1024 * 1024;
const ROW_H: f32 = 18.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Same,
    Changed,
    OnlyLeft,
    OnlyRight,
}

/// One aligned row: a line of each file (with its number) or a gap.
#[derive(Debug)]
struct Row {
    left: Option<(usize, String)>,
    right: Option<(usize, String)>,
    kind: Kind,
}

/// Lines of a file. A trailing line break does not make an extra empty line.
fn lines_of(text: &str) -> Vec<String> {
    text.lines().map(str::to_owned).collect()
}

/// The text of a line as it is compared (white space / case can be ignored).
fn key(line: &str, ignore_ws: bool, ignore_case: bool) -> String {
    let mut s = if ignore_ws { line.split_whitespace().collect::<Vec<_>>().join(" ") } else { line.to_owned() };
    if ignore_case {
        s = s.to_lowercase();
    }
    s
}

/// Aligns the lines of both files: equal lines, changed pairs, lines only on one side.
fn diff_rows(left: &[String], right: &[String], ignore_ws: bool, ignore_case: bool) -> Vec<Row> {
    let a: Vec<String> = left.iter().map(|l| key(l, ignore_ws, ignore_case)).collect();
    let b: Vec<String> = right.iter().map(|l| key(l, ignore_ws, ignore_case)).collect();
    let mut rows = Vec::with_capacity(left.len().max(right.len()));
    let l = |i: usize| Some((i + 1, left[i].clone()));
    let r = |i: usize| Some((i + 1, right[i].clone()));
    for op in capture_diff_slices(Algorithm::Myers, &a, &b) {
        match op {
            DiffOp::Equal { old_index, new_index, len } => {
                for k in 0..len {
                    rows.push(Row { left: l(old_index + k), right: r(new_index + k), kind: Kind::Same });
                }
            }
            DiffOp::Delete { old_index, old_len, .. } => {
                for k in 0..old_len {
                    rows.push(Row { left: l(old_index + k), right: None, kind: Kind::OnlyLeft });
                }
            }
            DiffOp::Insert { new_index, new_len, .. } => {
                for k in 0..new_len {
                    rows.push(Row { left: None, right: r(new_index + k), kind: Kind::OnlyRight });
                }
            }
            DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                for k in 0..old_len.max(new_len) {
                    let (lk, rk) = ((k < old_len).then(|| old_index + k), (k < new_len).then(|| new_index + k));
                    let kind = match (lk, rk) {
                        (Some(_), Some(_)) => Kind::Changed,
                        (Some(_), None) => Kind::OnlyLeft,
                        _ => Kind::OnlyRight,
                    };
                    rows.push(Row { left: lk.and_then(l), right: rk.and_then(r), kind });
                }
            }
        }
    }
    rows
}

/// Row numbers where a block of differences starts.
fn block_starts(rows: &[Row]) -> Vec<usize> {
    let mut v = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        if r.kind != Kind::Same && (i == 0 || rows[i - 1].kind == Kind::Same) {
            v.push(i);
        }
    }
    v
}

struct Doc {
    left_path: PathBuf,
    right_path: PathBuf,
    left_lines: Vec<String>,
    right_lines: Vec<String>,
    rows: Vec<Row>,
    blocks: Vec<usize>,
    cur: Option<usize>,
    ignore_ws: bool,
    ignore_case: bool,
    /// Why there is nothing to show side by side (binary, too big, unreadable).
    message: Option<String>,
    offset: f32,
    view_h: f32,
    want: Option<f32>,
}

impl Doc {
    fn load(left: &Path, right: &Path) -> Self {
        let mut d = Self {
            left_path: left.to_path_buf(),
            right_path: right.to_path_buf(),
            left_lines: Vec::new(),
            right_lines: Vec::new(),
            rows: Vec::new(),
            blocks: Vec::new(),
            cur: None,
            ignore_ws: false,
            ignore_case: false,
            message: None,
            offset: 0.0,
            view_h: 400.0,
            want: None,
        };
        d.message = d.read().err();
        d.recompute();
        d
    }

    fn read(&mut self) -> Result<(), String> {
        for p in [&self.left_path, &self.right_path] {
            let len = std::fs::metadata(p).map_err(|e| format!("{}: {e}", p.display()))?.len();
            if len > MAX_BYTES {
                return Err(format!("{} is bigger than {} MB: too big to compare here.", p.display(), MAX_BYTES >> 20));
            }
        }
        let a = std::fs::read(&self.left_path).map_err(|e| format!("{}: {e}", self.left_path.display()))?;
        let b = std::fs::read(&self.right_path).map_err(|e| format!("{}: {e}", self.right_path.display()))?;
        match (crate::textenc::decode(&a), crate::textenc::decode(&b)) {
            (Some((x, _)), Some((y, _))) => {
                self.left_lines = lines_of(&x);
                self.right_lines = lines_of(&y);
                Ok(())
            }
            _ => Err(if a == b { format!("Binary files, identical ({} bytes).", a.len()) } else { format!("Binary files, they differ ({} and {} bytes).", a.len(), b.len()) }),
        }
    }

    fn recompute(&mut self) {
        self.rows = if self.message.is_some() { Vec::new() } else { diff_rows(&self.left_lines, &self.right_lines, self.ignore_ws, self.ignore_case) };
        self.blocks = block_starts(&self.rows);
        self.cur = None;
        if !self.blocks.is_empty() {
            self.goto_block(0);
        }
    }

    fn goto_block(&mut self, i: usize) {
        self.cur = Some(i);
        // a little context above the first changed row
        self.want = Some(((self.blocks[i] as f32 - 2.0) * ROW_H).max(0.0));
    }

    fn step(&mut self, forward: bool) {
        if self.blocks.is_empty() {
            return;
        }
        let i = match (self.cur, forward) {
            (None, true) => 0,
            (None, false) => self.blocks.len() - 1,
            (Some(c), true) => (c + 1).min(self.blocks.len() - 1),
            (Some(c), false) => c.saturating_sub(1),
        };
        self.goto_block(i);
    }
}

#[derive(Default)]
pub struct DiffViewer {
    open: Option<Doc>,
    link: Link,
    fresh: bool,
}

impl DiffViewer {
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    pub fn open(&mut self, left: &Path, right: &Path) {
        self.open = Some(Doc::load(left, right));
        self.fresh = true;
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        let Some(doc) = &mut self.open else { return };
        let fresh = std::mem::take(&mut self.fresh);
        if fresh {
            self.link = Link::default();
        }
        let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let title = format!("Compare: {}  \u{2194}  {}", name(&doc.left_path), name(&doc.right_path));
        let builder = self.link.builder(ctx, &title);
        let id = egui::ViewportId::from_hash_of("gc_diff");
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

/// Background of a row for its kind (the side that has the line, or the empty side).
fn tint(kind: Kind, side_has_line: bool, dark: bool) -> Option<Color32> {
    let a = if dark { 70 } else { 90 };
    match (kind, side_has_line) {
        (Kind::Same, _) => None,
        (Kind::Changed, _) => Some(Color32::from_rgba_unmultiplied(230, 170, 40, a)),
        (Kind::OnlyLeft, true) => Some(Color32::from_rgba_unmultiplied(220, 70, 70, a)),
        (Kind::OnlyRight, true) => Some(Color32::from_rgba_unmultiplied(70, 190, 90, a)),
        (_, false) => Some(Color32::from_rgba_unmultiplied(128, 128, 128, 40)),
    }
}

/// Draws the window content; returns true when a key asked to close it.
fn draw(doc: &mut Doc, ui: &mut egui::Ui, fresh: bool) -> bool {
    let ctx = ui.ctx().clone();
    let typing = ctx.wants_keyboard_input();
    let dark = ui.visuals().dark_mode;
    let mut close = false;
    let mut recompute = false;

    if !fresh && !typing && ctx.input(|i| i.key_pressed(Key::Escape) || i.key_pressed(Key::Q)) {
        close = true;
    }
    let (next, prev) = ctx.input(|i| (i.key_pressed(Key::N) || i.key_pressed(Key::F7) || (i.modifiers.alt && i.key_pressed(Key::ArrowDown)), i.key_pressed(Key::P) || (i.modifiers.alt && i.key_pressed(Key::ArrowUp))));
    if !typing {
        if next {
            doc.step(true);
        }
        if prev {
            doc.step(false);
        }
        let max = (doc.rows.len() as f32 * ROW_H - doc.view_h).max(0.0);
        let plain = ctx.input(|i| !i.modifiers.alt);
        let pressed = |k| plain && ctx.input(|i| i.key_pressed(k));
        let mut off = doc.want.unwrap_or(doc.offset);
        if pressed(Key::ArrowDown) {
            off += ROW_H * 3.0;
        }
        if pressed(Key::ArrowUp) {
            off -= ROW_H * 3.0;
        }
        if pressed(Key::PageDown) {
            off += doc.view_h - ROW_H * 2.0;
        }
        if pressed(Key::PageUp) {
            off -= doc.view_h - ROW_H * 2.0;
        }
        if pressed(Key::Home) {
            off = 0.0;
        }
        if pressed(Key::End) {
            off = max;
        }
        if (off - doc.want.unwrap_or(doc.offset)).abs() > f32::EPSILON {
            doc.want = Some(off.clamp(0.0, max));
        }
    }

    ui.horizontal(|ui| {
        if ui.add_enabled(!doc.blocks.is_empty(), egui::Button::new(format!("{} Previous difference  (P)", egui_phosphor::regular::CARET_UP))).clicked() {
            doc.step(false);
        }
        if ui.add_enabled(!doc.blocks.is_empty(), egui::Button::new(format!("{} Next difference  (N)", egui_phosphor::regular::CARET_DOWN))).clicked() {
            doc.step(true);
        }
        recompute |= ui.checkbox(&mut doc.ignore_ws, "Ignore white space").changed();
        recompute |= ui.checkbox(&mut doc.ignore_case, "Ignore case").changed();
        let state = if let Some(m) = &doc.message {
            m.clone()
        } else if doc.blocks.is_empty() {
            "The files are identical".to_owned()
        } else {
            let changed = doc.rows.iter().filter(|r| r.kind != Kind::Same).count();
            format!("{} difference(s), {changed} line(s){}", doc.blocks.len(), doc.cur.map(|c| format!("   -   now at {} / {}", c + 1, doc.blocks.len())).unwrap_or_default())
        };
        ui.weak(state);
    });
    if recompute {
        doc.recompute();
    }
    ui.horizontal(|ui| {
        let half = (ui.available_width() - 8.0) / 2.0;
        ui.add_sized([half, 16.0], egui::Label::new(egui::RichText::new(doc.left_path.display().to_string()).strong()).truncate());
        ui.add_sized([half, 16.0], egui::Label::new(egui::RichText::new(doc.right_path.display().to_string()).strong()).truncate());
    });
    ui.separator();

    let font = FontId::monospace(13.0);
    let char_w = ui.fonts(|f| f.glyph_width(&font, '0'));
    let text_color = ui.visuals().text_color();
    let weak = ui.visuals().weak_text_color();
    let num_w = char_w * 6.0;
    ui.spacing_mut().item_spacing.y = 0.0;
    let mut area = egui::ScrollArea::vertical().id_salt("diff_rows").auto_shrink(false);
    if let Some(y) = doc.want.take() {
        area = area.vertical_scroll_offset(y);
    }
    let total = doc.rows.len();
    let cur_row = doc.cur.map(|c| doc.blocks[c]);
    let out = area.show_rows(ui, ROW_H, total, |ui, range| {
        for i in range {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), Sense::hover());
            let row = &doc.rows[i];
            let painter = ui.painter_at(rect);
            let mid = rect.center().x;
            for (side, line, x0, x1) in [(0, &row.left, rect.left(), mid - 2.0), (1, &row.right, mid + 2.0, rect.right())] {
                let cell = egui::Rect::from_min_max(egui::pos2(x0, rect.top()), egui::pos2(x1, rect.bottom()));
                let _ = side;
                if let Some(bg) = tint(row.kind, line.is_some(), dark) {
                    painter.rect_filled(cell, 0.0, bg);
                }
                if let Some((n, text)) = line {
                    let clip = painter.with_clip_rect(cell);
                    clip.text(egui::pos2(x0 + num_w - 4.0, rect.center().y), Align2::RIGHT_CENTER, n.to_string(), font.clone(), weak);
                    clip.text(egui::pos2(x0 + num_w + 4.0, rect.center().y), Align2::LEFT_CENTER, text.replace('\t', "    "), font.clone(), text_color);
                }
            }
            painter.line_segment([egui::pos2(mid, rect.top()), egui::pos2(mid, rect.bottom())], egui::Stroke::new(1.0_f32, weak.gamma_multiply(0.5)));
            if cur_row == Some(i) {
                painter.rect_stroke(rect.shrink(0.5), 0.0, egui::Stroke::new(1.0_f32, Color32::from_rgb(230, 170, 40)), egui::StrokeKind::Inside);
            }
        }
    });
    doc.offset = out.state.offset.y;
    doc.view_h = out.inner_rect.height();
    close
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn equal_files_have_no_blocks() {
        let rows = diff_rows(&v(&["a", "b"]), &v(&["a", "b"]), false, false);
        assert_eq!(rows.len(), 2);
        assert!(block_starts(&rows).is_empty());
    }

    #[test]
    fn changed_inserted_and_deleted_lines_are_aligned() {
        let rows = diff_rows(&v(&["a", "b", "c", "d"]), &v(&["a", "B", "c", "x", "d", "e"]), false, false);
        let kinds: Vec<Kind> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(kinds, vec![Kind::Same, Kind::Changed, Kind::Same, Kind::OnlyRight, Kind::Same, Kind::OnlyRight]);
        assert_eq!(block_starts(&rows), vec![1, 3, 5]);
        assert_eq!(rows[1].left.as_ref().unwrap().0, 2);
        assert_eq!(rows[3].left, None);
    }

    #[test]
    fn white_space_and_case_can_be_ignored() {
        let (a, b) = (v(&["Hello   world", "x"]), v(&["hello world", "x"]));
        assert_eq!(block_starts(&diff_rows(&a, &b, false, false)).len(), 1);
        assert!(block_starts(&diff_rows(&a, &b, true, true)).is_empty());
    }

    #[test]
    fn unequal_replace_blocks_pad_with_gaps() {
        let rows = diff_rows(&v(&["a", "b", "c"]), &v(&["a", "X"]), false, false);
        let kinds: Vec<Kind> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(kinds, vec![Kind::Same, Kind::Changed, Kind::OnlyLeft]);
    }
}
