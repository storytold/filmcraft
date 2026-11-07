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
    opus_file_with(npk, pre_skip, |done, last| done as i64 * 960 - if last { trim } else { 0 })
}

/// Like `opus_file`, with the granule of each data page chosen by `granule(packets_done, is_last_page)`.
fn opus_file_with(npk: usize, pre_skip: u16, granule: impl Fn(usize, bool) -> i64) -> Vec<u8> {
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
        let g = granule(done, last);
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

/// Bit-at-a-time CRC of RFC 3533 §6 (polynomial 0x04C11DB7, initial value 0, no reflection, no final XOR).
fn crc32_bitwise(mut crc: u32, data: &[u8]) -> u32 {
    for &b in data {
        crc ^= u32::from(b) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04C1_1DB7 } else { crc << 1 };
        }
    }
    crc
}

#[test]
fn crc_matches_bitwise_reference_on_every_length_and_alignment() {
    let mut seed = 7u64;
    let buf: Vec<u8> = (0..70_100)
        .map(|_| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as u8
        })
        .collect();
    // every short length (covers the 16-byte main loop and every tail length), at several start offsets
    for start in 0..4 {
        for len in 0..=64 {
            let d = &buf[start..start + len];
            assert_eq!(crc32(d), crc32_bitwise(0, d), "start {start} len {len}");
        }
    }
    for len in [65, 255, 4096, 65_307, 70_000] {
        for start in [0, 1, 3] {
            let d = &buf[start..start + len];
            assert_eq!(crc32(d), crc32_bitwise(0, d), "start {start} len {len}");
        }
    }
    // all-zero and all-ones input
    assert_eq!(crc32(&[0u8; 100]), 0);
    assert_eq!(crc32(&[0xFFu8; 100]), crc32_bitwise(0, &[0xFFu8; 100]));
}

#[test]
fn crc_update_continues_across_any_split() {
    let data: Vec<u8> = (0..200u32).map(|i| (i * 31 + 5) as u8).collect();
    let whole = crc32(&data);
    for cut in 0..=data.len() {
        let (a, b) = data.split_at(cut);
        assert_eq!(crc32_update(crc32_update(0, a), b), whole, "cut {cut}");
        assert_eq!(crc32_update(crc32_bitwise(0, a), b), whole, "cut {cut} (reference head)");
    }
}

#[test]
fn open_reads_a_generated_file_page_by_page() {
    // pages of several body sizes, each checked by the new CRC: every packet must come back byte for byte
    let f = opus_file(60, 312, 0);
    let o = open(&f).unwrap();
    assert!(o.warnings.is_empty(), "{:?}", o.warnings);
    assert_eq!(o.streams.len(), 1);
    let st = &o.streams[0];
    assert_eq!(st.pages, 2 + 20);
    assert_eq!(st.packets.len(), 60);
    for i in 0..60 {
        assert_eq!(o.read_packet(&f, 0, i).unwrap(), pkt(i, 40 + i % 5), "packet {i}");
    }
    // every page of the file carries the bitwise-reference CRC
    let mut pos = 0;
    while pos < f.len() {
        let nseg = f[pos + 26] as usize;
        let body: usize = f[pos + 27..pos + 27 + nseg].iter().map(|&b| usize::from(b)).sum();
        let end = pos + 27 + nseg + body;
        let mut page = f[pos..end].to_vec();
        let stored = u32::from_le_bytes([page[22], page[23], page[24], page[25]]);
        page[22..26].fill(0);
        assert_eq!(stored, crc32_bitwise(0, &page), "page at {pos}");
        pos = end;
    }
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

/// The timing must be defined, bounded and panic-free for any granule a file can carry.
fn assert_sane_timing(t: &OpusTiming, npk: usize) {
    assert_eq!(t.starts.len(), npk);
    // treated as unknown granules: the packets simply follow one another from the start
    for (k, s) in t.starts.iter().enumerate() {
        assert_eq!(*s, k as i64 * 960 - 312, "packet {k}");
    }
    // as for a truncated file with no usable granule, the total stays 0: it never exceeds what the packets add up to
    assert!((0..=npk as i64 * 960).contains(&t.total), "total {}", t.total);
}

#[test]
fn opus_timing_ignores_a_granule_near_i64_max() {
    // panicked with "attempt to add with overflow" in debug builds, wrapped in release
    let f = opus_file_with(10, 312, |done, _| i64::MAX - 10 + done as i64 / 3);
    let o = open(&f).unwrap();
    assert_sane_timing(&OpusTiming::of(&o.streams[0], 312), 10);
}

#[test]
fn opus_timing_ignores_a_granule_of_i64_min() {
    // panicked with "attempt to subtract with overflow" in debug builds
    for g in [i64::MIN, i64::MIN + 1, -2] {
        let f = opus_file_with(10, 312, |_, _| g);
        let o = open(&f).unwrap();
        assert_sane_timing(&OpusTiming::of(&o.streams[0], 312), 10);
    }
}

#[test]
fn opus_timing_with_a_bad_granule_on_one_page_recovers_on_the_next() {
    // pages of 3 packets: the second page's granule is absurd, the others are right
    let f = opus_file_with(9, 0, |done, _| if done == 6 { i64::MAX } else { done as i64 * 960 });
    let o = open(&f).unwrap();
    let t = OpusTiming::of(&o.streams[0], 0);
    let expect: Vec<i64> = (0..9).map(|k| k * 960).collect();
    assert_eq!(t.starts, expect);
    assert_eq!(t.total, 9 * 960);
}

#[test]
fn largest_trusted_granule_with_a_huge_pre_skip_stays_in_range() {
    let f = opus_file_with(6, 0, |done, _| MAX_OPUS_GRANULE - 6 * 960 + done as i64 * 960);
    let o = open(&f).unwrap();
    let t = OpusTiming::of(&o.streams[0], u32::MAX);
    // shift = base (the first raw start, 2^62 - 5760) + u32::MAX; every start is relative to it
    let expect: Vec<i64> = (0..6).map(|k| k * 960 - u32::MAX as i64).collect();
    assert_eq!(t.starts, expect);
    // the end (2^62) lies before the shift, so nothing is playable
    assert_eq!(t.total, 0);
}

/// Opus headers, then `pages` pages of 255 zero-length packets each (a 255-byte lacing table of zeros).
fn empty_packet_file(pages: usize) -> Vec<u8> {
    let mut f = Vec::new();
    let head = opus_head(0);
    let (l, b) = lace(&[&head], false);
    page(&mut f, 0x02, 0, 9, 0, &l, &b);
    let tags = b"OpusTags\x00\x00\x00\x00\x00\x00\x00\x00".to_vec();
    let (l, b) = lace(&[&tags], false);
    page(&mut f, 0, 0, 9, 1, &l, &b);
    for seq in 0..pages {
        page(&mut f, 0, 0, 9, seq as u32 + 2, &[0u8; 255], &[]);
    }
    f
}

#[test]
fn zero_length_packets_stop_at_the_packet_budget_with_a_warning() {
    let each = std::mem::size_of::<Packet>();
    // 40 pages = 10_200 empty packets, in a file of about 11 KB; the budget allows 600
    let f = empty_packet_file(40);
    assert!(f.len() < 12_000);
    let o = open_with_packet_budget(&f, each * 600).unwrap();
    assert_eq!(o.streams[0].packets.len(), 600);
    assert!(o.streams[0].packets.iter().all(|p| p.size == 0 && p.parts.is_empty()));
    assert!(o.warnings.iter().any(|w| w.contains("packet list exceeds")), "{:?}", o.warnings);
}

#[test]
fn a_packet_list_exactly_at_the_budget_is_kept_without_a_warning() {
    let each = std::mem::size_of::<Packet>();
    let f = empty_packet_file(2); // 510 packets
    let o = open_with_packet_budget(&f, each * 510).unwrap();
    assert_eq!(o.streams[0].packets.len(), 510);
    assert!(o.warnings.is_empty(), "{:?}", o.warnings);
    // one packet less of budget drops exactly the last packet
    let o = open_with_packet_budget(&f, each * 510 - 1).unwrap();
    assert_eq!(o.streams[0].packets.len(), 509);
    assert!(o.warnings.iter().any(|w| w.contains("packet list exceeds")));
}

#[test]
fn packets_with_data_are_charged_for_their_byte_ranges() {
    let f = opus_file(9, 0, 0);
    let all = open(&f).unwrap();
    let cost: usize = all.streams[0].packets.iter().map(packet_cost).sum();
    assert!(cost > 9 * std::mem::size_of::<Packet>());
    assert_eq!(open_with_packet_budget(&f, cost).unwrap().streams[0].packets.len(), 9);
    assert!(open_with_packet_budget(&f, cost - 1).unwrap().streams[0].packets.len() < 9);
}

#[test]
fn a_normal_file_is_unaffected_by_the_default_budget() {
    let f = opus_file(30, 312, 100);
    let o = open(&f).unwrap();
    assert_eq!(o.streams[0].packets.len(), 30);
    assert!(o.warnings.is_empty(), "{:?}", o.warnings);
}

#[test]
fn a_budget_stop_does_not_add_a_file_ends_inside_a_packet_warning() {
    let mut f = Vec::new();
    let head = opus_head(0);
    let (l, b) = lace(&[&head], false);
    page(&mut f, 0x02, 0, 9, 0, &l, &b);
    let tags = b"OpusTags\x00\x00\x00\x00\x00\x00\x00\x00".to_vec();
    let (l, b) = lace(&[&tags], false);
    page(&mut f, 0, 0, 9, 1, &l, &b);
    // two complete packets, then one that continues onto a next page that never comes
    let (p0, p1, p2) = (pkt(0, 40), pkt(1, 40), pkt(2, 255));
    let (l, b) = lace(&[&p0, &p1, &p2], true);
    page(&mut f, 0, -1, 9, 2, &l, &b);
    let full = open(&f).unwrap();
    assert_eq!(full.streams[0].packets.len(), 2);
    assert!(full.warnings.iter().any(|w| w == "file ends inside a packet"));
    let one = packet_cost(&full.streams[0].packets[0]);
    let o = open_with_packet_budget(&f, one).unwrap();
    assert_eq!(o.streams[0].packets.len(), 1);
    assert_eq!(o.warnings.len(), 1, "{:?}", o.warnings);
    assert!(o.warnings[0].contains("packet list exceeds"));
}

#[test]
fn a_packet_spanning_pages_is_charged_once_at_its_final_size() {
    let mut f = Vec::new();
    let head = opus_head(0);
    let (l, b) = lace(&[&head], false);
    page(&mut f, 0x02, 0, 9, 0, &l, &b);
    let tags = b"OpusTags\x00\x00\x00\x00\x00\x00\x00\x00".to_vec();
    let (l, b) = lace(&[&tags], false);
    page(&mut f, 0, 0, 9, 1, &l, &b);
    // one 600-byte packet over three pages: parts of 255, 255, 90 bytes
    let big = pkt(0, 600);
    let (l, b) = lace(&[&big[..255]], true);
    page(&mut f, 0, -1, 9, 2, &l, &b);
    let (l, b) = lace(&[&big[255..510]], true);
    page(&mut f, 1, -1, 9, 3, &l, &b);
    let (l, b) = lace(&[&big[510..]], false);
    page(&mut f, 1, 960, 9, 4, &l, &b);
    let o = open(&f).unwrap();
    let pk = &o.streams[0].packets;
    assert_eq!(pk.len(), 1);
    assert_eq!(pk[0].parts.len(), 3);
    let cost = std::mem::size_of::<Packet>() + 3 * std::mem::size_of::<(u64, u32)>();
    assert_eq!(packet_cost(&pk[0]), cost);
    assert_eq!(open_with_packet_budget(&f, cost).unwrap().streams[0].packets.len(), 1);
    assert!(open_with_packet_budget(&f, cost - 1).unwrap().streams[0].packets.is_empty());
}

#[test]
fn the_packet_budget_is_shared_by_all_streams() {
    let mut f = Vec::new();
    for serial in [9u32, 10] {
        let head = opus_head(0);
        let (l, b) = lace(&[&head], false);
        page(&mut f, 0x02, 0, serial, 0, &l, &b);
        let tags = b"OpusTags\x00\x00\x00\x00\x00\x00\x00\x00".to_vec();
        let (l, b) = lace(&[&tags], false);
        page(&mut f, 0, 0, serial, 1, &l, &b);
    }
    for seq in 0..2u32 {
        for serial in [9u32, 10] {
            page(&mut f, 0, 0, serial, seq + 2, &[0u8; 255], &[]);
        }
    }
    let each = std::mem::size_of::<Packet>();
    // 4 pages of 255 packets over two streams; the budget holds 700 in total
    let o = open_with_packet_budget(&f, each * 700).unwrap();
    let total: usize = o.streams.iter().map(|s| s.packets.len()).sum();
    assert_eq!(total, 700);
    assert!(o.warnings.iter().any(|w| w.contains("packet list exceeds")));
}
