# Cyclops — notes for coding agents

Cyclops Image Browser: a fast Rust image browser/viewer for Windows.
GPU-accelerated (eframe/egui 0.37 + wgpu 30 on DX12), glass UI, professional-grade format
support (camera RAW, HDR, GPU textures, video). Version and licence live in `Cargo.toml`.

## Project rules

- A **tool for professionals**: open anything there is memory for, never silently downscale or
  substitute previews for real data, show true bit depths.
- Follow **Windows conventions** where they exist (shortcuts, Explorer-like menus, per-user
  installs, Recycle Bin).
- Only bundle code whose licence allows selling Cyclops closed-source (MIT/BSD/Apache/zlib,
  CDDL for LibRaw). No GPL. Record every vendored component in `third_party/README.md` and the
  About page credits.
- The GitHub repository is private: don't link it from the app.
- **Every installer rebuild bumps the version** in `Cargo.toml` (patch for fixes, minor for features),
  then commit it and tag `v<version>` so each installer maps to a commit in git.

## Building

- cargo isn't on PATH in agent shells: use `& "$env:USERPROFILE\.cargo\bin\cargo.exe" build --release`.
- A running Cyclops locks the exe: `Get-Process cyclops | Stop-Process -Force; ... | Wait-Process` first.
- A full rebuild takes ~2 min (LibRaw, the GPR SDK and Basis are compiled from C/C++ by `build.rs`).
- Installer: `powershell -File installer\build.ps1` → `dist\Cyclops-<version>-setup.exe`
  (Inno Setup 6, per-user install, VC++ runtime incl. OpenMP shipped app-local).
- Tests: `cargo test --release`. Opt-in benchmarks/surveys (ignored by default):
  - `CYCLOPS_BENCH_DIR=<folder of JPEGs> cargo test --release bench_viewer_jpeg -- --ignored --nocapture`
  - `CYCLOPS_RAW_DIR=<folder> [CYCLOPS_RAW_LIBRAW=1 | CYCLOPS_RAW_DESCRIBE=1 | CYCLOPS_RAW_COMPARE=<out dir>] cargo test --release raw_survey -- --ignored --nocapture`

## Map

| Module | Role |
|---|---|
| `main.rs` | Entry; forces `WGPU_BACKEND=dx12` + `WGPU_DX12_PRESENTATION_SYSTEM=Visual` (the only swapchain with alpha, needed for Mica/Acrylic); wgpu limits (max texture size); icon handling |
| `app.rs` | All UI: browser, folder tree, grid, viewer, preview pane, menus, dialogs, Settings/About, startup marks |
| `loader.rs` | Decode worker pool; full-size jobs before thumbnails; RAWs send a provisional embedded preview first |
| `decode.rs` | Format routing and decoding; high-precision (HDR/RAW) path; downscaling; opt-in survey tests |
| `libraw.rs`, `gpr.rs` | Camera RAW via vendored LibRaw; GoPro GPR → DNG via the vendored GPR SDK |
| `wic.rs` | Windows Imaging Component (HEIC/AVIF/JXL/JXR; RAW fallback forces Microsoft's Raw Image Decoder) |
| `hwjpeg.rs` | GPU JPEG decoding through Media Foundation MJPEG MFTs (viewer images, baseline 4:2:0 only) |
| `gputex.rs`, `basis.rs` | DDS/KTX/KTX2 containers and block formats; Basis Universal transcoding |
| `hdr_gpu.rs`, `glass.rs`, `about_fx.rs` | wgpu paint callbacks: HDR tone mapping/sphere/panorama, frosted glass, About banner shader |
| `video.rs` | Video through the Media Foundation media engine (no bundled codecs) |
| `chrome.rs`, `theme.rs`, `icons.rs` | DWM title bar/Mica/icon, skins, vector icons |
| `shell.rs`, `fs.rs`, `ops.rs`, `editor.rs`, `clipboard.rs` | Explorer integration, listing/sorting, Recycle Bin/rename, external editor, clipboard |
| `perflog.rs`, `diag.rs` | Developer log (`%LOCALAPPDATA%\Cyclops\perf.log`) incl. startup milestones |

`third_party/`: LibRaw, GoPro GPR SDK (with Adobe DNG SDK/XMP, expat), Basis Universal, zstd —
all **unmodified**; integration goes through our own small `*_shim.cpp` files and build flags
(e.g. `gpr_compat.h` is force-included rather than patching expat).

## Developer tools and switches

- `CYCLOPS_DEV=1`: shows the Developer settings page and writes the developer log in release builds
  (debug builds always have it). The log is the place for timings; never draw them in the UI.
- `CYCLOPS_HWJPEG=off|nvidia|amd`: disable or pick the GPU JPEG decoder.
- `tools\profile-startup.ps1 -Target <path>`: startup milestones over several launches.
- `examples\startup_probe.rs`, `examples\hwjpeg_probe.rs`: wgpu setup timing; GPU-vs-CPU JPEG check.
- `tools\make_icon.py`, `tools\build_ico.py`: regenerate the app icon (ring shader on glass).

## Checking the UI

- Capture **only Cyclops's own window**: `tools\capture-window.ps1` (PrintWindow, no focus change).
  Never screenshot the desktop or other windows, and never simulate mouse/keyboard input that
  could land in the user's other applications. Delete any accidental capture immediately.
- To show a state that needs clicks (e.g. Settings → About), add a temporary env-var hook,
  capture, then remove it before committing (grep for `TEMP-HOOK`/`TMP`).
- Caption-button hover can be faked on a *separate test instance* with `WM_NCMOUSEMOVE`.

## Windows/toolchain pitfalls

- Files mix CRLF/LF: when patching with scripts, normalise line endings and assert each replacement
  matched. Bash heredocs mangle backslashes/quotes; write patch scripts to files and run `python -I`.
- MSVC: `/D` can't define function-like macros (use a force-included header); `canonicalize()`
  returns `\\?\` paths that `cl.exe` can't open; a crate referenced only from C (e.g. `libz-sys`)
  needs `use crate_name as _;` to be linked.
- egui-wgpu defaults to 8192 px textures; Cyclops requests the adapter's maximum.
- Folder names from other platforms may end in `.` (invalid on Windows); strip when extracting.
