//! Generated from hand-written XML, never from a private project. MIT OR Apache-2.0.

use super::*;
use std::fmt::Write as _;
use std::io::Write as _;

const MIB: usize = 1024 * 1024;
// Two XML nodes per 80 bytes: roughly 26,000 nodes/MiB, including element text.
const VALUE: &str = "<Value>00000000000000000000000000000000000000000000000000000000000000000</Value>";

fn project(sequences: usize, values_per_sequence: usize) -> String {
    let mut xml = String::from("<PremiereData>");
    for n in 0..sequences {
        write!(xml, r#"<Sequence ObjectUID="sequence-{n}"><Name>Episode {n}</Name><TrackGroups><TrackGroup><Second ObjectRef="group-{n}"/></TrackGroup></TrackGroups><Metadata>"#).unwrap();
        for _ in 0..values_per_sequence {
            xml.push_str(VALUE);
        }
        write!(xml, r#"</Metadata></Sequence>
<VideoTrackGroup ObjectID="group-{n}"><TrackGroup><FrameRate>8467200000</FrameRate><Tracks><Track ObjectURef="track-{n}"/></Tracks></TrackGroup></VideoTrackGroup>
<VideoClipTrack ObjectUID="track-{n}"><ClipTrack><Track><Index>0</Index></Track><ClipItems><TrackItems><TrackItem ObjectRef="item-{n}"/></TrackItems></ClipItems></ClipTrack></VideoClipTrack>
<VideoClipTrackItem ObjectID="item-{n}"><ClipTrackItem><TrackItem><Start>0</Start><End>254016000000</End></TrackItem><SubClip ObjectRef="sub-{n}"/></ClipTrackItem></VideoClipTrackItem>
<SubClip ObjectID="sub-{n}"><Name>Clip {n}</Name><Clip ObjectRef="clip-{n}"/></SubClip>
<VideoClip ObjectID="clip-{n}"><Clip><Source ObjectRef="source-{n}"/><InPoint>0</InPoint><OutPoint>254016000000</OutPoint></Clip></VideoClip>
<VideoMediaSource ObjectID="source-{n}"><MediaSource><Media ObjectURef="media-{n}"/></MediaSource></VideoMediaSource>
<Media ObjectUID="media-{n}"><Title>Media {n}</Title><ActualMediaFilePath>/missing/generated-{n}.mov</ActualMediaFilePath></Media>"#).unwrap();
    }
    xml.push_str("</PremiereData>");
    xml
}

fn gzip(xml: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(xml).unwrap();
    encoder.finish().unwrap()
}

fn verify(bytes: &[u8], sequences: usize) {
    let (fragment, document, report) = import(bytes, &ImportOptions::default(), None).unwrap();
    assert_eq!(document.sequences.len(), sequences);
    assert_eq!(fragment.sequences.len(), sequences);
    for (n, seq) in document.sequences.iter().enumerate() {
        assert_eq!(seq.object_uid, format!("sequence-{n}"));
        assert_eq!(seq.frame_rate, Some(FrameRate::FPS_30));
        assert_eq!(seq.tracks.len(), 1);
        assert_eq!(seq.tracks[0].items.len(), 1);
        assert_eq!(seq.tracks[0].items[0].end, Tick(254016000000));
        assert_eq!(seq.tracks[0].items[0].media_path.as_deref(), Some(format!("/missing/generated-{n}.mov").as_str()));
    }
    assert!(!report.mentions("title fallback"), "{report:?}");
}

#[test]
fn more_than_half_a_million_nodes_import_in_plain_and_gzip() {
    let xml = project(1, 250_001);
    assert!(roxmltree::Document::parse(&xml).unwrap().descendants().len() > 500_000);
    verify(xml.as_bytes(), 1);
    verify(&gzip(xml.as_bytes()), 1);
}

#[test]
fn compressed_input_keeps_64_mib_limit_in_stream_and_slice_apis() {
    let mut bytes = vec![0; 64 * MIB + 2];
    bytes[..2].copy_from_slice(&[0x1f, 0x8b]);
    let mut input = std::io::Cursor::new(&bytes);
    assert!(read_input(&mut input).unwrap_err().to_string().contains("compressed project exceeds 64 MiB"));
    assert_eq!(input.position(), (64 * MIB + 1) as u64, "must stop reading at the compressed limit plus one byte");
    assert!(import(&bytes, &ImportOptions::default(), None).unwrap_err().to_string().contains("compressed project exceeds 64 MiB"));
}

#[test]
fn input_reader_preserves_plain_and_gzip_and_reports_read_errors() {
    let xml = project(1, 0);
    for bytes in [xml.into_bytes(), gzip(project(1, 0).as_bytes()), vec![], vec![0x1f]] {
        assert_eq!(read_input(bytes.as_slice()).unwrap(), bytes);
    }
    struct FailedRead;
    impl Read for FailedRead {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("synthetic read failure"))
        }
    }
    for header in [vec![], vec![0x1f, 0x8b], b"<P".to_vec()] {
        assert!(read_input(std::io::Cursor::new(header).chain(FailedRead)).unwrap_err().to_string().contains("synthetic read failure"));
    }
}

#[test]
fn scaled_budget_still_rejects_repeated_large_subtrees() {
    let xml = project(1, 50_000).replace("<ClipTrackItem>", &format!("<ClipTrackItem><Metadata>{}</Metadata>", VALUE.repeat(10_000)));
    assert!(roxmltree::Document::parse(&xml).unwrap().descendants().len() * 64 > 5_000_000);
    let pointer = r#"<TrackItem ObjectRef="item-0"/>"#;
    let small = xml.replace(pointer, &pointer.repeat(2));
    assert_eq!(read(small.as_bytes(), None, &mut Report::default()).unwrap().sequences[0].tracks[0].items.len(), 2);
    let repeated = xml.replace(pointer, &pointer.repeat(250));
    assert!(read(repeated.as_bytes(), None, &mut Report::default()).unwrap_err().to_string().contains("import work limit"));
}

fn large_import(compressed: bool) {
    let sequences = 83;
    let values = 150 * MIB / (sequences * VALUE.len()) + 1;
    let xml = project(sequences, values);
    assert!(xml.len() >= 150 * MIB);
    let nodes = roxmltree::Document::parse(&xml).unwrap().descendants().len();
    eprintln!("synthetic: expanded_bytes={}, nodes={nodes}, sequences={sequences}, gzip={compressed}", xml.len());
    if compressed {
        let bytes = gzip(xml.as_bytes());
        drop(xml);
        eprintln!("compressed_bytes={}", bytes.len());
        verify(&bytes, sequences);
    } else {
        verify(xml.as_bytes(), sequences);
    }
}

#[test]
#[ignore = "150 MiB synthetic capacity/RSS receipt; run explicitly in a separate process"]
fn synthetic_150_mib_plain_import() {
    large_import(false);
}

#[test]
#[ignore = "150 MiB synthetic capacity/RSS receipt; run explicitly in a separate process"]
fn synthetic_150_mib_gzip_import() {
    large_import(true);
}
