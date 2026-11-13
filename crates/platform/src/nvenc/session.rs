//! An NVENC (H.264 or HEVC) encode session: the driver library, the device it is opened on, the
//! encoder configuration, and the input / output buffers (every call into the driver).
//!
//! FFI module (docs/adr/0001-platform-ffi.md): every `unsafe` block has a `// SAFETY:` comment, the
//! public items are safe and return `Result<_, String>`, no driver pointer leaves this module.

use std::ffi::{CStr, c_void};
use std::sync::OnceLock;

use super::device::Device;
use super::ffi::{
    CreateInstanceFn, Guid, H264_OUTPUT_AUD, H264_REPEAT_SPSPPS, HEVC_CHROMA_FORMAT_MASK, HEVC_CHROMA_FORMAT_SHIFT, HEVC_DISABLE_SPSPPS,
    HEVC_ENABLE_INTRA_REFRESH, HEVC_OUTPUT_AUD, HEVC_PIXEL_BIT_DEPTH_MASK, HEVC_PIXEL_BIT_DEPTH_SHIFT, HEVC_REPEAT_SPSPPS, Handle, MaxSupportedVersionFn,
    NV_ENC_BUFFER_FORMAT_ABGR, NV_ENC_BUFFER_FORMAT_NV12, NV_ENC_BUFFER_FORMAT_YUV420_10BIT, NV_ENC_CAPS_HEIGHT_MAX, NV_ENC_CAPS_HEIGHT_MIN,
    NV_ENC_CAPS_NUM_MAX_BFRAMES, NV_ENC_CAPS_PARAM, NV_ENC_CAPS_PARAM_VER, NV_ENC_CAPS_SUPPORT_10BIT_ENCODE, NV_ENC_CAPS_WIDTH_MAX, NV_ENC_CAPS_WIDTH_MIN,
    NV_ENC_CODEC_H264_GUID, NV_ENC_CODEC_HEVC_GUID, NV_ENC_CONFIG, NV_ENC_CONFIG_VER, NV_ENC_CREATE_BITSTREAM_BUFFER, NV_ENC_CREATE_BITSTREAM_BUFFER_VER,
    NV_ENC_CREATE_INPUT_BUFFER, NV_ENC_CREATE_INPUT_BUFFER_VER, NV_ENC_ERR_NEED_MORE_INPUT, NV_ENC_H264_ENTROPY_CODING_MODE_CABAC,
    NV_ENC_H264_PROFILE_BASELINE_GUID, NV_ENC_H264_PROFILE_HIGH_GUID, NV_ENC_H264_PROFILE_MAIN_GUID, NV_ENC_HEVC_CUSIZE_AUTOSELECT,
    NV_ENC_HEVC_PROFILE_MAIN_GUID, NV_ENC_HEVC_PROFILE_MAIN10_GUID, NV_ENC_INITIALIZE_PARAMS, NV_ENC_INITIALIZE_PARAMS_VER, NV_ENC_LEVEL_AUTOSELECT,
    NV_ENC_LOCK_BITSTREAM, NV_ENC_LOCK_BITSTREAM_VER, NV_ENC_LOCK_INPUT_BUFFER, NV_ENC_LOCK_INPUT_BUFFER_VER, NV_ENC_MAP_INPUT_RESOURCE,
    NV_ENC_MAP_INPUT_RESOURCE_VER, NV_ENC_MEMORY_HEAP_SYSMEM_CACHED, NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS, NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER,
    NV_ENC_PARAMS_RC_CBR, NV_ENC_PARAMS_RC_VBR, NV_ENC_PIC_FLAG_EOS, NV_ENC_PIC_PARAMS, NV_ENC_PIC_PARAMS_VER, NV_ENC_PIC_STRUCT_FRAME, NV_ENC_PRESET_CONFIG,
    NV_ENC_PRESET_CONFIG_VER, NV_ENC_PRESET_P5_GUID, NV_ENC_RC_PARAMS_VER, NV_ENC_SEI_PAYLOAD, NV_ENC_SEQUENCE_PARAM_PAYLOAD,
    NV_ENC_SEQUENCE_PARAM_PAYLOAD_VER, NV_ENC_SUCCESS, NV_ENC_TIER_HEVC_MAIN, NV_ENC_TUNING_INFO_HIGH_QUALITY, NV_ENC_VUI_COLOR_PRIMARIES_BT709,
    NV_ENC_VUI_MATRIX_COEFFS_BT709, NV_ENC_VUI_TRANSFER_CHARACTERISTIC_BT709, NV_ENC_VUI_VIDEO_FORMAT_UNSPECIFIED, NV_ENCODE_API_FUNCTION_LIST,
    NV_ENCODE_API_FUNCTION_LIST_VER, NVENCAPI_VERSION, NvStatus,
};

/// The driver's entry points (loaded once).
struct Api {
    list: NV_ENCODE_API_FUNCTION_LIST,
    #[cfg(target_os = "linux")]
    _library: libloading::Library,
}

// SAFETY: the function table is plain function pointers, immutable after loading; NVENC's API is
// callable from any thread (calls on one encoder session are serialised by `&mut Session`).
unsafe impl Send for Api {}
// SAFETY: see above.
unsafe impl Sync for Api {}

fn api() -> Result<&'static Api, String> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(load).as_ref().map_err(Clone::clone)
}

#[cfg(target_os = "windows")]
fn load() -> Result<Api, String> {
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
    use windows::core::{PCSTR, w};
    // SAFETY: loads the driver's library from the system directory only.
    let module: HMODULE =
        unsafe { LoadLibraryExW(w!("nvEncodeAPI64.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }.map_err(|_| "no NVIDIA encoder driver".to_string())?;
    // SAFETY: both symbols are looked up with the signatures of nvEncodeAPI.h; the table is
    // zero-initialised (all-zero is valid: null pointers) with its version set, as the API asks.
    unsafe {
        let version: MaxSupportedVersionFn = std::mem::transmute(
            GetProcAddress(module, PCSTR(c"NvEncodeAPIGetMaxSupportedVersion".as_ptr().cast())).ok_or("driver lacks NvEncodeAPIGetMaxSupportedVersion")?,
        );
        let create: CreateInstanceFn =
            std::mem::transmute(GetProcAddress(module, PCSTR(c"NvEncodeAPICreateInstance".as_ptr().cast())).ok_or("driver lacks NvEncodeAPICreateInstance")?);
        let mut v = 0u32;
        if version(&mut v) != NV_ENC_SUCCESS {
            return Err("cannot query the NVENC version".into());
        }
        let ours = (NVENCAPI_VERSION & 0xf) << 4 | ((NVENCAPI_VERSION >> 24) & 0xf);
        if v < ours {
            return Err(format!("the NVIDIA driver's NVENC ({}.{}) is older than {}.{}", v >> 4, v & 0xf, ours >> 4, ours & 0xf));
        }
        let mut list: NV_ENCODE_API_FUNCTION_LIST = std::mem::zeroed();
        list.version = NV_ENCODE_API_FUNCTION_LIST_VER;
        if create(&mut list) != NV_ENC_SUCCESS {
            return Err("NvEncodeAPICreateInstance failed".into());
        }
        Ok(Api { list })
    }
}

#[cfg(target_os = "linux")]
fn load() -> Result<Api, String> {
    // SAFETY: runtime-load NVIDIA's driver and use the signatures from nvEncodeAPI.h.
    // The library lives alongside the immutable function table, for the process lifetime.
    unsafe {
        let library = libloading::Library::new("libnvidia-encode.so.1").map_err(|e| format!("no NVIDIA encoder driver: {e}"))?;
        let version: libloading::Symbol<MaxSupportedVersionFn> = library.get(b"NvEncodeAPIGetMaxSupportedVersion\0").map_err(|e| e.to_string())?;
        let create: libloading::Symbol<CreateInstanceFn> = library.get(b"NvEncodeAPICreateInstance\0").map_err(|e| e.to_string())?;
        let mut v = 0;
        if version(&mut v) != NV_ENC_SUCCESS {
            return Err("cannot query the NVENC version".into());
        }
        let ours = (NVENCAPI_VERSION & 0xf) << 4 | ((NVENCAPI_VERSION >> 24) & 0xf);
        if v < ours {
            return Err(format!("the NVIDIA driver's NVENC ({}.{}) is older than {}.{}", v >> 4, v & 0xf, ours >> 4, ours & 0xf));
        }
        let mut list: NV_ENCODE_API_FUNCTION_LIST = std::mem::zeroed();
        list.version = NV_ENCODE_API_FUNCTION_LIST_VER;
        if create(&mut list) != NV_ENC_SUCCESS {
            return Err("NvEncodeAPICreateInstance failed".into());
        }
        Ok(Api { list, _library: library })
    }
}

/// The codec a session encodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    H264,
    Hevc,
}

impl Codec {
    fn guid(self) -> Guid {
        match self {
            Codec::H264 => NV_ENC_CODEC_H264_GUID,
            Codec::Hevc => NV_ENC_CODEC_HEVC_GUID,
        }
    }
}

/// One SEI message the encoder writes in front of every IDR picture (HEVC): its payload type and bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sei {
    pub payload_type: u32,
    pub payload: Vec<u8>,
}

/// The colour description of a stream (ITU-T H.273 code points, limited range) and the SEI messages
/// that go with it. The default is BT.709 without SEI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signal {
    pub primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
    pub sei: Vec<Sei>,
}

impl Default for Signal {
    fn default() -> Self {
        Signal { primaries: 1, transfer: 1, matrix: 1, sei: Vec::new() }
    }
}

/// What the encoder says it can do for a codec.
#[derive(Clone, Copy, Debug)]
pub struct Caps {
    /// 10-bit encoding (Main 10) is supported.
    pub ten_bit: bool,
    pub max_bframes: u32,
    pub min_size: (u32, u32),
    pub max_size: (u32, u32),
}

/// Encoder settings (already validated by the caller).
#[derive(Clone, Debug)]
pub struct Params {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    pub fps: (u32, u32),
    pub bitrate: u32,
    pub max_bitrate: u32,
    pub cbr: bool,
    pub gop: u32,
    /// H.264 only: 0 baseline, 1 main, 2 high (HEVC is Main, or Main 10 with `ten_bit`).
    pub profile: u8,
    /// HEVC Main 10: 10-bit 4:2:0 pictures (P010), otherwise 8-bit (NV12).
    pub ten_bit: bool,
    /// 8-bit only: pictures go in as packed RGBA (ABGR) and NVENC converts them to 4:2:0 on the GPU.
    pub rgba_input: bool,
    /// HEVC: the VUI colour description and the SEI messages of every IDR picture (H.264 keeps BT.709).
    pub signal: Signal,
    /// The codec's own level code: `level × 10` for H.264, `level × 30` for HEVC.
    pub level: Option<u8>,
    pub sar: Option<(u32, u32)>,
    /// B-frames between references (0 or 1).
    pub bframes: u32,
}

/// An input buffer locked for writing: NV12 (or P010) rows go to `data` at `pitch` bytes per row, the
/// chroma plane after `pitch * height` bytes; packed RGBA (ABGR) has `height` rows and no chroma plane.
pub struct Locked<'a> {
    pub data: &'a mut [u8],
    pub pitch: usize,
}

/// A finished picture's bitstream.
pub struct Output {
    /// Annex B bytes.
    pub data: Vec<u8>,
    pub pts: u64,
    /// `NV_ENC_PIC_TYPE_*`.
    pub pic_type: u32,
}

/// The result of submitting a picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Submitted {
    /// The pictures submitted so far have their bitstreams ready.
    Ready,
    /// The encoder wants more input (B-frame reordering) before it has output.
    NeedMoreInput,
}

struct Slot {
    input: *mut c_void,
    output: *mut c_void,
}

/// A texture registered as an input picture ([`Session::register_texture`]), and its mapping while
/// a submitted picture reads it (null otherwise).
struct Input {
    registered: *mut c_void,
    mapped: *mut c_void,
}

/// An initialised encoder with its buffers.
pub struct Session {
    api: &'static Api,
    enc: Handle,
    // Keeps the Direct3D device / retained CUDA context alive through session destruction.
    _device: Device,
    slots: Vec<Slot>,
    /// Textures registered as input pictures, by key (Windows: the target pool's pictures).
    inputs: Vec<Input>,
    size: (u32, u32),
    out_size: u32,
    /// `NV_ENC_BUFFER_FORMAT_*` of the input buffers and pictures, and its bytes per sample.
    fmt: u32,
    bytes_per_sample: usize,
    /// The SEI messages of every IDR picture. The payload bytes and the array of descriptors live on
    /// the heap, owned by the session for its whole life, so the pointers handed to the driver stay
    /// valid however the `Session` value is moved and however long the driver keeps a picture queued.
    sei_payloads: Vec<Box<[u8]>>,
    sei_array: Box<[NV_ENC_SEI_PAYLOAD]>,
    /// Pictures submitted, and the distance between IDR pictures (to know which pictures get the SEI).
    submitted: u64,
    gop: u64,
}

// SAFETY: an encoder session and its buffers are driver objects used from one thread at a time (the
// session is `&mut`-driven); Direct3D is free-threaded, and NVENC takes an explicit CUDA context.
unsafe impl Send for Session {}

/// A readable form of a driver status.
fn status(what: &str, s: NvStatus, enc: Handle, api: &Api) -> String {
    let detail = (!enc.is_null())
        .then(|| {
            // SAFETY: a valid session handle; the driver returns a NUL-terminated string it owns.
            api.list.nvEncGetLastErrorString.and_then(|f| unsafe {
                let p = f(enc);
                (!p.is_null()).then(|| CStr::from_ptr(p).to_string_lossy().into_owned())
            })
        })
        .flatten()
        // the driver's text is sometimes not text at all (HEVC parameter errors): keep it only when readable
        .filter(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_graphic() || c == ' '));
    match detail {
        Some(d) => format!("{what} failed ({s}): {d}"),
        None => format!("{what} failed ({s})"),
    }
}

impl Session {
    /// Open a session on the system's NVIDIA GPU, or say why not.
    pub fn open() -> Result<Session, String> {
        let api = api()?;
        let device = Device::new()?;
        // SAFETY: a zeroed parameter block with its version and the matching device pointer; the
        // device is kept in the session for as long as the encoder lives. On failure a non-null
        // handle is the driver's and is destroyed exactly once, here, before returning.
        let enc = unsafe {
            let mut p: NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS = std::mem::zeroed();
            p.version = NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER;
            p.deviceType = Device::KIND;
            p.device = device.as_raw();
            p.apiVersion = NVENCAPI_VERSION;
            let open = api.list.nvEncOpenEncodeSessionEx.ok_or("the driver has no nvEncOpenEncodeSessionEx")?;
            let mut enc: Handle = std::ptr::null_mut();
            let s = open(&mut p, &mut enc);
            if s != NV_ENC_SUCCESS || enc.is_null() {
                let e = status("opening the NVENC session", s, enc, api);
                // a failed open may still hand back a session; destroy it so the GPU's encoder
                // session limit is not used up by sessions nobody owns
                if !enc.is_null()
                    && let Some(d) = api.list.nvEncDestroyEncoder
                {
                    d(enc);
                }
                return Err(e);
            }
            enc
        };
        Ok(Session {
            api,
            enc,
            _device: device,
            slots: Vec::new(),
            inputs: Vec::new(),
            size: (0, 0),
            out_size: 0,
            fmt: NV_ENC_BUFFER_FORMAT_NV12,
            bytes_per_sample: 1,
            sei_payloads: Vec::new(),
            sei_array: Box::new([]),
            submitted: 0,
            gop: 1,
        })
    }

    /// The encoder's limits for `codec` (an error when the GPU cannot encode it).
    pub fn caps(&self, codec: Codec) -> Result<Caps, String> {
        let get = |cap: u32| -> Result<u32, String> {
            let f = self.api.list.nvEncGetEncodeCaps.ok_or("no nvEncGetEncodeCaps")?;
            // SAFETY: a valid session, a zeroed parameter block with its version, one int out.
            unsafe {
                let mut p: NV_ENC_CAPS_PARAM = std::mem::zeroed();
                p.version = NV_ENC_CAPS_PARAM_VER;
                p.capsToQuery = cap;
                let mut v = 0i32;
                let s = f(self.enc, codec.guid(), &mut p, &mut v);
                if s != NV_ENC_SUCCESS {
                    return Err(status(if codec == Codec::Hevc { "querying the HEVC encoder" } else { "querying the encoder" }, s, self.enc, self.api));
                }
                Ok(v.max(0) as u32)
            }
        };
        Ok(Caps {
            // a failing query means no 10-bit encoder
            ten_bit: get(NV_ENC_CAPS_SUPPORT_10BIT_ENCODE).unwrap_or(0) != 0,
            max_bframes: get(NV_ENC_CAPS_NUM_MAX_BFRAMES)?,
            min_size: (get(NV_ENC_CAPS_WIDTH_MIN)?, get(NV_ENC_CAPS_HEIGHT_MIN)?),
            max_size: (get(NV_ENC_CAPS_WIDTH_MAX)?, get(NV_ENC_CAPS_HEIGHT_MAX)?),
        })
    }

    /// Configure the encoder (preset P5, high quality tuning, the settings' rate control) and
    /// create `ring` pairs of input / output buffers.
    pub fn initialize(&mut self, p: &Params, ring: usize) -> Result<(), String> {
        if p.ten_bit && p.codec != Codec::Hevc {
            return Err("10-bit encoding is HEVC Main 10 here".into());
        }
        if !p.signal.sei.is_empty() && p.codec != Codec::Hevc {
            return Err("SEI messages are written for HEVC only".into());
        }
        if p.ten_bit && p.rgba_input {
            return Err("RGBA input is 8-bit only".into());
        }
        (self.fmt, self.bytes_per_sample) = match (p.ten_bit, p.rgba_input) {
            (true, _) => (NV_ENC_BUFFER_FORMAT_YUV420_10BIT, 2),
            (false, true) => (NV_ENC_BUFFER_FORMAT_ABGR, 4),
            (false, false) => (NV_ENC_BUFFER_FORMAT_NV12, 1),
        };
        self.gop = u64::from(p.gop.max(1));
        self.submitted = 0;
        // the SEI payloads: heap blocks that outlive every picture (see the field documentation)
        self.sei_payloads = p.signal.sei.iter().map(|m| m.payload.clone().into_boxed_slice()).collect();
        let mut array = Vec::with_capacity(self.sei_payloads.len());
        for (m, bytes) in p.signal.sei.iter().zip(self.sei_payloads.iter_mut()) {
            let size = u32::try_from(bytes.len()).map_err(|_| "an SEI payload longer than 4 GiB".to_string())?;
            array.push(NV_ENC_SEI_PAYLOAD { payloadSize: size, payloadType: m.payload_type, payload: bytes.as_mut_ptr() });
        }
        self.sei_array = array.into_boxed_slice();
        let preset_fn = self.api.list.nvEncGetEncodePresetConfigEx.ok_or("no nvEncGetEncodePresetConfigEx")?;
        let init_fn = self.api.list.nvEncInitializeEncoder.ok_or("no nvEncInitializeEncoder")?;
        // SAFETY: zeroed parameter blocks with their versions; the preset call fills `preset`; the
        // configuration lives in a box that outlives `nvEncInitializeEncoder`, which copies it.
        unsafe {
            let mut preset: Box<NV_ENC_PRESET_CONFIG> = Box::new(std::mem::zeroed());
            preset.version = NV_ENC_PRESET_CONFIG_VER;
            preset.presetCfg.version = NV_ENC_CONFIG_VER;
            let s = preset_fn(self.enc, p.codec.guid(), NV_ENC_PRESET_P5_GUID, NV_ENC_TUNING_INFO_HIGH_QUALITY, &mut *preset);
            if s != NV_ENC_SUCCESS {
                return Err(status("reading the encoder preset", s, self.enc, self.api));
            }
            let mut cfg: Box<NV_ENC_CONFIG> = Box::new(preset.presetCfg);
            cfg.version = NV_ENC_CONFIG_VER;
            cfg.profileGUID = match (p.codec, p.profile) {
                (Codec::Hevc, _) if p.ten_bit => NV_ENC_HEVC_PROFILE_MAIN10_GUID,
                (Codec::Hevc, _) => NV_ENC_HEVC_PROFILE_MAIN_GUID,
                (Codec::H264, 0) => NV_ENC_H264_PROFILE_BASELINE_GUID,
                (Codec::H264, 1) => NV_ENC_H264_PROFILE_MAIN_GUID,
                (Codec::H264, _) => NV_ENC_H264_PROFILE_HIGH_GUID,
            };
            cfg.gopLength = p.gop.max(1);
            cfg.frameIntervalP = 1 + p.bframes as i32;
            let rc = &mut cfg.rcParams;
            rc.version = NV_ENC_RC_PARAMS_VER;
            rc.rateControlMode = if p.cbr { NV_ENC_PARAMS_RC_CBR } else { NV_ENC_PARAMS_RC_VBR };
            rc.averageBitRate = p.bitrate.saturating_mul(1000);
            rc.maxBitRate = if p.cbr { rc.averageBitRate } else { p.max_bitrate.max(p.bitrate).saturating_mul(1000) };
            // a one-second buffer, the usual for streaming-style rate control
            rc.vbvBufferSize = rc.maxBitRate;
            rc.vbvInitialDelay = rc.vbvBufferSize;
            match p.codec {
                Codec::H264 => {
                    let h = &mut cfg.encodeCodecConfig.h264Config;
                    h.idrPeriod = p.gop.max(1);
                    h.level = p.level.map_or(NV_ENC_LEVEL_AUTOSELECT, u32::from);
                    h.flags &= !(H264_OUTPUT_AUD | H264_REPEAT_SPSPPS);
                    h.entropyCodingMode = if p.profile == 0 { 2 } else { NV_ENC_H264_ENTROPY_CODING_MODE_CABAC };
                    h.maxNumRefFrames = 0;
                    // BT.709 limited range, like our own encoder's default signalling
                    let vui = &mut h.h264VUIParameters;
                    vui.videoSignalTypePresentFlag = 1;
                    vui.videoFormat = NV_ENC_VUI_VIDEO_FORMAT_UNSPECIFIED;
                    vui.videoFullRangeFlag = 0;
                    vui.colourDescriptionPresentFlag = 1;
                    vui.colourPrimaries = NV_ENC_VUI_COLOR_PRIMARIES_BT709;
                    vui.transferCharacteristics = NV_ENC_VUI_TRANSFER_CHARACTERISTIC_BT709;
                    vui.colourMatrix = NV_ENC_VUI_MATRIX_COEFFS_BT709;
                    vui.timingInfoPresentFlag = 1;
                    vui.numUnitInTicks = p.fps.1;
                    vui.timeScale = p.fps.0.saturating_mul(2);
                }
                Codec::Hevc => {
                    let h = &mut cfg.encodeCodecConfig.hevcConfig;
                    h.level = p.level.map_or(NV_ENC_LEVEL_AUTOSELECT, u32::from);
                    h.tier = NV_ENC_TIER_HEVC_MAIN;
                    h.minCUSize = NV_ENC_HEVC_CUSIZE_AUTOSELECT;
                    h.maxCUSize = NV_ENC_HEVC_CUSIZE_AUTOSELECT;
                    h.idrPeriod = p.gop.max(1);
                    // parameter sets only in the first IDR (and the hvcC), no access unit delimiters, 4:2:0 at 8 or 10 bits
                    h.flags &= !(HEVC_OUTPUT_AUD
                        | HEVC_DISABLE_SPSPPS
                        | HEVC_REPEAT_SPSPPS
                        | HEVC_ENABLE_INTRA_REFRESH
                        | HEVC_CHROMA_FORMAT_MASK
                        | HEVC_PIXEL_BIT_DEPTH_MASK);
                    h.flags |= 1 << HEVC_CHROMA_FORMAT_SHIFT;
                    if p.ten_bit {
                        h.flags |= 2 << HEVC_PIXEL_BIT_DEPTH_SHIFT;
                    }
                    h.maxNumRefFramesInDPB = 0;
                    // the stream's colour description (BT.709 unless the signal says otherwise), limited
                    // range; an HEVC VUI counts frames, not fields: frame rate = time_scale / num_units_in_tick
                    let vui = &mut h.hevcVUIParameters;
                    vui.videoSignalTypePresentFlag = 1;
                    vui.videoFormat = NV_ENC_VUI_VIDEO_FORMAT_UNSPECIFIED;
                    vui.videoFullRangeFlag = 0;
                    vui.colourDescriptionPresentFlag = 1;
                    vui.colourPrimaries = u32::from(p.signal.primaries);
                    vui.transferCharacteristics = u32::from(p.signal.transfer);
                    vui.colourMatrix = u32::from(p.signal.matrix);
                    // Let NVENC derive timing from frameRateNum/frameRateDen. Some drivers
                    // reverse the explicitly supplied HEVC VUI timing pair.
                    vui.timingInfoPresentFlag = 0;
                    vui.numUnitInTicks = 0;
                    vui.timeScale = 0;
                }
            }

            let mut init: Box<NV_ENC_INITIALIZE_PARAMS> = Box::new(std::mem::zeroed());
            init.version = NV_ENC_INITIALIZE_PARAMS_VER;
            init.encodeGUID = p.codec.guid();
            init.presetGUID = NV_ENC_PRESET_P5_GUID;
            init.encodeWidth = p.width;
            init.encodeHeight = p.height;
            let (sn, sd) = p.sar.unwrap_or((1, 1));
            init.darWidth = p.width.saturating_mul(sn.max(1));
            init.darHeight = p.height.saturating_mul(sd.max(1));
            init.frameRateNum = p.fps.0;
            init.frameRateDen = p.fps.1;
            init.enablePTD = 1;
            init.encodeConfig = &mut *cfg;
            init.tuningInfo = NV_ENC_TUNING_INFO_HIGH_QUALITY;
            init.bufferFormat = self.fmt;
            let s = init_fn(self.enc, &mut *init);
            if s != NV_ENC_SUCCESS {
                return Err(status("initialising the encoder", s, self.enc, self.api));
            }
        }
        self.size = (p.width, p.height);
        self.out_size = (p.width.saturating_mul(p.height).saturating_mul(2)).max(1 << 20);
        for _ in 0..ring.max(1) {
            self.add_slot()?;
        }
        Ok(())
    }

    fn add_slot(&mut self) -> Result<(), String> {
        let (create_in, create_out) = (
            self.api.list.nvEncCreateInputBuffer.ok_or("no nvEncCreateInputBuffer")?,
            self.api.list.nvEncCreateBitstreamBuffer.ok_or("no nvEncCreateBitstreamBuffer")?,
        );
        // SAFETY: zeroed parameter blocks with their versions; the driver fills in the buffer
        // handles, which `Drop` destroys.
        unsafe {
            let mut i: NV_ENC_CREATE_INPUT_BUFFER = std::mem::zeroed();
            i.version = NV_ENC_CREATE_INPUT_BUFFER_VER;
            i.width = self.size.0;
            i.height = self.size.1;
            i.memoryHeap = NV_ENC_MEMORY_HEAP_SYSMEM_CACHED;
            i.bufferFmt = self.fmt;
            let s = create_in(self.enc, &mut i);
            if s != NV_ENC_SUCCESS {
                return Err(status("creating an input buffer", s, self.enc, self.api));
            }
            let mut o: NV_ENC_CREATE_BITSTREAM_BUFFER = std::mem::zeroed();
            o.version = NV_ENC_CREATE_BITSTREAM_BUFFER_VER;
            o.size = self.out_size;
            o.memoryHeap = NV_ENC_MEMORY_HEAP_SYSMEM_CACHED;
            let s = create_out(self.enc, &mut o);
            if s != NV_ENC_SUCCESS {
                if let Some(d) = self.api.list.nvEncDestroyInputBuffer {
                    d(self.enc, i.inputBuffer);
                }
                return Err(status("creating an output buffer", s, self.enc, self.api));
            }
            self.slots.push(Slot { input: i.inputBuffer, output: o.bitstreamBuffer });
        }
        Ok(())
    }

    /// Number of buffer slots.
    pub fn slots(&self) -> usize {
        self.slots.len()
    }

    /// The parameter sets as one Annex B byte string (SPS and PPS; VPS, SPS and PPS for HEVC).
    pub fn sequence_params(&self) -> Result<Vec<u8>, String> {
        let f = self.api.list.nvEncGetSequenceParams.ok_or("no nvEncGetSequenceParams")?;
        let mut buf = vec![0u8; 1024];
        let mut len = 0u32;
        // SAFETY: a zeroed parameter block with its version; `buf` and `len` outlive the call and
        // the driver writes at most `inBufferSize` bytes into `buf`.
        unsafe {
            let mut p: NV_ENC_SEQUENCE_PARAM_PAYLOAD = std::mem::zeroed();
            p.version = NV_ENC_SEQUENCE_PARAM_PAYLOAD_VER;
            p.inBufferSize = buf.len() as u32;
            p.spsppsBuffer = buf.as_mut_ptr().cast();
            p.outSPSPPSPayloadSize = &mut len;
            let s = f(self.enc, &mut p);
            if s != NV_ENC_SUCCESS {
                return Err(status("reading the parameter sets", s, self.enc, self.api));
            }
        }
        buf.truncate(len as usize);
        Ok(buf)
    }

    /// Write a picture into slot `slot`'s input buffer through `fill` (which gets the locked
    /// buffer), then submit it with timestamp `pts`; `output_slot` receives the bitstream.
    pub fn submit(&mut self, slot: usize, pts: u64, fill: impl FnOnce(Locked<'_>)) -> Result<Submitted, String> {
        let s = self.slots.get(slot).ok_or("no such buffer slot")?;
        let (input, output) = (s.input, s.output);
        let (lock, unlock, encode) = (
            self.api.list.nvEncLockInputBuffer.ok_or("no nvEncLockInputBuffer")?,
            self.api.list.nvEncUnlockInputBuffer.ok_or("no nvEncUnlockInputBuffer")?,
            self.api.list.nvEncEncodePicture.ok_or("no nvEncEncodePicture")?,
        );
        let (w, h) = self.size;
        // SAFETY: the buffer handle is one this session created. `LockInputBuffer` returns a
        // writable block of `pitch * h * 3 / 2` bytes (NV12 or P010: luma rows then chroma rows, the pitch
        // in bytes), or `pitch * h` bytes (ABGR: packed rows only), that stays valid until
        // `UnlockInputBuffer`; the slice borrows it only inside `fill`.
        let pitch = unsafe {
            let mut l: NV_ENC_LOCK_INPUT_BUFFER = std::mem::zeroed();
            l.version = NV_ENC_LOCK_INPUT_BUFFER_VER;
            l.inputBuffer = input;
            let st = lock(self.enc, &mut l);
            if st != NV_ENC_SUCCESS || l.bufferDataPtr.is_null() || l.pitch == 0 {
                return Err(status("locking an input buffer", st, self.enc, self.api));
            }
            // a driver pitch narrower than a row (in bytes: P010 samples are two) would make `fill` write past each row
            if (l.pitch as usize) < (w as usize).saturating_mul(self.bytes_per_sample) {
                // the lock succeeded, so release it before giving up (its status no longer matters)
                let _ = unlock(self.enc, input);
                return Err(format!("the driver returned an input pitch of {} bytes for {w}-pixel rows of {}-byte samples", l.pitch, self.bytes_per_sample));
            }
            let rows = (l.pitch as usize).saturating_mul(h as usize);
            let len = if self.fmt == NV_ENC_BUFFER_FORMAT_ABGR { rows } else { rows.saturating_mul(3) / 2 };
            fill(Locked { data: std::slice::from_raw_parts_mut(l.bufferDataPtr.cast::<u8>(), len), pitch: l.pitch as usize });
            let st = unlock(self.enc, input);
            if st != NV_ENC_SUCCESS {
                return Err(status("unlocking an input buffer", st, self.enc, self.api));
            }
            l.pitch
        };
        self.encode_picture(input, output, pitch, self.fmt, pts, encode)
    }

    /// Submit one picture: `input` (a host input buffer of this session, or a mapped input
    /// picture) of `fmt`, its bitstream into `output`.
    fn encode_picture(
        &mut self,
        input: *mut c_void,
        output: *mut c_void,
        pitch: u32,
        fmt: u32,
        pts: u64,
        encode: unsafe extern "system" fn(Handle, *mut NV_ENC_PIC_PARAMS) -> NvStatus,
    ) -> Result<Submitted, String> {
        let (w, h) = self.size;
        // IDR pictures (every `gop` pictures from the first, in submission = display order) carry the SEI messages
        let idr = !self.sei_array.is_empty() && self.submitted.is_multiple_of(self.gop);
        self.submitted = self.submitted.saturating_add(1);
        // SAFETY: a zeroed parameter block with its version; the input and output are this session's
        // (a buffer it created or a picture it mapped). The SEI descriptor array and the payload
        // bytes it points to are heap blocks owned by `self`, which stay allocated (and unmoved)
        // until the session is dropped, so they outlive this call and whatever time the driver
        // keeps the picture queued.
        let st = unsafe {
            let mut e: NV_ENC_PIC_PARAMS = std::mem::zeroed();
            if idr {
                let h = &mut e.codecPicParams.hevcPicParams;
                h.seiPayloadArrayCnt = u32::try_from(self.sei_array.len()).unwrap_or(0);
                h.seiPayloadArray = self.sei_array.as_mut_ptr();
            }
            e.version = NV_ENC_PIC_PARAMS_VER;
            e.inputWidth = w;
            e.inputHeight = h;
            e.inputPitch = pitch;
            e.inputBuffer = input;
            e.outputBitstream = output;
            e.bufferFmt = fmt;
            e.pictureStruct = NV_ENC_PIC_STRUCT_FRAME;
            e.inputTimeStamp = pts;
            e.inputDuration = 1;
            encode(self.enc, &mut e)
        };
        match st {
            NV_ENC_SUCCESS => Ok(Submitted::Ready),
            NV_ENC_ERR_NEED_MORE_INPUT => Ok(Submitted::NeedMoreInput),
            s => Err(status("encoding a picture", s, self.enc, self.api)),
        }
    }

    /// The Direct3D 11 device the session runs on (the input pictures' textures are made on it).
    #[cfg(target_os = "windows")]
    pub fn d3d11(&self) -> &windows::Win32::Graphics::Direct3D11::ID3D11Device {
        self._device.d3d11()
    }

    /// Register `texture` (an `R8G8B8A8_UNORM` texture of [`Self::d3d11`] at the encoder's size)
    /// as an input picture of an RGBA-input session; its key, counted from 0 in registration
    /// order. The texture must stay alive until the session is dropped (which unregisters it) or
    /// [`Self::unregister_inputs`].
    #[cfg(target_os = "windows")]
    pub fn register_texture(&mut self, texture: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D) -> Result<usize, String> {
        use super::ffi::{NV_ENC_INPUT_IMAGE, NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX, NV_ENC_REGISTER_RESOURCE, NV_ENC_REGISTER_RESOURCE_VER};
        use windows::core::Interface;
        if self.fmt != NV_ENC_BUFFER_FORMAT_ABGR {
            return Err("input textures are RGBA (ABGR) pictures".into());
        }
        let f = self.api.list.nvEncRegisterResource.ok_or("no nvEncRegisterResource")?;
        // SAFETY: a zeroed parameter block with its version and a live texture of the session's
        // device, which the caller keeps alive while it is registered; the driver fills in the
        // registration, which `unregister_inputs` / `Drop` undo once.
        let registered = unsafe {
            let mut r: NV_ENC_REGISTER_RESOURCE = std::mem::zeroed();
            r.version = NV_ENC_REGISTER_RESOURCE_VER;
            r.resourceType = NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX;
            r.width = self.size.0;
            r.height = self.size.1;
            r.pitch = 0;
            r.subResourceIndex = 0;
            r.resourceToRegister = texture.as_raw();
            r.bufferFormat = NV_ENC_BUFFER_FORMAT_ABGR;
            r.bufferUsage = NV_ENC_INPUT_IMAGE;
            let s = f(self.enc, &mut r);
            if s != NV_ENC_SUCCESS || r.registeredResource.is_null() {
                return Err(status("registering an input texture", s, self.enc, self.api));
            }
            r.registeredResource
        };
        self.inputs.push(Input { registered, mapped: std::ptr::null_mut() });
        Ok(self.inputs.len() - 1)
    }

    /// Map input picture `key` and submit it with timestamp `pts`, its bitstream into slot `slot`'s
    /// output buffer (the slot's input buffer is not used). The picture stays mapped, and its
    /// texture must not be written, until [`Self::unmap`] once the bitstream that consumed it was
    /// read.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub fn submit_mapped(&mut self, slot: usize, pts: u64, key: usize) -> Result<Submitted, String> {
        let output = self.slots.get(slot).ok_or("no such buffer slot")?.output;
        let input = self.inputs.get(key).ok_or("no such input picture")?;
        if !input.mapped.is_null() {
            return Err("an input picture submitted while it is still in the encoder".into());
        }
        let registered = input.registered;
        let (map, encode) =
            (self.api.list.nvEncMapInputResource.ok_or("no nvEncMapInputResource")?, self.api.list.nvEncEncodePicture.ok_or("no nvEncEncodePicture")?);
        // SAFETY: a zeroed parameter block with its version and a resource this session registered;
        // the driver returns the mapping, which `unmap` (or `Drop`) undoes once.
        let (mapped, fmt) = unsafe {
            let mut m: NV_ENC_MAP_INPUT_RESOURCE = std::mem::zeroed();
            m.version = NV_ENC_MAP_INPUT_RESOURCE_VER;
            m.registeredResource = registered;
            let s = map(self.enc, &mut m);
            if s != NV_ENC_SUCCESS || m.mappedResource.is_null() {
                return Err(status("mapping an input texture", s, self.enc, self.api));
            }
            (m.mappedResource, m.mappedBufferFmt)
        };
        if let Some(i) = self.inputs.get_mut(key) {
            i.mapped = mapped;
        }
        // the driver knows the texture's layout: the pitch is the width, as the API asks when unknown
        let st = self.encode_picture(mapped, output, self.size.0, fmt, pts, encode);
        if st.is_err() {
            // the picture never went in
            let _ = self.unmap(key);
        }
        st
    }

    /// Unmap input picture `key` (nothing when it is not mapped).
    // (input textures are registered on Windows only)
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub fn unmap(&mut self, key: usize) -> Result<(), String> {
        let Some(input) = self.inputs.get_mut(key) else {
            return Err("no such input picture".into());
        };
        if input.mapped.is_null() {
            return Ok(());
        }
        let mapped = std::mem::replace(&mut input.mapped, std::ptr::null_mut());
        let f = self.api.list.nvEncUnmapInputResource.ok_or("no nvEncUnmapInputResource")?;
        // SAFETY: a mapping this session's `nvEncMapInputResource` returned, unmapped once.
        let s = unsafe { f(self.enc, mapped) };
        if s != NV_ENC_SUCCESS {
            return Err(status("unmapping an input texture", s, self.enc, self.api));
        }
        Ok(())
    }

    /// Unmap and unregister every input picture (their textures may then be destroyed).
    pub fn unregister_inputs(&mut self) {
        let (unmap, unregister) = (self.api.list.nvEncUnmapInputResource, self.api.list.nvEncUnregisterResource);
        for input in self.inputs.drain(..) {
            // SAFETY: mappings and registrations this session made, each undone once, the mapping first.
            unsafe {
                if !input.mapped.is_null()
                    && let Some(f) = unmap
                {
                    f(self.enc, input.mapped);
                }
                if let Some(f) = unregister {
                    f(self.enc, input.registered);
                }
            }
        }
    }

    /// Tell the encoder the stream ends: every picture it still holds becomes ready.
    pub fn end_of_stream(&mut self) -> Result<(), String> {
        let encode = self.api.list.nvEncEncodePicture.ok_or("no nvEncEncodePicture")?;
        // SAFETY: a zeroed parameter block with its version and the EOS flag (no input buffer).
        let st = unsafe {
            let mut e: NV_ENC_PIC_PARAMS = std::mem::zeroed();
            e.version = NV_ENC_PIC_PARAMS_VER;
            e.encodePicFlags = NV_ENC_PIC_FLAG_EOS;
            encode(self.enc, &mut e)
        };
        if st != NV_ENC_SUCCESS {
            return Err(status("ending the stream", st, self.enc, self.api));
        }
        Ok(())
    }

    /// Read the finished bitstream of slot `slot` (blocks until the encoder has it).
    pub fn read(&mut self, slot: usize) -> Result<Output, String> {
        let output = self.slots.get(slot).ok_or("no such buffer slot")?.output;
        let (lock, unlock) =
            (self.api.list.nvEncLockBitstream.ok_or("no nvEncLockBitstream")?, self.api.list.nvEncUnlockBitstream.ok_or("no nvEncUnlockBitstream")?);
        // SAFETY: a zeroed parameter block with its version and a buffer of this session; after a
        // successful lock `bitstreamBufferPtr` addresses `bitstreamSizeInBytes` readable bytes until
        // `UnlockBitstream`, and they are copied before it.
        unsafe {
            let mut l: NV_ENC_LOCK_BITSTREAM = std::mem::zeroed();
            l.version = NV_ENC_LOCK_BITSTREAM_VER;
            l.outputBitstream = output;
            let st = lock(self.enc, &mut l);
            if st != NV_ENC_SUCCESS {
                return Err(status("reading a bitstream", st, self.enc, self.api));
            }
            let n = l.bitstreamSizeInBytes as usize;
            let data =
                if l.bitstreamBufferPtr.is_null() || n == 0 { Vec::new() } else { std::slice::from_raw_parts(l.bitstreamBufferPtr.cast::<u8>(), n).to_vec() };
            let out = Output { data, pts: l.outputTimeStamp, pic_type: l.pictureType };
            let st = unlock(self.enc, output);
            if st != NV_ENC_SUCCESS {
                return Err(status("releasing a bitstream", st, self.enc, self.api));
            }
            Ok(out)
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // input textures first: they belong to the caller, who may destroy them after this
        self.unregister_inputs();
        // SAFETY: every handle was created by this session and is destroyed once, buffers first.
        unsafe {
            for s in self.slots.drain(..) {
                if let Some(d) = self.api.list.nvEncDestroyInputBuffer {
                    d(self.enc, s.input);
                }
                if let Some(d) = self.api.list.nvEncDestroyBitstreamBuffer {
                    d(self.enc, s.output);
                }
            }
            if let Some(d) = self.api.list.nvEncDestroyEncoder {
                d(self.enc);
            }
        }
    }
}
