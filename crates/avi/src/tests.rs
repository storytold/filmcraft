use super::*;

fn chunk(id: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut v = id.to_vec();
    v.extend_from_slice(&(data.len() as u32).to_le_bytes());
    v.extend_from_slice(data);
    if data.len() % 2 == 1 {
        v.push(0);
    }
    v
}

fn list(ltype: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut d = ltype.to_vec();
    d.extend_from_slice(body);
    chunk(b"LIST", &d)
}

fn strl(video: bool) -> Vec<u8> {
    let mut strh = vec![0u8; 56];
    strh[..4].copy_from_slice(if video { b"vids" } else { b"auds" });
    strh[4..8].copy_from_slice(if video { b"MJPG" } else { &[0; 4] });
    let (scale, rate, ss) = if video { (1u32, 25u32, 0u32) } else { (4, 192_000, 4) };
    strh[20..24].copy_from_slice(&scale.to_le_bytes());
    strh[24..28].copy_from_slice(&rate.to_le_bytes());
    strh[44..48].copy_from_slice(&ss.to_le_bytes());
    let strf = if video {
        let mut b = vec![0u8; 40];
        b[..4].copy_from_slice(&40u32.to_le_bytes());
        b[4..8].copy_from_slice(&64i32.to_le_bytes());
        b[8..12].copy_from_slice(&48i32.to_le_bytes());
        b[14..16].copy_from_slice(&24u16.to_le_bytes());
        b[16..20].copy_from_slice(b"MJPG");
        b
    } else {
        let mut b = vec![0u8; 18];
        b[..2].copy_from_slice(&1u16.to_le_bytes());
        b[2..4].copy_from_slice(&2u16.to_le_bytes());
        b[4..8].copy_from_slice(&48_000u32.to_le_bytes());
        b[8..12].copy_from_slice(&192_000u32.to_le_bytes());
        b[12..14].copy_from_slice(&4u16.to_le_bytes());
        b[14..16].copy_from_slice(&16u16.to_le_bytes());
        b
    };
    let mut body = chunk(b"strh", &strh);
    body.extend(chunk(b"strf", &strf));
    body.extend(chunk(b"strn", if video { b"Camera\0" } else { b"Mic\0" }));
    list(b"strl", &body)
}

/// A small AVI: 3 video frames (the second a delta, the third odd-sized) and 2 audio chunks,
/// optionally inside a `rec ` list, with an `idx1` whose offsets are relative to `movi` or absolute.
fn avi(idx1: bool, absolute: bool, rec: bool) -> Vec<u8> {
    let mut avih = vec![0u8; 56];
    avih[0..4].copy_from_slice(&40_000u32.to_le_bytes());
    avih[12..16].copy_from_slice(&0x10u32.to_le_bytes());
    avih[16..20].copy_from_slice(&3u32.to_le_bytes());
    avih[24..28].copy_from_slice(&2u32.to_le_bytes());
    avih[32..36].copy_from_slice(&64u32.to_le_bytes());
    avih[36..40].copy_from_slice(&48u32.to_le_bytes());
    let mut hdrl = chunk(b"avih", &avih);
    hdrl.extend(strl(true));
    hdrl.extend(strl(false));
    let hdrl = list(b"hdrl", &hdrl);
    let samples: [(&[u8; 4], Vec<u8>, bool); 5] =
        [(b"00dc", vec![1; 10], true), (b"01wb", vec![2; 16], true), (b"00dc", vec![3; 6], false), (b"01wb", vec![4; 8], true), (b"00dc", vec![5; 7], true)];
    let mut body = Vec::new();
    let mut entries = Vec::new();
    for (id, data, key) in &samples {
        entries.push((**id, body.len() as u32 + 4, data.len() as u32, *key));
        body.extend(chunk(id, data));
    }
    let movi_body = if rec {
        let r = list(b"rec ", &body);
        // the samples moved 12 bytes in (the rec list's header and type)
        for e in &mut entries {
            e.1 += 12;
        }
        r
    } else {
        body
    };
    let movi = list(b"movi", &movi_body);
    let mut riff = b"AVI ".to_vec();
    riff.extend(hdrl);
    // position of the `movi` type id: after `RIFF` <size>, what `riff` holds so far, `LIST` <size>
    let movi_at = (8 + riff.len() + 8) as u32;
    riff.extend(movi);
    if idx1 {
        let mut ix = Vec::new();
        for (id, off, len, key) in entries {
            ix.extend_from_slice(&id);
            ix.extend_from_slice(&(if key { 0x10u32 } else { 0 }).to_le_bytes());
            ix.extend_from_slice(&(if absolute { off + movi_at } else { off }).to_le_bytes());
            ix.extend_from_slice(&len.to_le_bytes());
        }
        riff.extend(chunk(b"idx1", &ix));
    }
    chunk(b"RIFF", &riff)
}

fn table(f: &AviFile, s: usize) -> Vec<(u32, bool)> {
    f.streams[s].chunks.iter().map(|c| (c.size, c.key)).collect()
}

#[test]
fn headers_and_every_index_path_agree() {
    for (idx1, absolute, rec, want) in [
        (true, false, false, IndexSource::Idx1),
        (true, true, false, IndexSource::Idx1),
        (true, false, true, IndexSource::Idx1),
        (false, false, false, IndexSource::Scan),
        (false, false, true, IndexSource::Scan),
    ] {
        let file = avi(idx1, absolute, rec);
        let f = open(&file).unwrap();
        let what = format!("idx1 {idx1} absolute {absolute} rec {rec}");
        assert_eq!(f.index, want, "{what}");
        assert_eq!((f.header.width, f.header.height, f.header.total_frames, f.header.micro_sec_per_frame), (64, 48, 3, 40_000));
        let (v, a) = (&f.streams[0], &f.streams[1]);
        assert_eq!((v.kind, v.handler, v.scale, v.rate, v.name.as_deref()), (StreamKind::Video, *b"MJPG", 1, 25, Some("Camera")));
        assert_eq!(v.video.as_ref().map(|b| (b.width, b.height, b.bit_count, b.compression)), Some((64, 48, 24, *b"MJPG")));
        let w = a.audio.as_ref().unwrap();
        assert_eq!((w.format_tag, w.channels, w.sample_rate, w.block_align, w.bits_per_sample, a.sample_size), (1, 2, 48_000, 4, 16, 4));
        // the index knows the delta frame; a scan cannot
        let keys = if idx1 { [true, false, true] } else { [true; 3] };
        assert_eq!(table(&f, 0), vec![(10, keys[0]), (6, keys[1]), (7, keys[2])], "{what}");
        assert_eq!(f.streams[0].keyframes_known, idx1);
        assert_eq!(table(&f, 1), vec![(16, true), (8, true)], "{what}");
        assert_eq!(f.read_chunk(&file, 0, 1).unwrap(), vec![3; 6], "{what}");
        assert_eq!(f.read_chunk(&file, 0, 2).unwrap(), vec![5; 7], "{what}");
        assert_eq!(f.read_chunk(&file, 1, 1).unwrap(), vec![4; 8], "{what}");
        assert!(f.read_chunk(&file, 0, 3).is_err() && f.read_chunk(&file, 5, 0).is_err());
    }
    // the idx1 path can be skipped
    let f = open_with(&avi(true, false, false), OpenOptions { ignore_idx1: true, ..Default::default() }).unwrap();
    assert_eq!(f.index, IndexSource::Scan);
}

#[test]
fn not_avi_and_truncated_recordings() {
    assert_eq!(open(&b"RIFF\0\0\0\0WAVEfmt ".to_vec()), Err(Error::NotAvi));
    assert_eq!(open(&Vec::new()), Err(Error::NotAvi));
    assert!(sniff(&avi(true, false, false)) && !sniff(b"RIFF"));
    // a recording cut off in its last frame: the headers and the whole frames are there
    let full = avi(false, false, false);
    let cut = full[..full.len() - 4].to_vec();
    let f = open(&cut).unwrap();
    assert_eq!(f.streams[0].chunks.len(), 3);
    assert!(f.read_chunk(&cut, 0, 0).is_ok());
}

/// Every single-byte change and every truncation of a valid file opens or fails, never panics, and
/// whatever opens reads its chunks without panicking.
#[test]
fn mutations_never_panic() {
    for base in [avi(true, false, true), avi(false, false, false), avi(true, true, false)] {
        for cut in 0..base.len() {
            let b = base[..cut].to_vec();
            if let Ok(f) = open(&b) {
                for (s, st) in f.streams.iter().enumerate() {
                    for i in 0..st.chunks.len() {
                        let _ = f.read_chunk(&b, s, i);
                    }
                }
            }
        }
        for at in 0..base.len() {
            for v in [0x00u8, 0xFF, 0x7F, 0x80] {
                let mut b = base.clone();
                b[at] = v;
                if let Ok(f) = open(&b) {
                    for (s, st) in f.streams.iter().enumerate() {
                        for i in 0..st.chunks.len() {
                            let _ = f.read_chunk(&b, s, i);
                        }
                    }
                }
            }
        }
    }
}

/// An OpenDML file: each stream's `indx` super index points at an `ix##` standard index (base
/// offset + offsets to the data, bit 31 of the size marking delta frames). Built twice: the second
/// pass knows where the `ix##` chunks landed.
fn avi_odml(ix_at: [u64; 2]) -> (Vec<u8>, [u64; 2]) {
    let indx = |stream: u8, at: u64| {
        let mut b = Vec::new();
        b.extend_from_slice(&4u16.to_le_bytes()); // wLongsPerEntry
        b.extend_from_slice(&[0, 0]); // subtype, AVI_INDEX_OF_INDEXES
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&[b'0', b'0' + stream, if stream == 0 { b'd' } else { b'w' }, if stream == 0 { b'c' } else { b'b' }]);
        b.extend_from_slice(&[0; 12]);
        b.extend_from_slice(&at.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        chunk(b"indx", &b)
    };
    let mut hdrl = chunk(b"avih", &[0; 56]);
    for (s, at) in ix_at.iter().enumerate() {
        let l = strl(s == 0);
        // strl = LIST <size> strl …: append indx inside it
        let mut body = l[12..].to_vec();
        body.extend(indx(s as u8, *at));
        hdrl.extend(list(b"strl", &body));
    }
    let mut riff = b"AVI ".to_vec();
    riff.extend(list(b"hdrl", &hdrl));
    // movi: video 10 (key) 6 (delta) 7 (key), audio 16 8; then ix00, ix01
    let movi_data_at = 8 + riff.len() as u64 + 12; // RIFF hdr, so far, LIST hdr + `movi`
    let mut body = Vec::new();
    let mut at: [Vec<(u64, u32, bool)>; 2] = [Vec::new(), Vec::new()];
    for (s, data, key) in [(0usize, vec![1u8; 10], true), (1, vec![2; 16], true), (0, vec![3; 6], false), (1, vec![4; 8], true), (0, vec![5; 7], true)] {
        let id = if s == 0 { b"00dc" } else { b"01wb" };
        at[s].push((movi_data_at + body.len() as u64 + 8, data.len() as u32, key));
        body.extend(chunk(id, &data));
    }
    let mut where_ix = [0u64; 2];
    for (s, entries) in at.iter().enumerate() {
        let base = 100u64; // offsets are relative to qwBaseOffset
        let mut b = Vec::new();
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&[0, 1]); // subtype 0, AVI_INDEX_OF_CHUNKS
        b.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        b.extend_from_slice(if s == 0 { b"00dc" } else { b"01wb" });
        b.extend_from_slice(&base.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        for &(off, size, key) in entries {
            b.extend_from_slice(&((off - base) as u32).to_le_bytes());
            b.extend_from_slice(&(size | if key { 0 } else { 0x8000_0000 }).to_le_bytes());
        }
        where_ix[s] = movi_data_at + body.len() as u64;
        body.extend(chunk(if s == 0 { b"ix00" } else { b"ix01" }, &b));
    }
    riff.extend(list(b"movi", &body));
    (chunk(b"RIFF", &riff), where_ix)
}

#[test]
fn opendml_standard_indexes() {
    let (_, at) = avi_odml([0, 0]);
    let (file, again) = avi_odml(at);
    assert_eq!(at, again, "the layout does not depend on the offsets");
    let f = open(&file).unwrap();
    assert_eq!(f.index, IndexSource::OpenDml);
    assert_eq!(table(&f, 0), vec![(10, true), (6, false), (7, true)]);
    assert_eq!(table(&f, 1), vec![(16, true), (8, true)]);
    assert_eq!(f.read_chunk(&file, 0, 2).unwrap(), vec![5; 7]);
    // ignoring the OpenDML index, the scan finds the same chunks (and skips the ix## chunks)
    let s = open_with(&file, OpenOptions { ignore_odml: true, ..Default::default() }).unwrap();
    assert_eq!(s.index, IndexSource::Scan);
    assert_eq!(s.streams[0].chunks.iter().map(|c| c.offset).collect::<Vec<_>>(), f.streams[0].chunks.iter().map(|c| c.offset).collect::<Vec<_>>());
    // a broken super index (pointing nowhere) falls back to a scan
    let (bad, _) = avi_odml([3, 5]);
    assert_eq!(open(&bad).unwrap().index, IndexSource::Scan);
    // and mutations of the OpenDML file never panic
    for i in 0..file.len() {
        let mut b = file.clone();
        b[i] ^= 0xFF;
        if let Ok(f) = open(&b) {
            for (s, st) in f.streams.iter().enumerate() {
                for k in 0..st.chunks.len() {
                    let _ = f.read_chunk(&b, s, k);
                }
            }
        }
    }
}
