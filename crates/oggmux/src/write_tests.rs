//! The page writer, read back by [`open`].

use super::*;
use craft_media::ogg::{Codec, OpusTiming, open};

/// A TOC-only 20 ms CELT packet of `len` bytes whose bytes encode `i` (so packets differ).
fn opus_packet(i: usize, len: usize) -> Vec<u8> {
    let mut p = vec![0xF8u8];
    p.extend((0..len.saturating_sub(1)).map(|k| (i * 31 + k) as u8));
    p
}

fn opus_head(pre_skip: u16) -> Vec<u8> {
    let mut h = b"OpusHead".to_vec();
    h.extend([1, 2]);
    h.extend(pre_skip.to_le_bytes());
    h.extend(48_000u32.to_le_bytes());
    h.extend([0, 0, 0]);
    h
}

/// An Ogg Opus file of `n` packets of `sizes(i)` bytes, `samples` playable samples after the pre-skip.
fn write_opus(n: usize, sizes: impl Fn(usize) -> usize, pre_skip: u16, samples: i64) -> (Vec<u8>, Vec<Vec<u8>>) {
    let mut w = PageWriter::new(0x1234_5678);
    let mut f = w.header(&opus_head(pre_skip));
    f.extend(w.header(&opus_tags("FilmCraft", &[("TITLE", "Test"), ("bad=key", "dropped"), ("", "dropped")])));
    let mut packets = Vec::new();
    for i in 0..n {
        let p = opus_packet(i, sizes(i));
        f.extend(w.packet(&p, (i as i64 + 1) * 960));
        packets.push(p);
    }
    f.extend(w.finish(Some(i64::from(pre_skip) + samples)));
    (f, packets)
}

#[test]
fn written_pages_read_back_packet_for_packet() {
    let (f, packets) = write_opus(230, |i| 3 + (i * 37) % 400, 120, 230 * 960 - 120 - 500);
    let ogg = open(&f).unwrap();
    assert!(ogg.warnings.is_empty(), "{:?}", ogg.warnings);
    assert_eq!(ogg.streams.len(), 1);
    let s = &ogg.streams[0];
    assert_eq!((s.serial, s.codec, s.saw_eos), (0x1234_5678, Codec::Opus, true));
    assert_eq!(s.headers.len(), 2);
    assert_eq!(s.headers[0], opus_head(120));
    assert_eq!(s.headers[1], opus_tags("FilmCraft", &[("TITLE", "Test")]));
    assert_eq!(s.packets.len(), packets.len());
    for (k, p) in packets.iter().enumerate() {
        assert_eq!(&ogg.read_packet(&f, 0, k).unwrap(), p, "packet {k}");
    }
    assert!(s.packets.last().unwrap().eos);
    assert_eq!(s.last_granule, Some(120 + 230 * 960 - 120 - 500));
    // RFC 7845 §4: the end trimming of the final page.
    let t = OpusTiming::of(s, 120);
    assert_eq!(t.total, 230 * 960 - 120 - 500);
    assert_eq!(t.starts[0], -120);
    assert!(t.durations.iter().all(|&d| d == 960));
}

#[test]
fn headers_have_pages_of_their_own_and_only_the_first_page_opens_the_stream() {
    let (f, _) = write_opus(3, |_| 10, 0, 3 * 960);
    let mut pos = 0;
    let mut flags = Vec::new();
    let mut seqs = Vec::new();
    while pos + 27 <= f.len() {
        assert_eq!(&f[pos..pos + 4], b"OggS");
        let mut page = f[pos..].to_vec();
        let nseg = page[26] as usize;
        let len = 27 + nseg + page[27..27 + nseg].iter().map(|&l| l as usize).sum::<usize>();
        page.truncate(len);
        let stored = u32::from_le_bytes(page[22..26].try_into().unwrap());
        page[22..26].fill(0);
        assert_eq!(crc32(&page), stored, "page CRC");
        flags.push(f[pos + 5]);
        seqs.push(u32::from_le_bytes(f[pos + 18..pos + 22].try_into().unwrap()));
        pos += len;
    }
    assert_eq!(pos, f.len());
    assert_eq!(flags, [0x02, 0x00, 0x04], "BOS head page, tags page, one audio page with EOS");
    assert_eq!(seqs, [0, 1, 2]);
}

#[test]
fn a_packet_longer_than_a_page_continues() {
    let big = 2 * 255 * 255 + 7000; // spans three pages: the middle one completes no packet
    let (f, packets) = write_opus(4, |i| if i == 1 { big } else { 50 }, 0, 4 * 960);
    let ogg = open(&f).unwrap();
    assert!(ogg.warnings.is_empty());
    for (k, p) in packets.iter().enumerate() {
        assert_eq!(&ogg.read_packet(&f, 0, k).unwrap(), p, "packet {k}");
    }
    // a page on which no packet completes has granule position -1
    let mut pos = 0;
    let mut saw_none = false;
    while pos + 27 <= f.len() {
        let nseg = f[pos + 26] as usize;
        let lacing = &f[pos + 27..pos + 27 + nseg];
        let g = i64::from_le_bytes(f[pos + 6..pos + 14].try_into().unwrap());
        if lacing.iter().all(|&l| l == 255) {
            assert_eq!(g, -1);
            saw_none = true;
        }
        pos += 27 + nseg + lacing.iter().map(|&l| l as usize).sum::<usize>();
    }
    assert!(saw_none);
}

#[test]
fn page_targets_bound_page_size_and_finish_marks_the_end_once() {
    let mut w = PageWriter::new(7).with_page_target(1000, 10);
    let mut f = w.header(&opus_head(0));
    f.extend(w.header(&opus_tags("v", &[])));
    for i in 0..95 {
        f.extend(w.packet(&opus_packet(i, 160), (i as i64 + 1) * 960));
    }
    f.extend(w.finish(None));
    assert!(w.finish(None).is_empty());
    let ogg = open(&f).unwrap();
    let s = &ogg.streams[0];
    assert_eq!(s.packets.len(), 95);
    assert_eq!(s.pages, 2 + 95_u32.div_ceil(7), "1000-byte pages hold 7 packets of 160 bytes");
    assert_eq!(s.last_granule, Some(95 * 960));
    // An empty stream still ends.
    let mut w = PageWriter::new(1);
    let mut f = w.header(&opus_head(0));
    f.extend(w.finish(Some(0)));
    assert!(open(&f).unwrap().streams[0].saw_eos);
}

#[test]
fn vorbis_comment_has_the_framing_bit() {
    let c = vorbis_comment("FilmCraft", &[("ARTIST", "x")]);
    assert_eq!(&c[..7], b"\x03vorbis");
    assert_eq!(*c.last().unwrap(), 1);
    assert_eq!(u32::from_le_bytes(c[7..11].try_into().unwrap()), 9);
}

/// Packets on and around the lacing boundaries (multiples of 255, where a packet ends with a
/// zero lacing value) and an empty packet read back exactly.
#[test]
fn lacing_boundaries_read_back() {
    let sizes = [0usize, 1, 254, 255, 256, 509, 510, 511, 765, 65_024, 65_025, 65_026];
    let mut w = PageWriter::new(3);
    let mut f = w.header(&opus_head(0));
    f.extend(w.header(&opus_tags("v", &[])));
    let packets: Vec<Vec<u8>> = sizes.iter().enumerate().map(|(i, &n)| (0..n).map(|k| (i * 7 + k) as u8).collect()).collect();
    for (i, p) in packets.iter().enumerate() {
        f.extend(w.packet(p, (i as i64 + 1) * 960));
    }
    f.extend(w.finish(None));
    let ogg = open(&f).unwrap();
    assert!(ogg.warnings.is_empty(), "{:?}", ogg.warnings);
    assert_eq!(ogg.streams[0].packets.len(), packets.len());
    for (k, p) in packets.iter().enumerate() {
        assert_eq!(ogg.read_packet(&f, 0, k).unwrap(), *p, "packet {k} of {} bytes", p.len());
    }
}

/// A page exactly full of lacing values when a packet starts: the packet begins on the next page,
/// which must not be flagged as continuing one (the reader would drop its first packet). Review
/// of #4481; reachable with page targets larger than the defaults.
#[test]
fn a_packet_after_an_exactly_full_page_is_not_a_continuation() {
    let mut w = PageWriter::new(5).with_page_target(1 << 20, 1000);
    let mut f = w.header(&opus_head(0));
    f.extend(w.header(&opus_tags("v", &[])));
    let packets: Vec<Vec<u8>> = (0..300).map(|i| opus_packet(i, 1)).collect();
    for (i, p) in packets.iter().enumerate() {
        f.extend(w.packet(p, (i as i64 + 1) * 960));
    }
    f.extend(w.finish(None));
    let ogg = open(&f).unwrap();
    assert!(ogg.warnings.is_empty(), "{:?}", ogg.warnings);
    assert_eq!(ogg.streams[0].packets.len(), 300, "every packet is read back");
    for (k, p) in packets.iter().enumerate() {
        assert_eq!(&ogg.read_packet(&f, 0, k).unwrap(), p, "packet {k}");
    }
    // the second audio page starts with packet 255 and is not a continuation
    let mut pos = 0;
    let mut flags = Vec::new();
    while pos + 27 <= f.len() {
        let nseg = f[pos + 26] as usize;
        flags.push(f[pos + 5]);
        pos += 27 + nseg + f[pos + 27..pos + 27 + nseg].iter().map(|&l| l as usize).sum::<usize>();
    }
    assert!(flags.iter().all(|&fl| fl & 0x01 == 0), "no page continues a packet: {flags:?}");
}
