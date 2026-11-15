/// Largest texture side a plan needs on the GPU (output and every layer).
pub(crate) fn plan_side(plan: &filmcraft_render::plan::FramePlan) -> usize {
    plan.max_side()
}

/// A software rasterizer (llvmpipe, SwiftShader, softpipe — VMs and containers without GPU device
/// access, missing or broken drivers) runs wgpu entirely on the CPU. Compositing through one is
/// slower than the native CPU compositor and hides the cause: GPU usage stays at 0 % while the
/// CPU saturates. Returns a description when the adapter is one.
pub fn software_rasterizer(info: &eframe::wgpu::AdapterInfo) -> Option<String> {
    rasterizer_reason(&info.name, info.device_type)
}

fn rasterizer_reason(name: &str, device_type: eframe::wgpu::DeviceType) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    if device_type == eframe::wgpu::DeviceType::Cpu || ["llvmpipe", "swiftshader", "softpipe"].iter().any(|s| lower.contains(s)) {
        return Some(format!("software rasterizer ({name}, {device_type:?})"));
    }
    None
}

/// Whether the GPU compositor can run on this adapter: it renders and blends Rgba16Float
/// targets and uploads whole frames as textures. Older or OpenGL-backed GPUs (some Intel Macs,
/// VMs, Linux without Vulkan, WebGL) can't; those use the CPU compositor, which renders the same
/// frames. Returns the reason when unsupported.
pub fn gpu_compositor_unsupported(adapter: &eframe::wgpu::Adapter) -> Option<String> {
    use eframe::wgpu::{TextureFormat, TextureFormatFeatureFlags as F, TextureUsages as U};
    if let Some(why) = software_rasterizer(&adapter.get_info()) {
        return Some(why);
    }
    let f = adapter.get_texture_format_features(TextureFormat::Rgba16Float);
    if !f.allowed_usages.contains(U::RENDER_ATTACHMENT | U::TEXTURE_BINDING) {
        return Some("Rgba16Float is not renderable".into());
    }
    if !f.flags.contains(F::BLENDABLE | F::FILTERABLE) {
        return Some("Rgba16Float is not blendable/filterable".into());
    }
    let max = adapter.limits().max_texture_dimension_2d;
    if max < 4096 {
        return Some(format!("max texture size {max} < 4096"));
    }
    None
}

#[cfg(test)]
mod gpu_fallback_tests {
    use filmcraft_render::plan::{FramePlan, PlanLayer};

    /// Software rasterizers must not be treated as a GPU compositor: the preview crawled while
    /// "GPU" usage read 0 % because wgpu had silently fallen back to llvmpipe.
    #[test]
    fn software_rasterizers_are_detected() {
        use eframe::wgpu::DeviceType as D;
        let r = super::rasterizer_reason;
        assert!(r("llvmpipe (LLVM 23.1.1, 256 bits)", D::Cpu).is_some());
        assert!(r("NVIDIA GeForce RTX 4080", D::Cpu).is_some());
        assert!(r("SwiftShader Device", D::Other).is_some());
        assert!(r("softpipe", D::Cpu).is_some());
        assert!(r("AMD Radeon RX 5700 XT", D::DiscreteGpu).is_none());
        assert!(r("Apple M3 Pro", D::IntegratedGpu).is_none());
    }

    #[test]
    fn plan_side_covers_output_and_every_layer() {
        let frame = |w: u32, h: u32| std::sync::Arc::new(filmcraft_frame::VideoFrame::rgba_f32(w, h, vec![0.0; (w * h * 4) as usize]));
        let layer = |w, h| PlanLayer {
            frame: frame(w, h),
            matrix: filmcraft_geom::Affine::IDENTITY,
            opacity: 1.0,
            blend: filmcraft_render::Blend::Normal,
            fx: None,
            input: None,
        };
        // a 4K source in an HD sequence still needs a 3840-wide texture on the GPU
        let p = FramePlan::Layers { width: 1920, height: 1080, layers: vec![layer(64, 64), layer(3840, 2160)] };
        assert_eq!(super::plan_side(&p), 3840);
        let p = FramePlan::Layers { width: 1920, height: 1080, layers: vec![] };
        assert_eq!(super::plan_side(&p), 1920);
        let p = FramePlan::Image(filmcraft_render::Image::new(800, 4000));
        assert_eq!(super::plan_side(&p), 4000);
    }
}
