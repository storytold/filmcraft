//! Smart rendering of intra-only exports: a frame that shows one source frame untouched, from a
//! ProRes or DNxHR source in the flavour the export encodes, is written as the source's compressed
//! sample, unchanged, instead of being decoded, rendered and encoded again.
//!
//! The encoder adapters that take such copies say so with
//! [`VideoEncoder::sample_copy`](crate::VideoEncoder::sample_copy): a [`SampleCopy`] describing
//! the stream they write (codec flavour, size, scan, alpha, colour, pixel aspect, frame rate)
//! checks each candidate sample against it ([`SampleCopy::check`]). The exporter finds the
//! candidates without decoding anything (`Pipeline::copied_sample`): the export draws nothing on
//! the frame, the timeline shows one untouched source frame
//! ([`filmcraft_render::plan::untouched_clip`]), the clip plays
//! forward at 100 % from a source at the output frame rate, and the source hands out the sample
//! its decoder would decode ([`filmcraft_media::MediaSource::compressed_video_sample`]). Every
//! other frame (and any sample that fails a check, or can't be read) is rendered and encoded as
//! usual, in the same track: the encoders are intra-only, so the two kinds of packets mix freely.

use std::sync::atomic::{AtomicU64, Ordering};

use filmcraft_media::{CompressedSample, IntraCodec};
use filmcraft_time::FrameRate;

use crate::{ColorSignal, EncodedPacket};

/// The codec flavour an encoder writes, for [`SampleCopy`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyFlavour {
    /// ProRes of `profile`, with an alpha channel coded or not.
    ProRes { profile: filmcraft_prores::Profile, alpha: bool },
    /// DNxHR of `profile` at `bit_depth`, 4:2:2 without alpha.
    DnxHr { profile: filmcraft_dnx::Profile, bit_depth: u8 },
}

/// What an encoder that takes copied samples writes: a sample that matches it in every respect
/// a player sees can stand for one of its packets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SampleCopy {
    pub flavour: CopyFlavour,
    pub width: u32,
    pub height: u32,
    /// Output frame rate (the encoder's timescale is `rate.num`, a frame lasts `rate.den`).
    pub rate: FrameRate,
    /// The colour the stream is labelled with.
    pub signal: ColorSignal,
}

/// Unspecified colour code points (0 in ProRes, 2 in ITU-T H.273).
fn unspecified(code: u8) -> bool {
    matches!(code, 0 | 2)
}

/// The frame rate a ProRes `frame_rate_code` names (RDD 36), None for 0 (unknown) and reserved codes.
fn prores_rate(code: u8) -> Option<(i64, i64)> {
    Some(match code {
        1 => (24_000, 1001),
        2 => (24, 1),
        3 => (25, 1),
        4 => (30_000, 1001),
        5 => (30, 1),
        6 => (50, 1),
        7 => (60_000, 1001),
        8 => (60, 1),
        9 => (100, 1),
        10 => (120_000, 1001),
        11 => (120, 1),
        _ => return None,
    })
}

/// `a` and `b` are the same rate (as rationals; never divides).
pub(crate) fn same_rate(a: FrameRate, b: FrameRate) -> bool {
    a.num > 0 && a.den > 0 && i128::from(a.num) * i128::from(b.den) == i128::from(b.num) * i128::from(a.den)
}

impl SampleCopy {
    /// The packet a copied sample becomes: a sync sample of one frame at the encoder's timescale
    /// with no composition offset, like the encoder's own packets.
    pub fn packet(&self, data: Vec<u8>) -> EncodedPacket {
        EncodedPacket { data, key: true, duration: u32::try_from(self.rate.den).unwrap_or(1), composition_offset: 0 }
    }

    /// Whether `s` can be written unchanged as one of this encoder's packets: the same codec
    /// flavour, coded size, progressive scan, alpha, colour and square pixels, read from the
    /// container and checked in the frame header, whose sizes must also agree with the sample's.
    /// The colour the source decodes the sample with ([`CompressedSample::color`]) must be BT.709,
    /// so a copy shows what rendering the frame would; the frame header's own codes must also
    /// say BT.709 or nothing, for players that read them. Err says why not (for the debug log).
    pub fn check(&self, s: &CompressedSample) -> Result<(), String> {
        if self.signal != ColorSignal::default() {
            return Err("only BT.709 SDR exports take copies".into());
        }
        if (s.width, s.height) != (self.width, self.height) {
            return Err(format!("{}x{} source, {}x{} export", s.width, s.height, self.width, self.height));
        }
        if s.par.0 != s.par.1 || s.par.0 == 0 {
            return Err(format!("pixel aspect {}:{}", s.par.0, s.par.1));
        }
        if s.color != Some(filmcraft_color::ColorInfo::REC709) {
            return Err(format!("decoded colour {:?}", s.color));
        }
        match self.flavour {
            CopyFlavour::ProRes { profile, alpha } => self.check_prores(s, profile, alpha),
            CopyFlavour::DnxHr { profile, bit_depth } => self.check_dnx(s, profile, bit_depth),
        }
    }

    /// [`SampleCopy::check`] for a ProRes encoder of `profile`: the profile's fourcc, then the frame
    /// header (whole frame, size, chroma, progressive, alpha, square pixels, frame rate code, and
    /// colour codes that are BT.709 or unspecified).
    fn check_prores(&self, s: &CompressedSample, profile: filmcraft_prores::Profile, alpha: bool) -> Result<(), String> {
        use filmcraft_prores::{AlphaType, Interlace};
        if s.codec != IntraCodec::ProRes || s.fourcc != profile.fourcc() {
            return Err(format!("{} source, {} export", String::from_utf8_lossy(&s.fourcc), String::from_utf8_lossy(&profile.fourcc())));
        }
        let h = filmcraft_prores::check_frame(&s.data).map_err(|e| format!("ProRes frame: {e}"))?;
        if (u32::from(h.width), u32::from(h.height)) != (self.width, self.height) {
            return Err(format!("frame header says {}x{}", h.width, h.height));
        }
        if h.chroma != profile.chroma() {
            return Err(format!("{:?} frame in a {profile:?} stream", h.chroma));
        }
        if h.interlace != Interlace::Progressive {
            return Err("interlaced frame".into());
        }
        if (h.alpha != AlphaType::None) != alpha {
            return Err(if alpha { "no alpha in the source frame".into() } else { "the source frame has alpha".into() });
        }
        let (w, ht) = (u64::from(h.width), u64::from(h.height));
        let square = match h.aspect_ratio {
            0 | 1 => true,
            2 => w * 3 == ht * 4,
            3 => w * 9 == ht * 16,
            _ => false,
        };
        if !square {
            return Err(format!("aspect ratio code {}", h.aspect_ratio));
        }
        if h.frame_rate_code != 0 && prores_rate(h.frame_rate_code).is_none_or(|(n, d)| !same_rate(FrameRate::new(n, d), self.rate)) {
            return Err(format!("frame rate code {}", h.frame_rate_code));
        }
        let c = h.color;
        let sig = self.signal;
        for (code, want) in [(c.primaries, sig.primaries), (c.transfer, sig.transfer), (c.matrix, sig.matrix)] {
            if code != want && !unspecified(code) {
                return Err(format!("frame colour {}/{}/{}", c.primaries, c.transfer, c.matrix));
            }
        }
        Ok(())
    }

    /// [`SampleCopy::check`] for a DNxHR encoder of `profile` at `bit_depth`: the `AVdh` fourcc,
    /// then the frame header (whole frame, compression ID, size, progressive, constant bitrate,
    /// 4:2:2 Y'CbCr without alpha, depth, BT.709 colour volume, square pixels).
    fn check_dnx(&self, s: &CompressedSample, profile: filmcraft_dnx::Profile, bit_depth: u8) -> Result<(), String> {
        use filmcraft_dnx::{ChromaFormat, ColorVolume, FieldCode};
        if s.codec != IntraCodec::Dnx || s.fourcc != *b"AVdh" {
            return Err(format!("{} source, AVdh export", String::from_utf8_lossy(&s.fourcc)));
        }
        let h = filmcraft_dnx::check_frame(&s.data).map_err(|e| format!("VC-3 frame: {e}"))?;
        if h.cid != profile.cid() {
            return Err(format!("{} source, {} export", filmcraft_dnx::cid_name(h.cid), profile.name()));
        }
        if (h.width, h.lines) != (self.width, self.height) {
            return Err(format!("frame header says {}x{}", h.width, h.lines));
        }
        if h.interlaced || h.field != FieldCode::Frame {
            return Err("interlaced frame".into());
        }
        if h.vbr {
            return Err("variable bitrate frame".into());
        }
        if h.alpha || h.chroma != ChromaFormat::Yuv422 || h.rgb {
            return Err(format!("{:?} frame (rgb {}, alpha {})", h.chroma, h.rgb, h.alpha));
        }
        if h.bit_depth != bit_depth {
            return Err(format!("{}-bit source, {bit_depth}-bit export", h.bit_depth));
        }
        if h.color_volume != ColorVolume::Bt709 {
            return Err(format!("colour volume {:?}", h.color_volume));
        }
        if h.par.0 != h.par.1 {
            return Err(format!("pixel aspect {}:{}", h.par.0, h.par.1));
        }
        Ok(())
    }
}

static COPIED: AtomicU64 = AtomicU64::new(0);
static RENDERED: AtomicU64 = AtomicU64::new(0);

/// Smart rendering counters (`perf.stats` `export.smartRender`), summed over the exports of this
/// process: frames written as the source's compressed sample, and frames of exports whose encoder
/// takes such copies that were rendered and encoded instead.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SmartRenderStats {
    pub copied: u64,
    pub rendered: u64,
}

/// The smart rendering counters so far.
pub fn smart_render_stats() -> SmartRenderStats {
    SmartRenderStats { copied: COPIED.load(Ordering::Relaxed), rendered: RENDERED.load(Ordering::Relaxed) }
}

/// Count the frames of one batch of an export that takes copies.
pub(crate) fn note_frames(copied: u64, rendered: u64) {
    COPIED.fetch_add(copied, Ordering::Relaxed);
    RENDERED.fetch_add(rendered, Ordering::Relaxed);
}
