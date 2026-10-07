//! The Media Foundation side of the Windows decoder: finding a Direct3D-aware H.264 / HEVC decoder
//! MFT, handing it the shared device manager so it decodes with DXVA, and driving it
//! (`ProcessInput` / `ProcessOutput`, drain, flush) at the level of single access units.
//!
//! FFI module (docs/adr/0001-platform-ffi.md): every `unsafe` block has a `// SAFETY:` comment,
//! the public items are safe and return `Result<_, String>`.
//!
//! `mfplat.dll` is loaded at run time instead of linked: Windows "N" editions ship without the
//! Media Feature Pack, and FilmCraft must still start (and decode in software) on them.

use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::sync::OnceLock;

use filmcraft_codecs::hw::NalCodec;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D11::ID3D11Device;
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFDXGIDeviceManager, IMFMediaBuffer, IMFMediaType, IMFSample, IMFTransform, MF_E_NO_MORE_TYPES, MF_E_NOTACCEPTING,
    MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MPEG2_PROFILE, MF_MT_SUBTYPE,
    MF_SA_D3D11_AWARE, MF_TRANSFORM_ASYNC, MF_VERSION, MFMediaType_Video, MFT_CATEGORY_VIDEO_DECODER, MFT_ENUM_FLAG, MFT_ENUM_FLAG_ASYNCMFT,
    MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT, MFT_FRIENDLY_NAME_Attribute, MFT_MESSAGE_COMMAND_DRAIN,
    MFT_MESSAGE_COMMAND_FLUSH, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER,
    MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO, MFVideoFormat_H264, MFVideoFormat_HEVC, MFVideoFormat_NV12, MFVideoFormat_P010,
    MFVideoInterlace_Progressive,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
use windows::core::{GUID, HRESULT, Interface, PCSTR, w};

use super::gpu::{Gpu, SurfaceFormat, ensure_com};

type StartupFn = unsafe extern "system" fn(u32, u32) -> HRESULT;
type CreateFn = unsafe extern "system" fn(*mut *mut c_void) -> HRESULT;
type CreateBufferFn = unsafe extern "system" fn(u32, *mut *mut c_void) -> HRESULT;
type CreateManagerFn = unsafe extern "system" fn(*mut u32, *mut *mut c_void) -> HRESULT;
type EnumFn = unsafe extern "system" fn(GUID, u32, *const MFT_REGISTER_TYPE_INFO, *const MFT_REGISTER_TYPE_INFO, *mut *mut *mut c_void, *mut u32) -> HRESULT;

/// The few `mfplat.dll` entry points the backend uses, resolved at run time.
pub struct MfApi {
    create_media_type: CreateFn,
    create_sample: CreateFn,
    create_memory_buffer: CreateBufferFn,
    create_manager: CreateManagerFn,
    enum_transforms: EnumFn,
}

/// The exported function `name` of `module` as a function pointer of type `F`.
///
/// # Safety
/// `F` must be the `extern "system" fn` type that `name` really has.
unsafe fn symbol<F: Copy>(module: HMODULE, name: PCSTR) -> Result<F, String> {
    if std::mem::size_of::<F>() != std::mem::size_of::<usize>() {
        return Err("not a function pointer type".into());
    }
    // SAFETY: `module` is a loaded module handle and `name` a NUL-terminated string literal.
    let p = unsafe { GetProcAddress(module, name) }.ok_or_else(|| "mfplat.dll lacks an expected function".to_string())?;
    // SAFETY: `F` is pointer-sized (checked above) and, by this function's contract, the type of
    // the exported function `p` points to.
    Ok(unsafe { std::mem::transmute_copy::<_, F>(&p) })
}

/// Media Foundation, loaded and started once for the process (never shut down: decoders come and
/// go, and the threads of its worker queues end with the process).
pub fn api() -> Result<&'static MfApi, String> {
    static API: OnceLock<Result<MfApi, String>> = OnceLock::new();
    API.get_or_init(load).as_ref().map_err(Clone::clone)
}

fn load() -> Result<MfApi, String> {
    ensure_com()?;
    // SAFETY: loads a system DLL by name from the system directory only.
    let module =
        unsafe { LoadLibraryExW(w!("mfplat.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }.map_err(|_| "Media Foundation is not installed".to_string())?;
    // SAFETY: each symbol is looked up with the signature Microsoft documents for it
    // (MFStartup, MFCreateMediaType, MFCreateSample, MFCreateMemoryBuffer,
    // MFCreateDXGIDeviceManager, MFTEnumEx).
    let (startup, api) = unsafe {
        (
            symbol::<StartupFn>(module, PCSTR(c"MFStartup".as_ptr().cast()))?,
            MfApi {
                create_media_type: symbol(module, PCSTR(c"MFCreateMediaType".as_ptr().cast()))?,
                create_sample: symbol(module, PCSTR(c"MFCreateSample".as_ptr().cast()))?,
                create_memory_buffer: symbol(module, PCSTR(c"MFCreateMemoryBuffer".as_ptr().cast()))?,
                create_manager: symbol(module, PCSTR(c"MFCreateDXGIDeviceManager".as_ptr().cast()))?,
                enum_transforms: symbol(module, PCSTR(c"MFTEnumEx".as_ptr().cast()))?,
            },
        )
    };
    // SAFETY: MFSTARTUP_FULL (0) with the version this binding was built for.
    let hr = unsafe { startup(MF_VERSION, 0) };
    if hr.is_err() {
        return Err(format!("MFStartup failed ({hr:?})"));
    }
    Ok(api)
}

/// An out-pointer COM creation function's result as an interface.
fn made<T: Interface>(hr: HRESULT, raw: *mut c_void, what: &str) -> Result<T, String> {
    if hr.is_err() || raw.is_null() {
        return Err(format!("{what} failed ({hr:?})"));
    }
    // SAFETY: the creation function succeeded, so `raw` is a new reference to an object of type
    // `T` that this takes ownership of.
    Ok(unsafe { T::from_raw(raw) })
}

impl MfApi {
    pub(super) fn create_device_manager(&self, device: &ID3D11Device) -> Result<IMFDXGIDeviceManager, String> {
        let (mut token, mut raw) = (0u32, std::ptr::null_mut());
        // SAFETY: both out-pointers refer to live locals.
        let hr = unsafe { (self.create_manager)(&mut token, &mut raw) };
        let manager: IMFDXGIDeviceManager = made(hr, raw, "MFCreateDXGIDeviceManager")?;
        // SAFETY: a plain COM call with the token the manager just returned.
        unsafe { manager.ResetDevice(device, token) }.map_err(|e| format!("IMFDXGIDeviceManager::ResetDevice failed: {e}"))?;
        Ok(manager)
    }

    fn media_type(&self) -> Result<IMFMediaType, String> {
        let mut raw = std::ptr::null_mut();
        // SAFETY: the out-pointer refers to a live local.
        let hr = unsafe { (self.create_media_type)(&mut raw) };
        made(hr, raw, "MFCreateMediaType")
    }

    /// An input sample holding a copy of `data`, timestamped `time`.
    fn sample_of(&self, data: &[u8], time: i64) -> Result<IMFSample, String> {
        let len = u32::try_from(data.len()).map_err(|_| "sample is too large".to_string())?;
        let mut raw = std::ptr::null_mut();
        // SAFETY: the out-pointer refers to a live local.
        let hr = unsafe { (self.create_memory_buffer)(len, &mut raw) };
        let buffer: IMFMediaBuffer = made(hr, raw, "MFCreateMemoryBuffer")?;
        let mut raw = std::ptr::null_mut();
        // SAFETY: the out-pointer refers to a live local.
        let hr = unsafe { (self.create_sample)(&mut raw) };
        let sample: IMFSample = made(hr, raw, "MFCreateSample")?;
        // SAFETY: `Lock` gives a writable block of at least `max` bytes valid until `Unlock`; the
        // copy stays within `len <= max` bytes and the buffer is unlocked before it is used.
        unsafe {
            let (mut ptr, mut max) = (std::ptr::null_mut::<u8>(), 0u32);
            buffer.Lock(&mut ptr, Some(&mut max), None).map_err(|e| format!("cannot lock the input buffer: {e}"))?;
            if ptr.is_null() || max < len {
                let _ = buffer.Unlock();
                return Err("input buffer is too small".into());
            }
            std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
            buffer.Unlock().map_err(|e| format!("cannot unlock the input buffer: {e}"))?;
            buffer.SetCurrentLength(len).map_err(|e| format!("cannot size the input buffer: {e}"))?;
            sample.AddBuffer(&buffer).map_err(|e| format!("cannot add the input buffer: {e}"))?;
            sample.SetSampleTime(time).map_err(|e| format!("cannot timestamp the input: {e}"))?;
        }
        Ok(sample)
    }

    /// The decoder MFTs for `codec`, hardware ones first, as activation objects.
    fn decoders(&self, codec: NalCodec) -> Result<Vec<IMFActivate>, String> {
        let input = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: match codec {
                NalCodec::H264 => MFVideoFormat_H264,
                NalCodec::Hevc => MFVideoFormat_HEVC,
            },
        };
        let flags = MFT_ENUM_FLAG(MFT_ENUM_FLAG_SYNCMFT.0 | MFT_ENUM_FLAG_ASYNCMFT.0 | MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_SORTANDFILTER.0);
        let (mut list, mut count) = (std::ptr::null_mut::<*mut c_void>(), 0u32);
        // SAFETY: the type info and the out-pointers refer to live locals.
        let hr = unsafe { (self.enum_transforms)(MFT_CATEGORY_VIDEO_DECODER, flags.0 as u32, &input, std::ptr::null(), &mut list, &mut count) };
        if hr.is_err() {
            return Err(format!("MFTEnumEx failed ({hr:?})"));
        }
        let mut out = Vec::new();
        if !list.is_null() {
            for i in 0..count as usize {
                // SAFETY: `list` holds `count` pointers in memory allocated with CoTaskMemAlloc, each
                // null or a reference to an `IMFActivate` that this takes over.
                unsafe {
                    let p = *list.add(i);
                    if !p.is_null() {
                        out.push(IMFActivate::from_raw(p));
                    }
                }
            }
            // SAFETY: the array itself was allocated by MFTEnumEx with CoTaskMemAlloc.
            unsafe { CoTaskMemFree(Some(list as *const c_void)) };
        }
        Ok(out)
    }
}

/// What polling the decoder for output produced.
pub enum Poll {
    /// A decoded picture and its timestamp.
    Frame(IMFSample, i64),
    /// The decoder needs more input (or has finished draining).
    NeedInput,
    /// The output format changed and was renegotiated; poll again.
    FormatChanged,
}

/// A Direct3D-aware decoder MFT, configured for one stream.
pub struct Mft {
    transform: IMFTransform,
    format: SurfaceFormat,
    name: String,
}

// SAFETY: the decoder MFTs are free-threaded COM objects (they are used from the frame workers'
// threads, one at a time: an `Mft` is only used through `&mut` / `&` of its owning decoder).
unsafe impl Send for Mft {}

impl Mft {
    /// Create the decoder for `codec` with output in `format`, for pictures of `size`, decoding
    /// on the device of `gpu`. Errors (and so the stream is declined) when no Direct3D-aware
    /// synchronous decoder MFT takes the stream.
    pub fn new(api: &MfApi, gpu: &Gpu, codec: NalCodec, size: (u32, u32), format: SurfaceFormat) -> Result<Mft, String> {
        ensure_com()?;
        let candidates = api.decoders(codec)?;
        if candidates.is_empty() {
            return Err(match codec {
                NalCodec::H264 => "no H.264 decoder MFT".into(),
                NalCodec::Hevc => "no HEVC decoder MFT (the HEVC Video Extensions are not installed)".into(),
            });
        }
        let mut reasons = Vec::new();
        for a in candidates {
            match Self::configure(api, gpu, &a, codec, size, format) {
                Ok(m) => return Ok(m),
                Err(e) => reasons.push(e),
            }
        }
        Err(reasons.join("; "))
    }

    fn configure(api: &MfApi, gpu: &Gpu, activate: &IMFActivate, codec: NalCodec, size: (u32, u32), format: SurfaceFormat) -> Result<Mft, String> {
        let name = friendly_name(activate).unwrap_or_else(|| "a decoder MFT".into());
        // SAFETY: plain COM calls on live interfaces.
        let transform: IMFTransform = unsafe { activate.ActivateObject() }.map_err(|e| format!("{name}: cannot be created: {e}"))?;
        // SAFETY: plain COM calls on live interfaces; the D3D manager parameter is a pointer to
        // the device manager, kept alive by `gpu` (which every decoder holds an `Arc` of).
        unsafe {
            let attrs = transform.GetAttributes().map_err(|e| format!("{name}: no attributes: {e}"))?;
            if attrs.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) != 0 {
                return Err(format!("{name}: asynchronous decoder MFTs are not supported"));
            }
            if attrs.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) == 0 {
                return Err(format!("{name}: does not decode on Direct3D 11"));
            }
            transform
                .ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, gpu.manager_param())
                .map_err(|e| format!("{name}: refused the Direct3D device manager: {e}"))?;
        }
        let mft = Mft { transform, format, name };
        let input = api.media_type()?;
        // SAFETY: plain COM calls on live interfaces.
        unsafe {
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(|e| e.to_string())?;
            input
                .SetGUID(
                    &MF_MT_SUBTYPE,
                    &match codec {
                        NalCodec::H264 => MFVideoFormat_H264,
                        NalCodec::Hevc => MFVideoFormat_HEVC,
                    },
                )
                .map_err(|e| e.to_string())?;
            input.SetUINT64(&MF_MT_FRAME_SIZE, (u64::from(size.0) << 32) | u64::from(size.1)).map_err(|e| e.to_string())?;
            input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).map_err(|e| e.to_string())?;
            if codec == NalCodec::Hevc {
                // the HEVC decoder lists P010 output only when told the stream is Main 10
                // (eAVEncH265VProfile_Main_420_8 = 1, eAVEncH265VProfile_Main_420_10 = 2)
                let profile = if format == SurfaceFormat::P010 { 2 } else { 1 };
                input.SetUINT32(&MF_MT_MPEG2_PROFILE, profile).map_err(|e| e.to_string())?;
            }
            mft.transform.SetInputType(0, &input, 0).map_err(|e| format!("{}: does not take this stream: {e}", mft.name))?;
        }
        mft.select_output()?;
        // SAFETY: plain COM calls on live interfaces.
        unsafe {
            let info = mft.transform.GetOutputStreamInfo(0).map_err(|e| format!("{}: no output stream info: {e}", mft.name))?;
            if info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 == 0 {
                return Err(format!("{}: does not provide Direct3D output samples", mft.name));
            }
            mft.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0).map_err(|e| e.to_string())?;
            mft.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0).map_err(|e| e.to_string())?;
        }
        Ok(mft)
    }

    /// Pick the decoder's NV12 / P010 output type.
    fn select_output(&self) -> Result<(), String> {
        let want = match self.format {
            SurfaceFormat::Nv12 => MFVideoFormat_NV12,
            SurfaceFormat::P010 => MFVideoFormat_P010,
        };
        for i in 0..64 {
            // SAFETY: plain COM calls on live interfaces.
            unsafe {
                match self.transform.GetOutputAvailableType(0, i) {
                    Ok(t) => {
                        if t.GetGUID(&MF_MT_SUBTYPE).ok() == Some(want) {
                            return self.transform.SetOutputType(0, &t, 0).map_err(|e| format!("{}: refused {:?} output: {e}", self.name, self.format));
                        }
                    }
                    Err(e) if e.code() == MF_E_NO_MORE_TYPES => break,
                    Err(e) => return Err(format!("{}: no output types: {e}", self.name)),
                }
            }
        }
        Err(format!("{}: cannot output {:?}", self.name, self.format))
    }

    /// The MFT's name, for logs.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Feed one Annex B access unit. `Ok(false)`: the decoder holds enough input already; take
    /// its output ([`Mft::poll`]) and offer the sample again.
    pub fn input(&self, api: &MfApi, annex_b: &[u8], time: i64) -> Result<bool, String> {
        let sample = api.sample_of(annex_b, time)?;
        // SAFETY: plain COM call on live interfaces.
        match unsafe { self.transform.ProcessInput(0, &sample, 0) } {
            Ok(()) => Ok(true),
            Err(e) if e.code() == MF_E_NOTACCEPTING => Ok(false),
            Err(e) => Err(format!("{}: ProcessInput failed: {e}", self.name)),
        }
    }

    /// Ask the decoder for its next picture.
    pub fn poll(&self) -> Result<Poll, String> {
        let mut out = [MFT_OUTPUT_DATA_BUFFER { dwStreamID: 0, pSample: ManuallyDrop::new(None), dwStatus: 0, pEvents: ManuallyDrop::new(None) }];
        let mut status = 0u32;
        // SAFETY: one output buffer descriptor with no sample (the decoder provides its own, as
        // checked at creation) and a live status out-pointer.
        let r = unsafe { self.transform.ProcessOutput(0, &mut out, &mut status) };
        // SAFETY: the descriptor's fields were initialised above and are only taken once; the
        // decoder may have filled them in even on failure, and dropping them releases the
        // references it handed over.
        let (sample, _events) = unsafe { (ManuallyDrop::take(&mut out[0].pSample), ManuallyDrop::take(&mut out[0].pEvents)) };
        match r {
            Ok(()) => {
                let sample = sample.ok_or_else(|| format!("{}: produced no sample", self.name))?;
                // SAFETY: a plain COM call on a live interface.
                let time = unsafe { sample.GetSampleTime() }.unwrap_or(0);
                Ok(Poll::Frame(sample, time))
            }
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(Poll::NeedInput),
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                self.select_output()?;
                Ok(Poll::FormatChanged)
            }
            Err(e) => Err(format!("{}: ProcessOutput failed: {e}", self.name)),
        }
    }

    /// Start draining: [`Mft::poll`] returns the pictures held back, then `NeedInput`.
    pub fn drain(&self) -> Result<(), String> {
        // SAFETY: plain COM call on a live interface.
        unsafe { self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0) }.map_err(|e| format!("{}: drain failed: {e}", self.name))
    }

    /// Discard all queued input, output and reference pictures (a seek).
    pub fn flush(&self) -> Result<(), String> {
        // SAFETY: plain COM calls on a live interface.
        unsafe {
            self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0).map_err(|e| format!("{}: flush failed: {e}", self.name))?;
            self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0).map_err(|e| format!("{}: restart failed: {e}", self.name))
        }
    }
}

fn friendly_name(a: &IMFActivate) -> Option<String> {
    // SAFETY: a plain COM call; `GetAllocatedString` returns a CoTaskMem string that the
    // returned `PWSTR` wrapper does not free, so it is freed here after copying.
    unsafe {
        let mut s = windows::core::PWSTR::null();
        let mut len = 0u32;
        a.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut s, &mut len).ok()?;
        let name = s.to_string().ok();
        CoTaskMemFree(Some(s.0 as *const c_void));
        name
    }
}
