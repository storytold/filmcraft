//! The immersive-video WGSL kernels (`vr.wgsl`) parse and validate, and expose every kernel name
//! the render crate's VR plan uses. Parity with the CPU reference runs in `filmcraft_render`
//! (`vfx::immersive::row_tests`, which executes the same plan plus the kernels' semantics on the
//! CPU). Parity on the GPU itself is not measured here: see `TESTE-GPU-MULTIPASS-VR.md`.

use filmcraft_render::vfx::{VR_KERNEL_COL, VR_KERNEL_COPY, VR_KERNEL_DENOISE, VR_KERNEL_ROW, VR_KERNEL_SHARPEN};

fn module() -> wgpu::naga::Module {
    wgpu::naga::front::wgsl::parse_str(include_str!("vr.wgsl")).expect("vr.wgsl parses")
}

#[test]
fn vr_wgsl_validates() {
    let module = module();
    wgpu::naga::valid::Validator::new(wgpu::naga::valid::ValidationFlags::all(), wgpu::naga::valid::Capabilities::all())
        .validate(&module)
        .expect("vr.wgsl validates");
}

#[test]
fn vr_wgsl_exposes_every_plan_kernel() {
    let module = module();
    let names: Vec<&str> = module.entry_points.iter().map(|e| e.name.as_str()).collect();
    for kernel in [VR_KERNEL_ROW, VR_KERNEL_COL, VR_KERNEL_COPY, VR_KERNEL_SHARPEN, VR_KERNEL_DENOISE] {
        assert!(names.contains(&kernel), "{kernel} missing from vr.wgsl");
    }
}
