//! Opt-in runtime probe for a local AMD Vulkan adapter.

#![cfg(target_os = "linux")]

#[test]
#[ignore = "requires an AMD Vulkan GPU with DMA-BUF and VK_EXT_queue_family_foreign support"]
fn opens_interop_device_from_selected_amd_adapter() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor { backends: wgpu::Backends::VULKAN, ..wgpu::InstanceDescriptor::new_without_display_handle() });
    let adapter = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN))
        .into_iter()
        .find(|adapter| {
            let info = adapter.get_info();
            info.vendor == 0x1002 && matches!(info.device_type, wgpu::DeviceType::IntegratedGpu | wgpu::DeviceType::DiscreteGpu)
        })
        .unwrap_or_else(|| panic!("no physical AMD Vulkan adapter"));
    let node = filmcraft_platform::vulkan_import::render_node_id(&adapter).expect("adapter must expose DRM render-node identity");
    let (device, _queue) = filmcraft_platform::vulkan_interop_device::create_amd_interop_device(&adapter, &wgpu::DeviceDescriptor::default(), node)
        .expect("selected AMD adapter must open with DMA-BUF and foreign queue-family capabilities");
    assert!(device.features().contains(wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF));
}
