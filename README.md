<img src="assets/icon_256.png" width="128" alt="Cyclops icon">

# Cyclops

**A blazing-fast, GPU-accelerated image browser written in Rust — built to displace FastStone.**

Cyclops aims to be the image browser you never have to wait for: instant folder opens, zero-stutter
navigation, and a modern glass-like interface that stays at your display's full refresh rate.

## Goals

- **Extremely fast** — images appear instantly; navigation never blocks on I/O or decoding.
- **GPU-accelerated** — rendering, scaling, color conversion and transitions run on the GPU (wgpu: Vulkan / DX12 / Metal).
- **Buttery framerate** — UI and image panning/zooming locked to monitor refresh, including high-refresh displays.
- **Smart caching & reading** — predictive prefetch of neighbours, multi-threaded decoding, persistent thumbnail cache, memory-mapped reads.
- **Lightweight** — small single binary, fast cold start, modest memory footprint.
- **Modern glass UI** — translucent, blurred surfaces (Mica / Acrylic on Windows), smooth animations, ultra-polished UX.

## Planned features

- Folder browser with virtualized thumbnail grid
- Full-screen viewer with smooth zoom, pan and transitions
- Wide format support: JPEG, PNG, WebP, AVIF, HEIC, TIFF, GIF/APNG, BMP, RAW (via libraw), PSD previews
- EXIF / metadata panel, histogram
- Lossless JPEG rotation, batch rename/convert/resize
- Compare mode (side-by-side, synced zoom)
- Keyboard-first workflow with FastStone-compatible shortcuts

## Architecture (initial sketch)

| Layer        | Approach                                                             |
|--------------|----------------------------------------------------------------------|
| Windowing    | `winit`                                                              |
| Rendering    | `wgpu` — textures uploaded once, scaled/filtered in shaders          |
| UI           | Custom GPU-drawn widgets with glass/blur effects                     |
| Decoding     | `zune-jpeg`, `image`, `libavif`/`dav1d`, `libheif`, `libraw` — on a thread pool |
| Caching      | LRU GPU texture cache + RAM decode cache + on-disk thumbnail DB      |
| I/O          | Memory-mapped files, directory watching, predictive prefetch         |

## Status

Early prototype (egui on wgpu): dark UI, folder tree, virtualized thumbnail grid, background
multi-threaded decoding (zune-jpeg fast path), neighbour prefetch and a zoom/pan viewer.

### Keys

| Where   | Key                                   | Action                    |
|---------|---------------------------------------|---------------------------|
| Browser | Arrows / PgUp / PgDn / Home / End     | Move selection            |
| Browser | Enter / double-click                  | Open image or folder      |
| Browser | Backspace · Alt+← · F5                | Up · Back · Refresh       |
| Browser | Type letters/digits                   | Jump to the first file starting with them |
| Browser | Ctrl+E · Ctrl+Shift+N                 | Edit in external editor · New folder |
| Both    | F2 · Del · right-click                | Rename · Delete (Recycle Bin) · menu with Show in Explorer |
| Viewer  | → ↓ Space PgDn / ← ↑ Backspace PgUp   | Next / previous image     |
| Viewer  | Mouse wheel · pinch · + / -           | Smooth zoom around cursor |
| Viewer  | Left-drag · double-click              | Pan · toggle fit / 100%   |
| Viewer  | 0 · 1 · Esc                           | Fit · 100% · Back         |
| Anywhere| F11 · Alt+Enter (F in the viewer)     | Toggle fullscreen         |
| Viewer  | E                                     | Edit in external editor   |
| Video   | Space · M · wheel over volume         | Play/pause · Mute · Volume |

Pass a folder or image path on the command line to open it directly. Network: browse via the
Network node, or type `\\server` or `\\server\share` in the path bar; visited servers are remembered.

**Images:** JPEG, PNG/APNG, GIF, WebP, BMP, TIFF, ICO, TGA, DDS, HDR, EXR, QOI, PNM, SVG and PSD (composite)
are decoded in-process, as are GPU textures (DDS, KTX, KTX2: BC1–7, ETC1/2, EAC, ASTC, uncompressed
and half/float, Zstandard-supercompressed KTX2, Basis Universal ETC1S/UASTC/UASTC HDR via the vendored open-source transcoder;
step through mips, array layers and cube faces with Ctrl+arrows); HEIC/HEIF, AVIF, JPEG XL, JPEG XR and camera RAW (DNG, CR2/CR3, NEF, ARW, RAF,
ORF, RW2, …) go through the Windows Imaging Component, using the OS's (free) codec extensions.
Animated GIF, WebP and APNG play in the viewer and preview. HDR/EXR are tone-mapped (auto exposure +
ACES) with a live exposure slider, and can be viewed as a mirror sphere or a 360° panorama.

**Hardware JPEG decoding:** large baseline 4:2:0 photos opened in the viewer decode on the graphics
card's JPEG engine (the Media Foundation MJPEG decoders AMD, NVIDIA and Intel drivers install), about
2.5× faster than the CPU for a 24 MP photo, with output matching the CPU decoder to within rounding.
Other JPEGs and all thumbnails use the multi-threaded CPU path. Switch it off in Settings → Viewer.
Benchmark: `CYCLOPS_BENCH_DIR=<folder> cargo test --release bench_viewer_jpeg -- --ignored --nocapture`.

**Video** (mp4, mov, mkv, wmv, avi, webm, …) plays through Windows Media Foundation: the OS codecs
decode on the GPU, so Cyclops bundles no codecs and carries no codec licensing obligations.

## Building

```sh
cargo build --release
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
