//! Synthetic files: a small OP1a writer (enough of ST 377-1 to exercise the demuxer without
//! ffmpeg) and robustness checks.

use crate::*;

fn ul(b: &[u8]) -> [u8; 16] {
    let mut k = [0u8; 16];
    k[..b.len()].copy_from_slice(b);
    k
}

fn ber(n: usize) -> Vec<u8> {
    let mut v = vec![0x83];
    v.extend_from_slice(&(n as u32).to_be_bytes()[1..]);
    v
}

fn klv(out: &mut Vec<u8>, key: [u8; 16], v: &[u8]) {
    out.extend_from_slice(&key);
    out.extend_from_slice(&ber(v.len()));
    out.extend_from_slice(v);
}

struct LocalSet(Vec<u8>);
impl LocalSet {
    fn new(uid: u8) -> Self {
        let mut s = LocalSet(Vec::new());
        s.p(0x3C0A, &uuid(uid));
        s
    }
    fn p(&mut self, tag: u16, v: &[u8]) -> &mut Self {
        self.0.extend_from_slice(&tag.to_be_bytes());
        self.0.extend_from_slice(&(v.len() as u16).to_be_bytes());
        self.0.extend_from_slice(v);
        self
    }
}

fn uuid(n: u8) -> [u8; 16] {
    let mut u = [0xAAu8; 16];
    u[15] = n;
    u
}

fn umid(n: u8) -> [u8; 32] {
    let mut u = [0x55u8; 32];
    u[31] = n;
    u
}

fn batch(items: &[&[u8]]) -> Vec<u8> {
    let mut v = (items.len() as u32).to_be_bytes().to_vec();
    v.extend_from_slice(&(items.first().map_or(0, |i| i.len()) as u32).to_be_bytes());
    for i in items {
        v.extend_from_slice(i);
    }
    v
}

fn set_key(t: u8) -> [u8; 16] {
    ul(&[0x06, 0x0E, 0x2B, 0x34, 0x02, 0x53, 0x01, 0x01, 0x0D, 0x01, 0x01, 0x01, 0x01, 0x01, t, 0x00])
}

const PICTURE_DEF: [u8; 16] = [0x06, 0x0E, 0x2B, 0x34, 0x04, 0x01, 0x01, 0x01, 0x01, 0x03, 0x02, 0x02, 0x01, 0, 0, 0];
const SOUND_DEF: [u8; 16] = [0x06, 0x0E, 0x2B, 0x34, 0x04, 0x01, 0x01, 0x01, 0x01, 0x03, 0x02, 0x02, 0x02, 0, 0, 0];
const TC_DEF: [u8; 16] = [0x06, 0x0E, 0x2B, 0x34, 0x04, 0x01, 0x01, 0x01, 0x01, 0x03, 0x02, 0x01, 0x01, 0, 0, 0];
const OP1A: [u8; 16] = [0x06, 0x0E, 0x2B, 0x34, 0x04, 0x01, 0x01, 0x01, 0x0D, 0x01, 0x02, 0x01, 0x01, 0x01, 0x09, 0x00];
const PIC_KEY: [u8; 16] = [0x06, 0x0E, 0x2B, 0x34, 0x01, 0x02, 0x01, 0x01, 0x0D, 0x01, 0x03, 0x01, 0x15, 0x01, 0x05, 0x00];
const SND_KEY: [u8; 16] = [0x06, 0x0E, 0x2B, 0x34, 0x01, 0x02, 0x01, 0x01, 0x0D, 0x01, 0x03, 0x01, 0x16, 0x01, 0x03, 0x00];

fn partition(kind: u8, this: u64, body_sid: u32, index_sid: u32, hbc: u64) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&1u16.to_be_bytes());
    v.extend_from_slice(&3u16.to_be_bytes());
    v.extend_from_slice(&1u32.to_be_bytes());
    v.extend_from_slice(&this.to_be_bytes());
    v.extend_from_slice(&0u64.to_be_bytes());
    v.extend_from_slice(&0u64.to_be_bytes());
    v.extend_from_slice(&hbc.to_be_bytes());
    v.extend_from_slice(&0u64.to_be_bytes());
    v.extend_from_slice(&index_sid.to_be_bytes());
    v.extend_from_slice(&0u64.to_be_bytes());
    v.extend_from_slice(&body_sid.to_be_bytes());
    v.extend_from_slice(&OP1A);
    v.extend_from_slice(&batch(&[]));
    let mut out = Vec::new();
    klv(&mut out, ul(&[0x06, 0x0E, 0x2B, 0x34, 0x02, 0x05, 0x01, 0x01, 0x0D, 0x01, 0x02, 0x01, 0x01, kind, 0x04, 0x00]), &v);
    out
}

fn track(uid: u8, id: u32, number: u32, seq: u8) -> Vec<u8> {
    let mut s = LocalSet::new(uid);
    s.p(0x4801, &id.to_be_bytes()).p(0x4804, &number.to_be_bytes()).p(0x4B01, &[0, 0, 0, 25, 0, 0, 0, 1]).p(0x4B02, &0i64.to_be_bytes()).p(0x4803, &uuid(seq));
    s.0
}

fn sequence(uid: u8, def: [u8; 16], dur: i64, comp: u8) -> Vec<u8> {
    let mut s = LocalSet::new(uid);
    s.p(0x0201, &def).p(0x0202, &dur.to_be_bytes()).p(0x1001, &batch(&[&uuid(comp)]));
    s.0
}

fn source_clip(uid: u8, def: [u8; 16], dur: i64, pkg: [u8; 32], track: u32) -> Vec<u8> {
    let mut s = LocalSet::new(uid);
    s.p(0x0201, &def).p(0x0202, &dur.to_be_bytes()).p(0x1201, &0i64.to_be_bytes()).p(0x1101, &pkg).p(0x1102, &track.to_be_bytes());
    s.0
}

/// An OP1a file: `n` frame-wrapped content packages, each a picture element of `pic(i)` and a
/// 16-bit stereo sound element of 1920 frames (sample k of the file = k, -k on the channels);
/// a footer index with temporal offsets `to` and flags `flags`.
fn build(n: usize, pic: impl Fn(usize) -> Vec<u8>, to: &[i8], flags: &[u8]) -> Vec<u8> {
    let mut md = Vec::new();
    // primer: no dynamic tags
    klv(&mut md, ul(&[0x06, 0x0E, 0x2B, 0x34, 0x02, 0x05, 0x01, 0x01, 0x0D, 0x01, 0x02, 0x01, 0x01, 0x05, 0x01, 0x00]), &batch(&[]));
    let mut pre = LocalSet::new(1);
    pre.p(0x3B03, &uuid(2)).p(0x3B09, &OP1A);
    klv(&mut md, set_key(0x2F), &pre.0);
    let mut cs = LocalSet::new(2);
    cs.p(0x1901, &batch(&[&uuid(3), &uuid(4)]));
    klv(&mut md, set_key(0x18), &cs.0);
    // material package: timecode, picture, sound
    let mut mp = LocalSet::new(3);
    mp.p(0x4401, &umid(1)).p(0x4402, &[0, b'M', 0, b'P']).p(0x4403, &batch(&[&uuid(10), &uuid(11), &uuid(12)]));
    klv(&mut md, set_key(0x36), &mp.0);
    let d = n as i64;
    klv(&mut md, set_key(0x3B), &track(10, 1, 0, 20));
    klv(&mut md, set_key(0x0F), &sequence(20, TC_DEF, d, 30));
    let mut tc = LocalSet::new(30);
    tc.p(0x0201, &TC_DEF).p(0x0202, &d.to_be_bytes()).p(0x1501, &90_000i64.to_be_bytes()).p(0x1502, &25u16.to_be_bytes()).p(0x1503, &[0]);
    klv(&mut md, set_key(0x14), &tc.0);
    klv(&mut md, set_key(0x3B), &track(11, 2, 0, 21));
    klv(&mut md, set_key(0x0F), &sequence(21, PICTURE_DEF, d, 31));
    klv(&mut md, set_key(0x11), &source_clip(31, PICTURE_DEF, d, umid(2), 2));
    klv(&mut md, set_key(0x3B), &track(12, 3, 0, 22));
    klv(&mut md, set_key(0x0F), &sequence(22, SOUND_DEF, d, 32));
    klv(&mut md, set_key(0x11), &source_clip(32, SOUND_DEF, d, umid(2), 3));
    // file package
    let mut fp = LocalSet::new(4);
    fp.p(0x4401, &umid(2)).p(0x4403, &batch(&[&uuid(13), &uuid(14)])).p(0x4701, &uuid(40));
    klv(&mut md, set_key(0x37), &fp.0);
    klv(&mut md, set_key(0x3B), &track(13, 2, 0x1501_0500, 23));
    klv(&mut md, set_key(0x0F), &sequence(23, PICTURE_DEF, d, 33));
    klv(&mut md, set_key(0x11), &source_clip(33, PICTURE_DEF, d, [0; 32], 0));
    klv(&mut md, set_key(0x3B), &track(14, 3, 0x1601_0300, 24));
    klv(&mut md, set_key(0x0F), &sequence(24, SOUND_DEF, d, 34));
    klv(&mut md, set_key(0x11), &source_clip(34, SOUND_DEF, d, [0; 32], 0));
    let mut mdesc = LocalSet::new(40);
    mdesc.p(0x3001, &[0, 0, 0, 25, 0, 0, 0, 1]).p(0x3F01, &batch(&[&uuid(41), &uuid(42)]));
    klv(&mut md, set_key(0x44), &mdesc.0);
    let mut cdci = LocalSet::new(41);
    cdci.p(0x3006, &2u32.to_be_bytes())
        .p(0x3203, &64u32.to_be_bytes())
        .p(0x3202, &32u32.to_be_bytes())
        .p(0x320E, &[0, 0, 0, 2, 0, 0, 0, 1])
        .p(0x3301, &8u32.to_be_bytes())
        .p(0x3201, &ul(&[0x06, 0x0E, 0x2B, 0x34, 0x04, 0x01, 0x01, 0x0A, 0x04, 0x01, 0x02, 0x02, 0x03, 0x06, 0x03, 0x00]));
    klv(&mut md, set_key(0x28), &cdci.0);
    let mut wave = LocalSet::new(42);
    wave.p(0x3006, &3u32.to_be_bytes())
        .p(0x3D03, &[0, 0, 0xBB, 0x80, 0, 0, 0, 1])
        .p(0x3D07, &2u32.to_be_bytes())
        .p(0x3D01, &16u32.to_be_bytes())
        .p(0x3D0A, &4u16.to_be_bytes());
    klv(&mut md, set_key(0x48), &wave.0);
    let mut ecd = LocalSet::new(5);
    ecd.p(0x2701, &umid(2)).p(0x3F07, &1u32.to_be_bytes()).p(0x3F06, &2u32.to_be_bytes());
    klv(&mut md, set_key(0x23), &ecd.0);

    let mut f = partition(2, 0, 0, 0, md.len() as u64);
    f.extend_from_slice(&md);
    let body_at = f.len() as u64;
    f.extend_from_slice(&partition(3, body_at, 1, 0, 0));
    let mut offsets = Vec::new();
    let mut cp_base = None;
    for i in 0..n {
        let at = f.len() as u64;
        let base = *cp_base.get_or_insert(at);
        offsets.push(at - base);
        klv(&mut f, PIC_KEY, &pic(i));
        let mut snd = Vec::new();
        for k in 0..1920 {
            let s = ((i * 1920 + k) % 30000) as i16;
            snd.extend_from_slice(&s.to_le_bytes());
            snd.extend_from_slice(&(-s).to_le_bytes());
        }
        klv(&mut f, SND_KEY, &snd);
    }
    let footer_at = f.len() as u64;
    f.extend_from_slice(&partition(4, footer_at, 0, 2, 0));
    let mut ix = LocalSet::new(6);
    let mut entries = Vec::new();
    for i in 0..n {
        let mut e = vec![to.get(i).copied().unwrap_or(0) as u8, 0, flags.get(i).copied().unwrap_or(0)];
        e.extend_from_slice(&offsets[i].to_be_bytes());
        entries.push(e);
    }
    let refs: Vec<&[u8]> = entries.iter().map(Vec::as_slice).collect();
    ix.p(0x3F0B, &[0, 0, 0, 25, 0, 0, 0, 1])
        .p(0x3F0C, &0i64.to_be_bytes())
        .p(0x3F0D, &(n as i64).to_be_bytes())
        .p(0x3F06, &2u32.to_be_bytes())
        .p(0x3F07, &1u32.to_be_bytes())
        .p(0x3F0A, &batch(&refs));
    klv(&mut f, ul(&[0x06, 0x0E, 0x2B, 0x34, 0x02, 0x53, 0x01, 0x01, 0x0D, 0x01, 0x02, 0x01, 0x01, 0x10, 0x01, 0x00]), &ix.0);
    f
}

/// Stored order I0 P3 B1 B2 P6 B4 B5: display d is stored at d + TO[d].
fn ipbb() -> (Vec<i8>, Vec<u8>) {
    (vec![0, 1, 1, -2, 1, 1, -2], vec![0xC0, 0x22, 0x33, 0x33, 0x22, 0x33, 0x33])
}

fn prores_like(i: usize) -> Vec<u8> {
    let mut v = vec![0, 0, 0, 40];
    v.extend_from_slice(b"icpf");
    v.extend(std::iter::repeat_n(i as u8, 32));
    v
}

#[test]
fn opens_synthetic_op1a() {
    let (to, flags) = ipbb();
    let f = build(7, |i| vec![i as u8; 100 + i], &to, &flags);
    let m = open(&f).unwrap();
    assert_eq!(m.operational_pattern, OperationalPattern::Generalized { item: 1, package: 1 });
    assert_eq!(m.operational_pattern.name(), "OP1a");
    assert_eq!(m.material_package_name.as_deref(), Some("MP"));
    let tc = m.timecode.unwrap();
    assert_eq!((tc.start, tc.rounded_base, tc.drop_frame), (90_000, 25, false));
    assert_eq!(tc.format(), "01:00:00:00");
    let v = m.track_of_kind(TrackKind::Picture).unwrap();
    let t = &m.tracks[v];
    assert_eq!(t.track_number, 0x1501_0500);
    assert_eq!(t.edit_rate, Rational::new(25, 1));
    assert_eq!(t.duration, Some(7));
    assert_eq!(t.samples.len(), 7);
    let p = t.picture.as_ref().unwrap();
    assert_eq!((p.frame_width(), p.frame_height()), (64, 32));
    assert_eq!(t.codec, Codec::ProRes { profile: Some(3) }, "coding label wins (the bytes do not sniff as anything)");
    // presentation order from the temporal offsets
    let pts: Vec<i64> = t.samples.iter().map(|s| s.pts).collect();
    assert_eq!(pts, vec![0, 3, 1, 2, 6, 4, 5]);
    assert_eq!(t.display_order, vec![0, 2, 3, 1, 5, 6, 4]);
    assert!(t.temporal_offsets);
    assert_eq!(t.sample_at(3), Some(1));
    // the index marks only the first picture random-access (ProRes would be intra; this label says ProRes so all are key)
    assert!(t.samples.iter().all(|s| s.key));
    // sample bytes
    for i in 0..7 {
        assert_eq!(m.read_sample(&f, v, i).unwrap(), vec![i as u8; 100 + i]);
    }
    // sound: 7 × 1920 frames, sample k = k on the left, -k on the right
    let a = m.track_of_kind(TrackKind::Sound).unwrap();
    let s = &m.tracks[a];
    assert_eq!(s.codec, Codec::Pcm);
    assert_eq!(s.stored_sample_frames(), 7 * 1920);
    assert_eq!(s.samples_per_edit_unit(), (48000, 25));
    let pcm = m.read_pcm(&f, a, 1900, 50).unwrap();
    for k in 0..50 {
        assert_eq!(pcm[0][k], (1900 + k) as f32 / 32768.0);
        assert_eq!(pcm[1][k], -((1900 + k) as f32) / 32768.0);
    }
    // past the end: zeros
    let tail = m.read_pcm(&f, a, 7 * 1920 - 2, 4).unwrap();
    assert_eq!(tail[0][2..], [0.0, 0.0]);
}

#[test]
fn key_frames_from_index_flags_for_long_gop() {
    let (to, flags) = ipbb();
    // AVC-looking essence (start code + AUD) and no coding label: sniffed as AVC
    let f = build(7, |i| vec![0, 0, 0, 1, 9, 0x10, i as u8], &to, &flags);
    // replace the ProRes coding label so the codec comes from the bytes
    let mut f2 = f.clone();
    let lbl = [0x04, 0x01, 0x02, 0x02, 0x03, 0x06, 0x03, 0x00];
    let at = f2.windows(8).position(|w| w == lbl).unwrap();
    f2[at..at + 8].copy_from_slice(&[0x04, 0x01, 0x02, 0x02, 0x01, 0x31, 0x40, 0x01]);
    let m = open(&f2).unwrap();
    let t = &m.tracks[m.track_of_kind(TrackKind::Picture).unwrap()];
    assert_eq!(t.codec, Codec::Avc { intra: false });
    let keys: Vec<bool> = t.samples.iter().map(|s| s.key).collect();
    assert_eq!(keys, vec![true, false, false, false, false, false, false]);
    assert_eq!(t.sync_before(5), 0);
    assert!(t.indexed);
    let b: Vec<bool> = t.samples.iter().map(|s| s.b_picture).collect();
    assert_eq!(b, vec![false, false, true, true, false, true, true]);
}

#[test]
fn missing_temporal_offsets_with_b_pictures_ask_for_reordering() {
    let (_, flags) = ipbb();
    let m = open(&build(7, prores_like, &[], &flags)).unwrap();
    let t = &m.tracks[0];
    assert!(!t.temporal_offsets);
    // ProRes is intra-only: the B flags are ignored for key frames, reordering still reported
    assert!(t.samples.iter().all(|s| s.key));
}

#[test]
fn not_mxf_and_empty() {
    assert!(matches!(open(&b"RIFF....WAVE".to_vec()), Err(Error::NotMxf(_))));
    assert!(open(&Vec::new()).is_err());
    assert!(!sniff(b"\x1aE\xdf\xa3"));
}

#[test]
fn truncation_never_panics_and_keeps_whole_frames() {
    let (to, flags) = ipbb();
    let f = build(7, prores_like, &to, &flags);
    let mut opened = 0;
    for cut in (0..f.len()).step_by(37) {
        let part = f[..cut].to_vec();
        if let Ok(m) = open(&part) {
            opened += 1;
            for (ti, t) in m.tracks.iter().enumerate() {
                for i in 0..t.samples.len() {
                    // every listed sample is fully inside the file
                    assert!(m.read_sample(&part, ti, i).is_ok(), "cut {cut} track {ti} sample {i}");
                }
                if t.sound.is_some() {
                    let _ = m.read_pcm(&part, ti, 0, 5000);
                }
            }
        }
    }
    assert!(opened > 10, "truncated files with complete metadata still open ({opened})");
}

#[test]
fn mutation_never_panics() {
    let (to, flags) = ipbb();
    let f = build(5, prores_like, &to, &flags);
    let mut seed = 0x1234_5678u64;
    for _ in 0..600 {
        let mut g = f.clone();
        for _ in 0..4 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let at = (seed as usize) % g.len();
            g[at] = (seed >> 32) as u8;
        }
        if let Ok(m) = open(&g) {
            for (ti, t) in m.tracks.iter().enumerate() {
                for i in 0..t.samples.len().min(8) {
                    let _ = m.read_sample(&g, ti, i);
                }
                let _ = m.read_pcm(&g, ti, 0, 100);
            }
        }
    }
}

#[test]
fn drop_frame_timecode_format() {
    let tc = Timecode { start: 107_892, rounded_base: 30, drop_frame: true };
    assert_eq!(tc.format(), "01:00:00;00");
    let tc = Timecode { start: 1800, rounded_base: 30, drop_frame: true };
    assert_eq!(tc.format(), "00:01:00;02");
    let tc = Timecode { start: 90_000 + 25 * 61 + 3, rounded_base: 25, drop_frame: false };
    assert_eq!(tc.format(), "01:01:01:03");
}

#[test]
fn hostile_pcm_requests_fail_before_allocation() {
    let (to, flags) = ipbb();
    let bytes = build(5, prores_like, &to, &flags);
    let mut file = open(&bytes).unwrap();
    let track = file.track_of_kind(TrackKind::Sound).unwrap();
    file.tracks[track].sound.as_mut().unwrap().channels = u32::MAX;
    assert!(file.read_pcm(&bytes, track, 0, 100).is_err());
    file.tracks[track].sound.as_mut().unwrap().channels = 2;
    file.tracks[track].sound.as_mut().unwrap().block_align = u32::MAX;
    assert!(file.read_pcm(&bytes, track, 0, 100).is_err());
    assert!(file.read_pcm(&bytes, track, 0, usize::MAX).is_err());
    assert!(crate::decode_pcm(&[], SoundFormat::Pcm, usize::MAX, 16, 4).is_err());
}

#[test]
fn a_corrupt_index_position_cannot_allocate_a_giant_sparse_table() {
    let (to, flags) = ipbb();
    let mut bytes = build(5, prores_like, &to, &flags);
    let prefix = [0x3f, 0x0c, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0];
    let at = bytes.windows(prefix.len()).position(|w| w == prefix).unwrap();
    bytes[at + 4..at + 12].copy_from_slice(&49_999_999i64.to_be_bytes());
    let file = open(&bytes).unwrap();
    assert_eq!(file.tracks[file.track_of_kind(TrackKind::Picture).unwrap()].samples.len(), 5);
}

#[test]
fn opaque_header_packet_preserves_following_metadata() {
    let original = build(2, prores_like, &[], &[]);
    let at = original.windows(16).position(|b| b == set_key(0x2f)).unwrap();
    let mut opaque = Vec::new();
    klv(&mut opaque, [0x80; 16], &[0; 96]);
    let mut data = original.clone();
    data.splice(at..at, opaque.iter().copied());
    let (_, width) = crate::klv::ber_length(&data[16..]).unwrap();
    let count_at = 16 + width + 32;
    let count = u64::from_be_bytes(data[count_at..count_at + 8].try_into().unwrap());
    data[count_at..count_at + 8].copy_from_slice(&(count + opaque.len() as u64).to_be_bytes());
    let file = open(data.as_slice()).unwrap();
    assert_eq!(file.tracks.len(), 2);
    assert!(file.warnings.iter().any(|w| w.contains("opaque non-SMPTE")));
    let audio = file.track_of_kind(TrackKind::Sound).unwrap();
    assert_eq!(
        file.read_pcm(data.as_slice(), audio, 0, 100).unwrap(),
        open(original.as_slice()).unwrap().read_pcm(original.as_slice(), audio, 0, 100).unwrap()
    );
    // An unbounded purported private value must not be trusted as a packet boundary.
    data[at + 16..at + 20].copy_from_slice(&[0x83, 0xff, 0xff, 0xff]);
    assert!(std::panic::catch_unwind(|| open(data.as_slice())).is_ok());
}

#[test]
fn legacy_picture_audio_and_timecode_data_definitions_are_recognized() {
    let mut data = build(2, prores_like, &[], &[]);
    for (standard, legacy) in [
        (PICTURE_DEF, [0x80, 0x7d, 0, 0x60, 8, 0x14, 0x3e, 0x6f, 0x6f, 0x3c, 0x8c, 0xe1, 0x6c, 0xef, 0x11, 0xd2]),
        (SOUND_DEF, [0x80, 0x7d, 0, 0x60, 8, 0x14, 0x3e, 0x6f, 0x78, 0xe1, 0xeb, 0xe1, 0x6c, 0xef, 0x11, 0xd2]),
        (TC_DEF, [0x80, 0x7f, 0, 0x60, 8, 0x14, 0x3e, 0x6f, 0x7f, 0x27, 0x5e, 0x81, 0x77, 0xe5, 0x11, 0xd2]),
    ] {
        let positions: Vec<_> = data.windows(16).enumerate().filter(|(_, b)| *b == standard).map(|(i, _)| i).collect();
        for pos in positions {
            data[pos..pos + 16].copy_from_slice(&legacy);
        }
    }
    let file = open(data.as_slice()).unwrap();
    assert_eq!(file.tracks[file.track_of_kind(TrackKind::Picture).unwrap()].samples.len(), 2);
    assert!(file.track_of_kind(TrackKind::Sound).is_some());
    assert!(file.timecode.is_some());
}

#[test]
fn material_clock_and_duration_survive_file_unit_conversion() {
    let (offsets, flags) = ipbb();
    let mut bytes = build(7, |_| vec![0; 100], &offsets, &flags);
    let original = track(14, 3, 0x1601_0300, 24);
    let mut changed = original.clone();
    let rate = [0x4b, 0x01, 0, 8, 0, 0, 0, 25, 0, 0, 0, 1];
    let offset = changed.windows(rate.len()).position(|w| w == rate).unwrap();
    changed[offset + 4..offset + 8].copy_from_slice(&48000i32.to_be_bytes());
    let offset = bytes.windows(original.len()).position(|w| w == original).unwrap();
    bytes[offset..offset + original.len()].copy_from_slice(&changed);
    let file = open(&bytes).unwrap();
    let sound = &file.tracks[file.track_of_kind(TrackKind::Sound).unwrap()];
    assert_eq!(sound.edit_rate, Rational::new(48000, 1));
    assert_eq!(sound.duration, Some(13440));
    assert_eq!(sound.material_edit_rate, Some(Rational::new(25, 1)));
    assert_eq!(sound.material_duration, Some(7));
    assert_eq!(sound.stored_sample_frames(), 13440);
}
