//! FilmCraft captions.
//!
//! - **Files:** [SubRip](srt) (`.srt`), [WebVTT](vtt) (`.vtt`), [Scenarist SCC](scc) (`.scc`,
//!   CEA-608 pop-on, roll-up and paint-on on read; pop-on on write), [MacCaption MCC](mcc)
//!   (`.mcc`, SMPTE ST 334-2 packets with CEA-608 and CEA-708), [EBU STL](stl) (`.stl`, EBU
//!   Tech 3264) and [TTML](ttml) (`.ttml` IMSC1 Text profile, `.dfxp` DFXP) parse into a
//!   [`Document`] of [`Cue`]s and write back. Readers are forgiving (BOM, UTF-16, Windows-1252 fallback, CRLF/CR,
//!   missing SRT indexes, `,` or `.` milliseconds, missing hours, no blank line between cues);
//!   writers are strict. WebVTT cue identifiers, cue settings, `<v Speaker>` voices and
//!   STYLE/REGION/NOTE blocks are kept.
//! - **Time:** cue times are exact [`Tick`]s. SRT/VTT milliseconds and SCC 29.97 fps timecodes
//!   (drop-frame or not) convert exactly; [`Document::snap_to_frames`] snaps to a sequence's frames.
//! - **Model:** [`track_from_document`] / [`document_from_track`] convert to and from the project's
//!   [`CaptionTrack`].
//! - **Burn-in:** [`burn`] lays a caption out with the track style and draws it with the
//!   `filmcraft-text` engine (Inter SemiBold, shaped and kerned).

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod burn;
pub mod cea608;
pub mod mcc;
pub mod scc;
pub mod srt;
pub mod stl;
pub mod ttml;
pub mod vtt;

use filmcraft_project::{Caption, CaptionFormat, CaptionTrack, ClipId, TrackId};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick};

/// A caption file format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    Srt,
    WebVtt,
    Scc,
    /// MacCaption MCC (CEA-608 + CEA-708 in SMPTE ST 334-2 packets).
    Mcc,
    /// EBU STL (EBU Tech 3264).
    Stl,
    /// TTML, IMSC1 Text profile.
    Ttml,
    /// DFXP (TTML1).
    Dfxp,
}

impl Format {
    pub const ALL: [Format; 7] = [Format::Srt, Format::WebVtt, Format::Scc, Format::Mcc, Format::Stl, Format::Ttml, Format::Dfxp];
    pub fn name(self) -> &'static str {
        match self {
            Format::Srt => "SubRip (SRT)",
            Format::WebVtt => "WebVTT",
            Format::Scc => "Scenarist SCC (CEA-608)",
            Format::Mcc => "MacCaption MCC (CEA-608/708)",
            Format::Stl => "EBU STL",
            Format::Ttml => "TTML (IMSC1)",
            Format::Dfxp => "DFXP (TTML1)",
        }
    }
    pub fn extension(self) -> &'static str {
        match self {
            Format::Srt => "srt",
            Format::WebVtt => "vtt",
            Format::Scc => "scc",
            Format::Mcc => "mcc",
            Format::Stl => "stl",
            Format::Ttml => "ttml",
            Format::Dfxp => "dfxp",
        }
    }
    pub fn from_name(s: &str) -> Option<Format> {
        match s.trim_start_matches('.').to_ascii_lowercase().as_str() {
            "srt" | "subrip" => Some(Format::Srt),
            "vtt" | "webvtt" => Some(Format::WebVtt),
            "scc" | "cea608" | "cea-608" => Some(Format::Scc),
            "mcc" | "maccaption" | "cea708" | "cea-708" => Some(Format::Mcc),
            "stl" | "ebu-stl" | "ebustl" | "ebu" => Some(Format::Stl),
            "ttml" | "imsc" | "imsc1" | "xml" => Some(Format::Ttml),
            "dfxp" => Some(Format::Dfxp),
            _ => None,
        }
    }
    /// The caption track format a file of this kind maps to.
    pub fn track_format(self) -> CaptionFormat {
        match self {
            Format::Scc => CaptionFormat::Cea608,
            Format::Mcc => CaptionFormat::Cea708,
            Format::Stl => CaptionFormat::Teletext,
            _ => CaptionFormat::Subtitle,
        }
    }
    /// Whether the format is a binary file (EBU STL).
    pub fn is_binary(self) -> bool {
        self == Format::Stl
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum Error {
    #[error("not a {0} file")]
    NotFormat(&'static str),
    #[error("line {line}: {msg}")]
    Syntax { line: usize, msg: String },
}

pub type Result<T> = std::result::Result<T, Error>;

/// One caption cue.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cue {
    pub start: Tick,
    pub end: Tick,
    /// Text as written (lines separated by `\n`, inline tags kept).
    pub text: String,
    /// WebVTT voice (`<v Name>`).
    pub speaker: Option<String>,
    /// WebVTT cue identifier.
    pub id: Option<String>,
    /// WebVTT cue settings, verbatim.
    pub settings: String,
}

/// A parsed caption file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Document {
    pub cues: Vec<Cue>,
    /// WebVTT STYLE / REGION / NOTE blocks (verbatim, without trailing blank lines).
    pub blocks: Vec<String>,
    /// Non-fatal problems found while reading (skipped cues…).
    pub warnings: Vec<String>,
}

impl Document {
    /// Snap cue times to frame boundaries of `rate` (nearest), keeping every cue at least one frame
    /// long and removing overlaps (a cue is cut where the next one starts).
    pub fn snap_to_frames(&mut self, rate: FrameRate) {
        for c in &mut self.cues {
            c.start = rate.snap_nearest(c.start);
            c.end = rate.snap_nearest(c.end).max(c.start + rate.frame_duration());
        }
        self.normalize();
    }

    /// Sort by start, drop empty cues and cut overlaps.
    pub fn normalize(&mut self) {
        self.cues.retain(|c| c.end > c.start);
        self.cues.sort_by_key(|c| (c.start, c.end));
        let mut out: Vec<Cue> = Vec::with_capacity(self.cues.len());
        for c in self.cues.drain(..) {
            let mut c = c;
            if let Some(prev) = out.last_mut()
                && c.start < prev.end
            {
                if c.start > prev.start {
                    prev.end = c.start;
                } else {
                    // same start: keep the first, shift this one after it
                    c.start = prev.end;
                    if c.end <= c.start {
                        self.warnings.push(format!("dropped overlapping cue \"{}\"", c.text.lines().next().unwrap_or("")));
                        continue;
                    }
                }
            }
            out.push(c);
        }
        self.cues = out;
    }
}

/// Guess a caption format from content (and the file extension as a hint).
pub fn detect(bytes: &[u8], ext: Option<&str>) -> Option<Format> {
    if stl::sniff(bytes) {
        return Some(Format::Stl);
    }
    let text = decode_text(&bytes[..bytes.len().min(4096)]);
    let head = text.trim_start_matches('\u{feff}').trim_start();
    if head.starts_with("WEBVTT") {
        return Some(Format::WebVtt);
    }
    if head.starts_with("Scenarist_SCC") {
        return Some(Format::Scc);
    }
    if head.starts_with("File Format=MacCaption_MCC") {
        return Some(Format::Mcc);
    }
    let ext = ext.map(|e| e.trim_start_matches('.').to_ascii_lowercase());
    if (head.starts_with('<') && (head.contains("<tt ") || head.contains("<tt>") || head.contains(":tt ")))
        && (head.contains("ns/ttml") || head.contains("ttaf1"))
    {
        let legacy = head.contains("ttaf1") || head.contains("dfxp");
        return Some(if ext.as_deref() == Some("dfxp") || (legacy && ext.as_deref() != Some("ttml")) { Format::Dfxp } else { Format::Ttml });
    }
    let has_arrow_timing = head.lines().take(40).any(|l| srt::parse_timing_line(l).is_some());
    match ext.as_deref() {
        Some("srt") if has_arrow_timing || head.is_empty() => Some(Format::Srt),
        Some("vtt") if has_arrow_timing => Some(Format::WebVtt),
        Some("srt" | "vtt" | "scc" | "mcc" | "stl" | "ttml" | "dfxp" | "xml") => None,
        _ if has_arrow_timing => Some(Format::Srt),
        _ => None,
    }
}

/// Parse caption bytes.
pub fn parse(bytes: &[u8], format: Format) -> Result<Document> {
    if format == Format::Stl {
        return stl::parse(bytes);
    }
    let text = decode_text(bytes);
    match format {
        Format::Srt => srt::parse(&text),
        Format::WebVtt => vtt::parse(&text),
        Format::Scc => scc::parse(&text),
        Format::Mcc => mcc::parse(&text),
        Format::Ttml | Format::Dfxp => ttml::parse(&text),
        Format::Stl => stl::parse(bytes),
    }
}

/// Options for [`write`].
#[derive(Clone, Copy, Debug)]
pub struct WriteOptions {
    /// SCC / MCC: drop-frame timecode (the usual choice for 29.97 fps).
    pub drop_frame: bool,
    /// The sequence frame rate: TTML (IMSC1) writes exact frame times at it; EBU STL picks
    /// `STL25.01` or `STL30.01` from it (None = 25 fps). Other formats ignore it.
    pub rate: Option<FrameRate>,
}

impl Default for WriteOptions {
    fn default() -> Self {
        Self { drop_frame: true, rate: None }
    }
}

/// Write a document.
pub fn write(doc: &Document, format: Format, opts: WriteOptions) -> Vec<u8> {
    match format {
        Format::Srt => srt::write(doc).into_bytes(),
        Format::WebVtt => vtt::write(doc).into_bytes(),
        Format::Scc => scc::write(doc, opts.drop_frame).into_bytes(),
        Format::Mcc => mcc::write(doc, opts.drop_frame).into_bytes(),
        Format::Stl => stl::write(doc, opts.rate),
        Format::Ttml => ttml::write(doc, ttml::Flavor::Imsc1, opts.rate, "en").into_bytes(),
        Format::Dfxp => ttml::write(doc, ttml::Flavor::Dfxp, opts.rate, "en").into_bytes(),
    }
}

/// Decode text bytes: UTF-8 (BOM stripped), UTF-16 with BOM, else Windows-1252. Line endings are
/// normalised to `\n`.
pub fn decode_text(bytes: &[u8]) -> String {
    let s = if let Some(rest) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        String::from_utf8_lossy(rest).into_owned()
    } else if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        let le = bytes[0] == 0xff;
        let units: Vec<u16> =
            bytes[2..].as_chunks::<2>().0.iter().map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) }).collect();
        String::from_utf16_lossy(&units)
    } else {
        match std::str::from_utf8(bytes) {
            Ok(s) => s.to_string(),
            Err(_) => bytes.iter().map(|&b| cp1252(b)).collect(),
        }
    };
    let s = s.strip_prefix('\u{feff}').map(str::to_string).unwrap_or(s);
    if s.contains('\r') { s.replace("\r\n", "\n").replace('\r', "\n") } else { s }
}

fn cp1252(b: u8) -> char {
    const HI: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž', '\u{8f}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™',
        'š', '›', 'œ', '\u{9d}', 'ž', 'Ÿ',
    ];
    if (0x80..0xa0).contains(&b) { HI[(b - 0x80) as usize] } else { b as char }
}

pub(crate) const TICKS_PER_MS: i64 = TICKS_PER_SECOND / 1000;

/// Parse `[H…:]MM:SS[.,]fff` (the fraction may have 1–9 digits) into ticks.
pub fn parse_clock(s: &str) -> Option<Tick> {
    let s = s.trim();
    let (hms, frac) = match s.find([',', '.']) {
        Some(i) => (&s[..i], &s[i + 1..]),
        None => (s, ""),
    };
    let parts: Vec<&str> = hms.split(':').collect();
    let num = |p: &str| -> Option<i64> { if !p.is_empty() && p.len() <= 9 && p.bytes().all(|b| b.is_ascii_digit()) { p.parse().ok() } else { None } };
    let (h, m, sec) = match parts.as_slice() {
        [h, m, s] => (num(h)?, num(m)?, num(s)?),
        [m, s] => (0, num(m)?, num(s)?),
        _ => return None,
    };
    if m >= 60 || sec >= 60 || h > 9999 {
        return None;
    }
    let mut ticks = ((h * 60 + m) * 60 + sec) * TICKS_PER_SECOND;
    if !frac.is_empty() {
        if frac.len() > 9 || !frac.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let d: i64 = frac.parse().ok()?;
        ticks += (d as i128 * TICKS_PER_SECOND as i128 / 10i128.pow(frac.len() as u32)) as i64;
    }
    Some(Tick(ticks))
}

/// Format ticks as `HH:MM:SS<sep>mmm` (rounded to the nearest millisecond).
pub fn format_clock(t: Tick, sep: char) -> String {
    // The rounding addition must not overflow for an input-derived Tick near i64::MAX.
    let ms = (i128::from(t.0.max(0)) + i128::from(TICKS_PER_MS) / 2) / i128::from(TICKS_PER_MS);
    let (h, m, s, f) = (ms / 3_600_000, (ms / 60_000) % 60, (ms / 1000) % 60, ms % 1000);
    format!("{h:02}:{m:02}:{s:02}{sep}{f:03}")
}

/// Build a caption track from a document. Ids are allocated with `alloc`.
pub fn track_from_document(doc: &Document, id: TrackId, name: &str, format: CaptionFormat, alloc: &mut dyn FnMut() -> u64) -> CaptionTrack {
    let mut t = CaptionTrack::new(id, name.to_string(), format);
    t.vtt_blocks = doc.blocks.clone();
    t.captions = doc
        .cues
        .iter()
        .filter(|c| c.end > c.start)
        .map(|c| Caption {
            id: ClipId(alloc()),
            start: c.start,
            duration: c.end - c.start,
            text: c.text.clone(),
            speaker: c.speaker.clone(),
            cue_id: c.id.clone(),
            settings: c.settings.clone(),
        })
        .collect();
    t.sort();
    t
}

/// The document for a caption track (for export). `offset` is subtracted from every time (export
/// of a range starting later than zero).
pub fn document_from_track(t: &CaptionTrack, offset: Tick) -> Document {
    Document {
        cues: t
            .captions
            .iter()
            .filter(|c| c.end() > offset)
            .map(|c| Cue {
                start: (c.start - offset).max(Tick::ZERO),
                end: c.end() - offset,
                text: c.text.clone(),
                speaker: c.speaker.clone(),
                id: c.cue_id.clone(),
                settings: c.settings.clone(),
            })
            .collect(),
        blocks: t.vtt_blocks.clone(),
        warnings: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock() {
        assert_eq!(parse_clock("00:00:01,500"), Some(Tick(TICKS_PER_SECOND * 3 / 2)));
        assert_eq!(parse_clock("01:02.25"), Some(Tick(TICKS_PER_SECOND * 62 + TICKS_PER_SECOND / 4)));
        assert_eq!(parse_clock("100:00:00.000"), Some(Tick(TICKS_PER_SECOND * 360_000)));
        assert_eq!(parse_clock("00:61:00,000"), None);
        assert_eq!(parse_clock("aa:00:00,000"), None);
        assert_eq!(format_clock(Tick(TICKS_PER_SECOND * 3723 + TICKS_PER_MS * 45), ','), "01:02:03,045");
        assert!(format_clock(Tick(i64::MAX), ',').ends_with(",077"));
        assert_eq!(format_clock(Tick(i64::MIN), '.'), "00:00:00.000");
    }

    #[test]
    fn decode() {
        assert_eq!(decode_text(b"\xef\xbb\xbfa\r\nb\rc"), "a\nb\nc");
        assert_eq!(decode_text(&[0xff, 0xfe, b'h', 0, b'i', 0]), "hi");
        assert_eq!(decode_text(b"caf\xe9 \x93x\x94"), "café “x”");
    }

    #[test]
    fn detects() {
        assert_eq!(detect(b"WEBVTT\n\n00:01.000 --> 00:02.000\nhi\n", Some("vtt")), Some(Format::WebVtt));
        assert_eq!(detect(b"1\n00:00:01,000 --> 00:00:02,000\nhi\n", Some("srt")), Some(Format::Srt));
        assert_eq!(detect(b"Scenarist_SCC V1.0\n\n00:00:00;00\t9420\n", Some("scc")), Some(Format::Scc));
        assert_eq!(detect(b"hello", Some("srt")), None);
        assert_eq!(detect(b"<xml/>", None), None);
        assert_eq!(detect(b"File Format=MacCaption_MCC V1.0\n", Some("mcc")), Some(Format::Mcc));
        let ttml = b"<?xml version=\"1.0\"?><tt xmlns=\"http://www.w3.org/ns/ttml\"><body/></tt>";
        assert_eq!(detect(ttml, Some("ttml")), Some(Format::Ttml));
        assert_eq!(detect(ttml, Some("dfxp")), Some(Format::Dfxp));
        assert_eq!(detect(b"<tt xmlns=\"http://www.w3.org/2006/10/ttaf1\"></tt>", Some("xml")), Some(Format::Dfxp));
        let stl = stl::write(&Document::default(), None);
        assert_eq!(detect(&stl, Some("stl")), Some(Format::Stl));
        for f in Format::ALL {
            assert_eq!(Format::from_name(f.extension()), Some(f));
        }
    }

    #[test]
    fn normalize_cuts_overlaps() {
        let c = |s: i64, e: i64| Cue { start: Tick(s), end: Tick(e), text: "x".into(), ..Default::default() };
        let mut d = Document { cues: vec![c(10, 20), c(0, 15), c(10, 12)], ..Default::default() };
        d.normalize();
        let r: Vec<(i64, i64)> = d.cues.iter().map(|c| (c.start.0, c.end.0)).collect();
        assert_eq!(r, vec![(0, 10), (10, 12), (12, 20)]);
    }
}
