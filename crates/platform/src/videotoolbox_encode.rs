//! VideoToolbox (macOS) hardware H.264, HEVC, ProRes and Motion JPEG encoding.
//!
//! An FFI module of this crate (docs/adr/0001-platform-ffi.md): every `unsafe` block has a
//! `// SAFETY:` comment, no panic may unwind into VideoToolbox (the output callback runs under
//! `catch_unwind`), and every failure is a `Result` the caller answers by using the built-in
//! encoder instead.
//!
//! A compression session is created with a hardware encoder *required*, so a machine without one
//! is declined up front. Frame reordering (B-frames) is off, so compressed frames come out in
//! presentation order with decode time = presentation time.
//!
//! SDR H.264/Main pictures go in as 8-bit 4:2:0 (NV12) pixel buffers filled from planar BT.709 limited-range
//! Y'CbCr, the layout of `filmcraft_export::rgba_to_yuv420_8`, so the colours match the built-in
//! encoder's. VideoToolbox encodes asynchronously; its output callback (on a VideoToolbox thread)
//! copies each compressed frame (length-prefixed NAL units) into a queue that [`VtEncoder::encode`]
//! and [`VtEncoder::flush`] drain.
//!
//! The first frame is completed straight away: the parameter sets the container needs (SPS / PPS
//! for H.264, VPS / SPS / PPS and VideoToolbox's own `hvcC` record for HEVC) come with the first
//! compressed frame, and the muxer asks for them after the first group of frames.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::panic::AssertUnwindSafe;
use std::ptr::NonNull;
use std::sync::{Mutex, PoisonError};

use objc2_core_foundation::{CFArray, CFBoolean, CFData, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_media::{
    CMFormatDescription, CMSampleBuffer, CMTime, CMTimeFlags, CMVideoFormatDescriptionGetH264ParameterSetAtIndex,
    CMVideoFormatDescriptionGetHEVCParameterSetAtIndex, kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms, kCMVideoCodecType_H264,
    kCMVideoCodecType_HEVC,
};
use objc2_core_video::{
    CVImageBuffer, CVPixelBufferCreate, CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetHeightOfPlane,
    CVPixelBufferGetWidthOfPlane, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferPool, CVPixelBufferUnlockBaseAddress,
    kCVPixelBufferHeightKey, kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey,
};
use objc2_video_toolbox::{
    VTCompressionSession, VTEncodeInfoFlags, VTSessionCopyProperty, VTSessionSetProperty, kVTCompressionPropertyKey_AllowFrameReordering,
    kVTCompressionPropertyKey_AllowOpenGOP, kVTCompressionPropertyKey_AverageBitRate, kVTCompressionPropertyKey_ColorPrimaries,
    kVTCompressionPropertyKey_DataRateLimits, kVTCompressionPropertyKey_ExpectedFrameRate, kVTCompressionPropertyKey_MaxKeyFrameInterval,
    kVTCompressionPropertyKey_ProfileLevel, kVTCompressionPropertyKey_RealTime, kVTCompressionPropertyKey_TransferFunction,
    kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder, kVTCompressionPropertyKey_YCbCrMatrix, kVTProfileLevel_H264_Baseline_AutoLevel,
    kVTProfileLevel_H264_High_AutoLevel, kVTProfileLevel_H264_Main_AutoLevel, kVTProfileLevel_HEVC_Main_AutoLevel,
    kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
};
use rayon::prelude::*;

/// The pixel format of the buffers we hand over: biplanar Y + interleaved CbCr, video range.
const NV12_VIDEO: u32 = u32::from_be_bytes(*b"420v");
/// Largest compressed frame accepted from the callback (a sanity cap on a size we do not control).
const MAX_FRAME_BYTES: usize = 256 << 20;
/// Largest picture the hardware encoder is asked for.
const MAX_SIDE: u32 = 8192;

/// The codec of a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VtCodec {
    H264,
    Hevc,
    ProRes,
    Jpeg,
}

/// H.264 and HEVC profiles VideoToolbox offers (the profile also picks the codec).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VtProfile {
    Baseline,
    Main,
    High,
    /// HEVC Main: 8-bit 4:2:0.
    HevcMain,
    HevcMain10,
    HevcMain42210,
    /// A ProRes sample-entry fourcc (validated before creating a session).
    ProRes([u8; 4]),
    Jpeg,
}

impl VtProfile {
    pub fn codec(self) -> VtCodec {
        match self {
            VtProfile::Baseline | VtProfile::Main | VtProfile::High => VtCodec::H264,
            VtProfile::HevcMain | VtProfile::HevcMain10 | VtProfile::HevcMain42210 => VtCodec::Hevc,
            VtProfile::ProRes(_) => VtCodec::ProRes,
            VtProfile::Jpeg => VtCodec::Jpeg,
        }
    }
}

impl VtProfile {
    /// Choose the CoreVideo input layout required by the profile, including packed AYUV16 for ProRes 4444/XQ.
    pub fn pixel_format(self) -> u32 {
        match self {
            Self::HevcMain10 => u32::from_be_bytes(*b"x420"),
            Self::HevcMain42210 => u32::from_be_bytes(*b"x422"),
            Self::ProRes(c) if matches!(&c, b"ap4h" | b"ap4x") => u32::from_be_bytes(*b"y416"),
            Self::ProRes(_) => u32::from_be_bytes(*b"x422"),
            _ => NV12_VIDEO,
        }
    }

    /// Whether the encoder accepts the 10-bit plane/conversion path rather than 8-bit NV12 input.
    pub fn ten_bit(self) -> bool {
        matches!(self, Self::HevcMain10 | Self::HevcMain42210 | Self::ProRes(_))
    }
}

/// Additional options; the original constructor keeps its SDR defaults.
#[derive(Clone, Copy, Debug)]
pub struct VtOptions {
    pub signal: filmcraft_export::ColorSignal,
    pub alpha: bool,
    pub quality: u8,
}

impl Default for VtOptions {
    /// Use SDR color, no alpha, and quality 90 for callers that do not supply additional options.
    fn default() -> Self {
        Self { signal: Default::default(), alpha: false, quality: 90 }
    }
}

/// How the bitrate is controlled (kilobits per second).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VtRate {
    /// Constant bitrate.
    Cbr { kbps: u32 },
    /// Average bitrate with a ceiling over one second.
    Vbr { target_kbps: u32, max_kbps: u32 },
}

/// What to encode and how. Times are in a timescale of `timescale` units per second, one frame
/// lasting `frame_duration` units (the frame rate's numerator and denominator).
#[derive(Clone, Debug)]
pub struct VtConfig {
    /// Picture size in luma samples: even, at most 8192 each way.
    pub width: u32,
    pub height: u32,
    pub timescale: u32,
    pub frame_duration: u32,
    /// Frames between keyframes (every keyframe is an IDR picture).
    pub keyframe_interval: u32,
    /// The profile, which also says whether this is an H.264 or an HEVC session.
    pub profile: VtProfile,
    pub rate: VtRate,
}

/// One compressed frame in decoding order, which is presentation order (times in the config's timescale).
#[derive(Clone, Debug)]
pub struct VtPacket {
    /// Length-prefixed NAL units (4-byte lengths, as in an `avcC` / `hvcC` MP4 sample).
    pub data: Vec<u8>,
    pub key: bool,
    pub pts: i64,
    pub dts: i64,
}

/// The parameter sets of the stream (NAL units without length prefix), for the `avcC` / `hvcC` box.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParamSets {
    /// HEVC only.
    pub vps: Vec<Vec<u8>>,
    pub sps: Vec<Vec<u8>>,
    pub pps: Vec<Vec<u8>>,
    /// HEVC only: the `hvcC` record (HEVCDecoderConfigurationRecord) VideoToolbox wrote for the stream.
    pub hvcc: Option<Vec<u8>>,
}

/// One output-callback result.
enum Output {
    Packet(VtPacket),
    Failed(String),
}

/// State shared with the output callback (its `outputCallbackRefCon`). Boxed by the encoder and
/// kept alive until the session is invalidated.
struct Shared {
    codec: VtCodec,
    timescale: i64,
    out: Mutex<Vec<Output>>,
    params: Mutex<Option<ParamSets>>,
}

impl Shared {
    fn push(&self, o: Output) {
        self.out.lock().unwrap_or_else(PoisonError::into_inner).push(o);
    }
}

/// A compression session with its callback state. Dropping it completes the pending frames and
/// invalidates the session before the callback state is freed.
struct Session {
    session: CFRetained<VTCompressionSession>,
    /// Pointed to by the session's callback: must outlive `session`.
    shared: Box<Shared>,
}

// SAFETY: a VTCompressionSession and the CoreMedia / CoreVideo objects it uses may be used from
// any thread (Apple: "thread safe"); the encoder owning a `Session` is used by one thread at a time
// (`&mut self`), and the callback state is behind `Mutex`es.
unsafe impl Send for Session {}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: the session is valid until released. Completing the frames lets any callback in
        // flight finish; invalidating guarantees none runs afterwards, so `shared` can be freed
        // when this function returns.
        unsafe {
            let _ = self.session.complete_frames(invalid_time());
            self.session.invalidate();
        }
    }
}

/// `kCMTimeInvalid`: "all pending frames" for `complete_frames`.
fn invalid_time() -> CMTime {
    CMTime { value: 0, timescale: 0, flags: CMTimeFlags(0), epoch: 0 }
}

fn cm_time(value: i64, timescale: u32) -> CMTime {
    CMTime { value, timescale: timescale.min(i32::MAX as u32) as i32, flags: CMTimeFlags::Valid, epoch: 0 }
}

/// `t` in units of `1 / timescale` s, rounded to nearest; None for an invalid or non-numeric time.
fn ticks(t: CMTime, timescale: i64) -> Option<i64> {
    let (value, scale, flags) = (t.value, t.timescale, t.flags);
    if !flags.contains(CMTimeFlags::Valid) || flags.contains(CMTimeFlags::PositiveInfinity) || flags.contains(CMTimeFlags::NegativeInfinity) || scale <= 0 {
        return None;
    }
    let (num, den) = (i128::from(value) * i128::from(timescale), i128::from(scale));
    i64::try_from((2 * num + den).div_euclid(2 * den)).ok()
}

/// Whether length-prefixed NAL units hold a random access point: an IDR slice (nal_unit_type 5) in
/// H.264; a BLA, IDR or CRA slice (types 16 to 21) in HEVC.
fn is_sync(data: &[u8], codec: VtCodec) -> Result<bool, String> {
    if matches!(codec, VtCodec::ProRes | VtCodec::Jpeg) {
        return Ok(true);
    }
    let mut pos = 0usize;
    let mut key = false;
    while pos < data.len() {
        let len_bytes = data.get(pos..pos + 4).ok_or("truncated NAL length")?;
        let len = u32::from_be_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]) as usize;
        pos += 4;
        let end = pos.checked_add(len).filter(|e| *e <= data.len()).ok_or("NAL unit longer than the frame")?;
        let header = *data.get(pos).ok_or("empty NAL unit")?;
        key |= match codec {
            VtCodec::H264 => header & 0x1f == 5,
            VtCodec::Hevc => (16..=21).contains(&((header >> 1) & 0x3f)),
            _ => false,
        };
        pos = end;
    }
    Ok(key)
}

/// The `hvcC` record VideoToolbox wrote among the sample description extension atoms of an HEVC
/// format description (the container's own codec configuration, exactly as QuickTime writes it).
fn hvcc_record(format: &CMFormatDescription) -> Option<Vec<u8>> {
    // SAFETY: `format` is valid; the key is an immutable framework constant; the result is a
    // retained property list (or none).
    let atoms = unsafe { format.extension(kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms) }?;
    let atoms = atoms.downcast::<CFDictionary>().ok()?;
    // SAFETY: CoreMedia documents this extension as a dictionary from four-character-code strings
    // to property lists; every CoreFoundation object is a `CFType`.
    let atoms: &CFDictionary<CFString, CFType> = unsafe { atoms.cast_unchecked() };
    let data = atoms.get(&CFString::from_str("hvcC"))?.downcast::<CFData>().ok()?;
    let bytes = data.to_vec();
    (!bytes.is_empty() && bytes.len() <= 65_535).then_some(bytes)
}

/// The parameter sets of the first compressed frame's format description: SPS / PPS for H.264,
/// VPS / SPS / PPS and the `hvcC` record for HEVC.
fn read_param_sets(sample: &CMSampleBuffer, codec: VtCodec) -> Result<ParamSets, String> {
    // SAFETY: `sample` is a valid sample buffer for the duration of the callback.
    let format = unsafe { sample.format_description() }.ok_or("compressed frame without a format description")?;
    let (mut vps, mut sps, mut pps) = (Vec::new(), Vec::new(), Vec::new());
    let (mut index, mut count) = (0usize, 1usize);
    while index < count && index < 16 {
        let (mut ptr, mut size, mut total, mut nal_len): (*const u8, usize, usize, c_int) = (std::ptr::null(), 0, 0, 0);
        // SAFETY: the out-pointers are valid for the call; the returned pointer is only read
        // while `format` (retained above) is alive.
        let status = unsafe {
            match codec {
                VtCodec::H264 => CMVideoFormatDescriptionGetH264ParameterSetAtIndex(&format, index, &mut ptr, &mut size, &mut total, &mut nal_len),
                VtCodec::Hevc => CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(&format, index, &mut ptr, &mut size, &mut total, &mut nal_len),
                _ => return Err("this codec has no NAL parameter sets".into()),
            }
        };
        if status != 0 {
            return Err(format!("cannot read the {codec:?} parameter sets ({status})"));
        }
        if nal_len != 4 {
            return Err(format!("unexpected NAL length size {nal_len}"));
        }
        count = total;
        if ptr.is_null() || size == 0 || size > 65_535 {
            return Err("empty or oversized parameter set".into());
        }
        // SAFETY: VideoToolbox returned `size` readable bytes at `ptr`, owned by `format`.
        let bytes = unsafe { std::slice::from_raw_parts(ptr, size) }.to_vec();
        match (codec, bytes.first()) {
            (VtCodec::H264, Some(b)) => match b & 0x1f {
                7 => sps.push(bytes),
                8 => pps.push(bytes),
                _ => {}
            },
            (VtCodec::Hevc, Some(b)) => match (b >> 1) & 0x3f {
                32 => vps.push(bytes),
                33 => sps.push(bytes),
                34 => pps.push(bytes),
                _ => {}
            },
            _ => {}
        }
        index += 1;
    }
    let complete = !sps.is_empty() && !pps.is_empty() && (codec == VtCodec::H264 || !vps.is_empty());
    if !complete {
        return Err("the format description lacks parameter sets".into());
    }
    let hvcc = if codec == VtCodec::Hevc { hvcc_record(&format) } else { None };
    Ok(ParamSets { vps, sps, pps, hvcc })
}

/// One compressed frame: its bytes, key flag and timestamps (and, once, the parameter sets).
fn read_packet(sample: &CMSampleBuffer, shared: &Shared) -> Result<VtPacket, String> {
    // SAFETY: `sample` is a valid sample buffer for the duration of the callback.
    let block = unsafe { sample.data_buffer() }.ok_or("compressed frame without data")?;
    // SAFETY: `block` is a valid block buffer (retained above).
    let len = unsafe { block.data_length() };
    if len == 0 || len > MAX_FRAME_BYTES {
        return Err(format!("compressed frame of {len} bytes"));
    }
    let mut data = vec![0u8; len];
    let dest = NonNull::new(data.as_mut_ptr().cast::<c_void>()).ok_or("empty frame buffer")?;
    // SAFETY: `dest` points to `len` writable bytes, the buffer's own length.
    let status = unsafe { block.copy_data_bytes(0, len, dest) };
    if status != 0 {
        return Err(format!("cannot copy a compressed frame ({status})"));
    }
    // SAFETY: valid sample buffer, see above.
    let (pts, dts) = unsafe { (sample.presentation_time_stamp(), sample.decode_time_stamp()) };
    let pts = ticks(pts, shared.timescale).ok_or("compressed frame without a presentation time")?;
    let dts = ticks(dts, shared.timescale).unwrap_or(pts);
    let key = is_sync(&data, shared.codec)?;
    {
        let mut params = shared.params.lock().unwrap_or_else(PoisonError::into_inner);
        if params.is_none() && matches!(shared.codec, VtCodec::H264 | VtCodec::Hevc) {
            *params = Some(read_param_sets(sample, shared.codec)?);
        }
    }
    Ok(VtPacket { data, key, pts, dts })
}

/// The output callback. Never unwinds: panics are caught and reported as a failed frame.
unsafe extern "C-unwind" fn output_callback(
    refcon: *mut c_void,
    _frame_refcon: *mut c_void,
    status: i32,
    flags: VTEncodeInfoFlags,
    sample: *mut CMSampleBuffer,
) {
    if refcon.is_null() {
        return;
    }
    // SAFETY: `refcon` is the `Shared` the session was created with; it lives in a `Box` owned by
    // the `Session`, which invalidates the session (no more callbacks) before freeing it.
    let shared = unsafe { &*(refcon as *const Shared) };
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        if status != 0 {
            return Output::Failed(format!("VideoToolbox encode error {status}"));
        }
        if flags.contains(VTEncodeInfoFlags::FrameDropped) {
            return Output::Failed("VideoToolbox dropped a frame".into());
        }
        let Some(sample) = NonNull::new(sample) else {
            return Output::Failed("VideoToolbox returned no data for a frame".into());
        };
        // SAFETY: VideoToolbox passes a valid sample buffer that stays alive for the duration of
        // the callback; we only borrow it here.
        let sample = unsafe { sample.as_ref() };
        match read_packet(sample, shared) {
            Ok(p) => Output::Packet(p),
            Err(e) => Output::Failed(e),
        }
    }));
    shared.push(result.unwrap_or_else(|_| Output::Failed("panic while reading a compressed frame".into())));
}

/// Unlocks a pixel buffer's base address when dropped.
struct Locked<'a>(&'a CVImageBuffer);

impl Drop for Locked<'_> {
    fn drop(&mut self) {
        // SAFETY: `fill` locked the buffer with the same flags, and it is still alive (borrowed).
        unsafe { CVPixelBufferUnlockBaseAddress(self.0, CVPixelBufferLockFlags(0)) };
    }
}

/// One writable plane of a locked pixel buffer.
fn plane_mut<'l>(pb: &CVImageBuffer, _lock: &'l mut Locked<'_>, i: usize) -> Result<(&'l mut [u8], usize, usize, usize), String> {
    let base = CVPixelBufferGetBaseAddressOfPlane(pb, i) as *mut u8;
    let stride = CVPixelBufferGetBytesPerRowOfPlane(pb, i);
    let (width, height) = (CVPixelBufferGetWidthOfPlane(pb, i), CVPixelBufferGetHeightOfPlane(pb, i));
    if base.is_null() || stride == 0 || height == 0 {
        return Err(format!("plane {i} of the input buffer is not mapped"));
    }
    let len = stride.checked_mul(height).ok_or("plane size overflows")?;
    // SAFETY: the buffer's base address is locked for writing while `_lock` lives and the returned
    // slice borrows it ('l); CoreVideo maps `bytes_per_row * height` bytes from each plane's base.
    let data = unsafe { std::slice::from_raw_parts_mut(base, len) };
    Ok((data, stride, width, height))
}

/// Both writable planes (luma and interleaved chroma) of a locked biplanar pixel buffer, verified disjoint.
fn biplanar_planes_mut<'l>(
    pb: &CVImageBuffer,
    _lock: &'l mut Locked<'_>,
) -> Result<((&'l mut [u8], usize, usize, usize), (&'l mut [u8], usize, usize, usize)), String> {
    let base0 = CVPixelBufferGetBaseAddressOfPlane(pb, 0) as *mut u8;
    let stride0 = CVPixelBufferGetBytesPerRowOfPlane(pb, 0);
    let (w0, h0) = (CVPixelBufferGetWidthOfPlane(pb, 0), CVPixelBufferGetHeightOfPlane(pb, 0));
    let base1 = CVPixelBufferGetBaseAddressOfPlane(pb, 1) as *mut u8;
    let stride1 = CVPixelBufferGetBytesPerRowOfPlane(pb, 1);
    let (w1, h1) = (CVPixelBufferGetWidthOfPlane(pb, 1), CVPixelBufferGetHeightOfPlane(pb, 1));
    if base0.is_null() || stride0 == 0 || h0 == 0 || base1.is_null() || stride1 == 0 || h1 == 0 {
        return Err("planes of the input buffer are not mapped".into());
    }
    let len0 = stride0.checked_mul(h0).ok_or("plane 0 size overflows")?;
    let len1 = stride1.checked_mul(h1).ok_or("plane 1 size overflows")?;
    let end0 = (base0 as usize).checked_add(len0).ok_or("plane 0 address overflows")?;
    let end1 = (base1 as usize).checked_add(len1).ok_or("plane 1 address overflows")?;
    if (base0 as usize) < end1 && (base1 as usize) < end0 {
        return Err("biplanar input buffer planes overlap".into());
    }
    // SAFETY: the buffer's base address is locked for writing while `_lock` lives and both returned
    // slices borrow it ('l); CoreVideo maps `bytes_per_row * height` bytes from each plane's base,
    // and we verified above that `[base0, end0)` and `[base1, end1)` are non-null and disjoint.
    let (p0, p1) = unsafe { (std::slice::from_raw_parts_mut(base0, len0), std::slice::from_raw_parts_mut(base1, len1)) };
    Ok(((p0, stride0, w0, h0), (p1, stride1, w1, h1)))
}

/// Copy planar Y / U / V (`w`×`h` luma, `⌈w/2⌉`×`⌈h/2⌉` chroma, tightly packed) into an NV12 buffer.
fn fill(pb: &CVImageBuffer, w: usize, h: usize, y: &[u8], u: &[u8], v: &[u8]) -> Result<(), String> {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    if y.len() < w * h || u.len() < cw * ch || v.len() < cw * ch {
        return Err("the Y'CbCr planes are smaller than the picture".into());
    }
    // SAFETY: `pb` is a valid pixel buffer (the caller holds it retained).
    if unsafe { CVPixelBufferLockBaseAddress(pb, CVPixelBufferLockFlags(0)) } != 0 {
        return Err("cannot lock the input buffer".into());
    }
    let mut lock = Locked(pb);
    let ((luma, ls, lw, lh), (chroma, cs, cpw, cph)) = biplanar_planes_mut(pb, &mut lock)?;
    if lw < w || lh < h || ls < w {
        return Err("the input buffer's luma plane is smaller than the picture".into());
    }
    if cpw < cw || cph < ch || cs < cw * 2 {
        return Err("the input buffer's chroma plane is smaller than the picture".into());
    }
    for (row, src) in luma.chunks_mut(ls).zip(y.chunks_exact(w)).take(h) {
        row[..w].copy_from_slice(src);
    }
    for ((row, us), vs) in chroma.chunks_mut(cs).zip(u.chunks_exact(cw)).zip(v.chunks_exact(cw)).take(ch) {
        for ((pair, &cb), &cr) in row.as_chunks_mut::<2>().0.iter_mut().zip(us).zip(vs) {
            pair[0] = cb;
            pair[1] = cr;
        }
    }
    drop(lock);
    Ok(())
}

/// Convert straight RGBA8 directly into a locked NV12 (`420v`) pixel buffer in a single parallel pass
/// (BT.709 limited-range 8-bit 4:2:0, 2×2 chroma average), avoiding intermediate planar Vec allocations.
fn fill_rgba_nv12(pb: &CVImageBuffer, w: usize, h: usize, rgba: &[u8]) -> Result<(), String> {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    if rgba.len() < w.saturating_mul(h).saturating_mul(4) {
        return Err("the RGBA picture is smaller than its size".into());
    }
    // SAFETY: `pb` is a valid pixel buffer (the caller holds it retained).
    if unsafe { CVPixelBufferLockBaseAddress(pb, CVPixelBufferLockFlags(0)) } != 0 {
        return Err("cannot lock the input buffer".into());
    }
    let mut lock = Locked(pb);
    let ((luma, ls, lw, lh), (chroma, cs, cpw, cph)) = biplanar_planes_mut(pb, &mut lock)?;
    if lw < w || lh < h || ls < w {
        return Err("the input buffer's luma plane is smaller than the picture".into());
    }
    if cpw < cw || cph < ch || cs < cw * 2 {
        return Err("the input buffer's chroma plane is smaller than the picture".into());
    }
    filmcraft_export::timed(filmcraft_export::Stage::Convert, || {
        luma[..ls * h].par_chunks_mut(ls * 2).zip(chroma[..cs * ch].par_chunks_mut(cs)).enumerate().for_each(|(cy, (yr, cr_row))| {
            let rows = yr.len() / ls;
            let mut us = vec![0f32; cw];
            let mut vs = vec![0f32; cw];
            let mut cnt = vec![0f32; cw];
            for dy in 0..rows {
                let row = cy * 2 + dy;
                let src = &rgba[row * w * 4..(row + 1) * w * 4];
                let y_out = &mut yr[dy * ls..dy * ls + w];
                for x in 0..w {
                    let (r, g, b) = (src[x * 4] as f32 / 255.0, src[x * 4 + 1] as f32 / 255.0, src[x * 4 + 2] as f32 / 255.0);
                    let yy = 0.2126 * r + 0.7152 * g + 0.0722 * b;
                    y_out[x] = (16.0 + 219.0 * yy).round().clamp(1.0, 254.0) as u8;
                    us[x / 2] += (b - yy) / 1.8556;
                    vs[x / 2] += (r - yy) / 1.5748;
                    cnt[x / 2] += 1.0;
                }
            }
            for (cx, pair) in cr_row.as_chunks_mut::<2>().0.iter_mut().take(cw).enumerate() {
                pair[0] = (128.0 + 224.0 * us[cx] / cnt[cx]).round().clamp(1.0, 254.0) as u8;
                pair[1] = (128.0 + 224.0 * vs[cx] / cnt[cx]).round().clamp(1.0, 254.0) as u8;
            }
        });
    });
    drop(lock);
    Ok(())
}

/// A hardware H.264 or HEVC encoder: feed pictures, collect compressed frames (in presentation
/// order, which is decoding order since nothing is reordered).
pub struct VtEncoder {
    session: Session,
    config: VtConfig,
    pool: Option<CFRetained<CVPixelBufferPool>>,
    attrs: CFRetained<CFDictionary<CFString, CFType>>,
    /// Pictures submitted so far.
    submitted: u64,
    options: VtOptions,
    pixel_format: u32,
    selected_hardware: Option<CFRetained<CFString>>,
}

// SAFETY: the session and its callback state are `Send` (see `Session`); the dictionary of source
// buffer attributes is never mutated after creation and CoreFoundation dictionaries and
// CoreVideo pixel buffer pools are thread-safe; the encoder is used by one thread at a time
// (`&mut self`), so moving it to another thread (the export job's) is sound.
unsafe impl Send for VtEncoder {}

fn set_bool(session: &VTCompressionSession, key: &CFString, on: bool) -> i32 {
    set_value(session, key, CFBoolean::new(on).as_ref())
}

unsafe extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

/// `kVTCompressionPropertyKey_ConstantBitRate`, or `None` on a macOS that does not have it.
///
/// The key exists from macOS 13. Naming it as a linked symbol makes dyld refuse to start the whole
/// app on macOS 11 and 12 ("Symbol not found"), although the encoder already falls back to an
/// average bitrate when the key is unavailable. So it is looked up at run time instead.
fn constant_bitrate_key() -> Option<&'static CFString> {
    // `RTLD_DEFAULT` on macOS: search every image loaded into the process, VideoToolbox included.
    const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;
    const NAME: &CStr = c"kVTCompressionPropertyKey_ConstantBitRate";
    // SAFETY: `NAME` is a valid NUL-terminated string; `dlsym` only reads it.
    let symbol = unsafe { dlsym(RTLD_DEFAULT, NAME.as_ptr()) };
    let slot = NonNull::new(symbol)?.cast::<*const CFString>();
    // SAFETY: the symbol is a `CFStringRef` constant exported by VideoToolbox, so `slot` points at a
    // pointer to an immutable CFString that lives as long as the framework stays loaded (the whole
    // process); it is read once and never written.
    unsafe { slot.read().as_ref() }
}

fn set_value(session: &VTCompressionSession, key: &CFString, value: &CFType) -> i32 {
    // SAFETY: the session is valid, `key` is a framework constant and `value` a live CF object.
    unsafe { VTSessionSetProperty(session.as_ref(), key, Some(value)) }
}

/// Fail with the property's name when VideoToolbox refuses it.
fn require(status: i32, what: &str) -> Result<(), String> {
    if status == 0 { Ok(()) } else { Err(format!("VideoToolbox refused {what} ({status})")) }
}

impl VtEncoder {
    /// Create the session, or say why hardware encoding is not used for this configuration.
    pub fn new(config: VtConfig) -> Result<VtEncoder, String> {
        Self::with_options(config, VtOptions::default())
    }

    /// Validate dimensions, profile, color and alpha options before creating and configuring the native encoder.
    pub fn with_options(config: VtConfig, options: VtOptions) -> Result<VtEncoder, String> {
        let (w, h) = (config.width, config.height);
        if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
            return Err(format!("picture size {w}x{h}"));
        }
        if !w.is_multiple_of(2) || !h.is_multiple_of(2) {
            // 4:2:0 video crops in units of two samples: an odd size would come out one sample smaller
            return Err(format!("odd picture size {w}x{h}"));
        }
        if config.timescale == 0 || config.frame_duration == 0 || config.timescale > i32::MAX as u32 {
            return Err("invalid frame rate".into());
        }
        if let VtProfile::ProRes(c) = config.profile
            && !matches!(&c, b"apco" | b"apcs" | b"apcn" | b"apch" | b"ap4h" | b"ap4x")
        {
            return Err("invalid ProRes profile".into());
        }
        if options.alpha && config.profile.codec() != VtCodec::Hevc && !matches!(config.profile, VtProfile::ProRes(c) if matches!(&c, b"ap4h" | b"ap4x")) {
            return Err("alpha requires a ProRes 4444 profile".into());
        }
        if options.alpha && config.profile.codec() == VtCodec::Hevc && config.profile != VtProfile::HevcMain {
            return Err("HEVC alpha requires Main 8-bit SDR".into());
        }
        if options.signal.is_hdr() && !config.profile.ten_bit() {
            return Err("HDR requires a 10-bit profile".into());
        }
        let pixel_format = if options.alpha && config.profile.codec() == VtCodec::Hevc {
            if config.profile.ten_bit() { u32::from_be_bytes(*b"y416") } else { u32::from_be_bytes(*b"BGRA") }
        } else {
            config.profile.pixel_format()
        };
        let codec = config.profile.codec();
        let shared = Box::new(Shared { codec, timescale: i64::from(config.timescale), out: Mutex::new(Vec::new()), params: Mutex::new(None) });

        // SAFETY: reading immutable framework constants.
        let require_key: &CFString = unsafe { kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder };
        let selected = if codec == VtCodec::Jpeg { Some(jpeg_hardware_encoder()?) } else { None };
        // SAFETY: immutable framework key.
        let encoder_key = unsafe { objc2_video_toolbox::kVTVideoEncoderSpecification_EncoderID };
        let spec = match &selected {
            Some(id) => CFDictionary::<CFString, CFType>::from_slices(&[require_key, encoder_key], &[CFBoolean::new(true).as_ref(), id.as_ref()]),
            None => CFDictionary::<CFString, CFType>::from_slices(&[require_key], &[CFBoolean::new(true).as_ref()]),
        };

        let (pf, cw, ch) = (CFNumber::new_i32(pixel_format as i32), CFNumber::new_i32(w as i32), CFNumber::new_i32(h as i32));
        let io = CFDictionary::<CFString, CFType>::from_slices(&[], &[]);
        // SAFETY: reading immutable framework constants.
        let (pf_key, w_key, h_key, io_key): (&CFString, &CFString, &CFString, &CFString) =
            unsafe { (kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey, kCVPixelBufferHeightKey, kCVPixelBufferIOSurfacePropertiesKey) };
        let attrs = CFDictionary::<CFString, CFType>::from_slices(&[pf_key, w_key, h_key, io_key], &[pf.as_ref(), cw.as_ref(), ch.as_ref(), io.as_ref()]);

        let mut out: *mut VTCompressionSession = std::ptr::null_mut();
        // SAFETY: every pointer is valid for the call (VideoToolbox copies what it keeps); the
        // callback's refcon points into `shared`, which the returned `Session` keeps alive until
        // the session is invalidated.
        let status = unsafe {
            VTCompressionSession::create(
                None,
                w as i32,
                h as i32,
                match codec {
                    VtCodec::H264 => kCMVideoCodecType_H264,
                    VtCodec::Hevc => {
                        if options.alpha {
                            u32::from_be_bytes(*b"muxa")
                        } else {
                            kCMVideoCodecType_HEVC
                        }
                    }
                    VtCodec::ProRes => match config.profile {
                        VtProfile::ProRes(c) => u32::from_be_bytes(c),
                        _ => return Err("invalid ProRes profile".into()),
                    },
                    VtCodec::Jpeg => u32::from_be_bytes(*b"jpeg"),
                },
                Some(spec.as_ref()),
                Some(attrs.as_ref()),
                None,
                Some(output_callback),
                &*shared as *const Shared as *mut c_void,
                NonNull::from(&mut out),
            )
        };
        let session = NonNull::new(out).filter(|_| status == 0).ok_or_else(|| format!("VTCompressionSessionCreate failed ({status})"))?;
        // SAFETY: a created session is returned retained (+1); `CFRetained` takes that reference over.
        let session = Session { session: unsafe { CFRetained::from_raw(session) }, shared };
        Self::configure(&session.session, &config, options)?;
        // SAFETY: the session is valid. Preparing allocates the encoder's resources now, so a
        // configuration the hardware cannot take fails here and not on the first frame.
        require(unsafe { session.session.prepare_to_encode_frames() }, "to prepare the encoder")?;
        // SAFETY: the session is valid; the pool (if any) is returned retained.
        let pool = unsafe { session.session.pixel_buffer_pool() };
        let encoder = VtEncoder { session, config, pool, attrs, submitted: 0, options, pixel_format, selected_hardware: selected };
        if !encoder.uses_hardware() {
            return Err("VideoToolbox did not select a hardware encoder".into());
        }
        Ok(encoder)
    }

    /// Apply codec-specific quality or profile/GOP/bitrate properties, then configure the requested color signal.
    fn configure(session: &VTCompressionSession, c: &VtConfig, options: VtOptions) -> Result<(), String> {
        if matches!(c.profile.codec(), VtCodec::ProRes | VtCodec::Jpeg) {
            // SAFETY: reading immutable framework constants. Intra codecs do not have H.26x
            // profile-level / GOP / bitrate properties.
            let quality = unsafe { objc2_video_toolbox::kVTCompressionPropertyKey_Quality };
            if c.profile.codec() == VtCodec::Jpeg {
                require(set_value(session, quality, CFNumber::new_f64(f64::from(options.quality.min(100)) / 100.0).as_ref()), "quality")?;
            } else {
                // ProRes quality is selected by its profile. Prefer quality over the HW
                // encoder's speed hint on systems which expose that optional property.
                let _ = set_bool(session, &CFString::from_static_str("PrioritizeEncodingSpeedOverQuality"), false);
            }
            return configure_color(session, options.signal);
        }
        if options.alpha {
            // SAFETY: immutable framework constants available since macOS 10.15.
            let (key, value) =
                unsafe { (objc2_video_toolbox::kVTCompressionPropertyKey_AlphaChannelMode, objc2_video_toolbox::kVTAlphaChannelMode_StraightAlpha) };
            require(set_value(session, key, value.as_ref()), "straight alpha")?;
        }
        let profile422 = CFString::from_static_str("HEVC_Main42210_AutoLevel");
        // SAFETY: reading immutable framework constants.
        let (profile_key, profile) = unsafe {
            (
                kVTCompressionPropertyKey_ProfileLevel,
                match c.profile {
                    VtProfile::Baseline => kVTProfileLevel_H264_Baseline_AutoLevel,
                    VtProfile::Main => kVTProfileLevel_H264_Main_AutoLevel,
                    VtProfile::High => kVTProfileLevel_H264_High_AutoLevel,
                    VtProfile::HevcMain => kVTProfileLevel_HEVC_Main_AutoLevel,
                    VtProfile::HevcMain10 => objc2_video_toolbox::kVTProfileLevel_HEVC_Main10_AutoLevel,
                    VtProfile::HevcMain42210 => &*profile422,
                    _ => return Err("invalid inter-frame profile".into()),
                },
            )
        };
        require(set_value(session, profile_key, profile.as_ref()), "the profile")?;
        // SAFETY: reading immutable framework constants.
        let (realtime, reorder, open_gop) =
            unsafe { (kVTCompressionPropertyKey_RealTime, kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AllowOpenGOP) };
        require(set_bool(session, realtime, false), "offline encoding")?;
        // No B-frames: on real 1080p footage the hardware encoder's quality is the same with and
        // without them (±0.3 dB at equal bitrate), the bitrate lands closer to the target, and
        // decode and presentation order coincide, so the muxer needs no composition offsets or edit list.
        require(set_bool(session, reorder, false), "frame reordering")?;
        // closed GOPs: every keyframe is an IDR picture, so every MP4 sync sample is a clean seek point
        let _ = set_bool(session, open_gop, false);

        let keyint = c.keyframe_interval.clamp(1, i32::MAX as u32) as i32;
        // SAFETY: reading an immutable framework constant.
        let keyint_key = unsafe { kVTCompressionPropertyKey_MaxKeyFrameInterval };
        require(set_value(session, keyint_key, CFNumber::new_i32(keyint).as_ref()), "the keyframe interval")?;
        // SAFETY: reading an immutable framework constant.
        let fps_key = unsafe { kVTCompressionPropertyKey_ExpectedFrameRate };
        let fps = f64::from(c.timescale) / f64::from(c.frame_duration);
        let _ = set_value(session, fps_key, CFNumber::new_f64(fps).as_ref());

        // SAFETY: reading immutable framework constants.
        let (avg_key, limits_key) = unsafe { (kVTCompressionPropertyKey_AverageBitRate, kVTCompressionPropertyKey_DataRateLimits) };
        let cbr_key = constant_bitrate_key();
        let bps = |kbps: u32| i32::try_from(u64::from(kbps).saturating_mul(1000)).unwrap_or(i32::MAX);
        match c.rate {
            VtRate::Cbr { kbps } => {
                // true constant bitrate where the OS offers it (macOS 13+); otherwise an average
                // with a one-second ceiling at the same rate
                if cbr_key.is_none_or(|key| set_value(session, key, CFNumber::new_i32(bps(kbps)).as_ref()) != 0) {
                    require(set_value(session, avg_key, CFNumber::new_i32(bps(kbps)).as_ref()), "the bitrate")?;
                    let limits = CFArray::<CFType>::from_objects(&[CFNumber::new_i32(bps(kbps) / 8).as_ref(), CFNumber::new_f64(1.0).as_ref()]);
                    let _ = set_value(session, limits_key, limits.as_ref());
                }
            }
            VtRate::Vbr { target_kbps, max_kbps } => {
                require(set_value(session, avg_key, CFNumber::new_i32(bps(target_kbps)).as_ref()), "the bitrate")?;
                let limits =
                    CFArray::<CFType>::from_objects(&[CFNumber::new_i32(bps(max_kbps.max(target_kbps)) / 8).as_ref(), CFNumber::new_f64(1.0).as_ref()]);
                let _ = set_value(session, limits_key, limits.as_ref());
            }
        }

        configure_color(session, options.signal)
    }

    /// Whether VideoToolbox says this session runs on a hardware encoder. The session is created
    /// with one required, so it should always hold; asking (and testing it) means a requirement
    /// dropped by accident could never turn into a silent software encode.
    pub fn uses_hardware(&self) -> bool {
        // SAFETY: reading an immutable framework constant.
        let key: &CFString = unsafe { kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder };
        let mut value: *mut CFType = std::ptr::null_mut();
        // SAFETY: the session is valid and `value` is a valid out-pointer for the CFTypeRef the
        // property is copied to (returned retained, +1); it stays null when the call fails.
        let status = unsafe { VTSessionCopyProperty(self.session.session.as_ref(), key, None, (&raw mut value).cast()) };
        let Some(value) = NonNull::new(value).filter(|_| status == 0) else {
            let Some(expected) = self.selected_hardware.as_ref() else { return false };
            let mut actual: *mut CFType = std::ptr::null_mut();
            // SAFETY: immutable framework key, live session and valid output pointer. JPEG's
            // component omits UsingHardware, so match the active component to its enumerated HW ID.
            let status = unsafe {
                VTSessionCopyProperty(self.session.session.as_ref(), objc2_video_toolbox::kVTCompressionPropertyKey_EncoderID, None, (&raw mut actual).cast())
            };
            let Some(actual) = NonNull::new(actual).filter(|_| status == 0) else { return false };
            // SAFETY: Copy transfers one retained reference.
            let actual = unsafe { CFRetained::from_raw(actual) };
            return actual.downcast_ref::<CFString>().is_some_and(|id| id == expected.as_ref());
        };
        // SAFETY: a copied property is returned retained (+1); `CFRetained` takes that reference over.
        let value = unsafe { CFRetained::from_raw(value) };
        value.downcast_ref::<CFBoolean>().is_some_and(CFBoolean::as_bool)
    }

    /// The configuration this encoder was created with.
    pub fn config(&self) -> &VtConfig {
        &self.config
    }

    /// The stream's parameter sets, known once the first compressed frame came out.
    pub fn parameter_sets(&self) -> Option<ParamSets> {
        self.session.shared.params.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn new_buffer(&self) -> Result<CFRetained<CVImageBuffer>, String> {
        let mut raw: *mut CVImageBuffer = std::ptr::null_mut();
        let status = match &self.pool {
            // SAFETY: the pool is valid and `raw` is a valid out-pointer.
            Some(pool) => unsafe { CVPixelBufferPool::create_pixel_buffer(None, pool, NonNull::from(&mut raw)) },
            // SAFETY: `attrs` is a valid dictionary of CF types and `raw` a valid out-pointer.
            None => unsafe {
                CVPixelBufferCreate(
                    None,
                    self.config.width as usize,
                    self.config.height as usize,
                    self.pixel_format,
                    Some(self.attrs.as_ref()),
                    NonNull::from(&mut raw),
                )
            },
        };
        let raw = NonNull::new(raw).filter(|_| status == 0).ok_or_else(|| format!("cannot allocate an input buffer ({status})"))?;
        // SAFETY: both creation functions return the buffer retained (+1).
        Ok(unsafe { CFRetained::from_raw(raw) })
    }

    /// Submit picture number `index` (planar BT.709 limited-range Y'CbCr, tightly packed) and return
    /// the compressed frames that came out meanwhile.
    pub fn encode(&mut self, index: u64, y: &[u8], u: &[u8], v: &[u8]) -> Result<Vec<VtPacket>, String> {
        if self.config.profile.ten_bit() {
            return Err("this profile requires 10-bit input".into());
        }
        let buffer = self.new_buffer()?;
        fill(&buffer, self.config.width as usize, self.config.height as usize, y, u, v)?;
        self.submit(index, &buffer)
    }

    /// Convert straight RGBA8 directly into an NV12 (`420v`) pixel buffer and submit picture `index`
    /// without intermediate planar Vec allocations.
    pub fn encode_rgba_nv12(&mut self, index: u64, rgba: &[u8]) -> Result<Vec<VtPacket>, String> {
        if self.config.profile.ten_bit() {
            return Err("this profile requires 10-bit input".into());
        }
        let buffer = self.new_buffer()?;
        fill_rgba_nv12(&buffer, self.config.width as usize, self.config.height as usize, rgba)?;
        self.submit(index, &buffer)
    }

    /// 10-bit planar input; 4:2:0 / 4:2:2 / 4:4:4 follows the selected profile. Alpha is
    /// full-range 10-bit. Conversion to CoreVideo's 16-bit storage preserves every input code.
    pub fn encode10(&mut self, index: u64, y: &[u16], u: &[u16], v: &[u16], alpha: Option<&[u16]>) -> Result<Vec<VtPacket>, String> {
        if !self.config.profile.ten_bit() {
            return Err("this profile requires 8-bit input".into());
        }
        let buffer = self.new_buffer()?;
        fill10(&buffer, &self.config, self.pixel_format, y, u, v, if self.options.alpha { alpha } else { None })?;
        self.submit(index, &buffer)
    }

    /// Convert straight RGBA8 or HDR encoded R'G'B' floats directly into a 10-bit CoreVideo pixel
    /// buffer (`x420`, `x422`, or `y416`) in a single parallel pass without intermediate planar Vecs.
    pub fn encode_frame10(&mut self, index: u64, rgba: &[u8], hdr: Option<&[f32]>, kr: f32, kb: f32) -> Result<Vec<VtPacket>, String> {
        if !self.config.profile.ten_bit() {
            return Err("this profile requires 8-bit input".into());
        }
        let buffer = self.new_buffer()?;
        fill_frame10(&buffer, &self.config, self.pixel_format, self.options.alpha, rgba, hdr, kr, kb)?;
        self.submit(index, &buffer)
    }

    /// Straight RGBA8 input for HEVC with alpha; the source buffer is BGRA as required by VT.
    pub fn encode_rgba(&mut self, index: u64, rgba: &[u8]) -> Result<Vec<VtPacket>, String> {
        if self.pixel_format != u32::from_be_bytes(*b"BGRA") {
            return Err("encoder does not take RGBA8".into());
        }
        let (w, h) = (self.config.width as usize, self.config.height as usize);
        if rgba.len() != w * h * 4 {
            return Err("wrong RGBA length".into());
        }
        let buffer = self.new_buffer()?;
        // SAFETY: the encoder owns the live buffer, and the guard unlocks it after the copy.
        if unsafe { CVPixelBufferLockBaseAddress(&buffer, CVPixelBufferLockFlags(0)) } != 0 {
            return Err("cannot lock RGBA buffer".into());
        }
        let lock = Locked(&buffer);
        let base = objc2_core_video::CVPixelBufferGetBaseAddress(&buffer).cast::<u8>();
        let stride = objc2_core_video::CVPixelBufferGetBytesPerRow(&buffer);
        let len = stride.checked_mul(h).ok_or("RGBA size overflow")?;
        if base.is_null() || stride < w * 4 {
            return Err("short RGBA buffer".into());
        }
        // SAFETY: CoreVideo maps stride * height writable bytes while locked, and the stride
        // is large enough for each source row. The slice is dropped before unlocking.
        let dest = unsafe { std::slice::from_raw_parts_mut(base, len) };
        filmcraft_export::timed(filmcraft_export::Stage::Convert, || {
            dest[..stride * h].par_chunks_mut(stride).zip(rgba.par_chunks_exact(w * 4)).for_each(|(dst, src)| {
                for (d, p) in dst.as_chunks_mut::<4>().0.iter_mut().zip(src.as_chunks::<4>().0.iter()) {
                    d.copy_from_slice(&[p[2], p[1], p[0], p[3]]);
                }
            });
        });
        drop(lock);
        self.submit(index, &buffer)
    }

    /// Submit one retained input picture with checked timing and collect completed callback packets.
    fn submit(&mut self, index: u64, buffer: &CVImageBuffer) -> Result<Vec<VtPacket>, String> {
        let value = i64::try_from(index).ok().and_then(|i| i.checked_mul(i64::from(self.config.frame_duration))).ok_or("frame index out of range")?;
        let (pts, duration) = (cm_time(value, self.config.timescale), cm_time(i64::from(self.config.frame_duration), self.config.timescale));
        // SAFETY: the session and buffer are valid (VideoToolbox retains the buffer as long as it
        // needs it); no frame properties, refcon or info-flags out-pointer.
        let status = unsafe { self.session.session.encode_frame(buffer, pts, duration, None, std::ptr::null_mut(), std::ptr::null_mut()) };
        if status != 0 {
            return Err(format!("VTCompressionSessionEncodeFrame failed ({status})"));
        }
        self.submitted += 1;
        if self.submitted == 1 {
            // the first compressed frame carries the parameter sets the container needs
            // SAFETY: the session is valid.
            require(unsafe { self.session.session.complete_frames(pts) }, "to finish the first frame")?;
        }
        self.drain()
    }

    /// Finish every pending frame and return them.
    pub fn flush(&mut self) -> Result<Vec<VtPacket>, String> {
        // SAFETY: the session is valid; an invalid time means "all pending frames".
        require(unsafe { self.session.session.complete_frames(invalid_time()) }, "to finish the pending frames")?;
        self.drain()
    }

    fn drain(&mut self) -> Result<Vec<VtPacket>, String> {
        let outputs = std::mem::take(&mut *self.session.shared.out.lock().unwrap_or_else(PoisonError::into_inner));
        let mut packets = Vec::with_capacity(outputs.len());
        for o in outputs {
            match o {
                Output::Packet(p) => packets.push(p),
                Output::Failed(why) => return Err(why),
            }
        }
        Ok(packets)
    }
}

/// H.273 colour codes shared by the input conversion, bitstream and container.
fn configure_color(session: &VTCompressionSession, signal: filmcraft_export::ColorSignal) -> Result<(), String> {
    let (primaries, transfer, matrix) = if signal == filmcraft_export::ColorSignal::PQ {
        ("ITU_R_2020", "SMPTE_ST_2084_PQ", "ITU_R_2020")
    } else if signal == filmcraft_export::ColorSignal::HLG {
        ("ITU_R_2020", "ITU_R_2100_HLG", "ITU_R_2020")
    } else if signal == filmcraft_export::ColorSignal::default() {
        ("ITU_R_709_2", "ITU_R_709_2", "ITU_R_709_2")
    } else {
        return Err("unsupported colour signal".into());
    };
    // SAFETY: reading immutable framework constants.
    let keys = unsafe { [kVTCompressionPropertyKey_ColorPrimaries, kVTCompressionPropertyKey_TransferFunction, kVTCompressionPropertyKey_YCbCrMatrix] };
    for (key, value) in keys.into_iter().zip([primaries, transfer, matrix]) {
        require(set_value(session, key, CFString::from_str(value).as_ref()), "colour signal")?;
    }
    if let Some((md, (cll, fall))) = signal.static_metadata() {
        let cll = [cll.to_be_bytes(), fall.to_be_bytes()].concat();
        // SAFETY: reading immutable framework constants, available since macOS 10.13.
        let (md_key, cll_key) = unsafe {
            (objc2_video_toolbox::kVTCompressionPropertyKey_MasteringDisplayColorVolume, objc2_video_toolbox::kVTCompressionPropertyKey_ContentLightLevelInfo)
        };
        require(set_value(session, md_key, CFData::from_bytes(&md.to_bytes()).as_ref()), "mastering display metadata")?;
        require(set_value(session, cll_key, CFData::from_bytes(&cll).as_ref()), "content light metadata")?;
    }
    Ok(())
}

/// Validate and pack 10-bit planes and optional alpha into P010, P210 or AYUV16 CoreVideo storage.
#[allow(clippy::too_many_arguments)]
fn fill10(pb: &CVImageBuffer, config: &VtConfig, pixel_format: u32, y: &[u16], u: &[u16], v: &[u16], alpha: Option<&[u16]>) -> Result<(), String> {
    let (w, h) = (config.width as usize, config.height as usize);
    let packed = pixel_format == u32::from_be_bytes(*b"y416");
    let sub_y = if !packed && config.profile == VtProfile::HevcMain10 { 2 } else { 1 };
    let (cw, ch) = (if packed { w } else { w.div_ceil(2) }, h.div_ceil(sub_y));
    if y.len() != w * h || u.len() != cw * ch || v.len() != cw * ch || alpha.is_some_and(|a| a.len() != w * h) {
        return Err("wrong 10-bit plane lengths".into());
    }
    if y.iter().chain(u).chain(v).chain(alpha.unwrap_or_default()).any(|&p| p > 1023) {
        return Err("input code exceeds 10 bits".into());
    }
    // SAFETY: the caller holds a valid retained pixel buffer.
    if unsafe { CVPixelBufferLockBaseAddress(pb, CVPixelBufferLockFlags(0)) } != 0 {
        return Err("cannot lock input buffer".into());
    }
    let mut lock = Locked(pb);
    if packed {
        let base = objc2_core_video::CVPixelBufferGetBaseAddress(pb).cast::<u8>();
        let stride = objc2_core_video::CVPixelBufferGetBytesPerRow(pb);
        let len = stride.checked_mul(h).ok_or("buffer size overflow")?;
        if base.is_null() || stride < w * 8 {
            return Err("short AYUV buffer".into());
        }
        // SAFETY: CoreVideo owns stride * height writable bytes while locked; dimensions were
        // capped in VtEncoder::new and the stride was checked above.
        let data = unsafe { std::slice::from_raw_parts_mut(base, len) };
        for (row, dest) in data.chunks_mut(stride).take(h).enumerate() {
            for (x, pixel) in dest.as_chunks_mut::<8>().0.iter_mut().take(w).enumerate() {
                let i = row * w + x;
                let a = alpha.map_or(65535, |a| ((u32::from(a[i]) * 65535 + 511) / 1023) as u16);
                for (dst, value) in pixel.as_chunks_mut::<2>().0.iter_mut().zip([a, y[i] << 6, u[i] << 6, v[i] << 6]) {
                    dst.copy_from_slice(&value.to_le_bytes());
                }
            }
        }
    } else {
        let (dest, stride, bw, bh) = plane_mut(pb, &mut lock, 0)?;
        if bw < w || bh < h || stride < w * 2 {
            return Err("short luma buffer".into());
        }
        for (dst, src) in dest.chunks_mut(stride).take(h).zip(y.chunks_exact(w)) {
            for (d, &p) in dst.as_chunks_mut::<2>().0.iter_mut().zip(src) {
                d.copy_from_slice(&(p << 6).to_le_bytes());
            }
        }
        let (dest, stride, bw, bh) = plane_mut(pb, &mut lock, 1)?;
        if bw < cw || bh < ch || stride < cw * 4 {
            return Err("short chroma buffer".into());
        }
        for ((dst, us), vs) in dest.chunks_mut(stride).take(ch).zip(u.chunks_exact(cw)).zip(v.chunks_exact(cw)) {
            for ((d, &cb), &cr) in dst.as_chunks_mut::<4>().0.iter_mut().zip(us).zip(vs) {
                d[..2].copy_from_slice(&(cb << 6).to_le_bytes());
                d[2..].copy_from_slice(&(cr << 6).to_le_bytes());
            }
        }
    }
    Ok(())
}

/// Convert RGBA8 or HDR encoded R'G'B' floats directly into a locked 10-bit CoreVideo pixel buffer
/// (`y416`, `x420`, or `x422`) in a single parallel pass without intermediate planar Vecs.
#[allow(clippy::too_many_arguments)]
fn fill_frame10(
    pb: &CVImageBuffer,
    config: &VtConfig,
    pixel_format: u32,
    keep_alpha: bool,
    rgba: &[u8],
    hdr: Option<&[f32]>,
    kr: f32,
    kb: f32,
) -> Result<(), String> {
    let (w, h) = (config.width as usize, config.height as usize);
    let packed = pixel_format == u32::from_be_bytes(*b"y416");
    let is420 = !packed && config.profile == VtProfile::HevcMain10;
    let (cw, ch) = (if packed { w } else { w.div_ceil(2) }, if is420 { h.div_ceil(2) } else { h });
    if (hdr.is_none() || keep_alpha) && rgba.len() != w.saturating_mul(h).saturating_mul(4) {
        return Err("the RGBA picture is smaller than its size".into());
    }
    if hdr.is_some_and(|rgb| rgb.len() != w.saturating_mul(h).saturating_mul(3)) {
        return Err("wrong HDR picture length".into());
    }
    // SAFETY: the caller holds a valid retained pixel buffer.
    if unsafe { CVPixelBufferLockBaseAddress(pb, CVPixelBufferLockFlags(0)) } != 0 {
        return Err("cannot lock input buffer".into());
    }
    let mut lock = Locked(pb);
    if packed {
        let base = objc2_core_video::CVPixelBufferGetBaseAddress(pb).cast::<u8>();
        let stride = objc2_core_video::CVPixelBufferGetBytesPerRow(pb);
        let len = stride.checked_mul(h).ok_or("buffer size overflow")?;
        if base.is_null() || stride < w * 8 {
            return Err("short AYUV buffer".into());
        }
        // SAFETY: CoreVideo owns stride * height writable bytes while locked; dimensions were
        // capped in VtEncoder::new and the stride was checked above.
        let data = unsafe { std::slice::from_raw_parts_mut(base, len) };
        let (kr, kb) = if hdr.is_some() { (kr, kb) } else { (0.2126, 0.0722) };
        let kg = 1.0 - kr - kb;
        let (sb, sr) = (2.0 * (1.0 - kb), 2.0 * (1.0 - kr));
        filmcraft_export::timed(filmcraft_export::Stage::Convert, || {
            data[..stride * h].par_chunks_mut(stride).enumerate().for_each(|(row, dest)| {
                for (x, pixel) in dest.as_chunks_mut::<8>().0.iter_mut().take(w).enumerate() {
                    let i = row * w + x;
                    let [r, g, b] = match hdr {
                        Some(f) => f.get(i * 3..i * 3 + 3).map_or([0.0; 3], |p| [p[0], p[1], p[2]]),
                        None => rgba.get(i * 4..i * 4 + 3).map_or([0.0; 3], |p| [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0]),
                    };
                    let yy = kr * r + kg * g + kb * b;
                    let yv = (64.0 + 876.0 * yy).round().clamp(4.0, 1019.0) as u16;
                    let cbv = (512.0 + 896.0 * (b - yy) / sb).round().clamp(4.0, 1019.0) as u16;
                    let crv = (512.0 + 896.0 * (r - yy) / sr).round().clamp(4.0, 1019.0) as u16;
                    let a = if keep_alpha {
                        let a10 = rgba.get(i * 4 + 3).map_or(1023, |&v| ((u32::from(v) * 1023 + 127) / 255) as u16);
                        ((u32::from(a10) * 65535 + 511) / 1023) as u16
                    } else {
                        65535
                    };
                    for (dst, value) in pixel.as_chunks_mut::<2>().0.iter_mut().zip([a, yv << 6, cbv << 6, crv << 6]) {
                        dst.copy_from_slice(&value.to_le_bytes());
                    }
                }
            });
        });
    } else if is420 {
        let ((luma, ls, lw, lh), (chroma, cs, cpw, cph)) = biplanar_planes_mut(pb, &mut lock)?;
        if lw < w || lh < h || ls < w * 2 {
            return Err("short luma buffer".into());
        }
        if cpw < cw || cph < ch || cs < cw * 4 {
            return Err("short chroma buffer".into());
        }
        let kg = 1.0 - kr - kb;
        let (sb, sr) = (2.0 * (1.0 - kb), 2.0 * (1.0 - kr));
        let unit = |x: f32| if x.is_nan() { 0.0 } else { x.clamp(0.0, 1.0) };
        let code = |x: f32| x.round().clamp(4.0, 1019.0) as u16;
        filmcraft_export::timed(filmcraft_export::Stage::Convert, || {
            luma[..ls * h].par_chunks_mut(ls * 2).zip(chroma[..cs * ch].par_chunks_mut(cs)).enumerate().for_each(|(cy, (yr, cr_row))| {
                let rows = yr.len() / ls;
                let mut us = vec![0f32; cw];
                let mut vs = vec![0f32; cw];
                let mut cnt = vec![0f32; cw];
                for dy in 0..rows {
                    let row = cy * 2 + dy;
                    let y_out = yr[dy * ls..dy * ls + w * 2].as_chunks_mut::<2>().0;
                    for (x, yo) in y_out.iter_mut().enumerate() {
                        let i = row * w + x;
                        let (r, g, b) = match hdr {
                            Some(rgb) => (
                                unit(rgb.get(i * 3).copied().unwrap_or(0.0)),
                                unit(rgb.get(i * 3 + 1).copied().unwrap_or(0.0)),
                                unit(rgb.get(i * 3 + 2).copied().unwrap_or(0.0)),
                            ),
                            None => (
                                unit(f32::from(rgba.get(i * 4).copied().unwrap_or(0)) / 255.0),
                                unit(f32::from(rgba.get(i * 4 + 1).copied().unwrap_or(0)) / 255.0),
                                unit(f32::from(rgba.get(i * 4 + 2).copied().unwrap_or(0)) / 255.0),
                            ),
                        };
                        let yy = kr * r + kg * g + kb * b;
                        let yv = code(64.0 + 876.0 * yy);
                        yo.copy_from_slice(&(yv << 6).to_le_bytes());
                        if let (Some(a), Some(c), Some(n)) = (us.get_mut(x / 2), vs.get_mut(x / 2), cnt.get_mut(x / 2)) {
                            *a += (b - yy) / sb;
                            *c += (r - yy) / sr;
                            *n += 1.0;
                        }
                    }
                }
                for (cx, pair) in cr_row.as_chunks_mut::<4>().0.iter_mut().take(cw).enumerate() {
                    let n = cnt.get(cx).copied().unwrap_or(1.0).max(1.0);
                    let cb = code(512.0 + 896.0 * us.get(cx).copied().unwrap_or(0.0) / n);
                    let cr = code(512.0 + 896.0 * vs.get(cx).copied().unwrap_or(0.0) / n);
                    pair[..2].copy_from_slice(&(cb << 6).to_le_bytes());
                    pair[2..].copy_from_slice(&(cr << 6).to_le_bytes());
                }
            });
        });
    } else {
        let ((luma, ls, lw, lh), (chroma, cs, cpw, cph)) = biplanar_planes_mut(pb, &mut lock)?;
        if lw < w || lh < h || ls < w * 2 {
            return Err("short luma buffer".into());
        }
        if cpw < cw || cph < ch || cs < cw * 4 {
            return Err("short chroma buffer".into());
        }
        let (kr, kg, kb, sb, sr) = if hdr.is_some() {
            let kg = 1.0 - kr - kb;
            (kr, kg, kb, 2.0 * (1.0 - kb), 2.0 * (1.0 - kr))
        } else {
            (0.2126f32, 0.7152f32, 0.0722f32, 1.8556f32, 1.5748f32)
        };
        filmcraft_export::timed(filmcraft_export::Stage::Convert, || {
            luma[..ls * h].par_chunks_mut(ls).zip(chroma[..cs * h].par_chunks_mut(cs)).enumerate().for_each(|(row, (yr, cr_row))| {
                let mut us = vec![0f32; w];
                let mut vs = vec![0f32; w];
                let y_out = yr[..w * 2].as_chunks_mut::<2>().0;
                for (x, yo) in y_out.iter_mut().enumerate() {
                    let i = row * w + x;
                    let (r, g, b) = match hdr {
                        Some(rgb) => (rgb[i * 3], rgb[i * 3 + 1], rgb[i * 3 + 2]),
                        None => (rgba[i * 4] as f32 / 255.0, rgba[i * 4 + 1] as f32 / 255.0, rgba[i * 4 + 2] as f32 / 255.0),
                    };
                    let yy = kr * r + kg * g + kb * b;
                    let yv = (64.0 + 876.0 * yy).round().clamp(4.0, 1019.0) as u16;
                    yo.copy_from_slice(&(yv << 6).to_le_bytes());
                    us[x] = (b - yy) / sb;
                    vs[x] = (r - yy) / sr;
                }
                for (cx, pair) in cr_row.as_chunks_mut::<4>().0.iter_mut().take(cw).enumerate() {
                    let a = cx * 2;
                    let b2 = (a + 1).min(w - 1);
                    let cb = (512.0 + 896.0 * (us[a] + us[b2]) * 0.5).round().clamp(4.0, 1019.0) as u16;
                    let cr = (512.0 + 896.0 * (vs[a] + vs[b2]) * 0.5).round().clamp(4.0, 1019.0) as u16;
                    pair[..2].copy_from_slice(&(cb << 6).to_le_bytes());
                    pair[2..].copy_from_slice(&(cr << 6).to_le_bytes());
                }
            });
        });
    }
    Ok(())
}

/// JPEG's default codec is software even with RequireHardware set on some systems. Select an
/// enumerated hardware implementation explicitly instead of relying on the default component.
fn jpeg_hardware_encoder() -> Result<CFRetained<CFString>, String> {
    let mut list: *const CFArray = std::ptr::null();
    // SAFETY: valid output pointer; the copied array is returned retained.
    let status = unsafe { objc2_video_toolbox::VTCopyVideoEncoderList(None, NonNull::from(&mut list)) };
    let list = NonNull::new(list.cast_mut()).filter(|_| status == 0).ok_or("cannot enumerate hardware JPEG encoders")?;
    // SAFETY: the Copy rule transfers one retained reference.
    let list = unsafe { CFRetained::from_raw(list) };
    // SAFETY: the documented array holds CoreFoundation dictionaries, all of them CFTypes.
    let values: &CFArray<CFType> = unsafe { list.cast_unchecked() };
    for value in values.iter() {
        let Some(dict) = value.downcast_ref::<CFDictionary>() else { continue };
        // SAFETY: VideoToolbox documents a CFString-keyed property list; values are CFTypes.
        let dict: &CFDictionary<CFString, CFType> = unsafe { dict.cast_unchecked() };
        let codec = dict.get(&CFString::from_static_str("CodecType")).and_then(|v| v.downcast::<CFNumber>().ok()).and_then(|v| v.as_i64());
        let hardware =
            dict.get(&CFString::from_static_str("IsHardwareAccelerated")).is_some_and(|v| v.downcast_ref::<CFBoolean>().is_some_and(CFBoolean::as_bool));
        if hardware && codec == Some(i64::from(u32::from_be_bytes(*b"jpeg"))) {
            return dict
                .get(&CFString::from_static_str("EncoderID"))
                .and_then(|v| v.downcast::<CFString>().ok())
                .ok_or_else(|| "hardware JPEG encoder has no ID".into());
        }
    }
    Err("no hardware JPEG encoder".into())
}
