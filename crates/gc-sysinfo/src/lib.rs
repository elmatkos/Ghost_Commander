//! Hardware and OS summary for the System Info window (in the spirit of EFI Commander's F9 view).
//! Everything is plain data (`Section`s of label/value rows), so the UI only has to draw it.
//! Rows whose value the machine does not expose are left out instead of printing "Unknown".

mod smbios;
#[cfg(windows)]
mod win;

pub struct Section {
    pub title: &'static str,
    pub rows: Vec<(String, String)>,
}

#[derive(Default)]
pub struct SysInfo {
    pub sections: Vec<Section>,
}

impl SysInfo {
    /// Plain text for the clipboard or a bug report.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for s in &self.sections {
            out.push_str(&format!("== {} ==\n", s.title));
            for (label, value) in &s.rows {
                out.push_str(&format!("{label:<16}{value}\n"));
            }
            out.push('\n');
        }
        out
    }
}

/// `1.5 GB` style size.
pub fn fmt_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let (mut v, mut u) = (bytes as f64, 0);
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{bytes} B") } else { format!("{v:.1} {}", UNITS[u]) }
}

fn fmt_uptime(secs: u64) -> String {
    let (d, h, m) = (secs / 86400, secs % 86400 / 3600, secs % 3600 / 60);
    if d > 0 { format!("{d} d {h} h {m} min") } else { format!("{h} h {m} min") }
}

struct Rows(Vec<(String, String)>);

impl Rows {
    fn add(&mut self, label: &str, value: impl Into<String>) {
        self.0.push((label.to_owned(), value.into()));
    }

    fn add_opt(&mut self, label: &str, value: Option<String>) {
        if let Some(v) = value {
            self.add(label, v);
        }
    }
}

/// Gathers everything. Takes a moment (firmware tables, graphics adapters, drives): call it
/// from a worker thread.
#[cfg(windows)]
pub fn collect() -> SysInfo {
    use smbios::{find, parse};

    let table = win::smbios_table();
    let all = table.as_deref().map(parse).unwrap_or_default();
    let mut sections = Vec::new();

    // ---- System ----
    let mut r = Rows(Vec::new());
    if let Some(s) = find(&all, 1) {
        r.add_opt("Manufacturer", s.string(4));
        r.add_opt("Model", s.string(5));
        r.add_opt("Model name", s.string(6)); // Lenovo keeps the marketing name here
        r.add_opt("Family", s.string(26));
        r.add_opt("SKU", s.string(25));
        r.add_opt("Serial", s.string(7));
    }
    if let Some(s) = find(&all, 3) {
        r.add_opt("Asset tag", s.string(8));
    }
    if let Some(s) = find(&all, 2) {
        let board: Vec<String> = [s.string(4), s.string(5), s.string(6)].into_iter().flatten().collect();
        if !board.is_empty() {
            r.add("Board", board.join(" "));
        }
    }
    r.add_opt("Computer name", std::env::var("COMPUTERNAME").ok());
    r.add_opt("User", std::env::var("USERNAME").ok());
    sections.push(Section { title: "System", rows: r.0 });

    // ---- Firmware and security ----
    let mut r = Rows(Vec::new());
    if let Some(s) = find(&all, 0) {
        r.add_opt("BIOS vendor", s.string(4));
        r.add_opt("BIOS version", s.string(5));
        r.add_opt("BIOS date", s.string(8));
        if let (Some(major), Some(minor)) = (s.byte(0x16), s.byte(0x17)) {
            if major != 0xFF {
                r.add("EC version", format!("{major}.{minor}"));
            }
        }
    }
    if let Some(table) = &table {
        let _ = table; // SMBIOS version lives in the Windows header we already stripped
    }
    r.add_opt("Boot mode", win::is_uefi().map(|u| if u { "UEFI" } else { "Legacy BIOS" }.to_owned()));
    let secure = win::reg_dword(r"SYSTEM\CurrentControlSet\Control\SecureBoot\State", "UEFISecureBootEnabled");
    r.add_opt("Secure Boot", secure.map(|v| if v == 1 { "Enabled" } else { "Disabled" }.to_owned()));
    let tpm = win::tpm_version().map(|v| format!("Present, spec {v}")).unwrap_or_else(|| "Not detected".to_owned());
    r.add("TPM", tpm);
    if let Some(s) = find(&all, 43) {
        if let (Some(v), Some(maj), Some(min), Some(fw)) = (s.bytes(4, 4).map(String::from_utf8_lossy), s.byte(8), s.byte(9), s.dword(10)) {
            r.add("TPM detail", format!("{}, spec {maj}.{min}, firmware {}.{}", v.trim_end_matches('\0'), fw >> 16, fw & 0xFFFF));
        }
    }
    if let Some(s) = find(&all, 24).and_then(|s| s.byte(4)) {
        r.add("Passwords", format!("Power-on {}, Admin {}, Keyboard {}", smbios::security_state(s >> 6), smbios::security_state(s >> 2), smbios::security_state(s >> 4)));
    }
    sections.push(Section { title: "Firmware and security", rows: r.0 });

    // ---- Processor ----
    let mut r = Rows(Vec::new());
    let cpu_key = r"HARDWARE\DESCRIPTION\System\CentralProcessor\0";
    r.add_opt("CPU", win::reg_string(cpu_key, "ProcessorNameString"));
    let (mut cores, mut threads) = (0u32, 0u32);
    for s in all.iter().filter(|s| s.kind == 4) {
        if let (Some(max), Some(cur)) = (s.word(20), s.word(22)) {
            if r.0.iter().all(|(l, _)| l != "Speed") {
                let cur = if cur == 0 { win::reg_dword(cpu_key, "~MHz").unwrap_or(0) as u16 } else { cur };
                r.add("Speed", format!("{cur} MHz (max {max} MHz)"));
            }
        }
        cores += s.byte(35).map_or(0, u32::from);
        threads += s.byte(37).map_or(0, u32::from);
    }
    let logical = std::thread::available_parallelism().map_or(0, |n| n.get() as u32);
    if cores > 0 {
        r.add("Cores", format!("{cores} cores, {} threads", threads.max(logical)));
    } else if logical > 0 {
        r.add("Threads", logical.to_string());
    }
    r.add("Features", cpu_features());
    sections.push(Section { title: "Processor", rows: r.0 });

    // ---- Memory ----
    let mut r = Rows(Vec::new());
    if let Some((total, free)) = win::memory() {
        r.add("Total", fmt_bytes(total));
        r.add("Available", format!("{} ({}% in use)", fmt_bytes(free), 100 - free * 100 / total.max(1)));
    }
    for s in all.iter().filter(|s| s.kind == 17) {
        let size = match s.word(12) {
            Some(0) | None => continue, // empty slot
            Some(0x7FFF) => s.dword(28).map_or(0, |mb| u64::from(mb & 0x7FFF_FFFF) << 20),
            Some(0xFFFF) => continue,
            Some(v) if v & 0x8000 != 0 => u64::from(v & 0x7FFF) << 10,
            Some(v) => u64::from(v) << 20,
        };
        let kind = s.byte(18).map_or("", smbios::memory_type_name);
        let speed = s.word(32).filter(|&v| v > 0).or(s.word(21).filter(|&v| v > 0)).map(|v| format!("{v} MT/s")).unwrap_or_default();
        let part = [s.string(23), s.string(26)].into_iter().flatten().collect::<Vec<_>>().join(" ");
        let detail = [kind.to_owned(), speed, part].into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join(", ");
        // Slot names repeat per channel ("DIMM 0"), so the bank locator is part of the label.
        let slot = [s.string(17), s.string(16)].into_iter().flatten().collect::<Vec<_>>().join(" ");
        r.add(&if slot.is_empty() { "Module".to_owned() } else { slot }, format!("{}  {detail}", fmt_bytes(size)));
    }
    sections.push(Section { title: "Memory", rows: r.0 });

    // ---- Graphics ----
    let mut r = Rows(Vec::new());
    for g in win::gpus() {
        r.add("GPU", if g.vram > 0 { format!("{} ({} dedicated)", g.name, fmt_bytes(g.vram)) } else { g.name });
    }
    for d in win::displays() {
        r.add(if d.primary { "Display (main)" } else { "Display" }, format!("{}x{} @ {} Hz  {}", d.width, d.height, d.hz, d.name));
    }
    sections.push(Section { title: "Graphics and displays", rows: r.0 });

    // ---- Battery ----
    let mut r = Rows(Vec::new());
    if let Some(s) = find(&all, 22) {
        r.add_opt("Battery", s.string(8));
        r.add_opt("Manufacturer", s.string(5));
        if let (Some(cap), Some(volt)) = (s.word(10), s.word(12)) {
            let mult = s.byte(21).filter(|&m| m > 0).map_or(1, u32::from);
            r.add("Design", format!("{} mWh, {volt} mV", u32::from(cap) * mult));
        }
        r.add_opt("Chemistry", s.byte(9).map(|c| smbios::battery_chemistry_name(c).to_owned()));
        r.add_opt("Manufactured", s.string(6));
    }
    if let Some((ac, pct)) = win::battery() {
        r.add("State", format!("{}{}", pct.map_or(String::new(), |p| format!("{p}%, ")), if ac { "on AC power" } else { "on battery" }));
    }
    if !r.0.is_empty() {
        sections.push(Section { title: "Battery", rows: r.0 });
    }

    // ---- Storage ----
    let mut r = Rows(Vec::new());
    for d in gc_core::drive_infos() {
        let kind = match d.kind {
            gc_core::DriveKind::Fixed => "fixed",
            gc_core::DriveKind::Removable => "removable",
            gc_core::DriveKind::Network => "network",
            gc_core::DriveKind::Optical => "optical",
            gc_core::DriveKind::Other => "other",
        };
        let space = if d.total > 0 { format!("{} free of {}", fmt_bytes(d.free), fmt_bytes(d.total)) } else { "no medium".to_owned() };
        r.add(&d.root.display().to_string(), format!("{kind}, {space}"));
    }
    sections.push(Section { title: "Storage", rows: r.0 });

    // ---- Operating system ----
    let mut r = Rows(Vec::new());
    let nt = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let build: u32 = win::reg_string(nt, "CurrentBuildNumber").and_then(|b| b.parse().ok()).unwrap_or(0);
    if let Some(name) = win::reg_string(nt, "ProductName") {
        // Windows 11 still reports "Windows 10" in this value.
        r.add("Windows", if build >= 22000 { name.replace("Windows 10", "Windows 11") } else { name });
    }
    let version = win::reg_string(nt, "DisplayVersion").unwrap_or_default();
    let revision = win::reg_dword(nt, "UBR").map_or(String::new(), |u| format!(".{u}"));
    r.add("Version", format!("{version} (build {build}{revision})").trim().to_owned());
    r.add("Uptime", fmt_uptime(win::uptime_secs()));
    sections.push(Section { title: "Operating system", rows: r.0 });

    sections.retain(|s| !s.rows.is_empty());
    SysInfo { sections }
}

#[cfg(not(windows))]
pub fn collect() -> SysInfo {
    SysInfo { sections: vec![Section { title: "System", rows: vec![("Info".into(), "System Info is only implemented for Windows".into())] }] }
}

/// SIMD and crypto extensions the CPU offers (runtime-detected, the same checks the app's own
/// fast paths would use).
fn cpu_features() -> String {
    #[cfg(target_arch = "x86_64")]
    {
        let mut f = Vec::new();
        macro_rules! check {
            ($($name:tt),*) => { $(if std::arch::is_x86_feature_detected!($name) { f.push($name.to_ascii_uppercase()); })* };
        }
        check!("sse4.2", "avx", "avx2", "avx512f", "fma", "aes", "pclmulqdq", "sha", "bmi2");
        f.join(" ")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        String::from("n/a")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_uptime_are_readable() {
        assert_eq!(fmt_bytes(1536), "1.5 KB");
        assert_eq!(fmt_uptime(3 * 86400 + 4 * 3600 + 5 * 60), "3 d 4 h 5 min");
        assert_eq!(fmt_uptime(90), "0 h 1 min");
    }

    #[test]
    fn text_export_lists_sections_and_rows() {
        let info = SysInfo { sections: vec![Section { title: "CPU", rows: vec![("Cores".into(), "8".into())] }] };
        let text = info.to_text();
        assert!(text.contains("== CPU ==") && text.contains("Cores           8"));
    }

    #[cfg(windows)]
    #[test]
    fn collecting_on_this_machine_yields_the_basics() {
        let info = collect();
        let find = |title: &str, label: &str| info.sections.iter().find(|s| s.title == title).and_then(|s| s.rows.iter().find(|(l, _)| l == label)).map(|(_, v)| v.clone());
        assert!(find("Memory", "Total").is_some());
        assert!(find("Operating system", "Windows").is_some());
        assert!(find("Processor", "CPU").is_some());
    }
}
