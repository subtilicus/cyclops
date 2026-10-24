//! GPU tone mapping for high-precision images (HDR/EXR, 16-bit, deep HEIC/AVIF/RAW).
//!
//! The linear-light pixels are uploaded once as a mipmapped half-float texture; a small shader
//! applies exposure, the ACES filmic curve and sRGB encoding while drawing. Changing exposure
//! only rewrites a 32-byte uniform, so the slider is instant at any image size.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use eframe::egui_wgpu::{self, wgpu};
use egui::{Pos2, Rect};

use crate::decode::HdrLevel;

const SHADER: &str = r#"
struct Params {
    uv_min: vec2<f32>,
    uv_max: vec2<f32>,
    exposure: f32,
    encode_srgb: f32,
    clip_only: f32,
    // 0 = flat image, 1 = mirror sphere, 2 = environment (panorama camera)
    mode: f32,
    yaw: f32,
    pitch: f32,
    tan_half_fov: f32,
    aspect: f32,
    lod: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> params: Params;
// Wraps horizontally: equirectangular maps are continuous across the left/right edge.
@group(0) @binding(3) var samp_wrap: sampler;

const PI: f32 = 3.14159265;

/// Direction → equirectangular UV (longitude across, latitude down; −Z is the centre).
fn dir_to_uv(d: vec3<f32>) -> vec2<f32> {
    let lon = atan2(d.x, -d.z);
    let lat = asin(clamp(d.y, -1.0, 1.0));
    return vec2<f32>(lon / (2.0 * PI) + 0.5, 0.5 - lat / PI);
}

/// Rotate by pitch (around X) then yaw (around Y).
fn orient(d: vec3<f32>) -> vec3<f32> {
    let cp = cos(params.pitch);
    let sp = sin(params.pitch);
    let p = vec3<f32>(d.x, d.y * cp + d.z * sp, -d.y * sp + d.z * cp);
    let cy = cos(params.yaw);
    let sy = sin(params.yaw);
    return vec3<f32>(p.x * cy + p.z * sy, p.y, -p.x * sy + p.z * cy);
}

struct VertexOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) i: u32) -> VertexOut {
    let corner = vec2<f32>(f32(i & 1u), f32(i >> 1u));
    var out: VertexOut;
    out.pos = vec4<f32>(corner.x * 2.0 - 1.0, 1.0 - corner.y * 2.0, 0.0, 1.0);
    out.uv = mix(params.uv_min, params.uv_max, corner);
    return out;
}

fn aces(x: vec3<f32>) -> vec3<f32> {
    return clamp((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14), vec3<f32>(0.0), vec3<f32>(1.0));
}

fn to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(c, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3<f32>(0.0031308));
}

@fragment
fn fs(in: VertexOut) -> @location(0) vec4<f32> {
    var texel: vec4<f32>;
    var coverage = 1.0;
    if (params.mode < 0.5) {
        texel = textureSample(tex, samp, in.uv);
    } else {
        // Screen position in [-1, 1], y up, corrected for the view's aspect ratio.
        let ndc = vec2<f32>(in.uv.x * 2.0 - 1.0, 1.0 - in.uv.y * 2.0);
        var dir: vec3<f32>;
        if (params.mode < 1.5) {
            // Mirror ball filling the shorter side: reflect the view ray off the sphere.
            let q = ndc * vec2<f32>(max(params.aspect, 1.0), max(1.0 / params.aspect, 1.0)) / 0.92;
            let r2 = dot(q, q);
            let edge = fwidth(sqrt(r2));
            coverage = 1.0 - smoothstep(1.0 - edge, 1.0, sqrt(r2));
            if (coverage <= 0.0) {
                return vec4<f32>(0.0);
            }
            let n = vec3<f32>(q, sqrt(max(1.0 - r2, 0.0)));
            dir = reflect(vec3<f32>(0.0, 0.0, -1.0), n);
        } else {
            dir = normalize(vec3<f32>(ndc.x * params.tan_half_fov * params.aspect, ndc.y * params.tan_half_fov, -1.0));
        }
        texel = textureSampleLevel(tex, samp_wrap, dir_to_uv(orient(dir)), params.lod);
    }
    let exposed = max(texel.rgb * params.exposure, vec3<f32>(0.0));
    // Display-referred sources (16-bit photos) just clip; scene-referred HDR is tone-mapped.
    var rgb = select(aces(exposed), min(exposed, vec3<f32>(1.0)), params.clip_only > 0.5);
    if (params.encode_srgb > 0.5) {
        rgb = to_srgb(rgb);
    }
    let a = clamp(texel.a, 0.0, 1.0) * coverage;
    return vec4<f32>(rgb * a, a);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    uv_min: [f32; 2],
    uv_max: [f32; 2],
    exposure: f32,
    encode_srgb: f32,
    clip_only: f32,
    mode: f32,
    yaw: f32,
    pitch: f32,
    tan_half_fov: f32,
    aspect: f32,
    lod: f32,
    _pad: [f32; 3],
}

/// How an HDR image is presented.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EnvMode {
    /// The image itself.
    Flat,
    /// The environment reflected in a mirror ball.
    Sphere,
    /// Looking around inside the environment (equirectangular panorama).
    Panorama,
}

/// Camera for the sphere / panorama modes.
#[derive(Clone, Copy, Debug)]
pub struct EnvView {
    pub mode: EnvMode,
    pub yaw: f32,
    pub pitch: f32,
    /// Vertical field of view in degrees (panorama).
    pub fov: f32,
}

impl Default for EnvView {
    fn default() -> Self {
        Self { mode: EnvMode::Flat, yaw: 0.0, pitch: 0.0, fov: 75.0 }
    }
}

struct Uploaded {
    bind_group: wgpu::BindGroup,
    params: wgpu::Buffer,
}

/// Lives in egui-wgpu's callback resources.
struct Resources {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    sampler_wrap: wgpu::Sampler,
    textures: HashMap<u64, Uploaded>,
    encode_srgb: bool,
}

/// Create the pipeline. Call once with eframe's wgpu render state.
pub fn init(render_state: &egui_wgpu::RenderState) {
    let device = &render_state.device;
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("cyclops-hdr"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("cyclops-hdr"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("cyclops-hdr"),
        bind_group_layouts: &[Some(&layout)],
        ..Default::default()
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("cyclops-hdr"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: render_state.target_format,
                blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("cyclops-hdr"),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Linear,
        ..Default::default()
    });
    let sampler_wrap = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("cyclops-hdr-wrap"),
        address_mode_u: wgpu::AddressMode::Repeat,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Linear,
        ..Default::default()
    });
    // egui renders in gamma space into a non-sRGB target; encode ourselves unless the
    // target already does it.
    let encode_srgb = !render_state.target_format.is_srgb();
    render_state.renderer.write().callback_resources.insert(Resources {
        pipeline,
        layout,
        sampler,
        sampler_wrap,
        textures: HashMap::new(),
        encode_srgb,
    });
}

/// An uploaded HDR texture; freed from the GPU when dropped.
pub struct HdrTexture {
    id: u64,
    render_state: egui_wgpu::RenderState,
    /// Width of mip 0, for choosing the sampling level in the 3D views.
    width: u32,
}

impl Drop for HdrTexture {
    fn drop(&mut self) {
        if let Some(res) = self.render_state.renderer.write().callback_resources.get_mut::<Resources>() {
            res.textures.remove(&self.id);
        }
    }
}

/// Upload the mip chain (level 0 first, RGBA f16 texels).
pub fn upload(render_state: &egui_wgpu::RenderState, levels: &[HdrLevel]) -> Option<HdrTexture> {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let base = levels.first()?;
    let device = &render_state.device;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("cyclops-hdr"),
        size: wgpu::Extent3d { width: base.size[0], height: base.size[1], depth_or_array_layers: 1 },
        mip_level_count: levels.len() as u32,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    for (mip, level) in levels.iter().enumerate() {
        let [w, h] = level.size;
        render_state.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: mip as u32,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&level.texels),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * 8), rows_per_image: Some(h) },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
    }
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let params = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("cyclops-hdr-params"),
        size: std::mem::size_of::<Params>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut renderer = render_state.renderer.write();
    let res = renderer.callback_resources.get_mut::<Resources>()?;
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("cyclops-hdr"),
        layout: &res.layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&res.sampler) },
            wgpu::BindGroupEntry { binding: 2, resource: params.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&res.sampler_wrap) },
        ],
    });
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    res.textures.insert(id, Uploaded { bind_group, params });
    drop(renderer);
    Some(HdrTexture { id, render_state: render_state.clone(), width: base.size[0] })
}

impl HdrTexture {
    /// A paint shape drawing this image into `draw` (clipped to `clip`) at `exposure`.
    pub fn paint(&self, draw: Rect, clip: Rect, exposure: f32, tonemap: bool) -> Option<egui::Shape> {
        let visible = draw.intersect(clip);
        if !visible.is_positive() {
            return None;
        }
        let uv = |p: Pos2| ((p - draw.min) / draw.size()).to_pos2();
        let callback = Callback {
            id: self.id,
            params: Params {
                uv_min: uv(visible.min).into(),
                uv_max: uv(visible.max).into(),
                exposure,
                encode_srgb: 0.0, // filled in prepare
                clip_only: if tonemap { 0.0 } else { 1.0 },
                mode: 0.0,
                yaw: 0.0,
                pitch: 0.0,
                tan_half_fov: 1.0,
                aspect: 1.0,
                lod: 0.0,
                _pad: [0.0; 3],
            },
        };
        Some(egui::Shape::Callback(egui_wgpu::Callback::new_paint_callback(visible, callback)))
    }

    /// Paint the image as a mirror sphere or panorama filling `area` (`ppp`: pixels per point).
    pub fn paint_env(&self, area: Rect, exposure: f32, tonemap: bool, view: EnvView, ppp: f32) -> Option<egui::Shape> {
        if !area.is_positive() {
            return None;
        }
        let aspect = area.width() / area.height();
        let tan_half = (view.fov.to_radians() / 2.0).tan();
        // Match texel density to screen density so the view neither shimmers nor blurs.
        let texels_per_radian = self.width as f32 / std::f32::consts::TAU;
        let screen_per_radian = match view.mode {
            EnvMode::Panorama => area.height() * ppp / (2.0 * tan_half),
            _ => area.width().min(area.height()) * ppp * 0.25,
        };
        let lod = (texels_per_radian / screen_per_radian.max(1.0)).log2().max(0.0);
        let callback = Callback {
            id: self.id,
            params: Params {
                uv_min: [0.0, 0.0],
                uv_max: [1.0, 1.0],
                exposure,
                encode_srgb: 0.0,
                clip_only: if tonemap { 0.0 } else { 1.0 },
                mode: if view.mode == EnvMode::Sphere { 1.0 } else { 2.0 },
                yaw: view.yaw,
                pitch: view.pitch,
                tan_half_fov: tan_half,
                aspect,
                lod,
                _pad: [0.0; 3],
            },
        };
        Some(egui::Shape::Callback(egui_wgpu::Callback::new_paint_callback(area, callback)))
    }
}

struct Callback {
    id: u64,
    params: Params,
}

impl egui_wgpu::CallbackTrait for Callback {
    fn prepare(
        &self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(res) = resources.get::<Resources>() {
            if let Some(tex) = res.textures.get(&self.id) {
                let params = Params { encode_srgb: if res.encode_srgb { 1.0 } else { 0.0 }, ..self.params };
                queue.write_buffer(&tex.params, 0, bytemuck::bytes_of(&params));
            }
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        let Some(res) = resources.get::<Resources>() else { return };
        let Some(tex) = res.textures.get(&self.id) else { return };
        pass.set_pipeline(&res.pipeline);
        pass.set_bind_group(0, &tex.bind_group, &[]);
        pass.draw(0..4, 0..1);
    }
}
