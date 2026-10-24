//! The animated ring behind the Cyclops name on the About page: a small fragment shader drawn
//! through an egui-wgpu paint callback, added on top of a dark banner.

use eframe::egui_wgpu::{self, wgpu};
use egui::{Pos2, Rect};

const SHADER: &str = r#"
struct Params {
    // Banner size, ring centre and ring scale, in physical pixels.
    size: vec2<f32>,
    center: vec2<f32>,
    scale: f32,
    time: f32,
    corner_radius: f32,
    linear_out: f32,
};

@group(0) @binding(0) var<uniform> params: Params;

struct VertexOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) i: u32) -> VertexOut {
    let corner = vec2<f32>(f32(i & 1u), f32(i >> 1u));
    var out: VertexOut;
    out.pos = vec4<f32>(corner.x * 2.0 - 1.0, 1.0 - corner.y * 2.0, 0.0, 1.0);
    out.uv = corner;
    return out;
}

@fragment
fn fs(in: VertexOut) -> @location(0) vec4<f32> {
    let c = in.uv * params.size;
    // y up, like the original GLSL.
    let p = vec2<f32>(c.x - params.center.x, params.center.y - c.y) * 2.0 / params.scale / 0.6;
    let l = length(p) - 1.0;
    let a = atan2(p.y, p.x);
    let t = params.time;
    let glow = max(0.2 / l, -0.02 - 0.02 / l);
    let petals = 2.0 + cos(a * 8.0 + cos(a * 5.0 + t)) * sin(a * 4.0 - t);
    let o = tanh((cos(p.x + t / 2.0 + vec4<f32>(1.0, 2.0, 3.0, 0.0)) + 1.5) * glow / petals);
    var rgb = clamp(o.rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    // Rounded banner corners.
    let half = params.size * 0.5;
    let d = abs(c - half) - (half - vec2<f32>(params.corner_radius));
    let dist = length(max(d, vec2<f32>(0.0))) + min(max(d.x, d.y), 0.0) - params.corner_radius;
    rgb = rgb * clamp(0.5 - dist, 0.0, 1.0);
    if (params.linear_out > 0.5) {
        rgb = pow(rgb, vec3<f32>(2.2));
    }
    // Alpha 0 with premultiplied blending adds the glow to the banner underneath.
    return vec4<f32>(rgb, 0.0);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    size: [f32; 2],
    center: [f32; 2],
    scale: f32,
    time: f32,
    corner_radius: f32,
    linear_out: f32,
}

struct Resources {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    params: wgpu::Buffer,
    /// The target stores linear values (sRGB format), so the shader's display values are decoded.
    linear_out: bool,
}

/// Create the pipeline. Call once with eframe's wgpu render state.
pub fn init(render_state: &egui_wgpu::RenderState) {
    let device = &render_state.device;
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("cyclops-about"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("cyclops-about"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("cyclops-about"),
        bind_group_layouts: &[Some(&layout)],
        ..Default::default()
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("cyclops-about"),
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
    let params = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("cyclops-about-params"),
        size: std::mem::size_of::<Params>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("cyclops-about"),
        layout: &layout,
        entries: &[wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() }],
    });
    let linear_out = render_state.target_format.is_srgb();
    render_state.renderer.write().callback_resources.insert(Resources { pipeline, bind_group, params, linear_out });
}

/// The ring drawn over `banner`, centred on `center`; `scale` sets its size (the ring's radius
/// is 0.3 × `scale`). All in points; `ppp` is pixels per point.
pub fn paint(banner: Rect, center: Pos2, scale: f32, corner_radius: f32, time: f32, ppp: f32) -> egui::Shape {
    let params = Params {
        size: (banner.size() * ppp).into(),
        center: ((center - banner.min) * ppp).into(),
        scale: scale * ppp,
        // Keep the float precise when the app has been running for a long time.
        time: (time as f64 % 10_000.0) as f32,
        corner_radius: corner_radius * ppp,
        linear_out: 0.0,
    };
    egui::Shape::Callback(egui_wgpu::Callback::new_paint_callback(banner, Callback { params }))
}

struct Callback {
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
            let params = Params { linear_out: if res.linear_out { 1.0 } else { 0.0 }, ..self.params };
            queue.write_buffer(&res.params, 0, bytemuck::bytes_of(&params));
        }
        Vec::new()
    }

    fn paint(&self, _info: egui::PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>, resources: &egui_wgpu::CallbackResources) {
        let Some(res) = resources.get::<Resources>() else { return };
        pass.set_pipeline(&res.pipeline);
        pass.set_bind_group(0, &res.bind_group, &[]);
        pass.draw(0..4, 0..1);
    }
}
