//! Line scanners for the data / markup languages of `syntax`: JSON, XML / HTML, INI-like files,
//! Markdown and batch files.

use super::{find, is_ident, push, quoted, Kind, Span, State, BAT_KW};

fn is_name(c: u8) -> bool {
    is_ident(c) || c == b':' || c == b'-' || c == b'.'
}

pub fn scan_json(line: &str, st: State) -> (Vec<Span>, State) {
    let b = line.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut i = 0;
    if st == State::Block {
        match find(b, 0, b"*/") {
            Some(p) => {
                push(&mut out, 0..p + 2, Kind::Comment);
                i = p + 2;
            }
            None => {
                push(&mut out, 0..n, Kind::Comment);
                return (out, State::Block);
            }
        }
    }
    while i < n {
        let c = b[i];
        if c == b'"' {
            let (e, _) = quoted(b, i, b'"', Some(b'\\'), false);
            let mut k = e;
            while k < n && b[k].is_ascii_whitespace() {
                k += 1;
            }
            push(&mut out, i..e, if k < n && b[k] == b':' { Kind::Variable } else { Kind::String });
            i = e;
        } else if c == b'/' && i + 1 < n && b[i + 1] == b'/' {
            push(&mut out, i..n, Kind::Comment);
            return (out, State::Normal);
        } else if c == b'/' && i + 1 < n && b[i + 1] == b'*' {
            match find(b, i + 2, b"*/") {
                Some(p) => {
                    push(&mut out, i..p + 2, Kind::Comment);
                    i = p + 2;
                }
                None => {
                    push(&mut out, i..n, Kind::Comment);
                    return (out, State::Block);
                }
            }
        } else if c.is_ascii_digit() || (c == b'-' && i + 1 < n && b[i + 1].is_ascii_digit()) {
            let mut j = i + 1;
            while j < n && (b[j].is_ascii_alphanumeric() || matches!(b[j], b'.' | b'+' | b'-')) {
                j += 1;
            }
            push(&mut out, i..j, Kind::Number);
            i = j;
        } else if c.is_ascii_alphabetic() {
            let mut j = i;
            while j < n && b[j].is_ascii_alphanumeric() {
                j += 1;
            }
            if matches!(&line[i..j], "true" | "false" | "null") {
                push(&mut out, i..j, Kind::Keyword);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    (out, State::Normal)
}

pub fn scan_xml(line: &str, st: State) -> (Vec<Span>, State) {
    let b = line.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut i = 0;
    let mut in_tag = st == State::Tag;
    if st == State::Block {
        match find(b, 0, b"-->") {
            Some(p) => {
                push(&mut out, 0..p + 3, Kind::Comment);
                i = p + 3;
            }
            None => {
                push(&mut out, 0..n, Kind::Comment);
                return (out, State::Block);
            }
        }
    }
    while i < n {
        let c = b[i];
        if !in_tag {
            if b[i..].starts_with(b"<!--") {
                match find(b, i + 4, b"-->") {
                    Some(p) => {
                        push(&mut out, i..p + 3, Kind::Comment);
                        i = p + 3;
                    }
                    None => {
                        push(&mut out, i..n, Kind::Comment);
                        return (out, State::Block);
                    }
                }
            } else if c == b'<' && i + 1 < n {
                let mut j = i + 1;
                if b[j] == b'?' || b[j] == b'!' {
                    let e = find(b, j, b">").map_or(n, |e| e + 1);
                    push(&mut out, i..e, Kind::Preproc);
                    i = e;
                    continue;
                }
                if b[j] == b'/' {
                    j += 1;
                }
                let k0 = j;
                while j < n && is_name(b[j]) {
                    j += 1;
                }
                push(&mut out, k0..j, Kind::Keyword);
                in_tag = true;
                i = j.max(i + 1);
            } else if c == b'&' {
                match b[i..n.min(i + 12)].iter().position(|&x| x == b';') {
                    Some(p) => {
                        push(&mut out, i..i + p + 1, Kind::Preproc);
                        i += p + 1;
                    }
                    None => i += 1,
                }
            } else {
                i += 1;
            }
        } else if c == b'>' {
            in_tag = false;
            i += 1;
        } else if c == b'"' || c == b'\'' {
            let (e, _) = quoted(b, i, c, None, false);
            push(&mut out, i..e, Kind::String);
            i = e;
        } else if is_ident(c) {
            let mut j = i;
            while j < n && is_name(b[j]) {
                j += 1;
            }
            push(&mut out, i..j, Kind::Variable);
            i = j;
        } else {
            i += 1;
        }
    }
    (out, if in_tag { State::Tag } else { State::Normal })
}

/// Values after `=` in INI-like files: strings, numbers, true/false.
fn ini_value(line: &str, from: usize, out: &mut Vec<Span>) {
    let b = line.as_bytes();
    let n = b.len();
    let mut i = from;
    while i < n {
        let c = b[i];
        if c == b'"' || c == b'\'' {
            let (e, _) = quoted(b, i, c, if c == b'"' { Some(b'\\') } else { None }, false);
            push(out, i..e, Kind::String);
            i = e;
        } else if c.is_ascii_digit() {
            let mut j = i + 1;
            while j < n && (b[j].is_ascii_alphanumeric() || b[j] == b'.' || b[j] == b'_') {
                j += 1;
            }
            push(out, i..j, Kind::Number);
            i = j;
        } else if c.is_ascii_alphabetic() {
            let mut j = i;
            while j < n && b[j].is_ascii_alphanumeric() {
                j += 1;
            }
            if matches!(line[i..j].to_ascii_lowercase().as_str(), "true" | "false" | "yes" | "no" | "on" | "off" | "dword" | "hex" | "null") {
                push(out, i..j, Kind::Keyword);
            }
            i = j;
        } else {
            i += 1;
        }
    }
}

pub fn scan_ini(line: &str) -> Vec<Span> {
    let b = line.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let Some(f) = b.iter().position(|c| !c.is_ascii_whitespace()) else { return out };
    if b[f] == b';' || b[f] == b'#' {
        push(&mut out, f..n, Kind::Comment);
        return out;
    }
    if b[f] == b'[' {
        let e = find(b, f, b"]").map_or(n, |e| e + 1);
        push(&mut out, f..e, Kind::Type);
        return out;
    }
    // key [= value]; the key may be quoted (.reg files)
    let quoted_key = b[f] == b'"';
    let key_end = if quoted_key { quoted(b, f, b'"', Some(b'\\'), false).0 } else { b[f..].iter().position(|&c| c == b'=' || c == b':').map_or(n, |p| f + p) };
    let key_end = line[..key_end].trim_end().len().max(f);
    let eq = b[key_end..].iter().position(|&c| c == b'=' || (c == b':' && !quoted_key)).map(|p| key_end + p);
    match eq {
        Some(e) => {
            push(&mut out, f..key_end, Kind::Variable);
            ini_value(line, e + 1, &mut out);
        }
        None if quoted_key => push(&mut out, f..key_end, Kind::Variable),
        None => {}
    }
    out
}

pub fn scan_markdown(line: &str, st: State) -> (Vec<Span>, State) {
    let b = line.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let t = line.trim_start();
    let f = n - t.len();
    let fence = t.starts_with("```") || t.starts_with("~~~");
    if st == State::Fence {
        push(&mut out, 0..n, Kind::String);
        return (out, if fence { State::Normal } else { State::Fence });
    }
    if fence {
        push(&mut out, 0..n, Kind::String);
        return (out, State::Fence);
    }
    let hashes = t.bytes().take_while(|&c| c == b'#').count();
    if (1..=6).contains(&hashes) && (t.len() == hashes || t.as_bytes()[hashes] == b' ') {
        push(&mut out, f..n, Kind::Keyword);
        return (out, State::Normal);
    }
    if t.starts_with('>') {
        push(&mut out, f..n, Kind::Comment);
        return (out, State::Normal);
    }
    let mut i = f;
    // list markers: - * + or 1.
    let digits = t.bytes().take_while(|c| c.is_ascii_digit()).count();
    if t.len() > 1 && matches!(t.as_bytes()[0], b'-' | b'*' | b'+') && t.as_bytes()[1] == b' ' {
        push(&mut out, f..f + 1, Kind::Preproc);
        i = f + 1;
    } else if digits > 0 && t.len() > digits + 1 && t.as_bytes()[digits] == b'.' && t.as_bytes()[digits + 1] == b' ' {
        push(&mut out, f..f + digits + 1, Kind::Preproc);
        i = f + digits + 1;
    }
    while i < n {
        match b[i] {
            b'`' => match find(b, i + 1, b"`") {
                Some(e) => {
                    push(&mut out, i..e + 1, Kind::String);
                    i = e + 1;
                }
                None => i += 1,
            },
            b'[' => match find(b, i + 1, b"]") {
                Some(e) => {
                    push(&mut out, i..e + 1, Kind::Type);
                    i = e + 1;
                    if i < n && b[i] == b'(' {
                        if let Some(p) = find(b, i, b")") {
                            push(&mut out, i..p + 1, Kind::Variable);
                            i = p + 1;
                        }
                    }
                }
                None => i += 1,
            },
            _ => i += 1,
        }
    }
    (out, State::Normal)
}

pub fn scan_batch(line: &str) -> Vec<Span> {
    let b = line.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let Some(mut i) = b.iter().position(|c| !c.is_ascii_whitespace()) else { return out };
    if b[i] == b'@' {
        i += 1;
        if i >= n {
            return out;
        }
    }
    let rest = &line[i..];
    if rest.starts_with("::") || (rest.len() >= 3 && rest.is_char_boundary(3) && rest[..3].eq_ignore_ascii_case("rem") && (rest.len() == 3 || rest.as_bytes()[3].is_ascii_whitespace())) {
        push(&mut out, i..n, Kind::Comment);
        return out;
    }
    if b[i] == b':' && i + 1 < n && is_ident(b[i + 1]) {
        let e = b[i..].iter().position(|c| c.is_ascii_whitespace()).map_or(n, |p| i + p);
        push(&mut out, i..e, Kind::Preproc);
        return out;
    }
    while i < n {
        let c = b[i];
        if c == b'%' {
            let mut j = i + 1;
            if j < n && b[j] == b'%' {
                j += 1;
                while j < n && is_ident(b[j]) {
                    j += 1;
                }
            } else if j < n && (b[j].is_ascii_digit() || b[j] == b'~' || b[j] == b'*') {
                while j < n && (b[j].is_ascii_alphanumeric() || b[j] == b'~' || b[j] == b'*') {
                    j += 1;
                }
            } else if let Some(e) = find(b, j, b"%") {
                j = e + 1;
            } else {
                i += 1;
                continue;
            }
            push(&mut out, i..j, Kind::Variable);
            i = j;
        } else if c == b'!' && find(b, i + 1, b"!").is_some() {
            let e = find(b, i + 1, b"!").map_or(n, |p| p + 1);
            push(&mut out, i..e, Kind::Variable);
            i = e;
        } else if c == b'"' {
            let (e, _) = quoted(b, i, b'"', None, false);
            push(&mut out, i..e, Kind::String);
            i = e;
        } else if c.is_ascii_digit() {
            let mut j = i + 1;
            while j < n && b[j].is_ascii_alphanumeric() {
                j += 1;
            }
            push(&mut out, i..j, Kind::Number);
            i = j;
        } else if c == b'/' && i + 1 < n && b[i + 1].is_ascii_alphabetic() && (i == 0 || b[i - 1].is_ascii_whitespace()) {
            let mut j = i + 1;
            while j < n && b[j].is_ascii_alphanumeric() {
                j += 1;
            }
            push(&mut out, i..j, Kind::Param);
            i = j;
        } else if is_ident(c) && !c.is_ascii_digit() {
            let mut j = i;
            while j < n && is_ident(b[j]) {
                j += 1;
            }
            if BAT_KW.contains(&line[i..j].to_ascii_lowercase().as_str()) {
                push(&mut out, i..j, Kind::Keyword);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::{scan, Lang};
    use super::*;

    fn kinds(lang: Lang, line: &str) -> Vec<(String, Kind)> {
        scan(lang, line, State::Normal).0.into_iter().map(|(r, k)| (line[r].to_string(), k)).collect()
    }

    #[test]
    fn json_xml_ini() {
        let k = kinds(Lang::Json, r#"  "name": "x", "n": -1.5e3, "ok": true, // c"#);
        assert!(k.contains(&(r#""name""#.into(), Kind::Variable)) && k.contains(&(r#""x""#.into(), Kind::String)));
        assert!(k.contains(&("-1.5e3".into(), Kind::Number)) && k.contains(&("true".into(), Kind::Keyword)) && k.contains(&("// c".into(), Kind::Comment)));
        let k = kinds(Lang::Xml, r#"<item id="3" name='a'>x &amp; y</item>"#);
        assert!(k.contains(&("item".into(), Kind::Keyword)) && k.contains(&("id".into(), Kind::Variable)) && k.contains(&(r#""3""#.into(), Kind::String)) && k.contains(&("&amp;".into(), Kind::Preproc)));
        assert_eq!(scan(Lang::Xml, "<!-- start", State::Normal).1, State::Block);
        assert_eq!(scan(Lang::Xml, "<a href=\"x\"", State::Normal).1, State::Tag);
        assert_eq!(scan(Lang::Xml, "  b=\"1\">", State::Tag).1, State::Normal);
        assert_eq!(kinds(Lang::Ini, "[Main]"), vec![("[Main]".into(), Kind::Type)]);
        let k = kinds(Lang::Ini, "Path = \"C:\\x\" ; n 42");
        assert!(k.contains(&("Path".into(), Kind::Variable)) && k.contains(&("\"C:\\x\"".into(), Kind::String)));
        assert_eq!(kinds(Lang::Ini, "; note"), vec![("; note".into(), Kind::Comment)]);
    }

    #[test]
    fn markdown_and_batch() {
        assert_eq!(kinds(Lang::Markdown, "## Title"), vec![("## Title".into(), Kind::Keyword)]);
        assert_eq!(scan(Lang::Markdown, "```rust", State::Normal).1, State::Fence);
        assert_eq!(scan(Lang::Markdown, "```", State::Fence).1, State::Normal);
        let k = kinds(Lang::Markdown, "- see `code` and [text](http://x)");
        assert!(k.contains(&("-".into(), Kind::Preproc)) && k.contains(&("`code`".into(), Kind::String)) && k.contains(&("[text]".into(), Kind::Type)));
        assert_eq!(kinds(Lang::Batch, "REM hello"), vec![("REM hello".into(), Kind::Comment)]);
        let k = kinds(Lang::Batch, "@echo off & if exist \"%~dp0x\" goto :end /q");
        assert!(k.contains(&("echo".into(), Kind::Keyword)) && k.contains(&("if".into(), Kind::Keyword)) && k.contains(&("/q".into(), Kind::Param)));
        assert_eq!(kinds(Lang::Batch, ":start"), vec![(":start".into(), Kind::Preproc)]);
        assert!(kinds(Lang::Batch, "set X=%PATH% %1").contains(&("%PATH%".into(), Kind::Variable)));
    }

    #[test]
    fn odd_lines_do_not_panic() {
        for lang in [Lang::Json, Lang::Xml, Lang::Ini, Lang::Markdown, Lang::Batch] {
            for l in ["", "@", "%", "!", "<", "<a", "\"", "[", "žluťoučký = ž", "rem", "re", "```", "#", "1."] {
                for st in [State::Normal, State::Block, State::Tag, State::Fence] {
                    let (spans, _) = scan(lang, l, st);
                    for (r, _) in spans {
                        assert!(l.is_char_boundary(r.start) && l.is_char_boundary(r.end), "{lang:?} {l:?}");
                    }
                }
            }
        }
    }
}
