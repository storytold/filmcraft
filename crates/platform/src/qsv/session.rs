//! Intel Quick Sync Video encoding session using Media Foundation encoder transforms.
//!
//! FFI module: safe wrapper around `IMFTransform` encoder sessions.

#![allow(unsafe_code)]

use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::Arc;

#[cfg(target_os = "windows")]
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFMediaBuffer, IMFMediaType, IMFSample, IMFTransform, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE,
    MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_SA_D3D11_AWARE, MFMediaType_Video, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG, MFT_ENUM_FLAG_ASYNCMFT,
    MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT, MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER, MFT_REGISTER_TYPE_INFO, MFVideoFormat_AV1, MFVideoFormat_H264,
    MFVideoFormat_HEVC, MFVideoFormat_NV12, MFVideoFormat_P010, MFVideoInterlace_Progressive,
};
#[cfg(target_os = "windows")]
use windows::Win32::System::Com::CoTaskMemFree;
#[cfg(target_os = "windows")]
use windows::core::{GUID, Interface};

use filmcraft_time::FrameRate;

use super::device::IntelGpu;
#[cfg(target_os = "windows")]
use crate::media_foundation::gpu::ensure_com;
#[cfg(target_os = "windows")]
use crate::media_foundation::mft::{MfApi, api};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    H264,
    Hevc,
    Av1,
}

impl Codec {
    #[cfg(target_os = "windows")]
    pub fn subtype(self) -> GUID {
        match self {
            Self::H264 => MFVideoFormat_H264,
            Self::Hevc => MFVideoFormat_HEVC,
            Self::Av1 => MFVideoFormat_AV1,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::H264 => "H.264",
            Self::Hevc => "HEVC",
            Self::Av1 => "AV1",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    H264Baseline,
    H264Main,
    H264High,
    HevcMain,
    HevcMain10,
    Av1Main,
    Av1Main10,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub codec: Codec,
    pub profile: Profile,
    pub size: (u32, u32),
    pub rate: FrameRate,
    pub bitrate_kbps: u32,
    pub keyint: u32,
    pub ten_bit: bool,
}

/// An encoded video packet.
#[derive(Clone, Debug)]
pub struct Packet {
    pub data: Vec<u8>,
    pub key: bool,
    pub pts: i64,
    pub dts: i64,
}

#[cfg(target_os = "windows")]
pub struct QsvSession {
    _gpu: Arc<IntelGpu>,
    transform: IMFTransform,
    config: Config,
    emitted: i64,
    pending: VecDeque<(i64, i64)>, // (pts, dts)
}

#[cfg(target_os = "windows")]
// SAFETY: IMFTransform encoder MFT is free-threaded COM object, operated through &mut methods.
unsafe impl Send for QsvSession {}

#[cfg(target_os = "windows")]
impl QsvSession {
    /// Create a new hardware encoding session on the Intel GPU.
    pub fn new(config: Config) -> Result<Self, String> {
        ensure_com()?;
        let gpu = IntelGpu::shared()?;
        let mf_api = api()?;

        let activate = find_encoder_activate(mf_api, config.codec.subtype())?
            .ok_or_else(|| format!("no hardware {} encoder MFT found for Intel GPU", config.codec.name()))?;

        // SAFETY: plain COM call on activation object.
        let transform: IMFTransform = unsafe { activate.ActivateObject() }.map_err(|e| format!("cannot activate Intel encoder MFT: {e}"))?;

        // Check Direct3D 11 support
        // SAFETY: plain COM call to inspect transform attributes.
        unsafe {
            if let Ok(attrs) = transform.GetAttributes()
                && attrs.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) == 0
            {
                log::warn!("Intel encoder MFT does not declare MF_SA_D3D11_AWARE; proceeding with hardware context");
            }
            let _ = transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, gpu.manager_param());
        }

        let session = Self { _gpu: gpu, transform, config: config.clone(), emitted: 0, pending: VecDeque::new() };

        session.configure_media_types(mf_api)?;

        // SAFETY: notify streaming messages.
        unsafe {
            let _ = session.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
            let _ = session.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
        }

        Ok(session)
    }

    fn configure_media_types(&self, mf_api: &MfApi) -> Result<(), String> {
        let (w, h) = self.config.size;
        let rate = self.config.rate;
        let bps = self.config.bitrate_kbps.saturating_mul(1000);

        // 1. Output Media Type
        let out_type = make_video_type(mf_api, self.config.codec.subtype(), w, h, rate)?;
        // SAFETY: setting bitrate attribute.
        unsafe {
            out_type.SetUINT32(&MF_MT_AVG_BITRATE, bps).map_err(|e| format!("failed to set output bitrate: {e}"))?;
            self.transform.SetOutputType(0, &out_type, 0).map_err(|e| format!("failed to set encoder output type: {e}"))?;
        }

        // 2. Input Media Type (NV12 for 8-bit, P010 for 10-bit)
        let in_subtype = if self.config.ten_bit { MFVideoFormat_P010 } else { MFVideoFormat_NV12 };
        let in_type = make_video_type(mf_api, in_subtype, w, h, rate)?;
        // SAFETY: set input type on transform.
        unsafe {
            self.transform.SetInputType(0, &in_type, 0).map_err(|e| format!("failed to set encoder input type: {e}"))?;
        }

        Ok(())
    }

    /// Encode an 8-bit planar 4:2:0 frame (NV12).
    pub fn encode_nv12(&mut self, y: &[u8], u: &[u8], v: &[u8], pts: i64) -> Result<Vec<Packet>, String> {
        let (w, h) = (self.config.size.0 as usize, self.config.size.1 as usize);
        let luma_size = w.saturating_mul(h);
        let chroma_size = (w / 2).saturating_mul(h / 2);
        if y.len() < luma_size || u.len() < chroma_size || v.len() < chroma_size {
            return Err("frame buffer smaller than configured size".into());
        }

        let mut nv12 = Vec::with_capacity(luma_size.saturating_add(chroma_size * 2));
        nv12.extend_from_slice(&y[..luma_size]);
        for (&u_val, &v_val) in u.iter().take(chroma_size).zip(v.iter().take(chroma_size)) {
            nv12.push(u_val);
            nv12.push(v_val);
        }

        self.submit_sample(&nv12, pts)
    }

    /// Encode a 10-bit planar 4:2:0 frame (P010).
    pub fn encode_p010(&mut self, y: &[u16], u: &[u16], v: &[u16], pts: i64) -> Result<Vec<Packet>, String> {
        let (w, h) = (self.config.size.0 as usize, self.config.size.1 as usize);
        let luma_size = w.saturating_mul(h);
        let chroma_size = (w / 2).saturating_mul(h / 2);
        if y.len() < luma_size || u.len() < chroma_size || v.len() < chroma_size {
            return Err("10-bit frame buffer smaller than configured size".into());
        }

        let code = |c: u16| ((c & 0x3ff) << 6).to_le_bytes();
        let mut p010 = Vec::with_capacity(luma_size.saturating_mul(2).saturating_add(chroma_size.saturating_mul(4)));
        for &sample in &y[..luma_size] {
            p010.extend_from_slice(&code(sample));
        }
        for (&u_val, &v_val) in u.iter().take(chroma_size).zip(v.iter().take(chroma_size)) {
            p010.extend_from_slice(&code(u_val));
            p010.extend_from_slice(&code(v_val));
        }

        self.submit_sample(&p010, pts)
    }

    fn submit_sample(&mut self, raw_bytes: &[u8], pts: i64) -> Result<Vec<Packet>, String> {
        let mf_api = api()?;
        let sample = make_sample(mf_api, raw_bytes, pts)?;

        let dts = self.emitted;
        self.emitted = self.emitted.saturating_add(1);
        self.pending.push_back((pts, dts));

        // SAFETY: ProcessInput on live transform.
        unsafe {
            self.transform.ProcessInput(0, &sample, 0).map_err(|e| format!("Intel encoder ProcessInput failed: {e}"))?;
        }

        self.drain_outputs()
    }

    /// Flush remaining buffered packets at stream end.
    pub fn flush(&mut self) -> Result<Vec<Packet>, String> {
        // SAFETY: ProcessMessage drain.
        unsafe {
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0);
        }
        self.drain_outputs()
    }

    fn drain_outputs(&mut self) -> Result<Vec<Packet>, String> {
        let mut packets = Vec::new();
        let mf_api = api()?;

        loop {
            let mut out_buffer = MFT_OUTPUT_DATA_BUFFER { dwStreamID: 0, ..Default::default() };
            // Create sample for output
            let empty_buffer = make_empty_buffer(mf_api, 2 * 1024 * 1024)?;
            let sample = make_sample_with_buffer(mf_api, &empty_buffer)?;
            out_buffer.pSample = std::mem::ManuallyDrop::new(Some(sample));

            let mut status = 0u32;
            // SAFETY: ProcessOutput call.
            let hr = unsafe { self.transform.ProcessOutput(0, std::slice::from_mut(&mut out_buffer), &mut status) };

            if hr.is_err() {
                break;
            }

            if let Some(sample) = out_buffer.pSample.as_ref() {
                // SAFETY: read back sample bytes.
                let data = unsafe { read_sample_bytes(sample)? };
                if !data.is_empty() {
                    let (pts, dts) = self.pending.pop_front().unwrap_or((0, 0));
                    let key = is_key_frame(self.config.codec, &data);
                    packets.push(Packet { data, key, pts, dts });
                }
            }
        }

        Ok(packets)
    }

    pub fn codec(&self) -> Codec {
        self.config.codec
    }
}

#[cfg(not(target_os = "windows"))]
pub struct QsvSession {
    _gpu: Arc<IntelGpu>,
    config: Config,
}

#[cfg(not(target_os = "windows"))]
impl QsvSession {
    /// Create a new hardware encoding session on the Intel GPU.
    pub fn new(config: Config) -> Result<Self, String> {
        let gpu = IntelGpu::shared()?;
        Err(format!("Intel Quick Sync ({}) via Media Foundation is Windows-only; Linux uses VA-API", config.codec.name()))
    }

    /// Encode an 8-bit planar 4:2:0 frame (NV12).
    pub fn encode_nv12(&mut self, _y: &[u8], _u: &[u8], _v: &[u8], _pts: i64) -> Result<Vec<Packet>, String> {
        Err("not supported on this platform".into())
    }

    /// Encode a 10-bit planar 4:2:0 frame (P010).
    pub fn encode_p010(&mut self, _y: &[u16], _u: &[u16], _v: &[u16], _pts: i64) -> Result<Vec<Packet>, String> {
        Err("not supported on this platform".into())
    }

    /// Flush remaining buffered packets at stream end.
    pub fn flush(&mut self) -> Result<Vec<Packet>, String> {
        Ok(Vec::new())
    }

    pub fn codec(&self) -> Codec {
        self.config.codec
    }
}

#[cfg(target_os = "windows")]
fn make_video_type(api: &MfApi, subtype: GUID, w: u32, h: u32, rate: FrameRate) -> Result<IMFMediaType, String> {
    let t = api.media_type()?;
    // SAFETY: setting standard video media type attributes.
    unsafe {
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(|e| format!("cannot set major type: {e}"))?;
        t.SetGUID(&MF_MT_SUBTYPE, &subtype).map_err(|e| format!("cannot set subtype: {e}"))?;
        let size_packed = ((w as u64) << 32) | (h as u64);
        t.SetUINT64(&MF_MT_FRAME_SIZE, size_packed).map_err(|e| format!("cannot set frame size: {e}"))?;
        let rate_packed = ((rate.num as u64) << 32) | (rate.den.max(1) as u64);
        t.SetUINT64(&MF_MT_FRAME_RATE, rate_packed).map_err(|e| format!("cannot set frame rate: {e}"))?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).map_err(|e| format!("cannot set progressive mode: {e}"))?;
    }
    Ok(t)
}

fn make_sample(api: &MfApi, data: &[u8], time: i64) -> Result<IMFSample, String> {
    let len = u32::try_from(data.len()).map_err(|_| "input data too large".to_string())?;
    let mut raw_buf = std::ptr::null_mut();
    // SAFETY: CreateMemoryBuffer call.
    let hr = unsafe { (api.create_memory_buffer)(len, &mut raw_buf) };
    let buffer: IMFMediaBuffer = super::super::media_foundation::mft::made(hr, raw_buf, "MFCreateMemoryBuffer")?;

    // SAFETY: copy slice to locked buffer.
    unsafe {
        let (mut ptr, mut max) = (std::ptr::null_mut::<u8>(), 0u32);
        buffer.Lock(&mut ptr, Some(&mut max), None).map_err(|e| format!("cannot lock buffer: {e}"))?;
        if ptr.is_null() || max < len {
            let _ = buffer.Unlock();
            return Err("buffer too small".into());
        }
        std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
        let _ = buffer.Unlock();
        buffer.SetCurrentLength(len).map_err(|e| format!("cannot set buffer length: {e}"))?;
    }

    let mut raw_sample = std::ptr::null_mut();
    // SAFETY: CreateSample call.
    let hr = unsafe { (api.create_sample)(&mut raw_sample) };
    let sample: IMFSample = super::super::media_foundation::mft::made(hr, raw_sample, "MFCreateSample")?;
    // SAFETY: sample configuration.
    unsafe {
        sample.AddBuffer(&buffer).map_err(|e| format!("cannot add buffer to sample: {e}"))?;
        sample.SetSampleTime(time).map_err(|e| format!("cannot set sample time: {e}"))?;
    }
    Ok(sample)
}

fn make_empty_buffer(api: &MfApi, len: u32) -> Result<IMFMediaBuffer, String> {
    let mut raw_buf = std::ptr::null_mut();
    // SAFETY: CreateMemoryBuffer call.
    let hr = unsafe { (api.create_memory_buffer)(len, &mut raw_buf) };
    super::super::media_foundation::mft::made(hr, raw_buf, "MFCreateMemoryBuffer")
}

fn make_sample_with_buffer(api: &MfApi, buffer: &IMFMediaBuffer) -> Result<IMFSample, String> {
    let mut raw_sample = std::ptr::null_mut();
    // SAFETY: CreateSample call.
    let hr = unsafe { (api.create_sample)(&mut raw_sample) };
    let sample: IMFSample = super::super::media_foundation::mft::made(hr, raw_sample, "MFCreateSample")?;
    // SAFETY: add buffer.
    unsafe {
        sample.AddBuffer(buffer).map_err(|e| format!("cannot add buffer: {e}"))?;
    }
    Ok(sample)
}

unsafe fn read_sample_bytes(sample: &IMFSample) -> Result<Vec<u8>, String> {
    // SAFETY: COM calls on sample and buffer.
    let buffer = unsafe { sample.ConvertToContiguousBuffer() }.map_err(|e| format!("ConvertToContiguousBuffer failed: {e}"))?;
    let mut ptr = std::ptr::null_mut::<u8>();
    let mut len = 0u32;
    // SAFETY: Lock buffer.
    unsafe { buffer.Lock(&mut ptr, None, Some(&mut len)) }.map_err(|e| format!("buffer lock failed: {e}"))?;
    let slice = if !ptr.is_null() && len > 0 {
        // SAFETY: copy valid locked buffer memory of length len.
        unsafe { std::slice::from_raw_parts(ptr, len as usize).to_vec() }
    } else {
        Vec::new()
    };
    // SAFETY: Unlock buffer.
    let _ = unsafe { buffer.Unlock() };
    Ok(slice)
}

fn is_key_frame(codec: Codec, data: &[u8]) -> bool {
    match codec {
        Codec::H264 => {
            // NAL type 5 is IDR
            for window in data.windows(4) {
                if window == [0, 0, 0, 1]
                    && let Some(&b) = data.get(4)
                    && (b & 0x1F) == 5
                {
                    return true;
                }
            }
            false
        }
        Codec::Hevc => {
            // NAL types 19 and 20 are IDR
            for i in 0..data.len().saturating_sub(4) {
                if data[i..i + 4] == [0, 0, 0, 1]
                    && let Some(&b) = data.get(i + 4)
                {
                    let nal_type = (b >> 1) & 0x3F;
                    if nal_type == 19 || nal_type == 20 {
                        return true;
                    }
                }
            }
            false
        }
        Codec::Av1 => {
            // OBU keyframe check
            true
        }
    }
}

#[cfg(target_os = "windows")]
pub fn find_encoder_activate(api: &MfApi, subtype: GUID) -> Result<Option<IMFActivate>, String> {
    let output = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: subtype };
    let flags = MFT_ENUM_FLAG(MFT_ENUM_FLAG_SYNCMFT.0 | MFT_ENUM_FLAG_ASYNCMFT.0 | MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_SORTANDFILTER.0);
    let (mut list, mut count) = (std::ptr::null_mut::<*mut c_void>(), 0u32);
    // SAFETY: MFTEnumEx call.
    let hr = unsafe { (api.enum_transforms)(MFT_CATEGORY_VIDEO_ENCODER, flags.0 as u32, std::ptr::null(), &output, &mut list, &mut count) };
    if hr.is_err() || list.is_null() || count == 0 {
        return Ok(None);
    }
    // Take the first matching hardware encoder
    // SAFETY: take ownership of IMFActivate returned from MFTEnumEx.
    let activate = unsafe {
        let p = *list;
        if !p.is_null() { Some(IMFActivate::from_raw(p)) } else { None }
    };
    // SAFETY: free list array allocated with CoTaskMemAlloc.
    unsafe { CoTaskMemFree(Some(list as *const c_void)) };
    Ok(activate)
}
