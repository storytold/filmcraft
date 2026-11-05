//! #925: a chunk after an RF64 `data` sentinel must not become audio.

use std::sync::Arc;

use filmcraft_time::Tick;

fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    if body.len() % 2 == 1 {
        out.push(0);
    }
    out
}

/// Four 16-bit mono frames of silence at 48 kHz, as RIFF or RF64, optionally with a trailing iXML.
fn fixture(rf64: bool, trailing: bool) -> Vec<u8> {
    let mut fmt = Vec::new();
    for v in [1u16, 1] {
        fmt.extend_from_slice(&v.to_le_bytes());
    }
    fmt.extend_from_slice(&48_000u32.to_le_bytes());
    fmt.extend_from_slice(&96_000u32.to_le_bytes());
    fmt.extend_from_slice(&2u16.to_le_bytes());
    fmt.extend_from_slice(&16u16.to_le_bytes());
    let mut out = if rf64 { b"RF64\xff\xff\xff\xffWAVE".to_vec() } else { b"RIFF\0\0\0\0WAVE".to_vec() };
    if rf64 {
        let mut ds = vec![0u8; 28];
        ds[8..16].copy_from_slice(&8u64.to_le_bytes());
        ds[16..24].copy_from_slice(&4u64.to_le_bytes());
        out.extend(chunk(b"ds64", &ds));
    }
    out.extend(chunk(b"fmt ", &fmt));
    let offset = out.len();
    out.extend(chunk(b"data", &[0u8; 8]));
    if rf64 {
        out[offset + 4..offset + 8].copy_from_slice(&u32::MAX.to_le_bytes());
    }
    if trailing {
        out.extend(chunk(b"iXML", b"<BWFXML></BWFXML>"));
    }
    let size = out.len() - 8;
    if rf64 {
        out[20..28].copy_from_slice(&(size as u64).to_le_bytes());
    } else {
        out[4..8].copy_from_slice(&(size as u32).to_le_bytes());
    }
    out
}

#[test]
fn rf64_metadata_stays_outside_audio() {
    for (name, rf64, trailing) in [("rf64.wav", true, false), ("riff_ixml.wav", false, true), ("rf64_ixml.wav", true, true)] {
        let bytes: Arc<[u8]> = fixture(rf64, trailing).into();
        let src = filmcraft_codecs::open_bytes(name, bytes.clone()).unwrap();
        let stream = filmcraft_media::reader::open_reader(
            name,
            Arc::new(filmcraft_media::reader::MemReader(bytes)),
            &filmcraft_codecs::reader_openers(),
            &filmcraft_codecs::openers(),
        )
        .unwrap();
        for s in [&src, &stream] {
            assert_eq!(s.info().duration, Tick::from_units(4, 48_000), "{name}");
            assert_eq!(s.audio(0, 20, 48_000).unwrap().channels.remove(0), vec![0.0; 20], "{name}");
        }
    }
}
