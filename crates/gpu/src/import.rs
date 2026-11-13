//! A hardware encoder's input pictures in GPU memory ([`filmcraft_frame::GpuTarget`]) opened on
//! the export compositor's device, so an export draws its finished frames straight into them
//! ([`crate::GpuCompositor::render_export_to`]) instead of reading them back for the encoder to
//! upload again. The app registers the importer that knows them (filmcraft-platform on Windows:
//! NVENC's Direct3D 11 textures shared with a Vulkan device).

use std::sync::OnceLock;

/// Opens an encoder's input pictures on a device.
pub trait TargetImporter: Send + Sync {
    /// The device features importing needs: a device made to take such pictures asks for them.
    fn features(&self) -> wgpu::Features;
    /// `target` as a texture of `device` (`Rgba8Unorm` at the target's size) that work submitted
    /// from now on can copy the finished frame into, or `None` when this importer does not know
    /// the target or the device cannot take it. The caller waits for that work before handing the
    /// target back to its encoder.
    fn import(&self, device: &wgpu::Device, target: &dyn filmcraft_frame::GpuTarget) -> Option<wgpu::Texture>;
}

static IMPORTER: OnceLock<Box<dyn TargetImporter>> = OnceLock::new();

/// Register the process's importer (the app does at startup, before any export renderer exists).
/// Only the first registration counts.
pub fn register_target_importer(importer: Box<dyn TargetImporter>) {
    let _ = IMPORTER.set(importer);
}

/// The registered importer's device features (empty without one).
pub fn importer_features() -> wgpu::Features {
    IMPORTER.get().map_or(wgpu::Features::empty(), |i| i.features())
}

pub(crate) fn import_target(device: &wgpu::Device, target: &dyn filmcraft_frame::GpuTarget) -> Option<wgpu::Texture> {
    IMPORTER.get()?.import(device, target)
}
