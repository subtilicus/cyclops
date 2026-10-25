//! Times the GPU setup steps eframe performs at startup (DX12, as Cyclops runs):
//! `cargo run --release --example startup_probe`

use std::time::Instant;

use eframe::egui_wgpu::wgpu;

/// wgpu's native futures complete immediately; poll once with a no-op waker.
fn block_on<F: std::future::Future>(f: F) -> F::Output {
    let mut f = std::pin::pin!(f);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

fn ms(t: Instant) -> String {
    format!("{:7.1} ms", t.elapsed().as_secs_f64() * 1e3)
}

fn main() {
    // SAFETY: single-threaded at this point.
    unsafe {
        std::env::set_var("WGPU_BACKEND", "dx12");
        std::env::set_var("WGPU_DX12_PRESENTATION_SYSTEM", "Visual");
    }
    let total = Instant::now();
    let t = Instant::now();
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY),
        flags: wgpu::InstanceFlags::from_build_config().with_env(),
        backend_options: wgpu::BackendOptions::from_env_or_default(),
        memory_budget_thresholds: Default::default(),
        display: None,
    });
    println!("Instance::new          {}", ms(t));

    let t = Instant::now();
    let adapters = if std::env::var_os("SKIP_ENUM").is_some() {
        Vec::new()
    } else {
        block_on(instance.enumerate_adapters(wgpu::Backends::all()))
    };
    println!("enumerate_adapters     {}  ({} adapters)", ms(t), adapters.len());
    for a in &adapters {
        let i = a.get_info();
        println!("    {} {:?} {:?}", i.name, i.device_type, i.backend);
    }

    let t = Instant::now();
    let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .expect("adapter");
    println!("request_adapter        {}  -> {}", ms(t), adapter.get_info().name);

    let t = Instant::now();
    let (device, _queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: wgpu::Limits { max_texture_dimension_2d: 8192, ..Default::default() },
        ..Default::default()
    }))
    .expect("device");
    println!("request_device         {}", ms(t));
    println!("adapter max texture    {}", adapter.limits().max_texture_dimension_2d);
    // What egui-wgpu does again with a ready-made device (it lists adapters for its log).
    let t = Instant::now();
    let again = block_on(instance.enumerate_adapters(wgpu::Backends::all()));
    println!("enumerate again        {}  ({} adapters)", ms(t), again.len());
    drop(device);
    println!("total                  {}", ms(total));
}
