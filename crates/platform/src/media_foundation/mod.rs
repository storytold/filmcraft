//! Media Foundation / Direct3D 11 (Windows) hardware H.264 / HEVC decoding.
//!
//! The decoder is a Direct3D-aware decoder MFT (Microsoft's H.264 decoder, the HEVC Video
//! Extensions' decoder, or a vendor's synchronous hardware MFT) driven at the level of single
//! access units: the container demuxer FilmCraft already has supplies the samples, so there is no
//! Source Reader and no second media pipeline. The MFT is given the process's Direct3D 11 video
//! device through an `IMFDXGIDeviceManager`, which makes it decode with DXVA on the GPU's video
//! engine and return NV12 (8-bit) / P010 (10-bit) surfaces as Direct3D 11 textures.
//!
//! ```text
//! avcC / hvcC sample ──► Annex B ──► decoder MFT ──DXVA──► NV12 / P010 texture
//!                                                              │ copy to a staging texture (GPU → CPU)
//!                                                              ▼
//!                                          planar Yuv8 / Yuv16 VideoFrame ──► HybridDecoder
//! ```
//!
//! "Hardware" is verified, not assumed: a decoder MFT that is not Direct3D-aware, or that hands
//! back system-memory samples (Microsoft's decoders do that when DXVA is unavailable), is
//! declined or fails the stream, so [`crate::HybridDecoder`] continues with FilmCraft's own
//! decoder and Windows' software decoding is never used in its place. Streams the GPU's DXVA
//! decoder does not list (profile, bit depth, chroma format, size) are declined up front.
//!
//! The readback (`gpu::Readback`, [`crate::biplanar`]) is the one GPU to CPU copy; a later
//! zero-copy path would hand the texture to the renderer instead and keep everything else.
//!
//! The modules that call into Windows (`gpu`, `mft`) are the FFI modules of this backend
//! (docs/adr/0001-platform-ffi.md); this one is safe code.

#[allow(unsafe_code)]
mod gpu;
#[allow(unsafe_code)]
mod mft;
mod stream;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use filmcraft_codecs::hw::{NalCodec, StreamInfo};
use filmcraft_codecs::{CodecError, DecodedFrame, Result, VideoDecoder};

use self::gpu::{Gpu, Readback, SurfaceFormat};
use self::mft::{MfApi, Mft, Poll};
use self::stream::Stream;
use crate::biplanar::{self, Geometry};

/// Pictures the decoder may hold back (a DPB is at most 16 frames; more means it lost track).
const MAX_IN_FLIGHT: usize = 64;
/// Upper bound on the outputs taken per call: a misbehaving MFT cannot make a decode call loop.
const MAX_OUTPUTS_PER_CALL: usize = 4096;

/// Pictures a VP9 / AV1 decoder MFT accepted and never returned, in this process.
static DROPPED: AtomicU64 = AtomicU64::new(0);

/// How many pictures decoder MFTs dropped in this process (diagnostics and tests). Microsoft's VP9
/// decoder occasionally skips a picture without an error when the GPU is busy; the decoder notices
/// it and fails, so the hybrid decoder replays the run in software.
pub fn dropped_pictures() -> u64 {
    DROPPED.load(Ordering::Relaxed)
}

/// The stream description of a sample entry this backend could take (`avcC` / `hvcC` / `vpcC` /
/// `av1C`), or `None` for other codecs and unreadable configuration records.
pub fn stream_info(entry: &filmcraft_isobmff::SampleEntry) -> Option<StreamInfo> {
    use filmcraft_codecs::hw::{FrameStreamInfo, NalStreamInfo};
    match NalStreamInfo::from_entry(entry) {
        Some(r) => r.ok().map(StreamInfo::Nal),
        None => FrameStreamInfo::from_entry(entry)?.ok().map(StreamInfo::Frame),
    }
}

/// Whether Media Foundation and a Direct3D 11 video device exist on this system.
pub fn available() -> bool {
    mft::api().and_then(Gpu::shared).is_ok()
}

/// What each codec's decoders and the GPU's DXVA profiles offer on this machine (diagnostics:
/// `examples/mfcaps.rs`).
pub fn capabilities() -> String {
    use windows::Win32::Graphics::Direct3D11 as d3d;
    use windows::Win32::Media::MediaFoundation as mf;
    let (api, gpu) = match mft::api().and_then(|a| Gpu::shared(a).map(|g| (a, g))) {
        Ok(x) => x,
        Err(e) => return format!("no Media Foundation / Direct3D 11 video device: {e}"),
    };
    let mut out = format!("adapter: {}\n", gpu.name());
    for (name, subtype) in
        [("H.264", mf::MFVideoFormat_H264), ("HEVC", mf::MFVideoFormat_HEVC), ("VP9", mf::MFVideoFormat_VP90), ("AV1", mf::MFVideoFormat_AV1)]
    {
        let list = mft::describe(api, subtype);
        out += &format!("{name} decoder MFTs: {}\n", if list.is_empty() { "none".into() } else { list.join("; ") });
    }
    let profiles = gpu.profiles();
    for (name, guid) in [
        ("H.264 VLD", d3d::D3D11_DECODER_PROFILE_H264_VLD_NOFGT),
        ("HEVC Main", d3d::D3D11_DECODER_PROFILE_HEVC_VLD_MAIN),
        ("HEVC Main 10", d3d::D3D11_DECODER_PROFILE_HEVC_VLD_MAIN10),
        ("VP9 profile 0", d3d::D3D11_DECODER_PROFILE_VP9_VLD_PROFILE0),
        ("VP9 profile 2 (10-bit)", d3d::D3D11_DECODER_PROFILE_VP9_VLD_10BIT_PROFILE2),
        ("AV1 profile 0", d3d::D3D11_DECODER_PROFILE_AV1_VLD_PROFILE0),
        ("AV1 profile 1", d3d::D3D11_DECODER_PROFILE_AV1_VLD_PROFILE1),
        ("AV1 profile 2", d3d::D3D11_DECODER_PROFILE_AV1_VLD_PROFILE2),
    ] {
        out += &format!("DXVA {name}: {}\n", profiles.contains(&guid));
    }
    out
}

/// Where a decoder's time went (wall clock), for diagnostics (`examples/mfprobe.rs --time`).
#[derive(Clone, Copy, Debug, Default)]
pub struct Timings {
    /// Pictures returned.
    pub frames: u64,
    /// Feeding the MFT and waiting for its pictures (the DXVA decode itself).
    pub decode: Duration,
    /// GPU to CPU: copy of the picture to the staging texture and its mapping.
    pub readback: Duration,
    /// CPU: staging memory to planar `Yuv8` / `Yuv16`.
    pub convert: Duration,
}

/// The Media Foundation decoder for one `avcC` / `hvcC` stream.
pub struct MfDecoder {
    stream: Stream,
    /// The stream as the hybrid decoder's tests (random access, disposable) see it.
    info: StreamInfo,
    format: SurfaceFormat,
    geometry: Geometry,
    api: &'static MfApi,
    gpu: Arc<Gpu>,
    /// `None` only after a failed reset: made again on the next sample.
    mft: Option<Mft>,
    readback: Readback,
    /// Presentation times of submitted access units whose picture has not come out yet.
    in_flight: Vec<i64>,
    /// The next sample must carry the parameter sets (first of a run, after a reset or flush).
    need_headers: bool,
    /// The run started at a CRA / BLA picture: its RASL pictures are not decodable and are left
    /// out (as the software decoder does).
    skip_rasl: bool,
    first: bool,
    /// The size the MFT declares for its pictures was checked against the stream's (VP9 / AV1: the
    /// picture size is in the bitstream, and a decoder that disagrees would show padding or crop).
    size_checked: bool,
    /// [`VideoDecoder::flush`] drained the MFT, which then behaves as at the end of a stream: it
    /// only restarts at an IDR picture, the references of the run are gone.
    drained: bool,
    /// Test hook: fail every decode once this many samples were fed.
    fail_after: Option<u64>,
    fed: u64,
    /// Pictures the last flush lost ([`VideoDecoder::take_flush_losses`]).
    flush_losses: usize,
    name: &'static str,
    timings: Timings,
}

impl MfDecoder {
    /// A decoder for the stream, or why Media Foundation does not take it (unsupported format, no
    /// DXVA decoder for it on the GPU, no Direct3D-aware decoder MFT).
    pub fn new(stream: impl Into<Stream>) -> std::result::Result<Self, String> {
        let stream = stream.into();
        let plan = stream.plan()?;
        let api = mft::api()?;
        let gpu = Gpu::shared(api)?;
        gpu.supports(plan.profile, plan.format, plan.size)?;
        let format = plan.format;
        let mft = Mft::new(api, &gpu, &stream.spec(format))?;
        log::info!("hardware decoding: {} on {}", mft.name(), gpu.name());
        let geometry = stream.first_geometry(format);
        Ok(Self {
            name: stream.name(),
            info: stream.info(),
            geometry,
            stream,
            format,
            api,
            gpu,
            mft: Some(mft),
            readback: Readback::default(),
            in_flight: Vec::new(),
            need_headers: true,
            skip_rasl: false,
            first: true,
            size_checked: false,
            drained: false,
            fail_after: None,
            fed: 0,
            flush_losses: 0,
            timings: Timings::default(),
        })
    }

    /// Test hook: from the `n`-th sample on, every decode fails as a lost device would.
    pub fn fail_after(&mut self, n: u64) {
        self.fail_after = Some(n);
    }

    /// Where the time went so far.
    pub fn timings(&self) -> Timings {
        self.timings
    }

    /// Name of the decoder MFT in use (diagnostics and tests).
    pub fn mft_name(&self) -> &str {
        self.mft.as_ref().map_or("", Mft::name)
    }

    /// The submitted presentation time an output stamped `t` belongs to.
    fn take_pts(&mut self, t: i64) -> i64 {
        if let Some(i) = self.in_flight.iter().position(|&p| p == t) {
            return self.in_flight.swap_remove(i);
        }
        // the MFT restamped it: pictures come out in presentation order, so the earliest
        match self.in_flight.iter().enumerate().min_by_key(|(_, p)| **p).map(|(i, _)| i) {
            Some(i) => self.in_flight.swap_remove(i),
            None => t,
        }
    }

    /// A VP9 / AV1 picture still in flight that was submitted before the one stamped `t`: frame-coded
    /// streams come out in the order they go in, so the decoder dropped it.
    fn skipped_before(&self, t: i64) -> Option<i64> {
        if !matches!(self.stream, Stream::Frame(_)) {
            return None;
        }
        overtaken(&self.in_flight, t)
    }

    /// A VP9 / AV1 decoder must output the picture size the bitstream declares (H.264 / HEVC
    /// decoders report coded or cropped sizes, which the cropping handles).
    fn check_size(&mut self) -> std::result::Result<(), String> {
        self.size_checked = true;
        let (_, _, w, h) = self.geometry.crop;
        match (&self.stream, self.mft.as_ref().and_then(Mft::output_size)) {
            (Stream::Frame(_), Some(got)) if got != (w, h) => Err(format!("the decoder outputs {}x{} pictures, the stream says {w}x{h}", got.0, got.1)),
            _ => Ok(()),
        }
    }

    /// Take every picture the decoder has ready (presentation order). A picture overtaken by a
    /// later one was dropped: while decoding that is an error (the hybrid decoder replays the run in
    /// software); while `draining`, the pictures still held are taken and the loss is counted in
    /// `flush_losses`, which the hybrid decoder asks for after the flush.
    fn collect(&mut self, out: &mut Vec<DecodedFrame>, draining: bool) -> std::result::Result<(), String> {
        for _ in 0..MAX_OUTPUTS_PER_CALL {
            let Some(mft) = self.mft.as_ref() else { return Ok(()) };
            match mft.poll()? {
                Poll::NeedInput => return Ok(()),
                Poll::FormatChanged => self.size_checked = false,
                Poll::Frame(sample, t) => {
                    if let Some(skipped) = self.skipped_before(t) {
                        let before = self.in_flight.len();
                        if draining {
                            self.in_flight.retain(|&p| p >= t);
                            let lost = before.saturating_sub(self.in_flight.len());
                            DROPPED.fetch_add(lost as u64, Ordering::Relaxed);
                            self.flush_losses = self.flush_losses.saturating_add(lost);
                        } else {
                            // forget the dropped pictures and this one (not returned either), so
                            // a flush after the error does not count them again
                            self.in_flight.retain(|&p| p > t);
                            DROPPED.fetch_add(before.saturating_sub(self.in_flight.len() + 1) as u64, Ordering::Relaxed);
                            return Err(format!("the decoder dropped the picture at {skipped}"));
                        }
                    }
                    if !self.size_checked {
                        self.check_size()?;
                    }
                    let geometry = self.geometry;
                    let (t0, mut convert) = (Instant::now(), Duration::ZERO);
                    let frame = self.readback.read(&self.gpu, &sample, self.format, |b| {
                        let c0 = Instant::now();
                        let f = biplanar::to_frame(b, &geometry);
                        convert = c0.elapsed();
                        f
                    })?;
                    self.timings.frames += 1;
                    self.timings.convert += convert;
                    self.timings.readback += t0.elapsed().saturating_sub(convert);
                    let pts = self.take_pts(t);
                    out.push(DecodedFrame { pts, frame, draft: false });
                }
            }
        }
        Err("the decoder keeps producing output".into())
    }

    /// Feed one sample; `shows` is false for a sample that decodes to no picture (a VP9 hidden frame).
    fn feed(&mut self, annex_b: &[u8], pts: i64, shows: bool) -> std::result::Result<Vec<DecodedFrame>, String> {
        let mut out = Vec::new();
        for attempt in 0..2 {
            let Some(mft) = self.mft.as_ref() else { return Err("no decoder".into()) };
            if mft.input(self.api, annex_b, pts)? {
                if shows {
                    self.in_flight.push(pts);
                }
                self.need_headers = false;
                self.collect(&mut out, false)?;
                // a decoder holds back a DPB's worth of pictures at most
                if self.in_flight.len() > MAX_IN_FLIGHT {
                    return Err(format!("the decoder holds {} pictures back", self.in_flight.len()));
                }
                return Ok(out);
            }
            // full: take what it has ready, then offer the sample again
            self.collect(&mut out, false)?;
            if attempt == 1 {
                break;
            }
        }
        Err("the decoder does not accept input".into())
    }
}

impl VideoDecoder for MfDecoder {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        self.fed += 1;
        if self.fail_after.is_some_and(|n| self.fed > n) {
            return Err(CodecError::Decode("Media Foundation decoder failed (test hook)".into()));
        }
        gpu::ensure_com().map_err(CodecError::Decode)?;
        if self.drained {
            // A drained MFT cannot go on from the middle of a GOP (software decoders can). The
            // GOP cache seeks (`reset`) after every flush, so this is an error only for callers
            // that continue after one: the hybrid decoder then replays the run in software.
            if !self.stream.is_restart(sample) {
                return Err(CodecError::Decode("the hardware decoder was drained and restarts only at an IDR picture".into()));
            }
            self.drained = false;
            self.first = true;
        }
        if let Stream::Nal(info) = &self.stream
            && info.codec == NalCodec::Hevc
        {
            let types = info.nal_types(sample);
            if types.iter().any(|t| (16..=23).contains(t)) {
                // CRA / BLA starting a run: its RASL pictures reference pictures we never decoded.
                self.skip_rasl = self.first && types.iter().any(|t| (16..=21).contains(t) && !(19..=20).contains(t));
            } else if self.skip_rasl && types.iter().any(|t| matches!(t, 8 | 9)) {
                self.first = false;
                return Ok(Vec::new());
            }
        }
        self.first = false;
        // a VP9 key frame says the picture's size and colour (they are not in the container)
        if let Some(g) = self.stream.declared_picture(sample) {
            self.geometry = g;
        }
        let shows = match &self.stream {
            Stream::Frame(info) => info.shows_picture(sample),
            Stream::Nal(_) => true,
        };
        let annex_b = self.stream.input(sample, self.need_headers).map_err(CodecError::Decode)?;
        if self.mft.is_none() {
            let mft = Mft::new(self.api, &self.gpu, &self.stream.spec(self.format)).map_err(CodecError::Decode)?;
            self.mft = Some(mft);
            self.size_checked = false;
            self.need_headers = true;
        }
        let (t0, spent) = (Instant::now(), self.timings.readback + self.timings.convert);
        let r = self.feed(&annex_b, pts, shows).map_err(|e| {
            self.gpu.note_failure();
            CodecError::Decode(e)
        });
        let copying = (self.timings.readback + self.timings.convert).saturating_sub(spent);
        self.timings.decode += t0.elapsed().saturating_sub(copying);
        r
    }

    fn flush(&mut self) -> Vec<DecodedFrame> {
        let mut out = Vec::new();
        if gpu::ensure_com().is_err() {
            return out;
        }
        self.flush_losses = 0;
        let drained = self.mft.as_ref().map(Mft::drain).transpose().and_then(|_| self.collect(&mut out, true));
        if let Err(e) = drained {
            log::warn!("{}: {e} while flushing", self.name);
        } else if matches!(self.stream, Stream::Frame(_)) && !self.in_flight.is_empty() {
            // drained, yet some pictures never came out
            DROPPED.fetch_add(self.in_flight.len() as u64, Ordering::Relaxed);
            self.flush_losses = self.flush_losses.saturating_add(self.in_flight.len());
        }
        if self.flush_losses > 0 {
            log::warn!("{}: the decoder dropped {} pictures while flushing", self.name, self.flush_losses);
        }
        self.in_flight.clear();
        // what follows a drain starts like a new run: the decoder is given the headers again
        self.need_headers = true;
        self.drained = true;
        out
    }

    fn take_flush_losses(&mut self) -> usize {
        std::mem::take(&mut self.flush_losses)
    }

    fn reset(&mut self) {
        // after a seek no reference picture survives
        if gpu::ensure_com().is_ok() && self.mft.as_ref().map(Mft::flush).transpose().is_err() {
            self.mft = None;
        }
        self.in_flight.clear();
        self.need_headers = true;
        self.skip_rasl = false;
        self.first = true;
        self.size_checked = false;
        self.drained = false;
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

/// The earliest of `in_flight` that a picture stamped `t` came out ahead of, when `t` is one of them
/// (an output the decoder restamped says nothing about order).
fn overtaken(in_flight: &[i64], t: i64) -> Option<i64> {
    if !in_flight.contains(&t) {
        return None;
    }
    in_flight.iter().copied().filter(|&p| p < t).min()
}

impl Drop for MfDecoder {
    fn drop(&mut self) {
        // the decoder MFT is released on whichever thread drops the decoder: it needs COM
        let _ = gpu::ensure_com();
    }
}

#[cfg(test)]
mod tests {
    use super::overtaken;

    #[test]
    fn a_picture_overtaken_by_a_later_one_was_dropped() {
        assert_eq!(overtaken(&[512], 512), None, "in order");
        assert_eq!(overtaken(&[512, 1024], 512), None, "the next one is still coming");
        // the trace of a real drop: 21504 accepted, never returned, 22016 came out
        assert_eq!(overtaken(&[21504, 22016], 22016), Some(21504));
        assert_eq!(overtaken(&[1536, 512, 1024], 1536), Some(512), "the earliest");
        assert_eq!(overtaken(&[512, 1024], 700), None, "restamped: no evidence");
        assert_eq!(overtaken(&[], 0), None);
    }
}
