//! The encoder's input pictures in GPU memory (Windows): `R8G8B8A8_UNORM` textures made on NVENC's
//! Direct3D 11 device, shared by NT handle with the export compositor's Vulkan device
//! ([`Importer`] opens them there), which copies each finished frame into one. The
//! session then maps that texture as the picture's input (`Session::submit_mapped`), instead of the
//! frame being read back into CPU memory and copied into a host input buffer that the driver
//! uploads again.
//!
//! ```text
//! compositor (Vulkan) ──copy──► shared RGBA8 texture ──map──► NVENC (D3D11) ──► bitstream
//! ```
//!
//! A pool's textures are all made, and registered with the session, when the export asks for them
//! (`Nvenc::make_targets`); none is destroyed before the pool, which outlives the session's
//! registrations. A picture is taken by the compositor's side ([`take`]), drawn, handed to the
//! encoder, and comes back to the pool when the encoder releases it (after the bitstream of the
//! picture it went into was read) or when the frame is dropped unencoded.
//!
//! FFI module (docs/adr/0001-platform-ffi.md): every `unsafe` block has a `// SAFETY:` comment and
//! the public API is safe.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_RESOURCE_MISC_SHARED, D3D11_RESOURCE_MISC_SHARED_NTHANDLE, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, ID3D11Device, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{DXGI_SHARED_RESOURCE_READ, DXGI_SHARED_RESOURCE_WRITE, IDXGIResource1};
use windows::core::{Interface, PCWSTR};

static NEXT_POOL: AtomicU64 = AtomicU64::new(1);

/// One shared texture of a pool.
struct Slot {
    /// Its imports into the compositor's devices: one texture per device (dropped before the
    /// texture they alias).
    imports: Mutex<Vec<(wgpu::Device, wgpu::Texture)>>,
    texture: ID3D11Texture2D,
    handle: HANDLE,
}

// SAFETY: a texture of a free-threaded Direct3D 11 device and a kernel handle, neither tied to a
// thread; the import list is behind its mutex.
unsafe impl Send for Slot {}
// SAFETY: see above.
unsafe impl Sync for Slot {}

impl Drop for Slot {
    fn drop(&mut self) {
        // the compositor's views of the texture go first
        self.imports.get_mut().unwrap_or_else(PoisonError::into_inner).clear();
        // SAFETY: the handle was created for this slot and is closed once, here.
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

/// The input pictures of one encoder.
pub(crate) struct Pool {
    id: u64,
    size: (u32, u32),
    slots: Vec<Slot>,
    free: Mutex<Vec<usize>>,
    /// A compositor could not open one of them: no more are handed out (the export reads its
    /// frames back as before).
    broken: AtomicBool,
}

impl Pool {
    /// Up to `count` shareable RGBA8 textures of `size` on `device` (fewer when video memory runs
    /// out), or why not even one could be made.
    pub(crate) fn new(device: &ID3D11Device, size: (u32, u32), count: usize) -> Result<Self, String> {
        let mut slots = Vec::with_capacity(count);
        for _ in 0..count {
            match shared_texture(device, size) {
                Ok((texture, handle)) => slots.push(Slot { imports: Mutex::new(Vec::new()), texture, handle }),
                Err(e) if slots.is_empty() => return Err(e),
                Err(e) => {
                    log::info!("NVENC: {} input pictures in GPU memory instead of {count} ({e})", slots.len());
                    break;
                }
            }
        }
        Ok(Self {
            id: NEXT_POOL.fetch_add(1, Ordering::Relaxed),
            size,
            free: Mutex::new((0..slots.len()).rev().collect()),
            slots,
            broken: AtomicBool::new(false),
        })
    }

    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    pub(crate) fn len(&self) -> usize {
        self.slots.len()
    }

    /// The texture of picture `i` (what the session registers).
    pub(crate) fn texture(&self, i: usize) -> Option<&ID3D11Texture2D> {
        self.slots.get(i).map(|s| &s.texture)
    }
}

/// A free picture of `pool`, or `None` when every one is in use or the pool is retired.
pub(crate) fn take(pool: &Arc<Pool>) -> Option<Target> {
    if pool.broken.load(Ordering::Relaxed) {
        return None;
    }
    let index = pool.free.lock().unwrap_or_else(PoisonError::into_inner).pop()?;
    Some(Target { pool: pool.clone(), index })
}

/// A picture taken from a pool; it goes back when dropped.
pub struct Target {
    pool: Arc<Pool>,
    index: usize,
}

impl Drop for Target {
    fn drop(&mut self) {
        self.pool.free.lock().unwrap_or_else(PoisonError::into_inner).push(self.index);
    }
}

impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Target({}x{}, pool {}, picture {})", self.pool.size.0, self.pool.size.1, self.pool.id, self.index)
    }
}

impl filmcraft_frame::GpuTarget for Target {
    fn size(&self) -> (u32, u32) {
        self.pool.size
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl Target {
    /// The pool it belongs to ([`Pool::id`]).
    pub(crate) fn pool_id(&self) -> u64 {
        self.pool.id
    }

    /// Its index in the pool (the session's key for it).
    pub(crate) fn index(&self) -> usize {
        self.index
    }

    /// The texture as a texture of `device`, imported once per device by `import` (given its NT
    /// handle and size) and kept. A failed import retires the pool: the export reads the rest of
    /// its frames back, as before.
    pub(crate) fn texture_on(&self, device: &wgpu::Device, import: impl FnOnce(HANDLE, (u32, u32)) -> Option<wgpu::Texture>) -> Option<wgpu::Texture> {
        let slot = self.pool.slots.get(self.index)?;
        let mut imports = slot.imports.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((_, t)) = imports.iter().find(|(d, _)| d == device) {
            return Some(t.clone());
        }
        match import(slot.handle, self.pool.size) {
            Some(t) => {
                imports.push((device.clone(), t.clone()));
                Some(t)
            }
            None => {
                if !self.pool.broken.swap(true, Ordering::Relaxed) {
                    log::warn!("the encoder's input pictures in GPU memory could not be opened by the compositor: frames are read back instead");
                }
                None
            }
        }
    }
}

/// A `size` RGBA8 texture NVENC can take as an input picture and other devices open by NT handle.
fn shared_texture(device: &ID3D11Device, (w, h): (u32, u32)) -> Result<(ID3D11Texture2D, HANDLE), String> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        // NVENC's Direct3D 11 input pictures are render targets
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: (D3D11_RESOURCE_MISC_SHARED.0 | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0) as u32,
    };
    let mut tex = None;
    // SAFETY: a valid descriptor and out-pointer on a live device.
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex)) }.map_err(|e| format!("no shared texture: {e}"))?;
    let tex = tex.ok_or("no shared texture")?;
    let res: IDXGIResource1 = tex.cast().map_err(|e| format!("not a DXGI resource: {e}"))?;
    // SAFETY: a plain COM call on a live shareable texture; the returned handle is owned by the slot.
    let handle = unsafe { res.CreateSharedHandle(None, DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0, PCWSTR::null()) }
        .map_err(|e| format!("no NT handle: {e}"))?;
    Ok((tex, handle))
}

/// Opens the input pictures of every pool on the export compositor's Vulkan device
/// ([`filmcraft_gpu::TargetImporter`], registered by the app).
#[derive(Default)]
pub struct Importer;

impl filmcraft_gpu::TargetImporter for Importer {
    fn features(&self) -> wgpu::Features {
        wgpu::Features::VULKAN_EXTERNAL_MEMORY_WIN32
    }

    fn import(&self, device: &wgpu::Device, target: &dyn filmcraft_frame::GpuTarget) -> Option<wgpu::Texture> {
        let t = target.as_any().downcast_ref::<Target>()?;
        if !device.features().contains(wgpu::Features::VULKAN_EXTERNAL_MEMORY_WIN32) {
            return None;
        }
        t.texture_on(device, |handle, size| import_texture(device, handle, size))
    }
}

/// The RGBA8 texture behind `handle` (`size`) as a texture of `device` the compositor copies into,
/// or `None` when the device is not Vulkan or refuses it.
fn import_texture(device: &wgpu::Device, handle: HANDLE, (w, h): (u32, u32)) -> Option<wgpu::Texture> {
    let size = wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 };
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let hal_desc = wgpu::hal::TextureDescriptor {
        label: Some("encoder input picture"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUses::COPY_DST,
        memory_flags: wgpu::hal::MemoryFlags::empty(),
        view_formats: vec![],
    };
    // SAFETY: `handle` is a live NT handle to a `w` × `h` RGBA8 texture, as `hal_desc` says, kept
    // open by its slot, which drops every import of it before closing the handle; the device was
    // created with external memory support.
    let hal_texture = unsafe {
        let hal = device.as_hal::<wgpu::hal::api::Vulkan>()?;
        match hal.texture_from_d3d11_shared_handle(handle, &hal_desc) {
            Ok(t) => t,
            Err(e) => {
                log::warn!("an encoder input picture could not be imported into Vulkan: {e}");
                return None;
            }
        }
    };
    let desc = wgpu::TextureDescriptor {
        label: Some("encoder input picture"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    };
    // SAFETY: `hal_texture` was made on this device from `hal_desc`, which `desc` matches; its
    // contents are overwritten whole by every copy into it, so it starts UNINITIALIZED.
    Some(unsafe { device.create_texture_from_hal::<wgpu::hal::api::Vulkan>(hal_texture, &desc, wgpu::TextureUses::UNINITIALIZED) })
}
