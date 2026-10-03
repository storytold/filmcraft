//! Hand-built files exercising features FFmpeg does not write: unknown-size Segment/Clusters,
//! header stripping, BlockGroups, Colour + mastering metadata, Chapters, Tags, Attachments, CRC-32.

use crate::codec::Codec;
use crate::ebml::{self, write_id, write_unknown_size};
use crate::ids::*;
use crate::mux::{el, el_float, el_str, el_uint};
use crate::track::ContentEncoding;
use crate::{Demuxer, OpenOptions, Packet, open_with};

fn block(track: u8, rel: i16, flags: u8, data: &[u8]) -> Vec<u8> {
    let mut b = vec![0x80 | track];
    b.extend_from_slice(&rel.to_be_bytes());
    b.push(flags);
    b.extend_from_slice(data);
    b
}

fn synthetic() -> Vec<u8> {
    let mut f = Vec::new();
    let mut h = Vec::new();
    el_str(&mut h, DOC_TYPE, "matroska");
    el_uint(&mut h, DOC_TYPE_VERSION, 4);
    el(&mut f, EBML, &h);
    write_id(&mut f, SEGMENT);
    write_unknown_size(&mut f, 8);
    // Info with a correct CRC-32 and a Void
    let mut info = Vec::new();
    el_uint(&mut info, TIMESTAMP_SCALE, 1_000_000);
    el_str(&mut info, TITLE, "Synthetic");
    el(&mut info, VOID, &[0; 3]);
    let mut crc_info = Vec::new();
    el(&mut crc_info, CRC32, &ebml::crc32(&info).to_le_bytes());
    crc_info.extend_from_slice(&info);
    el(&mut f, INFO, &crc_info);
    // Tracks
    let mut v = Vec::new();
    el_uint(&mut v, TRACK_NUMBER, 1);
    el_uint(&mut v, TRACK_UID, 11);
    el_uint(&mut v, TRACK_TYPE, 1);
    el_str(&mut v, CODEC_ID, "V_MPEG4/ISO/AVC");
    el(&mut v, CODEC_PRIVATE, &[1, 0x64, 0, 0x1F]);
    el_uint(&mut v, DEFAULT_DURATION, 40_000_000);
    let mut mm = Vec::new();
    for (id, val) in [(PRIMARY_R_X, 0.708), (PRIMARY_R_Y, 0.292), (PRIMARY_G_X, 0.17), (PRIMARY_G_Y, 0.797), (PRIMARY_B_X, 0.131), (PRIMARY_B_Y, 0.046)] {
        el_float(&mut mm, id, val);
    }
    el_float(&mut mm, WHITE_POINT_X, 0.3127);
    el_float(&mut mm, WHITE_POINT_Y, 0.329);
    el_float(&mut mm, LUMINANCE_MAX, 1000.0);
    el_float(&mut mm, LUMINANCE_MIN, 0.0001);
    let mut col = Vec::new();
    el_uint(&mut col, MATRIX_COEFFICIENTS, 9);
    el_uint(&mut col, RANGE, 2);
    el_uint(&mut col, TRANSFER_CHARACTERISTICS, 16);
    el_uint(&mut col, PRIMARIES, 9);
    el_uint(&mut col, MAX_CLL, 1000);
    el_uint(&mut col, MAX_FALL, 400);
    el(&mut col, MASTERING_METADATA, &mm);
    let mut vid = Vec::new();
    el_uint(&mut vid, PIXEL_WIDTH, 1440);
    el_uint(&mut vid, PIXEL_HEIGHT, 1080);
    el_uint(&mut vid, DISPLAY_WIDTH, 1920);
    el_uint(&mut vid, DISPLAY_HEIGHT, 1080);
    el(&mut vid, COLOUR, &col);
    // rectangular, a quarter turn clockwise (portrait phone video)
    let mut proj = Vec::new();
    el_uint(&mut proj, PROJECTION_TYPE, 0);
    el_float(&mut proj, PROJECTION_POSE_ROLL, -90.0);
    el(&mut vid, PROJECTION, &proj);
    el(&mut v, VIDEO, &vid);
    // header stripping: every frame starts with 00 00 00 01
    let mut comp = Vec::new();
    el_uint(&mut comp, CONTENT_COMP_ALGO, 3);
    el(&mut comp, CONTENT_COMP_SETTINGS, &[0, 0, 0, 1]);
    let mut enc = Vec::new();
    el(&mut enc, CONTENT_COMPRESSION, &comp);
    let mut encs = Vec::new();
    el(&mut encs, CONTENT_ENCODING, &enc);
    el(&mut v, CONTENT_ENCODINGS, &encs);
    let mut a = Vec::new();
    el_uint(&mut a, TRACK_NUMBER, 2);
    el_uint(&mut a, TRACK_TYPE, 2);
    el_str(&mut a, CODEC_ID, "A_AAC/MPEG4/LC/SBR");
    el_str(&mut a, LANGUAGE, "ger");
    let mut au = Vec::new();
    el_float(&mut au, SAMPLING_FREQUENCY, 24000.0);
    el_float(&mut au, OUTPUT_SAMPLING_FREQUENCY, 48000.0);
    el_uint(&mut au, CHANNELS, 2);
    el(&mut a, AUDIO, &au);
    let mut tr = Vec::new();
    el(&mut tr, TRACK_ENTRY, &v);
    el(&mut tr, TRACK_ENTRY, &a);
    el(&mut f, TRACKS, &tr);
    // Chapters
    let mut disp = Vec::new();
    el_str(&mut disp, CHAP_STRING, "Intro");
    let mut atom = Vec::new();
    el_uint(&mut atom, CHAPTER_UID, 5);
    el_uint(&mut atom, CHAPTER_TIME_START, 0);
    el_uint(&mut atom, CHAPTER_TIME_END, 40_000_000);
    el(&mut atom, CHAPTER_DISPLAY, &disp);
    let mut ed = Vec::new();
    el(&mut ed, CHAPTER_ATOM, &atom);
    let mut ch = Vec::new();
    el(&mut ch, EDITION_ENTRY, &ed);
    el(&mut f, CHAPTERS, &ch);
    // Attachments
    let mut af = Vec::new();
    el_str(&mut af, FILE_NAME, "font.ttf");
    el_str(&mut af, FILE_MEDIA_TYPE, "font/ttf");
    el_uint(&mut af, FILE_UID, 77);
    el(&mut af, FILE_DATA, &[9; 100]);
    let mut att = Vec::new();
    el(&mut att, ATTACHED_FILE, &af);
    el(&mut f, ATTACHMENTS, &att);
    // Cluster 1 (unknown size): keyframe SimpleBlock, BlockGroup with ReferenceBlock + duration
    write_id(&mut f, CLUSTER);
    write_unknown_size(&mut f, 1);
    el_uint(&mut f, TIMESTAMP, 1000);
    el(&mut f, SIMPLE_BLOCK, &block(1, 0, 0x80, &[0x65, 1, 2]));
    el(&mut f, SIMPLE_BLOCK, &block(2, 5, 0x80, &[0xAA; 10]));
    let mut g = Vec::new();
    el(&mut g, BLOCK, &block(1, 40, 0, &[0x41, 3]));
    el(&mut g, REFERENCE_BLOCK, &[0xD8]);
    el_uint(&mut g, BLOCK_DURATION, 80);
    el(&mut f, BLOCK_GROUP, &g);
    el(&mut f, SIMPLE_BLOCK, &block(9, 0, 0x80, &[1])); // unknown track: ignored
    // Cluster 2 (unknown size), ended by Tags
    write_id(&mut f, CLUSTER);
    write_unknown_size(&mut f, 8);
    el_uint(&mut f, TIMESTAMP, 1120);
    el(&mut f, SIMPLE_BLOCK, &block(1, 0, 0x80, &[0x65, 4]));
    let mut st = Vec::new();
    el_str(&mut st, TAG_NAME, "ARTIST");
    el_str(&mut st, TAG_STRING, "Nobody");
    let mut tg = Vec::new();
    let mut tgt = Vec::new();
    el_uint(&mut tgt, TAG_TRACK_UID, 11);
    el(&mut tg, TARGETS, &tgt);
    el(&mut tg, SIMPLE_TAG, &st);
    let mut tags = Vec::new();
    el(&mut tags, TAG, &tg);
    el(&mut f, TAGS, &tags);
    f
}

#[test]
fn synthetic_file() {
    let bytes = synthetic();
    let f = open_with(&bytes[..], &OpenOptions { index: true, verify_crc: true }).unwrap();
    assert!(f.live);
    assert!(f.warnings.is_empty(), "{:?}", f.warnings);
    assert_eq!((f.crc_checked, f.crc_errors.len()), (1, 0));
    assert_eq!(f.info.title.as_deref(), Some("Synthetic"));
    let v = &f.tracks[0];
    assert!(matches!(&v.codec, Codec::Avc { avcc } if avcc[1] == 0x64));
    let vi = v.video.as_ref().unwrap();
    assert_eq!(vi.pixel_aspect(), (4, 3));
    assert_eq!(vi.projection.as_ref().map(|p| p.roll), Some(-90.0));
    assert_eq!(vi.display_rotation(), Some(1));
    let c = vi.colour.as_ref().unwrap();
    assert!(c.full_range());
    assert_eq!((c.matrix_coefficients, c.transfer_characteristics, c.primaries, c.max_cll, c.max_fall), (Some(9), Some(16), Some(9), Some(1000), Some(400)));
    let m = c.mastering.as_ref().unwrap();
    assert_eq!(m.primaries[1], (0.17, 0.797));
    assert_eq!(m.white_point, (0.3127, 0.329));
    assert_eq!((m.luminance_max, m.luminance_min), (1000.0, 0.0001));
    assert_eq!(v.encodings, vec![ContentEncoding::HeaderStripping { bytes: vec![0, 0, 0, 1], scope: 1 }]);
    // legacy AAC ID: ASC synthesised with explicit SBR signalling (LC, 24 kHz, stereo → 48 kHz)
    assert_eq!(f.tracks[1].codec, Codec::Aac { asc: vec![0x13, 0x10, 0x56, 0xE5, 0x98] });
    assert_eq!(f.tracks[1].language, "ger");
    assert_eq!(f.chapters[0].chapters[0].title(), Some("Intro"));
    assert_eq!(f.chapters[0].chapters[0].end_ns, Some(40_000_000));
    assert_eq!(f.attachments.len(), 1);
    let at = &f.attachments[0];
    assert_eq!((at.name.as_str(), at.media_type.as_str(), at.uid), ("font.ttf", "font/ttf", 77));
    assert_eq!(&bytes[at.data_offset as usize..][..100], &[9; 100]);
    assert_eq!(f.tags[0].targets.track_uids, vec![11]);
    assert_eq!(f.tags[0].simple[0].string.as_deref(), Some("Nobody"));
    assert_eq!(f.clusters.len(), 2);
    assert_eq!(v.samples.len(), 3);
    assert_eq!(v.keyframes(), &[0, 2]);

    let d = Demuxer::from_slice(&bytes).unwrap();
    let p: Vec<Packet> = d.map(|p| p.unwrap()).collect();
    assert_eq!(p.len(), 4);
    assert_eq!((p[0].track, p[0].pts, p[0].keyframe, p[0].duration), (0, 1000, true, 40));
    assert_eq!(p[0].data, [0, 0, 0, 1, 0x65, 1, 2]);
    assert_eq!((p[1].track, p[1].pts, p[1].data.len()), (1, 1005, 10));
    assert_eq!((p[2].pts, p[2].keyframe, p[2].duration), (1040, false, 80));
    assert_eq!(p[2].data, [0, 0, 0, 1, 0x41, 3]);
    assert_eq!((p[3].pts, p[3].pts_ns), (1120, 1_120_000_000));
    assert_eq!(f.read_sample(&bytes[..], 0, 1).unwrap(), p[2].data);

    // CRC mismatch is reported, not fatal
    let mut bad = bytes.clone();
    let title = bad.windows(9).position(|w| w == b"Synthetic").unwrap();
    bad[title] = b's';
    let f = open_with(&bad[..], &OpenOptions { index: true, verify_crc: true }).unwrap();
    assert_eq!(f.crc_errors.len(), 1);

    // seeking without index or cues builds the index
    let mut d = Demuxer::with_options(&bytes[..], OpenOptions { index: false, verify_crc: false }).unwrap();
    assert_eq!(d.file().tracks.len(), 2);
    assert_eq!(d.seek(0, 1_100_000_000).unwrap().pts, 1000);
    assert_eq!(d.seek(0, 1_130_000_000).unwrap().pts, 1120);
    assert_eq!(d.next_packet().unwrap().unwrap().pts, 1120);
    assert!(d.file().indexed);
}
