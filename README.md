# Ghost Commander

A fast, modern, dual-pane file manager written in Rust. It follows the Norton / Total / Midnight Commander tradition and aims to keep every feature of those three while improving on them.

**Status:** Planning (design proposal, nothing built yet).

**Repo:** https://github.com/elmatkos/Ghost_Commander (private for now, to be made public later).

## Goals

- Speed first: instant directory listing, no UI stalls, all I/O off the UI thread.
- Native archive support (7z, zip, tar, rar read, ...) treated as folders, no external 7-Zip needed.
- Built-in image viewer: opens the full image at native size if it fits the monitor, otherwise scales proportionally to 75% of the monitor *height* (wide-monitor friendly).
- Modern look (dark/light themes, crisp fonts, icons) without losing keyboard-first workflow.

## Proposed stack (to be approved)

| Area | Choice | Why |
|---|---|---|
| Language | Rust (stable) | Speed, safety |
| UI | `egui`/`eframe` (wgpu) | Pure Rust, fast to iterate, GPU rendered; alternative: Slint |
| Archives | `sevenz-rust2`, `zip`, `tar`, `flate2`, `xz2`/`liblzma`, `unrar` (read) | Native, no 7z.exe |
| Images | `image` + `webp`/`avif`/`zune-*`, `fast_image_resize` | Fast decode + SIMD resize |
| Hashes | `crc32fast`, `blake3`, `sha2`, `md-5` | CRC32 is hardware accelerated |
| Windows I/O | `windows-sys` (FindFirstFileEx, USN/MFT later) | Fastest listing, shell integration |

## Planned structure

```
Ghost_Commander/
  Cargo.toml            (workspace)
  crates/
    gc-core/            VFS trait, file ops, queue, hashing, search
    gc-archive/         archive providers (7z, zip, tar, ...) as VFS
    gc-image/           decode + fit logic for the viewer
    gc-ui/              egui app, panels, viewer, themes
    gc-app/             binary
```

## Roadmap

1. **M0** Skeleton: workspace, window, one panel listing a directory.
2. **M1** Dual panes, keyboard navigation, F5/F6/F7/F8 copy/move/mkdir/delete, background job queue with progress.
3. **M2** Virtual file system layer + archives browsed as folders, extract/pack (7z first).
4. **M3** Image viewer (fit rules above), text/hex viewer (F3), editor hand-off (F4).
5. **M4** CRC/hash verify (.sfv/.md5/.sha256), file compare, sync dirs, multi-rename, search (incl. in archives).
6. **M5** Tabs, bookmarks, themes, plugins/commands config, FTP/SFTP VFS.

## How to run

TBD (no code yet). Will be `cargo run --release`.

## Current state

M0 and M1 are built: dual panes, keyboard navigation (arrows, PgUp/PgDn, Home/End, Tab, Enter, Backspace, Space/Insert to mark), F5 copy, F6 move, F7 new folder, F8 delete to the recycle bin (Shift+F8 permanent), Ctrl+1/Ctrl+2 drive picker, date column. All file operations are cancellable background jobs with progress. Name conflicts open a dialog (overwrite / skip / rename / cancel, optionally for all remaining).

Run: `cargo run --release` (binary `ghost-commander`). Test: `cargo test`.

Branding lives in `assets/` (`icon.ico` is embedded into the exe by `crates/gc-app/build.rs`, `logo_512.png` is shown in the About window, F1).

## TODO (backlog)

**M1 gaps**
- Sortable columns (name, size, date), quick filter.

**Performance: caching and acceleration everywhere possible**
- Cache directory listings (keyed by path + change notification via `ReadDirectoryChangesW`), incremental refresh instead of full re-read.
- Cache icons, thumbnails and decoded images (LRU, optional on-disk thumbnail cache).
- Cache archive indexes (central directories) so reopening an archive is instant.
- Use modern CPU features wherever it pays off, with runtime detection and scalar fallback: AVX2/AVX-512 for search and compare, SHA-NI and PCLMULQDQ/CRC32 instructions for hashing, AES-NI, SIMD image resize/decode, BMI2. Applies to core and to every later feature (archives, hashing, compare, search).
- Parallelism: rayon for hashing, search, directory sizing; large-file copy with unbuffered/overlapped I/O; NTFS MFT/USN fast search.
- Measure first: benchmark suite (`criterion`) before and after each optimization.

**System info (like EFI Commander)**
- Built-in system information view: CPU (brand, features, caches), memory, SMBIOS tables, disks, firmware/Secure Boot state, displays.

**Release**
- Sign the binary and installer once a code-signing certificate is obtained (signtool + timestamping, CI step).

## Themes and file icons

F9 opens the theme editor (live preview). Built-in themes: Dark, Light, Ghost, Norton Classic. Custom themes are TOML files in `%APPDATA%\Ghost Commander\themes\`; the editor saves them there, and the selected theme is remembered in `settings.toml`.

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

Enter on any other file opens it with its Windows default program (pictures in the photo app, `.exe` runs, ...); Enter on a `.zip`, `.7z`, `.rar` (read only), `.tar`, `.tar.gz` (`.tgz`) or `.tar.xz` (`.txz`) opens it as a folder (browse, `..` leaves it); F5 extracts the marked items (or the one under the cursor) into the other panel's folder, with the same progress, cancel and conflict dialog as file copies. Names that could escape the destination (`..`, drive letters) are never listed or extracted. Archives inside archives open too: the inner file is extracted to a scratch folder in the background (header shows a spinner) and removed on exit. Archives open off the UI thread, and zip extraction runs on all cores. Alt+F5 packs the marked items (or the one under the cursor), with an editable target folder and name, into a new `.7z`, `.zip`, `.tar.gz` or `.tar.xz` (xz compresses on all cores) in the other panel's folder (never overwrites; adding to an existing archive is not supported yet). Browsing archives stays read-only; Tarballs are streams, so listing a big `.tar.gz` or `.tar.xz` reads it completely once.

## Context menu and Delete

Right click on files or folders on disk shows the real Windows Explorer menu (Open with, Send to, 7-Zip, Properties, ...); a right click on an unmarked row acts on that row only, on a marked row on all marked. Inside archives a small built-in menu offers Open and Extract. `Del` / `Shift+Del` work like F8 / Shift+F8. F3 opens a read-only text viewer (first 8 MB; also for files inside archives, which are extracted to a scratch folder first, as for F4 and Enter), or the image viewer for png, jpg, gif, bmp, webp and ico (own window: native size if it fits the monitor, otherwise scaled to 75% of the monitor height; Left/Right browse the folder's images, `+` / `-` zoom, `0` resets, drag pans; Esc, Q or F3 closes it), F4 opens the file in Notepad.

## Menu, drive buttons and status line

The menu bar has Config (Options..., Themes...) and Help (About). Config > Options switches the drive buttons on or off (and the free-space bar under each drive); settings live in `%APPDATA%\Ghost Commander\settings.toml`. With drive buttons on, every panel gets a row of pills (type icon, letter, used-space bar, details on hover) plus `\` (root of the drive) and `..` (parent folder). The drive list is refreshed by a background thread, so a disconnected network drive cannot freeze the window. Under each list a status line shows `marked / all in files, dirs` as in Total Commander (folder sizes are not calculated).

## System Info

Info > System Info shows a scrollable summary in the style of EFI Commander's F9 view (crate `gc-sysinfo`): system and board (SMBIOS), BIOS and EC version, boot mode, Secure Boot, TPM, BIOS passwords, CPU with speed, cores, threads and the SIMD/crypto features it offers (SSE4.2, AVX2, AVX-512, SHA, AES, ...), memory with every installed module, GPUs and displays, battery, drives and the Windows build. Rows a machine does not expose are left out. The data is gathered on a worker thread; Copy puts it all on the clipboard as text. `cargo run -p gc-sysinfo --example dump` prints the same without the GUI.

## Sorting and quick filter

Click a column heading (Name, Size, Date) to sort, click again to reverse; Ctrl+F3 sorts by name, Ctrl+F4 by extension, Ctrl+F5 by date, Ctrl+F6 by size. Folders always stay on top. Ctrl+F opens the quick filter under the list: typing narrows the panel to names containing the text (case-insensitive), Enter keeps the filtered list and gives the keyboard back, Esc clears it; entering another folder clears it too. RAR archives are read through the UnRAR library (extraction only; no RAR creation exists, by licence).

Ctrl+H (or Tools > Checksums) calculates CRC-32, MD5, SHA-1 and SHA-256 of the marked files, or of the file under the cursor, in one read pass; several files are hashed in parallel. Paste a checksum into "Compare with" and the matching value lights up. Folders are skipped and files inside archives are not supported yet.
