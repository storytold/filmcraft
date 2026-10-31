//! libva itself (Linux): `libva.so.2` and `libva-drm.so.2` are loaded at run time (a system without
//! them simply has no hardware decoder), a display is opened on a DRM render node, and a [`Session`]
//! (decode configuration, context and surfaces for one stream) decodes pictures and reads them back
//! as NV12 images.
//!
//! A decoded surface can also be handed over as DMABUF (`vaExportSurfaceHandle`, DRM PRIME 2) with
//! [`Display::export_surface`]: the plane layout and the objects' file descriptors, for a compositor
//! that imports them as a texture (no readback). The import itself is not here yet: wgpu's safe API
//! has no external-memory import, so it needs raw Vulkan (`wgpu-hal`) plus the same-GPU rule and a
//! fallback to the readback.
//!
//! FFI module (docs/adr/0001-platform-ffi.md): every `unsafe` block has a `// SAFETY:` comment, the
//! public API is safe, and the one callback into Rust (libva's error messages) runs under
//! `catch_unwind`.

use std::ffi::{CStr, c_char, c_int, c_uint, c_void};
use std::fs::{File, OpenOptions};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use filmcraft_frame::VideoFrame;
use libloading::Library;

use super::Accel;
use super::ffi::{self, VAConfigAttrib, VAImage, VAImageFormat, VASurfaceID};
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
    create_image: ffi::vaCreateImage,
    destroy_image: ffi::vaDestroyImage,
    get_image: ffi::vaGetImage,
    /// Maps a surface's own memory as an image (no copy into a second image); optional.
    derive_image: Option<ffi::vaDeriveImage>,
    put_image: ffi::vaPutImage,
    map_buffer: ffi::vaMapBuffer,
    unmap_buffer: ffi::vaUnmapBuffer,
    get_display_drm: ffi::vaGetDisplayDRM,
    /// DMABUF export; None on a libva older than 2.1 (the readback stays the only path there).
    export_surface_handle: Option<ffi::vaExportSurfaceHandle>,
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
    // A symbol an older libva may not have: absent is not an error (the caller declines).
    macro_rules! sym_opt {
        ($lib:expr, $name:ident) => {{
            // SAFETY: as `sym!`, but the pointer is kept only when the symbol resolves.
            let s = unsafe { $lib.get::<ffi::$name>(concat!(stringify!($name), "\0").as_bytes()) };
            s.ok().map(|s| *s)
        }};
    }
    Ok(Api {
        initialize: sym!(va, vaInitialize),
        terminate: sym!(va, vaTerminate),
        error_str: sym!(va, vaErrorStr),
        vendor_string: sym!(va, vaQueryVendorString),
        set_info_callback: sym!(va, vaSetInfoCallback),
        set_error_callback: sym!(va, vaSetErrorCallback),
        get_config_attributes: sym!(va, vaGetConfigAttributes),
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
        create_image: sym!(va, vaCreateImage),
        destroy_image: sym!(va, vaDestroyImage),
        get_image: sym!(va, vaGetImage),
        derive_image: sym_opt!(va, vaDeriveImage),
        put_image: sym!(va, vaPutImage),
        map_buffer: sym!(va, vaMapBuffer),
        unmap_buffer: sym!(va, vaUnmapBuffer),
        get_display_drm: sym!(drm, vaGetDisplayDRM),
        export_surface_handle: sym_opt!(va, vaExportSurfaceHandle),
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
        let mut why = "no DRM render node (/dev/dri/renderD*)".to_string();
        for n in 128..144 {
            let path = format!("/dev/dri/renderD{n}");
            let Ok(file) = OpenOptions::new().read(true).write(true).open(&path) else { continue };
            match Self::init(api, file) {
                Ok(d) => return Ok(d),
                Err(e) => why = format!("{path}: {e}"),
            }
        }
        Err(why)
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

    /// Make `count` decode surfaces of `rt_format` at `size` (no attributes). The caller destroys
    /// them with [`destroy_surfaces`](Self::destroy_surfaces) or hands them to a session.
    pub fn create_surfaces(&self, rt_format: c_uint, size: (u32, u32), count: usize) -> Result<Vec<VASurfaceID>, String> {
        let count_c = c_uint::try_from(count).map_err(|_| "too many surfaces")?;
        let mut surfaces = vec![ffi::VA_INVALID_SURFACE; count];
        // SAFETY: `surfaces` has room for `count` ids; `self.dpy` is an initialised display; no
        // attributes are passed (null, 0), so the driver picks the layout.
        let st = unsafe { (self.api.create_surfaces)(self.dpy, rt_format, size.0, size.1, surfaces.as_mut_ptr(), count_c, std::ptr::null_mut(), 0) };
        self.check(st, "vaCreateSurfaces")?;
        Ok(surfaces)
    }

    /// Destroy surfaces made by [`create_surfaces`](Self::create_surfaces).
    pub fn destroy_surfaces(&self, surfaces: &mut [VASurfaceID]) -> Result<(), String> {
        if surfaces.is_empty() {
            return Ok(());
        }
        let count = c_int::try_from(surfaces.len()).map_err(|_| "too many surfaces")?;
        // SAFETY: `surfaces` is a live slice of ids this display made, `count` its length.
        let st = unsafe { (self.api.destroy_surfaces)(self.dpy, surfaces.as_mut_ptr(), count) };
        self.check(st, "vaDestroySurfaces")?;
        surfaces.fill(ffi::VA_INVALID_SURFACE);
        Ok(())
    }

    /// Export `surface` as DMABUF (DRM PRIME 2, one layer per plane): the plane layout - format,
    /// object, offset, pitch - and the objects' file descriptors, which the caller owns and which
    /// are closed when the returned value drops. This is what a GPU importer
    /// (`VK_EXT_external_memory_dma_buf` + `VK_EXT_image_drm_format_modifier`) binds as a texture
    /// where the compositor works, instead of a readback. The surface is synchronised first
    /// (`vaSyncSurface`; the export itself does not wait), so the picture is complete when the
    /// importer reads it. `Err` when libva has no `vaExportSurfaceHandle`, the driver refuses the
    /// surface, or what it describes is inconsistent.
    pub fn export_surface(&self, surface: VASurfaceID) -> Result<SurfaceDmabuf, String> {
        let export = self.api.export_surface_handle.ok_or("libva has no vaExportSurfaceHandle (needs libva 2.1)")?;
        // SAFETY: `surface` is an id this display made; vaSyncSurface only waits for its work.
        let st = unsafe { (self.api.sync_surface)(self.dpy, surface) };
        self.check(st, "vaSyncSurface")?;
        // SAFETY: every field is a plain integer, an array of integers or (below) a file
        // descriptor libva fills in; an all-zero descriptor is "nothing exported" and is checked
        // before anything is read from it.
        let mut d: ffi::VADRMPRIMESurfaceDescriptor = unsafe { std::mem::zeroed() };
        // one layer per plane, read-only: the decoder wrote the surface, the importer only reads it
        let flags = ffi::VA_EXPORT_SURFACE_READ_ONLY | ffi::VA_EXPORT_SURFACE_SEPARATE_LAYERS;
        // SAFETY: `self.dpy` is an initialised display, `surface` an id this display made, and `d`
        // is a live descriptor libva fills in. The driver creates the file descriptors it lists and
        // hands them to the caller (va_drmcommon.h: the driver does not close them).
        let st = unsafe { export(self.dpy, surface, ffi::VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2, flags, &mut d) };
        self.check(st, "vaExportSurfaceHandle")?;
        // What the driver describes is input-derived: check the counts before they are used as
        // lengths, and every index before it indexes an object.
        let objects = (d.num_objects as usize).min(d.objects.len());
        let layers = (d.num_layers as usize).min(d.layers.len());
        // Own every descriptor the driver handed over before checking anything, so an error below
        // closes them all instead of leaking the ones not yet looked at.
        // SAFETY: libva hands each listed descriptor to the caller (it does not close it), so this
        // takes ownership: each is closed exactly once, when its `OwnedFd` is dropped.
        let fds: Vec<Option<OwnedFd>> = d.objects.iter().take(objects).map(|o| (o.fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(o.fd) })).collect();
        if objects == 0 {
            return Err("vaExportSurfaceHandle described no objects".into());
        }
        let mut out_objects = Vec::with_capacity(objects);
        for (o, fd) in d.objects.iter().zip(fds) {
            let fd = fd.ok_or("vaExportSurfaceHandle described an invalid file descriptor")?;
            out_objects.push(DmabufObject { fd, size: o.size, modifier: o.drm_format_modifier });
        }
        let mut planes = Vec::new();
        for l in d.layers.iter().take(layers) {
            let n = l.num_planes as usize;
            for (p, ((&object, &offset), &pitch)) in l.object_index.iter().zip(&l.offset).zip(&l.pitch).take(n).enumerate() {
                let object = object as usize;
                if object >= objects {
                    return Err(format!("vaExportSurfaceHandle described plane {p} in object {object} of {objects}"));
                }
                planes.push(DmabufPlane { format: l.drm_format, object, offset, pitch });
            }
        }
        if planes.is_empty() {
            return Err("vaExportSurfaceHandle described no planes".into());
        }
        Ok(SurfaceDmabuf { fourcc: d.fourcc, width: d.width, height: d.height, objects: out_objects, planes })
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

/// One stream's decoder: a VLD configuration of a profile, its surfaces and context, and the image
/// pictures are read back through.
pub struct Session {
    /// The shared display (see [`SHARED`]), or the one that replaced it.
    display: Arc<Display>,
    config: ffi::VAConfigID,
    context: ffi::VAContextID,
    surfaces: Vec<VASurfaceID>,
    image: Option<VAImage>,
    /// Whether to read pictures through `vaDeriveImage` (cleared the first time it fails).
    derive: bool,
    /// Coded size of the surfaces (luma samples).
    size: (u32, u32),
    geometry: Geometry,
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
        match Self::on(display.clone(), profiles, size, count, geometry) {
            Ok(s) => Ok(s),
            Err(first) => {
                // The shared display may have stopped working (a GPU reset): try a new one, and
                // share that one from now on if it works. (A stream the driver does not take
                // fails on both and costs the one extra display opening.)
                let fresh = Arc::new(Display::open().map_err(|e| format!("{first}; a new display: {e}"))?);
                let s = Self::on(fresh.clone(), profiles, size, count, geometry).map_err(|_| first)?;
                replace_shared(&display, fresh);
                Ok(s)
            }
        }
    }

    fn on(display: Arc<Display>, profiles: &[ffi::VAProfile], size: (u32, u32), count: usize, geometry: Geometry) -> Result<Self, String> {
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
        let mut s = Self { display, config, context: ffi::VA_INVALID_ID, surfaces: Vec::new(), image: None, derive: true, size, geometry };
        let surfaces = s.display.create_surfaces(rt_format, size, count)?;
        s.surfaces = surfaces;
        let (w, h) = (c_int::try_from(size.0).map_err(|_| "picture too wide")?, c_int::try_from(size.1).map_err(|_| "picture too tall")?);
        let n = c_int::try_from(count).map_err(|_| "too many surfaces")?;
        let mut context = ffi::VA_INVALID_ID;
        // SAFETY: `config` and the `count` surfaces were made on this display; `context` is a live output.
        let st = unsafe { (api.create_context)(dpy, s.config, w, h, ffi::VA_PROGRESSIVE, s.surfaces.as_mut_ptr(), n, &mut context) };
        s.display.check(st, "vaCreateContext")?;
        s.context = context;
        Ok(s)
    }

    pub fn vendor(&self) -> &str {
        self.display.vendor()
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

    /// The picture in surface `index`, read through an image derived from the surface itself:
    /// the driver maps the decoded memory instead of copying it into a second image first, which
    /// is most of `vaGetImage`'s cost at 4K. An error leaves the caller to use `vaGetImage`.
    fn read_derived(&mut self, index: usize) -> Result<VideoFrame, String> {
        let derive = self.display.api.derive_image.ok_or("libva has no vaDeriveImage")?;
        let surface = *self.surfaces.get(index).ok_or("surface out of range")?;
        let api = self.display.api;
        // SAFETY: `surface` is one of this session's surfaces.
        let st = unsafe { (api.sync_surface)(self.display.dpy, surface) };
        self.display.check(st, "vaSyncSurface")?;
        let mut image = VAImage { image_id: ffi::VA_INVALID_ID, buf: ffi::VA_INVALID_ID, ..Default::default() };
        // SAFETY: `surface` was made on this display; `image` is a live output the driver fills in.
        let st = unsafe { derive(self.display.dpy, surface, &mut image) };
        self.display.check(st, "vaDeriveImage")?;
        let frame = self.read_image(&image);
        // SAFETY: the image was derived above on this display and is destroyed once; its buffer
        // has been unmapped by `read_image`.
        let st = unsafe { (api.destroy_image)(self.display.dpy, image.image_id) };
        // a derived image the driver cannot free would pile up: stop deriving (the caller falls
        // back to vaGetImage)
        self.display.check(st, "vaDestroyImage (derived image)")?;
        frame
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

/// The two planes of a mapped NV12 / P010 image.
fn biplanar_view<'a>(image: &VAImage, data: &'a [u8]) -> Result<Biplanar<'a>, String> {
    if !matches!(image.format.fourcc, ffi::VA_FOURCC_NV12 | ffi::VA_FOURCC_P010) || image.num_planes < 2 {
        return Err(format!("the read-back image is not NV12 / P010 (fourcc {:#x}, {} planes)", image.format.fourcc, image.num_planes));
    }
    let [p0, p1, _] = image.pitches;
    let luma = data.get(image.offsets[0] as usize..).ok_or("NV12 luma offset out of range")?;
    let chroma = data.get(image.offsets[1] as usize..).ok_or("NV12 chroma offset out of range")?;
    Ok(Biplanar { luma, chroma, stride: p0 as usize, chroma_stride: p1 as usize, rows: usize::from(image.height) })
}

impl Accel for Session {
    fn surfaces(&self) -> &[VASurfaceID] {
        &self.surfaces
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
        if self.derive {
            match self.read_derived(index) {
                Ok(frame) => return Ok(frame),
                Err(e) => {
                    log::info!("VA-API: reading pictures with vaGetImage ({e})");
                    self.derive = false;
                }
            }
        }
        let image = self.get_image(index)?;
        self.read_image(&image)
    }

    fn fill(&mut self, index: usize, from: Option<usize>) -> Result<(), String> {
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

/// One DMABUF object of an exported surface: its file descriptor (owned: closed on drop), its size
/// in bytes and the DRM format modifier the driver decoded the surface with.
#[derive(Debug)]
pub struct DmabufObject {
    pub fd: OwnedFd,
    pub size: u32,
    pub modifier: u64,
}

/// One plane of an exported surface: the DRM format, which object holds it, the offset inside that
/// object and the pitch in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DmabufPlane {
    pub format: u32,
    pub object: usize,
    pub offset: u32,
    pub pitch: u32,
}

/// A decoded surface as DMABUF ([`Display::export_surface`]): what a GPU importer needs to bind the
/// picture as a texture with no GPU → CPU readback. The objects' file descriptors are owned here.
#[derive(Debug)]
pub struct SurfaceDmabuf {
    /// `VA_FOURCC_*` of the whole surface (NV12, P010, …).
    pub fourcc: u32,
    pub width: u32,
    pub height: u32,
    pub objects: Vec<DmabufObject>,
    pub planes: Vec<DmabufPlane>,
}

/// Whether the loaded libva can export a surface as DMABUF (`vaExportSurfaceHandle`, libva 2.1):
/// the prerequisite of a zero-copy GPU path. Checked without opening a display.
pub fn can_export_surface() -> bool {
    matches!(api(), Ok(a) if a.export_surface_handle.is_some())
}

/// Whether this system has a working VA-API driver on a DRM render node (checked once), with the
/// driver's description, or why not.
pub fn probe() -> &'static Result<String, String> {
    static PROBE: OnceLock<Result<String, String>> = OnceLock::new();
    // the display opened to answer is the one sessions share
    PROBE.get_or_init(|| shared().map(|d| d.vendor().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The DMABUF export describes a decoded surface's planes and hands over its objects: the first
    /// half of the zero-copy path, checked on the real driver. Skips without one (and without
    /// `vaExportSurfaceHandle`).
    #[test]
    fn surface_export_describes_the_planes() {
        let Ok(vendor) = probe() else {
            eprintln!("no VA-API driver, skipping");
            return;
        };
        let Ok(d) = shared() else {
            eprintln!("no shared display, skipping");
            return;
        };
        if !can_export_surface() {
            eprintln!("{vendor}: libva without vaExportSurfaceHandle, skipping");
            return;
        }
        // 8-bit 4:2:0: NV12, two planes (luma, interleaved chroma), at least one byte per sample
        let mut s8 = d.create_surfaces(ffi::VA_RT_FORMAT_YUV420, (64, 64), 1).unwrap();
        {
            let e = d.export_surface(s8[0]).unwrap();
            assert_eq!(e.fourcc, ffi::VA_FOURCC_NV12, "{e:?}");
            assert_eq!((e.width, e.height), (64, 64));
            assert_eq!(e.planes.len(), 2, "{e:?}");
            assert!(e.planes.iter().all(|p| p.pitch >= 64), "{e:?}");
            assert!(e.objects.iter().all(|o| o.size > 0), "{e:?}");
            // the descriptors are real dmabufs: they fstat like any file
            for o in &e.objects {
                let f = File::from(o.fd.try_clone().unwrap());
                assert!(f.metadata().unwrap().len() > 0, "{e:?}");
            }
        }
        d.destroy_surfaces(&mut s8).unwrap();
        // 10-bit 4:2:0: a 10-bit format (P010 or wider), still two planes
        let mut s10 = match d.create_surfaces(ffi::VA_RT_FORMAT_YUV420_10, (64, 64), 1) {
            Ok(s) => s,
            // a driver without 10-bit 4:2:0 surfaces: the 8-bit half above is the whole test
            Err(e) if e.ends_with(&format!("({:#x})", ffi::VA_STATUS_ERROR_UNSUPPORTED_RT_FORMAT)) => {
                eprintln!("{vendor}: no 10-bit 4:2:0 surfaces ({e}), skipping the 10-bit export");
                return;
            }
            Err(e) => panic!("{vendor}: 10-bit surfaces: {e}"),
        };
        {
            let e = d.export_surface(s10[0]).unwrap();
            assert_ne!(e.fourcc, ffi::VA_FOURCC_NV12, "10-bit surface as 8-bit: {e:?}");
            assert_eq!(e.planes.len(), 2, "{e:?}");
            assert!(e.planes.iter().all(|p| p.pitch >= 128), "10-bit pitch in bytes: {e:?}");
            assert!(e.objects.iter().all(|o| o.size > 0), "{e:?}");
            eprintln!("{vendor}: 8-bit NV12 and 10-bit 0x{:08x} exported over DMABUF", e.fourcc);
        }
        d.destroy_surfaces(&mut s10).unwrap();
    }
}
