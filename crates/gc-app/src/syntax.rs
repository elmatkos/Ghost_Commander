//! Small hand-written syntax highlighter (no regex, no big grammar packs): splits one source line
//! into coloured spans. Only the state at the start of a line (inside a block comment, a here-string,
//! ...) has to be carried from line to line, so a viewer can colour just the rows it shows.

use std::ops::Range;
use std::path::Path;

use eframe::egui::Color32;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    C,
    Cpp,
    CSharp,
    PowerShell,
    Rust,
    Python,
}

impl Lang {
    pub fn name(self) -> &'static str {
        match self {
            Lang::C => "C",
            Lang::Cpp => "C++",
            Lang::CSharp => "C#",
            Lang::PowerShell => "PowerShell",
            Lang::Rust => "Rust",
            Lang::Python => "Python",
        }
    }
}

/// The language of a file, from its extension.
pub fn detect(path: &Path) -> Option<Lang> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    Some(match ext.as_str() {
        "c" | "h" => Lang::C,
        "cpp" | "cc" | "cxx" | "c++" | "hpp" | "hh" | "hxx" | "inl" | "ipp" => Lang::Cpp,
        "cs" | "csx" => Lang::CSharp,
        "ps1" | "psm1" | "psd1" => Lang::PowerShell,
        "rs" => Lang::Rust,
        "py" | "pyw" => Lang::Python,
        _ => return None,
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Keyword,
    Type,
    String,
    Comment,
    Number,
    Preproc,
    Variable,
    Function,
    Param,
}

/// What a line starts inside of.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum State {
    #[default]
    Normal,
    /// `/* ... */` or PowerShell `<# ... #>`
    Block,
    /// PowerShell here-string; it ends at a line starting with the quote and `@`.
    PsHere(u8),
    /// Python triple-quoted string.
    Triple(u8),
    /// C# verbatim string (`@"..."`, `""` is an escaped quote).
    Verbatim,
}

pub type Span = (Range<usize>, Kind);

pub fn color(kind: Kind, dark: bool) -> Color32 {
    let (d, l) = match kind {
        Kind::Keyword => ((86, 156, 214), (0, 0, 255)),
        Kind::Type => ((78, 201, 176), (38, 127, 153)),
        Kind::String => ((206, 145, 120), (163, 21, 21)),
        Kind::Comment => ((106, 153, 85), (0, 128, 0)),
        Kind::Number => ((181, 206, 168), (9, 134, 88)),
        Kind::Preproc => ((189, 99, 197), (175, 0, 219)),
        Kind::Variable => ((156, 220, 254), (0, 16, 128)),
        Kind::Function => ((220, 220, 170), (121, 94, 38)),
        Kind::Param => ((200, 160, 230), (110, 70, 140)),
    };
    let (r, g, b) = if dark { d } else { l };
    Color32::from_rgb(r, g, b)
}

// ---- word lists -----------------------------------------------------------------

const C_KW: &[&str] = &[
    "auto", "break", "case", "const", "continue", "default", "do", "else", "enum", "extern", "for", "goto", "if", "inline", "register", "restrict", "return", "sizeof", "static", "struct", "switch", "typedef", "union", "volatile", "while", "true", "false", "NULL", "_Alignas", "_Atomic", "_Noreturn",
];
const C_TY: &[&str] = &["char", "double", "float", "int", "long", "short", "signed", "unsigned", "void", "bool", "_Bool", "size_t", "ssize_t", "int8_t", "int16_t", "int32_t", "int64_t", "uint8_t", "uint16_t", "uint32_t", "uint64_t", "uintptr_t", "intptr_t", "wchar_t", "FILE"];
const CPP_KW: &[&str] = &[
    "alignas", "alignof", "and", "auto", "break", "case", "catch", "class", "const", "consteval", "constexpr", "constinit", "const_cast", "continue", "co_await", "co_return", "co_yield", "decltype", "default", "delete", "do", "dynamic_cast", "else", "enum", "explicit", "export", "extern", "false", "final", "for", "friend", "goto", "if", "inline", "mutable", "namespace", "new", "noexcept", "not", "nullptr", "operator", "or", "override", "private", "protected", "public", "register", "reinterpret_cast", "requires", "return", "sizeof", "static", "static_assert", "static_cast", "struct", "switch", "template", "this", "thread_local", "throw", "true", "try", "typedef", "typeid", "typename", "union", "using", "virtual", "volatile", "while", "concept",
];
const CPP_TY: &[&str] = &["char", "char8_t", "char16_t", "char32_t", "double", "float", "int", "long", "short", "signed", "unsigned", "void", "bool", "wchar_t", "size_t", "int8_t", "int16_t", "int32_t", "int64_t", "uint8_t", "uint16_t", "uint32_t", "uint64_t", "string", "wstring", "vector", "map", "set", "unique_ptr", "shared_ptr"];
const CS_KW: &[&str] = &[
    "abstract", "as", "base", "break", "case", "catch", "checked", "class", "const", "continue", "default", "delegate", "do", "else", "enum", "event", "explicit", "extern", "false", "finally", "fixed", "for", "foreach", "goto", "if", "implicit", "in", "interface", "internal", "is", "lock", "namespace", "new", "null", "operator", "out", "override", "params", "private", "protected", "public", "readonly", "ref", "return", "sealed", "sizeof", "stackalloc", "static", "struct", "switch", "this", "throw", "true", "try", "typeof", "unchecked", "unsafe", "using", "virtual", "volatile", "while", "async", "await", "yield", "get", "set", "init", "value", "record", "required", "partial", "where", "when", "nameof", "var",
];
const CS_TY: &[&str] = &["bool", "byte", "char", "decimal", "double", "float", "int", "long", "object", "sbyte", "short", "string", "uint", "ulong", "ushort", "void", "dynamic", "nint", "nuint", "String", "Int32", "Int64", "Task", "List", "Dictionary"];
const PS_KW: &[&str] = &[
    "begin", "break", "catch", "class", "continue", "data", "do", "dynamicparam", "else", "elseif", "end", "enum", "exit", "filter", "finally", "for", "foreach", "from", "function", "hidden", "if", "in", "param", "process", "return", "static", "switch", "throw", "trap", "try", "until", "using", "while", "workflow",
];
const PS_OPS: &[&str] = &[
    "eq", "ne", "gt", "ge", "lt", "le", "like", "notlike", "match", "notmatch", "contains", "notcontains", "in", "notin", "replace", "and", "or", "not", "xor", "band", "bor", "bnot", "bxor", "is", "isnot", "as", "split", "join", "f", "ceq", "cne", "cgt", "cge", "clt", "cle", "clike", "cmatch", "ieq", "ine",
];
const PS_VERBS: &[&str] = &[
    "add", "clear", "close", "compare", "convert", "convertfrom", "convertto", "copy", "disable", "enable", "enter", "exit", "export", "find", "format", "get", "grant", "group", "hide", "import", "invoke", "join", "limit", "measure", "merge", "mount", "move", "new", "out", "ping", "pop", "push", "read", "receive", "register", "remove", "rename", "repair", "request", "reset", "resize", "restart", "restore", "resume", "revoke", "save", "search", "select", "send", "set", "show", "sort", "split", "start", "stop", "submit", "suspend", "switch", "test", "trace", "unblock", "undo", "uninstall", "unlock", "unregister", "update", "use", "wait", "watch", "where", "write", "foreach",
];
const RS_KW: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where", "while", "macro_rules",
];
const RS_TY: &[&str] = &["i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize", "f32", "f64", "bool", "char", "str", "String", "Vec", "Option", "Result", "Box", "Some", "None", "Ok", "Err"];
const PY_KW: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with", "yield", "True", "False", "None", "match", "case",
];
const PY_TY: &[&str] = &["int", "str", "float", "bool", "list", "dict", "set", "tuple", "bytes", "object", "self", "cls"];

fn keywords(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::C => C_KW,
        Lang::Cpp => CPP_KW,
        Lang::CSharp => CS_KW,
        Lang::PowerShell => PS_KW,
        Lang::Rust => RS_KW,
        Lang::Python => PY_KW,
    }
}

fn types(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::C => C_TY,
        Lang::Cpp => CPP_TY,
        Lang::CSharp => CS_TY,
        Lang::PowerShell => &[],
        Lang::Rust => RS_TY,
        Lang::Python => PY_TY,
    }
}

// ---- scanning -------------------------------------------------------------------

fn find(b: &[u8], from: usize, pat: &[u8]) -> Option<usize> {
    if from >= b.len() || pat.is_empty() {
        return None;
    }
    b[from..].windows(pat.len()).position(|w| w == pat).map(|p| p + from)
}

/// End (exclusive) of a quoted string starting at `i`, and whether it was closed on this line.
fn quoted(b: &[u8], i: usize, q: u8, escape: Option<u8>, doubled: bool) -> (usize, bool) {
    let n = b.len();
    let mut j = i + 1;
    while j < n {
        if Some(b[j]) == escape {
            j += 2;
            continue;
        }
        if b[j] == q {
            if doubled && j + 1 < n && b[j + 1] == q {
                j += 2;
                continue;
            }
            return (j + 1, true);
        }
        j += 1;
    }
    (n, false)
}

/// Index after the closing quote of a verbatim string whose content starts at `from`.
fn verbatim_end(b: &[u8], from: usize) -> Option<usize> {
    let n = b.len();
    let mut j = from;
    while j < n {
        if b[j] == b'"' {
            if j + 1 < n && b[j + 1] == b'"' {
                j += 2;
                continue;
            }
            return Some(j + 1);
        }
        j += 1;
    }
    None
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

fn push(out: &mut Vec<Span>, r: Range<usize>, k: Kind) {
    if r.start < r.end {
        out.push((r, k));
    }
}

/// Colours one line. `st` is what the line starts inside of; the returned state is for the next line.
pub fn scan(lang: Lang, line: &str, mut st: State) -> (Vec<Span>, State) {
    let b = line.as_bytes();
    let n = b.len();
    let mut out: Vec<Span> = Vec::new();
    let mut i = 0;
    let ps = lang == Lang::PowerShell;

    match st {
        State::Normal => {}
        State::Block => match find(b, 0, if ps { b"#>" } else { b"*/" }) {
            Some(p) => {
                push(&mut out, 0..p + 2, Kind::Comment);
                i = p + 2;
                st = State::Normal;
            }
            None => {
                push(&mut out, 0..n, Kind::Comment);
                return (out, st);
            }
        },
        State::PsHere(q) => {
            if n >= 2 && b[0] == q && b[1] == b'@' {
                push(&mut out, 0..2, Kind::String);
                i = 2;
                st = State::Normal;
            } else {
                push(&mut out, 0..n, Kind::String);
                return (out, st);
            }
        }
        State::Triple(q) => match find(b, 0, &[q, q, q]) {
            Some(p) => {
                push(&mut out, 0..p + 3, Kind::String);
                i = p + 3;
                st = State::Normal;
            }
            None => {
                push(&mut out, 0..n, Kind::String);
                return (out, st);
            }
        },
        State::Verbatim => match verbatim_end(b, 0) {
            Some(e) => {
                push(&mut out, 0..e, Kind::String);
                i = e;
                st = State::Normal;
            }
            None => {
                push(&mut out, 0..n, Kind::String);
                return (out, st);
            }
        },
    }

    let first = b.iter().position(|c| !c.is_ascii_whitespace());
    let c_like = matches!(lang, Lang::C | Lang::Cpp | Lang::CSharp | Lang::Rust);
    while i < n {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // comments
        if c_like && c == b'/' && i + 1 < n {
            if b[i + 1] == b'/' {
                push(&mut out, i..n, Kind::Comment);
                return (out, st);
            }
            if b[i + 1] == b'*' {
                match find(b, i + 2, b"*/") {
                    Some(p) => {
                        push(&mut out, i..p + 2, Kind::Comment);
                        i = p + 2;
                        continue;
                    }
                    None => {
                        push(&mut out, i..n, Kind::Comment);
                        return (out, State::Block);
                    }
                }
            }
        }
        if (ps || lang == Lang::Python) && c == b'#' {
            push(&mut out, i..n, Kind::Comment);
            return (out, st);
        }
        if ps && c == b'<' && i + 1 < n && b[i + 1] == b'#' {
            match find(b, i + 2, b"#>") {
                Some(p) => {
                    push(&mut out, i..p + 2, Kind::Comment);
                    i = p + 2;
                    continue;
                }
                None => {
                    push(&mut out, i..n, Kind::Comment);
                    return (out, State::Block);
                }
            }
        }
        // preprocessor directives
        if matches!(lang, Lang::C | Lang::Cpp | Lang::CSharp) && c == b'#' && Some(i) == first {
            let mut j = i + 1;
            while j < n && (b[j] == b' ' || b[j] == b'\t') {
                j += 1;
            }
            let w0 = j;
            while j < n && b[j].is_ascii_alphabetic() {
                j += 1;
            }
            push(&mut out, i..j, Kind::Preproc);
            if &line[w0..j] == "include" {
                let mut k = j;
                while k < n && b[k] == b' ' {
                    k += 1;
                }
                if k < n && b[k] == b'<' {
                    if let Some(e) = find(b, k, b">") {
                        push(&mut out, k..e + 1, Kind::String);
                        i = e + 1;
                        continue;
                    }
                }
            }
            i = j;
            continue;
        }
        // strings
        if lang == Lang::CSharp && (c == b'@' || c == b'$') {
            let mut j = i;
            let mut verbatim = false;
            while j < n && (b[j] == b'@' || b[j] == b'$') {
                verbatim |= b[j] == b'@';
                j += 1;
            }
            if j < n && b[j] == b'"' {
                if verbatim {
                    match verbatim_end(b, j + 1) {
                        Some(e) => {
                            push(&mut out, i..e, Kind::String);
                            i = e;
                        }
                        None => {
                            push(&mut out, i..n, Kind::String);
                            return (out, State::Verbatim);
                        }
                    }
                } else {
                    let (e, _) = quoted(b, j, b'"', Some(b'\\'), false);
                    push(&mut out, i..e, Kind::String);
                    i = e;
                }
                continue;
            }
        }
        if ps && c == b'@' && i + 1 < n && (b[i + 1] == b'"' || b[i + 1] == b'\'') && b[i + 2..].iter().all(|x| x.is_ascii_whitespace()) {
            push(&mut out, i..n, Kind::String);
            return (out, State::PsHere(b[i + 1]));
        }
        if c == b'"' || c == b'\'' {
            if lang == Lang::Python && i + 2 < n && b[i + 1] == c && b[i + 2] == c {
                match find(b, i + 3, &[c, c, c]) {
                    Some(p) => {
                        push(&mut out, i..p + 3, Kind::String);
                        i = p + 3;
                    }
                    None => {
                        push(&mut out, i..n, Kind::String);
                        return (out, State::Triple(c));
                    }
                }
                continue;
            }
            if lang == Lang::Rust && c == b'\'' {
                // a char literal, or a lifetime (left plain)
                if i + 1 < n && b[i + 1] == b'\\' {
                    let (e, _) = quoted(b, i, b'\'', Some(b'\\'), false);
                    push(&mut out, i..e, Kind::String);
                    i = e;
                } else if i + 2 < n && b[i + 2] == b'\'' {
                    push(&mut out, i..i + 3, Kind::String);
                    i += 3;
                } else {
                    i += 1;
                }
                continue;
            }
            let (escape, doubled) = if ps { (if c == b'"' { Some(b'`') } else { None }, true) } else { (Some(b'\\'), false) };
            let (e, _) = quoted(b, i, c, escape, doubled);
            push(&mut out, i..e, Kind::String);
            i = e;
            continue;
        }
        // numbers
        if c.is_ascii_digit() || (c == b'.' && i + 1 < n && b[i + 1].is_ascii_digit()) {
            let mut j = i + 1;
            while j < n && (b[j].is_ascii_alphanumeric() || b[j] == b'_' || (b[j] == b'.' && j + 1 < n && b[j + 1].is_ascii_digit())) {
                j += 1;
            }
            push(&mut out, i..j, Kind::Number);
            i = j;
            continue;
        }
        // PowerShell variables and parameters
        if ps && c == b'$' && i + 1 < n {
            let mut j = i + 1;
            if b[j] == b'{' {
                j = find(b, j, b"}").map_or(n, |e| e + 1);
            } else if matches!(b[j], b'$' | b'?' | b'^') {
                j += 1;
            } else {
                while j < n && (is_ident(b[j]) || (b[j] == b':' && j + 1 < n && is_ident(b[j + 1]))) {
                    j += 1;
                }
            }
            if j > i + 1 {
                push(&mut out, i..j, Kind::Variable);
                i = j;
                continue;
            }
        }
        if ps && c == b'-' && i + 1 < n && b[i + 1].is_ascii_alphabetic() && (i == 0 || b[i - 1].is_ascii_whitespace() || b[i - 1] == b'(') {
            let mut j = i + 1;
            while j < n && b[j].is_ascii_alphanumeric() {
                j += 1;
            }
            let w = line[i + 1..j].to_ascii_lowercase();
            push(&mut out, i..j, if PS_OPS.contains(&w.as_str()) { Kind::Keyword } else { Kind::Param });
            i = j;
            continue;
        }
        // words
        if is_ident(c) && !c.is_ascii_digit() {
            let mut j = i;
            while j < n && is_ident(b[j]) {
                j += 1;
            }
            let word = &line[i..j];
            // Python string prefixes: r"..", b'..', f"..", rb".."
            if lang == Lang::Python && word.len() <= 2 && word.bytes().all(|x| b"rbfuRBFU".contains(&x)) && j < n && (b[j] == b'"' || b[j] == b'\'') {
                push(&mut out, i..j, Kind::String);
                i = j;
                continue;
            }
            if ps {
                let lower = word.to_ascii_lowercase();
                // Verb-Noun cmdlets
                if j + 1 < n && b[j] == b'-' && b[j + 1].is_ascii_alphabetic() && PS_VERBS.contains(&lower.as_str()) {
                    let mut k = j + 1;
                    while k < n && b[k].is_ascii_alphanumeric() {
                        k += 1;
                    }
                    push(&mut out, i..k, Kind::Function);
                    i = k;
                    continue;
                }
                if PS_KW.contains(&lower.as_str()) {
                    push(&mut out, i..j, Kind::Keyword);
                }
                i = j;
                continue;
            }
            if keywords(lang).contains(&word) {
                push(&mut out, i..j, Kind::Keyword);
            } else if types(lang).contains(&word) {
                push(&mut out, i..j, Kind::Type);
            } else {
                let mut k = j;
                while k < n && b[k] == b' ' {
                    k += 1;
                }
                let after_bang = lang == Lang::Rust && k < n && b[k] == b'!';
                if k < n && (b[k] == b'(' || after_bang) {
                    push(&mut out, i..j, Kind::Function);
                }
            }
            i = j;
            continue;
        }
        i += 1;
    }
    (out, st)
}

/// The state at the start of every line (computed once, so any line can be coloured on its own).
pub fn line_states<'a>(lang: Lang, lines: impl Iterator<Item = &'a str>) -> Vec<State> {
    let mut st = State::Normal;
    lines
        .map(|l| {
            let at_start = st;
            st = scan(lang, l, st).1;
            at_start
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(lang: Lang, line: &str) -> Vec<(String, Kind)> {
        scan(lang, line, State::Normal).0.into_iter().map(|(r, k)| (line[r].to_string(), k)).collect()
    }

    #[test]
    fn c_basics() {
        let k = kinds(Lang::C, "#include <stdio.h>");
        assert_eq!(k, vec![("#include".into(), Kind::Preproc), ("<stdio.h>".into(), Kind::String)]);
        let k = kinds(Lang::C, "  int x = 0x1F; // note");
        assert!(k.contains(&("int".into(), Kind::Type)) && k.contains(&("0x1F".into(), Kind::Number)) && k.contains(&("// note".into(), Kind::Comment)));
        let k = kinds(Lang::C, r#"printf("a \" b");"#);
        assert!(k.contains(&("printf".into(), Kind::Function)) && k.contains(&(r#""a \" b""#.into(), Kind::String)));
    }

    #[test]
    fn block_comment_spans_lines() {
        let (spans, st) = scan(Lang::Cpp, "int a; /* start", State::Normal);
        assert_eq!(st, State::Block);
        assert_eq!(spans.last().unwrap().1, Kind::Comment);
        let (spans, st) = scan(Lang::Cpp, "still */ return 1;", st);
        assert_eq!(st, State::Normal);
        assert_eq!(spans[0], (0..8, Kind::Comment));
        assert!(spans.iter().any(|(r, k)| *k == Kind::Keyword && &"still */ return 1;"[r.clone()] == "return"));
    }

    #[test]
    fn csharp_verbatim_and_keywords() {
        let (_, st) = scan(Lang::CSharp, r#"var s = @"line one"#, State::Normal);
        assert_eq!(st, State::Verbatim);
        let (spans, st) = scan(Lang::CSharp, r#"two ""q"" end"; int y;"#, st);
        assert_eq!(st, State::Normal);
        assert_eq!(spans[0].1, Kind::String);
        assert!(kinds(Lang::CSharp, "public async Task Run()").contains(&("async".into(), Kind::Keyword)));
    }

    #[test]
    fn powershell() {
        let k = kinds(Lang::PowerShell, "Get-ChildItem -Path $env:TEMP -Recurse | Where-Object { $_.Length -gt 5 } # c");
        assert!(k.contains(&("Get-ChildItem".into(), Kind::Function)));
        assert!(k.contains(&("-Path".into(), Kind::Param)));
        assert!(k.contains(&("$env:TEMP".into(), Kind::Variable)));
        assert!(k.contains(&("-gt".into(), Kind::Keyword)));
        assert!(k.contains(&("# c".into(), Kind::Comment)));
        let (_, st) = scan(Lang::PowerShell, "$x = @\"", State::Normal);
        assert_eq!(st, State::PsHere(b'"'));
        let (_, st) = scan(Lang::PowerShell, "\"@", st);
        assert_eq!(st, State::Normal);
        let (_, st) = scan(Lang::PowerShell, "<# help", State::Normal);
        assert_eq!(st, State::Block);
    }

    #[test]
    fn python_and_rust() {
        let (_, st) = scan(Lang::Python, "x = \"\"\"doc", State::Normal);
        assert_eq!(st, State::Triple(b'"'));
        let k = kinds(Lang::Rust, "fn f<'a>(c: char) -> &'a str { 'x'; println!(); }");
        assert!(k.contains(&("fn".into(), Kind::Keyword)) && k.contains(&("'x'".into(), Kind::String)) && k.contains(&("println".into(), Kind::Function)));
    }

    #[test]
    fn spans_are_ordered_and_on_char_boundaries() {
        let line = "const char* s = \"zażółć\"; // gęślą jaźń";
        let (spans, _) = scan(Lang::Cpp, line, State::Normal);
        let mut end = 0;
        for (r, _) in spans {
            assert!(r.start >= end && line.is_char_boundary(r.start) && line.is_char_boundary(r.end));
            end = r.end;
        }
    }

    #[test]
    fn extension_detection() {
        assert_eq!(detect(Path::new("a/B.CPP")), Some(Lang::Cpp));
        assert_eq!(detect(Path::new("x.ps1")), Some(Lang::PowerShell));
        assert_eq!(detect(Path::new("notes.txt")), None);
    }
}
