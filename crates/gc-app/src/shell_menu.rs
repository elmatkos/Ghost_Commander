//! The real Windows Explorer context menu (Open with, Send to, 7-Zip, Properties, ...) for
//! files and folders on disk, shown with the shell's own `IContextMenu`.

use std::path::PathBuf;

/// Shows the shell menu for `paths` (all in one folder) at the mouse position and runs the
/// chosen command. Blocks in the menu's own message loop until it is closed.
#[cfg(windows)]
pub fn show(hwnd: isize, paths: &[PathBuf]) -> Result<(), String> {
    // SAFETY: plain Win32 / COM calls; every pointer comes from the shell and is used
    // only while the owning object is alive (see `Pidl` and the `menu` binding).
    unsafe { imp::show(windows::Win32::Foundation::HWND(hwnd as _), paths).map_err(|e| e.to_string()) }
}

#[cfg(not(windows))]
pub fn show(_hwnd: isize, _paths: &[PathBuf]) -> Result<(), String> {
    Err("the shell menu is only available on Windows".into())
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::mem::{size_of, ManuallyDrop};
    use std::os::windows::ffi::OsStrExt;
    use std::path::PathBuf;
    use std::ptr::null_mut;

    use windows::core::{Interface, PCSTR, PCWSTR};
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::Common::ITEMIDLIST;
    use windows::Win32::UI::Shell::{
        DefSubclassProc, IContextMenu, IContextMenu2, IShellFolder, RemoveWindowSubclass, SHBindToParent, SHParseDisplayName,
        SetWindowSubclass, CMF_NORMAL, CMINVOKECOMMANDINFO,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreatePopupMenu, DestroyMenu, GetCursorPos, SetForegroundWindow, TrackPopupMenuEx, SW_SHOWNORMAL, TPM_RETURNCMD,
        TPM_RIGHTBUTTON, WM_DRAWITEM, WM_INITMENUPOPUP, WM_MEASUREITEM,
    };

    const ID_FIRST: u32 = 1;
    const ID_LAST: u32 = 0x7FFF;
    const SUBCLASS_ID: usize = 0x6743; // "gC"

    /// Absolute item id list allocated by the shell.
    struct Pidl(*mut ITEMIDLIST);

    impl Drop for Pidl {
        fn drop(&mut self) {
            // SAFETY: allocated by SHParseDisplayName, freed exactly once.
            unsafe { CoTaskMemFree(Some(self.0 as *const c_void)) }
        }
    }

    pub unsafe fn show(hwnd: HWND, paths: &[PathBuf]) -> windows::core::Result<()> {
        let mut pidls = Vec::new();
        for path in paths {
            let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            let mut pidl = null_mut();
            SHParseDisplayName(PCWSTR(wide.as_ptr()), None, &mut pidl, 0, None)?;
            pidls.push(Pidl(pidl));
        }
        let mut folder: Option<IShellFolder> = None;
        let mut children: Vec<*const ITEMIDLIST> = Vec::new();
        for pidl in &pidls {
            let mut last = null_mut();
            let parent: IShellFolder = SHBindToParent(pidl.0, Some(&mut last))?;
            children.push(last);
            folder.get_or_insert(parent);
        }
        let Some(folder) = folder else { return Ok(()) };
        let menu: IContextMenu = folder.GetUIObjectOf(hwnd, &children, None)?;

        let hmenu = CreatePopupMenu()?;
        let result = (|| {
            menu.QueryContextMenu(hmenu, 0, ID_FIRST, ID_LAST, CMF_NORMAL)?;

            // "Send to" / "Open with" fill their submenus lazily and are owner-drawn: the
            // menu object has to see these messages, which arrive at our window.
            let menu2: Option<IContextMenu2> = menu.cast().ok();
            let hooked = menu2.as_ref().is_some_and(|m| SetWindowSubclass(hwnd, Some(forward_menu_messages), SUBCLASS_ID, m.as_raw() as usize).as_bool());

            let mut at = POINT::default();
            GetCursorPos(&mut at)?;
            let _ = SetForegroundWindow(hwnd);
            let chosen = TrackPopupMenuEx(hmenu, (TPM_RETURNCMD | TPM_RIGHTBUTTON).0, at.x, at.y, hwnd, None);
            if hooked {
                let _ = RemoveWindowSubclass(hwnd, Some(forward_menu_messages), SUBCLASS_ID);
            }

            if chosen.0 > 0 {
                let info = CMINVOKECOMMANDINFO {
                    cbSize: size_of::<CMINVOKECOMMANDINFO>() as u32,
                    hwnd,
                    lpVerb: PCSTR((chosen.0 as u32 - ID_FIRST) as usize as *const u8),
                    nShow: SW_SHOWNORMAL.0,
                    ..Default::default()
                };
                menu.InvokeCommand(&info)?;
            }
            Ok(())
        })();
        let _ = DestroyMenu(hmenu);
        result
    }

    unsafe extern "system" fn forward_menu_messages(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _id: usize, menu2: usize) -> LRESULT {
        if matches!(msg, WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM) {
            // Borrowed from the caller, who keeps it alive: do not release it here.
            let menu2 = ManuallyDrop::new(IContextMenu2::from_raw(menu2 as *mut c_void));
            if menu2.HandleMenuMsg(msg, wparam, lparam).is_ok() {
                return LRESULT(if msg == WM_INITMENUPOPUP { 0 } else { 1 });
            }
        }
        DefSubclassProc(hwnd, msg, wparam, lparam)
    }
}

/// Opens `path` with whatever Windows has registered for it (the "open" verb): the image
/// viewer for a picture, the program itself for an `.exe`. Does not wait for it to finish.
#[cfg(windows)]
pub fn open(path: &std::path::Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let wide = |p: &std::path::Path| -> Vec<u16> { p.as_os_str().encode_wide().chain(Some(0)).collect() };
    let file = wide(path);
    // Programs expect to start in their own folder.
    let dir = path.parent().map(wide);
    // SAFETY: both buffers are NUL-terminated and outlive the call.
    let result = unsafe {
        ShellExecuteW(HWND::default(), w!("open"), PCWSTR(file.as_ptr()), PCWSTR::null(), dir.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())), SW_SHOWNORMAL)
    };
    // Values above 32 mean success; below are error codes.
    if result.0 as isize > 32 {
        Ok(())
    } else {
        Err(format!("Windows could not open it (code {})", result.0 as isize))
    }
}

#[cfg(not(windows))]
pub fn open(path: &std::path::Path) -> Result<(), String> {
    std::process::Command::new("xdg-open").arg(path).spawn().map(|_| ()).map_err(|e| e.to_string())
}
