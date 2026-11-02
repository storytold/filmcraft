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
