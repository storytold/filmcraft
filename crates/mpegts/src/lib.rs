//! Clean-room MPEG-2 Systems demuxer (ITU-T H.222.0 | ISO/IEC 13818-1).
//!
//! - **Transport Stream**: 188-byte packets, 192-byte BDAV / AVCHD packets (`.m2ts`, `.mts`:
//!   a 4-byte arrival timestamp before each packet) and 204-byte packets (Reed-Solomon parity).
//!   PAT / PMT (sections reassembled across packets, CRC checked), PES reassembly, PCR, PTS/DTS
//!   (33-bit wrap-around unwrapped), discontinuity indicators.
//! - **Program Stream** (`.mpg`, `.vob`, `.mod`, `.tod`): MPEG-2 and ISO/IEC 11172-1 (MPEG-1)
//!   packs, system headers, program stream maps, DVD private stream 1 sub-streams (AC-3, DTS,
//!   LPCM).
//! - **Elementary streams** are split into access units on open — coded frames for MPEG-1/2
//!   video (picture start codes, field pairs), H.264 and HEVC (access unit delimiters / first
//!   slice of a picture), audio frames for MPEG audio, ADTS / LATM AAC, AC-3 and E-AC-3, PES
//!   packets for LPCM — each with its PTS/DTS (a PES timestamp belongs to the first access unit
//!   that starts in the packet), random-access flag and, for MPEG video, picture type, temporal
//!   reference and field structure. That index is the seeking table.
//! - Units are read back on demand by re-reading the file from the unit's first packet, so the
//!   index costs a few dozen bytes per access unit.
//!
//! The crate does no decoding; `filmcraft-codecs` pairs the streams with decoders.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

mod es;
pub use es::{FrameInfo, frame_bytes, frame_info};
mod pes;
mod ps;
mod ts;

use std::io;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not an MPEG transport or program stream")]
    NotMpeg,
    #[error("I/O: {0}")]
    Io(#[from] io::Error),
    #[error("invalid stream: {0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Random-access bytes (a file, a memory buffer).
pub trait ByteSource {
    fn len(&self) -> u64;
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()>;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl ByteSource for [u8] {
    fn len(&self) -> u64 {
        <[u8]>::len(self) as u64
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let o = usize::try_from(offset).map_err(|_| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        let src = self.get(o..o + buf.len()).ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        buf.copy_from_slice(src);
        Ok(())
    }
}

impl ByteSource for Vec<u8> {
    fn len(&self) -> u64 {
        self.as_slice().len() as u64
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.as_slice().read_at(offset, buf)
    }
}

/// Read as much as is available at `offset` (up to `buf.len()`); returns the count.
pub(crate) fn read_some<S: ByteSource + ?Sized>(src: &S, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
    let len = src.len();
    if offset >= len {
        return Ok(0);
    }
    let n = ((len - offset) as usize).min(buf.len());
    src.read_at(offset, &mut buf[..n])?;
    Ok(n)
}

/// The container layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// Transport stream with `packet_size`-byte packets whose 0x47 sync byte is at `sync_offset`
    /// (4 for 192-byte BDAV packets).
    Ts { packet_size: usize, sync_offset: usize },
    /// Program stream (`mpeg1`: ISO/IEC 11172-1 packs).
    Ps { mpeg1: bool },
}

impl Format {
    pub fn name(&self) -> &'static str {
        match self {
            Format::Ts { packet_size: 192, .. } => "MPEG-2 TS (BDAV/AVCHD)",
            Format::Ts { .. } => "MPEG-2 TS",
            Format::Ps { mpeg1: true } => "MPEG-1 System",
            Format::Ps { mpeg1: false } => "MPEG-2 PS",
        }
    }
}

/// Recognise a transport stream (five consecutive sync bytes at one of the packet sizes, within
/// the first packets) or a program stream (a pack header).
pub fn sniff(head: &[u8]) -> Option<Format> {
    for (size, off) in [(188usize, 0usize), (192, 4), (204, 0)] {
        for start in 0..size.min(head.len()) {
            let ok = (0..5).all(|k| head.get(start + off + k * size) == Some(&0x47));
            if ok {
                return Some(Format::Ts { packet_size: size, sync_offset: off });
            }
        }
    }
    // a pack header, possibly after a few zero bytes
    let z = head.iter().take(64).take_while(|&&b| b == 0).count();
    let p = z.saturating_sub(2);
    if head.len() >= p + 5 && head[p..p + 4] == [0, 0, 1, 0xBA] {
        return Some(Format::Ps { mpeg1: head[p + 4] >> 6 != 1 });
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Video,
    Audio,
    Subtitle,
    Other,
}

/// What an elementary stream carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Codec {
    Mpeg1Video,
    Mpeg2Video,
    H264,
    Hevc,
    /// MPEG-1/2 audio (layers I-III; the layer is in each frame header).
    MpegAudio,
    /// AAC in ADTS frames.
    AacAdts,
    /// AAC in LATM/LOAS frames.
    AacLatm,
    Ac3,
    Eac3,
    Dts,
    TrueHd,
    /// Blu-ray / AVCHD LPCM (a 4-byte header per PES packet, big-endian samples).
    LpcmBluray,
    /// DVD LPCM (program stream private stream 1; format in [`Stream::lpcm`]).
    LpcmDvd,
    Subtitle(&'static str),
    /// Anything else: the stream type.
    Unknown(u8),
}

impl Codec {
    pub fn name(&self) -> String {
        match self {
            Codec::Mpeg1Video => "MPEG-1 Video".into(),
            Codec::Mpeg2Video => "MPEG-2 Video".into(),
            Codec::H264 => "H.264".into(),
            Codec::Hevc => "HEVC".into(),
            Codec::MpegAudio => "MPEG Audio".into(),
            Codec::AacAdts => "AAC (ADTS)".into(),
            Codec::AacLatm => "AAC (LATM)".into(),
            Codec::Ac3 => "AC-3".into(),
            Codec::Eac3 => "E-AC-3".into(),
            Codec::Dts => "DTS".into(),
            Codec::TrueHd => "Dolby TrueHD".into(),
            Codec::LpcmBluray => "LPCM (Blu-ray)".into(),
            Codec::LpcmDvd => "LPCM (DVD)".into(),
            Codec::Subtitle(s) => (*s).into(),
            Codec::Unknown(t) => format!("stream type 0x{t:02x}"),
        }
    }
    pub fn kind(&self) -> Kind {
        match self {
            Codec::Mpeg1Video | Codec::Mpeg2Video | Codec::H264 | Codec::Hevc => Kind::Video,
            Codec::MpegAudio | Codec::AacAdts | Codec::AacLatm | Codec::Ac3 | Codec::Eac3 | Codec::Dts | Codec::TrueHd | Codec::LpcmBluray | Codec::LpcmDvd => {
                Kind::Audio
            }
            Codec::Subtitle(_) => Kind::Subtitle,
            Codec::Unknown(_) => Kind::Other,
        }
    }
}

/// MPEG-1/2 video picture properties of an access unit (its first picture).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PictureInfo {
    /// 1 I, 2 P, 3 B, 4 D.
    pub coding_type: u8,
    pub temporal_reference: u16,
    /// 1 top field, 2 bottom field, 3 frame (two field pictures form one unit).
    pub structure: u8,
    pub top_field_first: bool,
    pub repeat_first_field: bool,
    pub progressive_frame: bool,
    /// A sequence header precedes the picture in this unit.
    pub sequence_header: bool,
    /// A GOP header precedes the picture (and its flags).
    pub gop: bool,
    pub closed_gop: bool,
    pub broken_link: bool,
}

/// One access unit (a coded frame, an audio frame, or an LPCM PES packet).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unit {
    /// Presentation / decoding time stamps, 90 kHz, 33-bit wrap-around unwrapped.
    pub pts: Option<i64>,
    pub dts: Option<i64>,
    /// Decoding can start here (I / IDR / IRAP pictures; every audio frame).
    pub key: bool,
    /// No other unit depends on it (B pictures of MPEG-1/2 video, non-reference H.264/HEVC).
    pub disposable: bool,
    pub size: u32,
    /// TS: index of the packet holding the first byte; PS: byte offset of its PES packet.
    pub(crate) pos: u64,
    /// Offset of the first byte within that packet's elementary-stream payload.
    pub(crate) offset: u32,
    pub picture: Option<PictureInfo>,
}

/// DVD LPCM format (from the private stream 1 header).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LpcmFormat {
    pub sample_rate: u32,
    pub channels: u32,
    pub bits: u32,
}

/// Where a stream's PES packets come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamId {
    /// Transport stream PID.
    Pid(u16),
    /// Program stream stream_id, and the private stream 1 sub-stream id.
    Ps { stream_id: u8, sub_id: Option<u8> },
}

#[derive(Clone, Debug)]
pub struct Stream {
    pub id: StreamId,
    /// PMT stream_type (TS), or the equivalent for program streams.
    pub stream_type: u8,
    pub codec: Codec,
    /// ISO 639 language code from the PMT.
    pub language: Option<String>,
    pub units: Vec<Unit>,
    pub lpcm: Option<LpcmFormat>,
    /// PES packets seen.
    pub pes_packets: u64,
    /// Bytes of payload ignored because the stream could not be split (e.g. before the first
    /// frame header).
    pub skipped_bytes: u64,
}

impl Stream {
    pub fn kind(&self) -> Kind {
        self.codec.kind()
    }
    /// First and last presentation timestamps (90 kHz) of the stream's units.
    pub fn pts_range(&self) -> Option<(i64, i64)> {
        let mut it = self.units.iter().filter_map(|u| u.pts);
        let first = it.next()?;
        Some(it.fold((first, first), |(a, b), p| (a.min(p), b.max(p))))
    }
}

/// A demuxed file: its streams with their access-unit tables.
#[derive(Clone, Debug)]
pub struct File {
    pub format: Format,
    pub streams: Vec<Stream>,
    /// Program number of the program the streams belong to (TS).
    pub program: Option<u16>,
    /// First and last PCR (27 MHz, unwrapped) or SCR.
    pub pcr_range: Option<(i64, i64)>,
    /// Problems found while scanning (lost sync, CRC errors, continuity errors…).
    pub warnings: Vec<String>,
}

impl File {
    /// The first stream of `kind` whose codec satisfies `f`.
    pub fn find(&self, kind: Kind, f: impl Fn(&Codec) -> bool) -> Option<usize> {
        self.streams.iter().position(|s| s.kind() == kind && !s.units.is_empty() && f(&s.codec))
    }

    /// Read access unit `i` of stream `s`.
    pub fn read_unit<S: ByteSource + ?Sized>(&self, src: &S, s: usize, i: usize) -> Result<Vec<u8>> {
        let st = self.streams.get(s).ok_or_else(|| Error::Invalid("no such stream".into()))?;
        let u = st.units.get(i).ok_or_else(|| Error::Invalid("no such unit".into()))?;
        let mut out = Vec::with_capacity(u.size as usize);
        match self.format {
            Format::Ts { packet_size, sync_offset } => ts::read(src, packet_size, sync_offset, st, u, &mut out)?,
            Format::Ps { .. } => ps::read(src, st, u, &mut out)?,
        }
        if out.len() < u.size as usize {
            return Err(Error::Invalid(format!("unit {i} truncated ({} of {} bytes)", out.len(), u.size)));
        }
        Ok(out)
    }
}

/// Scan a transport or program stream and index its access units.
pub fn open<S: ByteSource + ?Sized>(src: &S) -> Result<File> {
    let mut head = vec![0u8; 64 * 1024];
    let n = read_some(src, 0, &mut head)?;
    head.truncate(n);
    match sniff(&head).ok_or(Error::NotMpeg)? {
        Format::Ts { packet_size, sync_offset } => ts::scan(src, packet_size, sync_offset),
        Format::Ps { mpeg1 } => ps::scan(src, mpeg1),
    }
}

/// Unwraps 33-bit timestamps into a monotonic-ish i64 timeline (relative to the first one seen).
#[derive(Default, Clone, Copy)]
pub(crate) struct Unwrap {
    last: Option<i64>,
    base: i64,
}

impl Unwrap {
    const WRAP: i64 = 1 << 33;
    pub fn unwrap(&mut self, raw: i64) -> i64 {
        let mut v = raw + self.base;
        if let Some(l) = self.last {
            if v < l - Self::WRAP / 2 {
                self.base += Self::WRAP;
                v += Self::WRAP;
            } else if v > l + Self::WRAP / 2 && self.base > 0 {
                // a timestamp from before the wrap (B pictures, slightly out-of-order audio)
                v -= Self::WRAP;
                return v;
            }
        }
        self.last = Some(v);
        v
    }
}

#[cfg(test)]
mod tests;
