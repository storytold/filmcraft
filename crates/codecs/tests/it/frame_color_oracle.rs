//! Decoded H.264 / HEVC frames carry the colour primaries their VUI signals (#924). ffmpeg writes
//! MP4 without a `colr` box unless asked, so for most files the bitstream is the only place the
//! primaries are described: stream info already read them, the decoded frames reported BT.709.

use crate::common;

use common::*;
use filmcraft_color::{ColorInfo, ColorSpace, Matrix, Primaries, Range, Transfer};
use filmcraft_media::FrameRequest;
use filmcraft_time::Tick;

/// One 64×64 frame of `codec` with the colour `params` (x264 / x265 syntax), and our stream and
/// frame colour for it.
fn colors(ff: &std::path::Path, name: &str, codec: &[&str]) -> Option<(ColorInfo, ColorInfo)> {
    let mut args = vec!["-f", "lavfi", "-i", "testsrc2=size=64x64:rate=1", "-frames:v", "1"];
    args.extend_from_slice(codec);
    let path = fixture(ff, name, &args)?;
    let isobmff = filmcraft_isobmff::open(bytes(&path)).unwrap();
    let track = isobmff.tracks.iter().find(|t| t.kind == filmcraft_isobmff::TrackKind::Video).unwrap();
    assert!(track.entries[0].video.as_ref().unwrap().color.is_none(), "{name}: the fixture has a colr box");
    let src = filmcraft_codecs::open_bytes(name, bytes(&path)).unwrap();
    let stream = src.info().video.as_ref().unwrap().color;
    let frame = src.video_frame(FrameRequest::full(Tick::ZERO)).unwrap().color;
    Some((stream, frame))
}

#[test]
fn decoded_frames_keep_the_signalled_primaries() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let x264 = |p: &'static str| ["-c:v", "libx264", "-pix_fmt", "yuv420p", "-x264-params", p];
    let x265 = |p: &'static str, pix: &'static str| ["-c:v", "libx265", "-pix_fmt", pix, "-tag:v", "hvc1", "-x265-params", p];
    let sdr2020 = ColorInfo { matrix: Matrix::Bt709, transfer: Transfer::Bt709, primaries: Primaries::Bt2020, range: Range::Limited };
    let cases: [(&str, Vec<&str>, ColorInfo, ColorSpace); 4] = [
        ("prim2020_h264.mp4", x264("colorprim=bt2020:transfer=bt709:colormatrix=bt709").to_vec(), sdr2020, ColorSpace::Rec2020),
        ("prim2020_hevc.mp4", x265("log-level=error:colorprim=bt2020:transfer=bt709:colormatrix=bt709", "yuv420p").to_vec(), sdr2020, ColorSpace::Rec2020),
        // control: BT.709 throughout
        (
            "prim709_h264.mp4",
            x264("colorprim=bt709:transfer=bt709:colormatrix=bt709").to_vec(),
            ColorInfo { primaries: Primaries::Bt709, ..sdr2020 },
            ColorSpace::Rec709,
        ),
        // HDR keeps its transfer and now its primaries too
        (
            "prim2020_pq_hevc.mp4",
            x265("log-level=error:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc", "yuv420p10le").to_vec(),
            ColorInfo { matrix: Matrix::Bt2020Ncl, transfer: Transfer::Pq, primaries: Primaries::Bt2020, range: Range::Limited },
            ColorSpace::Rec2100Pq,
        ),
    ];
    for (name, codec, want, space) in cases {
        let Some((stream, frame)) = colors(&ff, name, &codec) else {
            eprintln!("SKIPPED ({name}): this ffmpeg can't encode it");
            continue;
        };
        println!("{name}: stream {stream:?}, frame {frame:?}");
        assert_eq!(stream, want, "{name}: stream");
        assert_eq!(frame, want, "{name}: decoded frame");
        assert_eq!(ColorSpace::from_info(&frame), space, "{name}");
    }
}
