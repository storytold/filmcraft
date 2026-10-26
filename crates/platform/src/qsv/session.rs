//! Intel Quick Sync Video encoding session using Media Foundation encoder transforms.
//!
//! FFI module: safe wrapper around `IMFTransform` encoder sessions.

#![allow(unsafe_code)]

#[cfg(target_os = "windows")]
use std::collections::VecDeque;
#[cfg(target_os = "windows")]
use std::ffi::c_void;
use std::sync::Arc;

#[cfg(target_os = "windows")]
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFMediaBuffer, IMFMediaType, IMFSample, IMFTransform, MF_E_NOTACCEPTING, MF_E_TRANSFORM_NEED_MORE_INPUT, MF_MT_AVG_BITRATE,
    MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_SA_D3D11_AWARE, MFMediaType_Video,
    MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG, MFT_ENUM_FLAG_ASYNCMFT, MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT,
    MFT_ENUM_HARDWARE_VENDOR_ID_Attribute, MFT_FRIENDLY_NAME_Attribute, MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER, MFT_REGISTER_TYPE_INFO, MFVideoFormat_AV1,
    MFVideoFormat_H264, MFVideoFormat_HEVC, MFVideoFormat_NV12, MFVideoFormat_P010, MFVideoInterlace_Progressive,
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
            let hr = self.transform.ProcessInput(0, &sample, 0);
            if let Err(e) = hr {
                if e.code() == MF_E_NOTACCEPTING {
                    // Queue is full: drain pending outputs first and retry
                    let _ = self.drain_outputs();
                    self.transform.ProcessInput(0, &sample, 0).map_err(|e| format!("Intel encoder ProcessInput failed: {e}"))?;
                } else {
                    return Err(format!("Intel encoder ProcessInput failed: {e}"));
                }
            }
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

            match hr {
                Ok(()) => {}
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => break,
                Err(e) => {
                    log::debug!("Intel ProcessOutput ended: {e}");
                    break;
                }
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
        let _ = IntelGpu::shared()?;
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

#[cfg(target_os = "windows")]
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

#[cfg(target_os = "windows")]
fn make_empty_buffer(api: &MfApi, len: u32) -> Result<IMFMediaBuffer, String> {
    let mut raw_buf = std::ptr::null_mut();
    // SAFETY: CreateMemoryBuffer call.
    let hr = unsafe { (api.create_memory_buffer)(len, &mut raw_buf) };
    super::super::media_foundation::mft::made(hr, raw_buf, "MFCreateMemoryBuffer")
}

#[cfg(target_os = "windows")]
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

#[cfg(target_os = "windows")]
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
            // NAL type 5 is IDR. Scan for 3- or 4-byte start codes (00 00 01)
            for i in 0..data.len().saturating_sub(3) {
                if data[i..i + 3] == [0, 0, 1]
                    && let Some(&b) = data.get(i + 3)
                {
                    let nal_type = b & 0x1F;
                    if nal_type == 5 {
                        return true;
                    }
                }
            }
            false
        }
        Codec::Hevc => {
            // NAL types 19 (IDR_W_RADL), 20 (IDR_N_LP), 21 (CRA_NUT) are key/RAP
            for i in 0..data.len().saturating_sub(3) {
                if data[i..i + 3] == [0, 0, 1]
                    && let Some(&b) = data.get(i + 3)
                {
                    let nal_type = (b >> 1) & 0x3F;
                    if nal_type == 19 || nal_type == 20 || nal_type == 21 {
                        return true;
                    }
                }
            }
            false
        }
        Codec::Av1 => {
            // Parse OBUs: check for sequence header (1) or frame header with key_frame (3, 6)
            let mut pos = 0usize;
            while pos < data.len() {
                let b0 = data[pos];
                if (b0 >> 7) & 1 != 0 {
                    break;
                }
                let obu_type = (b0 >> 3) & 0x0F;
                let extension_flag = (b0 >> 2) & 1;
                let has_size = (b0 >> 1) & 1;
                pos += 1;
                if extension_flag != 0 {
                    if pos >= data.len() {
                        break;
                    }
                    pos += 1;
                }
                let payload_start = if has_size != 0 {
                    let mut payload_len = 0usize;
                    let mut shift = 0;
                    let mut valid = false;
                    while pos < data.len() {
                        let b = data[pos];
                        pos += 1;
                        payload_len |= ((b & 0x7F) as usize) << shift;
                        shift += 7;
                        if (b & 0x80) == 0 {
                            valid = true;
                            break;
                        }
                    }
                    if !valid {
                        break;
                    }
                    let start = pos;
                    pos = pos.saturating_add(payload_len);
                    start
                } else {
                    let start = pos;
                    pos = data.len();
                    start
                };

                if obu_type == 1 {
                    return true;
                }
                if (obu_type == 3 || obu_type == 6)
                    && let Some(&p0) = data.get(payload_start)
                {
                    let show_existing_frame = (p0 >> 7) & 1;
                    let frame_type = (p0 >> 5) & 3;
                    if show_existing_frame == 0 && frame_type == 0 {
                        return true;
                    }
                }
            }
            false
        }
    }
}

#[cfg(target_os = "windows")]
fn is_intel_activate(a: &IMFActivate) -> bool {
    // SAFETY: COM calls on activate object to read attributes.
    unsafe {
        let mut s = windows::core::PWSTR::null();
        let mut len = 0u32;
        if a.GetAllocatedString(&MFT_ENUM_HARDWARE_VENDOR_ID_Attribute, &mut s, &mut len).is_ok() {
            let res = s.to_string().ok();
            CoTaskMemFree(Some(s.0 as *const c_void));
            if let Some(v) = res {
                let lower = v.to_ascii_lowercase();
                if lower.contains("8086") {
                    return true;
                }
            }
        }
        if a.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut s, &mut len).is_ok() {
            let res = s.to_string().ok();
            CoTaskMemFree(Some(s.0 as *const c_void));
            if let Some(name) = res {
                let lower = name.to_ascii_lowercase();
                if lower.contains("intel") || lower.contains("quick sync") || lower.contains("qsv") {
                    return true;
                }
            }
        }
    }
    false
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
    // Filter for Intel hardware encoder, properly taking ownership of COM activate objects
    let mut selected = None;
    for i in 0..count as usize {
        // SAFETY: pointer offset is within 0..count elements allocated by MFTEnumEx.
        let p = unsafe { *list.add(i) };
        if p.is_null() {
            continue;
        }
        // SAFETY: take ownership of IMFActivate pointer from MFTEnumEx array.
        let activate = unsafe { IMFActivate::from_raw(p) };
        if selected.is_none() && is_intel_activate(&activate) {
            selected = Some(activate);
        }
    }
    // SAFETY: free list array allocated with CoTaskMemAlloc.
    unsafe { CoTaskMemFree(Some(list as *const c_void)) };
    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_frame_detection_h264() {
        // NAL 5 = IDR
        let idr = [0, 0, 0, 1, 0x65, 0x88, 0x84];
        assert!(is_key_frame(Codec::H264, &idr));
        // NAL 1 = Non-IDR slice
        let non_idr = [0, 0, 0, 1, 0x61, 0x88, 0x84];
        assert!(!is_key_frame(Codec::H264, &non_idr));
        // 3-byte start code
        let idr_3 = [0, 0, 1, 0x65, 0x12];
        assert!(is_key_frame(Codec::H264, &idr_3));
    }

    #[test]
    fn key_frame_detection_hevc() {
        // NAL 19 = IDR_W_RADL (0x26 = (19 << 1))
        let idr19 = [0, 0, 0, 1, 0x26, 0x01];
        assert!(is_key_frame(Codec::Hevc, &idr19));
        // NAL 20 = IDR_N_LP (0x28 = (20 << 1))
        let idr20 = [0, 0, 0, 1, 0x28, 0x01];
        assert!(is_key_frame(Codec::Hevc, &idr20));
        // NAL 21 = CRA_NUT (0x2A = (21 << 1))
        let cra21 = [0, 0, 0, 1, 0x2A, 0x01];
        assert!(is_key_frame(Codec::Hevc, &cra21));
        // NAL 1 = TRAIL_R (0x02 = (1 << 1))
        let non_idr = [0, 0, 0, 1, 0x02, 0x01];
        assert!(!is_key_frame(Codec::Hevc, &non_idr));
    }

    #[test]
    fn key_frame_detection_av1() {
        // OBU Sequence Header (type 1)
        // b0: (1 << 3) | 2 (has_size = 1) -> 0x0A
        let seq_obu = [0x0A, 0x01, 0x00];
        assert!(is_key_frame(Codec::Av1, &seq_obu));
        // OBU Frame (type 6) with key_frame:
        // b0: (6 << 3) | 2 -> 0x32, size 1, payload: show_existing=0, type=0 -> 0x00
        let frame_key = [0x32, 0x01, 0x00];
        assert!(is_key_frame(Codec::Av1, &frame_key));
        // OBU Frame with inter frame:
        // payload: show_existing=0, type=1 -> (1 << 5) = 0x20
        let frame_inter = [0x32, 0x01, 0x20];
        assert!(!is_key_frame(Codec::Av1, &frame_inter));
    }
}
