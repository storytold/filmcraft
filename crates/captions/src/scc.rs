//! Scenarist SCC (`.scc`): CEA-608 byte pairs with SMPTE timecodes at 29.97 fps.
//!
//! ```text
//! Scenarist_SCC V1.0
//!
//! 00:00:00;00 9420 9420 94ae 94ae 1370 1370 c845 4c4c cf80 942f 942f
//!
//! 00:00:02;00 942c 942c
//! ```
//!
//! A tab separates the timecode from the code words. Each line's pairs are sent one per frame
//! starting at its timecode. **Reading** runs a channel-1
//! CEA-608 decoder (pop-on, roll-up, paint-on; redundant control codes ignored) and emits a cue
//! whenever the displayed memory changes. **Writing** produces pop-on captions: for each cue the
//! load (RCL, ENM, PAC + tab offset per row, text) is scheduled into free frames before the cue so
//! its End of Caption lands exactly on the cue start; an Erase Displayed Memory clears it at the
//! cue end (omitted when the next caption replaces it). Rows are bottom-aligned (row 15 last),
//! centred, wrapped at 32 columns; characters outside the 608 sets are dropped.

use std::collections::BTreeMap;

use filmcraft_time::{FrameRate, Tick, fields_to_frames, frames_to_fields};

use crate::cea608::{self, Encoded, misc};
use crate::{Cue, Document, Error, Result};

/// SCC is always 29.97 fps.
pub const RATE: FrameRate = FrameRate::FPS_29_97;
const COLS: usize = 32;
const HEADER: &str = "Scenarist_SCC V1.0";

/// Parse `HH:MM:SS:FF` / `HH:MM:SS;FF` (drop-frame when any separator is `;` or `.`).
pub fn parse_timecode(s: &str) -> Option<i64> {
    let df = s.contains([';', '.', ',']);
    let parts: Vec<&str> = s.split([':', ';', '.', ',']).collect();
    let [h, m, sec, f] = parts.as_slice() else { return None };
    let n = |p: &str| if (1..=3).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit()) { p.parse::<i64>().ok() } else { None };
    Some(fields_to_frames(n(h)?, n(m)?, n(sec)?, n(f)?, RATE, df))
}

pub fn format_timecode(frame: i64, drop_frame: bool) -> String {
    let (_, h, m, s, f) = frames_to_fields(frame.max(0), RATE, drop_frame);
    format!("{h:02}:{m:02}:{s:02}{}{f:02}", if drop_frame { ';' } else { ':' })
}

// ------------------------------------------------------------------------------------------------
// Decoder
// ------------------------------------------------------------------------------------------------

type Memory = [[Option<char>; COLS]; 15];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    PopOn,
    RollUp(usize),
    PaintOn,
}

struct Decoder {
    displayed: Memory,
    hidden: Memory,
    mode: Mode,
    row: usize,
    col: usize,
    /// Last control pair (for redundancy filtering).
    last_ctrl: Option<(u8, u8)>,
    /// Channel of the current data (true = channel 1).
    ch1: bool,
    /// Cue being shown: (start frame, text).
    open: Option<(i64, String)>,
    cues: Vec<(i64, i64, String)>,
}

fn memory_text(m: &Memory) -> String {
    m.iter().map(|r| r.iter().map(|c| c.unwrap_or(' ')).collect::<String>().trim().to_string()).filter(|r| !r.is_empty()).collect::<Vec<_>>().join("\n")
}

impl Decoder {
    fn new() -> Self {
        Self {
            displayed: [[None; COLS]; 15],
            hidden: [[None; COLS]; 15],
            mode: Mode::PopOn,
            row: 14,
            col: 0,
            last_ctrl: None,
            ch1: true,
            open: None,
            cues: Vec::new(),
        }
    }

    fn target(&mut self) -> &mut Memory {
        if self.mode == Mode::PopOn { &mut self.hidden } else { &mut self.displayed }
    }

    fn put(&mut self, c: char) {
        let (r, col) = (self.row, self.col.min(COLS - 1));
        self.target()[r][col] = Some(c);
        self.col = (self.col + 1).min(COLS - 1);
    }

    /// Displayed memory may have changed at frame `f`: close/open cues.
    fn sync(&mut self, f: i64) {
        let now = memory_text(&self.displayed);
        let cur = self.open.as_ref().map(|o| o.1.clone()).unwrap_or_default();
        if now == cur {
            return;
        }
        if let Some((s, text)) = self.open.take()
            && f > s
        {
            self.cues.push((s, f, text));
        }
        if !now.is_empty() {
            self.open = Some((f, now));
        }
    }

    fn pair(&mut self, f: i64, b1: u8, b2: u8) {
        let (b1, b2) = (b1 & 0x7f, b2 & 0x7f);
        if (0x10..=0x1f).contains(&b1) {
            if self.last_ctrl == Some((b1, b2)) {
                self.last_ctrl = None;
                return;
            }
            self.last_ctrl = Some((b1, b2));
            self.ch1 = b1 & 0x08 == 0;
            if !self.ch1 {
                return;
            }
            self.control(f, b1 & 0x17, b2);
            return;
        }
        self.last_ctrl = None;
        if !self.ch1 {
            return;
        }
        for b in [b1, b2] {
            if let Some(c) = cea608::basic_char(b) {
                self.put(c);
            }
        }
        if self.mode != Mode::PopOn {
            self.sync(f);
        }
    }

    fn control(&mut self, f: i64, b1: u8, b2: u8) {
        // b1 has the channel bit cleared; 0x14/0x15 both carry misc codes (field 1 / 2 variants)
        if (b1 == 0x14 || b1 == 0x15) && (0x20..=0x2f).contains(&b2) {
            match b2 {
                misc::RCL => self.mode = Mode::PopOn,
                misc::RDC => self.mode = Mode::PaintOn,
                misc::RU2 | misc::RU3 | misc::RU4 => {
                    let n = (b2 - misc::RU2) as usize + 2;
                    if !matches!(self.mode, Mode::RollUp(_)) {
                        self.displayed = [[None; COLS]; 15];
                        self.row = 14;
                    }
                    self.mode = Mode::RollUp(n);
                    self.col = 0;
                }
                misc::BS => {
                    self.col = self.col.saturating_sub(1);
                    let (r, c) = (self.row, self.col);
                    self.target()[r][c] = None;
                }
                misc::DER => {
                    let (r, c) = (self.row, self.col);
                    for x in c..COLS {
                        self.target()[r][x] = None;
                    }
                }
                misc::EDM => self.displayed = [[None; COLS]; 15],
                misc::ENM => self.hidden = [[None; COLS]; 15],
                misc::EOC => {
                    std::mem::swap(&mut self.displayed, &mut self.hidden);
                    self.mode = Mode::PopOn;
                }
                misc::CR => {
                    if let Mode::RollUp(n) = self.mode {
                        let top = self.row + 1 - n.min(self.row + 1);
                        for r in top..self.row {
                            self.displayed[r] = self.displayed[r + 1];
                        }
                        if top > 0 {
                            self.displayed[top - 1] = [None; COLS];
                        }
                        self.displayed[self.row] = [None; COLS];
                        self.col = 0;
                    }
                }
                _ => {}
            }
            self.sync(f);
            return;
        }
        if b1 == 0x17 && (0x21..=0x23).contains(&b2) {
            self.col = (self.col + (b2 - 0x20) as usize).min(COLS - 1);
            return;
        }
        if b1 == 0x11 && (0x30..=0x3f).contains(&b2) {
            self.put(cea608::SPECIAL[(b2 - 0x30) as usize]);
            if self.mode != Mode::PopOn {
                self.sync(f);
            }
            return;
        }
        if (b1 == 0x12 || b1 == 0x13) && (0x20..=0x3f).contains(&b2) {
            // extended characters replace the preceding (fallback) character
            self.col = self.col.saturating_sub(1);
            let set = if b1 == 0x12 { &cea608::EXT_12 } else { &cea608::EXT_13 };
            self.put(set[(b2 - 0x20) as usize]);
            if self.mode != Mode::PopOn {
                self.sync(f);
            }
            return;
        }
        if b1 == 0x11 && (0x20..=0x2f).contains(&b2) {
            // mid-row style change: shown as a space
            self.put(' ');
            return;
        }
        if let Some((row, indent)) = cea608::decode_pac(b1, b2) {
            let row = row - 1;
            if let Mode::RollUp(n) = self.mode
                && row != self.row
            {
                // roll-up window moves with the base row
                let old = self.displayed;
                self.displayed = [[None; COLS]; 15];
                for k in 0..n {
                    if self.row >= k && row >= k {
                        self.displayed[row - k] = old[self.row - k];
                    }
                }
            }
            self.row = row;
            self.col = indent;
        }
    }
}

/// Decode channel-1 CEA-608 byte pairs `(frame, b1, b2)` (with or without parity; frames
/// ascending) into cues at `rate`. A caption still showing after the last pair stays up for one
/// second more.
pub fn decode_pairs(pairs: impl IntoIterator<Item = (i64, u8, u8)>, rate: FrameRate) -> Vec<Cue> {
    let mut dec = Decoder::new();
    let mut last_frame = 0;
    for (f, b1, b2) in pairs {
        dec.pair(f, b1, b2);
        last_frame = last_frame.max(f + 1);
    }
    let one_second = rate.frame_at(Tick(filmcraft_time::TICKS_PER_SECOND)).max(1);
    if let Some((s, text)) = dec.open.take() {
        let end = last_frame.max(s + 1) + one_second;
        dec.cues.push((s, end, text));
    }
    dec.cues.into_iter().map(|(s, e, text)| Cue { start: rate.tick_of(s), end: rate.tick_of(e), text, ..Default::default() }).collect()
}

pub fn parse(text: &str) -> Result<Document> {
    let mut lines = text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty());
    match lines.next() {
        Some((_, l)) if l.trim().starts_with("Scenarist_SCC") => {}
        _ => return Err(Error::NotFormat("Scenarist SCC")),
    }
    let mut dec = Decoder::new();
    let mut last_frame = 0;
    let mut doc = Document::default();
    for (n, l) in lines {
        let mut it = l.split_whitespace();
        let Some(tc) = it.next() else { continue };
        let Some(frame) = parse_timecode(tc) else {
            return Err(Error::Syntax { line: n + 1, msg: format!("bad timecode \"{tc}\"") });
        };
        for (k, w) in it.enumerate() {
            let Ok(v) = u16::from_str_radix(w, 16) else {
                doc.warnings.push(format!("line {}: bad code word \"{w}\"", n + 1));
                continue;
            };
            let f = frame + k as i64;
            dec.pair(f, (v >> 8) as u8, v as u8);
            last_frame = last_frame.max(f + 1);
        }
    }
    // a caption still showing at the end of the file stays up for 1 s more
    if let Some((s, text)) = dec.open.take() {
        let end = last_frame.max(s + 1) + 30;
        dec.cues.push((s, end, text));
    }
    doc.cues = dec.cues.into_iter().map(|(s, e, text)| Cue { start: RATE.tick_of(s), end: RATE.tick_of(e), text, ..Default::default() }).collect();
    Ok(doc)
}

// ------------------------------------------------------------------------------------------------
// Encoder
// ------------------------------------------------------------------------------------------------

/// Wrap plain text into rows of at most 32 columns (word wrap, hard-splitting long words).
pub fn wrap_rows(text: &str) -> Vec<String> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let mut cur = String::new();
        for word in line.split_whitespace() {
            if rows.len() == 15 {
                return rows;
            }
            // SCC can emit at most 15 32-column rows.  Do not repeatedly copy an unbounded word
            // only to discard all but that small prefix below.
            let mut word: String = word.chars().take(COLS * (15 - rows.len())).collect();
            loop {
                let need = if cur.is_empty() { word.chars().count() } else { cur.chars().count() + 1 + word.chars().count() };
                if need <= COLS {
                    if !cur.is_empty() {
                        cur.push(' ');
                    }
                    cur.push_str(&word);
                    break;
                }
                if !cur.is_empty() {
                    rows.push(std::mem::take(&mut cur));
                    if rows.len() == 15 {
                        return rows;
                    }
                    continue;
                }
                let head: String = word.chars().take(COLS).collect();
                word = word.chars().skip(COLS).collect();
                rows.push(head);
                if rows.len() == 15 {
                    return rows;
                }
                if word.is_empty() {
                    break;
                }
            }
        }
        if !cur.is_empty() {
            rows.push(cur);
        }
    }
    rows.truncate(15);
    rows
}

/// Byte pairs (without parity) that load `text` into non-displayed memory.
fn load_pairs(text: &str) -> Vec<(u8, u8)> {
    let plain = filmcraft_project::plain_text(text);
    let rows = wrap_rows(&plain);
    let mut out = vec![(0x14, misc::RCL), (0x14, misc::RCL), (0x14, misc::ENM), (0x14, misc::ENM)];
    let first_row = 16 - rows.len();
    for (i, row) in rows.iter().enumerate() {
        let encoded: Vec<Encoded> = row.chars().filter_map(cea608::encode_char).collect();
        let width = encoded.len().min(COLS);
        let col = (COLS - width) / 2;
        let pac = cea608::encode_pac(first_row + i, col);
        out.push(pac);
        out.push(pac);
        if !col.is_multiple_of(4) {
            let to = (0x17, 0x20 + (col % 4) as u8);
            out.push(to);
            out.push(to);
        }
        let mut pending: Option<u8> = None;
        let flush = |pending: &mut Option<u8>, out: &mut Vec<(u8, u8)>| {
            if let Some(b) = pending.take() {
                out.push((b, 0x00));
            }
        };
        for e in encoded.into_iter().take(COLS) {
            match e {
                Encoded::Basic(b) => match pending.take() {
                    Some(a) => out.push((a, b)),
                    None => pending = Some(b),
                },
                Encoded::Pair { b1, b2, fallback } => {
                    if let Some(fb) = fallback {
                        match pending.take() {
                            Some(a) => out.push((a, fb)),
                            None => out.push((fb, 0x00)),
                        }
                    } else {
                        flush(&mut pending, &mut out);
                    }
                    out.push((b1, b2));
                    out.push((b1, b2));
                }
            }
        }
        flush(&mut pending, &mut out);
    }
    out
}

fn word((a, b): (u8, u8)) -> u16 {
    (cea608::with_parity(a) as u16) << 8 | cea608::with_parity(b) as u16
}

/// Schedule pop-on CEA-608 byte pairs (without parity) for `doc`, one pair per 29.97 fps frame,
/// so each caption's End of Caption lands on its start frame (see the module docs).
pub fn schedule(doc: &Document) -> BTreeMap<i64, (u8, u8)> {
    let mut occ: BTreeMap<i64, (u8, u8)> = BTreeMap::new();
    let mut cursor = 0i64; // first frame this cue's load may use
    let mut prev_end: Option<i64> = None;
    let edm = (0x14, misc::EDM);
    let eoc = (0x14, misc::EOC);
    let place_free_from = |occ: &mut BTreeMap<i64, (u8, u8)>, mut f: i64, p: (u8, u8)| -> i64 {
        while occ.contains_key(&f) {
            f += 1;
        }
        occ.insert(f, p);
        f
    };
    let mut cues: Vec<&Cue> = doc.cues.iter().filter(|c| c.end > c.start).collect();
    cues.sort_by_key(|c| c.start);
    for c in cues {
        let s = RATE.frame_at(RATE.snap_nearest(c.start)).max(cursor);
        let e = RATE.frame_at(RATE.snap_nearest(c.end));
        // clear the previous caption if there is a gap before this one
        if let Some(pe) = prev_end.take()
            && pe < s
        {
            let f = place_free_from(&mut occ, pe, edm);
            if !occ.contains_key(&(f + 1)) && f + 1 < s {
                occ.insert(f + 1, edm);
            }
        }
        let load = load_pairs(&c.text);
        let n = load.len() as i64;
        // Doubled codes must stay adjacent, so the load is one contiguous run of free frames:
        // the latest run ending by `s`, else the first run after `cursor` (delaying the caption).
        let run_free = |occ: &BTreeMap<i64, (u8, u8)>, b: i64| (b..b + n).all(|f| !occ.contains_key(&f));
        let b = (cursor..=s - n).rev().find(|&b| run_free(&occ, b)).unwrap_or_else(|| (cursor..).find(|&b| run_free(&occ, b)).unwrap_or(cursor));
        for (k, p) in load.into_iter().enumerate() {
            occ.insert(b + k as i64, p);
        }
        let mut d = s.max(b + n);
        while occ.contains_key(&d) {
            d += 1;
        }
        occ.insert(d, eoc);
        let mut next = d + 1;
        if let std::collections::btree_map::Entry::Vacant(v) = occ.entry(next) {
            v.insert(eoc);
            next += 1;
        }
        cursor = next;
        prev_end = Some(e.max(next));
    }
    if let Some(pe) = prev_end {
        let f = place_free_from(&mut occ, pe, edm);
        occ.insert(f + 1, edm);
    }
    occ
}

/// The parity-coded SCC code word of a byte pair.
pub fn code_word(p: (u8, u8)) -> u16 {
    word(p)
}

pub fn write(doc: &Document, drop_frame: bool) -> String {
    let occ = schedule(doc);
    let mut out = String::from(HEADER);
    out.push_str("\n\n");
    let mut line: Option<(i64, Vec<String>)> = None;
    let mut last = i64::MIN;
    for (f, p) in occ {
        if f != last + 1
            && let Some((start, words)) = line.take()
        {
            out.push_str(&format!("{}\t{}\n\n", format_timecode(start, drop_frame), words.join(" ")));
        }
        line.get_or_insert_with(|| (f, Vec::new())).1.push(format!("{:04x}", word(p)));
        last = f;
    }
    if let Some((start, words)) = line {
        out.push_str(&format!("{}\t{}\n\n", format_timecode(start, drop_frame), words.join(" ")));
    }
    out
}

/// Convert ticks to the SCC frame grid (nearest frame).
pub fn frame_of(t: Tick) -> i64 {
    RATE.frame_at(RATE.snap_nearest(t))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue(s: i64, e: i64, text: &str) -> Cue {
        Cue { start: RATE.tick_of(s), end: RATE.tick_of(e), text: text.into(), ..Default::default() }
    }

    #[test]
    fn timecode_rejects_signed_fields() {
        assert_eq!(parse_timecode("00:00:-5:00"), None);
        assert_eq!(parse_timecode("00:+1:00:00"), None);
        assert_eq!(parse_timecode("00:00:00:"), None);
        assert_eq!(parse_timecode("00:00:01:00"), Some(30));
    }

    #[test]
    fn timecodes() {
        assert_eq!(parse_timecode("00:01:00;02"), Some(1800));
        assert_eq!(parse_timecode("00:01:00:00"), Some(1800));
        assert_eq!(format_timecode(1800, true), "00:01:00;02");
        assert_eq!(format_timecode(1800, false), "00:01:00:00");
        assert_eq!(parse_timecode("00:10:00;00"), Some(17982));
    }

    #[test]
    fn decode_known_popon() {
        // "HELLO" on row 15 at 00:00:01;00, cleared at 00:00:03;00
        let scc = "Scenarist_SCC V1.0\n\n00:00:00;20\t9420 9420 94ae 94ae 94d0 94d0 c845 4c4c 4f80 942f 942f\n\n00:00:03;00\t942c 942c\n";
        let d = parse(scc).unwrap();
        assert_eq!(d.cues.len(), 1);
        assert_eq!(d.cues[0].text, "HELLO");
        assert_eq!(RATE.frame_at(d.cues[0].start), 20 + 9);
        assert_eq!(RATE.frame_at(d.cues[0].end), 90);
    }

    #[test]
    fn decode_rollup() {
        // RU2, CR, "AB", CR, "CD"
        let scc = "Scenarist_SCC V1.0\n\n00:00:00:00\t9425 9425 94ad 94ad c1c2 94ad 94ad 4344\n\n00:00:02:00\t942c 942c\n";
        let d = parse(scc).unwrap();
        let texts: Vec<&str> = d.cues.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["AB", "AB\nCD"]);
    }

    #[test]
    fn write_then_read() {
        let doc = Document {
            cues: vec![cue(60, 120, "Hello, world!"), cue(120, 200, "Second caption\nwith two lines"), cue(260, 300, "Ñandú ♪ {x} * café “q”")],
            ..Default::default()
        };
        let s = write(&doc, true);
        assert!(s.starts_with("Scenarist_SCC V1.0\n\n"));
        let back = parse(&s).unwrap();
        let got: Vec<(i64, i64, &str)> = back.cues.iter().map(|c| (RATE.frame_at(c.start), RATE.frame_at(c.end), c.text.as_str())).collect();
        assert_eq!(got, vec![(60, 120, "Hello, world!"), (120, 200, "Second caption\nwith two lines"), (260, 300, "Ñandú ♪ {x} * café “q”")]);
    }

    #[test]
    fn wraps_long_lines() {
        let rows = wrap_rows("the quick brown fox jumps over the lazy dog and keeps running far away");
        assert!(rows.iter().all(|r| r.chars().count() <= 32));
        assert_eq!(rows.join(" "), "the quick brown fox jumps over the lazy dog and keeps running far away");
    }

    #[test]
    fn wrapping_discards_an_unrepresentable_word_without_quadratic_copying() {
        let rows = wrap_rows(&"x".repeat(100_000));
        assert_eq!(rows.len(), 15);
        assert!(rows.iter().all(|row| row.len() == COLS));
    }

    #[test]
    fn cue_at_zero_is_delayed_not_lost() {
        let doc = Document { cues: vec![cue(0, 60, "Start")], ..Default::default() };
        let back = parse(&write(&doc, false)).unwrap();
        assert_eq!(back.cues.len(), 1);
        assert_eq!(back.cues[0].text, "Start");
        assert_eq!(RATE.frame_at(back.cues[0].end), 60);
    }
}
