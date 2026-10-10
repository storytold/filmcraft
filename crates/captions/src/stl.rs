//! EBU STL (`.stl`): the EBU subtitling data exchange format, EBU Tech 3264-E (1991).
//!
//! A file is a 1024-byte General Subtitle Information (GSI) block of ASCII fields followed by
//! 128-byte Text and Timing Information (TTI) blocks. Each TTI block carries a subtitle number,
//! an extension block number (`0xFF` = last block of the subtitle), in / out timecodes (hours,
//! minutes, seconds, frames as binary bytes), vertical position, justification, a comment flag
//! and a 112-byte text field (`0x8A` = new line, `0x8F` = unused; `0x80`/`0x81` italics on/off,
//! `0x82`/`0x83` underline on/off; `0x00`–`0x1F` teletext control codes are skipped on read).
//!
//! - **Frame rates:** `STL25.01` is 25 fps; `STL30.01` is read and written as 29.97 fps with
//!   non-drop-frame timecode labels (how it is used in practice). Timecodes are made relative to
//!   the start-of-programme timecode (TCP).
//! - **Text:** character code table `00` (Latin, ISO/IEC 6937): ASCII, the 0xA0–0xFF specials and
//!   accented letters as a non-spacing diacritic (0xC1–0xCF) followed by the base letter. Other
//!   tables are read as Latin. Characters with no 6937 code are written as `?`.
//! - Long subtitles continue in extension blocks; user-data (`EBN 0xFE`) and comment blocks are
//!   skipped.

use filmcraft_time::{FrameRate, Tick, fields_to_frames, frames_to_fields};

use crate::{Cue, Document, Error, Result};

const GSI: usize = 1024;
const TTI: usize = 128;
const TF: usize = 112;

/// Read the frame rate from the Disk Format Code.
fn rate_of(dfc: &[u8]) -> Option<FrameRate> {
    match dfc {
        b"STL25.01" => Some(FrameRate::FPS_25),
        b"STL30.01" => Some(FrameRate::FPS_29_97),
        _ => None,
    }
}

/// Whether bytes look like an EBU STL file.
pub fn sniff(bytes: &[u8]) -> bool {
    bytes.len() >= GSI && bytes[3..11].starts_with(b"STL") && rate_of(&bytes[3..11]).is_some()
}

// ---------------------------------------------------------------------------------------------
// ISO/IEC 6937
// ---------------------------------------------------------------------------------------------

const SPECIALS: &[(u8, char)] = &[
    (0xa0, '\u{a0}'),
    (0xa1, '¡'),
    (0xa2, '¢'),
    (0xa3, '£'),
    (0xa4, '$'),
    (0xa5, '¥'),
    (0xa6, '#'),
    (0xa7, '§'),
    (0xa8, '¤'),
    (0xa9, '‘'),
    (0xaa, '“'),
    (0xab, '«'),
    (0xac, '←'),
    (0xad, '↑'),
    (0xae, '→'),
    (0xaf, '↓'),
    (0xb0, '°'),
    (0xb1, '±'),
    (0xb2, '²'),
    (0xb3, '³'),
    (0xb4, '×'),
    (0xb5, 'µ'),
    (0xb6, '¶'),
    (0xb7, '·'),
    (0xb8, '÷'),
    (0xb9, '’'),
    (0xba, '”'),
    (0xbb, '»'),
    (0xbc, '¼'),
    (0xbd, '½'),
    (0xbe, '¾'),
    (0xbf, '¿'),
    (0xd0, '―'),
    (0xd1, '¹'),
    (0xd2, '®'),
    (0xd3, '©'),
    (0xd4, '™'),
    (0xd5, '♪'),
    (0xd6, '¬'),
    (0xd7, '¦'),
    (0xdc, '⅛'),
    (0xdd, '⅜'),
    (0xde, '⅝'),
    (0xdf, '⅞'),
    (0xe0, 'Ω'),
    (0xe1, 'Æ'),
    (0xe2, 'Đ'),
    (0xe3, 'ª'),
    (0xe4, 'Ħ'),
    (0xe6, 'Ĳ'),
    (0xe7, 'Ŀ'),
    (0xe8, 'Ł'),
    (0xe9, 'Ø'),
    (0xea, 'Œ'),
    (0xeb, 'º'),
    (0xec, 'Þ'),
    (0xed, 'Ŧ'),
    (0xee, 'Ŋ'),
    (0xef, 'ŉ'),
    (0xf0, 'ĸ'),
    (0xf1, 'æ'),
    (0xf2, 'đ'),
    (0xf3, 'ð'),
    (0xf4, 'ħ'),
    (0xf5, 'ı'),
    (0xf6, 'ĳ'),
    (0xf7, 'ŀ'),
    (0xf8, 'ł'),
    (0xf9, 'ø'),
    (0xfa, 'œ'),
    (0xfb, 'ß'),
    (0xfc, 'þ'),
    (0xfd, 'ŧ'),
    (0xfe, 'ŋ'),
];

/// Non-spacing diacritics 0xC1–0xCF: (code, base letters, composed letters).
const DIACRITICS: &[(u8, &str, &str)] = &[
    (0xc1, "AEIOUaeiou", "ÀÈÌÒÙàèìòù"),
    (0xc2, "ACEILNORSUYZacegilnorsuyz", "ÁĆÉÍĹŃÓŔŚÚÝŹáćéģíĺńóŕśúýź"),
    (0xc3, "ACEGHIJOSUWYaceghijosuwy", "ÂĈÊĜĤÎĴÔŜÛŴŶâĉêĝĥîĵôŝûŵŷ"),
    (0xc4, "AINOUainou", "ÃĨÑÕŨãĩñõũ"),
    (0xc5, "AEIOUaeiou", "ĀĒĪŌŪāēīōū"),
    (0xc6, "AGUagu", "ĂĞŬăğŭ"),
    (0xc7, "CEGIZcegz", "ĊĖĠİŻċėġż"),
    (0xc8, "AEIOUYaeiouy", "ÄËÏÖÜŸäëïöüÿ"),
    (0xca, "AUau", "ÅŮåů"),
    (0xcb, "CGKLNRSTcklnrst", "ÇĢĶĻŅŖŞŢçķļņŗşţ"),
    (0xcd, "OUou", "ŐŰőű"),
    (0xce, "AEIUaeiu", "ĄĘĮŲąęįų"),
    (0xcf, "CDELNRSTZcdelnrstz", "ČĎĚĽŇŘŠŤŽčďěľňřšťž"),
];

fn compose(d: u8, base: u8) -> Option<char> {
    let (_, bases, comp) = DIACRITICS.iter().find(|x| x.0 == d)?;
    let i = bases.bytes().position(|b| b == base)?;
    comp.chars().nth(i)
}

/// ISO 6937 bytes for a character (None = not representable).
pub fn encode_6937(c: char) -> Option<Vec<u8>> {
    if (' '..='~').contains(&c) {
        return Some(vec![c as u8]);
    }
    if let Some((b, _)) = SPECIALS.iter().find(|x| x.1 == c && x.0 != 0xa4 && x.0 != 0xa6) {
        return Some(vec![*b]);
    }
    for (d, bases, comp) in DIACRITICS {
        if let Some(i) = comp.chars().position(|x| x == c) {
            return Some(vec![*d, bases.as_bytes()[i]]);
        }
    }
    None
}

/// Decode a text field (up to the first unused byte) into caption text with `<i>` / `<u>` tags.
fn decode_tf(tf: &[u8], out: &mut String, italic: &mut bool, underline: &mut bool) {
    let mut i = 0;
    while i < tf.len() {
        let b = tf[i];
        i += 1;
        match b {
            0x8f => {}
            0x8a => {
                // a line break; repeated 0x8A (double-height rows) count once
                if !out.ends_with('\n') || out.is_empty() {
                    out.push('\n');
                }
            }
            0x80 if !*italic => {
                out.push_str("<i>");
                *italic = true;
            }
            0x81 if *italic => {
                out.push_str("</i>");
                *italic = false;
            }
            0x82 if !*underline => {
                out.push_str("<u>");
                *underline = true;
            }
            0x83 if *underline => {
                out.push_str("</u>");
                *underline = false;
            }
            0x00..=0x1f | 0x7f..=0x9f => {}
            0x20..=0x7e => out.push(b as char),
            0xc1..=0xcf => {
                if let Some(&base) = tf.get(i)
                    && let Some(c) = compose(b, base)
                {
                    out.push(c);
                    i += 1;
                }
            }
            _ => {
                if let Some((_, c)) = SPECIALS.iter().find(|x| x.0 == b) {
                    out.push(*c);
                }
            }
        }
    }
}

/// Caption text → text-field bytes (`<i>`, `<u>` become control codes; other tags dropped).
fn encode_text(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(ch) = rest.chars().next() {
        if ch == '<'
            && let Some(end) = rest.find('>')
        {
            let tag = &rest[1..end];
            // Only a syntactically plausible tag is markup. Comparisons such as
            // "2 < 3 > 1" and "x < y > z" must not lose their middle text.
            let name = tag.strip_prefix('/').unwrap_or(tag);
            let is_tag = name.as_bytes().first().is_some_and(u8::is_ascii_alphabetic);
            if is_tag {
                match tag.trim().to_ascii_lowercase().as_str() {
                    "i" => out.push(0x80),
                    "/i" => out.push(0x81),
                    "u" => out.push(0x82),
                    "/u" => out.push(0x83),
                    _ => {}
                }
                rest = &rest[end + 1..];
                continue;
            }
        }
        if ch == '\n' {
            out.push(0x8a);
        } else {
            out.extend(encode_6937(ch).unwrap_or_else(|| vec![b'?']));
        }
        rest = &rest[ch.len_utf8()..];
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------------------------

fn ascii_field(b: &[u8]) -> String {
    String::from_utf8_lossy(b).trim().to_string()
}

fn tc_frames(b: &[u8], rate: FrameRate) -> i64 {
    fields_to_frames(b[0] as i64, b[1] as i64, b[2] as i64, b[3] as i64, rate, false)
}

pub fn parse(bytes: &[u8]) -> Result<Document> {
    if !sniff(bytes) {
        return Err(Error::NotFormat("EBU STL"));
    }
    let Some(rate) = bytes.get(3..11).and_then(rate_of) else {
        return Err(Error::NotFormat("EBU STL"));
    };
    let gsi = &bytes[..GSI];
    // start of programme (HHMMSSFF, ASCII)
    let tcp = ascii_field(&gsi[256..264]);
    let start = if tcp.len() == 8 && tcp.bytes().all(|b| b.is_ascii_digit()) {
        let n = |i: usize| tcp[i..i + 2].parse::<i64>().unwrap_or(0);
        fields_to_frames(n(0), n(2), n(4), n(6), rate, false)
    } else {
        0
    };
    let mut doc = Document::default();
    let mut cur: Option<(u16, i64, i64, String, bool, bool)> = None;
    let finish = |c: Option<(u16, i64, i64, String, bool, bool)>, doc: &mut Document| {
        if let Some((_, tin, tout, mut text, italic, underline)) = c {
            if italic {
                text.push_str("</i>");
            }
            if underline {
                text.push_str("</u>");
            }
            let text = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n");
            if tout > tin && !text.is_empty() {
                doc.cues.push(Cue { start: rate.tick_of((tin - start).max(0)), end: rate.tick_of((tout - start).max(0)), text, ..Default::default() });
            }
        }
    };
    for blk in bytes[GSI..].as_chunks::<TTI>().0 {
        let sn = u16::from_le_bytes([blk[1], blk[2]]);
        let ebn = blk[3];
        let cf = blk[15];
        if ebn == 0xfe || cf != 0 {
            continue; // user data / comment
        }
        let tin = tc_frames(&blk[5..9], rate);
        let tout = tc_frames(&blk[9..13], rate);
        let same = cur.as_ref().is_some_and(|c| c.0 == sn);
        if !same {
            finish(cur.take(), &mut doc);
            cur = Some((sn, tin, tout, String::new(), false, false));
        }
        if let Some(c) = cur.as_mut() {
            let (mut it, mut ul) = (c.4, c.5);
            decode_tf(&blk[16..16 + TF], &mut c.3, &mut it, &mut ul);
            c.4 = it;
            c.5 = ul;
        }
        if ebn == 0xff {
            finish(cur.take(), &mut doc);
        }
    }
    finish(cur.take(), &mut doc);
    if bytes.len() > GSI && !(bytes.len() - GSI).is_multiple_of(TTI) {
        doc.warnings.push("trailing bytes after the last TTI block".into());
    }
    Ok(doc)
}

// ---------------------------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------------------------

fn put(buf: &mut [u8], at: usize, len: usize, s: &str) {
    let b = s.as_bytes();
    for i in 0..len {
        buf[at + i] = *b.get(i).unwrap_or(&b' ');
    }
}

/// Write `doc` at `rate` (25 fps → `STL25.01`; 29.97 / 30 → `STL30.01`; others are written at 25).
pub fn write(doc: &Document, rate: Option<FrameRate>) -> Vec<u8> {
    let (rate, dfc) = match rate {
        Some(r) if r == FrameRate::FPS_29_97 || r == FrameRate::FPS_30 => (FrameRate::FPS_29_97, "STL30.01"),
        _ => (FrameRate::FPS_25, "STL25.01"),
    };
    // TTI blocks first (the GSI counts them)
    let mut ttis: Vec<[u8; TTI]> = Vec::new();
    let mut n_subs = 0u16;
    let mut first_in = None;
    for c in doc.cues.iter().filter(|c| c.end > c.start) {
        let sn = n_subs;
        n_subs = n_subs.wrapping_add(1);
        let tin = rate.frame_at(rate.snap_nearest(c.start));
        let tout = rate.frame_at(rate.snap_nearest(c.end)).max(tin + 1);
        first_in.get_or_insert(tin);
        let text = encode_text(&c.text);
        let chunks: Vec<&[u8]> = if text.is_empty() { vec![&[][..]] } else { split_tf(&text) };
        let lines = c.text.lines().count().clamp(1, 11);
        for (k, chunk) in chunks.iter().enumerate() {
            let mut b = [0u8; TTI];
            b[0] = 0;
            b[1..3].copy_from_slice(&sn.to_le_bytes());
            b[3] = if k + 1 == chunks.len() { 0xff } else { k.min(0xfd) as u8 };
            b[4] = 0;
            for (at, f) in [(5, tin), (9, tout)] {
                let (_, h, m, s, fr) = frames_to_fields(f, rate, false);
                b[at..at + 4].copy_from_slice(&[h.min(255) as u8, m as u8, s as u8, fr as u8]);
            }
            b[13] = (22 - 2 * (lines - 1)) as u8;
            b[14] = 2; // centred
            b[15] = 0;
            b[16..].fill(0x8f);
            b[16..16 + chunk.len()].copy_from_slice(chunk);
            ttis.push(b);
        }
    }
    let mut gsi = [b' '; GSI];
    put(&mut gsi, 0, 3, "850");
    put(&mut gsi, 3, 8, dfc);
    put(&mut gsi, 11, 1, "0");
    put(&mut gsi, 12, 2, "00");
    put(&mut gsi, 14, 2, "09");
    put(&mut gsi, 16, 32, "FilmCraft");
    put(&mut gsi, 224, 6, "000101");
    put(&mut gsi, 230, 6, "000101");
    put(&mut gsi, 236, 2, "00");
    put(&mut gsi, 238, 5, &format!("{:05}", ttis.len().min(99_999)));
    put(&mut gsi, 243, 5, &format!("{:05}", n_subs));
    put(&mut gsi, 248, 3, "001");
    put(&mut gsi, 251, 2, "40");
    put(&mut gsi, 253, 2, "23");
    put(&mut gsi, 255, 1, "1");
    put(&mut gsi, 256, 8, "00000000");
    let (_, h, m, s, f) = frames_to_fields(first_in.unwrap_or(0), rate, false);
    put(&mut gsi, 264, 8, &format!("{h:02}{m:02}{s:02}{f:02}"));
    put(&mut gsi, 272, 1, "1");
    put(&mut gsi, 273, 1, "1");
    let mut out = gsi.to_vec();
    for t in ttis {
        out.extend_from_slice(&t);
    }
    out
}

/// Split text-field bytes into ≤ 112-byte chunks without splitting a diacritic from its letter.
fn split_tf(text: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let mut end = (i + TF).min(text.len());
        if end < text.len() && (0xc1..=0xcf).contains(&text[end - 1]) {
            end -= 1;
        }
        out.push(&text[i..end]);
        i = end;
    }
    out
}

/// Frame of `t` on the STL grid of `rate` (see [`write`]).
pub fn frame_of(t: Tick, rate: FrameRate) -> i64 {
    rate.frame_at(rate.snap_nearest(t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso6937_round_trip() {
        for c in "Ñandú café Škoda Łódź Ærø ß £ ½ ♪ « » ‘q’ “q”".chars() {
            let b = encode_6937(c).unwrap_or_else(|| panic!("{c}"));
            let mut s = String::new();
            decode_tf(&b, &mut s, &mut false, &mut false);
            assert_eq!(s, c.to_string());
        }
        assert_eq!(encode_6937('€'), None);
    }

    #[test]
    fn writes_gsi_and_extension_blocks() {
        let long = "x".repeat(150);
        let doc = Document {
            cues: vec![
                Cue { start: FrameRate::FPS_25.tick_of(25), end: FrameRate::FPS_25.tick_of(75), text: "<i>Hello</i>\nWörld".into(), ..Default::default() },
                Cue { start: FrameRate::FPS_25.tick_of(100), end: FrameRate::FPS_25.tick_of(150), text: long.clone(), ..Default::default() },
            ],
            ..Default::default()
        };
        let b = write(&doc, Some(FrameRate::FPS_25));
        assert_eq!(&b[3..11], b"STL25.01");
        assert_eq!(b.len(), GSI + 3 * TTI, "the long subtitle takes two blocks");
        assert_eq!(&b[238..243], b"00003");
        assert_eq!(&b[243..248], b"00002");
        let tti0 = &b[GSI..GSI + TTI];
        assert_eq!(&tti0[5..9], &[0, 0, 1, 0]);
        assert_eq!(&tti0[9..13], &[0, 0, 3, 0]);
        assert_eq!(tti0[3], 0xff);
        assert_eq!(&tti0[16..24], &[0x80, b'H', b'e', b'l', b'l', b'o', 0x81, 0x8a]);
        assert_eq!(b[GSI + TTI + 3], 0);
        assert_eq!(b[GSI + 2 * TTI + 3], 0xff);
        let back = parse(&b).unwrap();
        assert_eq!(back.cues.len(), 2);
        assert_eq!(back.cues[0].text, "<i>Hello</i>\nWörld");
        assert_eq!(back.cues[1].text, long);
        assert_eq!(back.cues[1].start, FrameRate::FPS_25.tick_of(100));
        assert!(parse(&b[..500]).is_err());
    }

    #[test]
    fn literal_angle_brackets_are_not_discarded_as_markup() {
        let literal = "2 < 3 > 1, x < y > z, and 5 <7> 6";
        let doc = Document {
            cues: vec![Cue { start: FrameRate::FPS_25.tick_of(25), end: FrameRate::FPS_25.tick_of(75), text: literal.into(), ..Default::default() }],
            ..Default::default()
        };
        let encoded = write(&doc, Some(FrameRate::FPS_25));
        let decoded = parse(&encoded).expect("valid EBU STL");
        assert_eq!(decoded.cues.len(), 1);
        assert_eq!(decoded.cues[0].text, literal);
        // Recognized styling still becomes STL control bytes.
        assert_eq!(encode_text("<i>Hi</i>"), vec![0x80, b'H', b'i', 0x81]);
        assert_eq!(encode_text("<i >Hi</i >"), vec![0x80, b'H', b'i', 0x81]);
    }

    #[test]
    fn programme_start_is_subtracted() {
        let doc = Document {
            cues: vec![Cue { start: FrameRate::FPS_25.tick_of(250), end: FrameRate::FPS_25.tick_of(300), text: "A".into(), ..Default::default() }],
            ..Default::default()
        };
        let mut b = write(&doc, None);
        // re-stamp as a programme starting at 10:00:00:00 with absolute in-cues
        b[256..264].copy_from_slice(b"10000000");
        b[GSI + 5] = 10;
        b[GSI + 9] = 10;
        let back = parse(&b).unwrap();
        assert_eq!(back.cues[0].start, FrameRate::FPS_25.tick_of(250));
    }
}
