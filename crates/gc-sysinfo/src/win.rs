//! Thin wrappers over the Win32 calls the System Info window needs.

use std::ffi::c_void;
use std::mem::{size_of, zeroed};

use windows::core::PCWSTR;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
use windows::Win32::Graphics::Gdi::{EnumDisplayDevicesW, EnumDisplaySettingsW, DEVMODEW, DISPLAY_DEVICEW, ENUM_CURRENT_SETTINGS};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ};
use windows::Win32::System::SystemInformation::{GetFirmwareType, GetSystemFirmwareTable, GetTickCount64, GlobalMemoryStatusEx, FIRMWARE_TABLE_PROVIDER, FIRMWARE_TYPE, MEMORYSTATUSEX};
use windows::Win32::System::TpmBaseServices::{Tbsi_GetDeviceInfo, TPM_DEVICE_INFO};

/// True when the DLL can be loaded; the optional DLLs are delay-loaded so the exe also starts without them (WinPE).
fn dll_available(name: &str) -> bool {
    use windows::Win32::System::LibraryLoader::LoadLibraryW;
    let name = wide(name);
    // SAFETY: NUL-terminated name; the handle is intentionally kept (the DLL is needed afterwards).
    unsafe { LoadLibraryW(PCWSTR(name.as_ptr())).is_ok() }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(buf.len())]).trim().to_owned()
}

/// A string value under `HKEY_LOCAL_MACHINE`.
pub fn reg_string(key: &str, name: &str) -> Option<String> {
    let (key, name) = (wide(key), wide(name));
    let mut bytes = 0u32;
    // SAFETY: NUL-terminated names; the buffer is sized by the first call.
    unsafe {
        if RegGetValueW(HKEY_LOCAL_MACHINE, PCWSTR(key.as_ptr()), PCWSTR(name.as_ptr()), RRF_RT_REG_SZ, None, None, Some(&mut bytes)).is_err() {
            return None;
        }
        let mut buf = vec![0u16; bytes as usize / 2 + 1];
        let mut bytes = (buf.len() * 2) as u32;
        RegGetValueW(HKEY_LOCAL_MACHINE, PCWSTR(key.as_ptr()), PCWSTR(name.as_ptr()), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as *mut c_void), Some(&mut bytes)).is_ok().then(|| from_wide(&buf))
    }
}

/// A DWORD value under `HKEY_LOCAL_MACHINE`.
pub fn reg_dword(key: &str, name: &str) -> Option<u32> {
    let (key, name) = (wide(key), wide(name));
    let (mut value, mut bytes) = (0u32, 4u32);
    // SAFETY: `value` is 4 bytes, as announced in `bytes`.
    unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, PCWSTR(key.as_ptr()), PCWSTR(name.as_ptr()), RRF_RT_REG_DWORD, None, Some(&mut value as *mut u32 as *mut c_void), Some(&mut bytes)).is_ok().then_some(value) }
}

/// The raw SMBIOS table (without the `RawSMBIOSData` header).
pub fn smbios_table() -> Option<Vec<u8>> {
    // The provider signature 'RSMB' is packed big-endian.
    let provider = FIRMWARE_TABLE_PROVIDER(u32::from_be_bytes(*b"RSMB"));
    // SAFETY: the buffer has the size we pass.
    unsafe {
        let size = GetSystemFirmwareTable(provider, 0, None);
        if size < 8 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        if GetSystemFirmwareTable(provider, 0, Some(&mut buf)) == 0 {
            return None;
        }
        let len = u32::from_le_bytes(buf[4..8].try_into().ok()?) as usize;
        Some(buf[8..(8 + len).min(buf.len())].to_vec())
    }
}

/// (total, available) physical memory in bytes.
pub fn memory() -> Option<(u64, u64)> {
    let mut status = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    // SAFETY: dwLength is set as the API requires.
    unsafe { GlobalMemoryStatusEx(&mut status).ok()? };
    Some((status.ullTotalPhys, status.ullAvailPhys))
}

pub fn uptime_secs() -> u64 {
    // SAFETY: no arguments.
    unsafe { GetTickCount64() / 1000 }
}

/// `true` for UEFI, `false` for legacy BIOS.
pub fn is_uefi() -> Option<bool> {
    let mut kind = FIRMWARE_TYPE(0);
    // SAFETY: out pointer is valid.
    unsafe { GetFirmwareType(&mut kind).ok()? };
    Some(kind.0 == 2)
}

/// TPM spec version as reported by the TPM Base Services ("2.0", "1.2").
pub fn tpm_version() -> Option<&'static str> {
    // WinPE has no tbs.dll; it is delay-loaded, so check before the first call.
    if !dll_available("tbs.dll") {
        return None;
    }
    // SAFETY: the struct is plain data, zero-initialised, and its size is passed.
    unsafe {
        let mut info: TPM_DEVICE_INFO = zeroed();
        if Tbsi_GetDeviceInfo(size_of::<TPM_DEVICE_INFO>() as u32, &mut info as *mut _ as *mut c_void) != 0 {
            return None;
        }
        match info.tpmVersion {
            1 => Some("1.2"),
            2 => Some("2.0"),
            _ => None,
        }
    }
}

pub struct Gpu {
    pub name: String,
    pub vram: u64,
}

/// PCI id of a display adapter: (vendor, device, subsystem) from `PCI\VEN_xxxx&DEV_xxxx&SUBSYS_xxxxxxxx&...`.
type PciId = (u32, u32, u32);

fn parse_pci_id(instance_id: &str) -> Option<PciId> {
    let rest = instance_id.strip_prefix("PCI\\")?;
    let field = |tag: &str| rest.split('&').find_map(|p| p.strip_prefix(tag)).and_then(|h| u32::from_str_radix(&h[..h.find('\\').unwrap_or(h.len())], 16).ok());
    Some((field("VEN_")?, field("DEV_")?, field("SUBSYS_").unwrap_or(0)))
}

/// Display adapters that are really present on the PCI bus: one entry per physical card.
/// Virtual display drivers (headset links, remote desktop) show up in DXGI as extra adapters
/// of the same GPU, but not here.
fn pci_display_adapters() -> Vec<(String, PciId)> {
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDeviceInstanceIdW, SetupDiGetDeviceRegistryPropertyW, DIGCF_PRESENT, GUID_DEVCLASS_DISPLAY, SP_DEVINFO_DATA, SPDRP_DEVICEDESC,
    };
    use windows::Win32::Foundation::HWND;

    let mut out = Vec::new();
    // SAFETY: the info set is destroyed before returning; buffers are sized by their slices.
    unsafe {
        let Ok(set) = SetupDiGetClassDevsW(Some(&GUID_DEVCLASS_DISPLAY), PCWSTR::null(), HWND::default(), DIGCF_PRESENT) else { return out };
        let mut index = 0;
        loop {
            let mut data = SP_DEVINFO_DATA { cbSize: size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
            if SetupDiEnumDeviceInfo(set, index, &mut data).is_err() {
                break;
            }
            index += 1;
            let mut id = [0u16; 400];
            if SetupDiGetDeviceInstanceIdW(set, &data, Some(&mut id), None).is_err() {
                continue;
            }
            let Some(pci) = parse_pci_id(&from_wide(&id)) else { continue };
            let mut name = [0u8; 512];
            let name = match SetupDiGetDeviceRegistryPropertyW(set, &data, SPDRP_DEVICEDESC, None, Some(&mut name), None) {
                Ok(()) => from_wide(&name.chunks(2).filter(|c| c.len() == 2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>()),
                Err(_) => "Display adapter".to_owned(),
            };
            out.push((name, pci));
        }
        let _ = SetupDiDestroyDeviceInfoList(set);
    }
    out
}

/// Graphics cards with their dedicated memory. The cards come from the PCI bus (so a virtual
/// display driver does not double a card), the memory size from DXGI.
pub fn gpus() -> Vec<Gpu> {
    // (name, pci id, dedicated memory) of the DXGI hardware adapters.
    let mut dxgi = Vec::new();
    // SAFETY: plain COM calls on interfaces we own. WinPE has no dxgi.dll (delay-loaded).
    unsafe {
        if !dll_available("dxgi.dll") {
        } else if let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() {
            let mut i = 0;
            while let Ok(adapter) = factory.EnumAdapters1(i) {
                i += 1;
                if let Ok(d) = adapter.GetDesc1() {
                    if d.Flags & 2 == 0 {
                        dxgi.push((from_wide(&d.Description), (d.VendorId, d.DeviceId, d.SubSysId), d.DedicatedVideoMemory as u64));
                    }
                }
            }
        }
    }
    let pci = pci_display_adapters();
    if pci.is_empty() {
        // No PnP data: fall back to DXGI, merging the identical duplicates a virtual driver causes.
        dxgi.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1 && a.2 == b.2);
        return dxgi.into_iter().map(|(name, _, vram)| Gpu { name, vram }).collect();
    }
    pci.into_iter()
        .map(|(name, id)| {
            // Each physical card takes one matching DXGI adapter, so two identical cards stay two.
            let vram = dxgi.iter().position(|d| d.1 == id).map(|i| dxgi.remove(i).2).unwrap_or(0);
            Gpu { name, vram }
        })
        .collect()
}

pub struct Display {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub hz: u32,
    pub primary: bool,
}

/// Monitors attached to the desktop with their current mode.
pub fn displays() -> Vec<Display> {
    let mut out = Vec::new();
    // SAFETY: structs are zeroed and get their size field set before every call.
    unsafe {
        let mut i = 0;
        loop {
            let mut device: DISPLAY_DEVICEW = zeroed();
            device.cb = size_of::<DISPLAY_DEVICEW>() as u32;
            if !EnumDisplayDevicesW(PCWSTR::null(), i, &mut device, 0).as_bool() {
                break;
            }
            i += 1;
            if device.StateFlags & 1 == 0 {
                continue; // not attached to the desktop
            }
            let mut mode: DEVMODEW = zeroed();
            mode.dmSize = size_of::<DEVMODEW>() as u16;
            if !EnumDisplaySettingsW(PCWSTR(device.DeviceName.as_ptr()), ENUM_CURRENT_SETTINGS, &mut mode).as_bool() {
                continue;
            }
            let mut monitor: DISPLAY_DEVICEW = zeroed();
            monitor.cb = size_of::<DISPLAY_DEVICEW>() as u32;
            let name = if EnumDisplayDevicesW(PCWSTR(device.DeviceName.as_ptr()), 0, &mut monitor, 0).as_bool() { from_wide(&monitor.DeviceString) } else { from_wide(&device.DeviceString) };
            out.push(Display { name, width: mode.dmPelsWidth, height: mode.dmPelsHeight, hz: mode.dmDisplayFrequency, primary: device.StateFlags & 4 != 0 });
        }
    }
    out
}

/// (on AC power, charge percent) when a battery is present.
pub fn battery() -> Option<(bool, Option<u8>)> {
    let mut status = SYSTEM_POWER_STATUS::default();
    // SAFETY: out pointer is valid.
    unsafe { GetSystemPowerStatus(&mut status).ok()? };
    if status.BatteryFlag & 128 != 0 {
        return None; // no system battery
    }
    Some((status.ACLineStatus == 1, (status.BatteryLifePercent <= 100).then_some(status.BatteryLifePercent)))
}

#[cfg(test)]
mod tests {
    use super::parse_pci_id;

    #[test]
    fn pci_instance_ids_are_parsed() {
        assert_eq!(parse_pci_id(r"PCI\VEN_10DE&DEV_2B85&SUBSYS_89EC1043&REV_A1\E7BF60DF942DB04800"), Some((0x10DE, 0x2B85, 0x89EC1043)));
        assert_eq!(parse_pci_id(r"ROOT\DISPLAY\0000"), None, "virtual displays are not PCI cards");
    }
}
