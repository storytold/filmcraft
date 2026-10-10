//! Matroska / WebM demuxer: EBML header, Segment walk, cluster scanning, packet reading, seeking.

use std::collections::{HashSet, VecDeque};

use crate::ebml::{self, Children, Lacing, Size, parse_lacing, read_id, read_size};
use crate::error::{Error, Result};
use crate::ids::*;
use crate::meta::*;
use crate::source::{ByteSource, ReadSeekSource};
use crate::track::{Sample, Track, TrackKind, gcd};

const WIN: usize = 64 * 1024;
/// Upper bound for level-1 metadata elements read into memory (Info, Tracks, Cues, Tags…).
const MAX_META: u64 = 256 << 20;

/// Options for [`open_with`] / [`Demuxer::with_options`].
#[derive(Clone, Debug)]
pub struct OpenOptions {
    /// Scan every cluster at open to build per-track sample tables and keyframe indices
    /// (reads only element and block headers, never frame payloads). Default `true`.
    ///
    /// With `false`, open stops at the first Cluster and uses the SeekHead to find Cues, Tags,
    /// Chapters… placed after the clusters; [`Demuxer::seek`] then uses Cues (or builds the index
    /// on first use if there are none).
    pub index: bool,
    /// Verify CRC-32 elements of level-1 elements (and clusters while indexing). Mismatches are
    /// recorded in [`MkvFile::crc_errors`]; parsing continues. Default `false`.
    pub verify_crc: bool,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self { index: true, verify_crc: false }
    }
}

/// A parsed Matroska / WebM file (first Segment).
#[derive(Clone, Debug)]
pub struct MkvFile {
    /// EBML `DocType`: `matroska` or `webm`.
    pub doc_type: String,
    pub doc_type_version: u64,
    pub doc_type_read_version: u64,
    /// Absolute offset of the Segment's data (base for SeekHead and Cues positions).
    pub segment_data_start: u64,
    /// Absolute end of the Segment (file end for unknown-size / live segments).
    pub segment_end: u64,
    /// Segment size field was "unknown" (live / streamed output).
    pub live: bool,
    pub info: SegmentInfo,
    pub tracks: Vec<Track>,
    pub seek_head: Vec<SeekEntry>,
    /// Cue points, sorted by time.
    pub cues: Vec<CuePoint>,
    /// Clusters in file order (complete only when [`MkvFile::indexed`]).
    pub clusters: Vec<ClusterInfo>,
    pub chapters: Vec<Edition>,
    pub tags: Vec<Tag>,
    pub attachments: Vec<Attachment>,
    /// Absolute offset of the first Cluster.
    pub first_cluster: Option<u64>,
    /// Sample tables have been built by scanning all clusters.
    pub indexed: bool,
    /// Number of CRC-32 elements verified (with `verify_crc`).
    pub crc_checked: u32,
    /// Absolute offsets of elements whose CRC-32 did not match.
    pub crc_errors: Vec<u64>,
    /// Non-fatal problems met while parsing (damage, resyncs, truncation).
    pub warnings: Vec<String>,
}

/// One demuxed frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Packet {
    /// Index into [`MkvFile::tracks`].
    pub track: usize,
    /// Matroska `TrackNumber`.
    pub track_number: u64,
    /// Presentation timestamp in timestamp ticks (the track timebase, [`Track::timebase`]).
    pub pts: i64,
    /// Presentation timestamp in nanoseconds.
    pub pts_ns: i64,
    /// Duration in ticks (0 if unknown).
    pub duration: u64,
    /// Duration in nanoseconds (0 if unknown).
    pub duration_ns: u64,
    pub keyframe: bool,
    pub discardable: bool,
    pub invisible: bool,
    /// `DiscardPadding` (ns) from the enclosing BlockGroup.
    pub discard_padding_ns: Option<i64>,
    /// Absolute offset of the stored frame data.
    pub offset: u64,
    /// Index of the frame within a laced block (0 when not laced). Laced frames after the first
    /// only have exact timestamps when the block or track carries a duration; otherwise they
    /// repeat the block timestamp and `duration` is 0.
    pub lace: u16,
    /// Frame bytes with header stripping undone. Frames of tracks with other content encodings
    /// (zlib, encryption) are returned as stored; see [`Track::frames_readable`].
    pub data: Vec<u8>,
}

/// Result of [`Demuxer::seek`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeekPoint {
    pub track: usize,
    /// Keyframe timestamp in ticks and ns.
    pub pts: i64,
    pub pts_ns: i64,
    /// Sample index of the keyframe when the file is indexed.
    pub sample: Option<usize>,
    pub cluster_offset: u64,
    pub block_offset: u64,
}

/// A keyframe index entry ([`Demuxer::keyframe_index`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Keyframe {
    pub sample: usize,
    pub pts: i64,
    pub pts_ns: i64,
    pub block_offset: u64,
}

// ---------------------------------------------------------------------------------------------
// I/O with a small read window
// ---------------------------------------------------------------------------------------------

#[derive(Default, Clone, Debug)]
struct Cache {
    buf: Vec<u8>,
    start: u64,
}

struct Io<'a, S: ByteSource + ?Sized> {
    src: &'a S,
    len: u64,
    cache: &'a mut Cache,
}

#[derive(Clone, Copy, Debug)]
struct Hdr {
    id: u32,
    pos: u64,
    size: Size,
    hlen: usize,
}

impl Hdr {
    fn data_start(&self) -> u64 {
        self.pos + self.hlen as u64
    }
    /// End offset, clamped to `limit`; unknown sizes run to `limit`.
    fn end(&self, limit: u64) -> u64 {
        match self.size {
            Size::Known(n) => self.data_start().saturating_add(n).min(limit),
            Size::Unknown => limit,
        }
    }
    fn overruns(&self, limit: u64) -> bool {
        matches!(self.size, Size::Known(n) if self.data_start().saturating_add(n) > limit)
    }
}

impl<'a, S: ByteSource + ?Sized> Io<'a, S> {
    fn new(src: &'a S, cache: &'a mut Cache) -> Self {
        let len = src.len();
        Self { src, len, cache }
    }

    /// Up to `n` (≤ WIN) bytes at `pos` (fewer at EOF).
    fn peek(&mut self, pos: u64, n: usize) -> Result<&[u8]> {
        let n = n.min(WIN);
        if pos >= self.len {
            return Ok(&[]);
        }
        let want_end = (pos + n as u64).min(self.len);
        let c = &*self.cache;
        if !(pos >= c.start && want_end <= c.start + c.buf.len() as u64) {
            let fill = ((self.len - pos) as usize).min(WIN);
            let mut buf = std::mem::take(&mut self.cache.buf);
            buf.resize(fill, 0);
            self.src.read_at(pos, &mut buf)?;
            self.cache.buf = buf;
            self.cache.start = pos;
        }
        let o = (pos - self.cache.start) as usize;
        Ok(&self.cache.buf[o..o + (want_end - pos) as usize])
    }

    /// Exactly `n` bytes at `pos`.
    fn read_vec(&mut self, pos: u64, n: u64) -> Result<Vec<u8>> {
        if pos.checked_add(n).is_none_or(|e| e > self.len) {
            return Err(Error::Io(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "read past end of file")));
        }
        if n as usize <= WIN / 2 {
            return Ok(self.peek(pos, n as usize)?.to_vec());
        }
        let mut v = vec![0u8; n as usize];
        self.src.read_at(pos, &mut v)?;
        Ok(v)
    }

    /// Element header at `pos` (`None` if undecodable or past `limit`).
    fn header(&mut self, pos: u64, limit: u64) -> Result<Option<Hdr>> {
        if pos >= limit {
            return Ok(None);
        }
        let b = self.peek(pos, 12)?;
        let Some((id, il)) = read_id(b) else { return Ok(None) };
        let Some((size, sl)) = read_size(&b[il..]) else { return Ok(None) };
        let hlen = il + sl;
        if pos + hlen as u64 > limit {
            return Ok(None);
        }
        Ok(Some(Hdr { id, pos, size, hlen }))
    }

    /// Scan forward from `from` for a plausible level-1 element (Cluster preferred), up to `limit`.
    fn resync(&mut self, from: u64, limit: u64) -> Result<Option<u64>> {
        let mut pos = from;
        while pos + 4 <= limit {
            let chunk = self.peek(pos, WIN)?.to_vec();
            if chunk.len() < 4 {
                return Ok(None);
            }
            for i in 0..chunk.len() - 3 {
                if chunk[i] & 0xF0 != 0x10 {
                    continue;
                }
                let id = u32::from_be_bytes([chunk[i], chunk[i + 1], chunk[i + 2], chunk[i + 3]]);
                if !is_top_level(id) || id == EBML || id == SEGMENT {
                    continue;
                }
                let p = pos + i as u64;
                if self.plausible(p, id, limit)? {
                    return Ok(Some(p));
                }
            }
            pos += (chunk.len() - 3) as u64;
        }
        Ok(None)
    }

    fn plausible(&mut self, p: u64, id: u32, limit: u64) -> Result<bool> {
        let Some(h) = self.header(p, limit)? else { return Ok(false) };
        if h.id != id || h.overruns(limit) {
            return Ok(false);
        }
        if h.size == Size::Unknown && id != CLUSTER {
            return Ok(false);
        }
        // first child must decode and fit; clusters must start with Timestamp (after CRC-32/Void)
        let mut cp = h.data_start();
        let end = h.end(limit);
        for _ in 0..3 {
            let Some(c) = self.header(cp, end)? else { return Ok(false) };
            if c.overruns(end) {
                return Ok(false);
            }
            if id != CLUSTER {
                return Ok(true);
            }
            match c.id {
                TIMESTAMP => return Ok(true),
                CRC32 | VOID => cp = c.end(end),
                _ => return Ok(false),
            }
        }
        Ok(false)
    }

    /// End of an unknown-size master element: the first child that is a top-level ID or undecodable.
    fn unknown_end(&mut self, data_start: u64, limit: u64) -> Result<u64> {
        let mut pos = data_start;
        while pos < limit {
            let Some(h) = self.header(pos, limit)? else { return Ok(pos) };
            if is_top_level(h.id) || h.size == Size::Unknown {
                return Ok(pos);
            }
            pos = h.end(limit);
        }
        Ok(limit)
    }
}

// ---------------------------------------------------------------------------------------------
// Blocks
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct BlockRef {
    track: usize,
    block_offset: u64,
    /// Timestamp in ticks (cluster + relative).
    ts: i64,
    /// Absolute (offset, size) of each frame.
    frames: Vec<(u64, u32)>,
    block_duration: Option<u64>,
    keyframe: bool,
    discardable: bool,
    invisible: bool,
    discard_padding: Option<i64>,
    /// (offset, size) of the `BlockAdditional` with `BlockAddID` 1 (WebM: the VP8/VP9 alpha
    /// layer), kept for unlaced blocks only.
    addition: Option<(u64, u32)>,
}

#[derive(Clone, Copy, Debug)]
struct FrameRef {
    track: usize,
    offset: u64,
    size: u32,
    pts: i64,
    pts_ns: i64,
    duration: u64,
    duration_ns: u64,
    keyframe: bool,
    discardable: bool,
    invisible: bool,
    discard_padding: Option<i64>,
    block_offset: u64,
    lace: u16,
    addition: Option<(u64, u32)>,
}

fn track_lookup(tracks: &[Track]) -> impl Fn(u64) -> Option<usize> + '_ {
    move |n| tracks.iter().position(|t| t.number == n)
}

/// Parse the Block/SimpleBlock header at `data_start` (`len` bytes).
/// Returns (track index, relative ts, flags, frames) or `None` for unknown tracks / damage.
fn parse_block<S: ByteSource + ?Sized>(
    io: &mut Io<'_, S>,
    data_start: u64,
    len: u64,
    lookup: &dyn Fn(u64) -> Option<usize>,
) -> Result<Option<(usize, i16, u8, Vec<(u64, u32)>)>> {
    let head = io.peek(data_start, 16.min(len as usize))?;
    let Some((tn, tl, _)) = ebml::read_vint(head) else { return Ok(None) };
    if head.len() < tl + 3 {
        return Ok(None);
    }
    let rel = i16::from_be_bytes([head[tl], head[tl + 1]]);
    let flags = head[tl + 2];
    let Some(track) = lookup(tn) else { return Ok(None) };
    let hdr = (tl + 3) as u64;
    let lacing = Lacing::from_flags(flags);
    let frames = if lacing == Lacing::None {
        vec![(data_start + hdr, (len - hdr) as u32)]
    } else {
        let body = io.read_vec(data_start + hdr, len - hdr)?;
        match parse_lacing(lacing, &body) {
            Ok(f) => f.into_iter().map(|(o, l)| (data_start + hdr + o as u64, l as u32)).collect(),
            Err(_) => return Ok(None),
        }
    };
    Ok(Some((track, rel, flags, frames)))
}

/// The `BlockAdditional` with `BlockAddID` 1 (the default ID) in a `BlockAdditions` element:
/// (offset, size) of its data. Other IDs, empty additions and damaged children are skipped.
fn block_addition<S: ByteSource + ?Sized>(io: &mut Io<'_, S>, start: u64, end: u64) -> Result<Option<(u64, u32)>> {
    let mut pos = start;
    while pos < end {
        let Some(more) = io.header(pos, end)? else { break };
        let me = more.end(end);
        if more.id == BLOCK_MORE {
            let (mut id, mut data) = (1u64, None);
            let mut p = more.data_start();
            while p < me {
                let Some(c) = io.header(p, me)? else { break };
                let ce = c.end(me);
                let len = ce.saturating_sub(c.data_start());
                match c.id {
                    BLOCK_ADD_ID => id = ebml::uint(io.peek(c.data_start(), len.min(8) as usize)?),
                    BLOCK_ADDITIONAL => data = u32::try_from(len).ok().filter(|&n| n > 0).map(|n| (c.data_start(), n)),
                    _ => {}
                }
                p = ce;
            }
            if id == 1 && data.is_some() {
                return Ok(data);
            }
        }
        pos = me;
    }
    Ok(None)
}

/// Parse one SimpleBlock or BlockGroup element.
fn parse_block_elem<S: ByteSource + ?Sized>(
    io: &mut Io<'_, S>,
    h: &Hdr,
    end: u64,
    cluster_ts: i64,
    lookup: &dyn Fn(u64) -> Option<usize>,
) -> Result<Option<BlockRef>> {
    let ds = h.data_start();
    match h.id {
        SIMPLE_BLOCK => {
            let Some((track, rel, flags, frames)) = parse_block(io, ds, end - ds, lookup)? else { return Ok(None) };
            Ok(Some(BlockRef {
                track,
                block_offset: h.pos,
                ts: cluster_ts + rel as i64,
                frames,
                block_duration: None,
                keyframe: flags & 0x80 != 0,
                discardable: flags & 0x01 != 0,
                invisible: flags & 0x08 != 0,
                discard_padding: None,
                addition: None,
            }))
        }
        BLOCK_GROUP => {
            let mut pos = ds;
            let mut block = None;
            let (mut dur, mut has_ref, mut pad, mut addition) = (None, false, None, None);
            while pos < end {
                let Some(c) = io.header(pos, end)? else { break };
                let ce = c.end(end);
                match c.id {
                    BLOCK => block = parse_block(io, c.data_start(), ce - c.data_start(), lookup)?,
                    BLOCK_ADDITIONS => addition = block_addition(io, c.data_start(), ce)?,
                    BLOCK_DURATION => dur = Some(ebml::uint(io.peek(c.data_start(), (ce - c.data_start()) as usize)?)),
                    REFERENCE_BLOCK => has_ref = true,
                    DISCARD_PADDING => pad = Some(ebml::int(io.peek(c.data_start(), (ce - c.data_start()) as usize)?)),
                    _ => {}
                }
                pos = ce;
            }
            let Some((track, rel, flags, frames)) = block else { return Ok(None) };
            // with lacing, an addition belongs to the block, not to one of its frames
            let addition = addition.filter(|_| frames.len() == 1);
            Ok(Some(BlockRef {
                track,
                block_offset: h.pos,
                ts: cluster_ts + rel as i64,
                frames,
                block_duration: dur,
                keyframe: !has_ref,
                discardable: false,
                invisible: flags & 0x08 != 0,
                discard_padding: pad,
                addition,
            }))
        }
        _ => Ok(None),
    }
}

/// Expand a block into frames with per-lace timestamps.
fn expand(b: &BlockRef, t: &Track, scale: u64, skip: u16, out: &mut impl Extend<FrameRef>) {
    let n = b.frames.len() as u64;
    // total block duration in ns
    let block_ns: Option<u128> = match (b.block_duration, t.default_duration_ns) {
        (Some(d), _) => Some(d as u128 * scale as u128),
        (None, Some(dd)) => Some(dd as u128 * n as u128),
        _ => None,
    };
    let scale_i = scale as i128;
    // round half away from zero
    let to_ticks = |ns: i128| -> i64 { (if ns >= 0 { (ns + scale_i / 2) / scale_i } else { (ns - scale_i / 2) / scale_i }) as i64 };
    // CodecDelay is subtracted from every timestamp (RFC 9559 §5.1.4.1.25); in ticks it is rounded
    // once, so tick timestamps stay on the block grid.
    let delay_ns = t.codec_delay_ns as i128;
    let delay_ticks = to_ticks(delay_ns);
    let base_ns = b.ts as i128 * scale_i;
    out.extend(b.frames.iter().enumerate().skip(skip as usize).map(|(i, &(offset, size))| {
        let (start, end) = match block_ns {
            Some(total) => ((total * i as u128 / n as u128) as i128, Some((total * (i as u128 + 1) / n as u128) as i128)),
            None => (0, None),
        };
        let pts_ns = base_ns + start - delay_ns;
        let pts = to_ticks(base_ns + start) - delay_ticks;
        let duration = end.map_or(0, |e| (to_ticks(e) - to_ticks(start)).max(0) as u64);
        FrameRef {
            track: b.track,
            offset,
            size,
            pts,
            pts_ns: pts_ns as i64,
            duration,
            duration_ns: end.map_or(0, |e| (e - start).max(0) as u64),
            keyframe: b.keyframe,
            discardable: b.discardable,
            invisible: b.invisible,
            discard_padding: b.discard_padding,
            block_offset: b.block_offset,
            lace: i as u16,
            addition: b.addition,
        }
    }));
}

/// Scan a whole cluster (headers only), returning its info and blocks.
fn scan_cluster<S: ByteSource + ?Sized>(
    io: &mut Io<'_, S>,
    h: &Hdr,
    limit: u64,
    tracks: &[Track],
    warnings: &mut Vec<String>,
) -> Result<(ClusterInfo, Vec<BlockRef>)> {
    let lookup = track_lookup(tracks);
    let ds = h.data_start();
    let known_end = match h.size {
        Size::Known(_) => Some(h.end(limit)),
        Size::Unknown => None,
    };
    let lim = known_end.unwrap_or(limit);
    let mut ts: i64 = 0;
    let mut blocks = Vec::new();
    let mut pos = ds;
    let end = loop {
        if pos >= lim {
            break lim;
        }
        let Some(c) = io.header(pos, lim)? else {
            warnings.push(format!("damaged cluster at {}: undecodable element at {pos}", h.pos));
            break io.resync(pos + 1, limit)?.unwrap_or(limit);
        };
        if known_end.is_none() && is_top_level(c.id) {
            break pos;
        }
        if c.size == Size::Unknown || c.overruns(lim) {
            warnings.push(format!("damaged cluster at {}: element 0x{:X} at {pos} overruns", h.pos, c.id));
            if c.overruns(lim) && matches!(c.id, SIMPLE_BLOCK | BLOCK_GROUP) && io.len <= c.data_start() + c.size.known().unwrap_or(0) {
                break lim; // truncated file: drop the partial block
            }
            break io.resync(pos + 1, limit)?.unwrap_or(limit);
        }
        let ce = c.end(lim);
        match c.id {
            TIMESTAMP => ts = ebml::uint(io.peek(c.data_start(), (ce - c.data_start()) as usize)?) as i64,
            SIMPLE_BLOCK | BLOCK_GROUP => {
                if let Some(b) = parse_block_elem(io, &c, ce, ts, &lookup)? {
                    blocks.push(b);
                }
            }
            _ => {}
        }
        pos = ce;
    };
    Ok((ClusterInfo { offset: h.pos, data_start: ds, end, timestamp: ts.max(0) as u64 }, blocks))
}

// ---------------------------------------------------------------------------------------------
// Open
// ---------------------------------------------------------------------------------------------

/// Open a Matroska/WebM file with default options (full sample index).
pub fn open<S: ByteSource>(src: S) -> Result<MkvFile> {
    open_with(src, &OpenOptions::default())
}

/// Open with explicit options.
pub fn open_with<S: ByteSource>(src: S, opts: &OpenOptions) -> Result<MkvFile> {
    let mut cache = Cache::default();
    open_impl(&src, opts, &mut cache)
}

fn open_impl<S: ByteSource + ?Sized>(src: &S, opts: &OpenOptions, cache: &mut Cache) -> Result<MkvFile> {
    let mut io = Io::new(src, cache);
    let len = io.len;
    // EBML header (tolerate leading junk in the first 64 KiB)
    let start = {
        let b = io.peek(0, WIN)?;
        let magic = [0x1A, 0x45, 0xDF, 0xA3];
        b.windows(4).position(|w| w == magic).ok_or_else(|| Error::NotMatroska("no EBML header".into()))? as u64
    };
    let eh = io.header(start, len)?.ok_or_else(|| Error::NotMatroska("bad EBML header".into()))?;
    let hdr_end = eh.end(len);
    let hdr = io.read_vec(eh.data_start(), hdr_end - eh.data_start())?;
    let (mut doc_type, mut dtv, mut dtrv) = ("matroska".to_string(), 1, 1);
    for e in Children::new(&hdr) {
        match e.id {
            DOC_TYPE => doc_type = ebml::string(e.data),
            DOC_TYPE_VERSION => dtv = ebml::uint(e.data),
            DOC_TYPE_READ_VERSION => dtrv = ebml::uint(e.data),
            EBML_MAX_ID_LENGTH if ebml::uint(e.data) > 4 => return Err(Error::Unsupported("EBMLMaxIDLength > 4".into())),
            EBML_MAX_SIZE_LENGTH if ebml::uint(e.data) > 8 => return Err(Error::Unsupported("EBMLMaxSizeLength > 8".into())),
            _ => {}
        }
    }
    if doc_type != "matroska" && doc_type != "webm" {
        return Err(Error::NotMatroska(format!("DocType {doc_type:?}")));
    }
    // Segment
    let mut pos = hdr_end;
    let seg = loop {
        let h = io.header(pos, len)?.ok_or_else(|| Error::Invalid("no Segment".into()))?;
        if h.id == SEGMENT {
            break h;
        }
        if h.size == Size::Unknown {
            return Err(Error::Invalid("unknown-size element before Segment".into()));
        }
        pos = h.end(len);
    };
    let mut file = MkvFile {
        doc_type,
        doc_type_version: dtv,
        doc_type_read_version: dtrv,
        segment_data_start: seg.data_start(),
        segment_end: seg.end(len),
        live: seg.size == Size::Unknown,
        info: SegmentInfo::default(),
        tracks: Vec::new(),
        seek_head: Vec::new(),
        cues: Vec::new(),
        clusters: Vec::new(),
        chapters: Vec::new(),
        tags: Vec::new(),
        attachments: Vec::new(),
        first_cluster: None,
        indexed: false,
        crc_checked: 0,
        crc_errors: Vec::new(),
        warnings: Vec::new(),
    };
    if seg.overruns(len) {
        file.warnings.push("Segment extends past end of file (truncated)".into());
    }
    let mut w = Walker { opts, parsed: HashSet::new(), blocks_seen: false };
    w.walk(&mut io, &mut file)?;
    finish_tracks(&mut file);
    Ok(file)
}

struct Walker<'o> {
    opts: &'o OpenOptions,
    parsed: HashSet<u64>,
    blocks_seen: bool,
}

impl Walker<'_> {
    fn walk<S: ByteSource + ?Sized>(&mut self, io: &mut Io<'_, S>, file: &mut MkvFile) -> Result<()> {
        let seg_end = file.segment_end;
        let mut pos = file.segment_data_start;
        while pos < seg_end {
            let Some(h) = io.header(pos, seg_end)? else {
                match io.resync(pos + 1, seg_end)? {
                    Some(p) => {
                        file.warnings.push(format!("undecodable element at {pos}; resynced at {p}"));
                        pos = p;
                        continue;
                    }
                    None => break,
                }
            };
            match h.id {
                CLUSTER => {
                    if file.first_cluster.is_none() {
                        file.first_cluster = Some(h.pos);
                        // make sure Info/Tracks are known before interpreting blocks
                        self.follow_seek_head(io, file, &[INFO, TRACKS])?;
                        if !self.opts.index {
                            break;
                        }
                    }
                    let (ci, blocks) = scan_cluster(io, &h, seg_end, &file.tracks, &mut file.warnings)?;
                    if self.opts.verify_crc && h.size != Size::Unknown {
                        let data = io.read_vec(h.data_start(), ci.end - h.data_start())?;
                        self.check_crc(file, h.pos, &data);
                    }
                    add_blocks(file, file.clusters.len() as u32, &blocks);
                    self.blocks_seen = true;
                    file.clusters.push(ci);
                    pos = ci.end.max(h.data_start());
                }
                SEEK_HEAD | INFO | TRACKS | CUES | CHAPTERS | TAGS | ATTACHMENTS => {
                    pos = self.level1(io, file, &h, seg_end)?;
                }
                EBML | SEGMENT => break, // chained segment: only the first one is read
                _ => {
                    if h.size == Size::Unknown {
                        pos = io.unknown_end(h.data_start(), seg_end)?;
                    } else if h.overruns(seg_end) && h.id != VOID {
                        match io.resync(pos + 1, seg_end)? {
                            Some(p) => {
                                file.warnings.push(format!("oversized element 0x{:X} at {pos}; resynced at {p}", h.id));
                                pos = p;
                            }
                            None => break,
                        }
                    } else {
                        pos = h.end(seg_end);
                    }
                }
            }
        }
        self.follow_seek_head(io, file, &[INFO, TRACKS, CUES, CHAPTERS, TAGS, ATTACHMENTS, SEEK_HEAD])?;
        // follow a second-level SeekHead reached through the first one
        self.follow_seek_head(io, file, &[CUES, CHAPTERS, TAGS, ATTACHMENTS])?;
        file.cues.sort_by_key(|c| c.time);
        file.indexed = self.opts.index;
        Ok(())
    }

    fn follow_seek_head<S: ByteSource + ?Sized>(&mut self, io: &mut Io<'_, S>, file: &mut MkvFile, ids: &[u32]) -> Result<()> {
        let entries = file.seek_head.clone();
        for e in entries {
            if !ids.contains(&e.id) {
                continue;
            }
            let Some(abs) = file.segment_data_start.checked_add(e.position) else { continue };
            if self.parsed.contains(&abs) || abs >= file.segment_end {
                continue;
            }
            match io.header(abs, file.segment_end)? {
                Some(h) if h.id == e.id => {
                    self.level1(io, file, &h, file.segment_end)?;
                }
                _ => file.warnings.push(format!("SeekHead entry 0x{:X} -> {abs} does not point at that element", e.id)),
            }
        }
        Ok(())
    }

    fn check_crc(&self, file: &mut MkvFile, pos: u64, data: &[u8]) {
        if let Some(ok) = ebml::verify_crc(data) {
            file.crc_checked += 1;
            if !ok {
                file.crc_errors.push(pos);
                file.warnings.push(format!("CRC-32 mismatch in element at {pos}"));
            }
        }
    }

    /// Parse a level-1 metadata element; returns its end.
    fn level1<S: ByteSource + ?Sized>(&mut self, io: &mut Io<'_, S>, file: &mut MkvFile, h: &Hdr, limit: u64) -> Result<u64> {
        let end = if h.size == Size::Unknown { io.unknown_end(h.data_start(), limit)? } else { h.end(limit) };
        if h.overruns(limit) {
            file.warnings.push(format!("element 0x{:X} at {} truncated", h.id, h.pos));
        }
        if !self.parsed.insert(h.pos) {
            return Ok(end);
        }
        if h.id == ATTACHMENTS {
            self.attachments(io, file, h.data_start(), end)?;
            return Ok(end);
        }
        let n = end - h.data_start();
        if n > MAX_META {
            file.warnings.push(format!("element 0x{:X} at {} too large ({n} bytes); skipped", h.id, h.pos));
            return Ok(end);
        }
        let data = io.read_vec(h.data_start(), n)?;
        if self.opts.verify_crc {
            self.check_crc(file, h.pos, &data);
        }
        match h.id {
            SEEK_HEAD => file.seek_head.extend(parse_seekhead(&data)),
            INFO => file.info = parse_info(&data),
            TRACKS if file.tracks.is_empty() => {
                file.tracks = parse_tracks(&data, file.info.timestamp_scale);
            }
            CUES => parse_cues(&data, &mut file.cues),
            CHAPTERS => parse_chapters(&data, &mut file.chapters),
            TAGS => parse_tags(&data, &mut file.tags),
            _ => {}
        }
        Ok(end)
    }

    fn attachments<S: ByteSource + ?Sized>(&mut self, io: &mut Io<'_, S>, file: &mut MkvFile, start: u64, end: u64) -> Result<()> {
        let mut pos = start;
        while pos < end {
            let Some(h) = io.header(pos, end)? else { break };
            let he = h.end(end);
            if h.id == ATTACHED_FILE {
                let mut a = Attachment::default();
                let mut p = h.data_start();
                while p < he {
                    let Some(c) = io.header(p, he)? else { break };
                    let ce = c.end(he);
                    let n = ce - c.data_start();
                    if c.id == FILE_DATA {
                        a.data_offset = c.data_start();
                        a.data_size = n;
                    } else if n <= 4096 {
                        let d = io.peek(c.data_start(), n as usize)?;
                        match c.id {
                            FILE_NAME => a.name = ebml::string(d),
                            FILE_MEDIA_TYPE => a.media_type = ebml::string(d),
                            FILE_DESCRIPTION => a.description = Some(ebml::string(d)),
                            FILE_UID => a.uid = ebml::uint(d),
                            _ => {}
                        }
                    }
                    p = ce;
                }
                file.attachments.push(a);
            }
            pos = he;
        }
        Ok(())
    }
}

fn add_blocks(file: &mut MkvFile, cluster: u32, blocks: &[BlockRef]) {
    let scale = file.info.timestamp_scale;
    let mut frames: Vec<FrameRef> = Vec::new();
    for b in blocks {
        frames.clear();
        let t = &file.tracks[b.track];
        expand(b, t, scale, 0, &mut frames);
        let t = &mut file.tracks[b.track];
        for f in &frames {
            if f.keyframe {
                t.sync.push(t.samples.len() as u32);
            }
            t.samples.push(Sample {
                offset: f.offset,
                size: f.size,
                pts: f.pts,
                duration: f.duration,
                keyframe: f.keyframe,
                cluster,
                block_offset: f.block_offset,
                lace: f.lace,
                addition: f.addition,
            });
        }
    }
}

fn finish_tracks(file: &mut MkvFile) {
    let scale = file.info.timestamp_scale;
    let g = gcd(scale, 1_000_000_000);
    for t in &mut file.tracks {
        t.timebase = (scale / g, 1_000_000_000 / g);
    }
}

// ---------------------------------------------------------------------------------------------
// MkvFile helpers
// ---------------------------------------------------------------------------------------------

impl MkvFile {
    /// True for WebM (`DocType` = `webm`).
    pub fn is_webm(&self) -> bool {
        self.doc_type == "webm"
    }

    /// Segment duration in ns: `Info/Duration` if present, else the end of the last indexed sample.
    pub fn duration_ns(&self) -> Option<u64> {
        if let Some(d) = self.info.duration {
            return Some((d * self.info.timestamp_scale as f64).round() as u64);
        }
        let end = self.tracks.iter().flat_map(|t| t.samples.iter().map(|s| s.pts + s.duration as i64)).max()?;
        Some((end.max(0) as u64).saturating_mul(self.info.timestamp_scale))
    }

    /// Index of the track with this `TrackNumber`.
    pub fn track_by_number(&self, number: u64) -> Option<usize> {
        self.tracks.iter().position(|t| t.number == number)
    }

    /// Index of the first track of the given kind (preferring the default flag).
    pub fn track_of_kind(&self, kind: TrackKind) -> Option<usize> {
        let mut it = self.tracks.iter().enumerate().filter(|(_, t)| t.kind == kind);
        let first = it.clone().next().map(|x| x.0);
        it.find(|(_, t)| t.default).map(|x| x.0).or(first)
    }

    /// Read sample `index` of `track` (header stripping undone).
    pub fn read_sample<S: ByteSource + ?Sized>(&self, src: &S, track: usize, index: usize) -> Result<Vec<u8>> {
        let t = self.tracks.get(track).ok_or(Error::NoSuchTrack(track))?;
        let s = t.samples.get(index).ok_or(Error::NoSuchSample(index))?;
        if !t.frames_readable() {
            return Err(Error::Unsupported(format!("content encoding of track {} (compression/encryption)", t.number)));
        }
        let mut data = t.frame_prefix(s.size as u64);
        let at = data.len();
        data.resize(at + s.size as usize, 0);
        src.read_at(s.offset, &mut data[at..])?;
        Ok(data)
    }

    /// The `BlockAdditional` (ID 1) of sample `index` of `track`, `None` when the block has none
    /// (see [`Sample::addition`]). Stored as is: header stripping applies to frames only.
    pub fn read_sample_addition<S: ByteSource + ?Sized>(&self, src: &S, track: usize, index: usize) -> Result<Option<Vec<u8>>> {
        let t = self.tracks.get(track).ok_or(Error::NoSuchTrack(track))?;
        let s = t.samples.get(index).ok_or(Error::NoSuchSample(index))?;
        let Some((offset, size)) = s.addition else { return Ok(None) };
        if offset.checked_add(size as u64).is_none_or(|end| end > src.len()) {
            return Err(Error::Invalid("block addition past end of file".into()));
        }
        let mut data = vec![0; size as usize];
        src.read_at(offset, &mut data)?;
        Ok(Some(data))
    }

    /// Keyframe sample of `track` with the greatest pts ≤ `time_ns` (indexed files only).
    pub fn keyframe_before(&self, track: usize, time_ns: i64) -> Option<usize> {
        let t = self.tracks.get(track)?;
        t.keyframe_before_pts(t.ns_to_ticks(time_ns))
    }
}

// ---------------------------------------------------------------------------------------------
// Demuxer (streaming packets + seeking)
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct InCluster {
    ts: i64,
    end: Option<u64>,
}

/// Packet reader over a [`ByteSource`]: iterates frames in file order and seeks to keyframes.
pub struct Demuxer<S> {
    src: S,
    file: MkvFile,
    opts: OpenOptions,
    cache: Cache,
    pos: u64,
    cluster: Option<InCluster>,
    pending: VecDeque<FrameRef>,
    skip_laces: Option<(u64, u16)>,
    done: bool,
}

impl<R: std::io::Read + std::io::Seek> Demuxer<ReadSeekSource<R>> {
    /// Open from any `Read + Seek` stream.
    pub fn from_reader(r: R) -> Result<Self> {
        Self::new(ReadSeekSource::new(r)?)
    }
}

impl<'a> Demuxer<&'a [u8]> {
    /// Open from an in-memory byte slice.
    pub fn from_slice(data: &'a [u8]) -> Result<Self> {
        Self::new(data)
    }
}

impl<S: ByteSource> Demuxer<S> {
    /// Open with default options (full index).
    pub fn new(src: S) -> Result<Self> {
        Self::with_options(src, OpenOptions::default())
    }

    pub fn with_options(src: S, opts: OpenOptions) -> Result<Self> {
        let mut cache = Cache::default();
        let file = open_impl(&src, &opts, &mut cache)?;
        let pos = file.first_cluster.unwrap_or(file.segment_end);
        Ok(Self { src, file, opts, cache, pos, cluster: None, pending: VecDeque::new(), skip_laces: None, done: false })
    }

    pub fn file(&self) -> &MkvFile {
        &self.file
    }
    pub fn tracks(&self) -> &[Track] {
        &self.file.tracks
    }
    pub fn source(&self) -> &S {
        &self.src
    }
    pub fn into_parts(self) -> (S, MkvFile) {
        (self.src, self.file)
    }
    /// Segment duration in ns.
    pub fn duration_ns(&self) -> Option<u64> {
        self.file.duration_ns()
    }

    /// Scan all clusters and build sample tables (no-op if already indexed).
    pub fn build_index(&mut self) -> Result<()> {
        if self.file.indexed {
            return Ok(());
        }
        let opts = OpenOptions { index: true, verify_crc: self.opts.verify_crc };
        self.file = open_impl(&self.src, &opts, &mut self.cache)?;
        Ok(())
    }

    /// Keyframes of `track` (builds the index if needed).
    pub fn keyframe_index(&mut self, track: usize) -> Result<Vec<Keyframe>> {
        self.build_index()?;
        let t = self.file.tracks.get(track).ok_or(Error::NoSuchTrack(track))?;
        Ok(t.keyframes()
            .iter()
            .map(|&k| {
                let s = &t.samples[k as usize];
                Keyframe { sample: k as usize, pts: s.pts, pts_ns: t.ticks_to_ns(s.pts), block_offset: s.block_offset }
            })
            .collect())
    }

    /// Rewind to the first cluster.
    pub fn rewind(&mut self) {
        self.pos = self.file.first_cluster.unwrap_or(self.file.segment_end);
        self.cluster = None;
        self.pending.clear();
        self.skip_laces = None;
        self.done = false;
    }

    /// Read sample `index` of `track` (requires an index).
    pub fn read_sample(&self, track: usize, index: usize) -> Result<Vec<u8>> {
        self.file.read_sample(&self.src, track, index)
    }

    /// Position the packet stream at the keyframe of `track` with the greatest pts ≤ `time_ns`
    /// (or the track's first keyframe if none precedes it). The next [`Demuxer::next_packet`]
    /// call returns packets starting at that keyframe's block (all tracks).
    ///
    /// Uses the sample index when present, else Cues (refined by scanning the cued cluster), else
    /// builds the index first.
    pub fn seek(&mut self, track: usize, time_ns: i64) -> Result<SeekPoint> {
        let t = self.file.tracks.get(track).ok_or(Error::NoSuchTrack(track))?;
        let ticks = t.ns_to_ticks(time_ns);
        if !self.file.indexed {
            let tn = t.number;
            let cue = self
                .file
                .cues
                .iter()
                .filter(|c| c.time as i64 <= ticks)
                .filter_map(|c| c.positions.iter().find(|p| p.track == tn).map(|p| (c.time, *p)))
                .next_back()
                .or_else(|| self.file.cues.iter().find_map(|c| c.positions.iter().find(|p| p.track == tn).map(|p| (c.time, *p))));
            if let Some((_, cp)) = cue
                && let Some(sp) = self.seek_via_cue(track, ticks, cp.cluster_position)?
            {
                return Ok(sp);
            }
            self.build_index()?;
        }
        let t = &self.file.tracks[track];
        let k = t.keyframe_before_pts(ticks).ok_or_else(|| Error::Invalid(format!("track {} has no keyframes", t.number)))?;
        let s = t.samples[k];
        let c = self.file.clusters[s.cluster as usize];
        let sp = SeekPoint { track, pts: s.pts, pts_ns: t.ticks_to_ns(s.pts), sample: Some(k), cluster_offset: c.offset, block_offset: s.block_offset };
        self.position_at(c, s.block_offset, s.lace);
        Ok(sp)
    }

    fn position_at(&mut self, c: ClusterInfo, block_offset: u64, lace: u16) {
        self.pending.clear();
        self.done = false;
        self.pos = block_offset;
        let known = c.end;
        self.cluster = Some(InCluster { ts: c.timestamp as i64, end: Some(known) });
        self.skip_laces = (lace > 0).then_some((block_offset, lace));
    }

    fn seek_via_cue(&mut self, track: usize, ticks: i64, cluster_position: u64) -> Result<Option<SeekPoint>> {
        let abs = self.file.segment_data_start + cluster_position;
        let mut io = Io::new(&self.src, &mut self.cache);
        let Some(h) = io.header(abs, self.file.segment_end)? else { return Ok(None) };
        if h.id != CLUSTER {
            self.file.warnings.push(format!("cue points at {abs}, which is not a Cluster"));
            return Ok(None);
        }
        let mut warnings = Vec::new();
        let (ci, blocks) = scan_cluster(&mut io, &h, self.file.segment_end, &self.file.tracks, &mut warnings)?;
        let t = &self.file.tracks[track];
        let mut frames = Vec::new();
        for b in blocks.iter().filter(|b| b.track == track && b.keyframe) {
            expand(b, t, self.file.info.timestamp_scale, 0, &mut frames);
        }
        let best = frames.iter().filter(|f| f.pts <= ticks).max_by_key(|f| f.pts).or_else(|| frames.iter().min_by_key(|f| f.pts)).copied();
        let Some(f) = best else { return Ok(None) };
        let sp = SeekPoint { track, pts: f.pts, pts_ns: f.pts_ns, sample: None, cluster_offset: ci.offset, block_offset: f.block_offset };
        self.position_at(ci, f.block_offset, f.lace);
        Ok(Some(sp))
    }

    /// Next frame in file order, or `None` at the end of the Segment.
    pub fn next_packet(&mut self) -> Result<Option<Packet>> {
        loop {
            if let Some(f) = self.pending.pop_front() {
                return self.materialize(f).map(Some);
            }
            if self.done || !self.fill()? {
                self.done = true;
                return Ok(None);
            }
        }
    }

    fn materialize(&mut self, f: FrameRef) -> Result<Packet> {
        let t = &self.file.tracks[f.track];
        let mut data = if t.frames_readable() { t.frame_prefix(f.size as u64) } else { Vec::new() };
        let at = data.len();
        data.resize(at + f.size as usize, 0);
        let mut io = Io::new(&self.src, &mut self.cache);
        if (f.size as usize) <= WIN / 2 {
            let b = io.peek(f.offset, f.size as usize)?;
            if b.len() != f.size as usize {
                return Err(Error::Invalid("frame data past end of file".into()));
            }
            data[at..].copy_from_slice(b);
        } else {
            self.src.read_at(f.offset, &mut data[at..])?;
        }
        Ok(Packet {
            track: f.track,
            track_number: t.number,
            pts: f.pts,
            pts_ns: f.pts_ns,
            duration: f.duration,
            duration_ns: f.duration_ns,
            keyframe: f.keyframe,
            discardable: f.discardable,
            invisible: f.invisible,
            discard_padding_ns: f.discard_padding,
            offset: f.offset,
            lace: f.lace,
            data,
        })
    }

    /// Advance over elements until at least one frame is pending. Returns false at the end.
    fn fill(&mut self) -> Result<bool> {
        let seg_end = self.file.segment_end;
        let scale = self.file.info.timestamp_scale;
        let mut io = Io::new(&self.src, &mut self.cache);
        let lookup = track_lookup(&self.file.tracks);
        loop {
            if let Some(c) = self.cluster {
                let lim = c.end.unwrap_or(seg_end);
                if self.pos >= lim {
                    self.cluster = None;
                    continue;
                }
                let Some(h) = io.header(self.pos, lim)? else {
                    self.cluster = None;
                    match io.resync(self.pos + 1, seg_end)? {
                        Some(p) => {
                            self.pos = p;
                            continue;
                        }
                        None => return Ok(false),
                    }
                };
                if c.end.is_none() && is_top_level(h.id) {
                    self.cluster = None;
                    continue;
                }
                if h.size == Size::Unknown || h.overruns(lim) {
                    self.cluster = None;
                    match io.resync(self.pos + 1, seg_end)? {
                        Some(p) => {
                            self.pos = p;
                            continue;
                        }
                        None => return Ok(false),
                    }
                }
                let he = h.end(lim);
                self.pos = he;
                match h.id {
                    TIMESTAMP => {
                        let v = ebml::uint(io.peek(h.data_start(), (he - h.data_start()) as usize)?) as i64;
                        self.cluster = Some(InCluster { ts: v, end: c.end });
                    }
                    SIMPLE_BLOCK | BLOCK_GROUP => {
                        if let Some(b) = parse_block_elem(&mut io, &h, he, c.ts, &lookup)? {
                            let skip = match self.skip_laces.take() {
                                Some((off, n)) if off == h.pos => n,
                                _ => 0,
                            };
                            expand(&b, &self.file.tracks[b.track], scale, skip, &mut self.pending);
                            if !self.pending.is_empty() {
                                return Ok(true);
                            }
                        }
                    }
                    _ => {}
                }
                continue;
            }
            if self.pos >= seg_end {
                return Ok(false);
            }
            let Some(h) = io.header(self.pos, seg_end)? else {
                match io.resync(self.pos + 1, seg_end)? {
                    Some(p) => {
                        self.pos = p;
                        continue;
                    }
                    None => return Ok(false),
                }
            };
            match h.id {
                CLUSTER => {
                    self.cluster = Some(InCluster { ts: 0, end: h.size.known().map(|_| h.end(seg_end)) });
                    self.pos = h.data_start();
                }
                EBML | SEGMENT => return Ok(false),
                _ => {
                    self.pos = if h.size == Size::Unknown { io.unknown_end(h.data_start(), seg_end)? } else { h.end(seg_end) };
                    if self.pos <= h.pos {
                        return Ok(false);
                    }
                }
            }
        }
    }
}

impl<S: ByteSource> Iterator for Demuxer<S> {
    type Item = Result<Packet>;
    fn next(&mut self) -> Option<Result<Packet>> {
        match self.next_packet() {
            Ok(Some(p)) => Some(Ok(p)),
            Ok(None) => None,
            Err(e) => {
                self.done = true;
                Some(Err(e))
            }
        }
    }
}
