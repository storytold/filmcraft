//! Track descriptions (RFC 9559 §5.1.4).

use crate::codec::Codec;

/// Track type (`TrackType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackKind {
    Video,
    Audio,
    Complex,
    Logo,
    Subtitle,
    Buttons,
    Control,
    Metadata,
    Other(u8),
}

impl TrackKind {
    pub(crate) fn from_type(t: u64) -> TrackKind {
        match t {
            1 => TrackKind::Video,
            2 => TrackKind::Audio,
            3 => TrackKind::Complex,
            0x10 => TrackKind::Logo,
            0x11 => TrackKind::Subtitle,
            0x12 => TrackKind::Buttons,
            0x20 => TrackKind::Control,
            0x21 => TrackKind::Metadata,
            n => TrackKind::Other(n as u8),
        }
    }
}

/// SMPTE ST 2086 mastering display metadata (`MasteringMetadata`). Chromaticities are CIE 1931 xy,
/// luminance in cd/m².
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MasteringMetadata {
    /// Red, green, blue primaries as (x, y).
    pub primaries: [(f64, f64); 3],
    pub white_point: (f64, f64),
    pub luminance_max: f64,
    pub luminance_min: f64,
}

/// `Colour` element. Code points use ITU-T H.273 values (as Matroska does).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Colour {
    /// `MatrixCoefficients` (H.273; 2 = unspecified).
    pub matrix_coefficients: Option<u64>,
    pub bits_per_channel: Option<u64>,
    pub chroma_subsampling_horz: Option<u64>,
    pub chroma_subsampling_vert: Option<u64>,
    pub cb_subsampling_horz: Option<u64>,
    pub cb_subsampling_vert: Option<u64>,
    pub chroma_siting_horz: Option<u64>,
    pub chroma_siting_vert: Option<u64>,
    /// `Range`: 0 unspecified, 1 broadcast (limited), 2 full, 3 defined by matrix/transfer.
    pub range: Option<u64>,
    /// `TransferCharacteristics` (H.273).
    pub transfer_characteristics: Option<u64>,
    /// `Primaries` (H.273).
    pub primaries: Option<u64>,
    /// Maximum content light level (cd/m²).
    pub max_cll: Option<u64>,
    /// Maximum frame-average light level (cd/m²).
    pub max_fall: Option<u64>,
    pub mastering: Option<MasteringMetadata>,
}

impl Colour {
    /// True if `Range` says full range.
    pub fn full_range(&self) -> bool {
        self.range == Some(2)
    }
}

/// `Video` element.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VideoInfo {
    pub pixel_width: u32,
    pub pixel_height: u32,
    /// `DisplayWidth`/`DisplayHeight`, in `display_unit` (0 = pixels, 3 = display aspect ratio).
    pub display_width: Option<u32>,
    pub display_height: Option<u32>,
    pub display_unit: u64,
    /// Crop (top, bottom, left, right) in pixels.
    pub crop: (u32, u32, u32, u32),
    /// `FlagInterlaced`: 0 undetermined, 1 interlaced, 2 progressive.
    pub flag_interlaced: u64,
    /// `FieldOrder` (0 progressive, 1 tff, 2 undetermined, 6 bff, 9/14 interleaved).
    pub field_order: Option<u64>,
    pub stereo_mode: Option<u64>,
    /// `AlphaMode`: 1 if BlockAdditions carry alpha.
    pub alpha_mode: u64,
    /// `ColourSpace` FourCC (uncompressed video).
    pub colour_space: Option<[u8; 4]>,
    pub colour: Option<Colour>,
    pub projection: Option<Projection>,
}

/// `Projection` element (RFC 9559 §5.1.4.1.28.41): pose angles in degrees.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Projection {
    /// 0 rectangular, 1 equirectangular, 2 cubemap, 3 mesh.
    pub projection_type: u64,
    /// Clockwise around the up vector.
    pub yaw: f64,
    /// Counter-clockwise around the right vector.
    pub pitch: f64,
    /// Counter-clockwise around the forward vector.
    pub roll: f64,
}

impl VideoInfo {
    /// Clockwise quarter turns (0–3) to display a rectangular video, from `ProjectionPoseRoll`
    /// (a counter-clockwise angle, so −90 is a quarter turn clockwise). None for other
    /// projections, flips (non-zero yaw / pitch) or angles that are not a multiple of 90°.
    pub fn display_rotation(&self) -> Option<u8> {
        let Some(p) = &self.projection else { return Some(0) };
        if p.projection_type != 0 || p.yaw != 0.0 || p.pitch != 0.0 || p.roll % 90.0 != 0.0 {
            return None;
        }
        Some((-(p.roll / 90.0) as i64).rem_euclid(4) as u8)
    }

    /// Pixel aspect ratio derived from the display size (pixels or aspect-ratio units), reduced.
    pub fn pixel_aspect(&self) -> (u32, u32) {
        let (pw, ph) = (self.pixel_width.saturating_sub(self.crop.2 + self.crop.3) as u64, self.pixel_height.saturating_sub(self.crop.0 + self.crop.1) as u64);
        let (Some(dw), Some(dh)) = (self.display_width, self.display_height) else { return (1, 1) };
        if pw == 0 || ph == 0 || dw == 0 || dh == 0 || !matches!(self.display_unit, 0 | 3) {
            return (1, 1);
        }
        // PAR = (dw/dh) / (pw/ph)
        let (n, d) = (dw as u64 * ph, dh as u64 * pw);
        let g = gcd(n, d);
        ((n / g) as u32, (d / g) as u32)
    }
}

pub(crate) fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a.max(1)
}

/// `Audio` element.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioInfo {
    /// `SamplingFrequency` (Hz).
    pub sampling_frequency: f64,
    /// `OutputSamplingFrequency` (e.g. SBR output rate).
    pub output_sampling_frequency: Option<f64>,
    pub channels: u64,
    pub bit_depth: Option<u64>,
}

impl Default for AudioInfo {
    fn default() -> Self {
        Self { sampling_frequency: 8000.0, output_sampling_frequency: None, channels: 1, bit_depth: None }
    }
}

/// One `ContentEncoding`.
#[derive(Clone, Debug, PartialEq)]
pub enum ContentEncoding {
    /// `ContentCompAlgo` 3: the given bytes were stripped from the start of every frame.
    HeaderStripping { bytes: Vec<u8>, scope: u64 },
    /// Other compression (0 zlib, 1 bzlib, 2 lzo1x): not supported for reading frame data.
    Compression { algo: u64, settings: Vec<u8>, scope: u64 },
    /// Encryption (not supported for reading frame data).
    Encryption { scope: u64 },
}

/// One entry of a track's sample (frame) table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    /// Byte offset of the (stored) frame data in the file.
    pub offset: u64,
    /// Stored size (excluding header-stripped bytes; see [`Track::sample_size`]).
    pub size: u32,
    /// Presentation timestamp in the track timebase (`TimestampScale` ticks).
    pub pts: i64,
    /// Duration in timebase ticks (from `BlockDuration` or `DefaultDuration`), 0 if unknown.
    pub duration: u64,
    pub keyframe: bool,
    /// Index of the owning cluster in [`crate::MkvFile::clusters`].
    pub cluster: u32,
    /// Offset of the block element (SimpleBlock / BlockGroup) header.
    pub block_offset: u64,
    /// Index of this frame within a laced block.
    pub lace: u16,
}

/// A Matroska track (`TrackEntry`) plus, once indexed, its sample table.
#[derive(Clone, Debug)]
pub struct Track {
    pub number: u64,
    pub uid: u64,
    pub kind: TrackKind,
    pub enabled: bool,
    pub default: bool,
    pub forced: bool,
    pub hearing_impaired: bool,
    pub lacing: bool,
    /// `DefaultDuration` in nanoseconds.
    pub default_duration_ns: Option<u64>,
    pub name: String,
    /// `Language` (ISO 639-2, default `eng`).
    pub language: String,
    pub language_bcp47: Option<String>,
    pub codec_id: String,
    pub codec_private: Vec<u8>,
    pub codec_name: String,
    pub codec: Codec,
    /// `CodecDelay` in nanoseconds.
    pub codec_delay_ns: u64,
    /// `SeekPreRoll` in nanoseconds.
    pub seek_pre_roll_ns: u64,
    pub video: Option<VideoInfo>,
    pub audio: Option<AudioInfo>,
    pub encodings: Vec<ContentEncoding>,
    /// Timebase numerator/denominator in seconds: `TimestampScale / 1e9`, reduced.
    pub timebase: (u64, u64),
    /// Frame table (empty unless the file was opened with indexing).
    pub samples: Vec<Sample>,
    pub(crate) sync: Vec<u32>,
}

impl Track {
    /// Bytes prepended to every frame by header-stripping compression, if any.
    pub fn stripped_header(&self) -> Option<&[u8]> {
        self.encodings.iter().find_map(|e| match e {
            ContentEncoding::HeaderStripping { bytes, scope } if scope & 1 != 0 => Some(bytes.as_slice()),
            _ => None,
        })
    }

    /// True if frames can be returned verbatim or with header stripping undone.
    pub fn frames_readable(&self) -> bool {
        self.encodings.iter().all(|e| match e {
            ContentEncoding::HeaderStripping { .. } => true,
            ContentEncoding::Compression { scope, .. } | ContentEncoding::Encryption { scope } => scope & 1 == 0,
        })
    }

    /// Bytes to put in front of a stored frame of `stored_size` bytes to get the codec frame:
    /// header-stripped bytes, or for `V_PRORES` the 8-byte frame header (`size` + `icpf`) that the
    /// Matroska ProRes mapping omits.
    pub fn frame_prefix(&self, stored_size: u64) -> Vec<u8> {
        let mut p = self.stripped_header().map(<[u8]>::to_vec).unwrap_or_default();
        if matches!(self.codec, Codec::ProRes { .. }) {
            let total = stored_size + p.len() as u64 + 8;
            let mut h = (total as u32).to_be_bytes().to_vec();
            h.extend_from_slice(b"icpf");
            h.extend_from_slice(&p);
            p = h;
        }
        p
    }

    /// Frame size of sample `i` as returned by `read_sample` / packets (stored size plus prefix).
    pub fn sample_size(&self, i: usize) -> Option<u64> {
        let s = self.samples.get(i)?;
        Some(s.size as u64 + self.frame_prefix(s.size as u64).len() as u64)
    }

    /// Indices of keyframe samples, ascending by file order.
    pub fn keyframes(&self) -> &[u32] {
        &self.sync
    }

    /// Timebase ticks → nanoseconds.
    pub fn ticks_to_ns(&self, t: i64) -> i64 {
        (t as i128 * self.timebase.0 as i128 * 1_000_000_000 / self.timebase.1 as i128) as i64
    }

    /// Nanoseconds → timebase ticks (floor).
    pub fn ns_to_ticks(&self, ns: i64) -> i64 {
        let n = ns as i128 * self.timebase.1 as i128;
        let d = self.timebase.0 as i128 * 1_000_000_000;
        n.div_euclid(d) as i64
    }

    /// Index of the last sample with `pts <= t` (in file order), or the first sample.
    pub fn sample_at_pts(&self, t: i64) -> Option<usize> {
        if self.samples.is_empty() {
            return None;
        }
        let mut best: Option<usize> = None;
        for (i, s) in self.samples.iter().enumerate() {
            if s.pts <= t && best.is_none_or(|b| s.pts >= self.samples[b].pts) {
                best = Some(i);
            }
        }
        Some(best.unwrap_or(0))
    }

    /// Index of the keyframe with the greatest `pts <= t` (the first keyframe if none precedes `t`).
    pub fn keyframe_before_pts(&self, t: i64) -> Option<usize> {
        let mut best: Option<usize> = None;
        for &k in &self.sync {
            let s = &self.samples[k as usize];
            if s.pts <= t && best.is_none_or(|b| s.pts >= self.samples[b].pts) {
                best = Some(k as usize);
            }
        }
        best.or_else(|| self.sync.first().map(|&k| k as usize))
    }

    /// Index of the nearest keyframe at or before sample `index` in file order.
    pub fn sync_sample_before(&self, index: usize) -> usize {
        match self.sync.binary_search(&(index as u32)) {
            Ok(i) => self.sync[i] as usize,
            Err(0) => 0,
            Err(i) => self.sync[i - 1] as usize,
        }
    }
}
