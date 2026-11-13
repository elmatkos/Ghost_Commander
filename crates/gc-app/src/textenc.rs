//! Text encodings shared by the F3 viewer and the F4 editor: UTF-8 (with or without BOM), UTF-16 with
//! BOM and Windows-1250 (the Central European code page, the fallback for bytes that are not UTF-8).

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Enc {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    Win1250,
}

impl Enc {
    pub const ALL: [Enc; 5] = [Enc::Utf8, Enc::Utf8Bom, Enc::Utf16Le, Enc::Utf16Be, Enc::Win1250];

    pub fn name(self) -> &'static str {
        match self {
            Enc::Utf8 => "UTF-8",
            Enc::Utf8Bom => "UTF-8 BOM",
            Enc::Utf16Le => "UTF-16 LE",
            Enc::Utf16Be => "UTF-16 BE",
            Enc::Win1250 => "Windows-1250",
        }
    }
}

/// Windows-1250 for the bytes 0x80..=0xFF. The five undefined bytes map to the C1 control with the same value.
const CP1250: [char; 128] = [
    '\u{20AC}', '\u{81}', '\u{201A}', '\u{83}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}', '\u{88}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{015A}', '\u{0164}', '\u{017D}', '\u{0179}', //
    '\u{90}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}', '\u{98}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{015B}', '\u{0165}', '\u{017E}', '\u{017A}', //
    '\u{A0}', '\u{02C7}', '\u{02D8}', '\u{0141}', '\u{A4}', '\u{0104}', '\u{A6}', '\u{A7}', '\u{A8}', '\u{A9}', '\u{015E}', '\u{AB}', '\u{AC}', '\u{AD}', '\u{AE}', '\u{017B}', //
    '\u{B0}', '\u{B1}', '\u{02DB}', '\u{0142}', '\u{B4}', '\u{B5}', '\u{B6}', '\u{B7}', '\u{B8}', '\u{0105}', '\u{015F}', '\u{BB}', '\u{013D}', '\u{02DD}', '\u{013E}', '\u{017C}', //
    '\u{0154}', '\u{C1}', '\u{C2}', '\u{0102}', '\u{C4}', '\u{0139}', '\u{0106}', '\u{C7}', '\u{010C}', '\u{C9}', '\u{0118}', '\u{CB}', '\u{011A}', '\u{CD}', '\u{CE}', '\u{010E}', //
    '\u{0110}', '\u{0143}', '\u{0147}', '\u{D3}', '\u{D4}', '\u{0150}', '\u{D6}', '\u{D7}', '\u{0158}', '\u{016E}', '\u{DA}', '\u{0170}', '\u{DC}', '\u{DD}', '\u{0162}', '\u{DF}', //
    '\u{0155}', '\u{E1}', '\u{E2}', '\u{0103}', '\u{E4}', '\u{013A}', '\u{0107}', '\u{E7}', '\u{010D}', '\u{E9}', '\u{0119}', '\u{EB}', '\u{011B}', '\u{ED}', '\u{EE}', '\u{010F}', //
    '\u{0111}', '\u{0144}', '\u{0148}', '\u{F3}', '\u{F4}', '\u{0151}', '\u{F6}', '\u{F7}', '\u{0159}', '\u{016F}', '\u{FA}', '\u{0171}', '\u{FC}', '\u{FD}', '\u{0163}', '\u{02D9}',
];

fn cp1250_reverse() -> &'static HashMap<char, u8> {
    static MAP: OnceLock<HashMap<char, u8>> = OnceLock::new();
    MAP.get_or_init(|| CP1250.iter().enumerate().map(|(i, &c)| (c, 0x80 + i as u8)).collect())
}

/// Text of a file and the encoding it was in. `None`: binary (NUL bytes outside UTF-16).
pub fn decode(b: &[u8]) -> Option<(String, Enc)> {
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
    match std::str::from_utf8(b) {
        Ok(s) => Some((s.to_string(), Enc::Utf8)),
        Err(_) => Some((b.iter().map(|&x| if x < 0x80 { x as char } else { CP1250[x as usize - 0x80] }).collect(), Enc::Win1250)),
    }
}

/// The bytes to write for `text` (`\n` line ends become `\r\n` when `crlf`). `Err(c)`: `c` cannot be
/// written in Windows-1250.
pub fn encode(text: &str, crlf: bool, enc: Enc) -> Result<Vec<u8>, char> {
    let text: Cow<str> = if crlf { Cow::Owned(text.replace('\n', "\r\n")) } else { Cow::Borrowed(text) };
    Ok(match enc {
        Enc::Utf8 => text.as_bytes().to_vec(),
        Enc::Utf8Bom => [&[0xEF, 0xBB, 0xBF][..], text.as_bytes()].concat(),
        Enc::Utf16Le => [0xFF, 0xFE].into_iter().chain(text.encode_utf16().flat_map(u16::to_le_bytes)).collect(),
        Enc::Utf16Be => [0xFE, 0xFF].into_iter().chain(text.encode_utf16().flat_map(u16::to_be_bytes)).collect(),
        Enc::Win1250 => {
            let rev = cp1250_reverse();
            let mut out = Vec::with_capacity(text.len());
            for c in text.chars() {
                if (c as u32) < 0x80 {
                    out.push(c as u8);
                } else {
                    out.push(*rev.get(&c).ok_or(c)?);
                }
            }
            out
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for enc in Enc::ALL {
            let bytes = encode("a\nžľšč€\n", true, enc);
            if enc == Enc::Win1250 {
                let bytes = bytes.unwrap();
                assert_eq!(decode(&bytes).unwrap(), ("a\r\nžľšč€\r\n".to_string(), Enc::Win1250));
            } else {
                assert_eq!(decode(&bytes.unwrap()).unwrap(), ("a\r\nžľšč€\r\n".to_string(), enc));
            }
        }
    }

    #[test]
    fn win1250_and_binary() {
        // "Příliš žluťoučký kůň" in Windows-1250
        let b = [0x50, 0xF8, 0xED, 0x6C, 0x69, 0x9A, 0x20, 0x9E, 0x6C, 0x75, 0x9D, 0x6F, 0x75, 0xE8, 0x6B, 0xFD, 0x20, 0x6B, 0xF9, 0xF2];
        assert_eq!(decode(&b).unwrap(), ("Příliš žluťoučký kůň".to_string(), Enc::Win1250));
        assert!(decode(b"MZ\0\0").is_none());
        assert_eq!(encode("日本", false, Enc::Win1250), Err('日'));
    }
}
