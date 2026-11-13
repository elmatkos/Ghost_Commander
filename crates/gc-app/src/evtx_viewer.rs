//! F3 on a `.evtx` file: Windows event log viewer. The file is parsed by the `evtx` crate on a
//! worker thread (works on any machine and in WinPE, also for logs of another PC); the list can be
//! filtered by level, event ID, source and text and sorted by a column click; the selected event is
//! shown as XML with the formatted message added when this PC knows the event provider.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};

use eframe::egui::{self, text::LayoutJob, Align2, Color32, FontId, Key, Sense, TextFormat, Vec2};

use crate::fullscreen::Link;
use crate::syntax::{self, Kind, Lang};

/// More events than this are not loaded (the title says so).
const MAX_EVENTS: usize = 400_000;

struct Event {
    record: u64,
    time: String,
    level: u8,
    event_id: u32,
    provider: String,
    channel: String,
    computer: String,
    /// The values of the event data on one line.
    summary: String,
    xml: String,
}

fn tag_text<'a>(xml: &'a str, tag: &str) -> &'a str {
    let open = format!("<{tag}");
    let Some(s) = xml.find(&open) else { return "" };
    let rest = &xml[s + open.len()..];
    // `<EventID Qualifiers="0">4624</EventID>` or `<Level>4</Level>`
    let Some(gt) = rest.find('>') else { return "" };
    if rest[..gt].ends_with('/') {
        return "";
    }
    let body = &rest[gt + 1..];
    &body[..body.find('<').unwrap_or(body.len())]
}

fn tag_attr<'a>(xml: &'a str, tag: &str, attr: &str) -> &'a str {
    let open = format!("<{tag}");
    let Some(s) = xml.find(&open) else { return "" };
    let rest = &xml[s..];
    let end = rest.find('>').unwrap_or(rest.len());
    let key = format!("{attr}=\"");
    let Some(a) = rest[..end].find(&key) else { return "" };
    let v = &rest[a + key.len()..end];
    &v[..v.find('"').unwrap_or(v.len())]
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// `<Data Name="x">v</Data>` values (or the plain `<Data>v</Data>`), joined with `; `.
fn event_data_summary(xml: &str) -> String {
    let Some(s) = xml.find("<EventData").or_else(|| xml.find("<UserData")) else { return String::new() };
    let mut out = String::new();
    let mut rest = &xml[s..];
    while let Some(p) = rest.find("<Data") {
        rest = &rest[p + 5..];
        let Some(gt) = rest.find('>') else { break };
        if rest[..gt].ends_with('/') {
            continue;
        }
        let body = &rest[gt + 1..];
        let v = unescape(body[..body.find('<').unwrap_or(body.len())].trim());
        if !v.is_empty() {
            if !out.is_empty() {
                out.push_str("; ");
            }
            out.push_str(&v);
            if out.chars().count() > 300 {
                break;
            }
        }
    }
    out.chars().take(300).map(|c| if c.is_control() { ' ' } else { c }).collect()
}

fn parse_event(record: u64, xml: String) -> Event {
    Event {
        record,
        time: tag_attr(&xml, "TimeCreated", "SystemTime").trim_end_matches('Z').replace('T', " ").chars().take(23).collect(),
        level: tag_text(&xml, "Level").trim().parse().unwrap_or(4),
        event_id: tag_text(&xml, "EventID").trim().parse().unwrap_or(0),
        provider: tag_attr(&xml, "Provider", "Name").to_string(),
        channel: tag_text(&xml, "Channel").to_string(),
        computer: tag_text(&xml, "Computer").to_string(),
        summary: event_data_summary(&xml),
        xml,
    }
}

enum Msg {
    Batch(Vec<Event>),
    Done(Option<String>),
}

fn spawn_loader(ctx: &egui::Context, path: &Path) -> Receiver<Msg> {
    let (tx, rx) = channel();
    let (ctx, path) = (ctx.clone(), path.to_path_buf());
    std::thread::spawn(move || {
        let mut parser = match evtx::EvtxParser::from_path(&path) {
            Ok(p) => p,
            Err(e) => {
                let _ = tx.send(Msg::Done(Some(format!("Cannot read the event log: {e}"))));
                ctx.request_repaint();
                return;
            }
        };
        let (mut batch, mut total) = (Vec::new(), 0usize);
        for rec in parser.records() {
            let Ok(rec) = rec else { continue };
            batch.push(parse_event(rec.event_record_id, rec.data));
            total += 1;
            if batch.len() >= 2000 {
                if tx.send(Msg::Batch(std::mem::take(&mut batch))).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
            if total >= MAX_EVENTS {
                break;
            }
        }
        let _ = tx.send(Msg::Batch(batch));
        let _ = tx.send(Msg::Done(None));
        ctx.request_repaint();
    });
    rx
}

#[derive(Clone, Copy, PartialEq)]
enum SortBy {
    Time,
    Level,
    Source,
    Id,
}

struct Doc {
    path: PathBuf,
    name: String,
    events: Vec<Event>,
    rx: Option<Receiver<Msg>>,
    error: Option<String>,
    /// Indices into `events` that pass the filter, in display order.
    view: Vec<usize>,
    dirty: bool,
    last_rebuild: std::time::Instant,
    sort: SortBy,
    desc: bool,
    levels: [bool; 5],
    id_q: String,
    src_q: String,
    text_q: String,
    selected: Option<usize>,
    /// Position (in `view`) of the row the keys moved to: the list scrolls to show it.
    reveal: Option<usize>,
    scroll_y: f32,
    view_h: f32,
    /// Formatted message of the selected event (record, text).
    message: Option<(u64, Option<String>)>,
    msg: String,
    truncated: bool,
}

#[derive(Default)]
pub struct EvtxViewer {
    open: Option<Doc>,
    link: Link,
    fresh: bool,
}

fn level_name(l: u8) -> &'static str {
    match l {
        1 => "Critical",
        2 => "Error",
        3 => "Warning",
        5 => "Verbose",
        _ => "Information",
    }
}

fn level_color(l: u8, dark: bool, plain: Color32) -> Color32 {
    match l {
        1 | 2 => syntax::color(Kind::Error, dark),
        3 => syntax::color(Kind::Warn, dark),
        5 => Color32::GRAY,
        _ => plain,
    }
}

impl Doc {
    fn level_slot(l: u8) -> usize {
        (l.clamp(1, 5) - 1) as usize
    }

    fn matches(&self, e: &Event, id_ok: &dyn Fn(u32) -> bool, src: &str, text: &str) -> bool {
        self.levels[Self::level_slot(if e.level == 0 { 4 } else { e.level })]
            && id_ok(e.event_id)
            && (src.is_empty() || e.provider.to_lowercase().contains(src))
            && (text.is_empty() || e.summary.to_lowercase().contains(text) || e.xml.to_lowercase().contains(text))
    }

    fn rebuild(&mut self) {
        let ids: Vec<u32> = self.id_q.split([',', ' ', ';']).filter_map(|t| t.trim().parse().ok()).collect();
        let id_ok = |id: u32| ids.is_empty() || ids.contains(&id);
        let (src, text) = (self.src_q.trim().to_lowercase(), self.text_q.trim().to_lowercase());
        let keep: Vec<usize> = (0..self.events.len()).filter(|&i| self.matches(&self.events[i], &id_ok, &src, &text)).collect();
        self.view = keep;
        let ev = &self.events;
        match self.sort {
            SortBy::Time => self.view.sort_by(|&a, &b| ev[a].time.cmp(&ev[b].time).then(ev[a].record.cmp(&ev[b].record))),
            SortBy::Level => self.view.sort_by_key(|&i| (ev[i].level, ev[i].record)),
            SortBy::Source => self.view.sort_by(|&a, &b| ev[a].provider.to_lowercase().cmp(&ev[b].provider.to_lowercase()).then(ev[a].record.cmp(&ev[b].record))),
            SortBy::Id => self.view.sort_by_key(|&i| (ev[i].event_id, ev[i].record)),
        }
        if self.desc {
            self.view.reverse();
        }
        self.dirty = false;
        self.last_rebuild = std::time::Instant::now();
    }

    fn poll(&mut self) {
        let mut got = false;
        if let Some(rx) = &self.rx {
            while let Ok(m) = rx.try_recv() {
                match m {
                    Msg::Batch(b) => {
                        self.events.extend(b);
                        got = true;
                    }
                    Msg::Done(err) => {
                        self.error = err;
                        self.rx = None;
                        got = true;
                        break;
                    }
                }
            }
        }
        self.truncated = self.events.len() >= MAX_EVENTS;
        if got {
            // while loading the list is rebuilt at most twice a second
            if self.rx.is_none() || self.last_rebuild.elapsed().as_millis() > 500 {
                self.dirty = true;
            }
        }
    }

    fn write_csv(&self) -> std::io::Result<PathBuf> {
        let out = self.path.with_extension("filtered.csv");
        let q = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
        let mut text = String::from("Time,Level,EventID,Source,Channel,Computer,Description\n");
        for &i in &self.view {
            let e = &self.events[i];
            text.push_str(&format!("{},{},{},{},{},{},{}\n", q(&e.time), level_name(e.level), e.event_id, q(&e.provider), q(&e.channel), q(&e.computer), q(&e.summary)));
        }
        std::fs::write(&out, text)?;
        Ok(out)
    }
}

/// The message of an event as Windows Event Viewer would show it, when this PC knows the provider.
#[cfg(windows)]
fn format_message(path: &Path, record: u64, provider: &str) -> Option<String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::System::EventLog::{EvtClose, EvtFormatMessage, EvtFormatMessageEvent, EvtNext, EvtOpenPublisherMetadata, EvtQuery, EvtQueryFilePath, EVT_HANDLE};

    let wide = |s: &std::ffi::OsStr| -> Vec<u16> { s.encode_wide().chain(std::iter::once(0)).collect() };
    let file = wide(path.as_os_str());
    let query = wide(std::ffi::OsStr::new(&format!("*[System[EventRecordID={record}]]")));
    let prov = wide(std::ffi::OsStr::new(provider));
    unsafe {
        let result = EvtQuery(EVT_HANDLE::default(), PCWSTR(file.as_ptr()), PCWSTR(query.as_ptr()), EvtQueryFilePath.0).ok()?;
        let mut event = [0isize; 1];
        let mut returned = 0u32;
        let mut text = None;
        if EvtNext(result, &mut event, 2000, 0, &mut returned).is_ok() && returned == 1 {
            let ev = EVT_HANDLE(event[0]);
            if let Ok(meta) = EvtOpenPublisherMetadata(EVT_HANDLE::default(), PCWSTR(prov.as_ptr()), PCWSTR::null(), 0, 0) {
                let mut used = 0u32;
                let _ = EvtFormatMessage(meta, ev, 0, None, EvtFormatMessageEvent.0, None, &mut used);
                if used > 1 {
                    let mut buf = vec![0u16; used as usize];
                    if EvtFormatMessage(meta, ev, 0, None, EvtFormatMessageEvent.0, Some(&mut buf), &mut used).is_ok() {
                        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
                        text = Some(String::from_utf16_lossy(&buf[..len]));
                    }
                }
                let _ = EvtClose(meta);
            }
            let _ = EvtClose(ev);
        }
        let _ = EvtClose(result);
        let _ = PWSTR::null();
        text.filter(|t| !t.trim().is_empty())
    }
}

#[cfg(not(windows))]
fn format_message(_: &Path, _: u64, _: &str) -> Option<String> {
    None
}

impl EvtxViewer {
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    pub fn open(&mut self, ctx: &egui::Context, path: &Path) {
        self.open = Some(Doc {
            path: path.to_path_buf(),
            name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            events: Vec::new(),
            rx: Some(spawn_loader(ctx, path)),
            error: None,
            view: Vec::new(),
            dirty: false,
            last_rebuild: std::time::Instant::now(),
            sort: SortBy::Time,
            desc: true,
            levels: [true; 5],
            id_q: String::new(),
            src_q: String::new(),
            text_q: String::new(),
            selected: None,
            reveal: None,
            scroll_y: 0.0,
            view_h: 300.0,
            message: None,
            msg: String::new(),
            truncated: false,
        });
        self.fresh = true;
    }

    pub fn show(&mut self, ctx: &egui::Context, dark: bool) {
        let Some(doc) = &mut self.open else { return };
        let fresh = std::mem::take(&mut self.fresh);
        if fresh {
            self.link = Link::default();
        }
        doc.poll();
        if doc.dirty {
            doc.rebuild();
        }
        let title = format!("Event log: {}  ({} events{})", doc.name, doc.events.len(), if doc.rx.is_some() { ", loading..." } else if doc.truncated { ", first part only" } else { "" });
        let builder = self.link.builder(ctx, &title);
        let id = egui::ViewportId::from_hash_of("gc_evtx");
        let (mut close, mut minimized) = (false, false);
        ctx.show_viewport_immediate(id, builder, |ctx, class| {
            minimized = ctx.input(|i| i.viewport().minimized).unwrap_or(false);
            close = ctx.input(|i| i.viewport().close_requested());
            if crate::fullscreen::show(ctx, class, &title, |ui| {
                if draw(doc, ui, fresh, dark) {
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

const COLS: [(&str, f32); 4] = [("Level", 95.0), ("Date and Time", 190.0), ("Source", 230.0), ("Event ID", 80.0)];

/// Draws the whole window content; returns true when a key asked to close it.
fn draw(doc: &mut Doc, ui: &mut egui::Ui, fresh: bool, dark: bool) -> bool {
    let ctx = ui.ctx().clone();
    let typing = ctx.wants_keyboard_input();
    let mut close = false;
    if !fresh && !typing && ctx.input(|i| i.key_pressed(Key::Escape) || i.key_pressed(Key::Q) || i.key_pressed(Key::F3)) {
        close = true;
    }
    let row_h = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
    let plain = ui.visuals().text_color();
    let weak = ui.visuals().weak_text_color();

    // filters
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        for (slot, name) in [(0, "Critical"), (1, "Error"), (2, "Warning"), (3, "Information"), (4, "Verbose")] {
            let color = level_color(slot as u8 + 1, dark, plain);
            changed |= ui.checkbox(&mut doc.levels[slot], egui::RichText::new(name).color(color)).changed();
        }
        ui.label("ID");
        changed |= ui.add(egui::TextEdit::singleline(&mut doc.id_q).hint_text("e.g. 41, 1001").desired_width(90.0)).changed();
        ui.label("Source");
        changed |= ui.add(egui::TextEdit::singleline(&mut doc.src_q).desired_width(130.0)).changed();
        ui.label("Text");
        changed |= ui.add(egui::TextEdit::singleline(&mut doc.text_q).desired_width(170.0)).changed();
        ui.weak(format!("{} shown", doc.view.len()));
        if ui.button("Export shown to CSV").on_hover_text("Writes <file>.filtered.csv next to the log").clicked() {
            doc.msg = match doc.write_csv() {
                Ok(p) => format!("Saved {}", p.display()),
                Err(e) => format!("Cannot save: {e}"),
            };
        }
        if !doc.msg.is_empty() {
            ui.label(&doc.msg);
        }
    });
    if let Some(e) = &doc.error {
        ui.colored_label(syntax::color(Kind::Error, dark), e);
    }
    if changed {
        doc.dirty = true;
    }
    ui.separator();

    // selection with the keys
    if !typing && !doc.view.is_empty() {
        let (up, down, pgup, pgdn, home, end) = ctx.input(|i| (i.key_pressed(Key::ArrowUp), i.key_pressed(Key::ArrowDown), i.key_pressed(Key::PageUp), i.key_pressed(Key::PageDown), i.key_pressed(Key::Home), i.key_pressed(Key::End)));
        let cur = doc.selected.and_then(|s| doc.view.iter().position(|&i| i == s)).unwrap_or(0);
        let last = doc.view.len() - 1;
        let target = if up { Some(cur.saturating_sub(1)) } else if down { Some((cur + 1).min(last)) } else if pgup { Some(cur.saturating_sub(15)) } else if pgdn { Some((cur + 15).min(last)) } else if home { Some(0) } else if end { Some(last) } else { None };
        if let Some(t) = target {
            doc.selected = Some(doc.view[t]);
            doc.reveal = Some(t);
        }
    }

    let total_h = ui.available_height();
    let detail_h = (total_h * 0.40).clamp(140.0, 460.0).min((total_h - 120.0).max(60.0));
    let list_h = (total_h - detail_h - 10.0).max(60.0);
    let list_w = ui.available_width();
    let mut clicked = None;
    ui.allocate_ui(Vec2::new(list_w, list_h), |ui| {
        ui.set_min_size(Vec2::new(list_w, list_h));
        ui.set_max_height(list_h);
    // header: clickable column titles
    let total_w = ui.available_width();
    {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(total_w, row_h), Sense::hover());
        let font = FontId::proportional(13.0);
        let mut x = rect.left();
        for (k, (name, w)) in COLS.iter().enumerate() {
            let sort = [SortBy::Level, SortBy::Time, SortBy::Source, SortBy::Id][k];
            let cell = egui::Rect::from_min_size(egui::pos2(x, rect.top()), Vec2::new(*w, row_h));
            let r = ui.interact(cell, egui::Id::new(("evtx_col", k)), Sense::click());
            let arrow = if doc.sort == sort { if doc.desc { " v" } else { " ^" } } else { "" };
            let color = if r.hovered() { ui.visuals().strong_text_color() } else { plain };
            ui.painter().text(egui::pos2(x + 4.0, rect.center().y), Align2::LEFT_CENTER, format!("{name}{arrow}"), font.clone(), color);
            if r.clicked() {
                if doc.sort == sort {
                    doc.desc = !doc.desc;
                } else {
                    doc.sort = sort;
                    doc.desc = sort == SortBy::Time;
                }
                doc.dirty = true;
            }
            x += w;
        }
        ui.painter().text(egui::pos2(x + 4.0, rect.center().y), Align2::LEFT_CENTER, "Description", font, plain);
    }

    // rows
    let desc_x: f32 = COLS.iter().map(|c| c.1).sum();
    let n = doc.view.len();
    // show_rows spaces the rows by row_h + item spacing
    let step = row_h + ui.spacing().item_spacing.y;
    let mut area = egui::ScrollArea::vertical().auto_shrink(false).max_height((list_h - row_h - 6.0).max(40.0));
    if let Some(t) = doc.reveal.take() {
        let top = t as f32 * step;
        if top < doc.scroll_y {
            area = area.vertical_scroll_offset(top);
        } else if top + step > doc.scroll_y + doc.view_h {
            area = area.vertical_scroll_offset(top + step - doc.view_h);
        }
    }
    let out = area.show_rows(ui, row_h, n, |ui, range| {
        for row in range {
            let e = &doc.events[doc.view[row]];
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), row_h), Sense::click());
            let selected = doc.selected == Some(doc.view[row]);
            if selected {
                ui.painter().rect_filled(rect, 0.0, ui.visuals().selection.bg_fill);
            } else if resp.hovered() {
                ui.painter().rect_filled(rect, 0.0, ui.visuals().widgets.hovered.bg_fill.linear_multiply(0.4));
            }
            if resp.clicked() {
                clicked = Some(doc.view[row]);
            }
            let p = ui.painter().with_clip_rect(rect);
            let font = FontId::proportional(13.0);
            let cy = rect.center().y;
            let mut x = rect.left() + 4.0;
            let color = level_color(e.level, dark, plain);
            let id_text = e.event_id.to_string();
            let cells: [(&str, Color32); 4] = [(level_name(e.level), color), (&e.time, plain), (&e.provider, plain), (&id_text, plain)];
            for (k, (text, c)) in cells.iter().enumerate() {
                p.text(egui::pos2(x, cy), Align2::LEFT_CENTER, text, font.clone(), *c);
                x = rect.left() + 4.0 + COLS[..=k].iter().map(|c| c.1).sum::<f32>();
            }
            let _ = desc_x;
            p.text(egui::pos2(x, cy), Align2::LEFT_CENTER, &e.summary, font, weak);
        }
    });
    doc.scroll_y = out.state.offset.y;
    doc.view_h = out.inner_rect.height();
    });
    if let Some(i) = clicked {
        doc.selected = Some(i);
    }
    ui.separator();
    ui.allocate_ui(Vec2::new(list_w, detail_h), |ui| {
        ui.set_min_size(Vec2::new(list_w, detail_h - 6.0));
        detail(doc, ui, dark, plain, weak);
    });
    close
}

fn detail(doc: &mut Doc, ui: &mut egui::Ui, dark: bool, plain: Color32, weak: Color32) {
    let Some(sel) = doc.selected else {
        ui.weak("Select an event (click, or Up / Down).");
        return;
    };
    let (record, provider) = {
        let e = &doc.events[sel];
        (e.record, e.provider.clone())
    };
    if doc.message.as_ref().map(|m| m.0) != Some(record) {
        doc.message = Some((record, format_message(&doc.path, record, &provider)));
    }
    let e = &doc.events[sel];
    ui.horizontal_wrapped(|ui| {
        ui.strong(format!("{}  |  {}  |  ID {}  |  record {}  |  {}", level_name(e.level), e.provider, e.event_id, e.record, e.time));
        ui.weak(format!("{} / {}", e.channel, e.computer));
        if ui.button("Copy XML").clicked() {
            ui.ctx().copy_text(e.xml.clone());
        }
    });
    if let Some((_, Some(text))) = &doc.message {
        ui.label(text);
    } else {
        ui.colored_label(weak, "(no message text: the provider of this event is not installed on this PC; see the event data)");
    }
    ui.separator();
    let lines: Vec<&str> = e.xml.lines().collect();
    let states = syntax::line_states(Lang::Xml, lines.iter().copied());
    egui::ScrollArea::both().auto_shrink(false).id_salt("evtx_xml").show(ui, |ui| {
        for (n, line) in lines.iter().enumerate() {
            let mut job = LayoutJob::default();
            let font = FontId::monospace(12.5);
            let mut pos = 0;
            for (range, kind) in syntax::scan(Lang::Xml, line, states.get(n).copied().unwrap_or_default()).0 {
                if range.start > pos {
                    job.append(&line[pos..range.start], 0.0, TextFormat { font_id: font.clone(), color: plain, ..Default::default() });
                }
                job.append(&line[range.clone()], 0.0, TextFormat { font_id: font.clone(), color: syntax::color(kind, dark), ..Default::default() });
                pos = range.end;
            }
            if pos < line.len() {
                job.append(&line[pos..], 0.0, TextFormat { font_id: font.clone(), color: plain, ..Default::default() });
            }
            if line.is_empty() {
                job.append(" ", 0.0, TextFormat { font_id: font, color: plain, ..Default::default() });
            }
            ui.add(egui::Label::new(job).extend());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<Event xmlns="http://schemas.microsoft.com/win/2004/08/events/event">
  <System>
    <Provider Name="Microsoft-Windows-Kernel-Power" Guid="{331C3B3A-2005-44C2-AC5E-77220C37D6B4}" />
    <EventID Qualifiers="0">41</EventID>
    <Level>1</Level>
    <TimeCreated SystemTime="2024-05-01T12:30:45.123456Z" />
    <Channel>System</Channel>
    <Computer>PC1</Computer>
  </System>
  <EventData>
    <Data Name="BugcheckCode">159</Data>
    <Data Name="Empty"></Data>
    <Data Name="Text">a &amp; b</Data>
  </EventData>
</Event>"#;

    #[test]
    fn parses_event_fields() {
        let e = parse_event(7, XML.to_string());
        assert_eq!((e.record, e.event_id, e.level), (7, 41, 1));
        assert_eq!(e.provider, "Microsoft-Windows-Kernel-Power");
        assert_eq!(e.time, "2024-05-01 12:30:45.123");
        assert_eq!((e.channel.as_str(), e.computer.as_str()), ("System", "PC1"));
        assert_eq!(e.summary, "159; a & b");
    }

    #[test]
    fn filter_and_sort() {
        let mut doc = Doc {
            path: PathBuf::new(),
            name: String::new(),
            events: vec![parse_event(1, XML.to_string()), parse_event(2, XML.replace("<Level>1</Level>", "<Level>4</Level>").replace(">41<", ">1001<"))],
            rx: None,
            error: None,
            view: vec![],
            dirty: true,
            last_rebuild: std::time::Instant::now(),
            sort: SortBy::Id,
            desc: false,
            levels: [true; 5],
            id_q: String::new(),
            src_q: String::new(),
            text_q: String::new(),
            selected: None,
            reveal: None,
            scroll_y: 0.0,
            view_h: 300.0,
            message: None,
            msg: String::new(),
            truncated: false,
        };
        doc.rebuild();
        assert_eq!(doc.view, vec![0, 1]);
        doc.levels[0] = false;
        doc.rebuild();
        assert_eq!(doc.view, vec![1]);
        doc.levels[0] = true;
        doc.id_q = "41".into();
        doc.rebuild();
        assert_eq!(doc.view, vec![0]);
        doc.id_q.clear();
        doc.text_q = "BUGCHECK".into();
        doc.rebuild();
        assert_eq!(doc.view.len(), 2);
    }
}
