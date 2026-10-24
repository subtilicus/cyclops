//! Benchmarks hardware JPEG decoding against zune-jpeg and checks the pixels agree.
//! `cargo run --release --example hwjpeg_probe -- <jpeg files...>`
//! `CYCLOPS_HWJPEG=amd` / `nvidia` picks the vendor.

// The example uses the session directly, not the app's pool around it.
#[allow(dead_code)]
#[path = "../src/hwjpeg.rs"]
mod hwjpeg;

use std::time::Instant;

use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;

fn cpu(bytes: &[u8]) -> image::RgbaImage {
    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA).set_max_width(65535).set_max_height(65535);
    let mut d = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    let px = d.decode().unwrap();
    let info = d.info().unwrap();
    image::RgbaImage::from_raw(info.width as u32, info.height as u32, px).unwrap()
}

fn psnr(a: &image::RgbaImage, b: &image::RgbaImage) -> (f64, u8) {
    let mut se = 0f64;
    let mut worst = 0u8;
    for (x, y) in a.as_raw().iter().zip(b.as_raw()) {
        let d = x.abs_diff(*y);
        worst = worst.max(d);
        se += (d as f64).powi(2);
    }
    let mse = se / a.as_raw().len() as f64;
    (10.0 * (255.0f64.powi(2) / mse.max(1e-9)).log10(), worst)
}

fn best<T>(n: usize, mut f: impl FnMut() -> T) -> (f64, T) {
    let mut best = f64::MAX;
    let mut out = None;
    for _ in 0..n {
        let t = Instant::now();
        let r = f();
        best = best.min(t.elapsed().as_secs_f64() * 1e3);
        out = Some(r);
    }
    (best, out.unwrap())
}

fn main() {
    for i in 0..4 {
        match hwjpeg::Session::open_index(i) {
            Ok(s) => println!("decoder {i}: {} opens", s.name),
            Err(e) if e == "decoder gone" => break,
            Err(e) => println!("decoder {i}: {e}"),
        }
    }
    let t = Instant::now();
    let mut session = match hwjpeg::Session::open() {
        Ok(s) => s,
        Err(e) => return println!("no hardware session: {e}"),
    };
    println!("session: {} (open {:.1} ms)", session.name, t.elapsed().as_secs_f64() * 1e3);
    // Twice through, so every file also follows a different size.
    let files: Vec<String> = std::env::args().skip(1).collect();
    for pass in 0..2 {
        for path in &files {
            let bytes = std::fs::read(path).unwrap();
            let name = std::path::Path::new(path).file_name().unwrap().to_string_lossy().into_owned();
            let Some(h) = hwjpeg::header(&bytes) else {
                println!("{name}: not a JPEG");
                continue;
            };
            if !h.hardware {
                if pass == 0 {
                    println!("{name}: {}x{} CPU only", h.width, h.height);
                }
                continue;
            }
            let first = Instant::now();
            let r = session.decode(&bytes, h.width, h.height);
            let first_ms = first.elapsed().as_secs_f64() * 1e3;
            let img = match r {
                Err(e) => {
                    println!("{name}: hardware failed: {e}");
                    continue;
                }
                Ok(img) => img,
            };
            if pass == 1 {
                println!("{name}: pass 2 ok ({first_ms:.1} ms after a size change)");
                continue;
            }
            let (cpu_ms, reference) = best(3, || cpu(&bytes));
            let (hw_ms, _) = best(5, || session.decode(&bytes, h.width, h.height).unwrap());
            let (db, worst) = psnr(&img, &reference);
            println!(
                "{name}: {}x{}  hw {hw_ms:.1} ms (first {first_ms:.1})  cpu {cpu_ms:.1} ms  psnr {db:.1} dB, max diff {worst}",
                h.width, h.height
            );
        }
    }
}
