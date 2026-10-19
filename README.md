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

## Building

```sh
cargo build --release
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
