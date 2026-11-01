//! Minimal SMBIOS table reader: just the structures the System Info window shows.
//! Layouts follow the DMTF SMBIOS reference specification (offsets are from the structure start).

/// One structure: its formatted bytes (header included) and its string set.
pub struct Structure<'a> {
    pub kind: u8,
    data: &'a [u8],
    strings: Vec<&'a [u8]>,
}

/// Splits a raw SMBIOS table (without the Windows `RawSMBIOSData` header) into structures.
pub fn parse(table: &[u8]) -> Vec<Structure<'_>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 4 <= table.len() {
        let (kind, len) = (table[i], table[i + 1] as usize);
        if len < 4 || i + len > table.len() {
            break;
        }
        let data = &table[i..i + len];
        // The string set follows the formatted area and ends with a double NUL
        // (a structure without strings has just those two NULs).
        let mut strings = Vec::new();
        let mut j = i + len;
        if table.get(j) == Some(&0) {
            j += 2;
        } else {
            loop {
                let start = j;
                while j < table.len() && table[j] != 0 {
                    j += 1;
                }
                if j == start {
                    break;
                }
                strings.push(&table[start..j]);
                j += 1;
            }
            j += 1; // the final NUL
        }
        out.push(Structure { kind, data, strings });
        i = j;
        if kind == 127 {
            break;
        }
    }
    out
}

impl Structure<'_> {
    pub fn byte(&self, offset: usize) -> Option<u8> {
        self.data.get(offset).copied()
    }

    pub fn bytes(&self, offset: usize, len: usize) -> Option<&[u8]> {
        self.data.get(offset..offset + len)
    }

    pub fn word(&self, offset: usize) -> Option<u16> {
        Some(u16::from_le_bytes([*self.data.get(offset)?, *self.data.get(offset + 1)?]))
    }

    pub fn dword(&self, offset: usize) -> Option<u32> {
        Some(u32::from_le_bytes(self.data.get(offset..offset + 4)?.try_into().ok()?))
    }

    /// The string a byte field points to (1-based index into the string set; 0 = none).
    /// Empty and placeholder values ("To Be Filled By O.E.M.", ...) count as missing.
    pub fn string(&self, offset: usize) -> Option<String> {
        let index = self.byte(offset)? as usize;
        let raw = self.strings.get(index.checked_sub(1)?)?;
        let text = String::from_utf8_lossy(raw).trim().to_owned();
        let lower = text.to_ascii_lowercase();
        let placeholder = ["unknown", "to be filled", "default string", "not specified", "system product name", "system version", "none", "n/a"];
        (!text.is_empty() && !placeholder.iter().any(|p| lower.starts_with(p))).then_some(text)
    }
}

/// First structure of a type.
pub fn find<'a, 'b>(all: &'b [Structure<'a>], kind: u8) -> Option<&'b Structure<'a>> {
    all.iter().find(|s| s.kind == kind)
}

pub fn memory_type_name(code: u8) -> &'static str {
    match code {
        0x12 => "DDR",
        0x13 => "DDR2",
        0x18 => "DDR3",
        0x1A => "DDR4",
        0x1B => "LPDDR",
        0x1C => "LPDDR2",
        0x1D => "LPDDR3",
        0x1E => "LPDDR4",
        0x22 => "DDR5",
        0x23 => "LPDDR5",
        _ => "",
    }
}

pub fn battery_chemistry_name(code: u8) -> &'static str {
    match code {
        1 => "Other",
        3 => "Lead acid",
        4 => "Nickel cadmium",
        5 => "Nickel metal hydride",
        6 => "Lithium-ion",
        7 => "Zinc air",
        8 => "Lithium polymer",
        _ => "Unknown",
    }
}

/// Decodes one 2-bit field of the Hardware Security (type 24) byte.
pub fn security_state(bits: u8) -> &'static str {
    match bits & 3 {
        0 => "disabled",
        1 => "enabled",
        2 => "not implemented",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// type 1 (system) with 2 strings, then type 127 end marker.
    fn sample() -> Vec<u8> {
        let mut t = vec![1, 8, 0x01, 0x00, 1, 2, 0, 0];
        t.extend_from_slice(b"LENOVO\0ThinkPad X1\0\0");
        t.extend_from_slice(&[127, 4, 0x02, 0x00, 0, 0]);
        t
    }

    #[test]
    fn splits_structures_and_reads_strings() {
        let table = sample();
        let all = parse(&table);
        assert_eq!(all.len(), 2);
        let sys = find(&all, 1).unwrap();
        assert_eq!(sys.string(4).as_deref(), Some("LENOVO"));
        assert_eq!(sys.string(5).as_deref(), Some("ThinkPad X1"));
        assert_eq!(sys.string(6), None, "index 0 means no string");
        assert_eq!(all[1].kind, 127);
    }

    #[test]
    fn structure_without_strings_has_two_nuls() {
        // type 5 (4-byte header only), then type 6 with one string.
        let mut t = vec![5, 4, 0, 0, 0, 0];
        t.extend_from_slice(&[6, 5, 1, 0, 1]);
        t.extend_from_slice(b"x\0\0");
        let all = parse(&t);
        assert_eq!(all.iter().map(|s| s.kind).collect::<Vec<_>>(), [5, 6]);
        assert_eq!(all[1].string(4).as_deref(), Some("x"));
    }

    #[test]
    fn placeholder_strings_are_hidden() {
        let mut t = vec![2, 5, 0, 0, 1];
        t.extend_from_slice(b"To Be Filled By O.E.M.\0\0");
        assert_eq!(parse(&t)[0].string(4), None);
    }

    #[test]
    fn truncated_tables_do_not_panic() {
        assert!(parse(&[1, 200, 0, 0, 1, 2]).is_empty());
        assert!(parse(&[]).is_empty());
    }
}
