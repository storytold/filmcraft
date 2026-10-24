//! Opening an MXF file: KLV walk, partitions, metadata resolution, sample tables.

use std::collections::HashMap;
use std::sync::Arc;

use crate::essence::{Codec, PictureInfo, SoundFormat, SoundInfo, decode_pcm, identify_picture, sniff_picture};
use crate::index::{IndexEntry, IndexSegment};
use crate::klv::{KeyClass, Ul, ber_length, classify, find_header_partition};
use crate::meta::{Metadata, Primer, SUB_DESCRIPTORS_ITEM, Set, set_type as st, tag};
use crate::source::{ByteSource, read_upto};
use crate::{Error, Rational, Result};

/// Partition kinds (ST 377-1 §7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartitionKind {
    Header,
    Body,
    Footer,
}

/// A partition pack.
#[derive(Clone, Debug, PartialEq)]
pub struct Partition {
    /// File offset of the partition pack key.
    pub offset: u64,
    pub kind: PartitionKind,
    /// 1 open incomplete, 2 closed incomplete, 3 open complete, 4 closed complete.
    pub status: u8,
    pub major_version: u16,
    pub minor_version: u16,
    pub kag_size: u32,
    pub this_partition: u64,
    pub previous_partition: u64,
    pub footer_partition: u64,
    pub header_byte_count: u64,
    pub index_byte_count: u64,
    pub index_sid: u32,
    pub body_offset: u64,
    pub body_sid: u32,
    pub operational_pattern: Ul,
    pub essence_containers: Vec<Ul>,
    /// File offset of the first essence container KLV in this partition.
    pub essence_start: Option<u64>,
}

impl Partition {
    fn parse(offset: u64, kind: u8, status: u8, v: &[u8]) -> Option<Partition> {
        let mut c = crate::klv::Cur::new(v);
        let mut p = Partition {
            offset,
            kind: match kind {
                2 => PartitionKind::Header,
                3 => PartitionKind::Body,
                _ => PartitionKind::Footer,
            },
            status,
            major_version: c.u16()?,
            minor_version: c.u16()?,
            kag_size: c.u32()?,
            this_partition: c.u64()?,
            previous_partition: c.u64()?,
            footer_partition: c.u64()?,
            header_byte_count: c.u64()?,
            index_byte_count: c.u64()?,
            index_sid: c.u32()?,
            body_offset: c.u64()?,
            body_sid: c.u32()?,
            operational_pattern: c.ul()?,
            essence_containers: Vec::new(),
            essence_start: None,
        };
        let rest = &v[c.pos..];
        p.essence_containers = crate::klv::batch_of(rest).into_iter().filter_map(crate::klv::ul_of).collect();
        Some(p)
    }
    pub fn closed(&self) -> bool {
        self.status == 2 || self.status == 4
    }
    pub fn complete(&self) -> bool {
        self.status >= 3
    }
}

/// The operational pattern (ST 377-1 §5, ST 378, ST 390).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationalPattern {
    /// Generalized OP: item complexity 1-3 (1 single item, 2 playlist, 3 edit), package
    /// complexity 1-3 (a single, b ganged, c alternate). OP1a = `Generalized { item: 1, package: 1 }`.
    Generalized {
        item: u8,
        package: u8,
    },
    /// OP-Atom (ST 390): one essence track per file.
    Atom,
    Unknown,
}

impl OperationalPattern {
    pub fn from_ul(ul: &Ul) -> Self {
        if !ul.is_smpte() || !ul.item_starts_with(&[0x0D, 0x01, 0x02, 0x01]) {
            return OperationalPattern::Unknown;
        }
        match (ul.0[12], ul.0[13]) {
            (0x10, _) => OperationalPattern::Atom,
            (i @ 1..=3, p @ 1..=3) => OperationalPattern::Generalized { item: i, package: p },
            _ => OperationalPattern::Unknown,
        }
    }
    pub fn name(&self) -> String {
        match self {
            OperationalPattern::Generalized { item, package } => format!("OP{item}{}", (b'a' + package - 1) as char),
            OperationalPattern::Atom => "OP-Atom".into(),
            OperationalPattern::Unknown => "OP?".into(),
        }
    }
}

/// A timecode component (ST 377-1 §B.?: StartTimecode in frames at RoundedTimecodeBase).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timecode {
    /// Frame count of the first frame.
    pub start: i64,
    pub rounded_base: u16,
    pub drop_frame: bool,
}

impl Timecode {
    /// `HH:MM:SS:FF` (`;` before the frames when drop-frame).
    pub fn format(&self) -> String {
        let base = self.rounded_base.max(1) as i64;
        let mut f = self.start.max(0);
        if self.drop_frame && base % 30 == 0 {
            // SMPTE 12M drop-frame: skip 2 (4 at 60) frame numbers each minute except every tenth.
            let drop = 2 * base / 30;
            let per_10min = base * 600 - 9 * drop;
            let per_min = base * 60 - drop;
            let d = f / per_10min;
            let m = f % per_10min;
            f += 9 * drop * d + if m > drop { drop * ((m - drop) / per_min) } else { 0 };
        }
        let (hh, mm, ss, ff) = (f / (base * 3600), (f / (base * 60)) % 60, (f / base) % 60, f % base);
        format!("{hh:02}:{mm:02}:{ss:02}{}{ff:02}", if self.drop_frame { ';' } else { ':' })
    }
}

/// Essence track kinds (from the data definition label).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackKind {
    Picture,
    Sound,
    Data,
}

/// Frame wrapping (one element per edit unit) or clip wrapping (one element for the whole clip).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wrapping {
    Frame,
    Clip,
}

/// One picture edit unit, in stored (decode) order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    /// File offset of the picture data.
    pub offset: u64,
    pub size: u32,
    /// Presentation position in edit units (stored position when the order is unknown).
    pub pts: i64,
    /// A decoder can start here.
    pub key: bool,
    /// The index marks this picture as bidirectionally predicted.
    pub b_picture: bool,
}

/// A run of sound sample frames stored contiguously.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chunk {
    pub offset: u64,
    pub size: u64,
    /// Index of the chunk's first sample frame in the track.
    pub first_sample: u64,
    pub samples: u64,
}

/// A track of the file (source) package with essence in this file, as presented by the material
/// package.
#[derive(Clone, Debug)]
pub struct EssenceTrack {
    pub track_id: u32,
    pub track_number: u32,
    pub kind: TrackKind,
    pub name: Option<String>,
    /// Edit rate of the file package track.
    pub edit_rate: Rational,
    /// File package track origin (edit units of pre-charge before the zero point).
    pub origin: i64,
    /// Material package source clip start position (edit units after the origin).
    pub start_position: i64,
    /// Material package duration (edit units of `edit_rate`), when known.
    pub duration: Option<i64>,
    pub codec: Codec,
    pub wrapping: Wrapping,
    pub essence_container: Option<Ul>,
    pub picture: Option<PictureInfo>,
    pub sound: Option<SoundInfo>,
    pub sound_format: SoundFormat,
    pub body_sid: u32,
    pub index_sid: u32,
    /// Pictures in stored order.
    pub samples: Vec<Sample>,
    /// Display position → stored index (identity unless the index has temporal offsets).
    pub display_order: Vec<usize>,
    /// Sound data.
    pub chunks: Vec<Chunk>,
    /// The index carries temporal offsets (exact presentation order).
    pub temporal_offsets: bool,
    /// The index marks bidirectionally predicted pictures but has no temporal offsets: the
    /// presentation order must come from the bitstream.
    pub needs_reorder: bool,
    /// Random-access flags came from an index table.
    pub indexed: bool,
}

impl EssenceTrack {
    /// First presented edit unit (stored position of the material package's zero point).
    pub fn first_edit_unit(&self) -> i64 {
        self.origin.saturating_add(self.start_position).max(0)
    }
    /// Total sound sample frames stored.
    pub fn stored_sample_frames(&self) -> u64 {
        self.chunks.last().map_or(0, |c| c.first_sample.saturating_add(c.samples))
    }
    /// Sound sample frames per edit unit (rational), e.g. 48000/25.
    pub fn samples_per_edit_unit(&self) -> (i64, i64) {
        let sr = self.sound.as_ref().map(|s| s.sample_rate).unwrap_or_default();
        let er = self.edit_rate;
        if !sr.is_valid() || !er.is_valid() {
            return (1, 1);
        }
        (sr.num as i64 * er.den as i64, sr.den as i64 * er.num as i64)
    }
    /// Nearest random-access sample at or before stored index `i`.
    pub fn sync_before(&self, i: usize) -> usize {
        let i = i.min(self.samples.len().saturating_sub(1));
        (0..=i).rev().find(|&k| self.samples[k].key).unwrap_or(0)
    }
    /// The stored sample presented at display position `d`.
    pub fn sample_at(&self, d: i64) -> Option<usize> {
        if d < 0 {
            return None;
        }
        self.display_order.get(d as usize).copied()
    }
}

/// An opened MXF file.
#[derive(Clone, Debug)]
pub struct MxfFile {
    /// Bytes before the header partition pack.
    pub run_in: u64,
    pub operational_pattern: OperationalPattern,
    pub operational_pattern_label: Ul,
    pub partitions: Vec<Partition>,
    pub index_segments: Vec<IndexSegment>,
    pub tracks: Vec<EssenceTrack>,
    /// Start timecode of the material package (file package when the material package has none).
    pub timecode: Option<Timecode>,
    pub material_package_name: Option<String>,
    pub product_name: Option<String>,
    pub company_name: Option<String>,
    /// Index of the partition whose header metadata was used.
    pub metadata_partition: usize,
    /// Problems recovered from (truncation, unresolved references…).
    pub warnings: Vec<String>,
    pub file_size: u64,
}

/// An essence element found by the KLV walk.
#[derive(Clone, Copy, Debug)]
struct Element {
    key: Ul,
    value_offset: u64,
    size: u64,
    /// Cut short by the end of the file.
    truncated: bool,
}

struct WalkResult {
    run_in: u64,
    partitions: Vec<Partition>,
    /// Header metadata per partition index: (primer, sets as (key, value)).
    metadata: Vec<(usize, Primer, Vec<(Ul, Vec<u8>)>)>,
    index: Vec<IndexSegment>,
    elements: Vec<Element>,
    warnings: Vec<String>,
}

/// Values larger than this are never read while opening (metadata and index values are small).
const MAX_META_VALUE: u64 = 64 << 20;
/// Samples (frames x channels) of zero padding `read_pcm` will produce past the stored essence.
const PCM_PAD_FRAMES_LIMIT: usize = 1 << 24;

/// Buffered reads of KLV headers.
struct Window<'a, S: ByteSource + ?Sized> {
    src: &'a S,
    off: u64,
    buf: Vec<u8>,
}

impl<S: ByteSource + ?Sized> Window<'_, S> {
    fn get(&mut self, at: u64, n: usize) -> std::io::Result<&[u8]> {
        let have = at >= self.off && at + n as u64 <= self.off + self.buf.len() as u64;
        if !have {
            self.buf = read_upto(self.src, at, n.max(64 * 1024))?;
            self.off = at;
        }
        let a = (at - self.off) as usize;
        Ok(&self.buf[a..(a + n).min(self.buf.len())])
    }
}

fn walk(src: &(impl ByteSource + ?Sized)) -> Result<WalkResult> {
    let len = src.len();
    let head = read_upto(src, 0, 65_536 + 32)?;
    let run_in = find_header_partition(&head).ok_or_else(|| Error::NotMxf("no header partition pack".into()))? as u64;
    let mut w = WalkResult { run_in, partitions: Vec::new(), metadata: Vec::new(), index: Vec::new(), elements: Vec::new(), warnings: Vec::new() };
    let mut win = Window { src, off: 0, buf: Vec::new() };
    let mut pos = run_in;
    let mut cur_meta: Option<usize> = None;
    let mut resync_budget = 64 << 20;
    while pos + 17 <= len {
        let hdr = win.get(pos, 25)?.to_vec();
        let key = Ul(hdr[..16].try_into().unwrap_or([0; 16]));
        if !key.is_smpte() {
            // Lost sync (corrupt data): look for the next partition pack.
            match resync(src, pos + 1, len, &mut resync_budget)? {
                Some(p) => {
                    w.warnings.push(format!("lost KLV sync at byte {pos}; resumed at {p}"));
                    pos = p;
                    continue;
                }
                None => {
                    w.warnings.push(format!("lost KLV sync at byte {pos}"));
                    break;
                }
            }
        }
        let Some((vlen, n)) = ber_length(&hdr[16..]) else {
            w.warnings.push(format!("bad KLV length at byte {pos}"));
            break;
        };
        let vstart = pos + 16 + n as u64;
        let Some(vend) = vstart.checked_add(vlen) else { break };
        let class = classify(&key);
        if vend > len {
            w.warnings.push(format!("file truncated: KLV at byte {pos} needs {} bytes, {} left", vlen, len.saturating_sub(vstart)));
            if class == KeyClass::Essence && vstart < len && len - vstart > 0 && is_essence_item(&key) {
                // keep a truncated clip-wrapped element's whole edit units
                w.elements.push(Element { key, value_offset: vstart, size: len - vstart, truncated: true });
            }
            break;
        }
        let read_value = || -> Result<Vec<u8>> {
            if vlen > MAX_META_VALUE {
                return Err(Error::Invalid(format!("metadata value of {vlen} bytes at {pos}")));
            }
            Ok(read_upto(src, vstart, vlen as usize)?)
        };
        match class {
            KeyClass::Partition { kind, status } => {
                let v = read_value()?;
                match Partition::parse(pos, kind, status, &v) {
                    Some(p) => w.partitions.push(p),
                    None => w.warnings.push(format!("short partition pack at byte {pos}")),
                }
                cur_meta = None;
            }
            KeyClass::Primer => {
                let v = read_value()?;
                if let Some(pi) = w.partitions.len().checked_sub(1) {
                    w.metadata.push((pi, Primer::parse(&v), Vec::new()));
                    cur_meta = Some(w.metadata.len() - 1);
                }
            }
            KeyClass::LocalSet => {
                if let Some(m) = cur_meta {
                    let v = read_value()?;
                    w.metadata[m].2.push((key, v));
                }
            }
            KeyClass::IndexSegment => {
                let v = read_value()?;
                w.index.push(IndexSegment::parse(&v));
                cur_meta = None;
            }
            KeyClass::Essence => {
                cur_meta = None;
                if let Some(p) = w.partitions.last_mut()
                    && p.essence_start.is_none()
                {
                    p.essence_start = Some(pos);
                }
                if is_essence_item(&key) {
                    w.elements.push(Element { key, value_offset: vstart, size: vlen, truncated: false });
                }
            }
            KeyClass::Fill | KeyClass::RandomIndexPack | KeyClass::Other => {}
        }
        pos = vend;
    }
    if w.partitions.is_empty() {
        return Err(Error::NotMxf("no readable partition pack".into()));
    }
    Ok(w)
}

/// Generic container picture / sound / data / compound items (not system items).
fn is_essence_item(k: &Ul) -> bool {
    matches!(k.0[12], 0x05 | 0x06 | 0x07 | 0x15 | 0x16 | 0x17 | 0x18)
}

/// The next partition pack key at or after `from` (scans at most 16 MiB and shares `budget`
/// with every other recovery attempt in this file).
fn resync(src: &(impl ByteSource + ?Sized), from: u64, len: u64, budget: &mut usize) -> Result<Option<u64>> {
    let mut at = from;
    let end = len.min(from + (16 << 20));
    while at < end && *budget > 0 {
        let chunk = read_upto(src, at, (1 << 20).min(*budget))?;
        *budget = budget.saturating_sub(chunk.len());
        if chunk.len() < 16 {
            break;
        }
        for i in 0..=chunk.len() - 16 {
            if chunk[i..i + 13] == crate::klv::PARTITION_PREFIX && (2..=4).contains(&chunk[i + 13]) {
                return Ok(Some(at + i as u64));
            }
        }
        at += (chunk.len() - 15) as u64;
    }
    Ok(None)
}

/// Data definition labels (ST 377-1 Annex / RP 224): 06 0E 2B 34 04 01 01 01 01 03 02 kk tt 00 00 00.
fn track_kind_of(def: Option<Ul>) -> Option<(bool, TrackKind)> {
    let d = def?;
    if !d.item_starts_with(&[0x01, 0x03, 0x02]) {
        return None;
    }
    match (d.0[11], d.0[12]) {
        (0x01, _) => Some((true, TrackKind::Data)),
        (0x02, 0x01) => Some((false, TrackKind::Picture)),
        (0x02, 0x02) => Some((false, TrackKind::Sound)),
        _ => Some((false, TrackKind::Data)),
    }
}

/// The structural components of a track's sequence (or the component itself).
fn components<'a>(md: &'a Metadata, track: &Set) -> (Option<&'a Set>, Vec<&'a Set>) {
    let Some(seq) = track.reference(tag::TRACK_SEQUENCE).and_then(|r| md.get(&r)) else { return (None, Vec::new()) };
    if seq.kind() == Some(st::SEQUENCE) {
        (Some(seq), seq.references(tag::STRUCTURAL_COMPONENTS).iter().filter_map(|r| md.get(r)).collect())
    } else {
        (Some(seq), vec![seq])
    }
}

fn timecode_of(c: &Set) -> Option<Timecode> {
    Some(Timecode {
        start: c.i(tag::START_TIMECODE)?,
        rounded_base: c.u(tag::ROUNDED_TIMECODE_BASE).unwrap_or(0) as u16,
        drop_frame: c.u(tag::DROP_FRAME).unwrap_or(0) != 0,
    })
}

/// The first timecode component of a package's timecode tracks.
fn package_timecode(md: &Metadata, pkg: &Set) -> Option<Timecode> {
    for r in pkg.references(tag::PACKAGE_TRACKS) {
        let Some(t) = md.get(&r) else { continue };
        let (_, comps) = components(md, t);
        if let Some(tc) = comps.iter().filter(|c| c.kind() == Some(st::TIMECODE_COMPONENT)).find_map(|c| timecode_of(c)) {
            return Some(tc);
        }
    }
    None
}

/// The descriptor of file package track `track_id`: the package descriptor, or the matching
/// sub-descriptor of a multiple descriptor.
fn descriptor_for<'a>(md: &'a Metadata, pkg: &Set, track_id: u32, kind: TrackKind) -> Option<&'a Set> {
    let d = md.get(&pkg.reference(tag::SOURCE_PACKAGE_DESCRIPTOR)?)?;
    if d.kind() != Some(st::MULTIPLE_DESCRIPTOR) {
        return Some(d);
    }
    let subs: Vec<&Set> = d.references(tag::SUB_DESCRIPTORS).iter().filter_map(|r| md.get(r)).collect();
    subs.iter().find(|s| s.u(tag::LINKED_TRACK_ID) == Some(track_id as i64)).or_else(|| subs.iter().find(|s| descriptor_kind(s) == Some(kind))).copied()
}

fn descriptor_kind(s: &Set) -> Option<TrackKind> {
    match s.kind()? {
        st::GENERIC_PICTURE_DESCRIPTOR | st::CDCI_DESCRIPTOR | st::RGBA_DESCRIPTOR | st::MPEG2_VIDEO_DESCRIPTOR => Some(TrackKind::Picture),
        st::GENERIC_SOUND_DESCRIPTOR | st::AES3_DESCRIPTOR | st::WAVE_DESCRIPTOR => Some(TrackKind::Sound),
        st::GENERIC_DATA_DESCRIPTOR => Some(TrackKind::Data),
        _ => None,
    }
}

fn picture_info(d: &Set) -> PictureInfo {
    let u = |t| d.u(t).unwrap_or(0) as u32;
    PictureInfo {
        stored_width: u(tag::STORED_WIDTH),
        stored_height: u(tag::STORED_HEIGHT),
        display_width: u(tag::DISPLAY_WIDTH),
        display_height: u(tag::DISPLAY_HEIGHT),
        frame_layout: u(tag::FRAME_LAYOUT) as u8,
        aspect_ratio: d.rational(tag::ASPECT_RATIO).unwrap_or_default(),
        component_depth: u(tag::COMPONENT_DEPTH),
        horizontal_subsampling: u(tag::HORIZONTAL_SUBSAMPLING),
        vertical_subsampling: u(tag::VERTICAL_SUBSAMPLING),
        black_ref_level: d.u(tag::BLACK_REF_LEVEL).map(|v| v as u32),
        white_ref_level: d.u(tag::WHITE_REF_LEVEL).map(|v| v as u32),
        alpha_depth: u(tag::ALPHA_SAMPLE_DEPTH),
        rgba: d.kind() == Some(st::RGBA_DESCRIPTOR),
        picture_coding: d.ul(tag::PICTURE_ESSENCE_CODING),
        transfer_characteristic: d.ul(tag::TRANSFER_CHARACTERISTIC),
        coding_equations: d.ul(tag::CODING_EQUATIONS),
        color_primaries: d.ul(tag::COLOR_PRIMARIES),
    }
}

fn sound_info(d: &Set) -> SoundInfo {
    SoundInfo {
        sample_rate: d.rational(tag::AUDIO_SAMPLING_RATE).filter(Rational::is_valid).or_else(|| d.rational(tag::SAMPLE_RATE)).unwrap_or_default(),
        channels: d.u(tag::CHANNEL_COUNT).unwrap_or(1) as u32,
        bits: d.u(tag::QUANTIZATION_BITS).unwrap_or(16) as u32,
        block_align: d.u(tag::BLOCK_ALIGN).unwrap_or(0) as u32,
        locked: d.u(tag::LOCKED).map(|v| v != 0),
        sound_coding: d.ul(tag::SOUND_ESSENCE_CODING),
    }
}

/// Resolved track before the sample tables are built.
struct TrackRef {
    track_id: u32,
    track_number: u32,
    kind: TrackKind,
    name: Option<String>,
    edit_rate: Rational,
    origin: i64,
    start_position: i64,
    duration: Option<i64>,
    descriptor: Option<Arc<Set>>,
    sub_descriptors: Vec<Arc<Set>>,
    body_sid: u32,
    index_sid: u32,
}

fn umid_of(s: &Set, t: u16) -> Option<[u8; 32]> {
    s.get(t).and_then(|v| v.get(..32)).map(|b| b.try_into().unwrap_or([0; 32]))
}

/// Material package tracks → file package tracks with descriptors.
fn resolve_tracks(md: &Metadata, warnings: &mut Vec<String>) -> (Vec<TrackRef>, Option<Timecode>, Option<String>) {
    let sources: Vec<&Set> = md.of_kind(st::SOURCE_PACKAGE).collect();
    let file_pkg = |umid: &[u8; 32]| sources.iter().find(|p| umid_of(p, tag::PACKAGE_UID).as_ref() == Some(umid)).copied();
    // EssenceContainerData: package → (body SID, index SID)
    let mut sids: HashMap<[u8; 32], (u32, u32)> = HashMap::new();
    for e in md.of_kind(st::ESSENCE_CONTAINER_DATA) {
        if let Some(u) = umid_of(e, tag::ESSENCE_CONTAINER_DATA_LINKED_PACKAGE) {
            sids.insert(u, (e.u(tag::BODY_SID).unwrap_or(0) as u32, e.u(tag::INDEX_SID).unwrap_or(0) as u32));
        }
    }
    let material = md.of_kind(st::MATERIAL_PACKAGE).next();
    let mut out = Vec::new();
    let mut descriptor_cache: HashMap<[u8; 16], Arc<Set>> = HashMap::new();
    let intern_descriptor = |set: &Set, cache: &mut HashMap<[u8; 16], Arc<Set>>| {
        if set.instance_uid == [0; 16] { Arc::new(set.clone()) } else { cache.entry(set.instance_uid).or_insert_with(|| Arc::new(set.clone())).clone() }
    };
    let mut timecode = material.and_then(|m| package_timecode(md, m));
    let name = material.and_then(|m| m.string(tag::PACKAGE_NAME));
    let mut file_track = |pkg: &Set, kind: Option<TrackKind>, id: Option<u32>, start: i64, duration: Option<i64>, name: Option<String>| -> Option<TrackRef> {
        let umid = umid_of(pkg, tag::PACKAGE_UID).unwrap_or([0; 32]);
        for r in pkg.references(tag::PACKAGE_TRACKS) {
            let Some(t) = md.get(&r) else { continue };
            let tid = t.u(tag::TRACK_ID).unwrap_or(0) as u32;
            if id.is_some_and(|i| i != tid) {
                continue;
            }
            let (seq, _) = components(md, t);
            let k = track_kind_of(seq.and_then(|s| s.ul(tag::DATA_DEFINITION)));
            let tk = match (k, kind) {
                (Some((false, k)), _) => k,
                (_, Some(k)) => k,
                _ => continue,
            };
            if tk == TrackKind::Data && k.is_some_and(|(tc, _)| tc) {
                continue;
            }
            let desc = descriptor_for(md, pkg, tid, tk);
            let descriptor = desc.map(|d| intern_descriptor(d, &mut descriptor_cache));
            let subs: Vec<Arc<Set>> = desc
                .and_then(|d| d.dynamic_item(&SUB_DESCRIPTORS_ITEM))
                .map(|v| {
                    crate::klv::batch_of(v)
                        .into_iter()
                        .filter_map(|b| b.get(..16))
                        .filter_map(|b| md.get(&b.try_into().unwrap_or([0; 16])))
                        .map(|s| intern_descriptor(s, &mut descriptor_cache))
                        .collect()
                })
                .unwrap_or_default();
            let (body_sid, index_sid) = sids.get(&umid).copied().unwrap_or((0, 0));
            return Some(TrackRef {
                track_id: tid,
                track_number: t.u(tag::TRACK_NUMBER).unwrap_or(0) as u32,
                kind: tk,
                name: name.clone().or_else(|| t.string(tag::TRACK_NAME)),
                edit_rate: t.rational(tag::EDIT_RATE).unwrap_or_default(),
                origin: t.i(tag::ORIGIN).unwrap_or(0),
                start_position: start,
                duration,
                descriptor,
                sub_descriptors: subs,
                body_sid,
                index_sid,
            });
        }
        None
    };
    if let Some(m) = material {
        for r in m.references(tag::PACKAGE_TRACKS) {
            let Some(t) = md.get(&r) else { continue };
            let (seq, comps) = components(md, t);
            let Some((false, kind)) = track_kind_of(seq.and_then(|s| s.ul(tag::DATA_DEFINITION))) else { continue };
            if kind == TrackKind::Data {
                continue;
            }
            let Some(clip) = comps.iter().find(|c| c.kind() == Some(st::SOURCE_CLIP)) else { continue };
            let Some(umid) = umid_of(clip, tag::SOURCE_PACKAGE_ID) else { continue };
            let Some(pkg) = file_pkg(&umid) else {
                warnings.push(format!("material track {} refers to a package not in this file", t.u(tag::TRACK_ID).unwrap_or(0)));
                continue;
            };
            let src_track = clip.u(tag::SOURCE_TRACK_ID).map(|v| v as u32);
            let duration = clip.i(tag::DURATION).or_else(|| seq.and_then(|s| s.i(tag::DURATION))).filter(|d| *d >= 0);
            // durations are in the material track's edit rate; convert when the file track's differs
            let mrate = t.rational(tag::EDIT_RATE).unwrap_or_default();
            if let Some(mut tr) = file_track(pkg, Some(kind), src_track, clip.i(tag::START_POSITION).unwrap_or(0), duration, t.string(tag::TRACK_NAME)) {
                if mrate.is_valid() && tr.edit_rate.is_valid() && mrate != tr.edit_rate {
                    let conv = |v: i64| (v as i128 * tr.edit_rate.num as i128 * mrate.den as i128 / (tr.edit_rate.den as i128 * mrate.num as i128)) as i64;
                    tr.duration = tr.duration.map(conv);
                    tr.start_position = conv(tr.start_position);
                }
                if timecode.is_none() {
                    timecode = package_timecode(md, pkg);
                }
                if !out.iter().any(|o: &TrackRef| o.track_id == tr.track_id && o.track_number == tr.track_number) {
                    out.push(tr);
                }
            }
        }
    }
    if out.is_empty() {
        // No usable material package: present the file packages' essence tracks directly.
        for pkg in &sources {
            if pkg.reference(tag::SOURCE_PACKAGE_DESCRIPTOR).is_none() {
                continue;
            }
            for r in pkg.references(tag::PACKAGE_TRACKS) {
                let Some(t) = md.get(&r) else { continue };
                let id = t.u(tag::TRACK_ID).unwrap_or(0) as u32;
                if let Some(tr) = file_track(pkg, None, Some(id), 0, None, None) {
                    out.push(tr);
                }
            }
            if timecode.is_none() {
                timecode = package_timecode(md, pkg);
            }
        }
    }
    (out, timecode, name)
}

/// Open an MXF file: walk the KLV structure and resolve the essence tracks.
pub fn open(src: &(impl ByteSource + ?Sized)) -> Result<MxfFile> {
    let w = walk(src)?;
    let mut warnings = w.warnings;
    // Metadata of the most complete partition (closed complete > open complete > closed > open;
    // later partitions win ties: the footer has the final durations).
    let best = w
        .metadata
        .iter()
        .enumerate()
        .filter(|(_, m)| !m.2.is_empty())
        .max_by_key(|(_, m)| {
            let p = &w.partitions[m.0];
            (p.complete(), p.closed(), p.offset)
        })
        .map(|(i, _)| i);
    let Some(best) = best else { return Err(Error::Invalid("no header metadata".into())) };
    let (meta_partition, primer, sets) = &w.metadata[best];
    let mut md = Metadata::default();
    for (k, v) in sets {
        md.push(Set::parse(*k, v, primer));
    }
    let preface = md.of_kind(st::PREFACE).next();
    let op_label = preface.and_then(|p| p.ul(tag::OPERATIONAL_PATTERN)).unwrap_or(w.partitions[0].operational_pattern);
    let ident = md.of_kind(st::IDENTIFICATION).last();
    let product_name = ident.and_then(|i| i.string(tag::PRODUCT_NAME));
    let company_name = ident.and_then(|i| i.string(tag::COMPANY_NAME));
    let (refs, timecode, material_name) = resolve_tracks(&md, &mut warnings);
    if refs.is_empty() {
        return Err(Error::Invalid("no essence tracks in the header metadata".into()));
    }
    let mut tracks = Vec::new();
    let mut claimed: Vec<[u8; 4]> = Vec::new();
    let all_numbers: Vec<[u8; 4]> = {
        let mut v: Vec<[u8; 4]> = w.elements.iter().map(|e| e.key.0[12..16].try_into().unwrap_or([0; 4])).collect();
        v.dedup();
        v.sort_unstable();
        v.dedup();
        v
    };
    for r in &refs {
        let number = r.track_number.to_be_bytes();
        let mut key4 = all_numbers.iter().find(|n| **n == number).copied();
        if key4.is_none() {
            // Track number missing or not matching (some OP-Atom writers): the only unclaimed
            // element stream of the right kind.
            let kind_items: &[u8] = match r.kind {
                TrackKind::Picture => &[0x05, 0x15],
                TrackKind::Sound => &[0x06, 0x16],
                TrackKind::Data => &[0x07, 0x17],
            };
            let cands: Vec<[u8; 4]> = all_numbers.iter().filter(|n| kind_items.contains(&n[0]) && !claimed.contains(n)).copied().collect();
            if cands.len() == 1 {
                key4 = Some(cands[0]);
            }
        }
        let Some(key4) = key4 else {
            warnings.push(format!("no essence in this file for track {} (number {:08x})", r.track_id, r.track_number));
            continue;
        };
        claimed.push(key4);
        let elements: Vec<Element> = w.elements.iter().filter(|e| e.key.0[12..16] == key4).copied().collect();
        tracks.push(build_track(src, r, &elements, key4, &w.index, &mut warnings)?);
    }
    if tracks.is_empty() {
        return Err(Error::Unsupported("no essence of the material package is stored in this file".into()));
    }
    Ok(MxfFile {
        run_in: w.run_in,
        operational_pattern: OperationalPattern::from_ul(&op_label),
        operational_pattern_label: op_label,
        index_segments: w.index,
        partitions: w.partitions.clone(),
        tracks,
        timecode,
        material_package_name: material_name,
        product_name,
        company_name,
        metadata_partition: *meta_partition,
        warnings,
        file_size: src.len(),
    })
}

/// Index entries per stored edit unit for an index SID (all segments when `sid` is 0 or unmatched).
fn index_entries(index: &[IndexSegment], sid: u32) -> (std::collections::BTreeMap<usize, IndexEntry>, u32) {
    let any = index.iter().any(|s| s.index_sid == sid);
    let segs: Vec<&IndexSegment> = index.iter().filter(|s| !any || s.index_sid == sid).collect();
    // ST 377-1 segments use absolute edit-unit positions, which need not start at zero.
    // Keep positions sparse so even hostile gaps allocate only for real (already parsed) entries.
    let mut out = std::collections::BTreeMap::new();
    let mut eubc = 0;
    for s in segs {
        if s.edit_unit_byte_count > 0 {
            eubc = s.edit_unit_byte_count;
        }
        for (k, e) in s.entries.iter().enumerate() {
            let Some(position) = s.start_position.checked_add(k as i64) else { break };
            let Ok(p) = usize::try_from(position) else { continue };
            out.insert(p, *e);
        }
    }
    (out, eubc)
}

fn build_track(
    src: &(impl ByteSource + ?Sized),
    r: &TrackRef,
    elements: &[Element],
    key4: [u8; 4],
    index: &[IndexSegment],
    warnings: &mut Vec<String>,
) -> Result<EssenceTrack> {
    let desc = r.descriptor.as_ref();
    let container = desc.and_then(|d| d.ul(tag::ESSENCE_CONTAINER));
    let mut t = EssenceTrack {
        track_id: r.track_id,
        track_number: u32::from_be_bytes(key4),
        kind: r.kind,
        name: r.name.clone(),
        edit_rate: r.edit_rate,
        origin: r.origin,
        start_position: r.start_position,
        duration: r.duration,
        codec: Codec::Unknown,
        wrapping: Wrapping::Frame,
        essence_container: container,
        picture: None,
        sound: None,
        sound_format: SoundFormat::Other,
        body_sid: r.body_sid,
        index_sid: r.index_sid,
        samples: Vec::new(),
        display_order: Vec::new(),
        chunks: Vec::new(),
        temporal_offsets: false,
        needs_reorder: false,
        indexed: false,
    };
    match r.kind {
        TrackKind::Picture => {
            let pi = desc.map(|d| picture_info(d)).unwrap_or_default();
            let mpeg_desc = desc.is_some_and(|d| d.kind() == Some(st::MPEG2_VIDEO_DESCRIPTOR));
            let mut codec = identify_picture(pi.picture_coding, container, mpeg_desc);
            if r.sub_descriptors.iter().any(|s| s.kind() == Some(st::AVC_SUBDESCRIPTOR)) && !matches!(codec, Codec::Avc { .. }) {
                codec = Codec::Avc { intra: false };
            }
            // A wrong or missing label: trust the essence bytes.
            if let Some(first) = elements.first() {
                let head = read_upto(src, first.value_offset, 64)?;
                if let Some(sniffed) = sniff_picture(&head) {
                    let agree = std::mem::discriminant(&sniffed) == std::mem::discriminant(&codec);
                    if !agree && !matches!(codec, Codec::Avc { intra: true }) {
                        codec = sniffed;
                    }
                }
            }
            // CinemaDNG §7.1.1.2: a "TIFF" picture element type names DNG pictures
            if codec == Codec::Unknown && elements.first().is_some_and(|e| e.key.0[12] == 0x15 && e.key.0[14] == crate::essence::CINEMADNG_ELEMENT_TYPE) {
                codec = Codec::CinemaDng;
            }
            t.codec = codec;
            t.picture = Some(pi);
            build_pictures(src, &mut t, elements, index, warnings)?;
        }
        TrackKind::Sound => {
            let si = desc.map(|d| sound_info(d)).unwrap_or(SoundInfo { channels: 1, bits: 16, ..Default::default() });
            let d10 = container.is_some_and(|c| c.item_starts_with(&[0x0D, 0x01, 0x03, 0x01, 0x02, 0x01]));
            let aes_element = d10 || key4[0] == 0x06 && key4[2] == 0x10;
            let pcm_desc = desc.is_some_and(|d| matches!(d.kind(), Some(st::AES3_DESCRIPTOR) | Some(st::WAVE_DESCRIPTOR)));
            let pcm_coding = si.sound_coding.is_none_or(|c| !c.is_smpte() || c.item_starts_with(&[0x04, 0x02, 0x02, 0x01]) || c.0 == [0; 16]);
            t.sound_format = if aes_element {
                SoundFormat::Aes3Element
            } else if pcm_desc || pcm_coding {
                SoundFormat::Pcm
            } else {
                SoundFormat::Other
            };
            t.codec = match t.sound_format {
                SoundFormat::Aes3Element => Codec::Aes3Element,
                SoundFormat::Pcm => Codec::Pcm,
                SoundFormat::Other => Codec::Unknown,
            };
            let fb = si.frame_bytes() as u64;
            let mut first = 0u64;
            for e in elements {
                let n = match t.sound_format {
                    SoundFormat::Aes3Element => e.size.saturating_sub(4) / 32,
                    _ => e.size / fb.max(1),
                };
                t.chunks.push(Chunk { offset: e.value_offset, size: e.size, first_sample: first, samples: n });
                first += n;
            }
            t.wrapping = if elements.len() == 1 && r.duration.is_some_and(|d| d > 1) { Wrapping::Clip } else { Wrapping::Frame };
            t.sound = Some(si);
        }
        TrackKind::Data => {}
    }
    Ok(t)
}

fn build_pictures(
    src: &(impl ByteSource + ?Sized),
    t: &mut EssenceTrack,
    elements: &[Element],
    index: &[IndexSegment],
    warnings: &mut Vec<String>,
) -> Result<()> {
    let (entries, eubc) = index_entries(index, t.index_sid);
    let expected = t.duration.map(|d| d.saturating_add(t.first_edit_unit()).max(0) as usize);
    let clip = elements.len() == 1 && expected.is_none_or(|n| n > 1) && (eubc > 0 || entries.len() > 1);
    let mut samples: Vec<Sample> = Vec::new();
    if clip {
        t.wrapping = Wrapping::Clip;
        let e = elements[0];
        if eubc > 0 {
            let n = e.size / eubc as u64;
            for i in 0..n {
                samples.push(Sample { offset: e.value_offset + i * eubc as u64, size: eubc, pts: i as i64, key: true, b_picture: false });
            }
        } else {
            let offs: Vec<u64> =
                entries.iter().enumerate().map_while(|(position, (&indexed, entry))| (position == indexed).then_some(entry.stream_offset)).collect();
            let base = offs.first().copied().unwrap_or(0);
            for (i, o) in offs.iter().enumerate() {
                let start = o - base.min(*o);
                let end = offs.get(i + 1).map_or(e.size, |n| n - base.min(*n)).min(e.size);
                if end <= start {
                    break;
                }
                samples.push(Sample { offset: e.value_offset + start, size: (end - start) as u32, pts: i as i64, key: true, b_picture: false });
            }
        }
    } else {
        for (i, e) in elements.iter().filter(|e| !e.truncated).enumerate() {
            samples.push(Sample { offset: e.value_offset, size: e.size.min(u32::MAX as u64) as u32, pts: i as i64, key: true, b_picture: false });
        }
    }
    let n = samples.len();
    // Key frames and B pictures from the index.
    let flagged = entries.values().any(|e| e.random_access());
    if !t.codec.intra_only() {
        if flagged {
            t.indexed = true;
            for (i, s) in samples.iter_mut().enumerate() {
                let e = entries.get(&i).copied();
                s.key = e.is_some_and(|e| e.random_access()) || i == 0;
                s.b_picture = e.is_some_and(|e| e.is_b());
            }
        } else {
            // No index flags: find the random-access pictures in the bitstream.
            if !samples.is_empty() {
                warnings.push("no index table with random-access flags: key frames found by scanning the essence".into());
            }
            for (i, s) in samples.iter_mut().enumerate() {
                let head = read_upto(src, s.offset, (s.size as usize).min(1024))?;
                s.key = i == 0 || is_random_access(t.codec, &head);
            }
        }
    }
    // Presentation order from the temporal offsets: display d is stored at d + TO[d].
    let mut display: Vec<usize> = (0..n).collect();
    let has_to = entries.range(..n).any(|(_, e)| e.temporal_offset != 0);
    if has_to {
        let mut pts = vec![i64::MIN; n];
        let mut ok = true;
        for d in 0..n {
            let to = entries.get(&d).copied().map_or(0, |e| e.temporal_offset as i64);
            let s = d as i64 + to;
            if s < 0 || s >= n as i64 || pts[s as usize] != i64::MIN {
                ok = false;
                break;
            }
            pts[s as usize] = d as i64;
            display[d] = s as usize;
        }
        if ok {
            t.temporal_offsets = true;
            for (s, p) in samples.iter_mut().zip(pts) {
                s.pts = p;
            }
        } else {
            warnings.push("inconsistent index temporal offsets: using stored order".into());
            display = (0..n).collect();
        }
    } else {
        t.needs_reorder = samples.iter().any(|s| s.b_picture);
    }
    t.samples = samples;
    t.display_order = display;
    Ok(())
}

/// Whether a picture can start decoding (no index): AVC IDR / recovery point, MPEG-2 sequence
/// header + I picture; other codecs: always.
fn is_random_access(codec: Codec, head: &[u8]) -> bool {
    match codec {
        Codec::Avc { .. } => {
            let mut i = 0;
            while i + 3 < head.len() {
                if head[i] == 0 && head[i + 1] == 0 && head[i + 2] == 1 {
                    let t = head[i + 3] & 0x1F;
                    if t == 5 {
                        return true;
                    }
                    if t == 1 {
                        return false;
                    }
                    i += 3;
                } else {
                    i += 1;
                }
            }
            false
        }
        Codec::Mpeg2 => head.windows(4).any(|w| w == [0, 0, 1, 0xB3]),
        _ => true,
    }
}

impl MxfFile {
    /// The first track of a kind.
    pub fn track_of_kind(&self, kind: TrackKind) -> Option<usize> {
        self.tracks.iter().position(|t| t.kind == kind && (!t.samples.is_empty() || !t.chunks.is_empty()))
    }

    /// The bytes of picture sample `i` (stored order) of track `track`.
    pub fn read_sample(&self, src: &(impl ByteSource + ?Sized), track: usize, i: usize) -> Result<Vec<u8>> {
        let t = self.tracks.get(track).ok_or_else(|| Error::Invalid(format!("no track {track}")))?;
        let s = t.samples.get(i).ok_or_else(|| Error::Invalid(format!("no sample {i}")))?;
        if u64::from(s.size) > src.len().saturating_sub(s.offset) {
            return Err(Error::Invalid("picture sample extends past the end of the source".into()));
        }
        let mut v = vec![0u8; s.size as usize];
        src.read_at(s.offset, &mut v)?;
        Ok(v)
    }

    /// `frames` sound sample frames from stored sample `start` of track `track`, planar f32
    /// (zero past the end of the essence).
    pub fn read_pcm(&self, src: &(impl ByteSource + ?Sized), track: usize, start: u64, frames: usize) -> Result<Vec<Vec<f32>>> {
        let t = self.tracks.get(track).ok_or_else(|| Error::Invalid(format!("no track {track}")))?;
        let si = t.sound.as_ref().ok_or_else(|| Error::Invalid("not a sound track".into()))?;
        let ch = si.channels.max(1) as usize;
        if ch > 256 {
            return Err(Error::Invalid("sound track declares more than 256 channels".into()));
        }
        // Bound the output by what the file can actually hold (every stored sample frame takes at
        // least one byte per channel), plus slack for zero padding past the end: header-declared
        // sizes cannot force a giant allocation, while any legitimately long read still works.
        let backed = t.stored_sample_frames().saturating_sub(start).min(src.len() / ch as u64);
        let allowed = usize::try_from(backed).unwrap_or(usize::MAX).saturating_add(PCM_PAD_FRAMES_LIMIT / ch);
        if frames > allowed {
            return Err(Error::Invalid("sound request extends far past the stored essence".into()));
        }
        if t.sound_format == SoundFormat::Pcm && (si.bits == 0 || si.bits > 32) {
            return Err(Error::Unsupported("invalid PCM bit depth or block alignment".into()));
        }
        let fb = if t.sound_format == SoundFormat::Pcm { si.frame_bytes() } else { 0 };
        if t.sound_format == SoundFormat::Pcm && (fb == 0 || fb > 4096) {
            return Err(Error::Unsupported("invalid PCM block alignment".into()));
        }
        let mut out = vec![vec![0f32; frames]; ch];
        let end = start.saturating_add(frames as u64);
        let mut ci = t.chunks.partition_point(|c| c.first_sample.saturating_add(c.samples) <= start);
        while let Some(c) = t.chunks.get(ci) {
            if c.first_sample >= end {
                break;
            }
            let a = start.max(c.first_sample);
            let b = end.min(c.first_sample.saturating_add(c.samples));
            if b > a {
                let decoded = match t.sound_format {
                    SoundFormat::Pcm => {
                        let off = (a - c.first_sample)
                            .checked_mul(fb as u64)
                            .and_then(|n| c.offset.checked_add(n))
                            .ok_or_else(|| Error::Invalid("PCM chunk offset overflows".into()))?;
                        let bytes = ((b - a) as usize).checked_mul(fb).ok_or_else(|| Error::Invalid("PCM chunk size overflows".into()))?;
                        // Never allocate past the end of the source (a truncated or lying element).
                        let in_file = usize::try_from(src.len().saturating_sub(off)).unwrap_or(usize::MAX);
                        let bytes = bytes.min(in_file / fb * fb);
                        let mut v = vec![0u8; bytes];
                        src.read_at(off, &mut v)?;
                        decode_pcm(&v, SoundFormat::Pcm, ch, si.bits, fb)?
                    }
                    SoundFormat::Aes3Element => {
                        let size = c.size.min(src.len().saturating_sub(c.offset));
                        let size = usize::try_from(size).map_err(|_| Error::Invalid("AES3 chunk exceeds the address space".into()))?;
                        let mut v = vec![0u8; size];
                        src.read_at(c.offset, &mut v)?;
                        let all = decode_pcm(&v, SoundFormat::Aes3Element, ch, si.bits, 0)?;
                        let s0 = (a - c.first_sample) as usize;
                        all.into_iter().map(|p| p.get(s0..).map(<[f32]>::to_vec).unwrap_or_default()).collect()
                    }
                    SoundFormat::Other => return Err(Error::Unsupported("sound essence coding".into())),
                };
                let at = (a - start) as usize;
                for (c, d) in decoded.iter().enumerate().take(ch) {
                    for (k, v) in d.iter().take((b - a) as usize).enumerate() {
                        out[c][at + k] = *v;
                    }
                }
            }
            ci += 1;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod index_bounds_tests {
    use super::*;

    #[test]
    fn nonzero_segment_positions_and_sparse_gaps_preserve_entries() {
        let entry = IndexEntry { flags: 0x80, stream_offset: 1234, ..Default::default() };
        for start_position in [5, 49_999_999, i64::MAX] {
            let segment = IndexSegment { start_position, entries: vec![entry, entry], ..Default::default() };
            let (entries, _) = index_entries(&[segment], 0);
            let start = usize::try_from(start_position).unwrap();
            assert_eq!(entries.get(&start), Some(&entry));
            assert_eq!(entries.len(), if start_position == i64::MAX { 1 } else { 2 });
        }
    }

    #[test]
    fn negative_preroll_positions_do_not_drop_later_valid_entries() {
        let segment = IndexSegment { start_position: -1, entries: vec![IndexEntry::default(); 3], ..Default::default() };
        let (entries, _) = index_entries(&[segment], 0);
        assert_eq!(entries.keys().copied().collect::<Vec<_>>(), vec![0, 1]);
    }
}
