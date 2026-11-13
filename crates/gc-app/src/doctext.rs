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
