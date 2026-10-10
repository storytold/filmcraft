//! Windows graphics instance configuration, applied before wgpu can initialize any driver.

use eframe::{NativeOptions, egui_wgpu::WgpuSetup, wgpu::Backends};

/// Restrict the wgpu instance to the primary backends (Vulkan, DirectX 12, Metal), i.e. leave
/// OpenGL out, unless `backend_override` (normally `Backends::from_env()`, i.e. `WGPU_BACKEND`)
/// names other backends.
pub fn configure(options: &mut NativeOptions, backend_override: Option<Backends>) {
    if let WgpuSetup::CreateNew(create) = &mut options.wgpu_options.wgpu_setup {
        // eframe's default includes GL. Creating that backend can crash inside AMD's
        // atio6axx.dll before adapter selection or any Rust error handling runs, so the
        // window never appears. Choosing an adapter afterwards is too late: only leaving GL
        // out of the instance avoids it. Keep Vulkan and DX12 both: forcing DX12 alone sends
        // shaders through FXC (which rejects fx.wgsl, silently falling back to the CPU) and
        // leaves machines without DX12 with no backend at all.
        create.instance_descriptor.backends = backend_override.unwrap_or(Backends::PRIMARY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured_backends(backend_override: Option<Backends>) -> Backends {
        let mut options = NativeOptions::default();
        configure(&mut options, backend_override);
        let WgpuSetup::CreateNew(create) = options.wgpu_options.wgpu_setup else {
            panic!("default native options must create a wgpu instance");
        };
        create.instance_descriptor.backends
    }

    #[test]
    fn windows_default_leaves_out_gl() {
        // eframe's own default (PRIMARY | GL) would initialize OpenGL as well.
        let WgpuSetup::CreateNew(eframe_default) = NativeOptions::default().wgpu_options.wgpu_setup else {
            panic!("default native options must create a wgpu instance");
        };
        assert!(eframe_default.instance_descriptor.backends.contains(Backends::GL));
        let configured = configured_backends(None);
        assert_eq!(configured, Backends::PRIMARY);
        assert!(!configured.contains(Backends::GL));
        assert!(configured.contains(Backends::DX12) && configured.contains(Backends::VULKAN));
    }

    #[test]
    fn explicit_backend_override_is_preserved() {
        for backends in [Backends::VULKAN, Backends::GL, Backends::DX12, Backends::VULKAN | Backends::DX12, Backends::empty()] {
            assert_eq!(configured_backends(Some(backends)), backends);
        }
    }
}
