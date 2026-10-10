//! Splitting elementary streams into access units while the file is scanned.
//!
//! The scanners feed each stream's payload bytes chunk by chunk (a TS packet's payload, a PS
//! packet's payload). A chunk is identified by the position the reader restarts from (`pos`) and
//! the elementary-stream offset of its first byte. Splitting needs only a few bytes beyond a
//! boundary, so only the most recent chunks are remembered.

use std::collections::VecDeque;

use crate::{Codec, PictureInfo, Unit};

/// How a stream is cut into units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// MPEG-1/2 video: one coded frame (a frame picture or two field pictures) per unit.
    MpegVideo,
    /// H.264 / HEVC: one PES packet (with a timestamp) per unit; NAL types give the flags.
    Nal { hevc: bool },
    /// Framed audio with self-describing frame headers.
    Audio(AudioSync),
    /// One PES packet per unit.
    Pes,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AudioSync {
    Mpa,
    Adts,
    Loas,
    Ac3,
}

impl Mode {
    fn of(codec: &Codec) -> Mode {
        match codec {
            Codec::Mpeg1Video | Codec::Mpeg2Video => Mode::MpegVideo,
            Codec::H264 => Mode::Nal { hevc: false },
            Codec::Hevc => Mode::Nal { hevc: true },
            Codec::MpegAudio => Mode::Audio(AudioSync::Mpa),
            Codec::AacAdts => Mode::Audio(AudioSync::Adts),
            Codec::AacLatm => Mode::Audio(AudioSync::Loas),
            Codec::Ac3 | Codec::Eac3 => Mode::Audio(AudioSync::Ac3),
            _ => Mode::Pes,
        }
    }
}

#[derive(Clone, Copy)]
struct PesMark {
    es_start: u64,
    pts: Option<i64>,
    dts: Option<i64>,
    used: bool,
}

/// A unit whose end is not known yet.
#[derive(Clone, Copy)]
struct Open {
    start: u64,
    pos: u64,
    offset: u32,
    pts: Option<i64>,
    dts: Option<i64>,
    key: bool,
    disposable: bool,
    /// A coded slice has been seen (H.264 / HEVC).
    vcl: bool,
    picture: Option<PictureInfo>,
}

/// MPEG video picture waiting for its coding extension (to tell fields from frames).
#[derive(Clone, Copy)]
struct PendingPicture {
    /// Start of the unit if the picture begins one (its first sequence / GOP header).
    unit_start: u64,
    /// The picture start code (timestamps belong to the unit whose picture starts in the PES).
    at: u64,
    info: PictureInfo,
}

pub(crate) struct Splitter {
    mode: Mode,
    /// Elementary-stream bytes fed so far.
    es_pos: u64,
    /// Recent chunks: (reader position, ES offset of the first byte).
    chunks: VecDeque<(u64, u64)>,
    pes: VecDeque<PesMark>,
    /// Unconsumed bytes (start codes / frame headers spanning chunks), starting at `buf_start`.
    buf: Vec<u8>,
    buf_start: u64,
    open: Option<Open>,
    pub units: Vec<Unit>,
    pub skipped: u64,
    // MPEG video state
    header_start: Option<u64>,
    seq_in_unit: bool,
    gop: Option<(bool, bool)>,
    pending: Option<PendingPicture>,
    open_field: Option<u8>,
    // audio state
    next_frame: Option<u64>,
}

/// AC-3 frame sizes in 16-bit words for 48 / 44.1 / 32 kHz (A/52 Table 5.18), by
/// frmsizecod / 2; 44.1 kHz adds a word for odd frmsizecod.
const AC3_KBPS: [u32; 19] = [32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 576, 640];

pub(crate) fn ac3_frame_bytes(fscod: u8, frmsizecod: u8) -> Option<usize> {
    let kbps = *AC3_KBPS.get((frmsizecod / 2) as usize)?;
    let words = match fscod {
        0 => kbps * 2,
        1 => kbps * 96_000 / 44_100 + (frmsizecod & 1) as u32,
        2 => kbps * 3,
        _ => return None,
    };
    Some(words as usize * 2)
}

const MPA_KBPS: [[u16; 15]; 5] = [
    [0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448], // MPEG-1 layer I
    [0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384],    // MPEG-1 layer II
    [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320],     // MPEG-1 layer III
    [0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256],    // MPEG-2/2.5 layer I
    [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],         // MPEG-2/2.5 layers II, III
];

/// MPEG audio frame header (ISO/IEC 11172-3 §2.4.2.3, 13818-3 LSF): (frame bytes, sample rate,
/// samples per frame).
pub(crate) fn mpa_header(b: &[u8]) -> Option<(usize, u32, u32)> {
    if b.len() < 4 || b[0] != 0xFF || b[1] & 0xE0 != 0xE0 {
        return None;
    }
    let version = (b[1] >> 3) & 3; // 0: 2.5, 2: MPEG-2, 3: MPEG-1
    let layer = (b[1] >> 1) & 3; // 3: I, 2: II, 1: III
    let br = (b[2] >> 4) as usize;
    let sr = (b[2] >> 2) & 3;
    let pad = ((b[2] >> 1) & 1) as usize;
    if version == 1 || layer == 0 || br == 0 || br == 15 || sr == 3 {
        return None;
    }
    let rate = [44_100u32, 48_000, 32_000][sr as usize]
        >> match version {
            3 => 0,
            2 => 1,
            _ => 2,
        };
    let table = match (version == 3, layer) {
        (true, 3) => 0,
        (true, 2) => 1,
        (true, _) => 2,
        (false, 3) => 3,
        (false, _) => 4,
    };
    let kbps = MPA_KBPS[table][br] as usize;
    let (bytes, samples) = match layer {
        3 => ((12 * kbps * 1000 / rate as usize + pad) * 4, 384),
        2 => (144 * kbps * 1000 / rate as usize + pad, 1152),
        _ if version == 3 => (144 * kbps * 1000 / rate as usize + pad, 1152),
        _ => (72 * kbps * 1000 / rate as usize + pad, 576),
    };
    (bytes >= 4).then_some((bytes, rate, samples))
}

/// What an audio frame header says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameInfo {
    pub sample_rate: u32,
    pub channels: u32,
    /// Samples per channel in the frame.
    pub samples: u32,
    /// MPEG audio layer (1-3); AC-3 bsid; ADTS object type (profile + 1).
    pub variant: u8,
}

const ADTS_RATES: [u32; 13] = [96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000, 7_350];

/// Parse the frame header at the start of `b` (MPEG audio, ADTS, AC-3, E-AC-3).
pub fn frame_info(codec: &Codec, b: &[u8]) -> Option<FrameInfo> {
    match codec {
        Codec::MpegAudio => {
            let (_, sample_rate, samples) = mpa_header(b)?;
            let layer = 4 - ((b[1] >> 1) & 3);
            Some(FrameInfo { sample_rate, channels: if b[3] >> 6 == 3 { 1 } else { 2 }, samples, variant: layer })
        }
        Codec::AacAdts => {
            frame_len(AudioSync::Adts, b)?;
            let sf = ((b[2] >> 2) & 15) as usize;
            let ch = (((b[2] & 1) << 2) | (b[3] >> 6)) as u32;
            Some(FrameInfo {
                sample_rate: *ADTS_RATES.get(sf)?,
                channels: if ch == 0 {
                    2
                } else if ch == 7 {
                    8
                } else {
                    ch
                },
                samples: 1024 * ((b[6] & 3) as u32 + 1),
                variant: (b[2] >> 6) + 1,
            })
        }
        Codec::Ac3 | Codec::Eac3 => {
            frame_len(AudioSync::Ac3, b)?;
            let bsid = b[5] >> 3;
            if bsid > 10 {
                // E-AC-3: fscod(2) numblkscod(2) acmod(3) lfeon(1)
                let fscod = b[4] >> 6;
                let (rate, blocks) = if fscod == 3 {
                    ([24_000, 22_050, 16_000, 0][((b[4] >> 4) & 3) as usize], 6)
                } else {
                    ([48_000, 44_100, 32_000][fscod as usize], [1, 2, 3, 6][((b[4] >> 4) & 3) as usize])
                };
                let acmod = (b[4] >> 1) & 7;
                return Some(FrameInfo { sample_rate: rate, channels: AC3_CHANNELS[acmod as usize] + (b[4] & 1) as u32, samples: 256 * blocks, variant: bsid });
            }
            let fscod = b[4] >> 6;
            let rate = *[48_000u32, 44_100, 32_000].get(fscod as usize)?;
            // bsi: bsid(5) bsmod(3) acmod(3) [cmixlev(2)] [surmixlev(2)] [dsurmod(2)] lfeon(1)
            let acmod = b[6] >> 5;
            let mut bit = 3; // bits of byte 6 consumed
            if acmod & 1 != 0 && acmod != 1 {
                bit += 2;
            }
            if acmod & 4 != 0 {
                bit += 2;
            }
            if acmod == 2 {
                bit += 2;
            }
            let word = u16::from_be_bytes([b[6], *b.get(7)?]);
            let lfe = (word >> (15 - bit)) & 1;
            Some(FrameInfo { sample_rate: rate, channels: AC3_CHANNELS[acmod as usize] + lfe as u32, samples: 1536, variant: bsid })
        }
        _ => None,
    }
}

/// Whether the syncframe at the start of `b` is E-AC-3 (bsid 11-16) other than independent
/// substream 0: a dependent substream (strmtyp 1) or another program (A/52 §E2.3.1.1-2).
fn eac3_joins_unit(b: &[u8]) -> bool {
    match b.get(2..6) {
        Some(&[b2, _, _, b5]) => (11..=16).contains(&(b5 >> 3)) && (b2 >> 6 == 1 || (b2 >> 3) & 7 != 0),
        _ => false,
    }
}

/// Full-bandwidth channels per AC-3 audio coding mode (A/52 Table 5.8).
const AC3_CHANNELS: [u32; 8] = [2, 1, 2, 3, 3, 4, 4, 5];

/// Frame length at the start of `b` for the given sync type (`None`: no valid header; `Some(0)`
/// is never returned).
fn frame_len(sync: AudioSync, b: &[u8]) -> Option<usize> {
    match sync {
        AudioSync::Mpa => mpa_header(b).map(|h| h.0),
        AudioSync::Adts => {
            if b.len() < 7 || b[0] != 0xFF || b[1] & 0xF6 != 0xF0 {
                return None;
            }
            let len = ((b[3] as usize & 3) << 11) | (b[4] as usize) << 3 | (b[5] as usize) >> 5;
            (len >= 7).then_some(len)
        }
        AudioSync::Loas => {
            if b.len() < 3 || b[0] != 0x56 || b[1] & 0xE0 != 0xE0 {
                return None;
            }
            let len = ((b[1] as usize & 0x1F) << 8) | b[2] as usize;
            (len > 0).then_some(len + 3)
        }
        AudioSync::Ac3 => {
            if b.len() < 6 || b[0] != 0x0B || b[1] != 0x77 {
                return None;
            }
            let bsid = b[5] >> 3;
            if bsid <= 10 {
                ac3_frame_bytes(b[4] >> 6, b[4] & 0x3F)
            } else if bsid <= 16 {
                // E-AC-3: frmsiz
                Some(((((b[2] & 7) as usize) << 8) | b[3] as usize) * 2 + 2)
            } else {
                None
            }
        }
    }
}

impl Splitter {
    pub fn new(codec: &Codec) -> Splitter {
        Splitter {
            mode: Mode::of(codec),
            es_pos: 0,
            chunks: VecDeque::new(),
            pes: VecDeque::new(),
            buf: Vec::new(),
            buf_start: 0,
            open: None,
            units: Vec::new(),
            skipped: 0,
            header_start: None,
            seq_in_unit: false,
            gop: None,
            pending: None,
            open_field: None,
            next_frame: None,
        }
    }

    /// A PES packet starts (its payload follows in the next chunks).
    pub fn pes_start(&mut self, pts: Option<i64>, dts: Option<i64>) {
        self.pes.push_back(PesMark { es_start: self.es_pos, pts, dts, used: false });
        while self.pes.len() > 64 {
            self.pes.pop_front();
        }
        match self.mode {
            Mode::Pes => self.begin_pes_unit(true),
            Mode::Nal { .. } => self.begin_pes_unit(pts.is_some() || dts.is_some()),
            _ => {}
        }
    }

    /// PES-delimited units: a packet with a timestamp (or any packet in plain PES mode) starts a
    /// new unit; one without continues the previous unit.
    fn begin_pes_unit(&mut self, new: bool) {
        if !new && self.open.is_some() {
            return;
        }
        let at = self.es_pos;
        self.close(at);
        // the chunk of this position arrives with the next feed: resolved there
        let mut o = Open { start: at, pos: u64::MAX, offset: 0, pts: None, dts: None, key: false, disposable: false, vcl: false, picture: None };
        if let Some((pts, dts)) = self.timestamps_at(at) {
            o.pts = pts;
            o.dts = dts;
        }
        if self.mode == Mode::Pes {
            o.key = true;
        }
        self.open = Some(o);
    }

    /// The unused timestamps of the PES packet holding `at` (consumed).
    fn timestamps_at(&mut self, at: u64) -> Option<(Option<i64>, Option<i64>)> {
        let m = self.pes.iter_mut().rev().find(|m| m.es_start <= at)?;
        if m.used || (m.pts.is_none() && m.dts.is_none()) {
            return None;
        }
        m.used = true;
        Some((m.pts, m.dts))
    }

    /// Reader position and offset of elementary-stream byte `at`.
    fn locate(&self, at: u64) -> Option<(u64, u32)> {
        let c = self.chunks.iter().rev().find(|c| c.1 <= at)?;
        Some((c.0, (at - c.1) as u32))
    }

    /// End the open unit at `end`.
    fn close(&mut self, end: u64) {
        let Some(o) = self.open.take() else { return };
        if end <= o.start || o.pos == u64::MAX {
            return;
        }
        let size = (end - o.start).min(u32::MAX as u64) as u32;
        self.units.push(Unit { pts: o.pts, dts: o.dts, key: o.key, disposable: o.disposable, size, pos: o.pos, offset: o.offset, picture: o.picture });
    }

    /// Payload bytes of one chunk (`pos`: where the reader restarts to find them).
    pub fn feed(&mut self, pos: u64, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        self.chunks.push_back((pos, self.es_pos));
        if let Some(o) = self.open.as_mut()
            && o.pos == u64::MAX
            && o.start == self.es_pos
        {
            o.pos = pos;
            o.offset = 0;
        }
        let start = self.es_pos;
        self.es_pos += data.len() as u64;
        match self.mode {
            Mode::Pes => {}
            Mode::Nal { hevc } => self.scan_nal(start, data, hevc),
            Mode::MpegVideo => self.scan_mpeg(start, data),
            Mode::Audio(sync) => self.scan_audio(start, data, sync),
        }
        // forget chunks no pending position can refer to
        let keep_from = self.buf_start.min(self.pending.map_or(u64::MAX, |p| p.unit_start.min(p.at))).min(self.header_start.unwrap_or(u64::MAX));
        while self.chunks.len() > 2 && self.chunks[1].1 <= keep_from {
            self.chunks.pop_front();
        }
    }

    /// Append to the window and return it with its start.
    fn window(&mut self, start: u64, data: &[u8]) {
        if self.buf.is_empty() || self.buf_start + self.buf.len() as u64 != start {
            self.buf.clear();
            self.buf_start = start;
        }
        self.buf.extend_from_slice(data);
    }

    /// Keep only the last `keep` bytes of the window.
    fn trim(&mut self, keep: usize) {
        if self.buf.len() > keep {
            let drop = self.buf.len() - keep;
            self.buf.drain(..drop);
            self.buf_start += drop as u64;
        }
    }

    /// Start codes in the window from index `from`: (window index of `00 00 01`, code).
    fn next_start_code(&self, from: usize) -> Option<usize> {
        let b = &self.buf;
        let mut i = from;
        while i + 3 <= b.len() {
            match b[i + 2] {
                1 if b[i] == 0 && b[i + 1] == 0 => return Some(i),
                0 => i += 1,
                _ => i += 3,
            }
        }
        None
    }

    fn scan_nal(&mut self, start: u64, data: &[u8], hevc: bool) {
        self.window(start, data);
        let mut i = 0;
        let mut blocked = None;
        while let Some(k) = self.next_start_code(i) {
            // NAL header byte(s) needed
            if k + 5 > self.buf.len() {
                blocked = Some(k);
                break;
            }
            let h = self.buf[k + 3];
            let (key, vcl, non_ref) = if hevc {
                let t = (h >> 1) & 0x3F;
                (
                    (16..=23).contains(&t),
                    t < 32,
                    // sub-layer non-reference pictures (TRAIL_N, TSA_N, STSA_N, RADL_N, RASL_N)
                    t < 16 && t.is_multiple_of(2),
                )
            } else {
                let t = h & 0x1F;
                (t == 5, (1..=5).contains(&t), h & 0x60 == 0)
            };
            if let Some(o) = self.open.as_mut()
                && vcl
            {
                o.key |= key;
                // disposable while every slice is non-reference
                o.disposable = if o.vcl { o.disposable && non_ref } else { non_ref };
                o.vcl = true;
            }
            i = k + 3;
        }
        self.keep_tail(i, blocked);
    }

    /// Drop the scanned part of the window: keep from a start code waiting for more bytes, or
    /// the last bytes a split start code may begin in.
    fn keep_tail(&mut self, i: usize, blocked: Option<usize>) {
        let len = self.buf.len();
        let resume = blocked.unwrap_or_else(|| i.max(len.saturating_sub(3)));
        self.trim(len - resume.min(len));
    }

    fn scan_mpeg(&mut self, start: u64, data: &[u8]) {
        self.window(start, data);
        let mut i = 0;
        let mut blocked = None;
        while let Some(k) = self.next_start_code(i) {
            // up to 5 bytes after the code are inspected
            if k + 9 > self.buf.len() {
                blocked = Some(k);
                break;
            }
            let at = self.buf_start + k as u64;
            let code = self.buf[k + 3];
            let p = [self.buf[k + 4], self.buf[k + 5], self.buf[k + 6], self.buf[k + 7], self.buf[k + 8]];
            match code {
                0xB3 => {
                    self.settle_pending();
                    self.header_start.get_or_insert(at);
                    self.seq_in_unit = true;
                }
                0xB8 => {
                    self.settle_pending();
                    self.header_start.get_or_insert(at);
                    self.gop = Some((p[3] & 0x40 != 0, p[3] & 0x20 != 0));
                }
                0x00 => {
                    self.settle_pending();
                    let info = PictureInfo {
                        coding_type: (p[1] >> 3) & 7,
                        temporal_reference: ((p[0] as u16) << 2) | (p[1] >> 6) as u16,
                        structure: 3,
                        progressive_frame: true,
                        sequence_header: self.seq_in_unit,
                        gop: self.gop.is_some(),
                        closed_gop: self.gop.is_some_and(|g| g.0),
                        broken_link: self.gop.is_some_and(|g| g.1),
                        ..Default::default()
                    };
                    let unit_start = self.header_start.take().unwrap_or(at);
                    self.seq_in_unit = false;
                    self.gop = None;
                    self.pending = Some(PendingPicture { unit_start, at, info });
                }
                0xB5 if p[0] >> 4 == 8 => {
                    if let Some(pp) = self.pending.as_mut() {
                        pp.info.structure = p[2] & 3;
                        pp.info.top_field_first = p[3] & 0x80 != 0;
                        pp.info.repeat_first_field = p[3] & 0x02 != 0;
                        pp.info.progressive_frame = p[4] & 0x80 != 0;
                    }
                    self.settle_pending();
                }
                0xB5 | 0xB2 => {}
                _ => self.settle_pending(),
            }
            i = k + 3;
        }
        self.keep_tail(i, blocked);
    }

    /// A picture's structure is known: start a unit, or join a first field.
    fn settle_pending(&mut self) {
        let Some(pp) = self.pending.take() else { return };
        let s = pp.info.structure;
        if let Some(first) = self.open_field
            && s != 3
            && first != s
            && s != 0
        {
            self.open_field = None;
            return;
        }
        self.close(pp.unit_start);
        let (pos, offset) = self.locate(pp.unit_start).unwrap_or((u64::MAX, 0));
        let mut o = Open {
            start: pp.unit_start,
            pos,
            offset,
            pts: None,
            dts: None,
            key: matches!(pp.info.coding_type, 1 | 4),
            disposable: pp.info.coding_type == 3,
            vcl: true,
            picture: Some(pp.info),
        };
        if let Some((pts, dts)) = self.timestamps_at(pp.at) {
            o.pts = pts;
            o.dts = dts;
        }
        self.open = Some(o);
        self.open_field = (s == 1 || s == 2).then_some(s);
    }

    fn scan_audio(&mut self, start: u64, data: &[u8], sync: AudioSync) {
        let end = start + data.len() as u64;
        // skip whole chunks inside the current frame
        if let Some(n) = self.next_frame
            && n >= end
        {
            return;
        }
        self.window(start, data);
        loop {
            let at = match self.next_frame {
                Some(n) if n >= self.buf_start => n,
                _ => self.buf_start,
            };
            let i = (at - self.buf_start) as usize;
            if i + 8 > self.buf.len() {
                // not enough bytes for a header yet
                self.trim(self.buf.len().saturating_sub(i));
                return;
            }
            match frame_len(sync, &self.buf[i..]) {
                Some(len) if self.next_frame == Some(at) || self.confirmed(sync, i, len) => {
                    // an E-AC-3 dependent substream or further program joins the access unit of
                    // the independent substream 0 before it, as in MP4 and Matroska
                    if !(sync == AudioSync::Ac3 && self.open.is_some() && eac3_joins_unit(&self.buf[i..])) {
                        self.close(at);
                        let (pos, offset) = self.locate(at).unwrap_or((u64::MAX, 0));
                        let mut o = Open { start: at, pos, offset, pts: None, dts: None, key: true, disposable: false, vcl: false, picture: None };
                        if let Some((pts, dts)) = self.timestamps_at(at) {
                            o.pts = pts;
                            o.dts = dts;
                        }
                        self.open = Some(o);
                    }
                    self.next_frame = Some(at + len as u64);
                    if at + len as u64 >= self.buf_start + self.buf.len() as u64 {
                        self.buf.clear();
                        self.buf_start = at + len as u64;
                        return;
                    }
                }
                _ => {
                    // lost sync: look for the next header from the following byte
                    self.next_frame = None;
                    self.skipped += 1;
                    let next = self.buf[i + 1..]
                        .iter()
                        .position(|&b| matches!((sync, b), (AudioSync::Ac3, 0x0B) | (AudioSync::Loas, 0x56) | (AudioSync::Mpa | AudioSync::Adts, 0xFF)));
                    match next {
                        Some(d) => {
                            self.skipped += d as u64;
                            self.trim(self.buf.len() - (i + 1 + d));
                        }
                        None => {
                            self.skipped += (self.buf.len() - i - 1) as u64;
                            self.trim(0);
                            return;
                        }
                    }
                }
            }
        }
    }

    /// A header found while hunting for sync is accepted when the next frame's header follows
    /// it (or the data ends before it).
    fn confirmed(&self, sync: AudioSync, i: usize, len: usize) -> bool {
        let j = i + len;
        if j + 8 > self.buf.len() {
            return true;
        }
        frame_len(sync, &self.buf[j..]).is_some()
    }

    /// End of stream: close the open unit.
    pub fn finish(&mut self) {
        self.settle_pending();
        let end = match (self.mode, self.next_frame) {
            (Mode::Audio(_), Some(n)) => n.min(self.es_pos),
            _ => self.es_pos,
        };
        if let (Mode::Audio(_), Some(o)) = (self.mode, self.open) {
            // a truncated last frame is dropped
            if self.next_frame.is_some_and(|n| n > self.es_pos) {
                self.skipped += self.es_pos - o.start;
                self.open = None;
            }
        }
        self.close(end);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_frame_sizes() {
        // MPEG-1 layer II 48 kHz 192 kb/s: 576 bytes; layer III 44.1 kHz 128 kb/s with padding: 418
        assert_eq!(mpa_header(&[0xFF, 0xFD, 0xA4, 0x00]), Some((576, 48_000, 1152)));
        assert_eq!(mpa_header(&[0xFF, 0xFB, 0x92, 0x00]), Some((418, 44_100, 1152)));
        // MPEG-2 layer III 24 kHz 64 kb/s: 192 bytes, 576 samples
        assert_eq!(mpa_header(&[0xFF, 0xF3, 0x84, 0x00]), Some((192, 24_000, 576)));
        // AC-3 448 kb/s at 48 kHz = 1792 bytes; 44.1 kHz 192 kb/s odd code = 836 bytes
        assert_eq!(ac3_frame_bytes(0, 30), Some(1792));
        assert_eq!(ac3_frame_bytes(1, 21), Some(836));
        assert_eq!(ac3_frame_bytes(2, 0), Some(192));
    }

    #[test]
    fn eac3_dependent_substreams_join_the_access_unit() {
        // 16-byte E-AC-3 syncframes (frmsiz 7), 48 kHz six blocks stereo: independent, dependent
        // (7.1 extension), independent, dependent
        let frame = |strmtyp: u8| {
            let mut f = vec![0x0B, 0x77, strmtyp << 6, 7, (3 << 4) | (2 << 1), 16 << 3];
            f.resize(16, 0);
            f
        };
        let es: Vec<u8> = [frame(0), frame(1), frame(0), frame(1)].concat();
        let mut s = Splitter::new(&Codec::Eac3);
        s.feed(0, &es);
        s.finish();
        assert_eq!(s.units.iter().map(|u| u.size).collect::<Vec<_>>(), vec![32, 32]);
        assert_eq!(frame_info(&Codec::Eac3, &es).map(|f| f.samples), Some(1536));
        // AC-3 frames are never joined
        assert!(!eac3_joins_unit(&[0x0B, 0x77, 0x40, 0, 0, 8 << 3]));
    }
}
