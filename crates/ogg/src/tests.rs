//! Synthetic Ogg streams: a small page writer (RFC 3533) and Opus-like packets.

use super::*;

#[test]
fn recovery_scan_stops_when_its_shared_budget_is_spent() {
    let source = b"junkOggS".to_vec();
    let mut budget = 3;
    assert_eq!(find_capture(&source, 0, source.len() as u64, &mut budget).unwrap(), None);
    let mut budget = source.len();
    assert_eq!(find_capture(&source, 0, source.len() as u64, &mut budget).unwrap(), Some(4));
}

#[test]
fn recovery_scan_finds_a_capture_pattern_at_any_distance_and_charges_about_that_distance() {
    // distances around the window sizes 4 KiB, 8 KiB, 16 KiB … so a pattern straddling two windows is found too
    let mut distances = vec![0usize, 1, 2, 3, 4, 100, 5000, 100_000, 1 << 20, (1 << 20) + 5000];
    for edge in [4096usize, 12_288, 28_672, 61_440] {
        distances.extend(edge - 5..=edge + 2);
    }
    for d in distances {
        let mut source = vec![0x55u8; d];
        source.extend_from_slice(b"OggS");
        source.extend_from_slice(&[0x55; 10]);
        let mut budget = 64 << 20;
        assert_eq!(find_capture(&source, 0, source.len() as u64, &mut budget).unwrap(), Some(d as u64), "distance {d}");
        let charged = (64usize << 20) - budget;
        assert!(charged <= 2 * d + 4096 + 4, "distance {d} charged {charged}");
    }
    // not found: the whole source is scanned once, plus the overlap of three bytes per window
    let source = vec![0x55u8; 300_000];
    let mut budget = 64 << 20;
    assert_eq!(find_capture(&source, 0, source.len() as u64, &mut budget).unwrap(), None);
    assert!((64usize << 20) - budget < 300_000 + 3 * 16, "charged {}", (64usize << 20) - budget);
}

/// An Ogg Opus file of whole pages of about 64 KB (one packet each), `pages` data pages; the packet of data page
/// `i` is `pkt(i, 64_000)`.
fn big_page_file(pages: usize) -> Vec<u8> {
    let mut f = Vec::new();
    let head = opus_head(0);
    let (l, b) = lace(&[&head], false);
    page(&mut f, 0x02, 0, 9, 0, &l, &b);
    let (l, b) = lace(&[b"OpusTags"], false);
    page(&mut f, 0, 0, 9, 1, &l, &b);
    for i in 0..pages {
        let p = pkt(i, 64_000);
        let (l, b) = lace(&[&p], false);
        let last = i + 1 == pages;
        page(&mut f, if last { 0x04 } else { 0 }, (i as i64 + 1) * 960, 9, 2 + i as u32, &l, &b);
    }
    f
}

/// Offsets of the pages of `f`.
fn page_offsets(f: &[u8]) -> Vec<usize> {
    let mut v = Vec::new();
    let mut pos = 0;
    while pos + 27 <= f.len() {
        v.push(pos);
        let nseg = usize::from(f[pos + 26]);
        let body: usize = f[pos + 27..pos + 27 + nseg].iter().map(|&b| usize::from(b)).sum();
        pos += 27 + nseg + body;
    }
    v
}

#[test]
fn many_damaged_pages_far_apart_do_not_exhaust_the_recovery_budget() {
    // 70 damaged pages, each more than 1 MiB after the previous one: with a fixed 1 MiB scan window per
    // resynchronisation the 64 MiB budget ran out after about 64 of them and the rest of the file was dropped
    let pages = 1200;
    let mut f = big_page_file(pages);
    let offsets = page_offsets(&f);
    assert_eq!(offsets.len(), pages + 2);
    let damaged: Vec<usize> = (0..70).map(|k| 3 + k * 17).collect();
    assert!(damaged.last().is_some_and(|&d| d < pages));
    for &d in &damaged {
        f[offsets[2 + d] + 1000] ^= 0xFF;
    }
    let o = open(&f).unwrap();
    assert!(!o.warnings.iter().any(|w| w.contains("gave up")), "{:?}", o.warnings.last());
    let st = &o.streams[0];
    // every undamaged packet is kept
    assert_eq!(st.packets.len(), pages - damaged.len());
    assert_eq!(o.warnings.iter().filter(|w| w.contains("bad CRC")).count(), damaged.len());
    assert_eq!(o.read_packet(&f, 0, st.packets.len() - 1).unwrap(), pkt(pages - 1, 64_000));
    assert_eq!(o.read_packet(&f, 0, 0).unwrap(), pkt(0, 64_000));
    assert!(st.saw_eos);
}

#[test]
fn running_out_of_recovery_budget_in_the_scan_is_reported() {
    // one valid page, then more bytes without a capture pattern than the budget allows
    let mut f = opus_file(3, 0, 0);
    let valid = f.len();
    f.resize(valid + (65 << 20), 0x55);
    let o = open(&f).unwrap();
    assert_eq!(o.streams[0].packets.len(), 3);
    assert!(o.warnings.last().is_some_and(|w| w.starts_with("gave up resynchronising after byte ")), "{:?}", o.warnings);
    // a short tail of junk is not a failure to resynchronise
    let mut g = opus_file(3, 0, 0);
    g.extend_from_slice(&[0x55; 5000]);
    let o = open(&g).unwrap();
    assert_eq!(o.streams[0].packets.len(), 3);
    assert!(!o.warnings.iter().any(|w| w.contains("gave up")), "{:?}", o.warnings);
}

/// A source that counts the bytes read from it.
struct Counting<'a> {
    data: &'a [u8],
    read: std::cell::Cell<u64>,
}

impl ByteSource for Counting<'_> {
    fn len(&self) -> u64 {
        self.data.len() as u64
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.read.set(self.read.get() + buf.len() as u64);
        self.data.read_at(offset, buf)
    }
}

/// The most one candidate reads: header, segment table, then the page (header and table again, 255 segments of 255).
const MAX_CANDIDATE_READS: usize = 27 + 255 + (27 + 255 + 255 * 255);

#[test]
fn dense_fake_pages_with_bad_crcs_are_charged_to_the_recovery_budget() {
    // "OggS\0" repeated: every 5th byte is a plausible page header that fails its CRC
    let data: Vec<u8> = b"OggS\0".iter().copied().cycle().take(25_000).collect();
    let src = Counting { data: &data, read: std::cell::Cell::new(0) };
    assert!(open(&src).is_err(), "no valid page in the file");
    let read = src.read.get();
    // the shared budget bounds every byte of recovery work, including the pages read to test a candidate
    assert!(read <= (64u64 << 20) + MAX_CANDIDATE_READS as u64, "read {read} bytes of a {} byte file", data.len());
}

/// Write one page holding `segments` (lacing already computed by `lace`).
fn page(out: &mut Vec<u8>, flags: u8, granule: i64, serial: u32, seq: u32, lacing: &[u8], body: &[u8]) {
    let start = out.len();
    out.extend_from_slice(b"OggS");
    out.push(0);
    out.push(flags);
    out.extend_from_slice(&granule.to_le_bytes());
    out.extend_from_slice(&serial.to_le_bytes());
    out.extend_from_slice(&seq.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.push(lacing.len() as u8);
    out.extend_from_slice(lacing);
    out.extend_from_slice(body);
    let crc = crc32(&out[start..]);
    out[start + 22..start + 26].copy_from_slice(&crc.to_le_bytes());
}

fn lace(packets: &[&[u8]], last_continues: bool) -> (Vec<u8>, Vec<u8>) {
    let mut l = Vec::new();
    let mut b = Vec::new();
    for (i, p) in packets.iter().enumerate() {
        let mut n = p.len();
        while n >= 255 {
            l.push(255);
            n -= 255;
        }
        if !(last_continues && i + 1 == packets.len()) {
            l.push(n as u8);
        }
        b.extend_from_slice(p);
    }
    (l, b)
}

fn opus_head(pre_skip: u16) -> Vec<u8> {
    let mut h = b"OpusHead".to_vec();
    h.push(1);
    h.push(2);
    h.extend_from_slice(&pre_skip.to_le_bytes());
    h.extend_from_slice(&48_000u32.to_le_bytes());
    h.extend_from_slice(&0i16.to_le_bytes());
    h.push(0);
    h
}

/// A CELT 20 ms packet (960 samples): TOC config 31, code 0.
fn pkt(i: usize, len: usize) -> Vec<u8> {
    let mut p = vec![0xF8];
    p.extend((0..len.saturating_sub(1)).map(|k| (i * 7 + k) as u8));
    p
}

/// Header pages, then pages of 3 packets each; the last page's granule trims `trim` samples.
fn opus_file(npk: usize, pre_skip: u16, trim: i64) -> Vec<u8> {
    let mut f = Vec::new();
    let head = opus_head(pre_skip);
    let (l, b) = lace(&[&head], false);
    page(&mut f, 0x02, 0, 9, 0, &l, &b);
    let tags = b"OpusTags\x00\x00\x00\x00\x00\x00\x00\x00".to_vec();
    let (l, b) = lace(&[&tags], false);
    page(&mut f, 0, 0, 9, 1, &l, &b);
    let pkts: Vec<Vec<u8>> = (0..npk).map(|i| pkt(i, 40 + i % 5)).collect();
    let mut done = 0;
    for (seq, chunk) in (2..).zip(pkts.chunks(3)) {
        done += chunk.len();
        let last = done == npk;
        let mut g = done as i64 * 960;
        if last {
            g -= trim;
        }
        let refs: Vec<&[u8]> = chunk.iter().map(Vec::as_slice).collect();
        let (l, b) = lace(&refs, false);
        page(&mut f, if last { 0x04 } else { 0 }, g, 9, seq, &l, &b);
    }
    f
}

#[test]
fn crc_matches_known_value() {
    // CRC-32/CKSUM parameters without its final XOR (check value 0x765E7680 ^ 0xFFFFFFFF)
    assert_eq!(crc32(b"123456789"), 0x89A1_897F);
    assert_eq!(crc32(b""), 0);
    assert_ne!(crc32(b"OggS"), crc32(b"OggT"));
}

#[test]
fn opus_pages_packets_and_timing() {
    let f = opus_file(10, 312, 500);
    assert!(sniff(&f));
    let o = open(&f).unwrap();
    assert!(o.warnings.is_empty(), "{:?}", o.warnings);
    let s = o.stream_of(Codec::Opus).unwrap();
    let st = &o.streams[s];
    assert_eq!(st.headers.len(), 2);
    assert_eq!(st.packets.len(), 10);
    assert!(st.saw_eos);
    assert_eq!(o.read_packet(&f, s, 4).unwrap(), pkt(4, 44));
    let t = OpusTiming::of(st, 312);
    assert_eq!(t.durations, vec![960; 10]);
    assert_eq!(t.starts[0], -312);
    assert_eq!(t.starts[9], 9 * 960 - 312);
    // end trimming: the last page granule is 500 short
    assert_eq!(t.total, 10 * 960 - 500 - 312);
}

#[test]
fn packets_spanning_pages() {
    let mut f = Vec::new();
    let head = opus_head(0);
    let (l, b) = lace(&[&head], false);
    page(&mut f, 0x02, 0, 1, 0, &l, &b);
    let (l, b) = lace(&[b"OpusTags"], false);
    page(&mut f, 0, 0, 1, 1, &l, &b);
    let big = pkt(0, 700);
    // first 510 bytes on one page (two 255 lacing values, packet continues), rest on the next
    let (l, b) = (vec![255, 255], big[..510].to_vec());
    page(&mut f, 0, -1, 1, 2, &l, &b);
    let rest = &big[510..];
    let small = pkt(1, 30);
    let (l, b) = lace(&[rest, &small], false);
    page(&mut f, 0x01 | 0x04, 1920, 1, 3, &l, &b);
    let o = open(&f).unwrap();
    let st = &o.streams[0];
    assert_eq!(st.packets.len(), 2);
    assert_eq!(st.packets[0].parts.len(), 2);
    assert_eq!(o.read_packet(&f, 0, 0).unwrap(), big);
    assert_eq!(o.read_packet(&f, 0, 1).unwrap(), small);
    let t = OpusTiming::of(st, 0);
    assert_eq!(t.starts, vec![0, 960]);
    assert_eq!(t.total, 1920);
}

#[test]
fn corrupt_pages_are_skipped_and_truncation_is_safe() {
    let f = opus_file(12, 312, 0);
    // flip a byte inside the third data page's body: that page fails its CRC and is dropped
    let mut g = f.clone();
    let third = g.windows(4).enumerate().filter(|(_, w)| *w == b"OggS").map(|(i, _)| i).nth(4).unwrap();
    g[third + 40] ^= 0xFF;
    let o = open(&g).unwrap();
    assert!(o.warnings.iter().any(|w| w.contains("CRC")), "{:?}", o.warnings);
    assert_eq!(o.streams[0].packets.len(), 9);
    let t = OpusTiming::of(&o.streams[0], 312);
    // the granules after the gap put the later packets at their true positions
    assert_eq!(*t.starts.last().unwrap(), 11 * 960 - 312);
    for cut in 0..f.len() {
        if let Ok(o) = open(&f[..cut].to_vec()) {
            for st in &o.streams {
                let t = OpusTiming::of(st, 312);
                assert_eq!(t.starts.len(), st.packets.len());
            }
        }
    }
    let mut seed = 99u64;
    for _ in 0..300 {
        let mut g = f.clone();
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let at = (seed >> 33) as usize % g.len();
        g[at] = (seed >> 20) as u8;
        if let Ok(o) = open(&g) {
            for st in &o.streams {
                let _ = OpusTiming::of(st, 312);
            }
        }
    }
    assert!(matches!(open(&b"not ogg at all, no capture pattern".to_vec()), Err(Error::NotOgg)));
}

#[test]
fn opus_toc_durations() {
    assert_eq!(opus_packet_samples([0x00, 0], 10), Some(480)); // SILK NB 10 ms
    assert_eq!(opus_packet_samples([0x18, 0], 10), Some(2880)); // SILK NB 60 ms
    assert_eq!(opus_packet_samples([0x78 | 1, 0], 10), Some(1920)); // hybrid 20 ms × 2
    assert_eq!(opus_packet_samples([0x80 | 3, 6], 10), Some(720)); // CELT 2.5 ms × 6
    assert_eq!(opus_packet_samples([0xF8 | 3, 7], 10), None); // 140 ms: invalid
    assert_eq!(opus_packet_samples([0xF8, 0], 0), None);
}
