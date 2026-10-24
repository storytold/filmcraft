//! Opt-in, ignored hardware probe for VA DMA-BUF object import as a Vulkan buffer.
#![cfg(target_os = "linux")]

use filmcraft_platform::{
    vaapi::{
        ffi,
        va::{Display, EncodeSession, encode_caps},
    },
    vulkan_buffer_probe::probe_va_nv12_buffer_import,
    vulkan_interop_device::create_amd_interop_device,
};

#[test]
#[ignore = "requires explicit AMD Vulkan and VA-API hardware validation"]
fn imports_va_linear_nv12_object_as_vulkan_buffer() {
    let display = Display::open().expect("VA display");
    let node = display.drm_node_id().expect("VA DRM render node");
    let (profile, caps) = [ffi::VAProfileH264High, ffi::VAProfileH264Main]
        .into_iter()
        .find_map(|profile| encode_caps(profile).ok().map(|caps| (profile, caps)))
        .expect("AMD VA-API H.264 encode profile");
    let rc = if caps.rate_control & ffi::VA_RC_VBR != 0 { ffi::VA_RC_VBR } else { ffi::VA_RC_CBR };
    let session = EncodeSession::new_linear_diagnostic(node, profile, rc, 640, 368).expect("LINEAR NV12 session");
    let exported = session.export_linear_input_surface().expect("LINEAR NV12 export");

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor { backends: wgpu::Backends::VULKAN, ..wgpu::InstanceDescriptor::new_without_display_handle() });
    let adapter = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN))
        .into_iter()
        .find(|adapter| adapter.get_info().vendor == 0x1002)
        .expect("AMD Vulkan adapter");
    let (device, _queue) = create_amd_interop_device(&adapter, &wgpu::DeviceDescriptor::default(), node).expect("same-node AMD interop device");
    let report = probe_va_nv12_buffer_import(&adapter, &device, &exported).expect("DMA-BUF buffer import");
    eprintln!(
        "adapter={:?} VA={} node={:?} layout={:?} memory_type={} bits={:#x} allocation_size={}",
        adapter.get_info(),
        session.vendor(),
        report.drm_node,
        report,
        report.memory_type_index,
        report.memory_type_bits,
        report.allocation_size
    );
}
