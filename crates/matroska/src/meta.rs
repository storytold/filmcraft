//! Segment-level metadata: Info, SeekHead, Cues, Chapters, Tags, Attachments — types and parsers
//! for their in-memory element data.

use crate::codec::map_codec;
use crate::ebml::{Children, float, int, string, uint};
use crate::ids::*;
use crate::track::{AudioInfo, Colour, ContentEncoding, MasteringMetadata, Projection, Track, TrackKind, VideoInfo, gcd};

/// `Info` element.
#[derive(Clone, Debug, PartialEq)]
pub struct SegmentInfo {
    /// Nanoseconds per timestamp tick (default 1 000 000 = 1 ms).
    pub timestamp_scale: u64,
    /// `Duration` in timestamp ticks (float), if present.
    pub duration: Option<f64>,
    pub title: Option<String>,
    pub muxing_app: String,
    pub writing_app: String,
    /// `DateUTC`: nanoseconds since 2001-01-01T00:00:00 UTC.
    pub date_utc: Option<i64>,
    pub segment_uuid: Option<[u8; 16]>,
}

impl Default for SegmentInfo {
    fn default() -> Self {
        Self {
            timestamp_scale: 1_000_000,
            duration: None,
            title: None,
            muxing_app: String::new(),
            writing_app: String::new(),
            date_utc: None,
            segment_uuid: None,
        }
    }
}

/// One `SeekHead` entry: level-1 element ID and its position relative to the Segment data start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeekEntry {
    pub id: u32,
    pub position: u64,
}

/// One `CueTrackPositions` of a `CuePoint`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CuePosition {
    pub track: u64,
    /// Cluster position relative to the Segment data start.
    pub cluster_position: u64,
    /// Block position relative to the Cluster data start.
    pub relative_position: Option<u64>,
    pub block_number: Option<u64>,
    pub duration: Option<u64>,
}

/// One `CuePoint`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CuePoint {
    /// In timestamp ticks.
    pub time: u64,
    pub positions: Vec<CuePosition>,
}

/// A cluster found while scanning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClusterInfo {
    /// Absolute offset of the Cluster element header.
    pub offset: u64,
    /// Absolute offset of the first child.
    pub data_start: u64,
    /// Absolute end offset (for unknown-size clusters, where the next top-level element starts).
    pub end: u64,
    /// Cluster `Timestamp` in ticks.
    pub timestamp: u64,
}

/// One `ChapterDisplay`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChapterDisplay {
    pub string: String,
    pub languages: Vec<String>,
    pub languages_bcp47: Vec<String>,
    pub countries: Vec<String>,
}

/// `ChapterAtom` (possibly nested).
#[derive(Clone, Debug, PartialEq)]
pub struct Chapter {
    pub uid: u64,
    pub string_uid: Option<String>,
    pub start_ns: u64,
    pub end_ns: Option<u64>,
    pub hidden: bool,
    pub enabled: bool,
    pub displays: Vec<ChapterDisplay>,
    pub children: Vec<Chapter>,
}

impl Chapter {
    /// First display string, if any.
    pub fn title(&self) -> Option<&str> {
        self.displays.first().map(|d| d.string.as_str())
    }
}

/// `EditionEntry`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Edition {
    pub uid: Option<u64>,
    pub hidden: bool,
    pub default: bool,
    pub ordered: bool,
    pub chapters: Vec<Chapter>,
}

/// `Targets` of a `Tag`. Empty UID lists mean "the whole segment".
#[derive(Clone, Debug, PartialEq)]
pub struct TagTargets {
    pub type_value: u64,
    pub type_name: Option<String>,
    pub track_uids: Vec<u64>,
    pub edition_uids: Vec<u64>,
    pub chapter_uids: Vec<u64>,
    pub attachment_uids: Vec<u64>,
}

impl Default for TagTargets {
    fn default() -> Self {
        Self { type_value: 50, type_name: None, track_uids: Vec::new(), edition_uids: Vec::new(), chapter_uids: Vec::new(), attachment_uids: Vec::new() }
    }
}

/// `SimpleTag` (possibly nested).
#[derive(Clone, Debug, PartialEq)]
pub struct SimpleTag {
    pub name: String,
    pub language: String,
    pub default: bool,
    pub string: Option<String>,
    pub binary: Option<Vec<u8>>,
    pub children: Vec<SimpleTag>,
}

/// `Tag`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tag {
    pub targets: TagTargets,
    pub simple: Vec<SimpleTag>,
}

/// `AttachedFile` (listed only; the payload is located by `data_offset`/`data_size`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Attachment {
    pub uid: u64,
    pub name: String,
    pub media_type: String,
    pub description: Option<String>,
    pub data_offset: u64,
    pub data_size: u64,
}

pub(crate) fn parse_info(d: &[u8]) -> SegmentInfo {
    let mut info = SegmentInfo::default();
    for e in Children::new(d) {
        match e.id {
            TIMESTAMP_SCALE => {
                let v = uint(e.data);
                if v > 0 {
                    info.timestamp_scale = v;
                }
            }
            DURATION => info.duration = Some(float(e.data)).filter(|v| v.is_finite() && *v >= 0.0),
            TITLE => info.title = Some(string(e.data)),
            MUXING_APP => info.muxing_app = string(e.data),
            WRITING_APP => info.writing_app = string(e.data),
            DATE_UTC => info.date_utc = Some(int(e.data)),
            SEGMENT_UUID => info.segment_uuid = e.data.try_into().ok(),
            _ => {}
        }
    }
    info
}

pub(crate) fn parse_seekhead(d: &[u8]) -> Vec<SeekEntry> {
    let mut out = Vec::new();
    for s in Children::new(d).filter(|e| e.id == SEEK) {
        let (mut id, mut pos) = (None, None);
        for e in Children::new(s.data) {
            match e.id {
                SEEK_ID => id = Some(uint(e.data) as u32),
                SEEK_POSITION => pos = Some(uint(e.data)),
                _ => {}
            }
        }
        if let (Some(id), Some(position)) = (id, pos) {
            out.push(SeekEntry { id, position });
        }
    }
    out
}

pub(crate) fn parse_cues(d: &[u8], out: &mut Vec<CuePoint>) {
    for cp in Children::new(d).filter(|e| e.id == CUE_POINT) {
        let mut time = None;
        let mut positions = Vec::new();
        for e in Children::new(cp.data) {
            match e.id {
                CUE_TIME => time = Some(uint(e.data)),
                CUE_TRACK_POSITIONS => {
                    let mut p = CuePosition { track: 0, cluster_position: u64::MAX, relative_position: None, block_number: None, duration: None };
                    for c in Children::new(e.data) {
                        match c.id {
                            CUE_TRACK => p.track = uint(c.data),
                            CUE_CLUSTER_POSITION => p.cluster_position = uint(c.data),
                            CUE_RELATIVE_POSITION => p.relative_position = Some(uint(c.data)),
                            CUE_BLOCK_NUMBER => p.block_number = Some(uint(c.data)),
                            CUE_DURATION => p.duration = Some(uint(c.data)),
                            _ => {}
                        }
                    }
                    if p.track != 0 && p.cluster_position != u64::MAX {
                        positions.push(p);
                    }
                }
                _ => {}
            }
        }
        if let Some(time) = time
            && !positions.is_empty()
        {
            out.push(CuePoint { time, positions });
        }
    }
}

fn parse_chapter(d: &[u8]) -> Chapter {
    let mut c = Chapter { uid: 0, string_uid: None, start_ns: 0, end_ns: None, hidden: false, enabled: true, displays: Vec::new(), children: Vec::new() };
    for e in Children::new(d) {
        match e.id {
            CHAPTER_UID => c.uid = uint(e.data),
            CHAPTER_STRING_UID => c.string_uid = Some(string(e.data)),
            CHAPTER_TIME_START => c.start_ns = uint(e.data),
            CHAPTER_TIME_END => c.end_ns = Some(uint(e.data)),
            CHAPTER_FLAG_HIDDEN => c.hidden = uint(e.data) != 0,
            CHAPTER_FLAG_ENABLED => c.enabled = uint(e.data) != 0,
            CHAPTER_DISPLAY => {
                let mut disp = ChapterDisplay::default();
                for x in Children::new(e.data) {
                    match x.id {
                        CHAP_STRING => disp.string = string(x.data),
                        CHAP_LANGUAGE => disp.languages.push(string(x.data)),
                        CHAP_LANGUAGE_BCP47 => disp.languages_bcp47.push(string(x.data)),
                        CHAP_COUNTRY => disp.countries.push(string(x.data)),
                        _ => {}
                    }
                }
                if disp.languages.is_empty() {
                    disp.languages.push("eng".into());
                }
                c.displays.push(disp);
            }
            CHAPTER_ATOM => c.children.push(parse_chapter(e.data)),
            _ => {}
        }
    }
    c
}

pub(crate) fn parse_chapters(d: &[u8], out: &mut Vec<Edition>) {
    for ed in Children::new(d).filter(|e| e.id == EDITION_ENTRY) {
        let mut edition = Edition::default();
        for e in Children::new(ed.data) {
            match e.id {
                EDITION_UID => edition.uid = Some(uint(e.data)),
                EDITION_FLAG_HIDDEN => edition.hidden = uint(e.data) != 0,
                EDITION_FLAG_DEFAULT => edition.default = uint(e.data) != 0,
                EDITION_FLAG_ORDERED => edition.ordered = uint(e.data) != 0,
                CHAPTER_ATOM => edition.chapters.push(parse_chapter(e.data)),
                _ => {}
            }
        }
        out.push(edition);
    }
}

fn parse_simple_tag(d: &[u8]) -> SimpleTag {
    let mut t = SimpleTag { name: String::new(), language: "und".into(), default: true, string: None, binary: None, children: Vec::new() };
    for e in Children::new(d) {
        match e.id {
            TAG_NAME => t.name = string(e.data),
            TAG_LANGUAGE => t.language = string(e.data),
            TAG_DEFAULT => t.default = uint(e.data) != 0,
            TAG_STRING => t.string = Some(string(e.data)),
            TAG_BINARY => t.binary = Some(e.data.to_vec()),
            SIMPLE_TAG => t.children.push(parse_simple_tag(e.data)),
            _ => {}
        }
    }
    t
}

pub(crate) fn parse_tags(d: &[u8], out: &mut Vec<Tag>) {
    for tg in Children::new(d).filter(|e| e.id == TAG) {
        let mut tag = Tag::default();
        for e in Children::new(tg.data) {
            match e.id {
                TARGETS => {
                    for x in Children::new(e.data) {
                        match x.id {
                            TARGET_TYPE_VALUE => tag.targets.type_value = uint(x.data),
                            TARGET_TYPE => tag.targets.type_name = Some(string(x.data)),
                            TAG_TRACK_UID => tag.targets.track_uids.push(uint(x.data)),
                            TAG_EDITION_UID => tag.targets.edition_uids.push(uint(x.data)),
                            TAG_CHAPTER_UID => tag.targets.chapter_uids.push(uint(x.data)),
                            TAG_ATTACHMENT_UID => tag.targets.attachment_uids.push(uint(x.data)),
                            _ => {}
                        }
                    }
                }
                SIMPLE_TAG => tag.simple.push(parse_simple_tag(e.data)),
                _ => {}
            }
        }
        out.push(tag);
    }
}

fn parse_colour(d: &[u8]) -> Colour {
    let mut c = Colour::default();
    for e in Children::new(d) {
        let v = || Some(uint(e.data));
        match e.id {
            MATRIX_COEFFICIENTS => c.matrix_coefficients = v(),
            BITS_PER_CHANNEL => c.bits_per_channel = v(),
            CHROMA_SUBSAMPLING_HORZ => c.chroma_subsampling_horz = v(),
            CHROMA_SUBSAMPLING_VERT => c.chroma_subsampling_vert = v(),
            CB_SUBSAMPLING_HORZ => c.cb_subsampling_horz = v(),
            CB_SUBSAMPLING_VERT => c.cb_subsampling_vert = v(),
            CHROMA_SITING_HORZ => c.chroma_siting_horz = v(),
            CHROMA_SITING_VERT => c.chroma_siting_vert = v(),
            RANGE => c.range = v(),
            TRANSFER_CHARACTERISTICS => c.transfer_characteristics = v(),
            PRIMARIES => c.primaries = v(),
            MAX_CLL => c.max_cll = v(),
            MAX_FALL => c.max_fall = v(),
            MASTERING_METADATA => {
                let mut m = MasteringMetadata::default();
                for x in Children::new(e.data) {
                    let f = float(x.data);
                    match x.id {
                        PRIMARY_R_X => m.primaries[0].0 = f,
                        PRIMARY_R_Y => m.primaries[0].1 = f,
                        PRIMARY_G_X => m.primaries[1].0 = f,
                        PRIMARY_G_Y => m.primaries[1].1 = f,
                        PRIMARY_B_X => m.primaries[2].0 = f,
                        PRIMARY_B_Y => m.primaries[2].1 = f,
                        WHITE_POINT_X => m.white_point.0 = f,
                        WHITE_POINT_Y => m.white_point.1 = f,
                        LUMINANCE_MAX => m.luminance_max = f,
                        LUMINANCE_MIN => m.luminance_min = f,
                        _ => {}
                    }
                }
                c.mastering = Some(m);
            }
            _ => {}
        }
    }
    c
}

fn parse_video(d: &[u8]) -> VideoInfo {
    let mut v = VideoInfo::default();
    for e in Children::new(d) {
        let u = uint(e.data);
        match e.id {
            PIXEL_WIDTH => v.pixel_width = u as u32,
            PIXEL_HEIGHT => v.pixel_height = u as u32,
            DISPLAY_WIDTH => v.display_width = Some(u as u32),
            DISPLAY_HEIGHT => v.display_height = Some(u as u32),
            DISPLAY_UNIT => v.display_unit = u,
            PIXEL_CROP_TOP => v.crop.0 = u as u32,
            PIXEL_CROP_BOTTOM => v.crop.1 = u as u32,
            PIXEL_CROP_LEFT => v.crop.2 = u as u32,
            PIXEL_CROP_RIGHT => v.crop.3 = u as u32,
            FLAG_INTERLACED => v.flag_interlaced = u,
            FIELD_ORDER => v.field_order = Some(u),
            STEREO_MODE => v.stereo_mode = Some(u),
            ALPHA_MODE => v.alpha_mode = u,
            COLOUR_SPACE => v.colour_space = e.data.try_into().ok(),
            COLOUR => v.colour = Some(parse_colour(e.data)),
            PROJECTION => {
                let mut p = Projection::default();
                for x in Children::new(e.data) {
                    match x.id {
                        PROJECTION_TYPE => p.projection_type = uint(x.data),
                        PROJECTION_POSE_YAW => p.yaw = float(x.data),
                        PROJECTION_POSE_PITCH => p.pitch = float(x.data),
                        PROJECTION_POSE_ROLL => p.roll = float(x.data),
                        _ => {}
                    }
                }
                v.projection = Some(p);
            }
            _ => {}
        }
    }
    v
}

fn parse_audio(d: &[u8]) -> AudioInfo {
    let mut a = AudioInfo::default();
    for e in Children::new(d) {
        match e.id {
            SAMPLING_FREQUENCY => a.sampling_frequency = float(e.data),
            OUTPUT_SAMPLING_FREQUENCY => a.output_sampling_frequency = Some(float(e.data)),
            CHANNELS => a.channels = uint(e.data),
            BIT_DEPTH => a.bit_depth = Some(uint(e.data)),
            _ => {}
        }
    }
    a
}

fn parse_encodings(d: &[u8]) -> Vec<ContentEncoding> {
    let mut list: Vec<(u64, ContentEncoding)> = Vec::new();
    for ce in Children::new(d).filter(|e| e.id == CONTENT_ENCODING) {
        let (mut order, mut scope, mut ty) = (0u64, 1u64, 0u64);
        let mut comp: Option<(u64, Vec<u8>)> = None;
        for e in Children::new(ce.data) {
            match e.id {
                CONTENT_ENCODING_ORDER => order = uint(e.data),
                CONTENT_ENCODING_SCOPE => scope = uint(e.data),
                CONTENT_ENCODING_TYPE => ty = uint(e.data),
                CONTENT_COMPRESSION => {
                    let (mut algo, mut settings) = (0u64, Vec::new());
                    for x in Children::new(e.data) {
                        match x.id {
                            CONTENT_COMP_ALGO => algo = uint(x.data),
                            CONTENT_COMP_SETTINGS => settings = x.data.to_vec(),
                            _ => {}
                        }
                    }
                    comp = Some((algo, settings));
                }
                _ => {}
            }
        }
        let enc = if ty == 1 {
            ContentEncoding::Encryption { scope }
        } else {
            let (algo, settings) = comp.unwrap_or((0, Vec::new()));
            if algo == 3 { ContentEncoding::HeaderStripping { bytes: settings, scope } } else { ContentEncoding::Compression { algo, settings, scope } }
        };
        list.push((order, enc));
    }
    // highest order is applied last on muxing → first to undo; keep decode order
    list.sort_by_key(|e| std::cmp::Reverse(e.0));
    list.into_iter().map(|e| e.1).collect()
}

pub(crate) fn parse_tracks(d: &[u8], timestamp_scale: u64) -> Vec<Track> {
    let g = gcd(timestamp_scale, 1_000_000_000);
    let timebase = (timestamp_scale / g, 1_000_000_000 / g);
    let mut out = Vec::new();
    for te in Children::new(d).filter(|e| e.id == TRACK_ENTRY) {
        let mut t = Track {
            number: 0,
            uid: 0,
            kind: TrackKind::Other(0),
            enabled: true,
            default: true,
            forced: false,
            hearing_impaired: false,
            lacing: true,
            default_duration_ns: None,
            name: String::new(),
            language: "eng".into(),
            language_bcp47: None,
            codec_id: String::new(),
            codec_private: Vec::new(),
            codec_name: String::new(),
            codec: crate::Codec::Other(String::new()),
            codec_delay_ns: 0,
            seek_pre_roll_ns: 0,
            video: None,
            audio: None,
            encodings: Vec::new(),
            timebase,
            samples: Vec::new(),
            sync: Vec::new(),
        };
        for e in Children::new(te.data) {
            match e.id {
                TRACK_NUMBER => t.number = uint(e.data),
                TRACK_UID => t.uid = uint(e.data),
                TRACK_TYPE => t.kind = TrackKind::from_type(uint(e.data)),
                FLAG_ENABLED => t.enabled = uint(e.data) != 0,
                FLAG_DEFAULT => t.default = uint(e.data) != 0,
                FLAG_FORCED => t.forced = uint(e.data) != 0,
                FLAG_HEARING_IMPAIRED => t.hearing_impaired = uint(e.data) != 0,
                FLAG_LACING => t.lacing = uint(e.data) != 0,
                DEFAULT_DURATION => t.default_duration_ns = Some(uint(e.data)).filter(|&v| v > 0),
                NAME => t.name = string(e.data),
                LANGUAGE => t.language = string(e.data),
                LANGUAGE_BCP47 => t.language_bcp47 = Some(string(e.data)),
                CODEC_ID => t.codec_id = string(e.data),
                CODEC_PRIVATE => t.codec_private = e.data.to_vec(),
                CODEC_NAME => t.codec_name = string(e.data),
                CODEC_DELAY => t.codec_delay_ns = uint(e.data),
                SEEK_PRE_ROLL => t.seek_pre_roll_ns = uint(e.data),
                VIDEO => t.video = Some(parse_video(e.data)),
                AUDIO => t.audio = Some(parse_audio(e.data)),
                CONTENT_ENCODINGS => t.encodings = parse_encodings(e.data),
                _ => {}
            }
        }
        if t.number == 0 {
            continue;
        }
        if t.kind == TrackKind::Audio && t.audio.is_none() {
            t.audio = Some(AudioInfo::default());
        }
        // CodecPrivate may itself be header-stripped (scope bit 2 = private data)
        for enc in &t.encodings {
            if let ContentEncoding::HeaderStripping { bytes, scope } = enc
                && scope & 2 != 0
            {
                let mut p = bytes.clone();
                p.extend_from_slice(&t.codec_private);
                t.codec_private = p;
            }
        }
        t.codec = map_codec(&t.codec_id, &t.codec_private, t.audio.as_ref());
        out.push(t);
    }
    out
}
