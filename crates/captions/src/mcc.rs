//! MacCaption MCC (`.mcc`): closed-caption data as text, one line per video frame, each line a
//! SMPTE ST 334-2 caption distribution packet (CDP) carrying CEA-608 byte pairs and CEA-708
//! (DTVCC) packets, written in hexadecimal with MacCaption's letter abbreviations.
//!
//! ```text
//! File Format=MacCaption_MCC V1.0
//! …
//! Time Code Rate=30DF
//!
//! 00:00:01;00 T49S494F43ZZ72F4FC9420RFE...G7400Z4C
//! ```
//!
//! - **Line:** `timecode<TAB>data`; data is ancillary DID/SDID `61 01`, a byte count, then the
//!   CDP. Letters stand for byte runs: `G`–`O` = 1–9 × `FA 00 00`, `P` = `FB 80 80`,
//!   `Q` = `FC 80 80`, `R` = `FD 80 80`, `S` = `96 69`, `T` = `61 01`, `U` = `E1 00 00 00`,
//!   `Z` = `00`.
//! - **CDP (SMPTE ST 334-2):** identifier `96 69`, length, frame-rate code, flags, sequence
//!   counter, a `cc_data` section (`72`, `E0 | cc_count`, cc_count × (marker/valid/type, two
//!   bytes)), optional time code / service info sections, footer `74`, the counter again and a
//!   checksum that makes the packet's byte sum 0 mod 256. The ancillary packet then ends with one ST 291 checksum byte
//!   (low 8 bits of DID + SDID + data count + CDP bytes).
//! - **Writing** (29.97 fps, `30DF` or `30` timecode labels): every frame from the first caption
//!   data to one frame after the last carries a CDP with 20 cc_data triplets: field 1 CEA-608
//!   pop-on data (the same schedule as [SCC](crate::scc)), a null field-2 pair, CEA-708
//!   service 1 data and padding. The CEA-708 captions emulate pop-on with two windows: each
//!   caption is defined (hidden) and loaded into one window while the other shows, then a
//!   DisplayWindows / HideWindows pair switches them on the caption's first frame; HideWindows
//!   clears it at its end.
//! - **Reading:** every CDP's `cc_data`; channel 1 CEA-608 field-1 data is decoded with the SCC
//!   decoder when present, otherwise CEA-708 service 1 (windows, text, CR, clear / display /
//!   hide / toggle / delete windows, DefineWindow visibility); a cue starts whenever the visible
//!   text changes. `Time Code Rate` 30DF / 30 → 29.97 fps, 60DF / 60 → 59.94, 24, 25, 50.

use std::collections::BTreeMap;

use filmcraft_time::{FrameRate, fields_to_frames, frames_to_fields};

use crate::cea608::with_parity;
use crate::{Cue, Document, Error, Result, scc};

/// MCC files written by FilmCraft are 29.97 fps.
pub const RATE: FrameRate = FrameRate::FPS_29_97;
const CC_COUNT: usize = 20;
/// cc_data triplets per frame left for CEA-708 (after the two CEA-608 fields).
const DTVCC_PAIRS: usize = CC_COUNT - 2;

// ---------------------------------------------------------------------------------------------
// Hex with MacCaption abbreviations
// ---------------------------------------------------------------------------------------------

const ABBREV: &[(char, &[u8])] = &[
    ('P', &[0xfb, 0x80, 0x80]),
    ('Q', &[0xfc, 0x80, 0x80]),
    ('R', &[0xfd, 0x80, 0x80]),
    ('S', &[0x96, 0x69]),
    ('T', &[0x61, 0x01]),
    ('U', &[0xe1, 0x00, 0x00, 0x00]),
    ('Z', &[0x00]),
];

/// Bytes → MCC text (uppercase hex with abbreviations).
pub fn compress(b: &[u8]) -> String {
    let mut out = String::new();
    let mut i = 0;
    'outer: while i < b.len() {
        let mut n = 0;
        while n < 9 && b[i + 3 * n..].starts_with(&[0xfa, 0x00, 0x00]) {
            n += 1;
        }
        if n > 0 {
            out.push((b'G' + n as u8 - 1) as char);
            i += 3 * n;
            continue;
        }
        for (c, seq) in ABBREV {
            if b[i..].starts_with(seq) {
                out.push(*c);
                i += seq.len();
                continue 'outer;
            }
        }
        out.push_str(&format!("{:02X}", b[i]));
        i += 1;
    }
    out
}

/// MCC text → bytes (None if malformed).
pub fn expand(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut hex: Option<u8> = None;
    for c in s.chars().filter(|c| !c.is_whitespace()) {
        if let Some(d) = c.to_digit(16) {
            match hex.take() {
                Some(h) => out.push(h << 4 | d as u8),
                None => hex = Some(d as u8),
            }
            continue;
        }
        if hex.is_some() {
            return None;
        }
        let c = c.to_ascii_uppercase();
        match c {
            'G'..='O' => {
                for _ in 0..(c as u8 - b'G' + 1) {
                    out.extend_from_slice(&[0xfa, 0x00, 0x00]);
                }
            }
            _ => out.extend_from_slice(ABBREV.iter().find(|a| a.0 == c)?.1),
        }
    }
    if hex.is_some() { None } else { Some(out) }
}

// ---------------------------------------------------------------------------------------------
// CDP
// ---------------------------------------------------------------------------------------------

/// One frame's caption data: a field-1 CEA-608 pair (without parity) and DTVCC pairs
/// `(start_of_packet, b1, b2)`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FrameData {
    pub f1: Option<(u8, u8)>,
    pub dtvcc: Vec<(bool, u8, u8)>,
}

fn rate_code(r: FrameRate) -> u8 {
    match r {
        FrameRate::FPS_23_976 => 1,
        FrameRate::FPS_24 => 2,
        FrameRate::FPS_25 => 3,
        FrameRate::FPS_29_97 => 4,
        FrameRate::FPS_30 => 5,
        FrameRate::FPS_50 => 6,
        FrameRate::FPS_59_94 => 7,
        FrameRate::FPS_60 => 8,
        _ => 4,
    }
}

/// A CDP for one 29.97 fps frame, prefixed with the ancillary DID/SDID (`61 01`) and count.
pub fn build_cdp(seq: u16, data: &FrameData) -> Vec<u8> {
    let mut cc: Vec<u8> = Vec::with_capacity(CC_COUNT * 3);
    match data.f1 {
        Some((a, b)) => cc.extend_from_slice(&[0xfc, with_parity(a), with_parity(b)]),
        None => cc.extend_from_slice(&[0xfc, 0x80, 0x80]),
    }
    cc.extend_from_slice(&[0xfd, 0x80, 0x80]);
    for &(start, a, b) in data.dtvcc.iter().take(DTVCC_PAIRS) {
        cc.extend_from_slice(&[if start { 0xff } else { 0xfe }, a, b]);
    }
    while cc.len() < CC_COUNT * 3 {
        cc.extend_from_slice(&[0xfa, 0x00, 0x00]);
    }
    let len = 7 + 2 + cc.len() + 4;
    let [s1, s0] = seq.to_be_bytes();
    let mut cdp = vec![0x96, 0x69, len as u8, rate_code(RATE) << 4 | 0x0f, 0x43, s1, s0, 0x72, 0xe0 | CC_COUNT as u8];
    cdp.extend_from_slice(&cc);
    cdp.extend_from_slice(&[0x74, s1, s0]);
    let sum: u32 = cdp.iter().map(|&b| b as u32).sum();
    cdp.push(((256 - sum % 256) % 256) as u8);
    let mut out = vec![0x61, 0x01, cdp.len() as u8];
    out.extend(cdp);
    // SMPTE ST 291 ancillary checksum: low 8 bits of the sum of DID, SDID, data count and the user data words.
    let anc_sum: u32 = out.iter().map(|&b| b as u32).sum();
    out.push((anc_sum & 0xff) as u8);
    out
}

/// The cc_data triplets of one line's bytes (an ancillary packet with a CDP, or a bare CDP).
pub fn cc_triplets(b: &[u8]) -> Option<Vec<[u8; 3]>> {
    let cdp = if b.starts_with(&[0x61, 0x01]) { b.get(3..)? } else { b };
    if !cdp.starts_with(&[0x96, 0x69]) {
        return None;
    }
    let len = (*cdp.get(2)? as usize).min(cdp.len());
    let cdp = &cdp[..len];
    let mut i = 7;
    let mut out = Vec::new();
    while i < cdp.len() {
        match cdp[i] {
            0x71 => i += 5,
            0x72 => {
                let n = (*cdp.get(i + 1)? & 0x1f) as usize;
                for k in 0..n {
                    let at = i + 2 + 3 * k;
                    out.push([*cdp.get(at)?, *cdp.get(at + 1)?, *cdp.get(at + 2)?]);
                }
                i += 2 + 3 * n;
            }
            0x73 => i += 2 + 7 * (*cdp.get(i + 1)? & 0x0f) as usize,
            0x74 => break,
            0x75..=0xef => i += 2 + *cdp.get(i + 1)? as usize,
            _ => return None,
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------------------------
// CEA-708 service data
// ---------------------------------------------------------------------------------------------

/// G2 characters we write / read (after EXT1).
const G2: &[(u8, char)] = &[
    (0x25, '…'),
    (0x2a, 'Š'),
    (0x2c, 'Œ'),
    (0x31, '‘'),
    (0x32, '’'),
    (0x33, '“'),
    (0x34, '”'),
    (0x35, '•'),
    (0x39, '™'),
    (0x3a, 'š'),
    (0x3c, 'œ'),
    (0x3d, '℠'),
    (0x3f, 'Ÿ'),
    (0x76, '⅛'),
    (0x77, '⅜'),
    (0x78, '⅝'),
    (0x79, '⅞'),
];

/// Service-data atoms (a command with its parameters, or one character) for a character.
fn char_atom(c: char) -> Vec<u8> {
    match c {
        '♪' => vec![0x7f],
        ' '..='~' => vec![c as u8],
        '\u{a0}'..='\u{ff}' => vec![c as u32 as u8],
        _ => match G2.iter().find(|g| g.1 == c) {
            Some((b, _)) => vec![0x10, *b],
            None => vec![b'?'],
        },
    }
}

/// Atoms that define window `win` hidden and load `rows` into it.
fn load_atoms(win: u8, rows: &[String]) -> Vec<Vec<u8>> {
    let rc = rows.len().clamp(1, 12) as u8 - 1;
    let cols = rows.iter().map(|r| r.chars().count()).max().unwrap_or(1).clamp(1, 42) as u8 - 1;
    let mut v = vec![
        // DefineWindow: hidden, row/column lock, relative position 90 % down / 50 % across,
        // anchored at its bottom centre, rows / columns, window style 1, pen style 1
        vec![0x98 + win, 0x18, 0x80 | 90, 50, 7 << 4 | rc, cols, 1 << 3 | 1],
        vec![0x88, 1 << win], // ClearWindows
    ];
    for (i, r) in rows.iter().enumerate() {
        if i > 0 {
            v.push(vec![0x0d]);
        }
        v.extend(r.chars().map(char_atom));
    }
    v
}

/// Pack atoms into DTVCC packets for service 1 (each ≤ 34 bytes, so one fits in a frame).
fn packets(atoms: &[Vec<u8>], seq: &mut u8) -> Vec<Vec<u8>> {
    let mut blocks: Vec<Vec<u8>> = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    for a in atoms {
        if cur.len() + a.len() > 31 {
            blocks.push(std::mem::take(&mut cur));
        }
        cur.extend_from_slice(a);
    }
    if !cur.is_empty() {
        blocks.push(cur);
    }
    blocks
        .into_iter()
        .map(|b| {
            let mut p = vec![0, 1 << 5 | b.len() as u8];
            p.extend(b);
            if p.len() % 2 == 1 {
                p.push(0);
            }
            p[0] = (*seq & 3) << 6 | (p.len() / 2) as u8;
            *seq = seq.wrapping_add(1);
            p
        })
        .collect()
}

fn pairs_of(packet: &[u8]) -> Vec<(bool, u8, u8)> {
    packet.chunks(2).enumerate().map(|(i, c)| (i == 0, c[0], *c.get(1).unwrap_or(&0))).collect()
}

/// Frames → DTVCC pairs, with per-frame capacity.
struct Dtv {
    frames: BTreeMap<i64, Vec<(bool, u8, u8)>>,
}

impl Dtv {
    fn room(&self, f: i64) -> usize {
        DTVCC_PAIRS - self.frames.get(&f).map_or(0, Vec::len)
    }
    /// Put a packet in the first frame ≥ `f` with room; returns that frame.
    fn place(&mut self, mut f: i64, packet: &[u8]) -> i64 {
        let pairs = pairs_of(packet);
        while self.room(f) < pairs.len() {
            f += 1;
        }
        self.frames.entry(f).or_default().extend(pairs);
        f
    }
}

/// Schedule CEA-708 service 1 pop-on captions (see the module docs).
fn schedule_708(doc: &Document) -> BTreeMap<i64, Vec<(bool, u8, u8)>> {
    let mut d = Dtv { frames: BTreeMap::new() };
    let mut seq = 0u8;
    let mut cursor = 0i64;
    let mut cues: Vec<&Cue> = doc.cues.iter().filter(|c| c.end > c.start).collect();
    cues.sort_by_key(|c| c.start);
    for (i, c) in cues.iter().enumerate() {
        let win = (i % 2) as u8;
        let s = scc::frame_of(c.start).max(cursor);
        let e = scc::frame_of(c.end);
        let rows = scc::wrap_rows(&filmcraft_project::plain_text(&c.text));
        let load = packets(&load_atoms(win, &rows), &mut seq);
        let mut f = (s - load.len() as i64).max(cursor);
        let mut last = f;
        for p in &load {
            last = d.place(f, p);
            f = last;
        }
        let show = packets(&[vec![0x8a, 1 << (1 - win)], vec![0x89, 1 << win]], &mut seq);
        let shown = d.place(s.max(last), &show[0]);
        cursor = shown + 1;
        let next_start = cues.get(i + 1).map(|n| scc::frame_of(n.start));
        if next_start != Some(e) {
            let hide = packets(&[vec![0x8a, 1 << win]], &mut seq);
            d.place(e.max(cursor), &hide[0]);
        }
    }
    d.frames
}

/// CEA-708 decoder state for service 1.
#[derive(Clone, Default)]
struct Win {
    defined: bool,
    visible: bool,
    rows: Vec<String>,
}

struct Dec708 {
    win: [Win; 8],
    cur: usize,
    packet: Vec<u8>,
    want: usize,
    open: Option<(i64, String)>,
    cues: Vec<(i64, i64, String)>,
}

impl Dec708 {
    fn new() -> Self {
        Dec708 { win: Default::default(), cur: 0, packet: Vec::new(), want: 0, open: None, cues: Vec::new() }
    }
    fn visible_text(&self) -> String {
        let mut lines = Vec::new();
        for w in self.win.iter().filter(|w| w.defined && w.visible) {
            lines.extend(w.rows.iter().map(|r| r.trim().to_string()).filter(|r| !r.is_empty()));
        }
        lines.join("\n")
    }
    fn sync(&mut self, f: i64) {
        let now = self.visible_text();
        if self.open.as_ref().map(|o| o.1.as_str()) == Some(now.as_str()) || (self.open.is_none() && now.is_empty()) {
            return;
        }
        if let Some((s, t)) = self.open.take()
            && f > s
        {
            self.cues.push((s, f, t));
        }
        if !now.is_empty() {
            self.open = Some((f, now));
        }
    }
    fn pair(&mut self, f: i64, start: bool, a: u8, b: u8) {
        if start {
            self.packet.clear();
            let size = (a & 0x3f) as usize;
            self.want = if size == 0 { 128 } else { size * 2 };
        } else if self.want == 0 {
            return;
        }
        self.packet.extend_from_slice(&[a, b]);
        if self.packet.len() >= self.want {
            let p = std::mem::take(&mut self.packet);
            self.want = 0;
            self.service_blocks(&p[1..]);
            self.sync(f);
        }
    }
    fn service_blocks(&mut self, mut b: &[u8]) {
        while let Some(&h) = b.first() {
            let (svc, len) = (h >> 5, (h & 0x1f) as usize);
            if h == 0 {
                break;
            }
            let (svc, skip) = if svc == 7 { (b.get(1).map_or(0, |x| x & 0x3f), 2) } else { (svc, 1) };
            let data = b.get(skip..skip + len).unwrap_or(&[]);
            if svc == 1 {
                self.service_data(data);
            }
            b = b.get(skip + len..).unwrap_or(&[]);
        }
    }
    fn w(&mut self) -> &mut Win {
        &mut self.win[self.cur]
    }
    fn put(&mut self, c: char) {
        let w = self.w();
        match w.rows.last_mut() {
            Some(row) => row.push(c),
            None => w.rows.push(c.to_string()),
        }
    }
    fn bitmap(&mut self, m: u8, f: impl Fn(&mut Win)) {
        for (i, w) in self.win.iter_mut().enumerate() {
            if m & (1 << i) != 0 {
                f(w);
            }
        }
    }
    fn service_data(&mut self, d: &[u8]) {
        let mut i = 0;
        let arg = |k: usize| d.get(k).copied().unwrap_or(0);
        while i < d.len() {
            let c = d[i];
            i += 1;
            match c {
                0x03 | 0x00 => {}
                0x08 => {
                    if let Some(r) = self.w().rows.last_mut() {
                        r.pop();
                    }
                }
                0x0c => self.w().rows = vec![String::new()],
                0x0d => self.w().rows.push(String::new()),
                0x0e => {
                    if let Some(r) = self.w().rows.last_mut() {
                        r.clear();
                    }
                }
                0x10 => {
                    let x = arg(i);
                    i += 1;
                    match x {
                        0x00..=0x07 => {}
                        0x08..=0x0f => i += 1,
                        0x10..=0x17 => i += 2,
                        0x18..=0x1f => i += 3,
                        0x20 | 0x21 => self.put(' '),
                        0x20..=0x7f => {
                            if let Some((_, ch)) = G2.iter().find(|g| g.0 == x) {
                                self.put(*ch);
                            }
                        }
                        0x80..=0x87 => i += 4,
                        0x88..=0x8f => i += 5,
                        0x90..=0x9f => i += 1 + (arg(i) & 0x1f) as usize,
                        _ => {}
                    }
                }
                0x11..=0x17 => i += 1,
                0x18..=0x1f => i += 2,
                0x20..=0x7e => self.put(c as char),
                0x7f => self.put('♪'),
                0x80..=0x87 => self.cur = (c - 0x80) as usize,
                0x88 => {
                    self.bitmap(arg(i), |w| w.rows = vec![String::new()]);
                    i += 1;
                }
                0x89 => {
                    self.bitmap(arg(i), |w| w.visible = true);
                    i += 1;
                }
                0x8a => {
                    self.bitmap(arg(i), |w| w.visible = false);
                    i += 1;
                }
                0x8b => {
                    self.bitmap(arg(i), |w| w.visible = !w.visible);
                    i += 1;
                }
                0x8c => {
                    self.bitmap(arg(i), |w| *w = Win::default());
                    i += 1;
                }
                0x8d => i += 1,
                0x8e | 0x8f | 0x93..=0x96 => {}
                0x90 | 0x92 => i += 2,
                0x91 => i += 3,
                0x97 => i += 4,
                0x98..=0x9f => {
                    let n = (c - 0x98) as usize;
                    let vis = arg(i) & 0x20 != 0;
                    let w = &mut self.win[n];
                    if !w.defined {
                        *w = Win { defined: true, visible: vis, rows: vec![String::new()] };
                    } else {
                        w.visible = vis;
                    }
                    self.cur = n;
                    i += 6;
                }
                0xa0..=0xff => self.put(c as char),
                _ => {}
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// File
// ---------------------------------------------------------------------------------------------

/// Write `doc` as MCC: CEA-608 field 1 and CEA-708 service 1 (see the module docs).
pub fn write(doc: &Document, drop_frame: bool) -> String {
    write_with(doc, drop_frame, true, true)
}

/// [`write`] choosing which caption streams to include.
pub fn write_with(doc: &Document, drop_frame: bool, cea608: bool, cea708: bool) -> String {
    let f1 = if cea608 { scc::schedule(doc) } else { BTreeMap::new() };
    let dtv = if cea708 { schedule_708(doc) } else { BTreeMap::new() };
    let mut out = String::from("File Format=MacCaption_MCC V1.0\n\n");
    out.push_str("///////////////////////////////////////////////////////////////////////////////////\n");
    out.push_str("// Closed captions written by FilmCraft: SMPTE ST 334-2 caption distribution packets\n");
    out.push_str("// carrying CEA-608 field 1 and CEA-708 service 1 data, one packet per frame.\n");
    out.push_str("///////////////////////////////////////////////////////////////////////////////////\n\n");
    let n = doc.cues.len() as u64;
    let h = doc.cues.iter().fold(0xcbf2_9ce4_8422_2325u64 ^ n, |h, c| (h ^ c.start.0 as u64 ^ (c.end.0 as u64).rotate_left(17)).wrapping_mul(0x100_0000_01b3));
    out.push_str(&format!(
        "UUID={:08X}-{:04X}-4{:03X}-8{:03X}-{:012X}\n",
        h >> 32,
        (h >> 16) & 0xffff,
        h & 0xfff,
        (h >> 20) & 0xfff,
        h.rotate_left(29) & 0xffff_ffff_ffff
    ));
    out.push_str("Creation Program=FilmCraft\n");
    out.push_str(&format!("Time Code Rate={}\n\n", if drop_frame { "30DF" } else { "30" }));
    let first = f1.keys().next().copied().into_iter().chain(dtv.keys().next().copied()).min();
    let last = f1.keys().next_back().copied().into_iter().chain(dtv.keys().next_back().copied()).max();
    if let (Some(a), Some(b)) = (first, last) {
        for (seq, f) in (a..=b + 1).enumerate() {
            let data = FrameData { f1: f1.get(&f).copied(), dtvcc: dtv.get(&f).cloned().unwrap_or_default() };
            let (_, hh, mm, ss, ff) = frames_to_fields(f, RATE, drop_frame);
            let sep = if drop_frame { ';' } else { ':' };
            out.push_str(&format!("{hh:02}:{mm:02}:{ss:02}{sep}{ff:02}\t{}\n", compress(&build_cdp(seq as u16, &data))));
        }
    }
    out
}

fn tc_rate(v: &str) -> Option<(FrameRate, bool)> {
    Some(match v.trim().to_ascii_uppercase().as_str() {
        "30DF" => (FrameRate::FPS_29_97, true),
        "30" => (FrameRate::FPS_29_97, false),
        "60DF" => (FrameRate::FPS_59_94, true),
        "60" => (FrameRate::FPS_59_94, false),
        "24" => (FrameRate::FPS_24, false),
        "25" => (FrameRate::FPS_25, false),
        "50" => (FrameRate::FPS_50, false),
        _ => return None,
    })
}

pub fn parse(text: &str) -> Result<Document> {
    let mut lines = text.lines().enumerate();
    match lines.by_ref().find(|(_, l)| !l.trim().is_empty()) {
        Some((_, l)) if l.trim().starts_with("File Format=MacCaption_MCC") => {}
        _ => return Err(Error::NotFormat("MacCaption MCC")),
    }
    let mut rate = (RATE, true);
    let mut doc = Document::default();
    let mut f1: Vec<(i64, u8, u8)> = Vec::new();
    let mut dtv: Vec<(i64, bool, u8, u8)> = Vec::new();
    let mut any608 = false;
    for (n, l) in lines {
        let l = l.trim_end();
        if l.trim().is_empty() || l.starts_with("//") {
            continue;
        }
        if let Some(v) = l.strip_prefix("Time Code Rate=") {
            rate = tc_rate(v).ok_or_else(|| Error::Syntax { line: n + 1, msg: format!("unsupported time code rate \"{v}\"") })?;
            continue;
        }
        if l.contains('=') {
            continue; // other key=value header lines
        }
        let Some((tc, data)) = l.trim_start().split_once(['\t', ' ']) else {
            continue;
        };
        let tc = tc.split('.').next().unwrap_or(tc);
        let parts: Vec<i64> = tc.split([':', ';']).filter_map(|p| p.parse().ok()).collect();
        let [h, m, s, f] = parts[..] else {
            return Err(Error::Syntax { line: n + 1, msg: format!("bad timecode \"{tc}\"") });
        };
        let frame = fields_to_frames(h, m, s, f, rate.0, rate.1 || tc.contains(';'));
        let Some(bytes) = expand(data) else {
            doc.warnings.push(format!("line {}: bad caption data", n + 1));
            continue;
        };
        let Some(trips) = cc_triplets(&bytes) else {
            doc.warnings.push(format!("line {}: not a caption distribution packet", n + 1));
            continue;
        };
        for [m0, a, b] in trips {
            if m0 & 0x04 == 0 {
                continue;
            }
            match m0 & 0x03 {
                0 => {
                    any608 |= a & 0x7f != 0 || b & 0x7f != 0;
                    f1.push((frame, a, b));
                }
                1 => {}
                t => dtv.push((frame, t == 3, a, b)),
            }
        }
    }
    if any608 {
        doc.cues = scc::decode_pairs(f1, rate.0);
    } else {
        let mut d = Dec708::new();
        let mut last = 0;
        for (f, start, a, b) in dtv {
            d.pair(f, start, a, b);
            last = f + 1;
        }
        if let Some((s, t)) = d.open.take() {
            d.cues.push((s, last.max(s + 1), t));
        }
        doc.cues = d.cues.into_iter().map(|(s, e, text)| Cue { start: rate.0.tick_of(s), end: rate.0.tick_of(e), text, ..Default::default() }).collect();
    }
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue(s: i64, e: i64, text: &str) -> Cue {
        Cue { start: RATE.tick_of(s), end: RATE.tick_of(e), text: text.into(), ..Default::default() }
    }

    #[test]
    fn abbreviations_round_trip() {
        let b = [0x61, 0x01, 0x49, 0x96, 0x69, 0xfa, 0, 0, 0xfa, 0, 0, 0xfc, 0x80, 0x80, 0x00, 0x12, 0xe1, 0, 0, 0, 0xfb, 0x80, 0x80, 0xfd, 0x80, 0x80];
        let s = compress(&b);
        assert_eq!(s, "T49SHQZ12UPR");
        assert_eq!(expand(&s).unwrap(), b);
        assert_eq!(expand("T52S5"), None);
    }

    #[test]
    fn cdp_structure_and_checksum() {
        let d = FrameData { f1: Some((0x14, 0x20)), dtvcc: vec![(true, 0x02, 0x21), (false, 0x89, 0x01)] };
        let b = build_cdp(7, &d);
        assert_eq!(&b[..3], &[0x61, 0x01, 73]);
        // prefix (3) + CDP (73) + ancillary checksum byte (1)
        assert_eq!(b.len(), 77);
        let sum: u32 = b[..76].iter().map(|&x| x as u32).sum();
        assert_eq!(b[76], (sum & 0xff) as u8, "ancillary checksum");
        let cdp = &b[3..76];
        assert_eq!(cdp.len(), 73);
        assert_eq!(&cdp[..9], &[0x96, 0x69, 73, 0x4f, 0x43, 0, 7, 0x72, 0xf4]);
        assert_eq!(cdp.iter().map(|&x| x as u32).sum::<u32>() % 256, 0, "checksum");
        let t = cc_triplets(&b).unwrap();
        assert_eq!(t.len(), 20);
        assert_eq!(t[0], [0xfc, 0x94, 0x20]);
        assert_eq!(t[2], [0xff, 0x02, 0x21]);
        assert_eq!(t[19], [0xfa, 0, 0]);
    }

    #[test]
    fn writes_and_reads_608_and_708() {
        let doc = Document {
            cues: vec![cue(60, 120, "Hello, world!"), cue(120, 200, "Second caption\nwith two lines"), cue(260, 300, "Café “q” ♪")],
            ..Default::default()
        };
        let s = write(&doc, true);
        assert!(s.starts_with("File Format=MacCaption_MCC V1.0\n"));
        assert!(s.contains("Time Code Rate=30DF"));
        let back = parse(&s).unwrap();
        let got: Vec<(i64, i64, &str)> = back.cues.iter().map(|c| (RATE.frame_at(c.start), RATE.frame_at(c.end), c.text.as_str())).collect();
        assert_eq!(got, vec![(60, 120, "Hello, world!"), (120, 200, "Second caption\nwith two lines"), (260, 300, "Café “q” ♪")]);
        // 708 only
        let s708 = write_with(&doc, false, false, true);
        let back = parse(&s708).unwrap();
        let got: Vec<(i64, i64, &str)> = back.cues.iter().map(|c| (RATE.frame_at(c.start), RATE.frame_at(c.end), c.text.as_str())).collect();
        assert_eq!(got, vec![(60, 120, "Hello, world!"), (120, 200, "Second caption\nwith two lines"), (260, 300, "Café “q” ♪")]);
        assert!(parse("garbage").is_err());
    }
}
