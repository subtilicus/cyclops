//! Background decode workers. Full-size requests always take priority over thumbnails,
//! and thumbnail jobs from a folder the user has already left are dropped unprocessed.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender, select, unbounded};
use egui::ColorImage;

use crate::decode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Thumb,
    Full,
}

struct Job {
    path: PathBuf,
    kind: Kind,
    generation: u64,
    /// Thumb: max side. Full: GPU max texture side.
    max_side: u32,
    /// Full only: longest side of a pre-scaled "screen" copy used for smooth downscaled display.
    screen_side: u32,
}

const THUMB_SIDE_CAP: u32 = 512;

pub struct Loaded {
    pub path: PathBuf,
    pub kind: Kind,
    pub result: Result<Images, String>,
}

pub struct Images {
    pub main: ColorImage,
    /// Downscaled copy for when the image is shown much smaller than its native size
    /// (egui-wgpu has no mipmaps, so this keeps fit-to-window views alias-free).
    pub screen: Option<ColorImage>,
    /// Native pixel size of the source image (after EXIF orientation).
    pub source_size: [usize; 2],
    /// Animation frames and their durations in seconds (empty for stills).
    pub frames: Vec<(ColorImage, f32)>,
    /// Linear-light data of HDR/EXR images (full-size requests only).
    pub hdr: Option<decode::Hdr>,
    /// Tiny blurred copy for frosted-glass overlays (full-size requests only).
    pub blur: Option<ColorImage>,
    /// Bits per channel of the source, when known.
    pub bit_depth: Option<decode::BitDepth>,
    /// Storage format worth showing (e.g. "BC7").
    pub format: Option<String>,
    /// GPU textures: [levels, layers, faces] and the other sub-images.
    pub sub_layout: Option<[u32; 3]>,
    pub subimages: Vec<SubImage>,
    pub decode_ms: f32,
}

/// One more mip level / array layer / cube face of a GPU texture.
pub struct SubImage {
    pub coords: [u32; 3],
    pub main: ColorImage,
    pub hdr: Option<decode::Hdr>,
    pub size: [usize; 2],
}

pub struct Loader {
    full_tx: Sender<Job>,
    thumb_tx: Sender<Job>,
    done_rx: Receiver<Loaded>,
    generation: Arc<AtomicU64>,
}

impl Loader {
    pub fn new(ctx: egui::Context) -> Self {
        let (full_tx, full_rx) = unbounded::<Job>();
        let (thumb_tx, thumb_rx) = unbounded::<Job>();
        let (done_tx, done_rx) = unbounded();
        let generation = Arc::new(AtomicU64::new(0));
        let workers = std::thread::available_parallelism().map_or(4, |n| n.get()).saturating_sub(1).max(2);
        for i in 0..workers {
            let (full_rx, thumb_rx, done_tx) = (full_rx.clone(), thumb_rx.clone(), done_tx.clone());
            let generation = generation.clone();
            let ctx = ctx.clone();
            std::thread::Builder::new()
                .name(format!("cyclops-decode-{i}"))
                .spawn(move || worker(full_rx, thumb_rx, done_tx, generation, ctx))
                .expect("spawn decode worker");
        }
        Self { full_tx, thumb_tx, done_rx, generation }
    }

    /// Invalidate all queued thumbnail jobs (call when switching folders).
    pub fn new_generation(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    pub fn request_thumb(&self, path: PathBuf, max_side: u32) {
        let generation = self.generation.load(Ordering::Relaxed);
        let _ = self.thumb_tx.send(Job { path, kind: Kind::Thumb, generation, max_side, screen_side: 0 });
    }

    pub fn request_full(&self, path: PathBuf, max_texture_side: u32, screen_side: u32) {
        let _ = self.full_tx.send(Job {
            path,
            kind: Kind::Full,
            generation: 0,
            max_side: max_texture_side,
            screen_side,
        });
    }

    pub fn poll(&self) -> impl Iterator<Item = Loaded> + '_ {
        self.done_rx.try_iter()
    }
}

fn worker(
    full_rx: Receiver<Job>,
    thumb_rx: Receiver<Job>,
    done_tx: Sender<Loaded>,
    generation: Arc<AtomicU64>,
    ctx: egui::Context,
) {
    loop {
        let job = match full_rx.try_recv() {
            Ok(job) => job,
            Err(_) => select! {
                recv(full_rx) -> job => match job { Ok(j) => j, Err(_) => return },
                recv(thumb_rx) -> job => match job { Ok(j) => j, Err(_) => return },
            },
        };
        if job.kind == Kind::Thumb && job.generation != generation.load(Ordering::Relaxed) {
            continue;
        }
        let start = Instant::now();
        if decode::is_video(&job.path) {
            // Videos get Explorer's thumbnail (a representative frame) for tiles and previews.
            let side = job.max_side.min(if job.kind == Kind::Full { 1280 } else { THUMB_SIDE_CAP });
            let result = crate::shell::thumbnail(&job.path, side)
                .map(|main| Images {
                    source_size: main.size,
                    main,
                    screen: None,
                    frames: Vec::new(),
                    hdr: None,
                    blur: None,
                    bit_depth: None,
                    format: None,
                    sub_layout: None,
                    subimages: Vec::new(),
                    decode_ms: start.elapsed().as_secs_f32() * 1000.0,
                })
                .ok_or_else(|| "no thumbnail".to_owned());
            if done_tx.send(Loaded { path: job.path, kind: job.kind, result }).is_err() {
                return;
            }
            ctx.request_repaint();
            continue;
        }
        // RAW/HEIC thumbnails: Explorer's cached thumbnail is far faster than a full decode.
        if job.kind == Kind::Thumb && decode::prefers_shell_thumbnail(&job.path) {
            if let Some(main) = crate::shell::thumbnail_only(&job.path, job.max_side) {
                let images = Images { source_size: main.size, main, screen: None, frames: Vec::new(), hdr: None, blur: None, bit_depth: None, format: None, sub_layout: None, subimages: Vec::new(), decode_ms: 0.0 };
                if done_tx.send(Loaded { path: job.path, kind: job.kind, result: Ok(images) }).is_err() {
                    return;
                }
                ctx.request_repaint();
                continue;
            }
        }
        let full = job.kind == Kind::Full;
        let vector_side = if full { job.screen_side.max(1024) } else { job.max_side };
        let mut decode_ms = 0.0;
        let result = decode::decode(&job.path, job.max_side, vector_side, full).map(|d| {
            decode_ms = start.elapsed().as_secs_f32() * 1000.0;
            let source_size = [d.source_size[0] as usize, d.source_size[1] as usize];
            let frames: Vec<(ColorImage, f32)> = d
                .frames
                .into_iter()
                .map(|(img, secs)| (to_color_image(decode::fit_within(img, job.max_side), false), secs))
                .collect();
            let main_img = decode::fit_within(d.image, job.max_side);
            let screen = (job.kind == Kind::Full && frames.is_empty())
                .then(|| {
                    let longest = main_img.width().max(main_img.height());
                    // Only worth it when the image is substantially larger than the screen.
                    (longest as f32 > job.screen_side as f32 * 1.5)
                        .then(|| to_color_image(decode::downscaled(&main_img, job.screen_side), d.opaque))
                })
                .flatten();
            let main = to_color_image(main_img, d.opaque);
            let blur = full.then(|| crate::glass::blurred(&main.pixels, main.size));
            let hdr = if full { d.hdr } else { None };
            let bit_depth = d.bit_depth;
            let format = d.format;
            let sub_layout = d.sub_layout;
            let subimages = d
                .subimages
                .into_iter()
                .map(|(coords, sub)| SubImage {
                    coords,
                    size: [sub.image.width() as usize, sub.image.height() as usize],
                    main: to_color_image(decode::fit_within(sub.image, job.max_side), sub.opaque),
                    hdr: sub.hdr,
                })
                .collect();
            Images { main, screen, source_size, frames, hdr, blur, bit_depth, format, sub_layout, subimages, decode_ms: start.elapsed().as_secs_f32() * 1000.0 }
        });
        crate::perflog::record(|| match &result {
            Ok(img) => format!(
                "{:?}  {:>8.1} ms  (decode {:>6.1})  {}x{}  {}",
                job.kind, img.decode_ms, decode_ms, img.source_size[0], img.source_size[1], job.path.display()
            ),
            Err(e) => format!("{:?}  failed: {e}  {}", job.kind, job.path.display()),
        });
        if done_tx.send(Loaded { path: job.path, kind: job.kind, result }).is_err() {
            return;
        }
        ctx.request_repaint();
    }
}

fn to_color_image(img: image::RgbaImage, opaque: bool) -> ColorImage {
    let size = [img.width() as usize, img.height() as usize];
    if opaque {
        // Opaque pixels are already premultiplied: reuse the buffer instead of copying it.
        match bytemuck::allocation::try_cast_vec::<u8, egui::Color32>(img.into_raw()) {
            Ok(pixels) => ColorImage::new(size, pixels),
            Err((_, raw)) => ColorImage::from_rgba_premultiplied(size, &raw),
        }
    } else {
        ColorImage::from_rgba_unmultiplied(size, img.as_raw())
    }
}
