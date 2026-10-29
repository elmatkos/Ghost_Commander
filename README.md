# Ghost Commander

A fast, modern, dual-pane file manager written in Rust. It follows the Norton / Total / Midnight Commander tradition and aims to keep every feature of those three while improving on them.

**Status:** Planning (design proposal, nothing built yet).

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
