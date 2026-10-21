//! GPU texture containers: DDS, KTX (1) and KTX2.
//!
//! Shows mip level 0 of the first layer/face. Block-compressed data (BC1–7, ETC1/2, EAC, ASTC)
//! is decoded with `texture2ddecoder`; common uncompressed layouts are converted directly.
//! Half/float textures come back as floats so they get HDR tone mapping and exposure.

use image::{Rgba32FImage, RgbaImage};

/// A decoded texture: 8-bit pixels or floating point, plus a label for the info bar.
pub struct Texture {
    pub pixels: Pixels,
    pub format: String,
}

pub enum Pixels {
    Ldr(RgbaImage),
    Float(Rgba32FImage),
}

/// ASTC block footprints in the order both Vulkan and OpenGL enumerate them.
const ASTC_SIZES: [(u8, u8); 14] =
    [(4, 4), (5, 4), (5, 5), (6, 5), (6, 6), (8, 5), (8, 6), (8, 8), (10, 5), (10, 6), (10, 8), (10, 10), (12, 10), (12, 12)];

#[derive(Clone, Copy, Debug)]
enum Fmt {
    Bc1,
    Bc1a,
    Bc2,
    Bc3,
    Bc4,
    Bc5,
    Bc6h { signed: bool },
    Bc7,
    Etc1,
    Etc2Rgb,
    Etc2Rgba1,
    Etc2Rgba8,
    EacR,
    EacRg,
    Astc(u8, u8),
    Rgba8,
    Bgra8,
    Bgrx8,
    Rgb8,
    Bgr8,
    R8,
    Rg8,
    La8,
    Rgba16f,
    Rgba32f,
}

impl Fmt {
    fn label(self) -> String {
        match self {
            Fmt::Bc1 | Fmt::Bc1a => "BC1".into(),
            Fmt::Bc2 => "BC2".into(),
            Fmt::Bc3 => "BC3".into(),
            Fmt::Bc4 => "BC4".into(),
            Fmt::Bc5 => "BC5".into(),
            Fmt::Bc6h { .. } => "BC6H".into(),
            Fmt::Bc7 => "BC7".into(),
            Fmt::Etc1 => "ETC1".into(),
            Fmt::Etc2Rgb | Fmt::Etc2Rgba1 | Fmt::Etc2Rgba8 => "ETC2".into(),
            Fmt::EacR => "EAC R11".into(),
            Fmt::EacRg => "EAC RG11".into(),
            Fmt::Astc(w, h) => format!("ASTC {w}×{h}"),
            Fmt::Rgba8 | Fmt::Bgra8 | Fmt::Bgrx8 => "RGBA8".into(),
            Fmt::Rgb8 | Fmt::Bgr8 => "RGB8".into(),
            Fmt::R8 => "R8".into(),
            Fmt::Rg8 => "RG8".into(),
            Fmt::La8 => "LA8".into(),
            Fmt::Rgba16f => "RGBA16F".into(),
            Fmt::Rgba32f => "RGBA32F".into(),
        }
    }
}

fn decode(fmt: Fmt, data: &[u8], w: u32, h: u32) -> Result<Texture, String> {
    use texture2ddecoder as t;
    let (wu, hu) = (w as usize, h as usize);
    let block = |f: fn(&[u8], usize, usize, &mut [u32]) -> Result<(), &'static str>| -> Result<RgbaImage, String> {
        let mut out = vec![0u32; wu * hu];
        f(data, wu, hu, &mut out).map_err(str::to_owned)?;
        // texture2ddecoder packs pixels as little-endian BGRA.
        let rgba = out.iter().flat_map(|p| {
            let [b, g, r, a] = p.to_le_bytes();
            [r, g, b, a]
        });
        RgbaImage::from_raw(w, h, rgba.collect()).ok_or_else(|| "texture size mismatch".to_owned())
    };
    let raw = |bpp: usize, map: &dyn Fn(&[u8]) -> [u8; 4]| -> Result<RgbaImage, String> {
        let need = wu * hu * bpp;
        let src = data.get(..need).ok_or("texture data is truncated")?;
        let rgba: Vec<u8> = src.chunks_exact(bpp).flat_map(map).collect();
        RgbaImage::from_raw(w, h, rgba).ok_or_else(|| "texture size mismatch".to_owned())
    };
    let ldr = match fmt {
        Fmt::Bc1 => block(t::decode_bc1)?,
        Fmt::Bc1a => block(t::decode_bc1a)?,
        Fmt::Bc2 => block(t::decode_bc2)?,
        Fmt::Bc3 => block(t::decode_bc3)?,
        Fmt::Bc4 => block(t::decode_bc4)?,
        Fmt::Bc5 => block(t::decode_bc5)?,
        // BC6H is HDR: decode to real floats so it gets tone mapping and exposure.
        Fmt::Bc6h { signed } => return Ok(Texture { pixels: Pixels::Float(decode_bc6h(data, w, h, signed)?), format: fmt.label() }),
        Fmt::Bc7 => block(t::decode_bc7)?,
        Fmt::Etc1 => block(t::decode_etc1)?,
        Fmt::Etc2Rgb => block(t::decode_etc2_rgb)?,
        Fmt::Etc2Rgba1 => block(t::decode_etc2_rgba1)?,
        Fmt::Etc2Rgba8 => block(t::decode_etc2_rgba8)?,
        Fmt::EacR => block(t::decode_eacr)?,
        Fmt::EacRg => block(t::decode_eacrg)?,
        Fmt::Astc(bw, bh) => {
            let mut out = vec![0u32; wu * hu];
            t::decode_astc(data, wu, hu, bw as usize, bh as usize, &mut out).map_err(str::to_owned)?;
            let rgba = out.iter().flat_map(|p| {
                let [b, g, r, a] = p.to_le_bytes();
                [r, g, b, a]
            });
            RgbaImage::from_raw(w, h, rgba.collect()).ok_or("texture size mismatch")?
        }
        Fmt::Rgba8 => raw(4, &|p| [p[0], p[1], p[2], p[3]])?,
        Fmt::Bgra8 => raw(4, &|p| [p[2], p[1], p[0], p[3]])?,
        Fmt::Bgrx8 => raw(4, &|p| [p[2], p[1], p[0], 255])?,
        Fmt::Rgb8 => raw(3, &|p| [p[0], p[1], p[2], 255])?,
        Fmt::Bgr8 => raw(3, &|p| [p[2], p[1], p[0], 255])?,
        Fmt::R8 => raw(1, &|p| [p[0], p[0], p[0], 255])?,
        Fmt::Rg8 => raw(2, &|p| [p[0], p[1], 0, 255])?,
        Fmt::La8 => raw(2, &|p| [p[0], p[0], p[0], p[1]])?,
        Fmt::Rgba16f | Fmt::Rgba32f => {
            let half = matches!(fmt, Fmt::Rgba16f);
            let bpp = if half { 8 } else { 16 };
            let src = data.get(..wu * hu * bpp).ok_or("texture data is truncated")?;
            let floats: Vec<f32> = if half {
                src.chunks_exact(2).map(|c| half::f16::from_le_bytes([c[0], c[1]]).to_f32()).collect()
            } else {
                src.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
            };
            let img = Rgba32FImage::from_raw(w, h, floats).ok_or("texture size mismatch")?;
            return Ok(Texture { pixels: Pixels::Float(img), format: fmt.label() });
        }
    };
    Ok(Texture { pixels: Pixels::Ldr(ldr), format: fmt.label() })
}

/// BC6H blocks to linear RGB floats (alpha 1), via bcdec's float decoder.
fn decode_bc6h(data: &[u8], w: u32, h: u32, signed: bool) -> Result<Rgba32FImage, String> {
    let (wu, hu) = (w as usize, h as usize);
    let (bw, bh) = (wu.div_ceil(4), hu.div_ceil(4));
    if data.len() < bw * bh * 16 {
        return Err("texture data is truncated".into());
    }
    let mut out = vec![0f32; wu * hu * 4];
    let mut block = [0f32; 4 * 4 * 3];
    for by in 0..bh {
        for bx in 0..bw {
            let i = by * bw + bx;
            bcdec_rs::bc6h_float(&data[i * 16..i * 16 + 16], &mut block, 4 * 3, signed);
            // Copy the block's pixels that fall inside the image (edges may be partial).
            for y in 0..4 {
                for x in 0..4 {
                    let (px, py) = (bx * 4 + x, by * 4 + y);
                    if px < wu && py < hu {
                        let src = (y * 4 + x) * 3;
                        let dst = (py * wu + px) * 4;
                        out[dst..dst + 3].copy_from_slice(&block[src..src + 3]);
                        out[dst + 3] = 1.0;
                    }
                }
            }
        }
    }
    Rgba32FImage::from_raw(w, h, out).ok_or_else(|| "texture size mismatch".to_owned())
}

// ---------------------------------------------------------------- sub-images

/// One sub-image of a texture: a mip level of one array layer and cube face.
pub struct SubTexture {
    pub level: u32,
    pub layer: u32,
    pub face: u32,
    pub texture: Texture,
}

/// The sub-images of a texture (all of them, or just the first), plus its extent in each
/// direction so the viewer can step through levels, layers and faces.
pub struct TextureSet {
    pub subs: Vec<SubTexture>,
    pub levels: u32,
    pub layers: u32,
    pub faces: u32,
}

impl Fmt {
    /// Block footprint and bytes per block (uncompressed: 1×1 and bytes per pixel).
    fn block(self) -> (usize, usize, usize) {
        match self {
            Fmt::Bc1 | Fmt::Bc1a | Fmt::Bc4 | Fmt::Etc1 | Fmt::Etc2Rgb | Fmt::Etc2Rgba1 | Fmt::EacR => (4, 4, 8),
            Fmt::Bc2 | Fmt::Bc3 | Fmt::Bc5 | Fmt::Bc6h { .. } | Fmt::Bc7 | Fmt::Etc2Rgba8 | Fmt::EacRg => (4, 4, 16),
            Fmt::Astc(w, h) => (w as usize, h as usize, 16),
            Fmt::Rgba8 | Fmt::Bgra8 | Fmt::Bgrx8 => (1, 1, 4),
            Fmt::Rgb8 | Fmt::Bgr8 => (1, 1, 3),
            Fmt::R8 => (1, 1, 1),
            Fmt::Rg8 | Fmt::La8 => (1, 1, 2),
            Fmt::Rgba16f => (1, 1, 8),
            Fmt::Rgba32f => (1, 1, 16),
        }
    }

    fn image_bytes(self, w: u32, h: u32) -> usize {
        let (bw, bh, bytes) = self.block();
        (w as usize).div_ceil(bw) * (h as usize).div_ceil(bh) * bytes
    }
}

fn mip(size: u32, level: u32) -> u32 {
    (size >> level).max(1)
}

/// Decode a DDS / KTX / KTX2 file (`ext`); with `first_only`, just mip 0 of the first layer/face.
pub fn decode_container(bytes: &[u8], ext: &str, first_only: bool) -> Result<TextureSet, String> {
    match ext {
        "dds" => dds_set(bytes, first_only),
        "ktx2" => ktx2_set(bytes, first_only),
        _ => ktx1_set(bytes, first_only),
    }
}

// ---------------------------------------------------------------- DDS

fn dds_format(dds: &ddsfile::Dds) -> Result<Fmt, String> {
    use ddsfile::{D3DFormat, DxgiFormat as D};
    Ok(if let Some(f) = dds.get_dxgi_format() {
        match f {
            D::BC1_Typeless | D::BC1_UNorm | D::BC1_UNorm_sRGB => Fmt::Bc1a,
            D::BC2_Typeless | D::BC2_UNorm | D::BC2_UNorm_sRGB => Fmt::Bc2,
            D::BC3_Typeless | D::BC3_UNorm | D::BC3_UNorm_sRGB => Fmt::Bc3,
            D::BC4_Typeless | D::BC4_UNorm | D::BC4_SNorm => Fmt::Bc4,
            D::BC5_Typeless | D::BC5_UNorm | D::BC5_SNorm => Fmt::Bc5,
            D::BC6H_Typeless | D::BC6H_UF16 => Fmt::Bc6h { signed: false },
            D::BC6H_SF16 => Fmt::Bc6h { signed: true },
            D::BC7_Typeless | D::BC7_UNorm | D::BC7_UNorm_sRGB => Fmt::Bc7,
            D::R8G8B8A8_Typeless | D::R8G8B8A8_UNorm | D::R8G8B8A8_UNorm_sRGB => Fmt::Rgba8,
            D::B8G8R8A8_Typeless | D::B8G8R8A8_UNorm | D::B8G8R8A8_UNorm_sRGB => Fmt::Bgra8,
            D::B8G8R8X8_Typeless | D::B8G8R8X8_UNorm | D::B8G8R8X8_UNorm_sRGB => Fmt::Bgrx8,
            D::R8_Typeless | D::R8_UNorm => Fmt::R8,
            D::R8G8_Typeless | D::R8G8_UNorm => Fmt::Rg8,
            D::R16G16B16A16_Typeless | D::R16G16B16A16_Float => Fmt::Rgba16f,
            D::R32G32B32A32_Typeless | D::R32G32B32A32_Float => Fmt::Rgba32f,
            other => return Err(format!("DDS format {other:?} isn't supported yet")),
        }
    } else {
        match dds.get_d3d_format() {
            Some(D3DFormat::A8R8G8B8) => Fmt::Bgra8,
            Some(D3DFormat::X8R8G8B8) => Fmt::Bgrx8,
            Some(D3DFormat::A8B8G8R8) => Fmt::Rgba8,
            Some(D3DFormat::R8G8B8) => Fmt::Bgr8,
            Some(D3DFormat::L8) => Fmt::R8,
            Some(D3DFormat::A8L8) => Fmt::La8,
            Some(D3DFormat::A16B16G16R16F) => Fmt::Rgba16f,
            Some(D3DFormat::A32B32G32R32F) => Fmt::Rgba32f,
            Some(other) => return Err(format!("DDS format {other:?} isn't supported yet")),
            None => return Err("unknown DDS pixel format".into()),
        }
    })
}

fn dds_set(bytes: &[u8], first_only: bool) -> Result<TextureSet, String> {
    let dds = ddsfile::Dds::read(std::io::Cursor::new(bytes)).map_err(|e| format!("DDS: {e}"))?;
    let fmt = dds_format(&dds)?;
    let (w0, h0) = (dds.get_width(), dds.get_height());
    let levels = dds.get_num_mipmap_levels().max(1);
    // ddsfile counts each cube face as an array layer (layers × 6).
    let slices = dds.get_num_array_layers().max(1);
    let cube = dds.header.caps2.contains(ddsfile::Caps2::CUBEMAP)
        || dds.header10.as_ref().is_some_and(|h| h.misc_flag.contains(ddsfile::MiscFlag::TEXTURECUBE));
    let faces = if cube && slices % 6 == 0 { 6 } else { 1 };
    let mut subs = Vec::new();
    'slices: for slice in 0..slices {
        let data = dds.get_data(slice).map_err(|e| format!("DDS: {e}"))?;
        let mut offset = 0;
        for level in 0..levels {
            let (w, h) = (mip(w0, level), mip(h0, level));
            let size = fmt.image_bytes(w, h);
            let Some(chunk) = data.get(offset..offset + size) else { break };
            subs.push(SubTexture { level, layer: slice / faces, face: slice % faces, texture: decode(fmt, chunk, w, h)? });
            offset += size;
            if first_only {
                break 'slices;
            }
        }
    }
    Ok(TextureSet { subs, levels, layers: slices / faces, faces })
}

// ---------------------------------------------------------------- KTX2

fn vk_format(v: u32) -> Option<Fmt> {
    Some(match v {
        9 | 15 => Fmt::R8,
        16 | 22 => Fmt::Rg8,
        23 | 29 => Fmt::Rgb8,
        30 | 36 => Fmt::Bgr8,
        37 | 43 => Fmt::Rgba8,
        44 | 50 => Fmt::Bgra8,
        97 => Fmt::Rgba16f,
        109 => Fmt::Rgba32f,
        131 | 132 => Fmt::Bc1,
        133 | 134 => Fmt::Bc1a,
        135 | 136 => Fmt::Bc2,
        137 | 138 => Fmt::Bc3,
        139 | 140 => Fmt::Bc4,
        141 | 142 => Fmt::Bc5,
        143 => Fmt::Bc6h { signed: false },
        144 => Fmt::Bc6h { signed: true },
        145 | 146 => Fmt::Bc7,
        147 | 148 => Fmt::Etc2Rgb,
        149 | 150 => Fmt::Etc2Rgba1,
        151 | 152 => Fmt::Etc2Rgba8,
        153 | 154 => Fmt::EacR,
        155 | 156 => Fmt::EacRg,
        157..=184 => {
            let (bw, bh) = ASTC_SIZES[((v - 157) / 2) as usize];
            Fmt::Astc(bw, bh)
        }
        _ => return None,
    })
}

fn ktx2_set(bytes: &[u8], first_only: bool) -> Result<TextureSet, String> {
    use ktx2::SupercompressionScheme as S;
    let reader = ktx2::Reader::new(bytes).map_err(|e| format!("KTX2: {e:?}"))?;
    let header = reader.header();
    // Basis Universal: ETC1S (BasisLZ), UASTC (format undefined) and UASTC/ASTC HDR, which is
    // declared as an ASTC SFLOAT format (VK_FORMAT_ASTC_*_SFLOAT_BLOCK = 1000066000..13).
    let astc_hdr = header.format.is_some_and(|f| (1_000_066_000..=1_000_066_013).contains(&f.value()));
    if header.format.is_none() || header.supercompression_scheme == Some(S::BasisLZ) || astc_hdr {
        return crate::basis::decode_ktx2(bytes, first_only);
    }
    let format = header.format.expect("checked above");
    let fmt = vk_format(format.value()).ok_or_else(|| format!("KTX2 format {format:?} isn't supported yet"))?;
    let (w0, h0) = (header.pixel_width, header.pixel_height.max(1));
    let (layers, faces, depth) = (header.layer_count.max(1), header.face_count.max(1), header.pixel_depth.max(1));
    let mut subs = Vec::new();
    for (level, lvl) in reader.levels().enumerate() {
        let level = level as u32;
        let data: std::borrow::Cow<[u8]> = match header.supercompression_scheme {
            None => lvl.data.into(),
            Some(S::Zstandard) => {
                use std::io::Read;
                let mut out = Vec::with_capacity(lvl.uncompressed_byte_length as usize);
                ruzstd::decoding::StreamingDecoder::new(lvl.data)
                    .map_err(|e| format!("KTX2 zstd: {e}"))?
                    .read_to_end(&mut out)
                    .map_err(|e| format!("KTX2 zstd: {e}"))?;
                out.into()
            }
            Some(other) => return Err(format!("KTX2 supercompression {other:?} isn't supported yet")),
        };
        let (w, h) = (mip(w0, level), mip(h0, level));
        let size = fmt.image_bytes(w, h);
        // Level data is laid out layer → face → z-slice; show z = 0 of 3D textures.
        let slices = mip(depth, level) as usize;
        for layer in 0..layers {
            for face in 0..faces {
                let offset = ((layer * faces + face) as usize) * slices * size;
                let Some(chunk) = data.get(offset..offset + size) else { continue };
                subs.push(SubTexture { level, layer, face, texture: decode(fmt, chunk, w, h)? });
                if first_only {
                    return Ok(TextureSet { subs, levels: header.level_count.max(1), layers, faces });
                }
            }
        }
    }
    Ok(TextureSet { subs, levels: header.level_count.max(1), layers, faces })
}

// ---------------------------------------------------------------- KTX (version 1)

fn gl_format(internal: u32, format: u32, ty: u32) -> Option<Fmt> {
    const UNSIGNED_BYTE: u32 = 0x1401;
    const HALF_FLOAT: u32 = 0x140B;
    const FLOAT: u32 = 0x1406;
    Some(match internal {
        0x83F0 => Fmt::Bc1,
        0x83F1 => Fmt::Bc1a,
        0x83F2 => Fmt::Bc2,
        0x83F3 => Fmt::Bc3,
        0x8DBB | 0x8DBC => Fmt::Bc4,
        0x8DBD | 0x8DBE => Fmt::Bc5,
        0x8E8C | 0x8E8D => Fmt::Bc7,
        0x8E8E => Fmt::Bc6h { signed: true },
        0x8E8F => Fmt::Bc6h { signed: false },
        0x8D64 => Fmt::Etc1,
        0x9274 | 0x9275 => Fmt::Etc2Rgb,
        0x9276 | 0x9277 => Fmt::Etc2Rgba1,
        0x9278 | 0x9279 => Fmt::Etc2Rgba8,
        0x9270 | 0x9271 => Fmt::EacR,
        0x9272 | 0x9273 => Fmt::EacRg,
        0x93B0..=0x93BD | 0x93D0..=0x93DD => {
            let (bw, bh) = ASTC_SIZES[(internal & 0xF) as usize];
            Fmt::Astc(bw, bh)
        }
        // Uncompressed: decided by format + type.
        _ => match (format, ty) {
            (0x1908, UNSIGNED_BYTE) => Fmt::Rgba8,
            (0x80E1, UNSIGNED_BYTE) => Fmt::Bgra8,
            (0x1907, UNSIGNED_BYTE) => Fmt::Rgb8,
            (0x80E0, UNSIGNED_BYTE) => Fmt::Bgr8,
            (0x1903 | 0x1909, UNSIGNED_BYTE) => Fmt::R8,
            (0x8227, UNSIGNED_BYTE) => Fmt::Rg8,
            (0x190A, UNSIGNED_BYTE) => Fmt::La8,
            (0x1908, HALF_FLOAT) => Fmt::Rgba16f,
            (0x1908, FLOAT) => Fmt::Rgba32f,
            _ => return None,
        },
    })
}

fn ktx1_set(bytes: &[u8], first_only: bool) -> Result<TextureSet, String> {
    const MAGIC: [u8; 12] = [0xAB, b'K', b'T', b'X', b' ', b'1', b'1', 0xBB, b'\r', b'\n', 0x1A, b'\n'];
    if bytes.len() < 64 || bytes[..12] != MAGIC {
        return Err("not a KTX file".into());
    }
    let le = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) == 0x0403_0201;
    let read_u32 = |at: usize| -> Option<u32> {
        let b: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
        Some(if le { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) })
    };
    let word = |i: usize| read_u32(12 + i * 4).unwrap_or(0);
    let (gl_type, gl_format_, gl_internal) = (word(1), word(3), word(4));
    let (w0, h0, depth) = (word(6), word(7).max(1), word(8).max(1));
    let (array, faces, levels, kv_bytes) = (word(9), word(10).max(1), word(11).max(1), word(12) as usize);
    let fmt = gl_format(gl_internal, gl_format_, gl_type)
        .ok_or_else(|| format!("KTX format 0x{gl_internal:04X} isn't supported yet"))?;
    let layers = array.max(1);
    let align4 = |n: usize| (n + 3) & !3;
    let mut offset = 64 + kv_bytes;
    let mut subs = Vec::new();
    for level in 0..levels {
        let image_size = read_u32(offset).ok_or("KTX file is truncated")? as usize;
        offset += 4;
        let (w, h) = (mip(w0, level), mip(h0, level));
        let size = fmt.image_bytes(w, h);
        if faces == 6 && array == 0 {
            // Non-array cubemaps: imageSize is one face, each face padded to 4 bytes.
            for face in 0..6 {
                let chunk = bytes.get(offset..offset + size).ok_or("KTX file is truncated")?;
                subs.push(SubTexture { level, layer: 0, face, texture: decode(fmt, chunk, w, h)? });
                offset = align4(offset + image_size);
                if first_only {
                    return Ok(TextureSet { subs, levels, layers, faces });
                }
            }
        } else {
            let slices = mip(depth, level) as usize;
            for layer in 0..layers {
                for face in 0..faces {
                    let at = offset + ((layer * faces + face) as usize) * slices * size;
                    let chunk = bytes.get(at..at + size).ok_or("KTX file is truncated")?;
                    subs.push(SubTexture { level, layer, face, texture: decode(fmt, chunk, w, h)? });
                    if first_only {
                        return Ok(TextureSet { subs, levels, layers, faces });
                    }
                }
            }
            offset = align4(offset + image_size);
        }
    }
    Ok(TextureSet { subs, levels, layers, faces })
}
