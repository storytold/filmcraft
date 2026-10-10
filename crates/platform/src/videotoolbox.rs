//! VideoToolbox (macOS) hardware H.264 / HEVC decoding.
//!
//! The only module of the workspace with `unsafe` code besides the other FFI modules of this crate
//! (docs/adr/0001-platform-ffi.md). Rules: every `unsafe` block has a `// SAFETY:` comment; no
//! panic may unwind into VideoToolbox (the output callback runs under `catch_unwind`); every
//! failure is a `Result` the caller ([`crate::hybrid::HybridDecoder`]) answers by switching to the
//! software decoder.
//!
//! A session is created from the parameter sets of the sample entry (`avcC` / `hvcC`) with
//! hardware decoding required, so streams the hardware cannot take are declined up front. Samples
//! are fed as `CMSampleBuffer`s with asynchronous decompression (a couple in flight); the output
//! callback (on a VideoToolbox thread) retains eligible decoded `CVPixelBuffer`s as
//! [`PixelData::Native`] for Metal import and lazy CPU access, or copies unsupported layouts
//! into planar [`PixelData::Yuv8`] / [`PixelData::Yuv16`]. VideoToolbox emits pictures in decoding order, so a reorder buffer of the
//! stream's own depth (`max_num_reorder_frames` / `sps_max_num_reorder_pics`) puts them in
//! presentation order, as the software decoders do.

use std::ffi::c_void;
use std::panic::AssertUnwindSafe;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, PoisonError};

use filmcraft_codecs::hw::{NalCodec, NalStreamInfo};
use filmcraft_codecs::{CodecError, DecodedFrame, Result, VideoDecoder};
use filmcraft_frame::{Chroma, PixelData, VideoFrame, pool};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_media::{
    CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMSampleTimingInfo, CMTime, CMTimeFlags, CMVideoFormatDescriptionCreateFromH264ParameterSets,
    CMVideoFormatDescriptionCreateFromHEVCParameterSets, kCMBlockBufferAssureMemoryNowFlag,
};
use objc2_core_video::{
    CVImageBuffer, CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetHeight, CVPixelBufferGetHeightOfPlane,
    CVPixelBufferGetPixelFormatType, CVPixelBufferGetPlaneCount, CVPixelBufferGetWidth, CVPixelBufferGetWidthOfPlane, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress, kCVPixelBufferPixelFormatTypeKey,
};
use objc2_video_toolbox::{
    VTDecodeFrameFlags, VTDecodeInfoFlags, VTDecompressionOutputCallbackRecord, VTDecompressionSession,
    kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder,
};

/// `CVPixelBuffer` formats we ask VideoToolbox for: biplanar Y + interleaved CbCr, 8-bit or
/// 10-bit in the high bits of 16-bit words.
const NV12_VIDEO: u32 = u32::from_be_bytes(*b"420v");
const NV12_FULL: u32 = u32::from_be_bytes(*b"420f");
const P010_VIDEO: u32 = u32::from_be_bytes(*b"x420");
const P010_FULL: u32 = u32::from_be_bytes(*b"xf20");
const NV16_VIDEO: u32 = u32::from_be_bytes(*b"422v");
const NV16_FULL: u32 = u32::from_be_bytes(*b"422f");
const P210_VIDEO: u32 = u32::from_be_bytes(*b"x422");
const P210_FULL: u32 = u32::from_be_bytes(*b"xf22");

/// How decoded buffers are laid out and what each output picture carries.
#[derive(Clone, Debug)]
struct Layout {
    pixel_format: u32,
    /// Coded size and output rectangle (luma samples).
    coded: (u32, u32),
    crop: (u32, u32, u32, u32),
    chroma: Chroma,
    /// Significant bits per sample (8 = `Yuv8`).
    bits: u32,
    color: filmcraft_color::ColorInfo,
    par: (u32, u32),
}

/// One output-callback result.
enum Output {
    Frame(DecodedFrame),
    Failed(String),
}

/// State shared with the output callback (its `decompressionOutputRefCon`). Boxed by the decoder
/// and kept alive until the session is invalidated.
struct Shared {
    layout: Layout,
    out: Mutex<Vec<Output>>,
}

impl Shared {
    fn push(&self, o: Output) {
        self.out.lock().unwrap_or_else(PoisonError::into_inner).push(o);
    }
}

/// A VideoToolbox session with its callback state. Dropping it invalidates the session before the
/// callback state is freed.
struct Session {
    session: CFRetained<VTDecompressionSession>,
    format: CFRetained<CMFormatDescription>,
    /// Pointed to by the session's callback record: must outlive `session`.
    shared: Box<Shared>,
}

// SAFETY: a VTDecompressionSession and the CoreMedia objects it uses may be used from any thread
// (Apple: "thread safe"); the decoder owning a `Session` is used by one thread at a time (`&mut
// self`), and the callback state is behind a `Mutex`.
unsafe impl Send for Session {}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: the session is valid until released; invalidating it guarantees no callback runs
        // afterwards, so `shared` can be freed when this function returns.
        unsafe {
            self.session.wait_for_asynchronous_frames();
            self.session.invalidate();
        }
    }
}

/// The output callback. Never unwinds: panics are caught and reported as a failed frame.
unsafe extern "C-unwind" fn output_callback(
    refcon: *mut c_void,
    frame_refcon: *mut c_void,
    status: i32,
    _flags: VTDecodeInfoFlags,
    image: *mut CVImageBuffer,
    _pts: CMTime,
    _duration: CMTime,
) {
    if refcon.is_null() {
        return;
    }
    // SAFETY: `refcon` is the `Shared` the session was created with; it lives in a `Box` owned by
    // the `Session`, which invalidates the session (no more callbacks) before freeing it.
    let shared = unsafe { &*(refcon as *const Shared) };
    let pts = frame_refcon as isize as i64;
    let r = std::panic::catch_unwind(AssertUnwindSafe(|| {
        if status != 0 {
            return Output::Failed(format!("VideoToolbox decode error {status}"));
        }
        let Some(image) = NonNull::new(image) else {
            return Output::Failed("VideoToolbox dropped a picture".into());
        };
        // SAFETY: VideoToolbox passes a valid image buffer that stays alive for the duration of
        // the callback; we only borrow it here.
        let pb = unsafe { image.as_ref() };
        let l = &shared.layout;
        let frame = if CVPixelBufferGetPixelFormatType(pb) == l.pixel_format {
            crate::gpu_decode::frame(pb, l.crop.2, l.crop.3, l.chroma, l.bits).map(|mut frame| {
                frame.color = l.color;
                frame.par = l.par;
                frame
            })
        } else {
            None
        };
        match frame.map(Ok).unwrap_or_else(|| copy_out(pb, l)) {
            Ok(frame) => Output::Frame(DecodedFrame { pts, frame, draft: false }),
            Err(e) => Output::Failed(e),
        }
    }));
    shared.push(r.unwrap_or_else(|_| Output::Failed("panic while reading a decoded picture".into())));
}

/// Unlocks a pixel buffer's base address when dropped.
struct Locked<'a>(&'a CVImageBuffer);

impl Drop for Locked<'_> {
    fn drop(&mut self) {
        // SAFETY: `copy_out` locked the buffer with the same flags, and it is still alive (borrowed).
        unsafe { CVPixelBufferUnlockBaseAddress(self.0, CVPixelBufferLockFlags::ReadOnly) };
    }
}

/// One plane of a locked pixel buffer.
struct PlaneView<'a> {
    data: &'a [u8],
    stride: usize,
    width: usize,
    height: usize,
}

impl<'a> PlaneView<'a> {
    /// `len` bytes of row `y` from byte `x`.
    fn row(&self, y: usize, x: usize, len: usize) -> std::result::Result<&'a [u8], String> {
        let start = y * self.stride + x;
        self.data.get(start..start + len).ok_or_else(|| "plane row out of range".to_string())
    }
}

/// The returned view borrows the lock guard, so it cannot outlive the locked base address.
fn plane<'l>(pb: &CVImageBuffer, _lock: &'l Locked<'_>, i: usize) -> std::result::Result<PlaneView<'l>, String> {
    let base = CVPixelBufferGetBaseAddressOfPlane(pb, i) as *const u8;
    let stride = CVPixelBufferGetBytesPerRowOfPlane(pb, i);
    let (width, height) = (CVPixelBufferGetWidthOfPlane(pb, i), CVPixelBufferGetHeightOfPlane(pb, i));
    if base.is_null() || stride == 0 || height == 0 {
        return Err(format!("decoded picture plane {i} is not mapped"));
    }
    let len = stride.checked_mul(height).ok_or("plane size overflows")?;
    // SAFETY: the buffer's base address is locked while `_lock` lives, and the returned slice
    // borrows `_lock` ('l), so it cannot be used after the unlock; CoreVideo maps
    // `bytes_per_row * height` bytes from each plane's base address.
    let data = unsafe { std::slice::from_raw_parts(base, len) };
    Ok(PlaneView { data, stride, width, height })
}

/// Copy a decoded biplanar buffer into a planar frame (deinterleaving chroma, cropping to the
/// output rectangle, shifting 10-bit samples down from the high bits).
fn copy_out(pb: &CVImageBuffer, l: &Layout) -> std::result::Result<VideoFrame, String> {
    let fmt = CVPixelBufferGetPixelFormatType(pb);
    if fmt != l.pixel_format || CVPixelBufferGetPlaneCount(pb) != 2 {
        return Err(format!("unexpected decoded pixel format {fmt:#x}"));
    }
    let (bw, bh) = (CVPixelBufferGetWidth(pb) as u32, CVPixelBufferGetHeight(pb) as u32);
    let (cx, cy, w, h) = l.crop;
    // VideoToolbox outputs the cropped picture; a buffer of the coded size is cropped here.
    let (ox, oy) = if (bw, bh) == (w, h) {
        (0, 0)
    } else if (bw, bh) == l.coded && cx + w <= bw && cy + h <= bh {
        (cx, cy)
    } else {
        return Err(format!("decoded picture is {bw}x{bh}, expected {w}x{h}"));
    };
    // SAFETY: `pb` is a valid pixel buffer for the duration of this call (see the callback).
    if unsafe { CVPixelBufferLockBaseAddress(pb, CVPixelBufferLockFlags::ReadOnly) } != 0 {
        return Err("cannot lock the decoded picture".into());
    }
    let lock = Locked(pb);
    let (ys, cs) = (plane(pb, &lock, 0)?, plane(pb, &lock, 1)?);
    let (w, h, ox, oy) = (w as usize, h as usize, ox as usize, oy as usize);
    let sub_y = if l.chroma == Chroma::C420 { 2 } else { 1 };
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(sub_y));
    let (cox, coy) = (ox / 2, oy / sub_y);
    let bps = if l.bits > 8 { 2 } else { 1 };
    if ys.width < ox + w || ys.height < oy + h || ys.stride < ys.width * bps || cs.width < cox + cw || cs.height < coy + ch || cs.stride < cs.width * 2 * bps {
        return Err("decoded picture planes are smaller than the picture".into());
    }
    let data = if bps == 1 {
        let mut yp = pool::take_u8(w * h);
        for y in 0..h {
            yp.extend_from_slice(ys.row(oy + y, ox * bps, w * bps)?);
        }
        let (mut u, mut v) = (pool::take_u8(cw * ch), pool::take_u8(cw * ch));
        for y in 0..ch {
            let pairs = cs.row(coy + y, cox * 2 * bps, cw * 2 * bps)?.as_chunks::<2>().0;
            crate::chroma::append_u8(pairs, &mut u, &mut v);
        }
        PixelData::Yuv8 { planes: [Arc::new(yp), Arc::new(u), Arc::new(v)], chroma: l.chroma, alpha: None }
    } else {
        let shift = 16 - l.bits;
        let mut yp = pool::take_u16(w * h);
        for y in 0..h {
            yp.extend(ys.row(oy + y, ox * bps, w * bps)?.as_chunks::<2>().0.iter().map(|b| u16::from_ne_bytes([b[0], b[1]]) >> shift));
        }
        let (mut u, mut v) = (pool::take_u16(cw * ch), pool::take_u16(cw * ch));
        for y in 0..ch {
            let quads = cs.row(coy + y, cox * 2 * bps, cw * 2 * bps)?.as_chunks::<4>().0;
            crate::chroma::append_u10(quads, &mut u, &mut v);
        }
        PixelData::Yuv16 { planes: [Arc::new(yp), Arc::new(u), Arc::new(v)], chroma: l.chroma, bits: l.bits, alpha: None }
    };
    drop(lock);
    Ok(VideoFrame { width: w as u32, height: h as u32, data, color: l.color, par: l.par, pts: filmcraft_time::Tick::ZERO })
}

#[cfg(test)]
pub(crate) fn legacy_frame(pb: &CVImageBuffer, w: u32, h: u32, bits: u32) -> std::result::Result<VideoFrame, String> {
    copy_out(
        pb,
        &Layout {
            pixel_format: CVPixelBufferGetPixelFormatType(pb),
            coded: (w, h),
            crop: (0, 0, w, h),
            chroma: Chroma::C420,
            bits,
            color: filmcraft_color::ColorInfo::REC709,
            par: (1, 1),
        },
    )
}

/// The layout for a stream, or why VideoToolbox is not used for it.
fn layout(info: &NalStreamInfo) -> std::result::Result<Layout, String> {
    if info.interlaced {
        return Err("field-coded H.264".into());
    }
    if info.bit_depth_luma != info.bit_depth_chroma || !matches!(info.bit_depth_luma, 8 | 10) {
        return Err(format!("{}-bit luma / {}-bit chroma", info.bit_depth_luma, info.bit_depth_chroma));
    }
    let full = info.color.range == filmcraft_color::Range::Full;
    let ten = info.bit_depth_luma == 10;
    let (chroma, pixel_format) = match (info.chroma_format_idc, ten, full) {
        (1, false, false) => (Chroma::C420, NV12_VIDEO),
        (1, false, true) => (Chroma::C420, NV12_FULL),
        (1, true, false) => (Chroma::C420, P010_VIDEO),
        (1, true, true) => (Chroma::C420, P010_FULL),
        (2, false, false) => (Chroma::C422, NV16_VIDEO),
        (2, false, true) => (Chroma::C422, NV16_FULL),
        (2, true, false) => (Chroma::C422, P210_VIDEO),
        (2, true, true) => (Chroma::C422, P210_FULL),
        (c, ..) => return Err(format!("chroma_format_idc {c}")),
    };
    let (cx, cy, w, h) = info.crop;
    if w == 0 || h == 0 || w > 8192 || h > 8192 || cx.saturating_add(w) > info.coded.0 || cy.saturating_add(h) > info.coded.1 {
        return Err(format!("picture size {w}x{h}"));
    }
    Ok(Layout { pixel_format, coded: info.coded, crop: info.crop, chroma, bits: info.bit_depth_luma, color: info.color, par: info.par })
}

impl Session {
    fn new(info: &NalStreamInfo, layout: Layout) -> std::result::Result<Session, String> {
        let format = format_description(info)?;
        // SAFETY: reading an immutable framework constant.
        let key: &CFString = unsafe { kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder };
        let spec = CFDictionary::<CFString, CFType>::from_slices(&[key], &[CFBoolean::new(true).as_ref()]);
        let pf = CFNumber::new_i32(layout.pixel_format as i32);
        // SAFETY: reading an immutable framework constant.
        let pf_key: &CFString = unsafe { kCVPixelBufferPixelFormatTypeKey };
        let io = CFDictionary::<CFString, CFType>::from_slices(&[], &[]);
        // SAFETY: immutable CoreVideo constants.
        let (io_key, metal_key) = unsafe { (objc2_core_video::kCVPixelBufferIOSurfacePropertiesKey, objc2_core_video::kCVPixelBufferMetalCompatibilityKey) };
        let yes = CFBoolean::new(true);
        let attrs = CFDictionary::<CFString, CFType>::from_slices(&[pf_key, io_key, metal_key], &[pf.as_ref(), io.as_ref(), yes.as_ref()]);
        let shared = Box::new(Shared { layout, out: Mutex::new(Vec::new()) });
        let record = VTDecompressionOutputCallbackRecord {
            decompressionOutputCallback: Some(output_callback),
            decompressionOutputRefCon: &*shared as *const Shared as *mut c_void,
        };
        let mut out: *mut VTDecompressionSession = std::ptr::null_mut();
        // SAFETY: every pointer is valid for the call (VideoToolbox copies the callback record);
        // the refcon points into `shared`, which the returned `Session` keeps alive until the
        // session is invalidated.
        let status = unsafe { VTDecompressionSession::create(None, &format, Some(spec.as_ref()), Some(attrs.as_ref()), &record, NonNull::from(&mut out)) };
        let session = NonNull::new(out).filter(|_| status == 0).ok_or_else(|| format!("VTDecompressionSessionCreate failed ({status})"))?;
        // SAFETY: a created session is returned retained (+1); `CFRetained` takes that reference over.
        let session = unsafe { CFRetained::from_raw(session) };
        Ok(Session { session, format, shared })
    }

    /// Submit one length-prefixed access unit (asynchronous decompression: its picture arrives
    /// through the callback, possibly after this returns).
    fn submit(&self, sample: &[u8], pts: i64) -> std::result::Result<(), String> {
        let buffer = sample_buffer(sample, pts, &self.format)?;
        let mut info = VTDecodeInfoFlags(0);
        // SAFETY: the session and sample buffer are valid (VideoToolbox retains the sample buffer
        // for as long as it needs it); the frame refcon is not a pointer, it carries the pts.
        let status =
            unsafe { self.session.decode_frame(&buffer, VTDecodeFrameFlags::Frame_EnableAsynchronousDecompression, pts as isize as *mut c_void, &mut info) };
        if status != 0 {
            return Err(format!("VTDecompressionSessionDecodeFrame failed ({status})"));
        }
        Ok(())
    }

    /// Wait until every submitted picture has been emitted.
    fn wait(&self) {
        // SAFETY: the session is valid.
        unsafe { self.session.wait_for_asynchronous_frames() };
    }

    /// What the callback produced so far.
    fn take(&self) -> Vec<Output> {
        std::mem::take(&mut *self.shared.out.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// Access units decoding at once (asynchronous decompression overlaps decoding a picture with
/// copying out the previous one).
const MAX_IN_FLIGHT: usize = 2;

/// A `CMVideoFormatDescription` from the record's parameter sets.
fn format_description(info: &NalStreamInfo) -> std::result::Result<CFRetained<CMFormatDescription>, String> {
    let mut ptrs: Vec<NonNull<u8>> = Vec::with_capacity(info.parameter_sets.len());
    let mut sizes: Vec<usize> = Vec::with_capacity(info.parameter_sets.len());
    for p in &info.parameter_sets {
        ptrs.push(NonNull::new(p.as_ptr() as *mut u8).ok_or("empty parameter set")?);
        sizes.push(p.len());
    }
    let ptr_array = NonNull::new(ptrs.as_mut_ptr()).ok_or("no parameter sets")?;
    let size_array = NonNull::new(sizes.as_mut_ptr()).ok_or("no parameter sets")?;
    let mut out: *const CMFormatDescription = std::ptr::null();
    let ls = info.length_size as i32;
    // SAFETY: `ptrs` / `sizes` hold `len` valid (pointer, size) pairs into `info.parameter_sets`,
    // which outlive the call; CoreMedia copies the parameter sets.
    let status = unsafe {
        match info.codec {
            NalCodec::H264 => CMVideoFormatDescriptionCreateFromH264ParameterSets(None, ptrs.len(), ptr_array, size_array, ls, NonNull::from(&mut out)),
            NalCodec::Hevc => CMVideoFormatDescriptionCreateFromHEVCParameterSets(None, ptrs.len(), ptr_array, size_array, ls, None, NonNull::from(&mut out)),
        }
    };
    let fd = NonNull::new(out as *mut CMFormatDescription).filter(|_| status == 0).ok_or_else(|| format!("bad parameter sets for VideoToolbox ({status})"))?;
    // SAFETY: a "Create" function returns the description retained (+1); `CFRetained` takes it over.
    Ok(unsafe { CFRetained::from_raw(fd) })
}

/// A ready `CMSampleBuffer` holding a copy of `sample`.
fn sample_buffer(sample: &[u8], pts: i64, format: &CMFormatDescription) -> std::result::Result<CFRetained<CMSampleBuffer>, String> {
    if sample.is_empty() {
        return Err("empty sample".into());
    }
    let mut block: *mut CMBlockBuffer = std::ptr::null_mut();
    // SAFETY: CoreMedia allocates (AssureMemoryNow) a block of `len` bytes with the default
    // allocator; no external memory is referenced.
    let status = unsafe {
        CMBlockBuffer::create_with_memory_block(
            None,
            std::ptr::null_mut(),
            sample.len(),
            None,
            std::ptr::null(),
            0,
            sample.len(),
            kCMBlockBufferAssureMemoryNowFlag,
            NonNull::from(&mut block),
        )
    };
    let block = NonNull::new(block).filter(|_| status == 0).ok_or_else(|| format!("CMBlockBufferCreateWithMemoryBlock failed ({status})"))?;
    // SAFETY: created retained (+1).
    let block = unsafe { CFRetained::from_raw(block) };
    let src = NonNull::new(sample.as_ptr() as *mut c_void).ok_or("empty sample")?;
    // SAFETY: copies `len` bytes from `sample` into the block, which holds exactly `len` bytes.
    let status = unsafe { CMBlockBuffer::replace_data_bytes(src, &block, 0, sample.len()) };
    if status != 0 {
        return Err(format!("CMBlockBufferReplaceDataBytes failed ({status})"));
    }
    let invalid = CMTime { value: 0, timescale: 0, flags: CMTimeFlags(0), epoch: 0 };
    let timing = CMSampleTimingInfo {
        duration: invalid,
        presentationTimeStamp: CMTime { value: pts, timescale: 1, flags: CMTimeFlags::Valid, epoch: 0 },
        decodeTimeStamp: invalid,
    };
    let size = sample.len();
    let mut out: *mut CMSampleBuffer = std::ptr::null_mut();
    // SAFETY: one sample with one timing entry and one size entry, all pointing at locals that
    // outlive the call; the sample buffer retains the block buffer and format description.
    let status = unsafe { CMSampleBuffer::create_ready(None, Some(&block), Some(format), 1, 1, &timing, 1, &size, NonNull::from(&mut out)) };
    let out = NonNull::new(out).filter(|_| status == 0).ok_or_else(|| format!("CMSampleBufferCreateReady failed ({status})"))?;
    // SAFETY: created retained (+1).
    Ok(unsafe { CFRetained::from_raw(out) })
}

/// Whether VideoToolbox reports a hardware decoder for the codec at all.
pub fn hardware_available(codec: NalCodec) -> bool {
    let t = match codec {
        NalCodec::H264 => u32::from_be_bytes(*b"avc1"),
        NalCodec::Hevc => u32::from_be_bytes(*b"hvc1"),
    };
    // SAFETY: a pure query with no pointer arguments.
    unsafe { objc2_video_toolbox::VTIsHardwareDecodeSupported(t) }
}

/// The VideoToolbox decoder for one `avcC` / `hvcC` stream.
pub struct VtDecoder {
    info: NalStreamInfo,
    layout: Layout,
    /// `None` after a reset until the next sample (a fresh session per decoding run).
    session: Option<Session>,
    /// Decoded pictures waiting for presentation order (sorted by pts).
    pending: Vec<DecodedFrame>,
    /// Presentation times of submitted access units whose picture has not arrived yet.
    in_flight: Vec<i64>,
    /// The run started at a CRA / BLA picture: its RASL pictures are not decodable and are left
    /// out (as the software decoder does).
    skip_rasl: bool,
    first: bool,
    /// Test hook: fail every decode once this many samples were fed.
    fail_after: Option<u64>,
    fed: u64,
    name: &'static str,
}

impl VtDecoder {
    /// A decoder for the stream, or why VideoToolbox does not take it (unsupported format, no
    /// hardware decoder, session creation failed).
    pub fn new(info: NalStreamInfo) -> std::result::Result<Self, String> {
        let layout = layout(&info)?;
        if !hardware_available(info.codec) {
            return Err("no hardware decoder".into());
        }
        // Create the session now so unsupported streams are declined up front.
        let session = Session::new(&info, layout.clone())?;
        let name = match info.codec {
            NalCodec::H264 => "VideoToolbox H.264",
            NalCodec::Hevc => "VideoToolbox HEVC",
        };
        Ok(Self {
            info,
            layout,
            session: Some(session),
            pending: Vec::new(),
            in_flight: Vec::new(),
            skip_rasl: false,
            first: true,
            fail_after: None,
            fed: 0,
            name,
        })
    }

    /// Test hook: from the `n`-th sample on, every decode fails as a lost session would.
    pub fn fail_after(&mut self, n: u64) {
        self.fail_after = Some(n);
    }

    fn insert(&mut self, f: DecodedFrame) {
        let at = self.pending.partition_point(|p| p.pts <= f.pts);
        self.pending.insert(at, f);
    }

    /// Pictures that are due: everything beyond the reorder depth, smallest pts first.
    fn bump(&mut self) -> Vec<DecodedFrame> {
        let mut out = Vec::new();
        // A picture still decoding counts towards the depth and may come before the waiting ones.
        while self.pending.len() + self.in_flight.len() > self.info.reorder {
            let first = self.pending.first().map(|f| f.pts);
            match (first, self.in_flight.iter().min()) {
                (Some(p), Some(&q)) if q < p => break,
                (Some(_), _) => out.push(self.pending.remove(0)),
                (None, _) => break,
            }
        }
        out
    }

    /// Move the callback's pictures into the reorder buffer.
    fn collect(&mut self) -> std::result::Result<(), String> {
        let Some(session) = self.session.as_ref() else { return Ok(()) };
        for o in session.take() {
            match o {
                Output::Frame(f) => {
                    if let Some(i) = self.in_flight.iter().position(|&p| p == f.pts) {
                        self.in_flight.swap_remove(i);
                    }
                    self.insert(f);
                }
                Output::Failed(e) => return Err(e),
            }
        }
        Ok(())
    }
}

impl VideoDecoder for VtDecoder {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        self.fed += 1;
        if self.fail_after.is_some_and(|n| self.fed > n) {
            return Err(CodecError::Decode("VideoToolbox session failed (test hook)".into()));
        }
        if self.info.codec == NalCodec::Hevc {
            let types = self.info.nal_types(sample);
            let irap = types.iter().any(|t| (16..=23).contains(t));
            if irap {
                // CRA / BLA starting a run: its RASL pictures reference pictures we never decoded.
                self.skip_rasl = self.first && types.iter().any(|t| (16..=21).contains(t) && !(19..=20).contains(t));
            } else if self.skip_rasl && types.iter().any(|t| matches!(t, 8 | 9)) {
                self.first = false;
                return Ok(Vec::new());
            }
        }
        self.first = false;
        if self.session.is_none() {
            self.session = Some(Session::new(&self.info, self.layout.clone()).map_err(CodecError::Decode)?);
        }
        let Some(session) = self.session.as_ref() else {
            return Err(CodecError::Decode("no VideoToolbox session".into()));
        };
        session.submit(sample, pts).map_err(CodecError::Decode)?;
        self.in_flight.push(pts);
        if self.in_flight.len() > MAX_IN_FLIGHT {
            session.wait();
        }
        self.collect().map_err(CodecError::Decode)?;
        Ok(self.bump())
    }

    fn flush(&mut self) -> Vec<DecodedFrame> {
        if let Some(s) = self.session.as_ref() {
            s.wait();
        }
        if let Err(e) = self.collect() {
            log::warn!("{}: {e} while flushing", self.name);
        }
        self.in_flight.clear();
        std::mem::take(&mut self.pending)
    }

    fn reset(&mut self) {
        // A fresh session per run (made on the next sample): no reference pictures survive a seek.
        self.session = None;
        self.pending.clear();
        self.in_flight.clear();
        self.skip_rasl = false;
        self.first = true;
    }

    fn name(&self) -> &str {
        self.name
    }

    fn is_random_access(&self, sample: &[u8]) -> Option<bool> {
        self.info.is_random_access(sample)
    }

    fn is_disposable(&self, sample: &[u8]) -> bool {
        self.info.is_disposable(sample)
    }
}
