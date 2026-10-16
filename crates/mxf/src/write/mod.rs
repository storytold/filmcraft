//! Clean-room MXF writer: OP1a (ST 378, frame-wrapped generic container) and OP-Atom (ST 390,
//! Avid-style: one clip-wrapped essence per file).
//!
//! The writer streams to any `Write + Seek` sink: [`MxfWriter::new`] writes the header partition
//! (open, incomplete) with the header metadata, then a body partition; essence follows as it is
//! pushed. [`MxfWriter::finish`] writes the footer partition (closed, complete header metadata with
//! the final durations, index table segments), the random index pack, and rewrites the header and
//! body partition packs and the header metadata in place (same size) so the file's header is closed
//! and complete too.
//!
//! Layout written:
//!
//! ```text
//! header partition pack · primer · preface · identification · content storage · essence container
//!   data · material package (+ tracks) · file package (+ tracks) · descriptor(s) · fill
//! body partition pack (BodySID 1)
//!   OP1a: one content package per edit unit: picture element, then one element per sound track
//!   OP-Atom: one clip-wrapped essence element
//! footer partition pack (IndexSID 2) · header metadata · index table segments
//! random index pack
//! ```
//!
//! Essence mappings: VC-3 / DNxHD / DNxHR (ST 2019-4), Apple ProRes (RDD 44), AVC byte stream
//! (ST 381-3; Annex B access units with in-band SPS / PPS) with long-GOP index entries (temporal
//! offsets, key-frame offsets, prediction flags), and Broadcast Wave PCM (ST 382, 8–32 bit).

mod labels;
#[cfg(test)]
mod tests;

use std::collections::VecDeque;
use std::io::{self, Seek, SeekFrom, Write};

use crate::Rational;
use labels::*;

/// A SMPTE ST 330 basic UMID (32 bytes): package identifiers.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Umid(pub [u8; 32]);

impl std::fmt::Debug for Umid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl Umid {
    /// A basic UMID (universal label `06 0A 2B 34 01 01 01 05 01 01 0D 20`, length 0x13, instance
    /// 0) whose 16-byte material number is a hash of `seed` (callers pass something unique: path,
    /// time, track). The same seed always gives the same UMID.
    pub fn from_seed(seed: &[u8]) -> Umid {
        let mut u = [0u8; 32];
        u[..16].copy_from_slice(&[0x06, 0x0A, 0x2B, 0x34, 0x01, 0x01, 0x01, 0x05, 0x01, 0x01, 0x0D, 0x20, 0x13, 0x00, 0x00, 0x00]);
        u[16..].copy_from_slice(&hash128(seed));
        Umid(u)
    }
    /// The material number (bytes 16..32).
    pub fn material_number(&self) -> [u8; 16] {
        self.0[16..].try_into().unwrap_or([0; 16])
    }
    pub fn is_zero(&self) -> bool {
        self.0 == [0; 32]
    }
}

fn hash128(seed: &[u8]) -> [u8; 16] {
    let mut a = 0xcbf2_9ce4_8422_2325u64;
    let mut b = 0x84222325_cbf29ce4u64 ^ seed.len() as u64;
    for &x in seed {
        a = (a ^ x as u64).wrapping_mul(0x0000_0100_0000_01B3);
        b = (b ^ x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(23);
    }
    let mix = |mut z: u64| {
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&mix(a ^ b.rotate_left(17)).to_be_bytes());
    out[8..].copy_from_slice(&mix(b ^ a.rotate_left(41)).to_be_bytes());
    out
}

/// Operational pattern of the written file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pattern {
    /// OP1a: one material package playing one file package; all essence interleaved per edit
    /// unit (frame wrapping).
    Op1a,
    /// OP-Atom: exactly one essence track (a picture or one sound track), clip-wrapped.
    OpAtom,
}

/// Picture essence coding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PictureCoding {
    /// SMPTE VC-3 (DNxHD / DNxHR) with its compression id (e.g. 1272 = DNxHR HQ).
    Vc3 { cid: u32 },
    /// Apple ProRes; `profile` as in the RDD 44 coding label: 1 Proxy, 2 LT, 3 422, 4 HQ, 5 4444,
    /// 6 4444 XQ.
    ProRes { profile: u8 },
    /// H.264 / AVC Annex B byte stream; `profile_idc` from the SPS (66, 77, 100, …).
    Avc { profile_idc: u8, intra: bool },
    /// CinemaDNG: one DNG dataset per picture (CinemaDNG specification §7), frame-wrapped only,
    /// with an RGBA descriptor of the decoded 16-bit RGB image. `linear_raw`: the pictures are
    /// LinearRaw (PhotometricInterpretation 34892) rather than CFA (32803).
    CinemaDng { linear_raw: bool },
}

impl PictureCoding {
    fn intra_only(&self) -> bool {
        match self {
            PictureCoding::Avc { intra, .. } => *intra,
            _ => true,
        }
    }
    /// Picture essence coding label (None: CinemaDNG, which has no registered one, §7.2.2.2).
    fn coding_label(&self) -> Option<[u8; 16]> {
        Some(match *self {
            PictureCoding::CinemaDng { .. } => return None,
            PictureCoding::Vc3 { cid } => {
                let code = if (1235..=1234 + 0x7F).contains(&cid) { (cid - 1234) as u8 } else { 0 };
                [0x06, 0x0E, 0x2B, 0x34, 0x04, 0x01, 0x01, 0x0A, 0x04, 0x01, 0x02, 0x02, 0x71, code, 0x00, 0x00]
            }
            PictureCoding::ProRes { profile } => {
                [0x06, 0x0E, 0x2B, 0x34, 0x04, 0x01, 0x01, 0x0D, 0x04, 0x01, 0x02, 0x02, 0x03, 0x06, profile.clamp(1, 6), 0x00]
            }
            PictureCoding::Avc { profile_idc, intra } => {
                let (b13, b14) = if intra {
                    (0x32, 0x20)
                } else {
                    (
                        0x31,
                        match profile_idc {
                            66 => 0x10,
                            77 => 0x20,
                            88 => 0x30,
                            110 => 0x50,
                            122 => 0x60,
                            244 => 0x70,
                            _ => 0x40,
                        },
                    )
                };
                [0x06, 0x0E, 0x2B, 0x34, 0x04, 0x01, 0x01, 0x0A, 0x04, 0x01, 0x02, 0x02, 0x01, b13, b14, 0x01]
            }
        })
    }
    /// (essence container label, element type) for frame or clip wrapping.
    fn container(&self, clip: bool) -> ([u8; 16], u8) {
        let w = if clip { 0x02 } else { 0x01 };
        match self {
            PictureCoding::Vc3 { .. } => (essence_container(0x0A, 0x11, w, 0x00), if clip { 0x0D } else { 0x0C }),
            PictureCoding::ProRes { .. } => (essence_container(0x0D, 0x1C, w, 0x00), if clip { 0x18 } else { 0x17 }),
            PictureCoding::Avc { .. } => (essence_container(0x0A, 0x10, 0x60, w), if clip { 0x16 } else { 0x15 }),
            // CinemaDNG §7.2.1 (provisional label) and §7.1.1.2 (element type 42h); always frame
            // wrapped (`MxfWriter::new` refuses clip wrapping)
            PictureCoding::CinemaDng { linear_raw } => {
                ([0x06, 0x0E, 0x2B, 0x34, 0x04, 0x01, 0x01, 0x01, 0x0F, 0x0A, 0x0D, 0x0B, 0x02, 0x7F, if *linear_raw { 0x02 } else { 0x01 }, 0x00], 0x42)
            }
        }
    }
}

/// Colour description of the picture (transfer, coding equations, primaries labels).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorSpace {
    #[default]
    Rec709,
    /// BT.2020 primaries and matrix, SMPTE ST 2084 transfer.
    Rec2020Pq,
    /// BT.2020 primaries and matrix, ARIB STD-B67 (HLG) transfer.
    Rec2020Hlg,
}

/// The picture track.
#[derive(Clone, Debug, PartialEq)]
pub struct PictureDesc {
    pub coding: PictureCoding,
    pub width: u32,
    pub height: u32,
    /// Display aspect ratio of the frame (e.g. 16/9).
    pub aspect: Rational,
    /// Bits per component.
    pub depth: u32,
    /// Chroma subsampling: (2, 1) 4:2:2, (2, 2) 4:2:0, (1, 1) 4:4:4.
    pub subsampling: (u32, u32),
    pub color: ColorSpace,
}

impl PictureDesc {
    /// Square pixels; 8-bit 4:2:0 for AVC, 10-bit 4:2:2 for ProRes, 8-bit 4:2:2 for VC-3
    /// (10-bit for DNxHR HQX / 444 and the 10-bit DNxHD ids).
    pub fn new(coding: PictureCoding, width: u32, height: u32) -> Self {
        let g = gcd(width.max(1), height.max(1));
        let (depth, subsampling) = match coding {
            PictureCoding::Avc { .. } => (8, (2, 2)),
            PictureCoding::ProRes { profile } => (if profile >= 5 { 12 } else { 10 }, if profile >= 5 { (1, 1) } else { (2, 1) }),
            PictureCoding::Vc3 { cid } => {
                (if matches!(cid, 1235 | 1241 | 1250 | 1256 | 1270 | 1271) { 10 } else { 8 }, if matches!(cid, 1256 | 1270) { (1, 1) } else { (2, 1) })
            }
            // §7.2.2.3: the decoded image is described as 16 bits per component
            PictureCoding::CinemaDng { .. } => (16, (1, 1)),
        };
        PictureDesc {
            coding,
            width,
            height,
            aspect: Rational::new((width.max(1) / g) as i32, (height.max(1) / g) as i32),
            depth,
            subsampling,
            color: ColorSpace::Rec709,
        }
    }
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// A PCM sound track (Broadcast Wave mapping, little-endian interleaved samples).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SoundDesc {
    pub sample_rate: u32,
    pub channels: u32,
    /// 8, 16, 24 or 32.
    pub bits: u32,
}

impl SoundDesc {
    pub fn block_align(&self) -> u32 {
        self.channels.max(1) * self.bits.div_ceil(8)
    }
}

/// Start timecode of the packages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StartTimecode {
    /// Frame count of the first frame (at the timecode rate).
    pub frames: i64,
    /// Timecode frame rate (the track's edit rate; 30000/1001 for 29.97).
    pub rate: Rational,
    pub drop_frame: bool,
}

impl StartTimecode {
    pub fn rounded_base(&self) -> u16 {
        if !self.rate.is_valid() {
            return 25;
        }
        ((self.rate.num as i64 + self.rate.den as i64 - 1) / self.rate.den as i64) as u16
    }
}

/// An MXF timestamp (UTC).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Timestamp {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub millisecond: u16,
}

impl Timestamp {
    /// From seconds since the Unix epoch (proleptic Gregorian, UTC).
    pub fn from_unix(secs: i64) -> Timestamp {
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        // civil-from-days (H. Hinnant)
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
        Timestamp {
            year: y.clamp(0, 65535) as u16,
            month: m as u8,
            day: d as u8,
            hour: (rem / 3600) as u8,
            minute: (rem / 60 % 60) as u8,
            second: (rem % 60) as u8,
            millisecond: 0,
        }
    }
    fn bytes(&self) -> [u8; 8] {
        let y = self.year.to_be_bytes();
        [y[0], y[1], self.month, self.day, self.hour, self.minute, self.second, (self.millisecond / 4) as u8]
    }
}

/// Package identifiers and names. The material package is what an AAF composition (or an NLE)
/// references; the file package describes the stored essence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageIds {
    pub material: Umid,
    pub file: Umid,
    pub material_name: String,
    pub file_name: String,
}

impl PackageIds {
    /// UMIDs derived from `seed` (material and file differ).
    pub fn from_seed(seed: &str, name: &str) -> Self {
        PackageIds {
            material: Umid::from_seed(format!("material:{seed}").as_bytes()),
            file: Umid::from_seed(format!("file:{seed}").as_bytes()),
            material_name: name.to_string(),
            file_name: name.to_string(),
        }
    }
}

/// Everything the writer needs up front.
#[derive(Clone, Debug, PartialEq)]
pub struct WriterConfig {
    pub pattern: Pattern,
    /// Edit rate of the essence tracks (the frame rate; OP-Atom sound files may use the sample
    /// rate, one edit unit per sample).
    pub edit_rate: Rational,
    pub picture: Option<PictureDesc>,
    pub sound: Vec<SoundDesc>,
    pub timecode: Option<StartTimecode>,
    pub ids: PackageIds,
    /// Track id of the first essence track (the timecode track is 1). OP-Atom files that share a
    /// material package give their tracks distinct ids (Avid style: V1 = 2, A1 = 3, …).
    pub first_track_id: u32,
    pub company: String,
    pub product: String,
    pub version: String,
    pub modified: Timestamp,
}

impl WriterConfig {
    pub fn new(pattern: Pattern, edit_rate: Rational, ids: PackageIds) -> Self {
        WriterConfig {
            pattern,
            edit_rate,
            picture: None,
            sound: Vec::new(),
            timecode: None,
            ids,
            first_track_id: 2,
            company: "FilmCraft".into(),
            product: "FilmCraft".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            modified: Timestamp::default(),
        }
    }
}

/// How a picture was coded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CodedKind {
    #[default]
    Intra,
    /// Forward predicted (P).
    Predicted,
    /// Bidirectionally predicted (B).
    Bidirectional,
}

/// Index information for one picture, in stored (decode) order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameInfo {
    /// A decoder can start here (IDR / intra picture with its parameter sets).
    pub key: bool,
    pub kind: CodedKind,
    /// Presentation position (edit units from the start); `None` = the stored position.
    pub display: Option<i64>,
}

impl FrameInfo {
    /// An independently decodable picture presented in stored order.
    pub fn intra() -> Self {
        FrameInfo { key: true, kind: CodedKind::Intra, display: None }
    }
}

/// One index entry under construction.
#[derive(Clone, Debug)]
struct Entry {
    /// Stream offset of the edit unit in the essence container.
    offset: u64,
    /// Offsets of slices 1.. from the edit unit start (OP1a sound elements).
    slices: Vec<u32>,
    info: FrameInfo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrackKind {
    Picture,
    Sound(usize),
}

#[derive(Clone, Debug)]
struct TrackW {
    id: u32,
    kind: TrackKind,
    element_key: [u8; 16],
    container: [u8; 16],
}

/// Header region: header metadata plus reserved fill, rewritten in place by `finish`.
const HEADER_RESERVE: usize = 4096;
const INDEX_SID: u32 = 2;
const BODY_SID: u32 = 1;
/// Entries per index table segment (the entry array's 2-byte local length limits a segment).
const MAX_SEGMENT_BYTES: usize = 60_000;

/// Streaming MXF writer. See the [module docs](self).
pub struct MxfWriter<W: Write + Seek> {
    w: W,
    base: u64,
    /// Bytes written so far (relative to `base`).
    pos: u64,
    cfg: WriterConfig,
    tracks: Vec<TrackW>,
    header_region: usize,
    body_partition: u64,
    /// File offset (relative) of the first byte of the essence container.
    essence_start: u64,
    entries: Vec<Entry>,
    // OP1a
    pending: VecDeque<(Vec<u8>, FrameInfo)>,
    sound_buf: Vec<Vec<u8>>,
    // OP-Atom
    clip_len: u64,
    clip_kl: u64,
    samples: u64,
}

fn ber4(n: usize) -> [u8; 4] {
    let b = (n as u32).to_be_bytes();
    [0x83, b[1], b[2], b[3]]
}

fn klv(out: &mut Vec<u8>, key: &[u8; 16], v: &[u8]) {
    out.extend_from_slice(key);
    if v.len() < 1 << 24 {
        out.extend_from_slice(&ber4(v.len()));
    } else {
        out.push(0x88);
        out.extend_from_slice(&(v.len() as u64).to_be_bytes());
    }
    out.extend_from_slice(v);
}

fn fill(out: &mut Vec<u8>, total: usize) {
    if total < 20 {
        // too small for a fill item: zero-length fill items are not possible; caller avoids this
        out.extend(std::iter::repeat_n(0u8, total));
        return;
    }
    out.extend_from_slice(&KLV_FILL);
    out.extend_from_slice(&ber4(total - 20));
    out.extend(std::iter::repeat_n(0u8, total - 20));
}

fn batch(items: &[&[u8]], item_size: usize) -> Vec<u8> {
    let mut v = (items.len() as u32).to_be_bytes().to_vec();
    v.extend_from_slice(&(item_size as u32).to_be_bytes());
    for i in items {
        v.extend_from_slice(i);
    }
    v
}

fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(|u| u.to_be_bytes()).collect()
}

fn rational(r: Rational) -> [u8; 8] {
    let mut b = [0u8; 8];
    b[..4].copy_from_slice(&r.num.to_be_bytes());
    b[4..].copy_from_slice(&r.den.to_be_bytes());
    b
}

/// A header metadata local set under construction.
struct Set {
    key: [u8; 16],
    v: Vec<u8>,
}

impl Set {
    fn new(kind: u8, uid: [u8; 16]) -> Set {
        let mut s = Set { key: set_key(kind), v: Vec::new() };
        s.p(0x3C0A, &uid);
        s
    }
    fn p(&mut self, tag: u16, v: &[u8]) -> &mut Set {
        self.v.extend_from_slice(&tag.to_be_bytes());
        self.v.extend_from_slice(&(v.len().min(0xFFFF) as u16).to_be_bytes());
        self.v.extend_from_slice(&v[..v.len().min(0xFFFF)]);
        self
    }
    fn u8(&mut self, tag: u16, x: u8) -> &mut Set {
        self.p(tag, &[x])
    }
    fn u16(&mut self, tag: u16, x: u16) -> &mut Set {
        self.p(tag, &x.to_be_bytes())
    }
    fn u32(&mut self, tag: u16, x: u32) -> &mut Set {
        self.p(tag, &x.to_be_bytes())
    }
    fn i64(&mut self, tag: u16, x: i64) -> &mut Set {
        self.p(tag, &x.to_be_bytes())
    }
    fn rat(&mut self, tag: u16, r: Rational) -> &mut Set {
        self.p(tag, &rational(r))
    }
    fn refs(&mut self, tag: u16, uids: &[[u8; 16]]) -> &mut Set {
        let items: Vec<&[u8]> = uids.iter().map(|u| &u[..]).collect();
        self.p(tag, &batch(&items, 16))
    }
    fn write(&self, out: &mut Vec<u8>) {
        klv(out, &self.key, &self.v);
    }
}

/// Final (or provisional, -1 = unknown) durations for the metadata.
#[derive(Clone, Copy, Debug)]
struct Durations {
    /// Edit units of the essence tracks.
    essence: i64,
}

impl<W: Write + Seek> MxfWriter<W> {
    /// Start a file: writes the header partition and the body partition pack.
    pub fn new(mut w: W, cfg: WriterConfig) -> io::Result<Self> {
        if !cfg.edit_rate.is_valid() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "MXF edit rate must be positive"));
        }
        let n_essence = cfg.picture.is_some() as usize + cfg.sound.len();
        if n_essence == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "MXF file without essence tracks"));
        }
        if cfg.pattern == Pattern::OpAtom && n_essence != 1 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "OP-Atom files hold exactly one essence track"));
        }
        if cfg.sound.iter().any(|s| s.sample_rate == 0 || s.channels == 0 || !(1..=32).contains(&s.bits)) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid PCM sound description"));
        }
        let clip = cfg.pattern == Pattern::OpAtom;
        if clip && matches!(cfg.picture.as_ref().map(|p| p.coding), Some(PictureCoding::CinemaDng { .. })) {
            // CinemaDNG §7.1.1: a TIFF dataset has no length field, so DNG pictures can't be clip wrapped
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "CinemaDNG pictures are frame wrapped only (this writer's OP-Atom is clip wrapped)"));
        }
        let mut tracks = Vec::new();
        let mut id = cfg.first_track_id.max(2);
        if let Some(p) = &cfg.picture {
            let (container, et) = p.coding.container(clip);
            tracks.push(TrackW { id, kind: TrackKind::Picture, element_key: element_key(0x15, 1, et, 1), container });
            id += 1;
        }
        let ns = cfg.sound.len() as u8;
        for k in 0..cfg.sound.len() {
            let container = essence_container(0x01, 0x06, if clip { 0x02 } else { 0x01 }, 0x00);
            tracks.push(TrackW { id, kind: TrackKind::Sound(k), element_key: element_key(0x16, ns, if clip { 0x02 } else { 0x01 }, k as u8 + 1), container });
            id += 1;
        }
        let base = w.stream_position()?;
        let nsound = cfg.sound.len();
        let mut me = MxfWriter {
            w,
            base,
            pos: 0,
            cfg,
            tracks,
            header_region: 0,
            body_partition: 0,
            essence_start: 0,
            entries: Vec::new(),
            pending: VecDeque::new(),
            sound_buf: vec![Vec::new(); nsound],
            clip_len: 0,
            clip_kl: 0,
            samples: 0,
        };
        let meta = me.metadata(Durations { essence: -1 });
        me.header_region = meta.len() + HEADER_RESERVE;
        let pack = me.partition_pack(2, 1, 0, 0, 0, me.header_region as u64, 0, 0, 0);
        let mut out = pack;
        out.extend_from_slice(&meta);
        fill(&mut out, HEADER_RESERVE);
        me.put(&out)?;
        me.body_partition = me.pos;
        let body = me.partition_pack(3, 4, me.pos, 0, 0, 0, 0, 0, BODY_SID);
        me.put(&body)?;
        me.essence_start = me.pos;
        if clip {
            // clip-wrapped element: key + 9-byte BER length, patched by finish()
            me.clip_kl = me.pos;
            let mut kl = me.tracks[0].element_key.to_vec();
            kl.push(0x88);
            kl.extend_from_slice(&[0; 8]);
            me.put(&kl)?;
        }
        Ok(me)
    }

    /// The configuration (package UMIDs and names included).
    pub fn config(&self) -> &WriterConfig {
        &self.cfg
    }

    /// Edit units written so far (content packages, or OP-Atom pictures).
    pub fn edit_units(&self) -> u64 {
        self.entries.len() as u64
    }

    fn put(&mut self, b: &[u8]) -> io::Result<()> {
        self.w.write_all(b)?;
        self.pos += b.len() as u64;
        Ok(())
    }

    fn uid(&self, n: u32) -> [u8; 16] {
        let mut u = self.cfg.ids.file.material_number();
        u[6] = (u[6] & 0x0F) | 0x40;
        u[8] = (u[8] & 0x3F) | 0x80;
        u[12..].copy_from_slice(&n.to_be_bytes());
        u
    }

    fn op_label(&self) -> [u8; 16] {
        match self.cfg.pattern {
            Pattern::Op1a => op1a(self.tracks.len() > 1),
            Pattern::OpAtom => OP_ATOM,
        }
    }

    fn containers(&self) -> Vec<[u8; 16]> {
        let mut v: Vec<[u8; 16]> = Vec::new();
        for t in &self.tracks {
            if !v.contains(&t.container) {
                v.push(t.container);
            }
        }
        if self.tracks.len() > 1 {
            v.push(EC_MULTIPLE);
        }
        v
    }

    #[allow(clippy::too_many_arguments)]
    fn partition_pack(&self, kind: u8, status: u8, this: u64, prev: u64, footer: u64, hbc: u64, ibc: u64, index_sid: u32, body_sid: u32) -> Vec<u8> {
        let mut v = Vec::with_capacity(160);
        v.extend_from_slice(&1u16.to_be_bytes());
        v.extend_from_slice(&3u16.to_be_bytes());
        v.extend_from_slice(&1u32.to_be_bytes()); // KAG size
        v.extend_from_slice(&this.to_be_bytes());
        v.extend_from_slice(&prev.to_be_bytes());
        v.extend_from_slice(&footer.to_be_bytes());
        v.extend_from_slice(&hbc.to_be_bytes());
        v.extend_from_slice(&ibc.to_be_bytes());
        v.extend_from_slice(&index_sid.to_be_bytes());
        v.extend_from_slice(&0u64.to_be_bytes()); // body offset
        v.extend_from_slice(&body_sid.to_be_bytes());
        v.extend_from_slice(&self.op_label());
        let cs = self.containers();
        let items: Vec<&[u8]> = cs.iter().map(|c| &c[..]).collect();
        v.extend_from_slice(&batch(&items, 16));
        let mut out = Vec::new();
        klv(&mut out, &partition_key(kind, status), &v);
        out
    }

    /// Sound sample frames of edit unit `i` of OP1a track `k`.
    fn unit_samples(&self, k: usize, i: u64) -> u64 {
        let sr = self.cfg.sound[k].sample_rate as u128;
        let r = self.cfg.edit_rate;
        let at = |n: u64| (n as u128 * sr * r.den as u128 / r.num as u128) as u64;
        at(i + 1) - at(i)
    }

    /// Duration of an OP-Atom sound track in edit units.
    fn sound_units(&self) -> i64 {
        let sr = self.cfg.sound.first().map_or(1, |s| s.sample_rate) as u128;
        let r = self.cfg.edit_rate;
        let num = self.samples as u128 * r.num as u128;
        let den = sr * r.den as u128;
        num.div_ceil(den.max(1)) as i64
    }

    fn essence_units(&self) -> i64 {
        match (self.cfg.pattern, self.cfg.picture.is_some()) {
            (Pattern::OpAtom, false) => self.sound_units(),
            _ => self.entries.len() as i64,
        }
    }

    /// Push one picture (stored order). OP1a: content packages are written as soon as the sound
    /// of their edit unit is there too.
    pub fn push_picture(&mut self, data: &[u8], info: FrameInfo) -> io::Result<()> {
        if self.cfg.picture.is_none() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "this MXF file has no picture track"));
        }
        match self.cfg.pattern {
            Pattern::Op1a => {
                self.pending.push_back((data.to_vec(), info));
                self.emit(false)
            }
            Pattern::OpAtom => {
                self.entries.push(Entry { offset: self.clip_len, slices: Vec::new(), info });
                self.put(data)?;
                self.clip_len += data.len() as u64;
                Ok(())
            }
        }
    }

    /// Push interleaved little-endian PCM of sound track `track` (whole sample frames).
    pub fn push_sound(&mut self, track: usize, pcm: &[u8]) -> io::Result<()> {
        let Some(sd) = self.cfg.sound.get(track).copied() else {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("no sound track {track}")));
        };
        let ba = sd.block_align() as usize;
        if !pcm.len().is_multiple_of(ba) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "PCM data is not a whole number of sample frames"));
        }
        match self.cfg.pattern {
            Pattern::Op1a => {
                self.sound_buf[track].extend_from_slice(pcm);
                self.emit(false)
            }
            Pattern::OpAtom => {
                self.put(pcm)?;
                self.clip_len += pcm.len() as u64;
                self.samples += (pcm.len() / ba) as u64;
                Ok(())
            }
        }
    }

    /// Write every complete OP1a content package (`flush`: pad the sound of the pending pictures
    /// with silence, or, without a picture track, write the remaining sound).
    fn emit(&mut self, flush: bool) -> io::Result<()> {
        let has_pic = self.cfg.picture.is_some();
        loop {
            let i = self.entries.len() as u64;
            if has_pic && self.pending.is_empty() {
                return Ok(());
            }
            let need: Vec<usize> = (0..self.cfg.sound.len()).map(|k| self.unit_samples(k, i) as usize * self.cfg.sound[k].block_align() as usize).collect();
            let enough = need.iter().zip(&self.sound_buf).all(|(n, b)| b.len() >= *n);
            if !enough && !flush {
                return Ok(());
            }
            if !has_pic && self.sound_buf.iter().all(Vec::is_empty) {
                return Ok(());
            }
            let pic = if has_pic { self.pending.pop_front() } else { None };
            let mut cp = Vec::new();
            let mut slices = Vec::new();
            if let Some((data, _)) = &pic {
                klv(&mut cp, &self.tracks[0].element_key, data);
            }
            let first_sound = has_pic as usize;
            for (k, n) in need.iter().enumerate() {
                let take = (*n).min(self.sound_buf[k].len());
                let mut chunk: Vec<u8> = self.sound_buf[k].drain(..take).collect();
                chunk.resize(*n, 0);
                if k > 0 || has_pic {
                    slices.push(cp.len() as u32);
                }
                klv(&mut cp, &self.tracks[first_sound + k].element_key, &chunk);
            }
            let info = pic.map(|p| p.1).unwrap_or_else(FrameInfo::intra);
            self.entries.push(Entry { offset: self.pos - self.essence_start, slices, info });
            self.put(&cp)?;
        }
    }

    /// Header metadata (primer pack and sets).
    fn metadata(&self, d: Durations) -> Vec<u8> {
        let cfg = &self.cfg;
        let mut out = Vec::new();
        let primer: Vec<Vec<u8>> = PRIMER
            .iter()
            .map(|(t, ul)| {
                let mut e = t.to_be_bytes().to_vec();
                e.extend_from_slice(ul);
                e
            })
            .collect();
        let items: Vec<&[u8]> = primer.iter().map(Vec::as_slice).collect();
        klv(&mut out, &PRIMER_KEY, &batch(&items, 18));
        let ts = cfg.modified.bytes();
        let containers = self.containers();
        // Preface
        let mut s = Set::new(0x2F, self.uid(1));
        s.p(0x3B02, &ts).u16(0x3B05, 0x0103).u32(0x3B07, 1).refs(0x3B06, &[self.uid(2)]).p(0x3B03, &self.uid(3)).p(0x3B09, &self.op_label());
        let ec: Vec<&[u8]> = containers.iter().map(|c| &c[..]).collect();
        s.p(0x3B0A, &batch(&ec, 16)).p(0x3B0B, &batch(&[], 16));
        s.write(&mut out);
        // Identification
        let mut s = Set::new(0x30, self.uid(2));
        s.p(0x3C09, &self.uid(100))
            .p(0x3C01, &utf16(&cfg.company))
            .p(0x3C02, &utf16(&cfg.product))
            .p(0x3C04, &utf16(&cfg.version))
            .p(0x3C05, &hash128(b"FilmCraft MXF writer"))
            .p(0x3C06, &ts);
        s.write(&mut out);
        // Content storage, essence container data
        let mut s = Set::new(0x18, self.uid(3));
        s.refs(0x1901, &[self.uid(1000), self.uid(2000)]).refs(0x1902, &[self.uid(4)]);
        s.write(&mut out);
        let mut s = Set::new(0x23, self.uid(4));
        s.p(0x2701, &cfg.ids.file.0).u32(0x3F06, INDEX_SID).u32(0x3F07, BODY_SID);
        s.write(&mut out);
        // Packages
        for (pkg, base) in [(0u8, 1000u32), (1, 2000)] {
            let material = pkg == 0;
            let n = 1 + self.tracks.len() as u32;
            let track_uids: Vec<[u8; 16]> = (0..n).map(|k| self.uid(base + 1 + 3 * k)).collect();
            let mut s = Set::new(if material { 0x36 } else { 0x37 }, self.uid(base));
            let (umid, name) = if material { (cfg.ids.material, &cfg.ids.material_name) } else { (cfg.ids.file, &cfg.ids.file_name) };
            s.p(0x4401, &umid.0);
            if !name.is_empty() {
                s.p(0x4402, &utf16(name));
            }
            s.p(0x4405, &ts).p(0x4404, &ts).refs(0x4403, &track_uids);
            if !material {
                s.p(0x4701, &self.uid(3000));
            }
            s.write(&mut out);
            // timecode track (id 1)
            let tc = cfg.timecode.unwrap_or(StartTimecode { frames: 0, rate: cfg.edit_rate, drop_frame: false });
            let tc_rate = if cfg.picture.is_some() || !tc.rate.is_valid() { cfg.edit_rate } else { tc.rate };
            let tc_dur = if d.essence < 0 {
                -1
            } else {
                let er = cfg.edit_rate;
                {
                    let (a, b) = (d.essence as i128 * tc_rate.num as i128 * er.den as i128, (tc_rate.den as i128 * er.num as i128).max(1));
                    ((a + b - 1) / b) as i64
                }
            };
            self.track_sets(&mut out, base + 1, 1, 0, tc_rate, DEF_TIMECODE, tc_dur, if material { Some("TC1") } else { None }, |s| {
                s.u16(0x1502, tc.rounded_base()).i64(0x1501, tc.frames).u8(0x1503, tc.drop_frame as u8);
            });
            let mut vi = 0;
            let mut ai = 0;
            for (k, t) in self.tracks.iter().enumerate() {
                let (def, label) = match t.kind {
                    TrackKind::Picture => {
                        vi += 1;
                        (DEF_PICTURE, format!("V{vi}"))
                    }
                    TrackKind::Sound(_) => {
                        ai += 1;
                        (DEF_SOUND, format!("A{ai}"))
                    }
                };
                let number = if material { 0 } else { u32::from_be_bytes(t.element_key[12..16].try_into().unwrap_or([0; 4])) };
                let file = cfg.ids.file;
                self.track_sets(&mut out, base + 4 + 3 * k as u32, t.id, number, cfg.edit_rate, def, d.essence, material.then_some(label.as_str()), |s| {
                    let (src, src_track) = if material { (file, t.id) } else { (Umid([0; 32]), 0) };
                    s.i64(0x1201, 0).p(0x1101, &src.0).u32(0x1102, src_track);
                });
            }
        }
        // Descriptors
        let multiple = self.tracks.len() > 1;
        let mut desc_uids = Vec::new();
        for (k, t) in self.tracks.iter().enumerate() {
            let uid = if multiple { self.uid(3001 + k as u32) } else { self.uid(3000) };
            desc_uids.push(uid);
            match t.kind {
                TrackKind::Picture => {
                    // Picture tracks exist only with a picture configuration.
                    let Some(p) = cfg.picture.as_ref() else { continue };
                    let rgba = matches!(p.coding, PictureCoding::CinemaDng { .. });
                    let mut s = Set::new(if rgba { 0x29 } else { 0x28 }, uid);
                    if multiple {
                        s.u32(0x3006, t.id);
                    }
                    s.rat(0x3001, cfg.edit_rate);
                    if d.essence >= 0 {
                        s.i64(0x3002, d.essence);
                    }
                    let (tcl, ce, cp) = match p.color {
                        ColorSpace::Rec709 => (TC_709, CE_709, CP_709),
                        ColorSpace::Rec2020Pq => (TC_PQ, CE_2020, CP_2020),
                        ColorSpace::Rec2020Hlg => (TC_HLG, CE_2020, CP_2020),
                    };
                    let depth = p.depth.clamp(8, 16);
                    let black = 16u32 << (depth - 8);
                    let white = 235u32 << (depth - 8);
                    let range = (225u32 << (depth - 8)) + 1;
                    s.p(0x3004, &t.container)
                        .u8(0x320C, 0)
                        .u32(0x3203, p.width)
                        .u32(0x3202, p.height)
                        .u32(0x3205, p.width)
                        .u32(0x3204, p.height)
                        .u32(0x3209, p.width)
                        .u32(0x3208, p.height)
                        .rat(0x320E, p.aspect)
                        .p(0x320D, &batch(&[&0u32.to_be_bytes(), &0u32.to_be_bytes()], 4));
                    if let Some(label) = p.coding.coding_label() {
                        s.p(0x3201, &label);
                    }
                    if rgba {
                        // RGBA descriptor (ST 377-1 G.2.40): pixel layout R, G, B of `depth` bits
                        // each, and the component reference levels (CinemaDNG §7.2.2.3)
                        let d = depth as u8;
                        let mut layout = [0u8; 16];
                        layout[..6].copy_from_slice(&[b'R', d, b'G', d, b'B', d]);
                        let max = if depth >= 32 { u32::MAX } else { (1u32 << depth) - 1 };
                        s.p(0x3401, &layout).u32(0x3406, max).u32(0x3407, 0);
                        s.write(&mut out);
                        continue;
                    }
                    s.p(0x3210, &tcl)
                        .p(0x321A, &ce)
                        .p(0x3219, &cp)
                        .u32(0x3301, depth)
                        .u32(0x3302, p.subsampling.0)
                        .u32(0x3308, p.subsampling.1)
                        .u32(0x3304, black)
                        .u32(0x3305, white)
                        .u32(0x3306, range);
                    s.write(&mut out);
                }
                TrackKind::Sound(i) => {
                    let sd = cfg.sound[i];
                    let mut s = Set::new(0x48, uid);
                    if multiple {
                        s.u32(0x3006, t.id);
                    }
                    s.rat(0x3001, cfg.edit_rate);
                    if d.essence >= 0 {
                        s.i64(0x3002, d.essence);
                    }
                    s.p(0x3004, &t.container)
                        .rat(0x3D03, Rational::new(sd.sample_rate as i32, 1))
                        .u8(0x3D02, 1)
                        .u32(0x3D07, sd.channels)
                        .u32(0x3D01, sd.bits)
                        .u16(0x3D0A, sd.block_align() as u16)
                        .u32(0x3D09, sd.block_align() * sd.sample_rate);
                    s.write(&mut out);
                }
            }
        }
        if multiple {
            let mut s = Set::new(0x44, self.uid(3000));
            s.rat(0x3001, cfg.edit_rate);
            if d.essence >= 0 {
                s.i64(0x3002, d.essence);
            }
            s.p(0x3004, &EC_MULTIPLE).refs(0x3F01, &desc_uids);
            s.write(&mut out);
        }
        out
    }

    /// A track, its sequence and its one component (`component` adds the component-specific
    /// properties: source clip or timecode).
    #[allow(clippy::too_many_arguments)]
    fn track_sets(
        &self,
        out: &mut Vec<u8>,
        uid0: u32,
        id: u32,
        number: u32,
        rate: Rational,
        def: [u8; 16],
        duration: i64,
        name: Option<&str>,
        component: impl FnOnce(&mut Set),
    ) {
        let (tu, su, cu) = (self.uid(uid0), self.uid(uid0 + 1), self.uid(uid0 + 2));
        let mut t = Set::new(0x3B, tu);
        t.u32(0x4801, id).u32(0x4804, number);
        if let Some(n) = name {
            t.p(0x4802, &utf16(n));
        }
        t.rat(0x4B01, rate).i64(0x4B02, 0).p(0x4803, &su);
        t.write(out);
        let mut s = Set::new(0x0F, su);
        s.p(0x0201, &def).i64(0x0202, duration).refs(0x1001, &[cu]);
        s.write(out);
        let mut c = Set::new(if def == DEF_TIMECODE { 0x14 } else { 0x11 }, cu);
        c.p(0x0201, &def).i64(0x0202, duration);
        component(&mut c);
        c.write(out);
    }

    /// Index table segments for the essence written.
    fn index(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let rate = self.cfg.edit_rate;
        let mut seg_uid = 4000;
        let mut segment = |out: &mut Vec<u8>, f: &mut dyn FnMut(&mut Set)| {
            let mut s = Set { key: INDEX_KEY, v: Vec::new() };
            s.p(0x3C0A, &self.uid(seg_uid));
            seg_uid += 1;
            s.rat(0x3F0B, rate);
            f(&mut s);
            s.u32(0x3F06, INDEX_SID).u32(0x3F07, BODY_SID);
            s.write(out);
        };
        // OP-Atom sound at one sample per edit unit: constant bytes per edit unit
        if self.cfg.pattern == Pattern::OpAtom && self.cfg.picture.is_none() {
            let sd = self.cfg.sound[0];
            let per_unit = (sd.sample_rate as i64 * rate.den as i64) % rate.num as i64 == 0 && (sd.sample_rate as i64 * rate.den as i64) / rate.num as i64 == 1;
            if per_unit {
                let units = self.essence_units();
                segment(&mut out, &mut |s| {
                    s.i64(0x3F0C, 0).i64(0x3F0D, units).u32(0x3F05, sd.block_align()).u8(0x3F08, 0).u8(0x3F0E, 0);
                });
                return out;
            }
            // other edit rates: one entry per edit unit from the sample counts
            let units = self.essence_units().max(0) as u64;
            let ba = sd.block_align() as u64;
            let entries: Vec<Entry> = (0..units)
                .map(|i| {
                    let s = (i as u128 * sd.sample_rate as u128 * rate.den as u128 / rate.num as u128) as u64;
                    Entry { offset: s * ba, slices: Vec::new(), info: FrameInfo::intra() }
                })
                .collect();
            self.entry_segments(&mut out, &entries, &mut segment);
            return out;
        }
        self.entry_segments(&mut out, &self.entries, &mut segment);
        out
    }

    fn entry_segments(&self, out: &mut Vec<u8>, entries: &[Entry], segment: &mut dyn FnMut(&mut Vec<u8>, &mut dyn FnMut(&mut Set))) {
        let n = entries.len();
        let nslices = entries.iter().map(|e| e.slices.len()).max().unwrap_or(0);
        // temporal offsets: display d is stored at d + TO[d]
        let mut stored_of = vec![usize::MAX; n];
        let mut valid = true;
        for (s, e) in entries.iter().enumerate() {
            let d = e.info.display.unwrap_or(s as i64);
            if d < 0 || d as usize >= n || stored_of[d as usize] != usize::MAX {
                valid = false;
                break;
            }
            stored_of[d as usize] = s;
        }
        let intra = self.cfg.picture.as_ref().is_none_or(|p| p.coding.intra_only());
        let avc = matches!(self.cfg.picture.as_ref().map(|p| p.coding), Some(PictureCoding::Avc { .. }));
        let mut rows: Vec<Vec<u8>> = Vec::with_capacity(n);
        let mut last_key = 0usize;
        for (s, e) in entries.iter().enumerate() {
            let to = if valid { (stored_of[s] as i64 - s as i64).clamp(-128, 127) as i8 } else { 0 };
            if e.info.key || intra {
                last_key = s;
            }
            let kfo = (last_key as i64 - s as i64).max(-128) as i8;
            let flags: u8 = if e.info.key || intra {
                if avc { 0xC0 } else { 0x80 }
            } else {
                match e.info.kind {
                    CodedKind::Intra => 0x00,
                    CodedKind::Predicted => 0x22,
                    CodedKind::Bidirectional => 0x33,
                }
            };
            let mut r = vec![to as u8, kfo as u8, flags];
            r.extend_from_slice(&e.offset.to_be_bytes());
            for k in 0..nslices {
                r.extend_from_slice(&e.slices.get(k).copied().unwrap_or(0).to_be_bytes());
            }
            rows.push(r);
        }
        // delta entries: element k of the content package is in slice k (picture in slice 0)
        let n_el = if self.cfg.pattern == Pattern::Op1a { self.tracks.len() } else { 1 };
        let deltas: Vec<[u8; 6]> = (0..n_el).map(|k| [0, k.min(nslices) as u8, 0, 0, 0, 0]).collect();
        let delta_items: Vec<&[u8]> = deltas.iter().map(|d| &d[..]).collect();
        let delta = batch(&delta_items, 6);
        let entry_size = 11 + 4 * nslices;
        let per_seg = (MAX_SEGMENT_BYTES / entry_size).max(1);
        let mut start = 0usize;
        loop {
            let end = (start + per_seg).min(n);
            let items: Vec<&[u8]> = rows[start..end].iter().map(Vec::as_slice).collect();
            let arr = batch(&items, entry_size);
            segment(out, &mut |s: &mut Set| {
                s.i64(0x3F0C, start as i64)
                    .i64(0x3F0D, (end - start) as i64)
                    .u32(0x3F05, 0)
                    .u8(0x3F08, nslices as u8)
                    .u8(0x3F0E, 0)
                    .p(0x3F09, &delta)
                    .p(0x3F0A, &arr);
            });
            start = end;
            if start >= n {
                break;
            }
        }
    }

    /// Finish the file: footer partition with the final header metadata and the index, random
    /// index pack, and the closed header. Returns the sink (positioned at the end).
    pub fn finish(mut self) -> io::Result<W> {
        if self.cfg.pattern == Pattern::Op1a {
            self.emit(true)?;
        } else {
            // patch the clip element length
            let end = self.pos;
            self.w.seek(SeekFrom::Start(self.base + self.clip_kl + 16))?;
            let mut l = vec![0x88];
            l.extend_from_slice(&self.clip_len.to_be_bytes());
            self.w.write_all(&l)?;
            self.w.seek(SeekFrom::Start(self.base + end))?;
        }
        let d = Durations { essence: self.essence_units() };
        let meta = self.metadata(d);
        let index = self.index();
        let footer = self.pos;
        let pack = self.partition_pack(4, 4, footer, self.body_partition, footer, meta.len() as u64, index.len() as u64, INDEX_SID, 0);
        let mut out = pack;
        out.extend_from_slice(&meta);
        out.extend_from_slice(&index);
        // random index pack: (BodySID, offset) per partition, then the pack's length
        let mut rip = Vec::new();
        for (sid, off) in [(0u32, 0u64), (BODY_SID, self.body_partition), (0, footer)] {
            rip.extend_from_slice(&sid.to_be_bytes());
            rip.extend_from_slice(&off.to_be_bytes());
        }
        rip.extend_from_slice(&((16 + 4 + rip.len() + 4) as u32).to_be_bytes());
        klv(&mut out, &RIP_KEY, &rip);
        self.put(&out)?;
        let end = self.pos;
        // closed, complete header partition (same size as the provisional one)
        if meta.len() + 20 <= self.header_region {
            let mut h = self.partition_pack(2, 4, 0, 0, footer, self.header_region as u64, 0, 0, 0);
            h.extend_from_slice(&meta);
            fill(&mut h, self.header_region - meta.len());
            self.w.seek(SeekFrom::Start(self.base))?;
            self.w.write_all(&h)?;
        } else {
            // cannot happen (metadata sizes do not depend on the durations); keep the header open
            let h = self.partition_pack(2, 1, 0, 0, footer, self.header_region as u64, 0, 0, 0);
            self.w.seek(SeekFrom::Start(self.base))?;
            self.w.write_all(&h)?;
        }
        let body = self.partition_pack(3, 4, self.body_partition, 0, footer, 0, 0, 0, BODY_SID);
        self.w.seek(SeekFrom::Start(self.base + self.body_partition))?;
        self.w.write_all(&body)?;
        self.w.seek(SeekFrom::Start(self.base + end))?;
        self.w.flush()?;
        Ok(self.w)
    }
}

/// Interleave planar f32 samples as little-endian signed PCM of `bits` (16, 24 or 32; 8-bit is
/// unsigned as in WAVE).
pub fn encode_pcm(planar: &[Vec<f32>], bits: u32) -> Vec<u8> {
    let n = planar.iter().map(Vec::len).min().unwrap_or(0);
    let bps = bits.div_ceil(8).clamp(1, 4) as usize;
    let mut out = Vec::with_capacity(n * planar.len() * bps);
    for i in 0..n {
        for c in planar {
            // f32 products, as the WAV / QuickTime PCM writers quantise
            let s = c[i].clamp(-1.0, 1.0);
            match bps {
                1 => out.push(((s * 127.0).round() as i32 + 128) as u8),
                2 => out.extend_from_slice(&((s * 32767.0).round() as i16).to_le_bytes()),
                3 => out.extend_from_slice(&((s * 8_388_607.0).round() as i32).to_le_bytes()[..3]),
                _ => out.extend_from_slice(&((s as f64 * 2_147_483_647.0).round() as i32).to_le_bytes()),
            }
        }
    }
    out
}

/// Options of [`write_opatom_pcm`].
#[derive(Clone, Debug, PartialEq)]
pub struct OpAtomPcm {
    pub sample_rate: u32,
    /// 16 or 24 (any 8–32).
    pub bits: u32,
    pub channels: u32,
    /// Edit rate of the track (`None` = the sample rate: one edit unit per sample frame).
    pub edit_rate: Option<Rational>,
    pub ids: PackageIds,
    pub timecode: Option<StartTimecode>,
}

/// A complete OP-Atom PCM file in memory. `samples` are interleaved integer sample values in the
/// range of `bits` (e.g. -32768..=32767 for 16 bits).
pub fn write_opatom_pcm(opts: &OpAtomPcm, samples: &[i32]) -> io::Result<Vec<u8>> {
    let sd = SoundDesc { sample_rate: opts.sample_rate, channels: opts.channels.max(1), bits: opts.bits };
    let rate = opts.edit_rate.unwrap_or(Rational::new(opts.sample_rate as i32, 1));
    let mut cfg = WriterConfig::new(Pattern::OpAtom, rate, opts.ids.clone());
    cfg.sound = vec![sd];
    cfg.timecode = opts.timecode;
    let mut w = MxfWriter::new(io::Cursor::new(Vec::new()), cfg)?;
    let bps = sd.bits.div_ceil(8).clamp(1, 4) as usize;
    let frames = samples.len() / sd.channels as usize;
    let mut pcm = Vec::with_capacity(frames * sd.channels as usize * bps);
    for &s in &samples[..frames * sd.channels as usize] {
        if bps == 1 {
            pcm.push((s + 128) as u8);
        } else {
            pcm.extend_from_slice(&s.to_le_bytes()[..bps]);
        }
    }
    w.push_sound(0, &pcm)?;
    Ok(w.finish()?.into_inner())
}
