//! Zero-copy hand-over of a decoded picture to wgpu (DX12 backend): the picture lives in an NV12 /
//! P010 Direct3D 11 texture created shareable (NT handle); wgpu opens the same memory as a
//! Direct3D 12 resource and wraps its two planes as single-plane textures (`R8Unorm` / `Rg8Unorm`,
//! `R16Unorm` / `Rg16Unorm`), which the compositor samples like the planes it uploads today.
//! No pixel crosses the PCIe bus and the CPU never touches the picture.
//!
//! FFI module (docs/adr/0001-platform-ffi.md, docs/adr/0002-zero-copy-interop.md): every `unsafe`
//! block has a `// SAFETY:` comment; the public items are safe and return `Result<_, String>`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_SHADER_RESOURCE, D3D11_RESOURCE_MISC_SHARED, D3D11_RESOURCE_MISC_SHARED_NTHANDLE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, ID3D11Texture2D,
};
use windows::Win32::Graphics::Direct3D12::{ID3D12Device, ID3D12Resource};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_FORMAT_P010, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{DXGI_SHARED_RESOURCE_READ, DXGI_SHARED_RESOURCE_WRITE, IDXGIResource1};
use windows::core::{Interface, PCWSTR};

use super::gpu::{Gpu, SurfaceFormat};

/// A shareable NV12 / P010 texture: written by Direct3D 11 (the decoder side), readable by
/// Direct3D 12 through its NT handle. Dropping it closes the handle and releases the texture.
pub struct SharedSurface {
    texture: ID3D11Texture2D,
    handle: HANDLE,
    format: SurfaceFormat,
    size: (u32, u32),
}

// SAFETY: the texture is a free-threaded Direct3D 11 resource of the multithread-protected shared
// device, and an NT handle is a plain process-wide kernel handle; neither is tied to a thread.
unsafe impl Send for SharedSurface {}
// SAFETY: see above; the surface has no interior mutability.
unsafe impl Sync for SharedSurface {}

static LIVE: AtomicUsize = AtomicUsize::new(0);

/// Shareable surfaces alive right now (leak checks in tests, diagnostics).
pub fn live_surfaces() -> usize {
    LIVE.load(Ordering::Relaxed)
}

impl Drop for SharedSurface {
    fn drop(&mut self) {
        LIVE.fetch_sub(1, Ordering::Relaxed);
        // SAFETY: the handle was created by `CreateSharedHandle` for this surface and is closed
        // exactly once, here. Direct3D 12 resources opened from it keep their own reference.
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

impl SharedSurface {
    /// A shareable texture of `size` luma samples (even sizes only: 4:2:0).
    pub fn create(gpu: &Gpu, format: SurfaceFormat, size: (u32, u32)) -> Result<Self, String> {
        if size.0 == 0 || size.1 == 0 || !size.0.is_multiple_of(2) || !size.1.is_multiple_of(2) || size.0 > 16384 || size.1 > 16384 {
            return Err(format!("cannot share a {}x{} 4:2:0 picture", size.0, size.1));
        }
        let desc = D3D11_TEXTURE2D_DESC {
            Width: size.0,
            Height: size.1,
            MipLevels: 1,
            ArraySize: 1,
            Format: if format == SurfaceFormat::Nv12 { DXGI_FORMAT_NV12 } else { DXGI_FORMAT_P010 },
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: (D3D11_RESOURCE_MISC_SHARED | D3D11_RESOURCE_MISC_SHARED_NTHANDLE).0 as u32,
        };
        let mut texture: Option<ID3D11Texture2D> = None;
        // SAFETY: a valid descriptor and out-pointer on a live device; no initial data.
        unsafe { gpu.device().CreateTexture2D(&desc, None, Some(&mut texture)) }.map_err(|e| format!("cannot create a shareable surface: {e}"))?;
        let texture = texture.ok_or("no shareable surface")?;
        let dxgi: IDXGIResource1 = texture.cast().map_err(|e| format!("surface is not shareable: {e}"))?;
        // SAFETY: plain COM call with no security attributes and no name: an anonymous handle.
        let handle = unsafe { dxgi.CreateSharedHandle(None, DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0, PCWSTR::null()) }
            .map_err(|e| format!("cannot share the surface: {e}"))?;
        LIVE.fetch_add(1, Ordering::Relaxed);
        Ok(Self { texture, handle, format, size })
    }

    pub fn texture(&self) -> &ID3D11Texture2D {
        &self.texture
    }

    pub fn format(&self) -> SurfaceFormat {
        self.format
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }
}

/// The two planes of a surface as wgpu textures on one device (luma, interleaved chroma).
pub struct ImportedPlanes {
    pub luma: wgpu::Texture,
    pub chroma: wgpu::Texture,
    // keeps the shared surface (and so its memory) alive as long as wgpu uses the planes
    _surface: Arc<SharedSurface>,
}

/// Whether `device` is a wgpu DX12 device on the adapter (LUID) the shared Direct3D 11 device
/// runs on: the only combination whose memory can be shared.
pub fn same_adapter(device: &wgpu::Device, gpu: &Gpu) -> bool {
    // SAFETY: only reads the Direct3D 12 device's adapter LUID through a borrowed hal device.
    let luid = unsafe {
        let Some(hal) = device.as_hal::<wgpu::hal::api::Dx12>() else { return false };
        hal.raw_device().GetAdapterLuid()
    };
    gpu.luid() == Some((luid.LowPart, luid.HighPart))
}

/// Open `surface` on `device` as two single-plane wgpu textures. Fails when the device is not DX12
/// or not on the surface's adapter.
pub fn import(device: &wgpu::Device, surface: &Arc<SharedSurface>) -> Result<ImportedPlanes, String> {
    let (w, h) = surface.size;
    let (luma_format, chroma_format) = match surface.format {
        SurfaceFormat::Nv12 => (wgpu::TextureFormat::R8Unorm, wgpu::TextureFormat::Rg8Unorm),
        SurfaceFormat::P010 => (wgpu::TextureFormat::R16Unorm, wgpu::TextureFormat::Rg16Unorm),
    };
    let open = |plane: u32, format: wgpu::TextureFormat, size: (u32, u32)| -> Result<wgpu::Texture, String> {
        // SAFETY: `hal` is borrowed from a live wgpu DX12 device; `OpenSharedHandle` writes one
        // new `ID3D12Resource` reference into `resource`; the hal texture wraps that resource
        // (with the plane pinned) and `create_texture_from_hal` takes ownership of the wrapper.
        // The resource is in the COMMON state, which `UNINITIALIZED` declares to wgpu, and its
        // shared memory outlives the wrapper through the `Arc<SharedSurface>` held next to it.
        unsafe {
            let hal = device.as_hal::<wgpu::hal::api::Dx12>().ok_or("not a DX12 device")?;
            let mut resource: Option<ID3D12Resource> = None;
            let dev: &ID3D12Device = hal.raw_device();
            dev.OpenSharedHandle(surface.handle, &mut resource).map_err(|e| format!("Direct3D 12 cannot open the decoded surface: {e}"))?;
            let resource = resource.ok_or("no Direct3D 12 resource")?;
            let extent = wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 };
            let tex = wgpu::hal::dx12::Device::texture_from_raw(resource, format, wgpu::TextureDimension::D2, extent, 1, 1).with_plane_slice(plane);
            drop(hal);
            let desc = wgpu::TextureDescriptor {
                label: Some("decoded plane"),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            };
            Ok(device.create_texture_from_hal::<wgpu::hal::api::Dx12>(tex, &desc, wgpu::TextureUses::UNINITIALIZED))
        }
    };
    Ok(ImportedPlanes { luma: open(0, luma_format, (w, h))?, chroma: open(1, chroma_format, (w / 2, h / 2))?, _surface: surface.clone() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media_foundation::mft;

    /// A wgpu DX12 device (None when this machine has none).
    fn dx12() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor { backends: wgpu::Backends::DX12, ..wgpu::InstanceDescriptor::new_without_display_handle() });
        let adapter = pollster::block_on(
            instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() }),
        )
        .ok()?;
        let features = adapter.features() & wgpu::Features::TEXTURE_FORMAT_16BIT_NORM;
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor { required_features: features, ..Default::default() })).ok()
    }

    /// Read a plane texture back through a buffer: `(bytes per row, rows, bytes)`.
    fn read_back(device: &wgpu::Device, queue: &wgpu::Queue, tex: &wgpu::Texture, bytes_per_texel: u32) -> Vec<u8> {
        let (w, h) = (tex.width(), tex.height());
        let row = (w * bytes_per_texel).next_multiple_of(256);
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(row * h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            tex.as_image_copy(),
            wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(h) } },
            tex.size(),
        );
        queue.submit([enc.finish()]);
        buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        let data = buf.slice(..).get_mapped_range().unwrap();
        let mut out = Vec::new();
        for y in 0..h as usize {
            out.extend_from_slice(&data[y * row as usize..y * row as usize + (w * bytes_per_texel) as usize]);
        }
        out
    }

    /// A picture written through Direct3D 11 reads back identically through wgpu's DX12 planes.
    #[test]
    fn shared_surface_reaches_wgpu_unchanged() {
        let Some((device, queue)) = dx12() else {
            eprintln!("SKIPPED: no wgpu DX12 device");
            return;
        };
        let (Ok(api), true) = (mft::api(), true) else { return };
        let gpu = Gpu::shared(api).unwrap();
        if !same_adapter(&device, &gpu) {
            eprintln!("SKIPPED: wgpu and the decoder run on different adapters");
            return;
        }
        for (format, bps) in [(SurfaceFormat::Nv12, 1usize), (SurfaceFormat::P010, 2)] {
            let (w, h) = (64usize, 48usize);
            let surface = Arc::new(SharedSurface::create(&gpu, format, (w as u32, h as u32)).unwrap());
            let luma: Vec<u8> = (0..w * h * bps).map(|i| (i * 7 + 3) as u8).collect();
            let chroma: Vec<u8> = (0..w * h / 2 * bps).map(|i| (i * 13 + 5) as u8).collect();
            let mut all = luma.clone();
            all.extend_from_slice(&chroma);
            // SAFETY: `all` holds the two planes at a row pitch of `w * bps` bytes, as the
            // descriptor of the NV12 / P010 texture says; the context is used under no other thread.
            unsafe { gpu.context().UpdateSubresource(surface.texture(), 0, None, all.as_ptr().cast(), (w * bps) as u32, 0) };
            gpu.wait_for_gpu().unwrap();
            let planes = import(&device, &surface).unwrap();
            let y = read_back(&device, &queue, &planes.luma, bps as u32);
            let uv = read_back(&device, &queue, &planes.chroma, 2 * bps as u32);
            assert!(y == luma, "{format:?} luma differs");
            assert!(uv == chroma, "{format:?} chroma differs");
        }
    }
}
