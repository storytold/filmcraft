//! Chapter lists, in the two forms players read (#2747).
//!
//! - **QuickTime chapter track** (QuickTime File Format, "Chapter lists" and "Text media"): a
//!   disabled text track whose samples are the chapter titles, referenced from a video or audio track
//!   by a `tref/chap` track reference. QuickTime Player, Apple TV, iOS and Final Cut read this form.
//!   Each sample is a 16-bit big-endian length, the title, and an `encd` atom with value `0x100`
//!   marking the text as UTF-8 (the convention Apple's own chapter tracks in `.m4a`/`.m4v` files use;
//!   plain QuickTime text is Mac Roman).
//! - **Nero chapter list** `moov/udta/chpl`, version 1: `u32` reserved, `u8` chapter count, then per
//!   chapter a `u64` start in 100 ns units, a `u8` title length and the UTF-8 title. It has no
//!   official specification; this is the layout publicly documented for Nero Digital files and
//!   written by many tools. VLC and ffprobe (`-show_chapters`) read it.
//!
//! Both forms carry at most [`MAX_CHAPTERS`] chapters with titles of at most
//! [`MAX_CHAPTER_TITLE_BYTES`] bytes (the `chpl` count and length fields are one byte each).

use crate::bytes::{BoxBuf, Cur};

/// One chapter mark: `start` in the movie timescale ([`crate::WriterOptions::movie_timescale`]
/// when writing, [`crate::Mp4File::timescale`] when reading), `title` UTF-8.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chapter {
    pub start: u64,
    pub title: String,
}

/// Most chapters written (the `chpl` count is one byte); later ones are dropped.
pub const MAX_CHAPTERS: usize = 255;
/// Longest chapter title in bytes (the `chpl` length is one byte); longer titles are cut at a
/// character boundary.
pub const MAX_CHAPTER_TITLE_BYTES: usize = 255;

/// `chpl` times are in 100 ns units.
const CHPL_UNITS_PER_SECOND: u64 = 10_000_000;

/// Cut `s` to at most `max` bytes without splitting a character.
pub(crate) fn truncate_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.get(..end).unwrap_or("")
}

/// The chapters a file can carry: sorted by start, one per start time (the first title given for
/// a time wins), starts before `duration` only (when it is known), titles cut to
/// [`MAX_CHAPTER_TITLE_BYTES`] with control characters replaced by spaces, at most
/// [`MAX_CHAPTERS`].
pub fn normalize_chapters(chapters: &[Chapter], duration: Option<u64>) -> Vec<Chapter> {
    let mut v: Vec<&Chapter> = chapters.iter().filter(|c| duration.is_none_or(|d| c.start < d)).collect();
    v.sort_by_key(|c| c.start);
    v.dedup_by_key(|c| c.start);
    v.into_iter()
        .take(MAX_CHAPTERS)
        .map(|c| {
            let clean: String = c.title.chars().map(|ch| if ch.is_control() { ' ' } else { ch }).collect();
            Chapter { start: c.start, title: truncate_utf8(clean.trim(), MAX_CHAPTER_TITLE_BYTES).to_string() }
        })
        .collect()
}

fn rescale(v: u64, from: u64, to: u64) -> u64 {
    if from == 0 {
        return 0;
    }
    (u128::from(v) * u128::from(to) / u128::from(from)).min(u128::from(u64::MAX)) as u64
}

/// Write `udta/chpl` content (the box itself) for already normalized chapters.
pub(crate) fn write_chpl(b: &mut BoxBuf, chapters: &[Chapter], movie_timescale: u32) {
    let m = b.start_full(b"chpl", 1, 0);
    b.u32(0);
    let n = chapters.len().min(MAX_CHAPTERS);
    b.u8(n as u8);
    for c in chapters.iter().take(n) {
        b.u64(rescale(c.start, u64::from(movie_timescale), CHPL_UNITS_PER_SECOND));
        let t = truncate_utf8(&c.title, MAX_CHAPTER_TITLE_BYTES);
        b.u8(t.len() as u8);
        b.bytes(t.as_bytes());
    }
    b.end(m);
}

/// Parse a `chpl` payload (after the box header) into chapters in `movie_timescale` units.
/// Malformed or truncated lists yield the chapters read so far.
pub(crate) fn parse_chpl(p: &[u8], movie_timescale: u32) -> Vec<Chapter> {
    let mut out = Vec::new();
    let mut c = Cur::new(p, "chpl");
    let Ok((version, _)) = c.full_header() else { return out };
    if version >= 1 && c.skip(4).is_err() {
        return out;
    }
    let Ok(n) = c.u8() else { return out };
    for _ in 0..n {
        let Ok(start) = c.u64() else { break };
        let Ok(len) = c.u8() else { break };
        let Ok(title) = c.bytes(len as usize) else { break };
        out.push(Chapter { start: rescale(start, CHPL_UNITS_PER_SECOND, u64::from(movie_timescale)), title: String::from_utf8_lossy(title).into_owned() });
    }
    out
}

/// Payload of the QuickTime `text` sample description (after its 8-byte box header), QTFF "Text
/// sample description": reserved + data reference index, display flags, justification, background
/// colour, default text box, reserved, font number and face, reserved, foreground colour, an empty
/// Pascal font name, then an `ftab` font table naming one font.
pub(crate) fn text_sample_entry_payload() -> Vec<u8> {
    let mut b = BoxBuf::new();
    b.zeros(6);
    b.u16(1); // data reference index
    b.u32(0); // display flags
    b.i32(0); // justification: left
    b.zeros(6); // background colour (RGB)
    b.zeros(8); // default text box (top, left, bottom, right)
    b.zeros(8); // reserved
    b.u16(1); // font number (matches the ftab entry)
    b.u16(0); // font face: plain
    b.u8(0); // reserved
    b.u16(0); // reserved
    b.zeros(6); // foreground colour (RGB)
    b.u8(0); // text name: empty Pascal string
    let f = b.start(b"ftab");
    b.u16(1);
    b.u16(1);
    let name = b"Sans-Serif";
    b.u8(name.len() as u8);
    b.bytes(name);
    b.end(f);
    b.buf
}

/// One chapter-track sample: length-prefixed UTF-8 title plus an `encd` atom (UTF-8).
pub(crate) fn text_sample(title: &str) -> Vec<u8> {
    let t = truncate_utf8(title, MAX_CHAPTER_TITLE_BYTES);
    let mut b = BoxBuf::new();
    b.u16(t.len() as u16);
    b.bytes(t.as_bytes());
    let e = b.start(b"encd");
    b.u32(0x100);
    b.end(e);
    b.buf
}

/// The title in a QuickTime text sample (UTF-8, or UTF-16 when it starts with a byte order mark).
pub fn parse_text_sample(data: &[u8]) -> Option<String> {
    let mut c = Cur::new(data, "text sample");
    let n = c.u16().ok()? as usize;
    let t = c.bytes(n).ok()?;
    if let Some(rest) = t.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest.as_chunks::<2>().0.iter().map(|u| u16::from_be_bytes(*u)).collect();
        return Some(String::from_utf16_lossy(&units));
    }
    if let Some(rest) = t.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest.as_chunks::<2>().0.iter().map(|u| u16::from_le_bytes(*u)).collect();
        return Some(String::from_utf16_lossy(&units));
    }
    // trailing atoms (`encd`, `styl` …) are not part of the text
    Some(String::from_utf8_lossy(t).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(start: u64, title: &str) -> Chapter {
        Chapter { start, title: title.into() }
    }

    #[test]
    fn normalize_sorts_dedups_drops_and_caps() {
        let v = normalize_chapters(&[ch(500, "b"), ch(0, "a"), ch(500, "dup"), ch(2000, "past end"), ch(1999, "\tend\n")], Some(2000));
        assert_eq!(v, vec![ch(0, "a"), ch(500, "b"), ch(1999, "end")]);
        let many: Vec<Chapter> = (0..1000).map(|i| ch(i, "x")).collect();
        assert_eq!(normalize_chapters(&many, None).len(), MAX_CHAPTERS);
        let long = "é".repeat(300);
        let t = &normalize_chapters(&[ch(0, &long)], None)[0].title;
        assert!(t.len() <= MAX_CHAPTER_TITLE_BYTES && t.chars().all(|c| c == 'é'));
        assert!(normalize_chapters(&[ch(0, "a")], Some(0)).is_empty());
    }

    #[test]
    fn chpl_roundtrip() {
        let chapters = vec![ch(0, "Intro"), ch(1500, "Teil 2 — ü"), ch(u64::MAX, "far")];
        let mut b = BoxBuf::new();
        write_chpl(&mut b, &chapters, 1000);
        // box header (8) + version/flags (4) + reserved (4) + count (1)
        assert_eq!(&b.buf[4..8], b"chpl");
        assert_eq!(b.buf[8], 1);
        assert_eq!(b.buf[16], 3);
        assert_eq!(u64::from_be_bytes(b.buf[17..25].try_into().unwrap()), 0);
        assert_eq!(u64::from_be_bytes(b.buf[31..39].try_into().unwrap()), 15_000_000);
        let back = parse_chpl(&b.buf[8..], 1000);
        assert_eq!(back[..2], chapters[..2]);
        assert_eq!(back.len(), 3);
    }

    #[test]
    fn chpl_parse_survives_garbage() {
        let mut b = BoxBuf::new();
        write_chpl(&mut b, &[ch(0, "a"), ch(10, "bb")], 1000);
        let p = b.buf[8..].to_vec();
        for cut in 0..p.len() {
            let _ = parse_chpl(&p[..cut], 1000);
        }
        let mut lying = p.clone();
        lying[8] = 255; // count says 255
        assert_eq!(parse_chpl(&lying, 1000).len(), 2);
        assert!(parse_chpl(&p, 0).iter().all(|c| c.start == 0));
    }

    #[test]
    fn text_sample_roundtrip() {
        let s = text_sample("Kapitel “1”");
        assert_eq!(parse_text_sample(&s).as_deref(), Some("Kapitel “1”"));
        assert_eq!(&s[s.len() - 12..s.len() - 4], &[0, 0, 0, 12, b'e', b'n', b'c', b'd']);
        assert_eq!(parse_text_sample(&[0, 4, 0xFE, 0xFF, 0, b'A']).as_deref(), Some("A"));
        assert_eq!(parse_text_sample(&[0, 9, b'a']), None);
        assert_eq!(parse_text_sample(&[]), None);
    }
}
