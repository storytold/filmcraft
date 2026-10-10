//! The video decoder trait and the built-in Motion-JPEG decoder.

use filmcraft_frame::VideoFrame;
use filmcraft_isobmff::{CodecConfig, SampleEntry};

use crate::{CodecError, Result};

/// A decoded picture with its presentation timestamp (track timescale).
pub struct DecodedFrame {
    pub pts: i64,
    pub frame: VideoFrame,
    /// Decoded in draft mode ([`VideoDecoder::set_draft`]): approximate, for reduced-resolution
    /// playback only.
    pub draft: bool,
}

/// A stateful video decoder. Samples are fed in decode order; pictures come out in
/// presentation order (a decoder with reordering may return zero or several per call).
pub trait VideoDecoder: Send {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>>;
    /// Drain pictures held for reordering (end of stream / before a seek).
    fn flush(&mut self) -> Vec<DecodedFrame>;
    /// Forget all state (called after seeking to a sync sample).
    fn reset(&mut self);
    fn name(&self) -> &str;
    /// Every picture is coded independently (no reordering, no references), so frames can be
    /// decoded in any order and in parallel by separate decoder instances.
    fn intra_only(&self) -> bool {
        false
    }
    /// Whether decoding can start at `sample` (`None`: unknown, trust the container's sync flags).
    /// Containers may flag samples as sync that the codec cannot start from (an MP4 without
    /// `stss` marks every sample), so codecs that can tell say so.
    fn is_random_access(&self, _sample: &[u8]) -> Option<bool> {
        None
    }
    /// Whether `sample` can be left out without changing any other picture: a non-reference
    /// picture. Decoding forward to a frame while catching up skips such samples when their
    /// frames are late ([`filmcraft_media::cancel::catch_up`]). False when unknown.
    fn is_disposable(&self, _sample: &[u8]) -> bool {
        false
    }
    /// Draft decoding for reduced-resolution playback (off by default; see
    /// [`filmcraft_media::cancel::with_draft`]): samples fed from now on may be decoded with
    /// shortcuts that only change pictures nothing references, which come out flagged
    /// [`DecodedFrame::draft`]. Decoders without such a mode ignore it.
    fn set_draft(&mut self, _on: bool) {}
    /// Made with fewer worker threads than a decoder for playback gets, because it was made for
    /// background work ([`filmcraft_media::cancel::with_background`]). A request for a monitor
    /// replaces such a decoder.
    fn thread_limited(&self) -> bool {
        false
    }
}

/// Worker threads of the H.264 decoders made for background work (thumbnails): a frame-threaded
/// 4K decoder holds `threads + 2` pictures in flight (~0.5 GB with 16 threads), and the frame
/// workers already decode different files in parallel.
const BACKGROUND_H264_THREADS: usize = 2;

/// NAL unit headers of a length-prefixed (avcC / hvcC) sample: the first two bytes of each unit.
fn nal_headers(sample: &[u8], length_size: usize) -> impl Iterator<Item = (u8, u8)> + '_ {
    let mut pos = 0usize;
    std::iter::from_fn(move || {
        if length_size == 0 || length_size > 4 || pos + length_size > sample.len() {
            return None;
        }
        let len = sample[pos..pos + length_size].iter().fold(0usize, |a, &b| (a << 8) | b as usize);
        pos += length_size;
        if len == 0 || pos + len > sample.len() {
            return None;
        }
        let h = (sample[pos], if len > 1 { sample[pos + 1] } else { 0 });
        pos += len;
        Some(h)
    })
}

/// H.264 (7.4.1): every coded slice (NAL types 1-5) of the access unit has nal_ref_idc 0.
pub fn h264_disposable(sample: &[u8], length_size: usize) -> bool {
    let mut slices = 0;
    for (h, _) in nal_headers(sample, length_size) {
        if (1..=5).contains(&(h & 0x1f)) {
            if h & 0x60 != 0 {
                return false;
            }
            slices += 1;
        }
    }
    slices > 0
}

/// [`h264_disposable`] for an Annex B (start-code) sample.
pub fn h264_disposable_annexb(sample: &[u8]) -> bool {
    let mut slices = 0;
    for n in filmcraft_bitstream::annexb_nals(sample) {
        let Some(&h) = n.first() else { continue };
        if (1..=5).contains(&(h & 0x1f)) {
            if h & 0x60 != 0 {
                return false;
            }
            slices += 1;
        }
    }
    slices > 0
}

/// HEVC (7.4.2.2): every VCL NAL unit is a sub-layer non-reference picture (TRAIL_N, TSA_N,
/// STSA_N, RADL_N, RASL_N, RSV_VCL_N10/12/14) of the highest temporal sub-layer, so no picture
/// references it. `highest_tid` = numTemporalLayers - 1 from the hvcC (None: unknown).
pub fn hevc_disposable(sample: &[u8], length_size: usize, highest_tid: Option<u8>) -> bool {
    let Some(top) = highest_tid else { return false };
    let mut slices = 0;
    for (h0, h1) in nal_headers(sample, length_size) {
        let t = (h0 >> 1) & 0x3f;
        if t < 32 {
            let tid = (h1 & 7).saturating_sub(1);
            if t > 14 || t % 2 == 1 || tid != top {
                return false;
            }
            slices += 1;
        }
    }
    slices > 0
}

/// A plane as a tight `w`×`h` buffer. Decoders hand over owned planes that usually are tight
/// already: those are moved, not copied (a 2160p 4:2:0 picture is 12 MB per copy).
pub(crate) fn tight_plane<T: Copy>(src: Vec<T>, stride: usize, w: usize, h: usize) -> Vec<T> {
    if stride == w && src.len() >= w * h {
        let mut v = src;
        v.truncate(w * h);
        return v;
    }
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        out.extend_from_slice(&src[y * stride..y * stride + w]);
    }
    out
}

/// Colour of an H.264 / HEVC picture from its VUI code points (ITU-T H.273), as the decoders
/// report it: BT.709 by default with the matrix guessed from the size, the signalled matrix and
/// transfer when known, and full range when flagged. Shared by the software and hardware
/// decoders so the two are interchangeable.
pub fn vui_color(width: u32, height: u32, matrix: u8, transfer: u8, full_range: bool) -> filmcraft_color::ColorInfo {
    let mut color = filmcraft_color::ColorInfo { matrix: filmcraft_frame::default_matrix(width, height), ..filmcraft_color::ColorInfo::REC709 };
    if let Some(m) = filmcraft_color::Matrix::from_code(matrix) {
        color.matrix = m;
    }
    if let Some(t) = filmcraft_color::Transfer::from_code(transfer) {
        color.transfer = t;
    }
    if full_range {
        color.range = filmcraft_color::Range::Full;
    }
    color
}

/// Pixel aspect ratio from a VUI sample aspect ratio ((0, 0) = unspecified = square).
pub fn sar_par(sar: (u16, u16)) -> (u32, u32) {
    if sar.0 > 0 && sar.1 > 0 { (sar.0 as u32, sar.1 as u32) } else { (1, 1) }
}

/// NAL length-prefix size of an `avcC` record (byte 4: lengthSizeMinusOne).
pub fn avcc_length_size(avcc: &[u8]) -> usize {
    avcc.get(4).map_or(4, |b| (b & 3) as usize + 1)
}

/// NAL length-prefix size and highest TemporalId of an `hvcC` record.
pub fn hvcc_length_size_and_tid(hvcc: &[u8]) -> (usize, Option<u8>) {
    // hvcC byte 21: constantFrameRate(2) numTemporalLayers(3) temporalIdNested(1) lengthSizeMinusOne(2)
    let length_size = hvcc.get(21).map_or(4, |b| (b & 3) as usize + 1);
    let highest_tid = hvcc.get(21).map(|b| (b >> 3) & 7).filter(|&n| n > 0).map(|n| n - 1);
    (length_size, highest_tid)
}

/// A factory returns `None` when it does not handle the entry.
pub type VideoDecoderFactory = fn(&SampleEntry) -> Option<Result<Box<dyn VideoDecoder>>>;

/// Motion-JPEG / Photo-JPEG (each sample is a complete JPEG).
pub struct MjpegDecoder;

impl VideoDecoder for MjpegDecoder {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        // mjpa samples may contain two fields; decode the first image (field-merging lands with interlace support).
        let img = image::load_from_memory_with_format(sample, image::ImageFormat::Jpeg).map_err(|e| CodecError::Decode(e.to_string()))?;
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        Ok(vec![DecodedFrame { pts, frame: VideoFrame::rgba8(w, h, rgba.into_raw()), draft: false }])
    }
    fn flush(&mut self) -> Vec<DecodedFrame> {
        Vec::new()
    }
    fn reset(&mut self) {}
    fn name(&self) -> &str {
        "Motion JPEG"
    }
    fn intra_only(&self) -> bool {
        true
    }
}

pub fn mjpeg_factory(e: &SampleEntry) -> Option<Result<Box<dyn VideoDecoder>>> {
    matches!(e.codec, CodecConfig::Jpeg { .. }).then(|| Ok(Box::new(MjpegDecoder) as Box<dyn VideoDecoder>))
}

/// Our pure-Rust H.264 decoder (frame-threaded).
pub struct H264Decoder {
    avcc: Vec<u8>,
    dec: filmcraft_h264::Decoder,
    length_size: usize,
    draft: bool,
    /// Worker threads (a background decoder gets fewer).
    threads: usize,
}

impl H264Decoder {
    /// The worker threads for a decoder made now.
    fn threads_now() -> usize {
        if filmcraft_media::cancel::background() { BACKGROUND_H264_THREADS.min(h264_threads()) } else { h264_threads() }
    }

    fn make_inner(avcc: &[u8], threads: usize) -> Result<filmcraft_h264::Decoder> {
        let mut dec = filmcraft_h264::Decoder::with_threads(threads);
        if !avcc.is_empty() {
            dec.configure_avcc(avcc).map_err(|e| CodecError::Decode(e.to_string()))?;
        }
        Ok(dec)
    }

    pub fn new(avcc: Vec<u8>) -> Result<Self> {
        recycle_h264_planes();
        let threads = Self::threads_now();
        let dec = Self::make_inner(&avcc, threads)?;
        let length_size = avcc_length_size(&avcc);
        Ok(Self { avcc, dec, length_size, draft: false, threads })
    }
    /// A decoder for Annex B byte-stream samples (start codes, in-band parameter sets: MXF, TS).
    pub fn annexb() -> Self {
        recycle_h264_planes();
        let threads = Self::threads_now();
        Self { avcc: Vec::new(), dec: filmcraft_h264::Decoder::with_threads(threads), length_size: 0, draft: false, threads }
    }
    fn convert(p: filmcraft_h264::Picture) -> DecodedFrame {
        use std::sync::Arc;
        let draft = p.draft;
        let (w, h) = (p.width as usize, p.height as usize);
        let (cw, ch) = (p.chroma_width as usize, p.chroma_height as usize);
        let y = tight_plane(p.y, p.y_stride, w, h);
        let u = tight_plane(p.u, p.uv_stride, cw, ch);
        let v = tight_plane(p.v, p.uv_stride, cw, ch);
        let color = vui_color(p.width, p.height, p.color.matrix, p.color.transfer, p.color.full_range);
        let par = sar_par(p.sar);
        let frame = VideoFrame {
            width: p.width,
            height: p.height,
            data: filmcraft_frame::PixelData::Yuv8 { planes: [Arc::new(y), Arc::new(u), Arc::new(v)], chroma: filmcraft_frame::Chroma::C420, alpha: None },
            color,
            par,
            pts: filmcraft_time::Tick::ZERO,
        };
        DecodedFrame { pts: p.pts, frame, draft }
    }
}

impl VideoDecoder for H264Decoder {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        let pics = self.dec.decode(sample, pts).map_err(|e| CodecError::Decode(e.to_string()))?;
        Ok(pics.into_iter().map(Self::convert).collect())
    }
    fn flush(&mut self) -> Vec<DecodedFrame> {
        self.dec.flush().into_iter().map(Self::convert).collect()
    }
    fn reset(&mut self) {
        if let Ok(d) = Self::make_inner(&self.avcc, self.threads) {
            self.dec = d;
        }
        self.dec.set_draft(self.draft);
    }
    fn name(&self) -> &str {
        "FilmCraft H.264"
    }
    fn thread_limited(&self) -> bool {
        self.threads < h264_threads()
    }
    fn set_draft(&mut self, on: bool) {
        self.draft = on;
        self.dec.set_draft(on);
    }
    fn is_disposable(&self, sample: &[u8]) -> bool {
        if self.length_size == 0 {
            return h264_disposable_annexb(sample);
        }
        h264_disposable(sample, self.length_size)
    }
    fn is_random_access(&self, sample: &[u8]) -> Option<bool> {
        // Annex B samples carry their parameter sets: an IDR access unit is a starting point.
        (self.length_size == 0).then(|| filmcraft_bitstream::annexb_nals(sample).iter().any(|n| n.first().is_some_and(|h| h & 0x1f == 5)))
    }
}

/// The H.264 decoder's output planes come from the frames the caches evicted
/// (`filmcraft_frame::pool`) instead of the allocator.
fn recycle_h264_planes() {
    filmcraft_h264::set_plane_allocator(filmcraft_frame::pool::take_u8);
}

/// Worker threads each H.264 decoder uses (frame threading; 1 on wasm).
pub fn h264_threads() -> usize {
    filmcraft_h264::default_threads()
}

pub fn h264_factory(e: &SampleEntry) -> Option<Result<Box<dyn VideoDecoder>>> {
    match &e.codec {
        CodecConfig::Avc(a) => Some(H264Decoder::new(a.to_bytes()).map(|d| Box::new(d) as Box<dyn VideoDecoder>)),
        _ => None,
    }
}

/// Our pure-Rust HEVC decoder (Main / Main 10, frame-threaded).
pub struct HevcDecoder {
    hvcc: Vec<u8>,
    dec: filmcraft_hevc::Decoder,
    length_size: usize,
    highest_tid: Option<u8>,
}

impl HevcDecoder {
    pub fn new(hvcc: Vec<u8>) -> Result<Self> {
        let dec = filmcraft_hevc::Decoder::from_hvcc(&hvcc).map_err(|e| CodecError::Decode(e.to_string()))?;
        let (length_size, highest_tid) = hvcc_length_size_and_tid(&hvcc);
        Ok(Self { hvcc, dec, length_size, highest_tid })
    }
    /// A decoder for Annex B byte-stream samples (start codes, in-band parameter sets: TS).
    pub fn annexb() -> Self {
        Self { hvcc: Vec::new(), dec: filmcraft_hevc::Decoder::new(), length_size: 0, highest_tid: None }
    }
    fn convert(p: filmcraft_hevc::Picture) -> DecodedFrame {
        use filmcraft_hevc::Plane;
        use std::sync::Arc;
        let (w, h) = (p.width as usize, p.height as usize);
        let (cw, ch) = (p.chroma_width as usize, p.chroma_height as usize);
        let (ys, uvs) = (p.y_stride, p.uv_stride);
        let wide = |pl: Plane| -> Vec<u16> {
            match pl {
                Plane::U16(v) => v,
                Plane::U8(v) => v.into_iter().map(u16::from).collect(),
            }
        };
        let data = match (p.y, p.u, p.v) {
            (Plane::U8(y), Plane::U8(u), Plane::U8(v)) => filmcraft_frame::PixelData::Yuv8 {
                planes: [Arc::new(tight_plane(y, ys, w, h)), Arc::new(tight_plane(u, uvs, cw, ch)), Arc::new(tight_plane(v, uvs, cw, ch))],
                chroma: filmcraft_frame::Chroma::C420,
                alpha: None,
            },
            (y, u, v) => filmcraft_frame::PixelData::Yuv16 {
                planes: [Arc::new(tight_plane(wide(y), ys, w, h)), Arc::new(tight_plane(wide(u), uvs, cw, ch)), Arc::new(tight_plane(wide(v), uvs, cw, ch))],
                chroma: filmcraft_frame::Chroma::C420,
                bits: p.bit_depth,
                alpha: None,
            },
        };
        let color = vui_color(p.width, p.height, p.color.matrix, p.color.transfer, p.color.full_range);
        let par = sar_par(p.sar);
        let frame = VideoFrame { width: p.width, height: p.height, data, color, par, pts: filmcraft_time::Tick::ZERO };
        DecodedFrame { pts: p.pts, frame, draft: p.draft }
    }
}

impl VideoDecoder for HevcDecoder {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        let pics = self.dec.decode(sample, pts).map_err(|e| CodecError::Decode(e.to_string()))?;
        Ok(pics.into_iter().map(Self::convert).collect())
    }
    fn flush(&mut self) -> Vec<DecodedFrame> {
        self.dec.flush().into_iter().map(Self::convert).collect()
    }
    fn reset(&mut self) {
        if self.hvcc.is_empty() {
            self.dec = filmcraft_hevc::Decoder::new();
        } else if let Ok(d) = filmcraft_hevc::Decoder::from_hvcc(&self.hvcc) {
            self.dec = d;
        }
    }
    fn name(&self) -> &str {
        "FilmCraft HEVC"
    }
    fn is_disposable(&self, sample: &[u8]) -> bool {
        hevc_disposable(sample, self.length_size, self.highest_tid)
    }
    fn set_draft(&mut self, on: bool) {
        self.dec.set_draft(on);
    }
    fn is_random_access(&self, sample: &[u8]) -> Option<bool> {
        // Annex B samples carry their parameter sets: an IDR / CRA / BLA access unit is a
        // starting point.
        (self.length_size == 0)
            .then(|| filmcraft_bitstream::annexb_nals(sample).iter().any(|n| n.first().is_some_and(|h| (16..=21).contains(&((h >> 1) & 0x3F)))))
    }
}

pub fn hevc_factory(e: &SampleEntry) -> Option<Result<Box<dyn VideoDecoder>>> {
    match &e.codec {
        CodecConfig::Hevc(c) => Some(HevcDecoder::new(c.to_bytes()).map(|d| Box::new(d) as Box<dyn VideoDecoder>)),
        _ => None,
    }
}

/// Our pure-Rust VP9 decoder (profiles 0-3, 8/10/12-bit; tile columns and loop filter decode in
/// parallel).
pub struct Vp9Decoder {
    dec: filmcraft_vp9::Decoder,
    /// Container colour (vpcC / Matroska `Colour`): transfer and primaries are not in the VP9
    /// bitstream.
    transfer: Option<filmcraft_color::Transfer>,
    primaries: Option<filmcraft_color::Primaries>,
}

/// Colour primaries from an ISO/IEC 23091-2 code.
pub(crate) fn primaries_from_code(c: u8) -> Option<filmcraft_color::Primaries> {
    use filmcraft_color::Primaries;
    match c {
        1 => Some(Primaries::Bt709),
        5 => Some(Primaries::Bt601_625),
        6 => Some(Primaries::Bt601_525),
        9 => Some(Primaries::Bt2020),
        12 => Some(Primaries::P3D65),
        _ => None,
    }
}

impl Vp9Decoder {
    pub fn new(cfg: Option<&filmcraft_isobmff::VpcConfig>) -> Self {
        Self {
            dec: filmcraft_vp9::Decoder::new(),
            transfer: cfg.and_then(|c| filmcraft_color::Transfer::from_code(c.transfer_characteristics)),
            primaries: cfg.and_then(|c| primaries_from_code(c.colour_primaries)),
        }
    }

    /// `None` for a picture whose planes mix sample types (never produced by the decoder).
    fn convert(&self, p: filmcraft_vp9::Picture) -> Option<DecodedFrame> {
        use filmcraft_color::Range;
        use filmcraft_frame::{Chroma, PixelData};
        use filmcraft_vp9::Plane;
        use std::sync::Arc;
        let (w, h) = (p.width as usize, p.height as usize);
        let (cw, ch) = (p.chroma_width as usize, p.chroma_height as usize);
        // (shared with the hardware decoders: `hw_frame::vp9_color`)
        let color = crate::hw_frame::vp9_color(p.width, p.height, p.color.color_space, p.color.full_range, self.transfer, self.primaries);
        let (pts, draft) = (p.pts, p.draft);
        if p.color.color_space == 7 {
            // RGB (profiles 1 / 3, 4:4:4): the planes carry G, B, R.
            let shift = p.bit_depth - 8;
            let mut rgba = Vec::with_capacity(w * h * 4);
            for i in 0..w * h {
                rgba.extend_from_slice(&[(p.v.get(i) >> shift) as u8, (p.y.get(i) >> shift) as u8, (p.u.get(i) >> shift) as u8, 255]);
            }
            let mut frame = VideoFrame::rgba8(p.width, p.height, rgba);
            frame.color = filmcraft_color::ColorInfo { range: Range::Full, transfer: filmcraft_color::Transfer::Srgb, ..color };
            return Some(DecodedFrame { pts, frame, draft });
        }
        // 4:4:0 has no frame format of its own: chroma rows are repeated to 4:4:4.
        let (chroma, rows_440) = match (p.subsampling_x, p.subsampling_y) {
            (true, true) => (Chroma::C420, false),
            (true, false) => (Chroma::C422, false),
            (false, false) => (Chroma::C444, false),
            (false, true) => (Chroma::C444, true),
        };
        // planes are moved (tight already), not copied
        fn expand<T: Copy>(v: Vec<T>, cw: usize, ch: usize, h: usize, rows_440: bool) -> Vec<T> {
            if !rows_440 {
                return tight_plane(v, cw, cw, ch);
            }
            let mut out = Vec::with_capacity(cw * h);
            for y in 0..h {
                out.extend_from_slice(&v[(y >> 1) * cw..(y >> 1) * cw + cw]);
            }
            out
        }
        let data = match (p.y, p.u, p.v) {
            (Plane::U8(y), Plane::U8(u), Plane::U8(v)) => PixelData::Yuv8 {
                planes: [Arc::new(tight_plane(y, w, w, h)), Arc::new(expand(u, cw, ch, h, rows_440)), Arc::new(expand(v, cw, ch, h, rows_440))],
                chroma,
                alpha: None,
            },
            (Plane::U16(y), Plane::U16(u), Plane::U16(v)) => PixelData::Yuv16 {
                planes: [Arc::new(tight_plane(y, w, w, h)), Arc::new(expand(u, cw, ch, h, rows_440)), Arc::new(expand(v, cw, ch, h, rows_440))],
                chroma,
                bits: p.bit_depth,
                alpha: None,
            },
            _ => return None,
        };
        // render_size (the intended display size) is not applied: the container's display
        // dimensions / pixel aspect describe the presentation.
        let par = (1, 1);
        Some(DecodedFrame { pts, frame: VideoFrame { width: p.width, height: p.height, data, color, par, pts: filmcraft_time::Tick::ZERO }, draft })
    }
}

impl VideoDecoder for Vp9Decoder {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        let pics = self.dec.decode(sample, pts).map_err(|e| CodecError::Decode(e.to_string()))?;
        Ok(pics.into_iter().filter_map(|p| self.convert(p)).collect())
    }
    fn flush(&mut self) -> Vec<DecodedFrame> {
        self.dec.flush().into_iter().filter_map(|p| self.convert(p)).collect()
    }
    fn reset(&mut self) {
        self.dec.reset();
    }
    fn name(&self) -> &str {
        "FilmCraft VP9"
    }
    fn set_draft(&mut self, on: bool) {
        self.dec.set_draft(on);
    }
    fn is_random_access(&self, sample: &[u8]) -> Option<bool> {
        Some(filmcraft_vp9::is_keyframe(sample))
    }
}

pub fn vp9_factory(e: &SampleEntry) -> Option<Result<Box<dyn VideoDecoder>>> {
    match &e.codec {
        CodecConfig::Vp9(c) => Some(Ok(Box::new(Vp9Decoder::new(Some(c))) as Box<dyn VideoDecoder>)),
        _ => None,
    }
}

/// Our AV1 decoder. The `av1C` configuration OBUs (sequence header) are fed before the first
/// sample and again after every reset.
pub struct Av1Decoder {
    dec: filmcraft_av1::Decoder,
    config_obus: Vec<u8>,
    primed: bool,
}

impl Av1Decoder {
    pub fn new(config_obus: Vec<u8>) -> Av1Decoder {
        Av1Decoder { dec: filmcraft_av1::Decoder::new(), config_obus, primed: false }
    }

    fn convert(p: filmcraft_av1::Picture, pts: i64) -> DecodedFrame {
        use std::sync::Arc;
        let draft = p.draft;
        let w = p.width as usize;
        let h = p.height as usize;
        // (shared with the hardware decoders: `hw_frame::av1_color`)
        let color = crate::hw_frame::av1_color(p.width, p.height, p.matrix_coefficients, p.transfer_characteristics, p.color_primaries, p.full_range);
        let chroma = match (p.subsampling_x, p.subsampling_y) {
            (1, 1) => filmcraft_frame::Chroma::C420,
            (1, 0) => filmcraft_frame::Chroma::C422,
            _ => filmcraft_frame::Chroma::C444,
        };
        let cw = (w + p.subsampling_x as usize) >> p.subsampling_x;
        let ch = (h + p.subsampling_y as usize) >> p.subsampling_y;
        let [y, mut u, mut v] = p.planes;
        if p.mono_chrome {
            u = vec![1u16 << (p.bit_depth - 1); cw * ch];
            v = u.clone();
        }
        let data = if p.bit_depth == 8 {
            let to8 = |p: Vec<u16>| Arc::new(p.into_iter().map(|v| v as u8).collect::<Vec<u8>>());
            filmcraft_frame::PixelData::Yuv8 { planes: [to8(y), to8(u), to8(v)], chroma, alpha: None }
        } else {
            filmcraft_frame::PixelData::Yuv16 { planes: [Arc::new(y), Arc::new(u), Arc::new(v)], chroma, bits: p.bit_depth as u32, alpha: None }
        };
        DecodedFrame { pts, frame: VideoFrame { width: p.width, height: p.height, data, color, par: (1, 1), pts: filmcraft_time::Tick::ZERO }, draft }
    }
}

impl VideoDecoder for Av1Decoder {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        if !self.primed {
            self.primed = true;
            if !self.config_obus.is_empty() {
                self.dec.decode(&self.config_obus).map_err(|e| CodecError::Decode(e.to_string()))?;
            }
        }
        // With frame threads pictures can come out of a later call; each carries its own pts.
        let pics = self.dec.decode_pts(sample, pts).map_err(|e| CodecError::Decode(e.to_string()))?;
        Ok(pics
            .into_iter()
            .map(|p| {
                let pts = p.pts;
                Self::convert(p, pts)
            })
            .collect())
    }
    fn flush(&mut self) -> Vec<DecodedFrame> {
        self.dec
            .flush()
            .into_iter()
            .map(|p| {
                let pts = p.pts;
                Self::convert(p, pts)
            })
            .collect()
    }
    fn reset(&mut self) {
        self.dec = filmcraft_av1::Decoder::new();
        self.primed = false;
    }
    fn name(&self) -> &str {
        "FilmCraft AV1"
    }
    fn set_draft(&mut self, on: bool) {
        self.dec.set_draft(on);
    }
    fn is_random_access(&self, sample: &[u8]) -> Option<bool> {
        Some(filmcraft_av1::is_key_frame_unit(sample))
    }
}

pub fn av1_factory(e: &SampleEntry) -> Option<Result<Box<dyn VideoDecoder>>> {
    match &e.codec {
        CodecConfig::Av1(c) => Some(Ok(Box::new(Av1Decoder::new(c.config_obus.clone())) as Box<dyn VideoDecoder>)),
        _ => None,
    }
}

/// Our ProRes decoder (every frame is intra; slices decode in parallel).
pub struct ProResDecoder;

impl VideoDecoder for ProResDecoder {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        use std::sync::Arc;
        let f = filmcraft_prores::decode_frame(sample).map_err(|e| CodecError::Decode(e.to_string()))?;
        let chroma = match f.chroma {
            filmcraft_prores::ChromaFormat::Yuv422 => filmcraft_frame::Chroma::C422,
            filmcraft_prores::ChromaFormat::Yuv444 => filmcraft_frame::Chroma::C444,
        };
        let mut color = filmcraft_color::ColorInfo::REC709;
        if let Some(m) = filmcraft_color::Matrix::from_code(f.color.matrix) {
            color.matrix = m;
        }
        if let Some(t) = filmcraft_color::Transfer::from_code(f.color.transfer) {
            color.transfer = t;
        }
        let frame = VideoFrame {
            width: f.width,
            height: f.height,
            data: filmcraft_frame::PixelData::Yuv16 {
                planes: [Arc::new(f.y), Arc::new(f.cb), Arc::new(f.cr)],
                chroma,
                bits: f.bit_depth as u32,
                alpha: f.alpha.map(Arc::new),
            },
            color,
            par: (1, 1),
            pts: filmcraft_time::Tick::ZERO,
        };
        Ok(vec![DecodedFrame { pts, frame, draft: false }])
    }
    fn flush(&mut self) -> Vec<DecodedFrame> {
        Vec::new()
    }
    fn reset(&mut self) {}
    fn name(&self) -> &str {
        "FilmCraft ProRes"
    }
    fn intra_only(&self) -> bool {
        true
    }
}

pub fn prores_factory(e: &SampleEntry) -> Option<Result<Box<dyn VideoDecoder>>> {
    matches!(e.codec, CodecConfig::ProRes { .. }).then(|| Ok(Box::new(ProResDecoder) as Box<dyn VideoDecoder>))
}

/// Our APV (Advanced Professional Video, RFC 9924) decoder (every frame is intra; tiles decode in
/// parallel).
pub struct ApvDecoder;

/// Convert a decoded APV frame to a [`VideoFrame`].
pub fn apv_to_video_frame(f: filmcraft_apv::Frame) -> VideoFrame {
    use std::sync::Arc;
    let chroma = match f.chroma {
        filmcraft_apv::ChromaFormat::Yuv422 => filmcraft_frame::Chroma::C422,
        filmcraft_apv::ChromaFormat::Monochrome | filmcraft_apv::ChromaFormat::Yuv444 | filmcraft_apv::ChromaFormat::Yuv4444 => filmcraft_frame::Chroma::C444,
    };
    let mut color = filmcraft_color::ColorInfo { matrix: filmcraft_frame::default_matrix(f.width, f.height), ..filmcraft_color::ColorInfo::REC709 };
    if let Some(m) = filmcraft_color::Matrix::from_code(f.color.matrix) {
        color.matrix = m;
    }
    if let Some(t) = filmcraft_color::Transfer::from_code(f.color.transfer) {
        color.transfer = t;
    }
    if let Some(p) = primaries_from_code(f.color.primaries) {
        color.primaries = p;
    }
    if f.color.full_range {
        color.range = filmcraft_color::Range::Full;
    }
    let neutral = 1u16 << f.bit_depth.saturating_sub(1);
    let (cb, cr) = if f.chroma == filmcraft_apv::ChromaFormat::Monochrome {
        let u = vec![neutral; (f.width as usize) * (f.height as usize)];
        (u.clone(), u)
    } else {
        (f.cb, f.cr)
    };
    let data = if f.bit_depth == 8 {
        let to8 = |p: Vec<u16>| Arc::new(p.into_iter().map(|v| v as u8).collect::<Vec<u8>>());
        filmcraft_frame::PixelData::Yuv8 { planes: [to8(f.y), to8(cb), to8(cr)], chroma, alpha: f.alpha.map(to8) }
    } else {
        filmcraft_frame::PixelData::Yuv16 {
            planes: [Arc::new(f.y), Arc::new(cb), Arc::new(cr)],
            chroma,
            bits: f.bit_depth as u32,
            alpha: f.alpha.map(Arc::new),
        }
    };
    VideoFrame { width: f.width, height: f.height, data, color, par: (1, 1), pts: filmcraft_time::Tick::ZERO }
}

impl VideoDecoder for ApvDecoder {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        let f = filmcraft_apv::decode_frame(sample).map_err(|e| CodecError::Decode(e.to_string()))?;
        Ok(vec![DecodedFrame { pts, frame: apv_to_video_frame(f), draft: false }])
    }
    fn flush(&mut self) -> Vec<DecodedFrame> {
        Vec::new()
    }
    fn reset(&mut self) {}
    fn name(&self) -> &str {
        "FilmCraft APV"
    }
    fn intra_only(&self) -> bool {
        true
    }
}

pub fn apv_factory(e: &SampleEntry) -> Option<Result<Box<dyn VideoDecoder>>> {
    matches!(e.codec, CodecConfig::Apv(_)).then(|| Ok(Box::new(ApvDecoder) as Box<dyn VideoDecoder>))
}

/// Our DNxHD / DNxHR (VC-3) decoder (every frame is intra; macroblock rows decode in parallel).
pub struct DnxDecoder;

/// Convert a decoded VC-3 frame to a [`VideoFrame`]. RGB (4:4:4) frames are converted to
/// BT.709 video-range Y'CbCr 4:4:4 at the coded depth.
pub fn dnx_to_video_frame(f: filmcraft_dnx::Frame) -> VideoFrame {
    use std::sync::Arc;
    let chroma = match f.chroma {
        filmcraft_dnx::ChromaFormat::Yuv420 => filmcraft_frame::Chroma::C420,
        filmcraft_dnx::ChromaFormat::Yuv422 => filmcraft_frame::Chroma::C422,
        filmcraft_dnx::ChromaFormat::Yuv444 => filmcraft_frame::Chroma::C444,
    };
    let mut color = filmcraft_color::ColorInfo::REC709;
    if matches!(f.color_volume, filmcraft_dnx::ColorVolume::Bt2020Ncl | filmcraft_dnx::ColorVolume::Bt2020Cl) {
        color.matrix = filmcraft_color::Matrix::Bt2020Ncl;
        color.primaries = filmcraft_color::Primaries::Bt2020;
    }
    let (mut y, mut cb, mut cr) = (f.y, f.cb, f.cr);
    if f.rgb {
        // planes hold G, B, R (video range); derive Y'CbCr with the stream's matrix
        let (kr, kb) = color.matrix.kr_kb();
        let kg = 1.0 - kr - kb;
        let s = (1u32 << (f.bit_depth - 8)) as f32;
        let max = ((1u32 << f.bit_depth) - 1) as f32;
        let c = 224.0 / 219.0;
        for i in 0..y.len() {
            let (g, b, r) = (y[i] as f32, cb[i] as f32, cr[i] as f32);
            let yy = kr * r + kg * g + kb * b;
            let u = (b - yy) / (2.0 * (1.0 - kb)) * c + 128.0 * s;
            let v = (r - yy) / (2.0 * (1.0 - kr)) * c + 128.0 * s;
            y[i] = (yy + 0.5).clamp(0.0, max) as u16;
            cb[i] = (u + 0.5).clamp(0.0, max) as u16;
            cr[i] = (v + 0.5).clamp(0.0, max) as u16;
        }
    }
    let par = match f.par {
        (n, d) if n > 0 && d > 0 => (n as u32, d as u32),
        // thin rasters (1440 / 960 wide) are anamorphic 16:9
        _ if matches!(f.cid, 1244 | 1259 | 1260) => (4, 3),
        _ if f.cid == 1258 => (4, 3),
        _ => (1, 1),
    };
    let data = if f.bit_depth == 8 {
        let to8 = |p: Vec<u16>| Arc::new(p.into_iter().map(|v| v as u8).collect::<Vec<u8>>());
        filmcraft_frame::PixelData::Yuv8 { planes: [to8(y), to8(cb), to8(cr)], chroma, alpha: f.alpha.map(to8) }
    } else {
        filmcraft_frame::PixelData::Yuv16 { planes: [Arc::new(y), Arc::new(cb), Arc::new(cr)], chroma, bits: f.bit_depth as u32, alpha: f.alpha.map(Arc::new) }
    };
    VideoFrame { width: f.width, height: f.height, data, color, par, pts: filmcraft_time::Tick::ZERO }
}

impl VideoDecoder for DnxDecoder {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        let f = filmcraft_dnx::decode_frame(sample).map_err(|e| CodecError::Decode(e.to_string()))?;
        Ok(vec![DecodedFrame { pts, frame: dnx_to_video_frame(f), draft: false }])
    }
    fn flush(&mut self) -> Vec<DecodedFrame> {
        Vec::new()
    }
    fn reset(&mut self) {}
    fn name(&self) -> &str {
        "FilmCraft DNxHD/DNxHR"
    }
    fn intra_only(&self) -> bool {
        true
    }
}

pub fn dnx_factory(e: &SampleEntry) -> Option<Result<Box<dyn VideoDecoder>>> {
    matches!(e.codec, CodecConfig::Dnx { .. }).then(|| Ok(Box::new(DnxDecoder) as Box<dyn VideoDecoder>))
}

/// Our MPEG-1 / MPEG-2 video decoder (Main / 4:2:2 profile, field and frame pictures).
pub struct Mpeg2Decoder {
    dec: filmcraft_mpeg2v::Decoder,
    /// Sequence header (and extensions) fed after a reset, so a random-access point without its
    /// own sequence header decodes (MKV `CodecPrivate`, or the stream's first header).
    header: Vec<u8>,
}

impl Mpeg2Decoder {
    pub fn new(header: Vec<u8>) -> Self {
        let mut d = Self { dec: filmcraft_mpeg2v::Decoder::new(), header };
        d.prime();
        d
    }
    fn prime(&mut self) {
        if !self.header.is_empty() {
            let _ = self.dec.decode(&self.header, 0);
        }
    }
}

/// The sequence header with its extensions at the start of an MPEG video sample (up to the first
/// GOP or picture start code).
pub fn mpeg2_sequence_header(sample: &[u8]) -> Option<Vec<u8>> {
    let codes = filmcraft_mpeg2v::start_codes(sample);
    let (start, _) = *codes.iter().find(|c| c.1 == 0xB3)?;
    let end = codes.iter().find(|c| c.0 > start && matches!(c.1, 0x00 | 0xB8 | 0x01..=0xAF)).map_or(sample.len(), |c| c.0);
    Some(sample[start..end].to_vec())
}

/// Convert a decoded MPEG-1/2 picture. Colour comes from the sequence display extension
/// (defaults: BT.601 for SD, BT.709 for HD); the pixel aspect from the aspect ratio code.
pub fn mpeg2_to_video_frame(p: filmcraft_mpeg2v::Picture) -> VideoFrame {
    use std::sync::Arc;
    let info = &p.info;
    let mut color = filmcraft_color::ColorInfo { matrix: filmcraft_frame::default_matrix(p.width, p.height), ..filmcraft_color::ColorInfo::REC709 };
    if info.width <= 1024 && info.height <= 576 {
        color.primaries = if info.height == 576 || info.height == 608 { filmcraft_color::Primaries::Bt601_625 } else { filmcraft_color::Primaries::Bt601_525 };
    }
    if let Some((prim, transfer, matrix)) = info.colour {
        if let Some(m) = filmcraft_color::Matrix::from_code(matrix) {
            color.matrix = m;
        }
        if let Some(t) = filmcraft_color::Transfer::from_code(transfer) {
            color.transfer = t;
        }
        if let Some(pr) = primaries_from_code(prim) {
            color.primaries = pr;
        }
    }
    let chroma = match p.chroma {
        filmcraft_mpeg2v::ChromaFormat::Yuv420 => filmcraft_frame::Chroma::C420,
        filmcraft_mpeg2v::ChromaFormat::Yuv422 => filmcraft_frame::Chroma::C422,
        filmcraft_mpeg2v::ChromaFormat::Yuv444 => filmcraft_frame::Chroma::C444,
    };
    let par = info.sar;
    VideoFrame {
        width: p.width,
        height: p.height,
        data: filmcraft_frame::PixelData::Yuv8 { planes: [Arc::new(p.y), Arc::new(p.cb), Arc::new(p.cr)], chroma, alpha: None },
        color,
        par: if par.0 > 0 && par.1 > 0 { par } else { (1, 1) },
        pts: filmcraft_time::Tick::ZERO,
    }
}

/// "YUV 4:2:2 8-bit, interlaced (upper field first)" for the media info.
pub fn mpeg2_pixel_format(info: &filmcraft_mpeg2v::SequenceInfo, field_order: Option<filmcraft_mpeg2v::FieldOrder>) -> String {
    let sub = match info.chroma {
        filmcraft_mpeg2v::ChromaFormat::Yuv420 => "4:2:0",
        filmcraft_mpeg2v::ChromaFormat::Yuv422 => "4:2:2",
        filmcraft_mpeg2v::ChromaFormat::Yuv444 => "4:4:4",
    };
    let scan = match field_order {
        Some(filmcraft_mpeg2v::FieldOrder::TopFirst) => ", interlaced (upper field first)",
        Some(filmcraft_mpeg2v::FieldOrder::BottomFirst) => ", interlaced (lower field first)",
        None => ", progressive",
    };
    format!("YUV {sub} 8-bit{scan}")
}

impl VideoDecoder for Mpeg2Decoder {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        let pics = self.dec.decode(sample, pts).map_err(|e| CodecError::Decode(e.to_string()))?;
        Ok(pics.into_iter().map(|p| DecodedFrame { pts: p.pts, frame: mpeg2_to_video_frame(p), draft: false }).collect())
    }
    fn flush(&mut self) -> Vec<DecodedFrame> {
        self.dec.flush().into_iter().map(|p| DecodedFrame { pts: p.pts, frame: mpeg2_to_video_frame(p), draft: false }).collect()
    }
    fn reset(&mut self) {
        self.dec.reset();
        self.prime();
    }
    fn name(&self) -> &str {
        "FilmCraft MPEG-2"
    }
    fn is_random_access(&self, sample: &[u8]) -> Option<bool> {
        Some(filmcraft_mpeg2v::scan_access_unit(sample).is_intra())
    }
    fn is_disposable(&self, sample: &[u8]) -> bool {
        filmcraft_mpeg2v::scan_access_unit(sample).is_disposable()
    }
}

/// QuickTime / MP4 sample entries carrying MPEG-1/2 video: XDCAM (EX/HD/HD422), IMX, HDV,
/// generic `mp2v`/`mpg2`, and `mp4v` whose `esds` object type is MPEG-1/2 video (0x60-0x65, 0x6A).
fn is_mpeg2_entry(fourcc: &[u8; 4], raw: &[u8]) -> bool {
    match fourcc {
        b"mp2v" | b"mpg2" | b"m2v1" | b"mpg1" | b"m1v " | b"m1v1" | b"mx3n" | b"mx4n" | b"mx5n" | b"mx3p" | b"mx4p" | b"mx5p" | b"xdhd" | b"xdh2" => true,
        [b'x', b'd', b'v' | b'5', _] | [b'h', b'd', b'v', _] => true,
        b"mp4v" => esds_object_type(raw).is_some_and(|t| (0x60..=0x65).contains(&t) || t == 0x6A),
        _ => false,
    }
}

/// objectTypeIndication of the DecoderConfigDescriptor inside an `esds` box in `raw`.
fn esds_object_type(raw: &[u8]) -> Option<u8> {
    let at = raw.windows(4).position(|w| w == b"esds")? + 8;
    let mut p = at;
    let size = |p: &mut usize| -> Option<usize> {
        let mut n = 0usize;
        for _ in 0..4 {
            let b = *raw.get(*p)?;
            *p += 1;
            n = (n << 7) | (b & 0x7F) as usize;
            if b & 0x80 == 0 {
                break;
            }
        }
        Some(n)
    };
    if *raw.get(p)? != 0x03 {
        return None;
    }
    p += 1;
    size(&mut p)?;
    let flags = *raw.get(p + 2)?;
    p += 3;
    if flags & 0x80 != 0 {
        p += 2;
    }
    if flags & 0x40 != 0 {
        p += 1 + *raw.get(p)? as usize;
    }
    if flags & 0x20 != 0 {
        p += 2;
    }
    if *raw.get(p)? != 0x04 {
        return None;
    }
    p += 1;
    size(&mut p)?;
    raw.get(p).copied()
}

pub fn mpeg2_factory(e: &SampleEntry) -> Option<Result<Box<dyn VideoDecoder>>> {
    match &e.codec {
        CodecConfig::Unknown { fourcc, raw } if is_mpeg2_entry(&fourcc.0, raw) => {
            // MKV puts the sequence header in CodecPrivate (passed as `raw` for `mp2v`)
            let header = if fourcc.0 == *b"mp2v" { mpeg2_sequence_header(raw).unwrap_or_default() } else { Vec::new() };
            Some(Ok(Box::new(Mpeg2Decoder::new(header)) as Box<dyn VideoDecoder>))
        }
        _ => None,
    }
}

#[cfg(test)]
mod disposable_tests {
    use super::{h264_disposable, hevc_disposable};

    /// A 4-byte length-prefixed sample of the given NAL units.
    fn sample(nals: &[&[u8]]) -> Vec<u8> {
        let mut v = Vec::new();
        for n in nals {
            v.extend_from_slice(&(n.len() as u32).to_be_bytes());
            v.extend_from_slice(n);
        }
        v
    }

    #[test]
    fn h264_non_reference_access_units() {
        // nal_ref_idc 0 slice (type 1), with an SEI (type 6) before it
        assert!(h264_disposable(&sample(&[&[0x06, 5, 1], &[0x01, 0x9a, 0]]), 4));
        // reference P slice (nal_ref_idc 2), IDR, mixed reference / non-reference slices
        assert!(!h264_disposable(&sample(&[&[0x41, 0x9a]]), 4));
        assert!(!h264_disposable(&sample(&[&[0x65, 0x88]]), 4));
        assert!(!h264_disposable(&sample(&[&[0x01, 0x9a], &[0x21, 0x9a]]), 4));
        // no slice at all, truncated data
        assert!(!h264_disposable(&sample(&[&[0x06, 5]]), 4));
        assert!(!h264_disposable(&[0, 0, 0, 9, 1], 4));
    }

    #[test]
    fn hevc_sub_layer_non_reference_pictures_of_the_top_layer() {
        let nal = |t: u8, tid1: u8| [t << 1, tid1, 0xaf];
        // TRAIL_N (0) at TemporalId 0 with one temporal layer
        assert!(hevc_disposable(&sample(&[&nal(39, 1), &nal(0, 1)]), 4, Some(0)));
        // TRAIL_R (1), CRA (21), IDR (19) are referenced
        for t in [1u8, 19, 21] {
            assert!(!hevc_disposable(&sample(&[&nal(t, 1)]), 4, Some(0)), "type {t}");
        }
        // RASL_N (8) at the top layer of two; at a lower layer it may be referenced
        assert!(hevc_disposable(&sample(&[&nal(8, 2)]), 4, Some(1)));
        assert!(!hevc_disposable(&sample(&[&nal(0, 1)]), 4, Some(1)));
        // unknown layering: never
        assert!(!hevc_disposable(&sample(&[&nal(0, 1)]), 4, None));
    }
}
