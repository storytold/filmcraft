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
mod interop;
#[allow(unsafe_code)]
mod mft;
mod surface;

pub use interop::live_surfaces;
pub use surface::{MfSurface, disable as disable_zero_copy, enable as enable_zero_copy, enabled as zero_copy_enabled};

use std::sync::Arc;
use std::time::{Duration, Instant};

use filmcraft_codecs::hw::{NalCodec, NalStreamInfo};
use filmcraft_codecs::{CodecError, DecodedFrame, Result, VideoDecoder};
use filmcraft_frame::{Chroma, GpuPixels, PixelData, VideoFrame};
use windows::Win32::Graphics::Direct3D11::{D3D11_DECODER_PROFILE_H264_VLD_NOFGT, D3D11_DECODER_PROFILE_HEVC_VLD_MAIN, D3D11_DECODER_PROFILE_HEVC_VLD_MAIN10};
use windows::core::GUID;

use self::gpu::{Gpu, Readback, SurfaceFormat, sample_texture};
use self::interop::SharedSurface;
use self::mft::{MfApi, Mft, Poll};
use crate::annexb::to_annex_b;
use crate::biplanar::{self, Geometry};

/// Largest picture the backend takes (as the VideoToolbox one).
const MAX_SIDE: u32 = 8192;
/// Pictures the decoder may hold back (a DPB is at most 16 frames; more means it lost track).
const MAX_IN_FLIGHT: usize = 64;
/// Upper bound on the outputs taken per call: a misbehaving MFT cannot make a decode call loop.
const MAX_OUTPUTS_PER_CALL: usize = 4096;

/// Whether Media Foundation and a Direct3D 11 video device exist on this system.
pub fn available() -> bool {
    mft::api().and_then(Gpu::shared).is_ok()
}

/// What the decoder needs for a stream, or why it is declined.
fn plan(info: &NalStreamInfo) -> std::result::Result<(SurfaceFormat, GUID), String> {
    if info.interlaced {
        return Err("field-coded H.264".into());
    }
    if info.chroma_format_idc != 1 {
        return Err(format!("chroma_format_idc {}", info.chroma_format_idc));
    }
    if info.bit_depth_luma != info.bit_depth_chroma || !matches!(info.bit_depth_luma, 8 | 10) {
        return Err(format!("{}-bit luma / {}-bit chroma", info.bit_depth_luma, info.bit_depth_chroma));
    }
    let (_, _, w, h) = info.crop;
    let (cx, cy) = (info.crop.0, info.crop.1);
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE || cx.saturating_add(w) > info.coded.0 || cy.saturating_add(h) > info.coded.1 {
        return Err(format!("picture size {w}x{h}"));
    }
    let ten = info.bit_depth_luma == 10;
    match (info.codec, info.profile_idc, ten) {
        // Baseline, Main and High (the profiles DXVA H.264 decoders list)
        (NalCodec::H264, 66 | 77 | 100, false) => Ok((SurfaceFormat::Nv12, D3D11_DECODER_PROFILE_H264_VLD_NOFGT)),
        (NalCodec::H264, p, _) => Err(format!("H.264 profile {p} at {} bits", info.bit_depth_luma)),
        // Main and Main Still Picture
        (NalCodec::Hevc, 1 | 3, false) => Ok((SurfaceFormat::Nv12, D3D11_DECODER_PROFILE_HEVC_VLD_MAIN)),
        (NalCodec::Hevc, 2, true) => Ok((SurfaceFormat::P010, D3D11_DECODER_PROFILE_HEVC_VLD_MAIN10)),
        (NalCodec::Hevc, p, _) => Err(format!("HEVC profile {p} at {} bits", info.bit_depth_luma)),
    }
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
    info: NalStreamInfo,
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
    /// The picture rectangle is even in every coordinate, as sharing a 4:2:0 surface needs.
    can_share: bool,
    /// [`VideoDecoder::flush`] drained the MFT, which then behaves as at the end of a stream: it
    /// only restarts at an IDR picture, the references of the run are gone.
    drained: bool,
    /// Test hook: fail every decode once this many samples were fed.
    fail_after: Option<u64>,
    fed: u64,
    name: &'static str,
    timings: Timings,
}

impl MfDecoder {
    /// A decoder for the stream, or why Media Foundation does not take it (unsupported format, no
    /// DXVA decoder for it on the GPU, no Direct3D-aware decoder MFT).
    pub fn new(info: NalStreamInfo) -> std::result::Result<Self, String> {
        let (format, profile) = plan(&info)?;
        let api = mft::api()?;
        let gpu = Gpu::shared(api)?;
        gpu.supports(profile, format, info.coded)?;
        let (cx, cy, w, h) = info.crop;
        let info_even = [cx, cy, w, h].iter().all(|v| v % 2 == 0);
        let mft = Mft::new(api, &gpu, info.codec, (w, h), format)?;
        log::info!("hardware decoding: {} on {}", mft.name(), gpu.name());
        let name = match info.codec {
            NalCodec::H264 => "Media Foundation H.264",
            NalCodec::Hevc => "Media Foundation HEVC",
        };
        Ok(Self {
            geometry: Geometry { crop: info.crop, bits: format.bits(), color: info.color, par: info.par },
            info,
            format,
            api,
            gpu,
            mft: Some(mft),
            readback: Readback::default(),
            in_flight: Vec::new(),
            need_headers: true,
            skip_rasl: false,
            first: true,
            can_share: info_even,
            drained: false,
            fail_after: None,
            fed: 0,
            name,
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

    /// Whether `sample` holds an IDR picture (H.264 IDR slice; HEVC IDR_W_RADL / IDR_N_LP).
    fn is_idr(&self, sample: &[u8]) -> bool {
        self.info.nal_types(sample).iter().any(|&t| match self.info.codec {
            NalCodec::H264 => t == 5,
            NalCodec::Hevc => matches!(t, 19 | 20),
        })
    }

    /// The picture of `sample` as a GPU surface the compositor can open: a GPU to GPU copy of the
    /// cropped rectangle into a shareable texture (no CPU readback).
    fn share(&self, sample: &windows::Win32::Media::MediaFoundation::IMFSample) -> std::result::Result<VideoFrame, String> {
        let (cx, cy, w, h) = self.geometry.crop;
        let (tex, index) = sample_texture(sample)?;
        let shared = Arc::new(SharedSurface::create(&self.gpu, self.format, (w, h))?);
        self.gpu.copy_picture(shared.texture(), &tex, index, (cx, cy, w, h))?;
        let surface = Arc::new(MfSurface::new(self.gpu.clone(), shared, self.geometry));
        let data = PixelData::Gpu(GpuPixels::new(surface, Chroma::C420, self.format.bits()));
        Ok(VideoFrame { width: w, height: h, data, color: self.geometry.color, par: self.geometry.par, pts: filmcraft_time::Tick::ZERO })
    }

    /// Take every picture the decoder has ready (presentation order).
    fn collect(&mut self, out: &mut Vec<DecodedFrame>) -> std::result::Result<(), String> {
        let first_new = out.len();
        let r = self.collect_inner(out);
        // shared pictures were copied on the GPU's queue: wait for the copies before anyone else
        // (another device) reads them
        if out.get(first_new..).is_some_and(|f| f.iter().any(|d| matches!(d.frame.data, PixelData::Gpu(_)))) {
            self.gpu.wait_for_gpu()?;
        }
        r
    }

    fn collect_inner(&mut self, out: &mut Vec<DecodedFrame>) -> std::result::Result<(), String> {
        for _ in 0..MAX_OUTPUTS_PER_CALL {
            let Some(mft) = self.mft.as_ref() else { return Ok(()) };
            match mft.poll()? {
                Poll::NeedInput => return Ok(()),
                Poll::FormatChanged => {}
                Poll::Frame(sample, t) if self.can_share && zero_copy_enabled() => {
                    match self.share(&sample) {
                        Ok(frame) => {
                            filmcraft_codecs::hw::note_hw_zero_copy(1);
                            let pts = self.take_pts(t);
                            out.push(DecodedFrame { pts, frame, draft: false });
                        }
                        Err(e) => {
                            // (device lost, out of memory...) the stream goes on through the readback
                            log::warn!("{}: zero-copy failed ({e}); reading pictures back instead", self.name);
                            self.can_share = false;
                            let pts = self.take_pts(t);
                            let frame = self.readback.read(&self.gpu, &sample, self.format, |b| biplanar::to_frame(b, &self.geometry))?;
                            out.push(DecodedFrame { pts, frame, draft: false });
                        }
                    }
                }
                Poll::Frame(sample, t) => {
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

    fn feed(&mut self, annex_b: &[u8], pts: i64) -> std::result::Result<Vec<DecodedFrame>, String> {
        let mut out = Vec::new();
        for attempt in 0..2 {
            let Some(mft) = self.mft.as_ref() else { return Err("no decoder".into()) };
            if mft.input(self.api, annex_b, pts)? {
                self.in_flight.push(pts);
                self.need_headers = false;
                self.collect(&mut out)?;
                // a decoder holds back a DPB's worth of pictures at most
                if self.in_flight.len() > MAX_IN_FLIGHT {
                    return Err(format!("the decoder holds {} pictures back", self.in_flight.len()));
                }
                return Ok(out);
            }
            // full: take what it has ready, then offer the sample again
            self.collect(&mut out)?;
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
            if !self.is_idr(sample) {
                return Err(CodecError::Decode("the hardware decoder was drained and restarts only at an IDR picture".into()));
            }
            self.drained = false;
            self.first = true;
        }
        if self.info.codec == NalCodec::Hevc {
            let types = self.info.nal_types(sample);
            if types.iter().any(|t| (16..=23).contains(t)) {
                // CRA / BLA starting a run: its RASL pictures reference pictures we never decoded.
                self.skip_rasl = self.first && types.iter().any(|t| (16..=21).contains(t) && !(19..=20).contains(t));
            } else if self.skip_rasl && types.iter().any(|t| matches!(t, 8 | 9)) {
                self.first = false;
                return Ok(Vec::new());
            }
        }
        self.first = false;
        let annex_b = to_annex_b(&self.info, sample, self.need_headers).map_err(CodecError::Decode)?;
        if self.mft.is_none() {
            let (_, _, w, h) = self.info.crop;
            let mft = Mft::new(self.api, &self.gpu, self.info.codec, (w, h), self.format).map_err(CodecError::Decode)?;
            self.mft = Some(mft);
            self.need_headers = true;
        }
        let (t0, spent) = (Instant::now(), self.timings.readback + self.timings.convert);
        let r = self.feed(&annex_b, pts).map_err(|e| {
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
        let drained = self.mft.as_ref().map(Mft::drain).transpose().and_then(|_| self.collect(&mut out));
        if let Err(e) = drained {
            log::warn!("{}: {e} while flushing", self.name);
        }
        self.in_flight.clear();
        // what follows a drain starts like a new run: the decoder is given the headers again
        self.need_headers = true;
        self.drained = true;
        out
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

impl Drop for MfDecoder {
    fn drop(&mut self) {
        // the decoder MFT is released on whichever thread drops the decoder: it needs COM
        let _ = gpu::ensure_com();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(codec: NalCodec, profile_idc: u8, bits: u32, chroma_format_idc: u32) -> NalStreamInfo {
        NalStreamInfo {
            codec,
            length_size: 4,
            highest_tid: None,
            parameter_sets: Vec::new(),
            coded: (1920, 1088),
            crop: (0, 0, 1920, 1080),
            chroma_format_idc,
            bit_depth_luma: bits,
            bit_depth_chroma: bits,
            interlaced: false,
            profile_idc,
            color: filmcraft_color::ColorInfo::REC709,
            par: (1, 1),
            reorder: 2,
        }
    }

    #[test]
    fn takes_h264_baseline_main_high_and_hevc_main_main10() {
        for p in [66, 77, 100] {
            assert_eq!(plan(&info(NalCodec::H264, p, 8, 1)).unwrap().0, SurfaceFormat::Nv12, "H.264 profile {p}");
        }
        assert_eq!(plan(&info(NalCodec::Hevc, 1, 8, 1)).unwrap(), (SurfaceFormat::Nv12, D3D11_DECODER_PROFILE_HEVC_VLD_MAIN));
        assert_eq!(plan(&info(NalCodec::Hevc, 2, 10, 1)).unwrap(), (SurfaceFormat::P010, D3D11_DECODER_PROFILE_HEVC_VLD_MAIN10));
    }

    #[test]
    fn declines_what_dxva_does_not_decode() {
        // Hi10, High 4:2:2 / 4:4:4, Extended; 10-bit under an 8-bit profile; 4:2:2 / 4:4:4 / mono HEVC
        for (codec, p, bits, chroma) in [
            (NalCodec::H264, 110, 10, 1),
            (NalCodec::H264, 122, 10, 2),
            (NalCodec::H264, 244, 8, 3),
            (NalCodec::H264, 88, 8, 1),
            (NalCodec::H264, 100, 10, 1),
            (NalCodec::H264, 100, 8, 2),
            (NalCodec::Hevc, 4, 10, 2),
            (NalCodec::Hevc, 4, 12, 1),
            (NalCodec::Hevc, 1, 10, 1),
            (NalCodec::Hevc, 2, 8, 1),
            (NalCodec::Hevc, 1, 8, 0),
            (NalCodec::Hevc, 1, 8, 3),
        ] {
            assert!(plan(&info(codec, p, bits, chroma)).is_err(), "{codec:?} profile {p} {bits}-bit chroma {chroma}");
        }
        let mut field_coded = info(NalCodec::H264, 100, 8, 1);
        field_coded.interlaced = true;
        assert!(plan(&field_coded).is_err());
        // luma and chroma depths that differ
        let mut mixed = info(NalCodec::Hevc, 2, 10, 1);
        mixed.bit_depth_chroma = 8;
        assert!(plan(&mixed).is_err());
    }

    #[test]
    fn declines_absurd_pictures() {
        for crop in [(0, 0, 0, 1080), (0, 0, 1920, 0), (0, 0, 9000, 1080), (0, 0, 1080, 9000), (8, 0, 1920, 1080), (0, 16, 1920, 1080), (u32::MAX, 0, 16, 16)] {
            let mut i = info(NalCodec::H264, 100, 8, 1);
            i.crop = crop;
            assert!(plan(&i).is_err(), "{crop:?}");
        }
    }
}
