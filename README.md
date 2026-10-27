<img src="assets/icon_256.png" width="128" alt="Cyclops icon">

# Cyclops

**A blazing-fast, GPU-accelerated image browser for Windows, written in Rust.**

Cyclops is the image browser you never have to wait for: folders fill in instantly, navigation
never stutters, and a modern glass interface stays at your display's full refresh rate. It opens
the formats professionals actually use — camera RAW at full bit depth, HDR, GPU textures and video —
in one fast, native app.

## Highlights

- **Fast** — background decoding on all cores, whole-folder thumbnail generation with a session
  cache, neighbour prefetch in the viewer, hardware JPEG decoding, ~0.4 s to first frame.
- **GPU-rendered** — eframe/egui on wgpu (DirectX 12): smooth zoom and pan, HDR tone mapping and
  exposure in shaders, full-resolution textures up to the GPU's limit.
- **Professional formats** — camera RAW developed at the sensor's bit depth with exposure latitude,
  HDR/EXR with a live exposure slider (also as a mirror sphere or 360° panorama), DDS/KTX/KTX2 with
  mips, layers and cube faces, 16-bit and float images, PSD composites, SVG, video.
- **Native Windows** — Mica/Acrylic glass, dark title bar, Explorer folders, network browsing,
  Recycle Bin, "Copy" / "Copy as path", external editor, per-user installer.

## Formats

**Images:** JPEG, PNG/APNG, GIF, WebP, BMP, TIFF, ICO, TGA, DDS, HDR, EXR, QOI, PNM, SVG and PSD (composite)
are decoded in-process, as are GPU textures (DDS, KTX, KTX2: BC1–7, ETC1/2, EAC, ASTC, uncompressed
and half/float, Zstandard-supercompressed KTX2, Basis Universal ETC1S/UASTC/UASTC HDR via the vendored
open-source transcoder; step through mips, array layers and cube faces with Ctrl+arrows); HEIC/HEIF,
AVIF, JPEG XL and JPEG XR go through the Windows Imaging Component, using the OS's (free) codec extensions.
Animated GIF, WebP and APNG play in the viewer and preview. HDR/EXR are tone-mapped (auto exposure +
ACES) with a live exposure slider, and can be viewed as a mirror sphere or a 360° panorama.

**Camera RAW** (DNG, CR2/CR3/CRW, NEF/NRW, ARW/SR2/SRF, RAF, ORF, RW2/RWL, PEF, SRW, 3FR/FFF, IIQ, MEF,
MOS, ERF, KDC/DCR, MRW, X3F, GoPro GPR, …) is developed by the vendored LibRaw at the sensor's full bit depth into
linear light: the exposure slider pulls highlights back up to the sensor's clipping point. The viewer
shows the camera's embedded preview instantly and swaps in the develop (~1 s for 24 MP, multi-threaded);
thumbnails use the embedded preview. Anything LibRaw can't read falls back to Windows' Raw Image
Extension. GoPro GPRs (VC-5 compressed DNGs) are converted to plain DNGs by the vendored GPR SDK first.
On a one-per-maker sample of raw.pixls.us (70 files), 62 open; all 17 GoPro samples (HERO5–11, Fusion) do.

**Hardware JPEG decoding:** large baseline 4:2:0 photos opened in the viewer decode on the graphics
card's JPEG engine (the Media Foundation MJPEG decoders AMD, NVIDIA and Intel drivers install), about
2.5× faster than the CPU for a 24 MP photo, with output matching the CPU decoder to within rounding.
Other JPEGs and all thumbnails use the multi-threaded CPU path. Switch it off in Settings → Viewer.
Benchmark: `CYCLOPS_BENCH_DIR=<folder> cargo test --release bench_viewer_jpeg -- --ignored --nocapture`.

**Video** (mp4, mov, mkv, wmv, avi, webm, …) plays through Windows Media Foundation: the OS codecs
decode on the GPU, so Cyclops bundles no codecs and carries no codec licensing obligations.

## Keys

| Where   | Key                                   | Action                    |
|---------|---------------------------------------|---------------------------|
| Browser | Arrows / PgUp / PgDn / Home / End     | Move selection            |
| Browser | Enter / double-click                  | Open image or folder      |
| Browser | Backspace · Alt+← · F5                | Up · Back · Refresh       |
| Browser | Type letters/digits                   | Jump to the first file starting with them |
| Browser | Hold or click the middle button       | Autoscroll (like Chrome)  |
| Browser | Ctrl+E · Ctrl+Shift+N                 | Edit in external editor · New folder |
| Both    | Ctrl+C · Ctrl+Shift+C                 | Copy (file and picture) · Copy as path |
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

## Architecture

| Layer      | Approach                                                                      |
|------------|-------------------------------------------------------------------------------|
| UI         | eframe/egui, custom painted widgets, frosted glass from a pre-blurred image copy |
| Rendering  | wgpu on DirectX 12 (DirectComposition for transparent Mica/Acrylic windows); shaders for HDR tone mapping, sphere/panorama views |
| Decoding   | Worker pool (viewer images first, visible thumbnails next, the rest of the folder at low priority); zune-jpeg, GPU JPEG, LibRaw, GPR SDK, `image`, WIC, Basis Universal |
| Caching    | Session thumbnail cache (1 GB), full-image cache bounded by count and GPU memory, neighbour prefetch |
| Windows    | DWM, Shell (known folders, icons, network), Media Foundation, WIC, Recycle Bin, clipboard |

## Planned

- EXIF / metadata panel and histogram
- Lossless JPEG rotation, batch rename / convert / resize
- Compare mode (side by side, synced zoom)
- Persistent on-disk thumbnail cache

## Building

```sh
cargo build --release
```

`installer\build.ps1` builds the Windows installer (`dist\Cyclops-<version>-setup.exe`, Inno Setup 6).

Debug builds have a **Developer** settings page with a log (`%LOCALAPPDATA%\Cyclops\perf.log`:
decode times, startup milestones, GPU setup, warnings). Release builds hide it; start one with
`CYCLOPS_DEV=1` to get it back.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
Bundled third-party components and their licences are listed in [third_party/README.md](third_party/README.md).
