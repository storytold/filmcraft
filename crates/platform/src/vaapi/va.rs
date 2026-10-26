//! libva itself (Linux): `libva.so.2` and `libva-drm.so.2` are loaded at run time (a system without
//! them simply has no hardware decoder), a display is opened on a DRM render node, and a [`Session`]
//! (decode configuration, context and surfaces for one stream) decodes pictures and reads them back
//! as NV12 images; an [`EncodeSession`] (encode configuration, context, surfaces and coded buffer)
//! encodes pictures uploaded as NV12.
//!
//! FFI module (docs/adr/0001-platform-ffi.md): every `unsafe` block has a `// SAFETY:` comment, the
//! public API is safe, and the one callback into Rust (libva's error messages) runs under
//! `catch_unwind`.

use std::ffi::{CStr, c_char, c_int, c_uint, c_void};
use std::fs::{File, OpenOptions};
use std::marker::PhantomData;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use filmcraft_frame::VideoFrame;
use libloading::Library;

use super::ffi::{self, VAConfigAttrib, VAImage, VAImageFormat, VASurfaceID};
use super::nv12;
use super::{Accel, DrmNodeId};
use crate::biplanar::{self, Biplanar, Geometry};

/// The libva entry points (the libraries stay loaded for the life of the process).
struct Api {
    _va: Library,
    _drm: Library,
    initialize: ffi::vaInitialize,
    terminate: ffi::vaTerminate,
    error_str: ffi::vaErrorStr,
    vendor_string: ffi::vaQueryVendorString,
    set_info_callback: ffi::vaSetInfoCallback,
    set_error_callback: ffi::vaSetErrorCallback,
    get_config_attributes: ffi::vaGetConfigAttributes,
    query_surface_attributes: Option<ffi::vaQuerySurfaceAttributes>,
    max_profiles: ffi::vaMaxNumProfiles,
    query_profiles: ffi::vaQueryConfigProfiles,
    max_entrypoints: ffi::vaMaxNumEntrypoints,
    query_entrypoints: ffi::vaQueryConfigEntrypoints,
    create_config: ffi::vaCreateConfig,
    destroy_config: ffi::vaDestroyConfig,
    create_surfaces: ffi::vaCreateSurfaces,
    destroy_surfaces: ffi::vaDestroySurfaces,
    create_context: ffi::vaCreateContext,
    destroy_context: ffi::vaDestroyContext,
    create_buffer: ffi::vaCreateBuffer,
    destroy_buffer: ffi::vaDestroyBuffer,
    begin_picture: ffi::vaBeginPicture,
    render_picture: ffi::vaRenderPicture,
    end_picture: ffi::vaEndPicture,
    sync_surface: ffi::vaSyncSurface,
    sync_surface2: Option<ffi::vaSyncSurface2>,
    export_surface_handle: Option<ffi::vaExportSurfaceHandle>,
    create_image: ffi::vaCreateImage,
    destroy_image: ffi::vaDestroyImage,
    get_image: ffi::vaGetImage,
    put_image: ffi::vaPutImage,
    map_buffer: ffi::vaMapBuffer,
    unmap_buffer: ffi::vaUnmapBuffer,
    get_display_drm: ffi::vaGetDisplayDRM,
}

static API: OnceLock<Result<Api, String>> = OnceLock::new();

fn api() -> Result<&'static Api, String> {
    API.get_or_init(load).as_ref().map_err(Clone::clone)
}

fn load() -> Result<Api, String> {
    // SAFETY: loading libva runs its library initialisers, which only set up libva's own state;
    // it is the system's VA-API runtime, loaded the way every VA-API client loads it.
    let va = unsafe { Library::new("libva.so.2") }.map_err(|e| format!("libva.so.2: {e}"))?;
    // SAFETY: as above, for libva's DRM display library.
    let drm = unsafe { Library::new("libva-drm.so.2") }.map_err(|e| format!("libva-drm.so.2: {e}"))?;
    macro_rules! sym {
        ($lib:expr, $name:ident) => {{
            // SAFETY: `ffi::$name` is the C signature of the libva function of that name,
            // transcribed from va.h and checked against it (abi_tests.rs). The pointer is copied
            // out; the library it points into is kept loaded in `Api` for the life of the process.
            let s = unsafe { $lib.get::<ffi::$name>(concat!(stringify!($name), "\0").as_bytes()) };
            *s.map_err(|e| format!("{}: {e}", stringify!($name)))?
        }};
    }
    // SAFETY: this optional symbol has the public va.h signature; the owning library remains loaded.
    let sync_surface2 = unsafe { va.get::<ffi::vaSyncSurface2>(b"vaSyncSurface2\0") }.ok().map(|symbol| *symbol);
    // SAFETY: this optional symbol has the public va.h signature; the owning library remains loaded.
    let export_surface_handle = unsafe { va.get::<ffi::vaExportSurfaceHandle>(b"vaExportSurfaceHandle\0") }.ok().map(|symbol| *symbol);
    // SAFETY: this optional symbol has the public va.h signature; the owning library remains loaded.
    let query_surface_attributes = unsafe { va.get::<ffi::vaQuerySurfaceAttributes>(b"vaQuerySurfaceAttributes\0") }.ok().map(|symbol| *symbol);
    Ok(Api {
        initialize: sym!(va, vaInitialize),
        terminate: sym!(va, vaTerminate),
        error_str: sym!(va, vaErrorStr),
        vendor_string: sym!(va, vaQueryVendorString),
        set_info_callback: sym!(va, vaSetInfoCallback),
        set_error_callback: sym!(va, vaSetErrorCallback),
        get_config_attributes: sym!(va, vaGetConfigAttributes),
        query_surface_attributes,
        max_profiles: sym!(va, vaMaxNumProfiles),
        query_profiles: sym!(va, vaQueryConfigProfiles),
        max_entrypoints: sym!(va, vaMaxNumEntrypoints),
        query_entrypoints: sym!(va, vaQueryConfigEntrypoints),
        create_config: sym!(va, vaCreateConfig),
        destroy_config: sym!(va, vaDestroyConfig),
        create_surfaces: sym!(va, vaCreateSurfaces),
        destroy_surfaces: sym!(va, vaDestroySurfaces),
        create_context: sym!(va, vaCreateContext),
        destroy_context: sym!(va, vaDestroyContext),
        create_buffer: sym!(va, vaCreateBuffer),
        destroy_buffer: sym!(va, vaDestroyBuffer),
        begin_picture: sym!(va, vaBeginPicture),
        render_picture: sym!(va, vaRenderPicture),
        end_picture: sym!(va, vaEndPicture),
        sync_surface: sym!(va, vaSyncSurface),
        sync_surface2,
        export_surface_handle,
        create_image: sym!(va, vaCreateImage),
        destroy_image: sym!(va, vaDestroyImage),
        get_image: sym!(va, vaGetImage),
        put_image: sym!(va, vaPutImage),
        map_buffer: sym!(va, vaMapBuffer),
        unmap_buffer: sym!(va, vaUnmapBuffer),
        get_display_drm: sym!(drm, vaGetDisplayDRM),
        _va: va,
        _drm: drm,
    })
}

/// libva's error messages, into our log (instead of stderr).
extern "C" fn on_error(_user: *mut c_void, message: *const c_char) {
    let _ = std::panic::catch_unwind(|| {
        if message.is_null() {
            return;
        }
        // SAFETY: libva passes a NUL-terminated message that is valid for the duration of the call.
        let text = unsafe { CStr::from_ptr(message) }.to_string_lossy();
        log::warn!("libva: {}", text.trim_end());
    });
}

/// An initialised VA display on a DRM render node.
pub struct Display {
    api: &'static Api,
    dpy: ffi::VADisplay,
    vendor: String,
    /// The render node; open for as long as the display uses it.
    _device: File,
}

// SAFETY: a VA display is not tied to the thread that opened it.
unsafe impl Send for Display {}
// SAFETY: libva's functions are thread-safe and so must its drivers' be (va.h, "Multithreading
// Guide"): one display may be used from several threads at once, as long as each VA object
// (context, surfaces, buffers, images) is used by one thread at a time. Every `Session` on a
// shared display has its own configuration, context, surfaces and image, used only through that
// session (`&mut` or `&` of one owner); the display itself is only read (`api`, `dpy`, `vendor`).
unsafe impl Sync for Display {}

/// The display every [`Session`] decodes on. Opening a display takes a few milliseconds with
/// Intel's and Mesa's drivers but 0.2-0.3 s with NVIDIA's (a CUDA context each, about 100 MB of
/// video memory), and terminating one 60-70 ms, on the thread that creates or drops the decoder: a
/// frame worker at every clip on the timeline, the UI thread when media is removed. So one display
/// is opened (by [`probe`] at startup) and kept for the life of the process, like the libraries,
/// and sessions only make and destroy their own configuration, context and surfaces.
static SHARED: Mutex<Option<Arc<Display>>> = Mutex::new(None);
/// Displays opened so far (diagnostics and tests).
static OPENED: AtomicUsize = AtomicUsize::new(0);

/// The number of VA displays this process has opened (one, unless the shared one stopped working).
pub fn displays_opened() -> usize {
    OPENED.load(Ordering::Relaxed)
}

/// The shared display, opened on first use.
fn shared() -> Result<Arc<Display>, String> {
    let mut shared = SHARED.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(d) = shared.as_ref() {
        return Ok(d.clone());
    }
    let d = Arc::new(Display::open()?);
    *shared = Some(d.clone());
    Ok(d)
}

/// Share `new` from now on instead of `old` (if `old` is still the shared one). Sessions on `old`
/// keep it until they end.
fn replace_shared(old: &Arc<Display>, new: Arc<Display>) {
    let mut shared = SHARED.lock().unwrap_or_else(PoisonError::into_inner);
    if shared.as_ref().is_none_or(|d| Arc::ptr_eq(d, old)) {
        *shared = Some(new);
    }
}

impl Display {
    /// The first DRM render node with a working VA-API driver.
    pub fn open() -> Result<Self, String> {
        let api = api()?;
        let mut init_failure = None;
        let mut denied = None;
        for n in 128..144 {
            let path = format!("/dev/dri/renderD{n}");
            match OpenOptions::new().read(true).write(true).open(&path) {
                Ok(file) => match Self::init(api, file) {
                    Ok(d) => return Ok(d),
                    Err(e) => init_failure = Some(format!("{path}: {e}")),
                },
                Err(e) => denied = denied.or_else(|| render_node_denied(&path, e.kind())),
            }
        }
        Err(no_display_reason(denied, init_failure))
    }

    /// Open exactly the supplied DRM character device, without the shared display singleton.
    fn open_node(expected: DrmNodeId) -> Result<Self, String> {
        let sysfs = format!("/sys/dev/char/{}:{}", expected.major, expected.minor);
        let resolved = std::fs::canonicalize(&sysfs).map_err(|e| format!("{sysfs}: {e}"))?;
        let name = resolved.file_name().and_then(|s| s.to_str()).ok_or("missing DRM render node name")?;
        validate_render_node(expected, expected, true, name)?;
        let path = format!("/dev/dri/{name}");
        let device = OpenOptions::new().read(true).write(true).open(&path).map_err(|e| format!("{path}: {e}"))?;
        let metadata = device.metadata().map_err(|e| format!("{path}: {e}"))?;
        use std::os::unix::fs::FileTypeExt;
        validate_render_node(expected, drm_node_from_rdev(metadata.rdev()), metadata.file_type().is_char_device(), name)?;
        Self::init(api()?, device)
    }

    fn init(api: &'static Api, device: File) -> Result<Self, String> {
        // SAFETY: the descriptor is an open render node, kept open by `device` (moved into the
        // returned display) for as long as the display exists.
        let dpy = unsafe { (api.get_display_drm)(device.as_raw_fd()) };
        if dpy.is_null() {
            return Err("vaGetDisplayDRM failed".into());
        }
        // From here on `Drop` terminates the display.
        let mut d = Self { api, dpy, vendor: String::new(), _device: device };
        // SAFETY: `dpy` is a display from vaGetDisplayDRM; a null info callback turns libva's info
        // messages off, and `on_error` matches `VAMessageCallback` and never unwinds.
        unsafe {
            (api.set_info_callback)(dpy, None, std::ptr::null_mut());
            (api.set_error_callback)(dpy, Some(on_error), std::ptr::null_mut());
        }
        let (mut major, mut minor): (c_int, c_int) = (0, 0);
        // SAFETY: `dpy` is valid; the version outputs point to live locals.
        let st = unsafe { (api.initialize)(dpy, &mut major, &mut minor) };
        d.check(st, "vaInitialize")?;
        OPENED.fetch_add(1, Ordering::Relaxed);
        // SAFETY: `dpy` is an initialised display; the vendor string is NUL-terminated and owned by
        // the driver for the life of the display (copied out here).
        let vendor = unsafe { (api.vendor_string)(dpy) };
        if !vendor.is_null() {
            // SAFETY: non-null, NUL-terminated (see above).
            d.vendor = unsafe { CStr::from_ptr(vendor) }.to_string_lossy().into_owned();
        }
        Ok(d)
    }

    /// The driver's description ("Intel iHD driver …").
    pub fn vendor(&self) -> &str {
        &self.vendor
    }

    pub fn drm_node_id(&self) -> Result<DrmNodeId, String> {
        let rdev = self._device.metadata().map_err(|e| format!("DRM render node metadata: {e}"))?.rdev();
        Ok(drm_node_from_rdev(rdev))
    }

    fn check(&self, st: ffi::VAStatus, what: &str) -> Result<(), String> {
        if st == ffi::VA_STATUS_SUCCESS {
            return Ok(());
        }
        // SAFETY: vaErrorStr takes any status and returns a static NUL-terminated string.
        let text = unsafe { (self.api.error_str)(st) };
        let text = if text.is_null() {
            String::new()
        } else {
            // SAFETY: non-null static NUL-terminated string (see above).
            unsafe { CStr::from_ptr(text) }.to_string_lossy().into_owned()
        };
        Err(format!("{what}: {text} ({st:#x})"))
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        // SAFETY: `dpy` came from vaGetDisplayDRM and is terminated exactly once, here, after
        // every session object made on it was destroyed (`Session` drops its objects first).
        unsafe { (self.api.terminate)(self.dpy) };
    }
}

/// Shared destructor for VA allocations. Exported leases retain the whole session, not only FDs.
struct VaResourceOwner {
    release: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl Drop for VaResourceOwner {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
    }
}

fn retain_resources(
    display: Arc<Display>,
    config: ffi::VAConfigID,
    context: ffi::VAContextID,
    surfaces: Vec<VASurfaceID>,
    coded: ffi::VABufferID,
    poisoned: Option<Arc<AtomicBool>>,
) -> Arc<VaResourceOwner> {
    Arc::new(VaResourceOwner {
        release: Some(Box::new(move || {
            let Some((display, mut surfaces)) = retain_if_quarantined(poisoned.as_ref().is_some_and(|p| p.load(Ordering::Acquire)), (display, surfaces)) else {
                log::warn!("VA encode session released while quarantined: keeping its context, surfaces and coded buffer");
                return;
            };
            let (api, dpy) = (display.api, display.dpy);
            // SAFETY: this is the sole destructor of these IDs; all exported leases have retired.
            // The captured display remains live through destruction of every child allocation.
            unsafe {
                if coded != ffi::VA_INVALID_ID {
                    (api.destroy_buffer)(dpy, coded);
                }
                if context != ffi::VA_INVALID_ID {
                    (api.destroy_context)(dpy, context);
                }
                if !surfaces.is_empty()
                    && let Ok(count) = c_int::try_from(surfaces.len())
                {
                    (api.destroy_surfaces)(dpy, surfaces.as_mut_ptr(), count);
                }
                if config != ffi::VA_INVALID_ID {
                    (api.destroy_config)(dpy, config);
                }
            }
        })),
    })
}

/// One stream's decoder: a VLD configuration of a profile, its surfaces and context, and the image
/// pictures are read back through.
pub struct Session {
    resources: Option<Arc<VaResourceOwner>>,
    /// The shared display (see [`SHARED`]), or the one that replaced it.
    display: Arc<Display>,
    config: ffi::VAConfigID,
    context: ffi::VAContextID,
    surfaces: Vec<VASurfaceID>,
    surface_leases: Vec<Arc<AtomicBool>>,
    image: Option<VAImage>,
    /// Coded size of the surfaces (luma samples).
    size: (u32, u32),
    geometry: Geometry,
    strict_linear_nv12: bool,
}

/// DRM PRIME handles for one VA surface. Borrow keeps the session alive and blocks mutable reuse;
/// each exported FD closes when this value drops. Consumers must finish GPU access first.
pub struct ExportedDmaBuf<'a> {
    descriptor: ffi::VADRMPRIMESurfaceDescriptor,
    fds: [Option<OwnedFd>; ffi::VA_DRM_PRIME_MAX_OBJECTS],
    drm_node: DrmNodeId,
    _session: PhantomData<&'a ()>,
    resources: Option<Arc<VaResourceOwner>>,
    display: Option<Arc<Display>>,
    surface: VASurfaceID,
}

/// One plane of an exported surface. The borrowed FD cannot outlive its export descriptor.
pub struct ExportedPlane<'a> {
    pub fd: BorrowedFd<'a>,
    pub object_index: usize,
    pub object_size: u32,
    pub modifier: u64,
    pub layer_format: u32,
    pub offset: u32,
    pub pitch: u32,
}

/// Read-only decoded VA surface held until the exact GPU read submission and VA completion.
/// Dropping before `retire_after_gpu_submission` quarantines its owner and prevents surface reuse.
pub struct ResidentSurfaceLease {
    export: Option<ExportedDmaBuf<'static>>,
    slot: Arc<AtomicBool>,
    _permit: Option<crate::vulkan_import::TransferPermit>,
    retired: bool,
}

struct QuarantinedResidentSurface {
    _export: ExportedDmaBuf<'static>,
    _slot: Arc<AtomicBool>,
    _permit: crate::vulkan_import::TransferPermit,
}

fn resident_surface_quarantine() -> &'static Mutex<Vec<QuarantinedResidentSurface>> {
    static QUARANTINE: OnceLock<Mutex<Vec<QuarantinedResidentSurface>>> = OnceLock::new();
    QUARANTINE.get_or_init(|| Mutex::new(Vec::with_capacity(crate::vulkan_import::MAX_IN_FLIGHT_TRANSFERS)))
}

fn reserve_surface_slot(slot: &AtomicBool) -> Result<(), String> {
    slot.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map(|_| ())
        .map_err(|_| "decoded surface is already leased to an external consumer".into())
}

impl ResidentSurfaceLease {
    pub fn drm_node_id(&self) -> Option<DrmNodeId> {
        self.export.as_ref().map(|export| export.drm_node_id())
    }

    pub fn size(&self) -> Option<(u32, u32)> {
        self.export.as_ref().map(|export| export.size())
    }

    pub fn fourcc(&self) -> Option<u32> {
        self.export.as_ref().map(|export| export.fourcc())
    }

    pub fn object_count(&self) -> Option<usize> {
        self.export.as_ref().map(|export| export.object_count())
    }

    pub fn object_layout(&self, index: usize) -> Option<(u64, u64)> {
        self.export.as_ref()?.object_layout(index)
    }

    pub fn layer_count(&self) -> Option<usize> {
        self.export.as_ref().map(|export| export.layer_count())
    }

    pub fn plane_count(&self, layer: usize) -> Option<usize> {
        self.export.as_ref()?.plane_count(layer)
    }

    pub fn plane(&self, layer: usize, plane: usize) -> Option<ExportedPlane<'_>> {
        self.export.as_ref()?.plane(layer, plane)
    }

    /// Wait for the exact GPU submission that read this surface, then for bounded VA retirement.
    pub fn retire_after_gpu_submission(
        &mut self,
        device: &wgpu::Device,
        submission: wgpu::SubmissionIndex,
        timeout: std::time::Duration,
    ) -> Result<(), String> {
        if self.retired {
            return Err("decoded surface lease is already retired".into());
        }
        crate::vulkan_import::wait_for_submission(device, submission, timeout)?;
        let export = self.export.as_ref().ok_or("decoded surface lease owner is missing")?;
        let mut waiter = export.retirement_waiter()?;
        waiter(timeout)?;
        self.retired = true;
        self.slot.store(false, Ordering::Release);
        self.export.take();
        self._permit.take();
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Nv12ExportPlane {
    object_index: usize,
    object_size: u32,
    modifier: u64,
    layer_format: u32,
    offset: u32,
    pitch: u32,
}

fn verify_linear_nv12_planes(size: (u32, u32), object_size: u64, planes: [Nv12ExportPlane; 2]) -> Result<(), String> {
    let (width, height) = size;
    if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) || object_size == 0 {
        return Err("VA linear surface allocation unsupported: invalid coded NV12 dimensions or object size".into());
    }
    let expected = [(ffi::DRM_FORMAT_R8, width, height, 1u64), (ffi::DRM_FORMAT_GR88, width / 2, height / 2, 2u64)];
    let mut ends = [0u64; 2];
    for (index, (plane, (format, plane_width, plane_height, pixel_bytes))) in planes.into_iter().zip(expected).enumerate() {
        let row_bytes = u64::from(plane_width).checked_mul(pixel_bytes).ok_or("VA linear NV12 row size overflow")?;
        let offset = u64::from(plane.offset);
        let pitch = u64::from(plane.pitch);
        if plane.object_index != 0
            || u64::from(plane.object_size) != object_size
            || plane.modifier != ffi::DRM_FORMAT_MOD_LINEAR
            || plane.layer_format != format
            || pitch < row_bytes
            || pitch % pixel_bytes != 0
            || offset % pixel_bytes != 0
        {
            return Err("VA linear surface allocation unsupported: NV12 layer format, object, pitch, or alignment is invalid".into());
        }
        ends[index] = pitch
            .checked_mul(u64::from(plane_height - 1))
            .and_then(|span| offset.checked_add(span))
            .and_then(|start| start.checked_add(row_bytes))
            .ok_or("VA linear NV12 plane range overflow")?;
        if ends[index] > object_size {
            return Err("VA linear surface allocation unsupported: NV12 plane exceeds its DMA-BUF object".into());
        }
    }
    let y_offset = u64::from(planes[0].offset);
    let uv_offset = u64::from(planes[1].offset);
    if y_offset < ends[1] && uv_offset < ends[0] {
        return Err("VA linear surface allocation unsupported: NV12 planes overlap".into());
    }
    Ok(())
}

fn verify_separate_nv12_layers(layer_count: usize, plane_counts: [Option<usize>; 2]) -> Result<(), String> {
    if layer_count != 2 || plane_counts != [Some(1), Some(1)] {
        return Err("VA linear surface allocation unsupported: NV12 export is not two single-plane layers".into());
    }
    Ok(())
}

fn verify_linear_nv12_export(exported: &ExportedDmaBuf<'_>, expected_node: DrmNodeId, expected_size: (u32, u32)) -> Result<(), String> {
    if exported.drm_node_id() != expected_node || exported.fourcc() != ffi::VA_FOURCC_NV12 || exported.object_count() != 1 || exported.size() != expected_size {
        return Err("VA linear surface allocation unsupported: export is not single-object NV12 on the selected DRM node".into());
    }
    let (object_size, modifier) = exported.object_layout(0).ok_or("VA linear surface allocation unsupported: missing exported object")?;
    if object_size == 0 || modifier != ffi::DRM_FORMAT_MOD_LINEAR {
        return Err("VA linear surface allocation unsupported: exported object is not LINEAR".into());
    }
    verify_separate_nv12_layers(exported.layer_count(), [exported.plane_count(0), exported.plane_count(1)])?;
    let y = exported.plane(0, 0).ok_or("VA linear surface allocation unsupported: missing NV12 Y plane")?;
    let uv = exported.plane(1, 0).ok_or("VA linear surface allocation unsupported: missing NV12 UV plane")?;
    verify_linear_nv12_planes(
        expected_size,
        object_size,
        [
            Nv12ExportPlane {
                object_index: y.object_index,
                object_size: y.object_size,
                modifier: y.modifier,
                layer_format: y.layer_format,
                offset: y.offset,
                pitch: y.pitch,
            },
            Nv12ExportPlane {
                object_index: uv.object_index,
                object_size: uv.object_size,
                modifier: uv.modifier,
                layer_format: uv.layer_format,
                offset: uv.offset,
                pitch: uv.pitch,
            },
        ],
    )
}

impl Drop for ResidentSurfaceLease {
    fn drop(&mut self) {
        if !self.retired
            && let Some(export) = self.export.take()
        {
            let Some(permit) = self._permit.take() else {
                std::mem::forget(export);
                return;
            };
            resident_surface_quarantine().lock().unwrap_or_else(PoisonError::into_inner).push(QuarantinedResidentSurface {
                _export: export,
                _slot: self.slot.clone(),
                _permit: permit,
            });
        }
    }
}

impl ExportedDmaBuf<'_> {
    pub(crate) fn into_owned(self) -> ExportedDmaBuf<'static> {
        ExportedDmaBuf {
            descriptor: self.descriptor,
            fds: self.fds,
            drm_node: self.drm_node,
            _session: PhantomData,
            resources: self.resources,
            display: self.display,
            surface: self.surface,
        }
    }

    pub(crate) fn retirement_waiter(&self) -> Result<Box<dyn FnMut(std::time::Duration) -> Result<(), String> + Send>, String> {
        let display = self.display.clone().ok_or("VA export has no display owner")?;
        let sync = display.api.sync_surface2.ok_or("resident VA interop requires bounded vaSyncSurface2")?;
        let surface = self.surface;
        Ok(Box::new(move |timeout| {
            let nanos = u64::try_from(timeout.as_nanos()).map_err(|_| "VA retirement timeout overflow")?;
            // SAFETY: the callback retains the display; the enclosing lease retains the surface.
            let status = unsafe { sync(display.dpy, surface, nanos) };
            display.check(status, "vaSyncSurface2 (external consumer retirement)")
        }))
    }

    pub fn drm_node_id(&self) -> DrmNodeId {
        self.drm_node
    }

    pub fn fourcc(&self) -> u32 {
        self.descriptor.fourcc
    }

    pub fn size(&self) -> (u32, u32) {
        (self.descriptor.width, self.descriptor.height)
    }

    /// Object indices remain stable for the descriptor's lifetime, including shared objects.
    pub fn object_count(&self) -> usize {
        self.descriptor.num_objects as usize
    }

    pub fn object_layout(&self, index: usize) -> Option<(u64, u64)> {
        if index >= self.object_count() {
            return None;
        }
        let object = self.descriptor.objects.get(index)?;
        Some((u64::from(object.size), object.drm_format_modifier))
    }

    pub fn layer_count(&self) -> usize {
        self.descriptor.num_layers as usize
    }

    pub fn plane_count(&self, layer: usize) -> Option<usize> {
        if layer >= self.layer_count() {
            return None;
        }
        let layer = self.descriptor.layers.get(layer)?;
        usize::try_from(layer.num_planes).ok()
    }

    pub fn plane(&self, layer: usize, plane: usize) -> Option<ExportedPlane<'_>> {
        if layer >= self.layer_count() {
            return None;
        }
        let layer = self.descriptor.layers.get(layer)?;
        if plane >= usize::try_from(layer.num_planes).ok()? || plane >= ffi::VA_DRM_PRIME_MAX_PLANES {
            return None;
        }
        let object = usize::try_from(*layer.object_index.get(plane)?).ok()?;
        let metadata = self.descriptor.objects.get(object)?;
        let fd = self.fds.get(object)?.as_ref()?;
        Some(ExportedPlane {
            fd: fd.as_fd(),
            object_index: object,
            object_size: metadata.size,
            modifier: metadata.drm_format_modifier,
            layer_format: layer.drm_format,
            offset: *layer.offset.get(plane)?,
            pitch: *layer.pitch.get(plane)?,
        })
    }
}

fn bounded_export_sync(sync: Option<impl FnOnce(u64) -> ffi::VAStatus>) -> Result<(), String> {
    let status = sync.ok_or("resident VA export requires bounded vaSyncSurface2")?(5_000_000_000);
    if status != ffi::VA_STATUS_SUCCESS {
        return Err(format!("vaSyncSurface2 (DMA-BUF producer, 5 s) failed: {status}"));
    }
    Ok(())
}

fn query_surface_attributes(display: &Display, config: ffi::VAConfigID) -> Result<Vec<ffi::VASurfaceAttrib>, String> {
    let query = display.api.query_surface_attributes.ok_or("VA linear surface allocation unsupported: libva lacks vaQuerySurfaceAttributes")?;
    let mut count = 0;
    // SAFETY: config belongs to this live display; null attribute list requests required count.
    let status = unsafe { query(display.dpy, config, std::ptr::null_mut(), &mut count) };
    display.check(status, "vaQuerySurfaceAttributes (count)").map_err(|e| format!("VA linear surface allocation unsupported: {e}"))?;
    if count > 256 {
        return Err(format!("VA linear surface allocation unsupported: invalid surface attribute count {count}"));
    }
    let mut attrs = vec![ffi::VASurfaceAttrib::default(); usize::try_from(count).map_err(|_| "surface attribute count overflow")?];
    if attrs.is_empty() {
        return Ok(attrs);
    }
    // SAFETY: attrs has `count` writable elements; count is a live in/out value.
    let status = unsafe { query(display.dpy, config, attrs.as_mut_ptr(), &mut count) };
    display.check(status, "vaQuerySurfaceAttributes").map_err(|e| format!("VA linear surface allocation unsupported: {e}"))?;
    let returned = usize::try_from(count).map_err(|_| "surface attribute count overflow")?;
    if returned > attrs.len() {
        return Err(format!("VA linear surface allocation unsupported: surface attribute count grew to {count}"));
    }
    attrs.truncate(returned);
    Ok(attrs)
}

fn verify_modifier_attribute(display: &Display, config: ffi::VAConfigID) -> Result<(), String> {
    let attrs = query_surface_attributes(display, config)?;
    verify_modifier_attributes(&attrs)
}

fn verify_modifier_attributes(attrs: &[ffi::VASurfaceAttrib]) -> Result<(), String> {
    if !attrs.iter().any(|attr| attr.type_ == ffi::VA_SURFACE_ATTRIB_DRM_FORMAT_MODIFIERS && attr.flags & ffi::VA_SURFACE_ATTRIB_SETTABLE != 0) {
        return Err("VA linear surface allocation unsupported: DRM modifier attribute is not settable".into());
    }
    Ok(())
}

fn verify_nv12_surface_attribute(attrs: &[ffi::VASurfaceAttrib]) -> Result<(), String> {
    let nv12_settable = attrs.iter().any(|attr| {
        if attr.type_ != ffi::VA_SURFACE_ATTRIB_PIXEL_FORMAT
            || attr.flags & ffi::VA_SURFACE_ATTRIB_SETTABLE == 0
            || attr.value.type_ != ffi::VA_GENERIC_VALUE_TYPE_INTEGER
        {
            return false;
        }
        // SAFETY: libva reports VASurfaceAttribPixelFormat as an integer VAGenericValue.
        (unsafe { attr.value.value.integer }) == ffi::VA_FOURCC_NV12 as i32
    });
    if !nv12_settable {
        return Err("VA linear surface allocation unsupported: NV12 pixel-format attribute is not settable".into());
    }
    Ok(())
}

fn export_surface_impl<'a>(display: &'a Arc<Display>, surface: VASurfaceID, flags: u32) -> Result<ExportedDmaBuf<'a>, String> {
    let export = display.api.export_surface_handle.ok_or("libva does not expose vaExportSurfaceHandle")?;
    bounded_export_sync(display.api.sync_surface2.map(|sync| {
        move |timeout| {
            // SAFETY: surface belongs to this live display; bounded sync completes VA producer access.
            unsafe { sync(display.dpy, surface, timeout) }
        }
    }))?;

    let mut descriptor = ffi::VADRMPRIMESurfaceDescriptor::default();
    for object in &mut descriptor.objects {
        object.fd = -1;
    }
    // SAFETY: descriptor has exact va_drmcommon.h layout and stays writable; libva transfers returned FDs.
    let status = unsafe {
        export(display.dpy, surface, ffi::VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2, flags, (&mut descriptor as *mut ffi::VADRMPRIMESurfaceDescriptor).cast())
    };

    let mut fds: [Option<OwnedFd>; ffi::VA_DRM_PRIME_MAX_OBJECTS] = std::array::from_fn(|_| None);
    let mut duplicate_fd = false;
    for (index, object) in descriptor.objects.iter_mut().enumerate() {
        let raw = std::mem::replace(&mut object.fd, -1);
        if raw < 0 {
            continue;
        }
        if fds[..index].iter().flatten().any(|fd| fd.as_raw_fd() == raw) {
            duplicate_fd = true;
        } else {
            // SAFETY: successful libva export transfers each unique DRM PRIME descriptor to the caller.
            fds[index] = Some(unsafe { OwnedFd::from_raw_fd(raw) });
        }
    }
    display.check(status, "vaExportSurfaceHandle")?;
    if duplicate_fd {
        return Err("VA export returned duplicate object file descriptors".into());
    }
    let objects = usize::try_from(descriptor.num_objects).ok().filter(|n| (1..=ffi::VA_DRM_PRIME_MAX_OBJECTS).contains(n));
    let layers = usize::try_from(descriptor.num_layers).ok().filter(|n| (1..=ffi::VA_DRM_PRIME_MAX_LAYERS).contains(n));
    if descriptor.width == 0 || descriptor.height == 0 || objects.is_none() || layers.is_none() {
        return Err("VA export returned invalid dimensions or object/layer counts".into());
    }
    let (Some(objects), Some(layers)) = (objects, layers) else {
        return Err("VA export returned invalid object/layer counts".into());
    };
    for object in 0..objects {
        if fds[object].is_none() || descriptor.objects[object].size == 0 {
            return Err("VA export returned invalid object metadata".into());
        }
    }
    for layer in descriptor.layers.iter().take(layers) {
        let planes = usize::try_from(layer.num_planes).ok().filter(|n| (1..=ffi::VA_DRM_PRIME_MAX_PLANES).contains(n));
        let Some(planes) = planes else {
            return Err("VA export returned invalid plane count".into());
        };
        for plane in 0..planes {
            let object = usize::try_from(layer.object_index[plane]).map_err(|_| "VA export object index overflow")?;
            if object >= objects || layer.pitch[plane] == 0 || layer.offset[plane] >= descriptor.objects[object].size {
                return Err("VA export returned invalid plane metadata".into());
            }
        }
    }
    Ok(ExportedDmaBuf { descriptor, fds, drm_node: display.drm_node_id()?, _session: PhantomData, resources: None, display: Some(display.clone()), surface })
}

/// The surface and image formats of 8-bit (NV12) or 10-bit (P010) 4:2:0 pictures:
/// (render-target format, image fourcc, bits per pixel of the image).
fn formats(bits: u32) -> Result<(c_uint, u32, u32), String> {
    match bits {
        8 => Ok((ffi::VA_RT_FORMAT_YUV420, ffi::VA_FOURCC_NV12, 12)),
        10 => Ok((ffi::VA_RT_FORMAT_YUV420_10, ffi::VA_FOURCC_P010, 24)),
        _ => Err(format!("{bits}-bit pictures are not decoded through VA-API")),
    }
}

impl Session {
    /// A decoder for the first of `profiles` the driver decodes (VLD, 4:2:0 of `geometry.bits`:
    /// 8 or 10), with `count` surfaces of `size` (coded luma samples); pictures read back are cut
    /// to `geometry`.
    pub fn new(profiles: &[ffi::VAProfile], size: (u32, u32), count: usize, geometry: Geometry) -> Result<Self, String> {
        let display = shared()?;
        match Self::on(display.clone(), profiles, size, count, geometry, false) {
            Ok(s) => Ok(s),
            Err(first) => {
                // The shared display may have stopped working (a GPU reset): try a new one, and
                // share that one from now on if it works. (A stream the driver does not take
                // fails on both and costs the one extra display opening.)
                let fresh = Arc::new(Display::open().map_err(|e| format!("{first}; a new display: {e}"))?);
                let s = Self::on(fresh.clone(), profiles, size, count, geometry, false).map_err(|_| first)?;
                replace_shared(&display, fresh);
                Ok(s)
            }
        }
    }

    /// Strict decode session for one DRM node; only explicitly queried/requested NV12 LINEAR surfaces are accepted.
    pub(crate) fn new_linear_diagnostic(
        node: DrmNodeId,
        profiles: &[ffi::VAProfile],
        size: (u32, u32),
        count: usize,
        geometry: Geometry,
    ) -> Result<Self, String> {
        if geometry.bits != 8 || size.0 == 0 || size.1 == 0 || size.0 > 16_384 || size.1 > 16_384 || count == 0 || count > 18 {
            return Err("strict VA decode requires 8-bit NV12, nonzero dimensions up to 16384, and 1..=18 surfaces".into());
        }
        Self::on(Arc::new(Display::open_node(node)?), profiles, size, count, geometry, true)
    }

    fn on(display: Arc<Display>, profiles: &[ffi::VAProfile], size: (u32, u32), count: usize, geometry: Geometry, linear_nv12: bool) -> Result<Self, String> {
        let (rt_format, _, _) = formats(geometry.bits)?;
        let api = display.api;
        let dpy = display.dpy;
        let mut config = None;
        let mut why = "no VA-API profile for the stream".to_string();
        for &profile in profiles {
            let mut attr = VAConfigAttrib { type_: ffi::VAConfigAttribRTFormat, value: 0 };
            // SAFETY: `dpy` is initialised; `attr` is one live attribute (count 1).
            let st = unsafe { (api.get_config_attributes)(dpy, profile, ffi::VAEntrypointVLD, &mut attr, 1) };
            if let Err(e) = display.check(st, "vaGetConfigAttributes") {
                why = e;
                continue;
            }
            if attr.value & rt_format == 0 {
                why = format!("profile {profile} does not decode to {}-bit 4:2:0", geometry.bits);
                continue;
            }
            attr.value = rt_format;
            let mut id = ffi::VA_INVALID_ID;
            // SAFETY: as above; `id` is a live output.
            let st = unsafe { (api.create_config)(dpy, profile, ffi::VAEntrypointVLD, &mut attr, 1, &mut id) };
            match display.check(st, "vaCreateConfig") {
                Ok(()) => {
                    config = Some(id);
                    break;
                }
                Err(e) => why = e,
            }
        }
        let config = config.ok_or(why)?;
        // From here on `Drop` releases what was made.
        let mut s = Self {
            resources: None,
            display,
            config,
            context: ffi::VA_INVALID_ID,
            surfaces: Vec::new(),
            surface_leases: (0..count).map(|_| Arc::new(AtomicBool::new(false))).collect(),
            image: None,
            size,
            geometry,
            strict_linear_nv12: linear_nv12,
        };
        let count_c = c_uint::try_from(count).map_err(|_| "too many surfaces")?;
        let mut surfaces = vec![ffi::VA_INVALID_SURFACE; count];
        let mut modifiers = Vec::new();
        let mut modifier_lists = Vec::new();
        let mut surface_attribs = Vec::new();
        if linear_nv12 {
            let attrs = query_surface_attributes(&s.display, s.config)?;
            verify_modifier_attributes(&attrs)?;
            verify_nv12_surface_attribute(&attrs)?;
            modifiers.push(ffi::DRM_FORMAT_MOD_LINEAR);
            let modifier_count = u32::try_from(modifiers.len()).map_err(|_| "too many DRM modifiers")?;
            modifier_lists.extend((0..count).map(|_| ffi::VADRMFormatModifierList { num_modifiers: modifier_count, modifiers: modifiers.as_mut_ptr() }));
            surface_attribs.push(ffi::VASurfaceAttrib {
                type_: ffi::VA_SURFACE_ATTRIB_PIXEL_FORMAT,
                flags: ffi::VA_SURFACE_ATTRIB_SETTABLE,
                value: ffi::VAGenericValue {
                    type_: ffi::VA_GENERIC_VALUE_TYPE_INTEGER,
                    value: ffi::VAGenericValueData { integer: ffi::VA_FOURCC_NV12 as i32 },
                },
            });
            surface_attribs.push(ffi::VASurfaceAttrib {
                type_: ffi::VA_SURFACE_ATTRIB_DRM_FORMAT_MODIFIERS,
                flags: ffi::VA_SURFACE_ATTRIB_SETTABLE,
                value: ffi::VAGenericValue {
                    type_: ffi::VA_GENERIC_VALUE_TYPE_POINTER,
                    value: ffi::VAGenericValueData { pointer: modifier_lists.as_mut_ptr().cast() },
                },
            });
        }
        let attrib_count = u32::try_from(surface_attribs.len()).map_err(|_| "too many VA surface attributes")?;
        // SAFETY: `surfaces` has room for `count` IDs; modifier storage and attribute pointers stay
        // live for this call. Legacy sessions pass the original null attribute pointer and count 0.
        let st = unsafe {
            (api.create_surfaces)(
                dpy,
                rt_format,
                size.0,
                size.1,
                surfaces.as_mut_ptr(),
                count_c,
                if surface_attribs.is_empty() { std::ptr::null_mut() } else { surface_attribs.as_mut_ptr() },
                attrib_count,
            )
        };
        s.display
            .check(st, if linear_nv12 { "vaCreateSurfaces (NV12 LINEAR)" } else { "vaCreateSurfaces" })
            .map_err(|e| if linear_nv12 { format!("VA linear surface allocation unsupported: {e}") } else { e })?;
        s.surfaces = surfaces;
        let (w, h) = (c_int::try_from(size.0).map_err(|_| "picture too wide")?, c_int::try_from(size.1).map_err(|_| "picture too tall")?);
        let n = c_int::try_from(count).map_err(|_| "too many surfaces")?;
        let mut context = ffi::VA_INVALID_ID;
        // SAFETY: `config` and the `count` surfaces were made on this display; `context` is a live output.
        let st = unsafe { (api.create_context)(dpy, s.config, w, h, ffi::VA_PROGRESSIVE, s.surfaces.as_mut_ptr(), n, &mut context) };
        s.display.check(st, "vaCreateContext")?;
        s.context = context;
        s.resources = Some(retain_resources(s.display.clone(), s.config, s.context, s.surfaces.clone(), ffi::VA_INVALID_ID, None));
        Ok(s)
    }

    fn ensure_surface_available(&self, index: usize) -> Result<(), String> {
        if self.surface_leases.get(index).ok_or("surface out of range")?.load(Ordering::Acquire) {
            Err("decoded surface is leased to an external consumer".into())
        } else {
            Ok(())
        }
    }

    pub fn vendor(&self) -> &str {
        self.display.vendor()
    }

    /// Physical render-node identity from the same open DRM file used by this VA display.
    pub fn drm_node_id(&self) -> Result<DrmNodeId, String> {
        self.display.drm_node_id()
    }

    /// Export one decoded surface for read-only external GPU use. bounded `vaSyncSurface2` completes VA
    /// writes first; this API creates no cross-API fence, so callers must wait for their reads
    /// before allowing this surface to be reused. The borrowed result keeps this session alive.
    pub fn export_surface(&self, index: usize) -> Result<ExportedDmaBuf<'_>, String> {
        let surface = *self.surfaces.get(index).ok_or("surface out of range")?;
        let mut exported = export_surface_impl(&self.display, surface, ffi::VA_EXPORT_SURFACE_READ_ONLY | ffi::VA_EXPORT_SURFACE_SEPARATE_LAYERS)?;
        if self.strict_linear_nv12 {
            verify_linear_nv12_export(&exported, self.display.drm_node_id()?, self.size)?;
        }
        exported.resources = self.resources.clone();
        Ok(exported)
    }

    /// Own a decoded surface independently of this `Session`. While the lease is live, VA decode
    /// and fills fail for this surface; only exact GPU completion plus bounded VA sync releases it.
    pub fn export_surface_owned(&self, index: usize) -> Result<ResidentSurfaceLease, String> {
        let permit = crate::vulkan_import::TransferPermit::acquire(crate::vulkan_import::transfer_count())?;
        let slot = self.surface_leases.get(index).ok_or("surface out of range")?.clone();
        reserve_surface_slot(&slot)?;
        let exported = match self.export_surface(index) {
            Ok(exported) => exported.into_owned(),
            Err(error) => {
                slot.store(false, Ordering::Release);
                return Err(error);
            }
        };
        Ok(ResidentSurfaceLease { export: Some(exported), slot, _permit: Some(permit), retired: false })
    }

    /// A buffer of `ty` holding a copy of `len` bytes at `data`.
    fn buffer(&self, ty: ffi::VABufferType, data: *const c_void, len: usize) -> Result<ffi::VABufferID, String> {
        let size = c_uint::try_from(len).map_err(|_| "buffer too large")?;
        let mut id = ffi::VA_INVALID_ID;
        // SAFETY: `data` points to `len` readable bytes (the callers pass a live value or slice);
        // libva copies them into the new buffer and never writes through the pointer.
        let st = unsafe { (self.display.api.create_buffer)(self.display.dpy, self.context, ty, size, 1, data.cast_mut(), &mut id) };
        self.display.check(st, "vaCreateBuffer")?;
        Ok(id)
    }

    fn value_buffer<T>(&self, ty: ffi::VABufferType, value: &T) -> Result<ffi::VABufferID, String> {
        self.buffer(ty, (value as *const T).cast(), std::mem::size_of::<T>())
    }

    fn render(&self, buffers: &mut [ffi::VABufferID]) -> Result<(), String> {
        let n = c_int::try_from(buffers.len()).map_err(|_| "too many buffers")?;
        // SAFETY: `buffers` are live buffer ids made on this context, `n` of them.
        let st = unsafe { (self.display.api.render_picture)(self.display.dpy, self.context, buffers.as_mut_ptr(), n) };
        self.display.check(st, "vaRenderPicture")
    }

    /// Decode one picture into `surface`: the picture parameters `pic`, the scaling matrices `iq`
    /// when given, and a parameter + data buffer per slice; the buffers made go into `buffers`.
    fn decode_into<P, Q, S>(
        &self,
        surface: VASurfaceID,
        buffers: &mut Vec<ffi::VABufferID>,
        pic: &P,
        iq: Option<&Q>,
        slices: &[(S, Vec<u8>)],
    ) -> Result<(), String> {
        buffers.push(self.value_buffer(ffi::VAPictureParameterBufferType, pic)?);
        if let Some(iq) = iq {
            buffers.push(self.value_buffer(ffi::VAIQMatrixBufferType, iq)?);
        }
        let head = buffers.len();
        for (params, data) in slices {
            buffers.push(self.value_buffer(ffi::VASliceParameterBufferType, params)?);
            buffers.push(self.buffer(ffi::VASliceDataBufferType, data.as_ptr().cast(), data.len())?);
        }
        let api = self.display.api;
        // SAFETY: `surface` is one of this context's render targets.
        let st = unsafe { (api.begin_picture)(self.display.dpy, self.context, surface) };
        self.display.check(st, "vaBeginPicture")?;
        let (picture, slice_buffers) = buffers.split_at_mut(head);
        let mut rendered = self.render(picture);
        for pair in slice_buffers.chunks_mut(2) {
            if rendered.is_err() {
                break;
            }
            rendered = self.render(pair);
        }
        // a begun picture is always ended, so the context stays usable
        // SAFETY: the picture was begun on this context above.
        let st = unsafe { (api.end_picture)(self.display.dpy, self.context) };
        rendered?;
        self.display.check(st, "vaEndPicture")
    }

    fn image(&mut self) -> Result<VAImage, String> {
        if let Some(image) = self.image {
            return Ok(image);
        }
        let (_, fourcc, bits_per_pixel) = formats(self.geometry.bits)?;
        let mut format = VAImageFormat { fourcc, byte_order: 1, bits_per_pixel, ..Default::default() };
        let mut image = VAImage { image_id: ffi::VA_INVALID_ID, buf: ffi::VA_INVALID_ID, ..Default::default() };
        let (w, h) = (c_int::try_from(self.size.0).map_err(|_| "picture too wide")?, c_int::try_from(self.size.1).map_err(|_| "picture too tall")?);
        // SAFETY: `format` and `image` are live; the driver fills `image` in.
        let st = unsafe { (self.display.api.create_image)(self.display.dpy, &mut format, w, h, &mut image) };
        self.display.check(st, "vaCreateImage")?;
        self.image = Some(image);
        Ok(image)
    }

    /// The picture in surface `index`, copied into the session's image once decoded.
    fn get_image(&mut self, index: usize) -> Result<VAImage, String> {
        let surface = *self.surfaces.get(index).ok_or("surface out of range")?;
        let image = self.image()?;
        let api = self.display.api;
        // SAFETY: `surface` is one of this session's surfaces.
        let st = unsafe { (api.sync_surface)(self.display.dpy, surface) };
        self.display.check(st, "vaSyncSurface")?;
        // SAFETY: the image and the surface were made on this display, with the same size.
        let st = unsafe { (api.get_image)(self.display.dpy, surface, 0, 0, self.size.0, self.size.1, image.image_id) };
        self.display.check(st, "vaGetImage")?;
        Ok(image)
    }

    /// Every sample of `image` set to mid-gray: 128 in NV12, 512 (0x8000 in the high bits of
    /// little-endian 16-bit words) in P010.
    fn write_gray(&self, image: &VAImage) -> Result<(), String> {
        let api = self.display.api;
        let mut ptr: *mut c_void = std::ptr::null_mut();
        // SAFETY: `image.buf` is the data buffer of an image made on this display; `ptr` is a live output.
        let st = unsafe { (api.map_buffer)(self.display.dpy, image.buf, &mut ptr) };
        self.display.check(st, "vaMapBuffer")?;
        if !ptr.is_null() {
            // SAFETY: a mapped image buffer holds `data_size` writable bytes at `ptr` until it is
            // unmapped below; the slice does not outlive this block.
            let data = unsafe { std::slice::from_raw_parts_mut(ptr.cast::<u8>(), image.data_size as usize) };
            if self.geometry.bits > 8 {
                for w in data.as_chunks_mut::<2>().0 {
                    w.copy_from_slice(&0x8000u16.to_le_bytes());
                }
            } else {
                data.fill(128);
            }
        }
        // SAFETY: the buffer was mapped above.
        let st = unsafe { (api.unmap_buffer)(self.display.dpy, image.buf) };
        self.display.check(st, "vaUnmapBuffer")?;
        if ptr.is_null() { Err("vaMapBuffer returned no data".into()) } else { Ok(()) }
    }

    fn decode_picture<P, Q, S>(&mut self, target: usize, pic: &P, iq: Option<&Q>, slices: &[(S, Vec<u8>)]) -> Result<(), String> {
        self.ensure_surface_available(target)?;
        let surface = *self.surfaces.get(target).ok_or("surface out of range")?;
        let mut buffers = Vec::with_capacity(2 + 2 * slices.len());
        let decoded = self.decode_into(surface, &mut buffers, pic, iq, slices);
        for id in buffers {
            // SAFETY: each id is a buffer made above on this display, destroyed once; vaEndPicture
            // has returned, so the driver no longer reads it.
            unsafe { (self.display.api.destroy_buffer)(self.display.dpy, id) };
        }
        decoded
    }

    fn read_image(&self, image: &VAImage) -> Result<VideoFrame, String> {
        let api = self.display.api;
        let mut ptr: *mut c_void = std::ptr::null_mut();
        // SAFETY: `image.buf` is the data buffer of an image made on this display; `ptr` is a live output.
        let st = unsafe { (api.map_buffer)(self.display.dpy, image.buf, &mut ptr) };
        self.display.check(st, "vaMapBuffer")?;
        let result = if ptr.is_null() {
            Err("vaMapBuffer returned no data".to_string())
        } else {
            // SAFETY: a mapped image buffer holds `data_size` bytes at `ptr` until it is unmapped
            // below; the slice does not outlive this block.
            let data = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>().cast_const(), image.data_size as usize) };
            biplanar_view(image, data).and_then(|b| biplanar::to_frame(&b, &self.geometry))
        };
        // SAFETY: the buffer was mapped above; `data` is no longer used.
        let st = unsafe { (api.unmap_buffer)(self.display.dpy, image.buf) };
        let frame = result?;
        self.display.check(st, "vaUnmapBuffer")?;
        Ok(frame)
    }
}

#[cfg(test)]
mod linear_nv12_layout_tests {
    use super::*;

    fn valid() -> ((u32, u32), u64, [Nv12ExportPlane; 2]) {
        (
            (640, 368),
            353_280,
            [
                Nv12ExportPlane {
                    object_index: 0,
                    object_size: 353_280,
                    modifier: ffi::DRM_FORMAT_MOD_LINEAR,
                    layer_format: ffi::DRM_FORMAT_R8,
                    offset: 0,
                    pitch: 640,
                },
                Nv12ExportPlane {
                    object_index: 0,
                    object_size: 353_280,
                    modifier: ffi::DRM_FORMAT_MOD_LINEAR,
                    layer_format: ffi::DRM_FORMAT_GR88,
                    offset: 235_520,
                    pitch: 640,
                },
            ],
        )
    }

    #[test]
    fn accepts_exact_separate_nv12_layers() {
        let (size, object_size, planes) = valid();
        assert!(verify_linear_nv12_planes(size, object_size, planes).is_ok());
    }

    #[test]
    fn rejects_wrong_layer_format_object_and_modifier() {
        let (size, object_size, mut planes) = valid();
        planes[1].layer_format = ffi::DRM_FORMAT_R8;
        assert!(verify_linear_nv12_planes(size, object_size, planes).is_err());

        let (size, object_size, mut planes) = valid();
        planes[1].object_index = 1;
        assert!(verify_linear_nv12_planes(size, object_size, planes).is_err());

        let (size, object_size, mut planes) = valid();
        planes[1].modifier = 1;
        assert!(verify_linear_nv12_planes(size, object_size, planes).is_err());
    }

    #[test]
    fn rejects_short_pitch_out_of_bounds_and_overlapping_planes() {
        let (size, object_size, mut planes) = valid();
        planes[1].pitch = 638;
        assert!(verify_linear_nv12_planes(size, object_size, planes).is_err());

        let (size, object_size, mut planes) = valid();
        planes[1].offset = 235_518;
        assert!(verify_linear_nv12_planes(size, object_size, planes).is_err());

        let (size, mut object_size, mut planes) = valid();
        object_size -= 1;
        planes[0].object_size -= 1;
        planes[1].object_size -= 1;
        assert!(verify_linear_nv12_planes(size, object_size, planes).is_err());
    }

    #[test]
    fn requires_two_single_plane_layers() {
        assert!(verify_separate_nv12_layers(2, [Some(1), Some(1)]).is_ok());
        assert!(verify_separate_nv12_layers(1, [Some(1), Some(1)]).is_err());
        assert!(verify_separate_nv12_layers(2, [Some(2), Some(0)]).is_err());
        assert!(verify_separate_nv12_layers(2, [Some(1), None]).is_err());
    }
}

/// The two planes of a mapped NV12 / P010 image.
fn biplanar_view<'a>(image: &VAImage, data: &'a [u8]) -> Result<Biplanar<'a>, String> {
    if !matches!(image.format.fourcc, ffi::VA_FOURCC_NV12 | ffi::VA_FOURCC_P010) || image.num_planes < 2 {
        return Err(format!("the read-back image is not NV12 / P010 (fourcc {:#x}, {} planes)", image.format.fourcc, image.num_planes));
    }
    let [p0, p1, _] = image.pitches;
    if p0 != p1 {
        return Err(format!("NV12 planes with different pitches ({p0}, {p1})"));
    }
    let luma = data.get(image.offsets[0] as usize..).ok_or("NV12 luma offset out of range")?;
    let chroma = data.get(image.offsets[1] as usize..).ok_or("NV12 chroma offset out of range")?;
    Ok(Biplanar { luma, chroma, stride: p0 as usize, rows: usize::from(image.height) })
}

impl Accel for Session {
    fn surfaces(&self) -> &[VASurfaceID] {
        &self.surfaces
    }

    fn export_surface_owned(&self, index: usize) -> Result<ResidentSurfaceLease, String> {
        Session::export_surface_owned(self, index)
    }

    fn is_surface_leased(&self, index: usize) -> bool {
        self.surface_leases.get(index).is_some_and(|slot| slot.load(Ordering::Acquire))
    }

    fn decode_h264(
        &mut self,
        target: usize,
        pic: &ffi::VAPictureParameterBufferH264,
        iq: &ffi::VAIQMatrixBufferH264,
        slices: &[(ffi::VASliceParameterBufferH264, Vec<u8>)],
    ) -> Result<(), String> {
        self.decode_picture(target, pic, Some(iq), slices)
    }

    fn decode_hevc(
        &mut self,
        target: usize,
        pic: &ffi::VAPictureParameterBufferHEVC,
        iq: Option<&ffi::VAIQMatrixBufferHEVC>,
        slices: &[(ffi::VASliceParameterBufferHEVC, Vec<u8>)],
    ) -> Result<(), String> {
        self.decode_picture(target, pic, iq, slices)
    }

    fn read(&mut self, index: usize) -> Result<VideoFrame, String> {
        if self.strict_linear_nv12 {
            return Err("strict LINEAR NV12 resident decode forbids CPU image readback".into());
        }
        self.ensure_surface_available(index)?;
        let image = self.get_image(index)?;
        self.read_image(&image)
    }

    fn fill(&mut self, index: usize, from: Option<usize>) -> Result<(), String> {
        if self.strict_linear_nv12 {
            return Err("strict LINEAR NV12 resident decode forbids CPU reference fill".into());
        }
        self.ensure_surface_available(index)?;
        if let Some(from) = from {
            self.ensure_surface_available(from)?;
        }
        let surface = *self.surfaces.get(index).ok_or("surface out of range")?;
        let image = match from {
            Some(from) => self.get_image(from)?,
            None => {
                let image = self.image()?;
                self.write_gray(&image)?;
                image
            }
        };
        let (w, h) = self.size;
        // SAFETY: the image and the surface were made on this display, with the same size.
        let st = unsafe { (self.display.api.put_image)(self.display.dpy, surface, image.image_id, 0, 0, w, h, 0, 0, w, h) };
        self.display.check(st, "vaPutImage")
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.resources.is_some() {
            if let Some(image) = self.image.take() {
                // SAFETY: the upload/readback image is owned only by this session, never exported.
                unsafe { (self.display.api.destroy_image)(self.display.dpy, image.image_id) };
            }
            return;
        }
        let (api, dpy) = (self.display.api, self.display.dpy);
        // SAFETY: each object below was made on this display and is destroyed once, by this
        // session only; the display itself is terminated when its last user lets it go (the
        // shared one never is).
        unsafe {
            if let Some(image) = self.image.take() {
                (api.destroy_image)(dpy, image.image_id);
            }
            if self.context != ffi::VA_INVALID_ID {
                (api.destroy_context)(dpy, self.context);
            }
            if !self.surfaces.is_empty() {
                let n = c_int::try_from(self.surfaces.len()).unwrap_or(0);
                (api.destroy_surfaces)(dpy, self.surfaces.as_mut_ptr(), n);
            }
            if self.config != ffi::VA_INVALID_ID {
                (api.destroy_config)(dpy, self.config);
            }
        }
    }
}

/// What a driver's encoder offers for one profile (EncSlice).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncodeCaps {
    /// `VA_RC_*` modes it takes.
    pub rate_control: u32,
    /// Largest picture, when the driver says.
    pub max_size: Option<(u32, u32)>,
}

/// The shared display's encoder for `profile`, or why there is none (no driver, no EncSlice
/// entrypoint for the profile, no CBR / VBR rate control).
pub fn encode_caps(profile: ffi::VAProfile) -> Result<EncodeCaps, String> {
    let display = shared()?;
    encode_caps_on_display(&display, profile)
}

/// Query H.264 encode capabilities on exactly `node`; never consult the shared first-node display.
pub fn encode_caps_on_node(node: DrmNodeId, profile: ffi::VAProfile) -> Result<EncodeCaps, String> {
    encode_caps_on_display(&Display::open_node(node)?, profile)
}

fn encode_caps_on_display(d: &Display, profile: ffi::VAProfile) -> Result<EncodeCaps, String> {
    let mut a = [
        VAConfigAttrib { type_: ffi::VAConfigAttribRateControl, value: 0 },
        VAConfigAttrib { type_: ffi::VAConfigAttribMaxPictureWidth, value: 0 },
        VAConfigAttrib { type_: ffi::VAConfigAttribMaxPictureHeight, value: 0 },
    ];
    // SAFETY: `dpy` is initialised; `a` holds 3 live attributes for the driver to fill in. An
    // entrypoint the driver lacks for the profile is reported as an error status.
    let st = unsafe { (d.api.get_config_attributes)(d.dpy, profile, ffi::VAEntrypointEncSlice, a.as_mut_ptr(), 3) };
    d.check(st, "vaGetConfigAttributes (encode)")?;
    let [rc, w, h] = a.map(|x| x.value);
    if rc == ffi::VA_ATTRIB_NOT_SUPPORTED || rc & (ffi::VA_RC_CBR | ffi::VA_RC_VBR) == 0 {
        return Err(format!("profile {profile}: no CBR or VBR rate control ({rc:#x})"));
    }
    let known = |v: u32| v != ffi::VA_ATTRIB_NOT_SUPPORTED && v > 0;
    Ok(EncodeCaps { rate_control: rc, max_size: (known(w) && known(h)).then_some((w, h)) })
}

fn hevc_packed_headers(caps: &HevcEncodeCaps) -> Result<u32, String> {
    let packed = ffi::VA_ENC_PACKED_HEADER_SEQUENCE | ffi::VA_ENC_PACKED_HEADER_PICTURE | ffi::VA_ENC_PACKED_HEADER_SLICE;
    if caps.packed_headers == ffi::VA_ATTRIB_NOT_SUPPORTED || caps.packed_headers & packed != packed {
        return Err("HEVC driver lacks packed sequence/picture/slice headers".into());
    }
    Ok(packed)
}

/// HEVC Main's selected entrypoint and its 8-bit encode limits.
#[derive(Clone, Copy, Debug)]
pub struct HevcEncodeCaps {
    pub limits: EncodeCaps,
    pub entrypoint: ffi::VAEntrypoint,
    pub packed_headers: u32,
}

/// The bounded list actually advertised by the driver, rather than an attribute-only guess.
fn encode_entrypoints(d: &Display, profile: ffi::VAProfile) -> Result<Vec<ffi::VAEntrypoint>, String> {
    // SAFETY: the shared display is initialized and remains alive throughout the queries.
    let max_profiles = unsafe { (d.api.max_profiles)(d.dpy) };
    let max_profiles = usize::try_from(max_profiles).ok().filter(|n| (1..=256).contains(n)).ok_or("invalid VA profile capacity")?;
    let mut profiles = vec![0; max_profiles];
    let mut count = 0;
    // SAFETY: the array has the maximum length reported for this display, and count is writable.
    let status = unsafe { (d.api.query_profiles)(d.dpy, profiles.as_mut_ptr(), &mut count) };
    d.check(status, "vaQueryConfigProfiles")?;
    let count = usize::try_from(count).map_err(|_| "invalid VA profile count")?;
    if !profiles.get(..count).ok_or("VA profile count exceeds capacity")?.contains(&profile) {
        return Err(format!("VA profile {profile} not advertised"));
    }
    // SAFETY: the initialized display owns the queried profile.
    let max_entries = unsafe { (d.api.max_entrypoints)(d.dpy) };
    let max_entries = usize::try_from(max_entries).ok().filter(|n| (1..=256).contains(n)).ok_or("invalid VA entrypoint capacity")?;
    let mut entries = vec![0; max_entries];
    let mut count = 0;
    // SAFETY: the array has the maximum length reported by this display, and count is writable.
    let status = unsafe { (d.api.query_entrypoints)(d.dpy, profile, entries.as_mut_ptr(), &mut count) };
    d.check(status, "vaQueryConfigEntrypoints")?;
    let count = usize::try_from(count).map_err(|_| "invalid VA entrypoint count")?;
    if count > entries.len() {
        return Err("VA entrypoint count exceeds capacity".into());
    }
    entries.truncate(count);
    Ok(entries)
}

/// Query Main + an encode entrypoint. A decode-only profile never qualifies.
pub fn hevc_encode_caps() -> Result<HevcEncodeCaps, String> {
    hevc_encode_caps_on_display(&*shared()?)
}

/// [`hevc_encode_caps`] on the exact DRM render node the resident Vulkan device uses.
pub fn hevc_encode_caps_on_node(node: DrmNodeId) -> Result<HevcEncodeCaps, String> {
    hevc_encode_caps_on_display(&Display::open_node(node)?)
}

fn hevc_encode_caps_on_display(d: &Display) -> Result<HevcEncodeCaps, String> {
    let entries = encode_entrypoints(d, ffi::VAProfileHEVCMain)?;
    let mut reason = String::from("no HEVC Main encode entrypoint");
    for entrypoint in [ffi::VAEntrypointEncSlice, ffi::VAEntrypointEncSliceLP] {
        if !entries.contains(&entrypoint) {
            continue;
        }
        let mut attrs = [
            VAConfigAttrib { type_: ffi::VAConfigAttribRTFormat, value: 0 },
            VAConfigAttrib { type_: ffi::VAConfigAttribRateControl, value: 0 },
            VAConfigAttrib { type_: ffi::VAConfigAttribMaxPictureWidth, value: 0 },
            VAConfigAttrib { type_: ffi::VAConfigAttribMaxPictureHeight, value: 0 },
            VAConfigAttrib { type_: ffi::VAConfigAttribEncPackedHeaders, value: 0 },
        ];
        // SAFETY: the initialized shared display and five writable attributes remain live.
        let status = unsafe { (d.api.get_config_attributes)(d.dpy, ffi::VAProfileHEVCMain, entrypoint, attrs.as_mut_ptr(), 5) };
        if let Err(why) = d.check(status, "vaGetConfigAttributes (HEVC encode)") {
            reason = why;
            continue;
        }
        let [rt, rc, w, h, packed] = attrs.map(|a| a.value);
        if rt == ffi::VA_ATTRIB_NOT_SUPPORTED
            || rt & ffi::VA_RT_FORMAT_YUV420 == 0
            || rc == ffi::VA_ATTRIB_NOT_SUPPORTED
            || rc & (ffi::VA_RC_CBR | ffi::VA_RC_VBR) == 0
        {
            reason = "HEVC Main needs 8-bit 4:2:0 and CBR or VBR rate control".into();
            continue;
        }
        let known = |v| v != ffi::VA_ATTRIB_NOT_SUPPORTED && v > 0;
        return Ok(HevcEncodeCaps {
            limits: EncodeCaps { rate_control: rc, max_size: (known(w) && known(h)).then_some((w, h)) },
            entrypoint,
            packed_headers: packed,
        });
    }
    Err(reason)
}

/// One HEVC picture with the negotiated packed parameter sets and slice header.
pub struct HevcPicture<'a> {
    pub headers: &'a [(u32, &'a [u8])],
    pub sequence: Option<(&'a ffi::VAEncSequenceParameterBufferHEVC, &'a [Misc])>,
    pub picture: &'a ffi::VAEncPictureParameterBufferHEVC,
    pub slice: &'a ffi::VAEncSliceParameterBufferHEVC,
}

/// A misc parameter buffer of an encode (rate control, frame rate, HRD).
#[derive(Clone, Copy, Debug)]
pub enum Misc {
    RateControl(ffi::VAEncMisc<ffi::VAEncMiscParameterRateControl>),
    FrameRate(ffi::VAEncMisc<ffi::VAEncMiscParameterFrameRate>),
    Hrd(ffi::VAEncMisc<ffi::VAEncMiscParameterHRD>),
}

/// One H.264 picture to encode: on IDR pictures the sequence parameters and misc settings too.
pub struct H264Picture<'a> {
    pub sequence: Option<(&'a ffi::VAEncSequenceParameterBufferH264, &'a [Misc])>,
    pub picture: &'a ffi::VAEncPictureParameterBufferH264,
    pub slice: &'a ffi::VAEncSliceParameterBufferH264,
}

/// Most coded-buffer segments read per picture (bounds the driver's linked list).
const MAX_SEGMENTS: usize = 256;

/// One stream's encoder: an EncSlice configuration, its context, one input surface and the
/// reconstructed-picture surfaces the encoder references, the coded-output buffer and the image
/// pictures are uploaded through. On the shared display (see [`SHARED`]).
pub struct EncodeSession {
    resources: Option<Arc<VaResourceOwner>>,
    display: Arc<Display>,
    config: ffi::VAConfigID,
    context: ffi::VAContextID,
    /// `[input, recon 0, recon 1, …]`.
    surfaces: Vec<VASurfaceID>,
    coded: ffi::VABufferID,
    coded_capacity: usize,
    image: Option<VAImage>,
    /// A failed submission/synchronization quarantines all surfaces until context teardown.
    poisoned: Arc<std::sync::atomic::AtomicBool>,
    /// Coded size: the picture rounded up to whole macroblocks.
    size: (u32, u32),
}

impl EncodeSession {
    /// The VA driver description for this session's exact display.
    pub fn vendor(&self) -> &str {
        self.display.vendor()
    }

    /// Physical render-node identity from the same open DRM file used by this VA display.
    pub fn drm_node_id(&self) -> Result<DrmNodeId, String> {
        self.display.drm_node_id()
    }

    /// Export the NV12 input surface for external GPU writes. Drop the descriptor only after the
    /// GPU submission using its imported planes has completed, before submitting VA encoding.
    pub fn export_input_surface(&self) -> Result<ExportedDmaBuf<'_>, String> {
        if self.poisoned.load(std::sync::atomic::Ordering::Acquire) {
            return Err("VA encode session is quarantined after a failed submission".into());
        }
        let input = *self.surfaces.first().ok_or("no input surface")?;
        let mut exported = export_surface_impl(&self.display, input, ffi::VA_EXPORT_SURFACE_WRITE_ONLY | ffi::VA_EXPORT_SURFACE_SEPARATE_LAYERS)?;
        exported.resources = self.resources.clone();
        Ok(exported)
    }

    /// Export strict linear-allocation telemetry surface. Fails if VA did not export LINEAR.
    pub fn export_linear_input_surface(&self) -> Result<ExportedDmaBuf<'_>, String> {
        let exported = self.export_input_surface()?;
        for object in 0..exported.object_count() {
            let (_, modifier) = exported.object_layout(object).ok_or("VA export object metadata missing")?;
            if modifier != ffi::DRM_FORMAT_MOD_LINEAR {
                return Err(format!("VA linear surface export unsupported: object {object} modifier {modifier:#x}"));
            }
        }
        Ok(exported)
    }

    pub(crate) fn poison_token(&self) -> Arc<std::sync::atomic::AtomicBool> {
        self.poisoned.clone()
    }

    pub(crate) fn require_bounded_sync(&self) -> Result<(), String> {
        if self.display.api.sync_surface2.is_some() {
            Ok(())
        } else {
            Err("resident VA interop requires bounded vaSyncSurface2; refusing unbounded synchronization".into())
        }
    }

    /// A session for `profile` with rate control `rc` (`VA_RC_*`) at `width`×`height` (rounded up
    /// to whole macroblocks) with `recon` reconstructed-picture surfaces.
    pub fn new(profile: ffi::VAProfile, rc: u32, width: u32, height: u32, recon: usize) -> Result<Self, String> {
        Self::with_entrypoint(profile, ffi::VAEntrypointEncSlice, rc, width, height, recon, 0, None)
    }

    /// Diagnostic-only session that asks libva to allocate every surface with LINEAR modifier.
    pub fn new_linear_diagnostic(node: DrmNodeId, profile: ffi::VAProfile, rc: u32, width: u32, height: u32) -> Result<Self, String> {
        Self::with_entrypoint(profile, ffi::VAEntrypointEncSlice, rc, width, height, 1, 0, Some((ffi::DRM_FORMAT_MOD_LINEAR, node)))
    }

    /// Strict resident H.264 session on the exact Vulkan render node, with `recon` reference
    /// surfaces. Requires bounded VA synchronization and verifies the allocated input as single-
    /// object LINEAR NV12 before returning it.
    pub fn new_resident_linear(node: DrmNodeId, profile: ffi::VAProfile, rc: u32, width: u32, height: u32, recon: usize) -> Result<Self, String> {
        let session = Self::with_entrypoint(profile, ffi::VAEntrypointEncSlice, rc, width, height, recon, 0, Some((ffi::DRM_FORMAT_MOD_LINEAR, node)))?;
        session.require_bounded_sync()?;
        let exported = session.export_input_surface()?;
        verify_linear_nv12_export(&exported, node, session.size)?;
        drop(exported);
        Ok(session)
    }

    /// HEVC uses the entrypoint that its capability query selected; H.264 keeps EncSlice.
    pub fn new_hevc(caps: &HevcEncodeCaps, rc: u32, width: u32, height: u32) -> Result<Self, String> {
        let packed = hevc_packed_headers(caps)?;
        Self::with_entrypoint(ffi::VAProfileHEVCMain, caps.entrypoint, rc, width, height, 2, packed, None)
    }

    /// Strict resident HEVC session on the exact Vulkan render node (see [`Self::new_resident_linear`]).
    pub fn new_hevc_resident_linear(node: DrmNodeId, caps: &HevcEncodeCaps, rc: u32, width: u32, height: u32) -> Result<Self, String> {
        let packed = hevc_packed_headers(caps)?;
        let session = Self::with_entrypoint(ffi::VAProfileHEVCMain, caps.entrypoint, rc, width, height, 2, packed, Some((ffi::DRM_FORMAT_MOD_LINEAR, node)))?;
        session.require_bounded_sync()?;
        let exported = session.export_input_surface()?;
        verify_linear_nv12_export(&exported, node, session.size)?;
        drop(exported);
        Ok(session)
    }

    fn with_entrypoint(
        profile: ffi::VAProfile,
        entrypoint: ffi::VAEntrypoint,
        rc: u32,
        width: u32,
        height: u32,
        recon: usize,
        packed: u32,
        modifier: Option<(u64, DrmNodeId)>,
    ) -> Result<Self, String> {
        // HEVC drivers may round reconstructed widths to whole CTUs when writing the SPS.
        let alignment = if profile == ffi::VAProfileHEVCMain { 64 } else { 16 };
        let (w, h) = (width.div_ceil(alignment).saturating_mul(alignment), height.div_ceil(alignment).saturating_mul(alignment));
        if w == 0 || h == 0 || w > 16384 || h > 16384 || recon == 0 || recon > 16 {
            return Err(format!("no {width}x{height} encode session with {recon} references"));
        }
        let (modifier, display) = match modifier {
            Some((modifier, node)) => (Some(modifier), Arc::new(Display::open_node(node)?)),
            None => (None, shared()?),
        };
        let (api, dpy) = (display.api, display.dpy);
        let mut attr = vec![
            VAConfigAttrib { type_: ffi::VAConfigAttribRTFormat, value: ffi::VA_RT_FORMAT_YUV420 },
            VAConfigAttrib { type_: ffi::VAConfigAttribRateControl, value: rc },
        ];
        if packed != 0 {
            attr.push(VAConfigAttrib { type_: ffi::VAConfigAttribEncPackedHeaders, value: packed });
        }
        let attr_count = if packed != 0 { 3 } else { 2 };
        let mut config = ffi::VA_INVALID_ID;
        // SAFETY: `dpy` is initialised; `attr` holds `attr_count` live attributes; `config` is a live output.
        let st = unsafe { (api.create_config)(dpy, profile, entrypoint, attr.as_mut_ptr(), attr_count, &mut config) };
        display.check(st, "vaCreateConfig (encode)")?;
        // From here on `Drop` releases what was made.
        let mut s = Self {
            resources: None,
            display,
            config,
            context: ffi::VA_INVALID_ID,
            surfaces: Vec::new(),
            coded: ffi::VA_INVALID_ID,
            coded_capacity: 0,
            image: None,
            poisoned: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            size: (w, h),
        };
        let count = recon.saturating_add(1);
        let count_c = c_uint::try_from(count).map_err(|_| "too many surfaces")?;
        let mut surfaces = vec![ffi::VA_INVALID_SURFACE; count];
        let mut modifiers = modifier.into_iter().collect::<Vec<_>>();
        let modifier_count = u32::try_from(modifiers.len()).map_err(|_| "too many DRM modifiers")?;
        let mut modifier_lists =
            (0..count).map(|_| ffi::VADRMFormatModifierList { num_modifiers: modifier_count, modifiers: modifiers.as_mut_ptr() }).collect::<Vec<_>>();
        let mut surface_attribs = modifier.map(|_| ffi::VASurfaceAttrib {
            type_: ffi::VA_SURFACE_ATTRIB_DRM_FORMAT_MODIFIERS,
            flags: ffi::VA_SURFACE_ATTRIB_SETTABLE,
            value: ffi::VAGenericValue {
                type_: ffi::VA_GENERIC_VALUE_TYPE_POINTER,
                value: ffi::VAGenericValueData { pointer: modifier_lists.as_mut_ptr().cast() },
            },
        });
        if modifier.is_some() {
            verify_modifier_attribute(&s.display, s.config)?;
        }
        // SAFETY: `surfaces` has room for `count` IDs; optional attribute and modifier list stay
        // live for the call. Legacy paths pass no attributes and preserve old allocation behavior.
        let st = unsafe {
            (api.create_surfaces)(
                dpy,
                ffi::VA_RT_FORMAT_YUV420,
                w,
                h,
                surfaces.as_mut_ptr(),
                count_c,
                surface_attribs.as_mut().map_or(std::ptr::null_mut(), std::ptr::from_mut),
                u32::from(surface_attribs.is_some()),
            )
        };
        if modifier.is_some() {
            s.display.check(st, "vaCreateSurfaces (LINEAR)").map_err(|e| format!("VA linear surface allocation unsupported: {e}"))?;
        } else {
            s.display.check(st, "vaCreateSurfaces (encode)")?;
        }
        s.surfaces = surfaces;
        let (wc, hc) = (c_int::try_from(w).map_err(|_| "picture too wide")?, c_int::try_from(h).map_err(|_| "picture too tall")?);
        let n = c_int::try_from(count).map_err(|_| "too many surfaces")?;
        let mut context = ffi::VA_INVALID_ID;
        // SAFETY: `config` and the `count` surfaces were made on this display; `context` is a live output.
        let st = unsafe { (api.create_context)(dpy, s.config, wc, hc, ffi::VA_PROGRESSIVE, s.surfaces.as_mut_ptr(), n, &mut context) };
        s.display.check(st, "vaCreateContext (encode)")?;
        s.context = context;
        // coded output: an uncompressed 4:2:0 picture plus headroom is always enough
        let capacity = (w as usize).saturating_mul(h as usize).saturating_mul(3) / 2 + 65_536;
        let capacity_c = c_uint::try_from(capacity).map_err(|_| "coded buffer too large")?;
        let mut coded = ffi::VA_INVALID_ID;
        // SAFETY: a coded buffer is created without initial data (a null data pointer is allowed).
        let st = unsafe { (api.create_buffer)(dpy, s.context, ffi::VAEncCodedBufferType, capacity_c, 1, std::ptr::null_mut(), &mut coded) };
        s.display.check(st, "vaCreateBuffer (coded)")?;
        s.coded = coded;
        s.coded_capacity = capacity;
        s.resources = Some(retain_resources(s.display.clone(), s.config, s.context, s.surfaces.clone(), s.coded, Some(s.poisoned.clone())));
        Ok(s)
    }

    pub fn coded_size(&self) -> (u32, u32) {
        self.size
    }

    pub fn coded_buffer(&self) -> ffi::VABufferID {
        self.coded
    }

    /// The surface of reconstructed-picture slot `i`.
    pub fn recon(&self, i: usize) -> Option<VASurfaceID> {
        self.surfaces.get(i.checked_add(1)?).copied()
    }

    pub fn recon_count(&self) -> usize {
        self.surfaces.len().saturating_sub(1)
    }

    fn image(&mut self) -> Result<VAImage, String> {
        if let Some(image) = self.image {
            return Ok(image);
        }
        let mut format = VAImageFormat { fourcc: ffi::VA_FOURCC_NV12, byte_order: 1, bits_per_pixel: 12, ..Default::default() };
        let mut image = VAImage { image_id: ffi::VA_INVALID_ID, buf: ffi::VA_INVALID_ID, ..Default::default() };
        let (w, h) = (c_int::try_from(self.size.0).map_err(|_| "picture too wide")?, c_int::try_from(self.size.1).map_err(|_| "picture too tall")?);
        // SAFETY: `format` and `image` are live; the driver fills `image` in.
        let st = unsafe { (self.display.api.create_image)(self.display.dpy, &mut format, w, h, &mut image) };
        self.display.check(st, "vaCreateImage (upload)")?;
        self.image = Some(image);
        Ok(image)
    }

    /// Copy a planar 8-bit 4:2:0 picture (`w`×`h`, tight planes) into the input surface,
    /// replicating the right / bottom edges into the macroblock padding.
    pub fn upload(&mut self, y: &[u8], u: &[u8], v: &[u8], w: usize, h: usize) -> Result<(), String> {
        if self.poisoned.load(std::sync::atomic::Ordering::Acquire) {
            return Err("VA encode session is quarantined after a failed submission".into());
        }
        let input = *self.surfaces.first().ok_or("no input surface")?;
        let image = self.image()?;
        let api = self.display.api;
        let mut ptr: *mut c_void = std::ptr::null_mut();
        // SAFETY: `image.buf` is the data buffer of an image made on this display; `ptr` is a live output.
        let st = unsafe { (api.map_buffer)(self.display.dpy, image.buf, &mut ptr) };
        self.display.check(st, "vaMapBuffer (upload)")?;
        let filled = if ptr.is_null() {
            Err("vaMapBuffer returned no data".to_string())
        } else {
            // SAFETY: a mapped image buffer holds `data_size` writable bytes at `ptr` until it is
            // unmapped below; the slice does not outlive this block.
            let data = unsafe { std::slice::from_raw_parts_mut(ptr.cast::<u8>(), image.data_size as usize) };
            let [p0, p1, _] = image.pitches;
            let [o0, o1, _] = image.offsets;
            nv12::fill(data, [p0, p1], [o0, o1], y, u, v, w, h, self.size.0 as usize, self.size.1 as usize)
        };
        // SAFETY: the buffer was mapped above; `data` is no longer used.
        let st = unsafe { (api.unmap_buffer)(self.display.dpy, image.buf) };
        filled?;
        self.display.check(st, "vaUnmapBuffer (upload)")?;
        let (sw, sh) = self.size;
        // SAFETY: the image and the input surface were made on this display, with the same size.
        let st = unsafe { (api.put_image)(self.display.dpy, input, image.image_id, 0, 0, sw, sh, 0, 0, sw, sh) };
        self.display.check(st, "vaPutImage (upload)")
    }

    fn value_buffer<T>(&self, ty: ffi::VABufferType, value: &T) -> Result<ffi::VABufferID, String> {
        let size = c_uint::try_from(std::mem::size_of::<T>()).map_err(|_| "buffer too large")?;
        let mut id = ffi::VA_INVALID_ID;
        // SAFETY: `value` is a live `T` of `size` bytes (a `repr(C)` parameter structure); libva
        // copies it into the new buffer and never writes through the pointer.
        let st = unsafe { (self.display.api.create_buffer)(self.display.dpy, self.context, ty, size, 1, (value as *const T).cast_mut().cast(), &mut id) };
        self.display.check(st, "vaCreateBuffer (encode)")?;
        Ok(id)
    }

    fn buffers(&self, p: &H264Picture<'_>, ids: &mut Vec<ffi::VABufferID>) -> Result<(), String> {
        if let Some((seq, misc)) = p.sequence {
            ids.push(self.value_buffer(ffi::VAEncSequenceParameterBufferType, seq)?);
            for m in misc {
                ids.push(match m {
                    Misc::RateControl(x) => self.value_buffer(ffi::VAEncMiscParameterBufferType, x)?,
                    Misc::FrameRate(x) => self.value_buffer(ffi::VAEncMiscParameterBufferType, x)?,
                    Misc::Hrd(x) => self.value_buffer(ffi::VAEncMiscParameterBufferType, x)?,
                });
            }
        }
        ids.push(self.value_buffer(ffi::VAEncPictureParameterBufferType, p.picture)?);
        ids.push(self.value_buffer(ffi::VAEncSliceParameterBufferType, p.slice)?);
        Ok(())
    }

    fn submit(&self, ids: &mut [ffi::VABufferID]) -> Result<(), String> {
        let input = *self.surfaces.first().ok_or("no input surface")?;
        let api = self.display.api;
        let n = c_int::try_from(ids.len()).map_err(|_| "too many buffers")?;
        // SAFETY: `input` is one of this context's render targets.
        let st = unsafe { (api.begin_picture)(self.display.dpy, self.context, input) };
        self.display.check(st, "vaBeginPicture (encode)")?;
        // SAFETY: `ids` are `n` live buffers made on this context.
        let st = unsafe { (api.render_picture)(self.display.dpy, self.context, ids.as_mut_ptr(), n) };
        let rendered = self.display.check(st, "vaRenderPicture (encode)");
        // a begun picture is always ended, so the context stays usable
        // SAFETY: the picture was begun on this context above.
        let st = unsafe { (api.end_picture)(self.display.dpy, self.context) };
        rendered?;
        self.display.check(st, "vaEndPicture (encode)")
    }

    fn coded_bytes(&self) -> Result<Vec<u8>, String> {
        let input = *self.surfaces.first().ok_or("no input surface")?;
        let api = self.display.api;
        if let Some(sync) = api.sync_surface2 {
            // SAFETY: input belongs to this session; timeout is a finite five seconds in nanoseconds.
            let status = unsafe { sync(self.display.dpy, input, 5_000_000_000) };
            if status != ffi::VA_STATUS_ERROR_UNIMPLEMENTED {
                self.display.check(status, "vaSyncSurface2 (encode, 5 s)")?;
            } else {
                return Err("bounded vaSyncSurface2 is unsupported; refusing unbounded encode synchronization".into());
            }
        } else {
            // SAFETY: input belongs to this context; older libva provides no bounded synchronization.
            let status = unsafe { (api.sync_surface)(self.display.dpy, input) };
            self.display.check(status, "vaSyncSurface (encode)")?;
        }
        let mut ptr: *mut c_void = std::ptr::null_mut();
        // SAFETY: `coded` is this session's coded buffer; `ptr` is a live output.
        let st = unsafe { (api.map_buffer)(self.display.dpy, self.coded, &mut ptr) };
        self.display.check(st, "vaMapBuffer (coded)")?;
        let mut out = Vec::new();
        let mut result = Ok(());
        let mut seg = ptr.cast::<ffi::VACodedBufferSegment>().cast_const();
        for _ in 0..MAX_SEGMENTS {
            if seg.is_null() {
                break;
            }
            // SAFETY: a mapped coded buffer is a chain of `VACodedBufferSegment`s owned by the
            // driver and valid until vaUnmapBuffer; `seg` is non-null and from that chain.
            let s = unsafe { std::ptr::read(seg) };
            let size = s.size as usize;
            if s.bit_offset != 0
                || s.status
                    & (ffi::VA_CODED_BUF_STATUS_SLICE_OVERFLOW_MASK | ffi::VA_CODED_BUF_STATUS_FRAME_SIZE_OVERFLOW | ffi::VA_CODED_BUF_STATUS_BAD_BITSTREAM)
                    != 0
            {
                result = Err(format!("VA coded segment is incomplete or corrupt: status={:#x}, bit_offset={}", s.status, s.bit_offset));
                break;
            }
            if (s.buf.is_null() && size > 0) || out.len().saturating_add(size) > self.coded_capacity {
                result = Err("coded segment larger than its buffer".to_string());
                break;
            }
            if size > 0 {
                // SAFETY: the segment's `buf` holds `size` bytes while mapped (bounded above by the
                // buffer's capacity).
                out.extend_from_slice(unsafe { std::slice::from_raw_parts(s.buf.cast::<u8>().cast_const(), size) });
            }
            seg = s.next.cast::<ffi::VACodedBufferSegment>().cast_const();
        }
        if result.is_ok() && !seg.is_null() {
            result = Err("VA coded segment chain exceeds the safety limit".to_string());
        }
        // SAFETY: the buffer was mapped above and is no longer read.
        let st = unsafe { (api.unmap_buffer)(self.display.dpy, self.coded) };
        result?;
        self.display.check(st, "vaUnmapBuffer (coded)")?;
        Ok(out)
    }

    fn bytes_buffer(&self, ty: ffi::VABufferType, bytes: &[u8]) -> Result<ffi::VABufferID, String> {
        let size = c_uint::try_from(bytes.len()).map_err(|_| "packed header too large")?;
        let mut id = ffi::VA_INVALID_ID;
        // SAFETY: bytes is a live slice of size initialized bytes; libva copies it before returning.
        let status = unsafe { (self.display.api.create_buffer)(self.display.dpy, self.context, ty, size, 1, bytes.as_ptr().cast_mut().cast(), &mut id) };
        self.display.check(status, "vaCreateBuffer (packed HEVC header)")?;
        Ok(id)
    }

    /// Encode HEVC using the same surfaces, synchronous submission and coded-buffer ownership.
    pub fn encode_hevc(&mut self, p: &HevcPicture<'_>) -> Result<Vec<u8>, String> {
        if self.poisoned.load(std::sync::atomic::Ordering::Acquire) {
            return Err("VA encode session is quarantined after a failed submission".into());
        }
        self.poisoned.store(true, std::sync::atomic::Ordering::Release);
        let mut ids = Vec::with_capacity(8);
        let made = (|| {
            if let Some((sequence, misc)) = p.sequence {
                ids.push(self.value_buffer(ffi::VAEncSequenceParameterBufferType, sequence)?);
                for item in misc {
                    ids.push(match item {
                        Misc::RateControl(v) => self.value_buffer(ffi::VAEncMiscParameterBufferType, v)?,
                        Misc::FrameRate(v) => self.value_buffer(ffi::VAEncMiscParameterBufferType, v)?,
                        Misc::Hrd(v) => self.value_buffer(ffi::VAEncMiscParameterBufferType, v)?,
                    });
                }
            }
            ids.push(self.value_buffer(ffi::VAEncPictureParameterBufferType, p.picture)?);
            for (kind, data) in p.headers {
                let bits = u32::try_from(data.len()).ok().and_then(|n| n.checked_mul(8)).ok_or("packed header bit count overflow")?;
                let parameter = ffi::VAEncPackedHeaderParameterBuffer { type_: *kind, bit_length: bits, has_emulation_bytes: 1, va_reserved: [0; 4] };
                ids.push(self.value_buffer(ffi::VAEncPackedHeaderParameterBufferType, &parameter)?);
                ids.push(self.bytes_buffer(ffi::VAEncPackedHeaderDataBufferType, data)?);
            }
            ids.push(self.value_buffer(ffi::VAEncSliceParameterBufferType, p.slice)?);
            self.submit(&mut ids)
        })();
        for id in ids {
            // SAFETY: each parameter buffer belongs to this context and is released once after submission.
            unsafe { (self.display.api.destroy_buffer)(self.display.dpy, id) };
        }
        made?;
        let bytes = self.coded_bytes()?;
        self.poisoned.store(false, std::sync::atomic::Ordering::Release);
        Ok(bytes)
    }

    /// Encode the uploaded input picture with `p`'s parameters; the coded bytes (Annex B).
    pub fn encode_h264(&mut self, p: &H264Picture<'_>) -> Result<Vec<u8>, String> {
        if self.poisoned.load(std::sync::atomic::Ordering::Acquire) {
            return Err("VA encode session is quarantined after a failed submission".into());
        }
        self.poisoned.store(true, std::sync::atomic::Ordering::Release);
        let mut ids = Vec::with_capacity(8);
        let made = self.buffers(p, &mut ids);
        let submitted = made.and_then(|()| self.submit(&mut ids));
        for id in ids {
            // SAFETY: each id is a buffer made above on this display, destroyed once; vaEndPicture
            // has returned (or the picture was never begun), so the driver no longer reads it.
            unsafe { (self.display.api.destroy_buffer)(self.display.dpy, id) };
        }
        submitted?;
        let bytes = self.coded_bytes()?;
        self.poisoned.store(false, std::sync::atomic::Ordering::Release);
        Ok(bytes)
    }
}

/// Keep the display owner too: dropping its last Arc would call vaTerminate on live allocations.
fn retain_if_quarantined<T>(poisoned: bool, owner: T) -> Option<T> {
    if poisoned {
        std::mem::forget(owner);
        None
    } else {
        Some(owner)
    }
}

impl Drop for EncodeSession {
    fn drop(&mut self) {
        if let Some(image) = self.image.take() {
            // SAFETY: the CPU upload image is owned only by this session, never exported, and the
            // encoder never reads it after the upload returns.
            unsafe { (self.display.api.destroy_image)(self.display.dpy, image.image_id) };
        }
        if self.resources.is_some() {
            // The shared owner (see `retain_resources`) releases the rest, with the same rule.
            return;
        }
        if retain_if_quarantined(self.poisoned.load(Ordering::Acquire), self.display.clone()).is_none() {
            log::warn!("VA encode session dropped while quarantined: keeping its config, context, surfaces and coded buffer");
            return;
        }
        let (api, dpy) = (self.display.api, self.display.dpy);
        // SAFETY: each object below was made on this display and is destroyed once, children
        // first; the shared display itself is never terminated here.
        unsafe {
            if self.coded != ffi::VA_INVALID_ID {
                (api.destroy_buffer)(dpy, self.coded);
            }
            if self.context != ffi::VA_INVALID_ID {
                (api.destroy_context)(dpy, self.context);
            }
            if !self.surfaces.is_empty() {
                let n = c_int::try_from(self.surfaces.len()).unwrap_or(0);
                (api.destroy_surfaces)(dpy, self.surfaces.as_mut_ptr(), n);
            }
            if self.config != ffi::VA_INVALID_ID {
                (api.destroy_config)(dpy, self.config);
            }
        }
    }
}

/// Whether this system has a working VA-API driver on a DRM render node (checked once), with the
/// driver's description, or why not.
pub fn probe() -> &'static Result<String, String> {
    static PROBE: OnceLock<Result<String, String>> = OnceLock::new();
    // the display opened to answer is the one sessions share
    PROBE.get_or_init(|| shared().map(|d| d.vendor().to_string()))
}

pub(crate) fn validate_video_probe(
    request: filmcraft_export::Format,
    expected: DrmNodeId,
    actual: Option<DrmNodeId>,
    vendor: Option<u32>,
    device: Option<u32>,
    profile: ffi::VAProfile,
    entrypoint: ffi::VAEntrypoint,
    packed_headers: u32,
    encode: bool,
    input: bool,
) -> Result<crate::amd_backend::VaapiVideoCapabilities, String> {
    use filmcraft_export::Format;
    if actual != Some(expected) {
        return Err("VA-API DRM node missing or mismatched".into());
    }
    if vendor != Some(0x1002) {
        return Err("VA-API node has non-AMD or unknown vendor identity".into());
    }
    let supported = match request {
        Format::H264 => [ffi::VAProfileH264Main, ffi::VAProfileH264High].contains(&profile),
        Format::Hevc => profile == ffi::VAProfileHEVCMain,
        _ => false,
    };
    let valid_entrypoint = match request {
        Format::H264 => entrypoint == ffi::VAEntrypointEncSlice,
        Format::Hevc => matches!(entrypoint, ffi::VAEntrypointEncSlice | ffi::VAEntrypointEncSliceLP),
        _ => false,
    };
    if !supported || !valid_entrypoint || !encode || !input {
        return Err("VA-API needs requested codec profile, encode entrypoint and YUV420 input".into());
    }
    validate_packed_headers(request, packed_headers)?;
    Ok(crate::amd_backend::VaapiVideoCapabilities {
        drm_node: actual,
        vendor_id: vendor,
        device_id: device,
        profile: Some(profile),
        rate_control: 0,
        max_size: None,
        entrypoint,
        packed_headers,
        codec: request,
        encode,
        codec_profile: supported,
        input_format: input,
    })
}

/// Inspect only the Vulkan-selected node. No VA configuration, surface or session is created.
pub(crate) fn vaapi_video_capabilities(
    request: filmcraft_export::Format,
    expected_node: DrmNodeId,
) -> Result<crate::amd_backend::VaapiVideoCapabilities, String> {
    use filmcraft_export::Format;
    let sysfs = format!("/sys/dev/char/{}:{}/device", expected_node.major, expected_node.minor);
    let identity = |name| std::fs::read_to_string(format!("{sysfs}/{name}")).ok().and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok());
    let vendor = identity("vendor");
    let device = identity("device");
    if vendor != Some(0x1002) {
        return Err("VA-API node has non-AMD or unknown vendor identity".into());
    }
    let d = Display::open_node(expected_node)?;
    let profiles: &[ffi::VAProfile] = match request {
        Format::H264 => &[ffi::VAProfileH264High, ffi::VAProfileH264Main],
        Format::Hevc => &[ffi::VAProfileHEVCMain],
        _ => return Err("unsupported VA-API encode codec".into()),
    };
    let mut reason = String::from("no requested VA-API encode profile");
    for &profile in profiles {
        let entries = match encode_entrypoints(&d, profile) {
            Ok(entries) => entries,
            Err(why) => {
                reason = why;
                continue;
            }
        };
        for entrypoint in [ffi::VAEntrypointEncSlice, ffi::VAEntrypointEncSliceLP] {
            if !entries.contains(&entrypoint) || (request == Format::H264 && entrypoint != ffi::VAEntrypointEncSlice) {
                continue;
            }
            let mut attrs = [
                VAConfigAttrib { type_: ffi::VAConfigAttribRTFormat, value: 0 },
                VAConfigAttrib { type_: ffi::VAConfigAttribRateControl, value: 0 },
                VAConfigAttrib { type_: ffi::VAConfigAttribEncPackedHeaders, value: 0 },
                VAConfigAttrib { type_: ffi::VAConfigAttribMaxPictureWidth, value: 0 },
                VAConfigAttrib { type_: ffi::VAConfigAttribMaxPictureHeight, value: 0 },
            ];
            let count = 5;
            // SAFETY: initialized display, advertised profile/entrypoint, count fits the writable array.
            let status = unsafe { (d.api.get_config_attributes)(d.dpy, profile, entrypoint, attrs.as_mut_ptr(), count) };
            if let Err(why) = d.check(status, "vaGetConfigAttributes (same-node encode)") {
                reason = why;
                continue;
            }
            let [rt, rc, packed, width, height] = attrs.map(|a| a.value);
            let encode = rc != ffi::VA_ATTRIB_NOT_SUPPORTED && rc & (ffi::VA_RC_CBR | ffi::VA_RC_VBR) != 0;
            let input = rt != ffi::VA_ATTRIB_NOT_SUPPORTED && rt & ffi::VA_RT_FORMAT_YUV420 != 0;
            match validate_video_probe(request, expected_node, Some(d.drm_node_id()?), vendor, device, profile, entrypoint, packed, encode, input) {
                Ok(mut caps) => {
                    let known = |v| v != ffi::VA_ATTRIB_NOT_SUPPORTED && v > 0;
                    caps.rate_control = rc;
                    caps.max_size = (known(width) && known(height)).then_some((width, height));
                    caps.entrypoint = entrypoint;
                    caps.packed_headers = packed;
                    return Ok(caps);
                }
                Err(why) => reason = why,
            }
        }
    }
    Err(reason)
}

pub(crate) fn validate_render_node(expected: DrmNodeId, actual: DrmNodeId, character_device: bool, name: &str) -> Result<(), String> {
    let render_name = name.strip_prefix("renderD").is_some_and(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()));
    if !render_name || !character_device {
        return Err("expected DRM render character device".into());
    }
    if actual != expected {
        return Err("VA-API DRM node mismatch on opened descriptor".into());
    }
    Ok(())
}

/// The message for a render node that exists but this user may not open. Other open errors (the
/// node is missing, or its number is unused) are normal while scanning and give no reason.
fn render_node_denied(path: &str, kind: std::io::ErrorKind) -> Option<String> {
    (kind == std::io::ErrorKind::PermissionDenied).then(|| format!("{path}: permission denied (add the user to the render group)"))
}

/// The reason [`Display::open`] reports when no render node gives a usable display. A permission
/// denial comes first: it is the fix for the GPU node the user cannot open at all.
fn no_display_reason(denied: Option<String>, init_failure: Option<String>) -> String {
    denied.or(init_failure).unwrap_or_else(|| "no DRM render node (/dev/dri/renderD*)".to_string())
}

fn drm_node_from_rdev(rdev: u64) -> DrmNodeId {
    // Linux/glibc dev_t encoding, decoded from the same open node used by vaGetDisplayDRM.
    let major = ((rdev >> 8) & 0x0fff) | ((rdev >> 32) & 0xffff_f000);
    let minor = (rdev & 0x00ff) | ((rdev >> 12) & 0xffff_ff00);
    DrmNodeId { major: major as u32, minor: minor as u32 }
}

pub(crate) fn validate_packed_headers(request: filmcraft_export::Format, packed: u32) -> Result<(), String> {
    let required = ffi::VA_ENC_PACKED_HEADER_SEQUENCE | ffi::VA_ENC_PACKED_HEADER_PICTURE | ffi::VA_ENC_PACKED_HEADER_SLICE;
    if request == filmcraft_export::Format::Hevc && (packed == ffi::VA_ATTRIB_NOT_SUPPORTED || packed & required != required) {
        return Err("HEVC driver lacks packed sequence/picture/slice headers".into());
    }
    Ok(())
}

#[cfg(test)]
mod resource_retention_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn review_fix_export_requires_bounded_producer_sync() {
        assert!(bounded_export_sync(None::<fn(u64) -> ffi::VAStatus>).is_err());
        let exported = std::cell::Cell::new(false);
        let result = bounded_export_sync(Some(|timeout| {
            assert_eq!(timeout, 5_000_000_000);
            ffi::VA_STATUS_ERROR_TIMEDOUT
        }))
        .map(|()| exported.set(true));
        assert!(result.is_err());
        assert!(!exported.get(), "timed out producer must not export");
        assert!(bounded_export_sync(Some(|_| ffi::VA_STATUS_ERROR_UNIMPLEMENTED)).is_err());
        bounded_export_sync(Some(|timeout| {
            assert_eq!(timeout, 5_000_000_000);
            ffi::VA_STATUS_SUCCESS
        }))
        .expect("bounded producer completion");
    }

    #[test]
    fn owned_surface_reservation_blocks_reuse_until_retirement() {
        let slot = AtomicBool::new(false);
        reserve_surface_slot(&slot).expect("first lease");
        assert!(reserve_surface_slot(&slot).is_err(), "a surface cannot have concurrent consumers");
        slot.store(false, Ordering::Release); // The successful retirement transition.
        reserve_surface_slot(&slot).expect("reuse after retirement");
    }

    #[test]
    fn export_retains_session_resources_and_fd_until_final_drop() {
        let released = Arc::new(AtomicUsize::new(0));
        let observed = released.clone();
        let owner = Arc::new(VaResourceOwner {
            release: Some(Box::new(move || {
                observed.fetch_add(1, Ordering::SeqCst);
            })),
        });
        let (stream, _peer) = std::os::unix::net::UnixStream::pair().expect("synthetic FD");
        let fd: OwnedFd = stream.into();
        let raw = fd.as_raw_fd();
        let original_link = std::fs::read_link(format!("/proc/self/fd/{raw}")).expect("original FD");
        let mut fds = std::array::from_fn(|_| None);
        fds[0] = Some(fd);
        let exported = ExportedDmaBuf {
            descriptor: Default::default(),
            fds,
            drm_node: DrmNodeId { major: 226, minor: 128 },
            _session: PhantomData,
            resources: Some(owner.clone()),
            display: None,
            surface: ffi::VA_INVALID_SURFACE,
        };
        drop(owner);
        assert_eq!(released.load(Ordering::SeqCst), 0, "export still owns the complete VA session");
        assert!(std::path::Path::new(&format!("/proc/self/fd/{raw}")).exists());
        let imported_fd = exported.fds[0].as_ref().expect("object").try_clone().expect("import clone");
        let cloned_raw = imported_fd.as_raw_fd();
        drop(imported_fd); // Models partial import failure: HAL consumes or closes its clone.
        assert_ne!(std::fs::read_link(format!("/proc/self/fd/{cloned_raw}")).ok(), Some(original_link.clone()));
        assert_eq!(released.load(Ordering::SeqCst), 0);
        drop(exported);
        assert_eq!(released.load(Ordering::SeqCst), 1, "VA resources released exactly once");
        assert_ne!(std::fs::read_link(format!("/proc/self/fd/{raw}")).ok(), Some(original_link));
    }
}

#[cfg(test)]
mod render_node_error_tests {
    use super::*;

    #[test]
    fn permission_denied_names_the_node_and_the_group_fix() {
        let msg = render_node_denied("/dev/dri/renderD128", std::io::ErrorKind::PermissionDenied).expect("denial is reported");
        assert_eq!(msg, "/dev/dri/renderD128: permission denied (add the user to the render group)");
    }

    #[test]
    fn other_open_errors_are_not_reported_as_permission() {
        assert!(render_node_denied("/dev/dri/renderD129", std::io::ErrorKind::NotFound).is_none());
        assert!(render_node_denied("/dev/dri/renderD129", std::io::ErrorKind::NotADirectory).is_none());
    }

    #[test]
    fn denial_wins_over_a_driver_failure_and_default_is_kept() {
        let denied = render_node_denied("/dev/dri/renderD128", std::io::ErrorKind::PermissionDenied);
        let init = Some("/dev/dri/renderD129: vaInitialize failed".to_string());
        assert!(no_display_reason(denied.clone(), init.clone()).contains("permission denied"));
        assert_eq!(no_display_reason(None, init.clone()), "/dev/dri/renderD129: vaInitialize failed");
        assert_eq!(no_display_reason(denied, None), "/dev/dri/renderD128: permission denied (add the user to the render group)");
        assert_eq!(no_display_reason(None, None), "no DRM render node (/dev/dri/renderD*)");
    }
}

#[cfg(test)]
mod teardown_tests {
    use super::*;

    struct DropProbe(Arc<std::sync::atomic::AtomicUsize>);
    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn clean_session_releases_its_objects() {
        let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let owner = retain_if_quarantined(false, DropProbe(drops.clone()));
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(owner);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn quarantined_session_keeps_its_objects() {
        let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        assert!(retain_if_quarantined(true, DropProbe(drops.clone())).is_none());
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert_eq!(Arc::strong_count(&drops), 2, "quarantine must retain the display owner, not only skip child destruction");
    }
}
