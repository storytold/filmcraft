//! Calls into Blackmagic's `libBlackmagicRawAPI.so` (Linux), `BlackmagicRawAPI.dll` (Windows) or
//! `BlackmagicRawAPI.framework` (macOS). Every API object stays in this module; the rest of
//! [`super`] sees safe types.
//!
//! One codec serves every clip in the process (the API decodes the jobs of all its clips first in,
//! first out). It is made when the first `.braw` file opens and kept, with the library, for the
//! life of the process. FilmCraft's own calls into the API are serialised by one lock. The API
//! completes jobs on its worker threads through the callback object below: those callbacks never
//! take the lock (the API may hold its own while calling back) and never unwind into the API.

use std::ffi::c_void;
#[cfg(target_os = "linux")]
use std::ffi::{CStr, CString};
#[cfg(target_os = "macos")]
use objc2_core_foundation::{CFBundle, CFRetained, CFString, CFURL, CFURLPathStyle};
use std::mem::ManuallyDrop;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use super::ffi::*;

/// The runtime library's file name: the shared library, or Blackmagic's framework bundle.
#[cfg(target_os = "linux")]
pub const LIBRARY: &str = "libBlackmagicRawAPI.so";
/// The runtime library's file name.
#[cfg(windows)]
pub const LIBRARY: &str = "BlackmagicRawAPI.dll";
/// The runtime library's bundle name.
#[cfg(target_os = "macos")]
pub const LIBRARY: &str = "BlackmagicRawAPI.framework";
/// The Blackmagic RAW SDK whose headers `ffi` transcribes: its runtime (or a later one keeping
/// these interfaces) is needed.
pub const SDK_VERSION: &str = "6.0";
/// Largest picture side accepted from the API (Blackmagic's largest sensor is 17K: 17520 wide).
const MAX_SIDE: u32 = 32_768;
/// Largest picture accepted from the API, in bytes.
const MAX_PICTURE_BYTES: usize = 1 << 30;
/// Longest wait for one frame (a 17K frame on a slow CPU takes seconds; a hung job must not hang
/// FilmCraft).
const FRAME_TIMEOUT: Duration = Duration::from_secs(60);
/// Most audio sample frames read in one call.
const MAX_AUDIO_FRAMES: u32 = 1 << 20;
/// Longest a closing clip waits for its jobs before leaking its API objects instead.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(10);

#[cfg(target_os = "linux")]
unsafe extern "C" {
    /// The C library's `free`: on Linux the API returns some strings (time codes) allocated with
    /// `malloc`.
    fn free(p: *mut c_void);
}

/// A path as the API takes it: UTF-8 on Linux, a `BSTR` on Windows, a `CFStringRef` on macOS (alive
/// while the call runs).
#[cfg(target_os = "linux")]
struct ApiPath(CString);
#[cfg(target_os = "linux")]
impl ApiPath {
    fn new(path: &Path) -> Result<Self, String> {
        use std::os::unix::ffi::OsStrExt;
        CString::new(path.as_os_str().as_bytes()).map(Self).map_err(|_| "the path has a NUL byte".into())
    }
    fn as_ptr(&self) -> BrawString {
        self.0.as_ptr()
    }
}
#[cfg(target_os = "macos")]
struct ApiPath(CFRetained<CFString>);
#[cfg(target_os = "macos")]
impl ApiPath {
    fn new(path: &Path) -> Result<Self, String> {
        let text = path.to_str().ok_or("the path is not valid UTF-8")?;
        Ok(Self(CFString::from_str(text)))
    }
    fn as_ptr(&self) -> BrawString {
        CFRetained::as_ptr(&self.0).as_ptr().cast()
    }
}
#[cfg(windows)]
struct ApiPath(windows::core::BSTR);
#[cfg(windows)]
impl ApiPath {
    fn new(path: &Path) -> Result<Self, String> {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        if wide.is_empty() || wide.contains(&0) {
            return Err("the path is empty or has a NUL character".into());
        }
        Ok(Self(windows::core::BSTR::from_wide(&wide)))
    }
    fn as_ptr(&self) -> BrawString {
        self.0.as_ptr()
    }
}

/// Read, then free, a string the API handed over to the caller: a time code on Linux (allocated
/// with `malloc`, as the SDK's samples free it) or on macOS (a `CFStringRef` the caller releases,
/// as the SDK's samples do), or on Windows any `[out] BSTR`.
///
/// # Safety
/// `s` is null or such a string.
unsafe fn take_string(s: BrawString) -> Option<String> {
    if s.is_null() {
        return None;
    }
    #[cfg(target_os = "linux")]
    {
        // SAFETY: a NUL-terminated string the caller owns (the function's contract).
        let text = unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned();
        // SAFETY: allocated by the API with malloc for the caller.
        unsafe { free(s.cast_mut().cast()) };
        Some(text)
    }
    #[cfg(windows)]
    {
        // SAFETY: a BSTR the caller owns (the function's contract); dropping it frees it.
        Some(unsafe { windows::core::BSTR::from_raw(s) }.to_string())
    }
    #[cfg(target_os = "macos")]
    {
        // `s` was non-null (checked above) and is a CFString the caller owns (the function's
        // contract).
        let s = NonNull::new(s.cast_mut().cast::<CFString>())?;
        // SAFETY: as above; dropping the retained reference releases it.
        let s = unsafe { CFRetained::from_raw(s) };
        Some(s.to_string())
    }
}

/// Read a string the API keeps on Linux, and the version on macOS (it is read without releasing it:
/// a kept string must not be freed, and a returned one leaks one string instead); on Windows every
/// `[out] BSTR` is the caller's to free.
///
/// # Safety
/// `s` is null or a string a call just returned.
unsafe fn read_kept_string(s: BrawString) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: a non-null string from the API is NUL-terminated.
        (!s.is_null()).then(|| unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned())
    }
    #[cfg(windows)]
    {
        // SAFETY: the function's contract.
        unsafe { take_string(s) }
    }
    #[cfg(target_os = "macos")]
    {
        // SAFETY: `s` is null or a live CFString the API keeps; reading it takes no reference.
        NonNull::new(s.cast_mut().cast::<CFString>()).map(|s| unsafe { &*s.as_ptr() }.to_string())
    }
}

/// How an id is passed to `QueryInterface` (by value on Linux and macOS, by reference on Windows).
#[cfg(not(windows))]
fn iid_arg(iid: &Iid) -> IidArg {
    *iid
}
#[cfg(windows)]
fn iid_arg(iid: &Iid) -> IidArg {
    iid
}

fn check(code: Hresult, call: &str) -> Result<(), String> {
    if code == S_OK { Ok(()) } else { Err(format!("{call} failed (0x{:08X})", code as u32)) }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Every API vtable: `#[repr(C)]` with the `IUnknown` entries first.
///
/// # Safety
/// The implementing type's first field must be an [`IUnknownVtbl`].
unsafe trait Vtable {}
// SAFETY: each of these is `#[repr(C)]` and starts with `unknown: IUnknownVtbl` (ffi.rs).
unsafe impl Vtable for FactoryVtbl {}
// SAFETY: as above.
unsafe impl Vtable for CodecVtbl {}
// SAFETY: as above.
unsafe impl Vtable for ConfigurationVtbl {}
// SAFETY: as above.
unsafe impl Vtable for ClipProcessingAttributesVtbl {}
// SAFETY: as above.
unsafe impl Vtable for ProcessedImageVtbl {}
// SAFETY: as above.
unsafe impl Vtable for JobVtbl {}
// SAFETY: as above.
unsafe impl Vtable for ClipAudioVtbl {}
// SAFETY: as above.
unsafe impl Vtable for FrameVtbl {}
// SAFETY: as above.
unsafe impl Vtable for ClipVtbl {}

/// An owned reference to an API object, released on drop.
struct ComPtr<V: Vtable> {
    ptr: NonNull<Object<V>>,
}

impl<V: Vtable> ComPtr<V> {
    /// Take ownership of one reference.
    ///
    /// # Safety
    /// `p` is null or an object implementing `V` on which the caller owns a reference.
    unsafe fn from_raw(p: *mut Object<V>) -> Option<Self> {
        NonNull::new(p).map(|ptr| Self { ptr })
    }
    fn this(&self) -> This {
        self.ptr.as_ptr().cast()
    }
    fn vtbl(&self) -> &V {
        // SAFETY: the object is alive while this reference is held, and starts with its vtable
        // pointer, which the API keeps valid for the object's life.
        unsafe { &*(*self.ptr.as_ptr()).vtbl }
    }
    fn unknown(&self) -> &IUnknownVtbl {
        // SAFETY: `V: Vtable` starts with an `IUnknownVtbl` (#[repr(C)]).
        unsafe { &*(self.vtbl() as *const V).cast::<IUnknownVtbl>() }
    }
    /// The object's interface `iid`, as vtable `W`.
    ///
    /// # Safety
    /// `W` must be the vtable of the interface `iid` names.
    unsafe fn query<W: Vtable>(&self, iid: Iid) -> Option<ComPtr<W>> {
        let mut out: *mut c_void = ptr::null_mut();
        // SAFETY: a live object; on success the API stores an owned reference in `out`.
        let code = unsafe { (self.unknown().query_interface)(self.this(), iid_arg(&iid), &mut out) };
        // SAFETY: `out` is the requested interface (caller's contract) with a reference we own.
        if code == S_OK { unsafe { ComPtr::from_raw(out.cast()) } } else { None }
    }
    /// Give up ownership without releasing (the API releases it, as with a submitted job).
    fn into_raw(self) -> *mut Object<V> {
        let p = self.ptr.as_ptr();
        std::mem::forget(self);
        p
    }
}

impl<V: Vtable> Drop for ComPtr<V> {
    fn drop(&mut self) {
        // SAFETY: we own one reference to a live object.
        unsafe { (self.unknown().release)(self.this()) };
    }
}

/// The loaded runtime and its process-wide codec.
pub struct Sdk {
    _runtime: Runtime,
    #[cfg(not(windows))]
    variant_clear: VariantClearFn,
    codec: ComPtr<CodecVtbl>,
    _factory: ComPtr<FactoryVtbl>,
    /// Serialises FilmCraft's calls into the API.
    calls: Mutex<()>,
}

// SAFETY: the API objects held here are only called under `calls` (or by the API's own threads);
// the codec is the API's documented way to decode several clips from one process.
unsafe impl Send for Sdk {}
// SAFETY: as above.
unsafe impl Sync for Sdk {}

static SDK: OnceLock<Result<Sdk, String>> = OnceLock::new();

/// The runtime, loaded on first use from the first of `dirs` that has it (else the system library
/// path). A failure is kept: installing the runtime takes effect at the next start.
pub fn sdk(dirs: impl FnOnce() -> Vec<PathBuf>) -> Result<&'static Sdk, String> {
    SDK.get_or_init(|| Sdk::load(&dirs())).as_ref().map_err(Clone::clone)
}

/// The loaded runtime: the shared library on Linux and Windows, Blackmagic's framework on macOS.
/// It stays loaded for the life of the process (the [`Sdk`] holds it).
#[cfg(not(target_os = "macos"))]
struct Runtime(libloading::Library);
/// The loaded runtime: the shared library on Linux and Windows, Blackmagic's framework on macOS.
/// It stays loaded for the life of the process (the [`Sdk`] holds it).
#[cfg(target_os = "macos")]
struct Runtime(CFRetained<CFBundle>);

#[cfg(not(target_os = "macos"))]
impl Runtime {
    /// Where the runtime sits inside a folder that has it.
    fn path_in(dir: &Path) -> PathBuf {
        dir.join(LIBRARY)
    }
    /// Whether `path` is the runtime's file.
    fn present(path: &Path) -> bool {
        path.is_file()
    }
    fn open(path: &Path) -> Result<Self, String> {
        open_library(path).map(Self)
    }
    /// The runtime's `CreateBlackmagicRawFactoryInstance`.
    fn create_factory(&self) -> Result<CreateFactoryFn, String> {
        // SAFETY: the symbol has this C signature in `BlackmagicRawAPI.h`; the copy stays valid
        // while the runtime is loaded, which is for the life of the process.
        unsafe { self.0.get(b"CreateBlackmagicRawFactoryInstance\0").map(|f| *f).map_err(|e| e.to_string()) }
    }
    /// The runtime's `VariantClear` (Windows uses the system's).
    #[cfg(not(windows))]
    fn variant_clear(&self) -> Result<VariantClearFn, String> {
        // SAFETY: as above.
        unsafe { self.0.get(b"VariantClear\0").map(|f| *f).map_err(|e| e.to_string()) }
    }
}

#[cfg(target_os = "macos")]
impl Runtime {
    /// Where the runtime sits inside a folder that has it: the folder may be the framework itself or
    /// hold it (the SDK's own loader takes the folder holding it).
    fn path_in(dir: &Path) -> PathBuf {
        if dir.extension().is_some_and(|e| e == "framework") { dir.to_path_buf() } else { dir.join(LIBRARY) }
    }
    /// Whether `path` is the framework bundle.
    fn present(path: &Path) -> bool {
        path.is_dir()
    }
    fn open(path: &Path) -> Result<Self, String> {
        let text = path.to_str().ok_or("the path is not valid UTF-8")?;
        let url = CFURL::with_file_system_path(None, Some(&CFString::from_str(text)), CFURLPathStyle::CFURLPOSIXPathStyle, true)
            .ok_or("the path is not a folder")?;
        CFBundle::new(None, Some(&url)).map(Self).ok_or_else(|| "not a loadable framework".into())
    }
    /// A function the framework exports.
    fn symbol(&self, name: &str) -> Option<*mut c_void> {
        let name = CFString::from_str(name);
        NonNull::new(self.0.function_pointer_for_name(Some(&name))).map(NonNull::as_ptr)
    }
    /// The framework's `CreateBlackmagicRawFactoryInstance`.
    fn create_factory(&self) -> Result<CreateFactoryFn, String> {
        // SAFETY: the function has this C signature in `BlackmagicRawAPI.h`; the bundle stays loaded
        // (and held) for the life of the process.
        self.symbol("CreateBlackmagicRawFactoryInstance").map(|f| unsafe { std::mem::transmute(f) }).ok_or_else(|| "it exports no factory".into())
    }
    /// The framework's `VariantClear`.
    fn variant_clear(&self) -> Result<VariantClearFn, String> {
        // SAFETY: as above.
        self.symbol("VariantClear").map(|f| unsafe { std::mem::transmute(f) }).ok_or_else(|| "it exports no VariantClear".into())
    }
}

/// Where the runtime is called from by name when no folder holds it: the system's own search (its
/// installer puts it on the library path). macOS only ever finds it by path.
fn system_runtime() -> Option<PathBuf> {
    #[cfg(not(target_os = "macos"))]
    {
        Some(PathBuf::from(LIBRARY))
    }
    #[cfg(target_os = "macos")]
    {
        None
    }
}

#[cfg(target_os = "linux")]
fn open_library(path: &Path) -> Result<libloading::Library, String> {
    use libloading::os::unix::{Library, RTLD_GLOBAL, RTLD_NOW};
    // SAFETY: loading Blackmagic's runtime runs its initialisers, which only set up the library.
    // RTLD_GLOBAL as the SDK's own loader does: the runtime loads its decoder plug-ins
    // (libDecoderCUDA.so, libInstructionSetServicesAVX2.so…) from its folder, and they link back to it.
    unsafe { Library::open(Some(path), RTLD_NOW | RTLD_GLOBAL) }.map(Into::into).map_err(|e| e.to_string())
}

#[cfg(windows)]
fn open_library(path: &Path) -> Result<libloading::Library, String> {
    // SAFETY: loading Blackmagic's runtime runs its DllMain, which only sets up the library. Plain
    // LoadLibraryEx flags, as the SDK's own loader uses.
    unsafe { libloading::Library::new(path) }.map_err(|e| e.to_string())
}

impl Sdk {
    fn load(dirs: &[PathBuf]) -> Result<Self, String> {
        let mut tried = Vec::new();
        let mut found = None;
        for dir in dirs {
            let path = Runtime::path_in(dir);
            if !Runtime::present(&path) {
                continue;
            }
            match Runtime::open(&path) {
                Ok(l) => {
                    found = Some((l, path));
                    break;
                }
                Err(e) => tried.push(format!("{}: {e}", path.display())),
            }
        }
        let (runtime, path) = match found {
            Some(f) => f,
            None => match system_runtime() {
                Some(name) => match Runtime::open(&name) {
                    Ok(runtime) => (runtime, name),
                    Err(e) if tried.is_empty() => return Err(format!("the Blackmagic RAW runtime ({LIBRARY}) is not installed ({e})")),
                    Err(_) => return Err(format!("the Blackmagic RAW runtime could not be loaded: {}", tried.join("; "))),
                },
                None if tried.is_empty() => return Err(format!("the Blackmagic RAW runtime ({LIBRARY}) is not installed")),
                None => return Err(format!("the Blackmagic RAW runtime could not be loaded: {}", tried.join("; "))),
            },
        };
        let create_factory = runtime.create_factory()?;
        #[cfg(not(windows))]
        let variant_clear = runtime.variant_clear()?;
        // SAFETY: returns a new factory with one reference for us, or null.
        let factory = unsafe { ComPtr::from_raw(create_factory()) }.ok_or("the Blackmagic RAW runtime made no factory")?;
        // Interfaces are only used once the runtime confirms their ids: a runtime built from other
        // headers (an older Blackmagic RAW, such as 5.x) lays its vtables out differently and changes
        // their ids, so it is refused here instead of being called at the wrong slots.
        let incompatible = || {
            format!("the Blackmagic RAW runtime at {} is a version FilmCraft cannot use (FilmCraft is built for Blackmagic RAW {SDK_VERSION})", path.display())
        };
        // SAFETY: IID_IBlackmagicRawFactory names FactoryVtbl's interface.
        let factory = unsafe { factory.query::<FactoryVtbl>(IID_IBlackmagicRawFactory) }.ok_or_else(incompatible)?;
        let mut codec = ptr::null_mut();
        // SAFETY: a live factory; on success `codec` holds an owned reference.
        check(unsafe { (factory.vtbl().create_codec)(factory.this(), &mut codec) }, "CreateCodec")?;
        // SAFETY: CreateCodec returned an object with a reference for us; until the id check below
        // only its IUnknown entries are used.
        let codec = unsafe { ComPtr::<CodecVtbl>::from_raw(codec) }.ok_or("CreateCodec returned no codec")?;
        // SAFETY: IID_IBlackmagicRaw names CodecVtbl's interface.
        let codec = unsafe { codec.query::<CodecVtbl>(IID_IBlackmagicRaw) }.ok_or_else(incompatible)?;
        // SAFETY: IID_IBlackmagicRawConfiguration names ConfigurationVtbl's interface.
        let version = match unsafe { codec.query::<ConfigurationVtbl>(IID_IBlackmagicRawConfiguration) } {
            Some(config) => {
                // CPU decoding: GPU pipelines need a device and context FilmCraft does not set up.
                // SAFETY: a live configuration, read before the first OpenClip as the API requires.
                let _ = unsafe { (config.vtbl().set_pipeline)(config.this(), blackmagicRawPipelineCPU, ptr::null_mut(), ptr::null_mut()) };
                let mut v: BrawString = ptr::null();
                // SAFETY: a live configuration and a valid out-pointer.
                let code = unsafe { (config.vtbl().get_version)(config.this(), &mut v) };
                // SAFETY: what a successful call returned.
                if code == S_OK { unsafe { read_kept_string(v) } } else { None }
            }
            None => None,
        };
        // SAFETY: CALLBACK is a static object implementing IBlackmagicRawCallback; the API calls it
        // on its threads, and it outlives the codec (both live for the whole process).
        check(unsafe { (codec.vtbl().set_callback)(codec.this(), ptr::addr_of!(CALLBACK.0).cast_mut()) }, "SetCallback")?;
        log::info!("Blackmagic RAW runtime {} loaded from {}", version.as_deref().unwrap_or("(unknown version)"), path.display());
        Ok(Self {
            _runtime: runtime,
            #[cfg(not(windows))]
            variant_clear,
            codec,
            _factory: factory,
            calls: Mutex::new(()),
        })
    }

    /// Open a `.braw` file.
    pub fn open(&'static self, path: &Path) -> Result<(Clip, ClipInfo), String> {
        let name = ApiPath::new(path)?;
        let _calls = lock(&self.calls);
        let mut raw = ptr::null_mut();
        // SAFETY: a live codec and a NUL-terminated path; on success `raw` holds an owned reference.
        check(unsafe { (self.codec.vtbl().open_clip)(self.codec.this(), name.as_ptr(), &mut raw) }, "OpenClip")?;
        // SAFETY: OpenClip returned an object with a reference for us; until the id check below only
        // its IUnknown entries are used.
        let clip = unsafe { ComPtr::<ClipVtbl>::from_raw(raw) }.ok_or("OpenClip returned no clip")?;
        // SAFETY: IID_IBlackmagicRawClip names ClipVtbl's interface.
        let clip = unsafe { clip.query::<ClipVtbl>(IID_IBlackmagicRawClip) }.ok_or("the clip is not an IBlackmagicRawClip")?;
        let v = clip.vtbl();
        let (mut width, mut height, mut rate, mut frames) = (0u32, 0u32, 0f32, 0u64);
        // SAFETY: live clip, valid out-pointers.
        unsafe {
            check((v.get_width)(clip.this(), &mut width), "GetWidth")?;
            check((v.get_height)(clip.this(), &mut height), "GetHeight")?;
            check((v.get_frame_rate)(clip.this(), &mut rate), "GetFrameRate")?;
            check((v.get_frame_count)(clip.this(), &mut frames), "GetFrameCount")?;
        }
        let timecode = (frames > 0).then(|| self.timecode(&clip, 0)).flatten();
        let (gamma, gamut) = self.look(&clip);
        // SAFETY: IID_IBlackmagicRawClipAudio names ClipAudioVtbl's interface.
        let audio_object = unsafe { clip.query::<ClipAudioVtbl>(IID_IBlackmagicRawClipAudio) };
        let audio = audio_object.as_ref().and_then(audio_info);
        let audio_object = if audio.is_some() { audio_object } else { None };
        let info = ClipInfo { width, height, frame_rate: rate, frame_count: frames, start_timecode: timecode, gamma, gamut, audio };
        let clip = Clip { sdk: self, clip: ManuallyDrop::new(clip), audio: ManuallyDrop::new(audio_object), in_flight: Arc::new(AtomicUsize::new(0)) };
        Ok((clip, info))
    }

    /// The time code of frame `index` (the caller holds `calls`).
    fn timecode(&self, clip: &ComPtr<ClipVtbl>, index: u64) -> Option<String> {
        let mut s: BrawString = ptr::null();
        // SAFETY: live clip; on success `s` is a string handed over to the caller.
        let code = unsafe { (clip.vtbl().get_timecode_for_frame)(clip.this(), index, &mut s) };
        // SAFETY: as above.
        if code == S_OK { unsafe { take_string(s) } } else { None }
    }

    /// The string `v` holds, if any, clearing `v`.
    ///
    /// # Safety
    /// `v` is empty or was filled by the API.
    unsafe fn variant_string(&self, v: &mut Variant) -> Option<String> {
        #[cfg(not(windows))]
        {
            let text = if v.vt == blackmagicRawVariantTypeString {
                // SAFETY: a string variant's union holds its string pointer, NUL-terminated or null.
                unsafe { read_kept_string(v.value.bstr_val) }
            } else {
                None
            };
            // SAFETY: frees whatever the call stored in the variant.
            unsafe { (self.variant_clear)(v) };
            text
        }
        #[cfg(windows)]
        {
            // Strings are the only owned values the attributes FilmCraft reads hold (no arrays).
            if v.vt != blackmagicRawVariantTypeString {
                return None;
            }
            v.vt = blackmagicRawVariantTypeEmpty;
            // SAFETY: a VT_BSTR variant's BSTR belongs to the caller; taking it clears the variant.
            unsafe { take_string(v.value.bstr_val) }
        }
    }

    /// The clip's gamma and gamut as recorded (what frames are processed with), for display.
    fn look(&self, clip: &ComPtr<ClipVtbl>) -> (Option<String>, Option<String>) {
        let mut raw = ptr::null_mut();
        // SAFETY: live clip; on success `raw` holds an owned reference.
        if unsafe { (clip.vtbl().clone_clip_processing_attributes)(clip.this(), &mut raw) } != S_OK {
            return (None, None);
        }
        // SAFETY: the call returned an IBlackmagicRawClipProcessingAttributes with a reference for us.
        let Some(attrs) = (unsafe { ComPtr::from_raw(raw) }) else { return (None, None) };
        let get = |attribute: u32| -> Option<String> {
            let mut v = Variant::empty();
            // SAFETY: live attributes and a valid Variant to fill.
            let code = unsafe { (attrs.vtbl().get_clip_attribute)(attrs.this(), attribute, &mut v) };
            // SAFETY: the variant a successful call filled (or the empty one).
            unsafe { self.variant_string(&mut v) }.filter(|_| code == S_OK)
        };
        (get(blackmagicRawClipProcessingAttributeGamma), get(blackmagicRawClipProcessingAttributeGamut))
    }
}

fn audio_info(audio: &ComPtr<ClipAudioVtbl>) -> Option<AudioInfo> {
    let v = audio.vtbl();
    let (mut format, mut bits, mut channels, mut rate, mut samples) = (0u32, 0u32, 0u32, 0u32, 0u64);
    // SAFETY: live clip audio, valid out-pointers.
    let ok = unsafe {
        (v.get_audio_format)(audio.this(), &mut format) == S_OK
            && (v.get_audio_bit_depth)(audio.this(), &mut bits) == S_OK
            && (v.get_audio_channel_count)(audio.this(), &mut channels) == S_OK
            && (v.get_audio_sample_rate)(audio.this(), &mut rate) == S_OK
            && (v.get_audio_sample_count)(audio.this(), &mut samples) == S_OK
    };
    let usable = ok
        && format == blackmagicRawAudioFormatPCMLittleEndian
        && matches!(bits, 16 | 24 | 32)
        && (1..=filmcraft_media::MAX_AUDIO_STREAMS as u32).contains(&channels)
        && rate > 0
        && samples > 0;
    usable.then_some(AudioInfo { sample_rate: rate, channels, bits, sample_count: samples })
}

/// What a clip reports when it opens.
#[derive(Clone, Debug)]
pub struct ClipInfo {
    pub width: u32,
    pub height: u32,
    pub frame_rate: f32,
    pub frame_count: u64,
    /// The first frame's time code ("HH:MM:SS:FF").
    pub start_timecode: Option<String>,
    pub gamma: Option<String>,
    pub gamut: Option<String>,
    pub audio: Option<AudioInfo>,
}

/// A clip's PCM audio.
#[derive(Clone, Copy, Debug)]
pub struct AudioInfo {
    pub sample_rate: u32,
    pub channels: u32,
    pub bits: u32,
    pub sample_count: u64,
}

/// A processed frame: RGBA, 8 bits per channel, rows packed.
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// An open `.braw` file.
pub struct Clip {
    sdk: &'static Sdk,
    clip: ManuallyDrop<ComPtr<ClipVtbl>>,
    audio: ManuallyDrop<Option<ComPtr<ClipAudioVtbl>>>,
    /// Frame requests the API still holds.
    in_flight: Arc<AtomicUsize>,
}

// SAFETY: the clip's API objects are only called under the SDK's `calls` lock.
unsafe impl Send for Clip {}
// SAFETY: as above.
unsafe impl Sync for Clip {}

impl Clip {
    /// Read, decode and process frame `index` at resolution `scale` (a
    /// `blackmagicRawResolutionScale*` value), waiting for the API's worker threads.
    pub fn read_frame(&self, index: u64, scale: u32) -> Result<Picture, String> {
        self.in_flight.fetch_add(1, Ordering::SeqCst);
        let request = Arc::new(Request { scale, result: Mutex::new(None), done: Condvar::new(), _in_flight: InFlight(self.in_flight.clone()) });
        {
            let _calls = lock(&self.sdk.calls);
            let mut raw = ptr::null_mut();
            // SAFETY: live clip; on success `raw` holds an owned reference to a new job.
            check(unsafe { (self.clip.vtbl().create_job_read_frame)(self.clip.this(), index, &mut raw) }, "CreateJobReadFrame")?;
            // SAFETY: the call returned an IBlackmagicRawJob with a reference for us.
            let job = unsafe { ComPtr::from_raw(raw) }.ok_or("CreateJobReadFrame returned no job")?;
            // SAFETY: the job carries this reference to the request until a callback takes it back.
            unsafe { submit(job, &request) }?;
        }
        request.wait(FRAME_TIMEOUT)
    }

    /// Up to `frames` sample frames from sample frame `first`, interleaved, as floats in [-1, 1].
    pub fn read_audio(&self, first: u64, frames: u32, info: &AudioInfo) -> Result<Vec<f32>, String> {
        let audio = (*self.audio).as_ref().ok_or("the clip has no audio")?;
        let bytes_per_sample = (info.bits / 8) as usize;
        let frame_bytes = bytes_per_sample.checked_mul(info.channels as usize).filter(|&b| b > 0).ok_or("bad audio format")?;
        let frames = frames.min(MAX_AUDIO_FRAMES);
        let size = (frames as usize).checked_mul(frame_bytes).ok_or("audio read too large")?;
        let size_u32 = u32::try_from(size).map_err(|_| "audio read too large")?;
        let first = i64::try_from(first).map_err(|_| "audio position out of range")?;
        let mut buf = vec![0u8; size];
        let (mut read, mut bytes) = (0u32, 0u32);
        {
            let _calls = lock(&self.sdk.calls);
            // SAFETY: live clip audio; `buf` has `size_u32` writable bytes for at most `frames` frames.
            let code = unsafe { (audio.vtbl().get_audio_samples)(audio.this(), first, buf.as_mut_ptr().cast(), size_u32, frames, &mut read, &mut bytes) };
            check(code, "GetAudioSamples")?;
        }
        let bytes = (bytes as usize).min(size).min((read as usize).saturating_mul(frame_bytes));
        let pcm = buf.get(..bytes).ok_or("audio read past its buffer")?;
        Ok(match info.bits {
            16 => pcm.chunks_exact(2).map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32_768.0).collect(),
            24 => pcm.chunks_exact(3).map(|b| (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0).collect(),
            _ => pcm.chunks_exact(4).map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0).collect(),
        })
    }
}

impl Drop for Clip {
    fn drop(&mut self) {
        // A job that outlived its wait (a timeout) may still use the clip: wait for it, and if it
        // never finishes leak the clip rather than release it under the API.
        let deadline = Instant::now() + CLOSE_TIMEOUT;
        while self.in_flight.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        if self.in_flight.load(Ordering::SeqCst) > 0 {
            log::warn!("Blackmagic RAW: a frame job never finished; leaking its clip");
            return;
        }
        let _calls = lock(&self.sdk.calls);
        // SAFETY: dropped once, here; no job holds the clip any more.
        unsafe {
            ManuallyDrop::drop(&mut self.audio);
            ManuallyDrop::drop(&mut self.clip);
        }
    }
}

/// Counts a request in its clip's `in_flight` until dropped.
struct InFlight(Arc<AtomicUsize>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// One frame request, shared between the waiting thread and the job carrying it.
struct Request {
    scale: u32,
    result: Mutex<Option<Result<Picture, String>>>,
    done: Condvar,
    _in_flight: InFlight,
}

impl Request {
    /// Record the outcome (the first one wins) and wake the waiter.
    fn finish(&self, r: Result<Picture, String>) {
        let mut g = lock(&self.result);
        if g.is_none() {
            *g = Some(r);
            self.done.notify_all();
        }
    }
    fn wait(&self, timeout: Duration) -> Result<Picture, String> {
        let deadline = Instant::now() + timeout;
        let mut g = lock(&self.result);
        loop {
            if let Some(r) = g.take() {
                return r;
            }
            let now = Instant::now();
            if now >= deadline {
                return Err("timed out waiting for the Blackmagic RAW decoder".into());
            }
            g = self.done.wait_timeout(g, deadline - now).unwrap_or_else(PoisonError::into_inner).0;
        }
    }
}

/// Attach a reference to `request` to `job` and submit it; on success the job belongs to the API,
/// whose callback releases it.
///
/// # Safety
/// `job` is a live job whose completion reaches [`CALLBACK`].
unsafe fn submit(job: ComPtr<JobVtbl>, request: &Arc<Request>) -> Result<(), String> {
    let user = Arc::into_raw(request.clone()).cast_mut().cast::<c_void>();
    // SAFETY: a live job; `user` is ours until the job is submitted.
    let code = unsafe { (job.vtbl().set_user_data)(job.this(), user) };
    let code = if code == S_OK {
        // SAFETY: as above.
        unsafe { (job.vtbl().submit)(job.this()) }
    } else {
        code
    };
    if code != S_OK {
        // SAFETY: the job was not submitted, so no callback will take this reference back.
        drop(unsafe { Arc::from_raw(user.cast_const().cast::<Request>()) });
        return check(code, "submitting a job");
    }
    let _ = job.into_raw();
    Ok(())
}

/// Take back the request reference `job` carries.
///
/// # Safety
/// `job` is a live job submitted by [`submit`] whose reference has not been taken yet.
unsafe fn take_request(job: &ComPtr<JobVtbl>) -> Option<Arc<Request>> {
    let mut user: *mut c_void = ptr::null_mut();
    // SAFETY: a live job.
    if unsafe { (job.vtbl().get_user_data)(job.this(), &mut user) } != S_OK || user.is_null() {
        return None;
    }
    // SAFETY: a live job; clearing makes a second take impossible.
    unsafe { (job.vtbl().set_user_data)(job.this(), ptr::null_mut()) };
    // SAFETY: `user` came from Arc::into_raw in `submit`, and is taken once.
    Some(unsafe { Arc::from_raw(user.cast_const().cast::<Request>()) })
}

/// The callback object passed to `SetCallback`: a static, so it outlives the codec.
#[repr(transparent)]
struct CallbackObject(Object<CallbackVtbl>);

// SAFETY: immutable; its vtable pointer points at a static.
unsafe impl Sync for CallbackObject {}

static CALLBACK_VTBL: CallbackVtbl = CallbackVtbl {
    unknown: IUnknownVtbl { query_interface: cb_query_interface, add_ref: cb_add_ref, release: cb_release },
    read_complete: cb_read_complete,
    read_audio_complete: cb_read_audio_complete,
    decode_complete: cb_decode_complete,
    process_complete: cb_process_complete,
    trim_progress: cb_trim_progress,
    trim_complete: cb_job_result,
    sidecar_metadata_parse_warning: cb_sidecar,
    sidecar_metadata_parse_error: cb_sidecar,
    prepare_pipeline_complete: cb_prepare_pipeline_complete,
    #[cfg(not(windows))]
    destructor: cb_destructor,
    #[cfg(not(windows))]
    deleting_destructor: cb_destructor,
};

static CALLBACK: CallbackObject = CallbackObject(Object { vtbl: &CALLBACK_VTBL });

unsafe extern "system" fn cb_query_interface(this: This, iid: IidArg, out: *mut *mut c_void) -> Hresult {
    #[cfg(windows)]
    // SAFETY: Windows passes the id by reference; null is refused.
    let Some(&iid) = (unsafe { iid.as_ref() }) else { return E_NOINTERFACE };
    if out.is_null() {
        return E_NOINTERFACE;
    }
    let ours = iid == IID_IUnknown || iid == IID_IBlackmagicRawCallback;
    // SAFETY: the API passes a writable out-pointer.
    unsafe { *out = if ours { this } else { ptr::null_mut() } };
    if ours { S_OK } else { E_NOINTERFACE }
}

// The callback object is static: references need no counting.
unsafe extern "system" fn cb_add_ref(_this: This) -> Ulong {
    1
}
unsafe extern "system" fn cb_release(_this: This) -> Ulong {
    1
}
#[cfg(not(windows))]
unsafe extern "system" fn cb_destructor(_this: This) {}
unsafe extern "system" fn cb_trim_progress(_this: This, _job: *mut Object<JobVtbl>, _progress: f32) {}
unsafe extern "system" fn cb_job_result(_this: This, _job: *mut Object<JobVtbl>, _result: Hresult) {}
unsafe extern "system" fn cb_read_audio_complete(_this: This, _job: *mut Object<JobVtbl>, _result: Hresult, _buffer: *mut c_void) {}
unsafe extern "system" fn cb_sidecar(_this: This, _clip: *mut c_void, _file: BrawString, _line: u32, _info: BrawString) {}
unsafe extern "system" fn cb_prepare_pipeline_complete(_this: This, _user: *mut c_void, _result: Hresult) {}

/// A frame was read: decode and process it, handing the request on to the new job.
unsafe extern "system" fn cb_read_complete(_this: This, job: *mut Object<JobVtbl>, result: Hresult, frame: *mut Object<FrameVtbl>) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the API hands over the read job's reference (CreateJobReadFrame's), which is
        // released here as the SDK's samples do.
        let Some(job) = (unsafe { ComPtr::from_raw(job) }) else { return };
        // SAFETY: a job FilmCraft submitted.
        let Some(request) = (unsafe { take_request(&job) }) else { return };
        // SAFETY: `frame` is the API's frame for this job, alive during the callback.
        if let Err(e) = unsafe { process(&request, result, frame) } {
            request.finish(Err(e));
        }
    }));
}

/// # Safety
/// `frame` is null or the API's live frame.
unsafe fn process(request: &Arc<Request>, result: Hresult, frame: *mut Object<FrameVtbl>) -> Result<(), String> {
    check(result, "reading the frame")?;
    let frame = NonNull::new(frame).ok_or("the frame read has no frame")?;
    let this: This = frame.as_ptr().cast();
    // SAFETY: a live frame (the API's, borrowed for the callback).
    let v = unsafe { &*(*frame.as_ptr()).vtbl };
    let mut raw = ptr::null_mut();
    // SAFETY: a live frame; on success `raw` holds an owned reference to a new job.
    unsafe {
        check((v.set_resolution_scale)(this, request.scale), "SetResolutionScale")?;
        check((v.set_resource_format)(this, blackmagicRawResourceFormatRGBAU8), "SetResourceFormat")?;
        check((v.create_job_decode_and_process_frame)(this, ptr::null_mut(), ptr::null_mut(), &mut raw), "CreateJobDecodeAndProcessFrame")?;
    }
    // SAFETY: the call returned an IBlackmagicRawJob with a reference for us.
    let job = unsafe { ComPtr::from_raw(raw) }.ok_or("CreateJobDecodeAndProcessFrame returned no job")?;
    // SAFETY: the job completes through CALLBACK (the codec's callback).
    unsafe { submit(job, request) }
}

/// Only called for manual decoder flows; a failure still ends the request.
unsafe extern "system" fn cb_decode_complete(_this: This, job: *mut Object<JobVtbl>, result: Hresult) {
    if result == S_OK || job.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let mut user: *mut c_void = ptr::null_mut();
        // SAFETY: the API's live job, borrowed for the callback (not ours to release).
        let Some(v) = (unsafe { (*job).vtbl.as_ref() }) else { return };
        // SAFETY: as above.
        if unsafe { (v.get_user_data)(job.cast(), &mut user) } != S_OK || user.is_null() {
            return;
        }
        // SAFETY: the job's request reference, borrowed (ProcessComplete takes it back).
        let request = ManuallyDrop::new(unsafe { Arc::from_raw(user.cast_const().cast::<Request>()) });
        if let Err(e) = check(result, "decoding the frame") {
            request.finish(Err(e));
        }
    }));
}

/// A frame was processed: copy the picture out (it is only valid during this call).
unsafe extern "system" fn cb_process_complete(_this: This, job: *mut Object<JobVtbl>, result: Hresult, image: *mut Object<ProcessedImageVtbl>) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: the API hands over the decode job's reference, released here as its samples do.
        let Some(job) = (unsafe { ComPtr::from_raw(job) }) else { return };
        // SAFETY: a job FilmCraft submitted.
        let Some(request) = (unsafe { take_request(&job) }) else { return };
        // SAFETY: `image` is null or the API's live image for this callback.
        let picture = check(result, "processing the frame").and_then(|()| unsafe { copy_picture(image) });
        request.finish(picture);
    }));
}

/// # Safety
/// `image` is null or the API's live processed image.
unsafe fn copy_picture(image: *mut Object<ProcessedImageVtbl>) -> Result<Picture, String> {
    let image = NonNull::new(image).ok_or("the processed frame has no image")?;
    let this: This = image.as_ptr().cast();
    // SAFETY: a live image (the API's, borrowed for the callback).
    let v = unsafe { &*(*image.as_ptr()).vtbl };
    let (mut width, mut height, mut kind, mut format, mut size) = (0u32, 0u32, 0u32, 0u32, 0u32);
    let mut resource: *mut c_void = ptr::null_mut();
    // SAFETY: a live image, valid out-pointers.
    unsafe {
        check((v.get_width)(this, &mut width), "GetWidth")?;
        check((v.get_height)(this, &mut height), "GetHeight")?;
        check((v.get_resource_type)(this, &mut kind), "GetResourceType")?;
        check((v.get_resource_format)(this, &mut format), "GetResourceFormat")?;
        check((v.get_resource_size_bytes)(this, &mut size), "GetResourceSizeBytes")?;
        check((v.get_resource)(this, &mut resource), "GetResource")?;
    }
    if kind != blackmagicRawResourceTypeBufferCPU || format != blackmagicRawResourceFormatRGBAU8 {
        return Err(format!("unexpected processed image (type 0x{kind:08X}, format 0x{format:08X})"));
    }
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return Err(format!("unexpected processed image size {width}x{height}"));
    }
    let bytes =
        (width as usize).checked_mul(height as usize).and_then(|p| p.checked_mul(4)).filter(|&b| b <= MAX_PICTURE_BYTES).ok_or("processed image too large")?;
    if (size as usize) < bytes || resource.is_null() {
        return Err(format!("processed image buffer is {size} bytes, {width}x{height} RGBA needs {bytes}"));
    }
    // SAFETY: a CPU buffer of at least `bytes` bytes, valid during the callback.
    let rgba = unsafe { std::slice::from_raw_parts(resource.cast::<u8>(), bytes) }.to_vec();
    Ok(Picture { width, height, rgba })
}
