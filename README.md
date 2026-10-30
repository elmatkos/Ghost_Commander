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
