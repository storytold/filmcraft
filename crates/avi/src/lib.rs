//! Clean-room AVI demuxer, written from Microsoft's published AVI RIFF file reference and the
//! OpenDML AVI File Format Extensions 1.02.
//!
//! [`open`] reads the `hdrl` header list (`avih`, and per stream `strh` / `strf` / `strn` /
//! `indx`) of the first `RIFF AVI ` and builds a per-stream table of [`Chunk`]s from the best index
//! the file has: the OpenDML super index (`indx` → `ix##` standard indexes, which also cover the
//! `RIFF AVIX` extensions of files past 1 GB), else the AVI 1.0 `idx1` (plus a scan of any `AVIX`
//! extension it cannot see), else a scan of the `movi` lists. Frame data is read on demand with
//! [`AviFile::read_chunk`]. Truncated files (a recording cut off) open with what is there.
//!
//! Every size and count comes from the file and is treated as hostile: reads are bounded by the
//! file length, tables by the number of chunks the file could hold, and nesting by a fixed depth.
//!
//! Layer L0: no dependencies beyond `std`, no `unsafe`, builds for `wasm32-unknown-unknown`.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

mod source;
#[cfg(test)]
mod tests;

pub use source::ByteSource;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Not an AVI file (no `RIFF` … `AVI ` header).
    NotAvi,
    /// A read failed.
    Io(String),
    /// Structurally invalid data.
    Invalid(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotAvi => f.write_str("not an AVI file"),
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::Invalid(e) => write!(f, "invalid AVI: {e}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// Whether `head` starts like an AVI file: `RIFF` <size> `AVI `.
pub fn sniff(head: &[u8]) -> bool {
    head.get(..4) == Some(b"RIFF") && head.get(8..12) == Some(b"AVI ")
}

/// `avih`: the main header (frame period, flags, frame count, size).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MainHeader {
    pub micro_sec_per_frame: u32,
    pub flags: u32,
    pub total_frames: u32,
    pub streams: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamKind {
    Video,
    Audio,
    /// Text, MIDI or anything else (`fccType`).
    Other([u8; 4]),
}

/// `strf` of a video stream: a `BITMAPINFOHEADER`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BitmapInfo {
    pub width: i32,
    /// Positive: rows stored bottom-up (uncompressed RGB); negative: top-down.
    pub height: i32,
    pub bit_count: u16,
    /// `biCompression` as four bytes: a FourCC (`MJPG`, `H264`, `YUY2`…) or 0 / 3 (`BI_RGB`,
    /// `BI_BITFIELDS`) in little-endian.
    pub compression: [u8; 4],
    pub size_image: u32,
    /// Bytes after the 40-byte header (codec setup, palette).
    pub extra: Vec<u8>,
}

impl BitmapInfo {
    /// Uncompressed RGB (`BI_RGB` / `BI_BITFIELDS`).
    pub fn is_rgb(&self) -> bool {
        matches!(u32::from_le_bytes(self.compression), 0 | 3)
    }
}

/// `strf` of an audio stream: a `WAVEFORMATEX` (`WAVEFORMATEXTENSIBLE` is resolved to its sub-format).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WaveFormat {
    /// 1 PCM, 3 IEEE float, 0x50 MPEG audio, 0x55 MP3, 0x2000 AC-3, 0xFF AAC…
    pub format_tag: u16,
    pub channels: u16,
    pub sample_rate: u32,
    pub avg_bytes_per_sec: u32,
    pub block_align: u16,
    pub bits_per_sample: u16,
    /// `cbSize` bytes after the 18-byte header.
    pub extra: Vec<u8>,
}

/// One stored frame or audio chunk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Chunk {
    /// Offset of the data (after the 8-byte chunk header).
    pub offset: u64,
    pub size: u32,
    /// A key frame (always true for audio and when the index does not say).
    pub key: bool,
}

/// Where the chunk tables came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexSource {
    /// OpenDML `indx` super index and `ix##` standard indexes.
    OpenDml,
    /// The AVI 1.0 `idx1` (plus a scan of the `AVIX` extensions).
    Idx1,
    /// No usable index: the `movi` lists were scanned (key frames unknown).
    Scan,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stream {
    pub kind: StreamKind,
    /// `fccHandler`.
    pub handler: [u8; 4],
    /// Time base: `rate / scale` units per second (frames per second for video).
    pub scale: u32,
    pub rate: u32,
    /// Start time in `scale / rate` units.
    pub start: u32,
    /// Length in `scale / rate` units.
    pub length: u32,
    /// Bytes per sample for fixed-size samples (PCM block align), 0 when samples vary.
    pub sample_size: u32,
    pub video: Option<BitmapInfo>,
    pub audio: Option<WaveFormat>,
    /// `strn`.
    pub name: Option<String>,
    pub chunks: Vec<Chunk>,
    /// Whether [`Chunk::key`] comes from an index (a scan cannot tell).
    pub keyframes_known: bool,
    /// The OpenDML super index entries (offsets of the `ix##` chunks).
    super_index: Vec<u64>,
}

impl Stream {
    /// Stream index of a `movi` chunk id (`00dc`, `01wb`…): its first two characters.
    fn number(id: &[u8; 4]) -> Option<usize> {
        let d = |b: u8| (b as char).to_digit(16).map(|v| v as usize);
        Some(d(id[0])? * 16 + d(id[1])?)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AviFile {
    pub header: MainHeader,
    pub streams: Vec<Stream>,
    pub index: IndexSource,
}

/// Options for [`open_with`] (tests reach the fallback indexes with them).
#[derive(Clone, Copy, Debug, Default)]
pub struct OpenOptions {
    pub ignore_odml: bool,
    pub ignore_idx1: bool,
}

/// Headers larger than this are refused (a real `hdrl` is a few kilobytes).
const MAX_HEADER: u64 = 16 << 20;
/// Streams beyond this are ignored.
const MAX_STREAMS: usize = 64;
/// `LIST rec ` inside `movi` nests once; deeper is refused.
const MAX_DEPTH: u32 = 3;

fn io(e: std::io::Error) -> Error {
    Error::Io(e.to_string())
}

fn read<S: ByteSource + ?Sized>(src: &S, at: u64, n: u64) -> Result<Vec<u8>> {
    let end = at.checked_add(n).filter(|e| *e <= src.len()).ok_or_else(|| Error::Invalid(format!("{n} bytes at {at} run past the end")))?;
    let len = usize::try_from(end - at).map_err(|_| Error::Invalid("chunk too large".into()))?;
    let mut buf = vec![0; len];
    src.read_at(at, &mut buf).map_err(io)?;
    Ok(buf)
}

fn fourcc(b: &[u8], at: usize) -> [u8; 4] {
    let mut f = [0; 4];
    if let Some(s) = b.get(at..at + 4) {
        f.copy_from_slice(s);
    }
    f
}

fn le16(b: &[u8], at: usize) -> u16 {
    b.get(at..at + 2).map_or(0, |s| u16::from_le_bytes([s[0], s[1]]))
}

fn le32(b: &[u8], at: usize) -> u32 {
    b.get(at..at + 4).map_or(0, |s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn le64(b: &[u8], at: usize) -> u64 {
    u64::from(le32(b, at)) | (u64::from(le32(b, at + 4)) << 32)
}

/// A chunk header at `at`: (id, size, data offset). The size is clamped to the file (a truncated
/// recording keeps what is there).
fn header_at<S: ByteSource + ?Sized>(src: &S, at: u64) -> Result<([u8; 4], u64, u64)> {
    let h = read(src, at, 8)?;
    let data = at + 8;
    let size = u64::from(le32(&h, 4)).min(src.len().saturating_sub(data));
    Ok((fourcc(&h, 0), size, data))
}

/// Iterate the chunks of `[from, to)`: (id, size, data offset). Stops at the first unreadable
/// header; every step moves forward, so it ends.
fn children<S: ByteSource + ?Sized>(src: &S, from: u64, to: u64) -> Vec<([u8; 4], u64, u64)> {
    let mut out = Vec::new();
    let mut at = from;
    while at.checked_add(8).is_some_and(|e| e <= to) {
        let Ok((id, size, data)) = header_at(src, at) else { break };
        out.push((id, size, data));
        // chunks are padded to an even size
        let Some(next) = data.checked_add(size).and_then(|e| e.checked_add(size & 1)) else { break };
        if next <= at {
            break;
        }
        at = next;
    }
    out
}

/// Open an AVI file.
pub fn open<S: ByteSource + ?Sized>(src: &S) -> Result<AviFile> {
    open_with(src, OpenOptions::default())
}

pub fn open_with<S: ByteSource + ?Sized>(src: &S, opts: OpenOptions) -> Result<AviFile> {
    let head = read(src, 0, 12.min(src.len())).map_err(|_| Error::NotAvi)?;
    if !sniff(&head) {
        return Err(Error::NotAvi);
    }
    // the RIFF forms: the first `AVI `, then any `AVIX` extensions (OpenDML)
    let mut hdrl = None;
    let mut movis: Vec<(u64, u64, u64)> = Vec::new(); // (position of the `movi` id, data start, data end)
    let mut idx1 = None;
    for (id, size, data) in children(src, 0, src.len()) {
        if id != *b"RIFF" || size < 4 {
            continue;
        }
        let end = data + size;
        let form = fourcc(&read(src, data, 4)?, 0);
        if form != *b"AVI " && form != *b"AVIX" {
            continue;
        }
        for (cid, csize, cdata) in children(src, data + 4, end) {
            match &cid {
                b"LIST" if csize >= 4 => {
                    let ltype = fourcc(&read(src, cdata, 4)?, 0);
                    if ltype == *b"hdrl" && hdrl.is_none() && form == *b"AVI " {
                        hdrl = Some((cdata + 4, cdata + csize));
                    } else if ltype == *b"movi" {
                        movis.push((cdata, cdata + 4, cdata + csize));
                    }
                }
                b"idx1" if form == *b"AVI " => idx1 = Some((cdata, csize)),
                _ => {}
            }
        }
    }
    let (hfrom, hto) = hdrl.ok_or_else(|| Error::Invalid("no hdrl header list".into()))?;
    if hto - hfrom > MAX_HEADER {
        return Err(Error::Invalid("header list too large".into()));
    }
    let (header, mut streams) = parse_hdrl(src, hfrom, hto)?;
    if streams.is_empty() {
        return Err(Error::Invalid("no streams".into()));
    }
    // chunk tables from the best index
    let odml = !opts.ignore_odml && streams.iter().any(|s| !s.super_index.is_empty());
    let index = if odml && read_odml(src, &mut streams) {
        IndexSource::OpenDml
    } else if let (false, Some((at, size)), Some(first)) = (opts.ignore_idx1, idx1, movis.first()) {
        clear(&mut streams);
        read_idx1(src, &mut streams, at, size, first.0);
        // idx1 only covers the first RIFF: the AVIX extensions are scanned
        for &(_, from, to) in movis.iter().skip(1) {
            scan(src, &mut streams, from, to, 0);
        }
        if streams.iter().all(|s| s.chunks.is_empty()) { scan_all(src, &mut streams, &movis) } else { IndexSource::Idx1 }
    } else {
        scan_all(src, &mut streams, &movis)
    };
    Ok(AviFile { header, streams, index })
}

fn clear(streams: &mut [Stream]) {
    for s in streams.iter_mut() {
        s.chunks.clear();
        s.keyframes_known = true;
    }
}

fn scan_all<S: ByteSource + ?Sized>(src: &S, streams: &mut [Stream], movis: &[(u64, u64, u64)]) -> IndexSource {
    clear(streams);
    for &(_, from, to) in movis {
        scan(src, streams, from, to, 0);
    }
    for s in streams.iter_mut() {
        s.keyframes_known = false;
    }
    IndexSource::Scan
}

fn parse_hdrl<S: ByteSource + ?Sized>(src: &S, from: u64, to: u64) -> Result<(MainHeader, Vec<Stream>)> {
    let mut header = MainHeader::default();
    let mut streams = Vec::new();
    for (id, size, data) in children(src, from, to) {
        match &id {
            b"avih" => {
                let b = read(src, data, size.min(64))?;
                header = MainHeader {
                    micro_sec_per_frame: le32(&b, 0),
                    flags: le32(&b, 12),
                    total_frames: le32(&b, 16),
                    streams: le32(&b, 24),
                    width: le32(&b, 32),
                    height: le32(&b, 36),
                };
            }
            b"LIST" if size >= 4 && streams.len() < MAX_STREAMS && fourcc(&read(src, data, 4)?, 0) == *b"strl" => {
                streams.push(parse_strl(src, data + 4, data + size)?);
            }
            _ => {}
        }
    }
    Ok((header, streams))
}

fn parse_strl<S: ByteSource + ?Sized>(src: &S, from: u64, to: u64) -> Result<Stream> {
    let mut s = Stream {
        kind: StreamKind::Other([0; 4]),
        handler: [0; 4],
        scale: 1,
        rate: 1,
        start: 0,
        length: 0,
        sample_size: 0,
        video: None,
        audio: None,
        name: None,
        chunks: Vec::new(),
        keyframes_known: true,
        super_index: Vec::new(),
    };
    let mut strf = None;
    for (id, size, data) in children(src, from, to) {
        match &id {
            b"strh" => {
                let b = read(src, data, size.min(64))?;
                s.kind = match &fourcc(&b, 0) {
                    b"vids" => StreamKind::Video,
                    b"auds" => StreamKind::Audio,
                    other => StreamKind::Other(*other),
                };
                s.handler = fourcc(&b, 4);
                s.scale = le32(&b, 20);
                s.rate = le32(&b, 24);
                s.start = le32(&b, 28);
                s.length = le32(&b, 32);
                s.sample_size = le32(&b, 44);
            }
            b"strf" => strf = Some(read(src, data, size.min(1 << 20))?),
            b"strn" => {
                let b = read(src, data, size.min(4096))?;
                let name = String::from_utf8_lossy(b.split(|&c| c == 0).next().unwrap_or_default()).trim().to_string();
                s.name = (!name.is_empty()).then_some(name);
            }
            b"indx" => s.super_index = super_index(&read(src, data, size.min(MAX_HEADER))?),
            _ => {}
        }
    }
    if let Some(b) = strf {
        match s.kind {
            StreamKind::Video => {
                s.video = Some(BitmapInfo {
                    width: le32(&b, 4) as i32,
                    height: le32(&b, 8) as i32,
                    bit_count: le16(&b, 14),
                    compression: fourcc(&b, 16),
                    size_image: le32(&b, 20),
                    extra: b.get(40.min(b.len())..).unwrap_or_default().to_vec(),
                });
            }
            StreamKind::Audio => {
                let cb = usize::from(le16(&b, 16));
                let extra = b.get(18.min(b.len())..(18 + cb).min(b.len())).unwrap_or_default().to_vec();
                let mut tag = le16(&b, 0);
                // WAVE_FORMAT_EXTENSIBLE: the sub-format GUID starts with the real tag
                if tag == 0xFFFE && extra.len() >= 22 {
                    tag = le16(&extra, 6);
                }
                s.audio = Some(WaveFormat {
                    format_tag: tag,
                    channels: le16(&b, 2),
                    sample_rate: le32(&b, 4),
                    avg_bytes_per_sec: le32(&b, 8),
                    block_align: le16(&b, 12),
                    bits_per_sample: le16(&b, 14),
                    extra,
                });
            }
            StreamKind::Other(_) => {}
        }
    }
    if s.scale == 0 || s.rate == 0 {
        (s.scale, s.rate) = (1, 1);
    }
    Ok(s)
}

/// The `ix##` chunk offsets of an `indx` super index (`AVI_INDEX_OF_INDEXES`, 4 longs per entry).
fn super_index(b: &[u8]) -> Vec<u64> {
    let longs = usize::from(le16(b, 0));
    let (subtype, itype, n) = (b.get(2).copied().unwrap_or(0), b.get(3).copied().unwrap_or(0xFF), le32(b, 4) as usize);
    if itype != 0 || subtype != 0 || longs != 4 {
        return Vec::new();
    }
    // header 24 bytes, entries of 16: qwOffset, dwSize, dwDuration
    let n = n.min(b.len().saturating_sub(24) / 16);
    (0..n).map(|k| le64(b, 24 + k * 16)).filter(|&o| o > 0).collect()
}

/// Fill the chunk tables from the OpenDML standard indexes; false (tables cleared) when a stream
/// that has data lacks one or none could be read.
fn read_odml<S: ByteSource + ?Sized>(src: &S, streams: &mut [Stream]) -> bool {
    let limit = src.len() / 8;
    let mut total = 0u64;
    for s in streams.iter_mut() {
        s.chunks.clear();
        s.keyframes_known = true;
        for &at in &s.super_index {
            let Ok((id, size, data)) = header_at(src, at) else { continue };
            if &id[..2] != b"ix" {
                continue;
            }
            let Ok(b) = read(src, data, size.min(MAX_HEADER * 8)) else { continue };
            let (longs, subtype, itype) = (usize::from(le16(&b, 0)), b.get(2).copied().unwrap_or(0), b.get(3).copied().unwrap_or(0));
            // AVI_INDEX_OF_CHUNKS; 2 longs per entry, or 3 for the field index (AVI_INDEX_2FIELD)
            if itype != 1 || !(longs == 2 && subtype == 0 || longs == 3 && subtype == 1) {
                continue;
            }
            let base = le64(&b, 12);
            let step = longs * 4;
            let n = (le32(&b, 4) as usize).min(b.len().saturating_sub(24) / step);
            for k in 0..n {
                let e = 24 + k * step;
                let (off, raw) = (u64::from(le32(&b, e)), le32(&b, e + 4));
                let size = raw & 0x7FFF_FFFF;
                let Some(offset) = base.checked_add(off).filter(|o| o.checked_add(u64::from(size)).is_some_and(|end| end <= src.len())) else { continue };
                total += 1;
                if total > limit {
                    return false;
                }
                s.chunks.push(Chunk { offset, size, key: raw & 0x8000_0000 == 0 });
            }
        }
    }
    let ok = streams.iter().any(|s| !s.chunks.is_empty()) && streams.iter().all(|s| !s.super_index.is_empty() || matches!(s.kind, StreamKind::Other(_)));
    if !ok {
        clear(streams);
    }
    ok
}

/// Fill the chunk tables from `idx1` (16-byte entries: id, flags, offset, size). Offsets are
/// relative to the `movi` list's type id, or (some writers) absolute: the first entry decides.
fn read_idx1<S: ByteSource + ?Sized>(src: &S, streams: &mut [Stream], at: u64, size: u64, movi: u64) {
    let Ok(b) = read(src, at, size.min(src.len() / 2)) else { return };
    let entries: Vec<&[u8]> = b.as_chunks::<16>().0.iter().map(|e| e.as_slice()).collect();
    let id_at = |pos: u64| read(src, pos, 4).ok().map(|v| fourcc(&v, 0));
    let first = entries.iter().find(|e| le32(e, 4) & 1 == 0);
    let relative = match first {
        Some(e) => {
            let (id, off) = (fourcc(e, 0), u64::from(le32(e, 8)));
            id_at(movi + off) == Some(id) || id_at(off) != Some(id)
        }
        None => true,
    };
    let base = if relative { movi } else { 0 };
    for e in entries {
        let (id, flags, off, len) = (fourcc(e, 0), le32(e, 4), u64::from(le32(e, 8)), le32(e, 12));
        // AVIIF_LIST entries (`rec ` lists) and palette changes carry no samples
        if flags & 1 != 0 || &id[2..] == b"pc" {
            continue;
        }
        let Some(s) = Stream::number(&id).and_then(|n| streams.get_mut(n)) else { continue };
        let Some(offset) = base.checked_add(off).and_then(|o| o.checked_add(8)).filter(|o| o.checked_add(u64::from(len)).is_some_and(|end| end <= src.len()))
        else {
            continue;
        };
        s.chunks.push(Chunk { offset, size: len, key: flags & 0x10 != 0 || s.kind != StreamKind::Video });
    }
}

/// Append the data chunks of a `movi` list (`[from, to)`) to the streams' tables, in file order.
fn scan<S: ByteSource + ?Sized>(src: &S, streams: &mut [Stream], from: u64, to: u64, depth: u32) {
    if depth >= MAX_DEPTH {
        return;
    }
    for (id, size, data) in children(src, from, to) {
        if id == *b"LIST" {
            if size >= 4 {
                scan(src, streams, data + 4, data + size, depth + 1);
            }
            continue;
        }
        if &id[..2] == b"ix" || &id[2..] == b"pc" {
            continue;
        }
        if let Some(s) = Stream::number(&id).and_then(|n| streams.get_mut(n)) {
            s.chunks.push(Chunk { offset: data, size: u32::try_from(size).unwrap_or(u32::MAX), key: true });
        }
    }
}

impl AviFile {
    /// The bytes of chunk `i` of stream `stream`.
    pub fn read_chunk<S: ByteSource + ?Sized>(&self, src: &S, stream: usize, i: usize) -> Result<Vec<u8>> {
        let c = self.streams.get(stream).and_then(|s| s.chunks.get(i)).ok_or_else(|| Error::Invalid(format!("no chunk {i} in stream {stream}")))?;
        read(src, c.offset, u64::from(c.size))
    }
}
