//! Original synthetic ProRes and QuickTime descriptions, based on public sample
//! entry layouts. CoreMedia serializes LogTransferFunction as a `logs` child box.
use filmcraft_isobmff::{Brand, CodecConfig, FourCc, Mp4Writer, SampleEntry, TrackConfig, WriteSample, WriterOptions};
use filmcraft_prores::{ChromaFormat, Encoder, EncoderConfig, Frame, Profile};

pub const APPLE_LOG: &[u8] = b"com.apple.rec2020.apple-log";

fn atom(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

pub fn movie(identifier: Option<&[u8]>, colr: bool, matrix: u8) -> Vec<u8> {
    movie_with_logs(&identifier.into_iter().collect::<Vec<_>>(), colr, matrix)
}

pub fn movie_with_logs(identifiers: &[&[u8]], colr: bool, matrix: u8) -> Vec<u8> {
    let mut frame = Frame::new(64, 64, ChromaFormat::Yuv422, 10, false);
    // Apple Log's published 18% grey signal is approximately 0.488, in IRE.
    frame.y.fill(491);
    frame.cb.fill(512);
    frame.cr.fill(512);
    let mut config = EncoderConfig::new(Profile::Hq, 64, 64);
    config.color = filmcraft_prores::ColorInfo { primaries: 9, transfer: 1, matrix };
    let coded = Encoder::with_config(config).encode(&frame).unwrap();
    // An original VisualSampleEntry body. Unknown's raw-entry writer lets us
    // supply metadata independently of the typed writer, including malformed logs.
    let mut raw = vec![0u8; 78];
    raw[6..8].copy_from_slice(&1u16.to_be_bytes());
    raw[12..16].copy_from_slice(b"fcft");
    raw[24..26].copy_from_slice(&64u16.to_be_bytes());
    raw[26..28].copy_from_slice(&64u16.to_be_bytes());
    raw[28..32].copy_from_slice(&0x0048_0000u32.to_be_bytes());
    raw[32..36].copy_from_slice(&0x0048_0000u32.to_be_bytes());
    raw[40..42].copy_from_slice(&1u16.to_be_bytes());
    raw[74..76].copy_from_slice(&24u16.to_be_bytes());
    raw[76..78].copy_from_slice(&u16::MAX.to_be_bytes());
    if colr {
        let mut data = b"nclc".to_vec();
        for code in [9u16, 1, matrix as u16] {
            data.extend_from_slice(&code.to_be_bytes());
        }
        raw.extend(atom(b"colr", &data));
    }
    for identifier in identifiers {
        raw.extend(atom(b"logs", identifier));
    }
    let entry = SampleEntry::video(FourCc(*b"apch"), CodecConfig::Unknown { fourcc: FourCc(*b"apch"), raw }, 64, 64);
    let mut out = Mp4Writer::new(std::io::Cursor::new(Vec::new()), WriterOptions::new(Brand::Mov)).unwrap();
    let track = out.add_track(TrackConfig::new(entry, 30)).unwrap();
    for _ in 0..30 {
        out.write_sample(track, WriteSample { data: &coded, duration: 1, composition_offset: 0, is_sync: true }).unwrap();
    }
    out.finish().unwrap().into_inner()
}
