//! The zero-copy picture type of the Windows backend: [`MfSurface`] is a decoded NV12 / P010
//! picture in a shareable Direct3D 11 texture ([`interop::SharedSurface`]) that implements
//! [`filmcraft_frame::GpuSurface`]. The GPU compositor opens it on its own DX12 device
//! ([`import`]); every CPU consumer reads it back through [`MfSurface::download`] (the same staging
//! copy the readback path uses), once per picture.
//!
//! Safe code: the FFI is in `gpu` and `interop`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use filmcraft_frame::{GpuSurface, PixelData};

use super::gpu::{Gpu, Readback};
use super::interop::{self, SharedSurface};
use crate::biplanar::{self, Geometry};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
/// Whether new decoders hand out GPU pictures ([`enable`]; `filmcraft_codecs::hw::gpu_frames`).
pub fn enabled() -> bool {
    filmcraft_codecs::hw::gpu_frames()
}

/// Turn zero-copy decoding on if `device` (the renderer's) can open the decoder's surfaces: a DX12
/// device on the adapter the shared Direct3D 11 device uses. Returns whether it is on. Call it
/// once after the renderer's device exists; it also registers how the compositor opens surfaces.
pub fn enable(device: &wgpu::Device) -> bool {
    let ok = super::mft::api().and_then(Gpu::shared).is_ok_and(|gpu| interop::same_adapter(device, &gpu));
    if ok {
        filmcraft_gpu::set_surface_importer(import);
    } else {
        log::info!("zero-copy decoding is off: the renderer is not a DX12 device on the decoder's adapter");
    }
    filmcraft_codecs::hw::set_gpu_frames(ok);
    ok
}

/// Turn zero-copy decoding off (Settings, tests): new pictures are CPU planes again.
pub fn disable() {
    filmcraft_codecs::hw::set_gpu_frames(false);
}

/// A decoded picture in GPU memory.
pub struct MfSurface {
    id: u64,
    shared: Arc<SharedSurface>,
    gpu: Arc<Gpu>,
    geometry: Geometry,
}

impl std::fmt::Debug for MfSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MfSurface").field("id", &self.id).field("size", &self.shared.size()).field("format", &self.shared.format()).finish()
    }
}

impl MfSurface {
    pub(super) fn new(gpu: Arc<Gpu>, shared: Arc<SharedSurface>, geometry: Geometry) -> Self {
        Self { id: NEXT_ID.fetch_add(1, Ordering::Relaxed), shared, gpu, geometry }
    }
}

impl GpuSurface for MfSurface {
    fn size(&self) -> (u32, u32) {
        self.shared.size()
    }

    fn byte_len(&self) -> usize {
        let (w, h) = self.shared.size();
        let bps = if self.shared.format().bits() > 8 { 2 } else { 1 };
        (w as usize).saturating_mul(h as usize).saturating_mul(3).saturating_mul(bps) / 2
    }

    fn download(&self) -> Result<PixelData, String> {
        // the surface holds the cropped picture, so the geometry's crop is its whole extent
        let (w, h) = self.shared.size();
        let geometry = Geometry { crop: (0, 0, w, h), ..self.geometry };
        let frame = Readback::default().read_texture(&self.gpu, self.shared.texture(), 0, self.shared.format(), |b| biplanar::to_frame(b, &geometry))?;
        Ok(frame.data)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn id(&self) -> u64 {
        self.id
    }
}

/// The compositor's way in: open `surface` (which must be an [`MfSurface`]) on `device`.
fn import(surface: &dyn GpuSurface, device: &wgpu::Device) -> Result<filmcraft_gpu::ImportedPlanes, String> {
    let s = surface.as_any().downcast_ref::<MfSurface>().ok_or("not a Media Foundation surface")?;
    let planes = interop::import(device, &s.shared)?;
    Ok(filmcraft_gpu::ImportedPlanes { luma: planes.luma, chroma: planes.chroma, keepalive: Box::new(s.shared.clone()) })
}
