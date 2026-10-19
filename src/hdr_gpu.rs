//! GPU tone mapping for HDR/EXR images.
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
    _pad: vec2<f32>,
};

@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> params: Params;

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
    let texel = textureSample(tex, samp, in.uv);
    var rgb = aces(max(texel.rgb * params.exposure, vec3<f32>(0.0)));
    if (params.encode_srgb > 0.5) {
        rgb = to_srgb(rgb);
    }
    let a = clamp(texel.a, 0.0, 1.0);
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
    _pad: [f32; 2],
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
    // egui renders in gamma space into a non-sRGB target; encode ourselves unless the
    // target already does it.
    let encode_srgb = !render_state.target_format.is_srgb();
    render_state.renderer.write().callback_resources.insert(Resources {
        pipeline,
        layout,
        sampler,
        textures: HashMap::new(),
        encode_srgb,
    });
}

/// An uploaded HDR texture; freed from the GPU when dropped.
pub struct HdrTexture {
    id: u64,
    render_state: egui_wgpu::RenderState,
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
        ],
    });
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    res.textures.insert(id, Uploaded { bind_group, params });
    drop(renderer);
    Some(HdrTexture { id, render_state: render_state.clone() })
}

impl HdrTexture {
    /// A paint shape drawing this image into `draw` (clipped to `clip`) at `exposure`.
    pub fn paint(&self, draw: Rect, clip: Rect, exposure: f32) -> Option<egui::Shape> {
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
                _pad: [0.0; 2],
            },
        };
        Some(egui::Shape::Callback(egui_wgpu::Callback::new_paint_callback(visible, callback)))
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
