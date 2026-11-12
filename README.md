# Ghost Commander

A fast, modern, dual-pane file manager written in Rust. It follows the Norton / Total / Midnight Commander tradition and aims to keep every feature of those three while improving on them.

**Status:** v0.25.1, Windows (x64), actively developed. Also runs under WinPE.

**Repo:** https://github.com/elmatkos/Ghost_Commander

## Goals

- Speed first: instant directory listing, no UI stalls, all I/O off the UI thread.
- Native archive support (7z, zip, tar, rar read) treated as folders, no external 7-Zip needed.
- Built-in image viewer: opens the full image at native size if it fits the monitor, otherwise scales proportionally to 75% of the monitor *height* (wide-monitor friendly).
- Modern look (dark/light themes, crisp fonts, icons) without losing keyboard-first workflow.

## Stack

| Area | Choice |
|---|---|
| Language | Rust (stable) |
| UI | `egui`/`eframe` (OpenGL), `egui-phosphor` icons |
| Archives | `sevenz-rust2`, `zip`, `tar`, `flate2`, `lzma-rust2`, `unrar` (read only) |
| Images | `image` (png, jpeg, gif, bmp, webp, ico) |
| Hashes | `crc32fast`, `md-5`, `sha1`, `sha2` |
| FTP / FTPS | `suppaftp` + `rustls` (certificates checked against the Windows store), passwords saved with Windows DPAPI |
| Windows | `windows` / `windows-sys` (shell menu, `CopyFileExW`, SMBIOS, drives) |

## Structure

```
Ghost_Commander/
  Cargo.toml            (workspace)
  crates/
    gc-core/            file ops, background jobs, hashing, file types, Windows fs
    gc-archive/         archive providers (7z, zip, tar, rar read) browsed as folders
    gc-sysinfo/         system information (SMBIOS, CPU, TPM, drives, ...)
    gc-ftp/             FTP / FTPS client, ftp:// link parsing, saved sessions
    gc-dism/            DISM image mounting: mounted-image query, WIM image list, background dism.exe jobs
    gc-app/             egui app: panels, viewers, themes, dialogs (binary ghost-commander)
```

## Roadmap

Done: dual panes, file operations with job queue, archives as folders (extract/pack), text and image viewers, checksums and verify, themes, System Info, fast copy, WinPE support, FTP / FTPS client, tabs, DISM manager.

Planned: file compare, sync dirs, multi-rename, search (incl. in archives), bookmarks, plugins/commands config, SFTP.

## How to run

`cargo run --release` (binary `ghost-commander`). Tests: `cargo test`.

## Current state

Dual panes, keyboard navigation (arrows, PgUp/PgDn, Home/End, Tab, Enter, Backspace, Space/Insert to mark), F5 copy, F6 move, F7 new folder, F8 delete to the recycle bin (Shift+F8 permanent), Ctrl+1/Ctrl+2 drive picker, date column. All file operations are cancellable background jobs with progress. Name conflicts open a dialog (overwrite / skip / rename / cancel, optionally for all remaining).

Branding lives in `assets/` (`icon.ico` is embedded into the exe by `crates/gc-app/build.rs`, `logo_512.png` is shown in the About window, Help > About).

## TODO (backlog)
- FTP: implicit FTPS (port 990), SFTP, upload back after F4 on a server file, server-to-server copy, transfers in parallel with browsing (a transfer currently holds the connection).
- Verify v0.19.0 copy: measure parallel file counts (1/2/4/8/16), test copies between C: D: F: G:, check how parallel copying looks in the GUI.

**Performance: caching and acceleration everywhere possible**
- Cache directory listings (keyed by path + change notification via `ReadDirectoryChangesW`), incremental refresh instead of full re-read.
- Cache icons, thumbnails and decoded images (LRU, optional on-disk thumbnail cache).
- Cache archive indexes (central directories) so reopening an archive is instant.
- Use modern CPU features wherever it pays off, with runtime detection and scalar fallback: AVX2/AVX-512 for search and compare, SHA-NI and PCLMULQDQ/CRC32 instructions for hashing, AES-NI, SIMD image resize/decode, BMI2. Applies to core and to every later feature (archives, hashing, compare, search).
- Parallelism: rayon for hashing, search, directory sizing; large-file copy with unbuffered/overlapped I/O; NTFS MFT/USN fast search.
- Measure first: benchmark suite (`criterion`) before and after each optimization.

**Release**
- Releases are signed (Certum certificate, signtool + RFC 3161 timestamp) and published as `ghost-commander-vX-windows-x64.zip` and `-winpe-x64.zip`. Still open: an installer and a CI signing step.

## Themes and file icons

Config > Themes opens the theme editor (live preview). Built-in themes: Dark, Light, Ghost, Norton Classic. Custom themes are TOML files in `%APPDATA%\Ghost Commander\themes\`; the editor saves them there, and the selected theme is remembered in `settings.toml`.

A theme sets the semantic colors (background, text, cursor, folders, marked, errors), a color and an icon per file type (`folder`, `archive`, `image`, `audio`, `video`, `document`, `code`, `text`, `executable`, `config`, `other`), and optional `extensions` overrides (e.g. `mkv = "video"`). An icon is a built-in name (Phosphor icon font: `folder`, `archive`, `star`, ...) or any single character. Minimal custom theme:

```toml
name = "Mine"
dark = true
background = "#101820"
text = "#e0e0e0"
accent = "#2a5db0"
dir = "#7fd1ff"
marked = "#ffd166"
error = "#ff6b6b"

[types.archive]
color = "#ff9f43"
icon = "package"

[extensions]
mkv = "video"
```

## Archives

Enter on any other file opens it with its Windows default program (pictures in the photo app, `.exe` runs, ...); Enter on a `.zip`, `.7z`, `.rar` (read only), `.tar`, `.tar.gz` (`.tgz`) or `.tar.xz` (`.txz`) opens it as a folder (browse, `..` leaves it); F5 extracts the marked items (or the one under the cursor) into the other panel's folder, with the same progress, cancel and conflict dialog as file copies. Names that could escape the destination (`..`, drive letters) are never listed or extracted. Archives inside archives open too: the inner file is extracted to a scratch folder in the background (header shows a spinner) and removed on exit. Archives open off the UI thread, and zip extraction runs on all cores. Alt+F5 packs the marked items (or the one under the cursor), with an editable target folder and name, into a new `.7z`, `.zip`, `.tar.gz` or `.tar.xz` (xz compresses on all cores) in the other panel's folder (never overwrites; adding to an existing archive is not supported yet). Browsing archives stays read-only. RAR archives are read through the UnRAR library (extraction only; no RAR creation exists, by licence). Tarballs are streams, so listing a big `.tar.gz` or `.tar.xz` reads it completely once.

## Context menu and Delete

Right click on files or folders on disk shows the real Windows Explorer menu (Open with, Send to, 7-Zip, Properties, ...); a right click on an unmarked row acts on that row only, on a marked row on all marked. Inside archives a small built-in menu offers Open and Extract. `Del` / `Shift+Del` work like F8 / Shift+F8. F3 opens a read-only text viewer (first 8 MB; scrolls with the mouse wheel and with Up/Down, PgUp/PgDn or Space, Home/End, Left/Right; source files are syntax-coloured: C, C++ (`.c .h .cpp .cc .cxx .hpp ...`), C# (`.cs`), PowerShell (`.ps1 .psm1 .psd1`), Rust and Python, by a small built-in highlighter in `syntax.rs` that colours only the rows on screen, with block comments, here-strings and verbatim strings carried across lines, light and dark colour sets; also for files inside archives, which are extracted to a scratch folder first, as for F4 and Enter), or the image viewer for png, jpg, gif, bmp, webp and ico (own window: native size if it fits the monitor, otherwise scaled to 75% of the monitor height; Left/Right browse the folder's images, `+` / `-` zoom, `0` resets, drag pans; Esc, Q or F3 closes it), F4 opens the built-in editor (`editor.rs`): the same syntax colours, line numbers, Tab inserts spaces, Ctrl+S saves, PgUp/PgDn move by a page, every file opens at line 1, Ctrl+F opens find / replace (F3 = next, match case option, replace all), Esc closes (asks when the file has unsaved changes); it keeps the file's encoding (UTF-8, UTF-8 with BOM, UTF-16 with BOM) and line endings (LF / CRLF). Files over 2 MB, binary files and other encodings open in Notepad instead.

## Menu, drive buttons and status line

The menu bar has Config (Options..., Themes...) and Help (About). Config > Options switches the drive buttons on or off (and the free-space bar under each drive); settings live in `%APPDATA%\Ghost Commander\settings.toml`. With drive buttons on, every panel gets a row of pills (type icon, letter, used-space bar, details on hover) plus `\` (root of the drive) and `..` (parent folder). The drive list is refreshed by a background thread, so a disconnected network drive cannot freeze the window. Under each list a status line shows `marked / all in files, dirs` as in Total Commander (folder sizes are not calculated).

## System Info

Info > System Info (F9) shows a scrollable summary in the style of EFI Commander's F9 view (crate `gc-sysinfo`): system and board (SMBIOS), BIOS and EC version, boot mode, Secure Boot, TPM, BIOS passwords, CPU with speed, cores, threads and the SIMD/crypto features it offers (SSE4.2, AVX2, AVX-512, SHA, AES, ...), memory with every installed module, GPUs and displays, battery, drives and the Windows build. Rows a machine does not expose are left out. The data is gathered on a worker thread; Copy puts it all on the clipboard as text. `cargo run -p gc-sysinfo --example dump` prints the same without the GUI.

## Tabs

Each side has a tab strip under the drive buttons, every tab in a thin frame: click a tab to switch, `+` opens a new tab in the same folder, the red square x on the active tab (or a middle click on any tab) closes it. Keyboard: Ctrl+T new tab, Ctrl+W close, Ctrl+Tab / Ctrl+Shift+Tab next / previous tab of the active side. Both sides reopen their tabs (and the shown one) at the next start (`left_tabs`, `right_tabs`, `left_tab`, `right_tab` in `settings.toml`). The last tab of a side cannot be closed. **Locked tabs**: Ctrl+L or right click a tab > Lock. A locked tab has an orange frame and a lock icon and keeps its folder: when you go anywhere else in it (Enter, Backspace, drive buttons, `cd`, opening an archive or a server), that place opens in a new tab next to it and the locked tab shows its folder again. Locks are remembered (a `*` before the folder in `left_tabs` / `right_tabs`).

## DISM manager

Tools > DISM manager (Ctrl+D) is a GUI for mounting Windows images (crate `gc-dism`, it drives `dism.exe`):

- **Mount**: pick a `.wim` / `.esd` / `.swm` (or press *Use file under cursor*), choose the image from the list (read from the file itself, no admin rights needed) and a mount folder. The folder is created when it does not exist; it must be empty. Optional read-only mount. The suggested folder is `mount` next to the image.
- **Always knows what is mounted**: the state is read from Windows (`Get-WindowsImage -Mounted`) at start, whenever the window opens, after every job and every five minutes, so images mounted by other tools or before a restart show up too. Ghost Commander additionally remembers when it mounted an image (`dism_mounts.toml` in `%APPDATA%\Ghost Commander\`).
- **Badge in the menu bar**: after *Help* a highlighted *n WIM mounted* button appears while something is mounted (red when a mount needs a remount), and *DISM working 42%* while a job runs. Hover lists the mounts, a click opens the manager.
- **Update / commit**: files added to the mounted folder are saved with *Commit (keep mounted)*, or *Commit and unmount*; *Discard and unmount* (asks first) drops all changes. Panels and tabs showing the folder move out of it first. *Remount* revives a mount interrupted by a reboot, *Clean up orphaned mounts* runs `/Cleanup-Wim`. *Show in panel* opens the mount in the active panel.
- **Administrator**: DISM needs it. When Ghost Commander is not elevated, each job asks for approval (UAC) and its output is followed live (progress bar and log); mounted Windows images are also often unreadable without elevation. `cargo run -p gc-dism --example probe -- <wim> <new mount folder>` runs mount, commit and unmount against the real DISM.

## Sorting and quick filter

Click a column heading (Name, Size, Date) to sort, click again to reverse; Ctrl+F3 sorts by name, Ctrl+F4 by extension, Ctrl+F5 by date, Ctrl+F6 by size. Folders always stay on top. Ctrl+F opens the quick filter under the list: typing narrows the panel to names containing the text (case-insensitive), Enter keeps the filtered list and gives the keyboard back, Esc clears it; entering another folder clears it too.

Ctrl+H (or Tools > Checksums) calculates CRC-32, MD5, SHA-1 and SHA-256 of the marked files, or of the file under the cursor, in one read pass; several files are hashed in parallel. Paste a checksum into "Compare with" and the matching value lights up. Folders are skipped and files inside archives are not supported yet. "Save .crc" writes `name.ext.crc` next to each file, one SFV-style line `name.ext 1A2B3C4D`.

Enter on a `.crc`, `.sfv`, `.md5`, `.sha1` or `.sha256` file verifies it: every file it lists (looked up next to the checksum file) is hashed in parallel and shown as OK, FAILED (with expected and actual value) or MISSING. SFV, `md5sum`/`sha256sum` and bare-hash files are understood; the algorithm follows the hash length.

Panels refresh themselves when files appear, disappear or are renamed in the shown folder (checked once a second); Ctrl+R refreshes at once. Shift+F4 asks for a name, creates an empty file there and opens it in the editor.

The bottom line shows only the shortcuts for the modifier keys you hold: plain keys normally, the Shift ones while Shift is held, likewise Ctrl and Alt. Only as many as fit the window width are shown.

F1 opens a Help window listing every function key and shortcut, grouped by modifier (function keys, Shift, Alt, Ctrl, FTP, moving around, mouse). Alt+F1 / Alt+F2 (and Ctrl+1 / Ctrl+2) pick a drive for the left / right panel. About now lives only in the Help menu. F9 opens System Info (scroll it with Up / Down / PgUp / PgDn / Home / End); the theme editor is in Config > Themes.

Copy and move (across drives) hand each file to Windows (`CopyFileExW`): the copy runs in the kernel, a network share can copy server to server, ReFS clones instead of copying, and timestamps, attributes and alternate streams are kept. Files of 256 MiB and more bypass the system cache so the RAM is not flushed. Up to four files are copied at once; if the source or target is a spinning disk (detected by the seek-penalty flag) one at a time, so the disk head does not thrash. On a test machine (SSD to SSD) 8000 small files went from 30 s to 4.4 s and a 3 GB file from 1.07 s to 0.9 s. `cargo run --release -p gc-core --example bench_copy -- <source> <destination folder>` times a copy; set `GC_COPY_THREADS=1` to force one file at a time. Moves on one drive are still instant renames.

## Copy, Move, Rename and the command line

F5 / F6 between folders on disk open a dialog with the target in an editable field. For one item the text is the exact target: change the folder, the file name, or both (a new name, a folder that does not exist yet, and F6 into the same folder with a new name is a rename). If the text names another existing folder (or ends with `\`), the item goes into it under its own name; for several items the text is the folder they go into (created when missing). The dialog shows the result before you press OK and refuses an impossible target (same file, folder into itself). The server dialogs (F5 / F6 with an FTP panel) have editable local / remote folders too.

F2 or Shift+F6 renames the item under the cursor: one field with the whole name, extension included, selected so typing replaces it. A right click on a file or folder opens the Explorer menu with our own **Rename (F2)** on top (the shell's own Rename needs an Explorer window); on a server panel the same item is in its small menu and renames over FTP.

Under the panels there is a command line: `folder>` and a text field. Typing any letter starts it (a lone Space still marks files), Enter runs the command in a new console window (`cmd /K`, so the output stays readable) in the folder the active panel shows, Up / Down browse the history, Esc clears it, Tab gives the keyboard back to the panel. `cd <folder>` changes the panel's folder instead. It needs a folder on disk (not an archive or a server).

## FTP / FTPS

Net > FTP connect (Ctrl+N) opens the connection window. Paste a link such as `ftp://mato:secret@lenovo-ts.com/pub` (or `ftps://` for explicit TLS) and press Enter: the active panel becomes that server. A login with special characters is written percent-encoded (`@` as `%40`, `:` as `%3A`, `/` as `%2F`). A link without a login (`ftp://lenovo-ts.com`) uses the saved session of that host when there is one, else logs in anonymously. `ghost-commander ftp://user:pass@host/dir` connects the left panel at start.

"Save as session" (or New in the list) keeps a server under a name: host, port, user, password, start folder, FTPS and "accept self-signed certificate". Sessions live in `%APPDATA%\Ghost Commander\ftp_sessions.toml`; passwords are encrypted with Windows DPAPI, so the file works only for your Windows account on that PC (where DPAPI is missing, e.g. WinPE, passwords are not saved).

A server panel behaves like a folder: Enter / Backspace browse (the root of the server has no `..` and Backspace stays there; the panel header has a **Disconnect** button, also in the Net menu, and the drive buttons / Alt+F1 / Alt+F2 leave the server too), F5 downloads into the other panel's folder, and F5 in a local panel uploads into a server shown in the other panel (F6 moves, deleting the source only after everything arrived; folders are copied recursively, name conflicts open the usual overwrite / skip / rename dialog). F7 makes a folder and F8 deletes on the server (the cursor lands on a new folder). F3 / Enter on a file downloads it to a scratch folder and views / opens it; F4 opens a local copy (it is not uploaded back). Listing and transfers run on worker threads, a dropped connection is reopened on next use, and Cancel aborts a transfer. Drive buttons and Alt+F1 / Alt+F2 leave the server.

The **Log** button in the header of a server panel (and Net > FTP connection log) shows every step of every connection: name lookup, TCP, server banner, TLS handshake, login, each folder read, transfers and all errors with the full server answer. It opens by itself when a connect fails, keeps filling while you browse, and has Copy and Clear. Passwords are never written to it. FTPS uses one TLS session for the control and the data connections (many servers, e.g. FileZilla Server and vsftpd with `require_ssl_reuse`, insist on that); for a self-signed server certificate tick "Accept self-signed / mismatching certificate" in the session.

Live round trip test against a server: `GC_FTP_TEST_URL=ftp://user:pass@127.0.0.1:2121/ cargo test -p gc-ftp --test live`.

## Remembered folders

Both panels reopen the folders they showed when the app was closed (`left_dir`, `right_dir` in `settings.toml`; a folder that no longer exists falls back to the current folder). While a panel shows an archive or a server, the folder it was opened from is remembered.

**Vanished drives (Config > Options > When a drive disappears):** by default a panel keeps showing the path of a removed drive (USB stick, share) and loads it again when it returns; the other choice (`switch_on_missing_drive = true`) jumps to the nearest existing parent folder or another drive (C: first).

The window position, size and maximized state are remembered too (`win_pos`, `win_size`, `win_maximized` in `settings.toml`).

## Running under WinPE (v0.19.6+)

The same exe runs in WinPE; only the package differs (`ghost-commander-vX-winpe-x64.zip`):

- `ghost-commander.exe` is built with a static C runtime (`.cargo/config.toml`), so no VC++ redistributable is needed. `dxgi.dll` and `tbs.dll` (GPU and TPM rows of System Info) are delay-loaded and skipped when WinPE lacks them.
- WinPE has no GPU driver and no OpenGL, so the zip carries Mesa's software OpenGL: `opengl32.dll` + `libgallium_wgl.dll` (Mesa 26.2.5 MSVC release from `pal1000/mesa-dist-win`, not stored in the repo). When `libgallium_wgl.dll` is beside the exe, the app sets `GALLIUM_DRIVER=llvmpipe` and `LP_NUM_THREADS=4` (unless already set); the default of one thread per core hung an 8-core AMD laptop. Normal installs have no such DLL and use the real driver.
- The theme is pinned to egui's dark/light mode, otherwise WinPE's system theme overrode the chosen one on the first frame.
- `gc.log` next to the exe records startup steps, the OpenGL renderer, panics, crashes and UI stalls longer than 5 s.
- Tested on Lenovo L13 G3 AMD and L16 G2 Intel, WinPE 10.0.26100 booted from SCCM.

## License

Dual-licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.

Third-party notes: RAR reading uses the `unrar` crate, which builds Rome's UnRAR source under its own license (it may not be used to re-create the RAR compression algorithm). The WinPE package (not stored in this repo) adds Mesa llvmpipe software OpenGL (MIT).
