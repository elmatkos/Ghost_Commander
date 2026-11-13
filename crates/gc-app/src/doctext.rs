//! Plain text of `.docx`, `.odt` and `.rtf` documents for the viewer: paragraphs, headings (as
//! Markdown `#` lines), list items, tables as rows with ` | ` between cells. No layout, no pictures.

use std::io::Read;
use std::path::Path;

/// Text of the document at `path` and the format name, or `None` when it is not one of those formats
/// (or cannot be read).
pub fn extract(path: &Path) -> Option<(String, &'static str)> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    match ext.as_str() {
        "docx" => Some((docx_to_text(&zip_entry(path, "word/document.xml")?), "docx")),
        "odt" => Some((odt_to_text(&zip_entry(path, "content.xml")?), "odt")),
        "msg" => Some((msg_to_text(path)?, "msg")),
        "rtf" => {
            let bytes = std::fs::read(path).ok()?;
            bytes.starts_with(b"{\\rtf").then(|| (rtf_to_text(&bytes), "rtf"))
        }
        _ => None,
    }
}

fn zip_entry(path: &Path, name: &str) -> Option<String> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path).ok()?).ok()?;
    let entry = zip.by_name(name).ok()?;
    let mut s = String::new();
    entry.take(128 * 1024 * 1024).read_to_string(&mut s).ok()?;
    Some(s)
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find('&') {
        out.push_str(&rest[..p]);
        rest = &rest[p..];
        let Some(e) = rest.find(';').filter(|&e| e <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..e];
        match ent {
            "amp" => out.push('&'),
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "quot" => out.push('"'),
            "nbsp" => out.push(' '),
            "apos" => out.push('\''),
            _ => {
                let code = ent.strip_prefix("#x").and_then(|h| u32::from_str_radix(h, 16).ok()).or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()));
                match code.and_then(char::from_u32) {
                    Some(c) => out.push(c),
                    None => out.push_str(&rest[..=e]),
                }
            }
        }
        rest = &rest[e + 1..];
    }
    out.push_str(rest);
    out
}

/// Value of attribute `name` in the text of a tag.
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let key = format!("{name}=\"");
    let s = tag.find(&key)? + key.len();
    let e = tag[s..].find('"')?;
    Some(&tag[s..s + e])
}

/// Walks the tags of an XML text: `f(name, tag_text, is_closing, text_before)`. `name` has no `/`.
fn walk(xml: &str, mut f: impl FnMut(&str, &str, bool, &str)) {
    let mut pos = 0;
    while let Some(lt) = xml[pos..].find('<') {
        let lt = pos + lt;
        let Some(gt) = xml[lt..].find('>') else { break };
        let tag = &xml[lt + 1..lt + gt];
        let before = &xml[pos..lt];
        pos = lt + gt + 1;
        if tag.starts_with('?') || tag.starts_with('!') {
            continue;
        }
        let closing = tag.starts_with('/');
        let body = tag.trim_start_matches('/');
        let name_end = body.find(|c: char| c.is_whitespace() || c == '/').unwrap_or(body.len());
        f(&body[..name_end], body, closing, before);
        if tag.ends_with('/') && !closing {
            f(&body[..name_end], body, true, "");
        }
    }
}

fn heading_prefix(level: usize) -> String {
    format!("{} ", "#".repeat(level.clamp(1, 6)))
}

pub fn docx_to_text(xml: &str) -> String {
    let mut out = String::new();
    let (mut in_t, mut heading, mut list, mut cell, mut fresh) = (false, 0usize, false, 0usize, true);
    walk(xml, |name, tag, closing, before| {
        if in_t && !before.is_empty() {
            if fresh {
                fresh = false;
                if heading > 0 {
                    out.push_str(&heading_prefix(heading));
                } else if list {
                    out.push_str("- ");
                }
            }
            out.push_str(&unescape(before));
        }
        match (name, closing) {
            ("w:t", false) => in_t = true,
            ("w:t", true) => in_t = false,
            ("w:tab", false) => out.push('\t'),
            ("w:br", false) | ("w:cr", false) => out.push('\n'),
            ("w:p", false) => (heading, list, fresh) = (0, false, true),
            ("w:pStyle", false) => {
                let v = attr(tag, "w:val").unwrap_or("").to_ascii_lowercase();
                if v.starts_with("heading") || v.starts_with("nadpis") || v.starts_with("title") {
                    heading = v.chars().filter(char::is_ascii_digit).collect::<String>().parse().unwrap_or(1);
                }
            }
            ("w:numPr", false) => list = true,
            ("w:p", true) => out.push(if cell > 0 { ' ' } else { '\n' }),
            ("w:tc", false) => cell += 1,
            ("w:tc", true) => {
                cell = cell.saturating_sub(1);
                out.push_str("| ");
            }
            ("w:tr", true) => out.push('\n'),
            _ => {}
        }
    });
    out
}

pub fn odt_to_text(xml: &str) -> String {
    let mut out = String::new();
    let (mut depth, mut cell, mut list) = (0usize, 0usize, false);
    walk(xml, |name, tag, closing, before| {
        if depth > 0 && !before.is_empty() {
            out.push_str(&unescape(before));
        }
        match (name, closing) {
            ("text:h", false) => {
                depth += 1;
                out.push_str(&heading_prefix(attr(tag, "text:outline-level").and_then(|l| l.parse().ok()).unwrap_or(1)));
            }
            ("text:p", false) => {
                depth += 1;
                if std::mem::take(&mut list) {
                    out.push_str("- ");
                }
            }
            ("text:p", true) | ("text:h", true) => {
                depth = depth.saturating_sub(1);
                out.push(if cell > 0 { ' ' } else { '\n' });
            }
            ("text:list-item", false) => list = true,
            ("text:line-break", false) => out.push('\n'),
            ("text:tab", false) => out.push('\t'),
            ("text:s", false) => out.push_str(&" ".repeat(attr(tag, "text:c").and_then(|c| c.parse().ok()).unwrap_or(1))),
            ("table:table-cell", false) => cell += 1,
            ("table:table-cell", true) => {
                cell = cell.saturating_sub(1);
                out.push_str("| ");
            }
            ("table:table-row", true) => out.push('\n'),
            _ => {}
        }
    });
    out
}


// ---- Outlook .msg (a compound file with property streams) ---------------------------

type Msg = cfb::CompoundFile<std::fs::File>;

fn stream(comp: &mut Msg, path: &str) -> Option<Vec<u8>> {
    let mut s = comp.open_stream(path).ok()?;
    let mut v = Vec::new();
    s.read_to_end(&mut v).ok()?;
    Some(v)
}

/// String property `id` (4 hex digits) of the message (`storage` = "" ) or of an attachment storage.
fn prop_str(comp: &mut Msg, storage: &str, id: &str) -> Option<String> {
    if let Some(b) = stream(comp, &format!("{storage}/__substg1.0_{id}001F")) {
        let units: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        return Some(String::from_utf16_lossy(&units).trim_end_matches('\0').to_string());
    }
    let b = stream(comp, &format!("{storage}/__substg1.0_{id}001E"))?;
    Some(b.iter().take_while(|&&x| x != 0).map(|&x| crate::textenc::cp1250_char(x)).collect())
}

/// Delivery / submit time from the property stream of the message (FILETIME properties 0x0039, 0x0E06).
fn msg_date(comp: &mut Msg) -> Option<String> {
    let b = stream(comp, "/__properties_version1.0")?;
    let mut found = None;
    for e in b.get(32..)?.chunks_exact(16) {
        let tag = u32::from_le_bytes(e[0..4].try_into().ok()?);
        let (ty, id) = (tag & 0xFFFF, tag >> 16);
        if ty == 0x0040 && (id == 0x0039 || id == 0x0E06) {
            let ft = u64::from_le_bytes(e[8..16].try_into().ok()?);
            let secs = (ft / 10_000_000) as i64 - 11_644_473_600;
            found = chrono::DateTime::from_timestamp(secs, 0).map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string());
            if id == 0x0039 {
                break;
            }
        }
    }
    found
}

/// Very rough HTML to text for mail bodies without a plain text part.
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        out.push_str(&unescape(&rest[..lt]));
        let Some(gt) = rest[lt..].find('>') else { break };
        let tag = rest[lt + 1..lt + gt].trim_start_matches('/').to_ascii_lowercase();
        let name: String = tag.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
        if matches!(name.as_str(), "br" | "p" | "div" | "tr" | "li" | "h1" | "h2" | "h3" | "h4" | "table") {
            out.push('\n');
        }
        if matches!(name.as_str(), "script" | "style") && !rest[lt + 1..].starts_with('/') {
            let end = format!("</{name}");
            rest = match rest[lt..].to_ascii_lowercase().find(&end) {
                Some(p) => &rest[lt + p..],
                None => "",
            };
            continue;
        }
        rest = &rest[lt + gt + 1..];
    }
    out.push_str(&unescape(rest));
    let mut cleaned = String::new();
    let mut blank = 0;
    for line in out.lines().map(str::trim_end) {
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        cleaned.push_str(line);
        cleaned.push('\n');
    }
    cleaned
}

/// Subject, addresses, date, attachment names and the body of an Outlook `.msg` file.
pub fn msg_to_text(path: &Path) -> Option<String> {
    let mut comp = cfb::open(path).ok()?;
    let subject = prop_str(&mut comp, "", "0037");
    let from_name = prop_str(&mut comp, "", "0C1A").unwrap_or_default();
    let from_mail = prop_str(&mut comp, "", "5D01").or_else(|| prop_str(&mut comp, "", "0C1F")).unwrap_or_default();
    let to = prop_str(&mut comp, "", "0E04");
    let cc = prop_str(&mut comp, "", "0E03");
    let headers = prop_str(&mut comp, "", "007D");
    let date = headers
        .as_deref()
        .and_then(|h| h.lines().find(|l| l.len() > 5 && l[..5].eq_ignore_ascii_case("date:")))
        .map(|l| l[5..].trim().to_string())
        .or_else(|| msg_date(&mut comp));
    let body = prop_str(&mut comp, "", "1000").filter(|b| !b.trim().is_empty()).or_else(|| {
        let html = stream(&mut comp, "/__substg1.0_10130102")?;
        let text = String::from_utf8(html.clone()).unwrap_or_else(|_| html.iter().map(|&x| crate::textenc::cp1250_char(x)).collect());
        Some(html_to_text(&text))
    });
    let attach: Vec<String> = comp.walk().filter(|e| e.is_storage() && e.name().starts_with("__attach_version1.0_")).map(|e| e.path().to_string_lossy().replace('\\', "/")).collect();
    let mut files = Vec::new();
    for a in attach {
        let name = prop_str(&mut comp, &a, "3707").or_else(|| prop_str(&mut comp, &a, "3704")).unwrap_or_else(|| "(unnamed)".into());
        let size = stream(&mut comp, &format!("{a}/__substg1.0_37010102")).map(|d| d.len());
        files.push(match size {
            Some(n) => format!("{name} ({n} bytes)"),
            None => name,
        });
    }
    if subject.is_none() && body.is_none() {
        return None;
    }
    let mut out = String::new();
    out.push_str(&format!("Subject: {}\n", subject.unwrap_or_default()));
    out.push_str(&format!("From: {from_name} <{from_mail}>\n"));
    if let Some(t) = to.filter(|t| !t.is_empty()) {
        out.push_str(&format!("To: {t}\n"));
    }
    if let Some(c) = cc.filter(|c| !c.is_empty()) {
        out.push_str(&format!("Cc: {c}\n"));
    }
    if let Some(d) = date {
        out.push_str(&format!("Date: {d}\n"));
    }
    if !files.is_empty() {
        out.push_str(&format!("Attachments: {}\n", files.join("; ")));
    }
    out.push_str("\n----------------------------------------\n\n");
    out.push_str(&body.unwrap_or_default().replace("\r\n", "\n"));
    Some(out)
}

const RTF_SKIP: [&[u8]; 12] = [b"\\*", b"\\fonttbl", b"\\colortbl", b"\\stylesheet", b"\\info", b"\\pict", b"\\header", b"\\footer", b"\\themedata", b"\\datastore", b"\\listtable", b"\\listoverridetable"];

pub fn rtf_to_text(b: &[u8]) -> String {
    let mut out = String::new();
    let (mut i, mut depth, mut skip): (usize, usize, Option<usize>) = (0, 0, None);
    let mut uc = 1usize;
    let hex = |c: u8| (c as char).to_digit(16);
    while i < b.len() {
        match b[i] {
            b'{' => {
                depth += 1;
                if skip.is_none() && RTF_SKIP.iter().any(|d| b[i + 1..].starts_with(d)) {
                    skip = Some(depth);
                }
                i += 1;
            }
            b'}' => {
                if skip == Some(depth) {
                    skip = None;
                }
                depth = depth.saturating_sub(1);
                i += 1;
            }
            b'\\' => {
                i += 1;
                let Some(&c) = b.get(i) else { break };
                match c {
                    b'\\' | b'{' | b'}' => {
                        if skip.is_none() {
                            out.push(c as char);
                        }
                        i += 1;
                    }
                    b'\'' => {
                        if let (Some(h), Some(l)) = (b.get(i + 1).and_then(|&x| hex(x)), b.get(i + 2).and_then(|&x| hex(x))) {
                            if skip.is_none() {
                                out.push(crate::textenc::cp1250_char((h * 16 + l) as u8));
                            }
                            i += 3;
                        } else {
                            i += 1;
                        }
                    }
                    b'~' => {
                        if skip.is_none() {
                            out.push(' ');
                        }
                        i += 1;
                    }
                    b'_' => {
                        if skip.is_none() {
                            out.push('-');
                        }
                        i += 1;
                    }
                    c if c.is_ascii_alphabetic() => {
                        let s = i;
                        while i < b.len() && b[i].is_ascii_alphabetic() {
                            i += 1;
                        }
                        let word = std::str::from_utf8(&b[s..i]).unwrap_or("");
                        let ns = i;
                        if i < b.len() && b[i] == b'-' {
                            i += 1;
                        }
                        while i < b.len() && b[i].is_ascii_digit() {
                            i += 1;
                        }
                        let param: Option<i32> = std::str::from_utf8(&b[ns..i]).ok().and_then(|p| p.parse().ok());
                        if i < b.len() && b[i] == b' ' {
                            i += 1;
                        }
                        if skip.is_some() {
                            continue;
                        }
                        match word {
                            "par" | "line" | "sect" | "page" | "row" => out.push('\n'),
                            "tab" => out.push('\t'),
                            "cell" => out.push_str(" | "),
                            "bullet" => out.push('\u{2022}'),
                            "endash" => out.push('\u{2013}'),
                            "emdash" => out.push('\u{2014}'),
                            "lquote" => out.push('\u{2018}'),
                            "rquote" => out.push('\u{2019}'),
                            "ldblquote" => out.push('\u{201C}'),
                            "rdblquote" => out.push('\u{201D}'),
                            "uc" => uc = param.unwrap_or(1).max(0) as usize,
                            "u" => {
                                if let Some(n) = param {
                                    out.push(char::from_u32(n as i16 as u16 as u32).unwrap_or('?'));
                                }
                                // skip the fallback characters that follow
                                for _ in 0..uc {
                                    if b.get(i) == Some(&b'\\') && b.get(i + 1) == Some(&b'\'') {
                                        i += 4;
                                    } else if i < b.len() {
                                        i += 1;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    _ => i += 1,
                }
            }
            b'\r' | b'\n' => i += 1,
            c => {
                if skip.is_none() {
                    out.push(if c < 0x80 { c as char } else { crate::textenc::cp1250_char(c) });
                }
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docx() {
        let xml = r#"<?xml version="1.0"?><w:document><w:body>
            <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Title &amp; more</w:t></w:r></w:p>
            <w:p><w:r><w:t xml:space="preserve">Hello </w:t></w:r><w:r><w:t>world</w:t></w:r></w:p>
            <w:p><w:pPr><w:numPr><w:ilvl w:val="0"/></w:numPr></w:pPr><w:r><w:t>item</w:t></w:r></w:p>
            <w:tbl><w:tr><w:tc><w:p><w:r><w:t>a</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>b</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
            </w:body></w:document>"#;
        let t = docx_to_text(xml);
        assert!(t.contains("# Title & more\n"), "{t:?}");
        assert!(t.contains("Hello world\n"), "{t:?}");
        assert!(t.contains("- item\n"), "{t:?}");
        assert!(t.contains("a | b | \n"), "{t:?}");
    }

    #[test]
    fn odt() {
        let xml = r#"<office:text><text:h text:outline-level="2">Head</text:h><text:p>One<text:s text:c="2"/>two<text:line-break/>three</text:p><text:list><text:list-item><text:p>li</text:p></text:list-item></text:list></office:text>"#;
        let t = odt_to_text(xml);
        assert!(t.contains("## Head\n") && t.contains("One  two\nthree\n") && t.contains("- li\n"), "{t:?}");
    }

    #[test]
    fn rtf() {
        let t = rtf_to_text(br"{\rtf1\ansi{\fonttbl{\f0 Arial;}}{\*\generator x;}Hello \b bold\b0  world\par Second \u269? l\'edne\par}");
        assert_eq!(t, "Hello bold world\nSecond \u{10D} l\u{ED}ne\n");
    }

    #[test]
    fn msg_file() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("gc_msg_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.msg");
        let utf16 = |s: &str| s.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<u8>>();
        {
            let mut c = cfb::create(&path).unwrap();
            c.create_stream("/__substg1.0_0037001F").unwrap().write_all(&utf16("Hello subject")).unwrap();
            c.create_stream("/__substg1.0_0C1A001F").unwrap().write_all(&utf16("Alice")).unwrap();
            c.create_stream("/__substg1.0_0C1F001F").unwrap().write_all(&utf16("alice@example.com")).unwrap();
            c.create_stream("/__substg1.0_1000001F").unwrap().write_all(&utf16("Body line\r\nSecond")).unwrap();
            c.create_storage("/__attach_version1.0_#00000000").unwrap();
            c.create_stream("/__attach_version1.0_#00000000/__substg1.0_3707001F").unwrap().write_all(&utf16("report.pdf")).unwrap();
            c.create_stream("/__attach_version1.0_#00000000/__substg1.0_37010102").unwrap().write_all(b"12345").unwrap();
            c.flush().unwrap();
        }
        let t = msg_to_text(&path).unwrap();
        assert!(t.contains("Subject: Hello subject\n") && t.contains("From: Alice <alice@example.com>") && t.contains("Attachments: report.pdf (5 bytes)") && t.contains("Body line\nSecond"), "{t}");
        assert_eq!(extract(&path).map(|e| e.1), Some("msg"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn html_body() {
        let t = html_to_text("<html><style>p{}</style><body><p>Hello &amp; welcome</p><div>line two<br>three</div></body></html>");
        assert!(t.contains("Hello & welcome\n") && t.contains("line two\nthree"), "{t:?}");
        assert!(!t.contains("p{}"));
    }

    #[test]
    fn docx_from_zip() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("gc_doctext_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.docx");
        let mut z = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        z.start_file("word/document.xml", zip::write::SimpleFileOptions::default()).unwrap();
        z.write_all(b"<w:p><w:r><w:t>Hi</w:t></w:r></w:p>").unwrap();
        z.finish().unwrap();
        assert_eq!(extract(&path), Some(("Hi\n".to_string(), "docx")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
