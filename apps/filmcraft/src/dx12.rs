//! DX12/DXC renderer for shared D3D11 decode textures; Vulkan cannot import these handles.
//! Requires dxcompiler.dll + dxil.dll beside the executable or in FILMCRAFT_DXC_DIR; FXC cannot compile the compositor shader.
//! Missing DXC, unsupported compositor or FILMCRAFT_NO_ZERO_COPY=1 -> normal renderer + CPU readback.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui_wgpu::WgpuSetupExisting;
use eframe::wgpu;

/// The directory holding both DXC libraries, if there is one.
fn dxc_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("FILMCRAFT_DXC_DIR").map(PathBuf::from).or_else(|| std::env::current_exe().ok()?.parent().map(PathBuf::from))?;
    (dir.join("dxcompiler.dll").is_file() && dir.join("dxil.dll").is_file()).then_some(dir)
}

/// DX12/DXC instance, adapter and device after compositor construction succeeds; None retains normal startup.
pub fn setup() -> Option<WgpuSetupExisting> {
    if std::env::var_os("FILMCRAFT_NO_ZERO_COPY").is_some_and(|v| v != "0") {
        return None;
    }
    let dxc = dxc_dir()?.join("dxcompiler.dll");
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::DX12,
        backend_options: wgpu::BackendOptions {
            dx12: wgpu::Dx12BackendOptions {
                shader_compiler: wgpu::Dx12Compiler::DynamicDxc { dxc_path: dxc.to_string_lossy().into_owned() },
                ..Default::default()
            },
            ..Default::default()
        },
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(
        instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() }),
    )
    .ok()?;
    if filmcraft_ui_egui::gpu_compositor_unsupported(&adapter).is_some() {
        return None;
    }
    // P010 (10-bit) pictures are sampled as 16-bit normalised planes
    let features = adapter.features() & wgpu::Features::TEXTURE_FORMAT_16BIT_NORM;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor { required_features: features, ..Default::default() })).ok()?;
    // Probe the complete compositor on this DXC/device before replacing normal startup.
    let failed = Arc::new(AtomicBool::new(false));
    let flag = failed.clone();
    device.on_uncaptured_error(Arc::new(move |e: wgpu::Error| {
        log::warn!("DX12 renderer rejected: {e}");
        flag.store(true, Ordering::Relaxed);
    }));
    let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = filmcraft_gpu::GpuCompositor::new(&device, &queue);
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
    }));
    if built.is_err() || failed.load(Ordering::Relaxed) {
        return None;
    }
    // errors from here on belong to the app's own handler (`FilmcraftApp::set_wgpu`)
    device.on_uncaptured_error(Arc::new(|e: wgpu::Error| log::error!("GPU error: {e}")));
    log::info!("renderer: DX12 with DXC on {}", adapter.get_info().name);
    Some(WgpuSetupExisting { instance, adapter, device, queue })
}
