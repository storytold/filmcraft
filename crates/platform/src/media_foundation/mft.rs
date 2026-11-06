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

use windows::Win32::Foundation::{E_UNEXPECTED, HMODULE};
use windows::Win32::Graphics::Direct3D11::ID3D11Device;
use windows::Win32::Media::MediaFoundation::{
    CODECAPI_AVEncCommonRateControlMode, ICodecAPI, IMFActivate, IMFDXGIDeviceManager, IMFMediaBuffer, IMFMediaEventGenerator, IMFMediaType, IMFSample,
    IMFTransform, METransformDrainComplete, METransformHaveOutput, METransformNeedInput, MF_E_ATTRIBUTENOTFOUND, MF_E_NO_EVENTS_AVAILABLE, MF_E_NO_MORE_TYPES,
    MF_E_NOTACCEPTING, MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_EVENT_FLAG_NO_WAIT, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE,
    MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_MPEG2_PROFILE, MF_MT_SUBTYPE, MF_SA_D3D11_AWARE,
    MF_TRANSFORM_ASYNC, MF_TRANSFORM_ASYNC_UNLOCK, MF_VERSION, MFMediaType_Video, MFT_CATEGORY_VIDEO_DECODER, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG,
    MFT_ENUM_FLAG_ASYNCMFT, MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT, MFT_FRIENDLY_NAME_Attribute, MFT_MESSAGE_COMMAND_DRAIN,
    MFT_MESSAGE_COMMAND_FLUSH, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER,
    MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO, MFVideoFormat_NV12, MFVideoFormat_P010,
    MFVideoInterlace_Progressive, eAVEncCommonRateControlMode_CBR, eAVEncCommonRateControlMode_PeakConstrainedVBR,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
use windows::Win32::System::Variant::{VARIANT, VARIANT_0_0, VARIANT_0_0_0, VT_UI4};
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

/// How long [`MfEncoder::wait`] sleeps between asks of an asynchronous MFT that has nothing ready
/// yet. A few milliseconds each: the export is offline, so latency costs nothing and a short wait
/// covers an encoder's pipeline delay.
const POLL_WAIT: std::time::Duration = std::time::Duration::from_millis(5);
/// Bounded event credits and outputs per pump, even for a misbehaving transform.
const MAX_ENCODER_EVENTS: u32 = 4096;
const MAX_ENCODED_OUTPUT_BYTES: usize = 256 * 1024 * 1024;

#[derive(Clone, Copy)]
enum EncoderEvent {
    NeedInput,
    HaveOutput,
    DrainComplete,
}

#[derive(Default)]
struct EncoderEvents {
    input: u32,
    output: u32,
    draining: bool,
    complete: bool,
}

impl EncoderEvents {
    fn observe(&mut self, event: EncoderEvent) -> Result<(), String> {
        let credit = match event {
            EncoderEvent::NeedInput if !self.draining => &mut self.input,
            EncoderEvent::NeedInput => return Ok(()),
            EncoderEvent::HaveOutput => &mut self.output,
            EncoderEvent::DrainComplete => {
                if !self.draining {
                    return Err("unexpected encoder drain-complete event".into());
                }
                self.complete = true;
                return Ok(());
            }
        };
        *credit = credit.checked_add(1).filter(|v| *v <= MAX_ENCODER_EVENTS).ok_or("too many pending encoder events")?;
        Ok(())
    }

    fn input_ready(&self) -> bool {
        self.input != 0 && !self.draining
    }

    fn input_taken(&mut self) -> Result<(), String> {
        self.input = self.input.checked_sub(1).ok_or("encoder input without a NeedInput event")?;
        Ok(())
    }

    fn output_taken(&mut self) -> Result<(), String> {
        self.output = self.output.checked_sub(1).ok_or("encoder output without a HaveOutput event")?;
        Ok(())
    }

    fn begin_drain(&mut self) {
        self.draining = true;
        self.input = 0;
        self.complete = false;
    }

    fn drained(&self) -> bool {
        self.draining && self.complete && self.output == 0
    }
}

/// A startup-only driver compatibility path, not support for encoder format switching.
const MAX_STARTUP_TYPE_CHANGES: u8 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EncoderFormat {
    subtype: GUID,
    size: (u32, u32),
    rate: (u32, u32),
    profile: Option<u32>,
    bitrate: u32,
}

fn startup_output_change(emitted: u64, changes: u8, wanted: &EncoderFormat, candidate: &EncoderFormat) -> Result<(), String> {
    if emitted != 0 || changes >= MAX_STARTUP_TYPE_CHANGES {
        return Err("encoder output changes are only recoverable within the bounded startup negotiation".into());
    }
    let valid = |format: &EncoderFormat| format.size.0 != 0 && format.size.1 != 0 && format.rate.0 != 0 && format.rate.1 != 0 && format.bitrate != 0;
    if !valid(wanted)
        || !valid(candidate)
        || wanted.subtype != candidate.subtype
        || wanted.size != candidate.size
        || wanted.profile != candidate.profile
        || wanted.bitrate != candidate.bitrate
        || u64::from(wanted.rate.0) * u64::from(candidate.rate.1) != u64::from(candidate.rate.0) * u64::from(wanted.rate.1)
    {
        return Err("encoder startup output type changes the requested codec, dimensions, rate, profile or bitrate".into());
    }
    Ok(())
}

/// Complete partial candidates only before SetOutputType; current-type verification is read-only and requires every requested attribute.
fn complete_encoder_format(media_type: &IMFMediaType, wanted: &EncoderFormat, complete_missing: bool) -> Result<EncoderFormat, String> {
    fn u32_value(media_type: &IMFMediaType, key: &GUID, fallback: u32, complete_missing: bool) -> Result<u32, String> {
        // SAFETY: plain COM attribute calls on a live media type. Missing values alone
        // are completed from the unchanged request; other COM failures propagate.
        unsafe {
            match media_type.GetUINT32(key) {
                Ok(value) => Ok(value),
                Err(e) if complete_missing && e.code() == MF_E_ATTRIBUTENOTFOUND => {
                    media_type.SetUINT32(key, fallback).map_err(|e| e.to_string())?;
                    Ok(fallback)
                }
                Err(e) => Err(e.to_string()),
            }
        }
    }
    fn u64_value(media_type: &IMFMediaType, key: &GUID, fallback: u64, complete_missing: bool) -> Result<u64, String> {
        // SAFETY: same attribute-store contract as u32_value above.
        unsafe {
            match media_type.GetUINT64(key) {
                Ok(value) => Ok(value),
                Err(e) if complete_missing && e.code() == MF_E_ATTRIBUTENOTFOUND => {
                    media_type.SetUINT64(key, fallback).map_err(|e| e.to_string())?;
                    Ok(fallback)
                }
                Err(e) => Err(e.to_string()),
            }
        }
    }
    // SAFETY: plain COM reads on a live candidate media type.
    let (major, subtype) = unsafe { (media_type.GetGUID(&MF_MT_MAJOR_TYPE), media_type.GetGUID(&MF_MT_SUBTYPE)) };
    if major.map_err(|e| e.to_string())? != MFMediaType_Video {
        return Err("encoder output type is not video".into());
    }
    let subtype = subtype.map_err(|e| e.to_string())?;
    let interlace = u32_value(media_type, &MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32, complete_missing)?;
    if interlace != MFVideoInterlace_Progressive.0 as u32 {
        return Err("encoder startup output type is not progressive".into());
    }
    let size = u64_value(media_type, &MF_MT_FRAME_SIZE, (u64::from(wanted.size.0) << 32) | u64::from(wanted.size.1), complete_missing)?;
    let rate = u64_value(media_type, &MF_MT_FRAME_RATE, (u64::from(wanted.rate.0) << 32) | u64::from(wanted.rate.1), complete_missing)?;
    let profile = wanted.profile.map(|profile| u32_value(media_type, &MF_MT_MPEG2_PROFILE, profile, complete_missing)).transpose()?;
    let bitrate = u32_value(media_type, &MF_MT_AVG_BITRATE, wanted.bitrate, complete_missing)?;
    Ok(EncoderFormat { subtype, size: ((size >> 32) as u32, size as u32), rate: ((rate >> 32) as u32, rate as u32), profile, bitrate })
}

/// What [`MfEncoder::poll`] produced.
pub enum EncPoll {
    /// Startup renegotiated without producing a picture; never drain completion.
    FormatChanged,
    /// One finished access unit (Annex B) and its timestamp in 100 ns units.
    Frame(Vec<u8>, i64),
    /// Nothing is ready yet: feed another picture.
    NeedInput,
}

/// The bounded elapsed-time policy used by the real encoder wait. Silence is not
/// evidence that an asynchronous transform has completed draining.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Wait {
    elapsed: std::time::Duration,
    budget: std::time::Duration,
}

impl Wait {
    fn new(budget: std::time::Duration) -> Self {
        Self { elapsed: std::time::Duration::ZERO, budget }
    }

    fn nothing_yet(&mut self, step: std::time::Duration) -> bool {
        self.elapsed = self.elapsed.saturating_add(step);
        self.elapsed >= self.budget
    }
}

/// What [`MfEncoder::try_input`] made of a picture.
pub enum Input {
    Taken,
    /// The MFT's input queue is full: it needs to be asked for its output before it takes more.
    Full,
}

/// Whether an encoder MFT is one of AMD's (`AMDh264Encoder`, `AMDh265Encoder`, ...), by its
/// friendly name; the vendor's own prefix, so a driver rename to "AMD ..." still matches.
fn amd_encoder(friendly_name: &str) -> bool {
    friendly_name.trim_start().get(..3).is_some_and(|p| p.eq_ignore_ascii_case("amd"))
}

/// A `VT_UI4` VARIANT holding `value`, for `ICodecAPI::SetValue`.
fn variant_u32(value: u32) -> VARIANT {
    let mut var = VARIANT::default();
    var.Anonymous.Anonymous =
        ManuallyDrop::new(VARIANT_0_0 { vt: VT_UI4, wReserved1: 0, wReserved2: 0, wReserved3: 0, Anonymous: VARIANT_0_0_0 { ulVal: value } });
    var
}

/// What the encoder MFT is asked to produce.
pub struct EncSpec {
    /// Output subtype (`MFVideoFormat_H264` or `MFVideoFormat_HEVC`).
    pub subtype: GUID,
    /// `MF_MT_MPEG2_PROFILE` for the output, where the codec has one this path sets. Left unset,
    /// the encoder picks its own — the export asks for High and an AMD MFT answers Main.
    pub profile: Option<u32>,
    /// The codec's name, for logs and errors.
    pub label: &'static str,
    /// Picture size in luma samples.
    pub size: (u32, u32),
    /// Frame rate, numerator and denominator.
    pub rate: (u32, u32),
    /// Average bit rate in bits per second (never zero: encoders refuse a type without one).
    pub bitrate: u32,
    /// Constant bit rate (Export ▸ Bitrate Encoding ▸ CBR); otherwise variable.
    pub cbr: bool,
}

/// The cleanup contract is independent of COM so ownership transfers can be tested.
trait ShutdownTarget {
    fn shutdown(&mut self);
}

/// Owns the activating caller's shutdown responsibility, including early returns.
struct ActivationOwner<T: ShutdownTarget> {
    target: T,
}

impl<T: ShutdownTarget> ActivationOwner<T> {
    fn new(target: T) -> Self {
        Self { target }
    }
}

impl<T: ShutdownTarget> Drop for ActivationOwner<T> {
    fn drop(&mut self) {
        self.target.shutdown();
    }
}

impl ShutdownTarget for IMFActivate {
    fn shutdown(&mut self) {
        // SAFETY: this owner retains a live activation reference. ShutdownObject releases
        // the object created by this activation; callers make no subsequent transform calls.
        if let Err(error) = unsafe { self.ShutdownObject() } {
            log::warn!("Media Foundation encoder shutdown failed: {error}");
        }
    }
}

/// A hardware encoder MFT, configured for one stream.
///
/// Vendors' encoders are usually **asynchronous** MFTs (Qualcomm's, Intel's and AMD's are), and
/// an asynchronous MFT answers `MF_E_TRANSFORM_ASYNC_LOCKED` to every call until the caller sets
/// `MF_TRANSFORM_ASYNC_UNLOCK`, so that happens right after activation — see
/// <https://learn.microsoft.com/windows/win32/medfound/asynchronous-mfts>. Candidates come from
/// an unfiltered [`MfApi::all_video_encoders`], then each is configured for the requested stream.
/// The earlier empty filtered search was caused by reversed `MFTEnumEx` input/output arguments,
/// corrected in `MfApi::enumerate`; it did not establish missing vendor media-type registrations.
pub struct MfEncoder {
    // Fields drop in declaration order: shut down while the transform reference is live.
    _activation: ActivationOwner<IMFActivate>,
    transform: IMFTransform,
    events: Option<IMFMediaEventGenerator>,
    event_state: EncoderEvents,
    requested_type: IMFMediaType,
    requested_format: EncoderFormat,
    /// Native samples, including those not yet converted from a wait() batch.
    emitted_samples: u64,
    startup_type_changes: u8,
    /// The MFT hands out its finished samples itself, so the output descriptor carries none.
    provides: bool,
    /// Bytes an output buffer needs when the MFT does not provide samples.
    buffer_size: u32,
    name: String,
}

// SAFETY: the encoder MFTs are free-threaded COM objects and an `MfEncoder` is only used through
// `&mut` / `&` of its owning export encoder.
unsafe impl Send for MfEncoder {}

impl MfEncoder {
    /// Create a hardware encoder for `spec`, encoding on the device of `gpu`. Every candidate is
    /// tried and the reasons are joined, so a decline says what the machine has.
    pub fn new(api: &MfApi, gpu: &Gpu, spec: &EncSpec) -> Result<MfEncoder, String> {
        ensure_com()?;
        let candidates = api.all_video_encoders(true)?;
        if candidates.is_empty() {
            return Err("no hardware video encoder MFT".into());
        }
        let mut reasons = Vec::new();
        for activate in &candidates {
            match Self::configure(api, gpu, activate, spec) {
                Ok(m) => return Ok(m),
                Err(e) => reasons.push(e),
            }
        }
        Err(reasons.join("; "))
    }

    fn configure(api: &MfApi, gpu: &Gpu, activate: &IMFActivate, spec: &EncSpec) -> Result<MfEncoder, String> {
        let name = friendly_name(activate).unwrap_or_else(|| "an encoder MFT".into());
        let activation = ActivationOwner::new(activate.clone());
        // SAFETY: plain COM calls on live interfaces. The owner shuts down even if
        // creation or any subsequent configuration step fails.
        let transform: IMFTransform = unsafe { activate.ActivateObject() }.map_err(|e| format!("{name}: cannot be created: {e}"))?;
        // SAFETY: as above; the attributes store belongs to this transform.
        let attributes = unsafe { transform.GetAttributes() }.map_err(|e| format!("{name}: no attributes: {e}"))?;
        // SAFETY: a plain COM read of a documented UINT32 attribute.
        let asynchronous = unsafe { attributes.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) } != 0;
        let events = if asynchronous {
            // SAFETY: a plain COM write on the live transform's attribute store.
            unsafe { attributes.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1) }.map_err(|e| format!("{name}: refuses to be unlocked: {e}"))?;
            Some(transform.cast::<IMFMediaEventGenerator>().map_err(|e| format!("{name}: asynchronous encoder lacks events: {e}"))?)
        } else {
            None
        };
        // AMD's H.264 MFT does not take its rate control from the output type: left to its
        // default mode it delivers about twice MF_MT_AVG_BITRATE (its HEVC one lands on it), and
        // told the mode through the codec API it holds the target; the mean and peak rates alone
        // change nothing there. Only AMD's encoders are told, because Qualcomm's stamps pictures
        // with the times of an earlier session once its mode is set (the export is then refused
        // as reordering), and the others hold the target as they are.
        if amd_encoder(&name)
            && let Ok(codec) = transform.cast::<ICodecAPI>()
        {
            let mode = if spec.cbr { eAVEncCommonRateControlMode_CBR } else { eAVEncCommonRateControlMode_PeakConstrainedVBR };
            let var = variant_u32(mode.0 as u32);
            // SAFETY: a plain COM call; `var` is a fully initialised VT_UI4 VARIANT that outlives it.
            if let Err(e) = unsafe { codec.SetValue(&CODECAPI_AVEncCommonRateControlMode, &var) } {
                log::debug!("{name}: rate control mode not set: {e}");
            }
        }
        // Output type first: an encoder will not accept an input type before it has one.
        let output = api.media_type()?;
        // SAFETY: plain COM calls on live interfaces.
        unsafe {
            output.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(|e| e.to_string())?;
            output.SetGUID(&MF_MT_SUBTYPE, &spec.subtype).map_err(|e| e.to_string())?;
            output.SetUINT64(&MF_MT_FRAME_SIZE, (u64::from(spec.size.0) << 32) | u64::from(spec.size.1)).map_err(|e| e.to_string())?;
            output.SetUINT64(&MF_MT_FRAME_RATE, (u64::from(spec.rate.0) << 32) | u64::from(spec.rate.1)).map_err(|e| e.to_string())?;
            output.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).map_err(|e| e.to_string())?;
            output.SetUINT32(&MF_MT_AVG_BITRATE, spec.bitrate.max(1)).map_err(|e| e.to_string())?;
            // The profile the export asks for, not whatever the encoder would pick on its own.
            if let Some(p) = spec.profile {
                output.SetUINT32(&MF_MT_MPEG2_PROFILE, p).map_err(|e| e.to_string())?;
            }
            transform.SetOutputType(0, &output, 0).map_err(|e| format!("{name}: does not encode {}: {e}", spec.label))?;
        }
        // SAFETY: as above.
        let input = api.media_type()?;
        // SAFETY: as above.
        unsafe {
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(|e| e.to_string())?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12).map_err(|e| e.to_string())?;
            input.SetUINT64(&MF_MT_FRAME_SIZE, (u64::from(spec.size.0) << 32) | u64::from(spec.size.1)).map_err(|e| e.to_string())?;
            input.SetUINT64(&MF_MT_FRAME_RATE, (u64::from(spec.rate.0) << 32) | u64::from(spec.rate.1)).map_err(|e| e.to_string())?;
            input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).map_err(|e| e.to_string())?;
            transform.SetInputType(0, &input, 0).map_err(|e| format!("{name}: does not take NV12 pictures: {e}"))?;
            transform
                .ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, gpu.manager_param())
                .map_err(|e| format!("{name}: refused the Direct3D device manager: {e}"))?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0).map_err(|e| e.to_string())?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0).map_err(|e| e.to_string())?;
        }
        // SAFETY: a plain COM call on a live interface.
        let info = unsafe { transform.GetOutputStreamInfo(0) }.map_err(|e| format!("{name}: no output stream info: {e}"))?;
        let flags = info.dwFlags;
        let provides = flags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;
        let can_provide = flags & MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0 as u32 != 0;
        if !provides && !can_provide {
            return Err(format!("{name}: will not provide output samples"));
        }
        Ok(MfEncoder {
            _activation: activation,
            transform,
            events,
            event_state: EncoderEvents::default(),
            requested_type: output,
            requested_format: EncoderFormat {
                subtype: spec.subtype,
                size: spec.size,
                rate: spec.rate,
                profile: spec.profile.or_else(|| {
                    (spec.subtype == windows::Win32::Media::MediaFoundation::MFVideoFormat_HEVC)
                        .then_some(windows::Win32::Media::MediaFoundation::eAVEncH265VProfile_Main_420_8.0 as u32)
                }),
                bitrate: spec.bitrate.max(1),
            },
            emitted_samples: 0,
            startup_type_changes: 0,
            provides,
            buffer_size: if provides { 0 } else { info.cbSize.max(1) },
            name,
        })
    }

    /// The MFT's name, for logs.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Consume out-of-band notifications without blocking. Each successful input/output
    /// call must correspond to one event credit for asynchronous transforms.
    fn pump_events(&mut self) -> Result<(), String> {
        let Some(generator) = self.events.as_ref() else { return Ok(()) };
        for _ in 0..MAX_ENCODER_EVENTS {
            // SAFETY: a plain COM call on the retained event generator; NO_WAIT cannot
            // wait indefinitely for an event that the driver has not produced.
            let event = match unsafe { generator.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                Ok(event) => event,
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(()),
                Err(e) => return Err(format!("{}: cannot read encoder event: {e}", self.name)),
            };
            // SAFETY: plain COM reads on the live event returned above.
            let (kind, status) = unsafe { (event.GetType(), event.GetStatus()) };
            let kind = kind.map_err(|e| format!("{}: encoder event type: {e}", self.name))?;
            let status = status.map_err(|e| format!("{}: encoder event status: {e}", self.name))?;
            if status.is_err() {
                return Err(format!("{}: encoder event failed ({status:?})", self.name));
            }
            let event = if kind == METransformNeedInput.0 as u32 {
                EncoderEvent::NeedInput
            } else if kind == METransformHaveOutput.0 as u32 {
                EncoderEvent::HaveOutput
            } else if kind == METransformDrainComplete.0 as u32 {
                EncoderEvent::DrainComplete
            } else {
                continue;
            };
            self.event_state.observe(event).map_err(|e| format!("{}: {e}", self.name))?;
        }
        Err(format!("{}: encoder event pump exceeded its bound", self.name))
    }

    /// Feed one NV12 picture, timestamped in 100 ns units, or say the MFT's input queue is full. The
    /// caller waits for its output and offers the picture again, which is the only backpressure an
    /// asynchronous MFT gives.
    pub fn try_input(&mut self, api: &MfApi, nv12: &[u8], time: i64) -> Result<Input, String> {
        self.pump_events()?;
        if self.events.is_some() && !self.event_state.input_ready() {
            return Ok(Input::Full);
        }
        let sample = api.sample_of(nv12, time)?;
        // SAFETY: plain COM call on live interfaces; `sample` is a live object the MFT may keep.
        match unsafe { self.transform.ProcessInput(0, &sample, 0) } {
            Ok(()) => {
                if self.events.is_some() {
                    self.event_state.input_taken()?;
                }
                Ok(Input::Taken)
            }
            Err(e) if e.code() == MF_E_NOTACCEPTING => Ok(Input::Full),
            Err(e) => Err(format!("{}: ProcessInput failed: {e}", self.name)),
        }
    }

    /// Ask for the next finished access unit, without waiting: `EncPoll::NeedInput` means the MFT has
    /// nothing ready *yet*. An asynchronous MFT (every vendor's encoder) needs a moment after a
    /// picture goes in, so the caller asks again — see [`MfEncoder::wait`].
    pub fn poll(&mut self, api: &MfApi) -> Result<EncPoll, String> {
        self.pump_events()?;
        if self.events.is_some() && self.event_state.output == 0 {
            return Ok(EncPoll::NeedInput);
        }
        let output = self.take(api)?;
        if self.events.is_some() {
            match output {
                EncPoll::Frame(_, _) | EncPoll::FormatChanged => self.event_state.output_taken()?,
                EncPoll::NeedInput => return Err(format!("{}: HaveOutput event produced no output", self.name)),
            }
        }
        Ok(output)
    }

    /// Collect output with a deadline and allocation/output bounds. A synchronous
    /// NeedInput ends a drain; asynchronous draining requires DrainComplete after
    /// all HaveOutput credits have been consumed. Quiet time alone never succeeds.
    pub fn wait(&mut self, api: &MfApi, budget: std::time::Duration) -> Result<Vec<(Vec<u8>, i64)>, String> {
        let mut last = std::time::Instant::now();
        let mut wait = Wait::new(budget);
        let mut out = Vec::new();
        let mut bytes_total = 0_usize;
        loop {
            let needs_input = match self.poll(api)? {
                EncPoll::Frame(bytes, time) => {
                    bytes_total = bytes_total
                        .checked_add(bytes.len())
                        .filter(|n| *n <= MAX_ENCODED_OUTPUT_BYTES)
                        .ok_or_else(|| format!("{}: encoded output batch is too large", self.name))?;
                    if out.len() >= MAX_ENCODER_EVENTS as usize {
                        return Err(format!("{}: too many encoder outputs in one call", self.name));
                    }
                    out.push((bytes, time));
                    false
                }
                EncPoll::NeedInput => true,
                EncPoll::FormatChanged => false,
            };
            if self.event_state.draining && (self.event_state.drained() || (self.events.is_none() && needs_input)) {
                return Ok(out);
            }
            let now = std::time::Instant::now();
            let expired = wait.nothing_yet(now.duration_since(last));
            last = now;
            if expired {
                if self.event_state.draining {
                    return Err(format!("{}: encoder drain deadline expired before completion", self.name));
                }
                if needs_input || !budget.is_zero() {
                    return Ok(out);
                }
            }
            if needs_input {
                std::thread::sleep(POLL_WAIT.min(budget.saturating_sub(wait.elapsed)));
            }
        }
    }

    /// One finished access unit and its timestamp, or `None` when the MFT has nothing ready yet.
    fn take(&mut self, api: &MfApi) -> Result<EncPoll, String> {
        // A transform that provides its own samples must be given a descriptor with no sample at
        // all; only one that wants our buffer gets one.
        let sample = if self.provides {
            None
        } else {
            let buffer = api.memory_buffer(self.buffer_size)?;
            let sample = api.sample()?;
            // SAFETY: a plain COM call on two live interfaces.
            unsafe { sample.AddBuffer(&buffer) }.map_err(|e| format!("{}: cannot attach the output buffer: {e}", self.name))?;
            Some(sample)
        };
        let mut out = [MFT_OUTPUT_DATA_BUFFER { dwStreamID: 0, pSample: ManuallyDrop::new(sample), dwStatus: 0, pEvents: ManuallyDrop::new(None) }];
        let mut status = 0u32;
        // SAFETY: one output descriptor; `pSample` is either absent or a live sample we own, and
        // the status out-pointer is a live local.
        let r = unsafe { self.transform.ProcessOutput(0, &mut out, &mut status) };
        // SAFETY: the descriptor's fields were initialised above; taking them hands the references
        // the MFT filled in over to us (and releases them when the locals drop).
        let (got, _events) = unsafe { (ManuallyDrop::take(&mut out[0].pSample), ManuallyDrop::take(&mut out[0].pEvents)) };
        match r {
            Ok(()) => {
                let sample = got.ok_or_else(|| format!("{}: produced no sample", self.name))?;
                // SAFETY: a plain COM call on a live interface.
                let time = unsafe { sample.GetSampleTime() }.unwrap_or(0);
                let bytes = sample_bytes(&sample)?;
                self.emitted_samples = self.emitted_samples.checked_add(1).ok_or_else(|| format!("{}: native output count overflow", self.name))?;
                Ok(EncPoll::Frame(bytes, time))
            }
            // nothing ready yet, or the MFT wants more input before it says so
            Err(e) if matches!(e.code(), MF_E_NOTACCEPTING | E_UNEXPECTED | MF_E_TRANSFORM_NEED_MORE_INPUT) => Ok(EncPoll::NeedInput),
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                self.renegotiate_startup_output()?;
                Ok(EncPoll::FormatChanged)
            }
            Err(e) => Err(format!("{}: ProcessOutput failed: {e}", self.name)),
        }
    }

    /// Recover only an initial driver's output-type notification. General encoder
    /// format switching is unsupported, and no input pictures are discarded/replayed.
    fn renegotiate_startup_output(&mut self) -> Result<(), String> {
        startup_output_change(self.emitted_samples, self.startup_type_changes, &self.requested_format, &self.requested_format)
            .map_err(|e| format!("{}: {e}", self.name))?;
        self.startup_type_changes = self.startup_type_changes.checked_add(1).ok_or_else(|| format!("{}: output negotiation count overflow", self.name))?;
        let mut last_error = String::new();
        // Prefer the retained requested type. Some encoders do not implement type
        // enumeration; reapplying an unchanged request cannot silently switch format.
        for index in 0..=64_u32 {
            let candidate = if index == 0 {
                self.requested_type.clone()
            } else {
                // SAFETY: bounded output-type enumeration on the live transform.
                match unsafe { self.transform.GetOutputAvailableType(0, index - 1) } {
                    Ok(candidate) => candidate,
                    Err(e) if e.code() == MF_E_NO_MORE_TYPES => break,
                    Err(e) => {
                        last_error = e.to_string();
                        break;
                    }
                }
            };
            let shape = match complete_encoder_format(&candidate, &self.requested_format, true) {
                Ok(shape) => shape,
                Err(e) => {
                    last_error = e;
                    continue;
                }
            };
            if let Err(e) = startup_output_change(self.emitted_samples, self.startup_type_changes - 1, &self.requested_format, &shape) {
                last_error = e;
                continue;
            }
            // SAFETY: plain COM call with a validated, owned candidate type.
            if let Err(e) = unsafe { self.transform.SetOutputType(0, &candidate, 0) } {
                last_error = e.to_string();
                continue;
            }
            // SAFETY: query the newly configured type and allocation contract before
            // another ProcessOutput. Both are owned COM references/results.
            let (current, info) = unsafe { (self.transform.GetOutputCurrentType(0), self.transform.GetOutputStreamInfo(0)) };
            let current = current.map_err(|e| format!("{}: no renegotiated output type: {e}", self.name))?;
            let shape = complete_encoder_format(&current, &self.requested_format, false).map_err(|e| format!("{}: {e}", self.name))?;
            startup_output_change(self.emitted_samples, self.startup_type_changes - 1, &self.requested_format, &shape)
                .map_err(|e| format!("{}: renegotiated output violates request: {e}", self.name))?;
            let info = info.map_err(|e| format!("{}: no renegotiated allocation info: {e}", self.name))?;
            let provides = info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;
            let can_provide = info.dwFlags & MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0 as u32 != 0;
            if !provides && (!can_provide || info.cbAlignment != 0 || info.cbSize as usize > MAX_ENCODED_OUTPUT_BYTES) {
                return Err(format!("{}: unsupported renegotiated output buffer requirements", self.name));
            }
            self.provides = provides;
            self.buffer_size = if provides { 0 } else { info.cbSize.max(1) };
            return Ok(());
        }
        Err(format!("{}: cannot restore the requested startup output type: {last_error}", self.name))
    }

    /// Ask for whatever is left after the last picture.
    pub fn drain(&mut self) -> Result<(), String> {
        self.event_state.begin_drain();
        // SAFETY: a plain COM call on a live interface.
        unsafe { self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0) }.map_err(|e| format!("{}: drain failed: {e}", self.name))
    }
}

/// The bytes of a compressed sample, copied out of its buffer.
fn sample_bytes(sample: &IMFSample) -> Result<Vec<u8>, String> {
    // SAFETY: a plain COM call on a live interface.
    let buffer = unsafe { sample.ConvertToContiguousBuffer() }.map_err(|e| format!("cannot read the encoded sample: {e}"))?;
    // SAFETY: a plain COM read of the valid byte count.
    let valid = unsafe { buffer.GetCurrentLength() }.map_err(|e| format!("cannot read encoded sample length: {e}"))? as usize;
    if valid > MAX_ENCODED_OUTPUT_BYTES {
        return Err("encoded sample exceeds the output allocation bound".into());
    }
    // SAFETY: `Lock` gives a readable block valid until `Unlock`, which we always call.
    unsafe {
        let mut ptr = std::ptr::null_mut::<u8>();
        let mut locked = 0u32;
        buffer.Lock(&mut ptr, Some(&mut locked), None).map_err(|e| format!("cannot lock the encoded sample: {e}"))?;
        // `Lock` hands back the whole buffer, and an encoder MFT recycles one buffer across every
        // picture it writes: only `GetCurrentLength` bytes of it are this picture. Taking the rest
        // would put whatever the driver left there into the file — measured, the first keyframe of
        // a session carried 9 kB of stale bytes out to offset 262 kB. The minimum is belt and
        // braces against a driver that counts differently.
        let take = valid.min(locked as usize);
        let bytes = if ptr.is_null() { Vec::new() } else { std::slice::from_raw_parts(ptr, take).to_vec() };
        buffer.Unlock().map_err(|e| format!("cannot unlock the encoded sample: {e}"))?;
        Ok(bytes)
    }
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

    /// An empty media buffer of `len` bytes.
    pub(super) fn memory_buffer(&self, len: u32) -> Result<IMFMediaBuffer, String> {
        let mut raw = std::ptr::null_mut();
        // SAFETY: the out-pointer refers to a live local.
        let hr = unsafe { (self.create_memory_buffer)(len, &mut raw) };
        made(hr, raw, "MFCreateMemoryBuffer")
    }

    /// An empty sample.
    pub(super) fn sample(&self) -> Result<IMFSample, String> {
        let mut raw = std::ptr::null_mut();
        // SAFETY: the out-pointer refers to a live local.
        let hr = unsafe { (self.create_sample)(&mut raw) };
        made(hr, raw, "MFCreateSample")
    }

    /// An input sample holding a copy of `data`, timestamped `time`.
    fn sample_of(&self, data: &[u8], time: i64) -> Result<IMFSample, String> {
        let len = u32::try_from(data.len()).map_err(|_| "sample is too large".to_string())?;
        let buffer = self.memory_buffer(len)?;
        let sample = self.sample()?;
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

    /// The decoder MFTs taking input of `subtype`, hardware ones first, as activation objects.
    ///
    /// `subtype` goes in `MFTEnumEx`'s **input** slot — which is where a decoder's compressed type
    /// belongs — with the output left unconstrained, and every decoder MFT is found that way. Passing
    /// the codec subtype as the *output* type instead returns nothing at all on Windows and FilmCraft
    /// decodes everything in software without saying why. Measured on a Snapdragon X (Adreno X1-85,
    /// driver `31.0.137.0`): this lists Microsoft's H.264 decoder and the HEVC, VP9 and AV1
    /// extensions, and swapping the two arguments lists none of them.
    fn decoders(&self, subtype: GUID) -> Result<Vec<IMFActivate>, String> {
        let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: subtype };
        let flags = MFT_ENUM_FLAG(MFT_ENUM_FLAG_SYNCMFT.0 | MFT_ENUM_FLAG_ASYNCMFT.0 | MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_SORTANDFILTER.0);
        self.enumerate(MFT_CATEGORY_VIDEO_DECODER, flags, Some(&input), None)
    }

    /// The encoder MFTs writing `output` from `input`, as activation objects. This helper takes
    /// `output` first; `MFTEnumEx` takes the input filter first, then the output filter. With
    /// `hardware_only` cleared the list also holds the software encoders, so a machine with no
    /// hardware encoder at all can still be told apart from one that has no encoder.
    pub fn encoders(&self, output: GUID, input: GUID, hardware_only: bool) -> Result<Vec<IMFActivate>, String> {
        let out_info = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: output };
        let in_info = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: input };
        let mut flags = MFT_ENUM_FLAG_SORTANDFILTER.0;
        if hardware_only {
            flags |= MFT_ENUM_FLAG_HARDWARE.0;
        }
        self.enumerate(MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG(flags), Some(&in_info), Some(&out_info))
    }

    /// Every MFT in `MFT_CATEGORY_VIDEO_ENCODER`, with no constraint on either side's media type.
    /// The factory deliberately enumerates without type filters, then tries each candidate with
    /// the requested output type, input type and Direct3D device manager during configuration.
    pub fn all_video_encoders(&self, hardware_only: bool) -> Result<Vec<IMFActivate>, String> {
        let mut flags = MFT_ENUM_FLAG_SORTANDFILTER.0;
        if hardware_only {
            flags |= MFT_ENUM_FLAG_HARDWARE.0;
        }
        self.enumerate(MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG(flags), None, None)
    }

    /// The MFTs of `category` taking `input` and producing `output` (either may be `None` to leave that
    /// side unconstrained), hardware ones first, as activation objects.
    ///
    /// The order is `MFTEnumEx`'s own: **input first, then output**
    /// (<https://learn.microsoft.com/windows/win32/api/mfapi/nf-mfapi-mftenumex>). Get it backwards
    /// and a filtered encoder search matches nothing at all, because the codec's own subtype is
    /// being looked for on the wrong side.
    fn enumerate(
        &self,
        category: GUID,
        flags: MFT_ENUM_FLAG,
        input: Option<&MFT_REGISTER_TYPE_INFO>,
        output: Option<&MFT_REGISTER_TYPE_INFO>,
    ) -> Result<Vec<IMFActivate>, String> {
        let in_ptr = input.map_or(std::ptr::null(), |i| i as *const _);
        let out_ptr = output.map_or(std::ptr::null(), |o| o as *const _);
        let (mut list, mut count) = (std::ptr::null_mut::<*mut c_void>(), 0u32);
        // SAFETY: the type info and the out-pointers refer to live locals.
        let hr = unsafe { (self.enum_transforms)(category, flags.0 as u32, in_ptr, out_ptr, &mut list, &mut count) };
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

/// An attribute of the decoder's input media type.
pub enum Attr {
    U32(u32),
    Blob(Vec<u8>),
}

/// What the decoder MFT is asked to decode.
pub struct Spec {
    /// The input subtype (`MFVideoFormat_H264`, `_HEVC`, `_VP90`, `_AV1`).
    pub subtype: GUID,
    /// The codec's name and what is missing when no MFT takes it.
    pub label: &'static str,
    pub missing: &'static str,
    /// Picture size in luma samples.
    pub size: (u32, u32),
    /// Output format.
    pub format: SurfaceFormat,
    /// Extra input type attributes (profile, sequence header...).
    pub attrs: Vec<(GUID, Attr)>,
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
    pub fn new(api: &MfApi, gpu: &Gpu, spec: &Spec) -> Result<Mft, String> {
        ensure_com()?;
        let candidates = api.decoders(spec.subtype)?;
        if candidates.is_empty() {
            return Err(format!("no {} decoder MFT{}", spec.label, spec.missing));
        }
        let mut reasons = Vec::new();
        for a in candidates {
            match Self::configure(api, gpu, &a, spec) {
                Ok(m) => return Ok(m),
                Err(e) => reasons.push(e),
            }
        }
        Err(reasons.join("; "))
    }

    fn configure(api: &MfApi, gpu: &Gpu, activate: &IMFActivate, spec: &Spec) -> Result<Mft, String> {
        let (size, format) = (spec.size, spec.format);
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
            input.SetGUID(&MF_MT_SUBTYPE, &spec.subtype).map_err(|e| e.to_string())?;
            input.SetUINT64(&MF_MT_FRAME_SIZE, (u64::from(size.0) << 32) | u64::from(size.1)).map_err(|e| e.to_string())?;
            input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).map_err(|e| e.to_string())?;
            for (key, value) in &spec.attrs {
                match value {
                    Attr::U32(v) => input.SetUINT32(key, *v),
                    Attr::Blob(b) => input.SetBlob(key, b),
                }
                .map_err(|e| e.to_string())?;
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

    /// The size of the pictures the decoder says it outputs: the display aperture of its output
    /// type when it has one (the surfaces are aligned: 640x368 for a 640x360 picture), else its
    /// frame size.
    pub fn output_size(&self) -> Option<(u32, u32)> {
        // SAFETY: plain COM calls on live interfaces; `GetBlob` writes at most the 16 bytes of the
        // buffer it is given (an `MFVideoArea`: two offsets of 4 bytes, then the area as two i32).
        unsafe {
            let t = self.transform.GetOutputCurrentType(0).ok()?;
            let mut area = [0u8; 16];
            let mut len = 0u32;
            if t.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, &mut area, Some(&mut len)).is_ok() && len == 16 {
                let (cx, cy) = (i32::from_le_bytes([area[8], area[9], area[10], area[11]]), i32::from_le_bytes([area[12], area[13], area[14], area[15]]));
                if cx > 0 && cy > 0 {
                    return Some((cx as u32, cy as u32));
                }
            }
            let packed = t.GetUINT64(&MF_MT_FRAME_SIZE).ok()?;
            Some(((packed >> 32) as u32, packed as u32))
        }
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

/// One line per decoder MFT taking `subtype` (name, synchronous or not, Direct3D-aware or not):
/// diagnostics.
pub fn describe(api: &MfApi, subtype: GUID) -> Vec<String> {
    let Ok(list) = api.decoders(subtype) else { return vec!["enumeration failed".into()] };
    list.iter()
        .map(|a| {
            let name = friendly_name(a).unwrap_or_else(|| "?".into());
            // SAFETY: plain COM calls on live interfaces.
            let (asynchronous, d3d11) = unsafe {
                match a.ActivateObject::<IMFTransform>().and_then(|t| t.GetAttributes()) {
                    Ok(attrs) => (attrs.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) != 0, attrs.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) != 0),
                    Err(_) => (false, false),
                }
            };
            format!("{name} (async {asynchronous}, Direct3D 11 aware {d3d11})")
        })
        .collect()
}

/// One line per activation object: the MFT's friendly name, or `?`.
pub fn describe_activates(list: &[IMFActivate]) -> Vec<String> {
    list.iter().map(|a| friendly_name(a).unwrap_or_else(|| "?".into())).collect()
}

/// One line per encoder MFT writing `output` from `input` (name, Direct3D-aware or not):
/// diagnostics (`examples/mfcaps.rs`).
pub fn describe_encoders(api: &MfApi, output: GUID, input: GUID, hardware_only: bool) -> Vec<String> {
    let Ok(list) = api.encoders(output, input, hardware_only) else { return vec!["enumeration failed".into()] };
    list.iter()
        .map(|a| {
            let name = friendly_name(a).unwrap_or_else(|| "?".into());
            let _activation = ActivationOwner::new(a.clone());
            // SAFETY: plain COM calls on live interfaces. The scope's owner shuts
            // down the activated encoder after reading its attributes, including errors.
            let d3d11 = unsafe {
                a.ActivateObject::<IMFTransform>()
                    .ok()
                    .and_then(|t| t.GetAttributes().ok())
                    .and_then(|attrs| attrs.GetUINT32(&MF_SA_D3D11_AWARE).ok())
                    .is_some_and(|v| v != 0)
            };
            format!("{name} (Direct3D 11 aware {d3d11})")
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const STEP: Duration = Duration::from_millis(5);
    const BUDGET: Duration = Duration::from_millis(100);

    /// Only AMD's encoders are told their rate control mode (#936).
    #[test]
    fn only_amd_encoders_are_told_their_rate_control() {
        for (name, amd) in [
            ("AMDh264Encoder", true),
            ("AMDh265Encoder", true),
            ("AMD AV1 Encoder", true),
            ("NVIDIA H.264 Encoder MFT", false),
            ("Intel® Hardware H264 Encoder MFT", false),
            ("QCOM H264 Encoder MFT", false),
            ("", false),
            ("am", false),
        ] {
            assert_eq!(amd_encoder(name), amd, "{name}");
        }
    }

    /// An asynchronous encoder says "not yet" before it has anything. The wait must keep asking
    /// rather than stopping at the first silence, or a picture that arrives a moment later is lost.
    #[test]
    fn a_first_silence_does_not_end_the_wait() {
        let mut w = Wait::new(BUDGET);
        // the encoder is a few milliseconds behind: silence, silence, then a picture
        assert!(!w.nothing_yet(STEP), "the first silence must not end the wait");
        assert!(!w.nothing_yet(STEP), "nor the second");
        // Further empty polls still have the rest of the same bounded budget.
        assert!(!w.nothing_yet(STEP));
        assert!(!w.nothing_yet(STEP));
    }

    #[test]
    fn a_sustained_silence_ends_at_the_deadline() {
        let mut w = Wait::new(BUDGET);
        let mut steps = 0;
        while !w.nothing_yet(STEP) {
            steps += 1;
            assert!(steps < 1000, "the wait never ended");
        }
        assert!(w.elapsed >= BUDGET, "silence alone cannot complete the wait early");
    }

    /// Whatever the encoder does, the wait ends inside `budget`: it is the backstop for a driver
    /// that never answers, and the reason an export cannot hang here.
    #[test]
    fn the_budget_bounds_a_wait_that_never_ends() {
        let mut w = Wait::new(BUDGET);
        let mut steps = 0;
        while !w.nothing_yet(STEP) {
            steps += 1;
            assert!(steps < 1000, "the budget did not bound the wait");
        }
        assert!(w.elapsed >= BUDGET, "ended after {:?}, short of the budget", w.elapsed);
    }

    /// Poll progress does not reset the total elapsed-time budget.
    #[test]
    fn poll_progress_does_not_reset_the_elapsed_time_budget() {
        let mut w = Wait::new(Duration::from_secs(60));
        for _ in 0..10_000 {
            assert!(!w.nothing_yet(STEP), "still within the budget");
        }
        assert!(w.nothing_yet(Duration::from_secs(10)), "even a producing encoder reaches the deadline");
    }

    #[test]
    fn review_regression_drain_does_not_treat_quiet_as_completion() {
        // This is the actual drain wait's budget and quiet threshold. The final
        // picture can arrive after a normal quiet interval; only completion or
        // the bounded deadline can end draining, not absence of output alone.
        let mut wait = Wait::new(Duration::from_secs(10));
        assert!(!wait.nothing_yet(Duration::from_millis(35)), "a quiet encoder has not completed draining");
        assert!(!wait.nothing_yet(Duration::from_millis(40)), "late output does not make later silence completion evidence");
    }

    struct FakeActivation(std::rc::Rc<std::cell::Cell<usize>>);

    impl ShutdownTarget for FakeActivation {
        fn shutdown(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    #[test]
    fn lifecycle_regression_failed_configuration_shuts_down_once() {
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let configure = || -> Result<(), &'static str> {
            let _owner = ActivationOwner::new(FakeActivation(calls.clone()));
            Err("configuration failed")
        };
        assert!(configure().is_err());
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn lifecycle_regression_success_transfers_shutdown_to_encoder_lifetime() {
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let owner = ActivationOwner::new(FakeActivation(calls.clone()));
        let encoder_owner = owner;
        assert_eq!(calls.get(), 0, "configuration must not shut down a live encoder");
        drop(encoder_owner);
        assert_eq!(calls.get(), 1, "a moved owner shuts down exactly once");
    }

    #[test]
    fn lifecycle_regression_diagnostic_scope_always_shuts_down() {
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        {
            let _owner = ActivationOwner::new(FakeActivation(calls.clone()));
            assert_eq!(calls.get(), 0);
        }
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn transport_regression_input_requires_one_credit_per_event() {
        let mut state = EncoderEvents::default();
        assert!(!state.input_ready());
        state.observe(EncoderEvent::NeedInput).unwrap();
        assert!(state.input_ready());
        state.input_taken().unwrap();
        assert!(!state.input_ready());
        assert!(state.input_taken().is_err());
    }

    #[test]
    fn transport_regression_output_and_drain_completion_are_independent() {
        let mut state = EncoderEvents::default();
        state.begin_drain();
        state.observe(EncoderEvent::HaveOutput).unwrap();
        state.observe(EncoderEvent::DrainComplete).unwrap();
        assert!(!state.drained(), "pending final output must be consumed first");
        state.output_taken().unwrap();
        assert!(state.drained());
        assert!(state.output_taken().is_err());
    }

    #[test]
    fn transport_regression_silence_is_not_a_completion_event() {
        let mut state = EncoderEvents::default();
        state.observe(EncoderEvent::NeedInput).unwrap();
        state.begin_drain();
        assert!(!state.input_ready());
        assert!(!state.drained());
        state.observe(EncoderEvent::NeedInput).unwrap();
        assert!(!state.input_ready(), "late input requests are ignored while draining");
        state.observe(EncoderEvent::DrainComplete).unwrap();
        assert!(state.drained());
    }

    #[test]
    fn transport_regression_event_credits_are_bounded() {
        let mut state = EncoderEvents::default();
        for _ in 0..MAX_ENCODER_EVENTS {
            state.observe(EncoderEvent::HaveOutput).unwrap();
        }
        assert!(state.observe(EncoderEvent::HaveOutput).is_err());
    }

    fn requested_h264_format() -> EncoderFormat {
        use windows::Win32::Media::MediaFoundation::MFVideoFormat_H264;
        EncoderFormat { subtype: MFVideoFormat_H264, size: (1280, 720), rate: (24, 1), profile: Some(100), bitrate: 4_000_000 }
    }

    #[test]
    fn startup_type_regression_accepts_only_matching_initial_output() {
        let wanted = requested_h264_format();
        assert!(startup_output_change(0, 0, &wanted, &wanted).is_ok());
        assert!(startup_output_change(1, 0, &wanted, &wanted).is_err(), "even an unwrapped native sample makes this midstream");
        assert!(startup_output_change(0, MAX_STARTUP_TYPE_CHANGES, &wanted, &wanted).is_err());
    }

    #[test]
    fn startup_type_regression_rejects_changed_codec_dimensions_profile_or_bitrate() {
        let wanted = requested_h264_format();
        let mut changed = wanted;
        changed.subtype = windows::Win32::Media::MediaFoundation::MFVideoFormat_HEVC;
        assert!(startup_output_change(0, 0, &wanted, &changed).is_err());
        for changed in [
            EncoderFormat { size: (640, 360), ..wanted },
            EncoderFormat { rate: (30, 1), ..wanted },
            EncoderFormat { profile: Some(77), ..wanted },
            EncoderFormat { bitrate: 2_000_000, ..wanted },
        ] {
            assert!(startup_output_change(0, 0, &wanted, &changed).is_err());
        }
    }

    #[test]
    fn startup_type_regression_checks_exact_rational_rate_and_hostile_zero_fields() {
        let wanted = EncoderFormat { rate: (30_000, 1001), ..requested_h264_format() };
        assert!(startup_output_change(0, 0, &wanted, &EncoderFormat { rate: (60_000, 2002), ..wanted }).is_ok());
        for changed in [
            EncoderFormat { rate: (0, 1), ..wanted },
            EncoderFormat { rate: (30_000, 0), ..wanted },
            EncoderFormat { size: (0, 720), ..wanted },
            EncoderFormat { bitrate: 0, ..wanted },
        ] {
            assert!(startup_output_change(0, 0, &wanted, &changed).is_err());
        }
    }

    #[test]
    fn startup_type_regression_format_change_consumes_its_output_credit() {
        let mut state = EncoderEvents::default();
        state.observe(EncoderEvent::HaveOutput).unwrap();
        state.output_taken().unwrap(); // ProcessOutput returned STREAM_CHANGE, not a picture.
        assert_eq!(state.output, 0);
        assert!(state.output_taken().is_err(), "another native output call requires another event");
        state.observe(EncoderEvent::HaveOutput).unwrap();
        state.output_taken().unwrap();
    }
}
