//! Synthetic transport and program streams: a small muxer writes known access units, the
//! demuxer must give them back exactly (bytes, timestamps, flags); truncation and mutation never
//! panic.

use super::*;

fn ts_bytes(prefix: u8, v: i64) -> [u8; 5] {
    let v = v & ((1 << 33) - 1);
    [(prefix << 4) | (((v >> 30) & 7) as u8) << 1 | 1, (v >> 22) as u8, (((v >> 15) & 0x7F) as u8) << 1 | 1, (v >> 7) as u8, ((v & 0x7F) as u8) << 1 | 1]
}

fn pes(stream_id: u8, pts: Option<i64>, dts: Option<i64>, payload: &[u8], bounded: bool) -> Vec<u8> {
    let mut h = Vec::new();
    match (pts, dts) {
        (Some(p), Some(d)) => {
            h.extend(ts_bytes(3, p));
            h.extend(ts_bytes(1, d));
        }
        (Some(p), None) => h.extend(ts_bytes(2, p)),
        _ => {}
    }
    let flags = match (pts, dts) {
        (Some(_), Some(_)) => 0xC0,
        (Some(_), None) => 0x80,
        _ => 0,
    };
    let len = if bounded { 3 + h.len() + payload.len() } else { 0 };
    let mut v = vec![0, 0, 1, stream_id, (len >> 8) as u8, len as u8, 0x80, flags, h.len() as u8];
    v.extend(h);
    v.extend_from_slice(payload);
    v
}

/// A tiny TS muxer: PAT, PMT and PES packets cut into 188-byte packets (192 with `m2ts`).
struct TsMux {
    out: Vec<u8>,
    cc: std::collections::HashMap<u16, u8>,
    m2ts: bool,
}

impl TsMux {
    fn new(m2ts: bool) -> Self {
        Self { out: Vec::new(), cc: Default::default(), m2ts }
    }
    fn packet(&mut self, pid: u16, pusi: bool, payload: &[u8]) -> usize {
        let cc = self.cc.entry(pid).or_insert(0);
        let mut p = vec![0x47, (pusi as u8) << 6 | (pid >> 8) as u8, pid as u8];
        let n = payload.len().min(184);
        if n < 184 {
            // adaptation field stuffing
            let al = 183 - n;
            p.push(0x30 | *cc);
            p.push(al as u8);
            if al > 0 {
                p.push(0);
                p.extend(std::iter::repeat_n(0xFF, al - 1));
            }
        } else {
            p.push(0x10 | *cc);
        }
        *cc = (*cc + 1) & 15;
        p.extend_from_slice(&payload[..n]);
        assert_eq!(p.len(), 188);
        if self.m2ts {
            self.out.extend_from_slice(&[0, 0, 0, 0]);
        }
        self.out.extend(p);
        n
    }
    fn section(&mut self, pid: u16, body: &[u8]) {
        let mut s = body.to_vec();
        let crc = crate::ts::crc32_mpeg(&s);
        s.extend(crc.to_be_bytes());
        let mut payload = vec![0];
        payload.extend(s);
        self.packet(pid, true, &payload);
    }
    fn psi(&mut self, streams: &[(u8, u16)]) {
        // PAT: program 1 → PMT PID 0x100
        let pat = [0x00, 0xB0, 13, 0, 1, 0xC1, 0, 0, 0, 1, 0xE1, 0x00];
        self.section(0, &pat);
        let mut pmt = vec![0x02, 0xB0, 0, 0, 1, 0xC1, 0, 0, 0xE1, 0x00, 0xF0, 0];
        for &(st, pid) in streams {
            pmt.extend([st, 0xE0 | (pid >> 8) as u8, pid as u8, 0xF0, 0]);
        }
        let len = pmt.len() - 3 + 4;
        pmt[1] = 0xB0 | (len >> 8) as u8;
        pmt[2] = len as u8;
        self.section(0x100, &pmt);
    }
    fn pes(&mut self, pid: u16, data: &[u8]) {
        let mut rest = data;
        let mut first = true;
        while !rest.is_empty() {
            let n = self.packet(pid, first, rest);
            rest = &rest[n..];
            first = false;
        }
    }
}

/// An MPEG-2 coded frame: sequence header (on I), GOP, picture header, picture coding
/// extension and a slice of filler; or a field pair.
fn mpeg2_frame(coding_type: u8, tref: u16, fields: bool, filler: usize) -> Vec<u8> {
    let mut v = Vec::new();
    if coding_type == 1 {
        v.extend([0, 0, 1, 0xB3, 0x14, 0x00, 0xF0, 0x13, 0xFF, 0xFF, 0xE0, 0x18]);
        v.extend([0, 0, 1, 0xB8, 0, 8, 0, 0x40]);
    }
    let pics: &[u8] = if fields { &[1, 2] } else { &[3] };
    for &s in pics {
        v.extend([0, 0, 1, 0x00, (tref >> 2) as u8, ((tref & 3) << 6) as u8 | coding_type << 3, 0xFF, 0xF8]);
        v.extend([0, 0, 1, 0xB5, 0x8F, 0xFF, 0xF0 | s, 0x80, 0x80]);
        v.extend([0, 0, 1, 0x01]);
        v.extend((0..filler).map(|i| (i % 200) as u8 | 0x10));
    }
    v
}

/// MPEG-1 layer II frame, 48 kHz 192 kb/s (576 bytes).
fn mp2_frame(k: u8) -> Vec<u8> {
    let mut v = vec![0xFF, 0xFD, 0xA4, 0x00];
    v.extend((0..572).map(|i| (i as u8) ^ k));
    v
}

#[test]
fn transport_stream_units_round_trip() {
    for m2ts in [false, true] {
        let mut mux = TsMux::new(m2ts);
        mux.psi(&[(0x02, 0x1011), (0x03, 0x1100)]);
        // decode order I0 P3 B1 B2 with PTS = display time, DTS on anchors
        let frames = [(1u8, 0u16, false), (2, 3, false), (3, 1, true), (3, 2, false)];
        let mut expect_video = Vec::new();
        for (k, &(t, tref, fields)) in frames.iter().enumerate() {
            let f = mpeg2_frame(t, tref, fields, 300 + 97 * k);
            let pts = 90_000 + tref as i64 * 3600;
            let dts = (t != 3).then_some(90_000 - 3600 + k as i64 * 3600);
            mux.pes(0x1011, &pes(0xE0, Some(pts), dts, &f, false));
            expect_video.push((f, pts, t));
            // two audio frames per PES, the second without its own timestamp
            let a: Vec<u8> = [mp2_frame(2 * k as u8), mp2_frame(2 * k as u8 + 1)].concat();
            mux.pes(0x1100, &pes(0xC0, Some(90_000 + 2160 * 2 * k as i64), None, &a, true));
        }
        let data = mux.out;
        let f = open(&data).unwrap();
        assert_eq!(f.format, Format::Ts { packet_size: if m2ts { 192 } else { 188 }, sync_offset: if m2ts { 4 } else { 0 } });
        assert!(f.warnings.is_empty(), "{:?}", f.warnings);
        assert_eq!(f.program, Some(1));
        let v = f.find(Kind::Video, |_| true).unwrap();
        let s = &f.streams[v];
        assert_eq!(s.codec, Codec::Mpeg2Video);
        assert_eq!(s.units.len(), 4);
        for (i, (bytes, pts, t)) in expect_video.iter().enumerate() {
            let u = &s.units[i];
            assert_eq!(f.read_unit(&data, v, i).unwrap(), *bytes, "unit {i}");
            assert_eq!(u.pts, Some(*pts));
            assert_eq!(u.key, *t == 1);
            assert_eq!(u.disposable, *t == 3);
            assert_eq!(u.picture.unwrap().coding_type, *t);
        }
        assert_eq!(s.units[2].picture.unwrap().structure, 1, "field pair in one unit");
        assert!(s.units[0].picture.unwrap().sequence_header && s.units[0].picture.unwrap().gop);
        let a = f.find(Kind::Audio, |c| *c == Codec::MpegAudio).unwrap();
        let s = &f.streams[a];
        assert_eq!(s.units.len(), 8);
        for (i, u) in s.units.iter().enumerate() {
            assert_eq!(f.read_unit(&data, a, i).unwrap(), mp2_frame(i as u8));
            assert_eq!(u.pts, (i % 2 == 0).then_some(90_000 + 2160 * i as i64));
        }
    }
}

#[test]
fn program_stream_units_round_trip() {
    // packs of ≤ 2048 bytes: frames are split across PES packets
    let frames: Vec<Vec<u8>> = (0..5).map(|k| mpeg2_frame(if k == 0 { 1 } else { 2 }, k as u16, k == 3, 2500 + 300 * k)).collect();
    let es: Vec<u8> = frames.concat();
    let mut starts = Vec::new();
    let mut acc = 0;
    for f in &frames {
        starts.push(acc);
        acc += f.len();
    }
    let mut out = Vec::new();
    let mut p = 0;
    let mut a = 0u8;
    while p < es.len() {
        out.extend([0, 0, 1, 0xBA, 0x44, 0, 4, 0, 4, 1, 1, 0x89, 0xC3, 0xF8]);
        let n = (es.len() - p).min(2000);
        // a timestamp when a frame starts in this packet
        let k = starts.iter().position(|&s| s >= p && s < p + n);
        out.extend(pes(0xE0, k.map(|k| 3600 * k as i64), None, &es[p..p + n], true));
        p += n;
        // AC-3 in private stream 1 (sub-stream 0x80, 4-byte header), 448 kb/s frames
        let mut ac3 = vec![0x80, 1, 0, 1, 0x0B, 0x77, 0, 0, 30, 0x40];
        ac3.extend(std::iter::repeat_n(a, 1792 - 6));
        a += 1;
        out.extend(pes(0xBD, Some(1000 + 2880 * a as i64), None, &ac3, true));
    }
    out.extend([0, 0, 1, 0xB9]);
    let f = open(&out).unwrap();
    assert_eq!(f.format, Format::Ps { mpeg1: false });
    let v = f.find(Kind::Video, |_| true).unwrap();
    assert_eq!(f.streams[v].units.len(), 5);
    for (i, fr) in frames.iter().enumerate() {
        assert_eq!(&f.read_unit(&out, v, i).unwrap(), fr);
        assert_eq!(f.streams[v].units[i].pts, Some(3600 * i as i64));
    }
    let a = f.find(Kind::Audio, |c| *c == Codec::Ac3).unwrap();
    assert_eq!(f.streams[a].id, StreamId::Ps { stream_id: 0xBD, sub_id: Some(0x80) });
    assert_eq!(f.streams[a].units.len(), a_count(&out));
    let u0 = f.read_unit(&out, a, 0).unwrap();
    assert_eq!(&u0[..2], &[0x0B, 0x77]);
    assert_eq!(u0.len(), 1792);
}

fn a_count(ps: &[u8]) -> usize {
    ps.windows(4).filter(|w| *w == [0, 0, 1, 0xBD]).count()
}

#[test]
fn sniffing() {
    assert_eq!(sniff(&[0, 0, 1, 0xBA, 0x44]), Some(Format::Ps { mpeg1: false }));
    assert_eq!(sniff(&[0, 0, 1, 0xBA, 0x21]), Some(Format::Ps { mpeg1: true }));
    assert_eq!(sniff(&[0, 0, 1, 0xB3, 0x44]), None);
    let mut ts = vec![0u8; 188 * 6];
    for k in 0..6 {
        ts[k * 188] = 0x47;
    }
    assert!(matches!(sniff(&ts), Some(Format::Ts { packet_size: 188, .. })));
}

#[test]
fn truncation_and_mutation_never_panic() {
    let mut mux = TsMux::new(false);
    mux.psi(&[(0x02, 0x1011), (0x0F, 0x1100), (0x1B, 0x1200)]);
    for k in 0..6u16 {
        mux.pes(0x1011, &pes(0xE0, Some(3600 * k as i64), None, &mpeg2_frame(if k == 0 { 1 } else { 3 }, k, k % 2 == 1, 400), false));
        let mut adts = vec![0xFF, 0xF1, 0x50, 0x80, 0x0C, 0x9F, 0xFC];
        adts.extend([0x21u8; 93]);
        mux.pes(0x1100, &pes(0xC0, Some(1920 * k as i64), None, &adts, true));
        mux.pes(0x1200, &pes(0xE1, Some(3600 * k as i64), None, &[0, 0, 0, 1, 0x09, 0xF0, 0, 0, 1, 0x65, 0x88, 1, 2, 3], false));
    }
    let data = mux.out;
    let full = open(&data).unwrap();
    assert_eq!(full.streams.len(), 3);
    assert_eq!(full.streams[1].units.len(), 6, "ADTS frames");
    assert!(full.streams[2].units.iter().all(|u| u.key), "IDR access units");
    for cut in (0..data.len()).step_by(37) {
        if let Ok(f) = open(&data[..cut].to_vec()) {
            for (s, st) in f.streams.iter().enumerate() {
                for i in 0..st.units.len() {
                    let _ = f.read_unit(&data[..cut].to_vec(), s, i);
                }
            }
        }
    }
    let mut x = 0x9E37_79B9u32;
    for _ in 0..400 {
        let mut m = data.clone();
        for _ in 0..8 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let i = x as usize % m.len();
            m[i] = (x >> 8) as u8;
        }
        if let Ok(f) = open(&m) {
            for (s, st) in f.streams.iter().enumerate() {
                for i in 0..st.units.len() {
                    let _ = f.read_unit(&m, s, i);
                }
            }
        }
    }
}

/// Opens `data` and reads every unit; errors are fine, panics are not.
fn open_and_read_all(data: &[u8]) {
    let data = data.to_vec();
    if let Ok(f) = open(&data) {
        for (s, st) in f.streams.iter().enumerate() {
            for i in 0..st.units.len() {
                let _ = f.read_unit(&data, s, i);
            }
        }
    }
}

#[test]
fn program_stream_cut_inside_a_pack_header_never_panics() {
    // MPEG-1 pack header cut after the 4-byte start code + 3 bytes (once panicked in `scan`).
    open_and_read_all(&[0, 0, 1, 0xBA, 0x21, 0, 0]);
    for n in 4..=12 {
        let mpeg1 = [0u8, 0, 1, 0xBA, 0x21, 0, 1, 0, 1, 0x80, 0, 1];
        open_and_read_all(&mpeg1[..n]);
    }
}

#[test]
fn program_stream_truncation_at_every_length_never_panics() {
    let frames: Vec<Vec<u8>> = (0..3).map(|k| mpeg2_frame(if k == 0 { 1 } else { 2 }, k as u16, false, 300)).collect();
    let es: Vec<u8> = frames.concat();
    for mpeg1 in [false, true] {
        let mut out = Vec::new();
        for (k, chunk) in es.chunks(500).enumerate() {
            if mpeg1 {
                out.extend([0, 0, 1, 0xBA, 0x21, 0, 1, 0, 1, 0x80, 0, 1]);
            } else {
                out.extend([0, 0, 1, 0xBA, 0x44, 0, 4, 0, 4, 1, 1, 0x89, 0xC3, 0xF8]);
            }
            out.extend(pes(0xE0, Some(3600 * k as i64), None, chunk, true));
        }
        out.extend([0, 0, 1, 0xB9]);
        assert!(open(&out).is_ok(), "the untruncated stream opens (mpeg1: {mpeg1})");
        for cut in 0..=out.len() {
            open_and_read_all(&out[..cut]);
        }
    }
}

#[test]
fn timestamps_unwrap_across_the_33_bit_wrap() {
    let mut u = Unwrap::default();
    let near = (1i64 << 33) - 3000;
    assert_eq!(u.unwrap(near), near);
    assert_eq!(u.unwrap(600), (1 << 33) + 600);
    // a B picture timestamp from just before the wrap
    assert_eq!(u.unwrap(near + 1000), near + 1000);
    assert_eq!(u.unwrap(4200), (1 << 33) + 4200);
}

fn adts_frame(k: u8) -> Vec<u8> {
    let mut v = vec![0xFF, 0xF1, 0x50, 0x80, 0x0C, 0x9F, 0xFC];
    v.extend([k; 93]);
    v
}

#[test]
fn non_pes_payload_is_dropped_until_the_next_unit_start() {
    // a unit start whose payload is not a PES header, then continuation packets that happen to
    // hold ADTS frames: nothing of it may become a unit
    let mut mux = TsMux::new(false);
    mux.psi(&[(0x0F, 0x1100)]);
    mux.packet(0x1100, true, &[0x55; 184]);
    for k in 0..10 {
        mux.packet(0x1100, false, &adts_frame(k));
    }
    let f = open(&mux.out).unwrap();
    assert_eq!(f.streams.first().map(|s| s.units.len()), Some(0));
    // a real PES packet afterwards is indexed again
    let mut mux2 = TsMux::new(false);
    mux2.psi(&[(0x0F, 0x1100)]);
    mux2.packet(0x1100, true, &[0x55; 184]);
    mux2.packet(0x1100, false, &adts_frame(1));
    mux2.pes(0x1100, &pes(0xC0, Some(9000), None, &adts_frame(2), true));
    let f = open(&mux2.out).unwrap();
    let u = &f.streams[0].units;
    assert_eq!(u.len(), 1);
    assert_eq!(u[0].pts, Some(9000));
}

#[test]
fn adaptation_field_only_unit_start_keeps_the_pes_in_progress() {
    let mut mux = TsMux::new(false);
    mux.psi(&[(0x03, 0x1100)]);
    let data = pes(0xC0, Some(9000), None, &[mp2_frame(0), mp2_frame(1)].concat(), true);
    mux.packet(0x1100, true, &data[..184]);
    // PUSI set, adaptation field only (no payload)
    let cc = mux.cc.get(&0x1100).copied().unwrap_or(0);
    let mut af = vec![0x47, 0x40 | 0x11, 0x00, 0x20 | cc, 183, 0x00];
    af.resize(188, 0xFF);
    mux.out.extend(af);
    for chunk in data[184..].chunks(184) {
        mux.packet(0x1100, false, chunk);
    }
    let f = open(&mux.out).unwrap();
    let s = &f.streams[0];
    assert_eq!(s.units.len(), 2);
    assert_eq!(s.units[0].pts, Some(9000));
    assert_eq!(f.read_unit(&mux.out, 0, 1).unwrap(), mp2_frame(1));
}

#[test]
fn oversized_unit_is_refused_without_reserving_it() {
    let f = File {
        format: Format::Ts { packet_size: 188, sync_offset: 0 },
        streams: vec![Stream {
            id: StreamId::Pid(0x100),
            stream_type: 0x1B,
            codec: Codec::H264,
            language: None,
            units: vec![Unit { pts: None, dts: None, key: true, disposable: false, size: u32::MAX, pos: 0, offset: 0, picture: None }],
            lpcm: None,
            pes_packets: 0,
            skipped_bytes: 0,
        }],
        program: None,
        pcr_range: None,
        warnings: Vec::new(),
    };
    assert!(matches!(f.read_unit(&Vec::<u8>::new(), 0, 0), Err(Error::Invalid(_))));
}

/// A `ByteSource` that counts the bytes requested through `read_at`.
struct Counting<'a> {
    data: &'a [u8],
    read: std::cell::Cell<u64>,
}

impl ByteSource for Counting<'_> {
    fn len(&self) -> u64 {
        self.data.len() as u64
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        self.read.set(self.read.get() + buf.len() as u64);
        self.data.read_at(offset, buf)
    }
}

#[test]
fn bad_adaptation_fields_do_not_amplify_reads() {
    // aligned packets whose adaptation_field_length (255) overruns the packet: the packet grid is
    // intact, so this must cost one pass over the file, not one block read per packet
    let mut data = Vec::new();
    for _ in 0..3000 {
        data.extend([0x47, 0x11, 0x00, 0x30]);
        data.extend(std::iter::repeat_n(0xFF, 184));
    }
    let src = Counting { data: &data, read: Default::default() };
    let f = open(&src).unwrap();
    assert!(f.warnings.iter().any(|w| w.contains("adaptation field")), "{:?}", f.warnings);
    assert!(!f.warnings.iter().any(|w| w.contains("lost packet sync")), "{:?}", f.warnings);
    let read = src.read.get();
    assert!(read <= 4 * data.len() as u64, "read {read} bytes of a {}-byte file", data.len());
}

#[test]
fn garbage_without_sync_does_not_amplify_reads() {
    // a sync byte every 189 bytes: five-in-a-row never lines up, every 0x47 is a false candidate
    let mut data = Vec::new();
    for _ in 0..3000 {
        data.extend([0x47]);
        data.extend(std::iter::repeat_n(0x11, 188));
    }
    let head: Vec<u8> = (0..188 * 8).map(|i| if i % 188 == 0 { 0x47 } else { 0 }).collect();
    let mut all = head;
    all.extend(data);
    let src = Counting { data: &all, read: Default::default() };
    let f = open(&src).unwrap();
    assert!(f.warnings.iter().any(|w| w.contains("lost packet sync")), "{:?}", f.warnings);
    let read = src.read.get();
    assert!(read <= 4 * all.len() as u64, "read {read} bytes of a {}-byte file", all.len());
}

#[test]
fn transport_stream_resyncs_after_garbage() {
    const BLOCK_PACKETS: usize = 4096;
    for m2ts in [false, true] {
        let ps = if m2ts { 192 } else { 188 };
        // garbage in front of the stream (shorter than 8 packets) and, at a few places around the
        // first block boundary, between two frames; lengths that are not a multiple of the packet
        for (delta, glen) in [(0usize, 100usize), (1, 37), (2, 188), (3, 301), (5, 5), (8, 1000)] {
            let edge = (BLOCK_PACKETS - delta) * ps;
            let mut mux = TsMux::new(m2ts);
            mux.out.extend([0x11, 0x47, 0x22]); // garbage prefix with a lone 0x47
            mux.psi(&[(0x02, 0x1011)]);
            let mut expect = Vec::new();
            let mut placed = false;
            let mut k = 0usize;
            while k < 3000 || !placed {
                if !placed && mux.out.len() >= edge {
                    // garbage between two PES packets, with lone sync bytes inside
                    let mut g: Vec<u8> = (0..glen).map(|i| 0x10 + (i % 7) as u8).collect();
                    if let Some(b) = g.get_mut(3) {
                        *b = 0x47;
                    }
                    mux.out.extend(g);
                    placed = true;
                }
                let f = mpeg2_frame(if k == 0 { 1 } else { 2 }, (k % 1000) as u16, false, 40 + k % 300);
                mux.pes(0x1011, &pes(0xE0, Some(90_000 + 3600 * k as i64), None, &f, false));
                expect.push(f);
                k += 1;
            }
            assert!(mux.out.len() > (BLOCK_PACKETS + 4) * ps, "stream must cross a block boundary");
            let data = mux.out;
            let f = open(&data).unwrap();
            assert!(f.warnings.iter().any(|w| w.contains("lost packet sync")), "{:?}", f.warnings);
            let v = f.find(Kind::Video, |_| true).unwrap();
            let s = &f.streams[v];
            assert_eq!(s.units.len(), expect.len(), "m2ts {m2ts} delta {delta} glen {glen}");
            for (i, fr) in expect.iter().enumerate() {
                assert_eq!(&f.read_unit(&data, v, i).unwrap(), fr, "unit {i} (m2ts {m2ts} delta {delta} glen {glen})");
                assert_eq!(s.units[i].pts, Some(90_000 + 3600 * i as i64));
            }
        }
    }
}
